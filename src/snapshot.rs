//! Shared durable observation boundaries for the Machine and public adapters.

use crate::darwin::DarwinSystem;
use crate::domain::RunLifecycle;
use crate::ledger::ObservedLedger;
use crate::machine::MachineError;
use crate::projection::RunStateProjection;
use crate::run::{ControllerBinding, RunManifest, RunStore};
use crate::worker::{WorkerRuntimeRecord, read_runtime_record, runtime_record_path, runtime_root};
use crate::workspace::SystemWorkspacePlatform;
use crate::writer::{WriterRecord, WriterStore};
use std::path::Path;
use uuid::Uuid;

const CAPTURE_ATTEMPTS: usize = 3;

pub use crate::domain::ProjectionStamp;

/// Typed source DTO. Adapters may format these facts but cannot supply them.
#[derive(Clone, Debug)]
pub struct RunSnapshot {
    pub manifest: RunManifest,
    pub projection: RunStateProjection,
    pub writer: WriterRecord,
    pub controller: crate::domain::ControllerIdentity,
    controller_authority: Option<ControllerBinding>,
    pub runtime_record: Option<WorkerRuntimeRecord>,
    pub stamp: ProjectionStamp,
    pub control_socket_epoch: u64,
    pub app_server_epoch: Option<u64>,
    pub(crate) captured_effective_policy: crate::domain::EffectivePolicy,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CaptureSources {
    manifest: RunManifest,
    projection: RunStateProjection,
    writer: WriterRecord,
    controller: ControllerBinding,
    runtime_record: Option<WorkerRuntimeRecord>,
}

impl RunSnapshot {
    /// Authorize against the current binding and the captured observation.
    /// Protected readers call this both before and after reading their material.
    pub(crate) fn authorize_current_controller(
        &self,
        state_root: &Path,
        carrier: &crate::controller::CredentialCarrier,
        operation: &str,
    ) -> Result<(), MachineError> {
        let binding = crate::controller::load_reconciled_controller_binding(
            state_root,
            self.manifest.run_id,
        )?;
        crate::controller::authorize_controller(
            self.manifest.run_id,
            operation,
            &binding,
            carrier,
        )?;
        if &binding != self.controller_authority()? {
            return Err(MachineError::new(
                "RUN_STATE_CONFLICT",
                "refresh authoritative Run and aggregate snapshots",
                false,
                serde_json::json!({"run_id":self.manifest.run_id,"state":self.projection.lifecycle.as_str(),"operation":operation}),
            ));
        }
        Ok(())
    }

    /// Historical acceptance projections are never authorization snapshots.
    pub fn controller_authority(&self) -> Result<&ControllerBinding, MachineError> {
        self.controller_authority.as_ref().ok_or_else(|| {
            internal_runtime("historical acceptance cannot authorize a mutation".to_owned())
        })
    }

    /// Acceptance is already an immutable committed observation. Later worker
    /// events must not turn an accepted mutation into a snapshot-capture error.
    pub fn for_control_response(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
        response: &crate::worker::ControlResponseV1,
    ) -> Result<Self, MachineError> {
        let crate::worker::ControlResponseV1::AcceptedReceipt {
            stamp,
            state,
            writer,
            controller,
            effective_policy,
            server_key,
            server_epoch,
            ..
        } = response
        else {
            return Self::load(state_root, run_id, control_socket_epoch);
        };
        let manifest = RunStore::new(SystemWorkspacePlatform, state_root).load_manifest(run_id)?;
        if state.run_id != run_id
            || manifest
                .global_profile_binding
                .as_ref()
                .is_some_and(|binding| binding.server_key != *server_key)
        {
            return Err(internal_runtime(
                "acceptance receipt does not match Run identity".to_owned(),
            ));
        }
        Ok(Self {
            manifest,
            projection: state.clone(),
            writer: writer.as_ref().clone(),
            controller: controller.clone(),
            controller_authority: None,
            runtime_record: None,
            stamp: stamp.clone(),
            control_socket_epoch: 0,
            app_server_epoch: Some(*server_epoch),
            captured_effective_policy: effective_policy.clone(),
        })
    }

    pub fn load(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
    ) -> Result<Self, MachineError> {
        Self::capture(state_root, run_id, control_socket_epoch, |_, _, _| Ok(()))
            .map(|(snapshot, ())| snapshot)
    }

    /// Capture read-only runtime and durable projection facts at the same boundary.
    pub fn observe(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
    ) -> Result<(Self, RunObservation), MachineError> {
        Self::capture(
            state_root,
            run_id,
            control_socket_epoch,
            RunObservation::read,
        )
    }

