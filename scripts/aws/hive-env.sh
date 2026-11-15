#!/usr/bin/env bash
# Writes .env.hive for deploy/aws/compose.hive.yml. The three Beehive secrets live in
# SSM Parameter Store under /buzz/hive/ (the relay's render-env.sh reads /buzz/
# without recursion, so it never sees them): the token secret, the database password
# and BUZZ_HIVE_NOSTR_KEY, the key of the Beehive account that posts stage tasks.
# Missing ones are created, and existing ones are never read back to be replaced or
# changed. The file holds secrets, so it is written with mode 0600 and is ignored by git.
# Usage: hive-env.sh [outfile]      (default: .env.hive in the repository root)
# Reads BUZZ_DOMAIN and the other settings from .aws.env, as the other scripts do.
# Two settings are optional there:
#   BUZZ_HIVE_RELAY_URL  the relay's WebSocket address inside the stack (default ws://relay:3000)
#   POSTGRES_DB          the database name, the same value deploy/compose/.env sets (default buzz)
# Without BUZZ_HIVE_NOSTR_KEY and BUZZ_HIVE_RELAY_URL in .env.hive the service starts but
# cannot start a run. Add the Beehive account's public key to the relay's members so its
# posts are accepted.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"

# name:bytes. Hex only: the password is embedded in a database URL, and the key is 64 hex
# characters, which the service accepts as a secret key.
SECRETS=(
  BUZZ_HIVE_TOKEN_SECRET:32
  BUZZ_HIVE_DB_PASSWORD:24
  BUZZ_HIVE_NOSTR_KEY:32
)

require_tool aws
require_tool openssl
load_settings
out="${1:-$REPO_ROOT/.env.hive}"
db_name="${POSTGRES_DB:-buzz}"
relay_url="${BUZZ_HIVE_RELAY_URL:-ws://relay:3000}"
[[ "$db_name" =~ ^[A-Za-z0-9_]+$ ]] || die "POSTGRES_DB must be letters, digits and underscores: $db_name"
[[ "$relay_url" =~ ^wss?://[A-Za-z0-9.:-]+$ ]] || die "BUZZ_HIVE_RELAY_URL must be a ws:// or wss:// address: $relay_url"

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
nostr_key=""
while IFS=$'\t' read -r pname pvalue; do
  case "$pname" in
    /buzz/hive/BUZZ_HIVE_TOKEN_SECRET) token="$pvalue" ;;
    /buzz/hive/BUZZ_HIVE_DB_PASSWORD) password="$pvalue" ;;
    /buzz/hive/BUZZ_HIVE_NOSTR_KEY) nostr_key="$pvalue" ;;
  esac
done <<< "$params"
params=""
for entry in "${SECRETS[@]}"; do
  name="${entry%%:*}"
  case "$name" in
    BUZZ_HIVE_TOKEN_SECRET) var="token" ;;
    BUZZ_HIVE_DB_PASSWORD) var="password" ;;
    BUZZ_HIVE_NOSTR_KEY) var="nostr_key" ;;
  esac
  [[ "${!var}" =~ ^[A-Za-z0-9]+$ ]] \
    || die "SSM parameter /buzz/hive/$name is missing or has characters outside [A-Za-z0-9] (the value is not shown)"
done

umask 077
tmp="$(mktemp "$out.XXXXXX")"
trap 'rm -rf "$WORK" "$tmp"' EXIT
{
  echo "BUZZ_HIVE_DATABASE_URL=postgres://buzz_hive:$password@postgres:5432/$db_name"
  echo "BUZZ_HIVE_DB_PASSWORD=$password"
  echo "BUZZ_HIVE_TOKEN_SECRET=$token"
  echo "BUZZ_HIVE_NOSTR_KEY=$nostr_key"
  echo "BUZZ_HIVE_RELAY_URL=$relay_url"
  echo "BUZZ_HIVE_PUBLIC_URL=https://$BUZZ_DOMAIN"
  echo "BUZZ_HIVE_COMMUNITY_HOST=$BUZZ_DOMAIN"
  echo "BUZZ_HIVE_ALLOWED_ORIGINS=https://$BUZZ_DOMAIN"
} > "$tmp"
chmod 0600 "$tmp"
mv "$tmp" "$out"
echo "wrote $out"
