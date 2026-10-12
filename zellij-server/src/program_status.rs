//! fork addition: the Program Status Protocol, `OSC 7501`, kept per pane.
//!
//! A program tells its terminal what it is doing:
//!
//! ```text
//! ESC ] 7501 ; state=blocked:kind=permission:app=claude-code:msg=<base64> ST
//! ```
//!
//! The body is `key=value` pairs separated by `:`. `state` is `idle`, `working`, `done`, `blocked`
//! or `error`, or `clear`, which removes records rather than setting one. `id` names a record
//! (`/` makes a child of another), `kind` says why a blocked program waits, `progress` is 0-100,
//! `app` names the program, and `title` and `msg` are base64 UTF-8 text. The specification is at
//! <https://www.superlogical.com/rex/docs/build/program-status>.
//!
//! zellij is the terminal of every pane, so it answers the `OSC 7501 ; ?` feature query itself.
//! That answer is what turns the protocol on: Claude Code (2.1.295 and later) and pi (1.1) send no
//! report until a query has been answered. The pane keeps its records, and `list-panes --json` and
//! `list-agents --json` carry the most urgent one. Nothing is forwarded to the host terminal: it
//! holds one set of records per window, and a zellij client shows many panes.

use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::{
    alphabet::STANDARD as BASE64_STANDARD_ALPHABET,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
    Engine,
};
use zellij_utils::data::PaneProgramStatus;

/// The body of the answer to `OSC 7501 ; ?`: every state and kind this build understands.
pub const QUERY_REPLY_BODY: &str =
    "?:states=idle,working,done,blocked,error:kinds=permission,question,auth";

/// The specification caps the whole sequence.
const MAX_SEQUENCE_BYTES: usize = 4096;
const MAX_MSG_BYTES: usize = 2048;
const MAX_TITLE_BYTES: usize = 192;
const MAX_APP_BYTES: usize = 32;
const MAX_ID_BYTES: usize = 128;
const MAX_ID_SEGMENT_BYTES: usize = 32;
const MAX_ID_DEPTH: usize = 8;
/// A terminal must keep at least 64 records, and evicts the least recently updated past its cap.
pub const MAX_RECORDS: usize = 64;

const BASE64: GeneralPurpose = GeneralPurpose::new(
    &BASE64_STANDARD_ALPHABET,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(State::Idle),
            "working" => Some(State::Working),
            "done" => Some(State::Done),
            "blocked" => Some(State::Blocked),
            "error" => Some(State::Error),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
        }
    }

    /// Which record speaks for a pane holding several: the one that most needs a person.
    fn urgency(self) -> u8 {
        match self {
            State::Blocked => 5,
            State::Error => 4,
            State::Done => 3,
            State::Working => 2,
            State::Idle => 1,
        }
    }

    /// `done` and `error` are results. They survive a new prompt and the program exiting, so
    /// the outcome of a run is still there to be read after it.
    fn is_finished(self) -> bool {
        matches!(self, State::Done | State::Error)
    }
}

/// One report, parsed and checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    /// Empty for the root record.
    pub id: String,
    pub state: State,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
}

/// What an `OSC 7501` sequence asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// `OSC 7501 ; ?`: does this terminal speak the protocol.
    Query,
    Report(Report),
    /// `state=clear`: remove the record named and its children, or every record without an id.
    Clear {
        id: String,
    },
}

