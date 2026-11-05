// Package pondnet embeds an application-scoped Tailscale node. It never creates
// an OS VPN interface or changes routes for another application.
package pondnet

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"net/netip"
	"net/url"
	"os"
	"path/filepath"
	"regexp"
	"runtime"
	"sort"
	"strings"
	"sync"
	"time"
	"unicode/utf8"

	"golang.org/x/sys/unix"
	"tailscale.com/envknob"
	"tailscale.com/ipn"
	"tailscale.com/logtail"
	"tailscale.com/tsnet"
	"tailscale.com/types/key"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/tailnetdial"
)

var hostnamePattern = regexp.MustCompile(`^[a-z0-9][a-z0-9-]{0,61}[a-z0-9]$`)
var tailnet4 = netip.MustParsePrefix("100.64.0.0/10")
var tailnet6 = netip.MustParsePrefix("fd7a:115c:a1e0::/48")

// authURLPattern matches the enrollment capability wherever the backend prints
// it. The backend announces the registration URL in plain text, and Status
// documents that this value must never enter logs, so every line is filtered
// before it reaches a sink rather than trusting callers to be careful.
var authURLPattern = regexp.MustCompile(`https?://[^\s"'` + "`" + `]*/register/[^\s"'` + "`" + `]*`)

var diagnosticsMu sync.RWMutex
var diagnosticsSink func(string)

// SetDiagnostics installs a sink for backend log lines, or removes it with nil.
//
// Backend logging is discarded by default and that default is deliberate: it is
// verbose and names addresses and keys, and this process ships no remote
// logging. Turning it on is a local debugging decision for a debug build, never
// something a release does. Lines are redacted before the sink sees them.
//
// Without a sink, a node that cannot reach its control server fails completely
// silently, which is how a field failure becomes undiagnosable.
func SetDiagnostics(sink func(string)) {
	diagnosticsMu.Lock()
	diagnosticsSink = sink
	diagnosticsMu.Unlock()
}

// Redact strips a node-authorisation URL from a line about to be logged.
//
// That URL is a bearer capability for joining this household, and the backend
// announces it in plain text. Exported so every path that writes a line to a
// log goes through the same rule rather than each deciding for itself.
func Redact(line string) string {
	return authURLPattern.ReplaceAllString(line, "<redacted enrollment URL>")
}

func backendLogf(format string, args ...any) {
	diagnosticsMu.RLock()
	sink := diagnosticsSink
	diagnosticsMu.RUnlock()
	if sink == nil {
		return
	}
	sink(Redact(fmt.Sprintf(format, args...)))
}

// Status is safe to display locally. AuthURL is an enrollment capability: it
// must never enter logs, analytics, notifications, or unauthenticated APIs.
type Status struct {
	State      string   `json:"state"`
	Addresses  []string `json:"addresses"`
	AuthURL    string   `json:"authUrl,omitempty"`
	NodeKey    string   `json:"nodeKey,omitempty"`
	MachineKey string   `json:"machineKey"`
	// Health is the backend's own account of what stands between it and a working node, such as
	// the last error from the control server while it cannot log in. Without it a node that
	// cannot reach its coordinator sits in NoState and says nothing at all. Redacted, one line
	// per warning and bounded by [healthLines], so it is fit for a log.
	Health []string `json:"health,omitempty"`
}

// Bounds on [Status.Health]. The backend raises a handful of warnings at most, and one error can
// quote a whole server response.
const (
	maxHealthLines     = 4
	maxHealthLineBytes = 300
)

// healthLines makes the backend's health warnings fit to log: each redacted, folded onto one
// line and cut to [maxHealthLineBytes], sorted so the same warnings always read the same, and
// at most [maxHealthLines] of them.
func healthLines(warnings []string) []string {
	var lines []string
	for _, warning := range warnings {
		line := strings.Join(strings.Fields(Redact(warning)), " ")
		if line == "" {
			continue
		}
		if len(line) > maxHealthLineBytes {
			cut := maxHealthLineBytes
			for cut > 0 && !utf8.RuneStart(line[cut]) {
				cut--
			}
			line = line[:cut] + "..."
		}
		lines = append(lines, line)
	}
	sort.Strings(lines)
	if len(lines) > maxHealthLines {
		lines = lines[:maxHealthLines]
	}
	return lines
}

