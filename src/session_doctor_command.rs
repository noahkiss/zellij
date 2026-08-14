//! `zellij session doctor` - everything that has to hold for one session to come up and stay up.
//!
//! This was a 1280-line shell script before it was a subcommand, and moving it in is what makes it
//! honest. The script had to be told where the sockets live, which binary the launcher runs and
//! what the config says; it learned all three from the environment and from `sed`, and every one
//! of them was a place to be wrong. The binary already knows: it resolves its own socket
//! directory, it reads its own config, and it can compare an installed unit against the one it
//! would write. Nothing here parses zellij's output, because nothing here is outside zellij.
//!
//! Doctor never takes the session down. A pin refresh, a signature and a rewritten plist all take
//! effect at the next start, so the honest thing is to make the change and SAY that a restart is
//! needed - `zellij session restart` is the command that does it, and it belongs to the person
//! whose panes are in there.
//!
//! Every check answers in the three words of [`zellij_utils::session_doctor`] and nothing else
//! decides the exit code. See that module for why.

use std::path::{Path, PathBuf};

use zellij_utils::cli::CliArgs;
use zellij_utils::consts::ZELLIJ_SOCK_DIR;
use zellij_utils::home::find_default_config_dir;
use zellij_utils::session_doctor::{DoctorMode, Finding, Report};
use zellij_utils::session_lifecycle::{build_mismatch_warning, SessionFacts};
use zellij_utils::session_service::{
    self, configured_pinned_exe, path_dirs, resolve_service_exe, PinState, ServiceExe, ServiceKind,
    SessionServiceOptions, UnitDrift,
};
use zellij_utils::sessions;
use zellij_utils::setup::Setup;

use crate::session_commands::{configured_extras, resolve_session_name};

/// Run the checklist and print what it came to.
pub(crate) fn session_doctor_command(
    session_name: Option<String>,
    dry_run: bool,
    no_fix: bool,
    no_sign: bool,
    exe: Option<PathBuf>,
    opts: CliArgs,
) -> ! {
    let name = resolve_session_name(session_name, &opts, false);
    let mode = DoctorMode::from_flags(dry_run, no_fix, no_sign);
    let report = examine(&name, exe, mode, &opts);
    print!("{}", report.render());
    std::process::exit(report.exit_code());
}

/// The checklist itself.
///
/// Ordered the way a fault propagates rather than the way the code is organised: where the binary
/// is, what the config says, where the sockets are, what is installed to keep the session up, and
/// only then the session and the build serving it. A reader who stops at the first `Needs you` has
/// usually stopped at the cause.
fn examine(name: &str, exe: Option<PathBuf>, mode: DoctorMode, opts: &CliArgs) -> Report {
    let mut report = Report::new();
    let extras = configured_extras(opts);
    let pinned = configured_pinned_exe(extras.as_ref());

    check_path(&mut report);
    check_config(&mut report, opts);
    check_socket_dir(&mut report, name);
    check_artifacts(&mut report);
    check_unit(&mut report, name, exe, extras.as_ref(), pinned.clone());
    let facts = SessionFacts::collect(name);
    check_session(&mut report, name, &facts);
    check_dead_session(&mut report, name, &facts);
    check_build(&mut report, name, &facts);
    check_pin(&mut report, name, pinned.as_deref(), mode);
    platform_checks(&mut report, name, pinned.as_deref(), mode, opts, &facts);
    report
}

