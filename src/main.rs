//! tino - Tiny Init, No Overhead (production-grade)
//!
//! Build (static):  cargo build --release --target x86_64-unknown-linux-musl

#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod platform;

use anyhow::Result;

pub(crate) const LICENSE_TEXT: &str = include_str!("../LICENSE");

fn main() -> Result<()> {
    platform::run()
}
