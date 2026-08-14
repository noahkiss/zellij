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

/// How long to wait for another `session up` to finish before going ahead without the lock.
///
/// Comfortably longer than an `up` takes: the creating path waits at most
/// `SERVER_APPEARANCE_TIMEOUT` for the server to appear and does nothing slow after that. A wait
/// that runs out therefore means the holder is wedged rather than busy, and refusing to bring the
/// session up over a wedged neighbour would be a worse outcome than the race the lock prevents.
#[cfg(unix)]
const UP_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
#[cfg(unix)]
const UP_LOCK_POLL: std::time::Duration = std::time::Duration::from_millis(100);

/// The lock file one session name's `up` takes, in the socket directory this binary resolved.
///
/// That directory and no other: the two processes racing here are two zellij binaries, and the
/// socket directory is the one path they are guaranteed to have resolved the same way - it is the
/// directory whose contents they are racing to create. A lock somewhere derived from the
/// environment would be two different files on the two sides, which is no lock at all.
#[cfg(unix)]
pub fn up_lock_path(name: &str) -> PathBuf {
    ZELLIJ_SOCK_DIR.join(format!(".{}.up.lock", name))
}

/// An advisory lock held across the whole of one `session up` - the check AND the creation.
///
/// `up` is only idempotent if those two are one step. Two of them at once - a `restart` typed by
/// hand overlapping the watchdog's minute tick - both find no server, both create one, and the name
/// ends up with two servers where it allows one. `assert_up` then reports the duplicate on both
/// sides and neither cleans it up, so every later `up` refuses until somebody kills a server by
/// hand.
///
/// With the lock the second one waits, then finds the session healthy and says "already running",
/// which is what it should have said in the first place.
///
/// `restart` needs it over a longer stretch than that. It is a `down` followed by an `up`, and the
/// watchdog's tick fits between them: the tick's `up` takes the lock first, builds the session
/// fresh from the layout, and the restart's `up` then finds it healthy and reports success without
/// ever restoring the snapshot the user asked for. So the lock is taken once for the whole restart
/// and the inner `up` re-enters it - see [`lock_up`].
#[cfg(unix)]
pub struct UpLock {
    /// `None` on a re-entrant handle: this thread holds the `flock` further up the stack, and
    /// taking it again on a second descriptor would block against itself.
    file: Option<std::fs::File>,
    path: PathBuf,
    /// Not `Send`: the hold is recorded per thread, so it has to be given up on the thread that
    /// took it or the count it decrements is somebody else's.
    _not_send: std::marker::PhantomData<*const ()>,
}

thread_local! {
    /// The lock files this THREAD holds, and how many nested holders each has.
    ///
    /// Per thread rather than per process, because re-entrancy is a property of one call stack:
    /// `restart` calling `up` is the same work continuing, while two threads racing for one name
    /// are two `up`s and must contend at the `flock` like two processes would.
    ///
    /// Nothing is persisted, which is the point - a `restart` that dies mid-hold takes the table
    /// with it, and the kernel releases the `flock` on the way out. Staleness is therefore not a
    /// state anyone has to clean up.
    #[cfg(unix)]
    static HELD_UP_LOCKS: std::cell::RefCell<std::collections::HashMap<PathBuf, usize>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(unix)]
impl Drop for UpLock {
    fn drop(&mut self) {
        HELD_UP_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            match held.get_mut(&self.path) {
                // an inner holder went away; the outermost one still holds the flock
                Some(holders) if *holders > 1 => *holders -= 1,
                _ => {
                    held.remove(&self.path);
                },
            }
        });
        // The descriptor decides, not the count: a re-entrant handle never took an `flock` and so
        // has nothing to give back. The one handle that did is the one that releases it.
        let Some(file) = self.file.as_ref() else {
            return;
        };
        use std::os::unix::io::AsRawFd;
        // closing the descriptor would release it anyway; releasing it deliberately is cheaper
        // than leaving the next reader to know that
        // SAFETY: the descriptor is owned by this struct and is open for as long as it is
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Take the lock for `name`, waiting for a holder to finish.
///
/// Re-entrant WITHIN one thread: a caller that already holds this lock - `restart`, over its
/// `down` and its `up` - gets a handle that keeps the hold rather than a second `flock` that would
/// wait for the first until the timeout ran out. Only the outermost handle releases.
///
/// `None` means the caller proceeds unlocked, and both ways of getting it are deliberate. A lock
/// file that cannot be created (an unwritable socket directory) and a holder that never lets go are
/// both worse reasons to leave a machine with no session than the race is to run.
///
/// The longest legitimate hold is a `restart`: its `down` waits `--wait-timeout` seconds (10 by
/// default) and its `up` up to ten more, so the default fits inside this timeout with room. A
/// `--wait-timeout` raised past about twenty seconds does not, and a waiting `up` then gives up on
/// a holder that is merely slow and proceeds unlocked - the race this closes, reopened by hand.
/// Raise the timeout here with it if that combination is ever wanted.
#[cfg(unix)]
pub fn lock_up(name: &str) -> Option<UpLock> {
    lock_up_at(up_lock_path(name), name)
}

/// [`lock_up`], with the lock file named rather than derived, so it can be exercised off a real
/// socket directory.
#[cfg(unix)]
fn lock_up_at(path: PathBuf, name: &str) -> Option<UpLock> {
    use std::os::unix::io::AsRawFd;

    let already_held = HELD_UP_LOCKS.with(|held| match held.borrow_mut().get_mut(&path) {
        Some(holders) => {
            *holders += 1;
            true
        },
        None => false,
    });
    if already_held {
        return Some(UpLock {
            file: None,
            path,
            _not_send: std::marker::PhantomData,
        });
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    // never truncated: the file is a name to lock, not a file with contents, and truncating it
    // would rewrite a file another process is holding
    let file = match std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) => {
            log::debug!("could not open {} to lock it: {}", path.display(), e);
            return None;
        },
    };
    let deadline = std::time::Instant::now() + UP_LOCK_TIMEOUT;
    loop {
        // SAFETY: the descriptor is owned by `file`, which outlives this call
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            // recorded before the caller can nest: the entry is what makes the next one re-enter
            HELD_UP_LOCKS.with(|held| held.borrow_mut().insert(path.clone(), 1));
            return Some(UpLock {
                file: Some(file),
                path,
                _not_send: std::marker::PhantomData,
            });
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "warning: another `zellij session up` or `session restart` for '{}' has held {} \
                 for {}s. Going ahead without the lock, so two servers for this name are possible.",
                name,
                path.display(),
                UP_LOCK_TIMEOUT.as_secs()
            );
            return None;
        }
        std::thread::sleep(UP_LOCK_POLL);
    }
}

