// Package client is the Orbivisor manifest API client the provider uses: POST /apply (plan or apply one
// manifest), GET/DELETE /manifests/{kind}/{key}, task polling and a few read-only lists for data sources.
package client

import (
	"bytes"
	"context"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"encoding/hex"
	"encoding/json"
	"encoding/pem"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"
	"time"
)

// APIVersion is the manifest apiVersion the server accepts.
const APIVersion = "orbivisor.io/v1"

// ErrNotFound is returned for 404 answers (the object is gone or not visible).
var ErrNotFound = errors.New("not found")

// Config is how to reach the server.
type Config struct {
	// Endpoint is the API base URL, e.g. https://node1:8006.
	Endpoint string
	// Token is an API token (Authorization: Bearer).
	Token string
	// CAPEM is the cluster CA (PEM). Empty means the system roots.
	CAPEM string
	// CAFingerprint pins the cluster CA ("sha256:<hex>"): ca_pem (or, when empty, the CA read from
	// GET /cluster/ca) must have this fingerprint.
	CAFingerprint string
	// Timeout bounds one apply or delete (waiting for its Task).
	Timeout time.Duration
	// UserAgent identifies the client in Orbivisor-Client (terraform/<version>).
	UserAgent string
}

// Client talks to one Orbivisor cluster.
type Client struct {
	base    string
	token   string
	http    *http.Client
	timeout time.Duration
	agent   string
	// Poll is the task polling interval.
	Poll time.Duration
}

// New makes a client.
func New(cfg Config) (*Client, error) {
	u, err := url.Parse(strings.TrimRight(cfg.Endpoint, "/"))
	if err != nil || u.Host == "" || (u.Scheme != "https" && u.Scheme != "http") {
		return nil, fmt.Errorf("endpoint %q is not an http(s) URL", cfg.Endpoint)
	}
	caPEM := cfg.CAPEM
	if cfg.CAFingerprint != "" {
		want, err := parseFingerprint(cfg.CAFingerprint)
		if err != nil {
			return nil, err
		}
		if caPEM == "" {
			// The cluster CA is not in any system store: read it unverified and accept it only if it has
			// the pinned fingerprint (as `orbivisorctl login` does).
			if caPEM, err = fetchCA(u.String()); err != nil {
				return nil, err
			}
		}
		got, err := Fingerprint(caPEM)
		if err != nil {
			return nil, fmt.Errorf("the cluster CA: %w", err)
		}
		if g, _ := parseFingerprint(got); !bytes.Equal(g, want) {
			return nil, fmt.Errorf("the cluster CA's fingerprint is %s, not %s (ca_fingerprint)", got, cfg.CAFingerprint)
		}
	}
	tlsConfig := &tls.Config{MinVersion: tls.VersionTLS12}
	if caPEM != "" {
		pool := x509.NewCertPool()
		if !pool.AppendCertsFromPEM([]byte(caPEM)) {
			return nil, errors.New("ca_pem holds no PEM certificate")
		}
		tlsConfig.RootCAs = pool
	}
	timeout := cfg.Timeout
	if timeout == 0 {
		timeout = 20 * time.Minute
	}
	agent := cfg.UserAgent
	if agent == "" {
		agent = "terraform/dev"
	}
	return &Client{
		base:  u.String() + "/api/v1",
		token: cfg.Token,
		http: &http.Client{
			Transport: &http.Transport{TLSClientConfig: tlsConfig, Proxy: http.ProxyFromEnvironment},
			Timeout:   2 * time.Minute,
		},
		timeout: timeout,
		agent:   agent,
		Poll:    250 * time.Millisecond,
	}, nil
}

func parseFingerprint(fp string) ([]byte, error) {
	hexPart := strings.TrimPrefix(strings.ToLower(strings.TrimSpace(fp)), "sha256:")
	hexPart = strings.ReplaceAll(hexPart, ":", "")
	b, err := hex.DecodeString(hexPart)
	if err != nil || len(b) != sha256.Size {
		return nil, fmt.Errorf("ca_fingerprint %q is not sha256:<64 hex digits>", fp)
	}
	return b, nil
}

// fetchCA reads GET /api/v1/cluster/ca without verifying the server (the caller checks the fingerprint).
func fetchCA(base string) (string, error) {
	h := &http.Client{
		Timeout: 30 * time.Second,
		Transport: &http.Transport{
			TLSClientConfig: &tls.Config{InsecureSkipVerify: true}, //nolint:gosec // pinned by fingerprint
		},
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}
	resp, err := h.Get(base + "/api/v1/cluster/ca")
	if err != nil {
		return "", fmt.Errorf("cannot read the cluster CA: %w", err)
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return "", fmt.Errorf("cannot read the cluster CA: %s", resp.Status)
	}
	b, err := io.ReadAll(io.LimitReader(resp.Body, 64<<10))
	return string(b), err
}

