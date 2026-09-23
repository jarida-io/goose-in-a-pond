package pondnet

import (
	"bytes"
	"encoding/json"
	"errors"
	"io"
	"maps"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"golang.org/x/sys/unix"
	"tailscale.com/ipn"
	"tailscale.com/types/key"
)

const controlStateKey ipn.StateKey = "_pond-control"
const maxIdentityBytes = 4 << 20

// identityStore keeps the upstream state-file format, but never treats a lost or
// empty existing identity as a new installation. node.lock is the creation marker
// and must be backed up with tailscaled.state. The caller holds its process lock.
type identityStore struct {
	mu      sync.Mutex
	path    string
	values  map[ipn.StateKey][]byte
	failure error
}

func openIdentityStore(directory, control string, fresh bool) (*identityStore, error) {
	s := &identityStore{path: filepath.Join(directory, "tailscaled.state")}
	fd, err := unix.Open(s.path, unix.O_RDONLY|unix.O_NOFOLLOW|unix.O_NONBLOCK, 0)
	if errors.Is(err, os.ErrNotExist) && fresh {
		machine, _ := key.NewMachine().MarshalText()
		s.values = map[ipn.StateKey][]byte{ipn.MachineKeyStateKey: machine, controlStateKey: []byte(control)}
		if err = s.persist(s.values); err != nil {
			return nil, err
		}
		return s, nil
	}
	if err != nil {
		return nil, errors.New("embedded identity is missing or unreadable; restore its backup")
	}
	f := os.NewFile(uintptr(fd), "embedded identity")
	defer f.Close()
	info, err := f.Stat()
	if err != nil || !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > maxIdentityBytes {
		return nil, errors.New("embedded identity file is not private or regular")
	}
	d := json.NewDecoder(io.LimitReader(f, maxIdentityBytes+1))
	if d.Decode(&s.values) != nil || d.Decode(new(any)) != io.EOF || validateIdentity(s.values, control) != nil {
		return nil, errors.New("embedded identity is corrupt or belongs to another coordinator; restore its backup")
	}
	// Adopt valid identities from the earlier helper without changing their keys.
	if _, ok := s.values[controlStateKey]; !ok {
		s.values[controlStateKey] = []byte(control)
		if err = s.persist(s.values); err != nil {
			return nil, err
		}
	}
	return s, nil
}

func validateIdentity(values map[ipn.StateKey][]byte, control string) error {
	invalid := errors.New("invalid embedded identity")
	var machine key.MachinePrivate
	if machine.UnmarshalText(values[ipn.MachineKeyStateKey]) != nil || machine.IsZero() {
		return invalid
	}
	if saved, ok := values[controlStateKey]; ok && string(saved) != control {
		return invalid
	}
	profiles := map[ipn.ProfileID]ipn.LoginProfile{}
	if encoded, ok := values[ipn.KnownProfilesStateKey]; ok {
		if json.Unmarshal(encoded, &profiles) != nil || profiles == nil {
			return invalid
		}
	}
	for id, profile := range profiles {
		if profile.ID != id || profile.Key == "" || len(values[profile.Key]) == 0 || profile.ControlURL != control {
			return invalid
		}
	}
	for _, selector := range []ipn.StateKey{ipn.CurrentProfileStateKey, ipn.ServerModeStartKey} {
		if selected := string(values[selector]); selected != "" {
			found := false
			for _, profile := range profiles {
				if string(profile.Key) == selected {
					found = true
				}
			}
			if !found {
				return invalid
			}
		}
	}
	for name, value := range values {
		if strings.HasPrefix(string(name), "profile-") || name == ipn.LegacyGlobalDaemonStateKey {
			prefs := ipn.NewPrefs()
			if ipn.PrefsFromBytes(value, prefs) != nil || prefs.Persist == nil || prefs.Persist.PrivateNodeKey.IsZero() || prefs.ControlURL != control {
				return invalid
			}
		}
	}
	return nil
}

func (s *identityStore) ReadState(id ipn.StateKey) ([]byte, error) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return nil, s.failure
	}
	value, ok := s.values[id]
	if !ok {
		return nil, ipn.ErrStateNotExist
	}
	return bytes.Clone(value), nil
}

func (s *identityStore) WriteState(id ipn.StateKey, value []byte) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return s.failure
	}
	if bytes.Equal(s.values[id], value) {
		return nil
	}
	if id == ipn.MachineKeyStateKey || id == controlStateKey {
		s.failure = errors.New("embedded identity replacement requires explicit local recovery")
		return s.failure
	}
	next := maps.Clone(s.values)
	if value == nil {
		delete(next, id)
	} else {
		next[id] = bytes.Clone(value)
	}
	if err := s.persist(next); err != nil {
		s.failure = errors.New("embedded identity persistence failed; repair storage before restarting")
		return s.failure
	}
	s.values = next
	return nil
}

func (s *identityStore) health() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.failure
}

func (s *identityStore) persist(values map[ipn.StateKey][]byte) error {
	data, err := json.Marshal(values)
	if err != nil {
		return err
	}
	if len(data) > maxIdentityBytes {
		return errors.New("embedded identity exceeds storage limit")
	}
	directory := filepath.Dir(s.path)
	f, err := os.CreateTemp(directory, ".node-state-")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err = f.Write(data); err == nil {
		err = f.Sync()
	}
	closed := f.Close()
	if err == nil {
		err = closed
	}
	if err != nil {
		return err
	}
	if err = os.Rename(f.Name(), s.path); err != nil {
		return err
	}
	dir, err := os.Open(directory)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
