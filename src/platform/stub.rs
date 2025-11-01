use crate::{LICENSE_TEXT, cli::Cli};
use anyhow::{Result, bail};

pub fn run(cli: Cli) -> Result<()> {
    if cli.license {
        print!("{LICENSE_TEXT}");
        return Ok(());
    }

    if cli.cmd.is_empty() {
        bail!("missing CMD (use --help)");
    }

    bail!(
        "tino currently supports Unix-like targets only. Build and run inside a Linux container."
    );
}
