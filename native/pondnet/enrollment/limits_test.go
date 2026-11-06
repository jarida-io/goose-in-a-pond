package enrollment

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"golang.org/x/time/rate"
)

func TestInventoryIsStreamedAndBoundedByCount(t *testing.T) {
	node := `{"id":"%d","nodeKey":"nodekey:x","user":{"id":"1"},"ipAddresses":["100.64.0.1"]}`
	document := func(count int) string {
		nodes := make([]string, count)
		for i := range nodes {
			nodes[i] = fmt.Sprintf(node, i+1)
		}
		return `{"extra":{"ignored":[1,2]},"nodes":[` + strings.Join(nodes, ",") + `]}`
	}
	// Well past the old 1 MiB body limit, and still read.
	large := document(4000)
	if len(large) < 1<<18 {
		t.Fatalf("fixture too small to matter: %d bytes", len(large))
	}
	nodes, err := decodeNodes(strings.NewReader(large), 4000)
	if err != nil || len(nodes) != 4000 || nodes[3999].ID != "4000" || nodes[0].UserID != "1" {
		t.Fatalf("decoded %d nodes: %v", len(nodes), err)
	}
	if _, err := decodeNodes(strings.NewReader(document(11)), 10); !errors.Is(err, ErrInventoryTooLarge) {
		t.Fatalf("an oversized inventory was accepted: %v", err)
	}
	for _, malformed := range []string{`[]`, `{"nodes":{}}`, `{"nodes":[{"id":1}]}`, `{"nodes":[`} {
		if _, err := decodeNodes(strings.NewReader(malformed), 10); err == nil {
			t.Fatalf("accepted %s", malformed)
		}
	}
}

func TestUserLookupsAreFilteredByTheCoordinator(t *testing.T) {
	var queries []string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		queries = append(queries, r.URL.RawQuery)
		io.WriteString(w, `{"users":[{"id":"7","name":"household-0123456789abcdef0123456789abcdef"}]}`)
	}))
	defer server.Close()
	backend, err := NewHeadscale(server.URL, "fixture-credential")
	if err != nil {
		t.Fatal(err)
	}
	id, err := backend.EnsureUser(context.Background(), "household-0123456789abcdef0123456789abcdef")
	if err != nil || id != "7" {
		t.Fatalf("lookup: %q %v", id, err)
	}
	if len(queries) != 1 || queries[0] != "name=household-0123456789abcdef0123456789abcdef" {
		t.Fatalf("unfiltered user listing: %v", queries)
	}
}

func TestEnrollmentStopsAtCapacity(t *testing.T) {
	s, key, backend := fixture(t)
	if code := invoke(s, approval(), key); code != 200 {
		t.Fatalf("pond enrollment: %d", code)
	}
	// Other households fill the service; this one still has room of its own.
	s.Store.mu.Lock()
	for i := 1; i < EnrollmentCapacity; i++ {
		household := fmt.Sprintf("filler%010d", i/16)
		if s.Store.value.Devices[household] == nil {
			s.Store.value.Devices[household] = map[string]Device{}
		}
		s.Store.value.Devices[household][fmt.Sprintf("device%010d", i)] = Device{Role: "phone", Status: "active"}
	}
	s.Store.mu.Unlock()
	phone := approval()
	phone.Device, phone.Role, phone.Nonce = "phone00000000001", "phone", "nonce00000000002"
	phone.MachineKey = "mkey:" + strings.Repeat("c", 64)
	calls := backend.calls
	if code := invoke(s, phone, key); code != 503 {
		t.Fatalf("enrolled past capacity: %d", code)
	}
	if backend.calls != calls {
		t.Fatal("the coordinator was asked to register past capacity")
	}
}

func TestAnAuthenticApprovalIsSpentEvenWhenRefused(t *testing.T) {
	s, key, _ := fixture(t)
	refused := approval()
	refused.Role = "phone" // No active pond yet, so this is refused after authentication.
	if code := invoke(s, refused, key); code != 409 {
		t.Fatalf("expected a refusal, got %d", code)
	}
	if code := invoke(s, refused, key); code != 409 {
		t.Fatalf("replayed: %d", code)
	}
	s.Store.mu.Lock()
	_, spent := s.Store.value.Requests["household00000001:"+refused.Nonce]
	s.Store.mu.Unlock()
	if !spent {
		t.Fatal("the refused approval was not recorded as used")
	}
}

