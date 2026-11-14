//! The procedures this service answers, and the ones it never will.
//!
//! Paths keep the Go engine's `/kiln.engine.v1.EngineService/<Method>` so its specs
//! and generated types carry over. `tests/procedures.rs` checks that every rpc in
//! `engine.proto` is routed, dropped with a reason, or still pending.

mod agents;
mod cards;
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
    ("StartStage", Level::Run),
    ("StageDone", Level::Run),
    ("GetRun", Level::Read),
    ("ListRuns", Level::Read),
    ("GetSettings", Level::Read),
    // Run admits a card or workflow setting. The handler asks for Admin when the scope is
    // the community's own.
    ("SetSetting", Level::Run),
    ("ListAgents", Level::Read),
    ("SkipStage", Level::Run),
    ("PauseCard", Level::Run),
    ("ResumeCard", Level::Run),
    ("SetCardWorkflow", Level::Run),
    ("MoveCard", Level::Run),
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
    ("WatchEvents", "replaced by PollEvents"),
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

/// A route for every entry in [`ROUTES`].
pub fn router(app: &App) -> Router<App> {
    Router::new()
        .route(
            &path("GetStatus"),
            unary(app.clone(), needs("GetStatus"), status::get_status),
        )
        .route(
            &path("WhoAmI"),
            unary(app.clone(), needs("WhoAmI"), status::who_am_i),
        )
        .route(
            &path("ListCards"),
            unary(app.clone(), needs("ListCards"), cards::list_cards),
        )
        .route(
            &path("GetCard"),
            unary(app.clone(), needs("GetCard"), cards::get_card),
        )
        .route(
            &path("CreateCard"),
            unary(app.clone(), needs("CreateCard"), cards::create_card),
        )
        .route(
            &path("CreateTask"),
            unary(app.clone(), needs("CreateTask"), cards::create_task),
        )
        .route(
            &path("AddNote"),
            unary(app.clone(), needs("AddNote"), cards::add_note),
        )
        .route(
            &path("RemoveCard"),
            unary(app.clone(), needs("RemoveCard"), cards::remove_card),
        )
        .route(
            &path("ArchiveCards"),
            unary(app.clone(), needs("ArchiveCards"), cards::archive_cards),
        )
        .route(
            &path("RestoreCard"),
            unary(app.clone(), needs("RestoreCard"), cards::restore_card),
        )
        .route(
            &path("PollEvents"),
            unary(app.clone(), needs("PollEvents"), poll::poll_events),
        )
        .route(
            &path("ListWorkflows"),
            unary(
                app.clone(),
                needs("ListWorkflows"),
                workflows::list_workflows,
            ),
        )
        .route(
            &path("CheckWorkflow"),
            unary(
                app.clone(),
                needs("CheckWorkflow"),
                workflows::check_workflow,
            ),
        )
        .route(
            &path("ListTemplateVariables"),
            unary(
                app.clone(),
                needs("ListTemplateVariables"),
                workflows::list_template_variables,
            ),
        )
        .route(
            &path("ListStageTemplates"),
            unary(
                app.clone(),
                needs("ListStageTemplates"),
                workflows::list_stage_templates,
            ),
        )
        .route(
            &path("StartStage"),
            unary(app.clone(), needs("StartStage"), runs::start_stage),
        )
        .route(
            &path("StageDone"),
            unary(app.clone(), needs("StageDone"), stages::stage_done),
        )
        .route(
            &path("GetRun"),
            unary(app.clone(), needs("GetRun"), runs::get_run),
        )
        .route(
            &path("ListRuns"),
            unary(app.clone(), needs("ListRuns"), runs::list_runs),
        )
        .route(
            &path("GetSettings"),
            unary(app.clone(), needs("GetSettings"), settings::get_settings),
        )
        .route(
            &path("SetSetting"),
            unary(app.clone(), needs("SetSetting"), settings::set_setting),
        )
        .route(
            &path("ListAgents"),
            unary(app.clone(), needs("ListAgents"), agents::list_agents),
        )
        .route(
            &path("SkipStage"),
            unary(app.clone(), needs("SkipStage"), stages::skip_stage),
        )
        .route(
            &path("PauseCard"),
            unary(app.clone(), needs("PauseCard"), stages::pause_card),
        )
        .route(
            &path("ResumeCard"),
            unary(app.clone(), needs("ResumeCard"), stages::resume_card),
        )
        .route(
            &path("SetCardWorkflow"),
            unary(
                app.clone(),
                needs("SetCardWorkflow"),
                cards::set_card_workflow,
            ),
        )
        .route(
            &path("MoveCard"),
            unary(app.clone(), needs("MoveCard"), stages::move_card),
        )
}
