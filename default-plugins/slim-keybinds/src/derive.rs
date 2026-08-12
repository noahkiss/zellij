//! Turning the live keybind table into an ordered list of hints.
//!
//! THE MODEL, AND WHY THIS ONE
//!
//! Zellij's `status-bar` builds its hints from giant literal tables (`one_line_ui.rs:1337`) and
//! then looks each entry's key up by reverse-matching the action. `compact-bar` does the
//! opposite and better thing (`keybind_utils.rs:304`): an ordered list of PREDICATES over
//! `Action`, walked against `mode_info.get_keybinds_for_mode(mode)`. This is a port of the
//! second one.
//!
//! The property that buys: a hint exists only if a key is actually bound to it RIGHT NOW. Unbind
//! `Ctrl s` in config.kdl and the `scroll` hint disappears on its own, with no rebuild and no
//! list to keep in sync. A literal hint table cannot do that — it renders the hint whether or not
//! the key exists.
//!
//! Nothing in here knows about colour or width; it returns plain strings and `main.rs` draws them.

use crate::action_types::ActionType;
use std::collections::HashSet;
use zellij_tile::prelude::actions::Action;
use zellij_tile::prelude::*;

/// One rendered hint: the key(s) that trigger it and what it does.
pub struct Hint {
    pub keys: String,
    pub label: String,
}

/// Everything needed to draw a mode's hints.
pub struct Hints {
    /// A modifier shared by EVERY displayed key, drawn once on the left ("Ctrl") and stripped
    /// from the individual keys. `None` when the keys have no modifier in common — which is the
    /// case inside every mode except the base one, where the binds are bare letters.
    pub prefix: Option<String>,
    pub hints: Vec<Hint>,
}

/// Modifiers shared by every key in `keys`. Ported from `status-bar/src/main.rs:382`.
fn common_modifiers(keys: &[KeyWithModifier]) -> Vec<KeyModifier> {
    let Some((first, rest)) = keys.split_first() else {
        return vec![];
    };
    let mut common = first.key_modifiers.clone();
    for key in rest {
        common = common.intersection(&key.key_modifiers).cloned().collect();
    }
    common.into_iter().collect()
}

/// `Esc`, `Enter` and `Space` are bound in almost every mode as "go back", and showing them
/// crowds out hints that carry information. Dropped whenever the same action has another key;
/// kept when they are the ONLY way to trigger it, since a hint with no key is worse than a
/// noisy one. Same judgement as `first_line.rs:550`'s `to_char`.
fn is_go_back_key(key: &KeyWithModifier) -> bool {
    matches!(
        key.bare_key,
        BareKey::Esc | BareKey::Enter | BareKey::Char(' ')
    )
}

/// Walk `predicates` in order and emit one hint per matched action class.
///
/// For each predicate the FIRST bind whose first action matches wins the ordering slot, then
/// every key bound to the same `ActionType` is gathered into that one hint — which is how the
/// four `MoveFocus` directions become a single `hjkl` block rather than four hints.
fn collect(
    mode_info: &ModeInfo,
    mode: InputMode,
    predicates: &[fn(&Action) -> bool],
) -> Vec<(Vec<KeyWithModifier>, ActionType)> {
    let keybinds = mode_info.get_keybinds_for_mode(mode);
    let mut seen: HashSet<ActionType> = HashSet::new();
    let mut out = Vec::new();

    for predicate in predicates {
        let Some(action_type) = keybinds
            .iter()
            .filter_map(|(_key, actions)| actions.first())
            .find(|action| predicate(action))
            .map(ActionType::from_action)
        else {
            continue;
        };
        if !seen.insert(action_type.clone()) {
            continue;
        }

        let mut keys: Vec<KeyWithModifier> = keybinds
            .iter()
            .filter(|(_, actions)| {
                actions
                    .first()
                    .map(|a| ActionType::from_action(a) == action_type)
                    .unwrap_or(false)
            })
            .map(|(key, _)| key.clone())
            .collect();

        if keys.iter().any(|k| !is_go_back_key(k)) {
            keys.retain(|k| !is_go_back_key(k));
        }
        if !keys.is_empty() {
            out.push((keys, action_type));
        }
    }
    out
}