    fn capture<T>(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
        mut observe: impl FnMut(
            &CaptureSources,
            Option<&ObservedLedger>,
            &crate::domain::EffectivePolicy,
        ) -> Result<T, MachineError>,
    ) -> Result<(Self, T), MachineError> {
        let store = RunStore::new(SystemWorkspacePlatform, state_root);
        let captured = capture_consistent(
            CAPTURE_ATTEMPTS,
            || CaptureSources::read(&store, state_root, run_id),
            |sources| {
                let head = sources.projection.ledger_head.sequence;
                let ledger = (head != 0)
                    .then(|| ObservedLedger::open_run(state_root, run_id, head))
                    .transpose()?;
                let revision = ledger
                    .as_ref()
                    .map_or(0, ObservedLedger::interaction_state_revision);
                let effective_policy = durable_policy(sources, ledger.as_ref())?;
                let observation = observe(sources, ledger.as_ref(), &effective_policy)?;
                Ok((revision, effective_policy, observation))
            },
        )?;
        let Some((sources, (interaction_state_revision, effective_policy, observation))) = captured
        else {
            return Err(state_conflict(
                run_id,
                store.load_state_projection(run_id)?.lifecycle,
                "capture_snapshot",
            ));
        };
        let stamp = ProjectionStamp {
            captured_head_cursor: sources.projection.ledger_head.sequence.to_string(),
            run_state_revision: sources.projection.ledger_head.sequence,
            writer_state_revision: sources.writer.authority_revision,
            interaction_state_revision,
        };
        Ok((
            Self {
                app_server_epoch: sources
                    .runtime_record
                    .as_ref()
                    .and_then(|record| record.app_server_epoch),
                control_socket_epoch,
                manifest: sources.manifest,
                projection: sources.projection,
                writer: sources.writer,
                controller: sources.controller.identity.clone(),
                controller_authority: Some(sources.controller),
                runtime_record: sources.runtime_record,
                stamp,
                captured_effective_policy: effective_policy,
            },
            observation,
        ))
    }

    /// Used by a mutation owner only while its serialization boundary is held.
    /// Exact idempotency replay lookup must precede this new-operation gate.
    pub fn require_revision(&self, expected: u64, operation: &str) -> Result<(), MachineError> {
        if self.stamp.run_state_revision == expected {
            Ok(())
        } else {
            Err(state_conflict(
                self.manifest.run_id,
                self.projection.lifecycle,
                operation,
            ))
        }
    }
}

impl CaptureSources {
    fn read(
        store: &RunStore<SystemWorkspacePlatform>,
        state_root: &Path,
        run_id: Uuid,
    ) -> Result<Self, MachineError> {
        let uid = DarwinSystem.current_uid();
        let manifest = store.load_manifest(run_id)?;
        let writer = WriterStore::new(state_root, &manifest.workspace_id, uid).load()?;
        let projection = store.load_state_projection(run_id)?;
        let controller = store.load_controller_binding(run_id)?;
        let path = runtime_record_path(&runtime_root(state_root), run_id)
            .map_err(|error| internal_runtime(format!("{error:?}")))?;
        let runtime_record = match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(internal_runtime(error.to_string())),
            Ok(_) => Some(
                read_runtime_record(&path, uid)
                    .map_err(|error| internal_runtime(format!("{error:?}")))?,
            ),
        };
        if runtime_record
            .as_ref()
            .is_some_and(|record| record.identity.run_id != run_id)
        {
            return Err(internal_runtime(
                "runtime identity crossed the requested Run".to_owned(),
            ));
        }
        Ok(Self {
            manifest,
            projection,
            writer,
            controller,
            runtime_record,
        })
    }
}

fn internal_runtime(reason: String) -> MachineError {
    MachineError::new(
        "INTERNAL_ERROR",
        "snapshot runtime record is invalid",
        false,
        serde_json::json!({"invariant": reason}),
    )
}

pub(crate) fn state_conflict(
    run_id: Uuid,
    lifecycle: RunLifecycle,
    operation: &str,
) -> MachineError {
    MachineError::new(
        "RUN_STATE_CONFLICT",
        "run state changed",
        false,
        serde_json::json!({"run_id": run_id, "state": lifecycle, "operation": operation}),
    )
}

