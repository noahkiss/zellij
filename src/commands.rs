use dialoguer::Confirm;
use std::net::IpAddr;
use std::{path::PathBuf, process, time::Duration};

#[cfg(feature = "web_server_capability")]
use isahc::{config::RedirectPolicy, prelude::*, HttpClient, Request};

use zellij_client::session_resolution::{self, Resolved};
use zellij_client::{
    os_input_output::get_client_os_input, start_client as start_client_impl, ClientInfo,
};

use zellij_utils::sessions::{
    assert_dead_session, assert_session, assert_session_ne, delete_session as delete_session_impl,
    generate_unique_session_name, get_active_session, get_resurrectable_sessions, get_sessions,
    kill_session as kill_session_impl, ActiveSession, KillWait,
};

use zellij_utils::session_snapshot::{
    archive_session_info, list_snapshots, prune_all, remove_snapshot, resolve_snapshot,
    ArchiveOutcome, Snapshot, SnapshotReason, SnapshotSettings,
};

#[cfg(feature = "web_server_capability")]
use zellij_client::web_client::start_web_client as start_web_client_impl;

#[cfg(feature = "web_server_capability")]
use zellij_utils::web_server_commands::shutdown_all_webserver_instances;

#[cfg(feature = "web_server_capability")]
use zellij_utils::web_authentication_tokens::{
    create_token, list_tokens, revoke_all_tokens, revoke_token,
};

use miette::{Report, Result};
use zellij_server::{os_input_output::get_server_os_input, start_server as start_server_impl};
use zellij_utils::{
    cli::{destroys, AttachArgs, CliArgs, Command, Sessions, SnapshotCli},
    data::{ConnectToSession, PaneId, PaneTarget},
    envs,
    input::{
        actions::{initial_panes_from_cli, Action},
        config::{Config, ConfigError},
        options::Options,
    },
    setup::Setup,
};

pub(crate) use zellij_utils::sessions::list_sessions;

/// Asks before something that cannot be undone, or refuses when nothing can answer.
///
/// One implementation for every such verb, so the wording and the behaviour on a pipe are the same
/// wherever you meet them. `what` completes the sentence "`<verb>` <what>, and cannot be undone".
///
/// A refusal and a declined prompt both exit 2: each is a well-formed request that changed
/// nothing, which is what a 2 means - the same code the targetless guard and a miss use.
pub(crate) fn confirm_or_exit(verb: &str, what: &str, yes: bool) {
    use std::io::IsTerminal;
    match zellij_utils::cli::confirmation_for(verb, what, yes, std::io::stdin().is_terminal()) {
        zellij_utils::cli::Confirmation::Proceed => {},
        zellij_utils::cli::Confirmation::Refuse(message) => {
            eprintln!("{}", message);
            process::exit(2);
        },
        zellij_utils::cli::Confirmation::Ask(prompt) => {
            // a prompt that cannot be read is a no, not a panic: the default is no either way
            let answered_yes = Confirm::new()
                .with_prompt(prompt)
                .default(false)
                .interact()
                .unwrap_or(false);
            if !answered_yes {
                eprintln!("Abort.");
                process::exit(2);
            }
        },
    }
}

pub(crate) fn kill_all_sessions(yes: bool, wait: KillWait) {
    match get_sessions() {
        Ok(sessions) if sessions.is_empty() => {
            eprintln!("No active zellij sessions found.");
            process::exit(1);
        },
        Ok(sessions) => {
            confirm_or_exit("kill-all-sessions", destroys::KILL_ALL_SESSIONS, yes);
            // every session is attempted before the exit code is decided: a wedged server should
            // not stop the rest of them from being killed
            let mut all_gone = true;
            for session in &sessions {
                all_gone &= kill_session_impl(&session.0, wait);
            }
            process::exit(if all_gone { 0 } else { 1 });
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            process::exit(1);
        },
    }
}

pub(crate) fn snapshot_settings(opts: &CliArgs) -> SnapshotSettings {
    SnapshotSettings::from_options(get_config_options_from_cli_args(opts).ok().as_ref())
}

fn resolve_snapshot_or_exit(
    settings: &SnapshotSettings,
    id: &str,
    session_name: Option<&str>,
) -> Snapshot {
    match resolve_snapshot(settings, id, session_name) {
        Ok(snapshot) => snapshot,
        Err(e) => {
            eprintln!("{}", e);
            process::exit(2);
        },
    }
}

pub(crate) fn snapshot_command(snapshot_cli: SnapshotCli, opts: CliArgs) {
    let settings = snapshot_settings(&opts);
    match snapshot_cli {
        SnapshotCli::List { session, json } => {
            list_snapshots_command(&settings, session.as_deref(), json);
        },
        SnapshotCli::Show { id } => {
            let snapshot = resolve_snapshot_or_exit(&settings, &id, None);
            match std::fs::read_to_string(snapshot.layout_file()) {
                Ok(raw) => print!("{}", raw),
                Err(e) => {
                    eprintln!("Failed to read {}: {}", snapshot.layout_file().display(), e);
                    process::exit(2);
                },
            }
        },
        SnapshotCli::Restore { id, session } => {
            let snapshot = resolve_snapshot_or_exit(&settings, &id, None);
            let target_session_name = session.unwrap_or_else(|| snapshot.session_name.clone());
            // a restore is an attach that takes its layout from the archive, so it goes through the
            // same path rather than a parallel one
            let mut opts = opts;
            opts.session = None;
            opts.command = Some(Command::Sessions(Sessions::Attach {
                args: AttachArgs {
                    session_name: Some(target_session_name),
                    create: true,
                    no_resurrect: false,
                    restore: Some(snapshot.id),
                    ..AttachArgs::default()
                },
                create_background: false,
            }));
            start_client(opts);
        },
        SnapshotCli::Rm { id, yes } => {
            let snapshot = resolve_snapshot_or_exit(&settings, &id, None);
            confirm_or_exit("snapshot rm", destroys::SNAPSHOT_RM, yes);
            match remove_snapshot(&snapshot) {
                Ok(()) => println!("Deleted snapshot {}", snapshot.id),
                Err(e) => {
                    eprintln!("Failed to delete snapshot {}: {}", snapshot.id, e);
                    process::exit(2);
                },
            }
        },
        SnapshotCli::Prune { keep, yes } => {
            let keep = keep.unwrap_or(settings.limit);
            confirm_or_exit("snapshot prune", destroys::SNAPSHOT_PRUNE, yes);
            let removed = prune_all(&settings, keep);
            println!(
                "Pruned {} snapshot(s), keeping {} per session.",
                removed.len(),
                keep
            );
        },
    }
    process::exit(0);
}

