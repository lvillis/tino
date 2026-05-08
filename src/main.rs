#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::io::Write;
use tino::{Cli, run};

fn main() {
    let cli = Cli::parse();

    let exit_code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            std::hint::cold_path();
            let _ = writeln!(std::io::stderr().lock(), "ERROR tino: {err:#}");
            1
        }
    };

    std::process::exit(exit_code);
}
