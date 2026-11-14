//! Settings resolved through six layers: engine defaults, user, workflow package,
//! workflow, card and run. Every resolved value carries the layer that set it.
//!
//! Ported from the Go engine's `core/config`. The agent keys (`agent.*`, `agents.*`) are
//! gone, because a stage's agent is a Buzz agent set up in Buzz. So are the cost, runner
//! and engine-lease keys, which have no counterpart here. `update_channel` is new: the
//! Buzz channel where the engine posts.
//!
//! The "user" layer is stored as the `engine` scope, since one instance serves one
//! community.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;

use crate::domain::workflow::{Duration, Parameter, Stage};
use crate::workflow::names::{check_param_value, param_name};

/// The key that names the channel the engine posts in.
pub const KEY_UPDATE_CHANNEL: &str = "update_channel";
/// A package parameter's value is the key `params.<name>`.
pub const PARAM_PREFIX: &str = "params.";
/// A workflow or card override for one stage is the key `stage.<id>.<key>`.
pub const STAGE_PREFIX: &str = "stage.";

/// The JSON shape and rules a setting's value follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueType {
    /// Any string.
    Text,
    /// A Go duration string of zero or more, such as `30m`.
    Duration,
    /// A whole number of zero or more.
    Count,
    /// `true` or `false`.
    Bool,
    /// A list of strings.
    List,
    /// The id of a Buzz channel.
    Channel,
}

/// What a key is set to when no layer sets it.
#[derive(Debug, Clone, Copy)]
enum Default {
    Unset,
    Text(&'static str),
    Count(i64),
    Bool(bool),
}

/// One setting the engine knows. A name holding `*` is a pattern: the `*` stands for a name
/// the key is about, such as a parameter.
#[derive(Debug, Clone, Copy)]
pub struct Key {
    pub name: &'static str,
    ty: ValueType,
    default: Default,
    /// An engine key, not a stage key.
    pub engine: bool,
    /// Set only on a workflow, for every stage or one.
    pub workflow: bool,
    /// Set only on a card, for every stage.
    pub card: bool,
}

const fn key(name: &'static str, ty: ValueType, default: Default) -> Key {
    Key {
        name,
        ty,
        default,
        engine: false,
        workflow: false,
        card: false,
    }
}

const fn engine(k: Key) -> Key {
    Key { engine: true, ..k }
}

const fn workflow(k: Key) -> Key {
    Key {
        workflow: true,
        ..k
    }
}

const fn card(k: Key) -> Key {
    Key { card: true, ..k }
}

/// The one table of settings, stage keys first.
pub const KEYS: &[Key] = &[
    key(
        "execution.timeout",
        ValueType::Duration,
        Default::Text("30m0s"),
    ),
    key("execution.retries.max", ValueType::Count, Default::Count(0)),
    key(
        "execution.retries.backoff",
        ValueType::Duration,
        Default::Text("1m0s"),
    ),
    key("git.base_branch", ValueType::Text, Default::Unset),
    key("git.target_branch", ValueType::Text, Default::Unset),
    key(
        "git.branch_name_format",
        ValueType::Text,
        Default::Text("kiln/{{issue.number}}"),
    ),
    key("params.*", ValueType::Text, Default::Unset),
    workflow(key(
        "side_effects.labels_on_finish",
        ValueType::List,
        Default::Unset,
    )),
    workflow(key(
        "side_effects.comment_on_finish",
        ValueType::Text,
        Default::Unset,
    )),
    workflow(key(
        "side_effects.state_labels.running",
        ValueType::Text,
        Default::Unset,
    )),
    workflow(key(
        "side_effects.state_labels.complete",
        ValueType::Text,
        Default::Unset,
    )),
    workflow(key(
        "side_effects.agent_moves_cards",
        ValueType::Bool,
        Default::Bool(false),
    )),
    card(key(
        "card.dont_merge",
        ValueType::Bool,
        Default::Bool(false),
    )),
    card(key("card.auto_mode", ValueType::Bool, Default::Bool(false))),
    card(key("card.loop_limit", ValueType::Count, Default::Count(10))),
    engine(key("default_repo", ValueType::Text, Default::Text(""))),
    engine(key(KEY_UPDATE_CHANNEL, ValueType::Channel, Default::Unset)),
];

impl Key {
    fn default_value(&self) -> Option<Value> {
        match self.default {
            Default::Unset => None,
            Default::Text(text) => Some(Value::String(text.to_owned())),
            Default::Count(n) => Some(Value::from(n)),
            Default::Bool(b) => Some(Value::Bool(b)),
        }
    }

