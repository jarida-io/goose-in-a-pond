package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"golang.org/x/time/rate"
	"io"
	"log/slog"
	"net/http"
	"net/netip"
	"regexp"
	"strconv"
	"time"
)

var identifier = regexp.MustCompile(`^[a-zA-Z0-9_-]{16,80}$`)
var nodeKeyPattern = regexp.MustCompile(`^nodekey:[0-9a-f]{64}$`)
var machineKeyPattern = regexp.MustCompile(`^mkey:[0-9a-f]{64}$`)
var numeric = regexp.MustCompile(`^[1-9][0-9]{0,19}$`)

// userNamePattern bounds the coordinator user name derived from a household id.
var userNamePattern = regexp.MustCompile(`^household-[0-9a-f]{32}$`)

// ErrRegistrationRejected means no registration mutation was accepted. Unlike a
// lost response, this outcome must never be turned into an inventory-based grant.
var ErrRegistrationRejected = errors.New("registration was rejected")

// Approval is signed by the locally approved Pond, including the exact pending node.
type Approval struct {
	Household  string `json:"household"`
	Device     string `json:"device"`
	Action     string `json:"action"`
	Role       string `json:"role"`
	AuthID     string `json:"authId"`
	NodeKey    string `json:"nodeKey"`
	MachineKey string `json:"machineKey,omitempty"`
	Nonce      string `json:"nonce"`
	Expires    int64  `json:"expires"`
	// ExpectedRevision is supplied only for an explicitly approved replacement.
	ExpectedRevision string `json:"expectedRevision,omitempty"`
}

// Envelope signs the exact payload bytes, avoiding cross-language JSON canonicalization.
type Envelope struct {
	Payload   string `json:"payload"`
	Signature string `json:"signature"`
}

// Registered is the coordinator's response, verified before permitting any traffic.
type Registered struct {
	ID         string
	Key        string
	UserID     string
	Addresses  []string
	Tags       []string
	Routes     []string
	MachineKey string
}

// Backend is the private Headscale administrative boundary.
type Backend interface {
	Inventory(context.Context) ([]Registered, error)
	EnsureUser(context.Context, string) (string, error)
	Register(context.Context, string, string) (Registered, error)
	Delete(context.Context, string) error
	Policy(context.Context, []Rule) error
}

// Rule permits only approved phone addresses to reach their own Pond HTTPS port.
type Rule struct {
	Action string   `json:"action"`
	Src    []string `json:"src"`
	Dst    []string `json:"dst"`
}

// Service serializes security transitions and bounds public work before parsing.
type Service struct {
	Store   *Store
	Backend Backend
	Now     func() time.Time
	slots   chan struct{}
	limiter *rate.Limiter
	sources *sources
}

// New reconciles the durable allowlist before accepting enrollment requests.
func New(ctx context.Context, s *Store, b Backend) (*Service, error) {
	service := &Service{Store: s, Backend: b, Now: time.Now, slots: make(chan struct{}, 8), limiter: rate.NewLimiter(10, 20), sources: newSources()}
	s.mu.Lock()
	defer s.mu.Unlock()
	if err := service.policy(ctx); err != nil {
		return nil, err
	}
	return service, nil
}
func (s *Service) policy(ctx context.Context) error {
	if s.Store.failure != nil {
		return s.Store.failure
	}
	inventory, err := s.Backend.Inventory(ctx)
	if err != nil {
		return err
	}
	matches := func(device Device, household Household) bool {
		for _, node := range inventory {
			if node.ID != device.NodeID {
				continue
			}
			address := false
			for _, ip := range node.Addresses {
				if ip == device.Address {
					address = true
				}
			}
			return node.Key == device.Key && (device.MachineKey == "" || node.MachineKey == device.MachineKey) && node.UserID == household.UserID && address && len(node.Tags) == 0 && len(node.Routes) == 0
		}
		return false
	}
	rules := []Rule{}
	for id, devices := range s.Store.value.Devices {
		pond := ""
		phones := []string{}
		for _, device := range devices {
			if device.Status != "active" || !matches(device, s.Store.value.Households[id]) {
				continue
			}
			if device.Role == "pond" {
				pond = device.Address
			} else {
				phones = append(phones, device.Address)
			}
		}
		if pond != "" && len(phones) > 0 {
			rules = append(rules, Rule{Action: "accept", Src: phones, Dst: []string{pond + ":" + strconv.Itoa(int(s.Store.value.Households[id].Port))}})
		}
	}
	return s.Backend.Policy(ctx, rules)
}
func strict(data []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(value); err != nil {
		return err
	}
	if d.Decode(new(any)) != io.EOF {
		return errors.New("trailing JSON")
	}
	return nil
}

