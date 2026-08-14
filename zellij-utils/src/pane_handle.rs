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
/// With 55391 combinations and a session's worth of panes taken, a collision is rare and two in a
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

fn random_handle() -> String {
    let adjective = ADJECTIVES[random_index(ADJECTIVES.len())];
    let noun = NOUNS[random_index(NOUNS.len())];
    format!("{}{}{}", adjective, HANDLE_SEPARATOR, noun)
}

/// A random index below `len`, entropy borrowed from the same source pane uuids come from.
///
/// `uuid` is already a dependency and its v4 constructor is already how this crate gets randomness
/// for panes, so a handle costs no new dependency. The modulo bias over a 128-bit draw into a list
/// of a few hundred words is far below anything a name generator cares about.
fn random_index(len: usize) -> usize {
    let bytes = *uuid::Uuid::new_v4().as_bytes();
    let draw = u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[6], bytes[7], bytes[9],
    ]);
    (draw % len as u64) as usize
}

/// Whether `candidate` is shaped like a handle: two of this crate's words joined by a dash.
///
/// Shape only - it says nothing about whether a pane answers to it. Membership in the word lists
/// is what keeps the target parser unambiguous: `sunny-otter` can only be a handle, and a string
/// that merely looks two-worded is not mistaken for one.
pub fn is_handle_shaped(candidate: &str) -> bool {
    let Some((adjective, noun)) = candidate.split_once(HANDLE_SEPARATOR) else {
        return false;
    };
    // A suffixed fallback handle (`sunny-otter-2`) is still a handle; anything else after the
    // noun is not, so a three-word string does not sneak through as one.
    let noun = match noun.split_once(HANDLE_SEPARATOR) {
        Some((noun, suffix)) if suffix.parse::<usize>().is_ok() => noun,
        Some(_) => return false,
        None => noun,
    };
    ADJECTIVES.contains(&adjective) && NOUNS.contains(&noun)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
        // the collision rule in one test: whatever the first draw was, the generator must not
        // return it while the predicate says it is spoken for
        let first = generate_handle(|_| false);
        let taken = first.clone();
        let second = generate_handle(|candidate| candidate == taken);
        assert_ne!(second, first);
        assert!(is_handle_shaped(&second));
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
    fn handle_shape_rejects_what_is_not_a_handle() {
        for not_a_handle in [
            "",
            "otter",
            "sunny",
            "terminal_1",
            "plugin_2",
            "3",
            "f1e5dce9-a073-4594-b270-41f002924a9b",
            "sunny_otter",        // wrong separator
            "sunny-walrus-otter", // three words: only a numeric suffix may follow the noun
            "Sunny-Otter",        // handles are lowercase
            "purple-otter",       // `purple` is not in the adjective list
            "sunny-teapot",       // `teapot` is not in the noun list
        ] {
            assert!(
                !is_handle_shaped(not_a_handle),
                "should not read as a handle: {:?}",
                not_a_handle
            );
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
