//! Library entry points for `tino`.
//!
//! `tino` is primarily a command-line tool: a tiny init process (PID 1) for
//! Docker, Kubernetes, and other container workloads. The library surface is
//! intentionally small and mirrors the binary's runtime behavior.
//!
//! The main entry point is [`run`], which executes a parsed [`Cli`] value.
//! Linux-specific restrictions such as Landlock are configured through CLI
//! flags and follow the same semantics as the `tino` binary.
//!
//! # Example
//!
//! ```no_run
//! use clap::Parser;
//! use tino::{Cli, run};
//!
//! let cli = Cli::parse_from(["tino", "--", "/usr/bin/sleep", "10"]);
//! let exit_code = run(cli)?;
//! # let _ = exit_code;
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! This crate is binary-first. Internal helper APIs are not part of the
//! stable public interface unless they are explicitly documented here.

#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod platform;
mod signals;

/// Parsed `tino` command-line configuration.
///
/// This type is derived with `clap` and is intended to be constructed through
/// [`clap::Parser`] or [`clap::CommandFactory`], rather than by manually
/// filling every field.
pub use cli::{Cli, WritePreset};

/// Bundled project license text.
///
/// This is the same text printed by the `--license` CLI flag.
pub const LICENSE_TEXT: &str = include_str!("../LICENSE");

/// Execute `tino` with a parsed [`Cli`] configuration.
///
/// Returns the final process exit code that should be used by the caller.
/// Errors are reserved for configuration, setup, or runtime failures that
/// prevent normal supervision from completing.
///
/// In typical usage, the `tino` binary calls this function and then exits with
/// the returned code.
pub fn run(cli: Cli) -> anyhow::Result<i32> {
    platform::run(cli)
}

#[doc(hidden)]
pub mod bench_support {
    use anyhow::Result;

    pub fn resolve_command_args(cmd: &[String], expand_env: bool) -> Result<Vec<String>> {
        crate::platform::bench_resolve_command_args(cmd, expand_env)
    }

    #[cfg(target_os = "linux")]
    pub fn parse_shebang_interpreter(bytes: &[u8]) -> Option<String> {
        crate::platform::bench_parse_shebang_interpreter(bytes)
    }

    #[cfg(target_os = "linux")]
    pub fn parse_elf_interpreter(bytes: &[u8]) -> Result<Option<String>> {
        crate::platform::bench_parse_elf_interpreter(bytes)
    }
}
