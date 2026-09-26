//! Verify the Unix rendezvous layout used by Codex 0.157.1 app-server.

use sha2::{Digest as _, Sha256};
use std::fs::{self, Metadata};
use std::io::{self, ErrorKind};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};
use std::path::{Path, PathBuf};

fn invalid_socket() -> io::Error {
    io::Error::new(
        ErrorKind::InvalidData,
        "Codex socket rendezvous identity mismatch",
    )
}

/// Returns the physical socket metadata only for a direct socket or Codex's
/// exact protected rendezvous. Never follows an arbitrary symlink.
pub(crate) fn inspect(path: &Path, uid: u32) -> io::Result<Metadata> {
    let entry = fs::symlink_metadata(path)?;
    if entry.file_type().is_socket() {
        return (entry.uid() == uid)
            .then_some(entry)
            .ok_or_else(invalid_socket);
    }
    if !entry.file_type().is_symlink() || entry.uid() != uid {
        return Err(invalid_socket());
    }
    let root = verified_root(uid)?;
    let target = expected_target(path, &root)?;
    if fs::read_link(path)? != target {
        return Err(invalid_socket());
    }
    let socket = fs::symlink_metadata(target)?;
    if !socket.file_type().is_socket() || socket.uid() != uid || socket.mode() & 0o777 != 0o600 {
        return Err(invalid_socket());
    }
    Ok(socket)
}

/// Preflight an unpublished Dedicated Server's two socket locations. The
/// caller holds its startup lock until the server is published or aborted.
pub(crate) fn require_vacant(path: &Path, uid: u32) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
        Ok(_) => return Err(invalid_socket()),
    }
    let root = protected_root(uid)?;
    let target = expected_target(path, &root)?;
    match fs::symlink_metadata(&root) {
        Ok(metadata)
            if metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o777 == 0o700 => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        _ => return Err(invalid_socket()),
    }
    match fs::symlink_metadata(target) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) => Err(invalid_socket()),
    }
}

/// Cleanup for a launch whose two paths were vacant before spawn. The caller
/// must prove that exact spawned process group absent before using this.
pub(crate) fn remove_unpublished(path: &Path, uid: u32) -> io::Result<()> {
    match inspect(path, uid) {
        Ok(metadata) => remove_recorded(path, uid, metadata.dev(), metadata.ino()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            let root = protected_root(uid)?;
            let target = expected_target(path, &root)?;
            match fs::symlink_metadata(target) {
                Ok(metadata)
                    if metadata.file_type().is_socket()
                        && metadata.uid() == uid
                        && metadata.mode() & 0o777 == 0o600 =>
                {
                    remove_recorded(path, uid, metadata.dev(), metadata.ino())
                }
                Err(error) if error.kind() == ErrorKind::NotFound => {
                    remove_recorded(path, uid, 0, 0)
                }
                Err(error) => Err(error),
                _ => Err(invalid_socket()),
            }
        }
        Err(error) => Err(error),
    }
}

/// Remove a recorded socket after its process scope has been proven absent.
/// An exact dangling alias and a matching physical socket without its alias
/// are both recoverable; an unrelated entry is never removed.
pub(crate) fn remove_recorded(path: &Path, uid: u32, device: u64, inode: u64) -> io::Result<()> {
    let entry = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    if let Some(metadata) = entry.as_ref()
        && metadata.file_type().is_socket()
    {
        if metadata.uid() != uid || metadata.dev() != device || metadata.ino() != inode {
            return Err(invalid_socket());
        }
        return fs::remove_file(path);
    }
    if let Some(metadata) = entry.as_ref()
        && (!metadata.file_type().is_symlink() || metadata.uid() != uid)
    {
        return Err(invalid_socket());
    }
    let root = protected_root(uid)?;
    let target = expected_target(path, &root)?;
    if entry.is_some() && fs::read_link(path)? != target {
        return Err(invalid_socket());
    }
    match fs::symlink_metadata(&root) {
        Ok(metadata)
            if metadata.is_dir() && metadata.uid() == uid && metadata.mode() & 0o777 == 0o700 => {}
        Err(error) if error.kind() == ErrorKind::NotFound && entry.is_none() => return Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return fs::remove_file(path);
        }
        _ => return Err(invalid_socket()),
    }
    match fs::symlink_metadata(&target) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket()
                || metadata.uid() != uid
                || metadata.mode() & 0o777 != 0o600
                || metadata.dev() != device
                || metadata.ino() != inode
            {
                return Err(invalid_socket());
            }
            fs::remove_file(&target)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    if entry.is_some() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn protected_root(uid: u32) -> io::Result<PathBuf> {
    Ok(fs::canonicalize("/tmp")?.join(format!("codex-daemon-{uid}")))
}

fn verified_root(uid: u32) -> io::Result<PathBuf> {
    let root = protected_root(uid)?;
    let metadata = fs::symlink_metadata(&root)?;
    if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o777 != 0o700 {
        return Err(invalid_socket());
    }
    Ok(root)
}

