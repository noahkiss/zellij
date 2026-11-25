//! Zellij program-wide constants.

use crate::home::find_default_config_dir;
use directories::ProjectDirs;
use include_dir::{include_dir, Dir};
use lazy_static::lazy_static;
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
use uuid::Uuid;

pub const ZELLIJ_CONFIG_FILE_ENV: &str = "ZELLIJ_CONFIG_FILE";
pub const ZELLIJ_CONFIG_DIR_ENV: &str = "ZELLIJ_CONFIG_DIR";
pub const ZELLIJ_LAYOUT_DIR_ENV: &str = "ZELLIJ_LAYOUT_DIR";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The upstream tag this fork is a patch series on top of.
///
/// A rebase onto a newer upstream tag must update this and [`UPSTREAM_BASE_COMMIT`] - see FORK.md.
/// It is spelled literally rather than derived from git, so a build from a tarball or a shallow
/// checkout reports the same answer as a build from a full clone.
pub const UPSTREAM_BASE_TAG: &str = "v0.46.0";

/// The commit [`UPSTREAM_BASE_TAG`] points at, abbreviated. `v0.46.0` is an annotated tag, so this
/// is `git rev-parse --short 'v0.46.0^{commit}'` - `--short v0.46.0` gives the tag object instead.
pub const UPSTREAM_BASE_COMMIT: &str = "aedee21cb";

/// The day this binary was built, if whoever built it said so.
///
/// Unset in an ordinary `cargo build`, which keeps a local rebuild byte-identical; the release
/// workflow sets it, so a shipped artifact carries a date.
pub const BUILD_DATE: Option<&str> = option_env!("ZELLIJ_BUILD_DATE");

/// The version, plus the upstream base it was cut from, for `zellij --version`.
///
/// The FIRST LINE IS THE VERSION AND NOTHING ELSE, because scripts read it. `zellij-mac-setup`
/// takes the last whitespace-separated field of `zellij --version | head -1`, so anything appended
/// to that line would be read as the version number. The base goes on a second line, where a
/// `head -1` reader never sees it.
pub fn version_long() -> &'static str {
    static VERSION_LONG: OnceLock<String> = OnceLock::new();
    VERSION_LONG.get_or_init(|| match BUILD_DATE {
        Some(date) => format!(
            "{}\nupstream {} @ {}, built {}",
            VERSION, UPSTREAM_BASE_TAG, UPSTREAM_BASE_COMMIT, date
        ),
        None => format!(
            "{}\nupstream {} @ {}",
            VERSION, UPSTREAM_BASE_TAG, UPSTREAM_BASE_COMMIT
        ),
    })
}

pub const DEFAULT_SCROLL_BUFFER_SIZE: usize = 10_000;
pub static SCROLL_BUFFER_SIZE: OnceLock<usize> = OnceLock::new();
pub static DEBUG_MODE: OnceLock<bool> = OnceLock::new();

/// System-wide configuration directory, named after the running distribution
/// (`/etc/zellij`, `C:\ProgramData\Zellij`)
#[cfg(not(windows))]
pub fn system_default_config_dir() -> PathBuf {
    PathBuf::from("/etc").join(crate::distribution::name())
}

#[cfg(windows)]
pub fn system_default_config_dir() -> PathBuf {
    PathBuf::from("C:\\ProgramData").join(crate::distribution::display_name())
}

pub const SYSTEM_DEFAULT_DATA_DIR_PREFIX: &str = system_default_data_dir();

pub static ZELLIJ_DEFAULT_THEMES: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/themes");

pub const DEFAULT_THEME_NAME: &str = "default";
pub const DEFAULT_LIGHT_THEME_NAME: &str = "default-light";

pub const CLIENT_SERVER_CONTRACT_VERSION: usize = 1;

// The session-info cache is keyed by session id (a short random id for new sessions,
// or the session name for sessions from before ids), NOT the user-visible
// display name — so renaming a session never moves its cache folder.
pub fn session_info_cache_file_name(session_id: &str) -> PathBuf {
    session_info_folder_for_session(session_id).join("session-metadata.kdl")
}