fn list_snapshots_command(settings: &SnapshotSettings, session: Option<&str>, json: bool) {
    let snapshots = list_snapshots(settings, session);
    if json {
        let entries: Vec<String> = snapshots
            .iter()
            .map(|snapshot| {
                // trial-parse rather than trust: a layout a newer binary rejects is still a text
                // file a human can repair, and finding that out at list time beats finding it out
                // at restore time
                let layout_error = snapshot.layout().err();
                format!(
                    r#"{{"id":{:?},"session_name":{:?},"saved_at":{},"zellij_version":{:?},"contract_version":{},"reason":{:?},"tabs":{},"panes":{},"path":{:?},"layout_error":{}}}"#,
                    snapshot.id,
                    snapshot.session_name,
                    snapshot.meta.saved_at,
                    snapshot.meta.zellij_version,
                    snapshot.meta.contract_version,
                    snapshot.meta.reason.as_str(),
                    snapshot.meta.tabs,
                    snapshot.meta.panes,
                    snapshot.path.display().to_string(),
                    match layout_error {
                        Some(e) => format!("{:?}", e),
                        None => "null".to_owned(),
                    }
                )
            })
            .collect();
        println!("[{}]", entries.join(","));
        return;
    }
    if snapshots.is_empty() {
        eprintln!("No snapshots in {}.", settings.dir.display());
        return;
    }
    println!(
        "{:<24}  {:<20}  {:<14}  {:<9}  {:>4}  {:>5}",
        "ID", "SESSION", "SAVED", "REASON", "TABS", "PANES"
    );
    for snapshot in snapshots {
        let unparseable = if snapshot.layout().is_err() {
            "  (layout does not parse)"
        } else {
            ""
        };
        println!(
            "{:<24}  {:<20}  {:<14}  {:<9}  {:>4}  {:>5}{}",
            snapshot.id,
            snapshot.session_name,
            snapshot.saved_at_description(),
            snapshot.meta.reason.as_str(),
            snapshot.meta.tabs,
            snapshot.meta.panes,
            unparseable
        );
    }
}

pub(crate) fn delete_all_sessions(yes: bool, force: bool, wait: KillWait, opts: &CliArgs) {
    use std::collections::{BTreeMap, BTreeSet};
    use zellij_server::background_jobs::scan_session_list_default_dirs;

    let active_sessions: Vec<String> = get_sessions()
        .unwrap_or_default()
        .iter()
        .map(|s| s.0.clone())
        .collect();
    let (_live_sessions, resurrectable_map) =
        scan_session_list_default_dirs(&String::new(), &[], &BTreeMap::new());
    let mut sessions_to_delete: BTreeSet<String> = resurrectable_map.into_keys().collect();
    for (name, _elapsed) in get_resurrectable_sessions() {
        sessions_to_delete.insert(name);
    }
    // a live session only becomes resurrectable once it has serialized itself, so scanning for
    // resurrectable folders does not find a young one. `--force` is what makes live sessions
    // targets, so take them from the session list directly rather than hoping they show up there.
    let live_sessions_kept: Vec<String> = if force {
        sessions_to_delete.extend(active_sessions.iter().cloned());
        Vec::new()
    } else {
        sessions_to_delete.retain(|name| !active_sessions.contains(name));
        active_sessions.clone()
    };
    confirm_or_exit("delete-all-sessions", destroys::DELETE_ALL_SESSIONS, yes);
    // every session is attempted before the exit code is decided: one wedged server should not
    // stop the rest of them from being deleted
    let mut all_gone = true;
    for session in &sessions_to_delete {
        all_gone &= delete_session_impl(session, force, &snapshot_settings(opts), wait);
    }
    for session in &live_sessions_kept {
        eprintln!(
            "Session: {:?} is still running and was not deleted. Use --force to kill it first.",
            session
        );
    }
    process::exit(if all_gone && live_sessions_kept.is_empty() {
        0
    } else {
        1
    });
}

pub(crate) fn kill_session(target_session: &Option<String>, wait: KillWait, yes: bool) {
    match target_session {
        Some(target_session) => {
            assert_session(target_session);
            confirm_or_exit("kill-session", destroys::KILL_SESSION, yes);
            let gone = kill_session_impl(target_session, wait);
            process::exit(if gone { 0 } else { 1 });
        },
        None => {
            println!("Please specify the session name to kill.");
            process::exit(1);
        },
    }
}

pub(crate) fn delete_session(
    target_session: &Option<String>,
    force: bool,
    wait: KillWait,
    opts: &CliArgs,
    yes: bool,
) {
    match target_session {
        Some(target_session) => {
            assert_dead_session(target_session, force);
            confirm_or_exit("delete-session", destroys::DELETE_SESSION, yes);
            let gone = delete_session_impl(target_session, force, &snapshot_settings(opts), wait);
            process::exit(if gone { 0 } else { 1 });
        },
        None => {
            println!("Please specify the session name to delete.");
            process::exit(1);
        },
    }
}

fn get_os_input<OsInputOutput>(
    fn_get_os_input: fn() -> Result<OsInputOutput, std::io::Error>,
) -> OsInputOutput {
    match fn_get_os_input() {
        Ok(os_input) => os_input,
        Err(e) => {
            eprintln!("failed to open terminal:\n{}", e);
            process::exit(1);
        },
    }
}

pub(crate) fn start_server(path: PathBuf, debug: bool) {
    // Set instance-wide debug mode
    zellij_utils::consts::DEBUG_MODE.set(debug).unwrap();
    register_app_starter();
    let os_input = get_os_input(get_server_os_input);
    start_server_impl(Box::new(os_input), path);
}

#[cfg(feature = "terminal")]
fn register_app_starter() {
    zellij_server::set_app_starter(Box::new(start_terminal_app));
}

#[cfg(not(feature = "terminal"))]
fn register_app_starter() {}

#[cfg(feature = "terminal")]
fn start_terminal_app(
    request: &zellij_utils::app_control::OpenWindowRequest,
) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let program = std::env::current_exe()
        .map_err(|e| format!("Zellij could not find its own program file: {}", e))?;
    let mut command = Command::new(program);
    command.arg("terminal");
    if let Some(session) = &request.session {
        command.arg(session);
        if request.create {
            command.arg("--create");
        }
    }
    if let Some(cwd) = &request.cwd {
        command.arg("--working-directory").arg(cwd);
    }
    command
        .env_remove(zellij_utils::envs::ZELLIJ_ENV_KEY)
        .env_remove(zellij_utils::envs::SESSION_NAME_ENV_KEY)
        .env_remove("ZELLIJ_PANE_ID")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command
        .spawn()
        .map_err(|e| format!("The desktop app could not be started: {}", e))?;
    let _ = std::thread::Builder::new()
        .name("desktop_app_child".to_owned())
        .spawn(move || {
            let _ = child.wait();
        });
    Ok(())
}

#[cfg(feature = "terminal")]
pub(crate) fn start_terminal(args: zellij_utils::cli::TerminalArgs, opts: CliArgs) {
    if let Err(e) = zellij_terminal::run(args, opts) {
        eprintln!("{:?}", e);
        process::exit(1);
    }
}

#[cfg(not(feature = "terminal"))]
pub(crate) fn start_terminal(_args: zellij_utils::cli::TerminalArgs, _opts: CliArgs) {
    eprintln!("This Zellij was built without the window feature.");
    process::exit(1);
}

