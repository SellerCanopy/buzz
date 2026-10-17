#!/usr/bin/env bash
# Runs every tests/aws/test-*.sh and exits non-zero if any fails.
set -uo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
export REPO_ROOT

failed=0
for t in "$REPO_ROOT"/tests/aws/test-*.sh; do
  [ -e "$t" ] || continue
  echo "== $(basename "$t")"
  if ! bash "$t"; then
    echo "FAILED: $(basename "$t")" >&2
    failed=1
  fi
done

if [ "$failed" -ne 0 ]; then
  echo "tests/aws: FAIL" >&2
  exit 1
fi
echo "tests/aws: PASS"
