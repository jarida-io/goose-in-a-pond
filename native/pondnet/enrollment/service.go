package enrollment

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"golang.org/x/time/rate"
	"io"
	"log/slog"
	"net/http"
	"net/netip"
	"reflect"
	"regexp"
	"slices"
	"strconv"
	"sync/atomic"
	"time"
)

var identifier = regexp.MustCompile(`^[a-zA-Z0-9_-]{16,80}$`)
var nodeKeyPattern = regexp.MustCompile(`^nodekey:[0-9a-f]{64}$`)
var machineKeyPattern = regexp.MustCompile(`^mkey:[0-9a-f]{64}$`)
var numeric = regexp.MustCompile(`^[1-9][0-9]{0,19}$`)

// userNamePattern bounds the coordinator user name derived from a household id.
var userNamePattern = regexp.MustCompile(`^household-[0-9a-f]{32}$`)

// ErrRegistrationRejected means no registration mutation was accepted. Unlike a
// lost response, this outcome must never be turned into an inventory-based grant.
var ErrRegistrationRejected = errors.New("registration was rejected")

// Approval is signed by the locally approved Pond, including the exact pending node.
type Approval struct {
	Household  string `json:"household"`
	Device     string `json:"device"`
	Action     string `json:"action"`
	Role       string `json:"role"`
	AuthID     string `json:"authId"`
	NodeKey    string `json:"nodeKey"`
	MachineKey string `json:"machineKey,omitempty"`
	Nonce      string `json:"nonce"`
	Expires    int64  `json:"expires"`
	// ExpectedRevision is supplied only for an explicitly approved replacement.
	ExpectedRevision string `json:"expectedRevision,omitempty"`
}

// Envelope signs the exact payload bytes, avoiding cross-language JSON canonicalization.
type Envelope struct {
	Payload   string `json:"payload"`
	Signature string `json:"signature"`
}

// Registered is the coordinator's response, verified before permitting any traffic.
type Registered struct {
	ID         string
	Key        string
	UserID     string
	Addresses  []string
	Tags       []string
	Routes     []string
	MachineKey string
}

// Backend is the private Headscale administrative boundary.
type Backend interface {
	Inventory(context.Context) ([]Registered, error)
	EnsureUser(context.Context, string) (string, error)
	Register(context.Context, string, string) (Registered, error)
	Delete(context.Context, string) error
	// Policy writes the complete policy document built from the rules and attributes.
	Policy(context.Context, []Rule, []NodeAttr) error
	// CurrentPolicy reads the policy document the coordinator holds, so that an
	// unchanged policy is never written again.
	CurrentPolicy(context.Context) ([]byte, error)
}

// Rule permits only approved phone addresses to reach their own Pond HTTPS port.
type Rule struct {
	Action string   `json:"action"`
	Src    []string `json:"src"`
	Dst    []string `json:"dst"`
}

// NodeAttr grants node capabilities to every node whose address is in Target.
type NodeAttr struct {
	Target []string `json:"target"`
	Attr   []string `json:"attr"`
}

// cacheNetworkMaps is tailcfg.NodeAttrCacheNetworkMaps. A node granted it keeps
// its last network map on disk and starts from it when the coordinator is out of
// reach, instead of waiting for a live map. It is granted to phones only: a Pond
// must always start from the coordinator's current map, so that a phone revoked
// while the Pond was down never passes a stale packet filter on the Pond.
const cacheNetworkMaps = "cache-network-maps"

// Service serializes security transitions and bounds public work before parsing.
type Service struct {
	Store   *Store
	Backend Backend
	Now     func() time.Time
	// TrustedProxies are the reverse proxies whose X-Forwarded-For names the real client.
	// Without them every request would share the proxy's own rate-limit budget.
	TrustedProxies []netip.Prefix
	// ProvisioningKeys sign the device certificates that admit a household without an
	// invite. More than one lets a key be rotated; none means only invites admit.
	ProvisioningKeys []ed25519.PublicKey
	slots            chan struct{}
	limiter          *rate.Limiter
	// registrations and approvals limit each client address; households limits each
	// signed household, so one busy household cannot spend another's budget.
	registrations *sources
	approvals     *sources
	households    *sources
	// degraded is set while the coordinator cannot be reconciled; requests still run.
	degraded atomic.Bool
	// applied is the decoded policy document this service last wrote to the coordinator
	// or found already there, and nil while that is unknown: before the first policy
	// evaluation and after a write whose outcome is unknown. Guarded by Store.mu.
	applied any
}

