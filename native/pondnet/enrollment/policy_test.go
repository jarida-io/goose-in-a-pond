package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
)

// captureLogs sends the default logger to a buffer for the rest of the test.
func captureLogs(t *testing.T) *bytes.Buffer {
	t.Helper()
	var buffer bytes.Buffer
	previous := slog.Default()
	slog.SetDefault(slog.New(slog.NewTextHandler(&buffer, &slog.HandlerOptions{Level: slog.LevelDebug})))
	t.Cleanup(func() { slog.SetDefault(previous) })
	return &buffer
}

// policyKey is the node key every fixture node in this file carries.
var policyKey = "nodekey:" + strings.Repeat("a", 64)

// activePair records an active Pond and phone for the fixture household, with the
// coordinator nodes that verify them, so the policy holds exactly one rule.
func activePair(s *Service, backend *fakeBackend) {
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	s.Store.value.Devices["household00000001"] = map[string]Device{
		"pond":  {NodeID: "1", Key: policyKey, Role: "pond", Status: "active", Address: "100.64.0.1"},
		"phone": {NodeID: "2", Key: policyKey, Role: "phone", Status: "active", Address: "100.64.0.2"},
	}
	backend.nodes = []Registered{
		{ID: "1", Key: policyKey, UserID: "1", Addresses: []string{"100.64.0.1"}},
		{ID: "2", Key: policyKey, UserID: "1", Addresses: []string{"100.64.0.2"}},
	}
}

// settled is the fixture with an active pair whose policy the coordinator already holds,
// and the backend's counters cleared.
func settled(t *testing.T) (*Service, *fakeBackend) {
	t.Helper()
	s, _, backend := fixture(t)
	activePair(s, backend)
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(backend.rules) != 1 {
		t.Fatalf("the pair produced %d rules, want 1", len(backend.rules))
	}
	backend.policyWrites, backend.inventoryReads = 0, 0
	return s, backend
}

// An idle reconciliation reads the inventory and the coordinator's policy once each
// and writes nothing, and a restart against the same coordinator does not write either.
func TestAnUnchangedPolicyIsNeverWrittenAgain(t *testing.T) {
	s, backend := settled(t)
	for i := 1; i <= 5; i++ {
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
		if backend.policyWrites != 0 || backend.inventoryReads != i {
			t.Fatalf("after %d idle passes: %d policy writes, %d inventory reads", i, backend.policyWrites, backend.inventoryReads)
		}
	}
	if _, err := New(context.Background(), s.Store, backend); err != nil {
		t.Fatal(err)
	}
	if backend.policyWrites != 0 {
		t.Fatal("a restart rewrote an unchanged policy")
	}
}

