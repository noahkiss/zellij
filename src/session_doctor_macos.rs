//! What only macOS can answer, and the only place doctor writes to a keychain.
//!
//! Four things, and each of them is invisible from every other angle. The temp directory a shell
//! exports decides which socket directory zellij resolves, so a shell whose `TMPDIR` disagrees
//! with the system's sees a different set of sessions under the same names. The launch agent's
//! label is what launchd holds the job under, and a plist whose label cannot be derived is a job
//! nothing here can find. Which session domain the server was created in decides, for the life of
//! that server, whether any pane in it can reach a TCC-gated file at all. And the signature on the
//! pinned copy decides whether the grants survive the next build.
//!
//! The domain and Full Disk Access are both asked of the SERVER, from inside a pane, because the
//! server is the process macOS attributes a pane's file access to. Nothing this process can ask
//! about itself answers for it: doctor runs from a terminal, and a terminal-launched process is
//! judged against the terminal's grants rather than zellij's. The pane is floating and closes
//! itself, which is the shape the shell script used, and it is the v1 answer - the server could
//! answer for itself over the existing IPC and that is a larger change than this one.

use std::path::{Path, PathBuf};

use zellij_utils::consts::ZELLIJ_TMP_DIR;
use zellij_utils::session_doctor::{Commander, DoctorMode, Finding, Report, SystemCommander};
use zellij_utils::session_lifecycle::launchctl;
use zellij_utils::session_service::{find_session_job, installed_launch_agents, launchd_label};
use zellij_utils::session_signing::{sign_pin, SigningContext, SigningDir};

/// How long to wait for a pane to write its answer before giving up on it.
///
/// A pane that is going to answer does so as fast as a shell starts. Waiting longer would only
/// lengthen the run on a machine where the session is wedged, which is a machine with a worse
/// problem that the checks above have already reported.
const PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const PROBE_POLL: std::time::Duration = std::time::Duration::from_millis(100);

pub(crate) fn checks(
    report: &mut Report,
    name: &str,
    pinned: Option<&Path>,
    mode: DoctorMode,
    config_dir: Option<PathBuf>,
    session_is_up: bool,
) {
    let commander = SystemCommander;
    check_tmpdir(report, &commander);
    check_launch_agent(report, name);
    check_from_inside_a_pane(report, name, session_is_up);
    check_signature(report, &commander, pinned, mode, config_dir);
}

/// Whether this shell's `TMPDIR` is the one the system hands out.
///
/// zellij derives its socket directory from `TMPDIR`, so a shell carrying somebody else's - a
/// `sudo` that kept it, a launcher that never had one, a wrapper that set it - resolves a
/// different directory and sees a different machine. Two servers under one name, each invisible to
/// the other's clients, is what that looks like from the outside.
///
/// Reported and never fixed. The value was set by this process's parent before this process
/// existed; changing it here would change nothing for the shell that will run the next command,
/// and a fix that does not outlive the run is worse than a finding.
fn check_tmpdir(report: &mut Report, commander: &SystemCommander) {
    let Ok(system) = commander.run("getconf", &["DARWIN_USER_TEMP_DIR"], None) else {
        report.push(Finding::ok(
            "tmpdir",
            "getconf would not say what the system's temp directory is",
        ));
        return;
    };
    let system = system.stdout.trim().trim_end_matches('/').to_owned();
    let ours = std::env::var("TMPDIR")
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_owned();
    if system.is_empty() {
        // silence here would read as "checked, and fine", which is the one thing it is not
        report.push(Finding::ok(
            "tmpdir",
            "getconf named no temp directory, so there is nothing to compare this shell against",
        ));
        return;
    }
    if ours == system {
        report.push(Finding::ok("tmpdir", format!("{} - the system's", system)));
        return;
    }
    report.push(
        Finding::needs_you("tmpdir", format!("this shell has TMPDIR={}", ours))
            .note(format!("the system hands out {}", system))
            .note("zellij derives its socket directory from it, so this shell sees a different")
            .note("set of sessions under the same names. Nothing inside this process can fix it -")
            .note("the value came from whatever started this shell. Find it and unset it."),
    );
}

