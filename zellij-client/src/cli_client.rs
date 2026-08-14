//! The `[cli_client]` is used to attach to a running server session
//! and dispatch actions, that are specified through the command line.
use std::collections::{BTreeMap, HashSet};
use std::io::{self, BufRead, Write};
use std::process;
use std::str::FromStr;
use std::{fs, path::PathBuf};

use crate::os_input_output::ClientOsApi;
use uuid::Uuid;
use zellij_utils::{
    cli::{SubscribeCli, SubscribeFormat},
    data::PaneId,
    errors::prelude::*,
    input::actions::Action,
    ipc::{ClientToServerMsg, ExitReason, ServerToClientMsg},
};

pub fn start_cli_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    actions: Vec<Action>,
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
    let pane_id = os_input
        .env_variable("ZELLIJ_PANE_ID")
        .and_then(|e| e.trim().parse().ok());

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
                if let Some(exit_status) =
                    individual_messages_client(&mut os_input, action, pane_id)
                {
                    return exit_status;
                }
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

fn individual_messages_client(
    os_input: &mut Box<dyn ClientOsApi>,
    action: Action,
    pane_id: Option<u32>,
) -> Option<i32> {
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
                return None;
            },
            Some((ServerToClientMsg::Log { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| println!("{line}"));
                return None;
            },
            Some((ServerToClientMsg::LogError { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| eprintln!("{line}"));
                return Some(2);
            },
            Some((ServerToClientMsg::Exit { exit_reason }, _)) => match exit_reason {
                ExitReason::Error(e) => {
                    eprintln!("{}", e);
                    return Some(2);
                },
                ExitReason::CustomExitStatus(exit_status) => {
                    return Some(exit_status);
                },
                _ => {
                    return None;
                },
            },
            _ => {},
        }
    }
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

    #[test]
    fn an_ordinary_action_is_answered_by_the_unblock() {
        // the negative control: everything that is not a blocking pane is done when the server says
        // the input is free again, and must not be left waiting for a report that never comes
        assert!(!action_answers_with_its_own_report(
            &Action::ToggleFloatingPanes
        ));
    }
}
