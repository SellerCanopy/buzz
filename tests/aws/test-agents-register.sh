#!/usr/bin/env bash
# Tests for scripts/aws/agents-register.sh with stub aws, terraform, and docker.
# Some scenarios also run the logged server command against a fake /opt/buzz.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

NOT_FOUND="An error occurred (ParameterNotFound) when calling the GetParameter operation: Parameter not found."
GEN_PUB="$(printf 'b%.0s' $(seq 1 64))"
GEN_SEC="$(printf 'c%.0s' $(seq 1 64))"
OPT="$WORK/opt"

write_aws_env

# stubs_ok [reported-names]: no agent has a key yet, and the server reports each
# listed name as registered.
stubs_ok() {
  reset_stubs
  printf '%s' "i-0abc123" > "$STUB_DIR/tf-instance_id.out"
  echo "cmd-1" > "$STUB_DIR/ssm-send-command.out"
  echo "Success" > "$STUB_DIR/ssm-get-command-invocation.out"
  local n out=""
  for n in ${1:-}; do out="$out""registered $n $GEN_PUB"$'\n'; done
  printf '%s' "$out" > "$STUB_DIR/ssm-get-command-invocation.StandardOutputContent.out"
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.err"
}

# have <agent> <parameter> [value]: the stub reports /buzz/agents/<agent>/<parameter>
# as present. A value is also what every get-parameter call prints.
have() {
  echo 0 > "$STUB_DIR/ssm-get-parameter.$1.$2.rc"
  : > "$STUB_DIR/ssm-get-parameter.$1.$2.err"
  if [ -n "${3:-}" ]; then printf '%s\n' "$3" > "$STUB_DIR/ssm-get-parameter.out"; fi
}

# run_register <scripts-dir> <agents>: sets RC.
run_register() {
  RC=0
  env POLL_INTERVAL=0 POLL_ATTEMPTS=3 AGENTS="$2" bash "$1/agents-register.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

sent_line() { grep -F 'aws ssm send-command' "$STUB_LOG" | head -n 1; }

# decode_command <file>: the logged SSM command as a script under the sandbox.
decode_command() {
  sent_line \
    | sed -e 's/^.*{"commands":\["//' -e 's/"\]}.*$//' -e 's/","/\
/g' -e 's/\\"/"/g' -e 's/\\\\/\\/g' \
    | sed "s#/opt/buzz#$OPT#g" > "$1"
}

# count_in_sent <fixed-string>: occurrences in the logged server command.
count_in_sent() {
  decode_command "$WORK/count.sh"
  grep -oF -- "$1" "$WORK/count.sh" | wc -l | tr -d ' '
}

scenario_one_registration_per_agent() {
  stubs_ok "a b"
  run_register "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm send-command')" -eq 1 ] || return 1
  [ "$(count_in_sent 'buzz-admin generate-key')" -eq 2 ] || return 1
  [ "$(count_in_sent 'run.sh add-member "$pub"')" -eq 2 ] || return 1
  [ "$(count_in_sent "store_key 'a'")" -eq 1 ] && [ "$(count_in_sent "store_key 'b'")" -eq 1 ] || return 1
  case "$(sent_line)" in *"--instance-ids i-0abc123 --document-name AWS-RunShellScript "*) ;; *) return 1 ;; esac
  case "$(sent_line)" in *"cd /opt/buzz/deploy/compose"*) ;; *) return 1 ;; esac
}

scenario_existing_keys_send_nothing() {
  stubs_ok
  : > "$STUB_DIR/ssm-get-parameter.err"
  echo 0 > "$STUB_DIR/ssm-get-parameter.rc"
  run_register "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count 'send-command')" -eq 0 ] || return 1
  [ "$(log_count 'generate-key|add-member')" -eq 0 ] || return 1
  # Each agent with a key costs a second lookup, for the REGISTERED marker.
  [ "$(log_count '^aws ssm get-parameter ')" -eq 4 ] || return 1
  grep -qF "kept    a" "$WORK/out" && grep -qF "kept    b" "$WORK/out" || return 1
  grep -qF "nothing to register" "$WORK/out"
}

