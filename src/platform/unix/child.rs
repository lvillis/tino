use crate::cli::Cli;
use anyhow::{Context, Result, anyhow, bail};
use libc::{_exit, PR_SET_CHILD_SUBREAPER, PR_SET_PDEATHSIG};
use nix::{
    errno::Errno,
    sys::signal::SigSet,
    unistd::{ForkResult, Pid, execvp, fork, getpgid, setpgid},
};
use std::{env, ffi::CString};
use tracing::warn;

use super::landlock;
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

const MAX_ENV_EXPANSION_DEPTH: usize = 32;

pub(super) fn resolve_command_args(cmd: &[String], expand_env: bool) -> Result<Vec<String>> {
    if expand_env {
        expand_command_args(cmd)
    } else {
        Ok(cmd.to_vec())
    }
}

pub(super) fn prepare_command(cmd: &[String], expand_env: bool) -> Result<(CString, Vec<CString>)> {
    let args = resolve_command_args(cmd, expand_env)?;
    if args.is_empty() {
        bail!("missing CMD (use --help)");
    }

    let program = CString::new(args[0].as_str())
        .map_err(|_| anyhow!("command argument contains embedded NUL byte"))?;
    let argv = args
        .iter()
        .map(|s| {
            CString::new(s.as_str())
                .map_err(|_| anyhow!("command argument contains embedded NUL byte"))
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok((program, argv))
}

fn expand_command_args(cmd: &[String]) -> Result<Vec<String>> {
    cmd.iter().map(|arg| expand_command_arg(arg)).collect()
}

fn expand_command_arg(arg: &str) -> Result<String> {
    expand_command_arg_with_depth(arg, 0)
        .with_context(|| format!("expand environment references in command argument '{arg}'"))
}

fn expand_command_arg_with_depth(arg: &str, depth: usize) -> Result<String> {
    if depth > MAX_ENV_EXPANSION_DEPTH {
        bail!(
            "environment expansion nesting exceeds {} levels",
            MAX_ENV_EXPANSION_DEPTH
        );
    }

    let mut expanded = String::with_capacity(arg.len());
    let mut idx = 0;
    let bytes = arg.as_bytes();

    while idx < arg.len() {
        let Some(offset) = arg[idx..].find('$') else {
            expanded.push_str(&arg[idx..]);
            break;
        };
        let dollar = idx + offset;
        expanded.push_str(&arg[idx..dollar]);

        if dollar + 1 >= arg.len() {
            expanded.push('$');
            break;
        }

        match bytes[dollar + 1] {
            b'$' => {
                expanded.push('$');
                idx = dollar + 2;
            }
            b'{' => {
                let closing = find_matching_brace(arg, dollar + 2)?;
                let body = &arg[dollar + 2..closing];
                expanded.push_str(&expand_braced_env(body, depth + 1)?);
                idx = closing + 1;
            }
            _ => {
                expanded.push('$');
                idx = dollar + 1;
            }
        }
    }

    Ok(expanded)
}

fn expand_braced_env(body: &str, depth: usize) -> Result<String> {
    if let Some((name, default)) = split_braced_default(body) {
        if !is_valid_env_name(name) {
            bail!("invalid environment variable name '{name}'");
        }
        resolve_env_value(name, Some(default), depth)
    } else if is_valid_env_name(body) {
        resolve_env_value(body, None, depth)
    } else {
        bail!("unsupported braced environment expansion '${{{body}}}'");
    }
}

fn resolve_env_value(name: &str, default: Option<&str>, depth: usize) -> Result<String> {
    match env::var(name) {
        Ok(value) if default.is_none() || !value.is_empty() => Ok(value),
        Ok(_) | Err(env::VarError::NotPresent) => match default {
            Some(fallback) => expand_command_arg_with_depth(fallback, depth),
            None => Ok(String::new()),
        },
        Err(env::VarError::NotUnicode(_)) => {
            bail!("environment variable '{name}' contains non-Unicode data")
        }
    }
}

fn find_matching_brace(arg: &str, mut idx: usize) -> Result<usize> {
    let bytes = arg.as_bytes();
    let mut depth = 1usize;

    while idx < arg.len() {
        if bytes[idx] == b'$' && idx + 1 < arg.len() && bytes[idx + 1] == b'{' {
            depth += 1;
            idx += 2;
            continue;
        }
        if bytes[idx] == b'}' {
            depth -= 1;
            if depth == 0 {
                return Ok(idx);
            }
        }
        idx += 1;
    }

    bail!("missing closing '}}'")
}

fn split_braced_default(body: &str) -> Option<(&str, &str)> {
    let bytes = body.as_bytes();
    let mut idx = 0;
    let mut depth = 0usize;

    while idx < body.len() {
        if bytes[idx] == b'$' && idx + 1 < body.len() && bytes[idx + 1] == b'{' {
            depth += 1;
            idx += 2;
            continue;
        }
        if bytes[idx] == b'}' && depth > 0 {
            depth -= 1;
            idx += 1;
            continue;
        }
        if depth == 0 && bytes[idx] == b':' && idx + 1 < body.len() && bytes[idx + 1] == b'-' {
            return Some((&body[..idx], &body[idx + 2..]));
        }
        idx += 1;
    }

    None
}

fn is_valid_env_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && is_env_name_start(bytes[0])
        && bytes[1..].iter().all(|byte| is_env_name_continue(*byte))
}

fn is_env_name_start(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphabetic()
}

fn is_env_name_continue(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
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
    child_write_u32(errno as u32);
}

fn child_write_u32(mut value: u32) {
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

fn child_write_exec_failure_hint(errno: Errno) {
    match errno {
        Errno::ENOENT => {
            child_write(b": file not found; check the path or PATH lookup");
        }
        Errno::EACCES => {
            child_write(b": permission denied or file is not executable");
        }
        Errno::ENOEXEC => {
            child_write(b": file is not a recognized executable format");
        }
        Errno::ENOTDIR => {
            child_write(b": a path component is not a directory");
        }
        Errno::E2BIG => {
            child_write(b": argument list or environment is too large");
        }
        _ => {}
    }
}

fn report_exec_failure(program: &CString, errno: Errno) -> ! {
    child_write(b"tino: execvp failed for ");
    child_write(program.as_bytes());
    child_write_exec_failure_hint(errno);
    child_write(b" (errno ");
    child_write_errno(errno);
    child_write(b")\n");
    unsafe { _exit(127) }
}

fn claim_foreground_tty() {
    // SAFETY: `STDIN_FILENO` is a valid file descriptor constant and the libc calls are used
    // exactly as documented for best-effort tty foreground management.
    unsafe {
        let pgid = libc::getpgrp();
        let _ = libc::tcsetpgrp(libc::STDIN_FILENO, pgid);
    }
}

pub(super) fn spawn_child(
    child_mask: SigSet,
    landlock_config: Option<landlock::LandlockConfig>,
    cmd_c: &CString,
    argv_c: &[CString],
) -> Result<Pid> {
    // SAFETY: the forked child only performs async-signal-safe operations before exec or exit.
    match unsafe { fork()? } {
        ForkResult::Child => {
            if setpgid(Pid::from_raw(0), Pid::from_raw(0)).is_err() {
                child_write(b"tino: failed to establish child process group\n");
            }
            claim_foreground_tty();
            if child_mask.thread_set_mask().is_err() {
                child_write(b"tino: failed to restore signal mask in child\n");
                unsafe { _exit(1) }
            }
            if let Some(config) = landlock_config.as_ref()
                && let Err(err) = landlock::apply(config)
            {
                report_landlock_failure(config.warn_only, err);
                if !config.warn_only {
                    unsafe { _exit(1) }
                }
            }
            match execvp(cmd_c, argv_c) {
                Ok(_) => unsafe { _exit(127) },
                Err(err) => report_exec_failure(cmd_c, err),
            }
        }
        ForkResult::Parent { child } => Ok(child),
    }
}

fn report_landlock_failure(warn_only: bool, err: landlock::LandlockError<'_>) {
    if warn_only {
        child_write(b"tino: access restriction unavailable; continuing (backend landlock: ");
    } else {
        child_write(b"tino: access restriction failed (backend landlock: ");
    }
    match err {
        landlock::LandlockError::NotSupported => {
            child_write(b"not supported (kernel/LSM)");
        }
        landlock::LandlockError::AbiTooOld {
            feature,
            required_abi,
            current_abi,
        } => {
            child_write(feature.as_bytes());
            child_write(b" requires ABI ");
            child_write_u32(required_abi);
            child_write(b" but kernel reports ABI ");
            child_write_u32(current_abi);
        }
        landlock::LandlockError::QueryAbi(errno) => {
            child_write(b"query ABI errno ");
            child_write_errno(errno);
            child_write_seccomp_hint(errno);
        }
        landlock::LandlockError::CreateRuleset(errno) => {
            child_write(b"create ruleset errno ");
            child_write_errno(errno);
            child_write_seccomp_hint(errno);
        }
        landlock::LandlockError::OpenPath { path, errno } => {
            child_write(b"open ");
            child_write(path.to_bytes());
            child_write(b" errno ");
            child_write_errno(errno);
        }
        landlock::LandlockError::AddRule { path, errno } => {
            child_write(b"add rule ");
            child_write(path.to_bytes());
            child_write(b" errno ");
            child_write_errno(errno);
            child_write_seccomp_hint(errno);
        }
        landlock::LandlockError::AddNetPortRule {
            port,
            action,
            errno,
        } => {
            child_write(b"add ");
            child_write(action.as_bytes());
            child_write(b" rule for port ");
            child_write_u32(u32::from(port));
            child_write(b" errno ");
            child_write_errno(errno);
            child_write_seccomp_hint(errno);
        }
        landlock::LandlockError::SetNoNewPrivs(errno) => {
            child_write(b"PR_SET_NO_NEW_PRIVS errno ");
            child_write_errno(errno);
        }
        landlock::LandlockError::RestrictSelf(errno) => {
            child_write(b"restrict self errno ");
            child_write_errno(errno);
            child_write_seccomp_hint(errno);
        }
    }
    child_write(b")\n");
}

fn child_write_seccomp_hint(errno: Errno) {
    if errno == Errno::EPERM || errno == Errno::EACCES {
        child_write(b"; blocked by seccomp?");
    }
}

pub(super) fn manage_process_group(requested: bool, child_pid: Pid) -> bool {
    if !requested {
        return false;
    }
    match setpgid(child_pid, child_pid) {
        Ok(()) => true,
        Err(Errno::EACCES) => match getpgid(Some(child_pid)) {
            Ok(pgid) if pgid == child_pid => true,
            _ => {
                warn!(
                    "cannot manage process group (disabling --pgroup-kill): {}",
                    Errno::EACCES
                );
                false
            }
        },
        Err(Errno::ESRCH) => false,
        Err(e) => {
            warn!(
                "cannot manage process group (disabling --pgroup-kill): {}",
                e
            );
            false
        }
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
            write_restrict: false,
            write_allow: Vec::new(),
            write_preset: Vec::new(),
            write_warn_only: false,
            write_no_dev: false,
            bind_tcp_allow: Vec::new(),
            connect_tcp_allow: Vec::new(),
            scope_signals: false,
            scope_abstract_unix: false,
            exec_allow: Vec::new(),
            device_ioctl_allow: Vec::new(),
            expand_env: false,
            explain: false,
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

    #[test]
    fn expand_command_arg_supports_defaults_and_escapes() {
        let expanded = expand_command_arg(
            "port=${__TINO_TEST_MISSING_PORT_123456__:-8900},literal=$${HOME},missing=${__TINO_TEST_MISSING_VALUE_123456__}",
        )
        .expect("expand env with defaults and escapes");

        assert_eq!(expanded, "port=8900,literal=${HOME},missing=");
    }

    #[test]
    fn expand_command_arg_supports_nested_defaults() {
        let expanded = expand_command_arg(
            "${__TINO_TEST_MISSING_PRIMARY_123456__:-${__TINO_TEST_MISSING_FALLBACK_123456__:-8900}}",
        )
        .expect("expand nested default");

        assert_eq!(expanded, "8900");
    }

    #[test]
    fn expand_command_arg_rejects_invalid_syntax() {
        let err =
            expand_command_arg("${__TINO_TEST_MISSING_PORT_123456__").expect_err("missing brace");
        let message = format!("{err:#}");
        assert!(
            message.contains("missing closing '}'"),
            "unexpected error: {message}"
        );

        let err = expand_command_arg("${NAME:+value}").expect_err("unsupported operator");
        let message = format!("{err:#}");
        assert!(
            message.contains("unsupported braced environment expansion"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn expand_command_arg_leaves_unbraced_dollar_names_unchanged() {
        let expanded = expand_command_arg("$SERVICE_PORT ${SERVICE_PORT:-8900}")
            .expect("expand env with unbraced name");

        assert_eq!(expanded, "$SERVICE_PORT 8900");
    }
}
