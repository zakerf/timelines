provider "aws" {
  region = var.region
}

# CloudFront metrics are published in us-east-1, so the CDN 5xx alarm lives
# there even though the rest of the environment is eu-central-1 (PRD §10).
provider "aws" {
  alias  = "us_east_1"
  region = "us-east-1"
}