/// Render a key set as one compact string.
///
/// Ported from `compact-bar/src/keybind_utils.rs:85` minus the `<…>` brackets, which cost two
/// columns per hint and add nothing once the key is already coloured differently from the label.
/// The special-cased families are the ones where the keys read as a range rather than a list:
/// `hjkl`, `HJKL`, the arrows, `[]`, `+-` and page up/down.
fn group_keys(keys: &[KeyWithModifier]) -> String {
    let rendered: Vec<String> = keys.iter().map(|k| k.to_string()).collect();
    if rendered.len() == 1 {
        return rendered[0].clone();
    }

    let mut hjkl_lower = Vec::new();
    let mut hjkl_upper = Vec::new();
    let mut arrows = Vec::new();
    let mut brackets = Vec::new();
    let mut plus_minus = Vec::new();
    let mut pages = Vec::new();
    let mut other = Vec::new();

    for key in &rendered {
        match key.as_str() {
            "Left" | "←" => arrows.push("←"),
            "Down" | "↓" => arrows.push("↓"),
            "Up" | "↑" => arrows.push("↑"),
            "Right" | "→" => arrows.push("→"),
            "h" | "j" | "k" | "l" => hjkl_lower.push(key.as_str()),
            "H" | "J" | "K" | "L" => hjkl_upper.push(key.as_str()),
            "[" | "]" => brackets.push(key.as_str()),
            "+" | "-" | "=" => plus_minus.push(key.as_str()),
            "PgUp" | "PgDn" => pages.push(key.as_str()),
            _ => other.push(key.as_str()),
        }
    }

    // `=` is the unshifted `+` on most layouts, so both being bound is one key to the user.
    if plus_minus.contains(&"+") {
        plus_minus.retain(|k| *k != "=");
    }

    let order = |group: &mut Vec<&str>, canonical: &[&str]| {
        group.dedup();
        group.sort_by_key(|k| canonical.iter().position(|c| c == k).unwrap_or(usize::MAX));
    };
    order(&mut hjkl_lower, &["h", "j", "k", "l"]);
    order(&mut hjkl_upper, &["H", "J", "K", "L"]);
    arrows.sort();
    arrows.dedup();
    order(&mut arrows, &["←", "↓", "↑", "→"]);
    order(&mut brackets, &["[", "]"]);
    order(&mut plus_minus, &["+", "-"]);
    order(&mut pages, &["PgUp", "PgDn"]);

    let mut groups: Vec<String> = Vec::new();
    for group in [&hjkl_lower, &hjkl_upper, &arrows, &brackets, &plus_minus] {
        if !group.is_empty() {
            groups.push(group.join(""));
        }
    }
    if !pages.is_empty() {
        groups.push(pages.join("|"));
    }
    if !other.is_empty() {
        groups.push(other.join("/"));
    }
    groups.join("/")
}

/// The hints for `mode`, ready to draw.
pub fn hints_for_mode(mode_info: &ModeInfo, mode: InputMode) -> Hints {
    let collected = collect(mode_info, mode, predicates_for_mode(mode));

    // The shared modifier is intersected across every key we are about to SHOW, not across the
    // mode-switch binds specifically as `first_line.rs:507`'s `superkey` does. Same result in
    // the base mode (where every hint is a mode switch) and a better one elsewhere: a mode whose
    // binds all carry `Alt` gets the same one-off treatment for free.
    let all_keys: Vec<KeyWithModifier> = collected
        .iter()
        .flat_map(|(keys, _)| keys.iter().cloned())
        .collect();
    let common = common_modifiers(&all_keys);
    let prefix = (!common.is_empty()).then(|| {
        common
            .iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join("-")
    });

    let hints = collected
        .into_iter()
        .map(|(keys, action_type)| {
            let stripped: Vec<KeyWithModifier> = keys
                .iter()
                .map(|k| k.strip_common_modifiers(&common))
                .collect();
            Hint {
                keys: group_keys(&stripped),
                label: action_type.description(),
            }
        })
        .collect();

    Hints { prefix, hints }
}

