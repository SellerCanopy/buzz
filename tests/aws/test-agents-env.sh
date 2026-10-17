#!/usr/bin/env bash
# Tests for scripts/aws/agents-env.sh with a stub aws.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

NOT_FOUND="An error occurred (ParameterNotFound) when calling the GetParameter operation: Parameter not found."
TOKEN="ghp_Secret.value-123"
CFG="$WORK/cfg"

write_aws_env

# setup: fresh stubs, every parameter absent, and an agent "coder" whose mcp.json
# uses two secrets and one defaulted variable.
setup() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.err"
  rm -rf "$CFG"
  mkdir -p "$CFG/coder"
  cat > "$CFG/coder/mcp.json" <<'JSON'
{"mcpServers": {"gh": {"command": "npx", "args": ["-y", "gh-mcp@1.0.0"],
  "env": {"GITHUB_TOKEN": "${GITHUB_TOKEN}", "BRAVE_API_KEY": "${BRAVE_API_KEY}", "MODE": "${MODE:-fast}", "H": "${HOME}"}}}}
JSON
}

puts() { log_count '^aws ssm put-parameter'; }

# run_env <scripts-dir> [VAR=value ...]: sets RC. Callers pass AGENT, VAR, and the
# secret itself.
run_env() {
  local dir="$1"
  shift
  RC=0
  env -u GITHUB_TOKEN -u BRAVE_API_KEY -u OTHER_KEY -u MODE AGENTS="coder:claude,other:claude" AGENT_CONFIG_DIR="$CFG" ${@+"$@"} \
    bash "$dir/agents-env.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

scenario_stores_the_secret() {
  setup
  run_env "$1" AGENT=coder VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/agents/coder/env/GITHUB_TOKEN --type SecureString --overwrite ' "$STUB_LOG" || return 1
  [ "$(cat "$STUB_DIR/put-values.log")" = "$TOKEN" ] || return 1
  grep -qF 'created /buzz/agents/coder/env/GITHUB_TOKEN' "$WORK/out"
}

scenario_value_never_printed() {
  setup
  run_env "$1" AGENT=coder VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -eq 0 ] || return 1
  ! grep -qF -- "$TOKEN" "$STUB_LOG" "$WORK/out" "$WORK/err" || return 1
  [ "$(log_count '--value file://')" -eq 1 ]
}

scenario_existing_secret_is_replaced() {
  setup
  echo 0 > "$STUB_DIR/ssm-get-parameter.rc"
  : > "$STUB_DIR/ssm-get-parameter.err"
  run_env "$1" AGENT=coder VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qF -- '--overwrite' "$STUB_LOG" || return 1
  grep -qF 'updated /buzz/agents/coder/env/GITHUB_TOKEN' "$WORK/out"
}

scenario_agent_must_be_in_agents() {
  setup
  run_env "$1" AGENT=ghost VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -eq 1 ] && [ "$(puts)" -eq 0 ] && grep -qF ghost "$WORK/err"
}

scenario_agent_is_required() {
  setup
  run_env "$1" VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -eq 1 ] && [ "$(puts)" -eq 0 ] && grep -qF 'AGENT=' "$WORK/err"
}

scenario_reserved_and_bad_names_refused() {
  local v
  for v in BUZZ_PRIVATE_KEY CLAUDE_CODE_OAUTH_TOKEN ANTHROPIC_API_KEY OPENAI_API_KEY lower 1ABC; do
    setup
    run_env "$1" AGENT=coder VAR="$v" "$v=$TOKEN"
    [ "$RC" -eq 1 ] || return 1
    [ "$(puts)" -eq 0 ] || return 1
  done
}

scenario_empty_value_refused() {
  setup
  run_env "$1" AGENT=coder VAR=GITHUB_TOKEN
  [ "$RC" -eq 1 ] && [ "$(puts)" -eq 0 ] && grep -qF GITHUB_TOKEN "$WORK/err"
}

scenario_unsafe_characters_refused() {
  local v
  for v in 'pa$ss' 'two words' 'a"quote' 'a#b' "a'b" 'a\b'; do
    setup
    run_env "$1" AGENT=coder VAR=GITHUB_TOKEN GITHUB_TOKEN="$v"
    [ "$RC" -eq 1 ] || return 1
    [ "$(puts)" -eq 0 ] || return 1
    grep -qF GITHUB_TOKEN "$WORK/err" || return 1
    ! grep -qF -- "$v" "$WORK/err" "$STUB_LOG" || return 1
  done
}

