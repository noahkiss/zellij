//! What the pane privacy filter is configured to withhold - the settings only, never the matching.
//!
//! A session can be told that some panes are nobody's business but the person at the terminal's.
//! The panes are named by regular expressions over the fields the pane list already carries, and a
//! pane that matches is *withheld*: its row is dropped from `list-panes`, and every command that
//! names it answers as if it were not there. Withholding is not redaction - a withheld pane leaves
//! a count behind and nothing else, because a redacted row still says where the private work is.
//!
//! **A refusal is the ordinary miss, byte for byte.** A command that names a withheld pane gets
//! `No pane answers to '<target>'`, the same sentence and the same exit code an unknown pane id
//! gets; a withheld tab gets that action's own no-such-tab answer; a `--cwd` the policy withholds
//! is dropped from the request, which is exactly what zellij already does with a directory that
//! does not exist. A refusal that said "withheld" would be a yes/no oracle on any string the
//! caller cared to try, and a loop over that oracle recovers the pattern list - the one thing the
//! filter exists to hide. The aggregate `withheld: n` count on `--report-withheld` is the only
//! output that admits a policy is running, and it is deliberate: a caller is entitled to know its
//! view is partial without being told what is missing.
//!
//! **This module holds plain data and nothing else.** No regex, no file read, no environment. That
//! is not tidiness, it is the wasm gate: `kdl/mod.rs` parses the `pane_privacy` block and is built
//! for `wasm32-wasip1` along with every default plugin, so anything this module reaches has to
//! build there too. `regex` does not, and neither does reading a file. The matcher therefore lives
//! in `zellij-server`, which never builds for wasm and already depends on `regex`.
//!
//! The settings are a top-level `pane_privacy` block in `config.kdl`, so a binary that predates the
//! feature ignores the whole block rather than failing to parse the config.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Names a file of patterns, and wins over the config file when it is set.
///
/// A pane privacy policy is a property of the machine more often than of the config: the same
/// `config.kdl` is shared across machines, and which directories are private is not.
pub const PANE_PRIVACY_FILE_ENV: &str = "ZELLIJ_PANE_PRIVACY_FILE";

/// What `snapshot show` and `snapshot restore` are told while a policy is active.
///
/// A snapshot is one layout document carrying every pane's cwd and command, so there is no row to
/// drop: either the whole thing is shown or none of it is. It is refused whole.
pub const WITHHELD_SNAPSHOT_MESSAGE: &str =
    "Snapshots are withheld by this session's pane privacy policy.";

/// What `dump-layout` is told while a policy is active, for the same reason as a snapshot.
pub const WITHHELD_LAYOUT_MESSAGE: &str =
    "The session layout is withheld by this session's pane privacy policy.";

/// A field of the pane list a pattern is tried against.
///
/// The default pair is `cwd` and `command`, which is where a private path shows up on its own. The
/// other two are opt-in because a title or a tab name is the user's own text: matching it is
/// useful, and surprising if it happens without being asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MatchField {
    /// `pane_cwd` - the working directory of the process in the pane, as of the last refresh.
    Cwd,
    /// `pane_command` and `terminal_command` - what the pane is running.
    Command,
    /// `title` and `program_title`.
    Title,
    /// The name of the tab the pane is in.
    TabName,
}

impl MatchField {
    pub fn from_name(name: &str) -> Option<MatchField> {
        match name.to_lowercase().as_str() {
            "cwd" => Some(MatchField::Cwd),
            "command" => Some(MatchField::Command),
            "title" => Some(MatchField::Title),
            "tab_name" => Some(MatchField::TabName),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            MatchField::Cwd => "cwd",
            MatchField::Command => "command",
            MatchField::Title => "title",
            MatchField::TabName => "tab_name",
        }
    }
}

/// What to do with a terminal pane whose cwd is not known yet.
///
/// `pane_cwd` is `None` until the pty thread's cache has refreshed for that pane, which is a window
/// of about a second after it is created. Withholding through that window is the default: it is the
/// exact moment a pane opened in a private directory would otherwise be listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnUnknownCwd {
    Withhold,
    Allow,
}

impl OnUnknownCwd {
    pub fn from_name(name: &str) -> Option<OnUnknownCwd> {
        match name.to_lowercase().as_str() {
            "withhold" => Some(OnUnknownCwd::Withhold),
            "allow" => Some(OnUnknownCwd::Allow),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            OnUnknownCwd::Withhold => "withhold",
            OnUnknownCwd::Allow => "allow",
        }
    }
}

