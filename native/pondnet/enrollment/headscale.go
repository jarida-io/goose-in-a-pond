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

// NewHeadscale accepts only an explicit service origin and refuses redirects.
func NewHeadscale(origin, credential string) (*Headscale, error) {
	u, err := url.Parse(origin)
	if err != nil || u.Hostname() == "" || u.User != nil || u.RawQuery != "" || u.Fragment != "" || (u.Path != "" && u.Path != "/") || (u.Scheme != "https" && u.Scheme != "http") || strings.TrimSpace(credential) == "" {
		return nil, errors.New("invalid private Headscale endpoint")
	}
	return &Headscale{strings.TrimSuffix(origin, "/"), strings.TrimSpace(credential), &http.Client{Timeout: 10 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}}, nil
}
func (h *Headscale) call(ctx context.Context, method, path string, body, result any) error {
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
	if result == nil {
		return nil
	}
	return json.NewDecoder(io.LimitReader(response.Body, 1<<20)).Decode(result)
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
	if err := h.call(ctx, "GET", "/api/v1/user", nil, &accounts); err != nil {
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
	if err := h.call(ctx, "GET", "/api/v1/user", nil, &accounts); err != nil {
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

// Policy replaces the complete allowlist; an empty list denies every connection.
func (h *Headscale) Policy(ctx context.Context, rules []Rule) error {
	policy, err := json.Marshal(map[string]any{"acls": rules, "ssh": []any{}, "tagOwners": map[string]any{}})
	if err != nil {
		return err
	}
	return h.call(ctx, "PUT", "/api/v1/policy", map[string]string{"policy": string(policy)}, nil)
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
func (h *Headscale) Inventory(ctx context.Context) ([]Registered, error) {
	var response struct {
		Nodes []coordinatorNode `json:"nodes"`
	}
	if err := h.call(ctx, "GET", "/api/v1/node", nil, &response); err != nil {
		return nil, err
	}
	nodes := make([]Registered, 0, len(response.Nodes))
	for _, n := range response.Nodes {
		nodes = append(nodes, Registered{ID: n.ID, Key: n.Key, UserID: n.User.ID, Addresses: n.Addresses, Tags: n.Tags, Routes: n.Routes, MachineKey: n.MachineKey})
	}
	return nodes, nil
}
