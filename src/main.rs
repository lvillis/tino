use clap::Parser;
use tino::{Cli, run};
use tracing::error;

fn main() {
    let cli = Cli::parse();

    let exit_code = match run(cli) {
        Ok(code) => code,
        Err(err) => {
            error!(error = %format_args!("{err:#}"), "tino failed");
            1
        }
    };

    std::process::exit(exit_code);
}