/// How a tab inherits the verdict on its panes.
///
/// `any` is the default and the safe one. A tab is the unit of work: its name alone can say what
/// the work is, and a shell that has momentarily `cd`'d out of the private tree would otherwise
/// re-expose everything beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TabRule {
    /// Withhold the whole tab when any one of its panes is withheld.
    Any,
    /// Withhold the tab only when every one of its panes is.
    All,
}

impl TabRule {
    pub fn from_name(name: &str) -> Option<TabRule> {
        match name.to_lowercase().as_str() {
            "any" => Some(TabRule::Any),
            "all" => Some(TabRule::All),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            TabRule::Any => "any",
            TabRule::All => "all",
        }
    }
}

/// The `pane_privacy` block of `config.kdl`, as written.
///
/// Every field is optional so that a block naming one thing does not silently restate the defaults
/// for the rest, and so that merging two configs can tell "not set" from "set to the default".
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PanePrivacyOptions {
    /// A file of patterns, one per line. `#` comments and blank lines are ignored.
    pub patterns_file: Option<PathBuf>,
    /// Patterns written inline in the config, for a machine with no file to point at.
    pub patterns: Vec<String>,
    /// Which fields a pattern is tried against. Unset means [`DEFAULT_MATCH_FIELDS`].
    pub match_fields: Option<Vec<MatchField>>,
    /// What a terminal pane with no known cwd gets. Unset means [`OnUnknownCwd::Withhold`].
    pub on_unknown_cwd: Option<OnUnknownCwd>,
    /// How a tab inherits its panes' verdict. Unset means [`TabRule::Any`].
    pub tab_rule: Option<TabRule>,
}

/// The fields tried when the config does not say: where a private path lands by itself.
pub const DEFAULT_MATCH_FIELDS: [MatchField; 2] = [MatchField::Cwd, MatchField::Command];

impl PanePrivacyOptions {
    /// Whether this block asks for nothing.
    ///
    /// Only the patterns decide. A block that sets `match_fields` and names no pattern withholds
    /// nothing, and the whole filter short-circuits on this one question.
    pub fn is_empty(&self) -> bool {
        self.patterns_file.is_none() && self.patterns.is_empty()
    }

    pub fn match_fields_or_default(&self) -> Vec<MatchField> {
        self.match_fields
            .clone()
            .filter(|fields| !fields.is_empty())
            .unwrap_or_else(|| DEFAULT_MATCH_FIELDS.to_vec())
    }

    pub fn on_unknown_cwd_or_default(&self) -> OnUnknownCwd {
        self.on_unknown_cwd.unwrap_or(OnUnknownCwd::Withhold)
    }

    pub fn tab_rule_or_default(&self) -> TabRule {
        self.tab_rule.unwrap_or(TabRule::Any)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_block_asks_for_nothing() {
        assert!(PanePrivacyOptions::default().is_empty());
    }

    #[test]
    fn settings_without_patterns_still_ask_for_nothing() {
        let options = PanePrivacyOptions {
            match_fields: Some(vec![MatchField::Title]),
            on_unknown_cwd: Some(OnUnknownCwd::Withhold),
            ..Default::default()
        };
        assert!(options.is_empty());
    }

    #[test]
    fn one_inline_pattern_is_a_policy() {
        let options = PanePrivacyOptions {
            patterns: vec!["pretend-private".to_owned()],
            ..Default::default()
        };
        assert!(!options.is_empty());
    }

    #[test]
    fn defaults_are_cwd_and_command_withhold_and_any() {
        let options = PanePrivacyOptions::default();
        assert_eq!(
            options.match_fields_or_default(),
            vec![MatchField::Cwd, MatchField::Command]
        );
        assert_eq!(options.on_unknown_cwd_or_default(), OnUnknownCwd::Withhold);
        assert_eq!(options.tab_rule_or_default(), TabRule::Any);
    }

    #[test]
    fn an_empty_match_fields_list_falls_back_to_the_default() {
        let options = PanePrivacyOptions {
            match_fields: Some(vec![]),
            ..Default::default()
        };
        assert_eq!(
            options.match_fields_or_default(),
            DEFAULT_MATCH_FIELDS.to_vec()
        );
    }

    #[test]
    fn names_round_trip() {
        for field in [
            MatchField::Cwd,
            MatchField::Command,
            MatchField::Title,
            MatchField::TabName,
        ] {
            assert_eq!(MatchField::from_name(field.name()), Some(field));
        }
        for unknown in [OnUnknownCwd::Withhold, OnUnknownCwd::Allow] {
            assert_eq!(OnUnknownCwd::from_name(unknown.name()), Some(unknown));
        }
        for rule in [TabRule::Any, TabRule::All] {
            assert_eq!(TabRule::from_name(rule.name()), Some(rule));
        }
        assert_eq!(MatchField::from_name("nonsense"), None);
    }
}
