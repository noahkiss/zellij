use crate::data::{Direction, InputMode, PaneSignal, Resize, UnblockCondition};
use crate::setup::Setup;
use crate::{
    consts::{ZELLIJ_CONFIG_DIR_ENV, ZELLIJ_CONFIG_FILE_ENV},
    input::{
        layout::PluginUserConfiguration,
        options::{Options, PaneFrameStyle},
    },
};
use clap::builder::styling::{AnsiColor, Color, Style, Styles};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use std::path::PathBuf;
use url::Url;

const fn ansi(color: AnsiColor) -> Style {
    Style::new().fg_color(Some(Color::Ansi(color)))
}

const CLI_STYLES: Styles = Styles::styled()
    .header(ansi(AnsiColor::Yellow))
    .usage(ansi(AnsiColor::Yellow))
    .literal(ansi(AnsiColor::Green))
    .placeholder(Style::new())
    .error(ansi(AnsiColor::Red))
    .valid(ansi(AnsiColor::Green))
    .invalid(ansi(AnsiColor::Yellow));

fn validate_session(name: &str) -> Result<String, String> {
    #[cfg(unix)]
    {
        use crate::consts::ZELLIJ_SOCK_MAX_LENGTH;

        let mut socket_path = crate::consts::ZELLIJ_SOCK_DIR.clone();
        socket_path.push(name);

        if socket_path.as_os_str().len() >= ZELLIJ_SOCK_MAX_LENGTH {
            // socket path must be less than 108 bytes
            let available_length = ZELLIJ_SOCK_MAX_LENGTH
                .saturating_sub(socket_path.as_os_str().len())
                .saturating_sub(1);

            return Err(format!(
                "session name must be less than {} characters",
                available_length
            ));
        };
    };

    Ok(name.to_owned())
}

#[derive(Parser, Default, Debug, Clone, Serialize, Deserialize)]
#[clap(
    version,
    name = "zellij",
    about = "A terminal workspace with batteries included",
    long_about = "A terminal workspace with batteries included.

`zellij action <verb>` drives a session that is already running - reading it, moving around it, changing it - and `zellij action --help` states the conventions all of those verbs share.

`zellij setup --dump-surface` prints the whole command tree in one call: every command, its flags with their types and defaults, and what it puts on stdout.",
    styles = CLI_STYLES,
    args_override_self = true
)]
pub struct CliArgs {
    /// Maximum panes on screen, caution: opening more panes will close old ones
    #[clap(long, value_parser)]
    pub max_panes: Option<usize>,

    /// Change where zellij looks for plugins
    #[clap(long, value_parser, overrides_with = "data_dir")]
    pub data_dir: Option<PathBuf>,

    /// Run server listening at the specified socket path
    #[clap(long, value_parser, hide = true, overrides_with = "server")]
    pub server: Option<PathBuf>,

    /// Specify name of a new session
    #[clap(long, short, overrides_with = "session", value_parser = validate_session)]
    pub session: Option<String>,

    /// Name of a predefined layout inside the layout directory or the path to a layout file
    /// if inside a session (or using the --session flag) will be added to the session as a new tab
    /// or tabs, otherwise will start a new session
    #[clap(short, long, value_parser, overrides_with = "layout")]
    pub layout: Option<PathBuf>,

    /// Raw KDL layout string to use directly (instead of a file path)
    /// if inside a session (or using the --session flag) will be added to the session as a new tab
    /// or tabs, otherwise will start a new session
    #[clap(long, value_parser, conflicts_with_all = &["layout", "new_session_with_layout"])]
    pub layout_string: Option<String>,

    /// Name of a predefined layout inside the layout directory or the path to a layout file
    /// Will always start a new session, even if inside an existing session
    #[clap(short, long, value_parser, overrides_with = "new_session_with_layout")]
    pub new_session_with_layout: Option<PathBuf>,

    /// Change where zellij looks for the configuration file
    #[clap(short, long, overrides_with = "config", env = ZELLIJ_CONFIG_FILE_ENV, value_parser)]
    pub config: Option<PathBuf>,

    /// Change where zellij looks for the configuration directory
    #[clap(long, overrides_with = "config_dir", env = ZELLIJ_CONFIG_DIR_ENV, value_parser)]
    pub config_dir: Option<PathBuf>,

    #[clap(subcommand)]
    pub command: Option<Command>,

    /// Specify emitting additional debug information
    #[clap(short, long, value_parser)]
    pub debug: bool,
}

impl CliArgs {
    pub fn is_setup_clean(&self) -> bool {
        if let Some(Command::Setup(ref setup)) = &self.command {
            if setup.clean {
                return true;
            }
        }
        false
    }
    pub fn options(&self) -> Option<Options> {
        if let Some(Command::Options(options)) = &self.command {
            return Some(options.clone());
        }
        None
    }
}

#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Change the behaviour of zellij
    #[clap(name = "options", value_parser)]
    Options(Options),

    /// Setup zellij and check its configuration
    #[clap(name = "setup", value_parser)]
    Setup(Setup),

    /// Run a web server to serve terminal sessions
    #[clap(name = "web", value_parser)]
    Web(WebCli),

    /// Drive a running session: read it, move around it, change it
    ///
    /// `zellij action --help` states the conventions every one of these verbs follows.
    #[clap(visible_alias = "ac")]
    #[clap(subcommand)]
    Action(Box<CliAction>),

    /// Explore existing zellij sessions
    #[clap(flatten)]
    Sessions(Sessions),

    /// Subscribe to pane render updates (viewport and scrollback)
    #[clap(override_usage(
        "zellij [--session <OTHER SESSION NAME>] subscribe [OPTIONS] --pane-id..."
    ))]
    Subscribe(SubscribeCli),
}

#[derive(Debug, Parser, Clone, Serialize, Deserialize)]
pub struct SubscribeCli {
    /// Pane ID(s) to subscribe to (terminal_1, plugin_2, or a bare number like 1)
    #[clap(
        short,
        long,
        required = true,
        num_args(1..)
    )]
    pub pane_id: Vec<String>,

    /// Include scrollback lines in initial delivery.
    /// Bare --scrollback = all scrollback, --scrollback N = last N lines.
    #[clap(
        short,
        long,
        default_missing_value = "0",
        num_args(0..=1)
    )]
    pub scrollback: Option<usize>,

    /// Output format
    #[clap(short, long, default_value = "raw", value_enum)]
    pub format: SubscribeFormat,

    /// Preserve ANSI styling in the output
    #[clap(long)]
    pub ansi: bool,

    /// Stamp every line with the UTC time it was printed: `2026-08-14T18:03:12.345Z ` before each
    /// raw line, a `ts` key in each json object. It is when this client printed the line, not when
    /// the pane produced it
    #[clap(long)]
    pub timestamps: bool,
}

/// The stamp `subscribe --timestamps` puts on a line: RFC3339, UTC, to the millisecond.
///
/// It is taken once per update, so every line of one render shares it - the lines were printed in
/// one go and a stamp that drifted across them would say otherwise. It is a *print* time: the
/// client's clock when the line left it, which is later than the pane's output by however long the
/// render and the socket took, and is the only time this side of the connection can honestly claim.
pub fn event_timestamp(at: std::time::SystemTime) -> String {
    humantime::format_rfc3339_millis(at).to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, ValueEnum)]
pub enum SubscribeFormat {
    Raw,
    Json,
}

#[derive(Debug, Clone, Args, Serialize, Deserialize)]
pub struct WebCli {
    /// Start the server (default unless other arguments are specified)
    #[clap(long, value_parser, display_order = 1)]
    pub start: bool,

    /// Stop the server
    #[clap(long, value_parser, exclusive(true), display_order = 2)]
    pub stop: bool,

    /// Get the server status
    #[clap(long, value_parser, conflicts_with("start"), display_order = 3)]
    pub status: bool,

    /// Timeout in seconds for the status check (default: 30)
    #[clap(long, value_parser, requires = "status", display_order = 4)]
    pub timeout: Option<u64>,

    /// Run the server in the background
    #[clap(
        short,
        long,
        value_parser,
        conflicts_with_all(&["stop", "status", "create_token", "revoke_token", "revoke_all_tokens"]),
        display_order = 5
    )]
    pub daemonize: bool,
    /// Timeout in seconds waiting for the server to start (default: 10).
    /// Only used on Windows where the daemonized server is polled via TCP.
    /// On Unix, startup signaling uses pipes and this option is ignored.
    #[clap(long, value_parser, display_order = 6)]
    pub server_startup_timeout: Option<u64>,
    /// Create a login token for the web interface, will only be displayed once and cannot later be
    /// retrieved. Returns the token name and the token.
    #[clap(long, value_parser, exclusive(true), display_order = 7)]
    pub create_token: bool,
    /// Optional name for the token
    #[clap(long, value_parser, value_name = "TOKEN_NAME", display_order = 8)]
    pub token_name: Option<String>,
    /// Create a read-only login token (can only attach to existing sessions as watcher)
    #[clap(long, value_parser, exclusive(true), display_order = 9)]
    pub create_read_only_token: bool,
    /// Revoke a login token by its name
    #[clap(
        long,
        value_parser,
        exclusive(true),
        value_name = "TOKEN NAME",
        display_order = 10
    )]
    pub revoke_token: Option<String>,
    /// Revoke all login tokens
    #[clap(long, value_parser, exclusive(true), display_order = 11)]
    pub revoke_all_tokens: bool,
    /// List token names and their creation dates (cannot show actual tokens)
    #[clap(long, value_parser, exclusive(true), display_order = 12)]
    pub list_tokens: bool,
    /// The ip address to listen on locally for connections (defaults to 127.0.0.1)
    #[clap(
        long,
        value_parser,
        conflicts_with_all(&["stop", "create_token", "revoke_token", "revoke_all_tokens"]),
        display_order = 13
    )]
    pub ip: Option<IpAddr>,
    /// The port to listen on locally for connections (defaults to 8082)
    #[clap(
        long,
        value_parser,
        conflicts_with_all(&["stop", "create_token", "revoke_token", "revoke_all_tokens"]),
        display_order = 14
    )]
    pub port: Option<u16>,
    /// The path to the SSL certificate (required if not listening on 127.0.0.1)
    #[clap(
        long,
        value_parser,
        conflicts_with_all(&["stop", "status", "create_token", "revoke_token", "revoke_all_tokens"]),
        display_order = 15
    )]
    pub cert: Option<PathBuf>,
    /// The path to the SSL key (required if not listening on 127.0.0.1)
    #[clap(
        long,
        value_parser,
        conflicts_with_all(&["stop", "status", "create_token", "revoke_token", "revoke_all_tokens"]),
        display_order = 16
    )]
    pub key: Option<PathBuf>,
}

impl WebCli {
    pub fn get_start(&self) -> bool {
        self.start
            || !(self.stop
                || self.status
                || self.create_token
                || self.create_read_only_token
                || self.revoke_token.is_some()
                || self.revoke_all_tokens
                || self.list_tokens)
    }
}

#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum SessionCommand {
    /// Change the behaviour of zellij
    #[clap(name = "options")]
    Options(Options),
}

