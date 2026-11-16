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
  cp "$REPO_ROOT/scripts/aws/lib.sh" "$REPO_ROOT/scripts/aws/agent_spec.py" "$REPO_ROOT/scripts/aws/render-env.sh" \
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
  log_line "^docker compose -p buzz-agent-$1 -f $HOME_DIR/deploy/aws/compose.agents.yml --env-file $HOME_DIR/agents/$1.compose.env up -d --force-recreate\$"
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

# The first deploy has no agent keys or credentials, because registering an agent
# needs a running relay. The relay must come up first. A failed render then
# leaves it up, exits non-zero (so deploy.sh records no last-good image), and says
# what to run.
scenario_agent_render_failure_after_upgrade() {
  new_home; with_secrets
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -ne 0 ] || return 1
  [ "$(log_count '^run.sh upgrade$')" -eq 1 ] || return 1
  [ "$(log_count 'up -d')" -eq 0 ] || return 1
  grep -qF "the relay is up" "$WORK/err" || return 1
  grep -qF "make agents-secrets" "$WORK/err" || return 1
  grep -qF "make agents-register" "$WORK/err"
}

# `if ! render_agents` turns off set -e inside the function, so each step needs its
# own guard. A find that cannot clear the old agent files (AGENTS empty) must fail.
scenario_agent_cleanup_failure_fails_deploy() {
  new_home; with_secrets
  mkdir -p "$HOME_DIR/agents/old" "$WORK/failbin"
  echo "BUZZ_PRIVATE_KEY=secret" > "$HOME_DIR/agents/old.env"
  printf '#!/usr/bin/env bash\n[ "${1:-}" = "%s" ] && exit 1\nexec /usr/bin/find "$@"\n' "$HOME_DIR/agents" > "$WORK/failbin/find"
  chmod +x "$WORK/failbin/find"
  run_remote "$1" AGENTS= "PATH=$WORK/failbin:$PATH"
  [ "$RC" -ne 0 ] || return 1
  [ "$(log_count '^run.sh upgrade$')" -eq 1 ] || return 1
  grep -qF "the relay is up" "$WORK/err" && [ -e "$HOME_DIR/agents/old.env" ]
}

scenario_agents_render_after_upgrade() {
  new_home; with_secrets; with_agent_params a b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  run_remote "$1" "${AGENTS_ENV[@]}"
  [ "$RC" -eq 0 ] || return 1
  local upgrade render
  upgrade="$(log_line '^run.sh upgrade$')"
  render="$(log_line '^aws ssm get-parameters-by-path --path /buzz/agents/ ')"
  [ -n "$upgrade" ] && [ -n "$render" ] && [ "$render" -gt "$upgrade" ]
}

