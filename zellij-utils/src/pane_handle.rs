//! Pane handles: the two-word name a human uses to address a pane.
//!
//! A handle is the pane's ADDRESS, the pane's uuid is its LINEAGE. They answer different
//! questions, so they behave differently across a restore: the uuid rotates (a restored pane is a
//! new process and says so, see `PaneInfo::restored_from`) while the handle carries, because the
//! whole point of an address is that it still reaches the same pane tomorrow.
//!
//! A handle is unique among a session's LIVE panes, not forever. Once a pane closes its handle is
//! free to be handed out again - there is no promise that a handle you wrote down last week names
//! the same pane, only that it names at most one pane right now.

use crate::vendored::petname::{ADJECTIVES, NOUNS};

/// The separator between a handle's two words.
pub const HANDLE_SEPARATOR: char = '-';

/// How many rerolls a collision is worth before the generator falls back to a numeric suffix.
///
/// With 55104 combinations and a session's worth of panes taken, a collision is rare and two in a
/// row is rarer still; the cap is here so a caller with a pathological predicate terminates, not
/// because it is expected to be reached.
const MAX_REROLLS: usize = 32;

/// Generates a handle no live pane is using, per `is_taken`.
///
/// The caller owns the collision predicate because the caller owns the pane registry: this module
/// knows how to spell a handle, the server knows which ones are spoken for. `is_taken` is called
/// with the candidate handle and must return true if some live pane already answers to it.
///
/// Rerolls on collision. If the rerolls run out - which needs either a wildly full session or a
/// predicate that refuses everything - it appends `-2`, `-3` and so on to the last candidate until
/// the predicate is satisfied, so this always returns a usable handle rather than failing at a
/// point where the caller has a pane in hand and nothing to name it.
pub fn generate_handle(is_taken: impl Fn(&str) -> bool) -> String {
    let mut candidate = random_handle();
    for _ in 0..MAX_REROLLS {
        if !is_taken(&candidate) {
            return candidate;
        }
        candidate = random_handle();
    }
    // The suffix is deliberately ugly. A handle that reads oddly is a signal that the session is
    // in a state worth looking at, and it is still a handle: unique, typeable, and addressable.
    for suffix in 2..usize::MAX {
        let suffixed = format!("{}{}{}", candidate, HANDLE_SEPARATOR, suffix);
        if !is_taken(&suffixed) {
            return suffixed;
        }
    }
    candidate
}

/// The handle at position `n` of the handle space, in a fixed order.
///
/// Every `n` below `ADJECTIVES.len() * NOUNS.len()` names a different handle, and the same `n`
/// always names the same handle. Production draws at random ([`generate_handle`]); this exists for
/// a test that renders a pane frame, where a random address would make a golden snapshot a
/// coin toss.
pub fn nth_handle(n: usize) -> String {
    let adjective = ADJECTIVES[n % ADJECTIVES.len()];
    let noun = NOUNS[(n / ADJECTIVES.len()) % NOUNS.len()];
    format!("{}{}{}", adjective, HANDLE_SEPARATOR, noun)
}

/// How many handles [`nth_handle`] can name before it repeats itself.
pub fn handle_space_size() -> usize {
    ADJECTIVES.len() * NOUNS.len()
}

fn random_handle() -> String {
    let adjective = ADJECTIVES[random_index(ADJECTIVES.len())];
    let noun = NOUNS[random_index(NOUNS.len())];
    format!("{}{}{}", adjective, HANDLE_SEPARATOR, noun)
}

/// A random index below `len`, entropy borrowed from the same source pane uuids come from.
///
/// `uuid` is already a dependency and its v4 constructor is already how this crate gets randomness
/// for panes, so a handle costs no new dependency. Eight of its bytes make the draw, skipping the
/// variant byte; the version nibble in byte 6 is fixed, so the draw carries about 60 random bits.
/// The modulo bias that leaves over a list of a few hundred words is far below anything a name
/// generator cares about.
fn random_index(len: usize) -> usize {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let draw = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[6], bytes[7], bytes[9],
    ]);
    (draw % len as u64) as usize
}

