#![deny(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use tino::{Cli, run};

fn main() {
    let cli = Cli::parse();

    let exit_code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            std::hint::cold_path();
            eprintln!("ERROR tino: {err:#}");
            1
        }
    };

    std::process::exit(exit_code);
}
