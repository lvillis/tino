use crate::{
    Context, Error, LICENSE_TEXT, Result, bail,
    cli::{Cli, DEFAULT_CONFIG_PATH},
    logging,
};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

cfg_select! {
    target_os = "linux" => {
        pub(crate) mod unix;
        use unix as platform_impl;
    }
    _ => {
        mod stub;
        use stub as platform_impl;
    }
}

pub(crate) type ExitCodeRemap = [bool; 256];

pub fn run(mut cli: Cli) -> Result<i32> {
    validate_control_mode(&cli)?;
    if cli.license {
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(LICENSE_TEXT.as_bytes())
            .context("write stdout")?;
        stdout.flush().context("flush stdout")?;
        return Ok(0);
    }
    if cli.check_config {
        if !cli.cmd.is_empty() {
            bail!("--check-config does not accept CMD");
        }
        let config =
            Cli::load_required_default_config().map_err(|err| Error::msg(err.to_string()))?;
        validate_config(&config)?;
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "tino: config OK: {DEFAULT_CONFIG_PATH}").context("write stdout")?;
        stdout.flush().context("flush stdout")?;
        return Ok(0);
    }
    if cli.write_config {
        if !cli.cmd.is_empty() {
            bail!("--write-config does not accept CMD");
        }
        validate_config(&cli)?;
        write_default_config(&cli)?;
        let config =
            Cli::load_required_default_config().map_err(|err| Error::msg(err.to_string()))?;
        validate_config(&config)?;
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "tino: config written: {DEFAULT_CONFIG_PATH}").context("write stdout")?;
        stdout.flush().context("flush stdout")?;
        return Ok(0);
    }
    if cli.print_config {
        if !cli.cmd.is_empty() {
            bail!("--print-config does not accept CMD");
        }
        let config_text = cli
            .config_text()
            .map_err(|err| Error::msg(err.to_string()))?;
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(config_text.as_bytes())
            .context("write stdout")?;
        stdout.flush().context("flush stdout")?;
        return Ok(0);
    }

    let origins = ExplainOrigins {
        subreaper: cli.subreaper,
        pgroup_kill: cli.pgroup_kill,
        verbosity: cli.verbosity,
    };
    let overrides = apply_env_overrides(&mut cli);
    let warn_implies_subreaper = cli.warn_on_reap && !cli.subreaper;
    if warn_implies_subreaper {
        cli.subreaper = true;
    }

    let verbosity = cli.resolved_verbosity();
    init_logging(verbosity);
    if cli.explain {
        return explain(cli, &origins, &overrides, warn_implies_subreaper);
    }
    overrides.emit();
    if warn_implies_subreaper {
        logging::debug(format_args!("subreaper enabled via --warn-on-reap"));
    }

    if cli.cmd.is_empty() {
        bail!("missing CMD (use --help)");
    }

    let expect_zero = build_exit_remap(&cli.remap_exit);
    run_impl(cli, expect_zero)
}

fn validate_control_mode(cli: &Cli) -> Result<()> {
    if cli.no_config && cli.check_config {
        bail!("--no-config cannot be used with --check-config");
    }

    let modes = [
        (cli.license, "--license"),
        (cli.check_config, "--check-config"),
        (cli.write_config, "--write-config"),
        (cli.print_config, "--print-config"),
        (cli.explain, "--explain"),
    ];
    let mut selected = modes
        .iter()
        .filter_map(|&(enabled, name)| enabled.then_some(name));
    let Some(first) = selected.next() else {
        return Ok(());
    };
    if let Some(second) = selected.next() {
        bail!("{first} cannot be used with {second}");
    }
    if cli.check_config
        && let Some(option) = check_config_inline_option(cli)
    {
        bail!("--check-config does not accept {option}");
    }
    Ok(())
}

