//! Other machines: the `remote_hosts` block, `host/session` addressing, and the ssh that carries
//! a tool call to a session that runs somewhere else.
//!
//! A remote call is the same command line a local one is, run by the zellij on the other machine
//! instead of this one. Nothing new crosses the wire: ssh carries the argv, the remote CLI answers
//! exactly as it would at a terminal there, and its exit code means what the local one means.
//!
//! ```kdl
//! remote_hosts {
//!     host "boxa"
//!     host "boxb" bin="/opt/homebrew/bin/zellij"
//! }
//! ```
//!
//! The block is the whitelist: a host it does not list is refused before ssh is run. One config is
//! shared by every machine, so the list names the machine reading it too, and that entry is
//! skipped - a call to it stays local. The block is read here, by the MCP server alone, rather than
//! by the config parser every zellij process runs, because nothing else needs it and the parsed
//! `Options` cross the client/server contract. An older binary ignores an unknown top-level key.

use std::path::{Path, PathBuf};

use kdl::{KdlDocument, KdlNode, KdlValue};
use serde_json::{Map, Value};

use super::invoke::Outcome;

/// The environment variable that names the ssh program, in place of `ssh` on `PATH`. A test sets
/// it to a stand-in that records its argv.
pub const SSH_PROGRAM_ENV: &str = "ZELLIJ_MCP_SSH";

/// What separates the host from the session in a `session` argument: `boxa/work`. A session name
/// cannot contain it - see `zellij_utils::sessions::validate_session_name`.
pub const HOST_SEPARATOR: char = '/';

/// How long `zellij_overview scope=fleet` waits for any one host, in seconds. ssh gives up on a
/// connection after five; the rest is the remote `list-agents` itself.
pub const FLEET_TIMEOUT_S: u64 = 10;

/// One `host` line of the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    /// What ssh is given: a name from `~/.ssh/config`, a hostname, or `user@host`.
    pub name: String,
    /// The zellij binary on that machine. Without one, the remote shell's `PATH` and the usual
    /// install places are tried - see [`ZELLIJ_ON_REMOTE`].
    pub bin: Option<String>,
}

/// The `remote_hosts` block, and what in it was passed over.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteHosts {
    pub hosts: Vec<RemoteHost>,
    /// An entry this build did not understand, said once per entry. Never an error: a newer
    /// config on an older binary must not stop the server.
    pub warnings: Vec<String>,
    /// The file the block was read from, for a refusal to name.
    pub path: Option<PathBuf>,
}

