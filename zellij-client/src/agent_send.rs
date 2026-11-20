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
/// `~/src/develop` alike, and two such agents are two candidates rather than a pick. The caller's
/// own agent (`is_self`) never answers to a word: "the other agent in my tab" is the one meant.
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

/// Where a default harness is started for a target that names no agent.
#[derive(Debug, PartialEq, Eq)]
pub enum HarnessPlace {
    /// Type the harness command into this idle shell.
    TypeInto { session: String, terminal_id: u32 },
    /// Open a pane in this tab, without focus, and start the harness there.
    NewPaneIn {
        session: String,
        tab_id: usize,
        tab_name: String,
    },
}

/// A shell waiting at a fresh prompt with nothing typed, by its own OSC 133 markers.
///
/// Only the markers count. A shell without them cannot say it is at a prompt, nor whether a person
/// has typed at it, so it never qualifies, and the harness goes into a new pane beside it instead.
pub fn is_idle_shell(candidate: &Candidate) -> bool {
    let pane = candidate.pane;
    candidate.agent.is_none()
        && pane.agent.is_none()
        && !pane.pane_info.is_plugin
        && !pane.pane_info.exited
        && !pane.pane_info.is_held
        && pane.command_state_from_markers() == Some(zellij_utils::data::PaneCommandState::Prompt)
        && !pane.input_since_prompt
        && pane.prefilled_command.is_none()
}

/// Where to start a default harness for `target`, when [`resolve`] found no agent for it.
///
/// The target must name one pane (a handle or a pane id) or one tab by name, and the tab must run
/// no agent, the caller's own included. A named pane that is an idle shell takes the harness.
/// Otherwise the tab takes it in its one idle shell, and in a new pane when it has none or several:
/// a pick between two shells would be a guess. `None` when the target names no tab, several, or a
/// tab that already runs an agent.
pub fn harness_place(target: &str, candidates: &[Candidate]) -> Option<HarnessPlace> {
    let (session_filter, word) = match target.split_once('/') {
        Some((session, word)) if !session.is_empty() && !word.is_empty() => (Some(session), word),
        _ => (None, target),
    };
    let in_scope: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| session_filter.map_or(true, |session| c.session == session))
        .collect();
    let pane_id = pane_id_of(word);
    let named: Vec<&&Candidate> = in_scope
        .iter()
        .filter(|c| {
            !c.pane.pane_info.is_plugin
                && (c.pane.pane_info.handle == word || pane_id == Some(c.pane.pane_info.id))
        })
        .collect();
    let tab_key = |c: &Candidate| (c.session.to_owned(), c.pane.pane_info.tab_id);
    let (session, tab_id) = match named.as_slice() {
        [one] => tab_key(one),
        [] => {
            let mut tabs: Vec<(String, usize)> = in_scope
                .iter()
                .filter(|c| {
                    !c.pane.tab_name.is_empty() && c.pane.tab_name.eq_ignore_ascii_case(word)
                })
                .map(|c| tab_key(c))
                .collect();
            tabs.sort();
            tabs.dedup();
            match tabs.as_slice() {
                [one] => one.clone(),
                _ => return None,
            }
        },
        _ => return None,
    };
    let tab: Vec<&&Candidate> = in_scope
        .iter()
        .filter(|c| c.session == session && c.pane.pane_info.tab_id == tab_id)
        .collect();
    if tab
        .iter()
        .any(|c| c.agent.is_some() || c.pane.agent.is_some())
    {
        return None;
    }
    let tab_name = tab.first()?.pane.tab_name.clone();
    if let [one] = named.as_slice() {
        if is_idle_shell(one) {
            return Some(HarnessPlace::TypeInto {
                session,
                terminal_id: one.pane.pane_info.id,
            });
        }
    }
    let idle: Vec<&&&Candidate> = tab.iter().filter(|c| is_idle_shell(c)).collect();
    match (named.is_empty(), idle.as_slice()) {
        (true, [one]) => Some(HarnessPlace::TypeInto {
            session,
            terminal_id: one.pane.pane_info.id,
        }),
        _ => Some(HarnessPlace::NewPaneIn {
            session,
            tab_id,
            tab_name,
        }),
    }
}

