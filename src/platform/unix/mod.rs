use crate::{
    Context, Error, Result, bail,
    cli::{Cli, WritePreset},
    logging,
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

mod child;
mod landlock;
mod signals;
pub(crate) mod sys;

use child::{
    configure_prctl, manage_process_group, prepare_command, resolve_command_args, spawn_child,
};
use landlock::LandlockConfig;
use signals::{send_signal, setup_signal_delivery};
use sys::{
    Errno, Pid, PollFd, PollFlags, PollTimeout, SIGCHLD, SIGINT, SIGKILL, SIGQUIT, SIGTERM,
    SIGTTIN, SIGTTOU, Signal, SignalFd, WaitStatus, poll_fds, waitpid_any_nohang,
};

type ExitCodeRemap = super::ExitCodeRemap;

pub(super) struct LandlockExplain {
    pub write_requested: bool,
    pub warn_only: bool,
    pub no_dev: bool,
    pub preset_names: Vec<String>,
    pub writable_dirs: Vec<String>,
    pub bind_tcp_ports: Vec<u16>,
    pub connect_tcp_ports: Vec<u16>,
    pub scope_signals: bool,
    pub scope_abstract_unix: bool,
    pub exec_allow_paths: Vec<String>,
    pub device_ioctl_allow_paths: Vec<String>,
}

pub(super) fn run_impl(cli: Cli, expect_zero: ExitCodeRemap) -> Result<i32> {
    configure_prctl(&cli)?;
    let (child_mask, mut signal_fd) = setup_signal_delivery()?;
    let landlock_config = build_landlock_config(&cli)?;
    if let Some(config) = &landlock_config {
        logging::debug(format_args!(
            "landlock restriction enabled: warn_only={}, no_dev={}, writable_dirs={}, bind_tcp_ports={}, connect_tcp_ports={}, scope_signals={}, scope_abstract_unix={}, exec_allow_paths={}, device_ioctl_allow_paths={}",
            config.warn_only,
            config.no_dev,
            config.writable_dirs.len(),
            config.bind_tcp_ports.len(),
            config.connect_tcp_ports.len(),
            config.scope_signals,
            config.scope_abstract_unix,
            config.exec_allow_paths.len(),
            config.device_ioctl_allow_paths.len()
        ));
        if logging::debug_enabled() {
            for path in &config.writable_dirs {
                logging::debug(format_args!(
                    "write allow dir: {}",
                    path.as_c_str().to_string_lossy()
                ));
            }
            for port in &config.bind_tcp_ports {
                logging::debug(format_args!("bind TCP allow port: {}", port));
            }
            for port in &config.connect_tcp_ports {
                logging::debug(format_args!("connect TCP allow port: {}", port));
            }
            for path in &config.exec_allow_paths {
                logging::debug(format_args!(
                    "exec allow path: {}",
                    path.as_c_str().to_string_lossy()
                ));
            }
            for path in &config.device_ioctl_allow_paths {
                logging::debug(format_args!(
                    "device ioctl allow path: {}",
                    path.as_c_str().to_string_lossy()
                ));
            }
        }
    }

    let (cmd_c, argv_c) = prepare_command(&cli.cmd, cli.expand_env)
        .with_context(|| format!("prepare command {:?}", cli.cmd))?;
    let child_pid = spawn_child(child_mask, landlock_config, &cmd_c, &argv_c)
        .with_context(|| format!("spawn child {:?}", cli.cmd))?;
    let use_pgroup = manage_process_group(cli.pgroup_kill, child_pid);

    supervise_child(&cli, &expect_zero, child_pid, use_pgroup, &mut signal_fd)
}

pub(super) fn explain_effective_command(cmd: &[String], expand_env: bool) -> Result<Vec<String>> {
    resolve_command_args(cmd, expand_env)
}

pub(crate) fn bench_resolve_command_args(cmd: &[String], expand_env: bool) -> Result<Vec<String>> {
    resolve_command_args(cmd, expand_env)
}

pub(crate) fn bench_parse_shebang_interpreter(bytes: &[u8]) -> Option<String> {
    parse_shebang_interpreter(bytes)
}

pub(crate) fn bench_parse_elf_interpreter(bytes: &[u8]) -> Result<Option<String>> {
    parse_elf_interpreter(bytes)
}

pub(super) fn explain_landlock_config(cli: &Cli) -> Result<Option<LandlockExplain>> {
    let config = build_landlock_config(cli)?;
    Ok(config.map(|config| LandlockExplain {
        write_requested: config.write_requested,
        warn_only: config.warn_only,
        no_dev: config.no_dev,
        preset_names: config
            .preset_names
            .iter()
            .map(|name| (*name).to_string())
            .collect(),
        writable_dirs: config
            .writable_dirs
            .iter()
            .map(|path| path.as_c_str().to_string_lossy().into_owned())
            .collect(),
        bind_tcp_ports: config.bind_tcp_ports,
        connect_tcp_ports: config.connect_tcp_ports,
        scope_signals: config.scope_signals,
        scope_abstract_unix: config.scope_abstract_unix,
        exec_allow_paths: config
            .exec_allow_paths
            .iter()
            .map(|path| path.as_c_str().to_string_lossy().into_owned())
            .collect(),
        device_ioctl_allow_paths: config
            .device_ioctl_allow_paths
            .iter()
            .map(|path| path.as_c_str().to_string_lossy().into_owned())
            .collect(),
    }))
}

fn build_landlock_config(cli: &Cli) -> Result<Option<LandlockConfig>> {
    let write_requested = cli.write_restrict
        || !cli.write_allow.is_empty()
        || !cli.write_preset.is_empty()
        || cli.write_no_dev;
    let tcp_requested = !cli.bind_tcp_allow.is_empty() || !cli.connect_tcp_allow.is_empty();
    let scope_requested = cli.scope_signals || cli.scope_abstract_unix;
    let exec_requested = !cli.exec_allow.is_empty();
    let device_ioctl_requested = !cli.device_ioctl_allow.is_empty();
    let enabled = write_requested
        || tcp_requested
        || scope_requested
        || exec_requested
        || device_ioctl_requested;
    if !enabled {
        return Ok(None);
    }

    let mut unique = BTreeSet::new();
    let mut preset_names = Vec::new();
    let mut exec_allow = BTreeSet::new();
    let mut device_ioctl_allow = BTreeSet::new();

    for preset in &cli.write_preset {
        let name = preset.as_str();
        if !preset_names.contains(&name) {
            let _ = preset_names.push_mut(name);
        }
        for raw in preset_paths(*preset) {
            insert_landlock_writable_dir(&mut unique, raw, None, true)?;
        }
    }

    for raw in &cli.write_allow {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("--write-allow PATH cannot be empty");
        }
        insert_landlock_writable_dir(&mut unique, trimmed, None, false)?;
    }

    if exec_requested {
        let args = resolve_command_args(&cli.cmd, cli.expand_env)?;
        if let Some(program) = args.first() {
            insert_landlock_exec_path(&mut exec_allow, program, None)?;
        }
    }

    for raw in &cli.exec_allow {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("--exec-allow PATH cannot be empty");
        }
        insert_landlock_exec_path(&mut exec_allow, trimmed, None)?;
    }

    for raw in &cli.device_ioctl_allow {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            bail!("--device-ioctl-allow PATH cannot be empty");
        }
        insert_landlock_device_ioctl_path(&mut device_ioctl_allow, trimmed, None)?;
    }

    let writable_dirs = unique
        .into_iter()
        .map(|path| CString::new(path).context("landlock writable path contains NUL byte"))
        .collect::<Result<Vec<_>>>()?;
    let exec_allow_paths = exec_allow
        .into_iter()
        .map(|path| CString::new(path).context("landlock exec allow path contains NUL byte"))
        .collect::<Result<Vec<_>>>()?;
    let device_ioctl_allow_paths = device_ioctl_allow
        .into_iter()
        .map(|path| {
            CString::new(path).context("landlock device ioctl allow path contains NUL byte")
        })
        .collect::<Result<Vec<_>>>()?;

    let bind_tcp_ports = unique_ports(&cli.bind_tcp_allow);
    let connect_tcp_ports = unique_ports(&cli.connect_tcp_allow);

    Ok(Some(LandlockConfig {
        write_requested,
        warn_only: cli.write_warn_only,
        no_dev: cli.write_no_dev,
        preset_names,
        writable_dirs,
        bind_tcp_ports,
        connect_tcp_ports,
        scope_signals: cli.scope_signals,
        scope_abstract_unix: cli.scope_abstract_unix,
        exec_allow_paths,
        device_ioctl_allow_paths,
    }))
}

