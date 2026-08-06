//! Bringing a session up, tearing it down, and PROVING which of the two happened.
//!
//! Every session operation is scoped to one socket directory, resolved from the environment, and
//! `zellij ls` reads only its own. A launcher whose environment differs from a login shell's
//! therefore does not create a MISPLACED session - it creates an INVISIBLE one, and the next client
//! silently builds a second server for the same name. Nothing warns, because from inside either
//! environment everything looks right.
//!
//! The answer is not another layer of environment agreement: it is to state the post-condition and
//! check it. After `session up` there is to be exactly one server process for this name, and its
//! socket is to be the one path this binary resolved. Anything else is a fault with diagnostics,
//! not a session. That catches the next unknown variable too, without anyone predicting which.
//!
//! Nothing here reads `ZELLIJ_SOCKET_DIR`. [`ZELLIJ_SOCK_DIR`] already resolves it - honouring an
//! explicit override, deriving the value otherwise - so the checker and the thing it checks cannot
//! disagree by construction.

use crate::consts::ZELLIJ_SOCK_DIR;
use crate::sessions::{
    get_sessions_in_other_socket_dirs, session_exists, session_in_other_contract_versions,
    DeletedSession,
};
use std::path::{Path, PathBuf};

/// A live `zellij --server` process, as the process table describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerProcess {
    pub pid: u32,
    pub socket: PathBuf,
}

impl ServerProcess {
    /// The session a server serves is the last component of its socket path.
    pub fn session_name(&self) -> Option<&str> {
        self.socket.file_name().and_then(|n| n.to_str())
    }
}

/// The zellij servers named in `ps -eo pid=,command=` output.
///
/// Matched on the SHAPE of the command line rather than on the process name: the server's argv[0]
/// is the resolved binary path, which is not always literally "zellij" (a symlinked or renamed
/// build resolves to its real name). A zellij server's argv is exactly
/// `<binary> --server <socket-path>`, so `--server` must be the second-to-last field - an unrelated
/// `ssh ... rsync --server ...` carries the flag too and would otherwise be counted the moment one
/// of its paths happened to end in a session name.
pub fn parse_server_processes(ps_output: &str) -> Vec<ServerProcess> {
    ps_output
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 || fields[fields.len() - 2] != "--server" {
                return None;
            }
            Some(ServerProcess {
                pid: fields[0].parse().ok()?,
                socket: PathBuf::from(fields[fields.len() - 1]),
            })
        })
        .collect()
}

