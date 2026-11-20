//! Handles cli and configuration options
use crate::cli::Command;
use crate::data::{FloatingPaneCoordinates, HiddenBars, InputMode, ThemeHue, WebSharing};
use crate::input::layout::PercentOrFixed;
use crate::resurrect_command_hints::{ResurrectCommandHints, ResurrectCommands};
use crate::session_service::SessionServiceOptions;
use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::str::FromStr;

use std::net::IpAddr;

pub const DEFAULT_WORD_SEPARATORS: &str = "[]{}<>()";

/// The size a floating pane gets when nothing asks for a specific one.
///
/// Without this, every floating pane that carries no coordinates of its own - the session
/// manager, the plugin manager, a bare `NewFloatingPane` - lands at half the viewport, which is
/// small enough that those plugins truncate the very columns the user opened them to read. A
/// `None` field keeps the built-in size for that axis, so an absent block is byte-identical to
/// upstream behaviour.
#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
pub struct DefaultFloatingSize {
    #[serde(default)]
    pub width: Option<PercentOrFixed>,
    #[serde(default)]
    pub height: Option<PercentOrFixed>,
}

impl DefaultFloatingSize {
    pub fn is_empty(&self) -> bool {
        self.width.is_none() && self.height.is_none()
    }
    /// Fill in the width/height the caller did not ask for, leaving anything explicit alone.
    pub fn apply_to(
        &self,
        coordinates: Option<FloatingPaneCoordinates>,
    ) -> Option<FloatingPaneCoordinates> {
        if self.is_empty() {
            return coordinates;
        }
        let mut coordinates = coordinates.unwrap_or_default();
        if coordinates.width.is_none() {
            coordinates.width = self.width.clone();
        }
        if coordinates.height.is_none() {
            coordinates.height = self.height.clone();
        }
        Some(coordinates)
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Deserialize, Serialize, ValueEnum)]
pub enum OnForceClose {
    #[serde(alias = "quit")]
    Quit,
    #[serde(alias = "detach")]
    Detach,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
pub enum NestedSessionHandling {
    #[serde(alias = "ask")]
    Ask,
    #[serde(alias = "fullscreen")]
    Fullscreen,
    #[serde(alias = "descend")]
    Descend,
    #[serde(alias = "never")]
    Never,
}

impl Default for NestedSessionHandling {
    fn default() -> Self {
        Self::Ask
    }
}

impl FromStr for NestedSessionHandling {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Ask" | "ask" => Ok(Self::Ask),
            "Fullscreen" | "fullscreen" => Ok(Self::Fullscreen),
            "Descend" | "descend" => Ok(Self::Descend),
            "Never" | "never" => Ok(Self::Never),
            _ => Err(format!("No such nested_session_handling: {}", s)),
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
pub enum OnQuit {
    #[serde(alias = "ask_quit")]
    #[value(name = "ask_quit")]
    AskQuit,
    #[serde(alias = "ask_detach")]
    #[value(name = "ask_detach")]
    AskDetach,
    #[serde(alias = "quit")]
    Quit,
    #[serde(alias = "detach")]
    Detach,
}

impl Default for OnQuit {
    fn default() -> Self {
        Self::AskQuit
    }
}

impl OnQuit {
    pub fn as_str(&self) -> &'static str {
        match self {
            OnQuit::AskQuit => "ask_quit",
            OnQuit::AskDetach => "ask_detach",
            OnQuit::Quit => "quit",
            OnQuit::Detach => "detach",
        }
    }
    pub const ALL: [OnQuit; 4] = [
        OnQuit::AskQuit,
        OnQuit::AskDetach,
        OnQuit::Quit,
        OnQuit::Detach,
    ];
}

impl FromStr for OnQuit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        OnQuit::ALL
            .into_iter()
            .find(|on_quit| on_quit.as_str() == s)
            .ok_or_else(|| {
                format!(
                    "No such on_quit: {} (expected ask_quit, ask_detach, quit or detach)",
                    s
                )
            })
    }
}

/// What happens when a key is pressed in a pane whose viewport is scrolled up.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
pub enum InputWhileScrolled {
    /// Scrolling or focusing a scrolled pane implicitly enters Scroll mode.
    #[serde(alias = "scroll-mode")]
    ScrollMode,
    /// No implicit mode switch. A key clears the pane's scroll, flushes the held
    /// output, then reaches the pty.
    #[serde(alias = "jump")]
    Jump,
    /// No implicit mode switch. A key reaches the pty and the viewport stays put.
    #[serde(alias = "stay")]
    Stay,
}

impl Default for InputWhileScrolled {
    fn default() -> Self {
        Self::ScrollMode
    }
}

impl FromStr for InputWhileScrolled {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "ScrollMode" | "scroll-mode" => Ok(Self::ScrollMode),
            "Jump" | "jump" => Ok(Self::Jump),
            "Stay" | "stay" => Ok(Self::Stay),
            _ => Err(format!("No such input_while_scrolled: {}", s)),
        }
    }
}

/// What happens to a command pane when its command exits with status 0.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
pub enum CommandPaneOnCleanExit {
    /// The pane holds, showing EXIT CODE: 0 and waiting for ENTER, ESC or Ctrl-C.
    #[serde(alias = "hold")]
    Hold,
    /// The pane drops to a shell, exactly as an ESC press does.
    #[serde(alias = "shell")]
    Shell,
}

impl Default for CommandPaneOnCleanExit {
    fn default() -> Self {
        Self::Hold
    }
}

impl FromStr for CommandPaneOnCleanExit {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Hold" | "hold" => Ok(Self::Hold),
            "Shell" | "shell" => Ok(Self::Shell),
            _ => Err(format!("No such command_pane_on_clean_exit: {}", s)),
        }
    }
}

impl Default for OnForceClose {
    fn default() -> Self {
        Self::Detach
    }
}

impl FromStr for OnForceClose {
    type Err = Box<dyn std::error::Error>;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "quit" => Ok(Self::Quit),
            "detach" => Ok(Self::Detach),
            e => Err(e.to_string().into()),
        }
    }
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PaneFrameStyle {
    Full,
    Titles,
    None,
    // fork addition: `titles` with a horizontal rule behind the title and no
    // box-drawing separators between panes
    #[serde(rename = "top_only")]
    #[value(name = "top_only")]
    TopOnly,
}

impl Default for PaneFrameStyle {
    fn default() -> Self {
        PaneFrameStyle::Titles
    }
}

impl PaneFrameStyle {
    pub fn draws_full_frames(&self) -> bool {
        matches!(self, PaneFrameStyle::Full)
    }

    pub fn draws_titles(&self) -> bool {
        matches!(self, PaneFrameStyle::Titles | PaneFrameStyle::TopOnly)
    }

    /// Fork addition: `top_only` is `titles` with a rule behind the title and
    /// no separators between panes.
    pub fn is_top_only(&self) -> bool {
        matches!(self, PaneFrameStyle::TopOnly)
    }