/// Parses the params of an `OSC 7501` sequence. `None` for anything the specification says to
/// discard - an oversized sequence, a missing or unknown state, an id or a text field over its
/// limit, base64 that does not decode, or text with control characters in it.
pub fn parse(params: &[&[u8]]) -> Option<Message> {
    if params.first() != Some(&&b"7501"[..]) {
        return None;
    }
    // the parser split the body on `;`. A conforming body has none, but join it back rather
    // than read half of one that did
    let body = params[1..]
        .iter()
        .map(|part| std::str::from_utf8(part).ok())
        .collect::<Option<Vec<&str>>>()?
        .join(";");
    if body.starts_with('?') {
        return Some(Message::Query);
    }
    if body.len() + "\u{1b}]7501;\u{1b}\\".len() > MAX_SEQUENCE_BYTES {
        return None;
    }
    // malformed pairs are skipped, unknown keys ignored, and a repeated key takes its last value
    let mut pairs: BTreeMap<&str, &str> = BTreeMap::new();
    for pair in body.split(':') {
        if let Some((key, value)) = pair.split_once('=') {
            pairs.insert(key, value);
        }
    }
    let id = pairs.get("id").copied().unwrap_or("");
    if !valid_id(id) {
        return None;
    }
    let state = match pairs.get("state").copied()? {
        "clear" => {
            return Some(Message::Clear { id: id.to_owned() });
        },
        state => State::parse(state)?,
    };
    let kind = match (state, pairs.get("kind").copied()) {
        (State::Blocked, Some(kind @ ("permission" | "question" | "auth"))) => {
            Some(kind.to_owned())
        },
        _ => None,
    };
    // an invalid progress is indeterminate, not a reason to drop the report
    let progress = match state {
        State::Working | State::Blocked => pairs
            .get("progress")
            .and_then(|value| value.parse::<u8>().ok())
            .filter(|value| *value <= 100),
        _ => None,
    };
    let app = match pairs.get("app").copied() {
        None | Some("") => None,
        Some(app) if app.len() > MAX_APP_BYTES || has_control(app) => return None,
        Some(app) => Some(app.to_owned()),
    };
    let title = decode_text(pairs.get("title").copied(), MAX_TITLE_BYTES)?;
    let msg = decode_text(pairs.get("msg").copied(), MAX_MSG_BYTES)?;
    Some(Message::Report(Report {
        id: id.to_owned(),
        state,
        kind,
        progress,
        app,
        title,
        msg,
    }))
}

/// `Some(None)` for an absent or empty field, `None` for one the whole report must be dropped for.
fn decode_text(value: Option<&str>, max_bytes: usize) -> Option<Option<String>> {
    let value = match value {
        None | Some("") => return Some(None),
        Some(value) => value,
    };
    let bytes = BASE64.decode(value).ok()?;
    if bytes.len() > max_bytes {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    if has_control(&text) {
        return None;
    }
    Some(Some(text))
}

fn has_control(text: &str) -> bool {
    text.chars().any(|c| c.is_control())
}

fn valid_id(id: &str) -> bool {
    if id.is_empty() {
        return true;
    }
    if id.len() > MAX_ID_BYTES || has_control(id) {
        return false;
    }
    let segments: Vec<&str> = id.split('/').collect();
    segments.len() <= MAX_ID_DEPTH
        && segments
            .iter()
            .all(|segment| !segment.is_empty() && segment.len() <= MAX_ID_SEGMENT_BYTES)
}

/// One record, as the pane keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub state: State,
    pub kind: Option<String>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub title: Option<String>,
    pub msg: Option<String>,
    /// Wall-clock milliseconds when the record was reported.
    pub at: u64,
    /// Report order, which breaks ties `at` cannot: two reports in one millisecond.
    seq: u64,
}

/// A pane's records, by id. The terminal keeps them; the specification leaves that to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProgramStatusRecords {
    records: BTreeMap<String, Record>,
    seq: u64,
    /// `seq` at the last new prompt (`OSC 133;A`). A record at or below it was reported before
    /// that prompt, so by a program that ran earlier. 0 until a prompt is seen.
    prompt_seq: u64,
    /// Input was typed into the pane after the last report. See [`Self::input_since_report`].
    input_since_report: bool,
}

impl ProgramStatusRecords {
    /// Applies a report or a clear. A report replaces its record whole, so keys it omits are gone.
    pub fn apply(&mut self, message: Message, now_ms: u64) {
        match message {
            Message::Query => {},
            Message::Clear { id } => {
                self.input_since_report = false;
                self.clear(&id)
            },
            Message::Report(report) => {
                self.input_since_report = false;
                self.seq += 1;
                self.records.insert(
                    report.id,
                    Record {
                        state: report.state,
                        kind: report.kind,
                        progress: report.progress,
                        app: report.app,
                        title: report.title,
                        msg: report.msg,
                        at: now_ms,
                        seq: self.seq,
                    },
                );
                if self.records.len() > MAX_RECORDS {
                    let oldest = self
                        .records
                        .iter()
                        .min_by_key(|(_, record)| record.seq)
                        .map(|(id, _)| id.clone());
                    if let Some(oldest) = oldest {
                        self.records.remove(&oldest);
                    }
                }
            },
        }
    }