/// The file name zellij writes a serialized session's layout under, in the in-place resurrection
/// cache and inside a snapshot alike. It is what tells a layout that describes a session that
/// really ran apart from one a person authored - see `CliAssets::layout_is_a_serialized_session`.
pub const SERIALIZED_SESSION_LAYOUT_FILE_NAME: &str = "session-layout.kdl";

pub fn session_layout_cache_file_name(session_id: &str) -> PathBuf {
    session_info_folder_for_session(session_id).join(SERIALIZED_SESSION_LAYOUT_FILE_NAME)
}

/// The file a server writes into its own session_info folder as it starts: its pid, then its
/// version. `ls --json` reads it to name the version of a server that is not its own build. It
/// describes a process, not a session's shape, so snapshots neither hash it nor copy it.
pub const SERVER_VERSION_STAMP_FILE_NAME: &str = "server-version";

pub fn server_version_stamp_file_name(session_id: &str) -> PathBuf {
    session_info_folder_for_session(session_id).join(SERVER_VERSION_STAMP_FILE_NAME)
}

pub fn session_info_folder_for_session(session_id: &str) -> PathBuf {
    ZELLIJ_SESSION_INFO_CACHE_DIR.join(session_id)
}

/// Length of a generated session id (hex chars). Kept short so the socket/pipe
/// path stays within the platform limit (104 bytes on macOS) even under a long
/// temp dir — a full 36-char UUID would overflow it. The client checks the socket
/// directory against this length at startup. See `generate_session_id`.
pub const SESSION_ID_LENGTH: usize = 12;

pub fn create_config_and_cache_folders() {
    if let Err(e) = std::fs::create_dir_all(&ZELLIJ_CACHE_DIR.as_path()) {
        log::error!("Failed to create cache dir: {:?}", e);
    }
    if let Some(config_dir) = find_default_config_dir() {
        if let Err(e) = std::fs::create_dir_all(&config_dir.as_path()) {
            log::error!("Failed to create config dir: {:?}", e);
        }
    }
    // while session_info is a child of cache currently, it won't necessarily always be this way,
    // and so it's explicitly created here
    if let Err(e) = std::fs::create_dir_all(&ZELLIJ_SESSION_INFO_CACHE_DIR.as_path()) {
        log::error!("Failed to create session_info cache dir: {:?}", e);
    }
    prune_empty_session_info_folders();
}

fn prune_empty_session_info_folders() {
    let Ok(entries) = std::fs::read_dir(&*ZELLIJ_SESSION_INFO_CACHE_DIR) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let is_empty = std::fs::read_dir(&path)
            .ok()
            .map_or(false, |mut iter| iter.next().is_none());
        if is_empty {
            if let Err(e) = std::fs::remove_dir(&path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    log::debug!("Failed to prune empty session folder {:?}: {:?}", path, e);
                }
            }
        }
    }
}

const fn system_default_data_dir() -> &'static str {
    if let Some(data_dir) = std::option_env!("PREFIX") {
        data_dir
    } else if cfg!(windows) {
        "C:\\ProgramData\\Zellij"
    } else {
        "/usr"
    }
}

