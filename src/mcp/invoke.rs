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
use std::path::{Path, PathBuf};

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

/// Where `zellij_create` puts a pane when the caller does not say.
///
/// The agent's own tab, rather than beside whichever pane a person is looking at. See
/// [`agent_tab_name`] for what that tab is called.
pub const CREATE_KIND_DEFAULT: &str = "agent_tab";

/// The longest pane name a command is turned into, in characters.
const PANE_NAME_MAX: usize = 48;

/// Whether an `agent_tab` create is going into a tab that is already there, or making it.
///
/// Read off the pane list before the create runs - see [`agent_tab`] - rather than guessed at and
/// corrected afterwards.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TabPlacement {
    /// `--in-tab=<name>`: the tab exists, and nothing moves the focus.
    Existing,
    /// `--new-tab=<name>`: it does not, so this call makes it.
    Made,
}

/// What one tool call has to run, which for one tool is more than one command line.
pub enum Call {
    /// One command line, which is every tool but one - and `zellij_create` too, whenever the tab
    /// it is going into is already there.
    One(Vec<String>),
    /// `zellij_create` with `kind: agent_tab`, into a tab this call makes. One create, and the
    /// rename that create cannot do itself.
    MakingAgentTab {
        argv: Vec<String>,
        /// The name the pane takes once `argv` has made it. `--new-tab` refuses `--name`, so a
        /// pane that arrives with its tab is named in a second command instead.
        rename: Option<RenameAfter>,
    },
}

/// The rename that follows a pane made together with its tab.
///
/// `new-pane --new-tab` conflicts with `--name`, because one flag cannot mean the tab and the pane
/// in it. The name is applied afterwards, and only the create's own answer says which pane to
/// apply it to - so the command line is built here and finished with the id it reported.
pub struct RenameAfter {
    session: Option<String>,
    name: String,
}

impl RenameAfter {
    pub fn argv(&self, pane: &str) -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(session) = &self.session {
            argv.push("-s".to_owned());
            argv.push(session.clone());
        }
        argv.extend([
            "action".to_owned(),
            "rename-pane".to_owned(),
            flag_value("--pane-id", pane),
            // the name is positional, so `--` is what keeps a name beginning with a dash a name
            "--".to_owned(),
            self.name.clone(),
        ]);
        argv
    }
}

/// A long flag and its value as one argv word: `--name=<value>`.
///
/// clap reads `--flag=value` whatever the value looks like, while `--flag value` refuses a value
/// that begins with a dash and exits 2 - the same code a miss uses. So a pane called `--force`, or
/// a pane named after a command that starts with a flag, would otherwise be a usage error nothing
/// downstream could tell from a tab that is not there. Every value-taking long flag this module
/// builds goes through here.
fn flag_value(flag: &str, value: &str) -> String {
    format!("{}={}", flag, value)
}

/// The pane a create reported, read back out of its own answer.
///
/// `new-pane` prints `pane_id: terminal_3` on a line of its own, above `handle:` and below the
/// `tab_id:` a `--new-tab` run adds. Nothing else here invents a pane id: a create that printed
/// none leaves the pane unnamed rather than renaming a pane nobody named.
pub fn reported_pane_id(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .find_map(|line| line.trim().strip_prefix("pane_id:"))
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
}

/// What the agent's own tab is called, from the tab the server's own pane sits in.
///
/// `-zj` on the end of the tab the agent is in, so a person reading the tab bar knows whose tab it
/// is and that closing it costs them nothing. An agent already working inside one of these does
/// not nest a second: a name that ends in `-zj` is the answer as it stands. A server that was not
/// started inside a pane has no tab to be named after, and gets `zj`.
pub fn agent_tab_name(current_tab: Option<&str>) -> String {
    match current_tab.map(str::trim).filter(|name| !name.is_empty()) {
        Some(name) if name.ends_with("-zj") => name.to_owned(),
        Some(name) => format!("{}-zj", name),
        None => "zj".to_owned(),
    }
}

/// The name a pane running a command takes when the caller did not name it.
///
/// A command reaches the server as `<shell> -c <command>`, so a pane with no name of its own is
/// titled `/bin/zsh -c ...` - the shell, not the work. The command itself is the name a person
/// reading the tab wants. Whitespace is collapsed because a name is drawn on one line, and a long
/// one is cut rather than left to fill the frame.
fn default_pane_name(command: &str) -> String {
    let collapsed = command.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PANE_NAME_MAX {
        return collapsed;
    }
    let mut name: String = collapsed.chars().take(PANE_NAME_MAX - 1).collect();
    name.push('…');
    name
}

