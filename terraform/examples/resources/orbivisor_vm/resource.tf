resource "orbivisor_vm" "web01" {
  name       = "web01"
  folder     = "/prod/web"
  state      = "running"
  cpu        = { cores = 4 }
  memory_mib = 8192
  machine    = "q35"
  disks      = [{ volume = orbivisor_volume.web01_root.name, bus = "virtio-blk" }]
  nics       = [{ network = orbivisor_network.app.name, model = "virtio", security_groups = [orbivisor_security_group.web.name] }]
  cloud_init = { user = "rocky", ssh_keys = [file("~/.ssh/id_ed25519.pub")] }

  # Write-only: sent to the server, never stored in the state. Bump the version to send it again.
  cloud_init_password_wo         = var.web_password
  cloud_init_password_wo_version = 1
}

variable "web_password" {
  type      = string
  sensitive = true
  ephemeral = true
}
