//! Resume hints for the commands a serialized session records.
//!
//! Session serialization records the command a pane is running, and a restored pane holds that
//! command on screen so Enter re-runs it. For a long-lived tool that keeps its own state - a
//! coding agent, a REPL with a session id - re-running the bare command starts a NEW session and
//! the old one is only reachable through whatever resume flag the tool happens to have. The pane
//! comes back; the work in it does not.
//!
//! A hint says: when a pane is running `claude`, look for `CLAUDE_CODE_SESSION_ID` in that pane's
//! processes, and if it is there record the observed command line with `--continue` appended. The
//! restored pane then holds a command that picks the session back up.
//!
//! The observed command line is the ground truth and is never replaced. A hint only ADDS
//! arguments, and adds nothing when the observed arguments already say how to resume - a pane
//! started as `claude --continue` is recorded exactly as it ran. The variable is a detector, not a
//! source: it says the pane really is running that tool. Its value reaches the recorded command
//! only through an explicit `{}` in `resume_args`.
//!
//! Every part of this is best-effort. A hint that does not match, a variable that is not set, a
//! platform that cannot read another process's environment - each records the command unchanged.
//! Serialization must never fail because a hint did not apply.

use serde::{Deserialize, Serialize};

/// The placeholder a `resume_args` template substitutes the environment value into. Optional: a
/// resume flag that needs no id - `--continue` - carries no placeholder.
pub const HINT_PLACEHOLDER: &str = "{}";

/// One hint: which command it recognises, which variable proves the tool is there, what to add.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResurrectCommandHint {
    /// The name of the config block this hint was written under. Carried for error messages and
    /// logs only - matching never looks at it.
    pub name: String,
    /// Matched against the BASENAME of the recorded command, exactly. A hint of `claude` matches
    /// a pane running `claude` and one running `/opt/homebrew/bin/claude`, and does not match
    /// `claude-code`.
    pub match_command: String,
    /// The environment variable to look for in the pane's processes. Finding it is what makes the
    /// hint fire.
    ///
    /// The search is breadth first over the pane's whole process subtree, so the value can come
    /// from a child - a subagent, a hook - rather than from the tool the pane is running. That is
    /// harmless while the variable is only a detector, and it is the residual risk of writing a
    /// `{}` into `resume_args`: what lands in the command is then whichever process answered
    /// first, which need not be the session the pane holds.
    pub env: String,
    /// The arguments to APPEND to the observed command line, with `{}` standing for the variable's
    /// value. Split on whitespace - it is not passed to a shell, so quoting, globs and pipes mean
    /// nothing here.
    pub resume_args: String,
}

impl ResurrectCommandHint {
    /// Whether this hint applies to a command. `command` is the recorded argv0, path and all.
    pub fn matches(&self, command: &str) -> bool {
        basename(command) == self.match_command
    }

    /// The arguments to append to `observed_args`, with every `{}` replaced by `env_value`.
    ///
    /// `None` means append nothing: the template is empty, the template needs a value and the
    /// variable is set but empty, or the observed command line already carries one of these words.
    /// A pane started as `claude --continue`, or as `claude --resume <id>`, already says how it
    /// resumes, and the argv it actually ran beats anything this hint could reconstruct.
    ///
    /// Words are compared whole, never as substrings: a hint of `--continue` does not consider
    /// itself present because the pane ran `--continue-on-error`.
    pub fn resume_args_for(
        &self,
        observed_args: &[String],
        env_value: &str,
    ) -> Option<Vec<String>> {
        // an exported but empty variable proves the tool is there and gives nothing to substitute;
        // expanding it would append a bare `--session ""` the pane could not run
        if env_value.is_empty() && self.resume_args.contains(HINT_PLACEHOLDER) {
            return None;
        }
        let words: Vec<String> = self
            .resume_args
            .split_whitespace()
            .map(|word| word.replace(HINT_PLACEHOLDER, env_value))
            .collect();
        if words.is_empty() {
            return None;
        }
        if words.iter().any(|word| observed_args.contains(word)) {
            return None;
        }
        Some(words)
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
    /// Entries of a hint that this binary does not know, in the words a human should read.
    ///
    /// A block that parses its own children used to REJECT an unknown one, and a rejection here
    /// fails the whole config rather than the block - so a key could never reach a shared config
    /// before the binary that understands it reached every machine. Keeping the names instead of
    /// erroring is what makes the order "config first, binaries after" work for a nested key the
    /// way it already worked for a top-level one.
    ///
    /// Kept rather than only logged because `zellij setup --check` is where someone looks when a
    /// key appears to do nothing, and a server log line is not there. Not written back out by
    /// [`crate::input::config::Config::to_string`]: an ignored key is not part of this build's
    /// configuration, and re-emitting it would make a dump claim otherwise.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unknown_entries: Vec<String>,
}

impl ResurrectCommandHints {
    /// Whether this block holds no hint. [`Self::unknown_entries`] deliberately does not count: an
    /// entry this build ignores adds nothing to a recorded command, and a block holding only
    /// ignored names is as empty as one holding nothing.
    pub fn is_empty(&self) -> bool {
        self.hints.is_empty()
    }