/// Which `zellij` a shell on this machine runs, and whether it is this one.
///
/// Two builds of zellij reachable under one name is the fault behind most of the rest: a unit that
/// execs one of them, a client that speaks to the other, a config edit that reaches neither. The
/// question is asked against the RESOLVED path on both sides, because a package manager's stable
/// name is a symlink and comparing symlinks compares nothing.
fn check_path(report: &mut Report) {
    let Ok(current_exe) = std::env::current_exe() else {
        report.push(Finding::needs_you(
            "path",
            "this process cannot say which binary it is, so nothing below can be compared to it",
        ));
        return;
    };
    let resolved = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.clone());
    let name = resolved.file_name().unwrap_or_else(|| "zellij".as_ref());
    let first = path_dirs()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file());

    match first {
        None => report.push(
            Finding::needs_you(
                "path",
                format!("no `{}` anywhere on PATH", name.to_string_lossy()),
            )
            .note(format!("this binary is at {}", resolved.display()))
            .note("a unit that has to survive an upgrade needs a stable name that leads here"),
        ),
        Some(first) if first.canonicalize().ok().as_deref() == Some(resolved.as_path()) => report
            .push(Finding::ok(
                "path",
                format!("{} leads to this binary", first.display()),
            )),
        Some(first) => report.push(
            Finding::needs_you(
                "path",
                format!("{} is a DIFFERENT build from this one", first.display()),
            )
            .note(format!("this binary: {}", resolved.display()))
            .note(
                "a `zellij` typed in a shell runs the first one, so the two disagree about the \
                 same session",
            ),
        ),
    }
}

/// Whether the config this binary would load actually loads.
///
/// The same load `zellij setup --check` reports on, made here rather than described here: a doctor
/// that told the user to go and run another command would be handing back the question it was
/// asked. A config that does not parse is the one fault that makes every check below it describe a
/// machine that is not the one the user has, because the launcher, the pin and the session name
/// all come out of it.
fn check_config(report: &mut Report, opts: &CliArgs) {
    let config_file = opts
        .config
        .clone()
        .or_else(|| find_default_config_dir().map(|dir| dir.join("config.kdl")));
    let named = config_file
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| String::from("the built-in defaults"));
    match Setup::from_cli_args(opts) {
        Ok(_) => report.push(Finding::ok("config", format!("{} parses", named))),
        Err(error) => report.push(
            Finding::needs_you("config", format!("{} does not load", named))
                .note(error.to_string())
                .note("nothing below this line describes your machine until that is fixed"),
        ),
    }
}

/// Where this binary looks for sockets, and whether anything is serving this name somewhere else.
///
/// A session in another socket directory is invisible from here: `zellij ls` does not list it,
/// `attach` does not find it, and `session up` builds a second server for the same name beside it.
/// That was the original fault the lifecycle commands were written to end, and it is worth a line
/// even on the machines where the answer is always "no".
fn check_socket_dir(report: &mut Report, name: &str) {
    report.push(Finding::ok(
        "socket",
        format!("{}", ZELLIJ_SOCK_DIR.display()),
    ));

    let elsewhere: Vec<PathBuf> = sessions::get_sessions_in_other_socket_dirs()
        .into_iter()
        .filter(|(_, names)| names.iter().any(|listed| listed == name))
        .map(|(dir, _)| dir)
        .collect();
    for dir in elsewhere {
        report.push(
            Finding::needs_you(
                "socket",
                format!("'{}' also exists under {}", name, dir.display()),
            )
            .note("that session is invisible to this binary - nothing here can attach to it or")
            .note("take it down. Remove it from a shell whose ZELLIJ_SOCK_DIR resolves there."),
        );
    }

    let versions = sessions::session_in_other_contract_versions(name);
    for version in versions {
        report.push(
            Finding::needs_you(
                "socket",
                format!(
                    "'{}' is also served by a build speaking contract version {}",
                    name, version
                ),
            )
            .note("two servers for one name, each invisible to the other's clients"),
        );
    }
}

