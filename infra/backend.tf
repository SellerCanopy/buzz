# Bucket, key, region, and use_lockfile are passed by `make init` through
# -backend-config, so the state bucket name stays in .aws.env.
terraform {
  backend "s3" {}
}
