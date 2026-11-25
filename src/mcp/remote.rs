//! Other machines: the `remote_hosts` block, `host/session` addressing, and the ssh that carries
//! a tool call to a session that runs somewhere else.
//!
//! A remote call is the same command line a local one is, run by the zellij on the other machine
//! instead of this one. Nothing new crosses the wire: ssh carries the argv, the remote CLI answers
//! exactly as it would at a terminal there, and its exit code means what the local one means.
//!
//! ```kdl
//! remote_hosts {
//!     from "~/.config/fleet/machines.tsv" column="hostname" where="zellij_peer=yes"
//!     host "boxa"
//!     host "boxb" bin="/opt/homebrew/bin/zellij"
//!     host "boxc.example.net" alias="boxc"
//! }
//! ```
//!
//! `connect_timeout` and `fleet_timeout` set how long ssh waits for a connection and how long
//! `scope=fleet` waits for one host: whole seconds from 1 to 120, 5 and 10 unset. The bounds that
//! derive from them are in `zellij_utils::remote_timeouts`.
//!
//! An alias is a second, shorter name for a host: `boxc/work` reaches `boxc.example.net`. A `from`
//! line takes its aliases from a column, `alias_column="short"`.
//!
//! `from` reads hosts out of a tab-separated table with a header line, so a fleet that already
//! keeps one needs no second list. It is read on every call, like the rest of the block.
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

