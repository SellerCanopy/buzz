#!/usr/bin/env bash
# Tests for scripts/aws/render-agents-env.sh with a stub aws that returns the
# agent keys and credentials.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"

IMAGE="123456789012.dkr.ecr.us-east-1.amazonaws.com/buzz/agents:abc1234"
OUT="$WORK/out"
CFG="$WORK/cfg"
EMPTY_CONFIG='{"mcpServers": {}}'
OAUTH="oauth-token-AAA111"
APIKEY="api-key-BBB222"
OPENAI="openai-key-CCC333"
ALLOW="$(printf 'd%.0s' $(seq 1 64)),$(printf 'e%.0s' $(seq 1 64))"

# key_of <name>: a distinct 64-hex key per agent name.
key_of() { printf '%s' "$1" | od -An -tx1 | tr -d ' \n' | cut -c1-64 | awk '{ while (length($0) < 64) $0 = $0 "0"; print }'; }

# mode_of <path>: octal permission bits.
mode_of() { stat -c %a "$1" 2> /dev/null || stat -f %Lp "$1"; }

# params <agents...>: the stub answer for get-parameters-by-path. Set WITH_OAUTH=0
# to drop the OAuth token and WITH_APIKEY=0 to drop the API key.
params() {
  local n
  {
    [ "${WITH_OAUTH:-1}" = 1 ] && printf '/buzz/agents/CLAUDE_CODE_OAUTH_TOKEN\t%s\n' "$OAUTH"
    [ "${WITH_APIKEY:-1}" = 1 ] && printf '/buzz/agents/ANTHROPIC_API_KEY\t%s\n' "$APIKEY"
    printf '/buzz/agents/OPENAI_API_KEY\t%s\n' "$OPENAI"
    for n in "$@"; do printf '/buzz/agents/%s/BUZZ_PRIVATE_KEY\t%s\n' "$n" "$(key_of "$n")"; done
  } > "$STUB_DIR/ssm-get-parameters-by-path.out"
}

# setup <agents...>: fresh stubs, fresh output directory, and a config source in
# which only agent "a" has a file.
setup() {
  reset_stubs
  WITH_OAUTH=1 WITH_APIKEY=1
  params "$@"
  rm -rf "$OUT" "$CFG"
  mkdir -p "$CFG/a"
  printf '{\n  "mcpServers": {"probe-a": {"command": "echo", "args": ["a"]}}\n}\n' > "$CFG/a/mcp.json"
}

# render <scripts-dir> <agents> [VAR=value ...]: sets RC.
render() {
  local dir="$1" agents="$2"
  shift 2
  RC=0
  env BUZZ_DOMAIN="$DOMAIN" RELAY_OWNER_PUBKEY="$PUBKEY" AGENTS="$agents" ${@+"$@"} \
    bash "$dir/render-agents-env.sh" "$OUT" "$IMAGE" "$CFG" > "$WORK/stdout" 2> "$WORK/err" || RC=$?
}

# has_line <file> <line>: the file holds exactly this line.
has_line() { grep -qxF -- "$2" "$1"; }

scenario_two_claude_agents() {
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  local n
  for n in a b; do
    has_line "$OUT/$n.env" "BUZZ_ACP_AGENT_COMMAND=claude-agent-acp" || return 1
    has_line "$OUT/$n.env" "BUZZ_RELAY_URL=wss://$DOMAIN" || return 1
    has_line "$OUT/$n.env" "BUZZ_ACP_AGENT_OWNER=$PUBKEY" || return 1
    has_line "$OUT/$n.env" "BUZZ_ACP_DISPLAY_NAME=$n" || return 1
    has_line "$OUT/$n.env" "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp" || return 1
    has_line "$OUT/$n.env" "BUZZ_ACP_RESPOND_TO=owner-only" || return 1
    has_line "$OUT/$n.env" "BUZZ_PRIVATE_KEY=$(key_of "$n")" || return 1
    has_line "$OUT/$n.env" "CLAUDE_CODE_OAUTH_TOKEN=$OAUTH" || return 1
  done
  [ "$(key_of a)" != "$(key_of b)" ]
}

scenario_oauth_wins_and_never_both() {
  setup a
  render "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "CLAUDE_CODE_OAUTH_TOKEN=$OAUTH" || return 1
  ! grep -q ANTHROPIC_API_KEY "$OUT/a.env" || return 1
  ! grep -qF "$APIKEY" "$OUT/a.env"
}

