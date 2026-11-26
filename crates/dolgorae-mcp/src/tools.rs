//! Tool schemas and UDS dispatch for External Host Mode (15 tools) and
//! Scoped Actor Mode (7 tools).

use std::collections::BTreeMap;
use std::path::PathBuf;

use dolgorae_api::{UdsClient, url_encode};
use dolgorae_core::{
    AccessMode, AnswerInteractionRequest, ApprovalPolicy, AttachmentRef, CapabilitiesSnapshot,
    CoreError, CreateSessionRequest, InteractionList, MAX_INLINE_BYTES, MailboxMessage,
    MailboxPage, PostMailboxRequest, SessionPolicy, SessionSnapshot, SubmitTurnRequest,
    TimelinePage, TurnAccepted, WaitMode, WaitSessionsRequest, WaitSessionsResponse, WriterLease,
};
use serde_json::{Value, json};

use crate::McpMode;

const DEFAULT_MCP_WAIT_TIMEOUT_MS: u64 = 25_000;
const MAX_MCP_WAIT_TIMEOUT_MS: u64 = 55_000;
const DEFAULT_MCP_ARTIFACT_SLICE: usize = 65_536;
const MAX_MCP_ARTIFACT_SLICE: usize = 65_536;

/// Returns the JSON-RPC `tools/list` array for `mode`.
pub fn list_tool_schemas(mode: McpMode) -> Vec<Value> {
    match mode {
        McpMode::ExternalHost => external_host_tools(),
        McpMode::ScopedActor => scoped_actor_tools(),
    }
}

fn tool(name: &str, description: &str, schema: Value) -> Value {
    json!({
        "name": name,
        "description": description,
        "inputSchema": schema,
    })
}

