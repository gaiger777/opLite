package provider

import (
	"bufio"
	"context"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"os/exec"
	"testing"
	"time"

	"github.com/hashicorp/terraform-plugin-framework/providerserver"
	"github.com/hashicorp/terraform-plugin-go/tfprotov6"

	"github.com/gaiger777/opLite/terraform/internal/client"
)

// Acceptance tests (TF_ACC=1) run real terraform — or tofu with TF_ACC_TERRAFORM_PATH and
// TF_ACC_PROVIDER_HOST=registry.opentofu.org — against opl-devserver (OPL_DEVSERVER = its path:
// `cargo build -p opld --features devserver --bin opl-devserver`), which TestMain starts once. With
// OPL_SERVER/OPL_TOKEN/OPL_CA_PEM already set the tests use that server instead.

var factories = map[string]func() (tfprotov6.ProviderServer, error){
	"orbivisor": providerserver.NewProtocol6WithError(New("test")()),
}

// dev is the server the acceptance tests use (also for changes "outside Terraform").
var dev *client.Client

func TestMain(m *testing.M) {
	if os.Getenv("TF_ACC") == "" {
		os.Exit(m.Run())
	}
	stop := func() {}
	if os.Getenv("OPL_SERVER") == "" {
		var err error
		if stop, err = startDevserver(); err != nil {
			fmt.Fprintln(os.Stderr, "opl-devserver:", err)
			os.Exit(1)
		}
	}
	c, err := client.New(client.Config{
		Endpoint: os.Getenv("OPL_SERVER"),
		Token:    os.Getenv("OPL_TOKEN"),
		CAPEM:    os.Getenv("OPL_CA_PEM"),
	})
	if err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
	dev = c
	code := m.Run()
	stop()
	os.Exit(code)
}

// startDevserver runs OPL_DEVSERVER, reads its connection line and exports it as the provider's
// environment. The server ends when its standard input closes (also when this process dies).
func startDevserver() (func(), error) {
	bin := os.Getenv("OPL_DEVSERVER")
	if bin == "" {
		return nil, fmt.Errorf("set OPL_DEVSERVER to the opl-devserver binary (cargo build -p opld --features devserver --bin opl-devserver)")
	}
	cmd := exec.Command(bin, "--exit-on-stdin-eof")
	stdin, err := cmd.StdinPipe()
	if err != nil {
		return nil, err
	}
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}
	if os.Getenv("OPL_DEVSERVER_LOG") != "" {
		cmd.Stderr = os.Stderr
	}
	if err := cmd.Start(); err != nil {
		return nil, err
	}
	line := make(chan string, 1)
	go func() {
		r := bufio.NewReader(stdout)
		s, _ := r.ReadString('\n')
		line <- s
		_, _ = io.Copy(io.Discard, r)
	}()
	var info struct {
		URL   string `json:"url"`
		Token string `json:"token"`
		CA    string `json:"ca_pem"`
	}
	select {
	case s := <-line:
		if err := json.Unmarshal([]byte(s), &info); err != nil {
			_ = cmd.Process.Kill()
			return nil, fmt.Errorf("bad connection line %q: %w", s, err)
		}
	case <-time.After(2 * time.Minute):
		_ = cmd.Process.Kill()
		return nil, fmt.Errorf("no connection line after 2 minutes")
	}
	for k, v := range map[string]string{"OPL_SERVER": info.URL, "OPL_TOKEN": info.Token, "OPL_CA_PEM": info.CA} {
		if err := os.Setenv(k, v); err != nil {
			return nil, err
		}
	}
	return func() {
		_ = stdin.Close()
		done := make(chan struct{})
		go func() { _ = cmd.Wait(); close(done) }()
		select {
		case <-done:
		case <-time.After(30 * time.Second):
			_ = cmd.Process.Kill()
		}
	}, nil
}

// send is a request outside Terraform (a change someone made in the web UI).
func send(t *testing.T, method, path string, body any) {
	t.Helper()
	if err := dev.Send(context.Background(), method, path, body, nil); err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
}
