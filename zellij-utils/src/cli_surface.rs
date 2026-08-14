//! The CLI described to itself: the grouped, convention-first `zellij action --help`.
//!
//! Both readers here are agents rather than people. An agent that has to discover the surface one
//! `--help` at a time reads a subcommand's name, guesses the rest, and gets it wrong - that is the
//! failure this module exists to answer. So the command tree is walked rather than transcribed:
//! the one-liner beside each name is read out of the parser that will handle the call, and only
//! what clap cannot know - which verbs belong together - is written down here.
//!
//! What is written down is guarded by the tests at the bottom: a new `CliAction` variant that
//! nobody grouped fails the build, rather than quietly going missing from the map.

use clap::{Command as ClapCommand, CommandFactory, FromArgMatches};

use crate::cli::CliArgs;

/// A band of `zellij action` verbs that answer the same kind of question.
pub struct ActionGroup {
    pub name: &'static str,
    pub blurb: &'static str,
    pub commands: &'static [&'static str],
}

/// Every `zellij action` subcommand, in the band it belongs to.
///
/// A command lives in exactly one band, and every command lives in one: `every_action_is_grouped`
/// checks both against the clap tree.
pub const ACTION_GROUPS: &[ActionGroup] = &[
    ActionGroup {
        name: "read",
        blurb: "ask the session something; change nothing",
        commands: &[
            "are-floating-panes-visible",
            "current-tab-info",
            "dump-layout",
            "dump-screen",
            "list-clients",
            "list-panes",
            "list-tabs",
            "list-tree",
        ],
    },
    ActionGroup {
        name: "navigate",
        blurb: "move focus or the view; change no content",
        commands: &[
            "focus-last-pane",
            "focus-next-pane",
            "focus-pane-id",
            "focus-previous-pane",
            "go-to-next-tab",
            "go-to-previous-tab",
            "go-to-tab",
            "go-to-tab-by-id",
            "go-to-tab-name",
            "half-page-scroll-down",
            "half-page-scroll-up",
            "move-focus",
            "move-focus-or-tab",
            "page-scroll-down",
            "page-scroll-up",
            "scroll-down",
            "scroll-to-bottom",
            "scroll-to-top",
            "scroll-up",
        ],
    },
    ActionGroup {
        name: "create",
        blurb: "make a pane or a tab; each reports the id and handle of what it made",
        commands: &[
            "break-pane",
            "break-pane-left",
            "break-pane-right",
            "break-pane-to-tab",
            "edit",
            "edit-scrollback",
            "launch-or-focus-plugin",
            "launch-plugin",
            "new-pane",
            "new-tab",
            "start-or-reload-plugin",
        ],
    },
    ActionGroup {
        name: "mutate",
        blurb: "change a pane or a tab, or what runs in one",
        commands: &[
            "change-floating-pane-coordinates",
            "clear",
            "close-pane",
            "close-tab",
            "close-tab-by-id",
            "hide-floating-panes",
            "move-pane",
            "move-pane-backwards",
            "move-tab",
            "next-swap-layout",
            "override-layout",
            "paste",
            "pipe",
            "previous-swap-layout",
            "rename-pane",
            "rename-tab",
            "rename-tab-by-id",
            "resize",
            "send-keys",
            "set-fullscreen",
            "set-pane-borderless",
            "set-pane-color",
            "set-pane-floating",
            "set-pane-pinned",
            "set-sync-tab",
            "show-floating-panes",
            "signal-pane",
            "stack-panes",
            "toggle-active-sync-tab",
            "toggle-floating-panes",
            "toggle-fullscreen",
            "toggle-no-ui-fullscreen",
            "toggle-pane-borderless",
            "toggle-pane-embed-or-floating",
            "toggle-pane-pinned",
            "undo-rename-pane",
            "undo-rename-tab",
            "write",
            "write-chars",
        ],
    },
    ActionGroup {
        name: "session",
        blurb: "act on the whole session, or on every client attached to it",
        commands: &[
            "detach",
            "rename-session",
            "save-session",
            "set-dark-theme",
            "set-light-theme",
            "set-pane-frame-style",
            "switch-mode",
            "switch-session",
            "toggle-pane-frames",
            "toggle-theme",
        ],
    },
];

/// The conventions, said once, at the top of `zellij action --help`.
pub const ACTION_PREAMBLE: &str = "\
Send an action to a session: the one you are in, or the one `--session` names.

How every one of these answers:

  * A single record is `key: value` lines. A list of like things is a table with an UPPER_SNAKE
    header row. A nesting of them is an indented outline. Keys and columns are append-only:
    a release may add one, never rename or remove one.
  * `--json` carries the same information, structured, wherever it is offered. The queries all
    have it.
  * Results go to stdout, diagnostics go to stderr, and a payload command prints its payload
    alone.
  * Exit 0 acted or found, 1 error, 2 miss. A miss is a well-formed request about something that
    is not there - a closed pane, a tab by a name nothing answers to.
  * A command that only acts prints nothing. The ones that report say so in their own --help.
  * A mutation run from outside the session must name what it acts on. `close-pane`, `close-tab`,
    `move-tab` and `break-pane` refuse a targetless call from a script, because \"the focused pane\"
    there is a pane you have never seen.

A pane is addressed by any of `terminal_1`, `plugin_2`, a bare integer (3 means terminal_3), a
two-word handle like `sunny-otter`, or a pane uuid. The handle is the pane's address: it is
assigned when the pane is created, it survives a session restore, and `list-panes` prints it.

  zellij action dump-screen --pane-id sunny-otter";

