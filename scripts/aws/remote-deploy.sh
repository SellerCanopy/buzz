#!/usr/bin/env bash
# Runs on the server (over SSM) after the release tarball is unpacked into
# /opt/buzz. Logs in to ECR, renders deploy/compose/.env from SSM, and runs the
# Compose upgrade with TLS.
# Usage: remote-deploy.sh <tag>
# Environment: BUZZ_DOMAIN, RELAY_OWNER_PUBKEY, ECR_REPO (the relay repository
# URL). BUZZ_HOME overrides /opt/buzz for tests.
set -euo pipefail
umask 077

die() {
  echo "error: $*" >&2
  exit 1
}

[ $# -eq 1 ] || die "usage: remote-deploy.sh <tag>"
tag="$1"
[[ "$tag" =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid tag: $tag"
: "${BUZZ_DOMAIN:?BUZZ_DOMAIN must be set}"
: "${RELAY_OWNER_PUBKEY:?RELAY_OWNER_PUBKEY must be set}"
: "${ECR_REPO:?ECR_REPO must be set}"
BUZZ_HOME="${BUZZ_HOME:-/opt/buzz}"
export BUZZ_DOMAIN RELAY_OWNER_PUBKEY

compose_dir="$BUZZ_HOME/deploy/compose"
[ -d "$compose_dir" ] || die "release is not unpacked: $compose_dir not found"

aws ecr get-login-password | docker login --username AWS --password-stdin "${ECR_REPO%%/*}"

# Render to a temp file in the same directory, then rename, so a failed render
# never replaces a working .env. mktemp creates the file with mode 0600.
tmp="$(mktemp "$compose_dir/.env.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
bash "$BUZZ_HOME/scripts/aws/render-env.sh" "$compose_dir/.env.example" "$ECR_REPO:$tag" > "$tmp"
chmod 600 "$tmp"
mv "$tmp" "$compose_dir/.env"

cd "$compose_dir"
BUZZ_COMPOSE_TLS=true ./run.sh upgrade