/// Every zellij server running on this machine, in any socket directory.
///
/// `ps` rather than `/proc`, because this has to answer the same question on the BSDs and on macOS.
/// Sockets are not consulted at all: the point of the scan is to find servers this environment
/// cannot reach, and an unreachable server has no socket here to be found by.
#[cfg(unix)]
pub fn running_servers() -> Vec<ServerProcess> {
    let output = match std::process::Command::new("ps")
        .args(["-eo", "pid=,command="])
        .output()
    {
        Ok(output) => output,
        Err(e) => {
            log::debug!("Could not list processes: {}", e);
            return Vec::new();
        },
    };
    let me = std::process::id();
    parse_server_processes(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        // an argv of ours that happens to end in `--server <path>` is not a server
        .filter(|server| server.pid != me)
        .collect()
}

/// Windows has no process table to walk portably; the socket checks carry the assertion there.
#[cfg(not(unix))]
pub fn running_servers() -> Vec<ServerProcess> {
    Vec::new()
}

/// The servers serving this session name, wherever their sockets live.
pub fn servers_for_session(name: &str) -> Vec<ServerProcess> {
    running_servers()
        .into_iter()
        .filter(|server| server.session_name() == Some(name))
        .collect()
}

/// What the machine says about one session name, gathered once so a check and its diagnostics
/// cannot describe two different moments.
#[derive(Debug, Clone)]
pub struct SessionFacts {
    pub name: String,
    /// The socket directory this binary resolved, with no help from the environment.
    pub socket_dir: PathBuf,
    /// Where this session's socket belongs, if it is ours.
    pub expected_socket: PathBuf,
    /// Servers serving this name anywhere on the machine.
    pub servers: Vec<ServerProcess>,
    pub socket_exists: bool,
    /// Whether the session is one `zellij ls` would list here.
    pub listed: bool,
}

impl SessionFacts {
    pub fn collect(name: &str) -> Self {
        let expected_socket = ZELLIJ_SOCK_DIR.join(name);
        SessionFacts {
            name: name.to_owned(),
            socket_dir: ZELLIJ_SOCK_DIR.clone(),
            socket_exists: expected_socket.exists(),
            expected_socket,
            servers: servers_for_session(name),
            listed: session_exists(name).unwrap_or(false),
        }
    }

    /// The servers that are ours: serving this name from the socket this binary resolved.
    pub fn our_servers(&self) -> Vec<&ServerProcess> {
        self.servers
            .iter()
            .filter(|server| server.socket == self.expected_socket)
            .collect()
    }

    /// The servers serving this name from somewhere else - the invisible-duplicate case.
    pub fn foreign_servers(&self) -> Vec<&ServerProcess> {
        self.servers
            .iter()
            .filter(|server| server.socket != self.expected_socket)
            .collect()
    }

    /// The post-condition of `session up`: one server, ours, listening on a socket that is there.
    pub fn assert_up(&self) -> Result<(), String> {
        assert_up_from(
            &self.servers,
            &self.expected_socket,
            self.socket_exists,
            self.listed,
            cfg!(unix),
        )
    }

    /// The post-condition of `session down`: no server for this name anywhere, no socket left.
    pub fn assert_down(&self) -> Result<(), String> {
        if !self.servers.is_empty() {
            return Err(format!(
                "{} server process(es) still serving session '{}'",
                self.servers.len(),
                self.name
            ));
        }
        if self.socket_exists {
            return Err(format!(
                "socket {} is still present",
                self.expected_socket.display()
            ));
        }
        Ok(())
    }

    /// Say what is on the machine, in the terms that usually explain the failure.
    ///
    /// A server in another socket directory or under another contract version is the ordinary cause
    /// of both assertions failing, and it is invisible to every other command, so it is named here
    /// rather than left for the reader to go looking for.
    pub fn print_diagnostics(&self) {
        eprintln!("  session        : {}", self.name);
        eprintln!("  socket dir     : {}", self.socket_dir.display());
        for server in &self.servers {
            if server.socket == self.expected_socket {
                eprintln!("  server pid {} -> {}", server.pid, server.socket.display());
            } else {
                eprintln!(
                    "  server pid {} -> {} (OUTSIDE this socket dir - invisible to this binary)",
                    server.pid,
                    server.socket.display()
                );
            }
        }
        let others: Vec<ServerProcess> = running_servers()
            .into_iter()
            .filter(|server| server.session_name() != Some(self.name.as_str()))
            .collect();
        if !others.is_empty() {
            eprintln!("  other zellij servers on this machine:");
            for server in others {
                eprintln!("    pid {} -> {}", server.pid, server.socket.display());
            }
        }
        for contract in session_in_other_contract_versions(&self.name) {
            eprintln!(
                "  a session by this name also has a socket under contract version {}",
                contract
            );
        }
        for (dir, sessions) in get_sessions_in_other_socket_dirs() {
            eprintln!(
                "  {} live session(s) in {}: {}",
                sessions.len(),
                dir.display(),
                sessions.join(", ")
            );
        }
        eprintln!(
            "  sessions visible here: {}",
            match crate::sessions::get_sessions() {
                Ok(sessions) if sessions.is_empty() => "(none)".to_owned(),
                Ok(sessions) => sessions
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>()
                    .join(", "),
                Err(e) => format!("(could not be listed: {:?})", e),
            }
        );
    }
}

/// The up post-condition, separated from the machine so it can be exercised without one.
///
/// `have_process_table` is false where no portable process scan exists; the socket then carries the
/// whole assertion, which is weaker but not wrong.
fn assert_up_from(
    servers: &[ServerProcess],
    expected_socket: &Path,
    socket_exists: bool,
    listed: bool,
    have_process_table: bool,
) -> Result<(), String> {
    if have_process_table {
        let ours = servers
            .iter()
            .filter(|s| s.socket == expected_socket)
            .count();
        let elsewhere = servers.len() - ours;
        if servers.is_empty() {
            return Err("no 'zellij --server' process for this session".to_owned());
        }
        if servers.len() > 1 {
            return Err(format!(
                "{} server processes for this session (expected exactly 1)",
                servers.len()
            ));
        }
        if elsewhere > 0 {
            return Err(
                "the server for this session is listening outside the socket directory this binary \
                 resolved, so it is invisible here"
                    .to_owned(),
            );
        }
    }
    if !socket_exists {
        return Err(format!(
            "socket {} does not exist",
            expected_socket.display()
        ));
    }
    if !listed {
        return Err("the session is not listed by this binary".to_owned());
    }
    Ok(())
}

/// What a `session down` did, judged from what the removal found and whether the post-condition
/// then held.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownOutcome {
    /// The session was there, and is gone.
    Removed,
    /// There was nothing to remove, and the name is down all the same.
    ///
    /// This is a success. The caller asked for a state the machine was already in, and got it; a
    /// `restart` that gave up here - `restart` being a `down` followed by an `up` - would leave the
    /// user with no session at all over a session that was already absent.
    NothingToRemove,
    /// Something is still serving the name, or the removal itself failed.
    Failed(String),
}

impl DownOutcome {
    /// The post-condition is checked first and decides on its own: what is still serving the name
    /// is worth reporting whatever the removal thought it did.
    pub fn judge(deleted: DeletedSession, post_condition: Result<(), String>) -> Self {
        if let Err(reason) = post_condition {
            return DownOutcome::Failed(reason);
        }
        if !deleted.killed {
            return DownOutcome::Failed(
                "the server was still running when the wait ran out".to_owned(),
            );
        }
        if deleted.found {
            DownOutcome::Removed
        } else {
            DownOutcome::NothingToRemove
        }
    }

    pub fn is_failure(&self) -> bool {
        matches!(self, DownOutcome::Failed(_))
    }
}

/// What `session up` should do about the macOS session domain before it creates anything.
///
/// macOS puts every process in a session domain, and only the graphical one - launchd calls it
/// `Aqua` - can reach TCC-gated resources, the login keychain, the pasteboard or notifications. The
/// domain is fixed when the server is created and inherited by every pane in it; attaching later
/// never changes it. So whoever creates the session first decides this for its whole life, and a
/// shell over SSH is not in the graphical session: create it from there and the session is
/// permanently without that access, with nothing anywhere saying so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiDomainAction {
    /// Nothing to arrange - already graphical, or the session exists and the question is settled.
    Proceed,
    /// Ask launchd to start this job, so the session is created in the graphical session rather
    /// than in this one.
    Kickstart(String),
    /// Create it here, and say what it will not be able to do. There is no job to defer to, and
    /// refusing would leave the user with no session over a capability they may not want.
    ProceedWithoutGui,
    /// Nobody is logged in graphically, so there is no session domain to create it in. Reported
    /// rather than quietly answered with the wrong domain.
    NoGuiSession,
}

