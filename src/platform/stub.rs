use crate::{LICENSE_TEXT, cli::Cli};
use anyhow::{Result, bail};
use clap::Parser;
use std::process;

pub fn run() -> Result<()> {
    let cli = Cli::parse();

    if cli.license {
        print!("{LICENSE_TEXT}");
        process::exit(0);
    }
    if cli.cmd.is_empty() {
        bail!("missing CMD (use --help)");
    }

    bail!(
        "tino currently supports Unix-like targets only. Build and run inside a Linux container."
    );
}
