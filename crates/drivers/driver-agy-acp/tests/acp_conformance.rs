//! Offline fake-stdio conformance and lifecycle tests for `driver-agy-acp`.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::Duration;

use dolgorae_spi::{
    AccessMode, AgentDriver, ApprovalPolicy, AttachmentRef, DecisionKind, DriverError,
    EventReceiver, InteractionAnswer, InteractionKind, McpServerInjection, NormalizedEvent,
    ResumeSpec, SessionSpec, ToolActivityStatus, TurnInput, TurnTerminalStatus,
};
use driver_agy_acp::{AgyAcpDriver, AgyDriver};
use serde_json::{Value, json};
use uuid::Uuid;

struct TestHarness {
    dir: PathBuf,
    wrapper: PathBuf,
    log_path: PathBuf,
    workspace: PathBuf,
}

impl TestHarness {
    fn new(scenario: &str, extra_env: &[(&str, &str)]) -> Self {
        let dir = std::env::temp_dir().join(format!("dolgorae-agy-test-{}", Uuid::now_v7()));
        let workspace = dir.join("workspace");
        fs::create_dir_all(&workspace).expect("create test workspace");

        let log_path = dir.join("wire.jsonl");
        let script_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fake_agy_acp.py");

        let mut env_lines = String::new();
        env_lines.push_str(&format!(
            "export FAKE_AGY_SCENARIO='{}'\n",
            shell_escape(scenario)
        ));
        env_lines.push_str(&format!(
            "export FAKE_AGY_LOG='{}'\n",
            shell_escape(&log_path.to_string_lossy())
        ));
        env_lines.push_str(&format!(
            "export FAKE_AGY_WORKSPACE='{}'\n",
            shell_escape(&workspace.to_string_lossy())
        ));
        for (k, v) in extra_env {
            env_lines.push_str(&format!("export {k}='{}'\n", shell_escape(v)));
        }

        let wrapper = dir.join("fake-agy.sh");
        let wrapper_body = format!(
            "#!/bin/sh\n{env_lines}exec python3 '{}' \"$@\"\n",
            shell_escape(&script_path.to_string_lossy())
        );
        fs::write(&wrapper, wrapper_body).expect("write wrapper script");
        fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755))
            .expect("chmod wrapper script");

        Self {
            dir,
            wrapper,
            log_path,
            workspace,
        }
    }

    fn driver(&self) -> AgyDriver {
        AgyDriver::with_binary(self.wrapper.clone())
    }

    fn read_log(&self) -> Vec<Value> {
        let Ok(content) = fs::read_to_string(&self.log_path) else {
            return Vec::new();
        };
        content
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).expect("valid log json"))
            .collect()
    }
}