/// Decide from what the machine says: the domain this process is in, whether a graphical session
/// exists at all, the label of an installed job for this session if there is one, and whether the
/// session is already there.
pub fn gui_domain_action(
    manager_name: Option<&str>,
    gui_domain_available: bool,
    installed_job: Option<&str>,
    session_exists: bool,
) -> GuiDomainAction {
    // an existing session already has a domain, and `up` is not going to replace it over this
    if session_exists {
        return GuiDomainAction::Proceed;
    }
    if manager_name == Some(GUI_MANAGER_NAME) {
        return GuiDomainAction::Proceed;
    }
    if !gui_domain_available {
        return GuiDomainAction::NoGuiSession;
    }
    match installed_job {
        Some(label) => GuiDomainAction::Kickstart(label.to_owned()),
        None => GuiDomainAction::ProceedWithoutGui,
    }
}

/// What `launchctl managername` calls the graphical login session.
pub const GUI_MANAGER_NAME: &str = "Aqua";

/// Asking launchd what this process is in and what it has been given to run.
///
/// Compiled under `cfg(test)` on every unix so that the macOS-only path cannot rot unnoticed on a
/// machine that never builds it; nothing outside macOS calls it.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub mod launchctl {
    use std::process::Command;

    /// The session domain this process is in. `None` when launchctl will not say - an answer we do
    /// not have is not one to act on.
    pub fn manager_name() -> Option<String> {
        let output = Command::new("launchctl").arg("managername").output().ok()?;
        if !output.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }

    /// The graphical domain of the user running this process.
    pub fn gui_domain() -> String {
        format!("gui/{}", unsafe { libc::getuid() })
    }

    /// Whether there is a graphical login session for this user at all.
    pub fn gui_domain_exists() -> bool {
        print_succeeds(&gui_domain())
    }

    /// Whether a job by this label is loaded in the graphical domain.
    pub fn job_is_installed(label: &str) -> bool {
        print_succeeds(&format!("{}/{}", gui_domain(), label))
    }

    /// Start the job. It returns 0 from a non-graphical shell too: what the domain of the CALLER
    /// is has no bearing on the domain the job runs in, which is the point.
    pub fn kickstart(label: &str) -> Result<(), String> {
        let target = format!("{}/{}", gui_domain(), label);
        match Command::new("launchctl")
            .args(["kickstart", &target])
            .output()
        {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => Err(format!(
                "launchctl kickstart {} failed: {}",
                target,
                String::from_utf8_lossy(&output.stderr).trim()
            )),
            Err(e) => Err(format!("could not run launchctl: {}", e)),
        }
    }

    fn print_succeeds(target: &str) -> bool {
        Command::new("launchctl")
            .args(["print", target])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

/// Arrange for the session to be created in the graphical session, or say why it will not be.
///
/// `Ok(true)` means launchd has been asked for it and the caller is to wait for the session rather
/// than create it itself.
/// Compiled under `cfg(test)` on every unix for the same reason [`launchctl`] is.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub fn ensure_gui_session_domain(session: &str, session_exists: bool) -> Result<bool, String> {
    use crate::session_service::{find_session_job, installed_launch_agents, SessionJob};

    let derived = crate::session_service::launchd_label(session);
    let agents = installed_launch_agents();
    let found = find_session_job(&agents, session, &derived);
    if let SessionJob::Ambiguous(all) = &found {
        eprintln!(
            "warning: {} launch agents run `session up {}`: {}",
            all.len(),
            session,
            all.iter()
                .map(|job| job.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    // A plist on disk that launchd was never given is not a job that can be started, and a job may
    // equally have been loaded from a file this scan cannot see - so the disk says WHICH job it is
    // and launchd says whether it is there. Falling back to the derived label keeps the case this
    // build installed itself working even if its file has been moved.
    let label = found
        .job()
        .map(|job| job.name.clone())
        .filter(|label| launchctl::job_is_installed(label))
        .or_else(|| launchctl::job_is_installed(&derived).then(|| derived.clone()));
    let action = gui_domain_action(
        launchctl::manager_name().as_deref(),
        launchctl::gui_domain_exists(),
        label.as_deref(),
        session_exists,
    );
    match action {
        GuiDomainAction::Proceed => Ok(false),
        GuiDomainAction::Kickstart(label) => {
            // say why this label, when it is not the one this build would have installed: from the
            // outside the choice would otherwise look like it came from nowhere
            if label != derived {
                println!(
                    "      '{}' is kept up by the launch agent '{}', installed under a name this \
                     build did not choose",
                    session, label
                );
            }
            println!(
                "      asking launchd for '{}' in the graphical session",
                label
            );
            launchctl::kickstart(&label)?;
            Ok(true)
        },
        GuiDomainAction::ProceedWithoutGui => {
            eprintln!(
                "warning: creating '{}' outside the graphical session. Every pane in it inherits \n         \
                 that, for as long as the server lives, and attaching from a graphical terminal \n         \
                 later does not change it: access to TCC-gated resources, the login keychain, the \n         \
                 pasteboard and notifications will be unavailable.\n         \
                 No loaded launch agent was found naming `session up {}` - one that reaches it\n         \
                 through a wrapper script may not be recognisable from its plist.\n         \
                 Install one to avoid this: zellij session enable {}",
                session, session, session
            );
            Ok(false)
        },
        GuiDomainAction::NoGuiSession => Err(format!(
            "there is no graphical login session to create '{}' in, and creating it here would \
             give it a session domain it could never leave. Log in graphically first, or create \
             the session deliberately from a shell in that session.",
            session
        )),
    }
}

/// Re-exported so the term logic and its callers still read `session_lifecycle::DEFAULT_TERM`.
/// The const itself lives in `shared` because `session_service` needs it and is built for wasm,
/// while this module is not - see the note beside its definition.
pub use crate::shared::DEFAULT_TERM;

/// The TERM to give a session being CREATED, or `None` to keep what is already there.
///
/// The server hands its own environment to every pane shell it spawns, so whatever TERM the creator
/// had is the TERM of every pane, for the life of the session. A launcher - a launch agent, a
/// systemd user unit - is not a login shell and has no TERM at all, and the shells in those panes
/// then come up with `TERM=dumb`: keys repeat, and programs report that TERM is not set. It stayed
/// hidden for as long as a login shell always won the race to create the session, because a shell
/// always has one.
///
/// `dumb` is treated as absent rather than as a choice. It is what an environment with no terminal
/// type produces, and it is never what a pane wants. Anything else is left alone: a real terminal
/// knows what it is better than this does.
pub fn term_for_new_session(current: Option<&str>) -> Option<&'static str> {
    match current {
        Some(term) if !term.is_empty() && term != "dumb" => None,
        _ => Some(DEFAULT_TERM),
    }
}

/// What a truecolor-capable terminal puts in COLORTERM, and what zellij's renderer is.
pub const DEFAULT_COLORTERM: &str = "truecolor";

/// The COLORTERM to give a session being CREATED, or `None` to keep what is already there.
///
/// The same argument as [`term_for_new_session`], and the same place in the code, because it is the
/// same fact: this describes the CONNECTION, not the user. zellij's own renderer emits 24-bit
/// colour to the pane, so a pane inside it really is looking at a truecolor surface - the value is
/// true of the thing on the other end of the pty whatever the user would have preferred. That is
/// what separates it from a locale, which is a preference the rc chain re-derives for itself.
///
/// A launcher supplies none, and the server hands its environment to every pane, so without it
/// nvim colourschemes, delta, bat and eza fall back to 256 colours in a launcher-created session
/// and look right in a terminal opened beside it - the same session, judged differently by the two
/// panes. Anything already set is left alone: a terminal that says something about itself is
/// better informed than this is.
pub fn colorterm_for_new_session(current: Option<&str>) -> Option<&'static str> {
    match current {
        Some(colorterm) if !colorterm.is_empty() => None,
        _ => Some(DEFAULT_COLORTERM),
    }
}

/// Whether one `session_restart_drop_env` pattern names this variable.
///
/// Two cases and no more: an exact name, or a name ending in `*`, which matches by prefix. A `*`
/// anywhere else is an ordinary character and matches itself - environment variable names are
/// matched, not globbed, and half a glob implementation is worse than none, because the pattern
/// that silently means something else is the one that drops the wrong variable.
fn drop_pattern_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name.starts_with(prefix),
        None => pattern == name,
    }
}