    /// Removes the record `id` and its children. An empty id removes every record.
    pub fn clear(&mut self, id: &str) {
        if id.is_empty() {
            self.records.clear();
            return;
        }
        let prefix = format!("{}/", id);
        self.records
            .retain(|key, _| key != id && !key.starts_with(&prefix));
    }

    /// A new prompt, or the program exiting: the work it described is over. `working`, `blocked`
    /// and `idle` go; a `done` or `error` result stays until something replaces or clears it.
    pub fn drop_unfinished(&mut self) {
        self.records.retain(|_, record| record.state.is_finished());
    }

    /// A new prompt (`OSC 133;A`). The work reported so far is over, as in `drop_unfinished`, and
    /// every record left is marked as reported before this prompt.
    pub fn prompt(&mut self) {
        self.drop_unfinished();
        self.prompt_seq = self.seq;
    }

    /// A command starts (`OSC 133;C`). A result reported before the last prompt belongs to a
    /// program that ran earlier, not to the one starting now, so it goes. A record reported after
    /// that prompt stays: an agent that writes its own marks reports after its own `133;A`, and
    /// claude ends each turn with `133;C`. Nothing goes before the first prompt is seen.
    pub fn command_started(&mut self) {
        let prompt_seq = self.prompt_seq;
        if prompt_seq > 0 {
            self.records.retain(|_, record| record.seq > prompt_seq);
        }
    }

