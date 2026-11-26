//! ACP v1 JSON-RPC 2.0 wire mapping and normalization helpers.

use std::collections::HashMap;
use std::path::Path;

use dolgorae_spi::{
    DecisionKind, DecisionOption, FileChangeEntry, InteractionKind, McpServerInjection,
    NormalizedEvent, ToolActivityStatus, TurnInput, TurnTerminalStatus, UsageMetrics,
};
use serde_json::{Value, json};

const CWD_KEYS: &[&str] = &["cwd", "workingDirectory", "working_directory", "workdir"];
const MOVE_KEYS: &[&str] = &[
    "move_path",
    "movePath",
    "new_path",
    "newPath",
    "destination",
    "to",
];
const PATH_KEYS: &[&str] = &["path", "file_path", "filePath", "target_path", "targetPath"];

#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct AgentSessionCaps {
    pub(crate) supports_resume: bool,
    pub(crate) supports_close: bool,
}

#[derive(Debug)]
pub(crate) enum IncomingMessage {
    Response {
        id: Value,
        result: Option<Value>,
        error: Option<Value>,
    },
    ServerRequest {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
}

pub(crate) struct ParsedPermission {
    pub(crate) kind: InteractionKind,
    pub(crate) original_kind: String,
    pub(crate) title: String,
    pub(crate) decisions: Vec<DecisionOption>,
    pub(crate) option_by_decision: HashMap<String, String>,
    pub(crate) allow_once_decision: Option<(String, String)>,
    pub(crate) deny_decision: Option<(String, String)>,
    pub(crate) is_mode_change: bool,
}

pub(crate) fn parse_incoming(line: &str) -> Result<IncomingMessage, String> {
    let val: Value = serde_json::from_str(line).map_err(|e| format!("invalid json: {e}"))?;
    let obj = val
        .as_object()
        .ok_or("json-rpc message must be an object")?;
    let id = obj.get("id").filter(|v| !v.is_null()).cloned();
    let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);
    let params = obj.get("params").cloned().unwrap_or(Value::Null);
    match (id, method) {
        (Some(id), Some(method)) => Ok(IncomingMessage::ServerRequest { id, method, params }),
        (Some(id), None) => Ok(IncomingMessage::Response {
            id,
            result: obj.get("result").cloned(),
            error: obj.get("error").cloned(),
        }),
        (None, Some(method)) => Ok(IncomingMessage::Notification { method, params }),
        (None, None) => Err("json-rpc message missing both id and method".to_owned()),
    }
}

pub(crate) fn rpc_id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{s}"),
        Value::Number(n) => format!("n:{n}"),
        other => format!("o:{other}"),
    }
}

pub(crate) fn parse_session_caps(init: &Value) -> AgentSessionCaps {
    let sc = init
        .get("agentCapabilities")
        .and_then(|c| c.get("sessionCapabilities"));
    let has = |k| {
        sc.and_then(|o| o.get(k))
            .is_some_and(|v| !v.is_null() && *v != Value::Bool(false))
    };
    AgentSessionCaps {
        supports_resume: has("resume"),
        supports_close: has("close"),
    }
}

/// Selects the safest advertised read-only/ask mode ID from `session/new` or
/// `session/resume`, returning `None` if none is advertised. Never selects
/// `plan`, `auto`, or `accept-edits`.
pub(crate) fn select_readonly_mode(res: &Value) -> Option<String> {
    let modes = res.get("modes");
    let entries = modes
        .and_then(|m| m.get("availableModes").or(Some(m)))
        .or_else(|| res.get("availableModes"))
        .and_then(Value::as_array)?;
    let advertised: Vec<(&str, Option<&str>)> = entries
        .iter()
        .filter_map(|e| {
            let id = e.as_str().or_else(|| first_str(e, &["id", "modeId"]))?;
            Some((id, first_str(e, &["description"])))
        })
        .collect();
    ["read-only", "read_only", "ask", "default"]
        .iter()
        .find_map(|pref| {
            advertised
                .iter()
                .find(|(id, desc)| {
                    id.eq_ignore_ascii_case(pref)
                        && (*pref != "default" || default_desc_asks(*desc))
                })
                .map(|(id, _)| (*id).to_owned())
        })
}

