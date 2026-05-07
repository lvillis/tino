use std::convert::Infallible;
use std::ffi::CString;
use std::fmt;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

pub(super) type Result<T> = std::result::Result<T, Errno>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(crate) struct Errno(i32);

impl Errno {
    pub(super) const E2BIG: Self = Self(libc::E2BIG);
    pub(super) const EACCES: Self = Self(libc::EACCES);
    pub(super) const EAGAIN: Self = Self(libc::EAGAIN);
    pub(super) const ECHILD: Self = Self(libc::ECHILD);
    pub(super) const EINVAL: Self = Self(libc::EINVAL);
    pub(super) const EINTR: Self = Self(libc::EINTR);
    pub(super) const EIO: Self = Self(libc::EIO);
    pub(super) const ENOENT: Self = Self(libc::ENOENT);
    pub(super) const ENOEXEC: Self = Self(libc::ENOEXEC);
    pub(super) const ENOSYS: Self = Self(libc::ENOSYS);
    pub(super) const ENOTDIR: Self = Self(libc::ENOTDIR);
    pub(super) const EOPNOTSUPP: Self = Self(libc::EOPNOTSUPP);
    pub(super) const EPERM: Self = Self(libc::EPERM);
    pub(super) const ESRCH: Self = Self(libc::ESRCH);

    pub(super) fn last() -> Self {
        let code = std::io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(libc::EIO);
        Self(code)
    }

    pub(super) const fn from_raw(code: i32) -> Self {
        Self(code)
    }

    pub(super) const fn raw(self) -> i32 {
        self.0
    }
}

impl fmt::Display for Errno {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", std::io::Error::from_raw_os_error(self.0))
    }
}

impl std::error::Error for Errno {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct Pid(libc::pid_t);

impl Pid {
    pub(super) const fn from_raw(pid: libc::pid_t) -> Self {
        Self(pid)
    }

    pub(super) const fn as_raw(self) -> libc::pid_t {
        self.0
    }
}

impl fmt::Display for Pid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForkResult {
    Parent { child: Pid },
    Child,
}

#[allow(clippy::upper_case_acronyms)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(i32)]
pub(crate) enum Signal {
    SIGHUP = libc::SIGHUP,
    SIGINT = libc::SIGINT,
    SIGQUIT = libc::SIGQUIT,
    SIGILL = libc::SIGILL,
    SIGTRAP = libc::SIGTRAP,
    SIGABRT = libc::SIGABRT,
    SIGBUS = libc::SIGBUS,
    SIGFPE = libc::SIGFPE,
    SIGKILL = libc::SIGKILL,
    SIGUSR1 = libc::SIGUSR1,
    SIGSEGV = libc::SIGSEGV,
    SIGUSR2 = libc::SIGUSR2,
    SIGPIPE = libc::SIGPIPE,
    SIGALRM = libc::SIGALRM,
    SIGTERM = libc::SIGTERM,
    SIGCHLD = libc::SIGCHLD,
    SIGCONT = libc::SIGCONT,
    SIGWINCH = libc::SIGWINCH,
    SIGTTIN = libc::SIGTTIN,
    SIGTTOU = libc::SIGTTOU,
    SIGSYS = libc::SIGSYS,
}

impl TryFrom<i32> for Signal {
    type Error = i32;

