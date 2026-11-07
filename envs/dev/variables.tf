variable "project" {
  description = "Project slug used for resource naming (placeholder until branding is settled)."
  type        = string
}

variable "region" {
  description = "AWS region for this environment."
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
