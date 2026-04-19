use crate::signals::{SIGNAL_NAMES, canonical_signal_name};
use osarg::{Arg, Parser, count_flag, set_flag, standard};
use std::ffi::OsString;
use std::fmt;
use std::io::{self, Write};

const HELP_TEXT: &str = concat!(
    "usage: tino [OPTIONS] [--] CMD [ARGS...]\n\n",
    "options:\n",
    "  -s, --subreaper                 Enable PR_SET_CHILD_SUBREAPER\n",
    "  -p SIG                          Set PR_SET_PDEATHSIG (e.g. TERM, SIGTERM)\n",
    "  -v                              Increase log verbosity (repeatable)\n",
    "  -w, --warn-on-reap              Warn when reaping secondary child processes\n",
    "  -g, --pgroup-kill               Forward signals to the child's process group\n",
    "  -e, --remap-exit CODE           Remap child exit code to success (repeatable)\n",
    "  -t, --grace-ms MS               Grace period before SIGKILL (default: 500)\n",
    "      --write-restrict            Restrict child filesystem writes\n",
    "      --write-allow PATH          Allow writes beneath PATH (repeatable)\n",
    "      --write-preset PRESET       Add writable preset: tmp, runtime\n",
    "      --write-warn-only           Warn and continue when write restriction fails\n",
    "      --write-no-dev              Do not automatically allow /dev writes\n",
    "      --bind-tcp-allow PORT       Allow binding only on local TCP ports\n",
    "      --connect-tcp-allow PORT    Allow outbound TCP only to remote ports\n",
    "      --scope-signals             Restrict signal delivery to the same Landlock domain\n",
    "      --scope-abstract-unix       Restrict abstract UNIX socket connects to the same Landlock domain\n",
    "      --exec-allow PATH           Allow executing files beneath PATH\n",
    "      --device-ioctl-allow PATH   Allow device ioctl operations beneath PATH\n",
    "      --expand-env                Expand ${VAR} and ${VAR:-default} in child args\n",
    "      --explain                   Explain the effective configuration and exit\n",
    "  -l, --license                   Print license text and exit\n",
    "  -h, --help                      Show help\n",
    "  -V, --version                   Show version\n",
);

const VERSION_TEXT: &str = concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"), "\n");

/// Named presets for conservative writable directory allowlists.
///
/// These presets are only meaningful on Linux when Landlock-based write
/// restriction is enabled.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WritePreset {
    /// Allow `/tmp` and `/var/tmp`.
    Tmp,
    /// Allow `/tmp`, `/var/tmp`, and `/run`.
    Runtime,
}

impl WritePreset {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Tmp => "tmp",
            Self::Runtime => "runtime",
        }
    }

    fn parse(raw: &str) -> Result<Self, CliParseError> {
        match raw {
            "tmp" => Ok(Self::Tmp),
            "runtime" => Ok(Self::Runtime),
            _ => Err(CliParseError::message(format!(
                "invalid value for --write-preset: {raw} (expected tmp or runtime)"
            ))),
        }
    }
}

/// Kind of CLI parsing failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CliParseErrorKind {
    /// Argument parsing failed.
    Message,
    /// `--help` or `-h` was requested.
    Help,
    /// `--version` or `-V` was requested.
    Version,
}

/// Error returned by [`Cli::try_parse_from`].
#[derive(Debug)]
pub struct CliParseError {
    kind: CliParseErrorKind,
    message: String,
}

impl CliParseError {
    fn message(message: String) -> Self {
        std::hint::cold_path();
        Self {
            kind: CliParseErrorKind::Message,
            message,
        }
    }

    fn help() -> Self {
        Self {
            kind: CliParseErrorKind::Help,
            message: HELP_TEXT.to_string(),
        }
    }

    fn version() -> Self {
        Self {
            kind: CliParseErrorKind::Version,
            message: VERSION_TEXT.to_string(),
        }
    }

    /// Returns the parsing error category.
    #[must_use]
    pub const fn kind(&self) -> CliParseErrorKind {
        self.kind
    }

    fn print_and_exit(&self) -> ! {
        match self.kind {
            CliParseErrorKind::Help | CliParseErrorKind::Version => {
                print!("{}", self.message);
                let _ = io::stdout().flush();
                std::process::exit(0);
            }
            CliParseErrorKind::Message => {
                std::hint::cold_path();
                eprintln!("error: {}", self.message);
                eprintln!();
                eprint!("{HELP_TEXT}");
                let _ = io::stderr().flush();
                std::process::exit(2);
            }
        }
    }
}

