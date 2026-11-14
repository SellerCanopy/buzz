# Port ledger: Carrier (Go) to Beehive (Rust)

Every Go package with tests, every `app/e2e` spec and every named feature in the
Go engine has one row. Columns: `item | decision | phase`. A decision is
`ported`, `changed`, `dropped` or `pending`. Each phase updates the rows it
settles. Phase 27 closes the ledger: no row may stay `pending`.

The Go engine (`SellerCanopy/kiln`, directory `engine/`) is the reference and is
never edited. Phase numbers refer to the Beehive plan.

## Features

| item | decision | phase |
|---|---|---|
| ACP adapter | dropped (stages run on Buzz agents) | 1 |
| `mcptools` | dropped | 1 |
| Local runner | dropped | 1 |
| `gitwork` | dropped | 1 |
| Scratch folders | dropped | 1 |
| Usage reporter | dropped (usage counts runs and cards only) | 1 |
| Cost ledger | dropped | 1 |
| Pricing | dropped | 1 |
| `fakebeacon` | dropped | 1 |
| Keyring | dropped (secrets are write-only rows, Phase 23) | 23 |
| The five sign-in procedures | dropped (OAuth sign-in is Phase 24) | 23 |
| Sign-in modes | dropped (a Buzz key signs in) | 1 |
| Cookie session | dropped | 1 |
| Dev issuer | dropped | 1 |
| `apiVersion` header | dropped | 1 |
| Update feed | dropped | 1 |
| Engine lease and heartbeat | dropped (one `buzz-hive` per community) | 1 |
| Permission prompts | dropped | 1 |
| The `interrupted` inbox item | dropped | 1 |
| `core/openspec` | pending | 5 |
| `core/issuefiles` | pending | 16 |
| `core/config` | ported | 6 |

## Go test directories

Counts are test files per directory in the kiln checkout when this ledger was
seeded. A directory is `pending` until the phase that ports it names it.

| item (`engine/...`) | test files | decision | phase |
|---|---|---|---|
| `adapters/acp` | 9 | dropped | 1 |
| `adapters/actions` | 1 | pending | 18 |
| `adapters/artifacts/local` | 2 | pending | 9 |
| `adapters/downloadmark` | 3 | pending | unassigned |
| `adapters/github` | 20 | pending | 16 |
| `adapters/gitwork` | 5 | dropped | 1 |
| `adapters/keyring` | 1 | dropped | 23 |
| `adapters/localrepo` | 2 | dropped (scratch folders) | 1 |
| `adapters/mcptools` | 6 | dropped | 1 |
| `adapters/notify` | 1 | pending | unassigned |
| `adapters/runner/local` | 2 | dropped | 1 |
| `adapters/runner/remote` | 1 | dropped | 1 |
| `adapters/runner/wire` | 1 | dropped | 1 |
| `adapters/secrets/sqlcrypt` | 1 | pending | 23 |
| `adapters/signin/devissuer` | 1 | dropped | 1 |
| `adapters/signin/issuerclient` | 1 | dropped | 1 |
| `adapters/signin/jwt` | 1 | dropped | 1 |
| `adapters/sqlite` | 28 | pending | unassigned |
| `adapters/testenv` | 1 | pending | unassigned |
| `adapters/usage` | 8 | dropped | 1 |
| `adapters/webfetch` | 1 | pending | unassigned |
| `api` | 23 | pending | unassigned |
| `cmd/carrier-runner` | 1 | dropped (local runner) | 1 |
| `cmd/kiln-engine` | 88 | pending | unassigned |
| `core/assistant` | 15 | pending | 20 |
| `core/background` | 9 | pending | unassigned |
| `core/config` | 1 | ported | 6 |
| `core/domain` | 12 | pending | unassigned |
| `core/events` | 1 | changed (Phase 4: `domain/events.rs`, `bus.rs` and the `hive.events` log; one Postgres transaction per change and a long poll replace the Go subscription) | 4 |
| `core/events/schema` | 1 | changed (Phase 4: `EVENT_KINDS` and `check_new` refuse an unlisted kind or a payload over 64 KiB; each later phase registers the kinds it emits) | 4 |
| `core/issuefiles` | 7 | pending | 16 |
| `core/kinds` | 8 | pending | unassigned |
| `core/openspec` | 1 | pending | 5 |
| `core/orchestrator` | 56 | pending | unassigned |
| `core/pricing` | 1 | dropped | 1 |
| `core/pullrequests` | 2 | pending | 17 |
| `core/redact` | 1 | pending | unassigned |
| `core/rules` | 21 | pending | unassigned |
| `core/session` | 18 | pending | unassigned |
| `core/workflow` | 13 | pending | 5 |
| `core/workflow/schema` | 1 | pending | 5 |
| `internal/fakeagent` | 2 | pending | unassigned |
| `internal/ghreplay` | 1 | pending | 16 |
| `internal/gitconfig` | 1 | pending | unassigned |
| `internal/proc` | 3 | pending | unassigned |
| `internal/workflows` | 4 | pending | 5 |
| `ports` | 4 | pending | unassigned |
| `runnerd` | 1 | dropped (local runner) | 1 |

## `app/e2e` specs

The plan names the phase for `board`, `card`, `archive`, `list`, `browse`,
`guide`, `start`, `needs-you`, `usage`, `activity`, `permissions`, `merge`,
`assistant` and the dropped group. The other phases are assigned here from each
spec's title and are confirmed when that phase starts.