# A key with no REGISTERED marker means an earlier run stopped before the relay
# accepted the member. The rerun adds that member from the stored public key and
# never generates a new identity.
scenario_stored_key_without_marker_resumes() {
  stubs_ok "a"
  have a BUZZ_PRIVATE_KEY
  have a BUZZ_PUBLIC_KEY "$GEN_PUB"
  run_register "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm send-command')" -eq 1 ] || return 1
  [ "$(count_in_sent 'buzz-admin generate-key')" -eq 0 ] || return 1
  [ "$(count_in_sent "store_key 'a'")" -eq 0 ] || return 1
  [ "$(count_in_sent 'run.sh add-member "$pub"')" -eq 1 ] || return 1
  grep -qF "resume  a" "$WORK/out" || return 1
  server_setup
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  : > "$STUB_DIR/put-values.log"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -eq 0 ] || return 1
  [ "$(log_count "^run.sh add-member $GEN_PUB\$")" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/REGISTERED ')" -eq 1 ] || return 1
  [ "$(log_count 'BUZZ_PRIVATE_KEY|generate-key')" -eq 0 ] || return 1
  grep -qF "registered a $GEN_PUB" "$WORK/sout"
}

# A key with neither marker nor public key cannot be added to the relay, and the
# command must stop and say how to recover instead of sending anything.
scenario_key_without_public_key_fails() {
  stubs_ok
  have a BUZZ_PRIVATE_KEY
  run_register "$1" a:claude
  [ "$RC" -ne 0 ] || return 1
  grep -qF "agent a has a key" "$WORK/err" || return 1
  grep -qF "BUZZ_PRIVATE_KEY" "$WORK/err" || return 1
  [ "$(log_count 'send-command')" -eq 0 ]
}

# Add-member fails on the first run, so the identity is stored but not marked.
# The second run adds the member from the stored key and writes the marker.
scenario_failed_add_member_is_retried() {
  stubs_ok "a"
  run_register "$1" a:claude
  server_setup
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  : > "$STUB_DIR/put-values.log"
  echo 1 > "$STUB_DIR/run-sh.rc"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -ne 0 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/BUZZ_PUBLIC_KEY ')" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/BUZZ_PRIVATE_KEY ')" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/REGISTERED ')" -eq 0 ] || return 1
  # What the first run stored is now visible to the second.
  rm -f "$STUB_DIR/run-sh.rc"
  have a BUZZ_PRIVATE_KEY
  have a BUZZ_PUBLIC_KEY "$GEN_PUB"
  : > "$STUB_LOG"
  run_register "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(count_in_sent 'buzz-admin generate-key')" -eq 0 ] || return 1
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  : > "$STUB_DIR/put-values.log"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -eq 0 ] || return 1
  [ "$(log_count "^run.sh add-member $GEN_PUB\$")" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/REGISTERED ')" -eq 1 ] || return 1
  [ "$(log_count 'put-parameter --name /buzz/agents/a/BUZZ_PRIVATE_KEY ')" -eq 0 ]
}

scenario_only_missing_agent_registered() {
  stubs_ok "b"
  have a BUZZ_PRIVATE_KEY
  have a REGISTERED
  run_register "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  [ "$(count_in_sent 'buzz-admin generate-key')" -eq 1 ] || return 1
  [ "$(count_in_sent "store_key 'b'")" -eq 1 ] || return 1
  [ "$(count_in_sent "store_key 'a'")" -eq 0 ]
}

scenario_checks_the_per_agent_key_name() {
  stubs_ok "a b"
  run_register "$1" a:claude,b:claude
  grep -qF 'aws ssm get-parameter --name /buzz/agents/a/BUZZ_PRIVATE_KEY ' "$STUB_LOG" \
    && grep -qF 'aws ssm get-parameter --name /buzz/agents/b/BUZZ_PRIVATE_KEY ' "$STUB_LOG"
}

# One identity per agent name, not per runtime.
scenario_key_per_name_not_runtime() {
  stubs_ok "x y z"
  run_register "$1" x:claude,y:claude,z:codex
  [ "$RC" -eq 0 ] && [ "$(count_in_sent 'buzz-admin generate-key')" -eq 3 ]
}

scenario_empty_agents() {
  stubs_ok
  run_register "$1" ""
  [ "$RC" -eq 0 ] && [ "$(log_count '^aws ssm send-command')" -eq 0 ]
}

scenario_unreported_agent_fails() {
  stubs_ok "a"
  run_register "$1" a:claude,b:claude
  [ "$RC" -ne 0 ] && grep -qF "without reporting b as registered" "$WORK/err"
}

scenario_failed_command_fails() {
  stubs_ok "a"
  echo "Failed" > "$STUB_DIR/ssm-get-command-invocation.out"
  run_register "$1" a:claude
  [ "$RC" -ne 0 ] && grep -qF "status Failed" "$WORK/err"
}

# --- running the logged server command against a fake /opt/buzz ---

