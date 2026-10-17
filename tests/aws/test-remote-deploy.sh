#!/usr/bin/env bash
# Tests for scripts/aws/remote-deploy.sh against a fake /opt/buzz (BUZZ_HOME),
# a fake run.sh, and the real render-env.sh with stub aws and docker.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

REPO_URL="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/relay"
REGISTRY="123456789012.dkr.ecr.us-east-1.amazonaws.com"
NAMES="BUZZ_RELAY_PRIVATE_KEY BUZZ_GIT_HOOK_HMAC_SECRET POSTGRES_PASSWORD REDIS_PASSWORD BUZZ_S3_ACCESS_KEY BUZZ_S3_SECRET_KEY"
HOME_DIR="$WORK/opt-buzz"

# new_home <scripts-dir>: a fake unpacked release. Its run.sh records how it ran.
new_home() {
  rm -rf "$HOME_DIR"
  mkdir -p "$HOME_DIR/deploy/compose" "$HOME_DIR/scripts/aws"
  cp "$REPO_ROOT/deploy/compose/.env.example" "$HOME_DIR/deploy/compose/.env.example"
  cp "$REPO_ROOT/scripts/aws/lib.sh" "$REPO_ROOT/scripts/aws/render-env.sh" "$HOME_DIR/scripts/aws/"
  cat > "$HOME_DIR/deploy/compose/run.sh" <<RUN
#!/usr/bin/env bash
echo "tls=\${BUZZ_COMPOSE_TLS:-} args=\$* cwd=\$(basename "\$PWD")" >> "$WORK/run.log"
RUN
  chmod +x "$HOME_DIR/deploy/compose/run.sh"
  : > "$WORK/run.log"
}

with_secrets() {
  local n
  reset_stubs
  for n in $NAMES; do
    printf '/buzz/%s\tvalue-of-%s\n' "$n" "$n" >> "$STUB_DIR/ssm-get-parameters-by-path.out"
  done
}

# run_remote <scripts-dir>: sets RC.
run_remote() {
  RC=0
  env BUZZ_HOME="$HOME_DIR" BUZZ_DOMAIN="$DOMAIN" RELAY_OWNER_PUBKEY="$PUBKEY" ECR_REPO="$REPO_URL" \
    bash "$1/remote-deploy.sh" abc1234 > "$WORK/out" 2> "$WORK/err" || RC=$?
}

mode_of() { ls -l "$1" | cut -c1-10; }

scenario_deploys() {
  new_home; with_secrets
  run_remote "$1"
  [ "$RC" -eq 0 ] || return 1
  local env_file="$HOME_DIR/deploy/compose/.env"
  [ "$(mode_of "$env_file")" = "-rw-------" ] || return 1
  grep -qxF "BUZZ_IMAGE=$REPO_URL:abc1234" "$env_file" || return 1
  grep -qxF "RELAY_URL=wss://$DOMAIN" "$env_file" || return 1
  grep -qxF "POSTGRES_PASSWORD=value-of-POSTGRES_PASSWORD" "$env_file" || return 1
  [ "$(grep -c . "$WORK/run.log")" -eq 1 ] || return 1
  grep -qxF "tls=true args=upgrade cwd=compose" "$WORK/run.log" || return 1
  ! compgen -G "$HOME_DIR/deploy/compose/.env.??????" > /dev/null || return 1
  ! grep -q value-of "$WORK/out" "$WORK/err"
}

scenario_logs_in_before_upgrade() {
  new_home; with_secrets
  run_remote "$1"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^aws ecr get-login-password')" -eq 1 ] || return 1
  [ "$(log_count '^docker login --username AWS --password-stdin '"$REGISTRY"'$')" -eq 1 ] || return 1
  grep -qF upgrade "$WORK/run.log"
}

scenario_failed_render_keeps_old_env() {
  new_home; reset_stubs
  echo "OLD=1" > "$HOME_DIR/deploy/compose/.env"
  run_remote "$1"
  [ "$RC" -ne 0 ] || return 1
  [ "$(cat "$HOME_DIR/deploy/compose/.env")" = "OLD=1" ] || return 1
  [ ! -s "$WORK/run.log" ] || return 1
  ! compgen -G "$HOME_DIR/deploy/compose/.env.??????" > /dev/null
}

scenario_requires_unpacked_release() {
  new_home; with_secrets
  rm -rf "$HOME_DIR/deploy"
  run_remote "$1"
  [ "$RC" -ne 0 ] && grep -qF "release is not unpacked" "$WORK/err"
}

scenario_requires_settings() {
  new_home; with_secrets
  RC=0
  env -u RELAY_OWNER_PUBKEY BUZZ_HOME="$HOME_DIR" BUZZ_DOMAIN="$DOMAIN" ECR_REPO="$REPO_URL" \
    bash "$1/remote-deploy.sh" abc1234 > "$WORK/out" 2> "$WORK/err" || RC=$?
  [ "$RC" -ne 0 ] && [ ! -s "$WORK/run.log" ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "writes a 0600 .env and runs run.sh upgrade with TLS" scenario_deploys "$R"
expect_ok "logs in to ECR before the upgrade" scenario_logs_in_before_upgrade "$R"
expect_ok "a failed render keeps the old .env and starts nothing" scenario_failed_render_keeps_old_env "$R"
expect_ok "an unpacked release is required" scenario_requires_unpacked_release "$R"
expect_ok "RELAY_OWNER_PUBKEY is required" scenario_requires_settings "$R"

expect_caught "TLS flag dropped" scenario_deploys remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/.\/run.sh upgrade/'
expect_caught "ECR login removed" scenario_logs_in_before_upgrade remote-deploy.sh \
  's/^aws ecr get-login-password \| docker login .*$/true/'
expect_caught "env written in place" scenario_failed_render_keeps_old_env remote-deploy.sh \
  's#> "\$tmp"$#> "$compose_dir/.env"#'
expect_caught "umask dropped" scenario_deploys remote-deploy.sh \
  's/^umask 077$/umask 022/; s/^chmod 600 .*$/true/; s/^tmp="\$\(mktemp .*$/tmp="$compose_dir\/.env.tmp"; touch "$tmp"/'

exit "$fail"