#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum Sessions {
    /// List active sessions
    ///
    /// Returns: one row per session - NAME, STATUS (live or exited), CURRENT, CLIENTS, CREATED.
    /// CREATED is last because it is the only column that holds spaces.
    #[clap(visible_alias = "ls")]
    ListSessions {
        /// Do not add colors to the table
        #[clap(short, long)]
        no_formatting: bool,

        /// Print just the session name, with ` (EXITED)` after a resurrectable one
        #[clap(short, long)]
        short: bool,

        /// List the sessions in reverse order (default is ascending order)
        #[clap(short, long)]
        reverse: bool,

        /// Output as JSON (overrides --short and --no-formatting)
        #[clap(short, long)]
        json: bool,
    },
    /// List existing plugin aliases
    #[clap(visible_alias = "la")]
    ListAliases,
    /// Attach to a session
    #[clap(visible_alias = "a")]
    Attach {
        /// Name of the session to attach to.
        #[clap(value_parser)]
        session_name: Option<String>,

        /// Create a session if one does not exist.
        #[clap(short, long, value_parser)]
        create: bool,

        /// Create a detached session in the background if one does not exist
        #[clap(short('b'), long, value_parser)]
        create_background: bool,

        /// Number of the session index in the active sessions ordered creation date.
        #[clap(long, value_parser)]
        index: Option<usize>,

        /// Change the behaviour of zellij
        #[clap(subcommand, name = "options")]
        options: Option<Box<SessionCommand>>,

        /// If resurrecting a dead session, immediately run all its commands on startup
        #[clap(short, long)]
        force_run_commands: bool,

        /// Ignore any resurrection snapshot for this session and build it fresh from the layout
        #[clap(long)]
        no_resurrect: bool,

        /// Rebuild the session from an archived snapshot instead of resurrecting it in place.
        /// Takes a snapshot id (a unique prefix is enough) and defaults to the newest snapshot
        /// for this session name
        #[clap(
            long,
            value_name = "ID",
            value_parser,
            num_args(0..=1),
            require_equals(false),
            default_missing_value("latest")
        )]
        restore: Option<String>,

        /// Authentication token for remote sessions
        #[clap(short('t'), long, value_parser)]
        token: Option<String>,

        /// Save session for automatic re-authentication (4 weeks)
        #[clap(short('r'), long, value_parser)]
        remember: bool,

        /// Delete saved session before connecting
        #[clap(long, value_parser)]
        forget: bool,

        /// Path to a custom CA certificate (PEM format) for verifying the remote server
        #[clap(long, value_name = "FILE", value_parser)]
        ca_cert: Option<PathBuf>,

        /// Skip TLS certificate validation (DANGEROUS — development only)
        #[clap(long, value_parser)]
        insecure: bool,
    },

    /// Attach to a session read-only, seeing what it shows without being able to type into it
    #[clap(visible_alias = "w")]
    Watch {
        /// Name of the session to watch
        #[clap(value_parser)]
        session_name: Option<String>,
    },

    /// Stop a session's server, keeping the session resurrectable
    ///
    /// Returns once the server process is gone, so a caller that rebuilds the session next gets a
    /// true answer. `delete-session` is what removes it for good.
    #[clap(visible_alias = "k")]
    KillSession {
        /// Name of target session
        #[clap(value_parser)]
        target_session: Option<String>,
        /// Return as soon as the kill has been acknowledged, without waiting for the server to exit
        #[clap(long)]
        no_wait: bool,
        /// Seconds to wait for the server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Delete a session's saved state, so it can no longer be resurrected
    ///
    /// A running session has to be killed first; --force does both.
    #[clap(visible_alias = "d")]
    DeleteSession {
        /// Name of target session
        #[clap(value_parser)]
        target_session: Option<String>,
        /// Kill the session if it's running before deleting it
        #[clap(short, long)]
        force: bool,
        /// Return as soon as the kill has been acknowledged, without waiting for the server to exit
        #[clap(long)]
        no_wait: bool,
        /// Seconds to wait for the server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Inspect and restore archived session snapshots
    #[clap(subcommand)]
    Snapshot(SnapshotCli),

    /// Create, tear down and restart a named session, asserting the result
    #[clap(subcommand)]
    Session(SessionLifecycleCli),

    /// Stop every session's server, keeping the sessions resurrectable
    ///
    /// Attempts every session and exits non-zero if any server did not go.
    #[clap(visible_alias = "ka")]
    KillAllSessions {
        /// Automatic yes to prompts
        #[clap(short, long, value_parser)]
        yes: bool,
        /// Return as soon as each kill has been acknowledged, without waiting for servers to exit
        #[clap(long)]
        no_wait: bool,
        /// Seconds to wait for each server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Delete every session's saved state, so none of them can be resurrected
    ///
    /// Attempts every session and exits non-zero if any of them did not go.
    #[clap(visible_alias = "da")]
    DeleteAllSessions {
        /// Automatic yes to prompts
        #[clap(short, long, value_parser)]
        yes: bool,
        /// Kill the sessions if they're running before deleting them
        #[clap(short, long)]
        force: bool,
        /// Return as soon as each kill has been acknowledged, without waiting for servers to exit
        #[clap(long)]
        no_wait: bool,
        /// Seconds to wait for each server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Run a command in a new pane
    ///
    /// Returns: `pane_id: terminal_<id>` and `handle: <two-word handle>`
    #[clap(visible_alias = "r")]
    Run {
        /// Command to run
        #[clap(last(true), required(true))]
        command: Vec<String>,

        /// Direction to open the new pane in
        #[clap(short, long, value_parser, conflicts_with("floating"))]
        direction: Option<Direction>,

        /// Change the working directory of the new pane
        #[clap(long, value_parser)]
        cwd: Option<PathBuf>,

        /// Open the new pane in floating mode
        #[clap(short, long)]
        floating: bool,

        /// Open the new pane in place of the current pane, temporarily suspending it
        #[clap(short, long, conflicts_with("floating"), conflicts_with("direction"))]
        in_place: bool,

        /// Close the replaced pane instead of suspending it (only effective with --in-place)
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,

        /// Name of the new pane
        #[clap(short, long, value_parser)]
        name: Option<String>,

        /// Close the pane immediately when its command exits
        #[clap(short, long)]
        close_on_exit: bool,

        /// Start the command suspended, only running after you first presses ENTER
        #[clap(short, long)]
        start_suspended: bool,

        /// The x coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        x: Option<String>,
        /// The y coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        y: Option<String>,
        /// The width if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        width: Option<String>,
        /// The height if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        height: Option<String>,
        /// Whether to pin a floating pane so that it is always on top
        #[clap(long, requires("floating"))]
        pinned: Option<bool>,
        /// Open the pane into a stack with the pane it opens beside. Fails if that pane cannot be
        /// stacked, rather than opening the pane somewhere else
        #[clap(long, conflicts_with("floating"), conflicts_with("direction"))]
        stacked: bool,
        /// Wait until the pane closes, then exit with the command's exit status. Waiting replaces
        /// the report: a blocking run prints no `pane_id:`, because the status is the answer
        #[clap(long)]
        blocking: bool,

        /// Wait until the command exits 0 or its pane closes, then exit with its status. Prints no
        /// `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_failure"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_success: bool,

        /// Wait until the command exits non-zero or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_failure: bool,

        /// Wait until the command exits either way or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit_failure")
        )]
        block_until_exit: bool,
        /// Open the pane beside the pane this command was run from, rather than beside whichever
        /// pane the user is focused on
        #[clap(long)]
        near_current_pane: bool,
        /// Open the pane beside the pane this command was run from and leave every client's focus
        /// where it is
        #[clap(long)]
        no_focus: bool,
        /// Draw the pane without a frame (warning: a borderless pane cannot be moved with the
        /// mouse)
        #[clap(short, long, value_parser)]
        borderless: Option<bool>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(
            long,
            value_parser,
            conflicts_with("near_current_pane"),
            conflicts_with("in_place")
        )]
        tab_id: Option<usize>,
    },
    /// Load a plugin in a new pane
    ///
    /// Returns: `pane_id: plugin_<id>` and `handle: <two-word handle>`
    #[clap(visible_alias = "p")]
    Plugin {
        /// Plugin URL, can either start with http(s), file: or zellij:
        #[clap(last(true), required(true))]
        url: String,

        /// Plugin configuration
        #[clap(short, long, value_parser)]
        configuration: Option<PluginUserConfiguration>,

        /// Open the new pane in floating mode
        #[clap(short, long)]
        floating: bool,

        /// Open the new pane in place of the current pane, temporarily suspending it
        #[clap(short, long, conflicts_with("floating"))]
        in_place: bool,

        /// Close the replaced pane instead of suspending it (only effective with --in-place)
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,

        /// Skip the memory and HD cache and force recompile of the plugin (good for development)
        #[clap(short, long)]
        skip_plugin_cache: bool,
        /// The x coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        x: Option<String>,
        /// The y coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        y: Option<String>,
        /// The width if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        width: Option<String>,
        /// The height if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        height: Option<String>,
        /// Whether to pin a floating pane so that it is always on top
        #[clap(long, requires("floating"))]
        pinned: Option<bool>,
        #[clap(
            long,
            help = "if set, will open the plugin pane without changing the focus of any client, placing it relative to the pane the command was issued from"
        )]
        no_focus: bool,
        /// Draw the pane without a frame (warning: a borderless pane cannot be moved with the
        /// mouse)
        #[clap(short, long, value_parser)]
        borderless: Option<bool>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(long, value_parser, conflicts_with("in_place"))]
        tab_id: Option<usize>,
    },
    /// Open a file in a new pane running your $EDITOR
    ///
    /// Returns: `pane_id: terminal_<id>` and `handle: <two-word handle>`
    #[clap(visible_alias = "e")]
    Edit {
        file: PathBuf,

        /// Open the file in the specified line number
        #[clap(short, long, value_parser)]
        line_number: Option<usize>,

        /// Direction to open the new pane in
        #[clap(short, long, value_parser, conflicts_with("floating"))]
        direction: Option<Direction>,

        /// Open the new pane in place of the current pane, temporarily suspending it
        #[clap(short, long, conflicts_with("floating"), conflicts_with("direction"))]
        in_place: bool,

        /// Close the replaced pane instead of suspending it (only effective with --in-place)
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,

        /// Open the new pane in floating mode
        #[clap(short, long)]
        floating: bool,

        /// Change the working directory of the editor
        #[clap(long, value_parser)]
        cwd: Option<PathBuf>,
        /// The x coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        x: Option<String>,
        /// The y coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        y: Option<String>,
        /// The width if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        width: Option<String>,
        /// The height if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        height: Option<String>,
        /// Whether to pin a floating pane so that it is always on top
        #[clap(long, requires("floating"))]
        pinned: Option<bool>,
        /// Open the pane beside the pane this command was run from, rather than beside whichever
        /// pane the user is focused on
        #[clap(long)]
        near_current_pane: bool,
        /// Open the pane beside the pane this command was run from and leave every client's focus
        /// where it is
        #[clap(long)]
        no_focus: bool,
        /// Draw the pane without a frame (warning: a borderless pane cannot be moved with the
        /// mouse)
        #[clap(short, long, value_parser)]
        borderless: Option<bool>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(
            long,
            value_parser,
            conflicts_with("near_current_pane"),
            conflicts_with("in_place")
        )]
        tab_id: Option<usize>,
    },
    /// Send data to one or more plugins, launch them if they are not running.
    #[clap(override_usage(
r#"
zellij pipe [OPTIONS] [--] <PAYLOAD>

* Send data to a specific plugin:

zellij pipe --plugin file:/path/to/my/plugin.wasm --name my_pipe_name -- my_arbitrary_data

* To all running plugins (that are listening):

zellij pipe --name my_pipe_name -- my_arbitrary_data

* Pipe data into this command's STDIN and get output from the plugin on this command's STDOUT

tail -f /tmp/my-live-logfile | zellij pipe --name logs --plugin https://example.com/my-plugin.wasm | wc -l
"#))]
    Pipe {
        /// The name of the pipe
        #[clap(short, long, value_parser, display_order(1))]
        name: Option<String>,
        /// The data to send down this pipe (if blank, will listen to STDIN)
        payload: Option<String>,

        #[clap(short, long, value_parser, display_order(2))]
        /// The args of the pipe
        args: Option<PluginUserConfiguration>, // TODO: we might want to not re-use
        // PluginUserConfiguration
        /// The plugin url (eg. file:/tmp/my-plugin.wasm) to direct this pipe to, if not specified,
        /// will be sent to all plugins, if specified and is not running, the plugin will be launched
        #[clap(short, long, value_parser, display_order(3))]
        plugin: Option<String>,
        /// The plugin configuration (note: the same plugin with different configuration is
        /// considered a different plugin for the purposes of determining the pipe destination)
        #[clap(short('c'), long, value_parser, display_order(4))]
        plugin_configuration: Option<PluginUserConfiguration>,
    },
}

/// The lifecycle of one named session: create it, remove it, replace it.
///
/// Each of these states a post-condition and checks it, rather than reporting what it asked for.
/// The session name defaults to the `session_name` config option where one is set, so a machine
/// that runs a single named session can say `zellij session up` and mean it.
#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum SessionLifecycleCli {
    /// Create the session in the background if it is not already up, then assert that it is
    Up {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// Build the session from an archived snapshot instead of from the layout. Takes a
        /// snapshot id (a unique prefix is enough) and defaults to the newest snapshot for this
        /// session name. Without it the session comes up FRESH from the layout, which is what
        /// makes a layout edit apply
        #[clap(
            long,
            value_name = "ID",
            value_parser,
            num_args(0..=1),
            require_equals(false),
            default_missing_value("latest")
        )]
        restore: Option<String>,
    },

    /// Remove the session, archiving a snapshot of its shape first, then assert that it is gone
    Down {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// Seconds to wait for the server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Take the session down and bring it back, from inside it or from anywhere else
    Restart {
        /// Name of the session, defaults to the current session, then to the `session_name`
        /// config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// Come back from the default layout instead of from the pre-restart shape. This is how a
        /// layout edit is applied
        #[clap(long, conflicts_with("restore"))]
        fresh: bool,

        /// Come back from a specific snapshot rather than the one the teardown archives
        #[clap(long, value_name = "ID", value_parser)]
        restore: Option<String>,

        /// Seconds to wait for the server to exit before giving up (exits 1 on timeout)
        #[clap(long, value_parser, default_value("10"))]
        wait_timeout: u64,
    },

    /// Install the init-system unit that keeps the session up, and load it. systemd on Linux,
    /// launchd on macOS, both in the user's own domain. Running it again over an unchanged
    /// install changes nothing and says so
    Enable {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// The binary path the unit should run. Defaults to the stable name on PATH that leads to
        /// this binary, which is what survives an upgrade
        #[clap(long, value_name = "PATH", value_parser)]
        exe: Option<PathBuf>,

        /// Install even when another job already runs `session up` for this session. Two
        /// launchers race at login and one of them ends up failed, so this is refused by default
        #[clap(long)]
        force: bool,
    },

    /// Unload the init-system unit and remove it. Removing the file without unloading the job
    /// first is the mistake this exists to prevent
    Disable {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,
    },

    /// Report the unit: whether the file is installed, whether the init system has loaded it, and
    /// whether the session is actually running. The three come apart, and which one is missing is
    /// the diagnosis
    Status {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// The binary path to compare the installed unit against, as `enable` takes it
        #[clap(long, value_name = "PATH", value_parser)]
        exe: Option<PathBuf>,
    },

    /// Check everything that has to hold for this session to come up and stay up, fix what a
    /// program is allowed to fix, and name what only a person can. Reports in three sections -
    /// what changed, what was already correct, and what needs you - and exits non-zero only when
    /// something is waiting on you
    Doctor {
        /// Name of the session, defaults to the `session_name` config option
        #[clap(value_parser)]
        session_name: Option<String>,

        /// Report every finding and change nothing. Each fix says what it would have done
        #[clap(short('n'), long)]
        dry_run: bool,

        /// Whether to repair what can be repaired. `--dry-run` implies `--no-fix`
        #[clap(long, overrides_with("no_fix"))]
        fix: bool,

        /// Only report; make no change
        #[clap(long)]
        no_fix: bool,

        /// Whether the signing ladder may sign the pinned copy, minting a certificate of our own
        /// if the machine has no Apple one. macOS only, and nothing on any other platform
        #[clap(long, overrides_with("no_sign"))]
        sign: bool,

        /// Leave the pinned copy's signature alone, whatever state it is in
        #[clap(long)]
        no_sign: bool,

        /// The binary path to compare the installed unit against, as `status` takes it
        #[clap(long, value_name = "PATH", value_parser)]
        exe: Option<PathBuf>,
    },
}

