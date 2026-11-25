data "orbivisor_nodes" "all" {}

resource "orbivisor_ha_group" "prefer_first" {
  name  = "prefer-first"
  nodes = [for i, n in data.orbivisor_nodes.all.names : { node = n, priority = 100 - i }]
}
