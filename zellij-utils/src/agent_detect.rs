//! Which panes are running a coding agent, and which agent each one is.
//!
//! The question this answers is "send that to the develop agent" - resolving a harness by name
//! instead of by pane number. It is deliberately the cheap half of that: a pane's argv and a
//! handful of environment variables, matched against a table. Nothing here talks to the operating
//! system, and nothing here crosses the plugin API.
//!
//! Detection is two-phase, and the split is what lets it run by default:
//!
//! * **Phase one is free.** A pane's command is already recorded for every terminal pane, whatever
//!   the configuration says. Matching its basename against [`HARNESSES`] costs a string compare,
//!   so "which panes run an agent" needs no work from the operating system at all.
//! * **Phase two runs only for a pane phase one already matched.** A harness's own session id
//!   lives in the environment of the pane's processes, and reading that means walking the process
//!   subtree. A session with no agent panes therefore pays nothing.
//!
//! The identity variables in the table are best effort, and a harness that does not export the
//! name written here is still detected - by its command, with no id. That is what
//! [`PaneAgent::source`] reports, so a reader can tell an id that is missing from an id that was
//! never looked for.
//!
//! This module is pure data on purpose. It is built for wasm along with the rest of
//! `zellij-utils`' ungated half, so it must never reach for a platform API; the process walk that
//! feeds it lives in the server, behind that gate.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::data::{AgentListEntry, ListAgentsResponse, PaneListEntry};

/// One coding-agent harness: how to recognise it, and where it keeps its session id.
pub struct AgentHarness {
    /// The name reported to a reader: `claude`, `opencode`, `pi`, `codex`.
    pub kind: &'static str,
    /// Matched against the BASENAME of the pane's recorded command, exactly. `claude` matches a
    /// pane running `claude` and one running `/opt/homebrew/bin/claude`, and does not match
    /// `claude-code`. Same rule as a resurrect hint's `match`, for the same reason.
    pub match_commands: &'static [&'static str],
    /// Environment variables that carry this harness's own session id, most specific first. The
    /// first one set to a non-empty value in the pane's process subtree wins.
    ///
    /// Best effort: a harness absent from this list, or one that renamed its variable, is still
    /// detected by command alone.
    pub identity_env: &'static [&'static str],
}

/// Every harness this build recognises.
///
/// Order matters only in that the first match wins, and the commands do not overlap.
pub const HARNESSES: &[AgentHarness] = &[
    AgentHarness {
        kind: "claude",
        match_commands: &["claude"],
        identity_env: &["CLAUDE_CODE_SESSION_ID", "CLAUDE_SESSION_ID"],
    },
    AgentHarness {
        kind: "opencode",
        match_commands: &["opencode"],
        identity_env: &["OPENCODE_SESSION_ID", "OPENCODE_SESSION"],
    },
    AgentHarness {
        kind: "codex",
        match_commands: &["codex"],
        identity_env: &["CODEX_SESSION_ID", "CODEX_THREAD_ID"],
    },
    // `pi` is a short name that something else could plausibly own. It is matched anyway - a false
    // positive costs a wrong row in `list-agents`, which the command column makes obvious - but it
    // is the reason this table matches whole basenames and never substrings.
    AgentHarness {
        kind: "pi",
        match_commands: &["pi"],
        identity_env: &["PI_SESSION_ID"],
    },
];

/// What was found running in a pane.
///
/// Carried on the CLI-only [`crate::data::PaneListEntry`], never on `PaneInfo`: the consumer is the
/// command line and the MCP server, and the plugin contract costs too much for what it would add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgent {
    /// The harness: `claude`, `opencode`, `codex` or `pi`.
    pub kind: String,
    /// The harness's own session id, if it exports one this build knows to look for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// What the answer rests on: `command` when only the pane's argv matched, `command+env` when
    /// an identity variable was found as well.
    ///
    /// A reader that needs to know whether `agent_id: null` means "not exported" or "not looked
    /// for" reads this rather than guessing.
    pub source: String,
}

/// The harness a recorded command belongs to, or `None` for a command that is not one.
///
/// `command` is argv0, path and all.
pub fn harness_for_command(command: &str) -> Option<&'static AgentHarness> {
    let name = basename(command);
    HARNESSES
        .iter()
        .find(|harness| harness.match_commands.contains(&name))
}