fn unique_ports(raw_ports: &[u16]) -> Vec<u16> {
    raw_ports
        .iter()
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn preset_paths(preset: WritePreset) -> &'static [&'static str] {
    match preset {
        WritePreset::Tmp => &["/tmp", "/var/tmp"],
        WritePreset::Runtime => &["/tmp", "/var/tmp", "/run"],
    }
}

fn insert_landlock_writable_dir(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
    allow_missing: bool,
) -> Result<()> {
    let Some(canonical) = canonicalize_allow_path(raw, source, allow_missing, "write allow path")?
    else {
        return Ok(());
    };
    let metadata = std::fs::metadata(&canonical)
        .with_context(|| format!("inspect write allow path '{}'", canonical.display()))?;
    if !metadata.is_dir() {
        bail!(
            "write allow path '{}' is not a directory",
            canonical.display()
        );
    }
    unique.insert(canonical.as_os_str().as_bytes().to_vec());
    Ok(())
}

fn insert_landlock_exec_path(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
) -> Result<()> {
    let mut visited = BTreeSet::new();
    insert_landlock_exec_path_inner(unique, raw, source, &mut visited)
}

fn insert_landlock_exec_path_inner(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
    visited: &mut BTreeSet<PathBuf>,
) -> Result<()> {
    let resolved = resolve_exec_allow_path(raw, source)?;
    if !visited.insert(resolved.canonical.clone()) {
        return Ok(());
    }

    unique.insert(resolved.resolved.as_os_str().as_bytes().to_vec());
    unique.insert(resolved.canonical.as_os_str().as_bytes().to_vec());

    if resolved.metadata.is_file()
        && let Some(interpreter) = detect_exec_interpreter(&resolved.canonical)?
    {
        insert_landlock_exec_path_inner(unique, &interpreter, None, visited)?;
    }

    Ok(())
}