/// Leftovers from the shell scripts these commands replaced.
///
/// Both are read by something other than zellij - a wrapper on PATH, a line in an rc file - and
/// both go on working after they stop being right. A stale `session-env` names a socket directory
/// this binary no longer resolves, which is exactly how one name came to have two servers.
///
/// What counts as a leftover is narrow on purpose, and it is not "a file in `~/bin` with zellij in
/// its name". A companion tool that CALLS zellij is not a fault, and telling somebody to delete
/// the script that runs doctor would be the report at its least useful. Two shapes only: a file
/// that shadows the binary by taking its name, and one that sets `ZELLIJ_SOCK_DIR` before zellij
/// can resolve it for itself. The second is the fault by definition - that variable is the one
/// thing a wrapper can decide and get wrong.
///
/// Reported and never removed. Doctor did not write these files and cannot see what still sources
/// them, and a fix that deletes something a user put in their own `~/bin` is a fix that has to be
/// asked for. The exact command is given instead, which is the whole of what the fix would be.
fn check_artifacts(report: &mut Report) {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let running = std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok());

    let wrappers = wrapper_faults(&home.join("bin"), running.as_deref());
    for (wrapper, why) in &wrappers {
        report.push(
            Finding::needs_you("artifact", format!("{} {}", wrapper.display(), why))
                .note("a shell that runs it resolves a different socket directory, so it sees")
                .note("a different set of sessions under the same names. Remove it with")
                .note(format!("  rm {}", wrapper.display())),
        );
    }

    let session_env = home.join(".config/zellij/session-env");
    if session_env.exists() {
        report.push(
            Finding::needs_you(
                "artifact",
                format!(
                    "{} is left over from the shell scripts",
                    session_env.display()
                ),
            )
            .note("a shell that sources it gets a socket directory this binary does not resolve")
            .note(format!("  rm {}", session_env.display())),
        );
    }

    if wrappers.is_empty() && !session_env.exists() {
        report.push(Finding::ok(
            "artifact",
            "no wrapper scripts and no stale session-env",
        ));
    }
}

/// The files in one directory that mislead a shell about zellij, and what is wrong with each.
///
/// Its own function so the two shapes can be tested, which matters more here than anywhere else in
/// the report: this is the only check whose advice is `rm`, and a false positive costs somebody a
/// file doctor did not write.
///
/// `running` is this binary's resolved path, and a `zellij` in the directory that resolves to it is
/// not a leftover at all - it is where zellij is installed. `check_path` has already reported that
/// same file as the one a shell runs, so flagging it here would have doctor contradict itself and
/// tell the user to delete their installation.
fn wrapper_faults(directory: &Path, running: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let mut faults: Vec<(PathBuf, &'static str)> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter_map(|path| {
            let name = path.file_name().and_then(|name| name.to_str())?;
            if name == "zellij" {
                // a path that will not resolve - a dangling symlink - is still a name a shell
                // finds and fails to run, so it stays a fault
                if let (Ok(resolved), Some(running)) = (path.canonicalize(), running) {
                    if resolved == running {
                        return None;
                    }
                }
                return Some((path, "shadows the zellij on PATH"));
            }
            if !name.contains("zellij") {
                return None;
            }
            // a script, so a bounded read; nothing in `~/bin` that is 40 MB is a wrapper
            let source = std::fs::read_to_string(&path).ok()?;
            source
                .contains("ZELLIJ_SOCK_DIR")
                .then_some((path, "sets ZELLIJ_SOCK_DIR before zellij can resolve it"))
        })
        .collect();
    faults.sort();
    faults
}

