use anyhow::{Context, Result};
use nix::{
    errno::Errno,
    sys::{
        signal::{SIGCHLD, SigSet, Signal, kill, killpg},
        signalfd::{SfdFlags, SigSet as NixSigSet, SignalFd},
    },
    unistd::Pid,
};
use tracing::warn;

pub(super) fn setup_signal_delivery() -> Result<(SigSet, SignalFd)> {
    let mut block = SigSet::empty();
    block.add(SIGCHLD);
    for &s in crate::signals::FORWARDED_SIGNALS.iter() {
        block.add(s);
    }
    block.thread_block().context("sigprocmask")?;

    let mut sfd_set = NixSigSet::empty();
    for &s in crate::signals::FORWARDED_SIGNALS
        .iter()
        .chain(std::iter::once(&SIGCHLD))
    {
        sfd_set.add(s);
    }
    let signal_fd = SignalFd::with_flags(&sfd_set, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
        .context("signalfd")?;

    Ok((block, signal_fd))
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
    if let Err(e) = res {
        if e != Errno::ESRCH {
            warn!("forward {:?} failed: {}", sig, e);
        }
    }
}
