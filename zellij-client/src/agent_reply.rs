//! fork addition: an agent's last reply, read from the transcript its harness writes.
//!
//! `action last-reply` exists so that a supervisor can ask an agent in another pane what it said,
//! without reading - and parsing - a screen the agent redraws as it likes. The screen is the wrong
//! source: claude, codex, opencode and pi each draw a turn differently, wrap it to the pane, and
//! scroll the start of a long answer away. The harness's own transcript holds the text as the
//! model wrote it, so this reads that.
//!
//! Which transcript is the pane's comes from the session id `list-panes` already carries (the
//! `agent_id` of nkmk.31: claude's per-process record, codex's open rollout file, or the agent's
//! own `OSC 1337` report). The file is found by that id under the harness's own directory:
//!
//! | harness | file |
//! |---|---|
//! | claude | `$CLAUDE_CONFIG_DIR` or `~/.claude`, `projects/*/<id>.jsonl` |
//! | codex | `$CODEX_HOME` or `~/.codex`, `sessions/YYYY/MM/DD/rollout-*-<id>.jsonl` |
//! | pi | `$PI_CODING_AGENT_DIR` or `~/.pi/agent`, `sessions/*/*_<id>.jsonl` |
//!
//! opencode keeps its sessions in a database, and is not read.
//!
//! **The last reply** is the text the agent wrote after the last thing that was not its own text:
//! the last prompt, or the last tool call or tool result within the turn. That is the answer a
//! person reads at the end of a turn, without the "let me look at the file" narration between
//! tool calls. Thinking is never part of it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How much of a transcript's end is read. A turn's last reply is at the end, and a transcript can
/// run to tens of megabytes.
pub const TAIL_BYTES: u64 = 8 * 1024 * 1024;

/// The harnesses whose transcript this build reads.
pub fn reads_transcript_of(kind: &str) -> bool {
    matches!(kind, "claude" | "codex" | "pi")
}

/// Where a harness keeps its transcripts, from an environment lookup and the home directory.
pub fn transcript_root(
    kind: &str,
    var: &dyn Fn(&str) -> Option<String>,
    home: Option<&Path>,
) -> Option<PathBuf> {
    let from_var = |name: &str| {
        var(name)
            .filter(|value| !value.trim().is_empty())
            .map(PathBuf::from)
    };
    match kind {
        "claude" => from_var("CLAUDE_CONFIG_DIR")
            .or_else(|| home.map(|home| home.join(".claude")))
            .map(|root| root.join("projects")),
        "codex" => from_var("CODEX_HOME")
            .or_else(|| home.map(|home| home.join(".codex")))
            .map(|root| root.join("sessions")),
        "pi" => from_var("PI_CODING_AGENT_DIR")
            .or_else(|| home.map(|home| home.join(".pi").join("agent")))
            .map(|root| root.join("sessions")),
        _ => None,
    }
}

/// Whether `name` is the transcript file of session `id` for this harness.
fn names_session(kind: &str, name: &str, id: &str) -> bool {
    match kind {
        "claude" => name == format!("{}.jsonl", id),
        "codex" => name.starts_with("rollout-") && name.ends_with(&format!("-{}.jsonl", id)),
        "pi" => name.ends_with(&format!("_{}.jsonl", id)),
        _ => false,
    }
}

/// The transcript of session `id`: the newest file under `root` that names it.
///
/// claude and pi keep one directory per project, one level down; codex keeps one per day, three
/// levels down. The walk goes no deeper than the harness needs, and an id that is not a plain
/// token is refused before it reaches a file name.
pub fn find_transcript(kind: &str, root: &Path, id: &str) -> Option<PathBuf> {
    let plain = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.contains("..");
    if !plain {
        return None;
    }
    let depth = if kind == "codex" { 3 } else { 1 };
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, level)) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                if level < depth {
                    dirs.push((path, level + 1));
                }
                continue;
            }
            if level != depth {
                continue;
            }
            let name = entry.file_name();
            if !names_session(kind, &name.to_string_lossy(), id) {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .unwrap_or(std::time::UNIX_EPOCH);
            if best.as_ref().map_or(true, |(newest, _)| modified > *newest) {
                best = Some((modified, path));
            }
        }
    }
    best.map(|(_, path)| path)
}