    /// Whether a person may set the key in `scope`: `user`, `workflow` or `card`.
    pub fn allowed_in(&self, scope: &str) -> bool {
        match scope {
            "user" => !self.workflow && !self.card,
            "workflow" => !self.engine && !self.card,
            "card" => !self.engine && !self.workflow,
            _ => false,
        }
    }

    /// Decodes `value` as the key's type and applies its rules.
    fn check(&self, value: &Value) -> Result<(), String> {
        let fail = |want: &str| Err(format!("setting {}: {want}", self.name));
        match self.ty {
            ValueType::Text => match value {
                Value::String(_) => Ok(()),
                _ => fail("want a string"),
            },
            ValueType::Duration => match value.as_str().map(Duration::parse) {
                Some(Ok(d)) if d.nanos() >= 0 => Ok(()),
                _ => fail(&format!(
                    "want a duration of zero or more, such as \"30m\", got {value}"
                )),
            },
            ValueType::Count => match value.as_i64() {
                Some(n) if n >= 0 => Ok(()),
                _ => fail("want a whole number of zero or more"),
            },
            ValueType::Bool => match value {
                Value::Bool(_) => Ok(()),
                _ => fail("want true or false"),
            },
            ValueType::List => match value {
                Value::Array(items) if items.iter().all(Value::is_string) => Ok(()),
                _ => fail("want a list of strings"),
            },
            ValueType::Channel => match value.as_str().map(uuid::Uuid::parse_str) {
                Some(Ok(id)) if !id.is_nil() => Ok(()),
                _ => fail(&format!("want a channel id, got {value}")),
            },
        }
    }
}

/// The table entry for `name`, matching patterns.
pub fn lookup(name: &str) -> Option<&'static Key> {
    KEYS.iter().find(|k| matches(k.name, name))
}

fn matches(pattern: &str, name: &str) -> bool {
    match pattern.split_once('*') {
        None => pattern == name,
        Some((before, after)) => {
            name.len() > before.len() + after.len()
                && name.starts_with(before)
                && name.ends_with(after)
        }
    }
}

/// The parameter name in `params.<name>`, when it is a valid one.
pub fn param_key(key: &str) -> Option<&str> {
    key.strip_prefix(PARAM_PREFIX).filter(|n| param_name(n))
}

/// Splits a workflow or card override key into its stage id and setting key. The stage id
/// is `None` for a key that applies to every stage.
pub fn split_stage(key: &str) -> (Option<&str>, &str) {
    let Some(rest) = key.strip_prefix(STAGE_PREFIX) else {
        return (None, key);
    };
    for k in KEYS.iter().filter(|k| !k.engine && !k.card) {
        // The setting key is the longest suffix that names a stage key, so a stage id may
        // itself hold dots.
        for (i, byte) in rest.bytes().enumerate() {
            if byte == b'.' && i > 0 && matches(k.name, &rest[i + 1..]) {
                return (Some(&rest[..i]), &rest[i + 1..]);
            }
        }
    }
    (None, key)
}

/// Checks that `value` is a valid JSON value for `key`. For a workflow or card override
/// the key may name one stage (`stage.<id>.<key>`), and only stage keys are allowed there.
pub fn validate(key: &str, value: &Value) -> Result<(), String> {
    let (stage, name) = split_stage(key);
    let k = lookup(name).ok_or_else(|| format!("unknown setting {key:?}"))?;
    if stage.is_some() && k.engine {
        return Err(format!(
            "setting {name} applies to the engine, not to one stage"
        ));
    }
    k.check(value)
}

/// One layer's settings: JSON values by key. In a workflow or card layer a key may also
/// name one stage, which wins over the plain key in that layer.
pub type Values = BTreeMap<String, Value>;

/// One settings layer and where its values came from, such as `workflow engineering`.
#[derive(Debug, Clone, Default)]
pub struct Layer {
    pub where_: String,
    pub values: Values,
}

impl Layer {
    pub fn new(where_: impl Into<String>, values: Values) -> Self {
        Self {
            where_: where_.into(),
            values,
        }
    }
}