/// The most words a handle can be made of, counting a numeric suffix as one.
const MAX_HANDLE_WORDS: usize = 4;
/// The most characters one of those words can be.
const MAX_HANDLE_WORD_LEN: usize = 16;
/// The most characters a whole handle can be.
///
/// A handle is drawn on the pane frame and typed by hand, and both of those have opinions. The
/// generated ones are well inside this; the bound is here for the chosen ones.
pub const MAX_HANDLE_LEN: usize = 40;

/// Whether `candidate` is shaped like a handle: lowercase words joined by dashes.
///
/// Shape only - it says nothing about whether a pane answers to it. This is what makes the target
/// parser unambiguous, so the grammar is drawn to exclude the other forms a `--pane-id` takes:
///
/// - `terminal_7`, `plugin_2`: underscores are not in the grammar
/// - a bare integer: a handle has at least one letter in it
/// - a uuid: five dash-separated groups, and a handle is at most four words
///
/// It is a *grammar* rather than a word list because a handle can be chosen - `zellij action
/// new-pane --handle build` - and the CLI has no way to know which names a session's panes were
/// given. So a well-formed name that no pane answers to is a miss (exit 2) rather than malformed
/// input (exit 1); only a string that is not a handle in any session is refused by the parser.
pub fn is_handle_shaped(candidate: &str) -> bool {
    handle_grammar_error(candidate).is_none()
}

/// Why `candidate` is not shaped like a handle, in words a caller can print.
///
/// `None` means it is. This is [`is_handle_shaped`] with its reasons kept, for the one caller that
/// has somebody to tell: whoever typed `--handle`.
pub fn handle_grammar_error(candidate: &str) -> Option<String> {
    let says = |reason: &str| Some(format!("'{}' is not a handle: {}", candidate, reason));
    if candidate.is_empty() {
        return says("a handle has at least one word in it");
    }
    if candidate.len() > MAX_HANDLE_LEN {
        return says(&format!(
            "a handle is at most {} characters",
            MAX_HANDLE_LEN
        ));
    }
    let words: Vec<&str> = candidate.split(HANDLE_SEPARATOR).collect();
    if words.len() > MAX_HANDLE_WORDS {
        return says(&format!(
            "a handle is at most {} words joined by '{}'",
            MAX_HANDLE_WORDS, HANDLE_SEPARATOR
        ));
    }
    for word in &words {
        if word.is_empty() {
            return says("every word has to have something in it");
        }
        if word.len() > MAX_HANDLE_WORD_LEN {
            return says(&format!(
                "a word is at most {} characters",
                MAX_HANDLE_WORD_LEN
            ));
        }
        if !word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        {
            return says("a handle is lowercase letters and digits, joined by dashes");
        }
    }
    if !candidate.chars().any(|c| c.is_ascii_lowercase()) {
        // digits alone are how a bare `--pane-id 7` is spelled, and it means terminal_7
        return says("a handle has at least one letter in it, so a number stays a pane id");
    }
    None
}