fn insert_landlock_device_ioctl_path(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
) -> Result<()> {
    use std::os::unix::fs::FileTypeExt;

    let canonical = canonicalize_allow_path(raw, source, false, "device ioctl allow path")?
        .ok_or_else(|| Error::msg(format!("device ioctl allow path '{raw}' could not be resolved")))?;
    let metadata = std::fs::metadata(&canonical)
        .with_context(|| format!("inspect device ioctl allow path '{}'", canonical.display()))?;
    let file_type = metadata.file_type();
    if !metadata.is_dir() && !file_type.is_char_device() && !file_type.is_block_device() {
        bail!(
            "device ioctl allow path '{}' is neither a directory nor a device node",
            canonical.display()
        );
    }
    unique.insert(canonical.as_os_str().as_bytes().to_vec());
    Ok(())
}

fn canonicalize_allow_path(
    raw: &str,
    source: Option<(&str, usize)>,
    allow_missing: bool,
    kind: &str,
) -> Result<Option<PathBuf>> {
    let path = PathBuf::from(raw);
    match std::fs::canonicalize(&path) {
        Ok(canonical) => Ok(Some(canonical)),
        Err(err) if allow_missing && err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| match source {
            Some((file, line)) => format!("canonicalize {kind} '{raw}' (from {file}:{line})"),
            None => format!("canonicalize {kind} '{raw}'"),
        }),
    }
}

struct ResolvedExecAllowPath {
    resolved: PathBuf,
    canonical: PathBuf,
    metadata: std::fs::Metadata,
}

fn resolve_exec_allow_path(
    raw: &str,
    source: Option<(&str, usize)>,
) -> Result<ResolvedExecAllowPath> {
    let resolved = resolve_exec_allow_path_candidate(raw, source)?;
    let canonical = std::fs::canonicalize(&resolved)
        .with_context(|| format!("canonicalize exec allow path '{}'", resolved.display()))?;
    let metadata = std::fs::metadata(&canonical)
        .with_context(|| format!("inspect exec allow path '{}'", canonical.display()))?;
    if !metadata.is_dir() && !metadata.is_file() {
        bail!(
            "exec allow path '{}' is neither a regular file nor a directory",
            canonical.display()
        );
    }
    Ok(ResolvedExecAllowPath {
        resolved,
        canonical,
        metadata,
    })
}

fn resolve_exec_allow_path_candidate(raw: &str, source: Option<(&str, usize)>) -> Result<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    let path = PathBuf::from(raw);
    if raw.contains('/') {
        return Ok(path);
    }

    let search_path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&search_path) {
        let candidate = dir.join(raw);
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Ok(candidate);
        }
    }

    match source {
        Some((file, line)) => {
            bail!("resolve exec allow path '{raw}' from PATH (from {file}:{line})")
        }
        None => bail!("resolve exec allow path '{raw}' from PATH"),
    }
}

