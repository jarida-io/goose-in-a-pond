package pondnet

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestProxyRefusesUnauthenticatedAndUnconfiguredTargets(t *testing.T) {
	var calls atomic.Int32
	p, err := NewProxy(func(context.Context, string, string) (net.Conn, error) {
		calls.Add(1)
		return nil, errors.New("unexpected dial")
	})
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	if err = p.SetTargets([]string{"100.64.0.7:4443"}); err != nil {
		t.Fatal(err)
	}
	for _, target := range []string{"127.0.0.1:4443", "192.168.1.1:443", "1.1.1.1:443", "[::ffff:100.64.0.7]:443", "100.64.0.7:0"} {
		if p.SetTargets([]string{target}) == nil {
			t.Fatalf("unsafe target accepted: %s", target)
		}
	}
	client := &http.Client{Timeout: time.Second}
	for _, method := range []string{http.MethodConnect, http.MethodGet} {
		r, _ := http.NewRequest(method, "http://"+p.Address(), nil)
		response, err := client.Do(r)
		if err != nil {
			t.Fatal(err)
		}
		response.Body.Close()
		if response.StatusCode != 407 {
			t.Fatal(response.StatusCode)
		}
	}
	proxyURL, _ := url.Parse("http://" + p.Address())
	proxyURL.User = url.UserPassword("pond", p.Credential())
	transport := &http.Transport{Proxy: http.ProxyURL(proxyURL)}
	defer transport.CloseIdleConnections()
	client.Transport = transport
	if _, err = client.Get("https://100.64.0.8:4443/api/v1/health"); err == nil {
		t.Fatal("unconfigured target accepted")
	}
	if calls.Load() != 0 {
		t.Fatal("refused request reached the dialer")
	}
}

func TestProxyKeepsTLSOpaqueAndCannotReplaceTrust(t *testing.T) {
	var requests atomic.Int32
	upstream := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		requests.Add(1)
		if r.Header.Get("Proxy-Authorization") != "" {
			t.Error("proxy credential leaked into HTTPS")
		}
		io.WriteString(w, "pinned")
	}))
	defer upstream.Close()
	p, err := NewProxy(func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "tcp", upstream.Listener.Addr().String())
	})
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	if err = p.SetTargets([]string{"100.64.0.7:4443"}); err != nil {
		t.Fatal(err)
	}
	proxyURL, _ := url.Parse("http://" + p.Address())
	proxyURL.User = url.UserPassword("pond", p.Credential())
	roots := x509.NewCertPool()
	roots.AddCert(upstream.Certificate())
	transport := &http.Transport{Proxy: http.ProxyURL(proxyURL), TLSClientConfig: &tls.Config{RootCAs: roots, ServerName: "127.0.0.1", MinVersion: tls.VersionTLS12}}
	defer transport.CloseIdleConnections()
	client := &http.Client{Transport: transport, Timeout: 3 * time.Second}
	response, err := client.Get("https://100.64.0.7:4443/api/v1/health")
	if err != nil {
		t.Fatal(err)
	}
	body, _ := io.ReadAll(response.Body)
	response.Body.Close()
	if string(body) != "pinned" {
		t.Fatal(string(body))
	}
	transport.CloseIdleConnections()
	transport.TLSClientConfig.VerifyConnection = func(tls.ConnectionState) error { return errors.New("wrong expected pin") }
	if _, err = client.Get("https://100.64.0.7:4443/wrong-pin"); err == nil {
		t.Fatal("invalid pin accepted")
	}
	if requests.Load() != 1 {
		t.Fatal("invalid pin reached HTTP")
	}
	if err = p.SetTargets(nil); err != nil {
		t.Fatal(err)
	}
	if _, err = client.Get("https://100.64.0.7:4443/cleared"); err == nil {
		t.Fatal("cleared target accepted")
	}
}

