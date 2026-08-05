//! Init-system units that keep a session up.
//!
//! A generated unit is a DUMB SCHEDULER: it names the binary, the session, and when to try. It
//! holds no opinion about the environment, because every opinion a launcher held about the
//! environment was eventually wrong in a way nothing could see - a session born under a launchd
//! `TMPDIR` or a stale `ZELLIJ_SOCKET_DIR` is not a misplaced session but an invisible one. The
//! binary resolves its own socket directory and asserts the result, so there is nothing left for a
//! unit file to get right.
//!
//! The division is: supervision (when to run, what to do about failure) belongs to the init
//! system, session correctness belongs to `zellij session up`.
//!
//! Installing a unit is part of the same job, so it lives here too: `zellij session
//! enable|disable|status` writes the file this module renders, hands it to the init system, and
//! takes it back again. The removal half is the half worth having - a launchd job outlives the
//! plist that created it, so deleting the file by hand leaves a job still running from a
//! definition that is gone.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The init systems `zellij setup --generate-service` can write for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Systemd,
    Launchd,
}

impl ServiceKind {
    pub fn from_name(name: &str) -> Option<ServiceKind> {
        match name.to_lowercase().as_str() {
            "systemd" => Some(ServiceKind::Systemd),
            "launchd" => Some(ServiceKind::Launchd),
            _ => None,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            ServiceKind::Systemd => "systemd (user)",
            ServiceKind::Launchd => "launchd (user)",
        }
    }
}

/// Which binary path a unit should exec, and how it was arrived at.
///
/// Neither init system looks anything up on PATH, so the unit needs an absolute path - and WHICH
/// absolute path matters more than it looks. `current_exe` resolves symlinks, and a package manager
/// that installs into a versioned prefix keeps the stable name on PATH as a symlink into the
/// version currently installed. Writing the resolved path into a unit therefore writes down a
/// directory that the next upgrade deletes, and the agent stops working with nothing to show for
/// it. On macOS it is worse than a broken path: permission grants are recorded against the binary
/// launchd started, so a version in the path means the identity changes at every upgrade and the
/// grants stop applying to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceExe {
    /// A path the user named. Only they know when their installation is unusual.
    Given(PathBuf),
    /// A name on PATH that resolves to this binary - the stable half of a versioned install.
    Stable(PathBuf),
    /// The resolved path of the running binary, nothing steadier having been found.
    Resolved(PathBuf),
}

impl ServiceExe {
    pub fn path(&self) -> &Path {
        match self {
            ServiceExe::Given(path) | ServiceExe::Stable(path) | ServiceExe::Resolved(path) => path,
        }
    }
}

/// Pick the path to write into a unit: what the user said, else the stable name that leads here,
/// else where this binary actually is.
///
/// A PATH entry counts only if it resolves to the SAME file as the running binary - another
/// zellij, further along the same PATH, is a different program and a unit that execs it is a unit
/// that keeps the wrong version alive.
pub fn resolve_service_exe(
    explicit: Option<PathBuf>,
    current_exe: &Path,
    path_dirs: &[PathBuf],
) -> ServiceExe {
    if let Some(explicit) = explicit {
        return ServiceExe::Given(explicit);
    }
    let resolved = current_exe
        .canonicalize()
        .unwrap_or_else(|_| current_exe.to_path_buf());
    let name = resolved.file_name().unwrap_or_else(|| "zellij".as_ref());
    for dir in path_dirs {
        let candidate = dir.join(name);
        if candidate.canonicalize().ok().as_deref() == Some(resolved.as_path()) {
            return ServiceExe::Stable(candidate);
        }
    }
    ServiceExe::Resolved(resolved)
}

/// The directories of the PATH variable, in the order they are searched.
pub fn path_dirs() -> Vec<PathBuf> {
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default()
}

/// The launchd label of the agent that keeps `session` up.
///
/// One label per session name, because one label is one job: a fixed label would mean the second
/// session's plist replaced the first one's, and `zellij session up` would have no way to ask
/// launchd for the job that belongs to the name it was given.
pub fn launchd_label(session: &str) -> String {
    format!("dev.zellij.session.{}", session)
}

/// An installed job, reduced to the two things that identify it: the name the init system knows it
/// by, and the command it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledJob {
    /// The launchd label, or the systemd unit name.
    pub name: String,
    /// The file it was read from.
    pub path: PathBuf,
    /// The arguments it execs.
    pub exec: Vec<String>,
}

/// Which installed job keeps a session up, judged by what it RUNS and not by what it is called.
///
/// [`launchd_label`] and [`systemd_service_name`] are this build's naming convention and nobody
/// else's. A job installed by hand, by an earlier build, or by a dotfiles repository older than
/// these commands does the same work under whatever name its author chose, and a lookup by the
/// derived name calls it absent. That is worse than a cosmetic miss: the macOS domain guard exists
/// to stop a permanently crippled session being created, and a guard that cannot see the job falls
/// through to creating one - for everybody whose agent it did not install itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionJob<'a> {
    /// Nothing installed runs `session up` for this session.
    NotInstalled,
    /// A job runs it, under the name this build would have installed.
    Installed(&'a InstalledJob),
    /// A job runs it under another name. It is still the job; the caller is to say which.
    InstalledAs(&'a InstalledJob),
    /// Several run it and none carries this build's name. All of them are carried, so that the one
    /// chosen from them is not chosen silently.
    Ambiguous(Vec<&'a InstalledJob>),
}

impl<'a> SessionJob<'a> {
    /// The job to act on, where there is one.
    ///
    /// With several it is the first by name: arbitrary, but stable between runs. Every one of them
    /// runs the same command, so any of them does the work - what matters is that the caller names
    /// the others rather than pretending there was nothing to choose.
    pub fn job(&self) -> Option<&'a InstalledJob> {
        match self {
            SessionJob::NotInstalled => None,
            SessionJob::Installed(job) | SessionJob::InstalledAs(job) => Some(job),
            SessionJob::Ambiguous(jobs) => jobs.first().copied(),
        }
    }

    /// The job found under a name this build would not have written, where that is what happened.
    pub fn renamed(&self) -> Option<&'a InstalledJob> {
        match self {
            SessionJob::NotInstalled | SessionJob::Installed(_) => None,
            _ => self.job(),
        }
    }

    /// Every job that runs the command, under a name this build would not have written.
    pub fn renamed_jobs(&self) -> Vec<&'a InstalledJob> {
        match self {
            SessionJob::NotInstalled | SessionJob::Installed(_) => Vec::new(),
            SessionJob::InstalledAs(job) => vec![job],
            SessionJob::Ambiguous(jobs) => jobs.clone(),
        }
    }
}

/// Find the job that keeps `session` up among `jobs`, preferring the one named `derived_name`.
///
/// The exact name wins when it is there, so an install this build wrote is never reported as an
/// oddity, and the ambiguity that remains is real ambiguity.
pub fn find_session_job<'a>(
    jobs: &'a [InstalledJob],
    session: &str,
    derived_name: &str,
) -> SessionJob<'a> {
    let mut matched: Vec<&InstalledJob> = jobs
        .iter()
        .filter(|job| session_up_target(&job.exec) == Some(session))
        .collect();
    if let Some(exact) = matched.iter().copied().find(|job| job.name == derived_name) {
        return SessionJob::Installed(exact);
    }
    matched.sort_by(|one, other| one.name.cmp(&other.name));
    match matched.len() {
        0 => SessionJob::NotInstalled,
        1 => SessionJob::InstalledAs(matched[0]),
        _ => SessionJob::Ambiguous(matched),
    }
}

/// Options of `session up` that take a value, so that the value is not read as the session name.
const SESSION_UP_VALUE_FLAGS: &[&str] = &["--restore"];

/// The session a command line brings up, if that is what it does.
///
/// argv[0] is not looked at. A unit may exec zellij directly or hand it to a wrapper - a launcher,
/// an environment shim, a scheduling helper - with the zellij path several arguments in, and the
/// binary itself need not be called "zellij": a renamed or symlinked build is the same program.
/// What identifies the job is the subcommand it runs, so that is what is matched.
///
/// Two passes, because the subcommand is not always two arguments of the job's own argv. The
/// commonest hand-written agent runs a shell - `["/bin/sh", "-c", "exec zellij session up
/// my-session"]` - and there the whole command line is ONE argument, which is exactly the
/// population this scan exists for: an agent written before these subcommands existed could not
/// have called them, so it calls something that calls them. The second pass reads inside each
/// argument.
pub fn session_up_target(exec: &[String]) -> Option<&str> {
    if let Some(session) = session_up_in_argv(exec) {
        return Some(session);
    }
    exec.iter().find_map(|argument| session_up_inside(argument))
}

/// `session up <name>` spread across separate argv elements, which is what a unit that execs zellij
/// directly produces - and what this build writes.
fn session_up_in_argv(exec: &[String]) -> Option<&str> {
    let up = exec
        .windows(2)
        .position(|pair| pair[0] == "session" && pair[1] == "up")?;
    let mut rest = exec[up + 2..].iter();
    while let Some(argument) = rest.next() {
        if argument.starts_with('-') {
            // `--restore <id>` carries a value that is not the session name
            if SESSION_UP_VALUE_FLAGS.contains(&argument.as_str()) {
                rest.next();
            }
            continue;
        }
        return Some(argument);
    }
    // `session up` with no name takes it from the config, which this cannot read on the unit's
    // behalf - so it names no session here rather than guessing at one
    None
}

/// `session up <name>` written inside ONE argument, as a `sh -c` job writes it.
///
/// The words are read the way a shell reads them, quotes included, so a session name with a space
/// in it survives. What is not attempted is shell semantics: a name built from a variable, or a
/// command that reaches `session up` only through a script this cannot see, is not found here and
/// is not meant to be - see the caller's warning, which says a job may run something unreadable
/// rather than claiming none exists.
fn session_up_inside(argument: &str) -> Option<&str> {
    let words = word_spans(argument);
    let word = |span: &(usize, usize)| &argument[span.0..span.1];
    let up = words
        .windows(2)
        .position(|pair| word(&pair[0]) == "session" && word(&pair[1]) == "up")?;
    let mut rest = words[up + 2..].iter();
    while let Some(span) = rest.next() {
        let candidate = word(span);
        if candidate.starts_with('-') {
            if SESSION_UP_VALUE_FLAGS.contains(&candidate) {
                rest.next();
            }
            continue;
        }
        return Some(candidate);
    }
    None
}

/// Where each word of a command line begins and ends, with the quotes around a word left out of the
/// span. A byte offset is only ever taken at ASCII whitespace or an ASCII quote, so every span it
/// produces is a character boundary.
fn word_spans(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index].is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if bytes[index] == b'"' || bytes[index] == b'\'' {
            let quote = bytes[index];
            let start = index + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end] != quote {
                end += 1;
            }
            spans.push((start, end));
            index = (end + 1).min(bytes.len());
            continue;
        }
        let start = index;
        while index < bytes.len()
            && !bytes[index].is_ascii_whitespace()
            && bytes[index] != b'"'
            && bytes[index] != b'\''
        {
            index += 1;
        }
        spans.push((start, index));
    }
    spans
}

