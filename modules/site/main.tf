locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }
}

# PRD §4/§10: rendered pages live in a private S3 bucket and are read only
# through CloudFront with Origin Access Control. Public access stays blocked.
resource "aws_s3_bucket" "site" {
  bucket = var.bucket_name

  tags = local.tags
}

resource "aws_s3_bucket_versioning" "site" {
  bucket = aws_s3_bucket.site.id

  versioning_configuration {
    status = "Enabled"
  }
}

resource "aws_s3_bucket_server_side_encryption_configuration" "site" {
  bucket = aws_s3_bucket.site.id

  rule {
    bucket_key_enabled = true

    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "site" {
  bucket = aws_s3_bucket.site.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# Keep superseded rendered pages for rollback (`aws s3 sync --delete` replaces
# objects), then expire them. Versioning is on, so only noncurrent versions go.
resource "aws_s3_bucket_lifecycle_configuration" "site" {
  bucket = aws_s3_bucket.site.id

  rule {
    id     = "expire-noncurrent-versions"
    status = "Enabled"

    filter {}

    noncurrent_version_expiration {
      noncurrent_days = var.noncurrent_version_expiration_days
    }
  }

  depends_on = [aws_s3_bucket_versioning.site]
}

# CloudFront OAC reads objects as the cloudfront.amazonaws.com service
# principal. The condition is AWS:SourceAccount, NOT AWS:SourceArn: a
# SourceArn naming this distribution would make `cdn` depend on `site`'s
# policy while `cdn` already consumes `site`'s domain, closing a cycle.
data "aws_iam_policy_document" "site" {
  statement {
    sid    = "AllowCloudFrontServicePrincipalReadOnly"
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.site.arn}/*"]

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceAccount"
      values   = [var.account_id]
    }
  }
}

resource "aws_s3_bucket_policy" "site" {
  bucket = aws_s3_bucket.site.id
  policy = data.aws_iam_policy_document.site.json

  depends_on = [aws_s3_bucket_public_access_block.site]
}
