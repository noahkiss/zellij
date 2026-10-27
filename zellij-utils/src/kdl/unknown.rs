//! What the config parser does with a name or a value this build does not know.
//!
//! One `config.kdl` is shared by machines on different builds, so a config written for a newer
//! build has to load on an older one. Every surface of the config parser follows one rule:
//!
//! - An unknown NAME (a node, a mode, an action, a key) or an unknown enum VALUE is noted here and
//!   skipped. What is skipped is the smallest unit that cannot be kept in part: one key of a bind,
//!   a whole bind with an unknown action, one option (which then keeps its default).
//! - A malformed KNOWN thing is still an error: the wrong type for a known key, bad syntax, a known
//!   action with the wrong arguments.
//!
//! A note carries the byte offset of the node it is about. [`collect`] gathers the notes made while
//! one document parses, and the caller turns each offset into a line of that document. A note made
//! with no collector running - a plugin rebinding keys from a string, say - goes to the log.

use std::cell::RefCell;

thread_local! {
    // A stack, so that one document parsed while another is parsing keeps its own notes.
    static COLLECTORS: RefCell<Vec<Vec<(usize, String)>>> = RefCell::new(Vec::new());
}

/// Note that the node at `offset` names something this build does not know, and was skipped.
///
/// `message` names the thing and what was skipped; the caller of [`collect`] adds the file and
/// line.
pub fn note_unknown(offset: usize, message: String) {
    let unheard = COLLECTORS.with(|collectors| match collectors.borrow_mut().last_mut() {
        Some(notes) => {
            notes.push((offset, message));
            None
        },
        None => Some(message),
    });
    if let Some(message) = unheard {
        log::warn!("{}", message);
    }
}

/// Run `parse`, and return what it returned with every note it made, as (offset, message).
pub fn collect<R>(parse: impl FnOnce() -> R) -> (R, Vec<(usize, String)>) {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            COLLECTORS.with(|collectors| {
                collectors.borrow_mut().pop();
            });
        }
    }
    COLLECTORS.with(|collectors| collectors.borrow_mut().push(Vec::new()));
    let pop = Pop;
    let result = parse();
    let notes = COLLECTORS.with(|collectors| {
        collectors
            .borrow_mut()
            .last_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    });
    drop(pop);
    (result, notes)
}

/// The 1-based line of `source` that byte `offset` falls on.
pub fn line_of(source: &str, offset: usize) -> usize {
    let end = offset.min(source.len());
    source.as_bytes()[..end]
        .iter()
        .filter(|byte| **byte == b'\n')
        .count()
        + 1
}

/// Each note as `<source>:<line>: <message>`, or `line <line>: <message>` when the document has
/// no name. A note made twice for one node - a `shared_except` block is parsed once per mode it
/// binds - is kept once. They are in the order of the file, not of the parse.
pub fn render(
    source: &str,
    source_name: Option<&str>,
    mut notes: Vec<(usize, String)>,
) -> Vec<String> {
    notes.sort_by_key(|(offset, _)| *offset);
    let mut seen = std::collections::HashSet::new();
    notes
        .into_iter()
        .filter(|note| seen.insert(note.clone()))
        .map(|(offset, message)| {
            let line = line_of(source, offset);
            match source_name {
                Some(name) => format!("{}:{}: {}", name, line, message),
                None => format!("line {}: {}", line, message),
            }
        })
        .collect()
}

#[test]
fn a_note_made_inside_a_nested_collector_stays_in_that_collector() {
    let (((), inner), outer) = collect(|| {
        note_unknown(1, "outer".to_owned());
        collect(|| note_unknown(2, "inner".to_owned()))
    });
    assert_eq!(inner, vec![(2, "inner".to_owned())]);
    assert_eq!(outer, vec![(1, "outer".to_owned())]);
}

#[test]
fn line_of_counts_from_one() {
    assert_eq!(line_of("a\nb\nc", 0), 1);
    assert_eq!(line_of("a\nb\nc", 2), 2);
    assert_eq!(line_of("a\nb\nc", 4), 3);
    assert_eq!(line_of("a", 99), 1);
}

