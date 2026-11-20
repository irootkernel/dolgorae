use dolgorae::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
use dolgorae::domain::{
    Access, AggregateKind, Assurance, ControlMode, ControllerIdentity, ControllerKind,
    ExecutionLane, Purpose, PurposeKind,
};
use dolgorae::global_runtime::ResolvedGlobalProfile;
use dolgorae::jcs::{
    PayloadRepresentation, RAW_PAYLOAD_LIMIT, canonicalize, parse, represent_payload,
};
use dolgorae::profile::{
    CompatibilityVerdict as ProfileCompatibilityVerdict,
    ExecutableIdentity as ProfileExecutableIdentity, ProfileSnapshot as ServerProfileSnapshot,
};
use dolgorae::projection::RunStateProjection;
use dolgorae::run::{
    AgentConfigurationSnapshot, AggregateBinding, AggregateMemberKind, AppServerFacts, AuditPolicy,
    CallerStartIntent, CallerStartReservation, CapabilityState, CompatibilityVerdict,
    ControllerBinding, DolgoraeBuild, ExecutableIdentity, ForkProvenance, InstructionSnapshot,
    ParentReference, ProfileCapabilitySnapshot, ProfileSnapshot, RunManifest, RunStore,
    StartReservation, StartReservationStore, WriteContinuationProvenance,
    agent_configuration_digest, caller_start_request_digest, controller_capability_digest,
    launch_contract_digest, runtime_profile_snapshot_digest,
};
use dolgorae::workspace::{
    GitBaseline, LosslessPath, NativeSubagents, RuntimeProfile, SystemWorkspacePlatform,
    WorkspaceMode,
};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[path = "run_snapshot_consistency.rs"]
mod snapshot_consistency;

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
fn snapshot_binds_current_controller_and_socket_epoch_to_its_captured_sources() {
    use dolgorae::snapshot::RunSnapshot;
    use dolgorae::worker::{SocketIdentity, WorkerIdentity, WorkerRuntimeRecord};
    let tree = TestTree::new();
    let state_root = tree.path("state");
    for path in [
        state_root.clone(),
        state_root.join("runs"),
        state_root.join("runtime"),
        state_root.join("runtime/locks"),
        state_root.join("runtime/runs"),
    ] {
        make_dir(&path);
    }
    let manifest = sample_manifest();
    let uid = dolgorae::darwin::DarwinSystem.current_uid();
    dolgorae::writer::WriterStore::initialize_layout(&state_root, &manifest.workspace_id, uid)
        .unwrap();
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let directory = store.publish(&manifest).unwrap();
    let without_runtime = RunSnapshot::load_display(&state_root, manifest.run_id, 999).unwrap();
    assert_eq!(without_runtime.control_socket_epoch, 1);
    assert_eq!(
        without_runtime.controller_authority().unwrap(),
        &manifest.controller
    );
    let mut runtime = WorkerRuntimeRecord {
        schema_version: 1,
        identity: WorkerIdentity {
            workspace_id: manifest.workspace_id.clone(),
            run_id: manifest.run_id,
            run_generation: 1,
            boot_uuid: Uuid::now_v7(),
            pid: 42,
            process_group_id: 42,
            session_id: 42,
            uid,
            start_tvsec: 1,
            start_tvusec: 0,
            executable_path: PathBuf::from("/isolated-fixture/worker"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "a".repeat(64),
        },
        socket_path: dolgorae::worker::worker_socket_path(
            uid,
            &manifest.workspace_id,
            manifest.run_id,
        )
        .unwrap(),
        socket_identity: SocketIdentity {
            device: 1,
            inode: 1,
        },
        control_socket_epoch: 41,
        app_server_epoch: None,
        control_recovery_required: false,
        startup_scope: None,
        dedicated_server_identity: None,
        dolgorae_version: env!("CARGO_PKG_VERSION").to_owned(),
        mutation_protocol_version: dolgorae::worker::WORKER_PROTOCOL_VERSION,
        binary_sha256: "a".repeat(64),
    };
    // This is a checked observation DTO fixture, not live process or absence evidence.
    let runtime_path = dolgorae::worker::runtime_record_path(
        &dolgorae::worker::runtime_root(&state_root),
        manifest.run_id,
    )
    .unwrap();
    dolgorae::worker::write_runtime_record(&runtime_path, &runtime).unwrap();
    let first = RunSnapshot::load_display(&state_root, manifest.run_id, 999).unwrap();
    assert_eq!(
        first.control_socket_epoch,
        first.runtime_record.as_ref().unwrap().control_socket_epoch
    );
    assert_eq!(first.control_socket_epoch, 41);
    let binding_path = directory.root.join("controller.json");
    let mut current_binding = manifest.controller.clone();
    current_binding.identity.generation += 1;
    fs::write(&binding_path, serde_json::to_vec(&current_binding).unwrap()).unwrap();
    runtime.control_socket_epoch = 43;
    dolgorae::worker::write_runtime_record(&runtime_path, &runtime).unwrap();
    let second =
        RunSnapshot::load_display(&state_root, manifest.run_id, first.control_socket_epoch)
            .unwrap();
    assert_eq!(second.control_socket_epoch, 43);
    assert_eq!(second.controller_authority().unwrap(), &current_binding);
    assert_eq!(
        store.load_controller_binding(manifest.run_id).unwrap(),
        current_binding
    );
    assert_eq!(second.manifest.controller, manifest.controller);
    assert_eq!(
        second.stamp.run_state_revision,
        first.stamp.run_state_revision
    );
    assert!(
        second
            .require_revision(second.stamp.run_state_revision, "run.pause")
            .is_err()
    );
    // The ordinary fresh loader keeps the caller-provided epoch contract.
    assert_eq!(
        RunSnapshot::load(&state_root, manifest.run_id, 77)
            .unwrap()
            .control_socket_epoch,
        77
    );
    let mut regressed = current_binding.clone();
    regressed.identity.generation = 0;
    fs::write(&binding_path, serde_json::to_vec(&regressed).unwrap()).unwrap();
    assert_eq!(
        store
            .load_controller_binding(manifest.run_id)
            .unwrap_err()
            .code,
        RunSnapshot::load_display(&state_root, manifest.run_id, 0)
            .unwrap_err()
            .code
    );
    fs::write(&binding_path, serde_json::to_vec(&current_binding).unwrap()).unwrap();
    fs::set_permissions(&binding_path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.load_controller_binding(manifest.run_id).is_err());
    assert!(RunSnapshot::load_display(&state_root, manifest.run_id, 0).is_err());
    fs::set_permissions(&binding_path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::remove_file(binding_path).unwrap();
    assert_eq!(
        store.load_controller_binding(manifest.run_id).unwrap(),
        manifest.controller
    );
    assert_eq!(
        RunSnapshot::load_display(&state_root, manifest.run_id, 0)
            .unwrap()
            .controller_authority()
            .unwrap(),
        &manifest.controller
    );
}

#[test]
fn snapshot_preserves_audit_integrity_failure_for_committed_corruption() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    for path in [
        state_root.clone(),
        state_root.join("runs"),
        state_root.join("runtime"),
        state_root.join("runtime/locks"),
    ] {
        make_dir(&path);
    }
    let manifest = sample_manifest();
    dolgorae::writer::WriterStore::initialize_layout(
        &state_root,
        &manifest.workspace_id,
        dolgorae::darwin::DarwinSystem.current_uid(),
    )
    .unwrap();
    let directory = RunStore::new(SystemWorkspacePlatform, &state_root)
        .publish(&manifest)
        .unwrap();
    let record = AuditRecord::new(
        1,
        "2026-08-21T12:34:56.123456Z",
        manifest.run_id,
        0,
        AuditKind::RunCreated,
        parse("{}").unwrap(),
        GENESIS_PREVIOUS_HASH,
    )
    .unwrap();
    let valid = record.canonical_line().unwrap();
    let mut projection = RunStateProjection::starting(manifest.run_id);
    projection.ledger_head.sequence = 1;
    projection.ledger_head.hash = record.hash().to_owned();
    let state = directory.root.join("state.json");
    fs::write(&state, serde_json::to_vec(&projection).unwrap()).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&directory.audit, &valid).unwrap();
    dolgorae::snapshot::RunSnapshot::load(&state_root, manifest.run_id, 0).unwrap();
    let original: Value = serde_json::from_slice(&valid).unwrap();
    for field in ["structure", "sequence", "hash"] {
        let mut corrupt = original.clone();
        match field {
            "structure" => corrupt.as_object_mut().unwrap().remove("kind"),
            "sequence" => corrupt
                .as_object_mut()
                .unwrap()
                .insert("sequence".to_owned(), 2.into()),
            "hash" => corrupt
                .as_object_mut()
                .unwrap()
                .insert("previous_hash".to_owned(), "invalid".into()),
            _ => unreachable!(),
        };
        let mut bytes = serde_json::to_vec(&corrupt).unwrap();
        bytes.push(b'\n');
        fs::write(&directory.audit, bytes).unwrap();
        let error =
            dolgorae::snapshot::RunSnapshot::load(&state_root, manifest.run_id, 0).unwrap_err();
        assert_eq!(error.code, "AUDIT_INTEGRITY_FAILURE", "{field}");
        assert_eq!(dolgorae::machine::exit_status_for(&error.code), 8);
        assert!(!error.retryable);
    }
    fs::write(&directory.audit, valid).unwrap();
    dolgorae::snapshot::RunSnapshot::load(&state_root, manifest.run_id, 0).unwrap();
}

