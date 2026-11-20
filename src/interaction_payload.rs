//! Checked durable payloads shared by interaction writers and observers.

use crate::domain::MAX_JCS_SAFE_INTEGER;
use crate::interaction::{Interaction, UserInput};
use crate::machine::MachineError;
use crate::workspace::LosslessPath;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
use std::path::Path;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CommandApprovalPayload {
    pub title: String,
    pub message: String,
    pub command: Vec<String>,
    pub cwd: LosslessPath,
    pub reason: Option<String>,
}

impl CommandApprovalPayload {
    pub(crate) fn from_native(params: &serde_json::Value, workspace: &Path) -> Option<Self> {
        use serde_json::Value;
        if params.get("kind").is_some_and(|kind| kind != "command")
            || params
                .get("networkApprovalContext")
                .is_some_and(|context| !context.is_null())
        {
            return None;
        }
        let command: Vec<String> = match params.get("command")? {
            Value::String(command) => vec![command.clone()],
            value @ Value::Array(_) => serde_json::from_value(value.clone()).ok()?,
            _ => return None,
        };
        if command.first().is_none_or(|first| first.trim().is_empty()) {
            return None;
        }
        let cwd = match params.get("cwd") {
            None | Some(Value::Null) => LosslessPath::from_path(workspace),
            Some(cwd) => serde_json::from_value(cwd.clone()).ok()?,
        };
        let reason =
            serde_json::from_value(params.get("reason").cloned().unwrap_or(Value::Null)).ok()?;
        let payload = Self {
            title: params
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Command approval")
                .to_owned(),
            message: params
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Command execution requires approval")
                .to_owned(),
            command,
            cwd,
            reason,
        };
        payload.validate().ok()?;
        Some(payload)
    }