/// The end of a file, from the first whole line in its last `TAIL_BYTES`.
pub fn read_tail(path: &Path) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    if start == 0 {
        return Ok(text);
    }
    // the read began mid-line: that line is not JSON on its own
    Ok(match text.find('\n') {
        Some(newline) => text[newline + 1..].to_owned(),
        None => String::new(),
    })
}

/// One transcript line, as the last-reply walk sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Step {
    /// A prompt: what a person, or another program, sent the agent. A turn starts here.
    Prompt,
    /// Text the agent wrote for a person to read.
    Text(String),
    /// A tool call or a tool result: the agent is still working, and text before it was narration.
    Tool,
    /// Anything else: thinking, bookkeeping, token counts.
    Other,
}

/// The text blocks of a content array: claude's `text`, codex's `output_text`, pi's `text`.
fn text_blocks(content: &Value) -> Vec<String> {
    match content {
        Value::String(text) => vec![text.clone()],
        Value::Array(blocks) => blocks
            .iter()
            .filter(|block| {
                matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("text") | Some("output_text")
                )
            })
            .filter_map(|block| block.get("text").and_then(Value::as_str))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

/// Whether a content array holds anything but text: a tool call or a tool result.
fn has_tool_block(content: &Value) -> bool {
    content.as_array().map_or(false, |blocks| {
        blocks.iter().any(|block| {
            matches!(
                block.get("type").and_then(Value::as_str),
                Some("tool_use") | Some("tool_result") | Some("toolCall") | Some("toolResult")
            )
        })
    })
}

fn claude_steps(line: &Value) -> Vec<Step> {
    // a subagent's lines are another conversation, kept in the same file
    if line.get("isSidechain").and_then(Value::as_bool) == Some(true) {
        return vec![Step::Other];
    }
    let message = line.get("message").unwrap_or(&Value::Null);
    let content = message.get("content").unwrap_or(&Value::Null);
    match line.get("type").and_then(Value::as_str) {
        Some("user") => {
            if line.get("isMeta").and_then(Value::as_bool) == Some(true) {
                vec![Step::Other]
            } else if has_tool_block(content) {
                vec![Step::Tool]
            } else {
                vec![Step::Prompt]
            }
        },
        Some("assistant") => {
            let mut steps: Vec<Step> = text_blocks(content).into_iter().map(Step::Text).collect();
            if has_tool_block(content) {
                steps.push(Step::Tool);
            }
            steps
        },
        _ => vec![Step::Other],
    }
}

fn codex_steps(line: &Value) -> Vec<Step> {
    let payload = line.get("payload").unwrap_or(&Value::Null);
    let payload_type = payload.get("type").and_then(Value::as_str);
    match line.get("type").and_then(Value::as_str) {
        Some("response_item") => match payload_type {
            Some("message") => match payload.get("role").and_then(Value::as_str) {
                Some("assistant") => text_blocks(payload.get("content").unwrap_or(&Value::Null))
                    .into_iter()
                    .map(Step::Text)
                    .collect(),
                // codex's own context (`<environment_context>`, the instructions) arrives as user
                // messages too; it comes before the first real prompt, so it only ever starts a
                // turn that the real prompt starts again
                Some("user") => vec![Step::Prompt],
                _ => vec![Step::Other],
            },
            Some(kind) if kind.contains("call") => vec![Step::Tool],
            _ => vec![Step::Other],
        },
        _ => vec![Step::Other],
    }
}

fn pi_steps(line: &Value) -> Vec<Step> {
    if line.get("type").and_then(Value::as_str) != Some("message") {
        return vec![Step::Other];
    }
    let message = line.get("message").unwrap_or(&Value::Null);
    let content = message.get("content").unwrap_or(&Value::Null);
    match message.get("role").and_then(Value::as_str) {
        Some("user") => vec![Step::Prompt],
        Some("toolResult") => vec![Step::Tool],
        Some("assistant") => {
            let mut steps: Vec<Step> = text_blocks(content).into_iter().map(Step::Text).collect();
            if has_tool_block(content) {
                steps.push(Step::Tool);
            }
            steps
        },
        _ => vec![Step::Other],
    }
}

/// What the walk found: the reply, and whether the turn ended on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastReply {
    /// The agent's text after the last prompt or tool step, blocks joined by a blank line. Empty
    /// when the agent has written nothing since.
    pub reply: String,
    /// The transcript holds a prompt, so `reply` belongs to a turn rather than to whatever the
    /// tail happened to start with.
    pub saw_prompt: bool,
    /// The last step was a tool call or result: the agent is mid-turn, and `reply` is the text it
    /// wrote before that step's prompt, if any. Empty then.
    pub mid_tool: bool,
}

