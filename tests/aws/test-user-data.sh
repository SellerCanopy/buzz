#!/usr/bin/env bash
# Tests for the first-boot template infra/user-data.sh.tftpl. Renders it with
# placeholder values (pure bash, no terraform) and checks the result.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"
export REPO_ROOT

TEMPLATE="$REPO_ROOT/infra/user-data.sh.tftpl"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

VOLUME_ID="vol-0abc123def4567890"
VERSION="9.9.9"
SHA="$(printf 'c%.0s' $(seq 1 64))"

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

if [ ! -f "$TEMPLATE" ]; then
  echo "FAIL - infra/user-data.sh.tftpl not found"
  exit 1
fi

# render <out-file>: substitutes the three template variables.
render() {
  local text
  text="$(cat "$TEMPLATE")"
  text="${text//'${data_volume_id}'/$VOLUME_ID}"
  text="${text//'${compose_version}'/$VERSION}"
  text="${text//'${compose_sha256}'/$SHA}"
  printf '%s\n' "$text" > "$1"
}

# first_line <regex> <file>: line number of the first match, empty if none.
first_line() { grep -nE -- "$1" "$2" | head -n 1 | cut -d: -f1; }

# check <name> <expect: 0|1> <function> <file>: runs a check and compares its result.
check() {
  local name="$1" expect="$2" fn="$3" file="$4"
  "$fn" "$file" > /dev/null 2>&1
  if [ "$?" -eq "$expect" ]; then pass "$name"; else bad "$name"; fi
}

template_has_variables() {
  grep -qF '${data_volume_id}' "$1" && grep -qF '${compose_version}' "$1" \
    && grep -qF '${compose_sha256}' "$1"
}
has_no_markers() { ! grep -qE '%\{|\$\{' "$1"; }
has_strict_mode() { grep -qxF 'set -euo pipefail' "$1"; }
has_shebang() { [ "$(head -n 1 "$1")" = '#!/usr/bin/env bash' ]; }

# Every mkfs call must sit directly under an "if ! blkid" guard.
mkfs_is_guarded() {
  local f="$1" n prev
  grep -q 'mkfs' "$f" || return 1
  while IFS=: read -r n _; do
    prev="$(head -n "$((n - 1))" "$f" | grep -vE '^\s*(#|$)' | tail -n 1)"
    printf '%s' "$prev" | grep -qE '^\s*if ! blkid .*; then$' || return 1
  done < <(grep -n 'mkfs' "$f")
}

# The mount must come before the first line that starts or enables Docker.
mounts_before_docker_start() {
  local f="$1" m d
  m="$(first_line '^\s*mount "\$DOCKER_DIR"' "$f")"
  d="$(first_line 'systemctl (enable|start|restart).*docker' "$f")"
  [ -n "$m" ] && [ -n "$d" ] && [ "$m" -lt "$d" ]
}

enables_docker_now() { grep -qE 'systemctl enable --now docker$' "$1"; }

# The device is found by its by-id link, never a raw device name.
uses_by_id_link() {
  grep -qF '/dev/disk/by-id/nvme-Amazon_Elastic_Block_Store_' "$1" \
    && ! grep -vE '^\s*#' "$1" | grep -qE '/dev/(nvme[0-9]|sd[a-z]|xvd)'
}

# The fstab entry uses the UUID and nofail.
fstab_uses_uuid_nofail() { grep -qE 'echo "UUID=\$uuid \$DOCKER_DIR ext4 defaults,nofail ' "$1"; }

# The volume wait loop gives up with an error.
wait_is_bounded() { grep -qE 'did not appear' "$1" && grep -qE '^\s*exit 1$' "$1"; }

starts_no_compose_stack() { ! grep -qE 'docker (compose|-compose)|docker-compose (up|start)|compose up' "$1"; }

