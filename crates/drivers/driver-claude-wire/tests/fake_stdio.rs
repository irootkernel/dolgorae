//! Offline fake-stdio integration tests for `driver-claude-wire`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, AttachmentRef, DriverError, EventReceiver,
    InteractionAnswer, InteractionKind, McpServerInjection, NormalizedEvent, ResumeSpec,
    SessionSpec, ToolActivityStatus, TurnInput, TurnTerminalStatus,
};
use driver_claude_wire::ClaudeWireDriver;
use serde_json::{Value, json};
use uuid::Uuid;

struct TempWorkspace {
    path: PathBuf,
}

impl TempWorkspace {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("dolgorae-claude-test-{label}-{}", Uuid::now_v7()));
        fs::create_dir_all(&path).expect("create temp workspace");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write_mode(&self, mode: &Value) {
        fs::write(
            self.path.join(".fake_claude_mode.json"),
            serde_json::to_vec(mode).expect("encode mode"),
        )
        .expect("write mode");
    }

    fn read_startup(&self) -> Value {
        let raw = fs::read_to_string(self.path.join(".fake_claude_startup.json"))
            .expect("read startup report");
        serde_json::from_str(&raw).expect("parse startup report")
    }

    fn read_stdin_lines(&self) -> Vec<Value> {
        let path = self.path.join(".fake_claude_stdin.jsonl");
        let Ok(raw) = fs::read_to_string(path) else {
            return Vec::new();
        };
        raw.lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("parse stdin jsonl"))
            .collect()
    }
}

impl Drop for TempWorkspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn fake_claude_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fake_claude.py")
}

fn base_spec(ws: &Path, access: AccessMode, approval_policy: ApprovalPolicy) -> SessionSpec {
    SessionSpec {
        workspace: ws.to_path_buf(),
        role_instructions: Some("Follow repository rules.".to_owned()),
        model: Some("sonnet".to_owned()),
        effort: Some("high".to_owned()),
        access,
        approval_policy,
        output_schema: Some(json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"]
        })),
        mcp_servers: Vec::new(),
    }
}

fn simple_turn(text: &str) -> TurnInput {
    TurnInput {
        text: text.to_owned(),
        effort: None,
        attachments: Vec::new(),
        output_schema_override: None,
    }
}

async fn next_event(rx: &mut EventReceiver) -> NormalizedEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for NormalizedEvent")
        .expect("event channel closed unexpectedly")
}

async fn collect_until_turn_completed(rx: &mut EventReceiver) -> Vec<NormalizedEvent> {
    let mut events = Vec::new();
    loop {
        let ev = next_event(rx).await;
        let done = matches!(ev, NormalizedEvent::TurnCompleted { .. });
        events.push(ev);
        if done {
            return events;
        }
    }
}

