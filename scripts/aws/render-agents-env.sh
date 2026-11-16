#!/usr/bin/env bash
# Runs on the server. Renders one env file and one staged MCP config per agent.
# Usage: render-agents-env.sh <outdir> <agents-image> <config-src>
# For each agent in AGENTS it writes, under <outdir>:
#   <name>.env              container variables (holds secrets, mode 0600)
#   <name>.compose.env      variables for compose.agents.yml (AGENTS_IMAGE,
#                           AGENT_NAME, BUZZ_DOMAIN, AGENT_ENV_FILE, AGENT_CONFIG_DIR)
#   <name>/config/mcp.json  copy of <config-src>/<name>/mcp.json, or an empty config
# and agents.list (one name per line). A ${VAR} in an agent's mcp.json (without a
# :-default) is written to that agent's env file from the SSM parameter
# /buzz/agents/<name>/env/<VAR>; a missing one is an error. Env files of agents
# no longer in AGENTS are removed. Reads BUZZ_DOMAIN, RELAY_OWNER_PUBKEY, AGENTS, AGENT_RESPOND_TO,
# and AGENT_RESPOND_TO_ALLOWLIST from the environment, and the agent keys and
# credentials from SSM under /buzz/agents/.
# Each agent's optional <config-src>/<name>/agent.json sets its display_name,
# model, who it answers, and the other runtime settings listed in agent_spec.py.
# Who it answers falls back to AGENT_RESPOND_TO and AGENT_RESPOND_TO_ALLOWLIST. An
# instructions_file is copied next to mcp.json.
# Exits 1 before writing anything if a value is missing, empty, or unsafe, if an
# agent.json is invalid, or if a config file is not JSON with a top-level
# "mcpServers" object.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"

PLAIN_PATTERN='^[A-Za-z0-9._-]+$'
# Agent secrets for ${VAR} references in mcp.json. Values land unquoted in an env
# file, so $, #, quotes, backslash, spaces, and line breaks are not allowed.
SECRET_PATTERN='^[A-Za-z0-9._~+/=:@-]+$'

[ $# -eq 3 ] || die "usage: render-agents-env.sh <outdir> <agents-image> <config-src>"
outdir="$1"
image="$2"
config_src="$3"
[ -n "$image" ] || die "agents image is empty"
: "${BUZZ_DOMAIN:?BUZZ_DOMAIN must be set}"
: "${RELAY_OWNER_PUBKEY:?RELAY_OWNER_PUBKEY must be set}"
# Defaults for every agent. An agent's own agent.json overrides them.
default_respond_to="${AGENT_RESPOND_TO:-owner-only}"
default_allowlist="${AGENT_RESPOND_TO_ALLOWLIST:-}"
require_tool aws
require_tool python3
umask 077

# Values live in SSM_<key> variables (bash 3.2 has no associative arrays). A
# name holds '-', which a variable name cannot, so '-' and '/' become '_'.
ssm_var() { printf 'SSM_%s' "${1//[-\/]/_}"; }

params="$(aws ssm get-parameters-by-path --path /buzz/agents/ --recursive --with-decryption \
  --query 'Parameters[].[Name,Value]' --output text)"
while IFS=$'\t' read -r pname pvalue; do
  [[ "$pname" =~ ^/buzz/agents/[A-Za-z0-9/_-]+$ ]] || continue
  printf -v "$(ssm_var "${pname#/buzz/agents/}")" '%s' "$pvalue"
done <<< "$params"
params=""

# secret <key> <pattern> <shape>: prints the SSM value for <key>. Otherwise
# prints to stderr which parameter is missing or misshapen (never its value)
# and returns 1.
secret() {
  local var
  var="$(ssm_var "$1")"
  [ -n "${!var:-}" ] || { echo "error: SSM parameter /buzz/agents/$1 is missing or empty" >&2; return 1; }
  [[ "${!var}" =~ $2 ]] || { echo "error: SSM parameter /buzz/agents/$1 is not $3 (the value is not shown)" >&2; return 1; }
  printf '%s' "${!var}"
}

# agent_env <name> <runtime>: prints the container env file. Prints nothing and
# returns 1 after an error on stderr if the key or credential is missing, empty,
# or misshapen. Safe to run twice.
agent_env() {
  local name="$1" runtime="$2" key cred_name cred command mcp_vars var value
  local settings="$config_src/$name/agent.json"
  key="$(secret "$name/BUZZ_PRIVATE_KEY" '^[0-9a-f]{64}$' "64 lowercase hex characters")" \
    || { echo "error: agent $name has no usable key (run make agents-register)" >&2; return 1; }
  case "$runtime" in
    claude)
      command="claude-agent-acp"
      # The OAuth token wins when both are stored. Never write both.
      if [ -n "${SSM_CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
        cred_name="CLAUDE_CODE_OAUTH_TOKEN"
      else
        cred_name="ANTHROPIC_API_KEY"
      fi
      ;;
    codex)
      command="codex-acp"
      cred_name="OPENAI_API_KEY"
      ;;
    *)
      echo "error: agent $name has unknown runtime '$runtime'" >&2
      return 1
      ;;
  esac
  cred="$(secret "$cred_name" "$PLAIN_PATTERN" "plain characters [A-Za-z0-9._-]")" \
    || { echo "error: agent $name has no usable credential (run make agents-secrets)" >&2; return 1; }

  echo "BUZZ_PRIVATE_KEY=$key"
  echo "BUZZ_RELAY_URL=wss://$BUZZ_DOMAIN"
  echo "BUZZ_ACP_AGENT_OWNER=$RELAY_OWNER_PUBKEY"
  echo "BUZZ_ACP_AGENT_COMMAND=$command"
  echo "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp"
  # Settings from the agent's agent.json, with the global respond-to as the default.
  python3 -I "$SCRIPT_DIR/agent_spec.py" env "$settings" --default "display_name=$name" \
    --default "respond_to=$default_respond_to" --default "respond_to_allowlist=$default_allowlist" \
    || return 1
  echo "$cred_name=$cred"
  # Secrets for the ${VAR} references in this agent's mcp.json (make agents-env).
  # Only referenced names are written, so a stored secret nobody uses stays off disk.
  mcp_vars="$(python3 -I "$SCRIPT_DIR/agent_spec.py" mcp-vars "$config_src/$name/mcp.json")" || return 1
  for var in $mcp_vars; do
    value="$(secret "$name/env/$var" "$SECRET_PATTERN" "plain characters [A-Za-z0-9._~+/=:@-]")" \
      || { echo "error: agent $name: mcp.json uses \${$var} (run make agents-env AGENT=$name VAR=$var)" >&2; return 1; }
    echo "$var=$value"
  done
}

