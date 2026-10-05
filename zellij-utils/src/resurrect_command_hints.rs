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

    /// The whole argument list to record for a pane running `command`, or `None` to leave the
    /// pane as it was.
    ///
    /// For a harness this build knows - `claude`, `codex`, `opencode`, `pi` - the hint REPLACES
    /// whatever session the observed argv picked: every session word is dropped and the hint's
    /// words are added, so the command picks exactly one. Appending instead recorded
    /// `claude --continue --resume <id>`. For any other command the hint only appends, by the
    /// rule in [`Self::resume_args_for`], because this build does not know that tool's words.
    pub fn args_for(
        &self,
        command: &str,
        observed_args: &[String],
        env_value: &str,
    ) -> Option<Vec<String>> {
        if let Some(mut args) = strip_session_words(command, observed_args) {
            let words = self.resume_args_for(&[], env_value)?;
            args.extend(words);
            return Some(args);
        }
        let words = self.resume_args_for(observed_args, env_value)?;
        let mut args = observed_args.to_vec();
        args.extend(words);
        Some(args)
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

/// What a restored session does with the commands its panes were recorded running: the
/// top-level `resurrect_commands` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResurrectCommands {
    /// Every recorded command comes back as a command pane waiting to run. Upstream's behaviour,
    /// and what an unset key means.
    All,
    /// Only coding agents keep their command, recorded with how to resume it, and come back as a
    /// command pane waiting to run. Every other pane comes back as a plain shell.
    Agents,
    /// Only coding agents keep their command, and a restored agent pane is a plain shell with the
    /// resume command typed at its prompt, not run. Every other pane is a plain shell.
    Prefill,
}

impl ResurrectCommands {
    /// The value as the config spells it.
    pub fn as_str(&self) -> &'static str {
        match self {
            ResurrectCommands::All => "all",
            ResurrectCommands::Agents => "agents",
            ResurrectCommands::Prefill => "prefill",
        }
    }
    /// The mode a config value names, or `None` for a value this build does not know.
    pub fn from_config(value: &str) -> Option<Self> {
        match value {
            "all" => Some(ResurrectCommands::All),
            "agents" => Some(ResurrectCommands::Agents),
            "prefill" => Some(ResurrectCommands::Prefill),
            _ => None,
        }
    }
    /// Whether a serialized pane keeps its command only if it is an agent's.
    pub fn agents_only(&self) -> bool {
        !matches!(self, ResurrectCommands::All)
    }
}

/// Whether a word that picks a session takes the argument after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionValue {
    /// A bare flag: `--continue`.
    None,
    /// A value that may be left out: `claude --resume` alone opens a picker. The next word is
    /// taken as the value only when it is not itself a flag.
    Optional,
    /// A value that is always there: `--session-id <id>`.
    Required,
}

/// How a coding agent picks a session, and how it picks its newest one back up.
pub struct AgentResume {
    /// The harness, as [`crate::agent_detect::AgentHarness::kind`] names it.
    pub kind: &'static str,
    /// The arguments that resume the newest session. Every one of them needs no id: it asks the
    /// agent for its newest session in the pane's directory, which the restored pane is in.
    pub resume_args: &'static [&'static str],
    /// Whether `resume_args` is a subcommand, which goes straight after argv0 instead of last.
    /// `codex resume --last` is one; its subcommand takes the same options the bare command does.
    pub is_subcommand: bool,
    /// Every word that picks a session, and whether it takes a value. A recorded command keeps
    /// exactly one session choice, so these are removed before one is added.
    pub session_words: &'static [(&'static str, SessionValue)],
}

