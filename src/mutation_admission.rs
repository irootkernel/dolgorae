//! Durable ownership tokens for serialized Run mutations.

use crate::audit::AuditKind;
use crate::domain::RunLifecycle;
use crate::fault::FaultInjector;
use crate::jcs::canonicalize;
use crate::ledger::{Ledger, LedgerClock};
use crate::machine::MachineError;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MutationAdmission {
    pub schema_version: u32,
    pub admission_id: Uuid,
    pub controller_id: Uuid,
    pub expected_state_revision: u64,
    pub request_sha256: String,
    pub operation: String,
    pub allow_writer_acquire: bool,
}

pub(crate) struct MutationRequestIdentity {
    request_sha256: String,
    operation: &'static str,
    allow_writer_acquire: bool,
    compatible_admission_operations: &'static [&'static str],
}

impl MutationRequestIdentity {
    pub(crate) fn new(
        request_sha256: String,
        operation: &'static str,
        allow_writer_acquire: bool,
        compatible_admission_operations: &'static [&'static str],
    ) -> Self {
        Self {
            request_sha256,
            operation,
            allow_writer_acquire,
            compatible_admission_operations,
        }
    }

    #[cfg(test)]
    pub(crate) fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
}

pub(crate) trait MutationRequest {
    fn mutation_request_identity(&self) -> Result<MutationRequestIdentity, MachineError>;
}

fn internal(reason: impl Into<String>) -> MachineError {
    let reason = reason.into();
    MachineError::new(
        "INTERNAL_ERROR",
        reason.clone(),
        false,
        json!({"invariant": reason}),
    )
}

fn state_conflict(run_id: Uuid, state: RunLifecycle, operation: &str) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        format!(
            "{operation} is not allowed while the run is {}",
            state.as_str()
        ),
        false,
        json!({"run_id": run_id, "state": state.as_str(), "operation": operation}),
    )
}

pub(crate) fn active_mutation_admission<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &Ledger<C, F>,
) -> Result<Option<MutationAdmission>, MachineError> {
    let mut active = None;
    for record in ledger
        .durable_records()
        .map_err(|error| internal(error.to_string()))?
    {
        match record.kind() {
            AuditKind::MutationAdmitted => {
                let payload =
                    canonicalize(record.payload()).map_err(|error| internal(error.to_string()))?;
                let admission: MutationAdmission = serde_json::from_slice(&payload)
                    .map_err(|error| internal(error.to_string()))?;
                if admission.schema_version != 1
                    || admission.admission_id.get_version_num() != 7
                    || admission.expected_state_revision >= record.sequence()
                    || admission.request_sha256.len() != 64
                    || !admission
                        .request_sha256
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
                    || admission.operation.is_empty()
                    || admission.operation.len() > 128
                    || active.is_some()
                {
                    return Err(internal(
                        "durable mutation admissions overlap or use an unsupported schema",
                    ));
                }
                active = Some(admission);
            }
            AuditKind::MutationCompleted => {
                let payload =
                    canonicalize(record.payload()).map_err(|error| internal(error.to_string()))?;
                let payload: Value = serde_json::from_slice(&payload)
                    .map_err(|error| internal(error.to_string()))?;
                if active.as_ref().is_none_or(|admission| {
                    payload.get("admission_id") != Some(&json!(admission.admission_id))
                }) {
                    return Err(internal(
                        "mutation completion does not match its durable admission",
                    ));
                }
                active = None;
            }
            _ => {}
        }
    }
    if ledger
        .projection()
        .map_err(|error| internal(error.to_string()))?
        .lifecycle
        == RunLifecycle::Closed
    {
        return Ok(None);
    }
    Ok(active)
}

pub(crate) fn admit_mutation<
    C: LedgerClock + 'static,
    F: FaultInjector + 'static,
    R: MutationRequest,
>(
    ledger: &mut Ledger<C, F>,
    controller_id: Uuid,
    expected_state_revision: u64,
    request: &R,
) -> Result<MutationAdmission, MachineError> {
    let identity = request.mutation_request_identity()?;
    admit_mutation_digest(
        ledger,
        controller_id,
        expected_state_revision,
        identity.request_sha256,
        identity.operation,
        identity.allow_writer_acquire,
    )
}