scenario_api_key_when_no_oauth() {
  setup a
  WITH_OAUTH=0 params a
  render "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "ANTHROPIC_API_KEY=$APIKEY" || return 1
  ! grep -q CLAUDE_CODE_OAUTH_TOKEN "$OUT/a.env"
}

scenario_codex_agent() {
  setup c
  render "$1" c:codex
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/c.env" "BUZZ_ACP_AGENT_COMMAND=codex-acp" || return 1
  has_line "$OUT/c.env" "OPENAI_API_KEY=$OPENAI" || return 1
  ! grep -qE 'ANTHROPIC|CLAUDE' "$OUT/c.env"
}

scenario_bare_runtime_names() {
  setup claude codex
  render "$1" claude,codex
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/claude.env" "BUZZ_ACP_DISPLAY_NAME=claude" || return 1
  has_line "$OUT/codex.env" "BUZZ_ACP_AGENT_COMMAND=codex-acp"
}

scenario_mcp_config_staging() {
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  cmp -s "$CFG/a/mcp.json" "$OUT/a/config/mcp.json" || return 1
  [ "$(cat "$OUT/b/config/mcp.json")" = "$EMPTY_CONFIG" ] || return 1
  [ ! -e "$CFG/b/mcp.json" ] || return 1
  # a's config is not b's, and b's is not a's.
  ! cmp -s "$OUT/a/config/mcp.json" "$OUT/b/config/mcp.json"
}

# The container runs as uid 1001, so the mounted directory and file must be
# readable by others whatever umask the caller has.
scenario_config_modes_under_restrictive_umask() {
  setup a b
  RC=0
  (
    umask 077
    env BUZZ_DOMAIN="$DOMAIN" RELAY_OWNER_PUBKEY="$PUBKEY" AGENTS=a:claude,b:claude \
      bash "$1/render-agents-env.sh" "$OUT" "$IMAGE" "$CFG" > "$WORK/stdout" 2> "$WORK/err"
  ) || RC=$?
  [ "$RC" -eq 0 ] || return 1
  local n
  for n in a b; do
    [ "$(mode_of "$OUT/$n/config")" = 755 ] || return 1
    [ "$(mode_of "$OUT/$n/config/mcp.json")" = 644 ] || return 1
    # Secrets stay private to root.
    [ "$(mode_of "$OUT/$n.env")" = 600 ] || return 1
  done
}

scenario_compose_env_file() {
  setup a
  render "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  local abs
  abs="$(cd "$OUT" && pwd)"
  [ "$(sort "$OUT/a.compose.env")" = "$(printf '%s\n' \
    "AGENTS_IMAGE=$IMAGE" "AGENT_NAME=a" "BUZZ_DOMAIN=$DOMAIN" \
    "AGENT_ENV_FILE=$abs/a.env" "AGENT_CONFIG_DIR=$abs/a/config" | sort)" ]
}

# With real Docker, the files must satisfy the real compose file.
scenario_compose_accepts_rendered_files() {
  local real_path="${PATH#"$REPO_ROOT/tests/aws/stubs:"}"
  if ! PATH="$real_path" docker compose version > /dev/null 2>&1; then
    echo "skip - docker compose is not available" >&2
    return 0
  fi
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  local n
  for n in a b; do
    PATH="$real_path" docker compose -p "buzz-agent-$n" -f "$REPO_ROOT/deploy/aws/compose.agents.yml" \
      --env-file "$OUT/$n.compose.env" config > /dev/null 2>&1 || return 1
  done
}

scenario_agents_list() {
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] && [ "$(cat "$OUT/agents.list")" = "$(printf 'a\nb')" ]
}

scenario_no_presence_or_inactivity_variables() {
  setup a b c
  render "$1" a:claude,b:claude,c:codex BUZZ_ACP_NO_PRESENCE=1 BUZZ_ACP_EXIT_AFTER_INACTIVITY=60
  [ "$RC" -eq 0 ] || return 1
  ! grep -rqE 'BUZZ_ACP_NO_PRESENCE|BUZZ_ACP_EXIT_AFTER_INACTIVITY' "$OUT"
}

