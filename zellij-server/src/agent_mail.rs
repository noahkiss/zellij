//! fork addition: messages for the coding agents in this session's panes, held until they can be
//! typed in without mixing into what a person is typing.
//!
//! Two parts live here. [`Draft`] follows what a person types into an agent's prompt, so the
//! guard can tell a prompt with unsent text in it from an empty one. [`Mailbox`] keeps the held
//! messages, and the ones delivered lately so a reply can find its way back, one small JSON file
//! each under the session's state directory, so they outlive a restart of the server.
//!
//! The rule itself - deliver, hold or refuse - is `zellij_utils::agent_mail::guard`. The screen
//! applies it and writes to the pane in one step (`Screen::try_deliver_agent_message`).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use zellij_utils::agent_mail::{MessageRecord, Reason, Status};

/// How long a submitted prompt counts as unsent while the agent has not said it took it up.
///
/// An Enter on a prompt with text in it is normally followed by the agent reporting `working`
/// within a second. Some lines are not a turn - a slash command that opens nothing - and the
/// agent then reports nothing; after this long, the prompt is taken to be empty again.
pub const SUBMIT_GRACE: Duration = Duration::from_secs(10);

/// The delay between the paste and its Enter. A TUI that reads a paste as it arrives can take an
/// Enter right behind it as part of the paste; `zellij_write_input submit` waits the same.
pub const ENTER_DELAY: Duration = Duration::from_millis(150);

/// How many delivered or expired messages the session remembers, so `reply_to` can find their
/// sender, and for how long.
const RECENT_CAP: usize = 100;
const RECENT_MAX_AGE_MS: u64 = 7 * 24 * 60 * 60 * 1000;

/// What a person has typed into an agent's prompt since the agent last took a prompt up.
///
/// Read from the bytes that reach the pane, the same ones that set `input_since_state`. It is an
/// estimate that errs on one side only: any key whose effect on the prompt it cannot know makes
/// the draft [`Draft::Unknown`], which holds a message until the agent next reports `working`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Draft {
    /// Nothing typed, or everything typed was deleted or cleared.
    #[default]
    Clean,
    /// This many characters typed and not deleted.
    Typed(u32),
    /// A key with an effect on the prompt this cannot follow: an arrow through history, a tab
    /// completion, a key that may open a menu.
    Unknown,
    /// Enter pressed on a prompt with text in it, and the agent has not reported `working` yet.
    Submitted(Instant),
    /// The session typed a message in and submitted it, and the agent has not reported `working`
    /// yet. Pending like [`Draft::Submitted`], but the guard calls it the agent's turn, not a
    /// person's draft.
    Sent(Instant),
}

impl Draft {
    /// Whether a message pasted now could mix with a person's text.
    pub fn pending(&self, now: Instant) -> bool {
        match self {
            Draft::Clean => false,
            Draft::Typed(_) | Draft::Unknown => true,
            Draft::Submitted(at) | Draft::Sent(at) => {
                now.saturating_duration_since(*at) < SUBMIT_GRACE
            },
        }
    }

    /// Whether what is pending is a message the session sent, not a person's text.
    pub fn sent_by_session(&self, now: Instant) -> bool {
        matches!(self, Draft::Sent(_)) && self.pending(now)
    }

    /// Follows the bytes of one write to the pane. `working` is whether the agent is mid-turn,
    /// where Esc interrupts it rather than touching the prompt.
    pub fn note(&mut self, bytes: &[u8], working: bool, now: Instant) {
        let mut at = 0;
        while at < bytes.len() {
            let rest = &bytes[at..];
            if let Some(len) = paste_len(rest) {
                let (consumed, chars) = len;
                if chars > 0 {
                    self.add(chars);
                }
                at += consumed;
                continue;
            }
            match rest[0] {
                0x1b => {
                    let (consumed, effect) = escape(rest, working);
                    self.apply(effect, now);
                    at += consumed;
                },
                b'\r' | b'\n' => {
                    self.apply(Effect::Enter, now);
                    at += 1;
                },
                0x7f | 0x08 => {
                    self.apply(Effect::Delete, now);
                    at += 1;
                },
                // Ctrl c and Ctrl u clear the line in every harness this build knows
                0x03 | 0x15 => {
                    self.apply(Effect::Clear, now);
                    at += 1;
                },
                byte if byte < 0x20 => {
                    self.apply(Effect::Unknown, now);
                    at += 1;
                },
                _ => {
                    let width = utf8_width(rest[0]).min(rest.len());
                    self.add(1);
                    at += width;
                },
            }
        }
    }

