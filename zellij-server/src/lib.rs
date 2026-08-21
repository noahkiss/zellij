#[cfg(not(windows))]
#[path = "os_input_output_unix.rs"]
mod os_input_output_unix;
#[cfg(windows)]
#[path = "os_input_output_windows.rs"]
mod os_input_output_windows;

pub mod host_query;
pub mod os_input_output;
pub mod output;
mod pane_handles;
// fork addition: the integration harness asks for in-order pane handles, so a snapshot of a
// rendered frame is the same on every run
pub use pane_handles::SEQUENTIAL_HANDLES_VAR;
pub mod panes;
pub mod tab;

pub mod background_jobs;
mod bell_dwell;
mod global_async_runtime;
mod logging_pipe;
mod mobile_web;
pub mod nested_guest;
pub mod notifications;
mod pane_groups;
pub mod pane_privacy;
mod plugins;
mod pty;
mod pty_writer;
mod resurrect_hints;
mod route;
mod screen;
mod session_layout_metadata;
mod session_warnings;
mod terminal_bytes;
mod thread_bus;
mod ui;

use background_jobs::{background_jobs_main, BackgroundJob};
use log::info;
use pty_writer::{pty_writer_main, PtyWriteInstruction};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    thread,
};
use zellij_utils::envs;
use zellij_utils::pane_size::Size;

use zellij_utils::input::cli_assets::CliAssets;
use zellij_utils::input::options::{PaneFrameStyle, DEFAULT_WORD_SEPARATORS};

use wasmi::Engine;

use crate::{
    os_input_output::ServerOsApi,
    panes::PaneId,
    plugins::{plugin_thread_main, PluginInstruction},
    pty::{get_default_shell, pty_thread_main, Pty, PtyInstruction},
    screen::{screen_thread_main, ScreenInstruction},
    thread_bus::{Bus, ThreadSenders},
};
use route::{route_thread_main, NotificationEnd};
use zellij_utils::{
    channels::{self, ChannelWithContext, SenderWithContext},
    consts::{
        DEFAULT_SCROLL_BUFFER_SIZE, SCROLL_BUFFER_SIZE, ZELLIJ_SEEN_RELEASE_NOTES_CACHE_FILE,
    },
    data::{
        ConnectToSession, Direction, InputMode, KeyWithModifier, LayoutInfo, LayoutWithError,
        Style, WebSharing,
    },
    errors::{prelude::*, ContextType, ErrorInstruction, FatalError, ServerContext},
    home::{default_layout_dir, get_default_data_dir},
    input::{
        actions::Action,
        command::{RunCommand, TerminalAction},
        config::{watch_config_file_changes, watch_layout_dir_changes, Config},
        keybinds::Keybinds,
        layout::{FloatingPaneLayout, Layout, PluginAlias, Run, RunPluginOrAlias},
        options::Options,
        plugins::PluginAliases,
    },
    ipc::{ClientAttributes, ExitReason, ServerToClientMsg},
    session_service::configured_pinned_exe,
    session_snapshot::{archive_session_info, SnapshotReason, SnapshotSettings},
    shared::{
        default_palette, set_terminal_title_format, web_server_base_url, TerminalTitleFormat,
    },
};

pub type ClientId = u16;

/// Instructions related to server-side application
#[derive(Debug, Clone)]
pub enum ServerInstruction {
    FirstClientConnected(
        CliAssets,
        bool, // is_web_client
        ClientId,
    ),
    Render(Option<HashMap<ClientId, String>>),
    UnblockInputThread,
    ClientExit(ClientId, Option<NotificationEnd>),
    RemoveClient(ClientId),
    Error(String),
    KillSession,
    DetachSession(Vec<ClientId>, Option<NotificationEnd>),
    AttachClient(
        CliAssets,
        Option<usize>,       // tab position to focus
        Option<(u32, bool)>, // (pane_id, is_plugin) => pane_id to focus
        bool,                // is_web_client
        ClientId,
    ),
    AttachWatcherClient(ClientId, Size, bool), // bool -> is_web_client
    ConnStatus(ClientId),
    Log(Vec<String>, ClientId, Option<NotificationEnd>),
    LogError(Vec<String>, ClientId, Option<NotificationEnd>),
    SwitchSession(ConnectToSession, ClientId, Option<NotificationEnd>),
    UnblockCliPipeInput(String),   // String -> Pipe name
    CliPipeOutput(String, String), // String -> Pipe name, String -> Output
    AssociatePipeWithClient {
        pipe_id: String,
        client_id: ClientId,
    },
    DisconnectAllClientsExcept(ClientId),
    ChangeMode(ClientId, InputMode, Option<NotificationEnd>),
    ChangeModeForAllClients(InputMode),
    Reconfigure {
        client_id: ClientId,
        config: String,
        write_config_to_disk: bool,
    },
    ConfigWrittenToDisk(Config),
    FailedToWriteConfigToDisk(ClientId, Option<PathBuf>), // Pathbuf - file we failed to write
    RebindKeys {
        client_id: ClientId,
        keys_to_rebind: Vec<(InputMode, KeyWithModifier, Vec<Action>)>,
        keys_to_unbind: Vec<(InputMode, KeyWithModifier)>,
        write_config_to_disk: bool,
    },
    StartWebServer(ClientId),
    ShareCurrentSession(ClientId),
    StopSharingCurrentSession(ClientId),
    SendWebClientsForbidden(ClientId),
    WebServerStarted(String), // String -> base_url
    FailedToStartWebServer(String),
    ClearMouseHelpText(ClientId),
    ClearCommandOutputFlash(PaneId),
    /// Relay a forwarded-query dispatch from Screen to the server main
    /// loop. The main loop writes `ServerToClientMsg::ForwardQueryToHost`
    ForwardQueryToHost(u32, Vec<u8>, bool),
    KeyPassthroughChanged(ClientId, PaneId, PaneId, bool, Option<Direction>, bool),
    EmitNestedSessionFrameToClient(ClientId, Vec<u8>),
}

impl From<&ServerInstruction> for ServerContext {
    fn from(server_instruction: &ServerInstruction) -> Self {
        match *server_instruction {
            ServerInstruction::FirstClientConnected(..) => ServerContext::NewClient,
            ServerInstruction::Render(..) => ServerContext::Render,
            ServerInstruction::UnblockInputThread => ServerContext::UnblockInputThread,
            ServerInstruction::ClientExit(..) => ServerContext::ClientExit,
            ServerInstruction::RemoveClient(..) => ServerContext::RemoveClient,
            ServerInstruction::Error(_) => ServerContext::Error,
            ServerInstruction::KillSession => ServerContext::KillSession,
            ServerInstruction::DetachSession(..) => ServerContext::DetachSession,
            ServerInstruction::AttachClient(..) => ServerContext::AttachClient,
            ServerInstruction::AttachWatcherClient(..) => ServerContext::AttachClient,
            ServerInstruction::ConnStatus(..) => ServerContext::ConnStatus,
            ServerInstruction::Log(..) => ServerContext::Log,
            ServerInstruction::LogError(..) => ServerContext::LogError,
            ServerInstruction::SwitchSession(..) => ServerContext::SwitchSession,
            ServerInstruction::UnblockCliPipeInput(..) => ServerContext::UnblockCliPipeInput,
            ServerInstruction::CliPipeOutput(..) => ServerContext::CliPipeOutput,
            ServerInstruction::AssociatePipeWithClient { .. } => {
                ServerContext::AssociatePipeWithClient
            },
            ServerInstruction::DisconnectAllClientsExcept(..) => {
                ServerContext::DisconnectAllClientsExcept
            },
            ServerInstruction::ChangeMode(..) => ServerContext::ChangeMode,
            ServerInstruction::ChangeModeForAllClients(..) => {
                ServerContext::ChangeModeForAllClients
            },
            ServerInstruction::Reconfigure { .. } => ServerContext::Reconfigure,
            ServerInstruction::FailedToWriteConfigToDisk(..) => {
                ServerContext::FailedToWriteConfigToDisk
            },
            ServerInstruction::RebindKeys { .. } => ServerContext::RebindKeys,
            ServerInstruction::StartWebServer(..) => ServerContext::StartWebServer,
            ServerInstruction::ShareCurrentSession(..) => ServerContext::ShareCurrentSession,
            ServerInstruction::StopSharingCurrentSession(..) => {
                ServerContext::StopSharingCurrentSession
            },
            ServerInstruction::WebServerStarted(..) => ServerContext::WebServerStarted,
            ServerInstruction::FailedToStartWebServer(..) => ServerContext::FailedToStartWebServer,
            ServerInstruction::ConfigWrittenToDisk(..) => ServerContext::ConfigWrittenToDisk,
            ServerInstruction::SendWebClientsForbidden(..) => {
                ServerContext::SendWebClientsForbidden
            },
            ServerInstruction::ClearMouseHelpText(..) => ServerContext::ClearMouseHelpText,
            ServerInstruction::ClearCommandOutputFlash(..) => {
                ServerContext::ClearCommandOutputFlash
            },
            ServerInstruction::ForwardQueryToHost(..) => ServerContext::ForwardQueryToHost,
            ServerInstruction::KeyPassthroughChanged(..) => ServerContext::KeyPassthroughChanged,
            ServerInstruction::EmitNestedSessionFrameToClient(..) => {
                ServerContext::EmitNestedSessionFrameToClient
            },
        }
    }
}

