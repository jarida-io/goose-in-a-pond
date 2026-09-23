package enrollment

import (
	"context"
	"crypto/ed25519"
	"strings"
	"testing"
	"time"
)

func TestInspectEnrollmentRequiresAuthorityAndConsumesApproval(t *testing.T) {
	s, key, backend := fixture(t)
	a := approval()
	if invoke(s, a, key) != 200 {
		t.Fatal("initial enrollment failed")
	}
	a.Action, a.Nonce = "inspect", "inspect000000001"
	if code := invoke(s, a, key); code != 200 {
		t.Fatalf("signed inspection HTTP %d", code)
	}
	if code := invoke(s, a, key); code != 409 {
		t.Fatalf("inspection replay HTTP %d", code)
	}
	if backend.calls != 1 {
		t.Fatal("inspection registered a node")
	}
}

// Replacement is an explicit authority action, never a retry of enrollment.
func replacementFixture(t *testing.T, status string) (*Service, ed25519.PrivateKey, *fakeBackend, Approval) {
	t.Helper()
	s, key, backend := fixture(t)
	a := approval()
	devices := s.Store.value.Devices[a.Household]
	devices["pond000000000001"] = Device{Role: "pond", Status: "active", Key: "nodekey:" + strings.Repeat("c", 64), MachineKey: "mkey:" + strings.Repeat("d", 64), NodeID: "2", Address: "100.64.0.2", Revision: "pondrevision00001"}
	backend.nodes = append(backend.nodes, Registered{ID: "2", Key: devices["pond000000000001"].Key, MachineKey: devices["pond000000000001"].MachineKey, Addresses: []string{"100.64.0.2"}, UserID: "1"})
	devices[a.Device] = Device{Role: "phone", Status: status, MachineKey: "mkey:" + strings.Repeat("e", 64), Revision: "oldrevision00001"}
	if err := s.Store.save(); err != nil {
		t.Fatal(err)
	}
	a.Action, a.Role, a.ExpectedRevision = "replace", "phone", "oldrevision00001"
	return s, key, backend, a
}

func TestReplacementPersistsRetiredIdentityAndRejectsStaleApproval(t *testing.T) {
	for _, status := range []string{"revoked", "failed", "pending"} {
		t.Run(status, func(t *testing.T) {
			s, key, backend, a := replacementFixture(t, status)
			old := s.Store.value.Devices[a.Household][a.Device]
			if code := invoke(s, a, key); code != 200 {
				t.Fatalf("replacement HTTP %d", code)
			}
			current := s.Store.value.Devices[a.Household][a.Device]
			if current.Status != "active" || current.Revision == old.Revision || current.Revision == "" || len(backend.rules) != 1 {
				t.Fatal("replacement not activated with fresh revision")
			}
			retired := s.Store.value.Retired[a.Household]
			if len(retired) != 1 || retired[0].MachineKey != old.MachineKey {
				t.Fatal("old identity lost")
			}
			a.Nonce = "staleapproval00001"
			if invoke(s, a, key) != 409 || backend.calls != 1 {
				t.Fatal("stale replacement registered twice")
			}
			dir := s.Store.directory
			s.Store.Close()
			restored, err := Open(dir)
			if err != nil {
				t.Fatal(err)
			}
			defer restored.Close()
			service, err := New(context.Background(), restored, backend)
			if err != nil {
				t.Fatal(err)
			}
			// A registration that completes after replacement must still be removed.
			backend.nodes = append(backend.nodes, Registered{ID: "3", UserID: "1", MachineKey: old.MachineKey, Key: "nodekey:" + strings.Repeat("f", 64), Addresses: []string{"100.64.0.3"}})
			if err := service.Reconcile(context.Background()); err != nil {
				t.Fatal(err)
			}
			for _, n := range backend.nodes {
				if n.MachineKey == old.MachineKey {
					t.Fatal("retired identity survived reconciliation")
				}
			}
			a.Device, a.Nonce, a.Action, a.MachineKey = "anotherdevice0001", "retiredreuse00001", "enroll", old.MachineKey
			if invoke(service, a, key) != 409 {
				t.Fatal("retired machine was reused")
			}
		})
	}
}

func TestReplacementPreconditionsDenyWithoutRegistration(t *testing.T) {
	for _, kind := range []string{"active", "revoking", "no_revision", "stale_revision", "same_machine", "wrong_role", "wrong_household", "expired", "retired_limit", "ordinary_enroll"} {
		t.Run(kind, func(t *testing.T) {
			s, key, backend, a := replacementFixture(t, "revoked")
			switch kind {
			case "active", "revoking":
				d := s.Store.value.Devices[a.Household][a.Device]
				d.Status = kind
				s.Store.value.Devices[a.Household][a.Device] = d
			case "no_revision":
				a.ExpectedRevision = ""
			case "stale_revision":
				a.ExpectedRevision = "staleversion00001"
			case "same_machine":
				a.MachineKey = s.Store.value.Devices[a.Household][a.Device].MachineKey
			case "wrong_role":
				a.Role = "pond"
			case "wrong_household":
				a.Household = "household00000002"
			case "expired":
				a.Expires = time.Now().Add(-time.Second).Unix()
			case "retired_limit":
				s.Store.value.Retired[a.Household] = make([]Device, 256)
			case "ordinary_enroll":
				a.Action = "enroll"
			}
			if invoke(s, a, key) < 400 || backend.calls != 0 {
				t.Fatal("unsafe replacement accepted")
			}
		})
	}
}

