package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"
)

type fakeBackend struct {
	users        map[string]string
	created      int
	nodes        []Registered
	calls        int
	deleted      int
	rules        []Rule
	fail         bool
	lostResponse bool
	rejected     bool
}

func (f *fakeBackend) Register(_ context.Context, user, auth string) (Registered, error) {
	f.calls++
	if f.rejected {
		return Registered{}, ErrRegistrationRejected
	}
	if f.fail {
		return Registered{}, errors.New("offline")
	}
	n := Registered{ID: "1", UserID: user, Key: "nodekey:" + strings.Repeat("a", 64), MachineKey: "mkey:" + strings.Repeat("b", 64), Addresses: []string{"100.64.0.1"}}
	f.nodes = append(f.nodes, n)
	if f.lostResponse {
		return Registered{}, errors.New("registration response lost")
	}
	return n, nil
}
func (f *fakeBackend) Inventory(context.Context) ([]Registered, error) { return f.nodes, nil }
func (f *fakeBackend) EnsureUser(_ context.Context, name string) (string, error) {
	if f.fail {
		return "", errors.New("offline")
	}
	if id, ok := f.users[name]; ok {
		return id, nil
	}
	if f.users == nil {
		f.users = map[string]string{}
	}
	f.created++
	id := strconv.Itoa(len(f.users) + 1)
	f.users[name] = id
	return id, nil
}
func (f *fakeBackend) Delete(_ context.Context, id string) error {
	f.deleted++
	kept := f.nodes[:0]
	for _, node := range f.nodes {
		if node.ID != id {
			kept = append(kept, node)
		}
	}
	f.nodes = kept
	return nil
}
func (f *fakeBackend) Policy(_ context.Context, r []Rule) error { f.rules = r; return nil }
func fixture(t *testing.T) (*Service, ed25519.PrivateKey, *fakeBackend) {
	t.Helper()
	dir := filepath.Join(t.TempDir(), "state")
	s, e := Open(dir)
	if e != nil {
		t.Fatal(e)
	}
	t.Cleanup(func() { s.Close() })
	pub, key, _ := ed25519.GenerateKey(rand.Reader)
	e = s.Provision("household00000001", Household{PublicKey: base64.StdEncoding.EncodeToString(pub), UserID: "1", Port: 4443})
	if e != nil {
		t.Fatal(e)
	}
	b := &fakeBackend{}
	svc, e := New(context.Background(), s, b)
	if e != nil {
		t.Fatal(e)
	}
	return svc, key, b
}
func approval() Approval {
	return Approval{Household: "household00000001", Device: "device0000000001", Action: "enroll", Role: "pond", AuthID: "pending000000001", NodeKey: "nodekey:" + strings.Repeat("a", 64), MachineKey: "mkey:" + strings.Repeat("b", 64), Nonce: "nonce00000000001", Expires: time.Now().Add(time.Minute).Unix()}
}
func invoke(s *Service, a Approval, key ed25519.PrivateKey) int {
	env, _ := Sign(a, key)
	data, _ := json.Marshal(env)
	r := httptest.NewRequest("POST", "/v1/approval", bytes.NewReader(data))
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	return w.Code
}
func TestInvalidAuthorityNeverContactsCoordinator(t *testing.T) {
	for _, kind := range []string{"wrong_key", "expired", "far_future", "wrong_household", "tampered_node"} {
		t.Run(kind, func(t *testing.T) {
			s, k, b := fixture(t)
			a := approval()
			switch kind {
			case "wrong_key":
				_, k, _ = ed25519.GenerateKey(rand.Reader)
			case "expired":
				a.Expires = time.Now().Add(-time.Minute).Unix()
			case "far_future":
				a.Expires = time.Now().Add(time.Hour).Unix()
			case "wrong_household":
				a.Household = "household00000002"
			case "tampered_node":
				a.NodeKey = "bad"
			}
			if code := invoke(s, a, k); code < 400 {
				t.Fatalf("accepted %s", kind)
			}
			if b.calls != 0 {
				t.Fatal("registration attempted")
			}
		})
	}
}
func TestConcurrentApprovalIsConsumedOnce(t *testing.T) {
	s, k, b := fixture(t)
	a := approval()
	var wg sync.WaitGroup
	codes := make(chan int, 2)
	for i := 0; i < 2; i++ {
		wg.Add(1)
		go func() { defer wg.Done(); codes <- invoke(s, a, k) }()
	}
	wg.Wait()
	close(codes)
	ok, conflict := 0, 0
	for c := range codes {
		if c == 200 {
			ok++
		}
		if c == 409 {
			conflict++
		}
	}
	if ok != 1 || conflict != 1 || b.calls != 1 {
		t.Fatalf("ok=%d conflict=%d calls=%d", ok, conflict, b.calls)
	}
}
func TestAmbiguousRegistrationIsNotReplayed(t *testing.T) {
	s, k, b := fixture(t)
	b.fail = true
	a := approval()
	if invoke(s, a, k) != 503 {
		t.Fatal("failure not surfaced")
	}
	a.Nonce = "nonce00000000002"
	if invoke(s, a, k) != 409 || b.calls != 1 {
		t.Fatal("ambiguous write replayed")
	}
}