    pub fn from_options(options: &Options) -> Self {
        if options.pane_frames == Some(false) {
            return PaneFrameStyle::None;
        }
        match options.pane_frame_style {
            Some(PaneFrameStyle::Full) => PaneFrameStyle::Full,
            Some(PaneFrameStyle::TopOnly) => PaneFrameStyle::TopOnly,
            _ => PaneFrameStyle::Titles,
        }
    }
}

impl FromStr for PaneFrameStyle {
    type Err = Box<dyn std::error::Error>;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "full" => Ok(PaneFrameStyle::Full),
            "titles" => Ok(PaneFrameStyle::Titles),
            "none" => Ok(PaneFrameStyle::None),
            "top_only" => Ok(PaneFrameStyle::TopOnly),
            e => Err(format!(
                "Unknown pane frame style: '{}' (expected 'full', 'titles', 'none' or 'top_only')",
                e
            )
            .into()),
        }
    }
}

/// Whether a stack shows its titles in a column (vertical) or in a row (horizontal)
#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq, Default, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum StackListOrientation {
    #[default]
    Vertical,
    Horizontal,
}

impl std::fmt::Display for StackListOrientation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackListOrientation::Vertical => write!(f, "vertical"),
            StackListOrientation::Horizontal => write!(f, "horizontal"),
        }
    }
}

