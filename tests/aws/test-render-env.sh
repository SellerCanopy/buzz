#!/usr/bin/env bash
# Tests for scripts/aws/render-env.sh with a stub aws that returns the secrets.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

IMAGE="repo:abc1234"
REAL_TEMPLATE="$REPO_ROOT/deploy/compose/.env.example"
NAMES="BUZZ_RELAY_PRIVATE_KEY BUZZ_GIT_HOOK_HMAC_SECRET POSTGRES_PASSWORD REDIS_PASSWORD BUZZ_S3_ACCESS_KEY BUZZ_S3_SECRET_KEY"

# Small template in the same shape as the real one.
cat > "$WORK/template.env" <<'T'
# Replace every CHANGE_ME value.
BUZZ_IMAGE=ghcr.io/block/buzz:main
BUZZ_DOMAIN=old.example.com
RELAY_URL=wss://old.example.com
BUZZ_MEDIA_BASE_URL=https://old.example.com/media
BUZZ_MEDIA_SERVER_DOMAIN=old.example.com
BUZZ_CORS_ORIGINS=https://old.example.com
RELAY_OWNER_PUBKEY=CHANGE_ME_OWNER_PUBKEY_HEX
BUZZ_RELAY_PRIVATE_KEY=CHANGE_ME_64_HEX_PRIVATE_KEY
BUZZ_GIT_HOOK_HMAC_SECRET=CHANGE_ME_RANDOM_64_HEX
POSTGRES_USER=buzz
POSTGRES_PASSWORD=CHANGE_ME_RANDOM_PASSWORD
REDIS_PASSWORD=CHANGE_ME_RANDOM_PASSWORD
BUZZ_S3_ACCESS_KEY=CHANGE_ME_RANDOM_ACCESS_KEY
BUZZ_S3_SECRET_KEY=CHANGE_ME_RANDOM_SECRET_KEY
T

# secrets_out [skip-name]: stub answer for get-parameters-by-path. Includes a
# parameter the script must ignore.
secrets_out() {
  local n
  : > "$STUB_DIR/ssm-get-parameters-by-path.out"
  for n in $NAMES; do
    [ "$n" = "${1:-}" ] && continue
    printf '/buzz/%s\tvalue-of-%s\n' "$n" "$n" >> "$STUB_DIR/ssm-get-parameters-by-path.out"
  done
  printf '/buzz/last_good_image\tabc1234\n' >> "$STUB_DIR/ssm-get-parameters-by-path.out"
}

# run_render <scripts-dir> <template> [env assignments...]: sets RC; stdout in
# $WORK/out, stderr in $WORK/err.
run_render() {
  local dir="$1" template="$2"
  shift 2
  RC=0
  env BUZZ_DOMAIN="$DOMAIN" RELAY_OWNER_PUBKEY="$PUBKEY" "$@" \
    bash "$dir/render-env.sh" "$template" "$IMAGE" > "$WORK/out" 2> "$WORK/err" || RC=$?
}

scenario_renders_small_template() {
  reset_stubs; secrets_out
  run_render "$1" "$WORK/template.env"
  [ "$RC" -eq 0 ] || return 1
  ! grep -q CHANGE_ME "$WORK/out" || return 1
  grep -qxF "BUZZ_IMAGE=$IMAGE" "$WORK/out" || return 1
  grep -qxF "RELAY_URL=wss://$DOMAIN" "$WORK/out" || return 1
  grep -qxF "BUZZ_DOMAIN=$DOMAIN" "$WORK/out" || return 1
  grep -qxF "BUZZ_CORS_ORIGINS=https://$DOMAIN" "$WORK/out" || return 1
  grep -qxF "BUZZ_MEDIA_BASE_URL=https://$DOMAIN/media" "$WORK/out" || return 1
  grep -qxF "BUZZ_MEDIA_SERVER_DOMAIN=$DOMAIN" "$WORK/out" || return 1
  grep -qxF "RELAY_OWNER_PUBKEY=$PUBKEY" "$WORK/out" || return 1
  grep -qxF "POSTGRES_USER=buzz" "$WORK/out" || return 1
  local n
  for n in $NAMES; do
    grep -qxF "$n=value-of-$n" "$WORK/out" || return 1
  done
  ! grep -q old.example.com "$WORK/out"
}

# The real template, with the same check deploy/compose/run.sh applies.
scenario_renders_real_template() {
  reset_stubs; secrets_out
  run_render "$1" "$REAL_TEMPLATE"
  [ "$RC" -eq 0 ] || return 1
  ! grep -Eq '^[[:space:]]*[A-Za-z_][A-Za-z0-9_]*=.*CHANGE_ME' "$WORK/out" || return 1
  ! grep -q CHANGE_ME "$WORK/out" || return 1
  grep -qxF "BUZZ_IMAGE=$IMAGE" "$WORK/out" || return 1
  grep -qxF "RELAY_URL=wss://$DOMAIN" "$WORK/out" || return 1
  grep -qxF "BUZZ_REQUIRE_AUTH_TOKEN=true" "$WORK/out" || return 1
  grep -qxF "BUZZ_S3_ADDRESSING_STYLE=path" "$WORK/out"
}

scenario_ssm_call_shape() {
  reset_stubs; secrets_out
  run_render "$1" "$WORK/template.env"
  [ "$(log_count '^aws ssm get-parameters-by-path --path /buzz/ --with-decryption')" -eq 1 ] || return 1
  [ "$(log_count '--recursive')" -eq 0 ]
}

