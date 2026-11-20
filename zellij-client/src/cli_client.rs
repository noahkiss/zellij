//! The `[cli_client]` is used to attach to a running server session
//! and dispatch actions, that are specified through the command line.
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{self, BufRead, Read, Write};
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
    cli::{PromptCli, PromptElementCli, SubscribeCli, SubscribeFormat},
    data::{
        ListAgentsResponse, OutsideClick, PaneCommandState, PaneDump, PaneId, PaneListEntry,
        PaneProgramStatus, PaneTarget, PipePopupPlacement,
    },
    errors::prelude::*,
    input::actions::Action,
    ipc::{ClientToServerMsg, ExitReason, IpcReceiveError, ServerToClientMsg},
    prompt::{self, ChoiceItem, PromptElement},
};

/// `--wait-ready` on a verb that makes a pane: how long to wait for the pane's shell, and how to
/// open the connections the wait asks the session on.
pub struct WaitReady<'a> {
    pub timeout: Duration,
    pub make_os_input: &'a dyn Fn() -> Box<dyn ClientOsApi>,
}

/// The quiet window `--wait-ready` falls back on, for a shell that writes no OSC 133 prompt
/// marker. The same default `wait --for ready` takes.
const READY_QUIET: Duration = Duration::from_millis(300);

/// Runs the actions one CLI invocation turned into.
///
/// `anchor_pane` is the pane these actions should be read as coming from - what `--near` names. A
/// `zellij action` client is not attached to anything, so "the pane this came from" is normally the
/// ambient `$ZELLIJ_PANE_ID`, and this is the same answer given deliberately instead of inherited.
/// It is a terminal id because that is what the message carries; the refusal for anything else
/// happens before this, where there is a caller to tell.
///
/// `pipe_timeout` is what `pipe --timeout` asked for, and `None` is the pipe that waits forever.
///
/// `output_json` is `action --json`: the report is printed as one object instead of `key: value`
/// lines. It reaches only the printing - what the session is asked, and what it answers, are the
/// same either way.
///
/// `wait_ready` is `--wait-ready`: once the report names the pane, wait for its shell as `wait
/// --for ready` does, and add `ready_by:` and `ready_ms:` to the report. A pane that is not ready
/// in time is still reported - the caller made it and has to be able to close it - and the exit is
/// 2.
pub fn start_cli_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    actions: Vec<Action>,
    anchor_pane: Option<u32>,
    pipe_timeout: Option<Duration>,
    output_json: bool,
    wait_ready: Option<WaitReady>,
) -> i32 {
    let zellij_ipc_pipe: PathBuf = {
        let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        fs::create_dir_all(&sock_dir).unwrap();
        zellij_utils::shared::set_permissions(&sock_dir, 0o700).unwrap();
        sock_dir.push(zellij_utils::sessions::session_id_for(session_name));
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    if let Err(e) = os_input.connect_to_server(&*zellij_ipc_pipe) {
        eprintln!("{}", e);
        return 1;
    }
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
                popup,
                popup_no_focus,
                popup_capture_all_keys,
                popup_outside_click,
            } => {
                let exit_status = pipe_client(
                    &mut os_input,
                    PipeRequest {
                        pipe_id,
                        name,
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
                        popup,
                        popup_no_focus,
                        popup_capture_all_keys,
                        popup_outside_click,
                    },
                    payload,
                    PipeWait::cli(pipe_timeout),
                );
                if exit_status != 0 {
                    os_input.send_to_server(ClientToServerMsg::ClientExited);
                    return exit_status;
                }
            },
            action => {
                let mark_panes = matches!(
                    &action,
                    Action::ListPanes {
                        output_json: true,
                        ..
                    }
                );
                let promised_a_pane = action_reports_a_new_pane(&action);
                let lines = match individual_messages_client(&mut os_input, action, pane_id) {
                    ActionOutcome::Done(exit_status) => return exit_status,
                    ActionOutcome::Reported(lines) => lines,
                };
                let lines = if mark_panes {
                    pane_list_with_self(
                        lines,
                        os_input
                            .env_variable("ZELLIJ_PANE_ID")
                            .and_then(|id| id.parse().ok()),
                        os_input.env_variable("ZELLIJ_SESSION_NAME").as_deref()
                            == Some(session_name),
                    )
                } else {
                    lines
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
                let Some(wait) = &wait_ready else {
                    print_report(&lines, output_json);
                    continue;
                };
                let Some(made) = reported_pane(&lines) else {
                    print_report(&lines, output_json);
                    eprintln!("The session reported no pane, so there was none to wait for.");
                    return 2;
                };
                let started = Instant::now();
                match wait_for_ready(
                    wait.make_os_input,
                    session_name,
                    made,
                    READY_QUIET,
                    Some(started + wait.timeout),
                ) {
                    Ok(ready_by) => {
                        print_report(&with_ready(lines, ready_by, started.elapsed()), output_json)
                    },
                    Err(miss) => {
                        // the pane exists whether or not it is ready, so it is reported either way
                        print_report(&lines, output_json);
                        let (message, exit_status) = match miss {
                            WaitMiss::Missed(message) => (message, 2),
                            WaitMiss::Failed(message) => (message, 1),
                        };
                        eprintln!("{}", message);
                        os_input.send_to_server(ClientToServerMsg::ClientExited);
                        return exit_status;
                    },
                }
            },
        }
    }
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    0
}

struct PipeRequest {
    pipe_id: String,
    name: Option<String>,
    plugin: Option<String>,
    args: Option<BTreeMap<String, String>>,
    configuration: Option<BTreeMap<String, String>>,
    launch_new: bool,
    skip_cache: bool,
    floating: Option<bool>,
    in_place: Option<bool>,
    pane_id: Option<u32>,
    cwd: Option<PathBuf>,
    pane_title: Option<String>,
    popup: Option<PipePopupPlacement>,
    popup_no_focus: bool,
    popup_capture_all_keys: bool,
    popup_outside_click: Option<OutsideClick>,
}

impl PipeRequest {
    fn prepare(mut self) -> Self {
        self.name = self
            .name
            .take()
            .or_else(|| self.plugin.clone())
            .or_else(|| Some(Uuid::new_v4().to_string()));
        if self.launch_new {
            self.configuration
                .get_or_insert_with(BTreeMap::new)
                .insert("_zellij_id".to_owned(), Uuid::new_v4().to_string());
        }
        self
    }
    fn message(&self, payload: Option<String>) -> ClientToServerMsg {
        ClientToServerMsg::Action {
            action: Action::CliPipe {
                pipe_id: self.pipe_id.clone(),
                name: self.name.clone(),
                payload,
                args: self.args.clone(),
                plugin: self.plugin.clone(),
                configuration: self.configuration.clone(),
                floating: self.floating,
                in_place: self.in_place,
                launch_new: self.launch_new,
                skip_cache: self.skip_cache,
                cwd: self.cwd.clone(),
                pane_title: self.pane_title.clone(),
                popup: self.popup,
                popup_no_focus: self.popup_no_focus,
                popup_capture_all_keys: self.popup_capture_all_keys,
                popup_outside_click: self.popup_outside_click,
            },
            terminal_id: self.pane_id,
            client_id: None,
            is_cli_client: true,
        }
    }
}

impl PipeRequest {
    /// The pipe as the caller named it: `--name` is what they typed, and the id is a uuid that
    /// means nothing to them.
    fn display_name(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.pipe_id)
    }
}

/// How a pipe waits for the session to answer it.
///
/// Only a plugin that received the message can unblock a pipe, so a session with none listening
/// never answers. `idle_timeout` is `pipe --timeout`: an IDLE window rather than a budget for the
/// call. It measures how long the session has said nothing, and anything it says resets it. That
/// is what makes it safe for a streaming pipe - `tail -f | zellij pipe` spends its time reading
/// stdin, which is outside the window entirely, and each answer the plugin sends starts it again.
/// Nothing bounds that read, which is why only a pipe that IS its stdin reaches it - see
/// [`pipe_reads_stdin`].
struct PipeWait {
    idle_timeout: Option<Duration>,
    /// `zellij pipe` says so when the session goes away mid-wait, and exits 1; `zellij prompt`
    /// keeps its own exit codes and exits 2.
    explain_session_loss: bool,
}

impl PipeWait {
    fn cli(idle_timeout: Option<Duration>) -> Self {
        PipeWait {
            idle_timeout,
            explain_session_loss: true,
        }
    }
    fn prompt() -> Self {
        PipeWait {
            idle_timeout: None,
            explain_session_loss: false,
        }
    }
    fn session_gone(&self) -> i32 {
        if self.explain_session_loss {
            eprintln!("The session stopped answering while the pipe was waiting.");
            1
        } else {
            2
        }
    }
    /// The pipe went unanswered for the whole window. Whatever the plugins sent has already been
    /// written to stdout, so the sentence goes to stderr and the status says the pipe was not
    /// answered rather than that it failed.
    fn timed_out(&self, request: &PipeRequest, window: Duration) -> i32 {
        eprintln!("{}", pipe_timed_out(request.display_name(), window));
        2
    }
}

enum PipeWaitOutcome {
    Released(Option<i32>),
    Exit(i32),
}

fn handle_pipe_server_message(
    os_input: &mut Box<dyn ClientOsApi>,
    pipe_id: &str,
    message: ServerToClientMsg,
) -> Option<PipeWaitOutcome> {
    match message {
        ServerToClientMsg::UnblockCliPipeInput {
            pipe_name,
            exit_code,
        } => {
            if pipe_name == pipe_id {
                Some(PipeWaitOutcome::Released(exit_code))
            } else {
                None
            }
        },
        ServerToClientMsg::CliPipeOutput { pipe_name, output } => {
            let err_context = "Failed to write to stdout";
            if pipe_name == pipe_id {
                let mut stdout = os_input.get_stdout_writer();
                stdout
                    .write_all(output.as_bytes())
                    .context(err_context)
                    .non_fatal();
                stdout.flush().context(err_context).non_fatal();
            }
            None
        },
        ServerToClientMsg::Log { lines: log_lines } => {
            log_lines.iter().for_each(|line| println!("{line}"));
            Some(PipeWaitOutcome::Exit(0))
        },
        ServerToClientMsg::LogError { lines: log_lines } => {
            log_lines.iter().for_each(|line| eprintln!("{line}"));
            Some(PipeWaitOutcome::Exit(2))
        },
        ServerToClientMsg::Exit { exit_reason } => match exit_reason {
            ExitReason::Error(e) => {
                eprintln!("{}", e);
                Some(PipeWaitOutcome::Exit(2))
            },
            ExitReason::CustomExitStatus(exit_status) => Some(PipeWaitOutcome::Exit(exit_status)),
            _ => Some(PipeWaitOutcome::Exit(0)),
        },
        _ => None,
    }
}

