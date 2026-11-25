terraform {
  required_providers {
    orbivisor = {
      source = "gaiger777/orbivisor"
    }
  }
}

# endpoint, api_token and ca_pem can also come from OPL_SERVER, OPL_TOKEN and OPL_CA_PEM.
provider "orbivisor" {
  endpoint       = "https://node1.example.net:8006"
  api_token      = var.orbivisor_token
  ca_fingerprint = "sha256:3f1c…" # or ca_pem = file("cluster-ca.pem")
}

variable "orbivisor_token" {
  type      = string
  sensitive = true
}