# check_config <name>: returns 1 after an error on stderr if the agent's config
# file exists and is not JSON with a top-level "mcpServers" object.
check_config() {
  local src="$config_src/$1/mcp.json"
  [ -f "$src" ] || return 0
  python3 -I -c '
import json, sys
cfg = json.load(open(sys.argv[1]))
if not isinstance(cfg, dict) or not isinstance(cfg.get("mcpServers"), dict):
    raise ValueError("no mcpServers object")
' "$src" 2> /dev/null || {
    echo "error: agent $1: $src is not valid JSON with a top-level \"mcpServers\" object" >&2
    return 1
  }
}

# Pass 1 checks every agent. Pass 2 writes. A bad agent never leaves a
# half-updated directory behind.
agents="$(agent_list)"
while read -r name runtime; do
  [ -n "$name" ] || continue
  [[ "$name" =~ $AGENT_NAME_PATTERN ]] || die "invalid agent name: $name"
  agent_env "$name" "$runtime" > /dev/null || exit 1
  check_config "$name" || exit 1
done <<< "$agents"

mkdir -p "$outdir"
chmod 0700 "$outdir"
outdir="$(cd "$outdir" && pwd)"

names=$'\n'
while read -r name runtime; do
  [ -n "$name" ] || continue
  names="$names$name"$'\n'

  agent_env "$name" "$runtime" > "$outdir/$name.env.tmp"
  mv "$outdir/$name.env.tmp" "$outdir/$name.env"

  config_dir="$outdir/$name/config"
  mkdir -p "$config_dir"
  # The container runs as uid 1001 and mounts this directory read-only, so set
  # the modes explicitly. The umask above would otherwise make them 0700/0600.
  chmod 0755 "$config_dir"
  if [ -f "$config_src/$name/mcp.json" ]; then
    cp "$config_src/$name/mcp.json" "$config_dir/mcp.json"
  else
    printf '%s\n' '{"mcpServers": {}}' > "$config_dir/mcp.json"
  fi
  chmod 0644 "$config_dir/mcp.json"
  instructions="$(agent_field "$config_src/$name/agent.json" instructions_file "")"
  if [ -n "$instructions" ]; then
    cp "$config_src/$name/$instructions" "$config_dir/$instructions"
    chmod 0644 "$config_dir/$instructions"
  fi

  {
    echo "AGENTS_IMAGE=$image"
    echo "AGENT_NAME=$name"
    echo "BUZZ_DOMAIN=$BUZZ_DOMAIN"
    echo "AGENT_ENV_FILE=$outdir/$name.env"
    echo "AGENT_CONFIG_DIR=$config_dir"
  } > "$outdir/$name.compose.env"
done <<< "$agents"

printf '%s' "${names#$'\n'}" > "$outdir/agents.list"

# Remove the env files and staged config of agents that left AGENTS.
for f in "$outdir"/*.env "$outdir"/*/; do
  [ -e "$f" ] || continue
  stem="$(basename "$f")"
  stem="${stem%.compose.env}"
  stem="${stem%.env}"
  [[ "$stem" =~ $AGENT_NAME_PATTERN ]] || continue
  case "$names" in
    *$'\n'"$stem"$'\n'*) ;;
    *) rm -rf "${outdir:?}/$stem" "$outdir/$stem.env" "$outdir/$stem.compose.env" ;;
  esac
done
