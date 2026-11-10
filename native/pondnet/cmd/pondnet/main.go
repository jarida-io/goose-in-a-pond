// The bundled Pond networking helper is a userspace tsnet node, not a system
// daemon. Stdin EOF and process signals terminate it with the owning Pond.
package main

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"io"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"tailscale.com/envknob"

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

// refuseCachedNetworkMaps makes this node start from the coordinator's live
// network map every time, never from one cached on disk.
//
// The Pond is the side that enforces access: its packet filter decides which
// phones may reach it. Starting from a cached map would enforce the rules as they
// were when the map was written, so a phone revoked while the Pond was down could
// pass until the coordinator answered. The coordinator grants the cache to phones
// only, and this holds even if a policy ever granted it to the Pond.
//
// tailscale reads the knob on every check rather than once, and with it off it
// neither loads nor writes the cache.
func refuseCachedNetworkMaps() { envknob.Setenv("TS_USE_CACHED_NETMAP", "false") }

func main() {
	notices := flag.Bool("third-party-notices", false, "print bundled dependency licenses and notices")
	state := flag.String("state", "", "private networking identity directory")
	host := flag.String("hostname", "goose-in-a-pond", "tailnet node name")
	control := flag.String("control", "", "required Headscale HTTPS origin")
	socket := flag.String("socket", "", "private companion Unix socket")
	identity := flag.String("identity", "", "existing Pond TLS identity file")
	port := flag.Int("port", 4443, "tailnet HTTPS port")
	authorityAction := flag.String("authority-action", "", "local authority operation: identity, register, inspect, enroll, replace or revoke")
	enrollmentOrigin := flag.String("enrollment", "", "enrollment HTTPS origin")
	deviceAction := flag.String("device-action", "", "provisioning operation on the device directory named by --state: create or install")
	flag.Parse()
	if *notices {
		fmt.Print(pondnet.ThirdPartyNotices)
		return
	}
	if *deviceAction != "" {
		os.Exit(runDeviceAction(*deviceAction, *state))
	}
	if *authorityAction != "" {
		if *authorityAction != "identity" {
			if _, err := os.Lstat(filepath.Join(*state, "identity.json")); err != nil {
				fmt.Fprintln(os.Stderr, "existing household authority is required")
				os.Exit(1)
			}
		}
		authority, err := pondnet.LoadAuthority(*state)
		if err != nil {
			fmt.Fprintln(os.Stderr, "household authority unavailable:", err)
			os.Exit(1)
		}
		if *authorityAction == "identity" {
			json.NewEncoder(os.Stdout).Encode(authority)
			return
		}
		if *authorityAction == "register" {
			// The invite arrives on stdin, never as an argument other accounts could read.
			var input struct {
				Invite string `json:"invite"`
			}
			decoder := json.NewDecoder(io.LimitReader(os.Stdin, 8192))
			decoder.DisallowUnknownFields()
			if err := decoder.Decode(&input); err != nil && err != io.EOF {
				fmt.Fprintln(os.Stderr, "invalid registration input")
				os.Exit(2)
			}
			device, err := registrationDevice(pondnet.DeviceDirectory(*state), input.Invite, os.Stderr)
			if err != nil {
				fmt.Fprintln(os.Stderr, "device certificate unavailable:", err)
				os.Exit(1)
			}
			household, err := authority.Register(context.Background(), *enrollmentOrigin, uint16(*port), input.Invite, device)
			if err != nil {
				fmt.Fprintln(os.Stderr, "household registration incomplete:", err)
				// A refusal, like one from Submit, is a decision the Pond must report as one.
				var refused *pondnet.Refused
				if errors.As(err, &refused) {
					json.NewEncoder(os.Stdout).Encode(map[string]string{"refused": refused.Reason})
					os.Exit(3)
				}
				os.Exit(1)
			}
			json.NewEncoder(os.Stdout).Encode(map[string]string{"household": household})
			return
		}
		if *authorityAction != "enroll" && *authorityAction != "revoke" && *authorityAction != "inspect" && *authorityAction != "replace" {
			os.Exit(2)
		}
		var approval enrollment.Approval
		decoder := json.NewDecoder(io.LimitReader(os.Stdin, 8192))
		decoder.DisallowUnknownFields()
		if decoder.Decode(&approval) != nil || decoder.Decode(new(any)) != io.EOF {
			os.Exit(2)
		}
		approval.Action = *authorityAction
		result, err := authority.Submit(context.Background(), *enrollmentOrigin, approval)
		if err != nil {
			fmt.Fprintln(os.Stderr, "household enrollment incomplete:", err)
			// Exit 3 for a decision, 1 for a fault. A coordinator that refused
			// this request understood it perfectly; the pond must not report
			// that to a user as "remote access is not set up", which is what
			// one exit code for both outcomes forced it to do.
			var refused *pondnet.Refused
			if errors.As(err, &refused) {
				// The coordinator's own identifier, so the pond can act on which refusal
				// it was: enrollment_missing means there is nothing to revoke.
				json.NewEncoder(os.Stdout).Encode(map[string]string{"refused": refused.Reason})
				os.Exit(3)
			}
			os.Exit(1)
		}
		json.NewEncoder(os.Stdout).Encode(result)
		return
	}
	if *socket == "" || *identity == "" {
		fmt.Fprintln(os.Stderr, "embedded networking configuration is incomplete")
		os.Exit(2)
	}
	refuseCachedNetworkMaps()
	n, err := pondnet.Open(*state, *host, *control)
	if err != nil {
		fmt.Fprintln(os.Stderr, "embedded networking could not start:", err)
		os.Exit(1)
	}
	defer n.Close()
	server, finished, err := pondnet.ServePond(n, *port, *socket, *identity)
	if err != nil {
		fmt.Fprintln(os.Stderr, "embedded companion listener could not start:", err)
		n.Close()
		os.Exit(1)
	}
	defer server.Close()
	done := make(chan struct{})
	go func() {
		scanner := bufio.NewScanner(os.Stdin)
		for scanner.Scan() {
		}
		close(done)
	}()
	signals := make(chan os.Signal, 1)
	signal.Notify(signals, syscall.SIGTERM, os.Interrupt)
	defer signal.Stop(signals)
	ticker := time.NewTicker(2 * time.Second)
	defer ticker.Stop()
	encoder := json.NewEncoder(os.Stdout)
	for {
		status, err := n.Snapshot()
		if err != nil {
			status = pondnet.Status{State: "Unavailable", Addresses: []string{}}
		}
		if encoder.Encode(report{Status: status, ClockSynchronized: clockSynchronized()}) != nil {
			return
		}
		select {
		case <-finished:
			fmt.Fprintln(os.Stderr, "embedded companion listener stopped unexpectedly")
			n.Close()
			os.Exit(1)
		case <-done:
			return
		case <-signals:
			return
		case <-ticker.C:
		}
	}
}

