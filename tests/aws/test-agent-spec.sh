#!/usr/bin/env bash
# Tests for scripts/aws/agent_spec.py, the agent.json reader and checker.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

SPEC="$REPO_ROOT/scripts/aws/agent_spec.py"
KEY="$(printf 'c%.0s' $(seq 1 64))"

# spec_env <json> [default args...]: runs `env` on a spec with the given body.
spec_env() {
  local body="$1"
  shift
  mkdir -p "$WORK/spec"
  rm -f "$WORK/spec/instructions.md"
  printf '%s' "$body" > "$WORK/spec/agent.json"
  [ -n "${WITH_INSTRUCTIONS:-}" ] && printf 'be brief' > "$WORK/spec/instructions.md"
  python3 -I "${SPEC_BIN:-$SPEC}" env "$WORK/spec/agent.json" "$@" > "$WORK/spec.out" 2> "$WORK/spec.err"
}

scenario_every_field_maps_to_its_variable() {
  WITH_INSTRUCTIONS=1 spec_env '{"display_name":"Coder","about":"x","model":"opus","effort":"high","permission_mode":"accept-edits","respond_to":"allowlist","respond_to_allowlist":["'"$KEY"'"],"parallelism":3,"session_policy":"thread","multiple_event_handling":"queue","context_message_limit":5,"max_turns_per_session":40,"idle_timeout":120,"max_turn_duration":900,"heartbeat_interval":60,"heartbeat_prompt":"check in","memory":false,"instructions_file":"instructions.md"}' \
    || return 1
  local line
  for line in BUZZ_ACP_DISPLAY_NAME=Coder BUZZ_ACP_MODEL=opus BUZZ_ACP_EFFORT_LEVEL=high \
    BUZZ_ACP_PERMISSION_MODE=accept-edits BUZZ_ACP_RESPOND_TO=allowlist \
    BUZZ_ACP_RESPOND_TO_ALLOWLIST="$KEY" BUZZ_ACP_AGENTS=3 BUZZ_ACP_SESSION_POLICY=thread \
    BUZZ_ACP_MULTIPLE_EVENT_HANDLING=queue BUZZ_ACP_CONTEXT_MESSAGE_LIMIT=5 \
    BUZZ_ACP_MAX_TURNS_PER_SESSION=40 BUZZ_ACP_IDLE_TIMEOUT=120 BUZZ_ACP_MAX_TURN_DURATION=900 \
    BUZZ_ACP_HEARTBEAT_INTERVAL=60 "BUZZ_ACP_HEARTBEAT_PROMPT=check in" BUZZ_ACP_NO_MEMORY=true \
    BUZZ_ACP_SYSTEM_PROMPT_FILE=/etc/buzz-agent/instructions.md; do
    grep -qxF -- "$line" "$WORK/spec.out" || { echo "missing $line" >&2; return 1; }
  done
}

scenario_missing_file_gives_only_defaults() {
  RC=0
  python3 -I "$SPEC" env "$WORK/none.json" --default display_name=a --default respond_to=anyone \
    > "$WORK/spec.out" 2> /dev/null || RC=$?
  [ "$RC" -eq 0 ] || return 1
  [ "$(cat "$WORK/spec.out")" = "$(printf 'BUZZ_ACP_DISPLAY_NAME=a\nBUZZ_ACP_RESPOND_TO=anyone')" ]
}

scenario_bad_values_are_refused() {
  local body
  for body in '{"modle":"x"}' '[]' '{bad' '{"model":3}' '{"model":"a b"}' '{"parallelism":0}' '{"parallelism":"2"}' \
    '{"parallelism":true}' '{"memory":"yes"}' '{"respond_to":"everyone"}' '{"respond_to_allowlist":"abc"}' \
    '{"respond_to_allowlist":["abc"]}' '{"display_name":"it'"'"'s"}' '{"about":"line\nbreak"}' \
    '{"instructions_file":"../x"}' '{"instructions_file":"mcp.json"}' '{"instructions_file":"missing.md"}' \
    '{"permission_mode":"yolo"}'; do
    spec_env "$body" && { echo "accepted $body" >&2; return 1; }
    grep -q '^error:' "$WORK/spec.err" || { echo "no message for $body" >&2; return 1; }
  done
}