    /// Whether a hint's child names something this build reads.
    ///
    /// `rewrite` is here even though it is retired: it is a name this build knows and answers with
    /// its own warning, which says what replaced it. Reporting it as merely unknown would lose
    /// that.
    ///
    /// Asked BEFORE a child's value is read, because the value of every child has to be a string
    /// and an unknown name must not be held to a rule this build made up for it.
    pub fn is_known_hint_entry(entry: &str) -> bool {
        matches!(entry, "match" | "env" | "resume_args" | "rewrite")
    }

    /// Record an entry of a hint that this binary does not know, and say so in the log.
    ///
    /// The two surfaces are one call because they answer the same question in two places: the log
    /// line is for a session that is already running, and the kept string is what
    /// `zellij setup --check` prints for someone holding a config that seems to be ignored.
    pub fn note_unknown_entry(&mut self, message: String) {
        log::warn!("{}", message);
        self.unknown_entries.push(message);
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

    fn hint(match_command: &str, resume_args: &str) -> ResurrectCommandHint {
        ResurrectCommandHint {
            name: "test".to_owned(),
            match_command: match_command.to_owned(),
            env: "TEST_SESSION_ID".to_owned(),
            resume_args: resume_args.to_owned(),
        }
    }

    fn args(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    #[test]
    fn matches_on_the_basename() {
        let hint = hint("claude", "--continue");
        assert!(hint.matches("claude"));
        assert!(hint.matches("/opt/homebrew/bin/claude"));
        assert!(!hint.matches("claude-code"));
        assert!(!hint.matches("myclaude"));
        assert!(!hint.matches("/opt/bin/claude-code"));
    }

    #[test]
    fn a_flag_that_needs_no_id_is_appended_on_its_own() {
        assert_eq!(
            hint("claude", "--continue").resume_args_for(&[], "abc123"),
            Some(args(&["--continue"]))
        );
    }

    #[test]
    fn expands_the_placeholder_into_an_argument() {
        assert_eq!(
            hint("opencode", "--session {}").resume_args_for(&[], "abc123"),
            Some(args(&["--session", "abc123"]))
        );
    }

    #[test]
    fn expands_a_placeholder_glued_to_a_flag() {
        assert_eq!(
            hint("opencode", "--session={}").resume_args_for(&[], "xyz"),
            Some(args(&["--session=xyz"]))
        );
    }

    /// An exported but empty variable still fires the hint, and there is nothing to put in the
    /// placeholder. Appending `--session ""` would record a command the pane cannot run.
    #[test]
    fn an_empty_value_adds_nothing_when_the_template_needs_one() {
        assert_eq!(
            hint("opencode", "--session {}").resume_args_for(&[], ""),
            None
        );
        assert_eq!(
            hint("opencode", "--session={}").resume_args_for(&[], ""),
            None
        );
    }

    /// A template with no placeholder never touches the value, so an empty one is no obstacle -
    /// the variable did its whole job by existing.
    #[test]
    fn an_empty_value_still_appends_a_template_that_needs_none() {
        assert_eq!(
            hint("claude", "--continue").resume_args_for(&[], ""),
            Some(args(&["--continue"]))
        );
    }

    /// The guard compares whole arguments. A pane running a longer flag that merely starts with
    /// the hint's word has not resumed, and must still get the hint.
    #[test]
    fn a_longer_observed_flag_is_not_the_hints_word() {
        let observed = args(&["--continue-on-error"]);
        assert_eq!(
            hint("claude", "--continue").resume_args_for(&observed, "abc"),
            Some(args(&["--continue"]))
        );
    }

    #[test]
    fn an_empty_template_adds_nothing() {
        assert_eq!(hint("claude", "   ").resume_args_for(&[], "abc"), None);
    }

    /// The bug this whole surface exists to not have: the observed argv already resumed, and the
    /// hint appended a second, contradictory resume flag over the top of it.
    #[test]
    fn observed_arguments_that_already_resume_win() {
        let observed = args(&["--dangerously-skip-permissions", "--continue"]);
        assert_eq!(
            hint("claude", "--continue").resume_args_for(&observed, "abc123"),
            None
        );
    }

    #[test]
    fn an_observed_resume_flag_beats_a_reconstructed_id() {
        let observed = args(&["--resume", "the-id-that-actually-ran"]);
        assert_eq!(
            hint("claude", "--resume {}").resume_args_for(&observed, "some-other-id"),
            None
        );
    }

    #[test]
    fn unrelated_observed_arguments_do_not_block_the_hint() {
        let observed = args(&["--dangerously-skip-permissions"]);
        assert_eq!(
            hint("claude", "--continue").resume_args_for(&observed, "abc123"),
            Some(args(&["--continue"]))
        );
    }

    #[test]
    fn first_matching_hint_wins() {
        let mut hints = ResurrectCommandHints::default();
        hints.push(hint("claude", "--continue"));
        hints.push(hint("claude", "--resume {}"));
        assert_eq!(
            hints
                .hint_for("/usr/local/bin/claude")
                .map(|h| &h.resume_args),
            Some(&"--continue".to_owned())
        );
        assert!(hints.hint_for("bash").is_none());
    }
}