fn detect_exec_interpreter(path: &PathBuf) -> Result<Option<String>> {
    let bytes = std::fs::read(path).with_context(|| {
        format!(
            "read exec allow file '{}' for interpreter discovery",
            path.display()
        )
    })?;
    if let Some(interpreter) = parse_shebang_interpreter(&bytes) {
        return Ok(Some(interpreter));
    }
    parse_elf_interpreter(&bytes)
}

fn parse_shebang_interpreter(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"#!") {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let line = std::str::from_utf8(&bytes[2..end]).ok()?.trim_start();
    let interpreter = line.split_whitespace().next()?;
    if interpreter.starts_with('/') {
        Some(interpreter.to_string())
    } else {
        None
    }
}

fn parse_elf_interpreter(bytes: &[u8]) -> Result<Option<String>> {
    const ELF_MAGIC: &[u8; 4] = b"\x7FELF";
    const EI_CLASS: usize = 4;
    const EI_DATA: usize = 5;
    const ELFCLASS32: u8 = 1;
    const ELFCLASS64: u8 = 2;
    const ELFDATA2LSB: u8 = 1;
    const ELFDATA2MSB: u8 = 2;
    const PT_INTERP: u32 = 3;

    if bytes.len() < 0x34 || &bytes[..4] != ELF_MAGIC {
        return Ok(None);
    }

    let little_endian = match bytes[EI_DATA] {
        ELFDATA2LSB => true,
        ELFDATA2MSB => false,
        _ => return Ok(None),
    };

    let (phoff, phentsize, phnum) = match bytes[EI_CLASS] {
        ELFCLASS32 => (
            read_u32(bytes, 28, little_endian)? as usize,
            read_u16(bytes, 42, little_endian)? as usize,
            read_u16(bytes, 44, little_endian)? as usize,
        ),
        ELFCLASS64 => (
            read_u64(bytes, 32, little_endian)? as usize,
            read_u16(bytes, 54, little_endian)? as usize,
            read_u16(bytes, 56, little_endian)? as usize,
        ),
        _ => return Ok(None),
    };

    for idx in 0..phnum {
        let start = phoff + idx * phentsize;
        let end = start + phentsize;
        if end > bytes.len() {
            bail!("ELF program header exceeds file size");
        }
        let p_type = read_u32(bytes, start, little_endian)?;
        if p_type != PT_INTERP {
            continue;
        }

        let (offset, filesz) = if bytes[EI_CLASS] == ELFCLASS32 {
            (
                read_u32(bytes, start + 4, little_endian)? as usize,
                read_u32(bytes, start + 16, little_endian)? as usize,
            )
        } else {
            (
                read_u64(bytes, start + 8, little_endian)? as usize,
                read_u64(bytes, start + 32, little_endian)? as usize,
            )
        };
        let end = offset + filesz;
        if end > bytes.len() {
            bail!("ELF interpreter segment exceeds file size");
        }
        let interp = &bytes[offset..end];
        let nul = interp
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(interp.len());
        let interpreter = std::str::from_utf8(&interp[..nul])
            .context("ELF interpreter path is not valid UTF-8")?
            .to_string();
        if interpreter.is_empty() {
            return Ok(None);
        }
        return Ok(Some(interpreter));
    }

    Ok(None)
}

fn read_u16(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u16> {
    let slice = bytes
        .get(offset..offset + 2)
        .context("ELF header read out of bounds")?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    })
}

fn read_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .context("ELF header read out of bounds")?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    })
}

