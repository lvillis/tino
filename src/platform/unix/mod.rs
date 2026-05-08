use crate::{
    Context, Error, Result, bail,
    cli::{Cli, WritePreset},
    logging,
};
use std::{
    collections::BTreeSet,
    ffi::{CString, OsString},
    os::fd::AsFd,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

mod child;
mod landlock;
mod signals;
pub(crate) mod sys;

use child::{
    configure_parent_prctl, manage_process_group, pdeath_signal, prepare_command,
    resolve_command_args, spawn_child,
};
use landlock::LandlockConfig;
use signals::{send_signal, setup_signal_delivery};
use sys::{
    Errno, Pid, PollFd, PollFlags, PollTimeout, SIGCHLD, SIGINT, SIGKILL, SIGQUIT, SIGTERM,
    SIGTTIN, SIGTTOU, SigSet, SignalFd, WaitStatus, poll_fds, waitpid_any_nohang,
};
#[cfg(test)]
use sys::Signal;

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
    let (previous_mask, mut signal_fd) = setup_signal_delivery()?;
    let _signal_mask_restore = SignalMaskRestore::new(&previous_mask);
    let child_pdeath = pdeath_signal(&cli)?;
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
    configure_parent_prctl(&cli)?;
    let child_pid = spawn_child(&previous_mask, child_pdeath, landlock_config, &cmd_c, &argv_c)
        .with_context(|| format!("spawn child {:?}", cli.cmd))?;
    let use_pgroup = manage_process_group(cli.pgroup_kill, child_pid);

    supervise_child(&cli, &expect_zero, child_pid, use_pgroup, &mut signal_fd)
}

struct SignalMaskRestore<'a> {
    previous_mask: &'a SigSet,
}

impl<'a> SignalMaskRestore<'a> {
    fn new(previous_mask: &'a SigSet) -> Self {
        Self { previous_mask }
    }
}

impl Drop for SignalMaskRestore<'_> {
    fn drop(&mut self) {
        if let Err(err) = self.previous_mask.thread_set_mask() {
            logging::warn(format_args!("restore signal mask failed: {}", err));
        }
    }
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
    let write_requested =
        cli.write_restrict || !cli.write_allow.is_empty() || !cli.write_preset.is_empty();
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
            insert_landlock_main_exec_path(&mut exec_allow, program)?;
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
    insert_landlock_exec_path_inner(unique, raw, source, &mut visited, ExecAllowMode::Strict)
}

fn insert_landlock_main_exec_path(unique: &mut BTreeSet<Vec<u8>>, raw: &str) -> Result<()> {
    let mut visited = BTreeSet::new();
    insert_landlock_exec_path_inner(unique, raw, None, &mut visited, ExecAllowMode::Auto)
}

fn insert_landlock_exec_path_inner(
    unique: &mut BTreeSet<Vec<u8>>,
    raw: &str,
    source: Option<(&str, usize)>,
    visited: &mut BTreeSet<PathBuf>,
    mode: ExecAllowMode,
) -> Result<()> {
    let resolved = match mode {
        ExecAllowMode::Strict => Some(resolve_exec_allow_path(raw, source)?),
        ExecAllowMode::Auto => resolve_main_exec_allow_path(raw)?,
    };
    let Some(resolved) = resolved else {
        return Ok(());
    };
    insert_resolved_exec_path(unique, resolved, visited, mode)
}

fn insert_resolved_exec_path(
    unique: &mut BTreeSet<Vec<u8>>,
    resolved: ResolvedExecAllowPath,
    visited: &mut BTreeSet<PathBuf>,
    mode: ExecAllowMode,
) -> Result<()> {
    if !visited.insert(resolved.canonical.clone()) {
        return Ok(());
    }

    unique.insert(resolved.canonical.as_os_str().as_bytes().to_vec());

    if is_executable_file(&resolved.metadata) {
        for interpreter in detect_exec_interpreters(&resolved.canonical)? {
            match interpreter {
                ExecInterpreter::Candidate(path) => {
                    insert_landlock_exec_path_inner(unique, &path, None, visited, mode)?;
                }
                ExecInterpreter::Missing { .. } if mode == ExecAllowMode::Auto => {}
                ExecInterpreter::Missing { command } => {
                    bail!("resolve exec allow path '{command}' from shebang PATH");
                }
            }
        }
    }

    Ok(())
}

fn is_executable_file(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;

    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
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
    canonical: PathBuf,
    metadata: std::fs::Metadata,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecAllowMode {
    Strict,
    Auto,
}

fn resolve_exec_allow_path(
    raw: &str,
    source: Option<(&str, usize)>,
) -> Result<ResolvedExecAllowPath> {
    let resolved = resolve_exec_allow_path_candidate(raw, source)?;
    resolved_exec_allow_path_from_candidate(&resolved)
}

fn resolve_main_exec_allow_path(raw: &str) -> Result<Option<ResolvedExecAllowPath>> {
    let Some(candidate) = resolve_main_exec_allow_path_candidate(raw)? else {
        return Ok(None);
    };
    resolved_exec_allow_path_from_candidate(&candidate).map(Some)
}

fn resolved_exec_allow_path_from_candidate(resolved: &PathBuf) -> Result<ResolvedExecAllowPath> {
    let canonical = std::fs::canonicalize(resolved)
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
        canonical,
        metadata,
    })
}

fn resolve_main_exec_allow_path_candidate(raw: &str) -> Result<Option<PathBuf>> {
    if raw.contains('/') {
        return match std::fs::metadata(raw) {
            Ok(_) => Ok(Some(PathBuf::from(raw))),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err)
                .with_context(|| format!("inspect main exec allow path candidate '{raw}'")),
        };
    }

    let search_path = exec_search_path();
    Ok(find_executable_in_search_path(raw, &search_path, None))
}

fn resolve_exec_allow_path_candidate(raw: &str, source: Option<(&str, usize)>) -> Result<PathBuf> {
    let path = PathBuf::from(raw);
    if raw.contains('/') {
        return Ok(path);
    }

    let search_path = exec_search_path();
    if let Some(candidate) = find_executable_in_search_path(raw, &search_path, None) {
        return Ok(candidate);
    }

    match source {
        Some((file, line)) => {
            bail!("resolve exec allow path '{raw}' from PATH (from {file}:{line})")
        }
        None => bail!("resolve exec allow path '{raw}' from PATH"),
    }
}

fn find_executable_in_search_path(
    raw: &str,
    search_path: &OsString,
    relative_to: Option<&Path>,
) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    for dir in std::env::split_paths(&search_path) {
        let candidate = if dir.is_relative() {
            relative_to.map_or_else(|| dir.join(raw), |base| base.join(&dir).join(raw))
        } else {
            dir.join(raw)
        };
        let Ok(metadata) = std::fs::metadata(&candidate) else {
            continue;
        };
        if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
            return Some(candidate);
        }
    }
    None
}