/// The variable names among `names` that `patterns` names, in the order they were given.
///
/// A pattern that matches nothing is not an error: config is written once and travels between
/// machines, where the program it describes may not be installed.
pub fn env_vars_to_drop<'a>(
    patterns: &[String],
    names: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    names
        .into_iter()
        .filter(|name| {
            patterns
                .iter()
                .any(|pattern| drop_pattern_matches(pattern, name))
        })
        .map(|name| name.to_owned())
        .collect()
}

/// Which build is serving a session, relative to the binary that is asking.
///
/// `Unknown` is not a fault to report. A client that cannot see the server's executable knows
/// nothing about it, and a wrong "your session is stale" costs more than saying nothing: it sends
/// someone to restart a session that did not need restarting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMatch {
    Same,
    Different,
    Unknown,
}

/// A running program's executable, identified by more than its name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutableIdentity {
    /// The path, canonicalised where it could be. A package manager installs a stable name that is
    /// a symlink into a versioned directory, so two spellings routinely name one build.
    pub path: PathBuf,
    /// Device and inode, where the file could be stat'ed. One file has one pair whatever it is
    /// called, and a replacement gets a new one, so this decides wherever it is available.
    pub file_id: Option<(u64, u64)>,
    /// The file the process started from is no longer at its path - an upgrade wrote over it in
    /// place. Linux says so by appending " (deleted)" to the `/proc/<pid>/exe` link.
    pub replaced: bool,
}

/// What can be learnt about the file at `path`, without failing if the answer is "not much".
fn identify_executable(path: PathBuf) -> ExecutableIdentity {
    let (path, replaced) = match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(real_path) => (PathBuf::from(real_path), true),
        None => (path, false),
    };
    let path = path.canonicalize().unwrap_or(path);
    // a replaced file's path may now hold its replacement, whose inode is not the running one's
    #[cfg(unix)]
    let file_id = if replaced {
        None
    } else {
        std::fs::metadata(&path).ok().map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        })
    };
    #[cfg(not(unix))]
    let file_id: Option<(u64, u64)> = None;
    ExecutableIdentity {
        path,
        file_id,
        replaced,
    }
}

