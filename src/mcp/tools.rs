//! The seven tools, and the half of each one that is generated rather than written.
//!
//! A tool's description is routing logic: it decides whether the tool is called at all, so the
//! part that says what the tool is FOR is written by hand and kept blunt. The part that says what
//! comes back is not written here at all - it is generated from the same `OUTPUTS` table
//! `zellij setup --dump-surface` reads, and so is every input-schema property that stands for a
//! real CLI flag. A flag renamed in `cli.rs` therefore cannot leave a stale description behind:
//! `every_parameter_names_a_real_argument` fails the build instead.
//!
//! Seven, not one per verb. `zellij action` has eighty-seven of those, and a surface nobody can
//! route through is worse than a small one that names its own follow-ups.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{json, Map, Value};
use zellij_utils::cli_surface;

/// What a tool parameter is, in JSON Schema terms.
pub enum ParamKind {
    Str,
    Bool,
    Int,
    /// A closed set, written out here because the tool's own values need not be the CLI's.
    Enum(&'static [&'static str]),
}

/// One parameter of one tool.
pub struct ParamSpec {
    /// The property name the caller passes. Unambiguous on its own - `pane`, not `id`.
    pub name: &'static str,
    pub kind: ParamKind,
    pub required: bool,
    /// The CLI command and argument this property stands for, as `--dump-surface` names them.
    /// When it is set, the property's description is clap's own help for that argument, so the two
    /// cannot drift. `None` for a property the tool invents, which then uses `describe`.
    pub from: Option<(&'static str, &'static str)>,
    /// The description for a property the CLI has no argument for. Ignored when `from` is set.
    pub describe: &'static str,
    pub default: Option<&'static str>,
}

/// One tool: what is written by hand, and where the rest is generated from.
pub struct ToolSpec {
    pub name: &'static str,
    /// The first line. Imperative, blunt, no hedging.
    pub summary: &'static str,
    /// The situation this tool is the right answer to.
    pub best_for: &'static str,
    /// When NOT to use it. Ambiguity about scope is the main cause of a wrong tool being picked.
    pub not_for: &'static str,
    /// The tool to reach for next, and when.
    pub follow_up: Option<&'static str>,
    /// The CLI command whose printed output this tool returns. The `Returns:` line is generated
    /// from that command's row in the surface map, so it cannot promise a key nothing prints.
    pub reports: &'static str,
    /// Anything the caller has to know that the generated half cannot say - a discipline the CLI
    /// enforces, a value that has to be named explicitly. Empty for a tool with none.
    pub tips: &'static str,
    pub params: &'static [ParamSpec],
    pub read_only: bool,
    /// Whether a call can destroy something a caller cannot get back. Only meaningful when
    /// `read_only` is false.
    pub destructive: bool,
    pub idempotent: bool,
}

/// The `session` parameter, which every tool takes and none of them require.
const SESSION: ParamSpec = ParamSpec {
    name: "session",
    kind: ParamKind::Str,
    required: false,
    from: None,
    describe: "The session to act on. Defaults to ZELLIJ_SESSION_NAME in this server's own \
               environment, which is the session it was started from.",
    default: None,
};