/// The last reply in a transcript's lines.
pub fn last_reply(kind: &str, transcript: &str) -> LastReply {
    let mut reply: Vec<String> = Vec::new();
    let mut saw_prompt = false;
    let mut mid_tool = false;
    for line in transcript.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let steps = match kind {
            "claude" => claude_steps(&value),
            "codex" => codex_steps(&value),
            "pi" => pi_steps(&value),
            _ => Vec::new(),
        };
        for step in steps {
            match step {
                Step::Prompt => {
                    saw_prompt = true;
                    mid_tool = false;
                    reply.clear();
                },
                Step::Tool => {
                    mid_tool = true;
                    reply.clear();
                },
                Step::Text(text) => {
                    mid_tool = false;
                    if !text.trim().is_empty() {
                        reply.push(text.trim().to_owned());
                    }
                },
                Step::Other => {},
            }
        }
    }
    LastReply {
        reply: reply.join("\n\n"),
        saw_prompt,
        mid_tool,
    }
}

/// What `list-agents` reads out of a claude transcript about the chat itself, beside the reply.
///
/// claude records, in its transcript, a few facts no screen shows: the Remote Control session it
/// is connected to (`{"type":"bridge-session","bridgeSessionId":"cse_<X>"}`, repeated through the
/// file, the last one current) and each answer's token counts (`message.usage`). Both are
/// undocumented, so every miss is "not known" rather than an error.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatMeta {
    /// The address a harness's messaging tool takes for this chat from any machine:
    /// `bridge:session_<X>`, from the last `bridge-session` record.
    pub message_to: Option<String>,
    /// When the chat began, in milliseconds since the epoch: the first record's `timestamp`, or
    /// the file's birth when the head of the file has none.
    pub started_at: Option<u64>,
    /// The prompt the last answer was given, in tokens: input plus cache read plus cache
    /// creation. The model's context window is not in the transcript, so no percentage is made.
    pub context_tokens: Option<u64>,
}

/// How far back from a transcript's end the meta scan reads, at most.
pub const META_TAIL_BYTES: u64 = 4 * 1024 * 1024;
/// How much of a transcript's start is read for its first `timestamp`.
const META_HEAD_BYTES: u64 = 64 * 1024;
/// One backward read of the meta scan.
const META_CHUNK_BYTES: u64 = 256 * 1024;

/// The messaging address for a `bridgeSessionId`: `cse_<X>` is `bridge:session_<X>`. Anything
/// else, or an `<X>` that is not a plain token, is no address.
pub fn bridge_address(bridge_session_id: &str) -> Option<String> {
    let rest = bridge_session_id.strip_prefix("cse_")?;
    let plain = !rest.is_empty()
        && rest
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'));
    plain.then(|| format!("bridge:session_{}", rest))
}

/// The tail facts, filled from the last line backwards: the first value found is the newest.
#[derive(Debug, Default)]
struct TailFacts {
    message_to: Option<String>,
    bridge_seen: bool,
    context_tokens: Option<u64>,
}

impl TailFacts {
    fn done(&self) -> bool {
        self.bridge_seen && self.context_tokens.is_some()
    }

    /// One transcript line, newest first. Lines are matched as text before they are parsed, so a
    /// tail of tool output costs a substring search per line, not a JSON parse.
    fn note(&mut self, line: &str) {
        if !self.bridge_seen && line.contains("\"bridge-session\"") {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                if value.get("type").and_then(Value::as_str) == Some("bridge-session") {
                    self.bridge_seen = true;
                    self.message_to = value
                        .get("bridgeSessionId")
                        .and_then(Value::as_str)
                        .and_then(bridge_address);
                }
            }
        }
        if self.context_tokens.is_none()
            && line.contains("\"usage\"")
            && line.contains("\"assistant\"")
        {
            if let Ok(value) = serde_json::from_str::<Value>(line) {
                self.context_tokens = prompt_tokens(&value);
            }
        }
    }
}