// Sign prepares a request on the Pond; the private key never leaves its local directory.
func Sign(a Approval, key ed25519.PrivateKey) (Envelope, error) {
	payload, err := json.Marshal(a)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{Payload: base64.StdEncoding.EncodeToString(payload), Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(key, append([]byte("goose-enrollment-v1\x00"), payload...)))}, nil
}
func (s *Service) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Content-Type", "application/json")
	if r.Method == "GET" && r.URL.Path == "/health" {
		s.Store.mu.Lock()
		failed := s.Store.failure != nil
		s.Store.mu.Unlock()
		if failed {
			http.Error(w, `{"error":"storage_unavailable"}`, 503)
			return
		}
		io.WriteString(w, `{"ok":true}`)
		return
	}
	if r.Method == "POST" && r.URL.Path == "/v1/household" {
		s.registerHousehold(w, r)
		return
	}
	if r.Method != "POST" || r.URL.Path != "/v1/approval" {
		http.NotFound(w, r)
		return
	}
	if !s.limiter.Allow() {
		w.Header().Set("Retry-After", "2")
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
	var approval Approval
	if strict(body, &envelope) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	payload, err := base64.StdEncoding.DecodeString(envelope.Payload)
	if err != nil || strict(payload, &approval) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	signature, err := base64.StdEncoding.DecodeString(envelope.Signature)
	if err != nil {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	if s.Store.failure != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	h, exists := s.Store.value.Households[approval.Household]
	key, _ := base64.StdEncoding.DecodeString(h.PublicKey)
	now := s.Now().Unix()
	if !exists || len(key) != ed25519.PublicKeySize || !ed25519.Verify(key, append([]byte("goose-enrollment-v1\x00"), payload...), signature) || approval.Expires <= now || approval.Expires > now+300 || !identifier.MatchString(approval.Device) || !identifier.MatchString(approval.Nonce) {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}
	requestID := approval.Household + ":" + approval.Nonce
	if _, used := s.Store.value.Requests[requestID]; used {
		http.Error(w, `{"error":"approval_used"}`, 409)
		return
	}
	devices := s.Store.value.Devices[approval.Household]
	current, present := devices[approval.Device]
	if approval.Action == "inspect" {
		if !present {
			http.Error(w, `{"error":"enrollment_missing"}`, 404)
			return
		}
		// Legacy records acquire a durable revision before an operator can approve replacement.
		if current.Revision == "" {
			current.Revision = rand.Text()
			devices[approval.Device] = current
		}
		s.consume(requestID, approval.Expires, now)
		if err := s.Store.save(); err != nil {
			http.Error(w, `{"error":"storage_unavailable"}`, 503)
			return
		}
		json.NewEncoder(w).Encode(current)
		return
	}
	replacing := approval.Action == "replace"
	if replacing {
		if !present || current.Role != "phone" || approval.Role != "phone" || current.Revision == "" || approval.ExpectedRevision != current.Revision || (current.Status != "revoked" && current.Status != "failed" && current.Status != "pending") || len(s.Store.value.Retired[approval.Household]) >= 256 {
			http.Error(w, `{"error":"replacement_conflict"}`, 409)
			return
		}
	}
	if approval.Action == "enroll" || replacing {
		if (!replacing && (present || len(devices) >= 32)) || (approval.Role != "pond" && approval.Role != "phone") || len(approval.AuthID) < 16 || len(approval.AuthID) > 256 || !machineKeyPattern.MatchString(approval.MachineKey) || (approval.NodeKey != "" && !nodeKeyPattern.MatchString(approval.NodeKey)) {
			http.Error(w, `{"error":"invalid_enrollment"}`, 409)
			return
		}
		for _, householdDevices := range s.Store.value.Devices {
			for _, device := range householdDevices {
				if device.MachineKey == approval.MachineKey {
					http.Error(w, `{"error":"identity_already_enrolled"}`, 409)
					return
				}
			}
		}
		for _, retired := range s.Store.value.Retired {
			for _, device := range retired {
				if device.MachineKey == approval.MachineKey {
					http.Error(w, `{"error":"identity_retired"}`, 409)
					return
				}
			}
		}
		inventory, err := s.Backend.Inventory(ctx)
		if err != nil {
			http.Error(w, `{"error":"coordinator_unavailable"}`, 503)
			return
		}
		for _, node := range inventory {
			if node.MachineKey == approval.MachineKey {
				http.Error(w, `{"error":"identity_already_registered"}`, 409)
				return
			}
		}
		hasPond := false
		activePond := false
		for _, d := range devices {
			if d.Role == "pond" {
				hasPond = true
				activePond = d.Status == "active"
			}
		}
		if (approval.Role == "pond" && hasPond) || (approval.Role == "phone" && !activePond) {
			http.Error(w, `{"error":"pond_registration_required"}`, 409)
			return
		}
	} else if approval.Action != "revoke" || (!present && (approval.Role != "phone" || len(devices) >= 256)) {
		http.Error(w, `{"error":"invalid_action"}`, 400)
		return
	}
	s.consume(requestID, approval.Expires, now)
	if approval.Action == "enroll" || replacing {
		if replacing {
			// Retain the old binding permanently, including after a late registration.
			current.Status = "revoked"
			s.Store.value.Retired[approval.Household] = append(s.Store.value.Retired[approval.Household], current)
		}
		devices[approval.Device] = Device{Role: approval.Role, Status: "pending", MachineKey: approval.MachineKey, Key: approval.NodeKey, Revision: rand.Text()}
	} else {
		if !present {
			current.Role = "phone"
		}
		current.Status = "revoking"
		current.Revision = rand.Text()
		devices[approval.Device] = current
	}
	if err = s.Store.save(); err != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	// Persist consumption before an external mutation. Ambiguous registration is never replayed.
	if approval.Action == "revoke" {
		if err = s.policy(ctx); err == nil {
			err = s.deleteOwned(ctx, current, h.UserID)
		}
		if err == nil {
			current = Device{Role: current.Role, Status: "revoked", MachineKey: current.MachineKey, Revision: current.Revision}
			devices[approval.Device] = current
			err = s.Store.save()
		}
	} else {
		if replacing {
			// Exclude the old identity before permitting the new registration. A failure
			// leaves a recoverable pending intent; never replay the coordinator mutation.
			err = s.policy(ctx)
			if err == nil {
				err = s.deleteOwned(ctx, current, h.UserID)
			}
		}
		var node Registered
		if err == nil {
			node, err = s.Backend.Register(ctx, h.UserID, approval.AuthID)
		}
		if errors.Is(err, ErrRegistrationRejected) {
			failed := devices[approval.Device]
			failed.Status = "failed"
			devices[approval.Device] = failed
			if saveErr := s.Store.save(); saveErr != nil {
				err = saveErr
			}
		}
		if err == nil {
			var active Device
			active, err = s.validateRegistered(node, devices[approval.Device], h.UserID)
			if err == nil {
				devices[approval.Device] = active
				if err = s.Store.save(); err == nil {
					err = s.policy(ctx)
				}
			} else {
				failed := devices[approval.Device]
				failed.Status = "failed"
				devices[approval.Device] = failed
				if saveErr := s.Store.save(); saveErr != nil {
					err = saveErr
				}
			}
		}
	}
	if err != nil {
		slog.Warn("enrollment transition incomplete", "action", approval.Action)
		http.Error(w, `{"error":"enrollment_incomplete"}`, 503)
		return
	}
	slog.Info("enrollment transition completed", "action", approval.Action)
	json.NewEncoder(w).Encode(devices[approval.Device])
}

// Reconcile recovers a lost registration response by its approved machine key,
// and retries revocations. It never repeats an ambiguous registration write.
func (s *Service) Reconcile(ctx context.Context) error {
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	if err := s.policy(ctx); err != nil {
		return err
	}
	inventory, err := s.Backend.Inventory(ctx)
	if err != nil {
		return err
	}
	for household, devices := range s.Store.value.Devices {
		for id, device := range devices {
			if device.Status == "pending" && device.MachineKey != "" {
				var matches []Registered
				for _, node := range inventory {
					if node.MachineKey == device.MachineKey {
						matches = append(matches, node)
					}
				}
				if len(matches) == 1 {
					active, err := s.validateRegistered(matches[0], device, s.Store.value.Households[household].UserID)
					if err != nil {
						continue
					}
					devices[id] = active
					if err = s.Store.save(); err != nil {
						return err
					}
					slog.Info("enrollment registration reconciled")
				}
				continue
			}
			if device.Status != "revoking" && !(device.Status == "revoked" && device.MachineKey != "") {
				continue
			}
			if err := s.deleteOwnedFrom(ctx, device, s.Store.value.Households[household].UserID, inventory); err != nil {
				return err
			}
			if device.Status == "revoked" {
				continue
			}
			device = Device{Role: device.Role, Status: "revoked", MachineKey: device.MachineKey, Revision: device.Revision}
			devices[id] = device
			if err := s.Store.save(); err != nil {
				return err
			}
		}
	}
	for household, retired := range s.Store.value.Retired {
		for _, device := range retired {
			if err := s.deleteOwnedFrom(ctx, device, s.Store.value.Households[household].UserID, inventory); err != nil {
				return err
			}
		}
	}
	return s.policy(ctx)
}

func (s *Service) consume(requestID string, expiry, now int64) {
	for k, expires := range s.Store.value.Requests {
		if expires <= now {
			delete(s.Store.value.Requests, k)
		}
	}
	s.Store.value.Requests[requestID] = expiry
}

func (s *Service) validateRegistered(node Registered, pending Device, user string) (Device, error) {
	invalid := errors.New("coordinator returned an unexpected node")
	address := ""
	for _, value := range node.Addresses {
		if ip, err := netip.ParseAddr(value); err == nil && netip.MustParsePrefix("100.64.0.0/10").Contains(ip) {
			address = ip.String()
			break
		}
	}
	if node.MachineKey != pending.MachineKey || (pending.Key != "" && node.Key != pending.Key) || !nodeKeyPattern.MatchString(node.Key) || node.UserID != user || !numeric.MatchString(node.ID) || address == "" || len(node.Tags) != 0 || len(node.Routes) != 0 {
		return Device{}, invalid
	}
	for _, devices := range s.Store.value.Devices {
		for _, existing := range devices {
			if existing.NodeID != "" && (existing.NodeID == node.ID || existing.Address == address || existing.MachineKey == node.MachineKey) {
				return Device{}, invalid
			}
		}
	}
	return Device{Key: node.Key, MachineKey: node.MachineKey, NodeID: node.ID, Address: address, Role: pending.Role, Status: "active", Revision: pending.Revision}, nil
}

// A restored stale node ID must never delete a replacement belonging to another identity.
func (s *Service) deleteOwned(ctx context.Context, device Device, user string) error {
	nodes, err := s.Backend.Inventory(ctx)
	if err != nil {
		return err
	}
	return s.deleteOwnedFrom(ctx, device, user, nodes)
}

// Revoked machine identities remain tombstoned so a late Headscale registration
// can be removed even if the first revocation preceded its completion.
func (s *Service) deleteOwnedFrom(ctx context.Context, device Device, user string, nodes []Registered) error {
	for _, node := range nodes {
		matched := device.NodeID != "" && node.ID == device.NodeID && node.Key == device.Key
		if device.NodeID == "" && device.MachineKey != "" {
			matched = node.MachineKey == device.MachineKey
		}
		if matched && node.UserID == user && (device.MachineKey == "" || node.MachineKey == device.MachineKey) {
			if err := s.Backend.Delete(ctx, node.ID); err != nil {
				return err
			}
		}
	}
	return nil
}