pub const TOOLS: &[ToolSpec] = &[
    ToolSpec {
        name: "zellij_overview",
        summary: "List what a zellij session contains: its panes, the agents running in them, or \
                  the sessions on this machine.",
        best_for: "the first call of any zellij task - it is how you learn the pane handles \
                   everything else is addressed by, and which panes are running a coding agent.",
        not_for: "reading what is ON a pane's screen, and not for watching a pane over time.",
        follow_up: Some(
            "zellij_read_pane to read one pane's screen, or zellij_wait_for to block until \
             something happens in it",
        ),
        reports: "action list-panes",
        tips: "scope=agents narrows the same walk to the panes running claude, opencode, codex or \
               pi, each with the harness's own session id where it exports one. scope=sessions \
               answers about the machine rather than about one session.",
        params: &[
            SESSION,
            ParamSpec {
                name: "scope",
                kind: ParamKind::Enum(&["panes", "agents", "sessions"]),
                required: false,
                from: None,
                describe: "What to list: every pane of the session, only the panes running a \
                           coding agent, or the sessions on this machine.",
                default: Some("panes"),
            },
            ParamSpec {
                name: "include_hidden",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action list-panes", "--all")),
                describe: "",
                default: None,
            },
        ],
        read_only: true,
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_read_pane",
        summary: "Read what is on a pane's screen right now.",
        best_for: "seeing the output of something a pane is running, including a pane in a tab \
                   nobody is looking at - the grid is kept whether or not the pane renders.",
        not_for: "waiting for output that has not arrived yet, and not for a pane you cannot \
                  name: there is no default pane here.",
        follow_up: Some("zellij_wait_for when the output you want is not there yet"),
        reports: "action dump-screen",
        tips: "",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action dump-screen", "--pane-id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "full",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action dump-screen", "--full")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "ansi",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action dump-screen", "--ansi")),
                describe: "",
                default: None,
            },
        ],
        read_only: true,
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_wait_for",
        summary: "Block until a pane's command exits, its output matches a pattern, or it falls \
                  silent.",
        best_for: "the step after starting something long: wait for it rather than reading the \
                   pane over and over.",
        not_for: "a pane that is already in the state you want - read it instead. This call \
                  blocks, so it is not free.",
        follow_up: Some("zellij_read_pane to read what the pane says once the wait returns"),
        reports: "action wait",
        tips: "A wait that times out is a miss, not an error, and says so. until=match needs a \
               pattern; until=quiet takes the window in quiet_ms. Every wait is bounded: without \
               timeout_s it gives up after 300 seconds rather than blocking for the life of the \
               pane.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action wait", "pane_id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "until",
                kind: ParamKind::Enum(&["exit", "match", "quiet"]),
                required: false,
                from: Some(("action wait", "--for")),
                describe: "",
                default: Some("exit"),
            },
            ParamSpec {
                name: "pattern",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action wait", "--match")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "quiet_ms",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action wait", "--quiet-ms")),
                describe: "",
                default: Some("500"),
            },
            ParamSpec {
                name: "timeout_s",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action wait", "--timeout")),
                describe: "",
                default: Some("300"),
            },
        ],
        read_only: true,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_write_input",
        summary: "Type into a named pane, as if at its keyboard.",
        best_for: "driving a program that is already running in a pane - answering a prompt, \
                   sending a line to a shell.",
        not_for: "starting something in a NEW pane, and not for a pane you have not named. There \
                  is no focused pane here: an unnamed target would be a pane you have never seen.",
        follow_up: Some("zellij_wait_for or zellij_read_pane to see what the pane did with it"),
        reports: "action send-keys",
        tips: "keys goes through the key parser, so `Enter`, `C-c` and `Escape` mean those keys; \
               text is written literally and presses nothing. Pass one or the other.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action send-keys", "--pane-id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "keys",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-keys", "keys")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "text",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action write-chars", "chars")),
                describe: "",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_create",
        summary: "Make a pane or a tab, optionally running a command in it.",
        best_for: "starting work somewhere new, when you need the handle of what you made in \
                   order to talk to it afterwards.",
        not_for: "running something in a pane that already exists - write to it instead.",
        follow_up: Some("zellij_write_input or zellij_wait_for, using the handle this returns"),
        reports: "action new-pane",
        tips: "The handle in the answer is the pane's address and survives a session restore; use \
               it, not the integer id. A session with no client attached cannot lay out a new tab, \
               and this reports that miss rather than returning a pane that does not exist.",
        params: &[
            SESSION,
            ParamSpec {
                name: "kind",
                kind: ParamKind::Enum(&["pane", "tab"]),
                required: false,
                from: None,
                describe: "Whether to make a pane in the current tab, or a whole new tab.",
                default: Some("pane"),
            },
            ParamSpec {
                name: "command",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "command")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "cwd",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--cwd")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "name",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--name")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "handle",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--handle")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "floating",
                kind: ParamKind::Bool,
                required: false,
                from: Some(("action new-pane", "--floating")),
                describe: "",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_arrange",
        summary: "Move, stack, break out or close a pane or a tab, always by explicit target.",
        best_for: "reshaping a session you already have an overview of.",
        not_for: "anything you cannot name a target for. Every operation here takes one, \
                  including from inside the session.",
        follow_up: Some("zellij_overview to see the shape the session ended up in"),
        reports: "action move-pane",
        tips: "close_pane and close_tab cannot be undone and are confirmed for a person; this \
               tool passes the confirmation for you, so treat them as final.",
        params: &[
            SESSION,
            ParamSpec {
                name: "operation",
                kind: ParamKind::Enum(&[
                    "move_pane",
                    "move_tab",
                    "stack_panes",
                    "break_pane",
                    "close_pane",
                    "close_tab",
                ]),
                required: true,
                from: None,
                describe: "What to do. move_pane and break_pane take pane; move_tab and close_tab \
                           take tab; stack_panes takes panes; close_pane takes pane.",
                default: None,
            },
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action move-pane", "--pane-id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "panes",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action stack-panes", "pane_ids")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "tab",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action move-tab", "--tab-id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "direction",
                kind: ParamKind::Enum(&["left", "right", "up", "down"]),
                required: false,
                from: Some(("action move-pane", "direction")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "to_index",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action move-tab", "--to-index")),
                describe: "",
                default: None,
            },
        ],
        read_only: false,
        destructive: true,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_snapshot",
        summary: "List, show or restore an archived session snapshot.",
        best_for: "bringing a session back after it was shut down, and for finding out what \
                   snapshots exist to bring back.",
        not_for: "saving one. A snapshot is written when a session is taken down, by the CLI, and \
                  session lifecycle is deliberately not reachable from here.",
        follow_up: Some("zellij_overview once a restore has rebuilt the session"),
        reports: "snapshot list",
        tips: "An id may be given as a unique prefix, and `latest` is a valid id for a restore.",
        params: &[
            ParamSpec {
                name: "operation",
                kind: ParamKind::Enum(&["list", "show", "restore"]),
                required: true,
                from: None,
                describe: "Whether to list the archive, print one snapshot's layout, or rebuild a \
                           session from one.",
                default: None,
            },
            ParamSpec {
                name: "id",
                kind: ParamKind::Str,
                required: false,
                from: Some(("snapshot show", "id")),
                describe: "",
                default: None,
            },
            ParamSpec {
                name: "session",
                kind: ParamKind::Str,
                required: false,
                from: Some(("snapshot list", "--session")),
                describe: "",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
];

/// The tool by that name, or `None`.
pub fn tool_spec(name: &str) -> Option<&'static ToolSpec> {
    TOOLS.iter().find(|tool| tool.name == name)
}

