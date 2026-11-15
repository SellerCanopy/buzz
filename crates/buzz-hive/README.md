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

## Ending a stage

The stage task names the card and the results the stage accepts. The agent ends the stage
with the `stage_done` tool of `buzz-hive-mcp`, a stdio MCP server in `crates/buzz-hive-mcp`.
Only the agent the task mentioned may report. The workflow routes the result: to another
stage, which posts its own task, to Done, to stopped, or to a wait for a person. A `passed`
from the workflow's last stage ends the run and moves the card to Done. A call for a card
whose run is not waiting on a result is refused, not applied twice.

The call carries the card id, the stage and the visit, all three as the task gave them. A
call for any other stage or visit is refused with `failed_precondition`, so a late answer to
an earlier visit of a stage cannot move the card on from a later one. The same holds for
`move_card`. Holding, resuming and skipping a card are a person's: an agent calling them gets
`permission_denied`, and they are not tools.

Add the server to the agent's MCP servers, with the command `buzz-hive-mcp`. It reads
`BUZZ_PRIVATE_KEY` and `BUZZ_RELAY_URL`, which the agent runtime already sets, signs in as
the agent, and finds Beehive on the relay's host. Set `BUZZ_HIVE_URL` where Beehive is on
another address, such as `http://localhost:3100` for a local service. The agent's Beehive
level (set by an admin, never above its owner's) decides which tools it may call.

## Checking against a local relay

`tests/e2e_start.rs` starts a run against a running relay and reads the two posts back.
`tests/e2e_one_stage.rs` has a scripted agent create, start and finish a card through the
real `buzz-hive-mcp` binary. See the top of each file for the command.