/// The environment variable names to ask the process walk for, for a pane that matched `harness`.
///
/// One harness's names, not every harness's. The walk stops as soon as it has found everything it
/// was asked for, so asking for the union would mean a pane running `claude` could never satisfy
/// the walk - `codex`'s variables are not going to be there - and every descendant of every agent
/// pane would be read in full, every time. Asking only for the two names that could be there lets
/// the walk stop at the process that has them.
pub fn identity_env_names_for(harness: &AgentHarness) -> Vec<String> {
    let mut names: Vec<String> = harness
        .identity_env
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// The agent running in a pane, from its command and whatever identity variables were read for it.
///
/// `command` is the pane's recorded argv; an empty one is not an agent. `env` holds only the
/// identity variables - never the pane's whole environment, which is not read.
pub fn detect(command: &[String], env: &BTreeMap<String, String>) -> Option<PaneAgent> {
    let harness = harness_for_command(command.first()?)?;
    // an exported but empty variable is not an identity: it proves nothing a reader can use, and
    // reporting `agent_id: ""` would read as a real answer
    let agent_id = harness
        .identity_env
        .iter()
        .find_map(|name| env.get(*name).filter(|value| !value.is_empty()))
        .cloned();
    let source = if agent_id.is_some() {
        "command+env"
    } else {
        "command"
    };
    Some(PaneAgent {
        kind: harness.kind.to_owned(),
        agent_id,
        source: source.to_owned(),
    })
}

/// The agent running in a pane, from the command line the pane was STARTED with.
///
/// The same answer as [`detect`], for the case where the process table has not been asked about
/// this pane yet: a command pane is probed once it produces output, so a harness that has printed
/// nothing since it was made has no live argv to match. The recorded line is split on whitespace,
/// which is enough to find argv0.
///
/// An identity found this way is still an identity - the variables are read on the same tick and
/// keyed by the same pane - but a pane detected ONLY from this line will usually report
/// `source: "command"`, because the pty tick matches on the live argv and so never looked.
pub fn detect_command_line(
    command_line: &str,
    env: &BTreeMap<String, String>,
) -> Option<PaneAgent> {
    let argv: Vec<String> = command_line
        .split_whitespace()
        .map(|word| word.to_owned())
        .collect();
    detect(&argv, env)
}

/// The agent running in a pane, and the command line that answered.
///
/// A pane can offer two lines - the live argv, which is what it is running NOW, and the line it
/// was STARTED with, which is all there is for a pane the process table has not been asked about
/// yet. The live one wins, and whichever won is returned alongside the agent.
///
/// Both readers of the question go through here, and that is the point. Screen wants the agent;
/// `agents_from_pane_list` wants the line, because the `COMMAND` column of `list-agents` exists to
/// make a wrong row obvious and can only do that if it names the line the row was decided on. Two
/// implementations of "which line answered" would eventually disagree, and the column would be
/// contradicting its own row.
pub fn detect_in_pane<'a>(
    live_command: Option<&'a str>,
    recorded_command_line: Option<&'a str>,
    env: &BTreeMap<String, String>,
) -> Option<(PaneAgent, &'a str)> {
    if let Some(live_command) = live_command {
        if let Some(agent) = detect_command_line(live_command, env) {
            return Some((agent, live_command));
        }
    }
    let recorded_command_line = recorded_command_line?;
    let agent = detect_command_line(recorded_command_line, env)?;
    Some((agent, recorded_command_line))
}

/// The agents in a pane list: `zellij action list-agents`.
///
/// A projection of `list-panes`, not a second question. The pane list already carries the answer
/// on every entry - this drops the panes that are not agents and the fields a reader addressing an
/// agent has no use for. Doing it here rather than in the server is what keeps `list-agents` off
/// the client/server contract entirely: it is one `list-panes` on the wire.
pub fn agents_from_pane_list(panes: Vec<PaneListEntry>) -> ListAgentsResponse {
    panes
        .into_iter()
        .filter_map(|entry| {
            let agent = entry.agent.clone()?;
            // the line the match was made on, which is the one the COMMAND column reports. The
            // identity is already on the entry, so this asks only which line answered
            let command = detect_in_pane(
                entry.pane_info.pane_command.as_deref(),
                entry.pane_info.terminal_command.as_deref(),
                &BTreeMap::new(),
            )
            .map(|(_, command)| command.to_owned());
            Some(AgentListEntry {
                handle: entry.pane_info.handle.clone(),
                pane_id: entry.pane_info.id,
                tab_id: entry.tab_id,
                tab_position: entry.tab_position,
                tab_name: entry.tab_name,
                title: entry.pane_info.title.clone(),
                agent,
                command,
                cwd: entry.pane_info.pane_cwd.as_deref().map(PathBuf::from),
                pid: entry.pane_info.pane_pid,
            })
        })
        .collect()
}