/// What is installed to keep the session up, whether the init system holds it, and whether the
/// file on disk is still what this config would write.
///
/// Delegated whole to [`session_service::status`] - the same call `zellij session status` makes,
/// against the same three facts, so the two commands cannot disagree about one machine. Doctor
/// adds no judgement of its own here beyond turning each fact into a finding.
fn check_unit(
    report: &mut Report,
    name: &str,
    exe: Option<PathBuf>,
    extras: Option<&SessionServiceOptions>,
    pinned: Option<PathBuf>,
) {
    let Some(kind) = session_service::native_service_kind() else {
        report.push(
            Finding::ok("unit", "no init system this build can install into")
                .note("`zellij setup --generate-service` prints a unit to adapt by hand"),
        );
        return;
    };
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zellij"));
    let exe = resolve_service_exe(exe, pinned, &current_exe, &path_dirs());
    if let ServiceExe::Resolved(path) = &exe {
        report.push(
            Finding::needs_you(
                "unit",
                format!("the unit would have to name {}", path.display()),
            )
            .note("nothing on PATH and no `pin_exe` leads here, so an upgrade that moves this")
            .note("binary breaks the unit. Set `pin_exe` or name a stable path with --exe."),
        );
    }

    let status = match session_service::status(kind, exe.path(), name, extras) {
        Ok(status) => status,
        Err(reason) => {
            report.push(Finding::needs_you(
                "unit",
                format!("could not read the install: {}", reason),
            ));
            return;
        },
    };

    let elsewhere = status.installed_as.first();
    for (index, file) in status.files.iter().enumerate() {
        let found_elsewhere = if file.role == "timer" {
            status.timer_installed_as.as_ref().map(|timer| &timer.name)
        } else if index == 0 {
            elsewhere.map(|job| &job.name)
        } else {
            None
        };
        match (file.present, found_elsewhere) {
            (true, _) => report.push(Finding::ok(
                file.role,
                format!("{} is installed", file.path.display()),
            )),
            // a job somebody else wrote does the same work, so the file this build would have
            // written being absent is not the fault its absence looks like
            (false, Some(other)) => report.push(Finding::ok(
                file.role,
                format!("installed under another name: {}", other),
            )),
            (false, None) => report.push(
                Finding::needs_you(file.role, format!("{} is missing", file.path.display()))
                    .note(format!("`zellij session enable {}` writes it", name)),
            ),
        }
    }
    for other in status.installed_as.iter().skip(1) {
        report.push(
            Finding::needs_you(
                "unit",
                format!("{} ALSO runs `session up {}`", other.name, name),
            )
            .note(format!("({})", other.path.display()))
            .note("two launchers race at login and one of them ends up failed"),
        );
    }

    if status.loaded {
        report.push(Finding::ok(
            "loaded",
            format!("the init system holds the job ({})", status.load_detail),
        ));
    } else {
        report.push(
            Finding::needs_you("loaded", format!("not loaded ({})", status.load_detail))
                .note(format!("`zellij session enable {}` loads it", name)),
        );
    }

    check_drift(report, kind, exe.path(), name, extras);
}

/// Whether the unit on disk is still what this config would write.
///
/// Its own check because nothing else notices it. Edit the config and the loaded job does not
/// change with it: the file is stale, the init system is still running the definition it was
/// handed, and every angle you can look from is internally consistent. It is wrong only when the
/// two are compared.
///
/// Not fixed here even though doctor could. Rewriting a plist means `bootout` then `bootstrap`,
/// which stops the job that keeps the session alive - and doctor's one promise is that it never
/// disturbs a running session. `session enable` is that command and it is named instead.
fn check_drift(
    report: &mut Report,
    kind: ServiceKind,
    exe: &Path,
    name: &str,
    extras: Option<&SessionServiceOptions>,
) {
    match session_service::unit_drift(kind, exe, name, extras) {
        Ok(UnitDrift::NotInstalled) => report.push(Finding::ok(
            "drift",
            "nothing zellij wrote is installed to compare against",
        )),
        Ok(UnitDrift::Current) => report.push(Finding::ok(
            "drift",
            "the installed unit is what this config would write",
        )),
        Ok(UnitDrift::Drifted { paths }) => {
            let mut finding = Finding::needs_you(
                "drift",
                format!("`zellij session enable {}` would rewrite the install", name),
            );
            for path in paths {
                finding = finding.note(format!(
                    "{} is not what this config writes now",
                    path.display()
                ));
            }
            if kind == ServiceKind::Launchd {
                finding = finding
                    .note("a changed plist needs bootout then bootstrap, which that command does");
            }
            report.push(finding);
        },
        Err(reason) => report.push(Finding::needs_you(
            "drift",
            format!("could not compare the installed unit: {}", reason),
        )),
    }
}

/// Whether one server, and only one, is serving this name from the socket this binary resolved.
fn check_session(report: &mut Report, name: &str, facts: &SessionFacts) {
    for server in facts.foreign_servers() {
        report.push(
            Finding::needs_you(
                "session",
                format!(
                    "pid {} serves '{}' from {}",
                    server.pid,
                    name,
                    server.socket.display()
                ),
            )
            .note("outside this binary's socket directory, so it is invisible from here"),
        );
    }
    match facts.assert_up() {
        Ok(()) => report.push(Finding::ok(
            "session",
            format!("'{}' is up in {}", name, facts.socket_dir.display()),
        )),
        // a session that is down is not a fault by itself - it is a fault when something is
        // installed to keep it up, and the `loaded` finding above has already said so
        Err(reason) => report.push(
            Finding::needs_you("session", format!("'{}' is not up - {}", name, reason))
                .note(format!("`zellij session up {}` creates it", name)),
        ),
    }
}

