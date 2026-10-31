//! `zellij action watch --json`: the session's changes, pushed by the server as they happen.
//!
//! The server holds one [`EventStream`] per subscribed client. On every look it builds a
//! [`StreamModel`] of the session - its tabs, its panes and the agents in them, after the pane
//! privacy policy - and each stream turns the difference from its last look into lines. The lines
//! are JSON, written here, so the client only prints them and the client/server contract carries
//! one string per line.
//!
//! Every line carries:
//!
//! - `seq`, counted per stream from the `hello` line, which is 0, with no gaps;
//! - `epoch`, a random id that is new every time a server starts, so a reader that reconnects can
//!   tell "the same server, later" from "a new server, whose seq starts again";
//! - `ts`, when the line was written, in milliseconds since the Unix epoch.
//!
//! A stream starts with `hello`, then one `agent` line per agent, one `pane_opened` line per pane
//! and one `tab_opened` line per tab, then `synced`. After that every line is a change.
//!
//! The snapshot carries ids only. What a pane says about itself - its titles, cwd, command and
//! state - is a `pane_changed` line, written for every pane opened after `synced` and whenever
//! one of those fields changes, at most once a second per pane.
//!
//! Nothing here asks the operating system anything: the server hands in the model and the clock.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::agent_detect::{agents_from_pane_list, watched_part};
use crate::data::{AgentListEntry, PaneCommandState, PaneListEntry, PaneProgramStatus};

/// The shortest gap between two `pane_output` lines for one pane, in milliseconds.
pub const PANE_OUTPUT_INTERVAL_MS: u64 = 1000;

/// The shortest gap between two `pane_changed` lines for one pane, in milliseconds. Agents
/// animate their titles, so a title can change many times a second.
pub const PANE_CHANGED_INTERVAL_MS: u64 = 1000;

/// What a `pane_changed` line says about a pane: the fields of its `list-panes --json` row, under
/// the same names and with the same rule for when a key is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PaneDetails {
    pub title: String,
    pub program_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pane_command: Option<String>,
    pub terminal_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_state: Option<PaneCommandState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_command_exit_code: Option<i32>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub program_status: Option<PaneProgramStatus>,
}

impl PaneDetails {
    fn from_entry(entry: &PaneListEntry) -> Self {
        let info = &entry.pane_info;
        PaneDetails {
            title: info.title.clone(),
            program_title: info.program_title.clone(),
            pane_cwd: info.pane_cwd.clone(),
            pane_command: info.pane_command.clone(),
            terminal_command: info.terminal_command.clone(),
            command_state: entry.command_state,
            last_command_exit_code: entry.last_command_exit_code,
            program_status: entry.program_status.clone(),
        }
    }

    /// Whether a reader that holds `self` already knows `other`. `state_at` alone is not news: a
    /// program that reports the same state again has changed nothing, as on an `agent` line.
    fn same_as(&self, other: &PaneDetails) -> bool {
        let without_at = |details: &PaneDetails| {
            let mut details = details.clone();
            if let Some(status) = details.program_status.as_mut() {
                status.at = 0;
            }
            details
        };
        without_at(self) == without_at(other)
    }
}

/// One tab, as the stream knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTab {
    /// The tab's stable id, not its position.
    pub tab_id: usize,
    pub name: String,
}

/// One pane, as the stream knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamPane {
    /// What tells this pane from every other one the server has had: its uuid, or its id where it
    /// has none.
    pub key: String,
    /// `terminal_3`, `plugin_2`: the form every `--pane-id` takes back.
    pub pane_id: String,
    pub handle: String,
    pub tab_id: usize,
    pub exited: bool,
    pub exit_status: Option<i32>,
    pub last_output_at: Option<u64>,
    pub details: PaneDetails,
}

/// The session at one look: what a stream compares against the last one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StreamModel {
    pub tabs: Vec<StreamTab>,
    pub panes: Vec<StreamPane>,
    pub agents: Vec<AgentListEntry>,
}

impl StreamModel {
    /// The model of a session, from its tabs and from the pane list `list-panes` would answer
    /// with. The agents are that list's agents, exactly as `list-agents` projects them.
    pub fn new(tabs: Vec<StreamTab>, panes: Vec<PaneListEntry>) -> Self {
        let stream_panes = panes
            .iter()
            .map(|entry| {
                let info = &entry.pane_info;
                let pane_id = if info.is_plugin {
                    format!("plugin_{}", info.id)
                } else {
                    format!("terminal_{}", info.id)
                };
                StreamPane {
                    key: if info.uuid.is_empty() {
                        pane_id.clone()
                    } else {
                        info.uuid.clone()
                    },
                    pane_id,
                    handle: info.handle.clone(),
                    tab_id: info.tab_id,
                    exited: info.exited,
                    exit_status: info.exit_status,
                    last_output_at: info.last_output_at,
                    details: PaneDetails::from_entry(entry),
                }
            })
            .collect();
        StreamModel {
            tabs,
            panes: stream_panes,
            agents: agents_from_pane_list(panes),
        }
    }
}

