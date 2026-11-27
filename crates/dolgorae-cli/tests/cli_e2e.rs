//! Black-box CLI subprocess E2E tests for `dolgorae`.
//!
//! Spawns real `dolgorae serve`, CLI commands, and `dolgorae mcp --scoped`
//! subprocesses against isolated TempDir homes/sockets and offline fake driver
//! scripts (`fake_codex.py`, `fake_claude.py`, `fake_agy_acp.py`). Zero live calls.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

static TEMP_SEQ: AtomicU64 = AtomicU64::new(1);

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from("/tmp").join(format!("dg-c-{label}-{}-{seq}", std::process::id()));
        fs::create_dir_all(&dir).expect("create temp dir");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).expect("chmod 0700");
        Self { path: dir }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    fn id(&self) -> u32 {
        self.child.as_ref().expect("child alive").id()
    }

    fn wait_exit(mut self, timeout: Duration) -> std::process::ExitStatus {
        let mut child = self.child.take().expect("child");
        let start = Instant::now();
        loop {
            if let Some(status) = child.try_wait().expect("try_wait") {
                return status;
            }
            if start.elapsed() > timeout {
                let _ = child.kill();
                panic!("child process did not exit within {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn dolgorae_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_dolgorae"))
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn make_python_wrapper(dir: &Path, name: &str, script_rel: &str) -> PathBuf {
    let script = workspace_root().join(script_rel);
    assert!(
        script.is_file(),
        "fake driver script missing: {}",
        script.display()
    );
    let wrapper = dir.join(name);
    let content = format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", script.display());
    fs::write(&wrapper, content).expect("write wrapper");
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o755)).expect("chmod +x");
    wrapper
}