/// The session's messages for a pipe that waits under `--timeout`, read on a thread because
/// `recv_from_server` blocks, and a session with nothing to say is exactly the case where it
/// blocks longest. `None` is the pipe that waits forever, on the blocking read.
fn pipe_inbox(
    os_input: &Box<dyn ClientOsApi>,
    wait: &PipeWait,
) -> Option<mpsc::Receiver<ServerToClientMsg>> {
    wait.idle_timeout.map(|_| {
        let (sender, incoming) = mpsc::channel();
        let reader = os_input.box_clone();
        std::thread::spawn(move || {
            while let Some((message, _)) = reader.recv_from_server() {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        incoming
    })
}

fn wait_for_pipe_release(
    os_input: &mut Box<dyn ClientOsApi>,
    request: &PipeRequest,
    inbox: Option<&mpsc::Receiver<ServerToClientMsg>>,
    wait: &PipeWait,
) -> PipeWaitOutcome {
    // a message just went out, so the wait for an answer starts now
    let mut last_heard = Instant::now();
    loop {
        let message = match (inbox, wait.idle_timeout) {
            (Some(incoming), Some(window)) => {
                match incoming
                    .recv_timeout((last_heard + window).saturating_duration_since(Instant::now()))
                {
                    Ok(message) => {
                        last_heard = Instant::now();
                        message
                    },
                    Err(RecvTimeoutError::Timeout) => {
                        return PipeWaitOutcome::Exit(wait.timed_out(request, window))
                    },
                    Err(RecvTimeoutError::Disconnected) => {
                        return PipeWaitOutcome::Exit(wait.session_gone())
                    },
                }
            },
            _ => match os_input.recv_from_server() {
                Some((message, _)) => message,
                None => return PipeWaitOutcome::Exit(wait.session_gone()),
            },
        };
        if let Some(outcome) = handle_pipe_server_message(os_input, &request.pipe_id, message) {
            return outcome;
        }
    }
}

enum PipeEvent {
    Server(ServerToClientMsg),
    ServerGone,
    Payload(Option<String>),
}

const STREAM_BATCH_BYTES: usize = 256 * 1024;

pub(crate) fn next_payload(
    queued: &mut VecDeque<Option<String>>,
    batch: bool,
) -> Option<Option<String>> {
    let first = queued.pop_front()?;
    let Some(mut payload) = first else {
        return Some(None);
    };
    if batch {
        while let Some(Some(next)) = queued.front() {
            if payload.len() + next.len() > STREAM_BATCH_BYTES {
                break;
            }
            payload.push_str(next);
            queued.pop_front();
        }
    }
    Some(Some(payload))
}

fn drive_streaming_pipe(
    os_input: &mut Box<dyn ClientOsApi>,
    request: &PipeRequest,
    first_payload: Option<String>,
    separator: u8,
    batch: bool,
    wait: &PipeWait,
) -> i32 {
    let (sender, receiver) = std::sync::mpsc::channel();
    {
        let sender = sender.clone();
        let stdin_os_input = os_input.box_clone();
        std::thread::spawn(move || {
            if let Some(payload) = first_payload {
                if sender.send(PipeEvent::Payload(Some(payload))).is_err() {
                    return;
                }
            }
            let payloads = StdinPayloads {
                reader: stdin_os_input.get_stdin_reader(),
                separator,
                ended: false,
            };
            for payload in payloads {
                if sender.send(PipeEvent::Payload(payload)).is_err() {
                    return;
                }
            }
        });
    }
    {
        let server_os_input = os_input.box_clone();
        std::thread::spawn(move || loop {
            match server_os_input.recv_from_server() {
                Some((message, _)) => {
                    if sender.send(PipeEvent::Server(message)).is_err() {
                        return;
                    }
                },
                None => {
                    let _ = sender.send(PipeEvent::ServerGone);
                    return;
                },
            }
        });
    }
    let mut queued: VecDeque<Option<String>> = VecDeque::new();
    let mut waiting_for_release = false;
    let mut input_ended = false;
    // when the session was last heard from, or last asked: what the idle window is measured from
    let mut last_heard = Instant::now();
    loop {
        // the idle window runs only while an answer is owed: the wait for stdin is outside it
        let event = match (waiting_for_release, wait.idle_timeout) {
            (true, Some(window)) => {
                match receiver
                    .recv_timeout((last_heard + window).saturating_duration_since(Instant::now()))
                {
                    Ok(event) => Ok(event),
                    Err(RecvTimeoutError::Timeout) => return wait.timed_out(request, window),
                    Err(RecvTimeoutError::Disconnected) => Err(()),
                }
            },
            _ => receiver.recv().map_err(|_| ()),
        };
        match event {
            Ok(PipeEvent::Payload(payload)) => {
                if payload.is_none() {
                    input_ended = true;
                }
                queued.push_back(payload);
            },
            Ok(PipeEvent::Server(message)) => {
                last_heard = Instant::now();
                match handle_pipe_server_message(os_input, &request.pipe_id, message) {
                    Some(PipeWaitOutcome::Released(Some(exit_code))) => return exit_code,
                    Some(PipeWaitOutcome::Released(None)) => {
                        waiting_for_release = false;
                        if input_ended && queued.is_empty() {
                            return 0;
                        }
                    },
                    Some(PipeWaitOutcome::Exit(exit_code)) => return exit_code,
                    None => {},
                }
            },
            Ok(PipeEvent::ServerGone) | Err(()) => return wait.session_gone(),
        }
        if !waiting_for_release {
            if let Some(payload) = next_payload(&mut queued, batch) {
                os_input.send_to_server(request.message(payload));
                waiting_for_release = true;
                last_heard = Instant::now();
            }
        }
    }
}

fn drive_pipe(
    os_input: &mut Box<dyn ClientOsApi>,
    request: &PipeRequest,
    mut payloads: impl Iterator<Item = Option<String>>,
    wait: &PipeWait,
) -> i32 {
    let inbox = pipe_inbox(os_input, wait);
    let mut next_payload = payloads.next();
    while let Some(payload) = next_payload {
        os_input.send_to_server(request.message(payload));
        match wait_for_pipe_release(os_input, request, inbox.as_ref(), wait) {
            PipeWaitOutcome::Released(Some(exit_code)) => return exit_code,
            PipeWaitOutcome::Released(None) => {},
            PipeWaitOutcome::Exit(exit_code) => return exit_code,
        }
        next_payload = payloads.next();
    }
    0
}

struct StdinPayloads {
    reader: Box<dyn BufRead>,
    separator: u8,
    ended: bool,
}

impl Iterator for StdinPayloads {
    type Item = Option<String>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let mut buffer = vec![];
        let _ = self.reader.read_until(self.separator, &mut buffer);
        if buffer.is_empty() {
            self.ended = true;
            return Some(None);
        }
        Some(Some(String::from_utf8_lossy(&buffer).into_owned()))
    }
}

fn pipe_client(
    os_input: &mut Box<dyn ClientOsApi>,
    request: PipeRequest,
    payload: Option<String>,
    wait: PipeWait,
) -> i32 {
    let request = request.prepare();
    let is_piped = !os_input.stdin_is_terminal();
    // whether the pipe's own words came from the invocation
    let payload_from_command_line = payload.is_some();
    if !pipe_reads_stdin(is_piped, payload_from_command_line) {
        let exit_status = drive_pipe(os_input, &request, std::iter::once(payload), &wait);
        if exit_status == 0 && is_piped {
            // the pipe said everything it had to say, so this end-of-pipe message is the last one
            // there is: the same place `zellij pipe -- 'payload' < /dev/null` has always ended,
            // instead of falling through to a stdin the pipe never owned
            os_input.send_to_server(request.message(None));
        }
        return exit_status;
    }
    drive_streaming_pipe(os_input, &request, payload, b'\n', false, &wait)
}

pub fn start_prompt_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    prompt_cli: PromptCli,
    plugin_url: String,
) -> i32 {
    let is_piped = !os_input.stdin_is_terminal();
    let plan = match plan_prompt(
        &prompt_cli,
        || {
            let mut text = String::new();
            os_input
                .get_stdin_reader()
                .read_to_string(&mut text)
                .map(|_| text)
                .map_err(|e| format!("failed to read stdin: {}", e))
        },
        is_piped,
    ) {
        Ok(plan) => plan,
        Err(e) => {
            eprint!(
                "{}",
                prompt_error_message(
                    &e,
                    prompt_cli.element.name(),
                    std::io::IsTerminal::is_terminal(&std::io::stderr()),
                )
            );
            return prompt::EXIT_ERROR;
        },
    };
    let pane_id: Option<u32> = os_input
        .env_variable("ZELLIJ_PANE_ID")
        .and_then(|e| e.trim().parse().ok());
    let Some(pane_id) = pane_id else {
        eprintln!("zellij prompt must be run inside a Zellij pane (ZELLIJ_PANE_ID is not set)");
        return prompt::EXIT_ERROR;
    };
    let zellij_ipc_pipe: PathBuf = {
        let sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        fs::create_dir_all(&sock_dir).unwrap();
        zellij_utils::shared::set_permissions(&sock_dir, 0o700).unwrap();
        zellij_utils::sessions::resolve_session_socket_path(session_name)
            .unwrap_or_else(|| sock_dir.join(session_name))
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    if let Err(e) = os_input.connect_to_server(&*zellij_ipc_pipe) {
        eprintln!("{}", crate::connect_error_message(e));
        process::exit(1);
    }
    let request = PipeRequest {
        pipe_id: Uuid::new_v4().to_string(),
        name: Some(plan.element.name().to_owned()),
        plugin: Some(plugin_url),
        args: Some(plan.args),
        configuration: None,
        launch_new: true,
        skip_cache: false,
        floating: None,
        in_place: None,
        pane_id: Some(pane_id),
        cwd: None,
        pane_title: Some(format!("prompt: {}", plan.element.name())),
        popup: Some(prompt_cli.placement()),
        popup_no_focus: !prompt_cli.takes_focus(),
        popup_capture_all_keys: prompt_cli.answer_args().capture_all_keys,
        popup_outside_click: prompt_cli.answer_args().outside_click,
    }
    .prepare();
    let exit_status = match plan.input {
        PromptInput::Single(payload) => drive_pipe(
            &mut os_input,
            &request,
            std::iter::once(payload),
            &PipeWait::prompt(),
        ),
        PromptInput::StreamStdin { null } => drive_streaming_pipe(
            &mut os_input,
            &request,
            None,
            if null { 0 } else { b'\n' },
            true,
            &PipeWait::prompt(),
        ),
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    exit_status
}

#[derive(Debug, Clone, PartialEq)]
pub enum PromptInput {
    Single(Option<String>),
    StreamStdin { null: bool },
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptPlan {
    pub element: PromptElement,
    pub args: BTreeMap<String, String>,
    pub input: PromptInput,
}

const USAGE_ERROR_MARKER: &str = "\u{0}usage";

fn usage_error(message: &str) -> String {
    format!("{}{}", USAGE_ERROR_MARKER, message)
}

pub fn is_usage_error(error: &str) -> bool {
    error.starts_with(USAGE_ERROR_MARKER)
}

pub fn error_text(error: &str) -> &str {
    error.strip_prefix(USAGE_ERROR_MARKER).unwrap_or(error)
}

pub fn prompt_error_message(error: &str, element: &str, color: bool) -> String {
    let mut message = format!("error: {}\n", error_text(error));
    if is_usage_error(error) {
        message.push('\n');
        message.push_str(&zellij_utils::cli::prompt_element_help(element, color));
    }
    message
}

fn check_pattern(pattern: &str) -> Result<(), String> {
    regex::Regex::new(pattern)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

pub fn plan_prompt(
    prompt_cli: &PromptCli,
    read_stdin: impl FnOnce() -> Result<String, String>,
    stdin_is_piped: bool,
) -> Result<PromptPlan, String> {
    let mut args = BTreeMap::new();
    let answer_args = prompt_cli.answer_args();
    if let Some(title) = &prompt_cli.title {
        args.insert(prompt::ARG_TITLE.to_owned(), title.clone());
    }
    if let Some(timeout) = &prompt_cli.timeout {
        prompt::parse_duration(timeout).map_err(|e| format!("--timeout: {}", e))?;
        args.insert(prompt::ARG_TIMEOUT.to_owned(), timeout.clone());
    }
    if let Some(default) = &answer_args.default {
        args.insert(prompt::ARG_DEFAULT.to_owned(), default.clone());
    }
    if answer_args.json {
        args.insert(prompt::ARG_JSON.to_owned(), "true".to_owned());
    }
    let mut input = PromptInput::Single(None);
    let element = match &prompt_cli.element {
        PromptElementCli::Confirm {
            message, yes, no, ..
        } => {
            let message = message
                .as_ref()
                .ok_or_else(|| usage_error("confirm: the question is missing"))?;
            args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            if let Some(yes) = yes {
                args.insert(prompt::ARG_YES.to_owned(), yes.clone());
            }
            if let Some(no) = no {
                args.insert(prompt::ARG_NO.to_owned(), no.clone());
            }
            if let Some(default) = &answer_args.default {
                prompt::parse_bool(default).map_err(|e| format!("--default: {}", e))?;
            }
            PromptElement::Confirm
        },
        PromptElementCli::Choose {
            items,
            item,
            multi,
            labels,
            null,
            selected,
            ..
        } => {
            let mut choices: Vec<ChoiceItem> =
                items.iter().map(|i| ChoiceItem::plain(i.clone())).collect();
            choices.extend(item.iter().map(|i| ChoiceItem::from_item_arg(i)));
            if choices.is_empty() {
                if !stdin_is_piped {
                    return Err(usage_error(
                        "choose: no items given; pass them as arguments or pipe them into stdin",
                    ));
                }
                input = PromptInput::StreamStdin { null: *null };
            } else {
                args.insert(prompt::ARG_ITEMS.to_owned(), prompt::encode_items(&choices));
            }
            if *multi {
                args.insert(prompt::ARG_MULTI.to_owned(), "true".to_owned());
            }
            if *labels {
                args.insert(prompt::ARG_LABELS.to_owned(), "true".to_owned());
            }
            if *null {
                args.insert(prompt::ARG_NULL.to_owned(), "true".to_owned());
            }
            if !selected.is_empty() {
                args.insert(
                    prompt::ARG_SELECTED.to_owned(),
                    prompt::encode_list(selected),
                );
            }
            PromptElement::Choose
        },
        PromptElementCli::Input {
            message,
            placeholder,
            validate,
            required,
            ..
        } => {
            if let Some(message) = message {
                args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            }
            if let Some(placeholder) = placeholder {
                args.insert(prompt::ARG_PLACEHOLDER.to_owned(), placeholder.clone());
            }
            if let Some(validate) = validate {
                check_pattern(validate).map_err(|e| format!("--validate: {}", e))?;
                args.insert(prompt::ARG_VALIDATE.to_owned(), validate.clone());
            }
            if *required {
                args.insert(prompt::ARG_REQUIRED.to_owned(), "true".to_owned());
            }
            PromptElement::Input
        },
        PromptElementCli::Number {
            message,
            min,
            max,
            step,
            ..
        } => {
            if let Some(message) = message {
                args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            }
            if let (Some(min), Some(max)) = (min, max) {
                if min > max {
                    return Err("--min is larger than --max".to_owned());
                }
            }
            if let Some(step) = step {
                if *step <= 0 {
                    return Err("--step must be larger than 0".to_owned());
                }
                args.insert(prompt::ARG_STEP.to_owned(), step.to_string());
            }
            if let Some(min) = min {
                args.insert(prompt::ARG_MIN.to_owned(), min.to_string());
            }
            if let Some(max) = max {
                args.insert(prompt::ARG_MAX.to_owned(), max.to_string());
            }
            if let Some(default) = &answer_args.default {
                let value = default
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("--default: '{}' is not a whole number", default))?;
                let below = min.map(|min| value < min).unwrap_or(false);
                let above = max.map(|max| value > max).unwrap_or(false);
                if below || above {
                    let range = match (min, max) {
                        (Some(min), Some(max)) => format!("between {} and {}", min, max),
                        (Some(min), None) => format!("at least {}", min),
                        (_, Some(max)) => format!("at most {}", max),
                        _ => String::new(),
                    };
                    return Err(format!("--default: {} is not {}", value, range));
                }
            }
            PromptElement::Number
        },
        PromptElementCli::Toggle { message, .. } => {
            let message = message
                .as_ref()
                .ok_or_else(|| usage_error("toggle: the label is missing"))?;
            args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            if let Some(default) = &answer_args.default {
                prompt::parse_bool(default).map_err(|e| format!("--default: {}", e))?;
            }
            PromptElement::Toggle
        },
        PromptElementCli::Select {
            message, options, ..
        } => {
            if options.is_empty() {
                return Err(usage_error("select: give a label followed by the options"));
            }
            if let Some(message) = message {
                args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            }
            if let Some(default) = &answer_args.default {
                if !options.contains(default) {
                    return Err(format!(
                        "--default: '{}' is not one of the options",
                        default
                    ));
                }
            }
            args.insert(prompt::ARG_OPTIONS.to_owned(), prompt::encode_list(options));
            PromptElement::Select
        },
        PromptElementCli::Menu { items, item, .. } => {
            let mut choices: Vec<ChoiceItem> =
                items.iter().map(|i| ChoiceItem::plain(i.clone())).collect();
            choices.extend(item.iter().map(|i| ChoiceItem::from_item_arg(i)));
            if choices.is_empty() {
                return Err(usage_error("menu: no items given"));
            }
            args.insert(prompt::ARG_ITEMS.to_owned(), prompt::encode_items(&choices));
            PromptElement::Menu
        },
        PromptElementCli::Form { spec, .. } => {
            let text = match spec {
                Some(path) => fs::read_to_string(path)
                    .map_err(|e| format!("failed to read {}: {}", path.display(), e))?,
                None => {
                    if !stdin_is_piped {
                        return Err(usage_error(
                            "form: pass the form description with --spec or on stdin",
                        ));
                    }
                    read_stdin()?
                },
            };
            let form_spec = prompt::parse_form_spec(&text)?;
            prompt::check_form_patterns(&form_spec, check_pattern)?;
            if let Some(default) = &answer_args.default {
                match serde_json::from_str::<serde_json::Value>(default) {
                    Ok(serde_json::Value::Object(_)) => {},
                    _ => return Err("--default: must be a JSON object for form".to_owned()),
                }
            }
            input = PromptInput::Single(Some(text));
            PromptElement::Form
        },
        PromptElementCli::Notify {
            message,
            no_pane_name,
            no_tab_name,
            ..
        } => {
            let message = message
                .as_ref()
                .ok_or_else(|| usage_error("notify: the text is missing"))?;
            args.insert(prompt::ARG_MESSAGE.to_owned(), message.clone());
            if *no_pane_name {
                args.insert(prompt::ARG_NO_PANE_NAME.to_owned(), "true".to_owned());
            }
            if *no_tab_name {
                args.insert(prompt::ARG_NO_TAB_NAME.to_owned(), "true".to_owned());
            }
            PromptElement::Notify
        },
    };
    Ok(PromptPlan {
        element,
        args,
        input,
    })
}

/// Whether this pipe reads stdin at all, once the message it was invoked with has gone out.
///
/// Two things answer no, and only one of them used to be asked. Stdin that is a **terminal** is a
/// person at a keyboard rather than data waiting to be sent. And a payload given on the **command
/// line** is the whole pipe: `zellij pipe --name claim -- 'payload'` said its piece in the
/// invocation, so the stdin it inherited belongs to whoever spawned it and is nothing to do with
/// the pipe.
///
/// That second answer is what makes `--timeout` mean anything. The read it prevents sits outside
/// the idle window, so a stdin that is neither a terminal nor ever at EOF held the pipe open with
/// nothing able to end it - and that is the ordinary shape of an inherited stdin: a command run
/// over ssh, or a child of a long-lived server. `zellij pipe --timeout 5 --name claim -- 'payload'`
/// got its answer and then blocked forever anyway. Redirecting from `/dev/null` was the only cure,
/// and a caller should not have to know that.
///
/// The cost is that `echo data | zellij pipe --name n -- 'payload'` now sends only the payload. An
/// invocation that says its message twice was always ambiguous; the one that means the stream is
/// the one that leaves the payload off.
fn pipe_reads_stdin(stdin_is_piped: bool, payload_from_command_line: bool) -> bool {
    stdin_is_piped && !payload_from_command_line
}

/// The sentence a pipe that ran out of time prints on stderr.
///
/// Exit 2 is one bucket with several doors into it, so the sentence is what tells a script which
/// one this was. It names the pipe and the window, and it says why a pipe can go unanswered at
/// all - which is the thing a caller who has just met this for the first time does not know.
fn pipe_timed_out(pipe_id: &str, timeout: Duration) -> String {
    format!(
        "The pipe '{}' went unanswered for {}s and gave up. A pipe is unblocked by a plugin that \
         received it, and none did. `--timeout 0` waits for one however long it takes.",
        pipe_id,
        timeout.as_secs()
    )
}

/// A record report as one JSON object, or `None` for lines that are not a record.
///
/// A reporting verb prints `key: value` lines, so the object is that report with its keys kept and
/// its values typed the way the JSON surfaces already type them: `-` is the fork's "no value" and
/// becomes `null`, a whole number becomes a number, `true`/`false` become booleans, and everything
/// else stays a string. A pane id is a string in `list-panes --json` and stays one here.
///
/// `None` is the guard rather than a case that arises: a verb whose output is not a record is
/// refused before the call is made. If one ever reaches this, the lines are printed as they are
/// rather than folded into an object that has lost half of them.
///
/// The object is written out rather than collected into a `serde_json::Map`, because that map is a
/// `BTreeMap` here and would alphabetise the keys. The surface promises them in the order the
/// command prints them, and a reader of `--json` should meet the same order.
fn json_report(lines: &[String]) -> Option<String> {
    let mut pairs = Vec::with_capacity(lines.len());
    for line in lines {
        let (key, value) = line.split_once(": ")?;
        if key.is_empty() || !key.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
            return None;
        }
        pairs.push(format!(
            "  {}: {}",
            serde_json::Value::String(key.to_owned()),
            json_value(value)
        ));
    }
    Some(if pairs.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{\n{}\n}}", pairs.join(",\n"))
    })
}

/// One reported value, typed.
fn json_value(value: &str) -> serde_json::Value {
    match value {
        // the fork prints `-` where a field has no value, rather than an empty column
        "-" => serde_json::Value::Null,
        "true" => serde_json::Value::Bool(true),
        "false" => serde_json::Value::Bool(false),
        _ => match value.parse::<i64>() {
            Ok(number) => serde_json::Value::from(number),
            Err(_) => serde_json::Value::String(value.to_owned()),
        },
    }
}

/// The report the caller asked for, in the shape they asked for it.
///
/// A verb that reported nothing still answers `{}` under `--json`: a parser asked for an object
/// and "there was nothing to say" is one. Without the flag it prints nothing, exactly as before.
fn print_report(lines: &[String], output_json: bool) {
    if !output_json {
        lines.iter().for_each(|line| println!("{line}"));
        return;
    }
    match json_report(lines) {
        Some(json) => println!("{}", json),
        None => {
            eprintln!(
                "This report is not a record, so it is printed as it stands rather than as JSON."
            );
            lines.iter().for_each(|line| println!("{line}"));
        },
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
        sock_dir.push(zellij_utils::sessions::session_id_for(session_name));
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    os_input
        .connect_to_server(&*zellij_ipc_pipe)
        .map_err(|e| e.to_string())?;
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

/// fork: the stable id of the tab a `move-tab --to-index` without `--tab-id` moves - the tab a
/// connected client has focused, else the first tab. `Ok(None)` is a session with no tabs.
///
/// Upstream's `MoveTabToPosition` names its tab, so the client asks for it here, the way
/// `--in-tab` is asked. A detached session has no focused tab; the first one stands in for it.
pub fn resolve_focused_tab(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
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
        "the focused tab",
    )?;
    let tabs: Vec<zellij_utils::data::TabInfo> = serde_json::from_str(&lines.join("\n"))
        .map_err(|e| format!("Could not read the session's tabs: {}", e))?;
    Ok(tabs
        .iter()
        .find(|tab| tab.active)
        .or_else(|| tabs.iter().min_by_key(|tab| tab.position))
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
    /// The action's report. It is returned rather than printed because `--wait-ready` still has
    /// lines to add to it, and a report is printed once or not at all.
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
        match os_input
            .try_recv_from_server()
            .map_err(|e| matches!(e, IpcReceiveError::Disconnected))
        {
            Ok((ServerToClientMsg::UnblockInputThread, _)) if !is_blocking => {
                return ActionOutcome::Reported(Vec::new());
            },
            Ok((ServerToClientMsg::Log { lines: log_lines }, _)) => {
                return ActionOutcome::Reported(log_lines);
            },
            Ok((ServerToClientMsg::LogError { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| eprintln!("{line}"));
                return ActionOutcome::Done(2);
            },
            Ok((ServerToClientMsg::Exit { exit_reason }, _)) => {
                let (outcome, diagnostic) = outcome_of_exit(exit_reason);
                if let Some(diagnostic) = diagnostic {
                    eprintln!("{}", diagnostic);
                }
                return outcome;
            },
            // fork: a dropped connection is the same unknown as a `Disconnect` - the command may
            // have run - so it gets that answer and exit 1, not a miss a caller would resend on
            Err(true) => {
                let (outcome, diagnostic) = outcome_of_exit(ExitReason::Disconnect);
                if let Some(diagnostic) = diagnostic {
                    eprintln!("{}", diagnostic);
                }
                return outcome;
            },
            _ => {},
        }
    }
}

/// What an `Exit` from the session means for the command that is waiting for its report, and the
/// sentence to print on stderr before returning it.
///
/// A **disconnect is not an answer**. The session hands one to a client whose sender it dropped,
/// which happens to a `zellij action` client that has done nothing wrong: client ids are reused as
/// soon as they are freed, so a connection can be torn down by the departure of the one before it.
/// Reading that as "the session reported nothing" is what made `new-pane` print `No pane
/// was created` for a pane the session had made and confirmed. The command may well have run, so
/// the honest answer is to say the connection went and let the caller ask.
///
/// The rest keep their meanings: an error is the server's own words and the one exit code the
/// client has for them, a custom status is the status, and an ordinary end of the session is a
/// command that reported nothing.
fn outcome_of_exit(exit_reason: ExitReason) -> (ActionOutcome, Option<String>) {
    match exit_reason {
        ExitReason::Error(e) => (ActionOutcome::Done(2), Some(e)),
        ExitReason::CustomExitStatus(exit_status) => (ActionOutcome::Done(exit_status), None),
        ExitReason::Disconnect => (
            ActionOutcome::Done(1),
            Some(
                "The session dropped this connection before it answered. Whether the command ran \
                 is unknown; ask the session before sending it again."
                    .to_owned(),
            ),
        ),
        _ => (ActionOutcome::Reported(Vec::new()), None),
    }
}

/// The pane a creating command's report says it made.
/// A creation report with `--wait-ready`'s two lines added: which signal said the pane was ready,
/// and how long after it was made.
fn with_ready(mut lines: Vec<String>, ready_by: ReadyBy, waited: Duration) -> Vec<String> {
    lines.push(format!("ready_by: {}", ready_by.as_str()));
    lines.push(format!("ready_ms: {}", waited.as_millis()));
    lines
}

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
    /// The pane's shell is ready for input. The duration is the quiet window of the fallback.
    Ready(Duration),
    /// The coding agent in the pane stopped working (`waiting: false`), or is blocked on a
    /// person (`waiting: true`). `quiet` is the silence that answers for an agent that reports no
    /// state.
    Agent {
        waiting: bool,
        quiet: Duration,
    },
}

/// Reads `list-panes --json` for the one pane the wait is about.
///
/// A pane missing from the list is `Gone` rather than an error: a wait for an exit that ended in
/// the pane closing got what it asked for.
fn pane_state(panes_json: &str, pane: PaneId) -> Result<PaneState, String> {
    pane_look(panes_json, pane).map(|(state, _)| state)
}

/// [`pane_state`], and what the pane's shell says it is doing: its OSC 133 `command_state`, absent
/// for a shell that writes no markers - and for one whose markers a clear has just wiped.
///
/// A prompt the shell drew BEFORE the last input typed into the pane is read as `running`. Right
/// after a command is typed, the last marker is still that old prompt until the shell's preexec
/// hook writes the output marker, so taking it as a prompt would answer "ready" for a command that
/// has not started. Read as running, it is neither ready nor quiet, and the wait holds until the
/// shell draws a prompt after the input - which is the typed command finishing.
fn pane_look(
    panes_json: &str,
    pane: PaneId,
) -> Result<(PaneState, Option<PaneCommandState>), String> {
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
        None => (PaneState::Gone, None),
        Some(entry) if entry.pane_info.exited => {
            (PaneState::Exited(entry.pane_info.exit_status), None)
        },
        Some(entry) => {
            // the markers' word only: a `running` filled from the foreground process would hold
            // a wait on a pane whose program never ends, where the quiet fallback answers
            let command_state = match entry.command_state_from_markers() {
                Some(PaneCommandState::Prompt) if entry.input_since_prompt => {
                    Some(PaneCommandState::Running)
                },
                other => other,
            };
            (PaneState::Running, command_state)
        },
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

/// The sentence a render wait prints when its deadline passes.
///
/// A `--for match` miss names the pattern and the two rules that make a wait miss a line the
/// caller can see: lines already on screen when the wait began are the baseline, and a line
/// identical to one already on screen is not new. Both read as "the regex is wrong" otherwise.
fn timed_out_message(pane: PaneId, watching: &Watching) -> String {
    match watching {
        Watching::Match(pattern) => format!(
            "The wait on {} timed out: no new line matched the regex `{}`. Only a line drawn after \
             the wait began counts - lines already on screen do not, and neither does a line \
             identical to one already on screen.",
            pane,
            pattern.as_str()
        ),
        Watching::Quiet(_) => format!("The wait on {} timed out.", pane),
    }
}

/// How long is left before a deadline, or `None` once it has passed.
fn remaining(deadline: Option<Instant>, now: Instant) -> Option<Duration> {
    deadline.map(|deadline| deadline.saturating_duration_since(now))
}

/// The report a met wait prints: how long it took, and what it saw.
fn wait_report(waited: Duration, found: Option<String>) -> Vec<String> {
    let mut lines = vec![format!("waited_ms: {}", waited.as_millis())];
    // a wait may report several keys, one per line
    lines.extend(
        found
            .iter()
            .flat_map(|found| found.lines().map(str::to_owned)),
    );
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
    output_json: bool,
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
        WaitCondition::Ready(quiet) => {
            wait_for_ready(make_os_input, session_name, pane, quiet, deadline)
                .map(|ready_by| Some(format!("ready_by: {}", ready_by.as_str())))
        },
        WaitCondition::Agent { waiting, quiet } => {
            wait_for_agent(make_os_input, session_name, pane, waiting, quiet, deadline)
                .map(|verdict| Some(agent_report(&verdict).join("\n")))
        },
    };
    match outcome {
        Ok(found) => {
            print_report(&wait_report(started.elapsed(), found), output_json);
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
    let agents = match agents_of_session(os_input, session_name) {
        Ok(answer) => answer,
        Err(message) => {
            eprintln!("{}", message);
            return 1;
        },
    };
    print_agents(&agents, output_json, false)
}

/// `zellij action last-reply`: the agent's last reply, from its harness's transcript.
///
/// The pane is resolved the way every addressed verb resolves one. Its row in the agent list
/// names the harness and the session id, and [`crate::agent_reply`] finds and reads the transcript on this machine.
///
/// Exit 0 printed a reply. Exit 2 is a miss with the reason on stderr: not an agent, no session
/// id yet, a harness whose transcript this build does not read, no transcript file, or nothing
/// written since the last prompt. Exit 1 is a session that could not be asked.
pub fn start_last_reply_client(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    target: &str,
    output_json: bool,
) -> i32 {
    let pane = match resolve_pane_target(make_os_input(), session_name, target) {
        Ok(pane) => pane,
        Err(message) => {
            eprintln!("{}", message);
            return 2;
        },
    };
    let agents = match agents_of_session(make_os_input(), session_name) {
        Ok(agents) => agents,
        Err(message) => {
            eprintln!("{}", message);
            return 1;
        },
    };
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let var = |name: &str| std::env::var(name).ok();
    match last_reply_of(&agents, pane, &var, home.as_deref()) {
        Ok(found) => {
            if output_json {
                println!("{}", serde_json::to_string(&found).unwrap_or_default());
            } else {
                println!("{}", found.reply);
            }
            0
        },
        Err(message) => {
            eprintln!("{}: {}", target, message);
            2
        },
    }
}

/// What `last-reply --json` prints.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LastReplyReport {
    pub pane_id: String,
    pub handle: String,
    pub kind: String,
    pub agent_id: String,
    /// The transcript the reply was read from.
    pub transcript: PathBuf,
    /// The agent's `OSC 7501` state, when it reports one: `done` or `idle` means the turn ended.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    pub reply: String,
}

/// The last reply of the agent in `pane`, or why there is none. Pure but for the file reads.
fn last_reply_of(
    agents: &ListAgentsResponse,
    pane: PaneId,
    var: &dyn Fn(&str) -> Option<String>,
    home: Option<&std::path::Path>,
) -> Result<LastReplyReport, String> {
    let PaneId::Terminal(id) = pane else {
        return Err("a plugin pane runs no coding agent.".to_owned());
    };
    let agent = agents
        .iter()
        .find(|agent| agent.pane_id == id)
        .ok_or_else(|| {
            "it is not running a coding agent (`list-agents` does not list it).".to_owned()
        })?;
    let kind = agent.agent.kind.as_str();
    if !crate::agent_reply::reads_transcript_of(kind) {
        return Err(format!(
            "{} keeps its sessions where this build does not read them. Read the pane's screen \
             with dump-screen instead.",
            kind
        ));
    }
    let agent_id = agent.agent.agent_id.clone().ok_or_else(|| {
        format!(
            "no {} session id is known for it yet, so its transcript cannot be found. A {} that \
             has not been sent a message has none.",
            kind, kind
        )
    })?;
    let root = crate::agent_reply::transcript_root(kind, var, home)
        .ok_or_else(|| "no home directory to look for the transcript in.".to_owned())?;
    let transcript =
        crate::agent_reply::find_transcript(kind, &root, &agent_id).ok_or_else(|| {
            format!(
                "no transcript of {} session {} under {}.",
                kind,
                agent_id,
                root.display()
            )
        })?;
    let text = crate::agent_reply::read_tail(&transcript)
        .map_err(|e| format!("could not read {}: {}", transcript.display(), e))?;
    let found = crate::agent_reply::last_reply(kind, &text);
    let state = agent
        .program_status
        .as_ref()
        .map(|status| status.state.clone());
    if found.reply.is_empty() {
        return Err(format!(
            "the agent has written no reply since its last prompt{}.",
            match (&state, found.mid_tool) {
                (Some(state), _) => format!(" (state: {})", state),
                (None, true) => " (it is running a tool)".to_owned(),
                (None, false) => String::new(),
            }
        ));
    }
    Ok(LastReplyReport {
        pane_id: pane.to_string(),
        handle: agent.handle.clone(),
        kind: kind.to_owned(),
        agent_id,
        transcript,
        state,
        reply: found.reply,
    })
}

/// `zellij action list-agents --all-sessions`: the same rows, from every live session, each
/// carrying the session it came from.
///
/// One `list-panes --json` per session, each on its own connection. A session that does not
/// answer is named on stderr and left out rather than failing the rest: the question is about
/// the machine, and one wedged server should not hide every other agent on it. Exit 1 only when
/// there were sessions and none of them answered.
pub fn start_list_agents_all_sessions_client(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_names: &[String],
    output_json: bool,
) -> i32 {
    let (agents, answered) = agents_of_sessions(make_os_input, session_names);
    if answered == 0 && !session_names.is_empty() {
        return 1;
    }
    print_agents(&agents, output_json, true)
}

fn active_agents(panes: Vec<PaneListEntry>) -> ListAgentsResponse {
    agent_detect::agents_from_pane_list(
        panes
            .into_iter()
            .filter(|pane| !pane.pane_info.exited && !pane.pane_info.is_held)
            .collect(),
    )
}

fn agent_change_snapshot(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    filter: Option<&[u32]>,
) -> Result<ListAgentsResponse, String> {
    agent_snapshot(make_os_input, session_name, filter)
}

/// The agents of one session as a teardown sees them. `zellij session restart` and `session down` ask this before they
/// kill anything: the rows of `list-agents`, the caller's own pane marked, and the screen
/// detector's `blocked` where an agent reported no state.
pub fn agents_before_teardown(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
) -> Result<ListAgentsResponse, String> {
    agent_snapshot(make_os_input, session_name, None)
}

fn agent_snapshot(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    filter: Option<&[u32]>,
) -> Result<ListAgentsResponse, String> {
    let panes = panes_of_session(make_os_input(), session_name)?;
    let mut agents = active_agents(panes);
    agent_detect::mark_self(
        &mut agents,
        std::env::var("ZELLIJ_PANE_ID")
            .ok()
            .and_then(|id| id.parse().ok()),
        std::env::var("ZELLIJ_SESSION_NAME").ok().as_deref() == Some(session_name),
    );
    if let Some(panes) = filter {
        agents.retain(|agent| {
            if panes.is_empty() {
                !agent.is_self
            } else {
                panes.contains(&agent.pane_id)
            }
        });
    }
    for agent in &mut agents {
        if agent.program_status.is_none() {
            if let Ok(lines) = ask(
                make_os_input(),
                session_name,
                Action::DumpScreen {
                    file_path: None,
                    include_scrollback: false,
                    pane_id: Some(PaneId::Terminal(agent.pane_id)),
                    ansi: false,
                },
                "the agent screen",
            ) {
                if agent_detect::screen_agent_blocked(&lines.join("\n")) {
                    agent.program_status = Some(zellij_utils::data::PaneProgramStatus {
                        state: "blocked".into(),
                        kind: Some("question".into()),
                        app: None,
                        title: None,
                        msg: None,
                        progress: None,
                        at: 0,
                        working: 0,
                    });
                }
            }
        }
    }
    Ok(agents)
}

fn enrich_agent_change(change: &mut agent_detect::AgentChange, agents: &ListAgentsResponse) {
    let var = |key: &str| std::env::var(key).ok();
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    enrich_agent_change_from(change, agents, &var, home.as_deref());
}

fn enrich_agent_change_from(
    change: &mut agent_detect::AgentChange,
    agents: &ListAgentsResponse,
    var: &dyn Fn(&str) -> Option<String>,
    home: Option<&std::path::Path>,
) {
    if (change.old_state != change.new_state
        && matches!(
            change.new_state.as_deref(),
            Some("idle" | "done" | "error" | "blocked")
        ))
        || change.event == "gone"
    {
        let scoped = agents
            .iter()
            .filter(|agent| agent.session == change.session)
            .cloned()
            .collect();
        change.last_reply = last_reply_of(&scoped, PaneId::Terminal(change.pane_id), var, home)
            .ok()
            .and_then(|reply| {
                reply
                    .reply
                    .lines()
                    .find(|line| !line.trim().is_empty())
                    .map(str::to_owned)
            });
    }
}

fn agent_snapshot_answer<T>(
    receiver: std::sync::mpsc::Receiver<T>,
    deadline: Option<Instant>,
) -> Result<T, String> {
    match deadline {
        Some(end) => receiver
            .recv_timeout(end.saturating_duration_since(Instant::now()))
            .map_err(|_| "Agent-change wait timed out".to_owned()),
        None => receiver
            .recv()
            .map_err(|_| "Agent-change snapshot ended".to_owned()),
    }
}

/// Block until a watched agent changes. The caller is excluded only for an unscoped wait.
pub fn start_agent_change_client(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    panes: &[u32],
    timeout: Option<Duration>,
    output_json: bool,
) -> i32 {
    let deadline = timeout.map(|timeout| Instant::now() + timeout);
    let snapshot = || {
        let input = make_os_input();
        let session = session_name.to_owned();
        let panes = panes.to_vec();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(agent_change_snapshot(
                &|| input.box_clone(),
                &session,
                Some(&panes),
            ));
        });
        agent_snapshot_answer(receiver, deadline)?
    };
    let mut previous = match snapshot() {
        Ok(agents) => agents,
        Err(message) => {
            eprintln!("{}", message);
            return 2;
        },
    };
    loop {
        if deadline.map(|end| Instant::now() >= end).unwrap_or(false) {
            eprintln!("Agent-change wait timed out");
            return 2;
        }
        std::thread::sleep(
            Duration::from_millis(100).min(
                deadline
                    .map(|end| end.saturating_duration_since(Instant::now()))
                    .unwrap_or(Duration::from_millis(100)),
            ),
        );
        let current = match snapshot() {
            Ok(agents) => agents,
            Err(message) => {
                eprintln!("{}", message);
                return 2;
            },
        };
        if deadline.map(|end| Instant::now() >= end).unwrap_or(false) {
            eprintln!("Agent-change wait timed out");
            return 2;
        }
        if let Some(mut change) = agent_detect::agent_changes(&previous, &current)
            .into_iter()
            .next()
        {
            let source = if change.event == "gone" {
                &previous
            } else {
                &current
            };
            enrich_agent_change(&mut change, source);
            if output_json {
                println!("{}", serde_json::to_string(&change).unwrap());
            } else {
                println!(
                    "handle: {}\npane_id: {}\ntab_id: {}\ntab_name: {}\nkind: {}\nname: {}\nold_state: {}\nnew_state: {}\nevent: {}\nsession: {}\nlast_reply: {}",
                    change.handle, change.pane_id, change.tab_id, change.tab_name, change.kind,
                    change.name.as_deref().unwrap_or("-"), change.old_state.as_deref().unwrap_or("-"),
                    change.new_state.as_deref().unwrap_or("-"), change.event,
                    change.session.as_deref().unwrap_or("-"), change.last_reply.as_deref().unwrap_or("-")
                );
            }
            return 0;
        }
        previous = current;
    }
}

/// How long a session event stream waits for its `hello` before calling the server too old. A
/// server that predates the stream ignores the subscription it does not know, and says nothing.
const SESSION_EVENT_HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// How a session event stream ended.
#[derive(Debug, PartialEq, Eq)]
pub enum SessionEventStreamEnd {
    /// The reader closed the pipe: the way a watch is meant to end.
    ReaderGone,
    /// The session went away, or stopped answering.
    ServerGone(String),
    /// The server never said hello: it predates the stream.
    NoStream(String),
}

/// Open a session event stream and hand every line to `print`, until the reader or the server
/// goes. `print` returns false when its reader has gone.
fn run_session_event_stream(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    print: &mut dyn FnMut(&str) -> bool,
) -> SessionEventStreamEnd {
    let socket = match socket_for(session_name) {
        Ok(socket) => socket,
        Err(message) => return SessionEventStreamEnd::ServerGone(message),
    };
    if let Err(e) = os_input.connect_to_server(&socket) {
        return SessionEventStreamEnd::ServerGone(e.to_string());
    }
    os_input.send_to_server(ClientToServerMsg::SubscribeToSessionEvents { agents_only: false });
    let (sender, messages) = mpsc::channel();
    let reader = os_input.box_clone();
    std::thread::spawn(move || {
        while let Some((message, _)) = reader.recv_from_server() {
            if sender.send(message).is_err() {
                break;
            }
        }
    });
    let own_build_id = zellij_utils::session_lifecycle::own_executable()
        .and_then(|identity| identity.build_id)
        .map(|id| {
            id.iter()
                .map(|byte| format!("{:02x}", byte))
                .collect::<String>()
        });
    let end = session_event_stream_loop(&messages, session_name, own_build_id, print);
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    end
}

fn session_event_stream_loop(
    messages: &mpsc::Receiver<ServerToClientMsg>,
    session_name: &str,
    own_build_id: Option<String>,
    print: &mut dyn FnMut(&str) -> bool,
) -> SessionEventStreamEnd {
    let started = Instant::now();
    let mut said_hello = false;
    let gone = || {
        SessionEventStreamEnd::ServerGone(format!(
            "Session '{}' went away while it was being watched.",
            session_name
        ))
    };
    loop {
        if !said_hello && started.elapsed() >= SESSION_EVENT_HELLO_TIMEOUT {
            return SessionEventStreamEnd::NoStream(format!(
                "Session '{}' did not start an event stream: its server predates `zellij action \
                 watch`. Restart the session on this build to watch it.",
                session_name
            ));
        }
        match messages.recv_timeout(Duration::from_millis(500)) {
            Ok(ServerToClientMsg::SessionEvent { line }) => {
                let line = if said_hello {
                    line
                } else {
                    said_hello = true;
                    zellij_utils::event_stream::with_server_build(&line, own_build_id.as_deref())
                };
                if !print(&line) {
                    return SessionEventStreamEnd::ReaderGone;
                }
            },
            Ok(ServerToClientMsg::Exit { .. }) => return gone(),
            Ok(ServerToClientMsg::LogError { lines }) => {
                return SessionEventStreamEnd::ServerGone(lines.join("\n"))
            },
            Ok(_) => {},
            Err(RecvTimeoutError::Timeout) => {
                if zellij_utils::event_stream::stdout_reader_gone() {
                    return SessionEventStreamEnd::ReaderGone;
                }
            },
            Err(RecvTimeoutError::Disconnected) => return gone(),
        }
    }
}

/// Print one line and flush it, so a reader sees each change as it lands. False when the reader
/// has gone.
fn print_stream_line(line: &str) -> bool {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", line).is_ok() && out.flush().is_ok()
}

/// `zellij action watch`: every line of the session's event stream, until the reader stops
/// reading (exit 0) or the session goes (exit 2).
pub fn start_watch_client(os_input: Box<dyn ClientOsApi>, session_name: &str) -> i32 {
    match run_session_event_stream(os_input, session_name, &mut print_stream_line) {
        SessionEventStreamEnd::ReaderGone => 0,
        SessionEventStreamEnd::ServerGone(message) | SessionEventStreamEnd::NoStream(message) => {
            eprintln!("{}", message);
            2
        },
    }
}

/// Show the caller's pane and its session's live agents.
pub fn start_whoami_client(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    output_json: bool,
) -> i32 {
    if std::env::var("ZELLIJ_SESSION_NAME")
        .ok()
        .as_deref()
        .is_some_and(|own_session| own_session != session_name)
    {
        eprintln!("whoami must target the caller's own session");
        return 2;
    }
    let caller = match std::env::var("ZELLIJ_PANE_ID").ok().and_then(|id| {
        id.strip_prefix("terminal_")
            .unwrap_or(&id)
            .parse::<u32>()
            .ok()
    }) {
        Some(id) => id,
        None => {
            eprintln!("whoami needs ZELLIJ_PANE_ID from a terminal pane");
            return 2;
        },
    };
    let panes = match panes_of_session(os_input, session_name) {
        Ok(panes) => panes,
        Err(message) => {
            eprintln!("{}", message);
            return 1;
        },
    };
    let pane = match panes
        .iter()
        .find(|p| !p.pane_info.is_plugin && p.pane_info.id == caller)
    {
        Some(pane) => pane,
        None => {
            eprintln!("whoami could not find the caller's pane in this session");
            return 2;
        },
    };
    let report = whoami_report(
        session_name,
        zellij_utils::shared::short_hostname(),
        pane,
        &panes,
    );
    if output_json {
        println!("{}", serde_json::to_string(&report).unwrap_or_default());
    } else {
        println!("{}", whoami_text(&report));
    }
    0
}

fn whoami_report(
    session: &str,
    host: &str,
    pane: &PaneListEntry,
    panes: &[PaneListEntry],
) -> serde_json::Value {
    let others = agent_detect::agents_from_pane_list(panes.to_vec())
        .into_iter()
        .filter(|agent| agent.pane_id != pane.pane_info.id)
        .collect::<Vec<_>>();
    serde_json::json!({
        "session": session, "host": host,
        "pane_id": format!("terminal_{}", pane.pane_info.id),
        "handle": pane.pane_info.handle,
        "tab_id": pane.pane_info.tab_id, "tab_name": pane.tab_name,
        "tab_position": pane.tab_position,
        "name": pane.agent.as_ref().and_then(|agent| agent.name.as_deref()),
        "agents": others,
    })
}

fn whoami_text(report: &serde_json::Value) -> String {
    let string = |key: &str| report[key].as_str().unwrap_or("unnamed");
    let mut lines = vec![format!(
        "You are {} in session {} on {}, tab {} (position {}), pane {} ({}).",
        string("name"),
        string("session"),
        string("host"),
        string("tab_name"),
        report["tab_position"],
        string("handle"),
        string("pane_id")
    )];
    if let Some(agents) = report["agents"].as_array() {
        for agent in agents {
            lines.push(format!(
                "Agent {}: {}, tab {}, pane {}, state {}.",
                agent["name"].as_str().unwrap_or("unnamed"),
                agent["kind"].as_str().unwrap_or("unknown"),
                agent["tab_name"].as_str().unwrap_or(""),
                agent["handle"].as_str().unwrap_or(""),
                agent["state"].as_str().unwrap_or("unknown")
            ));
        }
    }
    lines.push("Agent list changes; run `zellij list-agents` before addressing another agent. To talk to an agent that has a name, message it by that name with your harness's messaging tool; type into its pane only when it has no messaging.".into());
    lines.join("\n")
}

/// The agents of one session.
fn agents_of_session(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
) -> Result<ListAgentsResponse, String> {
    let panes = panes_of_session(os_input, session_name)?;
    let mut agents = agent_detect::agents_from_pane_list(panes);
    agent_detect::mark_self(
        &mut agents,
        std::env::var("ZELLIJ_PANE_ID")
            .ok()
            .and_then(|id| id.parse().ok()),
        std::env::var("ZELLIJ_SESSION_NAME").ok().as_deref() == Some(session_name),
    );
    Ok(agents)
}

fn panes_of_session(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
) -> Result<Vec<PaneListEntry>, String> {
    let lines = ask(
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
    )?;
    // a session that answered with something other than a pane list is a bug rather than a
    // miss, so it is an error and says what it could not read
    serde_json::from_str(&lines.join("\n")).map_err(|e| {
        format!(
            "Could not read the pane list session '{}' answered with: {}",
            session_name, e
        )
    })
}

/// The agents of several sessions, each row naming its session, and how many sessions
/// answered. A session that did not answer is named on stderr.
fn agents_of_sessions(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_names: &[String],
) -> (ListAgentsResponse, usize) {
    let mut agents = Vec::new();
    let mut answered = 0;
    for session_name in session_names {
        match agents_of_session(make_os_input(), session_name) {
            Ok(session_agents) => {
                answered += 1;
                agents.extend(session_agents.into_iter().map(|mut agent| {
                    agent.session = Some(session_name.clone());
                    agent
                }));
            },
            Err(message) => eprintln!("session '{}': {}", session_name, message),
        }
    }
    (agents, answered)
}

/// One `list-agents --json` row: the agent, and how a caller reaches it to send it a message.
///
/// The keys after the agent's own are read from its transcript on this machine (see
/// [`crate::agent_reply::ChatMeta`]), so they are a client-side addition like the rest of the row.
/// They are JSON-only: the table keeps its columns, and `action watch` never sees them.
#[derive(serde::Serialize)]
struct AgentRow<'a> {
    #[serde(flatten)]
    agent: &'a zellij_utils::data::AgentListEntry,
    /// The address a harness's messaging tool takes, from any machine: `bridge:session_<X>`.
    #[serde(skip_serializing_if = "Option::is_none")]
    message_to: Option<String>,
    /// `harness` when `message_to` is there; `pane` when the way in is typing into the pane.
    message_route: &'static str,
    /// When the chat began, in milliseconds since the epoch.
    #[serde(skip_serializing_if = "Option::is_none")]
    chat_started_at: Option<u64>,
    /// Messages from other agents the session holds for this one (`send-message`). Written only
    /// when there are some.
    #[serde(skip_serializing_if = "is_no_count")]
    queued: usize,
}

