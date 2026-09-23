// Package enrollment authorizes pilot households without exposing Headscale administration.
package enrollment

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"errors"
	"golang.org/x/sys/unix"
	"io"
	"net/netip"
	"os"
	"path/filepath"
	"strings"
	"sync"
)

// Household is provisioned by an operator, never by an unauthenticated client.
type Household struct {
	PublicKey string `json:"publicKey"`
	UserID    string `json:"userId"`
	Port      uint16 `json:"port"`
}

// Device records the node authorized for one paired device. Pending entries deny reuse.
type Device struct {
	Key        string `json:"nodeKey,omitempty"`
	MachineKey string `json:"machineKey,omitempty"`
	NodeID     string `json:"nodeId"`
	Address    string `json:"address"`
	Role       string `json:"role"`
	Status     string `json:"status"`
	// Revision changes on every explicit enrollment or revocation, preventing stale replacement.
	Revision string `json:"revision,omitempty"`
}
type state struct {
	Households map[string]Household         `json:"households"`
	Devices    map[string]map[string]Device `json:"devices"`
	Requests   map[string]int64             `json:"requests"`
	Retired    map[string][]Device          `json:"retired,omitempty"`
}

// Store holds an exclusive process lock and atomically persists every security transition.
type Store struct {
	mu        sync.Mutex
	directory string
	lock      *os.File
	value     state
	failure   error
}

// Open loads existing state; malformed, missing-after-creation or insecure state fails closed.
func Open(directory string) (*Store, error) {
	if !filepath.IsAbs(directory) {
		return nil, errors.New("state directory must be absolute")
	}
	if err := os.Mkdir(directory, 0700); err != nil && !os.IsExist(err) {
		return nil, err
	}
	info, err := os.Lstat(directory)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return nil, errors.New("state directory is not private")
	}
	fd, err := unix.Open(filepath.Join(directory, "store.lock"), unix.O_CREAT|unix.O_RDWR|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return nil, err
	}
	lock := os.NewFile(uintptr(fd), "store.lock")
	if err = unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		lock.Close()
		return nil, err
	}
	s := &Store{directory: directory, lock: lock, value: state{Households: map[string]Household{}, Devices: map[string]map[string]Device{}, Requests: map[string]int64{}}}
	path := filepath.Join(directory, "state.json")
	info, err = os.Lstat(path)
	if os.IsNotExist(err) {
		if _, e := os.Stat(filepath.Join(directory, "initialized")); e == nil {
			s.Close()
			return nil, errors.New("enrollment state is missing")
		}
		if err = s.save(); err == nil {
			err = writeMarker(directory)
		}
	} else if err == nil {
		if !info.Mode().IsRegular() || info.Mode().Perm()&0077 != 0 || info.Size() > 16<<20 {
			err = errors.New("invalid enrollment state file")
		} else {
			var f *os.File
			f, err = os.Open(path)
			if err == nil {
				d := json.NewDecoder(io.LimitReader(f, 16<<20))
				d.DisallowUnknownFields()
				err = d.Decode(&s.value)
				if err == nil && d.Decode(new(any)) != io.EOF {
					err = errors.New("trailing state data")
				}
				f.Close()
			}
		}
	}
	if err == nil {
		if s.value.Retired == nil {
			s.value.Retired = map[string][]Device{}
		}
		err = validateState(s.value)
	}
	if err != nil {
		s.Close()
		return nil, err
	}
	return s, nil
}

// Close releases ownership. It never removes household state.
func (s *Store) Close() error { return s.lock.Close() }
func (s *Store) save() (err error) {
	if s.failure != nil {
		return s.failure
	}
	defer func() {
		if err != nil {
			s.failure = errors.New("enrollment persistence failed; restart after storage repair")
		}
	}()
	data, err := json.Marshal(s.value)
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(s.directory, ".state-")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err = f.Write(data); err == nil {
		err = f.Sync()
	}
	closeErr := f.Close()
	if err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	if err = os.Rename(f.Name(), filepath.Join(s.directory, "state.json")); err != nil {
		return err
	}
	dir, err := os.Open(s.directory)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

// Provision binds one immutable household key to a Headscale user, whether an
// operator created it or the household registered itself.
func (s *Store) Provision(id string, h Household) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.provisionLocked(id, h)
}

