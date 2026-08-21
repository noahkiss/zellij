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
    colorterm_for_new_session, env_vars_to_drop, lock_up, term_for_new_session,
    warn_if_server_build_differs, DownOutcome, SessionFacts,
};
use zellij_utils::session_service::{
    self, configured_pinned_exe, path_dirs, resolve_service_exe, DisableOutcome, EnableOutcome,
    PinState, PlistValue, ServiceExe, ServiceKind, SessionServiceOptions, UnitDrift,
};
use zellij_utils::sessions::{delete_session_reporting, validate_session_name, KillWait};

use crate::commands::{get_config_options_from_cli_args, snapshot_settings, start_client};

/// How long to wait for a freshly requested server to appear before calling the creation a failure.
/// `attach --create-background` returns as soon as the spawn is requested, not once it has happened.
///
/// Thirty seconds because launchd was measured at 15 to 20 on the fleet's Macs. The old ten
/// reported a post-condition failure on both of them during an upgrade, on sessions that were up
/// moments later - and a false failure on a healthy machine is worse than a slow true one, because
/// it is the shape a real fault takes and it teaches everyone to ignore the line.
const SERVER_APPEARANCE_TIMEOUT: Duration = Duration::from_secs(30);
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
            fresh,
        } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(match up(&name, up_shape(fresh, restore), &opts) {
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
            restart(
                &name,
                restart_restore_target(fresh, restore),
                wait_timeout,
                &opts,
            );
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
            process::exit(match disable(&name, &opts) {
                Ok(()) => 0,
                Err(()) => 1,
            });
        },
        SessionLifecycleCli::Status { session_name, exe } => {
            let name = resolve_session_name(session_name, &opts, false);
            process::exit(status(&name, exe, &opts));
        },
        SessionLifecycleCli::Doctor {
            session_name,
            dry_run,
            fix: _,
            no_fix,
            sign: _,
            no_sign,
            exe,
        } => crate::session_doctor_command::session_doctor_command(
            session_name,
            dry_run,
            no_fix,
            no_sign,
            exe,
            opts,
        ),
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
fn service_exe(explicit: Option<PathBuf>, pinned: Option<PathBuf>) -> PathBuf {
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
    let exe = resolve_service_exe(explicit, pinned, &current_exe, &path_dirs());
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
pub(crate) fn configured_extras(opts: &CliArgs) -> Option<SessionServiceOptions> {
    get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.session_service)
}

/// The `session_name` the CONFIG states, which is the name `managed_session` is about.
///
/// Deliberately not [`resolve_session_name`], which answers "which session does this command mean"
/// and prefers the one it was typed in. The question here is the other one: which name has this
/// machine handed to its init system, whatever the caller happens to be asking about.
pub(crate) fn configured_session_name(opts: &CliArgs) -> Option<String> {
    get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.session_name)
}

/// Whether this command is about the session the init system owns.
pub(crate) fn session_is_managed(name: &str, opts: &CliArgs) -> bool {
    zellij_utils::session_lifecycle::is_managed_session_name(
        session_service::configured_managed_session(configured_extras(opts).as_ref()),
        configured_session_name(opts).as_deref(),
        name,
    )
}

/// Put this build at the pinned path for a command that is about to name that path in a unit.
///
/// A failure is fatal only when there is nothing at the path yet: a unit naming a binary that does
/// not exist cannot be run by anything. When a build IS there it still runs, so the failure is a
/// stale copy rather than a broken install, and the next `session up` writes it again.
fn pin_before_writing_the_unit(pinned: &PathBuf) -> Result<(), ()> {
    let existed = pinned.exists();
    match pin_this_build_at(pinned) {
        Ok(()) => Ok(()),
        Err(reason) if existed => {
            eprintln!("warning: {}", reason);
            eprintln!(
                "         the build already at {} is what the unit will run",
                pinned.display()
            );
            Ok(())
        },
        Err(reason) => {
            eprintln!("session enable: {}", reason);
            Err(())
        },
    }
}

/// Copy this build to `pinned`, reporting what that came to.
///
/// Silent when the copy is already this build, which is every pass but the first after an upgrade:
/// `session up` runs this each time, and a line saying nothing happened, every minute, from a
/// watchdog, is a line nobody reads.
///
/// **THIS BINARY is the source, and once the launcher runs the pin this binary IS the pin.** That
/// is the ordinary configuration, and in it there is nothing to do:
/// [`install_pinned_exe`](zellij_utils::session_lifecycle::install_pinned_exe) recognises a source
/// that is its own target and returns without touching the pin or its stamp. Nothing is wrong with
/// that - the process has not seen the package and has nothing to compare against - but it means
/// the watchdog is not what notices an upgrade. What does is any zellij run off another path: an interactive launch,
/// which resolves the server binary through here on the way past, or `session up`, `session doctor
/// --fix` or `session enable` typed in a shell, where `PATH` leads to the new build. See FORK.md,
/// "Once the launcher runs the pin".
#[cfg(unix)]
fn pin_this_build_at(pinned: &PathBuf) -> Result<(), String> {
    use zellij_utils::session_lifecycle::{install_pinned_exe, PinOutcome};

    let current_exe =
        std::env::current_exe().map_err(|e| format!("cannot find this binary to pin it: {}", e))?;
    match install_pinned_exe(&current_exe, pinned)? {
        PinOutcome::Installed(path) => println!("      pinned this build at {}", path.display()),
        PinOutcome::Refreshed(path) => {
            println!("      refreshed the pinned copy at {}", path.display())
        },
        PinOutcome::Signed(path) => {
            println!(
                "      refreshed and signed the pinned copy at {}",
                path.display()
            )
        },
        // the pin was left alone because this run could not sign it, and the sink has already said
        // so in full. A second line here would read as a second fault.
        PinOutcome::Kept(_) => {},
        PinOutcome::UpToDate(_) => {},
    }
    Ok(())
}

