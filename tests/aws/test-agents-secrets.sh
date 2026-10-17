#!/usr/bin/env bash
# Tests for scripts/aws/agents-secrets.sh with a stub aws.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

NOT_FOUND="An error occurred (ParameterNotFound) when calling the GetParameter operation: Parameter not found."
OAUTH="oauth-token-AAA111"
APIKEY="api-key-BBB222"
OPENAI="openai-key-CCC333"

write_aws_env

# run_secrets <scripts-dir> <agents> [VAR=value ...]: sets RC. The credential
# variables are cleared first, so each scenario sets exactly what it needs.
run_secrets() {
  local dir="$1" agents="$2"
  shift 2
  RC=0
  env -u CLAUDE_CODE_OAUTH_TOKEN -u ANTHROPIC_API_KEY -u OPENAI_API_KEY AGENTS="$agents" ${@+"$@"} \
    bash "$dir/agents-secrets.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

# all_absent: every get-parameter reports ParameterNotFound.
all_absent() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.err"
}

puts() { log_count '^aws ssm put-parameter'; }

scenario_claude_needs_a_credential() {
  all_absent
  run_secrets "$1" claude
  [ "$RC" -eq 1 ] || return 1
  grep -qF CLAUDE_CODE_OAUTH_TOKEN "$WORK/err" && grep -qF ANTHROPIC_API_KEY "$WORK/err" || return 1
  ! grep -qF OPENAI_API_KEY "$WORK/err" || return 1
  [ "$(puts)" -eq 0 ]
}

scenario_oauth_only() {
  all_absent
  run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/agents/CLAUDE_CODE_OAUTH_TOKEN --type SecureString ' "$STUB_LOG" || return 1
  [ "$(cat "$STUB_DIR/put-values.log")" = "$OAUTH" ] || return 1
  ! grep -qF OPENAI_API_KEY "$STUB_LOG" "$WORK/err"
}

scenario_oauth_wins_over_api_key() {
  all_absent
  run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH" ANTHROPIC_API_KEY="$APIKEY"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qF '/buzz/agents/CLAUDE_CODE_OAUTH_TOKEN ' "$STUB_LOG" || return 1
  ! grep -qE 'put-parameter.*ANTHROPIC_API_KEY' "$STUB_LOG" || return 1
  ! grep -qF "$APIKEY" "$STUB_DIR/put-values.log"
}

scenario_api_key_only() {
  all_absent
  run_secrets "$1" claude ANTHROPIC_API_KEY="$APIKEY"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/agents/ANTHROPIC_API_KEY --type SecureString ' "$STUB_LOG" || return 1
  [ "$(cat "$STUB_DIR/put-values.log")" = "$APIKEY" ]
}

scenario_two_claude_agents_store_once() {
  all_absent
  run_secrets "$1" a:claude,b:claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ]
}

scenario_second_run_stores_nothing() {
  reset_stubs
  run_secrets "$1" claude,codex CLAUDE_CODE_OAUTH_TOKEN="$OAUTH" OPENAI_API_KEY="$OPENAI"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 0 ] || return 1
  # Two credentials, plus the check for the other Anthropic parameter.
  [ "$(log_count '^aws ssm get-parameter ')" -eq 3 ]
}

# exists <NAME>: the stub reports /buzz/agents/NAME as present.
exists() {
  echo 0 > "$STUB_DIR/ssm-get-parameter.$1.rc"
  : > "$STUB_DIR/ssm-get-parameter.$1.err"
}

scenario_note_when_api_key_already_stored() {
  all_absent
  exists ANTHROPIC_API_KEY
  run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 0 ] || return 1
  [ "$(grep -c '^note:' "$WORK/out")" -eq 1 ] || return 1
  grep '^note:' "$WORK/out" | grep -qF 'ANTHROPIC_API_KEY also exists' || return 1
  grep '^note:' "$WORK/out" | grep -qF 'OAuth token'
}

