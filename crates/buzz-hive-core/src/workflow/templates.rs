//! The stages the workflow builder can add, as shipped.

use std::collections::BTreeMap;

use crate::domain::workflow::Stage;
use crate::workflow::decode::decode_stage;

/// One stage the builder can add.
#[derive(Debug, Clone, PartialEq)]
pub struct StageTemplate {
    /// The file name without `.yaml`, and the stage's id.
    pub id: String,
    pub stage: Stage,
    /// The stage in the form `workflow.yaml`'s stages take, as JSON.
    pub json: String,
}

/// Reads every `<id>.yaml` in `files` as one stage, sorted by id. A file that does not decode
/// strictly, or whose stage id is not its file name, is an error naming the file.
pub fn load_stage_templates(
    files: &BTreeMap<String, String>,
) -> Result<Vec<StageTemplate>, String> {
    let mut out = Vec::new();
    for (name, text) in files {
        let Some(id) = name.strip_suffix(".yaml").filter(|_| !name.contains('/')) else {
            continue;
        };
        let (stage, json, findings) = decode_stage(name, text);
        if let Some(first) = findings.first() {
            return Err(format!("stage template {name}: {}", first.message));
        }
        if stage.id != id {
            return Err(format!(
                "stage template {name}: its id is {:?}, want {id:?}",
                stage.id
            ));
        }
        let json = json
            .ok_or_else(|| format!("stage template {name}: the file holds no stage"))?
            .to_string();
        out.push(StageTemplate {
            id: id.to_owned(),
            stage,
            json,
        });
    }
    Ok(out)
}
