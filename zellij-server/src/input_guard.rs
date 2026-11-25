//! fork addition: the input guard.
//!
//! When an agent's CLI or MCP call moves a client's focused pane, focused tab or input mode, that
//! client's input is held for `input_guard_ms` (500 ms unset), so the person's next keystrokes do
//! not land in a pane they never chose:
//!
//! - plain typing (characters, Enter, Backspace, paste: whatever resolves to writing to the pane
//!   with no keybinding) goes to the pane the person was in before the change, or with
//!   `input_guard_typing "drop"` nowhere. Typing that keeps coming keeps the guard up until the
//!   person pauses for `input_guard_ms`, never longer than [`TYPING_CAP`];
//! - a key bound to an action is dropped;
//! - a mouse press is dropped, and so is the release and the drag of that press - even after the
//!   guard ends. A release whose press passed always passes. The wheel passes.
//!
//! Fail open: a guard ends on the clock, computed from its start at every question - no timer has
//! to fire for it to end. It is also cleared when its client leaves, when that client's focus
//! moves by anything but an agent, and by `input_guard_ms 0`, which takes the old path entirely.
//!
//! The state is shared: the route thread asks it about keys and the mouse, and the screen thread
//! arms it and asks it about typing. Everything here is plain data with `now` passed in.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use zellij_utils::data::InputMode;
use zellij_utils::input::mouse::{MouseEvent, MouseEventType};

use crate::panes::PaneId;
use crate::ClientId;

/// `input_guard_ms` when the config does not say.
pub const DEFAULT_GUARD_MS: u64 = 500;
/// The most `input_guard_ms` is taken as.
pub const MAX_GUARD_MS: u64 = 10_000;
/// The longest typing keeps a guard up, counted from its start, when `input_guard_ms` is shorter.
pub const TYPING_CAP: Duration = Duration::from_secs(3);

/// `input_guard_typing`: where plain typing goes while a guard holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Typing {
    /// To the pane the person was in before the change, if it still exists.
    #[default]
    Previous,
    /// Nowhere.
    Drop,
}

impl Typing {
    /// The config's word, or the default for anything else (the config parser already warned).
    pub fn from_config(value: Option<&str>) -> Self {
        match value {
            Some("drop") => Typing::Drop,
            _ => Typing::Previous,
        }
    }
}

pub type SharedInputGuards = Arc<Mutex<InputGuards>>;

/// One client's guard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Guard {
    /// The pane the client was focused on before the first change of this guard.
    pub previous: Option<PaneId>,
    /// The pane the change focused. The frame marker goes here, and a focus anywhere else means
    /// the focus moved again by something that is not an agent.
    pub focused: Option<PaneId>,
    started: Instant,
    last_typed: Option<Instant>,
}

/// Where a client was looking: what a change is measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Spot {
    pub tab: Option<usize>,
    pub pane: Option<PaneId>,
    pub mode: Option<InputMode>,
}

/// What becomes of a piece of plain typing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypingVerdict {
    /// No guard: the normal path.
    Pass,
    /// To this pane, through the normal write path.
    ToPane(PaneId),
    /// Nowhere.
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Button {
    Left,
    Right,
    Middle,
}

fn buttons_of(event: &MouseEvent) -> Vec<Button> {
    let mut buttons = vec![];
    if event.left {
        buttons.push(Button::Left);
    }
    if event.right {
        buttons.push(Button::Right);
    }
    if event.middle {
        buttons.push(Button::Middle);
    }
    buttons
}

#[derive(Debug)]
pub struct InputGuards {
    guard_ms: u64,
    typing: Typing,
    guards: HashMap<ClientId, Guard>,
    /// Buttons whose press was dropped and whose release has not come yet, per client.
    dropped_presses: HashMap<ClientId, HashSet<Button>>,
    /// Friendly focus: `focus_wait_idle` and `focus_wait_max`, in milliseconds.
    wait_idle_ms: u64,
    wait_max_ms: u64,
    /// When each client's person last pressed a key, clicked or scrolled.
    last_input: HashMap<ClientId, Instant>,
    /// The focus change waiting for each client's person to pause.
    pending: HashMap<ClientId, Pending>,
    next_generation: u64,
}

