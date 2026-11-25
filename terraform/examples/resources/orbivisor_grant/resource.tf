resource "orbivisor_grant" "web_team" {
  group  = "web-team"
  role   = "operator"
  target = "vm:/prod/web"
}