func TestBridgeReplacesForgedPeerHeaders(t *testing.T) {
	directory, err := os.MkdirTemp("", "pn-")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { os.RemoveAll(directory) })
	socket := filepath.Join(directory, "api.sock")
	listener, err := net.Listen("unix", socket)
	if err != nil {
		t.Fatal(err)
	}
	backend := &http.Server{Handler: http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get(PeerHeader) != "100.64.0.9:4567" {
			t.Errorf("incorrect peer: %q", r.Header.Get(PeerHeader))
		}
		if r.Header.Get("Forwarded") != "" || r.Header.Get("X-Forwarded-For") != "" {
			t.Error("untrusted forwarding header survived")
		}
		w.WriteHeader(http.StatusNoContent)
	})}
	defer backend.Close()
	go backend.Serve(listener)
	handler := BridgeHandler(socket)
	for _, source := range []string{"100.64.0.9:4567", "127.0.0.1:4567", "malformed"} {
		r := httptest.NewRequest("GET", "https://pond/api/v1/health", nil)
		r.RemoteAddr = source
		r.Header.Set(PeerHeader, "127.0.0.1:1234")
		r.Header.Set("Forwarded", "for=127.0.0.1")
		r.Header.Set("X-Forwarded-For", "127.0.0.1")
		w := httptest.NewRecorder()
		handler.ServeHTTP(w, r)
		want := 403
		if source == "100.64.0.9:4567" {
			want = 204
		}
		if w.Code != want {
			t.Fatalf("%s: %d", source, w.Code)
		}
	}
}

func TestNodeRefusesUnsafeConfigurationBeforeEnrollment(t *testing.T) {
	for _, control := range []string{"http://headscale.example", "https://user:secret@headscale.example", "https://headscale.example/?key=value"} {
		if n, err := Open(filepath.Join(t.TempDir(), "node"), "pond-test", control); err == nil {
			n.Close()
			t.Fatal("unsafe control accepted")
		}
	}
	if n, err := Open("relative", "pond-test", ""); err == nil {
		n.Close()
		t.Fatal("relative identity path accepted")
	}
}

func TestNodeRefusesLostOrEmptyIdentityBeforeNetworking(t *testing.T) {
	for _, contents := range []string{"missing", "", "{}", "null"} {
		t.Run(contents, func(t *testing.T) {
			directory := t.TempDir()
			if err := os.Chmod(directory, 0700); err != nil {
				t.Fatal(err)
			}
			if err := os.WriteFile(filepath.Join(directory, "node.lock"), nil, 0600); err != nil {
				t.Fatal(err)
			}
			if contents != "missing" {
				if err := os.WriteFile(filepath.Join(directory, "tailscaled.state"), []byte(contents), 0600); err != nil {
					t.Fatal(err)
				}
			}
			node, err := Open(directory, "pond-test", "https://127.0.0.1:9")
			if err == nil {
				node.Close()
				t.Fatal("damaged identity silently accepted")
			}
		})
	}
}

func TestProfileChangeCancelsPendingProxyDial(t *testing.T) {
	started, cancelled := make(chan struct{}), make(chan struct{})
	p, err := NewProxy(func(ctx context.Context, _, _ string) (net.Conn, error) {
		close(started)
		<-ctx.Done()
		close(cancelled)
		return nil, ctx.Err()
	})
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	if err = p.SetTargets([]string{"100.64.0.7:4443"}); err != nil {
		t.Fatal(err)
	}
	u, _ := url.Parse("http://" + p.Address())
	u.User = url.UserPassword("pond", p.Credential())
	tr := &http.Transport{Proxy: http.ProxyURL(u)}
	defer tr.CloseIdleConnections()
	client := &http.Client{Transport: tr, Timeout: 3 * time.Second}
	done := make(chan error, 1)
	go func() {
		r, e := client.Get("https://100.64.0.7:4443/")
		if r != nil {
			r.Body.Close()
		}
		done <- e
	}()
	select {
	case <-started:
	case <-time.After(time.Second):
		t.Fatal("dial did not start")
	}
	if err = p.SetTargets(nil); err != nil {
		t.Fatal(err)
	}
	select {
	case <-cancelled:
	case <-time.After(time.Second):
		t.Fatal("stale dial was not cancelled")
	}
	if err = <-done; err == nil {
		t.Fatal("stale request succeeded")
	}
}

