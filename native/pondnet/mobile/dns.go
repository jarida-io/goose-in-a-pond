package mobile

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"net"
	"net/netip"
	"sync"
	"time"
)

// Resolvers are the nameservers the operating system reports for the active
// network. The native layer pushes them in; nothing here calls back into it.
//
// On Android the platform resolver does not answer for this node. Observed on a
// Galaxy A57: a lookup of the coordinator hostname hung, then fell through to
// Tailscale's bootstrap DNS, which knows nothing about a self-hosted coordinator
// and discloses its hostname to tailscale.com while asking. The node never
// fetched a control key and never logged in.
//
// Why the platform resolver fails there is not established - the cgo resolver is
// linked and Go prefers it on Android, and the same name resolves from a shell on
// the same device. This routes around that rather than explaining it.
//
// Push, not pull. An earlier build asked the native layer for the servers from
// inside the dial, and the Java round trip blocked long enough that the lookup
// context was already cancelled by the time it returned. A dial must touch only
// memory.
//
// Only server addresses cross this boundary. No query, answer or search domain is
// collected.

var resolverMu sync.RWMutex
var resolvers []netip.AddrPort

// SetResolvers installs the nameservers to query, as a JSON array of textual IP
// addresses. It takes no lifecycle lock: the native layer calls it from a network
// callback that must never block behind a starting or stopping node.
//
// An empty or unusable list is kept rather than applied: the operating system
// reports no active network for a moment while the app reloads or a link changes,
// and failing every lookup in that window would be worse than answering from the
// last good list.
func SetResolvers(encoded string) error {
	parsed, err := parseResolvers(encoded)
	if err != nil {
		return err
	}
	if len(parsed) == 0 {
		return nil
	}
	resolverMu.Lock()
	resolvers = parsed
	resolverMu.Unlock()
	return nil
}

// parseResolvers accepts a JSON array of textual IP addresses. A malformed entry
// rejects the whole list: a partially understood resolver set is worse than none,
// because the node would silently query the wrong network.
func parseResolvers(raw string) ([]netip.AddrPort, error) {
	invalid := errors.New("invalid native resolver metadata")
	if len(raw) > 4096 {
		return nil, invalid
	}
	var records []string
	if json.Unmarshal([]byte(raw), &records) != nil || records == nil || len(records) > 8 {
		return nil, invalid
	}
	result := make([]netip.AddrPort, 0, len(records))
	seen := map[string]bool{}
	for _, record := range records {
		address, err := netip.ParseAddr(record)
		// A resolver that is unspecified, loopback or multicast is never a server
		// this node can usefully query, and loopback in particular is the value Go
		// invents when it finds no configuration at all.
		if err != nil || !address.IsValid() || address.IsUnspecified() || address.IsLoopback() || address.IsMulticast() || seen[address.String()] {
			return nil, invalid
		}
		seen[address.String()] = true
		// Link-local servers are kept. On a home router the link-local address is
		// often the only one that answers, and it needs a zone to be dialable - a
		// numeric one, because resolving an interface name needs netlink, which
		// this platform denies to applications.
		if address.IsLinkLocalUnicast() && address.Zone() == "" {
			continue
		}
		result = append(result, netip.AddrPortFrom(address, 53))
	}
	return result, nil
}

func configuredServers() []netip.AddrPort {
	resolverMu.RLock()
	defer resolverMu.RUnlock()
	return resolvers
}

// udpProbe builds a DNS query for the root zone with a random id. The answer is
// not used: this asks whether the server responds at all, not what it says.
func udpProbe() []byte {
	id := make([]byte, 2)
	if _, err := rand.Read(id); err != nil {
		// A predictable id is still fine for a reachability probe on the local
		// link; failing the lookup over it would not be.
		id[0], id[1] = 0x50, 0x4e
	}
	return []byte{
		id[0], id[1],
		0x01, 0x00, // standard query, recursion desired
		0x00, 0x01, // one question
		0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
		0x00,       // root name
		0x00, 0x02, // NS
		0x00, 0x01, // IN
	}
}

// udpAnswers reports whether a server actually replies over UDP.
//
// This cannot be done by dialling. UDP is connectionless, so net.Dial succeeds
// against a server that will never answer -- which is why racing a UDP dial
// against a TCP one would hand back a dead socket every time, instantly. The
// only way to learn that UDP works is to use it.
func udpAnswers(ctx context.Context, server netip.AddrPort) bool {
	conn, err := (&net.Dialer{}).DialContext(ctx, "udp", server.String())
	if err != nil {
		return false
	}
	defer conn.Close()
	deadline, ok := ctx.Deadline()
	if !ok {
		deadline = time.Now().Add(2 * time.Second)
	}
	if conn.SetDeadline(deadline) != nil {
		return false
	}
	query := udpProbe()
	if _, err := conn.Write(query); err != nil {
		return false
	}
	reply := make([]byte, 512)
	read, err := conn.Read(reply)
	// A header and a matching id is proof enough. Anything more would be reading
	// an answer this function has no use for.
	return err == nil && read >= 12 && reply[0] == query[0] && reply[1] == query[1]
}

