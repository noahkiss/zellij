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

/// The shared variable every harness's start hook exports, from nkmk.31: the harness's own native
/// session id. Read first, for every harness. See the agent-session contract (`zellij setup
/// --agent-hooks`).
pub const AGENT_SESSION_ID: &str = "AGENT_SESSION_ID";

/// The harness that exported [`AGENT_SESSION_ID`]: `claude`, `codex`, `pi` or `opencode`. When it
/// names another harness than the one detected - a pane running codex whose process tree carries
/// a nested claude's variables - the shared id is not this pane's and is ignored.
pub const AGENT_KIND: &str = "AGENT_KIND";

/// Every harness this build recognises.
///
/// Order matters only in that the first match wins, and the commands do not overlap.
pub const HARNESSES: &[AgentHarness] = &[
    AgentHarness {
        kind: "claude",
        match_commands: &["claude"],
        identity_env: &[
            AGENT_SESSION_ID,
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_SESSION_ID",
        ],
    },
    AgentHarness {
        kind: "opencode",
        match_commands: &["opencode"],
        identity_env: &[AGENT_SESSION_ID, "OPENCODE_SESSION_ID", "OPENCODE_SESSION"],
    },
    AgentHarness {
        kind: "codex",
        match_commands: &["codex"],
        identity_env: &[AGENT_SESSION_ID, "CODEX_SESSION_ID", "CODEX_THREAD_ID"],
    },
    // `pi` is a short name that something else could plausibly own. It is matched anyway - a false
    // positive costs a wrong row in `list-agents`, which the command column makes obvious - but it
    // is the reason this table matches whole basenames and never substrings.
    AgentHarness {
        kind: "pi",
        match_commands: &["pi"],
        identity_env: &[AGENT_SESSION_ID, "PI_SESSION_ID"],
    },
];

/// Every variable that names one agent's session: the shared contract's `AGENT_SESSION_ID` and
/// `AGENT_KIND`, and each harness's own identity variables from [`HARNESSES`].
///
/// A session this binary creates drops all of them, whatever the config says. Each one describes
/// the one agent whose pane created the session, and a new session hands its server's environment
/// to every pane. The detection reads these from a pane's process tree, the pane's shell first, so
/// a leaked one would name the creator's session for every agent started in the new session.
///
/// Derived from the table, so a harness added to it is dropped too. A variable detection does not
/// read, such as `CLAUDECODE`, is left to `session_restart_drop_env`.
pub fn agent_identity_env_names() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = std::iter::once(AGENT_KIND)
        .chain(
            HARNESSES
                .iter()
                .flat_map(|harness| harness.identity_env.iter().copied()),
        )
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

/// What was found running in a pane.
///
/// Carried on the CLI-only [`crate::data::PaneListEntry`], never on `PaneInfo`: the consumer is the
/// command line and the MCP server, and the plugin contract costs too much for what it would add.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgent {
    /// The harness: `claude`, `opencode`, `codex` or `pi`.
    pub kind: String,
    /// The live messaging name; unknown names stay null.
    #[serde(default)]
    pub name: Option<String>,
    /// The harness's own session id, if it exports one this build knows to look for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// What the answer rests on: `command` when only the pane's argv matched, `command+env` when
    /// an identity variable was found as well.
    ///
    /// A reader that needs to know whether `agent_id: null` means "not exported" or "not looked
    /// for" reads this rather than guessing.
    pub source: String,
    /// Where `agent_id` came from, from nkmk.31: `record` (claude's own session record),
    /// `open-file` (the session file codex holds open), `env` (an identity variable), `argv` (the
    /// id the agent was started with), or `picker-fallback` when nothing named the session and a
    /// restored pane would open the agent's picker. See [`with_session_source`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_source: Option<String>,
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
        .chain(std::iter::once(&AGENT_KIND))
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
    // the shared id belongs to whichever harness AGENT_KIND names; another harness's is not ours
    let kind_disowns_shared_id = env
        .get(AGENT_KIND)
        .map_or(false, |kind| !kind.is_empty() && kind != harness.kind);
    // an exported but empty variable is not an identity: it proves nothing a reader can use, and
    // reporting `agent_id: ""` would read as a real answer
    let agent_id = harness
        .identity_env
        .iter()
        .filter(|name| !(kind_disowns_shared_id && **name == AGENT_SESSION_ID))
        .find_map(|name| env.get(*name).filter(|value| !value.is_empty()))
        .cloned();
    let source = if agent_id.is_some() {
        "command+env"
    } else {
        "command"
    };
    Some(PaneAgent {
        kind: harness.kind.to_owned(),
        name: None,
        agent_id,
        source: source.to_owned(),
        session_source: None,
    })
}

