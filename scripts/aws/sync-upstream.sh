#!/usr/bin/env bash
# Merges upstream/main into the current branch. Never pushes. A conflict stops the merge,
# except one confined to the lock files (Cargo.lock, mobile/pubspec.lock): those are
# regenerated from the merged manifests instead of resolved by hand.
# Env: UPSTREAM_URL (default https://github.com/block/buzz.git).
set -euo pipefail

UPSTREAM_URL="${UPSTREAM_URL:-https://github.com/block/buzz.git}"

die() { echo "sync-upstream: $*" >&2; exit 1; }

# regenerate_lock <file>: take upstream's copy, then let the package manager add what this
# fork's manifests need. Runs from the repository root.
regenerate_lock() {
  case "$1" in
    Cargo.lock) git checkout --theirs -- Cargo.lock && cargo metadata --format-version 1 >/dev/null ;;
    mobile/pubspec.lock) git checkout --theirs -- mobile/pubspec.lock && (cd mobile && flutter pub get) ;;
  esac
}

git rev-parse --is-inside-work-tree >/dev/null 2>&1 || die "not inside a git repository"

cd "$(git rev-parse --show-toplevel)"

branch="$(git symbolic-ref --quiet --short HEAD || true)"
[ -n "$branch" ] || die "HEAD is detached. Check out a branch first."

# Untracked files are ignored on purpose: git refuses a merge that would overwrite one.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
  die "uncommitted changes in the working tree. Commit or stash them first."
fi

if git remote get-url upstream >/dev/null 2>&1; then
  current_url="$(git remote get-url upstream)"
  if [ "$current_url" != "$UPSTREAM_URL" ]; then
    echo "sync-upstream: remote 'upstream' already exists with a different URL." >&2
    echo "  existing: $current_url" >&2
    echo "  expected: $UPSTREAM_URL" >&2
    exit 1
  fi
else
  git remote add upstream "$UPSTREAM_URL"
  echo "Added remote 'upstream' ($UPSTREAM_URL)."
fi

git fetch upstream
git rev-parse --verify --quiet upstream/main >/dev/null || die "upstream/main not found after fetch"

if git merge-base --is-ancestor upstream/main HEAD; then
  echo "$branch is already up to date with upstream/main."
  exit 0
fi

count="$(git rev-list --count HEAD..upstream/main)"

if ! merge_out="$(git merge --no-edit upstream/main 2>&1)"; then
  echo "$merge_out" >&2
  if [ -f "$(git rev-parse --git-path MERGE_HEAD)" ]; then
    conflicts="$(git diff --name-only --diff-filter=U)"
    if [ -n "$conflicts" ] \
      && [ -z "$(printf '%s\n' "$conflicts" | grep -vxF -e Cargo.lock -e mobile/pubspec.lock)" ]; then
      echo "sync-upstream: only lock files conflict. Regenerating them:" >&2
      for lock in $conflicts; do
        echo "  $lock" >&2
        if ! regenerate_lock "$lock" >&2; then
          echo "sync-upstream: could not regenerate $lock. The merge is left in place." >&2
          echo "Fix the cause and regenerate it, then 'git add' it and run 'git commit', or run 'git merge --abort'." >&2
          exit 1
        fi
        git add -- "$lock"
      done
      git commit -q --no-edit || die "the merge commit failed after the lock files were regenerated"
      echo "Merged $count commit(s) from upstream/main into $branch, with the lock files regenerated. Nothing was pushed."
      echo "To publish: git push origin $branch"
      exit 0
    elif [ -n "$conflicts" ]; then
      echo "sync-upstream: merge stopped on conflicts. Conflicting files:" >&2
      printf '%s\n' "$conflicts" | sed 's/^/  /' >&2
      echo "Resolve them, then run 'git commit', or run 'git merge --abort' to undo." >&2
    else
      echo "sync-upstream: merge stopped with no conflicting files (for example a failing hook)." >&2
      echo "The merge is left in place. Fix the cause, then run 'git commit', or run 'git merge --abort' to undo." >&2
    fi
  else
    echo "sync-upstream: merge failed before it started." >&2
  fi
  exit 1
fi

echo "$merge_out"
echo "Merged $count commit(s) from upstream/main into $branch. Nothing was pushed."
echo "To publish: git push origin $branch"
