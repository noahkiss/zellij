//! The `[cli_client]` is used to attach to a running server session
//! and dispatch actions, that are specified through the command line.
use std::collections::{BTreeMap, HashSet};
use std::io::{self, BufRead, Write};
use std::process;
use std::str::FromStr;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};
use std::{fs, path::PathBuf};

use crate::os_input_output::ClientOsApi;
use regex::Regex;
use uuid::Uuid;
use zellij_utils::{
    agent_detect,
    cli::{SubscribeCli, SubscribeFormat},
    data::{PaneId, PaneListEntry},
    errors::prelude::*,
    input::actions::Action,
    ipc::{ClientToServerMsg, ExitReason, ServerToClientMsg},
};

/// Runs the actions one CLI invocation turned into.
///
/// `anchor_pane` is the pane these actions should be read as coming from - what `--near` names. A
/// `zellij action` client is not attached to anything, so "the pane this came from" is normally the
/// ambient `$ZELLIJ_PANE_ID`, and this is the same answer given deliberately instead of inherited.
/// It is a terminal id because that is what the message carries; the refusal for anything else
/// happens before this, where there is a caller to tell.
pub fn start_cli_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    actions: Vec<Action>,
    anchor_pane: Option<u32>,
    mut chosen_handle: Option<String>,
) -> i32 {
    let zellij_ipc_pipe: PathBuf = {
        let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        fs::create_dir_all(&sock_dir).unwrap();
        zellij_utils::shared::set_permissions(&sock_dir, 0o700).unwrap();
        sock_dir.push(session_name);
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    os_input.connect_to_server(&*zellij_ipc_pipe);
    let pane_id = anchor_pane.or_else(|| {
        os_input
            .env_variable("ZELLIJ_PANE_ID")
            .and_then(|e| e.trim().parse().ok())
    });

    for action in actions {
        match action {
            Action::CliPipe {
                pipe_id,
                name,
                payload,
                plugin,
                args,
                configuration,
                launch_new,
                skip_cache,
                floating,
                in_place,
                cwd,
                pane_title,
            } => {
                pipe_client(
                    &mut os_input,
                    pipe_id,
                    name,
                    payload,
                    plugin,
                    args,
                    configuration,
                    launch_new,
                    skip_cache,
                    floating,
                    in_place,
                    pane_id,
                    cwd,
                    pane_title,
                );
            },
            action => {
                let promised_a_pane = action_reports_a_new_pane(&action);
                let lines = match individual_messages_client(&mut os_input, action, pane_id) {
                    ActionOutcome::Done(exit_status) => return exit_status,
                    ActionOutcome::Reported(lines) => lines,
                };
                // a verb that makes a pane answers with the pane it made, and the session writes
                // that line only once a tab has taken the pane. No line means no pane, and the
                // caller is about to address one - so this is a miss, said out loud, rather than a
                // report of an id nothing answers to
                if promised_a_pane && reported_pane(&lines).is_none() {
                    eprintln!(
                        "No pane was created: the session took the request and reported no pane."
                    );
                    return 2;
                }
                // the pane named itself when it was born; this is the name its creator asked for,
                // applied before the report is printed so the report says the one the caller has
                let lines = match chosen_handle.take() {
                    None => lines,
                    Some(handle) => {
                        let Some(made) = reported_pane(&lines) else {
                            eprintln!(
                                "The pane was made but reported no id, so it could not be given \
                                 the handle '{}'.",
                                handle
                            );
                            return 1;
                        };
                        match individual_messages_client(
                            &mut os_input,
                            Action::SetPaneHandle {
                                pane_id: made,
                                handle: handle.clone(),
                            },
                            pane_id,
                        ) {
                            ActionOutcome::Done(exit_status) => return exit_status,
                            ActionOutcome::Reported(_) => with_handle(lines, &handle),
                        }
                    },
                };
                lines.iter().for_each(|line| println!("{line}"));
            },
        }
    }
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    0
}

fn pipe_client(
    os_input: &mut Box<dyn ClientOsApi>,
    pipe_id: String,
    mut name: Option<String>,
    mut payload: Option<String>,
    plugin: Option<String>,
    args: Option<BTreeMap<String, String>>,
    mut configuration: Option<BTreeMap<String, String>>,
    launch_new: bool,
    skip_cache: bool,
    floating: Option<bool>,
    in_place: Option<bool>,
    pane_id: Option<u32>,
    cwd: Option<PathBuf>,
    pane_title: Option<String>,
) {
    let mut stdin = os_input.get_stdin_reader();
    let name = name
        // first we try to take the explicitly supplied message name
        .take()
        // then we use the plugin, to facilitate using aliases
        .or_else(|| plugin.clone())
        // then we use a uuid to at least have some sort of identifier for this message
        .or_else(|| Some(Uuid::new_v4().to_string()));
    if launch_new {
        // we do this to make sure the plugin is unique (has a unique configuration parameter) so
        // that a new one would be launched, but we'll still send it to the same instance rather
        // than launching a new one in every iteration of the loop
        configuration
            .get_or_insert_with(BTreeMap::new)
            .insert("_zellij_id".to_owned(), Uuid::new_v4().to_string());
    }
    let create_msg = |payload: Option<String>| -> ClientToServerMsg {
        ClientToServerMsg::Action {
            action: Action::CliPipe {
                pipe_id: pipe_id.clone(),
                name: name.clone(),
                payload,
                args: args.clone(),
                plugin: plugin.clone(),
                configuration: configuration.clone(),
                floating,
                in_place,
                launch_new,
                skip_cache,
                cwd: cwd.clone(),
                pane_title: pane_title.clone(),
            },
            terminal_id: pane_id,
            client_id: None,
            is_cli_client: true,
        }
    };
    let is_piped = !os_input.stdin_is_terminal();
    loop {
        if let Some(payload) = payload.take() {
            let msg = create_msg(Some(payload));
            os_input.send_to_server(msg);
        } else if !is_piped {
            // here we send an empty message to trigger the plugin, because we don't have any more
            // data
            let msg = create_msg(None);
            os_input.send_to_server(msg);
        } else {
            // we didn't get payload from the command line, meaning we listen on STDIN because this
            // signifies the user is about to pipe more (eg. cat my-large-file | zellij pipe ...)
            let mut buffer = String::new();
            let _ = stdin.read_line(&mut buffer);
            if buffer.is_empty() {
                let msg = create_msg(None);
                os_input.send_to_server(msg);
                break;
            } else {
                // we've got data! send it down the pipe (most common)
                let msg = create_msg(Some(buffer));
                os_input.send_to_server(msg);
            }
        }
        loop {
            // wait for a response and act accordingly
            match os_input.recv_from_server() {
                Some((ServerToClientMsg::UnblockCliPipeInput { pipe_name }, _)) => {
                    // unblock this pipe, meaning we need to stop waiting for a response and read
                    // once more from STDIN
                    if pipe_name == pipe_id {
                        if !is_piped {
                            // if this client is not piped, we need to exit the process completely
                            // rather than wait for more data
                            process::exit(0);
                        } else {
                            break;
                        }
                    }
                },
                Some((ServerToClientMsg::CliPipeOutput { pipe_name, output }, _)) => {
                    // send data to STDOUT, this *does not* mean we need to unblock the input
                    let err_context = "Failed to write to stdout";
                    if pipe_name == pipe_id {
                        let mut stdout = os_input.get_stdout_writer();
                        stdout
                            .write_all(output.as_bytes())
                            .context(err_context)
                            .non_fatal();
                        stdout.flush().context(err_context).non_fatal();
                    }
                },
                Some((ServerToClientMsg::Log { lines: log_lines }, _)) => {
                    log_lines.iter().for_each(|line| println!("{line}"));
                    process::exit(0);
                },
                Some((ServerToClientMsg::LogError { lines: log_lines }, _)) => {
                    log_lines.iter().for_each(|line| eprintln!("{line}"));
                    process::exit(2);
                },
                Some((ServerToClientMsg::Exit { exit_reason }, _)) => match exit_reason {
                    ExitReason::Error(e) => {
                        eprintln!("{}", e);
                        process::exit(2);
                    },
                    _ => {
                        process::exit(0);
                    },
                },
                _ => {},
            }
        }
    }
}

/// Asks the running session one question, on a connection opened for it and closed after it.
///
/// The CLI holds a string; only the server holds the panes and the tabs. So a question that has to
/// be answered before the real action can be built is asked here, ahead of it, and what comes back
/// is the report's own lines - `Ok(vec![])` when the command found nothing to report, which is how
/// a probe says "not there".
///
/// `subject` is what the question was about, and appears in the message if the session does not
/// answer. `Err` carries the server's own words wherever it had any.
fn ask(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    action: Action,
    subject: &str,
) -> Result<Vec<String>, String> {
    let zellij_ipc_pipe: PathBuf = {
        let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        fs::create_dir_all(&sock_dir).map_err(|e| e.to_string())?;
        zellij_utils::shared::set_permissions(&sock_dir, 0o700).map_err(|e| e.to_string())?;
        sock_dir.push(session_name);
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    os_input.connect_to_server(&*zellij_ipc_pipe);
    os_input.send_to_server(ClientToServerMsg::Action {
        action,
        terminal_id: None,
        client_id: None,
        is_cli_client: true,
    });
    let answer = loop {
        match os_input.recv_from_server() {
            Some((ServerToClientMsg::Log { lines }, _)) => break Ok(lines),
            // the server is free again and said nothing: the command reported nothing, which for a
            // question means the thing it asked about is not there
            Some((ServerToClientMsg::UnblockInputThread, _)) => break Ok(Vec::new()),
            Some((ServerToClientMsg::LogError { lines }, _)) => break Err(lines.join("\n")),
            Some((ServerToClientMsg::Exit { exit_reason }, _)) => {
                break Err(match exit_reason {
                    ExitReason::Error(e) => e,
                    _ => format!("The session exited while asking about '{}'", subject),
                });
            },
            Some(_) => {},
            None => break Err(format!("The session did not answer for '{}'", subject)),
        }
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    answer
}

/// Asks the running session which pane a handle or uuid names.
///
/// Asked before the real action is built - so by the time the action leaves, it names a pane id like
/// it always did, and nothing downstream has to know a handle existed.
///
/// `Err` carries the server's own message, which is what the caller prints before exiting 2.
pub fn resolve_pane_target(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    target: &str,
) -> Result<PaneId, String> {
    let lines = ask(
        os_input,
        session_name,
        Action::ResolvePaneTarget {
            target: target.to_owned(),
        },
        target,
    )?;
    // `pane_id: terminal_7`, the key-value shape every reporting command answers in
    lines
        .first()
        .and_then(|line| line.strip_prefix("pane_id: "))
        .ok_or_else(|| format!("Could not read the resolved pane for '{}'", target))
        .and_then(|id| {
            PaneId::from_str(id)
                .map_err(|e| format!("Could not read the resolved pane for '{}': {}", target, e))
        })
}

/// Asks the running session which tab a name or a stable id names.
///
/// `Ok(None)` is the miss: the session answered, and no tab is that one. Both forms are read out of
/// the same `list-tabs` answer, so an id that no tab holds is a miss like a name that no tab has,
/// rather than a number that quietly reaches nothing.
pub fn resolve_tab_target(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    wanted: &str,
) -> Result<Option<usize>, String> {
    let lines = ask(
        os_input,
        session_name,
        Action::ListTabs {
            show_state: false,
            show_dimensions: false,
            show_panes: false,
            show_layout: false,
            show_all: false,
            output_json: true,
        },
        wanted,
    )?;
    let tabs: Vec<zellij_utils::data::TabInfo> = serde_json::from_str(&lines.join("\n"))
        .map_err(|e| format!("Could not read the session's tabs: {}", e))?;
    // an all-digits value is the stable id from the TAB_ID column; anything else is a name. A tab
    // may be *named* "3", and the id is what wins - the names are the caller's to change
    if let Ok(id) = wanted.parse::<usize>() {
        if tabs.iter().any(|tab| tab.tab_id == id) {
            return Ok(Some(id));
        }
        return Ok(None);
    }
    Ok(tabs
        .iter()
        .find(|tab| tab.name == wanted)
        .map(|tab| tab.tab_id))
}

/// Whether this action's answer is a report of its own, rather than "the server is free again".
///
/// `UnblockInputThread` is addressed to whoever is holding the input, and every route thread sends
/// one to its own client when it finishes handling a message. For most actions that is the answer.
/// For a blocking one it is not: the answer is the command's exit status, which arrives later as
/// `Exit`, `Log` or `LogError`. A client that took the unblock would print nothing and exit 0 while
/// the command it was waiting for is still running.
///
/// Every `NewBlockingPane` counts, condition or none: bare `-b/--blocking` waits for the pane to
/// close rather than for a status to match, and it is no less blocking for that.
fn action_answers_with_its_own_report(action: &Action) -> bool {
    matches!(action, Action::NewBlockingPane { .. })
}

/// Whether this action's whole point is to make a pane, so that a report without one is a miss.
///
/// The pty is spawned before any tab has agreed to hold the pane, so "the action ran" and "the pane
/// exists" are two different facts. The session reports the id only for the second, which leaves
/// the client one thing to decide: whether an absent `pane_id:` line is an answer or a hole. For
/// these verbs it is a hole, because the caller asked for a pane and has nothing to address.
///
/// `NewBlockingPane` is deliberately absent: it is answered by the command's exit status, not by an
/// id, and it prints no `pane_id:` line even when it works. `NewTab` is absent for the same kind of
/// reason - it is a tab that is asked for, and the pane inside it is a detail of the report.
fn action_reports_a_new_pane(action: &Action) -> bool {
    matches!(
        action,
        Action::NewPane { .. }
            | Action::NewTiledPane { .. }
            | Action::NewFloatingPane { .. }
            | Action::NewStackedPane { .. }
            | Action::NewInPlacePane { .. }
            | Action::NewTiledPluginPane { .. }
            | Action::NewFloatingPluginPane { .. }
            | Action::NewInPlacePluginPane { .. }
            | Action::Run { .. }
            | Action::EditFile { .. }
    )
}

/// What came back from one action: the lines it wants printed, or the status the command ends on.
enum ActionOutcome {
    /// The action's report. It is returned rather than printed because a `--handle` still has a
    /// line of it to correct, and a report is printed once or not at all.
    Reported(Vec<String>),
    /// Nothing more will come: this is the exit status. Anything to say has been said on stderr.
    Done(i32),
}

fn individual_messages_client(
    os_input: &mut Box<dyn ClientOsApi>,
    action: Action,
    pane_id: Option<u32>,
) -> ActionOutcome {
    let is_blocking = action_answers_with_its_own_report(&action);
    let msg = ClientToServerMsg::Action {
        action,
        terminal_id: pane_id,
        client_id: None,
        is_cli_client: true,
    };
    os_input.send_to_server(msg);
    loop {
        match os_input.recv_from_server() {
            Some((ServerToClientMsg::UnblockInputThread, _)) if !is_blocking => {
                return ActionOutcome::Reported(Vec::new());
            },
            Some((ServerToClientMsg::Log { lines: log_lines }, _)) => {
                return ActionOutcome::Reported(log_lines);
            },
            Some((ServerToClientMsg::LogError { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| eprintln!("{line}"));
                return ActionOutcome::Done(2);
            },
            Some((ServerToClientMsg::Exit { exit_reason }, _)) => match exit_reason {
                ExitReason::Error(e) => {
                    eprintln!("{}", e);
                    return ActionOutcome::Done(2);
                },
                ExitReason::CustomExitStatus(exit_status) => {
                    return ActionOutcome::Done(exit_status);
                },
                _ => {
                    return ActionOutcome::Reported(Vec::new());
                },
            },
            _ => {},
        }
    }
}

/// The report a creating command printed, with the handle the caller chose in place of the one the
/// pane gave itself.
///
/// The `handle:` line is replaced rather than added, so the report still says one thing about the
/// pane's address. A report with no handle line at all gains one, because the caller asked about
/// the handle and the answer belongs in the report.
fn with_handle(lines: Vec<String>, handle: &str) -> Vec<String> {
    let named = format!("handle: {}", handle);
    let mut replaced = false;
    let mut lines: Vec<String> = lines
        .into_iter()
        .map(|line| {
            if line.starts_with("handle: ") {
                replaced = true;
                named.clone()
            } else {
                line
            }
        })
        .collect();
    if !replaced {
        lines.push(named);
    }
    lines
}

/// The pane a creating command's report says it made.
fn reported_pane(lines: &[String]) -> Option<PaneId> {
    lines
        .iter()
        .find_map(|line| line.strip_prefix("pane_id: "))
        .and_then(|id| PaneId::from_str(id).ok())
}

/// The prefix each raw `subscribe` line carries, and the empty string when it carries none.
fn now_stamp(timestamps: bool) -> String {
    if timestamps {
        format!(
            "{} ",
            zellij_utils::cli::event_timestamp(std::time::SystemTime::now())
        )
    } else {
        String::new()
    }
}

/// The same stamp, as the `ts` key of a json event. A key is added, never renamed or removed, so a
/// reader that does not know about it is unaffected.
fn add_timestamp(event: &mut serde_json::Value, timestamps: bool) {
    if !timestamps {
        return;
    }
    if let Some(object) = event.as_object_mut() {
        object.insert(
            "ts".to_owned(),
            serde_json::Value::String(zellij_utils::cli::event_timestamp(
                std::time::SystemTime::now(),
            )),
        );
    }
}

/// How often a `--for exit` wait asks the session about its pane.
///
/// The render stream says when a pane *closes*, and a command pane that ends is normally *held*
/// open instead - same event to a script, no message at all on that stream. So this one condition
/// is a poll rather than a subscription, and the interval is what a person can tolerate as latency
/// on a build that just finished.
const EXIT_POLL_INTERVAL: Duration = Duration::from_millis(250);

/// What one look at the session says about the pane a `--for exit` wait is watching.
#[derive(Debug, PartialEq, Eq)]
enum PaneState {
    /// Still running: nothing to report yet.
    Running,
    /// The command ended and the pane is still there, holding its status.
    Exited(Option<i32>),
    /// The pane is not in the session any more. Whatever status it had went with it.
    Gone,
}

/// The condition a wait blocks on, as the caller spelled it.
///
/// The pattern arrives as text rather than as a compiled regex so that the caller does not need the
/// regex crate to ask a question about a pane; compiling it is the first thing the wait does, and a
/// pattern that does not compile is an error before anything is subscribed to.
pub enum WaitCondition {
    Exit,
    Quiet(Duration),
    Match(String),
}

/// Reads `list-panes --json` for the one pane the wait is about.
///
/// A pane missing from the list is `Gone` rather than an error: a wait for an exit that ended in
/// the pane closing got what it asked for.
fn pane_state(panes_json: &str, pane: PaneId) -> Result<PaneState, String> {
    let panes: Vec<PaneListEntry> =
        serde_json::from_str(panes_json).map_err(|e| format!("Could not read the panes: {}", e))?;
    let found = panes.iter().find(|entry| {
        let info = &entry.pane_info;
        match pane {
            PaneId::Terminal(id) => !info.is_plugin && info.id == id,
            PaneId::Plugin(id) => info.is_plugin && info.id == id,
        }
    });
    Ok(match found {
        None => PaneState::Gone,
        Some(entry) if entry.pane_info.exited => PaneState::Exited(entry.pane_info.exit_status),
        Some(_) => PaneState::Running,
    })
}

/// The lines of a render update that were not on screen before it.
///
/// Each update carries the whole viewport rather than a delta, so "what is new" has to be worked
/// out here. Membership rather than position is what survives a scroll: a line that moved up the
/// screen is not new, and would otherwise match again on every render.
///
/// The cost of that choice is that a line identical to one already on screen is not new either. A
/// prompt printed twice looks like the same line, because on the rendered screen it is.
fn new_lines<'a>(previous: &[String], current: &'a [String]) -> Vec<&'a str> {
    let seen: HashSet<&str> = previous.iter().map(|line| line.as_str()).collect();
    current
        .iter()
        .map(|line| line.as_str())
        .filter(|line| !seen.contains(line))
        .collect()
}

/// The first of these lines the pattern matches, if any.
fn first_match<'a>(pattern: &Regex, lines: &[&'a str]) -> Option<&'a str> {
    lines.iter().copied().find(|line| pattern.is_match(line))
}