/// An agent's focus change waiting for a client's person to pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pending {
    /// Which deferral this is; a later one for the same client replaces it.
    pub generation: u64,
    /// The pane the agent's CLI ran in, which keys its virtual focus.
    pub caller_pane: Option<u32>,
    /// The pane the change will focus, when that is known before it applies.
    pub target: Option<PaneId>,
    since: Instant,
}

/// What a waiting focus change should do now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingState {
    /// Replaced by a later change, or dropped with its client: do nothing.
    Gone,
    /// The person is still typing: keep waiting.
    Wait,
    /// The person paused, or the wait ran out: apply it.
    Apply,
}

impl Default for InputGuards {
    fn default() -> Self {
        InputGuards {
            guard_ms: DEFAULT_GUARD_MS,
            typing: Typing::default(),
            guards: HashMap::new(),
            dropped_presses: HashMap::new(),
            wait_idle_ms: zellij_utils::focus_wait::DEFAULT_IDLE_MS,
            wait_max_ms: zellij_utils::focus_wait::DEFAULT_MAX_MS,
            last_input: HashMap::new(),
            pending: HashMap::new(),
            next_generation: 0,
        }
    }
}

impl InputGuards {
    /// The config's `input_guard_ms` and `input_guard_typing`. Zero turns the guard off and
    /// clears every guard held.
    pub fn configure(&mut self, guard_ms: u64, typing: Typing) {
        self.guard_ms = guard_ms.min(MAX_GUARD_MS);
        self.typing = typing;
        if self.guard_ms == 0 {
            self.guards.clear();
            self.dropped_presses.clear();
        }
    }

    /// The config's `focus_wait_idle` and `focus_wait_max`, in milliseconds. Either at zero
    /// turns the wait off; a change already waiting then applies at its next look.
    pub fn configure_focus_wait(&mut self, idle_ms: u64, max_ms: u64) {
        self.wait_idle_ms = idle_ms;
        self.wait_max_ms = max_ms;
    }

    /// Whether an agent's focus change waits at all. Off, it applies at once: the old path.
    pub fn focus_waits(&self) -> bool {
        self.wait_idle_ms > 0 && self.wait_max_ms > 0
    }

    /// The person at `client_id` pressed a key, clicked or scrolled.
    pub fn note_person_input(&mut self, client_id: ClientId, now: Instant) {
        self.last_input.insert(client_id, now);
    }

    /// Whether an agent's focus change for `client_id` waits now: the wait is on and the person
    /// typed within `focus_wait_idle`.
    pub fn should_wait(&self, client_id: ClientId, now: Instant) -> bool {
        self.focus_waits()
            && self.last_input.get(&client_id).map_or(false, |last| {
                now.saturating_duration_since(*last) < Duration::from_millis(self.wait_idle_ms)
            })
    }

    /// Hold an agent's focus change for `client_id` until its person pauses. A change already
    /// waiting for that client is replaced. Returns the generation to ask [`Self::pending_state`]
    /// about.
    pub fn defer(
        &mut self,
        client_id: ClientId,
        caller_pane: Option<u32>,
        target: Option<PaneId>,
        now: Instant,
    ) -> u64 {
        self.next_generation += 1;
        self.pending.insert(
            client_id,
            Pending {
                generation: self.next_generation,
                caller_pane,
                target,
                since: now,
            },
        );
        self.next_generation
    }

