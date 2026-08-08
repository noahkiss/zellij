use crate::{
    consts::{
        is_ipc_socket, session_info_folder_for_session, session_layout_cache_file_name,
        ZELLIJ_SESSION_INFO_CACHE_DIR, ZELLIJ_SOCK_DIR,
    },
    envs,
    input::layout::Layout,
    ipc::{ClientToServerMsg, IpcReceiverWithContext, IpcSenderWithContext, ServerToClientMsg},
    session_snapshot::{archive_session_info, SnapshotReason, SnapshotSettings},
};
use anyhow;
use humantime::format_duration;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use std::{fs, io, process};
use suggest::Suggest;

pub fn get_sessions() -> Result<Vec<(String, Duration)>, io::ErrorKind> {
    match fs::read_dir(&*ZELLIJ_SOCK_DIR) {
        Ok(files) => {
            let mut sessions = Vec::new();
            files.for_each(|file| {
                if let Ok(file) = file {
                    let file_name = file.file_name().into_string().unwrap();
                    // try to get creation time, fall back to modification time on platforms where it's not supported (e.g., musl)
                    // for session creation time these are almost always identical (notable
                    // exceptions are session name changes)
                    let ctime = std::fs::metadata(&file.path())
                        .ok()
                        .and_then(|f| f.created().ok().or_else(|| f.modified().ok()))
                        .and_then(|d| d.elapsed().ok())
                        .unwrap_or_default();
                    let duration = Duration::from_secs(ctime.as_secs());
                    if is_ipc_socket(&file.file_type().unwrap()) && assert_socket(&file_name) {
                        sessions.push((file_name, duration));
                    }
                }
            });
            Ok(sessions)
        },
        Err(err) if io::ErrorKind::NotFound != err.kind() => Err(err.kind()),
        Err(_) => Ok(Vec::with_capacity(0)),
    }
}

/// Live sessions of this contract version sitting in a socket root this environment did *not*
/// resolve to, keyed by the directory they were found in.
///
/// Two clients with different environments each build their own server and neither can see the
/// other, so `zellij ls` reports "no sessions" while a server is very much alive elsewhere. This
/// is the scan that makes that split visible.
///
/// Read-only, deliberately: unlike [`assert_socket`], a socket that refuses a connection is left
/// in place rather than removed, because it is not this environment's to clean up.
pub fn get_sessions_in_other_socket_dirs() -> Vec<(std::path::PathBuf, Vec<String>)> {
    use crate::consts::{socket_dir_candidates, CLIENT_SERVER_CONTRACT_DIR};

    socket_dir_candidates()
        .into_iter()
        .map(|root| root.join(&*CLIENT_SERVER_CONTRACT_DIR))
        .filter(|dir| dir != &*ZELLIJ_SOCK_DIR)
        .filter_map(|dir| {
            let sessions: Vec<String> = fs::read_dir(&dir)
                .ok()?
                .filter_map(|file| {
                    let file = file.ok()?;
                    if !is_ipc_socket(&file.file_type().ok()?) {
                        return None;
                    }
                    if !probe_socket(&file.path()) {
                        return None;
                    }
                    file.file_name().into_string().ok()
                })
                .collect();
            if sessions.is_empty() {
                None
            } else {
                Some((dir, sessions))
            }
        })
        .collect()
}

/// The client/server contract versions a session by this name has a socket under, other than the
/// one this binary speaks.
///
/// The socket path is contract-scoped and the wire format genuinely differs across a contract bump,
/// so a mismatched client attaching to a live server is a protocol violation rather than a path
/// problem. The right response is a better failure, which needs to know the mismatch is there.
/// Nothing is probed: another contract's server would not understand the question.
pub fn session_in_other_contract_versions(name: &str) -> Vec<usize> {
    use crate::consts::{socket_dir_candidates, CLIENT_SERVER_CONTRACT_DIR};

    let mut contracts: Vec<usize> = socket_dir_candidates()
        .into_iter()
        .flat_map(|root| fs::read_dir(&root).into_iter().flatten().flatten())
        .filter_map(|entry| {
            let dir_name = entry.file_name().to_string_lossy().to_string();
            if dir_name == *CLIENT_SERVER_CONTRACT_DIR {
                return None;
            }
            let contract = dir_name.strip_prefix("contract_version_")?.parse().ok()?;
            let socket = entry.path().join(name);
            if is_ipc_socket(&fs::metadata(&socket).ok()?.file_type()) {
                Some(contract)
            } else {
                None
            }
        })
        .collect();
    contracts.sort_unstable();
    contracts.dedup();
    contracts
}

