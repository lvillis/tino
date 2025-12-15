use crate::{LICENSE_TEXT, cli::Cli};
use anyhow::{Result, bail};
use once_cell::sync::OnceCell;
use std::collections::HashSet;
use tracing::{debug, warn};
use tracing_subscriber::{filter::EnvFilter, fmt};

#[cfg(not(target_os = "linux"))]
mod stub;
#[cfg(target_os = "linux")]
mod unix;

static LOGGER: OnceCell<()> = OnceCell::new();

pub fn run(mut cli: Cli) -> Result<i32> {
    if cli.license {
        print!("{LICENSE_TEXT}");
        return Ok(0);
    }

    let overrides = apply_env_overrides(&mut cli);
    debug_assert!(
        !cli.cmd.is_empty(),
        "CLI parsing should ensure at least one CMD argument"
    );
    if cli.cmd.is_empty() {
        bail!("missing CMD (use --help)");
    }

    let verbosity = cli.resolved_verbosity();
    init_logging(verbosity);
    overrides.emit();

    let expect_zero: HashSet<u8> = cli.remap_exit.iter().copied().collect();
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
    if let Some(raw) = cli.subreaper_env.as_deref() {
        match interpret_env_flag(raw) {
            Ok(enabled) => {
                cli.subreaper = enabled;
                log.subreaper_env = Some(enabled);
            }
            Err(value) => log.invalid_flags.push(("TINI_SUBREAPER", value)),
        }
    }
    if let Some(raw) = cli.pgroup_env.as_deref() {
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
            .with_target(false)
            .without_time()
            .try_init()
        {
            warn!(
                error = %e,
                "logging initialization failed; continuing with existing dispatcher"
            );
        }
    });
}

#[cfg(target_os = "linux")]
fn run_impl(cli: Cli, expect_zero: HashSet<u8>) -> Result<i32> {
    unix::run_impl(cli, expect_zero)
}

#[cfg(not(target_os = "linux"))]
fn run_impl(cli: Cli, expect_zero: HashSet<u8>) -> Result<i32> {
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
}
