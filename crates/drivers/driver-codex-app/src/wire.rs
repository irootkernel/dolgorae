//! Codex `app-server --stdio` wire builders, tolerant parser, and event/request mapping.
//!
//! Codex omits `"jsonrpc": "2.0"` on stdout responses/notifications, so frames
//! are classified solely by `"id"` and `"method"`.

use std::collections::BTreeMap;

use dolgorae_spi::{
    AccessMode, ApprovalPolicy, DecisionKind, DecisionOption, FileChangeEntry, InteractionKind,
    InteractionRequest, NormalizedEvent, QuestionOption, SessionSpec, ToolActivityStatus,
    TurnInput, TurnTerminalStatus, UsageMetrics, UserQuestionItem,
};
use serde_json::{Map, Value, json};
use uuid::Uuid;

/// Canonical string key for a JSON-RPC request/response `id` (integer or string).
pub(crate) fn id_key(v: &Value) -> Option<String> {
    (v.is_i64() || v.is_u64() || v.is_string()).then(|| v.to_string())
}

#[derive(Debug)]
pub(crate) enum IncomingFrame {
    Response(String, Result<Value, Value>),
    ServerRequest(Value, String, String, Value),
    Notification(String, Value),
    Unrecognized(Value),
}

#[derive(Debug, Clone)]
pub(crate) enum NativePendingKind {
    CommandApproval,
    FileChangeApproval,
    Permissions(Value),
    UserInput,
    McpElicitation,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingInteraction {
    pub(crate) raw_id: Value,
    pub(crate) key: String,
    pub(crate) turn_id: String,
    pub(crate) native_kind: NativePendingKind,
    pub(crate) allowed: Vec<String>,
}

pub(crate) fn parse_incoming_line(line: &str) -> Result<IncomingFrame, serde_json::Error> {
    let val: Value = serde_json::from_str(line)?;
    let Some(obj) = val.as_object() else {
        return Ok(IncomingFrame::Unrecognized(val));
    };
    let method = obj.get("method").and_then(Value::as_str);
    let raw_id = obj.get("id").filter(|v| !v.is_null());
    let params = || obj.get("params").cloned().unwrap_or(Value::Null);
    let key_pair = raw_id.and_then(|v| id_key(v).map(|k| (v, k)));
    Ok(match (method, key_pair) {
        (Some(m), Some((rv, k))) => IncomingFrame::ServerRequest(rv.clone(), k, m.into(), params()),
        (Some(m), None) => IncomingFrame::Notification(m.into(), params()),
        (None, Some((_, id))) => {
            let res = match obj.get("error").filter(|v| !v.is_null()) {
                Some(err) => Err(err.clone()),
                None => Ok(obj.get("result").cloned().unwrap_or(Value::Null)),
            };
            IncomingFrame::Response(id, res)
        }
        (None, None) => IncomingFrame::Unrecognized(val),
    })
}

/// Strictest native approval policy mapping (never `"never"`).
pub(crate) fn approval_policy_wire_value(p: ApprovalPolicy) -> &'static str {
    match p {
        ApprovalPolicy::Interactive | ApprovalPolicy::AutoApproveScoped => "on-request",
        ApprovalPolicy::AutoDeny => "untrusted",
    }
}

fn put(m: &mut Map<String, Value>, k: &str, v: Option<Value>) {
    if let Some(val) = v {
        m.insert(k.into(), val);
    }
}

