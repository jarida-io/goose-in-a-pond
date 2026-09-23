package enrollment

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/x509"
	"encoding/base64"
	"errors"
	"net"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"tailscale.com/envknob"
	"tailscale.com/ipn"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	tskey "tailscale.com/types/key"
	"testing"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/tailnetdial"
)

// TestHeadscaleLive uses an isolated real Headscale, never the operator's tailnet.
func TestHeadscaleLive(t *testing.T) {
	origin := os.Getenv("POND_TEST_HEADSCALE")
	secretFile := os.Getenv("POND_TEST_HEADSCALE_CREDENTIAL_FILE")
	if origin == "" || secretFile == "" {
		t.Skip("requires an isolated local Headscale fixture")
	}
	u, e := url.Parse(origin)
	if e != nil || u.Hostname() != "127.0.0.1" {
		t.Fatal("live fixture must be loopback")
	}
	credential, e := os.ReadFile(secretFile)
	if e != nil {
		t.Fatal(e)
	}
	backend, e := NewHeadscale(origin, string(credential))
	if e != nil {
		t.Fatal(e)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 90*time.Second)
	defer cancel()
	store, e := Open(filepath.Join(t.TempDir(), "store"))
	if e != nil {
		t.Fatal(e)
	}
	defer store.Close()
	loseResponse, registrationCalls := true, 0
	service, e := New(ctx, store, liveBackend{Headscale: backend, t: t, loseResponse: &loseResponse, calls: &registrationCalls})
	if e != nil {
		t.Fatal(e)
	}
	relay := os.Getenv("POND_TEST_FORCE_RELAY") == "1"
	if relay {
		pem, err := os.ReadFile(os.Getenv("POND_TEST_ROOT_CA"))
		if err != nil {
			t.Fatal(err)
		}
		roots := x509.NewCertPool()
		if !roots.AppendCertsFromPEM(pem) {
			t.Fatal("missing fixture root")
		}
		t.Setenv("GODEBUG", "x509usefallbackroots=1")
		x509.SetFallbackRoots(roots)
		envknob.SetenvForTest(t, "TS_DEBUG_ALWAYS_USE_DERP", "true")
		envknob.SetenvForTest(t, "TS_DEBUG_NEVER_DIRECT_UDP", "true")
	}
	envknob.SetNoLogsNoSupport()
	nodes := []*tsnet.Server{}
	householdKeys := []ed25519.PrivateKey{}
	approved := []Approval{}
	addresses := []string{}
	for household := 0; household < 2; household++ {
		public, key, _ := ed25519.GenerateKey(rand.Reader)
		householdKeys = append(householdKeys, key)
		var user struct {
			User struct {
				ID string `json:"id"`
			} `json:"user"`
		}
		suffix := time.Now().Format("150405000000") + string(rune('a'+household))
		if e = backend.call(ctx, "POST", "/api/v1/user", map[string]string{"name": "test-" + suffix}, &user); e != nil {
			t.Fatal(e)
		}
		id := "household0000000" + string(rune('1'+household))
		if e = store.Provision(id, Household{base64.StdEncoding.EncodeToString(public), user.User.ID, 4443}); e != nil {
			t.Fatal(e)
		}
		for index := 0; index < 2; index++ {
			// HTTP exists only inside this loopback fixture. Production Open requires HTTPS.
			n := &tsnet.Server{Dir: filepath.Join(t.TempDir(), "node"), Hostname: "fixture-" + suffix + string(rune('a'+index)), ControlURL: origin, Logf: func(string, ...any) {}, UserLogf: func(string, ...any) {}}
			if e = n.Start(); e != nil {
				t.Fatal(e)
			}
			nodes = append(nodes, n)
			t.Cleanup(func() { n.Close() })
			lc, e := n.LocalClient()
			if e != nil {
				t.Fatal(e)
			}
			authID, nodeKey := "", ""
			deadline := time.Now().Add(20 * time.Second)
			for time.Now().Before(deadline) {
				s, err := lc.StatusWithoutPeers(ctx)
				if err == nil {
					if parsed, err := url.Parse(s.AuthURL); err == nil && parsed.Path != "" {
						authID = parsed.Path[strings.LastIndex(parsed.Path, "/")+1:]
					}
					if prefs, err := lc.GetPrefs(ctx); err == nil && prefs.Persist != nil {
						if public, ok := prefs.Persist.PublicNodeKeyOK(); ok {
							nodeKey = public.String()
						}
					}
					if authID != "" {
						break
					}
				}
				time.Sleep(100 * time.Millisecond)
			}
			if authID == "" {
				t.Fatalf("pending identity unavailable (auth=%t key=%t)", authID != "", nodeKey != "")
			}
			a := approval()
			a.Household = id
			a.Device = "device000000000" + string(rune('1'+index))
			a.Nonce = "nonce0000000000" + string(rune('1'+index))
			a.AuthID = authID
			a.NodeKey = nodeKey
			encodedMachine, err := n.Store.ReadState(ipn.MachineKeyStateKey)
			var machine tskey.MachinePrivate
			if err != nil || machine.UnmarshalText(encodedMachine) != nil || machine.IsZero() {
				t.Fatal("pending machine identity unavailable")
			}
			a.MachineKey = machine.Public().String()
			if index == 1 {
				a.Role = "phone"
			}
			code := invoke(service, a, key)
			if household == 0 && index == 0 {
				if code != 503 {
					t.Fatalf("lost registration response: HTTP %d", code)
				}
				if err := service.Reconcile(ctx); err != nil {
					t.Fatal(err)
				}
				if store.value.Devices[id][a.Device].Status != "active" || registrationCalls != 1 {
					t.Fatal("ambiguous registration was not safely recovered")
				}
				t.Log("lost registration response recovered through machine-bound inventory without replay")
			} else if code != 200 {
				t.Fatalf("real registration HTTP %d", code)
			}
			approved = append(approved, a)
			addresses = append(addresses, store.value.Devices[id][a.Device].Address)
		}
	}
	// Listen on forbidden destinations too: rejection must come from policy, not a closed port.
	for _, destination := range []struct {
		index int
		port  string
	}{{0, "4443"}, {0, "4444"}, {2, "4443"}, {3, "4443"}} {
		ln, err := nodes[destination.index].Listen("tcp", ":"+destination.port)
		if err != nil {
			t.Fatal(err)
		}
		defer ln.Close()
		go func() {
			for {
				c, e := ln.Accept()
				if e != nil {
					return
				}
				c.Write([]byte("pond"))
				c.Close()
			}
		}()
	}
	// Policy propagation is asynchronous; wait only for the allowed connection.
	phone := func(ctx context.Context, address string) (net.Conn, error) {
		return tailnetdial.TCP(ctx, nodes[1], netip.MustParseAddrPort(address))
	}
	var allowed net.Conn
	for i := 0; i < 30; i++ {
		attempt, stop := context.WithTimeout(ctx, time.Second)
		allowed, e = phone(attempt, net.JoinHostPort(addresses[0], "4443"))
		stop()
		if e == nil {
			break
		}
		time.Sleep(100 * time.Millisecond)
	}
	if e != nil {
		t.Fatal("own household cannot connect:", e)
	}
	allowed.Close()
	if relay {
		lc, err := nodes[1].LocalClient()
		if err != nil {
			t.Fatal(err)
		}
		result, err := lc.Ping(ctx, netip.MustParseAddr(addresses[0]), tailcfg.PingDisco)
		if err != nil || result.Err != "" || result.DERPRegionID != 999 || result.Endpoint != "" {
			t.Fatalf("forced Goose relay was not proven: result=%+v error=%v", result, err)
		}
		t.Log("own-household traffic traversed encrypted Goose DERP region 999")
	}
	for _, target := range []string{net.JoinHostPort(addresses[2], "4443"), net.JoinHostPort(addresses[0], "4444"), net.JoinHostPort(addresses[3], "4443")} {
		attempt, stop := context.WithTimeout(ctx, 2*time.Second)
		connection, err := phone(attempt, target)
		stop()
		if err == nil {
			connection.Close()
			t.Fatal("forbidden connection succeeded")
		}
	}
	// Restore enrollment state into another private directory with the same real coordinator.
	backup := filepath.Join(t.TempDir(), "restored")
	if err := os.Mkdir(backup, 0700); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"state.json", "initialized"} {
		data, err := os.ReadFile(filepath.Join(store.directory, name))
		if err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(backup, name), data, 0600); err != nil {
			t.Fatal(err)
		}
	}
	restored, err := Open(backup)
	if err != nil {
		t.Fatal(err)
	}
	defer restored.Close()
	service, err = New(ctx, restored, backend)
	if err != nil {
		t.Fatal(err)
	}
	if invoke(service, approved[1], householdKeys[0]) != 409 {
		t.Fatal("restoration lost replay protection")
	}
	if registrationCalls != 4 {
		t.Fatal("unexpected registration replay")
	}
	restoredConnection, err := phone(ctx, net.JoinHostPort(addresses[0], "4443"))
	if err != nil {
		t.Fatal("active client could not reconnect after enrollment restoration")
	}
	restoredConnection.Close()
	revoke := approved[1]
	revoke.Action = "revoke"
	revoke.Nonce = "revocation00000001"
	revoke.Expires = time.Now().Add(time.Minute).Unix()
	if invoke(service, revoke, householdKeys[0]) != 200 {
		t.Fatal("real revocation failed")
	}
	for attempt := 0; attempt < 10; attempt++ {
		probe, cancel := context.WithTimeout(ctx, time.Second)
		connection, err := phone(probe, net.JoinHostPort(addresses[0], "4443"))
		cancel()
		if err != nil {
			t.Log("restored enrollment rejects replay and revoked phone traffic")
			break
		}
		connection.Close()
		if attempt == 9 {
			t.Fatal("revoked phone retained network access")
		}
		time.Sleep(100 * time.Millisecond)
	}
	// Explicit local authority replaces only the reviewed revision with a new machine.
	replacement := &tsnet.Server{Dir: filepath.Join(t.TempDir(), "replacement"), Hostname: "fixture-replacement", ControlURL: origin, Logf: func(string, ...any) {}, UserLogf: func(string, ...any) {}}
	if err := replacement.Start(); err != nil {
		t.Fatal(err)
	}
	defer replacement.Close()
	lc, err := replacement.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	next := approved[1]
	next.Action, next.Nonce = "replace", "replacement000001"
	next.NodeKey = ""
	next.ExpectedRevision = restored.value.Devices[next.Household][next.Device].Revision
	next.Expires = time.Now().Add(time.Minute).Unix()
	next.AuthID = ""
	deadline := time.Now().Add(20 * time.Second)
	for time.Now().Before(deadline) {
		status, err := lc.StatusWithoutPeers(ctx)
		if err == nil && status.AuthURL != "" {
			parsed, err := url.Parse(status.AuthURL)
			if err == nil {
				next.AuthID = strings.TrimPrefix(parsed.Path, "/register/")
				break
			}
		}
		time.Sleep(100 * time.Millisecond)
	}
	if next.AuthID == "" {
		t.Fatal("replacement registration unavailable")
	}
	encoded, err := replacement.Store.ReadState(ipn.MachineKeyStateKey)
	var machine tskey.MachinePrivate
	if err != nil || machine.UnmarshalText(encoded) != nil || machine.IsZero() {
		t.Fatal("replacement machine unavailable")
	}
	next.MachineKey = machine.Public().String()
	if code := invoke(service, next, householdKeys[0]); code != 200 {
		t.Fatalf("replacement HTTP %d", code)
	}
	var connected net.Conn
	for attempt := 0; attempt < 10; attempt++ {
		probe, cancel := context.WithTimeout(ctx, time.Second)
		connected, err = tailnetdial.TCP(probe, replacement, netip.MustParseAddrPort(net.JoinHostPort(addresses[0], "4443")))
		cancel()
		if err == nil {
			break
		}
		time.Sleep(500 * time.Millisecond)
	}
	if err != nil {
		t.Fatal("replacement phone cannot reach its Pond:", err)
	}
	connected.Close()
	if relay {
		result, err := lc.Ping(ctx, netip.MustParseAddr(addresses[0]), tailcfg.PingDisco)
		if err != nil || result.Err != "" || result.DERPRegionID != 999 || result.Endpoint != "" {
			t.Fatal("replacement did not use configured relay")
		}
	}
	next.Nonce = "replacementstale1"
	if invoke(service, next, householdKeys[0]) != 409 {
		t.Fatal("stale replacement accepted")
	}
	probe, stop := context.WithTimeout(ctx, time.Second)
	forbidden, err := phone(probe, net.JoinHostPort(addresses[0], "4443"))
	stop()
	if err == nil {
		forbidden.Close()
		t.Fatal("replacement revived revoked phone")
	}
	if err := service.Reconcile(ctx); err != nil {
		t.Fatal(err)
	}
	t.Log("explicit replacement reconnects through Goose relay; stale approval and old phone remain blocked")
	t.Log("real Headscale registration and household isolation passed")
}

type liveBackend struct {
	*Headscale
	t            *testing.T
	loseResponse *bool
	calls        *int
}

func (b liveBackend) Register(ctx context.Context, user, auth string) (Registered, error) {
	*b.calls++
	n, e := b.Headscale.Register(ctx, user, auth)
	if e != nil {
		b.t.Log("registration error:", e)
	} else {
		b.t.Logf("response validation: id=%t user=%t key=%t addresses=%d tags=%d routes=%d", numeric.MatchString(n.ID), n.UserID == user, strings.HasPrefix(n.Key, "nodekey:"), len(n.Addresses), len(n.Tags), len(n.Routes))
	}
	if e == nil && *b.loseResponse {
		*b.loseResponse = false
		return Registered{}, errors.New("fixture lost registration response")
	}
	return n, e
}