// Fingerprint is the "sha256:<hex>" fingerprint of the one certificate in pemText. A bundle of several
// certificates (or anything besides one CERTIFICATE block) is refused: the TLS pool trusts every
// certificate it is given, so pinning only the first would let a man in the middle append its own CA
// to the public cluster CA.
func Fingerprint(pemText string) (string, error) {
	var der []byte
	rest := []byte(pemText)
	for {
		block, r := pem.Decode(rest)
		if block == nil {
			break
		}
		rest = r
		if block.Type != "CERTIFICATE" || der != nil {
			return "", errors.New("must be exactly one PEM certificate")
		}
		der = block.Bytes
	}
	if der == nil {
		return "", errors.New("no PEM certificate")
	}
	if len(bytes.TrimSpace(rest)) != 0 {
		return "", errors.New("must be exactly one PEM certificate (trailing data)")
	}
	sum := sha256.Sum256(der)
	return "sha256:" + hex.EncodeToString(sum[:]), nil
}

// APIError is an error answer ({code, message, details}).
type APIError struct {
	Status  int             `json:"-"`
	Code    string          `json:"code"`
	Message string          `json:"message"`
	Details json.RawMessage `json:"details,omitempty"`
}

func (e *APIError) Error() string {
	if e.Code != "" {
		return fmt.Sprintf("%s (%d %s)", e.Message, e.Status, e.Code)
	}
	return fmt.Sprintf("%s (%d)", e.Message, e.Status)
}

func (c *Client) do(ctx context.Context, method, path string, body, out any) error {
	var rd io.Reader
	if body != nil {
		b, err := json.Marshal(body)
		if err != nil {
			return err
		}
		rd = bytes.NewReader(b)
	}
	req, err := http.NewRequestWithContext(ctx, method, c.base+path, rd)
	if err != nil {
		return err
	}
	if body != nil {
		req.Header.Set("Content-Type", "application/json")
	}
	req.Header.Set("Accept", "application/json")
	req.Header.Set("Orbivisor-Client", c.agent)
	if c.token != "" {
		req.Header.Set("Authorization", "Bearer "+c.token)
	}
	resp, err := c.http.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	data, err := io.ReadAll(io.LimitReader(resp.Body, 64<<20))
	if err != nil {
		return err
	}
	if resp.StatusCode == http.StatusNotFound {
		e := &APIError{Status: resp.StatusCode}
		_ = json.Unmarshal(data, e)
		return fmt.Errorf("%w: %s", ErrNotFound, e.Message)
	}
	if resp.StatusCode >= 300 {
		e := &APIError{Status: resp.StatusCode}
		if json.Unmarshal(data, e) != nil || e.Message == "" {
			e.Message = strings.TrimSpace(string(data))
		}
		return e
	}
	if out == nil || len(data) == 0 {
		return nil
	}
	// Numbers stay exact (sizes in bytes go past float64's integers).
	dec := json.NewDecoder(bytes.NewReader(data))
	dec.UseNumber()
	return dec.Decode(out)
}

// Manifest is one manifest document.
type Manifest struct {
	APIVersion string         `json:"apiVersion"`
	Kind       string         `json:"kind"`
	Metadata   Metadata       `json:"metadata"`
	Spec       map[string]any `json:"spec"`
	State      string         `json:"state,omitempty"`
}

// Metadata is a manifest's metadata.
type Metadata struct {
	Name   string            `json:"name"`
	Folder string            `json:"folder,omitempty"`
	Labels map[string]string `json:"labels,omitempty"`
}

// Problem is one step problem.
type Problem struct {
	Code    string `json:"code"`
	Message string `json:"message"`
	Path    string `json:"path,omitempty"`
	Level   string `json:"level"`
}

// Change is one changed field of a step.
type Change struct {
	Path string `json:"path"`
	Op   string `json:"op"`
	Old  any    `json:"old,omitempty"`
	New  any    `json:"new,omitempty"`
}

// Step is one plan step.
type Step struct {
	N        int       `json:"n"`
	Kind     string    `json:"kind"`
	Name     string    `json:"name"`
	Folder   string    `json:"folder,omitempty"`
	Action   string    `json:"action"`
	Diff     []Change  `json:"diff,omitempty"`
	Problems []Problem `json:"problems,omitempty"`
	Restart  bool      `json:"restart,omitempty"`
}

// Plan is a server plan (POST /apply?dry_run=true).
type Plan struct {
	Plan  string `json:"plan"`
	Steps []Step `json:"steps"`
}