/// Every socket directory a listing consulted: the one this binary resolved, then the ones a
/// differently-configured environment would have landed in.
///
/// These are the DERIVED candidates and nothing else. A server created under a `ZELLIJ_SOCKET_DIR`
/// that this process was not given cannot be derived from here by anything, which is exactly why
/// naming the list is worth doing: a reader who exported one somewhere else can see at a glance
/// that their directory is not on it.
pub fn searched_socket_dirs() -> Vec<std::path::PathBuf> {
    use crate::consts::{socket_dir_candidates, CLIENT_SERVER_CONTRACT_DIR};

    let mut dirs = vec![ZELLIJ_SOCK_DIR.clone()];
    for root in socket_dir_candidates() {
        let dir = root.join(&*CLIENT_SERVER_CONTRACT_DIR);
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    }
    dirs
}

/// Say where the listing looked, for the answer that otherwise explains nothing.
///
/// "No active zellij sessions found" is true of a directory, not of a machine, and the two come
/// apart routinely: a session created under an exported `ZELLIJ_SOCKET_DIR` that this shell does
/// not have is running, reachable and completely absent from this list. Naming the directories
/// turns the bare sentence into one a reader can check.
fn print_searched_socket_dirs() {
    let dirs = searched_socket_dirs();
    let mut dirs = dirs.iter();
    if let Some(first) = dirs.next() {
        eprintln!("  looked in {}", first.display());
    }
    for other in dirs {
        eprintln!("  and in    {}", other.display());
    }
    eprintln!(
        "  A server started with a ZELLIJ_SOCKET_DIR this shell does not have is not in that\n  \
         list and cannot be. `zellij session up <name>` scans the process table instead, so it\n  \
         sees one when this does not."
    );
}

fn print_other_socket_dir_warning() {
    for (dir, sessions) in get_sessions_in_other_socket_dirs() {
        eprintln!(
            "WARNING: {} live session(s) in another socket directory: {}",
            sessions.len(),
            dir.display()
        );
        for session in sessions {
            eprintln!("  {}", session);
        }
        eprintln!(
            "These are invisible to this environment. ZELLIJ_SOCK_DIR here is: {}",
            ZELLIJ_SOCK_DIR.display()
        );
    }
}

pub fn get_resurrectable_sessions() -> Vec<(String, Duration)> {
    match fs::read_dir(&*ZELLIJ_SESSION_INFO_CACHE_DIR) {
        Ok(files_in_session_info_folder) => {
            let files_that_are_folders = files_in_session_info_folder
                .filter_map(|f| f.ok().map(|f| f.path()))
                .filter(|f| f.is_dir());
            files_that_are_folders
                .filter_map(|folder_name| {
                    let layout_file_name =
                        session_layout_cache_file_name(&folder_name.display().to_string());
                    // Try to get creation time, fall back to modification time on platforms where it's not supported (e.g., musl)
                    let ctime = std::fs::metadata(&layout_file_name)
                        .ok()
                        .and_then(|metadata| {
                            metadata.created().ok().or_else(|| metadata.modified().ok())
                        });
                    let elapsed_duration = ctime
                        .map(|ctime| {
                            Duration::from_secs(ctime.elapsed().ok().unwrap_or_default().as_secs())
                        })
                        .unwrap_or_default();
                    let session_name = folder_name
                        .file_name()
                        .map(|f| std::path::PathBuf::from(f).display().to_string())?;
                    if std::path::Path::new(&layout_file_name).exists() {
                        Some((session_name, elapsed_duration))
                    } else {
                        None
                    }
                })
                .collect()
        },
        Err(e) => {
            log::error!(
                "Failed to read session_info cache folder: \"{:?}\": {:?}",
                &*ZELLIJ_SESSION_INFO_CACHE_DIR,
                e
            );
            vec![]
        },
    }
}