fn is_no_count(count: &usize) -> bool {
    *count == 0
}

/// How long `list-agents --json` waits for transcripts, all agents together. A row whose
/// transcript was not read in time goes out without its chat keys rather than holding the list.
const CHAT_META_BUDGET: Duration = Duration::from_millis(1500);

/// The rows with their chat keys, the transcripts read on a thread of their own and given at most
/// `budget`.
fn agent_rows<'a>(
    agents: &'a ListAgentsResponse,
    var: &dyn Fn(&str) -> Option<String>,
    home: Option<&std::path::Path>,
    budget: Duration,
) -> Vec<AgentRow<'a>> {
    let mut metas: Vec<crate::agent_reply::ChatMeta> = vec![Default::default(); agents.len()];
    let wanted: Vec<(usize, String, Option<PathBuf>)> = agents
        .iter()
        .enumerate()
        .filter(|(_, agent)| agent.agent.kind == "claude")
        .filter_map(|(at, agent)| {
            let id = agent.agent.agent_id.clone()?;
            Some((at, id, agent.cwd.clone()))
        })
        .collect();
    let root = crate::agent_reply::transcript_root("claude", var, home);
    if let (false, Some(root)) = (wanted.is_empty(), root) {
        let (sender, receiver) = mpsc::channel();
        // detached: a read that hangs is abandoned with the thread when the list is printed
        std::thread::spawn(move || {
            for (at, id, cwd) in wanted {
                let meta = crate::agent_reply::find_claude_transcript(&root, &id, cwd.as_deref())
                    .map(|path| crate::agent_reply::chat_meta(&path))
                    .unwrap_or_default();
                if sender.send((at, meta)).is_err() {
                    return;
                }
            }
        });
        let deadline = Instant::now() + budget;
        while let Ok((at, meta)) =
            receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            metas[at] = meta;
        }
    }
    agents
        .iter()
        .zip(metas)
        .map(|(agent, meta)| AgentRow {
            agent,
            message_route: if meta.message_to.is_some() {
                "harness"
            } else {
                "pane"
            },
            message_to: meta.message_to,
            chat_started_at: meta.started_at,
            queued: agent.queued_messages,
        })
        .collect()
}

