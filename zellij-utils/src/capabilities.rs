//! What this build can do, in a form a consumer can read.
//!
//! A consumer that wants to know whether a fork feature is present has only had the version string
//! to go on, and that string is not orderable across an upstream bump: the `-nkmk.<n>` counter
//! RESETS when the upstream version moves, so `0.45.0-nkmk.4` is newer than `0.44.3-nkmk.7` while
//! `4 < 7`. Anything gating on the counter alone silently switches features off on an upgrade.
//!
//! So the build says what it has. `zellij setup --check --json` prints the version split into the
//! pair that IS comparable - the upstream base and the fork counter - and a flat list of stable
//! capability names. **Gate on the names.** A name is added when a surface appears and is never
//! reused for anything else; the version pair is for reporting, not for deciding.

use serde::{Deserialize, Serialize};

use crate::consts::VERSION;

/// The name of this fork, as it appears in the version suffix.
pub const FORK_NAME: &str = "nkmk";

/// Every consumer-visible surface this fork adds, by stable name.
///
/// One line per feature, added when the feature lands. Names are lower-case and hyphenated, they
/// name the surface rather than the patch, and they never change meaning - a renamed feature gets
/// a new name and keeps the old one until nothing reads it.
pub const CAPABILITIES: &[&str] = &[
    // this list itself, so a consumer can tell "no capabilities" from "too old to answer"
    "capabilities-json",
    // session lifecycle and its init-system integration
    "session-lifecycle",      // zellij session up|down|restart
    "session-service",        // zellij session enable|disable|status, setup --generate-service
    "session-kill-wait",      // kill-session/delete-session wait, --wait-timeout, --no-wait
    "attach-no-resurrect",    // zellij attach --no-resurrect
    "ls-exited-marker",       // zellij ls -s marks dead sessions (EXITED)
    "ls-json",                // zellij ls --json
    "setup-socket-dir",       // setup --check prints [SOCKET DIR]; ls warns about other socket dirs
    "build-mismatch-warning", // a client warns when the running server is another build
    // the snapshot archive
    "session-snapshots", // zellij snapshot list|show|restore|rm|prune, attach --restore
    "snapshot-import",   // zellij snapshot import
    "snapshot-plugin-api", // PluginCommand::ListSnapshots / RestoreSnapshot
    "snapshot-session-list", // the session manager picks a session out of the archive
    // pane and client identity, all of it in `list-panes --json` / `list-clients`
    "pane-uuid",                // PaneInfo.uuid
    "pane-restored-from",       // PaneInfo.restored_from
    "pane-pid",                 // list-panes --json carries pane_pid
    "pane-process-info",        // PaneInfo.pane_pid, pane_cwd, pane_command
    "pane-last-output",         // PaneInfo.last_output_at
    "pane-pending-bell",        // PaneInfo.has_pending_bell, tracked while detached
    "pane-env-report",          // report_pane_env, PaneInfo.pane_env
    "pane-program-title",       // PaneInfo.program_title
    "pane-stack-fields",        // PaneInfo.stack_id, index_in_stack, is_expanded_in_stack
    "pane-alt-screen",          // PaneInfo.is_alternate_screen
    "pane-scrollback-position", // PaneInfo.scrollback_position, scrollback_length
    "pane-layout-fields", // PaneInfo.is_pinned, logical_position, is_borderless, exclude_from_sync, has_explicit_title
    "pane-exited-event",  // Event::PaneExited, the exit status of any terminal pane
    "plugin-died-event",  // Event::PluginDied, a crashed plugin says so
    "pane-opened-event",  // Event::PaneOpened, broadcast from every creation path
    "client-tty",         // ClientInfo.tty
    "list-clients-fixed", // the session manager's client list actually lists clients
    "list-clients-json",  // zellij action list-clients --json
    "move-tab-to-index",  // zellij action move-tab --to-index
    "signal-pane",        // zellij action signal-pane --pane-id X --signal int|hup|kill
    "break-pane-cli",     // zellij action break-pane / break-pane-to-tab / -right / -left
    "idempotent-setters", // set-fullscreen, set-pane-pinned, set-pane-floating, set-sync-tab
    "go-to-tab-name-no-focus", // zellij action go-to-tab-name --create --no-focus
    "new-pane-stacked-error", // --stacked refuses instead of creating nothing
    "dump-screen-plugin-panes", // dump-screen works on plugin panes
    // plugin development loop
    "plugin-watch",       // plugin_watch, hot-reload of file: plugins
    "plugin-permissions", // plugin_permissions in config.kdl
    "builtin-plugin-dir", // builtin_plugin_dir, a builtin loaded from disk
    "builtin-slim-bars",  // zellij:slim-tab-bar, zellij:slim-keybinds
    // configuration this fork adds
    "config-path-expansion",   // ~ and $VAR in config paths
    "terminal-title-template", // terminal_title_template, session_aliases
    "pane-frame-top-only",     // pane_frame_style "top_only"
    "bell-clear-delay",        // bell_clear_delay_ms
    "default-floating-size",   // default_floating_size
    "resurrect-command-hints", // resurrect_command_hints
    "session-drop-env",        // session_restart_drop_env
    "snapshot-dir-config",     // snapshot_dir, session_snapshot_limit
    "session-service-config",  // session_service, pin_exe
    "status-notices",          // Full Disk Access / superseded-build notices
];