fn exec_search_path() -> OsString {
    std::env::var_os("PATH").unwrap_or_else(default_exec_search_path)
}

fn default_exec_search_path() -> OsString {
    let fallback = || OsString::from("/bin:/usr/bin");
    // SAFETY: the first call queries the required buffer length for _CS_PATH.
    let len = unsafe { libc::confstr(libc::_CS_PATH, std::ptr::null_mut(), 0) };
    if len == 0 {
        return fallback();
    }

    let mut buf = vec![0u8; len];
    // SAFETY: buffer is valid for writes of `buf.len()` bytes.
    let written = unsafe { libc::confstr(libc::_CS_PATH, buf.as_mut_ptr().cast(), buf.len()) };
    if written == 0 || written > buf.len() {
        return fallback();
    }
    buf.truncate(written.saturating_sub(1));
    OsString::from_vec(buf)
}

fn detect_exec_interpreters(path: &PathBuf) -> Result<Vec<ExecInterpreter>> {
    let bytes = std::fs::read(path).with_context(|| {
        format!(
            "read exec allow file '{}' for interpreter discovery",
            path.display()
        )
    })?;
    let shebang_interpreters = parse_shebang_exec_interpreters(&bytes);
    if !shebang_interpreters.is_empty() {
        return Ok(shebang_interpreters);
    }
    Ok(parse_elf_interpreter(&bytes)?
        .into_iter()
        .map(ExecInterpreter::Candidate)
        .collect())
}

fn parse_shebang_interpreter(bytes: &[u8]) -> Option<String> {
    parse_shebang_exec_paths(bytes).into_iter().next()
}

fn parse_shebang_exec_paths(bytes: &[u8]) -> Vec<String> {
    parse_shebang_exec_interpreters(bytes)
        .into_iter()
        .map(ExecInterpreter::into_display_path)
        .collect()
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ExecInterpreter {
    Candidate(String),
    Missing { command: String },
}

impl ExecInterpreter {
    fn into_display_path(self) -> String {
        match self {
            Self::Candidate(path) => path,
            Self::Missing { command } => command,
        }
    }
}

fn parse_shebang_exec_interpreters(bytes: &[u8]) -> Vec<ExecInterpreter> {
    let Some(parts) = parse_shebang_parts(bytes) else {
        return Vec::new();
    };

    let mut paths = vec![ExecInterpreter::Candidate(parts.interpreter.to_string())];
    if is_env_interpreter(parts.interpreter)
        && let Some(command) = env_shebang_command(parts.argument)
    {
        let _ = paths.push_mut(command.resolve());
    }
    paths
}

struct ShebangParts<'a> {
    interpreter: &'a str,
    argument: Option<&'a str>,
}

fn parse_shebang_parts(bytes: &[u8]) -> Option<ShebangParts<'_>> {
    if !bytes.starts_with(b"#!") {
        return None;
    }
    let end = bytes
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(bytes.len());
    let line = std::str::from_utf8(&bytes[2..end]).ok()?;
    let line = line.trim_ascii_end();
    let interpreter_start = line
        .as_bytes()
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())?;
    let rest = &line[interpreter_start..];
    let interpreter_end = rest
        .as_bytes()
        .iter()
        .position(|byte| byte.is_ascii_whitespace())
        .unwrap_or(rest.len());
    let interpreter = &rest[..interpreter_end];
    if !interpreter.starts_with('/') {
        return None;
    }
    let argument = rest[interpreter_end..].trim_ascii_start();
    Some(ShebangParts {
        interpreter,
        argument: (!argument.is_empty()).then_some(argument),
    })
}

fn is_env_interpreter(path: &str) -> bool {
    path.rsplit('/').next() == Some("env")
}

struct EnvShebangCommand {
    command: String,
    search_path: Option<OsString>,
    chdir: Option<PathBuf>,
}

enum EnvSplitExpansion {
    Value(String),
    Unset,
}

enum EnvOptionAction {
    Continue,
    Return(Option<EnvShebangCommand>),
    Invalid,
}

impl EnvShebangCommand {
    fn resolve(&self) -> ExecInterpreter {
        if self.command.contains('/') {
            let path = PathBuf::from(self.command.as_str());
            let candidate = if path.is_relative() {
                self.chdir
                    .as_deref()
                    .map_or(path.clone(), |dir| dir.join(&path))
            } else {
                path
            };
            return ExecInterpreter::Candidate(candidate.to_string_lossy().into_owned());
        }
        if let Some(search_path) = &self.search_path
            && let Some(candidate) =
                find_executable_in_search_path(&self.command, search_path, self.chdir.as_deref())
        {
            return ExecInterpreter::Candidate(candidate.to_string_lossy().into_owned());
        }
        if self.search_path.is_some() {
            return ExecInterpreter::Missing {
                command: self.command.clone(),
            };
        }
        if let Some(chdir) = self.chdir.as_deref() {
            let search_path = exec_search_path();
            if let Some(candidate) =
                find_executable_in_search_path(&self.command, &search_path, Some(chdir))
            {
                return ExecInterpreter::Candidate(candidate.to_string_lossy().into_owned());
            }
            return ExecInterpreter::Missing {
                command: self.command.clone(),
            };
        }
        ExecInterpreter::Candidate(self.command.clone())
    }
}

fn env_shebang_command(argument: Option<&str>) -> Option<EnvShebangCommand> {
    let fields = env_shebang_argument_fields(argument?)?;
    env_shebang_command_fields(&fields, None, None)
}

fn env_shebang_argument_fields(argument: &str) -> Option<Vec<String>> {
    if let Some(split) = env_split_string_value(argument) {
        return split_env_split_string(split);
    }
    Some(vec![argument.to_owned()])
}

fn env_split_string_value(arg: &str) -> Option<&str> {
    arg.strip_prefix("-vS")
        .or_else(|| arg.strip_prefix("-S"))
        .or_else(|| arg.strip_prefix("--split-string="))
}

