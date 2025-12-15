use crate::cli::Cli;
use anyhow::{Result, anyhow, bail};
use libc::{_exit, PR_SET_CHILD_SUBREAPER, PR_SET_PDEATHSIG};
use nix::{
    errno::Errno,
    sys::signal::SigSet,
    unistd::{ForkResult, Pid, execvp, fork, getpgid, setpgid},
};
use std::ffi::CString;
use tracing::warn;

use super::signals;

#[derive(Default)]
pub(super) struct PrctlOutcome {
    pub subreaper_enabled: bool,
    pub pdeath_set: bool,
}

pub(super) fn configure_prctl(cli: &Cli) -> Result<PrctlOutcome> {
    let mut outcome = PrctlOutcome::default();
    if let Some(sig_name) = &cli.pdeath {
        let sig = signals::signal_by_name(sig_name).ok_or_else(|| {
            anyhow!(
                "invalid signal '{}'; supported values align with `tino --help`",
                sig_name
            )
        })?;
        // SAFETY: `sig` is a valid signal number and `prctl` is called with documented parameters.
        unsafe {
            if libc::prctl(PR_SET_PDEATHSIG, sig as i32) == -1 {
                bail!("prctl P_DEATHSIG: {}", Errno::last());
            }
        }
        outcome.pdeath_set = true;
    }
    if cli.subreaper {
        // SAFETY: enabling the child subreaper flag is safe for the current process.
        unsafe {
            if libc::prctl(PR_SET_CHILD_SUBREAPER, 1) == -1 {
                let err = Errno::last();
                if err == Errno::EPERM {
                    warn!(
                        error = %err,
                        "subreaper capability rejected; continuing without subreaper"
                    );
                } else {
                    bail!("prctl SUBREAPER: {}", err);
                }
            } else {
                outcome.subreaper_enabled = true;
            }
        }
    }
    Ok(outcome)
}

pub(super) fn start_session() -> Result<()> {
    // SAFETY: `setsid` is called on the current process and errors are handled immediately.
    unsafe {
        if libc::setsid() == -1 && Errno::last() != Errno::EPERM {
            bail!("setsid: {}", Errno::last());
        }
    }
    Ok(())
}

