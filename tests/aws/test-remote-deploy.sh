#!/usr/bin/env bash
# Tests for scripts/aws/remote-deploy.sh against a fake /opt/buzz (BUZZ_HOME),
# a fake run.sh, and the real render-env.sh and render-agents-env.sh with stub
# aws and docker.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

REPO_URL="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/relay"
REGISTRY="123456789012.dkr.ecr.us-east-1.amazonaws.com"
AGENTS_REPO="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/agents"
NAMES="BUZZ_RELAY_PRIVATE_KEY BUZZ_GIT_HOOK_HMAC_SECRET POSTGRES_PASSWORD REDIS_PASSWORD BUZZ_S3_ACCESS_KEY BUZZ_S3_SECRET_KEY"
HOME_DIR="$WORK/opt-buzz"

# new_home <scripts-dir>: a fake unpacked release. Its run.sh records how it ran.
new_home() {
  rm -rf "$HOME_DIR"
  mkdir -p "$HOME_DIR/deploy/compose" "$HOME_DIR/scripts/aws"
  cp -R "$REPO_ROOT/deploy/aws" "$HOME_DIR/deploy/aws"
  cp "$REPO_ROOT/deploy/compose/.env.example" "$HOME_DIR/deploy/compose/.env.example"
  cp "$REPO_ROOT/scripts/aws/lib.sh" "$REPO_ROOT/scripts/aws/render-env.sh" \
    "$REPO_ROOT/scripts/aws/render-agents-env.sh" "$HOME_DIR/scripts/aws/"
  # RUN_RC makes the fake upgrade fail, like a relay that never turns healthy.
  cat > "$HOME_DIR/deploy/compose/run.sh" <<RUN
#!/usr/bin/env bash
echo "tls=\${BUZZ_COMPOSE_TLS:-} args=\$* cwd=\$(basename "\$PWD")" >> "$WORK/run.log"
echo "run.sh \$*" >> "\$STUB_LOG"
exit "\${RUN_RC:-0}"
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

# with_agent_params <names...>: adds the agent keys and one credential per
# runtime to the stubbed SSM answer.
with_agent_params() {
  local n
  {
    printf '/buzz/agents/CLAUDE_CODE_OAUTH_TOKEN\toauth-token-AAA\n'
    printf '/buzz/agents/OPENAI_API_KEY\topenai-key-CCC\n'
    for n in "$@"; do
      printf '/buzz/agents/%s/BUZZ_PRIVATE_KEY\t%s\n' "$n" "$(printf '%s' "$n" | od -An -tx1 | tr -d ' \n' | awk '{ while (length($0) < 64) $0 = $0 "0"; print substr($0, 1, 64) }')"
    done
  } >> "$STUB_DIR/ssm-get-parameters-by-path.out"
}

# run_remote <scripts-dir> [VAR=value ...]: sets RC.
run_remote() {
  local dir="$1"
  shift
  RC=0
  env BUZZ_HOME="$HOME_DIR" BUZZ_DOMAIN="$DOMAIN" RELAY_OWNER_PUBKEY="$PUBKEY" ECR_REPO="$REPO_URL" \
    AGENT_SETTLE_SECONDS=0 ${@+"$@"} \
    bash "$dir/remote-deploy.sh" abc1234 > "$WORK/out" 2> "$WORK/err" || RC=$?
}

# up_line <name>: the log line of the Compose start for that agent, empty if none.
up_line() {
  log_line "^docker compose -p buzz-agent-$1 -f $HOME_DIR/deploy/aws/compose.agents.yml --env-file $HOME_DIR/agents/$1.compose.env up -d\$"
}

mode_of() { ls -ld "$1" | cut -c1-10; }

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

# Agents. The fake run.sh and the stub docker share STUB_LOG, so the log shows the order.

AGENTS_ENV=("AGENTS=a:claude,b:claude" "AGENTS_ECR_REPO=$AGENTS_REPO")

scenario_agents_start_after_upgrade() {
  new_home; with_secrets; with_agent_params a b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -eq 0 ] || return 1
  local upgrade ua ub n prev=0
  upgrade="$(log_line '^run.sh upgrade$')"
  ua="$(up_line a)"
  ub="$(up_line b)"
  for n in "$upgrade" "$ua" "$ub"; do
    [ -n "$n" ] && [ "$n" -gt "$prev" ] || return 1
    prev="$n"
  done
  # Rendered output: image, mode, and the list of names.
  grep -qxF "AGENTS_IMAGE=$AGENTS_REPO:abc1234" "$HOME_DIR/agents/a.compose.env" || return 1
  grep -qxF "AGENTS_IMAGE=$AGENTS_REPO:abc1234" "$HOME_DIR/agents/b.compose.env" || return 1
  [ "$(mode_of "$HOME_DIR/agents")" = "drwx------" ] || return 1
  [ "$(cat "$HOME_DIR/agents/agents.list")" = "$(printf 'a\nb')" ]
}