pub(crate) fn build_thread_params(thread_id: Option<&str>, spec: &SessionSpec) -> Value {
    let sb = if spec.access == AccessMode::ReadOnly {
        "read-only"
    } else {
        "workspace-write"
    };
    let mut m = Map::new();
    put(&mut m, "threadId", thread_id.map(Into::into));
    let cwd = spec.workspace.to_string_lossy().into_owned();
    m.insert("cwd".into(), cwd.into());
    m.insert("sandbox".into(), sb.into());
    m.insert(
        "approvalPolicy".into(),
        approval_policy_wire_value(spec.approval_policy).into(),
    );
    put(&mut m, "model", spec.model.as_deref().map(Into::into));
    put(
        &mut m,
        "developerInstructions",
        spec.role_instructions.as_deref().map(Into::into),
    );
    if !spec.mcp_servers.is_empty() {
        let mut mcp = Map::new();
        for srv in &spec.mcp_servers {
            let mut e = Map::new();
            let cmd = srv.command.to_string_lossy().into_owned();
            e.insert("command".into(), cmd.into());
            put(
                &mut e,
                "args",
                (!srv.args.is_empty()).then(|| json!(srv.args)),
            );
            put(&mut e, "env", (!srv.env.is_empty()).then(|| json!(srv.env)));
            mcp.insert(srv.name.clone(), Value::Object(e));
        }
        m.insert("config".into(), json!({ "mcp_servers": mcp }));
    }
    Value::Object(m)
}

pub(crate) fn build_turn_start_params(
    thread_id: &str,
    input: &TurnInput,
    effort: Option<&str>,
    schema: Option<&Value>,
) -> Value {
    let mut items = vec![json!({ "type": "text", "text": input.text })];
    for att in &input.attachments {
        let p = att.path.to_string_lossy().into_owned();
        if att
            .media_type
            .as_deref()
            .is_some_and(|mt| mt.starts_with("image/"))
        {
            items.push(json!({ "type": "localImage", "path": p }));
        } else {
            items.push(json!({ "type": "text", "text": format!("[attachment: {p}]") }));
        }
    }
    let mut m = Map::new();
    m.insert("threadId".into(), thread_id.into());
    m.insert("input".into(), Value::Array(items));
    put(&mut m, "effort", effort.map(Into::into));
    put(&mut m, "outputSchema", schema.cloned());
    Value::Object(m)
}

pub(crate) fn build_reply(
    p: &PendingInteraction,
    decision_id: &str,
    answers: &BTreeMap<String, Vec<String>>,
    payload: Option<&Value>,
) -> Value {
    let result = match &p.native_kind {
        NativePendingKind::CommandApproval | NativePendingKind::FileChangeApproval => {
            let d = match decision_id {
                "allow_once" => "accept",
                "allow_session" => "acceptForSession",
                "cancel" => "cancel",
                _ => "decline",
            };
            json!({ "decision": d })
        }
        NativePendingKind::Permissions(native) => match decision_id {
            "allow_once" => json!({ "permissions": native, "scope": "turn" }),
            "allow_session" => json!({ "permissions": native, "scope": "session" }),
            _ => json!({ "permissions": {}, "scope": "turn" }),
        },
        NativePendingKind::UserInput => {
            let mut mapped = Map::new();
            if matches!(decision_id, "submit" | "allow_once") {
                for (q_id, vals) in answers {
                    mapped.insert(q_id.clone(), json!({ "answers": vals }));
                }
            }
            json!({ "answers": Value::Object(mapped) })
        }
        NativePendingKind::McpElicitation => {
            let action = match decision_id {
                "allow_once" | "submit" => "accept",
                "cancel" => "cancel",
                _ => "decline",
            };
            let mut res = Map::new();
            res.insert("action".into(), action.into());
            put(
                &mut res,
                "content",
                (action == "accept").then_some(payload).flatten().cloned(),
            );
            Value::Object(res)
        }
    };
    json!({ "id": p.raw_id, "result": result })
}

pub(crate) fn s<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    let o = v.as_object()?;
    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_str))
}

fn s_own(v: &Value, keys: &[&str]) -> Option<String> {
    s(v, keys).map(str::to_owned)
}

fn b(v: &Value, keys: &[&str]) -> bool {
    v.as_object()
        .and_then(|o| keys.iter().find_map(|k| o.get(*k).and_then(Value::as_bool)))
        .unwrap_or(false)
}

fn u(v: &Value, keys: &[&str]) -> Option<u64> {
    let o = v.as_object()?;
    keys.iter().find_map(|k| o.get(*k).and_then(Value::as_u64))
}

fn opts(items: &[(&str, &str, DecisionKind)]) -> Vec<DecisionOption> {
    items
        .iter()
        .map(|&(id, label, kind)| DecisionOption {
            decision_id: id.into(),
            label: label.into(),
            kind,
        })
        .collect()
}