/// How long is left before a deadline, or `None` once it has passed.
fn remaining(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(now))
}

/// The report a met wait prints: how long it took, and what it saw.
fn wait_report(waited: Duration, found: Option<String>) -> Vec<String> {
    let mut lines = vec![format!("waited_ms: {}", waited.as_millis())];
    lines.extend(found);
    lines
}

/// Blocks until the pane meets the condition, and reports what happened.
///
/// `make_os_input` is a factory rather than one connection because `--for exit` asks the session
/// the same question repeatedly, and `ask` opens and closes a connection for each question.
///
/// Returns the process exit status: 0 met, 2 missed - a timeout, or a pane that closed while the
/// wait was for something else - and 1 for a call that could not be carried out at all.
pub fn start_wait_client(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    pane: PaneId,
    condition: WaitCondition,
    timeout: Option<Duration>,
) -> i32 {
    let started = Instant::now();
    let deadline = timeout.map(|timeout| started + timeout);
    let outcome = match condition {
        WaitCondition::Exit => wait_for_exit(make_os_input, session_name, pane, deadline),
        WaitCondition::Quiet(window) => wait_on_renders(
            make_os_input(),
            session_name,
            pane,
            Watching::Quiet(window),
            deadline,
        ),
        WaitCondition::Match(pattern) => match Regex::new(&pattern) {
            Ok(pattern) => wait_on_renders(
                make_os_input(),
                session_name,
                pane,
                Watching::Match(pattern),
                deadline,
            ),
            Err(e) => Err(WaitMiss::Failed(format!("`--match` is not a regex: {}", e))),
        },
    };
    match outcome {
        Ok(found) => {
            for line in wait_report(started.elapsed(), found) {
                println!("{}", line);
            }
            0
        },
        Err(WaitMiss::Missed(message)) => {
            eprintln!("{}", message);
            2
        },
        Err(WaitMiss::Failed(message)) => {
            eprintln!("{}", message);
            1
        },
    }
}