scenario_agents_log_in_before_pull() {
  new_home; with_secrets; with_agent_params a b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  local login
  login="$(log_line '^docker login ')"
  [ "$RC" -eq 0 ] && [ -n "$login" ] && [ "$login" -lt "$(up_line a)" ]
}

scenario_agents_wait_for_relay() {
  new_home; with_secrets; with_agent_params a b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}" RUN_RC=1
  [ "$RC" -ne 0 ] && [ "$(log_count 'docker compose .* up ')" -eq 0 ]
}

scenario_agent_failure_fails_deploy_with_logs() {
  new_home; with_secrets; with_agent_params a b
  : > "$STUB_DIR/docker-compose-ps.out"
  echo "agent-boom: cannot connect to relay" > "$STUB_DIR/docker-compose-logs.out"
  printf 'buzz-agent-old\n' > "$STUB_DIR/docker-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -ne 0 ] || return 1
  grep -qF "agent a did not stay running" "$WORK/err" || return 1
  grep -qF "agent b did not stay running" "$WORK/err" || return 1
  [ "$(grep -c 'agent-boom: cannot connect to relay' "$WORK/err")" -eq 2 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-a .* logs ')" -eq 1 ] || return 1
  # The second agent is still tried, and the stale project is still stopped.
  [ -n "$(up_line b)" ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-old down$')" -eq 1 ]
}

scenario_agent_up_failure_fails_deploy() {
  new_home; with_secrets; with_agent_params a b
  echo 1 > "$STUB_DIR/docker-compose-up.rc"
  echo "agent-boom: pull access denied" > "$STUB_DIR/docker-compose-logs.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -ne 0 ] && grep -qF "could not start agent a" "$WORK/err" \
    && grep -qF "agent-boom: pull access denied" "$WORK/err"
}

scenario_stale_projects_stopped_without_volumes() {
  new_home; with_secrets; with_agent_params a b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  printf 'buzz-agent-a\nbuzz-agent-old\nbuzz-agent-old\nbuzz-agent-b\nbuzz-compose\n' > "$STUB_DIR/docker-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-old down$')" -eq 1 ] || return 1
  [ "$(log_count 'down')" -eq 1 ] || return 1
  ! grep -qE -- ' (-v|--volumes)( |$)' "$STUB_LOG"
}

scenario_stale_cleanup_runs_with_no_agents() {
  new_home; with_secrets
  mkdir -p "$HOME_DIR/agents/old/config"
  echo "BUZZ_PRIVATE_KEY=secret" > "$HOME_DIR/agents/old.env"
  printf 'buzz-agent-old\nbuzz-agent-older\n' > "$STUB_DIR/docker-ps.out"
  run_remote "$1" AGENTS=
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-old down$')" -eq 1 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-older down$')" -eq 1 ] || return 1
  [ "$(log_count 'up -d')" -eq 0 ] || return 1
  # The old env files held secrets and are gone.
  [ ! -e "$HOME_DIR/agents/old.env" ] && [ ! -e "$HOME_DIR/agents/old" ]
}

scenario_no_agents_without_cleanup_target() {
  new_home; with_secrets
  run_remote "$1"
  [ "$RC" -eq 0 ] && [ "$(log_count 'docker compose')" -eq 0 ] && [ ! -e "$HOME_DIR/agents" ]
}

scenario_agents_need_the_release_files() {
  new_home; with_secrets; with_agent_params a b
  rm -rf "$HOME_DIR/deploy/aws"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -ne 0 ] && grep -qF "release has no deploy/aws" "$WORK/err" && [ ! -s "$WORK/run.log" ]
}

scenario_agents_need_a_repository() {
  new_home; with_secrets; with_agent_params a b
  run_remote "$1" AGENTS=a:claude,b:claude
  [ "$RC" -ne 0 ] && grep -qF "AGENTS_ECR_REPO must be set" "$WORK/err" && [ ! -s "$WORK/run.log" ]
}

