//! Reading a resume value out of the processes running in a pane.
//!
//! `resurrect_command_hints` (see `zellij_utils::resurrect_command_hints`) says which environment
//! variable carries a tool's session id. Finding it means looking at processes zellij did not
//! start: the pane's shell is zellij's child, but the tool is the shell's child, and the tool is
//! the process that exported the variable.
//!
//! So this module does two things - walk down from the pane's pid, and read one variable out of a
//! process's environment - and both are per-platform:
//!
//! - **Linux** reads `/proc/<pid>/environ`, which the kernel exposes for a process of the same
//!   user. This is the reference implementation; it is what the shape of the rest is built around.
//! - **macOS** has no `/proc`, so it asks `sysctl(KERN_PROCARGS2)`, the same call `ps -E` makes.
//!   The kernel serves it for a process of the same uid, which is what a pane's children are - and
//!   refuses for anything else, which is the correct answer to the question anyway.
//! - **Everything else** finds nothing and logs it. The feature degrades into recording the
//!   command unchanged, which is what zellij did before the feature existed.
//!
//! From nkmk.31 it also finds an agent's OWN session - claude's per-process record, the rollout
//! file a codex holds open - through [`ProcessTree::own_session`], which says why when it cannot.
//!
//! Every failure here is a debug log and a `None`. Serialization runs on a timer and its job is to
//! preserve what it can - a hint that cannot be resolved must cost the snapshot nothing.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
pub use zellij_utils::resurrect_command_hints::SessionSource;

/// A snapshot of parent -> children taken once per serialization pass.
///
/// Taken once because reading the whole process table is the expensive part and every pane in the
/// session asks the same question of it. Children are kept in ascending pid order so that "the
/// first descendant that has the variable" is a stable answer rather than whatever order the
/// process table happened to be in.
#[derive(Debug, Default)]
pub struct ProcessTree {
    children: HashMap<u32, Vec<u32>>,
}

impl ProcessTree {
    /// Reads the process table. Cheap enough to do once per serialization, too expensive per pane.
    pub fn read() -> Self {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::nothing(),
        );
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for (pid, process) in system.processes() {
            if let Some(parent) = process.parent() {
                children
                    .entry(parent.as_u32())
                    .or_default()
                    .push(pid.as_u32());
            }
        }
        for pids in children.values_mut() {
            pids.sort_unstable();
        }
        ProcessTree { children }
    }

    #[cfg(test)]
    pub fn from_children(children: HashMap<u32, Vec<u32>>) -> Self {
        ProcessTree { children }
    }

    /// `root` and its descendants, breadth first, `root` included.
    ///
    /// `root` is included because a pane whose shell itself carries the variable is a pane where
    /// the variable is the right answer - a shell only has it if something in its own lineage
    /// exported it.
    pub fn descendants(&self, root: u32) -> Vec<u32> {
        let mut found = vec![root];
        let mut seen: Vec<u32> = vec![root];
        let mut next = 0;
        while next < found.len() {
            let pid = found[next];
            next += 1;
            for child in self.children.get(&pid).into_iter().flatten() {
                // a process table read from a live system can disagree with itself; a cycle here
                // would be a hang, so it costs nothing to refuse one
                if seen.contains(child) {
                    continue;
                }
                seen.push(*child);
                found.push(*child);
            }
        }
        found
    }

    /// The value of `var` in the first of `root`'s processes that has it, or `None`.
    pub fn find_env(&self, root: u32, var: &str) -> Option<String> {
        for pid in self.descendants(root) {
            if let Some(value) = read_process_env(pid, var) {
                return Some(value);
            }
        }
        log::debug!(
            "resurrect_command_hints: no process under pid {} carries {}",
            root,
            var
        );
        None
    }

    /// The allowlisted variables carried by `root` or its descendants.
    ///
    /// One walk, one read per process, every name resolved from the same blob - resolving each
    /// name separately would read the same environments once per name. The nearest process to the
    /// root wins, so a tool started inside the pane's shell shadows the shell for a name they both
    /// export.
    pub fn find_all_envs(&self, root: u32, vars: &[String]) -> BTreeMap<String, String> {
        let mut found = BTreeMap::new();
        if vars.is_empty() {
            return found;
        }
        for pid in self.descendants(root) {
            if found.len() == vars.len() {
                break;
            }
            let environ = match read_process_environ(pid) {
                Some(environ) => environ,
                None => continue,
            };
            for var in vars {
                if found.contains_key(var) {
                    continue;
                }
                if let Some(value) = env_from_environ(&environ, var) {
                    found.insert(var.clone(), value);
                }
            }
        }
        found
    }
}