/// Nothing to lock where there is no `flock`: the socket checks carry what they carried before.
#[cfg(not(unix))]
pub struct UpLock;

#[cfg(not(unix))]
pub fn lock_up(_name: &str) -> Option<UpLock> {
    None
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

/// Whether an inherited `SSH_AUTH_SOCK` points at a socket that is not there any more.
///
/// A STALE value is worse than none, and that is the whole reason this is checked. With no
/// `SSH_AUTH_SOCK` at all, `ssh` and `git push` fall through to the keys on disk and ask for a
/// passphrase - awkward, but it works and the reason is legible. With one that names a socket that
/// has gone, every agent-backed operation in every pane fails with "Permission denied (publickey)"
/// for the life of the session, while a terminal opened beside it works.
///
/// A graphical login exports `/tmp/ssh-XXXX/agent.<pid>`, which is a new path at every login, so a
/// session created from an old shell hands out the previous login's path. The server gives its
/// environment to every pane, so the wrong value is inherited by all of them and outlives whatever
/// set it.
///
/// Only the DANGLING case is answered here. Inventing a value is a different question with no
/// portable answer - see the `session_service` docs for the one a config can state for itself.
pub fn ssh_auth_sock_is_dangling(value: Option<&str>) -> bool {
    match value {
        Some(path) if !path.is_empty() => !Path::new(path).exists(),
        _ => false,
    }
}

/// The variables an X or Wayland client needs to find the display it copies into.
pub const DISPLAY_ENV_NAMES: &[&str] = &["DISPLAY", "WAYLAND_DISPLAY"];

/// Whether a configured `copy_command` is about to be given no display to talk to.
///
/// `copy_command` runs IN THE SERVER, not in the pane that copied, so it inherits the environment
/// the session was CREATED with and keeps it for the session's whole life. A launcher has no
/// `DISPLAY` and no `WAYLAND_DISPLAY`, so `wl-copy` or `xclip` in a launcher-created session finds
/// no display and exits non-zero - and the only place that goes is `log::error!`. From inside, copy
/// does nothing at all and says nothing at all, in a session where everything else works.
///
/// This says the environment is missing, not that the command needs it: a `copy_command` that
/// writes a file or speaks OSC 52 wants neither variable and is not broken. Which is why the
/// caller's wording is conditional - what is being reported is a fact about the environment, and
/// the reader knows what their own command does.
pub fn copy_command_has_no_display<'a>(
    copy_command: Option<&str>,
    env_names: impl IntoIterator<Item = &'a str>,
) -> bool {
    if copy_command.map_or(true, |command| command.trim().is_empty()) {
        return false;
    }
    let mut names = env_names.into_iter();
    !names.any(|name| DISPLAY_ENV_NAMES.contains(&name))
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
    /// called, and a replacement gets a new one, so equal inodes settle it - but UNEQUAL ones do
    /// not, because a COPY of a build is a different file holding the same program.
    pub file_id: Option<(u64, u64)>,
    /// What the linker stamped into the file: the Mach-O `LC_UUID` on macOS, the GNU build-id on
    /// Linux. This is the only field that identifies a BUILD rather than a file, so two copies of
    /// one binary agree here and nowhere else.
    pub build_id: Option<Vec<u8>>,
    /// The file's length. Not an identity - two builds can share one - but a DIFFERENCE is proof
    /// of two builds, which is what it is kept for where nothing stamped an id.
    pub size: Option<u64>,
    /// The file the process started from is no longer at its path - an upgrade wrote over it in
    /// place. Linux says so by appending " (deleted)" to the `/proc/<pid>/exe` link.
    pub replaced: bool,
}

/// What can be learnt about the file at `path`, without failing if the answer is "not much".
pub fn identify_executable(path: PathBuf) -> ExecutableIdentity {
    let (path, replaced) = match path.to_str().and_then(|p| p.strip_suffix(" (deleted)")) {
        Some(real_path) => (PathBuf::from(real_path), true),
        None => (path, false),
    };
    let path = path.canonicalize().unwrap_or(path);
    // a replaced file's path now holds its replacement, and nothing read from it describes the
    // build that is actually running
    #[cfg(unix)]
    let (file_id, size, build_id) = if replaced {
        (None, None, None)
    } else {
        let stat = std::fs::metadata(&path).ok();
        let file_id = stat.as_ref().map(|metadata| {
            use std::os::unix::fs::MetadataExt;
            (metadata.dev(), metadata.ino())
        });
        (
            file_id,
            stat.as_ref().map(|metadata| metadata.len()),
            build_id_of(&path),
        )
    };
    #[cfg(not(unix))]
    let (file_id, size, build_id): (Option<(u64, u64)>, Option<u64>, Option<Vec<u8>>) =
        (None, None, None);
    ExecutableIdentity {
        path,
        file_id,
        build_id,
        size,
        replaced,
    }
}

/// A read of exactly `len` bytes at `offset`, and nothing else.
///
/// The binary is around 40 MB and this runs on every CLI invocation, so no path through here is
/// allowed to read the whole file. Every caller caps `len` from a header field before asking.
#[cfg(unix)]
fn read_at(file: &std::fs::File, offset: u64, len: usize) -> Option<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let mut buffer = vec![0u8; len];
    file.read_exact_at(&mut buffer, offset).ok()?;
    Some(buffer)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn u16_at(bytes: &[u8], offset: usize, little: bool) -> Option<u16> {
    let raw: [u8; 2] = bytes.get(offset..offset + 2)?.try_into().ok()?;
    Some(if little {
        u16::from_le_bytes(raw)
    } else {
        u16::from_be_bytes(raw)
    })
}