lazy_static! {
    pub static ref CLIENT_SERVER_CONTRACT_DIR: String =
        format!("contract_version_{}", CLIENT_SERVER_CONTRACT_VERSION);
    pub static ref ZELLIJ_PROJ_DIR: ProjectDirs = {
        let (qualifier, organization, application) =
            crate::distribution::distribution().project_dirs();
        ProjectDirs::from(qualifier, organization, application).unwrap_or_else(|| {
            panic!(
                "could not determine the platform directories for distribution '{}' \
                 (qualifier: {:?}, organization: {:?}, application: {:?})",
                crate::distribution::name(),
                qualifier,
                organization,
                application
            )
        })
    };
    pub static ref ZELLIJ_CACHE_DIR: PathBuf = ZELLIJ_PROJ_DIR.cache_dir().to_path_buf();
    pub static ref ZELLIJ_SESSION_CACHE_DIR: PathBuf = ZELLIJ_PROJ_DIR
        .cache_dir()
        .to_path_buf()
        .join(format!("{}", Uuid::new_v4()));
    pub static ref ZELLIJ_PLUGIN_PERMISSIONS_CACHE: PathBuf =
        ZELLIJ_CACHE_DIR.join("permissions.kdl");
    pub static ref ZELLIJ_SESSION_INFO_CACHE_DIR: PathBuf = ZELLIJ_CACHE_DIR
        .join(CLIENT_SERVER_CONTRACT_DIR.clone())
        .join("session_info");
    /// Where state that must outlive the cache lives.
    ///
    /// Session snapshots are state, not cache: they have to survive a cache wipe, an upgrade and a
    /// client/server contract bump. `$XDG_STATE_HOME` is checked by hand because `directories`
    /// honours the XDG variables on Linux but ignores them on macOS, where it hardcodes the
    /// `Library` paths - checking it ourselves is what makes a Mac with XDG variables exported
    /// behave like the Linux boxes. Explicit configuration wins, platform convention is the
    /// fallback.
    pub static ref ZELLIJ_STATE_DIR: PathBuf = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("zellij"))
        .or_else(|| ZELLIJ_PROJ_DIR.state_dir().map(Path::to_path_buf))
        .unwrap_or_else(|| ZELLIJ_PROJ_DIR.data_dir().to_path_buf());
    pub static ref ZELLIJ_SNAPSHOT_DIR: PathBuf = ZELLIJ_STATE_DIR.join("snapshots");
    pub static ref ZELLIJ_PLUGIN_ARTIFACT_DIR: PathBuf = ZELLIJ_CACHE_DIR.join(VERSION);
    pub static ref ZELLIJ_SEEN_RELEASE_NOTES_CACHE_FILE: PathBuf =
        ZELLIJ_CACHE_DIR.join(VERSION).join("seen_release_notes");
}

pub const FEATURES: &[&str] = &[
    #[cfg(feature = "disable_automatic_asset_installation")]
    "disable_automatic_asset_installation",
];

#[cfg(unix)]
pub fn is_ipc_socket(file_type: &std::fs::FileType) -> bool {
    use std::os::unix::fs::FileTypeExt;
    file_type.is_socket()
}

#[cfg(not(unix))]
pub fn is_ipc_socket(file_type: &std::fs::FileType) -> bool {
    file_type.is_file()
}

#[cfg(unix)]
pub fn ipc_connect(path: &std::path::Path) -> std::io::Result<interprocess::local_socket::Stream> {
    use interprocess::local_socket::{prelude::*, GenericFilePath, Stream as LocalSocketStream};
    let fs_name = path.to_fs_name::<GenericFilePath>()?;
    LocalSocketStream::connect(fs_name)
}

#[cfg(windows)]
pub fn ipc_connect(path: &std::path::Path) -> std::io::Result<interprocess::local_socket::Stream> {
    use interprocess::local_socket::{prelude::*, GenericNamespaced, Stream as LocalSocketStream};
    let name = path.to_string_lossy().to_string();
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;
    let stream = LocalSocketStream::connect(ns_name)?;
    crate::pipe_access::check_owner(&stream)?;
    Ok(stream)
}

#[cfg(unix)]
pub fn ipc_bind(path: &std::path::Path) -> std::io::Result<interprocess::local_socket::Listener> {
    use interprocess::local_socket::{prelude::*, GenericFilePath, ListenerOptions};
    let fs_name = path.to_fs_name::<GenericFilePath>()?;
    ListenerOptions::new().name(fs_name).create_sync()
}

#[cfg(windows)]
pub fn ipc_bind(path: &std::path::Path) -> std::io::Result<interprocess::local_socket::Listener> {
    use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    let name = path.to_string_lossy().to_string();
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new()
        .name(ns_name)
        .security_descriptor(crate::pipe_access::private_descriptor()?)
        .create_sync()?;
    std::fs::write(path, std::process::id().to_string())?;
    Ok(listener)
}

