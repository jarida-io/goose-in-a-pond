package mobile

import (
	"net"

	"tailscale.com/net/dnscache"
)

// The backend resolves through two independent paths, so both are pointed at the
// resolvers the operating system reported. dnscache's forwarder is the one that
// matters for reaching the coordinator: the control client, the control HTTP
// dialer and the DERP client all resolve through it, and because its Forward is
// non-nil it never consults net.DefaultResolver. Netcheck uses the default
// resolver directly, so that is set too.
func init() {
	resolver := configuredResolver()
	dnscache.Get().Forward = resolver
	net.DefaultResolver = resolver
}