fn process_alive(pid: u32) -> bool {
    Command::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn probe_succeeds_and_reports_claude_capabilities() {
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    assert_eq!(driver.driver_id(), "claude-wire");
    let caps = driver.probe().await.expect("probe fake binary");
    assert!(caps.available);
    assert_eq!(caps.driver_id, "claude-wire");
    assert_eq!(
        caps.cli_version.as_deref(),
        Some("2.1.332 (Fake Claude Code)")
    );
    assert!(caps.unavailable_reason.is_none());
    assert!(caps.native_output_schema);
    assert!(caps.supports_effort);
    assert!(caps.supports_interactive_approval);
    assert!(caps.supports_mcp_injection);
    assert!(caps.supports_resume);
}

#[tokio::test]
async fn launch_flags_zero_secrets_in_argv_and_cleanup_on_close_and_drop() {
    let secret_token = "tok-super-secret-actor-987654321";
    let mut env = BTreeMap::new();
    env.insert("DOLGORAE_ACTOR_TOKEN".to_owned(), secret_token.to_owned());

    // Part 1: Verify flags, 0600/0700 perms, secret absent from argv, and cleanup on close().
    let ws1 = TempWorkspace::new("secret-close");
    let mut spec1 = base_spec(
        ws1.path(),
        AccessMode::WorkspaceWrite,
        ApprovalPolicy::Interactive,
    );
    spec1.mcp_servers.push(McpServerInjection {
        name: "dolgorae".to_owned(),
        command: PathBuf::from("/usr/local/bin/dolgorae"),
        args: vec!["mcp".to_owned(), "--scoped".to_owned()],
        env: env.clone(),
    });

    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let session = driver.start_session(spec1).await.expect("start session");
    let startup = ws1.read_startup();
    let argv: Vec<String> = startup["argv"]
        .as_array()
        .expect("argv array")
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();

    assert!(argv.iter().any(|a| a == "--verbose"));
    assert!(argv.iter().any(|a| a == "--strict-mcp-config"));
    assert!(
        argv.windows(2)
            .any(|w| w == ["--permission-prompts", "host"])
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--permission-mode", "manual"])
    );
    assert!(
        argv.windows(2)
            .any(|w| w == ["--session-id", session.provider_session_id()]),
        "start_session must pass --session-id matching provider_session_id(): {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--resume"),
        "start_session must not pass --resume"
    );
    assert!(
        !argv.iter().any(|a| a == "--bare"),
        "--bare must never be passed"
    );
    assert!(
        !argv
            .iter()
            .any(|a| a.contains(secret_token) || a.contains("DOLGORAE_ACTOR_TOKEN")),
        "secret token leaked into argv: {argv:?}"
    );

    assert_eq!(startup["mcp_file_mode"].as_u64(), Some(0o600));
    assert_eq!(startup["mcp_dir_mode"].as_u64(), Some(0o700));
    assert_eq!(
        startup["mcp_config_json"]["mcpServers"]["dolgorae"]["env"]["DOLGORAE_ACTOR_TOKEN"]
            .as_str(),
        Some(secret_token)
    );

    let mcp_file = PathBuf::from(startup["mcp_config_path"].as_str().unwrap());
    let mcp_dir = PathBuf::from(startup["mcp_dir_path"].as_str().unwrap());
    assert!(
        mcp_file.exists(),
        "mcp config file should exist while running"
    );
    assert!(
        mcp_dir.exists(),
        "mcp config dir should exist while running"
    );

    session.close().await.expect("close session");
    assert!(
        !mcp_file.exists(),
        "mcp config file must be deleted on close()"
    );
    assert!(
        !mcp_dir.exists(),
        "mcp config dir must be deleted on close()"
    );

    // Part 2: Verify cleanup on Drop (without calling close()) and non-blocking Drop.
    let ws2 = TempWorkspace::new("secret-drop");
    let mut spec2 = base_spec(
        ws2.path(),
        AccessMode::WorkspaceWrite,
        ApprovalPolicy::Interactive,
    );
    spec2.mcp_servers.push(McpServerInjection {
        name: "dolgorae".to_owned(),
        command: PathBuf::from("/usr/local/bin/dolgorae"),
        args: vec!["mcp".to_owned(), "--scoped".to_owned()],
        env,
    });

    let session2 = driver.start_session(spec2).await.expect("start session2");
    let startup2 = ws2.read_startup();
    assert_eq!(startup2["mcp_file_mode"].as_u64(), Some(0o600));
    assert_eq!(startup2["mcp_dir_mode"].as_u64(), Some(0o700));
    let mcp_file2 = PathBuf::from(startup2["mcp_config_path"].as_str().unwrap());
    let mcp_dir2 = PathBuf::from(startup2["mcp_dir_path"].as_str().unwrap());
    assert!(mcp_file2.exists());

    let drop_start = Instant::now();
    drop(session2);
    assert!(
        drop_start.elapsed() < Duration::from_millis(200),
        "Drop must not block or sleep"
    );
    assert!(
        !mcp_file2.exists(),
        "mcp config file must be deleted on Drop"
    );
    assert!(!mcp_dir2.exists(), "mcp config dir must be deleted on Drop");
}