fn print_agents(agents: &ListAgentsResponse, output_json: bool, with_session: bool) -> i32 {
    if output_json {
        let var = |name: &str| std::env::var(name).ok();
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let rows = agent_rows(agents, &var, home.as_deref(), CHAT_META_BUDGET);
        match serde_json::to_string_pretty(&rows) {
            Ok(json) => println!("{}", json),
            Err(e) => {
                eprintln!("Could not write the agent list as JSON: {}", e);
                return 1;
            },
        }
    } else {
        let table = if with_session {
            agent_detect::agent_table_with_sessions(agents)
        } else {
            agent_detect::agent_table(agents)
        };
        for line in table {
            println!("{}", line);
        }
    }
    0
}

/// What one question got, on a connection that is about to be asked another.
///
/// The session answers a CLI message with its report and then, from the bottom of its route loop,
/// an `UnblockInputThread` addressed to whoever holds the input. A client that asks one question and
/// exits can ignore that second message; a client that asks several cannot, because the unblock it
/// left behind is the first thing the NEXT question reads, and every answer after that is one
/// question late.
enum Answer {
    /// The report, which is empty for a verb that had nothing to say.
    Reported(Vec<String>),
    /// The session refused or missed, in its own words. Already printed.
    Failed,
    /// The connection ended. Nothing more will come down it, whatever is still to ask.
    Ended(i32),
}