// Node owns its state lock and userspace networking stack until Close.
type Node struct {
	Server *tsnet.Server
	lock   *os.File
	store  *identityStore
	once   sync.Once
}

// Open starts a persistent node without waiting for browser authorization.
// A Headscale HTTPS origin is mandatory; no hosted-service fallback is allowed.
func Open(dir, hostname, control string) (*Node, error) {
	if !filepath.IsAbs(dir) || !hostnamePattern.MatchString(hostname) {
		return nil, errors.New("invalid embedded node configuration")
	}
	if control == "" {
		return nil, errors.New("Headscale control server is required")
	}
	{
		u, err := url.Parse(control)
		if err != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
			return nil, errors.New("control server must be an HTTPS origin")
		}
	}
	if err := os.Mkdir(dir, 0700); err != nil && !os.IsExist(err) {
		return nil, err
	}
	info, err := os.Lstat(dir)
	if err != nil {
		return nil, err
	}
	if !info.IsDir() || info.Mode()&os.ModeSymlink != 0 || info.Mode().Perm()&0077 != 0 {
		return nil, errors.New("embedded identity directory must be private and not a symlink")
	}
	// A new identity may be created only until one has been written once. This used to be
	// keyed on creating the lock file, so a crash between that and the first write left a
	// node that refused every later start for want of an identity it never had.
	_, err = os.Lstat(filepath.Join(dir, initializedMarker))
	fresh := errors.Is(err, os.ErrNotExist)
	fd, err := unix.Open(filepath.Join(dir, "node.lock"), unix.O_RDWR|unix.O_CREAT|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return nil, err
	}
	lock := os.NewFile(uintptr(fd), "node.lock")
	if err = unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		lock.Close()
		return nil, errors.New("embedded identity is already in use")
	}
	identity, err := openIdentityStore(dir, control, fresh)
	if err == nil && fresh {
		err = markInitialized(dir)
	}
	if err != nil {
		lock.Close()
		return nil, err
	}
	// Mobile applications have neither a shell home nor a writable system temp
	// directory. Even with remote logging disabled, the upstream backend asks
	// for a local diagnostics directory during construction. Keep it inside the
	// already validated private profile instead of letting that lookup panic.
	if runtime.GOOS == "android" || runtime.GOOS == "ios" {
		if err := os.Setenv("TS_LOGS_DIR", dir); err != nil {
			lock.Close()
			return nil, err
		}
	}
	// Opt out before starting any backend; discarded local logging alone does not
	// disable Tailscale's separate remote diagnostic uploader.
	envknob.SetNoLogsNoSupport()
	// tsnet also keeps its own log ring on disk in the profile, unredacted, whatever Logf
	// does. Disabling logtail drops entries before they reach it, and what earlier builds
	// wrote there is removed.
	logtail.Disable()
	if err := removeBackendLogs(dir); err != nil {
		lock.Close()
		return nil, err
	}
	s := &tsnet.Server{Dir: dir, Store: identity, Hostname: hostname, ControlURL: control,
		Logf: backendLogf, UserLogf: backendLogf}
	if err := s.Start(); err != nil {
		s.Close()
		lock.Close()
		return nil, err
	}
	return &Node{Server: s, lock: lock, store: identity}, nil
}