/// The session words of every harness [`crate::agent_detect::HARNESSES`] knows, from each CLI's
/// own `--help` (`opencode` from its published CLI reference).
pub const AGENT_RESUMES: &[AgentResume] = &[
    AgentResume {
        kind: "claude",
        resume_args: &["--continue"],
        is_subcommand: false,
        session_words: &[
            ("--continue", SessionValue::None),
            ("-c", SessionValue::None),
            ("--resume", SessionValue::Optional),
            ("-r", SessionValue::Optional),
            ("--session-id", SessionValue::Required),
            ("--fork-session", SessionValue::None),
            ("--from-pr", SessionValue::Optional),
        ],
    },
    AgentResume {
        kind: "opencode",
        resume_args: &["--continue"],
        is_subcommand: false,
        session_words: &[
            ("--continue", SessionValue::None),
            ("-c", SessionValue::None),
            ("--session", SessionValue::Required),
            ("-s", SessionValue::Required),
            ("--fork", SessionValue::None),
        ],
    },
    AgentResume {
        kind: "codex",
        resume_args: &["resume", "--last"],
        is_subcommand: true,
        session_words: &[
            ("resume", SessionValue::Optional),
            ("fork", SessionValue::Optional),
            ("--last", SessionValue::None),
            ("--all", SessionValue::None),
        ],
    },
    AgentResume {
        kind: "pi",
        resume_args: &["--continue"],
        is_subcommand: false,
        session_words: &[
            ("--continue", SessionValue::None),
            ("-c", SessionValue::None),
            ("--resume", SessionValue::None),
            ("-r", SessionValue::None),
            ("--session", SessionValue::Required),
            ("--session-id", SessionValue::Required),
            ("--fork", SessionValue::Required),
            ("--no-session", SessionValue::None),
        ],
    },
];

/// The resume table entry for the harness `command` runs, if this build knows one.
fn agent_resume_for(command: &str) -> Option<&'static AgentResume> {
    let harness = crate::agent_detect::harness_for_command(command)?;
    AGENT_RESUMES
        .iter()
        .find(|resume| resume.kind == harness.kind)
}

/// The session word `arg` is, and whether its value is glued on with `=`.
fn session_word<'a>(resume: &'a AgentResume, arg: &str) -> Option<(&'a SessionValue, bool)> {
    resume.session_words.iter().find_map(|(word, value)| {
        if arg == *word {
            Some((value, false))
        } else if arg.starts_with(word) && arg[word.len()..].starts_with('=') {
            Some((value, true))
        } else {
            None
        }
    })
}

/// Whether `command` is a coding agent that an agents-only mode keeps: a harness this build
/// knows, or a command a configured hint names.
pub fn is_resurrectable_agent(command: &str, hints: Option<&ResurrectCommandHints>) -> bool {
    crate::agent_detect::harness_for_command(command).is_some()
        || hints.map_or(false, |hints| hints.hint_for(command).is_some())
}

/// Whether `observed_args` already pick a session for the harness `command` runs.
pub fn picks_a_session(command: &str, observed_args: &[String]) -> bool {
    agent_resume_for(command).map_or(false, |resume| {
        observed_args
            .iter()
            .any(|arg| session_word(resume, arg).is_some())
    })
}

/// `observed_args` without any word that picks a session, for the harness `command` runs.
/// `None` when `command` is not a harness this build knows.
///
/// A restored command must pick exactly one session. Appending a resume to an argv that already
/// carried one recorded `claude --continue --resume <id>`, and a previously restored pane carried
/// its old `--resume <id>` into every later restart. Everything else in the argv is kept.
pub fn strip_session_words(command: &str, observed_args: &[String]) -> Option<Vec<String>> {
    let resume = agent_resume_for(command)?;
    let mut args = Vec::with_capacity(observed_args.len());
    let mut words = observed_args.iter().peekable();
    while let Some(word) = words.next() {
        match session_word(resume, word) {
            None => args.push(word.clone()),
            Some((_, true)) | Some((SessionValue::None, false)) => {},
            Some((SessionValue::Required, false)) => {
                words.next();
            },
            Some((SessionValue::Optional, false)) => {
                if words.peek().map_or(false, |next| !next.starts_with('-')) {
                    words.next();
                }
            },
        }
    }
    Some(args)
}

/// The arguments an agent pane is recorded with when nothing named its session: `observed_args`
/// with the agent's built-in resume of its newest session added. `None` when `command` is not an
/// agent this build knows how to resume, or when the observed arguments already pick a session.
pub fn builtin_resume_args(command: &str, observed_args: &[String]) -> Option<Vec<String>> {
    let resume = agent_resume_for(command)?;
    if picks_a_session(command, observed_args) {
        return None;
    }
    let resume_args = resume.resume_args.iter().map(|arg| (*arg).to_owned());
    let args = if resume.is_subcommand {
        resume_args.chain(observed_args.iter().cloned()).collect()
    } else {
        observed_args.iter().cloned().chain(resume_args).collect()
    };
    Some(args)
}

