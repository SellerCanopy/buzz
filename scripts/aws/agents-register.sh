#!/usr/bin/env bash
# Creates one identity per agent and admits it to the closed relay. Each agent
# has three parameters under /buzz/agents/<name>/:
#   BUZZ_PRIVATE_KEY  secret key (SecureString)
#   BUZZ_PUBLIC_KEY   public key (String)
#   REGISTERED        set once the relay accepted the public key (String)
# An agent with a key and a REGISTERED marker is skipped. An agent with a key but
# no marker (an earlier run stopped before add-member succeeded) is added again
# from its stored public key, which is safe because add-member is idempotent.
# Any other agent gets a new key: the server generates it inside the relay
# container, stores the secret itself (so it never travels back over SSM output),
# then adds the public key as a relay member. Only public keys are printed.
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
  # store_key <name> <generate-key output>: checks both keys, stores the public
  # key, then the secret key without overwriting, and sets pub for add-member.
  # The public key goes first, so a stop between the two leaves no key and the
  # next run generates a fresh pair. The secret goes through a mode-0600 file,
  # never a command line.
  echo "store_key() {"
  echo "  local pubk seck"
  # shellcheck disable=SC2028 # the server needs the literal backslash-n
  echo "  pubk=\"\$(printf '%s\\n' \"\$2\" | sed -n 's/^Public key: *//p')\""
  # shellcheck disable=SC2028 # same
  echo "  seck=\"\$(printf '%s\\n' \"\$2\" | sed -n 's/^Secret key: *//p')\""
  echo "  [[ \"\$pubk\" =~ ^[0-9a-f]{64}\$ ]] || { echo \"error: generate-key gave no usable public key for \$1\" >&2; exit 1; }"
  echo "  [[ \"\$seck\" =~ ^[0-9a-f]{64}\$ ]] || { echo \"error: generate-key gave no usable secret key for \$1\" >&2; exit 1; }"
  echo "  aws ssm put-parameter --name \"/buzz/agents/\$1/BUZZ_PUBLIC_KEY\" --type String --overwrite --value \"\$pubk\" > /dev/null"
  echo "  (umask 077; printf '%s' \"\$seck\" > \"\$tmp/key\")"
  echo "  aws ssm put-parameter --name \"/buzz/agents/\$1/BUZZ_PRIVATE_KEY\" --type SecureString --value \"file://\$tmp/key\" > /dev/null"
  echo "  rm -f \"\$tmp/key\""
  echo "  pub=\"\$pubk\""
  echo "}"
  # load_pub <name>: sets pub from the stored public key.
  echo "load_pub() {"
  echo "  pub=\"\$(aws ssm get-parameter --name \"/buzz/agents/\$1/BUZZ_PUBLIC_KEY\" --query Parameter.Value --output text)\""
  echo "  [[ \"\$pub\" =~ ^[0-9a-f]{64}\$ ]] || { echo \"error: stored public key for \$1 is not 64 hex characters\" >&2; exit 1; }"
  echo "}"
  # mark_registered <name>: records that the relay accepted the public key.
  echo "mark_registered() {"
  echo "  aws ssm put-parameter --name \"/buzz/agents/\$1/REGISTERED\" --type String --overwrite --value yes > /dev/null"
  echo "}"
  # add_failed <name> <pubkey>: nothing marks the agent registered, so the next
  # run of this command tries again.
  echo "add_failed() {"
  echo "  echo \"error: add-member failed for \$1 (\$2). Run this command again to retry.\" >&2"
  echo "  exit 1"
  echo "}"
}

# remote_agent <name>: prints the commands that create and register one agent.
remote_agent() {
  echo "out=\"\$(compose exec -T relay /usr/local/bin/buzz-admin generate-key)\""
  echo "store_key '$1' \"\$out\""
  echo "out=''"
  echo "BUZZ_COMPOSE_TLS=true ./run.sh add-member \"\$pub\" < /dev/null || add_failed '$1' \"\$pub\""
  echo "mark_registered '$1'"
  echo "echo \"$REGISTERED_MARKER $1 \$pub\""
}

# remote_resume <name>: prints the commands that register an agent whose key
# already exists.
remote_resume() {
  echo "load_pub '$1'"
  echo "BUZZ_COMPOSE_TLS=true ./run.sh add-member \"\$pub\" < /dev/null || add_failed '$1' \"\$pub\""
  echo "mark_registered '$1'"
  echo "echo \"$REGISTERED_MARKER $1 \$pub\""
}

require_tool aws
require_tool terraform
load_settings
cd "$REPO_ROOT"

fresh=()
resume=()
while read -r name _; do
  base="/buzz/agents/$name"
  if ! parameter_exists "$base/BUZZ_PRIVATE_KEY"; then
    fresh+=("$name")
  elif parameter_exists "$base/REGISTERED"; then
    echo "kept    $name"
  elif parameter_exists "$base/BUZZ_PUBLIC_KEY"; then
    echo "resume  $name (key stored, relay membership not confirmed)"
    resume+=("$name")
  else
    die "agent $name has a key but no REGISTERED marker and no BUZZ_PUBLIC_KEY, so it cannot be added to the relay. Delete $base/BUZZ_PRIVATE_KEY (aws ssm delete-parameter) and run again to create a new identity."
  fi
done < <(agent_list)

pending=(${fresh[@]+"${fresh[@]}"} ${resume[@]+"${resume[@]}"})
if [ "${#pending[@]}" -eq 0 ]; then
  echo "agents-register: nothing to register"
  exit 0
fi

instance="$(tf_output instance_id)"
[ -n "$instance" ] || die "terraform output instance_id is empty (run make apply first)"

command_id="$(
  {
    remote_prelude
    for name in ${fresh[@]+"${fresh[@]}"}; do remote_agent "$name"; done
    for name in ${resume[@]+"${resume[@]}"}; do remote_resume "$name"; done
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
