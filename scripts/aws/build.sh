#!/usr/bin/env bash
# Builds the relay image for linux/arm64 from the root Dockerfile, tagged
# <ecr-repo>:<tag>. This is the only place the build command lives.
# Usage: build.sh <tag> [--push]
# Without --push the image is loaded into the local Docker daemon. With --push
# the script logs in to ECR first and pushes the image instead.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"

usage() { die "usage: build.sh <tag> [--push]"; }

[ $# -ge 1 ] && [ $# -le 2 ] || usage
tag="$1"
mode="--load"
if [ $# -eq 2 ]; then
  [ "$2" = "--push" ] || usage
  mode="--push"
fi
[[ "$tag" =~ ^[A-Za-z0-9._-]+$ ]] || die "tag may contain only letters, digits, '.', '_' and '-': $tag"

require_tool docker
require_tool terraform
load_settings

repo="$(tf_output ecr_repository_url)"
[ -n "$repo" ] || die "terraform output ecr_repository_url is empty (run make apply first)"

if [ "$mode" = "--push" ]; then
  require_tool aws
  aws ecr get-login-password --region "$AWS_REGION" \
    | docker login --username AWS --password-stdin "${repo%%/*}"
fi

docker buildx build --platform linux/arm64 -t "$repo:$tag" "$mode" "$REPO_ROOT"
