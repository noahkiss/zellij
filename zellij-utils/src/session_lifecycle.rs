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
/// What the scan asks `ps` for.
///
/// **`-ww` is load-bearing.** BSD `ps` - which is macOS's - sizes its output from `COLUMNS` or a
/// `TIOCGWINSZ`, and truncates every line to it; only `-ww` makes the width unlimited. A server's
/// argv is `/opt/homebrew/bin/zellij --server /var/folders/xx/.../T/zellij-501/zellij-<contract>/
/// <name>`, well past eighty columns, and the socket path is the LAST field - so a cut line makes
/// `parse_server_processes` read a truncated socket, `servers_for_session` return nothing for a
/// healthy session, `session status` print `running no`, and the guard that refuses to create a
/// second server for a name stop guarding.
///
/// Accepted by procps and by BSD `ps` alike, which the test below asserts by running it.
#[cfg(unix)]
const PS_ARGS: &[&str] = &["-ww", "-eo", "pid=,command="];

#[cfg(unix)]
pub fn running_servers() -> Vec<ServerProcess> {
    let output = match std::process::Command::new("ps").args(PS_ARGS).output() {
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
/// Comfortably longer than an `up` takes, and it has to STAY that way: a `restart` holds this lock
/// across both halves, so the longest legitimate hold is a `--wait-timeout` down plus a
/// `SERVER_APPEARANCE_TIMEOUT` up - 10 plus 30 seconds at the defaults. A wait that runs out
/// therefore means the holder is wedged rather than busy, and refusing to bring the session up over
/// a wedged neighbour would be a worse outcome than the race the lock prevents. Set this below the
/// sum and a waiting `up` gives up on a restart that is merely slow, which is that race put back.
#[cfg(unix)]
pub const UP_LOCK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
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
    ///
    /// The descriptor is recorded beside the count so that the hold can be given up from a nested
    /// frame - see [`hand_over_up_lock`] - which the outermost `UpLock` alone could not do.
    #[cfg(unix)]
    static HELD_UP_LOCKS: std::cell::RefCell<
        std::collections::HashMap<PathBuf, (usize, std::os::unix::io::RawFd)>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

#[cfg(unix)]
impl Drop for UpLock {
    fn drop(&mut self) {
        HELD_UP_LOCKS.with(|held| {
            let mut held = held.borrow_mut();
            match held.get_mut(&self.path) {
                // an inner holder went away; the outermost one still holds the flock
                Some((holders, _)) if *holders > 1 => *holders -= 1,
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

/// Give the up-lock for `name` up NOW, because another process is about to do the creating.
///
/// The lock exists to keep two creators apart, and it is held across the whole of a `session up` -
/// across the whole of a `restart`, in fact, its `down` and its `up` together. Both of those are
/// right for as long as this process is the one that creates the session. The moment it hands
/// creation to launchd they are exactly wrong: the kickstarted job runs `session up`, that `up`
/// waits for this very lock, and this process is meanwhile waiting for the session that job was
/// going to create. Nothing here can see that it is waiting for itself. It ends with the wait
/// running out, `restart` reporting a post-condition failure, and the session appearing half a
/// minute later once the lock is finally released - a restart that says it failed and did not.
///
/// So the `flock` is released and this thread's record of it dropped, while the `UpLock` values
/// themselves stay alive up the stack. They become handles over nothing, which is what they should
/// be: releasing an already-released lock on drop does nothing, and the thing they were guarding
/// belongs to another process now.
#[cfg(unix)]
pub fn hand_over_up_lock(name: &str) {
    hand_over_up_lock_at(up_lock_path(name));
}

/// Whether the up-lock for `name` is free, asked on a descriptor of its own.
///
/// An `flock` belongs to an open file description rather than to a process, so a fresh handle
/// contends with a hold this process already has exactly as another process would. That is the
/// question a kickstarted launch agent asks, and the only way to ask it from this side without
/// being answered by our own re-entrancy.
#[cfg(unix)]
pub fn up_lock_is_free(name: &str) -> bool {
    use std::os::unix::io::AsRawFd;

    let Ok(file) = std::fs::OpenOptions::new()
        .write(true)
        .open(up_lock_path(name))
    else {
        // no lock file is no lock
        return true;
    };
    // SAFETY: the descriptor is owned by `file`, which outlives both calls
    let taken = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    if taken {
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    taken
}

/// [`hand_over_up_lock`], with the lock file named rather than derived, for the same reason
/// [`lock_up_at`] exists.
#[cfg(unix)]
fn hand_over_up_lock_at(path: PathBuf) {
    HELD_UP_LOCKS.with(|held| {
        if let Some((_, fd)) = held.borrow_mut().remove(&path) {
            // SAFETY: the descriptor is owned by an `UpLock` further up this thread's stack, which
            // cannot have been dropped while a frame below it is running
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
    });
}

/// [`lock_up`], with the lock file named rather than derived, so it can be exercised off a real
/// socket directory.
#[cfg(unix)]
fn lock_up_at(path: PathBuf, name: &str) -> Option<UpLock> {
    use std::os::unix::io::AsRawFd;

    let already_held = HELD_UP_LOCKS.with(|held| match held.borrow_mut().get_mut(&path) {
        Some((holders, _)) => {
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
            HELD_UP_LOCKS.with(|held| {
                held.borrow_mut()
                    .insert(path.clone(), (1, file.as_raw_fd()))
            });
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
/// exists at all, the label of a LOADED job for this session if there is one, whether the session
/// is already there, and whether this process is that job.
///
/// **Being in the graphical session is not a reason to create the session here.** It was, until it
/// turned out to answer a second question wrongly. The domain is only half of what creating a
/// server decides; the other half is which executable macOS holds RESPONSIBLE for what the server
/// then does. A server spawned as a child of the client is attributed to the client's binary, and
/// a Homebrew binary lives at a versioned Cellar path that changes at every upgrade - so the Full
/// Disk Access grant made yesterday names a path today's build is not, and the user is asked
/// again. The launch agent runs the pinned copy, whose path never changes, which is the whole
/// reason the pin exists. So when there is a job to defer to, defer to it from the graphical
/// session too, and keep every server on one responsible path.
///
/// `via_launchd` is the config's say over that one paragraph - `session_service {
/// restart_via_launchd false }` - and over nothing else here. It is an escape hatch for a machine
/// whose agent will not start, not a preference: every other branch is the older guard against a
/// session created in a domain it can never leave, which the config does not get to turn off.
///
/// `running_as_job` is what stops that from being infinite. The job's own `session up` reaches
/// this function in the Aqua domain with its own label installed, and without the flag it would
/// ask launchd to start the job it IS - see [`running_as_launchd_job`].
pub fn gui_domain_action(
    manager_name: Option<&str>,
    gui_domain_available: bool,
    installed_job: Option<&str>,
    session_exists: bool,
    running_as_job: bool,
    via_launchd: bool,
) -> GuiDomainAction {
    // an existing session already has a domain, and `up` is not going to replace it over this
    if session_exists {
        return GuiDomainAction::Proceed;
    }
    // the job cannot defer to itself, and this is the ONLY thing standing between it and a loop
    if running_as_job {
        return GuiDomainAction::Proceed;
    }
    if manager_name == Some(GUI_MANAGER_NAME) {
        // `restart_via_launchd false` turns exactly this off and nothing else: the domain here is
        // already the right one, so creating the session in place is a worse answer rather than a
        // wrong one. The non-graphical branch below is the older guard against a permanently
        // crippled session and is not the config's to disable.
        if !via_launchd {
            return GuiDomainAction::Proceed;
        }
        // already graphical, so a job is an improvement rather than a necessity: with none, this
        // process creates the session in the right domain and nothing is lost but the pinned path
        return match installed_job {
            Some(label) => GuiDomainAction::Kickstart(label.to_owned()),
            None => GuiDomainAction::Proceed,
        };
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

    /// What `launchctl print` says about a target, or `None` when it will not say.
    pub fn print(target: &str) -> Option<String> {
        let output = Command::new("launchctl")
            .args(["print", target])
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn print_succeeds(target: &str) -> bool {
        print(target).is_some()
    }
}

/// The pid `launchctl print` reports for a job, if it is running one.
///
/// Its output is an indented `key = value` block, and the line wanted is `pid = 1234`. Matched on
/// the whole key so that `ppid` - which is on the neighbouring line and would satisfy a substring
/// test - is not read as this one.
pub fn job_pid(printed: &str) -> Option<u32> {
    printed.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "pid").then(|| value.trim().parse().ok())?
    })
}

/// What launchd puts in a job's environment to name the job it is running.
///
/// **launchd's, not ours.** An earlier version of this guard added a `ZELLIJ_SESSION_SERVICE_JOB`
/// key to the generated plist to carry the same fact, which worked and cost every machine a
/// `session enable` to rewrite its agent before the guard could see anything. Reading launchd's
/// own variable needs no plist change at all, so the guard works on the agents that are already
/// installed - confirmed on a real Mac against an agent written two releases ago.
const LAUNCHD_JOB_ENV: &str = "XPC_SERVICE_NAME";

/// Whether the environment alone says this process is the launch agent job for `label`.
///
/// Pure, so the comparison can be exercised without a launchd. An ordinary process has this set to
/// `0` rather than unset, which is why the test is an equality against the label and never a
/// presence check.
pub fn env_says_launchd_job(xpc_service_name: Option<&str>, label: &str) -> bool {
    xpc_service_name == Some(label)
}

/// Whether THIS process is the launch agent job for `label`, and so must not ask launchd to start
/// it.
///
/// Two signals. [`LAUNCHD_JOB_ENV`] is the first and answers on every machine, installed agents
/// included, with no subprocess: launchd sets it in the job's environment and nothing else on the
/// system sets it to our label.
///
/// launchd is then asked directly, as a fallback for an agent that reaches zellij through a
/// wrapper script - where the variable belongs to the wrapper's environment and may not survive
/// into ours. The pid it reports for the job is compared with this process's and with its
/// parent's. **`getppid() == 1` is NOT one of the signals**, tempting as it looks: `session
/// restart` daemonizes before it does any of this work, so a restart typed at a terminal has a
/// parent of 1 too, and reading that as "I am the job" would stop the very command this patch
/// exists to fix from ever kickstarting.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub fn running_as_launchd_job(label: &str) -> bool {
    if env_says_launchd_job(std::env::var(LAUNCHD_JOB_ENV).ok().as_deref(), label) {
        return true;
    }
    let Some(printed) = launchctl::print(&format!("{}/{}", launchctl::gui_domain(), label)) else {
        return false;
    };
    let Some(pid) = job_pid(&printed) else {
        return false;
    };
    let ours = std::process::id();
    // SAFETY: getppid cannot fail and touches nothing
    let parent = unsafe { libc::getppid() } as u32;
    pid == ours || pid == parent
}

/// Arrange for the session to be created in the graphical session, or say why it will not be.
///
/// `Ok(true)` means launchd has been asked for it and the caller is to wait for the session rather
/// than create it itself.
/// Compiled under `cfg(test)` on every unix for the same reason [`launchctl`] is.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub fn ensure_gui_session_domain(
    session: &str,
    session_exists: bool,
    via_launchd: bool,
) -> Result<bool, String> {
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
    // A plist that is on disk and not loaded is the case that would otherwise be silent: nothing
    // is broken, the session is created here and works, and the pinned path the agent exists to
    // keep is quietly not the one macOS will remember. One line, and only when there is something
    // to say.
    if label.is_none() {
        if let Some(job) = found.job() {
            println!(
                "      the launch agent '{}' is on disk but not loaded, so '{}' is created here \
                 instead; `zellij session enable {}` loads it",
                job.name, session, session
            );
        }
    }
    let running_as_job = label
        .as_deref()
        .map(running_as_launchd_job)
        .unwrap_or(false);
    let action = gui_domain_action(
        launchctl::manager_name().as_deref(),
        launchctl::gui_domain_exists(),
        label.as_deref(),
        session_exists,
        running_as_job,
        via_launchd,
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

/// Whether `session` is the name the init system has been told to own.
///
/// TWO names, and the whole gate is that they are the same one. `managed_session` says "the session
/// this config names is the init system's", and the name it refers to is `session_name`, which is
/// already there and already what `session enable|status|up` default to. A name typed on the command
/// line is managed only when it IS that name.
///
/// Matched whole, never as a prefix or a pattern. `zellij -s scratch` on a machine whose
/// `mysession` is managed has to keep working exactly as it did, and a looser test would have
/// this writing a launch agent for every throwaway name somebody types.
pub fn is_managed_session_name(managed: bool, configured: Option<&str>, session: &str) -> bool {
    managed && configured == Some(session)
}

/// What a path that is about to CREATE a managed session should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedCreate {
    /// Build the server here, exactly as every build before this one did.
    CreateHere,
    /// Ask the init system for it, then attach to what it made.
    HandOff,
}

/// The decision itself, kept separate from the machine it is made about.
///
/// `unit_loaded` is the init system's own answer for THIS EXACT name - a loaded launchd job, or a
/// unit the systemd user manager knows. A unit that is merely a file on disk is not something that
/// can be asked for anything, so it is not a reason to defer.
///
/// `running_as_unit` is what stops this from being infinite, and it is the same guard
/// [`gui_domain_action`] carries for its own arm: the process the unit runs reaches this code too,
/// and without the flag it would ask the init system to start the job it already is.
pub fn managed_create_action(
    is_managed_name: bool,
    unit_loaded: bool,
    running_as_unit: bool,
) -> ManagedCreate {
    if is_managed_name && unit_loaded && !running_as_unit {
        ManagedCreate::HandOff
    } else {
        ManagedCreate::CreateHere
    }
}

/// Whether a `/proc/self/cgroup` body says this process is inside the systemd unit `unit`.
///
/// Pure, so the parse can be exercised on a machine with no systemd. The body is one or more
/// `hierarchy:controllers:path` lines and the path ends in the unit name for anything the user
/// manager started - `0::/user.slice/user-1000.slice/user@1000.service/app.slice/some.service`.
///
/// The test is on a whole path SEGMENT, not a substring: a unit called `zellij-session-my.service`
/// must not answer for `zellij-session-mysession.service`, and a substring test would say it
/// does.
pub fn cgroup_says_systemd_unit(cgroup: &str, unit: &str) -> bool {
    cgroup.lines().any(|line| {
        line.rsplit(':')
            .next()
            .map(|path| path.split('/').any(|segment| segment == unit))
            .unwrap_or(false)
    })
}

/// Whether THIS process is the systemd user unit for a session, read from its own cgroup.
///
/// The cgroup rather than an environment variable, for the reason the launchd guard reads launchd's
/// own `XPC_SERVICE_NAME`: it is the init system's record of what it started, so it answers on the
/// units that are already installed and costs no change to a generated file.
#[cfg(target_os = "linux")]
pub fn running_as_systemd_unit(unit: &str) -> bool {
    std::fs::read_to_string("/proc/self/cgroup")
        .map(|cgroup| cgroup_says_systemd_unit(&cgroup, unit))
        .unwrap_or(false)
}

/// Whether THIS process is the init system's own job for `session`.
///
/// The one thing standing between "every create goes through the unit" and a process asking the
/// init system to start the job it is. Both platforms answer from the init system's own record.
pub fn running_as_the_unit(session: &str) -> bool {
    #[cfg(target_os = "macos")]
    {
        running_as_launchd_job(&crate::session_service::launchd_label(session))
    }
    #[cfg(target_os = "linux")]
    {
        running_as_systemd_unit(&crate::session_service::systemd_service_name(session))
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        let _ = session;
        false
    }
}

/// Whether the word `systemctl is-enabled` printed means there is a unit the manager could start.
///
/// It prints a word for every state and exits non-zero for all but one, so the word is the answer
/// and the exit status is not. `disabled` is deliberately a yes: a unit file the manager knows is
/// one it can be asked to start, and only `not-found` means there is nothing there.
pub fn systemd_unit_is_known(state: Option<&str>) -> bool {
    !matches!(state, None | Some("not-found") | Some(""))
}

/// Ask the systemd user manager to create the session, and wait for it to have done so.
///
/// `Ok(true)` means systemd has been asked and the caller is to attach to what it made rather than
/// building one here. `Ok(false)` means there was nothing to defer to and the caller is the creator
/// after all - the same shape [`ensure_gui_session_domain`] answers in, so one caller can handle
/// both platforms.
///
/// The wait is free: the generated unit is `Type=oneshot`, so `systemctl start` returns when the
/// `session up` it runs has finished.
#[cfg(target_os = "linux")]
pub fn ensure_systemd_unit_session(session: &str) -> Result<bool, String> {
    use crate::session_service::{systemctl, systemd_service_name};

    let unit = systemd_service_name(session);
    if !systemd_unit_is_known(systemctl::is_enabled(&unit).as_deref()) {
        return Ok(false);
    }
    // the unit cannot be asked to start the unit
    if running_as_systemd_unit(&unit) {
        return Ok(false);
    }
    println!("      asking systemd for '{}' ({})", session, unit);
    systemctl::start(&unit)?;
    Ok(true)
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
    /// The pinned path held an anchored signature and a different build, and now holds this build
    /// signed with the same certificate. Refreshing and signing were one transaction; see
    /// [`refresh_pin_through_signing`](crate::session_signing::refresh_pin_through_signing).
    Signed(PathBuf),
    /// The pinned path holds an anchored signature and a DIFFERENT build, and it was left exactly
    /// as it was because this run could not sign. The caller gets the path anyway: the previous
    /// signed copy is a working server that still holds its macOS grants, and starting it beats
    /// replacing it with a new build that holds none. The refusal has already been reported.
    Kept(PathBuf),
}

impl PinOutcome {
    pub fn path(&self) -> &Path {
        match self {
            PinOutcome::Installed(path)
            | PinOutcome::Refreshed(path)
            | PinOutcome::UpToDate(path)
            | PinOutcome::Signed(path)
            | PinOutcome::Kept(path) => path,
        }
    }
}

/// What the pin's one writer decided to do about a pin it was about to overwrite.
///
/// Gated with the writer: `install_pinned_exe` is `cfg(unix)`, and a platform with no pin to write
/// has nothing to decide.
#[cfg(unix)]
#[derive(Debug, PartialEq, Eq)]
enum PinRefresh {
    /// Nothing to protect: no signature, or an ad-hoc one that a rebuild voids anyway. The
    /// ordinary copy proceeds.
    Copy,
    /// The signing transaction put this build at the pin's path and signed it. Nothing more to do,
    /// and nothing to re-stamp - the transaction wrote the stamp itself.
    Signed,
    /// The pin carries a signature this run cannot replace, so it was not touched. The reason, for
    /// the one line that says so.
    Kept(String),
}

/// The decision, with the two facts it turns on supplied by the caller.
///
/// Pure and injectable so that the rule can be tested where the macOS ladder cannot run: an
/// anchored pin is never overwritten, a signing run that works owns the refresh, and a signing run
/// that refuses leaves the pin alone rather than falling through to the copy.
#[cfg(unix)]
fn decide_pin_refresh<A, S>(anchored: A, sign: S) -> PinRefresh
where
    A: FnOnce() -> bool,
    S: FnOnce() -> Result<(), String>,
{
    if !anchored() {
        return PinRefresh::Copy;
    }
    match sign() {
        Ok(()) => PinRefresh::Signed,
        Err(reason) => PinRefresh::Kept(reason),
    }
}

/// Pins this process has already refused to refresh, so the refusal is said once.
#[cfg(unix)]
static PIN_REFUSALS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Ask the decision, and report a refusal ONCE per pin per process.
///
/// Once is the requirement, not a nicety. `zellij session up` asserts the pin and then launches a
/// client, which resolves the server binary through the pin again - so a machine that cannot sign
/// reaches this twice in one command. Saying it twice would read as two faults, and asking the
/// keychain twice would mean two dialogs on a machine that is going to refuse either way.
#[cfg(unix)]
fn guard_anchored_pin(source: &Path, target: &Path) -> PinRefresh {
    if pin_refusal_already_said(target) {
        return PinRefresh::Kept(String::new());
    }
    let decision = decide_pin_refresh(
        || crate::session_signing::pin_is_anchored(&crate::session_doctor::SystemCommander, target),
        || crate::session_signing::refresh_pin_through_signing(source, target),
    );
    if let PinRefresh::Kept(reason) = &decision {
        if let Ok(mut said) = PIN_REFUSALS.lock() {
            said.push(target.to_path_buf());
        }
        say_the_pin_was_not_refreshed(reason);
    }
    decision
}

#[cfg(unix)]
fn pin_refusal_already_said(target: &Path) -> bool {
    PIN_REFUSALS
        .lock()
        .map(|said| said.iter().any(|pin| pin == target))
        .unwrap_or(false)
}

/// The one line a machine that cannot sign is told, and the only place it is written.
#[cfg(unix)]
fn say_the_pin_was_not_refreshed(reason: &str) {
    eprintln!("warning: the pin was NOT refreshed: {}", reason);
    eprintln!("         the previously signed copy is still in place, on the previous build,");
    eprintln!("         and that is the build this session starts. Every grant it holds is");
    eprintln!("         intact. Run `zellij session doctor --fix` from a desktop terminal to");
    eprintln!("         finish the upgrade.");
}

/// Put this build at `target`, if it is not there already.
///
/// WRITTEN TO A TEMP FILE IN THE SAME DIRECTORY AND `rename(2)`d over the target, never in place.
/// The comment that stood here said the opposite - that the refresh had to keep the inode, because
/// macOS keys a permission grant to the file rather than the path. That model is refuted. TCC.db
/// has no inode or device column: a non-bundled client is keyed by absolute PATH plus a recorded
/// `csreq` requirement the running process has to satisfy. Mysk's 2026-07 write-up overwrites an
/// executable in place, keeping the inode, and the grants are gone anyway. Path stability is what
/// earns the pin its place, and a rename preserves the path.
///
/// Two reasons for the rename stand on their own, whatever TCC does:
///
/// - it is atomic, so no reader ever sees a half-written binary at the pinned path;
/// - it does not fail `ETXTBSY` against a running server, which holds the pinned copy open for
///   execution. An in-place refresh under a live session also left the kernel holding the OLD
///   cdhash for that vnode, and the next launch died with `OS_REASON_CODESIGNING` while
///   `codesign --verify` called the file valid on disk.
///
/// Whether to write at all is decided by [`pin_is_stale`], which asks what the pinned copy was made
/// FROM rather than what it now contains. The pinned copy is not required to stay byte-identical to
/// its source - signing it changes it deliberately - so a comparison of the two files would call a
/// signed pin stale and copy over the signature on the next `session up`.
///
/// That question is asked of a SHA-256 of the source, which is 40 MB of reading on a pass that
/// nearly always concludes nothing has to be done. [`pin_hash_can_be_skipped`] is a cache in front
/// of it, keyed on what the source looks like to `stat`. It is a cache and not an answer: it can
/// only skip the hash, never supply one, and every case it is unsure about falls through to
/// hashing.
///
/// A write that fails part-way leaves nothing behind: the temp file is removed and the pinned path
/// still holds whatever it held before, which is a working binary rather than a short one.
///
/// **A source that IS the target is refused before any of that.** Once the launcher runs the pin,
/// `session up` passes the pin as its own source - see `pin_this_build_at` - and there is nothing
/// a copy could achieve. Allowed through, it does harm: a SIGNED pin fails the stamp comparison,
/// because signing changed the very file the stamp was taken from, so the refresh copies the pin
/// over itself AND rewrites the stamp to the signed copy's own hash. The next zellij run off
/// `PATH` then reads its unchanged package binary as stale and copies it over the signature,
/// taking every macOS grant with it. Nothing upstream of here can tell the two paths apart.
///
/// **THE ONE WRITER, and it never replaces an anchored signature without signing.** Every path
/// that puts a build at the pin comes through here - `session up`, `session enable`, doctor's
/// `--fix`, and `server_exe_for_interactive_launch` on every interactive launch - and the last of
/// those is why the rule lives here rather than in a caller. It was added to ONE caller first,
/// and the caller that had not been told copied an unsigned build over an Apple Development
/// signature on the very next launch, while the other caller's refusal was still on the screen. A
/// rule a caller can be written without is a rule that will be written without. So: an existing
/// pin that carries an anchored signature is refreshed through the signing transaction or not at
/// all, and a caller cannot ask for anything else, because there is no parameter to ask with.
#[cfg(unix)]
pub fn install_pinned_exe(source: &Path, target: &Path) -> Result<PinOutcome, String> {
    if is_the_same_file(source, target) {
        return Ok(PinOutcome::UpToDate(target.to_path_buf()));
    }
    let key = source_identity(source);
    if target.exists() && pin_hash_can_be_skipped(target, key.as_ref()) {
        return Ok(PinOutcome::UpToDate(target.to_path_buf()));
    }
    let source_hash = sha256_of_file(source);
    if target.exists() && !pin_is_stale(source, target, source_hash.as_deref()) {
        // the pin is current, but nothing here could say so without hashing 40 MB. Record what the
        // source looked like while it was hashed, so the next pass answers from two `stat`s.
        record_pin_source(target, source_hash.as_deref(), key.as_ref());
        return Ok(PinOutcome::UpToDate(target.to_path_buf()));
    }
    let refreshing = target.exists();
    // THE guard, and it is here because here is the only place the pin is written. A pin that
    // holds an anchored signature holds macOS grants with it, and a plain copy over it destroys
    // both - so from this point the copy runs only once something has said there is nothing to
    // lose. See `guard_anchored_pin`.
    if refreshing {
        match guard_anchored_pin(source, target) {
            PinRefresh::Copy => {},
            PinRefresh::Signed => return Ok(PinOutcome::Signed(target.to_path_buf())),
            PinRefresh::Kept(_) => return Ok(PinOutcome::Kept(target.to_path_buf())),
        }
    }
    let directory = pin_directory(target);
    std::fs::create_dir_all(&directory).map_err(|e| pin_write_error(&directory, &e))?;
    let mut input = std::fs::File::open(source)
        .map_err(|e| format!("could not read {}: {}", source.display(), e))?;
    // the same directory, or the rename would cross a filesystem and stop being a rename
    let temporary = directory.join(format!("{}{}.tmp", pin_temp_prefix(), std::process::id()));
    if let Err(reason) = write_pin_temp(&mut input, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        return Err(reason);
    }
    if let Err(error) = std::fs::rename(&temporary, target) {
        let _ = std::fs::remove_file(&temporary);
        return Err(pin_write_error(target, &error));
    }
    sync_pin_directory(&directory);
    // `key` is reused rather than re-`stat`ing the source, and the pairing is the safety. The key
    // exists to say "this hash was taken from a source that looked like THIS", so it has to name
    // the source as it looked WHEN IT WAS HASHED. A source that changed while it was being read
    // then leaves a key the next pass cannot match, which is a re-hash and a correction. A
    // post-copy `stat` would instead file the OLD hash under the NEW identity - the next pass
    // matches, skips the hash, and calls the pin current for as long as the source sits still.
    record_pin_source(target, source_hash.as_deref(), key.as_ref());
    Ok(if refreshing {
        PinOutcome::Refreshed(target.to_path_buf())
    } else {
        PinOutcome::Installed(target.to_path_buf())
    })
}

/// Record which source build the pin was made from, when something other than
/// [`install_pinned_exe`] put it there.
///
/// The macOS signing flow refreshes and signs as one transaction - see
/// [`SigningContext::refresh_from`](crate::session_signing::SigningContext) - so it writes the pin
/// itself and this is how the stamp beside it stays true. Without it the next run reads a stamp
/// naming the previous build, decides the pin is stale, and refreshes a pin that is current.
///
/// The order is the same as `install_pinned_exe`'s and for the same reason: the identity is taken
/// BEFORE the hash, so a source that changed while it was read leaves a key the next pass cannot
/// match, which is a re-hash and a correction rather than a stale hash filed under a fresh identity.
#[cfg(unix)]
pub fn record_pin_refreshed_from(source: &Path, target: &Path) {
    let key = source_identity(source);
    let source_hash = sha256_of_file(source);
    record_pin_source(target, source_hash.as_deref(), key.as_ref());
}

/// Signing is a macOS flow, and `session_signing` compiles everywhere. A stub rather than a
/// `cfg` at the call site: the caller states what it wants once, and the platforms that have no
/// pin to stamp say so here.
#[cfg(not(unix))]
pub fn record_pin_refreshed_from(_source: &Path, _target: &Path) {}

/// Flush a temp the signing flow wrote, before it is renamed over the pin.
///
/// The same guarantee [`install_pinned_exe`] gives its own copy, and it now matters just as much:
/// that rename IS the pin refresh when the two steps are one transaction, so a temp that is not on
/// the disk is a pin that may not be there after a crash. Reported rather than best-effort, for the
/// reason [`sync_pin_temp`] gives - the next statement renames it over the only good binary there
/// is.
///
/// The file is opened again rather than kept: `codesign` wrote it, so this side never held a handle
/// to it. `fsync` on a read-only descriptor is what it needs and all it needs.
#[cfg(unix)]
pub fn flush_pin_temp(temporary: &Path) -> Result<(), String> {
    let handle = std::fs::File::open(temporary)
        .map_err(|e| format!("could not open {} to flush it: {}", temporary.display(), e))?;
    sync_pin_temp(&handle, temporary)
}

#[cfg(not(unix))]
pub fn flush_pin_temp(_temporary: &Path) -> Result<(), String> {
    Ok(())
}

/// Put the renamed NAME on the disk too, after the rename. Best-effort; see [`sync_pin_directory`].
#[cfg(unix)]
pub fn flush_pin_directory(directory: &Path) {
    sync_pin_directory(directory);
}

#[cfg(not(unix))]
pub fn flush_pin_directory(_directory: &Path) {}

/// Whether [`install_pinned_exe`] would write, asked without writing.
///
/// The same questions that function asks, in the same order, so that a dry run reports the
/// decision the real run makes rather than a guess at it. A dry run whose answer is derived some
/// other way is a dry run that eventually disagrees with the fix, and then it is worse than no dry
/// run at all.
///
/// The same-file question is the FIRST of them, and it was missing here while
/// [`install_pinned_exe`] had it - which is a disagreement with real consequences rather than a
/// tidiness point. Doctor run from the pin itself (its directory is on `PATH`, which is the point
/// of pinning) asks this, is told the pin is stale against itself, copies the file over itself,
/// re-signs it, and stamps it with its OWN hash. The next run from the package path then sees a
/// stamp naming the wrong source, calls the pin stale, and copies back over the signature just
/// made. The two alternate for ever, re-signing on every pass, and each cycle throws away the
/// grants the last signature earned.
#[cfg(unix)]
pub fn pin_needs_refresh(source: &Path, target: &Path) -> bool {
    if is_the_same_file(source, target) {
        return false;
    }
    !target.exists() || pin_is_stale(source, target, sha256_of_file(source).as_deref())
}

/// The file recording which source build the pinned copy was made from.
///
/// Beside the pin rather than inside it: the pin has to stay a plain executable a launcher can run,
/// and on macOS its bytes are the thing a signature covers.
#[cfg(unix)]
pub fn pin_source_stamp(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".source-sha256");
    pin_directory(target).join(name)
}

/// The file caching WHICH source the stamp's hash was taken from, as the source looked at the time.
///
/// A second file rather than a second line in the stamp, for two reasons. The stamp is the
/// authority and this is a cache, and a reader that cannot tell them apart will eventually trust
/// the wrong one. And the stamp's format is `<hash>\n` and is compared with `trim()`, so a build
/// that predates this file reads a one-line stamp exactly as it always did - append a line and
/// every older binary on the machine calls the pin stale and copies 40 MB to fix it.
#[cfg(unix)]
pub fn pin_source_key(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".source-key");
    pin_directory(target).join(name)
}

/// What a source file looks like from the outside, cheaply.
///
/// Device and inode as well as size and mtime, because they cost nothing here and they close the
/// commonest way the other three lie: a package manager that unpacks a new build over the old path
/// writes a NEW file, and a new file has a new inode however carefully its mtime was preserved.
#[cfg(unix)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SourceIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: u32,
}

#[cfg(unix)]
impl SourceIdentity {
    fn encode(&self) -> String {
        format!(
            "{} {} {} {} {}",
            self.device, self.inode, self.size, self.modified_seconds, self.modified_nanoseconds
        )
    }

    /// Anything that does not parse is no answer rather than a wrong one - the caller then hashes,
    /// which is what it would have done without this file at all.
    fn decode(fields: &[&str]) -> Option<SourceIdentity> {
        let [device, inode, size, seconds, nanoseconds] = fields else {
            return None;
        };
        Some(SourceIdentity {
            device: device.parse().ok()?,
            inode: inode.parse().ok()?,
            size: size.parse().ok()?,
            modified_seconds: seconds.parse().ok()?,
            modified_nanoseconds: nanoseconds.parse().ok()?,
        })
    }
}

/// Whether two paths name one file, asked of the inode rather than of the spelling.
///
/// A symlink, a hard link and a second spelling of the same directory all reach the pin under a
/// name that is not the pin's, and every one of them is still the pin.
#[cfg(unix)]
fn is_the_same_file(one: &Path, other: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;

    let (Ok(one), Ok(other)) = (std::fs::metadata(one), std::fs::metadata(other)) else {
        return false;
    };
    one.dev() == other.dev() && one.ino() == other.ino()
}

#[cfg(unix)]
fn source_identity(source: &Path) -> Option<SourceIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = std::fs::metadata(source).ok()?;
    Some(SourceIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        size: metadata.size(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec() as u32,
    })
}

/// Whether the 40 MB hash of the source can be skipped on this pass.
///
/// THIS IS A CACHE AND NOT THE ANSWER. The question the pin actually turns on is the one
/// [`pin_is_stale`] asks - does the stamp name the hash of the source in front of us - and the
/// recorded hash stays the only thing allowed to settle it. All this decides is whether the hash
/// has to be TAKEN AGAIN, and it says yes to anything it is not certain about: no key file, a key
/// file that does not parse, a key file recorded against a different stamp, a source that will not
/// `stat`. Every one of those falls through to hashing, which is the behaviour that existed before
/// this cache did.
///
/// It earns its place on the pass that does nothing, which is nearly every pass. `session up` runs
/// from a watchdog every minute and an interactive launch runs it too, and hashing 40 MB to
/// discover that nothing changed cost around 75 ms of every one of them.
///
/// **The blind spot, stated plainly.** A source rewritten in place, to the same size, with its
/// mtime restored and its inode kept, is a source this returns `true` for - so the pin is left
/// alone though its source now holds different bytes. Nothing short of hashing can see that, which
/// is the trade. It needs a writer that preserves size, mtime and inode together: no package
/// manager does, `install` and `cp` do not, and `cp -p` keeps the mtime but writes through a new
/// file only when the target is unlinked first. The recovery is `zellij session doctor --fix` after
/// removing the key file, or any change to the source that moves one of the five fields.
#[cfg(unix)]
fn pin_hash_can_be_skipped(target: &Path, key: Option<&SourceIdentity>) -> bool {
    let Some(key) = key else {
        return false;
    };
    let Ok(recorded) = std::fs::read_to_string(pin_source_key(target)) else {
        return false;
    };
    let fields: Vec<&str> = recorded.split_whitespace().collect();
    let Some((recorded_hash, recorded_identity)) = fields.split_first() else {
        return false;
    };
    if SourceIdentity::decode(recorded_identity) != Some(*key) {
        return false;
    }
    // the key names the hash it was recorded beside. A stamp rewritten by anything else - an older
    // build, a hand-edit, the signing flow re-stamping the pin - leaves the two disagreeing, and a
    // key that does not belong to the stamp in force is not allowed to speak for it.
    match std::fs::read_to_string(pin_source_stamp(target)) {
        Ok(stamped) => stamped.trim() == *recorded_hash,
        Err(_) => false,
    }
}

/// Whether the pinned copy has to be written again.
///
/// The question is NOT whether the two files agree. Once the pin is signed it differs from its
/// source by design, and every signature is a fresh certificate over a fresh cdhash - so an answer
/// derived from the pin's own contents says "stale" forever and the next `session up`, a minute
/// later on the watchdog, copies over the work. What settles it is the source this copy was made
/// from, recorded when it was made.
///
/// A stamp that is missing or unreadable means stale. A pin nobody stamped is a pin from before
/// this scheme, or one somebody put there by hand, and re-copying it once is cheap and correct.
///
/// [`compare_builds`] is the fallback for a source that cannot be hashed at all, which is the same
/// judgement the pin used before there were stamps: worse, because it cannot see a signature, but
/// better than refusing to answer.
#[cfg(unix)]
fn pin_is_stale(source: &Path, target: &Path, source_hash: Option<&str>) -> bool {
    let Some(source_hash) = source_hash else {
        let ours = identify_executable(source.to_path_buf());
        let theirs = identify_executable(target.to_path_buf());
        return compare_builds(Some(&ours), Some(&theirs)) != BuildMatch::Same;
    };
    match std::fs::read_to_string(pin_source_stamp(target)) {
        Ok(recorded) => recorded.trim() != source_hash,
        Err(_) => true,
    }
}

/// Record what the pin was made from, so the next pass can tell a signed pin from a stale one.
///
/// Best effort, and deliberately not an error: the copy is in place and working, and a stamp that
/// could not be written costs one needless 40 MB copy on the next pass rather than a failed
/// `session up`. A partial write is self-correcting for the same reason - it cannot match, so the
/// pin reads stale and is written again.
///
/// The stamp is written FIRST and the key second, and the order is the safety. A key naming a hash
/// the stamp does not carry is ignored by [`pin_hash_can_be_skipped`], so a crash between the two
/// writes costs one hash and never a wrong answer - where the reverse order would leave a key
/// vouching for a stamp that was never written.
#[cfg(unix)]
fn record_pin_source(target: &Path, source_hash: Option<&str>, key: Option<&SourceIdentity>) {
    let stamp = pin_source_stamp(target);
    let key_file = pin_source_key(target);
    match source_hash {
        Some(hash) => {
            if std::fs::write(stamp, format!("{}\n", hash)).is_err() {
                // no stamp means no authority, and a cache for an authority that is not there is
                // the one state that could outlive its stamp and be believed later
                let _ = std::fs::remove_file(&key_file);
                return;
            }
            match key {
                Some(key) => {
                    let _ = std::fs::write(key_file, format!("{} {}\n", hash, key.encode()));
                },
                None => {
                    let _ = std::fs::remove_file(key_file);
                },
            }
        },
        // a stamp left over from an earlier pin would name a source this copy did not come from
        None => {
            let _ = std::fs::remove_file(stamp);
            let _ = std::fs::remove_file(key_file);
        },
    }
}

/// The SHA-256 of a file, as lowercase hex, streamed rather than read whole - the binary is around
/// 40 MB and this runs on every `session up`.
#[cfg(unix)]
fn sha256_of_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;

    let mut file = std::fs::File::open(path).ok()?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => hasher.update(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return None,
        }
    }
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{:02x}", byte))
            .collect(),
    )
}

/// The directory the pinned copy lives in, for a target that may name no directory at all.
#[cfg(unix)]
pub fn pin_directory(target: &Path) -> PathBuf {
    match target.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

/// The name a half-finished refresh leaves behind in the pin directory.
///
/// Its own prefix, distinct from the signing flow's `.zellij.sign.`, so that sweeping one never
/// removes the other. Both live in the pin's directory because a rename has to stay inside one
/// filesystem.
#[cfg(unix)]
pub fn pin_temp_prefix() -> &'static str {
    ".zellij.pin."
}

/// How long an abandoned pin temp has to have sat there before anything removes it.
///
/// An hour, which is far longer than the copy it belongs to could possibly take, and short enough
/// that a `session doctor` run the day after a crash still finds it. The number is not load-bearing
/// - the pid check below is what makes the sweep safe - it is the second belt.
#[cfg(unix)]
pub const PIN_TEMP_MINIMUM_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// The abandoned temp copies in a pin directory: 40 MB each, and nothing else ever removes them.
///
/// A refresh writes `.zellij.pin.<pid>.tmp` and renames it. Killed with `SIGKILL` between the two -
/// an OOM kill, a reboot, a power cut - it leaves the temp file behind for good, and the next
/// refresh writes a new one under a new pid rather than reusing it.
///
/// **Two gates, and the pid one is the important one.** A temp file whose pid is still running
/// belongs to a refresh that is still going, and removing it would make that refresh rename a name
/// nothing holds. `kill(pid, 0)` answers that: `EPERM` counts as alive, because a pid this user may
/// not signal is still a pid in use. The age gate is the second belt, for a fresh temp being
/// written by a process that has not been observed yet, and for a clock or a filesystem that makes
/// the mtime unreadable.
///
/// Neither gate covers pid reuse, and the age gate is not the one that would. A pid recycled onto
/// an unrelated LIVE process makes `kill(pid, 0)` say yes however old the file is, so that temp is
/// kept for good - which is the safe direction, and is why the sweep is a reclamation and not a
/// guarantee.
///
/// **Deliberately not called from [`install_pinned_exe`].** That runs before anything takes a lock,
/// on every `session up` and every interactive launch, so two of them overlap routinely - and a
/// sweep there would be one refresh deleting another's temp file mid-copy. Sweeping belongs to
/// `session doctor`, which the user runs on purpose, one at a time.
#[cfg(unix)]
pub fn stale_pin_temps(directory: &Path, minimum_age: std::time::Duration) -> Vec<PathBuf> {
    stale_temps(directory, pin_temp_prefix(), minimum_age)
}

/// The same two gates, for any `<prefix><pid>.tmp` written beside the pin.
///
/// The pin's refresh is not the only thing that writes one: the macOS signing flow copies the pin
/// to `.zellij.sign.<pid>.tmp`, signs the copy and renames it, and it is abandoned by exactly the
/// same accidents. Both prefixes therefore ask the same question here rather than each keeping its
/// own answer - see [`crate::session_signing::sweep_stale_temps`], which used to remove every
/// `.zellij.sign.*.tmp` it found, including the one a concurrent run was signing into.
#[cfg(unix)]
pub fn stale_temps(
    directory: &Path,
    prefix: &str,
    minimum_age: std::time::Duration,
) -> Vec<PathBuf> {
    let now = std::time::SystemTime::now();
    let mut abandoned: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return false;
            };
            let Some(pid) = name
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_suffix(".tmp"))
            else {
                return false;
            };
            // a name shaped like ours but not written by us is not ours to remove
            let Ok(pid) = pid.parse::<i32>() else {
                return false;
            };
            if process_is_running(pid) {
                return false;
            }
            let Ok(metadata) = entry.metadata() else {
                return false;
            };
            metadata
                .modified()
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age >= minimum_age)
        })
        .map(|entry| entry.path())
        .collect();
    abandoned.sort();
    abandoned
}