#[test]
fn global_profile_manifest_v2_is_complete_and_recoverable_without_registry() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let manifest = sample_global_manifest();
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    store.publish(&manifest).unwrap();
    let recovered = store.load_manifest(manifest.run_id).unwrap();
    recovered
        .global_profile_binding
        .as_ref()
        .unwrap()
        .validate_for_recovery()
        .unwrap();

    let manifest_path = state_root
        .join("runs")
        .join(manifest.run_id.to_string())
        .join("manifest.json");
    let original = fs::read(&manifest_path).unwrap();
    let reject = |invalid: RunManifest, message: &str| {
        let error = store.publish(&invalid).unwrap_err();
        assert_eq!(error.code, "RUN_STATE_INVARIANT_VIOLATION");
        assert_eq!(error.message, message);
        assert_eq!(fs::read(&manifest_path).unwrap(), original);
        fs::write(&manifest_path, serde_json::to_vec(&invalid).unwrap()).unwrap();
        let error = store.load_manifest(manifest.run_id).unwrap_err();
        assert_eq!(error.code, "RUN_STATE_INVARIANT_VIOLATION");
        assert_eq!(error.message, message);
        fs::write(&manifest_path, &original).unwrap();
        store.load_manifest(manifest.run_id).unwrap();
    };
    let mut legacy_with_binding = manifest.clone();
    legacy_with_binding.schema_version = 1;
    legacy_with_binding.agent_configuration.schema_version = 1;
    reject(
        legacy_with_binding,
        "schema v1 excludes and schema v2 requires a global Profile binding",
    );
    let mut missing_binding = manifest.clone();
    missing_binding.global_profile_binding = None;
    reject(
        missing_binding,
        "schema v1 excludes and schema v2 requires a global Profile binding",
    );
    for mismatch_name in [true, false] {
        let mut mismatched = manifest.clone();
        let binding = manifest.global_profile_binding.as_ref().unwrap();
        let mut snapshot = binding.launch_snapshot.clone();
        if mismatch_name {
            snapshot.profile_name = "other".to_owned();
        } else {
            snapshot.server_key = "e".repeat(64);
        }
        let binding = ResolvedGlobalProfile::from_definition(
            &snapshot.profile_name,
            binding.definition.clone(),
        )
        .unwrap()
        .bind(snapshot)
        .unwrap();
        binding.validate_for_recovery().unwrap();
        mismatched.global_profile_binding = Some(binding);
        reject(
            mismatched,
            "global Profile binding disagrees with the Run profile",
        );
    }
}

#[test]
fn absent_run_directory_is_not_found_without_weakening_path_checks() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let run_id = Uuid::now_v7();

    let error = store.load_manifest(run_id).unwrap_err();
    assert_eq!(error.code, "RUN_NOT_FOUND");
    assert_eq!(error.details, serde_json::json!({"run_id": run_id}));

    let linked_run_id = Uuid::now_v7();
    let linked_target = tree.path("linked-run");
    make_dir(&linked_target);
    std::os::unix::fs::symlink(
        &linked_target,
        state_root.join("runs").join(linked_run_id.to_string()),
    )
    .unwrap();
    assert_eq!(
        store.load_manifest(linked_run_id).unwrap_err().code,
        "RUNTIME_PATH_INVALID"
    );
}

#[test]
fn run_store_owns_external_reviewer_thread_lookup() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let mut manifest = sample_manifest();
    manifest.control_mode = ControlMode::ManagedAgent;
    manifest.controller.identity.kind = ControllerKind::Automation;
    manifest.purpose = Purpose {
        kind: PurposeKind::Review,
        external_label: None,
    };
    manifest.agent_configuration.purpose = manifest.purpose.clone();
    manifest.agent_configuration.role_reference = Some("independent-reviewer-v1".to_owned());
    let aggregate_id = Uuid::now_v7();
    manifest.parent_ref = Some(ParentReference {
        namespace: "dolgorae.external-specialist-engagement.v1".to_owned(),
        kind: "specialist".to_owned(),
        id: aggregate_id.to_string(),
    });
    manifest.aggregate_binding = Some(AggregateBinding {
        aggregate_kind: AggregateKind::ExternalSpecialistEngagement,
        aggregate_id,
        operation_id: Uuid::now_v7(),
        member_kind: AggregateMemberKind::Specialist,
        policy_sha256: None,
        role_reference: manifest.agent_configuration.role_reference.clone(),
        role_snapshot_sha256: Some("7".repeat(64)),
        agent_configuration_sha256: Some(
            agent_configuration_digest(&manifest.agent_configuration).unwrap(),
        ),
    });
    let directory = store.publish(&manifest).unwrap();
    let mut projection = RunStateProjection::starting(manifest.run_id);
    projection.thread_id = Some("thread-reviewer".to_owned());
    let state = directory.root.join("state.json");
    fs::write(&state, serde_json::to_vec(&projection).unwrap()).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();

    assert!(store.reviewer_thread_registered("thread-reviewer").unwrap());
    assert!(
        store
            .external_specialist_thread_registered("thread-reviewer")
            .unwrap()
    );
    assert!(!store.reviewer_thread_registered("thread-parent").unwrap());

    let mut researcher = manifest.clone();
    researcher.run_id = Uuid::now_v7();
    researcher.purpose = Purpose {
        kind: PurposeKind::Research,
        external_label: None,
    };
    researcher.agent_configuration.purpose = researcher.purpose.clone();
    researcher.agent_configuration.role_reference = Some("researcher".to_owned());
    researcher.aggregate_binding.as_mut().unwrap().operation_id = Uuid::now_v7();
    researcher
        .aggregate_binding
        .as_mut()
        .unwrap()
        .role_reference = researcher.agent_configuration.role_reference.clone();
    researcher
        .aggregate_binding
        .as_mut()
        .unwrap()
        .agent_configuration_sha256 =
        Some(agent_configuration_digest(&researcher.agent_configuration).unwrap());
    let directory = store.publish(&researcher).unwrap();
    let mut projection = RunStateProjection::starting(researcher.run_id);
    projection.thread_id = Some("thread-researcher".to_owned());
    let state = directory.root.join("state.json");
    fs::write(&state, serde_json::to_vec(&projection).unwrap()).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(
        store
            .external_specialist_thread_registered("thread-researcher")
            .unwrap()
    );
    assert!(
        !store
            .reviewer_thread_registered("thread-researcher")
            .unwrap()
    );
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

/// docs/specs/README.md: "`parent_ref.namespace`, `kind`, and `id` are all-or-none,
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

#[test]
fn write_continuation_lineage_is_immutable_complete_and_dedicated() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let mut manifest = sample_manifest();
    manifest.execution_lane = ExecutionLane::Dedicated;
    manifest.agent_configuration.execution_lane = ExecutionLane::Dedicated;
    manifest.write_continuation_provenance = Some(WriteContinuationProvenance {
        source_run_id: Uuid::now_v7(),
        source_turn_id: "turn-source".to_owned(),
        source_thread_id: "thread-source".to_owned(),
        creation_reason: "shared_readonly_source".to_owned(),
        source_controller_kind: "human_cli".to_owned(),
        destination_controller_kind: "human_cli".to_owned(),
        handoff_summary_sha256: Some("a".repeat(64)),
        artifact_refs: vec![Uuid::now_v7()],
        workspace_baseline_sha256: "b".repeat(64),
        created_at: "2026-09-03T00:00:00.000000Z".to_owned(),
    });
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    store.publish(&manifest).unwrap();
    assert_eq!(
        store
            .load_manifest(manifest.run_id)
            .unwrap()
            .write_continuation_provenance,
        manifest.write_continuation_provenance
    );

    let mut invalid = manifest;
    invalid.run_id = Uuid::now_v7();
    invalid.execution_lane = ExecutionLane::SharedReadonly;
    invalid.agent_configuration.execution_lane = ExecutionLane::SharedReadonly;
    assert_eq!(
        store.publish(&invalid).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );
}