/// The layers above the engine defaults and the workflow package. Any may be empty.
#[derive(Debug, Clone, Default)]
pub struct Layers {
    pub user: Layer,
    pub workflow: Layer,
    pub card: Layer,
    pub run: Layer,
    /// The parameters the workflow package declares. Their defaults are the package
    /// layer's `params.<name>` values, and every stored `params.*` value is checked against
    /// them. With none declared a stored `params.*` value is skipped and reported.
    pub params: Vec<Parameter>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    Defaults,
    User,
    Package,
    Workflow,
    Card,
    Run,
}

impl LayerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Defaults => "defaults",
            Self::User => "user",
            Self::Package => "package",
            Self::Workflow => "workflow",
            Self::Card => "card",
            Self::Run => "run",
        }
    }
}

/// The layer that set a value, and where in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    pub layer: LayerKind,
    pub where_: String,
}

/// One resolved setting.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub key: String,
    /// `null` when no layer sets the key.
    pub value: Value,
    pub source: Source,
}

/// A stored value that is not valid for its key. Resolution skips it, so a lower layer's
/// value stands, and reports it here.
#[derive(Debug, Clone, PartialEq)]
pub struct Invalid {
    pub key: String,
    pub source: Source,
    pub message: String,
}

impl fmt::Display for Invalid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.source.where_, self.message)
    }
}

/// The winning entry for every key some layer sets, in key order, and the values skipped.
#[derive(Debug, Clone, Default)]
pub struct Resolution {
    pub entries: Vec<Entry>,
    pub invalid: Vec<Invalid>,
}

impl Resolution {
    pub fn entry(&self, key: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.key == key)
    }

    /// The value of `key`, `None` when no layer sets it.
    pub fn value(&self, key: &str) -> Option<&Value> {
        self.entry(key).map(|e| &e.value).filter(|v| !v.is_null())
    }

    /// Every `params.<name>` that has a value, by name.
    pub fn param_values(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .filter_map(|e| {
                let name = param_key(&e.key)?;
                Some((name.to_owned(), e.value.as_str()?.to_owned()))
            })
            .collect()
    }
}

/// The settings a stage's own fields set, by key. Fields left unset set nothing.
pub fn package_values(stage: &Stage) -> Values {
    let mut values = Values::new();
    let x = &stage.execution;
    if x.timeout.nanos() != 0 {
        values.insert("execution.timeout".into(), x.timeout.to_string().into());
    }
    if x.retries.max != 0 {
        values.insert("execution.retries.max".into(), x.retries.max.into());
    }
    if x.retries.backoff.nanos() != 0 {
        values.insert(
            "execution.retries.backoff".into(),
            x.retries.backoff.to_string().into(),
        );
    }
    values
}

struct SourceLayer<'a> {
    kind: LayerKind,
    where_: String,
    values: &'a Values,
    /// Keys may name a stage.
    scoped: bool,
}

/// Resolves every key through the six layers. `pkg` names the workflow package. With a
/// `stage`, stage keys include its package values and the stage-specific overrides of the
/// workflow and card layers; without one only the plain keys of each layer apply. A
/// pattern key appears once for each name some layer sets.
///
/// With `with_engine` false the engine keys are left out.
pub fn resolve(pkg: &str, stage: Option<&Stage>, layers: &Layers, with_engine: bool) -> Resolution {
    let mut defaults = Values::new();
    for k in KEYS {
        if let Some(value) = k.default_value() {
            defaults.insert(k.name.to_owned(), value);
        }
    }
    let mut package = stage.map(package_values).unwrap_or_default();
    for p in &layers.params {
        if !p.default.is_empty() {
            package.insert(
                format!("{PARAM_PREFIX}{}", p.name),
                Value::String(p.default.clone()),
            );
        }
    }
    let sources = [
        SourceLayer {
            kind: LayerKind::Defaults,
            where_: "engine defaults".into(),
            values: &defaults,
            scoped: false,
        },
        SourceLayer {
            kind: LayerKind::User,
            where_: layers.user.where_.clone(),
            values: &layers.user.values,
            scoped: false,
        },
        SourceLayer {
            kind: LayerKind::Package,
            where_: format!("package {pkg}"),
            values: &package,
            scoped: false,
        },
        SourceLayer {
            kind: LayerKind::Workflow,
            where_: layers.workflow.where_.clone(),
            values: &layers.workflow.values,
            scoped: true,
        },
        SourceLayer {
            kind: LayerKind::Card,
            where_: layers.card.where_.clone(),
            values: &layers.card.values,
            scoped: true,
        },
        SourceLayer {
            kind: LayerKind::Run,
            where_: layers.run.where_.clone(),
            values: &layers.run.values,
            scoped: false,
        },
    ];
    resolve_sources(
        &sources,
        stage.map(|s| s.id.as_str()),
        with_engine,
        &layers.params,
    )
}

