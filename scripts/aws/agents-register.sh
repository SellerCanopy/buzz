#!/usr/bin/env bash
# Creates one identity per agent and admits it to the closed relay. An agent is
# skipped when /buzz/agents/<name>/BUZZ_PRIVATE_KEY already exists. For the
# others, the server generates the key inside the relay container, stores the
# secret key in SSM itself (so it never travels back over SSM output), then adds
# the public key as a relay member. Only public keys are printed.
# Usage: agents-register.sh
# Environment: POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; SSM command wait).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/aws/lib.sh
. "$SCRIPT_DIR/lib.sh"
# shellcheck source=scripts/aws/params.sh
. "$SCRIPT_DIR/params.sh"
# shellcheck source=scripts/aws/ssm.sh
. "$SCRIPT_DIR/ssm.sh"

REGISTERED_MARKER="registered"

# remote_prelude: prints the commands that define the server-side helpers.
remote_prelude() {
  echo "#!/bin/bash"
  echo "set -euo pipefail"
  echo "export HOME=/root"
  echo "export AWS_DEFAULT_REGION='$AWS_REGION'"
  echo "cd /opt/buzz/deploy/compose"
  echo "compose() { docker compose --env-file .env -f compose.yml -f compose.caddy.yml \"\$@\"; }"
  echo "tmp=\"\$(mktemp -d)\""
  echo "trap 'rm -rf \"\$tmp\"' EXIT"
  # store_key <name> <generate-key output>: checks both keys, stores the secret
  # key without overwriting, and sets pub for the add-member call. The secret
  # goes through a mode-0600 file, never a command line.
  echo "store_key() {"
  echo "  local pubk seck"
  # shellcheck disable=SC2028 # the server needs the literal backslash-n
  echo "  pubk=\"\$(printf '%s\\n' \"\$2\" | sed -n 's/^Public key: *//p')\""
  # shellcheck disable=SC2028 # same
  echo "  seck=\"\$(printf '%s\\n' \"\$2\" | sed -n 's/^Secret key: *//p')\""
  echo "  [[ \"\$pubk\" =~ ^[0-9a-f]{64}\$ ]] || { echo \"error: generate-key gave no usable public key for \$1\" >&2; exit 1; }"
  echo "  [[ \"\$seck\" =~ ^[0-9a-f]{64}\$ ]] || { echo \"error: generate-key gave no usable secret key for \$1\" >&2; exit 1; }"
  echo "  (umask 077; printf '%s' \"\$seck\" > \"\$tmp/key\")"
  echo "  aws ssm put-parameter --name \"/buzz/agents/\$1/BUZZ_PRIVATE_KEY\" --type SecureString --value \"file://\$tmp/key\" > /dev/null"
  echo "  rm -f \"\$tmp/key\""
  echo "  pub=\"\$pubk\""
  echo "}"
  # add_failed <name> <pubkey>: the key is already stored, so a rerun would skip
  # this agent. Say how to finish by hand.
  echo "add_failed() {"
  echo "  echo \"error: the key for \$1 is stored but add-member failed. Finish with: cd /opt/buzz/deploy/compose && BUZZ_COMPOSE_TLS=true ./run.sh add-member \$2\" >&2"
  echo "  exit 1"
  echo "}"
}

# remote_agent <name>: prints the commands that register one agent.
remote_agent() {
  echo "out=\"\$(compose exec -T relay /usr/local/bin/buzz-admin generate-key)\""
  echo "store_key '$1' \"\$out\""
  echo "out=''"
  echo "BUZZ_COMPOSE_TLS=true ./run.sh add-member \"\$pub\" < /dev/null || add_failed '$1' \"\$pub\""
  echo "echo \"$REGISTERED_MARKER $1 \$pub\""
}

require_tool aws
require_tool terraform
load_settings
cd "$REPO_ROOT"

pending=()
while read -r name _; do
  if parameter_exists "/buzz/agents/$name/BUZZ_PRIVATE_KEY"; then
    echo "kept    /buzz/agents/$name/BUZZ_PRIVATE_KEY"
  else
    pending+=("$name")
  fi
done < <(agent_list)

if [ "${#pending[@]}" -eq 0 ]; then
  echo "agents-register: nothing to register"
  exit 0
fi

instance="$(tf_output instance_id)"
[ -n "$instance" ] || die "terraform output instance_id is empty (run make apply first)"

command_id="$(
  {
    remote_prelude
    for name in "${pending[@]}"; do remote_agent "$name"; done
  } | ssm_send "$instance" "buzz agents-register"
)"
[ -n "$command_id" ] || die "send-command returned no command id"
echo "sent SSM command $command_id, waiting"

ssm_wait "$command_id" "$instance" || exit 1
output="$(ssm_output "$command_id" "$instance")"
echo "$output"

# A command that succeeds without reporting every agent is not trusted.
for name in "${pending[@]}"; do
  case "$output" in
    *"$REGISTERED_MARKER $name "*) ;;
    *) die "the server finished without reporting $name as registered" ;;
  esac
done
echo "agents-register: ${#pending[@]} registered"