impl fmt::Display for CliParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CliParseError {}

/// Parsed `tino` command-line configuration.
///
/// This struct mirrors the binary CLI and is the primary configuration type
/// accepted by [`crate::run`]. Most callers should construct it via
/// [`Cli::parse`], [`Cli::parse_from`], or [`Cli::try_parse_from`].
#[derive(Debug)]
pub struct Cli {
    /// Enable `PR_SET_CHILD_SUBREAPER` so this init can reap orphaned grandchildren.
    pub subreaper: bool,
    /// Set a parent-death signal via `PR_SET_PDEATHSIG` (e.g. `TERM`, `SIGTERM`).
    pub pdeath: Option<String>,
    /// Increase log verbosity (-v, -vv, -vvv).
    pub verbosity: u8,
    /// Emit a warning when reaping secondary child processes.
    pub warn_on_reap: bool,
    /// Forward signals to the child's process group (like `tini -g`).
    pub pgroup_kill: bool,
    /// Remap these child exit codes to success (repeatable).
    pub remap_exit: Vec<u8>,
    /// Grace period in milliseconds before escalating SIGTERM/SIGINT/SIGQUIT to SIGKILL.
    pub grace_ms: u64,
    /// Restrict child filesystem writes to explicitly allowed directories (Linux only).
    pub write_restrict: bool,
    /// Allow writes beneath this path when write restriction is enabled (repeatable).
    pub write_allow: Vec<String>,
    /// Add a conservative writable directory preset (`tmp` or `runtime`; repeatable).
    pub write_preset: Vec<WritePreset>,
    /// Continue even if requested Landlock restrictions cannot be applied (warn and continue).
    pub write_warn_only: bool,
    /// Do not automatically allow `/dev` writes (may break TTY/stdout).
    pub write_no_dev: bool,
    /// Allow binding TCP listeners only on these local ports (repeatable; Linux only).
    pub bind_tcp_allow: Vec<u16>,
    /// Allow outbound TCP connections only to these remote ports (repeatable; Linux only).
    pub connect_tcp_allow: Vec<u16>,
    /// Restrict signal delivery to processes within the same Landlock domain (Linux only).
    pub scope_signals: bool,
    /// Restrict abstract UNIX socket connects to the same Landlock domain (Linux only).
    pub scope_abstract_unix: bool,
    /// Allow executing files beneath this path when exec restriction is enabled (repeatable).
    pub exec_allow: Vec<String>,
    /// Allow device ioctl operations beneath this path (directory or device node; repeatable).
    pub device_ioctl_allow: Vec<String>,
    /// Expand `${VAR}` and `${VAR:-default}` in child command arguments; `$$` becomes `$`.
    pub expand_env: bool,
    /// Explain the effective configuration and command, then exit without running the child.
    pub explain: bool,
    /// Print license text and exit.
    pub license: bool,
    /// Child command and trailing arguments.
    pub cmd: Vec<String>,
}

impl Cli {
    /// Parses command-line arguments from the process environment.
    #[must_use]
    pub fn parse() -> Self {
        match Self::try_parse_from(std::env::args_os()) {
            Ok(cli) => cli,
            Err(err) => err.print_and_exit(),
        }
    }

