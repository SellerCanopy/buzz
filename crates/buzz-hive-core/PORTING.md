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
| `adapters/artifacts/local` | 2 | changed (documents live in the `hive.artifacts` tables of the relay's Postgres and a file is a pointer to a media-store blob, so there is no folder adapter; `postgres_artifacts.rs` and `tests/artifacts.rs` carry the contract) | 9 |
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
| `artifacts` | changed (Phase 9 ports the contract: save, versions, read, hand-off, the stage-declared outputs rule and the task message naming inputs, in `tests/artifacts.rs` and `buzz-hive-api/tests/artifacts.rs`. The Go spec's approval and image-review screens are not ported; buzz-app `e2e` carries the Files tab) | 9 |
| `assistant` | pending | 20 |
| `auto-mode` | pending | 13 |
| `board` | changed (Phase 3 ports the empty board against a real `buzz-hive`, in buzz-app `src/features/beehive/e2e/board.spec.mjs`; the drag, move and label-suggestion tests return in the phases that add cards, stages and intake) | 3 |
| `boot` | pending | 3 |
| `browse` | pending (the Go spec browses GitHub issues, which needs intake) | 16 |
| `builder-run` | pending | 19 |
| `builder` | pending | 19 |
| `calls` | pending (Phase 8 routes a result to a call's target and honors `then` after `passed`; `give` carries documents; Phase 9 did not port it, so it stays here; the spec's inbox and screens are Phase 12) | 12 |
| `card` | changed (Phase 4 ports adding a card, reading its details and a card added elsewhere reaching an open board, in buzz-app `e2e/card.spec.mjs`. Live, History, Usage and the stage controls return with runs and stages) | 4 |
| `costs-pull-requests` | dropped (cost figures) | 1 |
| `costs` | dropped (cost figures) | 1 |
| `files` | changed (the Files tab lists a card's saved files and documents with a download link, in buzz-app `beehive.journey.mjs` and `BeehivePage.test.tsx`; `buzz-hive/tests/e2e_files.rs` proves the file round trip through the relay's media store. Intake files stay Phase 16) | 9 |
| `filters` | pending (filters narrow by run state and search; a card now reaches Done, but the Stopped and Failed states the spec filters on arrive with StopRun and routing, Phase 11) | 11 |
| `guide` | pending (the Go spec drives the workflow builder) | 19 |
| `home` | pending | 3 |
| `importexport` | pending | 19 |
| `kilntask` | dropped (Carrier task kind, no Buzz stage) | 1 |
| `list` | pending (the Go spec drives a List layout of the board; no phase in the plan builds one, so it needs an owner) | unassigned |
| `login` | dropped (team login and cookie session) | 1 |
| `merge` | pending | 18 |
| `needs-you` | pending | 12 |
| `newtask` | pending | 16 |
| `parameters` | changed (Phase 8: `StartStage` accepts the parameters a stage declares, refuses an undeclared one, and keeps them on the run. The spec's card form that saves and reads parameters back is Phase 16) | 16 |
| `paused-turn` | pending (a result of `needs_input`, or a stage that ends with no result, now sets the card waiting with the reason in a `card.state` event, Phase 8; the reply that finishes it is the Needs you item, Phase 12) | 12 |
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


## Phase 7 deliverables

- Served: `StageDone` (new rpc, `StageDoneRequest { card_id, result, summary }`, returns
  the `Run`), at Run. The caller must be the agent the stage's task was addressed to,
  read from the queued task post; any other caller gets `permission_denied`. The result
  must be one `reportable_results` lists, else `invalid_argument` names the allowed ones.
- Reducer: `Input::StageDone` records `result.reported`. Only a `passed` from the last
  stage ends the run (`run.ended`) and the card goes to `done` (`card.state`). Any other
  result is recorded and the run stays Running until routing exists (Phase 8), so a
  one-stage workflow whose agent reports `failed` waits there.
- `Run.result` and `Run.outcome` stay empty: no migration holds them, so the result is
  read from the `result.reported` and `run.ended` events.
- Event kinds registered: `result.reported`, `run.ended`.
- `OutboxRepo::by_key` reads one outbox row by its idempotency key, in `MemStore` and
  `PgStore`.
- The stage task now ends with the card id and the tool name, because the agent has no
  other way to learn the card id: "call the Beehive tool `stage_done` with card_id ...".
- One table, `buzz-hive-api/src/tools.rs`, lists the tools. Routes take their level from
  it, `buzz-hive-mcp` lists tools from it, and `tests/tools_parity.rs` fails when a route
  has neither a tool nor a `NOT_TOOLS` reason. `StartStage` is the tool `start_run` and
  `GetStatus` is `get_hive_health`; the plan's `StartRun` route does not exist.
- `buzz-hive-mcp` (new crate): a hand-written rmcp `ServerHandler`, so no `build.rs` is
  needed for runtime tools (TDD spike 5). It signs a NIP-98 event with the agent's key,
  caches the token and renews it 60 seconds early.


## Phase 8 deliverables

- Reducer: `rules::step(run, wf, &Facts, Input)` takes `Facts` (the card's labels and the
  visit counts per stage). `Input::Start` carries the stage to start at and whether it is
  held. `rules/decide.rs` holds `next_route`, `resolve_route(route, result, then)`,
  `routes_backward`, `loop_count`, `over_loop_limit` and `retry_decision`;
  `rules/repeat.rs` holds the repeat rule; `rules/conditions.rs` holds `labels` and `else`.
- A run holds several steps. A stage entered again is a new step with the next `visit`.
  The rounds of a repeating stage share one step. Outbox keys are
  `kiln:{card}:{stage}:{visit}` (task), `...:status`, `...:round:{n}` and `...:resume:{k}`.
- One thread per stage visit. Entering a stage posts a new top-level mention and the drain
  records a `stage_threads` row. A repeat round with `fresh_session` bumps the visit and
  opens a new root. A plain repeat round replies in the stage's root thread through
  `Relay::post_reply` (`Posting::StageReply`). The reply has `root == parent`, so it
  carries one `e` tag with the marker `reply`.
- Migration `0006_run_routing.sql`. Later migrations shift by one (artifacts 0007, timers 0008).
  `RunsRepo::visits` became `visit_counts`.
- Served: `SkipStage`, `PauseCard`, `ResumeCard`, `SetCardWorkflow` and `MoveCard` (new rpc,
  `MoveCardRequest { card_id, stage_id }`), all at Run. `card_id` is a string in the four
  existing requests. `Run.steps` (new `RunStep`) lists stage, visit, round, result, summary,
  routed_to, backward, held, skipped and resumes. `StartStage` honors `stage_id` and
  `parameters`. Skip, pause and resume act on the card's run and return `FailedPrecondition`
  when there is none. `SetCardWorkflow` refuses a card that has a run.
- Tool `move_card` works for an agent only when the workflow sets
  `side_effects.agent_moves_cards` (default false). `skip_stage`, `pause_card`,
  `resume_card` and `set_card_workflow` are tools too. (Later, `skip_stage`, `pause_card`
  and `resume_card` stopped being tools and refuse an agent. `stage_done` and `move_card`
  take the stage and visit the task named.)
- A failed or `ask` result, a `needs_input` result and a missing result set the card waiting.
- `calls`: `then` is honored after a `passed` call. `give` waits for Phase 9.
- A hand-off to a next workflow may leave the card done with an `activity.added` note.
  A run that ends in its start step does not hand off.
- Loop count is per run. The Go `card.loop_limit` auto mode does not apply.
- The card has no labels field yet, so `Facts.labels` is empty until Phase 16 and every
  stage that needs a label follows `else`. The plan score is (0, 0) until Phase 9.
- Dropped: `DecidePermission` and `LevelAsks`. Buzz owns who may do what.
- Tests: `tests/rules_decide.rs`, `tests/routes.rs` (flat, not `tests/scenarios/routes.rs`),
  `buzz-hive-api/tests/routing.rs` and `buzz-hive/tests/e2e_fresh_session.rs`.

## Phase 9 deliverables

- Migration `0007_artifacts.sql` (timers shift to 0008): `hive.artifacts` with a `handoff`
  flag and `hive.artifact_versions` with kind, size, media type, media ref and the `visit`
  that saved it. A text version has no sha256; a file version's `media_ref` holds the media
  URL if the agent gave one, else the sha256.
- Core: `domain/artifact.rs`, `rules/checkboxes.rs` (`count_checkboxes`), `rules/brief.rs`,
  `orchestrator/artifacts.rs` and `orchestrator/activity.rs`. `facts()` now computes
  `Facts.plan` from the latest version of the stage's plan artifact, which closes the Phase 8
  stub. Only the stage's mentioned agent saves an artifact. A first save sets the hand-off
  flag from the stage's `ship`; later saves keep a person's change. A 1 MiB text limit applies.
- A required output missing at `stage_done` (passed) or `move_card` sets the run and card
  `waiting` with `run.waiting` condition `missing_output`. Phase 12 opens the inbox item.
- The task message has an inputs section (artifacts handed to the stage) and an outputs
  section (names the stage must save). `{{artifacts.*}}` and `{{inputs.*}}` stay empty.
- Served at Read: `ListArtifacts`, `ListArtifactVersions`, `ReadArtifact`, `ListCardFiles`,
  `GetBrief`, `ReadHistory`. Served at Run: `SetHandoff`, `SaveArtifact`, `UpdatePlan`,
  `LogWork`, `RecordDecision`. `card_id` is a string in the existing artifact requests.
  `ArtifactVersion`, `ArtifactInfo` and `CardFile` gained fields.
- Tools: `save_artifact`, `read_artifact`, `update_plan`, `log_work`, `record_decision`,
  `read_history`. The other five methods are `NOT_TOOLS` with reasons.
- Brief: ported without blockers (there is no `report_blocker` tool); entries are decisions,
  finished work and notes. It joins the task message only on a visit after the first.
- Not done: `calls` `give` carrying documents, and `importexport` (Phase 19).
- Tests: core `tests/artifacts.rs`, api `tests/artifacts.rs`, store
  `tests/postgres_artifacts.rs`, `buzz-hive/tests/e2e_files.rs` (needs a relay with
  `BUZZ_MEDIA_READ_MODE=enforce`, a private channel and the community host of the relay).
