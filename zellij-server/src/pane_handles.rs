//! The session's live pane handles.
//!
//! A handle is unique among the panes alive in one session. A zellij server process serves exactly
//! one session, so a process-global set IS the session's set - which is why this is a static
//! rather than something threaded through the three dozen places a pane gets built.
//!
//! Claims are RAII: a pane owns a [`HeldHandle`], and closing the pane frees the name for reuse.
//! Uniqueness is over LIVE panes only, deliberately. A handle is an address, and an address that
//! could never be reissued would drain the word lists over a long-lived session.
//!
//! A freshly generated handle also reserves both its words: no two live panes share an adjective
//! or share a noun, even across different handles. That is `shares_a_word`, not the exact-match
//! `is_spoken_for` a caller asking about one specific name still gets. A chosen or restored handle
//! is exempt - it is taken verbatim by [`HeldHandle::claim`] - so the reservation is a property of
//! generation, not of the registry as a whole.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use zellij_utils::pane_handle::generate_handle;

/// Whether this process names its panes in order instead of drawing them at random.
///
/// A pane frame shows its handle, and a great many tests snapshot a rendered frame. A random
/// address would make every one of those a coin toss - not only because the address itself is
/// printed, but because its WIDTH moves the centered title beside it (see
/// `compose_bracketed_title`), so blanking the handle out of a snapshot is not enough.
///
/// `cfg!(test)` covers this crate's own tests. It does NOT cover the whole-app integration suite,
/// which links this crate as an ordinary dependency and so is built without it - hence the
/// variable, which `zellij-integration-tests` sets for its own process and nothing else reads.
fn names_in_order() -> bool {
    #[cfg(test)]
    {
        true
    }
    #[cfg(not(test))]
    {
        static IN_ORDER: OnceLock<bool> = OnceLock::new();
        *IN_ORDER.get_or_init(|| std::env::var_os(SEQUENTIAL_HANDLES_VAR).is_some())
    }
}

/// The variable that asks for in-order handles. Set by the integration harness, not by a user.
pub const SEQUENTIAL_HANDLES_VAR: &str = "ZELLIJ_SEQUENTIAL_PANE_HANDLES";

/// Draws a handle nothing has spoken for.
fn draw_handle(is_taken: impl Fn(&str) -> bool) -> String {
    if names_in_order() {
        next_handle_in_order(is_taken)
    } else {
        generate_handle(is_taken)
    }
}

/// The next unclaimed handle in a fixed walk of the handle space.
///
/// The counter is per thread because a thread is the unit a session is driven from here - one
/// session's panes are numbered from zero whatever another session is doing beside it.
///
/// The randomness this stands in for is tested where it lives, in
/// `zellij_utils::pane_handle`; what this module is responsible for - uniqueness among live panes,
/// reservation, release on close - is the same either way.
fn next_handle_in_order(is_taken: impl Fn(&str) -> bool) -> String {
    use std::cell::Cell;
    use zellij_utils::pane_handle::{handle_space_size, nth_handle};
    thread_local! {
        static NEXT: Cell<usize> = const { Cell::new(0) };
    }
    NEXT.with(|next| {
        for _ in 0..handle_space_size() {
            let candidate = nth_handle(next.get());
            next.set(next.get() + 1);
            if !is_taken(&candidate) {
                return candidate;
            }
        }
        generate_handle(is_taken)
    })
}

/// What a name in the registry is spoken for by.
#[derive(Debug, PartialEq, Eq)]
enum Claim {
    /// A live pane answers to it.
    Held,
    /// A layout about to be applied carries it, and the pane that will hold it does not exist yet.
    ///
    /// Reserved names are invisible to a snapshot's own panes and opaque to everyone else: a
    /// freshly generated handle rerolls around them, a restoring pane walks in and takes the one
    /// meant for it. This is what "snapshot handles win" is made of.
    Reserved,
}

/// The set of claims a draw must avoid.
///
/// One per session. A zellij server process serves exactly one session, so process-global IS the
/// session's set - except in the integration suite, where several sessions share one process and
/// each is driven from its own thread. In-order naming and a per-thread registry go together:
/// a shared registry would make one session's handles depend on what another happened to be doing
/// beside it, which is the same coin toss the ordering exists to remove.
fn registry() -> MutexGuard<'static, HashMap<String, Claim>> {
    if names_in_order() {
        per_thread_registry()
    } else {
        process_registry()
    }
}

fn process_registry() -> MutexGuard<'static, HashMap<String, Claim>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Claim>>> = OnceLock::new();
    REGISTRY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn per_thread_registry() -> MutexGuard<'static, HashMap<String, Claim>> {
    thread_local! {
        static REGISTRY: &'static Mutex<HashMap<String, Claim>> =
            Box::leak(Box::new(Mutex::new(HashMap::new())));
    }
    REGISTRY.with(|registry| {
        registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    })
}

/// Whether a name is out of the generator's reach: held by a live pane, or reserved for one.
///
/// Reserved counts, and that is the whole reservation mechanism - a name a layout is about to
/// restore a pane under must not be handed to a pane built in the meantime.
fn is_spoken_for(registry: &HashMap<String, Claim>, candidate: &str) -> bool {
    registry.contains_key(candidate)
}

