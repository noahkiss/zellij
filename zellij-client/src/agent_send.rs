//! fork addition: which agent a `send-message` target names.
//!
//! The target is one word a person would use: a pane handle, a tab name, an agent's name, or the
//! name of its project directory. `session/word` keeps the search to one session. Exactly one agent
//! must answer; none or several is a miss that lists the candidates, so a caller never reaches the
//! wrong agent by a guess.

use std::path::Path;

use zellij_utils::data::{AgentListEntry, PaneListEntry};

/// A live pane in a session, as the search sees it.
pub struct Candidate<'a> {
    pub session: &'a str,
    pub pane: &'a PaneListEntry,
    /// The agent row, when the pane runs one.
    pub agent: Option<&'a AgentListEntry>,
}

/// What a target resolved to.
#[derive(Debug, PartialEq, Eq)]
pub enum Resolved {
    /// One agent: its session and terminal id.
    One { session: String, terminal_id: u32 },
    /// The target names a pane that runs no agent.
    NotAgent { session: String, handle: String },
    /// Nothing answers; every agent is a candidate.
    Nothing(Vec<String>),
    /// Several agents answer.
    Several(Vec<String>),
}

/// The agent `target` names among `candidates`.
///
/// A handle or a pane id names one pane and wins outright. Otherwise a tab name, an agent name
/// and a project directory name all count, compared without case, and every agent that answers to
/// any of them is a match: "develop" names the agent in the tab `develop` and the agent working in
/// `~/src/develop` alike, and two such agents are two candidates rather than a pick.
pub fn resolve(target: &str, candidates: &[Candidate]) -> Resolved {
    let (session_filter, word) = match target.split_once('/') {
        Some((session, word)) if !session.is_empty() && !word.is_empty() => (Some(session), word),
        _ => (None, target),
    };
    let in_scope: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| session_filter.map_or(true, |session| c.session == session))
        .collect();
    let pane_id = pane_id_of(word);
    let exact: Vec<&&Candidate> = in_scope
        .iter()
        .filter(|c| {
            !c.pane.pane_info.is_plugin
                && (c.pane.pane_info.handle == word || pane_id == Some(c.pane.pane_info.id))
        })
        .collect();
    if exact.len() == 1 {
        let c = exact[0];
        return match c.agent {
            Some(_) => Resolved::One {
                session: c.session.to_owned(),
                terminal_id: c.pane.pane_info.id,
            },
            None => Resolved::NotAgent {
                session: c.session.to_owned(),
                handle: c.pane.pane_info.handle.clone(),
            },
        };
    }
    let agents: Vec<&&Candidate> = in_scope.iter().filter(|c| c.agent.is_some()).collect();
    let matched: Vec<&&&Candidate> = if exact.len() > 1 {
        // the same handle or id in several sessions
        agents
            .iter()
            .filter(|c| exact.iter().any(|e| std::ptr::eq(**e, ***c)))
            .collect()
    } else {
        agents.iter().filter(|c| answers_to(c, word)).collect()
    };
    match matched.as_slice() {
        [one] => Resolved::One {
            session: one.session.to_owned(),
            terminal_id: one.pane.pane_info.id,
        },
        [] => Resolved::Nothing(agents.iter().map(|c| describe(c)).collect()),
        several => Resolved::Several(several.iter().map(|c| describe(c)).collect()),
    }
}

fn answers_to(candidate: &Candidate, word: &str) -> bool {
    let same = |value: &str| !value.is_empty() && value.eq_ignore_ascii_case(word);
    let Some(agent) = candidate.agent else {
        return false;
    };
    same(&agent.tab_name)
        || agent.agent.name.as_deref().is_some_and(same)
        || agent
            .cwd
            .as_deref()
            .and_then(Path::file_name)
            .and_then(|name| name.to_str())
            .is_some_and(same)
}

fn pane_id_of(word: &str) -> Option<u32> {
    word.strip_prefix("terminal_").unwrap_or(word).parse().ok()
}