pub fn get_resurrectable_session_names() -> Vec<String> {
    match fs::read_dir(&*ZELLIJ_SESSION_INFO_CACHE_DIR) {
        Ok(files_in_session_info_folder) => {
            let files_that_are_folders = files_in_session_info_folder
                .filter_map(|f| f.ok().map(|f| f.path()))
                .filter(|f| f.is_dir());
            files_that_are_folders
                .filter_map(|folder_name| {
                    let folder = folder_name.display().to_string();
                    let resurrection_layout_file = session_layout_cache_file_name(&folder);
                    if std::path::Path::new(&resurrection_layout_file).exists() {
                        folder_name
                            .file_name()
                            .map(|f| format!("{}", f.to_string_lossy()))
                    } else {
                        None
                    }
                })
                .collect()
        },
        Err(e) => {
            log::error!(
                "Failed to read session_info cache folder: \"{:?}\": {:?}",
                &*ZELLIJ_SESSION_INFO_CACHE_DIR,
                e
            );
            vec![]
        },
    }
}

pub fn get_sessions_sorted_by_mtime() -> anyhow::Result<Vec<String>> {
    match fs::read_dir(&*ZELLIJ_SOCK_DIR) {
        Ok(files) => {
            let mut sessions_with_mtime: Vec<(String, SystemTime)> = Vec::new();
            for file in files {
                let file = file?;
                let file_name = file.file_name().into_string().unwrap();
                let file_modified_at = file.metadata()?.modified()?;
                if is_ipc_socket(&file.file_type()?) && assert_socket(&file_name) {
                    sessions_with_mtime.push((file_name, file_modified_at));
                }
            }
            sessions_with_mtime.sort_by_key(|x| x.1); // the oldest one will be the first

            let sessions = sessions_with_mtime.iter().map(|x| x.0.clone()).collect();
            Ok(sessions)
        },
        Err(err) if io::ErrorKind::NotFound != err.kind() => Err(err.into()),
        Err(_) => Ok(Vec::with_capacity(0)),
    }
}

/// Probe a session socket to check if a server is alive.
///
/// On Unix, connects and sends a `ConnStatus` message to verify the server responds.
/// On Windows, reads the server PID from the marker file and checks process liveness.
#[cfg(unix)]
fn assert_socket(name: &str) -> bool {
    use crate::consts::ipc_connect;
    let path = &*ZELLIJ_SOCK_DIR.join(name);
    match ipc_connect(path) {
        Ok(stream) => socket_answers(stream),
        Err(e) if e.kind() == io::ErrorKind::ConnectionRefused => {
            drop(fs::remove_file(path));
            false
        },
        Err(_) => false,
    }
}

/// Ask a connected server whether it is alive.
#[cfg(unix)]
fn socket_answers(stream: interprocess::local_socket::Stream) -> bool {
    let mut sender: IpcSenderWithContext<ClientToServerMsg> = IpcSenderWithContext::new(stream);
    let _ = sender.send_client_msg(ClientToServerMsg::ConnStatus);
    let mut receiver: IpcReceiverWithContext<ServerToClientMsg> = sender.get_receiver();
    match receiver.recv_server_msg() {
        Some((ServerToClientMsg::Connected, _)) => true,
        None | Some((_, _)) => false,
    }
}

/// [`assert_socket`] for a socket outside `ZELLIJ_SOCK_DIR`, without the stale-socket cleanup.
#[cfg(unix)]
fn probe_socket(path: &std::path::Path) -> bool {
    crate::consts::ipc_connect(path)
        .map(socket_answers)
        .unwrap_or(false)
}

/// On Windows, reads the server PID from the marker file and checks whether
/// the process is still alive via `OpenProcess`. Cleans up stale marker files.
#[cfg(windows)]
fn assert_socket(name: &str) -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};

    let path = &*ZELLIJ_SOCK_DIR.join(name);
    let pid_str = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(_) => {
            drop(fs::remove_file(path));
            return false;
        },
    };
    let pid: u32 = match pid_str.trim().parse() {
        Ok(p) => p,
        Err(_) => {
            // Marker file exists but has no valid PID (e.g. empty from old version).
            // Treat as stale.
            drop(fs::remove_file(path));
            return false;
        },
    };
    let alive = unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            false
        } else {
            CloseHandle(handle);
            true
        }
    };
    if !alive {
        drop(fs::remove_file(path));
    }
    alive
}

#[cfg(not(any(unix, windows)))]
fn assert_socket(_name: &str) -> bool {
    true
}

/// Only unix resolves the socket directory from the environment, so there is never another
/// directory to scan elsewhere.
#[cfg(not(unix))]
fn probe_socket(_path: &std::path::Path) -> bool {
    false
}