// New reconciles the durable allowlist before accepting enrollment requests. A coordinator
// that cannot be reconciled yet leaves the service degraded rather than failing to start,
// which would only restart it into the same failure; Reconcile retries.
func New(ctx context.Context, s *Store, b Backend) (*Service, error) {
	service := &Service{
		Store: s, Backend: b, Now: time.Now, slots: make(chan struct{}, 8), limiter: rate.NewLimiter(10, 20),
		registrations: newSources(rate.Limit(1.0/60.0), 3),
		approvals:     newSources(rate.Limit(1), 10),
		households:    newSources(rate.Limit(1.0/6.0), 10),
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.failure != nil {
		return nil, s.failure
	}
	// A failure marks the service degraded, and degrade logs it with its cause.
	_ = service.policy(ctx)
	return service, nil
}

// degrade marks the service degraded after err, logging only the transition into it,
// so a coordinator that stays unreachable is reported once rather than every cycle.
func (s *Service) degrade(err error) {
	if !s.degraded.Swap(true) {
		slog.Warn("enrollment service degraded: the coordinator could not be reconciled", "error", err)
	}
}

// policy brings the coordinator's policy into line with the store and a fresh inventory.
func (s *Service) policy(ctx context.Context) error {
	_, err := s.enforcePolicy(ctx)
	return err
}

// enforcePolicy is policy, also returning the inventory the policy was computed from,
// so that Reconcile can work from the same read rather than make another.
func (s *Service) enforcePolicy(ctx context.Context) (inventory []Registered, err error) {
	defer func() {
		if err != nil {
			s.degrade(err)
		}
	}()
	if s.Store.failure != nil {
		return nil, s.Store.failure
	}
	inventory, err = s.Backend.Inventory(ctx)
	if err != nil {
		return nil, err
	}
	// Indexed once, so matching runs under the store lock in devices plus nodes rather
	// than their product. The first node with a given ID wins, as a scan would find it.
	byID := make(map[string]Registered, len(inventory))
	for _, node := range inventory {
		if _, seen := byID[node.ID]; !seen {
			byID[node.ID] = node
		}
	}
	matches := func(device Device, household Household) bool {
		node, found := byID[device.NodeID]
		return found && node.Key == device.Key && (device.MachineKey == "" || node.MachineKey == device.MachineKey) && node.UserID == household.UserID && slices.Contains(node.Addresses, device.Address) && len(node.Tags) == 0 && len(node.Routes) == 0
	}
	// Sorted, so an unchanged store always produces a byte-identical policy.
	households := make([]string, 0, len(s.Store.value.Devices))
	for id := range s.Store.value.Devices {
		households = append(households, id)
	}
	slices.Sort(households)
	rules := []Rule{}
	// Every verified phone, across households, sorted so the policy is deterministic.
	cachers := []string{}
	for _, id := range households {
		devices := s.Store.value.Devices[id]
		pond := ""
		phones := []string{}
		for _, device := range devices {
			if device.Status != "active" || !matches(device, s.Store.value.Households[id]) {
				continue
			}
			if device.Role == "pond" {
				pond = device.Address
			} else {
				phones = append(phones, device.Address)
			}
		}
		cachers = append(cachers, phones...)
		if pond != "" && len(phones) > 0 {
			slices.Sort(phones)
			rules = append(rules, Rule{Action: "accept", Src: phones, Dst: []string{pond + ":" + strconv.Itoa(int(s.Store.value.Households[id].Port))}})
		}
	}
	attrs := []NodeAttr{}
	if len(cachers) > 0 {
		slices.Sort(cachers)
		attrs = append(attrs, NodeAttr{Target: cachers, Attr: []string{cacheNetworkMaps}})
	}
	if err := s.applyPolicy(ctx, rules, attrs); err != nil {
		return nil, err
	}
	if s.degraded.Swap(false) {
		slog.Info("enrollment service recovered: the coordinator policy is reconciled")
	}
	return inventory, nil
}

// applyPolicy writes the policy only when the coordinator's differs from it.
//
// Every write makes the coordinator re-evaluate the policy for every node and can push
// a new network map to every connected phone, so an unchanged policy is not written
// again. The two are compared decoded, so the coordinator's whitespace or key order is
// never a difference. Reading still runs every evaluation, so a policy changed on the
// coordinator by anything else is put back. A read that fails falls back to writing:
// an allowlist enforced twice costs a reload, one left unenforced is an exposure.
func (s *Service) applyPolicy(ctx context.Context, rules []Rule, attrs []NodeAttr) error {
	document, err := policyDocument(rules, attrs)
	if err != nil {
		return err
	}
	var computed any
	if err := json.Unmarshal(document, &computed); err != nil {
		return err
	}
	current, readErr := s.coordinatorPolicy(ctx)
	if readErr == nil && reflect.DeepEqual(current, computed) {
		s.applied = computed
		slog.Debug("coordinator policy unchanged; not written", "kind", "policy_unchanged")
		return nil
	}
	computedRules, computedAttrs, _ := policyShape(computed)
	switch {
	case readErr != nil:
		slog.Warn("coordinator policy could not be read; writing it", "kind", "policy_read_failed", "error", readErr)
	case s.applied == nil:
		slog.Info("coordinator policy differs from the computed one, and what this service last wrote is not known; writing it", "kind", "policy_unverified", "rules", computedRules, "node_attrs", computedAttrs)
	case !reflect.DeepEqual(current, s.applied):
		// Nothing but this service should write the policy, so this is an operator or a
		// fault on the coordinator. Only counts are logged, never addresses.
		currentRules, currentAttrs, currentFields := policyShape(current)
		_, _, computedFields := policyShape(computed)
		slog.Warn("coordinator policy drifted from what this service last wrote; restoring it", "kind", "policy_drift",
			"store_changed", !reflect.DeepEqual(computed, s.applied),
			"coordinator_rules", currentRules, "coordinator_node_attrs", currentAttrs, "coordinator_fields", currentFields,
			"computed_rules", computedRules, "computed_node_attrs", computedAttrs, "computed_fields", computedFields)
	default:
		slog.Info("coordinator policy updated: the store changed", "kind", "policy_store_changed", "rules", computedRules, "node_attrs", computedAttrs)
	}
	if err := s.Backend.Policy(ctx, rules, attrs); err != nil {
		// The write may still have landed, so what the coordinator holds is unknown.
		s.applied = nil
		slog.Warn("coordinator policy write failed", "kind", "policy_write_failed", "error", err)
		return err
	}
	s.applied = computed
	slog.Info("coordinator policy written", "kind", "policy_written", "rules", computedRules, "node_attrs", computedAttrs)
	return nil
}

// coordinatorPolicy reads and decodes the coordinator's policy document.
func (s *Service) coordinatorPolicy(ctx context.Context) (any, error) {
	document, err := s.Backend.CurrentPolicy(ctx)
	if err != nil {
		return nil, err
	}
	var current any
	if err := json.Unmarshal(document, &current); err != nil {
		return nil, errors.New("the coordinator policy is not a JSON document")
	}
	return current, nil
}

// policyShape summarises a decoded policy document for a log line: its rule count,
// node attribute count and top-level field count. It never reveals an address.
func policyShape(document any) (rules, attrs, fields int) {
	top, _ := document.(map[string]any)
	acls, _ := top["acls"].([]any)
	nodeAttrs, _ := top["nodeAttrs"].([]any)
	return len(acls), len(nodeAttrs), len(top)
}
func strict(data []byte, value any) error {
	d := json.NewDecoder(bytes.NewReader(data))
	d.DisallowUnknownFields()
	if err := d.Decode(value); err != nil {
		return err
	}
	if d.Decode(new(any)) != io.EOF {
		return errors.New("trailing JSON")
	}
	return nil
}

// Sign prepares a request on the Pond; the private key never leaves its local directory.
func Sign(a Approval, key ed25519.PrivateKey) (Envelope, error) {
	payload, err := json.Marshal(a)
	if err != nil {
		return Envelope{}, err
	}
	return Envelope{Payload: base64.StdEncoding.EncodeToString(payload), Signature: base64.StdEncoding.EncodeToString(ed25519.Sign(key, append([]byte("goose-enrollment-v1\x00"), payload...)))}, nil
}
func (s *Service) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Content-Type", "application/json")
	if r.Method == "GET" && r.URL.Path == "/health" {
		s.Store.mu.Lock()
		failed := s.Store.failure != nil
		s.Store.mu.Unlock()
		if failed {
			http.Error(w, `{"error":"storage_unavailable"}`, 503)
			return
		}
		// Still 200: the process is serving, and an unhealthy container would keep the
		// gateway, and so every Pond, from reaching it at all.
		if s.degraded.Load() {
			io.WriteString(w, `{"ok":true,"degraded":true}`)
			return
		}
		io.WriteString(w, `{"ok":true}`)
		return
	}
	if r.Method == "POST" && r.URL.Path == "/v1/household" {
		s.registerHousehold(w, r)
		return
	}
	if r.Method != "POST" || r.URL.Path != "/v1/approval" {
		http.NotFound(w, r)
		return
	}
	if !s.approvals.allow(s.client(r)) || !s.limiter.Allow() {
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	select {
	case s.slots <- struct{}{}:
		defer func() { <-s.slots }()
	default:
		w.Header().Set("Retry-After", "2")
		http.Error(w, `{"error":"busy"}`, 429)
		return
	}
	body, err := io.ReadAll(http.MaxBytesReader(w, r.Body, 8192))
	if err != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	var envelope Envelope
	var approval Approval
	if strict(body, &envelope) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	payload, err := base64.StdEncoding.DecodeString(envelope.Payload)
	if err != nil || strict(payload, &approval) != nil {
		http.Error(w, `{"error":"invalid_request"}`, 400)
		return
	}
	signature, err := base64.StdEncoding.DecodeString(envelope.Signature)
	if err != nil {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}
	ctx, cancel := context.WithTimeout(r.Context(), 15*time.Second)
	defer cancel()
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	if s.Store.failure != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	h, exists := s.Store.value.Households[approval.Household]
	key, _ := base64.StdEncoding.DecodeString(h.PublicKey)
	now := s.Now().Unix()
	if !exists || len(key) != ed25519.PublicKeySize || !ed25519.Verify(key, append([]byte("goose-enrollment-v1\x00"), payload...), signature) || approval.Expires <= now || approval.Expires > now+300 || !identifier.MatchString(approval.Device) || !identifier.MatchString(approval.Nonce) {
		http.Error(w, `{"error":"unauthorized"}`, 403)
		return
	}
	if !s.households.allow(approval.Household) {
		w.Header().Set("Retry-After", "10")
		http.Error(w, `{"error":"rate_limited"}`, 429)
		return
	}
	requestID := approval.Household + ":" + approval.Nonce
	if _, used := s.Store.value.Requests[requestID]; used {
		http.Error(w, `{"error":"approval_used"}`, 409)
		return
	}
	// Spent as soon as it is authentic, and durably, so no later refusal leaves a signed
	// approval that can be presented again.
	s.consume(requestID, approval.Expires, now)
	if err := s.Store.save(); err != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	devices := s.Store.value.Devices[approval.Household]
	current, present := devices[approval.Device]
	if approval.Action == "inspect" {
		if !present {
			http.Error(w, `{"error":"enrollment_missing"}`, 404)
			return
		}
		// Legacy records acquire a durable revision before an operator can approve replacement.
		if current.Revision == "" {
			current.Revision = rand.Text()
			devices[approval.Device] = current
		}
		if err := s.Store.save(); err != nil {
			http.Error(w, `{"error":"storage_unavailable"}`, 503)
			return
		}
		json.NewEncoder(w).Encode(current)
		return
	}
	replacing := approval.Action == "replace"
	if replacing {
		if !present || current.Role != "phone" || approval.Role != "phone" || current.Revision == "" || approval.ExpectedRevision != current.Revision || (current.Status != "revoked" && current.Status != "failed" && current.Status != "pending") {
			http.Error(w, `{"error":"replacement_conflict"}`, 409)
			return
		}
		// Its own reason, because unlike a conflict it is about the household rather than
		// this approval, and it passes: expireRetired frees records as identities leave.
		if len(s.Store.value.Retired[approval.Household]) >= retiredCapacity {
			slog.Warn("replacement refused: the household's retired records are at capacity", "household", approval.Household, "capacity", retiredCapacity)
			http.Error(w, `{"error":"retired_capacity"}`, 409)
			return
		}
	}
	if approval.Action == "enroll" || replacing {
		if (!replacing && (present || len(devices) >= 32)) || (approval.Role != "pond" && approval.Role != "phone") || len(approval.AuthID) < 16 || len(approval.AuthID) > 256 || !machineKeyPattern.MatchString(approval.MachineKey) || (approval.NodeKey != "" && !nodeKeyPattern.MatchString(approval.NodeKey)) {
			http.Error(w, `{"error":"invalid_enrollment"}`, 409)
			return
		}
		for _, householdDevices := range s.Store.value.Devices {
			for _, device := range householdDevices {
				if device.MachineKey == approval.MachineKey {
					http.Error(w, `{"error":"identity_already_enrolled"}`, 409)
					return
				}
			}
		}
		for _, retired := range s.Store.value.Retired {
			for _, device := range retired {
				if device.MachineKey == approval.MachineKey {
					http.Error(w, `{"error":"identity_retired"}`, 409)
					return
				}
			}
		}
		inventory, err := s.Backend.Inventory(ctx)
		if err != nil {
			http.Error(w, `{"error":"coordinator_unavailable"}`, 503)
			return
		}
		for _, node := range inventory {
			if node.MachineKey == approval.MachineKey {
				http.Error(w, `{"error":"identity_already_registered"}`, 409)
				return
			}
		}
		hasPond := false
		activePond := false
		for _, d := range devices {
			if d.Role == "pond" {
				hasPond = true
				activePond = d.Status == "active"
			}
		}
		if (approval.Role == "pond" && hasPond) || (approval.Role == "phone" && !activePond) {
			http.Error(w, `{"error":"pond_registration_required"}`, 409)
			return
		}
	} else if approval.Action != "revoke" || (!present && (approval.Role != "phone" || len(devices) >= 256)) {
		http.Error(w, `{"error":"invalid_action"}`, 400)
		return
	}
	if approval.Action == "enroll" && s.enrolledLocked() >= EnrollmentCapacity {
		// Every enrolled device is a coordinator node the policy must read; past this the
		// inventory would stop fitting and revocations everywhere would stall.
		slog.Warn("enrollment refused: the service is at capacity", "capacity", EnrollmentCapacity)
		w.Header().Set("Retry-After", "3600")
		http.Error(w, `{"error":"capacity"}`, 503)
		return
	}
	if approval.Action == "enroll" || replacing {
		if replacing {
			// Retain the old binding until its identity has stayed off the coordinator for
			// retiredGrace, so a late registration under it is still deleted; see expireRetired.
			current.Status = "revoked"
			s.Store.value.Retired[approval.Household] = append(s.Store.value.Retired[approval.Household], current)
		}
		devices[approval.Device] = Device{Role: approval.Role, Status: "pending", MachineKey: approval.MachineKey, Key: approval.NodeKey, Revision: rand.Text()}
	} else {
		if !present {
			current.Role = "phone"
		}
		current.Status = "revoking"
		current.Revision = rand.Text()
		devices[approval.Device] = current
	}
	if err = s.Store.save(); err != nil {
		http.Error(w, `{"error":"storage_unavailable"}`, 503)
		return
	}
	// Persist consumption before an external mutation. Ambiguous registration is never replayed.
	if approval.Action == "revoke" {
		if err = s.policy(ctx); err == nil {
			err = s.deleteOwned(ctx, current, h.UserID)
		}
		if err == nil {
			current = Device{Role: current.Role, Status: "revoked", MachineKey: current.MachineKey, Revision: current.Revision}
			devices[approval.Device] = current
			err = s.Store.save()
		}
	} else {
		if replacing {
			// Exclude the old identity before permitting the new registration. A failure
			// leaves a recoverable pending intent; never replay the coordinator mutation.
			err = s.policy(ctx)
			if err == nil {
				err = s.deleteOwned(ctx, current, h.UserID)
			}
		}
		var node Registered
		if err == nil {
			node, err = s.Backend.Register(ctx, h.UserID, approval.AuthID)
		}
		if errors.Is(err, ErrRegistrationRejected) {
			failed := devices[approval.Device]
			failed.Status = "failed"
			devices[approval.Device] = failed
			if saveErr := s.Store.save(); saveErr != nil {
				err = saveErr
			}
		}
		if err == nil {
			var active Device
			active, err = s.validateRegistered(node, devices[approval.Device], h.UserID)
			if err == nil {
				devices[approval.Device] = active
				if err = s.Store.save(); err == nil {
					err = s.policy(ctx)
				}
			} else {
				slog.Warn("enrollment registration refused: the coordinator returned an unexpected node", "node", node.ID, "error", err)
				failed := devices[approval.Device]
				failed.Status = "failed"
				devices[approval.Device] = failed
				if saveErr := s.Store.save(); saveErr != nil {
					err = saveErr
				} else if deleteErr := s.deleteRefused(ctx, node, failed, h.UserID); deleteErr != nil {
					// Best effort: a failed record that keeps its machine key is a tombstone,
					// so Reconcile deletes the node if this attempt could not.
					slog.Warn("refused registration's node left for reconciliation to delete", "error", deleteErr)
				}
			}
		}
	}
	if err != nil {
		slog.Warn("enrollment transition incomplete", "action", approval.Action)
		http.Error(w, `{"error":"enrollment_incomplete"}`, 503)
		return
	}
	slog.Info("enrollment transition completed", "action", approval.Action)
	json.NewEncoder(w).Encode(devices[approval.Device])
}

// Reconcile recovers a lost registration response by its approved machine key,
// and retries revocations. It never repeats an ambiguous registration write. A
// failure leaves the service degraded until a later reconciliation succeeds.
//
// The policy is enforced first, so a revoked identity is excluded before anything else
// happens, and again at the end only when this pass changed one of its inputs: a device
// record or the coordinator's nodes. Retired records are not an input to the policy.
func (s *Service) Reconcile(ctx context.Context) (err error) {
	s.Store.mu.Lock()
	defer s.Store.mu.Unlock()
	defer func() {
		if err != nil {
			s.degrade(err)
		}
	}()
	inventory, err := s.enforcePolicy(ctx)
	if err != nil {
		return err
	}
	changed := false
	// Indexed once by machine key, so pairing devices with nodes under the store lock
	// is linear rather than devices times nodes. Each list keeps inventory order.
	byMachine := map[string][]Registered{}
	for _, node := range inventory {
		byMachine[node.MachineKey] = append(byMachine[node.MachineKey], node)
	}
	// owned narrows the inventory to the nodes deleteOwnedFrom can match: a device
	// bound to a machine key matches only nodes carrying it.
	owned := func(device Device) []Registered {
		if device.MachineKey == "" {
			return inventory
		}
		return byMachine[device.MachineKey]
	}
	for household, devices := range s.Store.value.Devices {
		for id, device := range devices {
			if device.Status == "pending" && device.MachineKey != "" {
				matches := byMachine[device.MachineKey]
				if len(matches) == 1 {
					active, err := s.validateRegistered(matches[0], device, s.Store.value.Households[household].UserID)
					if err != nil {
						continue
					}
					devices[id] = active
					changed = true
					if err = s.Store.save(); err != nil {
						return err
					}
					slog.Info("enrollment registration reconciled")
				}
				continue
			}
			// A revoked or failed record that keeps its machine key is a tombstone: any node
			// still registered under that identity, late or refused, is deleted.
			tombstone := (device.Status == "revoked" || device.Status == "failed") && device.MachineKey != ""
			if device.Status != "revoking" && !tombstone {
				continue
			}
			deleted, err := s.deleteOwnedFrom(ctx, device, s.Store.value.Households[household].UserID, owned(device))
			changed = changed || deleted
			if err != nil {
				return err
			}
			if tombstone {
				continue
			}
			device = Device{Role: device.Role, Status: "revoked", MachineKey: device.MachineKey, Revision: device.Revision}
			devices[id] = device
			changed = true
			if err := s.Store.save(); err != nil {
				return err
			}
		}
	}
	for household, retired := range s.Store.value.Retired {
		for _, device := range retired {
			deleted, err := s.deleteOwnedFrom(ctx, device, s.Store.value.Households[household].UserID, owned(device))
			changed = changed || deleted
			if err != nil {
				return err
			}
		}
	}
	if err := s.expireRetired(inventory); err != nil {
		return err
	}
	if !changed {
		return nil
	}
	return s.policy(ctx)
}

// enrolledLocked counts the devices that hold, or may yet hold, a coordinator node.
func (s *Service) enrolledLocked() int {
	count := 0
	for _, devices := range s.Store.value.Devices {
		for _, device := range devices {
			if device.Status != "revoked" && device.Status != "failed" {
				count++
			}
		}
	}
	return count
}

func (s *Service) consume(requestID string, expiry, now int64) {
	for k, expires := range s.Store.value.Requests {
		if expires <= now {
			delete(s.Store.value.Requests, k)
		}
	}
	s.Store.value.Requests[requestID] = expiry
}

func (s *Service) validateRegistered(node Registered, pending Device, user string) (Device, error) {
	invalid := errors.New("coordinator returned an unexpected node")
	address := ""
	for _, value := range node.Addresses {
		if ip, err := netip.ParseAddr(value); err == nil && netip.MustParsePrefix("100.64.0.0/10").Contains(ip) {
			address = ip.String()
			break
		}
	}
	if node.MachineKey != pending.MachineKey || (pending.Key != "" && node.Key != pending.Key) || !nodeKeyPattern.MatchString(node.Key) || node.UserID != user || !numeric.MatchString(node.ID) || address == "" || len(node.Tags) != 0 || len(node.Routes) != 0 {
		return Device{}, invalid
	}
	for _, devices := range s.Store.value.Devices {
		for _, existing := range devices {
			if existing.NodeID != "" && (existing.NodeID == node.ID || existing.Address == address || existing.MachineKey == node.MachineKey) {
				return Device{}, invalid
			}
		}
	}
	return Device{Key: node.Key, MachineKey: node.MachineKey, NodeID: node.ID, Address: address, Role: pending.Role, Status: "active", Revision: pending.Revision}, nil
}

// deleteRefused deletes what a refused registration left on the coordinator: any node under
// the approved machine key, and the node it returned when that is this household's user's and
// no record holds it. A returned node under another machine key matches no tombstone, so
// without this it would count toward MaxNodes indefinitely.
func (s *Service) deleteRefused(ctx context.Context, node Registered, failed Device, user string) error {
	if err := s.deleteOwned(ctx, failed, user); err != nil {
		return err
	}
	if node.ID == "" || node.UserID != user || s.recorded(node) {
		return nil
	}
	if err := s.Backend.Delete(ctx, node.ID); err != nil {
		return err
	}
	slog.Info("coordinator node deleted: a refused registration returned it", "node", node.ID)
	return nil
}

// recorded reports whether any device record names this node or its machine key.
func (s *Service) recorded(node Registered) bool {
	for _, devices := range s.Store.value.Devices {
		for _, device := range devices {
			if (device.NodeID != "" && device.NodeID == node.ID) || (device.MachineKey != "" && device.MachineKey == node.MachineKey) {
				return true
			}
		}
	}
	return false
}

// A restored stale node ID must never delete a replacement belonging to another identity.
func (s *Service) deleteOwned(ctx context.Context, device Device, user string) error {
	nodes, err := s.Backend.Inventory(ctx)
	if err != nil {
		return err
	}
	_, err = s.deleteOwnedFrom(ctx, device, user, nodes)
	return err
}

// Revoked machine identities remain tombstoned so a late Headscale registration
// can be removed even if the first revocation preceded its completion. It reports
// whether it deleted any node, including when a later deletion then fails.
func (s *Service) deleteOwnedFrom(ctx context.Context, device Device, user string, nodes []Registered) (deleted bool, err error) {
	for _, node := range nodes {
		matched := device.NodeID != "" && node.ID == device.NodeID && node.Key == device.Key
		if device.NodeID == "" && device.MachineKey != "" {
			matched = node.MachineKey == device.MachineKey
		}
		if !matched || (device.MachineKey != "" && node.MachineKey != device.MachineKey) {
			continue
		}
		if node.UserID != user {
			slog.Warn("coordinator node kept: it is registered to another user", "node", node.ID, "owner", node.UserID, "household_user", user)
			continue
		}
		if err := s.Backend.Delete(ctx, node.ID); err != nil {
			return deleted, err
		}
		deleted = true
		slog.Info("coordinator node deleted", "node", node.ID)
	}
	return deleted, nil
}