pub(crate) fn run_setup_actions(setup: &zellij_utils::setup::Setup, opts: &CliArgs) {
    use zellij_utils::desktop_integration::{plan_setup, platform};
    let mut failed = false;
    if let Some(preset) = &setup.keybinding_preset {
        match set_keybinding_preset(opts, preset) {
            Ok(message) => println!("✓ {}", message),
            Err(e) => {
                eprintln!("✗ {}", e);
                failed = true;
            },
        }
    }
    let actions = setup.desktop_actions();
    if actions != zellij_utils::desktop_integration::SetupActions::default() {
        let plan = plan_setup(&platform::status(), actions);
        for note in &plan.notes {
            println!("- {}", note);
        }
        for refused in &plan.refused {
            eprintln!("✗ {}", refused);
            failed = true;
        }
        if !plan.install.is_empty() || !plan.remove.is_empty() {
            for outcome in platform::apply(&plan.install, &plan.remove, setup.replace_existing) {
                let verb = if outcome.install { "" } else { "Removed: " };
                let mark = if outcome.succeeded { "✓" } else { "✗" };
                let line = format!("{} {}{}", mark, verb, outcome.item.label());
                if outcome.succeeded {
                    println!("{}", line);
                } else {
                    eprintln!("{}", line);
                    failed = true;
                }
                for message_line in outcome.message.lines() {
                    if outcome.succeeded {
                        println!("  {}", message_line);
                    } else {
                        eprintln!("  {}", message_line);
                    }
                }
                if !outcome.succeeded && outcome.message.contains("Confirm to replace it.") {
                    eprintln!("  Run again with --replace-existing to replace it.");
                }
            }
        }
        #[cfg(windows)]
        if actions.uninstall_desktop_entry {
            for line in platform::forget_app() {
                println!("  {}", line);
            }
        }
    }
    if failed {
        process::exit(1);
    }
}

fn set_keybinding_preset(opts: &CliArgs, preset: &str) -> Result<String, String> {
    let config_file = Config::config_file_path(opts)
        .ok_or_else(|| "There is no config folder to save the keybinding preset in".to_owned())?;
    let keybinds_dir = config_file.parent().map(|dir| dir.join("keybinds"));
    zellij_utils::input::keybind_presets::find_keybind_preset(
        preset,
        keybinds_dir.as_deref(),
        config_file.parent(),
    )?;
    if !config_file.exists() {
        let defaults = Config::try_from(opts).map_err(|e| e.to_string())?;
        Config::write_config_to_disk_if_it_does_not_exist(
            defaults.to_string(true),
            &Some(config_file.clone()),
        );
    }
    zellij_utils::input::config_file_edit::set_keybinding_preset_in(&config_file, preset)?;
    Ok(format!(
        "Keybinding preset \"{}\" saved to {}",
        preset,
        config_file.display()
    ))
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn start_web_server(
    opts: CliArgs,
    run_daemonized: bool,
    ip: Option<IpAddr>,
    port: Option<u16>,
    cert: Option<PathBuf>,
    key: Option<PathBuf>,
    startup_timeout: Option<u64>,
) {
    // TODO: move this outside of this function
    let (config, _layout, config_options, _config_without_layout, _config_options_without_layout) =
        match Setup::from_cli_args(&opts) {
            Ok(results) => results,
            Err(e) => {
                if let ConfigError::KdlError(error) = e {
                    let report: Report = error.into();
                    eprintln!("{:?}", report);
                } else {
                    eprintln!("{}", e);
                }
                process::exit(1);
            },
        };
    start_web_client_impl(
        config,
        config_options,
        opts.config,
        run_daemonized,
        ip,
        port,
        cert,
        key,
        startup_timeout,
    );
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn start_web_server(
    _opts: CliArgs,
    _run_daemonized: bool,
    _ip: Option<IpAddr>,
    _port: Option<u16>,
    _cert: Option<PathBuf>,
    _key: Option<PathBuf>,
    _startup_timeout: Option<u64>,
) {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot run web server!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot run web server!"
    );
    std::process::exit(2);
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn stop_web_server() -> Result<(), String> {
    shutdown_all_webserver_instances().map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn stop_web_server() -> Result<(), String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot stop web server!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot stop web server!"
    );
    std::process::exit(2);
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn create_auth_token(name: Option<String>, read_only: bool) -> Result<String, String> {
    // returns the token and it's name
    create_token(name, read_only)
        .map(|(token, token_name)| {
            let access_type = if read_only { " (read-only)" } else { "" };
            format!("{}: {}{}", token_name, token, access_type)
        })
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn create_auth_token(_name: Option<String>, _read_only: bool) -> Result<String, String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot create auth token!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot create auth token!"
    );
    std::process::exit(2);
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn revoke_auth_token(token_name: &str) -> Result<bool, String> {
    revoke_token(token_name).map_err(|e| e.to_string())
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn revoke_auth_token(_token_name: &str) -> Result<bool, String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot revoke auth token!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot revoke auth token!"
    );
    std::process::exit(2);
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn revoke_all_auth_tokens() -> Result<usize, String> {
    // returns the revoked count
    revoke_all_tokens().map_err(|e| e.to_string())
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn revoke_all_auth_tokens() -> Result<usize, String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot revoke all tokens!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot revoke all tokens!"
    );
    std::process::exit(2);
}

#[cfg(feature = "web_server_capability")]
pub(crate) fn list_auth_tokens() -> Result<Vec<String>, String> {
    // returns the token list line by line
    list_tokens()
        .map(|tokens| {
            let mut res = vec![];
            for t in tokens {
                let access_type = if t.read_only { " [READ-ONLY]" } else { "" };
                res.push(format!(
                    "{}: created at {}{}",
                    t.name, t.created_at, access_type
                ))
            }
            res
        })
        .map_err(|e| e.to_string())
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn list_auth_tokens() -> Result<Vec<String>, String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot list tokens!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot list tokens!"
    );
    std::process::exit(2);
}

/// Default timeout for web server status check (in seconds)
#[cfg(feature = "web_server_capability")]
pub const DEFAULT_WEB_SERVER_STATUS_TIMEOUT_SECS: u64 = 30;

#[cfg(feature = "web_server_capability")]
pub(crate) fn web_server_status(
    web_server_base_url: &str,
    timeout_secs: Option<u64>,
) -> Result<String, String> {
    let timeout =
        Duration::from_secs(timeout_secs.unwrap_or(DEFAULT_WEB_SERVER_STATUS_TIMEOUT_SECS));
    let http_client = HttpClient::builder()
        .timeout(timeout)
        .redirect_policy(RedirectPolicy::Follow)
        .build()
        .map_err(|e| e.to_string())?;
    let request = Request::get(format!("{}/info/version", web_server_base_url,));
    let req = request.body(()).map_err(|e| e.to_string())?;
    let mut res = http_client.send(req).map_err(|e| e.to_string())?;
    let status_code = res.status();
    if status_code == 200 {
        let body = res.bytes().map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&body).to_string())
    } else {
        Err(format!(
            "Failed to stop web server, got status code: {}",
            status_code
        ))
    }
}

#[cfg(not(feature = "web_server_capability"))]
pub(crate) fn web_server_status(
    _web_server_base_url: &str,
    _timeout_secs: Option<u64>,
) -> Result<String, String> {
    log::error!(
        "This version of Zellij was compiled without web server support, cannot get web server status!"
    );
    eprintln!(
        "This version of Zellij was compiled without web server support, cannot get web server status!"
    );
    std::process::exit(2);
}

pub(crate) fn toggle_tray_icon_at(socket: &std::path::Path) -> Result<(), String> {
    match zellij_utils::app_control::send_app_request_to(
        socket,
        &zellij_utils::app_control::AppRequest::ToggleTrayIcon,
    ) {
        Ok(zellij_utils::app_control::AppReply::Refused(reason)) => Err(reason),
        Ok(_) => Ok(()),
        Err(_) => Err("The Zellij desktop app is not running.".to_owned()),
    }
}