#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum SnapshotCli {
    /// List archived snapshots, newest first
    #[clap(visible_alias = "ls")]
    List {
        /// Only snapshots of this session name
        #[clap(long, value_parser)]
        session: Option<String>,

        /// Print the list as JSON
        #[clap(long)]
        json: bool,
    },
    /// Print a snapshot's layout
    Show {
        /// The snapshot id, or a unique prefix of one
        #[clap(value_parser)]
        id: String,
    },
    /// Rebuild a session from a snapshot
    Restore {
        /// The snapshot id, a unique prefix of one, or `latest`
        #[clap(value_parser)]
        id: String,

        /// Restore under this session name instead of the one the snapshot was taken from
        #[clap(long, value_parser)]
        session: Option<String>,
    },
    /// Delete a snapshot
    Rm {
        /// The snapshot id, or a unique prefix of one
        #[clap(value_parser)]
        id: String,
    },
    /// Adopt saved layouts left in the cache by other versions or contract versions
    Import {
        /// A session_info directory, or a single session folder, to import instead of the cache
        #[clap(long, value_parser)]
        from: Option<PathBuf>,

        /// Report what would be imported without writing anything
        #[clap(long)]
        dry_run: bool,

        /// Delete each source folder once it has been imported
        #[clap(long)]
        prune_source: bool,
    },
    /// Delete all but the newest snapshots of each session
    Prune {
        /// How many snapshots to keep per session name, defaults to session_snapshot_limit
        #[clap(long, value_parser)]
        keep: Option<usize>,
    },
}

/// The condition a `zellij action wait` blocks on.
#[derive(ValueEnum, Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WaitFor {
    /// The pane's command ends, or the pane goes away.
    Exit,
    /// The pane stops producing output for `--quiet-ms`.
    Quiet,
    /// A line the pane delivers matches `--match`.
    Match,
}

impl Default for WaitFor {
    fn default() -> Self {
        WaitFor::Exit
    }
}