/// The columns `list-agents` prints, in order. Append-only, like every other table in the fork.
pub const AGENT_TABLE_KEYS: &str =
    "TAB_ID TAB_NAME PANE_ID HANDLE KIND AGENT_ID SOURCE TITLE COMMAND CWD";

/// The agent list as the fork's table: an UPPER_SNAKE header row and one line per agent.
///
/// An empty list prints the header alone, the way a query that found nothing says so rather than
/// printing nothing and leaving the reader to wonder whether it ran.
pub fn agent_table(agents: &ListAgentsResponse) -> Vec<String> {
    let header: Vec<&str> = AGENT_TABLE_KEYS.split(' ').collect();
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    for agent in agents {
        rows.push(vec![
            agent.tab_id.to_string(),
            dashed(Some(agent.tab_name.clone())),
            agent.pane_id.to_string(),
            dashed(Some(agent.handle.clone())),
            agent.agent.kind.clone(),
            dashed(agent.agent.agent_id.clone()),
            agent.agent.source.clone(),
            dashed(Some(agent.title.clone())),
            dashed(agent.command.clone()),
            dashed(agent.cwd.as_ref().map(|cwd| cwd.display().to_string())),
        ]);
    }
    let widths: Vec<usize> = (0..header.len())
        .map(|column| {
            rows.iter()
                .map(|row| row[column].chars().count())
                .max()
                .unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            row.iter()
                .enumerate()
                .map(|(column, cell)| {
                    if column + 1 == row.len() {
                        cell.clone()
                    } else {
                        format!("{:width$}", cell, width = widths[column])
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// `-` is how the fork's output says a field has no value, rather than printing an empty column.
fn dashed(value: Option<String>) -> String {
    match value {
        Some(value) if !value.is_empty() => value,
        _ => "-".to_owned(),
    }
}

/// The last path component of a command. `/usr/bin/claude` -> `claude`.
///
/// The same string surgery a resurrect hint does, and for the same reason: the command is whatever
/// the process table reported, so a trailing separator should simply not match.
fn basename(command: &str) -> &str {
    command
        .rsplit(std::path::MAIN_SEPARATOR)
        .next()
        .unwrap_or(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::PaneInfo;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| w.to_string()).collect()
    }

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn matches_a_harness_on_the_basename() {
        assert_eq!(
            harness_for_command("claude").map(|h| h.kind),
            Some("claude")
        );
        assert_eq!(
            harness_for_command("/opt/homebrew/bin/claude").map(|h| h.kind),
            Some("claude")
        );
        assert_eq!(
            harness_for_command("/usr/local/bin/opencode").map(|h| h.kind),
            Some("opencode")
        );
    }

    #[test]
    fn a_command_that_merely_contains_a_harness_name_is_not_one() {
        assert!(harness_for_command("claude-code").is_none());
        assert!(harness_for_command("myclaude").is_none());
        assert!(harness_for_command("/opt/bin/pipenv").is_none());
        assert!(harness_for_command("zsh").is_none());
    }

    #[test]
    fn a_plain_shell_is_not_an_agent() {
        assert_eq!(detect(&argv(&["zsh"]), &env(&[])), None);
        assert_eq!(detect(&argv(&["/bin/bash", "-l"]), &env(&[])), None);
        assert_eq!(detect(&[], &env(&[])), None);
    }

    #[test]
    fn a_shell_with_an_agents_variable_set_is_still_not_an_agent() {
        // the variable can be inherited by anything the pane starts; the command is what decides
        assert_eq!(
            detect(&argv(&["zsh"]), &env(&[("CLAUDE_CODE_SESSION_ID", "abc")])),
            None
        );
    }

    #[test]
    fn the_command_alone_is_enough_to_report_the_harness() {
        let found = detect(&argv(&["claude"]), &env(&[])).expect("claude is a harness");
        assert_eq!(found.kind, "claude");
        assert_eq!(found.agent_id, None);
        assert_eq!(found.source, "command");
    }

    #[test]
    fn an_identity_variable_upgrades_the_source_and_carries_the_id() {
        let found = detect(
            &argv(&["/opt/homebrew/bin/claude", "--continue"]),
            &env(&[("CLAUDE_CODE_SESSION_ID", "9f3c")]),
        )
        .expect("claude is a harness");
        assert_eq!(found.kind, "claude");
        assert_eq!(found.agent_id.as_deref(), Some("9f3c"));
        assert_eq!(found.source, "command+env");
    }

    #[test]
    fn an_empty_identity_variable_is_not_an_identity() {
        let found =
            detect(&argv(&["claude"]), &env(&[("CLAUDE_CODE_SESSION_ID", "")])).expect("a harness");
        assert_eq!(found.agent_id, None);
        assert_eq!(found.source, "command");
    }

    #[test]
    fn the_first_name_in_the_table_wins_when_a_harness_exports_both() {
        let found = detect(
            &argv(&["claude"]),
            &env(&[
                ("CLAUDE_SESSION_ID", "second"),
                ("CLAUDE_CODE_SESSION_ID", "first"),
            ]),
        )
        .expect("a harness");
        assert_eq!(found.agent_id.as_deref(), Some("first"));
    }

    #[test]
    fn another_harnesss_variable_does_not_identify_this_one() {
        let found = detect(
            &argv(&["opencode"]),
            &env(&[("CLAUDE_CODE_SESSION_ID", "9f3c")]),
        )
        .expect("opencode is a harness");
        assert_eq!(found.kind, "opencode");
        assert_eq!(found.agent_id, None);
        assert_eq!(found.source, "command");
    }

    #[test]
    fn a_recorded_command_line_is_matched_on_its_first_word() {
        let found = detect_command_line("/tmp/bin/opencode --resume 4", &env(&[]))
            .expect("opencode is a harness");
        assert_eq!(found.kind, "opencode");
        assert_eq!(found.source, "command");
    }

    #[test]
    fn the_line_that_answered_is_the_one_reported() {
        // a pane whose live argv is not a harness, detected from the line it was started with:
        // the COMMAND column has to name that line, not the argv that did not match
        let (agent, command) = detect_in_pane(
            Some("sleep 900"),
            Some("/opt/bin/claude --continue"),
            &env(&[]),
        )
        .expect("the recorded line is a harness");
        assert_eq!(agent.kind, "claude");
        assert_eq!(command, "/opt/bin/claude --continue");

        // and when the live argv is the one that matched, it wins over the recorded line
        let (_, command) = detect_in_pane(Some("/usr/bin/claude"), Some("zsh"), &env(&[]))
            .expect("the live argv is a harness");
        assert_eq!(command, "/usr/bin/claude");

        assert_eq!(
            detect_in_pane(Some("zsh"), Some("vim claude.md"), &env(&[])),
            None
        );
        assert_eq!(detect_in_pane(None, None, &env(&[])), None);
    }

    #[test]
    fn the_command_column_names_the_line_the_row_was_decided_on() {
        let entry = PaneListEntry {
            pane_info: PaneInfo {
                id: 3,
                handle: "sunny-otter".to_owned(),
                pane_command: Some("sleep 900".to_owned()),
                terminal_command: Some("/opt/bin/claude".to_owned()),
                ..Default::default()
            },
            tab_id: 1,
            tab_position: 0,
            tab_name: "develop".to_owned(),
            agent: detect_command_line("/opt/bin/claude", &env(&[])),
        };
        let agents = agents_from_pane_list(vec![entry]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].command.as_deref(), Some("/opt/bin/claude"));
    }

    #[test]
    fn a_harness_named_later_in_a_recorded_line_is_not_what_the_pane_runs() {
        // the pane was started as `env`, which is what it is: the line's first word decides
        assert_eq!(
            detect_command_line("/usr/bin/env FOO=1 /tmp/bin/claude", &env(&[])),
            None
        );
    }

    #[test]
    fn every_harnesss_identity_variables_are_asked_for() {
        for harness in HARNESSES {
            let asked = identity_env_names_for(harness);
            for name in harness.identity_env {
                assert!(
                    asked.contains(&(*name).to_owned()),
                    "{} is in the table but never read",
                    name
                );
            }
        }
    }

    #[test]
    fn a_pane_is_only_asked_about_the_harness_it_matched() {
        // the walk stops when it has everything it asked for, so asking a claude pane about
        // codex's variables would mean never stopping
        let claude = harness_for_command("claude").expect("claude is a harness");
        let asked = identity_env_names_for(claude);
        assert_eq!(asked.len(), claude.identity_env.len());
        assert!(!asked.iter().any(|name| name.starts_with("CODEX_")));
    }

    #[test]
    fn no_two_harnesses_answer_to_the_same_command() {
        let mut seen: Vec<&str> = Vec::new();
        for harness in HARNESSES {
            for command in harness.match_commands {
                assert!(
                    !seen.contains(command),
                    "{} is claimed by two harnesses",
                    command
                );
                seen.push(command);
            }
        }
    }
}