fn default_desc_asks(desc: Option<&str>) -> bool {
    desc.is_none_or(|d| {
        let low = d.to_ascii_lowercase();
        low.contains("ask") || low.contains("read-only") || low.contains("read only")
    })
}

/// Serializes MCP servers with ACP's required `env: [{"name", "value"}]` array.
pub(crate) fn build_mcp_servers(servers: &[McpServerInjection]) -> Value {
    Value::Array(
        servers
            .iter()
            .map(|s| {
                let env: Vec<Value> = s.env.iter().map(|(k, v)| json!({"name": k, "value": v})).collect();
                json!({"name": s.name, "command": s.command.to_string_lossy(), "args": s.args, "env": env})
            })
            .collect(),
    )
}

pub(crate) fn build_session_meta(role: Option<&str>, schema: Option<&Value>) -> Option<Value> {
    let mut parts = Vec::new();
    if let Some(r) = role.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(r.to_owned());
    }
    if let Some(s) = schema {
        parts.push(format_schema_guidance(s));
    }
    (!parts.is_empty())
        .then(|| json!({"antigravity": {"systemPrompt": {"append": parts.join("\n\n")}}}))
}

pub(crate) fn build_turn_prompt_text(input: &TurnInput) -> String {
    let mut parts = Vec::new();
    if let Some(s) = input.output_schema_override.as_ref() {
        parts.push(format_schema_guidance(s));
    }
    parts.push(input.text.clone());
    if !input.attachments.is_empty() {
        let mut lines = vec!["Attached files:".to_owned()];
        for att in &input.attachments {
            let suffix = att
                .media_type
                .as_deref()
                .map_or(String::new(), |mt| format!(" ({mt})"));
            lines.push(format!("- {}{suffix}", att.path.display()));
        }
        parts.push(lines.join("\n"));
    }
    parts.join("\n\n")
}

fn format_schema_guidance(schema: &Value) -> String {
    let rendered = serde_json::to_string_pretty(schema).unwrap_or_else(|_| schema.to_string());
    format!(
        "Output format requirement: Respond with valid JSON strictly conforming to the following JSON Schema (no markdown fences or extra prose):\n{rendered}"
    )
}

pub(crate) fn resolve_abs_cwd(workspace: &Path) -> String {
    let path = if workspace.is_absolute() {
        workspace.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| workspace.to_path_buf(), |c| c.join(workspace))
    };
    path.to_string_lossy().into_owned()
}

pub(crate) fn map_session_update(
    params: &Value,
    active_turn_id: Option<&str>,
    turn_text_buf: Option<&mut String>,
) -> NormalizedEvent {
    let update = params.get("update").unwrap_or(params);
    let kind = first_str(update, &["sessionUpdate"]).unwrap_or("unknown_session_update");
    let turn_id = active_turn_id.unwrap_or_default().to_owned();

    match kind {
        "agent_message_chunk" => {
            let delta = extract_chunk_text(update);
            if let Some(buf) = turn_text_buf {
                buf.push_str(&delta);
            }
            NormalizedEvent::TextDelta {
                turn_id,
                item_id: first_str(update, &["messageId", "itemId"]).map(str::to_owned),
                delta,
            }
        }
        "tool_call" | "tool_call_update" => {
            let status = match first_str(update, &["status"]) {
                Some("pending" | "in_progress") | None if kind == "tool_call" => {
                    ToolActivityStatus::Started
                }
                Some("pending" | "in_progress") | None => ToolActivityStatus::Delta,
                Some("completed") => ToolActivityStatus::Completed,
                Some("failed" | "error" | "cancelled") => ToolActivityStatus::Failed,
                Some(_) => ToolActivityStatus::Unknown,
            };
            NormalizedEvent::ToolActivity {
                turn_id,
                item_id: first_str(update, &["toolCallId"])
                    .unwrap_or("tool")
                    .to_owned(),
                tool_name: first_str(update, &["title", "kind"])
                    .unwrap_or("tool")
                    .to_owned(),
                status,
                summary: first_str(update, &["title"]).map(str::to_owned),
                command: extract_command(update).map(|a| a.join(" ")),
                exit_status: extract_exit_status(update),
                paths: extract_paths(update),
            }
        }
        "usage_update" => NormalizedEvent::Usage {
            turn_id: active_turn_id.map(str::to_owned),
            metrics: extract_usage_metrics(update),
        },
        other => NormalizedEvent::Unknown {
            event_type: format!("acp.session_update.{other}"),
            payload: Some(update.clone()),
        },
    }
}

