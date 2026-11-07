locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }
}

# PRD §7/§10: the OIDC seam is configuration, not provider code. Four URLs and
# a public client id are non-secret and live in a String parameter; the seam
# holds no secret. The renderer/API read this JSON.
#
# The IP-hash salt is a SecureString created out-of-band (never in state) and
# referenced by name only -- this module outputs that name for the api module.
resource "aws_ssm_parameter" "oidc" {
  name = var.parameter_name
  type = "String"

  value = jsonencode({
    discovery_url              = var.oidc_discovery_url
    authorization_url          = var.oidc_authorization_url
    token_url                  = var.oidc_token_url
    jwks_url                   = var.oidc_jwks_url
    client_id                  = var.oidc_client_id
    token_endpoint_auth_method = var.token_endpoint_auth_method
  })

  tags = local.tags
}
