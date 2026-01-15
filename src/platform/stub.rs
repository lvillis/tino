use crate::cli::Cli;
use anyhow::{Result, bail};

pub(super) fn run_impl(_cli: Cli, _expect_zero: super::ExitCodeRemap) -> Result<i32> {
    bail!(
        "tino supports Unix-like targets only. Build and test inside a Linux container or VM \
         (see README requirements)."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_cli() -> Cli {
        Cli {
            subreaper: false,
            pdeath: None,
            verbosity: 0,
            warn_on_reap: false,
            pgroup_kill: false,
            remap_exit: Vec::new(),
            grace_ms: 500,
            landlock: false,
            landlock_writable: Vec::new(),
            landlock_profile: None,
            landlock_warn_only: false,
            landlock_no_dev: false,
            license: false,
            subreaper_env: None,
            pgroup_env: None,
            verbosity_env: None,
            cmd: vec!["/bin/true".into()],
        }
    }

    #[test]
    fn stub_reports_linux_requirement() {
        let cli = base_cli();
        let err = run_impl(cli, [false; 256]).unwrap_err();
        let message = format!("{err}");
        assert!(
            message.contains("supports Unix-like targets"),
            "unexpected stub message: {message}"
        );
    }
}