#[cfg(unix)]
pub fn ipc_bind_async(
    path: &std::path::Path,
) -> std::io::Result<interprocess::local_socket::tokio::Listener> {
    use interprocess::local_socket::{prelude::*, GenericFilePath, ListenerOptions};
    let fs_name = path.to_fs_name::<GenericFilePath>()?;
    ListenerOptions::new().name(fs_name).create_tokio()
}

#[cfg(windows)]
pub fn ipc_bind_async(
    path: &std::path::Path,
) -> std::io::Result<interprocess::local_socket::tokio::Listener> {
    use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    let name = path.to_string_lossy().to_string();
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;
    let listener = ListenerOptions::new()
        .name(ns_name)
        .security_descriptor(crate::pipe_access::private_descriptor()?)
        .create_tokio()?;
    std::fs::write(path, std::process::id().to_string())?;
    Ok(listener)
}

#[cfg(windows)]
pub fn ipc_connect_reply(
    path: &std::path::Path,
) -> std::io::Result<interprocess::local_socket::Stream> {
    use interprocess::local_socket::{prelude::*, GenericNamespaced, Stream as LocalSocketStream};
    let name = format!("{}-reply", path.to_string_lossy());
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;
    let stream = LocalSocketStream::connect(ns_name)?;
    crate::pipe_access::check_owner(&stream)?;
    Ok(stream)
}

#[cfg(windows)]
pub fn ipc_bind_reply(
    path: &std::path::Path,
) -> std::io::Result<interprocess::local_socket::Listener> {
    use interprocess::local_socket::{prelude::*, GenericNamespaced, ListenerOptions};
    use interprocess::os::windows::local_socket::ListenerOptionsExt;
    let name = format!("{}-reply", path.to_string_lossy());
    let ns_name = name.to_ns_name::<GenericNamespaced>()?;
    ListenerOptions::new()
        .name(ns_name)
        .security_descriptor(crate::pipe_access::private_descriptor()?)
        .create_sync()
}

// Kept short: macOS limits socket paths to 104 bytes, and the temp dir already uses about half.
pub const APP_DIR_NAME: &str = ".desk";
pub const APP_CONTROL_SOCKET_NAME: &str = "control";
pub const APP_ATTENTION_SOCKET_NAME: &str = "attention";
pub const APP_LOCK_FILE_NAME: &str = "lock";
pub const APP_PANEL_SESSION: &str = ".zj-panel";
pub const APP_QUIT_REQUEST: &str = ".zellij-app-quit";

pub fn is_app_socket_name(name: &str) -> bool {
    name.starts_with('.')
}

pub fn app_dir_in(sock_dir: &std::path::Path) -> PathBuf {
    sock_dir.join(APP_DIR_NAME)
}

pub fn app_dir() -> PathBuf {
    app_dir_in(&ZELLIJ_SOCK_DIR)
}

pub fn app_control_socket() -> PathBuf {
    app_dir().join(APP_CONTROL_SOCKET_NAME)
}

pub fn app_attention_socket() -> PathBuf {
    app_dir().join(APP_ATTENTION_SOCKET_NAME)
}

pub fn app_lock_file() -> PathBuf {
    app_dir().join(APP_LOCK_FILE_NAME)
}

pub fn session_socket_path_in(sock_dir: &std::path::Path, session_name: &str) -> PathBuf {
    if session_name == APP_PANEL_SESSION {
        app_dir_in(sock_dir).join(session_name)
    } else {
        sock_dir.join(session_name)
    }
}

#[cfg(not(target_family = "wasm"))]
pub fn session_socket_path(session_name: &str) -> PathBuf {
    crate::sessions::resolve_session_socket_path(session_name)
        .unwrap_or_else(|| session_socket_path_in(&ZELLIJ_SOCK_DIR, session_name))
}

#[cfg(not(target_family = "wasm"))]
pub fn new_session_socket_path_in(sock_dir: &std::path::Path, session_name: &str) -> PathBuf {
    if is_app_socket_name(session_name) {
        session_socket_path_in(sock_dir, session_name)
    } else {
        sock_dir.join(crate::sessions::register_session(session_name))
    }
}

pub fn session_id_socket_path(session_id: &str) -> PathBuf {
    session_socket_path_in(&ZELLIJ_SOCK_DIR, session_id)
}

