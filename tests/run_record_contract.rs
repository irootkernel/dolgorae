use dolgorae::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
use dolgorae::domain::{
    Access, Assurance, ControlMode, ControllerIdentity, ControllerKind, ExecutionLane, Purpose,
    PurposeKind,
};
use dolgorae::jcs::{
    PayloadRepresentation, RAW_PAYLOAD_LIMIT, canonicalize, parse, represent_payload,
};
use dolgorae::run::{
    AgentConfigurationSnapshot, AppServerFacts, AuditPolicy, CapabilityState, CompatibilityVerdict,
    ControllerBinding, DolgoraeBuild, ExecutableIdentity, InstructionSnapshot, ParentReference,
    ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest, RunStore, StartReservation,
    StartReservationStore, controller_capability_digest, launch_contract_digest,
    runtime_profile_snapshot_digest,
};
use dolgorae::workspace::{GitBaseline, LosslessPath, SystemWorkspacePlatform, WorkspaceMode};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

fn canonical(input: &str) -> String {
    String::from_utf8(canonicalize(&parse(input).unwrap()).unwrap()).unwrap()
}

#[test]
fn rfc8785_published_serialization_and_sorting_vectors_are_exact() {
    let input = r#"{
      "numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],
      "string":"\u20ac$\u000F\nA'B\"\\\\\"/",
      "literals":[null,true,false]
    }"#;
    assert_eq!(
        canonical(input),
        r#"{"literals":[null,true,false],"numbers":[333333333.3333333,1e+30,4.5,0.002,1e-27],"string":"€$\u000f\nA'B\"\\\\\"/"}"#
    );

    let sorted = canonical(
        r#"{"\u20ac":"Euro Sign","\r":"Carriage Return","\ufb33":"Hebrew Letter Dalet With Dagesh","1":"One","\ud83d\ude00":"Emoji: Grinning Face","\u0080":"Control","\u00f6":"Latin Small Letter O With Diaeresis"}"#,
    );
    assert_eq!(
        sorted,
        r#"{"\r":"Carriage Return","1":"One","":"Control","ö":"Latin Small Letter O With Diaeresis","€":"Euro Sign","😀":"Emoji: Grinning Face","דּ":"Hebrew Letter Dalet With Dagesh"}"#
    );
}

#[test]
fn marker_escape_redaction_and_numeric_adaptation_follow_the_required_order() {
    let input = r#"{
      "$dolgorae_number":9007199254740993,
      "password2":"raw",
      "passwords":false,
      "oauth2_token":[],
      "api_key_2":{},
      "APIKeys":null,
      "apikey":"compact",
      "token":"not-secret",
      "session_id":"not-secret",
      "encoded":"{\"password\":\"still-a-string\"}",
      "비밀번호":{"clientSecret":"nested"},
      "items":[{"password_hash":1}]
    }"#;
    let PayloadRepresentation::Represented {
        canonical_bytes, ..
    } = represent_payload(input.as_bytes())
    else {
        panic!("payload should be representable");
    };
    let value: Value = serde_json::from_slice(&canonical_bytes).unwrap();
    assert!(value.get("$$dolgorae_number").is_some());
    assert_eq!(
        value["$$dolgorae_number"],
        serde_json::json!({"$dolgorae_number": "9007199254740993"})
    );
    for key in [
        "password2",
        "passwords",
        "oauth2_token",
        "api_key_2",
        "APIKeys",
        "apikey",
    ] {
        assert_eq!(value[key]["$dolgorae_redacted"]["reason"], "secret_key");
    }
    assert_eq!(value["token"], "not-secret");
    assert_eq!(value["session_id"], "not-secret");
    assert_eq!(value["encoded"], r#"{"password":"still-a-string"}"#);
    assert_eq!(
        value["비밀번호"]["clientSecret"]["$dolgorae_redacted"]["original_type"],
        "string"
    );
    assert_eq!(
        value["items"][0]["password_hash"]["$dolgorae_redacted"]["original_type"],
        "number"
    );
}

