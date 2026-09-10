//! Checked durable payloads shared by interaction writers and observers.

use crate::domain::MAX_JCS_SAFE_INTEGER;
use crate::interaction::{Interaction, UserInput};
use crate::machine::MachineError;
use crate::workspace::LosslessPath;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::BTreeSet;
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
        "command_execution_approval" => {
            let value: CommandApprovalPayload = serde_json::from_value(interaction.payload.clone())
                .map_err(|_| invalid("command approval payload"))?;
            bounded(&value.title, 512)?;
            bounded(&value.message, 4096)?;
            if let Some(reason) = &value.reason {
                bounded(reason, 4096)?;
            }
            if value.command.len() > 256 {
                return Err(invalid("command argument count"));
            }
            for argument in &value.command {
                bounded(argument, 4096)?;
            }
            path(&value.cwd)?;
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