# server_setup: a fake release directory with a run.sh that only logs.
server_setup() {
  rm -rf "$OPT"
  mkdir -p "$OPT/deploy/compose"
  cat > "$OPT/deploy/compose/run.sh" <<'RUN'
#!/usr/bin/env bash
echo "run.sh $*" >> "$STUB_LOG"
rc=0
[ -f "$STUB_DIR/run-sh.rc" ] && rc="$(cat "$STUB_DIR/run-sh.rc")"
exit "$rc"
RUN
  chmod +x "$OPT/deploy/compose/run.sh"
  printf 'Public key:  %s\nSecret key:  %s\n\nSet BUZZ_PRIVATE_KEY to the secret key to use this identity.\n' \
    "$GEN_PUB" "$GEN_SEC" > "$STUB_DIR/docker-compose-exec.out"
}

# run_server <scripts-dir> <agents>: registers, then runs the logged command.
run_server() {
  stubs_ok "a b"
  run_register "$1" "$2"
  server_setup
  decode_command "$WORK/server.sh"
  : > "$STUB_LOG"
  : > "$STUB_DIR/put-values.log"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
}

scenario_server_order_and_secret_handling() {
  run_server "$1" a:claude,b:claude
  [ "$SRC" -eq 0 ] || return 1
  grep -qF "registered a $GEN_PUB" "$WORK/sout" && grep -qF "registered b $GEN_PUB" "$WORK/sout" || return 1
  [ "$(log_count 'generate-key')" -eq 2 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/agents/a/BUZZ_PRIVATE_KEY --type SecureString --value file://' "$STUB_LOG" || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/agents/b/BUZZ_PRIVATE_KEY --type SecureString --value file://' "$STUB_LOG" || return 1
  grep -qxF "aws ssm put-parameter --name /buzz/agents/a/BUZZ_PUBLIC_KEY --type String --overwrite --value $GEN_PUB" "$STUB_LOG" || return 1
  grep -qxF "aws ssm put-parameter --name /buzz/agents/b/REGISTERED --type String --overwrite --value yes" "$STUB_LOG" || return 1
  # Three parameters per agent.
  [ "$(log_count '^aws ssm put-parameter')" -eq 6 ] || return 1
  [ "$(log_count "^run.sh add-member $GEN_PUB\$")" -eq 2 ] || return 1
  # The secret is stored from a file and never printed or put on a command line.
  [ "$(grep -cxF "$GEN_SEC" "$STUB_DIR/put-values.log")" -eq 2 ] || return 1
  ! grep -qF "$GEN_SEC" "$STUB_LOG" "$WORK/sout" "$WORK/serr" || return 1
  # For agent a: generate, store the public key, store the secret, add as a
  # member, and only then mark it registered.
  local gen pubp secp add mark
  gen="$(log_line 'generate-key')"
  pubp="$(log_line 'put-parameter --name /buzz/agents/a/BUZZ_PUBLIC_KEY ')"
  secp="$(log_line 'put-parameter --name /buzz/agents/a/BUZZ_PRIVATE_KEY ')"
  add="$(log_line '^run.sh add-member')"
  mark="$(log_line 'put-parameter --name /buzz/agents/a/REGISTERED ')"
  [ -n "$gen" ] && [ -n "$pubp" ] && [ -n "$secp" ] && [ -n "$add" ] && [ -n "$mark" ] || return 1
  [ "$gen" -lt "$pubp" ] && [ "$pubp" -lt "$secp" ] && [ "$secp" -lt "$add" ] && [ "$add" -lt "$mark" ]
}

scenario_server_add_member_failure_names_pubkey() {
  run_server "$1" a:claude
  stubs_ok "a"
  server_setup
  : > "$STUB_LOG"
  echo 1 > "$STUB_DIR/run-sh.rc"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -ne 0 ] || return 1
  grep -qF "$GEN_PUB" "$WORK/serr" && grep -qF "add-member failed for a" "$WORK/serr" || return 1
  grep -qF "Run this command again" "$WORK/serr" || return 1
  [ "$(log_count 'REGISTERED')" -eq 0 ] || return 1
  ! grep -qF "registered a" "$WORK/sout"
}

scenario_server_bad_generate_output_stores_nothing() {
  run_server "$1" a:claude
  stubs_ok "a"
  server_setup
  printf 'Public key:  nothex\nSecret key:  %s\n' "$GEN_SEC" > "$STUB_DIR/docker-compose-exec.out"
  : > "$STUB_LOG"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -ne 0 ] && [ "$(log_count 'put-parameter')" -eq 0 ] && [ "$(log_count 'add-member')" -eq 0 ]
}

