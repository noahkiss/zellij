//! The eight tools, and the half of each one that is generated rather than written.
//!
//! A tool's description is routing logic: it decides whether the tool is called at all, so the
//! part that says what the tool is FOR is written by hand and kept blunt. The part that says what
//! comes back is not written here at all - it is generated from the same `OUTPUTS` table
//! `zellij setup --dump-surface` reads, and so is every input-schema property that stands for a
//! real CLI flag. A flag renamed in `cli.rs` therefore cannot leave a stale description behind:
//! `every_parameter_names_a_real_argument` fails the build instead.
//!
//! Eight, not one per verb. `zellij action` has eighty-seven of those, and a surface nobody can
//! route through is worse than a small one that names its own follow-ups.

use std::borrow::Cow;
use std::sync::Arc;

use rmcp::model::{JsonObject, Tool, ToolAnnotations};
use serde_json::{json, Map, Value};
use zellij_utils::cli_surface;

use super::invoke;

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
    /// A renamed flag then fails the build rather than leaving a stale property behind. `None` for
    /// a property the tool invents.
    pub from: Option<(&'static str, &'static str)>,
    /// What this property is, in the caller's terms.
    ///
    /// Clap's help is the fallback rather than the rule: it is written for a command line, and a
    /// line that says "after a `--`", "one per argument", "pass `-` for stdin" or "without this,
    /// the focused pane" describes an argv this caller never writes. So a non-empty `describe`
    /// wins over the inherited help, and `from` goes on standing guard over the flag's name.
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
    /// The CLI commands whose printed output this tool returns. The `Returns:` line is generated
    /// from their rows in the surface map, so it cannot promise a key nothing prints.
    ///
    /// A list, because a tool that multiplexes several operations returns a different shape for
    /// each of them, and one command's row cannot speak for the others. One entry is the ordinary
    /// case; several make the line say which operation returns what.
    pub reports: &'static [&'static str],
    /// Anything the caller has to know that the generated half cannot say - a discipline the CLI
    /// enforces, a value that has to be named explicitly. Empty for a tool with none.
    pub tips: &'static str,
    pub params: &'static [ParamSpec],
    pub read_only: bool,
    /// Whether a call can destroy something a caller cannot get back. Only meaningful when
    /// `read_only` is false.
    pub destructive: bool,
    /// Whether repeating a call leaves the session as one call would. A read-only tool changes
    /// nothing, so it is always true for one of those - the hint only carries information for a
    /// tool that writes.
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
        // one row per scope: `panes` walks list-panes, `agents` walks list-agents and `sessions`
        // asks the machine with `ls`. One entry promised the pane columns for all three
        reports: &["action list-panes", "action list-agents", "list-sessions"],
        tips: "Call this before zellij_create: a pane you already own is cheaper than a new one, \
               and its handle is in this answer. scope=agents narrows the same walk to the panes \
               running claude, opencode, codex or pi, each with the harness's own session id where \
               it exports one. scope=sessions answers about the machine rather than about one \
               session. A `withheld` count above zero means the session keeps some panes to \
               itself: they are not in this answer, no tool here will reach them, and there is \
               nothing to ask for.",
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
        reports: &["action dump-screen"],
        tips: "This is the grid, not the frame. A pane held open after its command exited shows \
               `[ EXIT CODE: n ]` in its border, and that line is NOT in this answer. Use \
               zellij_wait_for with until=exit for a command's status.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action dump-screen", "--pane-id")),
                describe: "The pane to read: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid. zellij_overview prints every \
                           one of them.",
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
        reports: &["action wait"],
        tips:
            "The exit_code of THIS call says whether the wait succeeded, not whether the command \
               did. A command that failed still returns exit_code 0 here: read `exit_status` in \
               the result. A wait that times out is a miss, not an error, and says so. until=match \
               needs a pattern; until=quiet takes the window in quiet_ms. until=match sees lines \
               that arrive AFTER the wait began, so anchor on a short string rather than on a line \
               that may wrap. until=exit on a plain shell pane waits for the shell, which is a \
               timeout. Every wait is bounded: without timeout_s it gives up after 300 seconds \
               rather than blocking for the life of the pane.",
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
                describe: "The regex a delivered line must match, when until=match. Rust regex \
                           syntax, unanchored.",
                default: None,
            },
            ParamSpec {
                name: "quiet_ms",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action wait", "--quiet-ms")),
                describe: "How long a pane must produce nothing to count as quiet, when \
                           until=quiet.",
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
        // `idempotentHint` describes what repeating a call does to the session, and a read-only
        // call does nothing to it. Saying `false` here while the other read-only tool says `true`
        // was noise a client could route on
        destructive: false,
        idempotent: true,
    },
    ToolSpec {
        name: "zellij_write_input",
        summary: "Type into a named pane, as if at its keyboard.",
        best_for: "driving a program that is already running in a pane - answering a prompt, \
                   sending a line to a shell.",
        not_for: "starting something in a NEW pane, and not for a pane you have not named. There \
                  is no focused pane here: an unnamed target would be a pane you have never seen.",
        follow_up: Some("zellij_wait_for or zellij_read_pane to see what the pane did with it"),
        // `keys` presses keys and `text` writes characters, which are two verbs, not one with a
        // flag: a tool that multiplexes says what each of its commands returns
        reports: &["action send-keys", "action write-chars"],
        tips: "keys goes through the key parser, so `Enter`, `C-c` and `Escape` mean those keys; \
               text is written literally and presses nothing. Pass one or the other. This is the \
               cheap way to do more work in a pane you already made: reach for it before \
               zellij_create.",
        params: &[
            SESSION,
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: true,
                from: Some(("action send-keys", "--pane-id")),
                describe: "The pane to type into: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid. Required - there is no focused \
                           pane here.",
                default: None,
            },
            ParamSpec {
                name: "keys",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action send-keys", "keys")),
                describe: "The keys to press, space separated, each a modifier chain: `Enter`, \
                           `C-c`, `Ctrl a`, `F1`.",
                default: None,
            },
            ParamSpec {
                name: "text",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action write-chars", "chars")),
                describe:
                    "The text to write literally. It presses nothing: add a second call with \
                           keys `Enter` to submit it.",
                default: None,
            },
        ],
        read_only: false,
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_create",
        summary: "Make a pane to work in, optionally running a command in it.",
        best_for: "starting work when you have no pane of your own yet. One pane per job is the \
                   norm; after that, write to the one you have.",
        not_for:
            "running something in a pane that already exists - write to it instead. Not for a \
                  second pane when your first one is idle, and not for replacing a pane whose \
                  command failed: reuse it, or close it.",
        follow_up: Some("zellij_write_input or zellij_wait_for, using the handle this returns"),
        // `kind: tab` runs `new-tab`, which prints the same three keys today and need not keep
        // doing so. A tool that multiplexes says what each of its commands returns
        reports: &["action new-pane", "action new-tab"],
        tips:
            "Success here means the pane was MADE, not that the command worked: exit_code 0 says \
               nothing about what ran in it. Follow with zellij_wait_for until=exit and read \
               `exit_status`. The handle in the answer is the pane's address and survives a \
               session restore; use it, not the integer id, and pass `handle` yourself when you \
               want to find the pane again by a name you chose.",
        params: &[
            SESSION,
            ParamSpec {
                name: "kind",
                kind: ParamKind::Enum(&["agent_tab", "pane", "tab"]),
                required: false,
                from: None,
                describe: "Where the pane goes. agent_tab, the default, is your own tab - the tab \
                           you are in with `-zj` on the end, made once, reused after, and it does \
                           not take the person's focus. It is named after the tab this server's \
                           own pane is in, even when `session` names a different session. Use it \
                           unless you have a reason not to. pane splits your own pane where you \
                           are. tab makes a fresh tab of its own.",
                default: Some(invoke::CREATE_KIND_DEFAULT),
            },
            ParamSpec {
                name: "command",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "command")),
                describe: "The command to run in the pane, through your shell: quotes, pipes, \
                           `&&`, `~` and `$VAR` all work. Without one the pane runs your shell.",
                default: None,
            },
            ParamSpec {
                name: "cwd",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--cwd")),
                describe: "The directory the pane starts in. `~`, `$VAR` and a relative path are \
                           expanded against this server's own environment; a path that is not a \
                           directory fails the call rather than being ignored.",
                default: None,
            },
            ParamSpec {
                name: "name",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action new-pane", "--name")),
                describe:
                    "What to call the pane, drawn on its frame. Without one a pane running a \
                           command is named after the command.",
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
        summary: "Move, stack or break out a pane or a tab, always by explicit target.",
        best_for: "reshaping a session you already have an overview of. Nothing here loses \
                   anything: every operation can be moved back.",
        not_for: "closing anything - that is zellij_close. Not for anything you cannot name a \
                  target for, either: every operation here takes one.",
        follow_up: Some("zellij_overview to see the shape the session ended up in"),
        reports: &[
            "action move-pane",
            "action move-tab",
            "action stack-panes",
            "action break-pane",
        ],
        tips: "",
        params: &[
            SESSION,
            ParamSpec {
                name: "operation",
                kind: ParamKind::Enum(&["move_pane", "move_tab", "stack_panes", "break_pane"]),
                required: true,
                from: None,
                describe: "What to do. move_pane and break_pane take pane; move_tab takes tab; \
                           stack_panes takes panes.",
                default: None,
            },
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action move-pane", "--pane-id")),
                describe: "The pane to move or break out: its handle, like sunny-otter, or \
                           terminal_1, plugin_2, a bare integer or a pane uuid.",
                default: None,
            },
            ParamSpec {
                name: "panes",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action stack-panes", "pane_ids")),
                describe: "The panes to stack, space separated: handles, terminal_1, plugin_2, \
                           bare integers or pane uuids.",
                default: None,
            },
            ParamSpec {
                name: "tab",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action move-tab", "--tab-id")),
                describe: "The tab to move, by the stable id zellij_overview prints - not its \
                           1-based display position.",
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
        // every operation here is reversible; the two that are not moved out to `zellij_close`, so
        // that a client can allow a pane to be moved without allowing a tab to be closed
        destructive: false,
        idempotent: false,
    },
    ToolSpec {
        name: "zellij_close",
        summary: "Close a pane or a tab. This cannot be undone.",
        best_for: "clearing away a pane you are done with, or one whose command failed - close it \
                   rather than leaving it and making another.",
        not_for: "moving or restacking anything, which is zellij_arrange, and not for a pane \
                  somebody else made. Closing a tab closes every pane in it.",
        follow_up: Some("zellij_overview to see what the session has left"),
        reports: &["action close-pane", "action close-tab-by-id"],
        tips: "close_pane passes the `--yes` a person at a terminal would have to type. close_tab \
               has no confirmation to pass and takes every pane in the tab with it. Both are \
               final. Closing a pane whose command failed is the right move - do not leave it \
               behind and create a second one.",
        params: &[
            SESSION,
            ParamSpec {
                name: "operation",
                kind: ParamKind::Enum(&["close_pane", "close_tab"]),
                required: true,
                from: None,
                describe: "What to close. close_pane takes pane; close_tab takes tab.",
                default: None,
            },
            ParamSpec {
                name: "pane",
                kind: ParamKind::Str,
                required: false,
                from: Some(("action close-pane", "--pane-id")),
                describe: "The pane to close: its handle, like sunny-otter, or terminal_1, \
                           plugin_2, a bare integer or a pane uuid.",
                default: None,
            },
            ParamSpec {
                name: "tab",
                kind: ParamKind::Int,
                required: false,
                from: Some(("action close-tab-by-id", "id")),
                describe: "The tab to close, by the stable id zellij_overview prints - not its \
                           1-based display position.",
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
        reports: &["snapshot list", "snapshot show", "snapshot restore"],
        tips: "An id may be given as a unique prefix, and `latest` is a valid id for a restore. \
               list and show only read; restore rebuilds a whole session, which is why this tool \
               is marked destructive.",
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
                name: "of_session",
                kind: ParamKind::Str,
                required: false,
                from: Some(("snapshot list", "--session")),
                describe: "Which session's snapshots: the name they were taken of, and the name a \
                           restore brings back under. Unlike `session` on the other tools, it does \
                           not choose which session to talk to.",
                default: None,
            },
        ],
        read_only: false,
        destructive: true,
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

/// The shape of every tool's `structuredContent`, which is the same for all seven.
///
/// One schema rather than seven, because every tool is the same child process reported the same
/// way: how the CLI exited, what it printed - parsed when it printed JSON, carried as lines when
/// it did not - and, on a failure, whether it was a miss or an error. The per-tool part is the
/// `result` payload, whose shape the `Returns:` line already describes in the words the surface
/// map uses, so pinning it here would be a second copy of it to keep in step.
fn output_schema() -> JsonObject {
    let mut properties = Map::new();
    properties.insert(
        "exit_code".to_owned(),
        json!({
            "type": "integer",
            "description": "The CLI's exit status: 0 acted or found, 1 an error, 2 a miss - a \
                            well-formed request about something that is not there.",
        }),
    );
    properties.insert(
        "result".to_owned(),
        json!({
            "description": "What the command printed, when it printed JSON. Its shape is the one \
                            the tool's Returns line describes.",
        }),
    );
    properties.insert(
        "output".to_owned(),
        json!({
            "type": "string",
            "description": "What the command printed, when it did not print JSON.",
        }),
    );
    properties.insert(
        "diagnostics".to_owned(),
        json!({
            "type": "string",
            "description": "Anything the command wrote to stderr.",
        }),
    );
    properties.insert(
        "reason".to_owned(),
        json!({
            "type": "string",
            "enum": ["miss", "error", "bad_arguments", "not_run"],
            "description": "Present only on a failed call, saying which kind it was.",
        }),
    );
    let mut schema = Map::new();
    schema.insert("type".to_owned(), json!("object"));
    schema.insert("properties".to_owned(), Value::Object(properties));
    schema.insert("required".to_owned(), json!([]));
    schema
}

/// One tool, description and schema alike.
fn describe(spec: &'static ToolSpec) -> Tool {
    let mut tool = Tool::new(
        Cow::Borrowed(spec.name),
        Cow::Owned(description(spec)),
        Arc::new(input_schema(spec)),
    )
    .with_raw_output_schema(Arc::new(output_schema()));
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
    out.push_str(&format!("Returns: {}\n", returns_for(spec)));
    out.push_str(&format!("Not for: {}\n", spec.not_for));
    if !spec.tips.is_empty() {
        out.push_str(&format!("Notes: {}\n", spec.tips));
    }
    if let Some(follow_up) = spec.follow_up {
        out.push_str(&format!("Follow up with {}.\n", follow_up));
    }
    out
}

/// What a tool puts out, across every operation it multiplexes.
///
/// One command gets one sentence. Several get one sentence per distinct answer, named by the
/// commands that give it, because a tool whose `restore` rebuilds a session and whose `list` prints
/// a table cannot honestly promise the table for both - and a client that was promised columns and
/// handed a payload has been lied to by the description it routed on.
pub fn returns_for(spec: &ToolSpec) -> String {
    match spec.reports {
        [] => "nothing.".to_owned(),
        [only] => returns_line(only),
        many => {
            // commands that answer the same way are said once, together. Four verbs that each
            // print nothing used to spend four sentences saying so, in a line a client routes on
            let mut said: Vec<(String, Vec<&str>)> = Vec::new();
            for command in many {
                let line = returns_line(command);
                match said.iter_mut().find(|(seen, _)| *seen == line) {
                    Some((_, commands)) => commands.push(command),
                    None => said.push((line, vec![command])),
                }
            }
            let mut out = String::from("it depends on the operation.");
            for (line, commands) in said {
                let names = commands
                    .iter()
                    .map(|command| format!("`zellij {}`", command))
                    .collect::<Vec<_>>()
                    .join(", ");
                let verb = if commands.len() == 1 {
                    "returns"
                } else {
                    "return"
                };
                out.push_str(&format!(" {} {} {}", names, verb, line));
            }
            out
        },
    }
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
    if !param.describe.is_empty() {
        return param.describe.to_owned();
    }
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
                assert!(
                    !expected.is_empty(),
                    "`{}` of `zellij {}` has no help for {} to borrow",
                    arg,
                    command,
                    param.name,
                );
                if !param.describe.is_empty() {
                    // a property that says what it is in the caller's terms keeps saying it; the
                    // flag it names is still checked, above and in the test before this one
                    continue;
                }
                assert_eq!(
                    param_description(param),
                    expected,
                    "{}'s `{}` does not carry the help of `{}`",
                    tool.name,
                    param.name,
                    arg,
                );
            }
        }
    }

    #[test]
    fn a_property_written_for_the_caller_beats_the_help_written_for_a_command_line() {
        // clap's help describes an argv: `--` positions, `-` for stdin, "the focused pane". None of
        // those exist for a caller passing JSON, and every one of them was in the rendered schema
        for (tool, param) in [
            ("zellij_write_input", "keys"),
            ("zellij_write_input", "text"),
            ("zellij_read_pane", "pane"),
            ("zellij_create", "command"),
            ("zellij_arrange", "panes"),
        ] {
            let spec = tool_spec(tool).expect("a tool");
            let param = spec
                .params
                .iter()
                .find(|candidate| candidate.name == param)
                .expect("a parameter");
            let description = param_description(param);
            assert_eq!(description, param.describe, "{} of {}", param.name, tool);
            for argv_ism in ["`--`", "after a --", "stdin", "--focused", "see above"] {
                assert!(
                    !description.contains(argv_ism),
                    "{} of {} still says `{}`",
                    param.name,
                    tool,
                    argv_ism,
                );
            }
        }
    }

    #[test]
    fn every_returns_line_is_the_surface_maps_own_keys() {
        for tool in TOOLS {
            assert!(!tool.reports.is_empty(), "{} reports nothing", tool.name);
            let spec_line = returns_for(tool);
            for command in tool.reports {
                let line = returns_line(command);
                assert!(
                    cli_surface::surface_command(command).is_some(),
                    "{} reports `zellij {}`, which is not a command",
                    tool.name,
                    command,
                );
                if cli_surface::promised_output_shape(command).is_none() {
                    // a verb that only acts, and a Returns line that says exactly that
                    assert!(line.starts_with("nothing when it succeeds"), "{}", line);
                }
                if let Some(keys) = cli_surface::promised_output_keys(command) {
                    for key in keys.split_whitespace() {
                        assert!(
                            line.contains(key),
                            "{}'s Returns line drops the `{}` key of `zellij {}`",
                            tool.name,
                            key,
                            command,
                        );
                    }
                }
                // and every operation's shape reaches the description the client routes on
                assert!(
                    spec_line.contains(&line),
                    "{}'s description drops what `zellij {}` returns",
                    tool.name,
                    command,
                );
            }
        }
    }

    #[test]
    fn a_tool_that_multiplexes_says_what_each_operation_returns() {
        // `snapshot list` prints a table and `snapshot show` prints a payload; promising the
        // table for both is the kind of lie a client cannot detect
        let snapshot = tool_spec("zellij_snapshot").expect("a tool");
        let line = returns_for(snapshot);
        assert!(line.starts_with("it depends on the operation"), "{}", line);
        assert!(line.contains("snapshot list"), "{}", line);
        assert!(line.contains("the payload itself"), "{}", line);
    }

    #[test]
    fn a_read_only_tool_is_idempotent_and_a_multiplexed_one_owns_its_worst_operation() {
        for tool in TOOLS {
            if tool.read_only {
                assert!(
                    tool.idempotent,
                    "{} reads only, so repeating it cannot change anything",
                    tool.name
                );
            }
        }
        // restore rebuilds a whole session, so the tool that offers it is destructive whatever its
        // other two operations do
        assert!(tool_spec("zellij_snapshot").expect("a tool").destructive);
    }

    #[test]
    fn what_can_be_undone_is_gated_apart_from_what_cannot() {
        // the reason this server exists is that a client gates verbs one by one. Six operations
        // under one destructive name meant allowing `move_pane` allowed `close_tab` with it
        let arrange = tool_spec("zellij_arrange").expect("a tool");
        assert!(
            !arrange.destructive,
            "every arrange operation is reversible"
        );
        let close = tool_spec("zellij_close").expect("a tool");
        assert!(close.destructive, "closing cannot be undone");
        for spec in [arrange, close] {
            for param in spec.params {
                let ParamKind::Enum(operations) = param.kind else {
                    continue;
                };
                for operation in operations {
                    assert_eq!(
                        operation.starts_with("close_"),
                        spec.name == "zellij_close",
                        "{} offers {}",
                        spec.name,
                        operation,
                    );
                }
            }
        }
    }

    #[test]
    fn every_tool_declares_the_shape_of_what_it_returns() {
        for tool in tool_list() {
            let schema = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(schema.get("type"), Some(&json!("object")));
            let properties = schema
                .get("properties")
                .and_then(|properties| properties.as_object())
                .expect("properties");
            assert!(properties.contains_key("exit_code"), "{}", tool.name);
            assert!(properties.contains_key("result"), "{}", tool.name);
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
