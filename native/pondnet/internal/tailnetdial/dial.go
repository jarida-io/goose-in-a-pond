// Package tailnetdial isolates the pinned tsnet subsystem API used to guarantee
// that companion traffic never falls back to operating-system routes or VPNs.
package tailnetdial

import (
	"context"
	"errors"
	"net"
	"net/netip"
	"time"

	"tailscale.com/tsnet"
)

// TCP opens a connection exclusively through the node's userspace WireGuard
// stack. The caller supplies a validated tailnet destination from its allowlist.
func TCP(ctx context.Context, server *tsnet.Server, peer netip.AddrPort) (net.Conn, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	client, err := server.LocalClient()
	if err != nil {
		return nil, err
	}
	statusContext, cancel := context.WithTimeout(ctx, 3*time.Second)
	status, err := client.StatusWithoutPeers(statusContext)
	cancel()
	if err != nil || status.BackendState != "Running" {
		return nil, errors.New("embedded node is not running")
	}
	system := server.Sys()
	if system == nil {
		return nil, errors.New("embedded networking is unavailable")
	}
	dialer, ok := system.Dialer.GetOK()
	if !ok || dialer.NetstackDialTCP == nil {
		return nil, errors.New("embedded TCP transport is unavailable")
	}
	return dialer.NetstackDialTCP(ctx, peer)
}
