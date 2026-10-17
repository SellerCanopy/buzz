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

exit "$fail"
