package provider

import (
	"context"
	"encoding/base64"
	"errors"
	"fmt"
	"regexp"
	"strings"
	"testing"

	"github.com/hashicorp/terraform-plugin-testing/helper/resource"
	"github.com/hashicorp/terraform-plugin-testing/plancheck"
	"github.com/hashicorp/terraform-plugin-testing/terraform"

	"github.com/gaiger777/opLite/terraform/internal/client"
)

const providerBlock = "provider \"orbivisor\" {}\n"

// manifest reads an object as the server sees it.
func manifest(kind, key string) (*client.Manifest, error) {
	return dev.GetManifest(context.Background(), kind, key)
}

// gone checks that the objects no longer exist (CheckDestroy).
func gone(objs ...[2]string) resource.TestCheckFunc {
	return func(*terraform.State) error {
		for _, o := range objs {
			if _, err := manifest(o[0], o[1]); !errors.Is(err, client.ErrNotFound) {
				return fmt.Errorf("%s %s still exists (%v)", o[0], o[1], err)
			}
		}
		return nil
	}
}

// noSecretInState checks that no attribute of any resource holds the secret.
func noSecretInState(secret string) resource.TestCheckFunc {
	return func(s *terraform.State) error {
		for name, r := range s.RootModule().Resources {
			for k, v := range r.Primary.Attributes {
				if strings.Contains(v, secret) {
					return fmt.Errorf("%s.%s holds the write-only secret", name, k)
				}
			}
		}
		return nil
	}
}

func specIs(kind, key, field string, want any) resource.TestCheckFunc {
	return func(*terraform.State) error {
		m, err := manifest(kind, key)
		if err != nil {
			return err
		}
		if got := fmt.Sprint(m.Spec[field]); got != fmt.Sprint(want) {
			return fmt.Errorf("%s %s: %s = %s, want %v", kind, key, field, got, want)
		}
		return nil
	}
}

func vmConfig(memory int) string {
	return providerBlock + fmt.Sprintf(`
resource "orbivisor_folder" "f" {
  name  = "acc-vm"
  kind  = "vm"
  quota = { vcpus = 8 }
}

resource "orbivisor_volume" "root" {
  name       = "acc-web01-root"
  pool       = "local"
  size_bytes = 1048576
}

resource "orbivisor_security_group" "sg" {
  name        = "acc-web"
  description = "web"
  rules       = []
  labels      = { tier = "web" }
}

resource "orbivisor_vm" "web" {
  name       = "acc-web01"
  folder     = "/${orbivisor_folder.f.name}"
  state      = "running"
  cpu        = { cores = 1 }
  memory_mib = %d
  machine    = "virt"
  disks      = [{ volume = orbivisor_volume.root.name, bus = "virtio-blk" }]
  nics       = [{ network = "user", model = "virtio" }]
  cloud_init = { user = "rocky" }

  cloud_init_password_wo         = "hunter2-acc-secret"
  cloud_init_password_wo_version = 1
}
`, memory)
}