/// `observed_args` with whatever session they pick replaced by `session_id`, the session the
/// claude process holds NOW.
///
/// The argv names the session the process STARTED with. `/clear`, `/resume` and a new
/// conversation each move a running claude to another session and leave the argv as it was, so
/// for this one case the argv is the stale answer and the process's own record is the live one.
pub fn claude_args_for_live_session(observed_args: &[String], session_id: &str) -> Vec<String> {
    let mut args =
        strip_session_words("claude", observed_args).unwrap_or_else(|| observed_args.to_vec());
    args.push("--resume".to_owned());
    args.push(session_id.to_owned());
    args
}

/// A command line as a person would type it at a POSIX shell prompt: every word that is not
/// plainly safe is single-quoted. `resurrect_commands "prefill"` types this into a restored pane.
pub fn command_line_for_prompt(command: &str, args: &[String]) -> String {
    std::iter::once(command)
        .chain(args.iter().map(|arg| arg.as_str()))
        .map(quote_for_prompt)
        .collect::<Vec<_>>()
        .join(" ")
}

/// The bytes that put `command_line` at a prompt: one bracketed paste, with no newline.
///
/// A paste, not keystrokes, because a line editor binds widgets to plain keys - an abbreviation
/// expander on space, a completion menu on a character - and typed keys would run them. Inside a
/// paste ZLE and readline insert the text literally. It is only sent once the line editor has
/// turned bracketed paste on, so the editor is reading the markers it asked for.
///
/// Control characters are dropped from the text: an ESC would end the paste early and a newline
/// would run the line, and a recorded argument carries neither on purpose.
pub fn prefill_as_paste(command_line: &str) -> Vec<u8> {
    let text: String = command_line.chars().filter(|c| !c.is_control()).collect();
    let mut bytes = Vec::with_capacity(text.len() + 12);
    bytes.extend_from_slice(b"\x1b[200~");
    bytes.extend_from_slice(text.as_bytes());
    bytes.extend_from_slice(b"\x1b[201~");
    bytes
}