fn split_env_split_string(raw: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut current = String::new();
    let mut in_field = false;
    let mut quote = None;
    let mut escaped = false;
    let mut truncated = false;

    let mut idx = 0usize;
    while idx < raw.len() {
        let ch = raw[idx..].chars().next()?;
        idx += ch.len_utf8();
        if escaped {
            match ch {
                '_' if quote.is_none() => {
                    if in_field {
                        let _ = fields.push_mut(std::mem::take(&mut current));
                        in_field = false;
                    }
                }
                '_' => {
                    current.push(' ');
                    in_field = true;
                }
                'c' => {
                    escaped = false;
                    truncated = true;
                    break;
                }
                'n' => {
                    current.push('\n');
                    in_field = true;
                }
                't' => {
                    current.push('\t');
                    in_field = true;
                }
                'r' => {
                    current.push('\r');
                    in_field = true;
                }
                'f' => {
                    current.push('\x0c');
                    in_field = true;
                }
                'v' => {
                    current.push('\x0b');
                    in_field = true;
                }
                'a' => {
                    current.push('\x07');
                    in_field = true;
                }
                'b' => {
                    current.push('\x08');
                    in_field = true;
                }
                '\\' | '\'' | '"' | '$' | '#' => {
                    current.push(ch);
                    in_field = true;
                }
                _ => return None,
            }
            escaped = false;
            continue;
        }
        if quote != Some('\'') && ch == '\\' {
            escaped = true;
            continue;
        }
        if quote != Some('\'') && ch == '$' {
            match expand_env_split_variable(raw, &mut idx)? {
                EnvSplitExpansion::Value(value) => {
                    current.push_str(&value);
                    in_field = true;
                }
                EnvSplitExpansion::Unset => {}
            }
            continue;
        }
        if let Some(active_quote) = quote {
            if ch == active_quote {
                quote = None;
            } else {
                current.push(ch);
            }
            in_field = true;
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            in_field = true;
            continue;
        }
        if ch == '#' && !in_field {
            break;
        }
        if ch.is_ascii_whitespace() {
            if in_field {
                let _ = fields.push_mut(std::mem::take(&mut current));
                in_field = false;
            }
            continue;
        }
        current.push(ch);
        in_field = true;
    }

    if escaped || (quote.is_some() && !truncated) {
        return None;
    }
    if in_field {
        let _ = fields.push_mut(current);
    }
    Some(fields)
}

fn expand_env_split_variable(raw: &str, idx: &mut usize) -> Option<EnvSplitExpansion> {
    if !raw[*idx..].starts_with('{') {
        return None;
    }
    let name_start = *idx + 1;
    let name_end = name_start + raw[name_start..].find('}')?;
    let name = &raw[name_start..name_end];
    if !is_env_split_variable_name(name) {
        return None;
    }
    *idx = name_end + 1;
    Some(match std::env::var_os(name) {
        Some(value) => EnvSplitExpansion::Value(value.to_string_lossy().into_owned()),
        None => EnvSplitExpansion::Unset,
    })
}

fn is_env_split_variable_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && (bytes[0] == b'_' || bytes[0].is_ascii_alphabetic())
        && bytes[1..]
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric())
}

fn env_shebang_command_fields(
    fields: &[String],
    mut search_path: Option<OsString>,
    mut chdir: Option<PathBuf>,
) -> Option<EnvShebangCommand> {
    let mut idx = 0usize;

    while idx < fields.len() {
        let arg = fields[idx].as_str();
        idx += 1;
        if arg == "--" {
            return env_shebang_command_after_double_dash(&fields[idx..], search_path, chdir);
        }
        if arg.starts_with("--") {
            match env_long_option_action(arg, fields, &mut idx, &mut search_path, &mut chdir) {
                EnvOptionAction::Continue => continue,
                EnvOptionAction::Return(command) => return command,
                EnvOptionAction::Invalid => return None,
            }
        }
        if arg.starts_with('-') {
            match env_short_option_action(arg, fields, &mut idx, &mut search_path, &mut chdir) {
                EnvOptionAction::Continue => continue,
                EnvOptionAction::Return(command) => return command,
                EnvOptionAction::Invalid => return None,
            }
        }
        if let Some(path) = path_assignment(arg) {
            search_path = Some(OsString::from(path));
            continue;
        }
        if arg.contains('=') {
            continue;
        }
        return Some(EnvShebangCommand {
            command: arg.to_owned(),
            search_path,
            chdir,
        });
    }
    None
}

fn env_long_option_action(
    arg: &str,
    fields: &[String],
    idx: &mut usize,
    search_path: &mut Option<OsString>,
    chdir: &mut Option<PathBuf>,
) -> EnvOptionAction {
    if arg == "--split-string" {
        let Some(split) = fields.get(*idx) else {
            return EnvOptionAction::Continue;
        };
        *idx += 1;
        return EnvOptionAction::Return(env_shebang_command_with_split(
            split,
            &fields[*idx..],
            search_path.clone(),
            chdir.clone(),
        ));
    }
    if let Some(split) = arg.strip_prefix("--split-string=") {
        return EnvOptionAction::Return(env_shebang_command_with_split(
            split,
            &fields[*idx..],
            search_path.clone(),
            chdir.clone(),
        ));
    }
    if resets_env(arg) {
        *search_path = Some(default_exec_search_path());
        return EnvOptionAction::Continue;
    }
    if let Some(dir) = arg.strip_prefix("--chdir=") {
        if dir.is_empty() {
            return EnvOptionAction::Invalid;
        }
        *chdir = Some(PathBuf::from(dir));
        return EnvOptionAction::Continue;
    }
    if arg == "--chdir" {
        let Some(dir) = fields.get(*idx) else {
            return EnvOptionAction::Invalid;
        };
        *chdir = Some(PathBuf::from(dir.as_str()));
        *idx += 1;
        return EnvOptionAction::Continue;
    }
    if long_env_option_takes_value(arg) {
        if !arg.contains('=') {
            *idx = idx.saturating_add(1).min(fields.len());
        }
        return EnvOptionAction::Continue;
    }
    if long_env_option_exits_without_command(arg) {
        return EnvOptionAction::Return(None);
    }
    if long_env_option_takes_optional_value(arg) || long_env_option_takes_no_value(arg) {
        return EnvOptionAction::Continue;
    }
    EnvOptionAction::Invalid
}

fn env_short_option_action(
    arg: &str,
    fields: &[String],
    idx: &mut usize,
    search_path: &mut Option<OsString>,
    chdir: &mut Option<PathBuf>,
) -> EnvOptionAction {
    if arg == "-" {
        *search_path = Some(default_exec_search_path());
        return EnvOptionAction::Continue;
    }
    let mut chars = arg.char_indices();
    let _ = chars.next();
    for (offset, opt) in chars {
        let value_start = offset + opt.len_utf8();
        match opt {
            'i' => *search_path = Some(default_exec_search_path()),
            'v' => {}
            '0' => return EnvOptionAction::Return(None),
            'u' | 'a' => {
                if value_start == arg.len() {
                    *idx = idx.saturating_add(1).min(fields.len());
                }
                return EnvOptionAction::Continue;
            }
            'C' => {
                if value_start < arg.len() {
                    *chdir = Some(PathBuf::from(&arg[value_start..]));
                } else if let Some(dir) = fields.get(*idx) {
                    *chdir = Some(PathBuf::from(dir.as_str()));
                    *idx += 1;
                } else {
                    return EnvOptionAction::Invalid;
                }
                return EnvOptionAction::Continue;
            }
            'S' => {
                let split = if value_start < arg.len() {
                    &arg[value_start..]
                } else if let Some(split) = fields.get(*idx) {
                    *idx += 1;
                    split
                } else {
                    return EnvOptionAction::Continue;
                };
                return EnvOptionAction::Return(env_shebang_command_with_split(
                    split,
                    &fields[*idx..],
                    search_path.clone(),
                    chdir.clone(),
                ));
            }
            _ => return EnvOptionAction::Invalid,
        }
    }
    EnvOptionAction::Continue
}