/// `zellij action list-agents`: the panes running a coding agent.
///
/// Answered by the client, from one `list-panes --json`. The pane list already carries the
/// detection on every entry, so this is a filter and a printer - which is what keeps `list-agents`
/// off the client/server contract entirely.
///
/// Returns the process exit status: 0 answered - an empty list is still an answer - and 1 for a
/// call the session could not carry out.
pub fn start_list_agents_client(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    output_json: bool,
) -> i32 {
    let lines = match ask(
        os_input,
        session_name,
        Action::ListPanes {
            show_tab: true,
            show_command: true,
            show_state: false,
            show_geometry: false,
            show_all: false,
            output_json: true,
        },
        "the panes of this session",
    ) {
        Ok(lines) => lines,
        Err(message) => {
            eprintln!("{}", message);
            return 1;
        },
    };
    let panes: Vec<PaneListEntry> = match serde_json::from_str(&lines.join("\n")) {
        Ok(panes) => panes,
        // a session that answered with something other than a pane list is a bug rather than a
        // miss, so it is an error and says what it could not read
        Err(e) => {
            eprintln!(
                "Could not read the pane list this session answered with: {}",
                e
            );
            return 1;
        },
    };
    let agents = agent_detect::agents_from_pane_list(panes);
    if output_json {
        match serde_json::to_string_pretty(&agents) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Could not write the agent list as JSON: {}", e);
                return 1;
            },
        }
    } else {
        for line in agent_detect::agent_table(&agents) {
            println!("{}", line);
        }
    }
    0
}

