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
| `core/issuefiles` | dropped (the Go package copies an issue's attached images into the card's folder; a card has no folder here and the Issue tab shows the issue's own text. Phase 16 does not fetch or store issue attachments) | 16 |
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
| `adapters/github` | 20 | changed (Phase 16: `buzz-hive/src/github_adapter.rs` is the `CodeHost` over `reqwest` with `If-None-Match` etags, token only in an Authorization header; it lists issues, reads an issue and its comments, searches, lists repos and owners and writes a comment once per key. The Go pull-request calls stay Phase 17. `buzz-hive/tests/github_adapter.rs` runs it against a local replay server, not GitHub) | 16 |
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
| `core/issuefiles` | 7 | dropped (see Features) | 16 |
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
| `activity` | changed (Phase 12: a card's Activity tab lists what happened on it, newest first, and an answer shows who gave it and the reason; `buzz-hive-api/tests/inbox.rs`, buzz-app `ActivityTab.test.tsx`, `beehive.journey.mjs` and `e2e/needs-you.spec.mjs`. An answer is an `activity.added` event, not a `hive.activity` table) | 12 |
| `answers` | changed (Phase 12 answers an approval with Approve or Reject and a reason, a `needs_input` result with a reply, and the other items with retry, skip, stop or dismiss, in `tests/inbox.rs`, `buzz-hive-api/tests/inbox.rs` and `buzz-hive/tests/e2e_approval.rs`. The Go spec's permission requests and its field and multi-question forms are not ported: `AnswerInbox` refuses `values_json` and `answers`) | 12 |
| `archive` | changed (Phase 4 ports the contract in buzz-app `e2e/archive.spec.mjs`: archive leaves the board, the archive lists it, Restore returns it to Backlog. The Go spec drags a card onto Done; drag and drop returns with the phase that moves cards between columns) | 4 |
| `artifacts` | changed (Phase 9 ports the contract: save, versions, read, hand-off, the stage-declared outputs rule and the task message naming inputs, in `tests/artifacts.rs` and `buzz-hive-api/tests/artifacts.rs`. The Go spec's approval and image-review screens are not ported; buzz-app `e2e` carries the Files tab) | 9 |
| `assistant` | pending | 20 |
| `auto-mode` | changed (Phase 13 ports the contract: a card in auto mode has its approval gates approved when every listed artifact has a saved version, a failed run started again twice and a stage with no result asked to carry on twice, each with its reason in Activity, and a loop that is never unbounded. The Go spec's cost-cap cases, its Ask recommendations and forms and its paused-turn reply are dropped, see the Phase 13 deliverables. The Switch and the Activity entry are covered in buzz-app `ActivityTab.test.tsx` and `BeehivePage.test.tsx`) | 13 |
| `board` | changed (Phase 3 ports the empty board against a real `buzz-hive`, in buzz-app `src/features/beehive/e2e/board.spec.mjs`; the drag, move and label-suggestion tests return in the phases that add cards, stages and intake) | 3 |
| `boot` | pending | 3 |
| `browse` | changed (Phase 16: `SearchIssues` and `ListRepos` back the New card dialog's From an issue mode, in `buzz-hive-api/tests/intake.rs` and buzz-app `NewCardDialog.test.tsx`. The spec's typed GitHub query is not ported: a search is by repo, label and text) | 16 |
| `builder-run` | pending | 19 |
| `builder` | pending | 19 |
| `calls` | changed (Phase 8 routes a result to a call's target and honors `then` after `passed`; `give` carries documents; Phase 12 opens a Needs you item for a route that asks, with skip and stop) | 12 |
| `card` | changed (Phase 4 ports adding a card, reading its details and a card added elsewhere reaching an open board, in buzz-app `e2e/card.spec.mjs`. Live, History, Usage and the stage controls return with runs and stages) | 4 |
| `costs-pull-requests` | dropped (cost figures) | 1 |
| `costs` | dropped (cost figures) | 1 |
| `files` | changed (the Files tab lists a card's saved files and documents with a download link, in buzz-app `beehive.journey.mjs` and `BeehivePage.test.tsx`; `buzz-hive/tests/e2e_files.rs` proves the file round trip through the relay's media store. Issue attachments are dropped, see `core/issuefiles`) | 9 |
| `filters` | pending (filters narrow by run state and search; a card now reaches Done, but the Stopped and Failed states the spec filters on arrive with StopRun and routing, Phase 11) | 11 |
| `guide` | pending (the Go spec drives the workflow builder) | 19 |
| `home` | pending | 3 |
| `importexport` | pending | 19 |
| `kilntask` | dropped (Carrier task kind, no Buzz stage) | 1 |
| `list` | pending (the Go spec drives a List layout of the board; no phase in the plan builds one, so it needs an owner) | unassigned |
| `login` | dropped (team login and cookie session) | 1 |
| `merge` | pending | 18 |
| `needs-you` | changed (Phase 12: the Needs you view lists open items with the card, the stage and a button per answer, and its tab counts them live; buzz-app `NeedsYou.test.tsx`, `beehive.journey.mjs` and `e2e/needs-you.spec.mjs`. A gate an agent opens is answered end to end in `buzz-hive/tests/e2e_approval.rs`, which needs a relay. Resolved items are not listed) | 12 |
| `newtask` | changed (Phase 16: a card is made from an issue with `CreateCard` and `issue {repo, number}`, in `buzz-hive-api/tests/intake.rs`; making the same issue twice returns the first card) | 16 |
| `parameters` | changed (Phase 8: `StartStage` accepts the parameters a stage declares, refuses an undeclared one, and keeps them on the run. The spec's card form that saves and reads parameters back is not ported: a card made from an issue starts with no parameters) | 16 |
| `paused-turn` | changed (a result of `needs_input`, or a stage that ends with no result, now sets the card waiting with the reason in a `card.state` event, Phase 8; Phase 12 opens the Needs you item that a reply finishes, in `tests/inbox.rs` and `buzz-hive/tests/e2e_approval.rs`) | 12 |
| `permissions` | changed (Phase 14 ports the contract against Beehive levels: an admin sets a person's or an agent's level, the engine then allows and refuses to match, and Settings lists the same levels. Kiln's permission asks are dropped, Buzz owns permissions. Covered by buzz-app `e2e/permissions.spec.mjs`) | 14 |
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
| `usage` | changed (the Go spec checks the Dynatrace report. Phase 12 replaces it with the Usage view: runs, how long they took, and per week the cards and runs that agents started, with no token or cost figure; buzz-app `UsagePage.test.tsx`, `beehive.journey.mjs`, `e2e/needs-you.spec.mjs` and `buzz-hive-api/tests/inbox.rs`) | 12 |
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
- Loop count is per run. The Go `card.loop_limit` setting does not apply. Auto mode only fills in a
  bound for a stage whose workflow sets none (Phase 13).
- `Facts.labels` is the labels of the card's issue as last read (Phase 16), so a card with no
  issue has none and every stage that needs a label follows `else`. The plan score is (0, 0) until Phase 9.
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

## Phase 12 deliverables

- Migration `0011_inbox.sql`: `hive.inbox` (kind, stage, visit, title, detail, who and when
  it was resolved, and the answer). The plan's `hive.activity` table is not created: an
  answer is an `activity.added` event of type `answer`, so the Activity tab has one source.
  The plan's `relay_cursor` and `seen_events` tables are not created either, see below.
- No relay listener (plan question 3, resolved by the human): gates and questions are
  answered by buttons and tools only. There is no `relay_listener.rs`, so a reply typed in a
  stage thread is not read.
- Core: `domain/inbox.rs` (kinds and the answers each takes), `domain/usage.rs`,
  `orchestrator/inbox.rs` (list, answer, approve, send back, ask, send message) and
  `orchestrator/usage.rs`. There is no `kinds/approval.rs`; approval is a state of the
  reducer (`rules/run.rs`), as the other stage kinds are.
- Items: an approval stage (approve or reject), a `needs_input` result (reply, skip, stop), a
  route that asks (skip, stop), a stage with no reported result when its time ran out
  (retry, skip, stop), a missing output (retry, skip, stop) and a run that failed for good
  (retry, dismiss). Moving the run by any other route resolves its open items as
  `superseded`.
- A time-out opens a "no result" item only for a stage with no retries. A stage with
  retries keeps the Phase 11 behavior (retry, then fail), so a failed run stays reachable.
- Approve records the latest version of each listed artifact and the card moves on. Reject
  and SendBack need a reason, move the card on `needs_work`, and the next task of the stage
  carries the reason. Only a person (not an agent) may answer, approve or send back.
- Agents: `ask_gate` and `approve_gate` are tools at Run level. `approve_gate` is open to any
  agent with Run until Phase 13 decides who may approve.
- API: `ListInbox` and `GetUsage` at Read; `AnswerInbox`, `Approve`, `SendBack`,
  `SendMessage` and `AskGate` at Run; `ListActivity` and `GetFullPayload` at Read.
  `GetFullPayload` takes the event's seq as `full_ref`, because events are never cut.
  The `inbox.proto` ids changed to string UUIDs. `AskGate` and `GetUsage` are new rpcs.
- Usage counts what agents did: runs, how long they took, and per week the cards created
  and the runs started by agents (counted from `hive.events` where the actor is an agent).
  There is no token or cost field.
- Tests: core `tests/inbox.rs`, api `tests/inbox.rs`, store `tests/postgres_inbox.rs` and
  `buzz-hive/tests/e2e_approval.rs` (needs a local relay).

## Phase 13 deliverables

- Rules: `rules/auto.rs` ports `rules/autopilot.go` without its cost cap and its advisory
  counter: `AUTO_RETRIES` and `AUTO_RESUMES` are 2, `AUTO_LOOP_LIMIT` is 10, `answer_gate`
  approves only when every artifact an approval lists has a saved version (and names the first
  one that has none), and `auto_answer` leaves everything else to a person. `rules/run.rs`
  uses `auto_loop_limit(stage.loop_limit)` when `Facts.auto_mode` is set, so a loop with no
  limit stops after 10 backward routes and opens a Route item for a person.
- Setting: `card.auto_mode` stays a card-scope bool (the key already existed from Phase 6).
  A new `SetAutoMode` rpc (Run) sets it and returns the card. It is the one call an agent has,
  as the `set_auto_mode` tool. `SetSetting` for the same key is guarded in the same way. An
  agent may turn it on only when the person it acts for is at Run or above, which the Phase 15
  requester claim says; an agent with no claim is refused. A person at Run or above,
  and turning it off, are always allowed. `GetCard` reports `auto_mode`.
- Orchestrator: `orchestrator/auto.rs` runs at the end of every step that opens an item and
  when auto mode is turned on for a card that already waits. It answers each item as the
  Beehive account, through the path a person's answer takes, so a card's routing, outbox
  posts and events are the same. The answer writes an activity entry of type `auto` with the
  reason (`ListAutoDecisions` reads those entries, oldest first). An answer that cannot be
  carried out (for example the next agent left the channel) is undone by a savepoint and left
  for a person with a note, so it never fails the step that opened the item.
- Attempts are counted from the inbox rows: consecutive items of the same kind and stage that
  the Beehive account answered with `retry`. No migration was needed.
- Agents: `approve_gate` is held to the same rule, with or without auto mode. A gate that lists
  an artifact with no saved version is refused, naming it, and waits for a person.
- Directory: the relay's directory is asked before the first append of a transaction (the
  Phase 12 rule). `decide` did not do that for a person's answer to a gate; it now asks first,
  and so does turning auto mode on.
- Usage: `GetUsage` leaves out the Beehive account's own events, so a run auto mode started
  again is not counted as started by an agent.
- Dropped from `scenario_auto_mode_test.go`, with the reason:
  - The ten cost-cap cases (`StopsACardAtItsCostCap`, `LetsACardUnderItsCostCapPass`,
    `OverTheCostCapAnswersNothing`, `RaisingTheCostCapLetsTheCardStartAgain`,
    `StopsACardWhoseSpendEqualsItsCap`, `CostCapOfZeroRefusesAFreshCard`,
    `AutoStartLaneOverTheCostCapFailsNothing`, `OffStartsOverTheCostCap`,
    `RefusesARetryOverTheCostCap`, `TestCardIgnoresTheCostCap`): Beehive has no cost figures.
  - The Ask cases (`AnswersARecommendedAsk`, `AnswersAFormByTheTitleMark`,
    `NudgesOnceThenAnswersTheMarkedAsk`, `LeavesASecondUnmarkedAskOpen`,
    `AnswersAnUnmarkedAskWithTheAnsweringAgent`, `LeavesAnAskWhenTheAnsweringAgentFails`,
    `RepliesToOnePausedTurn`, `KeepsAQuietAgentWaiting`, `SkipsATestCard`): a Beehive question
    is an agent's `ask_gate` and always waits for a person, there is no test card, and a quiet
    agent is handled by the stage timeout of Phase 11.
  - `CardLoopLimitReplacesTheStages`: there is no `card.loop_limit` key.
- Ported in `tests/auto_mode.rs`: `KeyBelongsToACard`, `OffLeavesTheAskOpen` (the gate),
  `TurningAutoModeOnAnswersTheOpenAsk` (the gate), `ApprovesAReviewStage`,
  `LeavesAnApprovalWithAMissingArtifact`, `RetriesTwiceThenLeavesTheFailure`,
  `RetriesANoResultTwice`, `RouteBackCountsTowardTheLoopLimit` (as the 10-visit bound), and in
  `buzz-hive-api/tests/inbox.rs` `ListAutoDecisionsRefusesAnUnknownCard`.
- Differences from Go: a Route item, a Question and a missing output are left to a person (Go
  retries a missing output); a retry or resume carries a reason in Activity as the other
  answers do; the advisory counter is not ported because Buzz has no advisory inbox kind.
- Port seams: the `Tx` port gained `savepoint`, `rollback_savepoint` and `release_savepoint`
  (`MemStore`, `PgStore` and the directory spy implement them), and `UsageRepo::usage` takes the
  engine's pubkey.
- Tests: core `tests/auto_mode.rs` and `tests/directory_before_log.rs`, api `tests/inbox.rs`,
  store `tests/postgres_savepoint.rs` and `buzz-hive/tests/e2e_auto_mode.rs` (needs a local
  relay).

## Phase 14 deliverables

- Routes: `ListAccess` (Read) lists every person and agent with the level each acts at,
  `ListPeople` (Read) lists the relay's people with their level, and `SetAccessLevel` (Admin)
  sets one. Each change appends an `access.changed` event with the actor, holding the pubkey,
  kind, new level and previous level.
- Rules in `orchestrator/access.rs`: an agent is capped at its owner's level, and a change that
  would put it above is `failed_precondition` naming the agent, the cap and the owner. An
  admin may not lower their own level. A pubkey that is not in the community is `not_found`
  and a switched-off agent is `failed_precondition`. An agent holding Admin may set a level
  only for a requester at Admin, which the Phase 15 requester claim says; an agent with no claim
  is refused.
  The `set_access_level` tool is the one call an agent has.
- Directory: `Directory::people()` reads the relay's members with their display names. As in
  Phase 12 it is asked before the first append of a transaction.
- `Person.level` is a new field of the proto message, so `WhoAmI` and `ListPeople` carry it.
- Tests: core `tests/access.rs`, `tests/event_coverage.rs` and `tests/directory_before_log.rs`,
  api `tests/access.rs` and `tests/procedures.rs`, store `tests/postgres_directory.rs`.

## Phase 15 deliverables

- Rule for an agent with no claim: it is held to Read. It can read cards, runs and files and
  cannot act, so an unattended agent (a heartbeat, a timer, a turn with no message) does
  nothing that needs Run. A person sends no claim, and a claim header a person sends is
  ignored. `Ctx.requester` is the claim's person for an agent and `None` for a person, and
  `allow(actor, requester, needed)` takes the lower of the two.
- Claim: `POST /hive/v1/claim` takes `{event_id}` or `{run_id}` from an agent's bearer token
  and returns an EdDSA JWT `{sub, community, agent, via: "chat" | "run:<id>", iat, exp}`,
  valid 5 minutes, key id `beehive-claims-1`. The person's level is not in the claim: it is
  read from the directory when the claim is used, so a person who loses access takes the
  agent's reach with them. A bad claim (forged, expired, for another agent or community) is
  `unauthenticated`. `GET /.well-known/jwks.json` publishes the public key.
- Signing key: the Ed25519 seed is HMAC-SHA256 of the token secret and a fixed label, so
  there is no second secret to store. Rotating `BUZZ_HIVE_TOKEN_SECRET` rotates both.
- By chat (`event_id`): the message exists and is not deleted, is a chat kind (`MESSAGE_KINDS`),
  mentions the agent, is at most 2 hours old and has no newer message in its thread that
  mentions the agent. The person is its author. When the author is the Beehive account the
  message is a stage task: it is looked up through `stage_threads` (migration 0012 indexes
  `root_event_id`), the stage must belong to this agent, and the person is the `requester`
  of the run that is at that stage and visit.
- By run (`run_id`): the run has not ended, is in a stage, and that stage's thread holds this
  agent. The person is the run's `requester`.
- Relay reads per claim: two on `public.events` (the message by id, and the newer-mention
  check), plus the directory reads Beehive already does per call. `deploy/aws/hive-roles.sql`
  grants `SELECT` on eight `events` columns only; `content` and `sig` stay unreadable.
- Harness: `buzz-acp` writes the id of the message that starts each turn to
  `<temp>/buzz-requester/<agent>/<index>/<channel>[-<root>]` and gives every MCP server
  `BUZZ_REQUESTER_FILE`. A message steered into a running turn rewrites it only when it
  mentions the agent, since only that message can name a requester. A heartbeat turn has no
  file. `buzz-hive-mcp` reads the file on every call, asks for a claim, holds it for 60
  seconds per message, and sends it as `x-hive-claim`. A refused claim does not stop the call:
  it goes unclaimed and a permission refusal says why no person is named.
- Fanout: an issued claim is held up to 60 seconds per client, so a reconnecting fleet makes at
  most one claim request per agent per minute of activity. A refused claim is not held, so an
  agent whose message names no person asks again on every call; each ask is two relay reads
  (the message, and whether a newer one mentions the agent) plus one person lookup.
- Who a claim may name: a person, never an agent. A message an agent wrote, or a run an agent
  started for itself, is refused, and `SetAccessLevel` is refused to an agent whose requester is
  below Admin or is the person being lowered. A claim by chat is also checked for the message's
  channel, but the message is not tied to the agent's current turn: an agent can still pick any
  recent message that mentions it and names a person (see the checkpoint 3 fix plan, 1.3).
- Tests: api `tests/claims.rs`, `tests/access.rs`, `tests/inbox.rs`; shim `tests/shim.rs`;
  `buzz-acp` `requester.rs` and a pool test; store `tests/postgres_roles.rs`;
  `buzz-hive/tests/claim_echo.rs` and `e2e_requester.rs` (needs a local relay).

## Phase 16 deliverables

- `CodeHost` (`ports/code_host.rs`) is the only way the engine reads or writes a code host.
  `FakeCodeHost` answers it in tests; `buzz-hive/src/github_adapter.rs` answers it from GitHub
  with the team token `GH_TOKEN`. `BUZZ_HIVE_GITHUB_API_URL` moves the API address (tests point it
  at a replay server). Without a token the intake routes that read GitHub answer
  `FailedPrecondition` and no loop starts.
- A rule is one repo: `host: github.com`, `repos` with exactly one `owner/name`, an optional
  label, optional text, `assignees` of `anyone` or `unassigned`, and an interval of at least 60
  seconds. A typed `query` and `assigned_to_me` are refused. Only a person may save or delete a
  rule: its authors decide which outside comments reach an agent.
- A pass lists the open issues (one request per page of 100, up to 10 pages), makes a card for
  each issue without one, and reads once each attached issue that left the list (up to 50 per
  pass) to learn if it closed or lost its label. A card stays on the board either way; the
  engine records `card.left_intake` with the reason, and `RefreshIntake` always reports
  `removed: 0`. A card a person removed is not made again. One row per issue, taken with an
  upsert, keeps two passes from making two cards.
- GitHub calls: one list per rule per interval, one read per issue that left, one comment read
  per running or waiting card every 120 seconds. A `304` costs no quota. A rate limit holds every
  loop (`Gate`) for what GitHub asked; other failures wait twice the interval, then twice that,
  up to 30 minutes. Loops start after a jitter of up to a fifth of their interval. Nothing reads
  the relay.
- A comment from the host is outside text. It goes to a card's agent only when its author is in
  the rule's `authors` list, quoted and labelled as not an instruction from a person, and is
  recorded once per comment id. "Delivered" means queued in the stage thread's outbox.
- New on the wire beyond the plan: the `ListIssueComments` procedure and `Card.issue_labels`, for
  the Issue tab. Ids of rules are uuid strings. The plan's migration `0009` is `0013_intake.sql`.
- Tests: core `tests/intake.rs`, `tests/routes.rs`, `tests/event_coverage.rs`; store
  `tests/postgres_intake.rs`; api `tests/intake.rs`; binary `tests/github_adapter.rs`,
  `tests/intake_loop.rs` (paused time) and `tests/intake_replay.rs` (real adapter, loops and routes
  against the replay server, with a restart). Real GitHub is not exercised.
