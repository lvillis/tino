use crate::cli::Cli;
use anyhow::{Context, Result, bail};
use nix::{
    errno::Errno,
    poll::{PollFd, PollFlags, PollTimeout, poll},
    sys::{
        signal::{SIGCHLD, SIGINT, SIGKILL, SIGQUIT, SIGTERM, SIGTTIN, SIGTTOU, Signal},
        signalfd::SignalFd,
        wait::{WaitPidFlag, WaitStatus, waitpid},
    },
    unistd::Pid,
};
use std::{
    collections::BTreeSet,
    ffi::CString,
    os::fd::AsFd,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};
use tracing::{debug, info, warn};

mod child;
mod landlock;
mod signals;

use child::{configure_prctl, manage_process_group, prepare_command, spawn_child};
use landlock::LandlockConfig;
use signals::{send_signal, setup_signal_delivery};

type ExitCodeRemap = super::ExitCodeRemap;

pub(super) fn run_impl(cli: Cli, expect_zero: ExitCodeRemap) -> Result<i32> {
    configure_prctl(&cli)?;
    let (child_mask, mut signal_fd) = setup_signal_delivery()?;
    let landlock_config = build_landlock_config(&cli)?;
    if let Some(config) = &landlock_config {
        debug!(
            warn_only = config.warn_only,
            no_dev = config.no_dev,
            writable_dirs = config.writable_dirs.len(),
            "landlock enabled"
        );
        for path in &config.writable_dirs {
            debug!(path = %path.as_c_str().to_string_lossy(), "landlock writable dir");
        }
    }

    let (cmd_c, argv_c) = prepare_command(&cli.cmd, cli.expand_env)
        .with_context(|| format!("prepare command {:?}", cli.cmd))?;
    let child_pid = spawn_child(child_mask, landlock_config, &cmd_c, &argv_c)
        .with_context(|| format!("spawn child {:?}", cli.cmd))?;
    let use_pgroup = manage_process_group(cli.pgroup_kill, child_pid);

    supervise_child(&cli, &expect_zero, child_pid, use_pgroup, &mut signal_fd)
}

fn build_landlock_config(cli: &Cli) -> Result<Option<LandlockConfig>> {
    let enabled = cli.landlock
        || !cli.landlock_writable.is_empty()
        || cli.landlock_profile.is_some()
        || cli.landlock_warn_only
        || cli.landlock_no_dev;
    if !enabled {
        return Ok(None);
    }

    let mut unique = BTreeSet::new();

    if let Some(profile) = cli.landlock_profile.as_deref() {
        let content = std::fs::read_to_string(profile)
            .with_context(|| format!("read landlock profile file '{profile}'"))?;
        for (idx, line) in content.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            insert_landlock_writable_dir(&mut unique, trimmed, Some((profile, idx + 1)))?;
        }
    }

    for raw in &cli.landlock_writable {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("--landlock-writable PATH cannot be empty");
        }
        insert_landlock_writable_dir(&mut unique, trimmed, None)?;
    }

    let writable_dirs = unique
        .into_iter()
        .map(|path| CString::new(path).context("landlock writable path contains NUL byte"))
        .collect::<Result<Vec<_>>>()?;

    Ok(Some(LandlockConfig {
        warn_only: cli.landlock_warn_only,
        no_dev: cli.landlock_no_dev,
        writable_dirs,
    }))
}

fn insert_landlock_writable_dir(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
) -> Result<()> {
    let path = PathBuf::from(raw);
    let canonical = std::fs::canonicalize(&path).with_context(|| match source {
        Some((file, line)) => {
            format!("canonicalize landlock writable dir '{raw}' (from {file}:{line})")
        }
        None => format!("canonicalize landlock writable dir '{raw}'"),
    })?;
    let metadata = std::fs::metadata(&canonical)
        .with_context(|| format!("inspect landlock writable dir '{}'", canonical.display()))?;
    if !metadata.is_dir() {
        bail!(
            "landlock writable path '{}' is not a directory",
            canonical.display()
        );
    }
    unique.insert(canonical.as_os_str().as_bytes().to_vec());
    Ok(())
}

fn supervise_child(
    cli: &Cli,
    expect_zero: &ExitCodeRemap,
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
                } else if sig == SIGTTIN || sig == SIGTTOU {
                    debug!("ignoring {:?}", sig);
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

fn compute_exit_code(main_exit: Option<i32>, expect_zero: &ExitCodeRemap) -> i32 {
    let code = main_exit.unwrap_or(0);
    if u8::try_from(code).is_ok_and(|candidate| expect_zero[candidate as usize]) {
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
        let mut expect_zero = [false; 256];
        expect_zero[3] = true;
        assert_eq!(compute_exit_code(Some(3), &expect_zero), 0);
        assert_eq!(compute_exit_code(Some(5), &expect_zero), 5);
        assert_eq!(compute_exit_code(None, &expect_zero), 0);
    }
}
