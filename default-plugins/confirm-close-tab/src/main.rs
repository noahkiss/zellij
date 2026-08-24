//! A confirmation prompt for the interactive close-tab key.
//!
//! The tab-mode close key launches this plugin as a floating pane instead of sending `CloseTab`.
//! If the focused tab holds one selectable pane at most, the plugin closes the tab at once and
//! never renders. Otherwise it asks, and offers to close only the pane the user was in.
//!
//! The CLI path is untouched: `zellij action close-tab` confirms client-side, before the action
//! reaches the server, and never launches a plugin.

use std::collections::BTreeMap;
use zellij_tile::prelude::*;

/// The size the plugin asks for its own floating pane, in cells. One line of prompt plus the
/// pane frame.
const PANE_WIDTH: usize = 46;
const PANE_HEIGHT: usize = 3;

/// What the plugin does once it knows the shape of the focused tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    /// No focused tab or pane to act on. Close the plugin pane and change nothing else.
    Abort,
    /// One selectable pane at most, so the tab is worth nothing. Close it without asking.
    CloseTabNow,
    /// More than one selectable pane. Ask first.
    Ask { panes: usize },
}

/// The facts about the focused tab that the decision needs, however they were learned: the
/// synchronous queries in `load`, or a later `TabUpdate`/`PaneUpdate`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TabShape {
    /// The tab's position, which is what `PaneManifest` is keyed by. Not its stable id: those are
    /// different numbers, and only the position indexes a manifest.
    position: usize,
    /// Selectable panes in the tab, both layers, counting this plugin's own pane if
    /// `counts_own_pane` says the tab already holds it.
    selectable_panes: usize,
    counts_own_pane: bool,
}

impl TabShape {
    /// The panes the user would lose by closing this tab. The plugin's own pane is not one of
    /// them: it closes either way.
    fn panes_at_stake(&self) -> usize {
        self.selectable_panes
            .saturating_sub(usize::from(self.counts_own_pane))
    }

    fn decide(&self) -> Decision {
        match self.panes_at_stake() {
            0 | 1 => Decision::CloseTabNow,
            panes => Decision::Ask { panes },
        }
    }
}

/// The pane `p` closes: the one that had focus when the prompt opened.
///
/// It is read off the manifest rather than asked for, because by the time this plugin runs its own
/// pane is the focused one and `get_focused_pane_info` only ever names it. The tiled and floating
/// layers keep separate focus, so a tiled pane keeps `is_focused` while this floating pane holds
/// the floating layer's focus - which is also why a previously focused *floating* pane cannot be
/// recovered, and `p` falls back to the focused tiled pane.
fn previous_pane(own_plugin_id: u32, panes_in_tab: &[PaneInfo]) -> Option<PaneId> {
    let own_pane = PaneId::Plugin(own_plugin_id);
    let focused_in_layer = |is_floating: bool| {
        panes_in_tab
            .iter()
            .find(|pane| {
                pane.is_focused
                    && pane.is_selectable
                    && !pane.is_suppressed
                    && pane.is_floating == is_floating
                    && pane_id_of(pane) != own_pane
            })
            .map(pane_id_of)
    };
    focused_in_layer(false).or_else(|| focused_in_layer(true))
}

fn pane_id_of(pane: &PaneInfo) -> PaneId {
    if pane.is_plugin {
        PaneId::Plugin(pane.id)
    } else {
        PaneId::Terminal(pane.id)
    }
}

/// The selectable panes a manifest reports for one tab, and whether this plugin's own pane is
/// among them. Suppressed panes are excluded: the user cannot see them and does not think of them
/// as open.
fn count_selectable_panes(panes_in_tab: &[PaneInfo], own_plugin_id: u32) -> (usize, bool) {
    let own_pane = PaneId::Plugin(own_plugin_id);
    let selectable = panes_in_tab
        .iter()
        .filter(|pane| pane.is_selectable && !pane.is_suppressed);
    let mut count = 0;
    let mut counts_own_pane = false;
    for pane in selectable {
        count += 1;
        if pane_id_of(pane) == own_pane {
            counts_own_pane = true;
        }
    }
    (count, counts_own_pane)
}