# A stale project whose name starts with a wanted agent's name plus a dash is not
# the wanted agent.
scenario_stale_project_with_wanted_prefix() {
  new_home; with_secrets; with_agent_params b
  echo "container-id" > "$STUB_DIR/docker-compose-ps.out"
  printf 'buzz-agent-b\nbuzz-agent-b-c\n' > "$STUB_DIR/docker-ps.out"
  run_remote "$1" AGENTS=b:claude "AGENTS_ECR_REPO=$AGENTS_REPO"
  [ "$RC" -eq 0 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-b-c down$')" -eq 1 ] || return 1
  [ "$(log_count '^docker compose -p buzz-agent-b down$')" -eq 0 ]
}

# The Beehive host supervisor. The stub docker answers create, run and ps from files, and the
# stub systemctl logs. docker cp is a stub too, so the binary it would copy is put in place.

HIVE_IMAGE="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/hive:abc1234"
SUPERVISOR_PUBKEY="$(printf '1%.0s' $(seq 1 64))"
HIVE_PUBKEY="$(printf '2%.0s' $(seq 1 64))"
ETC_DIR="$WORK/etc"

# with_supervisor: everything a deploy needs to install the supervisor.
with_supervisor() {
  new_home; with_secrets
  rm -rf "$ETC_DIR"
  mkdir -p "$ETC_DIR"
  echo "POSTGRES_PASSWORD=x" > "$HOME_DIR/.env.hive"
  mkdir -p "$HOME_DIR/bin"
  printf '#!/bin/sh\n' > "$HOME_DIR/bin/buzz-hive-supervisor"
  chmod +x "$HOME_DIR/bin/buzz-hive-supervisor"
  echo "cid-1" > "$STUB_DIR/docker-create.out"
  printf 'BUZZ_HIVE_SUPERVISOR_PUBKEY=%s\nBUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY=%s\n' "$SUPERVISOR_PUBKEY" "$HIVE_PUBKEY" > "$STUB_DIR/docker-run.out"
  echo "hive-container" > "$STUB_DIR/docker-ps.out"
}

run_supervised() {
  local dir="$1"
  shift
  run_remote "$dir" "BUZZ_HIVE_IMAGE=$HIVE_IMAGE" "BUZZ_ETC_DIR=$ETC_DIR" ${@+"$@"}
}

scenario_supervisor_installed_and_agents_applied() {
  with_supervisor
  run_supervised "$1"
  [ "$RC" -eq 0 ] || return 1
  local env_file="$ETC_DIR/buzz-hive-supervisor.env"
  [ "$(mode_of "$env_file")" = "-rw-------" ] || return 1
  # The keys the image printed, then the paths the unit reads, and nothing else.
  [ "$(cat "$env_file")" = "$(printf '%s\n' \
    "BUZZ_HIVE_SUPERVISOR_PUBKEY=$SUPERVISOR_PUBKEY" \
    "BUZZ_HIVE_SUPERVISOR_HIVE_PUBKEY=$HIVE_PUBKEY" \
    "BUZZ_HIVE_SUPERVISOR_SOCKET=/run/buzz-hive/supervisor.sock" \
    "BUZZ_HIVE_SUPERVISOR_SOCKET_UID=1000" \
    "BUZZ_HIVE_SUPERVISOR_AGENTS_SRC=$HOME_DIR/deploy/aws/agents" \
    "BUZZ_HIVE_SUPERVISOR_RENDER_DIR=$HOME_DIR/agents" \
    "BUZZ_HIVE_SUPERVISOR_SECRETS=ssm")" ] || return 1
  cmp -s "$HOME_DIR/deploy/aws/buzz-hive-supervisor.service" "$ETC_DIR/systemd/system/buzz-hive-supervisor.service" || return 1
  # The image the service runs supplies the binary and the keys.
  [ "$(log_count "^docker create $HIVE_IMAGE\$")" -eq 1 ] || return 1
  [ "$(log_count "^docker cp cid-1:/usr/local/bin/buzz-hive-supervisor $HOME_DIR/bin/buzz-hive-supervisor\$")" -eq 1 ] || return 1
  [ "$(log_count "^docker rm cid-1\$")" -eq 1 ] || return 1
  [ "$(log_count "^docker run --rm --env-file $HOME_DIR/.env.hive $HIVE_IMAGE keys\$")" -eq 1 ] || return 1
  # Reload, enable, restart, in that order, and only then the apply.
  local reload enable restart apply prev=0 n
  reload="$(log_line '^systemctl daemon-reload$')"
  enable="$(log_line '^systemctl enable buzz-hive-supervisor.service$')"
  restart="$(log_line '^systemctl restart buzz-hive-supervisor.service$')"
  apply="$(log_line '^docker exec hive-container /usr/local/bin/buzz-hive apply-agents$')"
  for n in "$reload" "$enable" "$restart" "$apply"; do
    [ -n "$n" ] && [ "$n" -gt "$prev" ] || return 1
    prev="$n"
  done
  # No secret and no key of the deploy is in the file or the output.
  ! grep -q "value-of" "$env_file" "$WORK/out" "$WORK/err"
}

scenario_supervisor_skipped_without_its_inputs() {
  local what
  for what in no-image no-env-file no-unit; do
    with_supervisor
    case "$what" in
      no-image) run_remote "$1" "BUZZ_ETC_DIR=$ETC_DIR" ;;
      no-env-file) rm "$HOME_DIR/.env.hive"; run_supervised "$1" ;;
      no-unit) rm "$HOME_DIR/deploy/aws/buzz-hive-supervisor.service"; run_supervised "$1" ;;
    esac
    [ "$RC" -eq 0 ] || return 1
    grep -qF "supervisor: skipped" "$WORK/out" || return 1
    [ "$(log_count '^docker (create|run|exec) ')" -eq 0 ] || return 1
    [ "$(log_count '^systemctl ')" -eq 0 ] || return 1
    [ ! -e "$ETC_DIR/buzz-hive-supervisor.env" ] || return 1
  done
}

