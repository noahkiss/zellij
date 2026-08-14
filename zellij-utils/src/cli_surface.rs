//! The CLI described to itself: the grouped `zellij action --help`, and `--dump-surface`.
//!
//! Both readers here are agents rather than people. An agent that has to discover the surface one
//! `--help` at a time reads a subcommand's name, guesses the rest, and gets it wrong - that is the
//! failure this module exists to answer. So the command tree is walked rather than transcribed:
//! everything clap knows (names, arguments, types, defaults, help) is read out of the parser that
//! will handle the call, and only what clap cannot know (which verbs belong together, what a
//! command prints) is written down here.
//!
//! What is written down is guarded by the tests at the bottom: a new `CliAction` variant that
//! nobody grouped fails the build, rather than quietly going missing from the map.

use clap::{ArgAction, Command as ClapCommand, CommandFactory, FromArgMatches};

use crate::cli::CliArgs;
use crate::consts::VERSION;

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

/// What a command puts on stdout, for the commands that put anything there.
///
/// The one thing clap cannot introspect. A command absent from this table prints nothing when it
/// succeeds - that is the fork's default, not an omission - so the dump says so rather than
/// leaving the reader to guess.
struct OutputSpec {
    /// The command path, without the leading `zellij `.
    command: &'static str,
    /// `record`, `table`, `outline` or `payload` - the shapes of the output convention.
    shape: &'static str,
    /// The keys of a record, or the columns of a table, in the order they are printed.
    keys: &'static str,
}

const OUTPUTS: &[OutputSpec] = &[
    OutputSpec {
        command: "action current-tab-info",
        shape: "record",
        keys: "name id position",
    },
    OutputSpec {
        command: "action dump-layout",
        shape: "payload",
        keys: "",
    },
    OutputSpec {
        command: "action dump-screen",
        shape: "payload",
        keys: "",
    },
    OutputSpec {
        command: "action list-clients",
        shape: "table",
        keys: "CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND TTY SIZE CURRENT",
    },
    OutputSpec {
        command: "action list-panes",
        shape: "table",
        keys: "TAB_ID TAB_POS TAB_NAME PANE_ID HANDLE TYPE TITLE COMMAND CWD FOCUSED FLOATING \
               EXITED X Y ROWS COLS",
    },
    OutputSpec {
        command: "action list-tabs",
        shape: "table",
        keys: "TAB_ID POSITION NAME ACTIVE FULLSCREEN SYNC_PANES FLOATING_VIS VP_ROWS VP_COLS \
               DA_ROWS DA_COLS TILED_PANES FLOAT_PANES HIDDEN_PANES SWAP_LAYOUT LAYOUT_DIRTY",
    },
    OutputSpec {
        command: "action list-tree",
        shape: "outline",
        keys: "tab_id position name active / handle pane_id title command focused",
    },
    OutputSpec {
        command: "action new-pane",
        shape: "record",
        // `tab_id` is `--new-tab`'s line: the tab it made, above the pane it put in it. Every other
        // new-pane prints the pane alone, into a tab that already existed
        keys: "tab_id pane_id handle",
    },
    OutputSpec {
        command: "action edit",
        shape: "record",
        keys: "pane_id handle",
    },
    OutputSpec {
        command: "action launch-plugin",
        shape: "record",
        keys: "pane_id handle",
    },
    OutputSpec {
        command: "action launch-or-focus-plugin",
        shape: "record",
        keys: "pane_id handle",
    },
    OutputSpec {
        command: "action new-tab",
        shape: "record",
        keys: "tab_id pane_id handle",
    },
    OutputSpec {
        command: "action break-pane",
        shape: "record",
        keys: "tab_id",
    },
    OutputSpec {
        command: "action close-pane",
        shape: "record",
        keys: "closed",
    },
    OutputSpec {
        command: "action close-tab",
        shape: "record",
        keys: "closed",
    },
    OutputSpec {
        command: "action close-tab-by-id",
        shape: "record",
        keys: "closed",
    },
    OutputSpec {
        command: "action go-to-tab",
        shape: "record",
        keys: "from to",
    },
    OutputSpec {
        command: "action go-to-tab-by-id",
        shape: "record",
        keys: "from to",
    },
    OutputSpec {
        command: "action go-to-tab-name",
        shape: "record",
        keys: "from to id pane_id handle",
    },
    OutputSpec {
        command: "action focus-pane-id",
        shape: "record",
        // `id` and `handle` are the probe's answer (`--no-focus`), the way `id` is the tab probe's
        // above; the jump itself prints `from` and `to`
        keys: "from to id handle",
    },
    OutputSpec {
        command: "action move-tab",
        shape: "record",
        keys: "from to",
    },
    OutputSpec {
        command: "action are-floating-panes-visible",
        shape: "payload",
        keys: "",
    },
    OutputSpec {
        command: "ls",
        shape: "table",
        keys: "NAME STATUS CURRENT CLIENTS CREATED",
    },
    // a stream rather than an answer: the pane's own lines, until the pane closes. `--format json`
    // wraps each update in a record instead, and `--timestamps` adds the time to either
    OutputSpec {
        command: "subscribe",
        shape: "payload",
        keys: "",
    },
];

