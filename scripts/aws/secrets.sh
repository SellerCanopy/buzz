#!/usr/bin/env bash
# Creates the six relay secrets in SSM Parameter Store under /buzz/ when they
# are absent. Existing parameters are never read back, changed, or replaced.
# Usage: secrets.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"

# name:bytes. Hex only, because these values are embedded in DATABASE_URL and
# REDIS_URL, where '/', '+', and '=' break URL parsing.
SECRETS=(
  BUZZ_RELAY_PRIVATE_KEY:32
  BUZZ_GIT_HOOK_HMAC_SECRET:32
  POSTGRES_PASSWORD:24
  REDIS_PASSWORD:24
  BUZZ_S3_ACCESS_KEY:24
  BUZZ_S3_SECRET_KEY:24
)

require_tool aws
require_tool openssl
load_settings

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# parameter_exists <name>: exits the script on any error other than "not found".
parameter_exists() {
  local err rc=0
  err="$(aws ssm get-parameter --name "$1" --query Parameter.Name --output text 2>&1 >/dev/null)" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  case "$err" in
    *ParameterNotFound*) return 1 ;;
    *) die "cannot check $1: $err" ;;
  esac
}

created=0
kept=0
for entry in "${SECRETS[@]}"; do
  name="${entry%%:*}"
  bytes="${entry##*:}"
  param="/buzz/$name"
  if parameter_exists "$param"; then
    echo "kept    $param"
    kept=$((kept + 1))
    continue
  fi
  # The value goes through a mode-0600 file so it never shows in a process list.
  (umask 077; printf '%s' "$(openssl rand -hex "$bytes")" > "$WORK/value")
  aws ssm put-parameter --name "$param" --type SecureString \
    --value "file://$WORK/value" > /dev/null
  rm -f "$WORK/value"
  echo "created $param"
  created=$((created + 1))
done

echo "secrets: $created created, $kept kept"
