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

use std::path::Path;

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
}

/// Render the unit for `kind`, running `exe` against `session`.
pub fn service_unit(kind: ServiceKind, exe: &Path, session: &str) -> String {
    match kind {
        ServiceKind::Systemd => systemd_unit(exe, session),
        ServiceKind::Launchd => launchd_plist(exe, session),
    }
}

/// How often the scheduler re-runs `session up`. The command is idempotent, so a pass over a
/// healthy session is a no-op and a pass over a missing one restores it.
const CHECK_INTERVAL_SECS: u64 = 60;

fn systemd_unit(exe: &Path, session: &str) -> String {
    format!(
        "\
# zellij session '{session}' - write to ~/.config/systemd/user/zellij-session.service
#
# Install:
#     systemctl --user daemon-reload
#     systemctl --user enable --now zellij-session.service
#
# `zellij session up` is idempotent and asserts its own result, so a repeating timer is a watchdog
# rather than a duplicate risk. To add one, drop a zellij-session.timer beside this file with
# OnUnitActiveSec={interval} and enable that instead of the service.
#
# Deliberately absent: TMPDIR and ZELLIJ_SOCKET_DIR. The binary resolves its own socket directory,
# and a unit that pins a different one creates a session no login shell can see.

[Unit]
Description=zellij session {session}
After=default.target

[Service]
Type=oneshot
RemainAfterExit=no
ExecStart={exe} session up {session}

[Install]
WantedBy=default.target
",
        session = session,
        exe = exe.display(),
        interval = CHECK_INTERVAL_SECS,
    )
}

fn launchd_plist(exe: &Path, session: &str) -> String {
    format!(
        "\
<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<!--
  zellij session '{session}' - write to ~/Library/LaunchAgents/dev.zellij.session.plist

  Install:
      launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/dev.zellij.session.plist
      launchctl kickstart -k gui/$(id -u)/dev.zellij.session

  RunAtLoad brings the session up at login; StartInterval re-checks it. `zellij session up` is
  idempotent and asserts its own result, so a pass over a healthy session does nothing.

  LimitLoadToSessionType Aqua is why this agent is worth having, and it goes with loading the job
  into the gui/ domain above. A job in the graphical login session runs with the context that
  grants access to TCC-gated resources, the login keychain, the pasteboard and notifications. A
  process cannot ask for that context: it is conferred by the domain the job was loaded into, and
  inherited by children. For a multiplexer that is decisive, because the server is long-lived and
  every pane in it inherits what the server has - a server first started from an SSH shell lacks
  that access for as long as it lives, and attaching to it later from a graphical terminal does not
  change it. Started from here it always has it, however you attach afterwards.

  EnvironmentVariables carries PATH and NOTHING ELSE - in particular no TMPDIR and no
  ZELLIJ_SOCKET_DIR. launchd hands out a per-user TMPDIR that differs from the one a login shell
  sees, so a pinned socket directory here would build a session invisible to every terminal. The
  binary resolves that directory itself.
-->
<plist version=\"1.0\">
<dict>
    <key>Label</key>
    <string>dev.zellij.session</string>
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
    <key>ProcessType</key>
    <string>Background</string>
</dict>
</plist>
",
        session = session,
        exe = exe.display(),
        interval = CHECK_INTERVAL_SECS,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn exe() -> PathBuf {
        PathBuf::from("/usr/local/bin/zellij")
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

    #[test]
    fn the_systemd_unit_calls_session_up() {
        let unit = service_unit(ServiceKind::Systemd, &exe(), "work");
        assert!(unit.contains("ExecStart=/usr/local/bin/zellij session up work"));
        assert!(unit.contains("[Install]"));
    }

    #[test]
    fn the_launchd_plist_passes_the_session_as_its_own_argument() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work");
        assert!(plist.contains("<string>up</string>\n        <string>work</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
    }

    /// The session the server is started in is the session every pane inherits, and only the domain
    /// the job is loaded into can confer it. Without this the agent is no better than the first
    /// interactive attach, which is the thing it exists to beat.
    #[test]
    fn the_launchd_plist_loads_into_the_graphical_login_session() {
        let plist = service_unit(ServiceKind::Launchd, &exe(), "work");
        assert!(plist.contains("<key>LimitLoadToSessionType</key>\n    <string>Aqua</string>"));
    }

    /// The whole point of generating these: a unit that pins either variable builds a session the
    /// rest of the machine cannot see.
    #[test]
    fn no_unit_sets_a_socket_dir_or_a_tmpdir() {
        for kind in [ServiceKind::Systemd, ServiceKind::Launchd] {
            let unit = service_unit(kind, &exe(), "work");
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
