//! Zellij logging utility functions.

use std::{
    fs,
    io::{self, prelude::*},
    path::{Path, PathBuf},
};

use log::LevelFilter;

use log4rs::append::rolling_file::{
    policy::compound::{
        roll::fixed_window::FixedWindowRoller, trigger::size::SizeTrigger, CompoundPolicy,
    },
    RollingFileAppender,
};
use log4rs::config::{Appender, Config, Logger, Root};
use log4rs::encode::pattern::PatternEncoder;

use crate::consts::{ZELLIJ_TMP_DIR, ZELLIJ_TMP_LOG_DIR, ZELLIJ_TMP_LOG_FILE};
use crate::shared::set_permissions;

const LOG_MAX_BYTES: u64 = 1024 * 1024 * 16; // 16 MiB per log

/// Fail with the directory that could not be used and the variable that chose it.
///
/// FORK PATCH. This ran `.unwrap()`, and the commonest way it fails is a `TMPDIR` naming a
/// directory that is not there - on macOS that one variable also decides the socket directory
/// (`consts.rs`, where `runtime_dir()` is `None`), so it is the variable most likely to be wrong.
/// A backtrace out of the first line of `main` gives the reader nothing to act on; the path and
/// the variable that produced it are the whole of what they need.
fn log_setup_failed(what: &str, path: &Path, error: io::Error) -> ! {
    // the variable the standard library reads to place the temporary directory on this platform
    #[cfg(windows)]
    let variables = ["TMP", "TEMP"];
    #[cfg(not(windows))]
    let variables = ["TMPDIR"];

    eprintln!("zellij: could not create the {} for logging", what);
    eprintln!("  path           : {}", path.display());
    eprintln!("  temp directory : {}", std::env::temp_dir().display());
    for variable in variables {
        eprintln!(
            "  {:<15}: {}",
            variable,
            std::env::var(variable).unwrap_or_else(|_| "(unset)".to_owned())
        );
    }
    eprintln!("  reason         : {}", error);
    eprintln!(
        "  The log directory sits under the temporary directory, which {} chooses. Create that \
         directory, or unset {} to fall back to the system default.",
        variables.join(" or "),
        variables.join(" and "),
    );
    std::process::exit(1)
}

pub fn configure_logger() {
    if let Err(e) = atomic_create_dir(&ZELLIJ_TMP_DIR) {
        log_setup_failed("temporary directory", &*ZELLIJ_TMP_DIR, e);
    }
    if let Err(e) = atomic_create_dir(&ZELLIJ_TMP_LOG_DIR) {
        log_setup_failed("log directory", &*ZELLIJ_TMP_LOG_DIR, e);
    }
    if let Err(e) = atomic_create_file(&ZELLIJ_TMP_LOG_FILE) {
        log_setup_failed("log file", &*ZELLIJ_TMP_LOG_FILE, e);
    }

    let trigger = SizeTrigger::new(LOG_MAX_BYTES);
    let roller = FixedWindowRoller::builder()
        .build(
            ZELLIJ_TMP_LOG_DIR
                .join("zellij.log.old.{}")
                .to_str()
                .unwrap(),
            1,
        )
        .unwrap();

    // {n} means platform dependent newline
    // module is padded to exactly 25 bytes and thread is padded to be between 10 and 15 bytes.
    let file_pattern = "{highlight({level:<6})} |{module:<25.25}| {date(%Y-%m-%d %H:%M:%S.%3f)} [{thread:<10.15}] {file}:{line}: {message} {n}";

    // default zellij appender, should be used across most of the codebase.
    let log_file = RollingFileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(file_pattern)))
        .build(
            &*ZELLIJ_TMP_LOG_FILE,
            Box::new(CompoundPolicy::new(
                Box::new(trigger),
                Box::new(roller.clone()),
            )),
        )
        .unwrap();

    // plugin appender. To be used in logging_pipe to forward stderr output from plugins. We do some formatting
    // in logging_pipe to print plugin name as 'module' and plugin_id instead of thread.
    let log_plugin = RollingFileAppender::builder()
        .encoder(Box::new(PatternEncoder::new(
            "{highlight({level:<6})} {message} {n}",
        )))
        .build(
            &*ZELLIJ_TMP_LOG_FILE,
            Box::new(CompoundPolicy::new(Box::new(trigger), Box::new(roller))),
        )
        .unwrap();

    // Set the default logging level to "info" and log it to zellij.log file
    // Decrease verbosity for `wasmtime_wasi` module because it has a lot of useless info logs
    // For `zellij_server::logging_pipe`, we use custom format as we use logging macros to forward stderr output from plugins
    let config = Config::builder()
        .appender(Appender::builder().build("logFile", Box::new(log_file)))
        .appender(Appender::builder().build("logPlugin", Box::new(log_plugin)))
        // reduce the verbosity of isahc, otherwise it logs on every failed web request
        .logger(
            Logger::builder()
                .appender("logFile")
                .build("isahc", LevelFilter::Error),
        )
        .logger(
            Logger::builder()
                .appender("logPlugin")
                .build("wasmtime_wasi", LevelFilter::Warn),
        )
        .logger(
            Logger::builder()
                .appender("logPlugin")
                .additive(false)
                .build("zellij_server::logging_pipe", LevelFilter::Trace),
        )
        .build(Root::builder().appender("logFile").build(LevelFilter::Info))
        .unwrap();

    let _ = log4rs::init_config(config).unwrap();
}

pub fn atomic_create_file(file_name: &Path) -> io::Result<()> {
    let _ = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(file_name)?;
    set_permissions(file_name, 0o600)
}

pub fn atomic_create_dir(dir_name: &Path) -> io::Result<()> {
    let result = if let Err(e) = fs::create_dir(dir_name) {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            Ok(())
        } else {
            Err(e)
        }
    } else {
        Ok(())
    };
    if result.is_ok() {
        set_permissions(dir_name, 0o700)?;
    }
    result
}

pub fn debug_to_file(message: &[u8], terminal_id: i32) -> io::Result<()> {
    let mut path = PathBuf::new();
    path.push(&*ZELLIJ_TMP_LOG_DIR);
    path.push(format!("zellij-{}.log", terminal_id));

    let mut file = fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)?;
    set_permissions(&path, 0o600)?;
    file.write_all(message)
}
