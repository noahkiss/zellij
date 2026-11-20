//! fork addition: ConEmu `OSC 9;4` progress reports, kept per pane and forwarded to each client
//! for the pane it has focused.
//!
//! A program reports its progress with `ESC ] 9 ; 4 ; <state> [; <value>] BEL`, and a terminal
//! that understands it - Ghostty, Windows Terminal, ConEmu - draws a progress bar. The states:
//!
//! | state | meaning |
//! |---|---|
//! | 0 | clear |
//! | 1 | value, 0-100 |
//! | 2 | error, value optional |
//! | 3 | indeterminate |
//! | 4 | paused, value optional |
//!
//! The host terminal has one bar per window, and a zellij client shows many panes. So the grid
//! keeps the last report of its own pane, and the screen sends a client the report of the pane
//! that client has focused: again when focus moves to another pane or tab, and a clear when the
//! newly focused pane has nothing to report.

use crate::panes::PaneId;

/// The clear a client gets when its focused pane has no progress to show.
pub const CLEAR_PROGRESS: &str = "\u{1b}]9;4;0\u{7}";

/// The last progress a pane reported. A clear is not stored: the pane then has `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalProgress {
    pub state: u8,
    pub value: Option<u8>,
}

/// What an `OSC 9;4` sequence asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressReport {
    Set(TerminalProgress),
    Clear,
}

impl TerminalProgress {
    /// Parses the params of an `OSC 9` sequence. `None` when it is not a well-formed `9;4`
    /// report, which leaves the sequence to the other `OSC 9` handling.
    pub fn parse(params: &[&[u8]]) -> Option<ProgressReport> {
        if params.get(0) != Some(&&b"9"[..]) || params.get(1) != Some(&&b"4"[..]) {
            return None;
        }
        let state = match params.get(2) {
            // a bare `9;4` carries no state, which ConEmu reads as 0
            None => 0,
            Some(raw) if raw.is_empty() => 0,
            Some(raw) => std::str::from_utf8(raw).ok()?.trim().parse::<u8>().ok()?,
        };
        if state > 4 {
            return None;
        }
        if state == 0 {
            return Some(ProgressReport::Clear);
        }
        let value = params
            .get(3)
            .and_then(|raw| std::str::from_utf8(raw).ok())
            .and_then(|raw| raw.trim().parse::<u16>().ok())
            .map(|value| value.min(100) as u8);
        Some(ProgressReport::Set(TerminalProgress { state, value }))
    }

    /// The sequence that shows this progress on the host terminal.
    pub fn to_osc(&self) -> String {
        match self.value {
            Some(value) => format!("\u{1b}]9;4;{};{}\u{7}", self.state, value),
            None => format!("\u{1b}]9;4;{}\u{7}", self.state),
        }
    }
}

/// What one client was last sent: which pane's progress, at which report, and whether the host
/// terminal is showing a bar now.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClientProgress {
    pane: Option<PaneId>,
    seq: u64,
    showing: bool,
}

