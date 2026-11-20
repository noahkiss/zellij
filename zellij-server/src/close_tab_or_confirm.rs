//! fork addition: the decision behind the tab-mode close key (`Action::CloseTabOrConfirm`).
//!
//! The server makes it for the client that pressed the key, before any pane exists. A tab worth
//! nothing closes at once, so no prompt pane is ever drawn for it. A tab with more than one
//! selectable pane launches `zellij:confirm-close-tab`, which asks.
//!
//! The rule is the plugin's own (`TabShape::decide` in `default-plugins/confirm-close-tab`): count
//! the selectable panes in both layers, the same counts its `TabInfo` carries, and ask only above
//! one. The plugin keeps the rule too, for a config that still launches it directly.

use crate::tab::Tab;
use zellij_utils::input::layout::RunPluginOrAlias;

/// The plugin that asks.
pub const PROMPT_PLUGIN: &str = "zellij:confirm-close-tab";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// One selectable pane at most: close the tab without asking.
    CloseTabNow,
    /// More than one: launch the prompt.
    Ask,
}

/// The panes the user would lose by closing the tab: selectable panes in both layers. Hidden
/// floating panes count, as they do in the `TabInfo` the plugin reads.
pub fn panes_at_stake(tab: &Tab) -> usize {
    tab.get_selectable_tiled_panes_count() + tab.get_selectable_floating_panes_count()
}

pub fn decide(panes_at_stake: usize) -> Decision {
    if panes_at_stake <= 1 {
        Decision::CloseTabNow
    } else {
        Decision::Ask
    }
}

/// The prompt, as the default keybinding used to launch it.
pub fn prompt_plugin() -> RunPluginOrAlias {
    RunPluginOrAlias::from_url(PROMPT_PLUGIN, &None, None, None)
        .expect("a zellij: location always parses")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_or_one_pane_closes_without_asking() {
        assert_eq!(decide(0), Decision::CloseTabNow);
        assert_eq!(decide(1), Decision::CloseTabNow);
    }

    #[test]
    fn two_or_more_panes_ask() {
        assert_eq!(decide(2), Decision::Ask);
        assert_eq!(decide(7), Decision::Ask);
    }

    #[test]
    fn the_prompt_is_the_bundled_plugin() {
        let plugin = prompt_plugin();
        let run_plugin = plugin.get_run_plugin().expect("a builtin, not an alias");
        assert_eq!(run_plugin.location.display(), PROMPT_PLUGIN);
    }
}
