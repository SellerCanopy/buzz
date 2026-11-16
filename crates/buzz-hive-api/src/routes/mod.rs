//! The procedures this service answers, and the ones it never will.
//!
//! Paths keep the Go engine's `/kiln.engine.v1.EngineService/<Method>` so its specs
//! and generated types carry over. `tests/procedures.rs` checks that every rpc in
//! `engine.proto` is routed, dropped with a reason, or still pending.

mod access;
mod activity;
mod agents;
mod artifacts;
mod assistant;
mod cards;
mod inbox;
pub mod intake;
mod merge;
pub mod poll;
mod runs;
mod settings;
mod stages;
mod status;
mod workflows;

use axum::Router;
use buzz_hive_core::access::Level;

use crate::app::App;
use crate::tools::TOOLS;
use crate::unary::unary;

/// Every procedure served now, with the level it needs.
pub const ROUTES: &[(&str, Level)] = &[
    ("GetStatus", Level::Read),
    ("WhoAmI", Level::None),
    ("ListCards", Level::Read),
    ("GetCard", Level::Read),
    ("CreateCard", Level::Run),
    ("CreateTask", Level::Run),
    ("AddNote", Level::Run),
    ("RemoveCard", Level::Run),
    ("ArchiveCards", Level::Run),
    ("RestoreCard", Level::Run),
    ("PollEvents", Level::Read),
    ("ListWorkflows", Level::Read),
    ("CheckWorkflow", Level::Read),
    ("ListTemplateVariables", Level::Read),
    ("ListStageTemplates", Level::Read),
    ("GetWorkflowDraft", Level::Read),
    ("EditWorkflow", Level::Read),
    ("SaveWorkflow", Level::Run),
    ("DuplicateWorkflow", Level::Run),
    ("DeleteWorkflow", Level::Run),
    ("SetWorkflowOptions", Level::Run),
    ("ExportWorkflow", Level::Run),
    ("ImportWorkflow", Level::Run),
    ("StartStage", Level::Run),
    ("StageDone", Level::Run),
    ("GetRun", Level::Read),
    ("ListRuns", Level::Read),
    ("GetSettings", Level::Read),
    // Run admits a card or workflow setting. The handler asks for Admin when the scope is
    // the community's own.
    ("SetSetting", Level::Run),
    ("SetAutoMode", Level::Run),
    ("ListAgents", Level::Read),
    ("ListAccess", Level::Read),
    ("ListPeople", Level::Read),
    ("SetAccessLevel", Level::Admin),
    ("SkipStage", Level::Run),
    ("PauseCard", Level::Run),
    ("ResumeCard", Level::Run),
    ("SetCardWorkflow", Level::Run),
    ("MoveCard", Level::Run),
    ("GetBrief", Level::Read),
    ("ListArtifacts", Level::Read),
    ("ListArtifactVersions", Level::Read),
    ("ReadArtifact", Level::Read),
    ("ListCardFiles", Level::Read),
    ("SetHandoff", Level::Run),
    ("SaveArtifact", Level::Run),
    ("UpdatePlan", Level::Run),
    ("LogWork", Level::Run),
    ("RecordDecision", Level::Run),
    ("ReadHistory", Level::Read),
    ("StopRun", Level::Run),
    ("RetryRun", Level::Run),
    ("PauseAll", Level::Admin),
    ("ResetCard", Level::Run),
    ("Approve", Level::Run),
    ("SendBack", Level::Run),
    ("SendMessage", Level::Run),
    ("AskGate", Level::Run),
    ("ListInbox", Level::Read),
    ("AnswerInbox", Level::Run),
    ("ListActivity", Level::Read),
    ("GetFullPayload", Level::Read),
    ("GetUsage", Level::Read),
    ("ListAutoDecisions", Level::Read),
    ("SaveIntakeRule", Level::Run),
    ("DeleteIntakeRule", Level::Run),
    ("RefreshIntake", Level::Run),
    ("ListIntakeRules", Level::Read),
    ("PreviewIntake", Level::Read),
    ("ListRepos", Level::Read),
    ("ListOwners", Level::Read),
    ("SearchIssues", Level::Read),
    ("ListIssueComments", Level::Read),
    ("MergeCard", Level::Run),
    ("SetMergeOrder", Level::Run),
    // The assistant's calls only start work and read what it offered: a result is a proposal,
    // and a person applies it with SaveWorkflow at their own level. The agent reports with
    // AssistantResult, which the handler holds to the job's own agent.
    ("StartChat", Level::Read),
    ("StartProposeWorkflowEdit", Level::Read),
    ("StartAdaptSkills", Level::Read),
    ("CancelJob", Level::Read),
    ("ListProposals", Level::Read),
    ("ResolveProposal", Level::Read),
    ("AssistantResult", Level::Read),
];