scenario_respond_to_modes() {
  setup a
  render "$1" a:claude AGENT_RESPOND_TO=allowlist AGENT_RESPOND_TO_ALLOWLIST="$ALLOW"
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_RESPOND_TO=allowlist" || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_RESPOND_TO_ALLOWLIST=$ALLOW" || return 1
  render "$1" a:claude AGENT_RESPOND_TO=anyone AGENT_RESPOND_TO_ALLOWLIST="$ALLOW"
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_RESPOND_TO=anyone" || return 1
  ! grep -q ALLOWLIST "$OUT/a.env"
}

# fails_without_writing <name>: exit 1, no agent files, and no secret on stderr.
fails_without_writing() {
  [ "$RC" -eq 1 ] || return 1
  [ ! -e "$OUT/a.env" ] && [ ! -e "$OUT/b.env" ] && [ ! -e "$OUT/agents.list" ] || return 1
  ! grep -qE "$OAUTH|$APIKEY|$OPENAI|$(key_of a)|$(key_of b)" "$WORK/err" "$WORK/stdout"
}

scenario_empty_key_value_exits_1() {
  setup a b
  printf '/buzz/agents/a/BUZZ_PRIVATE_KEY\t\n' >> "$STUB_DIR/ssm-get-parameters-by-path.out"
  grep -v '^/buzz/agents/a/BUZZ_PRIVATE_KEY.[0-9a-f]' "$STUB_DIR/ssm-get-parameters-by-path.out" > "$WORK/p" \
    && mv "$WORK/p" "$STUB_DIR/ssm-get-parameters-by-path.out"
  render "$1" a:claude,b:claude
  fails_without_writing && grep -qF "/buzz/agents/a/BUZZ_PRIVATE_KEY" "$WORK/err"
}

scenario_empty_credential_value_exits_1() {
  setup a b
  { printf '/buzz/agents/CLAUDE_CODE_OAUTH_TOKEN\t\n'; grep -v CLAUDE_CODE_OAUTH_TOKEN "$STUB_DIR/ssm-get-parameters-by-path.out"; } > "$WORK/p"
  grep -v ANTHROPIC_API_KEY "$WORK/p" > "$STUB_DIR/ssm-get-parameters-by-path.out"
  render "$1" a:claude,b:claude
  fails_without_writing && grep -qF "ANTHROPIC_API_KEY" "$WORK/err"
}

scenario_missing_key_exits_1() {
  setup a
  render "$1" a:claude,b:claude
  fails_without_writing && grep -qF "/buzz/agents/b/BUZZ_PRIVATE_KEY" "$WORK/err"
}

scenario_no_stub_output_exits_1() {
  setup
  : > "$STUB_DIR/ssm-get-parameters-by-path.out"
  render "$1" a:claude
  fails_without_writing
}

scenario_unsafe_credential_refused() {
  local v
  for v in 'pa$ss' 'two words' 'a"quote' 'a=b' 'a/b'; do
    setup a b
    grep -v CLAUDE_CODE_OAUTH_TOKEN "$STUB_DIR/ssm-get-parameters-by-path.out" > "$WORK/p"
    printf '/buzz/agents/CLAUDE_CODE_OAUTH_TOKEN\t%s\n' "$v" >> "$WORK/p"
    mv "$WORK/p" "$STUB_DIR/ssm-get-parameters-by-path.out"
    render "$1" a:claude,b:claude
    fails_without_writing || return 1
    ! grep -qF -- "$v" "$WORK/err" || return 1
  done
}

scenario_invalid_json_exits_1_naming_agent() {
  local bad
  for bad in '{not json' '{"servers": {}}' '{"mcpServers": []}' '[]' ''; do
    setup a b
    mkdir -p "$CFG/b"
    printf '%s' "$bad" > "$CFG/b/mcp.json"
    render "$1" a:claude,b:claude
    fails_without_writing || return 1
    grep -qF "agent b" "$WORK/err" || return 1
    ! grep -qF "agent a" "$WORK/err" || return 1
  done
}