/// Ordered predicates per mode, ported from `compact-bar/src/keybind_utils.rs:304`.
///
/// Typed as `fn` pointers rather than a generic `F: Fn(&Action) -> bool`: every closure has its
/// own type, so a `vec![]` of them only compiles by coercion, and being explicit about the
/// coercion is cheaper than debugging it later.
fn predicates_for_mode(mode: InputMode) -> &'static [fn(&Action) -> bool] {
    match mode {
        InputMode::Locked => &[|a| {
            matches!(
                a,
                Action::SwitchToMode {
                    input_mode: InputMode::Normal
                }
            )
        }],
        InputMode::Normal => &[
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Locked
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Pane
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Tab
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Resize
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Move
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Scroll
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::Session
                    }
                )
            },
            |a| matches!(a, Action::Quit),
        ],
        InputMode::Pane => &[
            |a| {
                matches!(
                    a,
                    Action::NewPane {
                        direction: None,
                        pane_name: None,
                        start_suppressed: false
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::MoveFocus {
                        direction: Direction::Left
                    }
                )
            },
            |a| matches!(a, Action::CloseFocus),
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::RenamePane
                    }
                )
            },
            |a| matches!(a, Action::ToggleFocusFullscreen),
            |a| matches!(a, Action::ToggleFloatingPanes),
            |a| matches!(a, Action::TogglePaneEmbedOrFloating),
            |a| matches!(a, Action::NewStackedPane { .. }),
            |a| {
                matches!(
                    a,
                    Action::NewPane {
                        direction: Some(_),
                        pane_name: None,
                        start_suppressed: false
                    }
                )
            },
        ],
        InputMode::Tab => &[
            |a| matches!(a, Action::GoToPreviousTab | Action::GoToNextTab),
            |a| matches!(a, Action::NewTab { .. }),
            |a| matches!(a, Action::CloseTab),
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::RenameTab
                    }
                )
            },
            |a| matches!(a, Action::ToggleActiveSyncTab),
            |a| matches!(a, Action::BreakPane),
            |a| matches!(a, Action::BreakPaneLeft | Action::BreakPaneRight),
            |a| matches!(a, Action::ToggleTab),
        ],
        InputMode::Resize => &[
            |a| {
                matches!(
                    a,
                    Action::Resize {
                        resize: Resize::Increase,
                        direction: None
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::Resize {
                        resize: Resize::Decrease,
                        direction: None
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::Resize {
                        resize: Resize::Increase,
                        direction: Some(_)
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::Resize {
                        resize: Resize::Decrease,
                        direction: Some(_)
                    }
                )
            },
        ],
        InputMode::Move => &[
            |a| matches!(a, Action::MovePane { direction: Some(_) }),
            |a| matches!(a, Action::MovePane { direction: None }),
        ],
        InputMode::Scroll => &[
            |a| matches!(a, Action::ScrollUp | Action::ScrollDown),
            |a| matches!(a, Action::HalfPageScrollUp | Action::HalfPageScrollDown),
            |a| matches!(a, Action::PageScrollUp | Action::PageScrollDown),
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::EnterSearch
                    }
                )
            },
            |a| matches!(a, Action::EditScrollback { .. }),
        ],
        InputMode::Search => &[
            |a| {
                matches!(
                    a,
                    Action::SwitchToMode {
                        input_mode: InputMode::EnterSearch
                    }
                )
            },
            |a| matches!(a, Action::Search { .. }),
            |a| matches!(a, Action::ScrollUp | Action::ScrollDown),
            |a| matches!(a, Action::PageScrollUp | Action::PageScrollDown),
            |a| matches!(a, Action::HalfPageScrollUp | Action::HalfPageScrollDown),
            |a| {
                matches!(
                    a,
                    Action::SearchToggleOption {
                        option: actions::SearchOption::CaseSensitivity
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SearchToggleOption {
                        option: actions::SearchOption::Wrap
                    }
                )
            },
            |a| {
                matches!(
                    a,
                    Action::SearchToggleOption {
                        option: actions::SearchOption::WholeWord
                    }
                )
            },
        ],
        InputMode::Session => &[
            |a| matches!(a, Action::Detach),
            |a| a.launches_plugin("session-manager"),
            |a| a.launches_plugin("plugin-manager"),
            |a| a.launches_plugin("configuration"),
            |a| a.launches_plugin("zellij:about"),
        ],
        // Text-entry modes: every printable key is bound to the input action, so there is
        // nothing meaningful to hint. Stock derives nothing here either.
        InputMode::EnterSearch
        | InputMode::RenameTab
        | InputMode::RenamePane
        | InputMode::Prompt
        | InputMode::Tmux => &[],
    }
}