/// Procedures this service does not serve, each with the reason.
pub const DROPPED: &[(&str, &str)] = &[
    ("ImportPlugin", "skills come from agent images"),
    ("ImportSkill", "skills come from agent images"),
    ("StartTestRun", "no local runner (open question 10)"),
    ("ListSkillRepos", "skills come from agent images"),
    ("RefreshSkills", "skills come from agent images"),
    ("PlanBundle", "no skill bundles"),
    ("BundleSkills", "no skill bundles"),
    ("GetCostReport", "no cost figures"),
    ("GetCardCost", "no cost figures"),
    (
        "ListAgentProfiles",
        "agents are Buzz agents; see ListAgents",
    ),
    ("ListHosts", "no host sign-in"),
    ("StartSignIn", "team token secret"),
    ("FinishSignIn", "team token secret"),
    ("ImportCLIToken", "team token secret"),
    ("SetHostToken", "team token secret"),
    ("SignOut", "team token secret"),
    ("GetCardFilePath", "no local files"),
    ("ReportUsage", "no usage reporter"),
    (
        "ListConversations",
        "the assistant's thread in Buzz is the conversation",
    ),
    (
        "GetConversation",
        "the assistant's thread in Buzz is the conversation",
    ),
    (
        "DeleteConversation",
        "the assistant's thread in Buzz is the conversation",
    ),
];

/// The URL path of a procedure.
pub fn path(method: &str) -> String {
    format!("/kiln.engine.v1.EngineService/{method}")
}

/// The level a route needs. A route with a tool takes it from the tool's row, so the level
/// is written in one place for the route and the tool; `tests/tools_parity.rs` checks that
/// [`ROUTES`] agrees.
fn needs(method: &str) -> Level {
    if let Some(tool) = TOOLS.iter().find(|tool| tool.route == method) {
        return tool.needs;
    }
    ROUTES
        .iter()
        .find(|(name, _)| *name == method)
        .map(|(_, level)| *level)
        .unwrap_or_else(|| panic!("{method} is not in ROUTES"))
}

/// Adds the route for `method`, at the level its tool or [`ROUTES`] row names.
fn route<Req, Res, F, Fut>(r: Router<App>, method: &str, handler: F) -> Router<App>
where
    Req: serde::de::DeserializeOwned + Send + 'static,
    Res: serde::Serialize + Send + 'static,
    F: Fn(App, crate::unary::Ctx, Req) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = Result<Res, crate::error::HiveError>> + Send + 'static,
{
    r.route(&path(method), unary(needs(method), handler))
}

