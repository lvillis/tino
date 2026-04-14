use crate::{LICENSE_TEXT, cli::Cli};
use anyhow::{Context, Result, bail};
use std::fmt::Write as FmtWrite;
use std::io::{self, Write};
use std::sync::OnceLock;
use tracing::{debug, warn};
use tracing_subscriber::{filter::EnvFilter, fmt};

#[cfg(not(target_os = "linux"))]
mod stub;
#[cfg(target_os = "linux")]
mod unix;

static LOGGER: OnceLock<()> = OnceLock::new();

pub(crate) type ExitCodeRemap = [bool; 256];

pub fn run(mut cli: Cli) -> Result<i32> {
    if cli.license {
        print!("{LICENSE_TEXT}");
        let _ = io::stdout().flush();
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
        debug!("subreaper enabled via --warn-on-reap");
    }

    if cli.cmd.is_empty() {
        bail!("missing CMD (use --help)");
    }

    let expect_zero = build_exit_remap(&cli.remap_exit);
    run_impl(cli, expect_zero)
}

#[derive(Default)]
struct EnvOverrideLog {
    subreaper_env: Option<bool>,
    pgroup_env: Option<bool>,
    verbosity_env: Option<u8>,
    invalid_flags: Vec<(&'static str, String)>,
    verbosity_error: Option<(String, String)>,
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
        if let Some(enabled) = self.subreaper_env {
            if enabled {
                debug!("subreaper enabled via TINI_SUBREAPER");
            } else {
                debug!("subreaper disabled via TINI_SUBREAPER");
            }
        }
        if let Some(enabled) = self.pgroup_env {
            if enabled {
                debug!("process group kill enabled via TINI_KILL_PROCESS_GROUP");
            } else {
                debug!("process group kill disabled via TINI_KILL_PROCESS_GROUP");
            }
        }
        if let Some(level) = self.verbosity_env {
            debug!(verbosity = level, "verbosity sourced from TINI_VERBOSITY");
        }
        for (env, value) in &self.invalid_flags {
            warn!(env = *env, value = %value, "invalid boolean override");
        }
        if let Some((value, error)) = &self.verbosity_error {
            warn!(value = %value, error = %error, "invalid TINI_VERBOSITY");
        }
    }
}

fn apply_env_overrides(cli: &mut Cli) -> EnvOverrideLog {
    let mut log = EnvOverrideLog::default();
    if !cli.subreaper
        && let Some(raw) = cli.subreaper_env.as_deref()
    {
        match interpret_env_flag(raw) {
            Ok(enabled) => {
                cli.subreaper = enabled;
                log.subreaper_env = Some(enabled);
            }
            Err(value) => log.invalid_flags.push(("TINI_SUBREAPER", value)),
        }
    }
    if !cli.pgroup_kill
        && let Some(raw) = cli.pgroup_env.as_deref()
    {
        match interpret_env_flag(raw) {
            Ok(enabled) => {
                cli.pgroup_kill = enabled;
                log.pgroup_env = Some(enabled);
            }
            Err(value) => log.invalid_flags.push(("TINI_KILL_PROCESS_GROUP", value)),
        }
    }
    if cli.verbosity == 0
        && let Some(raw) = cli.verbosity_env.as_deref()
    {
        let trimmed = raw.trim();
        match trimmed.parse::<u8>() {
            Ok(parsed) => {
                cli.verbosity = parsed.min(3);
                log.verbosity_env = Some(cli.verbosity);
            }
            Err(err) => {
                log.verbosity_error = Some((raw.to_string(), err.to_string()));
            }
        }
    }
    log
}

