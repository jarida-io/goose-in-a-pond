package pondnet

import (
	"bytes"
	"context"
	"crypto/x509"
	"encoding/json"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
)

// TestNodeHeadscalePersistence runs the actual production node/store against an
// isolated HTTPS coordinator. The fixture CA is process-local, never installed.
func TestNodeHeadscalePersistence(t *testing.T) {
	admin, control := os.Getenv("POND_TEST_HEADSCALE"), os.Getenv("POND_TEST_CONTROL")
	if admin == "" || control == "" {
		t.Skip("requires disposable local HTTPS Headscale")
	}
	a, err := url.Parse(admin)
	if err != nil || a.Hostname() != "127.0.0.1" {
		t.Fatal("administration fixture must be loopback")
	}
	c, err := url.Parse(control)
	if err != nil || c.Scheme != "https" || (c.Hostname() != "localhost" && c.Hostname() != "127.0.0.1") {
		t.Fatal("control fixture must be loopback HTTPS")
	}
	credential, err := os.ReadFile(os.Getenv("POND_TEST_HEADSCALE_CREDENTIAL_FILE"))
	if err != nil {
		t.Fatal(err)
	}
	pem, err := os.ReadFile(os.Getenv("POND_TEST_ROOT_CA"))
	if err != nil {
		t.Fatal(err)
	}
	roots := x509.NewCertPool()
	if !roots.AppendCertsFromPEM(pem) {
		t.Fatal("invalid fixture CA")
	}
	t.Setenv("GODEBUG", "x509usefallbackroots=1")
	x509.SetFallbackRoots(roots)
	backend, err := enrollment.NewHeadscale(admin, string(credential))
	if err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	data, _ := json.Marshal(map[string]string{"name": "identity-" + time.Now().Format("150405000000")})
	r, err := http.NewRequestWithContext(ctx, "POST", admin+"/api/v1/user", bytes.NewReader(data))
	if err != nil {
		t.Fatal(err)
	}
	r.Header.Set("Authorization", "Bearer "+strings.TrimSpace(string(credential)))
	r.Header.Set("Content-Type", "application/json")
	response, err := http.DefaultClient.Do(r)
	if err != nil {
		t.Fatal("fixture administration unavailable")
	}
	defer response.Body.Close()
	if response.StatusCode != 200 {
		t.Fatalf("create fixture user: HTTP %d", response.StatusCode)
	}
	var user struct {
		User struct {
			ID string `json:"id"`
		} `json:"user"`
	}
	if json.NewDecoder(io.LimitReader(response.Body, 4096)).Decode(&user) != nil || user.User.ID == "" {
		t.Fatal("invalid fixture user")
	}
	directory := filepath.Join(t.TempDir(), "node")
	var previous Status
	for attempt := 0; attempt < 2; attempt++ {
		n, err := Open(directory, "pond-persistence-test", control)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(n.Close)
		registered := false
		for {
			if ctx.Err() != nil {
				t.Fatal("node did not become ready")
			}
			status, err := n.Snapshot()
			if err != nil {
				t.Fatal(err)
			}
			if status.State == "Running" {
				if status.NodeKey == "" || len(status.Addresses) == 0 {
					t.Fatal("running identity unavailable")
				}
				if attempt == 1 && (status.NodeKey != previous.NodeKey || status.Addresses[0] != previous.Addresses[0]) {
					t.Fatal("restart replaced registered identity")
				}
				previous = status
				break
			}
			if status.AuthURL != "" && !registered {
				if attempt != 0 {
					t.Fatal("restart required enrollment")
				}
				u, err := url.Parse(status.AuthURL)
				if err != nil || !strings.HasPrefix(u.Path, "/register/") {
					t.Fatal("invalid pending registration")
				}
				node, err := backend.Register(ctx, user.User.ID, strings.TrimPrefix(u.Path, "/register/"))
				if err != nil {
					t.Fatal(err)
				}
				t.Cleanup(func() {
					cleanup, stop := context.WithTimeout(context.Background(), 5*time.Second)
					defer stop()
					_ = backend.Delete(cleanup, node.ID)
				})
				registered = true
			}
			time.Sleep(100 * time.Millisecond)
		}
		n.Close()
	}
}