/// Asks one question down a connection that stays open, and reads to the end of its answer.
///
/// Returns with the connection at a message boundary: the report, then the unblock that closes it,
/// are both consumed. That is the whole difference from [`individual_messages_client`], which stops
/// at the report because the client it serves is about to exit.
fn ask_in_sequence(os_input: &mut Box<dyn ClientOsApi>, action: Action) -> Answer {
    os_input.send_to_server(ClientToServerMsg::Action {
        action,
        terminal_id: None,
        client_id: None,
        is_cli_client: true,
    });
    let mut answer = Answer::Reported(Vec::new());
    loop {
        match os_input.recv_from_server() {
            Some((ServerToClientMsg::Log { lines }, _)) => answer = Answer::Reported(lines),
            Some((ServerToClientMsg::LogError { lines }, _)) => {
                lines.iter().for_each(|line| eprintln!("{line}"));
                answer = Answer::Failed;
            },
            Some((ServerToClientMsg::UnblockInputThread, _)) => return answer,
            Some((ServerToClientMsg::Exit { exit_reason }, _)) => {
                let (outcome, diagnostic) = outcome_of_exit(exit_reason);
                if let Some(diagnostic) = diagnostic {
                    eprintln!("{}", diagnostic);
                }
                return match outcome {
                    ActionOutcome::Done(exit_status) => Answer::Ended(exit_status),
                    ActionOutcome::Reported(_) => Answer::Ended(0),
                };
            },
            Some(_) => {},
            // the socket went. Unlike a one-question client there is a loop above this one, so
            // saying so is what stops it rather than spinning on a connection that is gone
            None => return Answer::Ended(1),
        }
    }
}

/// `zellij action dump-screen` for more than one pane, or for `--all`.
///
/// The sweep is the point: a caller reaching a session over ssh pays for the round trip, not for the
/// dump, so N panes asked for in one invocation cost one connection and N local messages instead of
/// N of everything. Nothing about the request changes - it is the same per-pane `DumpScreen` the
/// session has always answered, asked N times - so this crosses no contract.
///
/// One JSON object per line, [`PaneDump`]. A single `--pane-id` never comes here: it keeps printing
/// the bare content it always did.
///
/// Exit 0 when every pane asked for answered, 2 when one did not - the miss every `--pane-id` verb
/// reports, said once per pane that missed and counted at the end. `--all` on a session with no
/// panes prints nothing and exits 0: nothing was asked for by name, so nothing is missing.
pub fn start_dump_screen_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    targets: Vec<String>,
    all: bool,
    full: bool,
    ansi: bool,
    file_path: Option<PathBuf>,
) -> i32 {
    // the targets are read before anything is asked. A string that names no pane in ANY form is
    // malformed input rather than a miss, and half-way through a sweep that has already printed
    // panes is the wrong place to discover it
    let mut wanted: Vec<(String, Option<PaneId>)> = Vec::with_capacity(targets.len());
    for target in targets {
        match target.parse::<PaneTarget>() {
            Ok(parsed) => {
                let known = parsed.as_pane_id();
                wanted.push((target, known));
            },
            Err(malformed) => {
                eprintln!("{}", malformed);
                return 1;
            },
        }
    }
    let zellij_ipc_pipe: PathBuf = {
        let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
        if let Err(e) = fs::create_dir_all(&sock_dir) {
            eprintln!("{}", e);
            return 1;
        }
        if let Err(e) = zellij_utils::shared::set_permissions(&sock_dir, 0o700) {
            eprintln!("{}", e);
            return 1;
        }
        sock_dir.push(zellij_utils::sessions::session_id_for(session_name));
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    if let Err(e) = os_input.connect_to_server(&*zellij_ipc_pipe) {
        eprintln!("{}", e);
        return 1;
    }
    // one listing, whatever was asked for: it is what `--all` sweeps, and it is where a named pane's
    // handle comes from. A dump says which pane it is, and the handle is the address a reader of the
    // sweep will use to talk to that pane
    let listed = match ask_in_sequence(
        &mut os_input,
        Action::ListPanes {
            show_tab: true,
            show_command: true,
            show_state: false,
            show_geometry: false,
            show_all: false,
            output_json: true,
        },
    ) {
        Answer::Reported(lines) => lines,
        Answer::Failed => return 1,
        Answer::Ended(exit_status) => return exit_status,
    };
    let listed: Vec<PaneListEntry> = match serde_json::from_str(&listed.join("\n")) {
        Ok(listed) => listed,
        Err(e) => {
            eprintln!(
                "Could not read the pane list this session answered with: {}",
                e
            );
            return 1;
        },
    };
    let handles: BTreeMap<PaneId, String> = listed
        .iter()
        .map(|entry| (pane_id_of(entry), entry.pane_info.handle.clone()))
        .collect();
    // a miss is per pane and does not end the sweep: one name nobody answers to should cost that
    // one pane, not the nine beside it that are there. The exit code carries it to the end
    let mut missed = false;
    let mut sweep: Vec<PaneId> = Vec::new();
    if all {
        sweep.extend(listed.iter().map(pane_id_of));
    } else {
        for (target, known) in wanted {
            match known {
                // an id form needs no lookup to be UNDERSTOOD, and whether a pane answers to it is
                // the dump's own question - the same one the single-pane form asks
                Some(pane_id) => sweep.push(pane_id),
                None => match ask_in_sequence(
                    &mut os_input,
                    Action::ResolvePaneTarget {
                        target: target.clone(),
                    },
                ) {
                    Answer::Reported(lines) => {
                        match lines
                            .first()
                            .and_then(|line| line.strip_prefix("pane_id: "))
                            .and_then(|id| PaneId::from_str(id).ok())
                        {
                            Some(pane_id) => sweep.push(pane_id),
                            None => {
                                eprintln!("Could not read the resolved pane for '{}'", target);
                                missed = true;
                            },
                        }
                    },
                    Answer::Failed => missed = true,
                    Answer::Ended(exit_status) => return exit_status,
                },
            }
        }
    }
    let mut out: Box<dyn Write> = match &file_path {
        Some(path) => match fs::File::create(path) {
            Ok(file) => Box::new(io::BufWriter::new(file)),
            Err(e) => {
                eprintln!("Could not write to '{}': {}", path.display(), e);
                return 1;
            },
        },
        None => Box::new(io::stdout()),
    };
    for pane_id in sweep {
        match ask_in_sequence(
            &mut os_input,
            Action::DumpScreen {
                file_path: None,
                include_scrollback: full,
                pane_id: Some(pane_id),
                ansi,
            },
        ) {
            Answer::Reported(lines) => {
                let dump = PaneDump {
                    pane_id: pane_id.to_string(),
                    handle: handles.get(&pane_id).cloned().unwrap_or_default(),
                    // the session answers with the dump as one string, so this is the same content
                    // the single-pane form prints, escaped into one line
                    content: lines.join("\n"),
                };
                let line = match serde_json::to_string(&dump) {
                    Ok(line) => line,
                    Err(e) => {
                        eprintln!("Could not write the dump of {} as JSON: {}", pane_id, e);
                        return 1;
                    },
                };
                if let Err(e) = writeln!(out, "{}", line) {
                    return end_of_writing(e);
                }
            },
            Answer::Failed => missed = true,
            Answer::Ended(exit_status) => return exit_status,
        }
    }
    if let Err(e) = out.flush() {
        return end_of_writing(e);
    }
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    if missed {
        2
    } else {
        0
    }
}

/// The pane a `list-panes` row is about.
///
/// The row carries the id and the kind separately, which is what the JSON says; a `PaneId` is the
/// two of them together, and is what everything else here holds a pane by.
fn pane_id_of(entry: &PaneListEntry) -> PaneId {
    if entry.pane_info.is_plugin {
        PaneId::Plugin(entry.pane_info.id)
    } else {
        PaneId::Terminal(entry.pane_info.id)
    }
}

/// A sweep that could not finish writing, and what that is worth as an exit code.
///
/// A reader that stopped reading - `| head -1` on a sweep of thirty panes - is not this command's
/// failure, and exits 0 saying nothing. Any other write error is one, and says so. The same answer
/// `session status` gives, for the same reason.
fn end_of_writing(e: io::Error) -> i32 {
    if e.kind() == io::ErrorKind::BrokenPipe {
        return 0;
    }
    eprintln!("Could not write the dump: {}", e);
    1
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
    let updates = subscribe_to_renders(&*os_input, session_name, pane)?;

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
                    break Err(WaitMiss::Missed(timed_out_message(pane, &watching)));
                }
                break Ok(None);
            },
        }
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    answer
}

/// Which signal said a pane was ready for input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadyBy {
    /// The pane's shell wrote an OSC 133 prompt marker: `command_state` reads `prompt`. Exact.
    Prompt,
    /// The pane drew something and then nothing for the quiet window, and its shell wrote no
    /// marker that says otherwise. A heuristic, for a shell without OSC 133.
    Quiet,
}

impl ReadyBy {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReadyBy::Prompt => "prompt",
            ReadyBy::Quiet => "quiet",
        }
    }
}

/// The decisions of a `--for ready` wait, apart from the session it asks, so they can be proven
/// from a scripted sequence of renders and looks.
///
/// Two signals, and the exact one wins:
///
/// - **The prompt.** A look at the session that reads `command_state: prompt` is ready at once.
/// - **Quiet after output.** A pane that has drawn a non-blank line and then nothing for the
///   window is ready - but only when its shell says nothing to the contrary. `running` means a
///   command is between its output marker and its end, and a silent build is not a ready shell.
///
/// A missing `command_state` is NOT taken as "this shell writes no markers, forever". A clear
/// resets the pane's markers, so a shell that does write them reads as absent until it draws its
/// next prompt - and one that redraws its prompt without running its hooks, as zsh's
/// `clear-screen` does, stays absent. Absent and `done` both leave the fallback in play.
#[derive(Debug)]
struct ReadyWatch {
    quiet: Duration,
    /// When the pane last drew anything, once it has drawn a non-blank line. `None` until then:
    /// a pane that has said nothing yet is not quiet, it has not started.
    last_output: Option<Instant>,
}

impl ReadyWatch {
    fn new(quiet: Duration) -> Self {
        ReadyWatch {
            quiet,
            last_output: None,
        }
    }

    /// A render of the pane's viewport arrived at `now`.
    fn rendered(&mut self, viewport: &[String], now: Instant) {
        if self.last_output.is_some() || viewport.iter().any(|line| !line.trim().is_empty()) {
            self.last_output = Some(now);
        }
    }

    /// When the quiet window closes, if the pane has drawn anything.
    fn quiet_at(&self) -> Option<Instant> {
        self.last_output.map(|at| at + self.quiet)
    }

    /// What a look at the session that read `command_state` says, at `now`.
    fn judge(&self, command_state: Option<PaneCommandState>, now: Instant) -> Option<ReadyBy> {
        match command_state {
            Some(PaneCommandState::Prompt) => Some(ReadyBy::Prompt),
            Some(PaneCommandState::Running) => None,
            Some(PaneCommandState::Done) | None => self
                .quiet_at()
                .filter(|at| now >= *at)
                .map(|_| ReadyBy::Quiet),
        }
    }
}

/// `--for ready`: the render stream for the fallback, and a poll of `list-panes` for the shell's
/// own word, on the 250 ms tick `--for exit` uses.
///
/// A pane whose command ends or that closes before it is ready is a miss: there is no shell left
/// to be ready.
fn wait_for_ready(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    pane: PaneId,
    quiet: Duration,
    deadline: Option<Instant>,
) -> Result<ReadyBy, WaitMiss> {
    let os_input = make_os_input();
    let updates = subscribe_to_renders(&*os_input, session_name, pane)?;
    let mut watch = ReadyWatch::new(quiet);
    let mut next_look = Instant::now();
    let answer = loop {
        if Instant::now() >= next_look {
            let look = ask(
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
            .and_then(|lines| pane_look(&lines.join("\n"), pane));
            match look {
                Err(message) => break Err(WaitMiss::Failed(message)),
                Ok((PaneState::Gone, _)) => {
                    break Err(WaitMiss::Missed(format!(
                        "{} closed before it was ready.",
                        pane
                    )))
                },
                Ok((PaneState::Exited(status), _)) => {
                    break Err(WaitMiss::Missed(format!(
                        "{} exited before it was ready (exit_status: {}).",
                        pane,
                        status.map_or_else(|| "-".to_owned(), |status| status.to_string())
                    )))
                },
                Ok((PaneState::Running, command_state)) => {
                    if let Some(ready_by) = watch.judge(command_state, Instant::now()) {
                        break Ok(ready_by);
                    }
                },
            }
            next_look = Instant::now() + EXIT_POLL_INTERVAL;
        }
        let now = Instant::now();
        if remaining(deadline, now) == Some(Duration::ZERO) {
            break Err(WaitMiss::Missed(format!(
                "{} was not ready when the wait timed out.",
                pane
            )));
        }
        // a quiet window that closes before the next look moves the look up to it, so a quiet
        // pane is answered when it falls quiet rather than up to a tick later. One that closed
        // already is judged at the next look, so it cannot spin this loop
        if let Some(quiet_at) = watch.quiet_at().filter(|at| *at > now) {
            next_look = next_look.min(quiet_at);
        }
        let wake = match deadline {
            Some(deadline) => next_look.min(deadline),
            None => next_look,
        };
        match updates.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(ServerToClientMsg::PaneRenderUpdate { viewport, .. }) => {
                watch.rendered(&viewport, Instant::now());
            },
            Ok(ServerToClientMsg::SubscribedPaneClosed { .. }) => {
                break Err(WaitMiss::Missed(format!(
                    "{} closed before it was ready.",
                    pane
                )))
            },
            Ok(ServerToClientMsg::LogError { lines }) => {
                break Err(WaitMiss::Failed(lines.join("\n")))
            },
            Ok(ServerToClientMsg::Exit { .. }) => {
                break Err(WaitMiss::Missed(
                    "The session exited before the pane was ready.".to_owned(),
                ))
            },
            Ok(_) | Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                break Err(WaitMiss::Failed(
                    "The session stopped answering while the wait was running.".to_owned(),
                ))
            },
        }
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    answer
}

/// What a `--for agent-idle` or `--for agent-waiting` look read off the pane's row.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentLook {
    /// The `OSC 7501` state the pane holds, `None` when no program in it has reported one.
    state: Option<PaneProgramStatus>,
    /// Input was typed after that state was reported, so it describes the turn before.
    stale: bool,
}

