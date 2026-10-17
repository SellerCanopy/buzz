#!/usr/bin/env bash
# Stores one secret for an agent's MCP servers in SSM Parameter Store, at
# /buzz/agents/<name>/env/<VAR>. The agent's mcp.json refers to it as ${VAR}, and
# make deploy writes it into that agent's container environment. The value comes
# from the shell variable named VAR, never from a file in git. A parameter that
# exists is replaced, so rerunning rotates the secret (then run make deploy).
# Usage: AGENT=<name> VAR=<NAME> agents-env.sh
#        AGENT=<name> agents-env.sh     lists the variables its mcp.json needs and
#                                       whether each one is stored
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="${REPO_ROOT:-$(cd "$SCRIPT_DIR/../.." && pwd)}"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"

# Same shape the renderer accepts: values land unquoted in an env file.
SECRET_PATTERN='^[A-Za-z0-9._~+/=:@-]+$'
config_dir="${AGENT_CONFIG_DIR:-$REPO_ROOT/deploy/aws/agents}"

require_tool aws
require_tool python3

agent="${AGENT:-}"
var="${VAR:-}"
# Read the secret before load_settings, which sources .aws.env and could replace
# a variable of the same name.
value=""
if [ -n "$var" ]; then
  python3 -I "$SCRIPT_DIR/agent_spec.py" var "$var" || exit 1
  value="${!var:-}"
fi
load_settings
[ -n "$agent" ] || die "set AGENT=<name from AGENTS>, for example: make agents-env AGENT=coder VAR=GITHUB_TOKEN"

known=0
while read -r name _; do
  [ "$name" = "$agent" ] && known=1
done < <(agent_list)
[ "$known" -eq 1 ] || die "$agent is not in AGENTS in .aws.env"

if [ -z "$var" ]; then
  needed="$(python3 -I "$SCRIPT_DIR/agent_spec.py" mcp-vars "$config_dir/$agent/mcp.json")"
  if [ -z "$needed" ]; then
    echo "agents-env: $agent/mcp.json uses no \${VAR} secrets"
    exit 0
  fi
  for name in $needed; do
    if parameter_exists "/buzz/agents/$agent/env/$name"; then
      echo "stored  $name"
    else
      echo "missing $name (run: make agents-env AGENT=$agent VAR=$name)"
    fi
  done
  exit 0
fi

[ -n "$value" ] || die "set $var in your shell first (export $var=...), then run again"
[[ "$value" =~ $SECRET_PATTERN ]] \
  || die "$var has characters outside [A-Za-z0-9._~+/=:@-] (the value is not shown)"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

param="/buzz/agents/$agent/env/$var"
if parameter_exists "$param"; then verb=updated; else verb=created; fi
# The value goes through a mode-0600 file so it never shows in a process list.
(umask 077; printf '%s' "$value" > "$WORK/value")
aws ssm put-parameter --name "$param" --type SecureString --overwrite \
  --value "file://$WORK/value" > /dev/null
echo "$verb $param"
if ! grep -qF "\${$var}" "$config_dir/$agent/mcp.json" 2> /dev/null; then
  echo "note: $agent/mcp.json does not use \${$var} yet, so the deploy will not pass it to the agent"
fi
echo "run make deploy to give the new value to $agent"
