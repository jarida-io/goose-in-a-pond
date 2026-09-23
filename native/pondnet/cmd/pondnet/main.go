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

	"github.com/Exile10/goose-in-a-pond/native/pondnet"
)

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
	flag.Parse()
	if *notices {
		fmt.Print(pondnet.ThirdPartyNotices)
		return
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
			household, err := authority.Register(context.Background(), *enrollmentOrigin, uint16(*port))
			if err != nil {
				fmt.Fprintln(os.Stderr, "household registration incomplete:", err)
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
		if encoder.Encode(status) != nil {
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
