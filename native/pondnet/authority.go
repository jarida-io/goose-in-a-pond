package pondnet

import (
	"bytes"
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"encoding/base64"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/enrollment"
	"github.com/Exile10/goose-in-a-pond/native/pondnet/internal/privatefile"
	"golang.org/x/sys/unix"
	"io"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strings"
	"time"
)

// Authority is independent of the Pond TLS key and its WireGuard identity.
type Authority struct {
	Household string `json:"household"`
	PublicKey string `json:"publicKey"`
	key       ed25519.PrivateKey
}

// LoadAuthority creates only on explicit first activation; a corrupt identity never rotates itself.
func LoadAuthority(directory string) (Authority, error) {
	var out Authority
	if !filepath.IsAbs(directory) {
		return out, errors.New("authority directory must be absolute")
	}
	if err := createAuthority(directory); err != nil {
		return out, err
	}
	info, err := os.Lstat(directory)
	if err != nil {
		return out, err
	}
	if !info.IsDir() || info.Mode().Perm()&0077 != 0 {
		return out, errors.New("authority directory is not private")
	}
	fd, err := unix.Open(filepath.Join(directory, "identity.lock"), unix.O_CREAT|unix.O_RDWR|unix.O_NOFOLLOW, 0600)
	if err != nil {
		return out, err
	}
	lock := os.NewFile(uintptr(fd), "identity.lock")
	defer lock.Close()
	if err = unix.Flock(fd, unix.LOCK_EX|unix.LOCK_NB); err != nil {
		return out, err
	}
	file, err := privatefile.Open(filepath.Join(directory, "identity.json"), 4096)
	if errors.Is(err, os.ErrNotExist) {
		return out, errors.New("authority identity is missing; restore its backup")
	}
	if err != nil {
		return out, errors.New("invalid authority identity")
	}
	defer file.Close()
	var stored struct {
		Seed string `json:"seed"`
	}
	decoder := json.NewDecoder(io.LimitReader(file, 4097))
	decoder.DisallowUnknownFields()
	if err = decoder.Decode(&stored); err != nil {
		return out, err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return out, errors.New("trailing identity data")
	}
	seed, err := base64.StdEncoding.DecodeString(stored.Seed)
	if err != nil || len(seed) != ed25519.SeedSize {
		return out, errors.New("invalid authority seed")
	}
	out.key = ed25519.NewKeyFromSeed(seed)
	public := out.key.Public().(ed25519.PublicKey)
	digest := sha256.Sum256(public)
	out.Household = hex.EncodeToString(digest[:16])
	out.PublicKey = base64.StdEncoding.EncodeToString(public)
	return out, nil
}

// createAuthority makes a household identity only when the directory does not exist. It
// is built in a staging directory beside the final one and renamed into place, so a crash
// part-way leaves nothing or a whole identity. It used to create the directory first, and
// a crash before the key was written left an empty directory every later start refused.
func createAuthority(directory string) error {
	if _, err := os.Lstat(directory); !errors.Is(err, os.ErrNotExist) {
		return err
	}
	parent, base := filepath.Dir(directory), filepath.Base(directory)
	// Earlier attempts that crashed before their rename.
	if stale, err := filepath.Glob(filepath.Join(parent, base+".new-*")); err == nil {
		for _, leftover := range stale {
			os.RemoveAll(leftover)
		}
	}
	staging, err := os.MkdirTemp(parent, base+".new-")
	if err != nil {
		return err
	}
	defer os.RemoveAll(staging)
	if err = os.Chmod(staging, 0700); err != nil {
		return err
	}
	seed := make([]byte, ed25519.SeedSize)
	if _, err = rand.Read(seed); err != nil {
		return err
	}
	data, _ := json.Marshal(struct {
		Seed string `json:"seed"`
	}{base64.StdEncoding.EncodeToString(seed)})
	if err = writeSynced(filepath.Join(staging, "identity.json"), data); err != nil {
		return err
	}
	if err = syncDirectory(staging); err != nil {
		return err
	}
	if err = os.Rename(staging, directory); err != nil {
		// Another process created it first; use theirs.
		if _, statErr := os.Lstat(directory); statErr == nil {
			return nil
		}
		return err
	}
	return syncDirectory(parent)
}

func writeSynced(path string, data []byte) error {
	f, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		return err
	}
	_, err = f.Write(data)
	if err == nil {
		err = f.Sync()
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	return err
}

func syncDirectory(path string) error {
	dir, err := os.Open(path)
	if err != nil {
		return err
	}
	defer dir.Close()
	return dir.Sync()
}