pub fn print_sessions(
    mut sessions: Vec<(String, Duration, bool)>,
    no_formatting: bool,
    short: bool,
    reverse: bool,
) {
    // (session_name, timestamp, is_dead)
    let curr_session = envs::get_session_name().unwrap_or_else(|_| "".into());
    sessions.sort_by(|a, b| {
        if reverse {
            // sort by `Duration` ascending (newest would be first)
            a.1.cmp(&b.1)
        } else {
            b.1.cmp(&a.1)
        }
    });
    sessions
        .iter()
        .for_each(|(session_name, timestamp, is_dead)| {
            if short {
                // the name stays the first whitespace-separated field so `zellij ls -s` remains
                // cut/awk-parseable, but a resurrectable session is no longer indistinguishable
                // from a running one
                if *is_dead {
                    println!("{} (EXITED)", session_name);
                } else {
                    println!("{}", session_name);
                }
                return;
            }
            if no_formatting {
                let suffix = if curr_session == *session_name {
                    format!("(current)")
                } else if *is_dead {
                    format!("(EXITED - attach to resurrect)")
                } else {
                    String::new()
                };
                let timestamp = format!("[Created {} ago]", format_duration(*timestamp));
                println!("{} {} {}", session_name, timestamp, suffix);
            } else {
                let formatted_session_name = format!("\u{1b}[32;1m{}\u{1b}[m", session_name);
                let suffix = if curr_session == *session_name {
                    format!("(current)")
                } else if *is_dead {
                    format!("(\u{1b}[31;1mEXITED\u{1b}[m - attach to resurrect)")
                } else {
                    String::new()
                };
                let timestamp = format!(
                    "[Created \u{1b}[35;1m{}\u{1b}[m ago]",
                    format_duration(*timestamp)
                );
                println!("{} {} {}", formatted_session_name, timestamp, suffix);
            }
        })
}

pub fn print_sessions_with_index(sessions: Vec<String>) {
    let curr_session = envs::get_session_name().unwrap_or_else(|_| "".into());
    for (i, session) in sessions.iter().enumerate() {
        let suffix = if curr_session == *session {
            " (current)"
        } else {
            ""
        };
        println!("{}: {}{}", i, session, suffix);
    }
}

pub enum ActiveSession {
    None,
    One(String),
    Many,
}

pub fn get_active_session() -> ActiveSession {
    match get_sessions() {
        Ok(sessions) if sessions.is_empty() => ActiveSession::None,
        Ok(mut sessions) if sessions.len() == 1 => ActiveSession::One(sessions.pop().unwrap().0),
        Ok(_) => ActiveSession::Many,
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            process::exit(1);
        },
    }
}

/// Whether a kill waits for the server process to be gone, and for how long.
///
/// The default is to wait: "the kill was sent" is not a useful answer to callers who kill a session
/// in order to build another one in its place.
#[derive(Debug, Clone, Copy)]
pub struct KillWait {
    timeout: Option<Duration>,
}

impl KillWait {
    /// Build from the CLI's `--no-wait` / `--wait-timeout <secs>` pair.
    pub fn from_cli(no_wait: bool, wait_timeout_secs: u64) -> Self {
        KillWait {
            timeout: if no_wait {
                None
            } else {
                Some(Duration::from_secs(wait_timeout_secs))
            },
        }
    }

    /// Send the kill and return, without waiting for anything.
    pub fn none() -> Self {
        KillWait { timeout: None }
    }
}

/// How long a `--no-wait` kill still allows for the message itself to reach the server.
const KILL_SEND_TIMEOUT: Duration = Duration::from_millis(500);
const KILL_POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Kill the session's server. Returns `false` if it was still running when the wait ran out.
pub fn kill_session(name: &str, wait: KillWait) -> bool {
    kill_and_wait(name, wait)
}