#[tokio::test]
async fn streaming_deltas_tool_activity_structured_output_usage_unknown_and_init_on_first_turn() {
    let ws = TempWorkspace::new("stream-full");
    ws.write_mode(&json!({
        "init_mode": "on_first_turn",
        "session_id": "sess-late-init-77"
    }));

    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start session");
    let mut rx = session.take_events().expect("take events");

    let mut turn_input = simple_turn("SCENARIO:stream_full");
    turn_input.attachments.push(AttachmentRef {
        path: ws.path().join("diff.patch"),
        media_type: Some("text/x-diff".to_owned()),
    });
    let ack = session.send_turn(turn_input).await.expect("send turn");
    assert!(!ack.turn_id.is_empty());

    let events = collect_until_turn_completed(&mut rx).await;

    assert!(matches!(
        events.first(),
        Some(NormalizedEvent::TurnStarted { turn_id }) if turn_id == &ack.turn_id
    ));

    let deltas: String = events
        .iter()
        .filter_map(|e| match e {
            NormalizedEvent::TextDelta { turn_id, delta, .. } => {
                assert_eq!(turn_id, &ack.turn_id);
                Some(delta.as_str())
            }
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "Hello, world!");

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::ToolActivity {
            turn_id,
            item_id,
            tool_name,
            status: ToolActivityStatus::Started,
            command: Some(cmd),
            ..
        } if turn_id == &ack.turn_id && item_id == "toolu_bash_1" && tool_name == "Bash" && cmd == "cargo test"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::ToolActivity {
            turn_id,
            item_id,
            tool_name,
            status: ToolActivityStatus::Completed,
            ..
        } if turn_id == &ack.turn_id && item_id == "toolu_bash_1" && tool_name == "Bash"
    )));
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::ToolActivity {
            turn_id,
            item_id,
            tool_name,
            status: ToolActivityStatus::Failed,
            ..
        } if turn_id == &ack.turn_id && item_id == "toolu_edit_1" && tool_name == "Edit"
    )));

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::Unknown { event_type, .. } if event_type == "vendor_future_event"
    )));

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::AssistantMessage {
            turn_id,
            text,
            structured_output: Some(so),
            ..
        } if turn_id == &ack.turn_id && text == "Hello, world!" && so == &json!({"ok": true, "score": 99})
    )));

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::Usage {
            turn_id: Some(tid),
            metrics,
        } if tid == &ack.turn_id
            && metrics.input_tokens == Some(120)
            && metrics.output_tokens == Some(45)
            && metrics.cached_input_tokens == Some(15)
            && metrics.total_cost_usd == Some(0.0042)
    )));

    assert!(matches!(
        events.last(),
        Some(NormalizedEvent::TurnCompleted {
            turn_id,
            status: TurnTerminalStatus::Completed,
            error: None,
        }) if turn_id == &ack.turn_id
    ));

    // Verify second turn uses updated session_id from first turn's system/init.
    let _ack2 = session
        .send_turn(simple_turn("second turn"))
        .await
        .expect("send turn 2");
    let _ = collect_until_turn_completed(&mut rx).await;
    let stdin_lines = ws.read_stdin_lines();
    let user_turns: Vec<&Value> = stdin_lines
        .iter()
        .filter(|v| v.get("type").and_then(Value::as_str) == Some("user"))
        .collect();
    assert_eq!(user_turns.len(), 2);
    assert_eq!(
        user_turns[1]["session_id"].as_str(),
        Some("sess-late-init-77")
    );

    session.close().await.expect("close");
}