/// Client entrypoint for all [`zellij_utils::cli::CliAction`]
///
/// Checks session to send the action to and attaches with client
pub(crate) fn send_action_to_session(
    cli_action: zellij_utils::cli::CliAction,
    requested_session_name: Option<String>,
    config: Option<Config>,
) {
    if matches!(cli_action, zellij_utils::cli::CliAction::ToggleTrayIcon) {
        match toggle_tray_icon_at(&zellij_utils::consts::app_control_socket()) {
            Ok(()) => std::process::exit(0),
            Err(reason) => {
                eprintln!("{}", reason);
                std::process::exit(1);
            },
        }
    }
    match get_active_session() {
        ActiveSession::None => {
            eprintln!("There is no active session!");
            std::process::exit(1);
        },
        ActiveSession::One(session_name) => {
            if let Some(requested_session_name) = requested_session_name {
                if requested_session_name != session_name {
                    eprintln!(
                        "Session '{}' not found. The following sessions are active:",
                        requested_session_name
                    );
                    eprintln!("{}", session_name);
                    std::process::exit(1);
                }
            }
            attach_with_cli_client(cli_action, &session_name, config);
        },
        ActiveSession::Many => {
            let existing_sessions: Vec<String> = get_sessions()
                .unwrap_or_default()
                .iter()
                .map(|s| s.0.clone())
                .collect();
            if let Some(session_name) = requested_session_name {
                if existing_sessions.contains(&session_name) {
                    attach_with_cli_client(cli_action, &session_name, config);
                } else {
                    eprintln!(
                        "Session '{}' not found. The following sessions are active:",
                        session_name
                    );
                    list_sessions(false, false, true);
                    std::process::exit(1);
                }
            } else if let Ok(session_name) = envs::get_session_name() {
                attach_with_cli_client(cli_action, &session_name, config);
            } else {
                eprintln!("Please specify the session name to send actions to. The following sessions are active:");
                list_sessions(false, false, true);
                std::process::exit(1);
            }
        },
    };
}
pub(crate) fn subscribe_to_session(
    subscribe_cli: zellij_utils::cli::SubscribeCli,
    requested_session_name: Option<String>,
    _config: Option<Config>,
) {
    let session_name = match get_active_session() {
        ActiveSession::None => {
            eprintln!("There is no active session!");
            std::process::exit(1);
        },
        ActiveSession::One(session_name) => {
            if let Some(ref requested) = requested_session_name {
                if *requested != session_name {
                    eprintln!(
                        "Session '{}' not found. The following sessions are active:",
                        requested
                    );
                    eprintln!("{}", session_name);
                    std::process::exit(1);
                }
            }
            session_name
        },
        ActiveSession::Many => {
            let existing_sessions: Vec<String> = get_sessions()
                .unwrap_or_default()
                .iter()
                .map(|s| s.0.clone())
                .collect();
            if let Some(session_name) = requested_session_name {
                if existing_sessions.contains(&session_name) {
                    session_name
                } else {
                    no_session_answers_to(format!(
                        "Session '{}' not found. The following sessions are active:",
                        session_name
                    ));
                }
            } else if let Ok(session_name) = envs::get_session_name() {
                session_name
            } else {
                no_session_answers_to(
                    "Please specify the session name to subscribe to. The following sessions are \
                     active:"
                        .to_owned(),
                );
            }
        },
    };
    // `--pane-id` takes every form a pane answers to, here as everywhere else. The two id forms
    // mean the same thing without asking anyone; a handle or a uuid names a pane only against the
    // session's live panes, so it is resolved before the subscription goes out and the stream is
    // opened on a pane id like it always was.
    let mut subscribe_cli = subscribe_cli;
    for target in subscribe_cli.pane_id.iter_mut() {
        match target.parse::<PaneTarget>() {
            // a string that names no pane in any form is malformed input: an error, exit 1
            Err(malformed) => {
                eprintln!("{}", malformed);
                std::process::exit(1);
            },
            // an id form is the same pane in any session, so nothing is asked
            Ok(PaneTarget::Id(_)) => continue,
            Ok(_) => match zellij_client::cli_client::resolve_pane_target(
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                )),
                &session_name,
                target,
            ) {
                Ok(pane_id) => *target = pane_id.to_string(),
                // a well-formed target no live pane answers to is a miss, exit 2
                Err(message) => {
                    eprintln!("{}", message);
                    std::process::exit(2);
                },
            },
        }
    }
    // a subscription reads a session for as long as it runs, so a stale server is said here too
    zellij_utils::session_lifecycle::check_server_build(&session_name);
    let os_input = get_os_input(zellij_client::os_input_output::get_cli_client_os_input);
    zellij_client::cli_client::start_subscribe_client(
        Box::new(os_input),
        &session_name,
        subscribe_cli,
    );
}

pub(crate) fn prompt_in_session(
    prompt_cli: zellij_utils::cli::PromptCli,
    requested_session_name: Option<String>,
    config: Option<Config>,
) -> ! {
    let session_name = match requested_session_name.or_else(|| envs::get_session_name().ok()) {
        Some(session_name) => session_name,
        None => {
            eprintln!("zellij prompt must be run inside a Zellij session");
            process::exit(zellij_utils::prompt::EXIT_ERROR);
        },
    };
    let existing_sessions: Vec<String> = get_sessions()
        .unwrap_or_default()
        .iter()
        .map(|s| s.0.clone())
        .collect();
    if !existing_sessions.contains(&session_name) {
        eprintln!("Session '{}' not found", session_name);
        process::exit(zellij_utils::prompt::EXIT_ERROR);
    }
    let plugin_url = match &config {
        Some(config)
            if config
                .plugins
                .aliases
                .contains_key(zellij_utils::prompt::PROMPT_PLUGIN_ALIAS) =>
        {
            zellij_utils::prompt::PROMPT_PLUGIN_ALIAS.to_owned()
        },
        _ => zellij_utils::prompt::PROMPT_PLUGIN_URL.to_owned(),
    };
    let os_input = get_os_input(zellij_client::os_input_output::get_cli_client_os_input);
    let exit_status = zellij_client::cli_client::start_prompt_client(
        Box::new(os_input),
        &session_name,
        prompt_cli,
        plugin_url,
    );
    process::exit(exit_status);
}

/// The text for a `write-chars` or a `paste` that was not given any on the command line.
///
/// Piping is the way multi-line text gets into a pane without being escaped twice - once for this
/// shell and once for the pane's. So no positional means stdin, and an explicit `-` means stdin
/// even from a terminal, where reading it would otherwise be a hang nobody asked for.
///
/// Every exit here is the command's own: 1 for text that cannot be sent, 2 for a stdin that carried
/// nothing, which is a request that would change nothing.
fn text_for(chars: Option<String>, verb: &str) -> String {
    use std::io::IsTerminal;
    match chars {
        Some(chars) if chars != "-" => chars,
        given => {
            if given.is_none() && std::io::stdin().is_terminal() {
                eprintln!(
                    "`{}` needs text: pass it as an argument, or pipe it in. \
                     `{} -` reads this terminal.",
                    verb, verb
                );
                std::process::exit(1);
            }
            match zellij_utils::cli::text_from_stdin(std::io::stdin().lock(), verb) {
                Ok(text) if text.is_empty() => {
                    eprintln!("Nothing arrived on stdin, so `{}` wrote nothing.", verb);
                    std::process::exit(2);
                },
                Ok(text) => text,
                Err(message) => {
                    eprintln!("{}", message);
                    std::process::exit(1);
                },
            }
        },
    }
}