/// Send `KillSession` and, unless waiting is switched off, block until the server is really gone.
///
/// The send itself awaits the server's `Exit` acknowledgement (or its socket closing), so even
/// `--no-wait` no longer returns before the server has read the message.
fn kill_and_wait(name: &str, wait: KillWait) -> bool {
    let send_budget = wait.timeout.unwrap_or(KILL_SEND_TIMEOUT);
    let deadline = std::time::Instant::now() + send_budget;
    // ask who is on the other end before killing them; afterwards there is nobody to ask
    let pid = server_pid(name);
    send_kill_session(name, send_budget);

    let Some(timeout) = wait.timeout else {
        return true;
    };
    while std::time::Instant::now() < deadline {
        if server_is_gone(name, pid) {
            return true;
        }
        std::thread::sleep(KILL_POLL_INTERVAL);
    }
    if server_is_gone(name, pid) {
        return true;
    }
    eprintln!(
        "session '{}': server still running after {}s",
        name,
        timeout.as_secs()
    );
    false
}

/// Deliver `KillSession` and wait for the server to acknowledge it.
///
/// Failures here are not reported: connecting to a socket that has already gone is the ordinary
/// outcome of racing another kill, and whether the server is gone is settled by polling, not by
/// this send.
fn send_kill_session(name: &str, budget: Duration) {
    let path = ZELLIJ_SOCK_DIR.join(name);
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(e) => {
            log::error!(
                "Failed to build a runtime to kill session {:?}: {}",
                name,
                e
            );
            return;
        },
    };
    if let Err(e) = runtime.block_on(async {
        tokio::time::timeout(budget, crate::ipc::async_send_kill_and_await(&path)).await
    }) {
        log::debug!("Session {:?} did not acknowledge the kill: {}", name, e);
    }
}

/// Whether the session's server is gone, as opposed to merely no longer answering.
///
/// A server that has been told to die stops answering immediately and then spends real time killing
/// pane shells, so "stopped answering" is the wrong question -- it is what made `zellij ls` report
/// a session as deleted while its server still held four shells. The server unlinks its socket only
/// once teardown is complete, so the file going away is the honest signal. Where the kernel would
/// name the peer (see [`server_pid`]), the process itself is checked and the socket is not
/// consulted at all.
#[cfg(unix)]
fn server_is_gone(name: &str, pid: Option<u32>) -> bool {
    if let Some(pid) = pid {
        return !process_is_alive(pid);
    }
    let path = ZELLIJ_SOCK_DIR.join(name);
    match crate::consts::ipc_connect(&path) {
        Ok(_) => false,
        Err(e)
            if e.kind() == io::ErrorKind::NotFound
                || e.kind() == io::ErrorKind::ConnectionRefused =>
        {
            // a refused connection on a file that is still there is an abandoned socket; take it
            // away so the absence check settles instead of spinning until the timeout
            drop(fs::remove_file(&path));
            !path.exists()
        },
        Err(_) => false,
    }
}

/// Windows records the server's PID beside the pipe, so "gone" is answerable directly.
#[cfg(not(unix))]
fn server_is_gone(name: &str, _pid: Option<u32>) -> bool {
    !assert_socket(name)
}

/// The PID of the process listening on this session's socket, where the kernel will say.
///
/// The server double-forks and writes no PID file, but a connected unix socket carries its peer's
/// credentials, which is all the kill path needs. Platforms that decline to report a PID leave the
/// caller watching the socket file instead.
#[cfg(unix)]
fn server_pid(name: &str) -> Option<u32> {
    use std::os::unix::io::AsRawFd;
    use std::os::unix::net::UnixStream;
    let path = ZELLIJ_SOCK_DIR.join(name);
    let stream = UnixStream::connect(&path).ok()?;
    peer_pid(stream.as_raw_fd())
}

#[cfg(not(unix))]
fn server_pid(_name: &str) -> Option<u32> {
    None
}

/// `std::os::unix::net::UnixStream::peer_cred` is still unstable, so ask the socket directly.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn peer_pid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut creds: libc::ucred = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of_val(&creds) as libc::socklen_t;
    let got = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut creds as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (got == 0 && creds.pid > 0).then_some(creds.pid as u32)
}

#[cfg(target_vendor = "apple")]
fn peer_pid(fd: std::os::unix::io::RawFd) -> Option<u32> {
    let mut pid: libc::pid_t = 0;
    let mut len = std::mem::size_of_val(&pid) as libc::socklen_t;
    let got = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };
    (got == 0 && pid > 0).then_some(pid as u32)
}

/// Everything else falls back to watching the socket file.
#[cfg(all(
    unix,
    not(any(target_os = "linux", target_os = "android", target_vendor = "apple"))
))]
fn peer_pid(_fd: std::os::unix::io::RawFd) -> Option<u32> {
    None
}

