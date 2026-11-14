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
| `core/openspec` | kept, not dropped: the built-in `openspec` package calls `check_openspec`. Phase 5 ports the action's argument checks (`workflow/actions.rs`) and ships the package with its `NOTICE` and `LICENSE-OpenSpec`. The document validator runs inside the action, so it ports with the action runner in Phase 18 | 18 |
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
| `core/openspec` | 1 | kept, not dropped (see Features): the validator ports with the action runner | 18 |
| `core/orchestrator` | 56 | pending | unassigned |
| `core/pricing` | 1 | dropped | 1 |
| `core/pullrequests` | 2 | pending | 17 |
| `core/redact` | 1 | pending | unassigned |
| `core/rules` | 21 | pending | unassigned |
| `core/session` | 18 | pending | unassigned |
| `core/workflow` | 13 | changed (Phase 5: load, strict decode, `check` and the template variables in `buzz-hive-core/src/workflow/`, tests in `tests/workflow_*.rs`. A stage names one Buzz agent, so the profile, permission, skill and conversation codes are gone and `agent_object_removed`, `conversation_removed`, `idle_timeout_removed` and `agent_unknown` are new. `archive`, `convert`, `encode`, `plugin`, `render`, `skill` and `skills` move with the builder and import in Phases 19 and 20) | 5 |
| `core/workflow/schema` | 1 | changed (Phase 5: `workflow.schema.json` is generated from the Rust types with schemars, the strict decoder reads it, and `tests/workflow_schema.rs` fails on drift) | 5 |
| `internal/fakeagent` | 2 | pending | unassigned |
| `internal/ghreplay` | 1 | pending | 16 |
| `internal/gitconfig` | 1 | pending | unassigned |
| `internal/proc` | 3 | pending | unassigned |
| `internal/workflows` | 4 | changed (Phase 5: the six built-in packages and five stage templates are embedded from `buzz-hive-core/workflows/` by `build.rs`; `tests/workflow_load.rs` loads and checks each one. Every stage names the agent `builder` until Phase 6 reads agents from Buzz) | 5 |
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
| `browse` | pending (the Go spec browses GitHub issues, which needs intake) | 16 |
| `builder-run` | pending | 19 |
| `builder` | pending | 19 |
| `calls` | pending | 12 |
| `card` | changed (Phase 4 ports adding a card, reading its details and a card added elsewhere reaching an open board, in buzz-app `e2e/card.spec.mjs`. Live, History, Usage and the stage controls return with runs and stages) | 4 |
| `costs-pull-requests` | dropped (cost figures) | 1 |
| `costs` | dropped (cost figures) | 1 |
| `files` | pending | 9 |
| `filters` | pending (filters narrow by run state and search; a run now starts, but a card leaves Running only when a stage ends, Phase 7) | 7 |
| `guide` | pending (the Go spec drives the workflow builder) | 19 |
| `home` | pending | 3 |
| `importexport` | pending | 19 |
| `kilntask` | dropped (Carrier task kind, no Buzz stage) | 1 |
| `list` | pending (the Go spec lists the board's cards with their run state; a card finishes only in Phase 7) | 7 |
| `login` | dropped (team login and cookie session) | 1 |
| `merge` | pending | 18 |
| `needs-you` | pending | 12 |
| `newtask` | pending | 16 |
| `parameters` | pending (`StartStage` refuses `parameters` until the run keeps them, Phase 8) | 8 |
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
| `start` | ported (Phase 6: an idle card with a workflow sits in To do, Start run posts the status and the first stage task, the card moves to Running; the third Go test, opening the Issue tab, waits for the card's later tabs) | 6 |
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

## Phase 5 deliverables

- The plan's "ported workflow-list specs" do not exist in the Go suite under those
  names: `list` is the board list, `browse` is the GitHub issue browser and `guide`
  is the workflow builder. Phase 5 adds a new spec instead, buzz-app
  `src/features/beehive/e2e/workflows.spec.mjs`, against a real `buzz-hive`: the
  Workflows page lists every built-in package with its stages, and a broken package
  shows its finding. The three Go specs moved to the phases above.
- Format: `Stage.kind` is a string, so `kind_missing` and `kind_unknown` stay checks
  (the plan had an enum). `with` values are `ActionArg` (a text or a list). Findings
  carry no `line N:` prefix because `serde_yaml` has no spans.
- Dropped codes: `conversation_not_allowed`, `conversation_mode_unknown`,
  `conversation_from_empty`, `setting_invalid`, `kind_unavailable` (all four kinds
  exist), the skill codes, `skills_path_forbidden`, `profile_*`, `mode_*` and
  `tools_allow_read_only`. `workspace_unknown` is `invalid_value` now.
- New codes: `agent_object_removed`, `conversation_removed`, `idle_timeout_removed`,
  `agent_unknown` (a prompt stage with no agent) and `agent_name_invalid`. Whether the
  named agent exists in Buzz arrives with Phase 6.
- Built-in packages: every old `agent:` mapping became `agent: builder`, and the
  `tools.allow`, `permission`, `model_tier` and `effort` values were not carried over.
  Set them on the agent in Buzz.
- Package files are UTF-8 text. A file that is not text makes its package
  `package_unreadable`.
- `has_plugin_content` and command lookup read only the `commands/` and `skills/`
  layout; manifest-declared hooks are not read.
- Store: `hive.workflow_packages`, `hive.workflow_versions` and `hive.workflow_options`
  (migration `0004_workflows.sql`) and `WorkflowsRepo`. The binary records the
  built-in packages by content hash on start. `workflow.options_changed` is registered
  with `SetWorkflowOptions` in Phase 19.
- Served: `ListWorkflows`, `CheckWorkflow`, `ListTemplateVariables` and
  `ListStageTemplates`, all at Read. Additive proto: `Workflow.stages = 14` and the
  message `WorkflowStage`. `BUZZ_HIVE_WORKFLOWS_DIR` adds a folder of packages beside
  the built-in ones. `CheckWorkflow` leaves `stage_calls` and `contents` to Phase 19.
- Not ported: `TestCheckListsThePackageContentsOnce` (an integration test cannot
  count the listing calls) and the Go tests for optional output YAML encoding (nothing
  stores the YAML form here; the hash is over the files).

