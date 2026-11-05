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
	"reflect"
	"strconv"
	"strings"
	"sync"
	"testing"
	"time"

	"tailscale.com/tailcfg"
)

type fakeBackend struct {
	users   map[string]string
	created int
	nodes   []Registered
	calls   int
	deleted int
	rules   []Rule
	attrs   []NodeAttr
	// policyWrites counts Policy calls; policyReadErr, when set, fails CurrentPolicy.
	policyWrites   int
	inventoryReads int
	policyReadErr  error
	fail           bool
	lostResponse   bool
	rejected       bool
	// mutate, when set, alters each node the coordinator registers before it is stored.
	mutate func(*Registered)
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
	if f.mutate != nil {
		f.mutate(&n)
	}
	f.nodes = append(f.nodes, n)
	if f.lostResponse {
		return Registered{}, errors.New("registration response lost")
	}
	return n, nil
}
func (f *fakeBackend) Inventory(context.Context) ([]Registered, error) {
	f.inventoryReads++
	return f.nodes, nil
}
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
func (f *fakeBackend) Policy(_ context.Context, r []Rule, a []NodeAttr) error {
	f.policyWrites++
	f.rules, f.attrs = r, a
	return nil
}

// CurrentPolicy reads back the document Policy last stored, as Headscale does.
func (f *fakeBackend) CurrentPolicy(context.Context) ([]byte, error) {
	if f.policyReadErr != nil {
		return nil, f.policyReadErr
	}
	return policyDocument(f.rules, f.attrs)
}
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
	// The failed record is a tombstone for its machine identity.
	if len(b.nodes) != 0 || b.deleted != 1 {
		t.Fatalf("the later node under the failed identity survived: %d nodes, %d deleted", len(b.nodes), b.deleted)
	}
}

// A node the coordinator registered but the service refused is deleted, right away
// and again by Reconcile if it is still there, and its record stays failed.
func TestARefusedRegistrationIsFailedAndItsNodeDeleted(t *testing.T) {
	for name, mutate := range map[string]func(*Registered){
		"tags":   func(n *Registered) { n.Tags = []string{"tag:unexpected"} },
		"routes": func(n *Registered) { n.Routes = []string{"0.0.0.0/0"} },
		// Under another machine key, no tombstone matches it: only the request deletes it.
		"machine key": func(n *Registered) { n.MachineKey = "mkey:" + strings.Repeat("c", 64) },
	} {
		t.Run(name, func(t *testing.T) {
			s, k, b := fixture(t)
			b.mutate = mutate
			a := approval()
			if code := invoke(s, a, k); code != 503 {
				t.Fatalf("refused registration answered %d", code)
			}
			if s.Store.value.Devices[a.Household][a.Device].Status != "failed" {
				t.Fatal("refused registration not recorded as failed")
			}
			if len(b.nodes) != 0 || b.deleted != 1 {
				t.Fatalf("refused node not deleted: %d nodes, %d deleted", len(b.nodes), b.deleted)
			}
			// As if the immediate deletion had not reached the coordinator.
			b.nodes = append(b.nodes, Registered{ID: "1", UserID: "1", Key: a.NodeKey, MachineKey: a.MachineKey, Tags: []string{"tag:unexpected"}})
			if err := s.Reconcile(context.Background()); err != nil {
				t.Fatal(err)
			}
			if len(b.nodes) != 0 || b.deleted != 2 || b.calls != 1 {
				t.Fatalf("reconciliation left the refused node: %d nodes, %d deleted, %d registrations", len(b.nodes), b.deleted, b.calls)
			}
			if s.Store.value.Devices[a.Household][a.Device].Status != "failed" || len(b.rules) != 0 {
				t.Fatal("the tombstone changed status or gained permission")
			}
		})
	}
}

