use dialoguer::Confirm;
use std::net::IpAddr;
use std::{fs::File, io::prelude::*, path::PathBuf, process, time::Duration};

#[cfg(feature = "web_server_capability")]
use isahc::{config::RedirectPolicy, prelude::*, HttpClient, Request};

use zellij_client::{
    old_config_converter::{
        config_yaml_to_config_kdl, convert_old_yaml_files, layout_yaml_to_layout_kdl,
    },
    os_input_output::get_client_os_input,
    start_client as start_client_impl, ClientInfo,
};

use zellij_utils::sessions::{
    assert_dead_session, assert_session, assert_session_ne, delete_session as delete_session_impl,
    discard_resurrection_snapshot, generate_unique_session_name, get_active_session,
    get_resurrectable_sessions, get_sessions, get_sessions_sorted_by_mtime,
    kill_session as kill_session_impl, match_session_name, print_sessions,
    print_sessions_with_index, resurrection_layout, session_exists,
    session_in_other_contract_versions, validate_session_name, ActiveSession, KillWait,
    SessionNameMatch,
};

use zellij_utils::consts::session_layout_cache_file_name;
use zellij_utils::consts::{CLIENT_SERVER_CONTRACT_VERSION, VERSION};
use zellij_utils::session_snapshot::{
    archive_session_info, archive_session_info_folder, importable_folders, is_already_archived,
    legacy_session_info_dirs, list_snapshots, prune_all, remove_snapshot, resolve_snapshot,
    unimported_legacy_layout_count, Snapshot, SnapshotReason, SnapshotSettings,
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
    cli::{CliArgs, Command, SessionCommand, Sessions, SnapshotCli},
    data::ConnectToSession,
    envs,
    input::{
        actions::Action,
        config::{Config, ConfigError},
        options::Options,
    },
    setup::Setup,
};

pub(crate) use zellij_utils::sessions::list_sessions;