/// The prompt line, plus the offsets the caller colours: the pane count, and each answer key.
fn prompt_line(
    panes: usize,
    offer_previous_pane: bool,
) -> (String, std::ops::Range<usize>, Vec<usize>) {
    let mut line = String::from("close tab? ");
    let count_start = line.chars().count();
    line.push_str(&panes.to_string());
    let count_range = count_start..line.chars().count();
    line.push_str(" panes open  [");
    let mut key_indices = Vec::new();
    key_indices.push(line.chars().count());
    line.push('y');
    line.push('/');
    key_indices.push(line.chars().count());
    line.push('N');
    if offer_previous_pane {
        line.push('/');
        key_indices.push(line.chars().count());
        line.push('p');
    }
    line.push(']');
    (line, count_range, key_indices)
}

#[derive(Default)]
struct State {
    own_plugin_id: u32,
    tab: Option<TabShape>,
    /// The pane `p` closes, resolved from the first manifest.
    previous_pane: Option<PaneId>,
    /// Whether a manifest has arrived. The prompt waits for one, so the first frame the user sees
    /// already knows whether it can offer `p`.
    saw_manifest: bool,
    /// Set once the prompt has been on screen. After that an event may refresh the count but may
    /// no longer close anything on its own - only a keypress does.
    has_prompted: bool,
    /// Set once the plugin has acted, so a late event cannot act twice.
    done: bool,
}

register_plugin!(State);

impl ZellijPlugin for State {
    fn load(&mut self, _configuration: BTreeMap<String, String>) {
        self.own_plugin_id = get_plugin_ids().plugin_id;
        subscribe(&[EventType::Key, EventType::PaneUpdate, EventType::TabUpdate]);
        let Ok((tab_id, focused_pane_id)) = get_focused_pane_info() else {
            // This client has no focused tab or pane, so there is no tab to confirm closing.
            self.act(Decision::Abort);
            return;
        };
        // By the time this runs the plugin's own pane is already in the tab and already focused,
        // so it is already one of the tab's selectable panes. That makes the count exact here, and
        // a tab worth nothing can be closed before a frame is ever drawn.
        let counts_own_pane = focused_pane_id == PaneId::Plugin(self.own_plugin_id);
        let Some(tab_info) = get_tab_info(tab_id) else {
            return; // decide on the first TabUpdate instead
        };
        let shape = TabShape {
            position: tab_info.position,
            selectable_panes: tab_info.selectable_tiled_panes_count
                + tab_info.selectable_floating_panes_count,
            counts_own_pane,
        };
        self.tab = Some(shape);
        if shape.decide() == Decision::CloseTabNow {
            self.act(Decision::CloseTabNow);
        }
    }

    fn update(&mut self, event: Event) -> bool {
        if self.done {
            return false;
        }
        match event {
            Event::TabUpdate(tabs) => self.handle_tab_update(tabs),
            Event::PaneUpdate(manifest) => self.handle_pane_update(manifest),
            Event::Key(key) => self.handle_key(key),
            _ => false,
        }
    }

    fn render(&mut self, rows: usize, columns: usize) {
        let Some(shape) = self.tab.filter(|_| self.saw_manifest) else {
            return;
        };
        let Decision::Ask { panes } = shape.decide() else {
            return;
        };
        if rows == 0 || columns == 0 {
            return;
        }
        if !self.has_prompted {
            self.size_self();
            self.has_prompted = true;
        }
        let (line, count_range, key_indices) = prompt_line(panes, self.previous_pane.is_some());
        let x = columns.saturating_sub(line.chars().count()) / 2;
        let y = rows / 2;
        print_text_with_coordinates(
            Text::new(line)
                .color_range(0, count_range)
                .color_indices(2, key_indices),
            x,
            y,
            None,
            None,
        );
    }
}