fn extract_argv(params: &Value) -> Vec<String> {
    for k in ["command", "argv"] {
        if let Some(arr) = params.get(k).and_then(Value::as_array) {
            let out: Vec<String> = arr
                .iter()
                .filter_map(Value::as_str)
                .map(Into::into)
                .collect();
            if !out.is_empty() {
                return out;
            }
        }
    }
    params
        .get("command")
        .and_then(Value::as_str)
        .map(|c| vec![c.into()])
        .unwrap_or_default()
}

fn file_entry(path: String, item: &Value) -> FileChangeEntry {
    let kv = item.get("kind");
    let (kind, nested_move) = match kv {
        Some(obj) if obj.is_object() => (
            s(obj, &["type", "kind"]).unwrap_or("modify"),
            s_own(obj, &["move_path", "movePath"]),
        ),
        _ => (s(item, &["kind", "type"]).unwrap_or("modify"), None),
    };
    FileChangeEntry {
        path,
        move_path: nested_move.or_else(|| s_own(item, &["movePath", "move_path"])),
        kind: kind.into(),
        unified_diff: s_own(item, &["unifiedDiff", "unified_diff", "diff"]),
    }
}

pub(crate) fn extract_changes(params: &Value) -> Vec<FileChangeEntry> {
    match params.get("changes") {
        Some(Value::Array(list)) => list
            .iter()
            .filter_map(|i| {
                s(i, &["path", "filePath", "file_path"])
                    .filter(|p| !p.is_empty())
                    .map(|p| file_entry(p.into(), i))
            })
            .collect(),
        Some(Value::Object(map)) => map.iter().map(|(p, i)| file_entry(p.clone(), i)).collect(),
        _ => s(params, &["path", "filePath", "file_path"])
            .map(|p| vec![file_entry(p.into(), params)])
            .unwrap_or_default(),
    }
}

/// Returns true if a Codex `permissions` payload requests any filesystem write access.
pub(crate) fn requests_filesystem_write(perms: &Value) -> bool {
    let Some(fs) = perms.get("fileSystem").or_else(|| perms.get("file_system")) else {
        return false;
    };
    let w = fs.get("write");
    if w.and_then(Value::as_array).is_some_and(|a| !a.is_empty())
        || w.and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    let has_w = |e: &Value| {
        b(e, &["write"])
            || s(e, &["access", "mode", "permission"])
                .is_some_and(|m| m.to_ascii_lowercase().contains("write"))
    };
    match fs.get("entries") {
        Some(Value::Array(arr)) => arr.iter().any(has_w),
        Some(Value::Object(map)) => map.values().any(has_w),
        _ => false,
    }
}

/// Extracts human-readable labels without leaking native grant objects.
fn summarize_permissions(val: &Value) -> Vec<String> {
    let mut out = Vec::new();
    match val {
        Value::Array(arr) => out.extend(
            arr.iter()
                .filter_map(|v| {
                    v.as_str()
                        .or_else(|| s(v, &["label", "scope", "kind", "name"]))
                })
                .map(Into::into),
        ),
        Value::Object(map) => {
            for (k, v) in map {
                if let Some(sub) = v.as_object() {
                    out.extend(sub.keys().map(|sk| format!("{k}:{sk}")));
                } else if !v.is_null() && v != &Value::Bool(false) {
                    out.push(k.clone());
                }
            }
        }
        Value::String(st) => out.push(st.clone()),
        _ => {}
    }
    out
}

fn extract_questions(params: &Value) -> (Vec<UserQuestionItem>, bool) {
    let (mut secret, mut items) = (false, Vec::new());
    let Some(qs) = params.get("questions").and_then(Value::as_array) else {
        return (items, false);
    };
    for q in qs {
        let is_secret = b(q, &["isSecret", "is_secret"]);
        secret |= is_secret;
        let mut options = Vec::new();
        if let Some(os) = q.get("options").and_then(Value::as_array) {
            for o in os {
                if let Some(lbl) = o.as_str().or_else(|| s(o, &["label", "value"])) {
                    options.push(QuestionOption {
                        label: lbl.into(),
                        description: s_own(o, &["description"]),
                    });
                }
            }
        }
        items.push(UserQuestionItem {
            id: s(q, &["id"]).unwrap_or("q0").into(),
            header: s_own(q, &["header"]),
            question: s(q, &["question"]).unwrap_or_default().into(),
            options,
            allows_other: b(q, &["isOther", "allowsOther", "allows_other"]),
            is_secret,
        });
    }
    (items, secret)
}