/// A wait that ended without its condition: the sentence to print, and which exit code it is.
enum WaitMiss {
    Missed(String),
    Failed(String),
}

/// `--for exit`, asked of the session rather than of the render stream.
fn wait_for_exit(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    pane: PaneId,
    deadline: Option<Instant>,
) -> Result<Option<String>, WaitMiss> {
    loop {
        let lines = ask(
            make_os_input(),
            session_name,
            Action::ListPanes {
                show_tab: false,
                show_command: false,
                show_state: false,
                show_geometry: false,
                show_all: true,
                output_json: true,
            },
            &pane.to_string(),
        )
        .map_err(WaitMiss::Failed)?;
        match pane_state(&lines.join("\n"), pane).map_err(WaitMiss::Failed)? {
            PaneState::Running => {},
            // a pane that closed took its status with it, and `-` is how the fork's output says a
            // field has no value rather than a value of nothing
            PaneState::Gone => return Ok(Some("exit_status: -".to_owned())),
            PaneState::Exited(status) => {
                return Ok(Some(match status {
                    Some(status) => format!("exit_status: {}", status),
                    None => "exit_status: -".to_owned(),
                }))
            },
        }
        match remaining(deadline, Instant::now()) {
            Some(left) if left.is_zero() => {
                return Err(WaitMiss::Missed(format!(
                    "{} was still running when the wait timed out.",
                    pane
                )))
            },
            Some(left) => std::thread::sleep(EXIT_POLL_INTERVAL.min(left)),
            None => std::thread::sleep(EXIT_POLL_INTERVAL),
        }
    }
}

