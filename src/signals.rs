#[cfg(target_os = "linux")]
use nix::sys::signal::Signal;
#[cfg(target_os = "linux")]
use once_cell::sync::Lazy;

macro_rules! signal_spec {
    ($macro:ident) => {
        $macro![
            (HUP, SIGHUP),
            (INT, SIGINT),
            (QUIT, SIGQUIT),
            (ILL, SIGILL),
            (TRAP, SIGTRAP),
            (ABRT, SIGABRT),
            (BUS, SIGBUS),
            (FPE, SIGFPE),
            (KILL, SIGKILL),
            (USR1, SIGUSR1),
            (SEGV, SIGSEGV),
            (USR2, SIGUSR2),
            (PIPE, SIGPIPE),
            (ALRM, SIGALRM),
            (TERM, SIGTERM),
            (CONT, SIGCONT),
            (WINCH, SIGWINCH),
            (TTIN, SIGTTIN),
            (TTOU, SIGTTOU),
        ]
    };
}

macro_rules! generate_name_array {
    ($(($name:ident, $sig:ident)),+ $(,)?) => {
        [$(stringify!($name)),+]
    };
}

const SIGNAL_NAMES_ARRAY: [&str; 19] = signal_spec!(generate_name_array);

pub(crate) const SIGNAL_NAMES: &[&str] = &SIGNAL_NAMES_ARRAY;

pub(crate) fn canonical_signal_name(raw: &str) -> Option<&'static str> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    let upper = trimmed.to_ascii_uppercase();
    let candidate = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNAL_NAMES.iter().copied().find(|name| *name == candidate)
}

#[cfg(target_os = "linux")]
macro_rules! generate_signal_array {
    ($(($name:ident, $sig:ident)),+ $(,)?) => {
        [$(Signal::$sig),+]
    };
}

#[cfg(target_os = "linux")]
const SIGNAL_VALUES_ARRAY: [Signal; 19] = signal_spec!(generate_signal_array);

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) const FORWARDED_SIGNAL_NAMES: &[&str] = &[
    "HUP", "INT", "QUIT", "TERM", "USR1", "USR2", "WINCH", "CONT", "TTIN", "TTOU",
];

#[cfg(target_os = "linux")]
pub(crate) static FORWARDED_SIGNALS: Lazy<Vec<Signal>> = Lazy::new(|| {
    FORWARDED_SIGNAL_NAMES
        .iter()
        .map(|name| {
            signal_from_canonical(name)
                .unwrap_or_else(|| panic!("missing canonical signal mapping for {name}"))
        })
        .collect()
});

#[cfg(target_os = "linux")]
pub(crate) fn signal_from_canonical(name: &str) -> Option<Signal> {
    SIGNAL_NAMES
        .iter()
        .position(|candidate| *candidate == name)
        .map(|idx| SIGNAL_VALUES_ARRAY[idx])
}

#[cfg(target_os = "linux")]
pub(crate) fn signal_from_str(raw: &str) -> Option<Signal> {
    canonical_signal_name(raw).and_then(signal_from_canonical)
}