#[test]
fn payload_limits_fail_closed_without_retaining_source_bytes() {
    let oversized = vec![b'x'; RAW_PAYLOAD_LIMIT + 1];
    let PayloadRepresentation::Unrepresentable(metadata) = represent_payload(&oversized) else {
        panic!("oversized raw payload must be rejected");
    };
    assert_eq!(metadata.reason, "raw_payload_too_large");
    assert_eq!(
        metadata.observed_byte_length,
        (RAW_PAYLOAD_LIMIT + 1) as u64
    );
    assert_eq!(metadata.raw_sha256.len(), 64);

    let mut expanding = String::from("{");
    let mut index = 0_u32;
    while expanding.len() < 750_000 {
        if index > 0 {
            expanding.push(',');
        }
        expanding.push_str(&format!("\"password{index}\":0"));
        index += 1;
    }
    expanding.push('}');
    let PayloadRepresentation::Unrepresentable(metadata) = represent_payload(expanding.as_bytes())
    else {
        panic!("redaction expansion must obey the represented payload limit");
    };
    assert_eq!(metadata.reason, "represented_payload_too_large");
}

#[test]
fn duplicate_members_are_classified_at_every_depth() {
    for input in [
        r#"{"same":1,"same":2}"#,
        r#"{"nested":{"same":1,"same":2}}"#,
        r#"[{"same":1,"same":2}]"#,
    ] {
        let PayloadRepresentation::Unrepresentable(metadata) = represent_payload(input.as_bytes())
        else {
            panic!("duplicate members must fail representation");
        };
        assert_eq!(metadata.reason, "duplicate_object_member");
    }
}

#[test]
fn audit_unrepresentable_metadata_and_hash_chain_are_bounded() {
    let run_id = fixed_run_id();
    let input = vec![b'x'; RAW_PAYLOAD_LIMIT + 1];
    let (kind, payload) =
        dolgorae::audit::represent_audit_payload(AuditKind::AppServerResponse, &input);
    assert_eq!(kind, AuditKind::PayloadUnrepresentable);
    let record = AuditRecord::new(
        1,
        "2026-08-21T12:34:56.123456Z",
        run_id,
        0,
        kind,
        payload,
        GENESIS_PREVIOUS_HASH,
    )
    .unwrap();
    assert!(record.verify_hash());
    let line = record.canonical_line().unwrap();
    let value: Value = serde_json::from_slice(&line[..line.len() - 1]).unwrap();
    assert_eq!(value["payload"]["source_kind"], "app_server_response");
    assert_eq!(value["payload"]["raw_sha256"].as_str().unwrap().len(), 64);
    assert!(value["payload"].get("raw_bytes").is_none());
}

#[test]
fn run_publication_is_exclusive_canonical_and_permission_safe() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let manifest = sample_manifest();
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let directory = store.publish(&manifest).unwrap();

    assert_eq!(mode(&directory.root), 0o700);
    assert_eq!(mode(&directory.recovery), 0o700);
    assert_eq!(mode(&directory.manifest), 0o600);
    assert_eq!(mode(&directory.audit), 0o600);
    assert_eq!(fs::read(&directory.audit).unwrap(), b"");
    let bytes = fs::read(&directory.manifest).unwrap();
    assert_eq!(bytes.last(), Some(&b'\n'));
    let body = &bytes[..bytes.len() - 1];
    assert_eq!(
        canonical(std::str::from_utf8(body).unwrap()).as_bytes(),
        body
    );
    let decoded: RunManifest = serde_json::from_slice(body).unwrap();
    assert_eq!(decoded, manifest);
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_CONFLICT"
    );
    assert!(fs::read_dir(state_root.join("runs")).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.')
    }));
}

#[test]
fn manifest_rejects_mutable_or_inconsistent_identity() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);

    let mut manifest = sample_manifest();
    manifest.parent_ref = Some(ParentReference {
        namespace: "external".to_owned(),
        kind: "task".to_owned(),
        id: "42".to_owned(),
    });
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.required_capabilities = vec!["writer".to_owned(), "reader".to_owned()];
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.created_at = "2026-08-21T12:34:56Z".to_owned();
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.profile_capability_snapshot.server_epoch = 0;
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.app_server.actual_codex_home = Some("/tmp/other-home".to_owned());
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.app_server.version = None;
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.controller.identity.kind = ControllerKind::Automation;
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut manifest = sample_manifest();
    manifest.profile.codex_version = "0.148.0".to_owned();
    assert_eq!(
        store.publish(&manifest).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );
}