/// `Label` and `ProgramArguments` out of a launch agent plist.
///
/// Two keys of a known shape, out of a file format that is XML. A whole plist parser, and the
/// dependency it arrives with, buys nothing for that. What this does not read is a plist saved in
/// the binary format - [`read_installed_job`] converts one first.
pub fn parse_launch_agent(xml: &str) -> Option<(String, Vec<String>)> {
    let label = first_string(value_after_key(xml, "Label")?)?;
    let arguments = string_array(value_after_key(xml, "ProgramArguments")?)?;
    Some((label, arguments))
}

/// What follows `<key>NAME</key>`, which is that key's value.
fn value_after_key<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let tag = format!("<key>{}</key>", key);
    let start = xml.find(&tag)? + tag.len();
    Some(&xml[start..])
}

/// The `<string>` a value begins with. It has to BEGIN with one: a value of another type followed
/// by an unrelated string later in the dictionary is not this key's value.
fn first_string(value: &str) -> Option<String> {
    let value = value.trim_start().strip_prefix("<string>")?;
    let end = value.find("</string>")?;
    Some(xml_unescape(&value[..end]))
}

fn string_array(value: &str) -> Option<Vec<String>> {
    let value = value.trim_start().strip_prefix("<array>")?;
    let end = value.find("</array>")?;
    let mut strings = Vec::new();
    let mut rest = &value[..end];
    while let Some(start) = rest.find("<string>") {
        rest = &rest[start + "<string>".len()..];
        let end = rest.find("</string>")?;
        strings.push(xml_unescape(&rest[..end]));
        rest = &rest[end..];
    }
    Some(strings)
}

/// The inverse of [`xml_escape`]. An unrecognised entity is left alone rather than dropped: a
/// literal that this does not know is still closer to the truth than nothing.
fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        // last, so that an escaped entity such as `&amp;lt;` does not become a tag
        .replace("&amp;", "&")
}

/// The `ExecStart` of a systemd unit, split into arguments.
///
/// `ExecStartPre` and `ExecStartPost` are other keys and are not it, and a commented-out line is
/// not a directive - both are the ordinary ways a naive substring search reads a unit wrongly.
pub fn parse_unit_exec_start(unit: &str) -> Option<Vec<String>> {
    for line in unit.lines() {
        let line = line.trim();
        if line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let Some((key, command)) = line.split_once('=') else {
            continue;
        };
        if key.trim() != "ExecStart" {
            continue;
        }
        // systemd's prefixes on a command: ignore failure, run as root, and so on. None of them
        // change which command it is.
        let command = command.trim_start_matches(['-', '+', '!', '@', ':']);
        return Some(split_command_line(command));
    }
    None
}

/// Split a command line the way both unit formats quote one: whitespace separates arguments, and
/// quotes hold one together.
fn split_command_line(line: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut started = false;
    let mut quote = None;
    for character in line.chars() {
        match (quote, character) {
            (Some(open), character) if character == open => quote = None,
            (Some(_), character) => current.push(character),
            (None, '"') | (None, '\'') => {
                quote = Some(character);
                started = true;
            },
            (None, character) if character.is_whitespace() => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            },
            (None, character) => {
                current.push(character);
                started = true;
            },
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Every job installed for this user that this build can read, whatever it is called.
///
/// The files are read rather than the init system asked, and for launchd that is the deliberate
/// choice: `launchctl list` gives labels with no command line, so the command would have to be
/// fetched with one `launchctl print` per label - hundreds of subprocesses, over output whose
/// format is undocumented and differs between releases. A plist holds both keys in a documented
/// format that has not changed. Whether the init system currently HOLDS a job is a separate
/// question, asked by name afterwards, so nothing here depends on the file being the whole truth.
///
/// Compiled under `cfg(test)` on every unix so the macOS path cannot rot on a machine that never
/// builds it.
#[cfg(any(target_os = "macos", all(unix, test)))]
pub fn installed_launch_agents() -> Vec<InstalledJob> {
    installed_jobs(ServiceKind::Launchd, "plist")
}

/// The systemd user units installed for this user.
#[cfg(target_os = "linux")]
pub fn installed_user_units() -> Vec<InstalledJob> {
    installed_jobs(ServiceKind::Systemd, "service")
}

#[cfg(any(target_os = "linux", target_os = "macos", all(unix, test)))]
fn installed_jobs(kind: ServiceKind, extension: &str) -> Vec<InstalledJob> {
    let Ok(dir) = service_dir(kind) else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        // no directory is not a fault: a machine with nothing installed has nothing to enumerate
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some(extension) {
            continue;
        }
        if let Some(job) = read_installed_job(kind, &path) {
            jobs.push(job);
        }
    }
    jobs
}

/// One job read off disk, or nothing if the file is not one this can make sense of.
#[cfg(any(target_os = "linux", target_os = "macos", all(unix, test)))]
fn read_installed_job(kind: ServiceKind, path: &Path) -> Option<InstalledJob> {
    match kind {
        ServiceKind::Systemd => {
            let unit = std::fs::read_to_string(path).ok()?;
            Some(InstalledJob {
                name: path.file_name()?.to_str()?.to_owned(),
                path: path.to_path_buf(),
                exec: parse_unit_exec_start(&unit)?,
            })
        },
        ServiceKind::Launchd => {
            let xml = launch_agent_xml(path)?;
            let (label, exec) = parse_launch_agent(&xml)?;
            Some(InstalledJob {
                name: label,
                path: path.to_path_buf(),
                exec,
            })
        },
    }
}

/// A launch agent as XML, converting it first if it was saved in the binary plist format.
#[cfg(any(target_os = "linux", target_os = "macos", all(unix, test)))]
fn launch_agent_xml(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if !bytes.starts_with(b"bplist") {
        return String::from_utf8(bytes).ok();
    }
    let converted = std::process::Command::new("plutil")
        .args(["-convert", "xml1", "-o", "-"])
        .arg(path)
        .output()
        .ok()?;
    converted
        .status
        .success()
        .then(|| String::from_utf8(converted.stdout).ok())
        .flatten()
}

/// Local facts a generated unit cannot know: that this session must start after some other
/// service, that it wants a particular nice level, that launchd should treat it as interactive.
///
/// RAW PASSTHROUGH, deliberately. zellij does not model systemd's schema or launchd's - it places
/// what it is given in the right section of the right file and validates only what it must, which
/// is that the entry does not overwrite the part of the unit the generator is responsible for.
/// Modelling the schemas would be a second, worse copy of two specifications that already exist,
/// and it would reject every key added to them after this was written.
///
/// This lives in the config rather than in a systemd drop-in directory because a drop-in is
/// invisible to the tool that generated the unit: `zellij session status` could not report it, and
/// someone reading the config would have no idea it existed. Configuration a tool generates from
/// belongs where the tool can see it.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionServiceOptions {
    #[serde(default)]
    pub systemd: SystemdDirectives,
    #[serde(default)]
    pub launchd: Vec<LaunchdKey>,
}

/// Literal directive lines, per section of the generated `.service` file.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemdDirectives {
    #[serde(default)]
    pub unit: Vec<String>,
    #[serde(default)]
    pub service: Vec<String>,
    #[serde(default)]
    pub install: Vec<String>,
}

/// One key of the generated plist. Strings, integers and booleans cover every key anyone has
/// wanted here; a plist can hold arrays and dictionaries too, and when someone needs one they can
/// be added without changing anything else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchdKey {
    pub name: String,
    pub value: PlistValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlistValue {
    String(String),
    Integer(i64),
    Bool(bool),
}

/// The plist keys the generator writes itself.
///
/// A dictionary with the same key twice is not a plist, so an extra key that is one of these is
/// not an override but a broken file - and every one of them is load-bearing: the label is the
/// job's identity, `ProgramArguments` is the command, `EnvironmentVariables` is where a TMPDIR
/// would go, and the two scheduling keys are what makes the job a watchdog.
const GENERATED_LAUNCHD_KEYS: &[&str] = &[
    "Label",
    "ProgramArguments",
    "LimitLoadToSessionType",
    "EnvironmentVariables",
    "RunAtLoad",
    "StartInterval",
];

/// Variables a unit must never pin. The whole design rests on the binary resolving its own socket
/// directory: a unit that sets either of these builds a session no login shell can see, and the
/// failure is invisible rather than loud. See this module's own documentation.
const FORBIDDEN_ENV_NAMES: &[&str] = &["TMPDIR", "ZELLIJ_SOCKET_DIR"];

impl SessionServiceOptions {
    pub fn is_empty(&self) -> bool {
        self.systemd.unit.is_empty()
            && self.systemd.service.is_empty()
            && self.systemd.install.is_empty()
            && self.launchd.is_empty()
    }

    /// Add a literal directive line to one section of the generated service file.
    ///
    /// The error names the offending entry, because the person reading it is looking at a config
    /// file and needs to know which line of it to change.
    pub fn add_systemd_directive(&mut self, section: &str, directive: &str) -> Result<(), String> {
        let target = match section {
            "unit" => &mut self.systemd.unit,
            "service" => &mut self.systemd.service,
            "install" => &mut self.systemd.install,
            other => {
                return Err(format!(
                    "unknown systemd section '{}' (expected unit, service or install)",
                    other
                ))
            },
        };
        let key = directive.split('=').next().unwrap_or("").trim();
        if key.is_empty() || !directive.contains('=') {
            return Err(format!(
                "'{}' is not a systemd directive (expected `Key=value`)",
                directive
            ));
        }
        // ExecStart IS the unit: what it runs, against which session, from which binary path. An
        // extra one appends for Type=oneshot rather than replacing, which is worse than an
        // override - the session would be brought up and then something else would run as well.
        if key.eq_ignore_ascii_case("ExecStart") {
            return Err(format!(
                "'{}' sets ExecStart, which `zellij session up` owns - the unit runs that command \
                 and nothing else",
                directive
            ));
        }
        if let Some(name) = forbidden_systemd_assignment(directive) {
            return Err(format!(
                "'{}' sets {}, which would build a session no terminal can see - the binary \
                 resolves that itself",
                directive, name
            ));
        }
        target.push(directive.to_owned());
        Ok(())
    }

    /// Add a key to the generated plist.
    pub fn add_launchd_key(&mut self, name: &str, value: PlistValue) -> Result<(), String> {
        if let Some(generated) = GENERATED_LAUNCHD_KEYS
            .iter()
            .find(|generated| generated.eq_ignore_ascii_case(name))
        {
            return Err(format!(
                "'{}' is written by zellij itself; a plist cannot carry the same key twice",
                generated
            ));
        }
        if let Some(forbidden) = forbidden_env_name(name).or_else(|| match &value {
            PlistValue::String(value) => forbidden_env_name(value),
            _ => None,
        }) {
            return Err(format!(
                "'{}' names {}, which would build a session no terminal can see - the binary \
                 resolves that itself",
                name, forbidden
            ));
        }
        self.launchd.push(LaunchdKey {
            name: name.to_owned(),
            value,
        });
        Ok(())
    }
}