impl Drop for TestHarness {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn shell_escape(s: &str) -> String {
    s.replace('\'', "'\\''")
}

async fn next_event(rx: &mut EventReceiver) -> NormalizedEvent {
    tokio::time::timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("timed out waiting for normalized event")
        .expect("event channel closed unexpectedly")
}

fn base_spec(workspace: &Path) -> SessionSpec {
    SessionSpec {
        workspace: workspace.to_path_buf(),
        role_instructions: None,
        model: None,
        effort: None,
        access: AccessMode::WorkspaceWrite,
        approval_policy: ApprovalPolicy::Interactive,
        output_schema: None,
        mcp_servers: Vec::new(),
    }
}

#[tokio::test]
async fn probe_available_missing_and_failing_binaries() {
    let h = TestHarness::new("basic", &[]);
    let caps = h.driver().probe().await.expect("probe ok");
    assert!(caps.available);
    assert_eq!(caps.cli_version.as_deref(), Some("agy 1.5.18"));
    assert!(!caps.native_output_schema);
    assert!(caps.supports_effort);
    assert!(caps.supports_interactive_approval);
    assert!(caps.supports_mcp_injection);
    assert!(caps.supports_resume);
    assert_eq!(caps.driver_id, "agy-acp");

    // Missing binary
    let missing = AgyAcpDriver::with_binary(PathBuf::from("/nonexistent/dolgorae-no-such-agy"));
    let missing_caps = missing.probe().await.expect("probe missing returns Ok");
    assert!(!missing_caps.available);
    assert!(missing_caps.unavailable_reason.is_some());

    // Failing binary
    let h_fail = TestHarness::new("basic", &[("FAKE_AGY_PROBE_FAIL", "1")]);
    let fail_caps = h_fail
        .driver()
        .probe()
        .await
        .expect("probe fail returns Ok");
    assert!(!fail_caps.available);
    assert!(
        fail_caps
            .unavailable_reason
            .as_deref()
            .is_some_and(|r| r.contains("broken agy binary"))
    );
}

#[tokio::test]
async fn interleaved_updates_mcp_env_array_zero_argv_secrets_and_schema_prompt() {
    let h = TestHarness::new("basic", &[]);
    let mut env = BTreeMap::new();
    env.insert(
        "DOLGORAE_ACTOR_TOKEN".to_owned(),
        "secret-agy-token-42".to_owned(),
    );
    env.insert("CUSTOM_KEY".to_owned(), "custom-val".to_owned());

    let mut spec = base_spec(&h.workspace);
    spec.role_instructions = Some("You are a meticulous security reviewer.".to_owned());
    spec.model = Some("gemini-pro-test".to_owned());
    spec.effort = Some("high".to_owned());
    spec.output_schema = Some(json!({
        "type": "object",
        "required": ["verdict"],
        "properties": {"verdict": {"type": "string"}}
    }));
    spec.mcp_servers.push(McpServerInjection {
        name: "dolgorae".to_owned(),
        command: PathBuf::from("/usr/local/bin/dolgorae"),
        args: vec!["mcp".to_owned(), "--scoped".to_owned()],
        env,
    });

    let mut session = h.driver().start_session(spec).await.expect("start_session");
    assert_eq!(session.provider_session_id(), "acp-sess-001");
    let mut rx = session.take_events().expect("take events");

    // Verify interleaved session/update notifications emitted BEFORE session/new response
    // were routed without deadlocking!
    let ev1 = next_event(&mut rx).await;
    assert!(matches!(
        ev1,
        NormalizedEvent::Unknown { ref event_type, .. }
            if event_type == "acp.session_update.available_commands_update"
    ));
    let ev2 = next_event(&mut rx).await;
    assert!(matches!(
        ev2,
        NormalizedEvent::Usage { turn_id: None, ref metrics }
            if metrics.input_tokens == Some(5)
    ));

    // Send a turn with output_schema_override + attachment and verify full streaming lifecycle.
    let ack = session
        .send_turn(TurnInput {
            text: "Review diff".to_owned(),
            effort: None,
            attachments: vec![AttachmentRef {
                path: h.workspace.join("diff.patch"),
                media_type: Some("text/x-diff".to_owned()),
            }],
            output_schema_override: Some(
                json!({"type": "object", "properties": {"ok": {"type": "boolean"}}}),
            ),
        })
        .await
        .expect("send_turn");

    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnStarted { ref turn_id } if turn_id == &ack.turn_id
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TextDelta { ref turn_id, ref delta, .. }
            if turn_id == &ack.turn_id && delta == "Hello, "
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::ToolActivity {
            ref turn_id,
            ref item_id,
            status: ToolActivityStatus::Started,
            ref command,
            ref paths,
            ..
        } if turn_id == &ack.turn_id
            && item_id == "tc-1"
            && command.as_deref() == Some("cargo test")
            && paths == &vec!["src/lib.rs".to_owned()]
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::ToolActivity {
            ref turn_id,
            status: ToolActivityStatus::Completed,
            exit_status: Some(0),
            ..
        } if turn_id == &ack.turn_id
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TextDelta { ref delta, .. } if delta == "world!"
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::Usage { turn_id: Some(ref tid), ref metrics }
            if tid == &ack.turn_id
                && metrics.input_tokens == Some(120)
                && metrics.output_tokens == Some(45)
                && metrics.cached_input_tokens == Some(10)
                && metrics.reasoning_tokens == Some(5)
                && metrics.total_cost_usd == Some(0.0042)
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::AssistantMessage {
            ref turn_id,
            ref text,
            structured_output: None,
            ..
        } if turn_id == &ack.turn_id && text == "Hello, world!"
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnCompleted {
            ref turn_id,
            status: TurnTerminalStatus::Completed,
            error: None,
        } if turn_id == &ack.turn_id
    ));

    session.close().await.expect("close");

    // Inspect recorded wire log for security & protocol invariants.
    let log = h.read_log();
    let argv_rec = log
        .iter()
        .find(|r| r["event"] == "argv")
        .expect("argv logged");
    let argv_strs: Vec<&str> = argv_rec["argv"]
        .as_array()
        .expect("argv array")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(argv_strs, vec!["acp", "--stdio", "--process-per-session"]);
    for arg in &argv_strs {
        assert!(!arg.contains("DOLGORAE_ACTOR_TOKEN"));
        assert!(!arg.contains("secret-agy-token-42"));
    }

    let session_new = log
        .iter()
        .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/new")
        .expect("session/new logged");
    let params = &session_new["msg"]["params"];
    let env_val = &params["mcpServers"][0]["env"];
    assert!(
        env_val.is_array(),
        "ACP mcpServers[].env MUST be a JSON array, got: {env_val}"
    );
    assert_eq!(
        env_val,
        &json!([
            {"name": "CUSTOM_KEY", "value": "custom-val"},
            {"name": "DOLGORAE_ACTOR_TOKEN", "value": "secret-agy-token-42"}
        ])
    );

    let sys_append = params["_meta"]["antigravity"]["systemPrompt"]["append"]
        .as_str()
        .expect("systemPrompt.append present");
    assert!(sys_append.contains("meticulous security reviewer"));
    assert!(sys_append.contains("verdict"));
    assert!(sys_append.contains("JSON Schema"));

    let set_model = log
        .iter()
        .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/set_model")
        .expect("session/set_model logged");
    assert_eq!(set_model["msg"]["params"]["modelId"], "gemini-pro-test");

    let set_effort = log
        .iter()
        .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/set_config_option")
        .expect("session/set_config_option logged");
    assert_eq!(set_effort["msg"]["params"]["configId"], "thought_level");
    assert_eq!(set_effort["msg"]["params"]["value"], "high");

    let prompt_req = log
        .iter()
        .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/prompt")
        .expect("session/prompt logged");
    let prompt_text = prompt_req["msg"]["params"]["prompt"][0]["text"]
        .as_str()
        .expect("prompt text");
    assert!(prompt_text.contains("Review diff"));
    assert!(prompt_text.contains("\"ok\""));
    assert!(prompt_text.contains("diff.patch"));
}

