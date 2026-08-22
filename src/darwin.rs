use crate::providers::{MonotonicClock, ProcessIdentity, ProviderError};
use std::ffi::{CStr, CString, OsString};
use std::mem::MaybeUninit;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::io::AsRawFd as _;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const F_SETLKWTIMEOUT: libc::c_int = 10;
const DARWIN_NSIG: libc::c_int = 32;

#[repr(C)]
struct FlockTimeout {
    fl: libc::flock,
    timeout: libc::timespec,
}

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

    pub fn lock_exclusive_nonblocking(self, file: &std::fs::File) -> Result<(), std::io::Error> {
        // SAFETY: flock receives the live descriptor borrowed from `file`; it
        // neither takes ownership nor outlives the call. The lock remains tied
        // to the open file description retained by the caller.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn lock_byte_timeout(
        self,
        file: &std::fs::File,
        offset: i64,
        timeout: Duration,
    ) -> Result<(), std::io::Error> {
        let start = libc::off_t::try_from(offset).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock offset is invalid")
        })?;
        let seconds = libc::time_t::try_from(timeout.as_secs()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock timeout is invalid")
        })?;
        let nanoseconds = libc::c_long::from(timeout.subsec_nanos());
        let mut request = FlockTimeout {
            fl: libc::flock {
                l_start: start,
                l_len: 1,
                l_pid: 0,
                l_type: libc::F_WRLCK,
                l_whence: libc::SEEK_SET as i16,
            },
            timeout: libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanoseconds,
            },
        };
        // SAFETY: fcntl receives a live borrowed descriptor and a pointer to a
        // correctly laid-out Darwin flocktimeout that remains valid for the
        // duration of the blocking call. Ownership of the descriptor is not
        // transferred.
        if unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                F_SETLKWTIMEOUT,
                std::ptr::addr_of_mut!(request),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn try_lock_byte(self, file: &std::fs::File, offset: i64) -> Result<(), std::io::Error> {
        let start = libc::off_t::try_from(offset).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock offset is invalid")
        })?;
        let mut request = libc::flock {
            l_start: start,
            l_len: 1,
            l_pid: 0,
            l_type: libc::F_WRLCK,
            l_whence: libc::SEEK_SET as i16,
        };
        // SAFETY: fcntl borrows the live descriptor and reads the initialized
        // flock during this nonblocking call without retaining the pointer.
        if unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_SETLK,
                std::ptr::addr_of_mut!(request),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn unlock_byte(self, file: &std::fs::File, offset: i64) -> Result<(), std::io::Error> {
        let start = libc::off_t::try_from(offset).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "lock offset is invalid")
        })?;
        let mut request = libc::flock {
            l_start: start,
            l_len: 1,
            l_pid: 0,
            l_type: libc::F_UNLCK,
            l_whence: libc::SEEK_SET as i16,
        };
        // SAFETY: fcntl borrows the live descriptor and reads the initialized
        // flock during this call without retaining the pointer.
        if unsafe {
            libc::fcntl(
                file.as_raw_fd(),
                libc::F_SETLK,
                std::ptr::addr_of_mut!(request),
            )
        } == -1
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    /// Spawn a session-detached child whose only startup channel is fd 3.
    /// Standard streams are disconnected before the fork, and inherited signal
    /// state is normalized before re-exec.
    pub fn spawn_detached_with_fd3(
        self,
        command: &mut Command,
    ) -> Result<(Child, UnixStream), std::io::Error> {
        let (parent, child_channel) = UnixStream::pair()?;
        let child_fd = child_channel.as_raw_fd();
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: the closure uses only async-signal-safe libc calls between
        // fork and exec. The captured descriptor remains open until spawn
        // returns and is duplicated to the fixed startup descriptor.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::umask(0o077);
                if child_fd != 3 && libc::dup2(child_fd, 3) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::fcntl(3, libc::F_SETFD, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
                if libc::sigemptyset(empty.as_mut_ptr()) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                if libc::pthread_sigmask(libc::SIG_SETMASK, empty.as_ptr(), std::ptr::null_mut())
                    != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                for signal in 1..DARWIN_NSIG {
                    if signal != libc::SIGKILL && signal != libc::SIGSTOP {
                        libc::signal(signal, libc::SIG_DFL);
                    }
                }
                Ok(())
            });
        }
        let child = command.spawn()?;
        drop(child_channel);
        Ok((child, parent))
    }

    pub fn install_worker_signal_policy(self) -> Result<(), std::io::Error> {
        // SAFETY: signal disposition and mask changes affect only the current
        // post-exec worker process. SIGINT/SIGHUP are intentionally ignored,
        // and SIGTERM is synchronously consumed by the worker signal thread.
        unsafe {
            if libc::signal(libc::SIGINT, libc::SIG_IGN) == libc::SIG_ERR
                || libc::signal(libc::SIGHUP, libc::SIG_IGN) == libc::SIG_ERR
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut termination = MaybeUninit::<libc::sigset_t>::uninit();
            if libc::sigemptyset(termination.as_mut_ptr()) == -1
                || libc::sigaddset(termination.as_mut_ptr(), libc::SIGTERM) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::pthread_sigmask(libc::SIG_BLOCK, termination.as_ptr(), std::ptr::null_mut())
                != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    pub fn wait_for_worker_sigterm(self) -> Result<(), std::io::Error> {
        // SAFETY: the set is fully initialized and contains only SIGTERM. The
        // caller blocks that signal before creating worker threads, so sigwait
        // is the sole synchronous consumer.
        unsafe {
            let mut termination = MaybeUninit::<libc::sigset_t>::uninit();
            if libc::sigemptyset(termination.as_mut_ptr()) == -1
                || libc::sigaddset(termination.as_mut_ptr(), libc::SIGTERM) == -1
            {
                return Err(std::io::Error::last_os_error());
            }
            let mut observed = 0;
            let result = libc::sigwait(termination.as_ptr(), &mut observed);
            if result != 0 || observed != libc::SIGTERM {
                return Err(if result == 0 {
                    std::io::Error::other("sigwait returned an unexpected signal")
                } else {
                    std::io::Error::from_raw_os_error(result)
                });
            }
        }
        Ok(())
    }

    pub fn close_startup_fd3(self) -> Result<(), std::io::Error> {
        // SAFETY: fd 3 is the fixed startup descriptor owned by the hidden
        // worker. This function is called exactly once after the terminal
        // startup handoff and deliberately invalidates that descriptor.
        if unsafe { libc::close(3) } == -1 {
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
    use std::io::Read as _;

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

    #[test]
    fn detached_spawn_preserves_only_the_fd3_startup_channel() {
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            "printf ready >&3; printf leaked; printf diagnostic >&2",
        ]);
        let (mut child, mut startup) = DarwinSystem.spawn_detached_with_fd3(&mut command).unwrap();
        let mut message = String::new();
        startup.read_to_string(&mut message).unwrap();
        assert!(child.wait().unwrap().success());
        assert_eq!(message, "ready");
    }
}