scenario_stale_env_removed() {
  setup a b-c
  mkdir -p "$OUT"
  echo "BUZZ_PRIVATE_KEY=old" > "$OUT/old.env"
  echo "AGENT_NAME=old" > "$OUT/old.compose.env"
  echo "x" > "$OUT/b.env"
  echo "keep" > "$OUT/notes.txt"
  echo "keep" > "$OUT/Not_An_Agent.env"
  render "$1" a:claude,b-c:claude
  [ "$RC" -eq 0 ] || return 1
  [ ! -e "$OUT/old.env" ] && [ ! -e "$OUT/old.compose.env" ] || return 1
  # b is a suffix of the kept name b-c, so it must still be removed.
  [ ! -e "$OUT/b.env" ] || return 1
  [ -e "$OUT/a.env" ] && [ -e "$OUT/b-c.env" ] && [ -e "$OUT/b-c.compose.env" ] || return 1
  [ -e "$OUT/notes.txt" ] && [ -e "$OUT/Not_An_Agent.env" ]
}

# An output directory that already exists keeps its old mode unless it is set.
scenario_existing_outdir_becomes_0700() {
  setup a
  mkdir -p "$OUT"
  chmod 0755 "$OUT"
  render "$1" a:claude
  [ "$RC" -eq 0 ] && [ "$(mode_of "$OUT")" = 700 ]
}

scenario_stale_config_dir_removed() {
  setup a b-c
  mkdir -p "$OUT/old/config" "$OUT/b/config" "$OUT/Not_An_Agent/config"
  echo '{"mcpServers": {"gone": {}}}' > "$OUT/old/config/mcp.json"
  echo '{"mcpServers": {"gone": {}}}' > "$OUT/b/config/mcp.json"
  echo "keep" > "$OUT/Not_An_Agent/config/mcp.json"
  render "$1" a:claude,b-c:claude
  [ "$RC" -eq 0 ] || return 1
  [ ! -e "$OUT/old" ] && [ ! -e "$OUT/b" ] || return 1
  # Agents still in AGENTS keep their staged config, and other directories stay.
  [ -f "$OUT/a/config/mcp.json" ] && [ -f "$OUT/b-c/config/mcp.json" ] || return 1
  cmp -s "$CFG/a/mcp.json" "$OUT/a/config/mcp.json" || return 1
  [ -f "$OUT/Not_An_Agent/config/mcp.json" ]
}

scenario_failed_run_keeps_old_files() {
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  local before
  before="$(cat "$OUT/a.env")"
  mkdir -p "$CFG/b"
  printf '{bad' > "$CFG/b/mcp.json"
  render "$1" a:claude,b:claude
  [ "$RC" -eq 1 ] && [ "$(cat "$OUT/a.env")" = "$before" ]
}

scenario_empty_agents_clears_everything() {
  setup a
  render "$1" a:claude
  [ "$RC" -eq 0 ] || return 1
  render "$1" ""
  [ "$RC" -eq 0 ] && [ ! -e "$OUT/a.env" ] && [ ! -e "$OUT/a.compose.env" ] && [ ! -s "$OUT/agents.list" ]
}

scenario_secrets_not_on_stdout_or_stderr() {
  setup a b
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  ! grep -qE "$OAUTH|$APIKEY|$OPENAI|$(key_of a)" "$WORK/err" "$WORK/stdout"
}

scenario_agent_json_sets_model_effort_and_name() {
  setup a b
  printf '{"display_name":"Alpha Coder","model":"claude-opus-4-1","effort":"high"}' > "$CFG/a/agent.json"
  render "$1" a:claude,b:claude
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_DISPLAY_NAME=Alpha Coder" || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_MODEL=claude-opus-4-1" || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_EFFORT_LEVEL=high" || return 1
  # b has no agent.json: its name is the agent name and it sets no model or effort.
  has_line "$OUT/b.env" "BUZZ_ACP_DISPLAY_NAME=b" || return 1
  ! grep -qE '^BUZZ_ACP_(MODEL|EFFORT_LEVEL)=' "$OUT/b.env"
}

scenario_bad_agent_json_exits_1_and_writes_nothing() {
  setup a
  local body
  for body in '{"modle":"x"}' '{"model":"bad value"}' '{"effort":"hi;rm"}' '{"display_name":"it'"'"'s"}' '[]' '{"model":3}' '{bad'; do
    rm -rf "$OUT"
    printf '%s' "$body" > "$CFG/a/agent.json"
    render "$1" a:claude
    [ "$RC" -eq 1 ] || return 1
    [ ! -e "$OUT/a.env" ] || return 1
    grep -qF "agent.json" "$WORK/err" || return 1
  done
}

