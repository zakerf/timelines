variable "project" {
  description = "Project slug used for tags and resource naming."
  type        = string
}

variable "env" {
  description = "Environment name (dev or prod)."
  type        = string
}

variable "region" {
  description = "AWS region the environment lives in."
  type        = string
}

variable "bucket_name" {
  description = "Name of the private S3 bucket holding Lambda build output."
  type        = string
}

variable "noncurrent_version_expiration_days" {
  description = "Days to keep superseded artifact versions for rollback."
  type        = number
  default     = 365
}
