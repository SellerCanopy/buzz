#!/usr/bin/env bash
# Shared helpers for running a script on the server through SSM (the AWS
# service that sends commands to the instance). Source this file; do not run it.
# Like lib.sh, it sets no shell options. The caller runs with `set -euo pipefail`.
# Environment: POLL_INTERVAL and POLL_ATTEMPTS (seconds, count; command wait).

POLL_INTERVAL="${POLL_INTERVAL:-5}"
POLL_ATTEMPTS="${POLL_ATTEMPTS:-180}"

# ssm_json_str <text>: prints the text as a JSON string. Handles backslash and
# double quote only; the lines passed in hold neither tabs nor newlines.
ssm_json_str() {
  local s="$1"
  s="${s//\\/\\\\}"
  s="${s//\"/\\\"}"
  printf '"%s"' "$s"
}

# ssm_parameters_json: reads script lines from stdin and prints the
# --parameters JSON document for AWS-RunShellScript.
ssm_parameters_json() {
  local line first=1
  printf '{"commands":['
  while IFS= read -r line; do
    [ "$first" -eq 1 ] || printf ','
    first=0
    ssm_json_str "$line"
  done
  printf ']}'
}

# ssm_send <instance-id> <comment>: reads script lines from stdin, sends them
# to the instance, and prints the SSM command id.
ssm_send() {
  local instance="$1" comment="$2" params
  params="$(ssm_parameters_json)"
  aws ssm send-command --instance-ids "$instance" --document-name AWS-RunShellScript \
    --comment "$comment" \
    --parameters "$params" \
    --query Command.CommandId --output text
}

# _ssm_wait_loop <command-id> <instance-id> <scratch-dir>: the polling loop of
# ssm_wait. Returns 1 on any final status other than Success or when the wait
# runs out.
_ssm_wait_loop() {
  local id="$1" instance="$2" scratch="$3" i status="" empty_reads=0 warned=0
  for ((i = 0; i < POLL_ATTEMPTS; i++)); do
    status="$(aws ssm get-command-invocation --command-id "$id" --instance-id "$instance" \
      --query Status --output text 2> "$scratch/poll.err" || true)"
    case "$status" in
      "" | None)
        # A few empty reads are normal right after send-command. A run of them
        # means the read itself is failing, so show the error once.
        empty_reads=$((empty_reads + 1))
        if [ "$empty_reads" -ge 5 ] && [ "$warned" -eq 0 ]; then
          warned=1
          echo "warning: $empty_reads status reads in a row returned nothing. Last aws error: $(tail -n 5 "$scratch/poll.err")" >&2
        fi
        sleep "$POLL_INTERVAL"
        ;;
      Success) return 0 ;;
      Pending | InProgress | Delayed)
        empty_reads=0
        sleep "$POLL_INTERVAL"
        ;;
      *)
        echo "server command ended with status $status. Last error output:" >&2
        aws ssm get-command-invocation --command-id "$id" --instance-id "$instance" \
          --query StandardErrorContent --output text 2>&1 | tail -n 40 >&2 || true
        return 1
        ;;
    esac
  done
  echo "error: timed out after $((POLL_ATTEMPTS * POLL_INTERVAL))s waiting for SSM command $id (last status: ${status:-none})" >&2
  return 1
}

# ssm_wait <command-id> <instance-id>: polls until the SSM command succeeds.
# Returns 1 and explains on stderr on any other final status or when the wait
# runs out.
ssm_wait() {
  local scratch rc=0
  scratch="$(mktemp -d)"
  _ssm_wait_loop "$1" "$2" "$scratch" || rc=$?
  rm -rf "$scratch"
  return "$rc"
}

# ssm_output <command-id> <instance-id>: prints what the finished command wrote
# to standard output. SSM keeps only the last 24000 characters.
ssm_output() {
  aws ssm get-command-invocation --command-id "$1" --instance-id "$2" \
    --query StandardOutputContent --output text
}
