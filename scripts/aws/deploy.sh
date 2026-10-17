#!/usr/bin/env bash
# Builds, ships, and starts a release on the server, then records it as the
# last good image once the relay answers its health check.
# Usage: deploy.sh [tag]
#   no tag      the short git SHA of HEAD
#   ROLLBACK=1  skip the clean-tree check, build, push, and upload; redeploy an
#               earlier tag, or /buzz/last_good_image when no tag is given. A
#               failed backup prints a warning and the rollback continues.
#   DRY_RUN=1   run the checks and the backup, then print the SSM command
#               instead of building, pushing, uploading, or sending anything
# Other environment: BACKUP_CMD (default scripts/aws/backup.sh next to this
# file), POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait),
# HEALTH_INTERVAL and HEALTH_ATTEMPTS (seconds, count; health check wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

HEALTH_INTERVAL="${HEALTH_INTERVAL:-10}"
HEALTH_ATTEMPTS="${HEALTH_ATTEMPTS:-30}"
LAST_GOOD_PARAM="/buzz/last_good_image"

[ $# -le 1 ] || die "usage: deploy.sh [tag]"
tag="${1:-}"

require_tool aws
require_tool terraform
load_settings
cd "$REPO_ROOT"

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
ERR_FILE="$work/err"

# remote_script <ecr-repo> <bucket> <tag>: prints the commands the server runs,
# one per line.
remote_script() {
  local repo="$1" bucket="$2" t="$3"
  echo "set -eu"
  echo "export HOME=/root"
  echo "export AWS_DEFAULT_REGION='$AWS_REGION'"
  echo "export BUZZ_DOMAIN='$BUZZ_DOMAIN'"
  echo "export RELAY_OWNER_PUBKEY='$RELAY_OWNER_PUBKEY'"
  echo "export ECR_REPO='$repo'"
  # Task 9 adds AGENTS and the AGENT_RESPOND_TO* settings to this block.
  echo "mkdir -p /opt/buzz"
  echo "aws s3 cp 's3://$bucket/releases/$t.tgz' /opt/buzz/release.tgz"
  echo "tar xzf /opt/buzz/release.tgz --no-same-owner -C /opt/buzz"
  echo "bash /opt/buzz/scripts/aws/remote-deploy.sh '$t'"
}

# wait_for_health: polls the relay's liveness URL until it answers.
wait_for_health() {
  local url="https://$BUZZ_DOMAIN/_liveness" i
  for ((i = 0; i < HEALTH_ATTEMPTS; i++)); do
    if curl -fsS --max-time 10 "$url" > /dev/null 2>&1; then
      echo "health check passed: $url"
      return 0
    fi
    sleep "$HEALTH_INTERVAL"
  done
  die "health check failed after $HEALTH_ATTEMPTS attempts: $url ($LAST_GOOD_PARAM not updated)"
}

if [ "${ROLLBACK:-}" = "1" ]; then
  if [ -z "$tag" ]; then
    rc=0
    tag="$(aws ssm get-parameter --name "$LAST_GOOD_PARAM" --query Parameter.Value --output text 2> "$ERR_FILE")" || rc=$?
    if [ "$rc" -ne 0 ] && ! grep -q ParameterNotFound "$ERR_FILE"; then
      die "cannot read $LAST_GOOD_PARAM: $(cat "$ERR_FILE")"
    fi
    case "$tag" in "" | None) die "no recorded good image ($LAST_GOOD_PARAM is empty)" ;; esac
  fi
else
  dirty="$(git -C "$REPO_ROOT" status --porcelain)"
  [ -z "$dirty" ] || die "working tree is not clean, commit or stash first so the image maps to a commit:
$dirty"
  [ -n "$tag" ] || tag="$(git -C "$REPO_ROOT" rev-parse --short HEAD)"
fi
[[ "$tag" =~ ^[A-Za-z0-9._-]+$ ]] || die "tag may contain only letters, digits, '.', '_' and '-': $tag"

require_tool curl
ecr_repo="$(tf_output ecr_repository_url)"
bucket="$(tf_output backup_bucket)"
instance="$(tf_output instance_id)"
[ -n "$ecr_repo" ] && [ -n "$bucket" ] && [ -n "$instance" ] \
  || die "terraform outputs are empty (run make apply first)"

echo "deploy: tag $tag to $BUZZ_DOMAIN"

backup_cmd="${BACKUP_CMD:-$SCRIPT_DIR/backup.sh}"
if ! bash "$backup_cmd"; then
  if [ "${ROLLBACK:-}" = "1" ]; then
    echo "WARNING: backup failed. Continuing because ROLLBACK=1; the current data is NOT backed up." >&2
  else
    die "backup failed, deploy aborted before any change"
  fi
fi

if [ "${ROLLBACK:-}" = "1" ]; then
  aws s3api head-object --bucket "$bucket" --key "releases/$tag.tgz" > /dev/null \
    || die "no release tarball at s3://$bucket/releases/$tag.tgz, cannot roll back to $tag"
fi

if [ "${DRY_RUN:-}" = "1" ]; then
  echo "dry run: would build, push, and upload release $tag; sending nothing."
  echo "SSM command (dry run):"
  echo "aws ssm send-command --instance-ids $instance --document-name AWS-RunShellScript --parameters <below>"
  remote_script "$ecr_repo" "$bucket" "$tag"
  exit 0
fi

if [ "${ROLLBACK:-}" != "1" ]; then
  bash "$SCRIPT_DIR/build.sh" "$tag" --push

  # Stage the files so every local .env* file except .env.example can be
  # dropped and no secret ships. COPYFILE_DISABLE stops macOS tar from adding
  # AppleDouble files.
  stage="$work/stage"
  mkdir -p "$stage/deploy" "$stage/scripts/aws"
  cp -R "$REPO_ROOT/deploy/compose" "$stage/deploy/compose"
  find "$stage/deploy/compose" -maxdepth 1 -name '.env*' ! -name '.env.example' -delete
  cp "$REPO_ROOT/scripts/aws/lib.sh" "$REPO_ROOT/scripts/aws/render-env.sh" \
    "$REPO_ROOT/scripts/aws/remote-deploy.sh" "$stage/scripts/aws/"
  COPYFILE_DISABLE=1 tar czf "$work/release.tgz" -C "$stage" deploy scripts
  aws s3 cp "$work/release.tgz" "s3://$bucket/releases/$tag.tgz"
fi

command_id="$(remote_script "$ecr_repo" "$bucket" "$tag" | ssm_send "$instance" "buzz deploy $tag")"
[ -n "$command_id" ] || die "send-command returned no command id"
echo "sent SSM command $command_id, waiting"

ssm_wait "$command_id" "$instance" || exit 1
wait_for_health

aws ssm put-parameter --name "$LAST_GOOD_PARAM" --type String --value "$tag" --overwrite > /dev/null
echo "deploy: $tag is live and recorded as $LAST_GOOD_PARAM"
