package enrollment

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// Headscale owns the private administrative credential. Never expose it through client APIs.
type Headscale struct {
	origin     string
	credential string
	client     *http.Client
}

// responseHeaderTimeout bounds the wait for a coordinator's response headers. It is
// not a deadline for the whole call: the inventory body can run to about 33 MB, and
// reading it is bounded by the caller's context deadline instead.
const responseHeaderTimeout = 10 * time.Second

// NewHeadscale accepts only an explicit service origin and refuses redirects. Every
// call must carry a context with a deadline, which is what bounds reading a body.
func NewHeadscale(origin, credential string) (*Headscale, error) {
	u, err := url.Parse(origin)
	if err != nil || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") || (u.Scheme != "https" && u.Scheme != "http") || strings.TrimSpace(credential) == "" {
		return nil, errors.New("invalid private Headscale endpoint")
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.ResponseHeaderTimeout = responseHeaderTimeout
	client := &http.Client{Transport: transport, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	return &Headscale{strings.TrimSuffix(origin, "/"), strings.TrimSpace(credential), client}, nil
}

// MaxNodes bounds the coordinator inventory this service will read. Enrollment stops at
// EnrollmentCapacity, below it, so the inventory never reaches the bound through this
// service; the difference leaves room for nodes an operator adds by hand.
const MaxNodes = 8192

// EnrollmentCapacity is the most devices this service enrolls across every household.
const EnrollmentCapacity = 4096

// ErrInventoryTooLarge means the coordinator holds more nodes than MaxNodes.
var ErrInventoryTooLarge = errors.New("coordinator inventory exceeds the supported size")

func (h *Headscale) call(ctx context.Context, method, path string, body, result any) error {
	return h.do(ctx, method, path, body, func(r io.Reader) error {
		if result == nil {
			return nil
		}
		return json.NewDecoder(io.LimitReader(r, 1<<20)).Decode(result)
	})
}

func (h *Headscale) do(ctx context.Context, method, path string, body any, read func(io.Reader) error) error {
	data, err := json.Marshal(body)
	if err != nil {
		return err
	}
	r, err := http.NewRequestWithContext(ctx, method, h.origin+path, bytes.NewReader(data))
	if err != nil {
		return err
	}
	r.Header.Set("Authorization", "Bearer "+h.credential)
	r.Header.Set("Content-Type", "application/json")
	response, err := h.client.Do(r)
	if err != nil {
		return errors.New("coordinator unavailable")
	}
	defer response.Body.Close()
	// Deletion is idempotent after a lost response or a coordinator restart.
	if method == "DELETE" && response.StatusCode == http.StatusNotFound {
		return nil
	}
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		if method == "POST" && path == "/api/v1/auth/register" && response.StatusCode >= 400 && response.StatusCode < 500 {
			return ErrRegistrationRejected
		}
		return fmt.Errorf("coordinator rejected operation: HTTP %d", response.StatusCode)
	}
	return read(response.Body)
}

// EnsureUser returns the numeric ID of the household's user, creating it if this
// is the household's first registration.
//
// Look-up comes first and creation is tolerant of an existing name, so a lost
// response cannot strand a household: the next attempt finds the user the
// previous one created rather than failing on a duplicate.
func (h *Headscale) EnsureUser(ctx context.Context, name string) (string, error) {
	if !userNamePattern.MatchString(name) {
		return "", errors.New("invalid household user")
	}
	existing, err := h.findUser(ctx, name)
	if err != nil || existing != "" {
		return existing, err
	}
	var created struct {
		User struct {
			ID string `json:"id"`
		} `json:"user"`
	}
	if err := h.call(ctx, "POST", "/api/v1/user", map[string]string{"name": name}, &created); err != nil {
		// The name may have been taken by a concurrent or previously lost attempt.
		if again, lookupErr := h.findUser(ctx, name); lookupErr == nil && again != "" {
			return again, nil
		}
		return "", err
	}
	if !numeric.MatchString(created.User.ID) {
		return "", errors.New("coordinator returned an invalid household user")
	}
	return created.User.ID, nil
}

func (h *Headscale) findUser(ctx context.Context, name string) (string, error) {
	var accounts struct {
		Users []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		} `json:"users"`
	}
	// Filtered by the coordinator, so the answer is one user however many households exist.
	if err := h.call(ctx, "GET", "/api/v1/user?name="+url.QueryEscape(name), nil, &accounts); err != nil {
		return "", err
	}
	for _, account := range accounts.Users {
		if account.Name == name {
			return account.ID, nil
		}
	}
	return "", nil
}