pub(crate) fn map_server_request(
    raw_id: Value,
    key: String,
    method: &str,
    params: &Value,
    active_turn: Option<&str>,
    cached: Option<Vec<FileChangeEntry>>,
) -> Option<(InteractionRequest, PendingInteraction)> {
    use DecisionKind::*;
    let turn_id = s(params, &["turnId", "turn_id"])
        .or(active_turn)
        .unwrap_or("unknown")
        .to_owned();
    let reason = s_own(params, &["reason"]);
    let std_opts = |sess: &str| {
        opts(&[
            ("allow_once", "Allow once", AllowOnce),
            ("allow_session", sess, AllowSession),
            ("deny_once", "Decline", DenyOnce),
            ("cancel", "Cancel", Cancel),
        ])
    };
    let (mut protected, mut schema) = (false, None);
    let (kind, title, decisions, native_kind) = match method {
        "item/commandExecution/requestApproval" => {
            let cmd = extract_argv(params);
            let t = if cmd.is_empty() {
                "Command execution approval".into()
            } else {
                format!("Run command: {}", cmd.join(" "))
            };
            let k = InteractionKind::CommandApproval {
                command: cmd,
                cwd: s_own(params, &["cwd"]),
                reason,
            };
            (
                k,
                t,
                std_opts("Allow for session"),
                NativePendingKind::CommandApproval,
            )
        }
        "item/fileChange/requestApproval" => {
            let mut changes = extract_changes(params);
            if changes.is_empty() {
                changes = cached.unwrap_or_default();
            }
            let n = changes.len();
            let (t, r, sess) = match s(params, &["grantRoot", "grant_root"]) {
                Some(rt) => (
                    format!("Approve {n} file change(s) (grantRoot={rt})"),
                    Some(reason.map_or_else(
                        || format!("grantRoot={rt}"),
                        |r| format!("{r} (grantRoot={rt})"),
                    )),
                    format!("Allow for session (grantRoot={rt})"),
                ),
                None => (
                    format!("Approve {n} file change(s)"),
                    reason,
                    "Allow for session".into(),
                ),
            };
            let k = InteractionKind::FileChangeApproval { changes, reason: r };
            (k, t, std_opts(&sess), NativePendingKind::FileChangeApproval)
        }
        "item/permissions/requestApproval" => {
            protected = true;
            let native = params
                .get("permissions")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let ds = opts(&[
                ("allow_once", "Allow for turn", AllowOnce),
                ("allow_session", "Allow for session", AllowSession),
                ("deny_once", "Deny", DenyOnce),
            ]);
            let k = InteractionKind::PermissionRequest {
                permissions: summarize_permissions(&native),
                reason,
            };
            (
                k,
                "Permission grant requested".into(),
                ds,
                NativePendingKind::Permissions(native),
            )
        }
        "item/tool/requestUserInput" => {
            let (questions, any_secret) = extract_questions(params);
            protected = any_secret;
            let t = questions
                .first()
                .and_then(|q| q.header.clone())
                .unwrap_or_else(|| "User input requested".into());
            let ds = opts(&[("submit", "Submit", Submit), ("cancel", "Cancel", Cancel)]);
            (
                InteractionKind::UserQuestion { questions },
                t,
                ds,
                NativePendingKind::UserInput,
            )
        }
        "mcpServer/elicitation/request" => {
            protected = true;
            let srv = s(params, &["serverName", "server_name"])
                .unwrap_or("mcp")
                .to_owned();
            schema = params
                .get("requestedSchema")
                .or_else(|| params.get("requested_schema"))
                .cloned()
                .filter(|v| !v.is_null());
            let ds = opts(&[
                ("allow_once", "Accept", AllowOnce),
                ("deny_once", "Decline", DenyOnce),
                ("cancel", "Cancel", Cancel),
            ]);
            let k = InteractionKind::McpElicitation {
                server_name: srv.clone(),
                message: s(params, &["message"]).unwrap_or_default().into(),
                requested_schema: schema.clone(),
            };
            (
                k,
                format!("MCP elicitation from {srv}"),
                ds,
                NativePendingKind::McpElicitation,
            )
        }
        _ => return None,
    };

    let allowed = decisions.iter().map(|d| d.decision_id.clone()).collect();
    let req = InteractionRequest {
        interaction_id: Uuid::now_v7().to_string(),
        turn_id: turn_id.clone(),
        kind,
        original_kind: method.into(),
        title,
        decisions,
        protected,
        response_schema: schema,
        max_response_bytes: None,
    };
    Some((
        req,
        PendingInteraction {
            raw_id,
            key,
            turn_id,
            native_kind,
            allowed,
        },
    ))
}