fn expected_target(path: &Path, root: &Path) -> io::Result<PathBuf> {
    let parent = path.parent().ok_or_else(invalid_socket)?;
    let name = path.file_name().ok_or_else(invalid_socket)?;
    let canonical = fs::canonicalize(parent)?.join(name);
    let digest = Sha256::digest(canonical.as_os_str().as_bytes());
    Ok(root.join(format!("{digest:x}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _, symlink};
    use std::os::unix::net::UnixListener;

    #[test]
    fn exact_rendezvous_is_accepted_and_redirect_is_rejected() {
        let root = Path::new("/tmp").join(format!("dgcs-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let rendezvous = root.join("server.sock");
        let uid = fs::metadata(&root).unwrap().uid();
        let protected = protected_root(uid).unwrap();
        match fs::DirBuilder::new().mode(0o700).create(&protected) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => panic!("protected test directory: {error}"),
        }
        let target = expected_target(&rendezvous, &protected).unwrap();
        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &rendezvous).unwrap();
        assert_eq!(
            inspect(&rendezvous, uid).unwrap().ino(),
            fs::metadata(&target).unwrap().ino()
        );
        fs::remove_file(&rendezvous).unwrap();
        let other = root.join("other.sock");
        let _other_listener = UnixListener::bind(&other).unwrap();
        symlink(&other, &rendezvous).unwrap();
        assert!(inspect(&rendezvous, uid).is_err());
        drop(listener);
        fs::remove_file(target).unwrap();
        fs::remove_file(rendezvous).unwrap();
        fs::remove_file(other).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn recorded_cleanup_handles_crash_residue_without_removing_changed_entries() {
        let root = Path::new("/tmp").join(format!("dgcs-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let rendezvous = root.join("server.sock");
        let uid = fs::metadata(&root).unwrap().uid();
        let protected = protected_root(uid).unwrap();
        match fs::DirBuilder::new().mode(0o700).create(&protected) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => panic!("protected test directory: {error}"),
        }
        let target = expected_target(&rendezvous, &protected).unwrap();

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let recorded = fs::symlink_metadata(&target).unwrap();
        symlink(&target, &rendezvous).unwrap();
        drop(listener);
        remove_recorded(&rendezvous, uid, recorded.dev(), recorded.ino()).unwrap();
        assert!(fs::symlink_metadata(&target).is_err());
        assert!(fs::symlink_metadata(&rendezvous).is_err());

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let recorded = fs::symlink_metadata(&target).unwrap();
        symlink(&target, &rendezvous).unwrap();
        drop(listener);
        fs::remove_file(&target).unwrap();
        remove_recorded(&rendezvous, uid, recorded.dev(), recorded.ino()).unwrap();
        assert!(fs::symlink_metadata(&rendezvous).is_err());

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let recorded = fs::symlink_metadata(&target).unwrap();
        symlink(&target, &rendezvous).unwrap();
        drop(listener);
        fs::remove_file(&rendezvous).unwrap();
        remove_recorded(&rendezvous, uid, recorded.dev(), recorded.ino()).unwrap();
        assert!(fs::symlink_metadata(&target).is_err());

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &rendezvous).unwrap();
        drop(listener);
        assert!(
            remove_recorded(
                &rendezvous,
                uid,
                recorded.dev(),
                recorded.ino().wrapping_add(1)
            )
            .is_err()
        );
        assert!(fs::symlink_metadata(&target).is_ok());
        assert!(fs::symlink_metadata(&rendezvous).is_ok());
        fs::remove_file(&rendezvous).unwrap();
        let other = root.join("other.sock");
        let other_listener = UnixListener::bind(&other).unwrap();
        symlink(&other, &rendezvous).unwrap();
        let current = fs::symlink_metadata(&target).unwrap();
        assert!(remove_recorded(&rendezvous, uid, current.dev(), current.ino()).is_err());
        assert!(fs::symlink_metadata(&target).is_ok());
        assert_eq!(fs::read_link(&rendezvous).unwrap(), other);
        drop(other_listener);
        fs::remove_file(&target).unwrap();
        fs::remove_file(&rendezvous).unwrap();
        fs::remove_file(&other).unwrap();
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn unpublished_cleanup_requires_vacant_paths_and_exact_ownership() {
        let root = Path::new("/tmp").join(format!("dgcs-{}", uuid::Uuid::now_v7()));
        fs::create_dir(&root).unwrap();
        let rendezvous = root.join("server.sock");
        let uid = fs::metadata(&root).unwrap().uid();
        let protected = protected_root(uid).unwrap();
        match fs::DirBuilder::new().mode(0o700).create(&protected) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
            Err(error) => panic!("protected test directory: {error}"),
        }
        let target = expected_target(&rendezvous, &protected).unwrap();
        require_vacant(&rendezvous, uid).unwrap();

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &rendezvous).unwrap();
        assert!(require_vacant(&rendezvous, uid).is_err());
        drop(listener);
        remove_unpublished(&rendezvous, uid).unwrap();
        require_vacant(&rendezvous, uid).unwrap();

        let listener = UnixListener::bind(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(require_vacant(&rendezvous, uid).is_err());
        drop(listener);
        remove_unpublished(&rendezvous, uid).unwrap();
        require_vacant(&rendezvous, uid).unwrap();

        symlink(&target, &rendezvous).unwrap();
        remove_unpublished(&rendezvous, uid).unwrap();
        require_vacant(&rendezvous, uid).unwrap();

        let other = root.join("other.sock");
        symlink(&other, &rendezvous).unwrap();
        assert!(remove_unpublished(&rendezvous, uid).is_err());
        assert_eq!(fs::read_link(&rendezvous).unwrap(), other);
        fs::remove_file(&rendezvous).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