impl RemoteHosts {
    /// The block in the config file at `path`. A file that is missing has no hosts; one that does
    /// not parse has none either, and says why in `warnings`.
    pub fn load(path: Option<&Path>) -> Self {
        let Some(path) = path else {
            return RemoteHosts::default();
        };
        let mut hosts = match std::fs::read_to_string(path) {
            Ok(text) => RemoteHosts::parse(&text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RemoteHosts::default(),
            Err(e) => RemoteHosts {
                warnings: vec![format!("{} could not be read: {}", path.display(), e)],
                ..Default::default()
            },
        };
        hosts.path = Some(path.to_path_buf());
        hosts
    }

    /// The block out of a whole config file's text.
    pub fn parse(text: &str) -> Self {
        let document: KdlDocument = match text.parse() {
            Ok(document) => document,
            Err(e) => {
                return RemoteHosts {
                    warnings: vec![format!("the config file does not parse: {}", e)],
                    ..Default::default()
                }
            },
        };
        let mut hosts = RemoteHosts::default();
        let Some(block) = document.get("remote_hosts") else {
            return hosts;
        };
        let Some(children) = block.children() else {
            return hosts;
        };
        for node in children.nodes() {
            match node.name().value() {
                "host" => {
                    if let Some(host) = hosts.host_from_kdl(node) {
                        if hosts.find(&host.name).is_some() {
                            hosts
                                .warnings
                                .push(format!("remote_hosts lists {:?} twice", host.name));
                        } else {
                            hosts.hosts.push(host);
                        }
                    }
                },
                other => hosts.warnings.push(format!(
                    "unknown remote_hosts entry {:?}, ignored (expected host)",
                    other
                )),
            }
        }
        hosts
    }

    fn host_from_kdl(&mut self, node: &KdlNode) -> Option<RemoteHost> {
        let mut name = None;
        let mut bin = None;
        for entry in node.entries() {
            match (entry.name().map(|name| name.value()), entry.value()) {
                (None, KdlValue::String(value)) if name.is_none() => name = Some(value.clone()),
                (Some("bin"), KdlValue::String(value)) if value.starts_with('/') => {
                    bin = Some(value.clone())
                },
                (Some("bin"), value) => self.warnings.push(format!(
                    "remote_hosts: bin={} is not an absolute path, ignored",
                    value
                )),
                (Some(other), _) => self.warnings.push(format!(
                    "unknown remote_hosts host property {:?}, ignored (expected bin)",
                    other
                )),
                (None, value) => self.warnings.push(format!(
                    "remote_hosts: host takes one name, and {} is not it",
                    value
                )),
            }
        }
        let Some(name) = name else {
            self.warnings
                .push("remote_hosts: a host line with no name, ignored".to_owned());
            return None;
        };
        if let Some(reason) = host_name_error(&name) {
            self.warnings
                .push(format!("remote_hosts: host {:?} {}, ignored", name, reason));
            return None;
        }
        Some(RemoteHost { name, bin })
    }

    /// The listed host by this name, ignoring case.
    pub fn find(&self, name: &str) -> Option<&RemoteHost> {
        self.hosts
            .iter()
            .find(|host| host.name.eq_ignore_ascii_case(name))
    }

    /// Every listed host but this machine.
    pub fn remote(&self, local: &str) -> Vec<&RemoteHost> {
        self.hosts
            .iter()
            .filter(|host| !is_local(&host.name, local))
            .collect()
    }

    /// What this machine is called in a fleet answer: its own entry in the list when it has one,
    /// so every machine names it the same way, and its short hostname otherwise.
    pub fn local_name(&self, local: &str) -> String {
        self.hosts
            .iter()
            .find(|host| is_local(&host.name, local))
            .map(|host| host.name.clone())
            .unwrap_or_else(|| local.to_ascii_lowercase())
    }
}

/// Why a name cannot be handed to ssh as a host, or `None` when it can.
///
/// A name is one argv word to ssh, so whitespace would split it and a leading dash would make it
/// an option. `/` is the session separator.
fn host_name_error(name: &str) -> Option<&'static str> {
    if name.is_empty() {
        Some("is empty")
    } else if name.starts_with('-') {
        Some("begins with a dash")
    } else if name.contains(HOST_SEPARATOR) {
        Some("contains `/`")
    } else if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "._-@:".contains(c))
    {
        Some("has a character a hostname cannot")
    } else {
        None
    }
}

/// Whether a listed host is this machine.
///
/// Compared by the first label, ignoring case and any `user@`: `boxa`, `BOXA.local`,
/// `user@boxa.example.net` all name a machine whose hostname is `boxa`. A Mac reports
/// `NAME.local`; the same name in the shared list is written bare.
pub fn is_local(host: &str, local: &str) -> bool {
    let label = |name: &str| -> String {
        let name = name.rsplit_once('@').map_or(name, |(_, host)| host);
        name.split('.')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    };
    let local = label(local);
    !local.is_empty() && label(host) == local
}

/// Where a tool call runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Local,
    Remote(RemoteHost),
}

impl Target {
    pub fn host(&self) -> Option<&str> {
        match self {
            Target::Local => None,
            Target::Remote(host) => Some(&host.name),
        }
    }
    pub fn is_remote(&self) -> bool {
        matches!(self, Target::Remote(_))
    }
}