scenario_missing_secret_exits_1() {
  reset_stubs; secrets_out POSTGRES_PASSWORD
  run_render "$1" "$WORK/template.env"
  [ "$RC" -eq 1 ] || return 1
  [ ! -s "$WORK/out" ] || return 1
  grep -qF "missing SSM parameters: /buzz/POSTGRES_PASSWORD" "$WORK/err"
}

scenario_no_secrets_at_all() {
  reset_stubs
  run_render "$1" "$WORK/template.env"
  [ "$RC" -eq 1 ] && [ ! -s "$WORK/out" ] && grep -qF "missing SSM parameters" "$WORK/err"
}

scenario_leftover_change_me_exits_1() {
  reset_stubs; secrets_out
  { cat "$WORK/template.env"; echo "EXTRA_SETTING=CHANGE_ME_SOMETHING"; } > "$WORK/extra.env"
  run_render "$1" "$WORK/extra.env"
  [ "$RC" -eq 1 ] || return 1
  [ ! -s "$WORK/out" ] || return 1
  grep -qF "CHANGE_ME values remain for: EXTRA_SETTING" "$WORK/err"
}

scenario_template_without_secret_line() {
  reset_stubs; secrets_out
  grep -v '^REDIS_PASSWORD=' "$WORK/template.env" > "$WORK/short.env"
  run_render "$1" "$WORK/short.env"
  [ "$RC" -eq 1 ] && [ ! -s "$WORK/out" ] && grep -qF "template has no REDIS_PASSWORD" "$WORK/err"
}

scenario_requires_domain() {
  reset_stubs; secrets_out
  RC=0
  env -u BUZZ_DOMAIN RELAY_OWNER_PUBKEY="$PUBKEY" \
    bash "$1/render-env.sh" "$WORK/template.env" "$IMAGE" > "$WORK/out" 2> "$WORK/err" || RC=$?
  [ "$RC" -eq 1 ] && [ ! -s "$WORK/out" ] && grep -qF "BUZZ_DOMAIN is not set" "$WORK/err"
}

# Values with characters that break a .env file or a URL are refused without echoing them.
scenario_unsafe_value_refused() {
  local v
  for v in 'pa$ss#word' 'two words' 'a"quote' "a'quote" 'a=b' 'a/b'; do
    reset_stubs; secrets_out
    printf '/buzz/POSTGRES_PASSWORD\t%s\n' "$v" > "$STUB_DIR/ssm-get-parameters-by-path.out.tmp"
    grep -v '^/buzz/POSTGRES_PASSWORD' "$STUB_DIR/ssm-get-parameters-by-path.out" >> "$STUB_DIR/ssm-get-parameters-by-path.out.tmp"
    mv "$STUB_DIR/ssm-get-parameters-by-path.out.tmp" "$STUB_DIR/ssm-get-parameters-by-path.out"
    run_render "$1" "$WORK/template.env"
    [ "$RC" -eq 1 ] || return 1
    [ ! -s "$WORK/out" ] || return 1
    grep -qF "/buzz/POSTGRES_PASSWORD has characters outside" "$WORK/err" || return 1
    ! grep -qF -- "$v" "$WORK/out" "$WORK/err" || return 1
  done
}

scenario_secrets_not_on_stderr() {
  reset_stubs; secrets_out POSTGRES_PASSWORD
  run_render "$1" "$WORK/template.env"
  ! grep -q value-of "$WORK/err"
}

R="$REPO_ROOT/scripts/aws"
expect_ok "fills secrets, image, domain settings, and leaves no CHANGE_ME" scenario_renders_small_template "$R"
expect_ok "renders the real .env.example with no CHANGE_ME" scenario_renders_real_template "$R"
expect_ok "reads /buzz/ with decryption and without --recursive" scenario_ssm_call_shape "$R"
expect_ok "one missing secret exits 1, prints nothing, names it" scenario_missing_secret_exits_1 "$R"
expect_ok "no secrets at all exits 1" scenario_no_secrets_at_all "$R"
expect_ok "a leftover CHANGE_ME exits 1, prints nothing, names the key" scenario_leftover_change_me_exits_1 "$R"
expect_ok "a template without a secret line exits 1" scenario_template_without_secret_line "$R"
expect_ok "missing BUZZ_DOMAIN exits 1" scenario_requires_domain "$R"
expect_ok "unsafe secret values are refused by parameter name, never shown" scenario_unsafe_value_refused "$R"
expect_ok "secret values never reach stderr" scenario_secrets_not_on_stderr "$R"

expect_caught "CHANGE_ME check removed" scenario_leftover_change_me_exits_1 render-env.sh \
  's/^\[ -z "\$left" \] \|\| die .*/true/'
expect_caught "missing-secret check removed" scenario_missing_secret_exits_1 render-env.sh \
  's/^\[ -z "\$missing" \] \|\| die .*/true/'
expect_caught "value character check removed" scenario_unsafe_value_refused render-env.sh \
  's/^  \[\[ "\$\{!var\}" =~ .*$/  true \\/'
expect_caught "template line check removed" scenario_template_without_secret_line render-env.sh \
  's/\*\) die "template has no \$name line: \$template" ;;/*) ;;/'
expect_caught "recursive read added" scenario_ssm_call_shape render-env.sh \
  's#--path /buzz/ --with-decryption#--path /buzz/ --recursive --with-decryption#'
expect_caught "domain settings not derived" scenario_renders_small_template render-env.sh \
  's#RELAY_URL\) printf .wss://%s. "\$BUZZ_DOMAIN" ;;#RELAY_URL) printf "wss://localhost" ;;#'

exit "$fail"