/// Which forbidden variable a systemd directive SETS, if it sets one.
///
/// A mention is not an assignment, and the difference is not academic: `UnsetEnvironment=ZELLIJ
/// ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID ZELLIJ_SOCKET_DIR` is a unit doing exactly what this guard
/// wants, and reading it as a violation refused it - at KDL parse time, which fails the WHOLE
/// config, so `setup --check` and every other command fail with it, not only `session enable`.
///
/// So the directives are read the way systemd reads them:
///
/// - `Environment=` / `DefaultEnvironment=` carry `NAME=value` words, and it is the NAME that has
///   to be looked at. Words are separated by whitespace and may be quoted.
/// - `PassEnvironment=` names variables to import from the manager's environment, which puts the
///   value into the unit as surely as an assignment does.
/// - `EnvironmentFile=` is opaque - the file could set anything and cannot be read from here - so
///   it stays as strict as it was: naming a forbidden variable at all is refused.
/// - Everything else, `UnsetEnvironment=` included, may name what it likes.
fn forbidden_systemd_assignment(directive: &str) -> Option<&'static str> {
    let (key, value) = directive.split_once('=')?;
    let key = key.trim();
    if key.eq_ignore_ascii_case("EnvironmentFile") {
        return forbidden_env_name(value);
    }
    let names_variables = key.eq_ignore_ascii_case("PassEnvironment");
    if !names_variables
        && !key.eq_ignore_ascii_case("Environment")
        && !key.eq_ignore_ascii_case("DefaultEnvironment")
    {
        return None;
    }
    value
        .split_whitespace()
        .map(|word| word.trim_matches(['"', '\'']))
        .find_map(|word| {
            let assigned = if names_variables {
                word
            } else {
                word.split('=').next()?
            };
            FORBIDDEN_ENV_NAMES
                .iter()
                .find(|name| **name == assigned)
                .copied()
        })
}

fn forbidden_env_name(text: &str) -> Option<&'static str> {
    FORBIDDEN_ENV_NAMES
        .iter()
        .find(|name| text.contains(*name))
        .copied()
}

/// Render the unit for `kind`, running `exe` against `session`, with whatever the config adds.
pub fn service_unit(
    kind: ServiceKind,
    exe: &Path,
    session: &str,
    extras: Option<&SessionServiceOptions>,
) -> String {
    match kind {
        ServiceKind::Systemd => systemd_unit(exe, session, extras),
        ServiceKind::Launchd => launchd_plist(exe, session, extras),
    }
}

/// Directive lines as they go into a unit file: one per line, or nothing at all.
///
/// Nothing at all has to be an empty string rather than a blank line, so that a config with no
/// extras produces the same bytes as a build that had never heard of them.
fn directive_lines(lines: &[String]) -> String {
    lines
        .iter()
        .map(|line| format!("{}\n", line))
        .collect::<String>()
}

/// A plist value, XML-escaped, at the indentation the generated dict uses.
fn plist_entries(keys: &[LaunchdKey]) -> String {
    keys.iter()
        .map(|key| {
            let value = match &key.value {
                PlistValue::String(value) => format!("<string>{}</string>", xml_escape(value)),
                PlistValue::Integer(value) => format!("<integer>{}</integer>", value),
                PlistValue::Bool(true) => "<true/>".to_owned(),
                PlistValue::Bool(false) => "<false/>".to_owned(),
            };
            format!("    <key>{}</key>\n    {}\n", xml_escape(&key.name), value)
        })
        .collect::<String>()
}

/// Escape for XML character data. A plist is XML, and a value carrying an ampersand or an angle
/// bracket would otherwise produce a file launchd refuses to parse - reported, if at all, as a job
/// that simply never loads.
fn xml_escape(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

/// How often the scheduler re-runs `session up`. The command is idempotent, so a pass over a
/// healthy session is a no-op and a pass over a missing one restores it.
const CHECK_INTERVAL_SECS: u64 = 60;

/// Whether the config already sets this variable for the service.
///
/// systemd takes the last assignment of a variable, so a second `Environment=NAME=` line would not
/// break anything - but two of them in one unit is a thing to read twice and get wrong, and the
/// generator's default is the one to drop.
fn sets_env_var(directives: &[String], name: &str) -> bool {
    let assignment = format!("{}=", name);
    directives.iter().any(|directive| {
        directive
            .strip_prefix("Environment=")
            .map(|value| {
                value
                    .trim_start_matches(['"', '\''])
                    .starts_with(&assignment)
            })
            .unwrap_or(false)
    })
}

/// One `Environment=NAME=value` line, or nothing when the config has already written one.
fn env_default(directives: &[String], name: &str, value: &str) -> String {
    if sets_env_var(directives, name) {
        String::new()
    } else {
        format!("Environment={}={}\n", name, value)
    }
}

/// The PATH a generated unit gives the server, and through it every command the server resolves.
///
/// This is not the same question as a pane's PATH, and that is the trap. A pane shell sources the
/// rc chain and fixes its own PATH; the SERVER resolves a layout `command`, a `zellij run --`, a
/// `zellij edit` and a `copy_command` against the PATH it was started with, once, for the life of
/// the session. So a launcher-created session had an interactive pane that worked beside a layout
/// pane reporting "Command not found" - and only on the machine where the launcher won the race.
///
/// The directory the unit's own binary lives in leads the list, because that is the one directory
/// this machine is known to keep terminal software in: it is where the binary the unit execs was
/// found. A package manager prefix arrives that way instead of being hardcoded. The rest is the
/// platform's own default, as `sh` would use with no PATH at all.
const PLATFORM_PATH: &str = "/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";

fn service_path(exe: &Path) -> String {
    match exe.parent().map(|dir| dir.display().to_string()) {
        Some(dir)
            if !dir.is_empty()
                && !PLATFORM_PATH
                    .split(':')
                    .any(|platform_dir| platform_dir == dir) =>
        {
            format!("{}:{}", dir, PLATFORM_PATH)
        },
        _ => PLATFORM_PATH.to_owned(),
    }
}

fn systemd_unit(exe: &Path, session: &str, extras: Option<&SessionServiceOptions>) -> String {
    let extras = extras.cloned().unwrap_or_default();
    // Defaults, not decisions: a config that sets its own TERM or PATH replaces these lines rather
    // than adding a second assignment of the same variable.
    let term = env_default(
        &extras.systemd.service,
        "TERM",
        crate::session_lifecycle::DEFAULT_TERM,
    );
    let path = env_default(&extras.systemd.service, "PATH", &service_path(exe));
    format!(
        "\
# zellij session '{session}' - write to ~/.config/systemd/user/{unit}
#
# `zellij session enable {session}` writes this file and its watchdog timer, and loads both. By
# hand it is:
#     systemctl --user daemon-reload
#     systemctl --user enable --now {unit}
#
# `zellij session up` is idempotent and asserts its own result, so the paired {timer} is a watchdog
# rather than a duplicate risk: it re-runs the same command every {interval}s.
#
# Deliberately absent: TMPDIR and ZELLIJ_SOCKET_DIR. The binary resolves its own socket directory,
# and a unit that pins a different one creates a session no login shell can see.

[Unit]
Description=zellij session {session}
After=default.target
{unit_extra}
[Service]
Type=oneshot
RemainAfterExit=no
# KillMode=process, and it is load-bearing. `session up` daemonizes the server and returns, so
# with the default control-group mode systemd tears the cgroup down the moment this oneshot
# deactivates - taking the server it has just started with it. The session appears for a second
# and vanishes, which looks like the session dying rather than the launcher killing it. It only
# bites when this unit CREATES the session, so it stays hidden for as long as something else
# gets there first.
KillMode=process
# The server hands its own environment to every pane shell, and a unit has no TERM: without this
# every pane in a session this unit created comes up with TERM=dumb. Setting your own terminal type
# in the config's `session_service` block replaces this line rather than adding a second one.
{term}\
# PATH, for the same reason and a different symptom. The SERVER resolves a layout `command`, a
# `zellij run --`, a `zellij edit` and a `copy_command` against its own PATH, once, for the life of
# the session - a pane shell fixing its own PATH from the rc chain does not fix that. A unit is
# started with none. The first entry is where the binary below was found; setting PATH in the
# config's `session_service` block replaces this line.
{path}ExecStart={exe} session up {session}
{service_extra}
[Install]
WantedBy=default.target
{install_extra}",
        session = session,
        exe = exe.display(),
        interval = CHECK_INTERVAL_SECS,
        term = term,
        path = path,
        unit = systemd_service_name(session),
        timer = systemd_timer_name(session),
        unit_extra = directive_lines(&extras.systemd.unit),
        service_extra = directive_lines(&extras.systemd.service),
        install_extra = directive_lines(&extras.systemd.install),
    )
}

/// The watchdog half of the systemd install.
///
/// launchd gets this for free - `StartInterval` is a key on the job itself - so without a timer the
/// two platforms would not behave the same: a session that died at 3am would come back at the next
/// login on Linux and within a minute on macOS. The service is enabled as well as the timer, so the
/// session is created at login and re-checked on the interval, which is what the plist does.
fn systemd_timer(session: &str) -> String {
    format!(
        "\
# Watchdog for {unit} - write to ~/.config/systemd/user/{timer}
#
# `zellij session enable {session}` writes and loads this alongside the service. It re-runs
# `zellij session up`, which is idempotent: a pass over a healthy session is a no-op.

[Unit]
Description=zellij session {session} watchdog

[Timer]
OnBootSec={interval}
OnUnitActiveSec={interval}
Unit={unit}

[Install]
WantedBy=timers.target
",
        session = session,
        interval = CHECK_INTERVAL_SECS,
        unit = systemd_service_name(session),
        timer = systemd_timer_name(session),
    )
}

/// The systemd unit name of the service that keeps `session` up.
///
/// One unit per session name, for the reason [`launchd_label`] gives: a fixed name would let the
/// second session's install overwrite the first one's.
pub fn systemd_service_name(session: &str) -> String {
    format!("zellij-session-{}.service", session)
}

/// The systemd unit name of the timer that re-runs the service.
pub fn systemd_timer_name(session: &str) -> String {
    format!("zellij-session-{}.timer", session)
}

/// Take a string value the generator has a default for out of the configured keys.
///
/// The keys are removed whether or not a string comes back, because every one of these is written
/// by the generator itself: leaving one in the extras would put the key in the dictionary twice,
/// and a dict with the same key twice is not a plist.
fn take_default_key(keys: &mut Vec<LaunchdKey>, name: &str) -> Option<String> {
    let mut given = None;
    keys.retain(|key| {
        if key.name != name {
            return true;
        }
        if let (None, PlistValue::String(value)) = (&given, &key.value) {
            given = Some(value.clone());
        }
        false
    });
    given
}

/// Where a launchd job's output goes, per session.
///
/// launchd sends the output of a job that names no path to `/dev/null`, and that is where the whole
/// design's diagnostics went: `session up` asserts its post-condition and prints why it failed, and
/// on a Mac nobody could read it. A session that never came back after login left no evidence
/// anywhere. systemd needed nothing here - a unit's stderr is the journal.
///
/// The state directory, not `/tmp`: this is the same state that holds the restart log and the
/// snapshot archive, it is per-user, and it survives a reboot, which a log about what happened at
/// login has to.
pub fn launchd_log_paths(session: &str) -> (PathBuf, PathBuf) {
    let dir = crate::consts::ZELLIJ_STATE_DIR.clone();
    (
        dir.join(format!("session-{}.out.log", session)),
        dir.join(format!("session-{}.err.log", session)),
    )
}

/// The directory a launchd-created session's panes start in.
///
/// launchd gives a job no working directory, so it inherits `/` and every pane of a session the
/// agent created opens there. A systemd user unit is right by accident - it defaults to `$HOME` -
/// which is why the generated unit says nothing about it and this one has to.
fn launchd_working_directory() -> String {
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().display().to_string())
        .unwrap_or_else(|| "/".to_owned())
}

