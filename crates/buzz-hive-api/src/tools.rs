//! The tools a Buzz agent has, one row each. The routes and `buzz-hive-mcp` both read this
//! table, so they cannot disagree about a tool's name, its level or what it calls.
//!
//! A tool's arguments are the route's request body in protobuf JSON, so the schema names
//! the fields as the route reads them (camelCase). `tests/tools_parity.rs` fails when a
//! route has neither a row here nor an entry in [`NOT_TOOLS`] with its reason.

use buzz_hive_core::access::Level;
use buzz_hive_core::domain::artifact::MAX_ARTIFACT_BYTES;
use buzz_hive_core::orchestrator::activity::{
    DEFAULT_HISTORY_LIMIT, MAX_DETAIL_CHARS, MAX_HISTORY_LIMIT, MAX_OPTION_CHARS, MAX_REJECTED,
    MAX_REJECTED_REASON_CHARS, MAX_SUMMARY_CHARS, MAX_WHY_CHARS,
};
use buzz_hive_core::orchestrator::artifacts::MAX_PLAN_ITEM_BYTES;
use buzz_hive_core::orchestrator::assistant::MAX_MESSAGE_BYTES;
use buzz_hive_core::orchestrator::cards::{
    MAX_DESCRIPTION_CHARS, MAX_NOTE_DETAIL_CHARS, MAX_NOTE_SUMMARY_CHARS, MAX_TITLE_CHARS,
};
use buzz_hive_core::orchestrator::diagnose::{DEFAULT_EVENTS_LIMIT, MAX_EVENTS_LIMIT};
use buzz_hive_core::orchestrator::inbox::MAX_TEXT_CHARS;
use buzz_hive_core::orchestrator::run::MAX_SUMMARY_CHARS as MAX_RESULT_SUMMARY_CHARS;
use buzz_hive_core::tool_names::{
    ASSISTANT_RESULT, MOVE_CARD, READ_ARTIFACT, SAVE_ARTIFACT, STAGE_DONE, UPDATE_PLAN,
};
use serde_json::{json, Value};

