//! fork addition: marks a trzsz trigger as having passed through a multiplexer.
//!
//! `trz` and `tsz` start a transfer by printing a plain-text trigger:
//! `ESC[s::TRZSZ:TRANSFER:<mode>:<version>:<13-digit id>:<port>`. The client side - `tssh`, or a
//! trzsz-aware terminal - watches its output for it. zellij prints the trigger onto the grid like
//! any other text, so every later redraw of that row sends it to the client again, and the client
//! starts the transfer again. It types its failure into the shell.
//!
//! trzsz already has an answer, made for tmux: an id ending `00` comes straight from `trz`, and the
//! client starts a transfer for it every time it appears; any other id is started once and a repeat
//! is ignored. Inside tmux, `trz` itself adds 20 to its id, and the trzsz relay rewrites a trailing
//! `00` to `20` (`rewriteTrzszTrigger` in trzsz-go). This does the same rewrite on the pane's
//! output. The tunnel handshake drops the id's last two digits, so the rewritten id still connects.

use regex::bytes::Regex;
use std::sync::OnceLock;

fn trigger_id() -> &'static Regex {
    static TRIGGER_ID: OnceLock<Regex> = OnceLock::new();
    TRIGGER_ID.get_or_init(|| {
        Regex::new(r"::TRZSZ:TRANSFER:[SRDF]:\d+\.\d+\.\d+:(\d{13}\d*)")
            .expect("the trzsz trigger pattern compiles")
    })
}

/// Rewrites, in place, the id of every trzsz trigger in `bytes` that ends `00` to end `20`.
/// The length never changes. A trigger split across two reads is left as it is.
pub fn mark_trzsz_triggers(bytes: &mut [u8]) {
    const MARKER: &[u8] = b"::TRZSZ:TRANSFER:";
    if !bytes.windows(MARKER.len()).any(|window| window == MARKER) {
        return;
    }
    let ends: Vec<usize> = trigger_id()
        .captures_iter(bytes)
        .filter_map(|captures| captures.get(1))
        .filter(|id| id.as_bytes().ends_with(b"00"))
        .map(|id| id.end())
        .collect();
    for end in ends {
        bytes[end - 2] = b'2';
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(input: &str) -> String {
        let mut bytes = input.as_bytes().to_vec();
        mark_trzsz_triggers(&mut bytes);
        String::from_utf8(bytes).unwrap()
    }

    #[test]
    fn a_trigger_straight_from_trz_is_marked() {
        assert_eq!(
            marked("\x1b[s::TRZSZ:TRANSFER:R:1.2.0:1760180000000:41235\r\n"),
            "\x1b[s::TRZSZ:TRANSFER:R:1.2.0:1760180000020:41235\r\n"
        );
    }

    #[test]
    fn every_mode_is_marked() {
        for mode in ["S", "R", "D", "F"] {
            assert_eq!(
                marked(&format!("::TRZSZ:TRANSFER:{}:1.2.0:1234567890100", mode)),
                format!("::TRZSZ:TRANSFER:{}:1.2.0:1234567890120", mode)
            );
        }
    }

    #[test]
    fn an_id_already_marked_is_left_alone() {
        // tmux's 20, and the 10 trz uses on Windows
        let tmux = "::TRZSZ:TRANSFER:S:1.2.0:1760180000020:41235";
        let windows = "::TRZSZ:TRANSFER:S:1.2.0:1760180000010:41235";
        assert_eq!(marked(tmux), tmux);
        assert_eq!(marked(windows), windows);
        // so a second pass changes nothing
        assert_eq!(marked(&marked(tmux)), tmux);
    }

    #[test]
    fn only_the_id_changes_and_the_surrounding_output_is_kept() {
        assert_eq!(
            marked("before 100 ::TRZSZ:TRANSFER:F:1.2.0:1760180000000:4100 after 00"),
            "before 100 ::TRZSZ:TRANSFER:F:1.2.0:1760180000020:4100 after 00"
        );
    }

    #[test]
    fn two_triggers_in_one_read_are_both_marked() {
        assert_eq!(
            marked(
                "::TRZSZ:TRANSFER:R:1.2.0:1111111111100\r\n::TRZSZ:TRANSFER:S:1.2.0:2222222222200"
            ),
            "::TRZSZ:TRANSFER:R:1.2.0:1111111111120\r\n::TRZSZ:TRANSFER:S:1.2.0:2222222222220"
        );
    }

    #[test]
    fn what_is_not_a_trigger_is_left_alone() {
        let short_id = "::TRZSZ:TRANSFER:R:1.2.0:176018000000:41235";
        let bad_mode = "::TRZSZ:TRANSFER:X:1.2.0:1760180000000:41235";
        let plain = "an ordinary line ending 00";
        assert_eq!(marked(short_id), short_id);
        assert_eq!(marked(bad_mode), bad_mode);
        assert_eq!(marked(plain), plain);
    }
}
