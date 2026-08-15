//! Turning a tool call into a `zellij` command line, and running it.
//!
//! Every tool runs the CLI rather than reaching into the client code beside it, and that is a
//! decision rather than laziness. The action path prints its answer to stdout and ends the process
//! on a miss. On a stdio MCP server stdout IS the protocol stream and the process is the session,
//! so a single missed pane would corrupt one and end the other. A child process gives clean
//! capture, and it maps the fork's exit convention straight onto the protocol:
//!
//! | exit | means | becomes |
//! |---|---|---|
//! | 0 | acted or found | a result |
//! | 1 | error | `isError`, with what the CLI said |
//! | 2 | miss - a well-formed request about something that is not there | `isError`, said as a miss |
//!
//! That is what makes an honest miss possible: a pane that was not created reports the CLI's own
//! refusal, and no tool here can invent a pane id the CLI never printed.
//!
//! The binary run is this one, found with `current_exe`, so the CLI a tool calls is always the
//! build the tool shipped in.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::{Map, Value};

/// How long `zellij_wait_for` waits when the caller did not say.
///
/// The same string the tool's `timeout_s` parameter documents as its default - one constant, so a
/// description that promises a bound and a command line that has none cannot drift apart.
pub const WAIT_TIMEOUT_DEFAULT_S: &str = "300";