    /// The agent took a prompt up: whatever was typed is gone from its prompt.
    pub fn taken_up(&mut self) {
        *self = Draft::Clean;
    }

    /// A message was typed in and submitted by the session.
    pub fn delivered(&mut self, now: Instant) {
        *self = Draft::Sent(now);
    }

    fn add(&mut self, chars: u32) {
        *self = match *self {
            Draft::Typed(n) => Draft::Typed(n.saturating_add(chars)),
            Draft::Unknown => Draft::Unknown,
            Draft::Clean | Draft::Submitted(_) | Draft::Sent(_) => Draft::Typed(chars),
        };
    }

    fn apply(&mut self, effect: Effect, now: Instant) {
        *self = match (effect, *self) {
            (Effect::None, draft) => draft,
            (Effect::Clear, _) => Draft::Clean,
            (Effect::Delete, Draft::Typed(n)) if n > 1 => Draft::Typed(n - 1),
            (Effect::Delete, Draft::Typed(_)) => Draft::Clean,
            (Effect::Delete, draft) => draft,
            (Effect::Char, draft) => {
                let mut draft = draft;
                draft.add(1);
                draft
            },
            (Effect::Enter, Draft::Clean) => Draft::Clean,
            (Effect::Enter, Draft::Submitted(at)) => Draft::Submitted(at),
            (Effect::Enter, Draft::Sent(at)) => Draft::Sent(at),
            (Effect::Enter, Draft::Typed(_) | Draft::Unknown) => Draft::Submitted(now),
            // a key with no visible effect on an empty prompt still can open a menu: `Esc Esc`
            // on an idle claude opens the rewind picker, `Up` recalls a line
            (Effect::Unknown, _) => Draft::Unknown,
        };
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    None,
    Char,
    Delete,
    Clear,
    Enter,
    Unknown,
}

/// A bracketed paste at the start of `bytes`: how many bytes it spans and how many characters it
/// carries. An unterminated paste runs to the end of the write.
fn paste_len(bytes: &[u8]) -> Option<(usize, u32)> {
    const START: &[u8] = b"\x1b[200~";
    const END: &[u8] = b"\x1b[201~";
    let body = bytes.strip_prefix(START)?;
    let (content, consumed) = match body.windows(END.len()).position(|w| w == END) {
        Some(end) => (&body[..end], START.len() + end + END.len()),
        None => (body, bytes.len()),
    };
    let chars = String::from_utf8_lossy(content).chars().count() as u32;
    Some((consumed, chars))
}

/// An escape sequence at the start of `bytes`: how many bytes it spans and what it does to a
/// prompt.
fn escape(bytes: &[u8], working: bool) -> (usize, Effect) {
    match bytes.get(1) {
        // a lone Esc: an interrupt while the agent works, otherwise a key that may open a menu
        None => (
            1,
            if working {
                Effect::None
            } else {
                Effect::Unknown
            },
        ),
        Some(b'[') => {
            let end = bytes[2..]
                .iter()
                .position(|b| (0x40..=0x7e).contains(b))
                .map(|at| at + 2);
            let Some(end) = end else {
                return (bytes.len(), Effect::Unknown);
            };
            let params = std::str::from_utf8(&bytes[2..end]).unwrap_or("");
            (end + 1, csi_effect(params, bytes[end], working))
        },
        Some(b'O') => match bytes.get(2) {
            Some(b'C' | b'D' | b'H' | b'F') => (3, Effect::None),
            Some(_) => (3, Effect::Unknown),
            None => (2, Effect::Unknown),
        },
        // Alt and a key
        Some(_) => (2, Effect::Unknown),
    }
}

fn csi_effect(params: &str, final_byte: u8, working: bool) -> Effect {
    match final_byte {
        // left, right, home, end: the cursor moves, the text stays
        b'C' | b'D' | b'H' | b'F' if !params.contains(';') || params == "1;1" => Effect::None,
        // focus in and out, and a mouse report: not a key
        b'I' | b'O' if params.is_empty() => Effect::None,
        b'M' | b'm' if params.starts_with('<') => Effect::None,
        b'~' if params == "3" => Effect::Delete,
        b'u' => kitty_effect(params, working),
        _ => Effect::Unknown,
    }
}

/// A key in the kitty keyboard protocol: `CSI code ; modifiers u`.
fn kitty_effect(params: &str, working: bool) -> Effect {
    let mut fields = params.split(';');
    let code: u32 = match fields
        .next()
        .and_then(|code| code.split(':').next())
        .and_then(|code| code.parse().ok())
    {
        Some(code) => code,
        None => return Effect::Unknown,
    };
    let modifiers: u32 = fields
        .next()
        .and_then(|m| m.split(':').next())
        .and_then(|m| m.parse().ok())
        .unwrap_or(1);
    let bits = modifiers.saturating_sub(1);
    let ctrl = bits & 0b100 != 0;
    let others = bits & !0b1 != 0; // anything beyond shift
    match code {
        13 if bits == 0 => Effect::Enter,
        127 | 8 if bits == 0 => Effect::Delete,
        99 | 117 if ctrl && bits & !0b100 == 0 => Effect::Clear,
        27 if bits == 0 && working => Effect::None,
        code if code >= 32 && !others && char::from_u32(code).is_some() => Effect::Char,
        _ => Effect::Unknown,
    }
}

fn utf8_width(first: u8) -> usize {
    match first {
        0xf0..=0xff => 4,
        0xe0..=0xef => 3,
        0xc0..=0xdf => 2,
        _ => 1,
    }
}

/// A message as the mailbox keeps it: the record, and the text that goes in under its envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stored {
    #[serde(flatten)]
    pub record: MessageRecord,
    pub text: String,
}

