#!/usr/bin/env bash
# Runs on the server (over SSM) after the release tarball is unpacked into
# /opt/buzz. Logs in to ECR, renders deploy/compose/.env from SSM, and runs the
# Compose upgrade with TLS. Then it renders and starts the agents and stops the
# agent projects that left AGENTS.
# Usage: remote-deploy.sh <tag>
# Environment: BUZZ_DOMAIN, RELAY_OWNER_PUBKEY, ECR_REPO (the relay repository
# URL). AGENTS (empty or unset: no agents), AGENTS_ECR_REPO (required when AGENTS
# is set; the agents image is $AGENTS_ECR_REPO:<tag>), AGENT_RESPOND_TO and
# AGENT_RESPOND_TO_ALLOWLIST (read by render-agents-env.sh), AGENT_SETTLE_SECONDS
# (how long an agent must stay up, default 15). BUZZ_HOME overrides /opt/buzz for
# tests.
# The whole agent phase (render, start, stale cleanup) runs after `run.sh upgrade`
# returns, which waits for the relay to be healthy. The first deploy has no agent
# keys or credentials yet, because registering an agent needs a running relay. So
# a failed render leaves the relay up and fails the deploy with a message that
# says what to run.
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
AGENTS="${AGENTS:-}"
AGENT_SETTLE_SECONDS="${AGENT_SETTLE_SECONDS:-15}"
export BUZZ_DOMAIN RELAY_OWNER_PUBKEY AGENTS

compose_dir="$BUZZ_HOME/deploy/compose"
[ -d "$compose_dir" ] || die "release is not unpacked: $compose_dir not found"

agents_dir="$BUZZ_HOME/agents"
agents_list="$agents_dir/agents.list"
agents_compose="$BUZZ_HOME/deploy/aws/compose.agents.yml"
failed=0
if [ -n "$AGENTS" ]; then
  : "${AGENTS_ECR_REPO:?AGENTS_ECR_REPO must be set when AGENTS is set}"
  # A release from before agents existed has no deploy/aws.
  [ -f "$agents_compose" ] && [ -f "$BUZZ_HOME/scripts/aws/render-agents-env.sh" ] \
    || die "release has no deploy/aws agent files, so AGENTS cannot start (deploy a newer release, or set AGENTS= in .aws.env)"
fi

# render_agents: writes the per-agent env files and staged config under
# $agents_dir. With AGENTS empty it removes them instead, because they hold keys.
render_agents() {
  if [ -z "$AGENTS" ]; then
    if [ -d "$agents_dir" ]; then
      find "$agents_dir" -mindepth 1 -maxdepth 1 -exec rm -rf {} + || return 1
    fi
    return 0
  fi
  bash "$BUZZ_HOME/scripts/aws/render-agents-env.sh" "$agents_dir" "$AGENTS_ECR_REPO:$tag" "$BUZZ_HOME/deploy/aws/agents" || return 1
}

# agent_compose <name> <compose args...>: runs Compose in one agent's project.
agent_compose() {
  local name="$1"
  shift
  docker compose -p "buzz-agent-$name" -f "$agents_compose" --env-file "$agents_dir/$name.compose.env" "$@"
}

# agent_failed <name> <message>: prints the agent's recent logs, then the
# message, to stderr (SSM returns the end of stderr) and marks the deploy failed.
agent_failed() {
  echo "--- last log lines of agent $1 ---" >&2
  agent_compose "$1" logs --no-color --tail 30 agent >&2 || true
  echo "error: $2" >&2
  failed=1
}

# start_agents: starts every agent in agents.list, waits AGENT_SETTLE_SECONDS,
# then checks each one is still running. One bad agent does not stop the others.
start_agents() {
  local name started=""
  [ -f "$agents_list" ] || return 0
  while IFS= read -r name || [ -n "$name" ]; do
    [ -n "$name" ] || continue
    if agent_compose "$name" up -d; then
      started="$started $name"
    else
      agent_failed "$name" "could not start agent $name"
    fi
  done < "$agents_list"
  [ -z "$started" ] || sleep "$AGENT_SETTLE_SECONDS"
  for name in $started; do
    [ -n "$(agent_compose "$name" ps --status running -q agent)" ] \
      || agent_failed "$name" "agent $name did not stay running"
  done
}

# stop_stale_agents: stops every buzz-agent-* Compose project whose agent is not
# in agents.list. down without -v keeps the project's volumes.
stop_stale_agents() {
  local projects project name wanted
  wanted=$'\n'"$(cat "$agents_list" 2> /dev/null || true)"$'\n'
  projects="$(docker ps -a --filter label=com.docker.compose.project --format '{{.Label "com.docker.compose.project"}}' | sort -u)"
  while IFS= read -r project; do
    case "$project" in buzz-agent-?*) ;; *) continue ;; esac
    name="${project#buzz-agent-}"
    case "$wanted" in
      *$'\n'"$name"$'\n'*) continue ;;
    esac
    echo "stopping agent project $project (not in AGENTS)"
    # Run from / so Compose finds no compose file and uses the project's labels.
    (cd / && docker compose -p "$project" down)
  done <<< "$projects"
}

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

if ! render_agents; then
  echo "error: the relay is up, but the agents could not be set up. Run make agents-secrets and make agents-register, then make deploy again." >&2
  exit 1
fi
start_agents
stop_stale_agents
[ "$failed" -eq 0 ] || exit 1