#[cfg(unix)]
fn u32_at(bytes: &[u8], offset: usize, little: bool) -> Option<u32> {
    let raw: [u8; 4] = bytes.get(offset..offset + 4)?.try_into().ok()?;
    Some(if little {
        u32::from_le_bytes(raw)
    } else {
        u32::from_be_bytes(raw)
    })
}

#[cfg(unix)]
fn u64_at(bytes: &[u8], offset: usize, little: bool) -> Option<u64> {
    let raw: [u8; 8] = bytes.get(offset..offset + 8)?.try_into().ok()?;
    Some(if little {
        u64::from_le_bytes(raw)
    } else {
        u64::from_be_bytes(raw)
    })
}

/// The build the file at `path` IS, as its own contents record it.
///
/// A copy of a binary is a different file holding the same program, so nothing the filesystem knows
/// can tell the two apart. Both formats stamp an identity near the front of the file - the Mach-O
/// `LC_UUID`, the GNU build-id note - and reading it costs a few kilobytes, not 40 MB.
///
/// `None` is an ordinary answer: not every linker emits one. It means "no evidence", never
/// "different".
#[cfg(unix)]
fn build_id_of(path: &Path) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let header = read_at(&file, 0, 64)?;
    #[cfg(target_os = "macos")]
    {
        macho_build_id(&file, &header)
    }
    #[cfg(not(target_os = "macos"))]
    {
        if header.get(..4)? == b"\x7fELF" {
            elf_build_id(&file, &header)
        } else {
            None
        }
    }
}

/// The GNU build-id, found through the PROGRAM headers.
///
/// The note is reachable two ways, and only one of them survives: sections are what `strip` is
/// entitled to discard, segments are not. So the `PT_NOTE` segments are walked, not
/// `.note.gnu.build-id` by name.
#[cfg(all(unix, not(target_os = "macos")))]
fn elf_build_id(file: &std::fs::File, header: &[u8]) -> Option<Vec<u8>> {
    const PT_NOTE: u32 = 4;
    // a program header table longer than this is not something a linker produced
    const MAX_SEGMENTS: u16 = 128;
    // notes are a handful of small records; a segment larger than this is not one of ours
    const MAX_NOTE_SEGMENT: u64 = 64 * 1024;

    let sixty_four = match header.get(4)? {
        2 => true,
        1 => false,
        _ => return None,
    };
    let little = match header.get(5)? {
        1 => true,
        2 => false,
        _ => return None,
    };
    let (table_at, entry_size, count) = if sixty_four {
        (
            u64_at(header, 0x20, little)?,
            u16_at(header, 0x36, little)? as usize,
            u16_at(header, 0x38, little)?,
        )
    } else {
        (
            u32_at(header, 0x1c, little)? as u64,
            u16_at(header, 0x2a, little)? as usize,
            u16_at(header, 0x2c, little)?,
        )
    };
    let smallest_entry = if sixty_four { 56 } else { 32 };
    if count == 0 || count > MAX_SEGMENTS || entry_size < smallest_entry {
        return None;
    }
    let table = read_at(file, table_at, entry_size * count as usize)?;
    for entry in table.chunks_exact(entry_size) {
        if u32_at(entry, 0, little)? != PT_NOTE {
            continue;
        }
        let (notes_at, notes_size) = if sixty_four {
            (u64_at(entry, 8, little)?, u64_at(entry, 32, little)?)
        } else {
            (
                u32_at(entry, 4, little)? as u64,
                u32_at(entry, 16, little)? as u64,
            )
        };
        if notes_size == 0 || notes_size > MAX_NOTE_SEGMENT {
            continue;
        }
        let Some(notes) = read_at(file, notes_at, notes_size as usize) else {
            continue;
        };
        if let Some(build_id) = gnu_build_id_in_notes(&notes, little) {
            return Some(build_id);
        }
    }
    None
}

/// The `NT_GNU_BUILD_ID` record in one note segment, if it holds one.
///
/// Each record is a 12-byte header, then a name and a payload, each padded up to four bytes. Both
/// lengths are checked against the buffer before either is used, so a malformed file yields `None`
/// rather than a panic.
#[cfg(all(unix, not(target_os = "macos")))]
fn gnu_build_id_in_notes(notes: &[u8], little: bool) -> Option<Vec<u8>> {
    const NT_GNU_BUILD_ID: u32 = 3;
    let padded = |length: usize| length.checked_add(3).map(|length| length & !3);

    let mut at = 0usize;
    while at + 12 <= notes.len() {
        let name_size = u32_at(notes, at, little)? as usize;
        let payload_size = u32_at(notes, at + 4, little)? as usize;
        let kind = u32_at(notes, at + 8, little)?;
        let name_at = at + 12;
        let payload_at = name_at.checked_add(padded(name_size)?)?;
        let end = payload_at.checked_add(padded(payload_size)?)?;
        if end > notes.len() {
            return None;
        }
        if kind == NT_GNU_BUILD_ID && notes.get(name_at..name_at + name_size) == Some(&b"GNU\0"[..])
        {
            return notes
                .get(payload_at..payload_at + payload_size)
                .map(|build_id| build_id.to_vec());
        }
        at = end;
    }
    None
}