/// Whether a dead session is holding this name.
///
/// A session that exited leaves its layout behind, and the name keeps pointing at it: the next
/// `session up` resurrects that layout rather than starting from the configured one, which is the
/// answer to "why did my session come back with yesterday's panes in it". Only worth saying when no
/// server is up - a live session has a snapshot too, and there it is the ordinary state.
///
/// Reported and never removed. The snapshot is the user's work, and `zellij delete-session` is the
/// command that decides against keeping it.
fn check_dead_session(report: &mut Report, name: &str, facts: &SessionFacts) {
    if facts.assert_up().is_ok() {
        return;
    }
    if !sessions::get_resurrectable_session_names()
        .iter()
        .any(|dead| dead == name)
    {
        return;
    }
    report.push(
        Finding::ok("dead", format!("'{}' has a saved layout waiting", name))
            .note("`zellij session up` resurrects those panes rather than starting fresh")
            .note(format!(
                "`zellij delete-session {}` drops it, and the next session starts clean",
                name
            )),
    );
}

/// Whether the server serving this session is running the build this binary is.
fn check_build(report: &mut Report, name: &str, facts: &SessionFacts) {
    if facts.our_servers().is_empty() {
        return;
    }
    match build_mismatch_warning(name) {
        None => report.push(Finding::ok(
            "build",
            "the running server is this build of zellij",
        )),
        Some(warning) => {
            let mut finding = Finding::needs_you(
                "build",
                format!("'{}' runs a different build from this binary", name),
            );
            for line in warning.lines().skip(1) {
                finding = finding.note(line.trim().to_owned());
            }
            report.push(finding);
        },
    }
}

/// What `pin_exe` asks for and what the launcher actually runs.
///
/// With no `pin_exe` configured this is correct BY CONFIGURATION rather than by accident, and it
/// says so and stops - everything below the pin is about a file that does not exist on this
/// machine, and reporting a missing signature for a missing file would send someone to fix a
/// setting they deliberately did not turn on. The one line about what turning it on would buy is
/// there because that decision is worth revisiting on a Mac and nowhere else says so.
fn check_pin(report: &mut Report, name: &str, pinned: Option<&Path>, mode: DoctorMode) {
    let Some(pinned) = pinned else {
        report.push(
            Finding::ok("pin", "off - no `pin_exe` in session_service")
                .note("turning it on keeps a build at one path zellij owns, which is what lets a")
                .note(
                    "macOS permission grant survive an upgrade. Nothing below the pin is checked.",
                ),
        );
        return;
    };
    match pin_state_of(name, pinned) {
        PinState::Recorded(path) => report.push(Finding::ok(
            "pin",
            format!("{} - the launcher runs it", path.display()),
        )),
        PinState::Unrecorded(path) => report.push(
            Finding::ok(
                "pin",
                format!("{} - nothing installed runs it yet", path.display()),
            )
            .note(format!(
                "`zellij session enable {}` points a launcher at it",
                name
            )),
        ),
        PinState::Mismatch {
            configured,
            installed,
        } => report.push(
            Finding::needs_you(
                "pin",
                format!("{} is NOT what the launcher runs", configured.display()),
            )
            .note(format!("the launcher runs {}", installed))
            .note(format!("`zellij session enable {}` re-points it", name))
            .note("a macOS grant names one exact path, so the two have to be the same file"),
        ),
    }
    check_pin_freshness(report, pinned, mode);
}

