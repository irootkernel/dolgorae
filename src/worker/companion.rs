//! The qualified resident host is infrastructure only in a caller policy proof.
//! Generic background and generation-absence inspection never call this module.

use super::*;

const CODEX_SHA256: &str = "d8990e3ee7acfbc26f91265b12a1a83f0fee40d0e0f8f1f40a37d4ec21f499cf";
const HOST_SHA256: &str = "416721e33af8822041dc650793512b3e58d43e0be4f3a5c6187eb4dfaad3148a";

fn admitted(server: &DedicatedServerIdentity, host: &RetirementProcess) -> bool {
    let identity = &host.identity;
    server.executable_sha256 == CODEX_SHA256
        && host.sha256 == HOST_SHA256
        && host.executable
            == server
                .executable_path
                .with_file_name("codex-code-mode-host")
        && identity.parent_pid == server.pid
        && identity.uid == server.uid
        && identity.birth.is_some()
        && !identity.zombie
        && identity.session_id == server.session_id
        && identity.process_group_id == identity.pid
        && host.device != 0
        && host.inode != 0
}

pub(super) fn capture(
    runtime: &WorkerRuntimeRecord,
) -> Result<Option<RetirementProcess>, WorkerProtocolError> {
    let Some(server) = runtime.dedicated_server_identity.as_ref() else {
        return Err(WorkerProtocolError::InvalidRuntimeRecord);
    };
    if server.executable_sha256 != CODEX_SHA256 {
        return Ok(None);
    }
    let census = DarwinSystem
        .all_process_identities()
        .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    let path = server
        .executable_path
        .with_file_name("codex-code-mode-host");
    let mut companion = None;
    for process in census.iter().filter(|p| p.parent_pid == server.pid) {
        if DarwinSystem.live_process_path(process.pid).as_ref() != Some(&path) {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| WorkerProtocolError::InvalidIdentity)?;
        if !metadata.is_file() || metadata.mode() & 0o111 == 0 || metadata.mode() & 0o022 != 0 {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        let host = retirement_process(*process, vec![*process])?;
        if !admitted(server, &host) || companion.is_some() {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        companion = Some(host);
    }
    Ok(companion)
}

/// Each independent census must contain the unchanged captured host. A vanished
/// or replaced host invalidates this proof instead of admitting a new PID.
pub(super) fn verify(
    host: Option<&RetirementProcess>,
    census: &[crate::darwin::ProcessCensusEntry],
) -> Result<(), WorkerProtocolError> {
    if let Some(host) = host {
        let current = census
            .iter()
            .find(|p| p.pid == host.identity.pid)
            .ok_or(WorkerProtocolError::InvalidIdentity)?;
        if current.parent_pid != host.identity.parent_pid || current.zombie {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        let before = DarwinSystem
            .bsd_process_identity(current.pid)
            .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
        if before.parent_pid != current.parent_pid
            || before.uid != current.uid
            || before.process_group_id != current.process_group_id
            || before.session_id != current.session_id
            || Some((before.start_tvsec, before.start_tvusec)) != current.birth
            || before.zombie
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
        GenerationScope::verify_process(host, current)?;
        let after = DarwinSystem
            .bsd_process_identity(current.pid)
            .map_err(|_| WorkerProtocolError::InvalidIdentity)?;
        if before != after
            || DarwinSystem.live_process_path(current.pid).as_ref() != Some(&host.executable)
        {
            return Err(WorkerProtocolError::InvalidIdentity);
        }
    }
    Ok(())
}

pub(super) fn contains(
    host: Option<&RetirementProcess>,
    process: &crate::darwin::ProcessCensusEntry,
) -> bool {
    host.is_some_and(|host| host.identity.pid == process.pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_requires_the_exact_pair_image_and_direct_owned_generation() {
        let server = DedicatedServerIdentity {
            pid: 10,
            uid: 501,
            process_group_id: 10,
            session_id: 10,
            start_tvsec: 1,
            start_tvusec: 2,
            executable_path: PathBuf::from("/private/owned/codex"),
            executable_device: 1,
            executable_inode: 2,
            executable_sha256: CODEX_SHA256.into(),
        };
        let host = RetirementProcess {
            identity: crate::darwin::ProcessCensusEntry {
                pid: 11,
                parent_pid: 10,
                uid: 501,
                process_group_id: 11,
                session_id: 10,
                birth: Some((3, 4)),
                zombie: false,
            },
            ancestry: vec![],
            executable: PathBuf::from("/private/owned/codex-code-mode-host"),
            device: 1,
            inode: 3,
            sha256: HOST_SHA256.into(),
        };
        assert!(admitted(&server, &host));
        let mut changed_server = server.clone();
        changed_server.executable_sha256 = "0".repeat(64);
        assert!(!admitted(&changed_server, &host));
        for change in 0..8 {
            let mut changed = host.clone();
            match change {
                0 => changed.sha256 = "0".repeat(64),
                1 => changed.executable = PathBuf::from("/foreign/codex-code-mode-host"),
                2 => changed.identity.parent_pid += 1,
                3 => changed.identity.uid += 1,
                4 => changed.identity.session_id += 1,
                5 => changed.identity.process_group_id += 1,
                6 => changed.identity.birth = None,
                _ => changed.identity.zombie = true,
            }
            assert!(
                !admitted(&server, &changed),
                "accepted changed identity {change}"
            );
        }
        assert!(!contains(
            Some(&host),
            &crate::darwin::ProcessCensusEntry {
                pid: 12,
                parent_pid: 11,
                ..host.identity
            }
        ));
        assert!(verify(Some(&host), &[]).is_err());
        let mut reparented = host.identity;
        reparented.parent_pid += 1;
        assert!(verify(Some(&host), &[reparented]).is_err());
    }
}