/// Whether two executables are the same build, judged only on what was actually established.
///
/// Inodes decide it where both sides have them. Where they do not, only agreement is trustworthy:
/// two paths that differ may still be two names for one build, and calling that a mismatch is the
/// false alarm this is written to avoid.
pub fn compare_builds(
    ours: Option<&ExecutableIdentity>,
    theirs: Option<&ExecutableIdentity>,
) -> BuildMatch {
    let (Some(ours), Some(theirs)) = (ours, theirs) else {
        return BuildMatch::Unknown;
    };
    if ours.replaced {
        // our own file has been replaced too, so there is nothing left on disk to compare against
        return BuildMatch::Unknown;
    }
    if theirs.replaced {
        // ours is on disk and theirs is not, so they cannot be the same file
        return BuildMatch::Different;
    }
    match (ours.file_id, theirs.file_id) {
        (Some(ours), Some(theirs)) if ours == theirs => BuildMatch::Same,
        (Some(_), Some(_)) => BuildMatch::Different,
        _ if ours.path == theirs.path => BuildMatch::Same,
        _ => BuildMatch::Unknown,
    }
}

/// The executable a running process started from.
///
/// `/proc/<pid>/exe` is the whole answer on Linux: it is the kernel's own reference to the file,
/// and it says so when the file has since been replaced.
#[cfg(target_os = "linux")]
fn executable_of_pid(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{}/exe", pid)).ok()
}

/// The executable a running process started from.
///
/// macOS has no `/proc`, and `ps -o comm=` is not a substitute - it is truncated at the column
/// width. `proc_pidpath` is the kernel asked directly, and fills a buffer with the full path.
#[cfg(target_os = "macos")]
fn executable_of_pid(pid: u32) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    // PROC_PIDPATHINFO_MAXSIZE, which is not exposed by the libc crate
    let mut buffer = vec![0u8; 4096];
    let length = unsafe {
        libc::proc_pidpath(
            pid as libc::c_int,
            buffer.as_mut_ptr() as *mut libc::c_void,
            buffer.len() as u32,
        )
    };
    if length <= 0 {
        return None;
    }
    buffer.truncate(length as usize);
    Some(PathBuf::from(std::ffi::OsString::from_vec(buffer)))
}

/// Everywhere else there is no portable way to ask, so nothing is claimed.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn executable_of_pid(_pid: u32) -> Option<PathBuf> {
    None
}

/// The build behind the server serving `name` from the socket directory this binary resolved.
///
/// Exactly one such server, or nothing: two servers for one name is its own fault, reported by the
/// post-conditions, and guessing which of them a client will reach would be inventing an answer.
pub fn server_executable(name: &str) -> Option<ExecutableIdentity> {
    let expected_socket = ZELLIJ_SOCK_DIR.join(name);
    let mut ours = servers_for_session(name)
        .into_iter()
        .filter(|server| server.socket == expected_socket);
    let server = ours.next()?;
    if ours.next().is_some() {
        return None;
    }
    executable_of_pid(server.pid).map(identify_executable)
}

/// The build of the binary running right now.
pub fn own_executable() -> Option<ExecutableIdentity> {
    std::env::current_exe().ok().map(identify_executable)
}

/// What to say about a session served by a build that is not this one, if it is.
///
/// A server keeps the binary it started with for the whole life of the session, so upgrading the
/// package changes nothing until the session is restarted. Nothing else says so, and a machine can
/// therefore sit on a superseded build for days while everyone believes the upgrade took effect.
pub fn build_mismatch_warning(name: &str) -> Option<String> {
    let ours = own_executable();
    let theirs = server_executable(name);
    if compare_builds(ours.as_ref(), theirs.as_ref()) != BuildMatch::Different {
        return None;
    }
    let (ours, theirs) = (ours?, theirs?);
    Some(format!(
        "warning: session '{}' is running a different build of zellij than this binary.\n  \
         running: {}\n  this:    {}\n  \
         A server keeps the binary it started with, so an upgrade does not reach a running \
         session.\n  Run `zellij session restart {}` to bring it onto this build.",
        name,
        theirs.path.display(),
        ours.path.display(),
        name
    ))
}

/// Say it, at most once for the life of this process.
///
/// A client talks to its server many times over; the mismatch is one fact about the session and is
/// worth one line, not one line per action, per reconnect or per render.
pub fn warn_if_server_build_differs(name: &str) {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        if let Some(warning) = build_mismatch_warning(name) {
            eprintln!("{}", warning);
        }
    });
}

/// What one probe of a TCC-protected location found.
///
/// The interesting distinction is not allowed-vs-refused, it is whether a DECISION EXISTS. macOS
/// asks the user once per client and remembers the answer forever; a refusal therefore means the
/// question has already been put and answered, and putting it again is not possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TccProbe {
    /// Opened. Either a grant exists, or none was needed, or the user has just been asked and said
    /// yes - from here the three are indistinguishable, and none of them is a problem.
    Reachable,
    /// macOS refused. A decision is already recorded as denied, so nothing prompted and nothing
    /// will.
    Refused,
    /// Not there to probe. Says nothing about permissions.
    Absent,
    /// Some other I/O failure. NOT a permission answer - do not report it as one.
    Inconclusive,
}

/// Classify a probe from the error it did or did not produce.
///
/// Split from the probing so the mapping can be tested on any platform: the part worth getting
/// right is that only `PermissionDenied` means refused, and every other failure is inconclusive
/// rather than quietly folded in with it.
pub fn classify_tcc_probe(error: Option<std::io::ErrorKind>) -> TccProbe {
    match error {
        None => TccProbe::Reachable,
        Some(std::io::ErrorKind::PermissionDenied) => TccProbe::Refused,
        Some(std::io::ErrorKind::NotFound) => TccProbe::Absent,
        Some(_) => TccProbe::Inconclusive,
    }
}