// Create, update one field, import (by name and by id), a change made outside Terraform shows in the
// plan and is put back, an object deleted outside Terraform is created again, destroy. The write-only
// password reaches the server and never the state.
func TestAccVmLifecycle(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy: gone([2]string{"Vm", "acc-web01"}, [2]string{"Volume", "acc-web01-root"},
			[2]string{"SecurityGroup", "acc-web"}, [2]string{"Folder", "vm:/acc-vm"}),
		Steps: []resource.TestStep{
			{
				Config: vmConfig(256),
				Check: resource.ComposeAggregateTestCheckFunc(
					resource.TestCheckResourceAttr("orbivisor_folder.f", "id", "vm:/acc-vm"),
					resource.TestCheckResourceAttr("orbivisor_vm.web", "id", "acc-web01"),
					resource.TestCheckResourceAttr("orbivisor_vm.web", "folder", "/acc-vm"),
					resource.TestCheckNoResourceAttr("orbivisor_vm.web", "cloud_init_password_wo"),
					noSecretInState("hunter2-acc-secret"),
					func(*terraform.State) error {
						m, err := manifest("Vm", "acc-web01")
						if err != nil {
							return err
						}
						if m.State != "running" || m.Metadata.Folder != "/acc-vm" {
							return fmt.Errorf("state %q folder %q", m.State, m.Metadata.Folder)
						}
						ci, _ := m.Spec["cloud_init"].(map[string]any)
						if _, ok := ci["password"].(map[string]any); !ok {
							return fmt.Errorf("the server has no password: %v", m.Spec["cloud_init"])
						}
						return nil
					},
				),
			},
			{
				Config: vmConfig(512),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_vm.web", plancheck.ResourceActionUpdate),
					plancheck.ExpectResourceAction("orbivisor_volume.root", plancheck.ResourceActionNoop),
				}},
				Check: specIs("Vm", "acc-web01", "memory_mib", 512),
			},
			{
				ResourceName:      "orbivisor_security_group.sg",
				ImportState:       true,
				ImportStateVerify: true,
			},
			{
				ResourceName:      "orbivisor_folder.f",
				ImportState:       true,
				ImportStateVerify: true,
			},
			{
				// By id: the state's id becomes the name.
				ResourceName: "orbivisor_vm.web",
				ImportState:  true,
				ImportStateIdFunc: func(*terraform.State) (string, error) {
					var vm struct {
						ID string `json:"id"`
					}
					err := dev.Get(context.Background(), "/vms/acc-web01", &vm)
					return vm.ID, err
				},
				ImportStateCheck: func(states []*terraform.InstanceState) error {
					a := states[0].Attributes
					if a["id"] != "acc-web01" || a["memory_mib"] != "512" || a["state"] != "running" || a["folder"] != "/acc-vm" {
						return fmt.Errorf("imported %v", a)
					}
					if _, ok := a["nics.0.mac"]; ok {
						return fmt.Errorf("import pinned the server's MAC: %v", a)
					}
					return nil
				},
			},
			{
				PreConfig: func() {
					send(t, "PATCH", "/security-groups/acc-web", map[string]any{"description": "changed in the web UI"})
				},
				Config:             vmConfig(512),
				PlanOnly:           true,
				ExpectNonEmptyPlan: true,
			},
			{
				Config: vmConfig(512),
				Check:  specIs("SecurityGroup", "acc-web", "description", "web"),
			},
			{
				PreConfig: func() { send(t, "DELETE", "/security-groups/acc-web", nil) },
				Config:    vmConfig(512),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_security_group.sg", plancheck.ResourceActionCreate),
				}},
				Check: specIs("SecurityGroup", "acc-web", "description", "web"),
			},
		},
	})
}

// A VM that would go over its folder's quota fails at `terraform plan`, before anything is applied.
func TestAccQuotaAtPlan(t *testing.T) {
	folder := providerBlock + `
resource "orbivisor_folder" "small" {
  name  = "acc-small"
  kind  = "vm"
  quota = { vcpus = 1 }
}
`
	vm := folder + `
resource "orbivisor_vm" "big" {
  name       = "acc-big"
  folder     = "/acc-small"
  cpu        = { cores = 2 }
  memory_mib = 256
  machine    = "virt"
}
`
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy:             gone([2]string{"Folder", "vm:/acc-small"}, [2]string{"Vm", "acc-big"}),
		Steps: []resource.TestStep{
			{Config: folder},
			{Config: vm, PlanOnly: true, ExpectError: regexp.MustCompile(`(?i)quota`)},
			{Config: folder},
		},
	})
}

func networkConfig(description string) string {
	return providerBlock + fmt.Sprintf(`
resource "orbivisor_folder" "net" {
  name = "acc-net"
  kind = "network"
}

resource "orbivisor_network" "ext" {
  name    = "acc-ext"
  folder  = "/${orbivisor_folder.net.name}"
  sdn     = { provider = { physnet = "phys0" } }
  subnets = [{ cidr = "192.0.2.0/24", gateway = "192.0.2.1", floating_pools = [{ start = "192.0.2.100", end = "192.0.2.150" }] }]
}

resource "orbivisor_network" "app" {
  name    = "acc-app"
  sdn     = {}
  subnets = [{ cidr = "10.77.0.0/24" }]
}

resource "orbivisor_network" "lan" {
  name   = "acc-lan"
  bridge = "br0"
  vlan   = 42
}

resource "orbivisor_router" "r" {
  name        = "acc-router"
  description = %q
  external    = { network = orbivisor_network.ext.name }
  interfaces  = [{ network = orbivisor_network.app.name }]
}

resource "orbivisor_floating_ip" "ip" {
  name    = "acc-fip"
  network = orbivisor_network.ext.name
}

resource "orbivisor_load_balancer" "lb" {
  # The VIP network must be a router interface first.
  depends_on = [orbivisor_router.r]
  name       = "acc-lb"
  network   = orbivisor_network.app.name
  listeners = [{ protocol = "tcp", port = 80 }]
}
`, description)
}