#[test]
fn fork_lineage_requires_mode_specific_boundaries_and_a_distinct_source() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);

    let mut history_copy = sample_manifest();
    history_copy.fork_provenance = Some(ForkProvenance {
        source_run_id: Uuid::now_v7(),
        mode: "history_copy".to_owned(),
        source_turn_id: Some("turn-source".to_owned()),
        source_thread_id: Some("thread-source".to_owned()),
        last_confirmed_boundary: Some("completed".to_owned()),
        observed_source_lifecycle: "paused".to_owned(),
        unresolved_turn_id: None,
    });
    store.publish(&history_copy).unwrap();

    let mut fresh = sample_manifest();
    fresh.run_id = Uuid::now_v7();
    fresh.fork_provenance = Some(ForkProvenance {
        source_run_id: Uuid::now_v7(),
        mode: "fresh".to_owned(),
        source_turn_id: None,
        source_thread_id: None,
        last_confirmed_boundary: None,
        observed_source_lifecycle: "outcome_unknown".to_owned(),
        unresolved_turn_id: Some("turn-unresolved".to_owned()),
    });
    store.publish(&fresh).unwrap();

    let mut incomplete = sample_manifest();
    incomplete.run_id = Uuid::now_v7();
    incomplete.fork_provenance = Some(ForkProvenance {
        source_run_id: Uuid::now_v7(),
        mode: "history_copy".to_owned(),
        source_turn_id: None,
        source_thread_id: Some("thread-source".to_owned()),
        last_confirmed_boundary: Some("completed".to_owned()),
        observed_source_lifecycle: "paused".to_owned(),
        unresolved_turn_id: None,
    });
    assert_eq!(
        store.publish(&incomplete).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    let mut self_referential = sample_manifest();
    self_referential.run_id = Uuid::now_v7();
    self_referential.fork_provenance = Some(ForkProvenance {
        source_run_id: self_referential.run_id,
        mode: "fresh".to_owned(),
        source_turn_id: None,
        source_thread_id: None,
        last_confirmed_boundary: None,
        observed_source_lifecycle: "outcome_unknown".to_owned(),
        unresolved_turn_id: None,
    });
    assert_eq!(
        store.publish(&self_referential).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );
}

#[test]
fn caller_manifest_recovery_retains_exact_captures_and_startup_selection() {
    use dolgorae::domain::CallerWorkspaceAccess;
    for access in [
        CallerWorkspaceAccess::ReadOnly,
        CallerWorkspaceAccess::Writable,
    ] {
        let tree = TestTree::new();
        let state_root = tree.path("state");
        make_dir(&state_root);
        make_dir(&state_root.join("runs"));
        let manifest = sample_caller_manifest(access);
        let store = RunStore::new(SystemWorkspacePlatform, &state_root);
        let directory = store.publish(&manifest).unwrap();
        make_dir(&state_root.join("runtime"));
        make_dir(&state_root.join("runtime/locks"));
        use dolgorae::workspace::WorkspacePlatform as _;
        dolgorae::writer::WriterStore::initialize_layout(
            &state_root,
            &manifest.workspace_id,
            SystemWorkspacePlatform.current_uid(),
        )
        .unwrap();
        let persisted: Value =
            serde_json::from_slice(&fs::read(&directory.manifest).unwrap()).unwrap();
        assert_eq!(persisted["schema_version"], 3);
        assert!(
            persisted["agent_configuration"]
                .get("normalized_instructions")
                .is_none()
        );
        assert_eq!(
            persisted["instructions"],
            persisted["agent_configuration"]["instructions"]
        );
        let recovered = store.load_manifest(manifest.run_id).unwrap();
        assert_eq!(recovered, manifest);
        let capture = recovered.instructions.caller().unwrap();
        assert_eq!(capture.common.body, "공통 지침\r\n");
        assert_eq!(capture.role.body, "Implement the current task.\n");
        assert_eq!(
            capture.developer_prefix().unwrap(),
            recovered.agent_configuration.normalized_instructions
        );
        assert_eq!(recovered.requested_access, Some(access));
        assert!(recovered.thread_id.is_none());
        // Startup provenance is retained without establishing current write access.
        let (snapshot, observation) =
            dolgorae::snapshot::RunSnapshot::observe(&state_root, manifest.run_id, 0).unwrap();
        assert_eq!(observation.effective_policy.access, Access::Unknown);
        assert_eq!(
            observation.effective_policy.verification,
            dolgorae::domain::PolicyVerification::Unverified
        );
        assert!(dolgorae::gateway_projection::configuration(&snapshot).is_err());
    }
}

fn sample_delegation_manifest(access: dolgorae::domain::CallerWorkspaceAccess) -> RunManifest {
    use dolgorae::delegation_identity::{InstructionSnapshot, NamedSourceCapture};
    let mut manifest = sample_caller_manifest(access);
    let old = manifest.instructions.caller().unwrap();
    let capture = InstructionSnapshot::compose(
        old.common.clone(),
        old.role.clone(),
        vec![NamedSourceCapture::new("contract".to_owned(), "Contract bytes\r\n").unwrap()],
        vec![NamedSourceCapture::new("project".to_owned(), "Explicit guidance\n").unwrap()],
    )
    .unwrap();
    let mut capabilities = manifest.agent_configuration.required_capabilities.clone();
    capabilities.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    let admitted = dolgorae::delegation_identity::AdmittedConfiguration {
        schema: dolgorae::delegation_identity::CONFIGURATION_SCHEMA.to_owned(),
        profile_binding_schema: dolgorae::run::ADMITTED_PROFILE_BINDING_SCHEMA.to_owned(),
        profile_binding_digest: format!(
            "sha256:{}",
            manifest
                .global_profile_binding
                .as_ref()
                .unwrap()
                .digest()
                .unwrap()
        ),
        model: manifest.agent_configuration.model.clone(),
        effort: manifest.agent_configuration.default_effort.clone(),
        purpose_kind: manifest
            .agent_configuration
            .purpose
            .kind
            .as_str()
            .to_owned(),
        purpose_label: manifest.agent_configuration.purpose.external_label.clone(),
        required_capabilities: capabilities,
        execution_lane: manifest
            .agent_configuration
            .execution_lane
            .as_str()
            .to_owned(),
        requested_access: match access {
            dolgorae::domain::CallerWorkspaceAccess::ReadOnly => "read_only",
            dolgorae::domain::CallerWorkspaceAccess::Writable => "canonical_workspace_write",
        }
        .to_owned(),
        required_assurance: manifest
            .agent_configuration
            .required_assurance
            .as_str()
            .to_owned(),
        native_subagent_policy: manifest.agent_configuration.native_subagent_policy.clone(),
        instruction_identity_digest: capture.instruction_identity_digest.clone(),
    };
    let profile = manifest.agent_configuration.runtime_profile.clone();
    let binding = manifest.global_profile_binding.clone().unwrap();
    assert_eq!(binding.selected_name, profile);
    manifest.agent_configuration = dolgorae::run::AgentConfigurationSnapshot::from_admitted(
        dolgorae::run::AdmittedConfigurationInput {
            admitted,
            instructions: capture.clone(),
            profile: &profile,
            profile_binding: &binding,
            role_reference: manifest.agent_configuration.role_reference.as_deref(),
        },
    )
    .unwrap();
    manifest.schema_version = 4;
    manifest.instructions = dolgorae::run::RunInstructionSnapshot::Delegation(Box::new(capture));
    manifest.creation_mode = Some(
        if access == dolgorae::domain::CallerWorkspaceAccess::Writable {
            "writable"
        } else {
            "read_only"
        }
        .to_owned(),
    );
    manifest.admitted_configuration_digest = Some(
        manifest
            .agent_configuration
            .admitted_configuration
            .as_ref()
            .unwrap()
            .digest()
            .unwrap(),
    );
    manifest.start_operation_receipt = Some(None);
    manifest
}

fn start_receipt(manifest: &RunManifest, scope: Option<(&str, &str)>) -> Value {
    let namespace = serde_json::json!({
        "controller_principal": "principal:fixture",
        "controller_generation": 1,
        "workspace_ref": manifest.workspace_id,
        "scope_kind": scope.map(|pair| pair.0),
        "scope_ref": scope.map(|pair| pair.1),
        "operation_kind": "start_run",
        "idempotency_key": "start-original-key",
    });
    let subjects = serde_json::json!({
        "run_ref": manifest.run_id.to_string(),
        "member_ref": null,
        "task_ref": null,
        "group_ref": null,
        "result_ref": null,
        "merge_ref": null,
        "engagement_ref": null,
        "maintenance_ref": null,
        "provisioning_ref": null,
        "turn_ref": null,
        "interaction_ref": null,
    });
    serde_json::json!({
        "schema": "dolgorae.operation-receipt/v1",
        "operation_ref": Uuid::now_v7(),
        "namespace": namespace,
        "accepted_input_digest": format!("sha256:{}", "a".repeat(64)),
        "accepted_subjects": subjects,
        "receipt_digest": format!("sha256:{}", "b".repeat(64)),
        "accepted_revision": 1,
        "result_digest": null,
    })
}

