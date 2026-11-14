#!/usr/bin/env bash
# Tests for scripts/aws/hive-env.sh with a stub aws.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

NOT_FOUND="An error occurred (ParameterNotFound) when calling the GetParameter operation: Parameter not found."
DENIED="An error occurred (AccessDeniedException) when calling the GetParameter operation: not authorized"
TOKEN="0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
PASSWORD="fedcba9876543210fedcba98"
OUT_FILE="$WORK/env.hive"

write_aws_env

# setup <present|absent>: fresh stubs. The by-path read always returns both values.
setup() {
  reset_stubs
  rm -f "$OUT_FILE"
  if [ "$1" = absent ]; then
    echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
    echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.err"
  fi
  printf '/buzz/hive/BUZZ_HIVE_TOKEN_SECRET\t%s\n/buzz/hive/BUZZ_HIVE_DB_PASSWORD\t%s\n' \
    "$TOKEN" "$PASSWORD" > "$STUB_DIR/ssm-get-parameters-by-path.out"
}

# run_env <scripts-dir>: sets RC; output goes to $WORK/out and $WORK/err.
run_env() {
  RC=0
  bash "$1/hive-env.sh" "$OUT_FILE" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

scenario_creates_both_when_absent() {
  setup absent
  run_env "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm put-parameter')" -eq 2 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/hive/BUZZ_HIVE_TOKEN_SECRET --type SecureString ' "$STUB_LOG" || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/hive/BUZZ_HIVE_DB_PASSWORD --type SecureString ' "$STUB_LOG" || return 1
  [ "$(log_count '--value file://')" -eq 2 ] || return 1
  local want="64 48" got="" line
  while IFS= read -r line; do
    [[ "$line" =~ ^[0-9a-f]+$ ]] || return 1
    got="$got ${#line}"
  done < "$STUB_DIR/put-values.log"
  [ "${got# }" = "$want" ]
}

scenario_keeps_existing() {
  setup present
  run_env "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count 'put-parameter')" -eq 0 ]
}

scenario_only_missing_created() {
  setup present
  echo 254 > "$STUB_DIR/ssm-get-parameter.BUZZ_HIVE_DB_PASSWORD.rc"
  echo "$NOT_FOUND" > "$STUB_DIR/ssm-get-parameter.BUZZ_HIVE_DB_PASSWORD.err"
  run_env "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ssm put-parameter')" -eq 1 ] || return 1
  grep -qE '^aws ssm put-parameter --name /buzz/hive/BUZZ_HIVE_DB_PASSWORD ' "$STUB_LOG"
}

scenario_other_error_stops() {
  setup present
  echo 254 > "$STUB_DIR/ssm-get-parameter.rc"
  echo "$DENIED" > "$STUB_DIR/ssm-get-parameter.err"
  run_env "$1"
  [ "$RC" -ne 0 ] || return 1
  [ "$(log_count 'put-parameter')" -eq 0 ] || return 1
  [ ! -e "$OUT_FILE" ] || return 1
  grep -qF AccessDeniedException "$WORK/err"
}

scenario_file_is_private_and_complete() {
  setup present
  run_env "$1"
  [ "$RC" -eq 0 ] || return 1
  [ -f "$OUT_FILE" ] || return 1
  local mode
  mode="$(stat -f '%Lp' "$OUT_FILE" 2> /dev/null || stat -c '%a' "$OUT_FILE")"
  [ "$mode" = 600 ] || return 1
  grep -qxF "BUZZ_HIVE_DATABASE_URL=postgres://buzz_hive:$PASSWORD@postgres:5432/buzz" "$OUT_FILE" || return 1
  grep -qxF "BUZZ_HIVE_DB_PASSWORD=$PASSWORD" "$OUT_FILE" || return 1
  grep -qxF "BUZZ_HIVE_TOKEN_SECRET=$TOKEN" "$OUT_FILE" || return 1
  grep -qxF "BUZZ_HIVE_PUBLIC_URL=https://$DOMAIN" "$OUT_FILE" || return 1
  grep -qxF "BUZZ_HIVE_COMMUNITY_HOST=$DOMAIN" "$OUT_FILE" || return 1
  grep -qxF "BUZZ_HIVE_ALLOWED_ORIGINS=https://$DOMAIN" "$OUT_FILE" || return 1
  # Nothing but the file may hold the secrets.
  ! grep -qF -e "$TOKEN" -e "$PASSWORD" "$WORK/out" "$WORK/err" "$STUB_LOG"
}