#[cfg(not(unix))]
fn pin_this_build_at(_pinned: &PathBuf) -> Result<(), String> {
    Ok(())
}

/// Keep the pinned copy current, on the path the INSTALLED UNIT records.
///
/// Not the path this process would derive: the canonical directory honours `XDG_DATA_HOME` on
/// Linux, and a launcher's environment is not the calling shell's, so a re-derived path can name a
/// different file from the one the launcher execs - and refreshing that one would leave the running
/// copy stale while reporting success.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn assert_pinned_exe(name: &str, extras: Option<&SessionServiceOptions>, opts: &CliArgs) {
    let Some(configured) = configured_pinned_exe(extras) else {
        return;
    };
    // The pin's writer signs for itself; what it cannot work out is where THIS run keeps its
    // config, and doctor resolves that the same way - so both flows leave the certificate backup
    // in one place.
    zellij_utils::session_signing::set_pin_signing_policy(
        zellij_utils::session_signing::PinSigningPolicy {
            allowed: true,
            backup_dir: opts
                .config_dir
                .clone()
                .or_else(zellij_utils::home::find_default_config_dir),
        },
    );
    let installed = session_service::installed_session_exe(name);
    match session_service::pin_state(&configured, installed.as_deref()) {
        PinState::Recorded(path) | PinState::Unrecorded(path) => {
            if let Err(reason) = pin_this_build_at(&path) {
                eprintln!("warning: {}", reason);
            }
        },
        PinState::Mismatch {
            configured,
            installed,
        } => eprintln!(
            "warning: `pin_exe` asks for a copy of zellij at\n           \
             {}\n         \
             but the launcher for '{}' runs\n           \
             {}\n         \
             Nothing was copied: what `session up` keeps current is the binary the launcher\n         \
             actually runs. Run `zellij session enable {}` to point it at the pinned path.\n         \
             On macOS a file-access grant is recorded against the exact path, so the grant has\n         \
             to name the path the launcher runs.",
            configured.display(),
            name,
            installed,
            name
        ),
    }
}

/// Nowhere else has a launcher this build installs into, so there is no recorded path and nothing
/// to keep current.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn assert_pinned_exe(_name: &str, _extras: Option<&SessionServiceOptions>, _opts: &CliArgs) {}

