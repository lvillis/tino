use anyhow::{Context, Result};
use nix::{
    errno::Errno,
    sys::{
        signal::{
            SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGSYS, SIGTRAP, SigSet, Signal, kill, killpg,
        },
        signalfd::{SfdFlags, SignalFd},
    },
    unistd::Pid,
};
use tracing::warn;

const SIGNALS_EXCLUDED_FROM_SIGNALFD: &[Signal] =
    &[SIGFPE, SIGILL, SIGSEGV, SIGBUS, SIGABRT, SIGTRAP, SIGSYS];

pub(super) fn setup_signal_delivery() -> Result<(SigSet, SignalFd)> {
    let previous_mask = SigSet::thread_get_mask().context("sigprocmask")?;
    let mut block = SigSet::all();
    for &signal in SIGNALS_EXCLUDED_FROM_SIGNALFD {
        block.remove(signal);
    }
    block.thread_set_mask().context("sigprocmask")?;

    let signal_fd = SignalFd::with_flags(&block, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
        .context("signalfd")?;

    Ok((previous_mask, signal_fd))
}

pub(super) fn signal_by_name(name: &str) -> Option<Signal> {
    crate::signals::signal_from_str(name)
}

pub(super) fn send_signal(pgid: bool, child: Pid, sig: Signal) {
    let res = if pgid {
        killpg(Pid::from_raw(child.as_raw()), sig)
    } else {
        kill(child, sig)
    };
    if let Err(e) = res
        && e != Errno::ESRCH
    {
        warn!("forward {:?} failed: {}", sig, e);
    }
}