pub fn prepare_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    crate::shared::set_permissions(dir, 0o700)
}

#[cfg(unix)]
pub use unix_only::*;

#[cfg(unix)]
mod unix_only {
    use super::*;
    use crate::envs;
    pub use crate::shared::set_permissions;
    use lazy_static::lazy_static;
    use nix::unistd::Uid;
    use std::env::temp_dir;

    // Maximum sockaddr_un.sun_path length: 104 on macOS/BSD, 108 on Linux/Android/Solaris.
    #[cfg(target_os = "macos")]
    pub const ZELLIJ_SOCK_MAX_LENGTH: usize = 104;
    #[cfg(not(target_os = "macos"))]
    pub const ZELLIJ_SOCK_MAX_LENGTH: usize = 108;

    pub const SOCKET_PATH_LENGTH_UNIT: &str = "bytes";

    /// A socket path's length as `ZELLIJ_SOCK_MAX_LENGTH` counts it.
    pub fn socket_path_length(path: &std::path::Path) -> usize {
        path.as_os_str().len()
    }

    lazy_static! {
        static ref UID: Uid = Uid::current();
        pub static ref ZELLIJ_TMP_DIR: PathBuf =
            temp_dir().join(format!("{}-{}", crate::distribution::name(), *UID));
        pub static ref ZELLIJ_TMP_LOG_DIR: PathBuf =
            ZELLIJ_TMP_DIR.join(format!("{}-log", crate::distribution::name()));
        pub static ref ZELLIJ_TMP_LOG_FILE: PathBuf =
            ZELLIJ_TMP_LOG_DIR.join(format!("{}.log", crate::distribution::name()));
        pub static ref ZELLIJ_SOCK_DIR: PathBuf = {
            let mut ipc_dir = envs::get_socket_dir().map_or_else(
                |_| {
                    ZELLIJ_PROJ_DIR
                        .runtime_dir()
                        .map_or_else(|| ZELLIJ_TMP_DIR.clone(), |p| p.to_owned())
                },
                PathBuf::from,
            );
            ipc_dir.push(CLIENT_SERVER_CONTRACT_DIR.clone());
            ipc_dir
        };
        pub static ref WEBSERVER_SOCKET_PATH: PathBuf = ZELLIJ_SOCK_DIR.join("web_server_bus");
    }

    /// Every socket root this build could resolve to, in the order `ZELLIJ_SOCK_DIR` tries them.
    ///
    /// The chosen one is first; the rest are the roots a differently-configured environment (a
    /// different `ZELLIJ_SOCKET_DIR`, a missing `XDG_RUNTIME_DIR`, a different `TMPDIR`) would
    /// have landed in. These are roots — the sockets themselves live one level down, under
    /// `CLIENT_SERVER_CONTRACT_DIR`.
    pub fn socket_dir_candidates() -> Vec<PathBuf> {
        let mut candidates = vec![];
        let mut push = |dir: PathBuf| {
            if !candidates.contains(&dir) {
                candidates.push(dir);
            }
        };
        if let Ok(dir) = envs::get_socket_dir() {
            push(PathBuf::from(dir));
        }
        if let Some(runtime_dir) = ZELLIJ_PROJ_DIR.runtime_dir() {
            push(runtime_dir.to_owned());
        }
        push(ZELLIJ_TMP_DIR.clone());
        push(PathBuf::from("/tmp").join(format!("zellij-{}", *UID)));
        // fork addition: the roots a login environment would have resolved to, for a shell that
        // lacks the variable that names it - a cron job or an ssh command with no
        // XDG_RUNTIME_DIR on Linux, or no per-user TMPDIR on macOS. Without these a session
        // started from the person's login is invisible to the scan that looks for a split.
        #[cfg(target_os = "linux")]
        push(
            PathBuf::from("/run/user")
                .join(UID.to_string())
                .join("zellij"),
        );
        #[cfg(target_os = "macos")]
        if let Some(user_temp) = darwin_user_temp_dir() {
            push(user_temp.join(format!("{}-{}", crate::distribution::name(), *UID)));
        }
        candidates
    }