/// The prompt size of one claude assistant line, or `None` for any other line. A subagent's line
/// is another conversation's, and a line with no tokens at all (a synthetic error message) says
/// nothing about the context.
fn prompt_tokens(line: &Value) -> Option<u64> {
    if line.get("type").and_then(Value::as_str) != Some("assistant")
        || line.get("isSidechain").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let usage = line.get("message")?.get("usage")?;
    let count = |key: &str| usage.get(key).and_then(Value::as_u64).unwrap_or(0);
    let total = count("input_tokens")
        + count("cache_read_input_tokens")
        + count("cache_creation_input_tokens");
    (total > 0).then_some(total)
}

/// The facts in a transcript's text, as the backward scan would find them.
#[cfg(test)]
fn tail_facts_of(text: &str) -> TailFacts {
    let mut facts = TailFacts::default();
    for line in text.lines().rev() {
        facts.note(line);
        if facts.done() {
            break;
        }
    }
    facts
}

/// Walk a file's lines from its end backwards, at most `cap` bytes, `chunk` bytes per read, until
/// `facts` has everything. A line longer than `cap` ends the walk.
fn scan_backward(path: &Path, cap: u64, chunk: u64, facts: &mut TailFacts) -> std::io::Result<()> {
    let mut file = File::open(path)?;
    let len = file.metadata()?.len();
    let floor = len.saturating_sub(cap);
    let mut pos = len;
    // the start of the line the previous read began inside
    let mut carry: Vec<u8> = Vec::new();
    while pos > floor && !facts.done() {
        let start = pos.saturating_sub(chunk).max(floor);
        file.seek(SeekFrom::Start(start))?;
        let mut bytes = vec![0; (pos - start) as usize];
        file.read_exact(&mut bytes)?;
        bytes.extend_from_slice(&carry);
        let whole_from = if start == 0 {
            0
        } else {
            match bytes.iter().position(|byte| *byte == b'\n') {
                Some(newline) => newline + 1,
                None => {
                    carry = bytes;
                    pos = start;
                    continue;
                },
            }
        };
        let text = String::from_utf8_lossy(&bytes[whole_from..]);
        for line in text.lines().rev() {
            facts.note(line);
            if facts.done() {
                return Ok(());
            }
        }
        carry = bytes[..whole_from].to_vec();
        pos = start;
    }
    Ok(())
}

/// The first `timestamp` in the first `META_HEAD_BYTES` of a transcript, in milliseconds.
fn first_timestamp(path: &Path) -> Option<u64> {
    let file = File::open(path).ok()?;
    let mut head = Vec::new();
    file.take(META_HEAD_BYTES).read_to_end(&mut head).ok()?;
    String::from_utf8_lossy(&head)
        .lines()
        .filter(|line| line.contains("\"timestamp\""))
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| {
            let stamp = value.get("timestamp")?.as_str()?;
            let at = humantime::parse_rfc3339(stamp).ok()?;
            at.duration_since(std::time::UNIX_EPOCH)
                .ok()
                .map(|since| since.as_millis() as u64)
        })
}

/// [`ChatMeta`] of a claude transcript, reading its head and at most `cap` bytes of its tail.
pub fn chat_meta_with_cap(path: &Path, cap: u64) -> ChatMeta {
    let mut facts = TailFacts::default();
    let _ = scan_backward(path, cap, META_CHUNK_BYTES.min(cap.max(1)), &mut facts);
    let started_at = first_timestamp(path).or_else(|| {
        std::fs::metadata(path)
            .and_then(|meta| meta.created())
            .ok()
            .and_then(|born| born.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|since| since.as_millis() as u64)
    });
    ChatMeta {
        message_to: facts.message_to,
        started_at,
        context_tokens: facts.context_tokens,
    }
}

/// [`ChatMeta`] of a claude transcript, bounded by [`META_TAIL_BYTES`].
pub fn chat_meta(path: &Path) -> ChatMeta {
    chat_meta_with_cap(path, META_TAIL_BYTES)
}