pub struct Tool {
    pub name: &'static str,
    /// The EngineService method the tool calls.
    pub route: &'static str,
    /// The least level that may call it. An agent acts at the lower of its own level and the
    /// level of the person it acts for.
    pub needs: Level,
    pub description: &'static str,
    /// JSON Schema of the arguments.
    pub schema: fn() -> Value,
}

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "create_card",
        route: "CreateCard",
        needs: Level::Run,
        description: "Create a card on the Beehive board. Give a workflow to put it in To do, or none to put it in Backlog.",
        schema: create_card,
    },
    Tool {
        name: "start_run",
        route: "StartStage",
        needs: Level::Run,
        description: "Start a run of the card's workflow at its first stage, or at the stage you name. The stage's agent is mentioned in a new post in the update channel.",
        schema: start_run,
    },
    Tool {
        name: STAGE_DONE,
        route: "StageDone",
        needs: Level::Run,
        description: "Report the result of the stage you were asked to do. Only the agent the stage task mentioned may call it. Pass the card_id, stage and visit from the task and one of the results it lists. A report for a stage or visit the card has moved past is refused.",
        schema: stage_done,
    },
    Tool {
        name: MOVE_CARD,
        route: "MoveCard",
        needs: Level::Run,
        description: "Move the card you are working on to another stage, in place of the route your result would give. Pass the card_id, stage and visit from the task, and stage_id for the stage to move to. Only the agent the stage task mentioned may call it, and only when the workflow allows agents to move cards.",
        schema: move_card,
    },
    Tool {
        name: "set_card_workflow",
        route: "SetCardWorkflow",
        needs: Level::Run,
        description: "Sort cards onto workflows, all or nothing. A card that is running, waiting, archived or has run is refused. An empty workflow sends a card to Backlog.",
        schema: set_card_workflow,
    },
    Tool {
        name: "get_card",
        route: "GetCard",
        needs: Level::Read,
        description: "Read one card, with its description.",
        schema: card_id_only,
    },
    Tool {
        name: "list_cards",
        route: "ListCards",
        needs: Level::Read,
        description: "List the cards on the board, or in the archive.",
        schema: list_cards,
    },
    Tool {
        name: "list_runs",
        route: "ListRuns",
        needs: Level::Read,
        description: "List runs, newest first. Give a card id to list that card's runs.",
        schema: list_runs,
    },
    Tool {
        name: "get_run",
        route: "GetRun",
        needs: Level::Read,
        description: "Read one run: its state and the stage it is at.",
        schema: get_run,
    },
    Tool {
        name: "add_note",
        route: "AddNote",
        needs: Level::Run,
        description: "Add a note to a card's activity.",
        schema: add_note,
    },
    Tool {
        name: "archive_cards",
        route: "ArchiveCards",
        needs: Level::Run,
        description: "Set cards aside in the archive. A card that is running or waiting cannot be archived.",
        schema: archive_cards,
    },
    Tool {
        name: "restore_card",
        route: "RestoreCard",
        needs: Level::Run,
        description: "Bring a card back from the archive.",
        schema: card_id_only,
    },
    Tool {
        name: "remove_card",
        route: "RemoveCard",
        needs: Level::Run,
        description: "Delete a card. A card that is running or waiting cannot be removed.",
        schema: card_id_only,
    },
    Tool {
        name: "list_workflows",
        route: "ListWorkflows",
        needs: Level::Read,
        description: "List the workflows a card can use, with their stages.",
        schema: no_arguments,
    },
    Tool {
        name: "check_workflow",
        route: "CheckWorkflow",
        needs: Level::Read,
        description: "Check a workflow and list its findings. Leave the name empty to check every workflow.",
        schema: check_workflow,
    },
    Tool {
        name: "get_workflow",
        route: "GetWorkflowDraft",
        needs: Level::Read,
        description: "Read a workflow's files as text, with the hash to pass back as baseHash when you edit it. Pass text true.",
        schema: get_workflow,
    },
    Tool {
        name: "edit_workflow",
        route: "EditWorkflow",
        needs: Level::Read,
        description: "Change a workflow, or make a new one. Give every file of the package as text: kiln-package.yaml and workflow.yaml, and any prompt files. Pass the baseHash get_workflow gave, or none for a new workflow. When the person you act for may run workflows, it is saved and the workflow changes for the cards that start next, and a card already running keeps the version it started with. Otherwise it is stored as a proposal for a person to save. A draft with an error is refused with the errors listed.",
        schema: edit_workflow,
    },
    Tool {
        name: "list_agents",
        route: "ListAgents",
        needs: Level::Read,
        description: "List the Buzz agents a workflow stage can name.",
        schema: no_arguments,
    },
    Tool {
        name: "get_settings",
        route: "GetSettings",
        needs: Level::Read,
        description: "Read the settings of one scope: user, workflow or card.",
        schema: get_settings,
    },
    Tool {
        name: "set_setting",
        route: "SetSetting",
        needs: Level::Run,
        description: "Set one setting, or remove it with an empty value. Changing a community setting needs the Admin level.",
        schema: set_setting,
    },
    Tool {
        name: "set_auto_mode",
        route: "SetAutoMode",
        needs: Level::Run,
        description: "Turn auto mode on or off for a card. In auto mode the engine approves a gate whose listed artifacts all have a saved version, starts a failed run again twice, and asks a stage that did not report to carry on twice. It leaves everything else to a person. You may turn it on only for a person at Run or above; with none named the call is refused.",
        schema: set_auto_mode,
    },
    Tool {
        name: "set_access_level",
        route: "SetAccessLevel",
        needs: Level::Admin,
        description: "Set the level of a person or an agent in Beehive: none, read, run or admin. An agent can never hold a level above its owner's, and you cannot lower your own. You may call it only for a person at Admin; with none named the call is refused.",
        schema: set_access_level,
    },
    Tool {
        name: SAVE_ARTIFACT,
        route: "SaveArtifact",
        needs: Level::Run,
        description: "Save the next version of one of the outputs your stage task lists. Give a document as content, or a file with kind file and the sha256, size, type and url that `buzz upload file` printed. Only the agent the stage task mentioned may call it.",
        schema: save_artifact,
    },
    Tool {
        name: READ_ARTIFACT,
        route: "ReadArtifact",
        needs: Level::Read,
        description: "Read one version of a document a stage saved, the latest when version is 0. A file has no text: the result says where to download it with `buzz media get`.",
        schema: read_artifact,
    },
    Tool {
        name: UPDATE_PLAN,
        route: "UpdatePlan",
        needs: Level::Run,
        description: "Check or uncheck one item of your stage's plan. It saves a new version of the plan and returns how many items are checked. The plan must be saved with save_artifact first.",
        schema: update_plan,
    },
    Tool {
        name: "log_work",
        route: "LogWork",
        needs: Level::Run,
        description: "Record one piece of work you finished, so a later session on this stage can see it. The summary is one short line.",
        schema: log_work,
    },
    Tool {
        name: "record_decision",
        route: "RecordDecision",
        needs: Level::Run,
        description: "Record a choice you made and why, with the options you turned down, so a later session does not reopen it.",
        schema: record_decision,
    },
    Tool {
        name: "read_history",
        route: "ReadHistory",
        needs: Level::Read,
        description: "Read the work and decisions logged on a card, oldest first. Give after_seq of the last entry you saw to read on.",
        schema: read_history,
    },
    Tool {
        name: "cancel_run",
        route: "StopRun",
        needs: Level::Run,
        description: "Stop a run that has not ended. Its stage's agent is told to stop in the stage's thread, and the card is stopped. Give the run's id from get_card or list_runs.",
        schema: run_id_only,
    },
    Tool {
        name: "retry_stage",
        route: "RetryRun",
        needs: Level::Run,
        description: "Start the next attempt of a run that failed or was stopped: a new run at the stage the old one ended at, with a new thread. Give the id of the card's newest run. The card must be failed or stopped.",
        schema: run_id_only,
    },
    Tool {
        name: "ask_gate",
        route: "AskGate",
        needs: Level::Run,
        description: "Ask a person a question and wait for the reply. It opens an item in Needs you and the card waits; the reply arrives in this stage's thread. Pass the card_id, stage and visit from the task. Only the agent the stage task mentioned may call it.",
        schema: ask_gate,
    },
    Tool {
        name: "approve_gate",
        route: "Approve",
        needs: Level::Run,
        description: "Approve the approval stage the card waits at. Each artifact the stage lists is approved at its latest version, or at the version you name. A listed artifact with no saved version is refused by name, and the gate waits for a person.",
        schema: approve_gate,
    },
    Tool {
        name: "get_inbox",
        route: "ListInbox",
        needs: Level::Read,
        description: "List what waits for a person in Needs you, open items first. Give a card id to list one card's items.",
        schema: get_inbox,
    },
    Tool {
        name: ASSISTANT_RESULT,
        route: "AssistantResult",
        needs: Level::Read,
        description: "Report the result of an assistant job you were asked to do, once. Pass the jobId from the message that asked you, and a message for the person. For a workflow edit or a skill adaptation also give the whole package: name and every file as text. The package is stored as a proposal for the person to apply at their own level, and nothing is saved by this call. A package with an error is refused with the errors listed, so fix it and call again. Only the agent the job was given to may call it.",
        schema: assistant_result,
    },
    Tool {
        name: "get_hive_health",
        route: "GetHiveHealth",
        needs: Level::Read,
        description: "Read whether Beehive itself looks well: runs that have sat in one stage too long, how many posts to the relay are waiting or have failed, and how long the oldest has waited. allowlistApplied is always \"unknown\" for now: do not treat it as a fault.",
        schema: no_arguments,
    },
    Tool {
        name: "get_run_events",
        route: "GetRunEvents",
        needs: Level::Read,
        description: "Read one run's events, oldest first, to find out what happened to it. Give nextSeq of the last reply as afterSeq to read on.",
        schema: get_run_events,
    },
    Tool {
        name: "report_diagnosis",
        route: "ReportDiagnosis",
        needs: Level::Run,
        description: "Record your diagnosis of a run in its card's activity. Name the likely cause and quote what you read that shows it: at least one evidence quote is required, and a diagnosis without one is refused. List in gaps what you could not see.",
        schema: report_diagnosis,
    },
    Tool {
        name: "file_ticket",
        route: "FileTicket",
        needs: Level::Run,
        description: "Report a fault. kind engine (Beehive) or ui (the app's screens) makes a Backlog card linked to the repos you name. kind code (the card's own code) holds a ticket for a person to accept, which then opens a GitHub issue on the card's repo, so give cardId. An open card or issue that already says the same gets your report as a note or comment instead of a second one.",
        schema: file_ticket,
    },
];