    /// A program asked `OSC 7501 ; ?`. Claude and pi ask once, as they start, so a program is
    /// starting to report here, and every record in the pane belongs to one that came before it.
    pub fn program_started(&mut self) {
        self.records.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Input was typed into the pane: a key, a paste, `write-chars`.
    pub fn note_input(&mut self) {
        self.input_since_report = true;
    }

    /// Whether input was typed into the pane after the program last reported or cleared.
    ///
    /// This is what tells a fresh state from a stale one. Right after a message is typed into an
    /// agent that reported `done`, the pane still holds that `done` until the agent reports
    /// `working`. A wait for the agent to be idle again would answer at once, for the turn
    /// before. With this, a state counts only once it was reported after the last input.
    pub fn input_since_report(&self) -> bool {
        self.input_since_report
    }

    pub fn len(&self) -> usize {
        self.records.len()
    }

    /// The record that speaks for the pane: the most urgent, then the newest.
    pub fn summary(&self) -> Option<PaneProgramStatus> {
        let (id, record) = self
            .records
            .iter()
            .max_by_key(|(_, record)| (record.state.urgency(), record.seq))?;
        Some(PaneProgramStatus {
            state: record.state.as_str().to_owned(),
            kind: record.kind.clone(),
            app: record.app.clone().or_else(|| self.inherited_app(id)),
            title: record.title.clone(),
            msg: record.msg.clone(),
            progress: record.progress,
            at: record.at,
        })
    }

    /// A child record without an `app` takes its nearest ancestor's.
    fn inherited_app(&self, id: &str) -> Option<String> {
        let mut id = id;
        while let Some((parent, _)) = id.rsplit_once('/') {
            if let Some(app) = self.records.get(parent).and_then(|r| r.app.clone()) {
                return Some(app);
            }
            id = parent;
        }
        if id.is_empty() {
            return None;
        }
        self.records.get("").and_then(|root| root.app.clone())
    }
}

/// Milliseconds since the Unix epoch, now.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn osc(body: &str) -> Option<Message> {
        let parts: Vec<&[u8]> = std::iter::once(&b"7501"[..])
            .chain(body.split(';').map(str::as_bytes))
            .collect();
        parse(&parts)
    }

    fn b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    fn report(body: &str) -> Report {
        match osc(body) {
            Some(Message::Report(report)) => report,
            other => panic!("{body:?} is not a report: {other:?}"),
        }
    }

    #[test]
    fn the_query_is_recognised_with_or_without_pairs_after_it() {
        assert_eq!(osc("?"), Some(Message::Query));
        assert_eq!(osc("?:states=idle"), Some(Message::Query));
    }

    #[test]
    fn the_reply_starts_with_the_question_mark_both_agents_look_for() {
        // claude: `e.data.startsWith("?")`; pi: /^\x1b\]7501;\?[^\x07\x1b]*(?:\x07|\x1b\\)$/
        assert!(QUERY_REPLY_BODY.starts_with('?'));
        assert!(!QUERY_REPLY_BODY.contains('\u{1b}') && !QUERY_REPLY_BODY.contains('\u{7}'));
    }

    #[test]
    fn a_claude_report_is_read_whole() {
        let r = report(&format!(
            "state=blocked:app=claude-code:kind=permission:progress=40:title={}:msg={}",
            b64("Edit main.rs"),
            b64("Allow Bash: cargo test?")
        ));
        assert_eq!(r.id, "");
        assert_eq!(r.state, State::Blocked);
        assert_eq!(r.kind.as_deref(), Some("permission"));
        assert_eq!(r.progress, Some(40));
        assert_eq!(r.app.as_deref(), Some("claude-code"));
        assert_eq!(r.title.as_deref(), Some("Edit main.rs"));
        assert_eq!(r.msg.as_deref(), Some("Allow Bash: cargo test?"));
    }

    #[test]
    fn kind_belongs_to_blocked_and_progress_to_working_and_blocked() {
        let r = report("state=done:kind=permission:progress=50");
        assert_eq!(r.kind, None);
        assert_eq!(r.progress, None);
        let r = report("state=working:progress=101");
        assert_eq!(
            r.progress, None,
            "out of range is indeterminate, not an error"
        );
        let r = report("state=working:progress=7");
        assert_eq!(r.progress, Some(7));
    }

    #[test]
    fn what_the_specification_discards_is_discarded() {
        assert_eq!(osc("app=x"), None, "no state");
        assert_eq!(osc("state=sleeping"), None, "unknown state");
        assert_eq!(osc("state=done:msg=not base64!"), None);
        assert_eq!(osc(&format!("state=done:msg={}", b64("a\nb"))), None);
        assert_eq!(
            osc(&format!("state=done:title={}", b64(&"t".repeat(193)))),
            None
        );
        assert_eq!(osc(&format!("state=done:app={}", "a".repeat(33))), None);
        assert_eq!(osc("state=done:id=a//b"), None);
        assert_eq!(
            osc(&format!("state=done:id={}", "a/".repeat(8) + "a")),
            None
        );
        assert_eq!(osc(&format!("state=done:x={}", "a".repeat(4096))), None);
    }

    #[test]
    fn malformed_pairs_are_skipped_and_the_last_repeat_wins() {
        let r = report("junk:state=idle:state=working:other=1");
        assert_eq!(r.state, State::Working);
    }

    #[test]
    fn clear_is_not_a_state() {
        assert_eq!(
            osc("state=clear"),
            Some(Message::Clear { id: String::new() })
        );
        assert_eq!(
            osc("state=clear:id=build"),
            Some(Message::Clear {
                id: "build".to_owned()
            })
        );
    }

    #[test]
    fn another_osc_is_not_ours() {
        assert_eq!(parse(&[b"7502", b"state=idle"]), None);
    }

    #[test]
    fn a_report_replaces_its_record_and_the_most_urgent_speaks_for_the_pane() {
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=working:app=claude-code").unwrap(), 10);
        let summary = records.summary().unwrap();
        assert_eq!(summary.state, "working");
        assert_eq!(summary.at, 10);
        records.apply(osc("state=working:id=tests:progress=5").unwrap(), 11);
        // two working records: the newer speaks
        assert_eq!(records.summary().unwrap().progress, Some(5));
        assert_eq!(
            records.summary().unwrap().app.as_deref(),
            Some("claude-code"),
            "a child takes the root's app"
        );
        records.apply(
            osc(&format!(
                "state=blocked:kind=question:msg={}",
                b64("Which branch?")
            ))
            .unwrap(),
            12,
        );
        let summary = records.summary().unwrap();
        assert_eq!(summary.state, "blocked");
        assert_eq!(summary.kind.as_deref(), Some("question"));
        assert_eq!(summary.msg.as_deref(), Some("Which branch?"));
        assert_eq!(
            summary.app, None,
            "the replaced root record kept nothing it omitted"
        );
    }

    #[test]
    fn clear_takes_a_record_and_its_children() {
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=working:id=a").unwrap(), 1);
        records.apply(osc("state=working:id=a/b").unwrap(), 2);
        records.apply(osc("state=working:id=ab").unwrap(), 3);
        records.apply(osc("state=clear:id=a").unwrap(), 4);
        assert_eq!(records.len(), 1, "`ab` is not a child of `a`");
        records.apply(osc("state=clear").unwrap(), 5);
        assert!(records.is_empty());
    }

    #[test]
    fn a_new_prompt_keeps_only_results() {
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=working:id=w").unwrap(), 1);
        records.apply(osc("state=blocked:id=b").unwrap(), 2);
        records.apply(osc("state=idle:id=i").unwrap(), 3);
        records.apply(osc("state=done:id=d").unwrap(), 4);
        records.apply(osc("state=error:id=e").unwrap(), 5);
        records.drop_unfinished();
        assert_eq!(records.len(), 2);
        assert_eq!(records.summary().unwrap().state, "error");
    }

    #[test]
    fn a_new_program_after_the_prompt_does_not_inherit_the_last_ones_result() {
        // claude finishes, exits to the shell prompt, then the shell runs the next agent
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=working:app=claude-code").unwrap(), 1);
        records.apply(osc("state=done:app=claude-code").unwrap(), 2);
        records.drop_unfinished(); // claude exits
        records.prompt(); // the shell draws its prompt
        assert_eq!(
            records.summary().unwrap().state,
            "done",
            "the result stays while the prompt after it is in the pane"
        );
        records.command_started(); // the shell runs the next agent
        assert_eq!(
            records.summary(),
            None,
            "nothing until the new agent reports"
        );
        records.apply(osc("?").unwrap(), 3); // and it asks, which changes nothing
        assert_eq!(records.summary(), None);
        records.apply(osc("state=working:app=pi").unwrap(), 4);
        let summary = records.summary().unwrap();
        assert_eq!(summary.state, "working");
        assert_eq!(summary.app.as_deref(), Some("pi"));
    }

    #[test]
    fn a_command_start_keeps_what_was_reported_after_the_prompt() {
        // claude writes its own marks: `133;A` as a turn starts, `133;C` and `133;D` as it ends
        let mut records = ProgramStatusRecords::default();
        records.prompt();
        records.apply(osc("state=working:app=claude-code").unwrap(), 1);
        records.apply(osc("state=done:app=claude-code:id=sub").unwrap(), 2);
        records.apply(osc("state=done:app=claude-code").unwrap(), 3);
        records.command_started();
        assert_eq!(records.len(), 2, "the turn's own results stay");
        assert_eq!(records.summary().unwrap().state, "done");
        // the next turn: its prompt marks the results as before it, and they go at its end
        records.prompt();
        records.apply(osc("state=working:app=claude-code").unwrap(), 4);
        records.command_started();
        assert_eq!(records.len(), 1, "the earlier turn's child result went");
        assert_eq!(records.summary().unwrap().state, "working");
    }

    #[test]
    fn a_command_start_before_any_prompt_keeps_everything() {
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=done").unwrap(), 1);
        records.command_started();
        assert_eq!(records.summary().unwrap().state, "done");
    }

    #[test]
    fn a_program_that_asks_starts_from_nothing() {
        // a shell without marks: only the new program's query says it started
        let mut records = ProgramStatusRecords::default();
        records.apply(osc("state=done:app=claude-code").unwrap(), 1);
        records.apply(osc("state=error:id=build").unwrap(), 2);
        records.program_started();
        assert!(records.is_empty());
    }

    #[test]
    fn the_least_recently_updated_record_goes_past_the_cap() {
        let mut records = ProgramStatusRecords::default();
        for n in 0..=MAX_RECORDS {
            records.apply(osc(&format!("state=idle:id=r{n}")).unwrap(), n as u64);
        }
        assert_eq!(records.len(), MAX_RECORDS);
        assert!(!records.records.contains_key("r0"));
        assert!(records.records.contains_key(&format!("r{MAX_RECORDS}")));
    }

    #[test]
    fn input_makes_the_state_stale_until_the_program_reports_again() {
        let mut records = ProgramStatusRecords::default();
        assert!(!records.input_since_report());
        records.apply(Message::Report(report("state=done")), 1);
        records.note_input();
        // the `done` is still there, and now it is the turn before
        assert!(records.input_since_report());
        assert_eq!(records.summary().unwrap().state, "done");
        records.apply(Message::Report(report("state=working")), 2);
        assert!(!records.input_since_report());
        records.note_input();
        records.apply(Message::Clear { id: String::new() }, 3);
        assert!(!records.input_since_report());
    }
}