/// Write the unit and hand it to the init system.
///
/// The whole command is idempotent, because the thing it installs is: `zellij session up` over a
/// healthy session is a no-op, so re-enabling costs nothing and enabling twice is not an error to
/// report but a state to confirm.
fn enable(name: &str, exe: Option<PathBuf>, force: bool, opts: &CliArgs) -> Result<(), ()> {
    let kind = native_service_kind()?;
    print_unit_dir_disagreement();
    let extras = configured_extras(opts);
    // The copy goes in BEFORE the unit that names it. A unit whose binary does not exist is a unit
    // the init system cannot run, and the command that would have created the copy is the one that
    // unit runs - so leaving it to the first `session up` would leave nothing able to make the
    // first `session up` happen.
    let pinned = configured_pinned_exe(extras.as_ref());
    if let Some(pinned) = &pinned {
        pin_before_writing_the_unit(pinned)?;
    }
    let exe = service_exe(exe, pinned);
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

/// Name the two answers to "where do this user's units live", when they differ.
///
/// The unit goes where the MANAGER reads, which is the one that matters - but a person who then
/// looks under their own `XDG_CONFIG_HOME` will not find it, and a file that is not where its owner
/// expects is the beginning of a second install beside the first. So the difference is said out
/// loud at the moment it is acted on rather than left to be discovered.
fn print_unit_dir_disagreement() {
    let Some((manager, shell)) = session_service::unit_dir_disagreement() else {
        return;
    };
    eprintln!(
        "note: this shell's XDG_CONFIG_HOME and the systemd user manager's disagree.\n      \
         manager: {}\n      \
         shell:   {}\n      \
         The unit goes in the manager's directory, because that is the one it reads. A unit\n      \
         written to the other would load for nothing and `daemon-reload` would never see it.",
        manager.display(),
        shell.display()
    );
}

/// Unload the unit, then remove it.
fn disable(name: &str, opts: &CliArgs) -> Result<(), ()> {
    let kind = native_service_kind()?;
    // `managed_session` makes the unit unconditional, so removing it here removes it until the next
    // thing that wants the session writes it again. Said before the removal rather than after,
    // because it is the difference between this command doing what was asked and this command
    // looking like it did.
    if session_is_managed(name, opts) {
        println!(
            "      `session_service {{ managed_session true }}` is set for '{}', so the next \n      \
             `zellij session up` or `zellij -s {}` installs the unit again. Remove that key to \n      \
             make this stick.",
            name, name
        );
    }
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
                "warn  nothing zellij installed for '{}'; nothing of ours to remove",
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
            // Non-zero, because the question this command answers is "will the session come back",
            // and here it still will. A caller reading only the exit code would otherwise take
            // this for a session that has been switched off - and the next boot would disagree.
            eprintln!(
                "session disable: '{}' is still launched by a job zellij did not write; remove \
                 that job by hand to stop it",
                name
            );
            Err(())
        },
        Ok(DisableOutcome::Disabled {
            removed,
            remaining,
            unload_error,
        }) => {
            for path in removed {
                println!("      removed {}", path.display());
            }
            println!(
                "off   service for '{}' unloaded and removed; the session itself is untouched",
                name
            );
            for job in &remaining {
                println!(
                    "      {} still runs `session up {}` ({}) - not written by zellij, so it is \
                     left alone",
                    job.name,
                    name,
                    job.path.display()
                );
            }
            // Two ways this is a partial result rather than a success, and both leave the session
            // able to come back. Reported after the removals, because the removals happened.
            let mut clean = true;
            if let Some(reason) = &unload_error {
                eprintln!(
                    "session disable: the files are removed, but the init system did not accept \
                     every\n                 command: {}",
                    reason
                );
                clean = false;
            }
            // Removing our own unit while another launcher still starts the session is a partial
            // result too: the session keeps coming back, from something this command has just made
            // harder to find. Same reasoning as the NotOurs arm above.
            if !remaining.is_empty() {
                eprintln!(
                    "session disable: '{}' is still launched by a job zellij did not write; \
                     remove that job by hand to stop it",
                    name
                );
                clean = false;
            }
            if clean {
                Ok(())
            } else {
                Err(())
            }
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
    let extras = configured_extras(opts);
    let pinned = configured_pinned_exe(extras.as_ref());
    let exe = resolve_service_exe(exe, pinned.clone(), &current_exe, &path_dirs());
    let status = match session_service::status(kind, exe.path(), name, extras.as_ref()) {
        Ok(status) => status,
        Err(reason) => {
            eprintln!("session status: {}", reason);
            return 1;
        },
    };

    println!("session   {}", name);
    println!("init      {}", status.kind.name());
    print_unit_dir_disagreement();
    // A job that runs `session up` for this session under another name is doing the work, so the
    // file this build would have written being absent is not the fault it looks like. It is
    // reported against the first file, which is the one that runs the command - the timer beside it
    // on systemd is this build's own arrangement and says nothing about someone else's.
    //
    // The timer beside it is found the same way, through the service it starts rather than through
    // a name derived here - a watchdog somebody wrote by hand arms whatever service does the work,
    // and calling it missing tells a reader to install a second one.
    let elsewhere = status.installed_as.first();
    let timer_elsewhere = status.timer_installed_as.as_ref();
    for (index, file) in status.files.iter().enumerate() {
        let found_elsewhere = if file.role == "timer" {
            timer_elsewhere.map(|timer| (&timer.name, &timer.path))
        } else if index == 0 {
            elsewhere.map(|job| (&job.name, &job.path))
        } else {
            None
        };
        let state = match (file.present, file.stale, found_elsewhere) {
            (false, _, Some((name, path))) => format!(
                "installed under a different name: {} ({})",
                name,
                path.display()
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
        // a timer under another name arms the same service, so it stands in for the file this
        // build would have written
        None => status
            .files
            .iter()
            .all(|file| file.present || (file.role == "timer" && timer_elsewhere.is_some())),
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
    let unit_is_current = print_unit_drift(kind, exe.path(), name, extras.as_ref());
    let pinned_agrees = print_pin_state(name, pinned.as_deref());

    let facts = SessionFacts::collect(name);
    match facts.assert_up() {
        Ok(()) => println!("running   yes, in {}", facts.socket_dir.display()),
        Err(reason) => println!("running   no - {}", reason),
    }

    if installed && status.loaded && pinned_agrees && unit_is_current {
        0
    } else {
        1
    }
}

/// The `drift` line: whether the unit on disk is still what this config would write.
///
/// Reported, and counted against the exit code, because nothing else notices. Edit the config and
/// the loaded job does not change with it - the file is stale, the init system is still running the
/// definition it was handed, and every angle you can look from is internally consistent. It is only
/// wrong when the two are compared, which is the same shape as the pin mismatch reported below it.
///
/// The remedy has to be `session enable` and not a reload by hand, and launchd is why: a plist
/// whose CONTENT changed needs `bootout` then `bootstrap`, and `launchctl kickstart` restarts the
/// job from the definition launchd already holds - so the obvious command runs the old plist and
/// looks like the edit did nothing.
fn print_unit_drift(
    kind: ServiceKind,
    exe: &std::path::Path,
    name: &str,
    extras: Option<&SessionServiceOptions>,
) -> bool {
    match session_service::unit_drift(kind, exe, name, extras) {
        Ok(UnitDrift::NotInstalled) => {
            println!("drift     nothing zellij wrote is installed to compare against");
            true
        },
        Ok(UnitDrift::Current) => {
            println!("drift     none - the installed unit is what this config would write");
            true
        },
        Ok(UnitDrift::Drifted { paths }) => {
            for path in paths {
                println!(
                    "drift     {} is NOT what this config would write now",
                    path.display()
                );
            }
            println!(
                "drift     run `zellij session enable {}` to rewrite and reload it",
                name
            );
            if kind == ServiceKind::Launchd {
                println!(
                    "drift     a changed plist needs bootout then bootstrap, which that command \
                     does; `launchctl kickstart` restarts the job from the definition launchd \
                     already holds"
                );
            }
            false
        },
        Err(reason) => {
            eprintln!("session status: {}", reason);
            false
        },
    }
}

/// Say once that the installed unit no longer matches the config.
///
/// On `up`, because `up` is what runs - from a shell after a config edit, and from the launcher
/// every minute. The launcher's copy of this message is the one that reaches a machine nobody is
/// looking at, and it goes to the journal or to the log the plist names.
///
/// Silent unless something this build wrote is actually installed and actually differs: an `up` on
/// a machine with no launcher has nothing to say, and saying it every minute would be worse than
/// saying nothing.
fn warn_if_unit_drifted(name: &str, opts: &CliArgs) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        let Some(kind) = session_service::native_service_kind() else {
            return;
        };
        let extras = configured_extras(opts);
        let pinned = configured_pinned_exe(extras.as_ref());
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
        let exe = resolve_service_exe(None, pinned, &current_exe, &path_dirs());
        let Ok(UnitDrift::Drifted { paths }) =
            session_service::unit_drift(kind, exe.path(), name, extras.as_ref())
        else {
            return;
        };
        for path in paths {
            eprintln!(
                "warning: {} is not what `zellij session enable` would write now, so the loaded\n         \
                 job is running an older definition. Run `zellij session enable {}` to bring\n         \
                 them back together - a config edit does not reach a unit that was not rewritten.",
                path.display(),
                name
            );
        }
    });
}

/// Install or refresh the unit for a MANAGED session, so that nobody has to remember
/// `zellij session enable`.
///
/// `managed_session` means the init system owns the name, and a thing that is owned is not
/// something a person has to opt into once per machine and then keep in step by hand. So the
/// command that is about to NEED the unit is the command that writes it: `session enable` stops
/// being the step that turns the feature on and becomes "do it now", which is what it was always
/// doing anyway.
///
/// Two things it will not do. It never touches a name the config did not name, and it never runs
/// inside the unit - `session enable` STARTS what it installs, and a process the init system
/// started asking the init system to start it is the deadlock this guard exists for.
///
/// `session disable` still removes it. With the key set the next create puts it back, which is what
/// "unconditional" means; removing the key is how a removal is made to stick, and `disable` says so.
pub(crate) fn manage_the_unit(name: &str, opts: &CliArgs) {
    static MANAGED: std::sync::Once = std::sync::Once::new();
    MANAGED.call_once(|| {
        if !session_is_managed(name, opts)
            || zellij_utils::session_lifecycle::running_as_the_unit(name)
        {
            return;
        }
        let Some(kind) = session_service::native_service_kind() else {
            return;
        };
        let extras = configured_extras(opts);
        let pinned = configured_pinned_exe(extras.as_ref());
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
        let exe = resolve_service_exe(None, pinned, &current_exe, &path_dirs());
        let reason = match session_service::unit_drift(kind, exe.path(), name, extras.as_ref()) {
            Ok(UnitDrift::NotInstalled) => "it has no unit",
            Ok(UnitDrift::Drifted { .. }) => "its unit is not what this config would write",
            // Current, or a machine that could not be asked - either way there is nothing to write
            _ => return,
        };
        println!(
            "      `managed_session` is set and {}, so it is installed now",
            reason
        );
        let _ = enable(name, None, false, opts);
    });
}

/// Set while `session up` is the one creating the session.
///
/// `up` drives the same client path [`create_through_the_unit`] hooks into, and it has ALREADY
/// asked the init system its question by the time it gets there. Without this the client path would
/// ask a second time and report the answer twice, and on the arm where the init system says yes it
/// would hand `up` an attach where `up` wanted a detached create.
static UP_IS_THE_CREATOR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Hand the creation of a MANAGED session to the init system, and wait for what it makes.
///
/// `true` means the session is up and the caller is to attach to it rather than build a server.
///
/// This is the second caller the guards were written for and never got. Which macOS session domain
/// a server is created in, and which executable macOS holds responsible for it, are both decided
/// once by whoever creates the session - so a session created by typing `zellij` in a terminal is a
/// session the launch agent can never take back, however many times the watchdog runs afterwards.
/// [`zellij_utils::session_lifecycle::ensure_gui_session_domain`] already knew all of that and was
/// reachable only from `session up`.
///
/// **Every failure falls back to creating the session here, loudly.** A machine that cannot start
/// its session is not a machine to debug over SSH, and the fault this prevents is a session with
/// fewer capabilities than it should have - not a session that does not exist. `session up` is
/// stricter, and can be: nobody is waiting at a terminal for it.
pub(crate) fn create_through_the_unit(name: &str, opts: &CliArgs) -> bool {
    if UP_IS_THE_CREATOR.load(std::sync::atomic::Ordering::SeqCst) {
        return false;
    }
    if !session_is_managed(name, opts) {
        return false;
    }
    // the unit has to exist before it can be asked for anything
    manage_the_unit(name, opts);
    if zellij_utils::session_lifecycle::running_as_the_unit(name) {
        return false;
    }

    #[cfg(target_os = "macos")]
    let asked = zellij_utils::session_lifecycle::ensure_gui_session_domain(
        name,
        false,
        session_service::configured_restart_via_launchd(configured_extras(opts).as_ref()),
    );
    #[cfg(target_os = "linux")]
    let asked = zellij_utils::session_lifecycle::ensure_systemd_unit_session(name);
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    let asked: Result<bool, String> = Ok(false);

    match asked {
        Ok(true) => {},
        // nothing loaded to defer to; the caller creates it, as it always did
        Ok(false) => return false,
        Err(reason) => {
            eprintln!(
                "warning: '{}' is managed by the init system, which could not be asked to create \n         \
                 it, so it is created here instead: {}",
                name, reason
            );
            return false;
        },
    }

    let facts = wait_for_server(name);
    if facts.assert_up().is_ok() {
        println!("up    session '{}' was created by the init system", name);
        return true;
    }
    eprintln!(
        "warning: the init system was asked for '{}' and it has not appeared, so it is created \n         \
         here instead. `zellij session doctor {}` says what the unit is doing.",
        name, name
    );
    false
}

/// What the config's `pin_exe` and the installed unit say between them, and whether they agree.
///
/// Reported even though nothing is broken, because the disagreement is invisible from every other
/// angle: the config asks for a pinned copy, the launcher runs something else, and each of them is
/// internally fine. It is the state a key turned on after `session enable` leaves behind, and the
/// paths are BOTH named because a macOS grant is keyed to one exact path - a reader who is about to
/// add it by hand in System Settings needs to know which.
fn print_pin_state(name: &str, pinned: Option<&std::path::Path>) -> bool {
    let Some(pinned) = pinned else {
        println!("pin       off (no `pin_exe` in session_service)");
        return true;
    };
    match pin_state_of(name, pinned) {
        PinState::Recorded(path) => {
            println!("pin       {} - the launcher runs it", path.display());
            true
        },
        PinState::Unrecorded(path) => {
            println!(
                "pin       {} - nothing installed runs it yet",
                path.display()
            );
            true
        },
        PinState::Mismatch {
            configured,
            installed,
        } => {
            println!(
                "pin       {} - NOT what the launcher runs",
                configured.display()
            );
            println!(
                "pin       the launcher runs {} - `zellij session enable {}` re-points it",
                installed, name
            );
            false
        },
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pin_state_of(name: &str, pinned: &std::path::Path) -> PinState {
    session_service::pin_state(
        pinned,
        session_service::installed_session_exe(name).as_deref(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pin_state_of(_name: &str, pinned: &std::path::Path) -> PinState {
    session_service::pin_state(pinned, None)
}

/// One configured plist value on one line of `status`.
///
/// A container is written out in full rather than summarised as "a dictionary": the whole reason
/// `status` lists the extras is that a person can check what the unit will contain against what
/// they typed, and a count of entries answers neither question.
fn plist_value_summary(value: &PlistValue) -> String {
    match value {
        PlistValue::String(value) => value.to_owned(),
        PlistValue::Integer(value) => value.to_string(),
        PlistValue::Bool(value) => value.to_string(),
        PlistValue::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(plist_value_summary)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        PlistValue::Dict(entries) => format!(
            "{{{}}}",
            entries
                .iter()
                .map(|(name, value)| format!("{} = {}", name, plist_value_summary(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// List what `session_service` in the config puts into the unit, for the init system in use.
fn print_configured_extras(kind: ServiceKind, extras: Option<&SessionServiceOptions>) {
    let Some(extras) = extras.filter(|extras| extras.has_unit_extras()) else {
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
                println!(
                    "config    {} = {}",
                    key.name,
                    plist_value_summary(&key.value)
                );
            }
        },
    }
}

/// The name to act on: what was asked for, else the session this shell is in (for `restart`, which
/// is the one command that means "this one"), else `--session`, else the `session_name` config
/// option. There is no built-in default: a lifecycle command that guesses the name is a lifecycle
/// command that eventually kills the wrong session.
pub(crate) fn resolve_session_name(
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

/// What shape a `session up` is asked to build.
///
/// The three cases differ in more than which layout is read. Only `Snapshot` is a demand: it names
/// a shape the caller chose, so failing to produce it is an error worth exiting on. `Resume` is a
/// preference - come back as you were, if you can - and every one of its sources is allowed to be
/// missing, because the caller of a bare `up` is usually the watchdog, which wants a session more
/// than it wants a particular one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UpShape {
    /// Come back with the shape the session had, from whichever store still holds it.
    Resume,
    /// Come back from the layout, discarding the shape the session had.
    Fresh,
    /// Come back from this archived snapshot, by id or unique prefix.
    Snapshot(String),
}

/// What the flags on `session up` mean. Clap refuses `--fresh` with `--restore`, so this never has
/// to.
fn up_shape(fresh: bool, restore: Option<String>) -> UpShape {
    match (fresh, restore) {
        (true, _) => UpShape::Fresh,
        (false, Some(id)) => UpShape::Snapshot(id),
        (false, None) => UpShape::Resume,
    }
}

/// Whether a `Resume` has to reach for the archive.
///
/// The in-place cache is the fresher of the two stores and `attach` reads it by itself, so the
/// archive is consulted only when that file is gone - which is exactly what `session down` and
/// `delete-session` leave behind, and the whole of the gap this branch closes.
fn resume_wants_archive(resume_enabled: bool, cache_exists: bool) -> bool {
    resume_enabled && !cache_exists
}

/// Whether a plain `up` resumes at all. Default true; `session_up_resume false` goes back to
/// coming up from the layout whenever the in-place cache is gone.
fn resume_enabled(opts: &CliArgs) -> bool {
    get_config_options_from_cli_args(opts)
        .ok()
        .and_then(|options| options.session_up_resume)
        .unwrap_or(true)
}

/// The snapshot a bare `up` should come back from, as `(id, age)`, or `None` for the layout.
///
/// The layout is parsed here rather than left to `attach`, because the two callers want opposite
/// things from a snapshot that no longer parses. `--restore` names one and exits 2 when it cannot
/// be read; a derived one was nobody's request, and turning a routine watchdog tick into a machine
/// with no session is a worse answer than a fresh session and a warning.
fn resume_archive_target(name: &str, opts: &CliArgs) -> Option<(String, String)> {
    use zellij_utils::consts::session_layout_cache_file_name;
    use zellij_utils::session_snapshot::snapshots_for_session;

    if !resume_wants_archive(
        resume_enabled(opts),
        session_layout_cache_file_name(name).exists(),
    ) {
        return None;
    }
    // oldest first, so the newest is the one to pop
    let snapshot = snapshots_for_session(&snapshot_settings(opts), name).pop()?;
    let age = snapshot.saved_at_description();
    match snapshot.layout() {
        Ok(_) => Some((snapshot.id, age)),
        Err(reason) => {
            println!(
                "      snapshot {} ({}) no longer parses, so it is left on disk and the session \
                 comes up from the layout: {}",
                snapshot.id, age, reason
            );
            None
        },
    }
}

/// Create the session if it is not there, then prove that it is.
fn up(name: &str, shape: UpShape, opts: &CliArgs) -> Result<(), ()> {
    // First, and on every `up` including the one that finds the session already healthy: an upgrade
    // reaches the pinned copy through this pass and no other, and the pass that returns early is
    // exactly the one an upgraded machine takes every minute.
    assert_pinned_exe(name, configured_extras(opts).as_ref(), opts);
    // Same pass, same reason: a config edit does not reach a unit nobody rewrote, and the `up` that
    // returns early is exactly the one a machine with a stale unit takes every minute. When the
    // session is MANAGED the drift is fixed rather than reported, and this runs BEFORE the lock
    // below: `session enable` starts what it installs, and the unit it starts runs this same
    // command, which would then be waiting for a lock this process is holding.
    manage_the_unit(name, opts);
    warn_if_unit_drifted(name, opts);

    // Held for the rest of this function, which is what makes `up` idempotent under concurrency:
    // the check and the creation are one step, so a `restart` overlapping the watchdog's minute
    // tick waits here and then finds the session already up. See `session_lifecycle::lock_up`.
    let _up_lock = lock_up(name);

    let facts = SessionFacts::collect(name);
    let healthy = facts.assert_up().is_ok();

    // Only a NAMED snapshot is a demand that a running session contradicts. A bare `up` that would
    // have resumed is answered by the session that is already there, which is what it wanted, and a
    // watchdog tick must not start exiting 2 for finding the session up.
    let named_snapshot = matches!(shape, UpShape::Snapshot(_));
    if healthy && !named_snapshot {
        println!("ok    session '{}' already running", name);
        // "already running" is exactly the answer that hides a superseded build: an `up` after an
        // upgrade reports success and leaves the old server serving the session.
        warn_if_server_build_differs(name);
        return Ok(());
    }
    if (healthy || facts.listed) && named_snapshot {
        eprintln!(
            "Session '{}' is running, so there is nothing to restore into.",
            name
        );
        process::exit(2);
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
    match zellij_utils::session_lifecycle::ensure_gui_session_domain(
        name,
        facts.listed,
        session_service::configured_restart_via_launchd(configured_extras(opts).as_ref()),
    ) {
        Ok(true) => {
            // launchd is the creator now, and the job it just started runs `session up` - which
            // takes this same lock. Held any longer, this process waits for a session that is
            // waiting for this process. See `session_lifecycle::hand_over_up_lock`.
            zellij_utils::session_lifecycle::hand_over_up_lock(name);
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

    // The Linux half of the same decision, and it only applies to a MANAGED session: on macOS the
    // domain a server is created in is a fact about the server that nothing can change afterwards,
    // so that guard runs whatever the config says; here there is no such fact, and the only reason
    // to prefer the unit is that the config asked for the unit to own the name.
    //
    // `ensure_systemd_unit_session` refuses when this process IS the unit, which is what stops the
    // service's own `session up` from asking systemd to start the service it is.
    #[cfg(target_os = "linux")]
    if session_is_managed(name, opts) {
        match zellij_utils::session_lifecycle::ensure_systemd_unit_session(name) {
            Ok(true) => {
                // the unit runs this same command and takes this same lock
                zellij_utils::session_lifecycle::hand_over_up_lock(name);
                let facts = wait_for_server(name);
                if let Err(reason) = facts.assert_up() {
                    eprintln!("session up: post-condition FAILED - {}", reason);
                    facts.print_diagnostics();
                    return Err(());
                }
                println!(
                    "up    session '{}' in {} (started by systemd)",
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

    // Which shape this session is built from, decided once, here. `attach` resurrects from the
    // in-place cache by itself, so `Resume` only has work to do when that file is gone.
    let derived = match &shape {
        UpShape::Resume => resume_archive_target(name, opts),
        UpShape::Fresh | UpShape::Snapshot(_) => None,
    };
    let restore = match &shape {
        UpShape::Snapshot(id) => Some(id.clone()),
        UpShape::Fresh => None,
        UpShape::Resume => derived.as_ref().map(|(id, _)| id.clone()),
    };
    // `--fresh` discards the in-place cache rather than merely ignoring it, which is what makes a
    // layout edit apply. The archived snapshot is untouched, so the discarded shape is still
    // reachable with `session up --restore`.
    let no_resurrect = matches!(shape, UpShape::Fresh);

    let mut opts = opts.clone();
    opts.session = None;
    opts.command = Some(Command::Sessions(Sessions::Attach {
        session_name: Some(name.to_owned()),
        create: true,
        create_background: true,
        force_run_commands: false,
        no_resurrect,
        restore: restore.clone(),
        index: None,
        options: None,
        token: None,
        remember: false,
        forget: false,
        ca_cert: None,
        insecure: false,
    }));
    // Both arms above have already had their answer from the init system, so the client path this
    // drives must not ask it again. See `UP_IS_THE_CREATOR`.
    UP_IS_THE_CREATOR.store(true, std::sync::atomic::Ordering::SeqCst);
    start_client(opts);
    UP_IS_THE_CREATOR.store(false, std::sync::atomic::Ordering::SeqCst);

    let facts = wait_for_server(name);
    if let Err(reason) = facts.assert_up() {
        eprintln!("session up: post-condition FAILED - {}", reason);
        facts.print_diagnostics();
        return Err(());
    }
    println!("up    session '{}' in {}", name, facts.socket_dir.display());
    match (&derived, &restore) {
        (Some((id, age)), _) => println!("      restored from snapshot {} (saved {})", id, age),
        (None, Some(id)) => println!("      restored from snapshot {}", id),
        (None, None) => {},
    }
    Ok(())
}

/// Poll until the session looks up, or until the timeout says it never will.
///
/// The gap doubles, because every poll costs a `ps` over the whole process table and the two cases
/// want opposite things from it. A session that comes up does so in the first few hundred
/// milliseconds and wants to be noticed at once; a session that is never coming up spends the whole
/// of `SERVER_APPEARANCE_TIMEOUT` proving it, and that is the case a launcher REPEATS every minute
/// for as long as the fault lasts. Backing off cuts the forks on the failing machine by about an
/// order of magnitude and costs the healthy one nothing.
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
    // The same lock `up` and `restart` take, and for the same reason from the other side: without
    // it a teardown races the watchdog tick it is meant to be serialised against. Either the
    // teardown kills the server a tick has just created - and the tick reports a post-condition
    // failure on a healthy machine - or the tick's `up` puts the session straight back after this
    // has printed `removed`, and the teardown silently did not stick.
    //
    // Held across the whole function, so the check and the removal are one step. A `restart` that
    // already holds it re-enters rather than waiting; see `session_lifecycle::lock_up`.
    let _down_lock = lock_up(name);

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

/// What a `session restart` comes back from.
///
/// The pre-restart shape is what a restart is almost always for - the process is the thing being
/// replaced, not the layout - so a snapshot is restored by default and `--fresh` is the deliberate
/// exception, for picking up a layout edit. Clap refuses the two together, so this never has to.
fn restart_restore_target(fresh: bool, restore: Option<String>) -> UpShape {
    if fresh {
        // Load-bearing, and the reason this returns a shape rather than an `Option`: a plain `up`
        // now resumes, so a `--fresh` restart that asked for "no snapshot" by passing `None` would
        // be handed the newest one back. `Fresh` is the only way to the layout and stays that way.
        UpShape::Fresh
    } else {
        UpShape::Snapshot(restore.unwrap_or_else(|| "latest".to_owned()))
    }
}

fn restart(name: &str, shape: UpShape, wait_timeout: u64, opts: &CliArgs) -> ! {
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

    // Held across BOTH steps, and taken here rather than left to `up`. A restart is a `down`
    // followed by an `up`, and the watchdog's minute tick fits between them: its `up` takes the
    // lock first, finds nothing, and builds the session fresh from the layout. This `up` then
    // waits, finds a healthy session, and either reports "already running" or refuses to restore
    // into it - having thrown away the snapshot the restart existed to bring back. The inner `up`
    // re-enters this hold rather than waiting for it; see `session_lifecycle::lock_up`.
    //
    // Taken after the daemonize, so the descriptor belongs to the process that does the work: a
    // restart that dies mid-hold has the flock released for it by the kernel. At the default
    // `--wait-timeout` both steps together are bounded well inside the lock's own 90 seconds - 10
    // for the down and 30 for the up - so a waiting `up` waits rather than giving up and proceeding
    // unlocked; a `--wait-timeout` past about a minute can outlast that, which `lock_up` records and
    // `a_slow_restart_still_fits_inside_the_up_lock` asserts.
    let _restart_lock = lock_up(name);

    if down(name, wait_timeout, opts).is_err() {
        eprintln!("teardown failed; NOT recreating the session.");
        process::exit(1);
    }
    process::exit(match up(name, shape, opts) {
        Ok(()) => 0,
        Err(()) => 1,
    });
}

/// Windows has no fork, and no process group to escape from: the caller's shell is not a pane shell
/// the server is about to kill.
#[cfg(not(unix))]
fn restart(name: &str, shape: UpShape, wait_timeout: u64, opts: &CliArgs) -> ! {
    drop_configured_env(opts);
    if down(name, wait_timeout, opts).is_err() {
        eprintln!("teardown failed; NOT recreating the session.");
        process::exit(1);
    }
    process::exit(match up(name, shape, opts) {
        Ok(()) => 0,
        Err(()) => 1,
    });
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_restart_comes_back_from_the_newest_snapshot() {
        assert_eq!(
            restart_restore_target(false, None),
            UpShape::Snapshot("latest".to_owned())
        );
    }

    #[test]
    fn a_restart_can_be_told_which_snapshot() {
        assert_eq!(
            restart_restore_target(false, Some("abc123".to_owned())),
            UpShape::Snapshot("abc123".to_owned())
        );
    }

    #[test]
    fn a_fresh_restart_restores_nothing() {
        // the negative control: --fresh is the only way back to the layout, and it stays that way.
        // `Fresh` rather than "no snapshot named" is the whole point of the enum - a bare `up`
        // resumes, so "nothing named" would now mean "resume from the newest snapshot".
        assert_eq!(restart_restore_target(true, None), UpShape::Fresh);
    }

    #[test]
    fn a_bare_up_resumes() {
        assert_eq!(up_shape(false, None), UpShape::Resume);
    }

    #[test]
    fn a_fresh_up_comes_from_the_layout() {
        assert_eq!(up_shape(true, None), UpShape::Fresh);
    }

    #[test]
    fn an_up_can_be_told_which_snapshot() {
        assert_eq!(
            up_shape(false, Some("abc123".to_owned())),
            UpShape::Snapshot("abc123".to_owned())
        );
    }

    #[test]
    fn a_resume_leaves_an_in_place_shape_to_attach() {
        // attach resurrects from the in-place cache by itself, and that file is fresher than any
        // snapshot, so the archive is not consulted while it exists
        assert!(!resume_wants_archive(true, true));
    }

    #[test]
    fn a_resume_reaches_the_archive_once_the_in_place_shape_is_gone() {
        // what `session down` and `delete-session` leave behind, and the gap this closes
        assert!(resume_wants_archive(true, false));
    }

    #[test]
    fn resuming_can_be_turned_off() {
        assert!(!resume_wants_archive(false, false));
    }
    use zellij_utils::session_lifecycle::UP_LOCK_TIMEOUT;

    /// The default `--wait-timeout` on `session restart`, read from the parser rather than copied,
    /// because the invariant below is only guarded if a change to that default breaks this test.
    ///
    /// Parsed on a big stack, like every other test that builds the clap tree: a test thread's own
    /// stack overflows on it. See [`on_big_stack`].
    fn default_restart_wait_timeout() -> Duration {
        use clap::Parser;
        use zellij_utils::cli::on_big_stack;

        let parsed =
            on_big_stack(|| CliArgs::try_parse_from(["zellij", "session", "restart", "a-session"]))
                .expect("`session restart` parses with no flags");
        match parsed.command {
            Some(Command::Sessions(Sessions::Session(SessionLifecycleCli::Restart {
                wait_timeout,
                ..
            }))) => Duration::from_secs(wait_timeout),
            other => panic!("expected a `session restart` command, got {:?}", other),
        }
    }

    /// `restart` holds the up-lock across its down and its up, so the longest hold a healthy
    /// machine produces is a `--wait-timeout` down plus this wait for the server. A waiting `up`
    /// that gives up first goes ahead without the lock, which is the two-servers-for-one-name race
    /// the lock exists to prevent - so raising ANY of the three without the others reintroduces it.
    #[test]
    fn a_slow_restart_still_fits_inside_the_up_lock() {
        let longest_down = default_restart_wait_timeout();
        assert!(
            longest_down + SERVER_APPEARANCE_TIMEOUT < UP_LOCK_TIMEOUT,
            "a restart can hold the lock for {:?}, but a waiting `up` gives up after {:?}",
            longest_down + SERVER_APPEARANCE_TIMEOUT,
            UP_LOCK_TIMEOUT
        );
    }

    /// The arithmetic above holds only while the thing being waited for is not itself waiting for
    /// this lock, and a kickstarted launch agent is exactly that. It runs `zellij session up
    /// <name>` in ANOTHER process, whose `lock_up` cannot re-enter this one - re-entrancy is
    /// per thread - so it blocks on the `flock` for `UP_LOCK_TIMEOUT` while this side spends
    /// `SERVER_APPEARANCE_TIMEOUT` waiting for the session it would have created. The inequality
    /// that makes a slow restart safe is what guarantees the deadlock here: the caller always
    /// gives up first, reports `post-condition FAILED`, exits non-zero, and the session appears
    /// moments later anyway.
    ///
    /// So the `Ok(true)` arm hands the lock over before it waits. Asserted through the derived
    /// path rather than a scratch one, because the two processes only meet if `up_lock_path` gives
    /// them the same file.
    #[test]
    #[cfg(unix)]
    fn handing_the_session_to_launchd_releases_the_lock_the_job_needs() {
        use zellij_utils::session_lifecycle::{
            hand_over_up_lock, lock_up, up_lock_is_free, up_lock_path,
        };

        let name = format!("zj-handover-{}", std::process::id());

        // the shape `restart` makes: its own hold, with the inner `up`'s inside it
        let restart_lock = lock_up(&name).expect("the lock is free");
        let up_lock = lock_up(&name).expect("re-entered rather than waited");
        assert!(
            !up_lock_is_free(&name),
            "nothing was holding it to begin with"
        );

        hand_over_up_lock(&name);
        // what the kickstarted job's own `lock_up` finds, asked on its own descriptor - which is
        // how another process asks it
        assert!(
            up_lock_is_free(&name),
            "the job launchd just started would block here for {:?}",
            UP_LOCK_TIMEOUT
        );

        drop(up_lock);
        drop(restart_lock);
        let _ = std::fs::remove_file(up_lock_path(&name));
    }

    /// `down` now takes the up-lock too, and a `restart` calls `down` while already holding it.
    ///
    /// So the nested acquisition has to RE-ENTER. A second `flock` would wait `UP_LOCK_TIMEOUT`
    /// for a hold this very thread owns and then go ahead unlocked, which is the two-servers race
    /// the lock exists to close - a restart would deadlock against itself for a minute and a half
    /// and then run without the protection.
    #[test]
    #[cfg(unix)]
    fn a_down_nested_inside_a_restart_re_enters_the_lock_rather_than_waiting() {
        use zellij_utils::session_lifecycle::{lock_up, up_lock_is_free, up_lock_path};

        let name = format!("zj-downlock-{}", std::process::id());
        let restart_lock = lock_up(&name).expect("the lock is free");

        let started = std::time::Instant::now();
        let down_lock = lock_up(&name).expect("re-entered rather than waited");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the nested `down` waited {:?} for a lock this thread already holds",
            started.elapsed()
        );

        // the outer hold survives the inner drop, or `restart`'s `up` would run unlocked
        drop(down_lock);
        assert!(
            !up_lock_is_free(&name),
            "`down` finishing released the lock `restart` still needs"
        );

        drop(restart_lock);
        assert!(up_lock_is_free(&name), "the outermost hold never let go");
        let _ = std::fs::remove_file(up_lock_path(&name));
    }

    /// launchd was measured at 15 to 20 seconds on the fleet's Macs, and a `session up` that
    /// reports a false post-condition failure on every slow start teaches everyone to ignore the
    /// one line that reports a real one.
    #[test]
    fn the_wait_for_a_server_outlasts_a_measured_launchd_start() {
        assert!(SERVER_APPEARANCE_TIMEOUT >= Duration::from_secs(30));
    }
}