/// Routes that are deliberately not tools, each with the reason.
pub const NOT_TOOLS: &[(&str, &str)] = &[
    (
        "GetStatus",
        "the page's header counts; get_hive_health reports what an agent needs",
    ),
    (
        "AddIssueRepo",
        "a person links a repo to a card on the card page; an agent names repos in file_ticket",
    ),
    (
        "RemoveIssueRepo",
        "a person unlinks a repo on the card page",
    ),
    (
        "AcceptTicketProposal",
        "a person decides a ticket an agent proposed, so an agent never accepts its own",
    ),
    (
        "PollEvents",
        "the board's live feed for a client; an agent reads get_card and list_runs",
    ),
    (
        "WhoAmI",
        "a client asks who it is; an agent acts as its own key and its level shows in the refusal",
    ),
    (
        "CreateTask",
        "create_card makes the same card; CreateTask keeps the Go engine's one-sentence form for the app",
    ),
    (
        "ListTemplateVariables",
        "an authoring aid for the workflow editor; an agent that edits a workflow reads get_workflow and checks with check_workflow",
    ),
    (
        "ListStageTemplates",
        "an authoring aid for the workflow editor; an agent that edits a workflow reads get_workflow and checks with check_workflow",
    ),
    (
        "SaveWorkflow",
        "a person's call from the editor; the engine refuses an agent, which uses edit_workflow and is held to a proposal unless its requester may run workflows",
    ),
    (
        "DuplicateWorkflow",
        "a person's call from the editor; the engine refuses an agent, which uses edit_workflow with the files of get_workflow under a new name",
    ),
    (
        "DeleteWorkflow",
        "a person's call from the editor; the engine refuses an agent, because a deleted workflow cannot be asked for again",
    ),
    (
        "SetWorkflowOptions",
        "a person's call from the editor; the engine refuses an agent, so a card's pauses and hand-offs stay a person's choice",
    ),
    (
        "ExportWorkflow",
        "a person's download from the editor; an agent reads get_workflow",
    ),
    (
        "ImportWorkflow",
        "a person's upload from the editor, a zip; an agent makes a workflow with edit_workflow",
    ),
    (
        "GetBrief",
        "the stage task already carries the brief; read_history gives every entry",
    ),
    (
        "ListArtifacts",
        "the Files tab lists them; a stage task names the inputs it reads, and read_artifact reads one",
    ),
    (
        "ListArtifactVersions",
        "the Files tab shows every version; an agent reads the one it needs with read_artifact",
    ),
    (
        "ListCardFiles",
        "the Files tab lists a card's files; a stage task says how to download its inputs",
    ),
    (
        "ListAutoDecisions",
        "the Activity tab shows each auto answer with its reason; an agent reads get_inbox for what still waits",
    ),
    (
        "SkipStage",
        "a person's call from the board. An agent that could hold, release or skip a card could override the others, so the engine refuses an agent and the shim offers no tool",
    ),
    (
        "PauseCard",
        "a person's call from the board; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "ResumeCard",
        "a person's call from the board; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "PauseAll",
        "a person's call from the board, at the Admin level; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "ResetCard",
        "a person's call from the board; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "AnswerInbox",
        "a person's call from Needs you; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "SendBack",
        "a person's call on an approval gate; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "SendMessage",
        "a person's way to write to a stage's agent; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "ListActivity",
        "the Activity tab filters a card's entries; an agent reads them with read_history",
    ),
    (
        "GetFullPayload",
        "the Activity tab opens one event; an agent has no use for it",
    ),
    (
        "GetUsage",
        "the Usage page's counts; an agent has no use for them",
    ),
    (
        "ListAccess",
        "the access page lists everyone with their level; an agent reads list_agents for the agents it may name",
    ),
    (
        "ListPeople",
        "the access page lists the community's members with their levels; an agent has no use for them",
    ),
    (
        "SaveIntakeRule",
        "a person's setting from the Intake page; a rule decides which outside text becomes cards and reaches agents, so an agent may not change it",
    ),
    (
        "DeleteIntakeRule",
        "a person's setting from the Intake page; an agent may not change which issues become cards",
    ),
    (
        "RefreshIntake",
        "a person's button on the Intake page; the loops read the host on their own schedule",
    ),
    (
        "ListIntakeRules",
        "the Intake page lists the rules; an agent has no use for them",
    ),
    (
        "PreviewIntake",
        "the Intake page's preview of a rule; an agent has no use for it",
    ),
    (
        "ListRepos",
        "the repo picker's list; an agent does not browse the team token's repos",
    ),
    (
        "ListOwners",
        "the repo picker's list; an agent does not browse the team token's accounts",
    ),
    (
        "SearchIssues",
        "the browse drawer's search; an agent does not search the team token's repos",
    ),
    (
        "ListIssueComments",
        "the Issue tab reads the comments live; an agent gets the ones a rule trusts in its stage thread",
    ),
    (
        "MergeCard",
        "a person's call from the card page; the engine refuses an agent, so the shim offers no tool. A workflow merges by its own merge stage",
    ),
    (
        "SetMergeOrder",
        "a person's call from the card page; the engine refuses an agent, so the shim offers no tool",
    ),
    (
        "SetHandoff",
        "a person's choice on the Files tab; an agent saves the artifact and the workflow sets its default",
    ),
    (
        "StartChat",
        "a person's message from the assistant panel; the assistant's agent answers with assistant_result",
    ),
    (
        "StartProposeWorkflowEdit",
        "a person's request from the workflow editor; the assistant's agent answers with assistant_result",
    ),
    (
        "StartAdaptSkills",
        "a person's request from the workflow editor; the assistant's agent answers with assistant_result",
    ),
    (
        "CancelJob",
        "a person's button on the assistant panel; the agent is told in the job's thread",
    ),
    (
        "ListProposals",
        "the person's own inbox of offers; an agent that proposes reads the reply of edit_workflow or assistant_result",
    ),
    (
        "ResolveProposal",
        "a person's decision on a proposal; an agent never decides for the person it proposed to",
    ),
];