/// What the agent's own tab is called, and whether the session has it yet.
///
/// One child call answers both, because one payload holds both: `action list-panes --json` lists
/// every pane with the name of the tab it is in, so the tab this create is named after and the
/// tabs that already exist are two reads of the same answer.
///
/// Asking is what keeps an `agent_tab` create to a single command line. Assuming the tab is there
/// and treating the refusal as the signal to make it cannot work: the CLI exits 2 for a tab
/// nothing answers to *and* for a command line clap would not parse, so a pane named `--force`
/// looked exactly like a first call of the session and made a second tab of the same name.
///
/// Every way the question can fail is the same answer - the tab named after nothing, and `Made`.
/// A server started outside a pane has no `$ZELLIJ_PANE_ID` to look up, and a session that is not
/// there answers nothing; neither is worth failing a create over, and a session that cannot be
/// listed is one the create will fail against too, with the session's own words.
async fn agent_tab(env: &CallEnv, ambient_session: Option<&str>) -> (String, TabPlacement) {
    let Some(panes) = pane_list(ambient_session).await else {
        return (agent_tab_name(None), TabPlacement::Made);
    };
    let tab = agent_tab_name(
        env.pane
            .as_deref()
            .and_then(|pane| tab_of_pane(&panes, pane))
            .as_deref(),
    );
    let placement = if tab_exists(&panes, &tab) {
        TabPlacement::Existing
    } else {
        TabPlacement::Made
    };
    (tab, placement)
}

/// Every pane of a session as `list-panes --json` prints them, or `None` if it would not answer.
async fn pane_list(ambient_session: Option<&str>) -> Option<String> {
    let mut argv = Vec::new();
    if let Some(session) = ambient_session {
        argv.push("-s".to_owned());
        argv.push(session.to_owned());
    }
    argv.extend([
        "action".to_owned(),
        "list-panes".to_owned(),
        "--json".to_owned(),
    ]);
    let outcome = run(&argv).await.ok()?;
    if outcome.is_error() {
        return None;
    }
    Some(outcome.stdout)
}

