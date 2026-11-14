#!/usr/bin/env bash
# Tests deploy/aws/compose.hive.yml and Caddyfile.hive by rendering the three compose
# files with the real "docker compose config" and dummy env files. Nothing is pulled,
# built, or started. Skips (exit 0) when docker compose or jq is unavailable. It does
# not source helpers.sh because that puts a fake docker first on PATH.
set -uo pipefail

REPO_ROOT="${REPO_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)}"

for _v in $(compgen -v | grep -E '^(AWS_|TF_|GIT_|BUZZ_|RELAY_|POSTGRES_|REDIS_|COMPOSE_|DOCKER_)'); do
  unset "$_v"
done
unset _v

if ! docker compose version >/dev/null 2>&1; then
  echo "skip - docker compose is not available"
  exit 0
fi
if ! command -v jq >/dev/null 2>&1; then
  echo "skip - jq is not available"
  exit 0
fi

# pwd -P: Compose reports resolved paths, and macOS temp directories sit behind a symlink.
WORK="$(cd "$(mktemp -d)" && pwd -P)"
trap 'rm -rf "$WORK"' EXIT

fail=0
pass() { echo "ok   - $1"; }
bad() { echo "FAIL - $1"; fail=1; }

# stage <name>: a copy of deploy/ under $WORK/<name> with dummy .env and .env.hive
# files, so Compose resolves every relative path the way it does on the server.
stage() {
  local d="$WORK/$1"
  mkdir -p "$d"
  cp -R "$REPO_ROOT/deploy" "$d/deploy"
  {
    echo "POSTGRES_PASSWORD=pgpass"
    echo "REDIS_PASSWORD=redispass"
    echo "BUZZ_S3_ACCESS_KEY=access"
    echo "BUZZ_S3_SECRET_KEY=secret"
    echo "BUZZ_DOMAIN=buzz.example.com"
  } > "$d/deploy/compose/.env"
  {
    echo "BUZZ_HIVE_DATABASE_URL=postgres://buzz_hive:hivepass@postgres:5432/buzz"
    echo "BUZZ_HIVE_DB_PASSWORD=hivepass"
  } > "$d/.env.hive"
  printf '%s' "$d"
}

# render <dir>: the rendered project as JSON on stdout.
render() {
  docker compose \
    -f "$1/deploy/compose/compose.yml" \
    -f "$1/deploy/compose/compose.caddy.yml" \
    -f "$1/deploy/aws/compose.hive.yml" \
    config --format json
}

# check_render <dir>: every rule must hold.
check_render() {
  local dir="$1" r caddyfile="$1/deploy/aws/Caddyfile.hive"
  r="$(render "$dir" 2> "$dir/render.err")" || { cat "$dir/render.err" >&2; return 1; }

  # Caddy serves the Beehive Caddyfile in place of the upstream one, read-only.
  jq -e --arg src "$dir/deploy/aws/Caddyfile.hive" \
    '.services.caddy.volumes[] | select(.target == "/etc/caddy/Caddyfile" and .source == $src and .read_only == true)' \
    <<< "$r" >/dev/null || return 1
  [ "$(jq '[.services.caddy.volumes[] | select(.target == "/etc/caddy/Caddyfile")] | length' <<< "$r")" = 1 ] || return 1
  # Caddy never waits for hive, so a hive failure cannot take the relay offline.
  jq -e '.services.caddy.depends_on | has("hive") | not' <<< "$r" >/dev/null || return 1

  # Workspace creation stays closed.
  [ "$(jq -r '.services.relay.environment.BUZZ_MULTI_COMMUNITY_RELAY' <<< "$r")" = "false" ] || return 1
  [ -z "$(jq -r '.services.relay.environment.RELAY_OPERATOR_PUBKEYS // ""' <<< "$r")" ] || return 1

  # The role setup runs after the relay migrated, and hive after the role setup.
  jq -e '.services["hive-db-init"].depends_on.relay.condition == "service_healthy"' <<< "$r" >/dev/null || return 1
  jq -e '.services.hive.depends_on["hive-db-init"].condition == "service_completed_successfully"' <<< "$r" >/dev/null || return 1
  [ "$(jq -r '.services["hive-db-init"].restart' <<< "$r")" = "no" ] || return 1
  jq -e --arg src "$dir/deploy/aws/hive-roles.sql" \
    '.services["hive-db-init"].volumes[] | select(.target == "/hive-roles.sql" and .source == $src and .read_only == true)' \
    <<< "$r" >/dev/null || return 1
  jq -e '.services["hive-db-init"].entrypoint | join(" ") | contains("hive_password=\"$$BUZZ_HIVE_DB_PASSWORD\"")' <<< "$r" >/dev/null || return 1

  # Both new services share the relay's network and take their settings from .env.hive.
  for s in hive hive-db-init; do
    jq -e --arg s "$s" '.services[$s].networks | has("buzz-net")' <<< "$r" >/dev/null || return 1
    jq -e --arg s "$s" '.services[$s].environment.BUZZ_HIVE_DB_PASSWORD == "hivepass"' <<< "$r" >/dev/null || return 1
  done
  jq -e '.services.hive.environment.BUZZ_HIVE_DATABASE_URL | startswith("postgres://buzz_hive:")' <<< "$r" >/dev/null || return 1
  [ "$(jq -r '.services.hive.environment.BUZZ_HIVE_LISTEN' <<< "$r")" = "0.0.0.0:3100" ] || return 1
  # hive is reached through Caddy only: no published port.
  [ "$(jq '.services.hive.ports // [] | length' <<< "$r")" = 0 ] || return 1
  [ "$(jq -r '.services.hive.build.dockerfile' <<< "$r")" = "deploy/aws/Dockerfile.hive" ] || return 1
  [ "$(jq -r '.services.hive.build.context' <<< "$r")" = "$dir" ] || return 1
  [ -f "$dir/deploy/aws/Dockerfile.hive" ] || return 1

  # The Caddyfile sends the Beehive paths to hive and everything else to the relay.
  grep -qE '^[[:space:]]*@hive path .*/auth/token' "$caddyfile" || return 1
  grep -qE '^[[:space:]]*reverse_proxy @hive hive:3100$' "$caddyfile" || return 1
  grep -qE '^[[:space:]]*reverse_proxy relay:3000$' "$caddyfile" || return 1
  grep -q 'kiln.engine.v1.EngineService/\*' "$caddyfile" || return 1
}

