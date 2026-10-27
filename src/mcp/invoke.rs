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
//! The binary run is this one, found with `current_exe`, so the CLI a tool calls is the build the
//! tool shipped in. After an upgrade renames a new build over it, the tool runs the new build at
//! the same path instead of failing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::remote::{self, Target};

/// How long `zellij_wait_for` waits when the caller did not say.
///
/// The same string the tool's `timeout_s` parameter documents as its default - one constant, so a
/// description that promises a bound and a command line that has none cannot drift apart.
pub const WAIT_TIMEOUT_DEFAULT_S: &str = "300";

/// How long `zellij_create` waits for a new pane's shell to be ready, in seconds.
///
/// Well under the request deadline an MCP client keeps: a create that waits a minute is a call the
/// client has already given up on.
pub const READY_TIMEOUT_DEFAULT_S: &str = "10";

/// How `zellij_create` runs a `command` when the caller does not say: typed into a shell pane.
pub const RUN_IN_DEFAULT: &str = "shell";

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

/// The zellij binary a tool should run: this one, or the build now at its path after an upgrade.
///
/// A long-lived MCP server outlives an upgrade, and on Linux `current_exe()` then names a
/// " (deleted)" path that no spawn can run.
pub fn zellij_binary() -> Result<PathBuf, String> {
    zellij_utils::session_lifecycle::own_exe_for_spawn()
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

/// Run the CLI where the call is going: this machine, or another over ssh.
///
/// `stdin` is the bytes a remote verb reads in place of a file on its own machine - a drop. A
/// local call never has any: the local CLI reads the file itself.
pub async fn run_on(
    target: &Target,
    argv: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<Outcome, String> {
    match target {
        Target::Local => run(argv).await,
        Target::Remote(host) => remote::run(host, argv, stdin).await,
    }
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
        self.flag_or(name, false)
    }
    fn flag_or(&self, name: &str, default: bool) -> bool {
        self.values
            .get(name)
            .and_then(|value| value.as_bool())
            .unwrap_or(default)
    }
    fn enumerated(&self, name: &str, default: &str) -> String {
        self.string(name).unwrap_or_else(|| default.to_owned())
    }
    /// A required list of non-empty strings. A lone string is taken as a list of one, which is the
    /// mistake a caller is most likely to make and has only one meaning.
    fn strings(&self, name: &str) -> Result<Vec<String>, String> {
        let values = match self.values.get(name) {
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| match item {
                    Value::String(text) if !text.is_empty() => Ok(text.clone()),
                    _ => Err(format!("`{}` must hold only non-empty strings.", name)),
                })
                .collect::<Result<Vec<_>, _>>()?,
            Some(Value::String(text)) if !text.is_empty() => vec![text.clone()],
            _ => Vec::new(),
        };
        if values.is_empty() {
            return Err(format!("`{}` is required and was not given.", name));
        }
        Ok(values)
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
    /// Command lines run one after another, with a short pause between, stopping at the first
    /// that fails: `zellij_write_input` with `submit`, which pastes and then presses Enter.
    Sequence(Vec<Vec<String>>),
}

/// The pause between the steps of a [`Call::Sequence`].
///
/// A program that reads a paste as it arrives can still be taking it in when an Enter right
/// behind it lands, and some take that Enter as part of the paste: a newline in the message
/// instead of the key that sends it. A pause a person would not notice keeps the two apart.
pub const SEQUENCE_PAUSE_MS: u64 = 150;

/// A tool call, planned: what it runs, and the command a shell-mode create types once its pane is
/// ready.
pub struct Plan {
    pub call: Call,
    /// `zellij_create` with `run_in: shell` and a `command`: typed into the pane the create made,
    /// and only once the create has said that pane is ready. `None` for every other call.
    pub then_type: Option<TypeAfter>,
    /// `zellij_overview` with `scope: agents` or `my_agents`: how the agent list is shaped before
    /// it is returned. `None` for every other call.
    pub agents: Option<AgentsView>,
    /// What a remote `zellij_write_input` `file` sends over stdin: the file's bytes, read on this
    /// machine. `None` for every other call.
    pub stdin: Option<Vec<u8>>,
}

/// How much of an agent's command line `zellij_overview` returns. Enough to tell agents apart.
pub const AGENT_COMMAND_MAX: usize = 120;

/// What `zellij_overview` adds to, and takes from, the `list-agents --json` answer.
pub struct AgentsView {
    /// `my_agents`: only the agents in this tab, the caller's own `-zj` tab, where every agent it
    /// started with `zellij_create` went. `None` keeps every agent.
    pub only_tab: Option<String>,
}

impl AgentsView {
    /// The answer as the caller gets it: each row with how long ago the agent last reported and
    /// last drew anything, in whole seconds, and, for `my_agents`, only the caller's own.
    ///
    /// Ages rather than the raw epoch milliseconds alone, because a reader asked "which agent has
    /// stalled" should not have to subtract 13-digit numbers. The raw keys stay beside them.
    pub fn shape(&self, stdout: &str, now_ms: u64) -> Option<String> {
        let mut answer: Value = serde_json::from_str(stdout.trim()).ok()?;
        let rows = match &mut answer {
            Value::Array(rows) => rows,
            Value::Object(envelope) => envelope.get_mut("agents")?.as_array_mut()?,
            _ => return None,
        };
        if let Some(tab) = &self.only_tab {
            rows.retain(|row| row.get("tab_name").and_then(Value::as_str) == Some(tab));
        }
        for row in rows.iter_mut() {
            let Some(row) = row.as_object_mut() else {
                continue;
            };
            // an agent started with a prompt carries the whole prompt in its command line, and a
            // supervisor polling this view would be handed every brief on every call
            if let Some(command) = row.get("command").and_then(Value::as_str) {
                let folded = command.split_whitespace().collect::<Vec<_>>().join(" ");
                let short = if folded.chars().count() > AGENT_COMMAND_MAX {
                    let mut cut: String = folded.chars().take(AGENT_COMMAND_MAX - 1).collect();
                    cut.push('…');
                    cut
                } else {
                    folded
                };
                row.insert("command".to_owned(), Value::from(short));
            }
            for (from, to) in [
                ("state_at", "state_age_s"),
                ("last_output_at", "output_age_s"),
            ] {
                if let Some(at) = row.get(from).and_then(Value::as_u64) {
                    row.insert(to.to_owned(), Value::from(now_ms.saturating_sub(at) / 1000));
                }
            }
        }
        serde_json::to_string_pretty(&answer).ok()
    }
}

impl From<Call> for Plan {
    fn from(call: Call) -> Self {
        Plan {
            call,
            then_type: None,
            agents: None,
            stdin: None,
        }
    }
}

/// The command a shell-mode create types into the pane it made, as a person would: the text, then
/// Enter - the two calls `zellij_write_input` makes.
///
/// Like [`RenameAfter`], it is finished with the pane id the create reported, and never with one
/// it guessed.
pub struct TypeAfter {
    session: Option<String>,
    command: String,
}

impl TypeAfter {
    /// The command lines that type the command into `pane` and press Enter, in order.
    pub fn argvs(&self, pane: &str) -> [Vec<String>; 2] {
        let scoped = |rest: [&str; 5]| {
            let mut argv = Vec::new();
            if let Some(session) = &self.session {
                argv.push("-s".to_owned());
                argv.push(session.clone());
            }
            argv.extend(rest.iter().map(|word| (*word).to_owned()));
            argv
        };
        let pane = flag_value("--pane-id", pane);
        [
            scoped(["action", "write-chars", &pane, "--", &self.command]),
            scoped(["action", "send-keys", &pane, "--", "Enter"]),
        ]
    }
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
///
/// A call to another machine names the tab after the same pane on this one, so a caller has one
/// tab name everywhere, and asks the remote session whether it has that tab yet.
async fn agent_tab(
    env: &CallEnv,
    ambient_session: Option<&str>,
    target: &Target,
    target_session: Option<&str>,
) -> (String, TabPlacement) {
    if target.is_remote() {
        let own_tab = match pane_list(&Target::Local, ambient_session).await {
            Some(panes) => env
                .pane
                .as_deref()
                .and_then(|pane| tab_of_pane(&panes, pane)),
            None => None,
        };
        let tab = agent_tab_name(own_tab.as_deref());
        let placement = match pane_list(target, target_session).await {
            Some(panes) if tab_exists(&panes, &tab) => TabPlacement::Existing,
            _ => TabPlacement::Made,
        };
        return (tab, placement);
    }
    let Some(panes) = pane_list(&Target::Local, ambient_session).await else {
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
async fn pane_list(target: &Target, session: Option<&str>) -> Option<String> {
    let mut argv = Vec::new();
    if let Some(session) = session {
        argv.push("-s".to_owned());
        argv.push(session.to_owned());
    }
    argv.extend([
        "action".to_owned(),
        "list-panes".to_owned(),
        "--json".to_owned(),
    ]);
    let outcome = run_on(target, &argv, None).await.ok()?;
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
    target: &Target,
) -> Result<Plan, String> {
    let mut env = CallEnv::from_process();
    env.remote = target.is_remote();
    plan_in(tool, args, ambient_session, &env, target).await
}

/// `ambient_session` is this machine's, and is what the agent's tab is named from wherever the call
/// goes. A call to another machine never falls back to it: a session there is named in the call or
/// left to the remote CLI.
async fn plan_in(
    tool: &str,
    args: &Map<String, Value>,
    ambient_session: Option<&str>,
    env: &CallEnv,
    target: &Target,
) -> Result<Plan, String> {
    let own_session = ambient_session;
    let ambient_session = if target.is_remote() {
        None
    } else {
        ambient_session
    };
    let read = Args::new(args);
    let target_session = read
        .string("session")
        .or_else(|| ambient_session.map(String::from));
    let remote_host = match target {
        Target::Remote(host) => Some(host),
        Target::Local => None,
    };
    if tool == "zellij_write_input" {
        if let Some(call) = write_input_call(&read, ambient_session, target).await? {
            return Ok(call.into());
        }
        if let (Some(_), Some(file)) = (remote_host, read.string("file")) {
            // the file is on this machine and the pane is not: its bytes go over ssh's stdin, to
            // a `drop-file` that reads `-`
            let argv = argv_in(tool, args, ambient_session, env, None)?;
            let path = local_file(&file, env, "file")?;
            let bytes = std::fs::read(&path)
                .map_err(|e| format!("`file` {} could not be read: {}", path.display(), e))?;
            let mut plan: Plan = Call::One(argv).into();
            plan.stdin = Some(bytes);
            return Ok(plan);
        }
    }
    if let (Some(host), "zellij_show") = (remote_host, tool) {
        // refused for free before anything is copied
        argv_in(tool, args, ambient_session, env, None)?;
        let mut uploaded = Vec::new();
        for path in read.strings("paths")? {
            let path = local_file(&path, env, "paths")?;
            uploaded.push(Value::String(remote::upload(host, &path).await?));
        }
        let mut args = args.clone();
        args.insert("paths".to_owned(), Value::Array(uploaded));
        return Ok(Call::One(argv_in(tool, &args, ambient_session, env, None)?).into());
    }
    if tool == "zellij_overview" {
        let scope = read.enumerated("scope", "panes");
        if scope == "agents" || scope == "my_agents" {
            let only_tab = if scope == "my_agents" {
                Some(
                    agent_tab(env, own_session, target, target_session.as_deref())
                        .await
                        .0,
                )
            } else {
                None
            };
            let call = Call::One(argv_in(tool, args, ambient_session, env, None)?);
            return Ok(Plan {
                call,
                then_type: None,
                agents: Some(AgentsView { only_tab }),
                stdin: None,
            });
        }
    }
    // a reopen brings a closed pane back where it was, so there is no tab to resolve
    if tool != "zellij_create" || read.string("reopen").is_some() {
        return Ok(Call::One(argv_in(tool, args, ambient_session, env, None)?).into());
    }
    create_kind_allowed(&read)?;
    let session = target_session.clone();
    // a brief is a file on this machine; a pane on another machine reads the copy made there, and
    // only once everything a refusal could be about has been checked
    let brief_at = match remote_host {
        Some(host) if run_in(&read)? == "agent" && read.string("brief").is_some() => {
            remote_kind_allowed(&read)?;
            agent_command(&read, env, None)?;
            match brief_path(&read, env)? {
                Some(path) => Some(remote::upload(host, &path).await?),
                None => None,
            }
        },
        _ => None,
    };
    let then_type = typed_command(&read, env, brief_at.as_deref())?.map(|command| TypeAfter {
        session: session.clone(),
        command,
    });
    if read.enumerated("kind", CREATE_KIND_DEFAULT) != CREATE_KIND_DEFAULT {
        let call = Call::One(argv_in(tool, args, ambient_session, env, None)?);
        return Ok(Plan {
            call,
            then_type,
            agents: None,
            stdin: None,
        });
    }
    // before the session is asked anything: a handle the CLI would refuse is refused for free
    chosen_handle(&read)?;
    let (tab, placement) = agent_tab(env, own_session, target, target_session.as_deref()).await;
    let argv = argv_in(tool, args, ambient_session, env, Some((&tab, placement)))?;
    if placement == TabPlacement::Existing {
        let call = Call::One(argv);
        return Ok(Plan {
            call,
            then_type,
            agents: None,
            stdin: None,
        });
    }
    let rename = pane_name(&read).map(|name| RenameAfter {
        session: session.clone(),
        name,
    });
    let call = Call::MakingAgentTab { argv, rename };
    Ok(Plan {
        call,
        then_type,
        agents: None,
        stdin: None,
    })
}

/// A file a caller named, on this machine: `~` and `$VAR` expanded, a relative path taken from
/// this server's directory, and refused when it is not a file.
fn local_file(path: &str, env: &CallEnv, param: &str) -> Result<PathBuf, String> {
    let expanded = expand_path(path, env)?;
    let path = Path::new(&expanded);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        env.base.join(path)
    };
    if !path.is_file() {
        return Err(format!("`{}` {} is not a file.", param, path.display()));
    }
    Ok(path)
}

/// Refuse removed placement options before querying a session or uploading a brief.
fn create_kind_allowed(args: &Args) -> Result<(), String> {
    if args.values.contains_key("floating") {
        return Err("`floating` is unsupported; use stacked, agent_tab or tab.".to_owned());
    }
    match args.enumerated("kind", CREATE_KIND_DEFAULT).as_str() {
        "agent_tab" | "stacked" | "tab" => Ok(()),
        other => Err(format!(
            "`kind` must be agent_tab, stacked or tab, not `{}`.",
            other
        )),
    }
}

/// A stack needs the caller's pane, which is not on another machine.
fn remote_kind_allowed(args: &Args) -> Result<(), String> {
    match args.enumerated("kind", CREATE_KIND_DEFAULT).as_str() {
        "stacked" => Err(
            "`kind: stacked` needs your own pane, which is not in a session on \
             another machine. Use agent_tab, the default, or tab."
                .to_owned(),
        ),
        _ => Ok(()),
    }
}

/// `zellij_write_input` when it is more than one keystroke or one write: a message to submit, or
/// an action on the coding agent in the pane. `None` for a plain `keys` or `text` call, which
/// [`argv_in`] builds.
async fn write_input_call(
    args: &Args<'_>,
    ambient_session: Option<&str>,
    target: &Target,
) -> Result<Option<Call>, String> {
    let submit = args.flag("submit");
    let action = args.string("agent_action");
    if args.string("file").is_some() && (submit || action.is_some()) {
        return Err(
            "`file` pastes a reference and presses nothing: pass it alone, then send the \
             message about it with text and submit in a second call."
                .to_owned(),
        );
    }
    if !submit && action.is_none() {
        return Ok(None);
    }
    let pane = args.required("pane")?;
    let session = args
        .string("session")
        .or_else(|| ambient_session.map(String::from));
    let scoped = |rest: Vec<String>| -> Vec<String> {
        let mut argv = Vec::new();
        if let Some(session) = &session {
            argv.push("-s".to_owned());
            argv.push(session.clone());
        }
        argv.extend(rest);
        argv
    };
    let press = |key: &str| {
        scoped(vec![
            "action".to_owned(),
            "send-keys".to_owned(),
            flag_value("--pane-id", &pane),
            "--".to_owned(),
            key.to_owned(),
        ])
    };
    if let Some(action) = action {
        if submit || args.string("keys").is_some() || args.string("text").is_some() {
            return Err(
                "`agent_action` presses its own key: pass it alone, without keys, text or \
                 submit."
                    .to_owned(),
            );
        }
        if !["interrupt", "approve", "deny"].contains(&action.as_str()) {
            return Err(format!(
                "`agent_action` must be interrupt, approve or deny, not `{}`.",
                action
            ));
        }
        let agents = run_on(
            target,
            &scoped(vec![
                "action".to_owned(),
                "list-agents".to_owned(),
                "--json".to_owned(),
            ]),
            None,
        )
        .await?;
        if agents.is_error() {
            return Err(format!(
                "Could not ask the session which agent is in `{}`: {}",
                pane,
                agents.stderr.trim()
            ));
        }
        let row = agent_row(&agents.stdout, &pane).ok_or_else(|| {
            format!(
                "`{}` is not running a coding agent this server can see. Pass the handle \
                 zellij_overview with scope=agents prints.",
                pane
            )
        })?;
        let field = |name: &str| row.get(name).and_then(Value::as_str);
        let key = agent_key(
            field("kind").unwrap_or_default(),
            field("state"),
            field("state_kind"),
            &action,
        )?;
        return Ok(Some(Call::One(press(key))));
    }
    if args.string("keys").is_some() {
        return Err("`submit` sends text: pass text, not keys.".to_owned());
    }
    let text = args
        .required("text")
        .map_err(|_| "`submit` needs the `text` to send.".to_owned())?;
    Ok(Some(Call::Sequence(vec![
        scoped(vec![
            "action".to_owned(),
            "paste".to_owned(),
            flag_value("--pane-id", &pane),
            "--".to_owned(),
            text,
        ]),
        press("Enter"),
    ])))
}

/// The `list-agents --json` row of the pane a caller named: by handle, bare id or `terminal_N`.
pub fn agent_row(json: &str, pane: &str) -> Option<Map<String, Value>> {
    let rows: Vec<Map<String, Value>> = serde_json::from_str(json.trim()).ok()?;
    rows.into_iter().find(|row| {
        let id = row.get("pane_id").and_then(Value::as_u64);
        row.get("handle").and_then(Value::as_str) == Some(pane)
            || id.map_or(false, |id| {
                pane == id.to_string() || pane == format!("terminal_{}", id)
            })
    })
}

/// The key an agent action presses, or why it presses none.
///
/// The keys are the ones the harnesses share. A permission prompt opens on its "allow once"
/// option in claude, codex and opencode, so Enter takes it; Esc refuses it, and interrupts a
/// working turn. pi asks for no approval at all. The agent's own `state` is the guard: an Enter
/// sent to an agent that is not asking anything would send whatever it holds in its input box.
/// An agent that reports no state cannot be checked, and gets the key.
pub fn agent_key(
    kind: &str,
    state: Option<&str>,
    state_kind: Option<&str>,
    action: &str,
) -> Result<&'static str, String> {
    let nothing = |what: &str| {
        Err(format!(
            "Nothing to {}: the agent's state is {}. Nothing was pressed.",
            what,
            state.unwrap_or("-")
        ))
    };
    match action {
        "interrupt" => match state {
            None | Some("working") => Ok("Esc"),
            Some("blocked") => Err(
                "The agent is blocked on a person, not working: approve or deny it instead. \
                 Nothing was pressed."
                    .to_owned(),
            ),
            Some(_) => nothing("interrupt"),
        },
        "approve" if kind == "pi" => {
            Err("pi asks for no approvals, so there is nothing to approve.".to_owned())
        },
        "approve" => match (state, state_kind) {
            (Some("blocked"), Some("question")) => Err(
                "The agent is asking a question, not for permission: answer it with keys (the \
                 option's number) or text. Nothing was pressed."
                    .to_owned(),
            ),
            (None, _) | (Some("blocked"), _) => Ok("Enter"),
            _ => nothing("approve"),
        },
        "deny" => match state {
            None | Some("blocked") => Ok("Esc"),
            Some(_) => nothing("deny"),
        },
        other => Err(format!(
            "`agent_action` must be interrupt, approve or deny, not `{}`.",
            other
        )),
    }
}

/// How a create runs its `command`: `shell`, `exec` or `agent`, refused when it is none of them.
fn run_in(args: &Args) -> Result<String, String> {
    let run_in = args.enumerated("run_in", RUN_IN_DEFAULT);
    match run_in.as_str() {
        "shell" | "exec" | "agent" => Ok(run_in),
        other => Err(format!(
            "`run_in` must be shell, exec or agent, not `{}`.",
            other
        )),
    }
}

/// The command a create types into its pane once the pane is ready: the `command` of a
/// `run_in: shell` create, the harness and its brief for an `agent` one, and nothing for an
/// `exec` one, which hands it to the server instead.
///
/// `brief_at` is where the brief is on the machine the pane is on, when that is not this one.
fn typed_command(
    args: &Args,
    env: &CallEnv,
    brief_at: Option<&str>,
) -> Result<Option<String>, String> {
    if args.string("brief").is_some() && run_in(args)? != "agent" {
        return Err(
            "`brief` is the first prompt of an agent: pass it with run_in=agent.".to_owned(),
        );
    }
    Ok(match run_in(args)?.as_str() {
        "shell" => args.string("command"),
        "agent" => Some(agent_command(args, env, brief_at)?),
        _ => None,
    })
}

/// The line an `agent` create types into its shell: the harness command as the caller wrote it,
/// and the brief file's text as its first prompt, in the form that harness takes one.
///
/// The brief is read by the pane's own shell, `"$(cat '<path>')"`, so its text never passes
/// through this command line, a shell history line keeps the short form, and quotes or newlines
/// in it arrive as written. Everything after `--` is the prompt, so a brief that begins with a
/// dash is not read as a flag. opencode takes its first prompt as `--prompt`.
///
/// A session restore records an agent pane as the bare resume of its session (nkmk.32), so the
/// brief, which reaches the harness as an argument, is never typed again by a restore.
///
/// `brief_at` replaces the brief's own path with a copy's, for a pane on another machine.
fn agent_command(args: &Args, env: &CallEnv, brief_at: Option<&str>) -> Result<String, String> {
    let command = args.required("command").map_err(|_| {
        "run_in=agent needs `command`: the harness to start, like `claude` or `codex --model \
         o3`."
            .to_owned()
    })?;
    let program = command.split_whitespace().next().unwrap_or_default();
    let harness = zellij_utils::agent_detect::harness_for_command(program).ok_or_else(|| {
        format!(
            "`{}` is not a coding agent this build knows: run_in=agent starts claude, codex, \
             opencode or pi.",
            program
        )
    })?;
    let Some(path) = brief_path(args, env)? else {
        return Ok(command);
    };
    let path = match brief_at {
        Some(copy) => copy.to_owned(),
        None => path.to_string_lossy().into_owned(),
    };
    let quoted = format!("\"$(cat '{}')\"", path.replace('\'', "'\\''"));
    Ok(match harness.kind {
        "opencode" => format!("{} --prompt {}", command, quoted),
        _ => format!("{} -- {}", command, quoted),
    })
}

/// The brief file on this machine, refused when it is not a file. `None` without a brief.
fn brief_path(args: &Args, env: &CallEnv) -> Result<Option<PathBuf>, String> {
    match args.string("brief") {
        Some(brief) => local_file(&brief, env, "brief").map(Some),
        None => Ok(None),
    }
}

/// The handle a create asked for, refused here by the rule the CLI's `--handle` applies.
///
/// The CLI would refuse it too, but only after a process was spawned to ask - and for an
/// `agent_tab` create, after the session was asked about the tab first.
fn chosen_handle(args: &Args) -> Result<Option<String>, String> {
    match args.string("handle") {
        Some(handle) => match zellij_utils::pane_handle::chosen_handle_error(&handle) {
            Some(reason) => Err(reason),
            None => Ok(Some(handle)),
        },
        None => Ok(None),
    }
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
            "agents" | "my_agents" => Ok(scoped(vec![
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
                "`scope` must be panes, agents, my_agents, sessions or fleet, not `{}`.",
                other
            )),
        },
        "zellij_read_pane" if args.flag("last_reply") => {
            if args.flag("full") || args.flag("ansi") {
                return Err(
                    "`last_reply` reads the agent's transcript, not the screen: pass it \
                            without full or ansi."
                        .to_owned(),
                );
            }
            Ok(scoped(vec![
                "action".to_owned(),
                "last-reply".to_owned(),
                "--pane-id".to_owned(),
                args.required("pane")?,
                "--json".to_owned(),
            ]))
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
            if ![
                "exit",
                "match",
                "quiet",
                "ready",
                "agent_idle",
                "agent_waiting",
                "agent_change",
            ]
            .contains(&until.as_str())
            {
                return Err(format!(
                    "`until` must be exit, match, quiet, ready, agent_idle, agent_waiting or agent_change, \
                     not `{}`.",
                    until
                ));
            }
            let mut rest = vec![
                "action".to_owned(),
                "wait".to_owned(),
                if until == "agent_change" {
                    args.string("pane").unwrap_or_default()
                } else {
                    args.required("pane")?
                },
                "--for".to_owned(),
                // the tool's enum spells its values the way its other enums do; the CLI's are
                // kebab-case
                until.replace('_', "-"),
            ];
            if rest[2].is_empty() {
                rest.remove(2);
            }
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
            if let Some(file) = args.string("file") {
                if args.string("keys").is_some() || args.string("text").is_some() {
                    return Err(
                        "`file` pastes its own reference: pass it alone, without keys \
                                or text."
                            .to_owned(),
                    );
                }
                // the CLI reads `-` as stdin, and this server's child has none to give
                if file.trim() == "-" {
                    return Err(
                        "`file` is a path on this server's machine; there is no stdin to read \
                         one from."
                            .to_owned(),
                    );
                }
                if env.remote {
                    // the bytes arrive on stdin - see `plan_in` - and the name keeps the extension
                    // the store and the pane's program go by
                    let expanded = expand_path(&file, env)?;
                    let name = Path::new(&expanded)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let mut rest = vec![
                        "action".to_owned(),
                        "drop-file".to_owned(),
                        "--pane-id".to_owned(),
                        pane,
                    ];
                    if !name.is_empty() {
                        rest.push(flag_value("--name", &name));
                    }
                    rest.extend(["--".to_owned(), "-".to_owned()]);
                    return Ok(scoped(rest));
                }
                return Ok(scoped(vec![
                    "action".to_owned(),
                    "drop-file".to_owned(),
                    "--pane-id".to_owned(),
                    pane,
                    // the path is a file, whatever its first character
                    "--".to_owned(),
                    expand_path(&file, env)?,
                ]));
            }
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
                (None, None) => Err("Nothing to send: pass `keys`, `text` or `file`.".to_owned()),
            }
        },
        "zellij_create" if args.string("reopen").is_some() => {
            let reopen = args.string("reopen").unwrap_or_default();
            // the pane comes back as it was: its tab, its directory, its handle, its line
            if let Some(param) = [
                "kind", "command", "run_in", "cwd", "name", "handle", "brief",
            ]
            .into_iter()
            .find(|param| args.values.contains_key(*param))
            {
                return Err(format!(
                    "`reopen` brings a closed pane back as it was, and takes no `{}`.",
                    param
                ));
            }
            let mut rest = vec!["action".to_owned(), "reopen-pane".to_owned()];
            match reopen.as_str() {
                "" => return Err("`reopen` is `last` or the handle of a closed pane.".to_owned()),
                "last" => rest.push("--last".to_owned()),
                handle => rest.push(flag_value("--handle", handle)),
            }
            Ok(scoped(rest))
        },
        "zellij_create" => {
            create_kind_allowed(&args)?;
            let kind = args.enumerated("kind", CREATE_KIND_DEFAULT);
            let command = args.string("command");
            // `shell` makes a plain shell pane and leaves `command` to be typed into it once it is
            // ready - see `TypeAfter`. `exec` hands `command` to the server as `$SHELL -c`, as
            // every create did before, and waits for nothing
            // an `agent` create is a shell create whose typed line is the harness and its brief
            let shell_mode = run_in(&args)? != "exec";
            let wait_ready = shell_mode && args.flag_or("wait_ready", true);
            // the end of every create's command line: the readiness wait, or the command after
            // `--`, which has to come last because everything after it is the command's own argv
            let finish = |rest: &mut Vec<String>| {
                if wait_ready {
                    rest.push("--wait-ready".to_owned());
                    rest.push(flag_value("--ready-timeout", READY_TIMEOUT_DEFAULT_S));
                }
                if let (false, Some(command)) = (shell_mode, &command) {
                    rest.push("--".to_owned());
                    // this machine's `$SHELL` names a path on this machine
                    let shell = if env.remote { None } else { env.shell.clone() };
                    rest.extend(shell_command(command.clone(), shell));
                }
            };
            if env.remote {
                remote_kind_allowed(&args)?;
            }
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
                        rest.push(flag_value("--cwd", &create_cwd(&cwd, env)?));
                    }
                    // a pane that arrives with its tab is named afterwards, by `RenameAfter`:
                    // `--new-tab` and `--name` conflict, because one flag cannot mean both
                    if placement == TabPlacement::Existing {
                        if let Some(name) = pane_name(&args) {
                            rest.push(flag_value("--name", &name));
                        }
                    }
                    if let Some(handle) = chosen_handle(&args)? {
                        rest.push(flag_value("--handle", &handle));
                    }
                    finish(&mut rest);
                },
                "stacked" => {
                    rest.push("new-pane".to_owned());
                    // Stack onto the caller's pane. The CLI requires ZELLIJ_PANE_ID.
                    rest.push("--near-current-pane".to_owned());
                    rest.push("--stacked".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push(flag_value("--cwd", &create_cwd(&cwd, env)?));
                    }
                    if let Some(name) = pane_name(&args) {
                        rest.push(flag_value("--name", &name));
                    }
                    if let Some(handle) = chosen_handle(&args)? {
                        rest.push(flag_value("--handle", &handle));
                    }
                    finish(&mut rest);
                },
                "tab" => {
                    rest.push("new-tab".to_owned());
                    if let Some(cwd) = args.string("cwd") {
                        rest.push(flag_value("--cwd", &create_cwd(&cwd, env)?));
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
                    finish(&mut rest);
                },
                other => {
                    return Err(format!(
                        "`kind` must be agent_tab, stacked or tab, not `{}`.",
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
        "zellij_show" => {
            let mode = args.enumerated("mode", "auto");
            if !["auto", "kitty", "blocks"].contains(&mode.as_str()) {
                return Err(format!(
                    "`mode` must be auto, kitty or blocks, not `{}`.",
                    mode
                ));
            }
            let mut rest = vec![
                "action".to_owned(),
                "show".to_owned(),
                flag_value("--mode", &mode),
                // every path after this is a file, whatever its first character
                "--".to_owned(),
            ];
            for path in args.strings("paths")? {
                // a remote call's paths are the copies already made on that machine
                rest.push(if env.remote {
                    path
                } else {
                    expand_path(&path, env)?
                });
            }
            Ok(scoped(rest))
        },
        other => Err(format!("`{}` is not a tool of this server.", other)),
    }
}

/// A path as a caller writes it, with `~` and `$VAR` expanded against this server's environment.
/// A relative path stays relative: the CLI resolves it against its own directory, which is this
/// server's, and refuses one that is not a file.
fn expand_path(path: &str, env: &CallEnv) -> Result<String, String> {
    shellexpand::full_with_context(
        path,
        || env.home.clone(),
        |name| Ok::<Option<String>, std::convert::Infallible>((env.var)(name)),
    )
    .map(|expanded| expanded.into_owned())
    .map_err(|e| format!("`{}` could not be expanded: {}", path, e))
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
    /// The call goes to a session on another machine, where this one's `$SHELL`, paths and
    /// files mean nothing.
    remote: bool,
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
            remote: false,
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

/// A create's `cwd`: resolved here for this machine, and passed through for another, where only
/// an absolute path means one thing.
fn create_cwd(cwd: &str, env: &CallEnv) -> Result<String, String> {
    if !env.remote {
        return resolve_cwd(cwd, env);
    }
    if !cwd.starts_with('/') {
        return Err(format!(
            "`cwd` {} is not an absolute path. On another machine `~`, `$VAR` and a relative \
             path have no meaning this server can resolve: write the whole path.",
            cwd
        ));
    }
    Ok(cwd.to_owned())
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
            remote: false,
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
                json!({"pane": "3", "keys": "Ctrl c"}),
                None
            ),
            "action send-keys --pane-id 3 -- Ctrl c"
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
    fn a_file_is_dropped_into_the_pane_and_never_submitted() {
        assert_eq!(
            line(
                "zellij_write_input",
                json!({"pane": "sunny-otter", "file": "-shot.png"}),
                Some("work")
            ),
            "-s work action drop-file --pane-id sunny-otter -- -shot.png"
        );
        for refused in [
            json!({"pane": "3", "file": "a.png", "text": "look"}),
            json!({"pane": "3", "file": "a.png", "keys": "Enter"}),
        ] {
            assert!(
                refusal("zellij_write_input", refused.clone()).contains("pass it alone"),
                "{}",
                refused
            );
        }
        assert!(refusal("zellij_write_input", json!({"pane": "3", "file": "-"})).contains("stdin"));
    }

    #[tokio::test]
    async fn a_file_is_not_submitted_or_mixed_with_an_agent_action() {
        for refused in [
            json!({"pane": "3", "file": "a.png", "submit": true}),
            json!({"pane": "3", "file": "a.png", "agent_action": "approve"}),
        ] {
            let said = plan(
                "zellij_write_input",
                refused.as_object().unwrap(),
                None,
                &Target::Local,
            )
            .await
            .err()
            .expect("refused");
            assert!(said.contains("presses nothing"), "{}", said);
        }
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
    fn a_ready_wait_is_the_cli_ready_wait() {
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"pane": "sunny-otter", "until": "ready"}),
                None
            ),
            "action wait sunny-otter --for ready --timeout 300"
        );
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"pane": "3", "until": "ready", "quiet_ms": 800, "timeout_s": 20}),
                None
            ),
            "action wait 3 --for ready --quiet-ms 800 --timeout 20"
        );
        assert!(
            refusal("zellij_wait_for", json!({"pane": "3", "until": "soon"}))
                .contains("exit, match, quiet, ready, agent_idle, agent_waiting or agent_change")
        );
    }

    #[test]
    fn an_agent_wait_is_the_cli_agent_wait() {
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"pane": "sunny-otter", "until": "agent_idle"}),
                None
            ),
            "action wait sunny-otter --for agent-idle --timeout 300"
        );
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"pane": "3", "until": "agent_waiting", "timeout_s": 60}),
                None
            ),
            "action wait 3 --for agent-waiting --timeout 60"
        );
    }

    #[test]
    fn agent_change_wait_reuses_the_optional_pane_parameter() {
        assert_eq!(
            line("zellij_wait_for", json!({"until": "agent_change"}), None),
            "action wait --for agent-change --timeout 300"
        );
        assert_eq!(
            line(
                "zellij_wait_for",
                json!({"until": "agent_change", "pane": "sunny-otter"}),
                None
            ),
            "action wait sunny-otter --for agent-change --timeout 300"
        );
        assert!(refusal("zellij_wait_for", json!({"until": "exit"})).contains("pane"));
    }

    #[test]
    fn a_last_reply_reads_the_transcript_and_not_the_screen() {
        assert_eq!(
            line(
                "zellij_read_pane",
                json!({"pane": "sunny-otter", "last_reply": true}),
                None
            ),
            "action last-reply --pane-id sunny-otter --json"
        );
        assert!(refusal(
            "zellij_read_pane",
            json!({"pane": "3", "last_reply": true, "full": true})
        )
        .contains("without full or ansi"));
    }

    #[test]
    fn an_agent_action_presses_the_key_the_state_calls_for_or_nothing() {
        // approve takes the prompt's first option, allow once, in every harness that asks
        for kind in ["claude", "codex", "opencode"] {
            assert_eq!(
                agent_key(kind, Some("blocked"), Some("permission"), "approve"),
                Ok("Enter")
            );
        }
        assert_eq!(
            agent_key("claude", Some("blocked"), Some("permission"), "deny"),
            Ok("Esc")
        );
        assert_eq!(
            agent_key("claude", Some("working"), None, "interrupt"),
            Ok("Esc")
        );
        // an agent that reports no state cannot be checked, and gets the key
        assert_eq!(agent_key("opencode", None, None, "approve"), Ok("Enter"));
        // the guard: an Enter to an idle agent would send its draft
        for (state, action) in [
            ("done", "approve"),
            ("idle", "interrupt"),
            ("working", "deny"),
        ] {
            let refused = agent_key("claude", Some(state), None, action).unwrap_err();
            assert!(refused.contains("Nothing was pressed"), "{}", refused);
        }
        assert!(
            agent_key("claude", Some("blocked"), Some("question"), "approve")
                .unwrap_err()
                .contains("asking a question")
        );
        assert!(agent_key("claude", Some("blocked"), Some("permission"), "interrupt").is_err());
        assert!(agent_key("pi", Some("blocked"), None, "approve").is_err());
    }

    #[test]
    fn an_agent_is_found_by_any_name_its_pane_answers_to() {
        let rows = r#"[{"handle":"sunny-otter","pane_id":3,"kind":"claude"}]"#;
        for name in ["sunny-otter", "3", "terminal_3"] {
            assert!(agent_row(rows, name).is_some(), "{}", name);
        }
        assert!(agent_row(rows, "4").is_none());
        assert!(agent_row("not json", "3").is_none());
    }

    #[tokio::test]
    async fn a_submitted_message_is_a_paste_then_enter() {
        let plan = plan_in(
            "zellij_write_input",
            &args(json!({"pane": "sunny-otter", "text": "line one\nline two", "submit": true})),
            Some("work"),
            &a_shell_env(),
            &Target::Local,
        )
        .await
        .ok()
        .unwrap();
        let Call::Sequence(argvs) = plan.call else {
            panic!("not a sequence");
        };
        assert_eq!(
            argvs,
            vec![
                vec![
                    "-s",
                    "work",
                    "action",
                    "paste",
                    "--pane-id=sunny-otter",
                    "--",
                    "line one\nline two"
                ],
                vec![
                    "-s",
                    "work",
                    "action",
                    "send-keys",
                    "--pane-id=sunny-otter",
                    "--",
                    "Enter"
                ],
            ]
        );
        for refused in [
            json!({"pane": "3", "submit": true}),
            json!({"pane": "3", "keys": "Enter", "submit": true}),
            json!({"pane": "3", "agent_action": "approve", "text": "x"}),
            json!({"pane": "3", "agent_action": "maybe"}),
        ] {
            assert!(
                plan_in(
                    "zellij_write_input",
                    &args(refused.clone()),
                    None,
                    &a_shell_env(),
                    &Target::Local,
                )
                .await
                .is_err(),
                "{}",
                refused
            );
        }
    }

    #[test]
    fn my_agents_is_the_agent_walk_filtered_to_my_tab_with_ages() {
        assert_eq!(
            line(
                "zellij_overview",
                json!({"scope": "my_agents"}),
                Some("work")
            ),
            "-s work action list-agents --json --report-withheld"
        );
        let answer = r#"{"agents": [
            {"handle": "a", "name": "project-1", "self": true, "tab_name": "dev-zj", "state": "working", "state_at": 1000000,
             "last_output_at": 1055000},
            {"handle": "b", "tab_name": "dev", "state": "done", "state_at": 1000000}
        ], "withheld": 0}"#;
        let mine = AgentsView {
            only_tab: Some("dev-zj".to_owned()),
        }
        .shape(answer, 1_060_000)
        .unwrap();
        let mine: Value = serde_json::from_str(&mine).unwrap();
        let rows = mine["agents"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["handle"], "a");
        assert_eq!(rows[0]["name"], "project-1");
        assert_eq!(rows[0]["self"], true);
        assert_eq!(rows[0]["state_age_s"], 60);
        assert_eq!(rows[0]["output_age_s"], 5);
        assert_eq!(mine["withheld"], 0);
        // every agent, from a bare array too; a row with no output keeps no output age
        let long = format!("claude -- {}\nmore", "x".repeat(300));
        let all = AgentsView { only_tab: None }
            .shape(
                &json!([{"handle": "b", "state_at": 1000000, "command": long}]).to_string(),
                1_002_500,
            )
            .unwrap();
        let all: Value = serde_json::from_str(&all).unwrap();
        assert_eq!(all[0]["state_age_s"], 2);
        assert!(all[0].get("output_age_s").is_none());
        let command = all[0]["command"].as_str().unwrap();
        assert_eq!(command.chars().count(), AGENT_COMMAND_MAX);
        assert!(
            command.ends_with('…') && !command.contains('\n'),
            "{}",
            command
        );
        assert!(AgentsView { only_tab: None }.shape("not json", 0).is_none());
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
    fn a_create_makes_a_shell_pane_and_waits_for_it_by_default() {
        // the command is not handed to the server: it is typed into the shell once it is ready
        assert_eq!(
            line(
                "zellij_create",
                json!({"kind": "stacked", "command": "cargo test"}),
                Some("work")
            ),
            "-s work action new-pane --near-current-pane --stacked --name=cargo test --wait-ready \
             --ready-timeout=10"
        );
        assert_eq!(
            line(
                "zellij_create",
                json!({"kind": "tab", "command": "make"}),
                None
            ),
            "action new-tab --wait-ready --ready-timeout=10"
        );
        assert_eq!(
            in_agent_tab(
                json!({"command": "cargo test"}),
                Some("work"),
                "work-zj",
                TabPlacement::Made
            ),
            "-s work action new-pane --new-tab=work-zj --no-focus --wait-ready --ready-timeout=10"
        );
        // a pane with nothing to type still waits, so the answer is a pane ready for input
        assert_eq!(
            line("zellij_create", json!({"kind": "stacked"}), None),
            "action new-pane --near-current-pane --stacked --wait-ready --ready-timeout=10"
        );
        // and the one bound is the one the CLI applies by default
        assert_eq!(READY_TIMEOUT_DEFAULT_S, "10");
    }

    #[test]
    fn reopen_runs_reopen_pane_and_takes_nothing_else() {
        assert_eq!(
            line("zellij_create", json!({"reopen": "last"}), Some("work")),
            "-s work action reopen-pane --last"
        );
        assert_eq!(
            line("zellij_create", json!({"reopen": "sunny-otter"}), None),
            "action reopen-pane --handle=sunny-otter"
        );
        let refused = argv_in(
            "zellij_create",
            &args(json!({"reopen": "last", "command": "ls"})),
            None,
            &a_shell_env(),
            None,
        )
        .unwrap_err();
        assert!(refused.contains("`command`"), "{}", refused);
    }

    #[test]
    fn exec_hands_the_command_to_the_server_and_waits_for_nothing() {
        // today's behaviour, for a one-shot command whose exit status the caller wants
        for wait_ready in [true, false] {
            assert_eq!(
                line(
                    "zellij_create",
                    json!({"kind": "stacked", "command": "make", "run_in": "exec",
                           "wait_ready": wait_ready}),
                    None
                ),
                "action new-pane --near-current-pane --stacked --name=make -- /bin/zsh -c make"
            );
        }
        // the negative control: a shell pane that is told not to wait types at once
        assert_eq!(
            line(
                "zellij_create",
                json!({"kind": "stacked", "command": "make", "wait_ready": false}),
                None
            ),
            "action new-pane --near-current-pane --stacked --name=make"
        );
        assert!(refusal(
            "zellij_create",
            json!({"kind": "stacked", "command": "make", "run_in": "login"})
        )
        .contains("shell, exec or agent"));
    }

    #[tokio::test]
    async fn a_shell_create_types_its_command_into_the_pane_it_made_and_an_exec_one_does_not() {
        let plan = plan_in(
            "zellij_create",
            &args(json!({"kind": "stacked", "command": "cd /tmp && ls"})),
            Some("work"),
            &a_shell_env(),
            &Target::Local,
        )
        .await
        .expect("a plan");
        let then_type = plan.then_type.expect("a command to type");
        let [write, enter] = then_type.argvs("terminal_7");
        assert_eq!(
            write.join(" "),
            "-s work action write-chars --pane-id=terminal_7 -- cd /tmp && ls"
        );
        assert_eq!(
            enter.join(" "),
            "-s work action send-keys --pane-id=terminal_7 -- Enter"
        );
        // the text goes as one word, whatever is in it: the shell in the pane reads it
        assert_eq!(write.last().map(String::as_str), Some("cd /tmp && ls"));
        for exec_or_nothing in [
            json!({"kind": "stacked", "command": "ls", "run_in": "exec"}),
            json!({"kind": "stacked"}),
        ] {
            let plan = plan_in(
                "zellij_create",
                &args(exec_or_nothing.clone()),
                None,
                &a_shell_env(),
                &Target::Local,
            )
            .await
            .expect("a plan");
            assert!(plan.then_type.is_none(), "{}", exec_or_nothing);
        }
        // and no other tool types anything
        let plan = plan_in(
            "zellij_read_pane",
            &args(json!({"pane": "3"})),
            None,
            &a_shell_env(),
            &Target::Local,
        )
        .await
        .expect("a plan");
        assert!(plan.then_type.is_none());
    }

    #[tokio::test]
    async fn an_agent_create_types_the_harness_with_its_brief_as_the_first_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let brief = dir.path().join("it's a brief.md");
        std::fs::write(&brief, "- fix the test\n").unwrap();
        let env = call_env(Some("/bin/zsh"), Some("/home/tester"), dir.path(), &[]);
        let typed = |value: Value| {
            let env = &env;
            async move {
                plan_in("zellij_create", &args(value), None, env, &Target::Local)
                    .await
                    .map(|plan| plan.then_type.map(|then_type| then_type.command))
            }
        };
        let quoted = format!(
            "\"$(cat '{}')\"",
            brief.to_string_lossy().replace('\'', "'\\''")
        );
        assert_eq!(
            typed(
                json!({"kind": "stacked", "run_in": "agent", "command": "claude --model haiku",
                         "brief": "it's a brief.md"})
            )
            .await,
            Ok(Some(format!("claude --model haiku -- {}", quoted)))
        );
        // opencode takes its first prompt as a flag
        assert_eq!(
            typed(
                json!({"kind": "stacked", "run_in": "agent", "command": "opencode",
                         "brief": brief.to_string_lossy()})
            )
            .await,
            Ok(Some(format!("opencode --prompt {}", quoted)))
        );
        // no brief starts the harness bare
        assert_eq!(
            typed(json!({"kind": "stacked", "run_in": "agent", "command": "/usr/bin/codex -m o3"}))
                .await,
            Ok(Some("/usr/bin/codex -m o3".to_owned()))
        );
        for (refused, says) in [
            (
                json!({"kind": "stacked", "run_in": "agent", "command": "vim"}),
                "not a coding agent",
            ),
            (
                json!({"kind": "stacked", "run_in": "agent"}),
                "needs `command`",
            ),
            (
                json!({"kind": "stacked", "run_in": "agent", "command": "pi", "brief": "nope.md"}),
                "is not a file",
            ),
            (
                json!({"kind": "stacked", "command": "ls", "brief": "it's a brief.md"}),
                "with run_in=agent",
            ),
        ] {
            let error = typed(refused.clone()).await.unwrap_err();
            assert!(error.contains(says), "{}: {}", refused, error);
        }
        // an agent pane waits for its shell like any shell create
        assert!(line(
            "zellij_create",
            json!({"kind": "stacked", "run_in": "agent", "command": "claude"}),
            None
        )
        .contains("--wait-ready"));
    }

    #[test]
    fn create_only_accepts_stacks_and_tabs() {
        let stacked = line("zellij_create", json!({"kind": "stacked"}), None);
        assert!(stacked.contains("--near-current-pane"));
        assert!(stacked.contains("--stacked"));
        assert!(!stacked.contains("--floating"));
        for kind in ["pane", "floating"] {
            assert!(refusal("zellij_create", json!({"kind": kind}))
                .contains("must be agent_tab, stacked or tab"));
        }
    }

    #[test]
    fn a_created_panes_command_comes_after_the_double_dash() {
        assert_eq!(
            line(
                "zellij_create",
                json!({"kind": "stacked", "command": "cargo test", "handle": "test-run", "run_in": "exec"}),
                Some("work")
            ),
            "-s work action new-pane --near-current-pane --stacked --name=cargo test --handle=test-run -- \
             /bin/zsh -c cargo test"
        );
    }

    #[test]
    fn a_handle_the_cli_would_refuse_is_refused_before_anything_runs() {
        // lowercase words joined by dashes, and still not a handle: it reads as plugin_1
        let refused = refusal(
            "zellij_create",
            json!({"kind": "stacked", "handle": "plugin-check"}),
        );
        assert!(
            refused.contains("too close to the pane id"),
            "got: {}",
            refused
        );
        assert!(refusal(
            "zellij_create",
            json!({"kind": "stacked", "handle": "Build"})
        )
        .contains("lowercase"));
        assert!(line(
            "zellij_create",
            json!({"kind": "stacked", "handle": "check-plugin"}),
            None
        )
        .contains("--handle=check-plugin"));
    }

    #[tokio::test]
    async fn an_agent_tab_create_refuses_a_bad_handle_before_asking_the_session() {
        // `agent_tab` asks the session which tab it is in; a handle the CLI would refuse is
        // refused first, so no process is spawned to find out what the answer already is
        let call = plan_in(
            "zellij_create",
            &args(json!({"handle": "terminal-1"})),
            Some("no-such-session-mcp-test"),
            &a_shell_env(),
            &Target::Local,
        )
        .await;
        assert!(matches!(call, Err(reason) if reason.contains("too close to the pane id")));
    }

    #[test]
    fn a_pane_opens_beside_the_agents_own_pane_and_not_beside_the_persons() {
        // bare `new-pane` lands beside whichever pane a person is focused on, so identical calls
        // used to land in different places and shrink whatever the person was reading
        assert!(line("zellij_create", json!({"kind": "stacked"}), None)
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
                json!({"command": "cargo test", "run_in": "exec"}),
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
                json!({"command": "cargo test", "run_in": "exec"}),
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
                json!({"name": "--force", "command": "echo hi", "run_in": "exec"}),
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
                json!({"kind": "stacked", "command": "--force --now", "run_in": "exec"}),
                None
            ),
            "action new-pane --near-current-pane --stacked --name=--force --now -- /bin/zsh -c --force --now"
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
            json!({"kind": "stacked", "command": "cargo test", "name": "tests"}),
            None
        )
        .contains("--name=tests --"));
        // and a pane with no command is not given one
        assert!(!line("zellij_create", json!({"kind": "stacked"}), None).contains("--name"));
    }

    #[test]
    fn create_refuses_the_removed_floating_parameter() {
        // Removed parameters fail before the create reaches the CLI.
        let said = argv_in(
            "zellij_create",
            &args(json!({"floating": true})),
            None,
            &a_shell_env(),
            Some(("work-zj", TabPlacement::Existing)),
        )
        .expect_err("a floating pane in the agent's tab");
        assert!(said.contains("`floating` is unsupported"), "{}", said);
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
                json!({"kind": "stacked", "command": "pwd", "run_in": "exec"}),
                None,
                &no_shell
            )
            .join(" "),
            "action new-pane --near-current-pane --stacked --name=pwd -- /bin/sh -c pwd"
        );
    }

    #[test]
    fn a_command_reaches_the_shell_as_one_word_whatever_is_in_it() {
        // the whole point: `&&`, the quotes, the pipe and the `$HOME` are the shell's to read, and
        // a split here would hand them to the first word as arguments
        let command = "cd /tmp && echo \"hello world\" $HOME | cat";
        let argv = built(
            "zellij_create",
            json!({"kind": "stacked", "command": command, "name": "shell", "run_in": "exec"}),
            None,
            &a_shell_env(),
        );
        assert_eq!(
            argv,
            vec![
                "action",
                "new-pane",
                "--near-current-pane",
                "--stacked",
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
            json!({"kind": "tab", "command": command, "run_in": "exec"}),
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
                    json!({"kind": "stacked", "cwd": cwd, "wait_ready": false}),
                    None,
                    &env
                )
                .join(" "),
                format!(
                    "action new-pane --near-current-pane --stacked --cwd={}",
                    expected
                )
            );
            assert_eq!(
                built(
                    "zellij_create",
                    json!({"kind": "tab", "cwd": cwd, "wait_ready": false}),
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
                json!({"kind": "stacked", "cwd": "work", "wait_ready": false}),
                None,
                &env
            )
            .join(" "),
            format!(
                "action new-pane --near-current-pane --stacked --cwd={}",
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
                &args(json!({"kind": "stacked", "cwd": cwd, "wait_ready": false})),
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
                .contains("`floating` is unsupported")
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

    #[test]
    fn showing_images_is_the_show_verb_with_every_path_after_a_double_dash() {
        assert_eq!(
            line(
                "zellij_show",
                json!({"paths": ["/s/a.png", "-odd name.png"]}),
                Some("work")
            ),
            "-s work action show --mode=auto -- /s/a.png -odd name.png"
        );
        // one argv word per path, so a name with a space is not split
        let argv = built(
            "zellij_show",
            json!({"paths": ["two words.png"], "mode": "blocks"}),
            None,
            &a_shell_env(),
        );
        assert_eq!(
            argv,
            vec!["action", "show", "--mode=blocks", "--", "two words.png"]
        );
    }

    #[test]
    fn a_shown_path_is_expanded_against_this_servers_environment() {
        let env = call_env(
            None,
            Some("/home/tester"),
            Path::new("/"),
            &[("SHOTS", "/var/shots")],
        );
        assert_eq!(
            built(
                "zellij_show",
                json!({"paths": ["~/a.png", "$SHOTS/b.png", "rel/c.png"]}),
                None,
                &env
            )
            .join(" "),
            "action show --mode=auto -- /home/tester/a.png /var/shots/b.png rel/c.png"
        );
        // a lone string is a list of one
        assert_eq!(
            built("zellij_show", json!({"paths": "/a.png"}), None, &env).join(" "),
            "action show --mode=auto -- /a.png"
        );
    }

    #[test]
    fn showing_nothing_or_in_an_unknown_mode_is_refused() {
        assert!(refusal("zellij_show", json!({})).contains("`paths` is required"));
        assert!(refusal("zellij_show", json!({"paths": []})).contains("`paths` is required"));
        assert!(refusal("zellij_show", json!({"paths": [""]})).contains("non-empty strings"));
        assert!(
            refusal("zellij_show", json!({"paths": ["a.png"], "mode": "sixel"}))
                .contains("`mode` must be auto, kitty or blocks")
        );
    }

    /// The process a call to another machine is built in.
    fn a_remote_env() -> CallEnv {
        let mut env = a_shell_env();
        env.remote = true;
        env
    }

    #[test]
    fn a_remote_drop_reads_stdin_and_keeps_the_name() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot one.png");
        std::fs::write(&shot, b"png").unwrap();
        assert_eq!(
            built(
                "zellij_write_input",
                json!({"pane": "sunny-otter", "file": shot.display().to_string()}),
                Some("work"),
                &a_remote_env()
            )
            .join(" "),
            "-s work action drop-file --pane-id sunny-otter --name=shot one.png -- -"
        );
    }

    #[tokio::test]
    async fn a_remote_drop_carries_the_bytes_of_the_local_file() {
        let dir = tempfile::tempdir().unwrap();
        let shot = dir.path().join("shot.png");
        std::fs::write(&shot, b"\x89PNG bytes").unwrap();
        let host = remote::RemoteHost {
            name: "boxa".into(),
            bin: None,
        };
        let plan = plan_in(
            "zellij_write_input",
            &args(
                json!({"pane": "sunny-otter", "file": shot.display().to_string(),
                         "session": "work"}),
            ),
            None,
            &a_remote_env(),
            &Target::Remote(host.clone()),
        )
        .await
        .ok()
        .expect("a plan");
        assert_eq!(plan.stdin.as_deref(), Some(&b"\x89PNG bytes"[..]));
        // and a file that is not there is refused before ssh is run
        let missing = plan_in(
            "zellij_write_input",
            &args(json!({"pane": "sunny-otter", "file": "/nonexistent/shot.png"})),
            None,
            &a_remote_env(),
            &Target::Remote(host),
        )
        .await;
        assert!(matches!(missing, Err(reason) if reason.contains("is not a file")));
    }

    #[test]
    fn a_remote_create_takes_only_an_absolute_cwd_and_runs_exec_through_sh() {
        let line = built(
            "zellij_create",
            json!({"kind": "tab", "cwd": "/srv/app", "command": "make", "run_in": "exec"}),
            Some("work"),
            &a_remote_env(),
        )
        .join(" ");
        assert_eq!(
            line, "-s work action new-tab --cwd=/srv/app -- /bin/sh -c make",
            "this machine's $SHELL is not a path there"
        );
        for cwd in ["~/app", "$HOME/app", "app"] {
            let refused = argv_in(
                "zellij_create",
                &args(json!({"kind": "tab", "cwd": cwd})),
                None,
                &a_remote_env(),
                None,
            )
            .expect_err("refused");
            assert!(refused.contains("not an absolute path"), "{}", refused);
        }
    }

    #[test]
    fn a_remote_create_does_not_go_beside_a_pane_it_cannot_see() {
        for value in [json!({"kind": "stacked"})] {
            let refused =
                argv_in("zellij_create", &args(value), None, &a_remote_env(), None).unwrap_err();
            assert!(
                refused.contains("agent_tab, the default, or tab"),
                "{}",
                refused
            );
        }
    }

    #[tokio::test]
    async fn removed_create_placements_are_refused_before_reading_or_uploading_a_brief() {
        let host = remote::RemoteHost {
            name: "boxa".into(),
            bin: None,
        };
        for (placement, expected) in [
            (json!({"kind": "pane"}), "must be agent_tab, stacked or tab"),
            (
                json!({"kind": "floating"}),
                "must be agent_tab, stacked or tab",
            ),
            (
                json!({"kind": "other"}),
                "must be agent_tab, stacked or tab",
            ),
            (json!({"floating": true}), "`floating` is unsupported"),
            (json!({"floating": false}), "`floating` is unsupported"),
        ] {
            let mut request = args(placement);
            request.insert("command".into(), json!("claude"));
            request.insert("run_in".into(), json!("agent"));
            // A missing brief proves placement fails before file access or SSH.
            request.insert("brief".into(), json!("/nonexistent/brief.md"));
            let result = plan_in(
                "zellij_create",
                &request,
                None,
                &a_remote_env(),
                &Target::Remote(host.clone()),
            )
            .await;
            assert!(matches!(result, Err(reason) if reason.contains(expected)));
        }
    }

    #[test]
    fn a_remote_show_passes_the_copies_paths_as_they_are() {
        assert_eq!(
            built(
                "zellij_show",
                json!({"paths": ["/tmp/zellij-mcp-files-501/x-shot.png"]}),
                Some("work"),
                &a_remote_env()
            )
            .join(" "),
            "-s work action show --mode=auto -- /tmp/zellij-mcp-files-501/x-shot.png"
        );
    }

    #[test]
    fn a_remote_agent_reads_its_brief_from_the_copy() {
        let dir = tempfile::tempdir().unwrap();
        let brief = dir.path().join("brief.md");
        std::fs::write(&brief, "do the thing").unwrap();
        let read = args(json!({"command": "claude", "run_in": "agent",
                               "brief": brief.display().to_string()}));
        let typed = typed_command(
            &Args::new(&read),
            &a_remote_env(),
            Some("/tmp/zellij-mcp-files-501/x-brief.md"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            typed,
            "claude -- \"$(cat '/tmp/zellij-mcp-files-501/x-brief.md')\""
        );
    }
}
