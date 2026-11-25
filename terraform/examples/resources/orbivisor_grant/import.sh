# The key is the grant itself: "user:NAME|group:NAME ROLE TARGET" (+ " no-propagate")
terraform import orbivisor_grant.web_team "group:web-team operator vm:/prod/web"