#[test]
fn delegation_manifest_accepts_one_explicit_preallocation_start_receipt() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let manifest = sample_delegation_manifest(dolgorae::domain::CallerWorkspaceAccess::ReadOnly);
    // `Some(None)` states that no public receipt was accepted yet.
    assert!(manifest.start_operation_receipt.is_some());
    assert!(manifest.start_operation_receipt.clone().unwrap().is_none());
    store.publish(&manifest).unwrap();

    let mut accepted = manifest.clone();
    accepted.run_id = Uuid::now_v7();
    accepted.start_operation_receipt = Some(Some(
        serde_json::from_value(start_receipt(&accepted, None)).unwrap(),
    ));
    let directory = store.publish(&accepted).unwrap();
    assert_eq!(store.load_manifest(accepted.run_id).unwrap(), accepted);
    let wire: Value = serde_json::from_slice(&fs::read(&directory.manifest).unwrap()).unwrap();
    assert!(wire["start_operation_receipt"].is_object());

    // A pre-allocation start receipt must not claim an aggregate scope.
    let mut scoped = manifest.clone();
    scoped.run_id = Uuid::now_v7();
    scoped.start_operation_receipt = Some(Some(
        serde_json::from_value(start_receipt(&scoped, Some(("run", "run:fixture")))).unwrap(),
    ));
    assert_eq!(
        store.publish(&scoped).unwrap_err().code,
        "RUN_STATE_INVARIANT_VIOLATION"
    );

    // A receipt for another Run, workspace or operation is refused.
    for (field, value) in [
        (
            "run",
            serde_json::json!({"run_ref": Uuid::now_v7().to_string()}),
        ),
        (
            "workspace",
            serde_json::json!({"workspace_ref": "workspace:other"}),
        ),
        ("kind", serde_json::json!({"operation_kind": "submit_turn"})),
    ] {
        let mut changed = manifest.clone();
        changed.run_id = Uuid::now_v7();
        let mut receipt = start_receipt(&changed, None);
        match field {
            "run" => receipt["accepted_subjects"]["run_ref"] = value["run_ref"].clone(),
            _ => {
                for (key, item) in value.as_object().unwrap() {
                    receipt["namespace"][key] = item.clone();
                }
            }
        }
        changed.start_operation_receipt = Some(Some(serde_json::from_value(receipt).unwrap()));
        assert!(store.publish(&changed).is_err(), "accepted changed {field}");
    }

    // The receipt field itself must be explicit in a generation-4 manifest: an
    // omitted key deserializes to the absent state and is refused on publish.
    let mut missing = serde_json::to_value(&manifest).unwrap();
    missing
        .as_object_mut()
        .unwrap()
        .remove("start_operation_receipt");
    let omitted: RunManifest = serde_json::from_value(missing).unwrap();
    assert!(omitted.start_operation_receipt.is_none());
    assert!(store.publish(&omitted).is_err());
}