/// `agent` with [`PaneAgent::session_source`] filled in, and `agent_id` taken from the argv when
/// nothing else named the session.
///
/// `live_source` is what the pty tick said about the id it found - `record` or `open-file` - and
/// is believed only when an id was found. An id with no such word came from an identity variable.
/// With no id at all, the command line that answered is asked: `--resume <id>` names the session
/// the agent was started on. Failing that the pane is `picker-fallback`, which is what a restore
/// would record for it.
pub fn with_session_source(
    mut agent: PaneAgent,
    live_source: Option<&str>,
    command_line: &str,
) -> PaneAgent {
    use crate::resurrect_command_hints::{argv_session_id, SessionSource};
    let source = if agent.agent_id.is_some() {
        live_source
            .map(|source| source.to_owned())
            .unwrap_or_else(|| SessionSource::Env.as_str().to_owned())
    } else {
        let mut words = command_line.split_whitespace().map(|word| word.to_owned());
        let argv0 = words.next().unwrap_or_default();
        let args: Vec<String> = words.collect();
        match argv_session_id(&argv0, &args) {
            Some(id) => {
                agent.agent_id = Some(id);
                SessionSource::Argv.as_str().to_owned()
            },
            None => SessionSource::PickerFallback.as_str().to_owned(),
        }
    };
    agent.session_source = Some(source);
    agent
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
                is_self: false,
                handle: entry.pane_info.handle.clone(),
                pane_id: entry.pane_info.id,
                tab_id: entry.pane_info.tab_id,
                tab_position: entry.tab_position,
                tab_name: entry.tab_name,
                title: entry.pane_info.title.clone(),
                agent,
                command,
                cwd: entry.pane_info.pane_cwd.as_deref().map(PathBuf::from),
                pid: entry.pane_info.pane_pid,
                last_output_at: entry.pane_info.last_output_at,
                program_status: entry.program_status.clone(),
                session: None,
            })
        })
        .collect()
}

/// The columns `list-agents` prints, in order. Append-only, like every other table in the fork.
///
/// `STATE` came last, in nkmk.33, because a column is never inserted before one a reader may
/// already count to. It is the `OSC 7501` state, with a blocked program's reason after a colon:
/// `blocked:permission`.
pub const AGENT_TABLE_KEYS: &str =
    "TAB_ID TAB_NAME PANE_ID HANDLE KIND AGENT_ID SOURCE TITLE COMMAND CWD STATE NAME";

/// The agent list as the fork's table: an UPPER_SNAKE header row and one line per agent.
///
/// An empty list prints the header alone, the way a query that found nothing says so rather than
/// printing nothing and leaving the reader to wonder whether it ran.
pub fn agent_table(agents: &ListAgentsResponse) -> Vec<String> {
    table_of(agents, false)
}

/// The agent table of `list-agents --all-sessions`: the same columns, and `SESSION` after them.
pub fn agent_table_with_sessions(agents: &ListAgentsResponse) -> Vec<String> {
    table_of(agents, true)
}

