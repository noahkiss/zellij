//! fork addition: the session's memory of its last few closed terminal panes, for
//! `zellij action reopen-pane`.
//!
//! In memory only, newest first. A restart forgets every entry, and nothing here holds
//! scrollback: an entry is what it takes to put a pane back - where it was, what it was called,
//! where it stood, and the one line to type at its prompt.

use std::collections::VecDeque;
use std::path::PathBuf;

use serde::Serialize;
use zellij_utils::agent_detect::harness_for_command;
use zellij_utils::resurrect_command_hints::{
    args_for_session, builtin_resume_args, command_line_for_prompt,
};

/// How many closed panes a session remembers.
pub const CLOSED_PANES_KEPT: usize = 10;

/// What a closed pane was running, which decides what a reopen types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ClosedPaneKind {
    /// A coding agent: the reopen types its bare resume.
    Agent,
    /// A command pane: the reopen types the command.
    Command,
    /// A plain shell: the reopen types nothing.
    Shell,
}

/// One closed terminal pane.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClosedPane {
    pub handle: String,
    pub tab_id: usize,
    pub tab_name: String,
    pub title: String,
    pub cwd: Option<PathBuf>,
    pub kind: ClosedPaneKind,
    /// The harness, for an agent pane.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The session the agent held when the pane closed, when one was known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_session_id: Option<String>,
    /// The line a reopen types at the prompt, without running it. `None` for a shell.
    pub prefill: Option<String>,
}

/// What is known about a terminal pane at the moment it closes.
pub struct ClosingPane<'a> {
    /// The harness running in it, if any, and the session it holds now.
    pub agent: Option<(&'a str, Option<&'a str>)>,
    /// The live argv, argv0 first, when the process table was read.
    pub live_argv: Option<&'a [String]>,
    /// The command the pane was started with, for a command pane.
    pub started_with: Option<(String, Vec<String>)>,
}

impl<'a> ClosingPane<'a> {
    pub fn kind(&self) -> ClosedPaneKind {
        if self.agent.is_some() {
            ClosedPaneKind::Agent
        } else if self.started_with.is_some() {
            ClosedPaneKind::Command
        } else {
            ClosedPaneKind::Shell
        }
    }

    /// The line to type at the reopened pane's prompt.
    ///
    /// An agent gets its bare resume: the session it holds now when that is known, else the id
    /// its argv names, else its picker - the same rule a restored session follows. A command pane
    /// gets its own command line. A shell gets nothing.
    pub fn prefill(&self) -> Option<String> {
        if let Some((harness, session_id)) = self.agent {
            let args = match session_id {
                Some(session_id) => args_for_session(harness, session_id),
                None => builtin_resume_args(harness, &self.observed_agent_args(harness)),
            }
            .unwrap_or_default();
            return Some(command_line_for_prompt(harness, &args));
        }
        self.started_with
            .as_ref()
            .map(|(command, args)| command_line_for_prompt(command, args))
    }

    /// The arguments the agent was seen with: the live argv when it is the agent's, else the
    /// command the pane started with when that is.
    fn observed_agent_args(&self, harness: &str) -> Vec<String> {
        let is_this_harness =
            |command: &str| harness_for_command(command).map_or(false, |h| h.kind == harness);
        if let Some(argv) = self.live_argv {
            if let Some((argv0, args)) = argv.split_first() {
                if is_this_harness(argv0) {
                    return args.to_vec();
                }
            }
        }
        match &self.started_with {
            Some((command, args)) if is_this_harness(command) => args.clone(),
            _ => vec![],
        }
    }
}

/// The ring itself, newest first.
#[derive(Debug, Default)]
pub struct ClosedPanes {
    entries: VecDeque<ClosedPane>,
}

impl ClosedPanes {
    pub fn remember(&mut self, pane: ClosedPane) {
        self.entries.push_front(pane);
        self.entries.truncate(CLOSED_PANES_KEPT);
    }
    /// Takes the newest entry, or the newest one that had `handle`. The entry leaves the ring.
    pub fn take(&mut self, handle: Option<&str>) -> Option<ClosedPane> {
        let index = match handle {
            None => 0,
            Some(handle) => self.entries.iter().position(|e| e.handle == handle)?,
        };
        self.entries.remove(index)
    }
    pub fn list(&self) -> Vec<ClosedPane> {
        self.entries.iter().cloned().collect()
    }
}