fn quote_for_prompt(word: &str) -> String {
    let is_safe = |c: char| c.is_ascii_alphanumeric() || "_-./:=@%+,".contains(c);
    if !word.is_empty() && word.chars().all(is_safe) {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', "'\\''"))
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

    #[test]
    fn only_known_agents_and_hinted_commands_are_resurrectable() {
        for agent in ["claude", "/opt/homebrew/bin/codex", "opencode", "pi"] {
            assert!(is_resurrectable_agent(agent, None), "{}", agent);
        }
        for other in [
            "starship",
            "/home/linuxbrew/.linuxbrew/bin/brew",
            "htop",
            "vim",
        ] {
            assert!(!is_resurrectable_agent(other, None), "{}", other);
        }
        let mut hints = ResurrectCommandHints::default();
        hints.push(hint("aider", "--restore-chat-history"));
        assert!(is_resurrectable_agent("aider", Some(&hints)));
    }

    #[test]
    fn a_builtin_resume_is_appended_to_the_observed_arguments() {
        let observed = args(&["--dangerously-skip-permissions"]);
        assert_eq!(
            builtin_resume_args("claude", &observed),
            Some(args(&["--dangerously-skip-permissions", "--continue"]))
        );
        assert_eq!(
            builtin_resume_args("/usr/bin/pi", &[]),
            Some(args(&["--continue"]))
        );
        assert_eq!(
            builtin_resume_args("opencode", &[]),
            Some(args(&["--continue"]))
        );
    }

    /// `codex resume` is a subcommand: it goes straight after argv0, and the observed options
    /// follow it, which the subcommand accepts.
    #[test]
    fn a_subcommand_resume_goes_first() {
        let observed = args(&["-m", "o3"]);
        assert_eq!(
            builtin_resume_args("codex", &observed),
            Some(args(&["resume", "--last", "-m", "o3"]))
        );
    }

    #[test]
    fn a_builtin_resume_adds_nothing_when_the_argv_already_resumes() {
        assert_eq!(
            builtin_resume_args("codex", &args(&["resume", "abc"])),
            None
        );
        assert_eq!(builtin_resume_args("pi", &args(&["-c"])), None);
        assert_eq!(builtin_resume_args("htop", &[]), None);
    }

    #[test]
    fn the_live_session_replaces_the_one_the_argv_started_with() {
        let observed = args(&[
            "--dangerously-skip-permissions",
            "--resume",
            "first-session",
        ]);
        assert_eq!(
            claude_args_for_live_session(&observed, "live-session"),
            args(&["--dangerously-skip-permissions", "--resume", "live-session"])
        );
    }

    #[test]
    fn every_session_picking_word_is_dropped_for_the_live_session() {
        let observed = args(&[
            "-c",
            "--resume",
            "--model",
            "opus",
            "--session-id",
            "x",
            "--resume=y",
        ]);
        assert_eq!(
            claude_args_for_live_session(&observed, "live"),
            args(&["--model", "opus", "--resume", "live"])
        );
    }

    #[test]
    fn every_agents_session_words_are_stripped() {
        assert_eq!(
            strip_session_words("codex", &args(&["-m", "o3", "resume", "abc", "--last"])),
            Some(args(&["-m", "o3"]))
        );
        assert_eq!(
            strip_session_words(
                "opencode",
                &args(&["-s", "x", "--continue", "--model", "m"])
            ),
            Some(args(&["--model", "m"]))
        );
        assert_eq!(
            strip_session_words(
                "pi",
                &args(&["--session=x", "-r", "--fork", "y", "--thinking", "high"])
            ),
            Some(args(&["--thinking", "high"]))
        );
        assert_eq!(strip_session_words("htop", &args(&["-d", "5"])), None);
    }

    #[test]
    fn a_hint_for_a_known_agent_replaces_the_argvs_session() {
        let observed = args(&["--dangerously-skip-permissions", "--continue"]);
        assert_eq!(
            hint("claude", "--resume {}").args_for("claude", &observed, "fced3e97"),
            Some(args(&[
                "--dangerously-skip-permissions",
                "--resume",
                "fced3e97"
            ]))
        );
        let restored = args(&["-r", "old", "--resume", "older"]);
        assert_eq!(
            hint("claude", "--resume {}").args_for("/usr/bin/claude", &restored, "new"),
            Some(args(&["--resume", "new"]))
        );
    }

    /// A tool this build has no session words for keeps the append-only rule.
    #[test]
    fn a_hint_for_another_tool_still_only_appends() {
        let observed = args(&["--restore"]);
        assert_eq!(
            hint("aider", "--restore").args_for("aider", &observed, "x"),
            None
        );
        assert_eq!(
            hint("aider", "--id {}").args_for("aider", &args(&["-v"]), "x"),
            Some(args(&["-v", "--id", "x"]))
        );
    }

    #[test]
    fn a_prompt_command_line_quotes_what_a_shell_would_split() {
        assert_eq!(
            command_line_for_prompt(
                "claude",
                &args(&["--dangerously-skip-permissions", "--resume", "fced3e97-31"])
            ),
            "claude --dangerously-skip-permissions --resume fced3e97-31"
        );
        assert_eq!(
            command_line_for_prompt("codex", &args(&["it's here", "$HOME", ""])),
            "codex 'it'\\''s here' '$HOME' ''"
        );
    }

    #[test]
    fn a_resurrect_commands_value_round_trips() {
        for mode in [
            ResurrectCommands::All,
            ResurrectCommands::Agents,
            ResurrectCommands::Prefill,
        ] {
            assert_eq!(ResurrectCommands::from_config(mode.as_str()), Some(mode));
        }
        assert_eq!(ResurrectCommands::from_config("never"), None);
        assert!(!ResurrectCommands::All.agents_only());
        assert!(ResurrectCommands::Prefill.agents_only());
    }

    #[test]
    fn a_prefill_is_one_bracketed_paste_with_no_newline() {
        assert_eq!(
            prefill_as_paste("claude --resume abc"),
            b"\x1b[200~claude --resume abc\x1b[201~".to_vec()
        );
    }

    #[test]
    fn a_prefill_cannot_end_its_paste_or_run_its_line() {
        assert_eq!(
            prefill_as_paste("x \x1b[201~y\nz\r"),
            b"\x1b[200~x [201~yz\x1b[201~".to_vec()
        );
    }
}
