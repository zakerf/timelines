locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }
}

# Remote state for this environment account. OpenTofu >= 1.10 takes its lock
# with a native S3 lockfile (backend `use_lockfile = true`), so there is no
# DynamoDB lock table.
resource "aws_s3_bucket" "state" {
  bucket = var.state_bucket_name

  tags = local.tags
}

resource "aws_s3_bucket_versioning" "state" {
  bucket = aws_s3_bucket.state.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "state" {
  bucket = aws_s3_bucket.state.id

  rule {
    bucket_key_enabled = true

    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "state" {
  bucket = aws_s3_bucket.state.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

resource "aws_s3_bucket_lifecycle_configuration" "state" {
  bucket = aws_s3_bucket.state.id

  rule {
    id     = "abort-incomplete-multipart-uploads"
    status = "Enabled"

    filter {}

    abort_incomplete_multipart_upload {
      days_after_initiation = 7
    }
  }

  rule {
    id     = "expire-noncurrent-versions"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = 90
    }
  }

  # Noncurrent-version expiration is only meaningful once versioning is on.
  depends_on = [aws_s3_bucket_versioning.state]
}