/// The `LC_UUID` of one Mach-O image, which may be a slice inside a universal file.
///
/// Only the load commands are read - they follow the header directly, and their total size is in
/// the header, so the read is bounded before the file is touched a second time.
#[cfg(target_os = "macos")]
fn macho_uuid(file: &std::fs::File, image_at: u64) -> Option<Vec<u8>> {
    const LC_UUID: u32 = 0x1b;
    // both caps are far above anything a real image carries
    const MAX_LOAD_COMMANDS: u32 = 4096;
    const MAX_LOAD_COMMANDS_SIZE: u32 = 256 * 1024;

    let header = read_at(file, image_at, 32)?;
    let (sixty_four, little) = match u32_at(&header, 0, true)? {
        0xfeed_facf => (true, true),
        0xcffa_edfe => (true, false),
        0xfeed_face => (false, true),
        0xcefa_edfe => (false, false),
        _ => return None,
    };
    let count = u32_at(&header, 16, little)?;
    let commands_size = u32_at(&header, 20, little)?;
    if count == 0 || count > MAX_LOAD_COMMANDS || commands_size > MAX_LOAD_COMMANDS_SIZE {
        return None;
    }
    let commands_at = image_at + if sixty_four { 32 } else { 28 };
    let commands = read_at(file, commands_at, commands_size as usize)?;
    let mut at = 0usize;
    for _ in 0..count {
        let kind = u32_at(&commands, at, little)?;
        let size = u32_at(&commands, at + 4, little)? as usize;
        if size < 8 {
            return None;
        }
        if kind == LC_UUID {
            return commands.get(at + 8..at + 24).map(|uuid| uuid.to_vec());
        }
        at = at.checked_add(size)?;
    }
    None
}

/// The identity of a Mach-O file, universal or not.
///
/// A universal file is one build only if every slice in it is, so all of their UUIDs are joined in
/// file order and compared as one value. A thin file is the same walk with one image at offset 0.
#[cfg(target_os = "macos")]
fn macho_build_id(file: &std::fs::File, header: &[u8]) -> Option<Vec<u8>> {
    const FAT_MAGIC: u32 = 0xcafe_babe;
    const FAT_MAGIC_64: u32 = 0xcafe_babf;
    // a universal binary of more than this many architectures is not one we produced
    const MAX_ARCHITECTURES: u32 = 16;

    // a fat header is big-endian whatever the images inside it are
    let magic = u32_at(header, 0, false)?;
    if magic != FAT_MAGIC && magic != FAT_MAGIC_64 {
        return macho_uuid(file, 0);
    }
    let wide = magic == FAT_MAGIC_64;
    let count = u32_at(header, 4, false)?;
    if count == 0 || count > MAX_ARCHITECTURES {
        return None;
    }
    let entry_size = if wide { 32 } else { 20 };
    let table = read_at(file, 8, entry_size * count as usize)?;
    let mut joined = Vec::new();
    for entry in table.chunks_exact(entry_size) {
        let image_at = if wide {
            u64_at(entry, 8, false)?
        } else {
            u32_at(entry, 8, false)? as u64
        };
        joined.extend(macho_uuid(file, image_at)?);
    }
    if joined.is_empty() {
        return None;
    }
    Some(joined)
}

/// Whether two executables are the same build, judged only on what was actually established.
///
/// The evidence is tried strongest first, and each step only reports `Different` on something that
/// PROVES it:
///
/// 1. one inode is one file, so equal inodes are `Same` and cost nothing beyond the stat already
///    taken. Unequal inodes prove nothing - a copy of a build is a different file;
/// 2. the id the linker stamped in decides both ways. It identifies the BUILD, so a copy at a
///    pinned path and the binary it was copied from agree here;
/// 3. with no stamp on one side, a size that DIFFERS is still proof of two builds;
/// 4. same size and nothing to identify either: `Unknown`. Two names for one build is at least as
///    likely as two builds, and only one of those two answers costs the user anything - a wrong
///    "your session is stale" sends someone to restart a session that did not need it.
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
    if let (Some(our_file), Some(their_file)) = (ours.file_id, theirs.file_id) {
        if our_file == their_file {
            return BuildMatch::Same;
        }
    }
    if let (Some(our_id), Some(their_id)) = (&ours.build_id, &theirs.build_id) {
        return if our_id == their_id {
            BuildMatch::Same
        } else {
            BuildMatch::Different
        };
    }
    if let (Some(our_size), Some(their_size)) = (ours.size, theirs.size) {
        if our_size != their_size {
            return BuildMatch::Different;
        }
    }
    match (ours.file_id, theirs.file_id) {
        // two distinct files that nothing above could tell apart
        (Some(_), Some(_)) => BuildMatch::Unknown,
        // neither could be stat'ed, so only agreement counts: two spellings may be one build
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

/// The path of the binary running right now, with symlinks resolved.
///
/// macOS keys a TCC grant (Full Disk Access and friends) to the RESOLVED executable, so anything
/// that shows a user which binary to grant has to name this path. A package manager installs the
/// binary in a versioned directory and puts a symlink on PATH; `current_exe()` hands back the
/// symlink, which is a path TCC never records. Falls back to the unresolved path, which is still
/// truer than nothing when the resolve fails.
pub fn own_executable_path() -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?;
    Some(std::fs::canonicalize(&path).unwrap_or(path))
}

/// What a pass over the pinned copy did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinOutcome {
    /// There was no file at the pinned path, and now there is.
    Installed(PathBuf),
    /// The pinned path held a different build, and now holds this one.
    Refreshed(PathBuf),
    /// The pinned path already holds this build. The common case, and the reason the build is
    /// identified at all: copying 40 MB on every `session up` for nothing.
    UpToDate(PathBuf),
}

impl PinOutcome {
    pub fn path(&self) -> &Path {
        match self {
            PinOutcome::Installed(path)
            | PinOutcome::Refreshed(path)
            | PinOutcome::UpToDate(path) => path,
        }
    }
}

