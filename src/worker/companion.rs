//! The qualified resident host is infrastructure only in a caller policy proof.
//! Generic background and generation-absence inspection never call this module.

use super::*;

#[derive(Clone)]
struct SelectedImage {
    resolved_path: String,
    device: u64,
    inode: u64,
    sha256: String,
}

fn selected_image(path: &Path) -> Result<SelectedImage, WorkerProtocolError> {
    let before = fs::symlink_metadata(path).map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if !before.is_file() || before.mode() & 0o111 == 0 || before.mode() & 0o022 != 0 {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    let sha256 = file_sha256(path)?;
    let after = fs::symlink_metadata(path).map_err(|_| WorkerProtocolError::InvalidIdentity)?;
    if before.dev() != after.dev()
        || before.ino() != after.ino()
        || before.len() != after.len()
        || before.mode() != after.mode()
        || (before.ctime(), before.ctime_nsec()) != (after.ctime(), after.ctime_nsec())
    {
        return Err(WorkerProtocolError::InvalidIdentity);
    }
    Ok(SelectedImage {
        resolved_path: path
            .to_str()
            .ok_or(WorkerProtocolError::InvalidIdentity)?
            .to_owned(),
        device: before.dev(),
        inode: before.ino(),
        sha256,
    })
}

fn admitted(
    server: &DedicatedServerIdentity,
    image: &SelectedImage,
    host: &RetirementProcess,
) -> bool {
    let identity = &host.identity;
    host.sha256 == image.sha256
        && host.executable == Path::new(&image.resolved_path)
        && host.device == image.device
        && host.inode == image.inode
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
        let image = selected_image(&path)?;
        let host = retirement_process(*process, vec![*process])?;
        if !admitted(server, &image, &host) || companion.is_some() {
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
    fn selected_image_rejects_symlinks_and_unsafe_permissions() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = std::env::temp_dir().join(format!("companion-image-{}", Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let path = root.join("codex-code-mode-host");
        fs::write(&path, b"selected image").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let image = selected_image(&path).unwrap();
        assert_eq!(image.sha256, file_sha256(&path).unwrap());
        for mode in [0o600, 0o777] {
            fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
            assert!(selected_image(&path).is_err());
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let alias = root.join("alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        assert!(selected_image(&alias).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn admission_requires_the_captured_image_and_direct_owned_generation() {
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
            executable_sha256: crate::global_runtime::CURRENT_CODEX_SHA256.into(),
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
            sha256: crate::global_runtime::CURRENT_COMPANION_SHA256.into(),
        };
        let image = SelectedImage {
            resolved_path: host.executable.to_str().unwrap().into(),
            device: host.device,
            inode: host.inode,
            sha256: host.sha256.clone(),
        };
        assert!(admitted(&server, &image, &host));
        let mut unregistered_server = server.clone();
        unregistered_server.executable_sha256 = "b".repeat(64);
        let mut unregistered_host = host.clone();
        unregistered_host.sha256 = "c".repeat(64);
        let mut unregistered_image = image.clone();
        unregistered_image.sha256 = unregistered_host.sha256.clone();
        assert!(admitted(
            &unregistered_server,
            &unregistered_image,
            &unregistered_host
        ));
        for change in 0..10 {
            let mut changed = host.clone();
            match change {
                0 => changed.sha256 = "0".repeat(64),
                1 => changed.executable = PathBuf::from("/foreign/codex-code-mode-host"),
                2 => changed.identity.parent_pid += 1,
                3 => changed.identity.uid += 1,
                4 => changed.identity.session_id += 1,
                5 => changed.identity.process_group_id += 1,
                6 => changed.identity.birth = None,
                7 => changed.identity.zombie = true,
                8 => changed.device += 1,
                _ => changed.inode += 1,
            }
            assert!(
                !admitted(&server, &image, &changed),
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