/// claude's project directory name for a working directory: every character that is not a letter
/// or a digit becomes `-`. Tried before the walk of every project directory.
pub fn claude_project_dir(cwd: &Path) -> String {
    cwd.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The claude transcript of session `id`, looked for first in the directory its `cwd` maps to,
/// then by [`find_transcript`]'s walk.
pub fn find_claude_transcript(root: &Path, id: &str, cwd: Option<&Path>) -> Option<PathBuf> {
    let plain = !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !id.contains("..");
    if !plain {
        return None;
    }
    if let Some(cwd) = cwd {
        let direct = root
            .join(claude_project_dir(cwd))
            .join(format!("{}.jsonl", id));
        if direct.is_file() {
            return Some(direct);
        }
    }
    find_transcript("claude", root, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE: &str = r#"{"type":"user","message":{"role":"user","content":"first question"}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"old answer"}]}}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"Run the tests"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me run them."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>x</system-reminder>"}}
{"type":"assistant","isSidechain":true,"message":{"role":"assistant","content":[{"type":"text","text":"a subagent"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"All 12 tests pass."}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Nothing failed."}]}}
{"type":"system","subtype":"turn_duration"}
not json at all
"#;

    #[test]
    fn a_claude_reply_is_the_text_after_the_last_tool_step_of_the_last_turn() {
        let found = last_reply("claude", CLAUDE);
        assert_eq!(found.reply, "All 12 tests pass.\n\nNothing failed.");
        assert!(found.saw_prompt);
        assert!(!found.mid_tool);
    }

    #[test]
    fn a_turn_still_in_a_tool_has_no_reply_yet() {
        let mid = CLAUDE.lines().take(6).collect::<Vec<_>>().join("\n");
        let found = last_reply("claude", &mid);
        assert_eq!(found.reply, "");
        assert!(found.mid_tool);
        // a new prompt with nothing after it has no reply either, and is not mid-tool
        let asked = format!(
            "{}\n{}",
            CLAUDE, r#"{"type":"user","message":{"role":"user","content":"and now?"}}"#
        );
        let found = last_reply("claude", &asked);
        assert_eq!((found.reply.as_str(), found.mid_tool), ("", false));
    }

    #[test]
    fn a_codex_reply_is_its_last_assistant_message() {
        let rollout = r#"{"type":"session_meta","payload":{"id":"x"}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}
{"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"fix it"}]}}
{"type":"response_item","payload":{"type":"reasoning","summary":[]}}
{"type":"response_item","payload":{"type":"function_call","name":"shell","call_id":"c1"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"done"}}
{"type":"event_msg","payload":{"type":"agent_message","message":"Fixed."}}
{"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Fixed."}]}}
{"type":"event_msg","payload":{"type":"task_complete","last_agent_message":"Fixed."}}"#;
        assert_eq!(last_reply("codex", rollout).reply, "Fixed.");
    }

    #[test]
    fn a_pi_reply_is_the_text_after_its_last_tool_result() {
        let session = r#"{"type":"session","id":"x"}
{"type":"message","message":{"role":"user","content":[{"type":"text","text":"look"}]}}
{"type":"message","message":{"role":"assistant","content":[{"type":"thinking","thinking":"."},{"type":"text","text":"Checking."},{"type":"toolCall","id":"1"}]}}
{"type":"message","message":{"role":"toolResult","content":[{"type":"text","text":"file"}]}}
{"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"It is there."}]}}"#;
        assert_eq!(last_reply("pi", session).reply, "It is there.");
    }

    #[test]
    fn each_harness_keeps_its_transcript_where_its_own_variable_says() {
        let none = |_: &str| None;
        let home = Path::new("/home/u");
        assert_eq!(
            transcript_root("claude", &none, Some(home)),
            Some(PathBuf::from("/home/u/.claude/projects"))
        );
        assert_eq!(
            transcript_root("codex", &none, Some(home)),
            Some(PathBuf::from("/home/u/.codex/sessions"))
        );
        assert_eq!(
            transcript_root("pi", &none, Some(home)),
            Some(PathBuf::from("/home/u/.pi/agent/sessions"))
        );
        let set = |name: &str| (name == "CLAUDE_CONFIG_DIR").then(|| "/cfg".to_owned());
        assert_eq!(
            transcript_root("claude", &set, Some(home)),
            Some(PathBuf::from("/cfg/projects"))
        );
        assert_eq!(transcript_root("opencode", &none, Some(home)), None);
        assert!(!reads_transcript_of("opencode"));
    }

    #[test]
    fn a_transcript_is_found_by_its_session_id_at_the_harnesss_own_depth() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let id = "0f3c1a2b-aaaa-bbbb-cccc-1234567890ab";
        std::fs::create_dir_all(root.join("claude/-home-u-src")).unwrap();
        std::fs::write(root.join(format!("claude/-home-u-src/{}.jsonl", id)), "").unwrap();
        std::fs::write(root.join("claude/-home-u-src/other.jsonl"), "").unwrap();
        assert_eq!(
            find_transcript("claude", &root.join("claude"), id),
            Some(root.join(format!("claude/-home-u-src/{}.jsonl", id)))
        );
        std::fs::create_dir_all(root.join("codex/2026/10/12")).unwrap();
        let rollout = root.join(format!(
            "codex/2026/10/12/rollout-2026-10-12T10-00-00-{}.jsonl",
            id
        ));
        std::fs::write(&rollout, "").unwrap();
        assert_eq!(
            find_transcript("codex", &root.join("codex"), id),
            Some(rollout)
        );
        std::fs::create_dir_all(root.join("pi/--home-u-src--")).unwrap();
        let pi = root.join(format!(
            "pi/--home-u-src--/2026-10-12T10-00-00-000Z_{}.jsonl",
            id
        ));
        std::fs::write(&pi, "").unwrap();
        assert_eq!(find_transcript("pi", &root.join("pi"), id), Some(pi));
        // an id that is not a plain token never reaches a file name
        assert_eq!(
            find_transcript("claude", &root.join("claude"), "../x"),
            None
        );
        assert_eq!(
            find_transcript("claude", &root.join("claude"), "missing"),
            None
        );
    }

    #[test]
    fn a_long_transcript_is_read_from_its_last_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let filler = format!("{}\n", "x".repeat(1023));
        let mut text = filler.repeat((TAIL_BYTES / 1024) as usize + 4);
        text.push_str("{\"last\":true}\n");
        std::fs::write(&path, &text).unwrap();
        let tail = read_tail(&path).unwrap();
        assert!(tail.len() as u64 <= TAIL_BYTES);
        assert!(tail.starts_with('x'));
        assert!(tail.ends_with("{\"last\":true}\n"));
    }

    const BRIDGE_OLD: &str =
        r#"{"type":"bridge-session","sessionId":"s","bridgeSessionId":"cse_OLD1"}"#;
    const BRIDGE_NEW: &str =
        r#"{"type":"bridge-session","sessionId":"s","bridgeSessionId":"cse_01NewAbc"}"#;
    const ANSWER: &str = r#"{"type":"assistant","timestamp":"2026-11-19T10:00:05.000Z","message":{"role":"assistant","usage":{"input_tokens":2,"cache_creation_input_tokens":100,"cache_read_input_tokens":1000,"output_tokens":50},"content":[{"type":"text","text":"ok"}]}}"#;

    #[test]
    fn a_bridge_session_id_becomes_the_address_the_messaging_tool_takes() {
        assert_eq!(
            bridge_address("cse_01QoVc8m9ajcz").as_deref(),
            Some("bridge:session_01QoVc8m9ajcz")
        );
        for bad in ["", "cse_", "session_01Q", "cse_a b", "cse_x;rm", "01Q"] {
            assert_eq!(bridge_address(bad), None, "{:?}", bad);
        }
    }

    #[test]
    fn the_last_bridge_record_is_the_current_one_and_a_missing_one_is_no_address() {
        let text = format!(
            "{}\n{}\n{}\n{}\n",
            BRIDGE_OLD, ANSWER, BRIDGE_NEW, r#"{"type":"user","message":{"content":"hi"}}"#
        );
        let facts = tail_facts_of(&text);
        assert_eq!(facts.message_to.as_deref(), Some("bridge:session_01NewAbc"));
        assert_eq!(facts.context_tokens, Some(1102));
        let facts = tail_facts_of(&format!("{}\n", ANSWER));
        assert_eq!(facts.message_to, None);
        assert_eq!(facts.context_tokens, Some(1102));
    }

    #[test]
    fn a_malformed_bridge_record_or_usage_is_not_known_rather_than_an_error() {
        let text = [
            r#"{"type":"bridge-session","bridgeSessionId":"cse_GOOD"}"#,
            // newer, and malformed: it is the current record, and it has no address
            r#"{"type":"bridge-session","bridgeSessionId":42}"#,
            r#"{"type":"bridge-session" truncated"#,
            r#"{"type":"assistant","isSidechain":true,"message":{"usage":{"input_tokens":9}}}"#,
            r#"{"type":"assistant","message":{"model":"<synthetic>","usage":{"input_tokens":0}}}"#,
        ]
        .join("\n");
        let facts = tail_facts_of(&text);
        assert_eq!(facts.message_to, None);
        assert!(facts.bridge_seen);
        assert_eq!(facts.context_tokens, None);
    }

    #[test]
    fn chat_meta_reads_the_head_for_its_start_and_the_tail_backwards() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let filler = format!(
            "{{\"type\":\"progress\",\"data\":\"{}\"}}\n",
            "x".repeat(1000)
        );
        let mut text = String::from("{\"type\":\"mode\"}\n");
        text.push_str(
            "{\"type\":\"user\",\"timestamp\":\"2026-11-19T10:00:00.250Z\",\"message\":{\"content\":\"hi\"}}\n",
        );
        text.push_str(&format!("{}\n", BRIDGE_OLD));
        text.push_str(&format!("{}\n", ANSWER));
        text.push_str(&format!("{}\n", BRIDGE_NEW));
        // far more than one chunk after the records, and a line still being written at the end
        text.push_str(&filler.repeat(600));
        text.push_str("{\"type\":\"assist");
        std::fs::write(&path, &text).unwrap();
        let meta = chat_meta(&path);
        assert_eq!(meta.message_to.as_deref(), Some("bridge:session_01NewAbc"));
        assert_eq!(meta.context_tokens, Some(1102));
        assert_eq!(meta.started_at, Some(1_795_082_400_250));
    }

    #[test]
    fn a_huge_transcript_is_read_only_as_far_back_as_the_cap() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.jsonl");
        let filler = format!("{}\n", "y".repeat(1023));
        let text = format!("{}\n{}\n{}", BRIDGE_NEW, ANSWER, filler.repeat(64));
        std::fs::write(&path, &text).unwrap();
        // the records are 64 KiB back: a 16 KiB cap does not reach them
        let meta = chat_meta_with_cap(&path, 16 * 1024);
        assert_eq!((meta.message_to, meta.context_tokens), (None, None));
        let meta = chat_meta_with_cap(&path, 1024 * 1024);
        assert_eq!(meta.message_to.as_deref(), Some("bridge:session_01NewAbc"));
        // a single line longer than the cap ends the walk without reading the file whole
        let long = format!("{}\n{}\n", BRIDGE_NEW, "z".repeat(200 * 1024));
        std::fs::write(&path, &long).unwrap();
        assert_eq!(chat_meta_with_cap(&path, 64 * 1024).message_to, None);
        // a missing file is nothing known
        assert_eq!(
            chat_meta(&dir.path().join("gone.jsonl")),
            ChatMeta::default()
        );
    }

    #[test]
    fn a_claude_transcript_is_found_from_the_cwd_first() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(
            claude_project_dir(Path::new("/home/u/.cache/my_app")),
            "-home-u--cache-my-app"
        );
        let id = "0f3c1a2b-aaaa";
        std::fs::create_dir_all(root.join("-srv-app")).unwrap();
        std::fs::write(root.join(format!("-srv-app/{}.jsonl", id)), "").unwrap();
        assert_eq!(
            find_claude_transcript(root, id, Some(Path::new("/srv/app"))),
            Some(root.join(format!("-srv-app/{}.jsonl", id)))
        );
        // a cwd that maps elsewhere falls back to the walk
        assert_eq!(
            find_claude_transcript(root, id, Some(Path::new("/elsewhere"))),
            Some(root.join(format!("-srv-app/{}.jsonl", id)))
        );
        assert_eq!(find_claude_transcript(root, "../x", None), None);
    }
}