    fn validate(&self) -> Result<(), MachineError> {
        bounded(&self.title, 512)?;
        bounded(&self.message, 4096)?;
        if let Some(reason) = &self.reason {
            bounded(reason, 4096)?;
        }
        if self.command.len() > 256 {
            return Err(invalid("command argument count"));
        }
        for argument in &self.command {
            bounded(argument, 4096)?;
        }
        path(&self.cwd)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileApprovalPayload {
    pub title: String,
    pub message: String,
    pub reason: Option<String>,
    pub snapshot_sha256: String,
    pub snapshot_revision: u64,
    pub truncated: bool,
    pub changes: Option<Vec<FileChangePayload>>,
    pub change_artifact: Option<ChangeArtifactPayload>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileChangePayload {
    pub path: LosslessPath,
    pub kind: String,
    pub diff: String,
    pub move_path: Option<LosslessPath>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangeArtifactPayload {
    pub artifact_id: Uuid,
    pub sha256: String,
    pub media_type: String,
    pub byte_length: u64,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UnsupportedPayload {
    pub(crate) method: String,
    pub(crate) reason: String,
}

fn invalid(invariant: &str) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "durable observation violates its checked shape",
        false,
        json!({"invariant":invariant}),
    )
}

fn bounded(value: &str, maximum: usize) -> Result<(), MachineError> {
    if value.len() > maximum {
        return Err(invalid("interaction UTF-8 byte bound"));
    }
    Ok(())
}

fn digest(value: &str) -> Result<(), MachineError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(invalid("artifact or snapshot SHA-256"));
    }
    Ok(())
}

fn path(value: &LosslessPath) -> Result<(), MachineError> {
    match value {
        LosslessPath::Utf8(value) if value.is_empty() || value.len() > 4096 => {
            Err(invalid("interaction path bound"))
        }
        LosslessPath::Bytes { bytes } if bytes.len() > 8192 => {
            Err(invalid("interaction path byte bound"))
        }
        _ => Ok(()),
    }
}

pub(crate) fn validate(interaction: &Interaction) -> Result<(), MachineError> {
    interaction.require_bounded_payload()?;
    let (schema, decisions) = match interaction.kind.as_str() {
        "command_execution_approval" => (
            "dolgorae.interaction.command-approval/v1",
            &["accept_once", "decline", "cancel"][..],
        ),
        "file_change_approval" => (
            "dolgorae.interaction.file-change-approval/v1",
            &["accept_once", "decline", "cancel"][..],
        ),
        "permission_request" => (
            "dolgorae.interaction.permission/v2",
            &["accept_once", "decline", "cancel"][..],
        ),
        "mcp_elicitation" => {
            let value: crate::successor_interaction::McpPayload =
                serde_json::from_value(interaction.payload.clone())
                    .map_err(|_| invalid("MCP payload"))?;
            (
                "dolgorae.interaction.mcp-elicitation/v2",
                if value.policy_only.is_some() {
                    &["decline", "cancel"][..]
                } else {
                    &["accept_once", "decline", "cancel"][..]
                },
            )
        }
        "user_input" => ("dolgorae.interaction.user-input/v1", &[][..]),
        "unsupported_request" => ("dolgorae.interaction.unsupported/v1", &[][..]),
        _ => return Err(invalid("closed normalized interaction kind")),
    };
    if interaction.response_schema != schema
        || interaction.available_decisions.len() != decisions.len()
        || !interaction
            .available_decisions
            .iter()
            .zip(decisions)
            .all(|(actual, expected)| actual == *expected)
    {
        return Err(invalid("interaction response schema and exact decisions"));
    }
    match interaction.kind.as_str() {
        "permission_request" => {
            let value: crate::successor_interaction::PermissionPayload =
                serde_json::from_value(interaction.payload.clone())
                    .map_err(|_| invalid("permission payload"))?;
            bounded(&value.title, 512)?;
            bounded(&value.reason, 4096)?;
            if value.requested_permissions.len() > 64 {
                return Err(invalid("permission count"));
            }
            let mut seen = BTreeSet::new();
            for token in value.requested_permissions {
                bounded(&token, 4352)?;
                if !seen.insert(token) {
                    return Err(invalid("unique permission identifiers"));
                }
            }
        }
        "mcp_elicitation" => {
            let value: crate::successor_interaction::McpPayload =
                serde_json::from_value(interaction.payload.clone())
                    .map_err(|_| invalid("MCP payload"))?;
            bounded(&value.title, 4096)?;
            bounded(&value.server_name, 512)?;
            if value.policy_only.is_some()
                && (value.title != "MCP input"
                    || !value.server_name.is_empty()
                    || !value.questions.is_empty())
            {
                return Err(invalid("closed policy-only MCP payload"));
            }
            if value.questions.len() > 64
                || (value.policy_only.is_none()
                    && interaction.status == "pending"
                    && value.questions.is_empty())
            {
                return Err(invalid("MCP question count"));
            }
            let mut ids = BTreeSet::new();
            for q in value.questions {
                bounded(&q.id, 128)?;
                bounded(&q.header, 512)?;
                bounded(&q.question, 4096)?;
                if q.id.is_empty() || !q.is_secret || !ids.insert(q.id) {
                    return Err(invalid("protected MCP question"));
                }
                if let Some(options) = q.options {
                    if options.len() > 32 {
                        return Err(invalid("MCP option count"));
                    }
                    for option in options {
                        bounded(&option.label, 256)?;
                        bounded(&option.description, 4096)?;
                    }
                }
            }
        }
        "command_execution_approval" => {
            let value: CommandApprovalPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("command approval payload"))?;
            value.validate()?;
        }
        "file_change_approval" => {
            let value: FileApprovalPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("file approval payload"))?;
            bounded(&value.title, 512)?;
            bounded(&value.message, 4096)?;
            if let Some(reason) = &value.reason {
                bounded(reason, 4096)?;
            }
            digest(&value.snapshot_sha256)?;
            if value.truncated || value.snapshot_revision > MAX_JCS_SAFE_INTEGER {
                return Err(invalid("file snapshot completeness"));
            }
            match (&value.changes, &value.change_artifact) {
                (Some(changes), None) if !changes.is_empty() && changes.len() <= 4096 => {
                    if changes
                        .iter()
                        .map(|change| change.diff.len())
                        .sum::<usize>()
                        > 65536
                    {
                        return Err(invalid("inline diff byte bound"));
                    }
                    for change in changes {
                        if !matches!(change.kind.as_str(), "add" | "update" | "delete") {
                            return Err(invalid("file change kind"));
                        }
                        if change.kind != "update" && change.move_path.is_some() {
                            return Err(invalid("file move requires update"));
                        }
                        path(&change.path)?;
                        if let Some(move_path) = &change.move_path {
                            path(move_path)?;
                        }
                    }
                }
                (None, Some(artifact)) => {
                    digest(&artifact.sha256)?;
                    if artifact.artifact_id.get_version_num() != 7
                        || artifact.truncated
                        || artifact.media_type != "text/x-diff"
                        || !(65537..=8 * 1024 * 1024).contains(&artifact.byte_length)
                    {
                        return Err(invalid("bounded immutable file-change artifact"));
                    }
                }
                _ => return Err(invalid("exactly one complete file-change representation")),
            }
        }
        "user_input" => {
            let value: UserInput = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("user input payload"))?;
            if value.questions.is_empty() || value.questions.len() > 3 {
                return Err(invalid("user input question count"));
            }
            let mut ids = BTreeSet::new();
            for question in value.questions {
                if question.id.is_empty() || !ids.insert(question.id.clone()) {
                    return Err(invalid("unique question identity"));
                }
                bounded(&question.id, 256)?;
                bounded(&question.header, 512)?;
                bounded(&question.question, 4096)?;
                if let Some(options) = question.options {
                    if options.len() > 32 {
                        return Err(invalid("question option count"));
                    }
                    for option in options {
                        bounded(&option.label, 512)?;
                        bounded(&option.description, 4096)?;
                    }
                }
            }
        }
        "unsupported_request" => {
            let value: UnsupportedPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("unsupported interaction payload"))?;
            if value.reason != "recognized_unsupported" || interaction.status != "resolved" {
                return Err(invalid("unsupported interaction resolution"));
            }
            if !matches!(
                value.method.as_str(),
                "item/permissions/requestApproval" | "mcpServer/elicitation/request"
            ) {
                return Err(invalid("recognized unsupported method"));
            }
        }
        _ => return Err(invalid("closed normalized interaction kind")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_only_mcp_is_closed_and_does_not_relax_ordinary_pending_forms() {
        let raw: serde_json::Value = serde_json::from_str(include_str!(
            "../docs/protocol/examples/interaction-v2/interaction-v2-mcp-policy-only.valid.json"
        ))
        .unwrap();
        let run_id = serde_json::from_value(raw["run_id"].clone()).unwrap();
        let interaction = Interaction::parse(raw.clone(), run_id).unwrap();
        validate(&interaction).unwrap();
        for field in [
            "policy_only",
            "null_marker",
            "object_marker",
            "title",
            "server_name",
            "questions",
            "decisions",
        ] {
            let mut changed = Interaction::parse(raw.clone(), run_id).unwrap();
            match field {
                "policy_only" => changed.payload[field] = json!("UNKNOWN"),
                "object_marker" => {
                    changed.payload["policy_only"] = json!({"RUNTIME_VARIANT_UNSUPPORTED":null})
                }
                "null_marker" => {
                    let supported: serde_json::Value = serde_json::from_str(include_str!(
                        "../docs/protocol/examples/interaction-v2/interaction-v2-mcp.valid.json"
                    ))
                    .unwrap();
                    changed.payload = supported["payload"].clone();
                    changed.payload["policy_only"] = serde_json::Value::Null;
                    changed.available_decisions.insert(0, "accept_once".into());
                }
                "title" | "server_name" => changed.payload[field] = json!("secret-canary"),
                "questions" => changed.payload[field] = json!([{}]),
                "decisions" => changed.available_decisions.insert(0, "accept_once".into()),
                _ => unreachable!(),
            }
            assert!(validate(&changed).is_err(), "{field}");
        }
        let mut ordinary = interaction;
        ordinary
            .payload
            .as_object_mut()
            .unwrap()
            .remove("policy_only");
        ordinary.available_decisions.insert(0, "accept_once".into());
        assert!(validate(&ordinary).is_err());
    }
}
