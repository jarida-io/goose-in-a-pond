package pondnet

import (
	"bytes"
	"encoding/json"
	"os"
	"path/filepath"
	"testing"

	"tailscale.com/ipn"
	"tailscale.com/types/key"
	"tailscale.com/types/persist"
)

const fixtureControl = "https://127.0.0.1:9"

func validIdentity(t *testing.T) map[ipn.StateKey][]byte {
	t.Helper()
	machine, _ := key.NewMachine().MarshalText()
	prefs := ipn.NewPrefs()
	prefs.ControlURL = fixtureControl
	prefs.Persist = &persist.Persist{PrivateNodeKey: key.NewNode()}
	encodedPrefs, err := json.Marshal(prefs)
	if err != nil {
		t.Fatal(err)
	}
	profiles, _ := json.Marshal(map[ipn.ProfileID]ipn.LoginProfile{"abcd": {ID: "abcd", Key: "profile-abcd", ControlURL: fixtureControl}})
	return map[ipn.StateKey][]byte{ipn.MachineKeyStateKey: machine, ipn.KnownProfilesStateKey: profiles, ipn.CurrentProfileStateKey: []byte("profile-abcd"), "profile-abcd": encodedPrefs}
}

func TestIdentityStorePreservesLegacyKeysAndCoordinator(t *testing.T) {
	dir := t.TempDir()
	values := validIdentity(t)
	encoded, _ := json.Marshal(values)
	path := filepath.Join(dir, "tailscaled.state")
	if err := os.WriteFile(path, encoded, 0600); err != nil {
		t.Fatal(err)
	}
	for attempt := 0; attempt < 2; attempt++ {
		s, err := openIdentityStore(dir, fixtureControl, false)
		if err != nil {
			t.Fatal(err)
		}
		for name, want := range values {
			got, err := s.ReadState(name)
			if err != nil || !bytes.Equal(got, want) {
				t.Fatalf("identity changed: %s", name)
			}
		}
	}
	if _, err := openIdentityStore(dir, "https://other.example", false); err == nil {
		t.Fatal("identity moved to another coordinator")
	}
}

func TestIdentityStoreRejectsPartialAndMalformedProfiles(t *testing.T) {
	for _, damage := range []string{"machine_missing", "machine_zero", "profile_missing", "profile_empty", "node_missing", "node_invalid", "profiles_null", "selector_missing", "other_control"} {
		t.Run(damage, func(t *testing.T) {
			values := validIdentity(t)
			switch damage {
			case "machine_missing":
				delete(values, ipn.MachineKeyStateKey)
			case "machine_zero":
				values[ipn.MachineKeyStateKey], _ = (key.MachinePrivate{}).MarshalText()
			case "profile_missing":
				delete(values, "profile-abcd")
			case "profile_empty":
				values["profile-abcd"] = nil
			case "node_missing":
				values["profile-abcd"] = []byte(`{"ControlURL":"https://127.0.0.1:9"}`)
			case "node_invalid":
				values["profile-abcd"] = []byte(`{"Config":{"PrivateNodeKey":"broken"}}`)
			case "profiles_null":
				values[ipn.KnownProfilesStateKey] = []byte("null")
			case "selector_missing":
				values[ipn.CurrentProfileStateKey] = []byte("profile-lost")
			case "other_control":
				values[controlStateKey] = []byte("https://other.example")
			}
			data, _ := json.Marshal(values)
			dir := t.TempDir()
			if err := os.WriteFile(filepath.Join(dir, "tailscaled.state"), data, 0600); err != nil {
				t.Fatal(err)
			}
			if _, err := openIdentityStore(dir, fixtureControl, false); err == nil {
				t.Fatal("corrupt identity accepted")
			}
		})
	}
}

func TestIdentityStoreRejectsUnsafeFiles(t *testing.T) {
	for _, damage := range []string{"symlink", "directory", "public", "oversized", "trailing"} {
		t.Run(damage, func(t *testing.T) {
			dir := t.TempDir()
			path := filepath.Join(dir, "tailscaled.state")
			data, _ := json.Marshal(validIdentity(t))
			var err error
			switch damage {
			case "symlink":
				target := filepath.Join(t.TempDir(), "external")
				if err = os.WriteFile(target, data, 0600); err == nil {
					err = os.Symlink(target, path)
				}
			case "directory":
				err = os.Mkdir(path, 0700)
			case "public":
				if err = os.WriteFile(path, data, 0600); err == nil {
					err = os.Chmod(path, 0644)
				}
			case "oversized":
				err = os.WriteFile(path, make([]byte, maxIdentityBytes+1), 0600)
			case "trailing":
				err = os.WriteFile(path, append(data, []byte(" {}")...), 0600)
			}
			if err != nil {
				t.Fatal(err)
			}
			if _, err = openIdentityStore(dir, fixtureControl, false); err == nil {
				t.Fatal("unsafe file accepted")
			}
		})
	}
}

func TestIdentityStorePersistenceFailureIsTerminal(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "node")
	if err := os.Mkdir(dir, 0700); err != nil {
		t.Fatal(err)
	}
	s, err := openIdentityStore(dir, fixtureControl, true)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.Rename(dir, dir+"-offline"); err != nil {
		t.Fatal(err)
	}
	if s.WriteState("fixture", []byte("new")) == nil {
		t.Fatal("write failure hidden")
	}
	if err = os.Rename(dir+"-offline", dir); err != nil {
		t.Fatal(err)
	}
	if s.health() == nil || s.WriteState("fixture", []byte("again")) == nil {
		t.Fatal("failure not terminal")
	}
	if _, err = s.ReadState(ipn.MachineKeyStateKey); err == nil {
		t.Fatal("failed store readable")
	}
	restored, err := openIdentityStore(dir, fixtureControl, false)
	if err != nil {
		t.Fatal(err)
	}
	if _, err = restored.ReadState("fixture"); err != ipn.ErrStateNotExist {
		t.Fatal("failed write changed persistent state")
	}
}

func TestNodeRestartPreservesMachineIdentity(t *testing.T) {
	dir := filepath.Join(t.TempDir(), "node")
	var identity []byte
	for attempt := 0; attempt < 2; attempt++ {
		n, err := Open(dir, "pond-test", fixtureControl)
		if err != nil {
			t.Fatal(err)
		}
		stored, err := n.store.ReadState(ipn.MachineKeyStateKey)
		n.Close()
		if err != nil {
			t.Fatal(err)
		}
		if attempt == 0 {
			identity = stored
		} else if !bytes.Equal(identity, stored) {
			t.Fatal("restart changed identity")
		}
	}
}

func TestIdentityStoreRejectsSilentMachineReplacement(t *testing.T) {
	s, err := openIdentityStore(t.TempDir(), fixtureControl, true)
	if err != nil {
		t.Fatal(err)
	}
	replacement, _ := key.NewMachine().MarshalText()
	if s.WriteState(ipn.MachineKeyStateKey, replacement) == nil || s.health() == nil {
		t.Fatal("machine identity replaced")
	}
}