/// Where a call runs, read off its `session` argument, which is rewritten to the bare session.
///
/// `boxa/work` is session `work` on `boxa`. `boxa/` is `boxa` with no session named, which the
/// remote CLI resolves the way a terminal there would. A session with no `/` is this machine's, as
/// it always was. A host this machine is called stays local, listed or not. Any other host has to
/// be in the list.
pub fn route(
    arguments: &mut Map<String, Value>,
    hosts: &RemoteHosts,
    local: &str,
) -> Result<Target, String> {
    let Some(Value::String(session)) = arguments.get("session") else {
        return Ok(Target::Local);
    };
    let Some((host, bare)) = session.split_once(HOST_SEPARATOR) else {
        return Ok(Target::Local);
    };
    if host.is_empty() {
        return Err(format!(
            "`session` {:?} names no host: write host/session, like boxa/work.",
            session
        ));
    }
    if bare.contains(HOST_SEPARATOR) {
        return Err(format!(
            "`session` {:?} has more than one `/`: write host/session, like boxa/work.",
            session
        ));
    }
    let target = if is_local(host, local) {
        Target::Local
    } else {
        match hosts.find(host) {
            Some(listed) => Target::Remote(listed.clone()),
            None => return Err(not_listed(host, hosts)),
        }
    };
    let bare = bare.to_owned();
    if bare.is_empty() {
        arguments.remove("session");
    } else {
        arguments.insert("session".to_owned(), Value::String(bare));
    }
    Ok(target)
}

fn not_listed(host: &str, hosts: &RemoteHosts) -> String {
    let file = hosts
        .path
        .as_ref()
        .map_or("config.kdl".to_owned(), |path| path.display().to_string());
    if hosts.hosts.is_empty() {
        format!(
            "`{}` is not a host this server may reach: no remote_hosts block is configured in {}.",
            host, file
        )
    } else {
        let listed: Vec<&str> = hosts.hosts.iter().map(|host| host.name.as_str()).collect();
        format!(
            "`{}` is not in remote_hosts in {}, so this server will not reach it. Listed: {}.",
            host,
            file,
            listed.join(", ")
        )
    }
}

/// One word for a POSIX shell, single-quoted.
///
/// ssh joins its arguments with spaces and hands the line to the remote user's login shell, so
/// every word has to survive a shell. Single quotes take everything literally but a single quote,
/// which is closed, escaped and reopened. The form reads the same in sh, bash, zsh and fish.
pub fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

/// The script the remote shell runs for a tool call: find zellij, then run it with the argv.
///
/// `$1` is the configured binary, or empty to look. A non-interactive ssh shell on macOS has no
/// Homebrew on `PATH` unless the user's rc says so, which is why the usual places are tried after
/// `command -v`. Everything after `$1` is the argv, passed as positional parameters so that none of
/// it is ever parsed as shell.
pub const ZELLIJ_ON_REMOTE: &str = "b=$1; shift; \
if [ -z \"$b\" ]; then \
for c in \"$(command -v zellij 2>/dev/null)\" /opt/homebrew/bin/zellij /usr/local/bin/zellij \
/home/linuxbrew/.linuxbrew/bin/zellij \"$HOME/.local/bin/zellij\" \"$HOME/.cargo/bin/zellij\"; do \
if [ -n \"$c\" ] && [ -x \"$c\" ]; then b=$c; break; fi; done; fi; \
if [ -z \"$b\" ]; then echo \"zellij was not found on $(hostname): give this host bin= in \
remote_hosts\" >&2; exit 127; fi; \
exec \"$b\" \"$@\"";

