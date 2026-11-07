data "aws_caller_identity" "current" {}

locals {
  account_id  = data.aws_caller_identity.current.account_id
  name_prefix = "${var.project}-${var.env}"

  # IAM actions on this namespace fill the gap PowerUserAccess leaves closed.
  app_role_arn_pattern = "arn:aws:iam::${local.account_id}:role/${local.name_prefix}-*"

  tags = {
    Project     = var.project
    Environment = var.env
    ManagedBy   = "opentofu"
  }
}

resource "aws_iam_openid_connect_provider" "github" {
  url            = "https://token.actions.githubusercontent.com"
  client_id_list = ["sts.amazonaws.com"]

  thumbprint_list = [
    "6938fd4d98bab03faadb97b34396831e3780aea1",
    "1c58a3a8518e8759bf075b76b750d4f2df264fcd",
  ]

  tags = local.tags
}

data "aws_iam_policy_document" "gha_deploy_assume_role" {
  statement {
    sid     = "GitHubActionsOIDC"
    effect  = "Allow"
    actions = ["sts:AssumeRoleWithWebIdentity"]

    principals {
      type        = "Federated"
      identifiers = [aws_iam_openid_connect_provider.github.arn]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:aud"
      values   = ["sts.amazonaws.com"]
    }

    condition {
      test     = "StringEquals"
      variable = "token.actions.githubusercontent.com:sub"
      values   = var.trusted_github_subjects
    }
  }
}

resource "aws_iam_role" "gha_deploy" {
  name               = var.deploy_role_name
  description        = "GitHub Actions deploy role for ${var.project} (${var.env}); assumed via OIDC, no static keys."
  assume_role_policy = data.aws_iam_policy_document.gha_deploy_assume_role.json

  tags = local.tags
}

resource "aws_iam_role_policy_attachment" "gha_deploy_power_user" {
  role       = aws_iam_role.gha_deploy.name
  policy_arn = "arn:aws:iam::aws:policy/PowerUserAccess"
}

# PowerUserAccess denies all IAM writes; the deploy role needs exactly enough
# to manage the app's own roles and pass them to Lambda.
data "aws_iam_policy_document" "app_iam" {
  statement {
    sid    = "ManageAppRoles"
    effect = "Allow"

    actions = [
      "iam:CreateRole",
      "iam:DeleteRole",
      "iam:GetRole",
      "iam:TagRole",
      "iam:UntagRole",
      "iam:ListRoleTags",
      "iam:AttachRolePolicy",
      "iam:DetachRolePolicy",
      "iam:ListAttachedRolePolicies",
      "iam:PutRolePolicy",
      "iam:GetRolePolicy",
      "iam:DeleteRolePolicy",
      "iam:ListRolePolicies",
    ]

    resources = [local.app_role_arn_pattern]
  }

  statement {
    sid     = "PassAppRolesToLambda"
    effect  = "Allow"
    actions = ["iam:PassRole"]

    resources = [local.app_role_arn_pattern]

    condition {
      test     = "StringEquals"
      variable = "iam:PassedToService"
      values   = ["lambda.amazonaws.com"]
    }
  }
}

resource "aws_iam_role_policy" "app_iam" {
  name   = "app-iam"
  role   = aws_iam_role.gha_deploy.id
  policy = data.aws_iam_policy_document.app_iam.json
}
