#!/usr/bin/env bash
# Tests for load_settings, require_tool, and tf_output in scripts/aws/lib.sh.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
export REPO_ROOT

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PUBKEY="$(printf 'a%.0s' $(seq 1 64))"
PUBKEY2="$(printf 'b%.0s' $(seq 1 64))"

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

# write_env <file> [extra lines...]: a valid base file followed by extra lines.
write_env() {
  local f="$1"; shift
  {
    echo "AWS_PROFILE=test"
    echo "TF_STATE_BUCKET=bucket"
    echo "ROUTE53_ZONE=example.com"
    echo "BUZZ_DOMAIN=buzz.example.com"
    echo "RELAY_OWNER_PUBKEY=$PUBKEY"
    for line in "$@"; do echo "$line"; done
  } > "$f"
}

# clean_env: drops inherited settings so the caller's shell cannot affect results.
clean_env() {
  unset AWS_PROFILE TF_STATE_BUCKET ROUTE53_ZONE BUZZ_DOMAIN RELAY_OWNER_PUBKEY \
    AWS_REGION INSTANCE_TYPE AGENTS AGENT_RESPOND_TO AGENT_RESPOND_TO_ALLOWLIST
}

# run_load <env-file>: runs load_settings in a subshell, prints combined output.
run_load() {
  (
    clean_env
    export AWS_ENV_FILE="$1"
    # shellcheck source=/dev/null
    . "$REPO_ROOT/scripts/aws/lib.sh"
    load_settings
    echo "AWS_REGION=$AWS_REGION INSTANCE_TYPE=$INSTANCE_TYPE AGENTS=$AGENTS RESPOND=$AGENT_RESPOND_TO"
  ) 2>&1
}

# expect_fail <name> <env-file> <message-substring>
expect_fail() {
  local name="$1" file="$2" want="$3" out rc
  out="$(run_load "$file")"; rc=$?
  if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -qF -- "$want"; then
    pass "$name"
  else
    bad "$name (rc=$rc, wanted rc=1 and '$want', got: $out)"
  fi
}

# expect_ok <name> <env-file> <output-substring>
expect_ok() {
  local name="$1" file="$2" want="$3" out rc
  out="$(run_load "$file")"; rc=$?
  if [ "$rc" -eq 0 ] && printf '%s' "$out" | grep -qF -- "$want"; then
    pass "$name"
  else
    bad "$name (rc=$rc, wanted '$want', got: $out)"
  fi
}

if [ ! -f "$REPO_ROOT/scripts/aws/lib.sh" ]; then
  echo "FAIL - scripts/aws/lib.sh not found"
  exit 1
fi

# Missing BUZZ_DOMAIN.
f="$WORK/missing-domain.env"
grep -v '^BUZZ_DOMAIN=' <(write_env /dev/stdout) > "$f"
expect_fail "missing BUZZ_DOMAIN names it" "$f" "BUZZ_DOMAIN"

# Each other required setting.
for var in AWS_PROFILE TF_STATE_BUCKET ROUTE53_ZONE RELAY_OWNER_PUBKEY; do
  f="$WORK/missing-$var.env"
  grep -v "^$var=" <(write_env /dev/stdout) > "$f"
  expect_fail "missing $var names it" "$f" "$var"
done

# Domain shape.
for d in "https://buzz.example.com/" "buzz.example.com/" "buzz.example.com." "buzz example.com" \
  "buzz.example.com:3000" ".example.com" "Buzz.Example.com" "localhost" "-buzz.example.com" "buzz-.example.com" "buzz..example.com"; do
  f="$WORK/domain.env"
  write_env "$f" "BUZZ_DOMAIN=\"$d\""
  expect_fail "BUZZ_DOMAIN '$d' rejected" "$f" "bare host name"
done

