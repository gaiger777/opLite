package client

import (
	"context"
	"encoding/json"
	"encoding/pem"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func newTLS(t *testing.T, h http.HandlerFunc) (*httptest.Server, string) {
	t.Helper()
	srv := httptest.NewTLSServer(h)
	t.Cleanup(srv.Close)
	ca := string(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw}))
	return srv, ca
}

func TestApplyWaitsForTheTaskAndSendsHeaders(t *testing.T) {
	var polls atomic.Int32
	srv, ca := newTLS(t, func(w http.ResponseWriter, r *http.Request) {
		if r.Header.Get("Authorization") != "Bearer tok" || r.Header.Get("Orbivisor-Client") != "terraform/1.2.3" {
			http.Error(w, "headers", 400)
			return
		}
		switch {
		case r.Method == "POST" && r.URL.Path == "/api/v1/apply":
			var body map[string]any
			_ = json.NewDecoder(r.Body).Decode(&body)
			if body["manager"] != "terraform" || len(body["documents"].([]any)) != 1 {
				http.Error(w, "body", 400)
				return
			}
			w.WriteHeader(202)
			_, _ = io.WriteString(w, `{"plan":{"plan":"p1","steps":[{"n":1,"kind":"Vm","name":"web","action":"create"}]},"task":{"id":"t1","status":{"state":"pending"}}}`)
		case r.URL.Path == "/api/v1/tasks/t1":
			state := "running"
			if polls.Add(1) >= 3 {
				state = "succeeded"
			}
			_, _ = io.WriteString(w, `{"id":"t1","status":{"state":"`+state+`"}}`)
		default:
			http.NotFound(w, r)
		}
	})
	c, err := New(Config{Endpoint: srv.URL, Token: "tok", CAPEM: ca, UserAgent: "terraform/1.2.3"})
	if err != nil {
		t.Fatal(err)
	}
	c.Poll = time.Millisecond
	plan, err := c.ApplyOne(context.Background(), Manifest{APIVersion: APIVersion, Kind: "Vm", Metadata: Metadata{Name: "web"}}, false)
	if err != nil {
		t.Fatal(err)
	}
	if plan.Steps[0].Action != "create" || polls.Load() != 3 {
		t.Fatalf("plan %+v polls %d", plan, polls.Load())
	}
}

func TestFailedTaskAndBlockedPlan(t *testing.T) {
	srv, ca := newTLS(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v1/apply":
			if r.URL.Query().Get("dry_run") == "true" {
				_, _ = io.WriteString(w, `{"plan":"p","steps":[{"n":1,"kind":"Vm","name":"web","action":"blocked","problems":[{"code":"quota_exceeded","message":"over","level":"error"}]}]}`)
				return
			}
			w.WriteHeader(409)
			_, _ = io.WriteString(w, `{"code":"plan_blocked","message":"1 step(s) are blocked","details":{"plan":"p","steps":[{"n":1,"kind":"Vm","name":"web","action":"blocked","problems":[{"code":"forbidden","message":"no","level":"error"}]}]}}`)
		case "/api/v1/manifests/Vm/web":
			w.WriteHeader(404)
			_, _ = io.WriteString(w, `{"code":"not_found","message":"Vm \"web\" not found"}`)
		case "/api/v1/manifests/Folder/vm:/prod":
			_, _ = io.WriteString(w, `{"apiVersion":"orbivisor.io/v1","kind":"Folder","metadata":{"name":"prod"},"spec":{"kind":"vm","quota":{"memory_mib":9007199254740993}}}`)
		}
	})
	c, err := New(Config{Endpoint: srv.URL, CAPEM: ca})
	if err != nil {
		t.Fatal(err)
	}
	ctx := context.Background()
	p, err := c.PlanOne(ctx, Manifest{Kind: "Vm"}, false)
	if err != nil || p.Steps[0].Problems[0].Code != "quota_exceeded" {
		t.Fatalf("%+v %v", p, err)
	}
	_, err = c.ApplyOne(ctx, Manifest{Kind: "Vm"}, false)
	blocked := BlockedPlan(err)
	if blocked == nil || blocked.Steps[0].Problems[0].Code != "forbidden" {
		t.Fatalf("blocked %v (%v)", blocked, err)
	}
	if _, err := c.GetManifest(ctx, "Vm", "web"); !errors.Is(err, ErrNotFound) {
		t.Fatalf("404: %v", err)
	}
	// The key is escaped as one path segment; numbers stay exact.
	m, err := c.GetManifest(ctx, "Folder", "vm:/prod")
	if err != nil {
		t.Fatal(err)
	}
	q := m.Spec["quota"].(map[string]any)["memory_mib"].(json.Number)
	if q.String() != "9007199254740993" {
		t.Fatalf("number %v", q)
	}
	// Deleting what is gone is fine.
	if err := c.DeleteManifest(ctx, "Vm", "web"); err != nil {
		t.Fatal(err)
	}
}

func TestFingerprintPin(t *testing.T) {
	var srv *httptest.Server
	srv, ca := newTLS(t, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/api/v1/cluster/ca" {
			_, _ = w.Write(pem.EncodeToMemory(&pem.Block{Type: "CERTIFICATE", Bytes: srv.Certificate().Raw}))
			return
		}
		_, _ = io.WriteString(w, `{"items":[]}`)
	})
	fp, err := Fingerprint(ca)
	if err != nil || !strings.HasPrefix(fp, "sha256:") {
		t.Fatal(fp, err)
	}
	c, err := New(Config{Endpoint: srv.URL, CAFingerprint: fp})
	if err != nil {
		t.Fatal(err)
	}
	if err := c.Get(context.Background(), "/nodes", &map[string]any{}); err != nil {
		t.Fatal(err)
	}
	wrong := "sha256:" + strings.Repeat("00", 32)
	if _, err := New(Config{Endpoint: srv.URL, CAFingerprint: wrong}); err == nil || !strings.Contains(err.Error(), "fingerprint") {
		t.Fatalf("wrong fingerprint accepted: %v", err)
	}
	if _, err := New(Config{Endpoint: srv.URL, CAFingerprint: "nope"}); err == nil {
		t.Fatal("bad fingerprint accepted")
	}
	// Without the CA the server is not trusted.
	c, _ = New(Config{Endpoint: srv.URL})
	if err := c.Get(context.Background(), "/nodes", nil); err == nil {
		t.Fatal("untrusted server accepted")
	}
	if _, err := New(Config{Endpoint: "ftp://x"}); err == nil {
		t.Fatal("bad endpoint accepted")
	}
}