/// Whether the pinned copy was made from the build running now.
///
/// Asked of the stamp beside the pin rather than of the pin's own bytes, for the reason
/// [`install_pinned_exe`](zellij_utils::session_lifecycle::install_pinned_exe) records: once the
/// pin is signed it differs from its source on purpose, and a comparison of the two files calls a
/// signed pin stale forever.
#[cfg(unix)]
fn check_pin_freshness(report: &mut Report, pinned: &Path, mode: DoctorMode) {
    use zellij_utils::session_lifecycle::{install_pinned_exe, pin_needs_refresh, PinOutcome};

    let Ok(current_exe) = std::env::current_exe() else {
        report.push(Finding::needs_you(
            "pin",
            "this process cannot say which binary it is, so the pin cannot be refreshed",
        ));
        return;
    };
    // A missing pin is not a `Needs you` on either path. With `--fix` this run writes it, and a
    // report that both wrote the file and exited non-zero would be telling a script that somebody
    // still has to act; without `--fix` the line below already says the copy would be made.
    if !mode.fix {
        // asked of the same function the fix asks, so a dry run reports the decision the fix would
        // make rather than a condition under which it might make one
        if !pin_needs_refresh(&current_exe, pinned) {
            report.push(Finding::ok(
                "pin",
                format!("{} was made from this build", pinned.display()),
            ));
        } else if pinned.exists() {
            report.push(Finding::changed(
                "pin",
                mode.describe(&format!("refresh the pinned copy at {}", pinned.display())),
            ));
        } else {
            report.push(Finding::changed(
                "pin",
                mode.describe(&format!("pin this build at {}", pinned.display())),
            ));
        }
        return;
    }
    match install_pinned_exe(&current_exe, pinned) {
        Ok(PinOutcome::UpToDate(path)) => report.push(Finding::ok(
            "pin",
            format!("{} was made from this build", path.display()),
        )),
        Ok(PinOutcome::Installed(path)) => report.push(Finding::changed(
            "pin",
            format!("pinned this build at {}", path.display()),
        )),
        Ok(PinOutcome::Refreshed(path)) => report.push(
            Finding::changed(
                "pin",
                format!("refreshed the pinned copy at {}", path.display()),
            )
            // the running server keeps executing the copy it started from, so the refresh reaches
            // nothing until the session is next created
            .note("the running session keeps the old copy until it is restarted"),
        ),
        Err(reason) => report.push(Finding::needs_you("pin", reason)),
    }
}