scenario_generated_values_not_printed() {
  setup absent
  run_env "$1"
  [ -f "$STUB_DIR/put-values.log" ] || return 1
  local line
  while IFS= read -r line; do
    grep -qF -- "$line" "$STUB_LOG" "$WORK/out" "$WORK/err" && return 1
  done < "$STUB_DIR/put-values.log"
  return 0
}

scenario_missing_value_writes_nothing() {
  setup present
  printf '/buzz/hive/BUZZ_HIVE_TOKEN_SECRET\t%s\n' "$TOKEN" > "$STUB_DIR/ssm-get-parameters-by-path.out"
  run_env "$1"
  [ "$RC" -ne 0 ] || return 1
  [ ! -e "$OUT_FILE" ] || return 1
  grep -qF BUZZ_HIVE_DB_PASSWORD "$WORK/err" && ! grep -qF "$TOKEN" "$WORK/err"
}

scenario_unsafe_value_writes_nothing() {
  setup present
  printf '/buzz/hive/BUZZ_HIVE_TOKEN_SECRET\t%s\n/buzz/hive/BUZZ_HIVE_DB_PASSWORD\tbad pass$word\n' \
    "$TOKEN" > "$STUB_DIR/ssm-get-parameters-by-path.out"
  run_env "$1"
  [ "$RC" -ne 0 ] || return 1
  [ ! -e "$OUT_FILE" ] || return 1
  ! grep -qF 'bad pass' "$WORK/err"
}

R="$REPO_ROOT/scripts/aws"
expect_ok "creates both secrets when absent, as hex SecureString passed by file" scenario_creates_both_when_absent "$R"
expect_ok "creates nothing when both exist" scenario_keeps_existing "$R"
expect_ok "creates only the missing secret" scenario_only_missing_created "$R"
expect_ok "an error other than not-found stops with no put and no file" scenario_other_error_stops "$R"
expect_ok "the env file is mode 0600 with every variable, and no output shows a secret" scenario_file_is_private_and_complete "$R"
expect_ok "generated values never reach stdout, stderr, or the call log" scenario_generated_values_not_printed "$R"
expect_ok "a missing value fails naming the parameter and writes no file" scenario_missing_value_writes_nothing "$R"
expect_ok "an unsafe value fails without printing it and writes no file" scenario_unsafe_value_writes_nothing "$R"

expect_caught "existence check removed" scenario_keeps_existing hive-env.sh \
  's/parameter_exists "\$param" && continue/false \&\& continue/'
expect_caught "any get error treated as absent" scenario_other_error_stops params.sh \
  's/\*\) die "cannot check \$1: \$err" ;;/*) return 1 ;;/'
expect_caught "file mode left at the umask default" scenario_file_is_private_and_complete hive-env.sh \
  's/^umask 077$/umask 022/; s/^chmod 0600 "\$tmp"$/chmod 0644 "$tmp"/'
expect_caught "value passed on the command line" scenario_generated_values_not_printed hive-env.sh \
  's#--value "file://\$WORK/value"#--value "$(cat "$WORK/value")"#'
expect_caught "unsafe value accepted" scenario_unsafe_value_writes_nothing hive-env.sh \
  's/\^\[A-Za-z0-9\]\+\$/^.+$/'

exit "$fail"
