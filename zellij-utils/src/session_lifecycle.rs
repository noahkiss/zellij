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
#[cfg(unix)]
pub struct UpLock {
    file: std::fs::File,
}

#[cfg(unix)]
impl Drop for UpLock {
    fn drop(&mut self) {
        use std::os::unix::io::AsRawFd;
        // closing the descriptor would release it anyway; releasing it deliberately is cheaper
        // than leaving the next reader to know that
        // SAFETY: the descriptor is owned by this struct and is open for as long as it is
        unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Take the lock for `name`, waiting for a holder to finish.
///
/// `None` means the caller proceeds unlocked, and both ways of getting it are deliberate. A lock
/// file that cannot be created (an unwritable socket directory) and a holder that never lets go are
/// both worse reasons to leave a machine with no session than the race is to run.
#[cfg(unix)]
pub fn lock_up(name: &str) -> Option<UpLock> {
    use std::os::unix::io::AsRawFd;

    let path = up_lock_path(name);
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
            return Some(UpLock { file });
        }
        if std::time::Instant::now() >= deadline {
            eprintln!(
                "warning: another `zellij session up {}` has held {} for {}s. Going ahead \
                 without the lock, so two servers for this name are possible.",
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
}
