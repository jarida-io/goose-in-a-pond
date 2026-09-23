// pond-enrollment serves signed pilot registrations; provision is an offline operator command.
package main

import (
	"context"
	"errors"
	"flag"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"log/slog"
	"net/http"
	"os"
	"os/signal"
	"syscall"
	"time"
)

func main() {
	if err := run(); err != nil {
		slog.Error("enrollment service failed", "error", err)
		os.Exit(1)
	}
}
func run() error {
	directory := flag.String("state", "", "private state directory")
	listen := flag.String("listen", "127.0.0.1:8081", "private HTTP listener behind HTTPS proxy")
	origin := flag.String("headscale", "", "private Headscale origin")
	secret := flag.String("credential-file", "", "Headscale admin credential file")
	provision := flag.String("provision", "", "operator-provisioned household identifier")
	public := flag.String("public-key", "", "household Ed25519 public key in base64")
	user := flag.String("user-id", "", "operator-created Headscale user ID")
	port := flag.Uint("https-port", 4443, "household companion port")
	health := flag.Bool("health-check", false, "check the local enrollment listener")
	flag.Parse()
	if *health {
		client := http.Client{Timeout: 2 * time.Second}
		r, e := client.Get("http://127.0.0.1:8081/health")
		if e != nil {
			return e
		}
		r.Body.Close()
		if r.StatusCode != 200 {
			return errors.New("enrollment unhealthy")
		}
		return nil
	}
	store, err := enrollment.Open(*directory)
	if err != nil {
		return err
	}
	defer store.Close()
	if *provision != "" {
		if *port == 0 || *port > 65535 {
			return &configError{}
		}
		return store.Provision(*provision, enrollment.Household{PublicKey: *public, UserID: *user, Port: uint16(*port)})
	}
	credential, err := os.ReadFile(*secret)
	if err != nil {
		return err
	}
	backend, err := enrollment.NewHeadscale(*origin, string(credential))
	if err != nil {
		return err
	}
	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	handler, err := enrollment.New(ctx, store, backend)
	if err != nil {
		return err
	}
	go func() {
		ticker := time.NewTicker(30 * time.Second)
		defer ticker.Stop()
		for {
			select {
			case <-ctx.Done():
				return
			case <-ticker.C:
				job, stop := context.WithTimeout(ctx, 15*time.Second)
				if handler.Reconcile(job) != nil {
					slog.Warn("network enrollment reconciliation deferred")
				}
				stop()
			}
		}
	}()
	server := &http.Server{Addr: *listen, Handler: handler, ReadHeaderTimeout: 5 * time.Second, ReadTimeout: 10 * time.Second, WriteTimeout: 20 * time.Second, IdleTimeout: 30 * time.Second, MaxHeaderBytes: 8192}
	go func() {
		<-ctx.Done()
		shutdown, stop := context.WithTimeout(context.Background(), 5*time.Second)
		defer stop()
		server.Shutdown(shutdown)
	}()
	slog.Info("enrollment service ready")
	err = server.ListenAndServe()
	if err == http.ErrServerClosed {
		return nil
	}
	return err
}

type configError struct{}

func (*configError) Error() string { return "invalid companion port" }
