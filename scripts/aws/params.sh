#!/usr/bin/env bash
# Shared SSM Parameter Store helpers. Source this file; do not run it.
# Like lib.sh, it sets no shell options. The caller runs with `set -euo pipefail`
# and has sourced lib.sh (for die).

# parameter_exists <name>: returns 0 if the parameter exists and 1 if it does
# not. Exits the script on any other error, so a denied read is never taken for
# "absent". The value is never read.
parameter_exists() {
  local err rc=0
  err="$(aws ssm get-parameter --name "$1" --query Parameter.Name --output text 2>&1 >/dev/null)" || rc=$?
  [ "$rc" -eq 0 ] && return 0
  case "$err" in
    *ParameterNotFound*) return 1 ;;
    *) die "cannot check $1: $err" ;;
  esac
}
