data "aws_caller_identity" "current" {}

locals {
  env        = "dev"
  account_id = data.aws_caller_identity.current.account_id

  # State bucket naming per PRD §10: <project>-tfstate-<account>.
  state_bucket_name = "${var.project}-tfstate-${local.account_id}"

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
