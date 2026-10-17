#!/usr/bin/env bash
# Builds the relay image for linux/arm64 from the root Dockerfile, tagged
# <ecr-repo>:<tag>. This is the only place the build commands live.
# Usage: build.sh <tag> [--push]
# Without --push the image is loaded into the local Docker daemon. With --push
# the script logs in to ECR first and pushes the image instead.
# When AGENTS is not empty it then builds the sprig image from Dockerfile.sprig
# (kept local as buzz-sprig:<tag>, never pushed) and the agents image from
# deploy/aws/Dockerfile.agents on top of it, tagged <agents-ecr-repo>:<tag>. The
# agents build reads the sprig image from the local Docker daemon, which needs
# Docker's default builder (the "docker" driver).
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
agents_repo=""
if [ -n "$AGENTS" ]; then
  agents_repo="$(tf_output agents_ecr_repository_url)"
  [ -n "$agents_repo" ] || die "terraform output agents_ecr_repository_url is empty (run make apply first)"
fi

if [ "$mode" = "--push" ]; then
  require_tool aws
  aws ecr get-login-password --region "$AWS_REGION" \
    | docker login --username AWS --password-stdin "${repo%%/*}"
fi

docker buildx build --platform linux/arm64 -t "$repo:$tag" "$mode" "$REPO_ROOT"

if [ -n "$AGENTS" ]; then
  sprig_image="buzz-sprig:$tag"
  docker buildx build --platform linux/arm64 -f "$REPO_ROOT/Dockerfile.sprig" \
    -t "$sprig_image" --load "$REPO_ROOT"
  docker buildx build --platform linux/arm64 -f "$REPO_ROOT/deploy/aws/Dockerfile.agents" \
    --build-arg "SPRIG_IMAGE=$sprig_image" -t "$agents_repo:$tag" "$mode" "$REPO_ROOT"
fi