fn external_host_tools() -> Vec<Value> {
    vec![
        tool(
            "dolgorae_capabilities",
            "Probe registered drivers, models, and normative size bounds.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        tool(
            "dolgorae_role_list",
            "List resolved Role / Persona definitions.",
            json!({
                "type": "object",
                "properties": { "workspace": { "type": "string" } }
            }),
        ),
        tool(
            "dolgorae_session_start",
            "Start a root session with optional Role and SessionPolicy.",
            json!({
                "type": "object",
                "required": ["workspace"],
                "properties": {
                    "workspace": { "type": "string" },
                    "driver": { "type": "string" },
                    "role": { "type": "string" },
                    "parent_session_id": { "type": "string" },
                    "model": { "type": "string" },
                    "effort": { "type": "string" },
                    "access": { "type": "string", "enum": ["read_only", "workspace_write"] },
                    "approval_policy": { "type": "string", "enum": ["interactive", "auto_deny", "auto_approve_scoped"] },
                    "output_schema": { "type": "object" },
                    "allow_delegation": { "type": "boolean" },
                    "policy": { "type": "object" }
                }
            }),
        ),
        tool(
            "dolgorae_session_get",
            "Return current session snapshot with monotonic revision stamps.",
            json!({
                "type": "object",
                "required": ["session_id"],
                "properties": { "session_id": { "type": "string" } }
            }),
        ),
        tool(
            "dolgorae_session_wait",
            "Wait for any or all target sessions to settle (clamped <= 55s).",
            json!({
                "type": "object",
                "required": ["session_ids"],
                "properties": {
                    "session_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "mode": { "type": "string", "enum": ["any", "all"] },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 55000 },
                    "waiter_session_id": { "type": "string" }
                }
            }),
        ),
        tool(
            "dolgorae_session_close",
            "Gracefully close a session and terminate its driver.",
            json!({
                "type": "object",
                "required": ["session_id"],
                "properties": { "session_id": { "type": "string" } }
            }),
        ),
        tool(
            "dolgorae_writer_lease",
            "Acquire or release the optional single-writer lease on a canonical workspace.",
            json!({
                "type": "object",
                "required": ["workspace", "session_id"],
                "properties": {
                    "workspace": { "type": "string" },
                    "session_id": { "type": "string" },
                    "release": { "type": "boolean" }
                }
            }),
        ),
        tool(
            "dolgorae_writer_transfer",
            "Atomically transfer an active workspace single-writer lease.",
            json!({
                "type": "object",
                "required": ["workspace", "from_session_id", "to_session_id"],
                "properties": {
                    "workspace": { "type": "string" },
                    "from_session_id": { "type": "string" },
                    "to_session_id": { "type": "string" }
                }
            }),
        ),
        tool(
            "dolgorae_turn_submit",
            "Submit a turn to an active session.",
            json!({
                "type": "object",
                "required": ["session_id", "text"],
                "properties": {
                    "session_id": { "type": "string" },
                    "text": { "type": "string" },
                    "effort": { "type": "string" },
                    "attachments": { "type": "array" },
                    "output_schema_override": { "type": "object" }
                }
            }),
        ),
        tool(
            "dolgorae_turn_interrupt",
            "Request interruption of an in-flight turn.",
            json!({
                "type": "object",
                "required": ["session_id", "turn_id"],
                "properties": {
                    "session_id": { "type": "string" },
                    "turn_id": { "type": "string" }
                }
            }),
        ),
        tool(
            "dolgorae_timeline_read",
            "Read bounded timeline items with distinct highest_cursor and next_page_token.",
            json!({
                "type": "object",
                "required": ["session_id"],
                "properties": {
                    "session_id": { "type": "string" },
                    "page_token": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500 }
                }
            }),
        ),
        tool(
            "dolgorae_interaction_answer",
            "Answer a pending interaction with optimistic concurrency checks.",
            json!({
                "type": "object",
                "required": [
                    "session_id",
                    "interaction_id",
                    "turn_id",
                    "session_generation",
                    "expected_interaction_revision",
                    "expected_session_revision",
                    "decision_id"
                ],
                "properties": {
                    "session_id": { "type": "string" },
                    "interaction_id": { "type": "string" },
                    "turn_id": { "type": "string" },
                    "session_generation": { "type": "integer", "minimum": 1 },
                    "expected_interaction_revision": { "type": "integer", "minimum": 0 },
                    "expected_session_revision": { "type": "integer", "minimum": 0 },
                    "decision_id": { "type": "string" },
                    "answers": { "type": "object" },
                    "payload": {}
                }
            }),
        ),
        tool(
            "dolgorae_mailbox_send",
            "Post a durable message to a session's mailbox.",
            json!({
                "type": "object",
                "required": ["session_id", "body"],
                "properties": {
                    "session_id": { "type": "string" },
                    "from_session_id": { "type": "string" },
                    "body": { "type": "string" },
                    "subject": { "type": "string" },
                    "artifact_ids": { "type": "array", "items": { "type": "string" } }
                }
            }),
        ),
        tool(
            "dolgorae_mailbox_read",
            "Read durable mailbox entries strictly after cursor after.",
            json!({
                "type": "object",
                "required": ["session_id"],
                "properties": {
                    "session_id": { "type": "string" },
                    "after": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500 }
                }
            }),
        ),
        tool(
            "dolgorae_artifact_read",
            "Read a bounded slice of an immutable artifact (<= 64 KiB per page).",
            json!({
                "type": "object",
                "required": ["artifact_id"],
                "properties": {
                    "artifact_id": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 65536 }
                }
            }),
        ),
    ]
}

