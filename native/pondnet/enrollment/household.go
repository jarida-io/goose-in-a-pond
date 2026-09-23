package enrollment

import (
	"context"
	"crypto/ed25519"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"io"
	"net"
	"net/http"
	"sync"
	"time"

	"golang.org/x/time/rate"
)

// HouseholdRegistration is a household's first contact. It is signed by the key
// it registers, which is the only thing it proves.
//
// That is deliberate. Admission is not what keeps households apart: the policy
// does, granting each phone its own Pond's HTTPS port and nothing else, so a
// stranger who registers gains a tailnet address and no reach into anyone's
// home. Requiring an operator to provision every household instead would mean
// nobody could set up a Pond without us.
type HouseholdRegistration struct {
	PublicKey string `json:"publicKey"`
	Port      uint16 `json:"port"`
	Expires   int64  `json:"expires"`
}

// householdDomain separates these signatures from enrollment approvals, so a
// signature captured from one can never be presented as the other.
const householdDomain = "goose-household-v1\x00"

// maxHouseholds bounds what open admission can consume. Provision used to be the
// only admission control; without a ceiling a stranger could enumerate keys and
// fill the store and the coordinator's address space.
const maxHouseholds = 10000

// sources rate-limits registration per client address, because the service-wide
// limiter cannot tell one household's first contact from a flood.
type sources struct {
	mu      sync.Mutex
	seen    map[string]*rate.Limiter
	maximum int
}

func newSources() *sources { return &sources{seen: map[string]*rate.Limiter{}, maximum: 4096} }

func (s *sources) allow(address string) bool {
	host, _, err := net.SplitHostPort(address)
	if err != nil {
		host = address
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	// Forget everything rather than grow without bound. A caller that loses its
	// budget this way is one among thousands and will be limited again at once.
	if len(s.seen) >= s.maximum {
		s.seen = map[string]*rate.Limiter{}
	}
	limiter, ok := s.seen[host]
	if !ok {
		limiter = rate.NewLimiter(rate.Limit(1.0/60.0), 3)
		s.seen[host] = limiter
	}
	return limiter.Allow()
}

// HouseholdID is the household's name for itself: a digest of its public key. The
// server derives it rather than trusting the caller, so a registration can only
// ever name the household whose key signed it.
func HouseholdID(public ed25519.PublicKey) string {
	digest := sha256.Sum256(public)
	return hex.EncodeToString(digest[:16])
}

// SignHousehold signs a first-contact registration with the key it registers.
func SignHousehold(registration HouseholdRegistration, key ed25519.PrivateKey) (Envelope, error) {
	payload, err := json.Marshal(registration)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{
		Payload:   base64.StdEncoding.EncodeToString(payload),
		Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(key, append([]byte(householdDomain), payload...))),
	}, nil
}

func (s *Service) registerHousehold(w http.ResponseWriter, r *http.Request) {
	if !s.sources.allow(r.RemoteAddr) {
		w.Header().Set("Retry-After", "60")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	select {
	case s.slots <- struct{}{}:
		defer func() { <-s.slots }()
	default:
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"busy"}`, 429)
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 8192))
	if err != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	var envelope Envelope
	var registration HouseholdRegistration
	if strict(body, &envelope) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	payload, err := base64.StdEncoding.DecodeString(envelope.Payload)
	if err != nil || strict(payload, &registration) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	signature, err := base64.StdEncoding.DecodeString(envelope.Signature)
	public, keyErr := base64.StdEncoding.DecodeString(registration.PublicKey)
	now := s.Now().Unix()
	if err != nil || keyErr != nil || len(public) != ed25519.PublicKeySize ||
		!ed25519.Verify(public, append([]byte(householdDomain), payload...), signature) ||
		registration.Expires <= now || registration.Expires > now+300 || registration.Port == 0 {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}

	id := HouseholdID(public)
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()

	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	if s.Store.failure != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	if existing, ok := s.Store.value.Households[id]; ok {
		// Registering again is how a household recovers from a lost response, so
		// it answers rather than conflicting. The key cannot differ: the id is a
		// digest of it.
		if existing.PublicKey != registration.PublicKey {
			http.Error(w, `{"error":"unauthorized"}`, 403)
			return
		}
		writeHousehold(w, id)
		return
	}
	if len(s.Store.value.Households) >= maxHouseholds {
		w.Header().Set("Retry-After", "3600")
		http.Error(w, `{"error":"capacity"}`, 503)
		return
	}
	user, err := s.Backend.EnsureUser(ctx, "household-"+id)
	if err != nil || user == "" {
		http.Error(w, `{"error":"coordinator_unavailable"}`, 503)
		return
	}
	if err := s.Store.provisionLocked(id, Household{PublicKey: registration.PublicKey, UserID: user, Port: registration.Port}); err != nil {
		http.Error(w, `{"error":"registration_failed"}`, 409)
		return
	}
	writeHousehold(w, id)
}

func writeHousehold(w http.ResponseWriter, id string) {
	_ = json.NewEncoder(w).Encode(map[string]string{"household": id})
}