/// How a met agent wait was judged.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AgentVerdict {
    /// The agent reported this state after the last input.
    State(PaneProgramStatus),
    /// No state to read, and the pane fell quiet: a heuristic, for an agent that reports none.
    Quiet,
}

/// The decisions of an agent wait, apart from the session it asks, so they can be proven from a
/// scripted sequence of renders and looks.
///
/// The agent's own word wins. A state counts only when it was reported after the last input:
/// right after a message is typed, the pane still holds the `done` of the turn before until the
/// agent reports `working`. With no state at all the pane's silence answers for `agent-idle`,
/// and nothing answers for `agent-waiting`, which misses once the pane has been quiet that long.
#[derive(Debug)]
struct AgentWatch {
    waiting: bool,
    quiet: ReadyWatch,
}

impl AgentWatch {
    fn new(waiting: bool, quiet: Duration) -> Self {
        AgentWatch {
            waiting,
            quiet: ReadyWatch::new(quiet),
        }
    }

    fn rendered(&mut self, viewport: &[String], now: Instant) {
        self.quiet.rendered(viewport, now);
    }

    fn quiet_at(&self) -> Option<Instant> {
        self.quiet.quiet_at()
    }

    /// `Ok(Some)` met, `Ok(None)` keep waiting, `Err` a miss to report.
    fn judge(&self, look: &AgentLook, now: Instant) -> Result<Option<AgentVerdict>, String> {
        let quiet_now = self.quiet.quiet_at().map_or(false, |at| now >= at);
        match (&look.state, look.stale) {
            // the pane holds a state from before the last input: the agent has not taken the
            // input up yet. Neither the state nor the silence answers until it reports again
            (Some(_), true) => Ok(None),
            (Some(state), false) => match (state.state.as_str(), self.waiting) {
                ("working", _) => Ok(None),
                ("blocked", _) => Ok(Some(AgentVerdict::State(state.clone()))),
                (_, false) => Ok(Some(AgentVerdict::State(state.clone()))),
                (finished, true) => Err(format!(
                    "the agent ended its turn ({}) without asking anything.",
                    finished
                )),
            },
            (None, _) if !quiet_now => Ok(None),
            (None, _) if !self.waiting => Ok(Some(AgentVerdict::Quiet)),
            (None, _) => Err(
                "it reports no agent state (OSC 7501), so nothing can say it is blocked. claude \
                 and pi report one; codex reports one through the hooks `zellij setup \
                 --agent-hooks` prints; opencode does not. Wait with `--for agent-idle`, which \
                 falls back to silence, and read the pane."
                    .to_owned(),
            ),
        }
    }
}

/// The record lines a met agent wait prints after `waited_ms`.
fn agent_report(verdict: &AgentVerdict) -> Vec<String> {
    // one line per key, so a message that spans lines is folded onto one
    let one_line = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    match verdict {
        AgentVerdict::Quiet => vec!["idle_by: quiet".to_owned()],
        AgentVerdict::State(state) => {
            let mut lines = vec![
                "idle_by: state".to_owned(),
                format!("state: {}", state.state),
            ];
            if let Some(kind) = &state.kind {
                lines.push(format!("state_kind: {}", kind));
            }
            if let Some(msg) = state
                .msg
                .as_deref()
                .map(one_line)
                .filter(|msg| !msg.is_empty())
            {
                lines.push(format!("state_msg: {}", msg));
            }
            lines
        },
    }
}

/// The pane's row in a `list-panes --json` answer, read for an agent wait.
fn agent_look(panes_json: &str, pane: PaneId) -> Result<(PaneState, AgentLook), String> {
    let panes: Vec<PaneListEntry> =
        serde_json::from_str(panes_json).map_err(|e| format!("Could not read the panes: {}", e))?;
    let found = panes.into_iter().find(|entry| {
        let info = &entry.pane_info;
        match pane {
            PaneId::Terminal(id) => !info.is_plugin && info.id == id,
            PaneId::Plugin(id) => info.is_plugin && info.id == id,
        }
    });
    let none = AgentLook {
        state: None,
        stale: false,
    };
    Ok(match found {
        None => (PaneState::Gone, none),
        Some(entry) if entry.pane_info.exited => {
            (PaneState::Exited(entry.pane_info.exit_status), none)
        },
        Some(entry) => (
            PaneState::Running,
            AgentLook {
                state: entry.program_status,
                stale: entry.input_since_state,
            },
        ),
    })
}

/// `--for agent-idle` and `--for agent-waiting`: a poll of `list-panes` for the agent's own word,
/// on the 250 ms tick `--for exit` uses, and the render stream for the silence fallback.
///
/// A pane that closes, or whose command exits, before the agent stops is a miss.
fn wait_for_agent(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    session_name: &str,
    pane: PaneId,
    waiting: bool,
    quiet: Duration,
    deadline: Option<Instant>,
) -> Result<AgentVerdict, WaitMiss> {
    let os_input = make_os_input();
    let updates = subscribe_to_renders(&*os_input, session_name, pane)?;
    let mut watch = AgentWatch::new(waiting, quiet);
    let mut next_look = Instant::now();
    let what = if waiting {
        "was not blocked on a person"
    } else {
        "had not stopped working since the last input"
    };
    let answer = loop {
        if Instant::now() >= next_look {
            let look = ask(
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
            .and_then(|lines| agent_look(&lines.join("\n"), pane));
            match look {
                Err(message) => break Err(WaitMiss::Failed(message)),
                Ok((PaneState::Gone, _)) => {
                    break Err(WaitMiss::Missed(format!("{} closed.", pane)))
                },
                Ok((PaneState::Exited(status), _)) => {
                    break Err(WaitMiss::Missed(format!(
                        "{} exited (exit_status: {}).",
                        pane,
                        status.map_or_else(|| "-".to_owned(), |status| status.to_string())
                    )))
                },
                Ok((PaneState::Running, look)) => match watch.judge(&look, Instant::now()) {
                    Ok(Some(verdict)) => break Ok(verdict),
                    Ok(None) => {},
                    Err(why) => break Err(WaitMiss::Missed(format!("{}: {}", pane, why))),
                },
            }
            next_look = Instant::now() + EXIT_POLL_INTERVAL;
        }
        let now = Instant::now();
        if remaining(deadline, now) == Some(Duration::ZERO) {
            break Err(WaitMiss::Missed(format!(
                "{} {} when the wait timed out.",
                pane, what
            )));
        }
        if let Some(quiet_at) = watch.quiet_at().filter(|at| *at > now) {
            next_look = next_look.min(quiet_at);
        }
        let wake = match deadline {
            Some(deadline) => next_look.min(deadline),
            None => next_look,
        };
        match updates.recv_timeout(wake.saturating_duration_since(now)) {
            Ok(ServerToClientMsg::PaneRenderUpdate { viewport, .. }) => {
                watch.rendered(&viewport, Instant::now());
            },
            Ok(ServerToClientMsg::SubscribedPaneClosed { .. }) => {
                break Err(WaitMiss::Missed(format!("{} closed.", pane)))
            },
            Ok(ServerToClientMsg::LogError { lines }) => {
                break Err(WaitMiss::Failed(lines.join("\n")))
            },
            Ok(ServerToClientMsg::Exit { .. }) => {
                break Err(WaitMiss::Missed(
                    "The session exited during the wait.".to_owned(),
                ))
            },
            Ok(_) | Err(RecvTimeoutError::Timeout) => {},
            Err(RecvTimeoutError::Disconnected) => {
                break Err(WaitMiss::Failed(
                    "The session stopped answering while the wait was running.".to_owned(),
                ))
            },
        }
    };
    os_input.send_to_server(ClientToServerMsg::ClientExited);
    answer
}

/// Subscribe to one pane's renders, and hand them over on a channel read by a thread - so that a
/// deadline is still reachable while nothing is arriving. `recv_from_server` blocks, and a pane
/// that fell silent is exactly the case where it blocks longest.
fn subscribe_to_renders(
    os_input: &dyn ClientOsApi,
    session_name: &str,
    pane: PaneId,
) -> Result<mpsc::Receiver<ServerToClientMsg>, WaitMiss> {
    let zellij_ipc_pipe = socket_for(session_name).map_err(WaitMiss::Failed)?;
    os_input
        .connect_to_server(&zellij_ipc_pipe)
        .map_err(|e| WaitMiss::Failed(e.to_string()))?;
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
    Ok(updates)
}

/// The session's socket, which every client here opens the same way.
fn socket_for(session_name: &str) -> Result<PathBuf, String> {
    let mut sock_dir = zellij_utils::consts::ZELLIJ_SOCK_DIR.clone();
    fs::create_dir_all(&sock_dir).map_err(|e| e.to_string())?;
    zellij_utils::shared::set_permissions(&sock_dir, 0o700).map_err(|e| e.to_string())?;
    sock_dir.push(zellij_utils::sessions::session_id_for(session_name));
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
        sock_dir.push(zellij_utils::sessions::session_id_for(session_name));
        sock_dir
    };
    crate::check_ipc_pipe_length(&zellij_ipc_pipe);
    if let Err(e) = os_input.connect_to_server(&*zellij_ipc_pipe) {
        eprintln!("{}", e);
        process::exit(1);
    }

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

/// fork addition: what `zellij action send-message` was asked to do.
pub struct SendMessageRequest {
    pub to: Option<String>,
    pub reply_to: Option<String>,
    pub text: String,
    pub expire_s: u64,
    pub wait_s: Option<u64>,
    pub from: Option<String>,
    pub from_name: Option<String>,
    pub from_tab: Option<String>,
    pub json: bool,
}

/// fork addition: `zellij action send-message`.
///
/// Resolves the target against the agents of `sessions` (or, for a reply, against the session the
/// answered message came from), then hands the message to that session's server, which types it in
/// or holds it. See `zellij_utils::agent_mail`.
///
/// Exit 0 for `delivered` and `queued`. Exit 2 for `refused`, `expired`, and a target that names no
/// agent, several, or a pane without one. Exit 1 for a session that could not be asked.
pub fn start_send_message_client(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
    own_session: Option<&str>,
    sessions: &[String],
    request: SendMessageRequest,
) -> i32 {
    use zellij_utils::agent_mail::{Address, Status};
    let text = if request.text == "-" {
        let mut text = String::new();
        if let Err(e) = io::Read::read_to_string(&mut io::stdin(), &mut text) {
            eprintln!("Could not read the message from stdin: {}", e);
            return 1;
        }
        text
    } else {
        request.text.clone()
    };
    if text.trim().is_empty() {
        eprintln!("The message is empty.");
        return 2;
    }
    let (target, scope): (String, Vec<String>) = match (&request.reply_to, &request.to) {
        (_, Some(to)) => (to.clone(), sessions.to_vec()),
        (Some(id), None) => {
            let Some(own) = own_session else {
                eprintln!(
                    "--reply-to looks the message up in the session that delivered it: run it inside that session, or pass --session."
                );
                return 2;
            };
            let record = match message_records(make_os_input(), own, Some(id)) {
                Ok(mut records) if !records.is_empty() => records.remove(0),
                Ok(_) => {
                    eprintln!("No message '{}' in session '{}'.", id, own);
                    return 2;
                },
                Err(message) => {
                    eprintln!("{}", message);
                    return 2;
                },
            };
            let Some(address) = Address::parse(&record.from) else {
                eprintln!(
                    "Message {} carries no address to reply to: '{}'.",
                    id, record.from
                );
                return 2;
            };
            if !address.host.is_empty() && !is_this_host(&address.host) {
                eprintln!(
                    "Message {} came from {}, which is another machine. Reply with zellij_send reply_to={}, or on that machine: zellij --session {} action send-message --to {} -- <text>",
                    id, address.host, id, address.session, address.handle
                );
                return 2;
            }
            (address.handle, vec![address.session])
        },
        (None, None) => {
            eprintln!("Pass --to <agent> or --reply-to <message id>.");
            return 2;
        },
    };

    // every pane of every session in scope, and the agents among them
    let mut listed: Vec<(String, Vec<PaneListEntry>, ListAgentsResponse)> = Vec::new();
    for session in &scope {
        match panes_of_session(make_os_input(), session) {
            Ok(panes) => {
                let mut agents = active_agents(panes.clone());
                // the caller is never the agent a word names; see `agent_send::resolve`
                agent_detect::mark_self(
                    &mut agents,
                    std::env::var("ZELLIJ_PANE_ID")
                        .ok()
                        .and_then(|id| id.parse().ok()),
                    std::env::var("ZELLIJ_SESSION_NAME").ok().as_deref() == Some(session.as_str()),
                );
                listed.push((session.clone(), panes, agents));
            },
            Err(message) if scope.len() > 1 => eprintln!("session '{}': {}", session, message),
            Err(message) => {
                eprintln!("{}", message);
                return 1;
            },
        }
    }
    let candidates: Vec<crate::agent_send::Candidate> = listed
        .iter()
        .flat_map(|(session, panes, agents)| {
            panes
                .iter()
                .filter(|pane| !pane.pane_info.exited && !pane.pane_info.is_held)
                .map(move |pane| crate::agent_send::Candidate {
                    session: session.as_str(),
                    pane,
                    agent: agents.iter().find(|a| a.pane_id == pane.pane_info.id),
                })
        })
        .collect();
    let (session, terminal_id) = match crate::agent_send::resolve(&target, &candidates) {
        crate::agent_send::Resolved::One {
            session,
            terminal_id,
        } => (session, terminal_id),
        crate::agent_send::Resolved::NotAgent { session, handle } => {
            eprintln!(
                "{}/{} runs no coding agent, so there is nobody to message. Type into it with zellij_write_input, or zellij action paste.",
                session, handle
            );
            return 2;
        },
        crate::agent_send::Resolved::Nothing(lines) if lines.is_empty() => {
            eprintln!(
                "No agent answers to '{}': no coding agent runs in {}.",
                target,
                if scope.len() == 1 {
                    format!("session '{}'", scope[0])
                } else {
                    "any session on this machine".to_owned()
                }
            );
            return 2;
        },
        crate::agent_send::Resolved::Nothing(lines) => {
            eprintln!("No agent answers to '{}'. The agents:", target);
            lines.iter().for_each(|line| eprintln!("  {}", line));
            return 2;
        },
        crate::agent_send::Resolved::Several(lines) => {
            eprintln!(
                "'{}' names {} agents; pass a handle, or session/handle:",
                target,
                lines.len()
            );
            lines.iter().for_each(|line| eprintln!("  {}", line));
            return 2;
        },
    };

    let (from, own_name, own_tab) = match &request.from {
        Some(from) => (from.clone(), None, None),
        None => sender_address(make_os_input),
    };
    let action = Action::SendMessage {
        terminal_id,
        text,
        from,
        from_name: request.from_name.clone().or(own_name),
        from_tab: request.from_tab.clone().or(own_tab),
        reply_to: request.reply_to.clone(),
        expire_s: request.expire_s,
    };
    let mut record = match ask(make_os_input(), &session, action, "the message").and_then(|lines| {
        serde_json::from_str::<zellij_utils::agent_mail::MessageRecord>(
            lines.first().map(String::as_str).unwrap_or(""),
        )
        .map_err(|e| format!("Could not read the session's answer: {}", e))
    }) {
        Ok(record) => record,
        Err(message) => {
            eprintln!("{}", message);
            return 1;
        },
    };
    if let (Some(wait_s), Status::Queued) = (request.wait_s, record.status) {
        let deadline = Instant::now() + Duration::from_secs(wait_s);
        while record.status == Status::Queued && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
            match message_records(make_os_input(), &session, Some(&record.id)) {
                Ok(mut records) if !records.is_empty() => record = records.remove(0),
                _ => break,
            }
        }
    }
    if request.json {
        println!("{}", serde_json::to_string(&record).unwrap_or_default());
    } else {
        println!("{}", record.line());
    }
    match record.status {
        Status::Delivered | Status::Queued => 0,
        Status::Refused | Status::Expired => 2,
    }
}

/// fork addition: `zellij action message-status`.
pub fn start_message_status_client(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    id: Option<&str>,
    output_json: bool,
) -> i32 {
    match message_records(os_input, session_name, id) {
        Ok(records) => {
            if output_json {
                let json = match (id, records.first()) {
                    (Some(_), Some(record)) => serde_json::to_string(record),
                    _ => serde_json::to_string(&records),
                };
                println!("{}", json.unwrap_or_default());
            } else {
                records
                    .iter()
                    .for_each(|record| println!("{}", record.line()));
            }
            0
        },
        Err(message) => {
            eprintln!("{}", message);
            2
        },
    }
}

