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
use zellij_utils::session_lifecycle::{
    colorterm_for_new_session, env_vars_to_drop, lock_up, term_for_new_session, DownOutcome,
    SessionFacts,
};
use zellij_utils::sessions::{delete_session_reporting, validate_session_name, KillWait};

use crate::commands::{get_config_options_from_cli_args, snapshot_settings, start_client};

/// How long to wait for a freshly requested server to appear before calling the creation a failure.
/// `attach --create-background` returns as soon as the spawn is requested, not once it has happened.
const SERVER_APPEARANCE_TIMEOUT: Duration = Duration::from_secs(10);
/// The first gap between polls. Short, because a server that is going to appear usually has.
const SERVER_APPEARANCE_FIRST_POLL: Duration = Duration::from_millis(50);
/// The longest gap. Each poll forks `ps` to walk the whole process table, and a fixed short
/// interval spent the same hundred forks on the session that came up in 200ms and on the one that
/// was never going to - and the second is the case the watchdog repeats every minute, forever.
const SERVER_APPEARANCE_MAX_POLL: Duration = Duration::from_millis(1500);

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
    // Held for the rest of this function, which is what makes `up` idempotent under concurrency:
    // the check and the creation are one step, so a `restart` overlapping the watchdog's minute
    // tick waits here and then finds the session already up. See `session_lifecycle::lock_up`.
    let _up_lock = lock_up(name);

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

    // Everything from here to `start_client` is the environment the new session is built with, and
    // only on the path that creates it: the server takes this process's environment and hands it to
    // every pane shell, so what is set here is set in every pane for the life of the session. An
    // `up` that found the session healthy has already returned, so nothing here touches a session
    // that exists, and `restart` ends in this function and is covered by it.

    // "Which session am I in" is read from these three. A launcher's environment has none - but
    // `systemctl --user import-environment` and `dbus-update-activation-environment --systemd`, run
    // from inside a pane as a desktop session ordinarily does, put a pane's copies into the user
    // manager's environment, and from there into this unit. zellij would then believe it is running
    // INSIDE a session, refuse to attach, and the timer would repeat that every minute forever.
    // `restart` scrubs them for its own reason - it is about to destroy the session it was typed
    // in - and this is the same scrub for the creator that never had a pane. It is also what makes
    // an `UnsetEnvironment=` directive in the unit unnecessary rather than merely permitted.
    for var in [
        envs::ZELLIJ_ENV_KEY,
        envs::SESSION_NAME_ENV_KEY,
        "ZELLIJ_PANE_ID",
    ] {
        std::env::remove_var(var);
    }
    // A stale agent socket is worse than none, and the server hands its environment to every pane
    // for the life of the session - so one dead path here is `git push` failing with "Permission
    // denied (publickey)" in every pane, beside a terminal where it works. See
    // `session_lifecycle::ssh_auth_sock_is_dangling`. Nothing is invented in its place: this
    // machine cannot know where an agent would be, and a wrong path is the fault being removed.
    let ssh_auth_sock = std::env::var("SSH_AUTH_SOCK").ok();
    if zellij_utils::session_lifecycle::ssh_auth_sock_is_dangling(ssh_auth_sock.as_deref()) {
        println!(
            "      SSH_AUTH_SOCK names a socket that is not there ({}); dropping it rather than \
             handing it to every pane",
            ssh_auth_sock.unwrap_or_default()
        );
        std::env::remove_var("SSH_AUTH_SOCK");
    }

    warn_if_copy_command_has_no_display(opts);

    // The configured drop-list is about the same hazard from the other end: a variable describing
    // the ONE program that asked for this would otherwise describe every pane of the session. It
    // reads as restart-specific and is not - `session up other-session` typed in an agent's pane
    // bakes the agent's variables into a session it has nothing to do with.
    drop_configured_env(opts);

    // A launcher has no TERM and no COLORTERM - see
    // `zellij_utils::session_lifecycle::term_for_new_session`. Both describe the CONNECTION rather
    // than the user, which is what makes them the generator's business at all: the rc chain in each
    // pane re-derives a locale or a PATH for itself and cannot re-derive these.
    if let Some(term) = term_for_new_session(std::env::var("TERM").ok().as_deref()) {
        println!("      no usable TERM here; the session gets TERM={}", term);
        std::env::set_var("TERM", term);
    }
    if let Some(colorterm) = colorterm_for_new_session(std::env::var("COLORTERM").ok().as_deref()) {
        std::env::set_var("COLORTERM", colorterm);
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
///
/// The gap doubles, because every poll costs a `ps` over the whole process table and the two cases
/// want opposite things from it. A session that comes up does so in the first few hundred
/// milliseconds and wants to be noticed at once; a session that is never coming up spends the rest
/// of the ten seconds proving it, and that is the case a launcher REPEATS every minute for as long
/// as the fault lasts. Backing off cuts the forks on the failing machine by about eight to one and
/// costs the healthy one nothing.
///
/// Nothing here gives up early or escalates, deliberately. This function's whole answer is "the
/// post-condition does not hold yet", and its caller already reports that loudly and with
/// diagnostics - to the journal on systemd, to the log the plist names on launchd. What would
/// escalate is the watchdog deciding to stop retrying, and a watchdog that switches itself off is
/// the one behaviour a person cannot recover from without a shell on the machine.
fn wait_for_server(name: &str) -> SessionFacts {
    let deadline = Instant::now() + SERVER_APPEARANCE_TIMEOUT;
    let mut gap = SERVER_APPEARANCE_FIRST_POLL;
    loop {
        let facts = SessionFacts::collect(name);
        if facts.assert_up().is_ok() || Instant::now() >= deadline {
            return facts;
        }
        std::thread::sleep(gap);
        gap = (gap * 2).min(SERVER_APPEARANCE_MAX_POLL);
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

/// Say so when the session about to be created has a `copy_command` and no display.
///
/// `copy_command` runs in the SERVER, with the environment the session was created with, for the
/// session's whole life - see `session_lifecycle::copy_command_has_no_display`. A launcher has
/// neither display variable, so `wl-copy` or `xclip` exits non-zero on every copy and the only
/// place that goes is `log::error!`: from inside, copy silently does nothing in a session where
/// everything else works.
///
/// Not on macOS, where the ordinary `copy_command` is `pbcopy` and neither variable exists on any
/// machine - the warning would be wrong on every Mac. Not on Windows either, which has no display
/// variables to be missing.
#[cfg(all(unix, not(target_os = "macos")))]
fn warn_if_copy_command_has_no_display(opts: &CliArgs) {
    use zellij_utils::session_lifecycle::{copy_command_has_no_display, DISPLAY_ENV_NAMES};

    let Some(copy_command) = get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.copy_command)
    else {
        return;
    };
    let names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
    if !copy_command_has_no_display(Some(&copy_command), names.iter().map(|name| name.as_str())) {
        return;
    }
    eprintln!(
        "warning: `copy_command` is set to `{}`, and none of {} is in the environment this\n         \
         session is being created with. copy_command runs in the SERVER with that environment\n         \
         for the life of the session, so if the command talks to X or Wayland every copy will\n         \
         fail and the only record will be a line in the server log.\n         \
         Give the launcher the variable it needs - a `session_service` extra sets one - or\n         \
         create the session from the graphical login.",
        copy_command,
        DISPLAY_ENV_NAMES.join(" or ")
    );
}

#[cfg(not(all(unix, not(target_os = "macos"))))]
fn warn_if_copy_command_has_no_display(_opts: &CliArgs) {}

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