pub(crate) fn parse_permission_request(params: &Value) -> ParsedPermission {
    let tc = params.get("toolCall").unwrap_or(&Value::Null);
    let kind_lower = first_str(tc, &["kind"])
        .unwrap_or("other")
        .to_ascii_lowercase();
    let title = first_str(tc, &["title"])
        .or_else(|| first_str(params, &["title"]))
        .unwrap_or("Permission request")
        .to_owned();
    let reason = first_str(params, &["reason"])
        .or_else(|| first_str(tc, &["title"]))
        .map(str::to_owned);
    let is_mode_change = matches!(
        kind_lower.as_str(),
        "switch_mode" | "set_mode" | "change_mode" | "mode"
    ) || title.to_ascii_lowercase().contains("switch_mode")
        || first_str(tc, &["name"]).is_some_and(|n| n.eq_ignore_ascii_case("switch_mode"));

    let kind = match kind_lower.as_str() {
        "execute" => InteractionKind::CommandApproval {
            command: extract_command(tc).unwrap_or_else(|| vec![title.clone()]),
            cwd: tc
                .get("rawInput")
                .and_then(|r| first_str(r, CWD_KEYS))
                .map(str::to_owned),
            reason,
        },
        "edit" | "delete" | "move" => InteractionKind::FileChangeApproval {
            changes: extract_file_changes(tc, &kind_lower),
            reason,
        },
        _ => {
            let mut permissions = vec![format!("acp:{kind_lower}")];
            if let Some(t) = first_str(tc, &["title"]).filter(|s| !s.is_empty()) {
                permissions.push(t.to_owned());
            }
            InteractionKind::PermissionRequest {
                permissions,
                reason,
            }
        }
    };

    let (mut decisions, mut option_by_decision) = (Vec::new(), HashMap::new());
    let (mut allow_once, mut deny_once, mut deny_always) = (None, None, None);

    if let Some(opts) = params.get("options").and_then(Value::as_array) {
        for (idx, opt) in opts.iter().enumerate() {
            let Some(native_id) = first_str(opt, &["optionId"]) else {
                continue;
            };
            let acp_kind = first_str(opt, &["kind"]).unwrap_or("");
            let (dec_kind, base) = match acp_kind {
                "allow_once" => (DecisionKind::AllowOnce, "allow_once"),
                "allow_always" => (DecisionKind::AllowSession, "allow_session"),
                "reject_once" => (DecisionKind::DenyOnce, "deny_once"),
                "reject_always" => (DecisionKind::DenySession, "deny_session"),
                _ => (DecisionKind::Unknown, "option"),
            };
            let decision_id = if base != "option" && !option_by_decision.contains_key(base) {
                base.to_owned()
            } else {
                format!("{base}_{idx}")
            };
            option_by_decision.insert(decision_id.clone(), native_id.to_owned());
            let pair = (decision_id.clone(), native_id.to_owned());
            match acp_kind {
                "allow_once" => {
                    allow_once.get_or_insert(pair);
                }
                "reject_once" => {
                    deny_once.get_or_insert(pair);
                }
                "reject_always" => {
                    deny_always.get_or_insert(pair);
                }
                _ => {}
            }
            decisions.push(DecisionOption {
                decision_id,
                label: first_str(opt, &["name"]).unwrap_or("Option").to_owned(),
                kind: dec_kind,
            });
        }
    }

    ParsedPermission {
        kind,
        original_kind: format!("acp:permission:{kind_lower}"),
        title,
        decisions,
        option_by_decision,
        allow_once_decision: allow_once,
        deny_decision: deny_once.or(deny_always),
        is_mode_change,
    }
}

pub(crate) fn map_stop_reason(stop_reason: Option<&str>) -> TurnTerminalStatus {
    match stop_reason {
        Some("end_turn" | "max_tokens" | "max_turn_requests") | None => {
            TurnTerminalStatus::Completed
        }
        Some("cancelled") => TurnTerminalStatus::Interrupted,
        Some("error" | "failed" | "refusal") => TurnTerminalStatus::Failed,
        Some(_) => TurnTerminalStatus::Unknown,
    }
}

