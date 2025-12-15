use crate::signals::{SIGNAL_NAMES, canonical_signal_name};
use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(short = 's', long)]
    pub subreaper: bool,
    #[arg(short = 'p', value_parser = parse_signal, value_name = "SIG")]
    pub pdeath: Option<String>,
    #[arg(short = 'v', action = clap::ArgAction::Count)]
    pub verbosity: u8,
    #[arg(short = 'w')]
    pub warn_on_reap: bool,
    #[arg(short = 'g')]
    pub pgroup_kill: bool,
    #[arg(short = 'e', value_parser = clap::value_parser!(u8).range(0..=255))]
    pub remap_exit: Vec<u8>,
    #[arg(short = 't', long, default_value_t = 500)]
    pub grace_ms: u64,
    #[arg(short = 'l', long)]
    pub license: bool,
    #[arg(long = "subreaper-env", env = "TINI_SUBREAPER", hide = true)]
    pub subreaper_env: Option<String>,
    #[arg(long = "pgroup-kill-env", env = "TINI_KILL_PROCESS_GROUP", hide = true)]
    pub pgroup_env: Option<String>,
    #[arg(long = "verbosity-env", env = "TINI_VERBOSITY", hide = true)]
    pub verbosity_env: Option<String>,
    #[arg(value_name = "CMD", trailing_var_arg = true)]
    pub cmd: Vec<String>,
}

impl Cli {
    pub(crate) fn resolved_verbosity(&self) -> u8 {
        self.verbosity.min(3)
    }
}

fn parse_signal(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("signal name cannot be empty".into());
    }
    if let Some(name) = canonical_signal_name(trimmed) {
        Ok(format!("SIG{}", name))
    } else {
        Err(format!(
            "invalid signal '{raw}'; supported values: {}",
            SIGNAL_NAMES.join(", ")
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::env;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    #[test]
    fn parse_signal_accepts_known_variants() {
        assert_eq!(parse_signal("TERM").unwrap(), "SIGTERM");
        assert_eq!(parse_signal("sigterm").unwrap(), "SIGTERM");
        assert_eq!(parse_signal("SIGUSR1").unwrap(), "SIGUSR1");
    }

    #[test]
    fn parse_signal_rejects_unknown_values() {
        assert!(parse_signal("NOPE").is_err());
        assert!(parse_signal("").is_err());
    }

    struct EnvVarsGuard {
        originals: Vec<(&'static str, Option<String>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl EnvVarsGuard {
        fn set(vars: &[(&'static str, &str)]) -> Self {
            static ENV_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            let lock = ENV_LOCK
                .get_or_init(|| Mutex::new(()))
                .lock()
                .expect("env lock poisoned");

            let mut originals = Vec::with_capacity(vars.len());
            for (key, value) in vars {
                originals.push((*key, env::var(*key).ok()));
                // SAFETY: Mutating the process environment is unsafe in Rust 2024. We hold a
                // global mutex for the duration of this guard to ensure no other test in this
                // crate reads or writes the environment concurrently.
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

    #[test]
    fn env_values_are_captured() {
        let _env = EnvVarsGuard::set(&[
            ("TINI_SUBREAPER", "1"),
            ("TINI_KILL_PROCESS_GROUP", "false"),
            ("TINI_VERBOSITY", "2"),
        ]);
        let cli = Cli::try_parse_from(["tino", "--", "/bin/true"]).unwrap();
        assert_eq!(cli.subreaper_env.as_deref(), Some("1"));
        assert_eq!(cli.pgroup_env.as_deref(), Some("false"));
        assert_eq!(cli.verbosity_env.as_deref(), Some("2"));
    }
}
