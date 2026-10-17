#!/usr/bin/env bash
# Tests for scripts/aws/sync-upstream.sh, run against throwaway local repos.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
SCRIPT="$REPO_ROOT/scripts/aws/sync-upstream.sh"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

# Isolate git from the caller: no inherited GIT_* or AWS_* variables, no global or
# system config, no hooks.
for v in $(env | sed -n 's/^\(GIT_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
for v in $(env | sed -n 's/^\(AWS_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

# repo_config <dir>: per-repo identity, no signing, no hooks.
repo_config() {
  git -C "$1" config user.name "Test User"
  git -C "$1" config user.email "test@example.com"
  git -C "$1" config commit.gpgsign false
  git -C "$1" config core.hooksPath /dev/null
}

# commit_file <dir> <file> <content> <message>
commit_file() {
  printf '%s\n' "$3" > "$1/$2"
  git -C "$1" add "$2"
  git -C "$1" commit -q -m "$4"
}

# make_repos <name>: sets UP (bare upstream), ORIGIN (bare origin), FORK (clone of origin).
make_repos() {
  local base="$WORK/$1"
  mkdir -p "$base"
  git init -q -b main "$base/seed"
  repo_config "$base/seed"
  commit_file "$base/seed" shared.txt "base" "seed"
  git clone -q --bare "$base/seed" "$base/upstream.git"
  git clone -q --bare "$base/seed" "$base/origin.git"
  git clone -q "$base/origin.git" "$base/fork"
  repo_config "$base/fork"
  UP="$base/upstream.git"
  ORIGIN="$base/origin.git"
  FORK="$base/fork"
  UPWORK="$base/upwork"
}

# upstream_commit <file> <content> <message>: pushes a new commit to the bare upstream.
upstream_commit() {
  if [ ! -d "$UPWORK" ]; then
    git clone -q "$UP" "$UPWORK"
    repo_config "$UPWORK"
  fi
  commit_file "$UPWORK" "$1" "$2" "$3"
  git -C "$UPWORK" push -q origin main
}

# run_sync: runs the script inside the fork; sets OUT and RC.
run_sync() {
  OUT="$(cd "$FORK" && UPSTREAM_URL="$UP" bash "$SCRIPT" 2>&1)"
  RC=$?
}

if [ ! -f "$SCRIPT" ]; then
  echo "FAIL - scripts/aws/sync-upstream.sh not found"
  exit 1
fi

# Dirty working tree.
make_repos dirty
echo "edit" >> "$FORK/shared.txt"
run_sync
if [ "$RC" -eq 1 ] && printf '%s' "$OUT" | grep -qF "uncommitted"; then
  pass "dirty tree exits 1 and mentions uncommitted"
else
  bad "dirty tree (rc=$RC, got: $OUT)"
fi
if ! git -C "$FORK" remote get-url upstream >/dev/null 2>&1; then
  pass "dirty tree adds no remote"
else
  bad "dirty tree should fail before touching remotes"
fi

# Existing upstream remote with a different URL.
make_repos mismatch
git -C "$FORK" remote add upstream "https://example.invalid/other.git"
run_sync
if [ "$RC" -eq 1 ] \
  && printf '%s' "$OUT" | grep -qF "https://example.invalid/other.git" \
  && printf '%s' "$OUT" | grep -qF "$UP"; then
  pass "remote mismatch exits 1 and prints both URLs"
else
  bad "remote mismatch (rc=$RC, got: $OUT)"
fi
if [ "$(git -C "$FORK" remote get-url upstream)" = "https://example.invalid/other.git" ]; then
  pass "remote mismatch leaves the remote unchanged"
else
  bad "remote mismatch changed the upstream remote"
fi

# Clean merge, and no push.
make_repos merge
upstream_commit new.txt "from upstream" "upstream change"
origin_before="$(git -C "$ORIGIN" rev-parse main)"
up_before="$(git -C "$UP" rev-parse main)"
run_sync
if [ "$RC" -eq 0 ] && [ -f "$FORK/new.txt" ]; then
  pass "clean merge brings in the upstream commit"
else
  bad "clean merge (rc=$RC, got: $OUT)"
fi
if [ "$(git -C "$FORK" remote get-url upstream 2>/dev/null)" = "$UP" ]; then
  pass "clean merge adds the upstream remote"
else
  bad "clean merge did not add the upstream remote"
fi
if printf '%s' "$OUT" | grep -qF "git push origin main"; then
  pass "clean merge prints the push command"
else
  bad "clean merge missing push command (got: $OUT)"
fi
if [ "$(git -C "$ORIGIN" rev-parse main)" = "$origin_before" ] \
  && [ "$(git -C "$UP" rev-parse main)" = "$up_before" ]; then
  pass "clean merge pushes nowhere"
else
  bad "clean merge pushed to a remote"
fi

# Already up to date.
count_before="$(git -C "$FORK" rev-list --count HEAD)"
run_sync
count_after="$(git -C "$FORK" rev-list --count HEAD)"
if [ "$RC" -eq 0 ] && printf '%s' "$OUT" | grep -qF "already up to date" \
  && [ "$count_before" = "$count_after" ]; then
  pass "already up to date exits 0 and adds no commits"
else
  bad "already up to date (rc=$RC, before=$count_before after=$count_after, got: $OUT)"
fi

# Detached HEAD.
make_repos detached
git -C "$FORK" checkout -q --detach
run_sync
if [ "$RC" -eq 1 ] && printf '%s' "$OUT" | grep -qF "detached"; then
  pass "detached HEAD exits 1 and mentions detached"
else
  bad "detached HEAD (rc=$RC, got: $OUT)"
fi

# Conflict: merge stays in place and the file is listed.
make_repos conflict
commit_file "$FORK" shared.txt "fork side" "fork edit"
upstream_commit shared.txt "upstream side" "upstream edit"
run_sync
if [ "$RC" -eq 1 ] && printf '%s' "$OUT" | grep -qF "shared.txt"; then
  pass "conflict exits 1 and lists the file"
else
  bad "conflict (rc=$RC, got: $OUT)"
fi
if [ -f "$FORK/.git/MERGE_HEAD" ]; then
  pass "conflict leaves the merge in place"
else
  bad "conflict aborted the merge"
fi

if [ "$fail" -ne 0 ]; then
  echo "test-sync-upstream: FAIL" >&2
  exit 1
fi
echo "test-sync-upstream: PASS"