pub(super) fn prepare_command(cmd: &[String]) -> Result<(CString, Vec<CString>)> {
    let program = CString::new(cmd[0].as_str())
        .map_err(|_| anyhow!("command argument contains embedded NUL byte"))?;
    let argv = cmd
        .iter()
        .map(|s| {
            CString::new(s.as_str())
                .map_err(|_| anyhow!("command argument contains embedded NUL byte"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((program, argv))
}

fn child_write(bytes: &[u8]) {
    unsafe {
        let _ = libc::write(
            libc::STDERR_FILENO,
            bytes.as_ptr() as *const libc::c_void,
            bytes.len(),
        );
    }
}

fn child_write_errno(errno: Errno) {
    let mut value = errno as i32;
    let mut buf = [0u8; 12];
    let mut idx = buf.len();
    if value == 0 {
        idx -= 1;
        buf[idx] = b'0';
    } else {
        while value > 0 {
            let digit = (value % 10) as u8;
            idx -= 1;
            buf[idx] = b'0' + digit;
            value /= 10;
        }
    }
    child_write(&buf[idx..]);
}

fn report_exec_failure(program: &CString, err: nix::Error) -> ! {
    child_write(b"tino: execvp failed for ");
    child_write(program.as_bytes());
    if let Some(errno) = err.as_errno() {
        child_write(b" (errno ");
        child_write_errno(errno);
        child_write(b")");
    }
    child_write(b"\n");
    unsafe { _exit(127) }
}

pub(super) fn spawn_child(mut block: SigSet, cmd_c: &CString, argv_c: &[CString]) -> Result<Pid> {
    // SAFETY: the forked child only performs async-signal-safe operations before exec or exit.
    match unsafe { fork()? } {
        ForkResult::Child => {
            if setpgid(Pid::from_raw(0), Pid::from_raw(0)).is_err() {
                child_write(b"tino: failed to establish child process group\n");
            }
            if block.thread_unblock().is_err() {
                child_write(b"tino: failed to restore signal mask in child\n");
                unsafe { _exit(1) }
            }
            match execvp(cmd_c, argv_c) {
                Ok(_) => unsafe { _exit(127) },
                Err(err) => report_exec_failure(cmd_c, err),
            }
        }
        ForkResult::Parent { child } => Ok(child),
    }
}

pub(super) fn manage_process_group(requested: bool, child_pid: Pid) -> bool {
    if !requested {
        return false;
    }
    match setpgid(child_pid, child_pid) {
        Ok(()) => true,
        Err(e) => match e.as_errno() {
            Some(Errno::EACCES) => match getpgid(Some(child_pid)) {
                Ok(pgid) if pgid == child_pid => true,
                _ => {
                    warn!(
                        "cannot manage process group (disabling --pgroup-kill): {}",
                        e
                    );
                    false
                }
            },
            Some(Errno::ESRCH) => false,
            _ => {
                warn!(
                    "cannot manage process group (disabling --pgroup-kill): {}",
                    e
                );
                false
            }
        },
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;
    use std::io;

    struct PrctlStateGuard {
        subreaper: libc::c_int,
        pdeath: libc::c_int,
    }

    impl PrctlStateGuard {
        fn capture() -> Self {
            let mut subreaper = 0;
            let mut pdeath = 0;
            // SAFETY: we pass valid pointers to store the current prctl state.
            let ret = unsafe {
                libc::prctl(
                    libc::PR_GET_CHILD_SUBREAPER,
                    &mut subreaper as *mut libc::c_int,
                )
            };
            assert_eq!(
                ret,
                0,
                "PR_GET_CHILD_SUBREAPER failed: {}",
                io::Error::last_os_error()
            );
            // SAFETY: pointer references a valid mutable integer on our stack.
            let ret =
                unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &mut pdeath as *mut libc::c_int) };
            assert_eq!(
                ret,
                0,
                "PR_GET_PDEATHSIG failed: {}",
                io::Error::last_os_error()
            );
            Self { subreaper, pdeath }
        }
    }

    impl Drop for PrctlStateGuard {
        fn drop(&mut self) {
            // SAFETY: we restore the previously captured values; best-effort errors are ignored.
            unsafe {
                libc::prctl(libc::PR_SET_CHILD_SUBREAPER, self.subreaper);
                libc::prctl(libc::PR_SET_PDEATHSIG, self.pdeath);
            }
        }
    }

    fn base_cli() -> Cli {
        Cli {
            subreaper: false,
            pdeath: None,
            verbosity: 0,
            warn_on_reap: false,
            pgroup_kill: false,
            remap_exit: Vec::new(),
            grace_ms: 500,
            license: false,
            subreaper_env: None,
            pgroup_env: None,
            verbosity_env: None,
            cmd: vec!["/bin/true".into()],
        }
    }

    #[test]
    fn configure_prctl_sets_pdeathsig() {
        let guard = PrctlStateGuard::capture();
        let mut cli = base_cli();
        cli.pdeath = Some("SIGUSR1".into());

        let outcome = configure_prctl(&cli).expect("configure prctl with pdeath");
        assert!(
            outcome.pdeath_set,
            "expected pdeath signal to be configured"
        );

        let mut current = guard.pdeath;
        // SAFETY: pointer references a valid mutable integer for prctl output.
        let ret = unsafe { libc::prctl(libc::PR_GET_PDEATHSIG, &mut current as *mut libc::c_int) };
        assert_eq!(
            ret,
            0,
            "PR_GET_PDEATHSIG failed: {}",
            io::Error::last_os_error()
        );
        assert_eq!(current, libc::SIGUSR1);
    }

    #[test]
    fn configure_prctl_handles_subreaper_capability() {
        let guard = PrctlStateGuard::capture();
        let mut cli = base_cli();
        cli.subreaper = true;

        let outcome = configure_prctl(&cli).expect("configure prctl with subreaper flag");

        let mut current = guard.subreaper;
        // SAFETY: pointer references a valid mutable integer for prctl output.
        let ret = unsafe {
            libc::prctl(
                libc::PR_GET_CHILD_SUBREAPER,
                &mut current as *mut libc::c_int,
            )
        };
        assert_eq!(
            ret,
            0,
            "PR_GET_CHILD_SUBREAPER failed: {}",
            io::Error::last_os_error()
        );
        if outcome.subreaper_enabled {
            assert_eq!(current, 1, "subreaper flag expected to be enabled");
        } else {
            assert_eq!(
                current, guard.subreaper,
                "subreaper state should be unchanged when capability is denied"
            );
        }
    }
}
