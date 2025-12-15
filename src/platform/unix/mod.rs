use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::{
        signal::{SIGCHLD, SIGINT, SIGKILL, SIGQUIT, SIGTERM, Signal},
        signalfd::SignalFd,
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::Pid,
};
use std::{
    collections::HashSet,
    os::fd::AsFd,
    thread,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

mod child;
mod signals;

use child::{configure_prctl, manage_process_group, prepare_command, spawn_child, start_session};
use signals::{send_signal, setup_signal_delivery};

pub(super) fn run_impl(cli: Cli, expect_zero: HashSet<u8>) -> Result<i32> {
    configure_prctl(&cli)?;
    let (block, mut signal_fd) = setup_signal_delivery()?;
    start_session()?;

    let (cmd_c, argv_c) =
        prepare_command(&cli.cmd).with_context(|| format!("prepare command {:?}", cli.cmd))?;
    let child_pid = spawn_child(block, &cmd_c, &argv_c)
        .with_context(|| format!("spawn child {:?}", cli.cmd))?;
    let use_pgroup = manage_process_group(cli.pgroup_kill, child_pid);

    supervise_child(&cli, &expect_zero, child_pid, use_pgroup, &mut signal_fd)
}

fn supervise_child(
    cli: &Cli,
    expect_zero: &HashSet<u8>,
    child_pid: Pid,
    use_pgroup: bool,
    signal_fd: &mut SignalFd,
) -> Result<i32> {
    let mut main_exit: Option<i32> = None;
    let mut shutdown_deadline: Option<Instant> = None;
    let mut sigkill_sent = false;
    let mut fds = [PollFd::new(signal_fd.as_fd(), PollFlags::POLLIN)];

    loop {
        let poll_timeout = match (shutdown_deadline, sigkill_sent, main_exit.is_some()) {
            (Some(deadline), false, false) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX)
            }
            _ => PollTimeout::NONE,
        };
        match poll(&mut fds, poll_timeout) {
            Ok(_) => {}
            Err(err) => {
                if err == Errno::EINTR {
                    continue;
                }
                return Err(err).context("poll");
            }
        }
        let ready = fds[0]
            .revents()
            .unwrap_or_else(PollFlags::empty)
            .contains(PollFlags::POLLIN);
        if ready {
            while let Some(info) = signal_fd.read_signal()? {
                let sig = match Signal::try_from(info.ssi_signo as i32) {
                    Ok(sig) => sig,
                    Err(_) => {
                        warn!("received unexpected signal {}", info.ssi_signo);
                        continue;
                    }
                };
                if sig == SIGCHLD {
                    handle_sigchld(cli, child_pid, &mut main_exit)?;
                } else {
                    send_signal(use_pgroup, child_pid, sig);
                    if cli.pgroup_kill
                        && is_termination_signal(sig)
                        && main_exit.is_none()
                        && !sigkill_sent
                    {
                        let now = Instant::now();
                        shutdown_deadline = Some(match shutdown_deadline {
                            None => now + Duration::from_millis(cli.grace_ms),
                            Some(_) => now,
                        });
                    }
                }
            }
        }
        if let Some(deadline) = shutdown_deadline
            && !sigkill_sent
            && main_exit.is_none()
            && Instant::now() >= deadline
        {
            info!("grace period expired; sending SIGKILL");
            send_signal(use_pgroup, child_pid, SIGKILL);
            sigkill_sent = true;
        }
        if main_exit.is_some() {
            break;
        }
    }

    let final_exit = compute_exit_code(main_exit, expect_zero);

    if use_pgroup {
        info!("sending SIGTERM to PGID");
        send_signal(true, child_pid, SIGTERM);
        if !wait_for_children(cli.grace_ms, cli.warn_on_reap)? {
            info!("still alive after {} ms; sending SIGKILL", cli.grace_ms);
            send_signal(true, child_pid, SIGKILL);
            let fully_reaped = wait_for_children(cli.grace_ms, cli.warn_on_reap)?;
            if !fully_reaped {
                warn!(
                    "child processes still alive after SIGKILL wait of {} ms",
                    cli.grace_ms
                );
            }
        }
    } else {
        let _ = wait_for_children(cli.grace_ms, cli.warn_on_reap)?;
    }

    info!("exiting with {}", final_exit);
    Ok(final_exit)
}

fn is_termination_signal(sig: Signal) -> bool {
    sig == SIGTERM || sig == SIGINT || sig == SIGQUIT
}

fn handle_sigchld(cli: &Cli, child_pid: Pid, main_exit: &mut Option<i32>) -> Result<()> {
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, code)) => {
                if pid == child_pid {
                    *main_exit = Some(code);
                } else if cli.warn_on_reap {
                    warn!("reaped secondary PID {}", pid);
                } else {
                    debug!("reaped secondary PID {}", pid);
                }
            }
            Ok(WaitStatus::Signaled(pid, sig, _)) => {
                let code = 128 + sig as i32;
                if pid == child_pid {
                    *main_exit = Some(code);
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
    Ok(())
}

fn compute_exit_code(main_exit: Option<i32>, expect_zero: &HashSet<u8>) -> i32 {
    let code = main_exit.unwrap_or(0);
    if expect_zero.contains(&(code as u8)) {
        0
    } else {
        code
    }
}

fn wait_for_children(timeout_ms: u64, warn_on_reap: bool) -> Result<bool> {
    let start = Instant::now();
    let timeout = Duration::from_millis(timeout_ms);
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => (),
            Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                if warn_on_reap {
                    warn!("reaped secondary PID {}", pid);
                } else {
                    debug!("reaped secondary PID {}", pid);
                }
                continue;
            }
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
    use crate::platform;

    #[test]
    fn license_text_includes_mit_header() {
        assert!(crate::LICENSE_TEXT.contains("MIT License"));
    }

    #[test]
    fn signal_lookup_accepts_variants_with_or_without_prefix() {
        assert_eq!(
            super::signals::signal_by_name("TERM"),
            Some(Signal::SIGTERM)
        );
        assert_eq!(
            super::signals::signal_by_name("SIGTERM"),
            Some(Signal::SIGTERM)
        );
    }

    #[test]
    fn signal_lookup_rejects_unknown_signal() {
        assert!(super::signals::signal_by_name("NOPE").is_none());
    }

    #[test]
    fn init_logging_is_idempotent() {
        platform::init_logging(0);
        platform::init_logging(1);
    }

    #[test]
    fn wait_for_children_without_children_succeeds() {
        assert!(wait_for_children(0, false).unwrap());
    }

    #[test]
    fn compute_exit_code_remaps_expected_values() {
        let mut expect_zero = HashSet::new();
        expect_zero.insert(3);
        assert_eq!(compute_exit_code(Some(3), &expect_zero), 0);
        assert_eq!(compute_exit_code(Some(5), &expect_zero), 5);
        assert_eq!(compute_exit_code(None, &expect_zero), 0);
    }
}