# Whitespace-only required value.
f="$WORK/blank.env"
write_env "$f" "AWS_PROFILE=\"   \""
expect_fail "whitespace-only AWS_PROFILE rejected" "$f" "AWS_PROFILE"

# Owner pubkey.
f="$WORK/npub.env"
write_env "$f" "RELAY_OWNER_PUBKEY=npub1abcdefghijklmnop"
expect_fail "npub rejected" "$f" "64 hex"
f="$WORK/upper.env"
write_env "$f" "RELAY_OWNER_PUBKEY=$(printf 'A%.0s' $(seq 1 64))"
expect_fail "uppercase pubkey rejected" "$f" "64 hex"

# Agents.
f="$WORK/agents-bad.env"
write_env "$f" "AGENTS=claude,gemini"
expect_fail "unknown agent names it" "$f" "gemini"
f="$WORK/agents-empty.env"
write_env "$f" "AGENTS="
expect_ok "empty AGENTS disables agents" "$f" "AGENTS= "
f="$WORK/agents-one.env"
write_env "$f" "AGENTS=codex"
expect_ok "single agent accepted" "$f" "AGENTS=codex "

# Named agents: name or name:runtime.
f="$WORK/agents-named.env"
write_env "$f" "AGENTS=reviewer:claude,docs:claude,codex"
expect_ok "named agents accepted" "$f" "AGENTS=reviewer:claude,docs:claude,codex "

# run_agent_list <env-file>: prints agent_list output after load_settings.
run_agent_list() {
  (
    clean_env
    export AWS_ENV_FILE="$1"
    # shellcheck source=/dev/null
    . "$REPO_ROOT/scripts/aws/lib.sh"
    load_settings
    agent_list
  ) 2>&1
}
out="$(run_agent_list "$f")"
if [ "$out" = "$(printf 'reviewer claude\ndocs claude\ncodex codex')" ]; then
  pass "agent_list prints name and runtime per entry"
else
  bad "agent_list prints name and runtime per entry (got: $out)"
fi
f="$WORK/agents-bare.env"
write_env "$f" "AGENTS=claude"
out="$(run_agent_list "$f")"
if [ "$out" = "claude claude" ]; then
  pass "agent_list: bare runtime name means name equals runtime"
else
  bad "agent_list: bare runtime name means name equals runtime (got: $out)"
fi
f="$WORK/agents-default.env"
write_env "$f"
out="$(run_agent_list "$f")"
if [ "$out" = "$(printf 'claude claude\ncodex codex')" ]; then
  pass "agent_list prints the default agents"
else
  bad "agent_list prints the default agents (got: $out)"
fi
f="$WORK/agents-none.env"
write_env "$f" "AGENTS="
out="$(run_agent_list "$f")"; rc=$?
if [ "$rc" -eq 0 ] && [ -z "$out" ]; then
  pass "agent_list prints nothing when AGENTS is empty"
else
  bad "agent_list prints nothing when AGENTS is empty (rc=$rc, got: $out)"
fi

f="$WORK/agents-runtime.env"
write_env "$f" "AGENTS=a:gemini"
expect_fail "a:gemini names the unknown runtime" "$f" "unknown runtime 'gemini'"
f="$WORK/agents-badname.env"
write_env "$f" "AGENTS=Bad_Name:claude"
expect_fail "Bad_Name:claude names the invalid name" "$f" "invalid agent name 'Bad_Name'"
f="$WORK/agents-dupname.env"
write_env "$f" "AGENTS=x:claude,x:codex"
expect_fail "duplicate agent name rejected" "$f" "duplicate agent name 'x'"
f="$WORK/agents-barename.env"
write_env "$f" "AGENTS=reviewer"
expect_fail "bare name that is not a runtime rejected" "$f" "unknown agent 'reviewer'"
f="$WORK/agents-noruntime.env"
write_env "$f" "AGENTS=reviewer:"
expect_fail "reviewer: (empty runtime) rejected" "$f" "empty runtime"
f="$WORK/agents-noname.env"
write_env "$f" "AGENTS=:claude"
expect_fail ":claude (empty name) rejected" "$f" "empty name"
f="$WORK/agents-dupbare.env"
write_env "$f" "AGENTS=claude,claude:claude"
expect_fail "bare and explicit entry with the same name rejected" "$f" "duplicate agent name 'claude'"
long="a$(printf 'b%.0s' $(seq 1 24))"
f="$WORK/agents-long.env"
write_env "$f" "AGENTS=$long:claude"
expect_fail "25-character agent name rejected" "$f" "invalid agent name"
f="$WORK/agents-24.env"
write_env "$f" "AGENTS=${long%b}:codex"
expect_ok "24-character agent name accepted" "$f" "AGENTS=${long%b}:codex "
f="$WORK/agents-three.env"
write_env "$f" "AGENTS=a:b:claude"
expect_fail "extra colon rejected" "$f" "unknown runtime"