fn scoped_actor_tools() -> Vec<Value> {
    vec![
        tool(
            "actor_delegate",
            "Spawn a scoped child actor under self and submit its initial prompt.",
            json!({
                "type": "object",
                "required": ["prompt"],
                "properties": {
                    "role": { "type": "string" },
                    "driver": { "type": "string" },
                    "model": { "type": "string" },
                    "effort": { "type": "string" },
                    "workspace": { "type": "string" },
                    "access": { "type": "string", "enum": ["read_only", "workspace_write"] },
                    "approval_policy": { "type": "string", "enum": ["interactive", "auto_deny", "auto_approve_scoped"] },
                    "prompt": { "type": "string" },
                    "output_schema": { "type": "object" }
                }
            }),
        ),
        tool(
            "actor_child_status",
            "Inspect status of a direct child actor.",
            json!({
                "type": "object",
                "required": ["child_session_id"],
                "properties": { "child_session_id": { "type": "string" } }
            }),
        ),
        tool(
            "actor_child_wait",
            "Wait for any or all same-tree child/target sessions (clamped <= 55s).",
            json!({
                "type": "object",
                "required": ["child_session_ids"],
                "properties": {
                    "child_session_ids": { "type": "array", "items": { "type": "string" }, "minItems": 1 },
                    "mode": { "type": "string", "enum": ["any", "all"] },
                    "timeout_ms": { "type": "integer", "minimum": 1, "maximum": 55000 }
                }
            }),
        ),
        tool(
            "actor_child_cancel",
            "Close a direct child actor and revoke its scoped token.",
            json!({
                "type": "object",
                "required": ["child_session_id"],
                "properties": { "child_session_id": { "type": "string" } }
            }),
        ),
        tool(
            "actor_mailbox_send",
            "Deliver a durable mailbox message within the same session tree.",
            json!({
                "type": "object",
                "required": ["target_session_id", "body"],
                "properties": {
                    "target_session_id": { "type": "string" },
                    "body": { "type": "string" },
                    "subject": { "type": "string" },
                    "artifact_ids": { "type": "array", "items": { "type": "string" } }
                }
            }),
        ),
        tool(
            "actor_mailbox_read",
            "Read durable mailbox messages addressed to self.",
            json!({
                "type": "object",
                "properties": {
                    "after": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500 }
                }
            }),
        ),
        tool(
            "actor_artifact_read",
            "Read a bounded slice of an immutable artifact accessible to self's tree (<= 64 KiB per page).",
            json!({
                "type": "object",
                "required": ["artifact_id"],
                "properties": {
                    "artifact_id": { "type": "string" },
                    "offset": { "type": "integer", "minimum": 0 },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": 65536 }
                }
            }),
        ),
    ]
}

/// Dispatches an MCP `tools/call` invocation to `UdsClient` enforcing mode boundaries.
pub async fn dispatch_tool(
    mode: McpMode,
    client: &UdsClient,
    self_session_id: Option<&str>,
    name: &str,
    args: Value,
) -> Result<Value, CoreError> {
    match mode {
        McpMode::ExternalHost => dispatch_external_tool(client, name, args).await,
        McpMode::ScopedActor => dispatch_scoped_tool(client, self_session_id, name, args).await,
    }
}

