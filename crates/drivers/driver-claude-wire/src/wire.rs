//! Claude Code `stream-json` NDJSON wire encoding and decoding helpers.

use std::collections::HashMap;

use dolgorae_spi::{
    DecisionKind, DecisionOption, FileChangeEntry, InteractionKind, InteractionRequest,
    NormalizedEvent, ToolActivityStatus, TurnInput, TurnTerminalStatus, UsageMetrics,
};
use serde_json::{Value, json};

pub(crate) const ALLOW_ONCE: &str = "allow_once";
pub(crate) const DENY_ONCE: &str = "deny_once";
pub(crate) const DENY_SESSION: &str = "deny_session";
pub(crate) const CANCEL: &str = "cancel";

pub(crate) fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn bool_field(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool) == Some(true)
}

pub(crate) fn encode_control_request(request_id: &str, subtype: &str) -> Value {
    json!({"type": "control_request", "request_id": request_id, "request": {"subtype": subtype}})
}

pub(crate) fn encode_user_turn(session_id: &str, input: &TurnInput) -> Value {
    let mut content = input.text.clone();
    if !input.attachments.is_empty() {
        content.push_str("\n\nAttached files:");
        for att in &input.attachments {
            content.push_str(&format!("\n- {}", att.path.display()));
            if let Some(mt) = &att.media_type {
                content.push_str(&format!(" ({mt})"));
            }
        }
    }
    if let Some(schema) = &input.output_schema_override {
        content.push_str(&format!("\n\nRespond matching JSON schema:\n{schema}"));
    }
    json!({"type": "user", "message": {"role": "user", "content": content}, "parent_tool_use_id": null, "session_id": session_id})
}

/// Builds an allow `control_response`; `updatedInput` is mandatory on Claude Code.
pub(crate) fn encode_allow_response(native_id: &str, original_input: &Value) -> Value {
    let updated = if original_input.is_null() {
        json!({})
    } else {
        original_input.clone()
    };
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": native_id, "response": {"behavior": "allow", "updatedInput": updated}}})
}

pub(crate) fn encode_deny_response(native_id: &str, reason: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "success", "request_id": native_id, "response": {"behavior": "deny", "message": reason, "interrupt": false}}})
}

pub(crate) fn encode_error_control_response(native_id: &str, error: &str) -> Value {
    json!({"type": "control_response", "response": {"subtype": "error", "request_id": native_id, "error": error}})
}

pub(crate) fn build_interaction_request(
    interaction_id: String,
    turn_id: String,
    tool_name: &str,
    input: &Value,
    decision_reason: Option<String>,
) -> InteractionRequest {
    let reason = decision_reason.or_else(|| str_field(input, "description").map(str::to_owned));
    let (kind, title) = match tool_name {
        "Bash" => {
            let cmd = str_field(input, "command").map(str::to_owned);
            let title = cmd.as_deref().map_or("Run Bash command".to_owned(), |c| {
                format!("Run command: {c}")
            });
            let cwd = str_field(input, "cwd").map(str::to_owned);
            let command = cmd.into_iter().collect();
            let k = InteractionKind::CommandApproval {
                command,
                cwd,
                reason,
            };
            (k, title)
        }
        "Edit" | "Write" | "NotebookEdit" | "MultiEdit" => {
            let paths = extract_file_paths(input);
            let kind_str = tool_name.to_ascii_lowercase();
            let changes = paths
                .iter()
                .map(|p| FileChangeEntry {
                    path: p.clone(),
                    move_path: None,
                    kind: kind_str.clone(),
                    unified_diff: None,
                })
                .collect();
            let title = paths
                .first()
                .map_or(format!("{tool_name} file change"), |p| {
                    format!("{tool_name}: {p}")
                });
            (
                InteractionKind::FileChangeApproval { changes, reason },
                title,
            )
        }
        _ => (
            InteractionKind::PermissionRequest {
                permissions: vec![tool_name.to_owned()],
                reason,
            },
            format!("Allow tool {tool_name}"),
        ),
    };

    let opts = [
        (ALLOW_ONCE, "Allow once", DecisionKind::AllowOnce),
        (DENY_ONCE, "Deny", DecisionKind::DenyOnce),
    ];
    let decisions = opts
        .into_iter()
        .map(|(id, lbl, kind)| DecisionOption {
            decision_id: id.to_owned(),
            label: lbl.to_owned(),
            kind,
        })
        .collect();

    InteractionRequest {
        interaction_id,
        turn_id,
        kind,
        original_kind: format!("can_use_tool:{tool_name}"),
        title,
        decisions,
        protected: false,
        response_schema: None,
        max_response_bytes: None,
    }
}

