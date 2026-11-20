//! The `[cli_client]` is used to attach to a running server session
//! and dispatch actions, that are specified through the command line.
use std::collections::{BTreeMap, HashSet, VecDeque};
use std::io::{self, BufRead, Read, Write};
use std::process;
use std::str::FromStr;
use std::{fs, path::PathBuf};

use crate::os_input_output::ClientOsApi;
use uuid::Uuid;
use zellij_utils::{
    cli::{PromptCli, PromptElementCli, SubscribeCli, SubscribeFormat},
    data::{OutsideClick, PaneId, PipePopupPlacement},
    errors::prelude::*,
    input::actions::Action,
    ipc::{ClientToServerMsg, ExitReason, IpcReceiveError, ServerToClientMsg},
    prompt::{self, ChoiceItem, PromptElement},
};

pub fn start_cli_client(
    mut os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    actions: Vec<Action>,
) -> i32 {
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
    let pane_id = os_input
        .env_variable("ZELLIJ_PANE_ID")
        .and_then(|e| e.trim().parse().ok());
/// `--wait-ready` on a verb that makes a pane: how long to wait for the pane's shell, and how to
/// open the connections the wait asks the session on.
pub struct WaitReady<'a> {
    pub timeout: Duration,
    pub make_os_input: &'a dyn Fn() -> Box<dyn ClientOsApi>,
}

/// The quiet window `--wait-ready` falls back on, for a shell that writes no OSC 133 prompt
/// marker. The same default `wait --for ready` takes.
const READY_QUIET: Duration = Duration::from_millis(300);


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

fn wait_for_pipe_release(os_input: &mut Box<dyn ClientOsApi>, pipe_id: &str) -> PipeWaitOutcome {
    loop {
        match os_input.recv_from_server() {
            Some((message, _)) => {
                if let Some(outcome) = handle_pipe_server_message(os_input, pipe_id, message) {
                    return outcome;
                }
            },
            None => return PipeWaitOutcome::Exit(2),
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
    loop {
        match receiver.recv() {
            Ok(PipeEvent::Payload(payload)) => {
                if payload.is_none() {
                    input_ended = true;
                }
                queued.push_back(payload);
            },
            Ok(PipeEvent::Server(message)) => {
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
            Ok(PipeEvent::ServerGone) | Err(_) => return 2,
        }
        if !waiting_for_release {
            if let Some(payload) = next_payload(&mut queued, batch) {
                os_input.send_to_server(request.message(payload));
                waiting_for_release = true;
            }
        }
    }
}

fn drive_pipe(
    os_input: &mut Box<dyn ClientOsApi>,
    request: &PipeRequest,
    mut payloads: impl Iterator<Item = Option<String>>,
) -> i32 {
    let mut next_payload = payloads.next();
    while let Some(payload) = next_payload {
        os_input.send_to_server(request.message(payload));
        match wait_for_pipe_release(os_input, &request.pipe_id) {
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
) -> i32 {
    let request = request.prepare();
    let is_piped = !os_input.stdin_is_terminal();
    if !is_piped {
        return drive_pipe(os_input, &request, std::iter::once(payload));
    }
    drive_streaming_pipe(os_input, &request, payload, b'\n', false)
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
        PromptInput::Single(payload) => {
            drive_pipe(&mut os_input, &request, std::iter::once(payload))
        },
        PromptInput::StreamStdin { null } => drive_streaming_pipe(
            &mut os_input,
            &request,
            None,
            if null { 0 } else { b'\n' },
            true,
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

fn individual_messages_client(
    os_input: &mut Box<dyn ClientOsApi>,
    action: Action,
    pane_id: Option<u32>,
) -> Option<i32> {
    let is_blocking = matches!(
        &action,
        Action::NewBlockingPane {
            unblock_condition: Some(_),
            ..
        }
    );
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
                return None;
            },
            Ok((ServerToClientMsg::Log { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| println!("{line}"));
                return None;
            },
            Ok((ServerToClientMsg::LogError { lines: log_lines }, _)) => {
                log_lines.iter().for_each(|line| eprintln!("{line}"));
                return Some(2);
            },
            Ok((ServerToClientMsg::Exit { exit_reason }, _)) => match exit_reason {
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
            Err(true) => {
                eprintln!("The session closed the connection before the action finished.");
                return Some(2);
            },
            _ => {},
        }
    }
}

pub fn start_subscribe_client(
    os_input: Box<dyn ClientOsApi>,
    session_name: &str,
    subscribe_cli: SubscribeCli,
) {
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
/// The pane a creating command's report says it made.
/// A creation report with `--wait-ready`'s two lines added: which signal said the pane was ready,
/// and how long after it was made.
fn with_ready(mut lines: Vec<String>, ready_by: ReadyBy, waited: Duration) -> Vec<String> {
    lines.push(format!("ready_by: {}", ready_by.as_str()));
    lines.push(format!("ready_ms: {}", waited.as_millis()));
    lines
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
                    if let Some(ref scrollback_lines) = scrollback {
                        for line in scrollback_lines {
                            let _ = writeln!(stdout, "{}", line);
                        }
                    }
                    for line in &viewport {
                        let _ = writeln!(stdout, "{}", line);
                    }
                    let _ = stdout.flush();
                },
                SubscribeFormat::Json => {
                    let json = serde_json::json!({
                        "event": "pane_update",
                        "pane_id": pane_id.to_string(),
                        "viewport": viewport,
                        "scrollback": scrollback,
                        "is_initial": is_initial,
                    });
                    let _ = writeln!(stdout, "{}", json);
                    let _ = stdout.flush();
                },
            },
            Some((ServerToClientMsg::SubscribedPaneClosed { pane_id }, _)) => {
                remaining_panes.remove(&pane_id);
                match subscribe_cli.format {
                    SubscribeFormat::Raw => {},
                    SubscribeFormat::Json => {
                        let json = serde_json::json!({
                            "event": "pane_closed",
                            "pane_id": pane_id.to_string(),
                        });
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