/// Every tool, as the protocol describes them.
pub fn tool_list() -> Vec<Tool> {
    TOOLS.iter().map(describe).collect()
}

/// One tool, description and schema alike.
fn describe(spec: &'static ToolSpec) -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(spec.name),
        Cow::Owned(description(spec)),
        Arc::new(input_schema(spec)),
    );
    tool.annotations = Some(
        ToolAnnotations::new()
            .read_only(spec.read_only)
            .destructive(spec.destructive)
            .idempotent(spec.idempotent)
            // every answer comes from a live session on this machine, which is state this server
            // does not own
            .open_world(true),
    );
    tool
}

/// The description a client routes on.
///
/// Four of the five lines are written in the table above. `Returns:` is the fifth, and it is
/// generated from the surface map so that it says what the command actually prints.
pub fn description(spec: &ToolSpec) -> String {
    let mut out = String::from(spec.summary);
    out.push_str("\n\n");
    out.push_str(&format!("Best for: {}\n", spec.best_for));
    out.push_str(&format!("Returns: {}\n", returns_line(spec.reports)));
    out.push_str(&format!("Not for: {}\n", spec.not_for));
    if !spec.tips.is_empty() {
        out.push_str(&format!("Notes: {}\n", spec.tips));
    }
    if let Some(follow_up) = spec.follow_up {
        out.push_str(&format!("Follow up with {}.\n", follow_up));
    }
    out
}