d="$(stage ok)"
if check_render "$d"; then
  pass "the three compose files render the Beehive rules"
else
  bad "the three compose files render the Beehive rules"
fi

# Each rule must be falsifiable: a copy without it must fail the check.
mutant() {
  local name="$1" file="$2" expr="$3" dir
  dir="$(stage "mutant-$name")"
  sed -E "$expr" "$REPO_ROOT/deploy/aws/$file" > "$dir/deploy/aws/$file"
  if cmp -s "$REPO_ROOT/deploy/aws/$file" "$dir/deploy/aws/$file"; then
    bad "mutation '$name' changed nothing"
    return
  fi
  if check_render "$dir" >/dev/null 2>&1; then bad "mutant survives: $name"; else pass "mutant fails: $name"; fi
}
mutant multi-community-true compose.hive.yml 's/BUZZ_MULTI_COMMUNITY_RELAY: "false"/BUZZ_MULTI_COMMUNITY_RELAY: "true"/'
mutant caddyfile-mount-removed compose.hive.yml 's#\.\./aws/Caddyfile\.hive:#../compose/Caddyfile:#'
mutant db-init-before-relay compose.hive.yml '/^  hive-db-init:/,/^  hive:/ s/condition: service_healthy/condition: service_started/'
mutant hive-before-roles compose.hive.yml 's/condition: service_completed_successfully/condition: service_started/'
mutant hive-port-published compose.hive.yml 's#^(      BUZZ_HIVE_LISTEN: .*)$#\1\n    ports: ["3100:3100"]#'
mutant hive-off-network compose.hive.yml '/^  hive:/,/^  caddy:|^  # Caddy/ s/^      - buzz-net$/      - default/'
mutant caddy-waits-for-hive compose.hive.yml 's#^  caddy:$#  caddy:\n    depends_on:\n      hive:\n        condition: service_started#'
mutant hive-paths-not-routed Caddyfile.hive 's#/auth/token ##'
mutant fallback-removed Caddyfile.hive 's#reverse_proxy relay:3000#reverse_proxy hive:3100#'

# Operators set in .env would open workspace creation, so the check must see them.
d="$(stage operators)"
echo "RELAY_OPERATOR_PUBKEYS=$(printf 'a%.0s' $(seq 1 64))" >> "$d/deploy/compose/.env"
if check_render "$d" >/dev/null 2>&1; then bad "mutant survives: operator pubkeys in .env"; else pass "mutant fails: operator pubkeys in .env"; fi

# A missing password fails the render instead of starting with an empty one.
d="$(stage no-password)"
grep -v '^POSTGRES_PASSWORD=' "$d/deploy/compose/.env" > "$d/env.tmp" && mv "$d/env.tmp" "$d/deploy/compose/.env"
if out="$(render "$d" 2>&1)"; then
  bad "render without POSTGRES_PASSWORD fails"
elif grep -q POSTGRES_PASSWORD <<< "$out"; then
  pass "render without POSTGRES_PASSWORD fails and names it"
else
  bad "render without POSTGRES_PASSWORD fails and names it (got: $out)"
fi

exit "$fail"