fn message_records(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    id: Option<&str>,
) -> Result<Vec<zellij_utils::agent_mail::MessageRecord>, String> {
    let lines = ask(
        os_input,
        session_name,
        Action::MessageStatus {
            id: id.map(str::to_owned),
        },
        id.unwrap_or("the messages"),
    )?;
    serde_json::from_str(lines.first().map(String::as_str).unwrap_or("[]"))
        .map_err(|e| format!("Could not read the session's messages: {}", e))
}

/// This pane's address, `host/session/handle`, the name of the agent in it and the name of its tab,
/// from the environment zellij gives every pane. Outside a pane, `host/-/cli`.
fn sender_address(
    make_os_input: &dyn Fn() -> Box<dyn ClientOsApi>,
) -> (String, Option<String>, Option<String>) {
    let host = zellij_utils::shared::short_hostname();
    let session = std::env::var("ZELLIJ_SESSION_NAME").ok();
    let pane_id: Option<u32> = std::env::var("ZELLIJ_PANE_ID")
        .ok()
        .and_then(|id| id.parse().ok());
    let (Some(session), Some(pane_id)) = (session, pane_id) else {
        return (format!("{}/-/cli", host), None, None);
    };
    let Ok(panes) = panes_of_session(make_os_input(), &session) else {
        return (
            format!("{}/{}/terminal_{}", host, session, pane_id),
            None,
            None,
        );
    };
    let pane = panes
        .iter()
        .find(|pane| !pane.pane_info.is_plugin && pane.pane_info.id == pane_id);
    let handle = pane
        .map(|pane| pane.pane_info.handle.clone())
        .filter(|handle| !handle.is_empty())
        .unwrap_or_else(|| format!("terminal_{}", pane_id));
    let name = pane
        .and_then(|pane| pane.agent.as_ref())
        .and_then(|agent| agent.name.clone());
    let tab = pane
        .map(|pane| pane.tab_name.clone())
        .filter(|tab| !tab.is_empty());
    (format!("{}/{}/{}", host, session, handle), name, tab)
}