// Register introduces this household to the enrollment service. A household the
// service has not seen must present an invite from whoever runs it; one already
// registered needs none.
//
// It proves possession of the household key, and the invite proves the operator
// agreed to serve it. The coordinator's policy grants each phone its own Pond and
// nothing else, so a household that is not yours gives you no reach into one that
// is. The service names the household from the key rather than trusting what is
// sent, so this cannot claim another household. Whoever runs the service can neither
// read household data nor use a Pond; it can deny or disrupt remote access.
//
// It is safe to repeat: the service answers with the same household rather than
// conflicting, which is how a lost response is recovered.
//
// A Pond provisioned with a device certificate sends a proof bound to this registration,
// which admits a new household without an invite; device is nil on one that was not.
func (a Authority) Register(ctx context.Context, origin string, port uint16, invite string, device *Device) (string, error) {
	u, e := url.Parse(origin)
	if e != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return "", errors.New("enrollment requires an HTTPS origin")
	}
	if port == 0 {
		return "", errors.New("a companion port is required")
	}
	envelope, e := enrollment.SignHousehold(a.registration(port, invite, device, time.Now()), a.key)
	if e != nil {
		return "", e
	}
	body, _ := json.Marshal(envelope)
	request, e := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(origin, "/")+"/v1/household", bytes.NewReader(body))
	if e != nil {
		return "", e
	}
	request.Header.Set("Content-Type", "application/json")
	response, e := coordinatorClient().Do(request)
	if e != nil {
		return "", fmt.Errorf("enrollment service unavailable: %w", e)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return "", &Refused{Status: response.StatusCode, Reason: refusal(response.Body), Action: "registration"}
	}
	var result struct {
		Household string `json:"household"`
	}
	if e = json.NewDecoder(io.LimitReader(response.Body, 4096)).Decode(&result); e != nil {
		return "", e
	}
	// The service must have named the household this key owns; anything else
	// means we are not talking to the coordinator we think we are.
	if result.Household != a.Household {
		return "", errors.New("coordinator named a different household")
	}
	return result.Household, nil
}

// registration is what Register signs: this household's key and companion port, any
// invite, and, on a provisioned Pond, a device proof bound to this household and expiry.
func (a Authority) registration(port uint16, invite string, device *Device, now time.Time) enrollment.HouseholdRegistration {
	registration := enrollment.HouseholdRegistration{
		PublicKey: a.PublicKey,
		Port:      port,
		Expires:   now.Add(2 * time.Minute).Unix(),
		Invite:    invite,
	}
	if device != nil {
		registration.Device = device.Prove(registration.PublicKey, registration.Expires)
	}
	return registration
}

// Submit sends a narrowly scoped approval to the configured enrollment origin.
// Callers must authorize the device locally before invoking this operation.
func (a Authority) Submit(ctx context.Context, origin string, approval enrollment.Approval) (enrollment.Device, error) {
	var result enrollment.Device
	u, e := url.Parse(origin)
	if e != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return result, errors.New("enrollment requires an HTTPS origin")
	}
	approval.Household = a.Household
	nonce := make([]byte, 24)
	if _, e = rand.Read(nonce); e != nil {
		return result, e
	}
	approval.Nonce = base64.RawURLEncoding.EncodeToString(nonce)
	approval.Expires = time.Now().Add(2 * time.Minute).Unix()
	envelope, e := enrollment.Sign(approval, a.key)
	if e != nil {
		return result, e
	}
	body, _ := json.Marshal(envelope)
	request, e := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(origin, "/")+"/v1/approval", bytes.NewReader(body))
	if e != nil {
		return result, e
	}
	request.Header.Set("Content-Type", "application/json")
	response, e := coordinatorClient().Do(request)
	if e != nil {
		return result, fmt.Errorf("enrollment service unavailable: %w", e)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		// The status is the whole diagnosis: 401 and 403 mean the coordinator
		// rejected this household's signature, 404 that the route is not
		// deployed, 429 that it is shedding load, 5xx that it is unwell. Losing
		// it left "enrollment was not completed", which names the outcome
		// everybody already knew and none of the causes.
		return result, &Refused{Status: response.StatusCode, Reason: refusal(response.Body), Action: approval.Action}
	}
	return decodeAnswer(response.Body, approval)
}

// ReportStatus sends one signed report about this Pond to the enrollment service and
// returns the service's answer: what it now believes about the Pond, including the outage
// this report ended, if any. The household and the expiry are set here; the caller sets
// the rest.
func (a Authority) ReportStatus(ctx context.Context, origin string, report enrollment.PondReport) (enrollment.PondStatus, error) {
	var result enrollment.PondStatus
	u, e := url.Parse(origin)
	if e != nil || u.Scheme != "https" || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") {
		return result, errors.New("enrollment requires an HTTPS origin")
	}
	report.Household = a.Household
	report.Expires = time.Now().Add(2 * time.Minute).Unix()
	envelope, e := enrollment.SignStatus(report, a.key)
	if e != nil {
		return result, e
	}
	body, _ := json.Marshal(envelope)
	request, e := http.NewRequestWithContext(ctx, "POST", strings.TrimSuffix(origin, "/")+"/v1/pond/status", bytes.NewReader(body))
	if e != nil {
		return result, e
	}
	request.Header.Set("Content-Type", "application/json")
	response, e := coordinatorClient().Do(request)
	if e != nil {
		return result, fmt.Errorf("enrollment service unavailable: %w", e)
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return result, &Refused{Status: response.StatusCode, Reason: refusal(response.Body), Action: "status"}
	}
	decoder := json.NewDecoder(io.LimitReader(response.Body, 4096))
	if e = decoder.Decode(&result); e != nil {
		return result, e
	}
	return result, nil
}