/// One line of the stream, before `seq`, `epoch` and `ts` are put on it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Hello {
        server_version: String,
        /// The server's own build id, hex. The client turns it into `server_build`.
        #[serde(skip_serializing_if = "Option::is_none")]
        server_build_id: Option<String>,
        session: String,
    },
    Synced,
    Agent(AgentListEntry),
    Gone {
        pane_id: u32,
        handle: String,
    },
    PaneOpened {
        pane_id: String,
        handle: String,
        tab_id: usize,
    },
    PaneExited {
        pane_id: String,
        handle: String,
        exit_status: Option<i32>,
    },
    PaneClosed {
        pane_id: String,
        handle: String,
    },
    PaneOutput {
        pane_id: String,
        handle: String,
        last_output_at: u64,
    },
    PaneChanged {
        pane_id: String,
        handle: String,
        tab_id: usize,
        #[serde(flatten)]
        details: PaneDetails,
    },
    TabOpened {
        tab_id: usize,
    },
    TabClosed {
        tab_id: usize,
    },
    TabRenamed {
        tab_id: usize,
        name: String,
    },
}

impl StreamEvent {
    /// Whether `list-agents --watch` prints this line.
    pub fn is_agent_line(&self) -> bool {
        matches!(self, StreamEvent::Agent(_) | StreamEvent::Gone { .. })
    }
}

#[derive(Serialize)]
struct Stamped<'a> {
    #[serde(flatten)]
    event: &'a StreamEvent,
    seq: u64,
    epoch: &'a str,
    ts: u64,
}

/// What a stream remembers of one pane between looks.
#[derive(Debug, Clone)]
struct SeenPane {
    pane: StreamPane,
    /// The newest `last_output_at` this stream has said, or found at the snapshot.
    output_reported: Option<u64>,
    /// When the stream last wrote a `pane_output` line for this pane.
    output_line_at: Option<u64>,
    /// The tab and details the reader last heard for this pane: from a `pane_changed` line, or,
    /// for a pane in the snapshot, from the `list-tree` read that follows it.
    changed_reported: (usize, PaneDetails),
    /// When the stream last wrote a `pane_changed` line for this pane.
    changed_line_at: Option<u64>,
}

/// One subscriber's stream: its count, and what it last told its reader.
#[derive(Debug, Clone)]
pub struct EventStream {
    epoch: String,
    next_seq: u64,
    agents_only: bool,
    tabs: BTreeMap<usize, String>,
    panes: BTreeMap<String, SeenPane>,
    agents: BTreeMap<u32, (String, String)>,
}

impl EventStream {
    /// A new stream, and its first lines: `hello`, the snapshot, `synced`.
    ///
    /// `agents_only` is `list-agents --watch`: the stream still counts every line it writes, but
    /// writes only `hello`, the agent lines and `synced`.
    pub fn start(
        epoch: &str,
        hello: StreamEvent,
        model: &StreamModel,
        agents_only: bool,
        now_ms: u64,
    ) -> (Self, Vec<String>) {
        let mut stream = EventStream {
            epoch: epoch.to_owned(),
            next_seq: 0,
            agents_only,
            tabs: BTreeMap::new(),
            panes: BTreeMap::new(),
            agents: BTreeMap::new(),
        };
        let mut events = vec![hello];
        for agent in &model.agents {
            events.push(StreamEvent::Agent(agent.clone()));
        }
        for pane in &model.panes {
            events.push(StreamEvent::PaneOpened {
                pane_id: pane.pane_id.clone(),
                handle: pane.handle.clone(),
                tab_id: pane.tab_id,
            });
        }
        for tab in &model.tabs {
            events.push(StreamEvent::TabOpened { tab_id: tab.tab_id });
        }
        events.push(StreamEvent::Synced);
        stream.remember(model, None);
        let lines = stream.stamp(events, now_ms);
        (stream, lines)
    }

