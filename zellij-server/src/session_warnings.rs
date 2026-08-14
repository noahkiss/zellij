//! Session-wide conditions the server re-asks on a timer and hands to the bars as data.
//!
//! Two facts are true of a whole session, actionable, and invisible everywhere else:
//!
//! - **The server is running a superseded build.** A server keeps the binary it started with for
//!   the life of the session, so an upgrade reaches nothing until the session is restarted.
//! - **Full Disk Access is missing** on a macOS host whose config says it is expected. TCC keys the
//!   grant to an absolute path, so a package upgrade silently invalidates it, and the failures show
//!   up later in an unrelated tool.
//!
//! Both are asked HERE rather than in a plugin, and that is the whole point of the module. The
//! answers need `std::env::current_exe`, the `PATH`, and a real `open(2)` against a TCC-gated file
//! - none of which a wasm plugin can reach without spawning a process. Asking once in the server
//! and shipping the answer as [`SessionWarning`] values costs one probe per session per tick, no
//! matter how many bars draw it.

use std::path::PathBuf;
use std::sync::OnceLock;
use zellij_utils::data::SessionWarning;
use zellij_utils::session_lifecycle::{build_is_superseded, full_disk_access_missing};

/// What the config says about each condition, recorded once at server startup.
///
/// Recorded rather than threaded: the questions are asked from `Screen`, whose constructor already
/// takes thirty arguments, and the answers are one small fact about the whole session.
#[derive(Debug, Default, Clone)]
pub struct WarningSettings {
    /// Whether this machine's user says zellij is meant to hold Full Disk Access
    pub expect_full_disk_access: bool,
    /// Whether to say so when this server's binary has been superseded
    pub stale_build_notice: bool,
    /// The pinned copy `pin_exe` asks for, when it asks for one - a server executing it cannot be
    /// overwritten in place, so an upgrade never shows up in its own path and the question has to
    /// look at what is installed instead
    pub pinned_exe: Option<PathBuf>,
}

static SETTINGS: OnceLock<WarningSettings> = OnceLock::new();

/// Tell the warnings what the config asked for. Only the first call counts.
pub fn record_settings(settings: WarningSettings) {
    let _ = SETTINGS.set(settings);
}

fn settings() -> WarningSettings {
    SETTINGS.get().cloned().unwrap_or(WarningSettings {
        expect_full_disk_access: false,
        // on unless the config turns it off, which is also what a test with no settings recorded
        // should see
        stale_build_notice: true,
        pinned_exe: None,
    })
}

/// Ask both questions now, in the order a bar draws them.
///
/// Asked fresh every time rather than cached: both answers change under a running server - an FDA
/// toggle takes effect immediately, and an upgrade can replace the binary at any moment.
pub fn current_warnings() -> Vec<SessionWarning> {
    let settings = settings();
    let mut warnings = vec![];
    if settings.stale_build_notice && build_is_superseded(settings.pinned_exe.as_deref()) {
        warnings.push(SessionWarning::SupersededBuild);
    }
    if settings.expect_full_disk_access && full_disk_access_missing() {
        warnings.push(SessionWarning::MissingFullDiskAccess);
    }
    warnings
}

#[cfg(test)]
#[path = "./unit/session_warnings_tests.rs"]
mod session_warnings_tests;