/// Which label launchd holds the job for this session under.
///
/// The plist on disk says which job it is; launchd says whether it is loaded. Both, because they
/// come apart: a plist that was never bootstrapped is a job that does not exist, and a job loaded
/// from a file this scan cannot see is one that does.
fn check_launch_agent(report: &mut Report, name: &str) {
    let derived = launchd_label(name);
    let agents = installed_launch_agents();
    let found = find_session_job(&agents, name, &derived);
    let label = found
        .job()
        .map(|job| job.name.clone())
        .unwrap_or_else(|| derived.clone());

    if launchctl::job_is_installed(&label) {
        let mut finding = Finding::ok(
            "agent",
            format!("{} is loaded in {}", label, launchctl::gui_domain()),
        );
        if label != derived {
            // a label this build did not choose is not a fault, and saying nothing about it would
            // leave a reader wondering where the name came from
            finding = finding.note(format!(
                "under a name this build did not choose ({})",
                derived
            ));
        }
        report.push(finding);
        return;
    }
    if !launchctl::gui_domain_exists() {
        report.push(
            Finding::needs_you(
                "agent",
                "there is no graphical login session to hold the job",
            )
            .note("a session created here could never reach a TCC-gated file, the login")
            .note("keychain, the pasteboard or notifications. Log in graphically first."),
        );
        return;
    }
    report.push(
        Finding::needs_you("agent", format!("{} is not loaded", label))
            .note(format!(
                "`zellij session enable {}` writes and bootstraps it",
                name
            ))
            .note("a changed plist needs bootout then bootstrap; kickstart reruns the old one"),
    );
}

/// The two questions only the server can answer, asked from inside one of its panes.
///
/// A pane inherits the server's session domain and macOS attributes a pane's file access to the
/// server's executable, so a pane is where both answers actually live. Skipped rather than guessed
/// when the session is down: an answer about a server that does not exist would be an answer about
/// this terminal.
fn check_from_inside_a_pane(report: &mut Report, name: &str, session_is_up: bool) {
    if !session_is_up {
        report.push(
            Finding::ok(
                "probe",
                format!("'{}' is not up, so there is no server to ask", name),
            )
            .note("the session domain and Full Disk Access are properties of the server,")
            .note("and this terminal's answers would not be its answers"),
        );
        return;
    }
    let Some(answer) = run_pane_probe(name) else {
        report.push(
            Finding::needs_you("probe", "the pane probe did not answer in time")
                .note("the session domain and Full Disk Access could not be checked"),
        );
        return;
    };

    match answer.manager.as_deref() {
        Some(zellij_utils::session_lifecycle::GUI_MANAGER_NAME) => report.push(Finding::ok(
            "domain",
            "the server is in the graphical session, so its panes can reach TCC and the keychain",
        )),
        Some(other) => report.push(
            Finding::needs_you("domain", format!("the server is in the {} domain", other))
                .note("its panes cannot reach a TCC-gated file, the login keychain, the")
                .note("pasteboard or notifications - and attaching from a graphical terminal")
                .note("does not change it. The domain is fixed when the server is created.")
                .note(format!(
                    "`zellij session restart {}` from a graphical login fixes it",
                    name
                )),
        ),
        None => report.push(Finding::ok(
            "domain",
            "launchctl in the pane would not say which domain the server is in",
        )),
    }

    match answer.full_disk_access {
        Some(true) => report.push(Finding::ok(
            "fda",
            "the server has Full Disk Access, so every pane does",
        )),
        Some(false) => report.push(
            Finding::needs_you("fda", "the server does NOT have Full Disk Access")
                .note("every pane sees \"Operation not permitted\" in a protected directory,")
                .note("whatever it runs. No program can grant this - Apple offers no API for it.")
                .note("System Settings > Privacy & Security > Full Disk Access, and add the")
                .note("EXACT path the server runs; the grant is keyed to it and to its signature."),
        ),
        None => report.push(Finding::ok(
            "fda",
            "the probe could not tell whether Full Disk Access is granted",
        )),
    }
}

/// What one pane came back with.
struct PaneAnswer {
    manager: Option<String>,
    full_disk_access: Option<bool>,
}