| item (`app/e2e/<name>.spec.ts`) | decision | phase |
|---|---|---|
| `actions` | pending | 11 |
| `activity` | pending | 12 |
| `answers` | pending | 12 |
| `archive` | changed (Phase 4 ports the contract in buzz-app `e2e/archive.spec.mjs`: archive leaves the board, the archive lists it, Restore returns it to Backlog. The Go spec drags a card onto Done; drag and drop returns with the phase that moves cards between columns) | 4 |
| `artifacts` | pending | 9 |
| `assistant` | pending | 20 |
| `auto-mode` | pending | 13 |
| `board` | changed (Phase 3 ports the empty board against a real `buzz-hive`, in buzz-app `src/features/beehive/e2e/board.spec.mjs`; the drag, move and label-suggestion tests return in the phases that add cards, stages and intake) | 3 |
| `boot` | pending | 3 |
| `browse` | pending | 5 |
| `builder-run` | pending | 19 |
| `builder` | pending | 19 |
| `calls` | pending | 12 |
| `card` | changed (Phase 4 ports adding a card, reading its details and a card added elsewhere reaching an open board, in buzz-app `e2e/card.spec.mjs`. Live, History, Usage and the stage controls return with runs and stages) | 4 |
| `costs-pull-requests` | dropped (cost figures) | 1 |
| `costs` | dropped (cost figures) | 1 |
| `files` | pending | 9 |
| `filters` | pending (filters narrow by run state and search, which the Phase 4 board does not have; it needs runs) | 6 |
| `guide` | pending | 5 |
| `home` | pending | 3 |
| `importexport` | pending | 19 |
| `kilntask` | dropped (Carrier task kind, no Buzz stage) | 1 |
| `list` | pending | 5 |
| `login` | dropped (team login and cookie session) | 1 |
| `merge` | pending | 18 |
| `needs-you` | pending | 12 |
| `newtask` | pending | 16 |
| `parameters` | pending | 6 |
| `paused-turn` | pending | 12 |
| `permissions` | pending | 14 |
| `pick-repo` | pending | 21 |
| `plugin-command` | dropped (plugins) | 1 |
| `plugin-contents` | dropped (plugins) | 1 |
| `prompt-skill` | dropped (prompt skills) | 1 |
| `remove` | pending | 4 |
| `repos` | pending | 21 |
| `reset` | pending | 17 |
| `runners` | dropped (runner pairing, stages run on Buzz agents) | 1 |
| `select` | pending | 5 |
| `session` | pending | 3 |
| `sidebar` | pending | 3 |
| `signin-token` | dropped (sign-in modes) | 1 |
| `signin` | dropped (sign-in modes) | 1 |
| `skills` | dropped | 1 |
| `sources-layout` | pending | 16 |
| `sources-repo-rule` | pending | 16 |
| `stack-seed` | pending | 16 |
| `start` | pending | 6 |
| `testrun` | dropped (`StartTestRun`) | 1 |
| `usage` | pending | 12 |
| `web` | dropped (token screen, sign-in modes) | 1 |
| `workflow-options` | pending | 19 |

## Phase 3 deliverables

- Proto: `crates/buzz-hive-api/proto/kiln/engine/v1/engine.proto` is kiln `main`
  (89 rpcs) at `proto/kiln/engine/v1/engine.proto`, plus three additive fields:
  `GetStatusResponse.community_id = 6`, `WhoAmIResponse.kind = 6` and
  `WhoAmIResponse.owner = 7`.
- Served: `GetStatus`, `WhoAmI` and `ListCards`. `tests/procedures.rs` fails when
  an rpc is in none of `ROUTES`, `DROPPED` and `PENDING`, or in two of them.
- Store: `hive.cards` (migration `0002_cards.sql`) and `CardsRepo` (`list`, `get`).
  `Card.id` is `int64` in the proto and `uuid` in the table, so `ListCards` sends
  `id: 0` until Phase 4 settles it.
- buzz-app: `src/features/beehive/` (types from this proto, client, session,
  dock page, offline fixture and journey), and the `beehive_authorization` and
  `beehive_request` desktop commands.

## Phase 4 deliverables

- Proto ids: `Card.id`, `Event.card_id`, `Event.run_id`, `GetCardRequest.card_id`,
  `RemoveCardRequest.card_id`, `RestoreCardRequest.card_id`,
  `AddNoteRequest.card_id`, `ArchiveCardsRequest.card_ids` and
  `ActivityEntry.card_id` and `run_id` are string UUIDs (the Go engine used
  `int64`). `Run.id`, `Run.card_id` and the other request `card_id` fields stay
  `int64` until the phase that serves them.
- Additive proto fields: `Event.actor_pubkey = 7`, `Event.actor_kind = 8`,
  `CreateCardRequest.title = 4` and `description = 5`, and the rpc `PollEvents`.
- Served: `GetCard`, `CreateCard`, `CreateTask`, `AddNote`, `RemoveCard`,
  `ArchiveCards`, `RestoreCard` and `PollEvents`. `WatchEvents` stays dropped.
- Store: `hive.events` (migration `0003_events.sql`); appends take
  `pg_advisory_xact_lock(hashtext('hive.events'))` so seq order equals commit order.
- Events: `card.created`, `card.moved`, `card.state`, `card.archived`,
  `card.restored`, `card.removed` and `activity.added` (registered now because
  `AddNote` appends it). `tests/event_coverage.rs` fails when a registered kind
  for the current phase is never emitted; each phase bumps `CURRENT_PHASE`.
- buzz-app: `engine/poll.ts`, `engine/useLive.ts`, the board with an archive
  view, the card page as an addressable visit, and the New card dialog.
