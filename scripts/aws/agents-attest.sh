#!/usr/bin/env bash
# Makes each agent show up, with its name, in the owner's Buzz Desktop agent list.
# Desktop lists a relay agent only when the owner has signed a record for it and
# the agent's profile carries the owner's proof (a NIP-OA auth tag). For each agent
# this script:
#   1. signs and publishes the owner's record (kind 30177) with the owner key, and
#      computes the auth tag. The owner key never leaves this machine.
#   2. uploads the agent's picture to the relay, if it has one (the owner key signs
#      the upload).
#   3. runs `buzz users set-profile` inside the agent's container with the auth tag,
#      so the agent publishes its own profile.
# The profile comes from deploy/aws/agents/<name>/: an optional agent.json whose
# display_name and about set the profile (display_name defaults to the AGENTS name), and an optional
# avatar.png (or .jpg, .jpeg, .webp, .gif).
# Both steps replace earlier versions, so running this again is safe.
# Usage: agents-attest.sh
# Environment: BUZZ_OWNER_KEY (the owner's secret key, hex or nsec; it must belong to
# RELAY_OWNER_PUBKEY), ATTEST_BIN and BUZZ_BIN (the built helper and CLI; tests override
# them), AGENT_CONFIG_DIR (tests override the agent folders), POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

ATTESTED_MARKER="attested"

require_tool aws
require_tool terraform
require_tool cargo
require_tool python3
load_settings
cd "$REPO_ROOT"

[ -n "${BUZZ_OWNER_KEY:-}" ] || die "BUZZ_OWNER_KEY is not set. Export the relay owner's secret key (hex or nsec) in this shell; it is used only on this machine."

# Defaults for every agent. An agent's own agent.json overrides them.
default_respond_to="${AGENT_RESPOND_TO:-owner-only}"
default_allowlist="${AGENT_RESPOND_TO_ALLOWLIST:-}"

agents=()
while read -r name _; do
  agents+=("$name")
done < <(agent_list)
if [ "${#agents[@]}" -eq 0 ]; then
  echo "agents-attest: no agents in AGENTS"
  exit 0
fi

cargo build --release -p buzz-cli --bin buzz --example attest_agent >&2
attest="${ATTEST_BIN:-$REPO_ROOT/target/release/examples/attest_agent}"
[ -x "$attest" ] || die "the attest_agent helper was not built at $attest"
buzz_bin="${BUZZ_BIN:-$REPO_ROOT/target/release/buzz}"
[ -x "$buzz_bin" ] || die "the buzz CLI was not built at $buzz_bin"

instance="$(tf_output instance_id)"
[ -n "$instance" ] || die "terraform output instance_id is empty (run make apply first)"

export BUZZ_RELAY_URL="wss://$BUZZ_DOMAIN"
# The helper stops if BUZZ_OWNER_KEY is not the key of this public key.
export BUZZ_OWNER_PUBKEY="$RELAY_OWNER_PUBKEY"
config_dir="${AGENT_CONFIG_DIR:-$REPO_ROOT/deploy/aws/agents}"

# avatar_file <name>: prints the path of the agent's picture, or nothing.
avatar_file() {
  local ext
  for ext in png jpg jpeg webp gif; do
    if [ -f "$config_dir/$1/avatar.$ext" ]; then
      echo "$config_dir/$1/avatar.$ext"
      return 0
    fi
  done
}
script="$(mktemp)"
trap 'rm -f "$script"' EXIT
{
  echo "#!/bin/bash"
  echo "set -euo pipefail"
} > "$script"

for name in "${agents[@]}"; do
  pub="$(aws ssm get-parameter --name "/buzz/agents/$name/BUZZ_PUBLIC_KEY" --query Parameter.Value --output text 2>/dev/null)" \
    || die "agent $name has no stored public key. Run make agents-register first."
  [[ "$pub" =~ ^[0-9a-f]{64}$ ]] || die "the stored public key for $name is not 64 hex characters"
  display="$(agent_field "$config_dir/$name/agent.json" display_name "$name")" || die "bad agent.json for $name"
  about="$(agent_field "$config_dir/$name/agent.json" about "")" || die "bad agent.json for $name"
  respond_to="$(agent_field "$config_dir/$name/agent.json" respond_to "$default_respond_to")" || die "bad agent.json for $name"
  allowlist="$(agent_field "$config_dir/$name/agent.json" respond_to_allowlist "$default_allowlist")" || die "bad agent.json for $name"
  parallelism="$(agent_field "$config_dir/$name/agent.json" parallelism 1)" || die "bad agent.json for $name"
  # The owner record has no "nobody" mode, so an agent that answers nobody is owner-only there.
  [ "$respond_to" != "nobody" ] || respond_to="owner-only"
  tag="$("$attest" "$pub" "$display" "$respond_to" "$allowlist" "$parallelism")" \
    || die "could not publish the owner record for $name"
  args="--name '$display'"
  [ -z "$about" ] || args="$args --about '$about'"
  avatar="$(avatar_file "$name")"
  if [ -n "$avatar" ]; then
    upload="$(BUZZ_PRIVATE_KEY="$BUZZ_OWNER_KEY" BUZZ_RELAY_URL="https://$BUZZ_DOMAIN" "$buzz_bin" upload file --file "$avatar")" \
      || die "could not upload the picture for $name ($avatar)"
    url="$(printf '%s' "$upload" | python3 -I -c 'import json, sys; print(json.load(sys.stdin)["url"])')" \
      || die "the upload for $name returned no url"
    case "$url" in
      "https://$BUZZ_DOMAIN/"*) ;;
      *) die "the upload for $name returned an unexpected url: $url" ;;
    esac
    args="$args --avatar '$url'"
  fi
  # The tag is a public signature, so it is safe on a command line. It has no single quote.
  echo "docker exec -e BUZZ_AUTH_TAG='$tag' 'buzz-agent-$name-agent-1' buzz users set-profile $args > /dev/null" >> "$script"
  echo "echo '$ATTESTED_MARKER $name'" >> "$script"
done

command_id="$(ssm_send "$instance" "buzz agents-attest" < "$script")"
[ -n "$command_id" ] || die "send-command returned no command id"
echo "sent SSM command $command_id, waiting"
ssm_wait "$command_id" "$instance" || exit 1
output="$(ssm_output "$command_id" "$instance")"
echo "$output"

for name in "${agents[@]}"; do
  case "$output" in
    *"$ATTESTED_MARKER $name"*) ;;
    *) die "the server finished without reporting $name as attested" ;;
  esac
done
echo "agents-attest: ${#agents[@]} attested"