func TestLostRegistrationResponseRecoversAfterRestartWithoutReplay(t *testing.T) {
	s, k, b := fixture(t)
	b.lostResponse = true
	a := approval()
	a.NodeKey = "" // The client has only its stable public machine identity before enrollment.
	if invoke(s, a, k) != 503 {
		t.Fatal("lost response must remain visible")
	}
	if s.Store.value.Devices[a.Household][a.Device].Status != "pending" {
		t.Fatal("intent not retained")
	}
	directory := s.Store.directory
	s.Store.Close()
	stored, err := Open(directory)
	if err != nil {
		t.Fatal(err)
	}
	defer stored.Close()
	restarted, err := New(context.Background(), stored, b)
	if err != nil {
		t.Fatal(err)
	}
	if err = restarted.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if stored.value.Devices[a.Household][a.Device].Status != "active" || b.calls != 1 {
		t.Fatal("recovery replayed registration or failed")
	}
	if invoke(restarted, a, k) != 409 {
		t.Fatal("consumed approval replayed")
	}
}

func TestRecoveryRejectsCoordinatorIdentityDrift(t *testing.T) {
	for _, drift := range []string{"machine", "owner", "node_key", "tags", "routes", "duplicate_machine", "claimed_address", "invalid_address"} {
		t.Run(drift, func(t *testing.T) {
			s, k, b := fixture(t)
			b.lostResponse = true
			a := approval()
			if invoke(s, a, k) != 503 {
				t.Fatal("expected lost response")
			}
			switch drift {
			case "machine":
				b.nodes[0].MachineKey = "mkey:" + strings.Repeat("c", 64)
			case "owner":
				b.nodes[0].UserID = "2"
			case "node_key":
				b.nodes[0].Key = "nodekey:" + strings.Repeat("c", 64)
			case "tags":
				b.nodes[0].Tags = []string{"tag:unexpected"}
			case "routes":
				b.nodes[0].Routes = []string{"0.0.0.0/0"}
			case "duplicate_machine":
				b.nodes = append(b.nodes, b.nodes[0])
			case "claimed_address":
				s.Store.value.Devices[a.Household]["device0000000002"] = Device{NodeID: "2", Address: "100.64.0.1", Status: "active", Role: "phone"}
			case "invalid_address":
				b.nodes[0].Addresses = []string{"192.168.1.2"}
			}
			if err := s.Reconcile(context.Background()); err != nil {
				t.Fatal(err)
			}
			if s.Store.value.Devices[a.Household][a.Device].Status != "pending" || b.calls != 1 || b.deleted != 0 || len(b.rules) != 0 {
				t.Fatal("unsafe registration recovered")
			}
		})
	}
}

func TestRevocationWinsOverDelayedRegistrationAndRecovery(t *testing.T) {
	s, k, b := fixture(t)
	b.lostResponse = true
	a := approval()
	if invoke(s, a, k) != 503 {
		t.Fatal("expected pending registration")
	}
	late := b.nodes[0]
	b.nodes = nil // Coordinator inventory has not observed the registration yet.
	a.Action, a.Nonce = "revoke", "nonce00000000002"
	if invoke(s, a, k) != 200 {
		t.Fatal("revocation failed")
	}
	b.nodes = append(b.nodes, late)
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(b.nodes) != 0 || b.deleted != 1 || b.calls != 1 || s.Store.value.Devices[a.Household][a.Device].Status != "revoked" {
		t.Fatal("delayed node survived revocation")
	}
	a.Action, a.Nonce = "enroll", "nonce00000000003"
	if invoke(s, a, k) != 409 {
		t.Fatal("revoked approval revived")
	}
}