/// The version, split into the parts that can actually be compared.
///
/// `base` moves with upstream and orders normally. `fork_counter` orders only WITHIN one `base`,
/// because it restarts at 1 whenever `base` moves. An upstream build reports `fork: None` and
/// `fork_counter: None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionInfo {
    /// The whole string, exactly as `zellij --version` prints it.
    pub version: String,
    /// The upstream version this build is based on, e.g. `0.45.0`.
    pub base: String,
    /// The fork name, e.g. `nkmk`. Absent in an upstream build.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork: Option<String>,
    /// The fork counter, e.g. `4`. Absent in an upstream build. Comparable only within one `base`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fork_counter: Option<u64>,
}

impl VersionInfo {
    /// Split a version string of the form `<base>-<fork>.<counter>`.
    ///
    /// `base` is always the part before the first `-`, so an upstream pre-release still reports the
    /// version it is a pre-release of. A suffix that is not `<name>.<number>` leaves `fork` and
    /// `fork_counter` unset - an upstream build has no fork counter to compare.
    pub fn from_version_string(version: &str) -> Self {
        let (base, suffix) = match version.split_once('-') {
            Some((base, suffix)) => (base, Some(suffix)),
            None => (version, None),
        };
        let fork_parts =
            suffix
                .and_then(|suffix| suffix.rsplit_once('.'))
                .and_then(|(fork, counter)| {
                    counter
                        .parse::<u64>()
                        .ok()
                        .map(|counter| (fork.to_owned(), counter))
                });
        VersionInfo {
            version: version.to_owned(),
            base: base.to_owned(),
            fork: fork_parts.as_ref().map(|(fork, _)| fork.clone()),
            fork_counter: fork_parts.map(|(_, counter)| counter),
        }
    }
}

/// The version and capabilities of this build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildCapabilities {
    #[serde(flatten)]
    pub version: VersionInfo,
    /// Stable capability names. Gate on these, never on the version.
    pub capabilities: Vec<String>,
}

impl Default for BuildCapabilities {
    fn default() -> Self {
        BuildCapabilities {
            version: VersionInfo::from_version_string(VERSION),
            capabilities: CAPABILITIES.iter().map(|c| c.to_string()).collect(),
        }
    }
}

impl BuildCapabilities {
    /// True when this build reports the named capability.
    pub fn has(&self, capability: &str) -> bool {
        self.capabilities.iter().any(|c| c == capability)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_string_splits_into_base_and_counter() {
        let parsed = VersionInfo::from_version_string("0.45.0-nkmk.4");
        assert_eq!(parsed.base, "0.45.0");
        assert_eq!(parsed.fork.as_deref(), Some("nkmk"));
        assert_eq!(parsed.fork_counter, Some(4));
    }

    #[test]
    fn an_upstream_version_has_no_fork_parts() {
        let parsed = VersionInfo::from_version_string("0.45.0");
        assert_eq!(parsed.base, "0.45.0");
        assert_eq!(parsed.fork, None);
        assert_eq!(parsed.fork_counter, None);
    }

    #[test]
    fn a_suffix_that_is_not_a_fork_counter_is_left_alone() {
        let parsed = VersionInfo::from_version_string("0.45.0-alpha1");
        assert_eq!(parsed.base, "0.45.0");
        assert_eq!(parsed.fork, None);
        assert_eq!(parsed.fork_counter, None);
    }

    #[test]
    fn this_build_reports_its_own_version_and_a_known_capability() {
        let capabilities = BuildCapabilities::default();
        assert_eq!(capabilities.version.version, VERSION);
        assert!(capabilities.has("capabilities-json"));
        assert!(capabilities.has("pane-uuid"));
    }

    #[test]
    fn capability_names_are_unique_and_well_formed() {
        let mut seen = std::collections::HashSet::new();
        for capability in CAPABILITIES {
            assert!(
                seen.insert(*capability),
                "capability '{}' is listed twice",
                capability
            );
            assert!(
                capability
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-'),
                "capability '{}' is not lower-case and hyphenated",
                capability
            );
        }
    }
}