# A missing credential fails the render, so the relay is not touched.
scenario_agent_render_failure_stops_before_upgrade() {
  new_home; with_secrets
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -ne 0 ] && [ ! -s "$WORK/run.log" ] && [ "$(log_count 'up -d')" -eq 0 ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "writes a 0600 .env and runs run.sh upgrade with TLS" scenario_deploys "$R"
expect_ok "logs in to ECR before the upgrade" scenario_logs_in_before_upgrade "$R"
expect_ok "a failed render keeps the old .env and starts nothing" scenario_failed_render_keeps_old_env "$R"
expect_ok "an unpacked release is required" scenario_requires_unpacked_release "$R"
expect_ok "RELAY_OWNER_PUBKEY is required" scenario_requires_settings "$R"

expect_ok "agents start after the upgrade, from the rendered files" scenario_agents_start_after_upgrade "$R"
expect_ok "agents are pulled after the ECR login" scenario_agents_log_in_before_pull "$R"
expect_ok "a failed relay upgrade starts no agent" scenario_agents_wait_for_relay "$R"
expect_ok "a failed agent fails the deploy and prints its logs" scenario_agent_failure_fails_deploy_with_logs "$R"
expect_ok "a failed agent start fails the deploy and prints its logs" scenario_agent_up_failure_fails_deploy "$R"
expect_ok "stale agent projects are stopped without removing volumes" scenario_stale_projects_stopped_without_volumes "$R"
expect_ok "stale agent projects are stopped when AGENTS is empty" scenario_stale_cleanup_runs_with_no_agents "$R"
expect_ok "with AGENTS empty and no agent files, nothing agent-related runs" scenario_no_agents_without_cleanup_target "$R"
expect_ok "agents need deploy/aws in the release" scenario_agents_need_the_release_files "$R"
expect_ok "agents need AGENTS_ECR_REPO" scenario_agents_need_a_repository "$R"
expect_ok "a failed agent render stops before the relay changes" scenario_agent_render_failure_stops_before_upgrade "$R"

expect_caught "TLS flag dropped" scenario_deploys remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/.\/run.sh upgrade/'
expect_caught "ECR login removed" scenario_logs_in_before_upgrade remote-deploy.sh \
  's/^aws ecr get-login-password \| docker login .*$/true/'
expect_caught "env written in place" scenario_failed_render_keeps_old_env remote-deploy.sh \
  's#> "\$tmp"$#> "$compose_dir/.env"#'
expect_caught "umask dropped" scenario_deploys remote-deploy.sh \
  's/^umask 077$/umask 022/; s/^chmod 600 .*$/true/; s/^tmp="\$\(mktemp .*$/tmp="$compose_dir\/.env.tmp"; touch "$tmp"/'

expect_caught "agents started before the upgrade" scenario_agents_start_after_upgrade remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/start_agents; BUZZ_COMPOSE_TLS=true .\/run.sh upgrade/; s/^start_agents$/true/'
expect_caught "failed upgrade does not stop the agents" scenario_agents_wait_for_relay remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/BUZZ_COMPOSE_TLS=true .\/run.sh upgrade || true/'
expect_caught "agent failure ignored" scenario_agent_failure_fails_deploy_with_logs remote-deploy.sh \
  's/^\[ "\$failed" -eq 0 \] \|\| exit 1$/true/'
expect_caught "agent logs not printed" scenario_agent_failure_fails_deploy_with_logs remote-deploy.sh \
  's/^  agent_compose "\$1" logs .*$/  true/'
expect_caught "up failure ignored" scenario_agent_up_failure_fails_deploy remote-deploy.sh \
  's/^    if agent_compose "\$name" up -d; then$/    if agent_compose "$name" up -d || true; then/'
expect_caught "stale projects stopped with their volumes" scenario_stale_projects_stopped_without_volumes remote-deploy.sh \
  's/docker compose -p "\$project" down/docker compose -p "$project" down -v/'
expect_caught "stale cleanup skipped when AGENTS is empty" scenario_stale_cleanup_runs_with_no_agents remote-deploy.sh \
  's/^stop_stale_agents$/[ -z "$AGENTS" ] || stop_stale_agents/'
expect_caught "stale env files kept when AGENTS is empty" scenario_stale_cleanup_runs_with_no_agents remote-deploy.sh \
  's/^      find "\$agents_dir" .* -exec rm -rf \{\} \+$/      true/'
expect_caught "running agent stopped as stale" scenario_stale_projects_stopped_without_volumes remote-deploy.sh \
  's/^    case "\$wanted" in$/    case "" in/'
expect_caught "release files not required" scenario_agents_need_the_release_files remote-deploy.sh \
  's/^  \[ -f "\$agents_compose" \] && \[ -f .*$/  true; true \\/'
expect_caught "render after the upgrade" scenario_agent_render_failure_stops_before_upgrade remote-deploy.sh \
  's/^render_agents$/true/; s/^start_agents$/render_agents; start_agents/'

exit "$fail"