/// `reopen-pane --list` as a table, newest first.
pub fn format_closed_panes_table(entries: &[ClosedPane]) -> Vec<String> {
    let mut lines = vec![["HANDLE", "TAB", "KIND", "CWD", "PREFILL"].join("  ")];
    for entry in entries {
        let kind = match entry.kind {
            ClosedPaneKind::Agent => "agent",
            ClosedPaneKind::Command => "command",
            ClosedPaneKind::Shell => "shell",
        };
        lines.push(
            [
                entry.handle.as_str(),
                entry.tab_name.as_str(),
                kind,
                &entry
                    .cwd
                    .as_ref()
                    .map(|cwd| cwd.display().to_string())
                    .unwrap_or_else(|| "-".to_owned()),
                entry.prefill.as_deref().unwrap_or("-"),
            ]
            .join("  "),
        );
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(handle: &str) -> ClosedPane {
        ClosedPane {
            handle: handle.to_owned(),
            tab_id: 0,
            tab_name: "Tab #1".to_owned(),
            title: handle.to_owned(),
            cwd: None,
            kind: ClosedPaneKind::Shell,
            agent: None,
            agent_session_id: None,
            prefill: None,
        }
    }

    #[test]
    fn the_ring_keeps_the_newest_ten_and_hands_back_the_newest_first() {
        let mut ring = ClosedPanes::default();
        for i in 0..12 {
            ring.remember(entry(&format!("pane-{}", i)));
        }
        let listed = ring.list();
        assert_eq!(listed.len(), CLOSED_PANES_KEPT);
        assert_eq!(listed[0].handle, "pane-11");
        assert_eq!(listed[9].handle, "pane-2");
        assert_eq!(ring.take(None).unwrap().handle, "pane-11");
        assert_eq!(ring.take(Some("pane-5")).unwrap().handle, "pane-5");
        assert!(
            ring.take(Some("pane-5")).is_none(),
            "a reopened entry leaves"
        );
        assert!(ring.take(Some("pane-0")).is_none(), "the oldest fell off");
        assert_eq!(ring.list().len(), 8);
    }

    #[test]
    fn an_agent_comes_back_as_its_bare_resume() {
        let argv = vec![
            "claude".to_owned(),
            "--model".to_owned(),
            "opus".to_owned(),
            "write the tests".to_owned(),
        ];
        // the session it holds now wins over whatever it started with
        let pane = ClosingPane {
            agent: Some(("claude", Some("abc-123"))),
            live_argv: Some(&argv),
            started_with: None,
        };
        assert_eq!(pane.kind(), ClosedPaneKind::Agent);
        assert_eq!(pane.prefill().as_deref(), Some("claude --resume abc-123"));
        // with no session known, the launch prompt is dropped and the picker is offered
        let pane = ClosingPane {
            agent: Some(("claude", None)),
            live_argv: Some(&argv),
            started_with: None,
        };
        assert_eq!(pane.prefill().as_deref(), Some("claude --resume"));
        let pane = ClosingPane {
            agent: Some(("codex", Some("s1"))),
            live_argv: None,
            started_with: Some(("codex".to_owned(), vec![])),
        };
        assert_eq!(pane.prefill().as_deref(), Some("codex resume s1"));
    }

    #[test]
    fn a_command_comes_back_typed_and_a_shell_comes_back_empty() {
        let pane = ClosingPane {
            agent: None,
            live_argv: None,
            started_with: Some((
                "cargo".to_owned(),
                vec!["test".to_owned(), "a b".to_owned()],
            )),
        };
        assert_eq!(pane.kind(), ClosedPaneKind::Command);
        assert_eq!(pane.prefill().as_deref(), Some("cargo test 'a b'"));
        let shell = ClosingPane {
            agent: None,
            live_argv: None,
            started_with: None,
        };
        assert_eq!(shell.kind(), ClosedPaneKind::Shell);
        assert_eq!(shell.prefill(), None);
    }

    #[test]
    fn the_list_json_names_what_a_consumer_reads() {
        let mut pane = entry("sunny-otter");
        pane.kind = ClosedPaneKind::Agent;
        pane.agent = Some("claude".to_owned());
        pane.agent_session_id = Some("abc".to_owned());
        pane.prefill = Some("claude --resume abc".to_owned());
        pane.cwd = Some(PathBuf::from("/work"));
        let json = serde_json::to_value(vec![pane]).unwrap();
        assert_eq!(json[0]["handle"], "sunny-otter");
        assert_eq!(json[0]["tab_name"], "Tab #1");
        assert_eq!(json[0]["kind"], "agent");
        assert_eq!(json[0]["cwd"], "/work");
        assert_eq!(json[0]["prefill"], "claude --resume abc");
        let shell = serde_json::to_value(vec![entry("calm-fox")]).unwrap();
        assert_eq!(shell[0]["kind"], "shell");
        assert!(shell[0]["prefill"].is_null());
        assert!(shell[0].get("agent").is_none());
    }
}