fn string(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn object(properties: Vec<(&str, Value)>, required: &[&str]) -> Value {
    let properties: serde_json::Map<String, Value> = properties
        .into_iter()
        .map(|(name, schema)| (name.to_owned(), schema))
        .collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn no_arguments() -> Value {
    object(Vec::new(), &[])
}

fn card_id_only() -> Value {
    object(vec![("cardId", string("The card's id."))], &["cardId"])
}

/// The argument that names a run, in the tools that take one. The MCP shim reads it from a
/// call to fall back to a run claim, so the schemas below are its only owner.
pub const RUN_ID_ARG: &str = "runId";

fn run_id_only() -> Value {
    object(vec![(RUN_ID_ARG, string("The run's id."))], &[RUN_ID_ARG])
}

fn create_card() -> Value {
    object(
        vec![
            (
                "title",
                string(&format!(
                    "The card's title, 1 to {MAX_TITLE_CHARS} characters."
                )),
            ),
            (
                "description",
                string(&format!(
                    "What the card is about, up to {MAX_DESCRIPTION_CHARS} characters."
                )),
            ),
            (
                "workflow",
                string("The workflow's name. Empty puts the card in Backlog."),
            ),
        ],
        &["title"],
    )
}

fn start_run() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            (
                "stageId",
                string("The stage to start at. Empty starts at the workflow's first stage."),
            ),
            (
                "parameters",
                json!({
                    "type": "object",
                    "additionalProperties": { "type": "string" },
                    "description": "Values for the parameters the workflow's package declares, by name.",
                }),
            ),
        ],
        &["cardId"],
    )
}