fn err_str(v: &Value, fallback: &Value) -> Option<String> {
    v.get("error")
        .and_then(|x| x.as_str().or_else(|| s(x, &["message"])))
        .or_else(|| s(fallback, &["error", "message"]))
        .map(str::to_owned)
}

fn tool_ev(tid: String, id: String, tool: &str, st: ToolActivityStatus) -> NormalizedEvent {
    NormalizedEvent::ToolActivity {
        turn_id: tid,
        item_id: id,
        tool_name: tool.into(),
        status: st,
        summary: None,
        command: None,
        exit_status: None,
        paths: Vec::new(),
    }
}

fn map_item(
    params: &Value,
    done: bool,
    active: Option<&str>,
    schema: bool,
) -> Option<NormalizedEvent> {
    let item = params.get("item").unwrap_or(params);
    let tid = s(params, &["turnId", "turn_id"])
        .or_else(|| s(item, &["turnId", "turn_id"]))
        .or(active)
        .unwrap_or("unknown")
        .into();
    let id = s(item, &["id", "itemId", "item_id"])
        .or_else(|| s(params, &["itemId", "item_id"]))
        .unwrap_or("item")
        .into();
    let failed = s(item, &["status"]) == Some("failed");
    let st = |f: bool| {
        if !done {
            ToolActivityStatus::Started
        } else if f {
            ToolActivityStatus::Failed
        } else {
            ToolActivityStatus::Completed
        }
    };

    match s(item, &["type"]).unwrap_or_default() {
        "agentMessage" if done => {
            let text = s_own(item, &["text"]).unwrap_or_else(|| {
                item.get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|x| s(x, &["text"]))
                    .collect()
            });
            let so = schema
                .then(|| serde_json::from_str::<Value>(&text).ok())
                .flatten();
            Some(NormalizedEvent::AssistantMessage {
                turn_id: tid,
                item_id: Some(id),
                text,
                structured_output: so,
            })
        }
        "commandExecution" => {
            let argv = extract_argv(item);
            let exit = item
                .as_object()
                .and_then(|o| {
                    ["exitCode", "exit_code", "exitStatus"]
                        .iter()
                        .find_map(|k| o.get(*k)?.as_i64())
                })
                .and_then(|i| i32::try_from(i).ok());
            let mut ev = tool_ev(
                tid,
                id,
                "command_execution",
                st(failed || exit.is_some_and(|c| c != 0)),
            );
            if let NormalizedEvent::ToolActivity {
                summary,
                command,
                exit_status,
                ..
            } = &mut ev
            {
                *summary = s_own(item, &["aggregatedOutput", "output", "summary"]);
                *command = (!argv.is_empty()).then(|| argv.join(" "));
                *exit_status = exit;
            }
            Some(ev)
        }
        "fileChange" => {
            let mut ev = tool_ev(tid, id, "file_change", st(failed));
            if let NormalizedEvent::ToolActivity { summary, paths, .. } = &mut ev {
                *summary = s_own(item, &["summary"]);
                *paths = extract_changes(item).into_iter().map(|c| c.path).collect();
            }
            Some(ev)
        }
        "mcpToolCall" => {
            let mut ev = tool_ev(
                tid,
                id,
                s(item, &["tool", "toolName", "name"]).unwrap_or("mcp_tool"),
                st(failed),
            );
            if let NormalizedEvent::ToolActivity { summary, .. } = &mut ev {
                *summary = s_own(item, &["summary"]);
            }
            Some(ev)
        }
        _ => None,
    }
}