pub(crate) fn kill_all_sessions(yes: bool, wait: KillWait) {
    match get_sessions() {
        Ok(sessions) if sessions.is_empty() => {
            eprintln!("No active zellij sessions found.");
            process::exit(1);
        },
        Ok(sessions) => {
            if !yes {
                println!("WARNING: this action will kill all sessions.");
                if !Confirm::new()
                    .with_prompt("Do you want to continue?")
                    .interact()
                    .unwrap()
                {
                    println!("Abort.");
                    process::exit(1);
                }
            }
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

/// A session that exists under another client/server contract is invisible to this binary, and the
/// bare "no session with that name" is misleading about why. Name the mismatch, and the way out.
fn print_contract_mismatch_help(session_name: &str, config_options: Option<&Options>) {
    let contracts = session_in_other_contract_versions(session_name);
    if contracts.is_empty() {
        return;
    }
    let contracts: Vec<String> = contracts.iter().map(|c| c.to_string()).collect();
    eprintln!(
        "Session '{}' is running under client/server contract {}; this binary speaks {}.",
        session_name,
        contracts.join(", "),
        CLIENT_SERVER_CONTRACT_VERSION
    );
    let settings = SnapshotSettings::from_options(config_options);
    if resolve_snapshot(&settings, "latest", Some(session_name)).is_ok() {
        eprintln!("Its layout was captured - rebuild it with:");
        eprintln!(
            "    zellij snapshot restore latest --session {}",
            session_name
        );
    }
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

/// The upstream release a fork version is built on, eg. `0.44.3` for `0.44.3-nkmk.4`.
fn upstream_base_version(version: &str) -> &str {
    version.split('-').next().unwrap_or(version)
}

/// Say so and continue. A snapshot from another upstream base may still restore perfectly; refusing
/// on the strength of a version string would be a guess in the unhelpful direction.
fn report_snapshot_version_drift(snapshot: &Snapshot) {
    let snapshot_version = snapshot.meta.zellij_version.as_str();
    if snapshot_version.is_empty() {
        return;
    }
    if upstream_base_version(snapshot_version) != upstream_base_version(VERSION) {
        eprintln!(
            "Note: snapshot {} was written by zellij {}, this binary is {}. Restoring anyway.",
            snapshot.id, snapshot_version, VERSION
        );
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
                session_name: Some(target_session_name),
                create: true,
                create_background: false,
                force_run_commands: false,
                no_resurrect: false,
                restore: Some(snapshot.id),
                index: None,
                options: None,
                token: None,
                remember: false,
                forget: false,
                ca_cert: None,
                insecure: false,
            }));
            start_client(opts);
        },
        SnapshotCli::Rm { id } => {
            let snapshot = resolve_snapshot_or_exit(&settings, &id, None);
            match remove_snapshot(&snapshot) {
                Ok(()) => println!("Deleted snapshot {}", snapshot.id),
                Err(e) => {
                    eprintln!("Failed to delete snapshot {}: {}", snapshot.id, e);
                    process::exit(2);
                },
            }
        },
        SnapshotCli::Import {
            from,
            dry_run,
            prune_source,
        } => {
            import_snapshots_command(&settings, from, dry_run, prune_source);
        },
        SnapshotCli::Prune { keep } => {
            let keep = keep.unwrap_or(settings.limit);
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

fn import_snapshots_command(
    settings: &SnapshotSettings,
    from: Option<PathBuf>,
    dry_run: bool,
    prune_source: bool,
) {
    let dirs = match from {
        Some(from) => vec![from],
        None => legacy_session_info_dirs(),
    };
    let folders = importable_folders(&dirs);
    if folders.is_empty() {
        println!("Nothing to import.");
        return;
    }
    let mut imported = 0;
    let mut skipped = 0;
    for folder in folders {
        if dry_run {
            if is_already_archived(settings, &folder) {
                println!(
                    "would skip  {} ({}) - already in the archive",
                    folder.session_name, folder.from
                );
                skipped += 1;
            } else {
                println!("would import {} ({})", folder.session_name, folder.from);
                imported += 1;
            }
            continue;
        }
        match archive_session_info_folder(
            &folder.path,
            &folder.session_name,
            SnapshotReason::Imported,
            settings,
            Some(folder.from.clone()),
        ) {
            Ok(Some(snapshot)) => {
                println!(
                    "imported {} ({}) as {}",
                    folder.session_name, folder.from, snapshot.id
                );
                imported += 1;
            },
            Ok(None) => {
                println!(
                    "skipped  {} ({}) - already in the archive",
                    folder.session_name, folder.from
                );
                skipped += 1;
            },
            Err(e) => {
                eprintln!("failed to import {}: {}", folder.path.display(), e);
                continue;
            },
        }
        if prune_source {
            if let Err(e) = std::fs::remove_dir_all(&folder.path) {
                eprintln!("failed to remove {}: {}", folder.path.display(), e);
            }
        }
    }
    println!("{} imported, {} already present.", imported, skipped);
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
        print_legacy_layout_hint(settings);
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
    print_legacy_layout_hint(settings);
}

/// Say that adoptable layouts exist; never adopt them. Silently relocating a user's files is the
/// kind of helpfulness that is indistinguishable from data loss when it goes wrong.
fn print_legacy_layout_hint(settings: &SnapshotSettings) {
    let count = unimported_legacy_layout_count(settings);
    if count > 0 {
        eprintln!(
            "\n{} saved layout(s) from another version or contract are not in the archive. Adopt them with:\n    zellij snapshot import",
            count
        );
    }
}

pub(crate) fn delete_all_sessions(yes: bool, force: bool, wait: KillWait, opts: &CliArgs) {
    use std::collections::BTreeMap;
    use zellij_server::background_jobs::scan_session_list_default_dirs;

    let active_sessions: Vec<String> = get_sessions()
        .unwrap_or_default()
        .iter()
        .map(|s| s.0.clone())
        .collect();
    let (_live_sessions, resurrectable_map) =
        scan_session_list_default_dirs(&String::new(), &[], &BTreeMap::new());
    let mut resurrectable_sessions: Vec<(String, Duration)> =
        resurrectable_map.into_iter().collect();
    if force {
        for entry in get_resurrectable_sessions() {
            if !resurrectable_sessions.iter().any(|(n, _)| *n == entry.0) {
                resurrectable_sessions.push(entry);
            }
        }
    }
    let dead_sessions: Vec<_> = if force {
        resurrectable_sessions
    } else {
        resurrectable_sessions
            .into_iter()
            .filter(|(name, _)| !active_sessions.contains(name))
            .collect()
    };
    if !yes {
        println!("WARNING: this action will delete all resurrectable sessions.");
        if !Confirm::new()
            .with_prompt("Do you want to continue?")
            .interact()
            .unwrap()
        {
            println!("Abort.");
            process::exit(1);
        }
    }
    let mut all_gone = true;
    for session in &dead_sessions {
        all_gone &= delete_session_impl(&session.0, force, &snapshot_settings(opts), wait);
    }
    process::exit(if all_gone { 0 } else { 1 });
}

pub(crate) fn kill_session(target_session: &Option<String>, wait: KillWait) {
    match target_session {
        Some(target_session) => {
            assert_session(target_session);
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
) {
    match target_session {
        Some(target_session) => {
            if let Err(e) = validate_session_name(target_session) {
                eprintln!("{}", e);
                process::exit(1);
            }
            assert_dead_session(target_session, force);
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
    let os_input = get_os_input(get_server_os_input);
    start_server_impl(Box::new(os_input), path);
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

fn create_new_client() -> ClientInfo {
    ClientInfo::New(generate_unique_session_name_or_exit(), None, None)
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

fn find_indexed_session(
    sessions: Vec<String>,
    config_options: Options,
    index: usize,
    create: bool,
) -> ClientInfo {
    match sessions.get(index) {
        Some(session) => ClientInfo::Attach(session.clone(), config_options),
        None if create => create_new_client(),
        None => {
            println!(
                "No session indexed by {} found. The following sessions are active:",
                index
            );
            print_sessions_with_index(sessions);
            process::exit(1);
        },
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
                    eprintln!(
                        "Session '{}' not found. The following sessions are active:",
                        session_name
                    );
                    list_sessions(false, false, true);
                    std::process::exit(1);
                }
            } else if let Ok(session_name) = envs::get_session_name() {
                session_name
            } else {
                eprintln!("Please specify the session name to subscribe to. The following sessions are active:");
                list_sessions(false, false, true);
                std::process::exit(1);
            }
        },
    };
    let os_input = get_os_input(zellij_client::os_input_output::get_cli_client_os_input);
    zellij_client::cli_client::start_subscribe_client(
        Box::new(os_input),
        &session_name,
        subscribe_cli,
    );
}

pub(crate) fn convert_old_config_file(old_config_file: PathBuf) {
    match File::open(&old_config_file) {
        Ok(mut handle) => {
            let mut raw_config_file = String::new();
            let _ = handle.read_to_string(&mut raw_config_file);
            match config_yaml_to_config_kdl(&raw_config_file, false) {
                Ok(kdl_config) => {
                    println!("{}", kdl_config);
                    process::exit(0);
                },
                Err(e) => {
                    eprintln!("Failed to convert config: {}", e);
                    process::exit(1);
                },
            }
        },
        Err(e) => {
            eprintln!("Failed to open file: {}", e);
            process::exit(1);
        },
    }
}

pub(crate) fn convert_old_layout_file(old_layout_file: PathBuf) {
    match File::open(&old_layout_file) {
        Ok(mut handle) => {
            let mut raw_layout_file = String::new();
            let _ = handle.read_to_string(&mut raw_layout_file);
            match layout_yaml_to_layout_kdl(&raw_layout_file) {
                Ok(kdl_layout) => {
                    println!("{}", kdl_layout);
                    process::exit(0);
                },
                Err(e) => {
                    eprintln!("Failed to convert layout: {}", e);
                    process::exit(1);
                },
            }
        },
        Err(e) => {
            eprintln!("Failed to open file: {}", e);
            process::exit(1);
        },
    }
}

pub(crate) fn convert_old_theme_file(old_theme_file: PathBuf) {
    match File::open(&old_theme_file) {
        Ok(mut handle) => {
            let mut raw_config_file = String::new();
            let _ = handle.read_to_string(&mut raw_config_file);
            match config_yaml_to_config_kdl(&raw_config_file, true) {
                Ok(kdl_config) => {
                    println!("{}", kdl_config);
                    process::exit(0);
                },
                Err(e) => {
                    eprintln!("Failed to convert config: {}", e);
                    process::exit(1);
                },
            }
        },
        Err(e) => {
            eprintln!("Failed to open file: {}", e);
            process::exit(1);
        },
    }
}

fn attach_with_cli_client(
    cli_action: zellij_utils::cli::CliAction,
    session_name: &str,
    config: Option<Config>,
) {
    // an action is the usual way a script meets a session, so it is where a stale server is usually
    // met too - the warning costs one process scan and changes nothing about the action
    zellij_utils::session_lifecycle::warn_if_server_build_differs(session_name);
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
    match Action::actions_from_cli(cli_action, Box::new(get_current_dir), config) {
        Ok(actions) => {
            zellij_client::cli_client::start_cli_client(Box::new(os_input), session_name, actions);
            if should_archive {
                match archive_session_info(session_name, SnapshotReason::Manual, &snapshot_settings)
                {
                    Ok(Some(snapshot)) => println!("Archived snapshot {}", snapshot.id),
                    Ok(None) => println!("Nothing new to archive."),
                    Err(e) => {
                        eprintln!("Failed to archive the session: {}", e);
                        std::process::exit(2);
                    },
                }
            }
            std::process::exit(0);
        },
        Err(e) => {
            eprintln!("{}", e);
            log::error!("Error sending action: {}", e);
            std::process::exit(2);
        },
    }
}

fn attach_with_session_index(config_options: Options, index: usize, create: bool) -> ClientInfo {
    // Ignore the session_name when `--index` is provided
    match get_sessions_sorted_by_mtime() {
        Ok(sessions) if sessions.is_empty() => {
            if create {
                create_new_client()
            } else {
                eprintln!("No active zellij sessions found.");
                process::exit(1);
            }
        },
        Ok(sessions) => find_indexed_session(sessions, config_options, index, create),
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            process::exit(1);
        },
    }
}

fn attach_with_session_name(
    session_name: Option<String>,
    config_options: Options,
    create: bool,
) -> ClientInfo {
    match &session_name {
        Some(session) if create => {
            if session_exists(session).unwrap() {
                ClientInfo::Attach(session_name.unwrap(), config_options)
            } else {
                ClientInfo::New(session_name.unwrap(), None, None)
            }
        },
        Some(prefix) => match match_session_name(prefix).unwrap() {
            SessionNameMatch::UniquePrefix(s) | SessionNameMatch::Exact(s) => {
                ClientInfo::Attach(s, config_options)
            },
            SessionNameMatch::AmbiguousPrefix(sessions) => {
                println!(
                    "Ambiguous selection: multiple sessions names start with '{}':",
                    prefix
                );
                print_sessions(
                    sessions
                        .iter()
                        .map(|s| (s.clone(), Duration::default(), false))
                        .collect(),
                    false,
                    false,
                    true,
                );
                process::exit(1);
            },
            SessionNameMatch::None => {
                eprintln!("No session with the name '{}' found!", prefix);
                print_contract_mismatch_help(prefix, Some(&config_options));
                process::exit(1);
            },
        },
        None => match get_active_session() {
            ActiveSession::None if create => create_new_client(),
            ActiveSession::None => {
                eprintln!("No active zellij sessions found.");
                process::exit(1);
            },
            ActiveSession::One(session_name) => ClientInfo::Attach(session_name, config_options),
            ActiveSession::Many => {
                println!("Please specify the session to attach to, either by using the full name or a unique prefix.\nThe following sessions are active:");
                list_sessions(false, false, true);
                process::exit(1);
            },
        },
    }
}

pub(crate) fn start_client(opts: CliArgs) {
    // look for old YAML config/layout/theme files and convert them to KDL
    convert_old_yaml_files(&opts);
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
                    session_name: reconnect_to_session.name.clone(),
                    create: true,
                    create_background: false,
                    force_run_commands: false,
                    no_resurrect: false,
                    restore: None,
                    index: None,
                    options: None,
                    token: None,
                    remember: false,
                    forget: false,
                    ca_cert: None,
                    insecure: false,
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

        let start_client_plan = |session_name: std::string::String| {
            assert_session_ne(&session_name);
        };

        if let Some(Command::Sessions(Sessions::Attach {
            session_name,
            create,
            create_background,
            force_run_commands,
            no_resurrect,
            restore,
            index,
            options,
            token,
            remember,
            forget,
            ca_cert,
            insecure,
        })) = opts.command.clone()
        {
            if let Some(remote_session_url) = session_name.as_ref().and_then(|s| {
                if s.starts_with("http://") || s.starts_with("https://") {
                    Some(s)
                } else {
                    None
                }
            }) {
                if !cfg!(feature = "web_server_capability") {
                    eprintln!("This version of Zellij was compiled without web/remote-attach capabilities.");
                    std::process::exit(2);
                }

                if options.is_some() || create || create_background || force_run_commands {
                    eprintln!("Cannot attach to remote session with options.");
                    std::process::exit(2);
                }

                #[cfg(feature = "web_server_capability")]
                if let Err(e) = zellij_client::start_remote_client(
                    Box::new(os_input.clone()),
                    remote_session_url,
                    token,
                    remember,
                    forget,
                    ca_cert,
                    insecure,
                    config_options.client_async_worker_tasks,
                ) {
                    eprintln!("{}", e);
                    std::process::exit(2);
                }
            } else {
                let config_options = match options.as_deref() {
                    Some(SessionCommand::Options(o)) => {
                        config_options.merge_from_cli(o.to_owned().into())
                    },
                    None => config_options,
                };
                should_create_detached = create_background;

                let mut client = if let Some(idx) = index {
                    attach_with_session_index(
                        config_options.clone(),
                        idx,
                        create || should_create_detached,
                    )
                } else {
                    let session_exists = session_name
                        .as_ref()
                        .and_then(|s| session_exists(&s).ok())
                        .unwrap_or(false);
                    // --restore is the counterpart to --no-resurrect: three explicit behaviours for
                    // a dead name - resurrect from the in-place file (default), start clean
                    // (--no-resurrect), or rebuild from a chosen snapshot
                    let snapshot_to_restore = restore.as_ref().map(|id| {
                        if session_exists {
                            eprintln!(
                                "Session '{}' is running, so there is nothing to restore into.",
                                session_name.as_deref().unwrap_or_default()
                            );
                            process::exit(2);
                        }
                        resolve_snapshot_or_exit(
                            &SnapshotSettings::from_options(Some(&config_options)),
                            id,
                            session_name.as_deref(),
                        )
                    });
                    // --no-resurrect makes the snapshot invisible to the rest of this
                    // decision, so the session is built from the layout instead of from whatever
                    // shape it happened to have when it died
                    let resurrection_layout = if let Some(snapshot) = snapshot_to_restore.as_ref() {
                        report_snapshot_version_drift(snapshot);
                        match snapshot.layout() {
                            Ok(layout) => Some(layout),
                            Err(e) => {
                                eprintln!("Cannot restore snapshot {}: {}", snapshot.id, e);
                                process::exit(2);
                            },
                        }
                    } else if no_resurrect {
                        // the snapshot is discarded rather than merely ignored: leaving it on disk
                        // would keep the session name taken by a dead session and block the fresh
                        // start the flag asks for
                        if !session_exists {
                            if let Some(session_name) = session_name.as_ref() {
                                discard_resurrection_snapshot(session_name);
                            }
                        }
                        None
                    } else {
                        session_name
                            .as_ref()
                            .and_then(|s| match resurrection_layout(&s) {
                                Ok(layout) => layout,
                                Err(e) => {
                                    eprintln!("{}", e);
                                    process::exit(2);
                                },
                            })
                    };
                    if (create || should_create_detached)
                        && !session_exists
                        && resurrection_layout.is_none()
                    {
                        session_name.clone().map(start_client_plan);
                    }
                    match (session_name.as_ref(), resurrection_layout) {
                        (Some(session_name), Some(mut resurrection_layout)) if !session_exists => {
                            if force_run_commands {
                                resurrection_layout.recursively_add_start_suspended(Some(false));
                            }
                            let path_to_layout = match snapshot_to_restore.as_ref() {
                                Some(snapshot) => snapshot.layout_file(),
                                None => session_layout_cache_file_name(session_name.as_ref()),
                            };
                            ClientInfo::Resurrect(
                                session_name.clone(),
                                path_to_layout,
                                force_run_commands,
                                new_session_cwd.clone(),
                            )
                        },
                        _ => attach_with_session_name(
                            session_name,
                            config_options.clone(),
                            create || should_create_detached,
                        ),
                    }
                };

                if let Ok(val) = std::env::var(envs::SESSION_NAME_ENV_KEY) {
                    if val == *client.get_session_name() {
                        panic!("You are trying to attach to the current session (\"{}\"). This is not supported.", val);
                    }
                }

                // an attach joins a server that may predate this binary. Said once per client, and
                // only for a session that already exists - a new one is this build by definition.
                if let ClientInfo::Attach(session_name, _) = &client {
                    zellij_utils::session_lifecycle::warn_if_server_build_differs(session_name);
                }

                if let Some(layout_info) = layout_info {
                    client.set_layout_info(layout_info);
                }

                if let Some(new_session_cwd) = new_session_cwd {
                    client.set_cwd(new_session_cwd);
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
                    ClientInfo::New(session_name, layout_info, new_session_cwd),
                    None,
                    None,
                    is_a_reconnect,
                    should_create_detached,
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
                            let client = attach_with_session_name(
                                Some(session_name.clone()),
                                config_options.clone(),
                                true,
                            );
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
                            );
                        },
                        _ => {
                            start_client_plan(session_name.clone());
                            reconnect_to_session = start_client_impl(
                                Box::new(os_input),
                                opts,
                                config,
                                config_options.clone(),
                                ClientInfo::New(session_name.clone(), layout_info, new_session_cwd),
                                None,
                                None,
                                is_a_reconnect,
                                should_create_detached,
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
                    ClientInfo::New(session_name, layout_info, new_session_cwd),
                    None,
                    None,
                    is_a_reconnect,
                    should_create_detached,
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

    // Resolve the session name to watch
    let client_info = match &session_name {
        Some(prefix) => match match_session_name(prefix).unwrap() {
            SessionNameMatch::UniquePrefix(s) | SessionNameMatch::Exact(s) => {
                ClientInfo::Watch(s, config_options.clone())
            },
            SessionNameMatch::AmbiguousPrefix(sessions) => {
                eprintln!(
                    "Ambiguous selection: multiple sessions names start with '{}':",
                    prefix
                );
                print_sessions(
                    sessions
                        .iter()
                        .map(|s| (s.clone(), Duration::default(), false))
                        .collect(),
                    false,
                    false,
                    true,
                );
                process::exit(1);
            },
            SessionNameMatch::None => {
                eprintln!("No session with the name '{}' found!", prefix);
                process::exit(1);
            },
        },
        None => match get_active_session() {
            ActiveSession::None => {
                eprintln!("No active zellij sessions found.");
                process::exit(1);
            },
            ActiveSession::One(name) => ClientInfo::Watch(name, config_options.clone()),
            ActiveSession::Many => {
                eprintln!("Please specify the session name to watch.");
                process::exit(1);
            },
        },
    };

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
