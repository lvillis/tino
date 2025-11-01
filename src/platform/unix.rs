use crate::{LICENSE_TEXT, cli::Cli};
use anyhow::{Context, Result, anyhow, bail};
use libc::{PR_SET_CHILD_SUBREAPER, PR_SET_PDEATHSIG};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::{
        signal::{SIGCHLD, SIGKILL, SIGTERM, SigSet, Signal, kill, killpg},
        signalfd::{SfdFlags, SigSet as NixSigSet, SignalFd},
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::{ForkResult, Pid, execvp, fork, setpgid},
};
use once_cell::sync::{Lazy, OnceCell};
use std::{
    collections::HashSet,
    ffi::CString,
    os::fd::AsFd,
    process, thread,
    time::{Duration, Instant},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{filter::EnvFilter, fmt};

static FWD_SIGS: Lazy<Vec<Signal>> = Lazy::new(|| {
    vec![
        Signal::SIGHUP,
        Signal::SIGINT,
        Signal::SIGQUIT,
        Signal::SIGTERM,
        Signal::SIGUSR1,
        Signal::SIGUSR2,
        Signal::SIGWINCH,
        Signal::SIGCONT,
        Signal::SIGTTIN,
        Signal::SIGTTOU,
    ]
});

static LOGGER: OnceCell<()> = OnceCell::new();

pub fn run(mut cli: Cli) -> Result<()> {
    if !cli.subreaper && std::env::var_os("TINI_SUBREAPER").is_some() {
        cli.subreaper = true;
    }
    if !cli.pgroup_kill && std::env::var_os("TINI_KILL_PROCESS_GROUP").is_some() {
        cli.pgroup_kill = true;
    }
    if cli.verbosity == 0 {
        if let Ok(v) = std::env::var("TINI_VERBOSITY") {
            cli.verbosity = v.parse::<u8>().unwrap_or(0).min(3);
        }
    }
    init_logging(cli.verbosity);

    if cli.license {
        print!("{LICENSE_TEXT}");
        return Ok(());
    }
    if cli.cmd.is_empty() {
        error!("missing CMD (use --help)");
        bail!("missing CMD (use --help)");
    }

    let expect_zero: HashSet<u8> = cli.remap_exit.iter().copied().collect();

    if let Some(sig_name) = &cli.pdeath {
        let sig = signal_by_name(sig_name).ok_or_else(|| anyhow!("invalid signal"))?;
        unsafe {
            if libc::prctl(PR_SET_PDEATHSIG, sig as i32) == -1 {
                bail!("prctl P_DEATHSIG: {}", Errno::last());
            }
        }
    }
    if cli.subreaper {
        unsafe {
            if libc::prctl(PR_SET_CHILD_SUBREAPER, 1) == -1 {
                bail!("prctl SUBREAPER: {}", Errno::last());
            }
        }
    }

    let mut block = SigSet::empty();
    block.add(SIGCHLD);
    for &s in &*FWD_SIGS {
        block.add(s);
    }
    block.thread_block().context("sigprocmask")?;
    let mut sfd_set = NixSigSet::empty();
    for &s in FWD_SIGS.iter().chain(std::iter::once(&SIGCHLD)) {
        sfd_set.add(s);
    }
    let mut sfd = SignalFd::with_flags(&sfd_set, SfdFlags::SFD_NONBLOCK | SfdFlags::SFD_CLOEXEC)
        .context("signalfd")?;

    unsafe {
        if libc::setsid() == -1 && Errno::last() != Errno::EPERM {
            bail!("setsid: {}", Errno::last());
        }
    }

    let cmd_c = CString::new(cli.cmd[0].as_str())
        .map_err(|_| anyhow!("command argument contains embedded NUL byte"))?;
    let argv_c = cli
        .cmd
        .iter()
        .map(|s| {
            CString::new(s.as_str())
                .map_err(|_| anyhow!("command argument contains embedded NUL byte"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let child_pid = match unsafe { fork()? } {
        ForkResult::Child => {
            if let Err(e) = setpgid(Pid::from_raw(0), Pid::from_raw(0)) {
                warn!("failed to establish child process group: {}", e);
            }
            if let Err(e) = block.thread_unblock() {
                error!("failed to restore signal mask in child: {}", e);
                process::exit(1);
            }
            execvp(&cmd_c, &argv_c)?;
            unreachable!();
        }
        ForkResult::Parent { child } => child,
    };
    info!("spawned child PID {}", child_pid);

    let mut use_pgroup = cli.pgroup_kill;
    if use_pgroup {
        match setpgid(child_pid, child_pid) {
            Ok(()) => (),
            Err(e) => {
                warn!(
                    "cannot manage process group (disabling --pgroup-kill): {}",
                    e
                );
                use_pgroup = false;
            }
        }
    }

    let mut main_exit: Option<i32> = None;
    let mut fds = [PollFd::new(sfd.as_fd(), PollFlags::POLLIN)]; // Poll signalfd readiness

    loop {
        poll(&mut fds, PollTimeout::NONE).context("poll")?;
        if fds[0]
            .revents()
            .unwrap_or(PollFlags::empty())
            .contains(PollFlags::POLLIN)
        {
            while let Some(info) = sfd.read_signal()? {
                let sig = match Signal::try_from(info.ssi_signo as i32) {
                    Ok(sig) => sig,
                    Err(_) => {
                        warn!("received unexpected signal {}", info.ssi_signo);
                        continue;
                    }
                };
                if sig == SIGCHLD {
                    loop {
                        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
                            Ok(WaitStatus::Exited(pid, c)) => {
                                if pid == child_pid {
                                    main_exit = Some(c);
                                } else if cli.warn_on_reap {
                                    warn!("reaped secondary PID {}", pid);
                                } else {
                                    debug!("reaped secondary PID {}", pid);
                                }
                            }
                            Ok(WaitStatus::Signaled(pid, s, _)) => {
                                let c = 128 + s as i32;
                                if pid == child_pid {
                                    main_exit = Some(c);
                                } else if cli.warn_on_reap {
                                    warn!("reaped secondary PID {}", pid);
                                } else {
                                    debug!("reaped secondary PID {}", pid);
                                }
                            }
                            Ok(WaitStatus::Stopped(pid, sig)) => {
                                if cli.warn_on_reap {
                                    warn!("child PID {} stopped by signal {:?}", pid, sig);
                                } else {
                                    debug!("child PID {} stopped by signal {:?}", pid, sig);
                                }
                                break;
                            }
                            Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Continued(_)) => break,
                            Err(Errno::ECHILD) => break,
                            Err(Errno::EINTR) => continue,
                            Ok(status) => {
                                debug!("waitpid yielded unhandled state: {:?}", status);
                                break;
                            }
                            Err(e) => bail!("waitpid: {e}"),
                        }
                    }
                } else {
                    send_signal(use_pgroup, child_pid, sig);
                }
            }
        }
        if main_exit.is_some() && wait_for_children(0)? {
            break;
        }
    }

    let code = main_exit.unwrap_or(0);
    let final_exit = if expect_zero.contains(&(code as u8)) {
        0
    } else {
        code
    };

    if use_pgroup {
        info!("sending SIGTERM to PGID");
        send_signal(true, child_pid, SIGTERM);
        if !wait_for_children(cli.grace_ms)? {
            info!("still alive after {} ms; sending SIGKILL", cli.grace_ms);
            send_signal(true, child_pid, SIGKILL);
            wait_for_children(cli.grace_ms)?;
        }
    }

    info!("exiting with {}", final_exit);
    process::exit(final_exit);
}

fn signal_by_name(name: &str) -> Option<Signal> {
    let up = if name.to_ascii_uppercase().starts_with("SIG") {
        name.to_ascii_uppercase()
    } else {
        format!("SIG{}", name.to_ascii_uppercase())
    };
    [
        ("SIGHUP", Signal::SIGHUP),
        ("SIGINT", Signal::SIGINT),
        ("SIGQUIT", Signal::SIGQUIT),
        ("SIGILL", Signal::SIGILL),
        ("SIGTRAP", Signal::SIGTRAP),
        ("SIGABRT", Signal::SIGABRT),
        ("SIGBUS", Signal::SIGBUS),
        ("SIGFPE", Signal::SIGFPE),
        ("SIGKILL", Signal::SIGKILL),
        ("SIGUSR1", Signal::SIGUSR1),
        ("SIGSEGV", Signal::SIGSEGV),
        ("SIGUSR2", Signal::SIGUSR2),
        ("SIGPIPE", Signal::SIGPIPE),
        ("SIGALRM", Signal::SIGALRM),
        ("SIGTERM", Signal::SIGTERM),
        ("SIGCONT", Signal::SIGCONT),
        ("SIGWINCH", Signal::SIGWINCH),
        ("SIGTTIN", Signal::SIGTTIN),
        ("SIGTTOU", Signal::SIGTTOU),
    ]
    .into_iter()
    .find(|(n, _)| *n == up)
    .map(|(_, s)| s)
}

fn init_logging(v: u8) {
    let lvl = match v {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    if LOGGER.get().is_some() {
        return;
    }
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(lvl));
    if LOGGER.set(()).is_ok() {
        if let Err(e) = fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_target(false)
            .without_time()
            .try_init()
        {
            eprintln!("failed to initialize logging: {e}");
        }
    }
}

fn send_signal(pgid: bool, child: Pid, sig: Signal) {
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

fn wait_for_children(timeout_ms: u64) -> Result<bool> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => (),
            Ok(_) => continue,
            Err(Errno::ECHILD) => return Ok(true),
            Err(Errno::EINTR) => continue,
            Err(e) => bail!("waitpid: {e}"),
        }
        if timeout_ms == 0 {
            return Ok(false);
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            return Ok(false);
        }
        let remaining = timeout - elapsed;
        thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn license_text_includes_mit_header() {
        assert!(LICENSE_TEXT.contains("MIT License"));
    }

    #[test]
    fn signal_lookup_accepts_variants_with_or_without_prefix() {
        assert_eq!(signal_by_name("TERM"), Some(Signal::SIGTERM));
        assert_eq!(signal_by_name("SIGTERM"), Some(Signal::SIGTERM));
    }

    #[test]
    fn signal_lookup_rejects_unknown_signal() {
        assert!(signal_by_name("NOPE").is_none());
    }

    #[test]
    fn init_logging_is_idempotent() {
        init_logging(0);
        init_logging(1);
    }

    #[test]
    fn wait_for_children_without_children_succeeds() {
        assert!(wait_for_children(0).unwrap());
    }
}
