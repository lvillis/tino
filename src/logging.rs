use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Eq, PartialEq, Ord, PartialOrd)]
enum Level {
    Warn = 1,
    Info = 2,
    Debug = 3,
}

impl Level {
    fn as_str(self) -> &'static str {
        match self {
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
        }
    }
}

static MAX_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub(crate) fn init(verbosity: u8) {
    let level = match verbosity {
        0 => Level::Info,
        _ => Level::Debug,
    };
    MAX_LEVEL.store(level as u8, Ordering::Relaxed);
}

pub(crate) fn debug_enabled() -> bool {
    enabled(Level::Debug)
}

pub(crate) fn warn(args: fmt::Arguments<'_>) {
    log(Level::Warn, args);
}

pub(crate) fn info(args: fmt::Arguments<'_>) {
    log(Level::Info, args);
}

pub(crate) fn debug(args: fmt::Arguments<'_>) {
    log(Level::Debug, args);
}

fn enabled(level: Level) -> bool {
    (level as u8) <= MAX_LEVEL.load(Ordering::Relaxed)
}

fn log(level: Level, args: fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }

    let mut stderr = io::stderr().lock();
    let _ = writeln!(stderr, "{} tino: {}", level.as_str(), args);
}