/// Signal 0 asks the kernel whether the process exists without touching it. The server is
/// daemonized, so it is never a zombie of ours and never lingers in the table after it exits.
#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

/// What a delete found to remove, and whether the server it belonged to went away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeletedSession {
    /// `false` if the server was still running when the wait ran out.
    pub killed: bool,
    /// `false` if there was no session_info folder to remove - the session was already gone.
    pub found: bool,
}

/// Delete the session, and say what was there to delete.
///
/// Whether finding nothing is a failure is the CALLER's question, not this function's: to
/// `delete-session` it is a name that does not exist, and to `session down` it is the state that
/// was asked for. Both need the same removal, so only the verdict is left to them.
pub fn delete_session_reporting(
    name: &str,
    force: bool,
    snapshot_settings: &SnapshotSettings,
    wait: KillWait,
) -> DeletedSession {
    let killed = if force {
        kill_and_wait(name, wait)
    } else {
        true
    };
    let mut found = true;
    if let Err(e) = remove_session_info_folder(name, snapshot_settings) {
        if e.kind() == std::io::ErrorKind::NotFound {
            found = false;
        } else {
            log::error!("Failed to remove session {:?}: {:?}", name, e);
        }
    } else {
        println!("Session: {:?} successfully deleted.", name);
    }
    DeletedSession { killed, found }
}

/// Delete the session. Returns `false` if the server was still running when the wait ran out.
pub fn delete_session(
    name: &str,
    force: bool,
    snapshot_settings: &SnapshotSettings,
    wait: KillWait,
) -> bool {
    let deleted = delete_session_reporting(name, force, snapshot_settings, wait);
    if !deleted.found {
        eprintln!("Session: {:?} not found.", name);
        process::exit(2);
    }
    deleted.killed
}

const DELETE_SESSION_POLL_INTERVAL: Duration = Duration::from_millis(50);
/// How long to keep sweeping the session_info folder after the server socket has gone away.
const DELETE_SESSION_SWEEP_DURATION: Duration = Duration::from_millis(500);

/// Archive the session's shape, remove the session_info folder, then keep sweeping it briefly in
/// case the exiting server writes its snapshot back after the socket has already gone.
///
/// Archiving first is what turns the destructive path into the capturing one: deleting a session is
/// exactly the operation that later motivates restoring it. The server usually archives the same
/// shape on its way out, and the archive drops a copy identical to the newest one, so an ordinary
/// `delete-session --force` still leaves one snapshot rather than two.
fn remove_session_info_folder(
    name: &str,
    snapshot_settings: &SnapshotSettings,
) -> std::io::Result<()> {
    if let Err(e) = archive_session_info(name, SnapshotReason::Delete, snapshot_settings) {
        log::error!(
            "Failed to archive session {:?} before deleting it: {}",
            name,
            e
        );
    }
    let folder = session_info_folder_for_session(name);
    std::fs::remove_dir_all(&folder)?;
    let deadline = std::time::Instant::now() + DELETE_SESSION_SWEEP_DURATION;
    while std::time::Instant::now() < deadline {
        std::thread::sleep(DELETE_SESSION_POLL_INTERVAL);
        if folder.exists() {
            let _ = std::fs::remove_dir_all(&folder);
        }
    }
    Ok(())
}

pub fn list_sessions(no_formatting: bool, short: bool, reverse: bool) {
    let exit_code = match get_sessions() {
        Ok(running_sessions) => {
            let resurrectable_sessions = get_resurrectable_sessions();
            let mut all_sessions: HashMap<String, (Duration, bool)> = resurrectable_sessions
                .iter()
                .map(|(name, timestamp)| (name.clone(), (timestamp.clone(), true)))
                .collect();
            for (session_name, duration) in running_sessions {
                all_sessions.insert(session_name.clone(), (duration, false));
            }
            if all_sessions.is_empty() {
                eprintln!("No active zellij sessions found.");
                print_searched_socket_dirs();
                print_other_socket_dir_warning();
                1
            } else {
                print_sessions(
                    all_sessions
                        .iter()
                        .map(|(name, (timestamp, is_dead))| {
                            (name.clone(), timestamp.clone(), *is_dead)
                        })
                        .collect(),
                    no_formatting,
                    short,
                    reverse,
                );
                print_other_socket_dir_warning();
                0
            }
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
            1
        },
    };
    process::exit(exit_code);
}