/// The `sessionId` in Claude Code's record for `pid`, or `None` when there is no record, it does
/// not parse, it names another pid, it holds no session, or it is a dead claude's record whose pid
/// has since been reused.
pub fn claude_session_record(sessions_dir: &Path, pid: u32) -> Option<String> {
    let record = std::fs::read(sessions_dir.join(format!("{}.json", pid))).ok()?;
    let Ok(record) = serde_json::from_slice::<serde_json::Value>(&record) else {
        log::debug!(
            "resurrect_commands: unreadable claude session record for {}",
            pid
        );
        return None;
    };
    if record.get("pid").and_then(|p| p.as_u64()) != Some(pid as u64) {
        return None;
    }
    if !record_is_for_this_process(&record, pid) {
        log::debug!(
            "resurrect_commands: claude session record for {} was written by an earlier process \
             with that pid",
            pid
        );
        return None;
    }
    record
        .get("sessionId")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty())
        .map(|id| id.to_owned())
}

/// Whether a claude record was written by the process that has its pid now.
///
/// A claude that crashes leaves its record behind, and the pid is reused. Claude Code writes
/// `procStart` beside the pid: on Linux it is field 22 of `/proc/<pid>/stat`, the start time in
/// clock ticks since boot, and `pidDomain` starts with `linux:`. A record that carries both and
/// disagrees with the live process is stale. Anything this cannot check - another platform, a
/// record without the fields, a process whose stat cannot be read - is believed, as before.
fn record_is_for_this_process(record: &serde_json::Value, pid: u32) -> bool {
    let is_linux_domain = record
        .get("pidDomain")
        .and_then(|domain| domain.as_str())
        .map_or(false, |domain| domain.starts_with("linux:"));
    let Some(recorded_start) = record.get("procStart").and_then(|start| start.as_str()) else {
        return true;
    };
    if !is_linux_domain {
        return true;
    }
    match process_start_ticks(pid) {
        Some(live_start) => live_start == recorded_start,
        None => true,
    }
}

/// Field 22 of `/proc/<pid>/stat`, the process start time, as the string Claude Code records.
#[cfg(target_os = "linux")]
fn process_start_ticks(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{}/stat", pid)).ok()?;
    start_ticks_from_stat(&stat)
}

#[cfg(not(target_os = "linux"))]
fn process_start_ticks(_pid: u32) -> Option<String> {
    None
}

/// The start time out of a `/proc/<pid>/stat` line. The command name is field 2, in parentheses,
/// and may itself hold spaces and parentheses, so the fields are counted from the LAST `)`.
fn start_ticks_from_stat(stat: &str) -> Option<String> {
    let after_comm = &stat[stat.rfind(')')? + 1..];
    // after the `)` come field 3 (state) onward; field 22 is the 20th of them
    after_comm
        .split_whitespace()
        .nth(19)
        .map(|field| field.to_owned())
}

/// Where Claude Code keeps its per-process session records: `$CLAUDE_CONFIG_DIR/sessions`, or
/// `~/.claude/sessions`. Read from the server's own environment, which is the user's.
pub fn claude_sessions_dir() -> Option<PathBuf> {
    claude_sessions_dir_for(
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("HOME"),
    )
}

/// [`claude_sessions_dir`] for a given `CLAUDE_CONFIG_DIR` and `HOME`, which need not be the
/// server's: a claude whose shell rc exports `CLAUDE_CONFIG_DIR` writes its records where the
/// server, started by launchd or systemd before any rc file ran, would never look.
pub fn claude_sessions_dir_for(
    config_dir: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let config_dir = match config_dir {
        Some(dir) if !dir.is_empty() => PathBuf::from(dir),
        _ => match home {
            Some(home) if !home.is_empty() => PathBuf::from(home).join(".claude"),
            _ => return None,
        },
    };
    Some(config_dir.join("sessions"))
}

/// The session id in a codex rollout file's name: `rollout-<timestamp>-<uuid>.jsonl`.
///
/// codex keeps the file open for as long as the session is live, so the file a codex process
/// holds is the session it is in. `None` for any other file.
pub fn codex_session_from_path(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_prefix("rollout-")?.strip_suffix(".jsonl")?;
    let id = stem.get(stem.len().checked_sub(36)?..)?;
    let is_uuid = id.char_indices().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => c == '-',
        _ => c.is_ascii_hexdigit(),
    });
    is_uuid.then(|| id.to_owned())
}

/// How a found session is re-read on a later tick without another walk of the process tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiveSession {
    /// claude's record for `pid` in `dir`. `/clear` rewrites it in place.
    ClaudeRecord { pid: u32, dir: PathBuf },
    /// the rollout file codex process `pid` holds. `/new` opens another one.
    CodexRollout { pid: u32 },
}