pub(crate) fn extract_usage_metrics(val: &Value) -> UsageMetrics {
    let src = val.get("usage").unwrap_or(val);
    let u64_of = |keys: &[&str]| {
        keys.iter()
            .find_map(|k| src.get(*k).or_else(|| val.get(*k)).and_then(Value::as_u64))
    };
    let cost = val
        .get("cost")
        .and_then(|c| c.get("amount"))
        .and_then(Value::as_f64)
        .or_else(|| {
            ["totalCostUsd", "total_cost_usd"]
                .iter()
                .find_map(|k| src.get(*k).or_else(|| val.get(*k)).and_then(Value::as_f64))
        });
    UsageMetrics {
        input_tokens: u64_of(&["inputTokens", "input_tokens"]),
        output_tokens: u64_of(&["outputTokens", "output_tokens"]),
        cached_input_tokens: u64_of(&[
            "cachedInputTokens",
            "cachedReadTokens",
            "cached_input_tokens",
        ]),
        reasoning_tokens: u64_of(&["reasoningTokens", "thoughtTokens", "reasoning_tokens"]),
        total_cost_usd: cost,
    }
}

pub(crate) fn perm_outcome(opt_id: Option<&str>) -> Value {
    match opt_id {
        Some(id) => json!({"outcome": {"outcome": "selected", "optionId": id}}),
        None => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

fn first_str<'a>(val: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| val.get(*k).and_then(Value::as_str))
}

fn extract_chunk_text(update: &Value) -> String {
    if let Some(c) = update.get("content") {
        if let Some(t) = c.get("text").and_then(Value::as_str).or_else(|| c.as_str()) {
            return t.to_owned();
        }
        if let Some(arr) = c.as_array() {
            let joined: String = arr
                .iter()
                .filter_map(|i| i.get("text").and_then(Value::as_str))
                .collect();
            if !joined.is_empty() {
                return joined;
            }
        }
    }
    first_str(update, &["text", "delta"])
        .unwrap_or_default()
        .to_owned()
}

fn extract_command(val: &Value) -> Option<Vec<String>> {
    if let Some(raw) = val.get("rawInput") {
        if let Some(arr) = raw.get("command").and_then(Value::as_array) {
            let argv: Vec<String> = arr
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if !argv.is_empty() {
                return Some(argv);
            }
        }
        if let Some(cmd) = first_str(raw, &["command", "cmd"]).filter(|s| !s.is_empty()) {
            return Some(vec![cmd.to_owned()]);
        }
    }
    first_str(val, &["command"])
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_owned()])
}

fn extract_exit_status(update: &Value) -> Option<i32> {
    let keys = ["exitCode", "exit_code", "exitStatus", "exit_status"];
    let find = |v: &Value| keys.iter().find_map(|k| v.get(*k).and_then(Value::as_i64));
    update
        .get("rawOutput")
        .and_then(find)
        .or_else(|| find(update))
        .and_then(|v| i32::try_from(v).ok())
}

fn extract_paths(val: &Value) -> Vec<String> {
    let mut paths = Vec::new();
    let mut push = |p: &str| {
        if !p.is_empty() && !paths.iter().any(|e| e == p) {
            paths.push(p.to_owned());
        }
    };
    if let Some(locs) = val.get("locations").and_then(Value::as_array) {
        locs.iter()
            .filter_map(|l| first_str(l, &["path"]))
            .for_each(&mut push);
    }
    if let Some(raw) = val.get("rawInput") {
        PATH_KEYS
            .iter()
            .filter_map(|k| raw.get(*k).and_then(Value::as_str))
            .for_each(&mut push);
        if let Some(arr) = raw.get("paths").and_then(Value::as_array) {
            arr.iter().filter_map(Value::as_str).for_each(&mut push);
        }
    }
    paths
}

fn extract_file_changes(tc: &Value, change_kind: &str) -> Vec<FileChangeEntry> {
    let raw = tc.get("rawInput");
    let move_path = raw.and_then(|r| first_str(r, MOVE_KEYS)).map(str::to_owned);
    let unified_diff = raw
        .and_then(|r| first_str(r, &["diff", "unified_diff"]))
        .map(str::to_owned);
    extract_paths(tc)
        .into_iter()
        .map(|path| FileChangeEntry {
            path,
            move_path: move_path.clone(),
            kind: change_kind.to_owned(),
            unified_diff: unified_diff.clone(),
        })
        .collect()
}