fn check_config_inline_option(cli: &Cli) -> Option<&'static str> {
    if cli.subreaper {
        return Some("--subreaper");
    }
    if cli.pdeath.is_some() {
        return Some("-p");
    }
    if cli.verbosity > 0 {
        return Some("-v");
    }
    if cli.warn_on_reap {
        return Some("--warn-on-reap");
    }
    if cli.pgroup_kill {
        return Some("--pgroup-kill");
    }
    if !cli.remap_exit.is_empty() {
        return Some("--remap-exit");
    }
    if cli.grace_ms != 500 {
        return Some("--grace-ms");
    }
    if cli.write_restrict {
        return Some("--write-restrict");
    }
    if !cli.write_allow.is_empty() {
        return Some("--write-allow");
    }
    if !cli.write_preset.is_empty() {
        return Some("--write-preset");
    }
    if cli.write_warn_only {
        return Some("--write-warn-only");
    }
    if cli.write_no_dev {
        return Some("--write-no-dev");
    }
    if !cli.bind_tcp_allow.is_empty() {
        return Some("--bind-tcp-allow");
    }
    if !cli.connect_tcp_allow.is_empty() {
        return Some("--connect-tcp-allow");
    }
    if cli.scope_signals {
        return Some("--scope-signals");
    }
    if cli.scope_abstract_unix {
        return Some("--scope-abstract-unix");
    }
    if !cli.exec_allow.is_empty() {
        return Some("--exec-allow");
    }
    if !cli.device_ioctl_allow.is_empty() {
        return Some("--device-ioctl-allow");
    }
    if cli.expand_env {
        return Some("--expand-env");
    }
    None
}

pub(crate) fn bench_resolve_command_args(cmd: &[String], expand_env: bool) -> Result<Vec<String>> {
    cfg_select! {
        target_os = "linux" => {
            platform_impl::bench_resolve_command_args(cmd, expand_env)
        }
        _ => {
            let _ = (cmd, expand_env);
            bail!("bench support is only available on Linux")
        }
    }
}

pub(crate) fn bench_parse_shebang_interpreter(bytes: &[u8]) -> Option<String> {
    cfg_select! {
        target_os = "linux" => {
            unix::bench_parse_shebang_interpreter(bytes)
        }
        _ => {
            let _ = bytes;
            None
        }
    }
}

pub(crate) fn bench_parse_elf_interpreter(bytes: &[u8]) -> Result<Option<String>> {
    cfg_select! {
        target_os = "linux" => {
            unix::bench_parse_elf_interpreter(bytes)
        }
        _ => {
            let _ = bytes;
            bail!("bench support is only available on Linux")
        }
    }
}

