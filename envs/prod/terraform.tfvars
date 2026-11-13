# Non-secret, stable values (PRD §11). `project` and the GitHub slug are
# placeholders until branding and the repository owner are settled.
project     = "timeline"
region      = "eu-central-1"
github_org  = "example-org"
github_repo = "timeline-site"

# Placeholder until the operator address is settled. SNS sends a confirmation
# email; the subscription stays pending until it is clicked.
alarm_email = "ops@example.com"

# Production OIDC provider is deliberately fog (PRD §7/§13). These are
# placeholders: swapping in the chosen provider is a config change only.
oidc_discovery_url     = "https://auth.example.com/.well-known/openid-configuration"
oidc_authorization_url = "https://auth.example.com/oauth2/authorize"
oidc_token_url         = "https://auth.example.com/oauth2/token"
oidc_jwks_url          = "https://auth.example.com/oauth2/jwks"
oidc_client_id         = "timeline-site"