func TestAStoreChangeWritesThePolicyOnce(t *testing.T) {
	s, _, backend := fixture(t)
	backend.policyWrites = 0
	logs := captureLogs(t)
	activePair(s, backend)
	for range 3 {
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if backend.policyWrites != 1 || len(backend.rules) != 1 {
		t.Fatalf("%d policy writes and %d rules, want 1 and 1", backend.policyWrites, len(backend.rules))
	}
	if !strings.Contains(logs.String(), "kind=policy_store_changed") || strings.Contains(logs.String(), "policy_drift") {
		t.Fatalf("a store change was not reported as one:\n%s", logs)
	}
}

// A policy changed on the coordinator by anything else is put back, once, with a
// warning that counts what differed and names no address.
func TestCoordinatorPolicyDriftIsRestoredAndReported(t *testing.T) {
	s, backend := settled(t)
	logs := captureLogs(t)
	backend.rules = []Rule{{Action: "accept", Src: []string{"*"}, Dst: []string{"*:*"}}, {Action: "accept", Src: []string{"*"}, Dst: []string{"*:22"}}}
	for range 3 {
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if backend.policyWrites != 1 || len(backend.rules) != 1 || backend.rules[0].Src[0] != "100.64.0.2" {
		t.Fatalf("drift not restored exactly once: %d writes, rules %v", backend.policyWrites, backend.rules)
	}
	out := logs.String()
	for _, want := range []string{"level=WARN", "kind=policy_drift", "store_changed=false", "coordinator_rules=2", "computed_rules=1"} {
		if !strings.Contains(out, want) {
			t.Fatalf("drift log lacks %s:\n%s", want, out)
		}
	}
	if strings.Contains(out, "100.64.") {
		t.Fatalf("drift log names an address:\n%s", out)
	}
}

// A coordinator policy that cannot be read is written anyway, every time: enforcing the
// allowlist again is cheaper than leaving it unenforced.
func TestAnUnreadableCoordinatorPolicyIsWritten(t *testing.T) {
	s, backend := settled(t)
	logs := captureLogs(t)
	backend.policyReadErr = errors.New("coordinator rejected operation: HTTP 503")
	for range 2 {
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if backend.policyWrites != 2 || !strings.Contains(logs.String(), "kind=policy_read_failed") {
		t.Fatalf("%d writes while the policy was unreadable:\n%s", backend.policyWrites, logs)
	}
	if strings.Contains(healthOf(t, s), "degraded") {
		t.Fatal("a policy written despite an unreadable one left the service degraded")
	}
	backend.policyReadErr = nil
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if backend.policyWrites != 2 {
		t.Fatal("the policy was rewritten once it could be read again")
	}
}

// Reconcile evaluates the policy a second time exactly when the pass changed one of its
// inputs, so a registration it recovers is permitted in the same pass.
func TestReconcileReevaluatesThePolicyAfterAChange(t *testing.T) {
	t.Run("recovered_registration", func(t *testing.T) {
		s, _, backend := fixture(t)
		machine := "mkey:" + strings.Repeat("c", 64)
		s.Store.mu.Lock()
		s.Store.value.Devices["household00000001"] = map[string]Device{
			"pond":  {NodeID: "1", Key: policyKey, Role: "pond", Status: "active", Address: "100.64.0.1"},
			"phone": {Role: "phone", Status: "pending", MachineKey: machine},
		}
		s.Store.mu.Unlock()
		backend.nodes = []Registered{
			{ID: "1", Key: policyKey, UserID: "1", Addresses: []string{"100.64.0.1"}},
			{ID: "2", Key: policyKey, MachineKey: machine, UserID: "1", Addresses: []string{"100.64.0.2"}},
		}
		backend.policyWrites, backend.inventoryReads = 0, 0
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
		if backend.inventoryReads != 2 || backend.policyWrites != 1 || len(backend.rules) != 1 {
			t.Fatalf("%d inventory reads, %d writes, %d rules; the recovered phone was not permitted in the same pass", backend.inventoryReads, backend.policyWrites, len(backend.rules))
		}
	})
	t.Run("deleted_node", func(t *testing.T) {
		s, backend := settled(t)
		machine := "mkey:" + strings.Repeat("d", 64)
		s.Store.mu.Lock()
		s.Store.value.Devices["household00000001"]["old"] = Device{Role: "phone", Status: "revoked", MachineKey: machine}
		s.Store.mu.Unlock()
		backend.nodes = append(backend.nodes, Registered{ID: "3", Key: policyKey, MachineKey: machine, UserID: "1", Addresses: []string{"100.64.0.3"}})
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
		if backend.deleted != 1 || backend.inventoryReads != 2 || backend.policyWrites != 0 {
			t.Fatalf("%d deleted, %d inventory reads, %d writes", backend.deleted, backend.inventoryReads, backend.policyWrites)
		}
	})
}

// Headscale may hand the document back with other whitespace and key order; that is the
// same policy, and it is not written again.
func TestAReformattedCoordinatorPolicyIsNotRewritten(t *testing.T) {
	stored, writes := "", 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch {
		case r.Method == "GET" && r.URL.Path == "/api/v1/node":
			io.WriteString(w, `{"nodes":[`+
				`{"id":"1","nodeKey":"`+policyKey+`","ipAddresses":["100.64.0.1"],"user":{"id":"1"}},`+
				`{"id":"2","nodeKey":"`+policyKey+`","ipAddresses":["100.64.0.2"],"user":{"id":"1"}}]}`)
		case r.Method == "GET" && r.URL.Path == "/api/v1/policy":
			if stored == "" {
				http.Error(w, `{"message":"loading ACL from database"}`, http.StatusInternalServerError)
				return
			}
			var document any
			if err := json.Unmarshal([]byte(stored), &document); err != nil {
				t.Error(err)
			}
			// Indented, and with every object's keys sorted, unlike what was written.
			reformatted, _ := json.MarshalIndent(document, "", "\t")
			if string(reformatted) == stored || !strings.Contains(stored, `"src"`) {
				t.Error("the fixture did not reformat a non-trivial policy")
			}
			json.NewEncoder(w).Encode(map[string]string{"policy": string(reformatted), "updatedAt": "2026-10-28T09:00:00Z"})
		case r.Method == "PUT" && r.URL.Path == "/api/v1/policy":
			var body struct {
				Policy string `json:"policy"`
			}
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Error(err)
			}
			stored = body.Policy
			writes++
			io.WriteString(w, `{}`)
		default:
			t.Errorf("unexpected coordinator request %s %s", r.Method, r.URL.Path)
		}
	}))
	defer server.Close()
	backend, err := NewHeadscale(server.URL, "fixture-credential")
	if err != nil {
		t.Fatal(err)
	}
	store, err := Open(t.TempDir() + "/state")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	pub, _, _ := ed25519.GenerateKey(rand.Reader)
	if err := store.Provision("household00000001", Household{PublicKey: base64.StdEncoding.EncodeToString(pub), UserID: "1", Port: 4443}); err != nil {
		t.Fatal(err)
	}
	store.mu.Lock()
	store.value.Devices["household00000001"] = map[string]Device{
		"pond":  {NodeID: "1", Key: policyKey, Role: "pond", Status: "active", Address: "100.64.0.1"},
		"phone": {NodeID: "2", Key: policyKey, Role: "phone", Status: "active", Address: "100.64.0.2"},
	}
	store.mu.Unlock()
	s, err := New(context.Background(), store, backend)
	if err != nil {
		t.Fatal(err)
	}
	for range 3 {
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
	}
	if writes != 1 {
		t.Fatalf("%d policy writes, want only the first", writes)
	}
}
