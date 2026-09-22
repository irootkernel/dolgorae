//! Shared black-box fixture: production CLI/server plus the independent native fake.
#![allow(dead_code)]
use dolgorae::protocol::public_v1 as pb;
use serde_json::Value;
use std::fs;
use std::io::{BufRead as _, BufReader, Read as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use uuid::Uuid;

pub fn context() -> Option<pb::RequestContext> {
    Some(pb::RequestContext {
        protocol_version: 1,
        client_request_id: Uuid::now_v7().to_string(),
        client_instance_id: "native-gateway-test".into(),
    })
}

pub fn base_scenario() -> Value {
    serde_json::from_slice(
        &fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tools/fake_app_server/scenarios/run_start_model_list.json"),
        )
        .unwrap(),
    )
    .unwrap()
}

pub struct Fixture {
    pub root: PathBuf,
    pub home: PathBuf,
    pub workspace: PathBuf,
    pub workspace_id: String,
    pub state_root: PathBuf,
    pub controller: PathBuf,
    pub operator: PathBuf,
    pub profile: String,
    pub controller_id: String,
    pub server_key: String,
    pub binary: PathBuf,
}

impl Fixture {
    pub fn new(scenario: &str) -> Self {
        Self::prepare(scenario, None, false)
    }
    pub fn new_compact(scenario: &str) -> Self {
        Self::prepare(scenario, None, true)
    }
    pub fn with_scenario(scenario: Value) -> Self {
        Self::prepare("custom", Some(scenario), false)
    }
    fn prepare(scenario: &str, custom: Option<Value>, compact_binary: bool) -> Self {
        // A short, exclusive directory is required by Darwin's 104-byte UDS
        // bound even when the enclosing Make invocation has a long TMPDIR.
        let root = PathBuf::from("/private/tmp").join(format!("dgg-{}", Uuid::now_v7().simple()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let source_binary = std::env::var_os("DOLGORAE_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_dolgorae")));
        // Keep the exact executable immutable while parallel native cases or
        // another build replace Cargo's output pathname.
        let binary = root.join("dolgorae");
        fs::copy(source_binary, &binary).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        if compact_binary {
            assert!(
                Command::new("/usr/bin/strip")
                    .arg(&binary)
                    .status()
                    .unwrap()
                    .success(),
                "failed to compact the native gateway fixture binary"
            );
        }
        let scenario = if let Some(custom) = custom {
            let path = root.join("custom-scenario.json");
            fs::write(&path, serde_json::to_vec(&custom).unwrap()).unwrap();
            path.to_string_lossy().into_owned()
        } else {
            scenario.to_owned()
        };
        let python = std::env::var_os("DOLGORAE_TEST_PYTHON")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let local = Path::new(env!("CARGO_MANIFEST_DIR")).join(".venv/bin/python");
                if local.is_file() {
                    local
                } else {
                    PathBuf::from("python3")
                }
            });
        let setup = Command::new(python)
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/gateway_fixture.py"))
            .args([
                "--binary",
                binary.to_str().unwrap(),
                "--root",
                root.to_str().unwrap(),
                "--scenario",
                &scenario,
            ])
            .output()
            .unwrap();
        assert!(
            setup.status.success(),
            "gateway fixture setup failed: {} {}",
            String::from_utf8_lossy(&setup.stdout),
            String::from_utf8_lossy(&setup.stderr)
        );
        let mut metadata: Value =
            serde_json::from_slice(&fs::read(root.join("fixture.json")).unwrap()).unwrap();
        let owner = dolgorae::darwin::DarwinSystem
            .bsd_process_identity(std::process::id())
            .unwrap();
        metadata["test_case"] = std::thread::current()
            .name()
            .unwrap_or("unnamed-native-case")
            .into();
        metadata["test_owner"] = serde_json::json!({"pid": owner.pid, "uid": owner.uid, "start_tvsec": owner.start_tvsec, "start_tvusec": owner.start_tvusec});
        fs::write(
            root.join("fixture.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .unwrap();
        let home = root.join("home");
        let workspace_id = metadata["workspace_id"].as_str().unwrap().to_owned();
        Self {
            state_root: home.join(".dolgorae/workspaces").join(&workspace_id),
            home,
            workspace: root.join("workspace"),
            workspace_id,
            controller: root
                .join("home/.dolgorae/controller-carriers/gateway-native/test-installation/controller.json"),
            operator: root.join("operator.json"),
            profile: "default".into(),
            controller_id: metadata["controller_id"].as_str().unwrap().into(),
            server_key: metadata["server_key"].as_str().unwrap().into(),
            binary,
            root,
        }
    }
    pub fn process(&self) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .env("HOME", &self.home)
            .env("TMPDIR", self.root.join("tmp"))
            .env("XDG_CONFIG_HOME", self.root.join("config"))
            .env("XDG_CACHE_HOME", self.root.join("cache"));
        command
    }
    pub fn command(&self, args: &[&str]) -> Output {
        self.process().args(args).output().unwrap()
    }
    pub fn cli(&self, args: &[&str]) -> Value {
        let output = self.command(args);
        assert!(
            output.status.success(),
            "CLI failed {args:?}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(envelope["ok"], true, "CLI failed: {envelope}");
        envelope["data"].clone()
    }
    pub fn workspace(&self) -> pb::WorkspaceRef {
        pb::WorkspaceRef {
            absolute_path: self.workspace.to_string_lossy().into_owned(),
            expected_workspace_id: self.workspace_id.clone(),
        }
    }
    pub fn run_ref(&self, id: &str) -> Option<pb::RunRef> {
        Some(pb::RunRef {
            workspace: Some(self.workspace()),
            run_id: id.into(),
        })
    }
    pub fn carrier(&self) -> Option<pb::ControllerCarrierRef> {
        Some(pb::ControllerCarrierRef {
            absolute_file_path: self.controller.to_string_lossy().into_owned(),
            expected_controller_id: self.controller_id.clone(),
            expected_controller_generation: 1,
        })
    }
    pub fn start_run(&self, extra: &[&str]) -> String {
        let mut args = vec![
            "run",
            "--controller-file",
            self.controller.to_str().unwrap(),
            "start",
            "--workspace",
            self.workspace.to_str().unwrap(),
            "--profile",
            &self.profile,
        ];
        let key = Uuid::now_v7().to_string();
        for (flag, value) in [
            ("--control-mode", "managed-agent"),
            ("--execution-lane", "shared-readonly"),
            ("--required-assurance", "best-effort-personal-alpha"),
            ("--purpose", "implementation"),
            ("--instructions", "Execute the isolated gateway test task."),
            ("--idempotency-key", key.as_str()),
        ] {
            if !extra
                .iter()
                .any(|argument| *argument == flag || argument.starts_with(&format!("{flag}=")))
            {
                args.extend([flag, value]);
            }
        }
        args.extend(extra);
        self.cli(&args)["run_id"].as_str().unwrap().to_owned()
    }
    pub fn start_gateway(&self) -> Gateway {
        Gateway::start(self, self.root.join("g.sock"))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let mut closed = true;
        if let Ok(runs) = fs::read_dir(self.state_root.join("runs")) {
            for run in runs.flatten() {
                if let Some(id) = run
                    .file_name()
                    .to_str()
                    .filter(|id| Uuid::parse_str(id).is_ok())
                {
                    let status = self.command(&[
                        "run",
                        "status",
                        id,
                        "--workspace",
                        self.workspace.to_str().unwrap(),
                    ]);
                    let already_closed = status.status.success()
                        && serde_json::from_slice::<Value>(&status.stdout)
                            .is_ok_and(|value| value["ok"] == true && value["data"]["state"] == "closed")
                        && dolgorae::worker::runtime_record_path(
                            &dolgorae::worker::runtime_root(&self.state_root),
                            Uuid::parse_str(id).unwrap(),
                        )
                        .is_ok_and(|path| {
                            matches!(fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
                        });
                    if already_closed {
                        continue;
                    }
                    closed &= self
                        .command(&[
                            "run",
                            "--controller-file",
                            self.controller.to_str().unwrap(),
                            "close",
                            id,
                            "--workspace",
                            self.workspace.to_str().unwrap(),
                        ])
                        .status
                        .success();
                }
            }
        }
        let stopped = self.command(&[
            "profile",
            "server",
            "stop",
            &self.profile,
            "--operator-file",
            self.operator.to_str().unwrap(),
            "--interrupt",
            "--confirm-server-key",
            &self.server_key,
        ]);
        if closed && stopped.status.success() && !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.root);
        } else {
            eprintln!(
                "native fixture retained after test failure or incomplete verified cleanup: {}",
                self.root.display()
            );
        }
    }
}