/// Put this build at `target`, if it is not there already.
///
/// WRITTEN OVER IN PLACE, never unlinked and replaced. Measured on one machine: macOS keeps a
/// permission grant when a different build is written over the file the grant names, and the grant
/// is keyed to that file rather than to its contents - the code signature it recorded is not
/// enforced for an ad-hoc-signed client. A new inode at the same path is a new client with none of
/// the grants, so `truncate` is load-bearing and a rename into place would undo the whole point.
///
/// Whether to write at all is decided by [`compare_builds`], not by a timestamp or a path: the
/// pinned copy is a COPY, so it is a different file from the binary it came from and only the id
/// the linker stamped in can say whether it is the same build.
///
/// A write that fails part-way leaves a short file at the pinned path. That is not silent and it is
/// not permanent: a truncated copy is a different size, so the next `session up` finds it different
/// and writes it again.
#[cfg(unix)]
pub fn install_pinned_exe(source: &Path, target: &Path) -> Result<PinOutcome, String> {
    use std::os::unix::fs::PermissionsExt;

    if target.exists() {
        let ours = identify_executable(source.to_path_buf());
        let theirs = identify_executable(target.to_path_buf());
        if compare_builds(Some(&ours), Some(&theirs)) == BuildMatch::Same {
            return Ok(PinOutcome::UpToDate(target.to_path_buf()));
        }
    }
    let refreshing = target.exists();
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
    }
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("could not read {}: {}", source.display(), e))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)
        .map_err(|e| pin_write_error(target, &e))?;
    std::io::copy(&mut input, &mut output)
        .map_err(|e| format!("could not write {}: {}", target.display(), e))?;
    std::fs::set_permissions(target, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("could not make {} executable: {}", target.display(), e))?;
    Ok(if refreshing {
        PinOutcome::Refreshed(target.to_path_buf())
    } else {
        PinOutcome::Installed(target.to_path_buf())
    })
}