scenario_note_when_token_already_stored() {
  all_absent
  exists CLAUDE_CODE_OAUTH_TOKEN
  run_secrets "$1" claude ANTHROPIC_API_KEY="$APIKEY"
  [ "$RC" -eq 0 ] || return 1
  [ "$(grep -c '^note:' "$WORK/out")" -eq 1 ] || return 1
  grep '^note:' "$WORK/out" | grep -qF 'CLAUDE_CODE_OAUTH_TOKEN exists'
}

scenario_no_note_otherwise() {
  all_absent
  run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 0 ] && ! grep -q '^note:' "$WORK/out"
}

scenario_codex_needs_openai() {
  all_absent
  run_secrets "$1" codex CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 1 ] || return 1
  grep -qF OPENAI_API_KEY "$WORK/err" || return 1
  [ "$(puts)" -eq 0 ]
}

scenario_codex_only_stores_openai() {
  all_absent
  run_secrets "$1" codex OPENAI_API_KEY="$OPENAI"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 1 ] || return 1
  grep -qF '/buzz/agents/OPENAI_API_KEY ' "$STUB_LOG"
}

# One runtime missing its credential stops everything, so no half-configured state.
scenario_missing_runtime_stores_nothing() {
  all_absent
  run_secrets "$1" claude,codex CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -eq 1 ] && grep -qF OPENAI_API_KEY "$WORK/err" && [ "$(puts)" -eq 0 ]
}

scenario_both_runtimes() {
  all_absent
  run_secrets "$1" claude,codex ANTHROPIC_API_KEY="$APIKEY" OPENAI_API_KEY="$OPENAI"
  [ "$RC" -eq 0 ] || return 1
  [ "$(puts)" -eq 2 ] || return 1
  grep -qF '/buzz/agents/ANTHROPIC_API_KEY ' "$STUB_LOG" && grep -qF '/buzz/agents/OPENAI_API_KEY ' "$STUB_LOG"
}

scenario_values_never_printed() {
  all_absent
  run_secrets "$1" claude,codex CLAUDE_CODE_OAUTH_TOKEN="$OAUTH" OPENAI_API_KEY="$OPENAI"
  [ -f "$STUB_DIR/put-values.log" ] || return 1
  [ "$(wc -l < "$STUB_DIR/put-values.log" | tr -d ' ')" -eq 2 ] || return 1
  local v
  for v in "$OAUTH" "$OPENAI"; do
    grep -qF -- "$v" "$STUB_DIR/put-values.log" || return 1
    ! grep -qF -- "$v" "$STUB_LOG" "$WORK/out" "$WORK/err" || return 1
  done
  [ "$(log_count '--value file://')" -eq 2 ]
}

scenario_unsafe_value_refused() {
  local v
  for v in 'pa$ss' 'two words' 'a"quote' 'a=b' 'a/b' 'a#b'; do
    all_absent
    run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$v"
    [ "$RC" -eq 1 ] || return 1
    [ "$(puts)" -eq 0 ] || return 1
    grep -qF CLAUDE_CODE_OAUTH_TOKEN "$WORK/err" || return 1
    ! grep -qF -- "$v" "$WORK/err" "$STUB_LOG" || return 1
  done
}

scenario_empty_agents() {
  all_absent
  run_secrets "$1" ""
  [ "$RC" -eq 0 ] && [ "$(puts)" -eq 0 ] && [ "$(log_count '^aws ')" -eq 0 ]
}

scenario_other_error_stops() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "An error occurred (AccessDeniedException) when calling the GetParameter operation: no" > "$STUB_DIR/ssm-get-parameter.err"
  run_secrets "$1" claude CLAUDE_CODE_OAUTH_TOKEN="$OAUTH"
  [ "$RC" -ne 0 ] && [ "$(puts)" -eq 0 ] && grep -qF AccessDeniedException "$WORK/err"
}