// dialResolver ignores the address Go derived from configuration that does not
// exist on this platform and dials a resolver the operating system actually
// reported. Successive calls rotate, so Go's own retry reaches a different
// server rather than the same unreachable one.
func dialResolver(ctx context.Context, network, _ string) (net.Conn, error) {
	servers := configuredServers()
	if len(servers) == 0 {
		diagnose("resolver: no nameserver has been installed")
		return nil, errors.New("no resolver is configured")
	}
	// A plain dialer, deliberately. netns.NewDialer panics without a monitor, and
	// obtaining the live one would mean taking the lifecycle lock that a starting
	// node already holds. On this platform netns only applies the protect and
	// bind-to-network hooks, and neither is registered here.
	// Go's choice of transport is ignored, deliberately: it only retries over TCP
	// when a UDP answer comes back truncated, never when it times out, so letting
	// it pick means one dead transport burns the whole lookup budget.
	//
	// This used to mean always TCP, on the reasoning that a home router commonly
	// answers DNS over TCP while ignoring UDP from a client it did not hand the
	// lease to. That is true of some routers and false of others, and when it is
	// false the node cannot resolve its coordinator at all. Observed on a home
	// network in September 2026: the gateway answered UDP and timed out on TCP,
	// for both the IPv4 and IPv6 resolvers it advertised, so every lookup failed
	// while every other application on the network resolved normally.
	//
	// So neither transport is assumed now. Both are tried at once and whichever
	// proves itself first is used. Which one Go then frames the query for follows
	// from the connection it is handed: it picks by whether the conn implements
	// net.PacketConn.
	_ = network

	// Every server at once, first one to answer wins.
	//
	// These used to be tried in order with three seconds each, and a carrier
	// showed why that is not good enough: Safaricom reports two resolvers and
	// the FIRST one refuses DNS over TCP. Every lookup spent its budget dialling
	// a server that would never answer before reaching the one that would, so
	// the node could not resolve its coordinator at all on cellular while
	// working perfectly on wifi.
	//
	// The file already warned about this shape for UDP -- "a UDP attempt here
	// just burns the lookup's whole budget" -- and then reintroduced it by
	// walking a list. Racing them costs one extra connection to a server that is
	// answering anyway, and removes a whole class of ordering luck.
	attempt, cancel := context.WithTimeout(ctx, 5*time.Second)
	defer cancel()

	type dialed struct {
		conn net.Conn
		err  error
	}
	// Two attempts per server: a TCP dial, which proves itself by connecting, and a
	// UDP exchange, which has to prove itself by being answered.
	attempts := len(servers) * 2
	results := make(chan dialed, attempts)
	for _, server := range servers {
		go func(server netip.AddrPort) {
			dialer := net.Dialer{}
			conn, err := dialer.DialContext(attempt, "tcp", server.String())
			if err != nil {
				diagnose("resolver: dialing " + server.String() + " over tcp failed: " + err.Error())
			}
			results <- dialed{conn: conn, err: err}
		}(server)
		go func(server netip.AddrPort) {
			if !udpAnswers(attempt, server) {
				diagnose("resolver: " + server.String() + " did not answer over udp")
				results <- dialed{err: errors.New("no udp answer from " + server.String())}
				return
			}
			// A fresh socket, so the probe's reply cannot be sitting in the buffer
			// waiting to be mistaken for the answer to Go's own query.
			dialer := net.Dialer{}
			conn, err := dialer.DialContext(attempt, "udp", server.String())
			results <- dialed{conn: conn, err: err}
		}(server)
	}

	var last error
	for range make([]struct{}, attempts) {
		select {
		case result := <-results:
			if result.err == nil && result.conn != nil {
				// Cancelling closes the losers' dials; any that already
				// succeeded are closed by the drain below.
				remaining := attempts - 1
				go func() {
					for range make([]struct{}, remaining) {
						if late := <-results; late.conn != nil {
							late.conn.Close()
						}
					}
				}()
				return result.conn, nil
			}
			last = result.err
		case <-ctx.Done():
			return nil, ctx.Err()
		}
	}
	if last != nil {
		return nil, last
	}
	return nil, errors.New("no configured resolver could be dialled")
}

// configuredResolver resolves through the OS-reported servers. PreferGo is
// required: it is what routes lookups into the dialer above instead of the
// platform resolver that cannot answer for this node.
func configuredResolver() *net.Resolver {
	return &net.Resolver{PreferGo: true, Dial: dialResolver}
}
