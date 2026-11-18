package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/json"
	"net/http/httptest"
	"os"
	"path/filepath"
	"slices"
	"testing"
	"time"
)

const (
	bootA = "0b5e1c2a-1111-4222-8333-944455556666"
	bootB = "7f00aa10-2222-4333-8444-a55566667777"
)

var minute = int64(60)

func heartbeat(run string, sequence int64, boot string) PondReport {
	return PondReport{Kind: KindHeartbeat, Run: run, Sequence: sequence, BootID: boot, UptimeSeconds: 3600}
}

func booted(run, previous, boot string, uptime int64) PondReport {
	return PondReport{Kind: KindBooted, PreviousShutdown: previous, Run: run, Sequence: 1, BootID: boot, UptimeSeconds: uptime}
}

// The 2026-11-12 cut, as this service would have seen it: heartbeats, ninety minutes of
// silence with no announcement, then a Pond on a new boot whose last run never stopped.
// The host was up eight minutes before its report arrived, so the outage ends there.
func TestAPowerCutIsTheSilenceUpToTheHostsReturn(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	during := derive(record, true, nil, 1000+20*minute)
	if during.State != StateUnreachable || during.Since != 1000 || !slices.Equal(during.PossibleCauses, []string{CausePowerLoss, CauseNetworkLoss}) {
		t.Fatalf("during the cut: %+v", during)
	}
	arrived := int64(1000 + 98*minute)
	record = observe(record, true, booted("run-bbbbbbbbbbbbbbbb", "unclean", bootB, 8*minute), arrived, nil)
	outage := record.LastOutage
	if outage == nil || outage.Cause != CausePowerLoss || outage.StartedAt != 1000 || outage.EndedAt != arrived-8*minute {
		t.Fatalf("outage: %+v", outage)
	}
	after := derive(record, true, nil, arrived+minute)
	if after.State != StateBack || after.LastOutage != outage {
		t.Fatalf("after: %+v", after)
	}
	for sequence := int64(2); sequence <= 13; sequence++ {
		record = observe(record, true, heartbeat("run-bbbbbbbbbbbbbbbb", sequence, bootB), arrived+(sequence-1)*5*minute, nil)
	}
	if later := derive(record, true, nil, arrived+61*minute); later.State != StateOnline || later.LastOutage == nil {
		t.Fatalf("an hour on it is simply online, still naming the outage: %+v", later)
	}
}

func TestAnAnnouncedRestartIsPlannedWithItsReason(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	record = observe(record, true, PondReport{Kind: KindGoingDown, Reason: "update", Run: "run-aaaaaaaaaaaaaaaa", Sequence: 2, BootID: bootA}, 1100, nil)
	for _, at := range []int64{1101, 1100 + 20*minute} {
		answer := derive(record, true, nil, at)
		if answer.State != StatePlannedRestart || answer.Reason != "update" || answer.Since != 1100 {
			t.Fatalf("at %d: %+v", at, answer)
		}
	}
	if stuck := derive(record, true, nil, 1100+45*minute); stuck.State != StateUnreachable || stuck.Since != 1100 {
		t.Fatalf("an update that never came back is no longer believed: %+v", stuck)
	}
	record = observe(record, true, booted("run-bbbbbbbbbbbbbbbb", "clean", bootA, 2*minute), 1400, nil)
	if o := record.LastOutage; o == nil || o.Cause != CausePlanned || o.Reason != "update" || o.StartedAt != 1100 || o.EndedAt != 1400 {
		t.Fatalf("outage: %+v", o)
	}
}

func TestAShutdownIsBelievedForAsLongAsItLasts(t *testing.T) {
	record := observe(pondRecord{}, false, PondReport{Kind: KindGoingDown, Reason: "shutdown", Run: "run-aaaaaaaaaaaaaaaa", Sequence: 1}, 1000, nil)
	if answer := derive(record, true, nil, 1000+48*3600); answer.State != StatePlannedRestart || answer.Reason != "shutdown" {
		t.Fatalf("%+v", answer)
	}
}