fn read_u64(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .context("ELF header read out of bounds")?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u64::from_le_bytes(raw)
    } else {
        u64::from_be_bytes(raw)
    })
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
            _ => PollTimeout::BLOCK,
        };
        match poll_fds(&mut fds, poll_timeout) {
            Ok(_) => {}
            Err(err) => {
                if err == Errno::EINTR {
                    continue;
                }
                return Err(err).context("poll");
            }
        }
        let events = fds[0].revents().unwrap_or_else(PollFlags::empty);
        if signal_fd_poll_failed(events) {
            bail!("signal fd poll failed with events {:?}", events);
        }
        if events.contains(PollFlags::POLLIN) {
            while let Some(info) = signal_fd.read_signal()? {
                let sig = match Signal::try_from(info.ssi_signo as i32) {
                    Ok(sig) => sig,
                    Err(_) => {
                        logging::warn(format_args!(
                            "received unexpected signal {}",
                            info.ssi_signo
                        ));
                        continue;
                    }
                };
                if sig == SIGCHLD {
                    handle_sigchld(cli, child_pid, &mut main_exit)?;
                } else if sig == SIGTTIN || sig == SIGTTOU {
                    logging::debug(format_args!("ignoring {:?}", sig));
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
            logging::info(format_args!("grace period expired; sending SIGKILL"));
            send_signal(use_pgroup, child_pid, SIGKILL);
            sigkill_sent = true;
        }
        if main_exit.is_some() {
            break;
        }
    }

    let final_exit = compute_exit_code(main_exit, expect_zero);

    if use_pgroup {
        logging::info(format_args!("sending SIGTERM to PGID"));
        send_signal(true, child_pid, SIGTERM);
        if !wait_for_children(cli.grace_ms, cli.warn_on_reap)? {
            logging::info(format_args!(
                "still alive after {} ms; sending SIGKILL",
                cli.grace_ms
            ));
            send_signal(true, child_pid, SIGKILL);
            let fully_reaped = wait_for_children(cli.grace_ms, cli.warn_on_reap)?;
            if !fully_reaped {
                logging::warn(format_args!(
                    "child processes still alive after SIGKILL wait of {} ms",
                    cli.grace_ms
                ));
            }
        }
    } else {
        let _ = wait_for_children(cli.grace_ms, cli.warn_on_reap)?;
    }

    logging::info(format_args!("exiting with {}", final_exit));
    Ok(final_exit)
}

fn signal_fd_poll_failed(events: PollFlags) -> bool {
    events.intersects(PollFlags::POLLERR)
        || events.intersects(PollFlags::POLLHUP)
        || events.intersects(PollFlags::POLLNVAL)
}

fn is_termination_signal(sig: Signal) -> bool {
    sig == SIGTERM || sig == SIGINT || sig == SIGQUIT
}

fn log_reaped_secondary(pid: Pid, warn_on_reap: bool) {
    if warn_on_reap {
        logging::warn(format_args!("reaped secondary PID {}", pid));
    } else {
        logging::debug(format_args!("reaped secondary PID {}", pid));
    }
}

fn log_stopped_child(pid: Pid, sig: Signal, warn_on_reap: bool) {
    if warn_on_reap {
        logging::warn(format_args!("child PID {} stopped by signal {:?}", pid, sig));
    } else {
        logging::debug(format_args!("child PID {} stopped by signal {:?}", pid, sig));
    }
}

fn handle_sigchld(cli: &Cli, child_pid: Pid, main_exit: &mut Option<i32>) -> Result<()> {
    loop {
        match waitpid_any_nohang() {
            Ok(WaitStatus::Exited(pid, code)) if pid == child_pid => *main_exit = Some(code),
            Ok(WaitStatus::Exited(pid, _)) => log_reaped_secondary(pid, cli.warn_on_reap),
            Ok(WaitStatus::Signaled(pid, sig, _)) if pid == child_pid => {
                *main_exit = Some(128 + sig as i32);
            }
            Ok(WaitStatus::Signaled(pid, _, _)) => log_reaped_secondary(pid, cli.warn_on_reap),
            Ok(WaitStatus::Stopped(pid, sig)) => {
                log_stopped_child(pid, sig, cli.warn_on_reap);
                break;
            }
            Ok(WaitStatus::StillAlive) | Ok(WaitStatus::Continued(_)) => break,
            Err(Errno::ECHILD) => break,
            Err(Errno::EINTR) => continue,
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
        match waitpid_any_nohang() {
            Ok(WaitStatus::StillAlive) => (),
            Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                log_reaped_secondary(pid, warn_on_reap);
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

    #[test]
    fn signal_fd_poll_failed_rejects_error_states() {
        assert!(!signal_fd_poll_failed(PollFlags::empty()));
        assert!(!signal_fd_poll_failed(PollFlags::POLLIN));
        assert!(signal_fd_poll_failed(PollFlags::POLLERR));
        assert!(signal_fd_poll_failed(PollFlags::POLLHUP));
        assert!(signal_fd_poll_failed(PollFlags::POLLNVAL));
    }
}
