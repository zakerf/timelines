# Non-secret, stable values (PRD §11). Project, region and the GitHub slug are set;
# `alarm_email` is the only value below that is still a placeholder.
project     = "timelines"
region      = "eu-central-1"
github_org  = "zakerf"
github_repo = "timelines"

# Placeholder until the operator address is settled. SNS sends a confirmation
# email; the subscription stays pending until it is clicked.
alarm_email = "ops@example.com"

# Local OIDC seam: the docker-compose Keycloak realm (provider for production
# is deliberately fog; swapping it is a config change, not a code change).
oidc_discovery_url     = "http://localhost:8080/realms/timeline/.well-known/openid-configuration"
oidc_authorization_url = "http://localhost:8080/realms/timeline/protocol/openid-connect/auth"
oidc_token_url         = "http://localhost:8080/realms/timeline/protocol/openid-connect/token"
oidc_jwks_url          = "http://localhost:8080/realms/timeline/protocol/openid-connect/certs"
oidc_client_id         = "timelines"