// Task is the part of a Task resource the client reads.
type Task struct {
	ID     string `json:"id"`
	Status struct {
		State string  `json:"state"`
		Error *string `json:"error"`
	} `json:"status"`
}

type applyRequest struct {
	Documents      []Manifest `json:"documents,omitempty"`
	Plan           string     `json:"plan,omitempty"`
	Manager        string     `json:"manager"`
	ForceConflicts bool       `json:"force_conflicts,omitempty"`
}

type applyStarted struct {
	Plan Plan `json:"plan"`
	Task Task `json:"task"`
}

// PlanOne asks the server to plan one document without writing (permissions, references, quotas,
// capacity, MAC/IP).
func (c *Client) PlanOne(ctx context.Context, doc Manifest, force bool) (*Plan, error) {
	var plan Plan
	err := c.do(ctx, http.MethodPost, "/apply?dry_run=true",
		applyRequest{Documents: []Manifest{doc}, Manager: "terraform", ForceConflicts: force}, &plan)
	if err != nil {
		return nil, err
	}
	return &plan, nil
}

// ApplyOne plans and applies one document and waits for the apply Task. A blocked plan comes back as an
// *APIError with code plan_blocked and the plan in Details.
func (c *Client) ApplyOne(ctx context.Context, doc Manifest, force bool) (*Plan, error) {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var started applyStarted
	err := c.do(ctx, http.MethodPost, "/apply",
		applyRequest{Documents: []Manifest{doc}, Manager: "terraform", ForceConflicts: force}, &started)
	if err != nil {
		return nil, err
	}
	if err := c.WaitTask(ctx, started.Task.ID); err != nil {
		return &started.Plan, err
	}
	return &started.Plan, nil
}

// BlockedPlan returns the plan of a plan_blocked error.
func BlockedPlan(err error) *Plan {
	var e *APIError
	if !errors.As(err, &e) || e.Code != "plan_blocked" || len(e.Details) == 0 {
		return nil
	}
	var p Plan
	if json.Unmarshal(e.Details, &p) != nil {
		return nil
	}
	return &p
}

// WaitTask polls a Task until it ends; a failed Task is an error with its message.
func (c *Client) WaitTask(ctx context.Context, id string) error {
	for {
		var t Task
		if err := c.do(ctx, http.MethodGet, "/tasks/"+url.PathEscape(id), nil, &t); err != nil {
			return err
		}
		switch t.Status.State {
		case "succeeded":
			return nil
		case "failed":
			msg := "the task failed"
			if t.Status.Error != nil && *t.Status.Error != "" {
				msg = *t.Status.Error
			}
			return fmt.Errorf("task %s: %s", id, msg)
		}
		select {
		case <-ctx.Done():
			return fmt.Errorf("waiting for task %s: %w", id, ctx.Err())
		case <-time.After(c.Poll):
		}
	}
}

// GetManifest reads one object as a manifest (key: id or name; folders "tree:/path").
func (c *Client) GetManifest(ctx context.Context, kind, key string) (*Manifest, error) {
	var m Manifest
	if err := c.do(ctx, http.MethodGet, "/manifests/"+kind+"/"+url.PathEscape(key), nil, &m); err != nil {
		return nil, err
	}
	return &m, nil
}

// DeleteManifest deletes one object through its kind's delete path and waits for the Task it made (if
// any). A missing object is not an error.
func (c *Client) DeleteManifest(ctx context.Context, kind, key string) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var out struct {
		Task string `json:"task"`
	}
	err := c.do(ctx, http.MethodDelete, "/manifests/"+kind+"/"+url.PathEscape(key), nil, &out)
	if errors.Is(err, ErrNotFound) {
		return nil
	}
	if err != nil {
		return err
	}
	if out.Task != "" {
		return c.WaitTask(ctx, out.Task)
	}
	return nil
}

// Get reads any API path into out.
func (c *Client) Get(ctx context.Context, path string, out any) error {
	return c.do(ctx, http.MethodGet, path, nil, out)
}

// Send makes any API request (method, path under /api/v1, JSON body) into out.
func (c *Client) Send(ctx context.Context, method, path string, body, out any) error {
	return c.do(ctx, method, path, body, out)
}

// Untemplate turns a template VM back into a VM (DELETE /vms/{key}/template) and waits for its Task.
func (c *Client) Untemplate(ctx context.Context, vm string) error {
	ctx, cancel := context.WithTimeout(ctx, c.timeout)
	defer cancel()
	var t Task
	if err := c.do(ctx, http.MethodDelete, "/vms/"+url.PathEscape(vm)+"/template", nil, &t); err != nil {
		return err
	}
	return c.WaitTask(ctx, t.ID)
}
