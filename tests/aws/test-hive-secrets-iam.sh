#!/usr/bin/env bash
# Checks the text of the secret store policies in deploy/aws/iam: the engine may only put, the
# supervisor may only get, and both stay inside the two secret prefixes.
set -uo pipefail
# shellcheck source=tests/aws/helpers.sh
. "$(dirname "${BASH_SOURCE[0]}")/helpers.sh"


# actions <file>: the allowed actions, one per line. resources <file>: likewise.
actions() { python3 -I -c 'import json,sys
for s in json.load(open(sys.argv[1]))["Statement"]:
    assert s["Effect"] == "Allow", s
    print("\n".join(s["Action"]))' "$1"; }
resources() { python3 -I -c 'import json,sys
for s in json.load(open(sys.argv[1]))["Statement"]:
    print("\n".join(s["Resource"]))' "$1"; }

# Each scenario takes the folder of the policies, so a mutant copy can be checked too.
scenario_engine_may_only_put() {
  local IAM="$1"
  [ "$(actions "$IAM/hive-engine-secrets.json")" = "ssm:PutParameter" ]
}

scenario_supervisor_may_only_get() {
  local IAM="$1"
  [ "$(actions "$IAM/hive-supervisor-secrets.json")" = "ssm:GetParameter" ]
}

scenario_both_stay_inside_the_secret_prefixes() {
  local IAM="$1" file res
  for file in hive-engine-secrets.json hive-supervisor-secrets.json; do
    res="$(resources "$IAM/$file")"
    [ -n "$res" ] || return 1
    while IFS= read -r r; do
      case "$r" in
        arn:aws:ssm:*:*:parameter/buzz/agents/*/env/* | arn:aws:ssm:*:*:parameter/buzz/team/env/*) ;;
        *) echo "$file: $r" >&2; return 1 ;;
      esac
    done <<< "$res"
  done
}

# The shared instance role grants Get on /buzz/* and Put on /buzz/agents/*. A team secret
# needs a Put statement of its own, or the engine's write is refused in production.
scenario_the_instance_role_may_put_team_secrets() {
  grep -q 'parameter/buzz/team/env/\*' "$REPO_ROOT/infra/iam.tf"
}

mutant() { # <sed-expr> <file>: a copy of the policies with one edit
  local dir
  dir="$(mktemp -d "$WORK/iam.XXXXXX")"
  cp "$REPO_ROOT"/deploy/aws/iam/*.json "$dir/"
  sed -i.bak -e "$1" "$dir/$2" && rm -f "$dir/$2.bak"
  printf '%s' "$dir"
}

IAM="$REPO_ROOT/deploy/aws/iam"
expect_ok "the engine policy allows put and nothing else" scenario_engine_may_only_put "$IAM"
expect_ok "the supervisor policy allows get and nothing else" scenario_supervisor_may_only_get "$IAM"
expect_ok "both policies stay inside the agent and team secret prefixes" scenario_both_stay_inside_the_secret_prefixes "$IAM"
expect_ok "the instance role may put team secrets" scenario_the_instance_role_may_put_team_secrets "$IAM"

# The checks can fail: the engine given a read, the supervisor given a write, a wider prefix.
if scenario_engine_may_only_put "$(mutant 's/"ssm:PutParameter"\]/"ssm:PutParameter", "ssm:GetParameter"]/' hive-engine-secrets.json)"; then bad "mutant survives: engine may get"; else pass "mutant fails: engine may get"; fi
if scenario_supervisor_may_only_get "$(mutant 's/"ssm:GetParameter"\]/"ssm:GetParameter", "ssm:PutParameter"]/' hive-supervisor-secrets.json)"; then bad "mutant survives: supervisor may put"; else pass "mutant fails: supervisor may put"; fi
if scenario_both_stay_inside_the_secret_prefixes "$(mutant 's#parameter/buzz/team/env/\*#parameter/buzz/*#' hive-engine-secrets.json)"; then bad "mutant survives: wider prefix"; else pass "mutant fails: wider prefix"; fi

exit "$fail"