pub(crate) fn admit_mutation_digest<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &mut Ledger<C, F>,
    controller_id: Uuid,
    expected_state_revision: u64,
    digest: String,
    operation: &'static str,
    allow_writer_acquire: bool,
) -> Result<MutationAdmission, MachineError> {
    let projection = ledger
        .projection()
        .map_err(|error| internal(error.to_string()))?;
    if matches!(
        projection.lifecycle,
        RunLifecycle::Closed | RunLifecycle::StartFailed
    ) {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            operation,
        ));
    }
    if let Some(active) = active_mutation_admission(ledger)? {
        if active.controller_id == controller_id
            && active.expected_state_revision == expected_state_revision
            && active.request_sha256 == digest
            && active.operation == operation
            && active.allow_writer_acquire == allow_writer_acquire
        {
            return Ok(active);
        }
        if operation != "run.reconcile"
            || projection.ledger_head.sequence != expected_state_revision
        {
            return Err(state_conflict(
                projection.run_id,
                projection.lifecycle,
                operation,
            ));
        }
        complete_mutation_admission(ledger, active.admission_id)?;
    }
    if projection.ledger_head.sequence != expected_state_revision {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            operation,
        ));
    }
    let admission = MutationAdmission {
        schema_version: 1,
        admission_id: Uuid::now_v7(),
        controller_id,
        expected_state_revision,
        request_sha256: digest,
        operation: operation.to_owned(),
        allow_writer_acquire,
    };
    ledger
        .append_required_payload(
            AuditKind::MutationAdmitted,
            &admission,
            projection.run_generation,
        )
        .map_err(|error| internal(error.to_string()))?;
    Ok(admission)
}

pub(crate) fn validate_mutation_admission<
    C: LedgerClock + 'static,
    F: FaultInjector + 'static,
    R: MutationRequest,
>(
    ledger: &Ledger<C, F>,
    admission_id: Uuid,
    controller_id: Uuid,
    request: &R,
) -> Result<(), MachineError> {
    let projection = ledger
        .projection()
        .map_err(|error| internal(error.to_string()))?;
    let identity = request.mutation_request_identity()?;
    let Some(active) = active_mutation_admission(ledger)? else {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            identity.operation,
        ));
    };
    if active.admission_id != admission_id
        || active.controller_id != controller_id
        || !(active.request_sha256 == identity.request_sha256
            || (!identity.allow_writer_acquire || active.allow_writer_acquire)
                && identity
                    .compatible_admission_operations
                    .contains(&active.operation.as_str()))
    {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            identity.operation,
        ));
    }
    Ok(())
}

/// Release only a prepared write Turn that has not begun native acceptance.
pub(crate) fn abort_prepared_turn_admission<
    C: LedgerClock + 'static,
    F: FaultInjector + 'static,
    R: MutationRequest,
>(
    ledger: &mut Ledger<C, F>,
    admission_id: Uuid,
    controller_id: Uuid,
    request: &R,
) -> Result<(), MachineError> {
    let identity = request.mutation_request_identity()?;
    let projection = ledger
        .projection()
        .map_err(|error| internal(error.to_string()))?;
    let active = active_mutation_admission(ledger)?;
    let Some(active) = active.filter(|active| {
        matches!(
            projection.lifecycle,
            RunLifecycle::Starting | RunLifecycle::Idle
        ) && matches!(identity.operation, "run.send" | "run.submit")
            && active.admission_id == admission_id
            && active.controller_id == controller_id
            && active.operation == identity.operation
            && active.request_sha256 == identity.request_sha256
            && active.allow_writer_acquire
            && identity.allow_writer_acquire
    }) else {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            identity.operation,
        ));
    };
    if ledger
        .durable_records()
        .map_err(|error| internal(error.to_string()))?
        .iter()
        .any(|record| {
            record.sequence() > active.expected_state_revision
                && matches!(
                    record.kind(),
                    AuditKind::TurnIntent
                        | AuditKind::IdempotencyReserved
                        | AuditKind::ThreadBound
                        | AuditKind::TurnStarted
                        | AuditKind::TurnTerminal
                )
        })
    {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            identity.operation,
        ));
    }
    complete_mutation_admission(ledger, admission_id)
}

