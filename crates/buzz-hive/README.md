# buzz-hive

The Beehive workflow engine service. It runs beside the Buzz relay, keeps its state in the
`hive` schema of the relay's Postgres, and posts to the relay as its own account.

## Settings

| Variable | Needed | Meaning |
| --- | --- | --- |
| `BUZZ_HIVE_DATABASE_URL` | yes | Postgres URL of the `buzz_hive` role |
| `BUZZ_HIVE_PUBLIC_URL` | yes | The origin clients reach, no trailing slash |
| `BUZZ_HIVE_TOKEN_SECRET` | yes | At least 32 bytes, signs the 15 minute tokens |
| `BUZZ_HIVE_COMMUNITY_HOST` | yes | The community this instance serves |
| `BUZZ_HIVE_NOSTR_KEY` | to start runs | The Beehive account's secret key, `nsec1...` or 64 hex characters |
| `BUZZ_HIVE_RELAY_URL` | with the key | The relay's address, such as `ws://relay:3000` |
| `BUZZ_HIVE_LISTEN`, `BUZZ_HIVE_ALLOWED_ORIGINS`, `BUZZ_HIVE_WORKFLOWS_DIR` | no | See `src/config.rs` |

Without `BUZZ_HIVE_NOSTR_KEY` the service runs, and starting a run or saving the update
channel is refused with a message that names the variable.

## Starting a run

1. Add the Beehive account to a channel in the community, and every agent a workflow names
   in `agent:` to the same channel.
2. In Beehive Settings, choose that channel as the update channel. Saving checks that the
   Beehive account is in it.
3. Start a run on a card. The service queues two posts in one transaction with the run: a
   status line for the card, and a top-level message that mentions the first stage's agent
   and holds the task. It sends them to the relay in the background and retries a failed
   post with the same signed event, so the relay keeps one message.

## Stage agents answer the Beehive account by hand for now

A Buzz agent answers only its owner by default. Until the agent setup in Phase 22 applies
this, set it by hand in the environment of each agent a workflow names:

```sh
BUZZ_ACP_RESPOND_TO=allowlist
BUZZ_ACP_RESPOND_TO_ALLOWLIST=<the Beehive account's pubkey, 64 hex characters>
BUZZ_ACP_SESSION_POLICY=thread
```

The Beehive account's pubkey is logged at startup as `account`.

## Checking against a local relay

`tests/e2e_start.rs` starts a run against a running relay and reads the two posts back. See
the top of that file for the command.