    /// The lines one more look is worth. An unchanged session is worth none.
    ///
    /// The order inside one look is the order a reader can act on: a tab before the panes in it,
    /// a pane before the agent in it, an agent's `gone` before its pane closes, a pane before its
    /// tab closes.
    pub fn advance(&mut self, model: &StreamModel, now_ms: u64) -> Vec<String> {
        let mut events = Vec::new();
        let current_tabs: BTreeMap<usize, &StreamTab> =
            model.tabs.iter().map(|tab| (tab.tab_id, tab)).collect();
        for tab in &model.tabs {
            match self.tabs.get(&tab.tab_id) {
                None => events.push(StreamEvent::TabOpened { tab_id: tab.tab_id }),
                Some(name) if name != &tab.name => events.push(StreamEvent::TabRenamed {
                    tab_id: tab.tab_id,
                    name: tab.name.clone(),
                }),
                Some(_) => {},
            }
        }
        let current_panes: BTreeSet<&str> =
            model.panes.iter().map(|pane| pane.key.as_str()).collect();
        for pane in &model.panes {
            let seen = self.panes.get(&pane.key);
            if seen.is_none() {
                events.push(StreamEvent::PaneOpened {
                    pane_id: pane.pane_id.clone(),
                    handle: pane.handle.clone(),
                    tab_id: pane.tab_id,
                });
            }
            let was_exited = seen.map_or(false, |seen| seen.pane.exited);
            if pane.exited && !was_exited {
                events.push(StreamEvent::PaneExited {
                    pane_id: pane.pane_id.clone(),
                    handle: pane.handle.clone(),
                    exit_status: pane.exit_status,
                });
            }
        }
        let current_agents: BTreeMap<u32, &AgentListEntry> = model
            .agents
            .iter()
            .map(|agent| (agent.pane_id, agent))
            .collect();
        for (pane_id, (_, handle)) in &self.agents {
            let replaced = current_agents
                .get(pane_id)
                .map_or(true, |agent| &agent.handle != handle);
            if replaced {
                events.push(StreamEvent::Gone {
                    pane_id: *pane_id,
                    handle: handle.clone(),
                });
            }
        }
        for agent in &model.agents {
            let part = watched_part(agent);
            let changed = self
                .agents
                .get(&agent.pane_id)
                .map_or(true, |(seen, _)| seen != &part);
            if changed {
                events.push(StreamEvent::Agent(agent.clone()));
            }
        }
        for (key, seen) in &self.panes {
            if !current_panes.contains(key.as_str()) {
                events.push(StreamEvent::PaneClosed {
                    pane_id: seen.pane.pane_id.clone(),
                    handle: seen.pane.handle.clone(),
                });
            }
        }
        for tab_id in self.tabs.keys() {
            if !current_tabs.contains_key(tab_id) {
                events.push(StreamEvent::TabClosed { tab_id: *tab_id });
            }
        }
        // details and output last: they are the kinds of line that can wait for a later look
        for pane in &model.panes {
            let Some(seen) = self.panes.get_mut(&pane.key) else {
                // a pane opened after the snapshot: its details follow its `pane_opened`, so a
                // reader that read `list-tree` once, after `synced`, never has to read it again
                events.push(StreamEvent::PaneChanged {
                    pane_id: pane.pane_id.clone(),
                    handle: pane.handle.clone(),
                    tab_id: pane.tab_id,
                    details: pane.details.clone(),
                });
                continue;
            };
            let (reported_tab, reported) = &seen.changed_reported;
            let changed = *reported_tab != pane.tab_id || !reported.same_as(&pane.details);
            let due = seen.changed_line_at.map_or(true, |at| {
                now_ms.saturating_sub(at) >= PANE_CHANGED_INTERVAL_MS
            });
            if changed && due {
                seen.changed_reported = (pane.tab_id, pane.details.clone());
                seen.changed_line_at = Some(now_ms);
                events.push(StreamEvent::PaneChanged {
                    pane_id: pane.pane_id.clone(),
                    handle: pane.handle.clone(),
                    tab_id: pane.tab_id,
                    details: pane.details.clone(),
                });
            }
        }
        for pane in &model.panes {
            let Some(output_at) = pane.last_output_at else {
                continue;
            };
            let Some(seen) = self.panes.get_mut(&pane.key) else {
                // a pane opened in this look: its output is part of opening, not news
                continue;
            };
            let newer = seen.output_reported.map_or(true, |at| output_at > at);
            let due = seen.output_line_at.map_or(true, |at| {
                now_ms.saturating_sub(at) >= PANE_OUTPUT_INTERVAL_MS
            });
            if newer && due {
                seen.output_reported = Some(output_at);
                seen.output_line_at = Some(now_ms);
                events.push(StreamEvent::PaneOutput {
                    pane_id: pane.pane_id.clone(),
                    handle: pane.handle.clone(),
                    last_output_at: output_at,
                });
            }
        }
        self.remember(model, Some(now_ms));
        self.stamp(events, now_ms)
    }

    /// Take `model` as what the reader now knows, keeping each pane's output and details
    /// bookkeeping. `opened_at` is when a pane new to this look had its `pane_changed` written:
    /// none for the snapshot, which writes none.
    fn remember(&mut self, model: &StreamModel, opened_at: Option<u64>) {
        self.tabs = model
            .tabs
            .iter()
            .map(|tab| (tab.tab_id, tab.name.clone()))
            .collect();
        let mut panes = BTreeMap::new();
        for pane in &model.panes {
            let seen = match self.panes.remove(&pane.key) {
                Some(seen) => SeenPane {
                    pane: pane.clone(),
                    ..seen
                },
                // a pane is opened with whatever output it already has: only output after that
                // is a `pane_output` line. Its details are the ones its `pane_changed` just
                // said, or, in the snapshot, the ones `list-tree` says
                None => SeenPane {
                    pane: pane.clone(),
                    output_reported: pane.last_output_at,
                    output_line_at: None,
                    changed_reported: (pane.tab_id, pane.details.clone()),
                    changed_line_at: opened_at,
                },
            };
            panes.insert(pane.key.clone(), seen);
        }
        self.panes = panes;
        self.agents = model
            .agents
            .iter()
            .map(|agent| (agent.pane_id, (watched_part(agent), agent.handle.clone())))
            .collect();
    }

