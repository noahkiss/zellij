//! Session-wide notices the server draws over the top-right of the viewport.
//!
//! Two facts are true of a whole session, actionable, and invisible everywhere else:
//!
//! - **Full Disk Access is missing** on a macOS host whose config says it is expected. TCC keys the
//!   grant to an absolute path, so a package upgrade silently invalidates it, and the failures show
//!   up later in an unrelated tool.
//! - **The server is running a superseded build.** A server keeps the binary it started with for
//!   the life of the session, so an upgrade reaches nothing until the session is restarted.
//!
//! Both are drawn by the SERVER rather than by a plugin, on purpose. Almost all zellij chrome is
//! plugins - tab bar, status bar - and this user has replaced theirs, so a notice living in a
//! bundled plugin would never appear. Compositing after the panes have rendered also means an
//! alt-screen application repainting underneath cannot clobber the notice, and nothing is written
//! into any pane's grid, so `dump-screen` and every transcript consumer see exactly what they saw
//! before.

use crate::output::CharacterChunk;
use crate::panes::terminal_character::{
    AnsiCode, CharacterStyles, RcCharacterStyles, TerminalCharacter,
};
use std::path::PathBuf;
use std::sync::OnceLock;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
use zellij_utils::data::{PaletteColor, Style};
use zellij_utils::pane_size::Size;
use zellij_utils::session_lifecycle::{full_disk_access_notice, stale_build_notice};

/// How far from the right edge the notices sit, so they do not touch a pane frame's corner.
const RIGHT_MARGIN: usize = 1;

/// Below this many columns nothing is drawn at all.
///
/// A notice narrower than this says nothing useful once truncated, and one that wrapped across the
/// top of the panes would be worse than no notice.
const MINIMUM_COLUMNS: usize = 24;

/// What the config says about each notice, recorded once at server startup.
///
/// Recorded rather than threaded: the questions are asked from `Screen`, whose constructor already
/// takes thirty arguments, and the answers are one small fact about the whole session.
#[derive(Debug, Default, Clone)]
pub struct NoticeSettings {
    /// Whether this machine's user says zellij is meant to hold Full Disk Access
    pub expect_full_disk_access: bool,
    /// Whether to say so when this server's binary has been superseded
    pub stale_build_notice: bool,
    /// The pinned copy `pin_exe` asks for, when it asks for one - a server executing it cannot be
    /// overwritten in place, so an upgrade never shows up in its own path and the notice has to
    /// look at what is installed instead
    pub pinned_exe: Option<PathBuf>,
}

static SETTINGS: OnceLock<NoticeSettings> = OnceLock::new();

/// Tell the notices what the config asked for. Only the first call counts.
pub fn record_settings(settings: NoticeSettings) {
    let _ = SETTINGS.set(settings);
}

fn settings() -> NoticeSettings {
    SETTINGS.get().cloned().unwrap_or(NoticeSettings {
        expect_full_disk_access: false,
        // on unless the config turns it off, which is also what a test with no settings recorded
        // should see
        stale_build_notice: true,
        pinned_exe: None,
    })
}

/// Ask both questions now, and say what is worth reporting.
pub fn current_notices(session_name: &str) -> StatusNotices {
    let settings = settings();
    let mut lines = vec![];
    if settings.expect_full_disk_access {
        if let Some(notice) = full_disk_access_notice() {
            lines.push(notice);
        }
    }
    if settings.stale_build_notice {
        if let Some(notice) = stale_build_notice(session_name, settings.pinned_exe.as_deref()) {
            lines.push(notice);
        }
    }
    StatusNotices::new(lines)
}

/// What the server is currently telling every client about the session.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct StatusNotices {
    lines: Vec<String>,
}

impl StatusNotices {
    /// The notices as plain text, newest concern first.
    pub fn new(lines: Vec<String>) -> Self {
        StatusNotices { lines }
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// One chunk per notice line, right-aligned, starting at the top of the viewport.
    ///
    /// Truncated rather than wrapped when the viewport is narrow, and suppressed entirely when it
    /// is very narrow: the notice is an interruption already, and one that reflows the top of the
    /// screen is a worse one.
    pub fn character_chunks(&self, viewport: Size, style: &Style) -> Vec<CharacterChunk> {
        if self.lines.is_empty() || viewport.cols < MINIMUM_COLUMNS || viewport.rows == 0 {
            return vec![];
        }
        let budget = viewport.cols.saturating_sub(RIGHT_MARGIN);
        // the palette's error colour, which is what the theme picked for "something is wrong"
        let color = Some(style.colors.exit_code_error.base);
        self.lines
            .iter()
            .take(viewport.rows)
            .enumerate()
            .map(|(row, line)| {
                let text = truncate_to_width(line, budget);
                let x = viewport.cols.saturating_sub(RIGHT_MARGIN + text.width());
                CharacterChunk::new(notice_characters(&text, color), x, row)
            })
            .collect()
    }

    /// The rows a previous set of notices covered, so they can be repainted when it goes away.
    ///
    /// Output is diffed between frames: a notice that simply stops being drawn leaves its glyphs
    /// on screen until something else happens to write those cells.
    pub fn rows_covered(&self, viewport: Size) -> usize {
        if self.lines.is_empty() || viewport.cols < MINIMUM_COLUMNS {
            0
        } else {
            self.lines.len().min(viewport.rows)
        }
    }
}

fn notice_characters(text: &str, color: Option<PaletteColor>) -> Vec<TerminalCharacter> {
    text.chars()
        .map(|character| {
            let mut styles = RcCharacterStyles::reset();
            styles.update(|styles: &mut CharacterStyles| {
                styles.bold = Some(AnsiCode::On);
                if let Some(color) = color {
                    styles.foreground = Some(AnsiCode::from(color));
                }
            });
            TerminalCharacter::new_styled(character, styles)
        })
        .collect()
}

/// Cut a notice to the columns available, counting display width rather than characters.
fn truncate_to_width(line: &str, budget: usize) -> String {
    if line.width() <= budget {
        return line.to_owned();
    }
    // one column for the ellipsis, so a cut line is visibly cut
    let content_budget = budget.saturating_sub(1);
    let mut truncated = String::new();
    for character in line.chars() {
        if truncated.width() + character.width().unwrap_or(0) > content_budget {
            break;
        }
        truncated.push(character);
    }
    truncated.push('…');
    truncated
}

#[cfg(test)]
#[path = "./unit/status_notices_tests.rs"]
mod status_notices_tests;
