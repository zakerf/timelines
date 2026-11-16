# Dev environment root. The stack lives in `modules/stack` so dev and prod
# cannot drift apart; this root only pins the environment.
module "stack" {
  source = "../../modules/stack"

  providers = {
    aws           = aws
    aws.us_east_1 = aws.us_east_1
  }

  project                = var.project
  env                    = "dev"
  region                 = var.region
  github_org             = var.github_org
  github_repo            = var.github_repo
  alarm_email            = var.alarm_email
  api_artifact_key       = var.api_artifact_key
  api_artifact_version   = var.api_artifact_version
  oidc_discovery_url     = var.oidc_discovery_url
  oidc_authorization_url = var.oidc_authorization_url
  oidc_token_url         = var.oidc_token_url
  oidc_jwks_url          = var.oidc_jwks_url
  oidc_client_id         = var.oidc_client_id
  public_base_url        = var.public_base_url
}
