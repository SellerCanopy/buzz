#!/usr/bin/env bash
# Prints a complete deploy/compose .env to stdout. The output holds secrets, so
# redirect it to a file with mode 0600 and never log it.
# Usage: render-env.sh <template> <image>
# Reads BUZZ_DOMAIN and RELAY_OWNER_PUBKEY from the environment and the six
# secrets from SSM (non-recursive /buzz/, so /buzz/agents/* is not read).
# Comments and blank lines in the template are dropped.
# Exits 1, printing nothing to stdout, if any parameter, template line, or
# setting is missing or any CHANGE_ME value remains.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"

SECRET_NAMES="BUZZ_RELAY_PRIVATE_KEY BUZZ_GIT_HOOK_HMAC_SECRET POSTGRES_PASSWORD REDIS_PASSWORD BUZZ_S3_ACCESS_KEY BUZZ_S3_SECRET_KEY"
DERIVED_NAMES="BUZZ_IMAGE BUZZ_DOMAIN RELAY_OWNER_PUBKEY RELAY_URL BUZZ_MEDIA_BASE_URL BUZZ_MEDIA_SERVER_DOMAIN BUZZ_CORS_ORIGINS"

[ $# -eq 2 ] || die "usage: render-env.sh <template> <image>"
template="$1"
image="$2"
[ -f "$template" ] || die "template not found: $template"
[ -n "${BUZZ_DOMAIN:-}" ] || die "BUZZ_DOMAIN is not set"
[ -n "${RELAY_OWNER_PUBKEY:-}" ] || die "RELAY_OWNER_PUBKEY is not set"
[ -n "$image" ] || die "image is empty"
require_tool aws

# Secret values live in SECRET_<name> variables (bash 3.2 has no associative arrays).
params="$(aws ssm get-parameters-by-path --path /buzz/ --with-decryption \
  --query 'Parameters[].[Name,Value]' --output text)"
while IFS=$'\t' read -r pname pvalue; do
  [ -n "$pname" ] || continue
  for name in $SECRET_NAMES; do
    if [ "$pname" = "/buzz/$name" ]; then
      printf -v "SECRET_$name" '%s' "$pvalue"
    fi
  done
done <<< "$params"
params=""

missing=""
for name in $SECRET_NAMES; do
  var="SECRET_$name"
  [ -n "${!var:-}" ] || missing="$missing /buzz/$name"
done
[ -z "$missing" ] || die "missing SSM parameters:$missing (run make secrets)"

# value_for <name>: prints the value for a managed template key, or returns 1.
value_for() {
  local var
  case "$1" in
    BUZZ_IMAGE) printf '%s' "$image" ;;
    BUZZ_DOMAIN) printf '%s' "$BUZZ_DOMAIN" ;;
    RELAY_OWNER_PUBKEY) printf '%s' "$RELAY_OWNER_PUBKEY" ;;
    RELAY_URL) printf 'wss://%s' "$BUZZ_DOMAIN" ;;
    BUZZ_MEDIA_BASE_URL) printf 'https://%s/media' "$BUZZ_DOMAIN" ;;
    BUZZ_MEDIA_SERVER_DOMAIN) printf '%s' "$BUZZ_DOMAIN" ;;
    BUZZ_CORS_ORIGINS) printf 'https://%s' "$BUZZ_DOMAIN" ;;
    *)
      case " $SECRET_NAMES " in
        *" $1 "*)
          var="SECRET_$1"
          printf '%s' "${!var}"
          ;;
        *) return 1 ;;
      esac
      ;;
  esac
}

out=""
seen=" "
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    "" | [[:space:]]* | "#"*) continue ;;
  esac
  key="${line%%=*}"
  if value="$(value_for "$key")"; then
    line="$key=$value"
    seen="$seen$key "
  fi
  out="$out$line"$'\n'
done < "$template"

for name in $SECRET_NAMES $DERIVED_NAMES; do
  case "$seen" in
    *" $name "*) ;;
    *) die "template has no $name line: $template" ;;
  esac
done

# Same rule as deploy/compose/run.sh: an assignment that still holds CHANGE_ME.
left="$(printf '%s' "$out" | grep -E '^[A-Za-z_][A-Za-z0-9_]*=.*CHANGE_ME' | cut -d= -f1 || true)"
[ -z "$left" ] || die "CHANGE_ME values remain for: $(printf '%s' "$left" | tr '\n' ' ')"

printf '%s' "$out"