/// specs.md: "`parent_ref.namespace`, `kind`, and `id` are all-or-none,
/// limited to 128, 64, and 256 UTF-8 bytes, and reject NUL/control
/// characters", and a `direct_interactive` Primary Run "MUST NOT carry a
/// parent reference".  The record layer is the last place that can refuse one,
/// so it keeps exactly the references a Run may hold and no others — and a
/// kept reference has to survive publication, or the Run's own record would
/// disagree with the allocation identity it was reserved under.
#[test]
fn public_parent_metadata_survives_publication_only_within_its_bounds() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);

    let brokered = |parent: ParentReference| {
        let mut manifest = sample_manifest();
        manifest.control_mode = ControlMode::ManagedAgent;
        manifest.controller.identity.kind = ControllerKind::Automation;
        manifest.parent_ref = Some(parent);
        manifest
    };
    let bounded = ParentReference {
        namespace: "external.tracker.v1".to_owned(),
        kind: "task".to_owned(),
        id: "TASK-003".to_owned(),
    };
    let manifest = brokered(bounded.clone());
    store.publish(&manifest).unwrap();
    assert_eq!(
        store.load_manifest(manifest.run_id).unwrap().parent_ref,
        Some(bounded.clone())
    );

    for parent in [
        ParentReference {
            namespace: "n".repeat(129),
            ..bounded.clone()
        },
        ParentReference {
            kind: "k".repeat(65),
            ..bounded.clone()
        },
        // 86 characters, 258 UTF-8 bytes: the bound counts bytes, not chars.
        ParentReference {
            id: "\u{fe0f}".repeat(86),
            ..bounded.clone()
        },
        ParentReference {
            namespace: String::new(),
            ..bounded.clone()
        },
        ParentReference {
            id: "with\u{7}control".to_owned(),
            ..bounded.clone()
        },
        // A reserved namespace names the durable aggregate registry, and only
        // an authoritative aggregate binding may speak for it.
        ParentReference {
            namespace: "dolgorae.orchestrated-session.v1".to_owned(),
            ..bounded.clone()
        },
    ] {
        assert_eq!(
            store.publish(&brokered(parent)).unwrap_err().code,
            "RUN_STATE_INVARIANT_VIOLATION"
        );
    }

    // The prohibition is about the control mode, not about the metadata:
    // the same reference a brokered Run keeps is refused outright here.
    let mut direct = sample_manifest();
    direct.parent_ref = Some(bounded);
    assert_eq!(
        store.publish(&direct).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );
}

