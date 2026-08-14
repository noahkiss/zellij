//! The session's live pane handles.
//!
//! A handle is unique among the panes alive in one session. A zellij server process serves exactly
//! one session, so a process-global set IS the session's set - which is why this is a static
//! rather than something threaded through the three dozen places a pane gets built.
//!
//! Claims are RAII: a pane owns a [`HeldHandle`], and closing the pane frees the name for reuse.
//! Uniqueness is over LIVE panes only, deliberately. A handle is an address, and an address that
//! could never be reissued would drain the word lists over a long-lived session.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock};

use zellij_utils::pane_handle::generate_handle;

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

fn registry() -> MutexGuard<'static, HashMap<String, Claim>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, Claim>>> = OnceLock::new();
    REGISTRY
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Whether a name is out of the generator's reach: held by a live pane, or reserved for one.
///
/// Reserved counts, and that is the whole reservation mechanism - a name a layout is about to
/// restore a pane under must not be handed to a pane built in the meantime.
fn is_spoken_for(registry: &HashMap<String, Claim>, candidate: &str) -> bool {
    registry.contains_key(candidate)
}

/// A handle held by a live pane, freed when that pane drops.
#[derive(Debug)]
pub struct HeldHandle(String);

impl HeldHandle {
    /// Takes a handle no live pane answers to and no pending layout has reserved.
    pub fn claim_new() -> Self {
        let mut registry = registry();
        let handle = generate_handle(|candidate| is_spoken_for(&registry, candidate));
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
#[derive(Debug, Default)]
pub struct Reservation(Vec<String>);

impl Reservation {
    /// Reserves each handle that is not already held by a live pane.
    pub fn hold(handles: impl IntoIterator<Item = String>) -> Self {
        let mut registry = registry();
        let mut reserved = Vec::new();
        for handle in handles {
            if !registry.contains_key(handle.as_str()) {
                registry.insert(handle.clone(), Claim::Reserved);
                reserved.push(handle);
            }
        }
        Reservation(reserved)
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

    /// A name for a test to claim that no generated handle can ever collide with.
    ///
    /// The registry is process-global on purpose, so under `cargo test` every test in this module
    /// shares it with siblings that draw hundreds of random handles. A name from the word lists
    /// would be a rare-but-real flake; `claim` takes any string, so the tests use names the
    /// generator cannot produce.
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
    fn an_unclaimed_reservation_is_released() {
        // a layout that never builds the pane must not leave the name spoken for forever
        let wanted = reserved_for_tests("unclaimed");
        drop(Reservation::hold([wanted.clone()]));
        assert_eq!(registry().get(&wanted), None);
    }
}