#[tokio::test]
async fn interactive_allow_and_deny_use_opaque_ids_and_map_back_privately() {
    let h = TestHarness::new("interactive_permissions", &[]);
    let mut session = h
        .driver()
        .start_session(base_spec(&h.workspace))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");

    // Drain startup interleaved notifications.
    let _ = next_event(&mut rx).await;
    let _ = next_event(&mut rx).await;

    let _ack = session
        .send_turn(TurnInput {
            text: "do work".to_owned(),
            effort: None,
            attachments: vec![],
            output_schema_override: None,
        })
        .await
        .expect("send_turn");
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnStarted { .. }
    ));

    // 1st permission: CommandApproval -> Allow
    let req1 = match next_event(&mut rx).await {
        NormalizedEvent::InteractionRequested { request } => request,
        other => panic!("expected InteractionRequested, got {other:?}"),
    };
    assert!(matches!(req1.kind, InteractionKind::CommandApproval { .. }));
    assert!(
        req1.decisions
            .iter()
            .all(|d| !d.decision_id.contains("acp_opt_"))
    );
    let allow_id = req1
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::AllowOnce)
        .expect("allow_once option")
        .decision_id
        .clone();

    session
        .answer_interaction(InteractionAnswer {
            interaction_id: req1.interaction_id.clone(),
            decision_id: allow_id.clone(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect("answer allow");
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::InteractionResolved { ref interaction_id, decision_id: Some(ref d), .. }
            if interaction_id == &req1.interaction_id && d == &allow_id
    ));

    // 2nd permission: FileChangeApproval -> Deny
    let req2 = match next_event(&mut rx).await {
        NormalizedEvent::InteractionRequested { request } => request,
        other => panic!("expected second InteractionRequested, got {other:?}"),
    };
    assert!(matches!(
        req2.kind,
        InteractionKind::FileChangeApproval { .. }
    ));
    let deny_id = req2
        .decisions
        .iter()
        .find(|d| d.kind == DecisionKind::DenyOnce)
        .expect("deny_once option")
        .decision_id
        .clone();

    session
        .answer_interaction(InteractionAnswer {
            interaction_id: req2.interaction_id.clone(),
            decision_id: deny_id.clone(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect("answer deny");
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::InteractionResolved { ref interaction_id, decision_id: Some(ref d), .. }
            if interaction_id == &req2.interaction_id && d == &deny_id
    ));

    // Finish turn
    loop {
        if matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnCompleted {
                status: TurnTerminalStatus::Completed,
                ..
            }
        ) {
            break;
        }
    }

    session.close().await.expect("close");

    let log = h.read_log();
    let perm_replies: Vec<&Value> = log.iter().filter(|r| r["event"] == "perm_reply").collect();
    assert_eq!(perm_replies.len(), 2);
    assert_eq!(
        perm_replies[0]["resp"]["result"]["outcome"],
        json!({"outcome": "selected", "optionId": "acp_opt_allow_once_77"})
    );
    assert_eq!(
        perm_replies[1]["resp"]["result"]["outcome"],
        json!({"outcome": "selected", "optionId": "acp_opt_reject_88"})
    );
}