fn move_card() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            ("stageId", string("The stage to move the card to.")),
            (
                "stage",
                string("The stage the task named, which the card is at."),
            ),
            ("visit", visit_field()),
        ],
        &["cardId", "stageId", "stage", "visit"],
    )
}

fn set_card_workflow() -> Value {
    object(
        vec![(
            "moves",
            json!({
                "type": "array",
                "items": object(
                    vec![
                        ("cardId", string("The card's id.")),
                        (
                            "workflow",
                            string("The workflow's name. Empty sends the card to Backlog."),
                        ),
                    ],
                    &["cardId"],
                ),
                "description": "The cards to move.",
            }),
        )],
        &["moves"],
    )
}

fn stage_done() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            ("stage", string("The stage the task named.")),
            ("visit", visit_field()),
            (
                "result",
                string("One of the results the stage task lists, such as passed or failed."),
            ),
            (
                "summary",
                string(&format!(
                    "What you did or found, up to {MAX_RESULT_SUMMARY_CHARS} characters. Optional."
                )),
            ),
        ],
        &["cardId", "stage", "visit", "result"],
    )
}

/// The visit a stage task names, which a report passes back.
fn visit_field() -> Value {
    json!({ "type": "integer", "minimum": 1, "description": "The visit the task named, from 1." })
}

fn ask_gate() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            ("stage", string("The stage the task named.")),
            ("visit", visit_field()),
            (
                "question",
                string(&format!(
                    "What you need a person to decide. Up to {MAX_TEXT_CHARS} characters."
                )),
            ),
        ],
        &["cardId", "stage", "visit", "question"],
    )
}

fn approve_gate() -> Value {
    object(
        vec![
            ("cardId", string("The card waiting at an approval stage.")),
            (
                "artifacts",
                json!({
                    "type": "array",
                    "description": "Versions to approve. An artifact left out is approved at its latest.",
                    "items": object(
                        vec![
                            ("name", string("The artifact's name.")),
                            (
                                "version",
                                json!({ "type": "integer", "minimum": 1, "description": "The version to approve." }),
                            ),
                        ],
                        &["name", "version"],
                    ),
                }),
            ),
        ],
        &["cardId"],
    )
}

