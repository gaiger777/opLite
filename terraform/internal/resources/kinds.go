package resources

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"strings"
)

// FieldType is how a manifest spec field shows in Terraform.
type FieldType int

const (
	// String is a string (also enums).
	String FieldType = iota
	// Int is an integer.
	Int
	// Number is a float.
	Number
	// Bool is a boolean.
	Bool
	// StringList is a list of strings.
	StringList
	// StringMap is a map of strings.
	StringMap
	// Dynamic is any HCL value (objects, lists of objects, tagged unions) in the server's JSON shape.
	Dynamic
)

func (t FieldType) String() string {
	return [...]string{"string", "int", "number", "bool", "list_string", "map_string", "dynamic"}[t]
}

// Field is one manifest spec field (the attribute has the same name).
type Field struct {
	Name     string
	Type     FieldType
	Required bool
	// Defaulted: the stored spec requires it but the server fills a default for manifests.
	Defaulted bool
	// Replace: changing it replaces the object (the server cannot change it in place). Kinds that hold
	// data (VMs, volumes, backup stores) never replace on a spec field: the server's plan blocks the
	// change (`immutable`) and says what to do by hand.
	Replace     bool
	Description string
}

// Secret is a write-only attribute (Terraform 1.11+) for a secret inside the spec: the value is sent
// on create and whenever its version attribute changes, and never stored in the state.
type Secret struct {
	// Attribute is the write-only attribute (`*_wo`); Attribute + "_version" triggers sending it.
	Attribute string
	// Path is the spec path (dots) the value goes to.
	Path        string
	Description string
}

// Kind is one Terraform resource over one manifest kind.
type Kind struct {
	// TypeName is the resource name after "orbivisor_".
	TypeName string
	// Manifest is the manifest kind (Vm, Volume…).
	Manifest    string
	Description string
	// Folder: the object lives in a folder tree (metadata.folder is a path).
	Folder bool
	// State: the VM power state (`state`).
	State bool
	// NoName: metadata.name is not the key (grants — the content is); the name is computed.
	NoName  bool
	Fields  []Field
	Secrets []Secret
	// Excluded spec fields of the OpenAPI schema the resource does not show, with the reason (the
	// schema match test reads it).
	Excluded map[string]string
}

// Key is the manifest key of a document: the name, folders "tree:/parent/name", grants the content.
func (k *Kind) Key(name, folder string, spec map[string]any) string {
	switch k.Manifest {
	case "Folder":
		tree, _ := spec["kind"].(string)
		return tree + ":" + joinPath(folder, name)
	case "Grant":
		return grantKey(spec)
	}
	return name
}

func joinPath(parent, name string) string {
	if parent == "" || parent == "/" {
		return "/" + name
	}
	return strings.TrimRight(parent, "/") + "/" + name
}

// grantKey is the server's grant key: "user:alice operator vm:/prod" (+ " no-propagate").
func grantKey(spec map[string]any) string {
	who := ""
	if u, ok := spec["user"].(string); ok && u != "" {
		who = "user:" + u
	} else if g, ok := spec["group"].(string); ok {
		who = "group:" + g
	}
	role, _ := spec["role"].(string)
	target, _ := spec["target"].(string)
	target = strings.TrimSpace(target)
	if target != "/" {
		if tree, path, ok := strings.Cut(target, ":"); ok && (tree == "vm" || tree == "network" || tree == "storage") {
			target = tree + ":" + strings.TrimRight(path, "/")
		}
	}
	key := fmt.Sprintf("%s %s %s", who, role, target)
	if p, ok := spec["propagate"].(bool); ok && !p {
		key += " no-propagate"
	}
	return key
}

// grantName is the server's export name of a grant ("grant-" + 12 hex digits of the key's SHA-256).
func grantName(key string) string {
	sum := sha256.Sum256([]byte(key))
	return "grant-" + hex.EncodeToString(sum[:])[:12]
}

var folderExcluded = map[string]string{"folder": "metadata.folder (a path)"}

