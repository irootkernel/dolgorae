use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, InteractionAnswer, InteractionKind,
    McpServerInjection, NormalizedEvent, ResumeSpec, SessionSpec, ToolActivityStatus, TurnInput,
    TurnTerminalStatus,
};
use driver_codex_app::CodexDriver;
use serde_json::{Value, json};
use uuid::Uuid;

fn fake_codex_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fake_codex.py")
}

fn temp_dir(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dolgorae-codex-test-{label}-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn base_spec(
    workspace: PathBuf,
    access: AccessMode,
    approval_policy: ApprovalPolicy,
) -> SessionSpec {
    SessionSpec {
        workspace,
        role_instructions: Some("Be concise.".to_owned()),
        model: Some("gpt-5".to_owned()),
        effort: Some("high".to_owned()),
        access,
        approval_policy,
        output_schema: None,
        mcp_servers: Vec::new(),
    }
}

async fn next_event(rx: &mut dolgorae_spi::EventReceiver) -> NormalizedEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for event")
        .expect("event channel closed unexpectedly")
}

async fn drain_until_turn_completed(rx: &mut dolgorae_spi::EventReceiver) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    loop {
        let ev = next_event(rx).await;
        let done = matches!(ev, NormalizedEvent::TurnCompleted { .. });
        events.push(ev);
        if done {
            break;
        }
    }
    events
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str::<Value>(l).expect("valid jsonl"))
        .collect()
}

/// Wrapper script setting FAKE_CODEX_LOG without mutating process-wide env vars.
fn make_logged_wrapper(dir: &Path, log_path: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let wrapper = dir.join("fake-codex-wrapper.sh");
    let script = format!(
        "#!/bin/sh\nexport FAKE_CODEX_LOG='{}'\nexec python3 '{}' \"$@\"\n",
        log_path.display(),
        fake_codex_bin().display(),
    );
    std::fs::write(&wrapper, script).expect("write wrapper");
    let mut perms = std::fs::metadata(&wrapper)
        .expect("stat wrapper")
        .permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&wrapper, perms).expect("chmod wrapper");
    wrapper
}

#[tokio::test]
async fn probe_and_missing_binary_behave_cleanly() {
    let driver = CodexDriver::with_binary(fake_codex_bin());
    assert_eq!(driver.driver_id(), "codex-app");

    let caps = driver.probe().await.expect("probe fake codex");
    assert!(caps.available);
    assert_eq!(caps.cli_version.as_deref(), Some("codex-cli 0.172.0-fake"));
    assert!(caps.native_output_schema);
    assert!(caps.supports_effort);
    assert!(caps.supports_interactive_approval);
    assert!(caps.supports_mcp_injection);
    assert!(caps.supports_resume);

    let missing = CodexDriver::with_binary(PathBuf::from("/nonexistent/dolgorae-codex-missing"));
    let missing_caps = missing
        .probe()
        .await
        .expect("probe missing binary returns Ok");
    assert!(!missing_caps.available);
    assert!(missing_caps.unavailable_reason.is_some());
}