fn get_inbox() -> Value {
    object(
        vec![
            (
                "cardId",
                string("The card's id. Empty lists every card's items."),
            ),
            (
                "includeResolved",
                json!({ "type": "boolean", "description": "Also list the items already answered." }),
            ),
        ],
        &[],
    )
}

fn list_cards() -> Value {
    object(
        vec![(
            "archived",
            json!({ "type": "boolean", "description": "List the archive instead of the board." }),
        )],
        &[],
    )
}

fn list_runs() -> Value {
    object(
        vec![("cardId", string("The card's id. Empty lists every run."))],
        &[],
    )
}

fn get_run() -> Value {
    run_id_only()
}

fn add_note() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            (
                "summary",
                string(&format!(
                    "One line, 1 to {MAX_NOTE_SUMMARY_CHARS} characters."
                )),
            ),
            (
                "detail",
                string(&format!(
                    "More text, up to {MAX_NOTE_DETAIL_CHARS} characters. Optional."
                )),
            ),
        ],
        &["cardId", "summary"],
    )
}

fn archive_cards() -> Value {
    object(
        vec![(
            "cardIds",
            json!({
                "type": "array",
                "items": { "type": "string" },
                "description": "The ids of the cards to archive.",
            }),
        )],
        &["cardIds"],
    )
}

fn save_artifact() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            (
                "name",
                string("The artifact's name, as the stage task lists it."),
            ),
            (
                "kind",
                string("text for a document (the default), or file for an uploaded file."),
            ),
            (
                "content",
                string(&format!("The document's full text, up to {MAX_ARTIFACT_BYTES} bytes. Leave empty for a file.")),
            ),
            (
                "sha256",
                string("For a file: the hash `buzz upload file` printed."),
            ),
            (
                "size",
                json!({ "type": "integer", "description": "For a file: its size in bytes." }),
            ),
            (
                "mediaType",
                string("For a file: its media type, such as image/png."),
            ),
            (
                "mediaRef",
                string("For a file: the url `buzz upload file` printed, or the hash when it printed none."),
            ),
        ],
        &["cardId", "name"],
    )
}

fn read_artifact() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            ("name", string("The artifact's name.")),
            (
                "version",
                json!({ "type": "integer", "description": "The version to read. 0 reads the latest." }),
            ),
        ],
        &["cardId", "name"],
    )
}

fn update_plan() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            (
                "item",
                string(&format!("The item's text as written after its checkbox, up to {MAX_PLAN_ITEM_BYTES} bytes.")),
            ),
            (
                "done",
                json!({ "type": "boolean", "description": "true checks the item, false unchecks it." }),
            ),
        ],
        &["cardId", "item", "done"],
    )
}

fn log_work() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            (
                "summary",
                string(&format!("One line, 1 to {MAX_SUMMARY_CHARS} characters.")),
            ),
            (
                "detail",
                string(&format!(
                    "More text, up to {MAX_DETAIL_CHARS} characters. Optional."
                )),
            ),
            (
                "planItem",
                string("The plan item this work finished. Optional."),
            ),
        ],
        &["cardId", "summary"],
    )
}

fn record_decision() -> Value {
    object(
        vec![
            ("cardId", string("The card_id the stage task gave you.")),
            (
                "summary",
                string(&format!(
                    "The choice, one line of 1 to {MAX_SUMMARY_CHARS} characters."
                )),
            ),
            (
                "why",
                string(&format!("Why, up to {MAX_WHY_CHARS} characters.")),
            ),
            (
                "rejected",
                json!({
                    "type": "array",
                    "maxItems": MAX_REJECTED,
                    "items": object(
                        vec![
                            ("option", string(&format!("An option you turned down, up to {MAX_OPTION_CHARS} characters."))),
                            ("reason", string(&format!("Why, up to {MAX_REJECTED_REASON_CHARS} characters."))),
                        ],
                        &["option"],
                    ),
                    "description": "The options you turned down. Optional.",
                }),
            ),
            (
                "detail",
                string(&format!(
                    "More text, up to {MAX_DETAIL_CHARS} characters. Optional."
                )),
            ),
            (
                "supersedes",
                json!({ "type": "integer", "description": "The seq of an earlier decision this one replaces. Optional." }),
            ),
        ],
        &["cardId", "summary", "why"],
    )
}