async fn dispatch_external_tool(
    client: &UdsClient,
    name: &str,
    args: Value,
) -> Result<Value, CoreError> {
    match name {
        "dolgorae_capabilities" => {
            let caps: CapabilitiesSnapshot = client.get_json("/v2/capabilities").await?;
            to_val(caps)
        }
        "dolgorae_role_list" => {
            let path = match opt_str(&args, "workspace") {
                Some(ws) => format!("/v2/roles?workspace={}", url_encode(ws)),
                None => "/v2/roles".to_owned(),
            };
            client.get_json(&path).await
        }
        "dolgorae_session_start" => {
            let req = CreateSessionRequest {
                workspace: PathBuf::from(req_str(&args, "workspace")?),
                driver: opt_str(&args, "driver").map(str::to_owned),
                role: opt_str(&args, "role").map(str::to_owned),
                parent_session_id: opt_str(&args, "parent_session_id").map(str::to_owned),
                model: opt_str(&args, "model").map(str::to_owned),
                effort: opt_str(&args, "effort").map(str::to_owned),
                access: opt_deser::<AccessMode>(&args, "access")?,
                approval_policy: opt_deser::<ApprovalPolicy>(&args, "approval_policy")?,
                output_schema: args.get("output_schema").cloned().filter(|v| !v.is_null()),
                allow_delegation: args
                    .get("allow_delegation")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                policy: opt_deser::<SessionPolicy>(&args, "policy")?,
            };
            let snap: SessionSnapshot = client.post_json("/v2/sessions", &req).await?;
            to_val(snap)
        }
        "dolgorae_session_get" => {
            let sid = req_str(&args, "session_id")?;
            let snap: SessionSnapshot = client
                .get_json(&format!("/v2/sessions/{}", url_encode(sid)))
                .await?;
            to_val(snap)
        }
        "dolgorae_session_wait" => {
            let session_ids = req_str_vec(&args, "session_ids")?;
            let mode = opt_deser::<WaitMode>(&args, "mode")?.unwrap_or(WaitMode::All);
            let timeout_ms = clamp_wait_timeout(args.get("timeout_ms").and_then(Value::as_u64));
            let req = WaitSessionsRequest {
                waiter_session_id: opt_str(&args, "waiter_session_id").map(str::to_owned),
                session_ids,
                mode,
                timeout_ms,
            };
            let resp: WaitSessionsResponse = client.post_json("/v2/sessions/wait", &req).await?;
            to_val(resp)
        }
        "dolgorae_session_close" => {
            let sid = req_str(&args, "session_id")?;
            let snap: SessionSnapshot = client
                .delete_json(&format!("/v2/sessions/{}", url_encode(sid)))
                .await?;
            to_val(snap)
        }
        "dolgorae_writer_lease" => {
            let workspace = req_str(&args, "workspace")?;
            let session_id = req_str(&args, "session_id")?;
            let release = args
                .get("release")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if release {
                let path = format!(
                    "/v2/workspaces/lease?workspace={}&session_id={}",
                    url_encode(workspace),
                    url_encode(session_id)
                );
                client.delete_json(&path).await
            } else {
                let lease: WriterLease = client
                    .post_json(
                        "/v2/workspaces/lease",
                        &json!({ "workspace": workspace, "session_id": session_id }),
                    )
                    .await?;
                to_val(lease)
            }
        }
        "dolgorae_writer_transfer" => {
            let workspace = req_str(&args, "workspace")?;
            let from_session_id = req_str(&args, "from_session_id")?;
            let to_session_id = req_str(&args, "to_session_id")?;
            let lease: WriterLease = client
                .post_json(
                    "/v2/workspaces/lease/transfer",
                    &json!({
                        "workspace": workspace,
                        "from_session_id": from_session_id,
                        "to_session_id": to_session_id,
                    }),
                )
                .await?;
            to_val(lease)
        }
        "dolgorae_turn_submit" => {
            let sid = req_str(&args, "session_id")?;
            let req = SubmitTurnRequest {
                text: req_str(&args, "text")?.to_owned(),
                effort: opt_str(&args, "effort").map(str::to_owned),
                attachments: opt_deser::<Vec<AttachmentRef>>(&args, "attachments")?
                    .unwrap_or_default(),
                output_schema_override: args
                    .get("output_schema_override")
                    .cloned()
                    .filter(|v| !v.is_null()),
            };
            let accepted: TurnAccepted = client
                .post_json(&format!("/v2/sessions/{}/turns", url_encode(sid)), &req)
                .await?;
            to_val(accepted)
        }
        "dolgorae_turn_interrupt" => {
            let sid = req_str(&args, "session_id")?;
            let tid = req_str(&args, "turn_id")?;
            client
                .post_empty(&format!(
                    "/v2/sessions/{}/turns/{}/interrupt",
                    url_encode(sid),
                    url_encode(tid)
                ))
                .await
        }
        "dolgorae_timeline_read" => {
            let sid = req_str(&args, "session_id")?;
            let mut path = format!("/v2/sessions/{}/timeline", url_encode(sid));
            let mut params = Vec::new();
            if let Some(tok) = opt_str(&args, "page_token") {
                params.push(format!("page_token={}", url_encode(tok)));
            }
            if let Some(lim) = args.get("limit").and_then(Value::as_u64) {
                params.push(format!("limit={lim}"));
            }
            if !params.is_empty() {
                path.push('?');
                path.push_str(&params.join("&"));
            }
            let page: TimelinePage = client.get_json(&path).await?;
            to_val(page)
        }
        "dolgorae_interaction_answer" => {
            let sid = req_str(&args, "session_id")?;
            let iid = req_str(&args, "interaction_id")?;
            let req = AnswerInteractionRequest {
                turn_id: req_str(&args, "turn_id")?.to_owned(),
                session_generation: req_u64(&args, "session_generation")?,
                expected_interaction_revision: req_u64(&args, "expected_interaction_revision")?,
                expected_session_revision: req_u64(&args, "expected_session_revision")?,
                decision_id: req_str(&args, "decision_id")?.to_owned(),
                answers: opt_deser::<BTreeMap<String, Vec<String>>>(&args, "answers")?
                    .unwrap_or_default(),
                payload: args.get("payload").cloned().filter(|v| !v.is_null()),
            };
            client
                .post_json(
                    &format!(
                        "/v2/sessions/{}/interactions/{}/answer",
                        url_encode(sid),
                        url_encode(iid)
                    ),
                    &req,
                )
                .await
        }
        "dolgorae_mailbox_send" => {
            let sid = req_str(&args, "session_id")?;
            let req = PostMailboxRequest {
                from_session_id: opt_str(&args, "from_session_id").map(str::to_owned),
                subject: opt_str(&args, "subject").map(str::to_owned),
                body: req_str(&args, "body")?.to_owned(),
                artifact_ids: opt_deser::<Vec<String>>(&args, "artifact_ids")?.unwrap_or_default(),
            };
            let msg: MailboxMessage = client
                .post_json(&format!("/v2/sessions/{}/mailbox", url_encode(sid)), &req)
                .await?;
            to_val(msg)
        }
        "dolgorae_mailbox_read" => {
            let sid = req_str(&args, "session_id")?;
            let path = build_mailbox_read_path(sid, &args);
            let page: MailboxPage = client.get_json(&path).await?;
            to_val(page)
        }
        "dolgorae_artifact_read" => read_artifact_tool(client, &args).await,
        other => Err(CoreError::bad_request(format!(
            "unknown MCP tool '{other}'"
        ))),
    }
}