/// Why `candidate` is not a handle somebody may choose, in words the caller can print.
///
/// `None` means it is one. Stricter than the grammar, on one point: a handle that starts with
/// `terminal` or `plugin` is refused even though it parses, because `terminal-1` beside
/// `terminal_1` is a name that will be typed wrong on the day it matters.
pub fn chosen_handle_error(candidate: &str) -> Option<String> {
    if let Some(reason) = handle_grammar_error(candidate) {
        return Some(reason);
    }
    let first_word = candidate
        .split(HANDLE_SEPARATOR)
        .next()
        .unwrap_or(candidate);
    if first_word == "terminal" || first_word == "plugin" {
        return Some(format!(
            "'{}' is too close to the pane id '{}_1' to be a handle. Pick a name that cannot be \
             read as an id.",
            candidate, first_word
        ));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn a_chosen_handle_is_words_a_person_would_pick() {
        for chosen in [
            "build",
            "web",
            "my-build",
            "web-2",
            "one-two-three-four",
            "sunny-otter",
        ] {
            assert_eq!(chosen_handle_error(chosen), None, "refused: {}", chosen);
            assert!(is_handle_shaped(chosen), "not handle-shaped: {}", chosen);
        }
    }

    #[test]
    fn a_chosen_handle_cannot_be_shaped_like_another_target_form() {
        // the grammar exists to keep one flag able to take four forms: anything that could be read
        // as an id or a uuid is not a handle
        for (rejected, why) in [
            ("terminal_1", "underscore"),
            ("plugin_2", "underscore"),
            ("7", "a bare number is terminal_7"),
            ("2-3", "digits alone"),
            ("e9b82dbd-0000-4000-8000-0000000000aa", "a uuid"),
            ("e9b82dbd0000400080000000000000aa", "a uuid, undashed"),
            ("Sunny-Otter", "uppercase"),
            ("sunny otter", "a space"),
            ("", "nothing at all"),
            ("-otter", "an empty word"),
            ("otter-", "an empty word"),
            ("sunny--otter", "an empty word"),
            ("one-two-three-four-five", "too many words"),
            ("terminal-1", "too close to an id"),
            ("plugin-inspector", "too close to an id"),
        ] {
            assert!(
                chosen_handle_error(rejected).is_some(),
                "should be refused ({}): {:?}",
                why,
                rejected
            );
        }
    }

    #[test]
    fn a_handle_is_bounded_in_length() {
        let too_long_word = "a".repeat(MAX_HANDLE_WORD_LEN + 1);
        assert!(chosen_handle_error(&too_long_word).is_some());
        let whole_thing = vec!["abcdefghijkl"; 4].join("-");
        assert!(whole_thing.len() > MAX_HANDLE_LEN);
        assert!(chosen_handle_error(&whole_thing).is_some());
    }

    #[test]
    fn a_grammar_error_says_what_was_wrong_with_it() {
        let message = handle_grammar_error("Sunny").expect("uppercase is not a handle");
        assert!(message.contains("Sunny"), "got: {}", message);
        assert!(message.contains("lowercase"), "got: {}", message);
    }

    #[test]
    fn a_generated_handle_is_two_words_from_the_lists() {
        let handle = generate_handle(|_| false);
        let (adjective, noun) = handle.split_once(HANDLE_SEPARATOR).expect("two words");
        assert!(ADJECTIVES.contains(&adjective), "got: {}", handle);
        assert!(NOUNS.contains(&noun), "got: {}", handle);
        assert!(is_handle_shaped(&handle), "got: {}", handle);
    }

    #[test]
    fn a_taken_handle_is_rerolled_around() {
        // refuse the first three distinct draws and accept the fourth: a generator that ignored
        // the predicate would hand back a name it was just told is spoken for
        let refused = std::cell::RefCell::new(HashSet::new());
        let handle = generate_handle(|candidate| {
            let mut refused = refused.borrow_mut();
            if refused.len() < 3 {
                refused.insert(candidate.to_owned());
                return true;
            }
            false
        });
        let refused = refused.into_inner();
        assert_eq!(
            refused.len(),
            3,
            "the predicate was not consulted three times"
        );
        assert!(
            !refused.contains(&handle),
            "handed out a handle the predicate refused: {}",
            handle
        );
        assert!(is_handle_shaped(&handle));
    }

    #[test]
    fn a_predicate_that_refuses_everything_still_yields_a_handle() {
        // the cap exists so this terminates - a caller holding a new pane always gets a name
        let refusals = std::cell::Cell::new(0);
        let handle = generate_handle(|candidate| {
            refusals.set(refusals.get() + 1);
            // refuse every bare two-word handle, accept only a suffixed one
            candidate.matches(HANDLE_SEPARATOR).count() < 2
        });
        assert!(
            refusals.get() > MAX_REROLLS,
            "expected the rerolls to run out"
        );
        assert!(handle.ends_with("-2"), "got: {}", handle);
        assert!(is_handle_shaped(&handle), "got: {}", handle);
    }

    #[test]
    fn generated_handles_are_unique_when_the_predicate_is_honest() {
        // what a session actually does: hand out handles one at a time, each checked against the
        // ones already live
        let mut live: HashSet<String> = HashSet::new();
        for _ in 0..500 {
            let handle = {
                let live = &live;
                generate_handle(|candidate| live.contains(candidate))
            };
            assert!(live.insert(handle.clone()), "duplicate handle: {}", handle);
        }
    }

    #[test]
    fn generation_is_not_stuck_on_one_name() {
        // a generator that always drew the same words would pass every test above
        let drawn: HashSet<String> = (0..50).map(|_| generate_handle(|_| false)).collect();
        assert!(drawn.len() > 40, "too few distinct handles: {:?}", drawn);
    }

    #[test]
    fn both_words_are_drawn_and_not_just_one() {
        // whole-handle variety hides a stuck list: a fixed adjective with a random noun still
        // yields 50 distinct handles, so each position is counted on its own. 100 draws over the
        // shorter list leaves the odds of a false alarm below any number worth writing down.
        let mut adjectives = HashSet::new();
        let mut nouns = HashSet::new();
        for _ in 0..100 {
            let handle = generate_handle(|_| false);
            let (adjective, noun) = handle.split_once(HANDLE_SEPARATOR).expect("two words");
            adjectives.insert(adjective.to_owned());
            nouns.insert(noun.to_owned());
        }
        assert!(
            adjectives.len() > 20,
            "adjectives barely move: {:?}",
            adjectives
        );
        assert!(nouns.len() > 20, "nouns barely move: {:?}", nouns);
    }

    #[test]
    fn the_ordered_draw_names_a_different_handle_every_time() {
        // what a rendering test leans on: same n, same handle, and no two n share one
        assert_eq!(nth_handle(7), nth_handle(7));
        let space = handle_space_size();
        let drawn: HashSet<String> = (0..500).map(nth_handle).collect();
        assert_eq!(drawn.len(), 500, "the ordered draw repeated itself early");
        assert!(drawn.iter().all(|handle| is_handle_shaped(handle)));
        // it wraps rather than panicking, which is what makes it safe past the end of the space
        assert_eq!(nth_handle(0), nth_handle(space));
    }

    #[test]
    fn handle_shape_rejects_what_is_not_a_handle() {
        for not_a_handle in [
            "",
            "terminal_1",
            "plugin_2",
            "3",
            "f1e5dce9-a073-4594-b270-41f002924a9b",
            "sunny_otter", // wrong separator
            "Sunny-Otter", // handles are lowercase
        ] {
            assert!(
                !is_handle_shaped(not_a_handle),
                "should not read as a handle: {:?}",
                not_a_handle
            );
        }
    }

    #[test]
    fn a_name_off_the_word_lists_is_still_a_handle() {
        // the word lists are how handles are GENERATED, not what makes a string one. A chosen
        // handle can be any word, and the parser cannot know which words a session's panes were
        // given - so these are well-formed targets that no pane may answer to, which is a miss and
        // not malformed input
        for chosen in [
            "otter",
            "purple-otter",
            "sunny-teapot",
            "sunny-walrus-otter",
        ] {
            assert!(is_handle_shaped(chosen), "should be a handle: {}", chosen);
        }
    }

    #[test]
    fn handle_shape_accepts_a_suffixed_fallback() {
        assert!(is_handle_shaped("sunny-otter"));
        assert!(is_handle_shaped("sunny-otter-2"));
    }

    #[test]
    fn the_word_lists_hold_up_their_end() {
        // the parser leans on these being unambiguous; a stray uppercase or overlong word would
        // make a handle unspellable and a duplicate would silently halve a list
        for (label, words) in [("adjective", &ADJECTIVES[..]), ("noun", &NOUNS[..])] {
            let unique: HashSet<&&str> = words.iter().collect();
            assert_eq!(unique.len(), words.len(), "duplicate {}", label);
            for word in words {
                assert!(
                    word.len() >= 3 && word.len() <= 8,
                    "{} out of length range: {}",
                    label,
                    word
                );
                assert!(
                    word.chars().all(|c| c.is_ascii_lowercase()),
                    "{} is not lowercase ascii: {}",
                    label,
                    word
                );
            }
        }
        // no word may appear in both lists, or `sunny-otter` could not be split back into its parts
        let adjectives: HashSet<&&str> = ADJECTIVES.iter().collect();
        for noun in &NOUNS {
            assert!(!adjectives.contains(noun), "{} is in both lists", noun);
        }
    }
}
