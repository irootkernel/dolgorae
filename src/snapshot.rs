//! Shared durable observation boundaries for the Machine and public adapters.

use crate::darwin::DarwinSystem;
use crate::domain::RunLifecycle;
use crate::ledger::{LedgerError, ObservedLedger};
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
    broker_approval_view: Result<Vec<crate::interaction::BrokerApprovalBinding>, MachineError>,
    pub writer: WriterRecord,
    pub controller: crate::domain::ControllerIdentity,
    controller_authority: Option<ControllerBinding>,
    captured_ledger: Option<ObservedLedger>,
    display_only: bool,
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

/// Run/source facts for the delegation service's enclosing read transaction.
/// Capture reads no ledger prefix and performs no nested retry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MemberRunObservation {
    sources: Option<MemberRunSources>,
    #[cfg(test)]
    fixture: Option<String>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
struct MemberRunSources {
    manifest: RunManifest,
    projection: RunStateProjection,
    controller: ControllerBinding,
    runtime_record: Option<WorkerRuntimeRecord>,
    generation: u64,
    supervisor: Option<(RunManifest, ControllerBinding)>,
    writer_revision: u64,
    selection: String,
    files: Vec<crate::ledger_generation::ObservationFileIdentity>,
    run_files: Vec<crate::ledger_generation::ObservationFileIdentity>,
}
impl MemberRunObservation {
    pub(crate) fn read(
        state_root: &Path,
        run_id: Uuid,
        supervisor_id: Option<Uuid>,
        budget: &mut crate::ledger::ObservationBudget,
    ) -> Result<Self, MachineError> {
        budget
            .check()
            .map_err(|e| crate::ledger::observation_error(run_id, 0, e))?;
        let store = RunStore::new(SystemWorkspacePlatform, state_root);
        let manifest = store.load_manifest(run_id)?;
        let projection = store.load_state_projection(run_id)?;
        let controller = store.load_controller_binding_from_manifest(&manifest)?;
        let path = runtime_record_path(&runtime_root(state_root), run_id)
            .map_err(|e| internal_runtime(format!("{e:?}")))?;
        let runtime_record = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(internal_runtime(e.to_string())),
            Ok(_) => Some(
                read_runtime_record(&path, DarwinSystem.current_uid())
                    .map_err(|e| internal_runtime(format!("{e:?}")))?,
            ),
        };
        if runtime_record
            .as_ref()
            .is_some_and(|r| r.identity.run_id != run_id)
        {
            return Err(internal_runtime(
                "runtime identity crossed the member Run".to_owned(),
            ));
        }
        if runtime_record.as_ref().is_some_and(|record| {
            record.identity.workspace_id != manifest.workspace_id
                || record.identity.run_generation != projection.run_generation
                || record.control_recovery_required
        }) {
            return Err(MachineError::new(
                "RECOVERY_REQUIRED",
                "member runtime does not match its current Run observation",
                false,
                serde_json::json!({"required_action":"restore_state"}),
            ));
        }
        let supervisor = supervisor_id
            .map(|id| {
                Ok::<_, MachineError>((
                    store.load_manifest(id)?,
                    store.load_controller_binding(id)?,
                ))
            })
            .transpose()?;
        let writer = WriterStore::new(
            state_root,
            &manifest.workspace_id,
            DarwinSystem.current_uid(),
        );
        let writer_revision = if writer.record_exists() {
            writer.load()?.authority_revision
        } else {
            0
        };
        let (generation, selection, files) = crate::ledger_generation::observation_selection(
            &state_root.join("runs").join(run_id.to_string()),
            run_id,
            budget,
        )
        .map_err(|e| {
            crate::ledger::observation_error(run_id, projection.ledger_head.sequence, e)
        })?;
        let mut run_files = Vec::new();
        for id in std::iter::once(run_id).chain(supervisor_id) {
            let root = state_root.join("runs").join(id.to_string());
            for name in ["manifest.json", "controller.json", "state.json"] {
                let path = root.join(name);
                match crate::ledger_generation::observation_file_identity(&path) {
                    Ok(identity) => run_files.push(identity),
                    Err(crate::ledger::LedgerError::Io(e))
                        if name == "state.json" && e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => {
                        return Err(crate::ledger::observation_error(
                            id,
                            projection.ledger_head.sequence,
                            e,
                        ));
                    }
                }
            }
        }
        if runtime_record.is_some() {
            run_files.push(
                crate::ledger_generation::observation_file_identity(&path).map_err(|e| {
                    crate::ledger::observation_error(run_id, projection.ledger_head.sequence, e)
                })?,
            );
        }
        Ok(Self {
            sources: Some(MemberRunSources {
                manifest,
                projection,
                controller,
                runtime_record,
                generation,
                supervisor,
                writer_revision,
                selection,
                files,
                run_files,
            }),
            #[cfg(test)]
            fixture: None,
        })
    }
    pub(crate) fn complete_ledger(
        &self,
        state_root: &Path,
        budget: &mut crate::ledger::ObservationBudget,
    ) -> Result<ObservedLedger, MachineError> {
        let source = self
            .sources
            .as_ref()
            .ok_or_else(|| internal_runtime("fixture has no authoritative history".to_owned()))?;
        let run_id = source.manifest.run_id;
        let head = &source.projection.ledger_head;
        let root = state_root.join("runs").join(run_id.to_string());
        let ledger = crate::ledger::ObservedLedger::open_complete_bounded(
            &root,
            run_id,
            head.sequence,
            budget,
        )
        .map_err(|e| crate::ledger::observation_error(run_id, head.sequence, e))?;
        if ledger.records().last().map_or(
            crate::audit::GENESIS_PREVIOUS_HASH,
            crate::audit::AuditRecord::hash,
        ) != head.hash
        {
            return Err(state_conflict(
                run_id,
                source.projection.lifecycle,
                "delegation_observation",
            ));
        }
        let replayed = crate::ledger::project_observation(run_id, ledger.records(), budget)
            .map_err(|e| crate::ledger::observation_error(run_id, head.sequence, e))?;
        if replayed != source.projection {
            return Err(MachineError::new(
                "RECOVERY_REQUIRED",
                "Run projection differs from its complete verified history",
                false,
                serde_json::json!({"required_action":"restore_state"}),
            ));
        }
        Ok(ledger)
    }
    pub(crate) fn interaction_context(
        &self,
    ) -> Result<
        (
            &RunManifest,
            &RunStateProjection,
            &ControllerBinding,
            Option<u64>,
            u64,
        ),
        MachineError,
    > {
        let source = self
            .sources
            .as_ref()
            .ok_or_else(|| internal_runtime("fixture has no authoritative history".to_owned()))?;
        Ok((
            &source.manifest,
            &source.projection,
            &source.controller,
            source
                .runtime_record
                .as_ref()
                .and_then(|r| r.app_server_epoch),
            source.writer_revision,
        ))
    }
    pub(crate) fn matches_stamp(&self, stamp: &ProjectionStamp) -> bool {
        self.sources.as_ref().is_some_and(|source| {
            stamp.run_state_revision == source.projection.ledger_head.sequence
                && stamp.captured_head_cursor == source.projection.ledger_head.sequence.to_string()
                && stamp.writer_state_revision == source.writer_revision
                && stamp.interaction_state_revision <= stamp.run_state_revision
        }) || cfg!(test) && self.sources.is_none()
    }
    pub(crate) fn matches_manifest(&self, manifest: &RunManifest) -> bool {
        self.sources
            .as_ref()
            .is_some_and(|sources| sources.manifest == *manifest)
            || cfg!(test) && self.sources.is_none()
    }
    pub(crate) fn matches_supervisor(&self, controller: &ControllerBinding) -> bool {
        self.sources.as_ref().is_some_and(|sources| {
            sources
                .supervisor
                .as_ref()
                .is_some_and(|(_, actual)| actual == controller)
        }) || cfg!(test) && self.sources.is_none()
    }
    #[cfg(test)]
    pub(crate) fn differing_components(&self) -> Vec<(&'static str, Self)> {
        let mut variants = Vec::new();
        macro_rules! differing {
            ($name:literal,$change:expr) => {{
                let mut changed = self.clone();
                $change(changed.sources.as_mut().unwrap());
                variants.push(($name, changed));
            }};
        }
        differing!("manifest fingerprint", |s: &mut MemberRunSources| s
            .manifest
            .created_at =
            "2026-10-26T00:00:01.000000Z".to_owned());
        differing!("Run generation", |s: &mut MemberRunSources| s
            .projection
            .run_generation +=
            1);
        differing!("lifecycle", |s: &mut MemberRunSources| s
            .projection
            .lifecycle =
            RunLifecycle::Paused);
        differing!("active Turn", |s: &mut MemberRunSources| s
            .projection
            .active_turn_id =
            Some("other-turn".to_owned()));
        differing!("projection revision", |s: &mut MemberRunSources| s
            .projection
            .ledger_head
            .sequence +=
            1);
        differing!("head hash", |s: &mut MemberRunSources| s
            .projection
            .ledger_head
            .hash =
            "f".repeat(64));
        differing!("writer revision", |s: &mut MemberRunSources| s
            .writer_revision +=
            1);
        differing!("Controller identity", |s: &mut MemberRunSources| s
            .controller
            .identity
            .controller_id =
            Uuid::now_v7());
        differing!("Controller generation", |s: &mut MemberRunSources| s
            .controller
            .identity
            .generation +=
            1);
        differing!(
            "Controller binding fingerprint",
            |s: &mut MemberRunSources| s.controller.capability_sha256 = "f".repeat(64)
        );
        differing!("selected generation", |s: &mut MemberRunSources| s
            .generation +=
            1);
        differing!("selection fingerprint", |s: &mut MemberRunSources| s
            .selection =
            "f".repeat(64));
        differing!("selected source inode", |s: &mut MemberRunSources| s
            .files[0]
            .inode +=
            1);
        differing!("selected source length", |s: &mut MemberRunSources| {
            let file = s
                .files
                .iter_mut()
                .find(|f| f.path.ends_with("audit.jsonl"))
                .unwrap();
            file.length += 1;
        });
        differing!("same-size source ctime", |s: &mut MemberRunSources| {
            let file = s
                .files
                .iter_mut()
                .find(|f| f.path.ends_with("audit.jsonl"))
                .unwrap();
            file.changed.1 += 1;
        });
        differing!("Run file identity", |s: &mut MemberRunSources| s
            .run_files[0]
            .inode +=
            1);
        if self.sources.as_ref().unwrap().runtime_record.is_some() {
            differing!("control socket epoch", |s: &mut MemberRunSources| s
                .runtime_record
                .as_mut()
                .unwrap()
                .control_socket_epoch +=
                1);
            differing!("App Server epoch", |s: &mut MemberRunSources| s
                .runtime_record
                .as_mut()
                .unwrap()
                .app_server_epoch =
                Some(2));
            differing!("runtime fingerprint", |s: &mut MemberRunSources| s
                .runtime_record
                .as_mut()
                .unwrap()
                .binary_sha256 =
                "f".repeat(64));
        }
        if self.sources.as_ref().unwrap().supervisor.is_some() {
            differing!("root Controller binding", |s: &mut MemberRunSources| s
                .supervisor
                .as_mut()
                .unwrap()
                .1
                .identity
                .generation +=
                1);
            differing!("root manifest fingerprint", |s: &mut MemberRunSources| s
                .supervisor
                .as_mut()
                .unwrap()
                .0
                .created_at =
                "2026-10-26T00:00:01.000000Z".to_owned());
        }
        variants
    }
    #[cfg(test)]
    pub(crate) fn fixture(fingerprint: String) -> Self {
        Self {
            sources: None,
            fixture: Some(fingerprint),
        }
    }
}