# List shape.
for a in "claude," ",claude" "claude,,codex"; do
  f="$WORK/agents-shape.env"
  write_env "$f" "AGENTS=$a"
  expect_fail "AGENTS '$a' rejected as empty entry" "$f" "empty entry"
done
f="$WORK/agents-dup.env"
write_env "$f" "AGENTS=claude,claude"
expect_fail "duplicate agent rejected" "$f" "duplicate"
for a in "$PUBKEY," ",$PUBKEY" "$PUBKEY,,$PUBKEY2"; do
  f="$WORK/allow-shape.env"
  write_env "$f" "AGENT_RESPOND_TO=allowlist" "AGENT_RESPOND_TO_ALLOWLIST=$a"
  expect_fail "allowlist '${a:0:5}...' rejected as empty entry" "$f" "empty entry"
done
f="$WORK/allow-dup.env"
write_env "$f" "AGENT_RESPOND_TO=allowlist" "AGENT_RESPOND_TO_ALLOWLIST=$PUBKEY,$PUBKEY"
expect_fail "duplicate allowlist pubkey rejected" "$f" "duplicate"

# Respond-to.
f="$WORK/respond-bad.env"
write_env "$f" "AGENT_RESPOND_TO=everyone"
expect_fail "unknown AGENT_RESPOND_TO rejected" "$f" "AGENT_RESPOND_TO"
f="$WORK/allow-empty.env"
write_env "$f" "AGENT_RESPOND_TO=allowlist"
expect_fail "allowlist with empty list rejected" "$f" "AGENT_RESPOND_TO_ALLOWLIST"
f="$WORK/allow-bad.env"
write_env "$f" "AGENT_RESPOND_TO=allowlist" "AGENT_RESPOND_TO_ALLOWLIST=$PUBKEY,notahex"
expect_fail "allowlist with non-hex rejected" "$f" "AGENT_RESPOND_TO_ALLOWLIST"
f="$WORK/allow-ok.env"
write_env "$f" "AGENT_RESPOND_TO=allowlist" "AGENT_RESPOND_TO_ALLOWLIST=$PUBKEY,$PUBKEY2"
expect_ok "valid allowlist accepted" "$f" "RESPOND=allowlist"
f="$WORK/anyone-ok.env"
write_env "$f" "AGENT_RESPOND_TO=anyone" "AGENT_RESPOND_TO_ALLOWLIST=garbage"
expect_ok "allowlist ignored unless AGENT_RESPOND_TO=allowlist" "$f" "RESPOND=anyone"

# Region shape.
for r in "us-east-1; rm -rf /" "US-EAST-1" "useast1" "us-east" "us-east-1 " "-east-1" "us--1" '$(id)'; do
  f="$WORK/region.env"
  write_env "$f" "AWS_REGION=\"$r\""
  expect_fail "AWS_REGION '$r' rejected" "$f" "AWS_REGION must look like"