    fn stamp(&mut self, events: Vec<StreamEvent>, now_ms: u64) -> Vec<String> {
        let mut lines = Vec::with_capacity(events.len());
        for event in events {
            let written = match &event {
                StreamEvent::Hello { .. } | StreamEvent::Synced => true,
                event => !self.agents_only || event.is_agent_line(),
            };
            if !written {
                continue;
            }
            let line = serde_json::to_string(&Stamped {
                event: &event,
                seq: self.next_seq,
                epoch: &self.epoch,
                ts: now_ms,
            });
            if let Ok(line) = line {
                self.next_seq += 1;
                lines.push(line);
            }
        }
        lines
    }
}

/// A fresh epoch: 16 hex digits, new for every server.
///
/// It needs to differ between two servers of one session, not to be secret, so the clock and the
/// process id mixed through the standard hasher are enough.
pub fn new_epoch() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    hasher.write_u32(std::process::id());
    format!("{:016x}", hasher.finish())
}

/// Milliseconds since the Unix epoch, the unit of `ts` and of `last_output_at`.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// The `server_build` word for a `hello` line, from the build id the server sent and this
/// binary's own: `same`, `different`, or `unknown` when either side has none.
pub fn server_build_word(theirs: Option<&str>, ours: Option<&str>) -> &'static str {
    match (theirs, ours) {
        (Some(theirs), Some(ours)) if theirs == ours => "same",
        (Some(_), Some(_)) => "different",
        _ => "unknown",
    }
}

/// The `hello` line as the client prints it: with `server_build`, which only the client can
/// decide, since it compares the server's build id with its own. Any other line is unchanged.
pub fn with_server_build(line: &str, own_build_id: Option<&str>) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_owned();
    };
    if value.get("event").and_then(|event| event.as_str()) != Some("hello") {
        return line.to_owned();
    }
    let theirs = value.get("server_build_id").and_then(|id| id.as_str());
    let word = server_build_word(theirs, own_build_id);
    match line.strip_suffix('}') {
        Some(body) => format!("{},\"server_build\":\"{}\"}}", body, word),
        None => line.to_owned(),
    }
}

/// An `agent` line with `self` set when it is the caller's own pane, as `list-agents` marks it.
/// The server cannot tell which pane asked, so the client does.
pub fn with_self_marked(line: &str, own_pane: Option<u32>) -> String {
    const UNMARKED: &str = "{\"event\":\"agent\",\"self\":false,";
    let Some(own_pane) = own_pane else {
        return line.to_owned();
    };
    if !line.starts_with(UNMARKED) {
        return line.to_owned();
    }
    let is_own = serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| value.get("pane_id").and_then(|id| id.as_u64()))
        == Some(own_pane as u64);
    if !is_own {
        return line.to_owned();
    }
    format!(
        "{{\"event\":\"agent\",\"self\":true,{}",
        &line[UNMARKED.len()..]
    )
}

