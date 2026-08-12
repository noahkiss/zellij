//! Action classification, ported from zellij's `default-plugins/compact-bar/src/action_types.rs`
//! (v0.44.3) with the label wording shortened for a one-line bar.
//!
//! The point of this type is DEDUPLICATION, not naming: several keys bind to the "same" thing
//! from a hint's point of view (`MoveFocus{Left}` .. `MoveFocus{Right}` are one hint with four
//! keys), and the only way to collapse them is to project `Action` onto a coarser enum first.
//! `description()` then names the class exactly once.

use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::*;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActionType {
    MoveFocus,
    MovePaneWithDirection,
    MovePaneWithoutDirection,
    ResizeIncrease,
    ResizeDecrease,
    ResizeAny,
    Search,
    SearchInput,
    SearchToggleCaseSensitivity,
    SearchToggleWrap,
    SearchToggleWholeWord,
    NewPaneWithDirection,
    NewPaneWithoutDirection,
    NewStackedPane,
    BreakPaneLeftOrRight,
    GoToAdjacentTab,
    Scroll,
    PageScroll,
    HalfPageScroll,
    SessionManager,
    Configuration,
    PluginManager,
    About,
    SwitchToMode(InputMode),
    TogglePaneEmbedOrFloating,
    ToggleFocusFullscreen,
    ToggleFloatingPanes,
    CloseFocus,
    CloseTab,
    ToggleActiveSyncTab,
    ToggleTab,
    BreakPane,
    EditScrollback,
    NewTab,
    TabNameInput,
    Detach,
    Quit,
    Other,
}

impl ActionType {
    /// Human label for the hint.
    ///
    /// Deliberately shorter than upstream's ("Split right" vs "Split right/down", "Fullscreen"
    /// vs "Toggle fullscreen"): every column here is one the user does not get to spend on a
    /// hint further right. Rewordings the user disagrees with are a `rename` config entry away.
    pub fn description(&self) -> String {
        match self {
            ActionType::MoveFocus => "focus".to_string(),
            ActionType::MovePaneWithDirection | ActionType::MovePaneWithoutDirection => {
                "move".to_string()
            },
            ActionType::ResizeIncrease => "grow".to_string(),
            ActionType::ResizeDecrease => "shrink".to_string(),
            ActionType::ResizeAny => "resize".to_string(),
            ActionType::Search => "next".to_string(),
            ActionType::SearchInput => "type".to_string(),
            ActionType::SearchToggleCaseSensitivity => "case".to_string(),
            ActionType::SearchToggleWrap => "wrap".to_string(),
            ActionType::SearchToggleWholeWord => "word".to_string(),
            ActionType::NewPaneWithDirection => "split".to_string(),
            ActionType::NewPaneWithoutDirection => "new".to_string(),
            ActionType::NewStackedPane => "stack".to_string(),
            ActionType::BreakPaneLeftOrRight => "to tab".to_string(),
            ActionType::GoToAdjacentTab => "focus".to_string(),
            ActionType::Scroll => "scroll".to_string(),
            ActionType::PageScroll => "page".to_string(),
            ActionType::HalfPageScroll => "half".to_string(),
            ActionType::SessionManager => "session-manager".to_string(),
            ActionType::PluginManager => "plugin-manager".to_string(),
            ActionType::Configuration => "config".to_string(),
            ActionType::About => "about".to_string(),
            ActionType::SwitchToMode(InputMode::RenamePane) => "rename".to_string(),
            ActionType::SwitchToMode(InputMode::RenameTab) => "rename".to_string(),
            ActionType::SwitchToMode(InputMode::EnterSearch) => "search".to_string(),
            ActionType::SwitchToMode(InputMode::Locked) => "lock".to_string(),
            ActionType::SwitchToMode(InputMode::Normal) => "unlock".to_string(),
            // Mode entry from the base mode: `pane`, `tab`, `resize`, `move`, `scroll`,
            // `session`. `Debug` is the mode's own name, which is exactly the label we want.
            ActionType::SwitchToMode(input_mode) => format!("{:?}", input_mode).to_lowercase(),
            ActionType::TogglePaneEmbedOrFloating => "embed".to_string(),
            ActionType::ToggleFocusFullscreen => "fullscreen".to_string(),
            ActionType::ToggleFloatingPanes => "floating".to_string(),
            ActionType::CloseFocus => "close".to_string(),
            ActionType::CloseTab => "close".to_string(),
            ActionType::ToggleActiveSyncTab => "sync".to_string(),
            ActionType::ToggleTab => "recent".to_string(),
            ActionType::BreakPane => "to new tab".to_string(),
            ActionType::EditScrollback => "edit".to_string(),
            ActionType::NewTab => "new".to_string(),
            ActionType::TabNameInput => "name".to_string(),
            ActionType::Detach => "detach".to_string(),
            ActionType::Quit => "quit".to_string(),
            ActionType::Other => "other".to_string(),
        }
    }