    fn try_from(value: i32) -> std::result::Result<Self, Self::Error> {
        match value {
            libc::SIGHUP => Ok(Self::SIGHUP),
            libc::SIGINT => Ok(Self::SIGINT),
            libc::SIGQUIT => Ok(Self::SIGQUIT),
            libc::SIGILL => Ok(Self::SIGILL),
            libc::SIGTRAP => Ok(Self::SIGTRAP),
            libc::SIGABRT => Ok(Self::SIGABRT),
            libc::SIGBUS => Ok(Self::SIGBUS),
            libc::SIGFPE => Ok(Self::SIGFPE),
            libc::SIGKILL => Ok(Self::SIGKILL),
            libc::SIGUSR1 => Ok(Self::SIGUSR1),
            libc::SIGSEGV => Ok(Self::SIGSEGV),
            libc::SIGUSR2 => Ok(Self::SIGUSR2),
            libc::SIGPIPE => Ok(Self::SIGPIPE),
            libc::SIGALRM => Ok(Self::SIGALRM),
            libc::SIGTERM => Ok(Self::SIGTERM),
            libc::SIGCHLD => Ok(Self::SIGCHLD),
            libc::SIGCONT => Ok(Self::SIGCONT),
            libc::SIGWINCH => Ok(Self::SIGWINCH),
            libc::SIGTTIN => Ok(Self::SIGTTIN),
            libc::SIGTTOU => Ok(Self::SIGTTOU),
            libc::SIGSYS => Ok(Self::SIGSYS),
            _ => Err(value),
        }
    }
}

pub(super) const SIGABRT: Signal = Signal::SIGABRT;
pub(super) const SIGBUS: Signal = Signal::SIGBUS;
pub(super) const SIGCHLD: Signal = Signal::SIGCHLD;
pub(super) const SIGFPE: Signal = Signal::SIGFPE;
pub(super) const SIGILL: Signal = Signal::SIGILL;
pub(super) const SIGINT: Signal = Signal::SIGINT;
pub(super) const SIGKILL: Signal = Signal::SIGKILL;
pub(super) const SIGQUIT: Signal = Signal::SIGQUIT;
pub(super) const SIGSEGV: Signal = Signal::SIGSEGV;
pub(super) const SIGSYS: Signal = Signal::SIGSYS;
pub(super) const SIGTERM: Signal = Signal::SIGTERM;
pub(super) const SIGTRAP: Signal = Signal::SIGTRAP;
pub(super) const SIGTTIN: Signal = Signal::SIGTTIN;
pub(super) const SIGTTOU: Signal = Signal::SIGTTOU;

pub(super) struct SigSet(libc::sigset_t);

impl SigSet {
    pub(super) fn all() -> Self {
        // SAFETY: sigset_t is plain old data and is immediately initialized by sigfillset.
        let mut set = unsafe { zeroed() };
        // SAFETY: pointer is valid for writes.
        unsafe { libc::sigfillset(&mut set) };
        Self(set)
    }

    pub(super) fn remove(&mut self, signal: Signal) {
        // SAFETY: pointer is valid and signal number comes from our enum.
        unsafe {
            libc::sigdelset(&mut self.0, signal as i32);
        }
    }