/// A handle's words, for reservation purposes: every dash-separated segment except a purely
/// numeric one.
///
/// The numeric segment is a suffix fallback (`sunny-otter-2`) rather than a word, and does not
/// belong to either generated position - keeping it out of the reserved set is what lets the
/// suffix mechanism reuse an already-reserved pair when it has to.
fn handle_words(handle: &str) -> impl Iterator<Item = &str> {
    handle
        .split('-')
        .filter(|segment| !segment.is_empty() && !segment.chars().all(|c| c.is_ascii_digit()))
}

/// Whether `candidate` shares a word - an adjective or a noun - with any name in `registry`.
///
/// This is the collision predicate `generate_handle` draws against: no two live panes may share
/// either word of their handle, held or reserved alike. It replaces plain exact-match uniqueness,
/// which is still what [`is_spoken_for`] gives a caller that wants to know about one specific name.
fn shares_a_word(registry: &HashMap<String, Claim>, candidate: &str) -> bool {
    let candidate_words: Vec<&str> = handle_words(candidate).collect();
    registry
        .keys()
        .any(|live| handle_words(live).any(|word| candidate_words.contains(&word)))
}

/// Whether a live pane already answers to `handle`.
///
/// A reserved name counts as taken: a pane is coming back under it, and handing it to somebody else
/// in the meantime would take the address away from the pane that had it first.
pub fn is_live(handle: &str) -> bool {
    is_spoken_for(&registry(), handle)
}

/// A handle held by a live pane, freed when that pane drops.
#[derive(Debug)]
pub struct HeldHandle(String);

impl HeldHandle {
    /// Takes a handle that shares no word with a pane that is live or a layout has reserved.
    pub fn claim_new() -> Self {
        let mut registry = registry();
        let handle = draw_handle(|candidate| shares_a_word(&registry, candidate));
        registry.insert(handle.clone(), Claim::Held);
        HeldHandle(handle)
    }