#[derive(Debug, Subcommand, Clone, Serialize, Deserialize)]
pub enum CliAction {
    /// Block until a pane does something, then say what it did
    ///
    /// This is the one command that replaces a poll loop. Instead of `send-keys`, `sleep 2`,
    /// `dump-screen`, look, `sleep 2` again, a script says what it is waiting for and gets woken
    /// when it happens.
    ///
    ///   zellij action wait build --for exit
    ///
    ///   zellij action wait build --for quiet --quiet-ms 2000
    ///
    ///   zellij action wait build --for match --match 'test result:'
    ///
    /// Exit 0 means the condition was met, and the report says how long it took. Exit 2 means it
    /// was not - the timeout ran out, or the pane closed while waiting for something else - and
    /// nothing is printed on stdout. Exit 1 is a malformed call, such as a regex that does not
    /// compile.
    ///
    /// `--for exit` prints `exit_status:`, and the wait's OWN exit code stays 0 whatever that
    /// status is. This is deliberately unlike `new-pane --block-until-exit`, which exits with the
    /// command's status: that one owns the pane it made, while `wait` is a question about someone
    /// else's pane, and a script has to be able to tell "the test failed" from "I never saw it
    /// finish".
    ///
    /// What `--for match` sees is the rendered viewport, line by line, as the pane draws it - not
    /// the byte stream. A line the terminal wrapped arrives as two lines, so a pattern spanning
    /// the wrap never matches; anchor on a short distinctive string. Lines already on screen when
    /// the wait began are the baseline and do not match: only a line the pane delivers afterwards
    /// does. A line identical to one already on screen is not new, and is not matched either.
    Wait {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns
        #[clap(value_parser)]
        pane_id: String,

        /// What to wait for: the pane's command to end, the pane to fall quiet, or a line to
        /// match
        #[clap(long = "for", value_enum, value_parser, default_value = "exit")]
        wait_for: WaitFor,

        /// The regex a delivered line must match, for `--for match`. Rust regex syntax, unanchored
        #[clap(long = "match", value_parser, required_if_eq("wait_for", "match"))]
        pattern: Option<String>,

        /// How long a pane must produce nothing to count as quiet, for `--for quiet`
        #[clap(long, value_parser, default_value = "500")]
        quiet_ms: u64,

        /// Seconds to wait before giving up and exiting 2. `0` waits forever, which is a hang a
        /// script has to ask for by name
        #[clap(short, long, value_parser, default_value = "300")]
        timeout: u64,
    },
    /// Write raw bytes into a pane, as if they had been typed
    Write {
        /// The bytes, as space-separated decimal values (27 91 65 is Escape [ A)
        bytes: Vec<u8>,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Write text into a pane, as if it had been typed
    ///
    /// No newline is added: `send-keys Enter` is how you submit what you wrote.
    ///
    /// With no text and something piped in, the text is read from stdin to EOF - so a multi-line
    /// prompt goes in without shell escaping. Exits 2 if what arrives is empty.
    WriteChars {
        /// The text to write. Leave it out, or pass `-`, to read it from stdin instead (1 MiB max)
        chars: Option<String>,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Paste text into a pane in bracketed paste mode
    ///
    /// The pane's program is told the text was pasted rather than typed, which is what keeps an
    /// editor from auto-indenting it and a shell from running each line as it arrives.
    ///
    /// With no text and something piped in, the text is read from stdin to EOF - so a file goes in
    /// without shell escaping. Exits 2 if what arrives is empty.
    Paste {
        /// The text to paste. Leave it out, or pass `-`, to read it from stdin instead (1 MiB max)
        chars: Option<String>,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Send named keys to a pane
    ///
    /// Keys by name, not by character: `Enter`, `Esc`, `Tab`, `F1`, `Ctrl a`, `Alt Shift b`. Use
    /// `write-chars` for literal text.
    SendKeys {
        /// One key per argument, each a space-separated modifier chain ("Ctrl a", "F1")
        #[clap(value_parser, required = true)]
        keys: Vec<String>,

        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Grow or shrink a pane at one of its borders
    Resize {
        /// increase or decrease
        resize: Resize,
        /// The border to move: left, down, up or right. Without it, the pane grows or shrinks on
        /// every side that can move
        direction: Option<Direction>,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Move focus to the next pane in the tab, wrapping at the end
    FocusNextPane,
    /// Move focus to the previous pane in the tab, wrapping at the start
    FocusPreviousPane,
    /// Focus a pane, and the tab it lives in
    ///
    /// Returns: `from:` and `to:` lines, each `<pane_id> <handle>`. A jump that landed where it
    /// started prints only `to:`. A target no live pane answers to exits 2
    ///
    /// With --no-focus this is an existence probe instead, the same one `go-to-tab-name` has: the
    /// exit code is 0 either way, and stdout is the answer - `id:` and `handle:` if the pane is
    /// there, nothing at all if it is not
    #[clap(visible_alias = "go-to-pane")]
    FocusPaneId {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns
        pane_id: String,
        /// Leave focus where it is and only report what the target names. Read stdout for the
        /// answer: `id:` and `handle:` mean the pane is there, empty means it is not. The exit
        /// code stays 0 for both
        #[clap(long, value_parser)]
        no_focus: bool,
    },
    /// Move focus back to the pane it was on before the current one
    FocusLastPane,
    /// Move focus to the neighbouring pane in a direction, stopping at the edge of the tab
    MoveFocus {
        /// right, left, up or down
        direction: Direction,
    },
    /// Move focus to the neighbouring pane in a direction, or to the neighbouring tab at the edge
    MoveFocusOrTab {
        /// right, left, up or down
        direction: Direction,
    },
    /// Move a pane to a different place in its tab's layout
    MovePane {
        /// right, left, up or down. Without it, the pane rotates forwards through the layout
        direction: Option<Direction>,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Rotate a pane backwards through its tab's layout
    MovePaneBackwards {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Clear a pane's screen and its scrollback
    Clear {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Print what a pane is showing
    ///
    /// The pane is not optional: without --pane-id this prints the panes you could have asked for,
    /// grouped by tab, on stderr and exits 2. "The focused pane" is not a thing a command run from
    /// outside a pane can mean.
    ///
    /// Prints the pane content and nothing else - no header, no trailing summary.
    DumpScreen {
        /// Write the content to this file instead of stdout
        #[clap(value_parser, conflicts_with = "path")]
        file: Option<PathBuf>,

        /// The same file, spelled as a flag. Give it one way or the other, not both
        #[clap(long, value_parser)]
        path: Option<PathBuf>,

        /// Include the whole scrollback, not just the visible screen. A long-lived pane's
        /// scrollback can be megabytes
        #[clap(short, long)]
        full: bool,

        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Required: see above
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,

        /// Keep the ANSI escape sequences, so colour and styling survive the dump
        #[clap(short, long)]
        ansi: bool,
    },
    /// Print the session's current layout, as the KDL a `--layout` would take
    ///
    /// Prints the layout and nothing else.
    DumpLayout,
    /// Write the session's resurrection state to disk now, rather than waiting for the next tick
    SaveSession {
        /// Also copy the saved state into the snapshot archive, as a manual snapshot
        #[clap(long)]
        archive: bool,
    },
    /// Open a pane's scrollback in a new pane running your $EDITOR
    EditScrollback {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,

        /// Preserve ANSI styling in the scrollback dump
        #[clap(short, long)]
        ansi: bool,
    },
    /// Scroll a pane up one line
    ScrollUp {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane down one line
    ScrollDown {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane to the bottom of its scrollback, where new output arrives
    ScrollToBottom {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane to the top of its scrollback
    ScrollToTop {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane up one screenful
    PageScrollUp {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane down one screenful
    PageScrollDown {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane up half a screenful
    HalfPageScrollUp {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Scroll a pane down half a screenful
    HalfPageScrollDown {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Toggle a pane between filling its tab and the tab's normal layout
    ///
    /// Use `set-fullscreen on|off` to say which state you want rather than flipping whichever one
    /// the pane is in.
    ToggleFullscreen {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Toggle a pane between filling the whole terminal - status and tab bars included - and the
    /// tab's normal layout
    ToggleNoUiFullscreen {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Toggle every pane frame in the session between the configured style and none
    TogglePaneFrames,
    /// Set the frame style for the whole session, rather than toggling it
    ///
    /// Lasts for the life of the session; `pane_frame_style` in the config is the persistent form.
    SetPaneFrameStyle {
        /// full (a box around each pane), titles (a title row only), top_only (a title row with a
        /// rule and no separators between panes), or none
        #[clap(value_enum, value_parser)]
        style: PaneFrameStyle,
    },
    /// Toggle whether typing into one pane of a tab types into all of them
    ///
    /// Use `set-sync-tab on|off` to say which state you want rather than flipping whichever one
    /// the tab is in.
    ToggleActiveSyncTab {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Open a pane, running a command or your shell
    ///
    /// Returns: `pane_id: terminal_<id>` or `pane_id: plugin_<id>`, and `handle: <two-word
    /// handle>`. Without --direction the pane splits whichever side has the most room.
    ///
    /// Where it lands: beside the focused pane by default, beside the pane --near names, in the tab
    /// --in-tab or --tab-id names, or in a tab of its own with --new-tab, which reports `tab_id:`.
    NewPane {
        /// Split the pane it opens beside towards right or down. Without it, zellij splits
        /// whichever side has the most room
        #[clap(short, long, value_parser, conflicts_with("floating"))]
        direction: Option<Direction>,

        /// The command to run in the pane, after a `--`. Without one the pane runs your shell
        #[clap(last(true))]
        command: Vec<String>,

        /// Open a plugin pane instead of a terminal, by plugin url (file:/path/to.wasm, an alias
        /// from the config, or https://...)
        #[clap(short, long, conflicts_with("command"), conflicts_with("direction"))]
        plugin: Option<String>,

        /// Change the working directory of the new pane
        #[clap(long, value_parser)]
        cwd: Option<PathBuf>,

        /// Open the new pane in floating mode
        #[clap(short, long)]
        floating: bool,

        /// Open the new pane on top of an existing one, suspending that pane until this one closes
        #[clap(short, long, conflicts_with("floating"), conflicts_with("direction"))]
        in_place: bool,

        /// With --in-place, close the pane that was replaced instead of suspending it
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,

        /// With --in-place, the pane to open on top of: terminal_1, plugin_2, a bare integer,
        /// a handle like sunny-otter, or a pane uuid. Without this, the focused pane
        #[clap(
            long,
            value_parser,
            requires("in_place"),
            conflicts_with("near_current_pane")
        )]
        pane_id: Option<String>,

        /// Name of the new pane
        #[clap(short, long, value_parser)]
        name: Option<String>,

        /// The handle to give the pane, instead of the two-word one it would name itself: lowercase
        /// words joined by dashes, eg. build. A handle another live pane holds is an error
        #[clap(
            long,
            value_parser = chosen_handle,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit_failure")
        )]
        handle: Option<String>,

        /// Close the pane immediately when its command exits
        #[clap(short, long, requires("command"))]
        close_on_exit: bool,
        /// Start the command suspended, only running it once someone presses ENTER in the pane
        #[clap(short, long, requires("command"))]
        start_suspended: bool,
        /// With --plugin, the plugin's configuration as key=value pairs, comma separated
        #[clap(long, value_parser)]
        configuration: Option<PluginUserConfiguration>,
        /// With --plugin, recompile the plugin instead of loading it from the wasm cache
        #[clap(long, value_parser)]
        skip_plugin_cache: bool,
        /// The x coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        x: Option<String>,
        /// The y coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        y: Option<String>,
        /// The width if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        width: Option<String>,
        /// The height if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        height: Option<String>,
        /// Whether to pin a floating pane so that it is always on top
        #[clap(long, requires("floating"))]
        pinned: Option<bool>,
        /// Open the pane into a stack with the pane it opens beside. Fails if that pane cannot be
        /// stacked, rather than opening the pane somewhere else
        #[clap(long, conflicts_with("floating"), conflicts_with("direction"))]
        stacked: bool,
        /// Wait until the pane closes, then exit with the command's exit status. Waiting replaces
        /// the report: a blocking run prints no `pane_id:`, because the status is the answer
        #[clap(short, long)]
        blocking: bool,

        /// Wait until the command exits 0 or its pane closes, then exit with its status. Prints no
        /// `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_failure"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_success: bool,

        /// Wait until the command exits non-zero or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_failure: bool,

        /// Wait until the command exits either way or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            conflicts_with("blocking"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit_failure")
        )]
        block_until_exit: bool,

        #[clap(skip)]
        unblock_condition: Option<UnblockCondition>,

        /// Put the pane in a tab of its own, made now, and report `tab_id:` as well as the pane.
        /// With a NAME the tab is called that; bare, zellij names it as it names any new tab. The
        /// pane in it cannot be named with --name: use --handle, or rename it once it is there
        #[clap(
            long,
            value_name = "NAME",
            num_args(0..=1),
            conflicts_with("direction"),
            conflicts_with("stacked"),
            conflicts_with("in_place"),
            conflicts_with("floating"),
            conflicts_with("tab_id"),
            conflicts_with("near_current_pane"),
            conflicts_with("blocking"),
            conflicts_with("name")
        )]
        new_tab: Option<Option<String>>,

        /// Open the pane beside the pane this command was run from, rather than beside whichever
        /// pane the user is focused on
        #[clap(long)]
        near_current_pane: bool,
        /// Open the pane beside this one, in whatever tab it lives in: terminal_1, a bare integer
        /// (3 means terminal_3), a handle like sunny-otter, or a pane uuid. It must name a terminal
        /// pane - a plugin pane cannot anchor one
        #[clap(
            long,
            value_name = "PANE",
            value_parser,
            conflicts_with("near_current_pane"),
            conflicts_with("in_place"),
            conflicts_with("tab_id"),
            conflicts_with("in_tab"),
            conflicts_with("new_tab")
        )]
        near: Option<String>,
        /// Open the pane beside the pane this command was run from and leave every client's focus
        /// where it is
        #[clap(long)]
        no_focus: bool,
        /// Draw the pane without a frame (warning: a borderless pane cannot be moved with the
        /// mouse)
        #[clap(long, value_parser)]
        borderless: Option<bool>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(
            long,
            value_parser,
            conflicts_with("near_current_pane"),
            conflicts_with("in_place")
        )]
        tab_id: Option<usize>,
        /// A tab that already exists, by name or by stable id, without going there: nothing moves
        /// the focus. A tab nothing answers to is a miss and nothing is created
        #[clap(
            long,
            value_name = "NAME_OR_ID",
            value_parser,
            conflicts_with("tab_id"),
            conflicts_with("new_tab"),
            conflicts_with("near_current_pane"),
            conflicts_with("in_place")
        )]
        in_tab: Option<String>,
    },
    /// Open a file in a new pane running your $EDITOR
    ///
    /// Returns: `pane_id: terminal_<id>` and `handle: <two-word handle>`
    Edit {
        /// The file to open. A relative path is resolved against --cwd, or the current directory
        file: PathBuf,

        /// Split the pane it opens beside towards right or down. Without it, zellij splits
        /// whichever side has the most room
        #[clap(short, long, value_parser, conflicts_with("floating"))]
        direction: Option<Direction>,

        /// Put the cursor on this line when the editor opens
        #[clap(short, long, value_parser)]
        line_number: Option<usize>,

        /// Open the new pane in floating mode
        #[clap(short, long)]
        floating: bool,

        /// Open the new pane in place of the current pane, temporarily suspending it
        #[clap(short, long, conflicts_with("floating"), conflicts_with("direction"))]
        in_place: bool,

        /// Close the replaced pane instead of suspending it (only effective with --in-place)
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,

        /// Change the working directory of the editor
        #[clap(long, value_parser)]
        cwd: Option<PathBuf>,
        /// The x coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        x: Option<String>,
        /// The y coordinates if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(short, long, requires("floating"))]
        y: Option<String>,
        /// The width if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        width: Option<String>,
        /// The height if the pane is floating as a bare integer (eg. 1) or percent (eg. 10%)
        #[clap(long, requires("floating"))]
        height: Option<String>,
        /// Whether to pin a floating pane so that it is always on top
        #[clap(long, requires("floating"))]
        pinned: Option<bool>,
        /// Open the pane beside the pane this command was run from, rather than beside whichever
        /// pane the user is focused on
        #[clap(long)]
        near_current_pane: bool,
        /// Open the pane beside the pane this command was run from and leave every client's focus
        /// where it is
        #[clap(long)]
        no_focus: bool,
        /// Draw the pane without a frame (warning: a borderless pane cannot be moved with the
        /// mouse)
        #[clap(short, long, value_parser)]
        borderless: Option<bool>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(
            long,
            value_parser,
            conflicts_with("near_current_pane"),
            conflicts_with("in_place")
        )]
        tab_id: Option<usize>,
    },
    /// Put every client of this session into an input mode
    SwitchMode {
        /// locked, normal, pane, tab, resize, move, search, session, scroll, prompt, tmux or enter
        input_mode: InputMode,
    },
    /// Toggle a pane between floating and embedded in its tab's layout
    ///
    /// Use `set-pane-floating on|off` to say which state you want rather than flipping whichever
    /// one the pane is in.
    TogglePaneEmbedOrFloating {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Toggle whether a tab's floating panes are shown, opening one if the tab has none
    ///
    /// Use `show-floating-panes` or `hide-floating-panes` to say which state you want rather than
    /// flipping whichever one the tab is in.
    ToggleFloatingPanes {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Show a tab's floating panes, rather than toggling them
    ///
    /// Exits 0 if they were hidden and are now shown, 2 if they were already shown, 1 if there is
    /// no such tab.
    ShowFloatingPanes {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Hide a tab's floating panes, rather than toggling them
    ///
    /// Exits 0 if they were shown and are now hidden, 2 if they were already hidden, 1 if there is
    /// no such tab.
    HideFloatingPanes {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Ask whether a tab's floating panes are shown
    ///
    /// Prints `true` and exits 0 if they are, `false` and exits 1 if they are not. The 1 is an
    /// answer here, not an error - this command predates the fork's exit-code convention.
    AreFloatingPanesVisible {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Close a pane, killing whatever runs in it
    ///
    /// Prints `closed: <pane id>`. A pane that is not there is a miss: a message on stderr and
    /// exit 2. Without --pane-id this closes the focused pane, which is only meaningful from
    /// inside a pane.
    ClosePane {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Give a pane a name, which is what its frame and the TITLE column then show
    RenamePane {
        /// The new name
        name: String,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Drop a name set by `rename-pane`, so the pane goes back to naming itself after its command
    UndoRenamePane {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Move focus to the next tab by display position, wrapping at the end
    GoToNextTab,
    /// Move focus to the previous tab by display position, wrapping at the start
    GoToPreviousTab,
    /// Close a tab and every pane in it
    ///
    /// Prints `closed: <tab id> <tab name>`. A tab that is not there is a miss: a message on
    /// stderr and exit 2. Without --tab-id this closes the current tab, which is only meaningful
    /// from inside a pane.
    CloseTab {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Focus the tab at a display position
    ///
    /// Prints `from:` and `to:` lines, each `<tab id> <tab name>`, for the tab focus left and the
    /// tab it landed on. A switch that did not move prints only `to:`. A position no tab sits at
    /// is a miss: a message on stderr and exit 2.
    GoToTab {
        /// The 1-based display position: 1 is the leftmost tab. This is the POSITION column of
        /// `list-tabs` plus one, not the TAB_ID column
        index: u32,
    },
    /// Focus the tab with a given name
    ///
    /// Prints `from:` and `to:` lines, each `<tab id> <tab name>`, for a real switch. When
    /// --create makes the tab it prints `id: <tab id>` instead, then `pane_id:` and `handle:` for
    /// the pane the new tab opened on. A name no tab answers to is a miss: a message on stderr and
    /// exit 2.
    ///
    /// With --no-focus and without --create this is an existence probe instead: the exit code is 0
    /// either way, and stdout is the answer - `id: <tab id>` if the tab is there, nothing at all
    /// if it is not.
    GoToTabName {
        /// The tab name, matched exactly - the NAME column of `list-tabs`
        name: String,
        /// Create the tab if no tab answers to that name
        #[clap(short, long, value_parser)]
        create: bool,
        /// Leave focus where it is, whether the tab already existed or was just created.
        /// Without --create, read stdout for the answer: a tab ID means it exists, empty means it
        /// does not. The exit code stays 0 for both.
        #[clap(long, value_parser)]
        no_focus: bool,
    },
    /// Give a tab a name, which is what the tab bar and the NAME column then show
    RenameTab {
        /// The new name
        name: String,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Drop a name set by `rename-tab`, so the tab goes back to its numbered default
    UndoRenameTab {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Focus a tab by its stable id
    ///
    /// Prints `from:` and `to:` lines, each `<tab id> <tab name>`. An id no tab answers to is a
    /// miss: a message on stderr and exit 2.
    GoToTabById {
        /// The stable id, from the TAB_ID column of `list-tabs`
        id: u64,
    },
    /// Close a tab by its stable id
    ///
    /// Prints `closed: <tab id> <tab name>`. An id no tab answers to is a miss: a message on
    /// stderr and exit 2.
    CloseTabById {
        /// The stable id, from the TAB_ID column of `list-tabs`
        id: u64,
    },
    /// Rename a tab by its stable id
    RenameTabById {
        /// The stable id, from the TAB_ID column of `list-tabs`
        id: u64,
        /// The new name
        name: String,
    },
    /// Create a tab, optionally from a layout
    ///
    /// Returns: `tab_id: <id>`, then `pane_id:` and `handle:` for the pane the tab opened on. A
    /// tab made from a layout gets whatever panes the layout names; otherwise it opens one pane
    /// running your shell or --initial-command.
    NewTab {
        /// A layout to build the tab from: a name in the layout directory, or a path to a file
        #[clap(short, long, value_parser, conflicts_with = "layout_string")]
        layout: Option<PathBuf>,

        /// A KDL layout as a string, instead of a file to read it from
        #[clap(long, value_parser, conflicts_with = "layout")]
        layout_string: Option<String>,

        /// Where to look for the --layout name, overriding the configured layout directory
        #[clap(long, value_parser, requires("layout"))]
        layout_dir: Option<PathBuf>,

        /// Name of the new tab. Without one the tab is numbered
        #[clap(short, long, value_parser)]
        name: Option<String>,

        /// The working directory the tab's panes start in
        #[clap(short, long, value_parser)]
        cwd: Option<PathBuf>,

        /// The command to run in the tab's first pane, after a `--`. Without one it runs your shell
        #[clap(value_parser, conflicts_with("initial_plugin"), last(true))]
        initial_command: Vec<String>,

        /// Load a plugin in the tab's first pane instead of a terminal, by plugin url
        #[clap(long, value_parser, conflicts_with("initial_command"))]
        initial_plugin: Option<String>,

        /// Close the pane immediately when its command exits
        #[clap(long, requires("initial_command"))]
        close_on_exit: bool,

        /// Start the command suspended, only running it after you first press ENTER
        #[clap(long, requires("initial_command"))]
        start_suspended: bool,

        /// Wait until the command exits 0 or its pane closes, then exit with its status. Prints no
        /// `pane_id:`, as above
        #[clap(
            long,
            requires("initial_command"),
            conflicts_with("block_until_exit_failure"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_success: bool,

        /// Wait until the command exits non-zero or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            requires("initial_command"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit")
        )]
        block_until_exit_failure: bool,

        /// Wait until the command exits either way or its pane closes, then exit with its status.
        /// Prints no `pane_id:`, as above
        #[clap(
            long,
            requires("initial_command"),
            conflicts_with("block_until_exit_success"),
            conflicts_with("block_until_exit_failure")
        )]
        block_until_exit: bool,

        /// Create the tab and leave every client's focus where it is
        #[clap(long)]
        no_focus: bool,
    },
    /// Move a tab in the specified direction. [right|left]
    ///
    /// Prints `from:` and `to:` lines, each a 0-based display position - the same numbers
    /// list-tabs prints in its POSITION column. A tab that is not there is a miss: a message on
    /// stderr and exit 2. Without --tab-id this moves the current tab, which is only meaningful
    /// from inside a pane.
    MoveTab {
        /// right or left: swap the tab with its neighbour on that side
        #[clap(
            value_parser,
            required_unless_present = "to_index",
            conflicts_with = "to_index"
        )]
        direction: Option<Direction>,
        /// Move the tab to this 0-based display position, shifting the tabs in between rather
        /// than swapping. Past the last position it lands at the end
        #[clap(long, value_parser)]
        to_index: Option<usize>,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Apply the previous swap layout to a tab
    ///
    /// Swap layouts are the alternative arrangements a layout file declares; this walks backwards
    /// through them.
    PreviousSwapLayout {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Apply the next swap layout to a tab
    ///
    /// Swap layouts are the alternative arrangements a layout file declares; this walks forwards
    /// through them.
    NextSwapLayout {
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Rearrange the panes of a tab into a layout
    ///
    /// The tab's existing panes are placed into the layout's slots. Panes the layout has no room
    /// for are closed, unless --retain-existing-terminal-panes or its plugin twin is passed.
    OverrideLayout {
        /// A path to a layout file
        #[clap(
            value_parser,
            required_unless_present = "layout_string",
            conflicts_with = "layout_string"
        )]
        layout: Option<PathBuf>,

        /// A KDL layout as a string, instead of a file to read it from
        #[clap(long, value_parser, conflicts_with = "layout")]
        layout_string: Option<String>,

        /// Where to look for the layout by name, overriding the configured layout directory
        #[clap(long, value_parser)]
        layout_dir: Option<PathBuf>,

        /// Keep terminal panes the layout has no room for, instead of closing them
        #[clap(long)]
        retain_existing_terminal_panes: bool,

        /// Keep plugin panes the layout has no room for, instead of closing them
        #[clap(long)]
        retain_existing_plugin_panes: bool,

        /// Apply only the layout's first tab, to the focused tab, instead of every tab it declares
        #[clap(long)]
        apply_only_to_active_tab: bool,
    },
    /// Reload a running plugin's wasm, or start it if it is not running
    ///
    /// The reload is how a plugin under development picks up a rebuild without its pane moving.
    StartOrReloadPlugin {
        /// The plugin url: file:/path/to.wasm, an alias from the config, or https://...
        url: String,
        /// The plugin's configuration as key=value pairs, comma separated
        #[clap(short, long, value_parser)]
        configuration: Option<PluginUserConfiguration>,
    },
    /// Focus a plugin's pane if it is already running, otherwise open it
    ///
    /// Returns: `pane_id: plugin_<id>` and `handle: <two-word handle>`, for the pane it made or
    /// the one it focused
    LaunchOrFocusPlugin {
        /// Open it as a floating pane
        #[clap(short, long, value_parser)]
        floating: bool,
        /// Open it on top of the focused pane, suspending that pane until this one closes
        #[clap(short, long, value_parser)]
        in_place: bool,
        /// With --in-place, close the pane that was replaced instead of suspending it
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,
        /// If the plugin is already running in another tab, move its pane here rather than
        /// focusing the tab it is in
        #[clap(short, long, value_parser)]
        move_to_focused_tab: bool,
        /// The plugin url: file:/path/to.wasm, an alias from the config, or https://...
        url: String,
        /// The plugin's configuration as key=value pairs, comma separated. A plugin running under
        /// a different configuration counts as a different plugin
        #[clap(short, long, value_parser)]
        configuration: Option<PluginUserConfiguration>,
        /// Recompile the plugin instead of loading it from the wasm cache
        #[clap(short, long, value_parser)]
        skip_plugin_cache: bool,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(long, value_parser, conflicts_with("in_place"))]
        tab_id: Option<usize>,
    },
    /// Open a plugin in a new pane, even if it is already running elsewhere
    ///
    /// Returns: `pane_id: plugin_<id>` and `handle: <two-word handle>`
    LaunchPlugin {
        /// Open it as a floating pane
        #[clap(short, long, value_parser)]
        floating: bool,
        /// Open it on top of the focused pane, suspending that pane until this one closes
        #[clap(short, long, value_parser)]
        in_place: bool,
        /// With --in-place, close the pane that was replaced instead of suspending it
        #[clap(long, requires("in_place"))]
        close_replaced_pane: bool,
        /// The plugin url: file:/path/to.wasm or https://...
        url: Url,
        /// The plugin's configuration as key=value pairs, comma separated
        #[clap(short, long, value_parser)]
        configuration: Option<PluginUserConfiguration>,
        /// Recompile the plugin instead of loading it from the wasm cache
        #[clap(short, long, value_parser)]
        skip_plugin_cache: bool,
        /// Open the pane and leave every client's focus where it is
        #[clap(long)]
        no_focus: bool,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(long, value_parser, conflicts_with("in_place"))]
        tab_id: Option<usize>,
    },
    /// Rename this session
    ///
    /// The name is what `zellij ls` lists and `zellij attach` takes. Panes that predate the rename
    /// keep the old name in their $ZELLIJ_SESSION_NAME until they are replaced.
    RenameSession {
        /// The new session name
        name: String,
    },
    /// Send data to one or more plugins, launch them if they are not running.
    #[clap(override_usage(
r#"
zellij action pipe [OPTIONS] [--] <PAYLOAD>

* Send data to a specific plugin:

zellij action pipe --plugin file:/path/to/my/plugin.wasm --name my_pipe_name -- my_arbitrary_data

* To all running plugins (that are listening):

zellij action pipe --name my_pipe_name -- my_arbitrary_data

* Pipe data into this command's STDIN and get output from the plugin on this command's STDOUT

tail -f /tmp/my-live-logfile | zellij action pipe --name logs --plugin https://example.com/my-plugin.wasm | wc -l
"#))]
    Pipe {
        /// The name of the pipe
        #[clap(short, long, value_parser, display_order(1))]
        name: Option<String>,
        /// The data to send down this pipe (if blank, will listen to STDIN)
        payload: Option<String>,

        #[clap(short, long, value_parser, display_order(2))]
        /// The args of the pipe
        args: Option<PluginUserConfiguration>, // TODO: we might want to not re-use
        // PluginUserConfiguration
        /// The plugin url (eg. file:/tmp/my-plugin.wasm) to direct this pipe to, if not specified,
        /// will be sent to all plugins, if specified and is not running, the plugin will be launched
        #[clap(short, long, value_parser, display_order(3))]
        plugin: Option<String>,
        /// The plugin configuration (note: the same plugin with different configuration is
        /// considered a different plugin for the purposes of determining the pipe destination)
        #[clap(short('c'), long, value_parser, display_order(4))]
        plugin_configuration: Option<PluginUserConfiguration>,
        /// Launch a new plugin even if one is already running
        #[clap(short('l'), long, display_order(5))]
        force_launch_plugin: bool,
        /// If launching a new plugin, skip cache and force-compile the plugin
        #[clap(short('s'), long, display_order(6))]
        skip_plugin_cache: bool,
        /// If launching a plugin, should it be floating or not, defaults to floating
        #[clap(short('f'), long, value_parser, display_order(7))]
        floating_plugin: Option<bool>,
        /// If launching a plugin, launch it in-place (on top of the current pane)
        #[clap(
            short('i'),
            long,
            value_parser,
            conflicts_with("floating_plugin"),
            display_order(8)
        )]
        in_place_plugin: Option<bool>,
        /// If launching a plugin, specify its working directory
        #[clap(short('w'), long, value_parser, display_order(9))]
        plugin_cwd: Option<PathBuf>,
        /// If launching a plugin, specify its pane title
        #[clap(short('t'), long, value_parser, display_order(10))]
        plugin_title: Option<String>,
    },
    /// List the clients attached to this session
    ///
    /// Returns: one row per client - id, focused pane, running command, tty, terminal size, and
    /// whether the row is the client asking - or the ClientInfo array with --json
    ListClients {
        /// Output as JSON
        #[clap(short, long, value_parser)]
        json: bool,
    },
    /// List all panes in the current session
    ///
    /// Returns: one row per pane, every column - tab, handle, command, state, geometry - or the
    /// same information as JSON with --json
    ListPanes {
        /// Accepted and ignored: tab columns are always printed
        #[clap(short, long, value_parser)]
        tab: bool,

        /// Accepted and ignored: command columns are always printed
        #[clap(short, long, value_parser)]
        command: bool,

        /// Accepted and ignored: state columns are always printed
        #[clap(short, long, value_parser)]
        state: bool,

        /// Accepted and ignored: geometry columns are always printed
        #[clap(short, long, value_parser)]
        geometry: bool,

        /// Also list the panes you cannot select - plugin panes the UI hides, suppressed panes
        #[clap(short, long, value_parser)]
        all: bool,

        /// Output as JSON
        #[clap(short, long, value_parser)]
        json: bool,
    },
    /// List all tabs with their information
    ///
    /// Returns: one row per tab, every column - state, dimensions, pane counts, layout - or the
    /// same information as JSON with --json
    ListTabs {
        /// Accepted and ignored: state columns are always printed
        #[clap(short, long, value_parser)]
        state: bool,

        /// Accepted and ignored: dimension columns are always printed
        #[clap(short, long, value_parser)]
        dimensions: bool,

        /// Accepted and ignored: pane-count columns are always printed
        #[clap(short, long, value_parser)]
        panes: bool,

        /// Accepted and ignored: layout columns are always printed
        #[clap(short, long, value_parser)]
        layout: bool,

        /// Accepted and ignored: every column is always printed
        #[clap(short, long, value_parser)]
        all: bool,

        /// Output as JSON
        #[clap(short, long, value_parser)]
        json: bool,
    },
    /// List every tab with its panes nested beneath it
    ///
    /// Returns: one line per tab, then its panes indented below it, each line `key: value` pairs
    /// two spaces apart - or the same tree structured with --json
    ListTree {
        /// Output as JSON
        #[clap(short, long, value_parser)]
        json: bool,
    },
    /// Get information about the currently active tab
    ///
    /// Returns: `name:`, `id:` and `position:` lines, or the full TabInfo with --json
    CurrentTabInfo {
        /// Output as JSON with full TabInfo
        #[clap(short, long, value_parser)]
        json: bool,
    },
    /// Toggle whether a floating pane stays on top of the other floating panes
    ///
    /// Use `set-pane-pinned on|off` to say which state you want rather than flipping whichever one
    /// the pane is in.
    TogglePanePinned {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Gather panes into one stack, in the order given
    ///
    /// Example: zellij action stack-panes -- terminal_1 plugin_2 3
    StackPanes {
        /// The panes, after a `--`: terminal_1, plugin_2, a bare integer (3 means terminal_3), a
        /// handle like sunny-otter, or a pane uuid
        #[clap(last(true), required(true))]
        pane_ids: Vec<String>,
    },
    /// Move, resize or re-dress a floating pane
    ///
    /// Every field is optional and the ones you leave out are left alone.
    ChangeFloatingPaneCoordinates {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. It must be floating already - `set-pane-floating on`
        /// makes it so
        #[clap(short, long, value_parser)]
        pane_id: String,
        /// Distance from the left edge, in columns (eg. 1) or percent of the screen (eg. 10%)
        #[clap(short, long)]
        x: Option<String>,
        /// Distance from the top edge, in rows (eg. 1) or percent of the screen (eg. 10%)
        #[clap(short, long)]
        y: Option<String>,
        /// Width in columns (eg. 80) or percent of the screen (eg. 50%)
        #[clap(long)]
        width: Option<String>,
        /// Height in rows (eg. 24) or percent of the screen (eg. 50%)
        #[clap(long)]
        height: Option<String>,
        /// Whether the pane stays on top of the other floating panes
        #[clap(long)]
        pinned: Option<bool>,
        /// Whether the pane is drawn with a frame (warning: a borderless pane cannot be moved with
        /// the mouse)
        #[clap(short, long, value_parser)]
        borderless: Option<bool>,
    },
    /// Set whether a pane is fullscreen, rather than toggling it
    ///
    /// Exits 0 if the state changed and 2 if it did not - both when the pane was already so and
    /// when the pane does not exist. Only the failure prints a reason on stderr, so read stderr to
    /// tell the two apart.
    SetFullscreen {
        /// on|off (also accepts true/false, yes/no, 1/0)
        #[clap(action = clap::ArgAction::Set, value_parser = clap::builder::BoolishValueParser::new())]
        enabled: bool,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Set whether a floating pane is pinned on top, rather than toggling it
    ///
    /// Exits 0 if the state changed and 2 if it did not - both when the pane was already so and
    /// when the pane does not exist. Only the failure prints a reason on stderr, so read stderr to
    /// tell the two apart.
    SetPanePinned {
        /// on|off (also accepts true/false, yes/no, 1/0)
        #[clap(action = clap::ArgAction::Set, value_parser = clap::builder::BoolishValueParser::new())]
        enabled: bool,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Set whether a pane floats or is embedded, rather than toggling it
    ///
    /// Exits 0 if the state changed and 2 if it did not - when the pane was already so, when the
    /// pane does not exist, and when the layout refuses the move (the last tiled pane cannot
    /// float). Only the failures print a reason on stderr, so read stderr to tell them apart.
    SetPaneFloating {
        /// on|off (also accepts true/false, yes/no, 1/0)
        #[clap(action = clap::ArgAction::Set, value_parser = clap::builder::BoolishValueParser::new())]
        enabled: bool,
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
    },
    /// Set whether input is synchronised across a tab's panes, rather than toggling it
    ///
    /// Exits 0 if the state changed and 2 if it did not - both when the tab was already so and
    /// when the tab does not exist. Only the failure prints a reason on stderr, so read stderr to
    /// tell the two apart.
    SetSyncTab {
        /// on|off (also accepts true/false, yes/no, 1/0)
        #[clap(action = clap::ArgAction::Set, value_parser = clap::builder::BoolishValueParser::new())]
        enabled: bool,
        /// The tab, by the stable id in the TAB_ID column of `zellij action list-tabs` - not
        /// the 1-based display position `go-to-tab` takes. Without this, the focused tab
        #[clap(short, long, value_parser)]
        tab_id: Option<usize>,
    },
    /// Move panes out into a tab of their own
    ///
    /// Returns: `tab_id: <id>` for the new tab.
    BreakPane {
        /// A pane to move, repeat the flag for more than one: terminal_1, plugin_2, a bare
        /// integer, a handle like sunny-otter, or a pane uuid. Without this, the focused pane
        #[clap(short, long, value_parser)]
        pane_id: Vec<String>,
        /// Name for the new tab. Without one the tab is numbered
        #[clap(short, long, value_parser)]
        name: Option<String>,
        /// Leave every client's focus where it is, instead of following the panes
        #[clap(long, value_parser)]
        no_focus: bool,
    },
    /// Move panes into an existing tab
    ///
    /// A tab or pane that does not exist prints the reason on stderr and exits non-zero.
    BreakPaneToTab {
        /// A pane to move, repeat the flag for more than one: terminal_1, plugin_2, a bare
        /// integer, a handle like sunny-otter, or a pane uuid
        #[clap(short, long, value_parser, required(true))]
        pane_id: Vec<String>,
        /// The tab to move them into, by the stable id in the TAB_ID column of `list-tabs`
        #[clap(short, long, value_parser)]
        tab_id: u32,
        /// Leave every client's focus where it is, instead of following the panes
        #[clap(long, value_parser)]
        no_focus: bool,
    },
    /// Move the focused pane into a new tab to the right of its own
    ///
    /// Names no pane, so it means nothing from outside the session and is refused there. Use
    /// `break-pane --pane-id` from a script.
    BreakPaneRight,
    /// Move the focused pane into a new tab to the left of its own
    ///
    /// Names no pane, so it means nothing from outside the session and is refused there. Use
    /// `break-pane --pane-id` from a script.
    BreakPaneLeft,
    /// Send a signal to the process running in a pane
    ///
    /// A pane that does not exist, or a plugin pane, which runs no process, prints the reason on
    /// stderr and exits non-zero.
    SignalPane {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns
        #[clap(short, long, value_parser)]
        pane_id: String,
        /// int (SIGINT), hup (SIGHUP) or kill (SIGKILL). It goes to the process the pane
        /// started - the shell itself, where the pane runs one, not the command under it
        #[clap(short, long, value_enum, value_parser, default_value = "int")]
        signal: PaneSignal,
    },
    /// Toggle whether a pane is drawn with a frame
    ///
    /// Use `set-pane-borderless` to say which state you want rather than flipping whichever one
    /// the pane is in.
    TogglePaneBorderless {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns
        #[clap(short, long, value_parser)]
        pane_id: String,
    },
    /// Set whether a pane is drawn with a frame, rather than toggling it
    SetPaneBorderless {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. `zellij action list-panes` prints every one of them
        /// in its PANE_ID and HANDLE columns
        #[clap(short, long, value_parser)]
        pane_id: String,
        /// Whether the pane should be borderless (flag present) or bordered (flag absent)
        #[clap(short, long, value_parser)]
        borderless: bool,
    },
    /// Detach every client from this session, leaving the session running
    Detach,
    /// Switch every client to the configured `theme_dark`
    SetDarkTheme,
    /// Switch every client to the configured `theme_light`
    SetLightTheme,
    /// Switch every client between the configured `theme_dark` and `theme_light`
    ToggleTheme,
    /// Move the clients of this session to another session, creating it if it is not running
    SwitchSession {
        /// The session to switch to, as `zellij ls` names it
        name: String,
        /// A 1-based tab position to focus in the session being switched to
        #[clap(long)]
        tab_position: Option<usize>,
        /// A pane to focus in the session being switched to: terminal_1, plugin_2, or a bare
        /// integer. A handle or a uuid is refused here - it names a pane against this session, not
        /// that one, and there is no way to ask that one
        #[clap(long)]
        pane_id: Option<String>,
        /// A layout to build the session from, if it is not already running: a name in the layout
        /// directory, or a path to a file
        #[clap(short, long, value_parser, conflicts_with = "layout_string")]
        layout: Option<PathBuf>,
        /// A KDL layout as a string, instead of a file to read it from
        #[clap(long, value_parser, conflicts_with = "layout")]
        layout_string: Option<String>,
        /// Where to look for the --layout name, overriding the configured layout directory
        #[clap(long, value_parser, requires("layout"))]
        layout_dir: Option<PathBuf>,
        /// The working directory the session's panes start in, if it is not already running
        #[clap(short, long, value_parser)]
        cwd: Option<PathBuf>,
    },
    /// Set a pane's default foreground and background colour
    ///
    /// The colours a program in the pane sets for itself still win; this is the ground it draws on.
    SetPaneColor {
        /// The pane: terminal_1, plugin_2, a bare integer (3 means terminal_3), a handle like
        /// sunny-otter, or a pane uuid. Without this, the pane named by $ZELLIJ_PANE_ID - which
        /// is the pane the command runs in, and unset outside a pane
        #[clap(short, long, value_parser)]
        pane_id: Option<String>,
        /// Foreground colour (e.g. "#00e000", "rgb:00/e0/00")
        #[clap(long, value_parser)]
        fg: Option<String>,
        /// Background colour (e.g. "#001a3a", "rgb:00/1a/3a")
        #[clap(long, value_parser)]
        bg: Option<String>,
        /// Drop both colours, so the pane draws on the terminal's own defaults
        #[clap(long, value_parser, conflicts_with_all(&["fg", "bg"]))]
        reset: bool,
    },
}

/// Run `f` on a thread with a stack big enough to build the clap tree.
///
/// `CliAction` has hundreds of variants and clap builds the whole subcommand tree recursively,
/// which overflows the 2MB stack the test harness gives a spawned test in a debug build. Every test
/// that builds or parses `CliArgs` must go through here, in this crate and in the `zellij` crate
/// (`src/tests/cli.rs`) alike: the next `CliAction` subcommand will silently re-break any module
/// that does not. The binary itself parses on the main thread and never sees this.
pub fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap()
}

/// A `--handle` value, refused at the parser when it is not a handle anyone could choose.
///
/// The reasons live in `pane_handle` with the grammar they come from; this is the clap end of it,
/// so a bad name is caught before a pane is made rather than after.
fn chosen_handle(value: &str) -> Result<String, String> {
    match crate::pane_handle::chosen_handle_error(value) {
        Some(reason) => Err(reason),
        None => Ok(value.to_owned()),
    }
}

impl CliAction {
    /// The handle `--handle` chose, taken out of the request for the client to apply.
    ///
    /// A pane names itself when it is born and the CLI has no say in that moment, so a chosen
    /// handle is given to the pane just after, by the client that is holding the report and knows
    /// which pane was made. Taking it here is what keeps a handle from also travelling as part of
    /// the creation action, where nothing would look at it.
    pub fn take_chosen_handle(&mut self) -> Option<String> {
        match self {
            CliAction::NewPane { handle, .. } => handle.take(),
            _ => None,
        }
    }

    /// The tab `--in-tab` named, for the caller that can ask the session which tab that is.
    ///
    /// The flag takes a name or a stable id, and neither can be turned into the `--tab-id` the
    /// action carries without the session's own list of tabs. So the CLI asks first and rewrites
    /// the request with [`CliAction::place_in_tab`]; nothing downstream knows the flag existed.
    pub fn in_tab_target(&self) -> Option<&str> {
        match self {
            CliAction::NewPane { in_tab, .. } => in_tab.as_deref(),
            _ => None,
        }
    }

    /// The pane `--near` named, for the caller that can ask the session which pane that is.
    ///
    /// Same shape as [`CliAction::in_tab_target`], for the same reason: a handle names a pane only
    /// against the session's live panes, and the anchor has to be a pane id by the time the action
    /// goes out.
    pub fn near_target(&self) -> Option<&str> {
        match self {
            CliAction::NewPane { near, .. } => near.as_deref(),
            _ => None,
        }
    }

    /// Anchors the new pane to the pane `--near` turned out to name.
    ///
    /// The anchor travels as the pane the command came from - the same channel
    /// `--near-current-pane` reads out of `$ZELLIJ_PANE_ID` - so what is left here is to say that
    /// there is one. The id itself is carried by the client, which is what talks to the server.
    pub fn anchor_near(&mut self) {
        if let CliAction::NewPane {
            near,
            near_current_pane,
            ..
        } = self
        {
            *near = None;
            *near_current_pane = true;
        }
    }

    /// Puts the pane in the tab `--in-tab` turned out to name, and leaves every focus alone.
    ///
    /// `no_focus` is the flag's whole point rather than a default it happens to take: a script that
    /// puts a pane in another tab has not asked to be taken there, and a `zellij action` client
    /// that "focuses" something is moving a focus that belongs to whoever is attached. `--tab-id`
    /// is the spelling for a caller that does want the view to follow.
    pub fn place_in_tab(&mut self, tab: usize) {
        if let CliAction::NewPane {
            in_tab,
            tab_id,
            no_focus,
            ..
        } = self
        {
            *in_tab = None;
            *tab_id = Some(tab);
            *no_focus = true;
        }
    }
}

/// The most text `write-chars` and `paste` will take from stdin, in bytes.
///
/// A megabyte is far more than anyone types and far less than a file that was piped in by mistake.
/// The text is delivered to the pane as keystrokes, so the pane's program reads all of it: a bound
/// here is what keeps `zellij action paste < some.iso` from being a way to wedge a shell.
pub const MAX_STDIN_TEXT_BYTES: usize = 1024 * 1024;

/// The text a `write-chars` or a `paste` was given on stdin, bounded and checked.
///
/// `Ok("")` is not an error here - an empty stdin is a well-formed request that writes nothing, and
/// the caller is what turns it into the miss it is. Both errors are the caller's exit 1: text that
/// is not text, and text past the bound.
pub fn text_from_stdin<R: std::io::Read>(reader: R, verb: &str) -> Result<String, String> {
    use std::io::Read;
    let mut bytes = Vec::new();
    // one byte past the bound, so a file exactly at it still passes and anything longer is caught
    // without reading the rest of it
    std::io::Read::take(reader, MAX_STDIN_TEXT_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("`{}` could not read stdin: {}", verb, e))?;
    if bytes.len() > MAX_STDIN_TEXT_BYTES {
        return Err(format!(
            "`{}` reads at most {} bytes from stdin, and this is more than that. \
             Send it in pieces, or write the file into the pane's program another way.",
            verb, MAX_STDIN_TEXT_BYTES
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        format!(
            "`{}` writes text, and what arrived on stdin is not valid UTF-8. \
             `zellij action write` takes raw bytes.",
            verb
        )
    })
}

/// Whether a command that acts on "the focused thing" has been given something to act on.
///
/// A `zellij action` client is not attached to anything. It has no focus of its own, so the server
/// resolves "focused" against whichever client it can find - which, from a script, is a pane the
/// caller has never seen. For a mutation that is not a wrong answer, it is a wrong tab getting
/// closed or moved.
///
/// Run from inside a pane, `focused` means the pane the hands are on, and that is exactly what the
/// caller meant, so nothing here applies. `inside_the_session` is that test: the ambient session is
/// this session.
///
/// Returns the sentence to print, naming the flag that would have made the command unambiguous.
pub fn missing_target_from_outside_a_pane(
    action: &CliAction,
    inside_the_session: bool,
) -> Option<String> {
    if inside_the_session {
        return None;
    }
    let needs = |verb: &str, flag: &str, lister: &str| {
        Some(format!(
            "`{verb}` run from outside a pane has no focused {thing} to act on. \
             Pass {flag}, or run it from inside the session. `zellij action {lister}` lists them.",
            verb = verb,
            thing = if flag == "--pane-id" { "pane" } else { "tab" },
            flag = flag,
            lister = lister,
        ))
    };
    match action {
        CliAction::ClosePane { pane_id: None } => needs("close-pane", "--pane-id", "list-panes"),
        // writing into a pane is the same footgun as closing one: the keystrokes land in whichever
        // pane the server found, and a shell that received them has already run them
        CliAction::Write { pane_id: None, .. } => needs("write", "--pane-id", "list-panes"),
        CliAction::WriteChars { pane_id: None, .. } => {
            needs("write-chars", "--pane-id", "list-panes")
        },
        CliAction::Clear { pane_id: None } => needs("clear", "--pane-id", "list-panes"),
        CliAction::EditScrollback { pane_id: None, .. } => {
            needs("edit-scrollback", "--pane-id", "list-panes")
        },
        CliAction::RenamePane { pane_id: None, .. } => {
            needs("rename-pane", "--pane-id", "list-panes")
        },
        CliAction::CloseTab { tab_id: None } => needs("close-tab", "--tab-id", "list-tabs"),
        CliAction::MoveTab { tab_id: None, .. } => needs("move-tab", "--tab-id", "list-tabs"),
        CliAction::BreakPane { pane_id, .. } if pane_id.is_empty() => {
            needs("break-pane", "--pane-id", "list-panes")
        },
        // these two name no pane at all, by design - they are keybindings that happen to be
        // reachable from the CLI, so the way to aim them is to use the verb that can be aimed
        CliAction::BreakPaneRight | CliAction::BreakPaneLeft => Some(
            "`break-pane-right` and `break-pane-left` act on the focused pane and cannot name \
             one, so they mean nothing from outside a pane. Use `break-pane --pane-id` instead."
                .to_owned(),
        ),
        _ => None,
    }
}

/// Whether `switch-session --pane-id` was handed a target only the other session could resolve.
///
/// A handle or a uuid names a pane against one session's live panes, and the only registry this
/// process can reach is the session it is standing in. Resolving one here and sending the number on
/// would land the switch on whichever pane happens to wear that id in the target session - a pane
/// the caller never named. Asking the target session instead would need a query across sessions
/// that the protocol does not carry.
///
/// So the id forms, which mean the same thing in every session, pass through untouched, and the
/// rest are refused. A string that names a pane in no form at all is refused here too, in the
/// parser's own words: it is malformed input like any other, and every other `--pane-id` reports it
/// and exits 1, so this one does the same rather than turning it into a miss.
///
/// Returns the sentence to print, before exiting 1.
pub fn cross_session_pane_target_needs_an_id(action: &CliAction) -> Option<String> {
    let (name, pane_id) = match action {
        CliAction::SwitchSession {
            name,
            pane_id: Some(pane_id),
            ..
        } => (name, pane_id),
        _ => return None,
    };
    match pane_id.parse::<crate::data::PaneTarget>() {
        Ok(crate::data::PaneTarget::Id(_)) => None,
        Err(malformed) => Some(malformed),
        Ok(_) => Some(format!(
            "`switch-session --pane-id {pane_id}` reads '{pane_id}' against this session, not \
             against '{name}', so it cannot mean a pane there. A --pane-id for another session \
             must be an id form: terminal_1, plugin_2, or a bare integer.",
            pane_id = pane_id,
            name = name,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::PaneId;
    use crate::input::actions::{pane_ids_only, Action};
    use clap::Parser;

    /// Parse a command line on a thread with a real stack. See [`on_big_stack`].
    fn parse_cli(args: Vec<String>) -> Result<CliArgs, clap::Error> {
        on_big_stack(move || CliArgs::try_parse_from(args))
    }

    fn parse_action(args: &[&str]) -> CliAction {
        let mut full_args = vec!["zellij".to_string(), "action".to_string()];
        full_args.extend(args.iter().map(|a| a.to_string()));
        let cli = parse_cli(full_args).unwrap();
        match cli.command {
            Some(Command::Action(action)) => *action,
            other => panic!("Expected Action, got {:?}", other),
        }
    }

    fn action_parse_fails(args: &[&str]) -> bool {
        let mut full_args = vec!["zellij".to_string(), "action".to_string()];
        full_args.extend(args.iter().map(|a| a.to_string()));
        parse_cli(full_args).is_err()
    }

    fn parse_subscribe(args: &[&str]) -> SubscribeCli {
        let mut full_args = vec!["zellij".to_string()];
        full_args.extend(args.iter().map(|a| a.to_string()));
        let cli = parse_cli(full_args).unwrap();
        match cli.command {
            Some(Command::Subscribe(s)) => s,
            other => panic!("Expected Subscribe, got {:?}", other),
        }
    }

    #[test]
    fn subscribe_scrollback_bare_flag() {
        let s = parse_subscribe(&["subscribe", "--pane-id", "terminal_1", "--scrollback"]);
        assert_eq!(s.scrollback, Some(0));
    }

    #[test]
    fn subscribe_scrollback_with_value() {
        let s = parse_subscribe(&[
            "subscribe",
            "--pane-id",
            "terminal_1",
            "--scrollback",
            "100",
        ]);
        assert_eq!(s.scrollback, Some(100));
    }

    #[test]
    fn subscribe_scrollback_absent() {
        let s = parse_subscribe(&["subscribe", "--pane-id", "terminal_1"]);
        assert_eq!(s.scrollback, None);
    }

    #[test]
    fn subscribe_format_json() {
        let s = parse_subscribe(&["subscribe", "--pane-id", "terminal_1", "--format", "json"]);
        assert!(matches!(s.format, SubscribeFormat::Json));
    }

    #[test]
    fn subscribe_format_default_raw() {
        let s = parse_subscribe(&["subscribe", "--pane-id", "terminal_1"]);
        assert!(matches!(s.format, SubscribeFormat::Raw));
    }

    #[test]
    fn subscribe_timestamps_are_off_until_asked_for() {
        let bare = parse_subscribe(&["subscribe", "--pane-id", "terminal_1"]);
        assert!(!bare.timestamps);
        let stamped = parse_subscribe(&["subscribe", "--pane-id", "terminal_1", "--timestamps"]);
        assert!(stamped.timestamps);
    }

    #[test]
    fn an_event_timestamp_is_rfc3339_utc_to_the_millisecond() {
        let epoch = event_timestamp(std::time::UNIX_EPOCH);
        assert_eq!(epoch, "1970-01-01T00:00:00.000Z");
        let later =
            event_timestamp(std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_234_567));
        assert_eq!(later, "1970-01-01T00:20:34.567Z");
    }

    #[test]
    fn subscribe_multiple_pane_ids() {
        let s = parse_subscribe(&[
            "subscribe",
            "--pane-id",
            "terminal_1",
            "--pane-id",
            "plugin_2",
        ]);
        assert_eq!(
            s.pane_id,
            vec!["terminal_1".to_string(), "plugin_2".to_string()]
        );
    }

    #[test]
    fn subscribe_requires_pane_id() {
        let result = parse_cli(vec!["zellij".to_string(), "subscribe".to_string()]);
        assert!(result.is_err());
    }

    #[test]
    fn close_pane_with_a_pane_id_sends_close_focus_by_pane_id() {
        // the action this maps to decides which ScreenInstruction reports a missing pane;
        // `CloseTerminalPane` is a different path and fixing that one does not fix this one
        let actions = Action::actions_from_cli(
            parse_action(&["close-pane", "--pane-id", "terminal_9"]),
            Box::new(PathBuf::new),
            None,
            &pane_ids_only,
        )
        .expect("TEST");
        assert_eq!(
            actions,
            vec![Action::CloseFocusByPaneId {
                pane_id: PaneId::Terminal(9)
            }]
        );
    }

    #[test]
    fn a_setter_takes_a_boolish_value_and_an_optional_target() {
        match parse_action(&["set-fullscreen", "on", "--pane-id", "terminal_1"]) {
            CliAction::SetFullscreen { enabled, pane_id } => {
                assert!(enabled);
                assert_eq!(pane_id.as_deref(), Some("terminal_1"));
            },
            other => panic!("Expected SetFullscreen, got {:?}", other),
        }
        match parse_action(&["set-pane-pinned", "false"]) {
            CliAction::SetPanePinned { enabled, pane_id } => {
                assert!(!enabled);
                assert_eq!(pane_id, None, "no target means the focused pane");
            },
            other => panic!("Expected SetPanePinned, got {:?}", other),
        }
        match parse_action(&["set-pane-floating", "1", "--pane-id", "3"]) {
            CliAction::SetPaneFloating { enabled, .. } => assert!(enabled),
            other => panic!("Expected SetPaneFloating, got {:?}", other),
        }
        match parse_action(&["set-sync-tab", "off", "--tab-id", "2"]) {
            CliAction::SetSyncTab { enabled, tab_id } => {
                assert!(!enabled);
                assert_eq!(tab_id, Some(2));
            },
            other => panic!("Expected SetSyncTab, got {:?}", other),
        }
    }

    #[test]
    fn a_setter_refuses_a_missing_or_unreadable_value() {
        assert!(action_parse_fails(&["set-fullscreen"]));
        assert!(action_parse_fails(&["set-fullscreen", "maybe"]));
        assert!(action_parse_fails(&["set-sync-tab"]));
    }

    #[test]
    fn break_pane_takes_several_panes_and_a_name() {
        let action = parse_action(&[
            "break-pane",
            "--pane-id",
            "terminal_1",
            "--pane-id",
            "plugin_2",
            "--name",
            "build",
            "--no-focus",
        ]);
        match action {
            CliAction::BreakPane {
                pane_id,
                name,
                no_focus,
            } => {
                assert_eq!(pane_id, vec!["terminal_1", "plugin_2"]);
                assert_eq!(name.as_deref(), Some("build"));
                assert!(no_focus);
            },
            other => panic!("Expected BreakPane, got {:?}", other),
        }
    }

    #[test]
    fn break_pane_with_no_target_means_the_focused_pane() {
        let action = parse_action(&["break-pane"]);
        match action {
            CliAction::BreakPane {
                pane_id, no_focus, ..
            } => {
                assert!(pane_id.is_empty());
                assert!(!no_focus);
            },
            other => panic!("Expected BreakPane, got {:?}", other),
        }
    }

    #[test]
    fn break_pane_to_tab_needs_both_ends() {
        let action = parse_action(&["break-pane-to-tab", "--pane-id", "3", "--tab-id", "2"]);
        match action {
            CliAction::BreakPaneToTab {
                pane_id,
                tab_id,
                no_focus,
            } => {
                assert_eq!(pane_id, vec!["3"]);
                assert_eq!(tab_id, 2);
                assert!(!no_focus);
            },
            other => panic!("Expected BreakPaneToTab, got {:?}", other),
        }
        assert!(action_parse_fails(&["break-pane-to-tab", "--tab-id", "2"]));
        assert!(action_parse_fails(&["break-pane-to-tab", "--pane-id", "3"]));
    }

    #[test]
    fn signal_pane_defaults_to_sigint() {
        let action = parse_action(&["signal-pane", "--pane-id", "terminal_1"]);
        match action {
            CliAction::SignalPane { pane_id, signal } => {
                assert_eq!(pane_id, "terminal_1");
                assert_eq!(signal, PaneSignal::Int);
            },
            other => panic!("Expected SignalPane, got {:?}", other),
        }
    }

    #[test]
    fn signal_pane_takes_the_named_signal() {
        for (name, expected) in [
            ("int", PaneSignal::Int),
            ("hup", PaneSignal::Hup),
            ("kill", PaneSignal::Kill),
        ] {
            let action = parse_action(&["signal-pane", "--pane-id", "3", "--signal", name]);
            match action {
                CliAction::SignalPane { signal, .. } => assert_eq!(signal, expected),
                other => panic!("Expected SignalPane, got {:?}", other),
            }
        }
    }

    #[test]
    fn signal_pane_needs_a_pane_and_rejects_an_unknown_signal() {
        assert!(action_parse_fails(&["signal-pane"]));
        assert!(action_parse_fails(&[
            "signal-pane",
            "--pane-id",
            "1",
            "--signal",
            "term"
        ]));
    }

    #[test]
    fn go_to_pane_takes_no_focus() {
        // the same probe flag `go-to-tab-name` has, on the alias people reach for
        let action = parse_action(&["go-to-pane", "sunny-otter", "--no-focus"]);
        match action {
            CliAction::FocusPaneId { pane_id, no_focus } => {
                assert_eq!(pane_id, "sunny-otter");
                assert!(no_focus);
            },
            other => panic!("Expected FocusPaneId, got {:?}", other),
        }
        // and the negative control: without the flag it is the jump it always was
        match parse_action(&["go-to-pane", "sunny-otter"]) {
            CliAction::FocusPaneId { no_focus, .. } => assert!(!no_focus),
            other => panic!("Expected FocusPaneId, got {:?}", other),
        }
    }

    #[test]
    fn go_to_tab_name_no_focus() {
        let action = parse_action(&["go-to-tab-name", "build", "--create", "--no-focus"]);
        match action {
            CliAction::GoToTabName {
                name,
                create,
                no_focus,
            } => {
                assert_eq!(name, "build");
                assert!(create);
                assert!(no_focus);
            },
            other => panic!("Expected GoToTabName, got {:?}", other),
        }
    }

    #[test]
    fn a_targetless_close_from_outside_a_pane_is_refused() {
        for args in [
            vec!["close-pane"],
            vec!["close-tab"],
            vec!["move-tab", "left"],
            vec!["break-pane"],
            vec!["break-pane-right"],
            vec!["break-pane-left"],
            vec!["write", "27"],
            vec!["write-chars", "hello"],
            vec!["clear"],
            vec!["edit-scrollback"],
            vec!["rename-pane", "build"],
        ] {
            let action = parse_action(&args);
            assert!(
                missing_target_from_outside_a_pane(&action, false).is_some(),
                "expected `{}` to need a target",
                args.join(" ")
            );
        }
    }

    #[test]
    fn the_refusal_names_the_verb_and_the_flag_that_answers_it() {
        for (args, verb) in [
            (vec!["write", "27"], "write"),
            (vec!["write-chars", "hello"], "write-chars"),
            (vec!["clear"], "clear"),
            (vec!["edit-scrollback"], "edit-scrollback"),
            (vec!["rename-pane", "build"], "rename-pane"),
        ] {
            let action = parse_action(&args);
            let message = missing_target_from_outside_a_pane(&action, false)
                .unwrap_or_else(|| panic!("expected `{}` to be refused", args.join(" ")));
            assert!(message.contains(verb), "got: {}", message);
            assert!(message.contains("--pane-id"), "got: {}", message);
            assert!(message.contains("list-panes"), "got: {}", message);
        }
    }

    #[test]
    fn the_same_commands_are_untouched_from_inside_a_pane() {
        // the negative control: inside the session, "focused" is the pane the hands are on, which
        // is what the caller meant
        for args in [
            vec!["close-pane"],
            vec!["close-tab"],
            vec!["move-tab", "left"],
            vec!["break-pane"],
            vec!["break-pane-right"],
            vec!["break-pane-left"],
            vec!["write", "27"],
            vec!["write-chars", "hello"],
            vec!["clear"],
            vec!["edit-scrollback"],
            vec!["rename-pane", "build"],
        ] {
            let action = parse_action(&args);
            assert_eq!(
                missing_target_from_outside_a_pane(&action, true),
                None,
                "`{}` should be untouched from inside a pane",
                args.join(" ")
            );
        }
    }

    #[test]
    fn a_command_that_names_its_target_is_never_refused() {
        for args in [
            vec!["close-pane", "--pane-id", "terminal_1"],
            vec!["close-tab", "--tab-id", "2"],
            vec!["move-tab", "left", "--tab-id", "2"],
            vec!["break-pane", "--pane-id", "terminal_1"],
            vec!["write", "27", "--pane-id", "terminal_1"],
            vec!["write-chars", "hello", "--pane-id", "sunny-otter"],
            vec!["clear", "--pane-id", "terminal_1"],
            vec!["edit-scrollback", "--pane-id", "terminal_1"],
            vec!["rename-pane", "build", "--pane-id", "terminal_1"],
        ] {
            let action = parse_action(&args);
            assert_eq!(
                missing_target_from_outside_a_pane(&action, false),
                None,
                "`{}` names its target",
                args.join(" ")
            );
        }
    }

    #[test]
    fn a_command_that_mutates_nothing_is_never_refused() {
        // reading and creating are not the footgun: a query has no target to get wrong, and a new
        // pane in the wrong tab is visible and undoable in a way a closed one is not
        for args in [
            vec!["list-panes"],
            vec!["list-tabs"],
            vec!["new-pane"],
            vec!["new-tab"],
        ] {
            let action = parse_action(&args);
            assert_eq!(missing_target_from_outside_a_pane(&action, false), None);
        }
    }

    /// The actions a `zellij action` command line turns into, with no session to ask.
    fn actions_of(args: &[&str]) -> Result<Vec<Action>, String> {
        Action::actions_from_cli(
            parse_action(args),
            Box::new(|| std::path::PathBuf::from("/tmp")),
            None,
            &pane_ids_only,
        )
    }

    #[test]
    fn new_tab_takes_a_name_or_none_at_all() {
        let named = parse_action(&["new-pane", "--new-tab", "build"]);
        let bare = parse_action(&["new-pane", "--new-tab"]);
        let without = parse_action(&["new-pane"]);
        let new_tab = |action: &CliAction| match action {
            CliAction::NewPane { new_tab, .. } => new_tab.clone(),
            other => panic!("Expected NewPane, got {:?}", other),
        };
        assert_eq!(new_tab(&named), Some(Some("build".to_owned())));
        assert_eq!(new_tab(&bare), Some(None));
        assert_eq!(new_tab(&without), None);
    }

    #[test]
    fn a_pane_in_a_new_tab_is_one_tab_carrying_one_pane() {
        // the point of the flag: one action, so the tab and the pane it holds are made together
        // and reported together, rather than a tab that opens a shell and a pane beside it
        let actions = actions_of(&["new-pane", "--new-tab", "build", "--", "cargo", "test"])
            .expect("a new tab with a command in it");
        assert_eq!(actions.len(), 1, "got: {:?}", actions);
        match &actions[0] {
            Action::NewTab {
                tiled_layout,
                tab_name,
                initial_panes,
                should_change_focus_to_new_tab,
                ..
            } => {
                assert_eq!(tab_name.as_deref(), Some("build"));
                assert!(should_change_focus_to_new_tab);
                // no layout of our own: the tab is built from the session's new-tab template, so it
                // keeps its status bars and its panes are serialized like any other tab's
                assert!(tiled_layout.is_none(), "got: {:?}", tiled_layout);
                let panes = initial_panes.as_ref().expect("the command");
                assert_eq!(panes.len(), 1);
                match &panes[0] {
                    crate::data::CommandOrPlugin::Command(command) => {
                        assert_eq!(command.command, std::path::PathBuf::from("cargo"));
                        assert_eq!(command.args, vec!["test".to_owned()]);
                    },
                    other => panic!("Expected a command, got {:?}", other),
                }
            },
            other => panic!("Expected NewTab, got {:?}", other),
        }
    }

    #[test]
    fn a_new_tab_that_names_nothing_still_makes_the_tab() {
        let actions = actions_of(&["new-pane", "--new-tab"]).expect("a new tab");
        match &actions[0] {
            Action::NewTab {
                tab_name,
                initial_panes,
                ..
            } => {
                assert_eq!(tab_name, &None, "zellij names it");
                // no command: the tab opens the shell, which is what a bare `new-pane` opens
                assert!(initial_panes.is_none());
            },
            other => panic!("Expected NewTab, got {:?}", other),
        }
    }

    #[test]
    fn a_new_tab_keeps_the_focus_where_it_is_when_asked() {
        let actions = actions_of(&["new-pane", "--new-tab", "--no-focus"]).expect("a new tab");
        match &actions[0] {
            Action::NewTab {
                should_change_focus_to_new_tab,
                ..
            } => assert!(!should_change_focus_to_new_tab),
            other => panic!("Expected NewTab, got {:?}", other),
        }
    }

    #[test]
    fn a_new_tab_refuses_the_flags_that_place_a_pane_in_an_existing_one() {
        // each of these says where the pane goes, and `--new-tab` has already answered that
        for args in [
            vec!["new-pane", "--new-tab", "--stacked"],
            vec!["new-pane", "--new-tab", "-d", "right"],
            vec!["new-pane", "--new-tab", "--floating"],
            vec!["new-pane", "--new-tab", "--in-place"],
            vec!["new-pane", "--new-tab", "--tab-id", "2"],
            vec!["new-pane", "--new-tab", "--near-current-pane"],
            // the pane in a new tab cannot carry a name, so the flag is refused rather than dropped
            vec!["new-pane", "--new-tab", "--name", "shell"],
        ] {
            assert!(
                action_parse_fails(&args),
                "expected `{}` to be refused",
                args.join(" ")
            );
        }
    }

    #[test]
    fn a_new_tab_says_which_waiting_flag_it_can_honour() {
        // `--blocking` waits for a pane to close and has no pane to name in a tab that does not
        // exist yet; the conditional ones ride on the tab's first pane
        assert!(action_parse_fails(&["new-pane", "--new-tab", "--blocking"]));
        let actions = actions_of(&["new-pane", "--new-tab", "--block-until-exit", "--", "true"])
            .expect("a new tab that waits for its command");
        match &actions[0] {
            Action::NewTab {
                first_pane_unblock_condition,
                ..
            } => assert_eq!(
                first_pane_unblock_condition,
                &Some(UnblockCondition::OnAnyExit)
            ),
            other => panic!("Expected NewTab, got {:?}", other),
        }
    }

    #[test]
    fn a_chosen_handle_is_taken_out_of_the_request() {
        let mut action = parse_action(&["new-pane", "--handle", "build"]);
        assert_eq!(action.take_chosen_handle().as_deref(), Some("build"));
        // taken once: the client applies it, and it must not also travel with the action
        assert_eq!(action.take_chosen_handle(), None);
    }

    #[test]
    fn a_handle_that_could_be_read_as_an_id_is_refused_at_the_parser() {
        for rejected in ["terminal_1", "7", "Build", "my handle", "terminal-1"] {
            assert!(
                action_parse_fails(&["new-pane", "--handle", rejected]),
                "expected `--handle {}` to be refused",
                rejected
            );
        }
        // the negative control: a name a person would pick goes through
        assert_eq!(
            parse_action(&["new-pane", "--handle", "my-build"]).take_chosen_handle(),
            Some("my-build".to_owned())
        );
    }

    #[test]
    fn a_handle_cannot_ride_with_a_command_that_prints_no_pane() {
        // the blocking family answers with an exit status instead of a `pane_id:`, and the handle
        // is applied to the pane the report names
        for waiting in [
            "--blocking",
            "--block-until-exit",
            "--block-until-exit-success",
            "--block-until-exit-failure",
        ] {
            assert!(
                action_parse_fails(&["new-pane", "--handle", "build", waiting]),
                "expected `--handle` with `{}` to be refused",
                waiting
            );
        }
    }

    #[test]
    fn an_unapplied_handle_never_reaches_the_session() {
        let error = actions_of(&["new-pane", "--handle", "build"])
            .expect_err("a handle still on the request is not an action");
        assert!(error.contains("--handle"), "got: {}", error);
    }

    #[test]
    fn near_takes_every_form_a_pane_answers_to() {
        for target in [
            "terminal_1",
            "3",
            "sunny-otter",
            "e9b82dbd-0000-4000-8000-0000000000aa",
        ] {
            let action = parse_action(&["new-pane", "--near", target]);
            assert_eq!(action.near_target(), Some(target));
        }
    }

    #[test]
    fn an_anchored_pane_asks_for_the_pane_the_command_came_from() {
        // the anchor rides on the channel `--near-current-pane` uses, so what is left in the action
        // is that there is one; the id itself is the client's to carry
        let mut action = parse_action(&["new-pane", "--near", "sunny-otter"]);
        action.anchor_near();
        match &action {
            CliAction::NewPane {
                near,
                near_current_pane,
                ..
            } => {
                assert_eq!(near, &None, "the name is spent once it is resolved");
                assert!(near_current_pane);
            },
            other => panic!("Expected NewPane, got {:?}", other),
        }
    }

    #[test]
    fn an_unresolved_near_never_reaches_the_session() {
        // without the lookup the pane would open beside whichever pane the server found, which is
        // the pane `--near` was passed to avoid
        let error = actions_of(&["new-pane", "--near", "sunny-otter"])
            .expect_err("an unresolved --near is not an action");
        assert!(error.contains("--near"), "got: {}", error);
    }

    #[test]
    fn near_refuses_the_flags_that_place_the_pane_somewhere_else() {
        for args in [
            vec!["new-pane", "--near", "terminal_1", "--near-current-pane"],
            vec!["new-pane", "--near", "terminal_1", "--tab-id", "2"],
            vec!["new-pane", "--near", "terminal_1", "--in-tab", "logs"],
            vec!["new-pane", "--near", "terminal_1", "--new-tab"],
            vec!["new-pane", "--near", "terminal_1", "--in-place"],
        ] {
            assert!(
                action_parse_fails(&args),
                "expected `{}` to be refused",
                args.join(" ")
            );
        }
    }

    #[test]
    fn in_tab_becomes_a_tab_id_and_takes_nobodys_focus() {
        let mut action = parse_action(&["new-pane", "--in-tab", "logs"]);
        assert_eq!(action.in_tab_target(), Some("logs"));
        action.place_in_tab(4);
        match &action {
            CliAction::NewPane {
                in_tab,
                tab_id,
                no_focus,
                ..
            } => {
                assert_eq!(in_tab, &None, "the name is spent once it is resolved");
                assert_eq!(tab_id, &Some(4));
                assert!(no_focus, "putting a pane somewhere is not going there");
            },
            other => panic!("Expected NewPane, got {:?}", other),
        }
    }

    #[test]
    fn an_unresolved_in_tab_never_reaches_the_session() {
        // the guard: a caller that skipped the lookup would open the pane in the focused tab, which
        // is the one tab `--in-tab` was passed to avoid
        let error = actions_of(&["new-pane", "--in-tab", "logs"])
            .expect_err("an unresolved --in-tab is not an action");
        assert!(error.contains("--in-tab"), "got: {}", error);
    }

    #[test]
    fn in_tab_refuses_the_other_ways_of_naming_a_tab() {
        for args in [
            vec!["new-pane", "--in-tab", "logs", "--tab-id", "2"],
            vec!["new-pane", "--in-tab", "logs", "--new-tab"],
            vec!["new-pane", "--in-tab", "logs", "--in-place"],
            vec!["new-pane", "--in-tab", "logs", "--near-current-pane"],
        ] {
            assert!(
                action_parse_fails(&args),
                "expected `{}` to be refused",
                args.join(" ")
            );
        }
        // the negative control: the flags that say how the pane is drawn, not where it goes, are
        // still free to travel with it
        assert_eq!(
            parse_action(&["new-pane", "--in-tab", "logs", "-d", "right"]).in_tab_target(),
            Some("logs")
        );
    }

    #[test]
    fn write_chars_and_paste_take_their_text_or_leave_it_to_stdin() {
        for verb in ["write-chars", "paste"] {
            let with_text = parse_action(&[verb, "hello"]);
            let from_stdin = parse_action(&[verb]);
            let explicit = parse_action(&[verb, "-"]);
            let text = |action: &CliAction| match action {
                CliAction::WriteChars { chars, .. } | CliAction::Paste { chars, .. } => {
                    chars.clone()
                },
                other => panic!("Expected {}, got {:?}", verb, other),
            };
            assert_eq!(text(&with_text), Some("hello".to_owned()));
            assert_eq!(text(&from_stdin), None);
            assert_eq!(text(&explicit), Some("-".to_owned()));
        }
    }

    #[test]
    fn stdin_text_arrives_whole() {
        // multi-line, and nothing added or trimmed: what was piped is what the pane is typed
        let piped = "first line\nsecond line\n";
        assert_eq!(
            text_from_stdin(piped.as_bytes(), "write-chars"),
            Ok(piped.to_owned())
        );
        // the negative control: an empty pipe is not an error here, it is the caller's miss
        assert_eq!(text_from_stdin(&b""[..], "write-chars"), Ok(String::new()));
    }

    #[test]
    fn stdin_text_stops_at_the_bound() {
        let at_the_bound = vec![b'x'; MAX_STDIN_TEXT_BYTES];
        assert!(text_from_stdin(&at_the_bound[..], "paste").is_ok());
        let one_past = vec![b'x'; MAX_STDIN_TEXT_BYTES + 1];
        let message = text_from_stdin(&one_past[..], "paste").expect_err("past the bound");
        assert!(
            message.contains(&MAX_STDIN_TEXT_BYTES.to_string()),
            "{}",
            message
        );
    }

    #[test]
    fn stdin_that_is_not_text_says_which_command_takes_bytes() {
        let message = text_from_stdin(&[0xff, 0xfe][..], "write-chars").expect_err("not utf-8");
        assert!(message.contains("write"), "{}", message);
    }

    #[test]
    fn a_cross_session_pane_target_that_needs_a_registry_is_refused() {
        // the registry a handle or a uuid would be read against is this session's, and the pane is
        // in the other one - so the id it resolves to names a pane the caller never asked for
        for target in ["sunny-otter", "e9b82dbd-0000-4000-8000-0000000000aa"] {
            let action = parse_action(&["switch-session", "other", "--pane-id", target]);
            let message = cross_session_pane_target_needs_an_id(&action)
                .unwrap_or_else(|| panic!("expected `{}` to be refused", target));
            assert!(message.contains(target), "got: {}", message);
            assert!(message.contains("terminal_1"), "got: {}", message);
        }
    }

    #[test]
    fn a_cross_session_id_form_passes_through() {
        // the negative control: an id means the same thing in every session, so nothing is resolved
        // and nothing is refused
        for target in ["terminal_7", "plugin_2", "3"] {
            let action = parse_action(&["switch-session", "other", "--pane-id", target]);
            assert_eq!(
                cross_session_pane_target_needs_an_id(&action),
                None,
                "`{}` is an id form",
                target
            );
        }
        // and a switch that names no pane has nothing to refuse
        let action = parse_action(&["switch-session", "other"]);
        assert_eq!(cross_session_pane_target_needs_an_id(&action), None);
    }

    #[test]
    fn a_malformed_cross_session_target_keeps_the_parsers_own_words() {
        // malformed input exits 1 wherever it appears, and the sentence that names the four forms
        // is more use than one about the session boundary, which is not what went wrong
        let action = parse_action(&["switch-session", "other", "--pane-id", "not a pane"]);
        let message = cross_session_pane_target_needs_an_id(&action)
            .expect("a malformed target is refused, not passed on");
        assert!(message.contains("does not name a pane"), "got: {}", message);
        assert!(
            !message.contains("switch-session"),
            "the session boundary is not what went wrong: {}",
            message
        );
    }

    #[test]
    fn switch_session_never_reaches_the_local_registry() {
        // the structural half: even handed a resolver that would answer, `switch-session` does not
        // ask it - the answer would be about the wrong session's panes
        let asked = std::cell::RefCell::new(Vec::new());
        let resolver = |target: &str| -> Result<PaneId, String> {
            asked.borrow_mut().push(target.to_owned());
            Ok(PaneId::Terminal(99))
        };
        let action = parse_action(&["switch-session", "other", "--pane-id", "sunny-otter"]);
        let built =
            Action::actions_from_cli(action, Box::new(|| PathBuf::from("/")), None, &resolver);
        assert!(
            built.is_err(),
            "a handle cannot be resolved for another session"
        );
        assert!(
            asked.borrow().is_empty(),
            "switch-session asked this session about {:?}",
            asked.borrow()
        );

        // and an id form is carried across untouched, still without asking
        let action = parse_action(&["switch-session", "other", "--pane-id", "terminal_7"]);
        let built =
            Action::actions_from_cli(action, Box::new(|| PathBuf::from("/")), None, &resolver)
                .expect("an id form needs no registry");
        assert_eq!(
            built,
            vec![Action::SwitchSession {
                name: "other".to_owned(),
                tab_position: None,
                pane_id: Some((7, false)),
                layout: None,
                cwd: None,
            }]
        );
        assert!(asked.borrow().is_empty(), "an id form needs no lookup");
    }

    #[test]
    fn dump_screen_takes_its_path_as_an_argument() {
        match parse_action(&["dump-screen", "--pane-id", "terminal_1", "/tmp/dump"]) {
            CliAction::DumpScreen { file, path, .. } => {
                assert_eq!(file, Some(PathBuf::from("/tmp/dump")));
                assert_eq!(path, None);
            },
            other => panic!("Expected DumpScreen, got {:?}", other),
        }
    }

    #[test]
    fn dump_screen_still_takes_its_path_as_a_flag() {
        match parse_action(&[
            "dump-screen",
            "--pane-id",
            "terminal_1",
            "--path",
            "/tmp/dump",
        ]) {
            CliAction::DumpScreen { file, path, .. } => {
                assert_eq!(file, None);
                assert_eq!(path, Some(PathBuf::from("/tmp/dump")));
            },
            other => panic!("Expected DumpScreen, got {:?}", other),
        }
    }

    #[test]
    fn dump_screen_refuses_both_spellings_of_the_path() {
        assert!(action_parse_fails(&[
            "dump-screen",
            "/tmp/dump",
            "--path",
            "/tmp/other"
        ]));
    }

    #[test]
    fn go_to_tab_name_focuses_by_default() {
        let action = parse_action(&["go-to-tab-name", "build"]);
        match action {
            CliAction::GoToTabName {
                create, no_focus, ..
            } => {
                assert!(!create);
                assert!(!no_focus);
            },
            other => panic!("Expected GoToTabName, got {:?}", other),
        }
    }
}