// Kinds are the fifteen resources (§10.2).
var Kinds = []*Kind{
	{
		TypeName:    "folder",
		Manifest:    "Folder",
		Description: "A folder in the VM, network or storage tree (permissions and quotas follow it). `folder` is the parent path.",
		Folder:      true,
		Fields: []Field{
			{Name: "kind", Type: String, Required: true, Replace: true, Description: "Tree: `vm`, `network` or `storage`."},
			{Name: "quota", Type: Dynamic, Description: "Quota for the folder and everything under it, e.g. `{ vcpus = 16, memory_mib = 65536 }`."},
		},
		Excluded: map[string]string{"name": "metadata.name", "parent": "metadata.folder (parent path)"},
	},
	{
		TypeName:    "network",
		Manifest:    "Network",
		Description: "A network: a Linux bridge/VLAN/VXLAN network or an SDN (OVN) network with subnets.",
		Folder:      true,
		Fields: []Field{
			{Name: "bridge", Type: String, Description: "Host bridge (simple networks)."},
			{Name: "vlan", Type: Int, Replace: true, Description: "VLAN id on the bridge."},
			{Name: "vxlan", Type: Dynamic, Description: "VXLAN overlay settings."},
			{Name: "mtu", Type: Int, Description: "MTU."},
			{Name: "subnets", Type: Dynamic, Description: "Subnets (IPAM) as a list of objects, e.g. `[{ cidr = \"10.0.0.0/24\", gateway = \"10.0.0.1\" }]`."},
			{Name: "sdn", Type: Dynamic, Description: "SDN (OVN) settings: subnets, DHCP, QoS, physnet…"},
		},
		Excluded: folderExcluded,
	},
	{
		TypeName:    "router",
		Manifest:    "Router",
		Description: "An SDN router: interfaces on SDN networks, an external gateway and static routes.",
		Folder:      true,
		Fields: []Field{
			{Name: "description", Type: String},
			{Name: "external", Type: Dynamic, Description: "External gateway (provider network, SNAT)."},
			{Name: "interfaces", Type: Dynamic, Description: "Interfaces: list of `{ network = \"name\", … }`."},
			{Name: "routes", Type: Dynamic, Description: "Static routes."},
		},
		Excluded: folderExcluded,
	},
	{
		TypeName:    "floating_ip",
		Manifest:    "FloatingIp",
		Description: "A floating IP on an external network, optionally pointing at a VM NIC.",
		Folder:      true,
		Fields: []Field{
			{Name: "network", Type: String, Required: true, Replace: true, Description: "External network name."},
			{Name: "address", Type: String, Description: "Address (empty: the server picks one)."},
			{Name: "description", Type: String},
			{Name: "target", Type: Dynamic, Description: "Target, e.g. `{ vm = \"web01\", nic = 0 }`."},
		},
		Excluded: folderExcluded,
	},
	{
		TypeName:    "security_group",
		Manifest:    "SecurityGroup",
		Description: "A security group (firewall rules VM NICs refer to by name).",
		Fields: []Field{
			{Name: "description", Type: String},
			{Name: "rules", Type: Dynamic, Description: "Rules as a list of objects (the whole list is compared)."},
		},
	},
	{
		TypeName:    "load_balancer",
		Manifest:    "LoadBalancer",
		Description: "An SDN load balancer (VIP, listeners and member VMs).",
		Folder:      true,
		Fields: []Field{
			{Name: "network", Type: String, Required: true, Replace: true, Description: "SDN network of the VIP."},
			{Name: "vip", Type: String, Description: "VIP address (empty: the server picks one)."},
			{Name: "listeners", Type: Dynamic, Required: true, Description: "Listeners with their members."},
			{Name: "affinity_secs", Type: Int},
			{Name: "description", Type: String},
			{Name: "external", Type: Dynamic, Description: "External VIP (floating IP)."},
		},
		Excluded: map[string]string{"folder": "metadata.folder (a path)", "monitors": "health monitors are kept outside manifests (server value)"},
	},
	{
		TypeName:    "volume",
		Manifest:    "Volume",
		Description: "A volume (disk or ISO) in a storage pool. Changing the pool or format is not possible in place; size only grows.",
		Fields: []Field{
			{Name: "pool", Type: String, Required: true, Description: "Storage pool name."},
			{Name: "size_bytes", Type: Int, Required: true, Description: "Size in bytes (grows in place; shrinking is refused)."},
			{Name: "format", Type: String, Defaulted: true, Description: "`qcow2` or `raw` (default: the pool's)."},
			{Name: "content", Type: String, Description: "`disk` (default) or `iso`."},
		},
		Excluded: map[string]string{"base": "linked-clone base (server field)"},
	},
	{
		TypeName:    "vm",
		Manifest:    "Vm",
		Description: "A virtual machine. `state` is `running` or `stopped`. Hardware changes on a running VM hot-plug or apply at the next restart (the plan warns).",
		Folder:      true,
		State:       true,
		Fields: []Field{
			{Name: "cpu", Type: Dynamic, Required: true, Description: "CPU, e.g. `{ cores = 2 }` (sockets, threads, model, flags…)."},
			{Name: "memory_mib", Type: Int, Required: true},
			{Name: "max_memory_mib", Type: Int, Description: "Memory hot-plug ceiling."},
			{Name: "machine", Type: String, Required: true, Description: "Machine type (`q35`, `virt`…)."},
			{Name: "firmware", Type: String, Description: "`bios` or `uefi`."},
			{Name: "secure_boot", Type: Bool},
			{Name: "tpm", Type: Bool},
			{Name: "template", Type: Bool, Description: "The VM is a template."},
			{Name: "disks", Type: Dynamic, Description: "Disks: list of `{ volume = \"name\", bus = \"virtio-blk\" }`."},
			{Name: "cdroms", Type: Dynamic, Description: "CD-ROMs: list of objects."},
			{Name: "nics", Type: Dynamic, Description: "NICs: list of `{ network = \"name\", model = \"virtio\", security_groups = [...] }`. The server fills MAC addresses; they are not compared unless declared."},
			{Name: "cloud_init", Type: Dynamic, Description: "cloud-init (user, ssh_keys, …). Give the password with `cloud_init_password_wo`."},
			{Name: "guest_agent", Type: Dynamic},
			{Name: "ha", Type: Dynamic, Description: "HA: `true`/`false` or an object."},
			{Name: "hugepages", Type: String},
			{Name: "numa", Type: Dynamic},
			{Name: "placement", Type: Dynamic},
			{Name: "replication", Type: Dynamic},
			{Name: "restart", Type: String, Description: "Restart policy."},
			{Name: "snapshot_schedule", Type: Dynamic},
		},
		Secrets: []Secret{
			{Attribute: "cloud_init_password_wo", Path: "cloud_init.password", Description: "cloud-init password (write-only: never stored in the state). Sent on create and when `cloud_init_password_wo_version` changes."},
		},
		Excluded: map[string]string{
			"folder": "metadata.folder (a path)",
			"power":  "the `state` attribute",
			"node":   "the scheduler picks the node",
		},
	},
	{
		TypeName:    "placement_rule",
		Manifest:    "PlacementRule",
		Description: "A placement rule (affinity, anti-affinity, node pinning).",
		Fields: []Field{
			{Name: "kind", Type: String, Required: true, Description: "Rule kind."},
			{Name: "vms", Type: StringList, Description: "VM names."},
			{Name: "vm_tag", Type: String},
			{Name: "nodes", Type: StringList, Description: "Node names."},
			{Name: "node_tag", Type: String},
			{Name: "weight", Type: Int},
			{Name: "enabled", Type: Bool},
			{Name: "ha_may_violate", Type: Bool},
		},
	},
	{
		TypeName:    "ha_group",
		Manifest:    "HaGroup",
		Description: "An HA group (node priorities for HA VMs).",
		Fields: []Field{
			{Name: "nodes", Type: Dynamic, Required: true, Description: "Nodes: list of `{ node = \"name\", priority = 1 }`."},
			{Name: "restricted", Type: Bool},
			{Name: "nofailback", Type: Bool},
		},
	},
	{
		TypeName:    "backup_job",
		Manifest:    "BackupJob",
		Description: "A scheduled backup job.",
		Fields: []Field{
			{Name: "vms", Type: Dynamic, Required: true, Description: "VM selector."},
			{Name: "schedule", Type: Dynamic, Required: true, Description: "Schedule."},
			{Name: "store", Type: String, Description: "Backup store name."},
			{Name: "pool", Type: String, Description: "Backup pool name."},
			{Name: "keep", Type: Dynamic, Description: "Retention."},
			{Name: "full_every", Type: Int},
			{Name: "enabled", Type: Bool},
			{Name: "copy_to", Type: Dynamic},
			{Name: "replicate", Type: Dynamic},
		},
	},
	{
		TypeName:    "backup_store",
		Manifest:    "BackupStore",
		Description: "A backup store (directory or S3). Give secrets with the write-only attributes.",
		Fields: []Field{
			{Name: "backend", Type: Dynamic, Required: true, Description: "Backend (`{ kind = \"dir\", path = … }` or S3)."},
			{Name: "encryption", Type: Dynamic, Description: "Encryption (the key goes in `encryption_key_wo`)."},
			{Name: "gc", Type: Dynamic},
			{Name: "verify", Type: Dynamic},
			{Name: "max_streams", Type: Int},
			{Name: "read_only", Type: Bool},
		},
		Secrets: []Secret{
			{Attribute: "encryption_key_wo", Path: "encryption.key", Description: "Encryption master key (base64; write-only). Only used when the store is created."},
			{Attribute: "s3_secret_access_key_wo", Path: "backend.s3.secret_access_key", Description: "S3 secret access key (write-only). Sent on create and when `s3_secret_access_key_wo_version` changes."},
		},
	},
	{
		TypeName:    "grant",
		Manifest:    "Grant",
		Description: "A permission grant: a user or group gets a role on `/`, a folder (`vm:/prod`) or an object (`vm/web01`). Any change replaces the grant.",
		NoName:      true,
		Fields: []Field{
			{Name: "user", Type: String, Replace: true, Description: "User name (or `group`)."},
			{Name: "group", Type: String, Replace: true, Description: "Group name (or `user`)."},
			{Name: "role", Type: String, Required: true, Replace: true, Description: "Built-in role (admin, operator, viewer) or access role name."},
			{Name: "target", Type: String, Required: true, Replace: true},
			{Name: "propagate", Type: Bool, Replace: true, Description: "Applies below the folder (default true)."},
		},
	},
	{
		TypeName:    "access_role",
		Manifest:    "AccessRole",
		Description: "A custom access role (a set of privileges).",
		Fields: []Field{
			{Name: "privileges", Type: StringList, Required: true},
			{Name: "description", Type: String},
		},
	},
	{
		TypeName:    "certificate",
		Manifest:    "Certificate",
		Description: "An ACME certificate for the API/web listener (the cluster orders and renews it).",
		Fields: []Field{
			{Name: "account", Type: String, Required: true, Description: "ACME account name."},
			{Name: "names", Type: StringList, Required: true, Description: "DNS names (`{node}` expands per node)."},
			{Name: "challenge", Type: Dynamic, Required: true, Description: "`{ kind = \"http01\" }` or `{ kind = \"dns01\", dns = \"provider\" }`."},
			{Name: "scope", Type: Dynamic, Description: "Per node or shared."},
			{Name: "key", Type: String},
			{Name: "profile", Type: String},
			{Name: "renew", Type: Dynamic},
			{Name: "preferred_chain", Type: String},
			{Name: "install", Type: String},
			{Name: "precheck", Type: Bool},
		},
	},
}

// KindByType finds a kind by its TypeName.
func KindByType(name string) *Kind {
	for _, k := range Kinds {
		if k.TypeName == name {
			return k
		}
	}
	return nil
}