    /// What the waiting change `generation` for `client_id` should do at `now`. `Apply` takes it
    /// off the list. Fail open: the wait always ends by `focus_wait_max`, and a wait turned off
    /// meanwhile applies at once.
    pub fn pending_state(
        &mut self,
        client_id: ClientId,
        generation: u64,
        now: Instant,
    ) -> PendingState {
        let Some(pending) = self.pending.get(&client_id) else {
            return PendingState::Gone;
        };
        if pending.generation != generation {
            return PendingState::Gone;
        }
        let waited = now.saturating_duration_since(pending.since);
        if !self.should_wait(client_id, now) || waited >= Duration::from_millis(self.wait_max_ms) {
            self.pending.remove(&client_id);
            return PendingState::Apply;
        }
        PendingState::Wait
    }

    /// The pane a held change will focus, learnt once its create said what it made.
    pub fn set_pending_target(&mut self, client_id: ClientId, generation: u64, target: PaneId) {
        if let Some(pending) = self.pending.get_mut(&client_id) {
            if pending.generation == generation {
                pending.target = Some(target);
            }
        }
    }

    /// Drop a held change that turned out to have nothing to focus.
    pub fn drop_pending(&mut self, client_id: ClientId, generation: u64) {
        if self
            .pending
            .get(&client_id)
            .map_or(false, |pending| pending.generation == generation)
        {
            self.pending.remove(&client_id);
        }
    }

    /// The virtual focus of the agent whose CLI runs in `caller_pane`: while a change it asked
    /// for waits, `Some(target)` - the pane its implicit "focused pane" writes go to, or `None`
    /// when that pane is not known until the change applies. `None` when nothing of it waits.
    pub fn virtual_focus(&self, caller_pane: u32) -> Option<Option<PaneId>> {
        self.pending
            .values()
            .filter(|pending| pending.caller_pane == Some(caller_pane))
            .max_by_key(|pending| pending.generation)
            .map(|pending| pending.target)
    }

    /// Whether the guard is on at all. Off, every caller takes the old path.
    pub fn enabled(&self) -> bool {
        self.guard_ms > 0
    }

    pub fn guard_ms(&self) -> u64 {
        self.guard_ms
    }

    fn window(&self) -> Duration {
        Duration::from_millis(self.guard_ms)
    }

    /// When a guard ends: `input_guard_ms` after its start or after the last typing it routed,
    /// whichever is later, and never past [`TYPING_CAP`] (or `input_guard_ms`, if longer) from its
    /// start.
    fn ends_at(&self, guard: &Guard) -> Instant {
        let window = self.window();
        let from = guard
            .last_typed
            .map_or(guard.started, |typed| typed.max(guard.started));
        let cap = guard.started + TYPING_CAP.max(window);
        (from + window).min(cap)
    }

    /// The guard a client is under at `now`, if any.
    pub fn active(&self, client_id: ClientId, now: Instant) -> Option<Guard> {
        if !self.enabled() {
            return None;
        }
        let guard = self.guards.get(&client_id)?;
        (now < self.ends_at(guard)).then_some(*guard)
    }

    /// Whether any client is under a guard at `now`.
    pub fn any_active(&self, now: Instant) -> bool {
        self.guards
            .keys()
            .any(|client_id| self.active(*client_id, now).is_some())
    }

    /// Every client under a guard at `now`, with the pane the change focused: where the frame
    /// marker goes. Ended guards are forgotten.
    pub fn markers(&mut self, now: Instant) -> HashMap<ClientId, PaneId> {
        let ended: Vec<ClientId> = self
            .guards
            .keys()
            .filter(|client_id| self.active(**client_id, now).is_none())
            .copied()
            .collect();
        for client_id in ended {
            self.guards.remove(&client_id);
        }
        self.guards
            .iter()
            .filter_map(|(client_id, guard)| Some((*client_id, guard.focused?)))
            .collect()
    }