#[tokio::test]
async fn interactive_allow_echoes_updated_input_and_deny_sends_deny() {
    let ws = TempWorkspace::new("interactive");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    // 1. Interactive Allow -> must echo updatedInput verbatim.
    let orig_input = json!({"command": "echo safe", "cwd": ws.path().to_string_lossy()});
    let ack = session
        .send_turn(simple_turn(&format!(
            "SCENARIO:can_use_tool:Bash:{orig_input}"
        )))
        .await
        .expect("send allow turn");

    let req = loop {
        if let NormalizedEvent::InteractionRequested { request } = next_event(&mut rx).await {
            break request;
        }
    };
    assert_eq!(req.turn_id, ack.turn_id);
    assert!(matches!(req.kind, InteractionKind::CommandApproval { .. }));
    let dec_ids: Vec<&str> = req
        .decisions
        .iter()
        .map(|d| d.decision_id.as_str())
        .collect();
    assert_eq!(
        dec_ids,
        vec!["allow_once", "deny_once"],
        "decisions must expose honest allow_once + deny_once only"
    );

    session
        .answer_interaction(InteractionAnswer {
            interaction_id: req.interaction_id.clone(),
            decision_id: "allow_once".to_owned(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect("allow interaction");

    let events = collect_until_turn_completed(&mut rx).await;
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved {
            interaction_id,
            decision_id: Some(dec),
            ..
        } if interaction_id == &req.interaction_id && dec == "allow_once"
    )));

    let perm_resp: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json"))
            .expect("read perm resp"),
    )
    .expect("parse perm resp");
    assert_eq!(
        perm_resp["response"]["response"]["behavior"].as_str(),
        Some("allow")
    );
    assert_eq!(
        perm_resp["response"]["response"]["updatedInput"], orig_input,
        "allow control_response MUST echo updatedInput verbatim"
    );

    // 2. Interactive Deny -> sends deny with interrupt: false.
    let edit_input = json!({"file_path": "src/main.rs", "old_string": "a", "new_string": "b"});
    let _ack2 = session
        .send_turn(simple_turn(&format!(
            "SCENARIO:can_use_tool:Edit:{edit_input}"
        )))
        .await
        .expect("send deny turn");
    let req2 = loop {
        if let NormalizedEvent::InteractionRequested { request } = next_event(&mut rx).await {
            break request;
        }
    };
    assert!(matches!(
        req2.kind,
        InteractionKind::FileChangeApproval { .. }
    ));

    session
        .answer_interaction(InteractionAnswer {
            interaction_id: req2.interaction_id.clone(),
            decision_id: "deny_once".to_owned(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect("deny interaction");

    let events2 = collect_until_turn_completed(&mut rx).await;
    assert!(events2.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved {
            interaction_id,
            decision_id: Some(dec),
            ..
        } if interaction_id == &req2.interaction_id && dec == "deny_once"
    )));
    let perm_resp2: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json"))
            .expect("read perm resp 2"),
    )
    .expect("parse perm resp 2");
    assert_eq!(
        perm_resp2["response"]["response"]["behavior"].as_str(),
        Some("deny")
    );
    assert_eq!(
        perm_resp2["response"]["response"]["interrupt"].as_bool(),
        Some(false)
    );

    session.close().await.expect("close");
}

#[tokio::test]
async fn auto_deny_policy_automatically_denies_with_audit_events() {
    let ws = TempWorkspace::new("auto-deny");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::AutoDeny,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    let input = json!({"file_path": "src/lib.rs"});
    let _ack = session
        .send_turn(simple_turn(&format!("SCENARIO:can_use_tool:Edit:{input}")))
        .await
        .expect("send");
    let events = collect_until_turn_completed(&mut rx).await;

    assert!(
        events
            .iter()
            .any(|e| matches!(e, NormalizedEvent::InteractionRequested { .. }))
    );
    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved {
            decision_id: Some(dec),
            ..
        } if dec == "deny_once"
    )));

    let perm_resp: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json"))
            .expect("read perm resp"),
    )
    .expect("parse");
    assert_eq!(
        perm_resp["response"]["response"]["behavior"].as_str(),
        Some("deny")
    );

    session.close().await.expect("close");
}