/// What a command puts out, said in a sentence, from the shape and keys the surface map records.
///
/// Not written by hand anywhere: a column added to a table appears here on the next build, and a
/// command whose row says it prints nothing says so rather than promising a payload.
pub fn returns_line(command: &str) -> String {
    let shape = cli_surface::promised_output_shape(command);
    let keys = cli_surface::promised_output_keys(command);
    match (shape, keys) {
        (Some("table"), Some(keys)) => format!(
            "a table, one row per result, with the columns {}. Structured as JSON where the \
             command offers it.",
            normalize(keys)
        ),
        (Some("record"), Some(keys)) => {
            format!("a record of {}.", normalize(keys))
        },
        (Some("outline"), Some(keys)) => format!("an indented outline of {}.", normalize(keys)),
        (Some("payload"), _) => "the payload itself, and nothing around it.".to_owned(),
        (Some(shape), _) => format!("a {}.", shape),
        (None, _) => {
            "nothing when it succeeds - the fork's convention for a command that only acts."
                .to_owned()
        },
    }
}

/// The keys of a row as written in the surface map, which wraps them across source lines.
fn normalize(keys: &str) -> String {
    keys.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tool's JSON Schema, with each property described by whatever describes the flag it stands
/// for.
pub fn input_schema(spec: &ToolSpec) -> JsonObject {
    let mut properties = Map::new();
    let mut required: Vec<Value> = Vec::new();
    for param in spec.params {
        properties.insert(param.name.to_owned(), property(param));
        if param.required {
            required.push(json!(param.name));
        }
    }
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), Value::Array(required));
    schema.insert("additionalProperties".to_owned(), json!(false));
    schema
}

fn property(param: &ParamSpec) -> Value {
    let mut property = Map::new();
    match param.kind {
        ParamKind::Str => {
            property.insert("type".to_owned(), json!("string"));
        },
        ParamKind::Bool => {
            property.insert("type".to_owned(), json!("boolean"));
        },
        ParamKind::Int => {
            property.insert("type".to_owned(), json!("integer"));
        },
        ParamKind::Enum(values) => {
            property.insert("type".to_owned(), json!("string"));
            property.insert("enum".to_owned(), json!(values));
        },
    }
    property.insert("description".to_owned(), json!(param_description(param)));
    if let Some(default) = param.default {
        // the table writes every default as a string, because that is what a command line takes.
        // An integer property whose `default` is a string is not valid against its own schema, so
        // it is put back into the type the property declares
        let default = match param.kind {
            ParamKind::Int => default
                .parse::<i64>()
                .map(|number| json!(number))
                .unwrap_or_else(|_| json!(default)),
            _ => json!(default),
        };
        property.insert("default".to_owned(), default);
    }
    Value::Object(property)
}

