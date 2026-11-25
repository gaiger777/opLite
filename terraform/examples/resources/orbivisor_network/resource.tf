# A VLAN on a host bridge
resource "orbivisor_network" "lan" {
  name   = "lan"
  bridge = "vmbr0"
  vlan   = 42
}

# An SDN (OVN) overlay network with a subnet
resource "orbivisor_network" "app" {
  name    = "app"
  folder  = "/prod"
  sdn     = {}
  subnets = [{ cidr = "10.20.0.0/24", gateway = "10.20.0.1" }]
}
