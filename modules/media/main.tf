locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }
}

# PRD §9/§10: cover images live in their own private bucket. It is separate
# from `site` because the deploy job runs `aws s3 sync --delete`, which would
# wipe uploads. Objects are read only through CloudFront with OAC.
resource "aws_s3_bucket" "media" {
  bucket = var.bucket_name

  tags = local.tags
}

# Deliberately no versioning. Cover keys are immutable (`cover/<id>/<uuid>.<ext>`)
# and a replacement deletes the superseded object, so old versions would only
# be stored cost with no rollback value.
resource "aws_s3_bucket_server_side_encryption_configuration" "media" {
  bucket = aws_s3_bucket.media.id

  rule {
    bucket_key_enabled = true

    apply_server_side_encryption_by_default {
      sse_algorithm = "AES256"
    }
  }
}

resource "aws_s3_bucket_public_access_block" "media" {
  bucket = aws_s3_bucket.media.id

  block_public_acls       = true
  block_public_policy     = true
  ignore_public_acls      = true
  restrict_public_buckets = true
}

# CloudFront OAC reads objects as the cloudfront.amazonaws.com service
# principal. The condition is AWS:SourceAccount, not AWS:SourceArn: a
# SourceArn naming this distribution would make `cdn` depend on `media`'s
# policy while `cdn` already consumes `media`'s domain, closing a cycle.
data "aws_iam_policy_document" "media" {
  statement {
    sid    = "AllowCloudFrontServicePrincipalReadOnly"
    effect = "Allow"

    principals {
      type        = "Service"
      identifiers = ["cloudfront.amazonaws.com"]
    }

    actions   = ["s3:GetObject"]
    resources = ["${aws_s3_bucket.media.arn}/*"]

    condition {
      test     = "StringEquals"
      variable = "AWS:SourceAccount"
      values   = [var.account_id]
    }
  }
}

resource "aws_s3_bucket_policy" "media" {
  bucket = aws_s3_bucket.media.id
  policy = data.aws_iam_policy_document.media.json

  depends_on = [aws_s3_bucket_public_access_block.media]
}
