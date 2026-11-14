terraform {
  required_version = ">= 1.10.0"

  required_providers {
    aws = {
      source  = "hashicorp/aws"
      version = "~> 6.0"
      # CloudFront metrics are published in us-east-1; the CDN module needs a
      # provider for that region.
      configuration_aliases = [aws.us_east_1]
    }
  }
}
