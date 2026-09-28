use crate::providers::{MonotonicClock, ProcessIdentity, ProviderError};
use serde::{Deserialize, Serialize};
use std::ffi::{CStr, CString, OsStr, OsString};
use std::fs::File;
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

const F_SETLKWTIMEOUT: libc::c_int = 10;
const DARWIN_NSIG: libc::c_int = 32;
const PROC_PIDTBSDINFO: libc::c_int = 3;
const SZOMB: u32 = 5;

#[repr(C)]
#[derive(Clone, Copy)]
struct ProcBsdInfo {
    flags: u32,
    status: u32,
    xstatus: u32,
    pid: u32,
    parent_pid: u32,
    uid: u32,
    gid: u32,
    real_uid: u32,
    real_gid: u32,
    saved_uid: u32,
    saved_gid: u32,
    reserved: u32,
    command: [libc::c_char; 16],
    name: [libc::c_char; 32],
    open_file_count: u32,
    process_group_id: u32,
    process_job_control_count: u32,
    controlling_terminal_device: u32,
    terminal_process_group_id: u32,
    nice: i32,
    start_tvsec: u64,
    start_tvusec: u64,
}

#[link(name = "proc")]
unsafe extern "C" {
    fn proc_pidpath(pid: libc::c_int, buffer: *mut libc::c_void, buffer_size: u32) -> libc::c_int;
    fn proc_pidinfo(
        pid: libc::c_int,
        flavor: libc::c_int,
        argument: u64,
        buffer: *mut libc::c_void,
        buffer_size: libc::c_int,
    ) -> libc::c_int;
    fn proc_listpgrppids(
        process_group_id: libc::pid_t,
        buffer: *mut libc::c_void,
        buffer_size: libc::c_int,
    ) -> libc::c_int;
    fn proc_listallpids(buffer: *mut libc::c_void, buffer_size: libc::c_int) -> libc::c_int;
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LiveProcessIdentity {
    pub uid: u32,
    pub process_group_id: u32,
    pub fingerprint: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BsdProcessIdentity {
    pub pid: u32,
    pub parent_pid: u32,
    pub uid: u32,
    pub process_group_id: u32,
    pub session_id: u32,
    pub start_tvsec: u64,
    pub start_tvusec: u64,
    pub zombie: bool,
}

#[derive(Debug)]
pub struct ProcessExitWatch {
    queue: OwnedFd,
}

impl ProcessExitWatch {
    pub fn exited(&self) -> Result<bool, std::io::Error> {
        let mut event = MaybeUninit::<libc::kevent>::zeroed();
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: the queue descriptor is live, the output points to one
        // writable kevent, and the zero timeout is borrowed only for the call.
        let count = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                std::ptr::null(),
                0,
                event.as_mut_ptr(),
                1,
                &raw const timeout,
            )
        };
        if count < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(count == 1)
    }
}

/// The account and platform runtime fields a launched Runtime Profile needs,
/// read from the platform's own authorities rather than from this process's
/// environment.
///
/// ADR-016 assembles the launch environment "from the account fields required
/// for login" and "platform runtime fields". `getenv` reports what whoever
/// started Dolgorae chose to export, which is a caller-controlled input and
/// not an account fact: an inherited `HOME` or `SHELL` would silently
/// redirect the launched Codex's login identity, and an inherited `TMPDIR`
/// would place its scratch state outside the per-uid directory the platform
/// owns. The account database (`getpwuid_r`) and the platform temporary
/// directory service (`confstr(_CS_DARWIN_USER_TEMP_DIR)`) are those
/// authorities.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccountEnvironment {
    pub home: PathBuf,
    pub user: String,
    pub shell: PathBuf,
    pub temporary_directory: PathBuf,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AtNodeIdentity {
    pub device: u64,
    pub inode: u64,
    pub uid: u32,
    pub mode: u32,
    pub links: u64,
}

unsafe extern "C" {
    fn __pthread_fchdir(fd: libc::c_int) -> libc::c_int;
}