fn launchd_plist(exe: &Path, session: &str, extras: Option<&SessionServiceOptions>) -> String {
    let extras = extras.cloned().unwrap_or_default();
    // Every one of these is a value the generator owns a DEFAULT for rather than a part of the
    // plist it owns outright, so a config entry naming one replaces the value instead of being
    // refused. TERM and PATH are environment variables, not plist keys - launchd has no top-level
    // key by either name - so a configured one is routed into EnvironmentVariables, where it means
    // something. The rest are real plist keys and are written where they belong.
    let mut keys = extras.launchd;
    let term = take_default_key(&mut keys, "TERM")
        .unwrap_or_else(|| crate::session_lifecycle::DEFAULT_TERM.to_owned());
    let path = take_default_key(&mut keys, "PATH").unwrap_or_else(|| service_path(exe));
    let working_directory =
        take_default_key(&mut keys, "WorkingDirectory").unwrap_or_else(launchd_working_directory);
    let (default_out, default_err) = launchd_log_paths(session);
    let out_path = take_default_key(&mut keys, "StandardOutPath")
        .unwrap_or_else(|| default_out.display().to_string());
    let err_path = take_default_key(&mut keys, "StandardErrorPath")
        .unwrap_or_else(|| default_err.display().to_string());
    format!(
        "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<!--
  zellij session '{session}' - write to ~/Library/LaunchAgents/{label}.plist

  `zellij session enable {session}` writes this file and loads it. By hand it is:
      launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/{label}.plist
      launchctl kickstart -k gui/$(id -u)/{label}

  RunAtLoad brings the session up at login; StartInterval re-checks it. `zellij session up` is
  idempotent and asserts its own result, so a pass over a healthy session does nothing.

  Loading into the gui/ domain above is what puts this job in the graphical login session, and that
  is why the agent is worth having. A job there runs with the context that grants access to
  TCC-gated resources, the login keychain, the pasteboard and notifications. A process cannot ask
  for that context: it is conferred by the domain the job was loaded into, and inherited by
  children. For a multiplexer that is decisive, because the server is long-lived and every pane in
  it inherits what the server has - a server first started from an SSH shell lacks that access for
  as long as it lives, and attaching to it later from a graphical terminal does not change it.
  Started from here it always has it, however you attach afterwards.

  LimitLoadToSessionType Aqua does NOT itself confer that context - a job bootstrapped into gui/
  reports the Aqua domain with or without the key. What it does is restrict which session types
  the job may auto-load into, so at login it cannot come up anywhere else. Keep it for that, but
  do not read its presence as the thing granting the context: the bootstrap target is.

  EnvironmentVariables carries PATH and TERM and NOTHING ELSE - in particular no TMPDIR and no
  ZELLIJ_SOCKET_DIR. launchd hands out a per-user TMPDIR that differs from the one a login shell
  sees, so a pinned socket directory here would build a session invisible to every terminal. The
  binary resolves that directory itself.

  TERM is here because a launch agent has none, and the server hands its own environment to every
  pane shell it spawns: without it every pane of a session this agent created comes up with
  TERM=dumb. PATH is here for the same reason and a different symptom: the SERVER resolves a layout
  `command`, a `zellij run --`, a `zellij edit` and a `copy_command` against its own PATH, once, for
  the life of the session, and a pane shell fixing its own PATH from the rc chain does not fix that.
  Its first entry is the directory the binary above was found in.

  StandardOutPath and StandardErrorPath are not decoration. `zellij session up` asserts that the
  session it created is really there and prints why if it is not, and launchd sends the output of a
  job that names no path to /dev/null - so a session that never came back after login used to leave
  no evidence anywhere at all.

  WorkingDirectory, because launchd gives a job none: without it every pane of a session this agent
  created opens in /. A systemd user unit defaults to the home directory and needs no such line.

  Each of these is a DEFAULT. A key of the same name in the config's `session_service` launchd
  block replaces it; TERM and PATH are replaced inside this dictionary, where they mean something,
  because launchd has no top-level key by either name.
-->
<plist version=\"1.0\">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>session</string>
        <string>up</string>
        <string>{session}</string>
    </array>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>PATH</key>
        <string>{path}</string>
        <key>TERM</key>
        <string>{term}</string>
    </dict>
    <key>WorkingDirectory</key>
    <string>{working_directory}</string>
    <key>StandardOutPath</key>
    <string>{out_path}</string>
    <key>StandardErrorPath</key>
    <string>{err_path}</string>
    <key>RunAtLoad</key>
    <true/>
    <key>StartInterval</key>
    <integer>{interval}</integer>
{extra_keys}</dict>
</plist>
",
        session = session,
        label = launchd_label(session),
        exe = exe.display(),
        interval = CHECK_INTERVAL_SECS,
        term = xml_escape(&term),
        path = xml_escape(&path),
        working_directory = xml_escape(&working_directory),
        out_path = xml_escape(&out_path),
        err_path = xml_escape(&err_path),
        extra_keys = plist_entries(&keys),
    )
}

/// The init system of the machine this is running on, where there is one this module can drive.
pub fn native_service_kind() -> Option<ServiceKind> {
    #[cfg(target_os = "linux")]
    {
        Some(ServiceKind::Systemd)
    }
    #[cfg(target_os = "macos")]
    {
        Some(ServiceKind::Launchd)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

/// Where a user's own units live. Both are per-user directories: nothing here needs root, and a
/// session belongs to the user whose login session it runs in.
pub fn service_dir(kind: ServiceKind) -> Result<PathBuf, String> {
    let dirs = directories::BaseDirs::new()
        .ok_or_else(|| "cannot find the home directory to install into".to_owned())?;
    Ok(match kind {
        // config_dir() honours XDG_CONFIG_HOME, which is also where the user's systemd manager
        // looks - so an unusual config home stays consistent between the two
        ServiceKind::Systemd => dirs.config_dir().join("systemd").join("user"),
        ServiceKind::Launchd => dirs.home_dir().join("Library").join("LaunchAgents"),
    })
}

/// One file of an install, and the name the init system knows it by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFile {
    /// What this file is, for a person reading the output: "service", "timer", "agent".
    pub role: &'static str,
    /// The unit name `systemctl` takes. launchd is addressed by label, not by file name, so this
    /// is the label there and the file is named after it.
    pub unit: String,
    pub path: PathBuf,
    pub contents: String,
}

impl ServiceFile {
    /// Whether the file on disk is already exactly this one. An install that would change nothing
    /// is an install that should say so rather than reload the init system for no reason.
    pub fn is_current(&self) -> bool {
        std::fs::read_to_string(&self.path).map_or(false, |on_disk| on_disk == self.contents)
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }
}

/// Every file an install of `session` writes, in the order they should be written.
///
/// systemd needs two - see [`systemd_timer`] for why - and launchd one.
pub fn service_files(
    kind: ServiceKind,
    exe: &Path,
    session: &str,
    extras: Option<&SessionServiceOptions>,
) -> Result<Vec<ServiceFile>, String> {
    let dir = service_dir(kind)?;
    Ok(match kind {
        ServiceKind::Systemd => {
            let unit = systemd_service_name(session);
            let timer = systemd_timer_name(session);
            vec![
                ServiceFile {
                    role: "service",
                    path: dir.join(&unit),
                    contents: service_unit(kind, exe, session, extras),
                    unit,
                },
                ServiceFile {
                    role: "timer",
                    path: dir.join(&timer),
                    contents: systemd_timer(session),
                    unit: timer,
                },
            ]
        },
        ServiceKind::Launchd => {
            let label = launchd_label(session);
            let file = format!("{}.plist", label);
            vec![ServiceFile {
                role: "agent",
                path: dir.join(&file),
                contents: service_unit(kind, exe, session, extras),
                unit: label,
            }]
        },
    })
}

/// What an `enable` did, so the caller can report the difference between doing the work and
/// finding it already done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnableOutcome {
    /// Every file was already what this build writes, and the init system already had the job.
    AlreadyEnabled,
    Enabled {
        written: Vec<PathBuf>,
        /// Jobs under another name that already keep this session up, installed beside all the
        /// same because `--force` said so. Never non-empty without it.
        beside: Vec<InstalledJob>,
    },
}

/// What a `disable` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisableOutcome {
    /// Nothing was installed and nothing was loaded: the state asked for is the state found.
    NotInstalled,
    /// Nothing this build wrote is installed, but something else keeps the session up. Saying
    /// "nothing to remove" here would contradict `status`, which reports that job by name.
    NotOurs { jobs: Vec<InstalledJob> },
    Disabled {
        removed: Vec<PathBuf>,
        /// Jobs under another name that this command did NOT touch. Removal is scoped to exactly
        /// what `enable` wrote; a job written by hand is somebody else's file.
        remaining: Vec<InstalledJob>,
    },
}

/// Why an install must not join a job that already keeps this session up, when there is one.
///
/// Two launchers for one session is not redundancy. Both fire at login - `RunAtLoad`, `After=` -
/// and they race: one creates the server, the other reaches `session up`, finds a server already
/// serving the name, and refuses to create a second one. On systemd that second job is then left
/// in `failed`, which is what a person eventually goes looking at, and it is not where the fault
/// is. So the second install is refused before it happens rather than diagnosed afterwards.
///
/// The message says which job is which, because the two are removed by different means: `session
/// disable` takes back exactly what `session enable` wrote and nothing else, so a job zellij did
/// not write has to be removed by whoever wrote it.
pub fn refusal_to_install_beside(jobs: &[InstalledJob], session: &str) -> Option<String> {
    if jobs.is_empty() {
        return None;
    }
    let listed = jobs
        .iter()
        .map(|job| format!("\n        {} ({})", job.name, job.path.display()))
        .collect::<String>();
    Some(format!(
        "something already runs `session up {session}` under a name this build did not \
         choose:{listed}\n  \
         Installing beside it gives '{session}' two launchers, and at login both start: one \
         creates\n  the server and the other refuses to create a second, and is left failed. \
         Remove the job\n  above first - it is not zellij's file, so `zellij session disable \
         {session}` will not touch\n  it - or re-run with --force to install beside it anyway.",
        session = session,
        listed = listed,
    ))
}