#[derive(Debug, Clone)]
pub enum SessionNameMatch {
    AmbiguousPrefix(Vec<String>),
    UniquePrefix(String),
    Exact(String),
    None,
}

pub fn match_session_name(prefix: &str) -> Result<SessionNameMatch, io::ErrorKind> {
    let sessions = get_sessions()?;

    let filtered_sessions: Vec<_> = sessions
        .iter()
        .filter(|s| s.0.starts_with(prefix))
        .collect();

    if filtered_sessions.iter().any(|s| s.0 == prefix) {
        return Ok(SessionNameMatch::Exact(prefix.to_string()));
    }

    Ok({
        match &filtered_sessions[..] {
            [] => SessionNameMatch::None,
            [s] => SessionNameMatch::UniquePrefix(s.0.to_string()),
            _ => SessionNameMatch::AmbiguousPrefix(
                filtered_sessions.into_iter().map(|s| s.0.clone()).collect(),
            ),
        }
    })
}

pub fn session_exists(name: &str) -> Result<bool, io::ErrorKind> {
    match match_session_name(name) {
        Ok(SessionNameMatch::Exact(_)) => Ok(true),
        Ok(_) => Ok(false),
        Err(e) => Err(e),
    }
}

// if the session is resurrecable, the returned layout is the one to be used to resurrect it
pub fn resurrection_layout(session_name_to_resurrect: &str) -> Result<Option<Layout>, String> {
    let layout_file_name = session_layout_cache_file_name(&session_name_to_resurrect);
    let raw_layout = match std::fs::read_to_string(&layout_file_name) {
        Ok(raw_layout) => raw_layout,
        Err(_e) => {
            return Ok(None);
        },
    };
    match Layout::from_kdl(
        &raw_layout,
        Some(layout_file_name.display().to_string()),
        None,
        None,
    ) {
        Ok(layout) => Ok(Some(layout)),
        Err(e) => {
            log::error!(
                "Failed to parse resurrection layout file {}: {}",
                layout_file_name.display(),
                e
            );
            return Err(format!(
                "Failed to parse resurrection layout file {}: {}.",
                layout_file_name.display(),
                e
            ));
        },
    }
}

pub fn assert_session(name: &str) {
    match session_exists(name) {
        Ok(result) => {
            if result {
                return;
            } else {
                println!("No session named {:?} found.", name);
                if let Some(sugg) = get_sessions()
                    .unwrap()
                    .iter()
                    .map(|s| s.0.clone())
                    .collect::<Vec<_>>()
                    .suggest(name)
                {
                    println!("  help: Did you mean `{}`?", sugg);
                }
            }
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
        },
    };
    process::exit(1);
}

pub fn assert_dead_session(name: &str, force: bool) {
    match session_exists(name) {
        Ok(exists) => {
            if exists && !force {
                println!(
                    "A session by the name {:?} exists and is active, use --force to delete it.",
                    name
                )
            } else if exists && force {
                println!("A session by the name {:?} exists and is active, but will be force killed and deleted.", name);
                return;
            } else {
                return;
            }
        },
        Err(e) => {
            eprintln!("Error occurred: {:?}", e);
        },
    };
    process::exit(1);
}

pub fn validate_session_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err(
            "Session name cannot be empty. Please provide a specific session name.".to_string(),
        );
    }
    if name == "." || name == ".." {
        return Err(format!("Invalid session name: \"{}\".", name));
    }
    if name.contains('/') {
        return Err("Session name cannot contain '/'.".to_string());
    }
    Ok(())
}

/// Drop a dead session's resurrection snapshot, so the name is free and the next session by that
/// name is built from the layout rather than from the shape it happened to have when it died.
pub fn discard_resurrection_snapshot(name: &str) {
    if let Err(e) = std::fs::remove_dir_all(session_info_folder_for_session(name)) {
        if e.kind() != std::io::ErrorKind::NotFound {
            log::error!(
                "Failed to discard the resurrection snapshot for session {:?}: {:?}",
                name,
                e
            );
        }
    }
}

