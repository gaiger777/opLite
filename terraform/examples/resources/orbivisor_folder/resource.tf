resource "orbivisor_folder" "prod" {
  name  = "prod"
  kind  = "vm"
  quota = { vcpus = 64, memory_mib = 262144 }
}

resource "orbivisor_folder" "web" {
  name   = "web"
  kind   = "vm"
  folder = "/${orbivisor_folder.prod.name}"
}
