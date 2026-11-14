//! The tools a Buzz agent has, one row each. The routes and `buzz-hive-mcp` both read this
//! table, so they cannot disagree about a tool's name, its level or what it calls.
//!
//! A tool's arguments are the route's request body in protobuf JSON, so the schema names
//! the fields as the route reads them (camelCase). `tests/tools_parity.rs` fails when a
//! route has neither a row here nor an entry in [`NOT_TOOLS`] with its reason.

use buzz_hive_core::access::Level;
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
        name: "stage_done",
        route: "StageDone",
        needs: Level::Run,
        description: "Report the result of the stage you were asked to do. Only the agent the stage task mentioned may call it. Pass the card_id from the task and one of the results it lists.",
        schema: stage_done,
    },
    Tool {
        name: "move_card",
        route: "MoveCard",
        needs: Level::Run,
        description: "Move the card you are working on to another stage, in place of the route your result would give. Only the agent the stage task mentioned may call it, and only when the workflow allows agents to move cards.",
        schema: move_card,
    },
    Tool {
        name: "skip_stage",
        route: "SkipStage",
        needs: Level::Run,
        description: "Move a card that is waiting or held on, as if its stage had passed. The stage is marked skipped.",
        schema: card_id_only,
    },
    Tool {
        name: "pause_card",
        route: "PauseCard",
        needs: Level::Run,
        description: "Hold a running card. Its agent's report is refused until the card is resumed.",
        schema: card_id_only,
    },
    Tool {
        name: "resume_card",
        route: "ResumeCard",
        needs: Level::Run,
        description: "Let a held card carry on: a stage held before it started posts its task, and a paused stage runs again.",
        schema: card_id_only,
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
        name: "save_artifact",
        route: "SaveArtifact",
        needs: Level::Run,
        description: "Save the next version of one of the outputs your stage task lists. Give a document as content, or a file with kind file and the sha256, size, type and url that `buzz upload file` printed. Only the agent the stage task mentioned may call it.",
        schema: save_artifact,
    },
    Tool {
        name: "read_artifact",
        route: "ReadArtifact",
        needs: Level::Read,
        description: "Read one version of a document a stage saved, the latest when version is 0. A file has no text: the result says where to download it with `buzz media get`.",
        schema: read_artifact,
    },
    Tool {
        name: "update_plan",
        route: "UpdatePlan",
        needs: Level::Run,
        description: "Check or uncheck one item of your stage's plan. It saves a new version of the plan and returns how many items are checked. The plan must be saved with save_artifact first.",
        schema: update_plan,
    },
    Tool {
        name: "log_work",
        route: "LogWork",
        needs: Level::Run,
        description: "Record one piece of work you finished, so a later session on this stage can see it. One line, up to 120 characters.",
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
        name: "get_hive_health",
        route: "GetStatus",
        needs: Level::Read,
        description: "Read Beehive's version, the newest event number and the running card count.",
        schema: no_arguments,
    },
];

/// Routes that are deliberately not tools, each with the reason.
pub const NOT_TOOLS: &[(&str, &str)] = &[
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
        "an authoring aid for the workflow editor; the editing tools arrive with workflow saving",
    ),
    (
        "ListStageTemplates",
        "an authoring aid for the workflow editor; the editing tools arrive with workflow saving",
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
        "SetHandoff",
        "a person's choice on the Files tab; an agent saves the artifact and the workflow sets its default",
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

fn create_card() -> Value {
    object(
        vec![
            ("title", string("The card's title, 1 to 200 characters.")),
            (
                "description",
                string("What the card is about, up to 65536 characters."),
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
        ],
        &["cardId", "stageId"],
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
            (
                "result",
                string("One of the results the stage task lists, such as passed or failed."),
            ),
            (
                "summary",
                string("What you did or found, up to 10000 characters. Optional."),
            ),
        ],
        &["cardId", "result"],
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
    object(vec![("runId", string("The run's id."))], &["runId"])
}

fn add_note() -> Value {
    object(
        vec![
            ("cardId", string("The card's id.")),
            ("summary", string("One line, 1 to 200 characters.")),
            (
                "detail",
                string("More text, up to 10000 characters. Optional."),
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
                string("The document's full text, up to 1 MiB. Leave empty for a file."),
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
                string("The item's text as written after its checkbox, up to 1 KiB."),
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
            ("summary", string("One line, 1 to 120 characters.")),
            (
                "detail",
                string("More text, up to 2000 characters. Optional."),
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
                string("The choice, one line of 1 to 120 characters."),
            ),
            ("why", string("Why, up to 300 characters.")),
            (
                "rejected",
                json!({
                    "type": "array",
                    "maxItems": 10,
                    "items": object(
                        vec![
                            ("option", string("An option you turned down.")),
                            ("reason", string("Why.")),
                        ],
                        &["option"],
                    ),
                    "description": "The options you turned down. Optional.",
                }),
            ),
            (
                "detail",
                string("More text, up to 2000 characters. Optional."),
            ),
            (
                "supersedes",
                json!({ "type": "integer", "description": "The seq of an earlier decision this one replaces. Optional." }),
            ),
        ],
        &["cardId", "summary", "why"],
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
                json!({ "type": "integer", "description": "At most this many, up to 500. 0 means 100." }),
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