/// `--for quiet` and `--for match`, both of which are questions about output as it arrives.
///
/// The render stream is read on a thread so that the deadline is still reachable while nothing is
/// arriving - `recv_from_server` blocks, and a pane that fell silent is exactly the case where it
/// blocks longest.
/// The two conditions the render stream can answer, ready to be tested.
enum Watching {
    Quiet(Duration),
    Match(Regex),
}

fn wait_on_renders(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    pane: PaneId,
    watching: Watching,
    deadline: Option<Instant>,
) -> Result<Option<String>, WaitMiss> {
    let zellij_ipc_pipe = socket_for(session_name).map_err(WaitMiss::Failed)?;
    os_input.connect_to_server(&zellij_ipc_pipe);
    os_input.send_to_server(ClientToServerMsg::SubscribeToPaneRenders {
        pane_ids: vec![pane],
        scrollback: None,
        ansi: false,
    });
    let (sender, updates) = mpsc::channel();
    let reader = os_input.box_clone();
    std::thread::spawn(move || {
        while let Some((message, _)) = reader.recv_from_server() {
            if sender.send(message).is_err() {
                break;
            }
        }
    });

    // the viewport as it stood at the last update: the baseline new lines are measured against
    let mut on_screen: Vec<String> = Vec::new();
    // a pane that has said nothing yet is not quiet - it has not been watched long enough to know
    let mut last_output = Instant::now();
    let answer = loop {
        let quiet_deadline = match watching {
            Watching::Quiet(window) => Some(last_output + window),
            _ => None,
        };
        let next = match (deadline, quiet_deadline) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let waited = match next {
            Some(next) => updates.recv_timeout(next.saturating_duration_since(Instant::now())),
            None => updates.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match waited {
            Ok(ServerToClientMsg::PaneRenderUpdate {
                viewport,
                is_initial,
                ..
            }) => {
                if !is_initial {
                    if let Watching::Match(ref pattern) = watching {
                        if let Some(line) = first_match(pattern, &new_lines(&on_screen, &viewport))
                        {
                            break Ok(Some(format!("matched: {}", line)));
                        }
                    }
                }
                last_output = Instant::now();
                on_screen = viewport;
            },
            Ok(ServerToClientMsg::SubscribedPaneClosed { .. }) => {
                break Err(WaitMiss::Missed(format!(
                    "{} closed before the wait was satisfied.",
                    pane
                )))
            },
            Ok(ServerToClientMsg::LogError { lines }) => {
                break Err(WaitMiss::Failed(lines.join("\n")))
            },
            Ok(ServerToClientMsg::Exit { .. }) => {
                break Err(WaitMiss::Missed(
                    "The session exited before the wait was satisfied.".to_owned(),
                ))
            },
            Ok(_) => {},
            Err(RecvTimeoutError::Disconnected) => {
                break Err(WaitMiss::Failed(
                    "The session stopped answering while the wait was running.".to_owned(),
                ))
            },
            // the only timer that can be the one that fired, when the overall deadline has not
            // arrived, is the quiet window
            Err(RecvTimeoutError::Timeout) => {
                let timed_out = remaining(deadline, Instant::now()) == Some(Duration::ZERO);
                if timed_out {
                    break Err(WaitMiss::Missed(format!("The wait on {} timed out.", pane)));
                }
                break Ok(None);
            },
        }
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    answer
}

/// The session's socket, which every client here opens the same way.
fn socket_for(session_name: &str) -> Result<PathBuf, String> {
    let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
    fs::create_dir_all(&sock_dir).map_err(|e| e.to_string())?;
    zellij_utils::shared::set_permissions(&sock_dir, 0o700).map_err(|e| e.to_string())?;
    sock_dir.push(session_name);
    crate::check_ipc_pipe_length(&sock_dir);
    Ok(sock_dir)
}

pub fn start_subscribe_client(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    subscribe_cli: SubscribeCli,
) {
    let zellij_ipc_pipe: PathBuf = {
        let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        fs::create_dir_all(&sock_dir).unwrap();
        zellij_utils::shared::set_permissions(&sock_dir, 0o700).unwrap();
        sock_dir.push(session_name);
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    os_input.connect_to_server(&*zellij_ipc_pipe);

    // Parse pane IDs
    let pane_ids: Vec<PaneId> = subscribe_cli
        .pane_id
        .iter()
        .map(|s| {
            PaneId::from_str(s).unwrap_or_else(|e| {
                eprintln!("Invalid pane ID '{}': {}", s, e);
                process::exit(2);
            })
        })
        .collect();

    // Send subscribe message
    os_input.send_to_server(ClientToServerMsg::SubscribeToPaneRenders {
        pane_ids: pane_ids.clone(),
        scrollback: subscribe_cli.scrollback,
        ansi: subscribe_cli.ansi,
    });

    // Track remaining panes for exit-on-all-closed
    let mut remaining_panes: HashSet<PaneId> = pane_ids.into_iter().collect();

    // Streaming receive loop
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    loop {
        match os_input.recv_from_server() {
            Some((
                ServerToClientMsg::PaneRenderUpdate {
                    pane_id,
                    viewport,
                    scrollback,
                    is_initial,
                },
                _,
            )) => match subscribe_cli.format {
                SubscribeFormat::Raw => {
                    // one stamp for the whole update: these lines are printed in one go, and a
                    // stamp that crept forward between them would be describing something else
                    let stamp = now_stamp(subscribe_cli.timestamps);
                    if let Some(ref scrollback_lines) = scrollback {
                        for line in scrollback_lines {
                            let _ = writeln!(stdout, "{}{}", stamp, line);
                        }
                    }
                    for line in &viewport {
                        let _ = writeln!(stdout, "{}{}", stamp, line);
                    }
                    let _ = stdout.flush();
                },
                SubscribeFormat::Json => {
                    let mut json = serde_json::json!({
                        "event": "pane_update",
                        "pane_id": pane_id.to_string(),
                        "viewport": viewport,
                        "scrollback": scrollback,
                        "is_initial": is_initial,
                    });
                    add_timestamp(&mut json, subscribe_cli.timestamps);
                    let _ = writeln!(stdout, "{}", json);
                    let _ = stdout.flush();
                },
            },
            Some((ServerToClientMsg::SubscribedPaneClosed { pane_id }, _)) => {
                remaining_panes.remove(&pane_id);
                match subscribe_cli.format {
                    SubscribeFormat::Raw => {},
                    SubscribeFormat::Json => {
                        let mut json = serde_json::json!({
                            "event": "pane_closed",
                            "pane_id": pane_id.to_string(),
                        });
                        add_timestamp(&mut json, subscribe_cli.timestamps);
                        let _ = writeln!(stdout, "{}", json);
                        let _ = stdout.flush();
                    },
                }
                if remaining_panes.is_empty() {
                    break;
                }
            },
            Some((ServerToClientMsg::Exit { .. }, _)) => break,
            Some((ServerToClientMsg::LogError { lines }, _)) => {
                for line in lines {
                    eprintln!("{}", line);
                }
                process::exit(2);
            },
            None => break,
            _ => {},
        }
    }

    os_input.send_to_server(ClientToServerMsg::ClientExited);
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::{NewPanePlacement, UnblockCondition};

    fn blocking_pane(unblock_condition: Option<UnblockCondition>) -> Action {
        Action::NewBlockingPane {
            placement: NewPanePlacement::default(),
            pane_name: None,
            command: None,
            unblock_condition,
            near_current_pane: false,
            no_focus: false,
            tab_id: None,
        }
    }

    #[test]
    fn a_chosen_handle_replaces_the_one_the_pane_gave_itself() {
        let reported = vec![
            "pane_id: terminal_9".to_owned(),
            "handle: sunny-otter".to_owned(),
        ];
        assert_eq!(
            with_handle(reported, "build"),
            vec!["pane_id: terminal_9".to_owned(), "handle: build".to_owned()],
            "the report says one thing about the pane's address"
        );
        // a report that never had a handle line gains one: the caller asked about the handle
        assert_eq!(
            with_handle(vec!["tab_id: 4".to_owned()], "build"),
            vec!["tab_id: 4".to_owned(), "handle: build".to_owned()]
        );
    }

    #[test]
    fn a_verb_that_makes_a_pane_owes_the_caller_one() {
        // the pair this rests on: these verbs answer with a pane, so a report without one is a
        // miss and gets a non-zero exit instead of a printed id that reaches nothing
        for action in [
            Action::NewTiledPane {
                direction: None,
                command: None,
                pane_name: None,
                near_current_pane: false,
                no_focus: false,
                tab_id: None,
                borderless: None,
            },
            Action::Run {
                command: Default::default(),
                near_current_pane: false,
                no_focus: false,
            },
        ] {
            assert!(
                action_reports_a_new_pane(&action),
                "{:?} makes a pane and was not held to reporting one",
                action
            );
        }
        // the negative control, and the reason the rule is a list rather than "anything that
        // creates something": a blocking pane is answered by its command's exit status and prints
        // no `pane_id:` line even when it worked
        assert!(
            !action_reports_a_new_pane(&blocking_pane(None)),
            "a blocking pane was held to a report it never prints"
        );
        assert!(
            !action_reports_a_new_pane(&Action::ClosePluginPane { pane_id: 3 }),
            "a verb that makes no pane was held to reporting one"
        );
    }

    #[test]
    fn the_pane_a_report_names_is_the_one_that_gets_the_handle() {
        let reported = vec![
            "tab_id: 4".to_owned(),
            "pane_id: plugin_2".to_owned(),
            "handle: sunny-otter".to_owned(),
        ];
        assert_eq!(reported_pane(&reported), Some(PaneId::Plugin(2)));
        // the negative control: a report with no pane in it names none
        assert_eq!(reported_pane(&["tab_id: 4".to_owned()]), None);
    }

    #[test]
    fn a_stamped_line_carries_the_time_and_a_bare_one_carries_nothing() {
        let stamp = now_stamp(true);
        assert!(
            stamp.ends_with(' '),
            "the prefix separates itself: {stamp:?}"
        );
        assert!(stamp.trim_end().ends_with('Z'), "utc: {stamp:?}");
        assert_eq!(stamp.trim_end().len(), "2026-08-14T18:03:12.345Z".len());
        // the negative control: without the flag the line is exactly what it was before
        assert_eq!(now_stamp(false), "");
    }

    #[test]
    fn a_json_event_gains_a_ts_key_only_when_it_was_asked_for() {
        let mut event = serde_json::json!({"event": "pane_closed"});
        add_timestamp(&mut event, false);
        assert!(event.get("ts").is_none());
        add_timestamp(&mut event, true);
        assert!(event["ts"].as_str().unwrap().ends_with('Z'), "{event}");
        // the rest of the object is untouched
        assert_eq!(event["event"], "pane_closed");
    }

    #[test]
    fn a_bare_blocking_pane_still_waits_for_its_own_report() {
        // `-b/--blocking` sets no unblock condition: it waits for the pane to close rather than for
        // a status to match. Reading an `UnblockInputThread` as its answer would print nothing and
        // exit 0 while the command is still running
        assert!(action_answers_with_its_own_report(&blocking_pane(None)));
    }

    #[test]
    fn a_conditional_blocking_pane_waits_for_its_own_report() {
        assert!(action_answers_with_its_own_report(&blocking_pane(Some(
            UnblockCondition::OnAnyExit
        ))));
    }

    /// A `list-panes --json` answer, built from the structs the server serializes rather than
    /// written out by hand: a field added to `PaneInfo` must not break a test about exit status.
    fn panes_json(entries: &[(u32, bool, bool, Option<i32>)]) -> String {
        let entries: Vec<PaneListEntry> = entries
            .iter()
            .map(|(id, is_plugin, exited, exit_status)| PaneListEntry {
                pane_info: zellij_utils::data::PaneInfo {
                    id: *id,
                    is_plugin: *is_plugin,
                    exited: *exited,
                    exit_status: *exit_status,
                    ..Default::default()
                },
                tab_id: 0,
                tab_position: 0,
                tab_name: "tab".to_owned(),
                agent: None,
            })
            .collect();
        serde_json::to_string(&entries).unwrap()
    }

    #[test]
    fn a_wait_for_exit_reads_the_pane_it_was_asked_about() {
        let panes = panes_json(&[(1, false, false, None), (2, false, true, Some(7))]);
        assert_eq!(
            pane_state(&panes, PaneId::Terminal(1)).unwrap(),
            PaneState::Running
        );
        assert_eq!(
            pane_state(&panes, PaneId::Terminal(2)).unwrap(),
            PaneState::Exited(Some(7))
        );
        // a pane that is not in the list is gone, which is an exit the wait was asking about and
        // not an error
        assert_eq!(
            pane_state(&panes, PaneId::Terminal(9)).unwrap(),
            PaneState::Gone
        );
        // the id spaces do not run together: plugin_1 is not terminal_1
        assert_eq!(
            pane_state(&panes, PaneId::Plugin(1)).unwrap(),
            PaneState::Gone
        );
    }

    #[test]
    fn a_pane_that_exited_without_a_status_says_so_rather_than_inventing_one() {
        let panes = panes_json(&[(3, false, true, None)]);
        assert_eq!(
            pane_state(&panes, PaneId::Terminal(3)).unwrap(),
            PaneState::Exited(None)
        );
        // the negative control: an answer that is not a pane list is an error, not "gone"
        assert!(pane_state("not json", PaneId::Terminal(3)).is_err());
    }

    #[test]
    fn only_a_line_that_was_not_on_screen_is_new() {
        let before = vec!["building".to_owned(), "linking".to_owned()];
        let after = vec!["linking".to_owned(), "done".to_owned()];
        assert_eq!(new_lines(&before, &after), vec!["done"]);
        // a viewport that scrolled carries the same lines at different rows, and none of them is
        // new - position would have said all of them were
        let scrolled = vec!["linking".to_owned(), "building".to_owned()];
        assert!(new_lines(&before, &scrolled).is_empty());
        // the negative control: nothing changed, nothing is new
        assert!(new_lines(&before, &before).is_empty());
    }

    #[test]
    fn a_match_is_tested_against_one_delivered_line_at_a_time() {
        let pattern = Regex::new("test result:").unwrap();
        assert_eq!(
            first_match(&pattern, &["ok", "test result: ok. 12 passed"]),
            Some("test result: ok. 12 passed")
        );
        // the negative control, and the limitation worth knowing: the terminal wrapped this line,
        // so the two halves arrive as two lines and a pattern spanning the wrap matches neither
        assert_eq!(first_match(&pattern, &["test resu", "lt: ok"]), None);
    }

    #[test]
    fn a_wait_reports_how_long_it_waited_and_what_it_saw() {
        assert_eq!(
            wait_report(
                Duration::from_millis(1500),
                Some("exit_status: 0".to_owned())
            ),
            vec!["waited_ms: 1500".to_owned(), "exit_status: 0".to_owned()]
        );
        // `--for quiet` has nothing to show but the wait itself
        assert_eq!(
            wait_report(Duration::from_millis(80), None),
            vec!["waited_ms: 80".to_owned()]
        );
    }

    #[test]
    fn a_wait_with_no_timeout_never_runs_out_of_time() {
        let now = Instant::now();
        // `--timeout 0` carries no deadline, and no amount of elapsed time turns into one
        assert_eq!(remaining(None, now), None);
        assert_eq!(
            remaining(Some(now - Duration::from_secs(1)), now),
            Some(Duration::ZERO),
            "a deadline that has passed has nothing left, rather than wrapping"
        );
        assert_eq!(
            remaining(Some(now + Duration::from_secs(5)), now),
            Some(Duration::from_secs(5))
        );
    }

    #[test]
    fn an_ordinary_action_is_answered_by_the_unblock() {
        // the negative control: everything that is not a blocking pane is done when the server says
        // the input is free again, and must not be left waiting for a report that never comes
        assert!(!action_answers_with_its_own_report(
            &Action::ToggleFloatingPanes
        ));
    }
}