    pub(super) fn thread_get_mask() -> Result<Self> {
        // SAFETY: sigset_t is plain old data and is immediately written by pthread_sigmask.
        let mut set = unsafe { zeroed() };
        // SAFETY: null new mask means query-only; oldset pointer is valid.
        let rc = unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, std::ptr::null(), &mut set) };
        if rc == 0 {
            Ok(Self(set))
        } else {
            Err(Errno::from_raw(rc))
        }
    }

    pub(super) fn thread_set_mask(&self) -> Result<()> {
        // SAFETY: set pointer is valid; null oldset means no previous mask capture.
        let rc =
            unsafe { libc::pthread_sigmask(libc::SIG_SETMASK, &self.0, std::ptr::null_mut()) };
        if rc == 0 {
            Ok(())
        } else {
            Err(Errno::from_raw(rc))
        }
    }

    fn as_ptr(&self) -> *const libc::sigset_t {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct PollFlags(libc::c_short);

impl PollFlags {
    pub(super) const POLLIN: Self = Self(libc::POLLIN);
    pub(super) const POLLERR: Self = Self(libc::POLLERR);
    pub(super) const POLLHUP: Self = Self(libc::POLLHUP);
    pub(super) const POLLNVAL: Self = Self(libc::POLLNVAL);

    pub(super) const fn empty() -> Self {
        Self(0)
    }

    pub(super) const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    pub(super) const fn intersects(self, other: Self) -> bool {
        (self.0 & other.0) != 0
    }
}

pub(super) struct PollFd {
    fd: RawFd,
    events: PollFlags,
    revents: Option<PollFlags>,
}

impl PollFd {
    pub(super) fn new(fd: BorrowedFd<'_>, events: PollFlags) -> Self {
        Self {
            fd: fd.as_raw_fd(),
            events,
            revents: None,
        }
    }

    pub(super) fn revents(&self) -> Option<PollFlags> {
        self.revents
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(transparent)]
pub(super) struct PollTimeout(i32);

impl PollTimeout {
    pub(super) const BLOCK: Self = Self(-1);
    pub(super) const MAX: Self = Self(i32::MAX);

    const fn as_millis(self) -> i32 {
        self.0
    }
}

impl TryFrom<Duration> for PollTimeout {
    type Error = ();

    fn try_from(value: Duration) -> std::result::Result<Self, Self::Error> {
        let millis = value.as_millis();
        let bounded = i32::try_from(millis).map_err(|_| ())?;
        Ok(Self(bounded))
    }
}

pub(super) struct SignalFd {
    fd: OwnedFd,
}

impl SignalFd {
    fn with_flags(block: &SigSet, flags: libc::c_int) -> Result<Self> {
        // SAFETY: signalfd receives a valid sigset pointer and returns a new owned fd on success.
        let fd = unsafe { libc::signalfd(-1, block.as_ptr(), flags) };
        if fd == -1 {
            Err(Errno::last())
        } else {
            // SAFETY: fd is freshly returned by signalfd and uniquely owned here.
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            Ok(Self { fd: owned })
        }
    }

    pub(super) fn read_signal(&mut self) -> Result<Option<libc::signalfd_siginfo>> {
        // SAFETY: POD zero-init is fine for signalfd_siginfo before read(2) fills it.
        let mut info = unsafe { zeroed::<libc::signalfd_siginfo>() };
        // SAFETY: buffer is valid for writes of the exact struct size.
        let rc = unsafe {
            libc::read(
                self.fd.as_raw_fd(),
                &mut info as *mut _ as *mut libc::c_void,
                size_of::<libc::signalfd_siginfo>(),
            )
        };
        if rc == -1 {
            let err = Errno::last();
            if err == Errno::EAGAIN {
                Ok(None)
            } else {
                Err(err)
            }
        } else if rc == 0 {
            Ok(None)
        } else if rc as usize == size_of::<libc::signalfd_siginfo>() {
            Ok(Some(info))
        } else {
            Err(Errno::EIO)
        }
    }
}

impl AsFd for SignalFd {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum WaitStatus {
    Exited(Pid, i32),
    Signaled(Pid, Signal, bool),
    Stopped(Pid, Signal),
    Continued(Pid),
    StillAlive,
}

impl WaitStatus {
    fn from_raw(pid: Pid, status: i32) -> Result<Self> {
        if libc::WIFEXITED(status) {
            Ok(Self::Exited(pid, libc::WEXITSTATUS(status)))
        } else if libc::WIFSIGNALED(status) {
            let sig = Signal::try_from(libc::WTERMSIG(status)).map_err(Errno::from_raw)?;
            Ok(Self::Signaled(pid, sig, libc::WCOREDUMP(status)))
        } else if libc::WIFSTOPPED(status) {
            let sig = Signal::try_from(libc::WSTOPSIG(status)).map_err(Errno::from_raw)?;
            Ok(Self::Stopped(pid, sig))
        } else if libc::WIFCONTINUED(status) {
            Ok(Self::Continued(pid))
        } else {
            Err(Errno::EINVAL)
        }
    }
}

// Single boundary for Unix process, signal, and poll primitives used by tino.
// Future syscall migration should happen here instead of being scattered across
// child/signal/supervision modules.

pub(super) unsafe fn fork_process() -> Result<ForkResult> {
    // SAFETY: caller is responsible for fork-safety constraints.
    match unsafe { libc::fork() } {
        -1 => Err(Errno::last()),
        0 => Ok(ForkResult::Child),
        pid => Ok(ForkResult::Parent {
            child: Pid::from_raw(pid),
        }),
    }
}

pub(super) fn exec_program(program: &CString, argv: &[CString]) -> Result<Infallible> {
    let mut argv_ptrs = argv.iter().map(|arg| arg.as_ptr()).collect::<Vec<_>>();
    argv_ptrs.push(std::ptr::null());
    // SAFETY: program and argv are valid NUL-terminated strings; argv_ptrs is
    // terminated with a trailing null pointer as required by execvp(3).
    let rc = unsafe { libc::execvp(program.as_ptr(), argv_ptrs.as_ptr()) };
    debug_assert_eq!(rc, -1, "execvp only returns on error");
    Err(Errno::last())
}

pub(super) fn set_process_group(pid: Pid, pgid: Pid) -> Result<()> {
    // SAFETY: arguments are plain process identifiers forwarded directly to libc.
    errno_unit(unsafe { libc::setpgid(pid.as_raw(), pgid.as_raw()) })
}

pub(super) fn process_group_of(pid: Pid) -> Result<Pid> {
    // SAFETY: argument is a valid process identifier for getpgid(2).
    errno_pid(unsafe { libc::getpgid(pid.as_raw()) })
}

pub(super) fn waitpid_any_nohang() -> Result<WaitStatus> {
    let mut status = 0;
    // SAFETY: we pass a valid mutable pointer and request the standard WNOHANG behavior.
    let rc = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    match rc {
        0 => Ok(WaitStatus::StillAlive),
        -1 => Err(Errno::last()),
        pid => WaitStatus::from_raw(Pid::from_raw(pid), status),
    }
}

pub(super) fn poll_fds(fds: &mut [PollFd], timeout: PollTimeout) -> Result<()> {
    let mut raw_fds = fds
        .iter()
        .map(|fd| libc::pollfd {
            fd: fd.fd,
            events: fd.events.0,
            revents: 0,
        })
        .collect::<Vec<_>>();
    // SAFETY: raw_fds points to a valid contiguous pollfd array for the duration of the call.
    let rc = unsafe { libc::poll(raw_fds.as_mut_ptr(), raw_fds.len() as libc::nfds_t, timeout.as_millis()) };
    if rc == -1 {
        return Err(Errno::last());
    }
    for (fd, raw) in fds.iter_mut().zip(raw_fds) {
        fd.revents = if raw.revents == 0 {
            None
        } else {
            Some(PollFlags(raw.revents))
        };
    }
    Ok(())
}

pub(super) fn new_signal_fd(block: &SigSet) -> Result<SignalFd> {
    SignalFd::with_flags(block, libc::SFD_NONBLOCK | libc::SFD_CLOEXEC)
}

pub(super) fn send_process_signal(pid: Pid, sig: Signal) -> Result<()> {
    // SAFETY: arguments are forwarded directly to kill(2).
    errno_unit(unsafe { libc::kill(pid.as_raw(), sig as i32) })
}

pub(super) fn send_process_group_signal(pgid: Pid, sig: Signal) -> Result<()> {
    // SAFETY: negative pid targets the process group per kill(2).
    errno_unit(unsafe { libc::kill(-pgid.as_raw(), sig as i32) })
}

fn errno_unit(rc: libc::c_int) -> Result<()> {
    if rc == -1 {
        Err(Errno::last())
    } else {
        Ok(())
    }
}

fn errno_pid(rc: libc::pid_t) -> Result<Pid> {
    if rc == -1 {
        Err(Errno::last())
    } else {
        Ok(Pid::from_raw(rc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_timeout_block_waits_forever() {
        assert_eq!(PollTimeout::BLOCK.as_millis(), -1);
    }

    #[test]
    fn poll_timeout_from_duration_uses_millis() {
        assert_eq!(
            PollTimeout::try_from(Duration::from_millis(25))
                .unwrap()
                .as_millis(),
            25
        );
    }

    #[test]
    fn poll_flags_detect_intersections() {
        assert!(PollFlags::POLLIN.contains(PollFlags::POLLIN));
        assert!(!PollFlags::POLLIN.intersects(PollFlags::POLLERR));
        assert!(PollFlags::POLLERR.intersects(PollFlags::POLLERR));
    }
}