pub(crate) fn extract_file_paths(input: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let mut add = |p: &str| {
        if !p.is_empty() && !paths.iter().any(|e| e == p) {
            paths.push(p.to_owned());
        }
    };
    for key in ["file_path", "notebook_path", "path"] {
        if let Some(p) = str_field(input, key) {
            add(p);
        }
    }
    if let Some(edits) = input.get("edits").and_then(Value::as_array) {
        for edit in edits {
            if let Some(p) = str_field(edit, "file_path") {
                add(p);
            }
        }
    }
    paths
}

pub(crate) fn parse_stream_text_delta(frame: &Value) -> Option<(Option<String>, String)> {
    let ev = frame.get("event")?;
    if str_field(ev, "type") != Some("content_block_delta") {
        return None;
    }
    let delta = ev.get("delta")?;
    if str_field(delta, "type") != Some("text_delta") {
        return None;
    }
    let text = str_field(delta, "text")?;
    let idx_id = ev
        .get("index")
        .and_then(Value::as_u64)
        .map(|i| format!("block-{i}"));
    let item_id = idx_id.or_else(|| str_field(frame, "uuid").map(str::to_owned));
    Some((item_id, text.to_owned()))
}

pub(crate) fn parse_assistant_blocks(
    frame: &Value,
    turn_id: &str,
    tool_names: &mut HashMap<String, String>,
) -> (String, Vec<NormalizedEvent>) {
    let mut text_out = String::new();
    let mut events = Vec::new();
    let blocks = frame
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = blocks else {
        return (text_out, events);
    };

    for block in blocks {
        match str_field(block, "type") {
            Some("text") => {
                if let Some(t) = str_field(block, "text") {
                    text_out.push_str(t);
                }
            }
            Some("tool_use") => {
                let item_id = str_field(block, "id").unwrap_or("tool-unknown").to_owned();
                let tool_name = str_field(block, "name").unwrap_or("unknown").to_owned();
                let input = block.get("input").unwrap_or(&Value::Null);
                let cmd = if tool_name == "Bash" {
                    str_field(input, "command").map(str::to_owned)
                } else {
                    None
                };
                let paths = extract_file_paths(input);
                let summary = str_field(input, "description")
                    .map(str::to_owned)
                    .or_else(|| cmd.clone())
                    .or_else(|| paths.first().cloned());
                tool_names.insert(item_id.clone(), tool_name.clone());
                events.push(NormalizedEvent::ToolActivity {
                    turn_id: turn_id.to_owned(),
                    item_id,
                    tool_name,
                    status: ToolActivityStatus::Started,
                    summary,
                    command: cmd,
                    exit_status: None,
                    paths,
                });
            }
            _ => {}
        }
    }
    (text_out, events)
}

pub(crate) fn parse_user_tool_results(
    frame: &Value,
    turn_id: &str,
    tool_names: &HashMap<String, String>,
) -> Vec<NormalizedEvent> {
    let blocks = frame
        .get("message")
        .and_then(|m| m.get("content"))
        .and_then(Value::as_array);
    let Some(blocks) = blocks else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for block in blocks {
        if str_field(block, "type") != Some("tool_result") {
            continue;
        }
        let item_id = str_field(block, "tool_use_id")
            .unwrap_or("tool-unknown")
            .to_owned();
        let tool_name = tool_names
            .get(&item_id)
            .cloned()
            .unwrap_or_else(|| "tool".to_owned());
        let status = if bool_field(block, "is_error") {
            ToolActivityStatus::Failed
        } else {
            ToolActivityStatus::Completed
        };
        out.push(NormalizedEvent::ToolActivity {
            turn_id: turn_id.to_owned(),
            item_id,
            tool_name,
            status,
            summary: extract_summary(block.get("content")),
            command: None,
            exit_status: None,
            paths: Vec::new(),
        });
    }
    out
}