scenario_server_put_failure_skips_add_member() {
  run_server "$1" a:claude
  stubs_ok "a"
  server_setup
  echo 1 > "$STUB_DIR/ssm-put-parameter.rc"
  : > "$STUB_LOG"
  SRC=0
  bash "$WORK/server.sh" > "$WORK/sout" 2> "$WORK/serr" || SRC=$?
  [ "$SRC" -ne 0 ] && [ "$(log_count '^run.sh add-member')" -eq 0 ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "sends one command with generate-key and add-member once per agent" scenario_one_registration_per_agent "$R"
expect_ok "agents whose keys exist send nothing" scenario_existing_keys_send_nothing "$R"
expect_ok "only the agent without a key is registered" scenario_only_missing_agent_registered "$R"
expect_ok "a stored key without a marker is added again, with no new key" scenario_stored_key_without_marker_resumes "$R"
expect_ok "a key with no marker and no public key fails and sends nothing" scenario_key_without_public_key_fails "$R"
expect_ok "a failed add-member is retried by the next run" scenario_failed_add_member_is_retried "$R"
expect_ok "the key is looked up per agent name" scenario_checks_the_per_agent_key_name "$R"
expect_ok "one identity per agent name, not per runtime" scenario_key_per_name_not_runtime "$R"
expect_ok "empty AGENTS sends nothing" scenario_empty_agents "$R"
expect_ok "a server run that omits an agent fails" scenario_unreported_agent_fails "$R"
expect_ok "a failed server command fails" scenario_failed_command_fails "$R"
expect_ok "server: generate, store from a file, then add the member; secret never shown" scenario_server_order_and_secret_handling "$R"
expect_ok "server: a failed add-member names the public key and fails" scenario_server_add_member_failure_names_pubkey "$R"
expect_ok "server: unusable generate-key output stores nothing" scenario_server_bad_generate_output_stores_nothing "$R"
expect_ok "server: a failed store skips add-member" scenario_server_put_failure_skips_add_member "$R"

expect_caught "existence check removed" scenario_existing_keys_send_nothing agents-register.sh \
  's/if ! parameter_exists "\$base\/BUZZ_PRIVATE_KEY"; then/if true; then/'
expect_caught "existing keys still registered" scenario_only_missing_agent_registered agents-register.sh \
  's/if ! parameter_exists "\$base\/BUZZ_PRIVATE_KEY"; then/if true; then/'
expect_caught "marker ignored" scenario_stored_key_without_marker_resumes agents-register.sh \
  's/elif parameter_exists "\$base\/REGISTERED"; then/elif true; then/'
expect_caught "missing public key accepted" scenario_key_without_public_key_fails agents-register.sh \
  's/^    die "agent \$name has a key but no REGISTERED.*$/    true/'
expect_caught "marker never written" scenario_failed_add_member_is_retried agents-register.sh \
  's/^  echo "  aws ssm put-parameter --name .*REGISTERED.*$/  echo "  true"/'
expect_caught "failed add-member still marked registered" scenario_server_add_member_failure_names_pubkey agents-register.sh \
  's/\|\| add_failed [^"]*"/|| true"/'
expect_caught "unreported agent accepted" scenario_unreported_agent_fails agents-register.sh \
  's/\*\) die "the server finished without reporting \$name as registered" ;;/*) ;;/'
expect_caught "secret key never stored" scenario_server_order_and_secret_handling agents-register.sh \
  's/^  echo "  aws ssm put-parameter --name .*BUZZ_PRIVATE_KEY.*$/  echo "  true"/'
expect_caught "public key never stored" scenario_server_order_and_secret_handling agents-register.sh \
  's/^  echo "  aws ssm put-parameter --name .*BUZZ_PUBLIC_KEY.*$/  echo "  true"/'
expect_caught "public key stored after the secret" scenario_server_order_and_secret_handling agents-register.sh \
  '/put-parameter --name .*BUZZ_PUBLIC_KEY/{h;d;}; /echo "  rm -f/{G;}'
expect_caught "secret key on the command line" scenario_server_order_and_secret_handling agents-register.sh \
  's#--value \\"file://\\\$tmp/key\\"#--value \\"\\$seck\\"#'
expect_caught "key shape not checked" scenario_server_bad_generate_output_stores_nothing agents-register.sh \
  's/^  echo "  \[\[ \\"\\\$pubk\\" .*$/  echo "  true"/'

exit "$fail"