fn sample_manifest() -> RunManifest {
    let mut environment = BTreeMap::new();
    environment.insert("LANG".to_owned(), "C".to_owned());
    environment.insert("LC_ALL".to_owned(), "C".to_owned());
    environment.insert("PATH".to_owned(), "/usr/bin:/bin".to_owned());
    let mut capabilities = BTreeMap::new();
    capabilities.insert("reader".to_owned(), CapabilityState::Supported);
    capabilities.insert("writer".to_owned(), CapabilityState::Unavailable);
    let instructions_text = "Do the task.".to_owned();
    let instructions = InstructionSnapshot {
        schema: "dolgorae.instructions/v1".to_owned(),
        common_prefix_version: 1,
        mode_prefix_version: 1,
        purpose_prefix_version: 1,
        normalized_byte_length: instructions_text.len() as u64,
        normalized_sha256: dolgorae::jcs::sha256_hex(instructions_text.as_bytes()),
    };
    let purpose = Purpose {
        kind: PurposeKind::Implementation,
        external_label: Some("TASK-003-A".to_owned()),
    };
    let mut profile = ProfileSnapshot {
        schema_version: 1,
        profile_name: "default".to_owned(),
        canonical_codex_home: "/tmp/codex-home".to_owned(),
        normalized_argv: vec![
            "/usr/local/bin/codex".to_owned(),
            "--strict-config".to_owned(),
        ],
        launch_cwd_policy: "profile_state_directory_v1".to_owned(),
        derived_launch_cwd: "/tmp/dolgorae/profiles/server".to_owned(),
        sanitized_environment: environment,
        enabled_features: vec!["multi_agent".to_owned()],
        disabled_features: vec!["experimental_windows_sandbox".to_owned()],
        process_static_configuration: BTreeMap::new(),
        initial_configuration_observation: BTreeMap::new(),
        executable_identity: ExecutableIdentity {
            resolved_path: LosslessPath::Utf8("/usr/local/bin/codex".to_owned()),
            device: 1,
            inode: 2,
            sha256: "8".repeat(64),
        },
        codex_version: "0.147.0".to_owned(),
        app_server_schema_sha256: "4".repeat(64),
        compatibility_manifest_sha256: "a".repeat(64),
        launch_contract_sha256: "0".repeat(64),
        initial_server_key: "3".repeat(64),
    };
    profile.launch_contract_sha256 = launch_contract_digest(&profile).unwrap();
    let agent_configuration = AgentConfigurationSnapshot {
        schema_version: 1,
        runtime_profile: "default".to_owned(),
        runtime_profile_snapshot_sha256: runtime_profile_snapshot_digest(&profile).unwrap(),
        model: "gpt-5.6".to_owned(),
        default_effort: "high".to_owned(),
        purpose: purpose.clone(),
        required_capabilities: vec!["reader".to_owned()],
        role_reference: None,
        normalized_instructions: instructions_text,
        instructions: instructions.clone(),
        execution_lane: ExecutionLane::SharedReadonly,
        required_assurance: Assurance::BestEffortPersonalAlpha,
        native_subagent_policy: "enabled".to_owned(),
    };
    RunManifest {
        schema_version: 1,
        run_id: fixed_run_id(),
        workspace_id: "1".repeat(64),
        canonical_workspace: LosslessPath::Utf8("/tmp/workspace".to_owned()),
        workspace_mode: WorkspaceMode::Git,
        start_baseline: GitBaseline {
            head: Some("2".repeat(40)),
            branch: Some("main".to_owned()),
            tracked_changes: Vec::new(),
            untracked_paths: vec![LosslessPath::Utf8("notes.txt".to_owned())],
        },
        created_at: "2026-08-21T12:34:56.123456Z".to_owned(),
        initial_access: Access::Read,
        control_mode: ControlMode::DirectInteractive,
        execution_lane: ExecutionLane::SharedReadonly,
        requested_assurance: Assurance::BestEffortPersonalAlpha,
        achieved_assurance: Assurance::BestEffortPersonalAlpha,
        profile,
        agent_configuration,
        profile_capability_snapshot: ProfileCapabilitySnapshot {
            schema_version: 1,
            profile_name: "default".to_owned(),
            server_key: "3".repeat(64),
            server_epoch: 1,
            app_server_version: "0.147.0".to_owned(),
            schema_sha256: "4".repeat(64),
            capabilities,
        },
        app_server: AppServerFacts {
            version: Some("0.147.0".to_owned()),
            schema_status: Some("accepted".to_owned()),
            actual_codex_home: Some("/tmp/codex-home".to_owned()),
        },
        dolgorae: DolgoraeBuild {
            version: env!("CARGO_PKG_VERSION").to_owned(),
            binary_sha256: "5".repeat(64),
            ipc_protocol_version: 1,
        },
        model: "gpt-5.6".to_owned(),
        initial_reasoning_effort: "high".to_owned(),
        default_reasoning_effort: "high".to_owned(),
        instructions,
        controller: ControllerBinding {
            identity: ControllerIdentity {
                controller_id: Uuid::parse_str("018f0c6a-7b01-7def-8abc-0123456789ab").unwrap(),
                kind: ControllerKind::HumanCli,
                instance_id: "cli".to_owned(),
                subject_id: None,
                generation: 1,
            },
            capability_sha256: controller_capability_digest(&[9_u8; 32]),
        },
        purpose,
        parent_ref: None,
        required_capabilities: vec!["reader".to_owned()],
        thread_id: None,
        fork_provenance: None,
        aggregate_binding: None,
        audit: AuditPolicy::default(),
        compatibility: CompatibilityVerdict::Accepted,
    }
}

fn fixed_run_id() -> Uuid {
    Uuid::parse_str("018f0c6a-7b01-7abc-8def-0123456789ab").unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
}

