# Non-secret, stable values (PRD §11). Project, region and the GitHub slug are set;
# `alarm_email` is the only value below that is still a placeholder.
project     = "timelines"
region      = "eu-central-1"
github_org  = "zakerf"
github_repo = "timelines"

# Placeholder until the operator address is settled. SNS sends a confirmation
# email; the subscription stays pending until it is clicked.
alarm_email = "ops@example.com"

# Production OIDC provider is deliberately fog (PRD §7/§13). These are
# placeholders: swapping in the chosen provider is a config change only.
oidc_discovery_url     = "https://auth.example.com/.well-known/openid-configuration"
oidc_authorization_url = "https://auth.example.com/oauth2/authorize"
oidc_token_url         = "https://auth.example.com/oauth2/token"
oidc_jwks_url          = "https://auth.example.com/oauth2/jwks"
oidc_client_id         = "timelines"