R="$REPO_ROOT/scripts/aws"
expect_ok "claude agents with no credential exit 1 naming both variables" scenario_claude_needs_a_credential "$R"
expect_ok "OAuth token only: one put, no OpenAI requirement" scenario_oauth_only "$R"
expect_ok "OAuth token and API key set: only the token is stored" scenario_oauth_wins_over_api_key "$R"
expect_ok "API key only: one put to ANTHROPIC_API_KEY" scenario_api_key_only "$R"
expect_ok "two claude agents store the credential once" scenario_two_claude_agents_store_once "$R"
expect_ok "a second run makes no put" scenario_second_run_stores_nothing "$R"
expect_ok "a stored API key next to a new OAuth token gets a note" scenario_note_when_api_key_already_stored "$R"
expect_ok "a stored OAuth token next to a new API key gets a note" scenario_note_when_token_already_stored "$R"
expect_ok "no note when the other Anthropic parameter is absent" scenario_no_note_otherwise "$R"
expect_ok "codex agent without OPENAI_API_KEY exits 1" scenario_codex_needs_openai "$R"
expect_ok "codex only stores just OPENAI_API_KEY" scenario_codex_only_stores_openai "$R"
expect_ok "one runtime missing its credential stores nothing" scenario_missing_runtime_stores_nothing "$R"
expect_ok "claude and codex store one credential each" scenario_both_runtimes "$R"
expect_ok "values go through file:// and never reach output or the call log" scenario_values_never_printed "$R"
expect_ok "unsafe characters are refused by variable name, never shown" scenario_unsafe_value_refused "$R"
expect_ok "empty AGENTS does nothing" scenario_empty_agents "$R"
expect_ok "a read error other than not-found stops with no put" scenario_other_error_stops "$R"

expect_caught "API key preferred over OAuth token" scenario_oauth_wins_over_api_key agents-secrets.sh \
  's/if \[ -n "\$\{CLAUDE_CODE_OAUTH_TOKEN:-\}" \]; then/if false; then/'
expect_caught "both claude credentials stored" scenario_oauth_wins_over_api_key agents-secrets.sh \
  's/names\+=\(CLAUDE_CODE_OAUTH_TOKEN\)/names+=(CLAUDE_CODE_OAUTH_TOKEN ANTHROPIC_API_KEY)/'
expect_caught "note removed" scenario_note_when_api_key_already_stored agents-secrets.sh \
  's/^        echo "note: \/buzz\/agents\/ANTHROPIC_API_KEY also exists.*$/        true/'
expect_caught "note removed for the reverse case" scenario_note_when_token_already_stored agents-secrets.sh \
  's/^        echo "note: \/buzz\/agents\/CLAUDE_CODE_OAUTH_TOKEN exists.*$/        true/'
expect_caught "credential queued once per claude agent" scenario_two_claude_agents_store_once agents-secrets.sh \
  's/claude\) has_claude=1 ;;/claude) has_claude=$((has_claude + 1)) ;;/; s/names\+=\(CLAUDE_CODE_OAUTH_TOKEN\)/for ((i = 0; i < has_claude; i++)); do names+=(CLAUDE_CODE_OAUTH_TOKEN); done/; s/\[ "\$has_claude" -eq 1 \]; then$/[ "$has_claude" -ge 1 ]; then/'
expect_caught "existence check removed" scenario_second_run_stores_nothing agents-secrets.sh \
  's/if parameter_exists "\$param"; then/if false; then/'
expect_caught "missing credential tolerated" scenario_claude_needs_a_credential agents-secrets.sh \
  's/^\[ -z "\$missing" \] \|\| die .*/true/'
expect_caught "missing OpenAI key not required" scenario_codex_needs_openai agents-secrets.sh \
  's/missing="\$missing OPENAI_API_KEY \(for the codex agents\);"/true/'
expect_caught "value passed on the command line" scenario_values_never_printed agents-secrets.sh \
  's#--value "file://\$WORK/value"#--value "${!name}"#'
expect_caught "character check removed" scenario_unsafe_value_refused agents-secrets.sh \
  's/^  \[\[ "\$\{!name\}" =~ .*$/  true \\/'

exit "$fail"