func TestProfileChangeClosesActiveProxyStream(t *testing.T) {
	upstream := httptest.NewTLSServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, "data: first\n\n")
		w.(http.Flusher).Flush()
		<-r.Context().Done()
	}))
	defer upstream.Close()
	p, err := NewProxy(func(ctx context.Context, _, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, "tcp", upstream.Listener.Addr().String())
	})
	if err != nil {
		t.Fatal(err)
	}
	defer p.Close()
	if err = p.SetTargets([]string{"100.64.0.7:4443"}); err != nil {
		t.Fatal(err)
	}
	u, _ := url.Parse("http://" + p.Address())
	u.User = url.UserPassword("pond", p.Credential())
	roots := x509.NewCertPool()
	roots.AddCert(upstream.Certificate())
	tr := &http.Transport{Proxy: http.ProxyURL(u), TLSClientConfig: &tls.Config{RootCAs: roots, ServerName: "127.0.0.1", MinVersion: tls.VersionTLS12}}
	defer tr.CloseIdleConnections()
	client := &http.Client{Transport: tr, Timeout: 3 * time.Second}
	response, err := client.Get("https://100.64.0.7:4443/stream")
	if err != nil {
		t.Fatal(err)
	}
	defer response.Body.Close()
	first := make([]byte, len("data: first\n\n"))
	if _, err = io.ReadFull(response.Body, first); err != nil {
		t.Fatal(err)
	}
	if err = p.SetTargets(nil); err != nil {
		t.Fatal(err)
	}
	closed := make(chan error, 1)
	go func() { _, err := response.Body.Read(make([]byte, 1)); closed <- err }()
	select {
	case err := <-closed:
		if err == nil {
			t.Fatal("stream survived profile replacement")
		}
	case <-time.After(time.Second):
		t.Fatal("stream cancellation was not prompt")
	}
}

// Backend diagnostics are opt-in, and the enrollment capability must not survive
// into whatever sink an operator installs. tsnet prints the registration URL in
// plain prose, so redaction happens on the way out rather than at each caller.
func TestBackendDiagnosticsAreOptInAndRedactEnrollmentURLs(t *testing.T) {
	t.Cleanup(func() { SetDiagnostics(nil) })

	// Absent a sink, a backend line must go nowhere at all.
	backendLogf("control: %s", "https://control.example/register/abcdef0123456789")

	var lines []string
	SetDiagnostics(func(line string) { lines = append(lines, line) })

	for _, sample := range []string{
		"To authenticate, visit: https://control.example/register/abcdef0123456789",
		`{"url":"https://control.example/register/abcdef0123456789"}`,
		"http://control.example:8080/register/abcdef0123456789 and trailing prose",
	} {
		backendLogf("%s", sample)
	}
	backendLogf("plain line with no capability")

	if len(lines) != 4 {
		t.Fatalf("expected 4 delivered lines once a sink exists, got %d", len(lines))
	}
	for _, line := range lines[:3] {
		if strings.Contains(line, "/register/") || strings.Contains(line, "abcdef0123456789") {
			t.Fatalf("enrollment capability survived redaction: %q", line)
		}
		if !strings.Contains(line, "<redacted enrollment URL>") {
			t.Fatalf("expected a redaction marker in %q", line)
		}
	}
	if lines[3] != "plain line with no capability" {
		t.Fatalf("an unrelated line was altered: %q", lines[3])
	}

	SetDiagnostics(nil)
	backendLogf("after removal: https://control.example/register/abcdef0123456789")
	if len(lines) != 4 {
		t.Fatalf("removing the sink must stop delivery, got %d lines", len(lines))
	}
}
