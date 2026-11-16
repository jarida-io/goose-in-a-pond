package enrollment

import (
	"crypto/ed25519"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"log/slog"
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"sync"
	"time"

	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/privatefile"
)

// The Pond status service: what each household's Pond last told this service about
// itself, so that a phone away from home can learn why its Pond does not answer.
//
// Every time here is this service's own clock, stamped when a report arrives. A Pond's
// clock is never trusted for a duration: a board that lost power restarts its clock at
// the last time it saved and lags until its time server answers, hours wrong at worst.
// The only duration a Pond contributes is its uptime, read from the monotonic clock.

// statusDomain separates status report signatures from enrollment approvals and
// household registrations, so a signature captured from one is never accepted as another.
const statusDomain = "goose-pond-status-v1\x00"

// grantDomain separates the read grants a Pond issues to its phones from everything else
// the household key signs.
const grantDomain = "goose-pond-status-grant-v1\x00"

// HeartbeatInterval is how often a running Pond reports. The Pond's own constant
// (`HEARTBEAT_INTERVAL` in pond-core's `network::domain::pond_status`) must match.
const HeartbeatInterval = 5 * time.Minute

// silentAfter is how long without any report before the Pond counts as unreachable: two
// missed heartbeats, and a minute for a slow delivery.
const silentAfter = 2*HeartbeatInterval + time.Minute

// plannedWindow is how long a restart or an update the Pond announced is believed. A
// Pond still silent after it has probably not come back the way it meant to.
const plannedWindow = 30 * time.Minute

// backWindow is how long after an outage ends the answer is `back` rather than `online`.
const backWindow = time.Hour

// MaxGrantLifetime bounds how long a phone's read grant is accepted after it is signed.
// It matches the 30-day presence lapse after which remote access stops anyway.
const MaxGrantLifetime = 31 * 24 * time.Hour

// statusFileLimit bounds the status file read at startup.
const statusFileLimit = 4 << 20

var bootIDPattern = regexp.MustCompile(`^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$`)
var nodeStatePattern = regexp.MustCompile(`^[A-Za-z]{1,32}$`)

// Report kinds.
const (
	KindHeartbeat = "heartbeat"
	KindGoingDown = "going_down"
	KindBooted    = "booted"
)

// Outage causes.
const (
	CausePowerLoss   = "power_loss"
	CauseNetworkLoss = "network_loss"
	CausePlanned     = "planned"
	CauseUnknown     = "unknown"
)

// Derived states a phone is answered with.
const (
	StateUnknown        = "unknown"
	StateOnline         = "online"
	StateBack           = "back"
	StatePlannedRestart = "planned_restart"
	StateUnreachable    = "unreachable"
)

// PondReport is one report from a household's Pond, signed by the household key.
type PondReport struct {
	Household string `json:"household"`
	// Kind is heartbeat, going_down or booted.
	Kind string `json:"kind"`
	// Reason is sent with going_down only: restart, update or shutdown.
	Reason string `json:"reason,omitempty"`
	// PreviousShutdown is sent with booted only: clean, or unclean when the Pond's
	// previous run ended without its graceful stop.
	PreviousShutdown string `json:"previousShutdown,omitempty"`
	// BootID is the host's kernel boot id, where the host has one. It changes only when
	// the host itself restarts, so it separates a reboot from a process restart.
	BootID string `json:"bootId,omitempty"`
	// Run names this run of the Pond process; Sequence counts its reports from one.
	// Together they refuse a replayed or reordered report.
	Run      string `json:"run"`
	Sequence int64  `json:"sequence"`
	// UptimeSeconds is how long the host has been up, from its monotonic clock.
	UptimeSeconds int64 `json:"uptimeSeconds"`
	// ClockSynchronized is whether the host's clock was disciplined by a time source.
	ClockSynchronized *bool `json:"clockSynchronized,omitempty"`
	// Network is what the Pond knows about its own connection when it reported.
	Network *NetworkFacts `json:"network,omitempty"`
	Expires int64         `json:"expires"`
}