func TestClientAddressTrustsOnlyTheConfiguredProxy(t *testing.T) {
	s := &Service{TrustedProxies: []netip.Prefix{netip.MustParsePrefix("172.31.250.0/28")}}
	request := func(remote, forwarded string) *http.Request {
		r := httptest.NewRequest("POST", "/v1/household", nil)
		r.RemoteAddr = remote
		if forwarded != "" {
			r.Header.Set("X-Forwarded-For", forwarded)
		}
		return r
	}
	for _, c := range []struct{ remote, forwarded, want string }{
		{"172.31.250.2:5000", "198.51.100.7", "198.51.100.7"},
		// Only the hop the proxy saw counts; the rest is the client's own claim.
		{"172.31.250.2:5000", "10.0.0.1, 198.51.100.7", "198.51.100.7"},
		{"203.0.113.9:5000", "198.51.100.7", "203.0.113.9"},
		{"172.31.250.2:5000", "not-an-address", "172.31.250.2"},
		{"172.31.250.2:5000", "2001:db8:1:2:3:4:5:6", "2001:db8:1:2::/64"},
		{"[2001:db8:9:9::1]:5000", "", "2001:db8:9:9::/64"},
	} {
		if got := s.client(request(c.remote, c.forwarded)); got != c.want {
			t.Errorf("%s via %q: got %s, want %s", c.remote, c.forwarded, got, c.want)
		}
	}
}

// A full table admits a new key only by forgetting one whose limit holds nobody back:
// idle, or refilled. A spent budget is never forgotten, however many keys arrive.
func TestAFullLimiterEvictsOnlyBudgetsItCanForget(t *testing.T) {
	limits := newSources(rate.Limit(1.0/60.0), 1)
	limits.maximum = 2
	if !limits.allow("a") || limits.allow("a") {
		t.Fatal("the first key's budget is wrong")
	}
	if !limits.allow("b") {
		t.Fatal("the second key was refused")
	}
	if limits.allow("c") {
		t.Fatal("a new key was admitted to a table of spent budgets")
	}
	if limits.allow("a") {
		t.Fatal("a flood reset an exhausted budget")
	}

	// A refused request does not refresh the key, so a flood cannot keep it current.
	earlier := time.Now().Add(-time.Minute)
	limits.seen["a"].last = earlier
	if limits.allow("a") {
		t.Fatal("an exhausted budget allowed a request")
	}
	if !limits.seen["a"].last.Equal(earlier) {
		t.Fatal("a refused request refreshed the key")
	}

	// An idle key goes, even with its bucket still spent; the busy one stays.
	limits.seen["b"].last = time.Now().Add(-2 * idle)
	if !limits.allow("c") {
		t.Fatal("an idle key was not evicted to make room")
	}
	if _, kept := limits.seen["b"]; kept {
		t.Fatal("the idle key was kept")
	}
	if _, kept := limits.seen["a"]; !kept {
		t.Fatal("a spent budget was evicted")
	}

	// A key whose bucket has refilled goes too, however recently it was used.
	limits.seen["c"].limiter = rate.NewLimiter(limits.every, limits.burst)
	if !limits.allow("d") {
		t.Fatal("a refilled key was not evicted to make room")
	}
	if _, kept := limits.seen["c"]; kept {
		t.Fatal("the refilled key was kept")
	}
	if _, kept := limits.seen["a"]; !kept || limits.allow("e") {
		t.Fatal("a spent budget was evicted for a new key")
	}
}

// healthOf answers the service's /health body, failing the test on anything but 200.
func healthOf(t *testing.T, s *Service) string {
	t.Helper()
	w := httptest.NewRecorder()
	s.ServeHTTP(w, httptest.NewRequest("GET", "/health", nil))
	if w.Code != 200 {
		t.Fatalf("health %d", w.Code)
	}
	return w.Body.String()
}

func TestAnUnreachableCoordinatorLeavesTheServiceDegradedNotDown(t *testing.T) {
	store, err := Open(t.TempDir() + "/state")
	if err != nil {
		t.Fatal(err)
	}
	defer store.Close()
	backend := &unreachable{failure: errors.New("coordinator unavailable")}
	s, err := New(context.Background(), store, backend)
	if err != nil {
		t.Fatalf("startup failed instead of degrading: %v", err)
	}
	if !strings.Contains(healthOf(t, s), `"degraded":true`) {
		t.Fatal("degraded state not reported")
	}
	backend.failure = nil
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if strings.Contains(healthOf(t, s), "degraded") {
		t.Fatal("still degraded after reconciling")
	}
	// A coordinator lost after a healthy start degrades the service just the same.
	for _, failure := range []error{errors.New("coordinator unavailable"), ErrInventoryTooLarge} {
		backend.failure = failure
		if err := s.Reconcile(context.Background()); !errors.Is(err, failure) {
			t.Fatalf("reconciliation hid %v: %v", failure, err)
		}
		if !strings.Contains(healthOf(t, s), `"degraded":true`) {
			t.Fatalf("%v not reported as degraded", failure)
		}
		backend.failure = nil
		if err := s.Reconcile(context.Background()); err != nil {
			t.Fatal(err)
		}
		if strings.Contains(healthOf(t, s), "degraded") {
			t.Fatalf("still degraded after recovering from %v", failure)
		}
	}
}

