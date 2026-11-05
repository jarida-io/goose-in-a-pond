package main

import (
	"encoding/json"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"tailscale.com/envknob"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

// The Pond helper must leave tailscale's network-map cache off, read the way the
// backend reads it.
func TestPondHelperRefusesCachedNetworkMaps(t *testing.T) {
	envknob.SetenvForTest(t, "TS_USE_CACHED_NETMAP", "")
	if !envknob.BoolDefaultTrue("TS_USE_CACHED_NETMAP") {
		t.Fatal("precondition: the cache is on by default")
	}
	refuseCachedNetworkMaps()
	if envknob.BoolDefaultTrue("TS_USE_CACHED_NETMAP") {
		t.Fatal("the Pond helper would start from a cached network map")
	}
}

// A device key that will not load blocks registration only when there is no invite to use.
func TestAnUnloadableDeviceFallsBackToTheInvite(t *testing.T) {
	directory := filepath.Join(t.TempDir(), "device")
	if err := os.Mkdir(directory, 0700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(directory, "key.json"), []byte(`{"seed":"bad"}`), 0600); err != nil {
		t.Fatal(err)
	}
	var warnings strings.Builder
	if _, err := registrationDevice(directory, "", &warnings); err == nil {
		t.Fatal("registered without an invite or a usable device certificate")
	}
	device, err := registrationDevice(directory, "invite-code", &warnings)
	if err != nil || device != nil {
		t.Fatalf("got %v, %v; want the invite alone", device, err)
	}
	if !strings.Contains(warnings.String(), "registering with the invite") {
		t.Fatalf("the fallback was not reported: %q", warnings.String())
	}
}

// The Pond reads each status line as one flat object; the clock reading sits beside the node's
// own fields, and is left out, not reported false, where the host cannot say.
func TestAReportIsTheStatusWithTheClockBesideIt(t *testing.T) {
	unsynchronized := false
	encoded, err := json.Marshal(report{
		Status:            pondnet.Status{State: "NoState", Addresses: []string{}, Health: []string{"why"}},
		ClockSynchronized: &unsynchronized,
	})
	if err != nil {
		t.Fatal(err)
	}
	var flat map[string]any
	if err := json.Unmarshal(encoded, &flat); err != nil {
		t.Fatal(err)
	}
	if flat["state"] != "NoState" || flat["clockSynchronized"] != false {
		t.Fatalf("not one flat status: %s", encoded)
	}
	if health, ok := flat["health"].([]any); !ok || len(health) != 1 || health[0] != "why" {
		t.Fatalf("health is missing from the line: %s", encoded)
	}
	encoded, err = json.Marshal(report{Status: pondnet.Status{State: "Running", Addresses: []string{}}})
	if err != nil {
		t.Fatal(err)
	}
	if strings.Contains(string(encoded), "clockSynchronized") || strings.Contains(string(encoded), "health") {
		t.Fatalf("an unknown clock or no health must be left out: %s", encoded)
	}
}
