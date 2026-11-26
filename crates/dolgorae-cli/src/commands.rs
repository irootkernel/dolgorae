//! CLI command execution helpers backing `dolgorae`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use dolgorae_api::{API_VERSION, BoundUdsServer, DaemonLock, ServerConfig, UdsClient, url_encode};
use dolgorae_core::{
    AccessMode, AnswerInteractionRequest, ApprovalPolicy, CORE_VERSION, CapabilitiesSnapshot,
    CoreConfig, CoreEngine, CreateSessionRequest, InteractionList, MailboxMessage, MailboxPage,
    PostMailboxRequest, ResumeSessionRequest, RoleDetail, SessionPolicy, SessionSnapshot,
    SubmitTurnRequest, TimelinePage, TurnAccepted, WaitMode, WaitSessionsRequest,
    WaitSessionsResponse, WriterLease,
};
use dolgorae_mcp::{McpMode, McpServer, McpServerConfig};
use dolgorae_spi::AgentDriver;
use serde::Serialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

pub fn resolve_home_dir(explicit: Option<PathBuf>) -> Result<PathBuf, String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Some(v) = std::env::var_os("DOLGORAE_HOME")
        && !v.is_empty()
    {
        return Ok(PathBuf::from(v));
    }
    if let Some(home) = std::env::var_os("HOME")
        && !home.is_empty()
    {
        return Ok(PathBuf::from(home).join(".dolgorae"));
    }
    Err(
        "unable to determine Dolgorae home directory (set --home, DOLGORAE_HOME, or HOME)"
            .to_owned(),
    )
}

pub fn resolve_socket_path(explicit: Option<PathBuf>, home_dir: &Path) -> PathBuf {
    if let Some(p) = explicit {
        return p;
    }
    if let Some(v) = std::env::var_os("DOLGORAE_SOCKET")
        && !v.is_empty()
    {
        return PathBuf::from(v);
    }
    home_dir.join("run").join("dolgorae.sock")
}

pub fn print_value<T: Serialize>(json_mode: bool, value: &T, human: impl FnOnce() -> String) {
    if json_mode {
        println!(
            "{}",
            serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned())
        );
    } else {
        println!("{}", human());
    }
}

pub fn run_version(json_mode: bool) {
    let payload = json!({
        "name": "dolgorae",
        "version": format!("v{CORE_VERSION}"),
        "api_version": API_VERSION,
    });
    print_value(json_mode, &payload, || format!("dolgorae v{CORE_VERSION}"));
}