pub(crate) fn map_notification(
    method: &str,
    params: &Value,
    active: Option<&str>,
    schema: bool,
) -> Option<NormalizedEvent> {
    let opt_turn = s(params, &["turnId", "turn_id"])
        .or(active)
        .map(str::to_owned);
    let tid = opt_turn.clone().unwrap_or_else(|| "unknown".into());
    match method {
        "turn/started" => Some(NormalizedEvent::TurnStarted {
            turn_id: params
                .get("turn")
                .and_then(|t| s_own(t, &["id"]))
                .unwrap_or(tid),
        }),
        "item/agentMessage/delta" => Some(NormalizedEvent::TextDelta {
            turn_id: tid,
            item_id: s_own(params, &["itemId", "item_id"]),
            delta: s(params, &["delta"]).unwrap_or_default().into(),
        }),
        "item/started" => map_item(params, false, active, schema),
        "item/completed" => map_item(params, true, active, schema),
        "item/commandExecution/outputDelta"
        | "item/fileChange/outputDelta"
        | "item/fileChange/patchUpdated" => {
            let is_cmd = method.starts_with("item/command");
            let id = s(params, &["itemId", "item_id"])
                .unwrap_or(if is_cmd { "cmd" } else { "file_change" })
                .into();
            let mut ev = tool_ev(
                tid,
                id,
                if is_cmd {
                    "command_execution"
                } else {
                    "file_change"
                },
                ToolActivityStatus::Delta,
            );
            if let NormalizedEvent::ToolActivity { summary, paths, .. } = &mut ev {
                *summary = s_own(params, &["delta", "patch"]);
                if !is_cmd {
                    *paths = extract_changes(params)
                        .into_iter()
                        .map(|c| c.path)
                        .collect();
                }
            }
            Some(ev)
        }
        "thread/tokenUsage/updated" => {
            let ug = params
                .get("tokenUsage")
                .and_then(|t| t.get("total").or(Some(t)))
                .or_else(|| params.get("usage"))
                .unwrap_or(params);
            let metrics = UsageMetrics {
                input_tokens: u(ug, &["inputTokens", "input_tokens"]),
                output_tokens: u(ug, &["outputTokens", "output_tokens"]),
                cached_input_tokens: u(ug, &["cachedInputTokens", "cached_input_tokens"]),
                reasoning_tokens: u(
                    ug,
                    &[
                        "reasoningOutputTokens",
                        "reasoningTokens",
                        "reasoning_tokens",
                    ],
                ),
                total_cost_usd: ug
                    .get("totalCostUsd")
                    .or_else(|| ug.get("total_cost_usd"))
                    .and_then(Value::as_f64),
            };
            Some(NormalizedEvent::Usage {
                turn_id: opt_turn,
                metrics,
            })
        }
        "turn/completed" => {
            let turn = params.get("turn").unwrap_or(params);
            let status = match s(turn, &["status"]).unwrap_or("completed") {
                "completed" | "success" => TurnTerminalStatus::Completed,
                "interrupted" | "cancelled" | "canceled" => TurnTerminalStatus::Interrupted,
                "failed" | "error" => TurnTerminalStatus::Failed,
                _ => TurnTerminalStatus::Unknown,
            };
            Some(NormalizedEvent::TurnCompleted {
                turn_id: s_own(turn, &["id"]).unwrap_or(tid),
                status,
                error: err_str(turn, params),
            })
        }
        "error" => Some(NormalizedEvent::Error {
            turn_id: opt_turn,
            message: err_str(params, params).unwrap_or_else(|| "codex error notification".into()),
            will_retry: b(params, &["willRetry", "will_retry"]),
        }),
        _ => Some(NormalizedEvent::Unknown {
            event_type: method.into(),
            payload: Some(params.clone()),
        }),
    }
}
