#!/usr/bin/env bash
# Proves that each Claude agent loads only its own MCP config. It starts the
# real agents image three times, with an alpha config, a beta config, and no
# config. The Claude adapter runs for real with a dummy API key and no network.
# Each container's mcp.json names a probe MCP server that records its start in
# /tmp/probe-started. The test reads that file after the container exits.
#
# The test mounts a small script over sprig-entrypoint. The image's own
# agent-entrypoint runs unchanged (it installs the MCP config), and its final exec
# lands in a script that drives claude-agent-acp the way buzz-acp does: from
# $HOME, over stdio, with initialize then session/new. The session/new request also
# carries a stand-in for the buzz-dev-mcp entry that buzz-acp always sends, so the
# test shows the adapter merges that list with the project .mcp.json.
#
# Skips (exit 0) with SKIP_IMAGE=1 or when Docker is unavailable.
# Set SPRIG_IMAGE to reuse an already built sprig image instead of building one.
# It does not source helpers.sh because that puts a fake docker first on PATH.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

if [ "${SKIP_IMAGE:-}" = "1" ]; then
  echo "skip - SKIP_IMAGE=1"
  exit 0
fi

prebuilt_sprig="${SPRIG_IMAGE:-}"
for _v in $(compgen -v | grep -E '^(AWS_|TF_|GIT_|BUZZ_|AGENT|COMPOSE_|SPRIG_)'); do
  unset "$_v"
done
unset _v

if ! docker info >/dev/null 2>&1; then
  echo "skip - Docker is not available"
  exit 0
fi

PREFIX="buzz-t7-mcp-$$-$RANDOM"
AGENTS_TAG="$PREFIX-agents:test"
BUILT_SPRIG_TAG="$PREFIX-sprig:test"
built_sprig=0
WORK="$(mktemp -d)"
# shellcheck disable=SC2329 # runs through the EXIT trap
cleanup() {
  local c
  for c in $(docker ps -aq --filter "name=^$PREFIX-"); do docker rm -f "$c" >/dev/null 2>&1; done
  docker rmi -f "$AGENTS_TAG" >/dev/null 2>&1
  docker volume rm -f "$PREFIX-unreadable" >/dev/null 2>&1
  if [ "$built_sprig" = "1" ]; then docker rmi -f "$BUILT_SPRIG_TAG" >/dev/null 2>&1; fi
  rm -rf "$WORK"
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

# Per-agent config dirs, in the same shape Task 8 stages into AGENT_CONFIG_DIR.
FIXTURES="$REPO_ROOT/tests/aws/fixtures"
for name in alpha beta; do
  mkdir -p "$WORK/$name"
  cat > "$WORK/$name/mcp.json" <<JSON
{"mcpServers": {"$name": {"command": "node", "args": ["/fixtures/probe-mcp.js", "$name"]}}}
JSON
done
mkdir -p "$WORK/none"
cat > "$WORK/driver.sh" <<'DRIVER'
#!/bin/bash
exec node /fixtures/acp-client.js "$@"
DRIVER
chmod -R a+rX "$WORK"
chmod a+rx "$WORK/driver.sh"

# run_agent <label> <config-dir> [docker run options...]: runs one container to
# completion and leaves it in place so its files can be read.
run_agent() {
  local label="$1" cfg="$2"
  shift 2
  docker run --name "$PREFIX-$label" --network none \
    -e ANTHROPIC_API_KEY=sk-ant-dummy-not-a-real-key \
    -v "$FIXTURES:/fixtures:ro" \
    -v "$WORK/driver.sh:/usr/local/bin/sprig-entrypoint:ro" \
    -v "$cfg:/etc/buzz-agent:ro" \
    "$@" "$AGENTS_TAG" > "$WORK/$label.out" 2> "$WORK/$label.err"
  echo $? > "$WORK/$label.rc"
}

# started <label>: the probe names the container recorded, one per line, sorted.
started() {
  docker cp "$PREFIX-$1:/tmp/probe-started" - 2>/dev/null | tar -xO 2>/dev/null | sort -u
}

# session_ok <label>: the adapter answered session/new with a session id, so an
# empty probe list means "nothing started", not "nothing ran".
session_ok() {
  [ "$(cat "$WORK/$1.rc")" = "0" ] && grep -q '^session/new: .*"sessionId"' "$WORK/$1.err"
}

# An unreadable mcp.json must fail with its own message, not "not a valid .mcp.json".
# A bind-mounted host file does not enforce modes on every Docker host, so the file
# is created root-owned with mode 000 inside a named volume.
UNREADABLE_VOLUME="$PREFIX-unreadable"
docker volume create "$UNREADABLE_VOLUME" >/dev/null
docker run --rm --network none --user 0 -v "$UNREADABLE_VOLUME:/d" -v "$WORK/alpha:/src:ro" --entrypoint sh "$AGENTS_TAG" \
  -c 'cp /src/mcp.json /d/mcp.json && chown root:root /d/mcp.json && chmod 000 /d/mcp.json && chmod 755 /d' >/dev/null 2>&1

run_agent alpha "$WORK/alpha"
run_agent beta "$WORK/beta"
run_agent none "$WORK/none"
# Wrong config on purpose: alpha's expectation must not hold for beta's config.
run_agent crossed "$WORK/beta"
# Config mounted but the image's agent-entrypoint skipped: nothing installs it.
run_agent skipped "$WORK/alpha" --entrypoint /usr/local/bin/sprig-entrypoint

run_agent unreadable "$UNREADABLE_VOLUME"

for label in alpha beta none crossed skipped; do
  if session_ok "$label"; then
    pass "$label: claude-agent-acp answered initialize and session/new"
  else
    bad "$label: claude-agent-acp answered initialize and session/new (rc=$(cat "$WORK/$label.rc"), stderr: $(tail -c 600 "$WORK/$label.err"))"
  fi
done

check() {
  local label="$1" want="$2" got
  got="$(started "$label")"
  if [ "$got" = "$want" ]; then pass "$label started: '${want:-nothing}'"; else bad "$label started '${got:-nothing}', wanted '${want:-nothing}'"; fi
}
check alpha alpha
check beta beta
check none ""

# The assertions must be able to fail: the same expectation is checked against a
# container with the wrong config and one where the install step did not run.
if [ "$(started crossed)" = "alpha" ]; then bad "mutant survives: alpha expectation holds with beta's config"; else pass "mutant fails: alpha expectation does not hold with beta's config"; fi
if [ "$(started skipped)" = "alpha" ]; then bad "mutant survives: alpha started without agent-entrypoint"; else pass "mutant fails: alpha does not start without agent-entrypoint"; fi

# The installed file is the agent's own, and an agent without one gets an empty list.
out="$(docker cp "$PREFIX-none:/home/agent/.mcp.json" - 2>/dev/null | tar -xO 2>/dev/null | tr -d ' \n')"
if [ "$out" = '{"mcpServers":{}}' ]; then pass "none: empty .mcp.json installed"; else bad "none: empty .mcp.json installed (got: $out)"; fi

out="$(cat "$WORK/unreadable.err")"
rc="$(cat "$WORK/unreadable.rc")"
if [ "$rc" = "1" ] && grep -q 'cannot read /etc/buzz-agent/mcp.json' <<< "$out" && ! grep -q 'not a valid' <<< "$out"; then
  pass "unreadable mcp.json exits 1 and names the path"
else
  bad "unreadable mcp.json exits 1 and names the path (rc=$rc, got: $out)"
fi

exit "$fail"