scenario_supervisor_skipped_for_an_image_name_that_is_not_one() {
  with_supervisor
  run_remote "$1" "BUZZ_HIVE_IMAGE=repo/hive;touch pwned" "BUZZ_ETC_DIR=$ETC_DIR"
  [ "$RC" -eq 0 ] || return 1
  grep -qF "BUZZ_HIVE_IMAGE is not an image name" "$WORK/err" || return 1
  [ "$(log_count '^docker (create|run|exec) ')" -eq 0 ] || return 1
  [ ! -e "$ETC_DIR/buzz-hive-supervisor.env" ] && [ ! -e pwned ]
}

scenario_supervisor_trouble_warns_and_never_fails_the_deploy() {
  # The image has no supervisor binary.
  with_supervisor
  rm "$HOME_DIR/bin/buzz-hive-supervisor"
  run_supervised "$1"
  [ "$RC" -eq 0 ] && grep -qF "the image has no buzz-hive-supervisor" "$WORK/err" || return 1
  [ ! -e "$ETC_DIR/buzz-hive-supervisor.env" ] || return 1
  # The keys cannot be read.
  with_supervisor
  echo 1 > "$STUB_DIR/docker-run.rc"
  run_supervised "$1"
  [ "$RC" -eq 0 ] && grep -qF "cannot read the Beehive keys" "$WORK/err" || return 1
  [ ! -e "$ETC_DIR/buzz-hive-supervisor.env" ] || return 1
  # The unit does not start: nothing is applied.
  with_supervisor
  echo 1 > "$STUB_DIR/systemctl-restart.rc"
  run_supervised "$1"
  [ "$RC" -eq 0 ] && grep -qF "the supervisor did not start" "$WORK/err" || return 1
  [ "$(log_count 'apply-agents')" -eq 0 ] || return 1
  # The hive service is not running yet: installed, nothing applied.
  with_supervisor
  : > "$STUB_DIR/docker-ps.out"
  run_supervised "$1"
  [ "$RC" -eq 0 ] && grep -qF "the hive service is not running" "$WORK/out" || return 1
  [ "$(log_count 'apply-agents')" -eq 0 ] || return 1
  # Some agent is not applied: a warning, and the deploy still succeeds.
  with_supervisor
  echo 1 > "$STUB_DIR/docker-exec.rc"
  run_supervised "$1"
  [ "$RC" -eq 0 ] && grep -qF "not every agent was given its tool servers" "$WORK/err"
}