done
for r in eu-west-2 us-gov-west-1 ap-southeast-4; do
  f="$WORK/region-ok.env"
  write_env "$f" "AWS_REGION=$r"
  expect_ok "AWS_REGION '$r' accepted" "$f" "AWS_REGION=$r "
done

# Valid file with defaults.
f="$WORK/valid.env"
write_env "$f"
expect_ok "valid file exports defaults" "$f" \
  "AWS_REGION=us-east-1 INSTANCE_TYPE=t4g.large AGENTS=claude,codex RESPOND=owner-only"

# Exported to child processes.
out="$(
  clean_env
  export AWS_ENV_FILE="$f"
  . "$REPO_ROOT/scripts/aws/lib.sh"
  load_settings
  bash -c 'echo "$AWS_REGION $INSTANCE_TYPE $AGENTS $BUZZ_DOMAIN"'
)"
if [ "$out" = "us-east-1 t4g.large claude,codex buzz.example.com" ]; then
  pass "settings are exported"
else
  bad "settings are exported (got: $out)"
fi

# Missing env file.
out="$(run_load "$WORK/nope.env")"; rc=$?
if [ "$rc" -eq 1 ] && printf '%s' "$out" | grep -qF "nope.env"; then
  pass "missing env file fails and names it"
else
  bad "missing env file fails and names it (rc=$rc, got: $out)"
fi

# require_tool.
out="$(. "$REPO_ROOT/scripts/aws/lib.sh"; require_tool definitely-not-a-tool-xyz 2>&1)"; rc=$?
if [ "$rc" -eq 1 ] && [ "$out" = "missing tool: definitely-not-a-tool-xyz" ]; then
  pass "require_tool fails on missing tool"
else
  bad "require_tool fails on missing tool (rc=$rc, got: $out)"
fi
out="$(. "$REPO_ROOT/scripts/aws/lib.sh"; require_tool bash 2>&1)"; rc=$?
if [ "$rc" -eq 0 ] && [ -z "$out" ]; then
  pass "require_tool passes for present tool"
else
  bad "require_tool passes for present tool (rc=$rc, got: $out)"
fi

# tf_output uses terraform via PATH; stub it.
mkdir -p "$WORK/bin"
cat > "$WORK/bin/terraform" <<'STUB'
#!/usr/bin/env bash
echo "terraform $*"
STUB
chmod +x "$WORK/bin/terraform"
out="$(PATH="$WORK/bin:$PATH"; . "$REPO_ROOT/scripts/aws/lib.sh"; tf_output instance_id)"
if [ "$out" = "terraform -chdir=infra output -raw instance_id" ]; then
  pass "tf_output calls terraform output -raw"
else
  bad "tf_output calls terraform output -raw (got: $out)"
fi

# The agent name rule is one pattern, and it agrees with the names the Rust check is tested on.
(
  . "$REPO_ROOT/scripts/aws/lib.sh"
  bad_names=""
  while read -r verdict name; do
    case "$verdict" in "" | "#"*) continue ;; esac
    if [[ "$name" =~ $AGENT_NAME_PATTERN ]]; then got=ok; else got=bad; fi
    [ "$got" = "$verdict" ] || bad_names="$bad_names $name"
  done < "$REPO_ROOT/tests/aws/fixtures/agent-names.txt"
  [ -z "$bad_names" ] || { echo "disagree:$bad_names"; exit 1; }
)
rc=$?
if [ "$rc" -eq 0 ]; then
  pass "AGENT_NAME_PATTERN agrees with the shared list of agent names"
else
  bad "AGENT_NAME_PATTERN agrees with the shared list of agent names"
fi
if grep -q "NAME_PATTERN=" "$REPO_ROOT/scripts/aws/render-agents-env.sh"; then
  bad "render-agents-env.sh uses the pattern of lib.sh and defines none"
else
  pass "render-agents-env.sh uses the pattern of lib.sh and defines none"
fi

exit "$fail"