func TestMachineIdentityCannotEnrollTwice(t *testing.T) {
	s, k, b := fixture(t)
	a := approval()
	if invoke(s, a, k) != 200 {
		t.Fatal("initial registration")
	}
	a.Device, a.Role, a.Nonce = "device0000000002", "phone", "nonce00000000002"
	if invoke(s, a, k) != 409 || b.calls != 1 {
		t.Fatal("same machine enrolled again")
	}
}

func TestEnrollmentRequiresPublicMachineBinding(t *testing.T) {
	s, k, b := fixture(t)
	a := approval()
	a.MachineKey = ""
	if invoke(s, a, k) != 409 || b.calls != 0 {
		t.Fatal("unbound registration accepted")
	}
}

func TestRecoveryCannotClaimPreexistingUnmanagedNode(t *testing.T) {
	s, k, b := fixture(t)
	if _, err := b.Register(context.Background(), "1", "fixture"); err != nil {
		t.Fatal(err)
	}
	before := b.calls
	if invoke(s, approval(), k) != 409 || b.calls != before {
		t.Fatal("preexisting node claimed")
	}
	if len(s.Store.value.Devices[approval().Household]) != 0 {
		t.Fatal("unapproved recovery intent recorded")
	}
}

func TestDefiniteRejectionCannotRecoverFromLaterInventory(t *testing.T) {
	s, k, b := fixture(t)
	b.rejected = true
	a := approval()
	if invoke(s, a, k) != 503 {
		t.Fatal("expected rejected registration")
	}
	b.rejected = false
	if _, err := b.Register(context.Background(), "1", "unrelated-later-registration"); err != nil {
		t.Fatal(err)
	}
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if s.Store.value.Devices[a.Household][a.Device].Status != "failed" || len(b.rules) != 0 {
		t.Fatal("definite failure became an approval")
	}
}
func TestPolicySeparatesHouseholdsAndOnlyPermitsCompanionPort(t *testing.T) {
	s, _, b := fixture(t)
	s.Store.value.Households["household00000002"] = Household{Port: 4444}
	for i, h := range []string{"household00000001", "household00000002"} {
		ip := []string{"100.64.0.1", "100.64.0.3"}[i]
		phone := []string{"100.64.0.2", "100.64.0.4"}[i]
		key := "nodekey:" + strings.Repeat("a", 64)
		user := strconv.Itoa(i + 1)
		household := s.Store.value.Households[h]
		household.UserID = user
		s.Store.value.Households[h] = household
		pondID, phoneID := strconv.Itoa(i*2+1), strconv.Itoa(i*2+2)
		s.Store.value.Devices[h] = map[string]Device{"pond": {NodeID: pondID, Key: key, Role: "pond", Status: "active", Address: ip}, "phone": {NodeID: phoneID, Key: key, Role: "phone", Status: "active", Address: phone}}
		b.nodes = append(b.nodes, Registered{ID: pondID, Key: key, UserID: user, Addresses: []string{ip}}, Registered{ID: phoneID, Key: key, UserID: user, Addresses: []string{phone}})
	}
	if e := s.policy(context.Background()); e != nil {
		t.Fatal(e)
	}
	if len(b.rules) != 2 {
		t.Fatal(b.rules)
	}
	for _, r := range b.rules {
		if len(r.Src) != 1 || len(r.Dst) != 1 || strings.Contains(r.Dst[0], "*") {
			t.Fatal(r)
		}
		if r.Src[0] == "100.64.0.2" && r.Dst[0] != "100.64.0.1:4443" {
			t.Fatal(r)
		}
	}
}
func TestStatePersistenceAndExclusiveOwnership(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "state")
	s, e := Open(dir)
	if e != nil {
		t.Fatal(e)
	}
	if other, e := Open(dir); e == nil {
		other.Close()
		t.Fatal("duplicate process accepted")
	}
	pub, _, _ := ed25519.GenerateKey(rand.Reader)
	if e = s.Provision("household00000001", Household{base64.StdEncoding.EncodeToString(pub), "1", 4443}); e != nil {
		t.Fatal(e)
	}
	s.Close()
	s, e = Open(dir)
	if e != nil {
		t.Fatal(e)
	}
	if len(s.value.Households) != 1 {
		t.Fatal("state lost")
	}
	s.Close()
	os.Remove(filepath.Join(dir, "state.json"))
	if s, e = Open(dir); e == nil {
		s.Close()
		t.Fatal("missing identity silently replaced")
	}
}
func TestRevocationRemovesPermissionBeforeNode(t *testing.T) {
	s, k, b := fixture(t)
	a := approval()
	if invoke(s, a, k) != 200 {
		t.Fatal("enroll")
	}
	a.Action = "revoke"
	a.Nonce = "nonce00000000002"
	if invoke(s, a, k) != 200 || b.deleted != 1 {
		t.Fatal("revoke")
	}
	if len(b.rules) != 0 {
		t.Fatal("permissions remain")
	}
}