impl LiveSession {
    /// The session this handle names NOW, or `None` once its process has moved on or exited.
    pub fn reread<O>(&self, open_files: O) -> Option<String>
    where
        O: Fn(u32) -> Option<Vec<PathBuf>>,
    {
        match self {
            LiveSession::ClaudeRecord { pid, dir } => claude_session_record(dir, *pid),
            LiveSession::CodexRollout { pid } => open_files(*pid)?
                .iter()
                .find_map(|path| codex_session_from_path(path)),
        }
    }

    pub fn source(&self) -> SessionSource {
        match self {
            LiveSession::ClaudeRecord { .. } => SessionSource::Record,
            LiveSession::CodexRollout { .. } => SessionSource::OpenFile,
        }
    }
}

/// A pane's own session: its id, and how to find it again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnSession {
    pub id: String,
    pub live: LiveSession,
}

/// Why a pane's own session was not found. Its `Display` is the log line's reason, so the
/// server log answers "why did this pane come back with a picker".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionMiss {
    /// No directory to look in: neither the server nor the agent has `HOME`.
    NoSessionsDir,
    /// None of the processes under the pane has a claude record.
    NoClaudeRecord {
        root: u32,
        searched: usize,
        dirs: Vec<PathBuf>,
    },
    /// None of the processes under the pane holds a codex rollout file open. A codex that has
    /// not been sent a message yet has none: it creates the file on the first one.
    NoOpenRollout { root: u32, searched: usize },
    /// This platform cannot list another process's open files.
    OpenFilesUnsupported,
    /// The agent keeps no per-process session handle to read.
    NoPerProcessHandle(&'static str),
}

impl std::fmt::Display for SessionMiss {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionMiss::NoSessionsDir => {
                write!(f, "no claude config directory: HOME is not set")
            },
            SessionMiss::NoClaudeRecord {
                root,
                searched,
                dirs,
            } => write!(
                f,
                "none of the {} processes under pid {} has a session record in {:?} (claude \
                 writes it a few seconds after it starts)",
                searched, root, dirs
            ),
            SessionMiss::NoOpenRollout { root, searched } => write!(
                f,
                "none of the {} processes under pid {} holds a rollout file open (codex \
                 creates it on the first message)",
                searched, root
            ),
            SessionMiss::OpenFilesUnsupported => {
                write!(f, "this platform cannot list another process's open files")
            },
            SessionMiss::NoPerProcessHandle(kind) => write!(
                f,
                "{} keeps no per-process session record or open session file",
                kind
            ),
        }
    }
}

impl ProcessTree {
    /// The session the agent of kind `kind` under `root` holds NOW.
    ///
    /// The pid rule: walk breadth first from the pane's own child, `root` included, and take the
    /// NEAREST process that answers. A shell pane's agent is the shell's child; a command pane's
    /// agent is `root` itself; a wrapper (`node` running the npm `codex`) puts the real process
    /// one level further down; and an agent the pane's agent started - a subagent - sits deeper,
    /// so the nearer one is the pane's.
    ///
    /// - **claude**: its `sessions/<pid>.json`. The server's own config directory first, then the
    ///   one each process under the pane names in its own `CLAUDE_CONFIG_DIR` or `HOME`.
    /// - **codex**: the `rollout-*-<uuid>.jsonl` a process holds open.
    /// - **opencode, pi**: nothing. Both write their sessions through a shared database or an
    ///   append that closes the file at once, so no process holds a handle that names its session.
    ///
    /// `open_files` and `env_of` are the seams over the platform, so the walk is testable with a
    /// fake process table.
    pub fn own_session<O, E>(
        &self,
        root: u32,
        kind: &str,
        claude_dir: Option<&Path>,
        open_files: O,
        env_of: E,
    ) -> Result<OwnSession, SessionMiss>
    where
        O: Fn(u32) -> Option<Vec<PathBuf>>,
        E: Fn(u32, &str) -> Option<String>,
    {
        let pids = self.descendants(root);
        match kind {
            "claude" => {
                let mut dirs: Vec<PathBuf> =
                    claude_dir.map(Path::to_path_buf).into_iter().collect();
                for pid in &pids {
                    if let Some(dir) = claude_dir {
                        if let Some(id) = claude_session_record(dir, *pid) {
                            return Ok(OwnSession {
                                id,
                                live: LiveSession::ClaudeRecord {
                                    pid: *pid,
                                    dir: dir.to_path_buf(),
                                },
                            });
                        }
                    }
                }
                // the server's directory had nothing: ask each process where IT keeps them
                for pid in &pids {
                    let Some(dir) = claude_sessions_dir_for(
                        env_of(*pid, "CLAUDE_CONFIG_DIR").map(Into::into),
                        env_of(*pid, "HOME").map(Into::into),
                    ) else {
                        continue;
                    };
                    if Some(dir.as_path()) == claude_dir {
                        continue;
                    }
                    if let Some(id) = claude_session_record(&dir, *pid) {
                        return Ok(OwnSession {
                            id,
                            live: LiveSession::ClaudeRecord { pid: *pid, dir },
                        });
                    }
                    if !dirs.contains(&dir) {
                        dirs.push(dir);
                    }
                }
                if dirs.is_empty() {
                    return Err(SessionMiss::NoSessionsDir);
                }
                Err(SessionMiss::NoClaudeRecord {
                    root,
                    searched: pids.len(),
                    dirs,
                })
            },
            "codex" => {
                let mut listed_any = false;
                for pid in &pids {
                    let Some(files) = open_files(*pid) else {
                        continue;
                    };
                    listed_any = true;
                    if let Some(id) = files.iter().find_map(|path| codex_session_from_path(path)) {
                        return Ok(OwnSession {
                            id,
                            live: LiveSession::CodexRollout { pid: *pid },
                        });
                    }
                }
                if !listed_any && !OPEN_FILES_SUPPORTED {
                    return Err(SessionMiss::OpenFilesUnsupported);
                }
                Err(SessionMiss::NoOpenRollout {
                    root,
                    searched: pids.len(),
                })
            },
            "opencode" => Err(SessionMiss::NoPerProcessHandle("opencode")),
            "pi" => Err(SessionMiss::NoPerProcessHandle("pi")),
            _ => Err(SessionMiss::NoPerProcessHandle("this agent")),
        }
    }
}

