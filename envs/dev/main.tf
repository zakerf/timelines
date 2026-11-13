data "aws_caller_identity" "current" {}

locals {
  env        = "dev"
  account_id = data.aws_caller_identity.current.account_id

  # State bucket naming per PRD §10: <project>-tfstate-<account>.
  state_bucket_name = "${var.project}-tfstate-${local.account_id}"

  # Single data table per environment.
  data_table_name = "${var.project}-${local.env}-data"

  # Private bucket holding rendered pages. The account id keeps the name
  # globally unique across AWS.
  site_bucket_name = "${var.project}-${local.env}-site-${local.account_id}"

  # Private bucket holding immutable cover objects (PRD §9). Separate from
  # `site` so the deploy's `aws s3 sync --delete` cannot wipe uploads.
  media_bucket_name = "${var.project}-${local.env}-media-${local.account_id}"

  # Distribution id for the API Lambda (S3 parameter, not module edge).
  distribution_id_param_name = "/${var.project}/${local.env}/cloudfront/distribution-id"

  # OIDC seam parameters. The salt is a SecureString created out-of-band;
  # OpenTofu owns only its name.
  oidc_config_param_name = "/${var.project}/${local.env}/oidc"
  ip_salt_param_name     = "/${var.project}/${local.env}/ip-salt"

  # Lambda build output (PRD §11): one immutable `api-<sha>.zip` per commit.
  artifacts_bucket_name = "${var.project}-${local.env}-artifacts-${local.account_id}"

  # Session policy for the API: opaque HttpOnly cookie, no tokens in the browser.
  session_cookie_name = "timeline_session"
  session_ttl_days    = 30

  # Deploy identity trusts pushes to main, pull requests (tofu plan),
  # and the GitHub `dev` Environment (deploy-dev).
  repo_claim = "repo:${var.github_org}/${var.github_repo}"

  trusted_github_subjects = [
    "${local.repo_claim}:ref:refs/heads/main",
    "${local.repo_claim}:pull_request",
    "${local.repo_claim}:environment:dev",
  ]
}

module "bootstrap" {
  source = "../../modules/bootstrap"

  project           = var.project
  env               = local.env
  region            = var.region
  state_bucket_name = local.state_bucket_name
}

module "bootstrap_iam" {
  source = "../../modules/bootstrap-iam"

  project                 = var.project
  env                     = local.env
  github_org              = var.github_org
  github_repo             = var.github_repo
  trusted_github_subjects = local.trusted_github_subjects
}

module "data" {
  source = "../../modules/data"

  project                     = var.project
  env                         = local.env
  region                      = var.region
  table_name                  = local.data_table_name
  deletion_protection_enabled = false
  sns_topic_arn               = var.alarm_sns_topic_arn
}

module "site" {
  source = "../../modules/site"

  project     = var.project
  env         = local.env
  region      = var.region
  bucket_name = local.site_bucket_name
  account_id  = local.account_id
}

module "media" {
  source = "../../modules/media"

  project     = var.project
  env         = local.env
  region      = var.region
  bucket_name = local.media_bucket_name
  account_id  = local.account_id
}

module "artifacts" {
  source = "../../modules/artifacts"

  project     = var.project
  env         = local.env
  region      = var.region
  bucket_name = local.artifacts_bucket_name
}

module "config" {
  source = "../../modules/config"

  project                    = var.project
  env                        = local.env
  region                     = var.region
  parameter_name             = local.oidc_config_param_name
  ip_salt_param_name         = local.ip_salt_param_name
  oidc_discovery_url         = var.oidc_discovery_url
  oidc_authorization_url     = var.oidc_authorization_url
  oidc_token_url             = var.oidc_token_url
  oidc_jwks_url              = var.oidc_jwks_url
  oidc_client_id             = var.oidc_client_id
  token_endpoint_auth_method = "none"
}

# The distribution-id parameter name is a local string, not a module.cdn
# reference: that keeps the PRD §10 cycle broken (cdn consumes api's domain).
module "api" {
  source = "../../modules/api"

  project                    = var.project
  env                        = local.env
  region                     = var.region
  account_id                 = local.account_id
  table_name                 = module.data.table_name
  table_arn                  = module.data.table_arn
  site_bucket_name           = module.site.bucket_name
  site_bucket_arn            = module.site.bucket_arn
  media_bucket_name          = module.media.bucket_name
  media_bucket_arn           = module.media.bucket_arn
  distribution_id_param_name = local.distribution_id_param_name
  oidc_config_param_name     = module.config.oidc_config_param_name
  ip_salt_param_name         = module.config.ip_salt_param_name
  artifact_bucket_name       = module.artifacts.bucket_name
  artifact_key               = var.api_artifact_key
  artifact_version           = var.api_artifact_version
  session_cookie_name        = local.session_cookie_name
  session_ttl_days           = local.session_ttl_days
  alarm_sns_topic_arn        = var.alarm_sns_topic_arn
}

module "cdn" {
  source = "../../modules/cdn"

  project                           = var.project
  env                               = local.env
  region                            = var.region
  site_bucket_regional_domain_name  = module.site.bucket_regional_domain_name
  media_bucket_regional_domain_name = module.media.bucket_regional_domain_name
  api_domain_name                   = module.api.api_domain_name
  distribution_id_param_name        = local.distribution_id_param_name
}