impl FromStr for StackListOrientation {
    type Err = Box<dyn std::error::Error>;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "vertical" => Ok(StackListOrientation::Vertical),
            "horizontal" => Ok(StackListOrientation::Horizontal),
            e => Err(format!(
                "Unknown stack list orientation: '{}' (expected 'vertical' or 'horizontal')",
                e
            )
            .into()),
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Deserialize, Serialize, Args)]
/// Options that can be set either through the config file,
/// or cli flags - cli flags should take precedence over the config file
/// TODO: In order to correctly parse boolean flags, this is currently split
/// into Options and CliOptions, this could be a good candidate for a macro
pub struct Options {
    /// Allow plugins to use a more simplified layout
    /// that is compatible with more fonts (true or false)
    #[clap(long, value_parser)]
    #[serde(default)]
    pub simplified_ui: Option<bool>,
    /// Set the default theme
    #[clap(long, value_parser)]
    pub theme: Option<String>,
    /// Theme name to apply when the host terminal reports a dark color palette
    /// (CSI 2031 / DSR 997). Requires `theme_light` to also be set; if either
    /// is missing the static `theme` remains authoritative.
    #[clap(long, value_parser)]
    pub theme_dark: Option<String>,
    /// Theme name to apply when the host terminal reports a light color palette
    /// (CSI 2031 / DSR 997). Requires `theme_dark` to also be set; if either
    /// is missing the static `theme` remains authoritative.
    #[clap(long, value_parser)]
    pub theme_light: Option<String>,
    /// Pin the session to a dark or light appearance ("dark" or "light"),
    /// resolved before the first render and kept authoritative over ambient
    /// host terminal reports (CSI 2031 / DSR 997). When unset, the session
    /// follows the host terminal.
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    pub explicit_theme_hue: Option<ThemeHue>,
    /// Set the default mode
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    pub default_mode: Option<InputMode>,
    /// Set the default shell
    #[clap(long, value_parser)]
    pub default_shell: Option<PathBuf>,
    /// Set the default cwd
    #[clap(long, value_parser)]
    pub default_cwd: Option<PathBuf>,
    /// Set the default layout
    #[clap(long, value_parser)]
    pub default_layout: Option<PathBuf>,
    /// Set the layout_dir, defaults to
    /// subdirectory of config dir
    #[clap(long, value_parser)]
    pub layout_dir: Option<PathBuf>,
    /// Set the theme_dir, defaults to
    /// subdirectory of config dir
    #[clap(long, value_parser)]
    pub theme_dir: Option<PathBuf>,
    #[clap(
        long,
        value_parser,
        help = "Set the keybinds_dir, defaults to subdirectory of config dir"
    )]
    #[serde(default)]
    pub keybinds_dir: Option<PathBuf>,
    #[clap(
        long,
        value_parser,
        help = "Use this keybinding preset for this session: a name from the keybinds folder, a built-in preset, or a path to a preset file"
    )]
    #[serde(default)]
    pub keybinds_preset: Option<String>,
    #[clap(
        long,
        value_parser,
        help = "The primary leader key for the keybinding preset (for example \"Ctrl\")"
    )]
    #[serde(default)]
    pub keybinds_primary: Option<String>,
    #[clap(
        long,
        value_parser,
        help = "The secondary leader key for the keybinding preset (for example \"Alt\")"
    )]
    #[serde(default)]
    pub keybinds_secondary: Option<String>,
    #[clap(
        long,
        value_parser,
        help = "The unlock key for the keybinding preset (for example \"Ctrl g\")"
    )]
    #[serde(default)]
    pub keybinds_unlock: Option<String>,
    #[clap(long, value_parser)]
    #[serde(default)]
    /// Set the handling of mouse events (true or false)
    /// Can be temporarily bypassed by the [SHIFT] key
    pub mouse_mode: Option<bool>,
    #[clap(long, value_parser)]
    #[serde(default)]
    /// Set display of the pane frames (true or false)
    pub pane_frames: Option<bool>,
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    #[serde(default)]
    pub pane_frame_style: Option<PaneFrameStyle>,
    #[clap(long, value_parser)]
    #[serde(default)]
    /// Mirror session when multiple users are connected (true or false)
    pub mirror_session: Option<bool>,
    /// Set behaviour on force close (quit or detach)
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    pub on_force_close: Option<OnForceClose>,
    #[clap(long, value_parser)]
    pub scroll_buffer_size: Option<usize>,

    /// Switch to using a user supplied command for clipboard instead of OSC52
    #[clap(long, value_parser)]
    #[serde(default)]
    pub copy_command: Option<String>,

    /// OSC52 destination clipboard
    #[clap(
        long,
        value_enum,
        ignore_case = true,
        conflicts_with = "copy_command",
        value_parser
    )]
    #[serde(default)]
    pub copy_clipboard: Option<Clipboard>,

    /// Automatically copy when selecting text (true or false)
    #[clap(long, value_parser)]
    #[serde(default)]
    pub copy_on_select: Option<bool>,

    /// Enable OSC8 hyperlink output (true or false)
    #[clap(long, value_parser)]
    #[serde(default)]
    pub osc8_hyperlinks: Option<bool>,

    /// Explicit full path to open the scrollback editor (default is $EDITOR or $VISUAL)
    #[clap(long, value_parser)]
    pub scrollback_editor: Option<PathBuf>,

    /// The name of the session to create when starting Zellij
    #[clap(long, value_parser)]
    #[serde(default)]
    pub session_name: Option<String>,

    /// Whether to attach to a session specified in "session-name" if it exists
    #[clap(long, value_parser)]
    #[serde(default)]
    pub attach_to_session: Option<bool>,

    /// Whether to lay out panes in a predefined set of layouts whenever possible
    #[clap(long, value_parser)]
    #[serde(default)]
    pub auto_layout: Option<bool>,

    /// Whether sessions should be serialized to the HD so that they can be later resurrected,
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub session_serialization: Option<bool>,

    /// Whether pane viewports are serialized along with the session, default is false
    #[clap(long, value_parser)]
    #[serde(default)]
    pub serialize_pane_viewport: Option<bool>,

    /// Scrollback lines to serialize along with the pane viewport when serializing sessions, 0
    /// defaults to the scrollback size. If this number is higher than the scrollback size, it will
    /// also default to the scrollback size
    #[clap(long, value_parser)]
    #[serde(default)]
    pub scrollback_lines_to_serialize: Option<usize>,

    /// How long a pane that rang the bell has to stay focused before its notification is
    /// cleared, in milliseconds. Default is 0 - the notification clears the moment the pane is
    /// focused.
    /// config.kdl only - it is read by the server, so there is nothing for a CLI flag to affect.
    #[clap(skip)]
    #[serde(default)]
    pub bell_clear_delay_ms: Option<u64>,

    /// The template of the terminal title (OSC 0) of the focused pane. Knows the {host},
    /// {session} and {pane} placeholders, anything else is literal text. Placeholders that come
    /// out empty take the literal text around them with them, so that no dangling separator is
    /// left behind. Default is "{session} | {pane}".
    /// config.kdl only - it is read by the server at session start, so there is nothing for a CLI
    /// flag to affect.
    #[clap(skip)]
    #[serde(default)]
    pub terminal_title_template: Option<String>,

    /// What to render instead of the session name in the {session} placeholder of
    /// terminal_title_template, keyed by session name. Session names that are not in here render
    /// as they are.
    /// config.kdl only, like terminal_title_template.
    #[clap(skip)]
    #[serde(default)]
    pub session_aliases: Option<BTreeMap<String, String>>,

    /// How to record the command of a pane running a tool that keeps its own session state, so
    /// that a resurrected pane offers to RESUME that state instead of starting over. A hint names
    /// a command, an environment variable the tool exports, and what to record when the variable
    /// is found in the pane's processes. Unset means record every command as it is - which is what
    /// zellij has always done, and what a pane whose hint does not apply still gets.
    /// config.kdl only - it is read by the server when it serializes a session.
    #[clap(skip)]
    #[serde(default)]
    pub resurrect_command_hints: Option<ResurrectCommandHints>,

    /// Which recorded commands a restored session offers, and how: "all" (default) offers every
    /// command as a pane waiting to run; "agents" offers only coding agents - `claude`, `codex`,
    /// `opencode`, `pi`, or a command a `resurrect_command_hints` entry names - with how to resume
    /// them; "prefill" offers the same agents as a shell with the resume command typed at the
    /// prompt, not run. In "agents" and "prefill" every other pane comes back as a plain shell.
    /// config.kdl only - it is read by the server when it serializes and restores a session.
    #[clap(skip)]
    #[serde(default)]
    pub resurrect_commands: Option<ResurrectCommands>,

    /// The size floating panes get when nothing asks for a specific one.
    /// config.kdl only - it is a nested block, which no CLI flag can express.
    #[clap(skip)]
    #[serde(default)]
    pub default_floating_size: Option<DefaultFloatingSize>,

    /// Whether to use ANSI styled underlines
    #[clap(long, value_parser)]
    #[serde(default)]
    pub styled_underlines: Option<bool>,

    /// The interval at which to serialize sessions for resurrection (in seconds)
    #[clap(long, value_parser)]
    pub serialization_interval: Option<u64>,

    /// If true, will disable writing session metadata to disk
    #[clap(long, value_parser)]
    pub disable_session_metadata: Option<bool>,

    /// Whether to enable support for the Kitty keyboard protocol (must also be supported by the
    /// host terminal), defaults to true if the terminal supports it
    #[clap(long, value_parser)]
    #[serde(default)]
    pub support_kitty_keyboard_protocol: Option<bool>,

    /// Whether to enable support for the Kitty graphics (image) protocol (must also be supported
    /// by the host terminal), defaults to true if the terminal supports it
    #[clap(long, value_parser)]
    #[serde(default)]
    pub support_kitty_graphics_protocol: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Count each grapheme cluster (an emoji sequence, a letter with its accents) as one \
                unit of width, as the Unicode standard does (true or false, default true). When \
                false, widths are counted per code point and programs may still ask for clusters \
                with mode 2027"
    )]
    #[serde(default)]
    pub grapheme_clusters: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Whether a focused mirror of a pane may resize that pane's program to the \
                mirror's size, and whether new mirrors resize their source pane by default \
                (true or false, default true)"
    )]
    #[serde(default)]
    pub mirror_resizes_source_pane: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "An extra key that gives typing back to a plugin while a live pane view inside it \
                is taking the keyboard (default: none, Alt Enter, the inbox key or a click outside \
                always do it; a key written twice means pressing it twice)"
    )]
    #[serde(default)]
    pub live_view_release_key: Option<String>,

    /// Whether to make sure a local web server is running when a new Zellij session starts.
    /// This web server will allow creating new sessions and attaching to existing ones that have
    /// opted in to being shared in the browser.
    ///
    /// Note: a local web server can still be manually started from within a Zellij session or from the CLI.
    /// If this is not desired, one can use a version of Zellij compiled without
    /// web_server_capability
    ///
    /// Possible values:
    /// - true
    /// - false
    /// Default: false
    #[clap(long, value_parser)]
    #[serde(default)]
    pub web_server: Option<bool>,

    /// Whether to allow new sessions to be shared through a local web server, assuming one is
    /// running (see the `web_server` option for more details).
    ///
    /// Note: if Zellij was compiled without web_server_capability, this option will be locked to
    /// "disabled"
    ///
    /// Possible values:
    /// - "on" (new sessions will allow web sharing through the local web server if it
    /// is online)
    /// - "off" (new sessions will not allow web sharing unless they explicitly opt-in to it)
    /// - "disabled" (new sessions will not allow web sharing and will not be able to opt-in to it)
    /// Default: "off"
    #[clap(long, value_parser)]
    #[serde(default)]
    pub web_sharing: Option<WebSharing>,

    /// Whether to stack panes when resizing beyond a certain size
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub stacked_resize: Option<bool>,

    #[clap(long, value_parser)]
    #[serde(default)]
    pub stacked_pane_list: Option<bool>,

    /// Whether new stacks show their titles in a column (vertical) or in a row (horizontal)
    /// default is vertical
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    #[serde(default)]
    pub stacked_pane_list_orientation: Option<StackListOrientation>,

    /// Whether to show startup tips when starting a new session
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub show_startup_tips: Option<bool>,

    /// Whether to show release notes on first run of a new version
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub show_release_notes: Option<bool>,

    /// Whether to enable mouse hover effects and pane grouping functionality
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub advanced_mouse_actions: Option<bool>,

    /// Whether Ctrl+ScrollWheel resizes panes
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub mouse_scroll_resize: Option<bool>,

    /// Whether scrolling a pane implicitly enters (and leaving the scroll implicitly exits) Scroll mode
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub scroll_mode_sync: Option<bool>,

    /// Whether to enable mouse hover visual effects (frame highlight and help text)
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub mouse_hover_effects: Option<bool>,

    /// Whether to show mouse hover help-text tips (resize help and group shortcuts)
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub mouse_hover_tips: Option<bool>,

    /// Whether to show visual bell indicators (pane/tab frame flash and [!] suffix)
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub visual_bell: Option<bool>,

    /// Whether to focus panes on mouse hover (true or false)
    /// default is false
    #[clap(long, value_parser)]
    #[serde(default)]
    pub focus_follows_mouse: Option<bool>,

    /// Whether clicking a pane to focus it also sends the click into the pane (true or false)
    /// default is false
    #[clap(long, value_parser)]
    #[serde(default)]
    pub mouse_click_through: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Whether a right click opens the right-click menu (true or false), default is true"
    )]
    #[serde(default)]
    pub context_menu_enabled: Option<bool>,

    /// Whether triple-clicking inside shell-marked (OSC 133) command output selects the command
    /// and its output rather than the logical line
    /// default is true
    #[clap(long, value_parser)]
    #[serde(default)]
    pub osc133_command_selection: Option<bool>,

    /// Characters that terminate a word when double-clicking to select it, in addition to
    /// whitespace (which is always a separator)
    /// default is "[]{}<>()"
    #[clap(long, value_parser)]
    #[serde(default)]
    pub word_separators: Option<String>,

    #[clap(long, value_parser)]
    #[serde(default)]
    pub host_notification_protocol: Option<HostNotificationProtocol>,

    // these are intentionally excluded from the CLI options as they must be specified in the
    // configuration file
    pub web_server_ip: Option<IpAddr>,
    pub web_server_port: Option<u16>,
    pub web_server_cert: Option<PathBuf>,
    pub web_server_key: Option<PathBuf>,
    pub enforce_https_for_localhost: Option<bool>,
    pub dangerously_allow_web_serving_without_a_certificate: Option<bool>,
    /// A command to run after the discovery of running commands when serializing, for the purpose
    /// of manipulating the command (eg. with a regex) before it gets serialized
    #[clap(long, value_parser)]
    pub post_command_discovery_hook: Option<String>,

    /// Number of background threads used by `zellij attach <url>` when it connects to a session
    /// on another machine. One set is made for that program, not one per client; the web server,
    /// browser clients and local sessions are not affected.
    ///
    /// Most of the work runs on the main thread, so these threads are usually idle. Set to 0 to
    /// use the number of (physical) CPU cores.
    #[clap(long)]
    pub client_async_worker_tasks: Option<usize>,

    /// How to handle a nested Zellij session detected inside a pane
    /// (ask, fullscreen, descend, never)
    #[clap(long, value_enum, hide_possible_values = true, value_parser)]
    #[serde(default)]
    pub nested_session_handling: Option<NestedSessionHandling>,

    #[clap(long, value_parser)]
    #[serde(default)]
    pub dangerously_enable_paste_buffer_read: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Whether to show the other sessions indicator in the tab bar and compact bar (true or false), default is true"
    )]
    #[serde(default)]
    pub session_indicator: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Whether to briefly show the new swap layout name when it is changed (true or false), default is true"
    )]
    #[serde(default)]
    pub swap_layout_notification: Option<bool>,

    #[clap(
        long,
        value_enum,
        help = "What the quit action does: ask_quit (ask, Quit preselected), ask_detach (ask, Detach preselected), quit or detach. Default is ask_quit"
    )]
    #[serde(default)]
    pub on_quit: Option<OnQuit>,

    #[clap(
        long,
        value_parser,
        help = "Which bars a new session starts with hidden: bar names separated by commas (eg. tab-bar,status-bar), all or none. Default is none"
    )]
    #[serde(default)]
    pub bars_hidden_on_start: Option<HiddenBars>,

    #[clap(
        long,
        value_parser,
        help = "Whether to briefly show what was hidden and how to show it again when bars are hidden (true or false), default is true"
    )]
    #[serde(default)]
    pub hidden_bars_notification: Option<bool>,

    #[clap(
        long,
        value_parser,
        help = "Whether to show the current mode's keys in a corner while a bar that would show them is hidden (true or false), default is true"
    )]
    #[serde(default)]
    pub mode_hints_when_bars_hidden: Option<bool>,
    /// What a keypress does in a pane that is scrolled up
    /// (scroll-mode, jump, stay)
    #[clap(skip)]
    #[serde(default)]
    pub input_while_scrolled: Option<InputWhileScrolled>,

    /// What a command pane does when its command exits with status 0
    /// (hold, shell)
    #[clap(skip)]
    #[serde(default)]
    pub command_pane_on_clean_exit: Option<CommandPaneOnCleanExit>,
}

