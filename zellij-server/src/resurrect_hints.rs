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
//! Every failure here is a debug log and a `None`. Serialization runs on a timer and its job is to
//! preserve what it can - a hint that cannot be resolved must cost the snapshot nothing.

use std::collections::{BTreeMap, HashMap};

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

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
}
