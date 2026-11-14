//! Settings procedures: read what applies at a scope, and change one value.

use buzz_hive_core::access::{allow, Level};
use buzz_hive_core::config::{Entry, Invalid, Source as CoreSource, KEY_UPDATE_CHANNEL};
use buzz_hive_core::orchestrator::settings::{self, Scope};
use buzz_hive_core::orchestrator::Env;
use serde_json::Value;
use uuid::Uuid;

use crate::app::App;
use crate::error::HiveError;
use crate::proto::{
    GetSettingsRequest, GetSettingsResponse, InvalidSetting, SetSettingRequest, Setting, Source,
};
use crate::unary::Ctx;

pub async fn get_settings(
    app: App,
    ctx: Ctx,
    req: GetSettingsRequest,
) -> Result<GetSettingsResponse, HiveError> {
    let scope = scope(&req.scope, &req.scope_id, &req.workflow)?;
    let catalog = app.catalog.load();
    let env = env(&app, &catalog);
    let stage = (!req.stage_id.is_empty()).then_some(req.stage_id.as_str());
    let resolution = settings::get_settings(&env, &ctx.op(), &scope, stage).await?;
    Ok(GetSettingsResponse {
        settings: resolution.entries.into_iter().map(setting).collect(),
        invalid: resolution.invalid.into_iter().map(invalid).collect(),
    })
}

pub async fn set_setting(app: App, ctx: Ctx, req: SetSettingRequest) -> Result<Setting, HiveError> {
    let scope = scope(&req.scope, &req.scope_id, &req.workflow)?;
    // The route admits Run, which covers a card or a workflow. The community's own settings
    // hold the update channel, so they need Admin.
    let needed = scope.level_to_set();
    if needed > Level::Run {
        allow(ctx.actor.level, ctx.requester, needed).map_err(|d| {
            HiveError::PermissionDenied(format!(
                "needs {:?} to change these settings, you have {:?}",
                d.needed, d.effective
            ))
        })?;
    }
    let value = parse_value(&req.value_json)?;
    // Saving the update channel checks that the Beehive account is in it.
    if value.is_some() && req.key == KEY_UPDATE_CHANNEL {
        app.require_hive()?;
    }
    let catalog = app.catalog.load();
    let env = env(&app, &catalog);
    let (entry, events) = settings::set_setting(&env, &ctx.op(), &scope, &req.key, value).await?;
    app.bus.publish(&events);
    Ok(entry.map(setting).unwrap_or_else(|| Setting {
        key: req.key,
        value_json: "null".to_owned(),
        source: None,
    }))
}

fn env<'a>(app: &'a App, catalog: &'a buzz_hive_core::workflow::load::Catalog) -> Env<'a> {
    Env {
        store: app.store.as_ref(),
        directory: app.directory.as_ref(),
        catalog,
        hive: &app.hive_pubkey,
    }
}

fn scope(scope: &str, scope_id: &str, workflow: &str) -> Result<Scope, HiveError> {
    match scope {
        "user" => Ok(Scope::User),
        "workflow" if workflow.trim().is_empty() => Err(HiveError::InvalidArgument(
            "workflow is required for scope workflow".into(),
        )),
        "workflow" => Ok(Scope::Workflow(workflow.to_owned())),
        "card" => Uuid::parse_str(scope_id)
            .map(Scope::Card)
            .map_err(|_| HiveError::InvalidArgument("scope_id is not a card id".into())),
        "run" => Err(HiveError::InvalidArgument(
            "run settings are not supported yet".into(),
        )),
        other => Err(HiveError::InvalidArgument(format!(
            "unknown scope {other:?}; use user, workflow or card"
        ))),
    }
}

/// `None` for an empty or null `value_json`, which removes the setting.
fn parse_value(text: &str) -> Result<Option<Value>, HiveError> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(text)
        .map_err(|e| HiveError::InvalidArgument(format!("value_json is not JSON: {e}")))?;
    Ok((!value.is_null()).then_some(value))
}

fn source(source: CoreSource) -> Source {
    Source {
        layer: source.layer.as_str().to_owned(),
        r#where: source.where_,
    }
}

fn setting(entry: Entry) -> Setting {
    Setting {
        key: entry.key,
        value_json: entry.value.to_string(),
        source: Some(source(entry.source)),
    }
}

fn invalid(invalid: Invalid) -> InvalidSetting {
    InvalidSetting {
        key: invalid.key,
        source: Some(source(invalid.source)),
        message: invalid.message,
    }
}
