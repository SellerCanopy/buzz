#!/usr/bin/env bash
# Tests for scripts/aws/agents-attest.sh with stub aws, terraform, cargo, and a
# stub attest_agent helper.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

PUB_A="$(printf 'a%.0s' $(seq 1 64))"
PUB_B="$(printf 'b%.0s' $(seq 1 64))"
OWNER_SECRET="$(printf 'd%.0s' $(seq 1 64))"

write_aws_env

# stub_helper: an attest_agent that logs its arguments and prints a fake tag. It
# never writes the owner key anywhere.
stub_helper() {
  cat > "$WORK/attest_agent" <<'HELPER'
#!/usr/bin/env bash
echo "attest_agent $*" >> "$STUB_LOG"
[ -n "${BUZZ_OWNER_KEY:-}" ] || exit 1
echo '["auth","owner","","sig"]'
HELPER
  chmod +x "$WORK/attest_agent"
}

# stub_buzz: a buzz CLI whose `upload file` logs the call and prints a relay URL.
stub_buzz() {
  cat > "$WORK/buzz" <<'BUZZ'
#!/usr/bin/env bash
echo "buzz $* key=${BUZZ_PRIVATE_KEY:-} relay=${BUZZ_RELAY_URL:-}" >> "$STUB_LOG"
[ "${UPLOAD_URL+set}" = set ] || UPLOAD_URL="https://buzz.example.com/media/abc.png"
printf '{"url":"%s"}\n' "$UPLOAD_URL"
BUZZ
  chmod +x "$WORK/buzz"
}

# make_config <name> [profile-json]: an agent folder in $WORK/agents with an avatar.
make_config() {
  mkdir -p "$WORK/agents/$1"
  printf 'png' > "$WORK/agents/$1/avatar.png"
  if [ -n "${2:-}" ]; then printf '%s' "$2" > "$WORK/agents/$1/agent.json"; fi
}

# stubs_ok [reported-names]: both agents have a public key and the server
# reports each listed name as attested.
stubs_ok() {
  reset_stubs
  stub_helper
  stub_buzz
  rm -rf "$WORK/agents"
  mkdir -p "$WORK/agents"
  printf '%s' "i-0abc123" > "$STUB_DIR/tf-instance_id.out"
  echo "cmd-1" > "$STUB_DIR/ssm-send-command.out"
  echo "Success" > "$STUB_DIR/ssm-get-command-invocation.out"
  local n out=""
  for n in ${1:-}; do out="$out""attested $n"$'\n'; done
  printf '%s' "$out" > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
  echo 0 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$PUB_A" > "$STUB_DIR/ssm-get-parameter.out"
  echo "$PUB_B" > "$STUB_DIR/ssm-get-parameter.b.BUZZ_PUBLIC_KEY.out"
}

# run_attest <scripts-dir> <agents> [owner-key]: sets RC.
run_attest() {
  RC=0
  env POLL_INTERVAL=0 POLL_ATTEMPTS=3 AGENTS="$2" BUZZ_OWNER_KEY="${3-$OWNER_SECRET}" \
    ATTEST_BIN="$WORK/attest_agent" BUZZ_BIN="$WORK/buzz" AGENT_CONFIG_DIR="$WORK/agents" bash "$1/agents-attest.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

sent_line() { grep -F 'aws ssm send-command' "$STUB_LOG" | head -n 1; }

scenario_owner_record_and_profile_per_agent() {
  stubs_ok "a b"
  run_attest "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^attest_agent ')" -eq 2 ] || return 1
  grep -qE "^attest_agent $PUB_A a owner-only" "$STUB_LOG" || return 1
  [ "$(log_count '^aws ssm send-command')" -eq 1 ] || return 1
  case "$(sent_line)" in *"buzz-agent-a-agent-1"*"set-profile --name 'a'"*) ;; *) return 1 ;; esac
  case "$(sent_line)" in *"buzz-agent-b-agent-1"*"set-profile --name 'b'"*) ;; *) return 1 ;; esac
  grep -qF "agents-attest: 2 attested" "$WORK/out"
}