#[derive(ValueEnum, Deserialize, Serialize, Debug, Clone, Copy, PartialEq)]
pub enum Clipboard {
    #[serde(alias = "system")]
    System,
    #[serde(alias = "primary")]
    Primary,
}

impl Default for Clipboard {
    fn default() -> Self {
        Self::System
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ValueEnum)]
pub enum HostNotificationProtocol {
    #[serde(alias = "auto")]
    Auto,
    #[serde(alias = "osc9")]
    Osc9,
    #[serde(alias = "osc99")]
    Osc99,
    #[serde(alias = "bell")]
    Bell,
    #[serde(alias = "off")]
    Off,
}

impl Default for HostNotificationProtocol {
    fn default() -> Self {
        Self::Auto
    }
}

impl HostNotificationProtocol {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Osc9 => "osc9",
            Self::Osc99 => "osc99",
            Self::Bell => "bell",
            Self::Off => "off",
        }
    }
}

impl FromStr for HostNotificationProtocol {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Auto" | "auto" => Ok(Self::Auto),
            "Osc9" | "osc9" => Ok(Self::Osc9),
            "Osc99" | "osc99" => Ok(Self::Osc99),
            "Bell" | "bell" => Ok(Self::Bell),
            "Off" | "off" => Ok(Self::Off),
            _ => Err(format!("No such host_notification_protocol: {}", s)),
        }
    }
}

impl FromStr for Clipboard {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "System" | "system" => Ok(Self::System),
            "Primary" | "primary" => Ok(Self::Primary),
            _ => Err(format!("No such clipboard: {}", s)),
        }
    }
}