func TestPendingPondCannotAuthorizePhone(t *testing.T) {
	s, k, b := fixture(t)
	b.fail = true
	a := approval()
	if invoke(s, a, k) != 503 {
		t.Fatal("pending Pond expected")
	}
	b.fail = false
	a.Role = "phone"
	a.Device = "device0000000002"
	a.Nonce = "nonce00000000002"
	if invoke(s, a, k) != 409 || b.calls != 1 {
		t.Fatal("phone enrolled before Pond became active")
	}
}
func TestStorageFailureStopsLaterMutations(t *testing.T) {
	s, k, b := fixture(t)
	// Renaming the private directory forces persistence failure without relying on root permissions.
	if err := os.Rename(s.Store.directory, s.Store.directory+"-offline"); err != nil {
		t.Fatal(err)
	}
	a := approval()
	if invoke(s, a, k) != 503 {
		t.Fatal("storage failure ignored")
	}
	os.Rename(s.Store.directory+"-offline", s.Store.directory)
	a.Nonce = "nonce00000000002"
	if invoke(s, a, k) != 503 || b.calls != 0 {
		t.Fatal("continued with uncommitted security state")
	}
}
func TestCorruptSemanticStateFailsBeforePolicy(t *testing.T) {
	s, _, _ := fixture(t)
	s.Store.value.Devices["household00000001"]["device0000000001"] = Device{NodeID: "1", Role: "pond", Status: "active", Address: "0.0.0.0/0"}
	if err := validateState(s.Store.value); err == nil {
		t.Fatal("network wildcard accepted from disk")
	}
}

func TestRevokeBeforeRegistrationPreventsLateEnrollment(t *testing.T) {
	s, k, b := fixture(t)
	a := approval()
	a.Action, a.Role = "revoke", "phone"
	if code := invoke(s, a, k); code != 200 {
		t.Fatalf("revoke: %d", code)
	}
	a.Action, a.Nonce = "enroll", "nonce00000000002"
	if code := invoke(s, a, k); code != 409 {
		t.Fatalf("late enrollment: %d", code)
	}
	if b.calls != 0 || b.deleted != 0 {
		t.Fatal("unknown device caused network mutation")
	}
	a.Action, a.Nonce = "revoke", "nonce00000000003"
	if code := invoke(s, a, k); code != 200 {
		t.Fatalf("idempotent revoke: %d", code)
	}
}

func TestCoordinatorIdentityDriftRemovesPermission(t *testing.T) {
	s, _, b := fixture(t)
	key := "nodekey:" + strings.Repeat("a", 64)
	s.Store.value.Devices["household00000001"] = map[string]Device{
		"pond":  {NodeID: "1", Key: key, Role: "pond", Status: "active", Address: "100.64.0.1"},
		"phone": {NodeID: "2", Key: key, Role: "phone", Status: "active", Address: "100.64.0.2"}}
	b.nodes = []Registered{{ID: "1", Key: key, UserID: "1", Addresses: []string{"100.64.0.1"}}, {ID: "2", Key: key, UserID: "1", Addresses: []string{"100.64.0.2"}}}
	if err := s.policy(context.Background()); err != nil || len(b.rules) != 1 {
		t.Fatal("expected initial permission", err, b.rules)
	}
	b.nodes[1].Key = "nodekey:" + strings.Repeat("b", 64)
	if err := s.policy(context.Background()); err != nil || len(b.rules) != 0 {
		t.Fatal("reassigned node inherited permission", err, b.rules)
	}
}