/// The keys or columns the dump promises for a command, or `None` if it promises nothing.
///
/// Exported so the code that does the printing can pin this table to what it actually prints. The
/// table is hand-written - clap cannot see stdout - and it has drifted from the printer more than
/// once, which is the whole reason a caller can reach it from outside this module.
pub fn promised_output_keys(command: &str) -> Option<&'static str> {
    OUTPUTS
        .iter()
        .find(|o| o.command == command)
        .map(|o| o.keys)
        .filter(|keys| !keys.is_empty())
}

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
    `move-tab`, `break-pane`, `write`, `write-chars`, `clear`, `edit-scrollback` and `rename-pane`
    refuse a targetless call from a script, because \"the focused pane\" there is a pane you have
    never seen.

A pane is addressed by any of `terminal_1`, `plugin_2`, a bare integer (3 means terminal_3), a
two-word handle like `sunny-otter`, or a pane uuid. The handle is the pane's address: it is
assigned when the pane is created - or chosen then, with `new-pane --handle` - it survives a session
restore, and `list-panes` prints it.

  zellij action dump-screen --pane-id sunny-otter

`zellij setup --dump-surface` prints this whole command tree - every command, flag and output
shape - in one call.";

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
    // listing this module builds and leaves clap to print the options below it. `{options}` is the
    // option list alone - the `Options:` heading belongs to `{all-args}`, which is what was
    // replaced - so the heading is written here, or the flags run on from the last command.
    let template = format!(
        "{{before-help}}{{about-with-newline}}\n{{usage-heading}} {{usage}}\n\nCommands:\n{}\nOptions:\n{{options}}{{after-help}}",
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

/// The whole command tree, in the fork's outline convention.
///
/// One record per command, deepest path spelled out in full, so a reader can grep for the command
/// it wants and get the flags with it.
pub fn dump_surface_text() -> String {
    let cmd = CliArgs::command();
    let mut out = String::new();
    out.push_str(&format!("version: {}\n", VERSION));
    out.push_str("convention: record is `key: value` lines, list is a table with an UPPER_SNAKE header, nesting is an indented outline\n");
    out.push_str("exit: 0 acted or found  1 error  2 miss\n");
    out.push_str("prints: a command with no `prints:` line prints nothing when it succeeds\n");
    out.push_str("pane_target: terminal_1 | plugin_2 | 3 | sunny-otter (handle) | uuid\n");
    out.push('\n');
    for command in walk(&cmd) {
        out.push_str(&command_record(&command));
    }
    out
}

/// The same map, structured, for a program rather than an agent reading a shell.
pub fn dump_surface_json() -> String {
    let cmd = CliArgs::command();
    let commands: Vec<serde_json::Value> = walk(&cmd)
        .iter()
        .map(|c| {
            let output = OUTPUTS.iter().find(|o| o.command == c.path);
            serde_json::json!({
                "command": c.path,
                "group": c.group,
                "about": c.about,
                "aliases": c.aliases,
                "json": c.args.iter().any(|a| a.name == "--json"),
                "prints": output.map(|o| o.shape),
                "keys": output.map(|o| o.keys).filter(|k| !k.is_empty()),
                "args": c.args.iter().map(|a| serde_json::json!({
                    "name": a.name,
                    "positional": a.positional,
                    "type": a.kind,
                    "required": a.required,
                    "repeatable": a.repeatable,
                    "default": a.default,
                    "about": a.about,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let document = serde_json::json!({
        "version": VERSION,
        "convention": {
            "record": "key: value lines",
            "list": "table with an UPPER_SNAKE header row",
            "nesting": "indented outline",
            "append_only": true,
            "exit": {"0": "acted or found", "1": "error", "2": "miss"},
            "silent_on_success": "a command with no `prints` prints nothing",
            "pane_target": ["terminal_1", "plugin_2", "3", "sunny-otter (handle)", "uuid"],
        },
        "commands": commands,
    });
    serde_json::to_string_pretty(&document).unwrap_or_else(|e| format!("{{\"error\": \"{}\"}}", e))
}

struct SurfaceCommand {
    path: String,
    group: Option<&'static str>,
    about: String,
    aliases: Vec<String>,
    args: Vec<SurfaceArg>,
}

struct SurfaceArg {
    name: String,
    positional: bool,
    kind: String,
    required: bool,
    repeatable: bool,
    default: Option<String>,
    about: String,
}

/// Every command in the tree, leaves and branches alike, in the order clap declares them.
fn walk(root: &ClapCommand) -> Vec<SurfaceCommand> {
    let mut root = root.clone();
    root.build();
    let mut found = Vec::new();
    for sub in root.get_subcommands() {
        collect(sub, "", &mut found);
    }
    found
}

fn collect(cmd: &ClapCommand, prefix: &str, found: &mut Vec<SurfaceCommand>) {
    if cmd.is_hide_set() || cmd.get_name() == "help" {
        return;
    }
    let path = if prefix.is_empty() {
        cmd.get_name().to_owned()
    } else {
        format!("{} {}", prefix, cmd.get_name())
    };
    found.push(SurfaceCommand {
        group: group_of(&path),
        about: cmd
            .get_about()
            .map(|a| one_line(&a.to_string()))
            .unwrap_or_default(),
        aliases: cmd
            .get_visible_aliases()
            .map(|a| a.to_owned())
            .collect::<Vec<_>>(),
        args: cmd.get_arguments().filter_map(surface_arg).collect(),
        path,
    });
    let path = found.last().map(|c| c.path.clone()).unwrap_or_default();
    for sub in cmd.get_subcommands() {
        collect(sub, &path, found);
    }
}

fn group_of(path: &str) -> Option<&'static str> {
    let name = path.strip_prefix("action ")?;
    ACTION_GROUPS
        .iter()
        .find(|g| g.commands.contains(&name))
        .map(|g| g.name)
}

fn surface_arg(arg: &clap::Arg) -> Option<SurfaceArg> {
    if arg.is_hide_set() || arg.get_id() == "help" || arg.get_id() == "version" {
        return None;
    }
    let name = if arg.is_positional() {
        arg.get_id().to_string()
    } else if let Some(long) = arg.get_long() {
        format!("--{}", long)
    } else if let Some(short) = arg.get_short() {
        format!("-{}", short)
    } else {
        arg.get_id().to_string()
    };
    let kind_is_flag = matches!(
        arg.get_action(),
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count
    );
    let kind = match arg.get_action() {
        ArgAction::SetTrue | ArgAction::SetFalse | ArgAction::Count => "flag".to_owned(),
        _ => match arg.get_possible_values() {
            values if !values.is_empty() => values
                .iter()
                .map(|v| v.get_name().to_owned())
                .collect::<Vec<_>>()
                .join("|"),
            _ => "value".to_owned(),
        },
    };
    Some(SurfaceArg {
        positional: arg.is_positional(),
        kind,
        required: arg.is_required_set(),
        repeatable: matches!(arg.get_action(), ArgAction::Append),
        // a flag's default is its absence, and printing `default: false` on ninety of them buries
        // the handful of defaults that are worth knowing
        default: if kind_is_flag {
            None
        } else {
            arg.get_default_values()
                .first()
                .map(|v| v.to_string_lossy().into_owned())
        },
        about: arg
            .get_help()
            .map(|h| one_line(&h.to_string()))
            .unwrap_or_default(),
        name,
    })
}

fn command_record(command: &SurfaceCommand) -> String {
    let mut out = format!("command: zellij {}", command.path);
    if let Some(group) = command.group {
        out.push_str(&format!("  group: {}", group));
    }
    if !command.aliases.is_empty() {
        out.push_str(&format!("  aliases: {}", command.aliases.join(" ")));
    }
    out.push('\n');
    if !command.about.is_empty() {
        out.push_str(&format!("  about: {}\n", command.about));
    }
    if let Some(output) = OUTPUTS.iter().find(|o| o.command == command.path) {
        out.push_str(&format!("  prints: {}", output.shape));
        if !output.keys.is_empty() {
            out.push_str(&format!("  keys: {}", output.keys));
        }
        out.push('\n');
    }
    for arg in &command.args {
        out.push_str(&format!("  arg: {}  type: {}", arg.name, arg.kind));
        if arg.positional {
            out.push_str("  positional: true");
        }
        if arg.required {
            out.push_str("  required: true");
        }
        if arg.repeatable {
            out.push_str("  repeatable: true");
        }
        if let Some(default) = &arg.default {
            out.push_str(&format!("  default: {}", default));
        }
        if !arg.about.is_empty() {
            out.push_str(&format!("  about: {}", arg.about));
        }
        out.push('\n');
    }
    out.push('\n');
    out
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

/// One record of the dump, spelled out: the snapshot half of `the_dump_is_the_shape_it_says_it_is`.
#[cfg(test)]
const EXPECTED_LIST_TREE_RECORD: &str = concat!(
    "command: zellij action list-tree  group: read\n",
    "  about: List every tab with its panes nested beneath it\n",
    "  prints: outline  keys: tab_id position name active / handle pane_id title command focused\n",
    "  arg: --json  type: flag  about: Output as JSON",
);

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
    fn the_dump_carries_every_command_and_its_flags() {
        let dump = on_big_stack(dump_surface_text);
        for name in action_subcommand_names() {
            assert!(
                dump.contains(&format!("command: zellij action {}  group:", name)),
                "`{}` is missing from the dump",
                name
            );
        }
        // the generated half tracks clap: a flag nobody dumped cannot hide
        assert!(dump.contains("command: zellij action list-panes  group: read"));
        assert!(dump.contains("  arg: --json  type: flag"));
        assert!(dump.contains("  prints: table  keys: TAB_ID TAB_POS"));
        assert!(dump.contains("command: zellij setup"));
        assert!(dump.contains("command: zellij session up"));
    }

    #[test]
    fn every_flag_of_a_command_reaches_the_dump() {
        // the shape is a contract, and the contract is "all of it": walk one busy command's
        // arguments out of clap and require each one by name
        let dump = on_big_stack(dump_surface_text);
        let names = on_big_stack(|| {
            let mut cmd = CliArgs::command();
            cmd.build();
            let names = cmd
                .get_subcommands()
                .find(|s| s.get_name() == "action")
                .unwrap()
                .get_subcommands()
                .find(|s| s.get_name() == "new-pane")
                .unwrap()
                .get_arguments()
                .filter(|a| !a.is_hide_set() && a.get_id() != "help")
                .map(|a| {
                    a.get_long()
                        .map(|l| format!("--{}", l))
                        .unwrap_or_else(|| a.get_id().to_string())
                })
                .collect::<Vec<_>>();
            names
        });
        assert!(names.len() > 20, "new-pane should have a lot of flags");
        let record = dump
            .split("\n\n")
            .find(|r| r.starts_with("command: zellij action new-pane"))
            .expect("new-pane is in the dump");
        for name in names {
            assert!(
                record.contains(&format!("  arg: {}  type:", name)),
                "`{}` is missing from new-pane's record",
                name
            );
        }
    }

    #[test]
    fn the_dump_is_the_shape_it_says_it_is() {
        // the snapshot: the grammar of the whole document, and one record spelled out. Snapshotting
        // all eight hundred lines would make every help edit a snapshot update, which is how a
        // snapshot stops being read - so what is pinned here is the shape a parser depends on
        let dump = on_big_stack(dump_surface_text);
        let (header, records) = dump.split_once("\n\n").expect("a header, then the records");
        assert_eq!(
            header
                .lines()
                .map(|l| l.split_once(": ").unwrap().0)
                .collect::<Vec<_>>(),
            vec!["version", "convention", "exit", "prints", "pane_target"]
        );
        for line in records.lines().filter(|l| !l.is_empty()) {
            let known = line.starts_with("command: zellij ")
                || line.starts_with("  about: ")
                || line.starts_with("  prints: ")
                || line.starts_with("  arg: ");
            assert!(
                known,
                "the dump grew a line shape nobody parses: {:?}",
                line
            );
        }
        let list_tree = records
            .split("\n\n")
            .find(|r| r.starts_with("command: zellij action list-tree"))
            .expect("list-tree is in the dump");
        assert_eq!(list_tree, EXPECTED_LIST_TREE_RECORD);
    }

    #[test]
    fn the_json_dump_is_the_same_map_structured() {
        let json: serde_json::Value =
            serde_json::from_str(&on_big_stack(dump_surface_json)).expect("the dump is JSON");
        let commands = json["commands"].as_array().expect("commands is an array");
        let list_panes = commands
            .iter()
            .find(|c| c["command"] == "action list-panes")
            .expect("list-panes is in the dump");
        assert_eq!(list_panes["group"], "read");
        assert_eq!(list_panes["json"], true);
        assert_eq!(list_panes["prints"], "table");
        let close_pane = commands
            .iter()
            .find(|c| c["command"] == "action close-pane")
            .expect("close-pane is in the dump");
        assert_eq!(close_pane["prints"], "record");
        assert_eq!(close_pane["keys"], "closed");
        let write = commands
            .iter()
            .find(|c| c["command"] == "action write")
            .expect("write is in the dump");
        assert!(write["prints"].is_null(), "write prints nothing");
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
        assert!(ACTION_PREAMBLE.contains("--dump-surface"));
    }
}
