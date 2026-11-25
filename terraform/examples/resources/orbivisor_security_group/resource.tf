resource "orbivisor_security_group" "web" {
  name        = "web"
  description = "HTTP and HTTPS from anywhere"
  rules = [
    { direction = "in", action = "accept", protocol = "tcp", ports = "80" },
    { direction = "in", action = "accept", protocol = "tcp", ports = "443" },
  ]
}