// NetworkFacts is what a Pond knows about how its report reached this service.
type NetworkFacts struct {
	// FailedAttempts is how many times this report was tried before it arrived. After a
	// power cut it measures how long the home internet took to return.
	FailedAttempts int64 `json:"failedAttempts"`
	// ProcessSeconds is how long the Pond process had been running when it reported.
	ProcessSeconds int64 `json:"processSeconds"`
	// NodeState is the Pond's own remote-access node state, such as Running.
	NodeState string `json:"nodeState,omitempty"`
}

// StatusGrant lets one of a household's phones read its Pond's status. The Pond signs
// it with the household key and hands it to the phone over its authenticated API. It is
// honoured only while the device it names is an active phone of that household here, so
// revoking or replacing the phone ends it at once.
type StatusGrant struct {
	Household string `json:"household"`
	Device    string `json:"device"`
	Expires   int64  `json:"expires"`
}

// Outage is one stretch during which the Pond did not report, as this service saw it.
type Outage struct {
	// Cause is power_loss, network_loss, planned or unknown.
	Cause string `json:"cause"`
	// Reason is the announced reason of a planned outage, when one arrived.
	Reason    string `json:"reason,omitempty"`
	StartedAt int64  `json:"startedAt"`
	EndedAt   int64  `json:"endedAt"`
}

// PondStatus is the answer to a phone, and to the Pond after each report.
type PondStatus struct {
	// State is unknown, online, back, planned_restart or unreachable.
	State string `json:"state"`
	// LastSeen is when the last report arrived; zero when none ever did.
	LastSeen int64 `json:"lastSeen,omitempty"`
	// Since is when the current silence began, for planned_restart and unreachable.
	Since int64 `json:"since,omitempty"`
	// Reason is the announced reason, for planned_restart.
	Reason string `json:"reason,omitempty"`
	// PossibleCauses lists what an unreachable Pond's silence is consistent with. From
	// outside the home a power cut and a lost home internet look the same.
	PossibleCauses []string `json:"possibleCauses,omitempty"`
	// NodeOnline is whether the coordinator saw the Pond's network node connected at its
	// last inventory read; absent when that is not known.
	NodeOnline *bool `json:"nodeOnline,omitempty"`
	// LastOutage is the most recent outage that has ended, once one has.
	LastOutage *Outage `json:"lastOutage,omitempty"`
	// Now is this service's clock, so a phone can allow for its own clock's error.
	Now int64 `json:"now"`
}

// pondRecord is what this service keeps about one household's Pond.
type pondRecord struct {
	LastSeen          int64      `json:"lastSeen"`
	BootID            string     `json:"bootId,omitempty"`
	Run               string     `json:"run"`
	Sequence          int64      `json:"sequence"`
	ClockSynchronized *bool      `json:"clockSynchronized,omitempty"`
	GoingDown         *goingDown `json:"goingDown,omitempty"`
	LastOutage        *Outage    `json:"lastOutage,omitempty"`
}

// goingDown is an announced stop that has not been followed by the Pond's return yet.
type goingDown struct {
	Reason string `json:"reason"`
	At     int64  `json:"at"`
}

// observe folds one report, arriving at now, into the record. known is false for a
// household that never reported before, which has no silence to explain.
func observe(previous pondRecord, known bool, report PondReport, now int64) pondRecord {
	next := previous
	next.LastSeen = now
	next.Run = report.Run
	next.Sequence = report.Sequence
	next.ClockSynchronized = report.ClockSynchronized
	if report.BootID != "" {
		next.BootID = report.BootID
	}
	switch report.Kind {
	case KindGoingDown:
		next.GoingDown = &goingDown{Reason: report.Reason, At: now}
	case KindBooted:
		next.GoingDown = nil
		if known {
			next.LastOutage = bootOutage(previous, report, now)
		}
	case KindHeartbeat:
		// Still running after an announced stop: the stop did not happen.
		next.GoingDown = nil
		if known && now-previous.LastSeen > int64(silentAfter/time.Second) {
			cause := CauseUnknown
			// The same process on the same boot kept counting: it ran throughout, and only
			// its reports did not arrive, so the home's connection was lost.
			if report.Run == previous.Run && sameBoot(previous.BootID, report.BootID) {
				cause = CauseNetworkLoss
			}
			next.LastOutage = &Outage{Cause: cause, StartedAt: previous.LastSeen, EndedAt: now}
		}
	}
	return next
}