/// Open a floating pane that writes two lines to a file and closes itself.
///
/// The file rather than the pane's screen, because reading a pane back means dumping and parsing a
/// terminal, and a shell prompt or a slow render would be indistinguishable from an answer. Under
/// zellij's own temp directory, which is the one directory both this process and the server agree
/// on by construction.
///
/// The client is spawned and never waited on, which is the one place doctor does not go through
/// [`Commander`]: that trait runs a command to completion, and a client talking to a wedged server
/// never completes. A machine whose session is stuck is exactly the machine somebody runs doctor
/// on, so the deadline below has to cover the client as well as its answer.
fn run_pane_probe(name: &str) -> Option<PaneAnswer> {
    let answer_file = ZELLIJ_TMP_DIR.join(format!("doctor-probe-{}", std::process::id()));
    let _ = std::fs::remove_file(&answer_file);
    let answer_path = answer_file.display().to_string();
    // The OPEN is the probe, and it has to be a real one: `[ -r ]` calls `access(2)`, which reads
    // the permission bits, while TCC refuses at `open(2)` instead - so a test on the bits answers
    // "readable" on a machine holding no grant at all. Nothing is read out of the file beyond the
    // one byte it takes to prove the open was allowed.
    //
    // A TCC.db that is not there is not a refusal, and the two are reported apart rather than
    // guessed at. `[ -e ]` is safe to ask: TCC gates the open, not the stat.
    //
    // `$HOME` is not trusted to be set - the server's environment comes from launchd rather than
    // from a login shell - so it falls back to the `eval echo ~user` idiom the vitals probe uses.
    let script = format!(
        "{{ printf 'manager=%s\\n' \"$(launchctl managername 2>/dev/null)\"; \
         home=${{HOME:-$(eval echo ~\"$(id -un)\")}}; \
         db=\"$home/Library/Application Support/com.apple.TCC/TCC.db\"; \
         if [ ! -e \"$db\" ]; then echo fda=unknown; \
         elif head -c 1 \"$db\" >/dev/null 2>&1; then echo fda=yes; \
         else echo fda=no; fi; }} > {} 2>/dev/null",
        shell_quote(&answer_path)
    );

    let zellij = std::env::current_exe().ok()?;
    let mut client = std::process::Command::new(&zellij)
        .args([
            "--session",
            name,
            "run",
            "--floating",
            "--close-on-exit",
            "--name",
            "zellij session doctor",
            "--",
            "sh",
            "-c",
            &script,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;

    let deadline = std::time::Instant::now() + PROBE_TIMEOUT;
    loop {
        if let Ok(written) = std::fs::read_to_string(&answer_file) {
            if written.contains("fda=") {
                let _ = std::fs::remove_file(&answer_file);
                reap(&mut client);
                return Some(parse_pane_answer(&written));
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = std::fs::remove_file(&answer_file);
            reap(&mut client);
            return None;
        }
        std::thread::sleep(PROBE_POLL);
    }
}

/// Take the client down if it is still up, and collect it either way.
fn reap(client: &mut std::process::Child) {
    if matches!(client.try_wait(), Ok(None)) {
        let _ = client.kill();
    }
    let _ = client.wait();
}

fn parse_pane_answer(written: &str) -> PaneAnswer {
    let value = |key: &str| {
        written
            .lines()
            .find_map(|line| line.trim().strip_prefix(key).map(str::to_owned))
    };
    PaneAnswer {
        manager: value("manager=").filter(|value| !value.is_empty()),
        full_disk_access: value("fda=").and_then(|value| match value.as_str() {
            "yes" => Some(true),
            "no" => Some(false),
            _ => None,
        }),
    }
}

/// Wrap a path for a shell, the only way that is right for every path: single quotes, with any
/// single quote inside them closed and re-opened around an escaped one.
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Bring the pinned copy's signature to something that outlives the build.
///
/// Everything about how that is done lives in
/// [`session_signing`](zellij_utils::session_signing); what is here is only the three paths that
/// module cannot know: where our own certificate is kept, which keychain to put it in, and where
/// to leave a second copy of it.
fn check_signature(
    report: &mut Report,
    commander: &SystemCommander,
    pinned: Option<&Path>,
    mode: DoctorMode,
    config_dir: Option<PathBuf>,
) {
    let Some(pinned) = pinned else {
        // `check_pin` has already said that the pin is off, and everything below it is skipped
        return;
    };
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        report.push(Finding::needs_you(
            "signing",
            "no HOME, so there is nowhere to keep a signing certificate",
        ));
        return;
    };
    let context = SigningContext {
        signing_dir: SigningDir::new(home.join("Library/Application Support/zellij/signing")),
        keychain: default_keychain(commander),
        // an environment variable holding a password is not a thing to want; it is the only way a
        // run over SSH can answer the keychain's dialog, and a run that cannot answer it hangs
        keychain_password: std::env::var("ZELLIJ_KEYCHAIN_PASSWORD").ok(),
        backup_dir: config_dir,
    };
    report.extend(sign_pin(commander, pinned, mode, &context).findings);
}

/// The keychain `codesign` will look in.
///
/// Asked rather than assumed: `login.keychain-db` is the answer on almost every machine and not on
/// all of them, and importing into a keychain nothing searches is an import that reports success
/// and leaves nothing able to sign.
fn default_keychain(commander: &SystemCommander) -> String {
    commander
        .run("security", &["default-keychain", "-d", "user"], None)
        .ok()
        .map(|output| output.stdout.trim().trim_matches('"').to_owned())
        .filter(|keychain| !keychain.is_empty())
        .unwrap_or_else(|| String::from("login.keychain-db"))
}