pub struct Gateway {
    pub socket: PathBuf,
    pub ready: Value,
    pub child: Child,
    stdout: Option<BufReader<std::process::ChildStdout>>,
}
impl Gateway {
    pub fn start(fixture: &Fixture, socket: PathBuf) -> Self {
        let mut child = fixture
            .process()
            .args(["serve", "--socket", socket.to_str().unwrap()])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        let ready: Value = serde_json::from_str(&line)
            .unwrap_or_else(|error| panic!("invalid gateway readiness {line:?}: {error}"));
        assert_eq!(ready["ok"], true, "gateway startup failed: {ready}");
        assert_eq!(ready["command"], "serve");
        Self {
            socket,
            ready,
            child,
            stdout: Some(stdout),
        }
    }
    pub async fn channel(&self) -> tonic::transport::Channel {
        let socket = self.socket.clone();
        let channel = tonic::transport::Endpoint::try_from("http://[::]:50051")
            .unwrap()
            .initial_stream_window_size(1024)
            .connect_with_connector(tower::service_fn(move |_| {
                let socket = socket.clone();
                async move {
                    tokio::net::UnixStream::connect(socket)
                        .await
                        .map(hyper_util::rt::TokioIo::new)
                }
            }))
            .await
            .unwrap();
        let mut runtime = pb::runtime_service_client::RuntimeServiceClient::new(channel.clone());
        let capabilities = runtime
            .get_capabilities(pb::GetCapabilitiesRequest {
                context: Some(pb::RequestContext {
                    protocol_version: 0,
                    ..context().unwrap()
                }),
                minimum_protocol_version: 1,
                maximum_protocol_version: 1,
            })
            .await
            .unwrap()
            .into_inner();
        let negotiated = capabilities.context.unwrap();
        assert_eq!(negotiated.protocol_version, 1);
        assert_eq!(
            negotiated.server_instance_id,
            self.ready["data"]["server_instance_id"].as_str().unwrap()
        );
        assert_eq!(
            capabilities.descriptor_sha256,
            dolgorae::protocol::PUBLIC_V1_DESCRIPTOR_SHA256
        );
        channel
    }
    pub fn terminate(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            assert!(
                Command::new("/bin/kill")
                    .args(["-TERM", &self.child.id().to_string()])
                    .status()
                    .unwrap()
                    .success()
            );
            let deadline = Instant::now() + Duration::from_secs(7);
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    assert!(
                        status.success(),
                        "gateway graceful shutdown failed: {status}"
                    );
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "gateway exceeded the five-second drain budget"
                );
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let mut later = String::new();
        if let Some(mut stdout) = self.stdout.take() {
            stdout.read_to_string(&mut later).unwrap();
        }
        assert!(
            later.is_empty(),
            "gateway wrote stdout after readiness: {later}"
        );
    }
    pub fn kill(&mut self) {
        if self.child.try_wait().unwrap().is_none() {
            self.child.kill().unwrap();
            self.child.wait().unwrap();
        }
        self.stdout.take();
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = Command::new("/bin/kill")
                .args(["-TERM", &self.child.id().to_string()])
                .status();
            let deadline = Instant::now() + Duration::from_secs(7);
            while self.child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if self.child.try_wait().ok().flatten().is_none() {
                let _ = self.child.kill();
                let _ = self.child.wait();
            }
        }
    }
}
