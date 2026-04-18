use crate::{Context, Result, logging};
use super::sys::{
    Errno, Pid, SIGABRT, SIGBUS, SIGFPE, SIGILL, SIGSEGV, SIGSYS, SIGTRAP, SigSet, Signal,
    SignalFd, new_signal_fd, send_process_group_signal, send_process_signal,
};

const SIGNALS_EXCLUDED_FROM_SIGNALFD: &[Signal] =
    &[SIGFPE, SIGILL, SIGSEGV, SIGBUS, SIGABRT, SIGTRAP, SIGSYS];

pub(super) fn setup_signal_delivery() -> Result<(SigSet, SignalFd)> {
    let previous_mask = SigSet::thread_get_mask().context("sigprocmask")?;
    let mut block = SigSet::all();
    for &signal in SIGNALS_EXCLUDED_FROM_SIGNALFD {
        block.remove(signal);
    }
    block.thread_set_mask().context("sigprocmask")?;

    let signal_fd = new_signal_fd(&block).context("signalfd")?;

    Ok((previous_mask, signal_fd))
}

pub(super) fn signal_by_name(name: &str) -> Option<Signal> {
    crate::signals::signal_from_str(name)
}

pub(super) fn send_signal(pgid: bool, child: Pid, sig: Signal) {
    let res = if pgid {
        send_process_group_signal(child, sig)
    } else {
        send_process_signal(child, sig)
    };
    if let Err(e) = res
        && e != Errno::ESRCH
    {
        logging::warn(format_args!("forward {:?} failed: {}", sig, e));
    }
}