impl Options {
    pub fn from_yaml(from_yaml: Option<Options>) -> Options {
        if let Some(opts) = from_yaml {
            opts
        } else {
            Options::default()
        }
    }
    /// Merges two [`Options`] structs, a `Some` in `other`
    /// will supersede a `Some` in `self`
    // TODO: Maybe a good candidate for a macro?
    pub fn merge(&self, other: Options) -> Options {
        let mouse_mode = other.mouse_mode.or(self.mouse_mode);
        let pane_frames = other.pane_frames.or(self.pane_frames);
        let pane_frame_style = other.pane_frame_style.or(self.pane_frame_style);
        let auto_layout = other.auto_layout.or(self.auto_layout);
        let mirror_session = other.mirror_session.or(self.mirror_session);
        let simplified_ui = other.simplified_ui.or(self.simplified_ui);
        let default_mode = other.default_mode.or(self.default_mode);
        let default_shell = other.default_shell.or_else(|| self.default_shell.clone());
        let default_cwd = other.default_cwd.or_else(|| self.default_cwd.clone());
        let default_layout = other.default_layout.or_else(|| self.default_layout.clone());
        let layout_dir = other.layout_dir.or_else(|| self.layout_dir.clone());
        let theme_dir = other.theme_dir.or_else(|| self.theme_dir.clone());
        let keybinds_dir = other.keybinds_dir.or_else(|| self.keybinds_dir.clone());
        let keybinds_preset = other
            .keybinds_preset
            .or_else(|| self.keybinds_preset.clone());
        let keybinds_primary = other
            .keybinds_primary
            .or_else(|| self.keybinds_primary.clone());
        let keybinds_secondary = other
            .keybinds_secondary
            .or_else(|| self.keybinds_secondary.clone());
        let keybinds_unlock = other
            .keybinds_unlock
            .or_else(|| self.keybinds_unlock.clone());
        let theme = other.theme.or_else(|| self.theme.clone());
        let theme_dark = other.theme_dark.or_else(|| self.theme_dark.clone());
        let theme_light = other.theme_light.or_else(|| self.theme_light.clone());
        let explicit_theme_hue = other.explicit_theme_hue.or(self.explicit_theme_hue);
        let on_force_close = other.on_force_close.or(self.on_force_close);
        let scroll_buffer_size = other.scroll_buffer_size.or(self.scroll_buffer_size);
        let copy_command = other.copy_command.or_else(|| self.copy_command.clone());
        let copy_clipboard = other.copy_clipboard.or(self.copy_clipboard);
        let copy_on_select = other.copy_on_select.or(self.copy_on_select);
        let osc8_hyperlinks = other.osc8_hyperlinks.or(self.osc8_hyperlinks);
        let scrollback_editor = other
            .scrollback_editor
            .or_else(|| self.scrollback_editor.clone());
        let session_name = other.session_name.or_else(|| self.session_name.clone());
        let attach_to_session = other
            .attach_to_session
            .or_else(|| self.attach_to_session.clone());
        let session_serialization = other.session_serialization.or(self.session_serialization);
        let serialize_pane_viewport = other
            .serialize_pane_viewport
            .or(self.serialize_pane_viewport);
        let scrollback_lines_to_serialize = other
            .scrollback_lines_to_serialize
            .or(self.scrollback_lines_to_serialize);
        let snapshot_dir = other.snapshot_dir.or_else(|| self.snapshot_dir.clone());
        let session_snapshot_limit = other.session_snapshot_limit.or(self.session_snapshot_limit);
        let bell_clear_delay_ms = other.bell_clear_delay_ms.or(self.bell_clear_delay_ms);
        let terminal_title_template = other
            .terminal_title_template
            .or_else(|| self.terminal_title_template.clone());
        let session_aliases = other
            .session_aliases
            .or_else(|| self.session_aliases.clone());
        let session_restart_drop_env = other
            .session_restart_drop_env
            .or_else(|| self.session_restart_drop_env.clone());
        let session_service = other
            .session_service
            .or_else(|| self.session_service.clone());
        let resurrect_command_hints = other
            .resurrect_command_hints
            .or_else(|| self.resurrect_command_hints.clone());
        let resurrect_commands = other.resurrect_commands.or(self.resurrect_commands);
        let detect_agents = other.detect_agents.or(self.detect_agents);
        let pane_frame_agent_name = other.pane_frame_agent_name.or(self.pane_frame_agent_name);
        let session_up_resume = other.session_up_resume.or(self.session_up_resume);
        let default_floating_size = other
            .default_floating_size
            .clone()
            .or_else(|| self.default_floating_size.clone());
        let styled_underlines = other.styled_underlines.or(self.styled_underlines);
        let serialization_interval = other.serialization_interval.or(self.serialization_interval);
        let disable_session_metadata = other
            .disable_session_metadata
            .or(self.disable_session_metadata);
        let support_kitty_keyboard_protocol = other
            .support_kitty_keyboard_protocol
            .or(self.support_kitty_keyboard_protocol);
        let support_kitty_graphics_protocol = other
            .support_kitty_graphics_protocol
            .or(self.support_kitty_graphics_protocol);
        let grapheme_clusters = other.grapheme_clusters.or(self.grapheme_clusters);
        let mirror_resizes_source_pane = other
            .mirror_resizes_source_pane
            .or(self.mirror_resizes_source_pane);
        let live_view_release_key = other
            .live_view_release_key
            .or_else(|| self.live_view_release_key.clone());
        let web_server = other.web_server.or(self.web_server);
        let web_sharing = other.web_sharing.or(self.web_sharing);
        let stacked_resize = other.stacked_resize.or(self.stacked_resize);
        let stacked_pane_list = other.stacked_pane_list.or(self.stacked_pane_list);
        let stacked_pane_list_orientation = other
            .stacked_pane_list_orientation
            .or(self.stacked_pane_list_orientation);
        let show_startup_tips = other.show_startup_tips.or(self.show_startup_tips);
        let show_release_notes = other.show_release_notes.or(self.show_release_notes);
        let advanced_mouse_actions = other.advanced_mouse_actions.or(self.advanced_mouse_actions);
        let mouse_scroll_resize = other.mouse_scroll_resize.or(self.mouse_scroll_resize);
        let scroll_mode_sync = other.scroll_mode_sync.or(self.scroll_mode_sync);
        let mouse_hover_effects = other.mouse_hover_effects.or(self.mouse_hover_effects);
        let mouse_hover_tips = other.mouse_hover_tips.or(self.mouse_hover_tips);
        let visual_bell = other.visual_bell.or(self.visual_bell);
        let focus_follows_mouse = other.focus_follows_mouse.or(self.focus_follows_mouse);
        let mouse_click_through = other.mouse_click_through.or(self.mouse_click_through);
        let context_menu_enabled = other.context_menu_enabled.or(self.context_menu_enabled);
        let osc133_command_selection = other
            .osc133_command_selection
            .or(self.osc133_command_selection);
        let word_separators = other
            .word_separators
            .or_else(|| self.word_separators.clone());
        let host_notification_protocol = other
            .host_notification_protocol
            .or(self.host_notification_protocol);
        let web_server_ip = other.web_server_ip.or(self.web_server_ip);
        let web_server_port = other.web_server_port.or(self.web_server_port);
        let web_server_cert = other
            .web_server_cert
            .or_else(|| self.web_server_cert.clone());
        let web_server_key = other.web_server_key.or_else(|| self.web_server_key.clone());
        let enforce_https_for_localhost = other
            .enforce_https_for_localhost
            .or(self.enforce_https_for_localhost);
        let dangerously_allow_web_serving_without_a_certificate = other
            .dangerously_allow_web_serving_without_a_certificate
            .or(self.dangerously_allow_web_serving_without_a_certificate);
        let post_command_discovery_hook = other
            .post_command_discovery_hook
            .or(self.post_command_discovery_hook.clone());
        let client_async_worker_tasks = other
            .client_async_worker_tasks
            .or(self.client_async_worker_tasks);
        let nested_session_handling = other
            .nested_session_handling
            .or(self.nested_session_handling);
        let dangerously_enable_paste_buffer_read = other
            .dangerously_enable_paste_buffer_read
            .or(self.dangerously_enable_paste_buffer_read);
        let session_indicator = other.session_indicator.or(self.session_indicator);
        let swap_layout_notification = other
            .swap_layout_notification
            .or(self.swap_layout_notification);
        let on_quit = other.on_quit.or(self.on_quit);
        let bars_hidden_on_start = other
            .bars_hidden_on_start
            .or(self.bars_hidden_on_start.clone());
        let hidden_bars_notification = other
            .hidden_bars_notification
            .or(self.hidden_bars_notification);
        let mode_hints_when_bars_hidden = other
            .mode_hints_when_bars_hidden
            .or(self.mode_hints_when_bars_hidden);
        let input_while_scrolled = other.input_while_scrolled.or(self.input_while_scrolled);
        let command_pane_on_clean_exit = other
            .command_pane_on_clean_exit
            .or(self.command_pane_on_clean_exit);

        Options {
            stacked_pane_list_orientation,
            simplified_ui,
            theme,
            theme_dark,
            theme_light,
            explicit_theme_hue,
            default_mode,
            default_shell,
            default_cwd,
            default_layout,
            layout_dir,
            theme_dir,
            keybinds_dir,
            keybinds_preset,
            keybinds_primary,
            keybinds_secondary,
            keybinds_unlock,
            mouse_mode,
            pane_frames,
            pane_frame_style,
            mirror_session,
            on_force_close,
            scroll_buffer_size,
            copy_command,
            copy_clipboard,
            copy_on_select,
            osc8_hyperlinks,
            scrollback_editor,
            session_name,
            attach_to_session,
            auto_layout,
            session_serialization,
            serialize_pane_viewport,
            scrollback_lines_to_serialize,
            styled_underlines,
            serialization_interval,
            disable_session_metadata,
            support_kitty_keyboard_protocol,
            support_kitty_graphics_protocol,
            grapheme_clusters,
            mirror_resizes_source_pane,
            live_view_release_key,
            web_server,
            web_sharing,
            stacked_resize,
            stacked_pane_list,
            show_startup_tips,
            show_release_notes,
            advanced_mouse_actions,
            mouse_scroll_resize,
            scroll_mode_sync,
            mouse_hover_effects,
            mouse_hover_tips,
            visual_bell,
            focus_follows_mouse,
            mouse_click_through,
            context_menu_enabled,
            osc133_command_selection,
            word_separators,
            host_notification_protocol,
            web_server_ip,
            web_server_port,
            web_server_cert,
            web_server_key,
            enforce_https_for_localhost,
            dangerously_allow_web_serving_without_a_certificate,
            post_command_discovery_hook,
            client_async_worker_tasks,
            nested_session_handling,
            dangerously_enable_paste_buffer_read,
            session_indicator,
            swap_layout_notification,
            on_quit,
            bars_hidden_on_start,
            hidden_bars_notification,
            mode_hints_when_bars_hidden,
            input_while_scrolled,
            command_pane_on_clean_exit,
        }
    }