#[test]
fn delegation_manifest_retains_exact_sources_and_explicit_creation_identity() {
    use dolgorae::domain::CallerWorkspaceAccess;
    for access in [
        CallerWorkspaceAccess::ReadOnly,
        CallerWorkspaceAccess::Writable,
    ] {
        let tree = TestTree::new();
        let state_root = tree.path("state");
        make_dir(&state_root);
        make_dir(&state_root.join("runs"));
        let store = RunStore::new(SystemWorkspacePlatform, &state_root);
        let manifest = sample_delegation_manifest(access);
        assert!(manifest.uses_caller_workspace_lane());
        let directory = store.publish(&manifest).unwrap();
        assert_eq!(store.load_manifest(manifest.run_id).unwrap(), manifest);
        let wire: Value = serde_json::from_slice(&fs::read(&directory.manifest).unwrap()).unwrap();
        assert!(wire.get("start_operation_receipt").unwrap().is_null());
        assert_eq!(
            wire["instructions"],
            wire["agent_configuration"]["instructions"]
        );
        assert_eq!(
            manifest.instructions.delegation().unwrap().dependencies[0]
                .source
                .body,
            "Contract bytes\r\n"
        );
        for pointer in [
            "/creation_mode",
            "/admitted_configuration_digest",
            "/instructions/dependencies/0/source/body",
            "/agent_configuration/admitted_configuration/model",
        ] {
            let mut changed = wire.clone();
            *changed.pointer_mut(pointer).unwrap() = serde_json::json!("changed");
            fs::write(&directory.manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                store.load_manifest(manifest.run_id).is_err(),
                "accepted altered {pointer}"
            );
        }
        for field in [
            "creation_mode",
            "admitted_configuration_digest",
            "start_operation_receipt",
            "parent_ref",
            "thread_id",
        ] {
            let mut changed = wire.clone();
            changed.as_object_mut().unwrap().remove(field);
            fs::write(&directory.manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(
                store.load_manifest(manifest.run_id).is_err(),
                "accepted absent {field}"
            );
        }
        let mut changed = wire.clone();
        changed["schema_version"] = serde_json::json!(3);
        fs::write(&directory.manifest, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(store.load_manifest(manifest.run_id).is_err());
    }
}

#[test]
fn caller_manifest_rejects_changed_capture_generation_and_startup_identity() {
    use dolgorae::domain::CallerWorkspaceAccess;
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    make_dir(&state_root.join("runs"));
    let manifest = sample_caller_manifest(CallerWorkspaceAccess::Writable);
    let store = RunStore::new(SystemWorkspacePlatform, &state_root);
    let path = store.publish(&manifest).unwrap().manifest;
    let original = fs::read(&path).unwrap();
    let value: Value = serde_json::from_slice(&original).unwrap();
    for pointer in [
        "/instructions/common/body",
        "/instructions/role/sha256",
        "/instructions/core_prefix_version",
        "/agent_configuration/instructions/common/byte_length",
        "/selected_role",
        "/requested_access",
        "/start_contract",
        "/schema_version",
        "/initial_access",
        "/execution_lane",
        "/agent_configuration/global_profile_binding_sha256",
    ] {
        let mut changed = value.clone();
        let slot = changed.pointer_mut(pointer).unwrap();
        *slot = match pointer {
            "/instructions/core_prefix_version" => serde_json::json!(1),
            "/schema_version" => serde_json::json!(2),
            "/agent_configuration/instructions/common/byte_length" => serde_json::json!(1),
            "/requested_access" => serde_json::json!("read-only"),
            "/initial_access" => serde_json::json!("read"),
            "/execution_lane" => serde_json::json!("shared_readonly"),
            _ => serde_json::json!("changed"),
        };
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(
            store.load_manifest(manifest.run_id).unwrap_err().code,
            "RUN_STATE_INVARIANT_VIOLATION",
            "{pointer}"
        );
    }
    for field in ["requested_access", "selected_role", "start_contract"] {
        for remove in [true, false] {
            let mut changed = value.clone();
            if remove {
                changed.as_object_mut().unwrap().remove(field);
            } else {
                changed[field] = Value::Null;
            }
            fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(store.load_manifest(manifest.run_id).is_err(), "{field}");
        }
    }
    for pointer in [
        "/parent_ref",
        "/thread_id",
        "/fork_provenance",
        "/write_continuation_provenance",
        "/aggregate_binding",
        "/purpose/external_label",
        "/agent_configuration/purpose/external_label",
        "/agent_configuration/role_reference",
    ] {
        let mut changed = value.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        changed
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert_eq!(
            store.load_manifest(manifest.run_id).unwrap_err().code,
            "RUN_STATE_INVARIANT_VIOLATION",
            "{pointer}"
        );
    }
    let mut changed = value;
    changed["agent_configuration"]["normalized_instructions"] = serde_json::json!("replacement");
    fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
    assert!(store.load_manifest(manifest.run_id).is_err());
    fs::write(&path, original).unwrap();
    assert_eq!(store.load_manifest(manifest.run_id).unwrap(), manifest);
}

#[test]
fn caller_policy_proof_binds_original_runtime_policy_and_writer_transaction() {
    use dolgorae::domain::{PolicyEpoch, PolicyVerification};
    use dolgorae::worker::{
        CallerPolicyExpectation, CallerPolicyProof, DedicatedServerIdentity, SocketIdentity,
        WorkerIdentity, WorkerRuntimeRecord,
    };
    use dolgorae::writer::{WriterAuthorityState, WriterRecord, WriterStore};
    let manifest = sample_caller_manifest(dolgorae::domain::CallerWorkspaceAccess::Writable);
    let binding = manifest.global_profile_binding.as_ref().unwrap();
    let executable = &binding.launch_snapshot.executable_identity;
    let operation_id = Uuid::now_v7();
    let identity = WorkerIdentity {
        workspace_id: manifest.workspace_id.clone(),
        run_id: manifest.run_id,
        run_generation: 3,
        boot_uuid: Uuid::now_v7(),
        pid: 42,
        process_group_id: 42,
        session_id: 42,
        uid: 501,
        start_tvsec: 1,
        start_tvusec: 2,
        executable_path: PathBuf::from("/example/dolgorae"),
        executable_device: 1,
        executable_inode: 2,
        executable_sha256: "22".repeat(32),
    };
    let dedicated = DedicatedServerIdentity {
        pid: 43,
        process_group_id: 43,
        session_id: 43,
        uid: 501,
        start_tvsec: 1,
        start_tvusec: 3,
        executable_path: PathBuf::from(&executable.resolved_path),
        executable_device: executable.device,
        executable_inode: executable.inode,
        executable_sha256: executable.sha256.clone(),
    };
    let runtime = WorkerRuntimeRecord {
        schema_version: 1,
        socket_path: dolgorae::worker::worker_socket_path(
            501,
            &manifest.workspace_id,
            manifest.run_id,
        )
        .unwrap(),
        identity: identity.clone(),
        socket_identity: SocketIdentity {
            device: 5,
            inode: 6,
        },
        control_socket_epoch: 2,
        app_server_epoch: Some(7),
        control_recovery_required: false,
        startup_scope: None,
        dedicated_server_identity: Some(dedicated.clone()),
        dolgorae_version: "0.1.3".to_owned(),
        mutation_protocol_version: dolgorae::worker::WORKER_PROTOCOL_VERSION,
        binary_sha256: "22".repeat(32),
    };
    let socket = SocketIdentity {
        device: 8,
        inode: 9,
    };
    let policy = dolgorae::domain::EffectivePolicy {
        access: Access::Write,
        verification: PolicyVerification::Verified,
        policy_epoch: PolicyEpoch(4),
        thread_generation: Some(2),
        server_epoch: Some(7),
        writer_generation: Some(6),
    };
    let proof = CallerPolicyProof {
        schema: "dolgorae.caller-policy-proof/v1".to_owned(),
        operation_id,
        identity,
        control_socket_epoch: 2,
        dedicated_server_identity: dedicated,
        app_server_socket_identity: socket.clone(),
        server_key: binding.server_key.clone(),
        server_epoch: 7,
        global_profile_binding_sha256: binding.digest().unwrap(),
        thread_id: "original-thread".to_owned(),
        effective_policy: policy.clone(),
    };
    let mut writer = WriterRecord::empty(&manifest.workspace_id);
    writer.state = WriterAuthorityState::Reserved;
    writer.writer_generation = 6;
    writer.transaction_id = Some(operation_id);
    writer.holder = Some(WriterStore::holder(
        manifest.run_id,
        binding.selected_name.clone(),
        &manifest.controller,
        3,
        binding.server_key.clone(),
        7,
        Some("original-thread".to_owned()),
        dolgorae::domain::RunLifecycle::Idle,
    ));
    let expected = CallerPolicyExpectation {
        phase: dolgorae::worker::CallerPolicyPhase::Prepared,
        operation_id,
        manifest: &manifest,
        runtime: &runtime,
        app_server_socket_identity: &socket,
        original_thread_id: "original-thread",
        durable_policy: &policy,
        controller: &manifest.controller,
        access: Access::Write,
        writer: Some(&writer),
    };
    proof.validate_binding(&expected).unwrap();
    let wire = serde_json::to_value(&proof).unwrap();
    for pointer in [
        "/operation_id",
        "/identity/run_id",
        "/identity/run_generation",
        "/identity/boot_uuid",
        "/identity/start_tvusec",
        "/control_socket_epoch",
        "/dedicated_server_identity/pid",
        "/dedicated_server_identity/executable_sha256",
        "/app_server_socket_identity/inode",
        "/server_key",
        "/server_epoch",
        "/global_profile_binding_sha256",
        "/thread_id",
        "/effective_policy/policy_epoch",
        "/effective_policy/thread_generation",
        "/effective_policy/server_epoch",
        "/effective_policy/writer_generation",
        "/effective_policy/verification",
    ] {
        let mut altered = wire.clone();
        let field = altered.pointer_mut(pointer).unwrap();
        *field = match pointer {
            "/operation_id" | "/identity/run_id" => serde_json::json!(Uuid::now_v7()),
            "/identity/boot_uuid" => serde_json::json!(Uuid::now_v7()),
            "/thread_id" => serde_json::json!("replacement-thread"),
            "/effective_policy/verification" => serde_json::json!("unverified"),
            _ if field.is_string() => {
                serde_json::json!(if field.as_str() == Some(&"33".repeat(32)) {
                    "44".repeat(32)
                } else {
                    "33".repeat(32)
                })
            }
            _ => serde_json::json!(99),
        };
        let altered: CallerPolicyProof = serde_json::from_value(altered).unwrap();
        assert!(
            altered.validate_binding(&expected).is_err(),
            "accepted altered {pointer}"
        );
    }
    for field in ["thread_generation", "server_epoch", "writer_generation"] {
        let mut omitted = wire.clone();
        omitted["effective_policy"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(serde_json::from_value::<CallerPolicyProof>(omitted).is_err());
    }
    let mut extra = wire.clone();
    extra["effective_policy"]["approved"] = serde_json::json!(true);
    assert!(serde_json::from_value::<CallerPolicyProof>(extra).is_err());
    let mut wrong_writer = writer.clone();
    wrong_writer.transaction_id = Some(Uuid::now_v7());
    assert!(
        proof
            .validate_binding(&CallerPolicyExpectation {
                writer: Some(&wrong_writer),
                ..expected
            })
            .is_err()
    );
    let mut stale_controller = manifest.controller.clone();
    stale_controller.identity.generation += 1;
    assert!(
        proof
            .validate_binding(&CallerPolicyExpectation {
                controller: &stale_controller,
                ..expected
            })
            .is_err()
    );
    assert!(
        proof
            .validate_binding(&CallerPolicyExpectation {
                writer: None,
                ..expected
            })
            .is_err()
    );
    let mut reader_proof = proof.clone();
    reader_proof.effective_policy.access = Access::Read;
    reader_proof.effective_policy.writer_generation = None;
    let reader_policy = reader_proof.effective_policy.clone();
    reader_proof
        .validate_binding(&CallerPolicyExpectation {
            access: Access::Read,
            durable_policy: &reader_policy,
            writer: None,
            ..expected
        })
        .unwrap();
    let mut releasing = writer.clone();
    releasing.state = WriterAuthorityState::Releasing;
    reader_proof
        .validate_binding(&CallerPolicyExpectation {
            access: Access::Read,
            durable_policy: &reader_policy,
            writer: Some(&releasing),
            ..expected
        })
        .unwrap();
    for state in [
        WriterAuthorityState::Reserved,
        WriterAuthorityState::Releasing,
        WriterAuthorityState::BlockedUnknown,
    ] {
        let mut guarded = writer.clone();
        guarded.state = state;
        reader_proof
            .validate_binding(&CallerPolicyExpectation {
                phase: dolgorae::worker::CallerPolicyPhase::Recovery,
                access: Access::Read,
                durable_policy: &reader_policy,
                writer: Some(&guarded),
                ..expected
            })
            .unwrap();
        assert!(
            proof
                .validate_binding(&CallerPolicyExpectation {
                    phase: dolgorae::worker::CallerPolicyPhase::Recovery,
                    writer: Some(&guarded),
                    ..expected
                })
                .is_err()
        );
        guarded.transaction_id = Some(Uuid::now_v7());
        assert!(
            reader_proof
                .validate_binding(&CallerPolicyExpectation {
                    phase: dolgorae::worker::CallerPolicyPhase::Recovery,
                    access: Access::Read,
                    durable_policy: &reader_policy,
                    writer: Some(&guarded),
                    ..expected
                })
                .is_err()
        );
    }
    for state in [
        WriterAuthorityState::Reserved,
        WriterAuthorityState::Releasing,
        WriterAuthorityState::BlockedUnknown,
    ] {
        let mut guarded = writer.clone();
        guarded.state = state;
        let recover = CallerPolicyExpectation {
            phase: dolgorae::worker::CallerPolicyPhase::RecoverAcquire,
            writer: Some(&guarded),
            ..expected
        };
        assert_eq!(
            proof.validate_binding(&recover).is_ok(),
            state == WriterAuthorityState::BlockedUnknown
        );
        assert!(
            reader_proof
                .validate_binding(&CallerPolicyExpectation {
                    access: Access::Read,
                    durable_policy: &reader_policy,
                    ..recover
                })
                .is_err()
        );
    }
    let legacy = serde_json::json!({"result":"writer_access_changed", "write":true,
        "writer_generation":6,"thread_id":"original-thread"});
    assert!(serde_json::from_value::<CallerPolicyProof>(legacy).is_err());
}

fn sample_caller_manifest(access: dolgorae::domain::CallerWorkspaceAccess) -> RunManifest {
    use dolgorae::instructions::{InstructionSnapshot as Capture, RoleCapture, SourceCapture};
    let mut manifest = sample_global_manifest();
    let common_body = "공통 지침\r\n".to_owned();
    let role_body = "Implement the current task.\n".to_owned();
    let capture = Capture::compose(
        SourceCapture {
            byte_length: common_body.len() as u64,
            sha256: dolgorae::jcs::sha256_hex(common_body.as_bytes()),
            body: common_body,
        },
        RoleCapture {
            name: "general".to_owned(),
            byte_length: role_body.len() as u64,
            sha256: dolgorae::jcs::sha256_hex(role_body.as_bytes()),
            body: role_body,
        },
    )
    .unwrap();
    manifest.schema_version = 3;
    manifest.execution_lane = ExecutionLane::Dedicated;
    manifest.initial_access = match access {
        dolgorae::domain::CallerWorkspaceAccess::ReadOnly => Access::Read,
        dolgorae::domain::CallerWorkspaceAccess::Writable => Access::Write,
    };
    manifest.requested_access = Some(access);
    manifest.selected_role = Some("general".to_owned());
    manifest.start_contract = Some("caller_workspace/v1".to_owned());
    manifest.agent_configuration = dolgorae::run::CallerAgentConfigurationSnapshot {
        schema_version: 3,
        selected_profile: manifest.profile.profile_name.clone(),
        global_profile_binding_sha256: manifest
            .global_profile_binding
            .as_ref()
            .unwrap()
            .digest()
            .unwrap(),
        model: manifest.model.clone(),
        default_effort: manifest.default_reasoning_effort.clone(),
        purpose: manifest.purpose.clone(),
        required_capabilities: manifest.required_capabilities.clone(),
        role_reference: None,
        instructions: capture.clone(),
        execution_lane: ExecutionLane::Dedicated,
        required_assurance: manifest.requested_assurance,
        native_subagent_policy: "enabled".to_owned(),
    }
    .try_into()
    .unwrap();
    manifest.instructions = capture.into();
    manifest
}

fn sample_global_manifest() -> RunManifest {
    let mut manifest = sample_manifest();
    manifest
        .profile
        .normalized_argv
        .extend(["--enable".to_owned(), "multi_agent".to_owned()]);
    manifest.profile.launch_contract_sha256 = launch_contract_digest(&manifest.profile).unwrap();
    let definition = RuntimeProfile {
        argv: manifest.profile.normalized_argv[..2].to_vec(),
        codex_home: manifest.profile.canonical_codex_home.clone(),
        environment: manifest.profile.sanitized_environment.clone(),
        native_subagents: NativeSubagents::Enabled,
    };
    let snapshot = ServerProfileSnapshot {
        schema_version: 1,
        profile_name: manifest.profile.profile_name.clone(),
        canonical_codex_home: manifest.profile.canonical_codex_home.clone(),
        normalized_argv: manifest.profile.normalized_argv.clone(),
        launch_cwd_policy: manifest.profile.launch_cwd_policy.clone(),
        derived_launch_cwd: manifest.profile.derived_launch_cwd.clone(),
        sanitized_environment: manifest.profile.sanitized_environment.clone(),
        enabled_features: manifest.profile.enabled_features.clone(),
        disabled_features: manifest.profile.disabled_features.clone(),
        process_static_configuration: manifest.profile.process_static_configuration.clone(),
        initial_configuration_observation: manifest
            .profile
            .initial_configuration_observation
            .clone(),
        executable_identity: ProfileExecutableIdentity {
            resolved_path: manifest
                .profile
                .executable_identity
                .resolved_path
                .to_path_buf()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            device: manifest.profile.executable_identity.device,
            inode: manifest.profile.executable_identity.inode,
            sha256: manifest.profile.executable_identity.sha256.clone(),
        },
        codex_version: manifest.profile.codex_version.clone(),
        schema_bundle_sha256: manifest.profile.app_server_schema_sha256.clone(),
        compatibility_manifest_sha256: manifest.profile.compatibility_manifest_sha256.clone(),
        launch_contract_sha256: manifest.profile.launch_contract_sha256.clone(),
        compatibility_verdict: ProfileCompatibilityVerdict::Tested,
        server_key: manifest.profile.initial_server_key.clone(),
    };
    manifest.schema_version = 2;
    manifest.global_profile_binding = Some(
        ResolvedGlobalProfile::from_definition("default", definition)
            .unwrap()
            .bind(snapshot)
            .unwrap(),
    );
    manifest.agent_configuration.schema_version = 2;
    manifest.agent_configuration.runtime_profile_snapshot_sha256 = manifest
        .global_profile_binding
        .as_ref()
        .unwrap()
        .digest()
        .unwrap();
    manifest
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
        instructions: instructions.clone().into(),
        execution_lane: ExecutionLane::SharedReadonly,
        required_assurance: Assurance::BestEffortPersonalAlpha,
        native_subagent_policy: "enabled".to_owned(),
        admitted_configuration: None,
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
        requested_access: None,
        selected_role: None,
        start_contract: None,
        creation_mode: None,
        admitted_configuration_digest: None,
        start_operation_receipt: None,
        control_mode: ControlMode::DirectInteractive,
        execution_lane: ExecutionLane::SharedReadonly,
        requested_assurance: Assurance::BestEffortPersonalAlpha,
        achieved_assurance: Assurance::BestEffortPersonalAlpha,
        profile,
        global_profile_binding: None,
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
        instructions: instructions.into(),
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
        write_continuation_provenance: None,
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

fn sample_caller_intent() -> CallerStartIntent {
    let manifest = sample_caller_manifest(dolgorae::domain::CallerWorkspaceAccess::Writable);
    let workspace_id =
        dolgorae::workspace::workspace_id(&manifest.canonical_workspace.to_path_buf().unwrap());
    let request: dolgorae::semantic::CallerRunStartRequest =
        serde_json::from_value(serde_json::json!({
            "schema":"dolgorae-run-start-request/v2", "workspace":null,
            "profile":"default", "control_mode":"direct_interactive",
            "model":null, "effort":null, "purpose":manifest.purpose,
            "parent_ref":null, "required_capabilities":["reader"], "idempotency_key":"caller-k1",
        }))
        .unwrap();
    let dolgorae::run::RunInstructionSnapshot::Caller(instructions) = manifest.instructions else {
        unreachable!();
    };
    CallerStartIntent {
        admitted_configuration: None,
        orchestration: None,
        readiness: dolgorae::run::CallerProfileReadiness {
            model: manifest.model.clone(),
            default_effort: manifest.default_reasoning_effort.clone(),
            profile_capability_snapshot: manifest.profile_capability_snapshot.clone(),
        },
        run_id: Uuid::now_v7(),
        created_at: manifest.created_at,
        workspace_id,
        canonical_workspace: manifest.canonical_workspace,
        workspace_mode: manifest.workspace_mode,
        start_baseline: manifest.start_baseline,
        request: request.resolve().unwrap(),
        controller: manifest.controller,
        global_profile_binding: manifest.global_profile_binding.unwrap(),
        instructions,
    }
}

#[test]
fn canonical_numeric_profile_and_start_reservation_preserve_timeout_identity() {
    let floats = serde_json::json!({
        "gaori": {"tool_timeout_sec": 3601.0},
        "mulgae": {"startup_timeout_sec": 30.0, "tool_timeout_sec": 7501.0}
    });
    let integers = serde_json::json!({
        "gaori": {"tool_timeout_sec": 3601},
        "mulgae": {"startup_timeout_sec": 30, "tool_timeout_sec": 7501}
    });
    let manifest_with = |metadata: Value| {
        let mut manifest = sample_manifest();
        manifest
            .profile
            .process_static_configuration
            .insert("mcp_servers".into(), metadata);
        manifest.profile.launch_contract_sha256 =
            launch_contract_digest(&manifest.profile).unwrap();
        manifest.agent_configuration.runtime_profile_snapshot_sha256 =
            runtime_profile_snapshot_digest(&manifest.profile).unwrap();
        manifest
    };
    let tree = TestTree::new();
    let state = tree.path("state");
    make_dir(&state);
    make_dir(&state.join("runs"));
    let runs = RunStore::new(SystemWorkspacePlatform, &state);
    let manifest = manifest_with(floats.clone());
    let directory = runs.publish(&manifest).unwrap();
    let loaded = runs.load_manifest(manifest.run_id).unwrap();
    assert_eq!(loaded.profile, manifest.profile);
    assert_eq!(
        runtime_profile_snapshot_digest(&loaded.profile).unwrap(),
        runtime_profile_snapshot_digest(&manifest_with(integers.clone()).profile).unwrap()
    );
    let original_manifest = fs::read(directory.root.join("manifest.json")).unwrap();

    let intent_with = |metadata: Value| {
        let mut intent = sample_caller_intent();
        let binding = &mut intent.global_profile_binding;
        binding
            .launch_snapshot
            .process_static_configuration
            .insert("mcp_servers".into(), metadata);
        binding.launch_snapshot_sha256 = dolgorae::jcs::sha256_hex(
            &canonicalize(
                &parse(&serde_json::to_string(&binding.launch_snapshot).unwrap()).unwrap(),
            )
            .unwrap(),
        );
        intent
    };
    let intent = intent_with(floats);
    let mut equivalent = intent.clone();
    equivalent.global_profile_binding = intent_with(integers).global_profile_binding;
    let reservation = CallerStartReservation::new(intent).unwrap();
    let equivalent = CallerStartReservation::new(equivalent).unwrap();
    assert_eq!(reservation.request_sha256, equivalent.request_sha256);
    assert_eq!(
        reservation.normalized_identity_sha256,
        equivalent.normalized_identity_sha256
    );
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state);
    store.reserve_caller(&reservation).unwrap();
    let replay = store.reserve_caller(&equivalent).unwrap();
    assert_eq!(replay.run_id, reservation.run_id);
    assert_eq!(
        replay.accepted_intent.global_profile_binding,
        reservation.accepted_intent.global_profile_binding
    );
    assert_eq!(
        dolgorae::global_runtime::GlobalLaunchSnapshot::from(
            &replay
                .accepted_intent
                .global_profile_binding
                .launch_snapshot
        ),
        dolgorae::global_runtime::GlobalLaunchSnapshot::from(
            &reservation
                .accepted_intent
                .global_profile_binding
                .launch_snapshot
        )
    );
    assert_eq!(
        replay.normalized_identity_sha256,
        reservation.normalized_identity_sha256
    );
    replay
        .accepted_intent
        .global_profile_binding
        .validate_for_recovery()
        .unwrap();

    let mut changed = replay.accepted_intent.clone();
    changed.global_profile_binding = intent_with(serde_json::json!({
        "gaori": {"tool_timeout_sec": 3602}
    }))
    .global_profile_binding;
    let changed = CallerStartReservation::new(changed).unwrap();
    assert_ne!(
        changed.accepted_intent.global_profile_binding,
        reservation.accepted_intent.global_profile_binding
    );
    assert_ne!(
        changed.normalized_identity_sha256,
        reservation.normalized_identity_sha256
    );
    let retained = store.reserve_caller(&changed).unwrap();
    assert_eq!(
        retained.normalized_identity_sha256,
        reservation.normalized_identity_sha256
    );

    for number in [
        serde_json::json!(9007199254740993_u64),
        serde_json::json!(u64::MAX),
        serde_json::json!(i64::MIN + 1),
    ] {
        let metadata = serde_json::json!({"gaori": {"tool_timeout_sec": number}});
        assert!(runs.publish(&manifest_with(metadata.clone())).is_err());
        assert!(CallerStartReservation::new(intent_with(metadata)).is_err());
        assert_eq!(
            fs::read(directory.root.join("manifest.json")).unwrap(),
            original_manifest
        );
    }
}

#[test]
fn checked_caller_reservation_example_recovers_with_its_bound_digests() {
    let reservation: CallerStartReservation = serde_json::from_str(include_str!(
        "../docs/protocol/examples/workspace-execution-start-reservation.valid.json"
    ))
    .unwrap();
    assert_eq!(
        CallerStartReservation::new(reservation.accepted_intent.clone()).unwrap(),
        reservation
    );
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    store.reserve_caller(&reservation).unwrap();
    assert_eq!(
        store
            .load_caller(&reservation.idempotency_key, &reservation.request_sha256)
            .unwrap()
            .unwrap(),
        reservation
    );
}

#[test]
fn caller_start_receipt_preserves_original_acceptance_and_rejects_corruption() {
    use dolgorae::domain::{
        CallerWorkspaceAccess, EffectivePolicy, PolicyEpoch, PolicyVerification,
    };
    use dolgorae::run::CallerStartReceipt;
    for access in [
        CallerWorkspaceAccess::Writable,
        CallerWorkspaceAccess::ReadOnly,
    ] {
        let tree = TestTree::new();
        let root = tree.path("state");
        make_dir(&root.join("runs"));
        let manifest = sample_caller_manifest(access);
        let store = RunStore::new(SystemWorkspacePlatform, &root);
        let directory = store.publish(&manifest).unwrap();
        let receipt = CallerStartReceipt {
            schema: "dolgorae.caller-start-receipt/v1".to_owned(),
            run_id: manifest.run_id,
            accepted_intent_sha256: "ab".repeat(32),
            global_profile_binding_sha256: manifest
                .global_profile_binding
                .as_ref()
                .unwrap()
                .digest()
                .unwrap(),
            original_thread_id: "original-turnless-thread".to_owned(),
            run_generation: 1,
            effective_policy: EffectivePolicy {
                access: if access == CallerWorkspaceAccess::Writable {
                    Access::Write
                } else {
                    Access::Read
                },
                verification: PolicyVerification::Verified,
                policy_epoch: PolicyEpoch(2),
                thread_generation: Some(1),
                server_epoch: Some(3),
                writer_generation: (access == CallerWorkspaceAccess::Writable).then_some(1),
            },
        };
        assert!(
            store
                .load_caller_start_receipt(manifest.run_id, &receipt.accepted_intent_sha256)
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store.publish_caller_start_receipt(&receipt).unwrap(),
            receipt
        );
        let path = directory.root.join("caller-start-receipt.json");
        assert_eq!(mode(&path), 0o600);
        let mut later = receipt.clone();
        later.run_generation = 9;
        later.effective_policy.policy_epoch = PolicyEpoch(10);
        assert_eq!(store.publish_caller_start_receipt(&later).unwrap(), receipt);
        assert!(
            store
                .load_caller_start_receipt(manifest.run_id, &"cd".repeat(32))
                .is_err()
        );
        for mutation in ["missing_epoch", "unknown_policy", "wrong_binding", "extra"] {
            let mut value = serde_json::to_value(&receipt).unwrap();
            match mutation {
                "missing_epoch" => {
                    value["effective_policy"]
                        .as_object_mut()
                        .unwrap()
                        .remove("server_epoch");
                }
                "unknown_policy" => {
                    value["effective_policy"]["verification"] = serde_json::json!("unverified")
                }
                "wrong_binding" => {
                    value["global_profile_binding_sha256"] = serde_json::json!("00".repeat(32))
                }
                _ => value["extra"] = serde_json::json!(true),
            }
            fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
            assert!(
                store
                    .load_caller_start_receipt(manifest.run_id, &receipt.accepted_intent_sha256)
                    .is_err(),
                "{mutation}"
            );
        }
    }
}

#[test]
fn caller_reservation_retains_resolved_defaults_and_rejects_unready_profile() {
    let intent = sample_caller_intent();
    assert!(intent.request.model.is_none());
    assert!(intent.request.effort.is_none());
    let reservation = CallerStartReservation::new(intent.clone()).unwrap();
    let restored: CallerStartReservation =
        serde_json::from_slice(&serde_json::to_vec(&reservation).unwrap()).unwrap();
    assert_eq!(
        CallerStartReservation::new(restored.accepted_intent.clone()).unwrap(),
        restored
    );
    assert_eq!(restored.accepted_intent.readiness, intent.readiness);
    for mutation in ["unsupported", "epoch", "binding", "explicit_model"] {
        let mut changed = intent.clone();
        match mutation {
            "unsupported" => {
                changed
                    .readiness
                    .profile_capability_snapshot
                    .capabilities
                    .insert("reader".to_owned(), CapabilityState::Unverified);
            }
            "epoch" => changed.readiness.profile_capability_snapshot.server_epoch = 0,
            "binding" => changed.readiness.profile_capability_snapshot.server_key = "00".repeat(32),
            _ => changed.request.model = Some("changed-after-acceptance".to_owned()),
        }
        assert!(CallerStartReservation::new(changed).is_err(), "{mutation}");
    }
}

#[test]
fn caller_retry_recovers_original_intent_before_live_sources_or_manifest_exist() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    let original = CallerStartReservation::new(sample_caller_intent()).unwrap();
    let source = tree.path("role.md");
    fs::write(&source, &original.accepted_intent.instructions.role.body).unwrap();
    assert_eq!(store.reserve_caller(&original).unwrap(), original);
    assert!(!state_root.join("runs").exists());
    fs::remove_file(&source).unwrap();
    let retry_digest = caller_start_request_digest(
        &original.accepted_intent.request,
        &original.accepted_intent.canonical_workspace,
        &original.accepted_intent.controller,
    )
    .unwrap();
    assert_eq!(
        store
            .load_caller("caller-k1", &retry_digest)
            .unwrap()
            .unwrap(),
        original
    );

    // A losing allocator must use the original captures and Profile binding,
    // even when its current source observation would produce different input.
    let mut retry = original.accepted_intent.clone();
    retry.run_id = Uuid::now_v7();
    let body = "Changed instructions.\n".to_owned();
    retry.instructions = dolgorae::instructions::InstructionSnapshot::compose(
        dolgorae::instructions::SourceCapture {
            byte_length: body.len() as u64,
            sha256: dolgorae::jcs::sha256_hex(body.as_bytes()),
            body,
        },
        retry.instructions.role,
    )
    .unwrap();
    let retry = CallerStartReservation::new(retry).unwrap();
    assert_ne!(
        retry.normalized_identity_sha256,
        original.normalized_identity_sha256
    );
    assert_eq!(retry.request_sha256, original.request_sha256);
    assert_eq!(store.reserve_caller(&retry).unwrap(), original);

    let mut alias = original.accepted_intent.request.clone();
    alias.workspace = Some("/workspace/alias".to_owned());
    alias.expected_workspace_id = Some(original.accepted_intent.workspace_id.clone());
    assert_eq!(
        caller_start_request_digest(
            &alias,
            &original.accepted_intent.canonical_workspace,
            &original.accepted_intent.controller,
        )
        .unwrap(),
        original.request_sha256
    );
    for field in ["role", "access", "model", "controller"] {
        let mut changed = original.accepted_intent.clone();
        match field {
            "role" => changed.request.role = "reviewer".to_owned(),
            "access" => changed.request.access = dolgorae::domain::CallerWorkspaceAccess::ReadOnly,
            "model" => changed.request.model = Some("different-model".to_owned()),
            "controller" => changed.controller.identity.generation += 1,
            _ => unreachable!(),
        }
        let digest = caller_start_request_digest(
            &changed.request,
            &changed.canonical_workspace,
            &changed.controller,
        )
        .unwrap();
        let error = store.load_caller("caller-k1", &digest).unwrap_err();
        assert_eq!(error.code, "IDEMPOTENCY_CONFLICT", "{field}");
        assert_eq!(error.details["run_id"], serde_json::json!(original.run_id));
    }
}

#[test]
fn caller_reservation_corruption_fails_closed_without_reconstructing_input() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    let original = CallerStartReservation::new(sample_caller_intent()).unwrap();
    store.reserve_caller(&original).unwrap();
    let path = state_root.join("idempotency/run-start").join(format!(
        "{}.json",
        dolgorae::jcs::sha256_hex(original.idempotency_key.as_bytes())
    ));
    for field in [
        "digest",
        "run",
        "capture",
        "key",
        "unknown",
        "access",
        "role",
        "controller_subject",
        "controller_unknown",
    ] {
        let mut value = serde_json::to_value(&original).unwrap();
        match field {
            "digest" => value["normalized_identity_sha256"] = serde_json::json!("0".repeat(64)),
            "run" => value["run_id"] = serde_json::json!(Uuid::now_v7()),
            "capture" => {
                value["accepted_intent"]["instructions"]["role"]["body"] =
                    serde_json::json!("tampered")
            }
            "key" => value["idempotency_key"] = serde_json::json!("other-key"),
            "unknown" => value["future_field"] = serde_json::json!(true),
            "access" | "role" => {
                value["accepted_intent"]["request"]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
            "controller_subject" => {
                value["accepted_intent"]["controller"]["identity"]
                    .as_object_mut()
                    .unwrap()
                    .remove("subject_id");
            }
            "controller_unknown" => {
                value["accepted_intent"]["controller"]["identity"]["unexpected"] =
                    serde_json::json!(true);
            }
            _ => unreachable!(),
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            store
                .load_caller("caller-k1", &original.request_sha256)
                .is_err(),
            "{field}"
        );
        assert!(store.load("caller-k1").is_err(), "{field}");
    }
    let mut invalid = original.accepted_intent.clone();
    invalid.request.idempotency_key = "invalid-new-key".to_owned();
    invalid.request.role = "reviewer".to_owned();
    assert!(CallerStartReservation::new(invalid).is_err());
    let mut lossy = original.accepted_intent.clone();
    lossy.controller.identity.generation = (1_u64 << 53) + 1;
    assert!(CallerStartReservation::new(lossy).is_err());
    assert_eq!(fs::read_dir(path.parent().unwrap()).unwrap().count(), 1);
}

#[test]
fn caller_reservation_preserves_the_maximum_raw_capture_through_json_escaping() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let mut intent = sample_caller_intent();
    let role = intent.instructions.role;
    let body = "\u{1}".repeat(65_536 - role.body.len());
    intent.instructions = dolgorae::instructions::InstructionSnapshot::compose(
        dolgorae::instructions::SourceCapture {
            byte_length: body.len() as u64,
            sha256: dolgorae::jcs::sha256_hex(body.as_bytes()),
            body,
        },
        role,
    )
    .unwrap();
    let reservation = CallerStartReservation::new(intent).unwrap();
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    store.reserve_caller(&reservation).unwrap();
    assert_eq!(
        store
            .load_caller("caller-k1", &reservation.request_sha256)
            .unwrap()
            .unwrap(),
        reservation
    );
}

#[test]
fn concurrent_caller_allocators_return_one_original_run_and_capture() {
    let tree = TestTree::new();
    let state_root = tree.path("state");
    make_dir(&state_root);
    let first = CallerStartReservation::new(sample_caller_intent()).unwrap();
    let mut second = first.accepted_intent.clone();
    second.run_id = Uuid::now_v7();
    let second = CallerStartReservation::new(second).unwrap();
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let handles = [&first, &second].map(|reservation| {
            scope.spawn(|| {
                let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
                barrier.wait();
                store.reserve_caller(reservation).unwrap()
            })
        });
        handles.map(|handle| handle.join().unwrap())
    });
    assert_eq!(results[0], results[1]);
    assert!([first.run_id, second.run_id].contains(&results[0].run_id));
    let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
    assert_eq!(
        store.load("caller-k1").unwrap_err().code,
        "CAPABILITY_UNSUPPORTED"
    );
    assert_eq!(
        fs::read_dir(state_root.join("idempotency/run-start"))
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn legacy_and_caller_allocators_cannot_shadow_each_others_key() {
    for caller_first in [true, false] {
        let tree = TestTree::new();
        let state_root = tree.path("state");
        make_dir(&state_root);
        let store = StartReservationStore::new(SystemWorkspacePlatform, &state_root);
        let caller = CallerStartReservation::new(sample_caller_intent()).unwrap();
        let legacy = StartReservation {
            schema_version: 1,
            operation: "start_run".to_owned(),
            idempotency_key: caller.idempotency_key.clone(),
            normalized_identity_sha256: "a".repeat(64),
            run_id: Uuid::now_v7(),
        };
        if caller_first {
            store.reserve_caller(&caller).unwrap();
            assert_eq!(
                store.reserve(&legacy).unwrap_err().code,
                "CAPABILITY_UNSUPPORTED"
            );
            assert_eq!(
                store
                    .load_caller("caller-k1", &caller.request_sha256)
                    .unwrap()
                    .unwrap(),
                caller
            );
        } else {
            store.reserve(&legacy).unwrap();
            assert_eq!(
                store.reserve_caller(&caller).unwrap_err().code,
                "IDEMPOTENCY_CONFLICT"
            );
            assert_eq!(store.load("caller-k1").unwrap().unwrap(), legacy);
        }
        assert_eq!(
            fs::read_dir(state_root.join("idempotency/run-start"))
                .unwrap()
                .count(),
            1
        );
    }
}

/// docs/specs/README.md: "Run allocation reserves its key before publishing a Run ...
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
