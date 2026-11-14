#!/usr/bin/env bash
# Writes .env.hive for deploy/aws/compose.hive.yml. The two Beehive secrets live in
# SSM Parameter Store under /buzz/hive/ (the relay's render-env.sh reads /buzz/
# without recursion, so it never sees them). Missing ones are created, and existing
# ones are never read back to be replaced or changed. The file holds secrets, so it
# is written with mode 0600 and is ignored by git.
# Usage: hive-env.sh [outfile]      (default: .env.hive in the repository root)
# Reads BUZZ_DOMAIN and the other settings from .aws.env, as the other scripts do.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"

# name:bytes. Hex only, because the password is embedded in a database URL.
SECRETS=(
  BUZZ_HIVE_TOKEN_SECRET:32
  BUZZ_HIVE_DB_PASSWORD:24
)

require_tool aws
require_tool openssl
load_settings
out="${1:-$REPO_ROOT/.env.hive}"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

for entry in "${SECRETS[@]}"; do
  name="${entry%%:*}"
  bytes="${entry##*:}"
  param="/buzz/hive/$name"
  parameter_exists "$param" && continue
  # The value goes through a mode-0600 file so it never shows in a process list.
  (umask 077; printf '%s' "$(openssl rand -hex "$bytes")" > "$WORK/value")
  aws ssm put-parameter --name "$param" --type SecureString \
    --value "file://$WORK/value" > /dev/null
  rm -f "$WORK/value"
  echo "created $param"
done

params="$(aws ssm get-parameters-by-path --path /buzz/hive/ --with-decryption \
  --query 'Parameters[].[Name,Value]' --output text)"
token=""
password=""
while IFS=$'\t' read -r pname pvalue; do
  case "$pname" in
    /buzz/hive/BUZZ_HIVE_TOKEN_SECRET) token="$pvalue" ;;
    /buzz/hive/BUZZ_HIVE_DB_PASSWORD) password="$pvalue" ;;
  esac
done <<< "$params"
params=""
for entry in "${SECRETS[@]}"; do
  name="${entry%%:*}"
  var="token"
  [ "$name" = "BUZZ_HIVE_DB_PASSWORD" ] && var="password"
  [[ "${!var}" =~ ^[A-Za-z0-9]+$ ]] \
    || die "SSM parameter /buzz/hive/$name is missing or has characters outside [A-Za-z0-9] (the value is not shown)"
done

umask 077
tmp="$(mktemp "$out.XXXXXX")"
trap 'rm -rf "$WORK" "$tmp"' EXIT
{
  echo "BUZZ_HIVE_DATABASE_URL=postgres://buzz_hive:$password@postgres:5432/buzz"
  echo "BUZZ_HIVE_DB_PASSWORD=$password"
  echo "BUZZ_HIVE_TOKEN_SECRET=$token"
  echo "BUZZ_HIVE_PUBLIC_URL=https://$BUZZ_DOMAIN"
  echo "BUZZ_HIVE_COMMUNITY_HOST=$BUZZ_DOMAIN"
  echo "BUZZ_HIVE_ALLOWED_ORIGINS=https://$BUZZ_DOMAIN"
} > "$tmp"
chmod 0600 "$tmp"
mv "$tmp" "$out"
echo "wrote $out"