fn attach_with_cli_client(
    cli_action: zellij_utils::cli::CliAction,
    session_name: &str,
    config: Option<Config>,
    output_json: bool,
) {
    // `action --json` is answered in one of two places, and which one is a question about the verb.
    // A verb that already prints JSON owns the flag, so it is simply turned on and nothing else
    // changes; every other reporting verb prints a record, which the client structures. A payload
    // has no keys to structure, so it is refused here rather than answered with something wrong.
    let mut cli_action = cli_action;
    let default_anchor = crate::pane_default::caller_anchor(
        session_name,
        std::env::var("ZELLIJ_SESSION_NAME").ok().as_deref(),
        std::env::var("ZELLIJ_PANE_ID").ok().as_deref(),
    );
    let stacked_edit = crate::pane_default::apply(
        &mut cli_action,
        config.as_ref().and_then(|config| config.new_pane_default),
        default_anchor,
        &mut std::io::stderr(),
    );
    let mut structure_report = false;
    if output_json && !cli_action.set_output_json() {
        let verb = cli_action.verb();
        match zellij_utils::cli_surface::output_shape(&format!("action {}", verb)) {
            Some(shape) if shape != "record" => {
                eprintln!(
                    "`{}` prints a {}, which has no keys to answer with. `--json` is for the \
                     verbs that report a record.",
                    verb, shape
                );
                std::process::exit(2);
            },
            _ => structure_report = true,
        }
    }
    let cli_action = cli_action;
    // an action is the usual way a script meets a session, so it is where a stale server is usually
    // met too - the warning costs one process scan and changes nothing about the action. In
    // strict mode a proven mismatch refuses here instead, before anything is sent.
    zellij_utils::session_lifecycle::check_server_build(session_name);
    let os_input = get_os_input(zellij_client::os_input_output::get_cli_client_os_input);
    let get_current_dir = || std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // `save-session --archive` is a client-side addition to the existing action: the action itself
    // is unchanged and synchronous, so the archive copy is taken once it has returned. Nothing
    // about it crosses the client/server contract.
    let should_archive = matches!(
        cli_action,
        zellij_utils::cli::CliAction::SaveSession { archive: true }
    );
    let snapshot_settings =
        SnapshotSettings::from_options(config.as_ref().map(|config| &config.options));
    // A handle or a uuid names a pane only against the session's live panes, so those forms are
    // resolved by asking the running server before the action is built. An id form needs no
    // lookup, and pays for nothing: the connection is only opened when there is a question.
    let resolve_pane_target = |target: &str| -> Result<PaneId, String> {
        match target.parse::<PaneTarget>() {
            // a string that names no pane in any form is malformed input, which is an error and
            // exits 1. Only a well-formed target that no live pane answers to is a miss, and the
            // miss is what the caller below turns into exit 2
            Err(malformed) => {
                eprintln!("{}", malformed);
                std::process::exit(1);
            },
            Ok(PaneTarget::Id(pane_id)) => Ok(pane_id),
            Ok(_) => zellij_client::cli_client::resolve_pane_target(
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                )),
                session_name,
                target,
            ),
        }
    };
    // a command that acts on "the focused thing" is only meaningful from inside the session that
    // has the focus. From a script it resolves to a pane the caller has never seen
    let inside_the_session = envs::get_session_name()
        .map(|ambient| ambient == session_name)
        .unwrap_or(false);
    // a refusal exits 2 like the confirm layer beside it: a well-formed request that changed
    // nothing is what this fork's exit codes call a miss, and a guard that stops a call before it
    // is sent has changed nothing
    if let Some(message) = zellij_utils::cli::missing_target(&cli_action, inside_the_session) {
        eprintln!("{}", message);
        std::process::exit(2);
    }
    // a pane in another session cannot be named by a handle or a uuid from here, and guessing is
    // worse than refusing: the id it would resolve to belongs to a pane in this session
    if let Some(message) = zellij_utils::cli::cross_session_pane_target_needs_an_id(&cli_action) {
        eprintln!("{}", message);
        std::process::exit(1);
    }
    // what cannot be undone is confirmed last of the guards and before anything is sent: a call
    // that was never going to reach a pane should not ask about closing one
    if let Some((verb, what, yes)) = zellij_utils::cli::confirmation_needed(&cli_action) {
        // and the target is looked up FIRST, which is the whole of that sentence. The prompt is
        // ours and the pane list is the server's, so without this `close-pane --pane-id
        // terminal_99` asked whether to close a pane that does not exist and reported the miss
        // after the answer. An id form is asked about like any other here - it needs no lookup to
        // be UNDERSTOOD, which is a different question from whether a pane answers to it
        if let Some(target) = zellij_utils::cli::pane_target_to_confirm(&cli_action) {
            if let Err(message) = zellij_client::cli_client::resolve_pane_target(
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                )),
                session_name,
                target,
            ) {
                eprintln!("{}", message);
                std::process::exit(2);
            }
        }
        confirm_or_exit(verb, what, yes);
    }
    // `wait` never becomes an action the server runs. It is a question asked over and over, or a
    // subscription read until something happens, and both of those are the client's to hold: the
    // server would have to keep a caller alive across an unbounded stretch of time to do it here
    if let zellij_utils::cli::CliAction::Wait {
        pane_id,
        panes,
        wait_for,
        pattern,
        quiet_ms,
        timeout,
    } = &cli_action
    {
        if *wait_for == zellij_utils::cli::WaitFor::AgentChange {
            let mut targets = panes.clone();
            if !pane_id.is_empty() {
                targets.push(pane_id.clone());
            }
            let make_input = || -> Box<dyn zellij_client::os_input_output::ClientOsApi> {
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                ))
            };
            let mut ids = Vec::new();
            for target in targets {
                match zellij_client::cli_client::resolve_pane_target(
                    make_input(),
                    session_name,
                    &target,
                ) {
                    Ok(PaneId::Terminal(id)) => ids.push(id),
                    Ok(_) => {
                        eprintln!("Agent-change requires terminal panes");
                        std::process::exit(2);
                    },
                    Err(message) => {
                        eprintln!("{}", message);
                        std::process::exit(2);
                    },
                }
            }
            std::process::exit(zellij_client::cli_client::start_agent_change_client(
                &make_input,
                session_name,
                &ids,
                (*timeout > 0).then(|| Duration::from_secs(*timeout)),
                structure_report,
            ));
        }
        if pane_id.is_empty() || !panes.is_empty() {
            eprintln!("This wait requires one positional pane; --pane requires --for agent-change");
            std::process::exit(2);
        }
        // an id form is asked about too: a wait on an id no pane holds would poll a list the pane
        // is not in and call that an exit
        let pane = match zellij_client::cli_client::resolve_pane_target(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
            pane_id,
        ) {
            Ok(pane) => pane,
            Err(message) => {
                eprintln!("{}", message);
                std::process::exit(2);
            },
        };
        let condition = match wait_for {
            zellij_utils::cli::WaitFor::AgentChange => unreachable!(),
            zellij_utils::cli::WaitFor::Exit => zellij_client::cli_client::WaitCondition::Exit,
            zellij_utils::cli::WaitFor::Quiet => {
                zellij_client::cli_client::WaitCondition::Quiet(Duration::from_millis(*quiet_ms))
            },
            // clap requires `--match` for this mode, so an absent pattern here is unreachable
            // rather than a case with a sensible answer. The wait compiles it and refuses one that
            // is not a regex
            zellij_utils::cli::WaitFor::Match => {
                zellij_client::cli_client::WaitCondition::Match(pattern.clone().unwrap_or_default())
            },
            zellij_utils::cli::WaitFor::Ready => {
                zellij_client::cli_client::WaitCondition::Ready(Duration::from_millis(*quiet_ms))
            },
            zellij_utils::cli::WaitFor::AgentIdle => {
                zellij_client::cli_client::WaitCondition::Agent {
                    waiting: false,
                    quiet: Duration::from_millis(*quiet_ms),
                }
            },
            zellij_utils::cli::WaitFor::AgentWaiting => {
                zellij_client::cli_client::WaitCondition::Agent {
                    waiting: true,
                    quiet: Duration::from_millis(*quiet_ms),
                }
            },
        };
        let exit_status = zellij_client::cli_client::start_wait_client(
            &|| {
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                ))
            },
            session_name,
            pane,
            condition,
            // `--timeout 0` is the caller asking, by name, for a wait that can hang
            (*timeout > 0).then(|| Duration::from_secs(*timeout)),
            structure_report,
        );
        std::process::exit(exit_status);
    }
    // `watch` never becomes an action: it is a stream the server pushes down its own connection
    if let zellij_utils::cli::CliAction::Watch { .. } = &cli_action {
        let exit_status = zellij_client::cli_client::start_watch_client(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
        );
        std::process::exit(exit_status);
    }
    if let zellij_utils::cli::CliAction::Whoami { json } = &cli_action {
        let exit_status = zellij_client::cli_client::start_whoami_client(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
            *json || output_json,
        );
        std::process::exit(exit_status);
    }
    // `list-agents` never becomes an action either. It is `list-panes` with a filter and a
    // different table, and doing that in the client is what keeps a whole verb off the
    // client/server contract
    if let zellij_utils::cli::CliAction::ListAgents { json, .. } = &cli_action {
        let exit_status = zellij_client::cli_client::start_list_agents_client(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
            *json,
        );
        std::process::exit(exit_status);
    }
    // `send-message` resolves its target from the agent list first, then sends the action itself;
    // `message-status` is one question, printed by the client
    if let zellij_utils::cli::CliAction::SendMessage { .. } = &cli_action {
        let exit_status = send_message(
            &cli_action,
            Some(session_name),
            &[session_name.to_owned()],
            output_json,
        );
        std::process::exit(exit_status);
    }
    if let zellij_utils::cli::CliAction::MessageStatus { id, json } = &cli_action {
        let exit_status = zellij_client::cli_client::start_message_status_client(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
            id.as_deref(),
            *json || output_json,
        );
        std::process::exit(exit_status);
    }
    // `last-reply` never becomes an action: it reads the agent's transcript on this machine,
    // found from the pane list
    if let zellij_utils::cli::CliAction::LastReply { pane_id, json } = &cli_action {
        let exit_status = zellij_client::cli_client::start_last_reply_client(
            &|| {
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                ))
            },
            session_name,
            pane_id,
            *json || output_json,
        );
        std::process::exit(exit_status);
    }
    // a `dump-screen` of several panes never becomes several actions either. It is the same per-pane
    // request, asked N times down one connection, and holding that in the client is what keeps a
    // sweep off the client/server contract. One `--pane-id` is not a sweep and does not come here:
    // it keeps printing the bare content it always did
    if let zellij_utils::cli::CliAction::DumpScreen {
        file,
        path,
        full,
        pane_id,
        all,
        ansi,
    } = &cli_action
    {
        if *all || pane_id.len() > 1 {
            let exit_status = zellij_client::cli_client::start_dump_screen_client(
                Box::new(get_os_input(
                    zellij_client::os_input_output::get_cli_client_os_input,
                )),
                session_name,
                pane_id.clone(),
                *all,
                *full,
                *ansi,
                path.clone().or_else(|| file.clone()),
            );
            std::process::exit(exit_status);
        }
    }
    let mut cli_action = cli_action;
    // an agent's new pane or tab leaves the person's focus alone unless it passed `--focus`
    cli_action.settle_focus_for_caller(zellij_utils::agent_detect::caller_is_agent(|name| {
        std::env::var(name).ok()
    }));
    // `pipe --timeout` is the client's to hold: the session has nothing to time out, and keeping it
    // here is what keeps the flag off `Action::CliPipe` and off the client/server contract
    let pipe_timeout = cli_action.take_pipe_timeout();
    // `--wait-ready` is the client's to hold too: it waits once the report names the pane
    let ready_timeout = match cli_action.take_wait_ready() {
        Ok(ready_timeout) => ready_timeout,
        Err(message) => {
            eprintln!("{}", message);
            std::process::exit(2);
        },
    };
    // `--near` names the pane the new one opens beside. It is resolved here, like every other pane
    // target, and travels as the pane this command came from - the channel `--near-current-pane`
    // reads out of the environment
    let mut anchor_pane: Option<u32> = None;
    if let Some(wanted) = cli_action.near_target().map(|t| t.to_owned()) {
        match resolve_pane_target(&wanted) {
            Ok(PaneId::Terminal(id)) => {
                anchor_pane = Some(id);
                cli_action.anchor_near();
            },
            Ok(PaneId::Plugin(_)) => {
                eprintln!(
                    "'{}' is a plugin pane, and `--near` anchors a new pane to a terminal one. \
                     Name a terminal pane, or use `--in-tab` to put the pane in the same tab.",
                    wanted
                );
                std::process::exit(1);
            },
            Err(message) => {
                eprintln!("{}", message);
                std::process::exit(2);
            },
        }
    }
    // `--in-tab` names a tab the way a person does, and the action carries a stable id. The session
    // is the only thing that knows which is which, so it is asked before the pane is made: a tab
    // nothing answers to is a miss, and nothing is created
    if let Some(wanted) = cli_action.in_tab_target().map(|t| t.to_owned()) {
        let found = zellij_client::cli_client::resolve_tab_target(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
            &wanted,
        );
        match found {
            Ok(Some(tab_id)) => cli_action.place_in_tab(tab_id),
            Ok(None) => {
                eprintln!(
                    "No tab answers to '{}'. `zellij action list-tabs` lists them by TAB_ID and \
                     NAME.",
                    wanted
                );
                std::process::exit(2);
            },
            Err(message) => {
                eprintln!("{}", message);
                std::process::exit(1);
            },
        }
    }
    // `move-tab --to-index` is upstream's `MoveTabToPosition`, which names its tab. Without
    // `--tab-id` it moves the focused tab, as it always has, so that tab is asked for here
    if let zellij_utils::cli::CliAction::MoveTab {
        to_index: Some(_),
        tab_id: tab_id @ None,
        ..
    } = &mut cli_action
    {
        match zellij_client::cli_client::resolve_focused_tab(
            Box::new(get_os_input(
                zellij_client::os_input_output::get_cli_client_os_input,
            )),
            session_name,
        ) {
            Ok(Some(focused)) => *tab_id = Some(focused),
            Ok(None) => {
                eprintln!("The session has no tab to move.");
                std::process::exit(2);
            },
            Err(message) => {
                eprintln!("{}", message);
                std::process::exit(1);
            },
        }
    }
    // the text these two write can come from stdin. It is read here, after the refusals above, so a
    // call that was never going to reach a pane does not drain the pipe on its way out
    let cli_action = match cli_action {
        zellij_utils::cli::CliAction::WriteChars {
            chars,
            pane_id,
            focused,
        } => zellij_utils::cli::CliAction::WriteChars {
            chars: Some(text_for(chars, "write-chars")),
            pane_id,
            focused,
        },
        zellij_utils::cli::CliAction::Paste {
            chars,
            pane_id,
            focused,
        } => zellij_utils::cli::CliAction::Paste {
            chars: Some(text_for(chars, "paste")),
            pane_id,
            focused,
        },
        other => other,
    };
    let actions = if stacked_edit {
        crate::new_pane_default::stacked_edit(cli_action, config.as_ref(), get_current_dir())
            .map(|action| vec![action])
    } else {
        Action::actions_from_cli(
            cli_action,
            Box::new(get_current_dir),
            config,
            &resolve_pane_target,
        )
    };
    match actions {
        Ok(actions) => {
            let exit_status = zellij_client::cli_client::start_cli_client(
                Box::new(os_input),
                session_name,
                actions,
                anchor_pane,
                pipe_timeout,
                structure_report,
                ready_timeout.map(|timeout| zellij_client::cli_client::WaitReady {
                    timeout,
                    make_os_input: &|| {
                        Box::new(get_os_input(
                            zellij_client::os_input_output::get_cli_client_os_input,
                        ))
                    },
                }),
            );
            if should_archive {
                match archive_session_info(session_name, SnapshotReason::Manual, &snapshot_settings)
                {
                    Ok(ArchiveOutcome::Archived(snapshot)) => {
                        println!("Archived snapshot {}", snapshot.id)
                    },
                    Ok(ArchiveOutcome::AlreadyArchived) => println!("Nothing new to archive."),
                    Ok(ArchiveOutcome::Disabled) => {
                        eprintln!(
                            "Archiving is off (`session_snapshot_limit 0`); nothing was archived."
                        );
                        std::process::exit(2);
                    },
                    Ok(ArchiveOutcome::NothingToArchive) => {
                        eprintln!("There is no serialized layout for this session to archive.");
                        std::process::exit(2);
                    },
                    Err(e) => {
                        eprintln!("Failed to archive the session: {}", e);
                        std::process::exit(2);
                    },
                }
            }
            std::process::exit(exit_status);
        },
        Err(e) => {
            eprintln!("{}", e);
            log::error!("Error sending action: {}", e);
            std::process::exit(2);
        },
    }
}