    /// Merges two [`Options`] structs,
    /// - `Some` in `other` will supersede a `Some` in `self`
    /// - `Some(bool)` in `other` will toggle a `Some(bool)` in `self`
    // TODO: Maybe a good candidate for a macro?
    pub fn merge_from_cli(&self, other: Options) -> Options {
        let merge_bool = |opt_other: Option<bool>, opt_self: Option<bool>| {
            if opt_other.is_some() ^ opt_self.is_some() {
                opt_other.or(opt_self)
            } else if opt_other.is_some() && opt_self.is_some() {
                Some(opt_other.unwrap() ^ opt_self.unwrap())
            } else {
                None
            }
        };

        let simplified_ui = merge_bool(other.simplified_ui, self.simplified_ui);
        let mouse_mode = merge_bool(other.mouse_mode, self.mouse_mode);
        let pane_frames = merge_bool(other.pane_frames, self.pane_frames);
        let pane_frame_style = other.pane_frame_style.or(self.pane_frame_style);
        let auto_layout = merge_bool(other.auto_layout, self.auto_layout);
        let mirror_session = merge_bool(other.mirror_session, self.mirror_session);
        let session_serialization =
            merge_bool(other.session_serialization, self.session_serialization);
        let serialize_pane_viewport =
            merge_bool(other.serialize_pane_viewport, self.serialize_pane_viewport);

        let default_mode = other.default_mode.or(self.default_mode);
        let default_shell = other.default_shell.or_else(|| self.default_shell.clone());
        let default_cwd = other.default_cwd.or_else(|| self.default_cwd.clone());
        let default_layout = other.default_layout.or_else(|| self.default_layout.clone());
        let layout_dir = other.layout_dir.or_else(|| self.layout_dir.clone());
        let theme_dir = other.theme_dir.or_else(|| self.theme_dir.clone());
        let keybinds_dir = other.keybinds_dir.or_else(|| self.keybinds_dir.clone());
        let keybinds_preset = other
            .keybinds_preset
            .or_else(|| self.keybinds_preset.clone());
        let keybinds_primary = other
            .keybinds_primary
            .or_else(|| self.keybinds_primary.clone());
        let keybinds_secondary = other
            .keybinds_secondary
            .or_else(|| self.keybinds_secondary.clone());
        let keybinds_unlock = other
            .keybinds_unlock
            .or_else(|| self.keybinds_unlock.clone());
        let theme = other.theme.or_else(|| self.theme.clone());
        let theme_dark = other.theme_dark.or_else(|| self.theme_dark.clone());
        let theme_light = other.theme_light.or_else(|| self.theme_light.clone());
        let explicit_theme_hue = other.explicit_theme_hue.or(self.explicit_theme_hue);
        let on_force_close = other.on_force_close.or(self.on_force_close);
        let scroll_buffer_size = other.scroll_buffer_size.or(self.scroll_buffer_size);
        let copy_command = other.copy_command.or_else(|| self.copy_command.clone());
        let copy_clipboard = other.copy_clipboard.or(self.copy_clipboard);
        let copy_on_select = other.copy_on_select.or(self.copy_on_select);
        let osc8_hyperlinks = other.osc8_hyperlinks.or(self.osc8_hyperlinks);
        let scrollback_editor = other
            .scrollback_editor
            .or_else(|| self.scrollback_editor.clone());
        let session_name = other.session_name.or_else(|| self.session_name.clone());
        let attach_to_session = other
            .attach_to_session
            .or_else(|| self.attach_to_session.clone());
        let scrollback_lines_to_serialize = other
            .scrollback_lines_to_serialize
            .or_else(|| self.scrollback_lines_to_serialize.clone());
        let snapshot_dir = other.snapshot_dir.or_else(|| self.snapshot_dir.clone());
        let session_snapshot_limit = other
            .session_snapshot_limit
            .or_else(|| self.session_snapshot_limit.clone());
        let bell_clear_delay_ms = other.bell_clear_delay_ms.or(self.bell_clear_delay_ms);
        let terminal_title_template = other
            .terminal_title_template
            .or_else(|| self.terminal_title_template.clone());
        let session_aliases = other
            .session_aliases
            .or_else(|| self.session_aliases.clone());
        let session_restart_drop_env = other
            .session_restart_drop_env
            .or_else(|| self.session_restart_drop_env.clone());
        let session_service = other
            .session_service
            .or_else(|| self.session_service.clone());
        let resurrect_command_hints = other
            .resurrect_command_hints
            .or_else(|| self.resurrect_command_hints.clone());
        let resurrect_commands = other.resurrect_commands.or(self.resurrect_commands);
        let detect_agents = other.detect_agents.or(self.detect_agents);
        let pane_frame_agent_name = other.pane_frame_agent_name.or(self.pane_frame_agent_name);
        let session_up_resume = other.session_up_resume.or(self.session_up_resume);
        let default_floating_size = other
            .default_floating_size
            .clone()
            .or_else(|| self.default_floating_size.clone());
        let styled_underlines = other.styled_underlines.or(self.styled_underlines);
        let serialization_interval = other.serialization_interval.or(self.serialization_interval);
        let disable_session_metadata = other
            .disable_session_metadata
            .or(self.disable_session_metadata);
        let support_kitty_keyboard_protocol = other
            .support_kitty_keyboard_protocol
            .or(self.support_kitty_keyboard_protocol);
        let support_kitty_graphics_protocol = other
            .support_kitty_graphics_protocol
            .or(self.support_kitty_graphics_protocol);
        let grapheme_clusters = other.grapheme_clusters.or(self.grapheme_clusters);
        let mirror_resizes_source_pane = other
            .mirror_resizes_source_pane
            .or(self.mirror_resizes_source_pane);
        let live_view_release_key = other
            .live_view_release_key
            .or_else(|| self.live_view_release_key.clone());
        let web_server = other.web_server.or(self.web_server);
        let web_sharing = other.web_sharing.or(self.web_sharing);
        let stacked_resize = other.stacked_resize.or(self.stacked_resize);
        let stacked_pane_list = other.stacked_pane_list.or(self.stacked_pane_list);
        let stacked_pane_list_orientation = other
            .stacked_pane_list_orientation
            .or(self.stacked_pane_list_orientation);
        let show_startup_tips = other.show_startup_tips.or(self.show_startup_tips);
        let show_release_notes = other.show_release_notes.or(self.show_release_notes);
        let advanced_mouse_actions = other.advanced_mouse_actions.or(self.advanced_mouse_actions);
        let mouse_scroll_resize = other.mouse_scroll_resize.or(self.mouse_scroll_resize);
        let scroll_mode_sync = other.scroll_mode_sync.or(self.scroll_mode_sync);
        let mouse_hover_effects = other.mouse_hover_effects.or(self.mouse_hover_effects);
        let mouse_hover_tips = other.mouse_hover_tips.or(self.mouse_hover_tips);
        let visual_bell = other.visual_bell.or(self.visual_bell);
        let focus_follows_mouse = merge_bool(other.focus_follows_mouse, self.focus_follows_mouse);
        let mouse_click_through = merge_bool(other.mouse_click_through, self.mouse_click_through);
        let context_menu_enabled = other.context_menu_enabled.or(self.context_menu_enabled);
        let osc133_command_selection = other
            .osc133_command_selection
            .or(self.osc133_command_selection);
        let word_separators = other
            .word_separators
            .or_else(|| self.word_separators.clone());
        let host_notification_protocol = other
            .host_notification_protocol
            .or(self.host_notification_protocol);
        let web_server_ip = other.web_server_ip.or(self.web_server_ip);
        let web_server_port = other.web_server_port.or(self.web_server_port);
        let web_server_cert = other
            .web_server_cert
            .or_else(|| self.web_server_cert.clone());
        let web_server_key = other.web_server_key.or_else(|| self.web_server_key.clone());
        let enforce_https_for_localhost = other
            .enforce_https_for_localhost
            .or(self.enforce_https_for_localhost);
        let dangerously_allow_web_serving_without_a_certificate = other
            .dangerously_allow_web_serving_without_a_certificate
            .or(self.dangerously_allow_web_serving_without_a_certificate);
        let post_command_discovery_hook = other
            .post_command_discovery_hook
            .or_else(|| self.post_command_discovery_hook.clone());
        let client_async_worker_tasks = other
            .client_async_worker_tasks
            .or(self.client_async_worker_tasks);
        let nested_session_handling = other
            .nested_session_handling
            .or(self.nested_session_handling);
        let dangerously_enable_paste_buffer_read = other
            .dangerously_enable_paste_buffer_read
            .or(self.dangerously_enable_paste_buffer_read);
        let session_indicator = merge_bool(other.session_indicator, self.session_indicator);
        let swap_layout_notification = merge_bool(
            other.swap_layout_notification,
            self.swap_layout_notification,
        );
        let on_quit = other.on_quit.or(self.on_quit);
        let bars_hidden_on_start = other
            .bars_hidden_on_start
            .or(self.bars_hidden_on_start.clone());
        let hidden_bars_notification = merge_bool(
            other.hidden_bars_notification,
            self.hidden_bars_notification,
        );
        let mode_hints_when_bars_hidden = merge_bool(
            other.mode_hints_when_bars_hidden,
            self.mode_hints_when_bars_hidden,
        );
        let input_while_scrolled = other.input_while_scrolled.or(self.input_while_scrolled);
        let command_pane_on_clean_exit = other
            .command_pane_on_clean_exit
            .or(self.command_pane_on_clean_exit);

        Options {
            stacked_pane_list_orientation,
            simplified_ui,
            theme,
            theme_dark,
            theme_light,
            explicit_theme_hue,
            default_mode,
            default_shell,
            default_cwd,
            default_layout,
            layout_dir,
            theme_dir,
            keybinds_dir,
            keybinds_preset,
            keybinds_primary,
            keybinds_secondary,
            keybinds_unlock,
            mouse_mode,
            pane_frames,
            pane_frame_style,
            mirror_session,
            on_force_close,
            scroll_buffer_size,
            copy_command,
            copy_clipboard,
            copy_on_select,
            osc8_hyperlinks,
            scrollback_editor,
            session_name,
            attach_to_session,
            auto_layout,
            session_serialization,
            serialize_pane_viewport,
            scrollback_lines_to_serialize,
            styled_underlines,
            serialization_interval,
            disable_session_metadata,
            support_kitty_keyboard_protocol,
            support_kitty_graphics_protocol,
            grapheme_clusters,
            mirror_resizes_source_pane,
            live_view_release_key,
            web_server,
            web_sharing,
            stacked_resize,
            stacked_pane_list,
            show_startup_tips,
            show_release_notes,
            advanced_mouse_actions,
            mouse_scroll_resize,
            scroll_mode_sync,
            mouse_hover_effects,
            mouse_hover_tips,
            visual_bell,
            focus_follows_mouse,
            mouse_click_through,
            context_menu_enabled,
            osc133_command_selection,
            word_separators,
            host_notification_protocol,
            web_server_ip,
            web_server_port,
            web_server_cert,
            web_server_key,
            enforce_https_for_localhost,
            dangerously_allow_web_serving_without_a_certificate,
            post_command_discovery_hook,
            client_async_worker_tasks,
            nested_session_handling,
            dangerously_enable_paste_buffer_read,
            session_indicator,
            swap_layout_notification,
            on_quit,
            bars_hidden_on_start,
            hidden_bars_notification,
            mode_hints_when_bars_hidden,
            input_while_scrolled,
            command_pane_on_clean_exit,
        }
    }

