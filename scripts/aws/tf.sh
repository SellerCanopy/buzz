#!/usr/bin/env bash
# Runs Terraform for the AWS deployment with settings from .aws.env.
# Usage: tf.sh init|plan|apply|destroy
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export REPO_ROOT
# shellcheck source=scripts/aws/lib.sh
. "$REPO_ROOT/scripts/aws/lib.sh"

PLAN_FILE=buzz.tfplan # relative to infra/, because of -chdir

# check_terraform: exits 1 unless terraform is on PATH and is 1.10 or newer.
check_terraform() {
  require_tool terraform
  local first
  first="$(terraform version | sed -n 1p)"
  if [[ ! "$first" =~ ^Terraform\ v([0-9]+)\.([0-9]+) ]]; then
    die "cannot read the terraform version from: $first"
  fi
  local major="${BASH_REMATCH[1]}" minor="${BASH_REMATCH[2]}"
  if [ "$major" -lt 1 ] || { [ "$major" -eq 1 ] && [ "$minor" -lt 10 ]; }; then
    die "terraform 1.10 or newer is required (found ${first#Terraform v}); 1.10 added the S3 backend lockfile"
  fi
}

# confirm <prompt> <expected>: exits 1 unless the typed line equals <expected>.
confirm() {
  local answer=""
  printf '%s' "$1" >&2
  read -r answer || true
  [ "$answer" = "$2" ] || die "aborted: input did not match '$2'"
}

cmd="${1:-}"
case "$cmd" in
  init | plan | apply | destroy) ;;
  *) die "usage: tf.sh init|plan|apply|destroy" ;;
esac

load_settings
cd "$REPO_ROOT"

tf_vars=(
  -var "aws_region=$AWS_REGION"
  -var "instance_type=$INSTANCE_TYPE"
  -var "domain=$BUZZ_DOMAIN"
  -var "route53_zone=$ROUTE53_ZONE"
)

case "$cmd" in
  init)
    check_terraform
    terraform -chdir=infra init \
      -backend-config="bucket=$TF_STATE_BUCKET" \
      -backend-config="key=buzz/terraform.tfstate" \
      -backend-config="region=$AWS_REGION" \
      -backend-config="use_lockfile=true"
    ;;
  plan)
    check_terraform
    terraform -chdir=infra fmt -check
    terraform -chdir=infra validate
    terraform -chdir=infra plan "${tf_vars[@]}"
    ;;
  apply)
    check_terraform
    terraform -chdir=infra fmt -check
    terraform -chdir=infra validate
    terraform -chdir=infra plan "${tf_vars[@]}" -out="$PLAN_FILE"
    confirm "Apply this plan? Type yes to continue: " yes
    terraform -chdir=infra apply "$PLAN_FILE"
    rm -f "infra/$PLAN_FILE"
    ;;
  destroy)
    confirm "This destroys the relay infrastructure. Type $BUZZ_DOMAIN to confirm: " "$BUZZ_DOMAIN"
    check_terraform
    terraform -chdir=infra destroy "${tf_vars[@]}"
    ;;
esac
