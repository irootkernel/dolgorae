use super::*;
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

const MAX_LIVE_CHILDREN: usize = 64;

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
                if kernel_identity_absent(&child.identity) {
                    continue;
                }
                return Err(WorkerProtocolError::InvalidIdentity);
            }
            if !entry.zombie {
                match retirement_process(*entry, child.ancestry.clone()) {
                    Ok(process) => live_children.push(process),
                    Err(_) if kernel_identity_absent(&child.identity) => {}
                    Err(error) => return Err(error),
                }
            }
        } else if !kernel_identity_absent(&child.identity) {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
    }
    if live_children.len() > MAX_LIVE_CHILDREN {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
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
        || witness.live_children.len() > MAX_LIVE_CHILDREN
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
        let worker_entry = ProcessCensusEntry {
            pid: record.identity.pid,
            parent_pid: 0,
            uid: record.identity.uid,
            process_group_id: record.identity.process_group_id,
            session_id: record.identity.session_id,
            birth: Some((record.identity.start_tvsec, record.identity.start_tvusec)),
            zombie: false,
        };
        let server_entry = ProcessCensusEntry {
            pid: server.pid,
            parent_pid: 0,
            uid: server.uid,
            process_group_id: server.process_group_id,
            session_id: server.session_id,
            birth: Some((server.start_tvsec, server.start_tvusec)),
            zombie: false,
        };
        let mut empty = 0;
        let mut last = Value::Null;
        let mut last_group_difference = Value::Null;
        let mut discovered_children: BTreeMap<(u32, (u64, u64)), ProcessCensusEntry> =
            BTreeMap::new();
        while started.elapsed() < Duration::from_secs(10) {
            let signal = if started.elapsed() < Duration::from_secs(5) {
                libc::SIGTERM
            } else {
                libc::SIGKILL
            };
            match classify_worker_identity(record) {
                ProcessIdentityVerdict::Absent => {}
                ProcessIdentityVerdict::Match => {
                    empty = 0;
                    signal_live(&worker_entry, signal)?;
                    let _ = observe_owned_worker_child(&record.identity);
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
            let server_live = match classify_dedicated_server_identity(server) {
                ProcessIdentityVerdict::Match => true,
                ProcessIdentityVerdict::Absent => false,
                observed => {
                    append_private_diagnostic(
                        log,
                        &serde_json::json!({
                            "phase":"startup_abort_leader","verdict":observed.as_str(),
                        }),
                    );
                    return Err(WorkerProtocolError::InvalidIdentity);
                }
            };
            let census = DarwinSystem
                .all_process_identities()
                .map_err(|_| WorkerProtocolError::Io)?;
            let parents = census
                .iter()
                .map(|entry| (entry.pid, entry.parent_pid))
                .collect::<BTreeMap<_, _>>();
            let plan = plan_stop_scope(
                &census,
                &parents,
                server,
                server_live,
                &scope.children,
                &discovered_children,
            )
            .inspect_err(|error| {
                append_private_diagnostic(
                    log,
                    &serde_json::json!({
                        "phase":"startup_abort_scope_plan","error":format!("{error:?}"),
                    }),
                );
            })?;
            // Record verified live descendants discovered under a currently live
            // leader so they stay exact-(pid, uid, birth)-targeted even if the
            // leader exits before they do.
            for entry in &plan.live_processes {
                if entry.pid != server.pid
                    && entry.pid != record.identity.pid
                    && !scope
                        .children
                        .iter()
                        .any(|child| matches_process_birth_identity(entry, &child.identity))
                    && let Some(birth) = entry.birth
                {
                    discovered_children
                        .entry((entry.pid, birth))
                        .or_insert(**entry);
                }
            }
            let absence_groups = absence_check_groups(
                &worker_entry,
                &server_entry,
                &scope.children,
                &discovered_children,
                &census,
            );
            let checked_groups = plan
                .live_owned_groups
                .union(&absence_groups)
                .copied()
                .collect::<BTreeSet<_>>();
            let mut consistent = true;
            let mut absence_groups_empty = true;
            for group in &checked_groups {
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
                if absence_groups.contains(group) && !members.is_empty() {
                    absence_groups_empty = false;
                }
            }
            let absence_verified = scope_absence_verified(
                &worker_entry,
                &server_entry,
                server_live,
                &scope.children,
                &discovered_children,
                &census,
                &parents,
            );
            last = serde_json::json!({
                "consistent": consistent,
                "absence_groups_empty": absence_groups_empty,
                "absence_verified": absence_verified,
                "server_live": server_live,
                "remaining_processes": plan.live_processes.len(),
                "discovered_children": discovered_children.len(),
                "empty_samples": empty,
            });
            if consistent
                && absence_groups_empty
                && absence_verified
                && !server_live
                && plan.live_processes.is_empty()
                && kernel_identity_absent(&worker_entry)
                && kernel_identity_absent(&server_entry)
                && scope
                    .children
                    .iter()
                    .all(|child| kernel_identity_absent(&child.identity))
                && discovered_children.values().all(kernel_identity_absent)
            {
                empty += 1;
                if empty == 5 {
                    return Ok(());
                }
            } else {
                empty = 0;
                // Census disagreement blocks absence, but cannot change the
                // ownership of an individually revalidated PID.
                let mut signalled = BTreeSet::new();
                for child in &self.live_children {
                    if let Some(entry) = plan
                        .live_processes
                        .iter()
                        .copied()
                        .find(|entry| matches_process_birth_identity(entry, &child.identity))
                    {
                        let mut details = Value::Null;
                        let sample = GenerationScope::verify_process_sample(
                            child,
                            entry,
                            true,
                            &mut Some(&mut details),
                        )
                        .inspect_err(|error| {
                            append_private_diagnostic(
                                log,
                                &serde_json::json!({
                                    "phase":"startup_abort_child","error":format!("{error:?}"),"details":details,
                                }),
                            );
                        })?;
                        signalled.insert(entry.pid);
                        if !sample.is_dirty() && !entry.zombie {
                            signal_live(entry, signal)?;
                        }
                    }
                }
                for entry in &plan.live_processes {
                    if matches_process_birth_identity(entry, &server_entry)
                        || signalled.contains(&entry.pid)
                    {
                        continue;
                    }
                    if !entry.zombie {
                        signal_live(entry, signal)?;
                    }
                }
                if consistent {
                    for &pgid in &plan.live_owned_groups {
                        let group_safe = census
                            .iter()
                            .filter(|entry| entry.process_group_id == pgid)
                            .all(|entry| entry.uid == server.uid && entry.birth.is_some());
                        if group_safe {
                            let _ = DarwinSystem.signal_process_group(pgid, signal);
                        }
                    }
                }
                if server_live
                    && let Some(entry) = plan
                        .live_processes
                        .iter()
                        .copied()
                        .find(|entry| matches_process_birth_identity(entry, &server_entry))
                    && !entry.zombie
                {
                    match classify_dedicated_server_identity(server) {
                        ProcessIdentityVerdict::Match => signal_live(entry, signal)?,
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

struct StopScopePlan<'a> {
    live_processes: Vec<&'a ProcessCensusEntry>,
    live_owned_groups: BTreeSet<u32>,
}

fn matches_process_birth_identity(
    entry: &ProcessCensusEntry,
    expected: &ProcessCensusEntry,
) -> bool {
    entry.pid == expected.pid
        && entry.uid == expected.uid
        && expected.birth.is_some()
        && entry.birth == expected.birth
}

fn pid_proven_recycled_in_census(
    census: &[ProcessCensusEntry],
    expected: &ProcessCensusEntry,
) -> bool {
    expected.birth.is_some()
        && census.iter().any(|entry| {
            entry.pid == expected.pid && entry.birth.is_some() && entry.birth != expected.birth
        })
}

fn recycled_scope_leaders(
    worker: &ProcessCensusEntry,
    server: &ProcessCensusEntry,
    children: &[StartupOwnedChild],
    discovered: &BTreeMap<(u32, (u64, u64)), ProcessCensusEntry>,
    census: &[ProcessCensusEntry],
) -> BTreeSet<u32> {
    [worker, server]
        .into_iter()
        .chain(children.iter().map(|child| &child.identity))
        .chain(discovered.values())
        .filter(|expected| pid_proven_recycled_in_census(census, expected))
        .map(|expected| expected.pid)
        .collect()
}

fn absence_check_groups(
    worker: &ProcessCensusEntry,
    server: &ProcessCensusEntry,
    children: &[StartupOwnedChild],
    discovered: &BTreeMap<(u32, (u64, u64)), ProcessCensusEntry>,
    census: &[ProcessCensusEntry],
) -> BTreeSet<u32> {
    let recycled = recycled_scope_leaders(worker, server, children, discovered, census);
    [worker, server]
        .into_iter()
        .chain(children.iter().map(|child| &child.identity))
        .chain(discovered.values())
        .map(|expected| expected.process_group_id)
        .filter(|pgid| !recycled.contains(pgid))
        .collect()
}

/// Fail-closed absence proof decoupled from signal targeting: any unrecorded
/// survivor in `worker`, `server`, `scope.children`, or `discovered_children`
/// scopes blocks absence (skipping a PGID/SID only when its exact leader PID
/// is proven recycled in `census` with a distinct non-null birth).
fn scope_absence_verified(
    worker: &ProcessCensusEntry,
    server: &ProcessCensusEntry,
    server_live: bool,
    children: &[StartupOwnedChild],
    discovered: &BTreeMap<(u32, (u64, u64)), ProcessCensusEntry>,
    census: &[ProcessCensusEntry],
    parents: &BTreeMap<u32, u32>,
) -> bool {
    if server_live {
        return false;
    }
    let recycled = recycled_scope_leaders(worker, server, children, discovered, census);
    let recorded = [worker, server]
        .into_iter()
        .chain(children.iter().map(|child| &child.identity))
        .chain(discovered.values())
        .collect::<Vec<_>>();
    !census.iter().any(|entry| {
        recorded.iter().any(|expected| {
            matches_process_birth_identity(entry, expected)
                || (!recycled.contains(&expected.pid)
                    && (entry.pid == expected.pid
                        || process_descends_from(entry.pid, expected.pid, parents)))
                || (!recycled.contains(&expected.process_group_id)
                    && entry.process_group_id == expected.process_group_id)
                || (!recycled.contains(&expected.session_id)
                    && entry.session_id == expected.session_id)
        })
    })
}

/// Only leaders verified live with their exact recorded identity authorize
/// process-group or session ownership. Absent or recycled leaders never admit
/// arbitrary same-PGID/same-SID bystanders for signaling.
fn plan_stop_scope<'a>(
    census: &'a [ProcessCensusEntry],
    parents: &BTreeMap<u32, u32>,
    server: &DedicatedServerIdentity,
    server_live: bool,
    children: &[StartupOwnedChild],
    discovered: &BTreeMap<(u32, (u64, u64)), ProcessCensusEntry>,
) -> Result<StopScopePlan<'a>, WorkerProtocolError> {
    let mut live_root_pids = BTreeSet::new();
    let mut live_owned_groups = BTreeSet::new();
    let mut live_owned_sessions = BTreeSet::new();
    if server_live && let Some(entry) = census.iter().find(|entry| entry.pid == server.pid) {
        if !server_census_matches(entry, server) {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        live_root_pids.insert(server.pid);
        live_owned_groups.insert(server.process_group_id);
        live_owned_sessions.insert(server.session_id);
    }
    for expected in children
        .iter()
        .map(|child| &child.identity)
        .chain(discovered.values())
    {
        if let Some(entry) = census
            .iter()
            .find(|entry| matches_process_birth_identity(entry, expected))
        {
            if !same_owned_child(entry, expected) {
                return Err(WorkerProtocolError::InvalidIdentity);
            }
            live_root_pids.insert(entry.pid);
            if expected.pid == expected.process_group_id {
                live_owned_groups.insert(expected.process_group_id);
            }
            if expected.pid == expected.session_id {
                live_owned_sessions.insert(expected.session_id);
            }
        }
    }
    let mut live_processes = Vec::new();
    for entry in census {
        let in_scope = live_root_pids.contains(&entry.pid)
            || live_owned_groups.contains(&entry.process_group_id)
            || live_owned_sessions.contains(&entry.session_id)
            || descends_from_any(entry.pid, &live_root_pids, parents);
        if in_scope {
            if entry.uid != server.uid || entry.birth.is_none() {
                return Err(WorkerProtocolError::InvalidIdentity);
            }
            live_processes.push(entry);
        }
    }
    Ok(StopScopePlan {
        live_processes,
        live_owned_groups,
    })
}

fn kernel_identity_absent(expected: &ProcessCensusEntry) -> bool {
    classify_tracked_child_identity(expected, &DarwinSystem.bsd_process_identity(expected.pid))
        == TrackedChildStatus::Absent
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

    #[test]
    fn startup_abort_scope_filters_recycled_pids_by_birth_and_enforces_live_child_cap() {
        let uid = DarwinSystem.current_uid();
        let server_identity = DedicatedServerIdentity {
            pid: 200,
            uid,
            process_group_id: 200,
            session_id: 200,
            start_tvsec: 10,
            start_tvusec: 2,
            executable_path: PathBuf::from("/usr/bin/true"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "b".repeat(64),
        };
        let server = ProcessCensusEntry {
            pid: 200,
            parent_pid: 100,
            uid,
            process_group_id: 200,
            session_id: 200,
            birth: Some((10, 2)),
            zombie: false,
        };
        let group_leader_child = ProcessCensusEntry {
            pid: 300,
            parent_pid: 200,
            uid,
            process_group_id: 300,
            session_id: 200,
            birth: Some((10, 3)),
            zombie: false,
        };
        let sibling_in_child_group = ProcessCensusEntry {
            pid: 301,
            parent_pid: 300,
            uid,
            process_group_id: 300,
            session_id: 200,
            birth: Some((10, 4)),
            zombie: false,
        };
        let child_in_server_group = ProcessCensusEntry {
            pid: 302,
            parent_pid: 200,
            uid,
            process_group_id: 200,
            session_id: 200,
            birth: Some((10, 5)),
            zombie: false,
        };
        let children = vec![
            StartupOwnedChild {
                identity: group_leader_child,
                ancestry: vec![group_leader_child, server],
            },
            StartupOwnedChild {
                identity: sibling_in_child_group,
                ancestry: vec![sibling_in_child_group, group_leader_child, server],
            },
            StartupOwnedChild {
                identity: child_in_server_group,
                ancestry: vec![child_in_server_group, server],
            },
        ];

        let parents_for = |census: &[ProcessCensusEntry]| {
            census
                .iter()
                .map(|entry| (entry.pid, entry.parent_pid))
                .collect::<BTreeMap<_, _>>()
        };

        let empty_discovered = BTreeMap::new();

        // Trigger 1: server exited and unrelated same-UID process U recycled PID 200
        // with pgid=200, sid=200 while a recorded child shared sid/pgid 200 -> U MUST NOT be targeted!
        let recycled_server_u = ProcessCensusEntry {
            pid: 200,
            parent_pid: 1,
            uid,
            process_group_id: 200,
            session_id: 200,
            birth: Some((99, 1)),
            zombie: false,
        };
        let census = vec![recycled_server_u];
        let plan = plan_stop_scope(
            &census,
            &parents_for(&census),
            &server_identity,
            false,
            &children,
            &empty_discovered,
        )
        .unwrap();
        assert!(plan.live_processes.is_empty());
        assert!(plan.live_owned_groups.is_empty());

        // Trigger 2: child group-leader C1 (pid 300, pgid 300) exited and unrelated same-UID U
        // recycled PID 300 (pgid 300) while recorded sibling C2 shared pgid 300 -> U MUST NOT be targeted!
        let recycled_child_leader_u = ProcessCensusEntry {
            pid: 300,
            parent_pid: 1,
            uid,
            process_group_id: 300,
            session_id: 300,
            birth: Some((99, 2)),
            zombie: false,
        };
        let recycled_child_u2 = ProcessCensusEntry {
            pid: 400,
            parent_pid: 300,
            uid,
            process_group_id: 300,
            session_id: 300,
            birth: Some((99, 3)),
            zombie: false,
        };
        let census = vec![
            recycled_child_leader_u,
            recycled_child_u2,
            child_in_server_group,
        ];
        let plan = plan_stop_scope(
            &census,
            &parents_for(&census),
            &server_identity,
            false,
            &children,
            &empty_discovered,
        )
        .unwrap();
        assert_eq!(plan.live_processes.len(), 1);
        assert_eq!(plan.live_processes[0].pid, child_in_server_group.pid);
        assert!(plan.live_owned_groups.is_empty());

        // Trigger 3: recycled leader U spawned U2 (pid 400, pgid 300, sid 300) and exited!
        // U is absent from census; U2 MUST NOT be targeted for signaling!
        let orphan_u2 = ProcessCensusEntry {
            pid: 400,
            parent_pid: 1,
            uid,
            process_group_id: 300,
            session_id: 300,
            birth: Some((99, 3)),
            zombie: false,
        };
        let census = vec![orphan_u2];
        let plan = plan_stop_scope(
            &census,
            &parents_for(&census),
            &server_identity,
            false,
            &children,
            &empty_discovered,
        )
        .unwrap();
        assert!(plan.live_processes.is_empty());
        assert!(plan.live_owned_groups.is_empty());

        // Positive: when server IS verified live, server + descendants in its pgid/sid ARE targeted,
        // and when group-leader child C1 IS verified live with exact (pid, uid, birth), its group IS targeted.
        let new_descendant = ProcessCensusEntry {
            pid: 350,
            parent_pid: 200,
            uid,
            process_group_id: 350,
            session_id: 200,
            birth: Some((10, 9)),
            zombie: false,
        };
        let census = vec![server, group_leader_child, new_descendant];
        let plan = plan_stop_scope(
            &census,
            &parents_for(&census),
            &server_identity,
            true,
            &children,
            &empty_discovered,
        )
        .unwrap();
        assert_eq!(plan.live_processes.len(), 3);
        assert!(plan.live_owned_groups.contains(&200));
        assert!(plan.live_owned_groups.contains(&300));

        // Same (pid, uid, birth) with drifted pgid must fail closed!
        let drifted_live = ProcessCensusEntry {
            process_group_id: 999,
            ..child_in_server_group
        };
        let census = vec![drifted_live];
        assert_eq!(
            plan_stop_scope(
                &census,
                &parents_for(&census),
                &server_identity,
                false,
                &children,
                &empty_discovered,
            )
            .err(),
            Some(WorkerProtocolError::InvalidIdentity)
        );

        // Verify cap constant matches load().
        assert_eq!(MAX_LIVE_CHILDREN, 64);
    }

    #[test]
    fn startup_abort_decouples_fail_closed_absence_and_persists_discovered_descendants() {
        let uid = DarwinSystem.current_uid();
        let worker_entry = ProcessCensusEntry {
            pid: 100,
            parent_pid: 1,
            uid,
            process_group_id: 100,
            session_id: 100,
            birth: Some((10, 1)),
            zombie: false,
        };
        let server_identity = DedicatedServerIdentity {
            pid: 200,
            uid,
            process_group_id: 200,
            session_id: 200,
            start_tvsec: 10,
            start_tvusec: 2,
            executable_path: PathBuf::from("/usr/bin/true"),
            executable_device: 1,
            executable_inode: 1,
            executable_sha256: "b".repeat(64),
        };
        let server_entry = ProcessCensusEntry {
            pid: 200,
            parent_pid: 100,
            uid,
            process_group_id: 200,
            session_id: 200,
            birth: Some((10, 2)),
            zombie: false,
        };
        let group_leader_child = ProcessCensusEntry {
            pid: 300,
            parent_pid: 200,
            uid,
            process_group_id: 300,
            session_id: 200,
            birth: Some((10, 3)),
            zombie: false,
        };
        let sibling_in_child_group = ProcessCensusEntry {
            pid: 301,
            parent_pid: 300,
            uid,
            process_group_id: 300,
            session_id: 200,
            birth: Some((10, 4)),
            zombie: false,
        };
        let children = vec![
            StartupOwnedChild {
                identity: group_leader_child,
                ancestry: vec![group_leader_child, server_entry],
            },
            StartupOwnedChild {
                identity: sibling_in_child_group,
                ancestry: vec![sibling_in_child_group, group_leader_child, server_entry],
            },
        ];
        let parents_for = |census: &[ProcessCensusEntry]| {
            census
                .iter()
                .map(|entry| (entry.pid, entry.parent_pid))
                .collect::<BTreeMap<_, _>>()
        };
        let empty_discovered = BTreeMap::new();

        // Trigger A: server absent (not recycled), unrecorded survivor G in server.pgid
        // or server.sid -> NOT targeted by plan_stop_scope, but BLOCKS absence proof!
        for survivor in [
            ProcessCensusEntry {
                pid: 450,
                parent_pid: 1,
                uid,
                process_group_id: 200,
                session_id: 900,
                birth: Some((10, 20)),
                zombie: false,
            },
            ProcessCensusEntry {
                pid: 451,
                parent_pid: 1,
                uid,
                process_group_id: 451,
                session_id: 200,
                birth: Some((10, 21)),
                zombie: false,
            },
        ] {
            let census = vec![survivor];
            let parents = parents_for(&census);
            let plan = plan_stop_scope(
                &census,
                &parents,
                &server_identity,
                false,
                &children,
                &empty_discovered,
            )
            .unwrap();
            assert!(plan.live_processes.is_empty());
            assert!(plan.live_owned_groups.is_empty());
            assert!(
                absence_check_groups(
                    &worker_entry,
                    &server_entry,
                    &children,
                    &empty_discovered,
                    &census,
                )
                .contains(&200)
            );
            assert!(!scope_absence_verified(
                &worker_entry,
                &server_entry,
                false,
                &children,
                &empty_discovered,
                &census,
                &parents,
            ));
        }

        // Trigger B: worker absent (not recycled), unrecorded survivor in worker.pgid
        // or worker.sid -> NOT targeted by plan_stop_scope, but BLOCKS absence proof!
        for survivor in [
            ProcessCensusEntry {
                pid: 460,
                parent_pid: 1,
                uid,
                process_group_id: 100,
                session_id: 900,
                birth: Some((10, 22)),
                zombie: false,
            },
            ProcessCensusEntry {
                pid: 461,
                parent_pid: 1,
                uid,
                process_group_id: 461,
                session_id: 100,
                birth: Some((10, 23)),
                zombie: false,
            },
        ] {
            let census = vec![survivor];
            let parents = parents_for(&census);
            let plan = plan_stop_scope(
                &census,
                &parents,
                &server_identity,
                false,
                &children,
                &empty_discovered,
            )
            .unwrap();
            assert!(plan.live_processes.is_empty());
            assert!(
                absence_check_groups(
                    &worker_entry,
                    &server_entry,
                    &children,
                    &empty_discovered,
                    &census,
                )
                .contains(&100)
            );
            assert!(!scope_absence_verified(
                &worker_entry,
                &server_entry,
                false,
                &children,
                &empty_discovered,
                &census,
                &parents,
            ));
        }

        // Trigger C: recorded child group-leader C1 (pid 300, pgid 300) absent (not recycled),
        // unrecorded survivor in C1.pgid -> NOT targeted, but BLOCKS absence proof!
        let survivor_c1 = ProcessCensusEntry {
            pid: 470,
            parent_pid: 1,
            uid,
            process_group_id: 300,
            session_id: 900,
            birth: Some((10, 24)),
            zombie: false,
        };
        let census = vec![survivor_c1];
        let parents = parents_for(&census);
        let plan = plan_stop_scope(
            &census,
            &parents,
            &server_identity,
            false,
            &children,
            &empty_discovered,
        )
        .unwrap();
        assert!(plan.live_processes.is_empty());
        assert!(
            absence_check_groups(
                &worker_entry,
                &server_entry,
                &children,
                &empty_discovered,
                &census,
            )
            .contains(&300)
        );
        assert!(!scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &empty_discovered,
            &census,
            &parents,
        ));

        // Trigger D: unrecorded descendants G (in server.pgid) and G_leader (own pgid)
        // discovered while server is live remain exact-(pid, uid, birth) targets after
        // server exits and reparents them to PID 1, without signaling dead server.pgid.
        let discovered_g = ProcessCensusEntry {
            pid: 350,
            parent_pid: 200,
            uid,
            process_group_id: 200,
            session_id: 200,
            birth: Some((10, 9)),
            zombie: false,
        };
        let discovered_g_leader = ProcessCensusEntry {
            pid: 360,
            parent_pid: 200,
            uid,
            process_group_id: 360,
            session_id: 200,
            birth: Some((10, 10)),
            zombie: false,
        };
        let census_live = vec![server_entry, discovered_g, discovered_g_leader];
        let parents_live = parents_for(&census_live);
        let plan_live = plan_stop_scope(
            &census_live,
            &parents_live,
            &server_identity,
            true,
            &children,
            &empty_discovered,
        )
        .unwrap();
        let mut discovered = BTreeMap::new();
        for entry in &plan_live.live_processes {
            if entry.pid != server_identity.pid
                && entry.pid != worker_entry.pid
                && let Some(birth) = entry.birth
            {
                discovered.entry((entry.pid, birth)).or_insert(**entry);
            }
        }
        assert_eq!(discovered.len(), 2);

        // Server exits; G and G_leader are reparented to PID 1, and G_leader spawns G_sub.
        let reparented_g = ProcessCensusEntry {
            parent_pid: 1,
            ..discovered_g
        };
        let reparented_g_leader = ProcessCensusEntry {
            parent_pid: 1,
            ..discovered_g_leader
        };
        let g_sub = ProcessCensusEntry {
            pid: 361,
            parent_pid: 360,
            uid,
            process_group_id: 360,
            session_id: 200,
            birth: Some((10, 11)),
            zombie: false,
        };
        let census_after_server = vec![reparented_g, reparented_g_leader, g_sub];
        let parents_after = parents_for(&census_after_server);
        let plan_after = plan_stop_scope(
            &census_after_server,
            &parents_after,
            &server_identity,
            false,
            &children,
            &discovered,
        )
        .unwrap();
        let targeted_pids = plan_after
            .live_processes
            .iter()
            .map(|entry| entry.pid)
            .collect::<BTreeSet<_>>();
        assert_eq!(targeted_pids, BTreeSet::from([350, 360, 361]));
        assert!(!plan_after.live_owned_groups.contains(&200));
        assert!(plan_after.live_owned_groups.contains(&360));
        assert!(!scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &discovered,
            &census_after_server,
            &parents_after,
        ));

        // Even if G_leader exits before G_sub was ever recorded into discovered, G_leader's
        // pgid (360) stays in absence_check_groups and G_sub blocks absence!
        let orphan_g_sub = ProcessCensusEntry {
            parent_pid: 1,
            session_id: 900,
            ..g_sub
        };
        let census_orphan_sub = vec![orphan_g_sub];
        let parents_orphan = parents_for(&census_orphan_sub);
        assert!(
            absence_check_groups(
                &worker_entry,
                &server_entry,
                &children,
                &discovered,
                &census_orphan_sub,
            )
            .contains(&360)
        );
        assert!(!scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &discovered,
            &census_orphan_sub,
            &parents_orphan,
        ));

        // Once all processes exit, absence is verified.
        assert!(scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &discovered,
            &[],
            &BTreeMap::new(),
        ));

        // Proven-recycled child leader C1 (distinct non-null birth) is excluded from absence_check_groups
        // and does not falsely block absence, whereas obscured-birth (birth: None) fails closed!
        let recycled_c1 = ProcessCensusEntry {
            pid: 300,
            parent_pid: 1,
            uid,
            process_group_id: 300,
            session_id: 300,
            birth: Some((99, 2)),
            zombie: false,
        };
        let census_recycled = vec![recycled_c1];
        let parents_recycled = parents_for(&census_recycled);
        let groups_recycled = absence_check_groups(
            &worker_entry,
            &server_entry,
            &children,
            &empty_discovered,
            &census_recycled,
        );
        assert!(!groups_recycled.contains(&300));
        assert!(groups_recycled.contains(&100));
        assert!(groups_recycled.contains(&200));
        assert!(scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &empty_discovered,
            &census_recycled,
            &parents_recycled,
        ));

        let obscured_c1 = ProcessCensusEntry {
            pid: 300,
            parent_pid: 1,
            uid: 0,
            process_group_id: 999,
            session_id: 999,
            birth: None,
            zombie: false,
        };
        let census_obscured = vec![obscured_c1];
        let parents_obscured = parents_for(&census_obscured);
        assert!(
            absence_check_groups(
                &worker_entry,
                &server_entry,
                &children,
                &empty_discovered,
                &census_obscured,
            )
            .contains(&300)
        );
        assert!(!scope_absence_verified(
            &worker_entry,
            &server_entry,
            false,
            &children,
            &empty_discovered,
            &census_obscured,
            &parents_obscured,
        ));
    }
}