    /// Parses command-line arguments from an arbitrary iterator.
    #[must_use]
    pub fn parse_from<I, T>(args: I) -> Self
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        match Self::try_parse_from(args) {
            Ok(cli) => cli,
            Err(err) => err.print_and_exit(),
        }
    }

    /// Tries to parse command-line arguments from an arbitrary iterator.
    pub fn try_parse_from<I, T>(args: I) -> Result<Self, CliParseError>
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        let mut argv = args.into_iter().map(Into::into);
        let _ = argv.next();
        let mut parser = Parser::new(argv);
        let mut cli = Self::default();

        while let Some(arg) = parser
            .next()
            .map_err(|err| CliParseError::message(err.to_string()))?
        {
            if let Some(flag) = standard::classify(arg) {
                return Err(match flag {
                    standard::Flag::Help => CliParseError::help(),
                    standard::Flag::Version => CliParseError::version(),
                });
            }

            match arg {
                Arg::Short('s') | Arg::Long("subreaper") => set_flag(&mut cli.subreaper),
                Arg::Short('p') => {
                    let raw = parser
                        .value()
                        .map_err(|err| CliParseError::message(err.to_string()))?;
                    cli.pdeath = Some(parse_signal(raw.to_str().map_err(from_osarg_error)?)?);
                }
                Arg::Short('v') => count_flag(&mut cli.verbosity),
                Arg::Short('w') | Arg::Long("warn-on-reap") => set_flag(&mut cli.warn_on_reap),
                Arg::Short('g') | Arg::Long("pgroup-kill") => set_flag(&mut cli.pgroup_kill),
                Arg::Short('e') | Arg::Long("remap-exit") => {
                    *cli.remap_exit.push_mut(0) = parser
                        .parse::<u8>()
                        .map_err(|err| CliParseError::message(err.to_string()))?;
                }
                Arg::Short('t') | Arg::Long("grace-ms") => {
                    cli.grace_ms = parser
                        .parse::<u64>()
                        .map_err(|err| CliParseError::message(err.to_string()))?;
                }
                Arg::Long("write-restrict") => set_flag(&mut cli.write_restrict),
                Arg::Long("write-allow") => {
                    *cli.write_allow.push_mut(String::new()) =
                        parse_string_value(&mut parser, "--write-allow")?;
                }
                Arg::Long("write-preset") => {
                    let preset = parse_string_value(&mut parser, "--write-preset")?;
                    *cli.write_preset.push_mut(WritePreset::Tmp) = WritePreset::parse(&preset)?;
                }
                Arg::Long("write-warn-only") => set_flag(&mut cli.write_warn_only),
                Arg::Long("write-no-dev") => set_flag(&mut cli.write_no_dev),
                Arg::Long("bind-tcp-allow") => {
                    *cli.bind_tcp_allow.push_mut(0) =
                        parse_u16_value(&mut parser, "--bind-tcp-allow")?;
                }
                Arg::Long("connect-tcp-allow") => {
                    *cli.connect_tcp_allow.push_mut(0) =
                        parse_u16_value(&mut parser, "--connect-tcp-allow")?;
                }
                Arg::Long("scope-signals") => set_flag(&mut cli.scope_signals),
                Arg::Long("scope-abstract-unix") => set_flag(&mut cli.scope_abstract_unix),
                Arg::Long("exec-allow") => {
                    *cli.exec_allow.push_mut(String::new()) =
                        parse_string_value(&mut parser, "--exec-allow")?;
                }
                Arg::Long("device-ioctl-allow") => {
                    *cli.device_ioctl_allow.push_mut(String::new()) =
                        parse_string_value(&mut parser, "--device-ioctl-allow")?;
                }
                Arg::Long("expand-env") => set_flag(&mut cli.expand_env),
                Arg::Long("explain") => set_flag(&mut cli.explain),
                Arg::Short('l') | Arg::Long("license") => set_flag(&mut cli.license),
                Arg::Value(value) => {
                    let _ = value;
                    let (command, remaining) = parser
                        .current_value_and_remaining()
                        .map_err(from_osarg_error)?;
                    *cli.cmd.push_mut(String::new()) = os_string_into_string(command)?;
                    cli.cmd.extend(
                        remaining
                            .into_iter()
                            .map(os_string_into_string)
                            .collect::<Result<Vec<_>, _>>()?,
                    );
                    return Ok(cli);
                }
                other => return Err(CliParseError::message(other.unexpected().to_string())),
            }
        }

        Ok(cli)
    }

    pub(crate) fn resolved_verbosity(&self) -> u8 {
        self.verbosity.min(3)
    }
}

impl Default for Cli {
    fn default() -> Self {
        Self {
            subreaper: false,
            pdeath: None,
            verbosity: 0,
            warn_on_reap: false,
            pgroup_kill: false,
            remap_exit: Vec::new(),
            grace_ms: 500,
            write_restrict: false,
            write_allow: Vec::new(),
            write_preset: Vec::new(),
            write_warn_only: false,
            write_no_dev: false,
            bind_tcp_allow: Vec::new(),
            connect_tcp_allow: Vec::new(),
            scope_signals: false,
            scope_abstract_unix: false,
            exec_allow: Vec::new(),
            device_ioctl_allow: Vec::new(),
            expand_env: false,
            explain: false,
            license: false,
            cmd: Vec::new(),
        }
    }
}

fn parse_signal(raw: &str) -> Result<String, CliParseError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(CliParseError::message("signal name cannot be empty".into()));
    }
    if let Some(name) = canonical_signal_name(trimmed) {
        Ok(format!("SIG{}", name))
    } else {
        Err(CliParseError::message(format!(
            "invalid signal '{raw}'; supported values: {}",
            SIGNAL_NAMES.join(", ")
        )))
    }
}