#[cfg(test)]
mod config_tests {
    use crate::data::{BareKey, InputMode, KeyWithModifier, PermissionType};
    use crate::input::actions::Action;
    use crate::input::config::Config;

    fn key(c: char) -> KeyWithModifier {
        KeyWithModifier::new(BareKey::Char(c))
    }

    fn bound(config: &Config, mode: InputMode, key: &KeyWithModifier) -> Option<Vec<Action>> {
        config
            .keybinds
            .0
            .get(&mode)
            .and_then(|binds| binds.get(key))
            .cloned()
    }

    fn warnings_of(kdl: &str) -> (Config, Vec<String>) {
        let config = Config::from_kdl(kdl, None).unwrap();
        let warnings = config.warnings.clone();
        (config, warnings)
    }

    /// The case that broke a shared config on nkmk.35: an action a newer build added.
    #[test]
    fn a_bind_with_an_unknown_action_is_skipped_whole_and_the_rest_loads() {
        let (config, warnings) = warnings_of(
            r#"keybinds clear-defaults=true {
    pane {
        bind "z" { ActionFromALaterBuild; SwitchToMode "Normal"; }
        bind "y" { SwitchToMode "Normal"; }
    }
}"#,
        );
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
        assert!(warnings[0].starts_with("line 3: "), "{}", warnings[0]);
        assert!(
            warnings[0].contains("ActionFromALaterBuild"),
            "{}",
            warnings[0]
        );
        assert!(warnings[0].contains("\"z\""), "{}", warnings[0]);
        // a partial action list would do something else, so nothing is bound
        assert_eq!(bound(&config, InputMode::Pane, &key('z')), None);
        assert_eq!(
            bound(&config, InputMode::Pane, &key('y')),
            Some(vec![Action::SwitchToMode {
                input_mode: InputMode::Normal
            }])
        );
    }

    #[test]
    fn a_switch_to_an_unknown_mode_skips_the_bind() {
        let (config, warnings) = warnings_of(
            r#"keybinds clear-defaults=true {
    normal {
        bind "z" { SwitchToMode "Hologram"; }
    }
}"#,
        );
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
        assert!(warnings[0].contains("Hologram"), "{}", warnings[0]);
        assert_eq!(bound(&config, InputMode::Normal, &key('z')), None);
    }

    /// Tolerance is for names this build does not know. A known action given an argument it cannot
    /// take is a mistake in something real, and still fails - even behind an unknown action.
    #[test]
    fn a_known_action_with_a_bad_argument_still_fails() {
        for kdl in [
            r#"keybinds { normal { bind "z" { MoveFocus "Sideways"; } } }"#,
            r#"keybinds { normal { bind "z" { ActionFromALaterBuild; MoveFocus "Sideways"; } } }"#,
        ] {
            assert!(Config::from_kdl(kdl, None).is_err(), "{}", kdl);
        }
    }

    #[test]
    fn an_unknown_key_is_left_out_and_the_other_keys_of_the_bind_are_bound() {
        let (config, warnings) = warnings_of(
            r#"keybinds clear-defaults=true {
    normal {
        bind "Frob x" "Alt x" { NoOp; }
        unbind "Frob y"
    }
}"#,
        );
        assert_eq!(warnings.len(), 2, "{:?}", warnings);
        assert!(warnings.iter().any(|w| w.contains("\"Frob x\"")));
        assert!(warnings.iter().any(|w| w.contains("\"Frob y\"")));
        let alt_x = KeyWithModifier::new(BareKey::Char('x')).with_alt_modifier();
        assert_eq!(
            bound(&config, InputMode::Normal, &alt_x),
            Some(vec![Action::NoOp])
        );
    }

    #[test]
    fn an_unknown_mode_in_shared_except_or_shared_among_is_ignored_and_the_block_applies() {
        let (config, warnings) = warnings_of(
            r#"keybinds clear-defaults=true {
    shared_except "locked" "hologram" {
        bind "z" { NoOp; }
    }
    shared_among "pane" "hologram" {
        bind "y" { NoOp; }
    }
}"#,
        );
        assert_eq!(warnings.len(), 2, "{:?}", warnings);
        assert!(warnings.iter().all(|w| w.contains("\"hologram\"")));
        assert_eq!(
            bound(&config, InputMode::Normal, &key('z')),
            Some(vec![Action::NoOp])
        );
        assert_eq!(bound(&config, InputMode::Locked, &key('z')), None);
        assert_eq!(
            bound(&config, InputMode::Pane, &key('y')),
            Some(vec![Action::NoOp])
        );
    }

    /// An unknown action inside `shared_except` is parsed once per mode the block binds. It is
    /// still one thing to fix, so it is one warning.
    #[test]
    fn an_unknown_action_in_a_shared_block_is_one_warning() {
        let (_, warnings) = warnings_of(
            r#"keybinds {
    shared_except "locked" {
        bind "z" { ActionFromALaterBuild; }
    }
}"#,
        );
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
    }

    #[test]
    fn an_unknown_top_level_setting_is_named() {
        let (config, warnings) = warnings_of("simplified_ui true\nsome_setting_from_later true\n");
        assert_eq!(config.options.simplified_ui, Some(true));
        assert_eq!(
            warnings,
            vec!["line 2: unknown setting \"some_setting_from_later\", ignored".to_owned()]
        );
    }

    #[test]
    fn an_unknown_permission_is_not_granted_and_the_others_are() {
        let (config, warnings) = warnings_of(
            r#"plugin_permissions {
    "/tmp/plugin.wasm" {
        ReadApplicationState
        ReadTheFuture
    }
}"#,
        );
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
        assert!(warnings[0].contains("ReadTheFuture"), "{}", warnings[0]);
        assert_eq!(
            config.plugin_permissions.granted.get("/tmp/plugin.wasm"),
            Some(&vec![PermissionType::ReadApplicationState])
        );
    }

    /// The written-for-a-newer-build config the rule exists for: one of everything a later
    /// release might add, all at once. It loads, every known setting in it takes effect, and each
    /// unknown one is named with the file and line it is on.
    #[test]
    fn a_config_written_for_a_newer_build_loads_with_a_warning_for_each_thing_skipped() {
        let kdl = r#"simplified_ui true
setting_from_later true
new_pane_default "floating"
default_mode "hologram"
pane_frame_style "dotted"
on_force_close "explode"
copy_clipboard "cloud"
web_sharing "public"
host_notification_protocol "osc777"
default_floating_size {
    width "80%"
    depth "10"
}
keybinds clear-defaults=true {
    normal {
        bind "z" { ActionFromALaterBuild; }
        bind "Frob a" "y" { NoOp; }
        bind "x" { SetPaneFrameStyle "dotted"; }
        bind "w" { SwitchToMode "hologram"; }
        teleport "q"
    }
    hologram {
        bind "v" { NoOp; }
    }
    shared_except "hologram" "locked" {
        bind "u" { NoOp; }
    }
}
ui {
    pane_frames {
        rounded_corners true
        sparkle true
    }
    status_bar_glow true
}
themes {
    later {
        text_unselected {
            base 1 2 3
            emphasis_0 1 2 3
            emphasis_1 1 2 3
            emphasis_2 1 2 3
            emphasis_3 1 2 3
            emphasis_4 1 2 3
        }
        tooltip {
            base 1 2 3
        }
    }
}
web_client {
    font "monospace"
    cursor_style "hollow"
    scroll_feel "smooth"
}
plugin_permissions {
    "/tmp/plugin.wasm" {
        ReadApplicationState
        ReadTheFuture
    }
}
"#;
        let config = Config::from_kdl_named(kdl, None, Some("/tmp/config.kdl")).unwrap();
        let warnings = config.warnings.clone();
        // every known setting took effect
        assert_eq!(config.options.simplified_ui, Some(true));
        assert!(config
            .options
            .default_floating_size
            .as_ref()
            .unwrap()
            .width
            .is_some());
        assert_eq!(
            bound(&config, InputMode::Normal, &key('y')),
            Some(vec![Action::NoOp])
        );
        assert_eq!(
            bound(&config, InputMode::Normal, &key('u')),
            Some(vec![Action::NoOp])
        );
        assert!(config.ui.pane_frames.rounded_corners);
        assert_eq!(config.web_client.font, "monospace");
        assert!(config.themes.get_theme("later").is_some());
        // every unknown one was skipped, and its default kept
        for skipped in ['z', 'x', 'w'] {
            assert_eq!(bound(&config, InputMode::Normal, &key(skipped)), None);
        }
        assert_eq!(config.new_pane_default, None);
        assert_eq!(config.options.default_mode, None);
        assert_eq!(config.options.pane_frame_style, None);
        assert_eq!(config.web_client.cursor_style, None);
        // and each one is named, with the file and the line it is on
        let expected = [
            (2, "setting_from_later"),
            (3, "floating"),
            (4, "hologram"),
            (5, "dotted"),
            (6, "explode"),
            (7, "cloud"),
            (8, "public"),
            (9, "osc777"),
            (12, "depth"),
            (16, "ActionFromALaterBuild"),
            (17, "Frob a"),
            (18, "dotted"),
            (19, "hologram"),
            (20, "teleport"),
            (22, "hologram"),
            (25, "hologram"),
            (32, "sparkle"),
            (34, "status_bar_glow"),
            (44, "emphasis_4"),
            (46, "tooltip"),
            (53, "hollow"),
            (54, "scroll_feel"),
            (59, "ReadTheFuture"),
        ];
        for (line, thing) in expected {
            let prefix = format!("/tmp/config.kdl:{}: ", line);
            assert!(
                warnings
                    .iter()
                    .any(|w| w.starts_with(&prefix) && w.contains(thing)),
                "no warning for {:?} on line {}:\n{}",
                thing,
                line,
                warnings.join("\n")
            );
        }
        assert_eq!(
            warnings.len(),
            expected.len(),
            "unexpected warnings:\n{}",
            warnings.join("\n")
        );
    }

    /// What ships must not warn: a warning nobody can act on teaches people to ignore the rest.
    #[test]
    fn the_default_config_parses_without_a_warning() {
        let config = Config::from_default_assets().unwrap();
        assert!(config.warnings.is_empty(), "{:?}", config.warnings);
    }

    /// The top-level list is written out by hand, so it is held to what `Options::from_kdl`
    /// actually reads: a new option added there without being added to the list would warn on
    /// every machine that sets it.
    #[test]
    fn every_option_from_kdl_reads_is_a_known_top_level_node() {
        let source = include_str!("mod.rs");
        let start = source
            .find("impl Options {\n    pub fn from_kdl(kdl_options: &KdlDocument)")
            .unwrap();
        let end = start + source[start..].find("\n    pub fn from_string(").unwrap();
        let body = &source[start..end];
        let mut read = vec![];
        let mut rest = body;
        while let Some(at) = rest.find("kdl_options") {
            rest = &rest[at + "kdl_options".len()..];
            let after = rest.trim_start_matches(|c: char| c == ',' || c.is_whitespace());
            let after = after.strip_prefix(".get(").unwrap_or(after);
            if let Some(quoted) = after.strip_prefix('"') {
                read.push(&quoted[..quoted.find('"').unwrap()]);
            }
        }
        assert!(read.len() > 50, "the scan found only {:?}", read);
        let missing: Vec<_> = read
            .iter()
            .filter(|name| !super::super::KNOWN_TOP_LEVEL_NODES.contains(name))
            .collect();
        assert!(
            missing.is_empty(),
            "not in KNOWN_TOP_LEVEL_NODES: {:?}",
            missing
        );
    }
}