/// The session's messages: the held ones, oldest first, and the ones settled lately.
#[derive(Debug, Default)]
pub struct Mailbox {
    /// Where each message is kept as `<id>.json`. `None` keeps them in memory only.
    dir: Option<PathBuf>,
    held: Vec<Stored>,
    recent: VecDeque<Stored>,
    /// The id the next message gets. Kept in `dir` as [`NEXT_ID_FILE`], so an id is never given
    /// twice by this session, across a restart of its server too.
    next_id: u64,
}

/// The file in a mailbox's directory that holds the next message id.
const NEXT_ID_FILE: &str = "next-id";

impl Mailbox {
    /// The mailbox kept in `dir`, with whatever an earlier server left there. A held message that
    /// expired while no server ran is settled as expired at once.
    pub fn open(dir: PathBuf, now_ms: u64) -> Mailbox {
        let mut mailbox = Mailbox {
            dir: Some(dir),
            ..Default::default()
        };
        let mut found: Vec<Stored> = mailbox
            .dir
            .as_deref()
            .and_then(|dir| std::fs::read_dir(dir).ok())
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .filter_map(|path| {
                let read = std::fs::read_to_string(&path).ok()?;
                match serde_json::from_str::<Stored>(&read) {
                    Ok(stored) => Some(stored),
                    Err(e) => {
                        log::warn!("skipping unreadable message {}: {}", path.display(), e);
                        None
                    },
                }
            })
            .collect();
        found.sort_by_key(|stored| stored.record.queued_at);
        // the saved counter, or past every numbered id found, whichever is further: a counter file
        // lost or written by nobody yet cannot hand out an id still in use
        let saved = mailbox
            .dir
            .as_deref()
            .and_then(|dir| std::fs::read_to_string(dir.join(NEXT_ID_FILE)).ok())
            .and_then(|text| text.trim().parse::<u64>().ok())
            .unwrap_or(0);
        let past_found = found
            .iter()
            .filter_map(|stored| stored.record.id.parse::<u64>().ok())
            .max()
            .map_or(0, |n| n.saturating_add(1));
        mailbox.next_id = saved.max(past_found).max(1);
        for stored in found {
            if stored.record.status == Status::Queued {
                mailbox.held.push(stored);
            } else {
                mailbox.recent.push_back(stored);
            }
        }
        mailbox.expire(now_ms);
        mailbox.prune(now_ms);
        mailbox
    }