fn interpret_env_flag(raw: &str) -> std::result::Result<bool, String> {
    let owned = raw.to_string();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(owned);
    }
    if trimmed == "1"
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed.eq_ignore_ascii_case("yes")
        || trimmed.eq_ignore_ascii_case("on")
    {
        return Ok(true);
    }
    if trimmed == "0"
        || trimmed.eq_ignore_ascii_case("false")
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

    writeln!(&mut out, "mode: explain").expect("write to string");
    writeln!(&mut out, "subreaper: {}", cli.subreaper).expect("write to string");
    writeln!(
        &mut out,
        "subreaper.source: {}",
        subreaper_source(origins, overrides, warn_implies_subreaper)
    )
    .expect("write to string");
    writeln!(
        &mut out,
        "pdeath: {}",
        cli.pdeath.as_deref().unwrap_or("none")
    )
    .expect("write to string");
    writeln!(&mut out, "verbosity: {}", cli.resolved_verbosity()).expect("write to string");
    writeln!(
        &mut out,
        "verbosity.source: {}",
        verbosity_source(origins, overrides)
    )
    .expect("write to string");
    writeln!(&mut out, "warn_on_reap: {}", cli.warn_on_reap).expect("write to string");
    writeln!(&mut out, "pgroup_kill: {}", cli.pgroup_kill).expect("write to string");
    writeln!(
        &mut out,
        "pgroup_kill.source: {}",
        pgroup_kill_source(origins, overrides)
    )
    .expect("write to string");
    writeln!(&mut out, "grace_ms: {}", cli.grace_ms).expect("write to string");
    writeln!(&mut out, "remap_exit: {:?}", cli.remap_exit).expect("write to string");
    writeln!(&mut out, "expand_env: {}", cli.expand_env).expect("write to string");
    writeln!(&mut out, "command.present: {}", !cli.cmd.is_empty()).expect("write to string");
    writeln!(&mut out, "command.original: {:?}", cli.cmd).expect("write to string");
    writeln!(&mut out, "command.effective: {:?}", platform.effective_cmd).expect("write to string");

    if let Some(write_restrict) = platform.write_restrict {
        writeln!(&mut out, "write_restrict.enabled: true").expect("write to string");
        writeln!(&mut out, "write_restrict.backend: landlock").expect("write to string");
        writeln!(
            &mut out,
            "write_restrict.presets: {:?}",
            write_restrict.preset_names
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "write_restrict.warn_only: {}",
            write_restrict.warn_only
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "write_restrict.dev_writable: {}",
            !write_restrict.no_dev
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "write_restrict.allow_dirs: {:?}",
            write_restrict.writable_dirs
        )
        .expect("write to string");
    } else {
        writeln!(&mut out, "write_restrict.enabled: false").expect("write to string");
    }

    if let Some(tcp_restrict) = platform.tcp_restrict {
        writeln!(&mut out, "tcp_restrict.enabled: true").expect("write to string");
        writeln!(&mut out, "tcp_restrict.backend: landlock").expect("write to string");
        writeln!(
            &mut out,
            "tcp_restrict.warn_only: {}",
            tcp_restrict.warn_only
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "tcp_restrict.bind_allow_ports: {:?}",
            tcp_restrict.bind_allow_ports
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "tcp_restrict.connect_allow_ports: {:?}",
            tcp_restrict.connect_allow_ports
        )
        .expect("write to string");
    } else {
        writeln!(&mut out, "tcp_restrict.enabled: false").expect("write to string");
    }

    if let Some(ipc_scope) = platform.ipc_scope {
        writeln!(&mut out, "ipc_scope.enabled: true").expect("write to string");
        writeln!(&mut out, "ipc_scope.backend: landlock").expect("write to string");
        writeln!(&mut out, "ipc_scope.warn_only: {}", ipc_scope.warn_only)
            .expect("write to string");
        writeln!(&mut out, "ipc_scope.signals: {}", ipc_scope.signals).expect("write to string");
        writeln!(
            &mut out,
            "ipc_scope.abstract_unix: {}",
            ipc_scope.abstract_unix
        )
        .expect("write to string");
    } else {
        writeln!(&mut out, "ipc_scope.enabled: false").expect("write to string");
    }

    if let Some(exec_restrict) = platform.exec_restrict {
        writeln!(&mut out, "exec_restrict.enabled: true").expect("write to string");
        writeln!(&mut out, "exec_restrict.backend: landlock").expect("write to string");
        writeln!(
            &mut out,
            "exec_restrict.warn_only: {}",
            exec_restrict.warn_only
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "exec_restrict.allow_paths: {:?}",
            exec_restrict.allow_paths
        )
        .expect("write to string");
    } else {
        writeln!(&mut out, "exec_restrict.enabled: false").expect("write to string");
    }

    if let Some(device_ioctl_restrict) = platform.device_ioctl_restrict {
        writeln!(&mut out, "device_ioctl_restrict.enabled: true").expect("write to string");
        writeln!(&mut out, "device_ioctl_restrict.backend: landlock").expect("write to string");
        writeln!(
            &mut out,
            "device_ioctl_restrict.warn_only: {}",
            device_ioctl_restrict.warn_only
        )
        .expect("write to string");
        writeln!(
            &mut out,
            "device_ioctl_restrict.allow_paths: {:?}",
            device_ioctl_restrict.allow_paths
        )
        .expect("write to string");
    } else {
        writeln!(&mut out, "device_ioctl_restrict.enabled: false").expect("write to string");
    }

    if !overrides.invalid_flags.is_empty() || overrides.verbosity_error.is_some() {
        writeln!(&mut out, "warnings:").expect("write to string");
        for (env, value) in &overrides.invalid_flags {
            writeln!(&mut out, "- invalid boolean override {env}={value:?}")
                .expect("write to string");
        }
        if let Some((value, error)) = &overrides.verbosity_error {
            writeln!(&mut out, "- invalid TINI_VERBOSITY={value:?}: {error}")
                .expect("write to string");
        }
    }

    print!("{out}");
    io::stdout().flush().context("flush stdout")?;
    Ok(0)
}