impl State {
    /// Adopt the newest view of the focused tab, and take the fast path if the tab turns out to be
    /// worth nothing. Once the prompt has been on screen only a keypress closes anything, so a
    /// pane closed elsewhere cannot pull the tab out from under the question.
    fn adopt(&mut self, shape: TabShape) -> bool {
        let changed = self.tab != Some(shape);
        self.tab = Some(shape);
        if !self.has_prompted && shape.decide() == Decision::CloseTabNow {
            self.act(Decision::CloseTabNow);
            return false;
        }
        changed
    }

    fn handle_tab_update(&mut self, tabs: Vec<TabInfo>) -> bool {
        let Some(tab_info) = tabs.into_iter().find(|tab| tab.active) else {
            return false;
        };
        // A manifest counts this plugin's own pane exactly; TabInfo cannot, so keep whatever the
        // manifest last established rather than guessing again.
        let counts_own_pane = self.tab.map(|shape| shape.counts_own_pane).unwrap_or(false);
        let shape = TabShape {
            position: tab_info.position,
            selectable_panes: tab_info.selectable_tiled_panes_count
                + tab_info.selectable_floating_panes_count,
            counts_own_pane,
        };
        self.adopt(shape)
    }

    fn handle_pane_update(&mut self, manifest: PaneManifest) -> bool {
        let Some(shape) = self.tab else {
            return false;
        };
        let Some(panes_in_tab) = manifest.panes.get(&shape.position) else {
            return false;
        };
        self.saw_manifest = true;
        self.previous_pane = previous_pane(self.own_plugin_id, panes_in_tab);
        let (selectable_panes, counts_own_pane) =
            count_selectable_panes(panes_in_tab, self.own_plugin_id);
        self.adopt(TabShape {
            selectable_panes,
            counts_own_pane,
            ..shape
        })
    }

    fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        if !self.has_prompted {
            return false;
        }
        match key.bare_key {
            BareKey::Char('y') if key.has_no_modifiers() => {
                self.close_tab();
            },
            BareKey::Char('p') if key.has_no_modifiers() => {
                self.close_previous_pane();
            },
            BareKey::Char('n') | BareKey::Esc | BareKey::Enter if key.has_no_modifiers() => {
                self.cancel();
            },
            BareKey::Char('c') if key.has_modifiers(&[KeyModifier::Ctrl]) => {
                self.cancel();
            },
            _ => {},
        }
        false
    }

    fn act(&mut self, decision: Decision) {
        if self.done {
            return;
        }
        match decision {
            Decision::Abort => self.cancel(),
            Decision::CloseTabNow => self.close_tab(),
            Decision::Ask { .. } => {},
        }
    }

    fn close_tab(&mut self) {
        self.done = true;
        // The plugin's own pane is in this tab, so it dies with it. Nothing to close afterwards.
        close_focused_tab();
    }

    fn close_previous_pane(&mut self) {
        let Some(pane_id) = self.previous_pane else {
            // Nothing to aim at, so this is a cancel rather than a silent no-op that leaves the
            // prompt up.
            self.cancel();
            return;
        };
        self.done = true;
        match pane_id {
            PaneId::Terminal(id) => close_terminal_pane(id),
            PaneId::Plugin(id) => close_plugin_pane(id),
        }
        close_self();
    }

    /// Closing the prompt's pane is the whole of the cleanup. Zellij hides a tab's floating layer
    /// again by itself when the last floating pane in it closes, so a tab that had none is left
    /// exactly as it was found. A tab that already had floating panes AND had them hidden is the
    /// one case this cannot restore: by the time the plugin runs, the layer is already unhidden
    /// and nothing tells it what the state was before.
    fn cancel(&mut self) {
        self.done = true;
        close_self();
    }

    /// Ask for a pane just big enough for the prompt. `LaunchOrFocusPlugin` carries no geometry,
    /// so the plugin sizes itself.
    fn size_self(&self) {
        change_floating_panes_coordinates(vec![(
            PaneId::Plugin(self.own_plugin_id),
            FloatingPaneCoordinates::default()
                .with_width_fixed(PANE_WIDTH)
                .with_height_fixed(PANE_HEIGHT),
        )]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(selectable_panes: usize, counts_own_pane: bool) -> TabShape {
        TabShape {
            position: 0,
            selectable_panes,
            counts_own_pane,
        }
    }

    fn pane(id: u32, is_plugin: bool, is_floating: bool, is_focused: bool) -> PaneInfo {
        PaneInfo {
            id,
            is_plugin,
            is_focused,
            is_floating,
            is_selectable: true,
            ..Default::default()
        }
    }

    #[test]
    fn a_tab_holding_only_the_prompt_closes_without_asking() {
        assert_eq!(shape(1, true).decide(), Decision::CloseTabNow);
    }

    #[test]
    fn a_tab_holding_one_pane_besides_the_prompt_closes_without_asking() {
        assert_eq!(shape(2, true).decide(), Decision::CloseTabNow);
        assert_eq!(shape(1, false).decide(), Decision::CloseTabNow);
    }

    #[test]
    fn a_tab_holding_two_panes_besides_the_prompt_asks() {
        assert_eq!(shape(3, true).decide(), Decision::Ask { panes: 2 });
        assert_eq!(shape(2, false).decide(), Decision::Ask { panes: 2 });
    }

    #[test]
    fn the_count_never_includes_the_prompts_own_pane() {
        assert_eq!(shape(4, true).panes_at_stake(), 3);
        assert_eq!(shape(4, false).panes_at_stake(), 4);
    }

    #[test]
    fn our_own_pane_is_never_the_previous_pane() {
        let panes = vec![pane(1, false, false, true), pane(7, true, true, true)];
        assert_eq!(previous_pane(7, &panes), Some(PaneId::Terminal(1)));
    }

    #[test]
    fn a_focused_tiled_pane_beats_a_focused_floating_one() {
        let panes = vec![
            pane(5, false, true, true),
            pane(1, false, false, true),
            pane(7, true, true, true),
        ];
        assert_eq!(previous_pane(7, &panes), Some(PaneId::Terminal(1)));
    }

    #[test]
    fn a_focused_floating_pane_is_used_when_no_tiled_pane_is_focused() {
        let panes = vec![pane(5, false, true, true), pane(7, true, true, true)];
        assert_eq!(previous_pane(7, &panes), Some(PaneId::Terminal(5)));
    }

    #[test]
    fn there_may_be_no_previous_pane_at_all() {
        let panes = vec![pane(7, true, true, true)];
        assert_eq!(previous_pane(7, &panes), None);
    }

    #[test]
    fn unselectable_and_suppressed_panes_are_not_panes_the_user_would_lose() {
        let mut status_bar = pane(2, true, false, false);
        status_bar.is_selectable = false;
        let mut suppressed = pane(3, false, false, false);
        suppressed.is_suppressed = true;
        let panes = vec![
            pane(1, false, false, true),
            status_bar,
            suppressed,
            pane(7, true, true, true),
        ];
        assert_eq!(count_selectable_panes(&panes, 7), (2, true));
    }

    #[test]
    fn the_prompt_names_the_count_and_the_answers() {
        let (line, count_range, key_indices) = prompt_line(3, true);
        assert_eq!(line, "close tab? 3 panes open  [y/N/p]");
        let chars: Vec<char> = line.chars().collect();
        assert_eq!(&chars[count_range], &['3']);
        let keys: Vec<char> = key_indices.iter().map(|i| chars[*i]).collect();
        assert_eq!(keys, vec!['y', 'N', 'p']);
    }

    #[test]
    fn the_prompt_offers_p_only_when_there_is_a_pane_to_close() {
        let (line, _, key_indices) = prompt_line(12, false);
        assert_eq!(line, "close tab? 12 panes open  [y/N]");
        let chars: Vec<char> = line.chars().collect();
        let keys: Vec<char> = key_indices.iter().map(|i| chars[*i]).collect();
        assert_eq!(keys, vec!['y', 'N']);
    }
}