# The pinned Compose binary is verified against its checksum before install.
verifies_checksum_before_install() {
  local f="$1" s i
  s="$(first_line 'sha256sum -c -$' "$f")"
  i="$(first_line 'install -D -m 0755 "\$tmp"' "$f")"
  [ -n "$s" ] && [ -n "$i" ] && [ "$s" -lt "$i" ]
}

creates_opt_buzz() { grep -qE 'install -d -m 0755 -o root -g root /opt/buzz$' "$1"; }
logs_to_file() { grep -qF '/var/log/buzz-first-boot.log' "$1"; }

OUT="$WORK/rendered.sh"
render "$OUT"

check "rendered text has no template markers"          0 has_no_markers "$OUT"
check "raw template carries its three variables"       0 template_has_variables "$TEMPLATE"
check "rendered text starts with the bash shebang"     0 has_shebang "$OUT"
check "rendered text uses set -euo pipefail"           0 has_strict_mode "$OUT"
check "mkfs only under an if ! blkid guard"            0 mkfs_is_guarded "$OUT"
check "mounts before Docker is enabled"                0 mounts_before_docker_start "$OUT"
check "enables and starts Docker"                      0 enables_docker_now "$OUT"
check "finds the disk by its by-id link"               0 uses_by_id_link "$OUT"
check "fstab entry uses UUID and nofail"               0 fstab_uses_uuid_nofail "$OUT"
check "volume wait is bounded and fails clearly"       0 wait_is_bounded "$OUT"
check "starts no Compose stack"                        0 starts_no_compose_stack "$OUT"
check "checks the Compose checksum before install"     0 verifies_checksum_before_install "$OUT"
check "creates /opt/buzz owned by root, mode 0755"     0 creates_opt_buzz "$OUT"
check "logs to /var/log/buzz-first-boot.log"           0 logs_to_file "$OUT"

if bash -n "$OUT" 2> "$WORK/syntax.err"; then
  pass "rendered text passes bash -n"
else
  bad "rendered text passes bash -n ($(cat "$WORK/syntax.err"))"
fi

if grep -qF "$VOLUME_ID" "$OUT" && grep -qF "$VERSION" "$OUT" && grep -qF "$SHA" "$OUT"; then
  pass "placeholder values reach the rendered text"
else
  bad "placeholder values reach the rendered text"
fi

# Falsifiability: each check must fail on a script that breaks its rule.
mutate() { # <name> <check-fn> <sed-expr>: applies the edit, expects the check to fail.
  local name="$1" fn="$2" expr="$3" m="$WORK/mutant.sh"
  sed -E "$expr" "$OUT" > "$m"
  if cmp -s "$OUT" "$m"; then bad "mutation '$name' changed nothing"; return; fi
  check "mutant fails: $name" 1 "$fn" "$m"
}

mutate "guard removed from mkfs" mkfs_is_guarded 's/^if ! blkid .*; then$/if true; then/'
mutate "unguarded mkfs added" mkfs_is_guarded 's/^mkdir -p "\$DOCKER_DIR"$/mkfs.ext4 "$DEVICE"\nmkdir -p "$DOCKER_DIR"/'
mutate "docker enabled before mount" mounts_before_docker_start 's/^mkdir -p "\$DOCKER_DIR"$/systemctl enable --now docker\nmkdir -p "$DOCKER_DIR"/'
mutate "strict mode dropped" has_strict_mode 's/^set -euo pipefail$/set -e/'
mutate "raw device name used" uses_by_id_link 's#^DEVICE=.*#DEVICE=/dev/nvme1n1#'
mutate "nofail dropped" fstab_uses_uuid_nofail 's/defaults,nofail/defaults/'
mutate "checksum check removed" verifies_checksum_before_install 's/sha256sum -c -$/true/'
mutate "compose stack started" starts_no_compose_stack 's#^install -d -m 0755 -o root -g root /opt/buzz$#docker compose up -d#'
mutate "template marker left" has_no_markers 's/^COMPOSE_VERSION=.*/COMPOSE_VERSION="${compose_version}"/'

exit "$fail"