impl ClientProgress {
    /// Decides what to send a client after a render. `focused` is the pane the client has
    /// focused, `progress` and `seq` that pane's last report and its report counter.
    ///
    /// A report is forwarded every time the pane sends one, not only when it changes: a host
    /// terminal may drop a bar that is not refreshed, and the program's refresh is what keeps it.
    /// A clear is sent only when the host is showing a bar, so a client whose panes never
    /// report progress is never sent an `OSC 9` at all - some terminals show any `OSC 9` as a
    /// desktop notification.
    pub fn update(
        &mut self,
        focused: Option<PaneId>,
        progress: Option<TerminalProgress>,
        seq: u64,
    ) -> Option<String> {
        if focused == self.pane && seq == self.seq {
            return None;
        }
        self.pane = focused;
        self.seq = seq;
        match progress {
            Some(progress) => {
                self.showing = true;
                Some(progress.to_osc())
            },
            None if self.showing => {
                self.showing = false;
                Some(CLEAR_PROGRESS.to_owned())
            },
            None => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(osc: &str) -> Option<ProgressReport> {
        let params: Vec<&[u8]> = osc.split(';').map(|p| p.as_bytes()).collect();
        TerminalProgress::parse(&params)
    }

    fn set(state: u8, value: Option<u8>) -> Option<ProgressReport> {
        Some(ProgressReport::Set(TerminalProgress { state, value }))
    }

    #[test]
    fn parses_every_conemu_state() {
        assert_eq!(parse("9;4;0"), Some(ProgressReport::Clear));
        assert_eq!(parse("9;4;1;42"), set(1, Some(42)));
        assert_eq!(parse("9;4;2"), set(2, None));
        assert_eq!(parse("9;4;2;80"), set(2, Some(80)));
        assert_eq!(parse("9;4;3"), set(3, None));
        assert_eq!(parse("9;4;4;10"), set(4, Some(10)));
    }

    #[test]
    fn a_bare_report_is_a_clear_and_a_large_value_is_capped() {
        assert_eq!(parse("9;4"), Some(ProgressReport::Clear));
        assert_eq!(parse("9;4;"), Some(ProgressReport::Clear));
        assert_eq!(parse("9;4;1;250"), set(1, Some(100)));
        assert_eq!(parse("9;4;1;x"), set(1, None));
    }

    #[test]
    fn rejects_what_is_not_a_progress_report() {
        assert_eq!(parse("9;4;5"), None);
        assert_eq!(parse("9;4;z"), None);
        assert_eq!(parse("9;hello"), None);
        assert_eq!(parse("9;1;x"), None);
        assert_eq!(parse("777;4;1"), None);
    }

    #[test]
    fn renders_the_osc_a_host_terminal_reads() {
        let with_value = TerminalProgress {
            state: 1,
            value: Some(50),
        };
        let without_value = TerminalProgress {
            state: 3,
            value: None,
        };
        assert_eq!(with_value.to_osc(), "\u{1b}]9;4;1;50\u{7}");
        assert_eq!(without_value.to_osc(), "\u{1b}]9;4;3\u{7}");
    }

    const BUSY: TerminalProgress = TerminalProgress {
        state: 3,
        value: None,
    };
    const HALF: TerminalProgress = TerminalProgress {
        state: 1,
        value: Some(50),
    };

    #[test]
    fn a_client_that_never_saw_progress_is_never_sent_a_clear() {
        let mut client = ClientProgress::default();
        assert_eq!(client.update(Some(PaneId::Terminal(1)), None, 0), None);
        assert_eq!(client.update(Some(PaneId::Terminal(2)), None, 0), None);
        assert_eq!(client.update(None, None, 0), None);
    }

    #[test]
    fn every_report_from_the_focused_pane_is_forwarded() {
        let mut client = ClientProgress::default();
        let pane = Some(PaneId::Terminal(1));
        assert_eq!(client.update(pane, Some(BUSY), 1), Some(BUSY.to_osc()));
        // a render with no new report sends nothing
        assert_eq!(client.update(pane, Some(BUSY), 1), None);
        // the same state sent again is a refresh, and is forwarded
        assert_eq!(client.update(pane, Some(BUSY), 2), Some(BUSY.to_osc()));
        assert_eq!(
            client.update(pane, None, 3),
            Some(CLEAR_PROGRESS.to_owned())
        );
        // a second clear while nothing shows is swallowed
        assert_eq!(client.update(pane, None, 4), None);
    }

    #[test]
    fn a_focus_switch_re_emits_the_new_panes_progress() {
        let mut client = ClientProgress::default();
        assert_eq!(
            client.update(Some(PaneId::Terminal(1)), Some(BUSY), 1),
            Some(BUSY.to_osc())
        );
        assert_eq!(
            client.update(Some(PaneId::Terminal(2)), Some(HALF), 7),
            Some(HALF.to_osc())
        );
        // back to the first pane, whose report counter has not moved
        assert_eq!(
            client.update(Some(PaneId::Terminal(1)), Some(BUSY), 1),
            Some(BUSY.to_osc())
        );
    }

    #[test]
    fn a_focus_switch_to_a_quiet_pane_clears_the_bar() {
        let mut client = ClientProgress::default();
        client.update(Some(PaneId::Terminal(1)), Some(BUSY), 1);
        assert_eq!(
            client.update(Some(PaneId::Terminal(2)), None, 0),
            Some(CLEAR_PROGRESS.to_owned())
        );
        // a closed pane leaves nothing focused, or another pane: either clears
        client.update(Some(PaneId::Terminal(1)), Some(BUSY), 1);
        assert_eq!(
            client.update(None, None, 0),
            Some(CLEAR_PROGRESS.to_owned())
        );
    }
}