pub(crate) fn complete_mutation_admission<C: LedgerClock + 'static, F: FaultInjector + 'static>(
    ledger: &mut Ledger<C, F>,
    admission_id: Uuid,
) -> Result<(), MachineError> {
    let projection = ledger
        .projection()
        .map_err(|error| internal(error.to_string()))?;
    if projection.lifecycle == RunLifecycle::Closed {
        return Ok(());
    }
    let active = active_mutation_admission(ledger)?;
    if active
        .as_ref()
        .is_none_or(|active| active.admission_id != admission_id)
    {
        return Err(state_conflict(
            projection.run_id,
            projection.lifecycle,
            "complete_mutation",
        ));
    }
    ledger
        .append_required_payload(
            AuditKind::MutationCompleted,
            &json!({"schema_version":1,"admission_id":admission_id}),
            projection.run_generation,
        )
        .map_err(|error| internal(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    struct PreparedTurn {
        digest: String,
        operation: &'static str,
    }

    impl MutationRequest for PreparedTurn {
        fn mutation_request_identity(&self) -> Result<MutationRequestIdentity, MachineError> {
            Ok(MutationRequestIdentity::new(
                self.digest.clone(),
                self.operation,
                true,
                &[],
            ))
        }
    }

    fn idle_ledger() -> (std::path::PathBuf, Ledger) {
        let root = std::env::temp_dir().join(format!("dg-prepared-abort-{}", Uuid::now_v7()));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(root.join("recovery"))
            .unwrap();
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut ledger = Ledger::open(&root, Uuid::now_v7()).unwrap();
        ledger
            .append_conformance_payload(
                AuditKind::LifecycleTransition,
                &json!({"previous":"starting","current":"idle"}),
                1,
            )
            .unwrap();
        (root, ledger)
    }

    #[test]
    fn closed_ledger_rejects_fresh_admission_without_appending_after_its_seal() {
        let (root, mut ledger) = idle_ledger();
        ledger
            .append_conformance_payload(
                AuditKind::CleanupResult,
                &json!({"outcome":"owned runtime absent"}),
                1,
            )
            .unwrap();
        ledger
            .append_conformance_payload(
                AuditKind::LifecycleTransition,
                &json!({"previous":"idle","current":"closed","terminal_seal":true}),
                1,
            )
            .unwrap();
        let before = std::fs::read(root.join("audit.jsonl")).unwrap();
        let head = ledger.head().unwrap().sequence;
        assert_eq!(
            admit_mutation_digest(
                &mut ledger,
                Uuid::now_v7(),
                head,
                "a".repeat(64),
                "run.submit",
                false,
            )
            .unwrap_err()
            .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(std::fs::read(root.join("audit.jsonl")).unwrap(), before);
        assert!(active_mutation_admission(&ledger).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_turn_abort_requires_exact_authority_and_preserves_future_admission() {
        let (root, mut ledger) = idle_ledger();
        let controller = Uuid::now_v7();
        let request = PreparedTurn {
            digest: "a".repeat(64),
            operation: "run.submit",
        };
        let head = ledger.head().unwrap().sequence;
        let admission = admit_mutation(&mut ledger, controller, head, &request).unwrap();
        let admitted_head = ledger.head().unwrap().sequence;
        for (id, controller, request) in [
            (
                Uuid::now_v7(),
                controller,
                PreparedTurn {
                    digest: "a".repeat(64),
                    operation: "run.submit",
                },
            ),
            (
                admission.admission_id,
                Uuid::now_v7(),
                PreparedTurn {
                    digest: "a".repeat(64),
                    operation: "run.submit",
                },
            ),
            (
                admission.admission_id,
                controller,
                PreparedTurn {
                    digest: "b".repeat(64),
                    operation: "run.submit",
                },
            ),
            (
                admission.admission_id,
                controller,
                PreparedTurn {
                    digest: "a".repeat(64),
                    operation: "run.send",
                },
            ),
        ] {
            assert_eq!(
                abort_prepared_turn_admission(&mut ledger, id, controller, &request)
                    .unwrap_err()
                    .code,
                "RUN_STATE_CONFLICT"
            );
            assert_eq!(ledger.head().unwrap().sequence, admitted_head);
        }
        abort_prepared_turn_admission(&mut ledger, admission.admission_id, controller, &request)
            .unwrap();
        assert!(active_mutation_admission(&ledger).unwrap().is_none());
        assert_eq!(ledger.head().unwrap().sequence, admitted_head + 1);
        let run_id = ledger.projection().unwrap().run_id;
        drop(ledger);
        let mut ledger = Ledger::open(&root, run_id).unwrap();
        assert!(active_mutation_admission(&ledger).unwrap().is_none());
        let head = ledger.head().unwrap().sequence;
        admit_mutation(&mut ledger, controller, head, &request).unwrap();
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepared_turn_abort_refuses_durable_native_acceptance_prefix() {
        let (root, mut ledger) = idle_ledger();
        let controller = Uuid::now_v7();
        let request = PreparedTurn {
            digest: "a".repeat(64),
            operation: "run.send",
        };
        let head = ledger.head().unwrap().sequence;
        let admission = admit_mutation(&mut ledger, controller, head, &request).unwrap();
        ledger
            .append_required_payload(
                AuditKind::IdempotencyReserved,
                &json!({
                    "schema_version":1,"operation":"submit_turn","idempotency_key":"accepted",
                    "normalized_identity_sha256":"b".repeat(64),
                    "run_id":ledger.projection().unwrap().run_id
                }),
                1,
            )
            .unwrap();
        let head = ledger.head().unwrap().sequence;
        assert_eq!(
            abort_prepared_turn_admission(
                &mut ledger,
                admission.admission_id,
                controller,
                &request
            )
            .unwrap_err()
            .code,
            "RUN_STATE_CONFLICT"
        );
        assert_eq!(ledger.head().unwrap().sequence, head);
        assert_eq!(
            active_mutation_admission(&ledger)
                .unwrap()
                .unwrap()
                .admission_id,
            admission.admission_id
        );
        drop(ledger);
        std::fs::remove_dir_all(root).unwrap();
    }
}
