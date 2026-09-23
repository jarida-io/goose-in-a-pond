package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"net/http/httptest"
	"path/filepath"
	"testing"
	"time"
)

func emptyService(t *testing.T) (*Service, *fakeBackend) {
	t.Helper()
	store, err := Open(filepath.Join(t.TempDir(), "state"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { store.Close() })
	backend := &fakeBackend{}
	service, err := New(context.Background(), store, backend)
	if err != nil {
		t.Fatal(err)
	}
	return service, backend
}

func register(s *Service, envelope Envelope, from string) (int, string) {
	data, _ := json.Marshal(envelope)
	r := httptest.NewRequest("POST", "/v1/household", bytes.NewReader(data))
	if from != "" {
		r.RemoteAddr = from
	}
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	var body struct {
		Household string `json:"household"`
	}
	_ = json.Unmarshal(w.Body.Bytes(), &body)
	return w.Code, body.Household
}

func registration(public ed25519.PublicKey) HouseholdRegistration {
	return HouseholdRegistration{
		PublicKey: base64.StdEncoding.EncodeToString(public),
		Port:      4443,
		Expires:   time.Now().Add(time.Minute).Unix(),
	}
}

func TestAHouseholdRegistersItselfAndTheServerNamesIt(t *testing.T) {
	service, backend := emptyService(t)
	public, key, _ := ed25519.GenerateKey(rand.Reader)

	envelope, _ := SignHousehold(registration(public), key)
	code, id := register(service, envelope, "198.51.100.7:1234")
	if code != 200 {
		t.Fatalf("registration rejected: %d", code)
	}
	// The name is derived from the key, never taken from the caller, so a
	// registration can only ever name the household that signed it.
	if id != HouseholdID(public) {
		t.Fatalf("household was not named after its key: %q", id)
	}
	stored, ok := service.Store.value.Households[id]
	if !ok || stored.UserID == "" || stored.Port != 4443 {
		t.Fatalf("household not stored: %+v", stored)
	}
	if backend.created != 1 {
		t.Fatalf("expected one coordinator user, got %d", backend.created)
	}

	// Registering again is how a household recovers from a lost response.
	again, _ := SignHousehold(registration(public), key)
	if code, second := register(service, again, "198.51.100.8:1234"); code != 200 || second != id {
		t.Fatalf("re-registration was not idempotent: %d %q", code, second)
	}
	if backend.created != 1 {
		t.Fatalf("re-registration created another user: %d", backend.created)
	}
}

func TestRegistrationRefusesWhatItCannotProve(t *testing.T) {
	public, _, _ := ed25519.GenerateKey(rand.Reader)
	_, other, _ := ed25519.GenerateKey(rand.Reader)

	for _, kind := range []string{"wrong_key", "expired", "far_future", "no_port", "approval_signature"} {
		t.Run(kind, func(t *testing.T) {
			service, backend := emptyService(t)
			body := registration(public)
			var envelope Envelope
			switch kind {
			case "wrong_key":
				// Signed by a key that is not the one being registered.
				envelope, _ = SignHousehold(body, other)
			case "expired":
				body.Expires = time.Now().Add(-time.Minute).Unix()
				envelope, _ = SignHousehold(body, other)
			case "far_future":
				body.Expires = time.Now().Add(time.Hour).Unix()
				envelope, _ = SignHousehold(body, other)
			case "no_port":
				body.Port = 0
				envelope, _ = SignHousehold(body, other)
			case "approval_signature":
				// An enrollment approval signature must not be presentable here.
				// The domain separation is what stops one being replayed as the
				// other.
				payload, _ := json.Marshal(body)
				envelope = Envelope{
					Payload:   base64.StdEncoding.EncodeToString(payload),
					Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(other, append([]byte("goose-enrollment-v1\x00"), payload...))),
				}
			}
			if code, _ := register(service, envelope, "203.0.113.5:9999"); code != 403 {
				t.Fatalf("expected 403, got %d", code)
			}
			if backend.created != 0 {
				t.Fatal("a rejected registration reached the coordinator")
			}
			if len(service.Store.value.Households) != 0 {
				t.Fatal("a rejected registration was stored")
			}
		})
	}
}

func TestRegistrationIsRateLimitedPerSource(t *testing.T) {
	service, _ := emptyService(t)
	limited := false
	for attempt := 0; attempt < 8; attempt++ {
		public, key, _ := ed25519.GenerateKey(rand.Reader)
		envelope, _ := SignHousehold(registration(public), key)
		if code, _ := register(service, envelope, "192.0.2.50:4321"); code == 429 {
			limited = true
			break
		}
	}
	if !limited {
		t.Fatal("one source registered household after household without being limited")
	}
	// A different source is unaffected by that one's budget.
	public, key, _ := ed25519.GenerateKey(rand.Reader)
	envelope, _ := SignHousehold(registration(public), key)
	if code, _ := register(service, envelope, "192.0.2.51:4321"); code != 200 {
		t.Fatalf("an unrelated source was limited: %d", code)
	}
}
