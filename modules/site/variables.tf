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
  description = "Name of the private S3 bucket holding rendered pages."
  type        = string
}

variable "account_id" {
  description = "AWS account id the CloudFront distribution lives in (AWS:SourceAccount condition)."
  type        = string
}

variable "noncurrent_version_expiration_days" {
  description = "Days to keep superseded rendered pages for rollback."
  type        = number
  default     = 90
}
