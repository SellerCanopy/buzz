#!/bin/sh
# Stands in for `docker compose exec -T agent <argv>`: run as `<agent> claude <args...>`, it
# runs the stub CLI next to it. The supervisor's tests point BUZZ_HIVE_SUPERVISOR_EXEC_CMD here
# (as Exec::Command). Nothing here calls Docker.
set -eu
here=$(cd "$(dirname "$0")" && pwd)
[ $# -ge 2 ] || exit 2
agent=$1
[ "$2" = claude ] || { echo "exec-stub: only claude runs here" >&2; exit 2; }
shift 2
STUB_STATE="$here/stub-state"
export STUB_STATE
mkdir -p "$STUB_STATE"
exec "$here/claude-stub.sh" "$agent" "$@"
