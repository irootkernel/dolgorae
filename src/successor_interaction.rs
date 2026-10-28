//! Successor permission grants and protected MCP forms, scoped to one Turn.
use crate::interaction::{Question, QuestionOption};
use crate::turn::TurnError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PermissionPayload {
    pub title: String,
    pub reason: String,
    pub requested_permissions: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct McpPayload {
    pub title: String,
    pub server_name: String,
    pub questions: Vec<Question>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "mcp_policy_only"
    )]
    pub policy_only: Option<McpPolicyOnly>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) enum McpPolicyOnly {
    #[serde(rename = "RUNTIME_VARIANT_UNSUPPORTED")]
    RuntimeVariantUnsupported,
}

fn mcp_policy_only<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<McpPolicyOnly>, D::Error> {
    let value = String::deserialize(deserializer)?;
    match value.as_str() {
        "RUNTIME_VARIANT_UNSUPPORTED" => Ok(Some(McpPolicyOnly::RuntimeVariantUnsupported)),
        _ => Err(serde::de::Error::unknown_variant(
            &value,
            &["RUNTIME_VARIANT_UNSUPPORTED"],
        )),
    }
}

pub(crate) fn supported(method: &str) -> bool {
    matches!(
        method,
        "item/permissions/requestApproval" | "mcpServer/elicitation/request"
    )
}

fn invalid(id: u64) -> TurnError {
    TurnError::InteractionResponseInvalid { request_id: id }
}

fn text(value: Option<&Value>, maximum: usize) -> Option<String> {
    value?
        .as_str()
        .filter(|v| !v.is_empty() && v.len() <= maximum)
        .map(str::to_owned)
}

