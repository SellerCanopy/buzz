#!/usr/bin/env bash
# Stores the agent credentials in SSM Parameter Store under /buzz/agents/ when
# they are absent. Existing parameters are never read back, changed, or replaced.
# A credential belongs to a runtime and is shared by every agent of that runtime.
#   claude: CLAUDE_CODE_OAUTH_TOKEN, or ANTHROPIC_API_KEY when no token is set
#           (the token wins and the API key is ignored)
#   codex:  OPENAI_API_KEY
# The values come from the shell environment, never from .aws.env.
# Usage: agents-secrets.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"

require_tool aws
load_settings

has_claude=0
has_codex=0
while read -r _ runtime; do
  case "$runtime" in
    claude) has_claude=1 ;;
    codex) has_codex=1 ;;
  esac
done < <(agent_list)

if [ "$has_claude" -eq 0 ] && [ "$has_codex" -eq 0 ]; then
  echo "agents-secrets: AGENTS is empty, nothing to store"
  exit 0
fi

# Names of the variables to store, in order. Each is checked before any is stored.
names=()
missing=""
if [ "$has_claude" -eq 1 ]; then
  if [ -n "${CLAUDE_CODE_OAUTH_TOKEN:-}" ]; then
    names+=(CLAUDE_CODE_OAUTH_TOKEN)
  elif [ -n "${ANTHROPIC_API_KEY:-}" ]; then
    names+=(ANTHROPIC_API_KEY)
  else
    missing="$missing CLAUDE_CODE_OAUTH_TOKEN or ANTHROPIC_API_KEY (for the claude agents);"
  fi
fi
if [ "$has_codex" -eq 1 ]; then
  if [ -n "${OPENAI_API_KEY:-}" ]; then
    names+=(OPENAI_API_KEY)
  else
    missing="$missing OPENAI_API_KEY (for the codex agents);"
  fi
fi
[ -z "$missing" ] || die "set these in your shell, then run again:$missing"

# Values land unquoted in an env file, so only plain characters are safe. The
# error names the variable and never the value.
for name in "${names[@]}"; do
  [[ "${!name}" =~ ^[A-Za-z0-9._-]+$ ]] \
    || die "$name has characters outside [A-Za-z0-9._-] (the value is not shown)"
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

created=0
kept=0
for name in "${names[@]}"; do
  param="/buzz/agents/$name"
  if parameter_exists "$param"; then
    echo "kept    $param"
    kept=$((kept + 1))
    continue
  fi
  # The value goes through a mode-0600 file so it never shows in a process list.
  (umask 077; printf '%s' "${!name}" > "$WORK/value")
  aws ssm put-parameter --name "$param" --type SecureString \
    --value "file://$WORK/value" > /dev/null
  rm -f "$WORK/value"
  echo "created $param"
  created=$((created + 1))
done

# The renderer prefers the OAuth token, so a stored token hides an API key.
if [ "$has_claude" -eq 1 ]; then
  case " ${names[*]} " in
    *" CLAUDE_CODE_OAUTH_TOKEN "*)
      if parameter_exists /buzz/agents/ANTHROPIC_API_KEY; then
        echo "note: /buzz/agents/ANTHROPIC_API_KEY also exists. Agents use the OAuth token and ignore it."
      fi
      ;;
    *" ANTHROPIC_API_KEY "*)
      if parameter_exists /buzz/agents/CLAUDE_CODE_OAUTH_TOKEN; then
        echo "note: /buzz/agents/CLAUDE_CODE_OAUTH_TOKEN exists, so agents keep using the OAuth token and ignore the API key. Delete the token parameter to switch."
      fi
      ;;
  esac
fi

echo "agents-secrets: $created created, $kept kept"