fn parse_string_value<I>(parser: &mut Parser<I>, option: &str) -> Result<String, CliParseError>
where
    I: Iterator<Item = OsString>,
{
    parser
        .value()
        .map_err(|err| CliParseError::message(format!("{option}: {err}")))?
        .to_str()
        .map(str::to_owned)
        .map_err(from_osarg_error)
}

fn parse_u16_value<I>(parser: &mut Parser<I>, option: &str) -> Result<u16, CliParseError>
where
    I: Iterator<Item = OsString>,
{
    parser
        .parse::<u16>()
        .map_err(|err| CliParseError::message(format!("{option}: {err}")))
}

fn os_string_into_string(value: OsString) -> Result<String, CliParseError> {
    value.into_string().map_err(|value| {
        CliParseError::message(format!(
            "argument is not valid UTF-8: {}",
            value.to_string_lossy()
        ))
    })
}

fn from_osarg_error(err: osarg::Error) -> CliParseError {
    CliParseError::message(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_family = "unix")]
    use std::os::unix::ffi::OsStringExt;

    type FlagCase<'a> = (&'a [&'a str], fn(&Cli) -> bool);

    fn parse_ok<I, T>(args: I) -> Cli
    where
        I: IntoIterator<Item = T>,
        T: Into<OsString>,
    {
        Cli::try_parse_from(args).expect("parse cli")
    }

    #[test]
    fn parse_signal_accepts_known_variants() {
        assert_eq!(parse_signal("TERM").unwrap(), "SIGTERM");
        assert_eq!(parse_signal("sigterm").unwrap(), "SIGTERM");
        assert_eq!(parse_signal("SIGUSR1").unwrap(), "SIGUSR1");
    }

    #[test]
    fn parse_signal_rejects_unknown_values() {
        assert!(parse_signal("NOPE").is_err());
        assert!(parse_signal("").is_err());
    }

    #[test]
    fn parse_counts_grouped_verbosity_and_collects_command_tail() {
        let cli =
            Cli::try_parse_from(["tino", "-vv", "--", "/bin/echo", "--value"]).expect("parse cli");
        assert_eq!(cli.verbosity, 2);
        assert_eq!(cli.cmd, vec!["/bin/echo", "--value"]);
    }

    #[test]
    fn parse_help_and_version_as_control_flow() {
        let help = Cli::try_parse_from(["tino", "--help"]).unwrap_err();
        assert_eq!(help.kind(), CliParseErrorKind::Help);

        let version = Cli::try_parse_from(["tino", "-V"]).unwrap_err();
        assert_eq!(version.kind(), CliParseErrorKind::Version);
    }

    #[test]
    fn parse_supports_short_and_long_flag_spellings() {
        let cases: &[FlagCase<'_>] = &[
            (&["tino", "-s"], |cli| cli.subreaper),
            (&["tino", "--subreaper"], |cli| cli.subreaper),
            (&["tino", "-w"], |cli| cli.warn_on_reap),
            (&["tino", "--warn-on-reap"], |cli| cli.warn_on_reap),
            (&["tino", "-g"], |cli| cli.pgroup_kill),
            (&["tino", "--pgroup-kill"], |cli| cli.pgroup_kill),
            (&["tino", "--write-restrict"], |cli| cli.write_restrict),
            (&["tino", "--write-warn-only"], |cli| cli.write_warn_only),
            (&["tino", "--write-no-dev"], |cli| cli.write_no_dev),
            (&["tino", "--scope-signals"], |cli| cli.scope_signals),
            (&["tino", "--scope-abstract-unix"], |cli| {
                cli.scope_abstract_unix
            }),
            (&["tino", "--expand-env"], |cli| cli.expand_env),
            (&["tino", "--explain"], |cli| cli.explain),
            (&["tino", "-l"], |cli| cli.license),
            (&["tino", "--license"], |cli| cli.license),
        ];

        for (args, predicate) in cases {
            let cli = parse_ok(*args);
            assert!(predicate(&cli), "flag did not parse as expected: {args:?}");
        }
    }

    #[test]
    fn parse_supports_value_option_spellings() {
        let cases: &[FlagCase<'_>] = &[
            (&["tino", "-p", "TERM"], |cli| {
                cli.pdeath.as_deref() == Some("SIGTERM")
            }),
            (&["tino", "-pTERM"], |cli| {
                cli.pdeath.as_deref() == Some("SIGTERM")
            }),
            (&["tino", "-e", "3"], |cli| cli.remap_exit == vec![3]),
            (&["tino", "--remap-exit", "3"], |cli| {
                cli.remap_exit == vec![3]
            }),
            (&["tino", "--remap-exit=3"], |cli| cli.remap_exit == vec![3]),
            (&["tino", "-t", "250"], |cli| cli.grace_ms == 250),
            (&["tino", "-t250"], |cli| cli.grace_ms == 250),
            (&["tino", "--grace-ms", "250"], |cli| cli.grace_ms == 250),
            (&["tino", "--grace-ms=250"], |cli| cli.grace_ms == 250),
            (&["tino", "--write-allow", "/tmp"], |cli| {
                cli.write_allow == vec!["/tmp"]
            }),
            (&["tino", "--write-allow=/tmp"], |cli| {
                cli.write_allow == vec!["/tmp"]
            }),
            (&["tino", "--write-preset", "tmp"], |cli| {
                cli.write_preset == vec![WritePreset::Tmp]
            }),
            (&["tino", "--write-preset=runtime"], |cli| {
                cli.write_preset == vec![WritePreset::Runtime]
            }),
            (&["tino", "--bind-tcp-allow", "80"], |cli| {
                cli.bind_tcp_allow == vec![80]
            }),
            (&["tino", "--bind-tcp-allow=80"], |cli| {
                cli.bind_tcp_allow == vec![80]
            }),
            (&["tino", "--connect-tcp-allow", "443"], |cli| {
                cli.connect_tcp_allow == vec![443]
            }),
            (&["tino", "--connect-tcp-allow=443"], |cli| {
                cli.connect_tcp_allow == vec![443]
            }),
            (&["tino", "--exec-allow", "/bin/sh"], |cli| {
                cli.exec_allow == vec!["/bin/sh"]
            }),
            (&["tino", "--exec-allow=/bin/sh"], |cli| {
                cli.exec_allow == vec!["/bin/sh"]
            }),
            (&["tino", "--device-ioctl-allow", "/dev/null"], |cli| {
                cli.device_ioctl_allow == vec!["/dev/null"]
            }),
            (&["tino", "--device-ioctl-allow=/dev/null"], |cli| {
                cli.device_ioctl_allow == vec!["/dev/null"]
            }),
        ];

        for (args, predicate) in cases {
            let cli = parse_ok(*args);
            assert!(
                predicate(&cli),
                "value option did not parse as expected: {args:?}"
            );
        }
    }

    #[test]
    fn parse_collects_repeatable_values_in_order() {
        let cli = parse_ok([
            "tino",
            "-e",
            "3",
            "--remap-exit=7",
            "--write-allow=/tmp",
            "--write-allow",
            "/run",
            "--bind-tcp-allow=80",
            "--bind-tcp-allow",
            "443",
            "--connect-tcp-allow=8080",
            "--connect-tcp-allow",
            "8443",
            "--exec-allow=/bin/sh",
            "--exec-allow",
            "/usr/bin/env",
            "--device-ioctl-allow=/dev/null",
            "--device-ioctl-allow",
            "/dev/pts",
        ]);

        assert_eq!(cli.remap_exit, vec![3, 7]);
        assert_eq!(cli.write_allow, vec!["/tmp", "/run"]);
        assert_eq!(cli.bind_tcp_allow, vec![80, 443]);
        assert_eq!(cli.connect_tcp_allow, vec![8080, 8443]);
        assert_eq!(cli.exec_allow, vec!["/bin/sh", "/usr/bin/env"]);
        assert_eq!(cli.device_ioctl_allow, vec!["/dev/null", "/dev/pts"]);
    }

    #[test]
    fn parse_accumulates_repeated_verbosity_flags() {
        let cases: &[(&[&str], u8)] = &[(&["tino", "-vvv"], 3), (&["tino", "-v", "-v"], 2)];

        for (args, expected) in cases {
            let cli = parse_ok(*args);
            assert_eq!(
                cli.verbosity, *expected,
                "unexpected verbosity for {args:?}"
            );
        }
    }

    #[test]
    fn parse_supports_short_long_attached_and_repeatable_options() {
        let cli = parse_ok([
            "tino",
            "-svgw",
            "-pTERM",
            "-e",
            "3",
            "--remap-exit=7",
            "-t250",
            "--write-restrict",
            "--write-allow",
            "/tmp",
            "--write-preset=runtime",
            "--write-warn-only",
            "--write-no-dev",
            "--bind-tcp-allow",
            "80",
            "--bind-tcp-allow=443",
            "--connect-tcp-allow",
            "8080",
            "--scope-signals",
            "--scope-abstract-unix",
            "--exec-allow",
            "/bin/sh",
            "--device-ioctl-allow",
            "/dev/null",
            "--expand-env",
            "--explain",
            "--license",
            "--",
            "/bin/echo",
            "hello",
        ]);

        assert!(cli.subreaper);
        assert_eq!(cli.pdeath.as_deref(), Some("SIGTERM"));
        assert_eq!(cli.verbosity, 1);
        assert!(cli.warn_on_reap);
        assert!(cli.pgroup_kill);
        assert_eq!(cli.remap_exit, vec![3, 7]);
        assert_eq!(cli.grace_ms, 250);
        assert!(cli.write_restrict);
        assert_eq!(cli.write_allow, vec!["/tmp"]);
        assert_eq!(cli.write_preset, vec![WritePreset::Runtime]);
        assert!(cli.write_warn_only);
        assert!(cli.write_no_dev);
        assert_eq!(cli.bind_tcp_allow, vec![80, 443]);
        assert_eq!(cli.connect_tcp_allow, vec![8080]);
        assert!(cli.scope_signals);
        assert!(cli.scope_abstract_unix);
        assert_eq!(cli.exec_allow, vec!["/bin/sh"]);
        assert_eq!(cli.device_ioctl_allow, vec!["/dev/null"]);
        assert!(cli.expand_env);
        assert!(cli.explain);
        assert!(cli.license);
        assert_eq!(cli.cmd, vec!["/bin/echo", "hello"]);
    }

    #[test]
    fn parse_first_positional_collects_remaining_command() {
        let cli = parse_ok(["tino", "--expand-env", "/bin/echo", "--literal-flag"]);

        assert!(cli.expand_env);
        assert_eq!(cli.cmd, vec!["/bin/echo", "--literal-flag"]);
    }

    #[test]
    fn parse_rejects_unknown_argument() {
        let err = Cli::try_parse_from(["tino", "--nope"]).unwrap_err();
        assert_eq!(err.kind(), CliParseErrorKind::Message);
        assert!(err.to_string().contains("unexpected argument"));
    }

    #[test]
    fn parse_rejects_missing_option_value() {
        let cases = [
            ["tino", "-p"],
            ["tino", "-e"],
            ["tino", "-t"],
            ["tino", "--grace-ms"],
            ["tino", "--write-allow"],
            ["tino", "--write-preset"],
            ["tino", "--bind-tcp-allow"],
            ["tino", "--connect-tcp-allow"],
            ["tino", "--exec-allow"],
            ["tino", "--device-ioctl-allow"],
        ];

        for args in cases {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.kind(), CliParseErrorKind::Message);
            assert!(
                err.to_string().contains("missing value"),
                "unexpected missing-value error for {args:?}: {err}"
            );
        }
    }

    #[test]
    fn parse_rejects_invalid_numeric_value() {
        let cases = [
            ["tino", "-e", "256"],
            ["tino", "--bind-tcp-allow", "70000"],
            ["tino", "--connect-tcp-allow", "70000"],
            ["tino", "--grace-ms", "abc"],
        ];

        for args in cases {
            let err = Cli::try_parse_from(args).unwrap_err();
            assert_eq!(err.kind(), CliParseErrorKind::Message);
            assert!(
                err.to_string().contains("invalid value")
                    || err.to_string().contains("invalid digit")
                    || err.to_string().contains("number too large"),
                "unexpected numeric parse error for {args:?}: {err}"
            );
        }
    }

    #[test]
    fn parse_rejects_invalid_write_preset() {
        let err = Cli::try_parse_from(["tino", "--write-preset", "logs"]).unwrap_err();
        assert_eq!(err.kind(), CliParseErrorKind::Message);
        assert!(err.to_string().contains("invalid value for --write-preset"));
    }

    #[cfg(target_family = "unix")]
    #[test]
    fn parse_rejects_non_utf8_command_arguments() {
        let err = Cli::try_parse_from([
            OsString::from("tino"),
            OsString::from("--"),
            OsString::from_vec(vec![0xff, 0xfe]),
        ])
        .unwrap_err();

        assert_eq!(err.kind(), CliParseErrorKind::Message);
        assert!(err.to_string().contains("not valid UTF-8"));
    }
}