fn table_of(agents: &ListAgentsResponse, with_session: bool) -> Vec<String> {
    let mut header: Vec<&str> = AGENT_TABLE_KEYS.split(' ').collect();
    if with_session {
        header.push("SESSION");
    }
    let mut rows: Vec<Vec<String>> = vec![header.iter().map(|h| (*h).to_owned()).collect()];
    for agent in agents {
        let mut row = vec![
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
            dashed(agent.program_status.as_ref().map(state_cell)),
            if agent.is_self {
                format!("{} (you)", dashed(agent.agent.name.clone()))
            } else {
                dashed(agent.agent.name.clone())
            },
        ];
        if with_session {
            row.push(dashed(agent.session.clone()));
        }
        rows.push(row);
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

/// One line of `list-agents --watch`.
///
/// `agent` carries the whole row, so a reader keeps one map keyed by session and pane and
/// replaces the entry; `gone` names the row to drop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentWatchEvent {
    Agent(AgentListEntry),
    Gone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
        pane_id: u32,
        handle: String,
    },
}

/// Where a watched agent lives: its session, when the watch spans several, and its pane.
pub type AgentWatchKey = (Option<String>, u32);

/// What `--watch` remembers of one agent: the parts whose change is worth a line.
///
/// The identity and the state, not the whole row. The title is left out on purpose: several
/// harnesses animate it, and a watch that printed every spinner frame would bury the change a
/// reader is waiting for.
fn watched_part(agent: &AgentListEntry) -> String {
    serde_json::to_string(&(
        &agent.handle,
        &agent.agent.kind,
        &agent.agent.agent_id,
        &agent.program_status,
    ))
    .unwrap_or_default()
}

/// What a watch remembers of one agent between looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchedAgent {
    part: String,
    handle: String,
    /// Looks in a row this agent was missing from. One miss is not enough to call it gone.
    misses: u8,
}

/// The lines one more look at the agents is worth, and what to remember for the next look.
///
/// An agent that is new, or whose identity or state changed, is an `agent` line. One that is
/// missing from two looks in a row is a `gone` line: a pane list taken while a pane is being made
/// or moved can miss it once, and a `gone` followed by the same `agent` is noise a reader would
/// have to filter. Nothing else prints: an unchanged agent says nothing.
pub fn agent_watch_events(
    previous: &BTreeMap<AgentWatchKey, WatchedAgent>,
    current: ListAgentsResponse,
) -> (Vec<AgentWatchEvent>, BTreeMap<AgentWatchKey, WatchedAgent>) {
    let mut events = Vec::new();
    let mut seen: BTreeMap<AgentWatchKey, WatchedAgent> = BTreeMap::new();
    for agent in current {
        let key = (agent.session.clone(), agent.pane_id);
        let part = watched_part(&agent);
        let changed = previous.get(&key).map(|old| &old.part) != Some(&part);
        seen.insert(
            key,
            WatchedAgent {
                part,
                handle: agent.handle.clone(),
                misses: 0,
            },
        );
        if changed {
            events.push(AgentWatchEvent::Agent(agent));
        }
    }
    for (key, old) in previous {
        if seen.contains_key(key) {
            continue;
        }
        if old.misses == 0 {
            seen.insert(
                key.clone(),
                WatchedAgent {
                    misses: 1,
                    ..old.clone()
                },
            );
        } else {
            events.push(AgentWatchEvent::Gone {
                session: key.0.clone(),
                pane_id: key.1,
                handle: old.handle.clone(),
            });
        }
    }
    (events, seen)
}

/// The `STATE` cell: the state, and a blocked program's reason after a colon.
fn state_cell(status: &crate::data::PaneProgramStatus) -> String {
    match &status.kind {
        Some(kind) => format!("{}:{}", status.state, kind),
        None => status.state.clone(),
    }
}