scenario_agent_json_overrides_respond_to() {
  setup a b
  mkdir -p "$CFG/b"
  printf '{"respond_to":"anyone"}' > "$CFG/a/agent.json"
  printf '{"respond_to":"allowlist","respond_to_allowlist":["%s"]}' "${ALLOW//,/\",\"}" > "$CFG/b/agent.json"
  render "$1" a:claude,b:claude AGENT_RESPOND_TO=nobody
  [ "$RC" -eq 0 ] || return 1
  has_line "$OUT/a.env" "BUZZ_ACP_RESPOND_TO=anyone" || return 1
  ! grep -q RESPOND_TO_ALLOWLIST "$OUT/a.env" || return 1
  has_line "$OUT/b.env" "BUZZ_ACP_RESPOND_TO=allowlist" || return 1
  has_line "$OUT/b.env" "BUZZ_ACP_RESPOND_TO_ALLOWLIST=$ALLOW"
}

scenario_agent_allowlist_without_keys_exits_1() {
  setup a
  printf '{"respond_to":"allowlist"}' > "$CFG/a/agent.json"
  render "$1" a:claude
  [ "$RC" -eq 1 ] || return 1
  grep -qF "respond_to_allowlist" "$WORK/err" || return 1
  [ ! -e "$OUT/a.env" ]
}

scenario_ssm_call_shape() {
  setup a
  render "$1" a:claude
  [ "$(log_count '^aws ssm get-parameters-by-path --path /buzz/agents/ --recursive --with-decryption')" -eq 1 ]
}

R="$REPO_ROOT/scripts/aws"
expect_ok "two claude agents get their own env files" scenario_two_claude_agents "$R"
expect_ok "the OAuth token wins and the API key is never written beside it" scenario_oauth_wins_and_never_both "$R"
expect_ok "the API key is used when no OAuth token is stored" scenario_api_key_when_no_oauth "$R"
expect_ok "a codex agent gets codex-acp and OPENAI_API_KEY only" scenario_codex_agent "$R"
expect_ok "bare claude and codex entries use the runtime as the name" scenario_bare_runtime_names "$R"
expect_ok "mcp.json is copied for a, and b gets the empty config" scenario_mcp_config_staging "$R"
expect_ok "config directory is 0755 and mcp.json 0644 under umask 077" scenario_config_modes_under_restrictive_umask "$R"
expect_ok "compose env file holds the five variables with absolute paths" scenario_compose_env_file "$R"
expect_ok "docker compose accepts the rendered files" scenario_compose_accepts_rendered_files "$R"
expect_ok "agents.list names every agent" scenario_agents_list "$R"
expect_ok "no file sets NO_PRESENCE or EXIT_AFTER_INACTIVITY" scenario_no_presence_or_inactivity_variables "$R"
expect_ok "respond-to modes render, with the allowlist only in allowlist mode" scenario_respond_to_modes "$R"
expect_ok "an empty key value exits 1 and writes nothing" scenario_empty_key_value_exits_1 "$R"
expect_ok "an empty credential value exits 1 and writes nothing" scenario_empty_credential_value_exits_1 "$R"
expect_ok "a missing agent key exits 1 naming the parameter" scenario_missing_key_exits_1 "$R"
expect_ok "no SSM parameters at all exits 1" scenario_no_stub_output_exits_1 "$R"
expect_ok "unsafe credential characters are refused, never shown" scenario_unsafe_credential_refused "$R"
expect_ok "invalid or wrong-shaped JSON exits 1 naming the agent" scenario_invalid_json_exits_1_naming_agent "$R"
expect_ok "stale env files are removed, other files are kept" scenario_stale_env_removed "$R"
expect_ok "an existing output directory is set to 0700" scenario_existing_outdir_becomes_0700 "$R"
expect_ok "a removed agent's staged config directory is removed too" scenario_stale_config_dir_removed "$R"
expect_ok "a failed run leaves the previous files untouched" scenario_failed_run_keeps_old_files "$R"
expect_ok "empty AGENTS clears all agent files" scenario_empty_agents_clears_everything "$R"
expect_ok "secrets never reach stdout or stderr" scenario_secrets_not_on_stdout_or_stderr "$R"
expect_ok "agent.json sets the display name, model, and effort in the env file" scenario_agent_json_sets_model_effort_and_name "$R"
expect_ok "an invalid agent.json exits 1 and writes nothing" scenario_bad_agent_json_exits_1_and_writes_nothing "$R"
expect_ok "an agent's respond_to overrides the global default" scenario_agent_json_overrides_respond_to "$R"
expect_ok "an allowlist agent without keys exits 1" scenario_agent_allowlist_without_keys_exits_1 "$R"
expect_ok "reads /buzz/agents/ recursively with decryption" scenario_ssm_call_shape "$R"