/// Whether any pane in a `list-panes --json` answer sits in the tab named.
///
/// A tab with no panes is not a tab - closing the last pane of one closes it - so the pane list
/// names every tab there is. An answer that will not parse counts as no tab, which makes the tab:
/// that leaves an agent with somewhere to work rather than with a miss it cannot act on.
///
/// The one blind spot is a pane privacy policy, which can withhold every pane of a tab and so hide
/// the tab with them. A second `-zj` tab is the cost, and it is visible in the tab bar.
pub fn tab_exists(json: &str, tab: &str) -> bool {
    let Ok(panes) = serde_json::from_str::<Value>(json.trim()) else {
        return false;
    };
    let Some(panes) = panes
        .get("panes")
        .and_then(Value::as_array)
        .or_else(|| panes.as_array())
    else {
        return false;
    };
    let tab = tab.trim();
    panes.iter().any(|entry| {
        entry
            .get("tab_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .is_some_and(|name| name == tab)
    })
}

/// The tab a pane is in, out of a `list-panes --json` answer.
///
/// `$ZELLIJ_PANE_ID` is the bare terminal id the server exports, but the same variable is written
/// as `terminal_3` by hand often enough that both are read here, along with `plugin_3` and a
/// handle. A pane nothing answers to is `None`, like a session that was not there.
pub fn tab_of_pane(json: &str, pane: &str) -> Option<String> {
    let panes: Value = serde_json::from_str(json.trim()).ok()?;
    // `--report-withheld` wraps the array; this call does not ask for it, but reading both costs a
    // line and outlives the choice
    let panes = panes
        .get("panes")
        .and_then(Value::as_array)
        .or_else(|| panes.as_array())?;
    let pane = pane.trim();
    let (want_plugin, want_id) = match pane.split_once('_') {
        Some(("terminal", id)) => (false, id.parse::<u64>().ok()),
        Some(("plugin", id)) => (true, id.parse::<u64>().ok()),
        _ => (false, pane.parse::<u64>().ok()),
    };
    panes
        .iter()
        .find(|entry| match want_id {
            Some(id) => {
                entry.get("id").and_then(Value::as_u64) == Some(id)
                    && entry.get("is_plugin").and_then(Value::as_bool) == Some(want_plugin)
            },
            None => entry.get("handle").and_then(Value::as_str) == Some(pane),
        })
        .and_then(|entry| entry.get("tab_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// What a tool call has to run, after anything only the session can answer has been asked.
///
/// Every tool but one is a pure build from the call itself. `zellij_create` with `kind: agent_tab`
/// is the exception: the tab it is named after is a fact about the session, so it is looked up
/// here rather than guessed at in the builder.
pub async fn plan(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
) -> Result<Call, String> {
    plan_in(tool, args, ambient_session, &CallEnv::from_process()).await
}

async fn plan_in(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
    env: &CallEnv,
) -> Result<Call, String> {
    let read = Args::new(args);
    if tool != "zellij_create"
        || read.enumerated("kind", CREATE_KIND_DEFAULT) != CREATE_KIND_DEFAULT
    {
        return Ok(Call::One(argv_in(tool, args, ambient_session, env, None)?));
    }
    let (tab, placement) = agent_tab(env, ambient_session).await;
    let argv = argv_in(tool, args, ambient_session, env, Some((&tab, placement)))?;
    if placement == TabPlacement::Existing {
        return Ok(Call::One(argv));
    }
    let rename = pane_name(&read).map(|name| RenameAfter {
        session: read
            .string("session")
            .or_else(|| ambient_session.map(String::from)),
        name,
    });
    Ok(Call::MakingAgentTab { argv, rename })
}

/// The name a created pane is given: the caller's own, or the command it runs.
fn pane_name(args: &Args) -> Option<String> {
    args.string("name")
        .or_else(|| args.string("command").as_deref().map(default_pane_name))
}

/// The command line a tool call becomes, after the binary's own name.
///
/// Pure: it reads the call and returns argv, so what every tool runs can be tested without a
/// session. `Err` is a call that could not be turned into one - a missing target, an operation
/// without the argument it needs - and is reported to the caller as a failed tool call rather than
/// guessed at.
///
/// A test's way in. The server goes through [`plan`], which asks the session the one question a
/// command line cannot be built without.
#[cfg(test)]
pub fn argv(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
) -> Result<Vec<String>, String> {
    argv_in(tool, args, ambient_session, &CallEnv::from_process(), None)
}

/// The same, told what the process around it looks like.
///
/// Two arguments of `zellij_create` are about the caller's own environment rather than the
/// session's - the shell a `command` runs in, and what `~`, `$VAR` and a relative path mean in a
/// `cwd`. Reading those once, here, keeps the building itself a function of its inputs, so a test
/// can say what `$SHELL` was without changing the environment every other test runs in.
/// `agent_tab` is the tab an `agent_tab` create is going into and whether it is there yet, which
/// [`plan_in`] asked the session about. `None` for every other call.
fn argv_in(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
    env: &CallEnv,
    agent_tab: Option<(&str, TabPlacement)>,
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
            // `--report-withheld` on both walks: an agent that is told a list is complete when it
            // is not will go looking for the missing pane, and the count is the only thing a
            // withheld pane leaves behind
            "agents" => Ok(scoped(vec![
                "action".to_owned(),
                "list-agents".to_owned(),
                "--json".to_owned(),
                "--report-withheld".to_owned(),
            ])),
            "panes" => {
                let mut rest = vec![
                    "action".to_owned(),
                    "list-panes".to_owned(),
                    "--json".to_owned(),
                    "--report-withheld".to_owned(),
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
            let kind = args.enumerated("kind", CREATE_KIND_DEFAULT);
            let command = args.string("command");
            let mut rest = vec!["action".to_owned()];
            match kind.as_str() {
                "agent_tab" => {
                    let Some((tab, placement)) = agent_tab else {
                        return Err(
                            "`kind: agent_tab` puts the pane in the agent's own tab, which this \
                             call did not resolve before building a command line."
                                .to_owned(),
                        );
                    };
                    if args.flag("floating") {
                        // `--new-tab` refuses `--floating`, so the first create into a tab that is
                        // not there yet would fail where the second succeeded. Refusing both is the
                        // only answer that does not depend on which call arrived first
                        return Err("A floating pane cannot arrive with the tab that holds it. \
                                    Ask for `kind: pane`, or make the agent's tab first."
                            .to_owned());
                    }
                    rest.push("new-pane".to_owned());
                    rest.push(match placement {
                        TabPlacement::Existing => flag_value("--in-tab", tab),
                        TabPlacement::Made => flag_value("--new-tab", tab),
                    });
                    // the agent's tab is not the person's, so neither form takes their focus.
                    // `--in-tab` already leaves it alone; `--new-tab` switches to what it made
                    // unless it is told this
                    rest.push("--no-focus".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push(flag_value("--cwd", &resolve_cwd(&cwd, env)?));
                    }
                    // a pane that arrives with its tab is named afterwards, by `RenameAfter`:
                    // `--new-tab` and `--name` conflict, because one flag cannot mean both
                    if placement == TabPlacement::Existing {
                        if let Some(name) = pane_name(&args) {
                            rest.push(flag_value("--name", &name));
                        }
                    }
                    if let Some(handle) = args.string("handle") {
                        rest.push(flag_value("--handle", &handle));
                    }
                    if let Some(command) = command {
                        rest.push("--".to_owned());
                        rest.extend(shell_command(command, env.shell.clone()));
                    }
                },
                "pane" => {
                    rest.push("new-pane".to_owned());
                    // beside the agent's own pane, not beside whichever pane a person happens to
                    // be focused on. Without a `$ZELLIJ_PANE_ID` to resolve, the CLI says so
                    rest.push("--near-current-pane".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push(flag_value("--cwd", &resolve_cwd(&cwd, env)?));
                    }
                    if let Some(name) = pane_name(&args) {
                        rest.push(flag_value("--name", &name));
                    }
                    if let Some(handle) = args.string("handle") {
                        rest.push(flag_value("--handle", &handle));
                    }
                    if args.flag("floating") {
                        rest.push("--floating".to_owned());
                    }
                    // `--` last, because everything after it is the command's own argv
                    if let Some(command) = command {
                        rest.push("--".to_owned());
                        rest.extend(shell_command(command, env.shell.clone()));
                    }
                },
                "tab" => {
                    rest.push("new-tab".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push(flag_value("--cwd", &resolve_cwd(&cwd, env)?));
                    }
                    if let Some(name) = args.string("name") {
                        rest.push(flag_value("--name", &name));
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
                        rest.extend(shell_command(command, env.shell.clone()));
                    }
                },
                other => {
                    return Err(format!(
                        "`kind` must be agent_tab, pane or tab, not `{}`.",
                        other
                    ));
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
                "close_pane" | "close_tab" => {
                    return Err(format!(
                        "`{}` closes something, which is `zellij_close`'s job rather than this \
                         tool's.",
                        operation
                    ))
                },
                other => return Err(format!("`{}` is not an operation of this tool.", other)),
            };
            Ok(scoped(rest))
        },
        // the two that cannot be undone, kept apart from the four that can. The CLI confirms them
        // for a person and refuses off a terminal; a tool call has already been approved by
        // whatever gates tool calls, so it answers the confirmation here rather than hanging on a
        // prompt nobody can see
        "zellij_close" => {
            let operation = args.required("operation")?;
            let rest = match operation.as_str() {
                "close_pane" => vec![
                    "action".to_owned(),
                    "close-pane".to_owned(),
                    "--pane-id".to_owned(),
                    args.required("pane")?,
                    "--yes".to_owned(),
                ],
                // `close-tab-by-id` takes its id as an argument rather than behind `--tab-id`, and
                // it has no confirmation to answer: the flags the pane verb takes were a clap
                // error here, on every call, from the day this tool was written
                "close_tab" => vec![
                    "action".to_owned(),
                    "close-tab-by-id".to_owned(),
                    args.required("tab")?,
                ],
                other => {
                    return Err(format!(
                        "`operation` must be close_pane or close_tab, not `{}`.",
                        other
                    ))
                },
            };
            Ok(scoped(rest))
        },
        "zellij_snapshot" => {
            let operation = args.required("operation")?;
            // `of_session` rather than the `session` every other tool takes, because it says which
            // snapshots to look at and which name to restore under - never which session to talk to
            let of_session = args.string("of_session");
            match operation.as_str() {
                "list" => {
                    let mut argv = vec![
                        "snapshot".to_owned(),
                        "list".to_owned(),
                        "--json".to_owned(),
                    ];
                    if let Some(session) = of_session {
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
                    if let Some(session) = of_session {
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

/// What the process around a call looks like, read once at the top of it.
///
/// Every field answers a question about the caller rather than about the session, and none of them
/// can be answered from the tool call alone. Holding them in one struct is what lets `argv_in`
/// stay pure and a test be told what it is pretending to be.
struct CallEnv {
    /// `$SHELL`: the shell a `command` is handed to.
    shell: Option<String>,
    /// `$HOME`: what a leading `~` in a `cwd` means.
    home: Option<String>,
    /// Where a relative `cwd` starts from: this process's own directory.
    base: PathBuf,
    /// `$ZELLIJ_PANE_ID`: the pane this server is running in, and so which tab the agent is in.
    /// Absent when the server was not started inside a pane.
    pane: Option<String>,
    /// How a `$VAR` in a `cwd` is looked up. `Send + Sync` because a call holds this across the
    /// child process it runs to find the agent's own tab.
    var: Box<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

impl CallEnv {
    /// The real one: this server's own environment and directory.
    fn from_process() -> Self {
        CallEnv {
            shell: std::env::var("SHELL").ok(),
            home: std::env::var("HOME").ok(),
            base: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            pane: std::env::var("ZELLIJ_PANE_ID")
                .ok()
                .filter(|pane| !pane.trim().is_empty()),
            var: Box::new(|name| std::env::var(name).ok()),
        }
    }
}

/// The shell a `command` is run through: the caller's own, or `/bin/sh` when it has none.
///
/// `$SHELL` is what the operator chose, so `-c` in it behaves the way their own scripts do. A
/// variable set to nothing is not a shell, so an empty value counts as absent rather than as a
/// command line that would fail to exec.
fn shell_for_command(env_shell: Option<String>) -> String {
    env_shell
        .filter(|shell| !shell.trim().is_empty())
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

/// A command given as one string, handed to a shell as one word.
///
/// The server execs the argv after `--` itself, with no shell in between, so a whitespace split
/// made `&&`, a pipe, a quote and a `$VAR` into literal arguments of the first word - `cd x && y`
/// reported `Command not found: cd`. `<shell> -c <command>` is what makes them mean what the
/// caller wrote. Nothing here parses the command; the shell does, and it is not interactive, so no
/// rc file or line editor rewrites it on the way.
fn shell_command(command: String, env_shell: Option<String>) -> Vec<String> {
    vec![shell_for_command(env_shell), "-c".to_owned(), command]
}

/// A `cwd` as the server will take it: expanded, absolute, and known to be a directory.
///
/// `--cwd` reaches the server as a path and nothing expands it there, so `~/x`, `$HOME/x` and a
/// relative path were dropped without a word and the pane opened in the session's own directory
/// instead. Expanding here, against this process's environment, is the only place that can mean
/// what the caller meant. Refusing a path that is not a directory turns the rest of that silence
/// into an answer: a wrong `cwd` is a failed call, not a pane in the wrong place.
fn resolve_cwd(cwd: &str, env: &CallEnv) -> Result<String, String> {
    let expanded = shellexpand::full_with_context(
        cwd,
        || env.home.clone(),
        |name| Ok::<Option<String>, std::convert::Infallible>((env.var)(name)),
    )
    .map_err(|e| format!("`cwd` {} could not be expanded: {}", cwd, e))?;
    let path = Path::new(expanded.as_ref());
    let resolved = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env.base.join(path)
    };
    if !resolved.is_dir() {
        return Err(format!("`cwd` {} is not a directory.", resolved.display()));
    }
    Ok(resolved.to_string_lossy().into_owned())
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

    /// A process for a test to pretend to be, so a built command line does not change with
    /// whoever's shell and home directory the suite happens to run under.
    fn call_env(
        shell: Option<&str>,
        home: Option<&str>,
        base: &Path,
        vars: &[(&str, &str)],
    ) -> CallEnv {
        let vars: BTreeMap<String, String> = vars
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        CallEnv {
            shell: shell.map(str::to_owned),
            home: home.map(str::to_owned),
            base: base.to_path_buf(),
            pane: None,
            var: Box::new(move |name| vars.get(name).cloned()),
        }
    }

    /// The env every call in this suite is built in unless it says otherwise.
    fn a_shell_env() -> CallEnv {
        call_env(Some("/bin/zsh"), Some("/home/tester"), Path::new("/"), &[])
    }

    fn built(tool: &str, value: Value, session: Option<&str>, env: &CallEnv) -> Vec<String> {
        argv_in(tool, &args(value), session, env, None).expect("a call that builds")
    }

    fn line(tool: &str, value: Value, session: Option<&str>) -> String {
        built(tool, value, session, &a_shell_env()).join(" ")
    }

    /// A `kind: agent_tab` create, told which tab it is going into and whether that tab is there.
    fn in_agent_tab(
        value: Value,
        session: Option<&str>,
        tab: &str,
        placement: TabPlacement,
    ) -> String {
        argv_in(
            "zellij_create",
            &args(value),
            session,
            &a_shell_env(),
            Some((tab, placement)),
        )
        .expect("a call that builds")
        .join(" ")
    }

    fn refusal(tool: &str, value: Value) -> String {
        argv_in(tool, &args(value), None, &a_shell_env(), None)
            .expect_err("a call that cannot be built")
    }

    #[test]
    fn the_ambient_session_is_used_when_the_call_does_not_name_one() {
        assert_eq!(
            line("zellij_overview", json!({}), Some("work")),
            "-s work action list-panes --json --report-withheld"
        );
    }

    #[test]
    fn a_named_session_beats_the_ambient_one() {
        assert_eq!(
            line("zellij_overview", json!({"session": "other"}), Some("work")),
            "-s other action list-panes --json --report-withheld"
        );
    }

    #[test]
    fn a_call_with_no_session_anywhere_lets_the_cli_resolve_it() {
        assert_eq!(
            line("zellij_overview", json!({}), None),
            "action list-panes --json --report-withheld"
        );
    }

    #[test]
    fn the_agent_scope_is_the_association_verb() {
        assert_eq!(
            line("zellij_overview", json!({"scope": "agents"}), Some("work")),
            "-s work action list-agents --json --report-withheld"
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
                json!({"kind": "pane", "command": "cargo test", "handle": "test-run"}),
                Some("work")
            ),
            "-s work action new-pane --near-current-pane --name=cargo test --handle=test-run -- \
             /bin/zsh -c cargo test"
        );
    }

    #[test]
    fn a_pane_opens_beside_the_agents_own_pane_and_not_beside_the_persons() {
        // bare `new-pane` lands beside whichever pane a person is focused on, so identical calls
        // used to land in different places and shrink whatever the person was reading
        assert!(line("zellij_create", json!({"kind": "pane"}), None)
            .starts_with("action new-pane --near-current-pane"));
    }

    #[test]
    fn the_agents_tab_is_named_after_the_tab_the_agent_is_in() {
        assert_eq!(agent_tab_name(Some("work")), "work-zj");
        // an agent working inside one of these does not nest a second
        assert_eq!(agent_tab_name(Some("work-zj")), "work-zj");
        // no pane means no tab to be named after
        assert_eq!(agent_tab_name(None), "zj");
        assert_eq!(agent_tab_name(Some("")), "zj");
        assert_eq!(agent_tab_name(Some("  ")), "zj");
    }

    #[test]
    fn a_create_asks_for_the_agents_tab_by_name_and_makes_it_only_when_it_is_not_there() {
        // the tab that exists: nothing moves the focus, and the pane carries its name
        assert_eq!(
            in_agent_tab(
                json!({"command": "cargo test"}),
                Some("work"),
                "work-zj",
                TabPlacement::Existing
            ),
            "-s work action new-pane --in-tab=work-zj --no-focus --name=cargo test -- /bin/zsh -c \
             cargo test"
        );
        // and the tab that does not: `--new-tab` takes no `--name`, and would take the person's
        // focus with it without `--no-focus`
        assert_eq!(
            in_agent_tab(
                json!({"command": "cargo test"}),
                Some("work"),
                "work-zj",
                TabPlacement::Made
            ),
            "-s work action new-pane --new-tab=work-zj --no-focus -- /bin/zsh -c cargo test"
        );
    }

    #[test]
    fn the_pane_list_says_whether_the_agents_tab_is_there_yet() {
        // the decision the create used to make by running a command line and reading its exit
        // code: one payload, one answer, before anything runs
        let panes = r#"[
            {"id": 0, "is_plugin": false, "handle": "sunny-otter", "tab_name": "work"},
            {"id": 1, "is_plugin": false, "handle": "brave-lynx", "tab_name": "work-zj"}
        ]"#;
        assert!(tab_exists(panes, "work-zj"));
        assert!(tab_exists(panes, "work"));
        assert!(!tab_exists(panes, "notes-zj"));
        // the shape `--report-withheld` wraps it in, and an answer that is not a pane list
        let wrapped = r#"{"panes": [{"id": 0, "is_plugin": false, "handle": "sunny-otter",
                          "tab_name": "work"}], "withheld": 1}"#;
        assert!(tab_exists(wrapped, "work"));
        assert!(!tab_exists(wrapped, "work-zj"));
        assert!(!tab_exists("not json", "work-zj"));
        // a tab is made when nothing says it is there, and reused when something does
        for (panes, tab, placement) in [
            (panes, "work-zj", TabPlacement::Existing),
            (panes, "notes-zj", TabPlacement::Made),
        ] {
            let decided = if tab_exists(panes, tab) {
                TabPlacement::Existing
            } else {
                TabPlacement::Made
            };
            assert_eq!(decided, placement, "{}", tab);
        }
    }

    #[test]
    fn a_value_that_begins_with_a_dash_still_reaches_the_cli_as_a_value() {
        // `--name --force` is a clap usage error, and clap exits 2 for that exactly as it does for
        // a tab nothing answers to. `--name=--force` is the form that cannot be read as a flag
        assert_eq!(
            in_agent_tab(
                json!({"name": "--force", "command": "echo hi"}),
                Some("work"),
                "work-zj",
                TabPlacement::Existing
            ),
            "-s work action new-pane --in-tab=work-zj --no-focus --name=--force -- /bin/zsh -c \
             echo hi"
        );
        // and the same for a pane named after a command that starts with a flag
        assert_eq!(
            line(
                "zellij_create",
                json!({"kind": "pane", "command": "--force --now"}),
                None
            ),
            "action new-pane --near-current-pane --name=--force --now -- /bin/zsh -c --force --now"
        );
        // a tab that begins with a dash is a tab name, not a flag, on both forms
        assert!(
            in_agent_tab(json!({}), None, "-zj", TabPlacement::Existing).contains("--in-tab=-zj")
        );
        assert!(in_agent_tab(json!({}), None, "-zj", TabPlacement::Made).contains("--new-tab=-zj"));
        // as does a handle and a tab's own name
        assert!(
            line("zellij_create", json!({"kind": "tab", "name": "-x"}), None).contains("--name=-x")
        );
    }

    #[test]
    fn a_pane_that_arrives_with_its_tab_is_named_in_a_second_command() {
        let rename = RenameAfter {
            session: Some("work".to_owned()),
            name: "cargo test".to_owned(),
        };
        assert_eq!(
            rename.argv("terminal_3").join(" "),
            "-s work action rename-pane --pane-id=terminal_3 -- cargo test"
        );
        assert_eq!(
            reported_pane_id("tab_id: 2\npane_id: terminal_3\nhandle: sunny-otter\n"),
            Some("terminal_3".to_owned())
        );
        // a create that printed no pane leaves the pane unnamed rather than renaming one it guessed
        assert_eq!(reported_pane_id("handle: sunny-otter\n"), None);
    }

    #[test]
    fn a_command_pane_is_titled_with_the_command_and_not_with_the_shell_that_runs_it() {
        // the command reaches the server as `<shell> -c <command>`, which is what an unnamed pane
        // would otherwise show
        assert_eq!(default_pane_name("cargo test"), "cargo test");
        assert_eq!(default_pane_name("  cargo   test \n"), "cargo test");
        let long = "cargo test --workspace --all-features -- --nocapture --test-threads 1";
        let cut = default_pane_name(long);
        assert_eq!(cut.chars().count(), PANE_NAME_MAX);
        assert!(cut.ends_with('…'), "{}", cut);
        assert!(
            long.starts_with(&cut[..cut.len() - '…'.len_utf8()]),
            "{}",
            cut
        );
        // a name the caller gave beats the command
        assert!(line(
            "zellij_create",
            json!({"kind": "pane", "command": "cargo test", "name": "tests"}),
            None
        )
        .contains("--name=tests --"));
        // and a pane with no command is not given one
        assert!(!line("zellij_create", json!({"kind": "pane"}), None).contains("--name"));
    }

    #[test]
    fn a_floating_pane_cannot_arrive_with_the_tab_that_holds_it() {
        // `--new-tab` refuses `--floating`, so allowing it would work or fail depending on whether
        // the agent's tab happened to exist yet
        let said = argv_in(
            "zellij_create",
            &args(json!({"floating": true})),
            None,
            &a_shell_env(),
            Some(("work-zj", TabPlacement::Existing)),
        )
        .expect_err("a floating pane in the agent's tab");
        assert!(said.contains("`kind: pane`"), "{}", said);
    }

    #[test]
    fn the_tab_a_pane_is_in_is_read_out_of_the_pane_list() {
        let panes = r#"[
            {"id": 0, "is_plugin": false, "handle": "sunny-otter", "tab_name": "work"},
            {"id": 1, "is_plugin": false, "handle": "brave-lynx", "tab_name": "notes"},
            {"id": 1, "is_plugin": true, "handle": "tab-bar", "tab_name": "work"}
        ]"#;
        // the server exports the bare id; the same variable is written as `terminal_1` by hand
        assert_eq!(tab_of_pane(panes, "1"), Some("notes".to_owned()));
        assert_eq!(tab_of_pane(panes, "terminal_1"), Some("notes".to_owned()));
        assert_eq!(tab_of_pane(panes, "plugin_1"), Some("work".to_owned()));
        assert_eq!(tab_of_pane(panes, "sunny-otter"), Some("work".to_owned()));
        // a pane nothing answers to, and an answer that is not a pane list
        assert_eq!(tab_of_pane(panes, "terminal_9"), None);
        assert_eq!(tab_of_pane("not json", "1"), None);
        // and the shape `--report-withheld` wraps it in
        let wrapped = r#"{"panes": [{"id": 0, "is_plugin": false, "handle": "sunny-otter",
                          "tab_name": "work"}], "withheld": 1}"#;
        assert_eq!(tab_of_pane(wrapped, "0"), Some("work".to_owned()));
    }

    #[test]
    fn a_command_runs_in_the_callers_shell_and_in_sh_when_it_has_none() {
        assert_eq!(shell_for_command(Some("/bin/zsh".to_owned())), "/bin/zsh");
        assert_eq!(shell_for_command(None), "/bin/sh");
        assert_eq!(shell_for_command(Some(String::new())), "/bin/sh");
        assert_eq!(shell_for_command(Some("   ".to_owned())), "/bin/sh");
        let no_shell = call_env(None, Some("/home/tester"), Path::new("/"), &[]);
        assert_eq!(
            built(
                "zellij_create",
                json!({"kind": "pane", "command": "pwd"}),
                None,
                &no_shell
            )
            .join(" "),
            "action new-pane --near-current-pane --name=pwd -- /bin/sh -c pwd"
        );
    }

    #[test]
    fn a_command_reaches_the_shell_as_one_word_whatever_is_in_it() {
        // the whole point: `&&`, the quotes, the pipe and the `$HOME` are the shell's to read, and
        // a split here would hand them to the first word as arguments
        let command = "cd /tmp && echo \"hello world\" $HOME | cat";
        let argv = built(
            "zellij_create",
            json!({"kind": "pane", "command": command, "name": "shell"}),
            None,
            &a_shell_env(),
        );
        assert_eq!(
            argv,
            vec![
                "action",
                "new-pane",
                "--near-current-pane",
                "--name=shell",
                "--",
                "/bin/zsh",
                "-c",
                command
            ]
        );
        // and the same for a tab, which builds its command line separately
        let argv = built(
            "zellij_create",
            json!({"kind": "tab", "command": command}),
            None,
            &a_shell_env(),
        );
        assert_eq!(
            argv,
            vec!["action", "new-tab", "--", "/bin/zsh", "-c", command]
        );
    }

    #[test]
    fn a_cwd_is_expanded_the_way_a_shell_would_expand_it() {
        let home = tempfile::tempdir().expect("a temp dir");
        let inside = home.path().join("work");
        std::fs::create_dir(&inside).expect("a directory to point at");
        let home_path = home.path().to_string_lossy().into_owned();
        let env = call_env(
            Some("/bin/zsh"),
            Some(&home_path),
            Path::new("/"),
            &[("HOME", &home_path)],
        );
        let expected = inside.to_string_lossy().into_owned();
        for cwd in ["~/work", "$HOME/work", "${HOME}/work"] {
            assert_eq!(
                built(
                    "zellij_create",
                    json!({"kind": "pane", "cwd": cwd}),
                    None,
                    &env
                )
                .join(" "),
                format!("action new-pane --near-current-pane --cwd={}", expected)
            );
            assert_eq!(
                built(
                    "zellij_create",
                    json!({"kind": "tab", "cwd": cwd}),
                    None,
                    &env
                )
                .join(" "),
                format!("action new-tab --cwd={}", expected)
            );
        }
    }

    #[test]
    fn a_relative_cwd_starts_from_the_directory_this_server_is_in() {
        let base = tempfile::tempdir().expect("a temp dir");
        let inside = base.path().join("work");
        std::fs::create_dir(&inside).expect("a directory to point at");
        let env = call_env(Some("/bin/zsh"), None, base.path(), &[]);
        assert_eq!(
            built(
                "zellij_create",
                json!({"kind": "pane", "cwd": "work"}),
                None,
                &env
            )
            .join(" "),
            format!(
                "action new-pane --near-current-pane --cwd={}",
                inside.to_string_lossy()
            )
        );
    }

    #[test]
    fn a_cwd_that_is_not_a_directory_is_refused_rather_than_quietly_dropped() {
        let home = tempfile::tempdir().expect("a temp dir");
        let home_path = home.path().to_string_lossy().into_owned();
        let env = call_env(
            Some("/bin/zsh"),
            Some(&home_path),
            Path::new("/"),
            &[("HOME", &home_path)],
        );
        let missing = home.path().join("nope");
        for cwd in ["~/nope", "$HOME/nope"] {
            let said = argv_in(
                "zellij_create",
                &args(json!({"kind": "pane", "cwd": cwd})),
                None,
                &env,
                None,
            )
            .expect_err("a cwd that is not there");
            // the message names what it resolved to, not what was typed, because the two differ
            assert!(
                said.contains(&missing.to_string_lossy().into_owned()),
                "{}",
                said
            );
            assert!(said.contains("is not a directory"), "{}", said);
        }
        assert!(argv_in(
            "zellij_create",
            &args(json!({"kind": "tab", "cwd": "/nonexistent-dir"})),
            None,
            &env,
            None
        )
        .expect_err("a cwd that is not there")
        .contains("/nonexistent-dir is not a directory"));
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
                "zellij_close",
                json!({"operation": "close_pane", "pane": "sunny-otter"}),
                None
            ),
            "action close-pane --pane-id sunny-otter --yes"
        );
        assert_eq!(
            line(
                "zellij_close",
                json!({"operation": "close_tab", "tab": 2}),
                None
            ),
            "action close-tab-by-id 2"
        );
    }

    #[test]
    fn closing_is_its_own_tool_so_a_client_can_allow_the_moves_without_it() {
        // one name for six operations meant a client that wanted `move_pane` had to allow
        // `close_tab` with it
        for operation in ["close_pane", "close_tab"] {
            let said = refusal("zellij_arrange", json!({ "operation": operation }));
            assert!(said.contains("zellij_close"), "{}", said);
        }
        for operation in ["move_pane", "stack_panes"] {
            let said = refusal("zellij_close", json!({ "operation": operation }));
            assert!(said.contains("close_pane or close_tab"), "{}", said);
        }
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
                json!({"operation": "restore", "id": "latest", "of_session": "revived"}),
                Some("work")
            ),
            "snapshot restore latest --session revived"
        );
        // and `session`, which means the session to talk to everywhere else, is not read here at
        // all: the parameter is called `of_session` so that the two cannot be confused
        assert_eq!(
            line(
                "zellij_snapshot",
                json!({"operation": "list", "session": "work"}),
                None
            ),
            "snapshot list --json"
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