impl DarwinSystem {
    /// Bind in a thread-local directory, without modifying the process cwd.
    /// Darwin has no bindat; the dedicated thread owns the cwd override until exit.
    pub fn bind_unix_at(
        self,
        directory: &File,
        name: &OsStr,
    ) -> Result<std::os::unix::net::UnixListener, std::io::Error> {
        let directory = directory.try_clone()?;
        let name = name.to_owned();
        std::thread::spawn(move || {
            // SAFETY: the descriptor remains live on this dedicated thread. Darwin's
            // thread-local cwd override disappears when this thread exits.
            if unsafe { __pthread_fchdir(directory.as_raw_fd()) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
            std::os::unix::net::UnixListener::bind(Path::new(&name))
        })
        .join()
        .map_err(|_| std::io::Error::other("socket bind thread failed"))?
    }

    pub fn peer_uid(self, socket: &UnixStream) -> Result<u32, std::io::Error> {
        let mut uid = 0;
        let mut gid = 0;
        // SAFETY: socket is live and both output scalars remain writable for the call.
        if unsafe { libc::getpeereid(socket.as_raw_fd(), &raw mut uid, &raw mut gid) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(uid)
    }

    pub fn statat_nofollow(
        self,
        directory: &File,
        name: &OsStr,
    ) -> Result<AtNodeIdentity, std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let mut stat = MaybeUninit::<libc::stat>::uninit();
        // SAFETY: name is terminated, directory is live, and stat is writable.
        if unsafe {
            libc::fstatat(
                directory.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the complete structure.
        let stat = unsafe { stat.assume_init() };
        Ok(AtNodeIdentity {
            device: stat.st_dev as u64,
            inode: stat.st_ino,
            uid: stat.st_uid,
            mode: u32::from(stat.st_mode),
            links: u64::from(stat.st_nlink),
        })
    }

    pub fn mkdirat_private(self, directory: &File, name: &OsStr) -> Result<(), std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: the borrowed directory and terminated component remain live.
        if unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o700) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn createat_private(
        self,
        directory: &File,
        name: &OsStr,
        exclusive: bool,
    ) -> Result<File, std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let flags = libc::O_RDWR
            | libc::O_CLOEXEC
            | libc::O_NOFOLLOW
            | libc::O_NONBLOCK
            | libc::O_CREAT
            | if exclusive { libc::O_EXCL } else { 0 };
        // SAFETY: directory and component are live; mode is supplied for O_CREAT.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, 0o600) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: fd is a newly owned descriptor returned by openat.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn chmodat_private_socket(
        self,
        directory: &File,
        name: &OsStr,
    ) -> Result<(), std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: directory and name remain live; no-follow prevents symlink traversal.
        if unsafe {
            libc::fchmodat(
                directory.as_raw_fd(),
                name.as_ptr(),
                0o600,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn unlinkat_file(self, directory: &File, name: &OsStr) -> Result<(), std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: directory and name remain live; unlinkat does not follow the leaf.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn renameat_file(
        self,
        directory: &File,
        source: &OsStr,
        destination: &OsStr,
    ) -> Result<(), std::io::Error> {
        let source = CString::new(source.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let destination = CString::new(destination.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: all arguments remain live; both names resolve relative to one descriptor.
        if unsafe {
            libc::renameat(
                directory.as_raw_fd(),
                source.as_ptr(),
                directory.as_raw_fd(),
                destination.as_ptr(),
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    pub fn openat_nofollow(
        self,
        directory: &File,
        name: &OsStr,
        require_directory: bool,
    ) -> Result<File, std::io::Error> {
        let name = CString::new(name.as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let mut flags = libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        if require_directory {
            flags |= libc::O_DIRECTORY;
        } else {
            // Open special leaves without waiting so the caller can reject
            // non-regular files through descriptor metadata.
            flags |= libc::O_NONBLOCK;
        }
        // SAFETY: directory is a live descriptor, name is a NUL-terminated
        // component, and a successful result transfers one new descriptor.
        let raw = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw is the newly-created descriptor and ownership transfers
        // exactly once into File.
        Ok(unsafe { File::from_raw_fd(raw) })
    }

    pub fn watch_process_exit(self, pid: u32) -> Result<ProcessExitWatch, std::io::Error> {
        let pid = libc::pid_t::try_from(pid)
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        // SAFETY: kqueue has no arguments and returns a new owned descriptor.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: raw is the newly-created descriptor and ownership transfers
        // exactly once into OwnedFd.
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let change = libc::kevent {
            ident: pid as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_CLEAR,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: the input points to one initialized kevent and no output or
        // timeout is requested for this registration call.
        let registered = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                &raw const change,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if registered < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(ProcessExitWatch { queue })
    }

    pub fn boot_session_uuid(self) -> Result<String, std::io::Error> {
        let name = c"kern.bootsessionuuid";
        let mut length = 0_usize;
        // SAFETY: the first sysctlbyname call has a null output buffer and only
        // initializes `length` for a fixed kernel-owned scalar string.
        if unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::null_mut(),
                &raw mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || !(2..=128).contains(&length)
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut bytes = vec![0_u8; length];
        // SAFETY: `bytes` owns `length` writable bytes and sysctlbyname updates
        // only that buffer and the live length scalar.
        if unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                bytes.as_mut_ptr().cast(),
                &raw mut length,
                std::ptr::null_mut(),
                0,
            )
        } != 0
            || length == 0
            || length > bytes.len()
        {
            return Err(std::io::Error::last_os_error());
        }
        bytes.truncate(length);
        if bytes.last() == Some(&0) {
            bytes.pop();
        }
        let value = String::from_utf8(bytes).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "boot session UUID is not UTF-8",
            )
        })?;
        uuid::Uuid::parse_str(&value).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "boot session UUID is invalid",
            )
        })?;
        Ok(value)
    }

    pub fn bsd_process_identity(self, pid: u32) -> Result<BsdProcessIdentity, std::io::Error> {
        let pid = libc::c_int::try_from(pid).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "process ID is invalid")
        })?;
        let mut info = MaybeUninit::<ProcBsdInfo>::zeroed();
        let expected = libc::c_int::try_from(std::mem::size_of::<ProcBsdInfo>())
            .expect("proc_bsdinfo size fits c_int");
        // SAFETY: proc_pidinfo receives one correctly sized writable
        // ProcBsdInfo buffer. A short result is rejected before assume_init.
        let read =
            unsafe { proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), expected) };
        if read != expected {
            return Err(if read == 0 {
                std::io::Error::from_raw_os_error(libc::ESRCH)
            } else if read < 0 {
                std::io::Error::last_os_error()
            } else {
                std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "short BSD process identity",
                )
            });
        }
        // SAFETY: the exact structure size was initialized above.
        let info = unsafe { info.assume_init() };
        // SAFETY: getsid performs a read-only lookup and retains no pointer.
        let session = unsafe { libc::getsid(pid) };
        if session < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(BsdProcessIdentity {
            pid: info.pid,
            parent_pid: info.parent_pid,
            uid: info.uid,
            process_group_id: info.process_group_id,
            session_id: u32::try_from(session).map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid session ID")
            })?,
            start_tvsec: info.start_tvsec,
            start_tvusec: info.start_tvusec,
            zombie: info.status == SZOMB,
        })
    }

    /// Return the current group census. A group that vanishes during the lookup
    /// has an empty census; emptiness says nothing about its earlier members.
    /// Callers making ownership decisions must verify the recorded identity.
    pub fn process_group_pids(self, process_group_id: u32) -> Result<Vec<u32>, std::io::Error> {
        if process_group_id <= 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "process group must be greater than one",
            ));
        }
        let process_group_id = libc::pid_t::try_from(process_group_id).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "process group is invalid")
        })?;
        let mut capacity = 32_usize;
        loop {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let byte_size = capacity
                .checked_mul(std::mem::size_of::<libc::pid_t>())
                .and_then(|value| libc::c_int::try_from(value).ok())
                .ok_or_else(|| std::io::Error::other("process group census is too large"))?;
            // SAFETY: `pids` owns `byte_size` writable bytes and the API
            // returns a PID count, not a byte count.
            let count =
                unsafe { proc_listpgrppids(process_group_id, pids.as_mut_ptr().cast(), byte_size) };
            if count < 0 {
                let error = std::io::Error::last_os_error();
                if matches!(error.raw_os_error(), Some(libc::ENOENT | libc::ESRCH)) {
                    return Ok(Vec::new());
                }
                return Err(error);
            }
            let count = usize::try_from(count)
                .map_err(|_| std::io::Error::other("invalid process group census count"))?;
            if count > capacity {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "process group census overran its capacity",
                ));
            }
            if count == capacity {
                capacity = capacity
                    .checked_mul(2)
                    .filter(|value| *value <= 1_048_576)
                    .ok_or_else(|| {
                        std::io::Error::other("process group census did not converge")
                    })?;
                continue;
            }
            pids.truncate(count);
            let mut result = Vec::with_capacity(count);
            for pid in pids {
                if pid > 0 {
                    result.push(u32::try_from(pid).map_err(|_| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid census PID")
                    })?);
                }
            }
            result.sort_unstable();
            result.dedup();
            return Ok(result);
        }
    }

    pub fn all_process_identities(self) -> Result<Vec<BsdProcessIdentity>, std::io::Error> {
        let mut capacity = 1_024_usize;
        loop {
            let mut pids = vec![0 as libc::pid_t; capacity];
            let byte_size = capacity
                .checked_mul(std::mem::size_of::<libc::pid_t>())
                .and_then(|value| libc::c_int::try_from(value).ok())
                .ok_or_else(|| std::io::Error::other("all-process census is too large"))?;
            // SAFETY: `pids` owns `byte_size` writable bytes and libproc
            // retains no pointer after returning.
            let count = unsafe { proc_listallpids(pids.as_mut_ptr().cast(), byte_size) };
            if count < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let count = usize::try_from(count)
                .map_err(|_| std::io::Error::other("invalid all-process census count"))?;
            if count >= capacity {
                capacity = capacity
                    .checked_mul(2)
                    .filter(|value| *value <= 1_048_576)
                    .ok_or_else(|| std::io::Error::other("all-process census did not converge"))?;
                continue;
            }
            pids.truncate(count);
            let mut identities = Vec::with_capacity(count);
            for pid in pids {
                if pid <= 0 {
                    continue;
                }
                match self.bsd_process_identity(u32::try_from(pid).map_err(|_| {
                    std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid census PID")
                })?) {
                    Ok(identity) => identities.push(identity),
                    Err(error) if error.raw_os_error() == Some(libc::ESRCH) => {}
                    Err(error) => return Err(error),
                }
            }
            return Ok(identities);
        }
    }
    /// Take the command's inherited readiness descriptor before starting the runtime.
    pub fn take_ready_file(self, fd: RawFd) -> Result<File, std::io::Error> {
        if fd < 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ready descriptor must be nonnegative",
            ));
        }
        // SAFETY: F_GETFD validates the inherited descriptor without taking ownership.
        if unsafe { libc::fcntl(fd, libc::F_GETFD) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: the foreground serve command is the sole owner of its inherited
        // readiness descriptor and calls this once before opening gateway resources.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub fn duplicate_fd_cloexec(self, fd: RawFd) -> Result<OwnedFd, std::io::Error> {
        // SAFETY: fcntl borrows the caller's descriptor and returns a distinct
        // descriptor on success. Ownership of that new descriptor is transferred
        // exactly once to OwnedFd.
        let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
        if duplicate == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: a successful F_DUPFD_CLOEXEC result is a newly owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }

    pub fn send_fd(self, socket: &UnixStream, fd: RawFd) -> Result<(), std::io::Error> {
        self.send_byte_with_optional_fd(socket, 0, Some(fd))
    }

    pub fn receive_fd(self, socket: &UnixStream) -> Result<OwnedFd, std::io::Error> {
        match self.receive_byte_with_optional_fd(socket)? {
            (_, Some(descriptor)) => Ok(descriptor),
            (_, None) => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "SCM_RIGHTS descriptor is missing",
            )),
        }
    }

    /// Send exactly one stream byte, attaching `fd` as an `SCM_RIGHTS` control
    /// message when one is supplied.
    ///
    /// On `SOCK_STREAM` the control message is bound to the byte it travels
    /// with, so a caller that must hand a descriptor to a framed protocol sends
    /// the frame's first byte through here and the remainder through ordinary
    /// writes.
    pub fn send_byte_with_optional_fd(
        self,
        socket: &UnixStream,
        byte: u8,
        fd: Option<RawFd>,
    ) -> Result<(), std::io::Error> {
        let mut payload = [byte; 1];
        let mut io = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        let control_len = usize::try_from(unsafe {
            // SAFETY: CMSG_SPACE is a pure size calculation for one descriptor.
            libc::CMSG_SPACE(u32::try_from(std::mem::size_of::<RawFd>()).expect("fd size"))
        })
        .expect("control size");
        let mut control = vec![0_u8; control_len];
        let mut message = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &mut io,
            msg_iovlen: 1,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        if fd.is_some() {
            message.msg_control = control.as_mut_ptr().cast();
            message.msg_controllen = u32::try_from(control.len()).expect("control size");
        }
        // SAFETY: message points to live iovec storage, and the control buffer is
        // attached only when it holds enough CMSG_SPACE for exactly one RawFd.
        unsafe {
            if let Some(descriptor) = fd {
                let header = libc::CMSG_FIRSTHDR(&message);
                if header.is_null() {
                    return Err(std::io::Error::other("SCM_RIGHTS header unavailable"));
                }
                (*header).cmsg_level = libc::SOL_SOCKET;
                (*header).cmsg_type = libc::SCM_RIGHTS;
                (*header).cmsg_len =
                    libc::CMSG_LEN(u32::try_from(std::mem::size_of::<RawFd>()).expect("fd size"));
                std::ptr::write_unaligned(libc::CMSG_DATA(header).cast::<RawFd>(), descriptor);
            }
            if libc::sendmsg(socket.as_raw_fd(), &message, 0) != 1 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(())
    }

    /// Receive exactly one stream byte plus at most one `SCM_RIGHTS`
    /// descriptor.
    ///
    /// The read is bounded to a single byte so it can never cross the boundary
    /// the control message is attached to, and a sender that supplies truncated
    /// or multiple descriptors is refused after every descriptor it passed is
    /// closed.
    pub fn receive_byte_with_optional_fd(
        self,
        socket: &UnixStream,
    ) -> Result<(u8, Option<OwnedFd>), std::io::Error> {
        let mut payload = [0_u8; 1];
        let mut io = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        let control_len = usize::try_from(unsafe {
            // SAFETY: CMSG_SPACE is a pure size calculation for two descriptors,
            // so an oversupplying sender is observed rather than truncated away.
            libc::CMSG_SPACE(u32::try_from(2 * std::mem::size_of::<RawFd>()).expect("fd size"))
        })
        .expect("control size");
        let mut control = vec![0_u8; control_len];
        let mut message = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: &mut io,
            msg_iovlen: 1,
            msg_control: control.as_mut_ptr().cast(),
            msg_controllen: u32::try_from(control.len()).expect("control size"),
            msg_flags: 0,
        };
        // SAFETY: recvmsg writes only into the live iovec/control buffers. Every
        // descriptor the kernel installed is owned here exactly once and closed
        // on any refusal, so no received descriptor leaks.
        unsafe {
            if libc::recvmsg(socket.as_raw_fd(), &mut message, 0) != 1 {
                return Err(std::io::Error::last_os_error());
            }
            let mut received = Vec::new();
            let mut header = libc::CMSG_FIRSTHDR(&message);
            while !header.is_null() {
                if (*header).cmsg_level == libc::SOL_SOCKET
                    && (*header).cmsg_type == libc::SCM_RIGHTS
                {
                    let payload_len = usize::try_from((*header).cmsg_len)
                        .unwrap_or(0)
                        .saturating_sub(
                            usize::try_from(libc::CMSG_LEN(0)).expect("cmsg header size"),
                        );
                    let count = payload_len / std::mem::size_of::<RawFd>();
                    let data = libc::CMSG_DATA(header).cast::<RawFd>();
                    for index in 0..count {
                        received.push(std::ptr::read_unaligned(data.add(index)));
                    }
                }
                header = libc::CMSG_NXTHDR(&message, header);
            }
            let truncated = message.msg_flags & libc::MSG_CTRUNC != 0;
            if truncated || received.len() > 1 || received.iter().any(|fd| *fd < 0) {
                for descriptor in received {
                    if descriptor >= 0 {
                        libc::close(descriptor);
                    }
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "SCM_RIGHTS control message is invalid",
                ));
            }
            let Some(descriptor) = received.first().copied() else {
                return Ok((payload[0], None));
            };
            if libc::fcntl(descriptor, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                let error = std::io::Error::last_os_error();
                libc::close(descriptor);
                return Err(error);
            }
            Ok((payload[0], Some(OwnedFd::from_raw_fd(descriptor))))
        }
    }

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

    /// Reads the calling account's login fields and the platform's per-uid
    /// temporary directory. See [`AccountEnvironment`] for why these come
    /// from the platform rather than from the caller's environment.
    pub fn account_environment(self) -> Result<AccountEnvironment, std::io::Error> {
        let (home, user, shell) = self.account_record(self.current_uid())?;
        Ok(AccountEnvironment {
            home,
            user,
            shell,
            temporary_directory: self.user_temporary_directory()?,
        })
    }

    fn account_record(self, uid: u32) -> Result<(PathBuf, String, PathBuf), std::io::Error> {
        // SAFETY: sysconf takes no pointers and reports the reentrant
        // getpwuid_r buffer hint, or -1 when the platform declines to state one.
        let hint = unsafe { libc::sysconf(libc::_SC_GETPW_R_SIZE_MAX) };
        let mut length = usize::try_from(hint).unwrap_or(4096).clamp(1024, 64 * 1024);
        loop {
            let mut buffer = vec![0_u8; length];
            let mut record = MaybeUninit::<libc::passwd>::uninit();
            let mut found: *mut libc::passwd = std::ptr::null_mut();
            // SAFETY: record points to writable storage for one passwd
            // structure, buffer is a live allocation of exactly `length`
            // bytes that the call fills and the structure borrows from, and
            // found points to one live pointer. The buffer outlives every
            // read of the returned structure below.
            let code = unsafe {
                libc::getpwuid_r(
                    uid,
                    record.as_mut_ptr(),
                    buffer.as_mut_ptr().cast(),
                    length,
                    std::ptr::addr_of_mut!(found),
                )
            };
            if code == libc::ERANGE && length < 64 * 1024 {
                length *= 2;
                continue;
            }
            if code != 0 {
                return Err(std::io::Error::from_raw_os_error(code));
            }
            if found.is_null() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "account record is absent from the account database",
                ));
            }
            // SAFETY: getpwuid_r returned success with a non-null result, so
            // it initialized the structure and pointed its string members
            // into `buffer`, which is still live.
            let record = unsafe { record.assume_init() };
            let home = unsafe { Self::account_field(record.pw_dir) }?;
            let user = unsafe { Self::account_field(record.pw_name) }?;
            let shell = unsafe { Self::account_field(record.pw_shell) }?;
            let user = String::from_utf8(user.into_vec()).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "account login name is not UTF-8",
                )
            })?;
            return Ok((PathBuf::from(home), user, PathBuf::from(shell)));
        }
    }

    /// # Safety
    ///
    /// `field` must be null or a valid NUL-terminated string that outlives
    /// this call.
    unsafe fn account_field(field: *const libc::c_char) -> Result<OsString, std::io::Error> {
        if field.is_null() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "account record field is absent",
            ));
        }
        // SAFETY: the caller guarantees a valid NUL-terminated string; the
        // bytes are copied before this function returns.
        let bytes = unsafe { CStr::from_ptr(field) }.to_bytes().to_vec();
        if bytes.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "account record field is empty",
            ));
        }
        Ok(OsString::from_vec(bytes))
    }

    fn user_temporary_directory(self) -> Result<PathBuf, std::io::Error> {
        // SAFETY: a zero length with a null buffer asks confstr only for the
        // size it would write, which is the documented sizing call.
        let length =
            unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, std::ptr::null_mut(), 0) };
        if length == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "platform temporary directory is unavailable",
            ));
        }
        let mut buffer = vec![0_u8; length];
        // SAFETY: buffer is a live allocation of exactly `length` bytes and
        // confstr writes at most that many, NUL terminator included.
        let written = unsafe {
            libc::confstr(
                libc::_CS_DARWIN_USER_TEMP_DIR,
                buffer.as_mut_ptr().cast(),
                length,
            )
        };
        if written == 0 || written > length {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "platform temporary directory is unavailable",
            ));
        }
        buffer.truncate(written.saturating_sub(1));
        if buffer.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "platform temporary directory is empty",
            ));
        }
        Ok(PathBuf::from(OsString::from_vec(buffer)))
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

    pub fn lock_exclusive(self, file: &std::fs::File) -> Result<(), std::io::Error> {
        loop {
            // SAFETY: flock borrows the live descriptor and retains no pointer. The
            // resulting lock remains tied to the caller-owned open file description.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn unlock(self, file: &std::fs::File) -> Result<(), std::io::Error> {
        loop {
            // SAFETY: flock borrows the live descriptor and retains no pointer.
            // LOCK_UN releases the shared lock identity even when fork or dup
            // left another descriptor referring to the same open file.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) } == 0 {
                return Ok(());
            }
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }

    pub fn spawn_detached(self, command: &mut Command) -> Result<Child, std::io::Error> {
        // SAFETY: the closure uses only async-signal-safe libc calls after fork
        // and before exec, and retains no borrowed pointer after it returns.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                libc::umask(0o077);
                let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
                if libc::sigemptyset(empty.as_mut_ptr()) == -1
                    || libc::pthread_sigmask(
                        libc::SIG_SETMASK,
                        empty.as_ptr(),
                        std::ptr::null_mut(),
                    ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if reset_signal_dispositions() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn()
    }

    pub fn spawn_detached_with_log_fds(
        self,
        command: &mut Command,
        stdout_read_fd: libc::c_int,
        stderr_read_fd: libc::c_int,
    ) -> Result<Child, std::io::Error> {
        // SAFETY: the inherited descriptors are live until spawn returns. The
        // child duplicates them onto the fixed private drainer descriptors and
        // performs only async-signal-safe operations before exec.
        unsafe {
            command.pre_exec(move || {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                // Stage both read ends above the fixed drainer descriptors
                // before placing them. Two fixed targets carry two hazards a
                // single descriptor never faces: a source that already sits on
                // the *other* target is clobbered before it is placed, and
                // `dup2(fd, fd)` is a no-op that leaves FD_CLOEXEC set, so a
                // source that already sits on its own target disappears at
                // exec and the drainer finds nothing on /dev/fd/3. Which
                // descriptors the sources land on is decided by whatever the
                // parent happened to have free, so neither hazard can be ruled
                // out from here. `F_DUPFD` (not `F_DUPFD_CLOEXEC`) is what
                // makes the staged copies survive the exec.
                let staged_stdout = libc::fcntl(stdout_read_fd, libc::F_DUPFD, 10);
                let staged_stderr = libc::fcntl(stderr_read_fd, libc::F_DUPFD, 10);
                if staged_stdout == -1
                    || staged_stderr == -1
                    || libc::dup2(staged_stdout, 3) == -1
                    || libc::dup2(staged_stderr, 4) == -1
                    || libc::fcntl(3, libc::F_SETFD, 0) == -1
                    || libc::fcntl(4, libc::F_SETFD, 0) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                libc::close(staged_stdout);
                libc::close(staged_stderr);
                libc::umask(0o077);
                let mut empty = MaybeUninit::<libc::sigset_t>::uninit();
                if libc::sigemptyset(empty.as_mut_ptr()) == -1
                    || libc::pthread_sigmask(
                        libc::SIG_SETMASK,
                        empty.as_ptr(),
                        std::ptr::null_mut(),
                    ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                if reset_signal_dispositions() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.spawn()
    }

    #[must_use]
    pub fn process_exists(self, pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        // SAFETY: signal zero performs an identity/existence check and does not
        // deliver a signal or dereference pointers.
        unsafe {
            libc::kill(pid, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }

    pub fn signal_process_group(
        self,
        pgid: u32,
        signal: libc::c_int,
    ) -> Result<(), std::io::Error> {
        if pgid <= 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "process group is invalid",
            ));
        }
        let pgid = libc::pid_t::try_from(pgid).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "process group is invalid")
        })?;
        // SAFETY: a negative verified process-group ID and a caller-selected
        // signal are passed by value; no pointers or ownership cross the call.
        if unsafe { libc::kill(-pgid, signal) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }

    /// A missing live path alone permits the recorded-image fallback.
    pub fn live_process_path(self, pid: u32) -> Option<PathBuf> {
        let pid = libc::c_int::try_from(pid).ok()?;
        let mut bytes = vec![0_u8; 4096];
        // SAFETY: the buffer is writable for the declared capacity and the PID
        // is passed by value. proc_pidpath does not retain the pointer.
        let count = unsafe { proc_pidpath(pid, bytes.as_mut_ptr().cast(), 4096) };
        if count <= 0 {
            return None;
        }
        let end = bytes.iter().position(|byte| *byte == 0)?;
        if end == 0 {
            return None;
        }
        bytes.truncate(end);
        Some(PathBuf::from(OsString::from_vec(bytes)))
    }

    pub fn live_process_identity(self, pid: u32) -> Result<LiveProcessIdentity, std::io::Error> {
        let output = Command::new("/bin/ps")
            .args([
                "-p",
                &pid.to_string(),
                "-o",
                "uid=",
                "-o",
                "pgid=",
                "-o",
                "lstart=",
                "-o",
                "command=",
            ])
            .env_clear()
            .stdin(Stdio::null())
            .output()?;
        if !output.status.success() || output.stdout.len() > 16 * 1024 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "process identity is unavailable",
            ));
        }
        let fingerprint = String::from_utf8(output.stdout)
            .map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "ps output is not UTF-8")
            })?
            .trim()
            .to_owned();
        let mut fields = fingerprint.split_whitespace();
        let uid = fields
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "process uid is missing")
            })?
            .parse::<u32>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        let process_group_id = fields
            .next()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "process group is missing")
            })?
            .parse::<u32>()
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if fingerprint.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "process fingerprint is empty",
            ));
        }
        Ok(LiveProcessIdentity {
            uid,
            process_group_id,
            fingerprint,
        })
    }

    #[must_use]
    pub fn reap_child_nonblocking(self, pid: u32) -> bool {
        let Ok(pid) = libc::pid_t::try_from(pid) else {
            return false;
        };
        let mut status = 0;
        // SAFETY: waitpid receives a concrete PID and a pointer to one live
        // integer. WNOHANG prevents this cleanup check from blocking.
        unsafe { libc::waitpid(pid, std::ptr::addr_of_mut!(status), libc::WNOHANG) == pid }
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

unsafe fn reset_signal_dispositions() -> libc::c_int {
    // SAFETY: zero is a valid baseline for Darwin sigaction; the mask is then
    // initialized by sigemptyset before the structure is passed to sigaction.
    let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
    action.sa_sigaction = libc::SIG_DFL;
    // SAFETY: action owns one live signal mask and sigemptyset initializes it.
    if unsafe { libc::sigemptyset(&raw mut action.sa_mask) } == -1 {
        return -1;
    }
    for signal in 1..DARWIN_NSIG {
        if signal != libc::SIGKILL && signal != libc::SIGSTOP {
            // SAFETY: signal is in Darwin's valid range and action remains live
            // for the duration of this async-signal-safe syscall.
            if unsafe { libc::sigaction(signal, &raw const action, std::ptr::null_mut()) } == -1 {
                return -1;
            }
        }
    }
    0
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
    fn one_byte_transfer_carries_at_most_one_descriptor_and_survives_its_absence() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let temporary = std::env::temp_dir().join(format!(
            "dolgorae-scm-{}-{}",
            std::process::id(),
            DarwinSystem.now().as_nanos()
        ));
        std::fs::write(&temporary, b"carried").unwrap();
        let carried = std::fs::File::open(&temporary).unwrap();

        DarwinSystem
            .send_byte_with_optional_fd(&sender, b'{', Some(carried.as_raw_fd()))
            .unwrap();
        let (byte, received) = DarwinSystem
            .receive_byte_with_optional_fd(&receiver)
            .unwrap();
        assert_eq!(byte, b'{');
        let mut contents = String::new();
        std::fs::File::from(received.expect("descriptor was not delivered"))
            .read_to_string(&mut contents)
            .unwrap();
        assert_eq!(contents, "carried");

        // A caller that passes no descriptor still delivers its byte, so an
        // observer request stays byte-identical to an ordinary write.
        DarwinSystem
            .send_byte_with_optional_fd(&sender, b'[', None)
            .unwrap();
        let (byte, received) = DarwinSystem
            .receive_byte_with_optional_fd(&receiver)
            .unwrap();
        assert_eq!(byte, b'[');
        assert!(received.is_none());

        // An ordinary write is received the same way, which is what lets a
        // credential-free caller keep using plain framing.
        std::io::Write::write_all(&mut (&sender), b"xy").unwrap();
        assert_eq!(
            DarwinSystem
                .receive_byte_with_optional_fd(&receiver)
                .unwrap()
                .0,
            b'x'
        );
        let mut remainder = [0_u8; 1];
        std::io::Read::read_exact(&mut (&receiver), &mut remainder).unwrap();
        assert_eq!(&remainder, b"y");
        std::fs::remove_file(&temporary).unwrap();
    }

    #[test]
    fn a_sender_that_oversupplies_descriptors_is_refused_without_leaking_them() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let (spare_one, spare_two) = UnixStream::pair().unwrap();
        let mut payload = [b'!'; 1];
        let mut io = libc::iovec {
            iov_base: payload.as_mut_ptr().cast(),
            iov_len: payload.len(),
        };
        let descriptors = [spare_one.as_raw_fd(), spare_two.as_raw_fd()];
        // SAFETY: the control buffer is sized by CMSG_SPACE for exactly the two
        // descriptors written into it, and every pointer stays live across the
        // sendmsg call.
        unsafe {
            let control_len = usize::try_from(libc::CMSG_SPACE(
                u32::try_from(std::mem::size_of_val(&descriptors)).unwrap(),
            ))
            .unwrap();
            let mut control = vec![0_u8; control_len];
            let message = libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: 0,
                msg_iov: &mut io,
                msg_iovlen: 1,
                msg_control: control.as_mut_ptr().cast(),
                msg_controllen: u32::try_from(control.len()).unwrap(),
                msg_flags: 0,
            };
            let header = libc::CMSG_FIRSTHDR(&message);
            (*header).cmsg_level = libc::SOL_SOCKET;
            (*header).cmsg_type = libc::SCM_RIGHTS;
            (*header).cmsg_len =
                libc::CMSG_LEN(u32::try_from(std::mem::size_of_val(&descriptors)).unwrap());
            std::ptr::copy_nonoverlapping(
                descriptors.as_ptr(),
                libc::CMSG_DATA(header).cast::<RawFd>(),
                descriptors.len(),
            );
            assert_eq!(libc::sendmsg(sender.as_raw_fd(), &message, 0), 1);
        }
        let refused = DarwinSystem
            .receive_byte_with_optional_fd(&receiver)
            .unwrap_err();
        assert_eq!(refused.kind(), std::io::ErrorKind::InvalidData);
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

    /// The launched profile's reserved names are account and platform facts.
    /// This is the adapter that has to produce them without consulting the
    /// caller's environment at all.
    #[test]
    fn the_account_environment_comes_from_the_account_database_and_the_platform() {
        let account = DarwinSystem.account_environment().unwrap();

        assert!(account.home.is_absolute(), "{:?}", account.home);
        assert!(account.shell.is_absolute(), "{:?}", account.shell);
        assert!(
            account.temporary_directory.is_absolute(),
            "{:?}",
            account.temporary_directory
        );
        assert!(!account.user.is_empty());
        assert!(!account.user.contains('\0'));
        // Reading it twice is the same answer: it is a lookup, not a
        // snapshot of mutable process state.
        assert_eq!(DarwinSystem.account_environment().unwrap(), account);
    }

    #[test]
    fn kernel_boot_identity_and_bsd_process_census_are_self_consistent() {
        let boot = DarwinSystem.boot_session_uuid().unwrap();
        assert!(!uuid::Uuid::parse_str(&boot).unwrap().is_nil());

        let pid = std::process::id();
        let identity = DarwinSystem.bsd_process_identity(pid).unwrap();
        assert_eq!(identity.pid, pid);
        assert_eq!(identity.uid, DarwinSystem.current_uid());
        assert!(!identity.zombie);
        assert!(identity.start_tvsec > 0);

        let members = DarwinSystem
            .process_group_pids(identity.process_group_id)
            .unwrap();
        assert!(members.binary_search(&pid).is_ok());
        assert!(DarwinSystem.process_group_pids(1).is_err());
        assert!(DarwinSystem.signal_process_group(1, 0).is_err());
    }
}