func TestRevocationInvalidatesPreviouslyApprovedReplacement(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	revoke := a
	revoke.Action = "revoke"
	revoke.Nonce = "newrevocation001"
	if invoke(s, revoke, key) != 200 {
		t.Fatal("revocation failed")
	}
	if invoke(s, a, key) != 409 || backend.calls != 0 {
		t.Fatal("older approval undid newer revocation")
	}
}

func TestAmbiguousReplacementRecoversWithoutReplayingMutation(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	backend.lostResponse = true
	if invoke(s, a, key) != 503 {
		t.Fatal("lost response hidden")
	}
	pending := s.Store.value.Devices[a.Household][a.Device]
	if pending.Status != "pending" {
		t.Fatal("lost replacement intent")
	}
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if s.Store.value.Devices[a.Household][a.Device].Revision != pending.Revision || backend.calls != 1 || s.Store.value.Devices[a.Household][a.Device].Status != "active" {
		t.Fatal("replacement recovery failed")
	}
}

func TestReplacementCannotRecoverAfterRevocation(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	backend.lostResponse = true
	if invoke(s, a, key) != 503 {
		t.Fatal("expected lost response")
	}
	revoke := a
	revoke.Action = "revoke"
	revoke.Nonce = "replacementrevoke1"
	if invoke(s, revoke, key) != 200 {
		t.Fatal("revoke failed")
	}
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if len(backend.rules) != 0 || s.Store.value.Devices[a.Household][a.Device].Status != "revoked" {
		t.Fatal("revoked replacement recovered")
	}
}

func TestInspectionMigratesLegacyRevisionWithoutNetworkMutation(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "pending")
	old := s.Store.value.Devices[a.Household][a.Device]
	old.Revision = ""
	old.MachineKey = ""
	s.Store.value.Devices[a.Household][a.Device] = old
	a.Action = "inspect"
	if invoke(s, a, key) != 200 {
		t.Fatal("inspection failed")
	}
	revision := s.Store.value.Devices[a.Household][a.Device].Revision
	if revision == "" || backend.calls != 0 {
		t.Fatal("legacy inspection did not generate durable revision")
	}
	a.Action = "replace"
	a.ExpectedRevision = revision
	a.Nonce = "legacyreplace001"
	if invoke(s, a, key) != 200 {
		t.Fatal("explicit legacy recovery failed")
	}
}

func TestConcurrentReplacementConsumesOneRevision(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	codes := make(chan int, 2)
	for _, nonce := range []string{"concurrent000001", "concurrent000002"} {
		next := a
		next.Nonce = nonce
		go func() { codes <- invoke(s, next, key) }()
	}
	first, second := <-codes, <-codes
	if !((first == 200 && second == 409) || (first == 409 && second == 200)) || backend.calls != 1 {
		t.Fatal("revision was used more than once")
	}
}

func TestRetiredCleanupCannotDeleteAnotherHouseholdMachine(t *testing.T) {
	s, key, backend, a := replacementFixture(t, "revoked")
	old := s.Store.value.Devices[a.Household][a.Device]
	if invoke(s, a, key) != 200 {
		t.Fatal("replacement failed")
	}
	backend.nodes = append(backend.nodes, Registered{ID: "3", UserID: "2", MachineKey: old.MachineKey, Key: "nodekey:" + strings.Repeat("f", 64), Addresses: []string{"100.64.0.3"}})
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	if backend.deleted != 0 {
		t.Fatal("retirement deleted another household node")
	}
}

func TestRetiredStateValidation(t *testing.T) {
	for _, kind := range []string{"active", "duplicate_machine", "unknown_household", "invalid_revision", "too_many"} {
		t.Run(kind, func(t *testing.T) {
			s, key, _, a := replacementFixture(t, "revoked")
			if invoke(s, a, key) != 200 {
				t.Fatal("replacement failed")
			}
			switch kind {
			case "active":
				s.Store.value.Retired[a.Household][0].Status = "active"
			case "duplicate_machine":
				s.Store.value.Retired[a.Household][0].MachineKey = a.MachineKey
			case "unknown_household":
				s.Store.value.Retired["household00000002"] = s.Store.value.Retired[a.Household]
			case "invalid_revision":
				s.Store.value.Retired[a.Household][0].Revision = "bad"
			case "too_many":
				s.Store.value.Retired[a.Household] = make([]Device, 257)
			}
			if validateState(s.Store.value) == nil {
				t.Fatal("invalid retirement state accepted")
			}
		})
	}
}