scenario_the_unit_runs_what_the_deploy_installs() {
  with_supervisor
  run_supervised "$1"
  [ "$RC" -eq 0 ] || return 1
  local unit="$ETC_DIR/systemd/system/buzz-hive-supervisor.service"
  # The unit reads the file the deploy wrote, under the folder the deploy defaults to, and runs
  # the binary the deploy copied, under the home it defaults to.
  grep -qF 'BUZZ_ETC_DIR="${BUZZ_ETC_DIR:-/etc}"' "$1/remote-deploy.sh" || return 1
  grep -qF 'BUZZ_HOME="${BUZZ_HOME:-/opt/buzz}"' "$1/remote-deploy.sh" || return 1
  grep -qxF "EnvironmentFile=/etc/$(basename "$ETC_DIR"/buzz-hive-supervisor.env)" "$unit" || return 1
  [ -x "$HOME_DIR/bin/buzz-hive-supervisor" ] || return 1
  grep -qxF "ExecStart=/opt/buzz/bin/$(basename "$HOME_DIR/bin/buzz-hive-supervisor")" "$unit" || return 1
  # And it may write where the deploy renders the agents.
  grep -qF "ReadWritePaths=/opt/buzz/agents " "$unit"
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
expect_ok "a failed agent render leaves the relay up, fails the deploy, and says what to run" scenario_agent_render_failure_after_upgrade "$R"
expect_ok "a failed cleanup of old agent files fails the deploy with the relay-is-up message" scenario_agent_cleanup_failure_fails_deploy "$R"
expect_ok "agent files are rendered after the relay upgrade" scenario_agents_render_after_upgrade "$R"
expect_ok "a stale project named like a wanted agent plus a suffix is stopped" scenario_stale_project_with_wanted_prefix "$R"

expect_ok "the supervisor is installed from the hive image and every agent applied" scenario_supervisor_installed_and_agents_applied "$R"
expect_ok "the supervisor is skipped without its image, env file or unit" scenario_supervisor_skipped_without_its_inputs "$R"
expect_ok "the supervisor is skipped for a name that is not an image" scenario_supervisor_skipped_for_an_image_name_that_is_not_one "$R"
expect_ok "supervisor trouble warns and never fails the deploy" scenario_supervisor_trouble_warns_and_never_fails_the_deploy "$R"
expect_ok "the unit runs the binary and reads the file the deploy installs" scenario_the_unit_runs_what_the_deploy_installs "$R"

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
  's/^    if agent_compose "\$name" up -d --force-recreate; then$/    if agent_compose "$name" up -d --force-recreate || true; then/'
expect_caught "agents not recreated for a changed secret" scenario_agents_start_after_upgrade remote-deploy.sh \
  's/ up -d --force-recreate; then$/ up -d; then/'
expect_caught "stale projects stopped with their volumes" scenario_stale_projects_stopped_without_volumes remote-deploy.sh \
  's/docker compose -p "\$project" down/docker compose -p "$project" down -v/'
expect_caught "stale cleanup skipped when AGENTS is empty" scenario_stale_cleanup_runs_with_no_agents remote-deploy.sh \
  's/^stop_stale_agents$/[ -z "$AGENTS" ] || stop_stale_agents/'
expect_caught "stale env files kept when AGENTS is empty" scenario_stale_cleanup_runs_with_no_agents remote-deploy.sh \
  's/^      find "\$agents_dir" .* -exec rm -rf \{\} \+ || return 1$/      true/'
expect_caught "running agent stopped as stale" scenario_stale_projects_stopped_without_volumes remote-deploy.sh \
  's/^    case "\$wanted" in$/    case "" in/'
expect_caught "release files not required" scenario_agents_need_the_release_files remote-deploy.sh \
  's/^  \[ -f "\$agents_compose" \] && \[ -f .*$/  true; true \\/'
expect_caught "agents rendered before the upgrade" scenario_agent_render_failure_after_upgrade remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/render_agents; BUZZ_COMPOSE_TLS=true .\/run.sh upgrade/; s/^if ! render_agents; then$/if false; then/'
expect_caught "agents rendered before the upgrade (order)" scenario_agents_render_after_upgrade remote-deploy.sh \
  's/^BUZZ_COMPOSE_TLS=true \.\/run\.sh upgrade$/render_agents; BUZZ_COMPOSE_TLS=true .\/run.sh upgrade/; s/^if ! render_agents; then$/if false; then/'
expect_caught "render failure does not say what to run" scenario_agent_render_failure_after_upgrade remote-deploy.sh \
  's/Run make agents-secrets and make agents-register, then make deploy again\./Fix it./'
expect_caught "render failure exits zero" scenario_agent_render_failure_after_upgrade remote-deploy.sh \
  's/^  exit 1$/  exit 0/'
expect_caught "stale project matched by name prefix" scenario_stale_project_with_wanted_prefix remote-deploy.sh \
  's/^    name="\$\{project#buzz-agent-\}"$/    name="${project#buzz-agent-}"; name="${name%%-*}"/'

expect_caught "rendered agents image uses another tag" scenario_agents_start_after_upgrade remote-deploy.sh \
  's/"\$AGENTS_ECR_REPO:\$tag"/"$AGENTS_ECR_REPO:latest"/'
expect_caught "rendered agents image uses the relay repository" scenario_agents_start_after_upgrade remote-deploy.sh \
  's/"\$AGENTS_ECR_REPO:\$tag"/"$ECR_REPO:$tag"/'

expect_caught "failed cleanup of old agent files ignored" scenario_agent_cleanup_failure_fails_deploy remote-deploy.sh \
  's/-exec rm -rf \{\} \+ \|\| return 1$/-exec rm -rf {} +/'

expect_caught "supervisor unit not reloaded before it starts" scenario_supervisor_installed_and_agents_applied remote-deploy.sh \
  's/^  systemctl daemon-reload$/  true/'
expect_caught "supervisor keys not read from the image" scenario_supervisor_installed_and_agents_applied remote-deploy.sh \
  's/^  keys="\$\(docker run --rm --env-file "\$env_file" "\$image" keys\)".*$/  keys="BUZZ_HIVE_SUPERVISOR_PUBKEY=x"/'
expect_caught "image name not checked" scenario_supervisor_skipped_for_an_image_name_that_is_not_one remote-deploy.sh \
  's/^  \[\[ "\$image" =~ .*$/  true/'
expect_caught "a supervisor that did not start fails the deploy" scenario_supervisor_trouble_warns_and_never_fails_the_deploy remote-deploy.sh \
  's/ \|\| \{ echo "warning: the supervisor did not start" >&2; return 0; \}//'
expect_caught "skip conditions dropped" scenario_supervisor_skipped_without_its_inputs remote-deploy.sh \
  's/^  if \[ -z "\$image" \] \|\| .*$/  if false; then/'

exit "$fail"