    pub fn from_action(action: &Action) -> Self {
        match action {
            Action::MoveFocus { .. } => ActionType::MoveFocus,
            Action::MovePane { direction: Some(_) } => ActionType::MovePaneWithDirection,
            Action::MovePane { direction: None } => ActionType::MovePaneWithoutDirection,
            Action::Resize {
                resize: Resize::Increase,
                direction: Some(_),
            } => ActionType::ResizeIncrease,
            Action::Resize {
                resize: Resize::Decrease,
                direction: Some(_),
            } => ActionType::ResizeDecrease,
            Action::Resize {
                direction: None, ..
            } => ActionType::ResizeAny,
            Action::Search { .. } => ActionType::Search,
            Action::SearchInput { .. } => ActionType::SearchInput,
            Action::SearchToggleOption {
                option: actions::SearchOption::CaseSensitivity,
            } => ActionType::SearchToggleCaseSensitivity,
            Action::SearchToggleOption {
                option: actions::SearchOption::Wrap,
            } => ActionType::SearchToggleWrap,
            Action::SearchToggleOption {
                option: actions::SearchOption::WholeWord,
            } => ActionType::SearchToggleWholeWord,
            Action::NewPane {
                direction: Some(_), ..
            } => ActionType::NewPaneWithDirection,
            Action::NewPane {
                direction: None, ..
            } => ActionType::NewPaneWithoutDirection,
            Action::NewStackedPane { .. } => ActionType::NewStackedPane,
            Action::BreakPaneLeft | Action::BreakPaneRight => ActionType::BreakPaneLeftOrRight,
            Action::GoToPreviousTab | Action::GoToNextTab => ActionType::GoToAdjacentTab,
            Action::ScrollUp | Action::ScrollDown => ActionType::Scroll,
            Action::PageScrollUp | Action::PageScrollDown => ActionType::PageScroll,
            Action::HalfPageScrollUp | Action::HalfPageScrollDown => ActionType::HalfPageScroll,
            Action::SwitchToMode { input_mode } => ActionType::SwitchToMode(*input_mode),
            Action::TogglePaneEmbedOrFloating => ActionType::TogglePaneEmbedOrFloating,
            Action::ToggleFocusFullscreen => ActionType::ToggleFocusFullscreen,
            Action::ToggleFloatingPanes => ActionType::ToggleFloatingPanes,
            Action::CloseFocus => ActionType::CloseFocus,
            Action::CloseTab => ActionType::CloseTab,
            Action::ToggleActiveSyncTab => ActionType::ToggleActiveSyncTab,
            Action::ToggleTab => ActionType::ToggleTab,
            Action::BreakPane => ActionType::BreakPane,
            Action::EditScrollback { .. } => ActionType::EditScrollback,
            Action::NewTab { .. } => ActionType::NewTab,
            Action::TabNameInput { .. } => ActionType::TabNameInput,
            Action::Detach => ActionType::Detach,
            Action::Quit => ActionType::Quit,
            // These are all `LaunchOrFocusPlugin` with different URLs, so they can only be told
            // apart by asking the action which plugin it launches.
            action if action.launches_plugin("session-manager") => ActionType::SessionManager,
            action if action.launches_plugin("configuration") => ActionType::Configuration,
            action if action.launches_plugin("plugin-manager") => ActionType::PluginManager,
            action if action.launches_plugin("zellij:about") => ActionType::About,
            _ => ActionType::Other,
        }
    }
}