// bootOutage explains the silence a booted report ends.
func bootOutage(previous pondRecord, report PondReport, now int64) *Outage {
	outage := &Outage{Cause: CauseUnknown, StartedAt: previous.LastSeen, EndedAt: now}
	rebooted := previous.BootID != "" && report.BootID != "" && previous.BootID != report.BootID
	switch {
	case previous.GoingDown != nil:
		outage.Cause, outage.Reason, outage.StartedAt = CausePlanned, previous.GoingDown.Reason, previous.GoingDown.At
	case report.PreviousShutdown == "clean":
		// Stopped on purpose, but its announcement never arrived.
		outage.Cause = CausePlanned
	case report.PreviousShutdown == "unclean" && rebooted:
		outage.Cause = CausePowerLoss
		// The host was back uptime seconds before this report; the rest of the silence was
		// the home's connection coming back after it.
		if up := now - report.UptimeSeconds; up > outage.StartedAt && up < now {
			outage.EndedAt = up
		}
	}
	return outage
}

func sameBoot(previous, current string) bool {
	return previous == "" || current == "" || previous == current
}

// nodePresence is what the coordinator last said about a Pond's network node.
type nodePresence struct {
	Online bool
}

// derive answers what the record says about the Pond at now.
func derive(record pondRecord, known bool, node *nodePresence, now int64) PondStatus {
	status := PondStatus{State: StateUnknown, Now: now}
	if node != nil {
		online := node.Online
		status.NodeOnline = &online
	}
	if !known {
		return status
	}
	status.LastSeen = record.LastSeen
	status.LastOutage = record.LastOutage
	silent := now-record.LastSeen > int64(silentAfter/time.Second)
	if down := record.GoingDown; down != nil {
		if !silent || down.Reason == "shutdown" || now-down.At <= int64(plannedWindow/time.Second) {
			status.State, status.Since, status.Reason = StatePlannedRestart, down.At, down.Reason
			return status
		}
		status.State, status.Since = StateUnreachable, down.At
		status.PossibleCauses = possibleCauses(node)
		return status
	}
	if silent {
		status.State, status.Since = StateUnreachable, record.LastSeen
		status.PossibleCauses = possibleCauses(node)
		return status
	}
	if outage := record.LastOutage; outage != nil && now-outage.EndedAt <= int64(backWindow/time.Second) {
		status.State = StateBack
		return status
	}
	status.State = StateOnline
	return status
}

// possibleCauses lists what a silence is consistent with. A node the coordinator still
// sees connected means the home's power and internet are up and the Pond itself stopped.
func possibleCauses(node *nodePresence) []string {
	if node != nil && node.Online {
		return []string{"pond_stopped"}
	}
	return []string{CausePowerLoss, CauseNetworkLoss}
}

// validReport checks a report's shape; the signature and household are checked apart.
func validReport(r PondReport, now int64) bool {
	if r.Expires <= now || r.Expires > now+300 || !identifier.MatchString(r.Run) || r.Sequence < 1 || r.Sequence > 1<<40 {
		return false
	}
	if r.UptimeSeconds < 0 || r.UptimeSeconds > 100*365*24*3600 || (r.BootID != "" && !bootIDPattern.MatchString(r.BootID)) {
		return false
	}
	if n := r.Network; n != nil && (n.FailedAttempts < 0 || n.FailedAttempts > 1<<20 || n.ProcessSeconds < 0 || n.ProcessSeconds > r.UptimeSeconds+60 || (n.NodeState != "" && !nodeStatePattern.MatchString(n.NodeState))) {
		return false
	}
	switch r.Kind {
	case KindHeartbeat:
		return r.Reason == "" && r.PreviousShutdown == ""
	case KindGoingDown:
		return (r.Reason == "restart" || r.Reason == "update" || r.Reason == "shutdown") && r.PreviousShutdown == ""
	case KindBooted:
		return r.Reason == "" && (r.PreviousShutdown == "clean" || r.PreviousShutdown == "unclean")
	}
	return false
}