fn contained(path: &str, cwd: &Path) -> bool {
    let path = Path::new(path);
    if path.as_os_str().is_empty() || path.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    if !absolute.starts_with(cwd) {
        return false;
    }
    let mut existing = absolute.as_path();
    loop {
        match std::fs::symlink_metadata(existing) {
            Ok(_) => return std::fs::canonicalize(existing).is_ok_and(|p| p.starts_with(cwd)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => match existing.parent() {
                Some(parent) => existing = parent,
                None => return false,
            },
            Err(_) => return false,
        }
    }
}

/// Identifier -> exact native grant fragment. Unknown expansion is a policy decline.
fn grants(params: &Value, cwd: &Path, write: bool) -> Option<BTreeMap<String, Value>> {
    if params.get("cwd")?.as_str()? != cwd.to_str()? {
        return None;
    }
    let permissions = params.get("permissions")?.as_object()?;
    if permissions
        .keys()
        .any(|key| !matches!(key.as_str(), "fileSystem" | "network"))
    {
        return None;
    }
    let mut grants = BTreeMap::new();
    if permissions
        .get("network")
        .is_some_and(|v| !v.is_null() && v.get("enabled") != Some(&json!(false)))
    {
        return None;
    }
    if let Some(fs) = permissions.get("fileSystem").filter(|v| !v.is_null()) {
        let fs = fs.as_object()?;
        if fs.keys().any(|key| {
            !matches!(
                key.as_str(),
                "read" | "write" | "entries" | "globScanMaxDepth"
            )
        }) || fs.get("globScanMaxDepth").is_some_and(|v| !v.is_null())
        {
            return None;
        }
        for access in ["read", "write"] {
            if let Some(paths) = fs.get(access).filter(|v| !v.is_null()) {
                for path in paths.as_array()? {
                    let path = text(Some(path), 4096)?;
                    if !contained(&path, cwd) || (access == "write" && !write) {
                        return None;
                    }
                    grants.insert(
                        format!("filesystem_{access}:{path}"),
                        json!({access:[path]}),
                    );
                }
            }
        }
        if let Some(entries) = fs.get("entries").filter(|v| !v.is_null()) {
            for entry in entries.as_array()? {
                let access = entry.get("access")?.as_str()?;
                let path = text(entry.pointer("/path/path"), 4096)?;
                if entry.pointer("/path/type")?.as_str()? != "path"
                    || !matches!(access, "read" | "write")
                    || !contained(&path, cwd)
                    || (access == "write" && !write)
                {
                    return None;
                }
                grants.insert(
                    format!("filesystem_{access}:{path}"),
                    json!({"entries":[entry]}),
                );
            }
        }
    }
    (grants.len() <= 64).then_some(grants)
}

pub(crate) fn permission(params: &Value, cwd: &Path, write: bool) -> (PermissionPayload, bool) {
    let grants = grants(params, cwd, write);
    (
        PermissionPayload {
            title: "Permission approval".into(),
            reason: text(params.get("reason"), 4096).unwrap_or_default(),
            requested_permissions: grants
                .as_ref()
                .map(|v| v.keys().cloned().collect())
                .unwrap_or_default(),
        },
        grants.is_some_and(|v| !v.is_empty()),
    )
}

pub(crate) fn mcp(params: &Value) -> (McpPayload, bool) {
    let mut payload = McpPayload {
        title: text(params.get("message"), 4096).unwrap_or_else(|| "MCP input".into()),
        server_name: text(params.get("serverName"), 512).unwrap_or_default(),
        questions: Vec::new(),
        policy_only: None,
    };
    let build = || -> Option<Vec<Question>> {
        if params.get("mode")?.as_str()? != "form" || payload.server_name.is_empty() {
            return None;
        }
        let schema = params.get("requestedSchema")?;
        if schema.get("type")?.as_str()? != "object" {
            return None;
        }
        let properties = schema.get("properties")?.as_object()?;
        if properties.is_empty() || properties.len() > 64 {
            return None;
        }
        let required: BTreeSet<&str> = match schema.get("required").filter(|v| !v.is_null()) {
            Some(v) => v
                .as_array()?
                .iter()
                .map(Value::as_str)
                .collect::<Option<_>>()?,
            None => BTreeSet::new(),
        };
        if required.iter().any(|name| !properties.contains_key(*name)) {
            return None;
        }
        properties
            .iter()
            .map(|(id, field)| {
                if id.is_empty() || id.len() > 128 {
                    return None;
                }
                let kind = field.get("type")?.as_str()?;
                let options = match kind {
                    "boolean" => Some(vec![
                        QuestionOption {
                            label: "true".into(),
                            description: String::new(),
                        },
                        QuestionOption {
                            label: "false".into(),
                            description: String::new(),
                        },
                    ]),
                    "string" => {
                        if field.get("oneOf").is_some()
                            || field.get("format").is_some_and(|v| !v.is_null())
                        {
                            return None;
                        }
                        match field.get("enum") {
                            Some(v) => Some(
                                v.as_array()?
                                    .iter()
                                    .map(|v| {
                                        Some(QuestionOption {
                                            label: text(Some(v), 256)?,
                                            description: String::new(),
                                        })
                                    })
                                    .collect::<Option<Vec<_>>>()?,
                            ),
                            None => None,
                        }
                    }
                    _ => return None,
                };
                if options
                    .as_ref()
                    .is_some_and(|o| o.is_empty() || o.len() > 32)
                {
                    return None;
                }
                Some(Question {
                    id: id.clone(),
                    header: text(field.get("title"), 512).unwrap_or_else(|| id.clone()),
                    question: text(field.get("description"), 4096).unwrap_or_else(|| id.clone()),
                    is_other: options.is_none(),
                    is_secret: true,
                    options,
                })
            })
            .collect()
    };
    let questions = build();
    match questions {
        Some(questions) => {
            payload.questions = questions;
            (payload, true)
        }
        None => (
            McpPayload {
                title: "MCP input".into(),
                server_name: String::new(),
                questions: Vec::new(),
                policy_only: Some(McpPolicyOnly::RuntimeVariantUnsupported),
            },
            false,
        ),
    }
}

pub(crate) fn response(
    method: &str,
    params: &Value,
    response: &Value,
    cwd: &Path,
    write: bool,
    id: u64,
) -> Result<Value, TurnError> {
    let object = response.as_object().ok_or_else(|| invalid(id))?;
    let decision = response
        .get("decision")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(id))?;
    if !matches!(decision, "accept_once" | "decline" | "cancel") {
        return Err(invalid(id));
    }
    if method == "item/permissions/requestApproval" {
        if object.len() != 2 {
            return Err(invalid(id));
        }
        let selected = response
            .get("granted_permissions")
            .and_then(Value::as_array)
            .ok_or_else(|| invalid(id))?;
        if decision != "accept_once" && !selected.is_empty() {
            return Err(invalid(id));
        }
        let allowed = grants(params, cwd, write);
        let mut fs = serde_json::Map::new();
        let mut seen = BTreeSet::new();
        for token in selected {
            let token = token.as_str().ok_or_else(|| invalid(id))?;
            if !seen.insert(token) {
                return Err(invalid(id));
            }
            let fragment = allowed
                .as_ref()
                .and_then(|g| g.get(token))
                .ok_or_else(|| invalid(id))?;
            for (key, values) in fragment.as_object().ok_or_else(|| invalid(id))? {
                fs.entry(key.clone())
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or_else(|| invalid(id))?
                    .extend(
                        values
                            .as_array()
                            .ok_or_else(|| invalid(id))?
                            .iter()
                            .cloned(),
                    );
            }
        }
        if decision == "accept_once" && selected.is_empty() {
            return Err(invalid(id));
        }
        return Ok(if fs.is_empty() {
            json!({"permissions":{},"scope":"turn"})
        } else {
            json!({"permissions":{"fileSystem":fs},"scope":"turn"})
        });
    }
    if object.len() != 2 {
        return Err(invalid(id));
    }
    let answers = response
        .get("answers")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid(id))?;
    if decision != "accept_once" {
        if !answers.is_empty() {
            return Err(invalid(id));
        }
        return Ok(json!({"action":decision,"content":null}));
    }
    let (payload, supported) = mcp(params);
    if !supported || answers.len() != payload.questions.len() {
        return Err(invalid(id));
    }
    let mut content = serde_json::Map::new();
    for question in payload.questions {
        let answer = answers
            .get(&question.id)
            .and_then(Value::as_str)
            .filter(|v| v.len() <= 65536)
            .ok_or_else(|| invalid(id))?;
        let field = &params["requestedSchema"]["properties"][&question.id];
        let value = if field["type"] == "boolean" {
            match answer {
                "true" => json!(true),
                "false" => json!(false),
                _ => return Err(invalid(id)),
            }
        } else {
            let length = answer.chars().count() as u64;
            if field["minLength"].as_u64().is_some_and(|n| length < n)
                || field["maxLength"].as_u64().is_some_and(|n| length > n)
                || question
                    .options
                    .as_ref()
                    .is_some_and(|options| !options.iter().any(|o| o.label == answer))
            {
                return Err(invalid(id));
            }
            json!(answer)
        };
        content.insert(question.id, value);
    }
    Ok(json!({"action":"accept","content":content}))
}
