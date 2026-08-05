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

use std::path::PathBuf;
use std::process;
use std::time::{Duration, Instant};

use zellij_utils::cli::{CliArgs, Command, SessionLifecycleCli, Sessions};
use zellij_utils::envs;
use zellij_utils::session_lifecycle::{
    colorterm_for_new_session, env_vars_to_drop, term_for_new_session,
    warn_if_server_build_differs, DownOutcome, SessionFacts,
};
use zellij_utils::session_service::{
    self, path_dirs, resolve_service_exe, DisableOutcome, EnableOutcome, PlistValue, ServiceExe,
    ServiceKind, SessionServiceOptions,
};
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
        SessionLifecycleCli::Enable {
            session_name,
            exe,
            force,
        } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(match enable(&name, exe, force, &opts) {
                Ok(()) => 0,
                Err(()) => 1,
            });
        },
        SessionLifecycleCli::Disable { session_name } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(match disable(&name) {
                Ok(()) => 0,
                Err(()) => 1,
            });
        },
        SessionLifecycleCli::Status { session_name, exe } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(status(&name, exe, &opts));
        },
    }
}

/// The init system of this machine, or a refusal naming what it would have driven.
fn native_service_kind() -> Result<ServiceKind, ()> {
    session_service::native_service_kind().ok_or_else(|| {
        eprintln!(
            "session enable: no init system this build can install into. \
             `zellij setup --generate-service` still prints a unit to adapt by hand."
        );
    })
}

/// Which binary the unit should run, warning when the answer is one an upgrade can break.
fn service_exe(explicit: Option<PathBuf>) -> PathBuf {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
    let exe = resolve_service_exe(explicit, &current_exe, &path_dirs());
    if let ServiceExe::Resolved(path) = &exe {
        eprintln!(
            "warning: no `zellij` on PATH resolves to this binary, so the unit will run\n  \
             {}\nwhich is where this binary actually is. If that is inside a version-specific\n\
             directory, the unit will break the next time zellij is upgraded. Name a stable\n\
             path with `--exe <PATH>` if it is.",
            path.display()
        );
    }
    exe.path().to_path_buf()
}

/// What the config adds to the generated unit, if anything.
///
/// A generated unit knows the binary, the session and the schedule; everything local - an ordering
/// against another service, a nice level - comes from here. The config is where it lives so that
/// the tool can see it: a systemd drop-in would work and `zellij session status` could never
/// report it.
fn configured_extras(opts: &CliArgs) -> Option<SessionServiceOptions> {
    get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.session_service)
}

/// Write the unit and hand it to the init system.
///
/// The whole command is idempotent, because the thing it installs is: `zellij session up` over a
/// healthy session is a no-op, so re-enabling costs nothing and enabling twice is not an error to
/// report but a state to confirm.
fn enable(name: &str, exe: Option<PathBuf>, force: bool, opts: &CliArgs) -> Result<(), ()> {
    let kind = native_service_kind()?;
    let exe = service_exe(exe);
    let extras = configured_extras(opts);
    match session_service::enable(kind, &exe, name, extras.as_ref(), force) {
        Ok(EnableOutcome::AlreadyEnabled) => {
            println!("ok    service for '{}' is already enabled", name);
            Ok(())
        },
        Ok(EnableOutcome::Enabled { written, beside }) => {
            for path in written {
                println!("      wrote {}", path.display());
            }
            // only reachable with --force: two launchers for one session race at login, and the
            // one that loses is left failed. Say so where the person who typed --force sees it.
            for job in beside {
                eprintln!(
                    "warning: {} also runs `session up {}` ({}); both will start at login",
                    job.name,
                    name,
                    job.path.display()
                );
            }
            println!("on    service for '{}' enabled and started", name);
            Ok(())
        },
        Err(reason) => {
            eprintln!("session enable: {}", reason);
            Err(())
        },
    }
}

/// Unload the unit, then remove it.
fn disable(name: &str) -> Result<(), ()> {
    let kind = native_service_kind()?;
    match session_service::disable(kind, name) {
        Ok(DisableOutcome::NotInstalled) => {
            println!(
                "ok    no service installed for '{}'; nothing to remove",
                name
            );
            Ok(())
        },
        Ok(DisableOutcome::NotOurs { jobs }) => {
            // `status` reports this job by name, so reporting "nothing installed" here would have
            // the two commands contradicting each other over the same machine. What this command
            // removes is what `session enable` wrote, and that is not this.
            println!(
                "ok    nothing zellij installed for '{}'; nothing of ours to remove",
                name
            );
            for job in jobs {
                println!(
                    "      but {} runs `session up {}` ({}) - not written by zellij, so it is left \
                     alone",
                    job.name,
                    name,
                    job.path.display()
                );
            }
            Ok(())
        },
        Ok(DisableOutcome::Disabled { removed, remaining }) => {
            for path in removed {
                println!("      removed {}", path.display());
            }
            println!(
                "off   service for '{}' unloaded and removed; the session itself is untouched",
                name
            );
            for job in remaining {
                println!(
                    "      {} still runs `session up {}` ({}) - not written by zellij, so it is \
                     left alone",
                    job.name,
                    name,
                    job.path.display()
                );
            }
            Ok(())
        },
        Err(reason) => {
            eprintln!("session disable: {}", reason);
            Err(())
        },
    }
}