/// Whether `host` names this machine: its short name, or a longer name that starts with it.
fn is_this_host(host: &str) -> bool {
    let local = zellij_utils::shared::short_hostname();
    host == local || host.split('.').next() == Some(local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::{NewPanePlacement, UnblockCondition};

    #[test]
    fn whoami_names_the_caller_and_excludes_it_from_other_agents() {
        let mut panes: Vec<PaneListEntry> = serde_json::from_str(&panes_json(&[
            (7, false, false, None),
            (8, false, false, None),
        ]))
        .unwrap();
        for (index, pane) in panes.iter_mut().enumerate() {
            pane.agent = Some(
                serde_json::from_value(serde_json::json!({
                    "kind": "claude", "name": format!("project-{}", index), "source": "command"
                }))
                .unwrap(),
            );
            pane.pane_info.handle = format!("pane-{}", index);
            pane.tab_name = "coding".into();
        }
        let report = whoami_report("scratch", "test-host", &panes[0], &panes);
        assert_eq!(report["name"], "project-0");
        assert_eq!(report["pane_id"], "terminal_7");
        assert_eq!(report["agents"].as_array().unwrap().len(), 1);
        assert_eq!(report["agents"][0]["name"], "project-1");
        let text = whoami_text(&report);
        assert!(text.ends_with("type into its pane only when it has no messaging."));
        panes[0].agent.as_mut().unwrap().name = None;
        assert!(whoami_report("scratch", "test-host", &panes[0], &panes)["name"].is_null());
    }

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
    fn a_payload_on_the_command_line_is_the_whole_pipe() {
        // the bug: a payload was sent, answered, and then the pipe read the stdin it had inherited
        // - which for a command run over ssh, or a child of a long-lived server, is a handle that
        // never reaches EOF and is not covered by `--timeout`
        assert!(
            !pipe_reads_stdin(true, true),
            "a command-line payload ends the pipe even when stdin is not a terminal"
        );
        // `echo data | zellij pipe --name n` is still the streaming pipe it always was
        assert!(
            pipe_reads_stdin(true, false),
            "a pipe with no payload of its own reads the stream it was given"
        );
        // a terminal is a person, not data
        assert!(!pipe_reads_stdin(false, false));
        assert!(!pipe_reads_stdin(false, true));
    }

    #[test]
    fn a_pipe_that_ran_out_of_time_says_which_door_it_came_through() {
        // exit 2 is shared by every kind of "nothing happened", so the sentence is what a caller
        // reads to tell a timed-out pipe from a session that is not there
        let said = pipe_timed_out("logs", Duration::from_secs(5));
        assert!(said.contains("'logs'"), "{said}");
        assert!(said.contains("5s"), "{said}");
        assert!(
            said.contains("unblocked by a plugin"),
            "the sentence says why a pipe goes unanswered: {said}"
        );
    }

    #[test]
    fn a_record_report_becomes_one_object_with_its_values_typed() {
        let report = vec![
            "tab_id: 4".to_owned(),
            "pane_id: terminal_9".to_owned(),
            "handle: sunny-otter".to_owned(),
        ];
        assert_eq!(
            json_report(&report).unwrap(),
            "{\n  \"tab_id\": 4,\n  \"pane_id\": \"terminal_9\",\n  \"handle\": \"sunny-otter\"\n}",
            "the keys are the report's own, and a tab id is a number the way list-tabs --json \
             reports one"
        );
        // a verb that reported nothing still answers an object: a parser asked for one
        assert_eq!(json_report(&[]).unwrap(), "{}");
    }

    #[test]
    fn the_values_a_report_can_carry_are_typed_the_way_the_other_json_surfaces_type_them() {
        assert_eq!(
            json_value("-"),
            serde_json::Value::Null,
            "the no-value mark"
        );
        assert_eq!(json_value("true"), serde_json::Value::Bool(true));
        assert_eq!(json_value("false"), serde_json::Value::Bool(false));
        assert_eq!(json_value("0"), serde_json::Value::from(0));
        assert_eq!(json_value("-3"), serde_json::Value::from(-3));
        // a pane id is a string in list-panes --json and stays one here, and so does a handle that
        // happens to look like nothing else
        assert_eq!(
            json_value("terminal_9"),
            serde_json::Value::String("terminal_9".to_owned())
        );
    }

    #[test]
    fn lines_that_are_not_a_record_are_not_folded_into_an_object() {
        // the guard, not a case that arises: a payload verb is refused before the call is made.
        // Folding these would silently lose the lines that carry no key
        assert_eq!(json_report(&["some screen content".to_owned()]), None);
        assert_eq!(json_report(&["Tab #1: hello".to_owned()]), None);
    }

    #[test]
    fn a_dropped_connection_is_not_an_empty_report() {
        // the session drops a `zellij action` connection for reasons that say nothing about the
        // command: client ids are reused, so the departure of the connection before this one can
        // take this one's sender with it. Reading the disconnect as "the session reported nothing"
        // is what made `new-pane` print a miss for a pane that had been made
        let (outcome, diagnostic) = outcome_of_exit(ExitReason::Disconnect);
        assert!(
            matches!(outcome, ActionOutcome::Done(1)),
            "a disconnect is a failed call, not a report with nothing in it"
        );
        assert!(
            diagnostic.is_some_and(|line| line.contains("dropped this connection")),
            "and it says so, rather than leaving the caller to read an empty report"
        );

        // the controls: the other reasons keep their meanings
        let (outcome, diagnostic) = outcome_of_exit(ExitReason::Error("boom".to_owned()));
        assert!(matches!(outcome, ActionOutcome::Done(2)));
        assert_eq!(diagnostic.as_deref(), Some("boom"));

        let (outcome, diagnostic) = outcome_of_exit(ExitReason::CustomExitStatus(7));
        assert!(matches!(outcome, ActionOutcome::Done(7)));
        assert_eq!(diagnostic, None);

        // an ordinary end of the session IS a report with nothing in it, and stays one
        let (outcome, diagnostic) = outcome_of_exit(ExitReason::Normal);
        assert!(
            matches!(&outcome, ActionOutcome::Reported(lines) if lines.is_empty()),
            "an ordinary exit still means the command reported nothing"
        );
        assert_eq!(diagnostic, None);
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
                border_style: None,
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
                tab_position: 0,
                tab_name: "tab".to_owned(),
                agent: None,
                command_state: None,
                last_command_exit_code: None,
                program_status: None,
                input_since_prompt: false,
                input_since_state: false,
                prefilled_command: None,
                queued_messages: 0,
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
    fn a_match_pattern_is_a_regex_anchored_to_the_line() {
        // a regex, not a substring: anchors and classes work against each delivered line
        let pattern = Regex::new("^round-7[0-9]-x$").unwrap();
        assert_eq!(
            first_match(&pattern, &["$ echo round", "round-77-x"]),
            Some("round-77-x")
        );
        // the negative control: the anchor holds, so the line that merely contains it misses
        assert_eq!(first_match(&pattern, &["$ echo round-77-x"]), None);
    }

    #[test]
    fn a_line_already_on_screen_never_matches_again() {
        // the baseline: the same output printed a second time is not a new line, so a wait for
        // it misses even though the regex matches it
        let pattern = Regex::new("^round-77-x$").unwrap();
        let before = vec!["round-77-x".to_owned(), "$".to_owned()];
        let after = vec![
            "round-77-x".to_owned(),
            "$ echo again".to_owned(),
            "round-77-x".to_owned(),
        ];
        assert_eq!(first_match(&pattern, &new_lines(&before, &after)), None);
        // the negative control: output that differs is new, and matches
        let other = Regex::new("^round-77-y$").unwrap();
        let fresh = vec!["round-77-x".to_owned(), "round-77-y".to_owned()];
        assert_eq!(
            first_match(&other, &new_lines(&before, &fresh)),
            Some("round-77-y")
        );
    }

    #[test]
    fn a_match_that_times_out_says_why_it_may_have_missed() {
        let pane = PaneId::Terminal(3);
        let message = timed_out_message(pane, &Watching::Match(Regex::new("^done$").unwrap()));
        assert!(message.contains("`^done$`"), "{}", message);
        assert!(message.contains("already on screen"), "{}", message);
        assert!(message.contains("identical"), "{}", message);
        // the negative control: a quiet wait has no pattern to explain
        assert_eq!(
            timed_out_message(pane, &Watching::Quiet(Duration::from_millis(500))),
            "The wait on terminal_3 timed out."
        );
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

    fn screen(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_owned()).collect()
    }

    #[test]
    fn a_ready_wait_reads_the_shells_own_word_from_the_pane_list() {
        let mut entries: Vec<PaneListEntry> = serde_json::from_str(&panes_json(&[
            (1, false, false, None),
            (2, false, false, None),
        ]))
        .unwrap();
        entries[0].command_state = Some(PaneCommandState::Prompt);
        let panes = serde_json::to_string(&entries).unwrap();
        assert_eq!(
            pane_look(&panes, PaneId::Terminal(1)).unwrap(),
            (PaneState::Running, Some(PaneCommandState::Prompt))
        );
        // a shell that writes no markers has no state, which is not the same answer as idle
        assert_eq!(
            pane_look(&panes, PaneId::Terminal(2)).unwrap(),
            (PaneState::Running, None)
        );
        assert_eq!(
            pane_look(&panes, PaneId::Terminal(9)).unwrap(),
            (PaneState::Gone, None)
        );
    }

    /// A shell without OSC 133 has its `command_state` filled from the foreground-process tracker.
    /// That `running` is not the shell's word, and a wait on a pane whose program never ends would
    /// never fall back to quiet if it were taken as one.
    #[test]
    fn a_ready_wait_ignores_a_command_state_filled_from_the_foreground_process() {
        use zellij_utils::attention::{CommandPhase, CommandState};
        let mut entries: Vec<PaneListEntry> = serde_json::from_str(&panes_json(&[
            (1, false, false, None),
            (2, false, false, None),
        ]))
        .unwrap();
        entries[0].command_state = Some(PaneCommandState::Running);
        entries[0].pane_info.command_state = CommandState {
            phase: CommandPhase::Running,
            reports_markers: false,
            ..Default::default()
        };
        // the same string from a shell that writes markers is the shell's word
        entries[1].command_state = Some(PaneCommandState::Running);
        entries[1].pane_info.command_state = CommandState {
            phase: CommandPhase::Running,
            reports_markers: true,
            ..Default::default()
        };
        let panes = serde_json::to_string(&entries).unwrap();
        assert_eq!(
            pane_look(&panes, PaneId::Terminal(1)).unwrap(),
            (PaneState::Running, None)
        );
        assert_eq!(
            pane_look(&panes, PaneId::Terminal(2)).unwrap(),
            (PaneState::Running, Some(PaneCommandState::Running))
        );
    }

    /// The race a caller that types and then waits runs into: the command is typed, and until the
    /// shell's preexec hook writes the output marker the last marker is still the prompt drawn
    /// BEFORE the typing. That prompt is not ready, and neither is the quiet after it; the prompt
    /// the shell draws after the command is.
    #[test]
    fn a_prompt_drawn_before_the_last_input_is_not_ready() {
        let mut entries: Vec<PaneListEntry> =
            serde_json::from_str(&panes_json(&[(1, false, false, None)])).unwrap();
        entries[0].command_state = Some(PaneCommandState::Prompt);
        entries[0].input_since_prompt = true;
        let typed = serde_json::to_string(&entries).unwrap();
        assert!(typed.contains("\"input_since_prompt\":true"));
        let (state, command_state) = pane_look(&typed, PaneId::Terminal(1)).unwrap();
        assert_eq!(
            (state, command_state),
            (PaneState::Running, Some(PaneCommandState::Running))
        );
        let quiet = Duration::from_millis(300);
        let mut watch = ReadyWatch::new(quiet);
        let start = Instant::now();
        watch.rendered(&screen(&["user@host ~ % ls"]), start);
        assert_eq!(
            watch.judge(command_state, start + Duration::from_secs(5)),
            None
        );

        // the command ran and the shell drew its next prompt: nothing typed since
        entries[0].input_since_prompt = false;
        let fresh = serde_json::to_string(&entries).unwrap();
        // written only when true, so a row with nothing typed reads as it always did
        assert!(!fresh.contains("input_since_prompt"));
        let (_, command_state) = pane_look(&fresh, PaneId::Terminal(1)).unwrap();
        assert_eq!(watch.judge(command_state, start), Some(ReadyBy::Prompt));
    }

    #[test]
    fn a_prompt_is_ready_at_once_and_says_so() {
        let watch = ReadyWatch::new(Duration::from_millis(300));
        let now = Instant::now();
        // no output has been seen and no window has passed: the shell's word is enough
        assert_eq!(
            watch.judge(Some(PaneCommandState::Prompt), now),
            Some(ReadyBy::Prompt)
        );
        assert_eq!(ReadyBy::Prompt.as_str(), "prompt");
        assert_eq!(ReadyBy::Quiet.as_str(), "quiet");
    }

    #[test]
    fn output_then_quiet_is_ready_for_a_shell_without_markers() {
        let quiet = Duration::from_millis(300);
        let mut watch = ReadyWatch::new(quiet);
        let start = Instant::now();
        // the blank screen a new pane starts with is not output, so no window opens
        watch.rendered(&screen(&["", "   "]), start);
        assert_eq!(watch.quiet_at(), None);
        assert_eq!(watch.judge(None, start + Duration::from_secs(5)), None);
        // the prompt is drawn
        let drawn = start + Duration::from_millis(100);
        watch.rendered(&screen(&["user@host ~ %", ""]), drawn);
        assert_eq!(watch.judge(None, drawn + quiet / 2), None);
        // more drawing moves the window on
        let redrawn = drawn + Duration::from_millis(200);
        watch.rendered(&screen(&["user@host ~ %", ""]), redrawn);
        assert_eq!(watch.judge(None, drawn + quiet), None);
        assert_eq!(watch.judge(None, redrawn + quiet), Some(ReadyBy::Quiet));
        // `done` - an end marker with no prompt after it - leaves the fallback in play
        assert_eq!(
            watch.judge(Some(PaneCommandState::Done), redrawn + quiet),
            Some(ReadyBy::Quiet)
        );
    }

    #[test]
    fn a_running_command_is_not_ready_however_quiet_it_is() {
        let quiet = Duration::from_millis(300);
        let mut watch = ReadyWatch::new(quiet);
        let start = Instant::now();
        watch.rendered(&screen(&["$ cargo build", "   Compiling zellij"]), start);
        assert_eq!(
            watch.judge(
                Some(PaneCommandState::Running),
                start + Duration::from_secs(60)
            ),
            None
        );
    }

    #[test]
    fn a_shell_whose_markers_a_clear_wiped_is_judged_by_the_fallback() {
        // a clear resets the pane's markers, so a shell that writes them reads as having none until
        // its next prompt. The wait neither latches "this shell has markers" and hangs, nor reads
        // the gap as a prompt
        let quiet = Duration::from_millis(300);
        let mut watch = ReadyWatch::new(quiet);
        let start = Instant::now();
        assert_eq!(watch.judge(Some(PaneCommandState::Running), start), None);
        let cleared = start + Duration::from_millis(50);
        watch.rendered(&screen(&["", ""]), cleared);
        assert_eq!(watch.judge(None, cleared + quiet), None);
        let redrawn = cleared + Duration::from_millis(10);
        watch.rendered(&screen(&["user@host ~ %"]), redrawn);
        assert_eq!(watch.judge(None, redrawn + quiet), Some(ReadyBy::Quiet));
        // and the prompt marker, when it comes, still wins at once
        assert_eq!(
            watch.judge(Some(PaneCommandState::Prompt), redrawn),
            Some(ReadyBy::Prompt)
        );
    }

    #[test]
    fn a_wait_for_ready_reports_which_signal_fired() {
        assert_eq!(
            wait_report(
                Duration::from_millis(412),
                Some(format!("ready_by: {}", ReadyBy::Quiet.as_str()))
            ),
            vec!["waited_ms: 412".to_owned(), "ready_by: quiet".to_owned()]
        );
        let promised = zellij_utils::cli_surface::promised_output_keys("action wait").unwrap();
        assert!(promised.split_whitespace().any(|key| key == "ready_by"));
    }

    #[test]
    fn a_creation_that_waited_for_ready_says_how_and_how_long() {
        let report = vec![
            "pane_id: terminal_4".to_owned(),
            "handle: sunny-otter".to_owned(),
        ];
        assert_eq!(
            with_ready(report.clone(), ReadyBy::Prompt, Duration::from_millis(187)),
            vec![
                "pane_id: terminal_4".to_owned(),
                "handle: sunny-otter".to_owned(),
                "ready_by: prompt".to_owned(),
                "ready_ms: 187".to_owned(),
            ]
        );
        // the pane the wait is about is still the one the report names
        assert_eq!(reported_pane(&report), Some(PaneId::Terminal(4)));
        // and every line it adds is one the surface map promises for each creating verb
        for command in ["action new-pane", "action edit", "action new-tab"] {
            let promised = zellij_utils::cli_surface::promised_output_keys(command).unwrap();
            for key in ["ready_by", "ready_ms"] {
                assert!(
                    promised.split_whitespace().any(|promised| promised == key),
                    "{} does not promise {}",
                    command,
                    key
                );
            }
        }
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

    fn status(state: &str, kind: Option<&str>) -> PaneProgramStatus {
        PaneProgramStatus {
            state: state.to_owned(),
            kind: kind.map(str::to_owned),
            app: Some("claude-code".to_owned()),
            title: None,
            msg: kind.map(|_| "Allow Bash: cargo test?\nreally".to_owned()),
            progress: None,
            at: 1,
            working: 0,
        }
    }

    fn agent(state: Option<PaneProgramStatus>, stale: bool) -> AgentLook {
        AgentLook { state, stale }
    }

    /// The trap this wait exists to avoid: a message is typed into an agent that said `done`, and
    /// the pane still holds that `done` until the agent says `working`. It is the turn before.
    #[test]
    fn a_state_reported_before_the_last_input_does_not_answer() {
        let watch = AgentWatch::new(false, Duration::from_millis(3000));
        let start = Instant::now();
        let later = start + Duration::from_secs(60);
        assert_eq!(
            watch.judge(&agent(Some(status("done", None)), true), later),
            Ok(None)
        );
        assert_eq!(
            watch.judge(&agent(Some(status("working", None)), false), later),
            Ok(None)
        );
        assert_eq!(
            watch.judge(&agent(Some(status("done", None)), false), start),
            Ok(Some(AgentVerdict::State(status("done", None))))
        );
    }

    #[test]
    fn idle_is_any_stop_and_waiting_is_only_a_block() {
        let idle = AgentWatch::new(false, Duration::from_millis(3000));
        let waiting = AgentWatch::new(true, Duration::from_millis(3000));
        let now = Instant::now();
        for state in ["idle", "done", "error"] {
            assert!(matches!(
                idle.judge(&agent(Some(status(state, None)), false), now),
                Ok(Some(AgentVerdict::State(_)))
            ));
            // an agent that finished without asking will not ask now: a miss, not a hang
            let miss = waiting
                .judge(&agent(Some(status(state, None)), false), now)
                .unwrap_err();
            assert!(miss.contains("without asking"), "{miss}");
        }
        let blocked = status("blocked", Some("permission"));
        for watch in [&idle, &waiting] {
            assert_eq!(
                watch.judge(&agent(Some(blocked.clone()), false), now),
                Ok(Some(AgentVerdict::State(blocked.clone())))
            );
        }
        assert_eq!(
            agent_report(&AgentVerdict::State(blocked)),
            vec![
                "idle_by: state",
                "state: blocked",
                "state_kind: permission",
                "state_msg: Allow Bash: cargo test? really",
            ]
        );
    }

    #[test]
    fn an_agent_with_no_state_is_judged_by_silence_and_cannot_be_waited_on_for_a_block() {
        let quiet = Duration::from_millis(3000);
        let mut idle = AgentWatch::new(false, quiet);
        let mut waiting = AgentWatch::new(true, quiet);
        let start = Instant::now();
        let none = agent(None, false);
        // nothing drawn yet: not started, so not quiet
        assert_eq!(idle.judge(&none, start + quiet * 2), Ok(None));
        idle.rendered(&screen(&["> working..."]), start);
        waiting.rendered(&screen(&["> working..."]), start);
        assert_eq!(idle.judge(&none, start + quiet / 2), Ok(None));
        assert_eq!(waiting.judge(&none, start + quiet / 2), Ok(None));
        assert_eq!(
            idle.judge(&none, start + quiet),
            Ok(Some(AgentVerdict::Quiet))
        );
        assert_eq!(agent_report(&AgentVerdict::Quiet), vec!["idle_by: quiet"]);
        let miss = waiting.judge(&none, start + quiet).unwrap_err();
        assert!(miss.contains("reports no agent state"), "{miss}");
    }

    #[test]
    fn the_agent_wait_reads_the_state_and_whether_it_is_stale_off_the_row() {
        let mut entries: Vec<PaneListEntry> = serde_json::from_str(&panes_json(&[
            (1, false, false, None),
            (2, false, true, Some(0)),
        ]))
        .unwrap();
        entries[0].program_status = Some(status("done", None));
        entries[0].input_since_state = true;
        let json = serde_json::to_string(&entries).unwrap();
        assert!(json.contains("\"input_since_state\":true"));
        assert_eq!(
            agent_look(&json, PaneId::Terminal(1)).unwrap(),
            (PaneState::Running, agent(Some(status("done", None)), true))
        );
        assert_eq!(
            agent_look(&json, PaneId::Terminal(2)).unwrap().0,
            PaneState::Exited(Some(0))
        );
        assert_eq!(
            agent_look(&json, PaneId::Terminal(9)).unwrap().0,
            PaneState::Gone
        );
        entries[0].input_since_state = false;
        // written only when true, so a row nobody typed into reads as it always did
        assert!(!serde_json::to_string(&entries)
            .unwrap()
            .contains("input_since_state"));
    }

    fn agent_row(
        pane_id: u32,
        kind: &str,
        agent_id: Option<&str>,
    ) -> zellij_utils::data::AgentListEntry {
        zellij_utils::data::AgentListEntry {
            is_self: false,
            handle: format!("pane-{}", pane_id),
            pane_id,
            tab_id: 0,
            tab_position: 0,
            tab_name: "t".to_owned(),
            title: String::new(),
            agent: agent_detect::PaneAgent {
                kind: kind.to_owned(),
                name: None,
                agent_id: agent_id.map(str::to_owned),
                source: "command".to_owned(),
                session_source: None,
            },
            command: None,
            cwd: None,
            pid: None,
            program_status: None,
            session: None,
            last_output_at: None,
            queued_messages: 0,
        }
    }

    #[test]
    fn agent_change_held_exit_needs_no_final_osc_state() {
        let mut entries: Vec<PaneListEntry> =
            serde_json::from_str(&panes_json(&[(1, false, false, None)])).unwrap();
        entries[0].agent =
            Some(agent_detect::detect_command_line("claude", &BTreeMap::new()).unwrap());
        let previous = active_agents(entries.clone());
        assert_eq!(previous.len(), 1);
        entries[0].pane_info.is_held = true;
        let current = active_agents(entries);
        assert!(current.is_empty());
        assert_eq!(
            agent_detect::agent_changes(&previous, &current)[0].event,
            "gone"
        );
    }

    #[test]
    fn agent_change_timeout_bounds_a_snapshot_that_never_answers() {
        let (_sender, receiver) = std::sync::mpsc::channel::<()>();
        assert!(agent_snapshot_answer(receiver, Some(Instant::now()))
            .unwrap_err()
            .contains("timed out"));
        let (sender, receiver) = std::sync::mpsc::channel();
        sender.send(7).unwrap();
        assert_eq!(agent_snapshot_answer(receiver, None), Ok(7));
    }

    #[test]
    fn agent_change_reply_uses_the_changed_session_and_only_a_finished_turn() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join(".claude/projects/-src");
        std::fs::create_dir_all(&project).unwrap();
        for (id, reply) in [("s1", "wrong"), ("s2", "hello\nsecond line")] {
            let text = serde_json::json!({"type": "assistant", "message": {"content": [{"type": "text", "text": reply}]}});
            std::fs::write(project.join(format!("{id}.jsonl")), text.to_string()).unwrap();
        }
        let mut first = agent_row(1, "claude", Some("s1"));
        first.session = Some("first".into());
        let mut old = agent_row(1, "claude", Some("s2"));
        old.session = Some("second".into());
        old.program_status = Some(status("working", None));
        let mut new = old.clone();
        new.program_status = Some(status("done", None));
        let agents = vec![first, new.clone()];
        let mut change = agent_detect::agent_changes(&[old], &[new.clone()]).remove(0);
        enrich_agent_change_from(&mut change, &agents, &|_| None, Some(dir.path()));
        assert_eq!(change.last_reply.as_deref(), Some("hello"));
        let mut renamed = new.clone();
        renamed.agent.name = Some("child".into());
        let mut change = agent_detect::agent_changes(&[new], &[renamed]).remove(0);
        enrich_agent_change_from(&mut change, &agents, &|_| None, Some(dir.path()));
        assert_eq!(change.last_reply, None);
    }

    #[test]
    fn a_last_reply_is_read_from_the_transcript_of_the_panes_own_session() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let project = home.join(".claude/projects/-src");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("s1.jsonl"),
            "{\"type\":\"user\",\"message\":{\"content\":\"hi\"}}\n\
             {\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":\"hello\"}]}}\n",
        )
        .unwrap();
        let none = |_: &str| None;
        let agents = vec![
            agent_row(1, "claude", Some("s1")),
            agent_row(2, "opencode", Some("s2")),
            agent_row(3, "claude", None),
            agent_row(4, "claude", Some("gone")),
        ];
        let found = last_reply_of(&agents, PaneId::Terminal(1), &none, Some(home)).unwrap();
        assert_eq!(
            (found.reply.as_str(), found.handle.as_str()),
            ("hello", "pane-1")
        );
        assert_eq!(found.transcript, project.join("s1.jsonl"));
        let why =
            |pane| last_reply_of(&agents, PaneId::Terminal(pane), &none, Some(home)).unwrap_err();
        assert!(why(2).contains("opencode keeps its sessions"), "{}", why(2));
        assert!(why(3).contains("no claude session id"), "{}", why(3));
        assert!(why(4).contains("no transcript"), "{}", why(4));
        assert!(why(9).contains("not running a coding agent"), "{}", why(9));
    }

    #[test]
    fn list_agents_rows_carry_the_message_address_from_the_transcript() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let project = home.join(".claude/projects/-src");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("s1.jsonl"),
            "{\"type\":\"user\",\"timestamp\":\"2026-11-19T10:00:00Z\",\"message\":{\"content\":\"hi\"}}\n\
             {\"type\":\"bridge-session\",\"bridgeSessionId\":\"cse_01Abc\"}\n\
             {\"type\":\"assistant\",\"message\":{\"usage\":{\"input_tokens\":5,\"cache_read_input_tokens\":10}}}\n",
        )
        .unwrap();
        let mut agents = vec![
            agent_row(1, "claude", Some("s1")),
            agent_row(2, "codex", Some("s2")),
            agent_row(3, "claude", Some("gone")),
        ];
        agents[1].queued_messages = 2;
        let none = |_: &str| None;
        let rows = agent_rows(&agents, &none, Some(home), Duration::from_secs(10));
        let json: serde_json::Value = serde_json::to_value(&rows).unwrap();
        assert_eq!(json[0]["message_to"], "bridge:session_01Abc");
        assert_eq!(json[0]["message_route"], "harness");
        assert_eq!(json[0]["chat_started_at"], 1_795_082_400_000u64);
        // no context size: every harness's window is large enough that the number misleads
        assert!(json[0].get("context_tokens").is_none());
        // the agent's own keys are still there, unchanged
        assert_eq!(json[0]["handle"], "pane-1");
        assert_eq!(json[0]["kind"], "claude");
        // held messages show as a count, only where there are some
        assert_eq!(json[1]["queued"], 2);
        assert!(json[0].get("queued").is_none());
        for row in [&json[1], &json[2]] {
            assert_eq!(row["message_route"], "pane");
            assert!(row.get("message_to").is_none());
        }
        // no time for the transcripts: every row still goes out, without the chat keys
        let rows = agent_rows(&agents, &none, Some(home), Duration::ZERO);
        let json: serde_json::Value = serde_json::to_value(&rows).unwrap();
        assert_eq!(json.as_array().map(Vec::len), Some(3));
        assert_eq!(json[0]["handle"], "pane-1");
    }
}

/// Attach the caller marker to JSON pane rows without changing the wire contract.
fn pane_list_with_self(
    lines: Vec<String>,
    pane_id: Option<u32>,
    same_session: bool,
) -> Vec<String> {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&lines.join("\n")) else {
        return lines;
    };
    let Some(rows) = value.as_array_mut() else {
        return lines;
    };
    for row in rows {
        let is_self = same_session
            && pane_id.is_some()
            && row.get("is_plugin").and_then(serde_json::Value::as_bool) != Some(true)
            && pane_id.map(u64::from) == row.get("id").and_then(|id| id.as_u64());
        if let Some(row) = row.as_object_mut() {
            row.insert("self".into(), serde_json::Value::Bool(is_self));
        }
    }
    serde_json::to_string_pretty(&value)
        .map(|value| vec![value])
        .unwrap_or(lines)
}

#[cfg(test)]
mod pane_self_tests {
    use super::*;
    #[test]
    fn json_panes_mark_only_the_callers_session_and_pane() {
        let json = r#"[{"id":7},{"id":8},{"id":7,"is_plugin":true}]"#;
        let rows: serde_json::Value =
            serde_json::from_str(&pane_list_with_self(vec![json.into()], Some(7), true).join("\n"))
                .unwrap();
        assert_eq!(rows[0]["self"], true);
        assert_eq!(rows[1]["self"], false);
        assert_eq!(rows[2]["self"], false);
        let rows: serde_json::Value = serde_json::from_str(
            &pane_list_with_self(vec![json.into()], Some(7), false).join("\n"),
        )
        .unwrap();
        assert_eq!(rows[0]["self"], false);
    }
}
