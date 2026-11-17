#!/usr/bin/env bash
# Tests for the upstream-edits check: every file this fork changes against upstream is
# fork-only (scripts/aws/fork-only-paths.txt) or listed in scripts/aws/upstream-edits.txt,
# and every listed file really is in the diff unless it carries an `(only if ...)` mark.
# The check runs against throwaway repos, then against this repo when upstream/main exists.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
EDITS="$REPO_ROOT/scripts/aws/upstream-edits.txt"
FORK_ONLY="$REPO_ROOT/scripts/aws/fork-only-paths.txt"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

for v in $(env | sed -n 's/^\(GIT_[A-Za-z0-9_]*\)=.*/\1/p'); do unset "$v"; done
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_SYSTEM=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_TERMINAL_PROMPT=0

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

# list_entries <file>: the paths in a list file, without comments, blanks or `(only if ...)`.
list_entries() {
  sed -e 's/[[:space:]]*(only if[^)]*)[[:space:]]*$//' -e 's/[[:space:]]*$//' "$1" \
    | grep -v -e '^#' -e '^$' || true
}

# optional_entries <file>: the paths that carry an `(only if ...)` mark.
optional_entries() {
  grep -v "^#" "$1" | grep -E "\(only if[^)]*\)[[:space:]]*\$" \
    | sed -e 's/[[:space:]]*(only if[^)]*)[[:space:]]*$//' || true
}

# check_edits <repo> <base-ref> <edits-file> <fork-only-file>
# Prints one problem per line. Returns 0 when there is none.
check_edits() {
  local repo="$1" base="$2" edits="$3" forkonly="$4" diff entry file problems=0
  diff="$(git -C "$repo" diff --name-only "$base...HEAD")" || {
    echo "cannot diff $base...HEAD"
    return 1
  }
  while IFS= read -r file; do
    [ -n "$file" ] || continue
    if list_entries "$edits" | grep -qxF -- "$file"; then continue; fi
    while IFS= read -r entry; do
      case "$entry" in
        */) case "$file" in "$entry"*) continue 2 ;; esac ;;
        *) [ "$file" = "$entry" ] && continue 2 ;;
      esac
    done < <(list_entries "$forkonly")
    echo "unlisted upstream edit: $file"
    problems=1
  done <<EOF
$diff
EOF
  optional="$(optional_entries "$edits")"
  while IFS= read -r entry; do
    [ -n "$entry" ] || continue
    printf '%s\n' "$optional" | grep -qxF -- "$entry" && continue
    printf '%s\n' "$diff" | grep -qxF -- "$entry" && continue
    echo "stale entry, not in the diff: $entry"
    problems=1
  done < <(list_entries "$edits")
  return "$problems"
}

repo_config() {
  git -C "$1" config user.name "Test User"
  git -C "$1" config user.email "test@example.com"
  git -C "$1" config commit.gpgsign false
  git -C "$1" config core.hooksPath /dev/null
}

# make_fixture <name>: a repo with an `upstream/main` ref at the base commit and HEAD on
# a branch from it. Sets FIX, EDITS_F and FORK_F (list files for the fixture).
make_fixture() {
  FIX="$WORK/$1"
  git init -q -b main "$FIX"
  repo_config "$FIX"
  mkdir -p "$FIX/src"
  echo base > "$FIX/src/lib.rs"
  echo base > "$FIX/Cargo.lock"
  git -C "$FIX" add -A
  git -C "$FIX" commit -q -m base
  git -C "$FIX" update-ref refs/remotes/upstream/main HEAD
  git -C "$FIX" checkout -q -b fork
  printf '%s\n' '# comment' 'Cargo.lock' 'src/lib.rs' 'src/maybe.rs   (only if needed)' > "$WORK/$1.edits"
  printf '%s\n' '# comment' 'hive/' 'Makefile' > "$WORK/$1.fork"
  EDITS_F="$WORK/$1.edits"
  FORK_F="$WORK/$1.fork"
}