impl ErrorInstruction for ServerInstruction {
    fn error(err: String) -> Self {
        ServerInstruction::Error(err)
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SessionConfiguration {
    runtime_config: HashMap<ClientId, Config>, // if present, overrides the saved_config
    saved_config: Config,                      // the config as it is on disk (not guaranteed),
                                               // when changed, this resets the runtime config to
                                               // be identical to it and override any previous
                                               // changes
}

impl SessionConfiguration {
    pub fn change_saved_config(&mut self, new_saved_config: Config) -> Vec<(ClientId, Config)> {
        self.saved_config = new_saved_config.clone();

        let mut config_changes = vec![];
        for (client_id, current_runtime_config) in self.runtime_config.iter_mut() {
            if *current_runtime_config != new_saved_config {
                *current_runtime_config = new_saved_config.clone();
                config_changes.push((*client_id, new_saved_config.clone()))
            }
        }
        config_changes
    }
    pub fn set_saved_configuration(&mut self, config: Config) {
        self.saved_config = config;
    }
    pub fn set_client_runtime_configuration(&mut self, client_id: ClientId, client_config: Config) {
        self.runtime_config.insert(client_id, client_config);
    }
    pub fn get_client_keybinds(&self, client_id: &ClientId) -> &Keybinds {
        self.runtime_config
            .get(client_id)
            .map(|c| &c.keybinds)
            .unwrap_or(&self.saved_config.keybinds)
    }
    pub fn get_client_default_input_mode(&self, client_id: &ClientId) -> InputMode {
        self.runtime_config
            .get(client_id)
            .or_else(|| Some(&self.saved_config))
            .and_then(|c| c.options.default_mode.clone())
            .unwrap_or_default()
    }
    pub fn get_client_configuration(&self, client_id: &ClientId) -> Config {
        self.runtime_config
            .get(client_id)
            .or_else(|| Some(&self.saved_config))
            .cloned()
            .unwrap_or_default()
    }
    pub fn reconfigure_runtime_config(
        &mut self,
        client_id: &ClientId,
        stringified_config: String,
    ) -> (Option<Config>, bool) {
        // bool is whether the config changed
        let mut full_reconfigured_config = None;
        let mut config_changed = false;
        let current_client_configuration = self.get_client_configuration(client_id);
        match Config::from_kdl(
            &stringified_config,
            Some(current_client_configuration.clone()),
        ) {
            Ok(new_config) => {
                config_changed = current_client_configuration != new_config;
                full_reconfigured_config = Some(new_config.clone());
                self.runtime_config.insert(*client_id, new_config);
            },
            Err(e) => {
                log::error!("Failed to reconfigure runtime config: {}", e);
            },
        }
        (full_reconfigured_config, config_changed)
    }
    pub fn rebind_keys(
        &mut self,
        client_id: &ClientId,
        keys_to_rebind: Vec<(InputMode, KeyWithModifier, Vec<Action>)>,
        keys_to_unbind: Vec<(InputMode, KeyWithModifier)>,
    ) -> (Option<Config>, bool) {
        let mut full_reconfigured_config = None;
        let mut config_changed = false;

        if self.runtime_config.get(client_id).is_none() {
            self.runtime_config
                .insert(*client_id, self.saved_config.clone());
        }
        match self.runtime_config.get_mut(client_id) {
            Some(config) => {
                for (input_mode, key_with_modifier) in keys_to_unbind {
                    let keys_in_mode = config
                        .keybinds
                        .0
                        .entry(input_mode)
                        .or_insert_with(Default::default);
                    let removed = keys_in_mode.remove(&key_with_modifier);
                    if removed.is_some() {
                        config_changed = true;
                    }
                }
                for (input_mode, key_with_modifier, actions) in keys_to_rebind {
                    let keys_in_mode = config
                        .keybinds
                        .0
                        .entry(input_mode)
                        .or_insert_with(Default::default);
                    if keys_in_mode.get(&key_with_modifier) != Some(&actions) {
                        config_changed = true;
                        keys_in_mode.insert(key_with_modifier, actions);
                    }
                }
                if config_changed {
                    full_reconfigured_config = Some(config.clone());
                }
            },
            None => {
                log::error!(
                    "Could not find runtime or saved configuration for client, cannot rebind keys"
                );
            },
        }

        (full_reconfigured_config, config_changed)
    }
}

pub(crate) struct SessionMetaData {
    pub senders: ThreadSenders,
    pub default_shell: Option<TerminalAction>,
    pub current_input_modes: HashMap<ClientId, InputMode>,
    pub session_configuration: SessionConfiguration,
    pub key_passthrough_clients: HashMap<ClientId, PaneId>,
    pub web_sharing: WebSharing, // this is a special attribute explicitly set on session
    // initialization because we don't want it to be overridden by
    // configuration changes, the only way it can be overwritten is by
    // explicit plugin action
    screen_thread: Option<thread::JoinHandle<()>>,
    pty_thread: Option<thread::JoinHandle<()>>,
    plugin_thread: Option<thread::JoinHandle<()>>,
    pty_writer_thread: Option<thread::JoinHandle<()>>,
    background_jobs_thread: Option<thread::JoinHandle<()>>,
    config_file_path: Option<PathBuf>,
}

impl SessionMetaData {
    pub fn get_client_keybinds_and_mode(
        &self,
        client_id: &ClientId,
    ) -> Option<(&Keybinds, &InputMode, InputMode)> {
        // (keybinds, current_input_mode,
        // default_input_mode)
        let client_keybinds = self.session_configuration.get_client_keybinds(client_id);
        let default_input_mode = self
            .session_configuration
            .get_client_default_input_mode(client_id);
        match self.current_input_modes.get(client_id) {
            Some(client_input_mode) => {
                Some((client_keybinds, client_input_mode, default_input_mode))
            },
            _ => None,
        }
    }
    pub fn remove_key_passthrough_client(&mut self, client_id: ClientId) {
        self.remove_key_passthrough_client_with_notify(client_id, true);
    }
    pub fn remove_key_passthrough_client_with_notify(
        &mut self,
        client_id: ClientId,
        notify_guest: bool,
    ) {
        if let Some(pane_id) = self.key_passthrough_clients.remove(&client_id) {
            let pane_still_active = self.key_passthrough_clients.values().any(|p| *p == pane_id);
            if !pane_still_active && notify_guest {
                if let PaneId::Terminal(terminal_id) = pane_id {
                    let frame = zellij_utils::nested_session::encode_frame(
                        &zellij_utils::nested_session::NestedSessionMessage::FocusLost,
                    );
                    let _ = self.senders.send_to_pty_writer(PtyWriteInstruction::Write(
                        frame,
                        terminal_id,
                        None,
                    ));
                }
            }
        }
    }
    pub fn change_mode_for_all_clients(&mut self, input_mode: InputMode) {
        let all_clients: Vec<ClientId> = self.current_input_modes.keys().copied().collect();
        for client_id in all_clients {
            self.current_input_modes.insert(client_id, input_mode);
        }
    }
    pub fn propagate_configuration_changes(
        &mut self,
        config_changes: Vec<(ClientId, Config)>,
        config_was_written_to_disk: bool,
    ) {
        let mut new_plugin_config = None;
        for (client_id, new_config) in config_changes {
            if new_plugin_config.is_none() {
                new_plugin_config = Some(new_config.plugins.clone());

                // Two settings the server keeps as process globals, because their readers are
                // nowhere near a config: the title format is read by a pane while it renders, and
                // the snapshot settings are read on the way out, after the session data has gone.
                // Everything else here travels to a client through `Reconfigure`; these two have no
                // per-client home to travel in, so they are rewritten from the FIRST config of the
                // change set and are the same for every client - which is what they already were,
                // fixed at `FirstClientConnected` and unchanged for the life of the session.
                set_snapshot_settings(SnapshotSettings::from_options(Some(&new_config.options)));
                set_terminal_title_format(TerminalTitleFormat::from_options(&new_config.options));
            }

            self.default_shell = new_config.options.default_shell.as_ref().map(|shell| {
                TerminalAction::RunCommand(RunCommand {
                    command: shell.clone(),
                    cwd: new_config.options.default_cwd.clone(),
                    use_terminal_title: true,
                    ..Default::default()
                })
            });
            let host_theme_dark = new_config
                .options
                .theme_dark
                .as_ref()
                .and_then(|name| new_config.theme_config(Some(name)));
            let host_theme_light = new_config
                .options
                .theme_light
                .as_ref()
                .and_then(|name| new_config.theme_config(Some(name)));
            if new_config.options.theme_dark.is_some() && host_theme_dark.is_none() {
                log::warn!(
                    "theme_dark='{}' not found in themes; auto-theme switch disabled for dark.",
                    new_config.options.theme_dark.as_deref().unwrap_or("?")
                );
            }
            if new_config.options.theme_light.is_some() && host_theme_light.is_none() {
                log::warn!(
                    "theme_light='{}' not found in themes; auto-theme switch disabled for light.",
                    new_config.options.theme_light.as_deref().unwrap_or("?")
                );
            }
            let pane_frame_style = PaneFrameStyle::from_options(&new_config.options);
            self.senders
                .send_to_screen(ScreenInstruction::Reconfigure {
                    client_id,
                    keybinds: new_config.keybinds.clone(),
                    default_mode: new_config
                        .options
                        .default_mode
                        .unwrap_or_else(Default::default),
                    theme: new_config
                        .theme_config(new_config.options.theme.as_ref())
                        .unwrap_or_else(|| default_palette().into()),
                    host_theme_dark,
                    host_theme_light,
                    simplified_ui: new_config.options.simplified_ui.unwrap_or(false),
                    default_shell: new_config.options.default_shell,
                    pane_frame_style,
                    copy_command: new_config.options.copy_command,
                    copy_to_clipboard: new_config.options.copy_clipboard,
                    copy_on_select: new_config.options.copy_on_select.unwrap_or(true),
                    auto_layout: new_config.options.auto_layout.unwrap_or(true),
                    rounded_corners: new_config.ui.pane_frames.rounded_corners,
                    hide_session_name: new_config.ui.pane_frames.hide_session_name,
                    stacked_resize: new_config.options.stacked_resize.unwrap_or(true),
                    stacked_pane_list: new_config.options.stacked_pane_list.unwrap_or(true),
                    default_editor: new_config.options.scrollback_editor.clone(),
                    default_floating_size: new_config.options.default_floating_size.clone(),
                    advanced_mouse_actions: new_config
                        .options
                        .advanced_mouse_actions
                        .unwrap_or(true),
                    mouse_scroll_resize: new_config.options.mouse_scroll_resize.unwrap_or(true),
                    mouse_hover_effects: new_config.options.mouse_hover_effects.unwrap_or(true),
                    mouse_hover_tips: new_config.options.mouse_hover_tips.unwrap_or(true),
                    visual_bell: new_config.options.visual_bell.unwrap_or(true),
                    bell_clear_delay_ms: new_config.options.bell_clear_delay_ms.unwrap_or(0),
                    focus_follows_mouse: new_config.options.focus_follows_mouse.unwrap_or(false),
                    mouse_click_through: new_config.options.mouse_click_through.unwrap_or(false),
                    osc133_command_selection: new_config
                        .options
                        .osc133_command_selection
                        .unwrap_or(true),
                    dangerously_enable_paste_buffer_read: new_config
                        .options
                        .dangerously_enable_paste_buffer_read
                        .unwrap_or(false),
                    word_separators: new_config
                        .options
                        .word_separators
                        .clone()
                        .unwrap_or_else(|| DEFAULT_WORD_SEPARATORS.to_owned()),
                    host_notification_protocol: new_config
                        .options
                        .host_notification_protocol
                        .unwrap_or_default(),
                    nested_session_handling: new_config
                        .options
                        .nested_session_handling
                        .unwrap_or_default(),
                })
                .unwrap();
            self.senders
                .send_to_plugin(PluginInstruction::Reconfigure {
                    client_id,
                    keybinds: Some(new_config.keybinds),
                    default_mode: new_config.options.default_mode,
                    default_shell: self.default_shell.clone(),
                    layout_dir: new_config.options.layout_dir,
                    was_written_to_disk: config_was_written_to_disk,
                })
                .unwrap();
            self.senders
                .send_to_pty(PtyInstruction::Reconfigure {
                    client_id,
                    default_editor: new_config.options.scrollback_editor,
                    post_command_discovery_hook: new_config.options.post_command_discovery_hook,
                    resurrect_command_hints: new_config.options.resurrect_command_hints,
                    report_pane_env: new_config.options.report_pane_env,
                    detect_agents: new_config.options.detect_agents,
                })
                .unwrap();
        }

        // Detect and notify plugins of configuration changes
        if config_was_written_to_disk {
            if let Some(new_plugins) = new_plugin_config {
                self.senders
                    .send_to_plugin(PluginInstruction::DetectPluginConfigChanges(new_plugins))
                    .unwrap();
            }
        }
    }
}

impl Drop for SessionMetaData {
    fn drop(&mut self) {
        let _ = self.senders.send_to_pty(PtyInstruction::Exit);
        let _ = self.senders.send_to_screen(ScreenInstruction::Exit);
        let _ = self.senders.send_to_plugin(PluginInstruction::Exit);
        let _ = self.senders.send_to_pty_writer(PtyWriteInstruction::Exit);
        let _ = self.senders.send_to_background_jobs(BackgroundJob::Exit);
        // pty before screen: dropping `Pty` is what signals the pane shells, and screen can take
        // an unbounded amount of time to wind down (it is still being fed by panes that are alive
        // until pty kills them). Joining screen first made the shells outlive the whole teardown.
        if let Some(pty_thread) = self.pty_thread.take() {
            let _ = pty_thread.join();
        }
        if let Some(screen_thread) = self.screen_thread.take() {
            let _ = screen_thread.join();
        }
        if let Some(plugin_thread) = self.plugin_thread.take() {
            let _ = plugin_thread.join();
        }
        if let Some(pty_writer_thread) = self.pty_writer_thread.take() {
            let _ = pty_writer_thread.join();
        }
        if let Some(background_jobs_thread) = self.background_jobs_thread.take() {
            let _ = background_jobs_thread.join();
        }
    }
}

/// Remove a client from session state and synthesize empty
/// `ForwardedReplyFromHost` instructions for any host-query forwards
/// that had been dispatched to it. Without this, a client that goes
/// away while holding an in-flight forward leaves `Screen`'s
/// `forward_in_flight` flag stuck — the flag is only released by
/// replies, and no reply will ever come.
fn remove_client_and_flush_forwards(
    client_id: ClientId,
    os_input: &mut Box<dyn ServerOsApi>,
    session_state: &Arc<RwLock<SessionState>>,
    session_data: &Arc<RwLock<Option<SessionMetaData>>>,
) {
    let _ = os_input.remove_client(client_id);
    let stuck_tokens = session_state.write().unwrap().remove_client(client_id);
    if stuck_tokens.is_empty() {
        return;
    }
    if let Some(session) = session_data.read().unwrap().as_ref() {
        for token in stuck_tokens {
            let _ = session
                .senders
                .send_to_screen(ScreenInstruction::ForwardedReplyFromHost {
                    token,
                    reply_bytes: Vec::new(),
                });
        }
    }
}

macro_rules! remove_client {
    ($client_id:expr, $os_input:expr, $session_state:expr, $session_data:expr) => {
        $crate::remove_client_and_flush_forwards(
            $client_id,
            &mut $os_input,
            &$session_state,
            &$session_data,
        );
    };
}

macro_rules! remove_watcher {
    ($client_id:expr, $os_input:expr, $session_state:expr) => {
        $os_input.remove_client($client_id).unwrap();
        $session_state.write().unwrap().remove_watcher($client_id);
    };
}

macro_rules! send_to_client {
    ($client_id:expr, $os_input:expr, $msg:expr, $session_state:expr, $session_data:expr) => {
        let send_to_client_res = $os_input.send_to_client($client_id, $msg);
        if let Err(e) = send_to_client_res {
            // Try to recover the message
            let context = match e.downcast_ref::<ZellijError>() {
                Some(ZellijError::ClientTooSlow { .. }) => {
                    format!(
                        "client {} is processing server messages too slow",
                        $client_id
                    )
                },
                _ => {
                    format!("failed to route server message to client {}", $client_id)
                },
            };
            // Log it so it isn't lost
            Err::<(), _>(e).context(context).non_fatal();
            // failed to send to client, remove it
            remove_client!($client_id, $os_input, $session_state, $session_data);
        }
    };
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionState {
    clients: HashMap<ClientId, Option<(Size, bool)>>, // bool -> is_web_client
    pipes: HashMap<String, ClientId>,                 // String => pipe_id
    watchers: HashMap<ClientId, bool>, // watcher clients (read-only observers) bool -> is_web_client
    last_active_client: Option<ClientId>, // last client that sent a Key message
    /// Host-query forward tokens that have been dispatched to a
    /// specific client and are waiting for a reply. Used to clean up
    /// when that client disconnects (or when there's no client to
    /// dispatch to in the first place) — each stuck token gets an
    /// empty synthetic reply so `Screen`'s `forward_in_flight` slot
    /// releases and the queued forwards keep moving.
    forwards_in_flight: HashMap<u32, ClientId>,
}

impl SessionState {
    pub fn new() -> Self {
        SessionState {
            clients: HashMap::new(),
            pipes: HashMap::new(),
            watchers: HashMap::new(),
            last_active_client: None,
            forwards_in_flight: HashMap::new(),
        }
    }
    pub fn new_client(&mut self) -> ClientId {
        let all_ids: HashSet<ClientId> = self
            .clients
            .keys()
            .copied()
            .chain(self.watchers.keys().copied())
            .collect();

        let mut next_client_id = 1;
        loop {
            if all_ids.contains(&next_client_id) {
                next_client_id += 1;
            } else {
                break;
            }
        }
        self.clients.insert(next_client_id, None);
        next_client_id
    }
    pub fn associate_pipe_with_client(&mut self, pipe_id: String, client_id: ClientId) {
        self.pipes.insert(pipe_id, client_id);
    }
    /// Remove a client and return any host-query tokens that had
    /// been dispatched to this client and were still awaiting a
    /// reply. Callers must synthesize an empty reply for each token
    /// (via `ScreenInstruction::ForwardedReplyFromHost`) so
    /// `Screen`'s in-flight slot releases and any queued forwards
    /// can proceed.
    pub fn remove_client(&mut self, client_id: ClientId) -> Vec<u32> {
        self.clients.remove(&client_id);
        self.pipes.retain(|_p_id, c_id| c_id != &client_id);
        self.clear_last_active_client(client_id);
        let stuck: Vec<u32> = self
            .forwards_in_flight
            .iter()
            .filter_map(|(token, owner)| (*owner == client_id).then_some(*token))
            .collect();
        for token in &stuck {
            self.forwards_in_flight.remove(token);
        }
        stuck
    }
    pub fn set_client_size(&mut self, client_id: ClientId, size: Size) {
        self.clients
            .entry(client_id)
            .or_insert_with(Default::default)
            .as_mut()
            .map(|(s, _is_web_client)| *s = size);
    }
    pub fn set_client_data(&mut self, client_id: ClientId, size: Size, is_web_client: bool) {
        self.clients.insert(client_id, Some((size, is_web_client)));
    }
    pub fn client_ids(&self) -> Vec<ClientId> {
        self.clients.keys().copied().collect()
    }
    /// The clients that have attached a terminal, and so have an input thread to unblock.
    ///
    /// A `zellij action` client is registered like any other but never attaches, so its entry has
    /// no size and stays `None`. It has no input to unblock either: its action is answered by the
    /// route thread serving it, in order, and a broadcast that reaches it first is read as that
    /// answer and ends the command early.
    pub fn attached_client_ids(&self) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter_map(|(client_id, attachment)| attachment.map(|_| *client_id))
            .collect()
    }
    pub fn watcher_client_ids(&self) -> Vec<ClientId> {
        self.watchers.keys().copied().collect()
    }
    pub fn web_client_ids(&self) -> Vec<ClientId> {
        self.clients
            .iter()
            .filter_map(|(c_id, size_and_is_web_client)| {
                size_and_is_web_client
                    .and_then(|(_s, is_web_client)| if is_web_client { Some(*c_id) } else { None })
            })
            .collect()
    }
    pub fn web_watcher_client_ids(&self) -> Vec<ClientId> {
        self.watchers
            .iter()
            .filter_map(
                |(&c_id, &is_web_client)| {
                    if is_web_client {
                        Some(c_id)
                    } else {
                        None
                    }
                },
            )
            .collect()
    }
    pub fn get_pipe(&self, pipe_name: &str) -> Option<ClientId> {
        self.pipes.get(pipe_name).copied()
    }
    pub fn active_clients_are_connected(&self) -> bool {
        let ids_of_pipe_clients: HashSet<ClientId> = self.pipes.values().copied().collect();
        let mut active_clients_connected = false;
        for client_id in self.clients.keys() {
            if ids_of_pipe_clients.contains(client_id) {
                continue;
            }
            active_clients_connected = true;
        }
        active_clients_connected
    }
    pub fn convert_client_to_watcher(&mut self, client_id: ClientId, is_web_client: bool) {
        self.clients.remove(&client_id);
        self.watchers.insert(client_id, is_web_client);
    }
    pub fn is_watcher(&self, client_id: &ClientId) -> bool {
        self.watchers.get(client_id).is_some()
    }
    pub fn remove_watcher(&mut self, client_id: ClientId) {
        self.watchers.remove(&client_id);
    }
    pub fn set_last_active_client(&mut self, client_id: ClientId) {
        self.last_active_client = Some(client_id);
    }
    pub fn get_last_active_client(&self) -> Option<ClientId> {
        self.last_active_client
    }
    pub fn clear_last_active_client(&mut self, client_id: ClientId) {
        if self.last_active_client == Some(client_id) {
            self.last_active_client = None;
        }
    }
    pub fn mark_forward_in_flight(&mut self, token: u32, client_id: ClientId) {
        self.forwards_in_flight.insert(token, client_id);
    }
    pub fn clear_forward_in_flight(&mut self, token: u32) {
        self.forwards_in_flight.remove(&token);
    }
    /// Client to route a host-query forward to. Prefers whichever
    /// client most recently interacted with the session; falls back
    /// to any currently-connected non-watcher client. Returns `None`
    /// only when no regular client is connected.
    pub fn pick_forward_target(&self) -> Option<ClientId> {
        if let Some(candidate) = self.last_active_client {
            if self.clients.contains_key(&candidate) {
                return Some(candidate);
            }
        }
        self.clients.keys().copied().next()
    }
}

#[cfg(test)]
mod session_state_tests {
    use super::*;

    fn with_client(id: ClientId) -> SessionState {
        let mut s = SessionState::new();
        s.clients.insert(id, None);
        s
    }

    #[test]
    fn a_client_that_never_attached_is_not_one_to_unblock() {
        // this is what keeps a `close-pane` report from being outrun: the pty thread broadcasts an
        // unblock as a pane is torn down, and a `zellij action` client reads the first message it
        // gets as the answer to its command. It is registered like any other client but never
        // attaches, so it is not in `attached_client_ids` and the broadcast passes it by
        let mut session_state = SessionState::new();
        let cli_client = session_state.new_client();
        let attached_client = session_state.new_client();
        session_state.set_client_data(attached_client, Size { rows: 20, cols: 80 }, false);

        assert_eq!(session_state.attached_client_ids(), vec![attached_client]);
        let mut all = session_state.client_ids();
        all.sort_unstable();
        assert_eq!(
            all,
            vec![cli_client, attached_client],
            "both are still clients: only the unblock treats them differently"
        );
    }

    #[test]
    fn a_client_id_is_handed_out_again_the_moment_it_is_removed() {
        // the precondition the route thread's single removal announcement rests on. Ids are the
        // lowest free number, so in a session nothing is attached to every transient `zellij
        // action` connection is client 1, one after another. Removing a client is what frees its
        // id - which is why the announcement has to come after that thread's last act, and why
        // announcing it twice let the second one tear down whatever connection had since been
        // given the id. If this test ever fails, read the comment at the end of `route_thread_main`
        // before deciding the change is harmless
        let mut session_state = SessionState::new();
        let first = session_state.new_client();
        assert_eq!(first, 1);
        session_state.remove_client(first);
        assert_eq!(
            session_state.new_client(),
            first,
            "the next connection is given the id the last one had just freed"
        );
        // and while the first is still registered, the next connection gets a different id - which
        // is what makes the removal announcement, not the client's departure, the moment of danger
        assert_eq!(session_state.new_client(), 2);
    }

    #[test]
    fn pick_forward_target_prefers_last_active_when_still_connected() {
        let mut s = SessionState::new();
        s.clients.insert(1, None);
        s.clients.insert(2, None);
        s.set_last_active_client(2);
        assert_eq!(s.pick_forward_target(), Some(2));
    }

    #[test]
    fn pick_forward_target_falls_back_when_last_active_disconnected() {
        let mut s = SessionState::new();
        s.clients.insert(1, None);
        s.clients.insert(2, None);
        // Client 3 was last active but has since disconnected — not in
        // `clients` map anymore. Must fall through to any connected
        // client rather than returning None.
        s.last_active_client = Some(3);
        let picked = s
            .pick_forward_target()
            .expect("some client still connected");
        assert!(picked == 1 || picked == 2);
    }

    #[test]
    fn pick_forward_target_none_when_no_clients() {
        let s = SessionState::new();
        assert_eq!(s.pick_forward_target(), None);
    }

    #[test]
    fn remove_client_returns_stuck_forward_tokens() {
        let mut s = with_client(1);
        s.mark_forward_in_flight(10, 1);
        s.mark_forward_in_flight(11, 1);
        // A forward dispatched to a *different* client — must not be
        // returned when we remove client 1.
        s.clients.insert(2, None);
        s.mark_forward_in_flight(12, 2);

        let mut stuck = s.remove_client(1);
        stuck.sort_unstable();
        assert_eq!(stuck, vec![10, 11]);
        assert_eq!(s.forwards_in_flight.get(&12), Some(&2));
    }

    #[test]
    fn remove_client_returns_empty_when_no_forwards_in_flight() {
        let mut s = with_client(1);
        assert!(s.remove_client(1).is_empty());
    }

    #[test]
    fn clear_forward_in_flight_removes_entry() {
        let mut s = with_client(1);
        s.mark_forward_in_flight(42, 1);
        s.clear_forward_in_flight(42);
        // After clear, removing the client yields no stuck tokens.
        assert!(s.remove_client(1).is_empty());
    }
}

/// The snapshot archive settings of the session running in this server.
///
/// First set when the first client connects and the config file has been read, set again on every
/// live reload, and read again on the way out - long after the session data has been dropped, which
/// is why it is a global rather than a field.
///
/// **A `RwLock` and not a `OnceLock`.** It was the latter, so an edited `snapshot_*` setting did
/// nothing until the session was recreated, which is the one thing a person editing snapshot
/// retention is least likely to want to do.
static SNAPSHOT_SETTINGS: std::sync::RwLock<Option<SnapshotSettings>> =
    std::sync::RwLock::new(None);

pub(crate) fn snapshot_settings() -> SnapshotSettings {
    SNAPSHOT_SETTINGS
        .read()
        .ok()
        .and_then(|settings| settings.clone())
        .unwrap_or_else(SnapshotSettings::default)
}

fn set_snapshot_settings(settings: SnapshotSettings) {
    if let Ok(mut slot) = SNAPSHOT_SETTINGS.write() {
        *slot = Some(settings);
    }
}

/// The pane privacy policy this session answers with, compiled once when the first client
/// connected and the config file was read.
///
/// A `OnceLock` rather than a field on the session, because `route.rs` is handed no configuration
/// and threading one through every call site would put the decision in more than one place. The
/// filter has exactly one evaluation point and this is how it stays that way.
static PANE_PRIVACY: std::sync::OnceLock<pane_privacy::PanePrivacy> = std::sync::OnceLock::new();

/// The policy, or `Off` before the config has been read.
///
/// `Off` is right for that window rather than fail-closed: nothing has been listed yet because no
/// client has connected, and the first thing that happens after a client connects is that the real
/// policy is set.
static PANE_PRIVACY_OFF: pane_privacy::PanePrivacy = pane_privacy::PanePrivacy::Off;

pub(crate) fn pane_privacy_policy() -> &'static pane_privacy::PanePrivacy {
    PANE_PRIVACY.get().unwrap_or(&PANE_PRIVACY_OFF)
}

/// Archive any `session_info` folder left behind by a server that is no longer running.
///
/// This is the SIGKILL and crash path: the periodic serializer's file survives, so it is promoted
/// into the archive rather than lost to the next session of the same name overwriting it. This
/// session's own folder is always swept, since starting is exactly what is about to reuse it.
fn promote_orphaned_session_info_folders(own_session_name: &str, settings: &SnapshotSettings) {
    let Ok(entries) = std::fs::read_dir(&*zellij_utils::consts::ZELLIJ_SESSION_INFO_CACHE_DIR)
    else {
        return;
    };
    for entry in entries.flatten() {
        if !entry.path().is_dir() {
            continue;
        }
        let session_name = entry.file_name().to_string_lossy().to_string();
        // a socket in place means someone else's server owns that folder. Testing for the file
        // rather than connecting to it is deliberate: this runs on the server thread, and probing
        // sockets from here would have us wait on a reply from ourselves.
        let has_socket = zellij_utils::consts::ZELLIJ_SOCK_DIR
            .join(&session_name)
            .exists();
        if has_socket && session_name != own_session_name {
            continue;
        }
        if let Err(e) = archive_session_info(&session_name, SnapshotReason::Promoted, settings) {
            log::error!("Failed to promote session {:?}: {}", session_name, e);
        }
    }
}

pub fn start_server(os_input: Box<dyn ServerOsApi>, socket_path: PathBuf) {
    // The listener below unlinks whatever sits at this path before it binds, which is right for a
    // socket a dead server left behind and catastrophic for one a live server is still holding:
    // the old server keeps running, keeps its panes, and becomes unreachable by any client,
    // because a unix socket cannot be given its pathname back. Nothing upstream of here is
    // guaranteed to have checked -- the client's check is a liveness probe, and a busy server can
    // fail one -- so the last thing that can still refuse, does.
    #[cfg(unix)]
    if zellij_utils::consts::ipc_connect(&socket_path).is_ok() {
        let message = format!(
            "refusing to start: a server is already listening on {}",
            socket_path.display()
        );
        log::error!("{}", message);
        eprintln!("{}", message);
        std::process::exit(1);
    }

    info!("Starting Zellij server!");

    #[cfg(unix)]
    {
        use nix::sys::stat::{umask, Mode};
        // preserve the current umask: read current value by setting to another mode, and then restoring it
        let current_umask = umask(Mode::all());
        umask(current_umask);
        daemonize::Daemonize::new()
            .working_directory(std::env::current_dir().unwrap())
            .umask(current_umask.bits() as u32)
            .start()
            .expect("could not daemonize the server process");
    }

    #[cfg(windows)]
    {
        // The server is spawned with CREATE_NEW_PROCESS_GROUP, which disables
        // Ctrl+C handling for the process.  Child processes inherit this
        // disabled state, so ConPTY children (shells, commands) would silently
        // ignore CTRL_C_EVENT signals.  Re-enable Ctrl+C here so that
        // descendants get the normal default handler (terminate on Ctrl+C).
        //
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        unsafe {
            SetConsoleCtrlHandler(None, 0);
        }
    }

    start_server_impl(os_input, socket_path, true);
}

pub fn start_server_impl(
    mut os_input: Box<dyn ServerOsApi>,
    socket_path: PathBuf,
    install_panic_hook: bool,
) {
    envs::set_zellij("0".to_string());

    // Settle macOS's file-access decisions about THIS executable, so the upgrade that lost the
    // grants is what asks for them back. A launcher-created session has no terminal emulator to
    // inherit grants from, and they are keyed to the executable's versioned path, so each upgrade
    // starts with none. Returns at once - it must never block startup on a consent dialog.
    // No-op off macOS.
    zellij_utils::session_lifecycle::probe_protected_locations();

    // the socket is named after the session, and this is the only place the server learns its own
    // name without asking a thread that may already be shutting down
    let session_name = socket_path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();

    let (to_server, server_receiver): ChannelWithContext<ServerInstruction> = channels::bounded(50);
    let to_server = SenderWithContext::new(to_server);
    let session_data: Arc<RwLock<Option<SessionMetaData>>> = Arc::new(RwLock::new(None));
    let session_state = Arc::new(RwLock::new(SessionState::new()));

    if install_panic_hook {
        std::panic::set_hook({
            use zellij_utils::errors::handle_panic;
            let to_server = to_server.clone();
            Box::new(move |info| {
                handle_panic(info, Some(&to_server));
            })
        });
    }

    // A SIGTERM is how a session ends on a reboot, a logout, and a `systemctl --user stop`, and
    // without a handler it kills the server outright: the last periodic serialize can be a whole
    // `serialization_interval` old, and no snapshot is archived at all. `KillSession` is exactly
    // the graceful path - serialize once more, tell the clients, then archive on the way out - so
    // this thread only has to ask for it. The shutdown that follows is the one `kill-session`
    // already runs, so nothing new can hang here that could not hang there.
    //
    // `install_panic_hook` is false only for the in-process server the integration tests run, and
    // that one must not take the test harness's signals.
    #[cfg(unix)]
    if install_panic_hook {
        let to_server = to_server.clone();
        let _ = thread::Builder::new()
            .name("signal_listener".to_string())
            .spawn(move || {
                match signal_hook::iterator::Signals::new([signal_hook::consts::SIGTERM]) {
                    Ok(mut signals) => {
                        if signals.forever().next().is_some() {
                            let _ = to_server.send(ServerInstruction::KillSession);
                        }
                    },
                    Err(e) => log::error!("could not listen for SIGTERM: {}", e),
                }
            });
    }

    let _ = thread::Builder::new()
        .name("server_listener".to_string())
        .spawn({
            use interprocess::local_socket::prelude::*;
            use zellij_utils::consts::ipc_bind;
            #[cfg(unix)]
            use zellij_utils::shared::set_permissions;

            let os_input = os_input.clone();
            let session_data = session_data.clone();
            let session_state = session_state.clone();
            let to_server = to_server.clone();
            let socket_path = socket_path.clone();
            move || {
                drop(std::fs::remove_file(&socket_path));
                let listener = ipc_bind(&socket_path).unwrap();
                // set the sticky bit to avoid the socket file being potentially cleaned up
                // https://specifications.freedesktop.org/basedir-spec/basedir-spec-latest.html states that for XDG_RUNTIME_DIR:
                // "To ensure that your files are not removed, they should have their access time timestamp modified at least once every 6 hours of monotonic time or the 'sticky' bit should be set on the file. "
                // It is not guaranteed that all platforms allow setting the sticky bit on sockets!
                #[cfg(unix)]
                drop(set_permissions(&socket_path, 0o1700));

                // On Windows, named pipes are half-duplex, so we need a separate
                // reply pipe for server→client messages.
                #[cfg(windows)]
                let reply_listener = zellij_utils::consts::ipc_bind_reply(&socket_path).unwrap();

                for stream in listener.incoming() {
                    match stream {
                        Ok(stream) => {
                            let mut os_input = os_input.clone();
                            let client_id = session_state.write().unwrap().new_client();

                            #[cfg(windows)]
                            let reply_stream = reply_listener
                                .accept()
                                .expect("failed to accept reply connection");

                            #[cfg(windows)]
                            let receiver = os_input
                                .new_client_with_reply(client_id, stream, reply_stream)
                                .unwrap();
                            #[cfg(not(windows))]
                            let receiver = os_input.new_client(client_id, stream).unwrap();

                            let session_data = session_data.clone();
                            let session_state = session_state.clone();
                            let to_server = to_server.clone();
                            thread::Builder::new()
                                .name("server_router".to_string())
                                .spawn(move || {
                                    route_thread_main(
                                        session_data,
                                        session_state,
                                        os_input,
                                        to_server,
                                        receiver,
                                        client_id,
                                    )
                                    .fatal()
                                })
                                .unwrap();
                        },
                        Err(err) => {
                            panic!("err {:?}", err);
                        },
                    }
                }
            }
        });

    loop {
        let (instruction, mut err_ctx) = server_receiver.recv().unwrap();
        err_ctx.add_call(ContextType::IPCServer((&instruction).into()));
        match instruction {
            ServerInstruction::FirstClientConnected(cli_assets, is_web_client, client_id) => {
                let host_terminal_env = cli_assets.host_terminal_env.clone();
                let (config, layout) = cli_assets.load_config_and_layout();
                let layout_is_welcome_screen = cli_assets.layout
                    == Some(LayoutInfo::BuiltIn("welcome".to_owned()))
                    || config.options.default_layout == Some(PathBuf::from("welcome"));

                let successfully_written_config = Config::write_config_to_disk_if_it_does_not_exist(
                    config.to_string(true),
                    &cli_assets.config_file_path,
                );
                // if we successfully wrote the config to disk, it means two things:
                // 1. It did not exist beforehand
                // 2. The config folder is writeable
                //
                // If these two are true, we should launch the setup wizard, if even one of them is
                // false, we should never launch it.
                let should_launch_setup_wizard = successfully_written_config;

                let runtime_config_options = match &cli_assets.configuration_options {
                    Some(configuration_options) => {
                        config.options.merge(configuration_options.clone())
                    },
                    None => config.options.clone(),
                };

                let client_attributes = ClientAttributes {
                    size: cli_assets.terminal_window_size,
                    tty: cli_assets.tty.clone(),
                    style: Style {
                        colors: config
                            .theme_config(runtime_config_options.theme.as_ref())
                            .unwrap_or_else(|| default_palette().into()),
                        rounded_corners: config.ui.pane_frames.rounded_corners,
                        hide_session_name: config.ui.pane_frames.hide_session_name,
                    },
                };

                set_snapshot_settings(SnapshotSettings::from_options(Some(
                    &runtime_config_options,
                )));
                // the policy is compiled once, here, and answered from `route.rs` for the life of
                // the session. A second copy of the rule is a second chance to disagree with the
                // first, and the way it would disagree is by showing a pane the other one hides
                let pane_privacy = pane_privacy::PanePrivacy::from_options(&runtime_config_options);
                if let Some(reason) = pane_privacy.broken_reason() {
                    log::error!(
                        "pane privacy policy is broken, so this session withholds every pane: {}",
                        reason
                    );
                }
                let _ = PANE_PRIVACY.set(pane_privacy);
                promote_orphaned_session_info_folders(&session_name, &snapshot_settings());

                set_terminal_title_format(TerminalTitleFormat::from_options(
                    &runtime_config_options,
                ));
                info!("FirstClientConnected: initializing session");
                let mut session = init_session(
                    os_input.clone(),
                    to_server.clone(),
                    client_attributes.clone(),
                    Box::new(runtime_config_options.clone()), // TODO: no box
                    Box::new(layout.clone()),                 // TODO: no box
                    cli_assets.clone(),
                    config.clone(),
                    config.plugins.clone(),
                    client_id,
                );
                info!("FirstClientConnected: session initialized, spawning tabs");
                let mut runtime_configuration = config.clone();
                runtime_configuration.options = runtime_config_options.clone();
                session
                    .session_configuration
                    .set_saved_configuration(config.clone());
                session
                    .session_configuration
                    .set_client_runtime_configuration(client_id, runtime_configuration);
                let default_input_mode = runtime_config_options.default_mode.unwrap_or_default();
                session
                    .current_input_modes
                    .insert(client_id, default_input_mode);

                *session_data.write().unwrap() = Some(session);
                session_state.write().unwrap().set_client_data(
                    client_id,
                    client_attributes.size,
                    is_web_client,
                );

                session_data
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_screen(ScreenInstruction::RecomputeTabSize(
                        client_id,
                        client_attributes.size,
                    ))
                    .unwrap();
                session_data
                    .read()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_screen(ScreenInstruction::SetClientTty(
                        client_id,
                        client_attributes.tty.clone(),
                    ))
                    .unwrap();

                let default_shell = runtime_config_options.default_shell.map(|shell| {
                    TerminalAction::RunCommand(RunCommand {
                        command: shell,
                        cwd: config.options.default_cwd.clone(),
                        use_terminal_title: true,
                        ..Default::default()
                    })
                });
                let cwd = cli_assets
                    .cwd
                    .or_else(|| runtime_config_options.default_cwd);

                let spawn_tabs = |tab_layout,
                                  floating_panes_layout,
                                  tab_name,
                                  swap_layouts,
                                  should_focus_tab| {
                    session_data
                        .read()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_screen(ScreenInstruction::NewTab(
                            cwd.clone(),
                            default_shell.clone(),
                            tab_layout,
                            floating_panes_layout,
                            tab_name,
                            swap_layouts,
                            None,  // initial_panes
                            false, // block_on_first_terminal
                            should_focus_tab,
                            (client_id, is_web_client),
                            None,
                        ))
                        .unwrap()
                };

                if layout.has_tabs() {
                    let focused_tab_index = layout.focused_tab_index().unwrap_or(0);
                    for (tab_index, (tab_name, tab_layout, floating_panes_layout)) in
                        layout.tabs().into_iter().enumerate()
                    {
                        let should_focus_tab = tab_index == focused_tab_index;
                        spawn_tabs(
                            Some(tab_layout.clone()),
                            floating_panes_layout.clone(),
                            tab_name,
                            (
                                Some(layout.swap_tiled_layouts.clone()),
                                Some(layout.swap_floating_layouts.clone()),
                            ),
                            should_focus_tab,
                        );
                    }
                } else {
                    let mut floating_panes =
                        layout.template.map(|t| t.1).clone().unwrap_or_default();
                    if should_launch_setup_wizard {
                        // we only do this here (and only once) because otherwise it will be
                        // intrusive
                        let setup_wizard = setup_wizard_floating_pane();
                        floating_panes.push(setup_wizard);
                    } else if should_show_release_notes(
                        runtime_config_options.show_release_notes,
                        layout_is_welcome_screen,
                    ) {
                        let about = about_floating_pane();
                        floating_panes.push(about);
                    } else if should_show_startup_tip(
                        runtime_config_options.show_startup_tips,
                        layout_is_welcome_screen,
                    ) {
                        let tip = tip_floating_pane();
                        floating_panes.push(tip);
                    }
                    spawn_tabs(
                        None,
                        floating_panes,
                        None,
                        (
                            Some(layout.swap_tiled_layouts.clone()),
                            Some(layout.swap_floating_layouts.clone()),
                        ),
                        true,
                    );
                }
                {
                    let rlock = session_data.read().unwrap();
                    let session_data = rlock.as_ref().unwrap();
                    session_data
                        .senders
                        .send_to_plugin(PluginInstruction::AddClient(client_id))
                        .unwrap();
                    session_data
                        .senders
                        .send_to_screen(ScreenInstruction::SetClientHostTerminalEnv(
                            client_id,
                            host_terminal_env,
                        ))
                        .unwrap();
                }
            },
            ServerInstruction::AttachClient(
                cli_assets,
                tab_position_to_focus,
                pane_id_to_focus,
                is_web_client,
                client_id,
            ) => {
                let mut rlock = session_data.write().unwrap();
                let session_data = rlock.as_mut().unwrap();
                let config = session_data.session_configuration.saved_config.clone();
                let host_terminal_env = cli_assets.host_terminal_env.clone();
                let runtime_config_options = match cli_assets.configuration_options {
                    Some(configuration_options) => config.options.merge(configuration_options),
                    None => config.options.clone(),
                };

                let client_attributes = ClientAttributes {
                    size: cli_assets.terminal_window_size,
                    tty: cli_assets.tty.clone(),
                    style: Style {
                        colors: config
                            .theme_config(runtime_config_options.theme.as_ref())
                            .unwrap_or_else(|| default_palette().into()),
                        rounded_corners: config.ui.pane_frames.rounded_corners,
                        hide_session_name: config.ui.pane_frames.hide_session_name,
                    },
                };

                let mut runtime_configuration = config.clone();
                runtime_configuration.options = runtime_config_options.clone();
                session_data
                    .session_configuration
                    .set_client_runtime_configuration(client_id, runtime_configuration);

                let default_input_mode = config.options.default_mode.unwrap_or_default();
                session_data
                    .current_input_modes
                    .insert(client_id, default_input_mode);

                session_state.write().unwrap().set_client_data(
                    client_id,
                    client_attributes.size,
                    is_web_client,
                );

                session_data
                    .senders
                    .send_to_screen(ScreenInstruction::SetClientHostTerminalEnv(
                        client_id,
                        host_terminal_env,
                    ))
                    .unwrap();
                session_data
                    .senders
                    .send_to_screen(ScreenInstruction::SetClientTty(
                        client_id,
                        client_attributes.tty.clone(),
                    ))
                    .unwrap();
                session_data
                    .senders
                    .send_to_screen(ScreenInstruction::AddClient(
                        client_id,
                        is_web_client,
                        client_attributes.size,
                        tab_position_to_focus,
                        pane_id_to_focus,
                    ))
                    .unwrap();
                session_data
                    .senders
                    .send_to_plugin(PluginInstruction::AddClient(client_id))
                    .unwrap();
                let default_mode = config.options.default_mode.unwrap_or_default();
                // ModeUpdate broadcast is handled by the screen thread via
                // change_mode() -> update_input_modes()
                session_data
                    .senders
                    .send_to_screen(ScreenInstruction::ChangeMode(
                        default_mode,
                        Some(default_mode),
                        client_id,
                        None,
                    ))
                    .unwrap();
            },
            ServerInstruction::AttachWatcherClient(client_id, terminal_size, is_web_client) => {
                // the client_id was inserted into clients upon ipc tunnel initialization
                // now that it identified itself as a watcher, we need to convert it

                // Convert to watcher in SessionState (needed for input filtering in route.rs)
                session_state
                    .write()
                    .unwrap()
                    .convert_client_to_watcher(client_id, is_web_client);

                // Also notify Screen to add this as a watcher client (for rendering) with the terminal size
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_screen(ScreenInstruction::AddWatcherClient(
                        client_id,
                        terminal_size,
                    ))
                    .unwrap();
            },
            ServerInstruction::UnblockInputThread => {
                // attached clients only: this says "input is yours again", which is meaningless to
                // a `zellij action` client and, worse, is read by it as the answer to the command
                // it is waiting for. A pane or tab teardown broadcasts this from the pty thread
                // while the route thread is still assembling that command's report, and whichever
                // arrived first won - which is why `close-pane` printed `closed:` some of the time
                // and, when the dying pane held the channel, none of the time
                let client_ids = session_state.read().unwrap().attached_client_ids();
                for client_id in client_ids {
                    send_to_client!(
                        client_id,
                        os_input,
                        ServerToClientMsg::UnblockInputThread,
                        session_state,
                        session_data
                    );
                }
            },
            ServerInstruction::UnblockCliPipeInput(pipe_name) => {
                let pipe = session_state.read().unwrap().get_pipe(&pipe_name);
                match pipe {
                    Some(client_id) => {
                        send_to_client!(
                            client_id,
                            os_input,
                            ServerToClientMsg::UnblockCliPipeInput {
                                pipe_name: pipe_name.clone()
                            },
                            session_state,
                            session_data
                        );
                    },
                    None => {
                        // send to all clients, this pipe might not have been associated yet
                        let client_ids = session_state.read().unwrap().client_ids();
                        for client_id in client_ids {
                            send_to_client!(
                                client_id,
                                os_input,
                                ServerToClientMsg::UnblockCliPipeInput {
                                    pipe_name: pipe_name.clone()
                                },
                                session_state,
                                session_data
                            );
                        }
                    },
                }
            },
            ServerInstruction::CliPipeOutput(pipe_name, output) => {
                let pipe = session_state.read().unwrap().get_pipe(&pipe_name);
                match pipe {
                    Some(client_id) => {
                        send_to_client!(
                            client_id,
                            os_input,
                            ServerToClientMsg::CliPipeOutput {
                                pipe_name: pipe_name.clone(),
                                output: output.clone()
                            },
                            session_state,
                            session_data
                        );
                    },
                    None => {
                        // send to all clients, this pipe might not have been associated yet
                        let client_ids = session_state.read().unwrap().client_ids();
                        for client_id in client_ids {
                            send_to_client!(
                                client_id,
                                os_input,
                                ServerToClientMsg::CliPipeOutput {
                                    pipe_name: pipe_name.clone(),
                                    output: output.clone()
                                },
                                session_state,
                                session_data
                            );
                        }
                    },
                }
            },
            ServerInstruction::ClientExit(client_id, completion_tx) => {
                let _ = os_input.send_to_client(
                    client_id,
                    ServerToClientMsg::Exit {
                        exit_reason: ExitReason::Normal,
                    },
                );

                // Check if this is a watcher
                let is_watcher = session_state.read().unwrap().is_watcher(&client_id);
                if is_watcher {
                    // Remove from SessionState watchers set
                    session_state.write().unwrap().remove_watcher(client_id);

                    // Also notify Screen to remove watcher
                    if let Some(session_data) = session_data.write().unwrap().as_ref() {
                        let _ = session_data
                            .senders
                            .send_to_screen(ScreenInstruction::RemoveWatcherClient(client_id));
                    }

                    os_input.remove_client(client_id).unwrap();
                } else {
                    if let Some(session_data) = session_data.write().unwrap().as_mut() {
                        session_data.remove_key_passthrough_client(client_id);
                    }
                    // Handle regular client removal
                    remove_client!(client_id, os_input, session_state, session_data);
                    drop(completion_tx); // prevent deadlock with route thread
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_screen(ScreenInstruction::RemoveClient(client_id))
                        .unwrap();
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_plugin(PluginInstruction::RemoveClient(client_id))
                        .unwrap();
                    if !session_state.read().unwrap().active_clients_are_connected() {
                        // the session itself is torn down after the loop, in the same order as
                        // every other exit path -- dropping it here joined the session threads
                        // while holding the write lock, which blocks every route thread
                        let client_ids_to_cleanup: Vec<ClientId> = session_state
                            .read()
                            .unwrap()
                            .clients
                            .keys()
                            .copied()
                            .collect();
                        // these are just the pipes
                        for client_id in client_ids_to_cleanup {
                            remove_client!(client_id, os_input, session_state, session_data);
                        }

                        let watcher_client_ids: Vec<ClientId> =
                            session_state.read().unwrap().watcher_client_ids();
                        for watcher_id in watcher_client_ids {
                            let _ = os_input.send_to_client(
                                watcher_id,
                                ServerToClientMsg::Exit {
                                    exit_reason: ExitReason::Normal,
                                },
                            );
                        }

                        break;
                    }
                }
            },
            ServerInstruction::RemoveClient(client_id) => {
                // Check if this is a watcher
                let is_watcher = session_state.read().unwrap().is_watcher(&client_id);
                if is_watcher {
                    // Remove from SessionState watchers set
                    session_state.write().unwrap().remove_watcher(client_id);

                    // Also notify Screen to remove watcher
                    if let Some(session_data) = session_data.write().unwrap().as_ref() {
                        let _ = session_data
                            .senders
                            .send_to_screen(ScreenInstruction::RemoveWatcherClient(client_id));
                    }

                    os_input.remove_client(client_id).unwrap();
                } else {
                    if let Some(session_data) = session_data.write().unwrap().as_mut() {
                        session_data.remove_key_passthrough_client(client_id);
                    }
                    // Handle regular client removal
                    remove_client!(client_id, os_input, session_state, session_data);
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_screen(ScreenInstruction::RemoveClient(client_id))
                        .unwrap();
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_plugin(PluginInstruction::RemoveClient(client_id))
                        .unwrap();
                }
            },
            ServerInstruction::SendWebClientsForbidden(client_id) => {
                let _ = os_input.send_to_client(
                    client_id,
                    ServerToClientMsg::Exit {
                        exit_reason: ExitReason::WebClientsForbidden,
                    },
                );
                remove_client!(client_id, os_input, session_state, session_data);
            },
            ServerInstruction::KillSession => {
                // the archive is cut on the way out below; serialize once more first, so what it
                // captures is the shape the session had when it was killed rather than one up to a
                // whole serialization interval old
                serialize_session_before_exit(&session_data);
                let client_ids = session_state.read().unwrap().client_ids();
                for client_id in client_ids {
                    let _ = os_input.send_to_client(
                        client_id,
                        ServerToClientMsg::Exit {
                            exit_reason: ExitReason::Normal,
                        },
                    );
                    remove_client!(client_id, os_input, session_state, session_data);
                }
                break;
            },
            ServerInstruction::DisconnectAllClientsExcept(client_id) => {
                let client_ids: Vec<ClientId> = session_state
                    .read()
                    .unwrap()
                    .client_ids()
                    .iter()
                    .copied()
                    .filter(|c| c != &client_id)
                    .collect();
                for client_id in client_ids {
                    if let Some(session_data) = session_data.write().unwrap().as_mut() {
                        session_data.remove_key_passthrough_client(client_id);
                    }
                    let _ = os_input.send_to_client(
                        client_id,
                        ServerToClientMsg::Exit {
                            exit_reason: ExitReason::KickedByHost,
                        },
                    );
                    remove_client!(client_id, os_input, session_state, session_data);
                }
            },
            ServerInstruction::DetachSession(client_ids, completion_tx) => {
                for client_id in &client_ids {
                    if let Some(session_data) = session_data.write().unwrap().as_mut() {
                        session_data.remove_key_passthrough_client(*client_id);
                    }
                    let _ = os_input.send_to_client(
                        *client_id,
                        ServerToClientMsg::Exit {
                            exit_reason: ExitReason::Normal,
                        },
                    );
                    remove_client!(*client_id, os_input, session_state, session_data);
                }
                drop(completion_tx); // we do this here explicitly to signal that the clients have
                                     // already disconnected and to prevent a deadlock below caused
                                     // by us having to wait for session_data to send cleanup
                                     // signals to the various threads
                for client_id in client_ids {
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_screen(ScreenInstruction::RemoveClient(client_id))
                        .unwrap();
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_plugin(PluginInstruction::RemoveClient(client_id))
                        .unwrap();
                }
            },
            ServerInstruction::Render(serialized_output) => {
                let client_ids = session_state.read().unwrap().client_ids();
                // If `Some(_)`- unwrap it and forward it to the clients to render.
                // If `None`- Send an exit instruction. This is the case when a user closes the last Tab/Pane.
                if let Some(output) = &serialized_output {
                    for (client_id, client_render_instruction) in output.iter() {
                        send_to_client!(
                            *client_id,
                            os_input,
                            ServerToClientMsg::Render {
                                content: client_render_instruction.clone()
                            },
                            session_state,
                            session_data
                        );
                    }
                } else {
                    // Session is exiting - disconnect all regular clients
                    for client_id in client_ids {
                        let _ = os_input.send_to_client(
                            client_id,
                            ServerToClientMsg::Exit {
                                exit_reason: ExitReason::Normal,
                            },
                        );
                        remove_client!(client_id, os_input, session_state, session_data);
                    }

                    // Also disconnect all watchers
                    let watcher_ids: Vec<ClientId> = session_state
                        .read()
                        .unwrap()
                        .watchers
                        .keys()
                        .copied()
                        .collect();
                    for watcher_id in watcher_ids {
                        let _ = os_input.send_to_client(
                            watcher_id,
                            ServerToClientMsg::Exit {
                                exit_reason: ExitReason::Normal,
                            },
                        );
                        remove_client!(watcher_id, os_input, session_state, session_data);
                    }
                    break;
                }
            },
            ServerInstruction::Error(backtrace) => {
                let client_ids = session_state.read().unwrap().client_ids();
                for client_id in client_ids {
                    let _ = os_input.send_to_client(
                        client_id,
                        ServerToClientMsg::Exit {
                            exit_reason: ExitReason::Error(backtrace.clone()),
                        },
                    );
                    remove_client!(client_id, os_input, session_state, session_data);
                }
                break;
            },
            ServerInstruction::ConnStatus(client_id) => {
                let _ = os_input.send_to_client(client_id, ServerToClientMsg::Connected);
                remove_client!(client_id, os_input, session_state, session_data);
            },
            ServerInstruction::Log(
                lines_to_log,
                client_id,
                _completion_tx, // the action ends here, dropping this will release anything waiting
                                // for it
            ) => {
                send_to_client!(
                    client_id,
                    os_input,
                    ServerToClientMsg::Log {
                        lines: lines_to_log
                    },
                    session_state,
                    session_data
                );
            },
            ServerInstruction::LogError(
                lines_to_log,
                client_id,
                _completion_tx, // the action ends here, dropping this will release anything waiting
                                // for it
            ) => {
                send_to_client!(
                    client_id,
                    os_input,
                    ServerToClientMsg::LogError {
                        lines: lines_to_log
                    },
                    session_state,
                    session_data
                );
            },
            ServerInstruction::SwitchSession(mut connect_to_session, client_id, completion_tx) => {
                let current_session_name = envs::get_session_name();
                if connect_to_session.name == current_session_name.ok() {
                    log::error!("Cannot attach to same session");
                } else {
                    let layout_dir = session_data
                        .read()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .session_configuration
                        .get_client_configuration(&client_id)
                        .options
                        .layout_dir
                        .or_else(|| default_layout_dir());
                    if let Some(layout_dir) = layout_dir {
                        connect_to_session.apply_layout_dir(&layout_dir);
                    }

                    send_to_client!(
                        client_id,
                        os_input,
                        ServerToClientMsg::SwitchSession { connect_to_session },
                        session_state,
                        session_data
                    );
                    remove_client!(client_id, os_input, session_state, session_data);
                    drop(completion_tx); // do not deadlock with route thread

                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_screen(ScreenInstruction::RemoveClient(client_id))
                        .unwrap();
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_plugin(PluginInstruction::RemoveClient(client_id))
                        .unwrap();
                }
            },
            ServerInstruction::AssociatePipeWithClient { pipe_id, client_id } => {
                session_state
                    .write()
                    .unwrap()
                    .associate_pipe_with_client(pipe_id, client_id);
            },
            ServerInstruction::ChangeMode(client_id, input_mode, completion) => {
                let mut session_data = session_data.write().unwrap();
                let session_data = session_data.as_mut().unwrap();
                let base_mode = session_data
                    .session_configuration
                    .get_client_default_input_mode(&client_id);
                session_data
                    .current_input_modes
                    .insert(client_id, input_mode);
                session_data
                    .senders
                    .send_to_screen(ScreenInstruction::ChangeMode(
                        input_mode,
                        Some(base_mode),
                        client_id,
                        completion,
                    ))
                    .unwrap();
            },
            ServerInstruction::ChangeModeForAllClients(input_mode) => {
                session_data
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .change_mode_for_all_clients(input_mode);
            },
            ServerInstruction::Reconfigure {
                client_id,
                config,
                write_config_to_disk,
            } => {
                let (new_config, runtime_config_changed) = session_data
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .session_configuration
                    .reconfigure_runtime_config(&client_id, config);
                update_new_saved_config(
                    new_config,
                    write_config_to_disk,
                    runtime_config_changed,
                    &session_data,
                    client_id,
                );
            },
            ServerInstruction::ConfigWrittenToDisk(new_config) => {
                let changes = session_data
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .session_configuration
                    .change_saved_config(new_config);
                let config_was_written_to_disk = true;
                session_data
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .propagate_configuration_changes(changes, config_was_written_to_disk);
                let client_ids = session_state.read().unwrap().client_ids();
                for client_id in client_ids {
                    send_to_client!(
                        client_id,
                        os_input,
                        ServerToClientMsg::ConfigFileUpdated,
                        session_state,
                        session_data
                    );
                }
            },
            ServerInstruction::FailedToWriteConfigToDisk(_client_id, file_path) => {
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_plugin(PluginInstruction::FailedToWriteConfigToDisk { file_path })
                    .unwrap();
            },
            ServerInstruction::RebindKeys {
                client_id,
                keys_to_rebind,
                keys_to_unbind,
                write_config_to_disk,
            } => {
                let (new_config, runtime_config_changed) = session_data
                    .write()
                    .unwrap()
                    .as_mut()
                    .unwrap()
                    .session_configuration
                    .rebind_keys(&client_id, keys_to_rebind, keys_to_unbind);

                update_new_saved_config(
                    new_config,
                    write_config_to_disk,
                    runtime_config_changed,
                    &session_data,
                    client_id,
                );
            },
            ServerInstruction::StartWebServer(client_id) => {
                if cfg!(feature = "web_server_capability") {
                    send_to_client!(
                        client_id,
                        os_input,
                        ServerToClientMsg::StartWebServer,
                        session_state,
                        session_data
                    );
                } else {
                    // TODO: test this
                    log::error!("Cannot start web server: this instance of Zellij was compiled without web_server_capability");
                }
            },
            ServerInstruction::ShareCurrentSession(_client_id) => {
                if cfg!(feature = "web_server_capability") {
                    let successfully_changed = session_data
                        .write()
                        .ok()
                        .and_then(|mut s| s.as_mut().map(|s| s.web_sharing.set_sharing()))
                        .unwrap_or(false);
                    if successfully_changed {
                        session_data
                            .write()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .senders
                            .send_to_screen(ScreenInstruction::SessionSharingStatusChange(true))
                            .unwrap();
                    }
                } else {
                    log::error!("Cannot share session: this instance of Zellij was compiled without web_server_capability");
                }
            },
            ServerInstruction::StopSharingCurrentSession(_client_id) => {
                if cfg!(feature = "web_server_capability") {
                    let successfully_changed = session_data
                        .write()
                        .ok()
                        .and_then(|mut s| s.as_mut().map(|s| s.web_sharing.set_not_sharing()))
                        .unwrap_or(false);
                    if successfully_changed {
                        // disconnect existing web clients
                        let web_client_ids: Vec<ClientId> = session_state
                            .read()
                            .unwrap()
                            .web_client_ids()
                            .iter()
                            .copied()
                            .collect();
                        for client_id in web_client_ids {
                            let _ = os_input.send_to_client(
                                client_id,
                                ServerToClientMsg::Exit {
                                    exit_reason: ExitReason::WebClientsForbidden,
                                },
                            );
                            remove_client!(client_id, os_input, session_state, session_data);
                        }
                        let web_watcher_client_ids: Vec<ClientId> = session_state
                            .read()
                            .unwrap()
                            .web_watcher_client_ids()
                            .iter()
                            .copied()
                            .collect();
                        for client_id in web_watcher_client_ids {
                            let _ = os_input.send_to_client(
                                client_id,
                                ServerToClientMsg::Exit {
                                    exit_reason: ExitReason::WebClientsForbidden,
                                },
                            );
                            remove_watcher!(client_id, os_input, session_state);
                        }

                        session_data
                            .write()
                            .unwrap()
                            .as_ref()
                            .unwrap()
                            .senders
                            .send_to_screen(ScreenInstruction::SessionSharingStatusChange(false))
                            .unwrap();
                    }
                } else {
                    // TODO: test this
                    log::error!("Cannot start web server: this instance of Zellij was compiled without web_server_capability");
                }
            },
            ServerInstruction::WebServerStarted(base_url) => {
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_plugin(PluginInstruction::WebServerStarted(base_url))
                    .unwrap();
            },
            ServerInstruction::FailedToStartWebServer(error) => {
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_plugin(PluginInstruction::FailedToStartWebServer(error))
                    .unwrap();
            },
            ServerInstruction::ClearMouseHelpText(client_id) => {
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_screen(ScreenInstruction::ClearMouseHelpText(client_id))
                    .unwrap();
            },
            ServerInstruction::ClearCommandOutputFlash(pane_id) => {
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_screen(ScreenInstruction::ClearCommandOutputFlash(pane_id))
                    .unwrap();
            },
            ServerInstruction::ForwardQueryToHost(token, query_bytes, resolve_async) => {
                // Pick a regular (non-watcher) client to carry the
                // forward. Preference is the most recently active
                // client (whichever last sent input); falls back to
                // any connected client. When the host terminals of
                // attached clients differ, the recently-active one
                // is the best proxy for "what the user is currently
                // looking at".
                let target_client_id = {
                    let mut session = session_state.write().unwrap();
                    let picked = session.pick_forward_target();
                    if let Some(cid) = picked {
                        session.mark_forward_in_flight(token, cid);
                    }
                    picked
                };
                if let Some(client_id) = target_client_id {
                    send_to_client!(
                        client_id,
                        os_input,
                        ServerToClientMsg::ForwardQueryToHost {
                            token,
                            query_bytes,
                            resolve_async
                        },
                        session_state,
                        session_data
                    );
                } else {
                    // No client to ask — synthesize an empty reply
                    // so `Screen`'s in-flight slot releases. Without
                    // this, a detached session (apps still running)
                    // accumulates forwards in `forward_queue` with
                    // no way for them to drain until someone
                    // reattaches.
                    log::warn!(
                        "No connected client to forward host query (token={}); returning empty reply",
                        token
                    );
                    if let Some(session) = session_data.read().unwrap().as_ref() {
                        let _ = session.senders.send_to_screen(
                            ScreenInstruction::ForwardedReplyFromHost {
                                token,
                                reply_bytes: Vec::new(),
                            },
                        );
                    }
                }
            },
            ServerInstruction::KeyPassthroughChanged(
                client_id,
                _old_pane_id,
                new_pane_id,
                should_route,
                entered_from_direction,
                notify_guest,
            ) => {
                let mut session_data = session_data.write().unwrap();
                if let Some(session_data) = session_data.as_mut() {
                    let previous_pane = session_data
                        .key_passthrough_clients
                        .get(&client_id)
                        .copied();

                    if should_route {
                        if previous_pane != Some(new_pane_id) {
                            if let Some(previous_pane_id) = previous_pane {
                                session_data.key_passthrough_clients.remove(&client_id);
                                let pane_still_active = session_data
                                    .key_passthrough_clients
                                    .values()
                                    .any(|p| *p == previous_pane_id);
                                if !pane_still_active && notify_guest {
                                    if let crate::panes::PaneId::Terminal(terminal_id) =
                                        previous_pane_id
                                    {
                                        let frame = zellij_utils::nested_session::encode_frame(
                                            &zellij_utils::nested_session::NestedSessionMessage::FocusLost,
                                        );
                                        let _ = session_data.senders.send_to_pty_writer(
                                            PtyWriteInstruction::Write(frame, terminal_id, None),
                                        );
                                    }
                                }
                            }
                            let pane_already_active = session_data
                                .key_passthrough_clients
                                .values()
                                .any(|p| *p == new_pane_id);
                            session_data
                                .key_passthrough_clients
                                .insert(client_id, new_pane_id);
                            if !pane_already_active {
                                if let crate::panes::PaneId::Terminal(terminal_id) = new_pane_id {
                                    let frame = zellij_utils::nested_session::encode_frame(
                                        &zellij_utils::nested_session::NestedSessionMessage::FocusGained {
                                            from_direction: entered_from_direction,
                                        },
                                    );
                                    let _ = session_data.senders.send_to_pty_writer(
                                        PtyWriteInstruction::Write(frame, terminal_id, None),
                                    );
                                }
                            }
                        }
                    } else if previous_pane.is_some() {
                        session_data
                            .remove_key_passthrough_client_with_notify(client_id, notify_guest);
                    }
                }
            },
            ServerInstruction::EmitNestedSessionFrameToClient(client_id, payload_bytes) => {
                send_to_client!(
                    client_id,
                    os_input,
                    ServerToClientMsg::EmitNestedSessionFrame { payload_bytes },
                    session_state,
                    session_data
                );
            },
        }
    }

    // Nobody reads `to_server` past this point and it is bounded, so any thread that sends into it
    // while winding down would block there forever and never reach its own exit — with the session
    // threads joined below, that is a teardown that never finishes. Keep the channel drained. The
    // panic hook holds a sender for the life of the process, so this thread is left detached
    // rather than joined; it costs nothing and the process is about to go.
    let _ = thread::Builder::new()
        .name("server_teardown_drain".to_string())
        .spawn(move || while server_receiver.recv().is_ok() {});

    // Take the session out of the lock before dropping it. Its `Drop` joins every session thread,
    // and doing that while holding the write lock blocks every route thread that touches it.
    let session = session_data.write().unwrap().take();
    drop(session);

    // Archive after the last serialize and before the socket goes: the socket disappearing is what
    // `delete-session` waits on, so a snapshot cut here is on disk before anything can delete the
    // folder it was copied from.
    if let Err(e) = archive_session_info(
        &session_name,
        SnapshotReason::Shutdown,
        &snapshot_settings(),
    ) {
        log::error!(
            "Failed to archive session {:?} on exit: {}",
            session_name,
            e
        );
    }

    // The socket file goes last, once there is nothing left to tear down. It stopped answering the
    // moment the loop broke, so it advertises nothing in the meantime -- and leaving it in place
    // until here is what lets a caller treat its absence as "the session is really gone" rather
    // than "the server has begun thinking about it".
    drop(std::fs::remove_file(&socket_path));
}

/// Ask the session to serialize itself one last time, and wait for that to reach the disk.
///
/// Best effort: a session that is already tearing down, or that has serialization turned off, just
/// leaves whatever the periodic serializer last wrote.
fn serialize_session_before_exit(session_data: &Arc<RwLock<Option<SessionMetaData>>>) {
    const FINAL_SERIALIZE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    {
        let session_data = session_data.read().unwrap();
        let Some(session_data) = session_data.as_ref() else {
            return;
        };
        if session_data
            .senders
            .send_to_screen(ScreenInstruction::SaveSession(
                0,
                Some(NotificationEnd::new(completion_tx)),
            ))
            .is_err()
        {
            return;
        }
    }
    let runtime = crate::global_async_runtime::get_tokio_runtime();
    if runtime
        .block_on(async { tokio::time::timeout(FINAL_SERIALIZE_TIMEOUT, completion_rx).await })
        .is_err()
    {
        log::warn!("Timed out serializing the session before exit");
    }
}

fn init_session(
    os_input: Box<dyn ServerOsApi>,
    to_server: SenderWithContext<ServerInstruction>,
    client_attributes: ClientAttributes,
    config_options: Box<Options>,
    layout: Box<Layout>,
    cli_assets: CliAssets,
    mut config: Config,
    plugin_aliases: PluginAliases,
    client_id: ClientId,
) -> SessionMetaData {
    config.options = config.options.merge(*config_options.clone());

    let _ = SCROLL_BUFFER_SIZE.set(
        config_options
            .scroll_buffer_size
            .unwrap_or(DEFAULT_SCROLL_BUFFER_SIZE),
    );

    let (to_screen, screen_receiver): ChannelWithContext<ScreenInstruction> = channels::unbounded();
    let to_screen = SenderWithContext::new(to_screen);

    let (to_screen_bounded, bounded_screen_receiver): ChannelWithContext<ScreenInstruction> =
        channels::bounded(50);
    let to_screen_bounded = SenderWithContext::new(to_screen_bounded);

    let (to_plugin, plugin_receiver): ChannelWithContext<PluginInstruction> = channels::unbounded();
    let to_plugin = SenderWithContext::new(to_plugin);
    let (to_pty, pty_receiver): ChannelWithContext<PtyInstruction> = channels::unbounded();
    let to_pty = SenderWithContext::new(to_pty);

    let (to_pty_writer, pty_writer_receiver): ChannelWithContext<PtyWriteInstruction> =
        channels::unbounded();
    let to_pty_writer = SenderWithContext::new(to_pty_writer);

    let (to_background_jobs, background_jobs_receiver): ChannelWithContext<BackgroundJob> =
        channels::unbounded();
    let to_background_jobs = SenderWithContext::new(to_background_jobs);

    // Determine and initialize the data directory
    let data_dir = cli_assets.data_dir.unwrap_or_else(get_default_data_dir);

    let serialization_interval = config_options.serialization_interval;
    let disable_session_metadata = config_options.disable_session_metadata.unwrap_or(false);
    let web_server_ip = config_options
        .web_server_ip
        .unwrap_or_else(|| IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)));
    let web_server_port = config_options.web_server_port.unwrap_or_else(|| 8082);
    let has_certificate =
        config_options.web_server_cert.is_some() && config_options.web_server_key.is_some();
    let enforce_https_for_localhost = config_options.enforce_https_for_localhost.unwrap_or(false);

    let default_shell = config_options.default_shell.clone().map(|command| {
        TerminalAction::RunCommand(RunCommand {
            command,
            use_terminal_title: true,
            ..Default::default()
        })
    });
    let path_to_default_shell = config_options
        .default_shell
        .clone()
        .unwrap_or_else(|| get_default_shell());

    let default_mode = config_options.default_mode.unwrap_or_default();
    let default_keybinds = config.keybinds.clone();

    let pty_thread = thread::Builder::new()
        .name("pty".to_string())
        .spawn({
            let layout = layout.clone();
            let pty = Pty::new(
                Bus::new(
                    vec![pty_receiver],
                    Some(&to_screen_bounded),
                    None,
                    Some(&to_plugin),
                    Some(&to_server),
                    Some(&to_pty_writer),
                    Some(&to_background_jobs),
                    Some(os_input.clone()),
                ),
                cli_assets.is_debug,
                config_options.scrollback_editor.clone(),
                config_options.post_command_discovery_hook.clone(),
                config_options.resurrect_command_hints.clone(),
                config_options.report_pane_env.clone(),
                config_options.detect_agents,
            );

            move || pty_thread_main(pty, layout.clone()).fatal()
        })
        .unwrap();

    let screen_thread = thread::Builder::new()
        .name("screen".to_string())
        .spawn({
            let screen_bus = Bus::new(
                vec![screen_receiver, bounded_screen_receiver],
                Some(&to_screen), // there are certain occasions (eg. caching) where the screen
                // needs to send messages to itself
                Some(&to_pty),
                Some(&to_plugin),
                Some(&to_server),
                Some(&to_pty_writer),
                Some(&to_background_jobs),
                Some(os_input.clone()),
            );
            let max_panes = cli_assets.max_panes;

            let client_attributes_clone = client_attributes.clone();
            let debug = cli_assets.is_debug;
            let layout = layout.clone();
            let config = config.clone();
            move || {
                screen_thread_main(
                    screen_bus,
                    max_panes,
                    client_attributes_clone,
                    config,
                    debug,
                    layout,
                )
                .fatal();
            }
        })
        .unwrap();

    let zellij_cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let session_env_vars: std::collections::BTreeMap<String, String> = std::env::vars().collect();

    let (available_layouts, available_layout_errors) = get_available_layouts(&config_options);

    let plugin_thread = thread::Builder::new()
        .name("wasm".to_string())
        .spawn({
            let plugin_bus = Bus::new(
                vec![plugin_receiver],
                Some(&to_screen_bounded),
                Some(&to_pty),
                Some(&to_plugin),
                Some(&to_server),
                Some(&to_pty_writer),
                Some(&to_background_jobs),
                None,
            );
            let engine = get_engine();

            let layout = layout.clone();
            let default_shell = default_shell.clone();
            let layout_dir = config_options
                .layout_dir
                .clone()
                .or_else(|| default_layout_dir());
            let background_plugins = config.background_plugins.clone();
            let plugin_permissions = std::sync::Arc::new(config.plugin_permissions.clone());
            // this fork watches loaded plugin .wasm files by default - it is the point of the fork
            let plugin_watch = config_options.plugin_watch.unwrap_or(true);
            // and a built-in can be developed the same way when a directory is configured to hold
            // its .wasm - recorded here, where the config is, for the loader and the watcher both
            zellij_utils::input::plugins::set_builtin_plugin_dir(
                config_options.builtin_plugin_dir.clone(),
            );
            // the about plugin names the binary a user should grant permissions to, and `pin_exe`
            // is the config saying where that binary is kept - read here, where the config is
            let pinned_exe = configured_pinned_exe(config_options.session_service.as_ref());
            crate::plugins::plugin_loader::record_configured_pinned_exe(pinned_exe.clone());
            // the same reason for the session warnings: they are asked from Screen, whose
            // constructor takes thirty arguments already
            crate::session_warnings::record_settings(crate::session_warnings::WarningSettings {
                expect_full_disk_access: config_options.expect_full_disk_access.unwrap_or(false),
                stale_build_notice: config_options.stale_build_notice.unwrap_or(true),
                pinned_exe,
            });
            let session_env_vars = session_env_vars.clone();
            move || {
                plugin_thread_main(
                    plugin_bus,
                    engine,
                    data_dir,
                    layout,
                    layout_dir,
                    available_layouts,
                    available_layout_errors,
                    path_to_default_shell,
                    zellij_cwd,
                    session_env_vars,
                    default_shell,
                    plugin_aliases,
                    default_mode,
                    default_keybinds,
                    plugin_permissions,
                    plugin_watch,
                    background_plugins,
                    client_id,
                )
                .fatal()
            }
        })
        .unwrap();

    let pty_writer_thread = thread::Builder::new()
        .name("pty_writer".to_string())
        .spawn({
            let pty_writer_bus = Bus::new(
                vec![pty_writer_receiver],
                Some(&to_screen),
                Some(&to_pty),
                Some(&to_plugin),
                Some(&to_server),
                None,
                Some(&to_background_jobs),
                Some(os_input.clone()),
            );
            || pty_writer_main(pty_writer_bus).fatal()
        })
        .unwrap();

    let background_jobs_thread = thread::Builder::new()
        .name("background_jobs".to_string())
        .spawn({
            let background_jobs_bus = Bus::new(
                vec![background_jobs_receiver],
                Some(&to_screen),
                Some(&to_pty),
                Some(&to_plugin),
                Some(&to_server),
                Some(&to_pty_writer),
                None,
                Some(os_input.clone()),
            );
            let web_server_base_url = web_server_base_url(
                web_server_ip,
                web_server_port,
                has_certificate,
                enforce_https_for_localhost,
            );
            move || {
                background_jobs_main(
                    background_jobs_bus,
                    serialization_interval,
                    disable_session_metadata,
                    web_server_base_url,
                )
                .fatal()
            }
        })
        .unwrap();
    if let Some(config_file_path) = cli_assets.config_file_path.clone() {
        let layout_dir = config_options
            .layout_dir
            .clone()
            .or_else(|| default_layout_dir());
        let default_layout_name = config_options
            .default_layout
            .map(|l| format!("{}", l.display()));
        report_changes_in_config_file(
            config_file_path,
            cli_assets.config_dir.as_deref(),
            to_server.clone(),
        );

        // Watch layout directory for changes
        if let Some(layout_dir_path) = layout_dir {
            report_changes_in_layout_dir(
                layout_dir_path,
                default_layout_name,
                to_plugin.clone(),
                to_screen.clone(),
            );
        }
    }

    SessionMetaData {
        senders: ThreadSenders {
            to_screen: Some(to_screen),
            to_pty: Some(to_pty),
            to_plugin: Some(to_plugin),
            to_pty_writer: Some(to_pty_writer),
            to_background_jobs: Some(to_background_jobs),
            to_server: Some(to_server),
            should_silently_fail: false,
        },
        default_shell,
        session_configuration: Default::default(),
        current_input_modes: HashMap::new(),
        screen_thread: Some(screen_thread),
        pty_thread: Some(pty_thread),
        plugin_thread: Some(plugin_thread),
        pty_writer_thread: Some(pty_writer_thread),
        background_jobs_thread: Some(background_jobs_thread),
        #[cfg(feature = "web_server_capability")]
        web_sharing: config.options.web_sharing.unwrap_or(WebSharing::Off),
        #[cfg(not(feature = "web_server_capability"))]
        web_sharing: WebSharing::Disabled,
        key_passthrough_clients: HashMap::new(),
        config_file_path: cli_assets.config_file_path,
    }
}

