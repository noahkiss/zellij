//! Dwell tracking for `bell_clear_delay_ms`.
//!
//! A pane that rang the bell keeps its `[!]` until it has been focused, without
//! interruption, for the configured delay. The rule the tracker implements is:
//! clear the bell once the pane has been continuously focused for the delay,
//! counted from the later of (the focus, the last ring).
//!
//! The tracker only decides. `Screen` records focus and rings into it, schedules
//! a background job per event, and asks `may_clear` when that job fires. A delay
//! of 0 disables all of it - `Screen` clears the bell on focus as it always has.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::panes::PaneId;
use crate::ClientId;

#[derive(Debug, Default)]
pub struct BellDwellTracker {
    /// 0 means "clear the bell the moment the pane is focused" - the stock behaviour.
    delay: Duration,
    /// Per client: the pane it has focused, and since when.
    focused_since: HashMap<ClientId, (PaneId, Instant)>,
    /// Per pane: when it last rang while it already had a pending bell.
    last_ring: HashMap<PaneId, Instant>,
}

impl BellDwellTracker {
    pub fn set_delay_ms(&mut self, delay_ms: u64) {
        self.delay = Duration::from_millis(delay_ms);
    }
    pub fn delay_ms(&self) -> u64 {
        self.delay.as_millis() as u64
    }
    pub fn is_delayed(&self) -> bool {
        !self.delay.is_zero()
    }
    /// Records that `client_id` now has `pane_id` focused. Focusing the pane it
    /// already had focused does not restart the dwell.
    pub fn record_focus(&mut self, client_id: ClientId, pane_id: PaneId, now: Instant) {
        match self.focused_since.get(&client_id) {
            Some((focused_pane_id, _)) if *focused_pane_id == pane_id => {},
            _ => {
                self.focused_since.insert(client_id, (pane_id, now));
            },
        }
    }
    /// Records a ring on a pane that already shows a bell. The dwell restarts
    /// from here.
    pub fn record_ring(&mut self, pane_id: PaneId, now: Instant) {
        self.prune_rings(now);
        self.last_ring.insert(pane_id, now);
    }
    /// Whether the pending bell of `pane_id` may be cleared now: some client has
    /// had it focused for the whole delay, and it has not rung within it.
    pub fn may_clear(&self, pane_id: PaneId, now: Instant) -> bool {
        let dwelled = self.focused_since.values().any(|(focused_pane_id, since)| {
            *focused_pane_id == pane_id && now.duration_since(*since) >= self.delay
        });
        let quiet = self
            .last_ring
            .get(&pane_id)
            .map_or(true, |rang_at| now.duration_since(*rang_at) >= self.delay);
        dwelled && quiet
    }
    /// Forgets a pane the tracker no longer has to reason about.
    pub fn forget_pane(&mut self, pane_id: PaneId) {
        self.last_ring.remove(&pane_id);
    }
    /// Rings older than the delay can no longer hold a clear back, so they are
    /// dropped. Keeps the map bounded without a hook on pane close.
    fn prune_rings(&mut self, now: Instant) {
        let delay = self.delay;
        self.last_ring
            .retain(|_, rang_at| now.duration_since(*rang_at) < delay);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLIENT: ClientId = 1;
    const OTHER_CLIENT: ClientId = 2;
    const PANE: PaneId = PaneId::Terminal(7);
    const OTHER_PANE: PaneId = PaneId::Terminal(8);

    fn tracker(delay_ms: u64) -> BellDwellTracker {
        let mut tracker = BellDwellTracker::default();
        tracker.set_delay_ms(delay_ms);
        tracker
    }

    #[test]
    fn a_zero_delay_is_not_delayed() {
        assert!(!tracker(0).is_delayed());
        assert!(tracker(1000).is_delayed());
    }

    #[test]
    fn the_bell_survives_a_pane_focused_for_less_than_the_delay() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        assert!(!tracker.may_clear(PANE, focused_at + Duration::from_millis(999)));
    }

    #[test]
    fn the_bell_clears_once_the_pane_has_dwelled() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        assert!(tracker.may_clear(PANE, focused_at + Duration::from_millis(1000)));
    }

    #[test]
    fn focusing_away_before_the_dwell_keeps_the_bell() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        tracker.record_focus(CLIENT, OTHER_PANE, focused_at + Duration::from_millis(300));
        assert!(!tracker.may_clear(PANE, focused_at + Duration::from_millis(1000)));
    }

    #[test]
    fn refocusing_the_same_pane_does_not_restart_the_dwell() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        tracker.record_focus(CLIENT, PANE, focused_at + Duration::from_millis(900));
        assert!(tracker.may_clear(PANE, focused_at + Duration::from_millis(1000)));
    }

    #[test]
    fn a_ring_during_the_dwell_restarts_it() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        tracker.record_ring(PANE, focused_at + Duration::from_millis(500));
        assert!(!tracker.may_clear(PANE, focused_at + Duration::from_millis(1200)));
        assert!(tracker.may_clear(PANE, focused_at + Duration::from_millis(1500)));
    }

    #[test]
    fn any_client_that_dwelled_clears_the_bell() {
        let mut tracker = tracker(1000);
        let focused_at = Instant::now();
        tracker.record_focus(CLIENT, PANE, focused_at);
        tracker.record_focus(OTHER_CLIENT, PANE, focused_at + Duration::from_millis(900));
        tracker.record_focus(CLIENT, OTHER_PANE, focused_at + Duration::from_millis(950));
        assert!(!tracker.may_clear(PANE, focused_at + Duration::from_millis(1000)));
        assert!(tracker.may_clear(PANE, focused_at + Duration::from_millis(1900)));
    }

    #[test]
    fn an_unfocused_pane_never_clears() {
        let mut tracker = tracker(1000);
        let now = Instant::now();
        tracker.record_ring(PANE, now);
        assert!(!tracker.may_clear(PANE, now + Duration::from_millis(5000)));
    }

    #[test]
    fn stale_rings_are_pruned() {
        let mut tracker = tracker(1000);
        let now = Instant::now();
        tracker.record_ring(OTHER_PANE, now);
        tracker.record_ring(PANE, now + Duration::from_millis(2000));
        assert_eq!(tracker.last_ring.len(), 1);
        assert!(tracker.last_ring.contains_key(&PANE));
    }

    #[test]
    fn forgetting_a_pane_drops_its_ring() {
        let mut tracker = tracker(1000);
        let now = Instant::now();
        tracker.record_focus(CLIENT, PANE, now);
        tracker.record_ring(PANE, now);
        tracker.forget_pane(PANE);
        assert!(tracker.may_clear(PANE, now + Duration::from_millis(1000)));
    }
}
