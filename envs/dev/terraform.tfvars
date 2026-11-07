# Non-secret, stable values (PRD §11). `project` and the GitHub slug are
# placeholders until branding and the repository owner are settled.
project     = "timeline"
region      = "eu-central-1"
github_org  = "example-org"
github_repo = "timeline-site"

# Local OIDC seam: the docker-compose Keycloak realm (provider for production
# is deliberately fog; swapping it is a config change, not a code change).
oidc_discovery_url     = "http://localhost:8080/realms/timeline/.well-known/openid-configuration"
oidc_authorization_url = "http://localhost:8080/realms/timeline/protocol/openid-connect/auth"
oidc_token_url         = "http://localhost:8080/realms/timeline/protocol/openid-connect/token"
oidc_jwks_url          = "http://localhost:8080/realms/timeline/protocol/openid-connect/certs"
oidc_client_id         = "timeline-site"