/// Whether [`open_files_of`] can answer on this platform.
const OPEN_FILES_SUPPORTED: bool = cfg!(any(target_os = "linux", target_os = "macos"));

/// The paths of the regular files `pid` holds open, or `None` when they cannot be listed.
///
/// Linux reads the `/proc/<pid>/fd` links. macOS asks libproc - `PROC_PIDLISTFDS`, then
/// `PROC_PIDFDVNODEPATHINFO` per vnode - the calls `lsof` itself is built on, made in-process: no
/// subprocess, no half-second `lsof` start-up, and no privacy prompt for a process of the same
/// user, which is what a pane's children are.
#[cfg(target_os = "linux")]
pub fn open_files_of(pid: u32) -> Option<Vec<PathBuf>> {
    let entries = std::fs::read_dir(format!("/proc/{}/fd", pid)).ok()?;
    Some(
        entries
            .filter_map(|entry| std::fs::read_link(entry.ok()?.path()).ok())
            .filter(|path| path.is_absolute())
            .collect(),
    )
}

#[cfg(target_os = "macos")]
pub fn open_files_of(pid: u32) -> Option<Vec<PathBuf>> {
    use std::ffi::CStr;
    use std::os::raw::{c_int, c_void};

    // `struct proc_fileinfo` and `struct vnode_fdinfowithpath` from <sys/proc_info.h>; libc has
    // `vnode_info_path` but not the wrapper, nor its flavor constant
    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: libc::off_t,
        fi_type: i32,
        fi_guardflags: u32,
    }
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: libc::vnode_info_path,
    }
    const PROC_PIDFDVNODEPATHINFO: c_int = 2;

    let fd_info_size = std::mem::size_of::<libc::proc_fdinfo>();
    let pid = pid as c_int;
    let needed =
        unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
    if needed <= 0 {
        log::debug!(
            "resurrect_commands: cannot list the open files of pid {}",
            pid
        );
        return None;
    }
    // room for a few files opened between the two calls
    let capacity = needed as usize / fd_info_size + 16;
    let mut fds: Vec<libc::proc_fdinfo> = Vec::with_capacity(capacity);
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDLISTFDS,
            0,
            fds.as_mut_ptr() as *mut c_void,
            (capacity * fd_info_size) as c_int,
        )
    };
    if written <= 0 {
        return None;
    }
    unsafe { fds.set_len(written as usize / fd_info_size) };
    let mut paths = Vec::new();
    for fd in fds
        .iter()
        .filter(|fd| fd.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
    {
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<VnodeFdInfoWithPath>() as c_int;
        let read = unsafe {
            libc::proc_pidfdinfo(
                pid,
                fd.proc_fd,
                PROC_PIDFDVNODEPATHINFO,
                &mut info as *mut VnodeFdInfoWithPath as *mut c_void,
                size,
            )
        };
        if read != size {
            continue;
        }
        let path = unsafe { CStr::from_ptr(info.pvip.vip_path.as_ptr() as *const libc::c_char) };
        if let Ok(path) = path.to_str() {
            if !path.is_empty() {
                paths.push(PathBuf::from(path));
            }
        }
    }
    Some(paths)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn open_files_of(_pid: u32) -> Option<Vec<PathBuf>> {
    None
}

/// One variable out of one process's environment, for [`ProcessTree::own_session`]'s seam.
pub fn process_env(pid: u32, var: &str) -> Option<String> {
    read_process_env(pid, var)
}

/// One process's environment in the form this platform serves it, ready for `env_from_environ`.
#[cfg(target_os = "linux")]
fn read_process_environ(pid: u32) -> Option<Vec<u8>> {
    std::fs::read(format!("/proc/{}/environ", pid)).ok()
}

#[cfg(target_os = "macos")]
fn read_process_environ(pid: u32) -> Option<Vec<u8>> {
    read_procargs2(pid)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_process_environ(_pid: u32) -> Option<Vec<u8>> {
    None
}

/// One variable out of the blob `read_process_environ` returns on this platform.
#[cfg(target_os = "linux")]
fn env_from_environ(environ: &[u8], var: &str) -> Option<String> {
    env_from_nul_list(environ, var)
}

#[cfg(target_os = "macos")]
fn env_from_environ(environ: &[u8], var: &str) -> Option<String> {
    env_from_procargs2(environ, var)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn env_from_environ(_environ: &[u8], _var: &str) -> Option<String> {
    None
}

/// One variable out of one process's environment, or `None` if it is not there or not readable.
#[cfg(target_os = "linux")]
fn read_process_env(pid: u32, var: &str) -> Option<String> {
    match std::fs::read(format!("/proc/{}/environ", pid)) {
        Ok(environ) => env_from_nul_list(&environ, var),
        Err(e) => {
            log::debug!(
                "resurrect_command_hints: cannot read the environment of pid {}: {}",
                pid,
                e
            );
            None
        },
    }
}

#[cfg(target_os = "macos")]
fn read_process_env(pid: u32, var: &str) -> Option<String> {
    read_procargs2(pid).and_then(|procargs| env_from_procargs2(&procargs, var))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn read_process_env(pid: u32, var: &str) -> Option<String> {
    log::debug!(
        "resurrect_command_hints: reading {} from pid {} is not implemented on this platform, \
         recording the command unchanged",
        var,
        pid
    );
    None
}

/// The raw `KERN_PROCARGS2` blob for a process.
///
/// `KERN_ARGMAX` first because the blob has no length the caller can ask for - `sysctl` will fill
/// whatever buffer it is given and the only safe size is the maximum the kernel will ever produce.
#[cfg(target_os = "macos")]
fn read_procargs2(pid: u32) -> Option<Vec<u8>> {
    let mut argmax: libc::c_int = 0;
    let mut argmax_size = std::mem::size_of::<libc::c_int>();
    let mut argmax_mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    let read_argmax = unsafe {
        libc::sysctl(
            argmax_mib.as_mut_ptr(),
            argmax_mib.len() as _,
            &mut argmax as *mut libc::c_int as *mut libc::c_void,
            &mut argmax_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read_argmax != 0 || argmax <= 0 {
        log::debug!("resurrect_command_hints: KERN_ARGMAX is unavailable");
        return None;
    }

    let mut buffer: Vec<u8> = vec![0; argmax as usize];
    let mut buffer_size = buffer.len();
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid as libc::c_int];
    let read_procargs = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as _,
            buffer.as_mut_ptr() as *mut libc::c_void,
            &mut buffer_size,
            std::ptr::null_mut(),
            0,
        )
    };
    if read_procargs != 0 {
        // the ordinary case, not a fault: the kernel refuses a process of another user, and a
        // process that exited between the table read and now is simply gone
        log::debug!(
            "resurrect_command_hints: cannot read the environment of pid {} (KERN_PROCARGS2 \
             refused)",
            pid
        );
        return None;
    }
    buffer.truncate(buffer_size);
    Some(buffer)
}

/// One variable out of a NUL-separated `KEY=VALUE` list, the form `/proc/<pid>/environ` takes.
#[cfg(any(target_os = "linux", test))]
fn env_from_nul_list(environ: &[u8], var: &str) -> Option<String> {
    let prefix = format!("{}=", var);
    environ
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .find_map(|entry| entry.strip_prefix(&prefix).map(|value| value.to_owned()))
}

/// One variable out of a `KERN_PROCARGS2` blob, whose layout is:
///
/// ```text
/// argc: i32 | exec_path\0 | \0 padding | argv[0]\0 .. argv[argc-1]\0 | KEY=VALUE\0 .. | \0
/// ```
///
/// The environment cannot be found by scanning for a `=`, because an argument may contain one -
/// the argument vector has to be walked past, which is what `argc` is there for.
#[cfg(any(target_os = "macos", test))]
fn env_from_procargs2(procargs: &[u8], var: &str) -> Option<String> {
    let argc_bytes: [u8; 4] = procargs.get(..4)?.try_into().ok()?;
    let argc = i32::from_ne_bytes(argc_bytes);
    if argc < 0 {
        return None;
    }
    let mut rest = &procargs[4..];

    fn skip_string(rest: &mut &[u8]) -> Option<()> {
        let end = rest.iter().position(|byte| *byte == 0)?;
        *rest = &rest[end + 1..];
        Some(())
    }

    // the executable path, then the padding that aligns what follows it
    skip_string(&mut rest)?;
    while rest.first() == Some(&0) {
        rest = &rest[1..];
    }
    for _ in 0..argc {
        skip_string(&mut rest)?;
    }

    let prefix = format!("{}=", var);
    loop {
        let end = rest.iter().position(|byte| *byte == 0)?;
        if end == 0 {
            // the empty string that terminates the environment
            return None;
        }
        let entry = &rest[..end];
        rest = &rest[end + 1..];
        if let Ok(entry) = std::str::from_utf8(entry) {
            if let Some(value) = entry.strip_prefix(&prefix) {
                return Some(value.to_owned());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(edges: &[(u32, &[u32])]) -> ProcessTree {
        ProcessTree::from_children(
            edges
                .iter()
                .map(|(parent, children)| (*parent, children.to_vec()))
                .collect(),
        )
    }

    #[test]
    fn descendants_are_breadth_first_and_include_the_root() {
        let tree = tree(&[(10, &[20, 21]), (20, &[30])]);
        assert_eq!(tree.descendants(10), vec![10, 20, 21, 30]);
    }

    #[test]
    fn a_leaf_is_its_own_only_descendant() {
        let tree = tree(&[(10, &[20])]);
        assert_eq!(tree.descendants(20), vec![20]);
    }

    #[test]
    fn a_cycle_in_the_process_table_terminates() {
        let tree = tree(&[(10, &[20]), (20, &[10])]);
        assert_eq!(tree.descendants(10), vec![10, 20]);
    }

    #[test]
    fn reads_a_variable_from_a_nul_separated_environ() {
        let environ = b"PATH=/usr/bin\0CLAUDE_CODE_SESSION_ID=abc-123\0TERM=xterm\0";
        assert_eq!(
            env_from_nul_list(environ, "CLAUDE_CODE_SESSION_ID"),
            Some("abc-123".to_owned())
        );
        assert_eq!(env_from_nul_list(environ, "NOT_SET"), None);
    }

    #[test]
    fn does_not_match_a_variable_by_suffix() {
        let environ = b"MY_CLAUDE_CODE_SESSION_ID=wrong\0";
        assert_eq!(env_from_nul_list(environ, "CLAUDE_CODE_SESSION_ID"), None);
    }

    fn procargs2(exec_path: &str, argv: &[&str], environ: &[&str]) -> Vec<u8> {
        let mut blob = (argv.len() as i32).to_ne_bytes().to_vec();
        blob.extend_from_slice(exec_path.as_bytes());
        blob.push(0);
        blob.extend_from_slice(&[0, 0, 0]); // the alignment padding the kernel inserts
        for entry in argv.iter().chain(environ.iter()) {
            blob.extend_from_slice(entry.as_bytes());
            blob.push(0);
        }
        blob.push(0);
        blob
    }

    #[test]
    fn reads_a_variable_from_a_procargs2_blob() {
        let blob = procargs2(
            "/opt/homebrew/bin/claude",
            &["claude", "--dangerously-skip-permissions"],
            &["PATH=/usr/bin", "CLAUDE_CODE_SESSION_ID=abc-123"],
        );
        assert_eq!(
            env_from_procargs2(&blob, "CLAUDE_CODE_SESSION_ID"),
            Some("abc-123".to_owned())
        );
        assert_eq!(env_from_procargs2(&blob, "NOT_SET"), None);
    }

    #[test]
    fn an_argument_containing_an_equals_sign_is_not_read_as_a_variable() {
        let blob = procargs2(
            "/usr/bin/tool",
            &["tool", "SESSION_ID=from-an-argument"],
            &["SESSION_ID=from-the-environment"],
        );
        assert_eq!(
            env_from_procargs2(&blob, "SESSION_ID"),
            Some("from-the-environment".to_owned())
        );
    }

    /// Reads this very process, because the platform code is the whole point of the function and a
    /// fake process table cannot exercise it. `PATH` is in every process's initial environment.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn allowlisted_variables_are_read_from_a_real_process() {
        let tree = tree(&[]);
        let found = tree.find_all_envs(
            std::process::id(),
            &["PATH".to_owned(), "NOT_A_REAL_VARIABLE_ZELLIJ".to_owned()],
        );
        assert!(found.contains_key("PATH"), "found: {:?}", found.keys());
        assert!(!found.contains_key("NOT_A_REAL_VARIABLE_ZELLIJ"));
        // the allowlist is a secret boundary: a present-but-unlisted variable must not leak, and
        // nothing beyond the asked-for names may come back
        assert!(
            !found.contains_key("HOME"),
            "an unlisted variable leaked: {:?}",
            found.keys()
        );
        assert_eq!(found.len(), 1, "found: {:?}", found.keys());
    }

    #[test]
    fn an_empty_allowlist_reads_nothing() {
        let tree = tree(&[(10, &[20])]);
        assert!(tree.find_all_envs(10, &[]).is_empty());
    }

    #[test]
    fn a_truncated_procargs2_blob_finds_nothing() {
        assert_eq!(env_from_procargs2(&[1, 0], "ANY"), None);
        assert_eq!(env_from_procargs2(&[], "ANY"), None);
    }

    fn claude_record(dir: &Path, file_pid: u32, record_pid: u32, session_id: &str) {
        std::fs::write(
            dir.join(format!("{}.json", file_pid)),
            format!(
                r#"{{"pid":{},"sessionId":"{}","cwd":"/x"}}"#,
                record_pid, session_id
            ),
        )
        .unwrap();
    }

    fn no_files(_pid: u32) -> Option<Vec<PathBuf>> {
        Some(Vec::new())
    }

    fn no_env(_pid: u32, _var: &str) -> Option<String> {
        None
    }

    fn claude_session(
        tree: &ProcessTree,
        root: u32,
        dir: &Path,
    ) -> Result<OwnSession, SessionMiss> {
        tree.own_session(root, "claude", Some(dir), no_files, no_env)
    }

    /// The pane's shell is 10, claude is 20, and 30 is an agent claude started. The live session
    /// is the one in 20's record, whatever 30's says.
    #[test]
    fn the_live_claude_session_names_the_process_whose_record_answered() {
        let dir = tempfile::tempdir().unwrap();
        claude_record(dir.path(), 20, 20, "live");
        let tree = tree(&[(10, &[20]), (20, &[30])]);
        assert_eq!(
            claude_session(&tree, 10, dir.path()),
            Ok(OwnSession {
                id: "live".to_owned(),
                live: LiveSession::ClaudeRecord {
                    pid: 20,
                    dir: dir.path().to_path_buf()
                },
            })
        );
        assert_eq!(claude_session_record(dir.path(), 30), None);
    }

    #[test]
    fn the_live_claude_session_is_the_nearest_record() {
        let dir = tempfile::tempdir().unwrap();
        claude_record(dir.path(), 20, 20, "live");
        claude_record(dir.path(), 30, 30, "nested");
        let tree = tree(&[(10, &[20]), (20, &[30])]);
        assert_eq!(
            claude_session(&tree, 10, dir.path()).map(|own| own.id),
            Ok("live".to_owned())
        );
    }

    #[test]
    fn a_record_for_another_pid_is_not_the_live_session() {
        let dir = tempfile::tempdir().unwrap();
        claude_record(dir.path(), 20, 99, "stale");
        let tree = tree(&[(10, &[20])]);
        assert!(claude_session(&tree, 10, dir.path()).is_err());
    }

    #[test]
    fn no_record_is_a_miss_that_names_where_it_looked() {
        let dir = tempfile::tempdir().unwrap();
        let tree = tree(&[(10, &[20])]);
        assert_eq!(
            claude_session(&tree, 10, dir.path()),
            Err(SessionMiss::NoClaudeRecord {
                root: 10,
                searched: 2,
                dirs: vec![dir.path().to_path_buf()],
            })
        );
    }

    /// The server started before any rc file ran, so it does not see a `CLAUDE_CONFIG_DIR` the
    /// pane's shell exports. claude does, and writes its record there.
    #[test]
    fn a_record_under_the_claudes_own_config_dir_is_found() {
        let server_dir = tempfile::tempdir().unwrap();
        let claude_config = tempfile::tempdir().unwrap();
        std::fs::create_dir(claude_config.path().join("sessions")).unwrap();
        claude_record(&claude_config.path().join("sessions"), 20, 20, "own-dir");
        let tree = tree(&[(10, &[20])]);
        let config = claude_config.path().display().to_string();
        let env_of =
            |pid: u32, var: &str| (pid == 20 && var == "CLAUDE_CONFIG_DIR").then(|| config.clone());
        let found = tree
            .own_session(10, "claude", Some(server_dir.path()), no_files, env_of)
            .unwrap();
        assert_eq!(found.id, "own-dir");
        assert_eq!(found.live.source(), SessionSource::Record);
    }

    #[test]
    fn with_no_home_anywhere_there_is_no_directory_to_look_in() {
        let tree = tree(&[(10, &[20])]);
        assert_eq!(
            tree.own_session(10, "claude", None, no_files, no_env),
            Err(SessionMiss::NoSessionsDir)
        );
    }

    #[test]
    fn the_config_dir_wins_over_home_and_empty_means_unset() {
        assert_eq!(
            claude_sessions_dir_for(Some("/c".into()), Some("/h".into())),
            Some(PathBuf::from("/c/sessions"))
        );
        assert_eq!(
            claude_sessions_dir_for(Some("".into()), Some("/h".into())),
            Some(PathBuf::from("/h/.claude/sessions"))
        );
        assert_eq!(claude_sessions_dir_for(None, None), None);
    }

    /// A crashed claude leaves its record; a later process gets the pid. On Linux the record's
    /// `procStart` must match the live process's start time.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_record_left_by_an_earlier_process_with_the_pid_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let start = process_start_ticks(pid).expect("this process has a start time");
        let write = |proc_start: &str| {
            std::fs::write(
                dir.path().join(format!("{}.json", pid)),
                format!(
                    r#"{{"pid":{},"sessionId":"s","pidDomain":"linux:boot:pid:[1]","procStart":"{}"}}"#,
                    pid, proc_start
                ),
            )
            .unwrap()
        };
        write(&start);
        assert_eq!(claude_session_record(dir.path(), pid).as_deref(), Some("s"));
        write("1");
        assert_eq!(claude_session_record(dir.path(), pid), None);
    }

    #[test]
    fn the_start_time_is_counted_from_the_last_parenthesis() {
        let mut fields = vec!["S".to_owned()];
        fields.extend((4..=21).map(|n| n.to_string()));
        fields.push("226826695".to_owned());
        fields.push("23".to_owned());
        let stat = format!("1037351 (cl (a) ude) {}", fields.join(" "));
        assert_eq!(start_ticks_from_stat(&stat).as_deref(), Some("226826695"));
    }

    #[test]
    fn a_rollout_file_name_carries_the_codex_session_id() {
        assert_eq!(
            codex_session_from_path(Path::new(
                "/home/u/.codex/sessions/2026/10/06/rollout-2026-10-06T16-17-39-01a112dd-5b0e-7db3-964b-68a8a71304b1.jsonl"
            ))
            .as_deref(),
            Some("01a112dd-5b0e-7db3-964b-68a8a71304b1")
        );
        assert_eq!(
            codex_session_from_path(Path::new("/home/u/.codex/state_5.sqlite")),
            None
        );
        assert_eq!(
            codex_session_from_path(Path::new("/x/rollout-2026-10-06T16-17-39-not-a-uuid.jsonl")),
            None
        );
    }

    /// The pane's shell is 10; 20 is the npm `codex` wrapper and 30 the native binary it runs,
    /// which holds the rollout open.
    #[test]
    fn the_codex_session_is_the_rollout_a_process_under_the_pane_holds() {
        let tree = tree(&[(10, &[20]), (20, &[30])]);
        let open_files = |pid: u32| {
            Some(match pid {
                30 => vec![
                    PathBuf::from("/home/u/.codex/state_5.sqlite"),
                    PathBuf::from(
                        "/home/u/.codex/sessions/2026/10/06/rollout-2026-10-06T16-17-39-01a112dd-5b0e-7db3-964b-68a8a71304b1.jsonl",
                    ),
                ],
                _ => vec![],
            })
        };
        let found = tree
            .own_session(10, "codex", None, open_files, no_env)
            .unwrap();
        assert_eq!(found.id, "01a112dd-5b0e-7db3-964b-68a8a71304b1");
        assert_eq!(found.live, LiveSession::CodexRollout { pid: 30 });
        assert_eq!(found.live.source(), SessionSource::OpenFile);
        assert_eq!(
            found.live.reread(open_files).as_deref(),
            Some("01a112dd-5b0e-7db3-964b-68a8a71304b1")
        );
        // codex exited, or `/new` closed the file and has not written the next one yet
        assert_eq!(found.live.reread(no_files), None);
    }

    #[test]
    fn a_codex_with_no_rollout_open_is_a_miss() {
        let tree = tree(&[(10, &[20])]);
        assert_eq!(
            tree.own_session(10, "codex", None, no_files, no_env),
            Err(SessionMiss::NoOpenRollout {
                root: 10,
                searched: 2
            })
        );
    }

    #[test]
    fn opencode_and_pi_have_no_per_process_session_to_read() {
        let tree = tree(&[(10, &[20])]);
        for kind in ["opencode", "pi"] {
            assert_eq!(
                tree.own_session(10, kind, None, no_files, no_env),
                Err(SessionMiss::NoPerProcessHandle(kind))
            );
        }
    }

    /// Reads this very process's open files, because the platform code is the point and a fake
    /// cannot exercise it. A file this test holds open must be among them.
    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn open_files_are_read_from_a_real_process() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir
            .path()
            .join("rollout-2026-10-06T16-17-39-01a112dd-5b0e-7db3-964b-68a8a71304b1.jsonl");
        let _held = std::fs::File::create(&path).unwrap();
        let files = open_files_of(std::process::id()).expect("this process's files can be listed");
        let canonical = std::fs::canonicalize(&path).unwrap();
        assert!(
            files.iter().any(|file| file == &path || file == &canonical),
            "{:?} not among {:?}",
            path,
            files
        );
    }
}