// The home's internet went away and came back while the Pond ran throughout: the same
// run on the same boot resumes counting after a long gap.
func TestHeartbeatsResumingOnTheSameRunAreANetworkLoss(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	record = observe(record, true, heartbeat("run-aaaaaaaaaaaaaaaa", 2, bootA), 1000+5*minute, nil)
	if record.LastOutage != nil {
		t.Fatalf("an on-time heartbeat is not an outage: %+v", record.LastOutage)
	}
	record = observe(record, true, heartbeat("run-aaaaaaaaaaaaaaaa", 3, bootA), 1000+70*minute, nil)
	if o := record.LastOutage; o == nil || o.Cause != CauseNetworkLoss || o.StartedAt != 1000+5*minute {
		t.Fatalf("outage: %+v", o)
	}
	// A heartbeat from a new run without a booted report first cannot be explained.
	record = observe(record, true, heartbeat("run-cccccccccccccccc", 1, bootB), 1000+200*minute, nil)
	if o := record.LastOutage; o.Cause != CauseUnknown {
		t.Fatalf("outage: %+v", o)
	}
}

func TestAnUncleanStopOnTheSameBootIsNotCalledAPowerCut(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	record = observe(record, true, booted("run-bbbbbbbbbbbbbbbb", "unclean", bootA, 3600), 1300, nil)
	if o := record.LastOutage; o == nil || o.Cause != CauseUnknown || o.EndedAt != 1300 {
		t.Fatalf("a crash is not a power cut: %+v", o)
	}
	record = observe(record, true, booted("run-cccccccccccccccc", "clean", bootB, 60), 2000, nil)
	if o := record.LastOutage; o.Cause != CausePlanned || o.Reason != "" {
		t.Fatalf("a clean stop whose announcement was lost is still planned: %+v", o)
	}
}

func TestAConnectedNodeMeansThePondItselfStopped(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	answer := derive(record, true, &nodePresence{Online: true}, 1000+30*minute)
	if answer.State != StateUnreachable || !slices.Equal(answer.PossibleCauses, []string{"pond_stopped"}) || answer.NodeOnline == nil || !*answer.NodeOnline {
		t.Fatalf("%+v", answer)
	}
	if never := derive(pondRecord{}, false, nil, 5); never.State != StateUnknown || never.LastSeen != 0 {
		t.Fatalf("a Pond that never reported: %+v", never)
	}
}

func TestReportShapesAreBounded(t *testing.T) {
	now := int64(1_000_000)
	valid := PondReport{Household: "household00000001", Kind: KindHeartbeat, Run: "run-aaaaaaaaaaaaaaaa", Sequence: 1, UptimeSeconds: 10, Expires: now + 60}
	if !validReport(valid, now) {
		t.Fatal("a plain heartbeat was refused")
	}
	for name, mutate := range map[string]func(*PondReport){
		"expired":           func(r *PondReport) { r.Expires = now },
		"far future":        func(r *PondReport) { r.Expires = now + 3600 },
		"no run":            func(r *PondReport) { r.Run = "x" },
		"sequence zero":     func(r *PondReport) { r.Sequence = 0 },
		"bad boot id":       func(r *PondReport) { r.BootID = "not-a-boot-id" },
		"unknown kind":      func(r *PondReport) { r.Kind = "rebooting" },
		"heartbeat reason":  func(r *PondReport) { r.Reason = "restart" },
		"going down reason": func(r *PondReport) { r.Kind, r.Reason = KindGoingDown, "nap" },
		"booted no shutdown": func(r *PondReport) {
			r.Kind = KindBooted
		},
		"node state": func(r *PondReport) { r.Network = &NetworkFacts{NodeState: "Running; rm -rf"} },
		"process outlives the host": func(r *PondReport) {
			r.Network = &NetworkFacts{ProcessSeconds: 10_000}
		},
	} {
		r := valid
		mutate(&r)
		if validReport(r, now) {
			t.Errorf("%s was accepted", name)
		}
	}
}

func postStatus(s *Service, report PondReport, key ed25519.PrivateKey) (int, PondStatus) {
	envelope, _ := SignStatus(report, key)
	data, _ := json.Marshal(envelope)
	w := httptest.NewRecorder()
	s.ServeHTTP(w, httptest.NewRequest("POST", "/v1/pond/status", bytes.NewReader(data)))
	var answer PondStatus
	_ = json.Unmarshal(w.Body.Bytes(), &answer)
	return w.Code, answer
}

func readPondStatus(s *Service, authorization string) (int, PondStatus) {
	r := httptest.NewRequest("GET", "/v1/pond/status", nil)
	if authorization != "" {
		r.Header.Set("Authorization", authorization)
	}
	w := httptest.NewRecorder()
	s.ServeHTTP(w, r)
	var answer PondStatus
	_ = json.Unmarshal(w.Body.Bytes(), &answer)
	return w.Code, answer
}

