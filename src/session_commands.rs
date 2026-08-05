//! `zellij session up|down|restart` - the whole life of one named session.
//!
//! These were three shell scripts before they were three subcommands, and moving them into the
//! binary is what fixes them. A script has to be told where the sockets live, which it learns from
//! the environment; a shell that started before the last correction keeps the old answer, panes
//! inherit it from the server, and the machine ends up with two servers for one name in two
//! directories, each invisible to the other. The binary already knows: `ZELLIJ_SOCK_DIR` resolves
//! itself, honouring an explicit override and deriving the value otherwise. Nothing here reads the
//! environment for it, and nothing here needs a file to be sourced first.
//!
//! What survives from the scripts is the discipline: every one of these commands states a
//! post-condition and checks it. See [`zellij_utils::session_lifecycle`].

use std::process;
use std::time::{Duration, Instant};

use zellij_utils::cli::{CliArgs, Command, SessionLifecycleCli, Sessions};
use zellij_utils::envs;
use zellij_utils::session_lifecycle::{env_vars_to_drop, DownOutcome, SessionFacts};
use zellij_utils::sessions::{delete_session_reporting, validate_session_name, KillWait};

use crate::commands::{get_config_options_from_cli_args, snapshot_settings, start_client};

/// How long to wait for a freshly requested server to appear before calling the creation a failure.
/// `attach --create-background` returns as soon as the spawn is requested, not once it has happened.
const SERVER_APPEARANCE_TIMEOUT: Duration = Duration::from_secs(10);
const SERVER_APPEARANCE_POLL_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) fn session_lifecycle_command(cli: SessionLifecycleCli, opts: CliArgs) {
    match cli {
        SessionLifecycleCli::Up {
            session_name,
            restore,
        } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(match up(&name, restore, &opts) {
                Ok(()) => 0,
                Err(()) => 1,
            });
        },
        SessionLifecycleCli::Down {
            session_name,
            wait_timeout,
        } => {
            let name = resolve_session_name(session_name, &opts, false);
            refuse_from_inside(&name);
            process::exit(match down(&name, wait_timeout, &opts) {
                Ok(()) => 0,
                Err(()) => 1,
            });
        },
        SessionLifecycleCli::Restart {
            session_name,
            fresh,
            restore,
            wait_timeout,
        } => {
            let name = resolve_session_name(session_name, &opts, true);
            // the pre-restart shape is what a restart is almost always for, so restoring it is the
            // default and --fresh is the deliberate exception, for picking up a layout edit
            let restore = if fresh {
                None
            } else {
                Some(restore.unwrap_or_else(|| "latest".to_owned()))
            };
            restart(&name, restore, wait_timeout, &opts);
        },
    }
}

/// The name to act on: what was asked for, else the session this shell is in (for `restart`, which
/// is the one command that means "this one"), else `--session`, else the `session_name` config
/// option. There is no built-in default: a lifecycle command that guesses the name is a lifecycle
/// command that eventually kills the wrong session.
fn resolve_session_name(
    session_name: Option<String>,
    opts: &CliArgs,
    prefer_current_session: bool,
) -> String {
    let name = session_name
        .or_else(|| {
            if prefer_current_session {
                envs::get_session_name().ok()
            } else {
                None
            }
        })
        .or_else(|| opts.session.clone())
        .or_else(|| {
            get_config_options_from_cli_args(opts)
                .ok()
                .and_then(|options| options.session_name)
        });
    let Some(name) = name else {
        eprintln!(
            "No session name given, and no `session_name` is set in the config. \
             Name the session to act on."
        );
        process::exit(2);
    };
    if let Err(e) = validate_session_name(&name) {
        eprintln!("{}", e);
        process::exit(2);
    }
    name
}

/// Tearing a session down kills every pane shell in it, including the one running the command, so a
/// `down` typed inside the session it names would never come back to report anything. `restart`
/// leaves the process tree before it does the same thing, and is the supported way in.
fn refuse_from_inside(name: &str) {
    if envs::get_session_name().ok().as_deref() == Some(name) {
        eprintln!(
            "Refusing to tear down '{}' from inside it - this shell would be killed mid-run.",
            name
        );
        eprintln!(
            "  Detach first and re-run, or use `zellij session restart`, which detaches itself."
        );
        process::exit(2);
    }
}