/// Touch every TCC-protected location a session needs, so macOS decides about THIS binary now.
///
/// A session created by a launcher has no terminal emulator above it, and macOS attributes a pane's
/// file access to the responsible process - which is this binary, not the shell and not whatever the
/// user ran. Proof from a real machine: a denial recorded while running `ls` and `codex` was written
/// against the zellij executable's path, not against theirs.
///
/// That attribution is the whole problem, because TCC keys a path-based client on its ABSOLUTE PATH.
/// A package manager puts each release in its own versioned directory, so every upgrade is a client
/// macOS has never seen, holding none of the grants the last one earned. The user does not connect
/// the two: they upgrade zellij, and a week later an unrelated tool fails with "Operation not
/// permitted" in a directory that worked yesterday.
///
/// Probing at server start puts the decision back where it belongs - next to the upgrade that caused
/// it, while the user is still looking. Two kinds of location, which behave differently and are both
/// worth touching:
///
/// - **Files & Folders** (Downloads, Desktop, Documents). Promptable. With no decision on record the
///   probe raises the ordinary consent dialog, and one click restores what the previous version had.
/// - **Full Disk Access**. NOT promptable - Apple offers no API to request it. But attempting it
///   registers the client, which is how a program comes to be listed in that settings pane at all,
///   greyed off and waiting for a toggle. Without the attempt there is nothing to toggle and the
///   user must find the versioned path by hand.
///
/// Deliberately silent about success and best-effort throughout: this reports a fault, it does not
/// gate a session on one. A refusal is logged rather than raised because the process that will
/// actually hit the wall is a pane's, minutes or days later, and nothing here can intercept it.
#[cfg(target_os = "macos")]
pub fn probe_protected_locations() {
    let Some(dirs) = directories::BaseDirs::new() else {
        return;
    };
    let home = dirs.home_dir();

    // The FDA-gated path goes last: it always refuses without a grant, and going first would put a
    // refusal at the top of the log every single start, ahead of the ones that mean something.
    let locations = [
        home.join("Downloads"),
        home.join("Desktop"),
        home.join("Documents"),
        home.join("Library/Application Support/com.apple.TCC/TCC.db"),
    ];

    for path in locations {
        // read_dir opens the directory, which is where TCC intercepts; File::open does the same for
        // the db. Neither reads an entry - the open IS the probe.
        let error = if path.is_dir() {
            std::fs::read_dir(&path).err()
        } else {
            std::fs::File::open(&path).err()
        }
        .map(|e| e.kind());

        if classify_tcc_probe(error) == TccProbe::Refused {
            log::warn!(
                "macOS refuses this server access to {}. Every pane will see \"Operation not \
                 permitted\" there, whatever it runs. The grant is keyed to this exact executable \
                 path, so an upgrade loses it: {}. Files & Folders records a refusal permanently \
                 and stops asking - REMOVE the zellij entry under System Settings > Privacy & \
                 Security > Files and Folders to be asked again. Full Disk Access is never asked \
                 for, only toggled, and this probe is what lists zellij there.",
                path.display(),
                std::env::current_exe()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| String::from("<unknown>"))
            );
        }
    }
}