// SignStatus signs a status report with the household key.
func SignStatus(report PondReport, key ed25519.PrivateKey) (Envelope, error) {
	payload, err := json.Marshal(report)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{
		Payload:   base64.StdEncoding.EncodeToString(payload),
		Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(key, append([]byte(statusDomain), payload...))),
	}, nil
}

// SignGrant signs a phone's read grant with the household key, in the form the phone
// presents it: `PondGrant <payload>.<signature>`, both standard base64.
func SignGrant(grant StatusGrant, key ed25519.PrivateKey) (string, error) {
	payload, err := json.Marshal(grant)
	if err != nil {
		return "", err
	}
	signature := ed25519.Sign(key, append([]byte(grantDomain), payload...))
	return base64.StdEncoding.EncodeToString(payload) + "." + base64.StdEncoding.EncodeToString(signature), nil
}

// statusStore keeps the last record of each household's Pond in its own file beside the
// enrollment state. It is not security state: losing it loses only last-seen times, so an
// unreadable file is set aside and the service starts without it rather than refusing to.
type statusStore struct {
	mu      sync.Mutex
	path    string
	records map[string]pondRecord
}

func openStatus(directory string) *statusStore {
	store := &statusStore{path: filepath.Join(directory, "status.json"), records: map[string]pondRecord{}}
	file, err := privatefile.Open(store.path, statusFileLimit)
	if errors.Is(err, os.ErrNotExist) {
		return store
	}
	if err == nil {
		decoder := json.NewDecoder(io.LimitReader(file, statusFileLimit))
		decoder.DisallowUnknownFields()
		err = decoder.Decode(&store.records)
		file.Close()
	}
	if err != nil || store.records == nil {
		store.records = map[string]pondRecord{}
		aside := store.path + ".unreadable"
		renamed := os.Rename(store.path, aside)
		slog.Warn("pond status file unreadable; starting without last-seen times", "kind", "pond_status_unreadable", "error", err, "set_aside", renamed == nil)
	}
	return store
}

// save writes every record atomically. Callers hold mu.
func (s *statusStore) save() error {
	data, err := json.Marshal(s.records)
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(s.path), ".status-")
	if err != nil {
		return err
	}
	defer os.Remove(f.Name())
	if _, err = f.Write(data); err == nil {
		err = f.Sync()
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	if err != nil {
		return err
	}
	return os.Rename(f.Name(), s.path)
}

// householdKey returns a registered household's public key, under the store lock.
func (s *Service) householdKey(household string) (ed25519.PublicKey, bool) {
	h, exists := s.Store.value.Households[household]
	key, err := base64.StdEncoding.DecodeString(h.PublicKey)
	if !exists || err != nil || len(key) != ed25519.PublicKeySize {
		return nil, false
	}
	return key, true
}

// pondNode is what the last inventory said about the household's Pond node, under the
// store lock; nil when that is not known.
func (s *Service) pondNode(household string) *nodePresence {
	for _, device := range s.Store.value.Devices[household] {
		if device.Role == "pond" && device.Status == "active" && device.NodeID != "" {
			if presence, ok := s.presence[device.NodeID]; ok {
				return &presence
			}
		}
	}
	return nil
}