/// What the CLI said, and how it ended.
pub struct Outcome {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Outcome {
    pub fn is_miss(&self) -> bool {
        self.code == 2
    }
    pub fn is_error(&self) -> bool {
        self.code != 0
    }
}

/// The zellij binary a tool should run: this one.
pub fn zellij_binary() -> Result<PathBuf, String> {
    std::env::current_exe()
        .map_err(|e| format!("This server could not find its own binary to run: {}", e))
}

/// Run the CLI and collect what it said. Never writes to this process's stdout.
///
/// `kill_on_drop` is what makes an abandoned call cost nothing. A tool call that blocks - `wait`
/// is the whole point of one - is a child process that outlives its caller if nobody kills it, and
/// a client that times out, disconnects or restarts drops the future without saying so. Dropping
/// the future now drops the child, at client cancellation and at shutdown alike: the runtime drops
/// its pending tasks when it goes, so an EOF on stdin reaps whatever was still in flight.
pub async fn run(argv: &[String]) -> Result<Outcome, String> {
    let binary = zellij_binary()?;
    let output = tokio::process::Command::new(&binary)
        .args(argv)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| {
            format!(
                "Could not run `{} {}`: {}",
                binary.display(),
                argv.join(" "),
                e
            )
        })?;
    Ok(Outcome {
        // a child killed by a signal has no code; that is a failure the caller should see rather
        // than a success with no output
        code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// The arguments of a tool call, read once so that a missing one is reported by name.
pub struct Args<'a> {
    values: &'a Map<String, Value>,
}

impl<'a> Args<'a> {
    pub fn new(values: &'a Map<String, Value>) -> Self {
        Args { values }
    }
    fn string(&self, name: &str) -> Option<String> {
        self.values.get(name).and_then(|value| match value {
            Value::String(text) if !text.is_empty() => Some(text.clone()),
            Value::Number(number) => Some(number.to_string()),
            _ => None,
        })
    }
    fn required(&self, name: &str) -> Result<String, String> {
        self.string(name)
            .ok_or_else(|| format!("`{}` is required and was not given.", name))
    }
    fn flag(&self, name: &str) -> bool {
        self.values
            .get(name)
            .and_then(|value| value.as_bool())
            .unwrap_or(false)
    }
    fn enumerated(&self, name: &str, default: &str) -> String {
        self.string(name).unwrap_or_else(|| default.to_owned())
    }
}

/// The command line a tool call becomes, after the binary's own name.
///
/// Pure: it reads the call and returns argv, so what every tool runs can be tested without a
/// session. `Err` is a call that could not be turned into one - a missing target, an operation
/// without the argument it needs - and is reported to the caller as a failed tool call rather than
/// guessed at.
pub fn argv(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
) -> Result<Vec<String>, String> {
    let args = Args::new(args);
    let session = args.string("session");
    // the session is named on the command line where the CLI takes it there, and passed as the
    // global `-s` everywhere else. `snapshot` is the exception: its `--session` means the name to
    // restore UNDER, not the session to ask, so the global flag would say something else
    let scoped = |rest: Vec<String>| -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(session) = session
            .clone()
            .or_else(|| ambient_session.map(String::from))
        {
            argv.push("-s".to_owned());
            argv.push(session);
        }
        argv.extend(rest);
        argv
    };

    match tool {
        "zellij_overview" => match args.enumerated("scope", "panes").as_str() {
            "sessions" => Ok(vec!["ls".to_owned(), "--json".to_owned()]),
            "agents" => Ok(scoped(vec![
                "action".to_owned(),
                "list-agents".to_owned(),
                "--json".to_owned(),
            ])),
            "panes" => {
                let mut rest = vec![
                    "action".to_owned(),
                    "list-panes".to_owned(),
                    "--json".to_owned(),
                ];
                if args.flag("include_hidden") {
                    rest.push("--all".to_owned());
                }
                Ok(scoped(rest))
            },
            other => Err(format!(
                "`scope` must be panes, agents or sessions, not `{}`.",
                other
            )),
        },
        "zellij_read_pane" => {
            let mut rest = vec![
                "action".to_owned(),
                "dump-screen".to_owned(),
                "--pane-id".to_owned(),
                args.required("pane")?,
            ];
            if args.flag("full") {
                rest.push("--full".to_owned());
            }
            if args.flag("ansi") {
                rest.push("--ansi".to_owned());
            }
            Ok(scoped(rest))
        },
        "zellij_wait_for" => {
            let until = args.enumerated("until", "exit");
            if !["exit", "match", "quiet"].contains(&until.as_str()) {
                return Err(format!(
                    "`until` must be exit, match or quiet, not `{}`.",
                    until
                ));
            }
            let mut rest = vec![
                "action".to_owned(),
                "wait".to_owned(),
                args.required("pane")?,
                "--for".to_owned(),
                until.clone(),
            ];
            if until == "match" {
                rest.push("--match".to_owned());
                rest.push(args.required("pattern").map_err(|_| {
                    "`until: match` needs a `pattern` to match against.".to_owned()
                })?);
            }
            if let Some(quiet_ms) = args.string("quiet_ms") {
                rest.push("--quiet-ms".to_owned());
                rest.push(quiet_ms);
            }
            // a wait with no timeout blocks until the pane does something, which for a pane that
            // never will is forever. The tool's own description has always named this default;
            // applying it here is what makes the two agree, and it is what stops an abandoned
            // call from holding a client connection to the session for the life of the pane
            rest.push("--timeout".to_owned());
            rest.push(
                args.string("timeout_s")
                    .unwrap_or_else(|| WAIT_TIMEOUT_DEFAULT_S.to_owned()),
            );
            Ok(scoped(rest))
        },
        "zellij_write_input" => {
            let pane = args.required("pane")?;
            match (args.string("keys"), args.string("text")) {
                (Some(_), Some(_)) => Err(
                    "Pass `keys` or `text`, not both: one presses keys and the other writes \
                     characters."
                        .to_owned(),
                ),
                (Some(keys), None) => Ok(scoped(vec![
                    "action".to_owned(),
                    "send-keys".to_owned(),
                    "--pane-id".to_owned(),
                    pane,
                    "--".to_owned(),
                    keys,
                ])),
                (None, Some(text)) => Ok(scoped(vec![
                    "action".to_owned(),
                    "write-chars".to_owned(),
                    "--pane-id".to_owned(),
                    pane,
                    "--".to_owned(),
                    text,
                ])),
                (None, None) => Err("Nothing to send: pass `keys` or `text`.".to_owned()),
            }
        },
        "zellij_create" => {
            let kind = args.enumerated("kind", "pane");
            let command = args.string("command");
            let mut rest = vec!["action".to_owned()];
            match kind.as_str() {
                "pane" => {
                    rest.push("new-pane".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push("--cwd".to_owned());
                        rest.push(cwd);
                    }
                    if let Some(name) = args.string("name") {
                        rest.push("--name".to_owned());
                        rest.push(name);
                    }
                    if let Some(handle) = args.string("handle") {
                        rest.push("--handle".to_owned());
                        rest.push(handle);
                    }
                    if args.flag("floating") {
                        rest.push("--floating".to_owned());
                    }
                    // `--` last, because everything after it is the command's own argv
                    if let Some(command) = command {
                        rest.push("--".to_owned());
                        rest.extend(split_command(&command));
                    }
                },
                "tab" => {
                    rest.push("new-tab".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push("--cwd".to_owned());
                        rest.push(cwd);
                    }
                    if let Some(name) = args.string("name") {
                        rest.push("--name".to_owned());
                        rest.push(name);
                    }
                    if args.string("handle").is_some() {
                        return Err(
                            "`handle` names a pane, and a tab is not one. Make the tab, then read \
                             back the handle of the pane it came with."
                                .to_owned(),
                        );
                    }
                    if args.flag("floating") {
                        return Err("A tab cannot float; `floating` names a pane.".to_owned());
                    }
                    if let Some(command) = command {
                        rest.push("--".to_owned());
                        rest.extend(split_command(&command));
                    }
                },
                other => {
                    return Err(format!("`kind` must be pane or tab, not `{}`.", other));
                },
            }
            Ok(scoped(rest))
        },
        "zellij_arrange" => {
            let operation = args.required("operation")?;
            let rest = match operation.as_str() {
                "move_pane" => {
                    let mut rest = vec![
                        "action".to_owned(),
                        "move-pane".to_owned(),
                        "--pane-id".to_owned(),
                        args.required("pane")?,
                    ];
                    if let Some(direction) = args.string("direction") {
                        rest.push(direction);
                    }
                    rest
                },
                "move_tab" => {
                    let mut rest = vec![
                        "action".to_owned(),
                        "move-tab".to_owned(),
                        "--tab-id".to_owned(),
                        args.required("tab")?,
                    ];
                    match (args.string("to_index"), args.string("direction")) {
                        (Some(to_index), _) => {
                            rest.push("--to-index".to_owned());
                            rest.push(to_index);
                        },
                        (None, Some(direction)) => rest.push(direction),
                        (None, None) => {
                            return Err(
                                "`move_tab` needs somewhere to move to: pass `to_index` or \
                                 `direction`."
                                    .to_owned(),
                            )
                        },
                    }
                    rest
                },
                "stack_panes" => {
                    let panes = args.required("panes").map_err(|_| {
                        "`stack_panes` needs `panes`: the panes to stack, space separated."
                            .to_owned()
                    })?;
                    let mut rest = vec![
                        "action".to_owned(),
                        "stack-panes".to_owned(),
                        "--".to_owned(),
                    ];
                    rest.extend(panes.split_whitespace().map(|pane| pane.to_owned()));
                    rest
                },
                "break_pane" => vec![
                    "action".to_owned(),
                    "break-pane".to_owned(),
                    "--pane-id".to_owned(),
                    args.required("pane")?,
                ],
                // the two that cannot be undone. The CLI confirms for a person and refuses off a
                // terminal; a tool call has already been approved by whatever gates tool calls, so
                // it answers the confirmation here rather than hanging on a prompt nobody can see
                "close_pane" => vec![
                    "action".to_owned(),
                    "close-pane".to_owned(),
                    "--pane-id".to_owned(),
                    args.required("pane")?,
                    "--yes".to_owned(),
                ],
                "close_tab" => vec![
                    "action".to_owned(),
                    "close-tab-by-id".to_owned(),
                    "--tab-id".to_owned(),
                    args.required("tab")?,
                    "--yes".to_owned(),
                ],
                other => return Err(format!("`{}` is not an operation of this tool.", other)),
            };
            Ok(scoped(rest))
        },
        "zellij_snapshot" => {
            let operation = args.required("operation")?;
            match operation.as_str() {
                "list" => {
                    let mut argv = vec![
                        "snapshot".to_owned(),
                        "list".to_owned(),
                        "--json".to_owned(),
                    ];
                    if let Some(session) = session {
                        argv.push("--session".to_owned());
                        argv.push(session);
                    }
                    Ok(argv)
                },
                "show" => Ok(vec![
                    "snapshot".to_owned(),
                    "show".to_owned(),
                    args.required("id")?,
                ]),
                "restore" => {
                    let mut argv = vec![
                        "snapshot".to_owned(),
                        "restore".to_owned(),
                        args.required("id")?,
                    ];
                    if let Some(session) = session {
                        argv.push("--session".to_owned());
                        argv.push(session);
                    }
                    Ok(argv)
                },
                other => Err(format!(
                    "`operation` must be list, show or restore, not `{}`.",
                    other
                )),
            }
        },
        other => Err(format!("`{}` is not a tool of this server.", other)),
    }
}

/// A command given as one string, split the way a shell would split a simple one.
///
/// Whitespace only: it is passed as argv and never to a shell, so quoting, globs and pipes mean
/// nothing here. Saying so in the tool's own description is cheaper than pretending otherwise.
fn split_command(command: &str) -> Vec<String> {
    command
        .split_whitespace()
        .map(|word| word.to_owned())
        .collect()
}

/// The session an unqualified tool call is about: whatever this server was started inside.
pub fn ambient_session() -> Option<String> {
    std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .filter(|name| !name.is_empty())
}

/// The environment a tool call reports about itself, for the structured result.
pub fn call_context(argv: &[String]) -> BTreeMap<String, String> {
    let mut context = BTreeMap::new();
    context.insert("command".to_owned(), format!("zellij {}", argv.join(" ")));
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn args(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    fn line(tool: &str, value: Value, session: Option<&str>) -> String {
        argv(tool, &args(value), session)
            .expect("a call that builds")
            .join(" ")
    }

    fn refusal(tool: &str, value: Value) -> String {
        argv(tool, &args(value), None).expect_err("a call that cannot be built")
    }

    #[test]
    fn the_ambient_session_is_used_when_the_call_does_not_name_one() {
        assert_eq!(
            line("zellij_overview", json!({}), Some("work")),
            "-s work action list-panes --json"
        );
    }

    #[test]
    fn a_named_session_beats_the_ambient_one() {
        assert_eq!(
            line("zellij_overview", json!({"session": "other"}), Some("work")),
            "-s other action list-panes --json"
        );
    }

    #[test]
    fn a_call_with_no_session_anywhere_lets_the_cli_resolve_it() {
        assert_eq!(
            line("zellij_overview", json!({}), None),
            "action list-panes --json"
        );
    }

    #[test]
    fn the_agent_scope_is_the_association_verb() {
        assert_eq!(
            line("zellij_overview", json!({"scope": "agents"}), Some("work")),
            "-s work action list-agents --json"
        );
    }

    #[test]
    fn the_session_scope_asks_about_the_machine_and_not_about_a_session() {
        // `-s` would be asking one session to list them all
        assert_eq!(
            line(
                "zellij_overview",
                json!({"scope": "sessions"}),
                Some("work")
            ),
            "ls --json"
        );
    }

    #[test]
    fn reading_a_pane_names_the_pane() {
        assert_eq!(
            line(
                "zellij_read_pane",
                json!({"pane": "sunny-otter", "full": true}),
                Some("work")
            ),
            "-s work action dump-screen --pane-id sunny-otter --full"
        );
    }

    #[test]
    fn a_pane_is_never_implied() {
        assert!(refusal("zellij_read_pane", json!({})).contains("`pane` is required"));
        assert!(refusal("zellij_write_input", json!({"keys": "Enter"})).contains("`pane`"));
    }

    #[test]
    fn keys_and_text_are_different_verbs() {
        assert_eq!(
            line(
                "zellij_write_input",
                json!({"pane": "3", "keys": "C-c"}),
                None
            ),
            "action send-keys --pane-id 3 -- C-c"
        );
        assert_eq!(
            line(
                "zellij_write_input",
                json!({"pane": "3", "text": "hello"}),
                None
            ),
            "action write-chars --pane-id 3 -- hello"
        );
    }

    #[test]
    fn sending_neither_or_both_is_refused_rather_than_guessed() {
        assert!(refusal("zellij_write_input", json!({"pane": "3"})).contains("Nothing to send"));
        assert!(refusal(
            "zellij_write_input",
            json!({"pane": "3", "keys": "Enter", "text": "hi"})
        )
        .contains("not both"));
    }

    #[test]
    fn a_matched_wait_needs_something_to_match() {
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"pane": "3", "until": "match", "pattern": "done"}),
                None
            ),
            "action wait 3 --for match --match done --timeout 300"
        );
        assert!(
            refusal("zellij_wait_for", json!({"pane": "3", "until": "match"}))
                .contains("needs a `pattern`")
        );
    }

    #[test]
    fn a_wait_defaults_to_the_panes_command_exiting() {
        assert_eq!(
            line("zellij_wait_for", json!({"pane": "3"}), None),
            "action wait 3 --for exit --timeout 300"
        );
    }

    #[test]
    fn a_wait_is_always_bounded_and_the_bound_is_the_one_advertised() {
        // an unbounded wait is a child process that outlives the client that asked for it. The
        // default is the one the tool's own parameter description promises
        assert_eq!(
            crate::mcp::tools::tool_spec("zellij_wait_for")
                .and_then(|spec| spec.params.iter().find(|p| p.name == "timeout_s"))
                .and_then(|param| param.default),
            Some(WAIT_TIMEOUT_DEFAULT_S)
        );
        assert!(line("zellij_wait_for", json!({"pane": "3"}), None).contains("--timeout 300"));
        assert!(line(
            "zellij_wait_for",
            json!({"pane": "3", "timeout_s": 5}),
            None
        )
        .contains("--timeout 5"));
    }

    #[test]
    fn a_created_panes_command_comes_after_the_double_dash() {
        assert_eq!(
            line(
                "zellij_create",
                json!({"command": "cargo test", "handle": "test-run"}),
                Some("work")
            ),
            "-s work action new-pane --handle test-run -- cargo test"
        );
    }

    #[test]
    fn a_tab_is_not_given_a_panes_arguments() {
        assert!(
            refusal("zellij_create", json!({"kind": "tab", "handle": "x"}))
                .contains("names a pane")
        );
        assert!(
            refusal("zellij_create", json!({"kind": "tab", "floating": true}))
                .contains("cannot float")
        );
    }

    #[test]
    fn what_cannot_be_undone_answers_the_confirmation_it_would_otherwise_hang_on() {
        assert_eq!(
            line(
                "zellij_arrange",
                json!({"operation": "close_pane", "pane": "sunny-otter"}),
                None
            ),
            "action close-pane --pane-id sunny-otter --yes"
        );
        assert_eq!(
            line(
                "zellij_arrange",
                json!({"operation": "close_tab", "tab": 2}),
                None
            ),
            "action close-tab-by-id --tab-id 2 --yes"
        );
    }

    #[test]
    fn a_structural_move_names_its_target_and_where_it_is_going() {
        assert_eq!(
            line(
                "zellij_arrange",
                json!({"operation": "move_tab", "tab": 1, "to_index": 3}),
                None
            ),
            "action move-tab --tab-id 1 --to-index 3"
        );
        assert!(
            refusal("zellij_arrange", json!({"operation": "move_tab", "tab": 1}))
                .contains("needs somewhere to move to")
        );
    }

    #[test]
    fn stacking_takes_the_panes_after_the_double_dash() {
        assert_eq!(
            line(
                "zellij_arrange",
                json!({"operation": "stack_panes", "panes": "3 4 5"}),
                None
            ),
            "action stack-panes -- 3 4 5"
        );
    }

    #[test]
    fn a_snapshot_call_is_not_scoped_to_a_session_the_way_the_others_are() {
        // `snapshot --session` names what to restore UNDER, so the global `-s` would say something
        // else entirely
        assert_eq!(
            line(
                "zellij_snapshot",
                json!({"operation": "list"}),
                Some("work")
            ),
            "snapshot list --json"
        );
        assert_eq!(
            line(
                "zellij_snapshot",
                json!({"operation": "restore", "id": "latest", "session": "revived"}),
                Some("work")
            ),
            "snapshot restore latest --session revived"
        );
    }

    #[test]
    fn saving_a_snapshot_is_not_offered_because_the_cli_does_not_offer_it() {
        assert!(refusal("zellij_snapshot", json!({"operation": "save"}))
            .contains("must be list, show or restore"));
    }

    #[test]
    fn a_tool_this_server_does_not_have_is_refused_by_name() {
        assert!(refusal("zellij_kill_session", json!({})).contains("is not a tool of this server"));
    }

    #[test]
    fn every_tool_in_the_table_can_be_called() {
        for tool in crate::mcp::tools::TOOLS {
            let refused = argv(tool.name, &Map::new(), None);
            // either it builds with no arguments, or it says which argument it wanted - never
            // "not a tool of this server", which would mean the table and this dispatch disagree
            if let Err(message) = refused {
                assert!(
                    !message.contains("is not a tool of this server"),
                    "{} is in the tool table and not in the dispatch",
                    tool.name
                );
            }
        }
    }
}