/// Compare every source field, including observation fields whose revision did not advance.
fn capture_consistent<S: PartialEq, O, E>(
    attempts: usize,
    mut read: impl FnMut() -> Result<S, E>,
    mut observe: impl FnMut(&S) -> Result<O, E>,
) -> Result<Option<(S, O)>, E> {
    for _ in 0..attempts {
        let before = read()?;
        let observation = observe(&before)?;
        let after = read()?;
        if before == after {
            return Ok(Some((before, observation)));
        }
    }
    Ok(None)
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackgroundObservation {
    NotApplicable,
    Unstarted,
    VerifiedAbsent(crate::worker::BackgroundAbsenceEvidence),
    Unverified,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedTurnStatus {
    Running,
    WaitingInteraction,
    OutcomeUnknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RunObservation {
    pub effective_policy: crate::domain::EffectivePolicy,
    pub worker_identity: crate::worker::ProcessIdentityVerdict,
    pub server_identity: Option<crate::worker::ProcessIdentityVerdict>,
    pub background: BackgroundObservation,
    pub active_turn_status: Option<ObservedTurnStatus>,
    pub last_final_response: Option<crate::turn::FinalResponse>,
}

impl RunObservation {
    fn read(
        sources: &CaptureSources,
        ledger: Option<&ObservedLedger>,
        effective_policy: &crate::domain::EffectivePolicy,
    ) -> Result<Self, MachineError> {
        use crate::worker::{
            ProcessIdentityVerdict, classify_dedicated_server_identity, classify_worker_identity,
        };
        let worker_identity = sources
            .runtime_record
            .as_ref()
            .map_or(ProcessIdentityVerdict::Absent, classify_worker_identity);
        let server_identity = sources
            .runtime_record
            .as_ref()
            .and_then(|record| record.dedicated_server_identity.as_ref())
            .map(classify_dedicated_server_identity);
        let background =
            if sources.manifest.execution_lane == crate::domain::ExecutionLane::SharedReadonly {
                BackgroundObservation::NotApplicable
            } else if sources.projection.thread_id.is_none() && server_identity.is_none() {
                BackgroundObservation::Unstarted
            } else if let Some(record) = &sources.runtime_record {
                match crate::worker::prove_worker_workload_absent(record) {
                    Ok(evidence) => BackgroundObservation::VerifiedAbsent(evidence),
                    Err(_) => BackgroundObservation::Unverified,
                }
            } else {
                BackgroundObservation::Unverified
            };
        let active_turn_status = sources
            .projection
            .active_turn_id
            .as_ref()
            .map(|_| match sources.projection.lifecycle {
                RunLifecycle::Running => Ok(ObservedTurnStatus::Running),
                RunLifecycle::WaitingInteraction => Ok(ObservedTurnStatus::WaitingInteraction),
                RunLifecycle::OutcomeUnknown | RunLifecycle::ReconciliationRequired => {
                    Ok(ObservedTurnStatus::OutcomeUnknown)
                }
                _ => Err(state_conflict(
                    sources.manifest.run_id,
                    sources.projection.lifecycle,
                    "capture_snapshot",
                )),
            })
            .transpose()?;
        let last_final_response = ledger
            .map(ObservedLedger::last_terminal)
            .transpose()
            .map_err(|error| internal_runtime(error.to_string()))?
            .flatten()
            .map(|terminal| {
                serde_json::from_value::<crate::turn::TerminalTurn>(terminal)
                    .map(|terminal| terminal.final_response)
                    .map_err(|error| internal_runtime(error.to_string()))
            })
            .transpose()?
            .flatten();
        // A process identity changed during the census; the record equality check alone cannot catch it.
        if sources
            .runtime_record
            .as_ref()
            .map_or(ProcessIdentityVerdict::Absent, classify_worker_identity)
            != worker_identity
            || sources
                .runtime_record
                .as_ref()
                .and_then(|record| record.dedicated_server_identity.as_ref())
                .map(classify_dedicated_server_identity)
                != server_identity
        {
            return Err(state_conflict(
                sources.manifest.run_id,
                sources.projection.lifecycle,
                "capture_snapshot",
            ));
        }
        Ok(Self {
            effective_policy: effective_policy.clone(),
            worker_identity,
            server_identity,
            background,
            active_turn_status,
            last_final_response,
        })
    }
}

fn durable_policy(
    sources: &CaptureSources,
    ledger: Option<&ObservedLedger>,
) -> Result<crate::domain::EffectivePolicy, MachineError> {
    let server_epoch = sources
        .runtime_record
        .as_ref()
        .and_then(|record| record.app_server_epoch);
    let policy = policy_at_head(&sources.projection, ledger, server_epoch)?;
    if sources.projection.thread_id.is_none() {
        return Ok(policy);
    }
    if server_epoch.is_some_and(|epoch| Some(epoch) != policy.server_epoch)
        || (policy.access == crate::domain::Access::Write
            && (!sources
                .writer
                .holder
                .as_ref()
                .is_some_and(|holder| holder.run_id == sources.manifest.run_id)
                || sources.writer.state != crate::writer::WriterAuthorityState::Active
                || policy.writer_generation != Some(sources.writer.writer_generation)))
    {
        return Err(state_conflict(
            sources.manifest.run_id,
            sources.projection.lifecycle,
            "capture_snapshot",
        ));
    }
    Ok(policy)
}

fn policy_at_head(
    projection: &RunStateProjection,
    ledger: Option<&ObservedLedger>,
    server_epoch: Option<u64>,
) -> Result<crate::domain::EffectivePolicy, MachineError> {
    use crate::domain::{Access, EffectivePolicy, PolicyEpoch, PolicyVerification};
    let conflict = || state_conflict(projection.run_id, projection.lifecycle, "capture_snapshot");
    if projection.thread_id.is_none() {
        return Ok(EffectivePolicy {
            access: Access::Unknown,
            verification: PolicyVerification::Unverified,
            policy_epoch: PolicyEpoch(0),
            thread_generation: None,
            server_epoch: None,
            writer_generation: None,
        });
    }
    let ledger = ledger.ok_or_else(conflict)?;
    let bindings = ledger
        .payloads_of_kind(crate::audit::AuditKind::ThreadBound)
        .map_err(|error| internal_runtime(error.to_string()))?;
    let generation = bindings
        .iter()
        .rev()
        .find(|payload| {
            payload.get("thread_id").and_then(serde_json::Value::as_str)
                == projection.thread_id.as_deref()
        })
        .and_then(|payload| payload.get("thread_generation"))
        .and_then(serde_json::Value::as_u64)
        .filter(|generation| *generation > 0)
        .ok_or_else(conflict)?;
    let mut payloads = Vec::new();
    for kind in [
        crate::audit::AuditKind::ThreadBound,
        crate::audit::AuditKind::WriterAcquired,
        crate::audit::AuditKind::WriterReleased,
    ] {
        payloads.extend(
            ledger
                .payloads_of_kind(kind)
                .map_err(|error| internal_runtime(error.to_string()))?,
        );
    }
    Ok(
        policy_from_payloads(generation, payloads)?.unwrap_or(EffectivePolicy {
            access: Access::Unknown,
            verification: PolicyVerification::Unverified,
            policy_epoch: PolicyEpoch(0),
            thread_generation: Some(generation),
            server_epoch,
            writer_generation: None,
        }),
    )
}

fn policy_from_payloads(
    generation: u64,
    payloads: impl IntoIterator<Item = serde_json::Value>,
) -> Result<Option<crate::domain::EffectivePolicy>, MachineError> {
    let mut policy: Option<crate::domain::EffectivePolicy> = None;
    for payload in payloads {
        let Some(value) = payload.get("effective_policy") else {
            continue;
        };
        let candidate: crate::domain::EffectivePolicy = serde_json::from_value(value.clone())
            .map_err(|error| internal_runtime(error.to_string()))?;
        if candidate.thread_generation != Some(generation) {
            continue;
        }
        if policy.as_ref().is_some_and(|previous| {
            previous.policy_epoch == candidate.policy_epoch && *previous != candidate
        }) {
            return Err(internal_runtime(
                "one policy epoch has conflicting durable observations".to_owned(),
            ));
        }
        if policy
            .as_ref()
            .is_none_or(|previous| previous.policy_epoch.0 < candidate.policy_epoch.0)
        {
            policy = Some(candidate);
        }
    }
    Ok(policy)
}

#[cfg(test)]
mod capture_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn same_revision_writer_observation_change_requires_recapture() {
        let mut original = WriterRecord::empty("workspace");
        original.authority_revision = 9;
        original.holder = Some(crate::writer::WriterHolder {
            run_id: Uuid::now_v7(),
            profile: "profile".to_owned(),
            controller_id: Uuid::now_v7(),
            controller_generation: 1,
            run_generation: 1,
            worker_generation: 1,
            profile_server_key: "server".to_owned(),
            profile_server_epoch: 1,
            thread_id: Some("thread".to_owned()),
            lifecycle: RunLifecycle::Idle,
            active_turn_id: None,
            pending_interaction_count: 0,
            last_event_cursor: Some("10".to_owned()),
        });
        let mut changed = original.clone();
        changed.holder.as_mut().unwrap().pending_interaction_count = 1;
        let records = [original, changed.clone(), changed.clone(), changed.clone()];
        let reads = Cell::new(0);
        let result = capture_consistent(
            3,
            || {
                let index = reads.get();
                reads.set(index + 1);
                Ok::<_, ()>(records[index].clone())
            },
            |writer| Ok(writer.holder.as_ref().unwrap().pending_interaction_count),
        )
        .unwrap()
        .unwrap();
        assert_eq!(reads.get(), 4);
        assert_eq!(result.0, changed);
        assert_eq!(result.1, 1);
    }

    #[test]
    fn continuously_changing_observations_exhaust_the_bounded_capture() {
        let revision = Cell::new(0);
        let captured = capture_consistent(
            3,
            || {
                let current = revision.get();
                revision.set(current + 1);
                Ok::<_, ()>(current)
            },
            |source| Ok(*source),
        )
        .unwrap();
        assert!(captured.is_none());
        assert_eq!(revision.get(), 6);
    }
}

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};

    fn payload(epoch: u64, generation: u64) -> serde_json::Value {
        serde_json::json!({"effective_policy": {
            "access":"unknown", "verification":"unverified", "policy_epoch":epoch,
            "thread_generation":generation, "server_epoch":7, "writer_generation":null,
        }})
    }

    #[test]
    fn historical_policy_selection_uses_durable_epoch_and_matching_thread_generation() {
        let selected = policy_from_payloads(3, [payload(8, 3), payload(20, 2), payload(11, 3)])
            .unwrap()
            .unwrap();
        assert_eq!(selected.policy_epoch.0, 11);
        assert_eq!(
            selected.verification,
            crate::domain::PolicyVerification::Unverified
        );
        assert!(policy_from_payloads(4, [payload(8, 3)]).unwrap().is_none());
    }

    #[test]
    fn captured_ledger_head_preserves_policy_before_a_later_thread_generation() {
        use crate::audit::{AuditKind, AuditRecord, GENESIS_PREVIOUS_HASH};
        use std::io::Write as _;

        let run_id = Uuid::now_v7();
        let root = std::env::temp_dir().join(format!("dolgorae-policy-head-{run_id}"));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(root.join("audit.jsonl"))
            .unwrap();
        let mut previous = GENESIS_PREVIOUS_HASH.to_owned();
        for (index, (kind, epoch, generation)) in [
            (AuditKind::ThreadBound, 8, 3),
            (AuditKind::WriterReleased, 11, 3),
            (AuditKind::ThreadBound, 20, 4),
        ]
        .into_iter()
        .enumerate()
        {
            let mut value = payload(epoch, generation);
            value["thread_id"] = serde_json::json!("same-thread");
            value["thread_generation"] = serde_json::json!(generation);
            let record = AuditRecord::new(
                index as u64 + 1,
                "2026-09-30T00:00:00.000000Z",
                run_id,
                generation,
                kind,
                crate::jcs::parse(&value.to_string()).unwrap(),
                &previous,
            )
            .unwrap();
            file.write_all(&record.canonical_line().unwrap()).unwrap();
            previous = record.hash().to_owned();
        }
        file.sync_all().unwrap();
        let mut projection = RunStateProjection::starting(run_id);
        projection.thread_id = Some("same-thread".to_owned());
        for (head, epoch, generation) in [(2, 11, 3), (3, 20, 4)] {
            projection.ledger_head.sequence = head;
            let observed = ObservedLedger::open(&root, run_id, head).unwrap();
            let selected = policy_at_head(&projection, Some(&observed), None).unwrap();
            assert_eq!(selected.policy_epoch.0, epoch);
            assert_eq!(selected.thread_generation, Some(generation));
        }
    }

    #[test]
    fn unstarted_run_has_no_thread_policy_generation() {
        let projection = RunStateProjection::starting(Uuid::now_v7());
        let selected = policy_at_head(&projection, None, Some(7)).unwrap();
        assert_eq!(selected.policy_epoch.0, 0);
        assert_eq!(selected.thread_generation, None);
        assert_eq!(selected.server_epoch, None);
    }

    #[test]
    fn conflicting_same_epoch_policy_is_never_selected_arbitrarily() {
        let mut conflict = payload(8, 3);
        conflict["effective_policy"]["access"] = serde_json::json!("read");
        assert!(policy_from_payloads(3, [payload(8, 3), conflict]).is_err());
    }
}
