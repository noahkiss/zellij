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
}
