//! Reading and changing settings.
//!
//! The settings table holds three scopes: `engine` (what the API calls `user`, since one
//! instance serves one community), `workflow` and `card`. The run layer is not stored here.
//! `config::resolve` merges the layers; this module loads them and checks what a person
//! writes.

use serde_json::{json, Value};
use uuid::Uuid;

use super::auto::settle;
use super::lookup::Memo;
use super::routes::ask_directory;
use super::{Ctx, Env, OpError};
use crate::access::Level;
use crate::config::{
    self, lookup, split_stage, validate, Entry, Layer, Layers, Resolution, KEY_AUTO_MODE,
    KEY_UPDATE_CHANNEL,
};
use crate::domain::events::Event;
use crate::ports::store::Tx;
use crate::workflow::load::Package;
use crate::workflow::names::check_param_value;

/// The text a person sees when the Beehive account is not in the update channel.
pub const JOIN_MESSAGE: &str = "add the Beehive account to this channel, then save again";

/// Whose settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// The community's own settings, such as the update channel.
    User,
    Workflow(String),
    Card(Uuid),
}

impl Scope {
    /// The level a person needs to change settings in this scope.
    pub fn level_to_set(&self) -> Level {
        match self {
            Self::User => Level::Admin,
            Self::Workflow(_) | Self::Card(_) => Level::Run,
        }
    }

    fn stored(&self) -> (&'static str, String) {
        match self {
            Self::User => ("engine", String::new()),
            Self::Workflow(name) => ("workflow", name.clone()),
            Self::Card(id) => ("card", id.to_string()),
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Workflow(_) => "workflow",
            Self::Card(_) => "card",
        }
    }
}

/// The layers of `scope`, and the package its stage and parameter checks use.
async fn layers_for<'a>(
    env: &Env<'a>,
    tx: &mut dyn Tx,
    scope: &Scope,
) -> Result<(Layers, Option<&'a Package>, String), OpError> {
    let mut layers = Layers {
        user: Layer::new(
            "community settings",
            tx.settings().list("engine", "").await?,
        ),
        ..Layers::default()
    };
    let workflow = match scope {
        Scope::User => None,
        Scope::Workflow(name) => Some(name.clone()),
        Scope::Card(id) => {
            let card = tx
                .cards()
                .get(*id)
                .await?
                .ok_or_else(|| OpError::NotFound(format!("card {id} does not exist")))?;
            layers.card = Layer::new(
                format!("card {}", card.title),
                tx.settings().list("card", &id.to_string()).await?,
            );
            Some(card.workflow).filter(|w| !w.is_empty())
        }
    };
    let mut package = None;
    let mut name = String::new();
    if let Some(workflow) = workflow {
        if matches!(scope, Scope::Workflow(_)) && env.catalog.get(&workflow).is_none() {
            return Err(OpError::NotFound(env.catalog.absent(&workflow)));
        }
        layers.workflow = Layer::new(
            format!("workflow {workflow}"),
            tx.settings().list("workflow", &workflow).await?,
        );
        package = env.catalog.get(&workflow);
        if let Some(p) = package {
            layers.params = p.meta.parameters.clone();
        }
        name = workflow;
    }
    Ok((layers, package, name))
}

fn resolve_in(
    layers: &Layers,
    package: Option<&Package>,
    name: &str,
    scope: &Scope,
    stage: Option<&str>,
) -> Result<Resolution, OpError> {
    let stage = match stage.filter(|s| !s.is_empty()) {
        None => None,
        Some(id) => {
            let package = package.ok_or_else(|| {
                OpError::InvalidArgument("stage_id needs a card or workflow with a package".into())
            })?;
            let (stage, _) = package
                .stage(id)
                .ok_or_else(|| OpError::NotFound(format!("workflow {name} has no stage {id}")))?;
            Some(stage)
        }
    };
    Ok(config::resolve(
        name,
        stage,
        layers,
        // Engine keys belong to the community's own settings.
        matches!(scope, Scope::User),
    ))
}

/// Every setting of `scope` with the layer that sets it. With a `stage`, the stage's own
/// package values and overrides count too.
pub async fn get_settings(
    env: &Env<'_>,
    ctx: &Ctx,
    scope: &Scope,
    stage: Option<&str>,
) -> Result<Resolution, OpError> {
    let mut tx = env.store.begin(ctx.community).await?;
    let (layers, package, name) = layers_for(env, &mut *tx, scope).await?;
    resolve_in(&layers, package, &name, scope, stage)
}

