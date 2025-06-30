//! tino — Tiny Init, No Overhead (production-grade)
//
//! Build (static):  `cargo build --release --target x86_64-unknown-linux-musl`

#![deny(unsafe_op_in_unsafe_fn)]

use anyhow::{Context, Result, bail};
use clap::Parser;
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
use once_cell::sync::Lazy;
use std::{
    collections::HashSet,
    ffi::CString,
    os::fd::AsFd,
    process,
    time::{Duration, Instant},
};
use tracing::{debug, error, info, warn};
use tracing_subscriber::{filter::EnvFilter, fmt};

/// ---------------- CLI -------------------------------------------------
#[derive(Parser, Debug)]
#[command(
    name = "tino",
    version = "1.0.0",
    author = "Your Team",
    about = "Tiny PID-1 in Rust"
)]
struct Cli {
    #[arg(short = 's', long)]
    subreaper: bool,
    #[arg(short = 'p')]
    pdeath: Option<String>,
    #[arg(short = 'v', action = clap::ArgAction::Count)]
    verbosity: u8,
    #[arg(short = 'w')]
    warn_on_reap: bool,
    #[arg(short = 'g')]
    pgroup_kill: bool,
    #[arg(short = 'e', value_parser = clap::value_parser!(u8).range(0..=255))]
    remap_exit: Vec<u8>,
    #[arg(short = 't', long, default_value_t = 500)]
    grace_ms: u64,
    #[arg(short = 'l')]
    license: bool,
    #[arg(value_name = "CMD", trailing_var_arg = true)]
    cmd: Vec<String>,
}

/// ---------------- constants -------------------------------------------
const LICENSE_TEXT: &str =
    "tino — MIT License.  Based on krallin/tini (see original project for full text).\n";

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

/// ---------------- helpers ---------------------------------------------
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
    let f = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(lvl));
    fmt::Subscriber::builder()
        .with_env_filter(f)
        .with_target(false)
        .without_time()
        .init();
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
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => (),
            Ok(_) => continue,
            Err(Errno::ECHILD) => return Ok(true),
            Err(Errno::EINTR) => continue,
            Err(e) => bail!("waitpid: {e}"),
        }
        if start.elapsed() >= Duration::from_millis(timeout_ms) {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// ---------------- main ------------------------------------------------
fn main() -> Result<()> {
    let mut cli = Cli::parse();

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
        process::exit(0);
    }
    if cli.cmd.is_empty() {
        error!("missing CMD (use --help)");
        process::exit(1);
    }

    let expect_zero: HashSet<u8> = cli.remap_exit.iter().copied().collect();

    if let Some(sig_name) = &cli.pdeath {
        let sig = signal_by_name(sig_name).ok_or_else(|| anyhow::anyhow!("invalid signal"))?;
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
    let mut sfd = SignalFd::with_flags(&sfd_set, SfdFlags::SFD_NONBLOCK).context("signalfd")?;

    unsafe {
        if libc::setsid() == -1 && Errno::last() != Errno::EPERM {
            bail!("setsid: {}", Errno::last());
        }
    }

    let cmd_c = CString::new(cli.cmd[0].clone())?;
    let argv_c: Vec<CString> = cli
        .cmd
        .iter()
        .map(|s| CString::new(s.as_str()).unwrap())
        .collect();

    let child_pid = match unsafe { fork()? } {
        ForkResult::Child => {
            unsafe {
                setpgid(Pid::from_raw(0), Pid::from_raw(0)).ok();
                block.thread_unblock().ok();
            }
            execvp(&cmd_c, &argv_c)?;
            unreachable!();
        }
        ForkResult::Parent { child } => child,
    };
    info!("spawned child PID {}", child_pid);

    let mut main_exit: Option<i32> = None;
    let mut fds = [PollFd::new(sfd.as_fd(), PollFlags::POLLIN)]; // ← 关键：直接 BorrowedFd

    loop {
        poll(&mut fds, PollTimeout::NONE).context("poll")?;
        if fds[0]
            .revents()
            .unwrap_or(PollFlags::empty())
            .contains(PollFlags::POLLIN)
        {
            while let Some(info) = sfd.read_signal()? {
                let sig = Signal::try_from(info.ssi_signo as i32).unwrap();
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
                            Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Continued(_)) => break,
                            Err(Errno::ECHILD) => break,
                            Err(Errno::EINTR) => continue,
                            Err(e) => bail!("waitpid: {e}"),
                            _ => (),
                        }
                    }
                } else {
                    send_signal(cli.pgroup_kill, child_pid, sig);
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

    if cli.pgroup_kill {
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