// provisionLocked is the same operation for a caller that already holds the lock,
// such as a registration that must create the user and the household as one step.
func (s *Store) provisionLocked(id string, h Household) error {
	key, err := base64.StdEncoding.DecodeString(h.PublicKey)
	if !identifier.MatchString(id) || err != nil || len(key) != ed25519.PublicKeySize || !numeric.MatchString(h.UserID) || h.Port == 0 {
		return errors.New("invalid household")
	}
	if old, ok := s.value.Households[id]; ok {
		if old == h {
			return nil
		}
		return errors.New("household already exists")
	}
	for _, other := range s.value.Households {
		if other.UserID == h.UserID || other.PublicKey == h.PublicKey {
			return errors.New("household authority must be unique")
		}
	}
	s.value.Households[id] = h
	s.value.Devices[id] = map[string]Device{}
	if err = s.save(); err != nil {
		delete(s.value.Households, id)
		delete(s.value.Devices, id)
	}
	return err
}

// Validate before generating policy; syntactically valid but corrupt state must not grant access.
func validateState(v state) error {
	invalid := errors.New("invalid enrollment state")
	if v.Households == nil || v.Devices == nil || v.Requests == nil {
		return invalid
	}
	users, keys, nodes, addresses := map[string]bool{}, map[string]bool{}, map[string]bool{}, map[string]bool{}
	machines := map[string]bool{}
	for id, h := range v.Households {
		key, err := base64.StdEncoding.DecodeString(h.PublicKey)
		if !identifier.MatchString(id) || err != nil || len(key) != ed25519.PublicKeySize || !numeric.MatchString(h.UserID) || h.Port == 0 || users[h.UserID] || keys[h.PublicKey] {
			return invalid
		}
		users[h.UserID], keys[h.PublicKey] = true, true
		devices, ok := v.Devices[id]
		if !ok || devices == nil || len(devices) > 256 {
			return invalid
		}
		ponds := 0
		for deviceID, d := range devices {
			if d.Revision != "" && !identifier.MatchString(d.Revision) {
				return invalid
			}
			if d.MachineKey != "" {
				if !machineKeyPattern.MatchString(d.MachineKey) || machines[d.MachineKey] {
					return invalid
				}
				machines[d.MachineKey] = true
			}
			if d.Key != "" && !nodeKeyPattern.MatchString(d.Key) {
				return invalid
			}
			if !identifier.MatchString(deviceID) || (d.Role != "pond" && d.Role != "phone") {
				return invalid
			}
			if d.Role == "pond" {
				ponds++
				if ponds > 1 {
					return invalid
				}
			}
			switch d.Status {
			case "pending", "active", "revoking", "revoked", "failed":
			default:
				return invalid
			}
			if d.NodeID == "" && d.Address == "" && d.Status != "active" {
				continue
			}
			ip, err := netip.ParseAddr(d.Address)
			if !nodeKeyPattern.MatchString(d.Key) || !numeric.MatchString(d.NodeID) || err != nil || !netip.MustParsePrefix("100.64.0.0/10").Contains(ip) || nodes[d.NodeID] || addresses[d.Address] {
				return invalid
			}
			nodes[d.NodeID], addresses[d.Address] = true, true
		}
	}
	for household, retired := range v.Retired {
		if _, ok := v.Households[household]; !ok || len(retired) > 256 {
			return invalid
		}
		for _, d := range retired {
			if d.Role != "phone" || d.Status != "revoked" || (d.Revision != "" && !identifier.MatchString(d.Revision)) {
				return invalid
			}
			if d.MachineKey != "" {
				if !machineKeyPattern.MatchString(d.MachineKey) || machines[d.MachineKey] {
					return invalid
				}
				machines[d.MachineKey] = true
			}
			if d.Key != "" && !nodeKeyPattern.MatchString(d.Key) {
				return invalid
			}
			if d.NodeID != "" && (!numeric.MatchString(d.NodeID) || d.Key == "") {
				return invalid
			}
			if d.Address != "" {
				ip, err := netip.ParseAddr(d.Address)
				if err != nil || !netip.MustParsePrefix("100.64.0.0/10").Contains(ip) {
					return invalid
				}
			}
		}
	}
	for id := range v.Devices {
		if _, ok := v.Households[id]; !ok {
			return invalid
		}
	}
	for key, expiry := range v.Requests {
		parts := strings.Split(key, ":")
		if len(parts) != 2 || !identifier.MatchString(parts[1]) || expiry <= 0 {
			return invalid
		}
		if _, ok := v.Households[parts[0]]; !ok {
			return invalid
		}
	}
	return nil
}
func writeMarker(directory string) error {
	f, err := os.OpenFile(filepath.Join(directory, "initialized"), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return err
	}
	_, err = f.WriteString("1\n")
	if err == nil {
		err = f.Sync()
	}
	closed := f.Close()
	if err == nil {
		err = closed
	}
	if err != nil {
		return err
	}
	dir, err := os.Open(directory)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}