/// Create the session if it is not there, then prove that it is.
fn up(name: &str, restore: Option<String>, opts: &CliArgs) -> Result<(), ()> {
    let facts = SessionFacts::collect(name);
    let healthy = facts.assert_up().is_ok();

    if healthy && restore.is_none() {
        println!("ok    session '{}' already running", name);
        return Ok(());
    }
    if healthy || facts.listed {
        if restore.is_some() {
            eprintln!(
                "Session '{}' is running, so there is nothing to restore into.",
                name
            );
            process::exit(2);
        }
    }
    // Building a second server for a name that already has one is how the invisible duplicates were
    // made in the first place. If something is already serving this name and the post-condition
    // still does not hold, the fault is the thing to report - not a third server.
    if !facts.servers.is_empty() && !healthy {
        eprintln!(
            "session up: refusing to create a second server for '{}'",
            name
        );
        facts.print_diagnostics();
        return Err(());
    }

    let mut opts = opts.clone();
    opts.session = None;
    opts.command = Some(Command::Sessions(Sessions::Attach {
        session_name: Some(name.to_owned()),
        create: true,
        create_background: true,
        force_run_commands: false,
        no_resurrect: false,
        restore: restore.clone(),
        index: None,
        options: None,
        token: None,
        remember: false,
        forget: false,
        ca_cert: None,
        insecure: false,
    }));
    start_client(opts);

    let facts = wait_for_server(name);
    if let Err(reason) = facts.assert_up() {
        eprintln!("session up: post-condition FAILED - {}", reason);
        facts.print_diagnostics();
        return Err(());
    }
    println!("up    session '{}' in {}", name, facts.socket_dir.display());
    if let Some(id) = restore {
        println!("      restored from snapshot {}", id);
    }
    Ok(())
}

/// Poll until the session looks up, or until the timeout says it never will.
fn wait_for_server(name: &str) -> SessionFacts {
    let deadline = Instant::now() + SERVER_APPEARANCE_TIMEOUT;
    loop {
        let facts = SessionFacts::collect(name);
        if facts.assert_up().is_ok() || Instant::now() >= deadline {
            return facts;
        }
        std::thread::sleep(SERVER_APPEARANCE_POLL_INTERVAL);
    }
}

/// Remove the session, then prove it is gone.
///
/// `delete-session` archives the session's shape on its way out and waits for the server process to
/// have actually exited, so nothing here polls and nothing here kills. A server that outlives its
/// delete is a fault to report, not a thing to paper over: a blind reap is what once left a server
/// running for twelve minutes, still parenting four pane shells, while every caller believed the
/// session was down.
///
/// Finding nothing to remove is not one of those faults, which is where this parts company with
/// `delete-session`: the name asked about is down, which is the state that was asked for. Only a
/// name something is still serving fails.
fn down(name: &str, wait_timeout: u64, opts: &CliArgs) -> Result<(), ()> {
    let facts = SessionFacts::collect(name);
    if facts.assert_down().is_ok() && !facts.listed {
        println!(
            "ok    session '{}' is already down; nothing to remove",
            name
        );
        return Ok(());
    }

    let no_wait = false;
    let deleted = delete_session_reporting(
        name,
        true,
        &snapshot_settings(opts),
        KillWait::from_cli(no_wait, wait_timeout),
    );

    let facts = SessionFacts::collect(name);
    match DownOutcome::judge(deleted, facts.assert_down()) {
        DownOutcome::Failed(reason) => {
            eprintln!("session down: post-condition FAILED - {}", reason);
            facts.print_diagnostics();
            eprintln!("  nothing was force-killed; inspect the above before retrying.");
            Err(())
        },
        DownOutcome::NothingToRemove => {
            println!("ok    session '{}' is down; nothing to remove", name);
            Ok(())
        },
        DownOutcome::Removed => {
            println!(
                "down  session '{}' removed; snapshot archived (zellij snapshot list --session {})",
                name, name
            );
            Ok(())
        },
    }
}