# The owner key is read from the environment. It must not reach the server, the
# command line of any logged call, or the output.
scenario_owner_key_stays_local() {
  stubs_ok "a"
  run_attest "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  ! grep -qF "$OWNER_SECRET" "$STUB_LOG" "$WORK/out" "$WORK/err"
}

scenario_missing_owner_key_fails() {
  stubs_ok "a"
  run_attest "$1" a:claude ""
  [ "$RC" -ne 0 ] || return 1
  grep -qF "BUZZ_OWNER_KEY is not set" "$WORK/err" || return 1
  [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_empty_agents_sends_nothing() {
  stubs_ok
  run_attest "$1" ""
  [ "$RC" -eq 0 ] && [ "$(log_count '^attest_agent ')" -eq 0 ] && [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_missing_public_key_fails() {
  stubs_ok "a"
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  run_attest "$1" a:claude
  [ "$RC" -ne 0 ] || return 1
  grep -qF "make agents-register" "$WORK/err" || return 1
  [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_nobody_publishes_owner_only() {
  stubs_ok "a"
  RC=0
  env POLL_INTERVAL=0 POLL_ATTEMPTS=3 AGENTS=a:claude AGENT_RESPOND_TO=nobody BUZZ_OWNER_KEY="$OWNER_SECRET" \
    ATTEST_BIN="$WORK/attest_agent" BUZZ_BIN="$WORK/buzz" AGENT_CONFIG_DIR="$WORK/agents" bash "$1/agents-attest.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
  [ "$RC" -eq 0 ] && grep -qE "^attest_agent $PUB_A a owner-only" "$STUB_LOG"
}

scenario_unreported_agent_fails() {
  stubs_ok "a"
  run_attest "$1" a:claude,b:claude
  [ "$RC" -ne 0 ] && grep -qF "without reporting b as attested" "$WORK/err"
}

scenario_failed_command_fails() {
  stubs_ok "a"
  echo "Failed" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_attest "$1" a:claude
  [ "$RC" -ne 0 ] && grep -qF "status Failed" "$WORK/err"
}

scenario_profile_json_sets_name_about_and_avatar() {
  stubs_ok "a"
  make_config a '{"display_name":"Alpha Coder","about":"Writes code."}'
  run_attest "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  grep -qE "^attest_agent $PUB_A Alpha Coder owner-only" "$STUB_LOG" || return 1
  grep -qE "^buzz upload file --file $WORK/agents/a/avatar.png key=$OWNER_SECRET relay=https://buzz.example.com" "$STUB_LOG" || return 1
  case "$(sent_line)" in
    *"set-profile --name 'Alpha Coder' --about 'Writes code.' --avatar 'https://buzz.example.com/media/abc.png'"*) ;;
    *) return 1 ;;
  esac
}

scenario_no_avatar_skips_upload() {
  stubs_ok "a"
  run_attest "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^buzz upload')" -eq 0 ] || return 1
  case "$(sent_line)" in *"--avatar"*) return 1 ;; esac
}

scenario_unsafe_profile_text_fails() {
  stubs_ok "a"
  make_config a "{\"display_name\":\"it's\"}"
  run_attest "$1" a:claude
  [ "$RC" -ne 0 ] || return 1
  grep -qF "display_name" "$WORK/err" || return 1
  [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_foreign_upload_url_fails() {
  stubs_ok "a"
  make_config a
  RC=0
  env UPLOAD_URL="https://evil.example.net/x.png" POLL_INTERVAL=0 POLL_ATTEMPTS=3 AGENTS=a:claude \
    BUZZ_OWNER_KEY="$OWNER_SECRET" ATTEST_BIN="$WORK/attest_agent" BUZZ_BIN="$WORK/buzz" \
    AGENT_CONFIG_DIR="$WORK/agents" bash "$1/agents-attest.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
  [ "$RC" -ne 0 ] || return 1
  grep -qF "unexpected url" "$WORK/err" || return 1
  [ "$(log_count 'send-command')" -eq 0 ]
}

scenario_agent_json_overrides_respond_to() {
  stubs_ok "a b"
  make_config a '{"respond_to":"anyone"}'
  make_config b '{"respond_to":"nobody"}'
  run_attest "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  grep -qE "^attest_agent $PUB_A a anyone" "$STUB_LOG" || return 1
  grep -qE "^attest_agent [0-9a-f]{64} b owner-only" "$STUB_LOG"
}

R="$REPO_ROOT/scripts/aws"
expect_ok "publishes the owner record and sets the profile for each agent" scenario_owner_record_and_profile_per_agent "$R"
expect_ok "the owner key never reaches the log or the output" scenario_owner_key_stays_local "$R"
expect_ok "a missing owner key fails before anything is sent" scenario_missing_owner_key_fails "$R"
expect_ok "empty AGENTS sends nothing" scenario_empty_agents_sends_nothing "$R"
expect_ok "an agent without a stored public key fails and sends nothing" scenario_missing_public_key_fails "$R"
expect_ok "AGENT_RESPOND_TO=nobody is published as owner-only" scenario_nobody_publishes_owner_only "$R"
expect_ok "a server run that omits an agent fails" scenario_unreported_agent_fails "$R"
expect_ok "a failed server command fails" scenario_failed_command_fails "$R"

expect_ok "agent.json sets the name, about text, and uploaded picture" scenario_profile_json_sets_name_about_and_avatar "$R"
expect_ok "an agent without a picture uploads nothing" scenario_no_avatar_skips_upload "$R"
expect_ok "profile text with a quote fails before anything is sent" scenario_unsafe_profile_text_fails "$R"
expect_ok "an upload URL on another host fails" scenario_foreign_upload_url_fails "$R"

expect_ok "an agent's respond_to overrides the global default" scenario_agent_json_overrides_respond_to "$R"

expect_caught "owner key check removed" scenario_missing_owner_key_fails agents-attest.sh \
  's/^\[ -n "\$\{BUZZ_OWNER_KEY:-\}" \] \|\| die .*$/true/'
expect_caught "profile step dropped" scenario_owner_record_and_profile_per_agent agents-attest.sh \
  's/set-profile \$args/true \$args/'
expect_caught "unreported agent accepted" scenario_unreported_agent_fails agents-attest.sh \
  's/\*\) die "the server finished without reporting \$name as attested" ;;/*) ;;/'
expect_caught "avatar flag dropped" scenario_profile_json_sets_name_about_and_avatar agents-attest.sh \
  's/args="\$args --avatar /args="$args --unused /'
expect_caught "unsafe text accepted" scenario_unsafe_profile_text_fails agent_spec.py \
  's/ord\(c\) < 32 or c in "[^"]*"/False/'
expect_caught "upload host check removed" scenario_foreign_upload_url_fails agents-attest.sh \
  's/\*) die "the upload for \$name returned an unexpected url: \$url" ;;/*) ;;/'
expect_caught "per-agent respond_to ignored" scenario_agent_json_overrides_respond_to agents-attest.sh \
  's/respond_to="\$\(agent_field "\$config_dir\/\$name\/agent.json" respond_to "\$default_respond_to"\)"/respond_to="$default_respond_to"/'
expect_caught "nobody mapping removed" scenario_nobody_publishes_owner_only agents-attest.sh \
  's/^  \[ "\$respond_to" != "nobody" \] \|\| respond_to="owner-only"$/  true/'
expect_caught "missing public key accepted" scenario_missing_public_key_fails agents-attest.sh \
  's/\|\| die "agent \$name has no stored public key[^"]*"/|| true/'

exit "$fail"