/// A property's description: clap's own help for the flag it stands for, or the hand-written line
/// for a property the CLI has no flag for.
pub fn param_description(param: &ParamSpec) -> String {
    match param.from {
        Some((command, arg)) => cli_surface::surface_command(command)
            .and_then(|command| command.arg(arg).map(|arg| arg.about.clone()))
            .filter(|about| !about.is_empty())
            // unreachable while `every_parameter_names_a_real_argument` passes; a sentence rather
            // than an empty description if it ever is not
            .unwrap_or_else(|| format!("As `zellij {} {}`.", command, arg)),
        None => param.describe.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_parameter_names_a_real_argument() {
        for tool in TOOLS {
            for param in tool.params {
                let Some((command, arg)) = param.from else {
                    continue;
                };
                let found = cli_surface::surface_command(command).unwrap_or_else(|| {
                    panic!(
                        "{} names `zellij {}`, which is not a command",
                        tool.name, command
                    )
                });
                assert!(
                    found.arg(arg).is_some(),
                    "{}'s `{}` names `{}` of `zellij {}`, which has no such argument",
                    tool.name,
                    param.name,
                    arg,
                    command,
                );
            }
        }
    }

    #[test]
    fn every_generated_description_is_the_flags_own_help() {
        for tool in TOOLS {
            for param in tool.params {
                let Some((command, arg)) = param.from else {
                    continue;
                };
                let expected = cli_surface::surface_command(command)
                    .and_then(|command| command.arg(arg).map(|arg| arg.about.clone()))
                    .expect("the argument exists");
                assert_eq!(
                    param_description(param),
                    expected,
                    "{}'s `{}` does not carry the help of `{}`",
                    tool.name,
                    param.name,
                    arg,
                );
                assert!(
                    !expected.is_empty(),
                    "`{}` of `zellij {}` has no help for {} to borrow",
                    arg,
                    command,
                    param.name,
                );
            }
        }
    }

    #[test]
    fn every_returns_line_is_the_surface_maps_own_keys() {
        for tool in TOOLS {
            let line = returns_line(tool.reports);
            let keys = cli_surface::promised_output_keys(tool.reports);
            assert!(
                cli_surface::surface_command(tool.reports).is_some(),
                "{} reports `zellij {}`, which is not a command",
                tool.name,
                tool.reports,
            );
            if cli_surface::promised_output_shape(tool.reports).is_none() {
                // a verb that only acts, and a Returns line that says exactly that
                assert!(line.starts_with("nothing when it succeeds"), "{}", line);
            }
            if let Some(keys) = keys {
                for key in keys.split_whitespace() {
                    assert!(
                        line.contains(key),
                        "{}'s Returns line drops the `{}` key of `zellij {}`",
                        tool.name,
                        key,
                        tool.reports,
                    );
                }
            }
        }
    }

    #[test]
    fn a_table_of_columns_is_reported_as_its_columns() {
        // the drift gate in miniature: `list-panes` gained an AGENT column, and the line says so
        // without anybody editing a description
        let line = returns_line("action list-panes");
        assert!(line.starts_with("a table"), "{}", line);
        assert!(line.contains("HANDLE"), "{}", line);
        assert!(line.contains("AGENT"), "{}", line);
    }

    #[test]
    fn a_command_that_prints_nothing_is_not_dressed_up_as_one_that_does() {
        let line = returns_line("action move-focus");
        assert!(line.starts_with("nothing when it succeeds"), "{}", line);
    }

    #[test]
    fn every_tool_says_what_it_is_not_for_and_what_comes_next() {
        for tool in TOOLS {
            let description = description(tool);
            assert!(
                description.contains("Best for:"),
                "{} has no Best for line",
                tool.name
            );
            assert!(
                description.contains("Not for:"),
                "{} has no Not for line",
                tool.name
            );
            assert!(
                description.contains("Returns:"),
                "{} has no Returns line",
                tool.name
            );
        }
    }

    #[test]
    fn the_surface_stays_small_and_the_names_do_not_collide() {
        assert!(
            TOOLS.len() <= 8,
            "the point of this server is a surface an agent can route through"
        );
        let mut names: Vec<&str> = TOOLS.iter().map(|tool| tool.name).collect();
        names.sort();
        let before = names.len();
        names.dedup();
        assert_eq!(before, names.len(), "two tools answer to the same name");
        for tool in TOOLS {
            assert!(
                tool.name.starts_with("zellij_"),
                "{} is not namespaced by its service",
                tool.name
            );
            assert!(
                tool.params.len() <= 8,
                "{} asks for more parameters than an agent can be expected to get right",
                tool.name
            );
        }
    }

    #[test]
    fn every_schema_is_an_object_that_lists_its_required_properties() {
        for tool in TOOLS {
            let schema = input_schema(tool);
            assert_eq!(schema.get("type"), Some(&json!("object")), "{}", tool.name);
            let properties = schema
                .get("properties")
                .and_then(|p| p.as_object())
                .expect("properties");
            let required = schema
                .get("required")
                .and_then(|r| r.as_array())
                .expect("required");
            for param in tool.params {
                assert!(properties.contains_key(param.name), "{}", param.name);
                assert_eq!(
                    required.contains(&json!(param.name)),
                    param.required,
                    "{} of {}",
                    param.name,
                    tool.name
                );
            }
        }
    }

    #[test]
    fn a_read_only_tool_is_not_also_a_destructive_one() {
        for tool in TOOLS {
            if tool.read_only {
                assert!(
                    !tool.destructive,
                    "{} claims to change nothing and to destroy something",
                    tool.name
                );
            }
        }
    }
}
