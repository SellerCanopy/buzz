# Secret store access for Beehive

Beehive's agent secrets and team secrets live in SSM Parameter Store, SecureString, under
`/buzz/agents/<agent>/env/<VAR>` and `/buzz/team/env/<VAR>`.

- `hive-engine-secrets.json` is what the `buzz-hive` engine may do: put, and nothing else. It
  cannot get, list or describe a parameter, so no engine code path can read a value back.
- `hive-supervisor-secrets.json` is what the host supervisor may do: get, and nothing else.
  It fills each agent's environment from these parameters.

Both name only the two prefixes above. `tests/aws/test-hive-secrets-iam.sh` checks the text.

The engine and the supervisor run on one instance today, so they share the instance role
(`infra/iam.tf`) and the separation is policy text until the engine container is denied the
instance metadata service or runs under its own role. Attach each file to its own role when
they are split.