#[tokio::test]
async fn streaming_deltas_tool_activity_usage_output_schema_and_unknown_notification() {
    let dir = temp_dir("stream");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);

    let driver = CodexDriver::with_binary(wrapper);
    let mut spec = base_spec(
        dir.clone(),
        AccessMode::WorkspaceWrite,
        ApprovalPolicy::Interactive,
    );
    spec.output_schema = Some(json!({
        "type": "object",
        "properties": {"ok": {"type": "boolean"}},
        "required": ["ok"]
    }));

    let mut session = driver.start_session(spec).await.expect("start session");
    assert_eq!(session.provider_session_id(), "thr-fake-001");
    let mut rx = session.take_events().expect("take events");

    let ack = session
        .send_turn(TurnInput {
            text: "basic_stream".to_owned(),
            effort: Some("xhigh".to_owned()),
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");
    assert_eq!(ack.turn_id, "turn-1");

    let events = drain_until_turn_completed(&mut rx).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::TurnStarted { turn_id } if turn_id == "turn-1"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::TextDelta { delta, .. } if delta == "{\"ok\":"))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::AssistantMessage { text, structured_output: Some(so), .. }
            if text == "{\"ok\":true}" && so == &json!({"ok": true})
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::ToolActivity { tool_name, status: ToolActivityStatus::Completed, command: Some(cmd), exit_status: Some(0), .. }
            if tool_name == "command_execution" && cmd == "echo hello"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::ToolActivity { tool_name, paths, .. }
            if tool_name == "file_change" && paths == &vec!["src/lib.rs".to_owned()]
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::Usage { metrics, .. }
            if metrics.input_tokens == Some(120) && metrics.output_tokens == Some(30)
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::Unknown { event_type, .. } if event_type == "vendor/futureMetric"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::TurnCompleted {
            status: TurnTerminalStatus::Completed,
            ..
        }
    )));

    // Verify turn/start wire carried per-turn effort & native outputSchema.
    let transcript = read_jsonl(&log);
    let turn_start = transcript
        .iter()
        .find(|v| v["dir"] == "in" && v["msg"]["method"] == "turn/start")
        .expect("turn/start logged");
    assert_eq!(turn_start["msg"]["params"]["effort"], "xhigh");
    assert_eq!(
        turn_start["msg"]["params"]["outputSchema"]["type"],
        "object"
    );

    session.close().await.expect("close");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn mcp_actor_token_passed_via_stdin_never_in_argv() {
    let dir = temp_dir("mcp-secret");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);
    let secret_token = "SUPER_SECRET_ACTOR_TOKEN_991122";

    let mut env = BTreeMap::new();
    env.insert("DOLGORAE_ACTOR_TOKEN".to_owned(), secret_token.to_owned());

    let mut spec = base_spec(
        dir.clone(),
        AccessMode::WorkspaceWrite,
        ApprovalPolicy::Interactive,
    );
    spec.mcp_servers.push(McpServerInjection {
        name: "dolgorae".to_owned(),
        command: PathBuf::from("/usr/local/bin/dolgorae"),
        args: vec!["mcp".to_owned(), "--scoped".to_owned()],
        env,
    });

    let driver = CodexDriver::with_binary(wrapper);
    let session = driver.start_session(spec).await.expect("start session");
    session.close().await.expect("close");

    let transcript = read_jsonl(&log);
    let argv_record = transcript
        .iter()
        .find(|v| v["dir"] == "argv")
        .expect("argv recorded");
    let argv_str = serde_json::to_string(argv_record).expect("serialize argv");
    assert!(
        !argv_str.contains(secret_token),
        "DOLGORAE_ACTOR_TOKEN must NEVER appear in spawned argv: {argv_str}"
    );
    assert!(
        !argv_str.contains("mcp_servers"),
        "mcp_servers must NOT be passed in argv: {argv_str}"
    );

    let thread_start = transcript
        .iter()
        .find(|v| v["dir"] == "in" && v["msg"]["method"] == "thread/start")
        .expect("thread/start recorded");
    assert_eq!(
        thread_start["msg"]["params"]["config"]["mcp_servers"]["dolgorae"]["env"]["DOLGORAE_ACTOR_TOKEN"],
        secret_token
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn interactive_allow_and_deny_across_all_five_request_kinds() {
    let dir = temp_dir("interactive");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);

    let driver = CodexDriver::with_binary(wrapper);
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    session
        .send_turn(TurnInput {
            text: "interactive_all".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    let mut seen_kinds = Vec::new();
    loop {
        match next_event(&mut rx).await {
            NormalizedEvent::InteractionRequested { request } => {
                // Opaque UUID v7, never raw numeric wire ID.
                assert!(Uuid::parse_str(&request.interaction_id).is_ok());
                match &request.kind {
                    InteractionKind::CommandApproval { command, .. } => {
                        assert_eq!(command, &vec!["cargo".to_owned(), "check".to_owned()]);
                        seen_kinds.push("cmd");
                        session
                            .answer_interaction(InteractionAnswer {
                                interaction_id: request.interaction_id,
                                decision_id: "allow_once".to_owned(),
                                answers: BTreeMap::new(),
                                payload: None,
                            })
                            .await
                            .expect("allow cmd");
                    }
                    InteractionKind::FileChangeApproval { changes, reason } => {
                        assert_eq!(
                            changes.len(),
                            1,
                            "must populate changes from item/started cache"
                        );
                        assert_eq!(changes[0].path, "src/main.rs");
                        assert_eq!(changes[0].kind, "update");
                        assert_eq!(changes[0].move_path.as_deref(), Some("src/app.rs"));
                        assert!(
                            reason.as_deref().is_some_and(|r| r.contains("grantRoot=")),
                            "grantRoot must be disclosed in reason: {reason:?}"
                        );
                        assert!(
                            request.title.contains("grantRoot="),
                            "grantRoot must be disclosed in title: {}",
                            request.title
                        );
                        assert!(
                            request
                                .decisions
                                .iter()
                                .any(|d| d.decision_id == "allow_session"
                                    && d.label.contains("grantRoot=")),
                            "allow_session label must disclose grantRoot"
                        );
                        seen_kinds.push("file");
                        session
                            .answer_interaction(InteractionAnswer {
                                interaction_id: request.interaction_id,
                                decision_id: "deny_once".to_owned(),
                                answers: BTreeMap::new(),
                                payload: None,
                            })
                            .await
                            .expect("deny file");
                    }
                    InteractionKind::PermissionRequest { permissions, .. } => {
                        // Native secret token must NOT leak into SPI labels!
                        let serialized = serde_json::to_string(&request).expect("ser");
                        assert!(
                            !serialized.contains("SECRET_NATIVE_GRANT_PROFILE"),
                            "native grant value leaked into SPI: {serialized}"
                        );
                        assert!(!permissions.is_empty());
                        seen_kinds.push("perm");
                        session
                            .answer_interaction(InteractionAnswer {
                                interaction_id: request.interaction_id,
                                decision_id: "allow_session".to_owned(),
                                answers: BTreeMap::new(),
                                payload: None,
                            })
                            .await
                            .expect("allow perm");
                    }
                    InteractionKind::UserQuestion { questions } => {
                        assert!(request.protected, "isSecret must set protected=true");
                        assert_eq!(questions[0].id, "q-secret");
                        seen_kinds.push("question");
                        let mut answers = BTreeMap::new();
                        answers.insert("q-secret".to_owned(), vec!["use-env".to_owned()]);
                        session
                            .answer_interaction(InteractionAnswer {
                                interaction_id: request.interaction_id,
                                decision_id: "submit".to_owned(),
                                answers,
                                payload: None,
                            })
                            .await
                            .expect("submit question");
                    }
                    InteractionKind::McpElicitation { server_name, .. } => {
                        assert_eq!(server_name, "vault-mcp");
                        seen_kinds.push("elicitation");
                        session
                            .answer_interaction(InteractionAnswer {
                                interaction_id: request.interaction_id,
                                decision_id: "allow_once".to_owned(),
                                answers: BTreeMap::new(),
                                payload: Some(json!({"confirmed": true})),
                            })
                            .await
                            .expect("accept elicitation");
                    }
                    other => panic!("unexpected kind: {other:?}"),
                }
            }
            NormalizedEvent::TurnCompleted { .. } => break,
            _ => {}
        }
    }

    assert_eq!(
        seen_kinds,
        vec!["cmd", "file", "perm", "question", "elicitation"]
    );

    let transcript = read_jsonl(&log);
    let reply = |id: i64| -> Value {
        transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == id && v["msg"]["method"].is_null())
            .expect("reply logged")["msg"]["result"]
            .clone()
    };

    assert_eq!(reply(101)["decision"], "accept");
    assert_eq!(reply(102)["decision"], "decline");
    assert_eq!(reply(103)["scope"], "session");
    assert_eq!(
        reply(103)["permissions"]["opaqueNativeToken"],
        "SECRET_NATIVE_GRANT_PROFILE"
    );
    assert_eq!(
        reply(104)["answers"]["q-secret"]["answers"],
        json!(["use-env"])
    );
    assert_eq!(reply(105)["action"], "accept");
    assert_eq!(reply(105)["content"], json!({"confirmed": true}));

    session.close().await.expect("close");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_deny_policy_declines_all_requests_with_audit_trail() {
    let dir = temp_dir("auto-deny");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);

    let driver = CodexDriver::with_binary(wrapper);
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::AutoDeny,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    session
        .send_turn(TurnInput {
            text: "interactive_all".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    let events = drain_until_turn_completed(&mut rx).await;
    let req_count = events
        .iter()
        .filter(|e| matches!(e, NormalizedEvent::InteractionRequested { .. }))
        .count();
    let res_count = events
        .iter()
        .filter(|e| matches!(e, NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "deny_once"))
        .count();
    assert_eq!(req_count, 5);
    assert_eq!(res_count, 5);

    let transcript = read_jsonl(&log);
    let thread_start = transcript
        .iter()
        .find(|v| v["dir"] == "in" && v["msg"]["method"] == "thread/start")
        .expect("thread/start");
    assert_eq!(thread_start["msg"]["params"]["approvalPolicy"], "untrusted");

    session.close().await.expect("close");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_approve_scoped_allows_in_workspace_denies_escapes_and_escalations() {
    let dir = temp_dir("auto-scoped");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);

    let driver = CodexDriver::with_binary(wrapper);
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::AutoApproveScoped,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    session
        .send_turn(TurnInput {
            text: "auto_scoped_mix".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    let events = drain_until_turn_completed(&mut rx).await;
    let requested_count = events
        .iter()
        .filter(|e| matches!(e, NormalizedEvent::InteractionRequested { .. }))
        .count();
    assert_eq!(
        requested_count, 10,
        "every auto-evaluated request must emit an audit event"
    );
    let decisions: Vec<Option<String>> = events
        .iter()
        .filter_map(|e| match e {
            NormalizedEvent::InteractionResolved { decision_id, .. } => Some(decision_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        decisions,
        vec![
            Some("allow_once".to_owned()), // 201 inside workspace file (PatchChangeKind object)
            Some("deny_once".to_owned()),  // 202 ../escape.rs (from item/started cache)
            Some("deny_once".to_owned()),  // 203 command prompt requests sandbox escape
            Some("deny_once".to_owned()),  // 204 /etc cmd
            Some("deny_once".to_owned()),  // 205 permissions
            Some("deny_once".to_owned()),  // 206 elicitation
            Some("deny_once".to_owned()),  // 207 user question
            Some("deny_once".to_owned()),  // 208 networkApprovalContext command
            Some("deny_once".to_owned()),  // 209 grantRoot=/etc escapes workspace
            Some("deny_once".to_owned()),  // 210 nested kind.move_path=/etc/escaped.rs escapes
        ]
    );

    let transcript = read_jsonl(&log);
    let reply = |id: i64| -> Value {
        transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == id && v["msg"]["method"].is_null())
            .expect("reply logged")["msg"]["result"]
            .clone()
    };
    assert_eq!(reply(201)["decision"], "accept");
    assert_eq!(reply(202)["decision"], "decline");
    assert_eq!(reply(203)["decision"], "decline");
    assert_eq!(reply(204)["decision"], "decline");
    assert_eq!(reply(205)["permissions"], json!({}));
    assert_eq!(reply(206)["action"], "decline");
    assert_eq!(reply(207)["answers"], json!({}));
    assert_eq!(reply(208)["decision"], "decline");
    assert_eq!(reply(209)["decision"], "decline");
    assert_eq!(reply(210)["decision"], "decline");

    session.close().await.expect("close");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn readonly_safety_floor_hard_denies_file_changes_under_interactive_and_scoped() {
    for policy in [
        ApprovalPolicy::Interactive,
        ApprovalPolicy::AutoApproveScoped,
    ] {
        let dir = temp_dir("readonly");
        let log = dir.join("transcript.jsonl");
        let wrapper = make_logged_wrapper(&dir, &log);

        let driver = CodexDriver::with_binary(wrapper);
        let mut session = driver
            .start_session(base_spec(dir.clone(), AccessMode::ReadOnly, policy))
            .await
            .expect("start readonly session");
        let mut rx = session.take_events().expect("take events");

        session
            .send_turn(TurnInput {
                text: "readonly_file".to_owned(),
                effort: None,
                attachments: Vec::new(),
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        let events = drain_until_turn_completed(&mut rx).await;
        let denied_count = events
            .iter()
            .filter(|e| matches!(
                e,
                NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "deny_once"
            ))
            .count();
        assert_eq!(
            denied_count, 3,
            "ReadOnly must hard-deny fileChange (301), fileSystem.write (302), and fileSystem.entries write (304)"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            NormalizedEvent::Unknown { event_type, .. } if event_type == "vendor/unknownReverseRequest"
        )));

        let transcript = read_jsonl(&log);
        let thread_start = transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["method"] == "thread/start")
            .expect("thread/start");
        assert_eq!(thread_start["msg"]["params"]["sandbox"], "read-only");

        let reply_301 = transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == 301 && v["msg"]["method"].is_null())
            .expect("reply 301");
        assert_eq!(reply_301["msg"]["result"]["decision"], "decline");

        let reply_302 = transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == 302 && v["msg"]["method"].is_null())
            .expect("reply 302");
        assert_eq!(reply_302["msg"]["result"]["permissions"], json!({}));

        let reply_304 = transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == 304 && v["msg"]["method"].is_null())
            .expect("reply 304");
        assert_eq!(reply_304["msg"]["result"]["permissions"], json!({}));

        let reply_303 = transcript
            .iter()
            .find(|v| v["dir"] == "in" && v["msg"]["id"] == 303 && v["msg"]["method"].is_null())
            .expect("reply 303");
        assert_eq!(reply_303["msg"]["error"]["code"], -32601);

        session.close().await.expect("close");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[tokio::test]
async fn server_request_resolved_emits_none_and_rejects_late_answer_without_stdin_write() {
    let dir = temp_dir("resolved-race");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);

    let driver = CodexDriver::with_binary(wrapper);
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    session
        .send_turn(TurnInput {
            text: "server_resolved_race".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    let events = drain_until_turn_completed(&mut rx).await;
    let interaction_id = events
        .iter()
        .find_map(|e| match e {
            NormalizedEvent::InteractionRequested { request } => {
                Some(request.interaction_id.clone())
            }
            _ => None,
        })
        .expect("interaction requested");

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved {
            interaction_id: iid,
            decision_id: None,
            ..
        } if iid == &interaction_id
    )));

    // Late answer MUST fail with typed error and NOT write to stdin.
    let err = session
        .answer_interaction(InteractionAnswer {
            interaction_id,
            decision_id: "allow_once".to_owned(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect_err("late answer must fail");
    assert!(matches!(err, dolgorae_spi::DriverError::InvalidConfig(_)));

    session.close().await.expect("close");

    let transcript = read_jsonl(&log);
    let any_reply_901 = transcript
        .iter()
        .any(|v| v["dir"] == "in" && v["msg"]["id"] == 901 && v["msg"]["method"].is_null());
    assert!(
        !any_reply_901,
        "late answer must not write a reply for server-resolved request 901"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn interrupt_turn_resume_session_and_crash_emits_session_closed() {
    let dir = temp_dir("lifecycle");
    let log = dir.join("transcript.jsonl");
    let wrapper = make_logged_wrapper(&dir, &log);
    let driver = CodexDriver::with_binary(wrapper);

    // 1. Interrupt
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("take events");
    let ack = session
        .send_turn(TurnInput {
            text: "interrupt_me".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnStarted { .. }
    ));
    session
        .interrupt_turn(&ack.turn_id)
        .await
        .expect("interrupt_turn");
    let events = drain_until_turn_completed(&mut rx).await;
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::TurnCompleted {
            status: TurnTerminalStatus::Interrupted,
            ..
        }
    )));
    session.close().await.expect("close");

    // 2. Resume + unexpected crash -> Error + SessionClosed
    let mut resumed = driver
        .resume_session(ResumeSpec {
            provider_session_id: "thr-resumed-77".to_owned(),
            workspace: dir.clone(),
            role_instructions: None,
            model: None,
            effort: None,
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::AutoDeny,
            output_schema: None,
            mcp_servers: Vec::new(),
        })
        .await
        .expect("resume_session");
    assert_eq!(resumed.provider_session_id(), "thr-resumed-77");
    let mut rx2 = resumed.take_events().expect("take events");

    resumed
        .send_turn(TurnInput {
            text: "crash_now".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send crash turn");

    let mut saw_error = false;
    let mut saw_closed = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(5), rx2.recv()).await {
            Ok(Some(NormalizedEvent::Error {
                message,
                will_retry: false,
                ..
            })) => {
                assert!(message.contains("simulated upstream failure"));
                saw_error = true;
            }
            Ok(Some(NormalizedEvent::SessionClosed { reason })) => {
                assert!(reason.is_some());
                saw_closed = true;
                break;
            }
            Ok(Some(_)) => {}
            _ => break,
        }
    }
    assert!(saw_error, "expected Error event before crash");
    assert!(
        saw_closed,
        "expected SessionClosed event on unexpected crash"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn pgid_teardown_reaps_grandchild_sleeper_process() {
    let dir = temp_dir("pgid");
    let driver = CodexDriver::with_binary(fake_codex_bin());
    let mut session = driver
        .start_session(base_spec(
            dir.clone(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::AutoDeny,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    session
        .send_turn(TurnInput {
            text: "spawn_grandchild".to_owned(),
            effort: None,
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    let events = drain_until_turn_completed(&mut rx).await;
    let gc_pid: u32 = events
        .iter()
        .find_map(|e| match e {
            NormalizedEvent::AssistantMessage { text, .. } => text
                .strip_prefix("GRANDCHILD_PID=")
                .and_then(|s| s.parse::<u32>().ok()),
            _ => None,
        })
        .expect("grandchild pid emitted");

    // Verify grandchild is alive before close.
    let alive_before = StdCommand::new("/bin/kill")
        .args(["-0", &gc_pid.to_string()])
        .status()
        .expect("kill -0")
        .success();
    assert!(
        alive_before,
        "grandchild pid {gc_pid} should be alive before close"
    );

    session.close().await.expect("close session");

    // Wait briefly for kernel process teardown after PGID signal.
    let mut reaped = false;
    for _ in 0..20 {
        let still_alive = StdCommand::new("/bin/kill")
            .args(["-0", &gc_pid.to_string()])
            .status()
            .expect("kill -0")
            .success();
        if !still_alive {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        reaped,
        "grandchild pid {gc_pid} survived PGID teardown after close()"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
#[ignore = "opt-in live smoke test gated by DOLGORAE_LIVE_SMOKE=1"]
async fn live_codex_smoke_opt_in() {
    if std::env::var("DOLGORAE_LIVE_SMOKE").as_deref() != Ok("1") {
        return;
    }
    let driver = CodexDriver::new();
    let caps = driver.probe().await.expect("probe live codex");
    assert!(
        caps.available,
        "live codex unavailable: {:?}",
        caps.unavailable_reason
    );

    let dir = temp_dir("live-smoke");
    let mut session = driver
        .start_session(SessionSpec {
            workspace: dir.clone(),
            role_instructions: Some("Reply with exactly OK.".to_owned()),
            model: None,
            effort: Some("low".to_owned()),
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::AutoDeny,
            output_schema: None,
            mcp_servers: Vec::new(),
        })
        .await
        .expect("start live session");
    let mut rx = session.take_events().expect("take events");
    session
        .send_turn(TurnInput {
            text: "Reply with OK".to_owned(),
            effort: Some("low".to_owned()),
            attachments: Vec::new(),
            output_schema_override: None,
        })
        .await
        .expect("send live turn");
    let events = drain_until_turn_completed(&mut rx).await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::TurnCompleted { .. }))
    );
    session.close().await.expect("close live session");
    let _ = std::fs::remove_dir_all(&dir);
}