fn run_cli_json(home: &Path, sock: &Path, envs: &[(&str, &Path)], args: &[&str]) -> Value {
    let mut cmd = Command::new(dolgorae_bin());
    cmd.env("HOME", home)
        .env("DOLGORAE_HOME", home)
        .env("DOLGORAE_SOCKET", sock)
        .arg("--home")
        .arg(home)
        .arg("--socket")
        .arg(sock)
        .arg("--json")
        .args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("run dolgorae cli");
    assert!(
        out.status.success(),
        "cli {:?} failed ({:?}): stdout={} stderr={}",
        args,
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("cli stdout valid JSON")
}

#[test]
fn cli_serve_duplicate_lock_headless_session_scoped_mcp_and_sigterm() {
    let home = TempDir::new("home");
    let ws = TempDir::new("ws");
    let bins = TempDir::new("fake-bins");
    let sock = home.path().join("run").join("dolgorae.sock");

    let fake_codex = make_python_wrapper(
        bins.path(),
        "fake-codex",
        "crates/drivers/driver-codex-app/tests/fake_codex.py",
    );
    let fake_claude = make_python_wrapper(
        bins.path(),
        "fake-claude",
        "crates/drivers/driver-claude-wire/tests/fake_claude.py",
    );
    let fake_agy = make_python_wrapper(
        bins.path(),
        "fake-agy",
        "crates/drivers/driver-agy-acp/tests/fake_agy_acp.py",
    );
    let driver_envs = [
        ("DOLGORAE_CODEX_BIN", fake_codex.as_path()),
        ("DOLGORAE_CLAUDE_BIN", fake_claude.as_path()),
        ("DOLGORAE_AGY_BIN", fake_agy.as_path()),
    ];

    // Add a workspace role for `role list`.
    fs::create_dir_all(ws.path().join(".dolgorae/roles")).unwrap();
    fs::write(
        ws.path().join(".dolgorae/roles/qa.md"),
        "---\ndriver: codex-app\naccess: read_only\napproval_policy: auto_deny\n---\nVerify output.",
    )
    .unwrap();

    // 1. Spawn real `dolgorae serve` subprocess with FAKE_CODEX_LOG to verify
    //    real injected DOLGORAE_ACTOR_TOKEN authentication in `dolgorae mcp --scoped`.
    let codex_log = home.path().join("fake_codex.jsonl");
    let mut serve_cmd = Command::new(dolgorae_bin());
    serve_cmd
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .env("DOLGORAE_CODEX_BIN", &fake_codex)
        .env("DOLGORAE_CLAUDE_BIN", &fake_claude)
        .env("DOLGORAE_AGY_BIN", &fake_agy)
        .env("FAKE_CODEX_LOG", &codex_log)
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let daemon = ChildGuard::new(serve_cmd.spawn().expect("spawn dolgorae serve"));

    // Wait for daemon socket to appear and answer `/v2/capabilities`.
    let start = Instant::now();
    while !sock.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "daemon socket did not appear in time"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    // 2. CLI `driver list` & `role list` over live daemon.
    let drivers = run_cli_json(home.path(), &sock, &driver_envs, &["driver", "list"]);
    let driver_arr = drivers.as_array().expect("drivers array");
    assert_eq!(driver_arr.len(), 3);
    assert!(
        driver_arr.iter().all(|d| d["available"] == json!(true)),
        "all 3 offline fake drivers should probe available: {drivers}"
    );

    let roles = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["role", "list", "--workspace", ws.path().to_str().unwrap()],
    );
    assert_eq!(roles["roles"][0]["name"], "qa");

    // 3. Haeryong headless pattern: `session start` with `auto-deny` + `output-schema`.
    let schema = r#"{"type":"object","required":["ok"],"properties":{"ok":{"type":"boolean"}}}"#;
    let started = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &[
            "session",
            "start",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--driver",
            "codex-app",
            "--approval-policy",
            "auto-deny",
            "--output-schema",
            schema,
        ],
    );
    let sid = started["session_id"].as_str().unwrap().to_owned();
    assert_eq!(started["status"], "active");

    // 4. Duplicate `dolgorae serve` on same home MUST exit non-zero while first session stays active!
    let dup = Command::new(dolgorae_bin())
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("serve")
        .output()
        .expect("run duplicate serve");
    assert!(
        !dup.status.success(),
        "duplicate dolgorae serve must exit non-zero"
    );

    let still_active = run_cli_json(home.path(), &sock, &driver_envs, &["session", "get", &sid]);
    assert_eq!(still_active["status"], "active");
    assert_eq!(still_active["session_generation"], 1);

    // 5. Submit turn, wait, read timeline, and close session cleanly via CLI.
    let turn = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "turn", &sid, "hello headless"],
    );
    assert!(!turn["turn_id"].as_str().unwrap().is_empty());

    let waited = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "wait", &sid, "--timeout-ms", "5000"],
    );
    assert_eq!(waited["timed_out"], json!(false));

    let timeline = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "timeline", &sid],
    );
    assert!(
        timeline["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "turn_completed"),
        "expected turn_completed in timeline: {timeline}"
    );

    let closed = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "close", &sid],
    );
    assert_eq!(closed["status"], "closed");

    // 6. Real `dolgorae mcp --scoped` subprocess:
    //    (a) fails closed when DOLGORAE_ACTOR_TOKEN is missing!
    let mcp_no_tok = Command::new(dolgorae_bin())
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .env_remove("DOLGORAE_ACTOR_TOKEN")
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("mcp")
        .arg("--scoped")
        .output()
        .expect("run mcp --scoped without token");
    assert!(
        !mcp_no_tok.status.success(),
        "mcp --scoped must fail closed without token"
    );

    //    (b) with a delegating session, verify `dolgorae mcp` (External Host) + `dolgorae mcp --scoped`
    //        over real stdio pipes against the live `dolgorae serve` subprocess!
    let mut mcp_child = Command::new(dolgorae_bin())
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .env("DOLGORAE_ACTOR_TOKEN", "forged-e2e-token")
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("mcp")
        .arg("--scoped")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn dolgorae mcp --scoped");

    {
        let stdin = mcp_child.stdin.as_mut().unwrap();
        let stdout = mcp_child.stdout.as_mut().unwrap();
        let mut reader = BufReader::new(stdout);

        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}})
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
        )
        .unwrap();
        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"actor_mailbox_read","arguments":{}}})
        )
        .unwrap();
        stdin.flush().unwrap();

        let mut l1 = String::new();
        let mut l2 = String::new();
        let mut l3 = String::new();
        reader.read_line(&mut l1).unwrap();
        reader.read_line(&mut l2).unwrap();
        reader.read_line(&mut l3).unwrap();

        let r1: Value = serde_json::from_str(&l1).unwrap();
        let r2: Value = serde_json::from_str(&l2).unwrap();
        let r3: Value = serde_json::from_str(&l3).unwrap();
        assert_eq!(r1["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r2["result"]["tools"].as_array().unwrap().len(), 7);
        assert_eq!(r3["result"]["isError"], json!(true));
        let err_payload: Value =
            serde_json::from_str(r3["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(err_payload["error"]["code"], "UNAUTHORIZED");
    }
    drop(mcp_child.stdin.take());
    let mcp_status = mcp_child.wait().expect("wait mcp child");
    assert!(mcp_status.success());

    //    (c) Start a delegating session, extract the REAL injected DOLGORAE_ACTOR_TOKEN
    //        from fake_codex.jsonl, and authenticate a real `dolgorae mcp --scoped` subprocess!
    let deleg = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &[
            "session",
            "start",
            "--workspace",
            ws.path().to_str().unwrap(),
            "--driver",
            "codex-app",
            "--allow-delegation",
        ],
    );
    let deleg_sid = deleg["session_id"].as_str().unwrap().to_owned();

    let log_text = fs::read_to_string(&codex_log).expect("read fake_codex log");
    let real_token = log_text
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find_map(|entry| {
            entry
                .pointer("/msg/params/config/mcp_servers/dolgorae/env/DOLGORAE_ACTOR_TOKEN")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .expect("dolgorae serve injected DOLGORAE_ACTOR_TOKEN into thread/start config");

    let mut auth_mcp = Command::new(dolgorae_bin())
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .env("DOLGORAE_ACTOR_TOKEN", &real_token)
        .env("DOLGORAE_SESSION_ID", &deleg_sid)
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("mcp")
        .arg("--scoped")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn authenticated dolgorae mcp --scoped");

    {
        let stdin = auth_mcp.stdin.as_mut().unwrap();
        let stdout = auth_mcp.stdout.as_mut().unwrap();
        let mut reader = BufReader::new(stdout);

        writeln!(
            stdin,
            "{}",
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"actor_mailbox_read","arguments":{}}})
        )
        .unwrap();
        stdin.flush().unwrap();

        let mut resp_line = String::new();
        reader.read_line(&mut resp_line).unwrap();
        let resp: Value = serde_json::from_str(&resp_line).unwrap();
        assert_eq!(
            resp["result"]["isError"],
            json!(false),
            "authenticated scoped MCP actor_mailbox_read must succeed: {resp}"
        );
    }
    drop(auth_mcp.stdin.take());
    assert!(auth_mcp.wait().expect("wait auth mcp").success());

    // Keep `deleg_sid` ACTIVE across SIGTERM to verify B3: graceful shutdown
    // detaches active sessions to resumable `idle` (NOT `closed`)!

    // 7. SIGTERM clean shutdown unlinks socket and exits 0 cleanly!
    let pid = daemon.id().to_string();
    let kill_status = Command::new("kill")
        .arg("-TERM")
        .arg(&pid)
        .status()
        .expect("send SIGTERM");
    assert!(kill_status.success());
    let daemon_status = daemon.wait_exit(Duration::from_secs(5));
    assert!(
        daemon_status.success(),
        "daemon should exit cleanly on SIGTERM: {daemon_status:?}"
    );
    assert!(
        !sock.exists(),
        "socket must be cleanly unlinked on SIGTERM shutdown"
    );

    // 8. Restart `dolgorae serve` on same home -> `deleg_sid` MUST be `idle`
    //    (resumable!) and resume cleanly back to `active`!
    let mut restart_cmd = Command::new(dolgorae_bin());
    restart_cmd
        .env("HOME", home.path())
        .env("DOLGORAE_HOME", home.path())
        .env("DOLGORAE_SOCKET", &sock)
        .env("DOLGORAE_CODEX_BIN", &fake_codex)
        .env("DOLGORAE_CLAUDE_BIN", &fake_claude)
        .env("DOLGORAE_AGY_BIN", &fake_agy)
        .arg("--home")
        .arg(home.path())
        .arg("--socket")
        .arg(&sock)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let daemon2 = ChildGuard::new(restart_cmd.spawn().expect("restart dolgorae serve"));
    let start2 = Instant::now();
    while !sock.exists() {
        assert!(
            start2.elapsed() < Duration::from_secs(5),
            "restarted socket missing"
        );
        std::thread::sleep(Duration::from_millis(25));
    }

    let detached = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "get", &deleg_sid],
    );
    assert_eq!(
        detached["status"], "idle",
        "clean SIGTERM must detach active session to resumable idle, not closed: {detached}"
    );
    assert_eq!(detached["session_generation"], 2);

    let resumed = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "resume", &deleg_sid],
    );
    assert_eq!(resumed["status"], "active");
    assert_eq!(resumed["session_generation"], 3);

    let _ = run_cli_json(
        home.path(),
        &sock,
        &driver_envs,
        &["session", "close", &deleg_sid],
    );

    let pid2 = daemon2.id().to_string();
    assert!(
        Command::new("kill")
            .arg("-TERM")
            .arg(&pid2)
            .status()
            .unwrap()
            .success()
    );
    assert!(daemon2.wait_exit(Duration::from_secs(5)).success());
    assert!(!sock.exists());
}

#[test]
fn cli_version_flags_and_subcommand_emit_consistent_v_prefixed_version() {
    let expected_version = format!("v{}", env!("CARGO_PKG_VERSION"));
    let expected_human = format!("dolgorae {expected_version}\n");

    for args in [&["--version"][..], &["-V"][..], &["version"][..]] {
        let out = Command::new(dolgorae_bin())
            .args(args)
            .output()
            .expect("run dolgorae version");
        assert!(
            out.status.success(),
            "cli {args:?} failed ({:?}): stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            expected_human,
            "unexpected stdout for {args:?}"
        );
        assert!(
            out.stderr.is_empty(),
            "expected empty stderr for {args:?}, got: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let out = Command::new(dolgorae_bin())
        .args(["version", "--json"])
        .output()
        .expect("run dolgorae version --json");
    assert!(
        out.status.success(),
        "cli version --json failed ({:?}): stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        out.stderr.is_empty(),
        "expected empty stderr for version --json, got: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let payload: Value =
        serde_json::from_slice(&out.stdout).expect("version --json stdout valid JSON");
    assert_eq!(payload["name"], json!("dolgorae"));
    assert_eq!(payload["version"], json!(expected_version));
    assert_eq!(payload["api_version"], json!("v2"));
}
