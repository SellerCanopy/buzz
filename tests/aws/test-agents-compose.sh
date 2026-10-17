#!/usr/bin/env bash
# Tests deploy/aws/compose.agents.yml by rendering it with the real
# "docker compose config" and dummy env files. Nothing is pulled or started.
# Skips (exit 0) when docker compose or jq is unavailable. It does not source
# helpers.sh because that puts a fake docker first on PATH.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
YML="$REPO_ROOT/deploy/aws/compose.agents.yml"

for _v in $(compgen -v | grep -E '^(AWS_|TF_|GIT_|BUZZ_|AGENT|COMPOSE_|DOCKER_)'); do
  unset "$_v"
done
unset _v

if ! docker compose version >/dev/null 2>&1; then
  echo "skip - docker compose is not available"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "skip - jq is not available"
  exit 0
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

# write_agent <name>: dummy runtime env file, config dir, and compose env file.
write_agent() {
  local name="$1"
  mkdir -p "$WORK/$name-config"
  echo "BUZZ_ACP_AGENT_COMMAND=claude-agent-acp" > "$WORK/$name.agent.env"
  {
    echo "AGENTS_IMAGE=buzz-agents:test"
    echo "AGENT_NAME=$name"
    echo "BUZZ_DOMAIN=buzz.example.com"
    echo "AGENT_ENV_FILE=$WORK/$name.agent.env"
    echo "AGENT_CONFIG_DIR=$WORK/$name-config"
  } > "$WORK/$name.env"
}

# render <compose-file> <name>: the rendered project as JSON on stdout.
render() {
  docker compose -p "buzz-agent-$2" -f "$1" --env-file "$WORK/$2.env" config --format json
}

# check_render <compose-file>: every run rule must hold for both agents.
check_render() {
  local file="$1" a b
  a="$(render "$file" reviewer 2>&1)" || { echo "$a" >&2; return 1; }
  b="$(render "$file" docs 2>&1)" || { echo "$b" >&2; return 1; }

  [ "$(jq -r '.services | keys | join(",")' <<< "$a")" = "agent" ] || return 1
  [ "$(jq -r '.services.agent.restart' <<< "$a")" = "on-failure" ] || return 1
  [ "$(jq -r '.services.agent.stop_grace_period' <<< "$a")" = "1m0s" ] || return 1
  jq -e '.services.agent.extra_hosts | index("buzz.example.com=host-gateway")' <<< "$a" >/dev/null || return 1
  jq -e --arg d "$WORK/reviewer-config" \
    '.services.agent.volumes[] | select(.target == "/etc/buzz-agent" and .source == $d and .read_only == true)' \
    <<< "$a" >/dev/null || return 1
  jq -e '.services.agent.volumes[] | select(.target == "/home/agent" and .type == "volume")' <<< "$a" >/dev/null || return 1
  ! grep -q 'docker.sock' <<< "$a" || return 1
  [ "$(jq -r '.volumes.home.name' <<< "$a")" = "buzz-agent-reviewer_home" ] || return 1
  [ "$(jq -r '.networks.default.name' <<< "$a")" = "buzz-agent-reviewer_default" ] || return 1

  # The second project differs in volume, network, and config mount.
  [ "$(jq -r '.volumes.home.name' <<< "$b")" = "buzz-agent-docs_home" ] || return 1
  [ "$(jq -r '.networks.default.name' <<< "$b")" = "buzz-agent-docs_default" ] || return 1
  jq -e --arg d "$WORK/docs-config" \
    '.services.agent.volumes[] | select(.target == "/etc/buzz-agent" and .source == $d)' \
    <<< "$b" >/dev/null || return 1
  [ "$(jq -r '.services.agent.labels["buzz.agent"]' <<< "$b")" = "docs" ] || return 1
}

write_agent reviewer
write_agent docs

if check_render "$YML"; then
  pass "compose renders the run rules for two agents with separate volumes and mounts"
else
  bad "compose renders the run rules for two agents with separate volumes and mounts"
fi

# Each rule must be falsifiable: a copy of the file without it must fail the check.
mutant() {
  local name="$1" expr="$2" f="$WORK/mutant-$1.yml"
  sed -E "$expr" "$YML" > "$f"
  if cmp -s "$YML" "$f"; then bad "mutation '$name' changed nothing"; return; fi
  if check_render "$f" >/dev/null 2>&1; then bad "mutant survives: $name"; else pass "mutant fails: $name"; fi
}
mutant restart-always 's/restart: on-failure/restart: always/'
mutant grace-period 's/stop_grace_period: 60s/stop_grace_period: 10s/'
mutant no-extra-hosts 's/host-gateway/127.0.0.1/'
mutant config-writable 's#:/etc/buzz-agent:ro#:/etc/buzz-agent#'
mutant docker-socket 's#^(      - home:/home/agent)#      - /var/run/docker.sock:/var/run/docker.sock\n\1#'
mutant shared-home-volume 's#- home:/home/agent#- shared:/home/agent#; s#^  home:#  shared:#'

# A missing variable fails the render instead of silently mounting nothing.
for var in AGENTS_IMAGE AGENT_NAME BUZZ_DOMAIN AGENT_ENV_FILE AGENT_CONFIG_DIR; do
  grep -v "^$var=" "$WORK/reviewer.env" > "$WORK/missing.env"
  if out="$(docker compose -p buzz-agent-reviewer -f "$YML" --env-file "$WORK/missing.env" config 2>&1)"; then
    bad "render without $var fails"
  elif grep -q "$var" <<< "$out"; then
    pass "render without $var fails and names it"
  else
    bad "render without $var fails and names it (got: $out)"
  fi
done

exit "$fail"
