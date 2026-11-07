variable "project" {
  description = "Project slug used for resource naming and tags."
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

variable "state_bucket_name" {
  description = "Globally unique name for the OpenTofu state bucket, conventionally <project>-tfstate-<account-id>."
  type        = string
}