#[derive(Default)]
struct EnvOverrideLog {
    subreaper_env: Option<(&'static str, bool)>,
    pgroup_env: Option<(&'static str, bool)>,
    verbosity_env: Option<(&'static str, u8)>,
    invalid_flags: Vec<(&'static str, String)>,
    verbosity_error: Option<(&'static str, String, String)>,
}

struct ExplainOrigins {
    subreaper: bool,
    pgroup_kill: bool,
    verbosity: u8,
}

struct ExplainPlatform {
    effective_cmd: Vec<String>,
    write_restrict: Option<ExplainWriteRestrict>,
    tcp_restrict: Option<ExplainTcpRestrict>,
    ipc_scope: Option<ExplainIpcScope>,
    exec_restrict: Option<ExplainExecRestrict>,
    device_ioctl_restrict: Option<ExplainDeviceIoctlRestrict>,
}

struct ExplainWriteRestrict {
    warn_only: bool,
    no_dev: bool,
    preset_names: Vec<String>,
    writable_dirs: Vec<String>,
}

struct ExplainTcpRestrict {
    warn_only: bool,
    bind_allow_ports: Vec<u16>,
    connect_allow_ports: Vec<u16>,
}

struct ExplainIpcScope {
    warn_only: bool,
    signals: bool,
    abstract_unix: bool,
}

struct ExplainExecRestrict {
    warn_only: bool,
    allow_paths: Vec<String>,
}

struct ExplainDeviceIoctlRestrict {
    warn_only: bool,
    allow_paths: Vec<String>,
}

impl EnvOverrideLog {
    fn emit(&self) {
        if let Some((name, enabled)) = self.subreaper_env {
            if enabled {
                logging::debug(format_args!("subreaper enabled via {name}"));
            } else {
                logging::debug(format_args!("subreaper disabled via {name}"));
            }
        }
        if let Some((name, enabled)) = self.pgroup_env {
            if enabled {
                logging::debug(format_args!("process group kill enabled via {name}"));
            } else {
                logging::debug(format_args!("process group kill disabled via {name}"));
            }
        }
        if let Some((name, level)) = self.verbosity_env {
            logging::debug(format_args!("verbosity sourced from {name}: {level}"));
        }
        for (env, value) in &self.invalid_flags {
            logging::warn(format_args!("invalid boolean override: {env}={value}"));
        }
        if let Some((name, value, error)) = &self.verbosity_error {
            logging::warn(format_args!("invalid {name} '{value}': {error}"));
        }
    }
}

fn apply_env_overrides(cli: &mut Cli) -> EnvOverrideLog {
    let mut log = EnvOverrideLog::default();
    if !cli.subreaper
        && let Some((name, raw)) = env_override(&["TINO_SUBREAPER", "TINI_SUBREAPER"])
    {
        match interpret_env_flag(&raw) {
            Ok(enabled) => {
                cli.subreaper = enabled;
                log.subreaper_env = Some((name, enabled));
            }
            Err(value) => log.invalid_flags.push((name, value)),
        }
    }
    if !cli.pgroup_kill
        && let Some((name, raw)) =
            env_override(&["TINO_KILL_PROCESS_GROUP", "TINI_KILL_PROCESS_GROUP"])
    {
        match interpret_env_flag(&raw) {
            Ok(enabled) => {
                cli.pgroup_kill = enabled;
                log.pgroup_env = Some((name, enabled));
            }
            Err(value) => log.invalid_flags.push((name, value)),
        }
    }
    if cli.verbosity == 0
        && let Some((name, raw)) = env_override(&["TINO_VERBOSITY", "TINI_VERBOSITY"])
    {
        let trimmed = raw.trim();
        match trimmed.parse::<u8>() {
            Ok(parsed) => {
                cli.verbosity = parsed.min(3);
                log.verbosity_env = Some((name, cli.verbosity));
            }
            Err(err) => {
                log.verbosity_error = Some((name, raw, err.to_string()));
            }
        }
    }
    log
}

fn env_override(names: &[&'static str]) -> Option<(&'static str, String)> {
    names.iter().find_map(|&name| {
        std::env::var_os(name).map(|value| (name, value.to_string_lossy().into_owned()))
    })
}

fn interpret_env_flag(raw: &str) -> std::result::Result<bool, String> {
    let owned = raw.to_string();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(owned);
    }
    match trimmed.parse::<u8>() {
        Ok(raw) if let Ok(enabled) = bool::try_from(raw) => return Ok(enabled),
        Ok(_) => return Err(owned),
        Err(_) => {}
    }
    if trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("yes")
        || trimmed.eq_ignore_ascii_case("on")
    {
        return Ok(true);
    }
    if trimmed.eq_ignore_ascii_case("false")
        || trimmed.eq_ignore_ascii_case("no")
        || trimmed.eq_ignore_ascii_case("off")
    {
        return Ok(false);
    }
    Err(owned)
}

fn explain(
    cli: Cli,
    origins: &ExplainOrigins,
    overrides: &EnvOverrideLog,
    warn_implies_subreaper: bool,
) -> Result<i32> {
    let platform = collect_explain_platform(&cli)?;
    let mut out = String::new();

    let mut line = |args: std::fmt::Arguments<'_>| {
        let _ = out.write_fmt(args);
        let _ = out.write_char('\n');
    };

    line(format_args!("mode: explain"));
    line(format_args!("subreaper: {}", cli.subreaper));
    line(format_args!(
        "subreaper.source: {}",
        subreaper_source(origins, overrides, warn_implies_subreaper)
    ));
    line(format_args!(
        "pdeath: {}",
        cli.pdeath.as_deref().unwrap_or("none")
    ));
    line(format_args!("verbosity: {}", cli.resolved_verbosity()));
    line(format_args!(
        "verbosity.source: {}",
        verbosity_source(origins, overrides)
    ));
    line(format_args!("warn_on_reap: {}", cli.warn_on_reap));
    line(format_args!("pgroup_kill: {}", cli.pgroup_kill));
    line(format_args!(
        "pgroup_kill.source: {}",
        pgroup_kill_source(origins, overrides)
    ));
    line(format_args!("grace_ms: {}", cli.grace_ms));
    line(format_args!("remap_exit: {:?}", cli.remap_exit));
    line(format_args!("expand_env: {}", cli.expand_env));
    line(format_args!("command.present: {}", !cli.cmd.is_empty()));
    line(format_args!("command.original: {:?}", cli.cmd));
    line(format_args!(
        "command.effective: {:?}",
        platform.effective_cmd
    ));

    if let Some(write_restrict) = platform.write_restrict {
        line(format_args!("write_restrict.enabled: true"));
        line(format_args!("write_restrict.backend: landlock"));
        line(format_args!(
            "write_restrict.presets: {:?}",
            write_restrict.preset_names
        ));
        line(format_args!(
            "write_restrict.warn_only: {}",
            write_restrict.warn_only
        ));
        line(format_args!(
            "write_restrict.dev_writable: {}",
            !write_restrict.no_dev
        ));
        line(format_args!(
            "write_restrict.allow_dirs: {:?}",
            write_restrict.writable_dirs
        ));
    } else {
        line(format_args!("write_restrict.enabled: false"));
    }

    if let Some(tcp_restrict) = platform.tcp_restrict {
        line(format_args!("tcp_restrict.enabled: true"));
        line(format_args!("tcp_restrict.backend: landlock"));
        line(format_args!(
            "tcp_restrict.warn_only: {}",
            tcp_restrict.warn_only
        ));
        line(format_args!(
            "tcp_restrict.bind_allow_ports: {:?}",
            tcp_restrict.bind_allow_ports
        ));
        line(format_args!(
            "tcp_restrict.connect_allow_ports: {:?}",
            tcp_restrict.connect_allow_ports
        ));
    } else {
        line(format_args!("tcp_restrict.enabled: false"));
    }

    if let Some(ipc_scope) = platform.ipc_scope {
        line(format_args!("ipc_scope.enabled: true"));
        line(format_args!("ipc_scope.backend: landlock"));
        line(format_args!("ipc_scope.warn_only: {}", ipc_scope.warn_only));
        line(format_args!("ipc_scope.signals: {}", ipc_scope.signals));
        line(format_args!(
            "ipc_scope.abstract_unix: {}",
            ipc_scope.abstract_unix
        ));
    } else {
        line(format_args!("ipc_scope.enabled: false"));
    }

    if let Some(exec_restrict) = platform.exec_restrict {
        line(format_args!("exec_restrict.enabled: true"));
        line(format_args!("exec_restrict.backend: landlock"));
        line(format_args!(
            "exec_restrict.warn_only: {}",
            exec_restrict.warn_only
        ));
        line(format_args!(
            "exec_restrict.allow_paths: {:?}",
            exec_restrict.allow_paths
        ));
    } else {
        line(format_args!("exec_restrict.enabled: false"));
    }

    if let Some(device_ioctl_restrict) = platform.device_ioctl_restrict {
        line(format_args!("device_ioctl_restrict.enabled: true"));
        line(format_args!("device_ioctl_restrict.backend: landlock"));
        line(format_args!(
            "device_ioctl_restrict.warn_only: {}",
            device_ioctl_restrict.warn_only
        ));
        line(format_args!(
            "device_ioctl_restrict.allow_paths: {:?}",
            device_ioctl_restrict.allow_paths
        ));
    } else {
        line(format_args!("device_ioctl_restrict.enabled: false"));
    }

    if !overrides.invalid_flags.is_empty() || overrides.verbosity_error.is_some() {
        line(format_args!("warnings:"));
        for (env, value) in &overrides.invalid_flags {
            line(format_args!("- invalid boolean override {env}={value:?}"));
        }
        if let Some((name, value, error)) = &overrides.verbosity_error {
            line(format_args!("- invalid {name}={value:?}: {error}"));
        }
    }

    let mut stdout = io::stdout().lock();
    stdout.write_all(out.as_bytes()).context("write stdout")?;
    stdout.flush().context("flush stdout")?;
    Ok(0)
}

fn subreaper_source(
    origins: &ExplainOrigins,
    overrides: &EnvOverrideLog,
    warn_implies_subreaper: bool,
) -> String {
    if origins.subreaper {
        "configured".into()
    } else if warn_implies_subreaper {
        "--warn-on-reap".into()
    } else if let Some((name, _)) = overrides.subreaper_env {
        format!("env:{name}")
    } else {
        "default".into()
    }
}

fn pgroup_kill_source(origins: &ExplainOrigins, overrides: &EnvOverrideLog) -> String {
    if origins.pgroup_kill {
        "configured".into()
    } else if let Some((name, _)) = overrides.pgroup_env {
        format!("env:{name}")
    } else {
        "default".into()
    }
}

fn verbosity_source(origins: &ExplainOrigins, overrides: &EnvOverrideLog) -> String {
    if origins.verbosity > 0 {
        "configured".into()
    } else if let Some((name, _)) = overrides.verbosity_env {
        format!("env:{name}")
    } else {
        "default".into()
    }
}

pub(crate) fn init_logging(v: u8) {
    logging::init(v);
}

fn build_exit_remap(codes: &[u8]) -> ExitCodeRemap {
    let mut map = [false; 256];
    for &code in codes {
        map[code as usize] = true;
    }
    map
}

fn validate_config(cli: &Cli) -> Result<()> {
    let _ = collect_explain_platform(cli)?;
    Ok(())
}

fn write_default_config(cli: &Cli) -> Result<()> {
    let path = Path::new(DEFAULT_CONFIG_PATH);
    let parent = path
        .parent()
        .context("default config path has no parent directory")?;
    fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    let config_text = cli
        .config_text()
        .map_err(|err| Error::msg(err.to_string()))?;
    write_file_atomically(path, config_text.as_bytes())?;
    Ok(())
}

fn write_file_atomically(path: &Path, content: &[u8]) -> Result<()> {
    let permissions = final_config_permissions(path)?;
    let temp_path = write_temp_file(path, content, permissions.as_ref())?;
    if let Err(err) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(err)
            .with_context(|| format!("rename {} to {}", temp_path.display(), path.display()));
    }
    sync_parent_dir(path)?;
    Ok(())
}

fn final_config_permissions(path: &Path) -> Result<Option<fs::Permissions>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(metadata.permissions())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(default_config_permissions()),
        Err(err) => Err(err).with_context(|| format!("inspect {}", path.display())),
    }
}

#[cfg(target_family = "unix")]
fn default_config_permissions() -> Option<fs::Permissions> {
    use std::os::unix::fs::PermissionsExt;

    Some(fs::Permissions::from_mode(0o644))
}

#[cfg(not(target_family = "unix"))]
fn default_config_permissions() -> Option<fs::Permissions> {
    None
}

#[cfg(target_family = "unix")]
fn sync_parent_dir(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .context("config path has no parent directory")?;
    fs::File::open(parent)
        .with_context(|| format!("open {}", parent.display()))?
        .sync_all()
        .with_context(|| format!("sync {}", parent.display()))
}

#[cfg(not(target_family = "unix"))]
fn sync_parent_dir(_path: &Path) -> Result<()> {
    Ok(())
}

fn write_temp_file(
    path: &Path,
    content: &[u8],
    permissions: Option<&fs::Permissions>,
) -> Result<PathBuf> {
    const MAX_TEMP_ATTEMPTS: u32 = 100;

    for attempt in 0..MAX_TEMP_ATTEMPTS {
        let temp_path = temp_path_for_attempt(path, attempt)?;
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(target_family = "unix")]
        {
            use std::os::unix::fs::OpenOptionsExt;

            options.mode(0o600);
        }
        let mut file = match options.open(&temp_path) {
            Ok(file) => file,
            Err(err) => {
                if err.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(err).with_context(|| format!("create {}", temp_path.display()));
            }
        };
        if let Err(err) = file.write_all(content) {
            let _ = fs::remove_file(&temp_path);
            return Err(err).with_context(|| format!("write {}", temp_path.display()));
        }
        if let Some(permissions) = permissions
            && let Err(err) = file.set_permissions(permissions.clone())
        {
            let _ = fs::remove_file(&temp_path);
            return Err(err).with_context(|| format!("set permissions on {}", temp_path.display()));
        }
        if let Err(err) = file.sync_all() {
            let _ = fs::remove_file(&temp_path);
            return Err(err).with_context(|| format!("sync {}", temp_path.display()));
        }
        return Ok(temp_path);
    }