# The shell value must win over a setting of the same name in .aws.env.
scenario_shell_value_wins_over_settings_file() {
  setup
  run_env "$1" AGENT=coder VAR=TF_STATE_BUCKET TF_STATE_BUCKET=from-shell
  [ "$RC" -eq 0 ] || return 1
  [ "$(cat "$STUB_DIR/put-values.log")" = "from-shell" ]
}

scenario_list_shows_stored_and_missing() {
  setup
  echo 0 > "$STUB_DIR/ssm-get-parameter.env.GITHUB_TOKEN.rc"
  run_env "$1" AGENT=coder
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 0 ] || return 1
  grep -qxF 'stored  GITHUB_TOKEN' "$WORK/out" || return 1
  grep -qF 'missing BRAVE_API_KEY' "$WORK/out" || return 1
  grep -qF 'AGENT=coder VAR=BRAVE_API_KEY' "$WORK/out" || return 1
  ! grep -qE 'MODE|HOME' "$WORK/out"
}

scenario_list_without_secrets() {
  setup
  printf '{"mcpServers": {}}\n' > "$CFG/coder/mcp.json"
  run_env "$1" AGENT=coder
  [ "$RC" -eq 0 ] && grep -qF 'uses no ${VAR} secrets' "$WORK/out"
}

scenario_note_when_mcp_json_does_not_use_it() {
  setup
  run_env "$1" AGENT=coder VAR=OTHER_KEY OTHER_KEY="$TOKEN"
  [ "$RC" -eq 0 ] && grep -qF 'does not use ${OTHER_KEY}' "$WORK/out"
}

scenario_other_read_error_stops() {
  setup
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "An error occurred (AccessDeniedException) when calling the GetParameter operation: no" > "$STUB_DIR/ssm-get-parameter.err"
  run_env "$1" AGENT=coder VAR=GITHUB_TOKEN GITHUB_TOKEN="$TOKEN"
  [ "$RC" -ne 0 ] && [ "$(puts)" -eq 0 ] && grep -qF AccessDeniedException "$WORK/err"
}

R="$REPO_ROOT/scripts/aws"
expect_ok "stores the secret at /buzz/agents/<name>/env/<VAR> as a SecureString" scenario_stores_the_secret "$R"
expect_ok "the value goes through file:// and never reaches output or the call log" scenario_value_never_printed "$R"
expect_ok "an existing secret is replaced, so rerunning rotates it" scenario_existing_secret_is_replaced "$R"
expect_ok "an agent outside AGENTS is refused" scenario_agent_must_be_in_agents "$R"
expect_ok "AGENT is required" scenario_agent_is_required "$R"
expect_ok "reserved and malformed names are refused" scenario_reserved_and_bad_names_refused "$R"
expect_ok "an empty value is refused" scenario_empty_value_refused "$R"
expect_ok "unsafe characters are refused, never shown" scenario_unsafe_characters_refused "$R"
expect_ok "the shell value wins over .aws.env" scenario_shell_value_wins_over_settings_file "$R"
expect_ok "without VAR it lists stored and missing secrets" scenario_list_shows_stored_and_missing "$R"
expect_ok "an mcp.json without secrets says so" scenario_list_without_secrets "$R"
expect_ok "a secret the mcp.json does not use gets a note" scenario_note_when_mcp_json_does_not_use_it "$R"
expect_ok "a read error other than not-found stops with no put" scenario_other_read_error_stops "$R"

expect_caught "existing secret not replaced" scenario_existing_secret_is_replaced agents-env.sh \
  's/ --overwrite \\$/ \\/'
expect_caught "value passed on the command line" scenario_value_never_printed agents-env.sh \
  's#--value "file://\$WORK/value"#--value "$value"#'
expect_caught "character check removed" scenario_unsafe_characters_refused agents-env.sh \
  's/^\[\[ "\$value" =~ .*$/true \\/'
expect_caught "agent membership not checked" scenario_agent_must_be_in_agents agents-env.sh \
  's/^\[ "\$known" -eq 1 \] .*$/true/'
expect_caught "name check removed" scenario_reserved_and_bad_names_refused agent_spec.py \
  's/^    if name.startswith\(RESERVED_PREFIXES\):$/    if False:/'
expect_caught "list mode puts" scenario_list_shows_stored_and_missing agents-env.sh \
  's/^  exit 0$/  aws ssm put-parameter --name x --type String --value y; exit 0/'
expect_caught "value read after settings load" scenario_shell_value_wins_over_settings_file agents-env.sh \
  's/^  value="\$\{!var:-\}"$/  value=""/; s/^load_settings$/load_settings; value="${!var:-}"/'

exit "$fail"
