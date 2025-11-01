//! tino - Tiny Init, No Overhead (production-grade)
//!
//! Build (static):  cargo build --release --target x86_64-unknown-linux-musl

#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod platform;

use anyhow::Result;
use clap::Parser;

pub(crate) const LICENSE_TEXT: &str = include_str!("../LICENSE");

fn main() -> Result<()> {
    let cli = cli::Cli::parse();

    if cli.license {
        print!("{LICENSE_TEXT}");
        return Ok(());
    }
    if cli.cmd.is_empty() {
        eprintln!("missing CMD (use --help)");
        std::process::exit(1);
    }

    platform::run(cli)
}