// report is one status line to the Pond: the node's own status and what this host can add to it.
type report struct {
	pondnet.Status
	// ClockSynchronized says whether this host's clock is disciplined by a time source, so the
	// Pond can tell a node that is down because the clock is wrong (TLS to the coordinator fails
	// while it is far enough off) from one that is down for any other reason. Absent where the
	// host cannot say.
	ClockSynchronized *bool `json:"clockSynchronized,omitempty"`
}

// registrationDevice is the device certificate to register with. One that exists but cannot
// be loaded stops registration without an invite, rather than registering without the
// certificate the Pond was imaged with; with an invite, the invite admits the household.
func registrationDevice(directory, invite string, warnings io.Writer) (*pondnet.Device, error) {
	device, err := pondnet.LoadDevice(directory)
	if err != nil && invite != "" {
		fmt.Fprintln(warnings, "device certificate unavailable, registering with the invite:", err)
		return nil, nil
	}
	return device, err
}

// runDeviceAction provisions a Pond while it is imaged: create prints the new device key's
// public half for the operator to sign, and install reads the signed certificate on stdin.
func runDeviceAction(action, directory string) int {
	switch action {
	case "create":
		public, err := pondnet.CreateDevice(directory)
		if err != nil {
			fmt.Fprintln(os.Stderr, "device key not created:", err)
			return 1
		}
		json.NewEncoder(os.Stdout).Encode(map[string]string{"devicePublicKey": public})
		return 0
	case "install":
		var certificate enrollment.Envelope
		decoder := json.NewDecoder(io.LimitReader(os.Stdin, 8192))
		decoder.DisallowUnknownFields()
		if decoder.Decode(&certificate) != nil || decoder.Decode(new(any)) != io.EOF {
			fmt.Fprintln(os.Stderr, "invalid device certificate input")
			return 2
		}
		if err := pondnet.InstallDeviceCertificate(directory, certificate); err != nil {
			fmt.Fprintln(os.Stderr, "device certificate not installed:", err)
			return 1
		}
		read, _ := enrollment.ReadDeviceCertificate(certificate)
		json.NewEncoder(os.Stdout).Encode(map[string]string{"serial": read.Serial})
		return 0
	}
	fmt.Fprintln(os.Stderr, "unknown device action:", action)
	return 2
}
