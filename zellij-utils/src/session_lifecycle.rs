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