/// Sets or, with `value` of `None`, removes one setting, and returns what the key resolves
/// to afterwards. A workflow or card key may name one stage as `stage.<id>.<key>`.
///
/// The caller checks the actor's level against [`Scope::level_to_set`].
pub async fn set_setting(
    env: &Env<'_>,
    ctx: &Ctx,
    scope: &Scope,
    key: &str,
    value: Option<Value>,
) -> Result<(Option<Entry>, Vec<Event>), OpError> {
    let (stage_id, name) = split_stage(key);
    let known =
        lookup(name).ok_or_else(|| OpError::InvalidArgument(format!("unknown setting {key:?}")))?;
    if !known.allowed_in(scope.name()) {
        return Err(OpError::InvalidArgument(format!(
            "setting {name} cannot be set on a {}",
            scope.name()
        )));
    }
    if stage_id.is_some() && matches!(scope, Scope::User) {
        return Err(OpError::InvalidArgument(
            "a stage setting belongs to a workflow or a card".into(),
        ));
    }
    if let Some(value) = &value {
        validate(key, value).map_err(OpError::InvalidArgument)?;
    }
    let mut tx = env.store.begin(ctx.community).await?;
    let (layers, package, workflow) = layers_for(env, &mut *tx, scope).await?;
    if let Some(id) = stage_id {
        let package = package.ok_or_else(|| {
            OpError::InvalidArgument("this card has no workflow, so it has no stages".into())
        })?;
        if package.stage(id).is_none() {
            return Err(OpError::NotFound(format!(
                "workflow {workflow} has no stage {id}"
            )));
        }
    }
    if let (Some(param), Some(value)) = (config::param_key(name), &value) {
        if matches!(scope, Scope::User) {
            return Err(OpError::InvalidArgument(
                "set a parameter on a workflow or a card".into(),
            ));
        }
        let declared = layers
            .params
            .iter()
            .find(|p| p.name == param)
            .ok_or_else(|| {
                OpError::InvalidArgument(format!(
                    "parameter {param} is not declared by workflow {workflow}"
                ))
            })?;
        let text = value.as_str().unwrap_or_default();
        check_param_value(declared, text).map_err(OpError::InvalidArgument)?;
    }
    if let (KEY_UPDATE_CHANNEL, Some(value)) = (name, &value) {
        let channel = parse_channel(value)?;
        check_channel(env, ctx, channel, JOIN_MESSAGE).await?;
    }
    // Turning auto mode on answers what the card already waits on. Ask the directory for the
    // card's agents first, before the first append takes the log lock.
    let memo = Memo::new(env.directory);
    let env = &Env {
        directory: &memo,
        ..*env
    };
    let enabling = match (scope, name, &value) {
        (Scope::Card(id), KEY_AUTO_MODE, Some(Value::Bool(true))) => Some(*id),
        _ => None,
    };
    if let Some(id) = enabling {
        let card = tx.cards().get(id).await?;
        let package = card.and_then(|card| env.catalog.get(&card.workflow));
        if let (Some(package), Some(channel)) = (package, update_channel(&mut *tx).await?) {
            ask_directory(env, ctx, package, channel).await?;
        }
    }
    let (stored_scope, scope_id) = scope.stored();
    match &value {
        Some(value) => {
            tx.settings()
                .set(stored_scope, &scope_id, key, value.clone())
                .await?
        }
        None => tx.settings().remove(stored_scope, &scope_id, key).await?,
    }
    let card = match scope {
        Scope::Card(id) => Some(*id),
        _ => None,
    };
    let event = Event::new(
        "setting.changed",
        card,
        None,
        ctx.actor.clone(),
        json!({
            "scope": scope.name(),
            "scope_id": scope_id,
            "key": key,
            "value": value,
        }),
    );
    tx.events().append(event).await?;
    if let Some(id) = enabling {
        settle(env, ctx, &mut *tx, id).await?;
    }
    // Read the layers again so the answer reflects the write.
    let (layers, package, name) = layers_for(env, &mut *tx, scope).await?;
    let resolution = resolve_in(&layers, package, &name, scope, stage_id)?;
    let entry = resolution.entry(split_stage(key).1).cloned();
    let events = tx.commit().await?;
    Ok((entry, events))
}

/// The channel id a setting value holds.
pub fn parse_channel(value: &Value) -> Result<Uuid, OpError> {
    value
        .as_str()
        .and_then(|text| Uuid::parse_str(text).ok())
        .ok_or_else(|| OpError::InvalidArgument("update_channel must be a channel id".into()))
}

/// Checks that `channel` exists and the Beehive account is in it. `join` is what to tell a
/// person when it is not.
pub async fn check_channel(
    env: &Env<'_>,
    ctx: &Ctx,
    channel: Uuid,
    join: &str,
) -> Result<(), OpError> {
    if !env.directory.channel_exists(ctx.community, channel).await? {
        return Err(OpError::FailedPrecondition(format!(
            "channel {channel} does not exist"
        )));
    }
    if !env
        .directory
        .member_of_channel(ctx.community, channel, env.hive)
        .await?
    {
        return Err(OpError::FailedPrecondition(join.to_owned()));
    }
    Ok(())
}

/// The update channel, when one is set and valid.
pub async fn update_channel(tx: &mut dyn Tx) -> Result<Option<Uuid>, OpError> {
    let value = tx.settings().get("engine", "", KEY_UPDATE_CHANNEL).await?;
    value.as_ref().map(parse_channel).transpose()
}
