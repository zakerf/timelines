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

variable "alarm_sns_topic_arn" {
  description = "SNS topic that receives operational alarms. Null until the observability topic exists."
  type        = string
  default     = null
}

variable "api_artifact_key" {
  description = "S3 object key of the packaged API Lambda artifact (per-run deploy value)."
  type        = string
  default     = "api.zip"
}

variable "api_artifact_version" {
  description = "S3 object version of the packaged API Lambda artifact (per-run deploy value)."
  type        = string
  default     = null
}

variable "oidc_discovery_url" {
  description = "OIDC discovery document URL for this environment."
  type        = string
}

variable "oidc_authorization_url" {
  description = "OIDC authorization endpoint URL."
  type        = string
}

variable "oidc_token_url" {
  description = "OIDC token endpoint URL."
  type        = string
}

variable "oidc_jwks_url" {
  description = "OIDC JWKS URL used to verify ID tokens."
  type        = string
}

variable "oidc_client_id" {
  description = "Public OIDC client id (no secret; authorization-code + PKCE)."
  type        = string
}
