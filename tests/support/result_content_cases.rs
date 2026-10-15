//! Native content maintenance keeps its retained owner and frozen Machine projection.
#![cfg(target_os = "macos")]

use super::support;

use dolgorae::controller::{CredentialCarrier, OperatorStore};
use dolgorae::engagement::EngagementStore;
use dolgorae::jcs::sha256_hex;
use dolgorae::machine::MachineError;
use dolgorae::maintenance::{
    ContentMaintenanceBarrier, MaintenancePlan, apply_result_content_with_barrier,
    get_result_content,
};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use support::Fixture;
use uuid::Uuid;

const MACHINE_SCHEMA: &str = "dolgorae-machine-v5.schema.json";
const CONTENT_SCHEMA: &str = "dolgorae-native-result-content-v1.schema.json";

fn retain_check(checks: &mut Vec<Value>, schema: &str, value: &Value) {
    checks.push(json!({"schema":schema,"value":value}));
}

fn check_all_shapes(checks: &[Value]) {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR"));
    let python = std::env::var_os("DOLGORAE_TEST_PYTHON")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join(".venv/bin/python"));
    let mut validator = Command::new(python)
        .args(["-B", "-c", "import json,pathlib,sys; sys.path.insert(0,str(pathlib.Path(sys.argv[1])/'tests/e2e')); from schema_support import validator,assert_valid; root=pathlib.Path(sys.argv[1])/'docs/protocol'; [assert_valid(v['value'],validator(root,v['schema']),'native content CLI '+str(i)) for i,v in enumerate(json.load(sys.stdin))]"])
        .arg(repository)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    validator
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(checks).unwrap())
        .unwrap();
    let output = validator.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn command(fixture: &Fixture, arguments: &[&str], checks: &mut Vec<Value>) -> Value {
    let output = fixture.command(arguments);
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        output.status.success(),
        "{arguments:?}: {envelope} {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(envelope["schema"], "dolgorae-machine/v5");
    assert_eq!(envelope["ok"], true);
    retain_check(checks, MACHINE_SCHEMA, &envelope);
    envelope["data"].clone()
}

fn plan(fixture: &Fixture, kind: &str, checks: &mut Vec<Value>) -> (MaintenancePlan, PathBuf) {
    let result = command(
        fixture,
        &[
            "maintenance",
            "plan",
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--operator-file",
            fixture.operator.to_str().unwrap(),
            "--selected-generation",
            "1",
            "--kind",
            kind,
        ],
        checks,
    );
    let plan: MaintenancePlan = serde_json::from_value(result["plan"].clone()).unwrap();
    assert_eq!(plan.kind, kind);
    assert_eq!(plan.selected_generation, 1);
    assert!(plan.dry_run);
    assert!(!plan.admission_closed);
    let path = fixture
        .root
        .join(format!("content-plan-{}.json", plan.plan_ref));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    file.write_all(&serde_json::to_vec(&plan).unwrap()).unwrap();
    file.sync_all().unwrap();
    (plan, path)
}

fn get(fixture: &Fixture, plan: &MaintenancePlan, checks: &mut Vec<Value>) -> Value {
    command(
        fixture,
        &[
            "maintenance",
            "get",
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--operator-file",
            fixture.operator.to_str().unwrap(),
            "--selected-generation",
            "1",
            "--plan-id",
            &plan.plan_ref.to_string(),
        ],
        checks,
    )["maintenance"]
        .clone()
}

fn apply(fixture: &Fixture, path: &Path, checks: &mut Vec<Value>) -> Value {
    command(
        fixture,
        &[
            "maintenance",
            "apply",
            "--workspace",
            fixture.workspace.to_str().unwrap(),
            "--operator-file",
            fixture.operator.to_str().unwrap(),
            "--plan-file",
            path.to_str().unwrap(),
            "--approve",
        ],
        checks,
    )["maintenance"]
        .clone()
}

fn projection(value: &Value, plan: &MaintenancePlan, state: &str, action: &str) {
    assert_eq!(value["operation_ref"], json!(plan.plan_ref));
    assert_eq!(value["plan"], json!(plan));
    assert_eq!(value["state"], state);
    assert_eq!(value["old_generation"], 1);
    assert!(value["new_generation"].is_null());
    assert_eq!(
        value["selected_manifest_digest"],
        plan.expected_manifest_digest
    );
    assert_eq!(value["required_action"], action);
    assert_eq!(
        value.as_object().unwrap().len(),
        7,
        "native-only fields leaked into the frozen projection"
    );
}

