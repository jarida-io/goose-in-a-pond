// Package mobile is the gomobile binding boundary shared by Android and iOS.
package mobile

import (
	"encoding/json"
	"errors"
	"sync"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

var lock sync.Mutex
var node *pondnet.Node
var proxy *pondnet.Proxy

// Diagnostics receives redacted backend log lines through the binding boundary.
// The native layer installs one only for a debuggable build; a release build
// leaves backend logging discarded, as it is by default.
type Diagnostics interface {
	Log(line string)
}

var diagnosticsMu sync.RWMutex
var diagnostics Diagnostics

// SetDiagnostics installs or, with nil, clears the backend diagnostic sink. The
// binding layer keeps its own reference so it can report its own failures: a
// resolver that cannot be reached is invisible in the backend's log, which
// reports only that a name did not resolve.
func SetDiagnostics(sink Diagnostics) {
	diagnosticsMu.Lock()
	diagnostics = sink
	diagnosticsMu.Unlock()
	if sink == nil {
		pondnet.SetDiagnostics(nil)
		return
	}
	pondnet.SetDiagnostics(func(line string) { sink.Log(line) })
}

// diagnose reports a binding-layer event when an operator has enabled
// diagnostics. It is a no-op otherwise, like the backend sink.
// diagnose records something this package decided was worth saying: a handful
// of deliberate lines about why the node cannot do its job.
//
// Always recorded when an event sink is installed, and the native layer
// installs one unconditionally. These are not tailscale's backend log -- that
// is verbose, names addresses and keys, and stays behind `SetDiagnostics`.
// These are ours, there are a couple of them, and they are the difference
// between "the node did not connect" and "the first resolver refused TCP".
//
// Requiring somebody to find a switch and reproduce the failure before the
// reason is written down is how a field failure becomes undiagnosable, which is
// the thing the switch was meant to prevent.
func diagnose(line string) {
	eventMu.RLock()
	sink := events
	eventMu.RUnlock()
	if sink != nil {
		sink.Log(pondnet.Redact(line))
	}
}

var eventMu sync.RWMutex
var events Diagnostics

// SetEventLog installs the sink for this package's own diagnostic lines, or
// removes it with nil. Installed unconditionally by the native layer; see
// `diagnose`.
func SetEventLog(sink Diagnostics) {
	eventMu.Lock()
	events = sink
	eventMu.Unlock()
}

// Start owns one node in the application's private directory. It does not touch
// the separately installed Tailscale app or the operating system VPN settings.
func Start(directory, hostname, control string) error {
	lock.Lock()
	defer lock.Unlock()
	if node != nil {
		return errors.New("embedded networking is already started")
	}
	n, err := pondnet.Open(directory, hostname, control)
	if err != nil {
		return err
	}
	p, err := pondnet.NewProxy(n.Dial)
	if err != nil {
		n.Close()
		return err
	}
	node = n
	proxy = p
	return nil
}

// Status returns connection/enrollment state, with no peer or account inventory.
func Status() (string, error) {
	lock.Lock()
	defer lock.Unlock()
	if node == nil {
		return `{"state":"Stopped","addresses":[]}`, nil
	}
	return node.SnapshotJSON()
}

// ProxyConfiguration is for native networking only; do not expose it to JS.
func ProxyConfiguration() (string, error) {
	lock.Lock()
	defer lock.Unlock()
	if proxy == nil {
		return "", errors.New("embedded networking is stopped")
	}
	b, err := json.Marshal(map[string]string{"address": proxy.Address(), "credential": proxy.Credential()})
	return string(b), err
}

// SetTargets atomically scopes the proxy to the paired Pond's tailnet addresses.
func SetTargets(encoded string) error {
	lock.Lock()
	defer lock.Unlock()
	if proxy == nil {
		return errors.New("embedded networking is stopped")
	}
	if len(encoded) > 4096 {
		return errors.New("target configuration is too large")
	}
	var targets []string
	if err := json.Unmarshal([]byte(encoded), &targets); err != nil {
		return err
	}
	return proxy.SetTargets(targets)
}

// Stop invalidates tunnels and releases the node without deleting its identity.
func Stop() {
	lock.Lock()
	defer lock.Unlock()
	if proxy != nil {
		proxy.Close()
		proxy = nil
	}
	if node != nil {
		node.Close()
		node = nil
	}
}
