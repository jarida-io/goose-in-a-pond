package enrollment

import (
	"context"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"testing"
)

func TestHeadscaleDistinguishesRejectedAndAmbiguousRegistration(t *testing.T) {
	for _, outcome := range []struct {
		name                 string
		lookup, registration int
		rejected             bool
	}{
		{"invalid_pending_id", 200, 400, true},
		{"conflict", 200, 409, true},
		{"rate_limit", 200, 429, true},
		{"lookup_unavailable_before_mutation", 503, 0, true},
		{"server_failure_after_mutation_may_be_ambiguous", 200, 503, false},
	} {
		t.Run(outcome.name, func(t *testing.T) {
			writes := 0
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/api/v1/user" {
					w.WriteHeader(outcome.lookup)
					io.WriteString(w, `{"users":[{"id":"1","name":"fixture"}]}`)
					return
				}
				if r.Method != "POST" || r.URL.Path != "/api/v1/auth/register" {
					t.Error("unexpected coordinator request")
				}
				writes++
				w.WriteHeader(outcome.registration)
			}))
			defer server.Close()
			backend, err := NewHeadscale(server.URL, "fixture-credential")
			if err != nil {
				t.Fatal(err)
			}
			_, err = backend.Register(context.Background(), "1", "fixture-pending-auth")
			if err == nil || errors.Is(err, ErrRegistrationRejected) != outcome.rejected {
				t.Fatalf("incorrect recovery classification: %v", err)
			}
			if (outcome.lookup != 200 && writes != 0) || writes > 1 {
				t.Fatal("registration mutation retried or sent after failed lookup")
			}
		})
	}
}
