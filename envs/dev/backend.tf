# Partial S3 backend. The bucket is created by module.bootstrap, so the first
# apply runs on local state; afterwards migrate:
#
#   cp backend.hcl.example backend.hcl   # fill in the account id
#   tofu init -migrate-state -backend-config=backend.hcl
#
# See docs/INFRA.md.
terraform {
  backend "s3" {}
}
