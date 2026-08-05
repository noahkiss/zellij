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
        if let Some(name) = forbidden_env_name(directive) {
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

fn systemd_unit(exe: &Path, session: &str, extras: Option<&SessionServiceOptions>) -> String {
    let extras = extras.cloned().unwrap_or_default();
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
ExecStart={exe} session up {session}
{service_extra}
[Install]
WantedBy=default.target
{install_extra}",
        session = session,
        exe = exe.display(),
        interval = CHECK_INTERVAL_SECS,
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

fn launchd_plist(exe: &Path, session: &str, extras: Option<&SessionServiceOptions>) -> String {
    let extras = extras.cloned().unwrap_or_default();
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

  EnvironmentVariables carries PATH and NOTHING ELSE - in particular no TMPDIR and no
  ZELLIJ_SOCKET_DIR. launchd hands out a per-user TMPDIR that differs from the one a login shell
  sees, so a pinned socket directory here would build a session invisible to every terminal. The
  binary resolves that directory itself.
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
        <string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string>
    </dict>
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
        extra_keys = plist_entries(&extras.launchd),
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
    },
}

/// What a `disable` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DisableOutcome {
    /// Nothing was installed and nothing was loaded: the state asked for is the state found.
    NotInstalled,
    Disabled {
        removed: Vec<PathBuf>,
    },
}

/// The facts that come apart, which is why `status` reports them apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceStatus {
    pub kind: ServiceKind,
    pub files: Vec<ServiceFileStatus>,
    /// Whether the init system currently holds the job, and how it phrased that.
    pub loaded: bool,
    pub load_detail: String,
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
) -> Result<EnableOutcome, String> {
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
    load_job(&files)?;
    Ok(EnableOutcome::Enabled { written })
}

/// Unload the job, then remove the files - in that order.
///
/// The order is the reason this command exists. Both init systems hold a definition of their own
/// once they have been given one, so removing the file first leaves a job that still runs, from a
/// definition nothing on disk describes. launchd is the worse of the two: `bootout` needs the
/// label, and the label lives in the file that has just been deleted.
pub fn disable(kind: ServiceKind, session: &str) -> Result<DisableOutcome, String> {
    // only the paths and the unit names matter here, not the contents, so any exe will do
    let files = service_files(kind, Path::new("zellij"), session, None)?;
    let anything_present = files.iter().any(|file| file.exists());
    let loaded = job_is_loaded(kind, &files);
    if !anything_present && !loaded {
        return Ok(DisableOutcome::NotInstalled);
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
    Ok(DisableOutcome::Disabled { removed })
}

/// Report the install without changing it.
pub fn status(
    kind: ServiceKind,
    exe: &Path,
    session: &str,
    extras: Option<&SessionServiceOptions>,
) -> Result<ServiceStatus, String> {
    let files = service_files(kind, exe, session, extras)?;
    let (loaded, load_detail) = job_load_state(&files);
    Ok(ServiceStatus {
        kind,
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
    job_load_state(files).0
}

#[cfg(target_os = "linux")]
fn job_load_state(files: &[ServiceFile]) -> (bool, String) {
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
fn job_load_state(files: &[ServiceFile]) -> (bool, String) {
    let label = files
        .first()
        .map(|file| file.unit.clone())
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
fn job_load_state(_files: &[ServiceFile]) -> (bool, String) {
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