// Grant signs a read grant for one of this household's phones, naming the device as the
// enrollment service knows it, valid for lifetime. The phone presents it to read its
// Pond's status while the Pond cannot answer; the service honours it only while that
// device is an active phone of this household.
func (a Authority) Grant(device string, lifetime time.Duration, now time.Time) (string, int64, error) {
	if len(device) < 16 || len(device) > 80 || strings.ContainsFunc(device, func(r rune) bool {
		return !(r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-' || r == '_')
	}) {
		return "", 0, errors.New("invalid device")
	}
	if lifetime <= 0 || lifetime > enrollment.MaxGrantLifetime-24*time.Hour {
		return "", 0, errors.New("invalid grant lifetime")
	}
	expires := now.Add(lifetime).Unix()
	token, err := enrollment.SignGrant(enrollment.StatusGrant{Household: a.Household, Device: device, Expires: expires}, a.key)
	return token, expires, err
}

// decodeAnswer reads the coordinator's account of a device and checks it against what
// was asked. Fields this helper does not know are ignored, so the service can add one
// without breaking Ponds already in homes; what protects the answer is its size bound,
// the refusal of trailing data, and answered(), which checks the status, role, revision
// and machine key against the request.
func decodeAnswer(body io.Reader, approval enrollment.Approval) (enrollment.Device, error) {
	var result enrollment.Device
	decoder := json.NewDecoder(io.LimitReader(body, 4096))
	if err := decoder.Decode(&result); err != nil {
		return result, err
	}
	if decoder.Decode(new(any)) != io.EOF {
		return result, errors.New("coordinator answered with trailing data")
	}
	return result, answered(approval, result)
}

// answered checks the coordinator's account of the device against what was asked, so a
// proxy or a confused service cannot report a different device as this one's outcome.
func answered(approval enrollment.Approval, device enrollment.Device) error {
	known := map[string]bool{"pending": true, "active": true, "revoking": true, "revoked": true, "failed": true}
	if !known[device.Status] || (device.Role != "pond" && device.Role != "phone") {
		return errors.New("coordinator answered with an unknown device state")
	}
	if len(device.Revision) > 80 || strings.ContainsFunc(device.Revision, func(r rune) bool {
		return !(r >= 'A' && r <= 'Z' || r >= 'a' && r <= 'z' || r >= '0' && r <= '9' || r == '-' || r == '_')
	}) {
		return errors.New("coordinator answered with an invalid revision")
	}
	switch approval.Action {
	case "enroll", "replace":
		if device.Role != approval.Role || device.MachineKey != approval.MachineKey {
			return errors.New("coordinator answered for a different device")
		}
	case "revoke":
		if device.Status != "revoked" {
			return errors.New("coordinator did not confirm the revocation")
		}
	}
	return nil
}

// coordinatorClient ignores proxy settings in the environment: the helper talks to the
// coordinator directly or not at all. Redirects are not followed.
func coordinatorClient() *http.Client {
	return &http.Client{
		Timeout:       20 * time.Second,
		Transport:     &http.Transport{Proxy: nil, TLSHandshakeTimeout: 10 * time.Second, ForceAttemptHTTP2: true},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
}

// refusal reads the coordinator's own account of why it said no.
//
// Its errors are a closed set of identifiers - approval_used,
// identity_already_enrolled, pond_registration_required and the rest - and each
// one calls for something different. The status alone cannot tell them apart:
// this service has six distinct 409s. Bounded, because it is a remote party's
// bytes going into a local log.
func refusal(body io.Reader) string {
	var answer struct {
		Error string `json:"error"`
	}
	raw, e := io.ReadAll(io.LimitReader(body, 512))
	if e != nil || len(raw) == 0 {
		return "with no explanation"
	}
	if json.Unmarshal(raw, &answer) == nil && answer.Error != "" {
		return answer.Error
	}
	// Not the shape we expect, so report that rather than nothing - a proxy
	// answering in place of the coordinator looks exactly like this.
	return "an unrecognised answer"
}

// Refused is a coordinator answer that is a decision, not a fault: the request
// reached it, it understood it, and it said no. The status and the service's
// own identifier are carried so a caller can act on WHICH no it was -- an
// already-enrolled device wants the replacement flow, an unreachable
// coordinator wants a retry, and telling a user the wrong one sends them to
// the wrong control.
type Refused struct {
	Status int
	Reason string
	Action string
}

func (r *Refused) Error() string {
	return fmt.Sprintf("the coordinator answered %d to %s: %s", r.Status, r.Action, r.Reason)
}