#[tokio::test]
async fn auto_deny_and_auto_approve_scoped_enforce_unsandboxed_command_and_path_bounds() {
    // AutoDeny: all 4 requests denied automatically.
    {
        let h = TestHarness::new("policy_matrix", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.approval_policy = ApprovalPolicy::AutoDeny;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "run".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        let mut req_count = 0usize;
        let mut res_count = 0usize;
        loop {
            match next_event(&mut rx).await {
                NormalizedEvent::InteractionRequested { .. } => req_count += 1,
                NormalizedEvent::InteractionResolved { .. } => res_count += 1,
                NormalizedEvent::TurnCompleted { status, .. } => {
                    assert_eq!(status, TurnTerminalStatus::Completed);
                    break;
                }
                _ => {}
            }
        }
        assert_eq!(req_count, 4);
        assert_eq!(res_count, 4);
        session.close().await.expect("close");

        let replies: BTreeMap<String, Value> = h
            .read_log()
            .into_iter()
            .filter(|r| r["event"] == "perm_reply")
            .map(|r| {
                (
                    r["tag"].as_str().unwrap().to_owned(),
                    r["resp"]["result"]["outcome"].clone(),
                )
            })
            .collect();
        for tag in ["edit_in", "edit_out", "exec_in", "perm_other"] {
            assert_eq!(
                replies[tag],
                json!({"outcome": "selected", "optionId": format!("reject_{tag}")})
            );
        }
    }

    // AutoApproveScoped + WorkspaceWrite:
    // inside-workspace edit ALLOWED; outside-workspace edit DENIED;
    // command DENIED (os_sandboxed = false); fetch/other DENIED.
    {
        let h = TestHarness::new("policy_matrix", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::WorkspaceWrite;
        spec.approval_policy = ApprovalPolicy::AutoApproveScoped;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "run".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        loop {
            if matches!(
                next_event(&mut rx).await,
                NormalizedEvent::TurnCompleted {
                    status: TurnTerminalStatus::Completed,
                    ..
                }
            ) {
                break;
            }
        }
        session.close().await.expect("close");

        let replies: BTreeMap<String, Value> = h
            .read_log()
            .into_iter()
            .filter(|r| r["event"] == "perm_reply")
            .map(|r| {
                (
                    r["tag"].as_str().unwrap().to_owned(),
                    r["resp"]["result"]["outcome"].clone(),
                )
            })
            .collect();
        assert_eq!(
            replies["edit_in"],
            json!({"outcome": "selected", "optionId": "allow_edit_in"})
        );
        assert_eq!(
            replies["edit_out"],
            json!({"outcome": "selected", "optionId": "reject_edit_out"})
        );
        assert_eq!(
            replies["exec_in"],
            json!({"outcome": "selected", "optionId": "reject_exec_in"})
        );
        assert_eq!(
            replies["perm_other"],
            json!({"outcome": "selected", "optionId": "reject_perm_other"})
        );
    }
}

#[tokio::test]
async fn readonly_safety_floor_hard_denies_edits_under_interactive_and_scoped_policies() {
    // ReadOnly + AutoApproveScoped: edit inside workspace MUST be hard-denied.
    {
        let h = TestHarness::new("policy_matrix", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::ReadOnly;
        spec.approval_policy = ApprovalPolicy::AutoApproveScoped;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "review".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        let mut saw_assistant = false;
        loop {
            match next_event(&mut rx).await {
                NormalizedEvent::AssistantMessage { text, .. } => {
                    assert_eq!(text, "matrix-complete");
                    saw_assistant = true;
                }
                NormalizedEvent::TurnCompleted { status, .. } => {
                    assert_eq!(status, TurnTerminalStatus::Completed);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_assistant);
        session.close().await.expect("close");

        let replies: BTreeMap<String, Value> = h
            .read_log()
            .into_iter()
            .filter(|r| r["event"] == "perm_reply")
            .map(|r| {
                (
                    r["tag"].as_str().unwrap().to_owned(),
                    r["resp"]["result"]["outcome"].clone(),
                )
            })
            .collect();
        assert_eq!(
            replies["edit_in"],
            json!({"outcome": "selected", "optionId": "reject_edit_in"})
        );
    }

    // ReadOnly + Interactive: edit is hard-denied automatically without asking human;
    // execute escalates to human authority and completes cleanly.
    {
        let h = TestHarness::new("readonly_interactive", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::ReadOnly;
        spec.approval_policy = ApprovalPolicy::Interactive;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "inspect".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnStarted { .. }
        ));
        // Edit is auto-denied: InteractionRequested immediately followed by InteractionResolved.
        let edit_req = match next_event(&mut rx).await {
            NormalizedEvent::InteractionRequested { request } => request,
            other => panic!("expected edit InteractionRequested, got {other:?}"),
        };
        assert!(matches!(
            edit_req.kind,
            InteractionKind::FileChangeApproval { .. }
        ));
        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::InteractionResolved { ref interaction_id, .. }
                if interaction_id == &edit_req.interaction_id
        ));

        // Execute escalates to human caller.
        let exec_req = match next_event(&mut rx).await {
            NormalizedEvent::InteractionRequested { request } => request,
            other => panic!("expected exec InteractionRequested, got {other:?}"),
        };
        assert!(matches!(
            exec_req.kind,
            InteractionKind::CommandApproval { .. }
        ));
        session
            .answer_interaction(InteractionAnswer {
                interaction_id: exec_req.interaction_id.clone(),
                decision_id: "allow_once".to_owned(),
                answers: BTreeMap::new(),
                payload: None,
            })
            .await
            .expect("human allows read-only command");

        let mut saw_assistant = false;
        loop {
            match next_event(&mut rx).await {
                NormalizedEvent::AssistantMessage { text, .. } => {
                    assert_eq!(text, "readonly-ok");
                    saw_assistant = true;
                }
                NormalizedEvent::TurnCompleted { status, .. } => {
                    assert_eq!(status, TurnTerminalStatus::Completed);
                    break;
                }
                _ => {}
            }
        }
        assert!(saw_assistant);
        session.close().await.expect("close");

        let log = h.read_log();
        let set_mode = log
            .iter()
            .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/set_mode")
            .expect("ReadOnly must call session/set_mode");
        assert_eq!(set_mode["msg"]["params"]["modeId"], "default");

        let replies: BTreeMap<String, Value> = log
            .into_iter()
            .filter(|r| r["event"] == "perm_reply")
            .map(|r| {
                (
                    r["tag"].as_str().unwrap().to_owned(),
                    r["resp"]["result"]["outcome"].clone(),
                )
            })
            .collect();
        assert_eq!(
            replies["ro_edit"],
            json!({"outcome": "selected", "optionId": "reject_ro_edit"})
        );
        assert_eq!(
            replies["ro_exec"],
            json!({"outcome": "selected", "optionId": "allow_ro_exec"})
        );
    }
}

#[tokio::test]
async fn readonly_enforces_set_mode_and_fails_closed_when_unadvertised() {
    // 1. Advertised real agy 1.5.18 modes -> selects "default" (never "plan"/"auto"/"accept-edits").
    {
        let h = TestHarness::new("basic", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::ReadOnly;
        let session = h
            .driver()
            .start_session(spec)
            .await
            .expect("start readonly");
        session.close().await.expect("close");

        let log = h.read_log();
        let set_mode = log
            .iter()
            .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/set_mode")
            .expect("session/set_mode must be called for ReadOnly");
        assert_eq!(set_mode["msg"]["params"]["modeId"], "default");
    }

    // 2. Only ["plan", "auto", "accept-edits"] advertised -> fails closed with Unsupported
    //    (proving "plan" is NEVER selected for ReadOnly) and terminates grandchild cleanly!
    {
        let pid_file =
            std::env::temp_dir().join(format!("agy-gc-ro-failclosed-{}.pid", Uuid::now_v7()));
        let h = TestHarness::new(
            "basic",
            &[
                ("FAKE_AGY_MODES", r#"["plan","auto","accept-edits"]"#),
                ("FAKE_AGY_GRANDCHILD_PID_FILE", &pid_file.to_string_lossy()),
            ],
        );
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::ReadOnly;
        let err = h
            .driver()
            .start_session(spec)
            .await
            .err()
            .expect("ReadOnly with only plan/auto/accept-edits must fail closed");
        assert!(matches!(err, DriverError::Unsupported(_)));

        let log = h.read_log();
        assert!(
            !log.iter()
                .any(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/set_mode"),
            "must not call session/set_mode when no safe mode is advertised"
        );

        let gc_pid: u32 = fs::read_to_string(&pid_file)
            .expect("read grandchild pid")
            .trim()
            .parse()
            .expect("parse grandchild pid");
        let reaped = wait_until_dead(gc_pid, Duration::from_secs(3)).await;
        let _ = fs::remove_file(&pid_file);
        assert!(
            reaped,
            "grandchild {gc_pid} must be reaped when ReadOnly fails closed"
        );
    }

    // 3. Resume under ReadOnly with only "plan" / "auto" or unsafe "default" description fails closed.
    {
        let h = TestHarness::new(
            "basic",
            &[
                ("FAKE_AGY_RESUME", "1"),
                (
                    "FAKE_AGY_MODES",
                    r#"{"availableModes":[{"id":"default","name":"Default","description":"Approve edits automatically without prompting"},{"id":"plan","name":"Plan","description":"Plan carefully"}]}"#,
                ),
            ],
        );
        let resume_spec = ResumeSpec {
            provider_session_id: "acp-prev-ro".to_owned(),
            workspace: h.workspace.clone(),
            role_instructions: None,
            model: None,
            effort: None,
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::Interactive,
            output_schema: None,
            mcp_servers: vec![],
        };
        let err = h
            .driver()
            .resume_session(resume_spec)
            .await
            .err()
            .expect("ReadOnly resume without ask-before-edits mode must fail closed");
        assert!(matches!(err, DriverError::Unsupported(_)));
    }
}

#[tokio::test]
async fn interrupt_turn_cancels_pending_permission_and_rejects_late_answer_without_writing() {
    let h = TestHarness::new("cancel_pending", &[]);
    let mut session = h
        .driver()
        .start_session(base_spec(&h.workspace))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");
    let _ = next_event(&mut rx).await;
    let _ = next_event(&mut rx).await;

    let ack = session
        .send_turn(TurnInput {
            text: "wait".to_owned(),
            effort: None,
            attachments: vec![],
            output_schema_override: None,
        })
        .await
        .expect("send_turn");
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnStarted { .. }
    ));

    let req = match next_event(&mut rx).await {
        NormalizedEvent::InteractionRequested { request } => request,
        other => panic!("expected InteractionRequested, got {other:?}"),
    };

    session
        .interrupt_turn(&ack.turn_id)
        .await
        .expect("interrupt_turn");

    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::InteractionResolved {
            ref interaction_id,
            decision_id: None,
            ..
        } if interaction_id == &req.interaction_id
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnCompleted {
            ref turn_id,
            status: TurnTerminalStatus::Interrupted,
            ..
        } if turn_id == &ack.turn_id
    ));

    let stdin_count_before = h
        .read_log()
        .iter()
        .filter(|r| r["event"] == "stdin")
        .count();

    // Late answer MUST return Err(DriverError::InvalidConfig) and MUST NOT write to provider.
    let late_err = session
        .answer_interaction(InteractionAnswer {
            interaction_id: req.interaction_id.clone(),
            decision_id: "allow_once".to_owned(),
            answers: BTreeMap::new(),
            payload: None,
        })
        .await
        .expect_err("late answer must fail");
    assert!(matches!(late_err, DriverError::InvalidConfig(_)));

    tokio::time::sleep(Duration::from_millis(50)).await;
    let stdin_count_after = h
        .read_log()
        .iter()
        .filter(|r| r["event"] == "stdin")
        .count();
    assert_eq!(
        stdin_count_before, stdin_count_after,
        "late answer must not write anything to provider stdin"
    );

    session.close().await.expect("close");

    let log = h.read_log();
    let cancel_rec = log
        .iter()
        .find(|r| r["event"] == "cancel_reply")
        .expect("cancel_reply logged");
    assert_eq!(cancel_rec["saw_cancel"], json!(true));
    assert_eq!(
        cancel_rec["resp"]["result"],
        json!({"outcome": {"outcome": "cancelled"}})
    );
}

#[tokio::test]
async fn conditional_resume_and_close_honor_advertised_session_capabilities() {
    // Advertised: both resume and close work and emit RPCs.
    {
        let h = TestHarness::new(
            "basic",
            &[("FAKE_AGY_RESUME", "1"), ("FAKE_AGY_CLOSE", "1")],
        );
        let mut env = BTreeMap::new();
        env.insert(
            "DOLGORAE_ACTOR_TOKEN".to_owned(),
            "resume-secret-7".to_owned(),
        );

        let resume_spec = ResumeSpec {
            provider_session_id: "acp-prev-99".to_owned(),
            workspace: h.workspace.clone(),
            role_instructions: None,
            model: None,
            effort: None,
            access: AccessMode::WorkspaceWrite,
            approval_policy: ApprovalPolicy::Interactive,
            output_schema: None,
            mcp_servers: vec![McpServerInjection {
                name: "dolgorae".to_owned(),
                command: PathBuf::from("/usr/local/bin/dolgorae"),
                args: vec!["mcp".to_owned()],
                env,
            }],
        };

        let session = h
            .driver()
            .resume_session(resume_spec)
            .await
            .expect("resume advertised");
        assert_eq!(session.provider_session_id(), "acp-prev-99");
        session.close().await.expect("close advertised");

        let log = h.read_log();
        let resume_rpc = log
            .iter()
            .find(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/resume")
            .expect("session/resume sent");
        assert_eq!(resume_rpc["msg"]["params"]["sessionId"], "acp-prev-99");
        assert_eq!(
            resume_rpc["msg"]["params"]["mcpServers"][0]["env"],
            json!([{"name": "DOLGORAE_ACTOR_TOKEN", "value": "resume-secret-7"}])
        );
        assert!(
            log.iter()
                .any(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/close"),
            "session/close must be called when advertised"
        );
    }

    // Unadvertised: resume returns Unsupported and close() skips session/close RPC.
    {
        let h = TestHarness::new(
            "basic",
            &[("FAKE_AGY_RESUME", "0"), ("FAKE_AGY_CLOSE", "0")],
        );
        let resume_spec = ResumeSpec {
            provider_session_id: "acp-prev-99".to_owned(),
            workspace: h.workspace.clone(),
            role_instructions: None,
            model: None,
            effort: None,
            access: AccessMode::ReadOnly,
            approval_policy: ApprovalPolicy::AutoDeny,
            output_schema: None,
            mcp_servers: vec![],
        };
        let err = h
            .driver()
            .resume_session(resume_spec)
            .await
            .err()
            .expect("unadvertised resume must fail");
        assert!(matches!(err, DriverError::Unsupported(_)));

        let session = h
            .driver()
            .start_session(base_spec(&h.workspace))
            .await
            .expect("start_session");
        session.close().await.expect("close without session/close");

        let log = h.read_log();
        assert!(
            !log.iter()
                .any(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/resume"),
            "must not send session/resume when unadvertised"
        );
        assert!(
            !log.iter()
                .any(|r| r["event"] == "stdin" && r["msg"]["method"] == "session/close"),
            "must not send session/close when unadvertised"
        );
    }
}

#[tokio::test]
async fn unknown_update_maps_to_unknown_and_crash_emits_session_closed() {
    let h = TestHarness::new("unknown_and_crash", &[]);
    let mut session = h
        .driver()
        .start_session(base_spec(&h.workspace))
        .await
        .expect("start");
    let mut rx = session.take_events().expect("events");
    let _ = next_event(&mut rx).await;
    let _ = next_event(&mut rx).await;

    let ack = session
        .send_turn(TurnInput {
            text: "trigger crash".to_owned(),
            effort: None,
            attachments: vec![],
            output_schema_override: None,
        })
        .await
        .expect("send_turn");

    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnStarted { .. }
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::Unknown { ref event_type, .. }
            if event_type == "acp.session_update.future_vendor_widget"
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::TurnCompleted {
            ref turn_id,
            status: TurnTerminalStatus::Failed,
            error: Some(_),
        } if turn_id == &ack.turn_id
    ));
    assert!(matches!(
        next_event(&mut rx).await,
        NormalizedEvent::SessionClosed { reason: Some(_) }
    ));
}

#[tokio::test]
async fn pgid_teardown_reaps_grandchild_sleeper_on_close_and_drop() {
    // 1. Explicit close() reaps grandchild even when parent ignores SIGTERM.
    {
        let pid_file = std::env::temp_dir().join(format!("agy-gc-close-{}.pid", Uuid::now_v7()));
        let h = TestHarness::new(
            "ignore_term",
            &[
                ("FAKE_AGY_CLOSE", "0"),
                ("FAKE_AGY_GRANDCHILD_PID_FILE", &pid_file.to_string_lossy()),
            ],
        );
        let session = h
            .driver()
            .start_session(base_spec(&h.workspace))
            .await
            .expect("start");

        let gc_pid: u32 = fs::read_to_string(&pid_file)
            .expect("read grandchild pid")
            .trim()
            .parse()
            .expect("parse grandchild pid");
        assert!(pid_is_alive(gc_pid), "grandchild {gc_pid} should be alive");

        session.close().await.expect("close");

        let reaped = wait_until_dead(gc_pid, Duration::from_secs(3)).await;
        let _ = fs::remove_file(&pid_file);
        assert!(reaped, "grandchild {gc_pid} must be reaped by PGID close()");
    }

    // 2. Drop triggers non-blocking SIGTERM -> grace -> SIGKILL via detached reaper (even on ignore_term).
    {
        let pid_file = std::env::temp_dir().join(format!("agy-gc-drop-{}.pid", Uuid::now_v7()));
        let h = TestHarness::new(
            "ignore_term",
            &[
                ("FAKE_AGY_CLOSE", "0"),
                ("FAKE_AGY_GRANDCHILD_PID_FILE", &pid_file.to_string_lossy()),
            ],
        );
        let session = h
            .driver()
            .start_session(base_spec(&h.workspace))
            .await
            .expect("start");

        let gc_pid: u32 = fs::read_to_string(&pid_file)
            .expect("read grandchild pid")
            .trim()
            .parse()
            .expect("parse grandchild pid");
        assert!(pid_is_alive(gc_pid), "grandchild {gc_pid} should be alive");

        let drop_start = tokio::time::Instant::now();
        drop(session);
        assert!(
            drop_start.elapsed() < Duration::from_millis(200),
            "Drop must not block or sleep"
        );

        let reaped = wait_until_dead(gc_pid, Duration::from_secs(3)).await;
        let _ = fs::remove_file(&pid_file);
        assert!(reaped, "grandchild {gc_pid} must be reaped after Drop");
    }
}

#[tokio::test]
async fn readonly_hard_denies_switch_mode_and_scoped_never_selects_allow_always() {
    // 1. ReadOnly + Interactive MUST hard-deny `switch_mode` automatically.
    {
        let h = TestHarness::new("readonly_switch_mode", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::ReadOnly;
        spec.approval_policy = ApprovalPolicy::Interactive;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "try mode switch".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnStarted { .. }
        ));
        let req = match next_event(&mut rx).await {
            NormalizedEvent::InteractionRequested { request } => request,
            other => panic!("expected InteractionRequested, got {other:?}"),
        };
        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::InteractionResolved {
                ref interaction_id,
                decision_id: Some(ref d),
                ..
            } if interaction_id == &req.interaction_id && d == "deny_once"
        ));
        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnCompleted {
                status: TurnTerminalStatus::Completed,
                ..
            }
        ));
        session.close().await.expect("close");

        let reply = h
            .read_log()
            .into_iter()
            .find(|r| r["event"] == "perm_reply" && r["tag"] == "switch_mode")
            .expect("switch_mode reply logged");
        assert_eq!(
            reply["resp"]["result"]["outcome"],
            json!({"outcome": "selected", "optionId": "reject_mode_switch"})
        );
    }

    // 2. AutoApproveScoped when only `allow_always` is offered MUST cancel, never select `allow_always`.
    {
        let h = TestHarness::new("scoped_allow_always_only", &[]);
        let mut spec = base_spec(&h.workspace);
        spec.access = AccessMode::WorkspaceWrite;
        spec.approval_policy = ApprovalPolicy::AutoApproveScoped;
        let mut session = h.driver().start_session(spec).await.expect("start");
        let mut rx = session.take_events().expect("events");
        let _ = next_event(&mut rx).await;
        let _ = next_event(&mut rx).await;

        session
            .send_turn(TurnInput {
                text: "edit".to_owned(),
                effort: None,
                attachments: vec![],
                output_schema_override: None,
            })
            .await
            .expect("send_turn");

        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnStarted { .. }
        ));
        let req = match next_event(&mut rx).await {
            NormalizedEvent::InteractionRequested { request } => request,
            other => panic!("expected InteractionRequested, got {other:?}"),
        };
        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::InteractionResolved {
                ref interaction_id,
                decision_id: None,
                ..
            } if interaction_id == &req.interaction_id
        ));
        assert!(matches!(
            next_event(&mut rx).await,
            NormalizedEvent::TurnCompleted {
                status: TurnTerminalStatus::Completed,
                ..
            }
        ));
        session.close().await.expect("close");

        let reply = h
            .read_log()
            .into_iter()
            .find(|r| r["event"] == "perm_reply" && r["tag"] == "always_only")
            .expect("always_only reply logged");
        assert_eq!(
            reply["resp"]["result"],
            json!({"outcome": {"outcome": "cancelled"}})
        );
    }
}

fn pid_is_alive(pid: u32) -> bool {
    StdCommand::new("/bin/kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

async fn wait_until_dead(pid: u32, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    while tokio::time::Instant::now() < deadline {
        if !pid_is_alive(pid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    !pid_is_alive(pid)
}

#[tokio::test]
#[ignore = "opt-in live account smoke test; set DOLGORAE_LIVE_SMOKE=1 to run"]
async fn live_agy_acp_smoke() {
    if std::env::var("DOLGORAE_LIVE_SMOKE").as_deref() != Ok("1") {
        return;
    }
    let driver = AgyDriver::new();
    let caps = driver.probe().await.expect("live probe");
    assert!(
        caps.available,
        "agy binary must be available for live smoke"
    );
    assert!(!caps.native_output_schema);
}