/// One candidate, on one line: where it is and the words it answers to.
fn describe(candidate: &Candidate) -> String {
    let Some(agent) = candidate.agent else {
        return format!("{}/{}", candidate.session, candidate.pane.pane_info.handle);
    };
    let mut line = format!(
        "{}/{}  tab={}  kind={}",
        candidate.session, agent.handle, agent.tab_name, agent.agent.kind
    );
    if let Some(name) = &agent.agent.name {
        line.push_str(&format!("  name={}", name));
    }
    if let Some(dir) = agent.cwd.as_deref().and_then(Path::file_name) {
        line.push_str(&format!("  project={}", dir.to_string_lossy()));
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::agent_detect::PaneAgent;
    use zellij_utils::data::PaneInfo;

    struct Fixture {
        session: &'static str,
        pane: PaneListEntry,
        agent: Option<AgentListEntry>,
    }

    fn pane(
        session: &'static str,
        id: u32,
        handle: &str,
        tab: &str,
        agent: Option<(&str, Option<&str>, &str)>,
    ) -> Fixture {
        let pane = PaneListEntry {
            pane_info: PaneInfo {
                id,
                handle: handle.to_owned(),
                ..Default::default()
            },
            tab_position: 0,
            tab_name: tab.to_owned(),
            agent: None,
            command_state: None,
            last_command_exit_code: None,
            program_status: None,
            input_since_prompt: false,
            input_since_state: false,
            prefilled_command: None,
            queued_messages: 0,
        };
        let agent = agent.map(|(kind, name, cwd)| AgentListEntry {
            is_self: false,
            handle: handle.to_owned(),
            pane_id: id,
            tab_id: 0,
            tab_position: 0,
            tab_name: tab.to_owned(),
            title: String::new(),
            agent: PaneAgent {
                kind: kind.to_owned(),
                name: name.map(str::to_owned),
                agent_id: None,
                source: "command".to_owned(),
                session_source: None,
            },
            command: None,
            cwd: Some(cwd.into()),
            pid: None,
            last_output_at: None,
            program_status: None,
            session: None,
            queued_messages: 0,
        });
        Fixture {
            session,
            pane,
            agent,
        }
    }

    fn fleet() -> Vec<Fixture> {
        vec![
            pane(
                "work",
                1,
                "sunny-otter",
                "develop",
                Some(("claude", Some("builder"), "/src/app")),
            ),
            pane(
                "work",
                2,
                "brisk-heron",
                "review",
                Some(("codex", None, "/src/develop")),
            ),
            pane("work", 3, "quiet-shell", "develop", None),
            pane(
                "play",
                4,
                "calm-lynx",
                "notes",
                Some(("claude", Some("scribe"), "/src/notes")),
            ),
            pane(
                "play",
                5,
                "sunny-otter",
                "notes",
                Some(("pi", None, "/src/pi")),
            ),
        ]
    }

    fn resolve_in(target: &str, fixtures: &[Fixture]) -> Resolved {
        let candidates: Vec<Candidate> = fixtures
            .iter()
            .map(|f| Candidate {
                session: f.session,
                pane: &f.pane,
                agent: f.agent.as_ref(),
            })
            .collect();
        resolve(target, &candidates)
    }

    fn one(session: &str, terminal_id: u32) -> Resolved {
        Resolved::One {
            session: session.to_owned(),
            terminal_id,
        }
    }

    #[test]
    fn a_name_a_tab_or_a_project_names_one_agent() {
        let fleet = fleet();
        assert_eq!(resolve_in("builder", &fleet), one("work", 1));
        assert_eq!(resolve_in("BUILDER", &fleet), one("work", 1));
        assert_eq!(resolve_in("review", &fleet), one("work", 2));
        assert_eq!(resolve_in("scribe", &fleet), one("play", 4));
        assert_eq!(resolve_in("app", &fleet), one("work", 1));
        assert_eq!(resolve_in("brisk-heron", &fleet), one("work", 2));
        assert_eq!(resolve_in("terminal_4", &fleet), one("play", 4));
    }

    #[test]
    fn a_tab_with_one_agent_in_it_names_that_agent_even_beside_a_shell() {
        // `develop` is also brisk-heron's project directory: two agents, two candidates
        match resolve_in("develop", &fleet()) {
            Resolved::Several(lines) => {
                assert_eq!(lines.len(), 2);
                assert!(lines[0].starts_with("work/sunny-otter  tab=develop  kind=claude"));
                assert!(lines[1].contains("project=develop"));
            },
            other => panic!("{other:?}"),
        }
        // without that clash, the shell in the tab does not count
        let mut fleet = fleet();
        fleet[1].agent.as_mut().unwrap().cwd = Some("/src/other".into());
        assert_eq!(resolve_in("develop", &fleet), one("work", 1));
    }

    #[test]
    fn several_or_none_is_a_miss_with_the_candidates() {
        let fleet = fleet();
        assert!(matches!(resolve_in("notes", &fleet), Resolved::Several(l) if l.len() == 2));
        // one handle in two sessions
        assert!(matches!(resolve_in("sunny-otter", &fleet), Resolved::Several(l) if l.len() == 2));
        match resolve_in("nobody", &fleet) {
            Resolved::Nothing(lines) => assert_eq!(lines.len(), 4, "every agent, no shell"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_session_prefix_narrows_the_search() {
        let fleet = fleet();
        assert_eq!(
            resolve_in("play/notes", &fleet),
            resolve_in("notes", &fleet)
        );
        assert_eq!(resolve_in("work/sunny-otter", &fleet), one("work", 1));
        assert_eq!(resolve_in("play/sunny-otter", &fleet), one("play", 5));
        assert!(matches!(
            resolve_in("play/builder", &fleet),
            Resolved::Nothing(_)
        ));
    }

    #[test]
    fn a_shell_pane_is_refused_by_name() {
        assert_eq!(
            resolve_in("quiet-shell", &fleet()),
            Resolved::NotAgent {
                session: "work".into(),
                handle: "quiet-shell".into()
            }
        );
    }
}