fn resolve_sources(
    sources: &[SourceLayer<'_>],
    stage: Option<&str>,
    with_engine: bool,
    params: &[Parameter],
) -> Resolution {
    let mut won: BTreeMap<String, Entry> = BTreeMap::new();
    // A named key with no default resolves to null from the defaults layer, meaning "not
    // set", so every named key has an entry.
    for k in KEYS {
        if k.default_value().is_none() && !k.name.contains('*') && (with_engine || !k.engine) {
            won.insert(
                k.name.to_owned(),
                Entry {
                    key: k.name.to_owned(),
                    value: Value::Null,
                    source: Source {
                        layer: sources[0].kind,
                        where_: sources[0].where_.clone(),
                    },
                },
            );
        }
    }
    let mut invalid = Vec::new();
    for layer in sources {
        let mut apply = |key: &str, where_: String, value: &Value| {
            let Some(k) = lookup(key) else { return };
            if k.engine && !with_engine {
                return;
            }
            let source = Source {
                layer: layer.kind,
                where_,
            };
            let checked = k.check(value).and_then(|()| {
                if key.starts_with(PARAM_PREFIX) {
                    check_param(params, key, value)
                } else {
                    Ok(())
                }
            });
            match checked {
                Ok(()) => {
                    won.insert(
                        key.to_owned(),
                        Entry {
                            key: key.to_owned(),
                            value: value.clone(),
                            source,
                        },
                    );
                }
                Err(message) => invalid.push(Invalid {
                    key: key.to_owned(),
                    source,
                    message,
                }),
            }
        };
        // Plain keys first, then stage-specific ones, so the latter win inside the layer.
        for (key, value) in layer.values {
            if split_stage(key).0.is_none() {
                apply(key, layer.where_.clone(), value);
            }
        }
        let Some(stage) = stage.filter(|_| layer.scoped) else {
            continue;
        };
        for (key, value) in layer.values {
            let (named, name) = split_stage(key);
            if named == Some(stage) {
                apply(name, format!("{}, stage {stage}", layer.where_), value);
            }
        }
    }
    Resolution {
        entries: won.into_values().collect(),
        invalid,
    }
}

/// Checks a `params.*` value against the declared parameters. A name no parameter declares
/// is invalid.
fn check_param(declared: &[Parameter], key: &str, value: &Value) -> Result<(), String> {
    let Some(name) = param_key(key) else {
        return Err(format!(
            "setting {key}: a parameter name starts with a lowercase letter and uses only a-z, 0-9 and _"
        ));
    };
    let Some(text) = value.as_str() else {
        return Err(format!("setting {key}: want a string"));
    };
    let parameter = declared
        .iter()
        .find(|p| p.name == name)
        .ok_or_else(|| format!("parameter {name} is not declared by the workflow"))?;
    check_param_value(parameter, text)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::domain::workflow::{ParamType, KIND_PROMPT};

    fn research() -> Stage {
        Stage {
            id: "research".into(),
            kind: KIND_PROMPT.into(),
            prompt: "Research".into(),
            ..Stage::default()
        }
    }

    fn values(pairs: &[(&str, Value)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    fn layer(where_: &str, pairs: &[(&str, Value)]) -> Layer {
        Layer::new(where_, values(pairs))
    }

    /// The winning value of `key`, and the layer and place that set it.
    fn won(r: &Resolution, key: &str) -> (Value, LayerKind, String) {
        let e = r.entry(key).unwrap_or_else(|| panic!("no entry for {key}"));
        (e.value.clone(), e.source.layer, e.source.where_.clone())
    }

    fn stage_resolution(stage: &Stage, layers: &Layers) -> Resolution {
        resolve("demo", Some(stage), layers, false)
    }

    #[test]
    fn defaults_stand_when_no_layer_sets_a_key() {
        let r = stage_resolution(&research(), &Layers::default());
        assert!(r.invalid.is_empty());
        for (key, value) in [
            ("execution.timeout", json!("30m0s")),
            ("execution.retries.max", json!(0)),
            ("execution.retries.backoff", json!("1m0s")),
        ] {
            assert_eq!(
                won(&r, key),
                (value, LayerKind::Defaults, "engine defaults".into()),
                "{key}"
            );
        }
        assert_eq!(won(&r, "git.target_branch").0, Value::Null);
    }

    #[test]
    fn the_user_layer_beats_the_defaults() {
        let layers = Layers {
            user: layer("user settings", &[("execution.retries.max", json!(3))]),
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(
            won(&r, "execution.retries.max"),
            (json!(3), LayerKind::User, "user settings".into())
        );
        assert_eq!(won(&r, "execution.timeout").1, LayerKind::Defaults);
    }

    #[test]
    fn the_package_layer_beats_the_user_layer() {
        let mut stage = research();
        stage.execution.timeout = Duration::parse("5m").unwrap();
        let layers = Layers {
            user: layer("user settings", &[("execution.timeout", json!("1h"))]),
            ..Layers::default()
        };
        let r = stage_resolution(&stage, &layers);
        assert_eq!(
            won(&r, "execution.timeout"),
            (json!("5m0s"), LayerKind::Package, "package demo".into())
        );
    }

    #[test]
    fn a_stage_override_beats_the_plain_key_in_its_own_layer() {
        let layers = Layers {
            workflow: layer(
                "workflow engineering",
                &[
                    ("execution.retries.max", json!(1)),
                    ("stage.research.execution.retries.max", json!(5)),
                    // Another stage's override does not apply.
                    ("stage.plan.execution.retries.backoff", json!("9m")),
                ],
            ),
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(
            won(&r, "execution.retries.max"),
            (
                json!(5),
                LayerKind::Workflow,
                "workflow engineering, stage research".into()
            )
        );
        assert_eq!(won(&r, "execution.retries.backoff").1, LayerKind::Defaults);
    }

    #[test]
    fn the_card_layer_beats_the_workflow_and_the_run_layer_beats_the_card() {
        let layers = Layers {
            workflow: layer(
                "workflow engineering",
                &[("execution.timeout", json!("10m"))],
            ),
            card: layer(
                "card 12",
                &[
                    ("execution.timeout", json!("20m")),
                    ("stage.research.execution.retries.max", json!(2)),
                ],
            ),
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(
            won(&r, "execution.timeout"),
            (json!("20m"), LayerKind::Card, "card 12".into())
        );
        assert_eq!(
            won(&r, "execution.retries.max"),
            (json!(2), LayerKind::Card, "card 12, stage research".into())
        );
        let layers = Layers {
            run: layer("run 40", &[("execution.timeout", json!("1m"))]),
            ..layers
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(
            won(&r, "execution.timeout"),
            (json!("1m"), LayerKind::Run, "run 40".into())
        );
    }

    #[test]
    fn an_invalid_stored_value_is_skipped_and_reported() {
        let layers = Layers {
            user: layer("user settings", &[("execution.timeout", json!("20m"))]),
            workflow: layer("workflow 1 (v)", &[("execution.timeout", json!("soon"))]),
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(r.invalid.len(), 1);
        let text = r.invalid[0].to_string();
        assert!(
            text.contains("workflow 1 (v)") && text.contains("soon"),
            "{text}"
        );
        assert_eq!(
            won(&r, "execution.timeout"),
            (json!("20m"), LayerKind::User, "user settings".into())
        );
    }

    #[test]
    fn the_agent_moves_cards_key_defaults_off_and_a_stage_value_wins() {
        let key = "side_effects.agent_moves_cards";
        let on = json!(true);
        let cases = [
            (values(&[]), false),
            (values(&[(key, on.clone())]), true),
            (
                values(&[
                    (key, on),
                    (
                        "stage.research.side_effects.agent_moves_cards",
                        json!(false),
                    ),
                ]),
                false,
            ),
        ];
        for (workflow_values, want) in cases {
            let layers = Layers {
                workflow: Layer::new("workflow engineering", workflow_values),
                ..Layers::default()
            };
            let r = stage_resolution(&research(), &layers);
            assert_eq!(r.value(key), Some(&json!(want)));
        }
    }

    #[test]
    fn list_returns_every_named_key_with_its_source() {
        let layers = Layers {
            user: layer("user settings", &[("execution.retries.max", json!(4))]),
            ..Layers::default()
        };
        let r = resolve("", None, &layers, true);
        for k in KEYS.iter().filter(|k| !k.name.contains('*')) {
            assert!(r.entry(k.name).is_some(), "list has no {}", k.name);
        }
        assert_eq!(
            won(&r, "execution.retries.max"),
            (json!(4), LayerKind::User, "user settings".into())
        );
        assert_eq!(
            won(&r, "git.base_branch"),
            (Value::Null, LayerKind::Defaults, "engine defaults".into())
        );
    }

    #[test]
    fn engine_keys_are_left_out_unless_asked_for() {
        let layers = Layers {
            user: layer(
                "user settings",
                &[(
                    KEY_UPDATE_CHANNEL,
                    json!(uuid::Uuid::from_u128(7).to_string()),
                )],
            ),
            ..Layers::default()
        };
        assert!(resolve("demo", Some(&research()), &layers, false)
            .entry(KEY_UPDATE_CHANNEL)
            .is_none());
        assert!(resolve("", None, &layers, true)
            .value(KEY_UPDATE_CHANNEL)
            .is_some());
    }

    #[test]
    fn validate_accepts_and_refuses_the_expected_values() {
        let channel = json!(uuid::Uuid::from_u128(7).to_string());
        let cases: Vec<(&str, Value, bool)> = vec![
            ("execution.timeout", json!("45m"), true),
            ("execution.timeout", json!("soon"), false),
            ("execution.timeout", json!("-1m"), false),
            ("execution.retries.max", json!(3), true),
            ("execution.retries.max", json!(-1), false),
            ("execution.retries.max", json!(1.5), false),
            ("card.loop_limit", json!(10), true),
            ("card.auto_mode", json!(true), true),
            ("card.auto_mode", json!("yes"), false),
            ("side_effects.labels_on_finish", json!([]), true),
            ("side_effects.labels_on_finish", json!("done"), false),
            ("params.depth", json!("deep"), true),
            ("params.depth", json!(3), false),
            ("stage.research.execution.timeout", json!("1h"), true),
            ("stage.research.update_channel", channel.clone(), false),
            (KEY_UPDATE_CHANNEL, channel, true),
            (KEY_UPDATE_CHANNEL, json!("general"), false),
            (
                KEY_UPDATE_CHANNEL,
                json!(uuid::Uuid::nil().to_string()),
                false,
            ),
            ("agent.colour", json!("blue"), false),
            ("enabled", json!(true), false),
        ];
        for (key, value, ok) in cases {
            assert_eq!(validate(key, &value).is_ok(), ok, "{key} {value}");
        }
    }

    #[test]
    fn split_stage_finds_the_stage_and_the_key() {
        for (key, stage, name) in [
            ("execution.timeout", None, "execution.timeout"),
            (
                "stage.research.execution.timeout",
                Some("research"),
                "execution.timeout",
            ),
            ("stage.v1.2.params.mode", Some("v1.2"), "params.mode"),
            ("stage.research", None, "stage.research"),
        ] {
            assert_eq!(split_stage(key), (stage, name), "{key}");
        }
    }

    #[test]
    fn a_key_is_allowed_only_in_the_scopes_it_belongs_to() {
        let allowed = |key: &str, scope: &str| lookup(key).unwrap().allowed_in(scope);
        assert!(allowed(KEY_UPDATE_CHANNEL, "user"));
        assert!(!allowed(KEY_UPDATE_CHANNEL, "card"));
        assert!(allowed("execution.timeout", "user"));
        assert!(allowed("execution.timeout", "workflow"));
        assert!(allowed("execution.timeout", "card"));
        assert!(allowed("card.auto_mode", "card"));
        assert!(!allowed("card.auto_mode", "workflow"));
        assert!(allowed("side_effects.agent_moves_cards", "workflow"));
        assert!(!allowed("side_effects.agent_moves_cards", "card"));
        assert!(!allowed("execution.timeout", "run"));
    }

    fn declared() -> Vec<Parameter> {
        vec![
            Parameter {
                name: "depth".into(),
                kind: Some(ParamType::Choice),
                choices: vec!["normal".into(), "deep".into()],
                default: "normal".into(),
                ..Parameter::default()
            },
            Parameter {
                name: "retries".into(),
                kind: Some(ParamType::Number),
                ..Parameter::default()
            },
        ]
    }

    fn param_resolution(layers: Layers) -> Resolution {
        stage_resolution(
            &research(),
            &Layers {
                params: declared(),
                ..layers
            },
        )
    }

    #[test]
    fn params_resolve_through_the_layers() {
        // The package default stands with no other layer.
        let r = param_resolution(Layers::default());
        assert!(r.invalid.is_empty());
        assert_eq!(
            won(&r, "params.depth"),
            (json!("normal"), LayerKind::Package, "package demo".into())
        );
        assert!(r.entry("params.retries").is_none(), "no default, no value");
        assert_eq!(
            r.param_values(),
            BTreeMap::from([("depth".to_owned(), "normal".to_owned())])
        );

        // Each layer above beats the one below.
        let workflow = layer("workflow eng", &[("params.depth", json!("deep"))]);
        let card = layer(
            "card 12",
            &[
                ("params.depth", json!("normal")),
                ("stage.research.params.retries", json!("4")),
            ],
        );
        let run = layer("run 40", &[("params.retries", json!("7"))]);
        let r = param_resolution(Layers {
            workflow: workflow.clone(),
            ..Layers::default()
        });
        assert_eq!(
            won(&r, "params.depth"),
            (json!("deep"), LayerKind::Workflow, "workflow eng".into())
        );
        let r = param_resolution(Layers {
            workflow: workflow.clone(),
            card: card.clone(),
            ..Layers::default()
        });
        assert_eq!(won(&r, "params.depth").1, LayerKind::Card);
        assert_eq!(
            won(&r, "params.retries"),
            (
                json!("4"),
                LayerKind::Card,
                "card 12, stage research".into()
            )
        );
        let r = param_resolution(Layers {
            workflow,
            card,
            run,
            ..Layers::default()
        });
        assert_eq!(
            won(&r, "params.retries"),
            (json!("7"), LayerKind::Run, "run 40".into())
        );
    }

    #[test]
    fn a_wrong_typed_or_undeclared_param_is_skipped_and_reported() {
        let workflow = layer(
            "workflow eng",
            &[
                ("params.depth", json!("deeper")),
                ("params.retries", json!("many")),
                ("params.colour", json!("red")),
            ],
        );
        let r = param_resolution(Layers {
            workflow,
            ..Layers::default()
        });
        assert_eq!(r.invalid.len(), 3, "{:?}", r.invalid);
        let text: String = r.invalid.iter().map(ToString::to_string).collect();
        for want in ["deeper", "many", "parameter colour is not declared"] {
            assert!(text.contains(want), "{text} should name {want}");
        }
        // The default stands where the value was skipped.
        assert_eq!(won(&r, "params.depth").1, LayerKind::Package);
        assert!(r.entry("params.retries").is_none());
    }

    #[test]
    fn with_no_declared_params_every_stored_value_is_reported() {
        let workflow = layer(
            "workflow eng",
            &[
                ("params.depth", json!("deep")),
                ("params.retries", json!("3")),
            ],
        );
        let layers = Layers {
            workflow,
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(r.invalid.len(), 2);
        assert!(r.param_values().is_empty());
    }

    #[test]
    fn a_param_key_with_a_dot_is_invalid() {
        let workflow = layer("workflow eng", &[("params.a.b", json!("x"))]);
        let r = param_resolution(Layers {
            workflow,
            ..Layers::default()
        });
        assert_eq!(r.invalid.len(), 1, "{:?}", r.invalid);
    }

    #[test]
    fn param_key_reads_valid_names_only() {
        for (key, want) in [("params.depth", "depth"), ("params.a_1", "a_1")] {
            assert_eq!(param_key(key), Some(want));
        }
        for key in [
            "params.",
            "params.Depth",
            "params.a.b",
            "agent.model",
            "params",
        ] {
            assert_eq!(param_key(key), None, "{key}");
        }
    }

    #[test]
    fn the_branch_keys_default_and_can_be_set_by_a_workflow() {
        let r = stage_resolution(&research(), &Layers::default());
        assert_eq!(
            won(&r, "git.branch_name_format"),
            (
                json!("kiln/{{issue.number}}"),
                LayerKind::Defaults,
                "engine defaults".into()
            )
        );
        let layers = Layers {
            workflow: layer(
                "workflow eng",
                &[
                    ("git.target_branch", json!("release")),
                    ("git.branch_name_format", json!("feat/{{issue.number}}")),
                ],
            ),
            ..Layers::default()
        };
        let r = stage_resolution(&research(), &layers);
        assert_eq!(won(&r, "git.target_branch").0, json!("release"));
        assert_eq!(won(&r, "git.branch_name_format").1, LayerKind::Workflow);
    }
}