func signedReport(kind string, sequence int64) PondReport {
	r := PondReport{Household: "household00000001", Kind: kind, Run: "run-aaaaaaaaaaaaaaaa", Sequence: sequence, BootID: bootA, UptimeSeconds: 100, Expires: time.Now().Add(time.Minute).Unix()}
	if kind == KindBooted {
		r.PreviousShutdown = "clean"
	}
	return r
}

func TestAPondReportsOnlyWithItsHouseholdKeyAndInOrder(t *testing.T) {
	s, key, _ := fixture(t)
	_, stranger, _ := ed25519.GenerateKey(rand.Reader)
	if code, _ := postStatus(s, signedReport(KindBooted, 1), stranger); code != 403 {
		t.Fatalf("another key: %d", code)
	}
	other := signedReport(KindBooted, 1)
	other.Household = "household00000002"
	if code, _ := postStatus(s, other, key); code != 403 {
		t.Fatalf("an unknown household: %d", code)
	}
	code, answer := postStatus(s, signedReport(KindBooted, 1), key)
	if code != 200 || answer.State != StateOnline || answer.LastSeen == 0 {
		t.Fatalf("booted: %d %+v", code, answer)
	}
	if code, _ := postStatus(s, signedReport(KindBooted, 1), key); code != 409 {
		t.Fatalf("a replayed report: %d", code)
	}
	if code, _ := postStatus(s, signedReport(KindHeartbeat, 2), key); code != 200 {
		t.Fatalf("the next report: %d", code)
	}
	// The record survives the service restarting.
	reopened := openStatus(s.Store.directory)
	if record, ok := reopened.records["household00000001"]; !ok || record.Sequence != 2 {
		t.Fatalf("not persisted: %+v", reopened.records)
	}
}

func TestAnUnreadableStatusFileIsSetAsideNotFatal(t *testing.T) {
	dir := t.TempDir()
	if err := os.WriteFile(filepath.Join(dir, "status.json"), []byte("{not json"), 0600); err != nil {
		t.Fatal(err)
	}
	store := openStatus(dir)
	if len(store.records) != 0 {
		t.Fatal("garbage was read as records")
	}
	if _, err := os.Stat(filepath.Join(dir, "status.json.unreadable")); err != nil {
		t.Fatalf("not set aside: %v", err)
	}
}

func TestOnlyAnActivePhoneWithAHouseholdGrantReads(t *testing.T) {
	s, key, _ := fixture(t)
	phone := "phone00000000001"
	s.Store.mu.Lock()
	s.Store.value.Devices["household00000001"][phone] = Device{Role: "phone", Status: "active"}
	s.Store.value.Devices["household00000001"]["pond0000000000001"] = Device{Role: "pond", Status: "active"}
	s.Store.mu.Unlock()
	grant := func(device string, lifetime time.Duration, signer ed25519.PrivateKey) string {
		token, err := SignGrant(StatusGrant{Household: "household00000001", Device: device, Expires: time.Now().Add(lifetime).Unix()}, signer)
		if err != nil {
			t.Fatal(err)
		}
		return "PondGrant " + token
	}
	if code, _ := readPondStatus(s, ""); code != 401 {
		t.Fatalf("no grant: %d", code)
	}
	_, stranger, _ := ed25519.GenerateKey(rand.Reader)
	if code, _ := readPondStatus(s, grant(phone, time.Hour, stranger)); code != 401 {
		t.Fatalf("a grant another key signed: %d", code)
	}
	if code, _ := readPondStatus(s, grant(phone, -time.Minute, key)); code != 401 {
		t.Fatalf("an expired grant: %d", code)
	}
	if code, _ := readPondStatus(s, grant(phone, 60*24*time.Hour, key)); code != 401 {
		t.Fatalf("a grant past the lifetime bound: %d", code)
	}
	if code, _ := readPondStatus(s, grant("pond0000000000001", time.Hour, key)); code != 403 {
		t.Fatalf("the Pond's own device is not a phone: %d", code)
	}
	if code, _ := readPondStatus(s, grant("phone00000000002", time.Hour, key)); code != 403 {
		t.Fatalf("a phone never enrolled: %d", code)
	}
	code, answer := readPondStatus(s, grant(phone, 30*24*time.Hour, key))
	if code != 200 || answer.State != StateUnknown || answer.Now == 0 {
		t.Fatalf("before any report: %d %+v", code, answer)
	}
	postStatus(s, signedReport(KindBooted, 1), key)
	if code, answer := readPondStatus(s, grant(phone, time.Hour, key)); code != 200 || answer.State != StateOnline {
		t.Fatalf("after a report: %d %+v", code, answer)
	}
	// Revoking the phone ends its grant at once, however long the grant had left.
	s.Store.mu.Lock()
	s.Store.value.Devices["household00000001"][phone] = Device{Role: "phone", Status: "revoked"}
	s.Store.mu.Unlock()
	if code, _ := readPondStatus(s, grant(phone, time.Hour, key)); code != 403 {
		t.Fatalf("a revoked phone: %d", code)
	}
}

