// Command proxyfixture exercises native client transports against the production
// CONNECT proxy with a loopback TLS destination. It is not bundled with the app.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/netip"
	"os"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

func main() {
	if err := run(); err != nil {
		// Never log the proxy configuration, which contains a native credential.
		os.Stderr.WriteString("native proxy fixture failed\n")
		os.Exit(1)
	}
}

func run() error {
	if len(os.Args) != 2 {
		return errors.New("expected loopback upstream")
	}
	upstream, err := netip.ParseAddrPort(os.Args[1])
	if err != nil || upstream.Addr() != netip.MustParseAddr("127.0.0.1") || upstream.Port() == 0 {
		return errors.New("fixture upstream must be IPv4 loopback")
	}
	p, err := pondnet.NewProxy(func(ctx context.Context, network, _ string) (net.Conn, error) {
		return (&net.Dialer{}).DialContext(ctx, network, upstream.String())
	})
	if err != nil {
		return err
	}
	defer p.Close()
	output := json.NewEncoder(os.Stdout)
	if err := output.Encode(map[string]string{"address": p.Address(), "credential": p.Credential()}); err != nil {
		return err
	}
	input := bufio.NewScanner(os.Stdin)
	input.Buffer(make([]byte, 4096), 4096)
	for input.Scan() {
		var targets []string
		err := json.Unmarshal(input.Bytes(), &targets)
		if err == nil {
			err = p.SetTargets(targets)
		}
		if err := output.Encode(map[string]bool{"ok": err == nil}); err != nil {
			return err
		}
	}
	return input.Err()
}