#[cfg(not(unix))]
fn check_pin_freshness(_report: &mut Report, _pinned: &Path, _mode: DoctorMode) {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn pin_state_of(name: &str, pinned: &Path) -> PinState {
    session_service::pin_state(
        pinned,
        session_service::installed_session_exe(name).as_deref(),
    )
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn pin_state_of(_name: &str, pinned: &Path) -> PinState {
    session_service::pin_state(pinned, None)
}

/// What only one platform can answer.
///
/// Every platform reports on every check, including the ones it cannot make - signing and TCC come
/// back as `n/a` on Linux rather than being left out. Silence there would read as "checked and
/// fine", which is the one answer that is never true.
#[cfg(target_os = "linux")]
fn platform_checks(
    report: &mut Report,
    name: &str,
    _pinned: Option<&Path>,
    _mode: DoctorMode,
    _opts: &CliArgs,
    _facts: &SessionFacts,
) {
    crate::session_doctor_linux::checks(report, name);
}

#[cfg(target_os = "macos")]
fn platform_checks(
    report: &mut Report,
    name: &str,
    pinned: Option<&Path>,
    mode: DoctorMode,
    opts: &CliArgs,
    facts: &SessionFacts,
) {
    // the same resolution `setup` makes, so a machine with ZELLIJ_CONFIG_DIR or an unusual XDG
    // layout gets its backup beside its own config rather than beside somebody's default
    let config_dir = opts.config_dir.clone().or_else(find_default_config_dir);
    crate::session_doctor_macos::checks(
        report,
        name,
        pinned,
        mode,
        config_dir,
        facts.assert_up().is_ok(),
    );
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn platform_checks(
    _report: &mut Report,
    _name: &str,
    _pinned: Option<&Path>,
    _mode: DoctorMode,
    _opts: &CliArgs,
    _facts: &SessionFacts,
) {
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(directory: &Path, name: &str, contents: &str) -> PathBuf {
        let path = directory.join(name);
        std::fs::write(&path, contents).unwrap();
        path
    }

    #[test]
    fn a_wrapper_that_sets_the_socket_directory_is_a_leftover() {
        let scratch = tempfile::tempdir().unwrap();
        let wrapper = write(
            scratch.path(),
            "zellij-attach",
            "#!/bin/sh\nexport ZELLIJ_SOCK_DIR=/tmp/mine\nexec zellij \"$@\"\n",
        );
        let faults = wrapper_faults(scratch.path(), None);
        assert_eq!(faults.len(), 1, "{:?}", faults);
        assert_eq!(faults[0].0, wrapper);
    }

    /// The report's one `rm` has to be earned. A companion script that merely calls zellij - the
    /// wrapper that runs doctor itself, most of all - is not a fault, and saying it is would have
    /// doctor ask somebody to delete a working tool.
    #[test]
    fn a_script_that_only_calls_zellij_is_left_alone() {
        let scratch = tempfile::tempdir().unwrap();
        write(
            scratch.path(),
            "zellij-mac-setup",
            "#!/bin/sh\nexec zellij session doctor \"$@\"\n",
        );
        assert!(wrapper_faults(scratch.path(), None).is_empty());
    }

    /// A `zellij` in the directory that IS this binary is where zellij is installed, and
    /// `check_path` has already reported it as the one a shell runs. Two checks disagreeing about
    /// one file, with `rm` as the advice, is the worst answer doctor can give.
    #[test]
    fn the_installed_zellij_is_not_called_a_shadow_of_itself() {
        let scratch = tempfile::tempdir().unwrap();
        let installed = write(scratch.path(), "zellij", "the real one");
        let resolved = installed.canonicalize().unwrap();
        assert!(wrapper_faults(scratch.path(), Some(&resolved)).is_empty());
    }

    /// A symlink to the running binary is an installation too - `~/bin` on PATH, pointing at the
    /// versioned path a package manager owns.
    #[test]
    fn a_symlink_to_this_binary_is_an_installation_and_not_a_shadow() {
        let scratch = tempfile::tempdir().unwrap();
        let real = write(scratch.path(), "zellij-0.45.0", "the real one");
        let link = scratch.path().join("zellij");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let faults = wrapper_faults(scratch.path(), Some(&real.canonicalize().unwrap()));
        assert!(faults.is_empty(), "{:?}", faults);
    }

    /// The shape the check exists for: another zellij entirely, earlier on PATH.
    #[test]
    fn another_zellij_under_the_same_name_is_a_shadow() {
        let scratch = tempfile::tempdir().unwrap();
        let other = write(scratch.path(), "zellij", "a different build");
        let elsewhere = tempfile::tempdir().unwrap();
        let ours = write(elsewhere.path(), "zellij", "this build");
        let faults = wrapper_faults(scratch.path(), Some(&ours.canonicalize().unwrap()));
        assert_eq!(faults.len(), 1, "{:?}", faults);
        assert_eq!(faults[0].0, other);
        assert!(faults[0].1.contains("shadows"));
    }

    /// A name that resolves to nothing is still a name a shell finds and fails to run.
    #[test]
    fn a_dangling_zellij_symlink_is_still_a_shadow() {
        let scratch = tempfile::tempdir().unwrap();
        let link = scratch.path().join("zellij");
        std::os::unix::fs::symlink(scratch.path().join("gone"), &link).unwrap();
        let ours = tempfile::tempdir().unwrap();
        let ours = write(ours.path(), "zellij", "this build");
        let faults = wrapper_faults(scratch.path(), Some(&ours.canonicalize().unwrap()));
        assert_eq!(faults.len(), 1, "{:?}", faults);
    }

    #[test]
    fn a_directory_that_is_not_there_holds_no_leftovers() {
        let scratch = tempfile::tempdir().unwrap();
        assert!(wrapper_faults(&scratch.path().join("no-such-bin"), None).is_empty());
    }
}