expect_caught "runtime settings never written" scenario_agent_json_sets_model_effort_and_name agent_spec.py \
  's/if field\.env is None or merged\.get\(field\.name, ""\) == "":/if True:/'
expect_caught "agent.json errors ignored" scenario_bad_agent_json_exits_1_and_writes_nothing render-agents-env.sh \
  's/^    \|\| return 1$/    || true/'
expect_caught "per-agent respond_to ignored" scenario_agent_json_overrides_respond_to agent_spec.py \
  's/merged\.update\(spec\)/merged.update({})/'
expect_caught "API key preferred over OAuth token" scenario_oauth_wins_and_never_both render-agents-env.sh \
  's/if \[ -n "\$\{SSM_CLAUDE_CODE_OAUTH_TOKEN:-\}" \]; then/if false; then/'
expect_caught "both Anthropic credentials written" scenario_oauth_wins_and_never_both render-agents-env.sh \
  's/^  echo "\$cred_name=\$cred"$/  echo "$cred_name=$cred"; echo "ANTHROPIC_API_KEY=${SSM_ANTHROPIC_API_KEY:-}"/'
expect_caught "presence disabled" scenario_no_presence_or_inactivity_variables render-agents-env.sh \
  's/^  echo "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp"$/  echo "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp"; echo "BUZZ_ACP_NO_PRESENCE=1"/'
expect_caught "inactivity exit set" scenario_no_presence_or_inactivity_variables render-agents-env.sh \
  's/^  echo "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp"$/  echo "BUZZ_ACP_MCP_COMMAND=buzz-dev-mcp"; echo "BUZZ_ACP_EXIT_AFTER_INACTIVITY=300"/'
expect_caught "empty values accepted" scenario_empty_key_value_exits_1 render-agents-env.sh \
  '/\[ -n "\$\{!var:-\}" \]/d; /\[\[ "\$\{!var\}" =~ \$2 \]\]/d'
expect_caught "empty credential accepted" scenario_empty_credential_value_exits_1 render-agents-env.sh \
  '/\[ -n "\$\{!var:-\}" \]/d; /\[\[ "\$\{!var\}" =~ \$2 \]\]/d'
expect_caught "JSON check removed" scenario_invalid_json_exits_1_naming_agent render-agents-env.sh \
  's/^  check_config "\$name" \|\| exit 1$/  true/'
expect_caught "JSON error does not name the agent" scenario_invalid_json_exits_1_naming_agent render-agents-env.sh \
  's/error: agent \$1: /error: /'
expect_caught "stale env files kept" scenario_stale_env_removed render-agents-env.sh \
  's/^    \*\) rm -rf .*$/    *) ;;/'
expect_caught "output directory mode left as found" scenario_existing_outdir_becomes_0700 render-agents-env.sh \
  's/^chmod 0700 "\$outdir"$/true/'
expect_caught "stale config directory kept" scenario_stale_config_dir_removed render-agents-env.sh \
  's/rm -rf "\$\{outdir:\?\}\/\$stem" /rm -f /'
expect_caught "stale match is a substring" scenario_stale_env_removed render-agents-env.sh \
  's/\*\$.\\n.\"\$stem\"\$.\\n.\*\) ;;/*"$stem"*) ;;/'
expect_caught "config directory mode left to umask" scenario_config_modes_under_restrictive_umask render-agents-env.sh \
  's/^  chmod 0755 "\$config_dir"$/  true/'
expect_caught "mcp.json mode left to umask" scenario_config_modes_under_restrictive_umask render-agents-env.sh \
  's/^  chmod 0644 "\$config_dir\/mcp.json"$/  true/'
expect_caught "env file readable by others" scenario_config_modes_under_restrictive_umask render-agents-env.sh \
  's/^umask 077$/umask 022/'
expect_caught "checks skipped before writing" scenario_failed_run_keeps_old_files render-agents-env.sh \
  's/^  agent_env "\$name" "\$runtime" > \/dev\/null \|\| exit 1$/  true/; s/^  check_config "\$name" \|\| exit 1$/  true/'

exit "$fail"
