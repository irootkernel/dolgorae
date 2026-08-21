use crate::providers::{MonotonicClock, ProcessIdentity, ProviderError};
use std::time::Duration;

#[derive(Clone, Copy, Debug, Default)]
pub struct DarwinSystem;

impl DarwinSystem {
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