    bail!(
        "create temporary config file for {} after {} attempts",
        path.display(),
        MAX_TEMP_ATTEMPTS
    )
}

#[cfg(test)]
fn temp_path_for(path: &Path) -> Result<PathBuf> {
    temp_path_for_attempt(path, 0)
}

fn temp_path_for_attempt(path: &Path, attempt: u32) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("config path has no valid UTF-8 file name")?;
    let mut temp_path = path.to_path_buf();
    temp_path.set_file_name(format!(
        ".{file_name}.tmp.{}.{}",
        std::process::id(),
        attempt
    ));
    Ok(temp_path)
}

fn collect_explain_platform(cli: &Cli) -> Result<ExplainPlatform> {
    cfg_select! {
        target_os = "linux" => {
            let landlock = unix::explain_landlock_config(cli)?;
            Ok(ExplainPlatform {
                effective_cmd: unix::explain_effective_command(&cli.cmd, cli.expand_env)?,
                write_restrict: landlock
                    .as_ref()
                    .filter(|config| config.write_requested)
                    .map(|config| ExplainWriteRestrict {
                        warn_only: config.warn_only,
                        no_dev: config.no_dev,
                        preset_names: config.preset_names.clone(),
                        writable_dirs: config.writable_dirs.clone(),
                    }),
                tcp_restrict: landlock
                    .as_ref()
                    .filter(|config| {
                        !config.bind_tcp_ports.is_empty() || !config.connect_tcp_ports.is_empty()
                    })
                    .map(|config| ExplainTcpRestrict {
                        warn_only: config.warn_only,
                        bind_allow_ports: config.bind_tcp_ports.clone(),
                        connect_allow_ports: config.connect_tcp_ports.clone(),
                    }),
                ipc_scope: landlock
                    .as_ref()
                    .filter(|config| config.scope_signals || config.scope_abstract_unix)
                    .map(|config| ExplainIpcScope {
                        warn_only: config.warn_only,
                        signals: config.scope_signals,
                        abstract_unix: config.scope_abstract_unix,
                    }),
                exec_restrict: landlock
                    .as_ref()
                    .filter(|config| !config.exec_allow_paths.is_empty())
                    .map(|config| ExplainExecRestrict {
                        warn_only: config.warn_only,
                        allow_paths: config.exec_allow_paths.clone(),
                    }),
                device_ioctl_restrict: landlock
                    .as_ref()
                    .filter(|config| !config.device_ioctl_allow_paths.is_empty())
                    .map(|config| ExplainDeviceIoctlRestrict {
                        warn_only: config.warn_only,
                        allow_paths: config.device_ioctl_allow_paths.clone(),
                    }),
            })
        }
        _ => {
            let _ = cli;
            bail!(
                "tino supports Linux targets only. Build and test inside a Linux container or VM \
                 (see README requirements)."
            )
        }
    }
}