async fn dispatch_scoped_tool(
    client: &UdsClient,
    self_session_id: Option<&str>,
    name: &str,
    args: Value,
) -> Result<Value, CoreError> {
    match name {
        "actor_delegate" => {
            let self_snap = resolve_self_session(client, self_session_id).await?;
            let workspace = match opt_str(&args, "workspace") {
                Some(ws) => PathBuf::from(ws),
                None => PathBuf::from(&self_snap.workspace),
            };
            let prompt = req_str(&args, "prompt")?.to_owned();
            let role = opt_str(&args, "role").map(str::to_owned);
            let driver = opt_str(&args, "driver").map(str::to_owned).or_else(|| {
                if role.is_none() {
                    Some(self_snap.driver.clone())
                } else {
                    None
                }
            });
            let req = CreateSessionRequest {
                workspace,
                driver,
                role,
                parent_session_id: Some(self_snap.session_id.clone()),
                model: opt_str(&args, "model").map(str::to_owned),
                effort: opt_str(&args, "effort").map(str::to_owned),
                access: opt_deser::<AccessMode>(&args, "access")?,
                approval_policy: opt_deser::<ApprovalPolicy>(&args, "approval_policy")?,
                output_schema: args.get("output_schema").cloned().filter(|v| !v.is_null()),
                allow_delegation: false,
                policy: None,
            };
            let child: SessionSnapshot = client.post_json("/v2/sessions", &req).await?;
            let turn_req = SubmitTurnRequest {
                text: prompt,
                effort: opt_str(&args, "effort").map(str::to_owned),
                attachments: Vec::new(),
                output_schema_override: None,
            };
            let turn: TurnAccepted = client
                .post_json(
                    &format!("/v2/sessions/{}/turns", url_encode(&child.session_id)),
                    &turn_req,
                )
                .await?;
            Ok(json!({
                "session": child,
                "turn": turn,
            }))
        }
        "actor_child_status" => {
            let child_id = req_str(&args, "child_session_id")?;
            let self_id = resolve_self_id(client, self_session_id).await?;
            let snap: SessionSnapshot = client
                .get_json(&format!("/v2/sessions/{}", url_encode(child_id)))
                .await?;
            if snap.parent_session_id.as_deref() != Some(self_id.as_str()) {
                return Err(CoreError::forbidden_scope(format!(
                    "session '{child_id}' is not a direct child of '{self_id}'"
                )));
            }
            let interactions: InteractionList = client
                .get_json(&format!(
                    "/v2/sessions/{}/interactions?pending_only=true",
                    url_encode(child_id)
                ))
                .await?;
            Ok(json!({
                "session": snap,
                "pending_interactions": interactions.interactions.len(),
            }))
        }
        "actor_child_wait" => {
            let session_ids = req_str_vec(&args, "child_session_ids")?;
            let mode = opt_deser::<WaitMode>(&args, "mode")?.unwrap_or(WaitMode::All);
            let timeout_ms = clamp_wait_timeout(args.get("timeout_ms").and_then(Value::as_u64));
            let req = WaitSessionsRequest {
                waiter_session_id: self_session_id.map(str::to_owned),
                session_ids,
                mode,
                timeout_ms,
            };
            let resp: WaitSessionsResponse = client.post_json("/v2/sessions/wait", &req).await?;
            to_val(resp)
        }
        "actor_child_cancel" => {
            let child_id = req_str(&args, "child_session_id")?;
            let snap: SessionSnapshot = client
                .delete_json(&format!("/v2/sessions/{}", url_encode(child_id)))
                .await?;
            to_val(snap)
        }
        "actor_mailbox_send" => {
            let target_id = req_str(&args, "target_session_id")?;
            let req = PostMailboxRequest {
                from_session_id: None,
                subject: opt_str(&args, "subject").map(str::to_owned),
                body: req_str(&args, "body")?.to_owned(),
                artifact_ids: opt_deser::<Vec<String>>(&args, "artifact_ids")?.unwrap_or_default(),
            };
            let msg: MailboxMessage = client
                .post_json(
                    &format!("/v2/sessions/{}/mailbox", url_encode(target_id)),
                    &req,
                )
                .await?;
            to_val(msg)
        }
        "actor_mailbox_read" => {
            let self_id = resolve_self_id(client, self_session_id).await?;
            let path = build_mailbox_read_path(&self_id, &args);
            let page: MailboxPage = client.get_json(&path).await?;
            to_val(page)
        }
        "actor_artifact_read" => read_artifact_tool(client, &args).await,
        other => Err(CoreError::forbidden_scope(format!(
            "tool '{other}' is not permitted in scoped actor mode"
        ))),
    }
}