    pub fn from_cli(&self, other: Option<Command>) -> Options {
        if let Some(Command::Options(options)) = other {
            Options::merge_from_cli(self, options.into())
        } else {
            self.to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_frame_style_from_str_accepts_all_variants() {
        assert_eq!(
            "full".parse::<PaneFrameStyle>().unwrap(),
            PaneFrameStyle::Full
        );
        assert_eq!(
            "titles".parse::<PaneFrameStyle>().unwrap(),
            PaneFrameStyle::Titles
        );
        assert_eq!(
            "none".parse::<PaneFrameStyle>().unwrap(),
            PaneFrameStyle::None
        );
        assert_eq!(
            "NONE".parse::<PaneFrameStyle>().unwrap(),
            PaneFrameStyle::None
        );
        assert!("bogus".parse::<PaneFrameStyle>().is_err());
    }

    #[test]
    fn host_notification_protocol_from_str_accepts_all_variants() {
        assert_eq!(
            "auto".parse::<HostNotificationProtocol>().unwrap(),
            HostNotificationProtocol::Auto
        );
        assert_eq!(
            "osc9".parse::<HostNotificationProtocol>().unwrap(),
            HostNotificationProtocol::Osc9
        );
        assert_eq!(
            "osc99".parse::<HostNotificationProtocol>().unwrap(),
            HostNotificationProtocol::Osc99
        );
        assert_eq!(
            "bell".parse::<HostNotificationProtocol>().unwrap(),
            HostNotificationProtocol::Bell
        );
        assert_eq!(
            "off".parse::<HostNotificationProtocol>().unwrap(),
            HostNotificationProtocol::Off
        );
        assert!("bogus".parse::<HostNotificationProtocol>().is_err());
    }

    #[test]
    fn every_host_notification_protocol_variant_stringifies_back_to_itself() {
        for variant in [
            HostNotificationProtocol::Auto,
            HostNotificationProtocol::Osc9,
            HostNotificationProtocol::Osc99,
            HostNotificationProtocol::Bell,
            HostNotificationProtocol::Off,
        ] {
            assert_eq!(
                variant
                    .as_str()
                    .parse::<HostNotificationProtocol>()
                    .unwrap(),
                variant
            );
        }
    }

    #[test]
    fn the_host_notification_protocol_defaults_to_auto() {
        assert_eq!(
            HostNotificationProtocol::default(),
            HostNotificationProtocol::Auto
        );
    }

    #[test]
    fn a_configured_host_notification_protocol_is_overridden_by_the_merged_in_one() {
        let config = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Osc9),
            ..Default::default()
        };
        let layout = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Bell),
            ..Default::default()
        };
        assert_eq!(
            config.merge(layout).host_notification_protocol,
            Some(HostNotificationProtocol::Bell)
        );
    }

    #[test]
    fn an_unset_host_notification_protocol_does_not_clobber_the_configured_one() {
        let config = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Osc9),
            ..Default::default()
        };
        assert_eq!(
            config.merge(Options::default()).host_notification_protocol,
            Some(HostNotificationProtocol::Osc9)
        );
    }

    #[test]
    fn a_host_notification_protocol_unset_everywhere_stays_unset() {
        assert_eq!(
            Options::default()
                .merge(Options::default())
                .host_notification_protocol,
            None
        );
        assert_eq!(
            Options::default()
                .merge_from_cli(Options::default())
                .host_notification_protocol,
            None
        );
    }

    #[test]
    fn a_host_notification_protocol_given_on_the_command_line_wins() {
        let config = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Osc9),
            ..Default::default()
        };
        let cli = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Off),
            ..Default::default()
        };
        assert_eq!(
            config.merge_from_cli(cli).host_notification_protocol,
            Some(HostNotificationProtocol::Off)
        );
    }

    #[test]
    fn a_host_notification_protocol_absent_from_the_command_line_keeps_the_configured_one() {
        let config = Options {
            host_notification_protocol: Some(HostNotificationProtocol::Osc99),
            ..Default::default()
        };
        assert_eq!(
            config
                .merge_from_cli(Options::default())
                .host_notification_protocol,
            Some(HostNotificationProtocol::Osc99),
            "the option is carried over verbatim, not toggled like the boolean options are"
        );
    }

    #[test]
    fn an_empty_default_floating_size_changes_nothing() {
        let default_floating_size = DefaultFloatingSize::default();
        assert!(default_floating_size.is_empty());
        assert_eq!(default_floating_size.apply_to(None), None);
        let explicit = FloatingPaneCoordinates::default().with_width_percent(30);
        assert_eq!(
            default_floating_size.apply_to(Some(explicit.clone())),
            Some(explicit)
        );
    }

    #[test]
    fn default_floating_size_fills_in_absent_coordinates() {
        let default_floating_size = DefaultFloatingSize {
            width: Some(PercentOrFixed::Percent(90)),
            height: Some(PercentOrFixed::Percent(80)),
        };
        let filled = default_floating_size.apply_to(None).unwrap();
        assert_eq!(filled.width, Some(PercentOrFixed::Percent(90)));
        assert_eq!(filled.height, Some(PercentOrFixed::Percent(80)));
        assert_eq!(filled.x, None, "position is left to the caller");
        assert_eq!(filled.y, None);
    }

    #[test]
    fn an_explicit_size_wins_over_the_default_axis_by_axis() {
        let default_floating_size = DefaultFloatingSize {
            width: Some(PercentOrFixed::Percent(90)),
            height: Some(PercentOrFixed::Percent(80)),
        };
        let asked_for_width_only = FloatingPaneCoordinates::default().with_width_percent(30);
        let filled = default_floating_size
            .apply_to(Some(asked_for_width_only))
            .unwrap();
        assert_eq!(filled.width, Some(PercentOrFixed::Percent(30)));
        assert_eq!(filled.height, Some(PercentOrFixed::Percent(80)));
    }

    #[test]
    fn a_default_floating_size_recenters_the_pane() {
        use crate::pane_size::{PaneGeom, Viewport};
        let viewport = Viewport {
            x: 0,
            y: 0,
            rows: 50,
            cols: 200,
        };
        // what `find_room_for_new_pane` hands back with no config: half the viewport, centered
        let mut geom = PaneGeom::default();
        geom.x = 50;
        geom.y = 13;
        geom.cols.set_inner(100);
        geom.rows.set_inner(25);

        let default_floating_size = DefaultFloatingSize {
            width: Some(PercentOrFixed::Percent(90)),
            height: Some(PercentOrFixed::Percent(80)),
        };
        geom.adjust_coordinates(default_floating_size.apply_to(None).unwrap(), viewport);

        assert_eq!(geom.cols.as_usize(), 180);
        assert_eq!(geom.rows.as_usize(), 40);
        assert_eq!(geom.x, 10, "still centered horizontally");
        assert_eq!(geom.y, 5, "still centered vertically");
    }
}