    pub fn held(&self) -> &[Stored] {
        &self.held
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    /// How many messages are held for the pane with this handle.
    pub fn held_for(&self, handle: &str) -> usize {
        self.held
            .iter()
            .filter(|stored| stored.record.handle == handle)
            .count()
    }

    /// The next id: a small number counted per receiving session, `1`, `2`, ... The receiver
    /// passes it back as `reply_to`, and the session that delivered the message resolves it, so it
    /// only has to be unique here. The counter is saved before the id is used, so a restart never
    /// gives one twice. An id an older build gave (`m3k9x2q`) still resolves.
    pub fn new_id(&mut self) -> String {
        loop {
            let id = self.next_id.max(1);
            self.next_id = id.saturating_add(1);
            self.save_next_id();
            let id = id.to_string();
            if self.get(&id).is_none() {
                return id;
            }
        }
    }

    fn save_next_id(&self) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let path = dir.join(NEXT_ID_FILE);
            let partial = path.with_extension("part");
            std::fs::write(&partial, self.next_id.to_string())?;
            std::fs::rename(&partial, &path)
        };
        if let Err(e) = write() {
            log::error!("could not save the next message id: {}", e);
        }
    }

    pub fn get(&self, id: &str) -> Option<&Stored> {
        self.held
            .iter()
            .chain(self.recent.iter())
            .find(|stored| stored.record.id == id)
    }

    /// Every record: the held ones first, oldest first, then the settled ones, newest first.
    pub fn records(&self) -> Vec<MessageRecord> {
        self.held
            .iter()
            .map(|stored| stored.record.clone())
            .chain(self.recent.iter().rev().map(|stored| stored.record.clone()))
            .collect()
    }

    pub fn hold(&mut self, stored: Stored) {
        self.save(&stored);
        self.held.push(stored);
    }

    /// Keeps a delivered message, so a reply can find its sender.
    pub fn remember(&mut self, stored: Stored) {
        self.save(&stored);
        self.recent.push_back(stored);
        while self.recent.len() > RECENT_CAP {
            if let Some(old) = self.recent.pop_front() {
                self.forget(&old.record.id);
            }
        }
    }

    /// Changes a held message's reason, saving it only when it changed.
    pub fn set_reason(&mut self, id: &str, reason: Reason, pane_id: Option<String>) {
        let Some(at) = self.held.iter().position(|s| s.record.id == id) else {
            return;
        };
        let stored = &mut self.held[at];
        let changed = stored.record.reason != Some(reason)
            || pane_id
                .as_ref()
                .is_some_and(|pane_id| *pane_id != stored.record.pane_id);
        stored.record.reason = Some(reason);
        if let Some(pane_id) = pane_id {
            stored.record.pane_id = pane_id;
        }
        if changed {
            let stored = stored.clone();
            self.save(&stored);
        }
    }

    /// Takes a held message out, settled as delivered or expired.
    pub fn settle(&mut self, id: &str, status: Status, now_ms: u64) -> Option<MessageRecord> {
        self.settle_in(id, status, now_ms, None)
    }

    /// [`Mailbox::settle`], for a message delivered to the pane that now has this id.
    pub fn delivered(&mut self, id: &str, pane_id: String, now_ms: u64) -> Option<MessageRecord> {
        self.settle_in(id, Status::Delivered, now_ms, Some(pane_id))
    }

    fn settle_in(
        &mut self,
        id: &str,
        status: Status,
        now_ms: u64,
        pane_id: Option<String>,
    ) -> Option<MessageRecord> {
        let at = self.held.iter().position(|s| s.record.id == id)?;
        let mut stored = self.held.remove(at);
        if let Some(pane_id) = pane_id {
            stored.record.pane_id = pane_id;
        }
        stored.record.status = status;
        if status == Status::Delivered {
            stored.record.delivered_at = Some(now_ms);
            stored.record.reason = None;
        }
        let record = stored.record.clone();
        self.remember(stored);
        Some(record)
    }

    /// Settles every held message whose time ran out. Returns their ids.
    pub fn expire(&mut self, now_ms: u64) -> Vec<String> {
        let overdue: Vec<String> = self
            .held
            .iter()
            .filter(|stored| stored.record.expires_at <= now_ms)
            .map(|stored| stored.record.id.clone())
            .collect();
        for id in &overdue {
            self.settle(id, Status::Expired, now_ms);
        }
        overdue
    }

    fn prune(&mut self, now_ms: u64) {
        let old: Vec<String> = self
            .recent
            .iter()
            .filter(|stored| {
                now_ms.saturating_sub(
                    stored
                        .record
                        .delivered_at
                        .unwrap_or(stored.record.queued_at),
                ) > RECENT_MAX_AGE_MS
            })
            .map(|stored| stored.record.id.clone())
            .collect();
        self.recent
            .retain(|stored| !old.contains(&stored.record.id));
        for id in old {
            self.forget(&id);
        }
    }

    fn path(dir: &Path, id: &str) -> PathBuf {
        dir.join(format!("{}.json", id))
    }

    fn save(&self, stored: &Stored) {
        let Some(dir) = self.dir.as_deref() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            std::fs::create_dir_all(dir)?;
            let path = Self::path(dir, &stored.record.id);
            let partial = path.with_extension("json.part");
            std::fs::write(&partial, serde_json::to_vec(stored).unwrap_or_default())?;
            std::fs::rename(&partial, &path)
        };
        if let Err(e) = write() {
            log::error!("could not save message {}: {}", stored.record.id, e);
        }
    }

    fn forget(&self, id: &str) {
        if let Some(dir) = self.dir.as_deref() {
            let _ = std::fs::remove_file(Self::path(dir, id));
        }
    }
}

#[cfg(test)]
#[path = "./unit/agent_mail_tests.rs"]
mod agent_mail_tests;