async fn resolve_self_id(
    _client: &UdsClient,
    self_session_id: Option<&str>,
) -> Result<String, CoreError> {
    match self_session_id {
        Some(id) if !id.trim().is_empty() => Ok(id.to_owned()),
        _ => Err(CoreError::unauthorized(
            "missing DOLGORAE_SESSION_ID in scoped actor mode",
        )),
    }
}

async fn resolve_self_session(
    client: &UdsClient,
    self_session_id: Option<&str>,
) -> Result<SessionSnapshot, CoreError> {
    let id = resolve_self_id(client, self_session_id).await?;
    client
        .get_json(&format!("/v2/sessions/{}", url_encode(&id)))
        .await
}

async fn read_artifact_tool(client: &UdsClient, args: &Value) -> Result<Value, CoreError> {
    let artifact_id = req_str(args, "artifact_id")?;
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
    let max_bytes = args
        .get("max_bytes")
        .and_then(Value::as_u64)
        .and_then(|v| usize::try_from(v).ok())
        .unwrap_or(DEFAULT_MCP_ARTIFACT_SLICE)
        .clamp(1, MAX_MCP_ARTIFACT_SLICE);

    // Fetch `max_bytes + 1` bytes so we determine exact `eof` in one round-trip.
    let mut raw = client
        .get_artifact_slice(artifact_id, offset, max_bytes + 1)
        .await?;
    let mut eof = raw.len() <= max_bytes;
    if !eof {
        raw.truncate(max_bytes);
    }

    // If a non-EOF slice cuts mid-UTF-8 codepoint, trim back 1..=3 bytes to preserve
    // clean UTF-8 pages while advancing `next_offset` losslessly.
    if !eof && let Err(err) = std::str::from_utf8(&raw) {
        let valid_up_to = err.valid_up_to();
        if valid_up_to > 0 && raw.len() - valid_up_to < 4 && err.error_len().is_none() {
            raw.truncate(valid_up_to);
            eof = false;
        }
    }

    let bytes_read = raw.len();
    let next_offset = offset.saturating_add(bytes_read as u64);

    // Encode as UTF-8 when valid and safely under MAX_INLINE_BYTES after JSON escaping;
    // otherwise fall back to lossless hex (<= 2 * 65_536 = 131_072 << 262_144).
    let (encoding, content) = match std::str::from_utf8(&raw) {
        Ok(text) => {
            let escaped_len = serde_json::to_string(text)
                .map(|s| s.len())
                .unwrap_or(usize::MAX);
            if escaped_len <= MAX_INLINE_BYTES.saturating_sub(512) {
                ("utf-8", text.to_owned())
            } else {
                ("hex", encode_hex(&raw))
            }
        }
        Err(_) => ("hex", encode_hex(&raw)),
    };

    Ok(json!({
        "artifact_id": artifact_id,
        "offset": offset,
        "bytes_read": bytes_read,
        "next_offset": next_offset,
        "eof": eof,
        "encoding": encoding,
        "content": content,
    }))
}

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