/// The script that stores a file sent over stdin and prints where it went: `$1` is its name.
///
/// A brief and an image live on the caller's machine, and the remote pane can only read files on
/// its own. They land in a private directory under the remote temp dir, and anything there older
/// than a week is removed first.
pub const UPLOAD_ON_REMOTE: &str = "d=${TMPDIR:-/tmp}; d=${d%/}/zellij-mcp-files-$(id -u); \
umask 077; mkdir -p \"$d\" || exit 1; \
find \"$d\" -type f -mtime +7 -exec rm -f {} + 2>/dev/null; \
f=\"$d/$1\"; cat > \"$f\" || exit 1; printf \"%s\\n\" \"$f\"";

/// The line the remote login shell is given: `sh -c '<script>' sh '<arg>' ...`, every word quoted.
pub fn remote_command(script: &str, args: &[String]) -> String {
    let mut words = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        sh_quote(script),
        "sh".to_owned(),
    ];
    words.extend(args.iter().map(|arg| sh_quote(arg)));
    words.join(" ")
}

/// The remote line that runs `zellij <argv>` on `host`.
pub fn zellij_command(host: &RemoteHost, argv: &[String]) -> String {
    let mut args = vec![host.bin.clone().unwrap_or_default()];
    args.extend(argv.iter().cloned());
    remote_command(ZELLIJ_ON_REMOTE, &args)
}

/// ssh's own argv, after the program: options, the host, and the one remote line.
///
/// Never interactive - `BatchMode` fails rather than asking for a password or a host key - and
/// never forwarding the agent or X. A connection is shared across calls for a minute when there is
/// a private directory for its socket, because a supervisor polling three hosts would otherwise pay
/// a handshake per host per poll.
pub fn ssh_argv(host: &str, remote_line: String, control_dir: Option<&Path>) -> Vec<String> {
    let mut argv: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        "ConnectTimeout=5",
        "-o",
        "ForwardAgent=no",
        "-o",
        "ForwardX11=no",
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect();
    match control_dir {
        Some(dir) => argv.extend([
            "-o".to_owned(),
            "ControlMaster=auto".to_owned(),
            "-o".to_owned(),
            format!("ControlPath={}/%C", dir.display()),
            "-o".to_owned(),
            "ControlPersist=60".to_owned(),
        ]),
        None => argv.extend(["-o".to_owned(), "ControlMaster=no".to_owned()]),
    }
    argv.push("--".to_owned());
    argv.push(host.to_owned());
    argv.push(remote_line);
    argv
}

/// A private directory for ssh's shared-connection sockets, or `None` to share nothing.
///
/// Under `/tmp` and short, never under `$TMPDIR`: on macOS that is a 50-character path, and a unix
/// socket path tops out near 104 bytes with ssh's own 40-character hash and temporary suffix on it.
/// A directory that is not ours, or not private, is not used.
#[cfg(unix)]
fn control_dir() -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = unsafe { libc::getuid() };
    let dir = PathBuf::from(format!("/tmp/zellij-mcp-{}", uid));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&dir);
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return None;
    }
    Some(dir)
}

#[cfg(not(unix))]
fn control_dir() -> Option<PathBuf> {
    None
}

fn ssh_program() -> String {
    std::env::var(SSH_PROGRAM_ENV)
        .ok()
        .filter(|program| !program.trim().is_empty())
        .unwrap_or_else(|| "ssh".to_owned())
}

/// Run one remote line on `host`, with `stdin` fed to it when there is any.
///
/// Exit 255 is ssh's own failure - the host did not answer, or refused the key - and says so,
/// since the remote CLI never exits 255 and the bare ssh message does not name the cause.
pub async fn run_line(
    host: &str,
    remote_line: String,
    stdin: Option<Vec<u8>>,
) -> Result<Outcome, String> {
    let program = ssh_program();
    let argv = ssh_argv(host, remote_line, control_dir().as_deref());
    let mut child = tokio::process::Command::new(&program)
        .args(&argv)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("Could not run `{}` to reach {}: {}", program, host, e))?;
    if let (Some(bytes), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // written beside the wait, not before it: a remote that answers before it has read
        // everything would otherwise fill its stdout pipe while this side is still writing
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = pipe.write_all(&bytes).await;
            let _ = pipe.shutdown().await;
        });
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|e| format!("`{}` to {} failed: {}", program, host, e))?;
    let code = output.status.code().unwrap_or(1);
    let mut stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    if code == 255 {
        stderr = format!("ssh could not reach {} (exit 255): {}", host, stderr.trim());
    }
    Ok(Outcome {
        code,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr,
    })
}