/// One `host` line of the block.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteHost {
    /// What ssh is given: a name from `~/.ssh/config`, a hostname, or `user@host`.
    pub name: String,
    /// The zellij binary on that machine. Without one, the remote shell's `PATH` and the usual
    /// install places are tried - see [`ZELLIJ_ON_REMOTE`].
    pub bin: Option<String>,
    /// A second name a `session` may use for this host: `alias="boxc"`, or a `from` table's
    /// `alias_column`. ssh is always given `name`.
    pub alias: Option<String>,
    /// ssh's `ConnectTimeout`, in seconds: the block's `connect_timeout`, 5 unset.
    pub connect_timeout_s: u64,
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
    /// The block's `connect_timeout` and `fleet_timeout`, or their defaults.
    pub timeouts: zellij_utils::remote_timeouts::RemoteTimeouts,
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

    /// The block out of a whole config file's text, with `~` in a `from` path meaning `$HOME`.
    pub fn parse(text: &str) -> Self {
        RemoteHosts::parse_in(text, std::env::var("HOME").ok().as_deref())
    }

    /// The same, told what `~` means.
    ///
    /// Inline `host` lines and the rows of every `from` file are merged: file rows first, in
    /// file order, then the inline hosts no file named. An inline host with a file row's name
    /// replaces that row, which is how a host listed in a shared table gets its own `bin=`.
    pub fn parse_in(text: &str, home: Option<&str>) -> Self {
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
        let mut from_files: Vec<RemoteHost> = Vec::new();
        for node in children.nodes() {
            match node.name().value() {
                "from" => {
                    for host in hosts.hosts_from_file(node, home) {
                        if !from_files
                            .iter()
                            .any(|seen| seen.name.eq_ignore_ascii_case(&host.name))
                        {
                            from_files.push(host);
                        }
                    }
                },
                "host" => {
                    if let Some(host) = hosts.host_from_kdl(node) {
                        if hosts.named(&host.name).is_some() {
                            hosts
                                .warnings
                                .push(format!("remote_hosts lists {:?} twice", host.name));
                        } else {
                            hosts.hosts.push(host);
                        }
                    }
                },
                // read by `zellij_utils::remote_timeouts`, below
                name if zellij_utils::remote_timeouts::is_timeout_entry(name) => {},
                other => hosts.warnings.push(format!(
                    "unknown remote_hosts entry {:?}, ignored (expected host, from, \
                     connect_timeout or fleet_timeout)",
                    other
                )),
            }
        }
        let inline = std::mem::take(&mut hosts.hosts);
        for row in from_files {
            let chosen = match inline
                .iter()
                .find(|host| host.name.eq_ignore_ascii_case(&row.name))
            {
                // an inline line without an alias keeps the table's
                Some(host) => RemoteHost {
                    alias: host.alias.clone().or(row.alias),
                    ..host.clone()
                },
                None => row,
            };
            hosts.hosts.push(chosen);
        }
        for host in inline {
            if hosts.named(&host.name).is_none() {
                hosts.hosts.push(host);
            }
        }
        hosts.drop_clashing_aliases();
        let (timeouts, warnings) = zellij_utils::remote_timeouts::from_block(children);
        hosts.warnings.extend(warnings);
        hosts.timeouts = timeouts.unwrap_or_default();
        for host in &mut hosts.hosts {
            host.connect_timeout_s = hosts.timeouts.connect_s;
        }
        hosts
    }

    /// An alias must name one host: one that is also a host's name, or an earlier host's alias, is
    /// dropped with a warning, and the hosts themselves stay.
    fn drop_clashing_aliases(&mut self) {
        for at in 0..self.hosts.len() {
            let Some(alias) = self.hosts[at].alias.clone() else {
                continue;
            };
            let clashes = self.hosts.iter().enumerate().any(|(other, host)| {
                host.name.eq_ignore_ascii_case(&alias)
                    || (other < at
                        && host
                            .alias
                            .as_deref()
                            .is_some_and(|theirs| theirs.eq_ignore_ascii_case(&alias)))
            });
            if clashes {
                self.warnings.push(format!(
                    "remote_hosts: alias {:?} of {:?} names another host too, ignored",
                    alias, self.hosts[at].name
                ));
                self.hosts[at].alias = None;
            }
        }
    }

    /// The hosts a `from` line names: one per row of a tab-separated table with a header line,
    /// read from the `column` named (default `hostname`), kept when `where="col=value"` matches.
    ///
    /// Every way this can go wrong - a missing file, a column the header does not have, a bad
    /// `where` - is a warning and no hosts, never a failed call: a table that moved should cost the
    /// fleet its remote rows, not the server.
    fn hosts_from_file(&mut self, node: &KdlNode, home: Option<&str>) -> Vec<RemoteHost> {
        let mut path = None;
        let mut column = "hostname".to_owned();
        let mut alias_column = None;
        let mut filter = None;
        for entry in node.entries() {
            match (entry.name().map(|name| name.value()), entry.value()) {
                (None, KdlValue::String(value)) if path.is_none() => path = Some(value.clone()),
                (Some("column"), KdlValue::String(value)) => column = value.clone(),
                (Some("alias_column"), KdlValue::String(value)) => {
                    alias_column = Some(value.clone())
                },
                (Some("where"), KdlValue::String(value)) => match value.split_once('=') {
                    Some((key, wanted)) if !key.trim().is_empty() => {
                        filter = Some((key.trim().to_owned(), wanted.trim().to_owned()))
                    },
                    _ => {
                        self.warnings.push(format!(
                            "remote_hosts: from where={:?} is not column=value, so the file \
                             was not read",
                            value
                        ));
                        return Vec::new();
                    },
                },
                (Some(other), _) => self.warnings.push(format!(
                    "unknown remote_hosts from property {:?}, ignored (expected column, alias_column or \
                     where)",
                    other
                )),
                (None, value) => self.warnings.push(format!(
                    "remote_hosts: from takes one path, and {} is not it",
                    value
                )),
            }
        }
        let Some(path) = path else {
            self.warnings
                .push("remote_hosts: a from line with no path, ignored".to_owned());
            return Vec::new();
        };
        let expanded = match (path.strip_prefix('~'), home) {
            (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{}", home.trim_end_matches('/'), rest)
            },
            _ => path.clone(),
        };
        let text = match std::fs::read_to_string(&expanded) {
            Ok(text) => text,
            Err(e) => {
                self.warnings.push(format!(
                    "remote_hosts: from {} could not be read: {}",
                    expanded, e
                ));
                return Vec::new();
            },
        };
        let mut lines = text.lines().filter(|line| !line.trim().is_empty());
        let header: Vec<&str> = lines
            .next()
            .unwrap_or_default()
            .split('\t')
            .map(str::trim)
            .collect();
        let index_of = |name: &str| header.iter().position(|column| *column == name);
        let Some(host_at) = index_of(&column) else {
            self.warnings.push(format!(
                "remote_hosts: from {} has no column {:?}",
                expanded, column
            ));
            return Vec::new();
        };
        let filter_at = match &filter {
            Some((key, wanted)) => match index_of(key) {
                Some(at) => Some((at, wanted.clone())),
                None => {
                    self.warnings.push(format!(
                        "remote_hosts: from {} has no column {:?} for where",
                        expanded, key
                    ));
                    return Vec::new();
                },
            },
            None => None,
        };
        // a missing alias column costs the aliases, not the hosts
        let alias_at = match &alias_column {
            Some(name) => {
                let at = index_of(name);
                if at.is_none() {
                    self.warnings.push(format!(
                        "remote_hosts: from {} has no column {:?} for alias_column",
                        expanded, name
                    ));
                }
                at
            },
            None => None,
        };
        let mut hosts = Vec::new();
        for line in lines {
            let cells: Vec<&str> = line.split('\t').map(str::trim).collect();
            if let Some((at, wanted)) = &filter_at {
                if !cells
                    .get(*at)
                    .is_some_and(|cell| cell.eq_ignore_ascii_case(wanted))
                {
                    continue;
                }
            }
            let Some(name) = cells.get(host_at).filter(|name| !name.is_empty()) else {
                continue;
            };
            if let Some(reason) = host_name_error(name) {
                self.warnings.push(format!(
                    "remote_hosts: from {}: host {:?} {}, ignored",
                    expanded, name, reason
                ));
                continue;
            }
            // lower-cased: a table may record a hostname as the machine reports it (`BOXA`), and
            // ssh matches `Host` patterns in its config case-sensitively, so `BOXA` would miss
            // the `Host boxa` entry that carries the user and key
            let alias = alias_at
                .and_then(|at| cells.get(at))
                .filter(|alias| !alias.is_empty() && !alias.eq_ignore_ascii_case(name))
                .and_then(|alias| match host_name_error(alias) {
                    Some(reason) => {
                        self.warnings.push(format!(
                            "remote_hosts: from {}: alias {:?} {}, ignored",
                            expanded, alias, reason
                        ));
                        None
                    },
                    None => Some(alias.to_ascii_lowercase()),
                });
            hosts.push(RemoteHost {
                name: name.to_ascii_lowercase(),
                bin: None,
                alias,
                connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
            });
        }
        hosts
    }

    fn host_from_kdl(&mut self, node: &KdlNode) -> Option<RemoteHost> {
        let mut name = None;
        let mut bin = None;
        let mut alias = None;
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
                (Some("alias"), KdlValue::String(value)) => match host_name_error(value) {
                    Some(reason) => self.warnings.push(format!(
                        "remote_hosts: alias {:?} {}, ignored",
                        value, reason
                    )),
                    None => alias = Some(value.clone()),
                },
                (Some(other), _) => self.warnings.push(format!(
                    "unknown remote_hosts host property {:?}, ignored (expected bin or alias)",
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
        let alias = alias.filter(|alias| !alias.eq_ignore_ascii_case(&name));
        Some(RemoteHost {
            name,
            bin,
            alias,
            connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
        })
    }

    /// The listed host by this name or alias, ignoring case.
    pub fn find(&self, name: &str) -> Option<&RemoteHost> {
        self.named(name).or_else(|| {
            self.hosts.iter().find(|host| {
                host.alias
                    .as_deref()
                    .is_some_and(|alias| alias.eq_ignore_ascii_case(name))
            })
        })
    }

    /// The listed host a `host/session` names: by name or alias, or else the one listed host
    /// whose name, alias or first label starts or ends with it, ignoring case.
    ///
    /// Only for a word the caller wrote as a host. `find` stays exact, because `zellij_send` tries
    /// the first word of `to` as a host before it reads it as a session, and a session named like
    /// the start of a host must not be sent to another machine. Two matches, or none, are `None`,
    /// and the caller's refusal lists the hosts.
    pub fn find_loose(&self, name: &str) -> Option<&RemoteHost> {
        if let Some(host) = self.find(name) {
            return Some(host);
        }
        let wanted = name.to_ascii_lowercase();
        if wanted.is_empty() {
            return None;
        }
        let mut matches = self.hosts.iter().filter(|host| {
            let label = host
                .name
                .rsplit_once('@')
                .map_or(host.name.as_str(), |(_, rest)| rest)
                .split('.')
                .next()
                .unwrap_or_default();
            [Some(host.name.as_str()), host.alias.as_deref(), Some(label)]
                .into_iter()
                .flatten()
                .map(str::to_ascii_lowercase)
                .any(|word| word.starts_with(&wanted) || word.ends_with(&wanted))
        });
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }

    /// The listed host by its name alone, ignoring case.
    fn named(&self, name: &str) -> Option<&RemoteHost> {
        self.hosts
            .iter()
            .find(|host| host.name.eq_ignore_ascii_case(name))
    }

    /// The alias of the host a fleet answer calls `name`, if it has one.
    pub fn alias_of(&self, name: &str) -> Option<&str> {
        self.named(name)?.alias.as_deref()
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
    // an alias names its host, and a host that is this machine stays local by either name. A
    // host written short - the end or the start of one listed name - reaches that host when no
    // other listed host matches it too
    let listed = hosts.find_loose(host);
    let target = if is_local(host, local) || listed.is_some_and(|l| is_local(&l.name, local)) {
        Target::Local
    } else {
        match listed {
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
        let listed: Vec<String> = hosts
            .hosts
            .iter()
            .map(|host| match &host.alias {
                Some(alias) => format!("{} (alias {})", host.name, alias),
                None => host.name.clone(),
            })
            .collect();
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
    zellij_command_with(
        host,
        argv,
        zellij_utils::session_lifecycle::strict_version(),
    )
}

/// [`zellij_command`], with strict version mode given rather than read.
///
/// ssh does not carry the local environment (only what `SendEnv` names), so strict mode is set on
/// the remote side by the script itself. A variable, not `--strict-version`: an older remote
/// binary ignores an unknown variable but rejects an unknown flag.
pub fn zellij_command_with(host: &RemoteHost, argv: &[String], strict: bool) -> String {
    let mut args = vec![host.bin.clone().unwrap_or_default()];
    args.extend(argv.iter().cloned());
    if strict {
        let script = format!(
            "{}=1; export {}; {}",
            zellij_utils::session_lifecycle::STRICT_VERSION_ENV,
            zellij_utils::session_lifecycle::STRICT_VERSION_ENV,
            ZELLIJ_ON_REMOTE
        );
        remote_command(&script, &args)
    } else {
        remote_command(ZELLIJ_ON_REMOTE, &args)
    }
}

/// ssh's own argv, after the program: options, the host, and the one remote line.
///
/// Never interactive - `BatchMode` fails rather than asking for a password or a host key - and
/// never forwarding the agent or X. A connection is shared across calls for a minute when there is
/// a private directory for its socket, because a supervisor polling three hosts would otherwise pay
/// a handshake per host per poll.
pub fn ssh_argv(
    host: &str,
    remote_line: String,
    control_dir: Option<&Path>,
    connect_timeout_s: u64,
) -> Vec<String> {
    let connect_timeout = format!("ConnectTimeout={}", connect_timeout_s);
    let mut argv: Vec<String> = [
        "-T",
        "-o",
        "BatchMode=yes",
        "-o",
        connect_timeout.as_str(),
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

/// Run one remote line on `host`.
///
/// Exit 255 is ssh's own failure - the host did not answer, or refused the key - and says so,
/// since the remote CLI never exits 255 and the bare ssh message does not name the cause.
pub async fn run_line(
    host: &str,
    remote_line: String,
    connect_timeout_s: u64,
) -> Result<Outcome, String> {
    let program = ssh_program();
    let argv = ssh_argv(
        host,
        remote_line,
        control_dir().as_deref(),
        connect_timeout_s,
    );
    let output = tokio::process::Command::new(&program)
        .args(&argv)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("Could not run `{}` to reach {}: {}", program, host, e))?;
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
pub async fn run(host: &RemoteHost, argv: &[String]) -> Result<Outcome, String> {
    run_line(
        &host.name,
        zellij_command(host, argv),
        host.connect_timeout_s,
    )
    .await
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
                    bin: None,
                    alias: None,
                    connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
                },
                RemoteHost {
                    name: "boxb".into(),
                    bin: Some("/opt/homebrew/bin/zellij".into()),
                    alias: None,
                    connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
                },
                RemoteHost {
                    name: "Local-Box".into(),
                    bin: None,
                    alias: None,
                    connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
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
    fn the_timeouts_reach_every_host_and_a_bad_one_warns() {
        let hosts = RemoteHosts::parse(
            "remote_hosts {\n  connect_timeout \"9s\"\n  fleet_timeout \"40s\"\n  host \"boxa\"\n  host \"boxb\"\n}\n",
        );
        assert!(hosts.warnings.is_empty(), "{:?}", hosts.warnings);
        assert_eq!(hosts.timeouts.connect_s, 9);
        assert_eq!(hosts.timeouts.fleet_s, 40);
        assert!(hosts.hosts.iter().all(|host| host.connect_timeout_s == 9));
        let bad =
            RemoteHosts::parse("remote_hosts {\n  connect_timeout \"0\"\n  host \"boxa\"\n}\n");
        assert_eq!(bad.hosts.len(), 1);
        assert_eq!(bad.hosts[0].connect_timeout_s, 5);
        assert_eq!(bad.warnings.len(), 1, "{:?}", bad.warnings);
        // no block, or one without the keys: the old constants
        assert_eq!(RemoteHosts::parse(CONFIG).timeouts.fleet_s, 10);
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
                bin: Some("/opt/homebrew/bin/zellij".into()),
                alias: None,
                connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
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
    fn a_host_written_short_reaches_the_one_listed_host_it_matches() {
        let hosts = RemoteHosts::parse(
            r#"
            remote_hosts {
                host "labalpha"
                host "labbeta"
                host "user@gamma.example.net"
            }
        "#,
        );
        let route_to = |session: &str| {
            let mut arguments = json!({"session": session}).as_object().unwrap().clone();
            route(&mut arguments, &hosts, "local-box").map(|target| {
                (
                    target.host().map(str::to_owned),
                    arguments.get("session").cloned(),
                )
            })
        };
        // the end of one name, the start of one name, and a first label
        assert_eq!(
            route_to("alpha/work").unwrap(),
            (Some("labalpha".to_owned()), Some(json!("work")))
        );
        assert_eq!(route_to("BETA/").unwrap().0, Some("labbeta".to_owned()));
        assert_eq!(route_to("labb/w").unwrap().0, Some("labbeta".to_owned()));
        assert_eq!(
            route_to("gam/w").unwrap().0,
            Some("user@gamma.example.net".to_owned())
        );
        // two matches, or none, keep the refusal that lists the hosts
        for session in ["lab/work", "delta/work"] {
            let refusal = route_to(session).unwrap_err();
            assert!(refusal.contains("is not in remote_hosts"), "{}", refusal);
            assert!(refusal.contains("labalpha, labbeta"), "{}", refusal);
        }
        // `find` stays exact: zellij_send reads a first word that is no host as a session
        assert!(hosts.find("alpha").is_none());
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
            alias: None,
            connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
        };
        let argv: Vec<String> = ["-s", "my work", "action", "write-chars", "--", "it's $x"]
            .iter()
            .map(|w| (*w).to_owned())
            .collect();
        let line = zellij_command_with(&host, &argv, false);
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
            alias: None,
            connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
        };
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(zellij_command_with(&with_bin, &argv, false))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "-s|my work|action|write-chars|--|it's $x|"
        );
        // and the probe finds it on PATH when no bin is configured
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(zellij_command_with(&host, &argv, false))
            .env("PATH", format!("{}:/usr/bin:/bin", dir.path().display()))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "-s|my work|action|write-chars|--|it's $x|"
        );
    }

    /// ssh does not carry the local environment, so strict mode crosses it inside the remote
    /// script, as a variable the remote zellij reads - and only when it is on here.
    #[test]
    fn strict_version_mode_reaches_the_remote_zellij_as_its_variable() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("zellij");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '%s|%s' \"${ZELLIJ_STRICT_VERSION-unset}\" \"$*\"\n",
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let host = RemoteHost {
            name: "boxa".into(),
            bin: Some(fake.display().to_string()),
            alias: None,
            connect_timeout_s: zellij_utils::remote_timeouts::DEFAULT_CONNECT_TIMEOUT_S,
        };
        let argv = vec!["action".to_owned(), "list-panes".to_owned()];
        let run = |strict: bool| {
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(zellij_command_with(&host, &argv, strict))
                .env_remove("ZELLIJ_STRICT_VERSION")
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        };
        assert_eq!(run(true), "1|action list-panes");
        assert_eq!(run(false), "unset|action list-panes");
    }

    #[test]
    fn ssh_is_never_interactive_and_the_host_is_never_an_option() {
        let argv = ssh_argv(
            "boxa",
            "sh -c 'x'".into(),
            Some(Path::new("/tmp/zellij-mcp-1000")),
            5,
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
        let unshared = ssh_argv("boxa", "x".into(), None, 12);
        assert!(unshared.iter().any(|w| w == "ControlMaster=no"));
        assert!(unshared.iter().any(|w| w == "ConnectTimeout=12"));
        assert!(!unshared.iter().any(|w| w.starts_with("ControlPath")));
    }

    /// A home directory holding a machine table, the way the shared one is laid out.
    fn table_home() -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".config")).unwrap();
        std::fs::write(
            home.path().join(".config/machines.tsv"),
            "hostname\tfriendly\tzellij_peer\n\
             boxa\tlinux box\tyes\n\
             BOXB\tmac\tYES\n\
             boxc\twork mac\tno\n\
             \n",
        )
        .unwrap();
        home
    }

    fn from_config(from: &str, home: &Path) -> RemoteHosts {
        RemoteHosts::parse_in(
            &format!("remote_hosts {{\n{}\n}}\n", from),
            Some(&home.display().to_string()),
        )
    }

    fn names(hosts: &RemoteHosts) -> Vec<&str> {
        hosts.hosts.iter().map(|host| host.name.as_str()).collect()
    }

    #[test]
    fn a_from_line_reads_the_hosts_out_of_a_table() {
        let home = table_home();
        let all = from_config(r#"from "~/.config/machines.tsv""#, home.path());
        assert_eq!(all.warnings, Vec::<String>::new());
        assert_eq!(names(&all), vec!["boxa", "boxb", "boxc"]);
        let peers = from_config(
            r#"from "~/.config/machines.tsv" column="hostname" where="zellij_peer=yes""#,
            home.path(),
        );
        assert_eq!(
            names(&peers),
            vec!["boxa", "boxb"],
            "the value matches in any case"
        );
        let by_name = from_config(
            r#"from "~/.config/machines.tsv" column="friendly" where="zellij_peer=no""#,
            home.path(),
        );
        assert_eq!(by_name.warnings.len(), 1, "{:?}", by_name.warnings);
        assert!(by_name.hosts.is_empty(), "`work mac` is not a host name");
    }

    #[test]
    fn a_table_that_cannot_be_read_warns_and_adds_nothing() {
        let home = table_home();
        let missing = from_config("from \"~/.config/gone.tsv\"\nhost \"boxz\"", home.path());
        assert_eq!(names(&missing), vec!["boxz"], "the inline host still loads");
        assert!(
            missing.warnings[0].contains("could not be read"),
            "{:?}",
            missing.warnings
        );
        for (from, said) in [
            (
                r#"from "~/.config/machines.tsv" column="ssh""#,
                "no column \"ssh\"",
            ),
            (
                r#"from "~/.config/machines.tsv" where="peer=yes""#,
                "no column \"peer\" for where",
            ),
            (
                r#"from "~/.config/machines.tsv" where="yes""#,
                "is not column=value",
            ),
        ] {
            let hosts = from_config(from, home.path());
            assert!(hosts.hosts.is_empty(), "{}", from);
            assert!(
                hosts.warnings.iter().any(|w| w.contains(said)),
                "{} gave {:?}",
                from,
                hosts.warnings
            );
        }
    }

    #[test]
    fn an_inline_host_overrides_its_table_row_and_the_rest_are_added() {
        let home = table_home();
        let hosts = from_config(
            "host \"boxd\"\n\
             from \"~/.config/machines.tsv\" where=\"zellij_peer=yes\"\n\
             host \"boxb\" bin=\"/opt/homebrew/bin/zellij\"",
            home.path(),
        );
        assert_eq!(hosts.warnings, Vec::<String>::new());
        assert_eq!(names(&hosts), vec!["boxa", "boxb", "boxd"]);
        assert_eq!(
            hosts.hosts[1].bin.as_deref(),
            Some("/opt/homebrew/bin/zellij")
        );
    }

    #[test]
    fn an_uppercase_table_row_for_this_machine_is_skipped_as_local() {
        let home = table_home();
        let hosts = from_config(
            r#"from "~/.config/machines.tsv" where="zellij_peer=yes""#,
            home.path(),
        );
        let remote: Vec<&str> = hosts
            .remote("boxb.local")
            .iter()
            .map(|host| host.name.as_str())
            .collect();
        assert_eq!(remote, vec!["boxa"]);
        assert_eq!(
            hosts.local_name("BOXB"),
            "boxb",
            "a table name is lower-cased for ssh"
        );
        let mut arguments = json!({"session": "boxb/work"}).as_object().unwrap().clone();
        assert_eq!(
            route(&mut arguments, &hosts, "BOXB.local").unwrap(),
            Target::Local
        );
    }

    #[test]
    fn an_alias_reaches_its_host_and_ssh_is_given_the_full_name() {
        let hosts = RemoteHosts::parse(
            r#"remote_hosts {
                host "boxa.example.net" alias="a"
                host "boxb"
                host "boxc" alias="BOXB"
                host "boxd" alias="-x"
                host "boxe" alias="a"
            }"#,
        );
        assert_eq!(hosts.hosts[0].alias.as_deref(), Some("a"));
        assert_eq!(hosts.alias_of("boxa.example.net"), Some("a"));
        // an alias that names another host, or one already taken, or not a host name, is dropped
        assert_eq!(hosts.hosts[2].alias, None);
        assert_eq!(hosts.hosts[3].alias, None);
        assert_eq!(hosts.hosts[4].alias, None);
        assert_eq!(
            hosts.hosts[0].alias.as_deref(),
            Some("a"),
            "the first keeps it"
        );
        let said = hosts.warnings.join("\n");
        assert!(
            said.contains("\"BOXB\" of \"boxc\" names another host"),
            "{}",
            said
        );
        assert!(said.contains("alias \"-x\" begins with a dash"), "{}", said);
        let mut arguments = json!({"session": "A/work"}).as_object().unwrap().clone();
        let target = route(&mut arguments, &hosts, "here").unwrap();
        assert_eq!(target.host(), Some("boxa.example.net"));
        assert_eq!(arguments["session"], json!("work"));
        // an alias of this machine stays local
        let mut arguments = json!({"session": "a/work"}).as_object().unwrap().clone();
        assert_eq!(
            route(&mut arguments, &hosts, "boxa").unwrap(),
            Target::Local
        );
        // a refusal lists the aliases beside the names
        let mut arguments = json!({"session": "zz/work"}).as_object().unwrap().clone();
        let refused = route(&mut arguments, &hosts, "here").unwrap_err();
        assert!(
            refused.contains("boxa.example.net (alias a)"),
            "{}",
            refused
        );
    }

    #[test]
    fn a_from_line_takes_its_aliases_from_a_column() {
        let home = tempfile::tempdir().unwrap();
        let table = home.path().join("m.tsv");
        std::fs::write(
            &table,
            "hostname\tshort\tpeer\n\
             boxa.example.net\tA\tyes\n\
             boxb\t\tyes\n\
             boxc.example.net\tbad name\tyes\n",
        )
        .unwrap();
        let hosts = from_config(
            &format!(
                "from \"{}\" alias_column=\"short\" where=\"peer=yes\"\nhost \"boxa.example.net\" bin=\"/bin/zellij\"",
                table.display()
            ),
            home.path(),
        );
        assert_eq!(
            names(&hosts),
            vec!["boxa.example.net", "boxb", "boxc.example.net"]
        );
        // lower-cased like the name, and kept when an inline line overrides the row
        assert_eq!(hosts.hosts[0].alias.as_deref(), Some("a"));
        assert_eq!(hosts.hosts[0].bin.as_deref(), Some("/bin/zellij"));
        assert_eq!(hosts.hosts[1].alias, None);
        assert_eq!(hosts.hosts[2].alias, None);
        assert!(
            hosts.warnings[0].contains("alias \"bad name\""),
            "{:?}",
            hosts.warnings
        );
        assert_eq!(
            hosts.find("A").map(|h| h.name.as_str()),
            Some("boxa.example.net")
        );
        // a column the header lacks costs the aliases, not the hosts
        let hosts = from_config(
            &format!("from \"{}\" alias_column=\"nick\"", table.display()),
            home.path(),
        );
        assert_eq!(hosts.hosts.len(), 3);
        assert!(hosts.hosts.iter().all(|host| host.alias.is_none()));
        assert!(hosts.warnings[0].contains("no column \"nick\" for alias_column"));
    }
}