// A coordinator that stops accepting the admin key, as after a botched rotation,
// must show on /health rather than leave it reporting ok.
func TestARefusedCoordinatorCredentialDegradesTheService(t *testing.T) {
	var refused atomic.Bool
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if refused.Load() {
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		switch {
		case r.Method == "GET" && r.URL.Path == "/api/v1/node":
			io.WriteString(w, `{"nodes":[]}`)
		case r.Method == "GET" && r.URL.Path == "/api/v1/policy":
			// Headscale fails the read while it holds no policy, before the first write.
			http.Error(w, `{"message":"loading ACL from database"}`, http.StatusInternalServerError)
		case r.Method == "PUT" && r.URL.Path == "/api/v1/policy":
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
	s, err := New(context.Background(), store, backend)
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(healthOf(t, s), "degraded") {
		t.Fatal("degraded at a healthy start")
	}
	refused.Store(true)
	if err := s.Reconcile(context.Background()); err == nil || !strings.Contains(err.Error(), "HTTP 401") {
		t.Fatalf("a refused credential was not surfaced: %v", err)
	}
	if !strings.Contains(healthOf(t, s), `"degraded":true`) {
		t.Fatal("a refused credential left /health reporting ok")
	}
}

type unreachable struct {
	fakeBackend
	failure error
}

func (u *unreachable) Inventory(ctx context.Context) ([]Registered, error) {
	if u.failure != nil {
		return nil, u.failure
	}
	return u.fakeBackend.Inventory(ctx)
}

func TestPolicyIsTheSameForTheSameStore(t *testing.T) {
	s, _, backend := fixture(t)
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	for i := 0; i < 5; i++ {
		id := fmt.Sprintf("household%08d", i+2)
		s.Store.value.Households[id] = Household{UserID: "1", Port: 4443}
		s.Store.value.Devices[id] = map[string]Device{}
		for j, role := range []string{"pond", "phone", "phone", "phone"} {
			address := fmt.Sprintf("100.64.%d.%d", i, j+1)
			node := Registered{ID: fmt.Sprint(i*10 + j + 1), Key: "k" + address, UserID: "1", Addresses: []string{address}}
			backend.nodes = append(backend.nodes, node)
			s.Store.value.Devices[id][fmt.Sprintf("device%010d", j)] = Device{Role: role, Status: "active", NodeID: node.ID, Key: node.Key, Address: address}
		}
	}
	first := ""
	for i := 0; i < 20; i++ {
		if err := s.policy(context.Background()); err != nil {
			t.Fatal(err)
		}
		got := fmt.Sprint(backend.rules, backend.attrs)
		if first == "" {
			first = got
		} else if got != first {
			t.Fatalf("policy changed between identical runs:\n%s\n%s", first, got)
		}
	}
	if len(backend.rules) != 5 {
		t.Fatalf("expected one rule per household, got %d", len(backend.rules))
	}
}

// The inventory is indexed by node ID, and the first node with an ID decides, as a
// scan would: a later duplicate neither grants nor withdraws permission.
func TestPolicyMatchesTheFirstNodeWithAnID(t *testing.T) {
	for _, order := range []string{"verified_first", "impostor_first"} {
		t.Run(order, func(t *testing.T) {
			s, _, backend := fixture(t)
			s.Store.mu.Lock()
			defer s.Store.mu.Unlock()
			id := "household00000001"
			key := "nodekey:" + strings.Repeat("a", 64)
			pond := Registered{ID: "1", Key: key, UserID: "1", Addresses: []string{"100.64.0.1"}}
			phone := Registered{ID: "2", Key: key, UserID: "1", Addresses: []string{"100.64.0.2"}}
			impostor := Registered{ID: "2", Key: "nodekey:" + strings.Repeat("c", 64), UserID: "1", Addresses: []string{"100.64.0.2"}}
			s.Store.value.Devices[id] = map[string]Device{
				"pond":  {NodeID: pond.ID, Key: key, Role: "pond", Status: "active", Address: "100.64.0.1"},
				"phone": {NodeID: phone.ID, Key: key, Role: "phone", Status: "active", Address: "100.64.0.2"}}
			backend.nodes = []Registered{pond, phone, impostor}
			want := 1
			if order == "impostor_first" {
				backend.nodes = []Registered{pond, impostor, phone}
				want = 0
			}
			if err := s.policy(context.Background()); err != nil {
				t.Fatal(err)
			}
			if len(backend.rules) != want {
				t.Fatalf("%d rules, want %d: %v", len(backend.rules), want, backend.rules)
			}
		})
	}
}
