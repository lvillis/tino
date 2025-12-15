//! tino - Tiny Init, No Overhead (production-grade)
//!
//! Build (static):  cargo build --release --target x86_64-unknown-linux-musl

#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod platform;
mod signals;

use clap::Parser;
use tracing::error;

pub(crate) const LICENSE_TEXT: &str = include_str!("../LICENSE");

fn main() {
    let cli = cli::Cli::parse();

    let exit_code = match platform::run(cli) {
        Ok(code) => code,
        Err(err) => {
            error!(error = %err, "tino failed");
            1
        }
    };

    std::process::exit(exit_code);
}