fn subreaper_source(
    origins: &ExplainOrigins,
    overrides: &EnvOverrideLog,
    warn_implies_subreaper: bool,
) -> &'static str {
    if origins.subreaper {
        "flag"
    } else if warn_implies_subreaper {
        "--warn-on-reap"
    } else if overrides.subreaper_env.is_some() {
        "env:TINI_SUBREAPER"
    } else {
        "default"
    }
}

fn pgroup_kill_source(origins: &ExplainOrigins, overrides: &EnvOverrideLog) -> &'static str {
    if origins.pgroup_kill {
        "flag"
    } else if overrides.pgroup_env.is_some() {
        "env:TINI_KILL_PROCESS_GROUP"
    } else {
        "default"
    }
}

fn verbosity_source(origins: &ExplainOrigins, overrides: &EnvOverrideLog) -> &'static str {
    if origins.verbosity > 0 {
        "flag"
    } else if overrides.verbosity_env.is_some() {
        "env:TINI_VERBOSITY"
    } else {
        "default"
    }
}

pub(crate) fn init_logging(v: u8) {
    let lvl = match v {
        0 => "info",
        1 => "debug",
        _ => "trace",
    };
    LOGGER.get_or_init(move || {
        let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(lvl));
        if let Err(e) = fmt::Subscriber::builder()
            .with_env_filter(filter)
            .with_target(true)
            .without_time()
            .with_writer(io::stderr)
            .try_init()
        {
            warn!(
                error = %e,
                "logging initialization failed; continuing with existing dispatcher"
            );
        }
    });
}

fn build_exit_remap(codes: &[u8]) -> ExitCodeRemap {
    let mut map = [false; 256];
    for &code in codes {
        map[code as usize] = true;
    }
    map
}

#[cfg(target_os = "linux")]
fn collect_explain_platform(cli: &Cli) -> Result<ExplainPlatform> {
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

#[cfg(not(target_os = "linux"))]
fn collect_explain_platform(_cli: &Cli) -> Result<ExplainPlatform> {
    bail!(
        "tino supports Unix-like targets only. Build and test inside a Linux container or VM \
         (see README requirements)."
    )
}

#[cfg(target_os = "linux")]
fn run_impl(cli: Cli, expect_zero: ExitCodeRemap) -> Result<i32> {
    unix::run_impl(cli, expect_zero)
}

#[cfg(not(target_os = "linux"))]
fn run_impl(cli: Cli, expect_zero: ExitCodeRemap) -> Result<i32> {
    stub::run_impl(cli, expect_zero)
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
    fn init_logging_is_idempotent() {
        init_logging(0);
        init_logging(1);
    }

    #[test]
    fn env_boolean_overrides_take_effect() {
        let mut cli = base_cli();
        cli.subreaper_env = Some("true".into());
        cli.pgroup_env = Some("0".into());

        let log = apply_env_overrides(&mut cli);
        assert!(cli.subreaper);
        assert!(!cli.pgroup_kill);
        assert_eq!(log.subreaper_env, Some(true));
        assert_eq!(log.pgroup_env, Some(false));
        assert!(log.invalid_flags.is_empty());
    }

    #[test]
    fn invalid_boolean_env_is_reported() {
        let mut cli = base_cli();
        cli.subreaper_env = Some("maybe".into());

        let log = apply_env_overrides(&mut cli);
        assert_eq!(log.invalid_flags, vec![("TINI_SUBREAPER", "maybe".into())]);
        assert!(!cli.subreaper);
    }

    #[test]
    fn verbosity_env_applies_when_flags_absent() {
        let mut cli = base_cli();
        cli.verbosity_env = Some("3".into());

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 3);
        assert_eq!(log.verbosity_env, Some(3));
        assert!(log.verbosity_error.is_none());
    }

    #[test]
    fn invalid_verbosity_is_logged_without_panicking() {
        let mut cli = base_cli();
        cli.verbosity_env = Some("noise".into());

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 0);
        assert!(log.verbosity_env.is_none());
        assert!(log.verbosity_error.is_some());
    }

    #[test]
    fn verbosity_flag_wins_over_env() {
        let mut cli = base_cli();
        cli.verbosity = 2;
        cli.verbosity_env = Some("3".into());

        let log = apply_env_overrides(&mut cli);
        assert_eq!(cli.verbosity, 2);
        assert!(log.verbosity_env.is_none());
    }

    #[test]
    fn boolean_flags_win_over_env() {
        let mut cli = base_cli();
        cli.subreaper = true;
        cli.subreaper_env = Some("false".into());
        cli.pgroup_kill = true;
        cli.pgroup_env = Some("0".into());

        let log = apply_env_overrides(&mut cli);
        assert!(cli.subreaper);
        assert!(cli.pgroup_kill);
        assert!(log.subreaper_env.is_none());
        assert!(log.pgroup_env.is_none());
    }
}