fn setup_wizard_floating_pane() -> FloatingPaneLayout {
    let mut setup_wizard_pane = FloatingPaneLayout::new();
    let configuration = BTreeMap::from_iter([("is_setup_wizard".to_owned(), "true".to_owned())]);
    setup_wizard_pane.run = Some(Run::Plugin(RunPluginOrAlias::Alias(PluginAlias::new(
        "configuration",
        &Some(configuration),
        None,
    ))));
    setup_wizard_pane
}

fn about_floating_pane() -> FloatingPaneLayout {
    let mut about_pane = FloatingPaneLayout::new();
    let configuration = BTreeMap::from_iter([("is_release_notes".to_owned(), "true".to_owned())]);
    about_pane.run = Some(Run::Plugin(RunPluginOrAlias::Alias(PluginAlias::new(
        "about",
        &Some(configuration),
        None,
    ))));
    about_pane
}

fn tip_floating_pane() -> FloatingPaneLayout {
    let mut about_pane = FloatingPaneLayout::new();
    let configuration = BTreeMap::from_iter([("is_startup_tip".to_owned(), "true".to_owned())]);
    about_pane.run = Some(Run::Plugin(RunPluginOrAlias::Alias(PluginAlias::new(
        "about",
        &Some(configuration),
        None,
    ))));
    about_pane
}

