#!/usr/bin/env bash
# Shared helpers for the AWS deployment scripts. Source this file; do not run it.

if [ -z "${REPO_ROOT:-}" ]; then
  REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
fi
export REPO_ROOT

# die <message>: prints the message to stderr and exits 1.
die() {
  echo "error: $*" >&2
  exit 1
}

# require_tool <name>: exits 1 unless the tool is on PATH.
require_tool() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing tool: $1" >&2
    exit 1
  }
}

# tf_output <name>: prints one Terraform output value, unquoted.
tf_output() {
  terraform -chdir=infra output -raw "$1"
}

# load_settings: sources the settings file, applies defaults, validates, and
# exports every setting. Reads ${AWS_ENV_FILE:-$REPO_ROOT/.aws.env}. Exits 1
# with a message on any invalid or missing value.
load_settings() {
  local env_file="${AWS_ENV_FILE:-$REPO_ROOT/.aws.env}"
  [ -f "$env_file" ] || die "settings file not found: $env_file (copy .aws.env.example to .aws.env)"
  # shellcheck source=/dev/null
  . "$env_file"

  local var
  for var in AWS_PROFILE TF_STATE_BUCKET ROUTE53_ZONE BUZZ_DOMAIN RELAY_OWNER_PUBKEY; do
    [ -n "${!var:-}" ] || die "$var is required in $env_file"
  done

  AWS_REGION="${AWS_REGION:-us-east-1}"
  INSTANCE_TYPE="${INSTANCE_TYPE:-t4g.large}"
  # No colon: AGENTS= (set but empty) stays empty and disables agents.
  AGENTS="${AGENTS-claude,codex}"
  AGENT_RESPOND_TO="${AGENT_RESPOND_TO:-owner-only}"
  AGENT_RESPOND_TO_ALLOWLIST="${AGENT_RESPOND_TO_ALLOWLIST:-}"

  case "$BUZZ_DOMAIN" in
    *://* | */* | *[[:space:]]* | *.)
      die "BUZZ_DOMAIN must be a bare host name (no scheme, slash, space, or trailing dot): $BUZZ_DOMAIN"
      ;;
  esac

  [[ "$RELAY_OWNER_PUBKEY" =~ ^[0-9a-f]{64}$ ]] \
    || die "RELAY_OWNER_PUBKEY must be 64 hex characters (lowercase), not an npub"

  local agent
  if [ -n "$AGENTS" ]; then
    local -a agent_list
    IFS=',' read -r -a agent_list <<< "$AGENTS"
    for agent in "${agent_list[@]}"; do
      case "$agent" in
        claude | codex) ;;
        *) die "AGENTS contains unknown agent '$agent' (allowed: claude, codex)" ;;
      esac
    done
  fi

  case "$AGENT_RESPOND_TO" in
    owner-only | anyone | nobody) ;;
    allowlist)
      [ -n "$AGENT_RESPOND_TO_ALLOWLIST" ] \
        || die "AGENT_RESPOND_TO_ALLOWLIST is required when AGENT_RESPOND_TO=allowlist"
      local key
      local -a keys
      IFS=',' read -r -a keys <<< "$AGENT_RESPOND_TO_ALLOWLIST"
      for key in "${keys[@]}"; do
        [[ "$key" =~ ^[0-9a-f]{64}$ ]] \
          || die "AGENT_RESPOND_TO_ALLOWLIST must be comma-separated 64 hex pubkeys, bad entry: '$key'"
      done
      ;;
    *) die "AGENT_RESPOND_TO must be one of owner-only, allowlist, anyone, nobody: $AGENT_RESPOND_TO" ;;
  esac

  export AWS_PROFILE TF_STATE_BUCKET ROUTE53_ZONE BUZZ_DOMAIN RELAY_OWNER_PUBKEY \
    AWS_REGION INSTANCE_TYPE AGENTS AGENT_RESPOND_TO AGENT_RESPOND_TO_ALLOWLIST
}