fn run_impl(cli: Cli, expect_zero: ExitCodeRemap) -> Result<i32> {
    platform_impl::run_impl(cli, expect_zero)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_boolean_override_is_rejected() {
        assert!(interpret_env_flag("   ").is_err());
    }

    fn base_cli() -> Cli {
        Cli {
            cmd: vec!["/bin/true".into()],
            ..Cli::default()
        }
    }

    #[test]
    fn init_logging_is_idempotent() {
        let _lock = crate::logging::test_lock();

        init_logging(0);
        init_logging(1);
        crate::logging::reset_for_test();
    }

    #[test]
    fn control_modes_are_mutually_exclusive() {
        let mut cli = base_cli();
        cli.check_config = true;
        cli.write_config = true;

        let err = validate_control_mode(&cli).expect_err("conflicting modes must fail");

        assert!(
            format!("{err}").contains("--check-config cannot be used with --write-config"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn no_config_cannot_check_config() {
        let mut cli = base_cli();
        cli.no_config = true;
        cli.check_config = true;

        let err = validate_control_mode(&cli).expect_err("contradictory config modes must fail");

        assert!(
            format!("{err}").contains("--no-config cannot be used with --check-config"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn check_config_rejects_inline_runtime_options() {
        let cli = Cli {
            check_config: true,
            write_allow: vec!["/tmp".into()],
            ..Cli::default()
        };

        let err = validate_control_mode(&cli).expect_err("inline config options must fail");

        assert!(
            format!("{err}").contains("--check-config does not accept --write-allow"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn env_boolean_overrides_take_effect() {
        let mut cli = base_cli();
        let _env =
            EnvVarsGuard::set(&[("TINI_SUBREAPER", "true"), ("TINI_KILL_PROCESS_GROUP", "0")]);

        let log = apply_env_overrides(&mut cli);
        assert!(cli.subreaper);
        assert!(!cli.pgroup_kill);
        assert_eq!(log.subreaper_env, Some(("TINI_SUBREAPER", true)));
        assert_eq!(log.pgroup_env, Some(("TINI_KILL_PROCESS_GROUP", false)));
        assert!(log.invalid_flags.is_empty());
    }

    #[test]
    fn native_env_overrides_win_over_tini_compatibility_names() {
        let mut cli = base_cli();
        let _env = EnvVarsGuard::set(&[
            ("TINO_SUBREAPER", "true"),
            ("TINI_SUBREAPER", "false"),
            ("TINO_KILL_PROCESS_GROUP", "1"),
            ("TINI_KILL_PROCESS_GROUP", "0"),
            ("TINO_VERBOSITY", "2"),
            ("TINI_VERBOSITY", "3"),
        ]);

        let log = apply_env_overrides(&mut cli);
        assert!(cli.subreaper);
        assert!(cli.pgroup_kill);
        assert_eq!(cli.verbosity, 2);
        assert_eq!(log.subreaper_env, Some(("TINO_SUBREAPER", true)));
        assert_eq!(log.pgroup_env, Some(("TINO_KILL_PROCESS_GROUP", true)));
        assert_eq!(log.verbosity_env, Some(("TINO_VERBOSITY", 2)));
    }

    #[test]
    fn invalid_boolean_env_is_reported() {
        let mut cli = base_cli();
        let _env = EnvVarsGuard::set(&[("TINI_SUBREAPER", "maybe")]);

        let log = apply_env_overrides(&mut cli);
        assert_eq!(log.invalid_flags, vec![("TINI_SUBREAPER", "maybe".into())]);
        assert!(!cli.subreaper);
    }

    #[test]
    fn verbosity_env_applies_when_flags_absent() {
        let mut cli = base_cli();
        let _env = EnvVarsGuard::set(&[("TINI_VERBOSITY", "3")]);

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 3);
        assert_eq!(log.verbosity_env, Some(("TINI_VERBOSITY", 3)));
        assert!(log.verbosity_error.is_none());
    }

    #[test]
    fn invalid_verbosity_is_logged_without_panicking() {
        let mut cli = base_cli();
        let _env = EnvVarsGuard::set(&[("TINI_VERBOSITY", "noise")]);

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 0);
        assert!(log.verbosity_env.is_none());
        assert_eq!(
            log.verbosity_error,
            Some((
                "TINI_VERBOSITY",
                "noise".into(),
                "invalid digit found in string".into()
            ))
        );
    }

    #[test]
    fn verbosity_flag_wins_over_env() {
        let mut cli = base_cli();
        cli.verbosity = 2;
        let _env = EnvVarsGuard::set(&[("TINI_VERBOSITY", "3")]);

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 2);
        assert!(log.verbosity_env.is_none());
    }

    #[test]
    fn boolean_flags_win_over_env() {
        let mut cli = base_cli();
        cli.subreaper = true;
        cli.pgroup_kill = true;
        let _env = EnvVarsGuard::set(&[
            ("TINI_SUBREAPER", "false"),
            ("TINI_KILL_PROCESS_GROUP", "0"),
        ]);

        let log = apply_env_overrides(&mut cli);
        assert!(cli.subreaper);
        assert!(cli.pgroup_kill);
        assert!(log.subreaper_env.is_none());
        assert!(log.pgroup_env.is_none());
    }

    #[test]
    fn explain_sources_use_configured_for_merged_cli_state() {
        let origins = ExplainOrigins {
            subreaper: true,
            pgroup_kill: true,
            verbosity: 1,
        };
        let overrides = EnvOverrideLog::default();

        assert_eq!(subreaper_source(&origins, &overrides, false), "configured");
        assert_eq!(pgroup_kill_source(&origins, &overrides), "configured");
        assert_eq!(verbosity_source(&origins, &overrides), "configured");
    }

    fn unique_test_dir(name: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("tino-{name}-{}-{nanos}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn atomic_write_replaces_existing_file() {
        let dir = unique_test_dir("atomic-write-replace");
        let path = dir.join("tino.conf");
        fs::write(&path, b"old\n").expect("write old config");

        write_file_atomically(&path, b"new\n").expect("atomic write");

        assert_eq!(fs::read_to_string(&path).expect("read new config"), "new\n");
        assert!(
            !temp_path_for(&path).expect("temp path").exists(),
            "temporary config file should be renamed away"
        );
        fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn atomic_write_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_test_dir("atomic-write-permissions");
        let path = dir.join("tino.conf");
        fs::write(&path, b"old\n").expect("write old config");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("set old config permissions");

        write_file_atomically(&path, b"new\n").expect("atomic write");

        let mode = fs::metadata(&path)
            .expect("stat new config")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn atomic_write_uses_readable_permissions_for_new_config() {
        use std::os::unix::fs::PermissionsExt;

        let dir = unique_test_dir("atomic-write-new-permissions");
        let path = dir.join("tino.conf");

        write_file_atomically(&path, b"new\n").expect("atomic write");

        let mode = fs::metadata(&path)
            .expect("stat new config")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o644);
        fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn atomic_write_does_not_follow_existing_temp_symlink() {
        use std::os::unix::fs::symlink;

        let dir = unique_test_dir("atomic-write-symlink");
        let path = dir.join("tino.conf");
        let outside = dir.join("outside");
        fs::write(&path, b"old\n").expect("write old config");
        fs::write(&outside, b"outside\n").expect("write outside marker");
        symlink(&outside, temp_path_for(&path).expect("temp path")).expect("create temp symlink");

        write_file_atomically(&path, b"new\n").expect("atomic write");

        assert_eq!(fs::read_to_string(&path).expect("read new config"), "new\n");
        assert_eq!(
            fs::read_to_string(&outside).expect("read outside marker"),
            "outside\n"
        );
        fs::remove_dir_all(&dir).expect("remove temp dir");
    }

    use std::env;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvVarsGuard {
        originals: Vec<(&'static str, Option<String>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvVarsGuard {
        fn set(vars: &[(&'static str, &str)]) -> Self {
            const ENV_NAMES: &[&str] = &[
                "TINO_SUBREAPER",
                "TINI_SUBREAPER",
                "TINO_KILL_PROCESS_GROUP",
                "TINI_KILL_PROCESS_GROUP",
                "TINO_VERBOSITY",
                "TINI_VERBOSITY",
            ];

            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("env lock poisoned");

            let mut originals = Vec::with_capacity(ENV_NAMES.len());
            for key in ENV_NAMES {
                let _ = originals.push_mut((*key, env::var(*key).ok()));
                unsafe {
                    env::remove_var(*key);
                }
            }
            for (key, value) in vars {
                unsafe {
                    env::set_var(*key, *value);
                }
            }

            Self {
                originals,
                _lock: lock,
            }
        }
    }

    impl Drop for EnvVarsGuard {
        fn drop(&mut self) {
            for (key, original) in &self.originals {
                if let Some(value) = original {
                    unsafe {
                        env::set_var(*key, value);
                    }
                } else {
                    unsafe {
                        env::remove_var(*key);
                    }
                }
            }
        }
    }
}
