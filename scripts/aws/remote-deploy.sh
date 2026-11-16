#!/usr/bin/env bash
# Runs on the server (over SSM) after the release tarball is unpacked into
# /opt/buzz. Logs in to ECR, renders deploy/compose/.env from SSM, and runs the
# Compose upgrade with TLS. Then it renders and starts the agents and stops the
# agent projects that left AGENTS.
# With BUZZ_HIVE_IMAGE set and /opt/buzz/.env.hive present, it also installs the host
# supervisor (deploy/aws/buzz-hive-supervisor.service) from that image, and asks the engine
# to apply every agent's tool servers through it. That step warns and never fails the deploy.
# Usage: remote-deploy.sh <tag>
# Environment: BUZZ_DOMAIN, RELAY_OWNER_PUBKEY, ECR_REPO (the relay repository
# URL). AGENTS (empty or unset: no agents), AGENTS_ECR_REPO (required when AGENTS
# is set; the agents image is $AGENTS_ECR_REPO:<tag>), AGENT_RESPOND_TO and
# AGENT_RESPOND_TO_ALLOWLIST (read by render-agents-env.sh), AGENT_SETTLE_SECONDS
# (how long an agent must stay up, default 15). BUZZ_HOME overrides /opt/buzz and BUZZ_ETC_DIR
# overrides /etc for tests.
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
BUZZ_ETC_DIR="${BUZZ_ETC_DIR:-/etc}"
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
    # --force-recreate so a changed secret in the env file takes effect even when
    # the image tag is unchanged.
    if agent_compose "$name" up -d --force-recreate; then
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

# supervise_agents: installs the Beehive host supervisor and has the engine apply every
# agent's tool servers and settings. A deploy renders each agent's env file without those
# settings and recreates the container, so the apply puts them back (and restarts an agent
# only when its files differ). Nothing here fails the deploy: get_hive_health reports an agent
# that is not set right.
supervise_agents() {
  local image="${BUZZ_HIVE_IMAGE:-}" env_file="$BUZZ_HOME/.env.hive" unit_src unit_env keys cid hive
  unit_src="$BUZZ_HOME/deploy/aws/buzz-hive-supervisor.service"
  unit_env="$BUZZ_ETC_DIR/buzz-hive-supervisor.env"
  if [ -z "$image" ] || [ ! -f "$env_file" ] || [ ! -f "$unit_src" ]; then
    echo "supervisor: skipped (no BUZZ_HIVE_IMAGE, $env_file or unit file)"
    return 0
  fi
  [[ "$image" =~ ^[A-Za-z0-9._:/@-]+$ ]] || { echo "warning: BUZZ_HIVE_IMAGE is not an image name, supervisor skipped" >&2; return 0; }
  # The binary comes out of the image the service runs, so the two always match.
  cid="$(docker create "$image")" || { echo "warning: cannot create a container from $image" >&2; return 0; }
  mkdir -p "$BUZZ_HOME/bin"
  docker cp "$cid:/usr/local/bin/buzz-hive-supervisor" "$BUZZ_HOME/bin/buzz-hive-supervisor" || true
  docker rm "$cid" > /dev/null || true
  [ -x "$BUZZ_HOME/bin/buzz-hive-supervisor" ] || { echo "warning: the image has no buzz-hive-supervisor, supervisor skipped" >&2; return 0; }
  # Public keys only: the engine's signing key and the Beehive account's, from its own secrets.
  keys="$(docker run --rm --env-file "$env_file" "$image" keys)" || { echo "warning: cannot read the Beehive keys, supervisor skipped" >&2; return 0; }
  {
    echo "$keys"
    echo "BUZZ_HIVE_SUPERVISOR_SOCKET=/run/buzz-hive/supervisor.sock"
    # The user the hive container runs as, so it can open the socket and nobody else can.
    echo "BUZZ_HIVE_SUPERVISOR_SOCKET_UID=1000"
    echo "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC=$BUZZ_HOME/deploy/aws/agents"
    echo "BUZZ_HIVE_SUPERVISOR_RENDER_DIR=$agents_dir"
    # Agent and team secrets are read from SSM with the host's aws program and instance role.
    echo "BUZZ_HIVE_SUPERVISOR_SECRETS=ssm"
  } > "$unit_env"
  chmod 600 "$unit_env"
  mkdir -p "$agents_dir"
  mkdir -p "$BUZZ_ETC_DIR/systemd/system"
  cp "$unit_src" "$BUZZ_ETC_DIR/systemd/system/buzz-hive-supervisor.service"
  systemctl daemon-reload
  systemctl enable buzz-hive-supervisor.service > /dev/null 2>&1 || true
  # A restart, so a new binary or new keys take effect.
  systemctl restart buzz-hive-supervisor.service || { echo "warning: the supervisor did not start" >&2; return 0; }
  hive="$(docker ps -q --filter label=com.docker.compose.service=hive --filter status=running | head -n 1)"
  [ -n "$hive" ] || { echo "supervisor: installed; the hive service is not running, nothing applied"; return 0; }
  docker exec "$hive" /usr/local/bin/buzz-hive apply-agents \
    || echo "warning: not every agent was given its tool servers; see get_hive_health" >&2
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
supervise_agents
[ "$failed" -eq 0 ] || exit 1