fn make_dir(path: &Path) {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

struct TestTree {
    root: PathBuf,
}

impl TestTree {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("dolgorae-task003a-{}", Uuid::now_v7()));
        make_dir(&root);
        Self { root }
    }

    fn path(&self, relative: &str) -> PathBuf {
        self.root.join(relative)
    }
}

impl Drop for TestTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// specs.md: "Run allocation reserves its key before publishing a Run ...
/// Response loss is reconciled by retrying the identical allocation key, which
/// returns the original Run; changed normalized input is
/// `IDEMPOTENCY_CONFLICT` and can never allocate another Run under that key."
#[test]
fn one_allocation_key_binds_one_run_across_response_loss_and_input_drift() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    assert!(store.load("k1").unwrap().is_none());

    let run_id = Uuid::now_v7();
    let reservation = StartReservation {
        schema_version: 1,
        operation: "start_run".to_owned(),
        idempotency_key: "k1".to_owned(),
        normalized_identity_sha256: "a".repeat(64),
        run_id,
    };
    assert_eq!(store.reserve(&reservation).unwrap(), reservation);

    // The reservation is durable before any Run is published, so a caller that
    // lost the response — or crashed before publishing — resolves back to the
    // same Run identity rather than allocating a second one.
    let mut second = reservation.clone();
    second.run_id = Uuid::now_v7();
    assert_eq!(store.reserve(&second).unwrap().run_id, run_id);
    assert_eq!(store.load("k1").unwrap().unwrap(), reservation);

    // Reused with different normalized input, the key still names the original
    // Run and never allocates another.
    let mut drifted = reservation.clone();
    drifted.normalized_identity_sha256 = "b".repeat(64);
    drifted.run_id = Uuid::now_v7();
    let held = store.reserve(&drifted).unwrap();
    assert_eq!(held.run_id, run_id);
    assert_eq!(held.normalized_identity_sha256, "a".repeat(64));

    // The key never becomes a pathname; the index is named by its digest.
    let index = state_root.join("idempotency").join("run-start");
    let files: Vec<String> = fs::read_dir(&index)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(files.len(), 1);
    assert!(!files[0].contains("k1"), "{files:?}");
    assert_eq!(
        fs::symlink_metadata(index.join(&files[0]))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let mut other = reservation.clone();
    other.idempotency_key = "k2".to_owned();
    other.run_id = Uuid::now_v7();
    assert_eq!(store.reserve(&other).unwrap(), other);
    assert_eq!(store.load("k1").unwrap().unwrap().run_id, run_id);
}

/// The checked error contract closes `RUN_STATE_INVARIANT_VIOLATION`'s reason
/// vocabulary and requires the same recovery action of every emission, and it
/// requires `RUN_STATE_CONFLICT` to name the state and operation it refused.
#[test]
fn run_state_refusals_carry_exactly_the_members_their_contract_requires() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);

    let mut manifest = sample_manifest();
    manifest.achieved_assurance = Assurance::BestEffortPersonalAlpha;
    manifest.requested_assurance = Assurance::StrongProcessContainment;
    let invariant = store.publish(&manifest).unwrap_err();
    assert_eq!(invariant.code, "RUN_STATE_INVARIANT_VIOLATION");
    assert_eq!(invariant.details["reason"], "assurance_order_invalid");
    assert_eq!(invariant.details["required_action"], "reconcile_run_state");
    assert_eq!(
        invariant.details["run_id"],
        serde_json::json!(manifest.run_id)
    );
    assert_eq!(
        invariant.details.as_object().unwrap().len(),
        3,
        "the contract forbids members it does not name"
    );

    let mut manifest = sample_manifest();
    manifest.profile_capability_snapshot.server_epoch = 0;
    let epoch = store.publish(&manifest).unwrap_err();
    assert_eq!(epoch.details["reason"], "shared_server_epoch_mismatch");

    let manifest = sample_manifest();
    store.publish(&manifest).unwrap();
    let conflict = store.publish(&manifest).unwrap_err();
    assert_eq!(conflict.code, "RUN_STATE_CONFLICT");
    assert_eq!(
        conflict.details["run_id"],
        serde_json::json!(manifest.run_id)
    );
    assert_eq!(conflict.details["state"], "starting");
    assert_eq!(conflict.details["operation"], "run.start");
}
