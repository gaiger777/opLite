resource "orbivisor_volume" "web01_root" {
  name       = "web01-root"
  pool       = "local"
  size_bytes = 40 * 1024 * 1024 * 1024
}