fn env_shebang_command_after_double_dash(
    fields: &[String],
    mut search_path: Option<OsString>,
    chdir: Option<PathBuf>,
) -> Option<EnvShebangCommand> {
    for arg in fields.iter().map(String::as_str) {
        if let Some(path) = path_assignment(arg) {
            search_path = Some(OsString::from(path));
            continue;
        }
        if arg.contains('=') {
            continue;
        }
        return Some(EnvShebangCommand {
            command: arg.to_owned(),
            search_path,
            chdir,
        });
    }
    None
}

fn env_shebang_command_with_split(
    split: &str,
    rest: &[String],
    search_path: Option<OsString>,
    chdir: Option<PathBuf>,
) -> Option<EnvShebangCommand> {
    let mut fields = split_env_split_string(split)?;
    fields.extend_from_slice(rest);
    env_shebang_command_fields(&fields, search_path, chdir)
}

fn resets_env(arg: &str) -> bool {
    matches!(arg, "-" | "-i" | "--ignore-environment")
}

fn path_assignment(arg: &str) -> Option<&str> {
    arg.strip_prefix("PATH=")
}

fn long_env_option_takes_value(arg: &str) -> bool {
    matches!(arg, "--unset" | "--argv0")
        || arg.starts_with("--unset=")
        || arg.starts_with("--argv0=")
}

fn long_env_option_takes_optional_value(arg: &str) -> bool {
    matches!(arg, "--block-signal" | "--default-signal" | "--ignore-signal")
        || arg.starts_with("--block-signal=")
        || arg.starts_with("--default-signal=")
        || arg.starts_with("--ignore-signal=")
}

fn long_env_option_exits_without_command(arg: &str) -> bool {
    matches!(arg, "--help" | "--version" | "--null")
}

fn long_env_option_takes_no_value(arg: &str) -> bool {
    matches!(arg, "--debug" | "--list-signal-handling")
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

    let (phoff, phentsize, phnum, min_phentsize) = match bytes[EI_CLASS] {
        ELFCLASS32 => (
            read_u32(bytes, 28, little_endian)? as usize,
            read_u16(bytes, 42, little_endian)? as usize,
            read_u16(bytes, 44, little_endian)? as usize,
            32usize,
        ),
        ELFCLASS64 => (
            read_u64_usize(bytes, 32, little_endian, "ELF program header offset")?,
            read_u16(bytes, 54, little_endian)? as usize,
            read_u16(bytes, 56, little_endian)? as usize,
            56usize,
        ),
        _ => return Ok(None),
    };
    if phentsize < min_phentsize {
        bail!("ELF program header entry too small");
    }

    for idx in 0..phnum {
        let start = idx
            .checked_mul(phentsize)
            .and_then(|offset| phoff.checked_add(offset))
            .context("ELF program header offset overflow")?;
        let end = start
            .checked_add(phentsize)
            .context("ELF program header offset overflow")?;
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
                read_u64_usize(
                    bytes,
                    start + 8,
                    little_endian,
                    "ELF interpreter segment offset",
                )?,
                read_u64_usize(
                    bytes,
                    start + 32,
                    little_endian,
                    "ELF interpreter segment size",
                )?,
            )
        };
        let end = offset
            .checked_add(filesz)
            .context("ELF interpreter segment offset overflow")?;
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
    let slice = read_elf_bytes(bytes, offset, 2, "ELF header")?;
    let mut raw = [0u8; 2];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    })
}

fn read_u32(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u32> {
    let slice = read_elf_bytes(bytes, offset, 4, "ELF header")?;
    let mut raw = [0u8; 4];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    })
}

fn read_u64(bytes: &[u8], offset: usize, little_endian: bool) -> Result<u64> {
    let slice = read_elf_bytes(bytes, offset, 8, "ELF header")?;
    let mut raw = [0u8; 8];
    raw.copy_from_slice(slice);
    Ok(if little_endian {
        u64::from_le_bytes(raw)
    } else {
        u64::from_be_bytes(raw)
    })
}

fn read_u64_usize(
    bytes: &[u8],
    offset: usize,
    little_endian: bool,
    context: &str,
) -> Result<usize> {
    usize::try_from(read_u64(bytes, offset, little_endian)?)
        .with_context(|| format!("{context} exceeds addressable size"))
}

