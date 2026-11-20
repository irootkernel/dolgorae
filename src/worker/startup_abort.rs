use super::*;
#[cfg(target_os = "macos")]
use crate::darwin::ProcessCensusEntry;
use crate::domain::ExecutionLane;
use crate::run::{AggregateMemberKind, RunManifest};

/// The owner of the initial accepted Start consumes this checkpoint under its
/// startup lock. The Worker publishes it only from the pre-dispatch timeout.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct StartupAbort {
    schema_version: u32,
    worker: WorkerIdentity,
    control_socket_epoch: u64,
    scope_sha256: String,
    live_children: Vec<RetirementProcess>,
}

pub(super) fn startup_cap(
    manifest: &RunManifest,
    session: &WorkerSessionBootstrap,
    record: &WorkerRuntimeRecord,
) -> u64 {
    if eligible(manifest, session, record) {
        60
    } else if public_caller(manifest) && session.generation_reader.is_some() {
        // A Close also retires the source and settles the successor within
        // the public response budget.
        50
    } else {
        30
    }
}

fn public_caller(manifest: &RunManifest) -> bool {
    manifest.schema_version == 4
        && manifest.execution_lane == ExecutionLane::Dedicated
        && matches!(manifest.start_operation_receipt, Some(Some(_)))
        && manifest
            .aggregate_binding
            .as_ref()
            .is_none_or(|member| member.member_kind == AggregateMemberKind::Primary)
}

fn eligible(
    manifest: &RunManifest,
    session: &WorkerSessionBootstrap,
    record: &WorkerRuntimeRecord,
) -> bool {
    public_caller(manifest)
        && session.attach == SessionAttach::Start
        && session.eager_thread_operation_id == Some(manifest.run_id)
        && session.generation_reader.is_none()
        && record.identity.run_generation == 1
}

fn path(runtime: &Path, run_id: Uuid) -> PathBuf {
    runtime
        .join("runs")
        .join(format!("{run_id}.startup-abort.json"))
}

fn scope_hash(scope: &StartupScopeWitness) -> Result<String, WorkerProtocolError> {
    let encoded =
        serde_json::to_string(scope).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    let value =
        crate::jcs::parse(&encoded).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    let canonical =
        crate::jcs::canonicalize(&value).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    Ok(crate::jcs::sha256_hex(&canonical))
}