# commit_files <repo> <path>...: change or add each path and commit.
commit_files() {
  local repo="$1" p
  shift
  for p in "$@"; do
    mkdir -p "$repo/$(dirname "$p")"
    echo "change $p" >> "$repo/$p"
  done
  git -C "$repo" add -A
  git -C "$repo" commit -q -m change
}

# Clean: listed edits, a fork-only directory and a fork-only file, and an optional entry
# that is absent from the diff.
make_fixture clean
commit_files "$FIX" Cargo.lock src/lib.rs hive/core.rs Makefile
out="$(check_edits "$FIX" upstream/main "$EDITS_F" "$FORK_F")"
if [ $? -eq 0 ] && [ -z "$out" ]; then
  pass "a clean diff passes"
else
  bad "a clean diff failed (got: $out)"
fi

# An unlisted upstream file fails and is named.
make_fixture unlisted
commit_files "$FIX" Cargo.lock src/lib.rs src/extra.rs
out="$(check_edits "$FIX" upstream/main "$EDITS_F" "$FORK_F")"
if [ $? -ne 0 ] && printf '%s\n' "$out" | grep -qxF "unlisted upstream edit: src/extra.rs"; then
  pass "an unlisted file fails and is named"
else
  bad "an unlisted file did not fail (got: $out)"
fi

# A fork-only prefix does not cover a sibling directory with the same leading letters.
make_fixture sibling
commit_files "$FIX" Cargo.lock src/lib.rs hive-other/x.rs
out="$(check_edits "$FIX" upstream/main "$EDITS_F" "$FORK_F")"
if [ $? -ne 0 ] && printf '%s\n' "$out" | grep -qxF "unlisted upstream edit: hive-other/x.rs"; then
  pass "a fork-only directory prefix does not cover a sibling name"
else
  bad "a sibling directory slipped through (got: $out)"
fi

# A listed file that is not in the diff and has no `(only if)` mark is stale.
make_fixture stale
commit_files "$FIX" src/lib.rs
out="$(check_edits "$FIX" upstream/main "$EDITS_F" "$FORK_F")"
if [ $? -ne 0 ] && printf '%s\n' "$out" | grep -qxF "stale entry, not in the diff: Cargo.lock"; then
  pass "a listed file missing from the diff fails as stale"
else
  bad "a stale entry did not fail (got: $out)"
fi

# An `(only if)` entry may be absent, and may be present.
make_fixture optional
commit_files "$FIX" Cargo.lock src/lib.rs src/maybe.rs
out="$(check_edits "$FIX" upstream/main "$EDITS_F" "$FORK_F")"
if [ $? -eq 0 ] && [ -z "$out" ]; then
  pass "an (only if) entry passes when it is in the diff"
else
  bad "an (only if) entry in the diff failed (got: $out)"
fi

# The committed lists are well formed: the files exist, and no entry is both fork-only
# and an upstream edit.
if [ -s "$EDITS" ] && [ -s "$FORK_ONLY" ]; then
  pass "both list files exist"
else
  bad "scripts/aws/upstream-edits.txt or scripts/aws/fork-only-paths.txt is missing or empty"
fi
overlap="$(list_entries "$EDITS" | sort | comm -12 - <(list_entries "$FORK_ONLY" | sort))"
if [ -z "$overlap" ]; then
  pass "no path is both fork-only and an upstream edit"
else
  bad "paths in both lists: $overlap"
fi

# The real repo, against the local upstream/main ref. Offline runs without the ref skip.
if git -C "$REPO_ROOT" rev-parse --verify --quiet refs/remotes/upstream/main >/dev/null; then
  out="$(check_edits "$REPO_ROOT" upstream/main "$EDITS" "$FORK_ONLY")"
  if [ $? -eq 0 ]; then
    pass "every file this branch changes against upstream/main is listed"
  else
    bad "this branch has upstream edits that are not listed or stale entries: $(printf '%s' "$out" | tr '\n' ';')"
  fi
else
  echo "skip - no upstream/main ref here, run 'git fetch upstream' to check the real diff"
fi

if [ "$fail" -ne 0 ]; then
  echo "test-upstream-edits: FAIL" >&2
  exit 1
fi
echo "test-upstream-edits: PASS"
