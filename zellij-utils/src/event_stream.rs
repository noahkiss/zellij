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
//! Nothing here asks the operating system anything: the server hands in the model and the clock.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::agent_detect::{agents_from_pane_list, watched_part};
use crate::data::{AgentListEntry, PaneListEntry};

/// The shortest gap between two `pane_output` lines for one pane, in milliseconds.
pub const PANE_OUTPUT_INTERVAL_MS: u64 = 1000;

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
        stream.remember(model);
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
        // output last: it is the one kind of line that can wait for a later look
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
        self.remember(model);
        self.stamp(events, now_ms)
    }

    /// Take `model` as what the reader now knows, keeping each pane's output bookkeeping.
    fn remember(&mut self, model: &StreamModel) {
        self.tabs = model
            .tabs
            .iter()
            .map(|tab| (tab.tab_id, tab.name.clone()))
            .collect();
        let mut panes = BTreeMap::new();
        for pane in &model.panes {
            let (output_reported, output_line_at) = match self.panes.remove(&pane.key) {
                Some(seen) => (seen.output_reported, seen.output_line_at),
                // a pane is opened with whatever output it already has: only output after that
                // is a `pane_output` line
                None => (pane.last_output_at, None),
            };
            panes.insert(
                pane.key.clone(),
                SeenPane {
                    pane: pane.clone(),
                    output_reported,
                    output_line_at,
                },
            );
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
            vec!["tab_opened", "pane_opened", "agent"]
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
        let (mut stream, _) = EventStream::start("ep", hello(), &model, false, 0);
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