// reportStatus is `POST /v1/pond/status`: a Pond's signed report about itself.
func (s *Service) reportStatus(w http.ResponseWriter, r *http.Request) {
	if !s.approvals.allow(s.client(r)) || !s.limiter.Allow() {
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 4096))
	if err != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	var envelope Envelope
	var report PondReport
	if strict(body, &envelope) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	payload, err := base64.StdEncoding.DecodeString(envelope.Payload)
	if err != nil || strict(payload, &report) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	signature, _ := base64.StdEncoding.DecodeString(envelope.Signature)
	now := s.Now().Unix()
	s.Store.mu.Lock()
	key, exists := s.householdKey(report.Household)
	node := s.pondNode(report.Household)
	s.Store.mu.Unlock()
	if !exists || !ed25519.Verify(key, append([]byte(statusDomain), payload...), signature) {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}
	if !validReport(report, now) {
		http.Error(w, `{"error":"invalid_report"}`, 400)
		return
	}
	if !s.statusReports.allow(report.Household) {
		w.Header().Set("Retry-After", "30")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	s.status.mu.Lock()
	defer s.status.mu.Unlock()
	previous, known := s.status.records[report.Household]
	if known && previous.Run == report.Run && report.Sequence <= previous.Sequence {
		http.Error(w, `{"error":"stale_report"}`, 409)
		return
	}
	next := observe(previous, known, report, now)
	s.status.records[report.Household] = next
	if err := s.status.save(); err != nil {
		// Kept in memory regardless: a phone asking now still gets the truth.
		slog.Warn("pond status not persisted", "kind", "pond_status_save_failed", "error", err)
	}
	logged := []any{"kind", "pond_status_report", "report", report.Kind, "household", report.Household}
	if report.Kind == KindGoingDown {
		logged = append(logged, "reason", report.Reason)
	}
	if next.LastOutage != nil && next.LastOutage != previous.LastOutage {
		logged = append(logged, "outage_cause", next.LastOutage.Cause, "outage_secs", next.LastOutage.EndedAt-next.LastOutage.StartedAt)
	}
	slog.Info("pond status reported", logged...)
	json.NewEncoder(w).Encode(derive(next, true, node, now))
}

// readStatus is `GET /v1/pond/status`: a phone asking about its household's Pond, with a
// grant the Pond signed in `Authorization: PondGrant <payload>.<signature>`.
func (s *Service) readStatus(w http.ResponseWriter, r *http.Request) {
	if !s.approvals.allow(s.client(r)) || !s.limiter.Allow() {
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	token, found := strings.CutPrefix(r.Header.Get("Authorization"), "PondGrant ")
	encodedPayload, encodedSignature, split := strings.Cut(token, ".")
	payload, payloadErr := base64.StdEncoding.DecodeString(encodedPayload)
	signature, signatureErr := base64.StdEncoding.DecodeString(encodedSignature)
	var grant StatusGrant
	if !found || !split || len(token) > 2048 || payloadErr != nil || signatureErr != nil || strict(payload, &grant) != nil {
		http.Error(w, `{"error":"unauthorized"}`, 401)
		return
	}
	now := s.Now().Unix()
	s.Store.mu.Lock()
	key, exists := s.householdKey(grant.Household)
	device, enrolled := s.Store.value.Devices[grant.Household][grant.Device]
	node := s.pondNode(grant.Household)
	s.Store.mu.Unlock()
	if !exists || !ed25519.Verify(key, append([]byte(grantDomain), payload...), signature) || grant.Expires <= now || grant.Expires > now+int64(MaxGrantLifetime/time.Second) {
		http.Error(w, `{"error":"unauthorized"}`, 401)
		return
	}
	// Signed by the household, but honoured only for a phone it still has enrolled here.
	if !enrolled || device.Role != "phone" || device.Status != "active" {
		http.Error(w, `{"error":"device_not_enrolled"}`, 403)
		return
	}
	if !s.statusReads.allow(grant.Household) {
		w.Header().Set("Retry-After", "10")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	s.status.mu.Lock()
	record, known := s.status.records[grant.Household]
	s.status.mu.Unlock()
	answer := derive(record, known, node, now)
	slog.Debug("pond status read", "kind", "pond_status_read", "household", grant.Household, "state", answer.State)
	json.NewEncoder(w).Encode(answer)
}