fn get_run_events() -> Value {
    object(
        vec![
            ("runId", string("The run's id.")),
            (
                "afterSeq",
                json!({ "type": "integer", "description": "Read events after this seq. 0 reads from the start." }),
            ),
            (
                "limit",
                json!({ "type": "integer", "description": format!("At most this many, up to {MAX_EVENTS_LIMIT}. 0 means {DEFAULT_EVENTS_LIMIT}.") }),
            ),
        ],
        &["runId"],
    )
}

fn report_diagnosis() -> Value {
    let evidence = json!({
        "type": "object",
        "properties": {
            "source": { "type": "string", "description": "Where you read it, such as \"event 8812\"." },
            "quote": { "type": "string", "description": "The words you read, copied exactly." },
        },
        "required": ["quote"],
    });
    object(
        vec![
            ("runId", string("The run's id.")),
            (
                "diagnosis",
                json!({
                    "type": "object",
                    "properties": {
                        "cause": string("One sentence naming the likely cause."),
                        "stageId": string("The stage the fault is in. Empty if you cannot say."),
                        "evidence": { "type": "array", "items": evidence, "description": "At least one quote." },
                        "gaps": { "type": "array", "items": { "type": "string" }, "description": "What you could not see." },
                    },
                    "required": ["cause", "evidence"],
                }),
            ),
        ],
        &["runId", "diagnosis"],
    )
}

fn file_ticket() -> Value {
    object(
        vec![
            (
                "kind",
                json!({ "type": "string", "enum": ["engine", "ui", "code"], "description": "engine: Beehive is at fault. ui: the app's screens are. code: the card's own code is." }),
            ),
            ("title", string("One line naming the fault.")),
            (
                "body",
                string("What happened, what you expected, and the evidence."),
            ),
            (
                "repos",
                json!({ "type": "array", "items": { "type": "string" }, "description": "owner/name of the repos it concerns. For code, the one repo of the card; empty means the card's first repo." }),
            ),
            (
                "cardId",
                string("The card whose code is at fault. Required for kind code."),
            ),
        ],
        &["kind", "title", "body"],
    )
}

fn read_history() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            (
                "afterSeq",
                json!({ "type": "integer", "description": "Read entries after this seq. 0 reads from the start." }),
            ),
            (
                "limit",
                json!({ "type": "integer", "description": format!("At most this many, up to {MAX_HISTORY_LIMIT}. 0 means {DEFAULT_HISTORY_LIMIT}.") }),
            ),
        ],
        &["cardId"],
    )
}

fn check_workflow() -> Value {
    object(
        vec![(
            "name",
            string("The workflow's name. Empty checks every workflow."),
        )],
        &[],
    )
}

fn get_workflow() -> Value {
    object(
        vec![
            ("name", string("The workflow's name.")),
            (
                "text",
                json!({ "type": "boolean", "description": "Pass true: every file comes back as text in textFiles." }),
            ),
        ],
        &["name", "text"],
    )
}

fn edit_workflow() -> Value {
    object(
        vec![
            (
                "name",
                string("The package name: 1 to 32 lower-case letters, digits, - and _. The same as the name in kiln-package.yaml."),
            ),
            (
                "files",
                json!({
                    "type": "object",
                    "description": "Every file of the package by path, as text. A file left out is removed.",
                    "additionalProperties": { "type": "string" },
                }),
            ),
            (
                "baseHash",
                string("The hash get_workflow gave. Empty makes a new workflow."),
            ),
        ],
        &["name", "files"],
    )
}

fn assistant_result() -> Value {
    object(
        vec![
            ("jobId", string("The job id the message that asked you named.")),
            (
                "message",
                string(&format!(
                    "What you say to the person, at most {MAX_MESSAGE_BYTES} bytes."
                )),
            ),
            (
                "name",
                string("The package name, when you offer a workflow: the same as the name in kiln-package.yaml."),
            ),
            (
                "files",
                json!({
                    "type": "object",
                    "description": "Every file of the offered package by path, as text.",
                    "additionalProperties": { "type": "string" },
                }),
            ),
            (
                "baseHash",
                string("The hash get_workflow gave for the package. Empty for a new one."),
            ),
        ],
        &["jobId", "message"],
    )
}

fn get_settings() -> Value {
    object(
        vec![
            ("scope", string("user, workflow or card.")),
            (
                "scopeId",
                string("The card's id for scope card. Empty otherwise."),
            ),
            (
                "workflow",
                string("The workflow's name for scope workflow."),
            ),
        ],
        &["scope"],
    )
}