// Snapshot excludes peer/account details and bounds the local status request.
func (n *Node) Snapshot() (Status, error) {
	if err := n.store.health(); err != nil {
		return Status{}, err
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	lc, err := n.Server.LocalClient()
	if err != nil {
		return Status{}, err
	}
	s, err := lc.StatusWithoutPeers(ctx)
	if err != nil {
		return Status{}, err
	}
	result := Status{State: s.BackendState, Addresses: []string{}, AuthURL: s.AuthURL, Health: healthLines(s.Health)}
	encoded, err := n.store.ReadState(ipn.MachineKeyStateKey)
	var machine key.MachinePrivate
	if err != nil || machine.UnmarshalText(encoded) != nil || machine.IsZero() {
		return Status{}, errors.New("embedded machine identity unavailable")
	}
	result.MachineKey = machine.Public().String()
	// The local preferences API deliberately strips private key material. The
	// status response provides the public node key once registration is complete.
	if s.Self != nil && !s.Self.PublicKey.IsZero() {
		result.NodeKey = s.Self.PublicKey.String()
	}
	for _, ip := range s.TailscaleIPs {
		if TailnetIP(ip) {
			result.Addresses = append(result.Addresses, ip.String())
		}
	}
	return result, nil
}

// SnapshotJSON is the small string boundary used by native bindings.
func (n *Node) SnapshotJSON() (string, error) {
	s, err := n.Snapshot()
	if err != nil {
		return "", err
	}
	b, err := json.Marshal(s)
	return string(b), err
}

// Dial connects only through this node's userspace WireGuard stack. Server.Dial
// must not be used here: tsnet's general-purpose UserDial can select the system
// dialer when an address is absent from the current peer map, including OS VPNs.
// The subsystem API is deliberately covered by the pinned-version live tests.
func (n *Node) Dial(ctx context.Context, network, address string) (net.Conn, error) {
	if err := n.store.health(); err != nil {
		return nil, err
	}
	peer, err := PeerAddress(address)
	if err != nil || network != "tcp" {
		return nil, errors.New("embedded dial requires a tailnet TCP destination")
	}
	return tailnetdial.TCP(ctx, n.Server, peer)
}

// Close stops all userspace networking and releases the identity lock.
func (n *Node) Close() { n.once.Do(func() { n.Server.Close(); n.lock.Close() }) }

// TailnetIP rejects LAN, internet, loopback and mapped addresses.
func TailnetIP(ip netip.Addr) bool { return tailnet4.Contains(ip) || tailnet6.Contains(ip) }

// PeerAddress accepts only a real tailnet socket address, never forwarded IP text.
func PeerAddress(value string) (netip.AddrPort, error) {
	peer, err := netip.ParseAddrPort(value)
	if err != nil || peer.Port() == 0 || !TailnetIP(peer.Addr()) {
		return netip.AddrPort{}, errors.New("invalid tailnet peer")
	}
	return peer, nil
}

// MachineOf asks the tailnet which machine holds a peer address; see [MachineOf].
func (n *Node) MachineOf(ctx context.Context, remoteAddr string) (string, error) {
	lc, err := n.Server.LocalClient()
	if err != nil {
		return "", err
	}
	who, err := lc.WhoIs(ctx, remoteAddr)
	if err != nil {
		return "", err
	}
	if who.Node == nil || who.Node.Machine.IsZero() {
		return "", errors.New("tailnet peer has no machine key")
	}
	return who.Node.Machine.String(), nil
}

// DialContext is implemented by tsnet and permits controlled transport tests.
type DialContext func(context.Context, string, string) (net.Conn, error)

// initializedMarker records that this profile has held an identity; see Open.
const initializedMarker = "initialized"

func markInitialized(dir string) error {
	f, err := os.OpenFile(filepath.Join(dir, initializedMarker), os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if errors.Is(err, os.ErrExist) {
		return nil
	}
	if err != nil {
		return err
	}
	err = f.Sync()
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	parent, err := os.Open(dir)
	if err != nil {
		return err
	}
	defer parent.Close()
	return parent.Sync()
}

// removeBackendLogs deletes the on-disk log ring tsnet keeps beside the identity.
func removeBackendLogs(dir string) error {
	for _, name := range []string{"tailscaled.log1.txt", "tailscaled.log2.txt"} {
		if err := os.Remove(filepath.Join(dir, name)); err != nil && !errors.Is(err, os.ErrNotExist) {
			return err
		}
	}
	return nil
}