// Register approves the pending auth ID under the operator-provisioned user.
func (h *Headscale) Register(ctx context.Context, user, auth string) (Registered, error) {
	// AuthRegister accepts a user name, not the numeric database ID.
	var accounts struct {
		Users []struct {
			ID   string `json:"id"`
			Name string `json:"name"`
		} `json:"users"`
	}
	if !numeric.MatchString(user) {
		return Registered{}, fmt.Errorf("%w: invalid household user", ErrRegistrationRejected)
	}
	if err := h.call(ctx, "GET", "/api/v1/user?id="+user, nil, &accounts); err != nil {
		return Registered{}, fmt.Errorf("%w: household lookup unavailable", ErrRegistrationRejected)
	}
	name := ""
	for _, account := range accounts.Users {
		if account.ID == user {
			name = account.Name
			break
		}
	}
	if name == "" {
		return Registered{}, fmt.Errorf("%w: household user missing", ErrRegistrationRejected)
	}
	var response struct {
		Node coordinatorNode `json:"node"`
	}
	if err := h.call(ctx, "POST", "/api/v1/auth/register", map[string]string{"user": name, "authId": auth}, &response); err != nil {
		return Registered{}, err
	}
	n := response.Node
	return Registered{ID: n.ID, Key: n.Key, UserID: n.User.ID, Addresses: n.Addresses, Tags: n.Tags, Routes: n.Routes, MachineKey: n.MachineKey}, nil
}

// Delete removes only the node ID previously stored for an authenticated household.
func (h *Headscale) Delete(ctx context.Context, id string) error {
	if !numeric.MatchString(id) {
		return errors.New("invalid node")
	}
	return h.call(ctx, "DELETE", "/api/v1/node/"+id, nil, nil)
}

// policyDocument is the complete coordinator policy for these rules and attributes:
// exactly what Policy writes, and what CurrentPolicy is compared against.
func policyDocument(rules []Rule, attrs []NodeAttr) ([]byte, error) {
	return json.Marshal(map[string]any{"acls": rules, "nodeAttrs": attrs, "ssh": []any{}, "tagOwners": map[string]any{}})
}

// Policy replaces the complete allowlist and node attributes; an empty rule list
// denies every connection, and an empty attribute list grants no capability.
func (h *Headscale) Policy(ctx context.Context, rules []Rule, attrs []NodeAttr) error {
	policy, err := policyDocument(rules, attrs)
	if err != nil {
		return err
	}
	return h.call(ctx, "PUT", "/api/v1/policy", map[string]string{"policy": string(policy)}, nil)
}

// CurrentPolicy returns the policy document the coordinator holds. Headscale answers
// `GET /api/v1/policy` with `{"policy": "<document>", "updatedAt": ...}`, the document
// being the text last written, kept verbatim; only the document is read.
func (h *Headscale) CurrentPolicy(ctx context.Context) ([]byte, error) {
	var response struct {
		Policy string `json:"policy"`
	}
	if err := h.call(ctx, "GET", "/api/v1/policy", nil, &response); err != nil {
		return nil, err
	}
	return []byte(response.Policy), nil
}

type coordinatorNode struct {
	ID         string   `json:"id"`
	Key        string   `json:"nodeKey"`
	MachineKey string   `json:"machineKey"`
	Addresses  []string `json:"ipAddresses"`
	Tags       []string `json:"tags"`
	Routes     []string `json:"approvedRoutes"`
	User       struct {
		ID string `json:"id"`
	} `json:"user"`
}

// Inventory verifies durable permissions against the current coordinator database.
//
// Nodes are decoded one at a time and counted, never buffered as one document: a size cap
// on the whole body is what let enough registered nodes stop every policy update.
func (h *Headscale) Inventory(ctx context.Context) ([]Registered, error) {
	var nodes []Registered
	err := h.do(ctx, "GET", "/api/v1/node", nil, func(body io.Reader) error {
		decoded, err := decodeNodes(body, MaxNodes)
		nodes = decoded
		return err
	})
	return nodes, err
}

// decodeNodes reads `{"nodes": [...]}`, refusing more than limit nodes.
func decodeNodes(body io.Reader, limit int) ([]Registered, error) {
	// A generous per-node allowance; the count, not this, is the real bound.
	decoder := json.NewDecoder(io.LimitReader(body, int64(limit+1)*4096))
	if err := expect(decoder, json.Delim('{')); err != nil {
		return nil, err
	}
	nodes := []Registered{}
	for decoder.More() {
		key, err := decoder.Token()
		if err != nil {
			return nil, err
		}
		if key != "nodes" {
			var skipped json.RawMessage
			if err := decoder.Decode(&skipped); err != nil {
				return nil, err
			}
			continue
		}
		if err := expect(decoder, json.Delim('[')); err != nil {
			return nil, err
		}
		for decoder.More() {
			if len(nodes) == limit {
				return nil, ErrInventoryTooLarge
			}
			var n coordinatorNode
			if err := decoder.Decode(&n); err != nil {
				return nil, err
			}
			nodes = append(nodes, Registered{ID: n.ID, Key: n.Key, UserID: n.User.ID, Addresses: n.Addresses, Tags: n.Tags, Routes: n.Routes, MachineKey: n.MachineKey})
		}
		if err := expect(decoder, json.Delim(']')); err != nil {
			return nil, err
		}
	}
	return nodes, expect(decoder, json.Delim('}'))
}

func expect(decoder *json.Decoder, want json.Delim) error {
	token, err := decoder.Token()
	if err != nil {
		return err
	}
	if token != want {
		return fmt.Errorf("coordinator inventory: expected %v", want)
	}
	return nil
}
