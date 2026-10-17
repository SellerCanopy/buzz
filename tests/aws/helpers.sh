#!/usr/bin/env bash
# Shared setup for the stub-based tests in tests/aws. Source this file.
# It makes the test hermetic: it drops AWS_*, TF_*, GIT_*, and deploy variables
# inherited from the caller, puts tests/aws/stubs first on PATH, and gives each
# test its own STUB_DIR and STUB_LOG under a temp directory. Never reads or
# writes the real AWS account or the real repo state.

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
export REPO_ROOT

for _v in $(compgen -v | grep -E '^(AWS_|TF_|GIT_|BACKUP_CMD$|DRY_RUN$|ROLLBACK$|POLL_|HEALTH_|BUZZ_|RELAY_|AGENT|INSTANCE_TYPE$|ROUTE53_ZONE$|ECR_REPO$)'); do
  unset "$_v"
done
unset _v

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

STUB_DIR="$WORK/stub"
STUB_LOG="$WORK/stub.log"
export STUB_DIR STUB_LOG
PATH="$REPO_ROOT/tests/aws/stubs:$PATH"
export PATH

PUBKEY="$(printf 'a%.0s' $(seq 1 64))"
DOMAIN="buzz.example.com"

fail=0
pass() { echo "ok   - $1"; }
# shellcheck disable=SC2034 # fail is read by the sourcing test (exit "$fail")
bad() { echo "FAIL - $1"; fail=1; }

# reset_stubs: clears every stub answer and the call log.
reset_stubs() {
  rm -rf "$STUB_DIR"
  mkdir -p "$STUB_DIR"
  : > "$STUB_LOG"
}

# write_aws_env: writes a valid settings file and points load_settings at it.
write_aws_env() {
  {
    echo "AWS_PROFILE=test"
    echo "TF_STATE_BUCKET=bucket"
    echo "ROUTE53_ZONE=example.com"
    echo "BUZZ_DOMAIN=$DOMAIN"
    echo "RELAY_OWNER_PUBKEY=$PUBKEY"
  } > "$WORK/aws.env"
  AWS_ENV_FILE="$WORK/aws.env"
  export AWS_ENV_FILE
}

# log_count <regex>: number of stub log lines matching the regex.
log_count() { grep -cE -- "$1" "$STUB_LOG" || true; }

# log_line <regex>: line number of the first match in the stub log, empty if none.
log_line() { grep -nE -- "$1" "$STUB_LOG" | head -n 1 | cut -d: -f1; }

# make_mutant <name> <file> <sed-expr>: copies scripts/aws to $WORK/mutant-<name>,
# applies the edit to <file> there, and prints the directory. Returns 1 if the
# edit changed nothing.
make_mutant() {
  local name="$1" file="$2" expr="$3" d="$WORK/mutant-$1"
  rm -rf "$d"
  mkdir -p "$d"
  cp "$REPO_ROOT"/scripts/aws/*.sh "$REPO_ROOT"/scripts/aws/*.py "$d/"
  sed -E "$expr" "$REPO_ROOT/scripts/aws/$file" > "$d/$file"
  if cmp -s "$REPO_ROOT/scripts/aws/$file" "$d/$file"; then
    return 1
  fi
  printf '%s' "$d"
}

# expect_ok <name> <function> <scripts-dir>: the scenario must pass on the real scripts.
expect_ok() {
  if "$2" "$3"; then pass "$1"; else bad "$1"; fi
}

# expect_caught <name> <function> <sed-file> <sed-expr>: the same scenario must
# FAIL on a copy of the scripts with the guard edited out, proving it checks the guard.
expect_caught() {
  local name="$1" fn="$2" file="$3" expr="$4" dir
  dir="$(make_mutant "$name" "$file" "$expr")" || { bad "mutation '$name' changed nothing"; return; }
  if "$fn" "$dir"; then bad "mutant survives: $name"; else pass "mutant fails: $name"; fi
}