/// One line per command, grouped, for the body of `zellij action --help`.
///
/// The names and one-liners come out of the clap tree, so a command cannot appear here saying
/// something its own `--help` does not.
pub fn action_command_listing(action: &ClapCommand) -> String {
    let mut out = String::new();
    let width = ACTION_GROUPS
        .iter()
        .flat_map(|g| g.commands.iter())
        .map(|c| c.len())
        .max()
        .unwrap_or(0);
    for group in ACTION_GROUPS {
        out.push_str(&format!("{} - {}:\n", group.name, group.blurb));
        for name in group.commands {
            let command = action.get_subcommands().find(|s| s.get_name() == *name);
            let mut about = command
                .and_then(|s| s.get_about().map(|a| one_line(&a.to_string())))
                .unwrap_or_default();
            // an alias a reader will meet in the wild - `go-to-pane` - belongs beside the name it
            // stands for, not one --help deeper
            let aliases: Vec<&str> = command
                .map(|s| s.get_visible_aliases().collect())
                .unwrap_or_default();
            if !aliases.is_empty() {
                about.push_str(&format!(" [alias: {}]", aliases.join(", ")));
            }
            out.push_str(&format!("  {:width$}  {}\n", name, about, width = width));
        }
        out.push('\n');
    }
    out
}

/// The parser the binary actually runs, with the grouped help attached to `action`.
pub fn decorated_command() -> ClapCommand {
    let cmd = CliArgs::command();
    let listing = action_command_listing(
        cmd.get_subcommands()
            .find(|s| s.get_name() == "action")
            .expect("the CLI has an `action` subcommand"),
    );
    // clap renders subcommands only as one flat list, so the grouping replaces `{all-args}` with a
    // listing this module builds and leaves clap to print the options below it.
    let template = format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\nCommands:\n{}{{options}}{{after-help}}",
        indent(&listing, 2)
    );
    cmd.mut_subcommand("action", move |action| {
        action
            .long_about(ACTION_PREAMBLE)
            .help_template(template.clone())
    })
}

/// Parse the command line through the decorated tree.
///
/// The binary's entry point, in place of `CliArgs::parse()`: the decoration is help text only, so
/// what parses here parses the same, but `zellij action --help` reads as one document instead of
/// eighty-seven names.
pub fn parse_cli_args() -> CliArgs {
    let matches = decorated_command().get_matches();
    match CliArgs::from_arg_matches(&matches) {
        Ok(args) => args,
        Err(e) => e.exit(),
    }
}

/// Help text is written over several lines; a record line holds one.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn indent(text: &str, by: usize) -> String {
    let pad = " ".repeat(by);
    text.lines()
        .map(|line| {
            if line.is_empty() {
                line.to_owned()
            } else {
                format!("{}{}", pad, line)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::on_big_stack;

    fn action_subcommand_names() -> Vec<String> {
        on_big_stack(|| {
            let mut cmd = CliArgs::command();
            cmd.build();
            let names = cmd
                .get_subcommands()
                .find(|s| s.get_name() == "action")
                .expect("the CLI has an `action` subcommand")
                .get_subcommands()
                .filter(|s| !s.is_hide_set() && s.get_name() != "help")
                .map(|s| s.get_name().to_owned())
                .collect::<Vec<_>>();
            names
        })
    }

    #[test]
    fn every_action_is_grouped_exactly_once() {
        // the guard on the hand-written half of this module: a new `CliAction` variant that nobody
        // put in a band fails here rather than going missing from a map that claims to be whole
        let grouped: Vec<&str> = ACTION_GROUPS
            .iter()
            .flat_map(|g| g.commands.iter().copied())
            .collect();
        for name in action_subcommand_names() {
            assert_eq!(
                grouped.iter().filter(|g| **g == name).count(),
                1,
                "`{}` is in {} groups, expected exactly 1",
                name,
                grouped.iter().filter(|g| **g == name).count()
            );
        }
        for name in &grouped {
            assert!(
                action_subcommand_names().iter().any(|n| n == name),
                "`{}` is grouped but is not a command",
                name
            );
        }
    }

    #[test]
    fn every_grouped_command_has_a_one_liner() {
        // the grouped listing is the first thing an agent reads; a blank line in it teaches nothing
        on_big_stack(|| {
            let mut cmd = CliArgs::command();
            cmd.build();
            let action = cmd
                .get_subcommands()
                .find(|s| s.get_name() == "action")
                .expect("the CLI has an `action` subcommand");
            for sub in action.get_subcommands() {
                if sub.is_hide_set() || sub.get_name() == "help" {
                    continue;
                }
                assert!(
                    sub.get_about().is_some(),
                    "`zellij action {}` has no about line",
                    sub.get_name()
                );
            }
        })
    }

    #[test]
    fn the_grouped_help_names_the_conventions_once() {
        let listing = on_big_stack(|| {
            let mut cmd = CliArgs::command();
            cmd.build();
            let action = cmd
                .get_subcommands()
                .find(|s| s.get_name() == "action")
                .unwrap()
                .clone();
            action_command_listing(&action)
        });
        for group in ACTION_GROUPS {
            assert!(listing.contains(group.name), "{} is missing", group.name);
        }
        assert!(listing.contains("list-panes"));
        assert!(ACTION_PREAMBLE.contains("Exit 0 acted or found, 1 error, 2 miss"));
        assert!(ACTION_PREAMBLE.contains("sunny-otter"));
    }
}