pub fn assert_session_ne(name: &str) {
    if let Err(e) = validate_session_name(name) {
        eprintln!("{}", e);
        process::exit(1);
    }

    match session_exists(name) {
        Ok(result) if !result => {
            let resurrectable_sessions = get_resurrectable_session_names();
            if resurrectable_sessions.iter().find(|s| s == &name).is_some() {
                println!("Session with name {:?} already exists, but is dead. Use the attach command to resurrect it or, the delete-session command to kill it or specify a different name.", name);
            } else {
                return
            }
        }
        Ok(_) => println!("Session with name {:?} already exists. Use attach command to connect to it or specify a different name.", name),
        Err(e) => eprintln!("Error occurred: {:?}", e),
    };
    process::exit(1);
}

pub fn generate_unique_session_name() -> Option<String> {
    let sessions = get_sessions().map(|sessions| {
        sessions
            .iter()
            .map(|s| s.0.clone())
            .collect::<Vec<String>>()
    });
    let dead_sessions = get_resurrectable_session_names();
    let Ok(sessions) = sessions else {
        eprintln!("Failed to list existing sessions: {:?}", sessions);
        return None;
    };

    let name = get_name_generator()
        .take(1000)
        .find(|name| !sessions.contains(name) && !dead_sessions.contains(name));

    if let Some(name) = name {
        return Some(name);
    } else {
        return None;
    }
}

/// Create a new random name generator
///
/// Used to provide a memorable handle for a session when users don't specify a session name when the session is
/// created.
///
/// Uses the list of adjectives and nouns defined below, with the intention of avoiding unfortunate
/// and offensive combinations. Care should be taken when adding or removing to either list due to the birthday paradox/
/// hash collisions, e.g. with 4096 unique names, the likelihood of a collision in 10 session names is 1%.
pub fn get_name_generator() -> impl Iterator<Item = String> {
    names::Generator::new(&ADJECTIVES, &NOUNS, names::Name::Plain)
}

/// Generates a random human-readable name using curated adjectives and nouns.
/// Returns a single name in the format: AdjectiveNoun (e.g., "BraveRustacean")
pub fn generate_random_name() -> String {
    get_name_generator().next().unwrap()
}

const ADJECTIVES: &[&'static str] = &[
    "adamant",
    "adept",
    "adventurous",
    "arcadian",
    "auspicious",
    "awesome",
    "blossoming",
    "brave",
    "charming",
    "chatty",
    "circular",
    "considerate",
    "cubic",
    "curious",
    "delighted",
    "didactic",
    "diligent",
    "effulgent",
    "erudite",
    "excellent",
    "exquisite",
    "fabulous",
    "fascinating",
    "friendly",
    "glowing",
    "gracious",
    "gregarious",
    "hopeful",
    "implacable",
    "inventive",
    "joyous",
    "judicious",
    "jumping",
    "kind",
    "likable",
    "loyal",
    "lucky",
    "marvellous",
    "mellifluous",
    "nautical",
    "oblong",
    "outstanding",
    "polished",
    "polite",
    "profound",
    "quadratic",
    "quiet",
    "rectangular",
    "remarkable",
    "rusty",
    "sensible",
    "sincere",
    "sparkling",
    "splendid",
    "stellar",
    "tenacious",
    "tremendous",
    "triangular",
    "undulating",
    "unflappable",
    "unique",
    "verdant",
    "vitreous",
    "wise",
    "zippy",
];

const NOUNS: &[&'static str] = &[
    "aardvark",
    "accordion",
    "apple",
    "apricot",
    "bee",
    "brachiosaur",
    "cactus",
    "capsicum",
    "clarinet",
    "cowbell",
    "crab",
    "cuckoo",
    "cymbal",
    "diplodocus",
    "donkey",
    "drum",
    "duck",
    "echidna",
    "elephant",
    "foxglove",
    "galaxy",
    "glockenspiel",
    "goose",
    "hill",
    "horse",
    "iguanadon",
    "jellyfish",
    "kangaroo",
    "lake",
    "lemon",
    "lemur",
    "magpie",
    "megalodon",
    "mountain",
    "mouse",
    "muskrat",
    "newt",
    "oboe",
    "ocelot",
    "orange",
    "panda",
    "peach",
    "pepper",
    "petunia",
    "pheasant",
    "piano",
    "pigeon",
    "platypus",
    "quasar",
    "rhinoceros",
    "river",
    "rustacean",
    "salamander",
    "sitar",
    "stegosaurus",
    "tambourine",
    "tiger",
    "tomato",
    "triceratops",
    "ukulele",
    "viola",
    "weasel",
    "xylophone",
    "yak",
    "zebra",
];
