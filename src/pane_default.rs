use std::io::Write;
use zellij_utils::{cli::CliAction, input::config::NewPaneDefault};

pub(crate) fn caller_anchor(
    session: &str,
    caller_session: Option<&str>,
    pane_id: Option<&str>,
) -> Option<u32> {
    if caller_session.is_some_and(|caller| caller != session) {
        return None;
    }
    pane_id.and_then(|id| id.trim().parse().ok())
}

/// Apply the CLI default before building actions. Return whether an edit needs rewriting.
pub(crate) fn apply(
    action: &mut CliAction,
    default: Option<NewPaneDefault>,
    anchor: Option<u32>,
    stderr: &mut impl Write,
) -> bool {
    if default != Some(NewPaneDefault::Stacked) {
        return false;
    }
    let eligible = match action {
        CliAction::NewPane {
            direction,
            floating,
            stacked,
            in_place,
            new_tab,
            in_tab,
            near,
            tab_id,
            near_current_pane,
            plugin,
            ..
        } => {
            direction.is_none()
                && !*floating
                && !*stacked
                && !*in_place
                && new_tab.is_none()
                && in_tab.is_none()
                && near.is_none()
                && tab_id.is_none()
                && !*near_current_pane
                && plugin.is_none()
        },
        CliAction::Edit {
            direction,
            floating,
            in_place,
            tab_id,
            near_current_pane,
            ..
        } => {
            direction.is_none()
                && !*floating
                && !*in_place
                && tab_id.is_none()
                && !*near_current_pane
        },
        _ => false,
    };
    if !eligible {
        return false;
    }
    if anchor.is_none() {
        let _ =
            writeln!(stderr,
            "new_pane_default=stacked: no anchor pane in ZELLIJ_PANE_ID; falling back to a split.");
        return false;
    }
    match action {
        CliAction::NewPane {
            stacked,
            near_current_pane,
            ..
        } => {
            *stacked = true;
            *near_current_pane = true;
            false
        },
        CliAction::Edit { .. } => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use zellij_utils::cli::{on_big_stack, CliArgs, Command};

    fn parse(argv: Vec<&str>) -> CliAction {
        let argv: Vec<String> = argv.into_iter().map(str::to_owned).collect();
        let cli = on_big_stack(move || CliArgs::try_parse_from(argv)).unwrap();
        match cli.command.unwrap() {
            Command::Action { action, .. } => *action,
            _ => unreachable!(),
        }
    }

    fn action(flags: &[&str]) -> CliAction {
        let mut argv = vec!["zellij", "action", "new-pane"];
        argv.extend(flags);
        parse(argv)
    }

    #[test]
    fn absent_direction_stacks_on_the_caller() {
        let mut request = action(&[]);
        let mut stderr = Vec::new();
        apply(
            &mut request,
            Some(NewPaneDefault::Stacked),
            Some(7),
            &mut stderr,
        );
        assert!(matches!(
            request,
            CliAction::NewPane {
                stacked: true,
                near_current_pane: true,
                ..
            }
        ));
        assert!(stderr.is_empty());
    }

    #[test]
    fn another_session_cannot_supply_the_implicit_anchor() {
        assert_eq!(caller_anchor("work", Some("work"), Some("7")), Some(7));
        assert_eq!(caller_anchor("work", Some("other"), Some("7")), None);
        assert_eq!(caller_anchor("work", None, Some("7")), Some(7));
        assert_eq!(caller_anchor("work", Some("work"), Some("invalid")), None);
    }

    #[test]
    fn explicit_placement_is_untouched() {
        for flags in [
            vec!["--direction", "right"],
            vec!["--floating"],
            vec!["--stacked"],
            vec!["--in-place"],
            vec!["--new-tab", "build"],
            vec!["--tab-id", "2"],
            vec!["--in-tab", "build"],
            vec!["--near", "terminal_7"],
            vec!["--near-current-pane"],
        ] {
            let mut request = action(&flags);
            let before = format!("{:?}", request);
            let mut stderr = Vec::new();
            apply(
                &mut request,
                Some(NewPaneDefault::Stacked),
                Some(7),
                &mut stderr,
            );
            assert_eq!(format!("{:?}", request), before);
            assert!(stderr.is_empty());
        }
    }

    #[test]
    fn missing_anchor_splits_and_explains_why() {
        let mut request = action(&[]);
        let before = format!("{:?}", request);
        let mut stderr = Vec::new();
        apply(
            &mut request,
            Some(NewPaneDefault::Stacked),
            None,
            &mut stderr,
        );
        assert_eq!(format!("{:?}", request), before);
        assert!(String::from_utf8(stderr)
            .unwrap()
            .contains("falling back to a split"));
    }

    #[test]
    fn split_and_unset_keep_the_upstream_default() {
        for default in [None, Some(NewPaneDefault::Split)] {
            let mut request = action(&[]);
            let before = format!("{:?}", request);
            let mut stderr = Vec::new();
            apply(&mut request, default, Some(7), &mut stderr);
            assert_eq!(format!("{:?}", request), before);
            assert!(stderr.is_empty());
        }
    }

    #[test]
    fn edits_stack_only_without_explicit_placement_and_with_an_anchor() {
        for (flags, anchor, expected, warning) in [
            (vec![], Some(7), true, false),
            (vec![], None, false, true),
            (vec!["--direction", "down"], Some(7), false, false),
            (vec!["--floating"], Some(7), false, false),
            (vec!["--in-place"], Some(7), false, false),
            (vec!["--tab-id", "2"], Some(7), false, false),
            (vec!["--near-current-pane"], Some(7), false, false),
        ] {
            let mut argv = vec!["zellij", "action", "edit", "notes.txt"];
            argv.extend(flags);
            let mut request = parse(argv);
            let before = format!("{:?}", request);
            let mut stderr = Vec::new();
            assert_eq!(
                apply(
                    &mut request,
                    Some(NewPaneDefault::Stacked),
                    anchor,
                    &mut stderr
                ),
                expected
            );
            assert_eq!(!stderr.is_empty(), warning);
            assert_eq!(format!("{:?}", request), before);
        }
    }
}