/// The session this invocation names, where it names one.
///
/// Two spellings, because `session up` uses the second: it clears `opts.session` and puts the name
/// in the `Attach` command it hands to `start_client`. `None` is a session with no name yet, which
/// has no launcher installed for it and so nothing to disagree with.
fn session_name_in_opts(opts: &CliArgs) -> Option<String> {
    if let Some(session) = opts.session.clone() {
        return Some(session);
    }
    match &opts.command {
        Some(Command::Sessions(Sessions::Attach { args, .. })) => args.session_name.clone(),
        _ => None,
    }
}

pub(crate) fn start_client(opts: CliArgs) {
    let (
        config,
        client_layout_info,
        config_options,
        mut config_without_layout,
        mut config_options_without_layout,
    ) = match Setup::from_cli_args(&opts) {
        Ok(results) => results,
        Err(e) => {
            if let ConfigError::KdlError(error) = e {
                let report: Report = error.into();
                eprintln!("{:?}", report);
            } else {
                eprintln!("{}", e);
            }
            process::exit(1);
        },
    };

    // `pin_exe` covered only the generated service unit until now, so a session started by typing
    // `zellij` ran a server from the package manager's versioned path - a new TCC subject after
    // every upgrade, which is exactly what the pin exists to prevent. Decided once here, where the
    // config is, and only for the server: the client stays the binary the user typed.
    #[cfg(unix)]
    zellij_client::record_pinned_server_exe(
        zellij_utils::session_service::server_exe_for_interactive_launch(
            config_options.session_service.as_ref(),
            session_name_in_opts(&opts).as_deref(),
        ),
    );

    let mut reconnect_to_session: Option<ConnectToSession> = None;
    let os_input = get_os_input(get_client_os_input);
    loop {
        let os_input = os_input.clone();
        let mut config = config.clone();
        let mut config_options = config_options.clone();
        let mut opts = opts.clone();
        let mut is_a_reconnect = false;
        let mut should_create_detached = false;
        let mut layout_info = client_layout_info.clone();
        let mut new_session_cwd = None;

        if let Some(reconnect_to_session) = &reconnect_to_session {
            // this is integration code to make session reconnects work with this existing,
            // untested and pretty involved function
            //
            // ideally, we should write tests for this whole function and refctor it
            reload_config_from_disk(
                &mut config_without_layout,
                &mut config_options_without_layout,
                &opts,
            );
            if reconnect_to_session.name.is_some() {
                opts.command = Some(Command::Sessions(Sessions::Attach {
                    args: AttachArgs {
                        session_name: reconnect_to_session.name.clone(),
                        create: true,
                        ..AttachArgs::default()
                    },
                    create_background: false,
                }));
            } else {
                opts.command = None;
                opts.session = None;
                config_options.attach_to_session = None;
            }

            if let Some(reconnect_layout) = &reconnect_to_session.layout {
                layout_info = Some(reconnect_layout.clone());
            }
            if let Some(cwd) = &reconnect_to_session.cwd {
                new_session_cwd = Some(cwd.clone());
            }
            config = config_without_layout.clone();
            config_options = config_options_without_layout.clone();
            is_a_reconnect = true;
        }
        let switch_context = reconnect_to_session
            .as_ref()
            .and_then(|r| r.switch_context.clone());

        let start_client_plan = |session_name: std::string::String| {
            assert_session_ne(&session_name);
        };

        #[cfg_attr(not(feature = "web_server_capability"), allow(unused_variables))]
        if let Some(Command::Sessions(Sessions::Attach {
            args,
            create_background,
        })) = opts.command.clone()
        {
            // fork: `attach`'s own positional wins when given - `zellij -s foo attach bar` means
            // bar. But the top-level `-s`/`--session` already names which session every other verb
            // here targets (`send_action_to_session`, `subscribe_to_session`), so `attach` on its
            // own dropped it silently: `zellij -s myname attach -c` ignored `-s` entirely and, with
            // no active session to fall back to, generated a fresh random name instead.
            let mut args = args;
            if args.session_name.is_none() {
                args.session_name = opts.session.clone();
            }
            if let Some(remote_session_url) = args.remote_url() {
                if !cfg!(feature = "web_server_capability") {
                    eprintln!("This version of Zellij was compiled without web/remote-attach capabilities.");
                    std::process::exit(2);
                }

                if args.options.is_some()
                    || args.create
                    || create_background
                    || args.force_run_commands
                {
                    eprintln!("Cannot attach to remote session with options.");
                    std::process::exit(2);
                }

                if !args.initial_command.is_empty() {
                    eprintln!("Cannot run an initial command on a remote session.");
                    std::process::exit(2);
                }

                #[cfg(feature = "web_server_capability")]
                if let Err(e) = zellij_client::start_remote_client(
                    Box::new(os_input.clone()),
                    remote_session_url,
                    args.token.clone(),
                    args.remember,
                    args.forget,
                    args.ca_cert.clone(),
                    args.insecure,
                    config_options.client_async_worker_tasks,
                ) {
                    eprintln!("{}", e);
                    std::process::exit(2);
                }
            } else {
                should_create_detached = create_background;

                let Resolved {
                    mut client,
                    config_options,
                } = match session_resolution::resolve_attach(
                    &args,
                    config_options,
                    should_create_detached,
                    new_session_cwd.clone(),
                ) {
                    Ok(resolved) => resolved,
                    Err(failure) => failure.report_and_exit(),
                };

                if let Err(failure) = session_resolution::refuse_self_attach(&client) {
                    panic!("{}", failure.message);
                }

                // an attach joins a server that may predate this binary. Said once per client, and
                // only for a session that already exists - a new one is this build by definition.
                if let ClientInfo::Attach(session_name, _) = &client {
                    zellij_utils::session_lifecycle::check_server_build(session_name);
                }

                if let Some(layout_info) = layout_info {
                    client.set_layout_info(layout_info);
                }

                if let Some(new_session_cwd) = new_session_cwd {
                    client.set_cwd(new_session_cwd);
                }

                let current_dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
                if let Some(initial_panes) = initial_panes_from_cli(
                    args.initial_command.clone(),
                    None,
                    Some(current_dir.clone()),
                    current_dir,
                    args.close_on_exit,
                    args.start_suspended,
                ) {
                    client.set_initial_panes(initial_panes);
                }

                let tab_position_to_focus = reconnect_to_session
                    .as_ref()
                    .and_then(|r| r.tab_position.clone());
                let pane_id_to_focus = reconnect_to_session
                    .as_ref()
                    .and_then(|r| r.pane_id.clone());
                reconnect_to_session = start_client_impl(
                    Box::new(os_input),
                    opts,
                    config,
                    config_options,
                    client,
                    tab_position_to_focus,
                    pane_id_to_focus,
                    is_a_reconnect,
                    should_create_detached,
                    switch_context.clone(),
                );
            }
        } else {
            if let Some(session_name) = opts.session.clone() {
                start_client_plan(session_name.clone());
                reconnect_to_session = start_client_impl(
                    Box::new(os_input),
                    opts,
                    config,
                    config_options,
                    ClientInfo::New(session_name, layout_info, new_session_cwd, None),
                    None,
                    None,
                    is_a_reconnect,
                    should_create_detached,
                    switch_context.clone(),
                );
            } else {
                if let Some(session_name) = config_options.session_name.as_ref() {
                    if let Ok(val) = envs::get_session_name() {
                        // This prevents the same type of recursion as above, only that here we
                        // don't get the command to "attach", but to start a new session instead.
                        // This occurs for example when declaring the session name inside a layout
                        // file and then, from within this session, trying to open a new zellij
                        // session with the same layout. This causes an infinite recursion in the
                        // `zellij_server::terminal_bytes::listen` task, flooding the server and
                        // clients with infinite `Render` requests.
                        if *session_name == val {
                            eprintln!("You are trying to attach to the current session (\"{}\"). Zellij does not support nesting a session in itself.", session_name);
                            process::exit(1);
                        }
                    }
                    match config_options.attach_to_session {
                        Some(true) => {
                            let client = match session_resolution::attach_by_name(
                                Some(session_name.clone()),
                                config_options.clone(),
                                true,
                            ) {
                                Ok(client) => client,
                                Err(failure) => failure.report_and_exit(),
                            };
                            reconnect_to_session = start_client_impl(
                                Box::new(os_input),
                                opts,
                                config,
                                config_options,
                                client,
                                None,
                                None,
                                is_a_reconnect,
                                should_create_detached,
                                switch_context.clone(),
                            );
                        },
                        _ => {
                            start_client_plan(session_name.clone());
                            reconnect_to_session = start_client_impl(
                                Box::new(os_input),
                                opts,
                                config,
                                config_options.clone(),
                                ClientInfo::New(
                                    session_name.clone(),
                                    layout_info,
                                    new_session_cwd,
                                    None,
                                ),
                                None,
                                None,
                                is_a_reconnect,
                                should_create_detached,
                                switch_context.clone(),
                            );
                        },
                    }
                    if reconnect_to_session.is_some() {
                        continue;
                    }
                    // after we detach, this happens and so we need to exit before the rest of the
                    // function happens
                    process::exit(0);
                }

                let session_name = generate_unique_session_name_or_exit();
                start_client_plan(session_name.clone());
                reconnect_to_session = start_client_impl(
                    Box::new(os_input),
                    opts,
                    config,
                    config_options,
                    ClientInfo::New(session_name, layout_info, new_session_cwd, None),
                    None,
                    None,
                    is_a_reconnect,
                    should_create_detached,
                    switch_context.clone(),
                );
            }
        }
        if reconnect_to_session.is_none() {
            break;
        }
    }
}