#[tokio::test]
async fn auto_approve_scoped_allows_inside_edit_denies_outside_edit_and_denies_unsandboxed_bash() {
    let ws = TempWorkspace::new("scoped");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::AutoApproveScoped,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    // 1. Inside-workspace Edit -> ALLOWED (with updatedInput echoed).
    let inside = json!({"file_path": ws.path().join("src/app.rs").to_string_lossy()});
    session
        .send_turn(simple_turn(&format!("SCENARIO:can_use_tool:Edit:{inside}")))
        .await
        .expect("send inside edit");
    let ev1 = collect_until_turn_completed(&mut rx).await;
    assert!(ev1.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "allow_once"
    )));
    let resp1: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resp1["response"]["response"]["behavior"].as_str(),
        Some("allow")
    );
    assert_eq!(resp1["response"]["response"]["updatedInput"], inside);

    // 2. Outside-workspace Edit -> DENIED.
    let outside = json!({"file_path": "/etc/passwd"});
    session
        .send_turn(simple_turn(&format!(
            "SCENARIO:can_use_tool:Edit:{outside}"
        )))
        .await
        .expect("send outside edit");
    let ev2 = collect_until_turn_completed(&mut rx).await;
    assert!(ev2.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "deny_once"
    )));
    let resp2: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resp2["response"]["response"]["behavior"].as_str(),
        Some("deny")
    );

    // 3. Bash under WorkspaceWrite + AutoApproveScoped -> DENIED because os_sandboxed = false!
    let bash = json!({"command": "rm -rf /", "cwd": ws.path().to_string_lossy()});
    session
        .send_turn(simple_turn(&format!("SCENARIO:can_use_tool:Bash:{bash}")))
        .await
        .expect("send bash");
    let ev3 = collect_until_turn_completed(&mut rx).await;
    assert!(ev3.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "deny_once"
    )));
    let resp3: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        resp3["response"]["response"]["behavior"].as_str(),
        Some("deny")
    );

    session.close().await.expect("close");
}