    /// fork addition: the per-user temporary directory macOS gives a login (`$TMPDIR` there),
    /// read from the system rather than from the environment, so a shell without `TMPDIR` can
    /// still name it.
    #[cfg(target_os = "macos")]
    fn darwin_user_temp_dir() -> Option<PathBuf> {
        use std::os::unix::ffi::OsStrExt;
        let mut buf = vec![0 as libc::c_char; 1024];
        // SAFETY: the buffer is writable for its whole length, and confstr writes at most that
        // many bytes, NUL included
        let needed =
            unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
        if needed == 0 || needed > buf.len() {
            return None;
        }
        let bytes: Vec<u8> = buf[..needed - 1].iter().map(|c| *c as u8).collect();
        let path = PathBuf::from(std::ffi::OsStr::from_bytes(&bytes));
        path.is_absolute().then_some(path)
    }
}

#[cfg(not(unix))]
pub use not_unix::*;

#[cfg(not(unix))]
mod not_unix {
    use super::*;
    use crate::envs;
    pub use crate::shared::set_permissions;
    #[cfg(windows)]
    use dunce;
    use lazy_static::lazy_static;
    use std::env::temp_dir;

    #[cfg(windows)]
    fn canonicalize_path(path: PathBuf) -> PathBuf {
        dunce::canonicalize(&path).unwrap_or(path)
    }

    #[cfg(not(windows))]
    fn canonicalize_path(path: PathBuf) -> PathBuf {
        path
    }

    // A pipe's whole name, `\\.\pipe\` + the socket path + `-reply`, must fit in MAX_PATH (260
    // UTF-16 units with its NUL), since Zellij isn't long-path aware.
    pub const ZELLIJ_SOCK_MAX_LENGTH: usize = 260 - r"\\.\pipe\".len() - "-reply".len();

    pub const SOCKET_PATH_LENGTH_UNIT: &str = "characters";

    /// A socket path's length as `ZELLIJ_SOCK_MAX_LENGTH` counts it: in UTF-16 units, as Windows
    /// counts a pipe name.
    #[cfg(windows)]
    pub fn socket_path_length(path: &std::path::Path) -> usize {
        use std::os::windows::ffi::OsStrExt;
        path.as_os_str().encode_wide().count()
    }

    #[cfg(not(windows))]
    pub fn socket_path_length(path: &std::path::Path) -> usize {
        path.as_os_str().len()
    }

    lazy_static! {
        pub static ref ZELLIJ_TMP_DIR: PathBuf = {
            let tmp_dir = canonicalize_path(temp_dir());
            tmp_dir.join(crate::distribution::name())
        };
        pub static ref ZELLIJ_TMP_LOG_DIR: PathBuf =
            ZELLIJ_TMP_DIR.join(format!("{}-log", crate::distribution::name()));
        pub static ref ZELLIJ_TMP_LOG_FILE: PathBuf =
            ZELLIJ_TMP_LOG_DIR.join(format!("{}.log", crate::distribution::name()));
        pub static ref ZELLIJ_SOCK_DIR: PathBuf = {
            let mut ipc_dir = canonicalize_path(envs::get_socket_dir().map_or_else(
                |_| {
                    ZELLIJ_PROJ_DIR
                        .runtime_dir()
                        .map_or_else(|| ZELLIJ_TMP_DIR.clone(), |p| p.to_owned())
                },
                PathBuf::from,
            ));
            ipc_dir.push(CLIENT_SERVER_CONTRACT_DIR.clone());
            ipc_dir
        };
        pub static ref WEBSERVER_SOCKET_PATH: PathBuf = ZELLIJ_SOCK_DIR.join("web_server_bus");
    }