/// What a `disable` amounts to when this build's own install is not there to remove.
///
/// Not the same answer as "nothing is installed": a job under another name is still keeping the
/// session up, and this command will not remove somebody else's file.
pub fn nothing_of_ours_to_remove(foreign: Vec<InstalledJob>) -> DisableOutcome {
    if foreign.is_empty() {
        DisableOutcome::NotInstalled
    } else {
        DisableOutcome::NotOurs { jobs: foreign }
    }
}

/// The facts that come apart, which is why `status` reports them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub kind: ServiceKind,
    pub files: Vec<ServiceFileStatus>,
    /// Whether the init system currently holds the job, and how it phrased that.
    pub loaded: bool,
    pub load_detail: String,
    /// Jobs that keep this session up under a name this build would not have written. Ordinarily
    /// empty. When it is not, the derived file is absent and the work is being done all the same -
    /// which is the opposite of what "missing" would say.
    pub installed_as: Vec<InstalledJob>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceFileStatus {
    pub role: &'static str,
    pub path: PathBuf,
    pub present: bool,
    /// Present, but not what this build would write now - an upgrade or a config edit that has not
    /// been re-enabled.
    pub stale: bool,
}

/// Write the files and hand them to the init system.
///
/// Idempotent in both directions: a second `enable` over an unchanged install reports
/// [`EnableOutcome::AlreadyEnabled`] and touches nothing, and an `enable` over a changed one
/// rewrites and reloads. The reload matters - both init systems keep the definition they were
/// given rather than the file it came from, so a rewritten file that is not reloaded is a lie on
/// disk.
pub fn enable(
    kind: ServiceKind,
    exe: &Path,
    session: &str,
    extras: Option<&SessionServiceOptions>,
    force: bool,
) -> Result<EnableOutcome, String> {
    // Asked BEFORE anything is written, because the fault this prevents is a second launcher
    // existing at all - see [`refusal_to_install_beside`]. `status` has always reported such a job;
    // this is the command that acts on the same fact instead of installing on top of it.
    let beside = jobs_under_another_name(kind, session);
    if !force {
        if let Some(refusal) = refusal_to_install_beside(&beside, session) {
            return Err(refusal);
        }
    }
    let files = service_files(kind, exe, session, extras)?;
    let up_to_date = files.iter().all(|file| file.is_current());
    if up_to_date && job_is_loaded(kind, &files) {
        return Ok(EnableOutcome::AlreadyEnabled);
    }

    let mut written = Vec::new();
    for file in &files {
        if file.is_current() {
            continue;
        }
        if let Some(parent) = file.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
        }
        std::fs::write(&file.path, &file.contents)
            .map_err(|e| format!("could not write {}: {}", file.path.display(), e))?;
        written.push(file.path.clone());
    }
    // launchd opens the paths the plist names and creates neither the directory above them nor the
    // job when it cannot: a log directory that does not exist yet would stop the session coming up
    // at all, which is a poor trade for a log.
    if kind == ServiceKind::Launchd {
        let (out, _) = launchd_log_paths(session);
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
        }
    }
    load_job(&files)?;
    Ok(EnableOutcome::Enabled { written, beside })
}