scenario_allowlist_mode_needs_keys() {
  spec_env '{"respond_to":"allowlist"}' && return 1
  grep -qF "respond_to_allowlist" "$WORK/spec.err" || return 1
  # A global default allowlist satisfies it.
  spec_env '{"respond_to":"allowlist"}' --default "respond_to_allowlist=$KEY" || return 1
  grep -qxF "BUZZ_ACP_RESPOND_TO_ALLOWLIST=$KEY" "$WORK/spec.out"
}

scenario_allowlist_dropped_unless_allowlist_mode() {
  spec_env '{"respond_to":"anyone"}' --default "respond_to_allowlist=$KEY" || return 1
  ! grep -q ALLOWLIST "$WORK/spec.out"
}

# The README must document every field the spec accepts.
scenario_readme_lists_every_field() {
  local name
  for name in $(python3 -I -c '
import sys
sys.path.insert(0, sys.argv[1])
import agent_spec
print(" ".join(f.name for f in agent_spec.FIELDS))' "$REPO_ROOT/scripts/aws"); do
    grep -qF "\`$name\`" "$REPO_ROOT/deploy/aws/agents/README.md" || { echo "README lacks $name" >&2; return 1; }
  done
}

scenario_example_spec_is_valid() {
  python3 -I "$SPEC" check "$REPO_ROOT/deploy/aws/agents/coder/agent.json" \
    && python3 -I "$SPEC" check "$REPO_ROOT/deploy/aws/agents/researcher/agent.json"
}

scenario_mcp_vars_lists_required_secrets() {
  local d="${1:-$REPO_ROOT/scripts/aws}"
  printf '{"a":"${B_KEY}","b":"${A_KEY}","c":"${B_KEY}","d":"${OPT:-x}","e":"${HOME}/y","f":"${PATH}"}' > "$WORK/m.json"
  python3 -I "$d/agent_spec.py" mcp-vars "$WORK/m.json" > "$WORK/spec.out" 2> "$WORK/spec.err" || return 1
  [ "$(tr '\n' ' ' < "$WORK/spec.out")" = "A_KEY B_KEY " ] || return 1
  python3 -I "$d/agent_spec.py" mcp-vars "$WORK/none.json" > "$WORK/spec.out" || return 1
  [ ! -s "$WORK/spec.out" ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "every field maps to its container variable" scenario_every_field_maps_to_its_variable "$R"
expect_ok "a missing file gives only the defaults" scenario_missing_file_gives_only_defaults "$R"
expect_ok "bad values are refused with a message" scenario_bad_values_are_refused "$R"
expect_ok "allowlist mode needs keys from the spec or the default" scenario_allowlist_mode_needs_keys "$R"
expect_ok "the allowlist is dropped unless respond_to is allowlist" scenario_allowlist_dropped_unless_allowlist_mode "$R"
expect_ok "mcp-vars lists each required secret once, sorted" scenario_mcp_vars_lists_required_secrets "$R"
expect_ok "the README documents every field" scenario_readme_lists_every_field "$R"
expect_ok "the committed agent specs are valid" scenario_example_spec_is_valid "$R"

mutant_spec() { SPEC_BIN="$1/agent_spec.py"; "$2"; local rc=$?; unset SPEC_BIN; return $rc; }
scenario_fields_mapped_in() { mutant_spec "$1" scenario_every_field_maps_to_its_variable; }
scenario_bad_refused_in() { mutant_spec "$1" scenario_bad_values_are_refused; }
scenario_allowlist_dropped_in() { mutant_spec "$1" scenario_allowlist_dropped_unless_allowlist_mode; }

expect_caught "memory flag never written" scenario_fields_mapped_in agent_spec.py \
  's/BUZZ_ACP_NO_MEMORY=true/BUZZ_ACP_UNUSED=true/'
expect_caught "unknown fields accepted" scenario_bad_refused_in agent_spec.py \
  's/if key not in BY_NAME:/if False:/'
expect_caught "integer range ignored" scenario_bad_refused_in agent_spec.py \
  's/if not low <= value <= high:/if False:/'
expect_caught "allowlist kept in every mode" scenario_allowlist_dropped_in agent_spec.py \
  's/ and merged\.get\("respond_to"\) != "allowlist":/ and False:/'

exit "$fail"