fn read_elf_bytes<'a>(
    bytes: &'a [u8],
    offset: usize,
    len: usize,
    context: &str,
) -> Result<&'a [u8]> {
    let end = offset
        .checked_add(len)
        .with_context(|| format!("{context} offset overflow"))?;
    bytes
        .get(offset..end)
        .with_context(|| format!("{context} read out of bounds"))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ShutdownDeadline {
    At(Instant),
    Never,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WaitLoop {
    Continue,
    Break,
}

impl ShutdownDeadline {
    fn after(now: Instant, millis: u64) -> Self {
        now.checked_add(Duration::from_millis(millis))
            .map_or(Self::Never, Self::At)
    }

    fn poll_timeout(self) -> PollTimeout {
        match self {
            Self::At(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                PollTimeout::try_from(remaining).unwrap_or(PollTimeout::MAX)
            }
            Self::Never => PollTimeout::MAX,
        }
    }
}

fn supervise_child(
    cli: &Cli,
    expect_zero: &ExitCodeRemap,
    child_pid: Pid,
    use_pgroup: bool,
    signal_fd: &mut SignalFd,
) -> Result<i32> {
    let mut main_exit: Option<i32> = None;
    let mut shutdown_deadline: Option<ShutdownDeadline> = None;
    let mut sigkill_sent = false;
    let mut fds = [PollFd::new(signal_fd.as_fd(), PollFlags::POLLIN)];

    loop {
        let poll_timeout = match (shutdown_deadline, sigkill_sent, main_exit.is_some()) {
            (Some(deadline), false, false) => deadline.poll_timeout(),
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
                let sig = info.ssi_signo as libc::c_int;
                if sig == SIGCHLD as libc::c_int {
                    handle_sigchld(cli, child_pid, &mut main_exit)?;
                } else if sig == SIGTTIN as libc::c_int || sig == SIGTTOU as libc::c_int {
                    logging::debug(format_args!("ignoring signal {}", sig));
                } else {
                    send_signal(use_pgroup, child_pid, sig);
                    if is_termination_signal(sig) && main_exit.is_none() && !sigkill_sent {
                        let now = Instant::now();
                        shutdown_deadline = Some(match shutdown_deadline {
                            None => ShutdownDeadline::after(now, cli.grace_ms),
                            Some(_) => ShutdownDeadline::At(now),
                        });
                    }
                }
            }
        }
        if let Some(ShutdownDeadline::At(deadline)) = shutdown_deadline
            && !sigkill_sent
            && main_exit.is_none()
            && Instant::now() >= deadline
        {
            logging::info(format_args!("grace period expired; sending SIGKILL"));
            send_signal(use_pgroup, child_pid, SIGKILL as libc::c_int);
            sigkill_sent = true;
        }
        if main_exit.is_some() {
            break;
        }
    }

    let final_exit = compute_exit_code(main_exit, expect_zero);

    if use_pgroup {
        logging::info(format_args!("sending SIGTERM to PGID"));
        send_signal(true, child_pid, SIGTERM as libc::c_int);
        if !wait_for_children(cli.grace_ms, cli.warn_on_reap)? {
            logging::info(format_args!(
                "still alive after {} ms; sending SIGKILL",
                cli.grace_ms
            ));
            send_signal(true, child_pid, SIGKILL as libc::c_int);
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

fn is_termination_signal(sig: libc::c_int) -> bool {
    sig == SIGTERM as libc::c_int || sig == SIGINT as libc::c_int || sig == SIGQUIT as libc::c_int
}

fn log_reaped_secondary(pid: Pid, warn_on_reap: bool) {
    if warn_on_reap {
        logging::warn(format_args!("reaped secondary PID {}", pid));
    } else {
        logging::debug(format_args!("reaped secondary PID {}", pid));
    }
}

fn log_stopped_child(pid: Pid, sig: i32, warn_on_reap: bool) {
    if warn_on_reap {
        logging::warn(format_args!("child PID {} stopped by signal {}", pid, sig));
    } else {
        logging::debug(format_args!("child PID {} stopped by signal {}", pid, sig));
    }
}

fn handle_sigchld(cli: &Cli, child_pid: Pid, main_exit: &mut Option<i32>) -> Result<()> {
    loop {
        match waitpid_any_nohang() {
            Ok(status) => match handle_wait_status(status, cli, child_pid, main_exit) {
                WaitLoop::Continue => continue,
                WaitLoop::Break => break,
            },
            Err(Errno::ECHILD) if main_exit.is_some() => break,
            Err(Errno::ECHILD) => {
                bail!("main child is no longer waitable before its exit status was observed")
            }
            Err(Errno::EINTR) => continue,
            Err(e) => bail!("waitpid: {e}"),
        }
    }
    Ok(())
}

fn handle_wait_status(
    status: WaitStatus,
    cli: &Cli,
    child_pid: Pid,
    main_exit: &mut Option<i32>,
) -> WaitLoop {
    let action = wait_status_loop_action(status);
    match status {
        WaitStatus::Exited(pid, code) if pid == child_pid => {
            *main_exit = Some(code);
        }
        WaitStatus::Exited(pid, _) => {
            log_reaped_secondary(pid, cli.warn_on_reap);
        }
        WaitStatus::Signaled(pid, sig, _) if pid == child_pid => {
            *main_exit = Some(128 + sig);
        }
        WaitStatus::Signaled(pid, _, _) => {
            log_reaped_secondary(pid, cli.warn_on_reap);
        }
        WaitStatus::Stopped(pid, sig) => {
            log_stopped_child(pid, sig, cli.warn_on_reap);
        }
        WaitStatus::Continued(_) | WaitStatus::StillAlive => {}
    }
    action
}

fn wait_status_loop_action(status: WaitStatus) -> WaitLoop {
    match status {
        WaitStatus::StillAlive => WaitLoop::Break,
        WaitStatus::Exited(..)
        | WaitStatus::Signaled(..)
        | WaitStatus::Stopped(..)
        | WaitStatus::Continued(..) => WaitLoop::Continue,
    }
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
            Ok(WaitStatus::StillAlive | WaitStatus::Stopped(..) | WaitStatus::Continued(..)) => (),
            Ok(WaitStatus::Exited(pid, _) | WaitStatus::Signaled(pid, _, _)) => {
                log_reaped_secondary(pid, warn_on_reap);
                continue;
            }
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
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvVarGuard {
        name: &'static str,
        original: Option<OsString>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: OsString) -> Self {
            Self::replace(name, Some(value))
        }

        fn unset(name: &'static str) -> Self {
            Self::replace(name, None)
        }

        fn replace(name: &'static str, value: Option<OsString>) -> Self {
            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("environment lock poisoned");
            let original = std::env::var_os(name);
            match value {
                Some(value) => unsafe {
                    std::env::set_var(name, value);
                },
                None => unsafe {
                    std::env::remove_var(name);
                },
            }
            Self {
                name,
                original,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.original.take() {
                Some(value) => unsafe {
                    std::env::set_var(self.name, value);
                },
                None => unsafe {
                    std::env::remove_var(self.name);
                },
            }
        }
    }

    struct PathEnvGuard {
        _env: EnvVarGuard,
    }

    impl PathEnvGuard {
        fn set(value: OsString) -> Self {
            Self {
                _env: EnvVarGuard::set("PATH", value),
            }
        }

        fn unset() -> Self {
            Self {
                _env: EnvVarGuard::unset("PATH"),
            }
        }
    }

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
        assert_eq!(
            super::signals::signal_by_name("TSTP"),
            Some(Signal::SIGTSTP)
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
    fn sigchld_without_waitable_main_child_errors() {
        let mut main_exit = None;
        let err = handle_sigchld(&Cli::default(), Pid::from_raw(i32::MAX), &mut main_exit)
            .expect_err("missing main child status must be explicit");

        assert!(
            format!("{err:#}").contains("main child is no longer waitable"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn write_no_dev_alone_does_not_request_write_restriction() {
        let cli = Cli {
            write_no_dev: true,
            ..Cli::default()
        };

        assert!(build_landlock_config(&cli).unwrap().is_none());
    }

    #[test]
    fn write_no_dev_modifies_write_restriction_when_requested() {
        let cli = Cli {
            write_restrict: true,
            write_no_dev: true,
            ..Cli::default()
        };

        let config = build_landlock_config(&cli)
            .unwrap()
            .expect("write restriction config");
        assert!(config.write_requested);
        assert!(config.no_dev);
    }

    #[test]
    fn exec_allow_symlink_stores_only_canonical_target() {
        use std::os::unix::fs::symlink;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-exec-allow-symlink-{}-{nanos}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create symlink test dir");
        let target = root.join("target");
        let link = root.join("link");
        std::fs::write(&target, b"not-an-elf\n").expect("write target");
        symlink(&target, &link).expect("create symlink");

        let cli = Cli {
            exec_allow: vec![link.to_string_lossy().into_owned()],
            ..Cli::default()
        };
        let config = build_landlock_config(&cli)
            .expect("build config")
            .expect("exec allow config");
        let allowed = config
            .exec_allow_paths
            .iter()
            .map(|path| path.as_c_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert_eq!(allowed, vec![target.canonicalize().unwrap().display().to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exec_allow_non_executable_file_skips_interpreter_probe() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-exec-allow-nonexec-{}-{nanos}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create non-exec test dir");
        let path = root.join("not-executable");
        let mut bytes = minimal_elf64();
        bytes[54..56].copy_from_slice(&1u16.to_le_bytes());
        std::fs::write(&path, bytes).expect("write invalid non-exec elf");
        let mut perms = std::fs::metadata(&path)
            .expect("stat invalid non-exec elf")
            .permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&path, perms).expect("chmod invalid non-exec elf");

        let cli = Cli {
            exec_allow: vec![path.to_string_lossy().into_owned()],
            ..Cli::default()
        };
        let config = build_landlock_config(&cli)
            .expect("non-executable exec allow path should not be probed as ELF")
            .expect("exec allow config");
        assert_eq!(config.exec_allow_paths.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_exec_auto_allow_skips_missing_program() {
        let mut unique = BTreeSet::new();

        insert_landlock_main_exec_path(&mut unique, "/definitely/missing/tino-test-binary")
            .expect("missing main program should be left to execvp");

        assert!(unique.is_empty());
    }

    #[test]
    fn main_exec_auto_allow_skips_missing_env_shebang_command() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let missing = format!("definitely-missing-tino-interpreter-{}-{nanos}", std::process::id());
        let root = std::env::temp_dir().join(format!(
            "tino-main-exec-missing-env-{}-{nanos}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create env shebang test dir");
        let script = root.join("script");
        std::fs::write(&script, format!("#!/usr/bin/env {missing}\n"))
            .expect("write env shebang script");
        let mut perms = std::fs::metadata(&script)
            .expect("stat env shebang script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod env shebang script");

        let mut unique = BTreeSet::new();
        insert_landlock_main_exec_path(&mut unique, &script.to_string_lossy())
            .expect("missing shebang command should be left to child execution");
        let allowed = unique
            .iter()
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect::<Vec<_>>();

        assert!(
            allowed.contains(&script.canonicalize().unwrap().display().to_string()),
            "script itself must still be auto-allowed: {allowed:?}"
        );
        assert!(
            allowed.iter().all(|path| !path.contains(&missing)),
            "missing env shebang command must not be resolved from an unrelated path: {allowed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_exec_auto_allow_respects_env_shebang_path_assignment() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-main-exec-env-path-{}-{nanos}",
            std::process::id(),
        ));
        let parent_path_dir = root.join("parent-path");
        let shebang_path_dir = root.join("shebang-path");
        std::fs::create_dir_all(&parent_path_dir).expect("create parent PATH dir");
        std::fs::create_dir_all(&shebang_path_dir).expect("create shebang PATH dir");
        let parent_tool = parent_path_dir.join("python3");
        std::fs::write(&parent_tool, b"parent python\n").expect("write parent python");
        let mut perms = std::fs::metadata(&parent_tool)
            .expect("stat parent python")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&parent_tool, perms).expect("chmod parent python");

        let script = root.join("script");
        std::fs::write(
            &script,
            format!(
                "#!/usr/bin/env -S -i PATH={} python3\n",
                shebang_path_dir.display()
            ),
        )
        .expect("write env PATH shebang script");
        let mut perms = std::fs::metadata(&script)
            .expect("stat env PATH shebang script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod env PATH shebang script");

        let _path = PathEnvGuard::set(parent_path_dir.as_os_str().to_os_string());
        let mut unique = BTreeSet::new();
        insert_landlock_main_exec_path(&mut unique, &script.to_string_lossy())
            .expect("missing shebang PATH command should not fall back to parent PATH");
        let allowed = unique
            .iter()
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect::<Vec<_>>();

        assert!(
            !allowed.contains(&parent_tool.canonicalize().unwrap().display().to_string()),
            "shebang PATH must not fall back to parent PATH: {allowed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_exec_auto_allow_respects_env_chdir_for_relative_path() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-main-exec-env-chdir-{}-{nanos}",
            std::process::id(),
        ));
        let app_dir = root.join("app");
        let app_bin = app_dir.join("bin");
        let other_bin = root.join("bin");
        std::fs::create_dir_all(&app_bin).expect("create app bin dir");
        std::fs::create_dir_all(&other_bin).expect("create other bin dir");
        let expected_tool = app_bin.join("tool");
        let wrong_tool = other_bin.join("tool");
        for tool in [&expected_tool, &wrong_tool] {
            std::fs::write(tool, b"fake tool\n").expect("write chdir candidate");
            let mut perms = std::fs::metadata(tool)
                .expect("stat chdir candidate")
                .permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(tool, perms).expect("chmod chdir candidate");
        }
        let script = root.join("script");
        std::fs::write(
            &script,
            format!(
                "#!/usr/bin/env -S --chdir {} PATH=bin tool\n",
                app_dir.display()
            ),
        )
        .expect("write chdir shebang script");
        let mut perms = std::fs::metadata(&script)
            .expect("stat chdir shebang script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod chdir shebang script");

        let mut unique = BTreeSet::new();
        insert_landlock_main_exec_path(&mut unique, &script.to_string_lossy())
            .expect("relative shebang PATH should resolve after env --chdir");
        let allowed = unique
            .iter()
            .map(|path| String::from_utf8_lossy(path).into_owned())
            .collect::<Vec<_>>();

        assert!(
            allowed.contains(&expected_tool.canonicalize().unwrap().display().to_string()),
            "expected env --chdir target to be allowed: {allowed:?}"
        );
        assert!(
            !allowed.contains(&wrong_tool.canonicalize().unwrap().display().to_string()),
            "relative shebang PATH must be resolved after chdir: {allowed:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn main_exec_auto_allow_prefers_executable_path_candidate() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-main-exec-path-{}-{nanos}",
            std::process::id(),
        ));
        let first_dir = root.join("first");
        let second_dir = root.join("second");
        std::fs::create_dir_all(&first_dir).expect("create first PATH dir");
        std::fs::create_dir_all(&second_dir).expect("create second PATH dir");
        let first_tool = first_dir.join("tool");
        let second_tool = second_dir.join("tool");
        std::fs::write(&first_tool, b"not executable\n").expect("write first tool");
        std::fs::write(&second_tool, b"executable\n").expect("write second tool");
        let mut perms = std::fs::metadata(&first_tool)
            .expect("stat first tool")
            .permissions();
        perms.set_mode(0o644);
        std::fs::set_permissions(&first_tool, perms).expect("chmod first tool");
        let mut perms = std::fs::metadata(&second_tool)
            .expect("stat second tool")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&second_tool, perms).expect("chmod second tool");
        let path = std::env::join_paths([&first_dir, &second_dir]).expect("join PATH");

        let _path = PathEnvGuard::set(path);
        let resolved =
            resolve_main_exec_allow_path_candidate("tool").expect("resolve main exec candidate");

        assert_eq!(resolved, Some(second_tool));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn exec_path_resolution_uses_default_path_when_path_unset() {
        let _path = PathEnvGuard::unset();

        let auto = resolve_main_exec_allow_path_candidate("sh")
            .expect("resolve main exec candidate")
            .expect("default exec path should find sh");
        let explicit =
            resolve_exec_allow_path_candidate("sh", None).expect("resolve explicit exec allow");

        assert_eq!(auto.file_name().and_then(|name| name.to_str()), Some("sh"));
        assert_eq!(
            explicit.file_name().and_then(|name| name.to_str()),
            Some("sh")
        );
    }

    #[test]
    fn explicit_exec_allow_still_rejects_missing_program() {
        let mut unique = BTreeSet::new();

        let err = insert_landlock_exec_path(
            &mut unique,
            "/definitely/missing/tino-test-binary",
            None,
        )
        .expect_err("explicit missing exec allow path must fail");

        assert!(
            format!("{err:#}").contains("canonicalize exec allow path"),
            "unexpected error: {err:#}"
        );
    }

    #[test]
    fn explicit_exec_allow_rejects_missing_env_shebang_command() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let missing = format!("definitely-missing-tino-interpreter-{}-{nanos}", std::process::id());
        let root = std::env::temp_dir().join(format!(
            "tino-explicit-exec-missing-env-{}-{nanos}",
            std::process::id(),
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create explicit env shebang test dir");
        let script = root.join("script");
        std::fs::write(&script, format!("#!/usr/bin/env {missing}\n"))
            .expect("write explicit env shebang script");
        let mut perms = std::fs::metadata(&script)
            .expect("stat explicit env shebang script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod explicit env shebang script");

        let mut unique = BTreeSet::new();
        let err = insert_landlock_exec_path(&mut unique, &script.to_string_lossy(), None)
            .expect_err("explicit exec allow should strictly validate shebang dependencies");

        assert!(
            format!("{err:#}").contains(&missing),
            "unexpected error: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn explicit_exec_allow_rejects_missing_env_path_assignment_command() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-explicit-exec-env-path-{}-{nanos}",
            std::process::id(),
        ));
        let shebang_path_dir = root.join("shebang-path");
        std::fs::create_dir_all(&shebang_path_dir).expect("create explicit shebang PATH dir");
        let script = root.join("script");
        std::fs::write(
            &script,
            format!(
                "#!/usr/bin/env -S -i PATH={} python3\n",
                shebang_path_dir.display()
            ),
        )
        .expect("write explicit env PATH shebang script");
        let mut perms = std::fs::metadata(&script)
            .expect("stat explicit env PATH shebang script")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("chmod explicit env PATH shebang script");

        let mut unique = BTreeSet::new();
        let err = insert_landlock_exec_path(&mut unique, &script.to_string_lossy(), None)
            .expect_err("explicit exec allow should reject missing shebang PATH command");

        assert!(
            format!("{err:#}").contains("from shebang PATH"),
            "unexpected error: {err:#}"
        );
        let _ = std::fs::remove_dir_all(&root);
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

    #[test]
    fn wait_loop_continues_after_non_terminal_child_statuses() {
        assert_eq!(
            wait_status_loop_action(WaitStatus::Stopped(Pid::from_raw(11), SIGTERM as i32)),
            WaitLoop::Continue
        );
        assert_eq!(
            wait_status_loop_action(WaitStatus::Continued(Pid::from_raw(11))),
            WaitLoop::Continue
        );
        assert_eq!(
            wait_status_loop_action(WaitStatus::StillAlive),
            WaitLoop::Break
        );
    }

    #[test]
    fn huge_grace_period_does_not_panic() {
        let _deadline = ShutdownDeadline::after(Instant::now(), u64::MAX);
    }

    #[test]
    fn never_shutdown_deadline_uses_max_poll_timeout() {
        assert_eq!(ShutdownDeadline::Never.poll_timeout(), PollTimeout::MAX);
    }

    #[test]
    fn parse_elf_interpreter_rejects_overflowing_program_header_offset() {
        let mut bytes = minimal_elf64();
        bytes[32..40].copy_from_slice(&u64::MAX.to_le_bytes());

        let err = parse_elf_interpreter(&bytes).expect_err("overflowing program header offset");
        let message = format!("{err:#}");
        assert!(
            message.contains("ELF program header offset overflow"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn parse_elf_interpreter_rejects_tiny_program_header_entries() {
        let mut bytes = minimal_elf64();
        bytes[54..56].copy_from_slice(&1u16.to_le_bytes());

        let err = parse_elf_interpreter(&bytes).expect_err("tiny program header entry");
        let message = format!("{err:#}");
        assert!(
            message.contains("ELF program header entry too small"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn parse_elf_interpreter_rejects_overflowing_interpreter_segment() {
        let mut bytes = minimal_elf64();
        let ph = 64;
        bytes[ph..ph + 4].copy_from_slice(&3u32.to_le_bytes());
        bytes[ph + 8..ph + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[ph + 32..ph + 40].copy_from_slice(&16u64.to_le_bytes());

        let err = parse_elf_interpreter(&bytes).expect_err("overflowing interpreter segment");
        let message = format!("{err:#}");
        assert!(
            message.contains("ELF interpreter segment offset overflow"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn parse_shebang_exec_paths_detects_direct_interpreter() {
        assert_eq!(
            parse_shebang_exec_paths(b"#!/bin/sh -e\necho ok\n"),
            vec!["/bin/sh"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_detects_env_command() {
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env python3\nprint('ok')\n"),
            vec!["/usr/bin/env", "python3"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_detects_env_split_command() {
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S python3 -u\nprint('ok')\n"),
            vec!["/usr/bin/env", "python3"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -vS python3 -u\nprint('ok')\n"),
            vec!["/usr/bin/env", "python3"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_detects_env_options_and_assignments() {
        assert_eq!(
            parse_shebang_exec_paths(
                b"#!/usr/bin/env -S SERVICE_ENV=test -u OLD python3 -u\nprint('ok')\n"
            ),
            vec!["/usr/bin/env", "python3"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env --split-string=--chdir /tmp /bin/sh\n"),
            vec!["/usr/bin/env", "/bin/sh"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S -- NAME=value -tool --flag\n"),
            vec!["/usr/bin/env", "-tool"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S -iu OLD /bin/sh -c true\n"),
            vec!["/usr/bin/env", "/bin/sh"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S -ia NAME /bin/sh -c true\n"),
            vec!["/usr/bin/env", "/bin/sh"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_ignores_invalid_env_options() {
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S --not-an-env-option /bin/sh\n"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S -q /bin/sh\n"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S --help /bin/sh\n"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env -S -0 /bin/sh\n"),
            vec!["/usr/bin/env"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_keeps_env_argument_without_split() {
        assert_eq!(
            parse_shebang_exec_paths(b"#!/usr/bin/env python3 -u\nprint('ok')\n"),
            vec!["/usr/bin/env", "python3 -u"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_handles_env_split_quotes() {
        assert_eq!(
            parse_shebang_exec_paths(
                br#"#!/usr/bin/env -S --argv0 "shell alias" /bin/sh -e
"#
            ),
            vec!["/usr/bin/env", "/bin/sh"]
        );
        assert_eq!(
            parse_shebang_exec_paths(br"#!/usr/bin/env -S python3\_-u
"),
            vec!["/usr/bin/env", "python3"]
        );
        assert_eq!(
            parse_shebang_exec_paths(br#"#!/usr/bin/env -S "/bin/with\_space" -e
"#),
            vec!["/usr/bin/env", "/bin/with space"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_handles_env_split_variables() {
        let _env = EnvVarGuard::set(
            "TINO_TEST_ENV_SHEBANG_COMMAND",
            OsString::from("/bin/sh"),
        );
        assert_eq!(
            parse_shebang_exec_paths(
                b"#!/usr/bin/env -S ${TINO_TEST_ENV_SHEBANG_COMMAND} -c true\n"
            ),
            vec!["/usr/bin/env", "/bin/sh"]
        );
        assert_eq!(
            parse_shebang_exec_paths(
                br"#!/usr/bin/env -S '${TINO_TEST_ENV_SHEBANG_COMMAND}' -c true
"
            ),
            vec!["/usr/bin/env", "${TINO_TEST_ENV_SHEBANG_COMMAND}"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_omits_unset_env_split_variables() {
        let _env = EnvVarGuard::unset("TINO_TEST_ENV_SHEBANG_COMMAND");
        assert_eq!(
            parse_shebang_exec_paths(
                b"#!/usr/bin/env -S ${TINO_TEST_ENV_SHEBANG_COMMAND} /bin/sh\n"
            ),
            vec!["/usr/bin/env", "/bin/sh"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_preserves_empty_env_split_variables() {
        let _env = EnvVarGuard::set("TINO_TEST_ENV_SHEBANG_COMMAND", OsString::new());
        assert_eq!(
            parse_shebang_exec_paths(
                b"#!/usr/bin/env -S ${TINO_TEST_ENV_SHEBANG_COMMAND} /bin/sh\n"
            ),
            vec!["/usr/bin/env", ""]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_keeps_env_split_variable_values_atomic() {
        let _env = EnvVarGuard::set(
            "TINO_TEST_ENV_SHEBANG_COMMAND",
            OsString::from("/bin/sh -c"),
        );
        assert_eq!(
            parse_shebang_exec_paths(
                b"#!/usr/bin/env -S ${TINO_TEST_ENV_SHEBANG_COMMAND} true\n"
            ),
            vec!["/usr/bin/env", "/bin/sh -c"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_ignores_invalid_env_split_string() {
        assert_eq!(
            parse_shebang_exec_paths(br"#!/usr/bin/env -S /bin/sh\ x
"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(br#"#!/usr/bin/env -S "/bin/sh -e
"#),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(br"#!/usr/bin/env -S # /bin/sh
"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(br"#!/usr/bin/env -S $TINO_TEST_ENV_SHEBANG_COMMAND /bin/sh
"),
            vec!["/usr/bin/env"]
        );
        assert_eq!(
            parse_shebang_exec_paths(
                br"#!/usr/bin/env -S ${TINO_TEST_ENV_SHEBANG_COMMAND:-/bin/sh}
"
            ),
            vec!["/usr/bin/env"]
        );
    }

    #[test]
    fn parse_shebang_exec_paths_uses_env_path_assignment() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-env-shebang-path-{}-{nanos}",
            std::process::id(),
        ));
        let bin_dir = root.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create env PATH dir");
        let tool = bin_dir.join("python3");
        std::fs::write(&tool, b"fake python\n").expect("write fake python");
        let mut perms = std::fs::metadata(&tool)
            .expect("stat fake python")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool, perms).expect("chmod fake python");
        let shebang = format!(
            "#!/usr/bin/env -S -i PATH={} python3 -u\nprint('ok')\n",
            bin_dir.display()
        );

        assert_eq!(
            parse_shebang_exec_paths(shebang.as_bytes()),
            vec!["/usr/bin/env".to_string(), tool.display().to_string()]
        );

        let after_double_dash = format!(
            "#!/usr/bin/env -S -- PATH={} python3 -u\nprint('ok')\n",
            bin_dir.display()
        );
        assert_eq!(
            parse_shebang_exec_paths(after_double_dash.as_bytes()),
            vec!["/usr/bin/env".to_string(), tool.display().to_string()]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_shebang_exec_paths_uses_env_chdir_for_relative_path() {
        use std::os::unix::fs::PermissionsExt;
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "tino-env-shebang-chdir-{}-{nanos}",
            std::process::id(),
        ));
        let app_dir = root.join("app");
        let bin_dir = app_dir.join("bin");
        std::fs::create_dir_all(&bin_dir).expect("create chdir PATH dir");
        let tool = bin_dir.join("tool");
        std::fs::write(&tool, b"fake tool\n").expect("write chdir tool");
        let mut perms = std::fs::metadata(&tool)
            .expect("stat chdir tool")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&tool, perms).expect("chmod chdir tool");
        let shebang = format!(
            "#!/usr/bin/env -S --chdir {} PATH=bin tool\n",
            app_dir.display()
        );

        assert_eq!(
            parse_shebang_exec_paths(shebang.as_bytes()),
            vec!["/usr/bin/env".to_string(), tool.display().to_string()]
        );

        let inline_short = format!(
            "#!/usr/bin/env -S--chdir {} PATH=bin tool\n",
            app_dir.display()
        );
        assert_eq!(
            parse_shebang_exec_paths(inline_short.as_bytes()),
            vec!["/usr/bin/env".to_string(), tool.display().to_string()]
        );

        let inline_long = format!(
            "#!/usr/bin/env --split-string=--chdir {} PATH=bin tool\n",
            app_dir.display()
        );
        assert_eq!(
            parse_shebang_exec_paths(inline_long.as_bytes()),
            vec!["/usr/bin/env".to_string(), tool.display().to_string()]
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn minimal_elf64() -> Vec<u8> {
        let mut bytes = vec![0u8; 120];
        bytes[0..4].copy_from_slice(b"\x7FELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[32..40].copy_from_slice(&64u64.to_le_bytes());
        bytes[54..56].copy_from_slice(&56u16.to_le_bytes());
        bytes[56..58].copy_from_slice(&1u16.to_le_bytes());
        bytes
    }
}
