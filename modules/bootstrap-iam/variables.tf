variable "project" {
  description = "Project slug used for resource naming and tags."
  type        = string
}

variable "env" {
  description = "Environment name (dev or prod)."
  type        = string
}

variable "github_org" {
  description = "GitHub organisation or user that owns the repository."
  type        = string
}

variable "github_repo" {
  description = "GitHub repository name (without the owner)."
  type        = string
}

variable "trusted_github_subjects" {
  description = "Exact OIDC `sub` claims allowed to assume the deploy role, e.g. repo:<org>/<repo>:environment:dev."
  type        = list(string)

  validation {
    condition     = length(var.trusted_github_subjects) > 0
    error_message = "At least one trusted GitHub OIDC subject is required."
  }
}

variable "deploy_role_name" {
  description = "Name of the GitHub Actions deploy role."
  type        = string
  default     = "gha-deploy"
}
