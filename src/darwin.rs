use crate::providers::{MonotonicClock, ProcessIdentity, ProviderError};
use std::ffi::{CStr, CString, OsString};
use std::mem::MaybeUninit;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub struct DarwinSystem;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FilesystemInfo {
    pub local: bool,
    pub filesystem_type: String,
}

impl DarwinSystem {
    pub fn realpath(self, path: &Path) -> Result<PathBuf, std::io::Error> {
        let path = c_path(path)?;
        // SAFETY: realpath is given a valid NUL-terminated input and a null
        // destination, so libc allocates the result. The returned allocation is
        // copied before exactly one free.
        let resolved = unsafe { libc::realpath(path.as_ptr(), std::ptr::null_mut()) };
        if resolved.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a successful realpath result is a valid NUL-terminated byte
        // string owned by this call and remains live until free below.
        let bytes = unsafe { CStr::from_ptr(resolved).to_bytes().to_vec() };
        // SAFETY: resolved was allocated by realpath and has not been freed.
        unsafe { libc::free(resolved.cast()) };
        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }

    pub fn filesystem_info(self, path: &Path) -> Result<FilesystemInfo, std::io::Error> {
        let path = c_path(path)?;
        let mut value = MaybeUninit::<libc::statfs>::uninit();
        // SAFETY: value points to writable storage for one statfs structure and
        // path is a valid NUL-terminated pathname.
        if unsafe { libc::statfs(path.as_ptr(), value.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: statfs returned success and initialized the entire structure.
        let value = unsafe { value.assume_init() };
        // SAFETY: Darwin guarantees f_fstypename is NUL-terminated within its
        // fixed-size field after a successful statfs call.
        let filesystem_type = unsafe { CStr::from_ptr(value.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .into_owned();
        Ok(FilesystemInfo {
            local: value.f_flags & u32::try_from(libc::MNT_LOCAL).expect("MNT_LOCAL is positive")
                != 0,
            filesystem_type,
        })
    }

    #[must_use]
    pub fn current_uid(self) -> u32 {
        // SAFETY: getuid takes no pointers and returns the calling process uid.
        unsafe { libc::getuid() }
    }

    pub fn rename_exclusive(self, source: &Path, destination: &Path) -> Result<(), std::io::Error> {
        let source = c_path(source)?;
        let destination = c_path(destination)?;
        // SAFETY: both path arguments are valid NUL-terminated strings. The
        // Darwin RENAME_EXCL flag makes the publication fail rather than replace
        // an existing destination.
        if unsafe { libc::renamex_np(source.as_ptr(), destination.as_ptr(), libc::RENAME_EXCL) }
            != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn current_process(self) -> Result<ProcessIdentity, ProviderError> {
        // SAFETY: these libc calls take no pointers, have no ownership effects, and
        // return process-local scalar identifiers. getpgid is checked for failure.
        let (pid, process_group_id, uid) = unsafe {
            let pid = libc::getpid();
            let process_group_id = libc::getpgid(pid);
            let uid = libc::getuid();
            (pid, process_group_id, uid)
        };
        if process_group_id < 0 {
            return Err(ProviderError(std::io::Error::last_os_error().to_string()));
        }
        Ok(ProcessIdentity {
            pid: u32::try_from(pid).map_err(|error| ProviderError(error.to_string()))?,
            process_group_id: u32::try_from(process_group_id)
                .map_err(|error| ProviderError(error.to_string()))?,
            uid,
        })
    }
}

fn c_path(path: &Path) -> Result<CString, std::io::Error> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "path contains an interior NUL byte",
        )
    })
}

impl MonotonicClock for DarwinSystem {
    fn now(&self) -> Duration {
        let mut timestamp = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: timestamp points to initialized writable storage for one
        // timespec and CLOCK_MONOTONIC has no caller-owned lifetime.
        let result = unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut timestamp) };
        assert_eq!(
            result, 0,
            "CLOCK_MONOTONIC must be available on supported Darwin"
        );
        Duration::new(
            u64::try_from(timestamp.tv_sec).expect("monotonic seconds are non-negative"),
            u32::try_from(timestamp.tv_nsec).expect("nanoseconds fit u32"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_identity_wrapper_returns_current_process() {
        let identity = DarwinSystem.current_process().unwrap();
        assert!(identity.pid > 0);
        assert!(identity.process_group_id > 0);
    }

    #[test]
    fn monotonic_clock_is_addressable() {
        let first = DarwinSystem.now();
        let second = DarwinSystem.now();
        assert!(second >= first);
    }
}