// SDN networks (the dev server records SDN as set up; nothing reaches OVN), a VLAN network, a router,
// a floating IP and a load balancer: create, change one field, import.
func TestAccNetworking(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy: gone([2]string{"Network", "acc-ext"}, [2]string{"Network", "acc-app"}, [2]string{"Network", "acc-lan"},
			[2]string{"Router", "acc-router"}, [2]string{"FloatingIp", "acc-fip"}, [2]string{"LoadBalancer", "acc-lb"}),
		Steps: []resource.TestStep{
			{
				Config: networkConfig("edge"),
				Check: resource.ComposeAggregateTestCheckFunc(
					resource.TestCheckResourceAttr("orbivisor_network.ext", "folder", "/acc-net"),
					specIs("Router", "acc-router", "description", "edge"),
					func(*terraform.State) error {
						m, err := manifest("FloatingIp", "acc-fip")
						if err != nil {
							return err
						}
						if a, _ := m.Spec["address"].(string); !strings.HasPrefix(a, "192.0.2.") {
							return fmt.Errorf("floating IP address %v", m.Spec["address"])
						}
						return nil
					},
				),
			},
			{
				Config: networkConfig("edge router"),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_router.r", plancheck.ResourceActionUpdate),
					plancheck.ExpectResourceAction("orbivisor_network.ext", plancheck.ResourceActionNoop),
				}},
				Check: specIs("Router", "acc-router", "description", "edge router"),
			},
			{ResourceName: "orbivisor_network.lan", ImportState: true, ImportStateVerify: true},
			{ResourceName: "orbivisor_network.app", ImportState: true, ImportStateVerify: true},
		},
	})
}

func accessConfig(privileges string) string {
	return providerBlock + fmt.Sprintf(`
resource "orbivisor_folder" "team" {
  name = "acc-team"
  kind = "vm"
}

resource "orbivisor_access_role" "ops" {
  name        = "acc-ops"
  description = "operators"
  privileges  = [%s]
}

resource "orbivisor_grant" "alice" {
  user   = "acc-alice"
  role   = orbivisor_access_role.ops.name
  target = "vm:/${orbivisor_folder.team.name}"
}

resource "orbivisor_grant" "root_viewer" {
  user      = "acc-alice"
  role      = "viewer"
  target    = "/"
  propagate = false
}

resource "orbivisor_placement_rule" "spread" {
  name   = "acc-spread"
  kind   = "anti-affinity"
  vm_tag = "tier=web"
}

resource "orbivisor_ha_group" "pref" {
  name  = "acc-pref"
  nodes = [{ node = "dev1", priority = 10 }]
}
`, privileges)
}

// Access roles, grants (the key is the grant's content — a change replaces it), placement rules and
// HA groups.
func TestAccAccess(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy:             gone([2]string{"AccessRole", "acc-ops"}, [2]string{"PlacementRule", "acc-spread"}, [2]string{"HaGroup", "acc-pref"}),
		Steps: []resource.TestStep{
			{
				PreConfig: func() {
					err := dev.Send(context.Background(), "POST", "/users",
						map[string]any{"name": "acc-alice", "password": "password-1", "role": "none"}, nil)
					var e *client.APIError
					if err != nil && !(errors.As(err, &e) && e.Status == 409) {
						t.Fatal(err)
					}
				},
				Config: accessConfig(`"vm.config", "vm.power"`),
				Check: resource.ComposeAggregateTestCheckFunc(
					resource.TestCheckResourceAttr("orbivisor_grant.alice", "id", "user:acc-alice acc-ops vm:/acc-team"),
					resource.TestCheckResourceAttr("orbivisor_grant.root_viewer", "id", "user:acc-alice viewer / no-propagate"),
					resource.TestCheckResourceAttrSet("orbivisor_grant.alice", "name"),
				),
			},
			{
				Config: accessConfig(`"vm.config"`),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_access_role.ops", plancheck.ResourceActionUpdate),
					plancheck.ExpectResourceAction("orbivisor_grant.alice", plancheck.ResourceActionNoop),
				}},
			},
			{ResourceName: "orbivisor_grant.alice", ImportState: true, ImportStateVerify: true},
			{ResourceName: "orbivisor_access_role.ops", ImportState: true, ImportStateVerify: true},
			{ResourceName: "orbivisor_ha_group.pref", ImportState: true, ImportStateVerify: true},
		},
	})
}

func backupConfig(at string) string {
	key := base64.StdEncoding.EncodeToString([]byte("0123456789abcdef0123456789abcdef"))
	return providerBlock + fmt.Sprintf(`
resource "orbivisor_backup_store" "bs" {
  name    = "acc-store"
  backend = { pool = "backup" }

  encryption_key_wo         = %q
  encryption_key_wo_version = 1
}

resource "orbivisor_backup_job" "nightly" {
  name     = "acc-nightly"
  store    = orbivisor_backup_store.bs.name
  vms      = { all = true }
  schedule = { at = %q }
  keep     = { last = 7 }
}
`, key, at)
}