/// Remove what [`stale_pin_temps`] found, reporting only what actually went.
#[cfg(unix)]
pub fn sweep_stale_pin_temps(directory: &Path, minimum_age: std::time::Duration) -> Vec<PathBuf> {
    sweep_stale_temps(directory, pin_temp_prefix(), minimum_age)
}

/// Remove what [`stale_temps`] found under one prefix, reporting only what actually went.
#[cfg(unix)]
pub fn sweep_stale_temps(
    directory: &Path,
    prefix: &str,
    minimum_age: std::time::Duration,
) -> Vec<PathBuf> {
    stale_temps(directory, prefix, minimum_age)
        .into_iter()
        .filter(|path| std::fs::remove_file(path).is_ok())
        .collect()
}

/// Whether a pid is in use, asked the only way that is portable across unix.
///
/// Signal 0 is the "check, do not send" signal. `EPERM` means a process is there and belongs to
/// somebody else, which for this purpose is the same answer as yes.
#[cfg(unix)]
fn process_is_running(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().kind() == std::io::ErrorKind::PermissionDenied
}

/// Fill the temp copy, make it executable and get it ONTO THE DISK before it takes the pinned
/// path, so the file that appears there is complete and runnable from its first instant.
///
/// The rename is atomic against other processes, which is a different guarantee from atomic
/// against the power going out. `rename(2)` orders nothing: the directory entry can reach the disk
/// while the 40 MB it points at is still in page cache, and the machine that comes back up then
/// has a pinned path holding a short file. Nothing above could tell - the stamp beside it describes
/// the SOURCE, and the source is intact - so a truncated pin would be judged current and executed
/// on every start until somebody upgraded.
#[cfg(unix)]
fn write_pin_temp(source: &mut std::fs::File, temporary: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;

    let mut output =
        std::fs::File::create(temporary).map_err(|e| pin_write_error(temporary, &e))?;
    std::io::copy(source, &mut output)
        .map_err(|e| format!("could not write {}: {}", temporary.display(), e))?;
    output
        .set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(|e| format!("could not make {} executable: {}", temporary.display(), e))?;
    // after the mode, not before: this has to carry the permission bits down with the bytes
    sync_pin_temp(&output, temporary)
}

