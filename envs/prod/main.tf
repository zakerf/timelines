data "aws_caller_identity" "current" {}

locals {
  env        = "prod"
  account_id = data.aws_caller_identity.current.account_id

  # State bucket naming per PRD §10: <project>-tfstate-<account>.
  state_bucket_name = "${var.project}-tfstate-${local.account_id}"

  # Single data table per environment.
  data_table_name = "${var.project}-${local.env}-data"

  # Private bucket holding rendered pages. The account id keeps the name
  # globally unique across AWS.
  site_bucket_name = "${var.project}-${local.env}-site-${local.account_id}"

  # Distribution id for the API Lambda (S3 parameter, not module edge).
  distribution_id_param_name = "/${var.project}/${local.env}/cloudfront/distribution-id"

  # Prod deploys run only from the GitHub `prod` Environment (required
  # reviewers, v* tags); no branch or pull_request trust.
  repo_claim = "repo:${var.github_org}/${var.github_repo}"

  trusted_github_subjects = [
    "${local.repo_claim}:environment:prod",
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
  deletion_protection_enabled = true
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

module "cdn" {
  source = "../../modules/cdn"

  project                          = var.project
  env                              = local.env
  region                           = var.region
  site_bucket_regional_domain_name = module.site.bucket_regional_domain_name
  distribution_id_param_name       = local.distribution_id_param_name
}