    /// Takes `wanted` if it is free or reserved for this pane, and a fresh handle if it is not.
    ///
    /// The fallback is what keeps the invariant "every pane has a handle, and no two live panes
    /// share one" true even for a snapshot that somehow names the same handle twice. Restoring a
    /// session must not fail because two panes want the same name; one of them just gets a new one.
    pub fn claim(wanted: &str) -> Self {
        let mut registry = registry();
        if registry.get(wanted) != Some(&Claim::Held) {
            registry.insert(wanted.to_owned(), Claim::Held);
            return HeldHandle(wanted.to_owned());
        }
        drop(registry);
        HeldHandle::claim_new()
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Drop for HeldHandle {
    fn drop(&mut self) {
        registry().remove(&self.0);
    }
}

/// Names a layout carries, held out of the generator's reach until its panes are built.
///
/// Without this, a handle-less pane earlier in the same layout could be handed a name that a later
/// pane is about to restore under, and the restoring pane - the one with the prior claim - would be
/// the one to reroll. Reserving first inverts that: the snapshot's names are spoken for before the
/// first pane is built.
///
/// A restore is a session-wide event, not a tab-wide one: it announces every tab before the first
/// tab's panes exist. So one reservation grows across all of them ([`Reservation::extend`]) and is
/// released once, when the last tab has been applied. A per-tab reservation would leave a
/// handle-less pane in tab 1 free to take a name tab 3 is coming back under.
#[derive(Debug, Default)]
pub struct Reservation(Vec<String>);

impl Reservation {
    /// Reserves each handle that is not already held by a live pane.
    pub fn hold(handles: impl IntoIterator<Item = String>) -> Self {
        let mut reservation = Reservation::default();
        reservation.extend(handles);
        reservation
    }

    /// Adds more names to this reservation, on the same terms.
    pub fn extend(&mut self, handles: impl IntoIterator<Item = String>) {
        let mut registry = registry();
        for handle in handles {
            if !registry.contains_key(handle.as_str()) {
                registry.insert(handle.clone(), Claim::Reserved);
                self.0.push(handle);
            }
        }
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // Releases only what is still merely reserved. A name a pane has since claimed is that
        // pane's to free, so a layout that fails halfway leaves nothing stranded either way.
        let mut registry = registry();
        for handle in &self.0 {
            if registry.get(handle.as_str()) == Some(&Claim::Reserved) {
                registry.remove(handle.as_str());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A name for a test to claim that no drawn handle can ever collide with.
    ///
    /// The registry is per thread under `cargo test`, so a sibling test cannot reach it - but a
    /// draw made *within* one of these tests still can, and `claim` takes any string. A name the
    /// generator cannot produce keeps each test about the one thing it is testing.
    fn reserved_for_tests(name: &str) -> String {
        format!("test~{}", name)
    }

    #[test]
    fn two_live_panes_never_share_a_handle() {
        let held: Vec<HeldHandle> = (0..200).map(|_| HeldHandle::claim_new()).collect();
        let mut names: Vec<&str> = held.iter().map(|h| h.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "a handle was handed out twice");
    }

    #[test]
    fn two_live_panes_never_share_a_word() {
        // reservation is stricter than exact-match uniqueness: not only is the whole handle
        // unique, neither of its two words repeats across any other live pane's handle
        let held: Vec<HeldHandle> = (0..200).map(|_| HeldHandle::claim_new()).collect();
        let mut adjectives: Vec<&str> = Vec::new();
        let mut nouns: Vec<&str> = Vec::new();
        for handle in &held {
            let (adjective, noun) = handle.as_str().split_once('-').expect("two words");
            adjectives.push(adjective);
            nouns.push(noun);
        }
        adjectives.sort_unstable();
        let adjective_count = adjectives.len();
        adjectives.dedup();
        assert_eq!(adjectives.len(), adjective_count, "an adjective was reused");
        nouns.sort_unstable();
        let noun_count = nouns.len();
        nouns.dedup();
        assert_eq!(nouns.len(), noun_count, "a noun was reused");
    }

    #[test]
    fn handle_words_ignores_a_numeric_suffix_segment() {
        let words: Vec<&str> = handle_words("sunny-otter-2").collect();
        assert_eq!(words, vec!["sunny", "otter"]);
    }

    #[test]
    fn shares_a_word_catches_a_collision_on_either_position() {
        let mut registry = HashMap::new();
        registry.insert("sunny-otter".to_owned(), Claim::Held);
        assert!(
            shares_a_word(&registry, "sunny-badger"),
            "adjective collision missed"
        );
        assert!(
            shares_a_word(&registry, "brave-otter"),
            "noun collision missed"
        );
        assert!(!shares_a_word(&registry, "brave-badger"), "false collision");
    }

    #[test]
    fn shares_a_word_sees_the_real_words_behind_a_suffix_fallback() {
        // the numeric segment of a suffixed handle is not a word, but the two real words behind
        // it are still reserved - that is what stops a suffix fallback from quietly freeing them
        let mut registry = HashMap::new();
        registry.insert("sunny-otter-2".to_owned(), Claim::Held);
        assert!(
            shares_a_word(&registry, "sunny-badger"),
            "the numeric suffix should not hide the real words"
        );
    }

    #[test]
    fn a_closed_panes_handle_comes_back_into_circulation() {
        let name = {
            let handle = HeldHandle::claim_new();
            handle.as_str().to_owned()
        };
        // the pane is gone, so the name is free for the taking - uniqueness is over LIVE panes
        let reclaimed = HeldHandle::claim(&name);
        assert_eq!(reclaimed.as_str(), name);
    }

    #[test]
    fn a_snapshot_handle_is_taken_verbatim() {
        let wanted = reserved_for_tests("verbatim");
        let restored = HeldHandle::claim(&wanted);
        assert_eq!(restored.as_str(), wanted);
    }

    #[test]
    fn a_snapshot_handle_a_live_pane_already_holds_falls_back() {
        let wanted = reserved_for_tests("contended");
        let _live = HeldHandle::claim(&wanted);
        let second = HeldHandle::claim(&wanted);
        assert_ne!(
            second.as_str(),
            wanted,
            "two live panes answered to the same handle"
        );
    }

    #[test]
    fn a_reserved_handle_is_kept_for_the_pane_restoring_under_it() {
        // the ordering this exists for: a handle-less pane is built first, and must not be given
        // the name a later pane in the same layout is coming back under
        let wanted = reserved_for_tests("reserved");
        let reservation = Reservation::hold([wanted.clone()]);
        assert!(
            is_spoken_for(&registry(), &wanted),
            "the generator would hand out a reserved handle"
        );
        let restored = HeldHandle::claim(&wanted);
        assert_eq!(restored.as_str(), wanted);
        drop(reservation);
        // the reservation must not have freed the name the restored pane went on to claim
        assert_eq!(registry().get(&wanted), Some(&Claim::Held));
    }

    #[test]
    fn one_reservation_covers_every_tab_of_a_restore() {
        // the cross-tab ordering: a restore announces all its tabs before any tab's panes exist,
        // so the later tab's names must already be out of the generator's reach
        let first_tab = reserved_for_tests("tab-one");
        let last_tab = reserved_for_tests("tab-three");
        let mut reservation = Reservation::hold([first_tab.clone()]);
        reservation.extend([last_tab.clone()]);
        assert!(is_spoken_for(&registry(), &last_tab));
        // the pane built for tab 1 cannot be handed tab 3's name, so tab 3 gets it verbatim
        let restored = HeldHandle::claim(&last_tab);
        assert_eq!(restored.as_str(), last_tab);
        drop(reservation);
        assert_eq!(registry().get(&last_tab), Some(&Claim::Held));
        assert_eq!(
            registry().get(&first_tab),
            None,
            "never claimed, so released"
        );
    }

    #[test]
    fn an_unclaimed_reservation_is_released() {
        // a layout that never builds the pane must not leave the name spoken for forever
        let wanted = reserved_for_tests("unclaimed");
        drop(Reservation::hold([wanted.clone()]));
        assert_eq!(registry().get(&wanted), None);
    }
}