    /// See the unix counterpart. Named pipes are not filesystem paths here, so the only
    /// candidate is the one in use.
    pub fn socket_dir_candidates() -> Vec<PathBuf> {
        vec![ZELLIJ_SOCK_DIR.clone()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `zellij-mac-setup` reads the version as the last whitespace-separated field of
    /// `zellij --version | head -1`, and feeds it to a numeric comparison. Anything appended to
    /// that line is read as the version number instead, and the comparison then refuses to run.
    #[test]
    fn the_first_line_of_the_long_version_is_the_version_and_nothing_else() {
        let first_line = version_long().lines().next().unwrap();
        assert_eq!(first_line, VERSION);
        assert_eq!(first_line.rsplit(' ').next().unwrap(), VERSION);
    }

    #[test]
    fn the_long_version_names_the_upstream_base_it_was_cut_from() {
        let base_line = version_long().lines().nth(1).unwrap();
        assert!(
            base_line.contains(UPSTREAM_BASE_TAG),
            "expected the base tag in {:?}",
            base_line
        );
        assert!(
            base_line.contains(UPSTREAM_BASE_COMMIT),
            "expected the base commit in {:?}",
            base_line
        );
    }

    /// The tag is a tag and the commit is an abbreviated sha, so a rebase that updates one and
    /// forgets the other is caught here rather than in a release note.
    #[test]
    fn the_base_tag_and_commit_are_spelled_the_way_git_spells_them() {
        assert!(UPSTREAM_BASE_TAG.starts_with('v'));
        assert!(UPSTREAM_BASE_COMMIT.len() >= 7);
        assert!(UPSTREAM_BASE_COMMIT
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
    }
}

#[cfg(test)]
mod app_dir_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn every_desktop_app_socket_fits_the_macos_limit_with_room_to_spare() {
        const MACOS_SOCKET_PATH_LIMIT: usize = 104;
        // Leaves room for a longer uid and contract version than we have today.
        let sock_dir = Path::new(
            "/var/folders/g_/w_mkp3kj0jnbzm398bln58rm0000gn/T/zellij-50100/contract_version_100",
        );
        let app = app_dir_in(sock_dir);
        for path in [
            session_socket_path_in(sock_dir, APP_PANEL_SESSION),
            app.join(APP_CONTROL_SOCKET_NAME),
            app.join(APP_ATTENTION_SOCKET_NAME),
            app.join("windows").join("99999"),
        ] {
            let length = path.as_os_str().len();
            assert!(
                length < MACOS_SOCKET_PATH_LIMIT,
                "{:?} is {} bytes",
                path,
                length
            );
        }
    }

    #[test]
    fn the_desktop_app_files_and_its_panel_session_live_in_their_own_folder() {
        let sock_dir = Path::new("/run/zellij/contract_version_1");
        let app = sock_dir.join(APP_DIR_NAME);
        assert_eq!(app_dir_in(sock_dir), app);
        assert_eq!(
            session_socket_path_in(sock_dir, APP_PANEL_SESSION),
            app.join(APP_PANEL_SESSION)
        );
        assert_eq!(
            session_socket_path_in(sock_dir, "api"),
            sock_dir.join("api")
        );
        for name in [
            APP_CONTROL_SOCKET_NAME,
            APP_ATTENTION_SOCKET_NAME,
            APP_LOCK_FILE_NAME,
        ] {
            assert!(!name.contains('/'));
        }
    }

    #[test]
    fn a_session_listing_that_keeps_only_sockets_never_sees_the_app_folder() {
        let dir = tempfile::tempdir().unwrap();
        prepare_private_dir(&app_dir_in(dir.path())).unwrap();
        let listed: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|t| is_ipc_socket(&t))
                    .unwrap_or(false)
            })
            .collect();
        assert!(listed.is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(app_dir_in(dir.path()))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}

#[cfg(all(test, windows))]
mod socket_length_tests {
    use super::*;

    #[test]
    fn the_longest_socket_path_allowed_still_binds_and_connects() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut path = dir.path().to_path_buf();
        let room = ZELLIJ_SOCK_MAX_LENGTH - 1 - socket_path_length(&path) - 1;
        path.push("a".repeat(room));
        assert_eq!(socket_path_length(&path), ZELLIJ_SOCK_MAX_LENGTH - 1);
        let _requests = ipc_bind(&path).unwrap();
        let _replies = ipc_bind_reply(&path).unwrap();
        crate::pipe_pairs::connect_pair(&path).unwrap();
    }
}