fn generate_unique_session_name_or_exit() -> String {
    let Some(unique_session_name) = generate_unique_session_name() else {
        eprintln!("Failed to generate a unique session name, giving up");
        process::exit(1);
    };
    unique_session_name
}

pub(crate) fn list_aliases(opts: CliArgs) {
    let (config, _layout, _config_options, _config_without_layout, _config_options_without_layout) =
        match Setup::from_cli_args(&opts) {
            Ok(results) => results,
            Err(e) => {
                if let ConfigError::KdlError(error) = e {
                    let report: Report = error.into();
                    eprintln!("{:?}", report);
                } else {
                    eprintln!("{}", e);
                }
                process::exit(1);
            },
        };
    for alias in config.plugins.list() {
        println!("{}", alias);
    }
    process::exit(0);
}

pub(crate) fn watch_session(session_name: Option<String>, opts: CliArgs) {
    let (config, _, config_options, _, _) = match Setup::from_cli_args(&opts) {
        Ok(results) => results,
        Err(e) => {
            if let ConfigError::KdlError(error) = e {
                let report: Report = error.into();
                eprintln!("{:?}", report);
            } else {
                eprintln!("{}", e);
            }
            process::exit(1);
        },
    };

    let client_info = match session_resolution::resolve_watch(session_name, config_options.clone())
    {
        Ok(client_info) => client_info,
        Err(failure) => failure.report_and_exit(),
    };

    // a watcher is an attach that cannot type, so it meets a stale server the way an attach does
    zellij_utils::session_lifecycle::check_server_build(client_info.get_session_name());

    let mut opts = opts.clone();
    opts.session = Some(client_info.get_session_name().to_string());

    let os_input = get_os_input(get_client_os_input);

    // Start the watcher client
    start_client_impl(
        Box::new(os_input),
        opts,
        config,
        config_options,
        client_info,
        None,  // tab_position_to_focus
        None,  // pane_id_to_focus
        false, // is_a_reconnect
        false, // should_create_detached
        None,
    );
}

fn reload_config_from_disk(
    config_without_layout: &mut Config,
    config_options_without_layout: &mut Options,
    opts: &CliArgs,
) {
    match Setup::from_cli_args(&opts) {
        Ok((_, _, _, reloaded_config_without_layout, reloaded_config_options_without_layout)) => {
            *config_without_layout = reloaded_config_without_layout;
            *config_options_without_layout = reloaded_config_options_without_layout;
        },
        Err(e) => {
            log::error!("Failed to reload config: {}", e);
        },
    };
}

pub fn get_config_options_from_cli_args(opts: &CliArgs) -> Result<Options, String> {
    Setup::from_cli_args(&opts)
        .map(|(_, _, config_options, _, _)| config_options)
        .map_err(|e| e.to_string())
}
