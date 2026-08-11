//! Resume hints for the commands a serialized session records.
//!
//! Session serialization records the command a pane is running, and a restored pane holds that
//! command on screen so Enter re-runs it. For a long-lived tool that keeps its own state - a
//! coding agent, a REPL with a session id - re-running the bare command starts a NEW session and
//! the old one is only reachable through whatever resume flag the tool happens to have. The pane
//! comes back; the work in it does not.
//!
//! A hint says: when a pane is running `claude`, look for `CLAUDE_CODE_SESSION_ID` in that pane's
//! processes, and if it is there record `claude --resume <id>` instead of `claude`. The restored
//! pane then holds the resume command, and Enter picks the session back up.
//!
//! Every part of this is best-effort. A hint that does not match, a variable that is not set, a
//! platform that cannot read another process's environment - each records the command unchanged.
//! Serialization must never fail because a hint did not apply.

use serde::{Deserialize, Serialize};

/// The placeholder a `rewrite` template substitutes the environment value into.
pub const HINT_PLACEHOLDER: &str = "{}";

/// One hint: which command it recognises, which variable carries the state, what to record.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResurrectCommandHint {
    /// The name of the config block this hint was written under. Carried for error messages and
    /// logs only - matching never looks at it.
    pub name: String,
    /// Matched against the BASENAME of the recorded command, exactly. A hint of `claude` matches
    /// a pane running `claude` and one running `/opt/homebrew/bin/claude`, and does not match
    /// `claude-code`.
    pub match_command: String,
    /// The environment variable to look for in the pane's processes.
    pub env: String,
    /// What to record instead, with `{}` standing for the variable's value. Split on whitespace
    /// into a command and its arguments - it is not passed to a shell, so quoting, globs and
    /// pipes mean nothing here.
    pub rewrite: String,
}

impl ResurrectCommandHint {
    /// Whether this hint applies to a command. `command` is the recorded argv0, path and all.
    pub fn matches(&self, command: &str) -> bool {
        basename(command) == self.match_command
    }

    /// The rewritten command line: the template, split into a command and its arguments, with
    /// every `{}` replaced by `env_value`. `None` if the template holds no command.
    pub fn expand(&self, env_value: &str) -> Option<(String, Vec<String>)> {
        let mut words = self
            .rewrite
            .split_whitespace()
            .map(|word| word.replace(HINT_PLACEHOLDER, env_value));
        let command = words.next()?;
        Some((command, words.collect()))
    }
}

/// The `resurrect_command_hints` block: hints in the order they were configured.
///
/// Order is the order of the config file, and the FIRST hint whose `match` applies wins. Two hints
/// for the same command is a config mistake rather than a merge, so there is nothing to resolve.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResurrectCommandHints {
    #[serde(default)]
    pub hints: Vec<ResurrectCommandHint>,
}

impl ResurrectCommandHints {
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    pub fn push(&mut self, hint: ResurrectCommandHint) {
        self.hints.push(hint);
    }

    /// The first hint that applies to `command`, if any.
    pub fn hint_for(&self, command: &str) -> Option<&ResurrectCommandHint> {
        self.hints.iter().find(|hint| hint.matches(command))
    }
}

/// The last path component of a command, as a string. `/usr/bin/claude` -> `claude`.
///
/// Deliberately string surgery rather than `Path::file_name`: the recorded command is whatever the
/// process table reported, and a trailing separator or an empty tail should simply not match.
fn basename(command: &str) -> &str {
    command
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(command)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(match_command: &str, rewrite: &str) -> ResurrectCommandHint {
        ResurrectCommandHint {
            name: "test".to_owned(),
            match_command: match_command.to_owned(),
            env: "TEST_SESSION_ID".to_owned(),
            rewrite: rewrite.to_owned(),
        }
    }

    #[test]
    fn matches_on_the_basename() {
        let hint = hint("claude", "claude --resume {}");
        assert!(hint.matches("claude"));
        assert!(hint.matches("/opt/homebrew/bin/claude"));
        assert!(!hint.matches("claude-code"));
        assert!(!hint.matches("myclaude"));
        assert!(!hint.matches("/opt/bin/claude-code"));
    }

    #[test]
    fn expands_the_placeholder_into_an_argument() {
        let (command, args) = hint("claude", "claude --resume {}")
            .expand("abc123")
            .unwrap();
        assert_eq!(command, "claude");
        assert_eq!(args, vec!["--resume".to_owned(), "abc123".to_owned()]);
    }

    #[test]
    fn expands_a_placeholder_glued_to_a_flag() {
        let (command, args) = hint("opencode", "opencode --session={}")
            .expand("xyz")
            .unwrap();
        assert_eq!(command, "opencode");
        assert_eq!(args, vec!["--session=xyz".to_owned()]);
    }

    #[test]
    fn expands_a_template_with_no_arguments() {
        let (command, args) = hint("tool", "resume-{}").expand("7").unwrap();
        assert_eq!(command, "resume-7");
        assert!(args.is_empty());
    }

    #[test]
    fn first_matching_hint_wins() {
        let mut hints = ResurrectCommandHints::default();
        hints.push(hint("claude", "claude --resume {}"));
        hints.push(hint("claude", "claude --continue {}"));
        assert_eq!(
            hints.hint_for("/usr/local/bin/claude").map(|h| &h.rewrite),
            Some(&"claude --resume {}".to_owned())
        );
        assert!(hints.hint_for("bash").is_none());
    }
}