/// `-` is how the fork's output says a field has no value, rather than printing an empty column.
fn dashed(value: Option<String>) -> String {
    match value {
        // a table row is one line: an agent started with a multi-line prompt has newlines in its
        // command, and they would break the row in two
        Some(value) if !value.is_empty() => value.replace(['\n', '\r', '\t'], " "),
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

    /// The names a new session drops: the contract's two and every identity variable detection
    /// reads, each once, and nothing detection does not read.
    #[test]
    fn a_new_session_drops_every_identity_detection_reads() {
        let names = agent_identity_env_names();
        for name in [
            "AGENT_SESSION_ID",
            "AGENT_KIND",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_SESSION_ID",
            "OPENCODE_SESSION_ID",
            "OPENCODE_SESSION",
            "CODEX_SESSION_ID",
            "CODEX_THREAD_ID",
            "PI_SESSION_ID",
        ] {
            assert!(names.contains(&name), "{} is not dropped", name);
        }
        for harness in HARNESSES {
            for name in identity_env_names_for(harness) {
                assert!(names.contains(&name.as_str()), "{} is not dropped", name);
            }
        }
        assert_eq!(names.len(), 9, "{:?}", names);
        assert!(!names.contains(&"CLAUDECODE"));
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
                tab_id: 1,
                ..Default::default()
            },
            tab_position: 0,
            tab_name: "develop".to_owned(),
            agent: detect_command_line("/opt/bin/claude", &env(&[])),
            command_state: None,
            last_command_exit_code: None,
            program_status: None,
            input_since_prompt: false,
            input_since_state: false,
        };
        let agents = agents_from_pane_list(vec![entry]);
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].command.as_deref(), Some("/opt/bin/claude"));
    }

    /// The OSC 7501 state rides on both rows flattened, under `state` and `state_*` keys, and is
    /// absent - not null - when nothing was reported.
    #[test]
    fn program_status_is_flattened_onto_both_rows_and_the_table() {
        use crate::data::PaneProgramStatus;
        let status = PaneProgramStatus {
            state: "blocked".to_owned(),
            kind: Some("permission".to_owned()),
            app: Some("claude-code".to_owned()),
            title: None,
            msg: Some("Allow Bash?".to_owned()),
            progress: None,
            at: 1_760_000_000_000,
        };
        let entry = PaneListEntry {
            pane_info: PaneInfo {
                id: 3,
                handle: "sunny-otter".to_owned(),
                pane_command: Some("claude".to_owned()),
                ..Default::default()
            },
            tab_position: 0,
            tab_name: "develop".to_owned(),
            agent: detect_command_line("claude", &env(&[])),
            command_state: None,
            last_command_exit_code: None,
            input_since_prompt: false,
            input_since_state: false,
            program_status: Some(status.clone()),
        };
        let pane_json = serde_json::to_value(&entry).unwrap();
        assert_eq!(pane_json["state"], "blocked");
        assert_eq!(pane_json["state_kind"], "permission");
        assert_eq!(pane_json["state_msg"], "Allow Bash?");
        assert_eq!(pane_json["state_at"], 1_760_000_000_000u64);
        assert!(pane_json.get("state_title").is_none());
        let back: PaneListEntry = serde_json::from_value(pane_json).unwrap();
        assert_eq!(back.program_status, Some(status));

        let agents = agents_from_pane_list(vec![entry.clone()]);
        let agent_json = serde_json::to_value(&agents[0]).unwrap();
        assert_eq!(agent_json["state"], "blocked");
        assert_eq!(agent_json["state_app"], "claude-code");
        assert_eq!(
            agent_json["kind"], "claude",
            "the harness keeps its own key"
        );
        let table = agent_table(&agents);
        assert!(table[0]
            .split_whitespace()
            .collect::<Vec<_>>()
            .ends_with(&["STATE", "NAME"]));
        assert!(table[1].contains("blocked:permission"));

        let quiet = PaneListEntry {
            program_status: None,
            ..entry
        };
        let quiet_json = serde_json::to_value(&quiet).unwrap();
        assert!(quiet_json.get("state").is_none());
        let back: PaneListEntry = serde_json::from_value(quiet_json).unwrap();
        assert_eq!(back.program_status, None);
        assert!(agent_table(&agents_from_pane_list(vec![quiet]))[1].ends_with('-'));
    }

    fn watched(pane_id: u32, session: Option<&str>, state: Option<&str>) -> AgentListEntry {
        AgentListEntry {
            is_self: false,
            handle: format!("pane-{pane_id}"),
            pane_id,
            tab_id: 0,
            tab_position: 0,
            tab_name: "t".to_owned(),
            title: "spinner 1".to_owned(),
            agent: detect_command_line("claude", &env(&[])).unwrap(),
            command: None,
            cwd: None,
            pid: None,
            last_output_at: Some(pane_id as u64),
            program_status: state.map(|state| crate::data::PaneProgramStatus {
                state: state.to_owned(),
                at: 1,
                ..Default::default()
            }),
            session: session.map(str::to_owned),
        }
    }

    #[test]
    fn a_multi_line_command_stays_on_its_own_row() {
        let mut agent = watched(1, None, None);
        agent.command = Some("claude -- line one\nline two".to_owned());
        let table = agent_table(&vec![agent]);
        assert_eq!(table.len(), 2);
        assert!(
            table[1].contains("claude -- line one line two"),
            "{}",
            table[1]
        );
    }

    #[test]
    fn an_agent_row_carries_when_its_pane_last_drew_and_a_watch_ignores_it() {
        let mut quiet = watched(1, None, Some("working"));
        let (_, seen) = agent_watch_events(&BTreeMap::new(), vec![quiet.clone()]);
        assert!(serde_json::to_string(&quiet)
            .unwrap()
            .contains("\"last_output_at\":1"));
        // a spinner frame moves last_output_at; it is not a change worth a line
        quiet.last_output_at = Some(99);
        let (events, _) = agent_watch_events(&seen, vec![quiet]);
        assert!(events.is_empty(), "{:?}", events);
    }

    #[test]
    fn a_watch_prints_what_changed_and_nothing_else() {
        let (events, seen) = agent_watch_events(
            &BTreeMap::new(),
            vec![
                watched(1, Some("a"), Some("working")),
                watched(1, Some("b"), None),
            ],
        );
        assert_eq!(events.len(), 2, "the first look reports every agent");
        // same pane id in two sessions is two agents
        assert_eq!(seen.len(), 2);

        // a new title alone is not a change; a new state is
        let mut retitled = watched(1, Some("a"), Some("working"));
        retitled.title = "spinner 2".to_owned();
        let (events, seen) = agent_watch_events(
            &seen,
            vec![retitled, watched(1, Some("b"), Some("blocked"))],
        );
        assert_eq!(
            events,
            vec![AgentWatchEvent::Agent(watched(
                1,
                Some("b"),
                Some("blocked")
            ))]
        );

        // missing once is not gone: a pane list can miss a pane that is being made
        let (events, seen) =
            agent_watch_events(&seen, vec![watched(1, Some("b"), Some("blocked"))]);
        assert!(events.is_empty());
        // and coming back after one miss, unchanged, says nothing either
        let (events, _) = agent_watch_events(
            &seen,
            vec![
                watched(1, Some("a"), Some("working")),
                watched(1, Some("b"), Some("blocked")),
            ],
        );
        assert!(events.is_empty());
        let (events, seen) =
            agent_watch_events(&seen, vec![watched(1, Some("b"), Some("blocked"))]);
        assert_eq!(
            events,
            vec![AgentWatchEvent::Gone {
                session: Some("a".to_owned()),
                pane_id: 1,
                handle: "pane-1".to_owned(),
            }]
        );
        assert_eq!(seen.len(), 1);
    }

    #[test]
    fn a_watch_line_is_tagged_and_carries_the_row_flat() {
        let line =
            serde_json::to_value(AgentWatchEvent::Agent(watched(3, None, Some("done")))).unwrap();
        assert_eq!(line["event"], "agent");
        assert_eq!(line["pane_id"], 3);
        assert_eq!(line["state"], "done");
        assert!(line.get("session").is_none());
        let gone = serde_json::to_value(AgentWatchEvent::Gone {
            session: Some("work".to_owned()),
            pane_id: 3,
            handle: "pane-3".to_owned(),
        })
        .unwrap();
        assert_eq!(gone["event"], "gone");
        assert_eq!(gone["session"], "work");
    }

    #[test]
    fn the_all_sessions_table_puts_session_last() {
        let table = agent_table_with_sessions(&vec![watched(1, Some("work"), Some("idle"))]);
        assert!(table[0].ends_with("NAME  SESSION"));
        assert!(table[1].ends_with("work"));
        assert!(agent_table_with_sessions(&vec![])[0].ends_with("SESSION"));
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
        // its own names, and AGENT_KIND to check the shared id against
        assert_eq!(asked.len(), claude.identity_env.len() + 1);
        assert!(asked.contains(&AGENT_KIND.to_owned()));
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

    #[test]
    fn a_found_id_keeps_the_source_the_tick_named() {
        let agent = detect(
            &argv(&["claude"]),
            &env(&[("CLAUDE_CODE_SESSION_ID", "from-record")]),
        )
        .unwrap();
        let agent = with_session_source(agent, Some("record"), "claude");
        assert_eq!(agent.agent_id.as_deref(), Some("from-record"));
        assert_eq!(agent.session_source.as_deref(), Some("record"));
    }

    #[test]
    fn an_id_with_no_named_source_came_from_the_environment() {
        let agent = detect(&argv(&["pi"]), &env(&[("PI_SESSION_ID", "p-1")])).unwrap();
        let agent = with_session_source(agent, None, "pi");
        assert_eq!(agent.session_source.as_deref(), Some("env"));
    }

    #[test]
    fn with_no_live_id_the_argv_names_the_session() {
        let agent = detect(&argv(&["claude"]), &BTreeMap::new()).unwrap();
        let agent = with_session_source(agent, None, "claude --resume abc --model m");
        assert_eq!(agent.agent_id.as_deref(), Some("abc"));
        assert_eq!(agent.session_source.as_deref(), Some("argv"));
    }

    #[test]
    fn with_nothing_naming_the_session_the_pane_falls_back_to_the_picker() {
        for line in [
            "claude --resume",
            "claude --continue",
            "codex",
            "codex resume --last",
        ] {
            let argv0 = line.split_whitespace().next().unwrap();
            let agent = detect(&argv(&[argv0]), &BTreeMap::new()).unwrap();
            let agent = with_session_source(agent, Some("record"), line);
            assert_eq!(agent.agent_id, None, "{}", line);
            assert_eq!(
                agent.session_source.as_deref(),
                Some("picker-fallback"),
                "{}",
                line
            );
        }
    }

    #[test]
    fn a_pane_agent_without_a_session_source_omits_the_key() {
        let agent = detect(&argv(&["claude"]), &BTreeMap::new()).unwrap();
        let json = serde_json::to_string(&agent).unwrap();
        assert!(!json.contains("session_source"), "{}", json);
        let json = serde_json::to_string(&with_session_source(agent, None, "claude")).unwrap();
        assert!(
            json.contains(r#""session_source":"picker-fallback""#),
            "{}",
            json
        );
    }

    #[test]
    fn the_shared_variable_is_read_first_for_every_harness() {
        for harness in HARNESSES {
            assert_eq!(harness.identity_env.first(), Some(&AGENT_SESSION_ID));
            let argv0 = harness.match_commands[0];
            let found = detect(
                &argv(&[argv0]),
                &env(&[
                    (AGENT_SESSION_ID, "shared"),
                    (harness.identity_env[1], "native"),
                ]),
            )
            .unwrap();
            assert_eq!(
                found.agent_id.as_deref(),
                Some("shared"),
                "{}",
                harness.kind
            );
        }
    }

    #[test]
    fn a_matching_agent_kind_keeps_the_shared_id() {
        let found = detect(
            &argv(&["codex"]),
            &env(&[(AGENT_SESSION_ID, "thread-1"), (AGENT_KIND, "codex")]),
        )
        .unwrap();
        assert_eq!(found.agent_id.as_deref(), Some("thread-1"));
    }

    /// A codex pane whose process tree carries a nested claude's variables: the shared id is
    /// claude's, not this pane's. codex's own name still answers.
    #[test]
    fn another_harnesss_agent_kind_disowns_the_shared_id() {
        let found = detect(
            &argv(&["codex"]),
            &env(&[(AGENT_SESSION_ID, "claude-id"), (AGENT_KIND, "claude")]),
        )
        .unwrap();
        assert_eq!(found.agent_id, None);
        let found = detect(
            &argv(&["codex"]),
            &env(&[
                (AGENT_SESSION_ID, "claude-id"),
                (AGENT_KIND, "claude"),
                ("CODEX_THREAD_ID", "own"),
            ]),
        )
        .unwrap();
        assert_eq!(found.agent_id.as_deref(), Some("own"));
    }

    /// opencode's bash tool passes on its parent claude's variables when nested. A pane detected
    /// as opencode never takes claude's: neither its native name nor a shared id claude exported.
    #[test]
    fn an_inherited_claude_identity_never_names_an_opencode_pane() {
        let inherited = env(&[
            ("CLAUDE_CODE_SESSION_ID", "parent-claude"),
            (AGENT_SESSION_ID, "parent-claude"),
            (AGENT_KIND, "claude"),
        ]);
        let found = detect(&argv(&["opencode"]), &inherited).unwrap();
        assert_eq!(found.agent_id, None);
        let mut own = inherited.clone();
        own.insert("OPENCODE_SESSION_ID".to_owned(), "ses_own".to_owned());
        let found = detect(&argv(&["opencode"]), &own).unwrap();
        assert_eq!(found.agent_id.as_deref(), Some("ses_own"));
        // once opencode's own hook has run, the shared pair is its own
        own.insert(AGENT_SESSION_ID.to_owned(), "ses_own".to_owned());
        own.insert(AGENT_KIND.to_owned(), "opencode".to_owned());
        let found = detect(&argv(&["opencode"]), &own).unwrap();
        assert_eq!(found.agent_id.as_deref(), Some("ses_own"));
    }
}

/// Mark the caller only within its own session.
pub fn mark_self(agents: &mut ListAgentsResponse, pane_id: Option<u32>, same_session: bool) {
    for agent in agents {
        agent.is_self = same_session && pane_id == Some(agent.pane_id);
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;
    #[test]
    fn names_are_null_until_a_live_record_supplies_them() {
        for kind in ["claude", "codex", "opencode", "pi"] {
            let agent = detect_command_line(kind, &BTreeMap::new()).unwrap();
            assert!(serde_json::to_value(agent).unwrap()["name"].is_null());
        }
    }
    #[test]
    fn caller_marker_requires_the_same_pane_and_session() {
        let mut agent = crate::data::AgentListEntry {
            is_self: false,
            handle: "test-pane".into(),
            pane_id: 7,
            tab_id: 0,
            tab_position: 0,
            tab_name: "tab".into(),
            title: "title".into(),
            agent: detect_command_line("claude", &BTreeMap::new()).unwrap(),
            command: None,
            cwd: None,
            pid: None,
            last_output_at: None,
            program_status: None,
            session: None,
        };
        agent.agent.name = Some("project-7".into());
        let mut agents = vec![agent];
        mark_self(&mut agents, Some(7), true);
        assert_eq!(serde_json::to_value(&agents[0]).unwrap()["self"], true);
        assert!(agent_table(&agents)[1].contains("project-7 (you)"));
        mark_self(&mut agents, Some(7), false);
        assert!(!agents[0].is_self);
        mark_self(&mut agents, Some(8), true);
        assert!(!agents[0].is_self);
    }
}