/// Run `zellij <argv>` on `host`.
pub async fn run(
    host: &RemoteHost,
    argv: &[String],
    stdin: Option<Vec<u8>>,
) -> Result<Outcome, String> {
    run_line(&host.name, zellij_command(host, argv), stdin).await
}

/// Copy a file on this machine to `host`, and return the path it now has there.
///
/// The name on the remote side keeps the file's own name, behind a random prefix so two calls
/// cannot collide, and with anything but letters, digits, `.`, `-` and `_` replaced.
pub async fn upload(host: &RemoteHost, local: &Path) -> Result<String, String> {
    let bytes = std::fs::read(local)
        .map_err(|e| format!("{} could not be read: {}", local.display(), e))?;
    let name = local
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || ".-_".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let stored = format!(
        "{:x}-{:x}-{}-{}",
        nanos,
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        safe
    );
    let outcome = run_line(
        &host.name,
        remote_command(UPLOAD_ON_REMOTE, &[stored]),
        Some(bytes),
    )
    .await?;
    let path = outcome.stdout.trim();
    if outcome.is_error() || !path.starts_with('/') {
        return Err(format!(
            "{} could not be copied to {}: {}",
            local.display(),
            host.name,
            outcome.stderr.trim()
        ));
    }
    Ok(path.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CONFIG: &str = r#"
        default_shell "zsh"
        remote_hosts {
            host "boxa"
            host "boxb" bin="/opt/homebrew/bin/zellij"
            host "Local-Box"
        }
    "#;

    #[test]
    fn the_block_lists_hosts_and_their_binaries() {
        let hosts = RemoteHosts::parse(CONFIG);
        assert_eq!(hosts.warnings, Vec::<String>::new());
        assert_eq!(
            hosts.hosts,
            vec![
                RemoteHost {
                    name: "boxa".into(),
                    bin: None
                },
                RemoteHost {
                    name: "boxb".into(),
                    bin: Some("/opt/homebrew/bin/zellij".into())
                },
                RemoteHost {
                    name: "Local-Box".into(),
                    bin: None
                },
            ]
        );
    }

    #[test]
    fn a_config_without_the_block_has_no_hosts_and_says_nothing() {
        let hosts = RemoteHosts::parse("default_shell \"zsh\"\n");
        assert!(hosts.hosts.is_empty());
        assert!(hosts.warnings.is_empty());
        assert_eq!(RemoteHosts::load(None), RemoteHosts::default());
        let missing = RemoteHosts::load(Some(Path::new("/nonexistent/zellij/config.kdl")));
        assert!(missing.hosts.is_empty() && missing.warnings.is_empty());
    }

    #[test]
    fn an_entry_this_build_does_not_know_warns_and_the_rest_still_load() {
        let hosts = RemoteHosts::parse(
            r#"remote_hosts {
                host "boxa" port=22
                group "lab"
                host "boxb" bin="zellij"
                host "-oProxyCommand=x"
                host "boxa"
                host
            }"#,
        );
        let names: Vec<&str> = hosts.hosts.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(names, vec!["boxa", "boxb"]);
        assert_eq!(hosts.hosts[1].bin, None, "a relative bin is not trusted");
        let said = hosts.warnings.join("\n");
        for expected in [
            "unknown remote_hosts host property \"port\"",
            "unknown remote_hosts entry \"group\"",
            "not an absolute path",
            "begins with a dash",
            "lists \"boxa\" twice",
            "no name",
        ] {
            assert!(
                said.contains(expected),
                "{} missing from:\n{}",
                expected,
                said
            );
        }
    }

    #[test]
    fn a_config_that_does_not_parse_is_no_hosts_and_a_warning() {
        let hosts = RemoteHosts::parse("remote_hosts {");
        assert!(hosts.hosts.is_empty());
        assert_eq!(hosts.warnings.len(), 1);
    }

    #[test]
    fn this_machine_is_matched_by_its_first_label_ignoring_case() {
        assert!(is_local("boxa", "BOXA.local"));
        assert!(is_local("BoxA", "boxa"));
        assert!(is_local("user@boxa.example.net", "boxa"));
        assert!(!is_local("boxb", "boxa"));
        assert!(!is_local("boxa", ""));
        let hosts = RemoteHosts::parse(CONFIG);
        let remote: Vec<&str> = hosts
            .remote("local-box.local")
            .iter()
            .map(|h| h.name.as_str())
            .collect();
        assert_eq!(remote, vec!["boxa", "boxb"]);
        assert_eq!(hosts.local_name("LOCAL-BOX.local"), "Local-Box");
        assert_eq!(hosts.local_name("unlisted"), "unlisted");
    }

    fn routed(session: &str, local: &str) -> (Result<Target, String>, Map<String, Value>) {
        let mut arguments = json!({"session": session, "pane": "sunny-otter"})
            .as_object()
            .unwrap()
            .clone();
        let target = route(&mut arguments, &RemoteHosts::parse(CONFIG), local);
        (target, arguments)
    }

    #[test]
    fn host_slash_session_goes_to_that_host_with_the_bare_session() {
        let (target, arguments) = routed("boxb/work", "local-box");
        assert_eq!(
            target.unwrap(),
            Target::Remote(RemoteHost {
                name: "boxb".into(),
                bin: Some("/opt/homebrew/bin/zellij".into())
            })
        );
        assert_eq!(arguments["session"], json!("work"));
        assert_eq!(arguments["pane"], json!("sunny-otter"));
    }

    #[test]
    fn a_host_with_no_session_lets_the_remote_cli_resolve_one() {
        let (target, arguments) = routed("BOXA/", "local-box");
        assert_eq!(target.unwrap().host(), Some("boxa"));
        assert!(arguments.get("session").is_none());
    }

    #[test]
    fn a_plain_session_and_this_machine_stay_local() {
        let (target, arguments) = routed("work", "local-box");
        assert_eq!(target.unwrap(), Target::Local);
        assert_eq!(arguments["session"], json!("work"));
        let (target, arguments) = routed("local-box/work", "Local-Box.local");
        assert_eq!(target.unwrap(), Target::Local);
        assert_eq!(arguments["session"], json!("work"));
        // this machine, even when it is not listed
        let (target, _) = routed("elsewhere/work", "elsewhere");
        assert_eq!(target.unwrap(), Target::Local);
        let mut none = Map::new();
        assert_eq!(
            route(&mut none, &RemoteHosts::default(), "x").unwrap(),
            Target::Local
        );
    }

    #[test]
    fn a_host_not_in_the_list_is_refused_before_anything_runs() {
        let (target, arguments) = routed("boxz/work", "local-box");
        let refusal = target.unwrap_err();
        assert!(
            refusal.contains("`boxz` is not in remote_hosts"),
            "{}",
            refusal
        );
        assert!(refusal.contains("boxa, boxb, Local-Box"), "{}", refusal);
        assert_eq!(arguments["session"], json!("boxz/work"), "left as given");
        let mut arguments = json!({"session": "boxa/work"}).as_object().unwrap().clone();
        let refusal = route(&mut arguments, &RemoteHosts::default(), "x").unwrap_err();
        assert!(refusal.contains("no remote_hosts block"), "{}", refusal);
        assert!(routed("/work", "x")
            .0
            .unwrap_err()
            .contains("names no host"));
        assert!(routed("boxa/a/b", "x")
            .0
            .unwrap_err()
            .contains("more than one"));
    }

    #[test]
    fn every_word_survives_a_posix_shell() {
        assert_eq!(sh_quote("plain"), "'plain'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote("$(rm -rf ~) `x` \"y\""), "'$(rm -rf ~) `x` \"y\"'");
        // and a real shell agrees, word for word
        let words = [
            "a b",
            "it's",
            "$HOME",
            "`id`",
            "",
            "-n",
            "line\nbreak",
            "\\",
        ];
        let line = format!(
            "printf '%s\\0' {}",
            words
                .iter()
                .map(|w| sh_quote(w))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&line)
            .output()
            .expect("sh runs");
        let got: Vec<String> = String::from_utf8(out.stdout)
            .unwrap()
            .split('\0')
            .map(str::to_owned)
            .collect();
        assert_eq!(&got[..words.len()], &words.map(str::to_owned)[..]);
    }

    #[test]
    fn the_remote_line_runs_the_probe_with_the_argv_as_positional_words() {
        let host = RemoteHost {
            name: "boxa".into(),
            bin: None,
        };
        let argv: Vec<String> = ["-s", "my work", "action", "write-chars", "--", "it's $x"]
            .iter()
            .map(|w| (*w).to_owned())
            .collect();
        let line = zellij_command(&host, &argv);
        assert!(line.starts_with("sh -c '"), "{}", line);
        assert!(
            line.ends_with(" sh '' '-s' 'my work' 'action' 'write-chars' '--' 'it'\\''s $x'"),
            "{}",
            line
        );
        // run it through a real shell with a stand-in zellij, as the remote login shell would
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("zellij");
        std::fs::write(&fake, "#!/bin/sh\nprintf '%s|' \"$@\"\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let with_bin = RemoteHost {
            name: "boxa".into(),
            bin: Some(fake.display().to_string()),
        };
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(zellij_command(&with_bin, &argv))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "-s|my work|action|write-chars|--|it's $x|"
        );
        // and the probe finds it on PATH when no bin is configured
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(zellij_command(&host, &argv))
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "-s|my work|action|write-chars|--|it's $x|"
        );
    }

    #[test]
    fn ssh_is_never_interactive_and_the_host_is_never_an_option() {
        let argv = ssh_argv(
            "boxa",
            "sh -c 'x'".into(),
            Some(Path::new("/tmp/zellij-mcp-1000")),
        );
        let joined = argv.join(" ");
        for expected in [
            "-T",
            "BatchMode=yes",
            "ConnectTimeout=5",
            "ForwardAgent=no",
            "ControlMaster=auto",
            "ControlPath=/tmp/zellij-mcp-1000/%C",
            "ControlPersist=60",
        ] {
            assert!(
                argv.iter().any(|w| w == expected),
                "{} in {}",
                expected,
                joined
            );
        }
        assert_eq!(&argv[argv.len() - 3..], &["--", "boxa", "sh -c 'x'"]);
        let unshared = ssh_argv("boxa", "x".into(), None);
        assert!(unshared.iter().any(|w| w == "ControlMaster=no"));
        assert!(!unshared.iter().any(|w| w.starts_with("ControlPath")));
    }

    #[test]
    fn an_upload_stores_stdin_privately_and_prints_where() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new("sh")
            .arg("-c")
            .arg(remote_command(UPLOAD_ON_REMOTE, &["x-brief.md".to_owned()]))
            .env("TMPDIR", tmp.path())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"line one\nit's line two\n")
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let path = String::from_utf8(out.stdout).unwrap().trim().to_owned();
        assert!(
            path.starts_with(&tmp.path().display().to_string()),
            "{}",
            path
        );
        assert!(path.ends_with("/x-brief.md"), "{}", path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "line one\nit's line two\n"
        );
        let dir = Path::new(&path).parent().unwrap();
        let mode = std::fs::metadata(dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "the directory is private: {:o}", mode);
    }
}