fn answers_to(candidate: &Candidate, word: &str) -> bool {
    let same = |value: &str| !value.is_empty() && value.eq_ignore_ascii_case(word);
    let Some(agent) = candidate.agent.filter(|agent| !agent.is_self) else {
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
    fn the_caller_never_answers_to_a_word() {
        // the sender shares the tab `notes` with one other agent: that one is meant
        let mut fleet = fleet();
        fleet[3].agent.as_mut().unwrap().is_self = true;
        assert_eq!(resolve_in("notes", &fleet), one("play", 5));
        assert!(matches!(resolve_in("scribe", &fleet), Resolved::Nothing(_)));
        // its own handle still names it
        assert_eq!(resolve_in("calm-lynx", &fleet), one("play", 4));
    }

    fn place_in(target: &str, fixtures: &[Fixture]) -> Option<HarnessPlace> {
        let candidates: Vec<Candidate> = fixtures
            .iter()
            .map(|f| Candidate {
                session: f.session,
                pane: &f.pane,
                agent: f.agent.as_ref(),
            })
            .collect();
        harness_place(target, &candidates)
    }

    /// A shell pane in `tab` (tab id `tab_id`), at a fresh marked prompt when `idle`.
    fn shell(id: u32, handle: &str, tab: &str, tab_id: usize, idle: bool) -> Fixture {
        let mut fixture = pane("work", id, handle, tab, None);
        fixture.pane.pane_info.tab_id = tab_id;
        if idle {
            fixture.pane.command_state = Some(zellij_utils::data::PaneCommandState::Prompt);
        }
        fixture
    }

    fn type_into(terminal_id: u32) -> Option<HarnessPlace> {
        Some(HarnessPlace::TypeInto {
            session: "work".into(),
            terminal_id,
        })
    }

    fn new_pane_in(tab_id: usize, tab_name: &str) -> Option<HarnessPlace> {
        Some(HarnessPlace::NewPaneIn {
            session: "work".into(),
            tab_id,
            tab_name: tab_name.into(),
        })
    }

    #[test]
    fn a_tab_with_one_idle_shell_takes_the_harness_there() {
        let panes = vec![shell(7, "idle-shell", "notes", 3, true)];
        assert_eq!(place_in("notes", &panes), type_into(7));
        assert_eq!(place_in("NOTES", &panes), type_into(7));
        assert_eq!(place_in("idle-shell", &panes), type_into(7));
        assert_eq!(place_in("work/notes", &panes), type_into(7));
    }

    #[test]
    fn a_shell_with_typed_text_or_a_prefill_or_no_markers_is_not_idle() {
        let mut typed = shell(7, "typed-shell", "notes", 3, true);
        typed.pane.input_since_prompt = true;
        assert_eq!(place_in("notes", &[typed]), new_pane_in(3, "notes"));
        let mut prefilled = shell(7, "prefill-shell", "notes", 3, true);
        prefilled.pane.prefilled_command = Some("make".into());
        assert_eq!(place_in("notes", &[prefilled]), new_pane_in(3, "notes"));
        let unmarked = shell(7, "plain-shell", "notes", 3, false);
        assert_eq!(place_in("notes", &[unmarked]), new_pane_in(3, "notes"));
        // a prompt the foreground-process tracker inferred, without markers, is not enough
        let mut tracked = shell(7, "tracked-shell", "notes", 3, false);
        tracked.pane.pane_info.command_state.phase =
            zellij_utils::attention::CommandPhase::AtPrompt;
        tracked.pane.command_state = Some(zellij_utils::data::PaneCommandState::Prompt);
        assert_eq!(place_in("notes", &[tracked]), new_pane_in(3, "notes"));
    }

    #[test]
    fn two_idle_shells_are_not_a_pick_unless_one_is_named() {
        let panes = vec![
            shell(7, "first-shell", "notes", 3, true),
            shell(8, "second-shell", "notes", 3, true),
        ];
        assert_eq!(place_in("notes", &panes), new_pane_in(3, "notes"));
        assert_eq!(place_in("second-shell", &panes), type_into(8));
        // a named pane that is busy puts the harness in a new pane in its tab, not in a neighbour
        let mut busy = shell(9, "busy-shell", "notes", 3, false);
        busy.pane.command_state = Some(zellij_utils::data::PaneCommandState::Running);
        let panes = vec![busy, shell(7, "idle-shell", "notes", 3, true)];
        assert_eq!(place_in("busy-shell", &panes), new_pane_in(3, "notes"));
    }

    #[test]
    fn a_tab_with_an_agent_or_no_such_tab_starts_nothing() {
        // `develop` holds sunny-otter's claude; `review` holds a codex
        assert_eq!(place_in("develop", &fleet()), None);
        assert_eq!(place_in("quiet-shell", &fleet()), None);
        assert_eq!(place_in("nowhere", &fleet()), None);
        // the caller's own agent counts: a harness is never started beside the sender
        let mut fleet = fleet();
        fleet[3].agent.as_mut().unwrap().is_self = true;
        assert_eq!(place_in("calm-lynx", &fleet), None);
        // one tab name in two tabs is two places
        let panes = vec![
            shell(7, "one-shell", "notes", 3, true),
            shell(8, "two-shell", "notes", 4, true),
        ];
        assert_eq!(place_in("notes", &panes), None);
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
