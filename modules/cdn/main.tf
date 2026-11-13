locals {
  tags = {
    Project     = var.project
    Environment = var.env
    Region      = var.region
    ManagedBy   = "opentofu"
  }

  origin_id       = "s3-site"
  media_origin_id = "s3-media"
  api_origin_id   = "api-gateway"

  alarm_actions = var.alarm_sns_topic_arn == null ? [] : [var.alarm_sns_topic_arn]
}

# PRD §10: the origin signs every request with SigV4 via Origin Access Control.
resource "aws_cloudfront_origin_access_control" "site" {
  name                              = "${var.project}-${var.env}-site"
  description                       = "OAC for the rendered-pages S3 bucket"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# PRD §9/§10: media objects are private too; /media/* reads through this OAC.
resource "aws_cloudfront_origin_access_control" "media" {
  name                              = "${var.project}-${var.env}-media"
  description                       = "OAC for the cover-image S3 bucket"
  origin_access_control_origin_type = "s3"
  signing_behavior                  = "always"
  signing_protocol                  = "sigv4"
}

# PRD §4: extensionless viewer URIs (`/timeline/<id>`) resolve to the
# `<id>.html` object. The rewrite rules are unit-tested in rewrite.test.mjs.
resource "aws_cloudfront_function" "rewrite" {
  name    = "${var.project}-${var.env}-extensionless-rewrite"
  runtime = "cloudfront-js-2.0"
  comment = "Append index.html / .html to extensionless viewer paths"
  publish = true
  code    = file("${path.module}/rewrite.js")

  tags = local.tags
}

# PRD §10: one distribution, default behavior -> private site bucket.
# Managed policy ids:
#   CachingOptimized     658327ea-f89d-4fab-a63d-7e88639e58f6
#   SecurityHeadersPolicy 67f7725c-6f97-4210-82d7-5512b31e9d03
resource "aws_cloudfront_distribution" "cdn" {
  enabled             = true
  is_ipv6_enabled     = true
  http_version        = "http2and3"
  price_class         = "PriceClass_100"
  default_root_object = var.default_root_object
  comment             = "${var.project}-${var.env} read path"

  origin {
    domain_name              = var.site_bucket_regional_domain_name
    origin_id                = local.origin_id
    origin_access_control_id = aws_cloudfront_origin_access_control.site.id
  }

  # PRD §9: immutable cover objects, read through OAC.
  origin {
    domain_name              = var.media_bucket_regional_domain_name
    origin_id                = local.media_origin_id
    origin_access_control_id = aws_cloudfront_origin_access_control.media.id
  }

  # PRD §10: API Gateway HTTP API, reached over HTTPS only (no OAC; the HTTP
  # API endpoint is public and the Lambda authorizes/rate-limits).
  origin {
    domain_name = var.api_domain_name
    origin_id   = local.api_origin_id

    custom_origin_config {
      http_port              = 80
      https_port             = 443
      origin_protocol_policy = "https-only"
      origin_ssl_protocols   = ["TLSv1.2"]
    }
  }

  default_cache_behavior {
    target_origin_id           = local.origin_id
    viewer_protocol_policy     = "redirect-to-https"
    allowed_methods            = ["GET", "HEAD"]
    cached_methods             = ["GET", "HEAD"]
    compress                   = true
    cache_policy_id            = "658327ea-f89d-4fab-a63d-7e88639e58f6"
    response_headers_policy_id = "67f7725c-6f97-4210-82d7-5512b31e9d03"

    function_association {
      event_type   = "viewer-request"
      function_arn = aws_cloudfront_function.rewrite.arn
    }
  }

  # PRD §9/§10: /media/* is an immutable-key object path. GET/HEAD only, no
  # viewer-request rewrite (media keys are real keys), and the managed
  # SecurityHeadersPolicy supplies nosniff. Media paths are never invalidated.
  ordered_cache_behavior {
    path_pattern           = "/media/*"
    target_origin_id       = local.media_origin_id
    viewer_protocol_policy = "redirect-to-https"
    allowed_methods        = ["GET", "HEAD"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id            = "658327ea-f89d-4fab-a63d-7e88639e58f6" # CachingOptimized
    response_headers_policy_id = "67f7725c-6f97-4210-82d7-5512b31e9d03" # SecurityHeadersPolicy
  }

  # PRD §4/§10: /api/* never enters a shared cache and must forward every
  # header except Host (AllViewer would break API Gateway). The managed
  # policies also carry CloudFront-Viewer-Address, the trusted client IP.
  ordered_cache_behavior {
    path_pattern           = "/api/*"
    target_origin_id       = local.api_origin_id
    viewer_protocol_policy = "https-only"
    allowed_methods        = ["GET", "HEAD", "OPTIONS", "PUT", "POST", "PATCH", "DELETE"]
    cached_methods         = ["GET", "HEAD"]
    compress               = true

    cache_policy_id          = "4135ea2d-6df8-44a3-9df3-4b5a84be39ad" # CachingDisabled
    origin_request_policy_id = "b689b0a8-53d0-40ab-baf2-68738e2966ac" # AllViewerExceptHostHeader
  }

  # With OAC and no s3:ListBucket, S3 answers a missing object with 403.
  # Mapping 403 to the same 404 page is mandatory (PRD §10).
  custom_error_response {
    error_code            = 403
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 10
  }

  custom_error_response {
    error_code            = 404
    response_code         = 404
    response_page_path    = "/404.html"
    error_caching_min_ttl = 10
  }

  restrictions {
    geo_restriction {
      restriction_type = "none"
    }
  }

  viewer_certificate {
    cloudfront_default_certificate = true
  }

  tags = local.tags
}

# PRD §10 dependency-cycle break: the API Lambda resolves the distribution id
# from this named parameter at init instead of Terraform wiring cdn -> api.
resource "aws_ssm_parameter" "distribution_id" {
  name  = var.distribution_id_param_name
  type  = "String"
  value = aws_cloudfront_distribution.cdn.id

  tags = local.tags
}

# PRD §10 observability: CloudFront 5xx alarm. CloudFront publishes its metrics
# in us-east-1, so this alarm is created there through the aliased provider.
resource "aws_cloudwatch_metric_alarm" "cdn_5xx" {
  provider = aws.us_east_1

  alarm_name        = "${var.project}-${var.env}-cdn-5xx"
  alarm_description = "The CloudFront distribution is returning 5xx responses."
  namespace         = "AWS/CloudFront"
  metric_name       = "5xxErrorRate"
  dimensions = {
    DistributionId = aws_cloudfront_distribution.cdn.id
    Region         = "Global"
  }
  statistic           = "Average"
  period              = 60
  evaluation_periods  = 1
  comparison_operator = "GreaterThanOrEqualToThreshold"
  threshold           = var.alarm_threshold
  treat_missing_data  = "notBreaching"

  alarm_actions = local.alarm_actions

  tags = local.tags
}
