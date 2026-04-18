//! tino library entry points and benchmark support.

#![deny(unsafe_op_in_unsafe_fn)]

mod cli;
mod platform;
mod signals;

pub use cli::Cli;

pub const LICENSE_TEXT: &str = include_str!("../LICENSE");

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