/// Everywhere else has no TCC, so there is nothing to decide.
#[cfg(not(target_os = "macos"))]
pub fn probe_protected_locations() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_server_line() {
        let servers =
            parse_server_processes("  1234 /usr/bin/zellij --server /run/user/1000/zellij/c1/work");
        assert_eq!(
            servers,
            vec![ServerProcess {
                pid: 1234,
                socket: PathBuf::from("/run/user/1000/zellij/c1/work"),
            }]
        );
    }

    #[test]
    fn a_renamed_binary_is_still_a_server() {
        let servers = parse_server_processes("7 /opt/builds/zellij-nightly --server /tmp/z/work");
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].session_name(), Some("work"));
    }

    #[test]
    fn an_unrelated_server_flag_is_not_a_zellij_server() {
        // rsync-over-ssh carries `--server` too, just not in the second-to-last position
        let servers = parse_server_processes(
            "42 /usr/bin/ssh host rsync --server -logDtpre.iLsfxCIvu . /srv/work\n\
             43 /usr/bin/rsync --server --sender -e.LsfxCIvu . /srv/work",
        );
        assert!(servers.is_empty());
    }

    #[test]
    fn a_prefix_named_session_is_not_this_session() {
        let servers = parse_server_processes(
            "1 /usr/bin/zellij --server /run/zellij/c1/work\n\
             2 /usr/bin/zellij --server /run/zellij/c1/work-notes",
        );
        let for_work: Vec<&ServerProcess> = servers
            .iter()
            .filter(|s| s.session_name() == Some("work"))
            .collect();
        assert_eq!(for_work.len(), 1);
        assert_eq!(for_work[0].pid, 1);
    }

    fn server(pid: u32, socket: &str) -> ServerProcess {
        ServerProcess {
            pid,
            socket: PathBuf::from(socket),
        }
    }

    #[test]
    fn one_server_on_the_resolved_socket_is_up() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        assert!(assert_up_from(
            &[server(1, "/run/zellij/c1/work")],
            &expected,
            true,
            true,
            true
        )
        .is_ok());
    }

    #[test]
    fn no_server_is_not_up() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        assert!(assert_up_from(&[], &expected, false, false, true).is_err());
    }

    #[test]
    fn two_servers_for_one_name_is_the_duplicate_fault() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        let err = assert_up_from(
            &[
                server(1, "/run/zellij/c1/work"),
                server(2, "/tmp/zellij-1000/c1/work"),
            ],
            &expected,
            true,
            true,
            true,
        )
        .unwrap_err();
        assert!(err.contains("expected exactly 1"), "{}", err);
    }

    #[test]
    fn a_lone_server_in_another_socket_dir_is_named_as_invisible() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        let err = assert_up_from(
            &[server(2, "/tmp/zellij-1000/c1/work")],
            &expected,
            false,
            false,
            true,
        )
        .unwrap_err();
        assert!(err.contains("invisible"), "{}", err);
    }

    #[test]
    fn a_server_without_its_socket_is_not_up() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        let err = assert_up_from(
            &[server(1, "/run/zellij/c1/work")],
            &expected,
            false,
            true,
            true,
        )
        .unwrap_err();
        assert!(err.contains("does not exist"), "{}", err);
    }

    #[test]
    fn without_a_process_table_the_socket_carries_the_assertion() {
        let expected = PathBuf::from("/run/zellij/c1/work");
        assert!(assert_up_from(&[], &expected, true, true, false).is_ok());
        assert!(assert_up_from(&[], &expected, false, true, false).is_err());
    }

    #[test]
    fn a_creator_with_no_term_hands_out_a_usable_one() {
        // the launcher case: a launch agent or a systemd unit has no TERM at all
        assert_eq!(term_for_new_session(None), Some(DEFAULT_TERM));
        assert_eq!(term_for_new_session(Some("")), Some(DEFAULT_TERM));
        // `dumb` is what an environment with no terminal type produces, not a choice anyone made
        assert_eq!(term_for_new_session(Some("dumb")), Some(DEFAULT_TERM));
    }

    #[test]
    fn a_creator_with_no_colorterm_hands_out_truecolor() {
        assert_eq!(
            colorterm_for_new_session(None),
            Some(DEFAULT_COLORTERM),
            "zellij's renderer really does present a truecolor surface to the pane"
        );
        assert_eq!(colorterm_for_new_session(Some("")), Some(DEFAULT_COLORTERM));
    }

    #[test]
    fn a_colorterm_that_is_already_set_is_never_overridden() {
        assert_eq!(colorterm_for_new_session(Some("truecolor")), None);
        assert_eq!(colorterm_for_new_session(Some("24bit")), None);
    }

    #[test]
    fn a_real_terminal_keeps_the_term_it_came_with() {
        assert_eq!(term_for_new_session(Some("xterm-256color")), None);
        assert_eq!(term_for_new_session(Some("screen-256color")), None);
        assert_eq!(term_for_new_session(Some("dumb-but-not-dumb")), None);
    }

    fn patterns(patterns: &[&str]) -> Vec<String> {
        patterns.iter().map(|p| p.to_string()).collect()
    }

    const ENVIRONMENT: &[&str] = &[
        "MY_VAR",
        "MY_VAR_TOO",
        "MY_PREFIX_ONE",
        "MY_PREFIX_TWO",
        "PATH",
    ];

    #[test]
    fn an_exact_pattern_drops_only_that_variable() {
        assert_eq!(
            env_vars_to_drop(&patterns(&["MY_VAR"]), ENVIRONMENT.iter().copied()),
            vec!["MY_VAR".to_owned()]
        );
    }

    #[test]
    fn a_trailing_star_drops_the_whole_prefix() {
        assert_eq!(
            env_vars_to_drop(&patterns(&["MY_PREFIX_*"]), ENVIRONMENT.iter().copied()),
            vec!["MY_PREFIX_ONE".to_owned(), "MY_PREFIX_TWO".to_owned()]
        );
    }

    #[test]
    fn a_star_that_is_not_at_the_end_is_a_literal_character() {
        // the pattern names a variable called exactly `MY_*_VAR`, which is not in the environment
        assert!(env_vars_to_drop(&patterns(&["MY_*_VAR"]), ENVIRONMENT.iter().copied()).is_empty());
    }

    #[test]
    fn a_pattern_that_matches_nothing_drops_nothing() {
        assert!(env_vars_to_drop(
            &patterns(&["NOT_HERE", "NOT_HERE_*"]),
            ENVIRONMENT.iter().copied()
        )
        .is_empty());
    }

    #[test]
    fn no_patterns_drop_nothing() {
        assert!(env_vars_to_drop(&[], ENVIRONMENT.iter().copied()).is_empty());
    }

    #[test]
    fn a_variable_named_by_two_patterns_is_dropped_once() {
        assert_eq!(
            env_vars_to_drop(&patterns(&["MY_VAR", "MY_*"]), ENVIRONMENT.iter().copied()),
            vec![
                "MY_VAR".to_owned(),
                "MY_VAR_TOO".to_owned(),
                "MY_PREFIX_ONE".to_owned(),
                "MY_PREFIX_TWO".to_owned()
            ]
        );
    }

    fn deleted(killed: bool, found: bool) -> DeletedSession {
        DeletedSession { killed, found }
    }

    #[test]
    fn a_session_that_was_there_and_is_gone_was_removed() {
        assert_eq!(
            DownOutcome::judge(deleted(true, true), Ok(())),
            DownOutcome::Removed
        );
    }

    #[test]
    fn a_session_that_was_already_absent_is_not_a_failure() {
        // what a restart depends on: it tears down and rebuilds, and a session that was already
        // down must not stop it from getting to the rebuild
        let outcome = DownOutcome::judge(deleted(true, false), Ok(()));
        assert_eq!(outcome, DownOutcome::NothingToRemove);
        assert!(!outcome.is_failure());
    }

    #[test]
    fn a_name_still_being_served_is_a_failure() {
        let outcome = DownOutcome::judge(
            deleted(true, true),
            Err("1 server process(es) still serving session 'my-session'".to_owned()),
        );
        assert!(outcome.is_failure());
        // the post-condition speaks for itself, whatever the removal believed it did
        assert_eq!(
            outcome,
            DownOutcome::Failed(
                "1 server process(es) still serving session 'my-session'".to_owned()
            )
        );
    }

    #[test]
    fn a_server_that_outlived_the_wait_is_a_failure() {
        assert!(DownOutcome::judge(deleted(false, true), Ok(())).is_failure());
    }

    #[test]
    fn a_graphical_shell_creates_the_session_itself() {
        assert_eq!(
            gui_domain_action(Some(GUI_MANAGER_NAME), true, Some("a.label"), false),
            GuiDomainAction::Proceed
        );
    }

    #[test]
    fn a_session_that_already_exists_settles_the_question() {
        // its domain was decided when it was created and `up` is not going to replace it
        assert_eq!(
            gui_domain_action(Some("Background"), true, Some("a.label"), true),
            GuiDomainAction::Proceed
        );
    }

    #[test]
    fn a_non_graphical_shell_defers_to_the_installed_job() {
        assert_eq!(
            gui_domain_action(Some("Background"), true, Some("a.label"), false),
            GuiDomainAction::Kickstart("a.label".to_owned())
        );
    }

    #[test]
    fn without_a_job_the_session_is_created_anyway_and_said_so() {
        // no job to defer to, and no session at all is worse than a session without GUI access
        assert_eq!(
            gui_domain_action(Some("Background"), true, None, false),
            GuiDomainAction::ProceedWithoutGui
        );
    }

    #[test]
    fn with_nobody_logged_in_graphically_there_is_nothing_to_create_it_in() {
        assert_eq!(
            gui_domain_action(Some("Background"), false, Some("a.label"), false),
            GuiDomainAction::NoGuiSession
        );
        // and an unknown domain is not treated as the graphical one
        assert_eq!(
            gui_domain_action(None, false, None, false),
            GuiDomainAction::NoGuiSession
        );
    }

    fn executable(path: &str, file_id: Option<(u64, u64)>) -> ExecutableIdentity {
        ExecutableIdentity {
            path: PathBuf::from(path),
            file_id,
            replaced: false,
        }
    }

    #[test]
    fn one_file_under_two_names_is_one_build() {
        // the stable name and the versioned path a package manager points it at
        let ours = executable("/opt/zellij/bin/zellij", Some((66, 1234)));
        let theirs = executable("/opt/zellij/1.2.3/bin/zellij", Some((66, 1234)));
        assert_eq!(compare_builds(Some(&ours), Some(&theirs)), BuildMatch::Same);
    }

    #[test]
    fn a_different_file_at_the_same_path_is_a_different_build() {
        // the ordinary upgrade: one path, a new file behind it, a server still on the old one
        let ours = executable("/opt/zellij/bin/zellij", Some((66, 4321)));
        let theirs = executable("/opt/zellij/bin/zellij", Some((66, 1234)));
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Different
        );
    }

    #[test]
    fn a_server_whose_file_is_gone_is_on_another_build() {
        let ours = executable("/opt/zellij/bin/zellij", Some((66, 4321)));
        let theirs = ExecutableIdentity {
            path: PathBuf::from("/opt/zellij/bin/zellij"),
            file_id: None,
            replaced: true,
        };
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Different
        );
    }

    #[test]
    fn an_executable_that_could_not_be_read_says_nothing() {
        let ours = executable("/opt/zellij/bin/zellij", Some((66, 4321)));
        assert_eq!(compare_builds(Some(&ours), None), BuildMatch::Unknown);
        assert_eq!(compare_builds(None, Some(&ours)), BuildMatch::Unknown);
        assert_eq!(compare_builds(None, None), BuildMatch::Unknown);
    }

    #[test]
    fn without_inodes_only_agreement_is_trusted() {
        // two spellings with nothing to tell them apart by may still be one build
        let ours = executable("/opt/zellij/bin/zellij", None);
        let theirs = executable("/opt/zellij/1.2.3/bin/zellij", None);
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Unknown
        );
        assert_eq!(
            compare_builds(
                Some(&ours),
                Some(&executable("/opt/zellij/bin/zellij", None))
            ),
            BuildMatch::Same
        );
    }

    #[test]
    fn only_permission_denied_is_a_refusal() {
        use std::io::ErrorKind;
        assert_eq!(
            classify_tcc_probe(Some(ErrorKind::PermissionDenied)),
            TccProbe::Refused
        );
        assert_eq!(classify_tcc_probe(None), TccProbe::Reachable);
        assert_eq!(
            classify_tcc_probe(Some(ErrorKind::NotFound)),
            TccProbe::Absent
        );
    }

    #[test]
    fn other_failures_do_not_masquerade_as_a_refusal() {
        // A busy disk or a broken symlink says nothing about consent. Reporting either as a
        // refusal would send the user to a settings pane that cannot help them.
        for kind in [
            std::io::ErrorKind::Interrupted,
            std::io::ErrorKind::InvalidInput,
            std::io::ErrorKind::Other,
        ] {
            assert_eq!(classify_tcc_probe(Some(kind)), TccProbe::Inconclusive);
        }
    }

    #[test]
    fn probing_is_safe_to_call_anywhere() {
        // Off macOS this is empty, and on macOS it must survive a missing home, an absent
        // directory and a refusal without propagating any of them. A session is never gated on it.
        probe_protected_locations();
    }
}