pub async fn run_serve(
    home_dir: PathBuf,
    socket_path: PathBuf,
    drivers: Vec<Arc<dyn AgentDriver>>,
) -> Result<(), String> {
    // CRITICAL: acquire exclusive DaemonLock BEFORE opening SQLite / running recovery.
    let lock = DaemonLock::acquire(&home_dir).map_err(|e| e.to_string())?;

    let mut cfg = CoreConfig::new(home_dir);
    cfg.mcp_bin_path = std::env::current_exe().ok();
    cfg.socket_path = Some(socket_path.clone());

    let engine = Arc::new(
        CoreEngine::new(cfg, drivers)
            .await
            .map_err(|e| e.to_string())?,
    );
    let server = BoundUdsServer::bind(lock, engine, ServerConfig::new(socket_path.clone()))
        .await
        .map_err(|e| e.to_string())?;

    tracing::info!(
        socket = %socket_path.display(),
        version = CORE_VERSION,
        "dolgorae serve listening"
    );

    server
        .serve_until(shutdown_signal())
        .await
        .map_err(|e| e.to_string())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        } else {
            std::future::pending::<()>().await;
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

pub async fn run_mcp(scoped: bool, socket_path: PathBuf) -> Result<(), String> {
    let mode = if scoped {
        McpMode::ScopedActor
    } else {
        McpMode::ExternalHost
    };
    let actor_token = if scoped {
        std::env::var("DOLGORAE_ACTOR_TOKEN")
            .ok()
            .map(Zeroizing::new)
    } else {
        None
    };
    let self_session_id = std::env::var("DOLGORAE_SESSION_ID").ok();

    let server = McpServer::new(McpServerConfig {
        mode,
        socket_path,
        actor_token,
        self_session_id,
    })
    .map_err(|e| e.to_string())?;

    let stdin = tokio::io::BufReader::new(tokio::io::stdin());
    let stdout = tokio::io::stdout();
    server
        .run_stdio(stdin, stdout)
        .await
        .map_err(|e| e.to_string())
}

pub async fn run_driver_list(
    json_mode: bool,
    socket_path: &Path,
    drivers: Vec<Arc<dyn AgentDriver>>,
) -> Result<(), String> {
    let client = UdsClient::new(socket_path.to_path_buf());
    let caps = match client
        .get_json::<CapabilitiesSnapshot>("/v2/capabilities")
        .await
    {
        Ok(c) => c.drivers,
        Err(_) => {
            let mut out = Vec::with_capacity(drivers.len());
            for d in drivers {
                match d.probe().await {
                    Ok(c) => out.push(c),
                    Err(e) => {
                        return Err(format!("driver probe failed: {e}"));
                    }
                }
            }
            out
        }
    };
    print_value(json_mode, &caps, || {
        caps.iter()
            .map(|c| {
                let state = if c.available {
                    "available"
                } else {
                    "unavailable"
                };
                let ver = c.cli_version.as_deref().unwrap_or("-");
                format!("{}\t{}\t{}", c.driver_id, state, ver)
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

pub async fn run_role_list(
    json_mode: bool,
    client: &UdsClient,
    workspace: Option<PathBuf>,
) -> Result<(), String> {
    let path = match workspace.as_deref().and_then(Path::to_str) {
        Some(ws) => format!("/v2/roles?workspace={}", url_encode(ws)),
        None => "/v2/roles".to_owned(),
    };
    let resp: Value = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &resp, || {
        resp["roles"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|r| {
                        format!(
                            "{}\t{}\t{}",
                            r["name"].as_str().unwrap_or("-"),
                            r["source"].as_str().unwrap_or("-"),
                            r["access"].as_str().unwrap_or("-")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    });
    Ok(())
}

pub async fn run_role_show(
    json_mode: bool,
    client: &UdsClient,
    name: &str,
    workspace: Option<PathBuf>,
) -> Result<(), String> {
    let mut path = format!("/v2/roles/{}", url_encode(name));
    if let Some(ws) = workspace.as_deref().and_then(Path::to_str) {
        path.push_str(&format!("?workspace={}", url_encode(ws)));
    }
    let detail: RoleDetail = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &detail, || {
        format!(
            "Role: {} ({:?})\nAccess: {:?}\nApproval: {:?}\nSHA256: {}\n\n{}",
            detail.summary.name,
            detail.summary.source,
            detail.summary.access,
            detail.summary.approval_policy,
            detail.summary.snapshot_sha256,
            detail.instructions
        )
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_session_start(
    json_mode: bool,
    client: &UdsClient,
    workspace: PathBuf,
    driver: Option<String>,
    role: Option<String>,
    parent_session_id: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    access: Option<AccessMode>,
    approval_policy: Option<ApprovalPolicy>,
    output_schema: Option<String>,
    allow_delegation: bool,
    max_depth: Option<u8>,
    allow_collaboration: Option<bool>,
) -> Result<(), String> {
    let parsed_schema = match output_schema {
        Some(raw) => Some(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("invalid --output-schema JSON: {e}"))?,
        ),
        None => None,
    };
    let policy = if max_depth.is_some() || allow_collaboration.is_some() {
        let mut p = SessionPolicy::default();
        if let Some(d) = max_depth {
            p.max_delegation_depth = d;
        }
        if let Some(c) = allow_collaboration {
            p.allow_actor_collaboration = c;
        }
        Some(p)
    } else {
        None
    };

    let req = CreateSessionRequest {
        workspace,
        driver,
        role,
        parent_session_id,
        model,
        effort,
        access,
        approval_policy,
        output_schema: parsed_schema,
        allow_delegation,
        policy,
    };
    let snap: SessionSnapshot = client
        .post_json("/v2/sessions", &req)
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &snap, || format_session_human(&snap));
    Ok(())
}

pub async fn run_session_list(
    json_mode: bool,
    client: &UdsClient,
    parent: Option<String>,
    root: Option<String>,
    status: Option<String>,
) -> Result<(), String> {
    let mut path = "/v2/sessions".to_owned();
    let mut qs = Vec::new();
    if let Some(p) = parent {
        qs.push(format!("parent_session_id={}", url_encode(&p)));
    }
    if let Some(r) = root {
        qs.push(format!("root_session_id={}", url_encode(&r)));
    }
    if let Some(s) = status {
        qs.push(format!("status={}", url_encode(&s)));
    }
    if !qs.is_empty() {
        path.push('?');
        path.push_str(&qs.join("&"));
    }
    let val: Value = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &val, || {
        val["sessions"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .map(|s| {
                        format!(
                            "{}\t{}\t{}\trev={}",
                            s["session_id"].as_str().unwrap_or("-"),
                            s["driver"].as_str().unwrap_or("-"),
                            s["status"].as_str().unwrap_or("-"),
                            s["revision"].as_u64().unwrap_or(0)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    });
    Ok(())
}

pub async fn run_session_get(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
) -> Result<(), String> {
    let snap: SessionSnapshot = client
        .get_json(&format!("/v2/sessions/{}", url_encode(session_id)))
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &snap, || format_session_human(&snap));
    Ok(())
}

pub async fn run_session_resume(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    model: Option<String>,
    effort: Option<String>,
    access: Option<AccessMode>,
    approval_policy: Option<ApprovalPolicy>,
) -> Result<(), String> {
    let req = ResumeSessionRequest {
        model,
        effort,
        access,
        approval_policy,
    };
    let snap: SessionSnapshot = client
        .post_json(
            &format!("/v2/sessions/{}/resume", url_encode(session_id)),
            &req,
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &snap, || format_session_human(&snap));
    Ok(())
}

pub async fn run_session_close(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
) -> Result<(), String> {
    let snap: SessionSnapshot = client
        .delete_json(&format!("/v2/sessions/{}", url_encode(session_id)))
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &snap, || format_session_human(&snap));
    Ok(())
}

pub async fn run_session_wait(
    json_mode: bool,
    client: &UdsClient,
    session_ids: Vec<String>,
    mode: WaitMode,
    timeout_ms: u64,
    waiter_session_id: Option<String>,
) -> Result<(), String> {
    let req = WaitSessionsRequest {
        waiter_session_id,
        session_ids,
        mode,
        timeout_ms,
    };
    let resp: WaitSessionsResponse = client
        .post_json("/v2/sessions/wait", &req)
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &resp, || {
        format!(
            "timed_out={} completed=[{}]",
            resp.timed_out,
            resp.completed_session_ids.join(", ")
        )
    });
    Ok(())
}

pub async fn run_session_turn(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    text: String,
    effort: Option<String>,
    output_schema: Option<String>,
) -> Result<(), String> {
    let output_schema_override = match output_schema {
        Some(raw) => Some(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("invalid --output-schema JSON: {e}"))?,
        ),
        None => None,
    };
    let req = SubmitTurnRequest {
        text,
        effort,
        attachments: Vec::new(),
        output_schema_override,
    };
    let acc: TurnAccepted = client
        .post_json(
            &format!("/v2/sessions/{}/turns", url_encode(session_id)),
            &req,
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &acc, || {
        format!(
            "turn={} session={} rev={}",
            acc.turn_id, acc.session_id, acc.revision
        )
    });
    Ok(())
}

pub async fn run_session_interrupt(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    turn_id: &str,
) -> Result<(), String> {
    let resp: Value = client
        .post_empty(&format!(
            "/v2/sessions/{}/turns/{}/interrupt",
            url_encode(session_id),
            url_encode(turn_id)
        ))
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &resp, || {
        format!("interrupt requested for turn {turn_id}")
    });
    Ok(())
}

pub async fn run_session_timeline(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    page_token: Option<String>,
    limit: usize,
) -> Result<(), String> {
    let mut path = format!(
        "/v2/sessions/{}/timeline?limit={limit}",
        url_encode(session_id)
    );
    if let Some(tok) = page_token {
        path.push_str(&format!("&page_token={}", url_encode(&tok)));
    }
    let page: TimelinePage = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &page, || {
        page.items
            .iter()
            .map(|i| {
                format!(
                    "[{}] {} {}",
                    i.cursor,
                    i.kind,
                    i.inline_text.as_deref().unwrap_or("")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

pub async fn run_session_events(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    after: Option<String>,
    max_events: Option<usize>,
) -> Result<(), String> {
    let path = format!("/v2/sessions/{}/events", url_encode(session_id));
    let mut stream = client
        .open_sse(&path, after.as_deref())
        .await
        .map_err(|e| e.to_string())?;
    let mut count = 0usize;
    while let Some(frame) = stream.next_frame().await.map_err(|e| e.to_string())? {
        if json_mode {
            println!(
                "{}",
                json!({
                    "id": frame.id,
                    "event": frame.event,
                    "data": serde_json::from_str::<Value>(&frame.data).unwrap_or(Value::String(frame.data)),
                })
            );
        } else {
            println!(
                "id={} event={} data={}",
                frame.id.as_deref().unwrap_or("-"),
                frame.event.as_deref().unwrap_or("message"),
                frame.data
            );
        }
        count += 1;
        if let Some(limit) = max_events
            && count >= limit
        {
            break;
        }
    }
    Ok(())
}

pub async fn run_session_interactions(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    all: bool,
) -> Result<(), String> {
    let path = format!(
        "/v2/sessions/{}/interactions?pending_only={}",
        url_encode(session_id),
        !all
    );
    let list: InteractionList = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &list, || {
        list.interactions
            .iter()
            .map(|i| format!("{}\t{:?}\t{}", i.interaction_id, i.status, i.title))
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn run_session_answer(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    interaction_id: &str,
    turn_id: String,
    session_generation: u64,
    expected_interaction_revision: u64,
    expected_session_revision: u64,
    decision_id: String,
    payload: Option<String>,
) -> Result<(), String> {
    let parsed_payload = match payload {
        Some(raw) => Some(
            serde_json::from_str::<Value>(&raw)
                .map_err(|e| format!("invalid --payload JSON: {e}"))?,
        ),
        None => None,
    };
    let req = AnswerInteractionRequest {
        turn_id,
        session_generation,
        expected_interaction_revision,
        expected_session_revision,
        decision_id,
        answers: BTreeMap::new(),
        payload: parsed_payload,
    };
    let resp: Value = client
        .post_json(
            &format!(
                "/v2/sessions/{}/interactions/{}/answer",
                url_encode(session_id),
                url_encode(interaction_id)
            ),
            &req,
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &resp, || {
        format!(
            "interaction {} resolved (rev={})",
            interaction_id,
            resp["session_revision"].as_u64().unwrap_or(0)
        )
    });
    Ok(())
}

pub async fn run_mailbox_send(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    body: String,
    subject: Option<String>,
    from_session_id: Option<String>,
) -> Result<(), String> {
    let req = PostMailboxRequest {
        from_session_id,
        subject,
        body,
        artifact_ids: Vec::new(),
    };
    let msg: MailboxMessage = client
        .post_json(
            &format!("/v2/sessions/{}/mailbox", url_encode(session_id)),
            &req,
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &msg, || {
        format!("sent {} at cursor {}", msg.message_id, msg.cursor)
    });
    Ok(())
}

pub async fn run_mailbox_read(
    json_mode: bool,
    client: &UdsClient,
    session_id: &str,
    after: Option<String>,
    limit: usize,
) -> Result<(), String> {
    let mut path = format!(
        "/v2/sessions/{}/mailbox?limit={limit}",
        url_encode(session_id)
    );
    if let Some(a) = after {
        path.push_str(&format!("&after={}", url_encode(&a)));
    }
    let page: MailboxPage = client.get_json(&path).await.map_err(|e| e.to_string())?;
    print_value(json_mode, &page, || {
        page.messages
            .iter()
            .map(|m| {
                format!(
                    "[{}] {} {}",
                    m.cursor,
                    m.subject.as_deref().unwrap_or("-"),
                    m.inline_body.as_deref().unwrap_or("<artifact>")
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    });
    Ok(())
}

pub async fn run_workspace_lease(
    json_mode: bool,
    client: &UdsClient,
    workspace: PathBuf,
    session_id: String,
) -> Result<(), String> {
    let lease: WriterLease = client
        .post_json(
            "/v2/workspaces/lease",
            &json!({ "workspace": workspace, "session_id": session_id }),
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &lease, || {
        format!(
            "lease acquired by {} on {}",
            lease.holder_session_id, lease.workspace
        )
    });
    Ok(())
}

pub async fn run_workspace_transfer(
    json_mode: bool,
    client: &UdsClient,
    workspace: PathBuf,
    from_session_id: String,
    to_session_id: String,
) -> Result<(), String> {
    let lease: WriterLease = client
        .post_json(
            "/v2/workspaces/lease/transfer",
            &json!({
                "workspace": workspace,
                "from_session_id": from_session_id,
                "to_session_id": to_session_id,
            }),
        )
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &lease, || {
        format!(
            "lease transferred to {} on {}",
            lease.holder_session_id, lease.workspace
        )
    });
    Ok(())
}

pub async fn run_workspace_release(
    json_mode: bool,
    client: &UdsClient,
    workspace: PathBuf,
    session_id: String,
) -> Result<(), String> {
    let ws_str = workspace
        .to_str()
        .ok_or_else(|| "workspace path must be valid UTF-8".to_owned())?;
    let resp: Value = client
        .delete_json(&format!(
            "/v2/workspaces/lease?workspace={}&session_id={}",
            url_encode(ws_str),
            url_encode(&session_id)
        ))
        .await
        .map_err(|e| e.to_string())?;
    print_value(json_mode, &resp, || {
        format!("released={}", resp["released"].as_bool().unwrap_or(false))
    });
    Ok(())
}

pub async fn run_artifact_read(
    json_mode: bool,
    client: &UdsClient,
    artifact_id: &str,
    offset: u64,
    max_bytes: usize,
) -> Result<(), String> {
    let bytes = client
        .get_artifact_slice(artifact_id, offset, max_bytes)
        .await
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let payload = json!({
        "artifact_id": artifact_id,
        "offset": offset,
        "byte_len": bytes.len(),
        "content": text,
    });
    print_value(json_mode, &payload, || text.clone());
    Ok(())
}

fn format_session_human(snap: &SessionSnapshot) -> String {
    format!(
        "session={} driver={} status={:?} gen={} rev={} head={} active_turn={}",
        snap.session_id,
        snap.driver,
        snap.status,
        snap.session_generation,
        snap.revision,
        snap.head_cursor,
        snap.active_turn_id.as_deref().unwrap_or("-")
    )
}