// A refused registration whose returned node a record already holds leaves that node alone.
func TestARefusedNodeThatARecordHoldsIsKept(t *testing.T) {
	s, k, b := fixture(t)
	held := "mkey:" + strings.Repeat("c", 64)
	a := approval()
	s.Store.value.Devices[a.Household] = map[string]Device{"device0000000002": {NodeID: "1", Key: a.NodeKey, MachineKey: held, Role: "phone", Status: "active", Address: "100.64.0.1"}}
	b.nodes = []Registered{{ID: "1", UserID: "1", Key: a.NodeKey, MachineKey: held, Addresses: []string{"100.64.0.1"}}}
	b.mutate = func(n *Registered) { n.MachineKey = held }
	if code := invoke(s, a, k); code != 503 {
		t.Fatalf("refused registration answered %d", code)
	}
	if s.Store.value.Devices[a.Household][a.Device].Status != "failed" {
		t.Fatal("refused registration not recorded as failed")
	}
	if b.deleted != 0 {
		t.Fatalf("a node a record holds was deleted: %d deleted", b.deleted)
	}
}

// A refused node registered to another user is never deleted, by the request or by
// Reconcile: the machine key alone does not make it this household's.
func TestARefusedNodeOwnedByAnotherUserIsKept(t *testing.T) {
	s, k, b := fixture(t)
	b.mutate = func(n *Registered) { n.UserID = "2" }
	a := approval()
	if code := invoke(s, a, k); code != 503 {
		t.Fatalf("refused registration answered %d", code)
	}
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if s.Store.value.Devices[a.Household][a.Device].Status != "failed" {
		t.Fatal("refused registration not recorded as failed")
	}
	if len(b.nodes) != 1 || b.deleted != 0 {
		t.Fatalf("another user's node was deleted: %d nodes, %d deleted", len(b.nodes), b.deleted)
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

// Only phones may start from a cached network map. A Pond that did would enforce
// the packet filter it last saw, so a phone revoked while it was down could pass.
func TestPolicyGrantsTheNetworkMapCacheToPhonesOnly(t *testing.T) {
	if cacheNetworkMaps != string(tailcfg.NodeAttrCacheNetworkMaps) {
		t.Fatalf("attribute %q is not the one the pinned tailscale reads (%q)", cacheNetworkMaps, tailcfg.NodeAttrCacheNetworkMaps)
	}
	s, _, b := fixture(t)
	key := "nodekey:" + strings.Repeat("a", 64)
	// The first household has two phones, the second a Pond and no phone at all.
	s.Store.value.Households["household00000002"] = Household{UserID: "2", Port: 4444}
	s.Store.value.Devices["household00000001"] = map[string]Device{
		"pond":   {NodeID: "1", Key: key, Role: "pond", Status: "active", Address: "100.64.0.1"},
		"phone2": {NodeID: "3", Key: key, Role: "phone", Status: "active", Address: "100.64.0.3"},
		"phone1": {NodeID: "2", Key: key, Role: "phone", Status: "active", Address: "100.64.0.2"},
		"gone":   {NodeID: "4", Key: key, Role: "phone", Status: "revoking", Address: "100.64.0.4"}}
	s.Store.value.Devices["household00000002"] = map[string]Device{
		"pond": {NodeID: "5", Key: key, Role: "pond", Status: "active", Address: "100.64.0.5"}}
	b.nodes = []Registered{
		{ID: "1", Key: key, UserID: "1", Addresses: []string{"100.64.0.1"}},
		{ID: "2", Key: key, UserID: "1", Addresses: []string{"100.64.0.2"}},
		{ID: "3", Key: key, UserID: "1", Addresses: []string{"100.64.0.3"}},
		{ID: "4", Key: key, UserID: "1", Addresses: []string{"100.64.0.4"}},
		{ID: "5", Key: key, UserID: "2", Addresses: []string{"100.64.0.5"}}}
	for range 8 { // map iteration order varies; the policy must not
		if err := s.policy(context.Background()); err != nil {
			t.Fatal(err)
		}
		want := []NodeAttr{{Target: []string{"100.64.0.2", "100.64.0.3"}, Attr: []string{"cache-network-maps"}}}
		if !reflect.DeepEqual(b.attrs, want) {
			t.Fatalf("node attributes %+v, want %+v", b.attrs, want)
		}
	}

	// With no verified phone anywhere, nothing is granted, and the Pond still is not.
	delete(s.Store.value.Devices, "household00000001")
	if err := s.policy(context.Background()); err != nil {
		t.Fatal(err)
	}
	if b.attrs == nil || len(b.attrs) != 0 {
		t.Fatalf("node attributes %+v, want an empty list", b.attrs)
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

// refusalOf is invoke for a request expected to be refused, returning the reason too.
func refusalOf(t *testing.T, s *Service, a Approval, key ed25519.PrivateKey) (int, string) {
	t.Helper()
	env, _ := Sign(a, key)
	data, _ := json.Marshal(env)
	w := httptest.NewRecorder()
	s.ServeHTTP(w, httptest.NewRequest("POST", "/v1/approval", bytes.NewReader(data)))
	var answer struct {
		Error string `json:"error"`
	}
	if err := json.Unmarshal(w.Body.Bytes(), &answer); err != nil {
		t.Fatalf("HTTP %d with an unreadable answer: %v", w.Code, err)
	}
	return w.Code, answer.Error
}

// retiredAfterReplacement replaces the fixture phone, retiring its old record, and puts
// the service on a clock the test moves.
func retiredAfterReplacement(t *testing.T) (*Service, ed25519.PrivateKey, *fakeBackend, Approval, *time.Time) {
	t.Helper()
	s, key, backend, a := replacementFixture(t, "revoked")
	if code := invoke(s, a, key); code != 200 {
		t.Fatalf("replacement HTTP %d", code)
	}
	if retired := s.Store.value.Retired[a.Household]; len(retired) != 1 || retired[0].AbsentSince != 0 {
		t.Fatalf("replacement did not retire one unchecked record: %+v", retired)
	}
	clock := time.Now()
	s.Now = func() time.Time { return clock }
	return s, key, backend, a, &clock
}

func reconciled(t *testing.T, s *Service) []Device {
	t.Helper()
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	return s.Store.value.Retired["household00000001"]
}

func TestRetiredRecordExpiresOnceItsIdentityStaysAbsent(t *testing.T) {
	s, key, backend, a, clock := retiredAfterReplacement(t)
	old := s.Store.value.Retired[a.Household][0].MachineKey
	// However long ago the replacement was, one read is one sighting, not a grace period.
	*clock = clock.Add(2 * retiredGrace)
	retired := reconciled(t, s)
	if len(retired) != 1 || retired[0].AbsentSince != clock.Unix() {
		t.Fatalf("a record seen absent once was not kept with the sighting: %+v", retired)
	}
	*clock = clock.Add(retiredGrace - time.Second)
	if retired = reconciled(t, s); len(retired) != 1 {
		t.Fatal("a record expired before its grace period ended")
	}
	*clock = clock.Add(time.Second)
	if retired = reconciled(t, s); len(retired) != 0 {
		t.Fatalf("a record absent for its whole grace period was kept: %+v", retired)
	}
	dir := s.Store.directory
	s.Store.Close()
	restored, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer restored.Close()
	if _, ok := restored.value.Retired[a.Household]; ok {
		t.Fatal("the expiry was not persisted")
	}
	// The expired key may enroll again, with the household's own approval for it.
	service, err := New(context.Background(), restored, backend)
	if err != nil {
		t.Fatal(err)
	}
	service.Now = s.Now
	a.Action, a.Device, a.Nonce, a.MachineKey, a.ExpectedRevision = "enroll", "anotherdevice0001", "expiredreuse00001", old, ""
	a.Expires = clock.Add(time.Minute).Unix()
	// Reaching registration is passing every identity check; the fake coordinator then
	// registers a different machine, which is not what this test is about.
	registered := backend.calls
	if code, reason := refusalOf(t, service, a, key); backend.calls != registered+1 {
		t.Fatalf("an expired identity was refused its new approval: HTTP %d %s", code, reason)
	}
}

func TestRetiredRecordIsKeptWhileItsIdentityIsHeld(t *testing.T) {
	s, _, backend, a, clock := retiredAfterReplacement(t)
	old := s.Store.value.Retired[a.Household][0].MachineKey
	reconciled(t, s)
	// A late registration under the old identity, after a grace period already ran out.
	*clock = clock.Add(retiredGrace + time.Hour)
	backend.nodes = append(backend.nodes, Registered{ID: "9", UserID: "1", MachineKey: old, Key: "nodekey:" + strings.Repeat("9", 64), Addresses: []string{"100.64.0.9"}})
	retired := reconciled(t, s)
	if len(retired) != 1 || retired[0].AbsentSince != 0 {
		t.Fatalf("a record whose identity a node holds was not kept and reset: %+v", retired)
	}
	for _, node := range backend.nodes {
		if node.MachineKey == old {
			t.Fatal("the late registration was not deleted")
		}
	}
	// The node it deleted counts as present in that pass; absence starts at the next read.
	if retired = reconciled(t, s); len(retired) != 1 || retired[0].AbsentSince != clock.Unix() {
		t.Fatalf("the next read did not start a fresh grace period: %+v", retired)
	}
}

func TestReplacementAtTheRetiredCapacityIsRefusedWithItsOwnReason(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	s.Store.value.Retired[a.Household] = make([]Device, retiredCapacity)
	if code, reason := refusalOf(t, s, a, key); code != 409 || reason != "retired_capacity" || backend.calls != 0 {
		t.Fatalf("HTTP %d %q with %d registrations", code, reason, backend.calls)
	}
	// A stale approval is still a conflict: the capacity is reported only for one that
	// would otherwise have been carried out.
	a.Nonce, a.ExpectedRevision = "staleatcapacity01", "staleversion00001"
	if code, reason := refusalOf(t, s, a, key); code != 409 || reason != "replacement_conflict" {
		t.Fatalf("HTTP %d %q", code, reason)
	}
}

func TestStateFromBeforeRetiredExpiryStillLoads(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "state")
	if err := os.Mkdir(dir, 0700); err != nil {
		t.Fatal(err)
	}
	key := base64.StdEncoding.EncodeToString(make([]byte, ed25519.PublicKeySize))
	old := `{"households":{"household00000001":{"publicKey":"` + key + `","userId":"1","port":4443}},` +
		`"devices":{"household00000001":{"device0000000001":{"machineKey":"mkey:` + strings.Repeat("b", 64) + `","nodeId":"","address":"","role":"phone","status":"pending","revision":"newrevision000001"}}},` +
		`"requests":{},` +
		`"retired":{"household00000001":[{"machineKey":"mkey:` + strings.Repeat("e", 64) + `","nodeId":"","address":"","role":"phone","status":"revoked","revision":"oldrevision000001"}]}}`
	if err := os.WriteFile(filepath.Join(dir, "state.json"), []byte(old), 0600); err != nil {
		t.Fatal(err)
	}
	s, err := Open(dir)
	if err != nil {
		t.Fatalf("a state file written before retired expiry did not load: %v", err)
	}
	defer s.Close()
	retired := s.value.Retired["household00000001"]
	if len(retired) != 1 || retired[0].AbsentSince != 0 {
		t.Fatalf("the retired record did not load as unchecked: %+v", retired)
	}
	// The sighting belongs to retired records alone, and is never negative.
	retired[0].AbsentSince = -1
	if validateState(s.value) == nil {
		t.Fatal("a negative sighting was accepted")
	}
	retired[0].AbsentSince = 0
	live := s.value.Devices["household00000001"]["device0000000001"]
	live.AbsentSince = 1
	s.value.Devices["household00000001"]["device0000000001"] = live
	if validateState(s.value) == nil {
		t.Fatal("a live device carrying a sighting was accepted")
	}
}
