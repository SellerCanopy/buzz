#!/usr/bin/env bash
# Tests for the init, plan, apply, and destroy Make targets (scripts/aws/tf.sh)
# against a stub terraform that logs its arguments.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
export REPO_ROOT

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

PUBKEY="$(printf 'a%.0s' $(seq 1 64))"
LOG="$WORK/terraform.log"

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

cat > "$WORK/env" <<ENV
AWS_PROFILE=test
TF_STATE_BUCKET=state-bucket
ROUTE53_ZONE=example.com
BUZZ_DOMAIN=buzz.example.com
RELAY_OWNER_PUBKEY=$PUBKEY
AWS_REGION=eu-west-1
INSTANCE_TYPE=t4g.small
ENV

mkdir -p "$WORK/bin"
cat > "$WORK/bin/terraform" <<'STUB'
#!/usr/bin/env bash
if [ "$1" = "version" ]; then
  echo "Terraform v$STUB_VERSION"
  echo "on linux_arm64"
  exit 0
fi
echo "$*" >> "$STUB_LOG"
STUB
chmod +x "$WORK/bin/terraform"

# run_make <version> <stdin> <target>: runs make in a clean environment so the
# caller's AWS_* and TF_* variables cannot leak in. Prints combined output.
run_make() {
  local version="$1" input="$2" target="$3"
  : > "$LOG"
  (
    cd "$REPO_ROOT" || exit 1
    printf '%s\n' "$input" | env -i PATH="$WORK/bin:$PATH" HOME="$WORK" \
      AWS_ENV_FILE="$WORK/env" STUB_VERSION="$version" STUB_LOG="$LOG" \
      make "$target" 2>&1
  )
}

# Old terraform: init fails, names 1.10, and never calls init.
out="$(run_make 1.9.8 "" init)"; rc=$?
if [ "$rc" -ne 0 ] && printf '%s' "$out" | grep -qF "1.10" && ! grep -q "init" "$LOG"; then
  pass "init refuses terraform 1.9 and names 1.10"
else
  bad "init refuses terraform 1.9 and names 1.10 (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# New terraform: init passes the four backend-config flags built from settings.
out="$(run_make 1.10.5 "" init)"; rc=$?
want="-chdir=infra init -backend-config=bucket=state-bucket -backend-config=key=buzz/terraform.tfstate -backend-config=region=eu-west-1 -backend-config=use_lockfile=true"
if [ "$rc" -eq 0 ] && [ "$(cat "$LOG")" = "$want" ]; then
  pass "init calls terraform with the backend-config flags"
else
  bad "init calls terraform with the backend-config flags (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# plan: fmt, validate, then plan with the four -var flags.
out="$(run_make 1.10.5 "" plan)"; rc=$?
want_plan="-chdir=infra plan -var aws_region=eu-west-1 -var instance_type=t4g.small -var domain=buzz.example.com -var route53_zone=example.com"
if [ "$rc" -eq 0 ] \
  && [ "$(sed -n 1p "$LOG")" = "-chdir=infra fmt -check" ] \
  && [ "$(sed -n 2p "$LOG")" = "-chdir=infra validate" ] \
  && [ "$(sed -n 3p "$LOG")" = "$want_plan" ]; then
  pass "plan runs fmt, validate, then plan with -var flags"
else
  bad "plan runs fmt, validate, then plan with -var flags (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# apply: declining at the prompt never calls apply.
out="$(run_make 1.10.5 "no" apply)"; rc=$?
if [ "$rc" -ne 0 ] && grep -q "plan .*-out=buzz.tfplan" "$LOG" && ! grep -q " apply " "$LOG"; then
  pass "apply declined does not call terraform apply"
else
  bad "apply declined does not call terraform apply (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# apply: typing yes applies the saved plan file.
out="$(run_make 1.10.5 "yes" apply)"; rc=$?
if [ "$rc" -eq 0 ] && grep -qxF -- "-chdir=infra apply buzz.tfplan" "$LOG"; then
  pass "apply confirmed applies the saved plan"
else
  bad "apply confirmed applies the saved plan (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# destroy: wrong input aborts and never calls terraform destroy.
out="$(run_make 1.10.5 "wrong.example.com" destroy)"; rc=$?
if [ "$rc" -ne 0 ] && ! grep -q "destroy" "$LOG"; then
  pass "destroy with wrong input aborts without calling destroy"
else
  bad "destroy with wrong input aborts without calling destroy (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# destroy: empty input (closed stdin) aborts too.
out="$(cd "$REPO_ROOT" && : > "$LOG" && env -i PATH="$WORK/bin:$PATH" HOME="$WORK" \
  AWS_ENV_FILE="$WORK/env" STUB_VERSION=1.10.5 STUB_LOG="$LOG" make destroy < /dev/null 2>&1)"; rc=$?
if [ "$rc" -ne 0 ] && ! grep -q "destroy" "$LOG"; then
  pass "destroy with no input aborts without calling destroy"
else
  bad "destroy with no input aborts without calling destroy (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

# destroy: the exact domain calls destroy with the -var flags.
out="$(run_make 1.10.5 "buzz.example.com" destroy)"; rc=$?
want_destroy="-chdir=infra destroy -var aws_region=eu-west-1 -var instance_type=t4g.small -var domain=buzz.example.com -var route53_zone=example.com"
if [ "$rc" -eq 0 ] && [ "$(cat "$LOG")" = "$want_destroy" ]; then
  pass "destroy with the exact domain calls terraform destroy"
else
  bad "destroy with the exact domain calls terraform destroy (rc=$rc, out: $out, log: $(cat "$LOG"))"
fi

exit "$fail"