func TestTheInventoryTellsWhetherThePondNodeIsConnected(t *testing.T) {
	s, key, backend := fixture(t)
	if code := invoke(s, approval(), key); code != 200 {
		t.Fatalf("pond enrollment: %d", code)
	}
	backend.nodes[0].Online = true
	if err := s.Reconcile(context.Background()); err != nil {
		t.Fatal(err)
	}
	_, answer := postStatus(s, signedReport(KindBooted, 1), key)
	if answer.NodeOnline == nil || !*answer.NodeOnline {
		t.Fatalf("%+v", answer)
	}
}

// The 2026-11-18 cuts on the Jetson lasted one to three minutes: the Pond retried its next
// heartbeat within a minute of the connection coming back, so it never missed two, and the
// phone was told nothing while the Pond was cut off or after.
func TestAShortNetworkCutIsSeenFromTheNodeDropping(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	node := presenceAt(nodePresence{Online: true}, true, 1000+minute)
	node = presenceAt(node, false, 1000+2*minute)
	during := derive(record, true, &node, 1000+3*minute)
	if during.State != StateUnreachable || during.Since != 1000 || !slices.Equal(during.PossibleCauses, []string{CausePowerLoss, CauseNetworkLoss}) {
		t.Fatalf("while the node is offline: %+v", during)
	}
	node = presenceAt(node, false, 1000+150)
	if node.OfflineSince != 1000+2*minute {
		t.Fatalf("a later inventory keeps when it went: %+v", node)
	}
	node = presenceAt(node, true, 1000+4*minute)
	retried := heartbeat("run-aaaaaaaaaaaaaaaa", 2, bootA)
	retried.Network = &NetworkFacts{FailedAttempts: 2}
	record = observe(record, true, retried, 1000+5*minute, &node)
	o := record.LastOutage
	if o == nil || o.Cause != CauseNetworkLoss || o.StartedAt != 1000 || o.EndedAt != 1000+5*minute {
		t.Fatalf("the cut is kept: %+v", o)
	}
	if after := derive(record, true, &node, 1000+6*minute); after.State != StateBack {
		t.Fatalf("after the cut: %+v", after)
	}
}

// Either sign alone is not a cut: a node that briefly reconnected while every report arrived,
// or a report that needed a retry while the node stayed up (this service was unreachable).
func TestANodeBlipOrARetryAloneIsNotACut(t *testing.T) {
	record := observe(pondRecord{}, false, heartbeat("run-aaaaaaaaaaaaaaaa", 1, bootA), 1000, nil)
	blip := presenceAt(presenceAt(nodePresence{Online: true}, false, 1060), true, 1090)
	record = observe(record, true, heartbeat("run-aaaaaaaaaaaaaaaa", 2, bootA), 1000+5*minute, &blip)
	if record.LastOutage != nil {
		t.Fatalf("a blip with no failed report: %+v", record.LastOutage)
	}
	if answer := derive(record, true, &blip, 1000+6*minute); answer.State != StateOnline {
		t.Fatalf("a node offline before the last report is not news: %+v", answer)
	}
	up := nodePresence{Online: true}
	retried := heartbeat("run-aaaaaaaaaaaaaaaa", 3, bootA)
	retried.Network = &NetworkFacts{FailedAttempts: 1}
	record = observe(record, true, retried, 1000+10*minute, &up)
	if record.LastOutage != nil {
		t.Fatalf("a retry with the node up: %+v", record.LastOutage)
	}
}
