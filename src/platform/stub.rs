use crate::{Result, bail, cli::Cli};

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
            cmd: vec!["/bin/true".into()],
            ..Cli::default()
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
