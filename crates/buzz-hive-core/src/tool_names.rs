//! The names of the Beehive tools that the text core writes for an agent points at.
//!
//! A stage task, a resume note and an artifact error each tell the agent to call a tool by
//! name. The api crate's tool list holds the same names, so both read them from here and a
//! rename cannot leave the text pointing at a tool that is gone.

/// Reports the result of the stage the agent was asked to do.
pub const STAGE_DONE: &str = "stage_done";
/// Reads a saved artifact.
pub const READ_ARTIFACT: &str = "read_artifact";
/// Saves a new version of an artifact.
pub const SAVE_ARTIFACT: &str = "save_artifact";
/// Checks items off a stage's plan artifact.
pub const UPDATE_PLAN: &str = "update_plan";
/// Moves the card to another stage in place of the route its result gives.
pub const MOVE_CARD: &str = "move_card";
/// Reports the result of an assistant job.
pub const ASSISTANT_RESULT: &str = "assistant_result";
/// Reads a workflow's files as text, with the hash to pass back when editing it.
pub const GET_WORKFLOW: &str = "get_workflow";