fn should_show_release_notes(
    should_show_release_notes_config: Option<bool>,
    layout_is_welcome_screen: bool,
) -> bool {
    if layout_is_welcome_screen {
        return false;
    }
    if let Some(should_show_release_notes_config) = should_show_release_notes_config {
        if !should_show_release_notes_config {
            // if we were explicitly told not to show release notes, we don't show them,
            // otherwise we make sure we only show them if they were not seen AND we know
            // we are able to write to the cache
            return false;
        }
    }
    if ZELLIJ_SEEN_RELEASE_NOTES_CACHE_FILE.exists() {
        return false;
    } else {
        if let Some(parent) = ZELLIJ_SEEN_RELEASE_NOTES_CACHE_FILE.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&*ZELLIJ_SEEN_RELEASE_NOTES_CACHE_FILE, &[]) {
            log::error!(
                "Failed to write seen release notes indication to disk: {}",
                e
            );
            return false;
        }
        return true;
    }
}

fn should_show_startup_tip(
    should_show_startup_tip_config: Option<bool>,
    layout_is_welcome_screen: bool,
) -> bool {
    if layout_is_welcome_screen {
        false
    } else {
        should_show_startup_tip_config.unwrap_or(true)
    }
}

fn report_changes_in_config_file(
    config_file_path: PathBuf,
    config_dir: Option<&Path>,
    to_server: SenderWithContext<ServerInstruction>,
) {
    let config_dir = config_dir.map(Path::to_path_buf);
    global_async_runtime::get_tokio_runtime().spawn(async move {
        watch_config_file_changes(config_file_path, config_dir.as_deref(), move |new_config| {
            let to_server = to_server.clone();
            async move {
                let _ = to_server.send(ServerInstruction::ConfigWrittenToDisk(new_config));
            }
        })
        .await;
    });
}

