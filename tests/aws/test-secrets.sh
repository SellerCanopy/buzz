#!/usr/bin/env bash
# Tests for scripts/aws/secrets.sh with stub aws and openssl-generated values.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

NAMES="BUZZ_RELAY_PRIVATE_KEY BUZZ_GIT_HOOK_HMAC_SECRET POSTGRES_PASSWORD REDIS_PASSWORD BUZZ_S3_ACCESS_KEY BUZZ_S3_SECRET_KEY"
NOT_FOUND="An error occurred (ParameterNotFound) when calling the GetParameter operation: Parameter not found."
DENIED="An error occurred (AccessDeniedException) when calling the GetParameter operation: not authorized"

write_aws_env

# run_secrets <scripts-dir>: sets RC; output goes to $WORK/out and $WORK/err.
run_secrets() {
  RC=0
  bash "$1/secrets.sh" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

# all_absent: every get-parameter reports ParameterNotFound.
all_absent() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.err"
}

scenario_creates_all() {
  all_absent
  run_secrets "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm put-parameter')" -eq 6 ] || return 1
  local n
  for n in $NAMES; do
    grep -qE "^aws ssm put-parameter --name /buzz/$n --type SecureString " "$STUB_LOG" || return 1
  done
}

scenario_value_shapes() {
  all_absent
  run_secrets "$1"
  [ "$RC" -eq 0 ] || return 1
  local want="64 64 48 48 48 48" got="" line
  while IFS= read -r line; do
    [[ "$line" =~ ^[0-9a-f]+$ ]] || return 1
    got="$got ${#line}"
  done < "$STUB_DIR/put-values.log"
  [ "${got# }" = "$want" ] || return 1
  [ "$(sort -u "$STUB_DIR/put-values.log" | wc -l | tr -d ' ')" -eq 6 ] || return 1
}

scenario_values_not_printed() {
  all_absent
  run_secrets "$1"
  [ "$(wc -l < "$STUB_DIR/put-values.log" 2> /dev/null | tr -d ' ')" -eq 6 ] || return 1
  local line
  while IFS= read -r line; do
    grep -qF -- "$line" "$STUB_LOG" "$WORK/out" "$WORK/err" && return 1
  done < "$STUB_DIR/put-values.log"
  return 0
}

scenario_value_via_file() {
  all_absent
  run_secrets "$1"
  [ "$(log_count '^aws ssm put-parameter')" -eq 6 ] || return 1
  [ "$(log_count '--value file://')" -eq 6 ]
}

scenario_present_twice_no_put() {
  reset_stubs
  run_secrets "$1"; local a=$RC
  run_secrets "$1"; local b=$RC
  [ "$a" -eq 0 ] && [ "$b" -eq 0 ] || return 1
  [ "$(log_count 'put-parameter')" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm get-parameter')" -eq 12 ] || return 1
}

scenario_only_missing_created() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.POSTGRES_PASSWORD.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.POSTGRES_PASSWORD.err"
  run_secrets "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm put-parameter')" -eq 1 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/POSTGRES_PASSWORD ' "$STUB_LOG"
}

scenario_other_error_stops() {
  reset_stubs
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$DENIED" > "$STUB_DIR/ssm-get-parameter.err"
  run_secrets "$1"
  [ "$RC" -ne 0 ] || return 1
  [ "$(log_count 'put-parameter')" -eq 0 ] || return 1
  grep -qF AccessDeniedException "$WORK/err"
}

scenario_put_failure_fails() {
  all_absent
  echo 1 > "$STUB_DIR/ssm-put-parameter.rc"
  run_secrets "$1"
  [ "$RC" -ne 0 ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "creates all six when absent, as SecureString" scenario_creates_all "$R"
expect_ok "keys are 64 hex, passwords and S3 values are 48 hex, all distinct" scenario_value_shapes "$R"
expect_ok "generated values never reach stdout, stderr, or the call log" scenario_values_not_printed "$R"
expect_ok "values are passed as file:// so they stay out of process lists" scenario_value_via_file "$R"
expect_ok "two runs with all present make zero put-parameter calls" scenario_present_twice_no_put "$R"
expect_ok "creates only the missing parameter" scenario_only_missing_created "$R"
expect_ok "an error other than not-found stops with no put" scenario_other_error_stops "$R"
expect_ok "a failed put fails the script" scenario_put_failure_fails "$R"

expect_caught "existence check removed" scenario_present_twice_no_put secrets.sh \
  's/if parameter_exists "\$param"; then/if false; then/'
expect_caught "only-missing check removed" scenario_only_missing_created secrets.sh \
  's/if parameter_exists "\$param"; then/if false; then/'
expect_caught "any get error treated as absent" scenario_other_error_stops secrets.sh \
  's/\*\) die "cannot check \$1: \$err" ;;/*) return 1 ;;/'
expect_caught "value passed on the command line" scenario_value_via_file secrets.sh \
  's#--value "file://\$WORK/value"#--value "$(cat "$WORK/value")"#'
expect_caught "base64 values" scenario_value_shapes secrets.sh \
  's/openssl rand -hex/openssl rand -base64/'

exit "$fail"
