package pondnet

import (
	"context"
	"crypto/rand"
	"crypto/subtle"
	"encoding/base64"
	"errors"
	"io"
	"net"
	"net/http"
	"net/netip"
	"sync"
	"time"
)

// Proxy is an authenticated, allowlisted loopback CONNECT transport. TLS remains
// between the mobile native client and the Pond, including hostname/SPKI checks.
type Proxy struct {
	listener   net.Listener
	server     *http.Server
	dial       DialContext
	credential string
	mu         sync.Mutex
	targets    map[string]bool
	active     map[net.Conn]bool
	closed     bool
	limit      chan struct{}
	generation uint64
	pending    map[uint64]context.CancelFunc
	nextDial   uint64
}

// NewProxy binds an ephemeral IPv4 loopback port and starts serving CONNECT.
func NewProxy(dial DialContext) (*Proxy, error) {
	key := make([]byte, 32)
	if _, err := rand.Read(key); err != nil {
		return nil, err
	}
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		return nil, err
	}
	p := &Proxy{listener: listener, dial: dial, credential: base64.RawURLEncoding.EncodeToString(key), targets: map[string]bool{}, active: map[net.Conn]bool{}, limit: make(chan struct{}, 32), pending: map[uint64]context.CancelFunc{}}
	p.server = &http.Server{Handler: p, ReadHeaderTimeout: 5 * time.Second, MaxHeaderBytes: 8192}
	go p.server.Serve(listener)
	return p, nil
}

// Address is local transport configuration, never a saved Pond URL.
func (p *Proxy) Address() string { return p.listener.Addr().String() }

// Credential must stay in native memory, out of JS persistence and logs.
func (p *Proxy) Credential() string { return p.credential }

// SetTargets replaces the tailnet socket allowlist and invalidates live tunnels.
func (p *Proxy) SetTargets(targets []string) error {
	if len(targets) > 8 {
		return errors.New("too many embedded targets")
	}
	next := make(map[string]bool)
	for _, target := range targets {
		peer, err := PeerAddress(target)
		if err != nil {
			return err
		}
		next[peer.String()] = true
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if p.closed {
		return net.ErrClosed
	}
	p.generation++
	for _, cancel := range p.pending {
		cancel()
	}
	p.targets = next
	for conn := range p.active {
		conn.Close()
	}
	return nil
}

// Close cancels active streams as well as pending accepts.
func (p *Proxy) Close() {
	p.mu.Lock()
	p.closed = true
	p.generation++
	for _, cancel := range p.pending {
		cancel()
	}
	for conn := range p.active {
		conn.Close()
	}
	p.mu.Unlock()
	p.server.Close()
}

func (p *Proxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	expected := "Basic " + base64.StdEncoding.EncodeToString([]byte("pond:"+p.credential))
	if subtle.ConstantTimeCompare([]byte(r.Header.Get("Proxy-Authorization")), []byte(expected)) != 1 {
		w.Header().Set("Proxy-Authenticate", `Basic realm="pond"`)
		http.Error(w, "proxy authorization required", http.StatusProxyAuthRequired)
		return
	}
	peer, err := netip.ParseAddrPort(r.Host)
	p.mu.Lock()
	epoch := p.generation
	allowed := err == nil && p.targets[peer.String()] && !p.closed
	p.mu.Unlock()
	if r.Method != http.MethodConnect || r.RequestURI != r.Host || !allowed {
		http.Error(w, "target refused", http.StatusForbidden)
		return
	}
	select {
	case p.limit <- struct{}{}:
		defer func() { <-p.limit }()
	default:
		http.Error(w, "transport busy", http.StatusServiceUnavailable)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
	p.mu.Lock()
	if p.closed || p.generation != epoch {
		p.mu.Unlock()
		cancel()
		return
	}
	p.nextDial++
	dialID := p.nextDial
	p.pending[dialID] = cancel
	p.mu.Unlock()
	upstream, err := p.dial(ctx, "tcp", peer.String())
	p.mu.Lock()
	delete(p.pending, dialID)
	p.mu.Unlock()
	cancel()
	if err != nil {
		http.Error(w, "remote unavailable", http.StatusBadGateway)
		return
	}
	defer upstream.Close()
	hijacker, ok := w.(http.Hijacker)
	if !ok {
		http.Error(w, "transport unavailable", http.StatusInternalServerError)
		return
	}
	client, buffered, err := hijacker.Hijack()
	if err != nil {
		return
	}
	defer client.Close()
	p.mu.Lock()
	if p.closed || p.generation != epoch || !p.targets[peer.String()] {
		p.mu.Unlock()
		return
	}
	p.active[client] = true
	p.active[upstream] = true
	p.mu.Unlock()
	defer func() { p.mu.Lock(); delete(p.active, client); delete(p.active, upstream); p.mu.Unlock() }()
	if _, err = buffered.WriteString("HTTP/1.1 200 Connection Established\r\n\r\n"); err != nil {
		return
	}
	if err = buffered.Flush(); err != nil {
		return
	}
	done := make(chan struct{})
	go func() { io.Copy(upstream, buffered); upstream.Close(); client.Close(); close(done) }()
	io.Copy(client, upstream)
	client.Close()
	upstream.Close()
	<-done
}