/// A route for every entry in [`ROUTES`].
pub fn router() -> Router<App> {
    let r = Router::new();
    let r = route(r, "GetStatus", status::get_status);
    let r = route(r, "WhoAmI", status::who_am_i);
    let r = route(r, "ListCards", cards::list_cards);
    let r = route(r, "GetCard", cards::get_card);
    let r = route(r, "CreateCard", cards::create_card);
    let r = route(r, "CreateTask", cards::create_task);
    let r = route(r, "AddNote", cards::add_note);
    let r = route(r, "RemoveCard", cards::remove_card);
    let r = route(r, "ArchiveCards", cards::archive_cards);
    let r = route(r, "RestoreCard", cards::restore_card);
    let r = route(r, "PollEvents", poll::poll_events);
    let r = route(r, "ListWorkflows", workflows::list_workflows);
    let r = route(r, "CheckWorkflow", workflows::check_workflow);
    let r = route(
        r,
        "ListTemplateVariables",
        workflows::list_template_variables,
    );
    let r = route(r, "ListStageTemplates", workflows::list_stage_templates);
    let r = route(r, "GetWorkflowDraft", workflows::get_workflow_draft);
    let r = route(r, "SaveWorkflow", workflows::save_workflow);
    let r = route(r, "DuplicateWorkflow", workflows::duplicate_workflow);
    let r = route(r, "DeleteWorkflow", workflows::delete_workflow);
    let r = route(r, "SetWorkflowOptions", workflows::set_workflow_options);
    let r = route(r, "ExportWorkflow", workflows::export_workflow);
    let r = route(r, "ImportWorkflow", workflows::import_workflow);
    let r = route(r, "EditWorkflow", workflows::edit_workflow);
    let r = route(r, "StartStage", runs::start_stage);
    let r = route(r, "StageDone", stages::stage_done);
    let r = route(r, "GetRun", runs::get_run);
    let r = route(r, "ListRuns", runs::list_runs);
    let r = route(r, "GetSettings", settings::get_settings);
    let r = route(r, "SetSetting", settings::set_setting);
    let r = route(r, "SetAutoMode", settings::set_auto_mode);
    let r = route(r, "ListAgents", agents::list_agents);
    let r = route(r, "ListAccess", access::list_access);
    let r = route(r, "ListPeople", access::list_people);
    let r = route(r, "SetAccessLevel", access::set_access_level);
    let r = route(r, "SkipStage", stages::skip_stage);
    let r = route(r, "PauseCard", stages::pause_card);
    let r = route(r, "ResumeCard", stages::resume_card);
    let r = route(r, "SetCardWorkflow", cards::set_card_workflow);
    let r = route(r, "MoveCard", stages::move_card);
    let r = route(r, "GetBrief", artifacts::get_brief);
    let r = route(r, "ListArtifacts", artifacts::list_artifacts);
    let r = route(r, "ListArtifactVersions", artifacts::list_artifact_versions);
    let r = route(r, "ReadArtifact", artifacts::read_artifact);
    let r = route(r, "ListCardFiles", artifacts::list_card_files);
    let r = route(r, "SetHandoff", artifacts::set_handoff);
    let r = route(r, "SaveArtifact", artifacts::save_artifact);
    let r = route(r, "UpdatePlan", artifacts::update_plan);
    let r = route(r, "LogWork", artifacts::log_work);
    let r = route(r, "RecordDecision", artifacts::record_decision);
    let r = route(r, "ReadHistory", artifacts::read_history);
    let r = route(r, "StopRun", runs::stop_run);
    let r = route(r, "RetryRun", runs::retry_run);
    let r = route(r, "PauseAll", stages::pause_all);
    let r = route(r, "ResetCard", stages::reset_card);
    let r = route(r, "Approve", inbox::approve);
    let r = route(r, "SendBack", inbox::send_back);
    let r = route(r, "SendMessage", inbox::send_message);
    let r = route(r, "AskGate", inbox::ask_gate);
    let r = route(r, "ListInbox", inbox::list_inbox);
    let r = route(r, "AnswerInbox", inbox::answer_inbox);
    let r = route(r, "ListActivity", activity::list_activity);
    let r = route(r, "GetFullPayload", activity::get_full_payload);
    let r = route(r, "ListAutoDecisions", activity::list_auto_decisions);
    let r = route(r, "SaveIntakeRule", intake::save_intake_rule);
    let r = route(r, "DeleteIntakeRule", intake::delete_intake_rule);
    let r = route(r, "RefreshIntake", intake::refresh_intake);
    let r = route(r, "ListIntakeRules", intake::list_intake_rules);
    let r = route(r, "PreviewIntake", intake::preview_intake);
    let r = route(r, "ListRepos", intake::list_repos);
    let r = route(r, "ListOwners", intake::list_owners);
    let r = route(r, "SearchIssues", intake::search_issues);
    let r = route(r, "ListIssueComments", intake::list_issue_comments);
    let r = route(r, "MergeCard", merge::merge_card);
    let r = route(r, "SetMergeOrder", merge::set_merge_order);
    let r = route(r, "StartChat", assistant::start_chat);
    let r = route(
        r,
        "StartProposeWorkflowEdit",
        assistant::start_propose_workflow_edit,
    );
    let r = route(r, "StartAdaptSkills", assistant::start_adapt_skills);
    let r = route(r, "CancelJob", assistant::cancel_job);
    let r = route(r, "ListProposals", assistant::list_proposals);
    let r = route(r, "ResolveProposal", assistant::resolve_proposal);
    let r = route(r, "AssistantResult", assistant::assistant_result);
    route(r, "GetUsage", status::get_usage)
}