/// Unload the job, then remove the files - in that order.
///
/// The order is the reason this command exists. Both init systems hold a definition of their own
/// once they have been given one, so removing the file first leaves a job that still runs, from a
/// definition nothing on disk describes. launchd is the worse of the two: `bootout` needs the
/// label, and the label lives in the file that has just been deleted.
///
/// What is removed is exactly what `enable` wrote, and that scope is deliberate: a job somebody
/// wrote by hand is their file, and a command that deletes it because the name matched a session
/// would be a command nobody could trust with a `--force`. Such a job is REPORTED instead - saying
/// "nothing to remove" while `status` names the job that is doing the work is the contradiction
/// this fixes.
pub fn disable(kind: ServiceKind, session: &str) -> Result<DisableOutcome, String> {
    // only the paths and the unit names matter here, not the contents, so any exe will do
    let files = service_files(kind, Path::new("zellij"), session, None)?;
    let anything_present = files.iter().any(|file| file.exists());
    let loaded = job_is_loaded(kind, &files);
    let foreign = jobs_under_another_name(kind, session);
    if !anything_present && !loaded {
        return Ok(nothing_of_ours_to_remove(foreign));
    }

    unload_job(&files)?;
    let mut removed = Vec::new();
    for file in &files {
        match std::fs::remove_file(&file.path) {
            Ok(()) => removed.push(file.path.clone()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
            Err(e) => return Err(format!("could not remove {}: {}", file.path.display(), e)),
        }
    }
    // systemd keeps a removed unit in its state until it is told to look again
    reload_after_removal()?;
    Ok(DisableOutcome::Disabled {
        removed,
        remaining: foreign,
    })
}

/// Report the install without changing it.
pub fn status(
    kind: ServiceKind,
    exe: &Path,
    session: &str,
    extras: Option<&SessionServiceOptions>,
) -> Result<ServiceStatus, String> {
    let files = service_files(kind, exe, session, extras)?;
    let installed_as = jobs_under_another_name(kind, session);
    let (loaded, load_detail) = job_load_state(&files, &installed_as);
    Ok(ServiceStatus {
        kind,
        installed_as,
        files: files
            .iter()
            .map(|file| ServiceFileStatus {
                role: file.role,
                path: file.path.clone(),
                present: file.exists(),
                stale: file.exists() && !file.is_current(),
            })
            .collect(),
        loaded,
        load_detail,
    })
}

fn job_is_loaded(_kind: ServiceKind, files: &[ServiceFile]) -> bool {
    // `enable` and `disable` act on the install this build writes, so only its own names decide
    // whether that install is loaded
    job_load_state(files, &[]).0
}

/// The jobs that keep `session` up under a name this build would not have written.
///
/// Empty on a machine whose install came from this command, which is the ordinary case.
#[cfg(target_os = "linux")]
fn jobs_under_another_name(_kind: ServiceKind, session: &str) -> Vec<InstalledJob> {
    let units = installed_user_units();
    find_session_job(&units, session, &systemd_service_name(session))
        .renamed_jobs()
        .into_iter()
        .cloned()
        .collect()
}

#[cfg(target_os = "macos")]
fn jobs_under_another_name(_kind: ServiceKind, session: &str) -> Vec<InstalledJob> {
    let agents = installed_launch_agents();
    find_session_job(&agents, session, &launchd_label(session))
        .renamed_jobs()
        .into_iter()
        .cloned()
        .collect()
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn jobs_under_another_name(_kind: ServiceKind, _session: &str) -> Vec<InstalledJob> {
    Vec::new()
}

#[cfg(target_os = "linux")]
fn job_load_state(files: &[ServiceFile], installed_as: &[InstalledJob]) -> (bool, String) {
    // The install this build would write is not the one doing the work, so its unit names say
    // nothing about whether the session is looked after. Report the unit that does it.
    if let Some(job) = installed_as.first() {
        let state = systemctl::is_enabled(&job.name).unwrap_or_else(|| "unknown".to_owned());
        return (state == "enabled", format!("{} {}", job.name, state));
    }
    let states: Vec<String> = files
        .iter()
        .map(|file| {
            let state = systemctl::is_enabled(&file.unit).unwrap_or_else(|| "unknown".to_owned());
            // whether the watchdog is armed is a separate fact from whether it is enabled, and it
            // is the one that decides whether a session that dies at night is back by morning
            if file.role == "timer" && systemctl::is_active(&file.unit) {
                format!("{} {} and armed", file.role, state)
            } else {
                format!("{} {}", file.role, state)
            }
        })
        .collect();
    // every unit of the install has to be enabled for the install to be: a service without its
    // timer comes up at login and is never checked again
    let loaded = files
        .iter()
        .all(|file| systemctl::is_enabled(&file.unit).as_deref() == Some("enabled"));
    (loaded, states.join(", "))
}

#[cfg(target_os = "macos")]
fn job_load_state(files: &[ServiceFile], installed_as: &[InstalledJob]) -> (bool, String) {
    // whichever label carries the job, that is the one to ask launchd about
    let label = installed_as
        .first()
        .map(|job| job.name.clone())
        .or_else(|| files.first().map(|file| file.unit.clone()))
        .unwrap_or_default();
    let loaded = launchctl::job_is_loaded(&label);
    let detail = if loaded {
        format!("{}/{}", launchctl::gui_domain(), label)
    } else {
        format!("not in {}", launchctl::gui_domain())
    };
    (loaded, detail)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn job_load_state(_files: &[ServiceFile], _installed_as: &[InstalledJob]) -> (bool, String) {
    (false, "unsupported init system".to_owned())
}

#[cfg(target_os = "linux")]
fn load_job(files: &[ServiceFile]) -> Result<(), String> {
    systemctl::daemon_reload()?;
    for file in files {
        systemctl::enable_now(&file.unit)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn load_job(files: &[ServiceFile]) -> Result<(), String> {
    for file in files {
        // launchd refuses to bootstrap a label it already holds, and it would keep the definition
        // it was given rather than the file just written - so a reload is boot out, then in
        if launchctl::job_is_loaded(&file.unit) {
            launchctl::bootout(&file.unit)?;
        }
        launchctl::bootstrap(&file.path)?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn load_job(_files: &[ServiceFile]) -> Result<(), String> {
    Err("no init system this build knows how to load a unit into".to_owned())
}

#[cfg(target_os = "linux")]
fn unload_job(files: &[ServiceFile]) -> Result<(), String> {
    // the timer first: stopping the service while its timer still runs invites the timer to start
    // it again between the two commands
    for file in files.iter().rev() {
        systemctl::disable_now(&file.unit)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn unload_job(files: &[ServiceFile]) -> Result<(), String> {
    for file in files {
        if launchctl::job_is_loaded(&file.unit) {
            launchctl::bootout(&file.unit)?;
        }
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn unload_job(_files: &[ServiceFile]) -> Result<(), String> {
    Ok(())
}

#[cfg(target_os = "linux")]
fn reload_after_removal() -> Result<(), String> {
    systemctl::daemon_reload()
}

#[cfg(not(target_os = "linux"))]
fn reload_after_removal() -> Result<(), String> {
    Ok(())
}

/// The systemd user manager, addressed as the user - never as root, never system-wide.
#[cfg(target_os = "linux")]
pub mod systemctl {
    use std::process::Command;

    fn run(args: &[&str]) -> Result<String, String> {
        let output = Command::new("systemctl")
            .arg("--user")
            .args(args)
            .output()
            .map_err(|e| format!("could not run systemctl: {}", e))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
        } else {
            Err(format!(
                "systemctl --user {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }

    pub fn daemon_reload() -> Result<(), String> {
        run(&["daemon-reload"]).map(|_| ())
    }

    pub fn enable_now(unit: &str) -> Result<(), String> {
        run(&["enable", "--now", unit]).map(|_| ())
    }

    pub fn disable_now(unit: &str) -> Result<(), String> {
        run(&["disable", "--now", unit]).map(|_| ())
    }

    /// What systemd calls the unit's install state - "enabled", "disabled", "not-found". It exits
    /// non-zero for every answer but "enabled", so the exit status says nothing the word does not.
    pub fn is_enabled(unit: &str) -> Option<String> {
        let output = Command::new("systemctl")
            .args(["--user", "is-enabled", unit])
            .output()
            .ok()?;
        let state = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        (!state.is_empty()).then_some(state)
    }

    /// Whether the timer is armed, which is the fact a person wants next after "is it enabled".
    pub fn is_active(unit: &str) -> bool {
        Command::new("systemctl")
            .args(["--user", "is-active", unit])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }
}

/// launchd, in the graphical login session domain - see the plist's own comment for why that
/// domain and no other.
#[cfg(target_os = "macos")]
pub mod launchctl {
    use std::path::Path;
    use std::process::Command;

    pub fn gui_domain() -> String {
        format!("gui/{}", unsafe { libc::getuid() })
    }

    pub fn job_is_loaded(label: &str) -> bool {
        Command::new("launchctl")
            .args(["print", &format!("{}/{}", gui_domain(), label)])
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    pub fn bootstrap(plist: &Path) -> Result<(), String> {
        run(&["bootstrap", &gui_domain(), &plist.display().to_string()])
    }

    pub fn bootout(label: &str) -> Result<(), String> {
        run(&["bootout", &format!("{}/{}", gui_domain(), label)])
    }

    fn run(args: &[&str]) -> Result<(), String> {
        let output = Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| format!("could not run launchctl: {}", e))?;
        if output.status.success() {
            return Ok(());
        }
        Err(format!(
            "launchctl {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn exe() -> PathBuf {
        PathBuf::from("/usr/local/bin/zellij")
    }

    /// A versioned install: the stable name on PATH is a symlink into the version installed now.
    fn versioned_install() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::TempDir::new().unwrap();
        let versioned = root.path().join("versions/1.2.3/bin");
        let stable_dir = root.path().join("bin");
        std::fs::create_dir_all(&versioned).unwrap();
        std::fs::create_dir_all(&stable_dir).unwrap();
        let real = versioned.join("zellij");
        std::fs::write(&real, b"binary").unwrap();
        let stable = stable_dir.join("zellij");
        std::os::unix::fs::symlink(&real, &stable).unwrap();
        (root, real, stable)
    }

    #[test]
    fn the_stable_name_on_path_is_preferred_to_the_version_it_points_at() {
        let (_root, real, stable) = versioned_install();
        let stable_dir = stable.parent().unwrap().to_path_buf();
        assert_eq!(
            resolve_service_exe(None, &real, &[stable_dir]),
            ServiceExe::Stable(stable)
        );
    }

    #[test]
    fn a_path_entry_that_is_a_different_binary_is_not_this_one() {
        let (_root, real, stable) = versioned_install();
        // another zellij, earlier on PATH: same name, its own file
        let other_dir = stable.parent().unwrap().parent().unwrap().join("other");
        std::fs::create_dir_all(&other_dir).unwrap();
        std::fs::write(other_dir.join("zellij"), b"another binary").unwrap();
        assert_eq!(
            resolve_service_exe(None, &real, &[other_dir]),
            ServiceExe::Resolved(real.canonicalize().unwrap())
        );
    }

    #[test]
    fn a_path_the_user_named_wins() {
        let (_root, real, stable) = versioned_install();
        let stable_dir = stable.parent().unwrap().to_path_buf();
        let given = PathBuf::from("/opt/zellij/bin/zellij");
        assert_eq!(
            resolve_service_exe(Some(given.clone()), &real, &[stable_dir]),
            ServiceExe::Given(given)
        );
    }

    #[test]
    fn with_nothing_on_path_the_unit_still_gets_an_absolute_path() {
        let (_root, real, _stable) = versioned_install();
        let exe = resolve_service_exe(None, &real, &[]);
        assert_eq!(exe, ServiceExe::Resolved(real.canonicalize().unwrap()));
        assert!(exe.path().is_absolute());
    }

    #[test]
    fn each_session_has_its_own_launchd_job() {
        assert_eq!(launchd_label("work"), "dev.zellij.session.work");
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(plist.contains(&format!(
            "<key>Label</key>\n    <string>{}</string>",
            launchd_label("work")
        )));
        // what `zellij session up` will ask launchd for is what the install line loads
        assert!(plist.contains(&format!("gui/$(id -u)/{}", launchd_label("work"))));
    }

    #[test]
    fn each_session_has_its_own_systemd_units() {
        assert_eq!(systemd_service_name("work"), "zellij-session-work.service");
        assert_eq!(systemd_timer_name("work"), "zellij-session-work.timer");
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        // the install line has to name the file the installer writes, or a hand install lands
        // somewhere `zellij session status` never looks
        assert!(unit.contains("systemctl --user enable --now zellij-session-work.service"));
    }

    /// The plist gets its watchdog from a key on the job itself. Without a matching timer the same
    /// command on Linux would only try at login, and the two platforms would behave differently in
    /// exactly the case the unit exists for.
    #[test]
    fn the_linux_timer_re_checks_as_often_as_the_plist_does() {
        let timer = systemd_timer("work");
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(timer.contains(&format!("OnUnitActiveSec={}", CHECK_INTERVAL_SECS)));
        assert!(timer.contains("Unit=zellij-session-work.service"));
        assert!(plist.contains(&format!(
            "<key>StartInterval</key>\n    <integer>{}</integer>",
            CHECK_INTERVAL_SECS
        )));
    }

    #[test]
    fn a_systemd_install_is_the_service_and_its_timer() {
        let files = service_files(ServiceKind::Systemd, &exe(), "work", None).unwrap();
        let names: Vec<&str> = files.iter().map(|file| file.unit.as_str()).collect();
        assert_eq!(
            names,
            ["zellij-session-work.service", "zellij-session-work.timer"]
        );
        // the service is written first: a timer loaded before the unit it triggers is a timer
        // systemd cannot start
        assert_eq!(files[0].role, "service");
    }

    #[test]
    fn a_launchd_install_is_one_plist_named_by_its_label() {
        let files = service_files(ServiceKind::Launchd, &exe(), "work", None).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].unit, launchd_label("work"));
        assert_eq!(
            files[0].path.file_name().unwrap().to_string_lossy(),
            format!("{}.plist", launchd_label("work"))
        );
    }

    fn job(name: &str, exec: &[&str]) -> InstalledJob {
        InstalledJob {
            name: name.to_owned(),
            path: PathBuf::from(format!("/agents/{}", name)),
            exec: exec.iter().map(|argument| (*argument).to_owned()).collect(),
        }
    }

    /// `status` has always reported a job installed under another name. Installing a second job
    /// beside it is what produces the login race: both start, one creates the server and the other
    /// refuses to create a second and is left failed.
    #[test]
    fn an_install_is_refused_beside_a_job_that_already_does_the_work() {
        let jobs = [job(
            "com.example.my-terminal",
            &["/usr/local/bin/zellij", "session", "up", "work"],
        )];
        let refusal = refusal_to_install_beside(&jobs, "work").expect("must refuse");
        assert!(refusal.contains("com.example.my-terminal"));
        assert!(refusal.contains("/agents/com.example.my-terminal"));
        // and it says which job is which: `session disable` takes back what `enable` wrote, so it
        // is not the way to remove somebody else's file
        assert!(refusal.contains("--force"));
        assert!(refusal.contains("will not touch"));
    }

    #[test]
    fn every_job_that_does_the_work_is_named_in_the_refusal() {
        let jobs = [
            job(
                "com.example.one",
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
            job(
                "com.example.two",
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
        ];
        let refusal = refusal_to_install_beside(&jobs, "work").expect("must refuse");
        assert!(refusal.contains("com.example.one"));
        assert!(refusal.contains("com.example.two"));
    }

    #[test]
    fn nothing_is_refused_on_the_ordinary_machine() {
        assert_eq!(refusal_to_install_beside(&[], "work"), None);
    }

    /// The contradiction this fixes: `status` naming the job that keeps the session up while
    /// `disable` calls the same machine "nothing installed".
    #[test]
    fn disable_names_the_foreign_job_rather_than_calling_it_nothing() {
        let foreign = vec![job(
            "com.example.my-terminal",
            &["/usr/local/bin/zellij", "session", "up", "work"],
        )];
        assert_eq!(
            nothing_of_ours_to_remove(foreign.clone()),
            DisableOutcome::NotOurs { jobs: foreign }
        );
        // and removal is still scoped to what `enable` wrote: the outcome carries the job to
        // report, not a file to delete
        assert_eq!(
            nothing_of_ours_to_remove(Vec::new()),
            DisableOutcome::NotInstalled
        );
    }

    #[test]
    fn the_job_this_build_installed_is_found_by_its_own_name() {
        let jobs = [job(
            &launchd_label("work"),
            &["/usr/local/bin/zellij", "session", "up", "work"],
        )];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::Installed(&jobs[0])
        );
    }

    /// The case the derived name misses entirely: an agent installed by hand, or by anything older
    /// than this command, doing the job under a name of its author's choosing.
    #[test]
    fn a_job_under_another_name_is_still_the_job() {
        let jobs = [job(
            "com.example.my-terminal",
            &["/usr/local/bin/zellij", "session", "up", "work"],
        )];
        let found = find_session_job(&jobs, "work", &launchd_label("work"));
        assert_eq!(found, SessionJob::InstalledAs(&jobs[0]));
        assert_eq!(found.renamed(), Some(&jobs[0]));
    }

    #[test]
    fn a_job_for_another_session_is_not_this_session_s() {
        let jobs = [
            job(
                "com.example.one",
                &["/usr/bin/zellij", "session", "up", "notes"],
            ),
            job(
                "com.example.two",
                &["/usr/bin/zellij", "session", "up", "work-notes"],
            ),
        ];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::NotInstalled
        );
    }

    #[test]
    fn a_job_running_another_subcommand_is_not_a_match() {
        let jobs = [
            job(
                "com.example.down",
                &["/usr/bin/zellij", "session", "down", "work"],
            ),
            job("com.example.attach", &["/usr/bin/zellij", "attach", "work"]),
            job(
                "com.example.action",
                &["/usr/bin/zellij", "-s", "work", "action", "dump-screen"],
            ),
        ];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::NotInstalled
        );
    }

    /// argv[0] is whatever the author put in front: a wrapper, an environment shim, a scheduler.
    /// The subcommand is what identifies the job, and the binary need not even be called zellij.
    #[test]
    fn a_wrapper_in_front_of_the_binary_does_not_hide_the_job() {
        let jobs = [
            job(
                "com.example.wrapped",
                &[
                    "/usr/bin/env",
                    "-i",
                    "/opt/builds/zj-nightly",
                    "session",
                    "up",
                    "work",
                ],
            ),
            job(
                "com.example.restoring",
                &[
                    "/usr/bin/zellij",
                    "session",
                    "up",
                    "--restore",
                    "3",
                    "other",
                ],
            ),
        ];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::InstalledAs(&jobs[0])
        );
        // and the value of an option is not read as the session name
        assert_eq!(
            find_session_job(&jobs, "3", &launchd_label("3")),
            SessionJob::NotInstalled
        );
        assert_eq!(
            find_session_job(&jobs, "other", &launchd_label("other")),
            SessionJob::InstalledAs(&jobs[1])
        );
    }

    #[test]
    fn several_jobs_for_one_session_are_all_reported() {
        let jobs = [
            job(
                "com.example.second",
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
            job(
                "com.example.first",
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
        ];
        let found = find_session_job(&jobs, "work", &launchd_label("work"));
        assert_eq!(found, SessionJob::Ambiguous(vec![&jobs[1], &jobs[0]]));
        // one of them is acted on, and both are named so that the choice is not made in silence
        assert_eq!(found.job(), Some(&jobs[1]));
        assert_eq!(found.renamed_jobs().len(), 2);
    }

    /// This build's own name settles it: an install it wrote is never reported as an oddity just
    /// because something else on the machine runs the same command.
    #[test]
    fn the_derived_name_wins_over_the_others() {
        let jobs = [
            job(
                "com.example.other",
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
            job(
                &launchd_label("work"),
                &["/usr/bin/zellij", "session", "up", "work"],
            ),
        ];
        let found = find_session_job(&jobs, "work", &launchd_label("work"));
        assert_eq!(found, SessionJob::Installed(&jobs[1]));
        assert!(found.renamed().is_none());
    }

    #[test]
    fn with_nothing_installed_nothing_is_found() {
        assert_eq!(
            find_session_job(&[], "work", &launchd_label("work")),
            SessionJob::NotInstalled
        );
        // a job that names no session takes it from the config, which cannot be read from here
        let jobs = [job(
            "com.example.bare",
            &["/usr/bin/zellij", "session", "up"],
        )];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::NotInstalled
        );
    }

    /// What this build writes has to be readable by what this build reads, or the whole lookup is
    /// theoretical.
    #[test]
    fn the_generated_units_are_read_back_as_the_jobs_they_are() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", Some(&extras()));
        let (label, arguments) = parse_launch_agent(&plist).unwrap();
        assert_eq!(label, launchd_label("work"));
        assert_eq!(session_up_target(&arguments), Some("work"));

        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", Some(&extras()));
        let arguments = parse_unit_exec_start(&unit).unwrap();
        assert_eq!(arguments[0], exe().display().to_string());
        assert_eq!(session_up_target(&arguments), Some("work"));
    }

    /// A hand-written agent: keys in another order, other keys around them, and XML entities in the
    /// values. All three are ordinary in a file this build did not write.
    #[test]
    fn a_hand_written_agent_is_read_the_way_launchd_reads_it() {
        let plist = "\
<plist version=\"1.0\">
<dict>
    <key>RunAtLoad</key>
    <true/>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/zellij</string>
        <string>session</string>
        <string>up</string>
        <string>one &amp; two</string>
    </array>
    <key>Label</key>
    <string>com.example.my-terminal</string>
    <key>StandardOutPath</key>
    <string>/dev/null</string>
</dict>
</plist>
";
        let (label, arguments) = parse_launch_agent(plist).unwrap();
        assert_eq!(label, "com.example.my-terminal");
        assert_eq!(arguments.len(), 4);
        assert_eq!(session_up_target(&arguments), Some("one & two"));
    }

    #[test]
    fn a_hand_written_unit_is_read_the_way_systemd_reads_it() {
        let unit = "\
[Unit]
Description=a terminal
# ExecStart=/usr/bin/zellij session up commented-out

[Service]
ExecStartPre=/usr/bin/zellij session down work
ExecStart=-/opt/my tools/zellij \"session\" up 'my session'
";
        let arguments = parse_unit_exec_start(unit).unwrap();
        // the `-` prefix is systemd's, not part of the path, and a quoted argument is one argument
        assert_eq!(arguments[0], "/opt/my");
        assert_eq!(session_up_target(&arguments), Some("my session"));
    }

    /// The commonest hand-written agent of all: the whole command line in one argument, handed to
    /// a shell. An agent old enough to predate these subcommands could not have called them
    /// directly, so this is the shape the scan most needs to read.
    #[test]
    fn a_command_line_inside_one_argument_is_still_the_job() {
        let arguments: Vec<String> = ["/bin/sh", "-c", "exec zellij session up my-session"]
            .iter()
            .map(|argument| (*argument).to_string())
            .collect();
        assert_eq!(session_up_target(&arguments), Some("my-session"));

        let jobs = [job(
            "com.example.my-terminal",
            &["/bin/sh", "-c", "exec zellij session up work"],
        )];
        assert_eq!(
            find_session_job(&jobs, "work", &launchd_label("work")),
            SessionJob::InstalledAs(&jobs[0])
        );
    }

    #[test]
    fn a_quoted_name_inside_one_argument_survives_the_quotes() {
        let arguments = vec![
            "/bin/bash".to_string(),
            "-lc".to_string(),
            "zellij session up 'my session' >/dev/null".to_string(),
        ];
        assert_eq!(session_up_target(&arguments), Some("my session"));
    }

    #[test]
    fn a_flag_with_a_value_inside_one_argument_is_not_the_session_name() {
        let arguments = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "zellij session up --restore latest work".to_string(),
        ];
        assert_eq!(session_up_target(&arguments), Some("work"));
    }

    #[test]
    fn a_shell_running_another_subcommand_is_still_not_a_match() {
        let arguments = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "zellij session down work && zellij attach work".to_string(),
        ];
        assert_eq!(session_up_target(&arguments), None);
    }

    #[test]
    fn a_file_that_is_not_a_job_is_not_read_as_one() {
        assert_eq!(parse_launch_agent("<plist><dict></dict></plist>"), None);
        assert_eq!(
            parse_unit_exec_start("[Unit]\nDescription=nothing to run\n"),
            None
        );
    }

    #[test]
    fn kinds_are_named_by_their_init_system() {
        assert_eq!(
            ServiceKind::from_name("SystemD"),
            Some(ServiceKind::Systemd)
        );
        assert_eq!(
            ServiceKind::from_name("launchd"),
            Some(ServiceKind::Launchd)
        );
        assert_eq!(ServiceKind::from_name("upstart"), None);
    }

    fn extras() -> SessionServiceOptions {
        let mut extras = SessionServiceOptions::default();
        extras
            .add_systemd_directive("unit", "After=network.target")
            .unwrap();
        extras
            .add_systemd_directive("unit", "Before=some-other.service")
            .unwrap();
        extras.add_systemd_directive("service", "Nice=-5").unwrap();
        extras
            .add_systemd_directive("install", "WantedBy=graphical-session.target")
            .unwrap();
        extras
            .add_launchd_key("ProcessType", PlistValue::String("Interactive".to_owned()))
            .unwrap();
        extras
            .add_launchd_key("Nice", PlistValue::Integer(5))
            .unwrap();
        extras
            .add_launchd_key("AbandonProcessGroup", PlistValue::Bool(true))
            .unwrap();
        extras
    }

    /// A machine with nothing configured must get the unit it got before this option existed.
    /// Asserted at the three places a directive can be inserted, which are the only three places
    /// the bytes could drift - an empty section must leave no blank line and no trailing one.
    #[test]
    fn a_config_with_no_extras_writes_the_unit_unchanged() {
        let empty = SessionServiceOptions::default();
        for kind in [ServiceKind::Systemd, ServiceKind::Launchd] {
            assert_eq!(
                service_unit(kind, &exe(), "work", None),
                service_unit(kind, &exe(), "work", Some(&empty))
            );
        }
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        assert!(unit.contains("After=default.target\n\n[Service]"));
        assert!(unit.contains("session up work\n\n[Install]"));
        assert!(unit.ends_with("WantedBy=default.target\n"));
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(plist.contains("<integer>60</integer>\n</dict>\n</plist>\n"));
    }

    #[test]
    fn systemd_extras_are_appended_to_the_section_they_name() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", Some(&extras()));
        assert!(unit.contains(
            "After=default.target\nAfter=network.target\nBefore=some-other.service\n\n[Service]"
        ));
        assert!(unit.contains("session up work\nNice=-5\n\n[Install]"));
        assert!(unit.ends_with("WantedBy=default.target\nWantedBy=graphical-session.target\n"));
    }

    #[test]
    fn launchd_extras_become_plist_keys_of_their_own_type() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", Some(&extras()));
        assert!(plist.contains("    <key>ProcessType</key>\n    <string>Interactive</string>\n"));
        assert!(plist.contains("    <key>Nice</key>\n    <integer>5</integer>\n"));
        assert!(plist.contains("    <key>AbandonProcessGroup</key>\n    <true/>\n"));
        // and the dict is still closed after them
        assert!(plist.contains("<true/>\n</dict>\n</plist>\n"));
    }

    /// A plist is XML. An unescaped ampersand does not produce a wrong value, it produces a file
    /// launchd will not parse - reported, if at all, as a job that never loads.
    #[test]
    fn a_plist_value_is_xml_escaped() {
        let mut extras = SessionServiceOptions::default();
        extras
            .add_launchd_key("ProcessType", PlistValue::String("<one & two>".to_owned()))
            .unwrap();
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", Some(&extras));
        assert!(plist.contains("<string>&lt;one &amp; two&gt;</string>"));
    }

    /// The generator owns what the unit runs. Everything this module promises follows from it.
    #[test]
    fn an_extra_cannot_take_over_what_the_unit_runs() {
        let mut extras = SessionServiceOptions::default();
        let error = extras
            .add_systemd_directive("service", "ExecStart=/bin/false")
            .unwrap_err();
        assert!(error.contains("ExecStart=/bin/false"), "{}", error);

        for key in GENERATED_LAUNCHD_KEYS {
            let error = extras
                .add_launchd_key(key, PlistValue::String("mine".to_owned()))
                .unwrap_err();
            assert!(error.contains(key), "{}", error);
        }
    }

    /// The two variables the whole design rests on the binary resolving for itself. A unit that
    /// pins either builds a session no terminal can see, and nothing reports it.
    #[test]
    fn an_extra_cannot_pin_a_socket_dir_or_a_tmpdir() {
        let mut extras = SessionServiceOptions::default();
        for directive in [
            "Environment=TMPDIR=/tmp/mine",
            "Environment=ZELLIJ_SOCKET_DIR=/tmp/mine",
            "EnvironmentFile=/etc/zellij/TMPDIR",
        ] {
            assert!(
                extras.add_systemd_directive("service", directive).is_err(),
                "accepted {}",
                directive
            );
        }
        assert!(extras
            .add_launchd_key("TMPDIR", PlistValue::String("/tmp/mine".to_owned()))
            .is_err());
        assert!(extras
            .add_launchd_key(
                "StandardOutPath",
                PlistValue::String("/tmp/ZELLIJ_SOCKET_DIR".to_owned())
            )
            .is_err());
    }

    /// A directive that UNSETS the variables is the guard's own argument written into the unit, and
    /// refusing it was a mention being read as an assignment. It refused at KDL parse time, so the
    /// whole config failed with it - `setup --check` and every other command, not just `session
    /// enable`.
    #[test]
    fn a_directive_that_unsets_the_variables_is_the_opposite_of_pinning_them() {
        let mut extras = SessionServiceOptions::default();
        for directive in [
            "UnsetEnvironment=ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID ZELLIJ_SOCKET_DIR",
            "UnsetEnvironment=TMPDIR",
            // a value that merely mentions one is not an assignment of it either
            "Description=keeps TMPDIR out of the session",
        ] {
            assert!(
                extras.add_systemd_directive("service", directive).is_ok(),
                "refused {}",
                directive
            );
        }
    }

    #[test]
    fn setting_them_is_still_refused_however_it_is_written() {
        let mut extras = SessionServiceOptions::default();
        for directive in [
            "Environment=TMPDIR=/tmp/mine",
            "Environment=FOO=bar TMPDIR=/tmp/mine",
            "Environment=\"ZELLIJ_SOCKET_DIR=/tmp/mine\"",
            "DefaultEnvironment=TMPDIR=/tmp/mine",
            // importing it from the manager's environment puts it in the unit just the same
            "PassEnvironment=ZELLIJ_SOCKET_DIR",
            // an env file could set anything and cannot be read from here, so it stays strict
            "EnvironmentFile=/etc/zellij/TMPDIR",
        ] {
            assert!(
                extras.add_systemd_directive("service", directive).is_err(),
                "accepted {}",
                directive
            );
        }
    }

    #[test]
    fn a_variable_whose_name_only_starts_the_same_is_not_a_forbidden_one() {
        let mut extras = SessionServiceOptions::default();
        assert!(extras
            .add_systemd_directive("service", "Environment=TMPDIR_BACKUP=/tmp/mine")
            .is_ok());
        assert!(extras
            .add_systemd_directive("service", "Environment=MY_TMPDIR=/tmp/mine")
            .is_ok());
    }

    #[test]
    fn an_entry_that_is_not_a_directive_is_refused() {
        let mut extras = SessionServiceOptions::default();
        assert!(extras.add_systemd_directive("unit", "After").is_err());
        assert!(extras.add_systemd_directive("unit", "=value").is_err());
        let error = extras
            .add_systemd_directive("timer", "OnCalendar=daily")
            .unwrap_err();
        assert!(error.contains("unit, service or install"), "{}", error);
    }

    #[test]
    fn the_systemd_unit_calls_session_up() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        assert!(unit.contains("ExecStart=/usr/local/bin/zellij session up work"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn the_launchd_plist_passes_the_session_as_its_own_argument() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(plist.contains("<string>up</string>\n        <string>work</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    /// launchd sends the output of a job that names no path to /dev/null - including the
    /// post-condition failure `session up` prints, which is the whole diagnostic this design rests
    /// on. Without these keys a session that never came back after login left no evidence anywhere.
    #[test]
    fn the_launchd_plist_says_where_the_job_s_output_goes() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        let (out, err) = launchd_log_paths("work");
        assert!(plist.contains(&format!(
            "<key>StandardOutPath</key>\n    <string>{}</string>",
            out.display()
        )));
        assert!(plist.contains(&format!(
            "<key>StandardErrorPath</key>\n    <string>{}</string>",
            err.display()
        )));
        // the state directory, not /tmp: per-user, and it survives the reboot the log is about
        assert!(out.starts_with(&*crate::consts::ZELLIJ_STATE_DIR));
        assert!(out.to_string_lossy().contains("work"));
    }

    /// launchd gives a job no working directory, so without this every pane of a session the agent
    /// created opens in `/`. A systemd user unit defaults to the home directory, which is why only
    /// this generator says anything about it.
    #[test]
    fn the_launchd_plist_starts_the_session_in_the_home_directory() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        let home = directories::BaseDirs::new().unwrap();
        assert!(plist.contains(&format!(
            "<key>WorkingDirectory</key>\n    <string>{}</string>",
            home.home_dir().display()
        )));
    }

    /// The server resolves a layout `command`, `zellij run --`, `zellij edit` and `copy_command`
    /// against its OWN PATH, once, for the life of the session - so a launcher-created session had
    /// an interactive pane that worked beside a layout pane reporting "Command not found". Both
    /// generators have to answer it, and they used to disagree: the plist pinned a Homebrew-shaped
    /// PATH and the unit pinned none at all.
    #[test]
    fn both_generators_give_the_server_a_path() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        let expected = service_path(&exe());
        assert!(unit.contains(&format!("Environment=PATH={}\n", expected)));
        assert!(plist.contains(&format!(
            "<key>PATH</key>\n        <string>{}</string>",
            expected
        )));
        // derived, not hardcoded: the directory the unit's own binary was found in leads the list
        assert!(expected.starts_with("/usr/local/bin:"));
        assert!(expected.contains(PLATFORM_PATH));
    }

    #[test]
    fn a_binary_outside_the_platform_path_puts_its_own_directory_first() {
        let path = service_path(Path::new("/opt/homebrew/bin/zellij"));
        assert_eq!(path, format!("/opt/homebrew/bin:{}", PLATFORM_PATH));
        // and a directory the platform default already names is not repeated
        assert_eq!(service_path(Path::new("/usr/bin/zellij")), PLATFORM_PATH);
    }

    /// Each generated default has to be replaceable, and replacing it must not leave the key in the
    /// file twice: a dict with one key twice is not a plist, and two systemd assignments of one
    /// variable are a unit nobody can read with confidence.
    #[test]
    fn every_generated_default_is_overridable_without_being_written_twice() {
        let mut extras = SessionServiceOptions::default();
        extras
            .add_systemd_directive("service", "Environment=PATH=/my/bin")
            .unwrap();
        for (key, value) in [
            ("PATH", "/my/bin"),
            ("WorkingDirectory", "/my/home"),
            ("StandardOutPath", "/my/logs/out.log"),
            ("StandardErrorPath", "/my/logs/err.log"),
        ] {
            extras
                .add_launchd_key(key, PlistValue::String(value.to_owned()))
                .unwrap();
        }

        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", Some(&extras));
        assert_eq!(unit.matches("Environment=PATH=").count(), 1);
        assert!(unit.contains("Environment=PATH=/my/bin"));

        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", Some(&extras));
        for (key, value) in [
            ("PATH", "/my/bin"),
            ("WorkingDirectory", "/my/home"),
            ("StandardOutPath", "/my/logs/out.log"),
            ("StandardErrorPath", "/my/logs/err.log"),
        ] {
            assert_eq!(
                plist.matches(&format!("<key>{}</key>", key)).count(),
                1,
                "{} is written twice",
                key
            );
            assert!(plist.contains(&format!("<string>{}</string>", value)));
        }
        // PATH belongs inside EnvironmentVariables: a top-level key by that name is one launchd
        // ignores in silence
        assert!(plist.contains("<key>PATH</key>\n        <string>/my/bin</string>"));
    }

    /// The session the server is started in is the session every pane inherits, and only the domain
    /// the job is loaded into can confer it. Without this the agent is no better than the first
    /// interactive attach, which is the thing it exists to beat.
    #[test]
    fn the_launchd_plist_loads_into_the_graphical_login_session() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(plist.contains("<key>LimitLoadToSessionType</key>\n    <string>Aqua</string>"));
    }

    /// The whole point of generating these: a unit that pins either variable builds a session the
    /// rest of the machine cannot see.
    #[test]
    fn the_systemd_unit_does_not_kill_the_session_it_started() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        // The default control-group kill mode reaps the daemonized server when this oneshot
        // deactivates, so the session it just created dies seconds after appearing.
        assert!(
            unit.contains("KillMode=process"),
            "systemd unit would kill the server it started"
        );
    }
    #[test]
    fn the_launchd_plist_does_not_throttle_the_session() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        // ProcessType Background asks launchd to deprioritize CPU and I/O. Panes inherit the
        // server's QoS, so it would throttle every build and long-running job in the session.
        // Omitting the key leaves launchd's Standard default, which is what an interactive
        // multiplexer wants.
        assert!(
            !plist.contains("<key>ProcessType</key>"),
            "plist sets ProcessType, which panes inherit"
        );
    }
    /// A launcher has no TERM, and the server hands its own environment to every pane shell it
    /// spawns - so without this every pane of a session the unit created comes up with TERM=dumb.
    #[test]
    fn every_generated_unit_sets_a_term() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", None);
        assert!(unit.contains(&format!(
            "Environment=TERM={}\n",
            crate::session_lifecycle::DEFAULT_TERM
        )));
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", None);
        assert!(plist.contains(&format!(
            "<key>TERM</key>\n        <string>{}</string>",
            crate::session_lifecycle::DEFAULT_TERM
        )));
    }

    /// TERM is a default, unlike ExecStart or the label: a machine whose terminal is something else
    /// has to be able to say so. What it must not do is end up set twice - a plist dict cannot
    /// carry one key twice at all, and two systemd assignments of one variable are a unit nobody
    /// can read with confidence.
    #[test]
    fn a_configured_term_replaces_the_default_rather_than_joining_it() {
        let mut extras = SessionServiceOptions::default();
        extras
            .add_systemd_directive("service", "Environment=TERM=screen-256color")
            .unwrap();
        extras
            .add_launchd_key("TERM", PlistValue::String("screen-256color".to_owned()))
            .unwrap();

        let unit = service_unit(ServiceKind::Systemd, &exe(), "work", Some(&extras));
        assert_eq!(unit.matches("Environment=TERM=").count(), 1);
        assert!(unit.contains("Environment=TERM=screen-256color"));

        let plist = service_unit(ServiceKind::Launchd, &exe(), "work", Some(&extras));
        assert_eq!(plist.matches("<key>TERM</key>").count(), 1);
        assert!(plist.contains("<key>TERM</key>\n        <string>screen-256color</string>"));
        // and inside EnvironmentVariables, where it means something - launchd has no top-level
        // key by that name
        assert!(plist.contains("<key>EnvironmentVariables</key>"));
        assert!(!plist.contains("    <key>TERM</key>\n    <string>"));
    }

    #[test]
    fn no_unit_sets_a_socket_dir_or_a_tmpdir() {
        let units = [
            (
                "systemd",
                service_unit(ServiceKind::Systemd, &exe(), "work", None),
            ),
            (
                "launchd",
                service_unit(ServiceKind::Launchd, &exe(), "work", None),
            ),
            ("timer", systemd_timer("work")),
        ];
        for (kind, unit) in units {
            for line in unit.lines().filter(|l| !l.trim_start().starts_with('#')) {
                assert!(
                    !line.contains("<key>TMPDIR</key>") && !line.contains("Environment=TMPDIR"),
                    "{:?} unit sets TMPDIR: {}",
                    kind,
                    line
                );
                assert!(
                    !line.contains("<key>ZELLIJ_SOCKET_DIR</key>")
                        && !line.contains("Environment=ZELLIJ_SOCKET_DIR"),
                    "{:?} unit sets ZELLIJ_SOCKET_DIR: {}",
                    kind,
                    line
                );
            }
        }
    }
}
