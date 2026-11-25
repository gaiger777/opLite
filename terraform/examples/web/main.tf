# A small web tier: folder with a quota, SDN network, security group, volume and a running VM.
# Try it against the dev server:
#   cargo run -p orbivisord --features devserver --bin orbivisor-devserver   (prints url, token, ca_pem)
#   export ORBIVISOR_SERVER=… ORBIVISOR_TOKEN=… ORBIVISOR_CA_PEM="$(…)"; terraform init && terraform apply
terraform {
  required_providers {
    orbivisor = { source = "gaiger777/orbivisor" }
  }
}

provider "orbivisor" {}

resource "orbivisor_folder" "web" {
  name  = "web"
  kind  = "vm"
  quota = { vcpus = 8, memory_mib = 16384 }
}

resource "orbivisor_volume" "root" {
  name       = "web01-root"
  pool       = "local"
  size_bytes = 1024 * 1024 * 1024
}

resource "orbivisor_vm" "web01" {
  name       = "web01"
  folder     = "/${orbivisor_folder.web.name}"
  state      = "running"
  cpu        = { cores = 2 }
  memory_mib = 2048
  machine    = "virt"
  disks      = [{ volume = orbivisor_volume.root.name, bus = "virtio-blk" }]
  nics       = [{ network = "user", model = "virtio" }]
  cloud_init = { user = "rocky" }
}