    /// Start (or restart) a client's guard: an agent's call moved it from `previous` to
    /// `focused`. A guard already up keeps the pane the person was in before the first change.
    pub fn arm(
        &mut self,
        client_id: ClientId,
        previous: Option<PaneId>,
        focused: Option<PaneId>,
        now: Instant,
    ) {
        if !self.enabled() {
            return;
        }
        let previous = self
            .active(client_id, now)
            .map_or(previous, |guard| guard.previous);
        self.guards.insert(
            client_id,
            Guard {
                previous,
                focused,
                started: now,
                last_typed: None,
            },
        );
    }

    /// End a client's guard now.
    pub fn clear(&mut self, client_id: ClientId) {
        self.guards.remove(&client_id);
    }

    /// A client left: nothing of it is kept.
    pub fn forget_client(&mut self, client_id: ClientId) {
        self.guards.remove(&client_id);
        self.dropped_presses.remove(&client_id);
        self.last_input.remove(&client_id);
        self.pending.remove(&client_id);
    }

    /// The client's focus is now on `focused_now`. A guard whose pane that is not was overtaken
    /// by a move that is not an agent's, and ends.
    pub fn note_focus(&mut self, client_id: ClientId, focused_now: Option<PaneId>) {
        if let Some(guard) = self.guards.get(&client_id) {
            if guard.focused != focused_now {
                self.guards.remove(&client_id);
            }
        }
    }

    /// Whether a key bound to an action is dropped now.
    pub fn drops_bound_keys(&self, client_id: ClientId, now: Instant) -> bool {
        self.active(client_id, now).is_some()
    }

    /// Where a piece of the person's plain typing goes. `focused_now` is the pane the client is
    /// focused on as the typing is handled.
    pub fn typing(
        &mut self,
        client_id: ClientId,
        focused_now: Option<PaneId>,
        now: Instant,
    ) -> TypingVerdict {
        self.note_focus(client_id, focused_now);
        let Some(guard) = self.active(client_id, now) else {
            self.guards.remove(&client_id);
            return TypingVerdict::Pass;
        };
        if let Some(held) = self.guards.get_mut(&client_id) {
            held.last_typed = Some(now);
        }
        match (self.typing, guard.previous) {
            (Typing::Drop, _) => TypingVerdict::Drop,
            // only the mode changed: the pane the person types into is the one they chose
            (Typing::Previous, Some(previous)) if Some(previous) == guard.focused => {
                TypingVerdict::Pass
            },
            (Typing::Previous, Some(previous)) => TypingVerdict::ToPane(previous),
            (Typing::Previous, None) => TypingVerdict::Drop,
        }
    }

    /// Whether a mouse event from the person passes. A press while the guard holds is dropped,
    /// and so is everything of that press after it - its drag and its release - guard or no
    /// guard. A press with no guard passes, and forgets a dropped press of that button whose
    /// release never came. The wheel and a hover always pass.
    pub fn mouse_passes(&mut self, client_id: ClientId, event: &MouseEvent, now: Instant) -> bool {
        let buttons = buttons_of(event);
        if buttons.is_empty() {
            return true;
        }
        let held = self.active(client_id, now).is_some();
        let passes = match event.event_type {
            MouseEventType::Press => {
                let dropped = self.dropped_presses.entry(client_id).or_default();
                if held {
                    dropped.extend(buttons);
                    false
                } else {
                    for button in buttons {
                        dropped.remove(&button);
                    }
                    true
                }
            },
            MouseEventType::Release => match self.dropped_presses.get_mut(&client_id) {
                Some(dropped) if buttons.iter().any(|button| dropped.contains(button)) => {
                    for button in buttons {
                        dropped.remove(&button);
                    }
                    false
                },
                _ => true,
            },
            MouseEventType::Motion => !self
                .dropped_presses
                .get(&client_id)
                .map_or(false, |dropped| {
                    buttons.iter().any(|button| dropped.contains(button))
                }),
        };
        if self
            .dropped_presses
            .get(&client_id)
            .map_or(false, HashSet::is_empty)
        {
            self.dropped_presses.remove(&client_id);
        }
        passes
    }
}