/// Flush one finished temp copy to the disk, and REPORT IT WHEN IT FAILS.
///
/// Not best-effort. Everything else here treats a failure as one wasted copy, because the pinned
/// path still holds a working binary. This one is different: a sync that failed is a copy that may
/// not be on the disk, and the very next statement renames it over the only good binary there is.
/// Refusing to rename costs a refresh; renaming anyway can cost the session.
#[cfg(unix)]
fn sync_pin_temp(output: &std::fs::File, temporary: &Path) -> Result<(), String> {
    output.sync_all().map_err(|error| {
        format!(
            "could not flush {} to the disk: {}",
            temporary.display(),
            error
        )
    })
}

/// Flush the pin directory, so the renamed name is on the disk too.
///
/// The other half of the same problem, and the cheaper half: syncing the file puts 40 MB of bytes
/// somewhere durable, and syncing the directory puts the NAME that reaches them there. Without it
/// a crash can lose the rename and leave the old pin in place - which is a stale pin rather than a
/// broken one, and no worse than never having refreshed.
///
/// Best-effort for exactly that reason, and because the rename has already happened by the time it
/// is called: there is nothing left to refuse to do. Not every filesystem lets a directory be
/// opened for `fsync` at all.
#[cfg(unix)]
fn sync_pin_directory(directory: &Path) {
    if let Ok(handle) = std::fs::File::open(directory) {
        let _ = handle.sync_all();
    }
}