fn report_changes_in_layout_dir(
    layout_dir: PathBuf,
    default_layout_name: Option<String>,
    to_plugin: SenderWithContext<PluginInstruction>,
    to_screen: SenderWithContext<ScreenInstruction>,
) {
    std::thread::spawn(move || {
        let rt = crate::global_async_runtime::get_tokio_runtime();
        rt.block_on(async move {
            watch_layout_dir_changes(
                layout_dir,
                default_layout_name,
                move |new_layouts, layout_errors| {
                    let to_plugin = to_plugin.clone();
                    let to_screen = to_screen.clone();
                    async move {
                        let _ = to_plugin.send(PluginInstruction::LayoutListUpdate(
                            new_layouts.clone(),
                            layout_errors.clone(),
                        ));
                        let _ = to_screen.send(ScreenInstruction::UpdateAvailableLayouts(
                            new_layouts,
                            layout_errors,
                        ));
                    }
                },
            )
            .await;
        });
    });
}

fn update_new_saved_config(
    new_config: Option<Config>,
    write_config_to_disk: bool,
    runtime_config_changed: bool,
    session_data: &Arc<RwLock<Option<SessionMetaData>>>,
    client_id: ClientId,
) {
    if let Some(new_config) = new_config {
        if write_config_to_disk {
            let clear_defaults = true;
            let config_file_path = session_data
                .read()
                .unwrap()
                .as_ref()
                .unwrap()
                .config_file_path
                .clone();

            let Some(config_file_path) = config_file_path.as_ref() else {
                log::error!("No config file path found.");
                session_data
                    .write()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .senders
                    .send_to_plugin(PluginInstruction::FailedToWriteConfigToDisk {
                        file_path: None,
                    })
                    .unwrap();
                return;
            };
            match Config::write_config_to_disk(
                new_config.to_string(clear_defaults),
                &config_file_path,
            ) {
                Ok(written_config) => {
                    let changes = session_data
                        .write()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .session_configuration
                        .change_saved_config(written_config);
                    let config_was_written_to_disk = true;
                    session_data
                        .write()
                        .unwrap()
                        .as_mut()
                        .unwrap()
                        .propagate_configuration_changes(changes, config_was_written_to_disk);
                },
                Err(e) => {
                    let error_path = e
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(String::new);
                    log::error!("Failed to write config to disk: {}", error_path);
                    session_data
                        .write()
                        .unwrap()
                        .as_ref()
                        .unwrap()
                        .senders
                        .send_to_plugin(PluginInstruction::FailedToWriteConfigToDisk {
                            file_path: e,
                        })
                        .unwrap();
                },
            }
        } else if runtime_config_changed {
            let config_was_written_to_disk = false;
            session_data
                .write()
                .unwrap()
                .as_mut()
                .unwrap()
                .propagate_configuration_changes(
                    vec![(client_id, new_config)],
                    config_was_written_to_disk,
                );
        }
    }
}

pub fn get_engine() -> Engine {
    log::info!("Loading plugins using Wasmi interpreter");
    Engine::default()
}

// TODO: move elsewhere
fn get_available_layouts(config_options: &Options) -> (Vec<LayoutInfo>, Vec<LayoutWithError>) {
    let layout_dir = config_options
        .layout_dir
        .clone()
        .or_else(|| default_layout_dir());
    let default_layout_name = config_options
        .default_layout
        .as_ref()
        .map(|l| format!("{}", l.display()));
    Layout::list_available_layouts(layout_dir, &default_layout_name)
}