/// Report the install, one fact per line.
///
/// Installed, loaded and running are three different states and they come apart in ways that each
/// mean something different: a file with no job is an install that was never loaded, a job with no
/// session is a unit that is failing, and a session with no job is one that will not come back.
/// Reporting them together as "ok" would hide exactly the case worth reporting.
///
/// Exits 0 when the unit is installed AND loaded, whatever the session is doing - the session is
/// the thing the unit repairs, and `zellij session up` is the command that reports on it.
fn status(name: &str, exe: Option<PathBuf>, opts: &CliArgs) -> i32 {
    let Ok(kind) = native_service_kind() else {
        return 1;
    };
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
    let exe = resolve_service_exe(exe, &current_exe, &path_dirs());
    let extras = configured_extras(opts);
    let status = match session_service::status(kind, exe.path(), name, extras.as_ref()) {
        Ok(status) => status,
        Err(reason) => {
            eprintln!("session status: {}", reason);
            return 1;
        },
    };

    println!("session   {}", name);
    println!("init      {}", status.kind.name());
    // A job that runs `session up` for this session under another name is doing the work, so the
    // file this build would have written being absent is not the fault it looks like. It is
    // reported against the first file, which is the one that runs the command - the timer beside it
    // on systemd is this build's own arrangement and says nothing about someone else's.
    let elsewhere = status.installed_as.first();
    for (index, file) in status.files.iter().enumerate() {
        let state = match (file.present, file.stale, index == 0, elsewhere) {
            (false, _, true, Some(job)) => format!(
                "installed under a different name: {} ({})",
                job.name,
                job.path.display()
            ),
            (false, ..) => "missing".to_owned(),
            (true, true, ..) => {
                "installed (differs from what `session enable` would write now)".to_owned()
            },
            (true, false, ..) => "installed".to_owned(),
        };
        println!("{:9} {} - {}", file.role, file.path.display(), state);
    }
    let installed = match elsewhere {
        // this build did not write that install and cannot say which files belong to it, so
        // whether the init system holds the job is the whole of what can be judged
        Some(_) => true,
        None => status.files.iter().all(|file| file.present),
    };
    for other in status.installed_as.iter().skip(1) {
        println!(
            "{:9} {} also runs `session up {}` ({})",
            "ambiguous",
            other.name,
            name,
            other.path.display()
        );
    }
    println!(
        "loaded    {} ({})",
        if status.loaded { "yes" } else { "no" },
        status.load_detail
    );

    // what the config adds is reported here because it can be: this is the whole argument for
    // keeping it in the config rather than in a drop-in directory the tool cannot see
    print_configured_extras(kind, extras.as_ref());

    let facts = SessionFacts::collect(name);
    match facts.assert_up() {
        Ok(()) => println!("running   yes, in {}", facts.socket_dir.display()),
        Err(reason) => println!("running   no - {}", reason),
    }

    if installed && status.loaded {
        0
    } else {
        1
    }
}

/// List what `session_service` in the config puts into the unit, for the init system in use.
fn print_configured_extras(kind: ServiceKind, extras: Option<&SessionServiceOptions>) {
    let Some(extras) = extras.filter(|extras| !extras.is_empty()) else {
        println!("config    no session_service extras");
        return;
    };
    match kind {
        ServiceKind::Systemd => {
            let sections = [
                ("Unit", &extras.systemd.unit),
                ("Service", &extras.systemd.service),
                ("Install", &extras.systemd.install),
            ];
            for (section, directives) in sections {
                for directive in directives {
                    println!("config    [{}] {}", section, directive);
                }
            }
        },
        ServiceKind::Launchd => {
            for key in &extras.launchd {
                let value = match &key.value {
                    PlistValue::String(value) => value.to_owned(),
                    PlistValue::Integer(value) => value.to_string(),
                    PlistValue::Bool(value) => value.to_string(),
                };
                println!("config    {} = {}", key.name, value);
            }
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
        // "already running" is exactly the answer that hides a superseded build: an `up` after an
        // upgrade reports success and leaves the old server serving the session.
        warn_if_server_build_differs(name);
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

    // Which macOS session domain a server is created in is decided once and inherited by every
    // pane, so `up` from a shell that is not in the graphical session hands that decision to
    // launchd rather than making it here. See `zellij_utils::session_lifecycle`.
    #[cfg(target_os = "macos")]
    match zellij_utils::session_lifecycle::ensure_gui_session_domain(name, facts.listed) {
        Ok(true) => {
            let facts = wait_for_server(name);
            if let Err(reason) = facts.assert_up() {
                eprintln!("session up: post-condition FAILED - {}", reason);
                facts.print_diagnostics();
                return Err(());
            }
            println!(
                "up    session '{}' in {} (started in the graphical session)",
                name,
                facts.socket_dir.display()
            );
            return Ok(());
        },
        Ok(false) => {},
        Err(reason) => {
            eprintln!("session up: {}", reason);
            return Err(());
        },
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
