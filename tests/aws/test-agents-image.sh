#!/usr/bin/env bash
# Builds the sprig image from Dockerfile.sprig and the agents image on top of it,
# then checks the tools, the non-root user, and that the Alpine-built sprig binary
# runs on the Debian-based agents image. Slow on a cold cache (it compiles Rust).
# Skips (exit 0) with SKIP_IMAGE=1 or when Docker is unavailable.
# Set SPRIG_IMAGE to reuse an already built sprig image instead of building one.
# It does not source helpers.sh because that puts a fake docker first on PATH.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

if [ "${SKIP_IMAGE:-}" = "1" ]; then
  echo "skip - SKIP_IMAGE=1"
  exit 0
fi

# Keep the caller's SPRIG_IMAGE, then drop other inherited settings.
prebuilt_sprig="${SPRIG_IMAGE:-}"
for _v in $(compgen -v | grep -E '^(AWS_|TF_|GIT_|BUZZ_|AGENT|COMPOSE_|SPRIG_)'); do
  unset "$_v"
done
unset _v

if ! docker info >/dev/null 2>&1; then
  echo "skip - Docker is not available"
  exit 0
fi

PREFIX="buzz-t7-image-$$-$RANDOM"
AGENTS_TAG="$PREFIX-agents:test"
BUILT_SPRIG_TAG="$PREFIX-sprig:test"
built_sprig=0
# shellcheck disable=SC2329 # runs through the EXIT trap
cleanup() {
  docker rmi -f "$AGENTS_TAG" >/dev/null 2>&1
  if [ "$built_sprig" = "1" ]; then docker rmi -f "$BUILT_SPRIG_TAG" >/dev/null 2>&1; fi
}
trap cleanup EXIT

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

cd "$REPO_ROOT" || exit 1

if [ -n "$prebuilt_sprig" ]; then
  sprig="$prebuilt_sprig"
else
  sprig="$BUILT_SPRIG_TAG"
  built_sprig=1
  if ! docker build -q -f Dockerfile.sprig -t "$sprig" . >/dev/null; then
    bad "build the sprig image from Dockerfile.sprig"
    exit 1
  fi
fi
if ! docker build -q -f deploy/aws/Dockerfile.agents --build-arg "SPRIG_IMAGE=$sprig" -t "$AGENTS_TAG" . >/dev/null; then
  bad "build the agents image from Dockerfile.agents"
  exit 1
fi
pass "agents image builds on top of the sprig image"

# run_in_image <command...>: runs a command in the image, bypassing the entrypoint.
run_in_image() {
  docker run --rm --entrypoint "" "$AGENTS_TAG" "$@"
}

out="$(run_in_image bash -c 'command -v buzz-acp claude-agent-acp codex-acp' 2>&1)"; rc=$?
if [ "$rc" -eq 0 ] && [ "$(wc -l <<< "$out" | tr -d ' ')" = "3" ]; then
  pass "buzz-acp, claude-agent-acp, and codex-acp are on PATH"
else
  bad "buzz-acp, claude-agent-acp, and codex-acp are on PATH (rc=$rc, got: $out)"
fi

for tool in buzz-agent buzz-dev-mcp rg tree buzz git-credential-nostr git-sign-nostr git bash; do
  if run_in_image bash -c "command -v $tool" >/dev/null 2>&1; then
    pass "$tool is on PATH"
  else
    bad "$tool is on PATH"
  fi
done

out="$(run_in_image id -u 2>&1)"
if [ -n "$out" ] && [ "$out" != "0" ]; then
  pass "container runs as a non-root user (uid $out)"
else
  bad "container runs as a non-root user (got: $out)"
fi
# shellcheck disable=SC2016 # expanded inside the container
out="$(run_in_image bash -c 'echo "$(id -un) $HOME $(pwd)"' 2>&1)"
if [ "$out" = "agent /home/agent /home/agent" ]; then
  pass "user is agent with HOME and working directory /home/agent"
else
  bad "user is agent with HOME and working directory /home/agent (got: $out)"
fi

out="$(run_in_image buzz-acp --help 2>&1)"; rc=$?
if [ "$rc" -eq 0 ]; then
  pass "buzz-acp --help exits 0 (the Alpine-built binary runs on Debian)"
else
  bad "buzz-acp --help exits 0 (rc=$rc, got: $(head -c 300 <<< "$out"))"
fi

# The pinned Claude Code version. Update it together with the ARG in Dockerfile.agents.
want_cc="2.1.302"
out="$(run_in_image claude --version 2>&1)"
if [ -n "$want_cc" ] && [ "$out" = "$want_cc (Claude Code)" ]; then
  pass "claude --version prints the pinned $want_cc"
else
  bad "claude --version prints the pinned '$want_cc' (got: $out)"
fi
# The adapter reads CLAUDE_CODE_EXECUTABLE; it must be an executable that reports the pin.
# shellcheck disable=SC2016 # expanded inside the container
out="$(run_in_image bash -c 'test -n "$CLAUDE_CODE_EXECUTABLE" && test -x "$CLAUDE_CODE_EXECUTABLE" && "$CLAUDE_CODE_EXECUTABLE" --version' 2>&1)"
if [ -n "$want_cc" ] && [ "$out" = "$want_cc (Claude Code)" ]; then
  pass "CLAUDE_CODE_EXECUTABLE is an executable running Claude Code $want_cc"
else
  bad "CLAUDE_CODE_EXECUTABLE is an executable running Claude Code '$want_cc' (got: $out)"
fi

# The image entrypoint installs the MCP config and ends in the shared entrypoint.
out="$(docker inspect -f '{{json .Config.Entrypoint}}' "$AGENTS_TAG")"
if [ "$out" = '["/usr/local/bin/agent-entrypoint"]' ]; then
  pass "image entrypoint is agent-entrypoint"
else
  bad "image entrypoint is agent-entrypoint (got: $out)"
fi

exit "$fail"