/// The `event` of a line, or an empty string for a line that is not one.
pub fn event_of(line: &str) -> String {
    serde_json::from_str::<serde_json::Value>(line)
        .ok()
        .and_then(|value| {
            value
                .get("event")
                .and_then(|event| event.as_str())
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

/// Whether nobody reads this process's stdout any more: the far end of its pipe closed.
///
/// A watch on a quiet session writes nothing for a long time, so a write failing is not enough
/// to notice that its reader left. A pipe whose reader has closed reports an error or a hang-up
/// to `poll` without anything being written; a terminal or a file never does.
#[cfg(unix)]
pub fn stdout_reader_gone() -> bool {
    let mut fd = libc::pollfd {
        fd: 1,
        events: 0,
        revents: 0,
    };
    // SAFETY: one valid pollfd, a zero timeout
    let ready = unsafe { libc::poll(&mut fd, 1, 0) };
    ready > 0 && fd.revents & (libc::POLLERR | libc::POLLHUP) != 0
}

#[cfg(not(unix))]
pub fn stdout_reader_gone() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{PaneInfo, PaneProgramStatus};
    use serde_json::Value;

    fn pane(id: u32, handle: &str, tab_id: usize, command: Option<&str>) -> PaneListEntry {
        PaneListEntry {
            pane_info: PaneInfo {
                id,
                handle: handle.to_owned(),
                tab_id,
                uuid: format!("uuid-{id}"),
                is_selectable: true,
                pane_command: command.map(str::to_owned),
                title: "title".to_owned(),
                ..Default::default()
            },
            tab_position: tab_id,
            tab_name: format!("tab{tab_id}"),
            agent: command.and_then(|command| {
                crate::agent_detect::detect_command_line(command, &Default::default())
            }),
            command_state: None,
            last_command_exit_code: None,
            input_since_prompt: false,
            input_since_state: false,
            prefilled_command: None,
            program_status: None,
        }
    }

    fn tab(tab_id: usize, name: &str) -> StreamTab {
        StreamTab {
            tab_id,
            name: name.to_owned(),
        }
    }

    fn hello() -> StreamEvent {
        StreamEvent::Hello {
            server_version: "0.0.0".into(),
            server_build_id: Some("abcd".into()),
            session: "s".into(),
        }
    }

    fn parse(lines: &[String]) -> Vec<Value> {
        lines
            .iter()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn events(lines: &[String]) -> Vec<String> {
        parse(lines)
            .iter()
            .map(|v| v["event"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn the_snapshot_is_hello_agents_panes_tabs_then_synced() {
        let model = StreamModel::new(
            vec![tab(0, "one"), tab(1, "two")],
            vec![
                pane(1, "sunny-otter", 0, Some("claude")),
                pane(2, "brisk-heron", 1, None),
            ],
        );
        let (_, lines) = EventStream::start("ep", hello(), &model, false, 5);
        assert_eq!(
            events(&lines),
            vec![
                "hello",
                "agent",
                "pane_opened",
                "pane_opened",
                "tab_opened",
                "tab_opened",
                "synced"
            ]
        );
        let parsed = parse(&lines);
        assert_eq!(parsed[0]["seq"], 0);
        assert_eq!(parsed[0]["session"], "s");
        assert_eq!(parsed[0]["server_version"], "0.0.0");
        assert_eq!(parsed[1]["handle"], "sunny-otter");
        assert_eq!(parsed[1]["kind"], "claude");
        assert_eq!(parsed[2]["pane_id"], "terminal_1");
        assert_eq!(parsed[2]["tab_id"], 0);
        assert_eq!(parsed[6]["seq"], 6);
        for line in &parsed {
            assert_eq!(line["epoch"], "ep");
            assert_eq!(line["ts"], 5);
        }
    }

    #[test]
    fn seq_has_no_gaps_across_looks_and_an_unchanged_look_says_nothing() {
        let mut panes = vec![pane(1, "a-b", 0, None)];
        let model = StreamModel::new(vec![tab(0, "t")], panes.clone());
        let (mut stream, lines) = EventStream::start("ep", hello(), &model, false, 0);
        let mut seqs: Vec<u64> = parse(&lines)
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect();
        assert!(stream.advance(&model, 1).is_empty());
        panes.push(pane(2, "c-d", 0, None));
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t")], panes.clone()), 2);
        seqs.extend(parse(&lines).iter().map(|v| v["seq"].as_u64().unwrap()));
        panes.remove(0);
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t")], panes), 3);
        seqs.extend(parse(&lines).iter().map(|v| v["seq"].as_u64().unwrap()));
        let expected: Vec<u64> = (0..seqs.len() as u64).collect();
        assert_eq!(seqs, expected);
    }

    #[test]
    fn two_epochs_differ() {
        assert_ne!(new_epoch(), new_epoch());
        assert_eq!(new_epoch().len(), 16);
    }

    #[test]
    fn every_pane_line_carries_its_handle() {
        let model = StreamModel::new(vec![tab(0, "t")], vec![pane(1, "a-b", 0, None)]);
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
        let mut exited = pane(1, "a-b", 0, None);
        exited.pane_info.exited = true;
        exited.pane_info.exit_status = Some(1);
        exited.pane_info.last_output_at = Some(50);
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t")], vec![exited]), 2000);
        let parsed = parse(&lines);
        assert_eq!(events(&lines), vec!["pane_exited", "pane_output"]);
        assert_eq!(parsed[0]["exit_status"], 1);
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t")], vec![]), 3000);
        assert_eq!(events(&lines), vec!["pane_closed"]);
        for line in parse(&lines).iter().chain(parsed.iter()) {
            assert_eq!(line["handle"], "a-b", "{line}");
            assert_eq!(line["pane_id"], "terminal_1", "{line}");
        }
        // a pane that exited with no status says so as null, not as 0
        let model = StreamModel::new(vec![tab(0, "t")], vec![pane(2, "c-d", 0, None)]);
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
        let mut killed = pane(2, "c-d", 0, None);
        killed.pane_info.exited = true;
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t")], vec![killed]), 1);
        assert_eq!(parse(&lines)[0]["exit_status"], Value::Null);
    }

    #[test]
    fn tabs_open_rename_and_close() {
        let model = StreamModel::new(vec![tab(0, "t")], vec![]);
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
        let lines = stream.advance(
            &StreamModel::new(vec![tab(0, "renamed"), tab(4, "new")], vec![]),
            1,
        );
        assert_eq!(events(&lines), vec!["tab_renamed", "tab_opened"]);
        assert_eq!(parse(&lines)[0]["name"], "renamed");
        assert_eq!(parse(&lines)[1]["tab_id"], 4);
        let lines = stream.advance(&StreamModel::new(vec![tab(4, "new")], vec![]), 2);
        assert_eq!(events(&lines), vec!["tab_closed"]);
        assert_eq!(parse(&lines)[0]["tab_id"], 0);
    }

    #[test]
    fn a_new_tab_comes_before_its_pane_and_a_gone_agent_before_its_pane_closes() {
        let model = StreamModel::new(vec![tab(0, "t")], vec![]);
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
        let opened = StreamModel::new(
            vec![tab(0, "t"), tab(1, "u")],
            vec![pane(3, "a-b", 1, Some("claude"))],
        );
        assert_eq!(
            events(&stream.advance(&opened, 1)),
            vec!["tab_opened", "pane_opened", "agent", "pane_changed"]
        );
        let closed = StreamModel::new(vec![tab(0, "t")], vec![]);
        assert_eq!(
            events(&stream.advance(&closed, 2)),
            vec!["gone", "pane_closed", "tab_closed"]
        );
    }

    #[test]
    fn an_agent_line_is_a_change_of_identity_or_state_and_never_of_title() {
        let mut entry = pane(1, "a-b", 0, Some("claude"));
        let model = StreamModel::new(vec![tab(0, "t")], vec![entry.clone()]);
        // agents only: the `pane_changed` lines a title or state change is also worth are not
        // what this test is about
        let (mut stream, _) = EventStream::start("ep", hello(), &model, true, 0);
        entry.pane_info.title = "spinner frame".into();
        let look = |entry: &PaneListEntry| StreamModel::new(vec![tab(0, "t")], vec![entry.clone()]);
        assert!(
            stream.advance(&look(&entry), 1).is_empty(),
            "a title change"
        );
        entry.program_status = Some(PaneProgramStatus {
            state: "working".into(),
            at: 1,
            ..Default::default()
        });
        let lines = stream.advance(&look(&entry), 2);
        assert_eq!(events(&lines), vec!["agent"]);
        assert_eq!(parse(&lines)[0]["state"], "working");
        // the same state reported again, later
        entry.program_status.as_mut().unwrap().at = 9;
        assert!(stream.advance(&look(&entry), 3).is_empty());
        entry.program_status.as_mut().unwrap().state = "blocked".into();
        entry.program_status.as_mut().unwrap().kind = Some("permission".into());
        let lines = stream.advance(&look(&entry), 4);
        assert_eq!(parse(&lines)[0]["state_kind"], "permission");
        entry.agent.as_mut().unwrap().agent_id = Some("session-1".into());
        assert_eq!(events(&stream.advance(&look(&entry), 5)), vec!["agent"]);
        // a renamed handle is the same pane under a new name: the old agent line is withdrawn
        entry.pane_info.handle = "c-d".into();
        let lines = stream.advance(&look(&entry), 6);
        assert_eq!(events(&lines), vec!["gone", "agent"]);
        assert_eq!(parse(&lines)[0]["handle"], "a-b");
        assert_eq!(parse(&lines)[1]["handle"], "c-d");
        // the agent left the pane; the pane stays
        let shell = pane(1, "c-d", 0, None);
        let lines = stream.advance(&look(&shell), 7);
        assert_eq!(events(&lines), vec!["gone"]);
        assert_eq!(parse(&lines)[0]["pane_id"], 1);
    }

    #[test]
    fn pane_output_is_at_most_one_line_per_pane_per_second_and_the_last_one_is_not_lost() {
        let mut entry = pane(1, "a-b", 0, None);
        entry.pane_info.last_output_at = Some(100);
        let look = |entry: &PaneListEntry| StreamModel::new(vec![tab(0, "t")], vec![entry.clone()]);
        let (mut stream, _) = EventStream::start("ep", hello(), &look(&entry), false, 1000);
        // output from before the subscription is not news
        assert!(stream.advance(&look(&entry), 1001).is_empty());
        entry.pane_info.last_output_at = Some(1500);
        let lines = stream.advance(&look(&entry), 1500);
        assert_eq!(events(&lines), vec!["pane_output"]);
        assert_eq!(parse(&lines)[0]["last_output_at"], 1500);
        assert_eq!(parse(&lines)[0]["handle"], "a-b");
        // more output inside the second: held back
        entry.pane_info.last_output_at = Some(1700);
        assert!(stream.advance(&look(&entry), 1700).is_empty());
        entry.pane_info.last_output_at = Some(2400);
        assert!(stream.advance(&look(&entry), 2400).is_empty());
        // the output stopped; the next look after the second has passed still reports the newest
        let lines = stream.advance(&look(&entry), 2500);
        assert_eq!(events(&lines), vec!["pane_output"]);
        assert_eq!(parse(&lines)[0]["last_output_at"], 2400);
        // nothing new: nothing said
        assert!(stream.advance(&look(&entry), 9000).is_empty());
    }

    #[test]
    fn pane_changed_is_a_change_of_title_cwd_command_state_or_tab() {
        let mut entry = pane(1, "a-b", 0, None);
        entry.pane_info.pane_cwd = Some("/home".into());
        let look = |entry: &PaneListEntry| {
            StreamModel::new(vec![tab(0, "t"), tab(1, "u")], vec![entry.clone()])
        };
        let (mut stream, lines) = EventStream::start("ep", hello(), &look(&entry), false, 0);
        // the snapshot is ids only
        assert!(!events(&lines).contains(&"pane_changed".to_owned()));
        // nothing changed: nothing said, however often the stream looks
        assert!(stream.advance(&look(&entry), 1).is_empty());
        assert!(stream.advance(&look(&entry), 5000).is_empty());
        // a field this line does not carry is not a change
        entry.pane_info.pane_x = 7;
        entry.input_since_prompt = true;
        assert!(stream.advance(&look(&entry), 5001).is_empty());
        let mut now = 10_000;
        let mut expect_change = |entry: &PaneListEntry, stream: &mut EventStream| {
            let lines = stream.advance(&look(entry), now);
            now += PANE_CHANGED_INTERVAL_MS;
            assert_eq!(events(&lines), vec!["pane_changed"]);
            parse(&lines).remove(0)
        };
        entry.pane_info.title = "renamed".into();
        let line = expect_change(&entry, &mut stream);
        assert_eq!(line["pane_id"], "terminal_1");
        assert_eq!(line["handle"], "a-b");
        assert_eq!(line["tab_id"], 0);
        assert_eq!(line["title"], "renamed");
        assert_eq!(line["pane_cwd"], "/home");
        // the same rule for an absent key as `list-panes --json`
        assert_eq!(line["program_title"], Value::Null);
        assert_eq!(line["terminal_command"], Value::Null);
        assert!(line.get("pane_command").is_none());
        assert!(line.get("state").is_none());
        entry.pane_info.program_title = Some("vim".into());
        assert_eq!(expect_change(&entry, &mut stream)["program_title"], "vim");
        entry.pane_info.pane_cwd = Some("/tmp".into());
        assert_eq!(expect_change(&entry, &mut stream)["pane_cwd"], "/tmp");
        entry.pane_info.pane_command = Some("htop".into());
        assert_eq!(expect_change(&entry, &mut stream)["pane_command"], "htop");
        entry.pane_info.terminal_command = Some("make test".into());
        assert_eq!(
            expect_change(&entry, &mut stream)["terminal_command"],
            "make test"
        );
        entry.command_state = Some(PaneCommandState::Done);
        entry.last_command_exit_code = Some(2);
        let line = expect_change(&entry, &mut stream);
        assert_eq!(line["command_state"], "done");
        assert_eq!(line["last_command_exit_code"], 2);
        entry.program_status = Some(PaneProgramStatus {
            state: "working".into(),
            title: Some("fix the bug".into()),
            at: 1,
            ..Default::default()
        });
        let line = expect_change(&entry, &mut stream);
        assert_eq!(line["state"], "working");
        assert_eq!(line["state_title"], "fix the bug");
        assert_eq!(line["state_at"], 1);
        // the same state reported again is not a change
        entry.program_status.as_mut().unwrap().at = 99;
        assert!(stream.advance(&look(&entry), 1_000_000).is_empty());
        // a pane moved to another tab is a change, with the new tab
        entry.pane_info.tab_id = 1;
        assert_eq!(expect_change(&entry, &mut stream)["tab_id"], 1);
        // a renamed handle is carried, but is not a change on its own
        entry.pane_info.handle = "c-d".into();
        assert!(stream.advance(&look(&entry), 1_000_000).is_empty());
        entry.pane_info.title = "again".into();
        assert_eq!(expect_change(&entry, &mut stream)["handle"], "c-d");
    }

    #[test]
    fn pane_changed_is_at_most_one_line_per_pane_per_second_and_the_last_one_is_not_lost() {
        let mut entry = pane(1, "a-b", 0, None);
        let other_entry = pane(2, "c-d", 0, None);
        let look = |entry: &PaneListEntry, other: &PaneListEntry| {
            StreamModel::new(vec![tab(0, "t")], vec![entry.clone(), other.clone()])
        };
        let (mut stream, lines) =
            EventStream::start("ep", hello(), &look(&entry, &other_entry), false, 1000);
        let mut seqs: Vec<u64> = parse(&lines)
            .iter()
            .map(|v| v["seq"].as_u64().unwrap())
            .collect();
        let mut said = Vec::new();
        let mut advance = |stream: &mut EventStream, model: &StreamModel, at: u64| {
            let lines = stream.advance(model, at);
            seqs.extend(parse(&lines).iter().map(|v| v["seq"].as_u64().unwrap()));
            said.extend(parse(&lines));
            lines
        };
        // the first change after a quiet second is said at once
        entry.pane_info.title = "frame 1".into();
        let lines = advance(&mut stream, &look(&entry, &other_entry), 1100);
        assert_eq!(parse(&lines)[0]["title"], "frame 1");
        // an animated title inside the second: held back
        for (at, frame) in [(1200, "frame 2"), (1500, "frame 3"), (2000, "frame 4")] {
            entry.pane_info.title = frame.into();
            assert!(advance(&mut stream, &look(&entry, &other_entry), at).is_empty());
        }
        // another pane is not held back by this one's limit
        let mut other = other_entry.clone();
        other.pane_info.title = "other".into();
        let lines = advance(&mut stream, &look(&entry, &other), 2050);
        assert_eq!(events(&lines), vec!["pane_changed"]);
        assert_eq!(parse(&lines)[0]["pane_id"], "terminal_2");
        // the title stopped; the stream's tick after the second says the newest frame
        let lines = advance(&mut stream, &look(&entry, &other), 2100);
        assert_eq!(events(&lines), vec!["pane_changed"]);
        assert_eq!(parse(&lines)[0]["title"], "frame 4");
        assert_eq!(parse(&lines)[0]["ts"], 2100);
        // nothing new: nothing said
        assert!(advance(&mut stream, &look(&entry, &other), 9000).is_empty());
        // a change that is undone inside the second is no change at all
        entry.pane_info.title = "frame 5".into();
        advance(&mut stream, &look(&entry, &other), 9100);
        entry.pane_info.title = "frame 6".into();
        advance(&mut stream, &look(&entry, &other), 9200);
        entry.pane_info.title = "frame 5".into();
        assert!(advance(&mut stream, &look(&entry, &other), 10_200).is_empty());
        assert_eq!(said.len(), 4);
        let expected: Vec<u64> = (0..seqs.len() as u64).collect();
        assert_eq!(seqs, expected);
    }

    #[test]
    fn a_pane_opened_after_the_snapshot_is_followed_by_its_details() {
        let model = StreamModel::new(vec![tab(0, "t")], vec![]);
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
        let mut entry = pane(4, "a-b", 0, None);
        entry.pane_info.pane_cwd = Some("/srv".into());
        let look = |entry: &PaneListEntry| StreamModel::new(vec![tab(0, "t")], vec![entry.clone()]);
        let lines = stream.advance(&look(&entry), 10);
        assert_eq!(events(&lines), vec!["pane_opened", "pane_changed"]);
        let parsed = parse(&lines);
        // `pane_opened` stays ids only
        assert!(parsed[0].get("title").is_none());
        assert_eq!(parsed[1]["pane_cwd"], "/srv");
        assert_eq!(parsed[1]["title"], "title");
        // the line it opened with counts towards the limit
        entry.pane_info.title = "prompt".into();
        assert!(stream.advance(&look(&entry), 500).is_empty());
        let lines = stream.advance(&look(&entry), 1010);
        assert_eq!(parse(&lines)[0]["title"], "prompt");
        // `list-agents --watch` never sees one
        let (mut agents, _) = EventStream::start("ep", hello(), &model, true, 0);
        assert!(agents.advance(&look(&entry), 10).is_empty());
    }

    #[test]
    fn agents_only_writes_hello_agent_lines_and_synced_and_counts_only_those() {
        let model = StreamModel::new(
            vec![tab(0, "t")],
            vec![pane(1, "a-b", 0, Some("claude")), pane(2, "c-d", 0, None)],
        );
        let (mut stream, lines) = EventStream::start("ep", hello(), &model, true, 0);
        assert_eq!(events(&lines), vec!["hello", "agent", "synced"]);
        let lines = stream.advance(&StreamModel::new(vec![tab(0, "t"), tab(1, "u")], vec![]), 1);
        assert_eq!(events(&lines), vec!["gone"]);
        assert_eq!(parse(&lines)[0]["seq"], 3);
        // the existing `list-agents --watch` gone shape
        assert!(parse(&lines)[0].get("session").is_none());
    }

    #[test]
    fn the_client_adds_server_build_to_hello_and_marks_its_own_agent() {
        let model = StreamModel::new(vec![tab(0, "t")], vec![pane(7, "a-b", 0, Some("claude"))]);
        let (_, lines) = EventStream::start("ep", hello(), &model, false, 0);
        let hello_line = with_server_build(&lines[0], Some("abcd"));
        let parsed: Value = serde_json::from_str(&hello_line).unwrap();
        assert_eq!(parsed["server_build"], "same");
        assert_eq!(parsed["seq"], 0);
        let other: Value =
            serde_json::from_str(&with_server_build(&lines[0], Some("ffff"))).unwrap();
        assert_eq!(other["server_build"], "different");
        // only hello gains it
        assert_eq!(with_server_build(&lines[1], Some("abcd")), lines[1]);
        let mine: Value = serde_json::from_str(&with_self_marked(&lines[1], Some(7))).unwrap();
        assert_eq!(mine["self"], true);
        assert_eq!(mine["seq"], 1);
        assert_eq!(with_self_marked(&lines[1], Some(8)), lines[1]);
        assert_eq!(with_self_marked(&lines[1], None), lines[1]);
        assert_eq!(event_of(&lines[1]), "agent");
        assert_eq!(event_of("not json"), "");
    }

    #[test]
    fn the_build_word() {
        assert_eq!(server_build_word(Some("ab"), Some("ab")), "same");
        assert_eq!(server_build_word(Some("ab"), Some("cd")), "different");
        assert_eq!(server_build_word(None, Some("cd")), "unknown");
    }
}