#[tokio::test]
async fn readonly_safety_floor_disallows_write_tools_hard_denies_and_completes_assistant_message() {
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());

    for policy in [
        ApprovalPolicy::Interactive,
        ApprovalPolicy::AutoApproveScoped,
    ] {
        let ws = TempWorkspace::new("readonly");
        let mut session = driver
            .start_session(base_spec(ws.path(), AccessMode::ReadOnly, policy))
            .await
            .expect("start readonly");
        let mut rx = session.take_events().expect("events");

        // Verify --disallowedTools and --permission-mode manual (NOT plan).
        let startup = ws.read_startup();
        let argv: Vec<String> = startup["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect();
        assert!(
            argv.windows(2)
                .any(|w| w == ["--permission-mode", "manual"])
        );
        assert!(!argv.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(argv.iter().any(|a| a == "--disallowedTools"));
        for tool in ["Edit", "Write", "NotebookEdit", "MultiEdit"] {
            assert!(
                argv.iter().any(|a| a == tool),
                "missing disallowed tool {tool}"
            );
        }

        // Even if Edit/Write can_use_tool arrives, ReadOnly hard-denies it without asking!
        let write_input = json!({"file_path": ws.path().join("README.md").to_string_lossy()});
        session
            .send_turn(simple_turn(&format!(
                "SCENARIO:can_use_tool:Write:{write_input}"
            )))
            .await
            .expect("send write");
        let ev_deny = collect_until_turn_completed(&mut rx).await;
        assert!(ev_deny.iter().any(|e| matches!(
            e,
            NormalizedEvent::InteractionResolved { decision_id: Some(d), .. } if d == "deny_once"
        )));
        let resp: Value = serde_json::from_str(
            &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            resp["response"]["response"]["behavior"].as_str(),
            Some("deny")
        );

        // ReadOnly review turn still completes cleanly with AssistantMessage.
        let ack = session
            .send_turn(simple_turn("SCENARIO:readonly_review"))
            .await
            .expect("send review turn");
        let ev_review = collect_until_turn_completed(&mut rx).await;
        assert!(ev_review.iter().any(|e| matches!(
            e,
            NormalizedEvent::AssistantMessage { turn_id, text, .. }
            if turn_id == &ack.turn_id && text == "Review complete: LGTM"
        )));

        session.close().await.expect("close");
    }
}

#[tokio::test]
async fn control_cancel_request_resolves_none_and_late_answer_errors_without_writing_stdin() {
    let ws = TempWorkspace::new("cancel-req");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    session
        .send_turn(simple_turn("SCENARIO:cancel_request"))
        .await
        .expect("send");
    let events = collect_until_turn_completed(&mut rx).await;

    let interaction_id = events
        .iter()
        .find_map(|e| match e {
            NormalizedEvent::InteractionRequested { request } => {
                Some(request.interaction_id.clone())
            }
            _ => None,
        })
        .expect("expected InteractionRequested");

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::InteractionResolved {
            interaction_id: id,
            decision_id: None,
            ..
        } if id == &interaction_id
    )));

    let before_count = ws.read_stdin_lines().len();
    let err = session
        .answer_interaction(InteractionAnswer {
            interaction_id,
            decision_id: "allow_once".to_owned(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect_err("late answer must fail");
    assert!(
        matches!(err, DriverError::InvalidConfig(_)),
        "expected DriverError::InvalidConfig, got {err:?}"
    );

    tokio::time::sleep(Duration::from_millis(50)).await;
    let after_lines = ws.read_stdin_lines();
    assert_eq!(
        after_lines.len(),
        before_count,
        "late answer must NOT write anything to provider stdin"
    );

    session.close().await.expect("close");
}

#[tokio::test]
async fn interrupt_turn_resume_session_and_unexpected_crash_emit_expected_events() {
    // 1. Start fresh session, capture its --session-id UUID, then resume with that exact UUID.
    let ws_start = TempWorkspace::new("start-then-resume");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let first = driver
        .start_session(base_spec(
            ws_start.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start first session");
    let minted_sid = first.provider_session_id().to_owned();
    assert!(!minted_sid.is_empty());
    let start_argv: Vec<String> = ws_start.read_startup()["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(
        start_argv
            .windows(2)
            .any(|w| w == ["--session-id", minted_sid.as_str()]),
        "start_session must pass --session-id {minted_sid}: {start_argv:?}"
    );
    first.close().await.expect("close first session");

    let ws = TempWorkspace::new("interrupt-resume");
    let mut session = driver
        .resume_session(ResumeSpec {
            provider_session_id: minted_sid.clone(),
            workspace: ws.path().to_path_buf(),
            role_instructions: None,
            model: None,
            effort: None,
            access: AccessMode::WorkspaceWrite,
            approval_policy: ApprovalPolicy::Interactive,
            output_schema: None,
            mcp_servers: Vec::new(),
        })
        .await
        .expect("resume session");
    assert_eq!(session.provider_session_id(), minted_sid);

    let startup = ws.read_startup();
    let argv: Vec<String> = startup["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert!(
        argv.windows(2)
            .any(|w| w == ["--resume", minted_sid.as_str()]),
        "missing --resume {minted_sid} flag in {argv:?}"
    );
    assert!(
        !argv.iter().any(|a| a == "--session-id"),
        "resume_session must not pass --session-id"
    );

    let mut rx = session.take_events().expect("events");
    let ack = session
        .send_turn(simple_turn("SCENARIO:interrupt_wait"))
        .await
        .expect("send interrupt turn");

    // Wait for first delta before interrupting.
    loop {
        if matches!(next_event(&mut rx).await, NormalizedEvent::TextDelta { .. }) {
            break;
        }
    }
    session
        .interrupt_turn(&ack.turn_id)
        .await
        .expect("interrupt_turn");

    let events = collect_until_turn_completed(&mut rx).await;
    assert!(matches!(
        events.last(),
        Some(NormalizedEvent::TurnCompleted {
            turn_id,
            status: TurnTerminalStatus::Interrupted,
            ..
        }) if turn_id == &ack.turn_id
    ));
    session.close().await.expect("close");

    // 2. Unexpected crash -> emits SessionClosed.
    let ws_crash = TempWorkspace::new("crash");
    let mut crash_session = driver
        .start_session(base_spec(
            ws_crash.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start crash session");
    let mut rx_crash = crash_session.take_events().expect("events");
    crash_session
        .send_turn(simple_turn("SCENARIO:crash_mid_turn"))
        .await
        .expect("send crash turn");

    let closed_reason = loop {
        if let NormalizedEvent::SessionClosed { reason } = next_event(&mut rx_crash).await {
            break reason;
        }
    };
    assert!(closed_reason.is_some(), "expected non-empty crash reason");
}

#[tokio::test]
async fn pgid_teardown_reaps_grandchild_sleeper_process() {
    let ws = TempWorkspace::new("pgid-reap");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    session
        .send_turn(simple_turn("SCENARIO:spawn_grandchild"))
        .await
        .expect("spawn grandchild");
    let _ = collect_until_turn_completed(&mut rx).await;

    let gc_pid: u32 = fs::read_to_string(ws.path().join(".fake_claude_grandchild.pid"))
        .expect("read grandchild pid")
        .trim()
        .parse()
        .expect("parse pid");

    assert!(
        process_alive(gc_pid),
        "grandchild sleeper should be alive before close()"
    );

    session.close().await.expect("close session");

    // Give the OS a brief moment to finalize process-group signal delivery.
    let mut reaped = false;
    for _ in 0..20 {
        if !process_alive(gc_pid) {
            reaped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(
        reaped,
        "grandchild sleeper PID {gc_pid} survived PGID teardown"
    );
}

#[tokio::test]
async fn unknown_control_request_replies_with_error_response_and_emits_unknown() {
    let ws = TempWorkspace::new("unknown-ctrl");
    let driver = ClaudeWireDriver::with_binary(fake_claude_bin());
    let mut session = driver
        .start_session(base_spec(
            ws.path(),
            AccessMode::WorkspaceWrite,
            ApprovalPolicy::Interactive,
        ))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    session
        .send_turn(simple_turn("SCENARIO:unknown_control_request"))
        .await
        .expect("send");
    let events = collect_until_turn_completed(&mut rx).await;

    assert!(events.iter().any(|e| matches!(
        e,
        NormalizedEvent::Unknown { event_type, .. } if event_type == "control_request"
    )));

    let resp: Value = serde_json::from_str(
        &fs::read_to_string(ws.path().join(".fake_claude_last_perm_response.json"))
            .expect("read control_response"),
    )
    .expect("parse control_response");
    assert_eq!(resp["type"].as_str(), Some("control_response"));
    assert_eq!(resp["response"]["subtype"].as_str(), Some("error"));
    assert_eq!(
        resp["response"]["request_id"].as_str(),
        Some("native-unk-1")
    );
    assert_eq!(
        resp["response"]["error"].as_str(),
        Some("unsupported control_request subtype: future_hook_probe")
    );

    session.close().await.expect("close");
}

#[tokio::test]
#[ignore = "opt-in live Claude Code smoke test; set DOLGORAE_LIVE_SMOKE=1 to run"]
async fn live_claude_smoke_opt_in() {
    if std::env::var("DOLGORAE_LIVE_SMOKE").as_deref() != Ok("1") {
        return;
    }
    let driver = ClaudeWireDriver::new();
    let caps = driver.probe().await.expect("probe live claude");
    assert!(
        caps.available,
        "live claude unavailable: {:?}",
        caps.unavailable_reason
    );
}
