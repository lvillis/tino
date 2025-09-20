use clap::Parser;

#[derive(Parser, Debug)]
#[command(author, version, about)]
pub struct Cli {
    #[arg(short = 's', long)]
    pub subreaper: bool,
    #[arg(short = 'p')]
    pub pdeath: Option<String>,
    #[arg(short = 'v', action = clap::ArgAction::Count)]
    pub verbosity: u8,
    #[arg(short = 'w')]
    pub warn_on_reap: bool,
    #[arg(short = 'g')]
    pub pgroup_kill: bool,
    #[arg(short = 'e', value_parser = clap::value_parser!(u8).range(0..=255))]
    pub remap_exit: Vec<u8>,
    #[arg(short = 't', long, default_value_t = 500)]
    pub grace_ms: u64,
    #[arg(short = 'l')]
    pub license: bool,
    #[arg(value_name = "CMD", trailing_var_arg = true)]
    pub cmd: Vec<String>,
}