// A backup store (the encryption key is write-only) and a backup job.
func TestAccBackups(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy:             gone([2]string{"BackupStore", "acc-store"}, [2]string{"BackupJob", "acc-nightly"}),
		Steps: []resource.TestStep{
			{
				Config: backupConfig("03:00"),
				Check: resource.ComposeAggregateTestCheckFunc(
					noSecretInState(base64.StdEncoding.EncodeToString([]byte("0123456789abcdef0123456789abcdef"))),
					func(*terraform.State) error {
						m, err := manifest("BackupStore", "acc-store")
						if err != nil {
							return err
						}
						if _, ok := m.Spec["encryption"].(map[string]any); !ok {
							return fmt.Errorf("the store is not encrypted: %v", m.Spec)
						}
						return nil
					},
				),
			},
			{
				Config: backupConfig("04:30"),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_backup_job.nightly", plancheck.ResourceActionUpdate),
					plancheck.ExpectResourceAction("orbivisor_backup_store.bs", plancheck.ResourceActionNoop),
				}},
			},
			{
				// An import takes the server's defaults too (keep.daily = 0, enabled = true).
				ResourceName: "orbivisor_backup_job.nightly", ImportState: true, ImportStateVerify: true,
				ImportStateVerifyIgnore: []string{"keep", "enabled"},
			},
		},
	})
}

func certConfig(names string) string {
	return providerBlock + fmt.Sprintf(`
resource "orbivisor_certificate" "api" {
  name      = "acc-api"
  account   = "dev"
  names     = [%s]
  challenge = { kind = "http01" }
  scope     = { kind = "shared" }
}
`, names)
}

// An ACME certificate resource (the dev server never orders it: there is no renewer and no CA).
func TestAccCertificate(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		CheckDestroy:             gone([2]string{"Certificate", "acc-api"}),
		Steps: []resource.TestStep{
			{Config: certConfig(`"api.example.test"`)},
			{
				Config: certConfig(`"api.example.test", "www.example.test"`),
				ConfigPlanChecks: resource.ConfigPlanChecks{PreApply: []plancheck.PlanCheck{
					plancheck.ExpectResourceAction("orbivisor_certificate.api", plancheck.ResourceActionUpdate),
				}},
			},
			{
				ResourceName: "orbivisor_certificate.api", ImportState: true, ImportStateVerify: true,
				ImportStateVerifyIgnore: []string{"key"},
			},
		},
	})
}

// Data sources read what Terraform does not manage.
func TestAccDataSources(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		Steps: []resource.TestStep{{
			Config: providerBlock + `
resource "orbivisor_folder" "ds" {
  name = "acc-ds"
  kind = "vm"
}
resource "orbivisor_network" "ds" {
  name   = "acc-ds-lan"
  bridge = "br1"
}
data "orbivisor_nodes" "all" {}
data "orbivisor_storage_pool" "local" { name = "local" }
data "orbivisor_image" "ubuntu" {
  id   = "ubuntu-24.04"
  arch = "aarch64"
}
data "orbivisor_network" "lan" { name = orbivisor_network.ds.name }
data "orbivisor_folder" "ds" { path = "/${orbivisor_folder.ds.name}" }
`,
			Check: resource.ComposeAggregateTestCheckFunc(
				resource.TestCheckResourceAttr("data.orbivisor_nodes.all", "names.0", "dev1"),
				resource.TestCheckResourceAttr("data.orbivisor_nodes.all", "nodes.0.online", "true"),
				resource.TestCheckResourceAttr("data.orbivisor_storage_pool.local", "backend", "dir"),
				resource.TestCheckResourceAttr("data.orbivisor_image.ubuntu", "source_format", "qcow2"),
				resource.TestCheckResourceAttr("data.orbivisor_network.lan", "bridge", "br1"),
				resource.TestCheckResourceAttr("data.orbivisor_folder.ds", "key", "vm:/acc-ds"),
				resource.TestCheckResourceAttrSet("data.orbivisor_folder.ds", "id"),
			),
		}},
	})
}

// A template data source needs a template VM: make one outside Terraform and read it.
func TestAccTemplateDataSource(t *testing.T) {
	resource.Test(t, resource.TestCase{
		ProtoV6ProviderFactories: factories,
		Steps: []resource.TestStep{
			{
				Config: providerBlock + `
resource "orbivisor_vm" "base" {
  name       = "acc-base"
  cpu        = { cores = 1 }
  memory_mib = 384
  machine    = "virt"
  template   = true
}
`,
			},
			{
				Config: providerBlock + `
resource "orbivisor_vm" "base" {
  name       = "acc-base"
  cpu        = { cores = 1 }
  memory_mib = 384
  machine    = "virt"
  template   = true
}
data "orbivisor_template" "base" { name = orbivisor_vm.base.name }
`,
				Check: resource.TestCheckResourceAttr("data.orbivisor_template.base", "memory_mib", "384"),
			},
		},
	})
}