fn sidecars(fixture: &Fixture, plan: &MaintenancePlan, checks: &mut Vec<Value>) -> Value {
    let namespace = fixture.state_root.join("result-maintenance");
    let operation: Value = serde_json::from_slice(
        &fs::read(namespace.join(format!("{}.json", plan.plan_ref))).unwrap(),
    )
    .unwrap();
    assert_eq!(operation["plan"], json!(plan));
    retain_check(checks, CONTENT_SCHEMA, &operation);
    let owner_path = namespace.join(format!("{}.owner.json", plan.plan_ref));
    if owner_path.exists() {
        let owner: Value = serde_json::from_slice(&fs::read(owner_path).unwrap()).unwrap();
        assert_eq!(owner["operation_ref"], json!(plan.plan_ref));
        assert_eq!(owner["source_generation"], 1);
        retain_check(checks, CONTENT_SCHEMA, &owner);
    }
    operation
}

#[test]
fn native_result_content_cli_preserves_frozen_projection_and_retained_owner() {
    let fixture = Fixture::new("run_start_model_list.json");
    let database = EngagementStore::workspace_database_path(&fixture.state_root);
    drop(EngagementStore::open(&database).unwrap());
    let body = br#"{"result":"shared retained content"}"#;
    let sha = sha256_hex(body);
    let artifacts = [Uuid::now_v7(), Uuid::now_v7()];
    // Seed only this isolated fixture's delivered storage rows, using the same
    // minimal provenance arrangement as maintenance's component tests. Every
    // maintenance effect and frozen envelope below comes from production code.
    let mut connection = Connection::open(&database).unwrap();
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .unwrap();
    let transaction = connection.transaction().unwrap();
    transaction
        .execute(
            "INSERT INTO result_content(sha256,bytes) VALUES(?1,?2)",
            params![sha, body.as_slice()],
        )
        .unwrap();
    for artifact in artifacts {
        let engagement = Uuid::now_v7();
        let run = Uuid::now_v7();
        let task = Uuid::now_v7();
        transaction.execute("INSERT INTO engagements(engagement_id,workspace_id,controller_sha256,state,revision) VALUES(?1,?2,?3,'result_ready',1)", params![engagement.to_string(),fixture.workspace_id,"a".repeat(64)]).unwrap();
        transaction.execute("INSERT INTO members(specialist_run_id,engagement_id,hire_operation_id,configuration_sha256,state) VALUES(?1,?2,?3,?4,'ready')", params![run.to_string(),engagement.to_string(),Uuid::now_v7().to_string(),"b".repeat(64)]).unwrap();
        transaction.execute("INSERT INTO tasks(task_id,engagement_id,specialist_run_id,objective_sha256,state,result_sha256,artifact_id) VALUES(?1,?2,?3,?4,'delivered',?5,?6)", params![task.to_string(),engagement.to_string(),run.to_string(),"c".repeat(64),sha,artifact.to_string()]).unwrap();
        transaction.execute("INSERT INTO artifacts(artifact_id,engagement_id,task_id,specialist_run_id,result_sha256,canonical_json) VALUES(?1,?2,?3,?4,?5,'')", params![artifact.to_string(),engagement.to_string(),task.to_string(),run.to_string(),sha]).unwrap();
        transaction.execute("INSERT INTO delivery_receipts(task_id,artifact_id,delivered_at_ms) VALUES(?1,?2,1)", params![task.to_string(),artifact.to_string()]).unwrap();
        transaction.execute("INSERT INTO result_content_refs(owner_kind,owner_id,representation,sha256,byte_length) VALUES('external_artifact',?1,'json',?2,?3)",params![artifact.to_string(),sha,body.len() as u64]).unwrap();
    }
    transaction.commit().unwrap();
    drop(connection);
    let content_root = fixture.state_root.join("result-content");
    fs::create_dir(&content_root).unwrap();
    fs::set_permissions(&content_root, fs::Permissions::from_mode(0o700)).unwrap();
    let content_path = content_root.join(format!("{sha}.bin"));
    let mut content = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&content_path)
        .unwrap();
    content.write_all(body).unwrap();
    content.sync_all().unwrap();
    drop(content);

    let mut checks = Vec::new();
    let (retirement, retirement_path) = plan(&fixture, "retire_result_content", &mut checks);
    assert_eq!(
        retirement.retire_content,
        vec![json!({"byte_length":body.len(),"sha256":format!("sha256:{sha}")})]
    );
    let planned = get(&fixture, &retirement, &mut checks);
    projection(&planned, &retirement, "planned", "none");
    let planned_sidecar = sidecars(&fixture, &retirement, &mut checks);
    assert_eq!(planned_sidecar["targets"].as_array().unwrap().len(), 2);
    // Exercise the native reference union in the operation carrier as well as
    // the standalone reference definition. A cached chunk has this same shape.
    let mut chunk_carrier = planned_sidecar.clone();
    chunk_carrier["references"][0]["owner_kind"] = json!("brokered_chunk_receipt");
    chunk_carrier["references"][0]["representation"] = json!("raw");
    retain_check(&mut checks, CONTENT_SCHEMA, &chunk_carrier);
    let rejected = fixture.command(&[
        "maintenance",
        "apply",
        "--workspace",
        fixture.workspace.to_str().unwrap(),
        "--operator-file",
        fixture.operator.to_str().unwrap(),
        "--plan-file",
        retirement_path.to_str().unwrap(),
    ]);
    assert!(!rejected.status.success());
    let rejected: Value = serde_json::from_slice(&rejected.stdout).unwrap();
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["error"]["effect"], "none");
    retain_check(&mut checks, MACHINE_SCHEMA, &rejected);
    assert_eq!(get(&fixture, &retirement, &mut checks), planned);

    // Interrupt after the actual tombstone commit. CLI read-back must identify
    // the original owner and require resumption, without allocating new work.
    let operator = OperatorStore::new(fixture.home.join(".dolgorae/operator"))
        .authorize(&CredentialCarrier::open_path(&fixture.operator).unwrap())
        .unwrap();
    let interrupted = apply_result_content_with_barrier(
        &fixture.state_root,
        &fixture.workspace_id,
        &retirement,
        true,
        &operator,
        |barrier| {
            if barrier == ContentMaintenanceBarrier::TombstonesCommitted {
                Err(MachineError::new(
                    "RECOVERY_REQUIRED",
                    "injected post-tombstone response loss",
                    false,
                    json!({}),
                ))
            } else {
                Ok(())
            }
        },
    )
    .unwrap_err();
    assert_eq!(
        interrupted.details["operation_ref"],
        json!(retirement.plan_ref)
    );
    let native = get_result_content(
        &fixture.state_root,
        &fixture.workspace_id,
        retirement.plan_ref,
        1,
        &operator,
    )
    .unwrap();
    assert_eq!(native["state"], "retiring");
    retain_check(&mut checks, CONTENT_SCHEMA, &native);
    drop(operator);
    projection(
        &get(&fixture, &retirement, &mut checks),
        &retirement,
        "recovery_needed",
        "resume_maintenance",
    );
    sidecars(&fixture, &retirement, &mut checks);
    let completed = apply(&fixture, &retirement_path, &mut checks);
    projection(&completed, &retirement, "completed", "none");
    assert_eq!(get(&fixture, &retirement, &mut checks), completed);
    assert_eq!(apply(&fixture, &retirement_path, &mut checks), completed);
    sidecars(&fixture, &retirement, &mut checks);
    assert_eq!(fs::read(&content_path).unwrap(), body);
    let (collection, collection_path) = plan(&fixture, "garbage_collection", &mut checks);
    assert_ne!(collection.plan_ref, retirement.plan_ref);
    assert_eq!(collection.retire_content, retirement.retire_content);
    assert!(
        sidecars(&fixture, &collection, &mut checks)["targets"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    projection(
        &apply(&fixture, &collection_path, &mut checks),
        &collection,
        "completed",
        "none",
    );
    projection(
        &get(&fixture, &collection, &mut checks),
        &collection,
        "completed",
        "none",
    );
    sidecars(&fixture, &collection, &mut checks);
    assert!(!content_path.exists());
    assert_eq!(get(&fixture, &retirement, &mut checks), completed);
    let connection = Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM result_content", [], |r| r
                .get::<_, u64>(0))
            .unwrap(),
        0
    );
    for table in ["artifacts", "delivery_receipts", "tasks"] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r
                    .get::<_, u64>(0))
                .unwrap(),
            2
        );
    }
    let retired: Vec<String> = connection
        .prepare("SELECT retired_operation FROM result_content_refs ORDER BY owner_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(retired, vec![retirement.plan_ref.to_string(); 2]);
    drop(connection);
    check_all_shapes(&checks);
}