fn build_mailbox_read_path(session_id: &str, args: &Value) -> String {
    let mut path = format!("/v2/sessions/{}/mailbox", url_encode(session_id));
    let mut params = Vec::new();
    if let Some(after) = opt_str(args, "after") {
        params.push(format!("after={}", url_encode(after)));
    }
    if let Some(lim) = args.get("limit").and_then(Value::as_u64) {
        params.push(format!("limit={lim}"));
    }
    if !params.is_empty() {
        path.push('?');
        path.push_str(&params.join("&"));
    }
    path
}

/// Clamps MCP wait tool `timeout_ms` to `[1, 55_000]` (default `25_000`) so the
/// tool returns `timed_out: true` before typical 60s client tool timeouts.
pub fn clamp_wait_timeout(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(DEFAULT_MCP_WAIT_TIMEOUT_MS)
        .clamp(1, MAX_MCP_WAIT_TIMEOUT_MS)
}

fn req_str<'a>(args: &'a Value, key: &str) -> Result<&'a str, CoreError> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| CoreError::bad_request(format!("missing required string field '{key}'")))
}

fn opt_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn req_u64(args: &Value, key: &str) -> Result<u64, CoreError> {
    args.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| CoreError::bad_request(format!("missing required integer field '{key}'")))
}

fn req_str_vec(args: &Value, key: &str) -> Result<Vec<String>, CoreError> {
    let arr = args
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| CoreError::bad_request(format!("missing required array field '{key}'")))?;
    let mut out = Vec::with_capacity(arr.len());
    for item in arr {
        let s = item
            .as_str()
            .ok_or_else(|| CoreError::bad_request(format!("'{key}' elements must be strings")))?;
        out.push(s.to_owned());
    }
    Ok(out)
}

fn opt_deser<T: serde::de::DeserializeOwned>(
    args: &Value,
    key: &str,
) -> Result<Option<T>, CoreError> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| CoreError::bad_request(format!("invalid field '{key}': {e}"))),
    }
}

fn to_val<T: serde::Serialize>(val: T) -> Result<Value, CoreError> {
    serde_json::to_value(val)
        .map_err(|e| CoreError::internal(format!("serialize tool output failed: {e}")))
}