pub(super) fn capture(
    manifest: &RunManifest,
    session: &WorkerSessionBootstrap,
    runtime_path: &Path,
) -> Result<bool, WorkerProtocolError> {
    let uid = DarwinSystem.current_uid();
    let record = read_runtime_record(runtime_path, uid)?;
    if !eligible(manifest, session, &record) {
        return Ok(false);
    }
    if record.control_recovery_required {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let scope = record
        .startup_scope
        .as_ref()
        .filter(|scope| !scope.complete)
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    let server = scope
        .dedicated_server_identity
        .as_ref()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    if classify_dedicated_server_identity(server) != ProcessIdentityVerdict::Match {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let census = DarwinSystem
        .all_process_identities()
        .map_err(|_| WorkerProtocolError::Io)?;
    let mut live_children = Vec::new();
    for child in &scope.children {
        if let Some(entry) = census.iter().find(|entry| entry.pid == child.identity.pid) {
            if !same_owned_child(entry, &child.identity) {
                return Err(WorkerProtocolError::InvalidIdentity);
            }
            if !entry.zombie {
                match retirement_process(*entry, child.ancestry.clone()) {
                    Ok(process) => live_children.push(process),
                    Err(_) if kernel_absent(entry.pid) => {}
                    Err(error) => return Err(error),
                }
            }
        } else if !kernel_absent(child.identity.pid) {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
    }
    let witness = StartupAbort {
        schema_version: 1,
        worker: record.identity,
        control_socket_epoch: record.control_socket_epoch,
        scope_sha256: scope_hash(scope)?,
        live_children,
    };
    let root = runtime_path
        .parent()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    let runtime = root
        .parent()
        .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
    let bytes = serde_json::to_vec(&witness).map_err(|_| WorkerProtocolError::Io)?;
    crate::writer::publish_immutable_release_file(root, &path(runtime, manifest.run_id), &bytes)
        .map_err(|_| WorkerProtocolError::Io)?;
    Ok(true)
}

pub(crate) fn load(
    state_root: &Path,
    run_id: Uuid,
) -> Result<Option<StartupAbort>, WorkerProtocolError> {
    let path = path(&runtime_root(state_root), run_id);
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(&path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(WorkerProtocolError::InvalidRuntimeRecord),
    };
    let metadata = file.metadata().map_err(|_| WorkerProtocolError::Io)?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != DarwinSystem.current_uid()
        || metadata.len() > 256 * 1024
    {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    let witness: StartupAbort =
        serde_json::from_reader(file).map_err(|_| WorkerProtocolError::InvalidRuntimeRecord)?;
    witness.worker.validate()?;
    if witness.schema_version != 1
        || witness.worker.run_id != run_id
        || witness.worker.run_generation != 1
        || witness.control_socket_epoch == 0
        || decode_sha256(&witness.scope_sha256).is_none()
        || witness.live_children.len() > 64
    {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    }
    Ok(Some(witness))
}

impl StartupAbort {
    pub(crate) fn has_retirement_proof(
        &self,
        state_root: &Path,
    ) -> Result<bool, WorkerProtocolError> {
        let runtime = runtime_root(state_root);
        let id = self.worker.run_id;
        match fs::symlink_metadata(runtime_record_path(&runtime, id)?) {
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(WorkerProtocolError::Io),
        }
        Ok(
            verified_absence_marker(&runtime, id)?.is_some_and(|marker| {
                marker.run_generation == self.worker.run_generation
                    && worker_identity_digest(&self.worker).as_ref()
                        == Some(&marker.worker_identity_sha256)
            }),
        )
    }

    #[cfg(target_os = "macos")]
    pub(crate) fn retire(
        &self,
        state_root: &Path,
        manifest: &RunManifest,
    ) -> Result<crate::conformance::StartFailureAuthority, WorkerProtocolError> {
        let uid = DarwinSystem.current_uid();
        let runtime = runtime_root(state_root);
        let id = self.worker.run_id;
        let path = runtime_record_path(&runtime, id)?;
        if manifest.run_id != id || manifest.workspace_id != self.worker.workspace_id {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        if self.has_retirement_proof(state_root)? {
            return Ok(crate::conformance::StartFailureAuthority::from_retired_startup_abort(id));
        }
        let record = read_runtime_record(&path, uid)?;
        let bootstrap = read_worker_bootstrap(&bootstrap_path(&runtime, id), uid)?;
        let session = bootstrap
            .session
            .as_ref()
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
        let scope = record
            .startup_scope
            .as_ref()
            .filter(|scope| !scope.complete)
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
        if bootstrap.run_id != id
            || bootstrap.run_generation != 1
            || bootstrap.control_socket_epoch != record.control_socket_epoch
            || record.control_socket_epoch != self.control_socket_epoch
            || record.identity != self.worker
            || !eligible(manifest, session, &record)
            || self.scope_sha256 != scope_hash(scope)?
        {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }
        let mut seen = BTreeSet::new();
        if self.live_children.iter().any(|child| {
            !seen.insert(child.identity.pid)
                || !child.executable.is_absolute()
                || child.device == 0
                || child.inode == 0
                || decode_sha256(&child.sha256).is_none()
                || !scope.children.iter().any(|retained| {
                    same_owned_child(&retained.identity, &child.identity)
                        && retained.ancestry == child.ancestry
                })
        }) {
            return Err(WorkerProtocolError::InvalidRuntimeRecord);
        }

        let server = scope
            .dedicated_server_identity
            .as_ref()
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
        let socket = scope
            .app_server_socket_identity
            .as_ref()
            .ok_or(WorkerProtocolError::InvalidRuntimeRecord)?;
        let socket_path = dedicated_server_socket_path(uid, id, 1)?;
        crate::codex_socket::verify_recorded_or_absent(
            &socket_path,
            uid,
            socket.device,
            socket.inode,
        )
        .map_err(|_| WorkerProtocolError::SocketIdentityMismatch)?;
        let log = state_root
            .join("runs")
            .join(id.to_string())
            .join("dedicated-server-1.log");
        self.stop_scope(&record, scope, server, &log)?;
        crate::codex_socket::remove_recorded(&socket_path, uid, socket.device, socket.inode)
            .map_err(|_| WorkerProtocolError::SocketIdentityMismatch)?;
        remove_proven_absent_runtime(&path, &record)?;
        Ok(crate::conformance::StartFailureAuthority::from_retired_startup_abort(id))
    }

    #[cfg(target_os = "macos")]
    fn stop_scope(
        &self,
        record: &WorkerRuntimeRecord,
        scope: &StartupScopeWitness,
        server: &DedicatedServerIdentity,
        log: &Path,
    ) -> Result<(), WorkerProtocolError> {
        let started = Instant::now();
        let groups = scope
            .children
            .iter()
            .map(|child| child.identity.process_group_id)
            .chain([record.identity.process_group_id, server.process_group_id])
            .collect::<BTreeSet<_>>();
        let mut empty = 0;
        let mut last = Value::Null;
        let mut last_group_difference = Value::Null;
        while started.elapsed() < Duration::from_secs(10) {
            match classify_worker_identity(record) {
                ProcessIdentityVerdict::Absent => {}
                ProcessIdentityVerdict::Match => {
                    thread::sleep(Duration::from_millis(100));
                    continue;
                }
                observed => {
                    append_private_diagnostic(
                        log,
                        &serde_json::json!({
                            "phase":"startup_abort_worker","verdict":observed.as_str(),
                        }),
                    );
                    return Err(WorkerProtocolError::InvalidIdentity);
                }
            }
            let census = DarwinSystem
                .all_process_identities()
                .map_err(|_| WorkerProtocolError::Io)?;
            let parents = census
                .iter()
                .map(|entry| (entry.pid, entry.parent_pid))
                .collect();
            let mut consistent = true;
            for group in &groups {
                let mut enumerated = census
                    .iter()
                    .filter(|entry| entry.process_group_id == *group)
                    .map(|entry| entry.pid)
                    .collect::<Vec<_>>();
                enumerated.sort_unstable();
                let members = DarwinSystem
                    .process_group_pids(*group)
                    .map_err(|_| WorkerProtocolError::Io)?;
                if members != enumerated {
                    consistent = false;
                    last_group_difference = serde_json::json!({
                        "group":group,
                        "group_only": members.iter().filter(|pid| !enumerated.contains(pid)).take(8)
                            .map(|pid| serde_json::json!({
                                "pid":pid,"zombie":DarwinSystem.bsd_process_identity(*pid)
                                    .ok().map(|identity| identity.zombie),
                            })).collect::<Vec<_>>(),
                        "census_only": census.iter().filter(|entry| entry.process_group_id == *group
                            && !members.contains(&entry.pid)).take(8)
                            .map(|entry| serde_json::json!({"pid":entry.pid,"zombie":entry.zombie}))
                            .collect::<Vec<_>>(),
                    });
                }
            }
            let live = census
                .iter()
                .filter(|entry| {
                    entry.pid == server.pid
                        || entry.pid == record.identity.pid
                        || process_is_in_scope(
                            entry,
                            server.pid,
                            server.process_group_id,
                            server.session_id,
                            &parents,
                        )
                        || process_is_in_scope(
                            entry,
                            record.identity.pid,
                            record.identity.process_group_id,
                            record.identity.session_id,
                            &parents,
                        )
                        || scope.children.iter().any(|child| {
                            let child = &child.identity;
                            entry.pid == child.pid
                                || process_is_in_scope(
                                    entry,
                                    child.pid,
                                    child.process_group_id,
                                    child.session_id,
                                    &parents,
                                )
                        })
                })
                .collect::<Vec<_>>();
            let signal = if started.elapsed() < Duration::from_secs(5) {
                libc::SIGTERM
            } else {
                libc::SIGKILL
            };
            last = serde_json::json!({"consistent":consistent,"remaining_processes":live.len(),"empty_samples":empty});
            if consistent
                && live.is_empty()
                && kernel_absent(server.pid)
                && scope
                    .children
                    .iter()
                    .all(|child| kernel_absent(child.identity.pid))
            {
                empty += 1;
                if empty == 5 {
                    return Ok(());
                }
            } else {
                empty = 0;
                // Census disagreement blocks absence, but cannot change the
                // ownership of an individually revalidated PID.
                if let Some(entry) = live.iter().find(|entry| entry.pid == server.pid) {
                    if !server_census_matches(entry, server) {
                        return Err(WorkerProtocolError::InvalidIdentity);
                    }
                    match classify_dedicated_server_identity(server) {
                        ProcessIdentityVerdict::Match if !entry.zombie => {
                            signal_live(entry, signal)?
                        }
                        ProcessIdentityVerdict::Absent => {}
                        observed => {
                            append_private_diagnostic(
                                log,
                                &serde_json::json!({
                                    "phase":"startup_abort_leader","verdict":observed.as_str(),
                                }),
                            );
                            return Err(WorkerProtocolError::InvalidIdentity);
                        }
                    }
                }
                for child in &self.live_children {
                    if let Some(entry) = live.iter().find(|entry| entry.pid == child.identity.pid) {
                        let mut details = Value::Null;
                        let sample = GenerationScope::verify_process_sample(
                            child, entry, true, &mut Some(&mut details)
                        ).inspect_err(|error| {
                            append_private_diagnostic(log, &serde_json::json!({
                                "phase":"startup_abort_child","error":format!("{error:?}"),"details":details,
                            }));
                        })?;
                        if !sample.is_dirty() && !entry.zombie {
                            signal_live(entry, signal)?;
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(100));
        }
        append_private_diagnostic(
            log,
            &serde_json::json!({
                "phase":"startup_abort_scope_timeout","observation":last,
                "last_group_difference":last_group_difference,
            }),
        );
        Err(WorkerProtocolError::InvalidIdentity)
    }
}

fn kernel_absent(pid: u32) -> bool {
    DarwinSystem
        .bsd_process_identity(pid)
        .is_err_and(|error| error.raw_os_error() == Some(libc::ESRCH))
}

#[cfg(target_os = "macos")]
fn signal_live(entry: &ProcessCensusEntry, signal: libc::c_int) -> Result<(), WorkerProtocolError> {
    match DarwinSystem.signal_exact_process(entry, signal) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Err(_) => Err(WorkerProtocolError::InvalidIdentity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_abort_reentry_requires_the_exact_retired_generation_and_missing_locator() {
        let run_id = Uuid::now_v7();
        let state = std::env::temp_dir().join(format!("dolgorae-abort-proof-{run_id}"));
        let runtime = runtime_root(&state);
        for directory in [
            &state,
            &runtime,
            &runtime.join("runs"),
            &runtime.join("verified-absence"),
        ] {
            fs::DirBuilder::new().mode(0o700).create(directory).unwrap();
        }
        let witness = StartupAbort {
            schema_version: 1,
            worker: WorkerIdentity {
                workspace_id: "a".repeat(64),
                run_id,
                run_generation: 1,
                boot_uuid: Uuid::now_v7(),
                pid: 42,
                process_group_id: 42,
                session_id: 42,
                uid: DarwinSystem.current_uid(),
                start_tvsec: 1,
                start_tvusec: 0,
                executable_path: PathBuf::from("/usr/bin/true"),
                executable_device: 1,
                executable_inode: 1,
                executable_sha256: "b".repeat(64),
            },
            control_socket_epoch: 2,
            scope_sha256: "c".repeat(64),
            live_children: Vec::new(),
        };
        assert!(!witness.has_retirement_proof(&state).unwrap());

        let path = verified_absence_marker_path(&runtime, run_id);
        let mut marker = VerifiedAbsenceMarker {
            schema_version: 1,
            run_id,
            run_generation: 1,
            worker_identity_sha256: "d".repeat(64),
        };
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
            .unwrap();
        serde_json::to_writer(&mut file, &marker).unwrap();
        file.sync_all().unwrap();
        drop(file);
        assert!(!witness.has_retirement_proof(&state).unwrap());
        marker.worker_identity_sha256 = worker_identity_digest(&witness.worker).unwrap();
        fs::write(&path, serde_json::to_vec(&marker).unwrap()).unwrap();
        assert!(witness.has_retirement_proof(&state).unwrap());

        let locator = runtime_record_path(&runtime, run_id).unwrap();
        std::os::unix::fs::symlink("missing", &locator).unwrap();
        assert!(!witness.has_retirement_proof(&state).unwrap());
        fs::remove_file(locator).unwrap();
        assert!(witness.has_retirement_proof(&state).unwrap());
        fs::remove_dir_all(state).unwrap();
    }
}