impl RunSnapshot {
    pub(crate) fn startup_ownership_blocker(&self) -> Option<&'static str> {
        (self
            .runtime_record
            .as_ref()
            .is_some_and(|record| record.startup_ownership_incomplete())
            && self
                .writer
                .pending_generation_release(self.manifest.run_id)
                .is_none()
            && self
                .writer
                .acquire_retirement_recovery(self.manifest.run_id)
                .is_none())
        .then_some(crate::worker::STARTUP_OWNERSHIP_INCOMPLETE)
    }

    pub(crate) fn generation_release_blocker(&self) -> Option<&'static str> {
        let release = self
            .writer
            .pending_generation_release(self.manifest.run_id)?;
        Some(
            if release.phase == crate::writer::GenerationReleasePhase::Prepared
                && release.scope_sha256.is_none()
            {
                "generation_release_prepared_scope_missing"
            } else {
                "generation_release_pending"
            },
        )
    }

    pub(crate) fn broker_approvals(
        &self,
    ) -> Result<&[crate::interaction::BrokerApprovalBinding], MachineError> {
        self.broker_approval_view.as_deref().map_err(Clone::clone)
    }

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
            broker_approval_view: Ok(Vec::new()),
            writer: writer.as_ref().clone(),
            controller: controller.clone(),
            controller_authority: None,
            captured_ledger: None,
            display_only: false,
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
        Self::capture(
            state_root,
            run_id,
            control_socket_epoch,
            false,
            |_, _, _| Ok(()),
        )
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
            false,
            |sources, ledger, policy| {
                RunObservation::read(Some(state_root), sources, ledger, policy)
            },
        )
    }

    /// Display-only history reuse. All manifest, writer, controller and runtime
    /// sources are still captured afresh and compared at the same boundary.
    pub fn load_display(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
    ) -> Result<Self, MachineError> {
        Self::capture(state_root, run_id, control_socket_epoch, true, |_, _, _| {
            Ok(())
        })
        .map(|(snapshot, ())| snapshot)
    }

    pub fn observe_display(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
    ) -> Result<(Self, RunObservation), MachineError> {
        Self::capture(
            state_root,
            run_id,
            control_socket_epoch,
            true,
            RunObservation::read_display,
        )
    }

    /// Reuse the exact verified history captured with this snapshot, rather than
    /// selecting a potentially different ledger generation for each projection.
    pub(crate) fn observed_ledger(
        &self,
        state_root: &Path,
    ) -> Result<ObservedLedger, MachineError> {
        if let Some(ledger) = &self.captured_ledger {
            return Ok(ledger.clone());
        }
        ObservedLedger::open_run(
            state_root,
            self.manifest.run_id,
            self.stamp.run_state_revision,
        )
    }

    fn capture<T>(
        state_root: &Path,
        run_id: Uuid,
        control_socket_epoch: u64,
        display: bool,
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
                let ledger = if head == 0 {
                    None
                } else if display {
                    let Some(ledger) = display_ledger_result(
                        ObservedLedger::open_run_display(state_root, run_id, head),
                        run_id,
                        head,
                        sources.projection.lifecycle,
                    )?
                    else {
                        return Ok(None);
                    };
                    Some(ledger)
                } else {
                    Some(ObservedLedger::open_run(state_root, run_id, head)?)
                };
                if ledger
                    .as_ref()
                    .and_then(|ledger| ledger.records().last())
                    .is_some_and(|record| record.hash() != sources.projection.ledger_head.hash)
                {
                    #[cfg(all(test, dolgorae_native_acceptance))]
                    crate::worker::native_acceptance_run_diagnostic(
                        state_root,
                        run_id,
                        "snapshot_ledger",
                        "head_mismatch",
                    );
                    return Err(state_conflict(
                        run_id,
                        sources.projection.lifecycle,
                        "capture_snapshot",
                    ));
                }
                let revision = ledger
                    .as_ref()
                    .map_or(0, ObservedLedger::interaction_state_revision);
                let broker_approvals = if let Some(ledger) = ledger.as_ref() {
                    crate::orchestration::approval::checked_bindings(
                        state_root,
                        &sources.manifest,
                        ledger,
                    )
                } else {
                    Ok(Vec::new())
                };
                let policy = durable_policy(sources, ledger.as_ref());
                #[cfg(all(test, dolgorae_native_acceptance))]
                if policy.is_err() {
                    let head = policy_at_head(
                        &sources.projection,
                        ledger.as_ref(),
                        sources
                            .runtime_record
                            .as_ref()
                            .and_then(|r| r.app_server_epoch),
                    );
                    crate::worker::native_acceptance_run_diagnostic(
                        state_root,
                        run_id,
                        "snapshot_policy",
                        native_snapshot_policy_category(sources, head.as_ref().ok()),
                    );
                }
                let effective_policy = policy?;
                let observation = observe(sources, ledger.as_ref(), &effective_policy)?;
                Ok(Some((
                    revision,
                    effective_policy,
                    observation,
                    ledger,
                    broker_approvals,
                )))
            },
        )?;
        let Some((
            sources,
            (interaction_state_revision, effective_policy, observation, ledger, broker_approvals),
        )) = captured
        else {
            #[cfg(all(test, dolgorae_native_acceptance))]
            crate::worker::native_acceptance_run_diagnostic(
                state_root,
                run_id,
                "snapshot_sources",
                "source_motion_exhausted",
            );
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
                control_socket_epoch: if display {
                    sources
                        .runtime_record
                        .as_ref()
                        .map_or(1, |record| record.control_socket_epoch)
                } else {
                    control_socket_epoch
                },
                manifest: sources.manifest,
                projection: sources.projection,
                broker_approval_view: broker_approvals,
                writer: sources.writer,
                controller: sources.controller.identity.clone(),
                controller_authority: Some(sources.controller),
                captured_ledger: ledger,
                display_only: display,
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
        if !self.display_only && self.stamp.run_state_revision == expected {
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
        let controller = store.load_controller_binding_from_manifest(&manifest)?;
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

/// Only the ledger's explicitly classified, freshly verified source motion is
/// eligible for recapture. Integrity, capacity and security errors pass through.
fn display_ledger_result(
    result: Result<ObservedLedger, LedgerError>,
    run_id: Uuid,
    head: u64,
    lifecycle: RunLifecycle,
) -> Result<Option<ObservedLedger>, MachineError> {
    match result {
        Ok(ledger) => Ok(Some(ledger)),
        Err(LedgerError::CaptureConflict) => Ok(None),
        Err(error) => Err(crate::ledger::display_observation_error(
            run_id, head, lifecycle, error,
        )),
    }
}

/// Compare every source field, including observation fields whose revision did not advance.
fn capture_consistent<S: PartialEq, O, E>(
    attempts: usize,
    mut read: impl FnMut() -> Result<S, E>,
    mut observe: impl FnMut(&S) -> Result<Option<O>, E>,
) -> Result<Option<(S, O)>, E> {
    for _ in 0..attempts {
        let before = read()?;
        let Some(observation) = observe(&before)? else {
            continue;
        };
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
        diagnostic_state_root: Option<&Path>,
        sources: &CaptureSources,
        ledger: Option<&ObservedLedger>,
        effective_policy: &crate::domain::EffectivePolicy,
    ) -> Result<Self, MachineError> {
        Self::read_with_identity(
            diagnostic_state_root,
            sources,
            ledger,
            effective_policy,
            false,
        )
    }

    fn read_display(
        sources: &CaptureSources,
        ledger: Option<&ObservedLedger>,
        effective_policy: &crate::domain::EffectivePolicy,
    ) -> Result<Self, MachineError> {
        Self::read_with_identity(None, sources, ledger, effective_policy, true)
    }

    fn read_with_identity(
        diagnostic_state_root: Option<&Path>,
        sources: &CaptureSources,
        ledger: Option<&ObservedLedger>,
        effective_policy: &crate::domain::EffectivePolicy,
        display: bool,
    ) -> Result<Self, MachineError> {
        use crate::worker::{
            ProcessIdentityVerdict, classify_dedicated_server_identity, classify_worker_identity,
        };
        let classify_worker_identity = |record: &WorkerRuntimeRecord| {
            if display {
                crate::worker::classify_worker_identity_for_display(record)
            } else {
                classify_worker_identity(record)
            }
        };
        let classify_dedicated_server_identity =
            |identity: &crate::worker::DedicatedServerIdentity| {
                if display {
                    crate::worker::classify_dedicated_server_identity_for_display(identity)
                } else {
                    classify_dedicated_server_identity(identity)
                }
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
        let background = if sources.manifest.execution_lane
            == crate::domain::ExecutionLane::SharedReadonly
        {
            BackgroundObservation::NotApplicable
        } else if sources.projection.thread_id.is_none() && server_identity.is_none() {
            BackgroundObservation::Unstarted
        } else if let Some(record) = &sources.runtime_record {
            let proof = match diagnostic_state_root {
                Some(state_root) if !display => {
                    crate::worker::prove_worker_workload_absent_with_diagnostic(state_root, record)
                }
                _ => crate::worker::prove_worker_workload_absent(record),
            };
            match proof {
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

#[cfg(test)]
fn native_snapshot_policy_category(
    sources: &CaptureSources,
    policy: Option<&crate::domain::EffectivePolicy>,
) -> &'static str {
    let Some(policy) = policy else {
        return "policy_history";
    };
    if sources
        .runtime_record
        .as_ref()
        .and_then(|r| r.app_server_epoch)
        .is_some_and(|epoch| Some(epoch) != policy.server_epoch)
    {
        "server_epoch"
    } else if policy.access == crate::domain::Access::Write {
        if !sources
            .writer
            .holder
            .as_ref()
            .is_some_and(|h| h.run_id == sources.manifest.run_id)
        {
            "writer_holder"
        } else if sources.writer.state != crate::writer::WriterAuthorityState::Active {
            "writer_state"
        } else if policy.writer_generation != Some(sources.writer.writer_generation) {
            "writer_generation"
        } else {
            "other"
        }
    } else {
        "other"
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
    let mut policy = policy_at_head(&sources.projection, ledger, server_epoch)?;
    if let Some(release) = sources
        .writer
        .pending_generation_release(sources.manifest.run_id)
    {
        release.verify_ledger_prefix(
            ledger
                .ok_or_else(|| {
                    state_conflict(
                        sources.manifest.run_id,
                        sources.projection.lifecycle,
                        "capture_snapshot",
                    )
                })?
                .records(),
        )?;
        let runtime_matches = sources.runtime_record.as_ref().is_none_or(|runtime| {
            runtime.identity.workspace_id == sources.manifest.workspace_id
                && ((runtime.identity.run_generation == release.source.run_generation
                    && runtime.app_server_epoch == Some(release.source.profile_server_epoch))
                    || release.successor.as_ref().is_some_and(|reader| {
                        reader.attempt_started
                            && runtime.identity.run_generation == reader.run_generation
                            && runtime.app_server_epoch == Some(reader.server_epoch)
                    }))
        });
        if release.source.thread_id != sources.projection.thread_id || !runtime_matches {
            return Err(state_conflict(
                sources.manifest.run_id,
                sources.projection.lifecycle,
                "capture_snapshot",
            ));
        }
        policy.access = crate::domain::Access::Unknown;
        policy.verification = crate::domain::PolicyVerification::Unverified;
        policy.writer_generation = None;
        return Ok(policy);
    }
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

pub(crate) fn policy_at_head(
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
    let inputs = ledger
        .policy_inputs()
        .map_err(|error| internal_runtime(error.to_string()))?;
    let generation = inputs
        .bindings
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
    Ok(
        policy_from_payloads(generation, inputs.policies.iter().cloned())?.unwrap_or(
            EffectivePolicy {
                access: Access::Unknown,
                verification: PolicyVerification::Unverified,
                policy_epoch: PolicyEpoch(0),
                thread_generation: Some(generation),
                server_epoch,
                writer_generation: None,
            },
        ),
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
    fn pending_release_observations_are_fenced_and_reject_changed_history() {
        use crate::semantic::successor_review_tests::Scene;
        for managed in [false, true] {
            let scene = Scene::writable(managed);
            scene.prepare_worker_ledger();
            let task = scene.accept_write_task("pending-observation");
            scene.record_running_execution(&task, "pending-observation", true);
            scene.publish("turn-1", "completed");
            let run = scene.manifest().run_id;
            let root = scene.state_root();
            let projection = RunStore::new(SystemWorkspacePlatform, root)
                .load_state_projection(run)
                .unwrap();
            let writer =
                WriterStore::new(root, &scene.view().workspace_id, DarwinSystem.current_uid());
            let transaction = Uuid::now_v7();
            writer
                .transact(|record| {
                    record.prepare_release(run, transaction)?;
                    record.prepare_generation_release(crate::writer::GenerationRelease {
                        purpose: crate::writer::GenerationRetirementPurpose::Release,
                        transaction_id: transaction,
                        source: record.holder.clone().unwrap(),
                        writer_generation: record.writer_generation,
                        authority_revision: record.authority_revision,
                        source_revision: projection.ledger_head.sequence,
                        source_ledger_sha256: crate::jcs::sha256_hex(
                            &serde_json::to_vec(&projection.ledger_head).unwrap(),
                        ),
                        source_runtime_sha256: "2".repeat(64),
                        registration_sha256: "3".repeat(64),
                        scope_sha256: None,
                        absence_sha256: None,
                        reader_proof_sha256: None,
                        retired_reader_sha256: None,
                        phase: crate::writer::GenerationReleasePhase::Prepared,
                        successor: None,
                    })
                })
                .unwrap();
            let before = std::fs::read(root.join("runtime/writer.json")).unwrap();
            for display in [false, true] {
                let snapshot = if display {
                    RunSnapshot::load_display(root, run, 0)
                } else {
                    RunSnapshot::load(root, run, 0)
                }
                .unwrap();
                assert_eq!(
                    snapshot.captured_effective_policy.access,
                    crate::domain::Access::Unknown
                );
                assert_eq!(
                    snapshot.captured_effective_policy.verification,
                    crate::domain::PolicyVerification::Unverified
                );
                assert_eq!(snapshot.writer.transaction_id, Some(transaction));
                assert!(snapshot.writer.require_no_generation_release(run).is_err());
            }
            assert_eq!(
                std::fs::read(root.join("runtime/writer.json")).unwrap(),
                before
            );
            let (snapshot, observation) = RunSnapshot::observe_display(root, run, 0).unwrap();
            let facts = crate::gateway_projection::ProjectionFacts::from_observation(
                &snapshot,
                &observation,
            )
            .unwrap();
            let machine = snapshot.run_value("Absent", serde_json::Value::Null);
            assert_eq!(machine["effective_policy"]["access"], "unknown");
            assert_eq!(machine["effective_policy"]["verification"], "unverified");
            assert_eq!(machine["recovery"]["status"], "recovery_required");
            assert_eq!(
                machine["recovery"]["reason"],
                "generation_release_prepared_scope_missing"
            );
            assert_eq!(
                machine["writer_authority"]["transaction_id"],
                transaction.to_string()
            );
            assert!(
                machine["recovery"]["unsafe_actions"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|action| action == "run.submit")
            );
            let public = crate::gateway_projection_v2::run_projection(&snapshot, &facts).unwrap();
            let recovery = public.recovery.unwrap();
            assert_eq!(
                recovery.state,
                crate::protocol::public_v2::RecoveryState::Required as i32
            );
            assert_eq!(
                recovery.blocker_code.as_deref(),
                Some("generation_release_prepared_scope_missing")
            );
            let public_writer =
                crate::gateway_projection_v2::writer_state(&snapshot, &facts, Default::default())
                    .unwrap();
            assert_eq!(
                public_writer.recovery_blocker.as_deref(),
                Some("generation_release_prepared_scope_missing")
            );
            assert!(!public_writer.handoff_eligible);
            assert_eq!(
                public_writer.policy_verification,
                crate::protocol::public_v2::PolicyVerification::Unverified as i32
            );
            let store = RunStore::new(SystemWorkspacePlatform, root);
            let mut sources = CaptureSources::read(&store, root, run).unwrap();
            let ledger =
                ObservedLedger::open_run(root, run, projection.ledger_head.sequence).unwrap();
            sources
                .writer
                .generation_release
                .as_mut()
                .unwrap()
                .source_ledger_sha256 = "f".repeat(64);
            assert!(durable_policy(&sources, Some(&ledger)).is_err());
            sources = CaptureSources::read(&store, root, run).unwrap();
            sources
                .writer
                .generation_release
                .as_mut()
                .unwrap()
                .source
                .thread_id = Some("foreign".into());
            assert!(durable_policy(&sources, Some(&ledger)).is_err());

            sources = CaptureSources::read(&store, root, run).unwrap();
            let release = sources.writer.generation_release.as_ref().unwrap();
            let source_generation = release.source.run_generation;
            let source_epoch = release.source.profile_server_epoch;
            let mut runtime = crate::semantic::prepared_release_tests::runtime();
            runtime.identity.workspace_id = sources.manifest.workspace_id.clone();
            runtime.identity.run_id = run;
            runtime.identity.run_generation = source_generation;
            runtime.app_server_epoch = Some(source_epoch);
            sources.runtime_record = Some(runtime.clone());
            assert_eq!(
                durable_policy(&sources, Some(&ledger)).unwrap().access,
                crate::domain::Access::Unknown
            );
            for field in ["workspace", "generation", "epoch"] {
                let mut foreign = runtime.clone();
                match field {
                    "workspace" => foreign.identity.workspace_id = "foreign".into(),
                    "generation" => foreign.identity.run_generation += 1,
                    _ => foreign.app_server_epoch = Some(source_epoch + 1),
                }
                sources.runtime_record = Some(foreign);
                assert!(durable_policy(&sources, Some(&ledger)).is_err(), "{field}");
            }
            // Reach a reserved Reader only through the checked in-memory
            // checkpoint transitions, so the owner has a valid phase/proof shape.
            sources = CaptureSources::read(&store, root, run).unwrap();
            let reader = crate::writer::ReaderReservation {
                run_generation: source_generation + 1,
                server_epoch: source_epoch + 1,
                control_socket_epoch: 3,
                attempt_started: false,
            };
            {
                use crate::writer::GenerationReleasePhase as P;
                for phase in [P::StopStarted, P::OldAbsent, P::ReaderStarting] {
                    let expected = sources.writer.generation_release.clone().unwrap();
                    let mut next = expected.clone();
                    next.phase = phase;
                    match phase {
                        P::StopStarted => next.scope_sha256 = Some("4".repeat(64)),
                        P::OldAbsent => next.absence_sha256 = Some("5".repeat(64)),
                        _ => next.successor = Some(reader.clone()),
                    }
                    sources
                        .writer
                        .advance_generation_release(&expected, next)
                        .unwrap();
                }
            }
            let mut successor = runtime.clone();
            successor.identity.run_generation = reader.run_generation;
            successor.app_server_epoch = Some(reader.server_epoch);
            // A reserved but not started Reader admits no locator; with the
            // old locator already retired, observation stays fenced.
            sources.runtime_record = None;
            assert_eq!(
                durable_policy(&sources, Some(&ledger)).unwrap().access,
                crate::domain::Access::Unknown
            );
            sources.runtime_record = Some(successor.clone());
            assert!(durable_policy(&sources, Some(&ledger)).is_err());
            sources
                .writer
                .generation_release
                .as_mut()
                .unwrap()
                .successor
                .as_mut()
                .unwrap()
                .attempt_started = true;
            assert_eq!(
                durable_policy(&sources, Some(&ledger)).unwrap().access,
                crate::domain::Access::Unknown
            );
            // Generation and epoch must match as one source or Reader pair.
            for (generation, epoch, workspace) in [
                (reader.run_generation, source_epoch, None),
                (source_generation, reader.server_epoch, None),
                (reader.run_generation + 1, reader.server_epoch, None),
                (reader.run_generation, reader.server_epoch + 1, None),
                (reader.run_generation, reader.server_epoch, Some("foreign")),
            ] {
                let mut foreign = successor.clone();
                foreign.identity.run_generation = generation;
                foreign.app_server_epoch = Some(epoch);
                if let Some(workspace) = workspace {
                    foreign.identity.workspace_id = workspace.into();
                }
                sources.runtime_record = Some(foreign);
                assert!(
                    durable_policy(&sources, Some(&ledger)).is_err(),
                    "{generation}/{epoch}/{workspace:?}"
                );
            }
            assert_eq!(
                std::fs::read(root.join("runtime/writer.json")).unwrap(),
                before
            );
        }
    }

    #[test]
    fn native_policy_diagnostic_uses_closed_failure_predicates() {
        let fixture = crate::semantic::successor_fixture::SuccessorMember::new();
        let mut projection = RunStateProjection::starting(fixture.member_ref);
        projection.thread_id = Some("original-thread".into());
        let sources = CaptureSources {
            manifest: fixture.manifest.clone(),
            projection,
            writer: WriterRecord::empty(&fixture.view.workspace_id),
            controller: fixture.owner_binding.clone(),
            runtime_record: None,
        };
        assert!(durable_policy(&sources, None).is_err());
        assert_eq!(
            native_snapshot_policy_category(&sources, None),
            "policy_history"
        );
        let policy = crate::domain::EffectivePolicy {
            access: crate::domain::Access::Write,
            verification: crate::domain::PolicyVerification::Verified,
            policy_epoch: crate::domain::PolicyEpoch(1),
            thread_generation: Some(1),
            server_epoch: Some(1),
            writer_generation: Some(1),
        };
        assert_eq!(
            native_snapshot_policy_category(&sources, Some(&policy)),
            "writer_holder"
        );
    }

    #[test]
    fn explicit_source_motion_recaptures_the_current_coherent_head() {
        use std::cell::RefCell;
        let run_id = Uuid::now_v7();
        let mut initial = RunStateProjection::starting(run_id);
        initial.ledger_head.sequence = 1;
        initial.ledger_head.hash = "a".repeat(64);
        let source = RefCell::new(initial);
        let reads = Cell::new(0);
        let observations = Cell::new(0);
        let captured = capture_consistent(
            CAPTURE_ATTEMPTS,
            || {
                reads.set(reads.get() + 1);
                Ok::<_, MachineError>(source.borrow().clone())
            },
            |before| {
                observations.set(observations.get() + 1);
                if observations.get() == 1 {
                    source.borrow_mut().ledger_head.sequence = 2;
                    source.borrow_mut().ledger_head.hash = "b".repeat(64);
                    return display_ledger_result(
                        Err(LedgerError::CaptureConflict),
                        run_id,
                        before.ledger_head.sequence,
                        before.lifecycle,
                    )
                    .map(|ledger| ledger.map(|_| before.ledger_head.clone()));
                }
                Ok(Some(before.ledger_head.clone()))
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(reads.get(), 3);
        assert_eq!(observations.get(), 2);
        assert_eq!(captured.0, *source.borrow());
        assert_eq!(captured.1, captured.0.ledger_head);
        assert_eq!(captured.1.sequence, 2);
    }

    #[test]
    fn continued_explicit_source_motion_exhausts_only_the_existing_capture_budget() {
        let run_id = Uuid::now_v7();
        let reads = Cell::new(0);
        let observations = Cell::new(0);
        let captured = capture_consistent(
            CAPTURE_ATTEMPTS,
            || {
                reads.set(reads.get() + 1);
                Ok::<_, MachineError>(RunStateProjection::starting(run_id))
            },
            |before| {
                observations.set(observations.get() + 1);
                display_ledger_result(
                    Err(LedgerError::CaptureConflict),
                    run_id,
                    before.ledger_head.sequence,
                    before.lifecycle,
                )
            },
        )
        .unwrap();
        assert!(captured.is_none());
        assert_eq!(reads.get(), CAPTURE_ATTEMPTS);
        assert_eq!(observations.get(), CAPTURE_ATTEMPTS);
        let conflict = state_conflict(run_id, RunLifecycle::Starting, "capture_snapshot");
        assert_eq!(conflict.code, "RUN_STATE_CONFLICT");
    }

    #[test]
    fn integrity_and_security_failures_are_not_source_motion_retries() {
        for failure in [
            LedgerError::Integrity("damaged published record".to_owned()),
            LedgerError::SecurityPolicy(MachineError::new(
                "RUNTIME_PATH_INVALID",
                "invalid private source mode",
                false,
                serde_json::json!({}),
            )),
        ] {
            let run_id = Uuid::now_v7();
            let reads = Cell::new(0);
            let observations = Cell::new(0);
            let mut failure = Some(failure);
            let error = capture_consistent(
                CAPTURE_ATTEMPTS,
                || {
                    reads.set(reads.get() + 1);
                    Ok::<_, MachineError>(RunStateProjection::starting(run_id))
                },
                |before| {
                    observations.set(observations.get() + 1);
                    display_ledger_result(
                        Err(failure.take().expect("integrity failure cannot be retried")),
                        run_id,
                        before.ledger_head.sequence,
                        before.lifecycle,
                    )
                },
            )
            .unwrap_err();
            assert_eq!(error.code, "AUDIT_INTEGRITY_FAILURE");
            assert_eq!(reads.get(), 1);
            assert_eq!(observations.get(), 1);
        }
    }

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
            |writer| {
                Ok(Some(
                    writer.holder.as_ref().unwrap().pending_interaction_count,
                ))
            },
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
            |source| Ok(Some(*source)),
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