/// Unset what `session_restart_drop_env` names, so the rebuilt session does not hand it out.
///
/// The environment a restart inherits is the environment of the pane it was typed in, and the
/// session built from it gives that environment to every pane in it. A variable that describes the
/// one program that asked for the restart would then describe all of them, and programs that read
/// it would believe something about their pane that is not true. Called once the restart is a
/// session of its own and before anything is rebuilt, so nothing carries the old values across.
fn drop_configured_env(opts: &CliArgs) {
    let Some(patterns) = get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.session_restart_drop_env)
    else {
        return;
    };
    let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    for name in env_vars_to_drop(&patterns, names.iter().map(|name| name.as_str())) {
        println!("      dropping {} from the rebuilt session", name);
        std::env::remove_var(name);
    }
}

/// Take the session down and bring it back, from inside it or from anywhere else.
///
/// The work has to outlive the session it is destroying: the teardown kills every pane shell,
/// including whichever one asked for the restart. Leaving the session's process tree first is the
/// whole trick, and a background job is not enough - a pane's children are signalled as a process
/// group and SIGKILLed shortly after. The double fork plus `setsid` that daemonizes the server is
/// reused here for the same reason it exists there.
#[cfg(unix)]
fn restart(name: &str, restore: Option<String>, wait_timeout: u64, opts: &CliArgs) -> ! {
    use zellij_utils::consts::ZELLIJ_STATE_DIR;

    let log_file = ZELLIJ_STATE_DIR.join("restart.log");
    eprintln!("detaching; output -> {}", log_file.display());

    if let Err(e) = std::fs::create_dir_all(&*ZELLIJ_STATE_DIR) {
        eprintln!("Failed to create {}: {}", ZELLIJ_STATE_DIR.display(), e);
        process::exit(1);
    }
    // Keep one generation: a restart that goes wrong is usually diagnosed after the next one has
    // already overwritten the log.
    if log_file.exists() {
        let _ = std::fs::rename(&log_file, log_file.with_extension("log.1"));
    }
    let (stdout, stderr) = match (
        std::fs::File::create(&log_file),
        std::fs::OpenOptions::new().append(true).open(&log_file),
    ) {
        (Ok(stdout), Ok(stderr)) => (stdout, stderr),
        _ => {
            eprintln!("Failed to open {}", log_file.display());
            process::exit(1);
        },
    };

    let working_directory = std::env::current_dir().unwrap_or_else(|_| "/".into());
    if let Err(e) = daemonize::Daemonize::new()
        .working_directory(working_directory)
        .stdout(stdout)
        .stderr(stderr)
        .start()
    {
        eprintln!("Failed to detach from this session: {}", e);
        process::exit(1);
    }

    // Past this point we are a session of our own, and the pane we came from is about to die.
    // zellij reads "the session I am in" from these; left in place they make `down` refuse and the
    // recreated session's panes believe they are nested inside a session that no longer exists.
    for var in [
        envs::ZELLIJ_ENV_KEY,
        envs::SESSION_NAME_ENV_KEY,
        "ZELLIJ_PANE_ID",
    ] {
        std::env::remove_var(var);
    }
    drop_configured_env(opts);

    if down(name, wait_timeout, opts).is_err() {
        eprintln!("teardown failed; NOT recreating the session.");
        process::exit(1);
    }
    process::exit(match up(name, restore, opts) {
        Ok(()) => 0,
        Err(()) => 1,
    });
}

/// Windows has no fork, and no process group to escape from: the caller's shell is not a pane shell
/// the server is about to kill.
#[cfg(not(unix))]
fn restart(name: &str, restore: Option<String>, wait_timeout: u64, opts: &CliArgs) -> ! {
    drop_configured_env(opts);
    if down(name, wait_timeout, opts).is_err() {
        eprintln!("teardown failed; NOT recreating the session.");
        process::exit(1);
    }
    process::exit(match up(name, restore, opts) {
        Ok(()) => 0,
        Err(()) => 1,
    });
}