fn extract_summary(content: Option<&Value>) -> Option<String> {
    let raw = match content? {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Array(items) => {
            let texts: Vec<&str> = items.iter().filter_map(|i| str_field(i, "text")).collect();
            if texts.is_empty() {
                return None;
            }
            texts.join("\n")
        }
        _ => return None,
    };
    let mut chars = raw.chars();
    let snippet: String = chars.by_ref().take(256).collect();
    Some(if chars.next().is_some() {
        format!("{snippet}...")
    } else {
        snippet
    })
}

pub(crate) fn parse_result_events(
    frame: &Value,
    turn_id: &str,
    fallback: &str,
    was_interrupted: bool,
) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    let subtype = str_field(frame, "subtype").unwrap_or("");
    let is_error = bool_field(frame, "is_error");
    let raw_text = str_field(frame, "result").unwrap_or_default();
    let text = if raw_text.is_empty() {
        fallback.to_owned()
    } else {
        raw_text.to_owned()
    };
    let structured = frame
        .get("structured_output")
        .filter(|v| !v.is_null())
        .cloned();

    if !is_error && (!text.is_empty() || structured.is_some()) {
        events.push(NormalizedEvent::AssistantMessage {
            turn_id: turn_id.to_owned(),
            item_id: None,
            text,
            structured_output: structured,
        });
    }
    if let Some(metrics) = parse_usage(frame) {
        events.push(NormalizedEvent::Usage {
            turn_id: Some(turn_id.to_owned()),
            metrics,
        });
    }

    let (status, error) = if subtype == "success" && !is_error {
        (TurnTerminalStatus::Completed, None)
    } else if matches!(subtype, "interrupted" | "error_interrupted" | "cancelled")
        || (was_interrupted && (is_error || subtype.starts_with("error")))
    {
        (TurnTerminalStatus::Interrupted, extract_error(frame))
    } else {
        let fallback_err = if subtype.is_empty() {
            "turn failed".to_owned()
        } else {
            format!("turn ended with subtype '{subtype}'")
        };
        let msg = extract_error(frame).or(Some(fallback_err));
        (TurnTerminalStatus::Failed, msg)
    };
    events.push(NormalizedEvent::TurnCompleted {
        turn_id: turn_id.to_owned(),
        status,
        error,
    });
    events
}

fn parse_usage(frame: &Value) -> Option<UsageMetrics> {
    let u = frame.get("usage");
    let get = |k: &str| u.and_then(|o| o.get(k)).and_then(Value::as_u64);
    let m = UsageMetrics {
        input_tokens: get("input_tokens"),
        output_tokens: get("output_tokens"),
        cached_input_tokens: get("cache_read_input_tokens").or_else(|| get("cached_input_tokens")),
        reasoning_tokens: get("reasoning_tokens"),
        total_cost_usd: frame.get("total_cost_usd").and_then(Value::as_f64),
    };
    let any = m.input_tokens.is_some()
        || m.output_tokens.is_some()
        || m.cached_input_tokens.is_some()
        || m.reasoning_tokens.is_some()
        || m.total_cost_usd.is_some();
    any.then_some(m)
}

fn extract_error(frame: &Value) -> Option<String> {
    if let Some(err) = str_field(frame, "error").filter(|s| !s.is_empty()) {
        return Some(err.to_owned());
    }
    if let Some(arr) = frame.get("errors").and_then(Value::as_array) {
        let parts: Vec<&str> = arr.iter().filter_map(Value::as_str).collect();
        if !parts.is_empty() {
            return Some(parts.join("; "));
        }
    }
    if bool_field(frame, "is_error") {
        return str_field(frame, "result")
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
    }
    None
}