/// Why the pinned path could not be opened for writing, saying what to do about the one cause that
/// is ordinary: the copy is running.
///
/// A server keeps the binary it started with, so the pinned copy of a session that is up is being
/// executed, and no unix will let it be written to. Restarting the session releases it, and the
/// restart is what the new build was wanted for anyway.
#[cfg(unix)]
fn pin_write_error(target: &Path, error: &std::io::Error) -> String {
    if error.raw_os_error() == Some(libc::ETXTBSY) {
        return format!(
            "the pinned copy at {} is being executed right now, so it cannot be written over. \
             A server keeps the binary it started with, so restart the session to release it - \
             `zellij session restart` - and the copy is refreshed on the way back up.",
            target.display()
        );
    }
    format!("could not write {}: {}", target.display(), error)
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

/// The notice a running server should show about its own build being superseded, if any.
///
/// A server keeps the binary it started with for the whole life of the session, so an upgrade
/// reaches nothing until the session is restarted - and nothing else says so, which is how a
/// machine sits on a superseded build for days while everyone believes the upgrade took effect.
///
/// Asked of the path this server was STARTED FROM, which is what makes the answer trustworthy
/// rather than a guess: the file being gone, or holding a different build than the one running, is
/// proof that what is installed there is no longer what is running. Comparing against whatever
/// `zellij` happens to be on `PATH` would call a deliberately-mixed setup stale forever.
///
/// The one addition is for [`pin_exe`](crate::session_service::configured_pinned_exe): a pinned
/// copy cannot be written over while it is being executed, so an upgrade CANNOT change it under a
/// running server and rule two can never fire. There the binary on `PATH` is the intended source
/// of that copy, so it is the right thing to compare against.
pub fn stale_build_notice(session_name: &str, pinned_exe: Option<&Path>) -> Option<String> {
    let running_path = std::env::current_exe().ok()?;
    let running = own_executable()?;

    let superseded = if !running_path.exists() {
        // the upgrade took the whole versioned directory with it
        true
    } else if compare_builds(
        Some(&running),
        Some(&identify_executable(running_path.clone())),
    ) == BuildMatch::Different
    {
        true
    } else if pinned_exe.map_or(false, |pinned| same_executable_path(pinned, &running_path)) {
        installed_on_path(&running_path).map_or(false, |installed| {
            compare_builds(Some(&running), Some(&installed)) == BuildMatch::Different
        })
    } else {
        false
    };

    if !superseded {
        return None;
    }
    Some(format!(
        "⚠ session '{}' runs a superseded build - `zellij session restart {}`",
        session_name, session_name
    ))
}

/// The build of the `PATH` entry that shares this binary's file name, if there is one.
fn installed_on_path(running_path: &Path) -> Option<ExecutableIdentity> {
    let name = running_path.file_name()?;
    crate::session_service::path_dirs()
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .map(identify_executable)
}

/// Two spellings of one file, for paths that both exist.
fn same_executable_path(one: &Path, other: &Path) -> bool {
    one == other
        || match (one.canonicalize(), other.canonicalize()) {
            (Ok(one), Ok(other)) => one == other,
            _ => false,
        }
}

/// Whether Full Disk Access is granted to THIS binary, asked fresh.
///
/// Cheap enough to ask on a timer, which it has to be: an FDA toggle takes effect on a live
/// process, so a session that was refused at startup can be granted while it runs and a cached
/// answer would keep telling the user to fix something they have already fixed.
///
/// `None` means the question was not answered - the file is missing, or the failure was not a
/// permission one. It is not the same as "denied" and must never be reported as one.
#[cfg(target_os = "macos")]
pub fn full_disk_access_granted() -> Option<bool> {
    let dirs = directories::BaseDirs::new()?;
    // the same file the startup probe uses last: reachable only with Full Disk Access, and reading
    // nothing out of it - the open IS the question
    let gated = dirs
        .home_dir()
        .join("Library/Application Support/com.apple.TCC/TCC.db");
    let error = std::fs::File::open(&gated).err().map(|e| e.kind());
    match classify_tcc_probe(error) {
        TccProbe::Reachable => Some(true),
        TccProbe::Refused => Some(false),
        TccProbe::Absent | TccProbe::Inconclusive => None,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn full_disk_access_granted() -> Option<bool> {
    // there is no such permission to be missing
    None
}

/// The notice a session should show about Full Disk Access, if any.
///
/// Names the path, because the grant is keyed to that exact file and auto-registration was not
/// observed to happen - the user may have to add it by hand, and a notice that does not name it
/// sends them hunting through a versioned package directory.
pub fn full_disk_access_notice() -> Option<String> {
    if full_disk_access_granted()? {
        return None;
    }
    let path = own_executable_path()?;
    Some(format!(
        "⚠ Full Disk Access not granted for {}",
        path.display()
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
/// - **Full Disk Access**. NOT promptable - Apple offers no API to request it, and the attempt was
///   NOT observed to list the client in that settings pane either: four controlled tests on a clean
///   slate produced no row, from the server process and from a pane descendant alike. So the FDA
///   half of this probe has no demonstrated effect, and the path still has to be added by hand. It
///   is kept because the attempt is free and the log line tells the user which path to add.
///
/// Deliberately silent about success and best-effort throughout: this reports a fault, it does not
/// gate a session on one. A refusal is logged rather than raised because the process that will
/// actually hit the wall is a pane's, minutes or days later, and nothing here can intercept it.
///
/// Blocking. Call [`probe_protected_locations`] instead unless the wait is wanted.
#[cfg(target_os = "macos")]
pub fn probe_protected_locations_now() {
    // The warning has to name the resolved path, not the one this process was started through -
    // see `own_executable_path`. Naming the symlink sends the reader to a settings entry that will
    // never appear.
    fn responsible_executable_path() -> String {
        own_executable_path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| String::from("<unknown>"))
    }

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
                 for, only toggled, and has to be added by hand at that path.",
                path.display(),
                responsible_executable_path()
            );
        }
    }
}

/// Start the probe on its own thread and return at once.
///
/// The probe MUST NOT run on the caller's thread. A promptable location with no decision on record
/// blocks inside the open until someone answers the consent dialog - measured at about 100 seconds
/// on one machine, and once until the machine was rebooted. The caller is the server's main thread,
/// so a waiting dialog means the session never appears at all, and every retry is refused as a
/// second server. The failure names nothing near its cause.
///
/// This gives up an ordering guarantee: panes now spawn while a decision may still be pending. It
/// costs nothing, because TCC coalesces. Measured on a machine with no decision on record: two
/// processes touching the same protected directory, one pending dialog, and BOTH waited - neither
/// was refused. So a pane that touches a protected directory in those first seconds waits alongside
/// the probe and proceeds once the user answers.
#[cfg(target_os = "macos")]
pub fn probe_protected_locations() {
    let _ = std::thread::Builder::new()
        .name("tcc_probe".to_string())
        .spawn(probe_protected_locations_now);
}

/// Everywhere else has no TCC, so there is nothing to decide.
#[cfg(not(target_os = "macos"))]
pub fn probe_protected_locations() {}

/// Everywhere else has no TCC, so there is nothing to decide.
#[cfg(not(target_os = "macos"))]
pub fn probe_protected_locations_now() {}

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
    fn an_ssh_auth_sock_nothing_is_listening_on_is_dangling() {
        assert!(ssh_auth_sock_is_dangling(Some(
            "/tmp/ssh-NOTHING/agent.99999"
        )));
    }

    #[test]
    fn an_ssh_auth_sock_that_is_there_is_left_alone() {
        let dir = tempfile::TempDir::new().unwrap();
        let socket = dir.path().join("agent.1");
        std::fs::write(&socket, b"").unwrap();
        assert!(!ssh_auth_sock_is_dangling(socket.to_str()));
    }

    #[test]
    fn an_unset_or_empty_ssh_auth_sock_is_not_dangling() {
        // nothing to drop, and reporting one would send the reader after a variable that is
        // already absent
        assert!(!ssh_auth_sock_is_dangling(None));
        assert!(!ssh_auth_sock_is_dangling(Some("")));
    }

    #[test]
    fn a_copy_command_with_no_display_variable_is_reported() {
        assert!(copy_command_has_no_display(
            Some("wl-copy"),
            ["HOME", "PATH", "TERM"]
        ));
    }

    #[test]
    fn either_display_variable_answers_it() {
        assert!(!copy_command_has_no_display(Some("xclip -i"), ["DISPLAY"]));
        assert!(!copy_command_has_no_display(
            Some("wl-copy"),
            ["WAYLAND_DISPLAY"]
        ));
    }

    #[test]
    fn no_copy_command_is_nothing_to_report() {
        assert!(!copy_command_has_no_display(None, ["HOME"]));
        assert!(!copy_command_has_no_display(Some("  "), ["HOME"]));
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
            build_id: None,
            size: None,
            replaced: false,
        }
    }

    fn stamped(path: &str, file_id: (u64, u64), build_id: &[u8]) -> ExecutableIdentity {
        ExecutableIdentity {
            build_id: Some(build_id.to_vec()),
            size: Some(41_000_000),
            ..executable(path, Some(file_id))
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
        let ours = stamped("/opt/zellij/bin/zellij", (66, 4321), b"newer");
        let theirs = stamped("/opt/zellij/bin/zellij", (66, 1234), b"older");
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Different
        );
    }

    #[test]
    fn a_copy_of_one_binary_is_one_build() {
        // what a pinned copy at a stable path looks like: same program, two files, two inodes.
        // The whole point of the stamped id - by inode alone this is the false alarm.
        let ours = stamped("/opt/zellij/bin/zellij", (66, 4321), b"one build");
        let theirs = stamped("/var/zellij/bin/zellij", (66, 9876), b"one build");
        assert_eq!(compare_builds(Some(&ours), Some(&theirs)), BuildMatch::Same);
    }

    #[test]
    fn without_a_stamp_a_size_that_differs_is_still_proof() {
        // no linker id on either side, so the only evidence left is the one that cannot lie
        let ours = ExecutableIdentity {
            size: Some(41_000_000),
            ..executable("/opt/zellij/bin/zellij", Some((66, 4321)))
        };
        let theirs = ExecutableIdentity {
            size: Some(40_000_000),
            ..executable("/opt/zellij/bin/zellij", Some((66, 1234)))
        };
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Different
        );
    }

    #[test]
    fn two_unstamped_files_of_one_size_say_nothing() {
        // a copy on a toolchain that emitted no build id. Silence is the answer that cannot send
        // someone to restart a session that did not need it
        let ours = ExecutableIdentity {
            size: Some(41_000_000),
            ..executable("/opt/zellij/bin/zellij", Some((66, 4321)))
        };
        let theirs = ExecutableIdentity {
            size: Some(41_000_000),
            ..executable("/var/zellij/bin/zellij", Some((66, 9876)))
        };
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Unknown
        );
    }

    #[test]
    fn a_server_whose_file_is_gone_is_on_another_build() {
        let ours = executable("/opt/zellij/bin/zellij", Some((66, 4321)));
        let theirs = ExecutableIdentity {
            replaced: true,
            ..executable("/opt/zellij/bin/zellij", None)
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
        probe_protected_locations_now();
        probe_protected_locations();
    }

    /// The smallest ELF file that carries a build id: a header, one `PT_NOTE` program header, and
    /// the note itself. Written by hand so the parser is tested against the format rather than
    /// against whatever the local toolchain happens to emit - which, as it turns out, may emit no
    /// build id at all.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn elf_with_build_id(build_id: &[u8], padding: usize) -> Vec<u8> {
        let mut note = Vec::new();
        note.extend((4u32).to_le_bytes()); // n_namesz, for "GNU\0"
        note.extend((build_id.len() as u32).to_le_bytes());
        note.extend((3u32).to_le_bytes()); // NT_GNU_BUILD_ID
        note.extend(b"GNU\0");
        note.extend(build_id);
        while note.len() % 4 != 0 {
            note.push(0);
        }

        let mut file = vec![0u8; 120];
        file[..4].copy_from_slice(b"\x7fELF");
        file[4] = 2; // 64-bit
        file[5] = 1; // little-endian
        file[6] = 1; // version
        file[0x10..0x12].copy_from_slice(&2u16.to_le_bytes()); // e_type: ET_EXEC
        file[0x12..0x14].copy_from_slice(&0x3eu16.to_le_bytes()); // e_machine
        file[0x14..0x18].copy_from_slice(&1u32.to_le_bytes()); // e_version
        file[0x20..0x28].copy_from_slice(&64u64.to_le_bytes()); // e_phoff
        file[0x34..0x36].copy_from_slice(&64u16.to_le_bytes()); // e_ehsize
        file[0x36..0x38].copy_from_slice(&56u16.to_le_bytes()); // e_phentsize
        file[0x38..0x3a].copy_from_slice(&1u16.to_le_bytes()); // e_phnum

        file[64..68].copy_from_slice(&4u32.to_le_bytes()); // p_type: PT_NOTE
        file[72..80].copy_from_slice(&120u64.to_le_bytes()); // p_offset
        file[96..104].copy_from_slice(&(note.len() as u64).to_le_bytes()); // p_filesz

        file.extend(note);
        file.extend(std::iter::repeat(0).take(padding));
        file
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    struct ScratchDir(PathBuf);

    #[cfg(all(unix, not(target_os = "macos")))]
    impl ScratchDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "zellij-build-identity-{}-{}",
                std::process::id(),
                name
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a writable temp dir");
            ScratchDir(path)
        }

        fn write(&self, name: &str, contents: &[u8]) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, contents).expect("a writable temp dir");
            path
        }
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_copied_binary_reads_as_the_same_build() {
        // the case that made this necessary: byte-identical files, two inodes, no false warning
        let scratch = ScratchDir::new("copy");
        let original = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let copy = scratch.0.join("zellij-pinned");
        std::fs::copy(&original, &copy).expect("a writable temp dir");

        let ours = identify_executable(original);
        let theirs = identify_executable(copy);
        assert_ne!(ours.file_id, theirs.file_id, "a copy is a different file");
        assert_eq!(ours.build_id, Some(vec![0xab; 20]));
        assert_eq!(compare_builds(Some(&ours), Some(&theirs)), BuildMatch::Same);
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn two_builds_read_as_different_builds() {
        let scratch = ScratchDir::new("different");
        let ours = scratch.write("new", &elf_with_build_id(&[0xab; 20], 4096));
        // a differing size alone would settle this, so keep them equal and make the id do the work
        let theirs = scratch.write("old", &elf_with_build_id(&[0xcd; 20], 4096));

        let ours = identify_executable(ours);
        let theirs = identify_executable(theirs);
        assert_eq!(ours.size, theirs.size);
        assert_eq!(
            compare_builds(Some(&ours), Some(&theirs)),
            BuildMatch::Different
        );
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_symlink_still_reads_as_the_same_build() {
        // the original case: the installed name pointing into a versioned directory
        let scratch = ScratchDir::new("symlink");
        let original = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let link = scratch.0.join("zellij-current");
        std::os::unix::fs::symlink(&original, &link).expect("a writable temp dir");

        let ours = identify_executable(original);
        let theirs = identify_executable(link);
        assert_eq!(ours.file_id, theirs.file_id, "a symlink is one file");
        assert_eq!(compare_builds(Some(&ours), Some(&theirs)), BuildMatch::Same);
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_pinned_copy_is_created_where_there_was_none() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new("pin-new");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        // a directory that does not exist yet: the pin is zellij's own, so nothing else made it
        let target = scratch.0.join("bin/zellij");

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Installed(target.clone()))
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            std::fs::read(&source).unwrap()
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111, "a launcher has to be able to exec it");
    }

    /// The reason the build is identified at all. `session up` runs this on every pass, including
    /// the one a watchdog takes every minute, and the binary is around 40 MB.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_same_build_is_not_copied_over_itself() {
        let scratch = ScratchDir::new("pin-same");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        std::fs::copy(&source, &target).unwrap();
        let before = std::fs::metadata(&target).unwrap().modified().unwrap();

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone()))
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().modified().unwrap(),
            before,
            "a copy of the same build was written all the same"
        );
    }

    /// The measured constraint the whole feature rests on: macOS keeps a permission grant when the
    /// file the grant names is written over, and loses it when a new file takes the path. So the
    /// refresh has to keep the inode.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_refresh_writes_over_the_same_file_rather_than_replacing_it() {
        use std::os::unix::fs::MetadataExt;

        let scratch = ScratchDir::new("pin-refresh");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 8192));
        let target = scratch.write("pinned", &elf_with_build_id(&[0xcd; 20], 4096));
        let before = std::fs::metadata(&target).unwrap().ino();

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone()))
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().ino(),
            before,
            "the pinned path holds a new file, so a macOS grant would not follow it"
        );
        assert_eq!(
            identify_executable(target).build_id,
            Some(vec![0xab; 20]),
            "and it is the new build"
        );
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_file_that_is_not_an_executable_yields_no_id() {
        // "no evidence" has to stay distinct from "different", or every unreadable file warns
        let scratch = ScratchDir::new("not-elf");
        let path = scratch.write("zellij", b"#!/bin/sh\nexec zellij \"$@\"\n");
        assert_eq!(identify_executable(path).build_id, None);
    }

    /// A directory of this test's own, so the lock tests never touch a real socket directory.
    #[cfg(unix)]
    fn lock_scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("zellij-up-lock-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("a writable temp dir");
        path
    }

    /// Whether the lock file is free, asked the way another PROCESS would ask: a descriptor of its
    /// own. `flock` is per open file description, so this contends with a held lock even here.
    #[cfg(unix)]
    fn lock_is_free(path: &Path) -> bool {
        use std::os::unix::io::AsRawFd;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .expect("a writable lock file");
        let free = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
        if free {
            unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        }
        free
    }

    /// `restart` holds the lock and then calls `up`, which takes it again. Without re-entrancy that
    /// inner take waits for the outer one until the timeout runs out and then proceeds unlocked -
    /// the deadlock dressed up as a 30-second pause.
    #[test]
    #[cfg(unix)]
    fn a_nested_lock_re_enters_the_one_this_thread_already_holds() {
        let dir = lock_scratch("nested");
        let path = dir.join(".work.up.lock");

        let outer = lock_up_at(path.clone(), "work").expect("the lock is free");
        assert!(outer.file.is_some(), "the outer holder owns the flock");
        assert!(!lock_is_free(&path));

        let inner = lock_up_at(path.clone(), "work").expect("re-entered rather than waited");
        assert!(inner.file.is_none(), "the inner holder took a second flock");

        // the inner `up` finishing must not open the window the outer hold exists to close
        drop(inner);
        assert!(
            !lock_is_free(&path),
            "the inner drop released the outer hold"
        );

        drop(outer);
        assert!(lock_is_free(&path), "the outermost drop left the lock held");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A restart that dies between its two steps must not leave the count claiming a hold nobody
    /// has. The unwind drops both handles, so the next take on this thread is a real `flock`
    /// again - a count left high would hand out a pass-through handle over an unheld lock.
    #[test]
    #[cfg(unix)]
    fn an_unwind_gives_back_every_nested_hold() {
        let dir = lock_scratch("unwind");
        let path = dir.join(".work.up.lock");

        let died = std::panic::catch_unwind({
            let path = path.clone();
            move || {
                let _outer = lock_up_at(path.clone(), "work").expect("the lock is free");
                let _inner = lock_up_at(path, "work").expect("re-entered rather than waited");
                panic!("the `down` failed");
            }
        });
        assert!(died.is_err(), "the panic never happened");

        assert!(lock_is_free(&path), "the unwind left the flock held");
        let after = lock_up_at(path.clone(), "work").expect("the lock is free again");
        assert!(
            after.file.is_some(),
            "re-entered a hold the unwind had given up"
        );
        drop(after);
        assert!(lock_is_free(&path));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The fault this is all for. `restart` is a `down` and then an `up`, and the watchdog's minute
    /// tick used to fit between them: its `up` rebuilt the session fresh from the layout, and the
    /// restart's `up` then found a healthy session and reported success - having dropped the
    /// snapshot the restart existed to restore.
    #[test]
    #[cfg(unix)]
    fn the_watchdog_cannot_slip_between_a_restarts_down_and_its_up() {
        use std::sync::mpsc;
        use std::sync::{Arc, Mutex};

        #[derive(Default)]
        struct Session {
            alive: bool,
            built_from: Option<&'static str>,
            builds: usize,
        }

        /// `up`, reduced to the part that races: take the lock, and build only what is not there.
        fn up(path: &Path, session: &Mutex<Session>, from: &'static str) {
            let _guard = lock_up_at(path.to_path_buf(), "work");
            let mut session = session.lock().unwrap();
            if !session.alive {
                session.alive = true;
                session.built_from = Some(from);
                session.builds += 1;
            }
        }

        let dir = lock_scratch("restart-window");
        let path = dir.join(".work.up.lock");
        let session = Arc::new(Mutex::new(Session {
            alive: true,
            ..Session::default()
        }));

        // the restart takes the lock ONCE, for both of its steps
        let restart_lock = lock_up_at(path.clone(), "work");
        assert!(restart_lock.is_some(), "the lock is free");
        // ... `down`
        session.lock().unwrap().alive = false;

        // and the watchdog's tick lands here, in the window
        let (tx, rx) = mpsc::channel();
        let watchdog = std::thread::spawn({
            let path = path.clone();
            let session = Arc::clone(&session);
            move || {
                up(&path, &session, "layout");
                let _ = tx.send(());
            }
        });
        // Blocks until the watchdog HAS rebuilt the session, so the failing case is decided by the
        // event rather than by the clock. The timeout only bounds the case where the lock held it
        // off, which is the case that has nothing to wait for.
        let slipped_in = rx
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();

        // ... and `up`, which re-enters the hold rather than waiting for it
        up(&path, &session, "restore");
        drop(restart_lock);
        watchdog.join().unwrap();

        let built_from = { session.lock().unwrap().built_from };
        assert_eq!(
            built_from,
            Some("restore"),
            "the session came back from the layout, so the restore was discarded"
        );
        assert!(
            !slipped_in,
            "the watchdog's `up` ran between the restart's `down` and its `up`"
        );
        assert_eq!(
            session.lock().unwrap().builds,
            1,
            "the session was built more than once"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