fn set_access_level() -> Value {
    object(
        vec![
            (
                "pubkey",
                string("The person's or agent's pubkey, 64 hex characters."),
            ),
            (
                "level",
                json!({
                    "type": "string",
                    "enum": ["none", "read", "run", "admin"],
                    "description": "The level to give.",
                }),
            ),
        ],
        &["pubkey", "level"],
    )
}

fn set_auto_mode() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            (
                "enabled",
                json!({ "type": "boolean", "description": "True turns auto mode on, false turns it off." }),
            ),
        ],
        &["cardId", "enabled"],
    )
}

fn set_setting() -> Value {
    object(
        vec![
            ("scope", string("user, workflow or card.")),
            (
                "scopeId",
                string("The card's id for scope card. Empty otherwise."),
            ),
            ("key", string("The setting's key, such as update_channel.")),
            (
                "valueJson",
                string("The value as JSON text. Empty removes the setting."),
            ),
            (
                "workflow",
                string("The workflow's name for scope workflow."),
            ),
        ],
        &["scope", "key"],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_names_and_routes_are_unique() {
        let mut names: Vec<_> = TOOLS.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len(), "a tool name repeats");
        let mut routes: Vec<_> = TOOLS.iter().map(|t| t.route).collect();
        routes.sort_unstable();
        routes.dedup();
        assert_eq!(routes.len(), TOOLS.len(), "two tools call one route");
    }

    #[test]
    fn the_limits_a_schema_states_are_the_ones_core_enforces() {
        let text = |tool: &str| {
            let tool = TOOLS.iter().find(|t| t.name == tool).unwrap();
            (tool.schema)().to_string()
        };
        let has = |tool: &str, limit: &dyn std::fmt::Display| {
            let text = text(tool);
            assert!(
                text.contains(&limit.to_string()),
                "{tool} does not state {limit}: {text}"
            );
        };
        has("create_card", &MAX_TITLE_CHARS);
        has("create_card", &MAX_DESCRIPTION_CHARS);
        has("stage_done", &MAX_RESULT_SUMMARY_CHARS);
        has("add_note", &MAX_NOTE_SUMMARY_CHARS);
        has("add_note", &MAX_NOTE_DETAIL_CHARS);
        has("save_artifact", &MAX_ARTIFACT_BYTES);
        has("update_plan", &MAX_PLAN_ITEM_BYTES);
        has("log_work", &MAX_SUMMARY_CHARS);
        has("log_work", &MAX_DETAIL_CHARS);
        has("record_decision", &MAX_WHY_CHARS);
        has("record_decision", &MAX_OPTION_CHARS);
        has("record_decision", &MAX_REJECTED_REASON_CHARS);
        has("record_decision", &MAX_REJECTED);
        has("read_history", &MAX_HISTORY_LIMIT);
        has("read_history", &DEFAULT_HISTORY_LIMIT);
        has("ask_gate", &MAX_TEXT_CHARS);
        has("assistant_result", &MAX_MESSAGE_BYTES);
    }

    #[test]
    fn every_tool_the_text_core_writes_points_at_is_a_tool() {
        for name in [
            STAGE_DONE,
            READ_ARTIFACT,
            SAVE_ARTIFACT,
            UPDATE_PLAN,
            MOVE_CARD,
            ASSISTANT_RESULT,
        ] {
            assert!(
                TOOLS.iter().any(|t| t.name == name),
                "core tells agents to call {name}, which is not in TOOLS"
            );
        }
    }

    #[test]
    fn a_report_and_a_move_require_the_stage_and_visit() {
        for name in [STAGE_DONE, MOVE_CARD] {
            let tool = TOOLS.iter().find(|t| t.name == name).unwrap();
            let schema = (tool.schema)();
            let required: Vec<&str> = schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert!(
                required.contains(&"stage") && required.contains(&"visit"),
                "{name}"
            );
        }
    }

    #[test]
    fn every_schema_is_an_object_that_names_only_fields_it_declares() {
        for tool in TOOLS {
            let schema = (tool.schema)();
            assert_eq!(schema["type"], "object", "{}", tool.name);
            let declared = schema["properties"].as_object().unwrap();
            for required in schema["required"].as_array().unwrap() {
                assert!(
                    declared.contains_key(required.as_str().unwrap()),
                    "{} requires {required}, which it does not declare",
                    tool.name
                );
            }
        }
    }
}