/// Why the pinned copy could not be written, saying what to do about the cause that is ordinary:
/// the directory is not ours to write in.
///
/// There is no `ETXTBSY` case here any more. The copy is written to a temp file and renamed over
/// the target, and neither step opens a file that is being executed - which is the point of the
/// rename. A server that is up no longer blocks the refresh of the copy it started from.
#[cfg(unix)]
fn pin_write_error(path: &Path, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::PermissionDenied {
        return format!(
            "could not write {}: {}. The pinned copy is zellij's own, so the directory holding it \
             has to be writable by the user the session runs as.",
            path.display(),
            error
        );
    }
    format!("could not write {}: {}", path.display(), error)
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

/// Whether this running server's own build has been superseded.
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
/// The one addition is for [`pin_exe`](crate::session_service::configured_pinned_exe): an upgrade
/// reaches the pinned copy only when something runs `session up`, so between the two the pinned
/// path still holds the build the server is running and rule two stays silent. There the binary on
/// `PATH` is the intended source of that copy, so it is the right thing to compare against. Once
/// the refresh does happen it renames over the pinned path, which unlinks the file this server
/// started from - and rule one answers.
pub fn build_is_superseded(pinned_exe: Option<&Path>) -> bool {
    inner_build_is_superseded(pinned_exe).unwrap_or(false)
}

fn inner_build_is_superseded(pinned_exe: Option<&Path>) -> Option<bool> {
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

    Some(superseded)
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

/// Whether this binary was refused a Full Disk Access-gated open just now.
///
/// False on every platform without the permission, and false when the question was not answered:
/// [`full_disk_access_granted`] returns `None` for a missing file or a non-permission failure, and
/// neither is a denial.
pub fn full_disk_access_missing() -> bool {
    full_disk_access_granted() == Some(false)
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

    /// A pin with nothing to protect is copied over, exactly as it always was. Ad-hoc and unsigned
    /// pins land here: a rebuild voids what they carry anyway, so refusing to refresh one would
    /// strand the machine on an old build to protect nothing.
    #[cfg(unix)]
    #[test]
    fn a_pin_that_is_not_anchored_is_copied_over() {
        let decision = decide_pin_refresh(
            || false,
            || panic!("the signing transaction was run for a pin with no signature to lose"),
        );
        assert_eq!(decision, PinRefresh::Copy);
    }

    /// An anchored pin is only ever replaced by the transaction that signs it, so a run that CAN
    /// sign hands the whole refresh over - copy, sign and rename are one step, and nothing after
    /// this writes the pin again.
    #[cfg(unix)]
    #[test]
    fn an_anchored_pin_is_refreshed_through_the_signing_transaction() {
        let decision = decide_pin_refresh(|| true, || Ok(()));
        assert_eq!(decision, PinRefresh::Signed);
    }

    /// The case the guard exists for. A locked keychain refuses the key, and the answer is to
    /// leave the signed pin exactly where it is and say so - NOT to fall through to the plain
    /// copy, which is what voided an Apple Development signature on a real machine.
    #[cfg(unix)]
    #[test]
    fn an_anchored_pin_that_cannot_be_signed_is_left_alone() {
        let decision = decide_pin_refresh(|| true, || Err("the keychain is locked".to_owned()));
        assert_eq!(
            decision,
            PinRefresh::Kept("the keychain is locked".to_owned())
        );
    }

    /// A refusal is reported once per pin per process. `session up` asserts the pin and then
    /// launches a client that resolves the server binary through the pin again, so the same
    /// refusal is reached twice in one command.
    #[cfg(unix)]
    #[test]
    fn a_refusal_is_said_once_per_pin() {
        let pin = PathBuf::from("/nowhere/a-pin-this-test-owns");
        assert!(!pin_refusal_already_said(&pin));
        PIN_REFUSALS.lock().unwrap().push(pin.clone());
        assert!(pin_refusal_already_said(&pin));
        assert!(!pin_refusal_already_said(Path::new("/nowhere/another-pin")));
    }

    /// The scan's argv has to be one BOTH `ps` implementations accept, and there is no way to know
    /// that except by running it. `-ww` is the flag that stops BSD `ps` cutting each line to the
    /// terminal width; a cut line loses the socket path, which is the last field, and a healthy
    /// session then looks like no session at all.
    #[test]
    #[cfg(unix)]
    fn the_process_scan_asks_ps_for_untruncated_lines() {
        assert_eq!(PS_ARGS[0], "-ww");
        let output = std::process::Command::new("ps")
            .args(PS_ARGS)
            .output()
            .expect("ps is on every unix this builds for");
        assert!(
            output.status.success(),
            "this platform's ps rejected {:?}: {}",
            PS_ARGS,
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.stdout.is_empty(), "ps listed no processes at all");
    }

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

    /// A terminal in the graphical session used to create the session itself, and the domain it
    /// got was right. What was wrong was the RESPONSIBLE executable: the server it spawns is
    /// attributed to the binary that spawned it, which under Homebrew is a versioned Cellar path
    /// that moves at every upgrade, so the grant made against the pinned copy is never consulted
    /// and the user is prompted again after each one. The agent runs the pin, so it gets the work.
    #[test]
    fn a_graphical_shell_hands_the_work_to_the_job_so_the_pin_stays_responsible() {
        assert_eq!(
            gui_domain_action(
                Some(GUI_MANAGER_NAME),
                true,
                Some("a.label"),
                false,
                false,
                true
            ),
            GuiDomainAction::Kickstart("a.label".to_owned())
        );
    }

    /// With no job there is nothing to hand it to, and being graphical already means creating it
    /// here costs only the pinned path. It is NOT `ProceedWithoutGui`: nothing is missing.
    #[test]
    fn a_graphical_shell_with_no_job_still_creates_the_session_itself() {
        assert_eq!(
            gui_domain_action(Some(GUI_MANAGER_NAME), true, None, false, false, true),
            GuiDomainAction::Proceed
        );
    }

    /// The recursion guard, and the one assertion standing between this patch and a fork bomb of
    /// kickstarts. The job's own `session up` sees precisely the graphical case above.
    #[test]
    fn the_job_itself_never_asks_launchd_to_start_the_job() {
        assert_eq!(
            gui_domain_action(
                Some(GUI_MANAGER_NAME),
                true,
                Some("a.label"),
                false,
                true,
                true
            ),
            GuiDomainAction::Proceed
        );
        // and not from a non-graphical domain either, which is where a `KeepAlive` restart of the
        // job lands if the plist ever grows one
        assert_eq!(
            gui_domain_action(Some("Background"), true, Some("a.label"), false, true, true),
            GuiDomainAction::Proceed
        );
    }

    /// The escape hatch, and the shape of what it may and may not turn off. A machine whose agent
    /// will not start needs a way back to creating the session in place without waiting for a
    /// release; it does not get a way to create one in a domain it can never leave.
    #[test]
    fn restart_via_launchd_false_only_gives_the_graphical_shell_its_old_answer_back() {
        assert_eq!(
            gui_domain_action(
                Some(GUI_MANAGER_NAME),
                true,
                Some("a.label"),
                false,
                false,
                false
            ),
            GuiDomainAction::Proceed
        );
        // the non-graphical guard is older than the key and is not the config's to disable: a
        // session created from here would be crippled for its whole life
        assert_eq!(
            gui_domain_action(
                Some("Background"),
                true,
                Some("a.label"),
                false,
                false,
                false
            ),
            GuiDomainAction::Kickstart("a.label".to_owned())
        );
        assert_eq!(
            gui_domain_action(
                Some("Background"),
                false,
                Some("a.label"),
                false,
                false,
                false
            ),
            GuiDomainAction::NoGuiSession
        );
    }

    /// The primary signal, and the reason no plist has to be rewritten for the guard to work:
    /// launchd sets this itself. Confirmed on a real Mac against an agent two releases old.
    #[test]
    fn launchd_names_the_job_in_the_environment_it_gives_it() {
        let label = "dev.zellij.session.mysession";
        assert!(env_says_launchd_job(Some(label), label));

        // what a process that is NOT a job carries. `0` is the trap: a presence check would read
        // it as "I am the job" and nothing would ever kickstart again
        assert!(!env_says_launchd_job(Some("0"), label));
        assert!(!env_says_launchd_job(None, label));
        // another zellij agent's job is not this one, and a prefix is not a match
        assert!(!env_says_launchd_job(
            Some("dev.zellij.session.other"),
            label
        ));
        assert!(!env_says_launchd_job(Some("dev.zellij.session"), label));
    }

    /// The fallback, for an agent that reaches zellij through a wrapper script - where the variable
    /// above belongs to the wrapper and may not reach us.
    ///
    /// The block is the real one, from `launchctl print gui/501/dev.zellij.session.mysession` on a
    /// Mac, with the paths made generic. The `pid` line is the documented shape a running job adds
    /// to it; that machine's job was idle when it was captured.
    #[test]
    fn the_job_pid_is_read_off_launchctl_print_and_is_not_the_parent_pid() {
        let printed = "\
dev.zellij.session.mysession = {
	active count = 1
	path = /Users/someone/Library/LaunchAgents/dev.zellij.session.mysession.plist
	state = running
	program = /Users/someone/Library/Application Support/zellij/bin/zellij
	arguments = {
		/Users/someone/Library/Application Support/zellij/bin/zellij
		session
		up
		mysession
	}
	runs = 3936
	last exit code = 0
	ppid = 1
	pid = 47213
	environment = {
		XPC_SERVICE_NAME => dev.zellij.session.mysession
	}
}
";
        assert_eq!(job_pid(printed), Some(47213));
        // an idle job reports `state = not running` and no pid at all, which is the state that
        // machine was actually in - inventing one there would make every caller the job
        assert_eq!(
            job_pid("dev.zellij.session.x = {\n\tstate = not running\n\truns = 3936\n}\n"),
            None
        );
        assert_eq!(job_pid(""), None);
    }

    #[test]
    fn a_session_that_already_exists_settles_the_question() {
        // its domain was decided when it was created and `up` is not going to replace it
        assert_eq!(
            gui_domain_action(Some("Background"), true, Some("a.label"), true, false, true),
            GuiDomainAction::Proceed
        );
    }

    #[test]
    fn a_non_graphical_shell_defers_to_the_installed_job() {
        assert_eq!(
            gui_domain_action(
                Some("Background"),
                true,
                Some("a.label"),
                false,
                false,
                true
            ),
            GuiDomainAction::Kickstart("a.label".to_owned())
        );
    }

    #[test]
    fn without_a_job_the_session_is_created_anyway_and_said_so() {
        // no job to defer to, and no session at all is worse than a session without GUI access
        assert_eq!(
            gui_domain_action(Some("Background"), true, None, false, false, true),
            GuiDomainAction::ProceedWithoutGui
        );
    }

    #[test]
    fn with_nobody_logged_in_graphically_there_is_nothing_to_create_it_in() {
        assert_eq!(
            gui_domain_action(
                Some("Background"),
                false,
                Some("a.label"),
                false,
                false,
                true
            ),
            GuiDomainAction::NoGuiSession
        );
        // and an unknown domain is not treated as the graphical one
        assert_eq!(
            gui_domain_action(None, false, None, false, false, true),
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
        install_pinned_exe(&source, &target).expect("a writable temp dir");
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

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn installing_the_pin_records_the_source_it_came_from() {
        let scratch = ScratchDir::new("pin-stamp");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");

        install_pinned_exe(&source, &target).expect("a writable temp dir");
        let recorded = std::fs::read_to_string(pin_source_stamp(&target)).expect("a stamp");
        assert_eq!(
            recorded.trim(),
            sha256_of_file(&source).unwrap(),
            "the stamp has to name the SOURCE, not the copy"
        );
    }

    /// The reason the stamp exists. A signed pin differs from its source by design, and the pass
    /// `session up` takes every minute must not copy over the signature.
    ///
    /// The source carries no linker stamp, which is the case that makes this bite: with no id to
    /// compare, [`compare_builds`] falls through to size, a signature changes the size, and the pin
    /// reads as another build every minute forever.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_pin_that_differs_from_its_source_is_left_alone_while_the_stamp_agrees() {
        let scratch = ScratchDir::new("pin-signed");
        let source = scratch.write("zellij", &vec![0x7f; 4096]);
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");
        // what signing does to the copy: same build, more bytes
        let mut signed = std::fs::read(&target).unwrap();
        signed.extend(std::iter::repeat(0xcd).take(900));
        std::fs::write(&target, &signed).unwrap();

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone()))
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            signed,
            "the signature was copied over by the next `session up`"
        );
    }

    /// What `zellij session doctor --dry-run` reports, asked without writing.
    ///
    /// It has to answer the same three ways the fix acts - no pin, stale pin, current pin - or a
    /// dry run says one thing and the run after it does another, which is the one failure a dry run
    /// cannot survive.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn what_the_pin_would_do_is_answered_without_writing_anything() {
        let scratch = ScratchDir::new("pin-dry");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");

        assert!(
            pin_needs_refresh(&source, &target),
            "a pin that is not there has to be written"
        );
        assert!(!target.exists(), "asking must not write the pin");

        install_pinned_exe(&source, &target).expect("a writable temp dir");
        assert!(
            !pin_needs_refresh(&source, &target),
            "a pin made from this source is current"
        );

        std::fs::write(pin_source_stamp(&target), "a hash of some other build\n").unwrap();
        assert!(
            pin_needs_refresh(&source, &target),
            "a stamp naming another source is a pin from another source"
        );
    }

    /// The pin is not stale against itself, and saying it is starts a loop that never settles.
    ///
    /// Doctor run FROM the pin - which is the ordinary case once the pin directory is on `PATH` -
    /// used to be told to refresh it, so it copied the file over itself, re-signed it, and stamped
    /// it with its own hash. The next run from the package path then read a stamp naming the wrong
    /// source, called the pin stale, and copied back over the signature. Each half of that cycle
    /// throws away the grants the other half's signature earned.
    ///
    /// `install_pinned_exe` has always answered this correctly, which is what made the
    /// disagreement invisible: the writer refused while the question said yes, so only the callers
    /// that ACT on the question - doctor's report, `refresh_belongs_to_signing` - saw it.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_pin_asked_about_itself_is_never_stale() {
        let scratch = ScratchDir::new("pin-self");
        let target = scratch.write("pinned", &elf_with_build_id(&[0xcd; 20], 4096));

        // no stamp at all, which is what a pin doctor has never refreshed looks like, and what
        // `pin_is_stale` reads as "made from something else"
        assert!(!pin_source_stamp(&target).exists());
        assert!(
            !pin_needs_refresh(&target, &target),
            "the pin was called stale against itself"
        );
        // and a stamp naming somebody else does not change it either: the file IS the source
        std::fs::write(pin_source_stamp(&target), "a hash of some other build\n").unwrap();
        assert!(!pin_needs_refresh(&target, &target));
        // the writer agreed all along; this is the assertion the two now share
        assert!(matches!(
            install_pinned_exe(&target, &target),
            Ok(PinOutcome::UpToDate(_))
        ));
    }

    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_pin_whose_stamp_is_missing_or_disagrees_is_written_again() {
        let scratch = ScratchDir::new("pin-stale");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");

        std::fs::write(pin_source_stamp(&target), "a hash of some other build\n").unwrap();
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone())),
            "a stamp naming another source is a pin from another source"
        );

        std::fs::remove_file(pin_source_stamp(&target)).unwrap();
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone())),
            "an unstamped pin is one nothing here put there, and is not trusted"
        );
    }

    /// The point of the key file. The pass that finds nothing to do is nearly every pass, and it
    /// used to hash 40 MB of source to reach that conclusion.
    ///
    /// Proved by making the hash IMPOSSIBLE to take while leaving the five recorded fields exactly
    /// where they were: the source is chmod-ed unreadable, which moves its ctime and nothing the
    /// key looks at. A pass that reaches the hash gets `None` from it, falls through to
    /// [`compare_builds`], finds a pin that is bigger than its source - the signed shape - calls it
    /// stale, and then fails outright because the copy cannot open the source either. So `UpToDate`
    /// is reachable here only by never taking the hash.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_source_that_has_not_moved_is_not_hashed_again() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new("pin-key");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");
        let key = std::fs::read_to_string(pin_source_key(&target)).expect("a key beside the stamp");
        let recorded_hash = key.split_whitespace().next().unwrap().to_owned();
        assert_eq!(
            recorded_hash,
            sha256_of_file(&source).unwrap(),
            "the key has to name the hash it was recorded beside, or it cannot vouch for it"
        );
        // what signing does: the pin stops matching its source by size, so the fallback that a
        // missing hash lands on would call this pin stale
        let mut signed = std::fs::read(&target).unwrap();
        signed.extend(std::iter::repeat(0xcd).take(900));
        std::fs::write(&target, &signed).unwrap();

        let before = source_identity(&source).unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(sha256_of_file(&source), None, "the hash is now impossible");
        assert_eq!(
            source_identity(&source),
            Some(before),
            "chmod moved a field the key reads, so this no longer tests what it says"
        );

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone())),
            "the source was hashed, or the pin was judged without its hash"
        );
        assert_eq!(
            std::fs::read_to_string(pin_source_stamp(&target))
                .unwrap()
                .trim(),
            recorded_hash,
            "the stamp is still the authority and still says what it said"
        );
        // or the scratch directory cannot be removed
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    /// An old pin carries a stamp and no key, and must not be copied over for want of one. It is
    /// hashed once, as it always was, and the key it lacked is written while the hash is in hand.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_stamp_from_before_the_key_is_given_one_without_a_copy() {
        use std::os::unix::fs::MetadataExt;

        let scratch = ScratchDir::new("pin-migrate");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");
        // what a pin installed by a build that predates the key looks like
        std::fs::remove_file(pin_source_key(&target)).unwrap();
        let inode = std::fs::metadata(&target).unwrap().ino();

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone())),
            "a pin whose stamp agrees is current, key file or no key file"
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().ino(),
            inode,
            "40 MB was copied to make up for a missing cache entry"
        );
        assert!(
            std::fs::read_to_string(pin_source_key(&target))
                .unwrap()
                .starts_with(&sha256_of_file(&source).unwrap()),
            "the key was not written, so every later pass hashes the source again"
        );
    }

    /// The cache may never outvote the stamp. A key recorded against some other hash is a key from
    /// a recording that is no longer in force, and the source gets hashed.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_key_that_does_not_belong_to_the_stamp_is_ignored() {
        let scratch = ScratchDir::new("pin-key-orphan");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");

        // the stamp now names a source this pin did not come from. The key still describes the
        // source in front of us, and must not be allowed to say the pin is current.
        std::fs::write(pin_source_stamp(&target), "a hash of some other build\n").unwrap();
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone())),
            "the key spoke for a stamp it was not recorded against"
        );
    }

    /// A source that moved is hashed, and the pin is refreshed when the hash disagrees. The key
    /// gates the hash and nothing else, so a NEW build at the same path is still caught.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_source_that_changed_is_still_caught_through_the_key() {
        let scratch = ScratchDir::new("pin-key-upgrade");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");

        // an upgrade: a new file over the same path, which is a new inode whatever else it keeps
        std::fs::remove_file(&source).unwrap();
        scratch.write("zellij", &elf_with_build_id(&[0xcd; 20], 4096));
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone()))
        );
        assert_eq!(
            identify_executable(target.clone()).build_id,
            Some(vec![0xcd; 20]),
            "the pin was left on the build before the upgrade"
        );
        assert!(
            std::fs::read_to_string(pin_source_key(&target))
                .unwrap()
                .starts_with(&sha256_of_file(&source).unwrap()),
            "the key still names the source the pin no longer came from"
        );
    }

    /// The accepted blind spot, asserted rather than described, so that the day it stops being
    /// true this test says so.
    ///
    /// A source rewritten in place - same size, same inode, mtime put back - is a source the key
    /// cannot tell apart from the one it recorded, and the pin is left alone. Nothing short of
    /// hashing every pass can see this, which is the whole trade. See FORK.md.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_source_rewritten_under_its_own_identity_is_the_blind_spot() {
        use std::fs::OpenOptions;
        use std::io::Write;

        let scratch = ScratchDir::new("pin-key-blind");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");
        let recorded = std::fs::read_to_string(pin_source_key(&target)).unwrap();

        // written THROUGH the file, so the inode and the size are the ones the key recorded
        let replacement = elf_with_build_id(&[0xcd; 20], 4096);
        let mut handle = OpenOptions::new().write(true).open(&source).unwrap();
        handle.write_all(&replacement).unwrap();
        handle.flush().unwrap();
        drop(handle);
        // and the mtime put back where it was, which is the last of the five fields to move
        let key_fields: Vec<&str> = recorded.split_whitespace().collect();
        let seconds: i64 = key_fields[4].parse().unwrap();
        let nanoseconds: i64 = key_fields[5].parse().unwrap();
        set_modified_time(&source, seconds, nanoseconds);
        assert_eq!(
            source_identity(&source).map(|identity| identity.encode()),
            Some(key_fields[1..].join(" ")),
            "the identity moved, so this is no longer the blind spot it means to test"
        );

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone())),
            "the blind spot closed, which is good news and makes this test wrong"
        );
        // and the recovery FORK.md names: drop the cache, and the hash settles it
        std::fs::remove_file(pin_source_key(&target)).unwrap();
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone())),
            "removing the key has to put the pin back under the hash's judgement"
        );
    }

    /// A source that changes after it is hashed must not be filed under the identity it ENDED up
    /// with. The key says "this hash came from a source that looked like this"; pairing the old
    /// hash with the new identity makes the next pass match, skip the hash, and believe a pin that
    /// was never made from the file now sitting at the source path.
    ///
    /// A FIFO at the source path is what holds the run still. `install_pinned_exe` reads the
    /// source twice - once to hash it, once to copy it - and a read of a FIFO blocks until a
    /// writer arrives and ends when that writer leaves. So the writer decides when the hash
    /// finishes, and renaming a plain file over the path BEFORE it closes puts the change squarely
    /// between the hash and everything after it. No sleeps and no polling: the ordering is the
    /// FIFO's, not the scheduler's.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_source_that_changed_after_it_was_hashed_is_not_cached_under_its_new_identity() {
        use std::ffi::CString;
        use std::io::Write;

        let scratch = ScratchDir::new("pin-key-race");
        let source = scratch.0.join("zellij");
        let target = scratch.0.join("pinned");
        let raw = CString::new(source.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(
            unsafe { libc::mkfifo(raw.as_ptr(), 0o644) },
            0,
            "the scratch directory would not take a FIFO"
        );
        // taken before anything writes to the FIFO, so it is the identity the run itself sees
        let hashed_identity = source_identity(&source).expect("a FIFO still `stat`s");

        let hashed_bytes = elf_with_build_id(&[0xab; 20], 4096);
        let replacement_bytes = elf_with_build_id(&[0xcd; 20], 8192);
        let writer_path = source.clone();
        let writer_bytes = hashed_bytes.clone();
        let writer_replacement = replacement_bytes.clone();
        let writer = std::thread::spawn(move || {
            // blocks until the hash opens the FIFO to read it
            let mut handle = std::fs::OpenOptions::new()
                .write(true)
                .open(&writer_path)
                .unwrap();
            handle.write_all(&writer_bytes).unwrap();
            // the source is replaced while the hash still holds the FIFO open. The rename lands
            // before this writer closes, and the hash cannot end until it does - so the copy that
            // follows opens the REPLACEMENT, and any `stat` after the copy sees it too.
            let staged = writer_path.with_extension("replacement");
            std::fs::write(&staged, &writer_replacement).unwrap();
            std::fs::rename(&staged, &writer_path).unwrap();
            drop(handle);
        });

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Installed(target.clone()))
        );
        writer.join().expect("the writer thread panicked");

        // the run is only interesting if the source really did move under it
        assert_ne!(
            source_identity(&source),
            Some(hashed_identity),
            "the source never changed, so this test no longer tests the race"
        );
        let recorded = std::fs::read_to_string(pin_source_key(&target)).unwrap();
        assert!(
            recorded.contains(&hashed_identity.encode()),
            "the key was filed under the identity the source ENDED with: {}",
            recorded.trim()
        );

        // and the behaviour that costs, if it is filed wrong: the next pass matches its own
        // `stat`, skips the hash, and calls a pin the source never produced current
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone())),
            "the change was cached away - the pin is now wrong for as long as the source sits still"
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            replacement_bytes,
            "the refresh did not put the pin on the source that is actually there"
        );
    }

    /// `utimensat`, which `std::fs` has no wrapper for. Only a test needs it: the blind spot cannot
    /// be reached without putting an mtime back where it was.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn set_modified_time(path: &Path, seconds: i64, nanoseconds: i64) {
        use std::ffi::CString;

        let raw = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let times = [
            libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanoseconds,
            },
            libc::timespec {
                tv_sec: seconds,
                tv_nsec: nanoseconds,
            },
        ];
        let set = unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(set, 0, "could not put the mtime back");
    }

    /// A pid that is certainly not in use: spawned, waited for, and therefore reaped.
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_pid_that_has_finished() -> u32 {
        // `/bin/sh`, not `/bin/true`: POSIX puts a shell at that path on every unix, while macOS
        // keeps `true` in `/usr/bin` and has nothing at `/bin/true` to spawn.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("every unix has a shell");
        let pid = child.id();
        child.wait().expect("it exits at once");
        pid
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    fn an_abandoned_temp(scratch: &ScratchDir, pid: u32, age: std::time::Duration) -> PathBuf {
        let path = scratch.write(
            &format!("{}{}.tmp", pin_temp_prefix(), pid),
            &vec![0x7f; 512],
        );
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            - age;
        set_modified_time(&path, when.as_secs() as i64, 0);
        path
    }

    /// The file finding 3 is about: `SIGKILL` between the copy and the rename leaves 40 MB in the
    /// pin directory, the next refresh writes a new one under a new pid, and nothing else ever
    /// removes them.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn an_abandoned_temp_copy_is_swept_once_it_is_old_enough() {
        let scratch = ScratchDir::new("pin-sweep");
        let abandoned = an_abandoned_temp(
            &scratch,
            a_pid_that_has_finished(),
            std::time::Duration::from_secs(2 * 60 * 60),
        );

        assert_eq!(
            sweep_stale_pin_temps(&scratch.0, PIN_TEMP_MINIMUM_AGE),
            vec![abandoned.clone()]
        );
        assert!(!abandoned.exists(), "it is still there");
    }

    /// The gate that makes the sweep safe. A temp file whose pid is alive belongs to a refresh
    /// that is still copying into it, and removing it would leave that refresh renaming a name
    /// nothing holds.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_temp_copy_whose_process_is_alive_is_never_swept() {
        let scratch = ScratchDir::new("pin-sweep-live");
        // this process, which is beyond argument the most alive pid available
        let live = an_abandoned_temp(
            &scratch,
            std::process::id(),
            std::time::Duration::from_secs(48 * 60 * 60),
        );

        assert!(process_is_running(std::process::id() as i32));
        assert_eq!(
            sweep_stale_pin_temps(&scratch.0, PIN_TEMP_MINIMUM_AGE),
            Vec::<PathBuf>::new(),
            "a refresh in flight had its temp file deleted under it"
        );
        assert!(live.exists());
    }

    /// The second belt: a pid can be recycled onto some unrelated process, and a temp written
    /// moments ago by a process nothing has observed yet must survive that coincidence.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_temp_copy_younger_than_the_gate_is_left_alone() {
        let scratch = ScratchDir::new("pin-sweep-young");
        let fresh = an_abandoned_temp(
            &scratch,
            a_pid_that_has_finished(),
            std::time::Duration::from_secs(60),
        );

        assert_eq!(
            stale_pin_temps(&scratch.0, PIN_TEMP_MINIMUM_AGE),
            Vec::<PathBuf>::new()
        );
        assert!(fresh.exists());
    }

    /// The pin directory holds the pin, its two stamps and the signing flow's own temp files. A
    /// sweep that took any of those would be worse than the files it is there to remove.
    ///
    /// The signing temp is given THE SAME dead pid as the pin temp, deliberately. Any other pid
    /// and the liveness gate would be what spares it, and the prefix - the thing that actually
    /// keeps the two sweeps out of each other's files - would go untested.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_sweep_takes_nothing_that_is_not_a_pin_temp() {
        let scratch = ScratchDir::new("pin-sweep-others");
        let old = std::time::Duration::from_secs(48 * 60 * 60);
        let finished = a_pid_that_has_finished();
        let sign_temp = format!(".zellij.sign.{}.tmp", finished);
        let bystanders = [
            sign_temp.as_str(),
            "zellij",
            "zellij.source-sha256",
            "zellij.source-key",
            ".zellij.pin.not-a-pid.tmp",
            ".zellij.pin.1",
        ];
        for name in bystanders {
            let path = scratch.write(name, b"whatever");
            let when = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                - old;
            set_modified_time(&path, when.as_secs() as i64, 0);
        }
        let ours = an_abandoned_temp(&scratch, finished, old);

        assert_eq!(
            sweep_stale_pin_temps(&scratch.0, PIN_TEMP_MINIMUM_AGE),
            vec![ours]
        );
        for name in bystanders {
            assert!(scratch.0.join(name).exists(), "{} was swept", name);
        }
    }

    /// The temp copy is flushed to the disk before anything renames it over the pinned path, and a
    /// flush that fails STOPS the refresh instead of renaming a file that may not be there.
    ///
    /// `fsync` on a character device is `EINVAL` on Linux, which is the only way to make the flush
    /// fail without a filesystem that is coming apart. It also has to be asked of the flush on its
    /// own: the whole of [`write_pin_temp`] cannot be pointed at `/dev/null`, because setting the
    /// mode on it fails first for a user that does not own it.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_temp_copy_that_will_not_flush_is_a_refusal_and_not_a_warning() {
        let sink = std::fs::OpenOptions::new()
            .write(true)
            .open("/dev/null")
            .expect("every unix has one");

        let refused = sync_pin_temp(&sink, Path::new("/dev/null"));
        assert!(
            refused.is_err(),
            "fsync on a character device stopped failing, so this no longer proves anything"
        );
        assert!(
            refused.unwrap_err().contains("could not flush"),
            "the refusal has to name what went wrong, or `session up` prints nothing useful"
        );
    }

    /// And the ordinary path runs the flush and completes: same bytes, still executable.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_finished_copy_is_flushed_before_it_takes_the_pinned_path() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = ScratchDir::new("pin-flush");
        let contents = elf_with_build_id(&[0xab; 20], 4096);
        let source = scratch.write("zellij", &contents);
        let temporary = scratch.0.join(".zellij.pin.1.tmp");
        let mut handle = std::fs::File::open(&source).unwrap();

        assert_eq!(write_pin_temp(&mut handle, &temporary), Ok(()));
        assert_eq!(std::fs::read(&temporary).unwrap(), contents);
        let mode = std::fs::metadata(&temporary).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o111,
            0o111,
            "the flush has to come after the mode, or it carries down the wrong metadata"
        );

        // the directory flush is the best-effort half: it must not panic, and it must not care
        sync_pin_directory(&scratch.0);
        sync_pin_directory(&scratch.0.join("no-such-directory"));
    }

    /// The stamp cannot answer for a source that will not read, and refusing to answer would leave
    /// the pin unmanaged. The build comparison is the same judgement the pin used before stamps.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn without_a_source_hash_the_build_comparison_decides() {
        let scratch = ScratchDir::new("pin-nohash");
        let unreadable = scratch.0.join("a-directory");
        std::fs::create_dir(&unreadable).expect("a writable temp dir");
        let other = scratch.write("pinned", &elf_with_build_id(&[0xcd; 20], 4096));

        assert_eq!(sha256_of_file(&unreadable), None);
        assert!(!pin_is_stale(&unreadable, &unreadable, None), "one file");
        assert!(pin_is_stale(&unreadable, &other, None), "two files");
    }

    /// The refresh renames a finished copy over the target rather than writing through the file a
    /// running server is executing. What TCC keys on is the path, which the rename keeps; what an
    /// in-place write cost was a live server killed with `OS_REASON_CODESIGNING`.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_refresh_replaces_the_pinned_file_rather_than_writing_through_it() {
        use std::os::unix::fs::MetadataExt;

        let scratch = ScratchDir::new("pin-refresh");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 8192));
        let target = scratch.write("pinned", &elf_with_build_id(&[0xcd; 20], 4096));
        let before = std::fs::metadata(&target).unwrap().ino();

        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::Refreshed(target.clone()))
        );
        assert_ne!(
            std::fs::metadata(&target).unwrap().ino(),
            before,
            "the old file was written through, so a running server keeps executing the new bytes"
        );
        assert_eq!(
            identify_executable(target).build_id,
            Some(vec![0xab; 20]),
            "and it is the new build"
        );
    }

    /// The pinned copy is the binary a launcher execs, so a failed refresh must leave the working
    /// one in place. A directory opens but does not read, which fails the copy after the temp file
    /// exists - the one window where an in-place write would already have truncated the target.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn a_write_that_fails_leaves_the_pinned_copy_and_no_temp_behind() {
        let scratch = ScratchDir::new("pin-fail");
        let source = scratch.0.join("not-a-file");
        std::fs::create_dir(&source).expect("a writable temp dir");
        let target = scratch.write("pinned", &elf_with_build_id(&[0xcd; 20], 4096));
        let before = std::fs::read(&target).unwrap();

        assert!(install_pinned_exe(&source, &target).is_err());
        assert_eq!(
            std::fs::read(&target).unwrap(),
            before,
            "the pinned copy was damaged by a refresh that never completed"
        );
        let leftovers: Vec<_> = std::fs::read_dir(&scratch.0)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "a failed refresh left {:?} behind, and the binary is around 40 MB",
            leftovers
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

    /// Handing creation to launchd hands the lock over with it. The job launchd starts runs
    /// `session up` and waits for this lock; a caller that kept holding it while waiting for the
    /// session would be waiting for itself, and `restart` would report a failure it caused.
    ///
    /// Written as `restart` shapes it - an outer hold with a nested one inside - because that is
    /// the case a lone `UpLock::drop` cannot answer: the nested frame cannot drop its caller's
    /// handle.
    #[test]
    #[cfg(unix)]
    fn handing_the_work_to_launchd_gives_the_lock_up_from_a_nested_frame() {
        let dir = lock_scratch("handover");
        let path = dir.join(".work.up.lock");

        let outer = lock_up_at(path.clone(), "work").expect("the lock is free");
        let inner = lock_up_at(path.clone(), "work").expect("re-entered rather than waited");
        assert!(!lock_is_free(&path));

        hand_over_up_lock_at(path.clone());
        assert!(
            lock_is_free(&path),
            "the job launchd just started would wait for this"
        );

        // both handles are still alive and become handles over nothing. Dropping them must not
        // panic, and must not release a lock the other process now holds either.
        drop(inner);
        drop(outer);
        let after = lock_up_at(path.clone(), "work").expect("the lock is free");
        assert!(
            after.file.is_some(),
            "re-entered a hold that had been handed over"
        );
        drop(after);
        assert!(lock_is_free(&path));
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

    /// The launcher execs the pin, so `session up` hands the pin to itself as its own source. A
    /// copy could achieve nothing there, and the ordinary path does harm.
    ///
    /// Signing deliberately changes the pin, and the stamp was taken from the file signing
    /// changed. Left to fall through, the self-compare calls the signed pin stale, copies it over
    /// itself and re-stamps it with the signed copy's own hash - and the next run off `PATH`, with
    /// the package binary UNCHANGED and no upgrade anywhere, then reads that stamp, calls the pin
    /// stale and copies the unsigned package over the signature. Every macOS grant goes with it.
    #[test]
    #[cfg(all(unix, not(target_os = "macos")))]
    fn the_pin_handed_to_itself_as_its_own_source_is_left_exactly_alone() {
        let scratch = ScratchDir::new("pin-self");
        let source = scratch.write("zellij", &elf_with_build_id(&[0xab; 20], 4096));
        let target = scratch.0.join("pinned");
        install_pinned_exe(&source, &target).expect("a writable temp dir");
        let stamped = std::fs::read_to_string(pin_source_stamp(&target)).unwrap();

        // what signing does: the pin stops being byte-identical to the source it came from
        let mut signed = std::fs::read(&target).unwrap();
        signed.extend(std::iter::repeat(0xcd).take(900));
        std::fs::write(&target, &signed).unwrap();

        assert_eq!(
            install_pinned_exe(&target, &target),
            Ok(PinOutcome::UpToDate(target.clone())),
            "the pin was compared with itself and copied over itself"
        );
        assert_eq!(
            std::fs::read_to_string(pin_source_stamp(&target)).unwrap(),
            stamped,
            "the self-compare re-stamped the pin with its own hash, which is the damage"
        );

        // and the package binary, unchanged and still the source the stamp names, stays current
        assert_eq!(
            install_pinned_exe(&source, &target),
            Ok(PinOutcome::UpToDate(target.clone()))
        );
        assert_eq!(
            std::fs::read(&target).unwrap(),
            signed,
            "the signature was copied over by a refresh nothing had asked for"
        );
    }

    /// The gate that keeps `managed_session` from touching anything but the one name it is about.
    #[test]
    fn only_the_configured_session_name_is_managed() {
        assert!(is_managed_session_name(
            true,
            Some("mysession"),
            "mysession"
        ));
        // the ad-hoc session on the same machine, which must be untouched
        assert!(!is_managed_session_name(true, Some("mysession"), "scratch"));
        // a prefix of the managed name is a DIFFERENT name, not a match
        assert!(!is_managed_session_name(true, Some("mysession"), "my"));
        // the key off is the whole feature off
        assert!(!is_managed_session_name(
            false,
            Some("mysession"),
            "mysession"
        ));
        // and no `session_name` in the config means there is nothing for the key to refer to
        assert!(!is_managed_session_name(true, None, "mysession"));
    }

    /// Three conditions, and every one of them is a veto. The last is the recursion guard.
    #[test]
    fn a_create_is_handed_over_only_when_a_loaded_unit_owns_the_name() {
        assert_eq!(
            managed_create_action(true, true, false),
            ManagedCreate::HandOff
        );
        // a unit the init system does not hold cannot be asked for anything
        assert_eq!(
            managed_create_action(true, false, false),
            ManagedCreate::CreateHere
        );
        assert_eq!(
            managed_create_action(false, true, false),
            ManagedCreate::CreateHere
        );
        // the unit's own process asking the init system for the unit is the loop this prevents
        assert_eq!(
            managed_create_action(true, true, true),
            ManagedCreate::CreateHere
        );
    }

    /// The Linux half of the recursion guard, and the reason it reads a path SEGMENT: one session
    /// name being a prefix of another must not make one unit answer for the other.
    #[test]
    fn a_cgroup_names_the_unit_it_is_in_and_not_a_neighbour() {
        let inside = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/\
                      zellij-session-mysession.service\n";
        assert!(cgroup_says_systemd_unit(
            inside,
            "zellij-session-mysession.service"
        ));
        assert!(!cgroup_says_systemd_unit(
            inside,
            "zellij-session-my.service"
        ));
        // a login shell is in the session scope, not in any unit of ours
        assert!(!cgroup_says_systemd_unit(
            "0::/user.slice/user-1000.slice/session-3.scope\n",
            "zellij-session-mysession.service"
        ));
        // cgroup v1 writes several lines, and the unit may be named on only one of them
        let v1 = "12:pids:/user.slice/user-1000.slice/session-3.scope\n\
                  0::/user.slice/user-1000.slice/user@1000.service/zellij-session-x.service\n";
        assert!(cgroup_says_systemd_unit(v1, "zellij-session-x.service"));
        assert!(!cgroup_says_systemd_unit("", "zellij-session-x.service"));
    }

    /// `systemctl is-enabled` prints a word for every state and exits non-zero for all but one, so
    /// the word is the answer. A DISABLED unit is still one the manager can be told to start.
    #[test]
    fn only_not_found_means_there_is_no_unit_to_start() {
        assert!(systemd_unit_is_known(Some("enabled")));
        assert!(systemd_unit_is_known(Some("disabled")));
        assert!(systemd_unit_is_known(Some("static")));
        assert!(!systemd_unit_is_known(Some("not-found")));
        assert!(!systemd_unit_is_known(Some("")));
        assert!(!systemd_unit_is_known(None));
    }
}