/// The clients whose spot changed between two snapshots, each with the pane it was on before
/// and the pane it is on now. A client that was not there before (it attached meanwhile) is
/// not counted.
pub fn moved(
    before: &HashMap<ClientId, Spot>,
    after: &HashMap<ClientId, Spot>,
) -> Vec<(ClientId, Option<PaneId>, Option<PaneId>)> {
    let mut moved: Vec<_> = after
        .iter()
        .filter_map(|(client_id, now)| {
            let was = before.get(client_id)?;
            (was != now).then_some((*client_id, was.pane, now.pane))
        })
        .collect();
    moved.sort_by_key(|(client_id, _, _)| *client_id);
    moved
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::position::Position;

    const ME: ClientId = 1;
    const OLD: PaneId = PaneId::Terminal(1);
    const NEW: PaneId = PaneId::Terminal(2);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    fn armed(at: Instant) -> InputGuards {
        let mut guards = InputGuards::default();
        guards.arm(ME, Some(OLD), Some(NEW), at);
        guards
    }

    fn mouse(event_type: MouseEventType, left: bool, wheel_up: bool) -> MouseEvent {
        let mut event = MouseEvent::new();
        event.event_type = event_type;
        event.left = left;
        event.wheel_up = wheel_up;
        event.position = Position::new(1, 1);
        event
    }

    #[test]
    fn a_guard_routes_typing_to_the_previous_pane_and_ends_on_the_clock() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert!(guards.drops_bound_keys(ME, t0 + ms(100)));
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(100)),
            TypingVerdict::ToPane(OLD)
        );
        // no more typing: it ends input_guard_ms after the last
        assert!(guards.active(ME, t0 + ms(599)).is_some());
        assert!(guards.active(ME, t0 + ms(600)).is_none());
        assert!(!guards.drops_bound_keys(ME, t0 + ms(600)));
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(700)),
            TypingVerdict::Pass
        );
    }

    #[test]
    fn typing_that_keeps_coming_holds_the_guard_up_to_the_cap() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        let mut at = t0;
        // a key every 300 ms, past the cap
        while at < t0 + ms(4_000) {
            at += ms(300);
            let verdict = guards.typing(ME, Some(NEW), at);
            if at < t0 + TYPING_CAP {
                assert_eq!(verdict, TypingVerdict::ToPane(OLD), "{:?}", at - t0);
            } else {
                assert_eq!(verdict, TypingVerdict::Pass, "{:?}", at - t0);
            }
        }
    }

    #[test]
    fn a_pause_of_the_guard_window_ends_it() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(400)),
            TypingVerdict::ToPane(OLD)
        );
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(800)),
            TypingVerdict::ToPane(OLD)
        );
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(1_300)),
            TypingVerdict::Pass
        );
    }

    #[test]
    fn drop_mode_drops_typing() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.configure(500, Typing::Drop);
        guards.arm(ME, Some(OLD), Some(NEW), t0);
        assert_eq!(
            guards.typing(ME, Some(NEW), t0 + ms(10)),
            TypingVerdict::Drop
        );
    }

    #[test]
    fn a_mode_change_alone_lets_typing_through_but_holds_bound_keys() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.arm(ME, Some(OLD), Some(OLD), t0);
        assert!(guards.drops_bound_keys(ME, t0 + ms(10)));
        assert_eq!(
            guards.typing(ME, Some(OLD), t0 + ms(10)),
            TypingVerdict::Pass
        );
    }

    #[test]
    fn a_second_change_keeps_the_first_previous_pane() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        guards.arm(ME, Some(NEW), Some(PaneId::Terminal(3)), t0 + ms(100));
        assert_eq!(
            guards.typing(ME, Some(PaneId::Terminal(3)), t0 + ms(200)),
            TypingVerdict::ToPane(OLD)
        );
        // and the second change restarted the clock
        assert!(guards.active(ME, t0 + ms(650)).is_some());
    }

    #[test]
    fn a_focus_move_that_is_not_the_agents_ends_the_guard() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert_eq!(
            guards.typing(ME, Some(PaneId::Terminal(9)), t0 + ms(10)),
            TypingVerdict::Pass
        );
        assert!(guards.active(ME, t0 + ms(20)).is_none());
        let mut guards = armed(t0);
        guards.note_focus(ME, Some(OLD));
        assert!(!guards.drops_bound_keys(ME, t0 + ms(20)));
    }

    #[test]
    fn leaving_and_zero_clear_everything() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        guards.forget_client(ME);
        assert!(guards.active(ME, t0).is_none());
        let mut guards = armed(t0);
        assert!(!guards.mouse_passes(ME, &mouse(MouseEventType::Press, true, false), t0));
        guards.configure(0, Typing::Previous);
        assert!(!guards.enabled());
        assert!(guards.active(ME, t0).is_none());
        // a release after the guard was turned off is not held back either
        assert!(guards.mouse_passes(ME, &mouse(MouseEventType::Release, true, false), t0));
        // and nothing arms while it is off
        guards.arm(ME, Some(OLD), Some(NEW), t0);
        assert!(guards.active(ME, t0).is_none());
        assert_eq!(guards.typing(ME, Some(NEW), t0), TypingVerdict::Pass);
    }

    #[test]
    fn a_dropped_press_takes_its_drag_and_release_with_it_even_after_the_guard() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert!(!guards.mouse_passes(ME, &mouse(MouseEventType::Press, true, false), t0));
        assert!(!guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Motion, true, false),
            t0 + ms(900)
        ));
        assert!(!guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Release, true, false),
            t0 + ms(1_000)
        ));
        // and the next press is the person's own again
        assert!(guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Press, true, false),
            t0 + ms(1_100)
        ));
        assert!(guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Release, true, false),
            t0 + ms(1_200)
        ));
    }

    #[test]
    fn a_release_whose_press_passed_always_passes() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        assert!(guards.mouse_passes(ME, &mouse(MouseEventType::Press, true, false), t0));
        guards.arm(ME, Some(OLD), Some(NEW), t0 + ms(10));
        assert!(guards.mouse_passes(ME, &mouse(MouseEventType::Motion, true, false), t0 + ms(20)));
        assert!(guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Release, true, false),
            t0 + ms(30)
        ));
    }

    #[test]
    fn a_lost_release_is_forgotten_at_the_next_press() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert!(!guards.mouse_passes(ME, &mouse(MouseEventType::Press, true, false), t0));
        // the release never came; the guard ended; the next press passes, and so does its release
        assert!(guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Press, true, false),
            t0 + ms(5_000)
        ));
        assert!(guards.mouse_passes(
            ME,
            &mouse(MouseEventType::Release, true, false),
            t0 + ms(5_100)
        ));
    }

    #[test]
    fn the_wheel_and_a_hover_pass_under_a_guard() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert!(guards.mouse_passes(ME, &mouse(MouseEventType::Press, false, true), t0));
        assert!(guards.mouse_passes(ME, &mouse(MouseEventType::Motion, false, false), t0));
    }

    #[test]
    fn markers_name_the_focused_pane_until_the_guard_ends() {
        let t0 = Instant::now();
        let mut guards = armed(t0);
        assert_eq!(guards.markers(t0 + ms(10)).get(&ME), Some(&NEW));
        assert!(guards.any_active(t0 + ms(10)));
        assert!(guards.markers(t0 + ms(600)).is_empty());
        assert!(!guards.any_active(t0 + ms(600)));
    }

    #[test]
    fn a_focus_change_waits_only_while_the_person_types() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        // nobody typed: no wait
        assert!(!guards.should_wait(ME, t0));
        guards.note_person_input(ME, t0);
        assert!(guards.should_wait(ME, t0 + ms(1_999)));
        assert!(!guards.should_wait(ME, t0 + ms(2_000)));
    }

    #[test]
    fn a_waiting_change_applies_at_the_pause() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.note_person_input(ME, t0);
        let generation = guards.defer(ME, Some(5), Some(NEW), t0);
        assert_eq!(guards.virtual_focus(5), Some(Some(NEW)));
        assert_eq!(guards.virtual_focus(6), None);
        // typing goes on: it waits
        guards.note_person_input(ME, t0 + ms(1_500));
        assert_eq!(
            guards.pending_state(ME, generation, t0 + ms(3_000)),
            PendingState::Wait
        );
        assert_eq!(
            guards.pending_state(ME, generation, t0 + ms(3_500)),
            PendingState::Apply
        );
        assert_eq!(guards.virtual_focus(5), None, "applied: no virtual focus");
        assert_eq!(
            guards.pending_state(ME, generation, t0 + ms(3_600)),
            PendingState::Gone
        );
    }

    #[test]
    fn a_waiting_change_applies_after_focus_wait_max_however_much_the_person_types() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.configure_focus_wait(2_000, 10_000);
        let generation = guards.defer(ME, None, None, t0);
        let mut at = t0;
        let mut state = PendingState::Wait;
        while state == PendingState::Wait {
            at += ms(500);
            guards.note_person_input(ME, at);
            state = guards.pending_state(ME, generation, at);
            assert!(at <= t0 + ms(10_000), "never stuck");
        }
        assert_eq!(state, PendingState::Apply);
        assert_eq!(at, t0 + ms(10_000));
    }

    #[test]
    fn a_later_change_replaces_a_waiting_one_and_leaving_drops_it() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.note_person_input(ME, t0);
        let first = guards.defer(ME, Some(5), Some(OLD), t0);
        let second = guards.defer(ME, Some(5), Some(NEW), t0);
        assert_eq!(guards.pending_state(ME, first, t0), PendingState::Gone);
        assert_eq!(guards.virtual_focus(5), Some(Some(NEW)));
        guards.forget_client(ME);
        assert_eq!(guards.pending_state(ME, second, t0), PendingState::Gone);
        assert_eq!(guards.virtual_focus(5), None);
    }

    #[test]
    fn a_zero_wait_takes_the_old_path() {
        let t0 = Instant::now();
        let mut guards = InputGuards::default();
        guards.note_person_input(ME, t0);
        for (idle, max) in [(0, 300_000), (2_000, 0)] {
            guards.configure_focus_wait(idle, max);
            assert!(!guards.focus_waits());
            assert!(!guards.should_wait(ME, t0));
        }
        // and a change already waiting applies at its next look
        guards.configure_focus_wait(2_000, 300_000);
        let generation = guards.defer(ME, None, None, t0);
        guards.configure_focus_wait(0, 300_000);
        assert_eq!(
            guards.pending_state(ME, generation, t0),
            PendingState::Apply
        );
    }

    #[test]
    fn moved_names_clients_whose_pane_tab_or_mode_changed() {
        let spot = |tab, pane, mode| Spot {
            tab: Some(tab),
            pane: Some(pane),
            mode: Some(mode),
        };
        let before: HashMap<_, _> = [
            (1, spot(0, OLD, InputMode::Normal)),
            (2, spot(0, OLD, InputMode::Normal)),
            (3, spot(0, OLD, InputMode::Normal)),
        ]
        .into_iter()
        .collect();
        let after: HashMap<_, _> = [
            (1, spot(0, NEW, InputMode::Normal)),
            (2, spot(0, OLD, InputMode::Normal)),
            (3, spot(0, OLD, InputMode::Locked)),
            (4, spot(1, NEW, InputMode::Normal)),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            moved(&before, &after),
            vec![(1, Some(OLD), Some(NEW)), (3, Some(OLD), Some(OLD))]
        );
    }
}
