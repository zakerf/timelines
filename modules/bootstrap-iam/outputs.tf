output "gha_deploy_role_arn" {
  description = "ARN of the GitHub Actions deploy role."
  value       = aws_iam_role.gha_deploy.arn
}

output "github_oidc_provider_arn" {
  description = "ARN of the GitHub Actions OIDC provider."
  value       = aws_iam_openid_connect_provider.github.arn
}
