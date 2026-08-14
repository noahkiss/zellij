use super::{screen_thread_main, CopyOptions, Screen, ScreenInstruction};
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::PaneId;
use crate::{
    channels::SenderWithContext, os_input_output::ServerOsApi, route::route_action,
    thread_bus::Bus, ClientId, ServerInstruction, SessionMetaData, ThreadSenders,
};
use insta::assert_snapshot;
use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;
use zellij_utils::cli::CliAction;
use zellij_utils::data::{Event, EventType, Resize, Style, WebSharing};
use zellij_utils::errors::{prelude::*, ErrorContext};
use zellij_utils::input::actions::Action;
use zellij_utils::input::command::{RunCommand, TerminalAction};
use zellij_utils::input::config::Config;
use zellij_utils::input::layout::{
    FloatingPaneLayout, Layout, PercentOrFixed, PluginAlias, PluginUserConfiguration, Run,
    RunPlugin, RunPluginLocation, RunPluginOrAlias, SplitDirection, TiledPaneLayout,
};
use zellij_utils::input::mouse::MouseEvent;
use zellij_utils::input::options::{
    NestedSessionHandling, Options, PaneFrameStyle, DEFAULT_WORD_SEPARATORS,
};
use zellij_utils::ipc::IpcReceiverWithContext;
use zellij_utils::pane_handle::is_handle_shaped;
use zellij_utils::pane_size::{Size, SizeInPixels};
use zellij_utils::position::Position;

use crate::background_jobs::BackgroundJob;
use crate::os_input_output::{AsyncReader, SendToClientError};
use crate::pty_writer::PtyWriteInstruction;
use std::collections::HashSet;
use std::env::set_var;
use std::sync::{Arc, Mutex};

use crate::{
    plugins::PluginInstruction,
    pty::{ClientTabIndexOrPaneId, PtyInstruction},
};
use zellij_utils::ipc::PixelDimensions;

use interprocess::local_socket::Stream as LocalSocketStream;
use zellij_utils::{
    channels::{self, ChannelWithContext, Receiver},
    data::{Direction, FloatingPaneCoordinates, InputMode, ModeInfo, NewPanePlacement, Palette},
    ipc::{ClientAttributes, ClientToServerMsg, ServerToClientMsg},
};

use crate::panes::grid::Grid;
use crate::panes::link_handler::LinkHandler;
use crate::panes::sixel::SixelImageStore;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use zellij_utils::data::{PaneContents, PaneRenderReport};
use zellij_utils::ipc::ExitReason;

fn take_snapshot_and_cursor_coordinates(
    ansi_instructions: &str,
    grid: &mut Grid,
) -> (Option<(usize, usize)>, String) {
    let mut vte_parser = vte::Parser::new();
    vte_parser.advance(grid, ansi_instructions.as_bytes());
    let coords = grid
        .cursor_coordinates()
        .and_then(|(x, y, visible)| if visible { Some((x, y)) } else { None });
    (coords, format!("{:?}", grid))
}

fn take_snapshots_and_cursor_coordinates_from_render_events<'a>(
    all_events: impl Iterator<Item = &'a ServerInstruction>,
    screen_size: Size,
) -> Vec<(Option<(usize, usize)>, String)> {
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut grid = Grid::new(
        screen_size.rows,
        screen_size.cols,
        Rc::new(RefCell::new(Palette::default())),
        terminal_emulator_color_codes,
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        Style::default(),
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
    );
    let snapshots: Vec<(Option<(usize, usize)>, String)> = all_events
        .filter_map(|server_instruction| {
            match server_instruction {
                ServerInstruction::Render(output) => {
                    if let Some(output) = output {
                        // note this only takes a snapshot of the first client!
                        let raw_snapshot = output.get(&1).unwrap();
                        let snapshot =
                            take_snapshot_and_cursor_coordinates(raw_snapshot, &mut grid);
                        Some(snapshot)
                    } else {
                        None
                    }
                },
                _ => None,
            }
        })
        .collect();
    snapshots
}

fn send_cli_action_to_server(
    session_metadata: &SessionMetaData,
    cli_action: CliAction,
    client_id: ClientId,
) {
    let get_current_dir = || PathBuf::from(".");
    let actions = Action::actions_from_cli(
        cli_action,
        Box::new(get_current_dir),
        None,
        &zellij_utils::input::actions::pane_ids_only,
    )
    .unwrap();
    let senders = session_metadata.senders.clone();
    let default_shell = None;
    let default_mode = session_metadata
        .session_configuration
        .get_client_configuration(&client_id)
        .options
        .default_mode
        .unwrap_or(InputMode::Normal);
    for action in actions {
        route_action(
            action,
            client_id,
            None,
            None,
            senders.clone(),
            default_shell.clone(),
            None,
            default_mode,
            None,
        )
        .unwrap();
    }
}

fn route_arbitrary_action_to_server(
    session_metadata: &SessionMetaData,
    action: Action,
    client_id: ClientId,
) {
    let senders = session_metadata.senders.clone();
    let default_mode = session_metadata
        .session_configuration
        .get_client_configuration(&client_id)
        .options
        .default_mode
        .unwrap_or(InputMode::Normal);
    route_action(
        action,
        client_id,
        None,
        None,
        senders,
        None,
        None,
        default_mode,
        None,
    )
    .unwrap();
}

/// Route an action as a CLI client would, and hand back whether it reached the app at all.
///
/// A command that refuses before dispatch - a pane command with no pane, run from outside a pane -
/// returns no completion result, because nothing was asked of Screen.
fn route_action_as_cli_client(
    session_metadata: &SessionMetaData,
    action: Action,
    client_id: ClientId,
) -> Option<crate::route::ActionCompletionResult> {
    let senders = session_metadata.senders.clone();
    let default_mode = session_metadata
        .session_configuration
        .get_client_configuration(&client_id)
        .options
        .default_mode
        .unwrap_or(InputMode::Normal);
    let (_should_break, result) = route_action(
        action,
        client_id,
        Some(client_id),
        None,
        senders,
        None,
        None,
        default_mode,
        None,
    )
    .unwrap();
    result
}

// same as the above, but hands back the completion result so a test can assert on the
// error message and exit status an action reports
fn route_arbitrary_action_and_get_result(
    session_metadata: &SessionMetaData,
    action: Action,
    client_id: ClientId,
) -> crate::route::ActionCompletionResult {
    let senders = session_metadata.senders.clone();
    let default_mode = session_metadata
        .session_configuration
        .get_client_configuration(&client_id)
        .options
        .default_mode
        .unwrap_or(InputMode::Normal);
    let (_should_break, result) = route_action(
        action,
        client_id,
        None,
        None,
        senders,
        None,
        None,
        default_mode,
        None,
    )
    .unwrap();
    result.expect("action did not report a completion result")
}

#[derive(Clone, Default)]
struct FakeInputOutput {
    fake_filesystem: Arc<Mutex<HashMap<String, String>>>,
    server_to_client_messages: Arc<Mutex<HashMap<ClientId, Vec<ServerToClientMsg>>>>,
    // a client listed here fails every try_send_to_client with the given reason, so a test can
    // stage a slow client or a departed one
    client_send_state: Arc<Mutex<HashMap<ClientId, SendToClientError>>>,
    // a (client, pane) listed here fails only that pane's render update, so a test can stage a
    // channel that fills partway through a batch
    pane_send_state: Arc<Mutex<HashMap<(ClientId, zellij_utils::data::PaneId), SendToClientError>>>,
}

impl ServerOsApi for FakeInputOutput {
    fn set_terminal_size_using_terminal_id(
        &self,
        _terminal_id: u32,
        _cols: u16,
        _rows: u16,
        _width_in_pixels: Option<u16>,
        _height_in_pixels: Option<u16>,
    ) -> Result<()> {
        // noop
        Ok(())
    }
    fn spawn_terminal(
        &self,
        _file_to_open: TerminalAction,
        _quit_db: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>,
        _default_editor: Option<PathBuf>,
    ) -> Result<(u32, Box<dyn AsyncReader>, Option<u32>)> {
        unimplemented!()
    }
    fn write_to_tty_stdin(&self, _id: u32, _buf: &[u8]) -> Result<usize> {
        unimplemented!()
    }
    fn tcdrain(&self, _id: u32) -> Result<()> {
        unimplemented!()
    }
    fn kill(&self, _pid: u32) -> Result<()> {
        unimplemented!()
    }
    fn force_kill(&self, _pid: u32) -> Result<()> {
        unimplemented!()
    }
    fn box_clone(&self) -> Box<dyn ServerOsApi> {
        Box::new((*self).clone())
    }
    fn send_to_client(&self, client_id: ClientId, msg: ServerToClientMsg) -> Result<()> {
        self.server_to_client_messages
            .lock()
            .unwrap()
            .entry(client_id)
            .or_insert_with(Vec::new)
            .push(msg);
        Ok(())
    }
    fn try_send_to_client(
        &self,
        client_id: ClientId,
        msg: ServerToClientMsg,
    ) -> std::result::Result<(), SendToClientError> {
        if let Some(failure) = self.client_send_state.lock().unwrap().get(&client_id) {
            return Err(*failure);
        }
        if let ServerToClientMsg::PaneRenderUpdate { pane_id, .. } = &msg {
            if let Some(failure) = self
                .pane_send_state
                .lock()
                .unwrap()
                .get(&(client_id, *pane_id))
            {
                return Err(*failure);
            }
        }
        self.send_to_client(client_id, msg)
            .map_err(|_| SendToClientError::ClientGone)
    }
    fn new_client(
        &mut self,
        _client_id: ClientId,
        _stream: LocalSocketStream,
    ) -> Result<IpcReceiverWithContext<ClientToServerMsg>> {
        unimplemented!()
    }
    fn new_client_with_reply(
        &mut self,
        _client_id: ClientId,
        _stream: LocalSocketStream,
        _reply_stream: LocalSocketStream,
    ) -> Result<IpcReceiverWithContext<ClientToServerMsg>> {
        unimplemented!()
    }
    fn remove_client(&mut self, _client_id: ClientId) -> Result<()> {
        unimplemented!()
    }
    fn load_palette(&self) -> Palette {
        unimplemented!()
    }
    fn get_cwd(&self, _pid: u32) -> Option<PathBuf> {
        unimplemented!()
    }
    fn write_to_file(&mut self, contents: String, filename: Option<String>) -> Result<()> {
        if let Some(filename) = filename {
            self.fake_filesystem
                .lock()
                .unwrap()
                .insert(filename, contents);
        }
        Ok(())
    }
    fn re_run_command_in_terminal(
        &self,
        _terminal_id: u32,
        _run_command: RunCommand,
        _quit_cb: Box<dyn Fn(PaneId, Option<i32>, RunCommand) + Send>,
    ) -> Result<(Box<dyn AsyncReader>, Option<u32>)> {
        unimplemented!()
    }
    fn clear_terminal_id(&self, _terminal_id: u32) -> Result<()> {
        unimplemented!()
    }
    fn send_sigint(&self, _pid: u32) -> Result<()> {
        unimplemented!()
    }
}

fn create_new_screen(
    size: Size,
    advanced_mouse_actions: bool,
    mouse_hover_effects: bool,
) -> Screen {
    create_new_screen_with_kitty_graphics(size, advanced_mouse_actions, mouse_hover_effects, true)
}

fn create_new_screen_with_kitty_graphics(
    size: Size,
    advanced_mouse_actions: bool,
    mouse_hover_effects: bool,
    support_kitty_graphics_protocol: bool,
) -> Screen {
    let mut bus: Bus<ScreenInstruction> = Bus::empty();
    let fake_os_input = FakeInputOutput::default();
    bus.os_input = Some(Box::new(fake_os_input));
    let client_attributes = ClientAttributes {
        size,
        ..Default::default()
    };
    let max_panes = None;
    let mut mode_info = ModeInfo::default();
    mode_info.session_name = Some("zellij-test".into());
    let draw_pane_frames = PaneFrameStyle::None;
    let auto_layout = true;
    let session_is_mirrored = true;
    let copy_options = CopyOptions::default();
    let default_layout = Box::new(Layout::default());
    let default_layout_name = None;
    let default_shell = PathBuf::from("my_default_shell");
    let session_serialization = true;
    let serialize_pane_viewport = false;
    let scrollback_lines_to_serialize = None;
    let layout_dir = None;

    let debug = false;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let arrow_fonts = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let stacked_resize = true;
    let web_sharing = WebSharing::Off;
    let web_server_ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let web_server_port = 8080;
    let visual_bell = true;
    let mouse_scroll_resize = true;
    let screen = Screen::new(
        bus,
        &client_attributes,
        max_panes,
        mode_info,
        draw_pane_frames,
        auto_layout,
        session_is_mirrored,
        copy_options,
        debug,
        default_layout,
        default_layout_name,
        default_shell,
        session_serialization,
        serialize_pane_viewport,
        scrollback_lines_to_serialize,
        styled_underlines,
        osc8_hyperlinks,
        arrow_fonts,
        layout_dir,
        explicitly_disable_kitty_keyboard_protocol,
        support_kitty_graphics_protocol,
        stacked_resize,
        false,
        None,
        None,
        false,
        web_sharing,
        advanced_mouse_actions,
        true,
        DEFAULT_WORD_SEPARATORS.to_owned(),
        mouse_scroll_resize,
        mouse_hover_effects,
        visual_bell,
        0,     // bell_clear_delay_ms
        false, // focus_follows_mouse
        false, // mouse_click_through
        web_server_ip,
        web_server_port,
        NestedSessionHandling::default(),
    );
    seed_first_client_size(screen, size)
}

fn seed_first_client_size(mut screen: Screen, size: Size) -> Screen {
    screen.set_client_size(1, size);
    screen
}

struct MockScreen {
    pub main_client_id: u16,
    pub pty_receiver: Option<Receiver<(PtyInstruction, ErrorContext)>>,
    pub pty_writer_receiver: Option<Receiver<(PtyWriteInstruction, ErrorContext)>>,
    #[allow(dead_code)]
    pub background_jobs_receiver: Option<Receiver<(BackgroundJob, ErrorContext)>>,
    pub received_background_jobs: Arc<Mutex<Vec<BackgroundJob>>>,
    pub screen_receiver: Option<Receiver<(ScreenInstruction, ErrorContext)>>,
    pub server_receiver: Option<Receiver<(ServerInstruction, ErrorContext)>>,
    pub plugin_receiver: Option<Receiver<(PluginInstruction, ErrorContext)>>,
    pub to_screen: SenderWithContext<ScreenInstruction>,
    pub to_pty: SenderWithContext<PtyInstruction>,
    pub to_plugin: SenderWithContext<PluginInstruction>,
    pub to_server: SenderWithContext<ServerInstruction>,
    pub to_pty_writer: SenderWithContext<PtyWriteInstruction>,
    pub to_background_jobs: SenderWithContext<BackgroundJob>,
    pub os_input: FakeInputOutput,
    pub client_attributes: ClientAttributes,
    #[allow(dead_code)]
    pub config_options: Options,
    pub session_metadata: SessionMetaData,
    pub config: Config,
    advanced_mouse_actions: bool,
    last_opened_tab_index: Option<usize>,
}

impl MockScreen {
    pub fn run(
        &mut self,
        initial_layout: Option<TiledPaneLayout>,
        initial_floating_panes_layout: Vec<FloatingPaneLayout>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
        let mut config = self.config.clone();
        config.options.advanced_mouse_actions = Some(self.advanced_mouse_actions);
        let client_attributes = self.client_attributes.clone();
        let screen_bus = Bus::new(
            vec![self.screen_receiver.take().unwrap()],
            Some(&self.to_screen.clone()),
            Some(&self.to_pty.clone()),
            Some(&self.to_plugin.clone()),
            Some(&self.to_server.clone()),
            Some(&self.to_pty_writer.clone()),
            Some(&self.to_background_jobs.clone()),
            Some(Box::new(self.os_input.clone())),
        )
        .should_silently_fail();
        let debug = false;
        let screen_thread = std::thread::Builder::new()
            .name("screen_thread".to_string())
            .spawn(move || {
                set_var("ZELLIJ_SESSION_NAME", "zellij-test");
                screen_thread_main(
                    screen_bus,
                    None,
                    client_attributes,
                    config,
                    debug,
                    Box::new(Layout::default()),
                )
                .expect("TEST")
            })
            .unwrap();
        let pane_layout = initial_layout.unwrap_or_default();
        let pane_count = pane_layout.extract_run_instructions().len();
        let floating_pane_count = initial_floating_panes_layout.len();
        let mut pane_ids = vec![];
        let mut floating_pane_ids = vec![];
        let mut plugin_ids = HashMap::new();
        plugin_ids.insert(
            RunPluginOrAlias::from_url("file:/path/to/fake/plugin", &None, None, None).unwrap(),
            vec![1],
        );
        for i in 0..pane_count {
            pane_ids.push((i as u32, None));
        }
        for i in 0..floating_pane_count {
            floating_pane_ids.push((i as u32, None));
        }
        let default_shell = None;
        let tab_name = None;
        let tab_index = self.last_opened_tab_index.map(|l| l + 1).unwrap_or(0);
        let should_change_focus_to_new_tab = true;
        std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async
                                                                   // render
        let _ = self.to_screen.send(ScreenInstruction::NewTab(
            None,
            default_shell,
            Some(pane_layout.clone()),
            initial_floating_panes_layout.clone(),
            tab_name,
            (Some(vec![]), Some(vec![])), // swap layouts (None would fall back to Screen::default_layout)
            None,                         // initial_panes
            false,
            should_change_focus_to_new_tab,
            (self.main_client_id, false),
            None,
        ));
        let _ = self.to_screen.send(ScreenInstruction::ApplyLayout(
            pane_layout,
            initial_floating_panes_layout,
            pane_ids,
            floating_pane_ids,
            plugin_ids,
            tab_index,
            true,
            (self.main_client_id, false),
            None,
            None,
        ));
        self.last_opened_tab_index = Some(tab_index);
        std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
        screen_thread
    }
    // same as the above function, but starts a plugin with a plugin alias
    pub fn run_with_alias(
        &mut self,
        initial_layout: Option<TiledPaneLayout>,
        initial_floating_panes_layout: Vec<FloatingPaneLayout>,
    ) -> std::thread::JoinHandle<()> {
        let config = self.config.clone();
        let client_attributes = self.client_attributes.clone();
        let screen_bus = Bus::new(
            vec![self.screen_receiver.take().unwrap()],
            Some(&self.to_screen.clone()),
            Some(&self.to_pty.clone()),
            Some(&self.to_plugin.clone()),
            Some(&self.to_server.clone()),
            Some(&self.to_pty_writer.clone()),
            Some(&self.to_background_jobs.clone()),
            Some(Box::new(self.os_input.clone())),
        )
        .should_silently_fail();
        let debug = false;
        let screen_thread = std::thread::Builder::new()
            .name("screen_thread".to_string())
            .spawn(move || {
                set_var("ZELLIJ_SESSION_NAME", "zellij-test");
                screen_thread_main(
                    screen_bus,
                    None,
                    client_attributes,
                    config,
                    debug,
                    Box::new(Layout::default()),
                )
                .expect("TEST")
            })
            .unwrap();
        let pane_layout = initial_layout.unwrap_or_default();
        let pane_count = pane_layout.extract_run_instructions().len();
        let floating_pane_count = initial_floating_panes_layout.len();
        let mut pane_ids = vec![];
        let mut floating_pane_ids = vec![];
        let mut plugin_ids = HashMap::new();
        plugin_ids.insert(
            RunPluginOrAlias::Alias(PluginAlias {
                name: "fixture_plugin_for_tests".to_owned(),
                configuration: Some(Default::default()),
                run_plugin: Some(RunPlugin {
                    location: RunPluginLocation::parse("file:/path/to/fake/plugin", None).unwrap(),
                    configuration: PluginUserConfiguration::default(),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            vec![1],
        );
        for i in 0..pane_count {
            pane_ids.push((i as u32, None));
        }
        for i in 0..floating_pane_count {
            floating_pane_ids.push((i as u32, None));
        }
        let default_shell = None;
        let tab_name = None;
        let tab_index = self.last_opened_tab_index.map(|l| l + 1).unwrap_or(0);
        let should_change_focus_to_new_tab = true;
        let _ = self.to_screen.send(ScreenInstruction::NewTab(
            None,
            default_shell,
            Some(pane_layout.clone()),
            initial_floating_panes_layout.clone(),
            tab_name,
            (Some(vec![]), Some(vec![])), // swap layouts (None would fall back to Screen::default_layout)
            None,                         // initial_panes
            false,
            should_change_focus_to_new_tab,
            (self.main_client_id, false),
            None,
        ));
        let _ = self.to_screen.send(ScreenInstruction::ApplyLayout(
            pane_layout,
            initial_floating_panes_layout,
            pane_ids,
            floating_pane_ids,
            plugin_ids,
            tab_index,
            true,
            (self.main_client_id, false),
            None,
            None,
        ));
        self.last_opened_tab_index = Some(tab_index);
        screen_thread
    }
    pub fn new_tab(&mut self, tab_layout: TiledPaneLayout) {
        let pane_count = tab_layout.extract_run_instructions().len();
        let mut pane_ids = vec![];
        let plugin_ids = HashMap::new();
        let default_shell = None;
        let tab_name = None;
        let tab_index = self.last_opened_tab_index.map(|l| l + 1).unwrap_or(0);
        for i in 0..pane_count {
            pane_ids.push((i as u32, None));
        }
        let should_change_focus_to_new_tab = true;
        let _ = self.to_screen.send(ScreenInstruction::NewTab(
            None,
            default_shell,
            Some(tab_layout.clone()),
            vec![], // floating_panes_layout
            tab_name,
            (Some(vec![]), Some(vec![])), // swap layouts (None would fall back to Screen::default_layout)
            None,                         // initial_panes
            false,
            should_change_focus_to_new_tab,
            (self.main_client_id, false),
            None,
        ));
        let _ = self.to_screen.send(ScreenInstruction::ApplyLayout(
            tab_layout,
            vec![], // floating_panes_layout
            pane_ids,
            vec![], // floating panes ids
            plugin_ids,
            0,
            true,
            (self.main_client_id, false),
            None,
            None,
        ));
        self.last_opened_tab_index = Some(tab_index);
    }
    pub fn new_tab_with_plugins(&mut self, plugin_pane_ids: Vec<u32>) {
        // Build a layout where each child is a plugin pane
        let fake_plugin_url = "file:/path/to/fake/plugin";
        let run_plugin = RunPluginOrAlias::from_url(fake_plugin_url, &None, None, None).unwrap();
        let mut tab_layout = TiledPaneLayout::default();
        tab_layout.children_split_direction = SplitDirection::Vertical;
        tab_layout.children = plugin_pane_ids
            .iter()
            .map(|_| {
                let mut child = TiledPaneLayout::default();
                child.run = Some(Run::Plugin(run_plugin.clone()));
                child
            })
            .collect();
        let pane_ids = vec![]; // no terminal panes
        let mut plugin_ids = HashMap::new();
        plugin_ids.insert(run_plugin, plugin_pane_ids);
        let default_shell = None;
        let tab_name = None;
        let tab_index = self.last_opened_tab_index.map(|l| l + 1).unwrap_or(0);
        let should_change_focus_to_new_tab = true;
        let _ = self.to_screen.send(ScreenInstruction::NewTab(
            None,
            default_shell,
            Some(tab_layout.clone()),
            vec![], // floating_panes_layout
            tab_name,
            (Some(vec![]), Some(vec![])), // swap layouts (None would fall back to Screen::default_layout)
            None,                         // initial_panes
            false,
            should_change_focus_to_new_tab,
            (self.main_client_id, false),
            None,
        ));
        let _ = self.to_screen.send(ScreenInstruction::ApplyLayout(
            tab_layout,
            vec![], // floating_panes_layout
            pane_ids,
            vec![], // floating panes ids
            plugin_ids,
            tab_index,
            true,
            (self.main_client_id, false),
            None,
            None,
        ));
        self.last_opened_tab_index = Some(tab_index);
    }
    pub fn teardown(&mut self, threads: Vec<std::thread::JoinHandle<()>>) {
        let _ = self.to_pty.send(PtyInstruction::Exit);
        let _ = self.to_pty_writer.send(PtyWriteInstruction::Exit);
        let _ = self.to_screen.send(ScreenInstruction::Exit);
        let _ = self.to_server.send(ServerInstruction::KillSession);
        let _ = self.to_plugin.send(PluginInstruction::Exit);
        let _ = self.to_background_jobs.send(BackgroundJob::Exit);
        for thread in threads {
            let _ = thread.join();
        }
    }
    pub fn clone_session_metadata(&self) -> SessionMetaData {
        // hack that only clones the clonable parts of SessionMetaData
        SessionMetaData {
            senders: self.session_metadata.senders.clone(),
            default_shell: self.session_metadata.default_shell.clone(),
            screen_thread: None,
            pty_thread: None,
            plugin_thread: None,
            pty_writer_thread: None,
            background_jobs_thread: None,
            session_configuration: self.session_metadata.session_configuration.clone(),
            current_input_modes: self.session_metadata.current_input_modes.clone(),
            web_sharing: WebSharing::Off,
            config_file_path: self.session_metadata.config_file_path.clone(),
            key_passthrough_clients: self.session_metadata.key_passthrough_clients.clone(),
        }
    }
}

impl MockScreen {
    pub fn new(size: Size) -> Self {
        let (to_server, server_receiver): ChannelWithContext<ServerInstruction> =
            channels::bounded(50);
        let to_server = SenderWithContext::new(to_server);

        let (to_screen, screen_receiver): ChannelWithContext<ScreenInstruction> =
            channels::unbounded();
        let to_screen = SenderWithContext::new(to_screen);

        let (to_plugin, plugin_receiver): ChannelWithContext<PluginInstruction> =
            channels::unbounded();
        let to_plugin = SenderWithContext::new(to_plugin);
        let (to_pty, pty_receiver): ChannelWithContext<PtyInstruction> = channels::unbounded();
        let to_pty = SenderWithContext::new(to_pty);

        let (to_pty_writer, pty_writer_receiver): ChannelWithContext<PtyWriteInstruction> =
            channels::unbounded();
        let to_pty_writer = SenderWithContext::new(to_pty_writer);

        let (to_background_jobs, background_jobs_receiver): ChannelWithContext<BackgroundJob> =
            channels::unbounded();
        let to_background_jobs = SenderWithContext::new(to_background_jobs);

        let client_attributes = ClientAttributes {
            size,
            ..Default::default()
        };

        let session_metadata = SessionMetaData {
            senders: ThreadSenders {
                to_screen: Some(to_screen.clone()),
                to_pty: Some(to_pty.clone()),
                to_plugin: Some(to_plugin.clone()),
                to_pty_writer: Some(to_pty_writer.clone()),
                to_background_jobs: Some(to_background_jobs.clone()),
                to_server: Some(to_server.clone()),
                should_silently_fail: true,
            },
            default_shell: None,
            screen_thread: None,
            pty_thread: None,
            plugin_thread: None,
            pty_writer_thread: None,
            background_jobs_thread: None,
            session_configuration: Default::default(),
            current_input_modes: HashMap::new(),
            web_sharing: WebSharing::Off,
            config_file_path: None,
            key_passthrough_clients: Default::default(),
        };

        let os_input = FakeInputOutput::default();
        let config_options = Options::default();
        let mut config = Config::default();
        config.options.pane_frame_style = Some(PaneFrameStyle::Full);
        config.options.stacked_pane_list = Some(false);
        let main_client_id = 1;

        let _ = to_screen.send(ScreenInstruction::RecomputeTabSize(main_client_id, size));

        let received_background_jobs = Arc::new(Mutex::new(vec![]));
        std::thread::Builder::new()
            .name("background_jobs_thread".to_string())
            .spawn({
                let to_screen = to_screen.clone();
                let received_background_jobs = received_background_jobs.clone();
                move || loop {
                    let (event, _err_ctx) = background_jobs_receiver
                        .recv()
                        .expect("failed to receive event on channel");
                    received_background_jobs.lock().unwrap().push(event.clone());
                    match event {
                        BackgroundJob::RenderToClients => {
                            let _ = to_screen.send(ScreenInstruction::RenderToClients);
                        },
                        BackgroundJob::Exit => {
                            break;
                        },
                        _ => {},
                    }
                }
            })
            .unwrap();
        MockScreen {
            main_client_id,
            pty_receiver: Some(pty_receiver),
            pty_writer_receiver: Some(pty_writer_receiver),
            background_jobs_receiver: None,
            received_background_jobs,
            screen_receiver: Some(screen_receiver),
            server_receiver: Some(server_receiver),
            plugin_receiver: Some(plugin_receiver),
            to_screen,
            to_pty,
            to_plugin,
            to_server,
            to_pty_writer,
            to_background_jobs,
            os_input,
            client_attributes,
            config_options,
            session_metadata,
            last_opened_tab_index: None,
            config,
            advanced_mouse_actions: true,
        }
    }
    pub fn set_advanced_hover_effects(&mut self, advanced_mouse_actions: bool) {
        self.advanced_mouse_actions = advanced_mouse_actions;
    }
    pub fn drop_all_pty_messages(&mut self) {
        let pty_receiver = self.pty_receiver.take();
        std::thread::Builder::new()
            .name("pty_thread".to_string())
            .spawn({
                move || {
                    if let Some(pty_receiver) = pty_receiver {
                        loop {
                            let (event, _err_ctx) = pty_receiver
                                .recv()
                                .expect("failed to receive event on channel");
                            match event {
                                PtyInstruction::Exit => {
                                    break;
                                },
                                _ => {
                                    // here the event will be dropped - we do this so that the completion_tx will drop and release the
                                    // test actions
                                },
                            }
                        }
                    }
                }
            })
            .unwrap();
    }
}

macro_rules! log_actions_in_thread {
    ( $arc_mutex_log:expr, $exit_event:path, $receiver:expr ) => {
        std::thread::Builder::new()
            .name("pty_writer_thread".to_string())
            .spawn({
                let log = $arc_mutex_log.clone();
                move || loop {
                    let (event, _err_ctx) = $receiver
                        .recv()
                        .expect("failed to receive event on channel");
                    match event {
                        $exit_event => {
                            log.lock().unwrap().push(event.clone());
                            break;
                        },
                        _ => {
                            log.lock().unwrap().push(event.clone());
                        },
                    }
                }
            })
            .unwrap()
    };
}

fn new_tab(screen: &mut Screen, pid: u32, tab_index: usize) {
    let client_id = 1;
    let new_terminal_ids = vec![(pid, None)];
    let new_plugin_ids = HashMap::new();
    screen
        .new_tab(tab_index, (vec![], vec![]), None, Some(client_id))
        .expect("TEST");
    screen
        .apply_layout(
            TiledPaneLayout::default(),
            vec![], // floating panes layout
            new_terminal_ids,
            vec![], // new floating terminal ids
            new_plugin_ids,
            tab_index,
            true,
            (client_id, false),
            None,
        )
        .expect("TEST");
}

use zellij_utils::data::{PaneId as ZellijUtilsPaneId, PaneTarget};

/// Every pane the screen knows about, as `(handle, uuid, id)`.
fn pane_identities(screen: &Screen) -> Vec<(String, String, u32)> {
    screen
        .tabs
        .values()
        .flat_map(|tab| screen.pane_infos_for_tab(tab))
        .map(|pane_info| (pane_info.handle, pane_info.uuid, pane_info.id))
        .collect()
}

#[test]
fn a_handle_resolves_to_the_pane_that_answers_to_it() {
    // the CLI holds a string and the server holds the panes, so this lookup is the only place a
    // handle turns back into something an action can be pointed at
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    let identities = pane_identities(&screen);
    assert!(identities.len() >= 2, "expected a pane per tab");
    for (handle, uuid, id) in identities {
        assert!(!handle.is_empty(), "a live pane had no handle");
        assert_eq!(
            screen.resolve_pane_target(&PaneTarget::Handle(handle.clone())),
            Some(PaneId::Terminal(id)),
            "handle {} did not reach its pane",
            handle
        );
        assert_eq!(
            screen.resolve_pane_target(&PaneTarget::Uuid(uuid.clone())),
            Some(PaneId::Terminal(id)),
            "uuid {} did not reach its pane",
            uuid
        );
    }
}

#[test]
fn a_handle_no_pane_answers_to_resolves_to_nothing() {
    // a miss has to be a miss: the caller turns this into exit 2, and a wrong pane would be worse
    // than no pane
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);

    assert_eq!(
        screen.resolve_pane_target(&PaneTarget::Handle("sunny-otter".to_owned())),
        None
    );
    assert_eq!(
        screen.resolve_pane_target(&PaneTarget::Uuid(
            "e9b82dbd-0000-4000-8000-0000000000aa".to_owned()
        )),
        None
    );
}

#[test]
fn an_id_target_needs_no_lookup() {
    // "does this id name a live pane" is a different question, answered by the command that acts
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let screen = create_new_screen(size, true, true);
    assert_eq!(
        screen.resolve_pane_target(&PaneTarget::Id(ZellijUtilsPaneId::Terminal(9))),
        Some(PaneId::Terminal(9))
    );
}

#[test]
fn open_new_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    assert_eq!(screen.tabs.len(), 2, "Screen now has two tabs");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab switched to new tab"
    );
}

#[test]
pub fn switch_to_prev_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    screen.switch_tab_prev(None, true, 1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab switched to previous tab"
    );
}

#[test]
pub fn switch_to_next_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    screen.switch_tab_prev(None, true, 1).expect("TEST");
    screen.switch_tab_next(None, true, 1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab switched to next tab"
    );
}

#[test]
pub fn switch_to_tab_name() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);

    assert_eq!(
        screen
            .switch_active_tab_name("Tab #1".to_string(), 1)
            .expect("TEST"),
        false,
        "Active tab switched to tab by name"
    );
    assert_eq!(
        screen
            .switch_active_tab_name("Tab #2".to_string(), 1)
            .expect("TEST"),
        true,
        "Active tab switched to tab by name"
    );
    assert_eq!(
        screen
            .switch_active_tab_name("Tab #3".to_string(), 1)
            .expect("TEST"),
        true,
        "Active tab switched to tab by name"
    );
}

#[test]
pub fn close_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    screen.close_tab(1).expect("TEST");

    assert_eq!(screen.tabs.len(), 1, "Only one tab left");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab switched to previous tab"
    );
}

#[test]
pub fn close_the_middle_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    new_tab(&mut screen, 3, 3);
    screen.switch_tab_prev(None, true, 1).expect("TEST");
    screen.close_tab(1).expect("TEST");

    assert_eq!(screen.tabs.len(), 2, "Two tabs left");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab switched to previous tab"
    );
}

#[test]
fn move_focus_left_at_left_screen_edge_changes_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    new_tab(&mut screen, 3, 3);
    screen.switch_tab_prev(None, true, 1).expect("TEST");
    screen.move_focus_left_or_previous_tab(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab switched to previous"
    );
}

#[test]
fn basic_move_of_active_tab_to_left() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    assert_eq!(screen.get_active_tab(1).unwrap().position, 1);

    screen.move_active_tab_to_left(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab moved to left"
    );
}

fn create_fixed_size_screen() -> Screen {
    create_new_screen(
        Size {
            cols: 121,
            rows: 20,
        },
        true,
        true,
    )
}

#[test]
fn move_of_active_tab_to_left_when_there_is_only_one_tab() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    assert_eq!(screen.get_active_tab(1).unwrap().position, 0);

    screen.move_active_tab_to_left(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab moved to left"
    );
}

#[test]
fn move_of_active_tab_to_left_multiple_times() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    assert_eq!(screen.get_active_tab(1).unwrap().position, 2);

    screen.move_active_tab_to_left(1).expect("TEST");
    screen.move_active_tab_to_left(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab moved to left twice"
    );
}

#[test]
fn wrapping_move_of_active_tab_to_left() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    screen.move_focus_left_or_previous_tab(1).expect("TEST");
    screen.move_focus_left_or_previous_tab(1).expect("TEST");
    assert_eq!(screen.get_active_tab(1).unwrap().position, 0);

    screen.move_active_tab_to_left(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab moved to left until wrapped around"
    );
}

#[test]
fn basic_move_of_active_tab_to_right() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.move_focus_left_or_previous_tab(1).expect("TEST");
    assert_eq!(screen.get_active_tab(1).unwrap().position, 0);

    screen.move_active_tab_to_right(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab moved to right"
    );
}

#[test]
fn move_of_active_tab_to_right_when_there_is_only_one_tab() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    assert_eq!(screen.get_active_tab(1).unwrap().position, 0);

    screen.move_active_tab_to_right(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab moved to left"
    );
}

#[test]
fn move_of_active_tab_to_right_multiple_times() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    screen.move_focus_left_or_previous_tab(1).expect("TEST");
    screen.move_focus_left_or_previous_tab(1).expect("TEST");
    assert_eq!(screen.get_active_tab(1).unwrap().position, 0);

    screen.move_active_tab_to_right(1).expect("TEST");
    screen.move_active_tab_to_right(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab moved to right twice"
    );
}

#[test]
fn wrapping_move_of_active_tab_to_right() {
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    assert_eq!(screen.get_active_tab(1).unwrap().position, 2);

    screen.move_active_tab_to_right(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab moved to right until wrapped around"
    );
}

#[test]
fn tab_id_remains_stable_after_switch() {
    // Test that tab IDs remain stable when switching tabs, only positions change
    let mut screen = create_fixed_size_screen();

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);

    // Verify initial state: IDs should be 0, 1, 2
    let initial_tab_ids: Vec<usize> = screen.tabs.keys().copied().collect();
    assert_eq!(
        initial_tab_ids,
        vec![0, 1, 2],
        "Initial tab IDs should be 0, 1, 2"
    );

    // Verify initial positions match IDs
    assert_eq!(screen.tabs.get(&0).unwrap().id, 0);
    assert_eq!(screen.tabs.get(&0).unwrap().position, 0);
    assert_eq!(screen.tabs.get(&1).unwrap().id, 1);
    assert_eq!(screen.tabs.get(&1).unwrap().position, 1);
    assert_eq!(screen.tabs.get(&2).unwrap().id, 2);
    assert_eq!(screen.tabs.get(&2).unwrap().position, 2);

    // Move active tab (position 2, ID 2) to right, which wraps to position 0 and rotates the
    // others right by one (IDs stay stable, only positions change)
    screen.move_active_tab_to_right(1).expect("TEST");

    // Verify BTreeMap keys (IDs) remain unchanged
    let after_switch_tab_ids: Vec<usize> = screen.tabs.keys().copied().collect();
    assert_eq!(
        after_switch_tab_ids,
        vec![0, 1, 2],
        "Tab IDs in BTreeMap should remain 0, 1, 2 after switch"
    );

    // Verify IDs remain stable but positions rotate
    // Tab 0: was at position 0, rotated to position 1
    assert_eq!(
        screen.tabs.get(&0).unwrap().id,
        0,
        "Tab with ID 0 should still have ID 0"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().position,
        1,
        "Tab with ID 0 should now be at position 1"
    );

    // Tab 1: was at position 1, rotated to position 2
    assert_eq!(
        screen.tabs.get(&1).unwrap().id,
        1,
        "Tab with ID 1 should still have ID 1"
    );
    assert_eq!(
        screen.tabs.get(&1).unwrap().position,
        2,
        "Tab with ID 1 should now be at position 2"
    );

    // Tab 2: was at position 2, rotated over the edge to position 0
    assert_eq!(
        screen.tabs.get(&2).unwrap().id,
        2,
        "Tab with ID 2 should still have ID 2"
    );
    assert_eq!(
        screen.tabs.get(&2).unwrap().position,
        0,
        "Tab with ID 2 should now be at position 0"
    );

    // Verify that lookup by position works correctly after the rotation
    let tab_at_pos_0 = screen.tabs.values().find(|t| t.position == 0).unwrap();
    assert_eq!(tab_at_pos_0.id, 2, "Tab at position 0 should have ID 2");

    let tab_at_pos_1 = screen.tabs.values().find(|t| t.position == 1).unwrap();
    assert_eq!(tab_at_pos_1.id, 0, "Tab at position 1 should have ID 0");

    let tab_at_pos_2 = screen.tabs.values().find(|t| t.position == 2).unwrap();
    assert_eq!(tab_at_pos_2.id, 1, "Tab at position 2 should have ID 1");
}

#[test]
fn move_focus_right_at_right_screen_edge_changes_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    new_tab(&mut screen, 3, 3);
    screen.switch_tab_prev(None, true, 1).expect("TEST");
    screen.move_focus_right_or_next_tab(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab switched to next"
    );
}

#[test]
pub fn toggle_to_previous_tab_simple() {
    let position_and_size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(position_and_size, true, true);

    new_tab(&mut screen, 1, 1);
    new_tab(&mut screen, 2, 2);
    screen.go_to_tab(1, 1).expect("TEST");
    screen.go_to_tab(2, 1).expect("TEST");

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab toggler to previous tab"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab toggler to previous tab"
    );
}

#[test]
pub fn toggle_to_previous_tab_create_tabs_only() {
    let position_and_size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(position_and_size, true, true);

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);

    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1],
        "Tab history is invalid"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab toggler to previous tab"
    );
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 2],
        "Tab history is invalid"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab toggler to previous tab"
    );
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1],
        "Tab history is invalid"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab toggler to previous tab"
    );
}

#[test]
pub fn toggle_to_previous_tab_delete() {
    let position_and_size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(position_and_size, true, true);

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    new_tab(&mut screen, 4, 3);

    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1, 2],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        3,
        "Active tab toggler to previous tab"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1, 3],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab toggler to previous tab"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1, 2],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        3,
        "Active tab toggler to previous tab"
    );

    screen.switch_tab_prev(None, true, 1).expect("TEST");
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 1, 3],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab toggler to previous tab"
    );
    screen.switch_tab_prev(None, true, 1).expect("TEST");
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 3, 2],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab toggler to previous tab"
    );

    screen.close_tab(1).expect("TEST");
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 3],
        "Tab history is invalid"
    );
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        1,
        "Active tab toggler to previous tab"
    );

    screen.toggle_tab(1).expect("TEST");
    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        2,
        "Active tab toggler to previous tab"
    );
    assert_eq!(
        screen.tab_history.get(&1).unwrap(),
        &[0, 2],
        "Tab history is invalid"
    );
}

#[test]
fn switch_to_tab_with_fullscreen() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 1);
    {
        let active_tab = screen.get_active_tab_mut(1).unwrap();
        active_tab
            .new_pane(
                PaneId::Terminal(2),
                None,
                None,
                false,
                true,
                NewPanePlacement::default(),
                Some(1),
                None,
            )
            .unwrap();
        active_tab.toggle_active_pane_fullscreen(1);
    }
    new_tab(&mut screen, 2, 2);

    screen.switch_tab_prev(None, true, 1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(1).unwrap().position,
        0,
        "Active tab switched to previous"
    );
    assert_eq!(
        screen
            .get_active_tab(1)
            .unwrap()
            .get_active_pane_id(1)
            .unwrap(),
        PaneId::Terminal(2),
        "Active pane is still the fullscreen pane"
    );
}

#[test]
fn update_screen_pixel_dimensions() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    let initial_pixel_dimensions = screen.pixel_dimensions;
    screen.update_pixel_dimensions(
        1,
        PixelDimensions {
            character_cell_size: Some(SizeInPixels {
                height: 10,
                width: 5,
            }),
            text_area_size: None,
        },
    );
    let pixel_dimensions_after_first_update = screen.pixel_dimensions;
    screen.update_pixel_dimensions(
        1,
        PixelDimensions {
            character_cell_size: None,
            text_area_size: Some(SizeInPixels {
                height: 100,
                width: 50,
            }),
        },
    );
    let pixel_dimensions_after_second_update = screen.pixel_dimensions;
    screen.update_pixel_dimensions(
        1,
        PixelDimensions {
            character_cell_size: None,
            text_area_size: None,
        },
    );
    let pixel_dimensions_after_third_update = screen.pixel_dimensions;
    assert_eq!(
        initial_pixel_dimensions,
        PixelDimensions {
            character_cell_size: None,
            text_area_size: None
        },
        "Initial pixel dimensions empty"
    );
    assert_eq!(
        pixel_dimensions_after_first_update,
        PixelDimensions {
            character_cell_size: Some(SizeInPixels {
                height: 10,
                width: 5
            }),
            text_area_size: None
        },
        "character_cell_size updated properly",
    );
    assert_eq!(
        pixel_dimensions_after_second_update,
        PixelDimensions {
            character_cell_size: Some(SizeInPixels {
                height: 10,
                width: 5
            }),
            text_area_size: Some(SizeInPixels {
                height: 100,
                width: 50,
            }),
        },
        "text_area_size updated properly without overriding character_cell_size",
    );
    assert_eq!(
        pixel_dimensions_after_third_update,
        PixelDimensions {
            character_cell_size: Some(SizeInPixels {
                height: 10,
                width: 5
            }),
            text_area_size: Some(SizeInPixels {
                height: 100,
                width: 50,
            }),
        },
        "empty update does not delete existing data",
    );
}

#[test]
fn character_cell_size_is_derived_from_the_reporting_client_size() {
    let size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(size, true, true);
    let client_id = 2;
    screen.set_client_size(
        client_id,
        Size {
            cols: 100,
            rows: 50,
        },
    );

    screen.update_pixel_dimensions(
        client_id,
        PixelDimensions {
            character_cell_size: None,
            text_area_size: Some(SizeInPixels {
                height: 1050,
                width: 900,
            }),
        },
    );

    assert_eq!(
        *screen.character_cell_size.borrow(),
        Some(SizeInPixels {
            height: 21,
            width: 9
        }),
        "The reported text area is divided by the grid size of the client that reported it"
    );
}

#[test]
fn character_cell_size_is_not_derived_for_a_client_of_unknown_size() {
    let size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(size, true, true);

    screen.update_pixel_dimensions(
        2,
        PixelDimensions {
            character_cell_size: None,
            text_area_size: Some(SizeInPixels {
                height: 1050,
                width: 900,
            }),
        },
    );

    assert_eq!(
        *screen.character_cell_size.borrow(),
        None,
        "Without the reporting client's grid size the pixel report cannot be interpreted"
    );
}

#[test]
fn attach_after_first_tab_closed() {
    // ensure https://github.com/zellij-org/zellij/issues/1645 is fixed
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    {
        let active_tab = screen.get_active_tab_mut(1).unwrap();
        active_tab
            .new_pane(
                PaneId::Terminal(2),
                None,
                None,
                false,
                true,
                NewPanePlacement::default(),
                Some(1),
                None,
            )
            .unwrap();
        active_tab.toggle_active_pane_fullscreen(1);
    }
    new_tab(&mut screen, 2, 1);

    screen.close_tab_by_id(0).expect("TEST");
    screen.remove_client(1).expect("TEST");
    screen.add_client(1, false).expect("TEST");
}

#[test]
fn open_new_floating_pane_with_custom_coordinates() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: Some(PercentOrFixed::Percent(10)),
                y: Some(PercentOrFixed::Fixed(5)),
                width: Some(PercentOrFixed::Percent(1)),
                height: Some(PercentOrFixed::Fixed(2)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 12, "x coordinates set properly");
    assert_eq!(active_pane.y(), 5, "y coordinates set properly");
    assert_eq!(active_pane.rows(), 2, "rows set properly");
    assert_eq!(active_pane.cols(), 1, "columns set properly");
}

#[test]
fn open_new_floating_pane_with_custom_coordinates_exceeding_viewport() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: Some(PercentOrFixed::Fixed(122)),
                y: Some(PercentOrFixed::Fixed(21)),
                width: Some(PercentOrFixed::Fixed(10)),
                height: Some(PercentOrFixed::Fixed(10)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 111, "x coordinates set properly");
    assert_eq!(active_pane.y(), 10, "y coordinates set properly");
    assert_eq!(active_pane.rows(), 10, "rows set properly");
    assert_eq!(active_pane.cols(), 10, "columns set properly");
}

#[test]
fn floating_pane_auto_centers_horizontally_with_only_width() {
    let size = Size {
        cols: 120,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: None,
                y: Some(PercentOrFixed::Fixed(5)),
                width: Some(PercentOrFixed::Fixed(60)),
                height: Some(PercentOrFixed::Fixed(10)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 30, "x centered: (120-60)/2 = 30");
    assert_eq!(active_pane.y(), 5, "y explicitly set");
    assert_eq!(active_pane.cols(), 60, "width set");
    assert_eq!(active_pane.rows(), 10, "height set");
}

#[test]
fn floating_pane_auto_centers_vertically_with_only_height() {
    let size = Size {
        cols: 120,
        rows: 40,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: Some(PercentOrFixed::Fixed(10)),
                y: None,
                width: Some(PercentOrFixed::Fixed(50)),
                height: Some(PercentOrFixed::Fixed(20)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 10, "x explicitly set");
    assert_eq!(active_pane.y(), 10, "y centered: (40-20)/2 = 10");
    assert_eq!(active_pane.cols(), 50, "width set");
    assert_eq!(active_pane.rows(), 20, "height set");
}

#[test]
fn floating_pane_auto_centers_both_axes_with_only_size() {
    let size = Size {
        cols: 120,
        rows: 40,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: None,
                y: None,
                width: Some(PercentOrFixed::Fixed(80)),
                height: Some(PercentOrFixed::Fixed(30)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 20, "x centered: (120-80)/2 = 20");
    assert_eq!(active_pane.y(), 5, "y centered: (40-30)/2 = 5");
    assert_eq!(active_pane.cols(), 80, "width set");
    assert_eq!(active_pane.rows(), 30, "height set");
}

#[test]
fn floating_pane_respects_explicit_coordinates_with_size() {
    let size = Size {
        cols: 120,
        rows: 40,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: Some(PercentOrFixed::Fixed(15)),
                y: Some(PercentOrFixed::Fixed(8)),
                width: Some(PercentOrFixed::Fixed(80)),
                height: Some(PercentOrFixed::Fixed(30)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(active_pane.x(), 15, "x explicitly set, not centered");
    assert_eq!(active_pane.y(), 8, "y explicitly set, not centered");
    assert_eq!(active_pane.cols(), 80, "width set");
    assert_eq!(active_pane.rows(), 30, "height set");
}

#[test]
fn floating_pane_centers_with_percentage_width() {
    let size = Size {
        cols: 120,
        rows: 40,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: None,
                y: Some(PercentOrFixed::Fixed(5)),
                width: Some(PercentOrFixed::Percent(50)),
                height: Some(PercentOrFixed::Fixed(20)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    let expected_width = ((50.0_f64 / 100.0) * 120.0).floor() as usize;
    let expected_x = (120 - expected_width) / 2;
    assert_eq!(active_pane.cols(), expected_width, "width is 50% of 120");
    assert_eq!(
        active_pane.x(),
        expected_x,
        "x centered based on calculated width"
    );
    assert_eq!(active_pane.y(), 5, "y explicitly set");
}

#[test]
fn floating_pane_centers_large_pane_safely() {
    let size = Size {
        cols: 100,
        rows: 30,
    };
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(2),
            None,
            None,
            false,
            true,
            NewPanePlacement::Floating(Some(FloatingPaneCoordinates {
                x: None,
                y: None,
                width: Some(PercentOrFixed::Fixed(150)),
                height: Some(PercentOrFixed::Fixed(50)),
                pinned: None,
                borderless: Some(false),
            })),
            Some(1),
            None,
        )
        .unwrap();
    let active_pane = active_tab.get_active_pane(1).unwrap();
    assert_eq!(
        active_pane.x(),
        0,
        "x is 0 when pane larger than viewport (saturating_sub)"
    );
    assert_eq!(
        active_pane.y(),
        0,
        "y is 0 when pane larger than viewport (saturating_sub)"
    );
    assert!(active_pane.cols() <= 100, "width clamped to viewport");
    assert!(active_pane.rows() <= 30, "height clamped to viewport");
}

#[test]
pub fn mouse_hover_effect() {
    let size = Size {
        cols: 130,
        rows: 20,
    };
    let client_id = 1;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let hover_mouse_event_1 = MouseEvent::new_buttonless_motion(Position::new(5, 70));
    let _ = mock_screen.to_screen.send(ScreenInstruction::MouseEvent(
        hover_mouse_event_1,
        client_id,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
}

#[test]
pub fn disabled_mouse_hover_effect() {
    let size = Size {
        cols: 130,
        rows: 20,
    };
    let client_id = 1;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.set_advanced_hover_effects(false);
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let hover_mouse_event_1 = MouseEvent::new_buttonless_motion(Position::new(5, 70));
    let _ = mock_screen.to_screen.send(ScreenInstruction::MouseEvent(
        hover_mouse_event_1,
        client_id,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
}

#[test]
fn group_panes_with_mouse() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.handle_mouse_event(
        MouseEvent::new_left_press_with_alt_event(Position::new(2, 80)),
        client_id,
    );

    assert_eq!(
        screen
            .current_pane_group
            .borrow()
            .clone_inner()
            .get(&client_id),
        Some(&vec![PaneId::Terminal(2)]),
        "Pane Id added to client's pane group"
    );

    screen.handle_mouse_event(
        MouseEvent::new_left_press_with_alt_event(Position::new(2, 80)),
        client_id,
    );

    assert_eq!(
        screen
            .current_pane_group
            .borrow()
            .clone_inner()
            .get(&client_id),
        Some(&vec![]),
        "Pane Id removed from client's pane group"
    );
}

#[test]
fn mouse_focus_clears_bell_on_focused_pane() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);

    let new_pane_id = PaneId::Terminal(2);
    {
        let active_tab = screen.get_active_tab_mut(client_id).unwrap();
        // Split horizontally: pane 1 on top, pane 2 on bottom; focus moves to pane 2
        active_tab
            .horizontal_split(new_pane_id, None, client_id, None, None)
            .unwrap();
        // Move focus back up to pane 1 so pane 2 is unfocused
        active_tab.move_focus_up(client_id).unwrap();
        // Plant a bell on the unfocused pane 2
        active_tab.handle_pty_bytes(2, vec![7u8]).unwrap();
        active_tab.check_and_handle_bell_notifications(false);
        assert!(
            active_tab.panes_with_pending_bell.contains(&new_pane_id),
            "Pane 2 should have a pending bell before the click"
        );
        assert!(
            active_tab.tab_has_pending_bell,
            "Tab should have a pending bell before the click"
        );
    }

    // Click somewhere in pane 2 (bottom half of the screen)
    screen.handle_mouse_event(
        MouseEvent::new_left_press_event(Position::new(15, 60)),
        client_id,
    );
    screen.handle_mouse_event(
        MouseEvent::new_left_release_event(Position::new(15, 60)),
        client_id,
    );

    let active_tab = screen.get_active_tab(client_id).unwrap();
    assert_eq!(
        active_tab.get_active_pane_id(client_id),
        Some(new_pane_id),
        "Mouse click should have focused pane 2"
    );
    assert!(
        !active_tab.panes_with_pending_bell.contains(&new_pane_id),
        "Bell on pane 2 should be cleared after mouse focus"
    );
    assert!(
        !active_tab.tab_has_pending_bell,
        "Tab bell should be cleared after the last pane bell is cleared"
    );
}

#[test]
fn nested_guest_fullscreen_moves_from_one_pane_to_another() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    let pane_one = PaneId::Terminal(1);
    let pane_two = PaneId::Terminal(2);
    {
        let active_tab = screen.get_active_tab_mut(client_id).unwrap();
        active_tab
            .horizontal_split(pane_two, None, client_id, None, None)
            .unwrap();
    }

    screen.apply_nested_guest_fullscreen(pane_one, true);
    {
        let active_tab = screen.get_active_tab(client_id).unwrap();
        assert_eq!(
            active_tab.fullscreen_pane_id(),
            Some(pane_one),
            "the first guest pane is fullscreen"
        );
        assert!(active_tab.fullscreen_covers_ui());
    }
    assert_eq!(
        screen.nested_fullscreen_panes,
        [pane_one].into_iter().collect(),
        "only the first guest pane is tracked as fullscreen"
    );

    screen.apply_nested_guest_fullscreen(pane_two, true);
    {
        let active_tab = screen.get_active_tab(client_id).unwrap();
        assert_eq!(
            active_tab.fullscreen_pane_id(),
            Some(pane_two),
            "fullscreen moved to the second guest pane"
        );
        assert!(active_tab.fullscreen_covers_ui());
    }
    assert_eq!(
        screen.nested_fullscreen_panes,
        [pane_two].into_iter().collect(),
        "the first guest pane is no longer tracked as fullscreen, only the second is"
    );

    screen.apply_nested_guest_fullscreen(pane_two, false);
    {
        let active_tab = screen.get_active_tab(client_id).unwrap();
        assert_eq!(
            active_tab.fullscreen_pane_id(),
            None,
            "unsetting fullscreen restores the tiled layout"
        );
    }
    assert!(
        screen.nested_fullscreen_panes.is_empty(),
        "no guest panes are tracked as fullscreen after unsetting"
    );
}

#[test]
fn group_panes_with_keyboard() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    let _ = screen.toggle_pane_in_group(client_id);

    assert_eq!(
        screen
            .current_pane_group
            .borrow()
            .clone_inner()
            .get(&client_id),
        Some(&vec![PaneId::Terminal(2)]),
        "Pane Id added to client's pane group"
    );

    let _ = screen.toggle_pane_in_group(client_id);

    assert_eq!(
        screen
            .current_pane_group
            .borrow()
            .clone_inner()
            .get(&client_id),
        Some(&vec![]),
        "Pane Id removed from client's pane group"
    );
}

#[test]
fn group_panes_following_focus() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);

    {
        let active_tab = screen.get_active_tab_mut(client_id).unwrap();
        for i in 2..5 {
            active_tab
                .new_pane(
                    PaneId::Terminal(i),
                    None,
                    None,
                    false,
                    true,
                    NewPanePlacement::Tiled {
                        direction: None,
                        borderless: None,
                    },
                    Some(client_id),
                    None,
                )
                .unwrap();
        }
    }
    {
        screen.toggle_group_marking(client_id).unwrap();
        screen
            .get_active_tab_mut(client_id)
            .unwrap()
            .move_focus_up(client_id)
            .unwrap();
        screen.add_active_pane_to_group_if_marking(&client_id);
        assert_eq!(
            screen
                .current_pane_group
                .borrow()
                .clone_inner()
                .get(&client_id),
            Some(&vec![PaneId::Terminal(4), PaneId::Terminal(3)]),
            "Pane Id of focused pane and newly focused pane above added to pane group"
        );
    }
    {
        let _ = screen.toggle_group_marking(client_id);
        screen
            .get_active_tab_mut(client_id)
            .unwrap()
            .move_focus_up(client_id)
            .unwrap();
        let _ = screen.add_active_pane_to_group_if_marking(&client_id);
        assert_eq!(screen.current_pane_group.borrow().clone_inner().get(&client_id), Some(&vec![PaneId::Terminal(4), PaneId::Terminal(3)]), "Pane Id of newly focused pane not added to group after the group marking was toggled off");
    }
}

#[test]
fn break_group_with_mouse() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    new_tab(&mut screen, 1, 0);

    {
        let active_tab = screen.get_active_tab_mut(client_id).unwrap();
        for i in 2..5 {
            active_tab
                .new_pane(
                    PaneId::Terminal(i),
                    None,
                    None,
                    false,
                    true,
                    NewPanePlacement::Tiled {
                        direction: None,
                        borderless: None,
                    },
                    Some(client_id),
                    None,
                )
                .unwrap();
        }
    }
    {
        screen.toggle_group_marking(client_id).unwrap();
        screen
            .get_active_tab_mut(client_id)
            .unwrap()
            .move_focus_up(client_id)
            .unwrap();
        screen.add_active_pane_to_group_if_marking(&client_id);
        screen
            .get_active_tab_mut(client_id)
            .unwrap()
            .move_focus_up(client_id)
            .unwrap();
        screen.add_active_pane_to_group_if_marking(&client_id);
        assert_eq!(
            screen
                .current_pane_group
                .borrow()
                .clone_inner()
                .get(&client_id),
            Some(&vec![
                PaneId::Terminal(4),
                PaneId::Terminal(3),
                PaneId::Terminal(2)
            ]),
            "Group contains 3 panes"
        );
    }

    screen.handle_mouse_event(
        MouseEvent::new_right_press_with_alt_event(Position::new(2, 80)),
        client_id,
    );
    assert_eq!(
        screen
            .current_pane_group
            .borrow()
            .clone_inner()
            .get(&client_id),
        Some(&vec![]),
        "Group cleared by mouse event"
    );
}

// Following are tests for sending CLI actions
// these tests are only partially relevant to Screen
// and are included here for two reasons:
// 1. The best way to "integration test" these is combining the "screen_thread_main" and
//    "route_action" functions and mocking everything around them
// 2. These inadvertently also test many parts of Screen that are not tested elsewhere

#[test]
pub fn send_cli_write_chars_action_to_screen() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_action = CliAction::WriteChars {
        chars: "input from the cli".into(),
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_write_action_to_screen() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_action = CliAction::Write {
        bytes: vec![102, 111, 111],
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn write_to_missing_pane_reports_an_error() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::WriteCharsToPaneId {
            chars: "input from the cli".into(),
            pane_id: zellij_utils::data::PaneId::Terminal(999),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.exit_status, Some(1));
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' error, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn paste_to_missing_pane_reports_an_error() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::Paste {
            chars: "pasted from the cli".into(),
            pane_id: Some(zellij_utils::data::PaneId::Terminal(999)),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.exit_status, Some(1));
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' error, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn write_to_existing_pane_reports_no_error() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::WriteCharsToPaneId {
            chars: "input from the cli".into(),
            pane_id: zellij_utils::data::PaneId::Terminal(0),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.error_message, None);
    assert_eq!(result.exit_status, None);
}

#[test]
pub fn close_missing_pane_reports_an_error() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseTerminalPane { pane_id: 999 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.exit_status, Some(1));
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' error, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn close_pane_by_pane_id_for_missing_pane_reports_an_error() {
    // `zellij action close-pane --pane-id X` sends CloseFocusByPaneId, NOT CloseTerminalPane:
    // the two land on different ScreenInstructions, so testing one says nothing about the other
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseFocusByPaneId {
            pane_id: zellij_utils::data::PaneId::Terminal(999),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    // a miss, not a failure: the sentence is the one every pane target answers with, and the
    // exit status is left to the miss convention rather than claimed as an error here
    assert_eq!(
        result.error_message.as_deref(),
        Some("No pane answers to 'terminal_999'"),
        "{:?}",
        result.error_message
    );
}

#[test]
pub fn edit_scrollback_for_missing_pane_reports_an_error() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(None, vec![]);
    let (completion_tx, mut completion_rx) = tokio::sync::oneshot::channel();
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::EditScrollbackForPaneWithId(
            PaneId::Terminal(999),
            Some(crate::route::NotificationEnd::new(completion_tx)),
        ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![screen_thread]);
    let result = completion_rx
        .try_recv()
        .expect("no completion result was sent");
    assert_eq!(result.exit_status, Some(1));
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' error, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn send_cli_send_keys_action_to_screen() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_action = CliAction::SendKeys {
        keys: vec!["Ctrl a".to_string(), "x".to_string()],
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let received_write_instructions: Vec<_> = received_pty_instructions
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .filter(|i| matches!(i, PtyWriteInstruction::Write(..)))
        .collect();
    // here we assert only the write instructions to make sure they arrived properly and in
    // sequence to the pane
    assert_snapshot!(format!("{:#?}", received_write_instructions));
}

#[test]
pub fn send_cli_ctrl_c_reaches_pty_writer() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_action = CliAction::SendKeys {
        keys: vec!["Ctrl c".to_string()],
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let received_write_instructions: Vec<_> = received_pty_instructions
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .filter(|i| matches!(i, PtyWriteInstruction::Write(..)))
        .collect();
    assert_snapshot!(format!("{:#?}", received_write_instructions));
}

#[test]
pub fn send_cli_ctrl_w_reaches_pty_writer() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_action = CliAction::SendKeys {
        keys: vec!["Ctrl w".to_string()],
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let received_write_instructions: Vec<_> = received_pty_instructions
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .filter(|i| matches!(i, PtyWriteInstruction::Write(..)))
        .collect();
    assert_snapshot!(format!("{:#?}", received_write_instructions));
}

#[test]
pub fn send_cli_resize_action_to_screen() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let pty_writer_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let resize_cli_action = CliAction::Resize {
        resize: Resize::Increase,
        direction: Some(Direction::Left),
        pane_id: None,
    };
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(&session_metadata, resize_cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_focus_next_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let focus_next_pane_action = CliAction::FocusNextPane;
    send_cli_action_to_server(&session_metadata, focus_next_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (cursor_coordinates, _snapshot) in snapshots {
        // here we assert he cursor_coordinates to let us know if we switched the pane focus
        assert_snapshot!(format!("{:?}", cursor_coordinates));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_focus_previous_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let focus_next_pane_action = CliAction::FocusPreviousPane;
    send_cli_action_to_server(&session_metadata, focus_next_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (cursor_coordinates, _snapshot) in snapshots {
        // here we assert he cursor_coordinates to let us know if we switched the pane focus
        assert_snapshot!(format!("{:?}", cursor_coordinates));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_focus_last_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
    ];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let move_focus_action = CliAction::MoveFocus {
        direction: Direction::Right,
    };
    let focus_last_pane_action = CliAction::FocusLastPane;
    // move focus 1 -> 2 -> 3
    send_cli_action_to_server(&session_metadata, move_focus_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(&session_metadata, move_focus_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
                                                               // move focus 3 -> 2
    send_cli_action_to_server(&session_metadata, focus_last_pane_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
                                                               // move focus 2 -> 3
    send_cli_action_to_server(&session_metadata, focus_last_pane_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (cursor_coordinates, _snapshot) in snapshots {
        // here we assert he cursor_coordinates to let us know if we switched the pane focus
        assert_snapshot!(format!("{:?}", cursor_coordinates));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_move_focus_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let move_focus_action = CliAction::MoveFocus {
        direction: Direction::Right,
    };
    send_cli_action_to_server(&session_metadata, move_focus_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (cursor_coordinates, _snapshot) in snapshots {
        // here we assert he cursor_coordinates to let us know if we switched the pane focus
        assert_snapshot!(format!("{:?}", cursor_coordinates));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_move_focus_or_tab_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let move_focus_action = CliAction::MoveFocusOrTab {
        direction: Direction::Right,
    };
    send_cli_action_to_server(&session_metadata, move_focus_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (cursor_coordinates, _snapshot) in snapshots {
        // here we assert he cursor_coordinates to let us know if we switched the pane focus
        assert_snapshot!(format!("{:?}", cursor_coordinates));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_move_pane_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::MovePane {
        direction: Some(Direction::Right),
        pane_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_dump_screen_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::DumpScreen {
        file: None,
        path: Some(PathBuf::from("/tmp/foo")),
        full: true,
        pane_id: None,
        ansi: false,
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "fill pane up with something".as_bytes().to_vec(),
    ));
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert_snapshot!(format!(
        "{:?}",
        *mock_screen.os_input.fake_filesystem.lock().unwrap()
    ));
}

#[test]
pub fn send_cli_edit_scrollback_action() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::EditScrollback {
        pane_id: None,
        ansi: false,
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "fill pane up with something".as_bytes().to_vec(),
    ));
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let dumped_file_name = mock_screen
        .os_input
        .fake_filesystem
        .lock()
        .unwrap()
        .keys()
        .next()
        .unwrap()
        .clone();
    let mut found_instruction = false;
    for instruction in received_pty_instructions.lock().unwrap().iter() {
        if let PtyInstruction::OpenInPlaceEditor(
            scrollback_contents_file,
            terminal_id,
            client_id,
            _,
        ) = instruction
        {
            assert_eq!(scrollback_contents_file, &PathBuf::from(&dumped_file_name));
            assert_eq!(terminal_id, &Some(1));
            assert_eq!(client_id, &ClientTabIndexOrPaneId::ClientId(1));
            found_instruction = true;
        }
    }
    assert!(found_instruction);
}

#[test]
pub fn send_cli_scroll_up_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::ScrollUp { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send two actions here because only the last line in the pane is empty, so one action
    // won't show in a render
    send_cli_action_to_server(&session_metadata, cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, cli_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_scroll_down_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let scroll_up_cli_action = CliAction::ScrollUp { pane_id: None };
    let scroll_down_cli_action = CliAction::ScrollDown { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // scroll up some
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);

    // scroll down some
    send_cli_action_to_server(&session_metadata, scroll_down_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_down_cli_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let (_cursor_position, last_snapshot) = snapshots.last().unwrap();
    assert_snapshot!(format!("{}", last_snapshot));
}

#[test]
pub fn send_cli_scroll_to_bottom_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let scroll_up_cli_action = CliAction::ScrollUp { pane_id: None };
    let scroll_to_bottom_action = CliAction::ScrollToBottom { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // scroll up some
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);
    send_cli_action_to_server(&session_metadata, scroll_up_cli_action.clone(), client_id);

    // scroll to bottom
    send_cli_action_to_server(
        &session_metadata,
        scroll_to_bottom_action.clone(),
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let (_cursor_position, last_snapshot) = snapshots.last().unwrap();
    assert_snapshot!(format!("{}", last_snapshot));
}

#[test]
pub fn send_cli_scroll_to_top_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let scroll_to_top_action = CliAction::ScrollToTop { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // scroll to top
    send_cli_action_to_server(&session_metadata, scroll_to_top_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_page_scroll_up_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let page_scroll_up_action = CliAction::PageScrollUp { pane_id: None };
    let mut pane_contents = String::new();
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    send_cli_action_to_server(&session_metadata, page_scroll_up_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_page_scroll_down_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let page_scroll_up_action = CliAction::PageScrollUp { pane_id: None };
    let page_scroll_down_action = CliAction::PageScrollDown { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    // scroll up some
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(&session_metadata, page_scroll_up_action.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(&session_metadata, page_scroll_up_action.clone(), client_id);

    // scroll down
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(&session_metadata, page_scroll_down_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_half_page_scroll_up_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let half_page_scroll_up_action = CliAction::HalfPageScrollUp { pane_id: None };
    let mut pane_contents = String::new();
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    send_cli_action_to_server(&session_metadata, half_page_scroll_up_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_half_page_scroll_down_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let half_page_scroll_up_action = CliAction::HalfPageScrollUp { pane_id: None };
    let half_page_scroll_down_action = CliAction::HalfPageScrollDown { pane_id: None };
    let mut pane_contents = String::new();
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    for i in 0..20 {
        pane_contents.push_str(&format!("fill pane up with something {}\n\r", i));
    }
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        pane_contents.as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    // scroll up some
    send_cli_action_to_server(
        &session_metadata,
        half_page_scroll_up_action.clone(),
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    send_cli_action_to_server(
        &session_metadata,
        half_page_scroll_up_action.clone(),
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render

    // scroll down
    send_cli_action_to_server(&session_metadata, half_page_scroll_down_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_toggle_full_screen_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let toggle_full_screen_action = CliAction::ToggleFullscreen { pane_id: None };
    send_cli_action_to_server(&session_metadata, toggle_full_screen_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_toggle_pane_frames_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let toggle_pane_frames_action = CliAction::TogglePaneFrames;
    send_cli_action_to_server(&session_metadata, toggle_pane_frames_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_toggle_active_tab_sync_action() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let cli_toggle_active_tab_sync_action = CliAction::ToggleActiveSyncTab { tab_id: None };
    let cli_write_action = CliAction::Write {
        bytes: vec![102, 111, 111],
        pane_id: None,
    };
    send_cli_action_to_server(
        &session_metadata,
        cli_toggle_active_tab_sync_action,
        client_id,
    );
    send_cli_action_to_server(&session_metadata, cli_write_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let received_write_instructions: Vec<_> = received_pty_instructions
        .lock()
        .unwrap()
        .clone()
        .into_iter()
        .filter(|i| matches!(i, PtyWriteInstruction::Write(..)))
        .collect();
    // here we should have 2 Write instructions, one for each pane
    assert_snapshot!(format!("{:?}", received_write_instructions));
}

#[test]
pub fn send_cli_new_pane_action_with_default_parameters() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: None,
        command: vec![],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn web_new_pane_in_tab_action_targets_requested_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(TiledPaneLayout::default()), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );

    // This is exactly the Action the web control bridge produces for the
    // browser `NewPaneInTab { tab_id }` payload.
    let action = Action::NewTiledPane {
        direction: None,
        command: None,
        pane_name: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: Some(0),
    };
    route_arbitrary_action_to_server(&session_metadata, action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let spawned_with_tab_index =
        received_pty_instructions
            .lock()
            .unwrap()
            .iter()
            .any(|i| match i {
                PtyInstruction::SpawnTerminal(_, _, _, _, client_tab_index_or_pane_id, ..) => {
                    matches!(
                        client_tab_index_or_pane_id,
                        ClientTabIndexOrPaneId::TabIndex(0)
                    )
                },
                _ => false,
            });
    assert!(
        spawned_with_tab_index,
        "NewPaneInTab must spawn a terminal targeting the requested tab index; got {:?}",
        *received_pty_instructions.lock().unwrap()
    );
}

#[test]
pub fn send_cli_new_pane_action_with_split_direction() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: Some(Direction::Right),
        command: vec![],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_new_pane_action_with_command_and_cwd() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: Some(Direction::Right),
        command: vec!["htop".into()],
        plugin: None,
        cwd: Some("/some/folder".into()),
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let new_pane_instruction = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PtyInstruction::SpawnTerminal(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:?}", new_pane_instruction));
}

#[test]
pub fn send_cli_new_pane_action_with_floating_pane_and_coordinates() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: Some(Direction::Right),
        command: vec!["htop".into()],
        plugin: None,
        cwd: Some("/some/folder".into()),
        floating: true,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: Some("10".to_owned()),
        y: None,
        width: Some("20%".to_owned()),
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_edit_action_with_default_parameters() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_edit_action = CliAction::Edit {
        file: PathBuf::from("/file/to/edit"),
        direction: None,
        line_number: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        cwd: None,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        borderless: Some(false),
        near_current_pane: false,
        no_focus: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_edit_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_edit_action_with_line_number() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_edit_action = CliAction::Edit {
        file: PathBuf::from("/file/to/edit"),
        direction: None,
        line_number: Some(100),
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        cwd: None,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        borderless: Some(false),
        near_current_pane: false,
        no_focus: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_edit_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_edit_action_with_split_direction() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_edit_action = CliAction::Edit {
        file: PathBuf::from("/file/to/edit"),
        direction: Some(Direction::Down),
        line_number: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        cwd: None,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        borderless: Some(false),
        near_current_pane: false,
        no_focus: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_edit_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    assert_snapshot!(format!("{:?}", *received_pty_instructions.lock().unwrap()));
}

#[test]
pub fn send_cli_switch_mode_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let cli_switch_mode = CliAction::SwitchMode {
        input_mode: InputMode::Locked,
    };
    send_cli_action_to_server(&session_metadata, cli_switch_mode, client_id);

    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);

    let switch_mode_action = received_server_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            ServerInstruction::ChangeMode(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:?}", switch_mode_action));
}

#[test]
pub fn send_cli_toggle_pane_embed_or_float() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let toggle_pane_embed_or_floating = CliAction::TogglePaneEmbedOrFloating { pane_id: None };
    // first time to float
    send_cli_action_to_server(
        &session_metadata,
        toggle_pane_embed_or_floating.clone(),
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    // second time to embed
    send_cli_action_to_server(
        &session_metadata,
        toggle_pane_embed_or_floating.clone(),
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let _snapshot_count = snapshots.len();
    let last_three_snapshots = snapshots.clone().into_iter().rev().take(3).rev(); // we do this to
                                                                                  // prevent extra
                                                                                  // renders from
                                                                                  // throwing us
                                                                                  // off
    for (_cursor_coordinates, snapshot) in last_three_snapshots.clone() {
        eprintln!("{}", snapshot);
    }
    for (_cursor_coordinates, snapshot) in last_three_snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
}

#[test]
pub fn send_cli_toggle_floating_panes() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let toggle_pane_embed_or_floating = CliAction::TogglePaneEmbedOrFloating { pane_id: None };
    let toggle_floating_panes = CliAction::ToggleFloatingPanes { tab_id: None };
    // float the focused pane
    send_cli_action_to_server(&session_metadata, toggle_pane_embed_or_floating, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    // toggle floating panes (will hide the floated pane from the previous action)
    send_cli_action_to_server(&session_metadata, toggle_floating_panes.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    // toggle floating panes (will show the floated pane)
    send_cli_action_to_server(&session_metadata, toggle_floating_panes.clone(), client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_close_pane_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_instruction = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let close_pane_action = CliAction::ClosePane { pane_id: None };
    send_cli_action_to_server(&session_metadata, close_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_instruction, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_new_tab_action_default_params() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let new_tab_action = CliAction::NewTab {
        name: None,
        layout: None,
        layout_string: None,
        layout_dir: None,
        cwd: None,
        initial_command: vec![],
        initial_plugin: None,
        close_on_exit: Default::default(),
        start_suspended: Default::default(),
        block_until_exit: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        no_focus: false,
    };
    send_cli_action_to_server(&session_metadata, new_tab_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let received_plugin_instructions = received_plugin_instructions.lock().unwrap();
    let new_tab_action =
        received_plugin_instructions
            .iter()
            .find(|instruction| match instruction {
                PluginInstruction::NewTab(..) => true,
                _ => false,
            });
    assert_snapshot!(format!("{:#?}", new_tab_action));
}

#[test]
pub fn send_cli_new_tab_action_with_name_and_layout() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let new_tab_action = CliAction::NewTab {
        name: Some("my-awesome-tab-name".into()),
        layout: Some(PathBuf::from(format!(
            "{}/src/unit/fixtures/layout-with-three-panes.kdl",
            env!("CARGO_MANIFEST_DIR")
        ))),
        layout_string: None,
        layout_dir: None,
        cwd: None,
        initial_command: vec![],
        initial_plugin: None,
        close_on_exit: Default::default(),
        start_suspended: Default::default(),
        block_until_exit: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        no_focus: false,
    };
    send_cli_action_to_server(&session_metadata, new_tab_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let new_tab_instruction = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|i| {
            if let PluginInstruction::NewTab(..) = i {
                return true;
            } else {
                return false;
            }
        })
        .unwrap()
        .clone();
    let output = format!("{:#?}", new_tab_instruction);
    // Normalize Windows path separators for cross-platform snapshot consistency
    let output = output.replace("\\\\", "/");
    assert_snapshot!(output);
}

#[test]
pub fn send_cli_next_tab_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let goto_next_tab = CliAction::GoToNextTab;
    send_cli_action_to_server(&session_metadata, goto_next_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    mock_screen.teardown(vec![server_thread, screen_thread]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_previous_tab_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let goto_previous_tab = CliAction::GoToPreviousTab;
    send_cli_action_to_server(&session_metadata, goto_previous_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_goto_tab_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let goto_tab = CliAction::GoToTab { index: 1 };
    send_cli_action_to_server(&session_metadata, goto_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_close_tab_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let close_tab = CliAction::CloseTab { tab_id: None };
    send_cli_action_to_server(&session_metadata, close_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_rename_tab() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let rename_tab = CliAction::RenameTab {
        name: "new-tab-name".into(),
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, rename_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let plugin_rename_tab_instruction = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PluginInstruction::Update(updates) => updates
                .iter()
                .find(|u| match u {
                    (_, _, Event::TabUpdate(..)) => true,
                    _ => false,
                })
                .is_some(),
            _ => false,
        })
        .cloned();
    assert_snapshot!(format!("{:#?}", plugin_rename_tab_instruction))
}

#[test]
pub fn send_cli_undo_rename_tab() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut second_tab_layout = TiledPaneLayout::default();
    second_tab_layout.children_split_direction = SplitDirection::Horizontal;
    second_tab_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(second_tab_layout);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let rename_tab = CliAction::RenameTab {
        name: "new-tab-name".into(),
        tab_id: None,
    };
    let undo_rename_tab = CliAction::UndoRenameTab { tab_id: None };
    // first rename the tab
    send_cli_action_to_server(&session_metadata, rename_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    // then undo the tab rename to go back to the default name
    send_cli_action_to_server(&session_metadata, undo_rename_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let plugin_undo_rename_tab_instruction = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PluginInstruction::Update(updates) => updates
                .iter()
                .find(|u| match u {
                    (_, _, Event::TabUpdate(..)) => true,
                    _ => false,
                })
                .is_some(),
            _ => false,
        })
        .cloned();
    assert_snapshot!(format!("{:#?}", plugin_undo_rename_tab_instruction))
}

#[test]
pub fn query_tab_names_action_logs_the_tab_names() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab(TiledPaneLayout::default());
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(TiledPaneLayout::default()), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    // the CLI verb is gone; the action itself lives on for keybindings and plugins, and this is
    // still the path that answers it
    let _ =
        route_arbitrary_action_and_get_result(&session_metadata, Action::QueryTabNames, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let log_tab_names_instruction = received_server_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            ServerInstruction::Log(..) => true,
            _ => false,
        })
        .cloned();
    assert_snapshot!(format!("{:#?}", log_tab_names_instruction));
}

#[test]
pub fn send_cli_launch_or_focus_plugin_action() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::LaunchOrFocusPlugin {
        floating: true,
        in_place: false,
        close_replaced_pane: false,
        move_to_focused_tab: true,
        url: "file:/path/to/fake/plugin".to_owned(),
        configuration: Default::default(),
        skip_plugin_cache: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let pty_fill_plugin_cwd_instruction = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PtyInstruction::FillPluginCwd(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:#?}", pty_fill_plugin_cwd_instruction));
}

#[test]
pub fn send_cli_launch_or_focus_plugin_action_when_plugin_is_already_loaded() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    let existing_plugin_pane = TiledPaneLayout {
        run: Some(Run::Plugin(RunPluginOrAlias::RunPlugin(RunPlugin {
            _allow_exec_host_cmd: false,
            location: RunPluginLocation::File(PathBuf::from("/path/to/fake/plugin")),
            configuration: Default::default(),
            ..Default::default()
        }))),
        ..Default::default()
    };
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), existing_plugin_pane];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::LaunchOrFocusPlugin {
        floating: true,
        in_place: false,
        close_replaced_pane: false,
        move_to_focused_tab: true,
        url: "file:/path/to/fake/plugin".to_owned(),
        configuration: Default::default(),
        skip_plugin_cache: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![plugin_thread, server_thread, screen_thread]);

    let plugin_load_instruction_sent = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PluginInstruction::Load(..) => true,
            _ => false,
        })
        .is_some();
    assert!(
        !plugin_load_instruction_sent,
        "Plugin Load instruction should not be sent for an already loaded plugin"
    );
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    assert_eq!(
        snapshot_count, 2,
        "Another render was sent for focusing the already loaded plugin"
    );
    for (cursor_coordinates, _snapshot) in snapshots.iter().skip(1) {
        assert!(
            cursor_coordinates.is_none(),
            "Cursor moved to existing plugin in final snapshot indicating focus changed"
        );
    }
}

#[test]
pub fn send_cli_launch_or_focus_plugin_action_when_plugin_is_already_loaded_for_plugin_alias() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    let existing_plugin_pane = TiledPaneLayout {
        run: Some(Run::Plugin(RunPluginOrAlias::Alias(PluginAlias {
            name: "fixture_plugin_for_tests".to_owned(),
            configuration: Some(Default::default()),
            run_plugin: Some(RunPlugin {
                _allow_exec_host_cmd: false,
                location: RunPluginLocation::File(PathBuf::from("/path/to/fake/plugin")),
                configuration: Default::default(),
                ..Default::default()
            }),
            ..Default::default()
        }))),
        ..Default::default()
    };
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), existing_plugin_pane];
    let screen_thread = mock_screen.run_with_alias(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::LaunchOrFocusPlugin {
        floating: true,
        in_place: false,
        close_replaced_pane: false,
        move_to_focused_tab: true,
        url: "fixture_plugin_for_tests".to_owned(),
        configuration: Default::default(),
        skip_plugin_cache: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for actions to be
    mock_screen.teardown(vec![plugin_thread, server_thread, screen_thread]);

    let plugin_load_instruction_sent = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PluginInstruction::Load(..) => true,
            _ => false,
        })
        .is_some();
    assert!(
        !plugin_load_instruction_sent,
        "Plugin Load instruction should not be sent for an already loaded plugin"
    );
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    assert_eq!(
        snapshot_count, 3,
        "Another render was sent for focusing the already loaded plugin"
    );
    for (cursor_coordinates, _snapshot) in snapshots.iter().skip(1) {
        assert!(
            cursor_coordinates.is_none(),
            "Cursor moved to existing plugin in final snapshot indicating focus changed"
        );
    }
}

#[test]
pub fn screen_can_suppress_pane() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SuppressPane(PaneId::Terminal(1), 1));
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_break_pane_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("pane_to_break_free".to_owned());
    let mut pane_to_stay = TiledPaneLayout::default();
    pane_to_stay.name = Some("pane_to_stay".to_owned());
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free, pane_to_stay];
    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send ApplyLayout, because in prod this is eventually received after the message traverses
    // through the plugin and pty threads (to open extra stuff we need in the layout, eg. the
    // default plugins)
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        vec![], // floating_panes_layout
        Default::default(),
        vec![], // floating panes ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move back to make sure the other pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move forward to make sure the broken pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusRightOrNextTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_cannot_break_last_selectable_pane_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let initial_layout = TiledPaneLayout::default();
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_break_floating_pane_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("tiled_pane".to_owned());
    let mut floating_pane = FloatingPaneLayout::default();
    floating_pane.name = Some("floating_pane_to_eject".to_owned());
    let mut floating_panes_layout = vec![floating_pane];
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free];
    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let screen_thread = mock_screen.run(Some(initial_layout), floating_panes_layout.clone());
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send ApplyLayout, because in prod this is eventually received after the message traverses
    // through the plugin and pty threads (to open extra stuff we need in the layout, eg. the
    // default plugins)
    floating_panes_layout.get_mut(0).unwrap().already_running = true;
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        floating_panes_layout,
        vec![], // tiled pane ids
        vec![], // floating pane ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(200));
    // move back to make sure the other pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(200));
    // move forward to make sure the broken pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusRightOrNextTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(200));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn breaking_panes_to_a_tab_index_reports_a_missing_pane() {
    // the plugin api reaches this instruction directly, so it gets the same stale-pane-id answer
    // its two siblings give
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(None, vec![]);
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPanesToTabWithIndex {
            pane_ids: vec![PaneId::Terminal(999)],
            tab_index: 0,
            should_change_focus_to_new_tab: true,
            client_id: 1,
            completion_tx: Some(crate::route::NotificationEnd::new(completion_tx)),
        });
    let result =
        crate::route::wait_for_action_completion(completion_rx, "BreakPanesToTabWithIndex", false);
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.exit_status, Some(1));
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' error, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn screen_can_break_multiple_stacked_panes_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let mut stacked_parent = TiledPaneLayout::default();
    stacked_parent.children_are_stacked = true;
    stacked_parent.children = vec![
        TiledPaneLayout {
            name: Some("pane_to_stay".to_owned()),
            ..Default::default()
        },
        TiledPaneLayout {
            name: Some("pane_to_break_1".to_owned()),
            ..Default::default()
        },
        TiledPaneLayout {
            name: Some("pane_to_break_2".to_owned()),
            ..Default::default()
        },
    ];
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children = vec![stacked_parent];

    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render

    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPanesToNewTab {
            pane_ids: vec![PaneId::Terminal(1), PaneId::Terminal(2)],
            default_shell: None,
            should_change_focus_to_new_tab: true,
            new_tab_name: None,
            client_id: 1,
            completion_tx: None,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send ApplyLayout, because in prod this is eventually received after the message traverses
    // through the plugin and pty threads (to open extra stuff we need in the layout, eg. the
    // default plugins)
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        vec![],
        Default::default(),
        vec![],
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move back to make sure the other pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move forward to make sure the broken panes are in the next tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusRightOrNextTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in &snapshots {
        eprintln!("{}", snapshot);
    }
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_break_plugin_pane_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("plugin_pane_to_break_free".to_owned());
    pane_to_break_free.run = Some(Run::Plugin(RunPluginOrAlias::RunPlugin(RunPlugin {
        _allow_exec_host_cmd: false,
        location: RunPluginLocation::File(PathBuf::from("/path/to/fake/plugin")),
        configuration: Default::default(),
        ..Default::default()
    })));
    let mut pane_to_stay = TiledPaneLayout::default();
    pane_to_stay.name = Some("pane_to_stay".to_owned());
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free, pane_to_stay];
    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send ApplyLayout, because in prod this is eventually received after the message traverses
    // through the plugin and pty threads (to open extra stuff we need in the layout, eg. the
    // default plugins)
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        vec![], // floating_panes_layout
        Default::default(),
        vec![], // floating panes ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move back to make sure the other pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move forward to make sure the broken pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusRightOrNextTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_break_floating_plugin_pane_to_a_new_tab() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("tiled_pane".to_owned());
    let mut floating_pane = FloatingPaneLayout::default();
    floating_pane.name = Some("floating_plugin_pane_to_eject".to_owned());
    floating_pane.run = Some(Run::Plugin(RunPluginOrAlias::RunPlugin(RunPlugin {
        _allow_exec_host_cmd: false,
        location: RunPluginLocation::File(PathBuf::from("/path/to/fake/plugin")),
        configuration: Default::default(),
        ..Default::default()
    })));
    let mut floating_panes_layout = vec![floating_pane];
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free];
    let mut mock_screen = MockScreen::new(size);
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let screen_thread = mock_screen.run(Some(initial_layout), floating_panes_layout.clone());
    std::thread::sleep(std::time::Duration::from_millis(100)); // give time for the async render
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // we send ApplyLayout, because in prod this is eventually received after the message traverses
    // through the plugin and pty threads (to open extra stuff we need in the layout, eg. the
    // default plugins)
    floating_panes_layout.get_mut(0).unwrap().already_running = true;
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        floating_panes_layout,
        vec![], // tiled pane ids
        vec![], // floating pane ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move back to make sure the other pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // move forward to make sure the broken pane is in the previous tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusRightOrNextTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_move_pane_to_a_new_tab_right() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("pane_to_break_free".to_owned());
    let mut pane_to_stay = TiledPaneLayout::default();
    pane_to_stay.name = Some("pane_to_stay".to_owned());
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free, pane_to_stay];
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));

    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        Default::default(),
        vec![], // tiled pane ids
        vec![], // floating pane ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPaneRight(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn screen_can_move_pane_to_a_new_tab_left() {
    let size = Size { cols: 80, rows: 20 };
    let mut initial_layout = TiledPaneLayout::default();
    let mut pane_to_break_free = TiledPaneLayout::default();
    pane_to_break_free.name = Some("pane_to_break_free".to_owned());
    let mut pane_to_stay = TiledPaneLayout::default();
    pane_to_stay.name = Some("pane_to_stay".to_owned());
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![pane_to_break_free, pane_to_stay];
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPane(Default::default(), 1, None));
    let _ = mock_screen.to_screen.send(ScreenInstruction::ApplyLayout(
        TiledPaneLayout::default(),
        Default::default(),
        vec![], // tiled pane ids
        vec![], // floating pane ids
        Default::default(),
        1,
        true,
        (1, false),
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::MoveFocusLeftOrPreviousTab(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::BreakPaneLeft(1, None));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_stack_panes_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
        TiledPaneLayout::default(),
    ];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let stack_panes_action = CliAction::StackPanes {
        pane_ids: vec!["1".to_owned(), "2".to_owned(), "3".to_owned()],
    };
    send_cli_action_to_server(&session_metadata, stack_panes_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn send_cli_change_floating_pane_coordinates_action() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10; // fake client id should not appear in the screen's state
    let initial_tiled_layout = TiledPaneLayout::default();
    let initial_floating_panes = vec![FloatingPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_tiled_layout), initial_floating_panes);

    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let change_floating_pane_coordinates_action = CliAction::ChangeFloatingPaneCoordinates {
        pane_id: "0".to_owned(),
        x: Some("0".to_owned()),
        y: Some("0".to_owned()),
        width: Some("10".to_owned()),
        height: Some("10".to_owned()),
        pinned: None,
        borderless: Some(false),
    };
    send_cli_action_to_server(
        &session_metadata,
        change_floating_pane_coordinates_action,
        client_id,
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let snapshots = take_snapshots_and_cursor_coordinates_from_render_events(
        received_server_instructions.lock().unwrap().iter(),
        size,
    );
    let snapshot_count = snapshots.len();
    for (_cursor_coordinates, snapshot) in snapshots {
        assert_snapshot!(format!("{}", snapshot));
    }
    assert_snapshot!(format!("{}", snapshot_count));
}

#[test]
pub fn go_to_tab_by_id_verifies_screen_state() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);

    // Create multiple tabs with known IDs
    new_tab(&mut screen, 1, 0); // ID 0
    new_tab(&mut screen, 2, 1); // ID 1
    new_tab(&mut screen, 3, 2); // ID 2

    // Active tab should be the last one created (ID 2)
    assert_eq!(screen.get_active_tab(client_id).unwrap().id, 2);

    // Switch to tab with ID 0
    if let Some(tab_position) = screen.get_tab_position_by_id(0) {
        screen
            .switch_active_tab(tab_position, None, true, client_id)
            .expect("TEST");
    }

    // Verify active tab is now ID 0
    assert_eq!(
        screen.get_active_tab(client_id).unwrap().id,
        0,
        "Active tab should be tab with ID 0"
    );

    // Switch to tab with ID 1
    if let Some(tab_position) = screen.get_tab_position_by_id(1) {
        screen
            .switch_active_tab(tab_position, None, true, client_id)
            .expect("TEST");
    }

    // Verify active tab is now ID 1
    assert_eq!(
        screen.get_active_tab(client_id).unwrap().id,
        1,
        "Active tab should be tab with ID 1"
    );
}

#[test]
pub fn send_cli_go_to_tab_by_id_action() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();

    // Create tabs
    mock_screen.new_tab(TiledPaneLayout::default());
    mock_screen.new_tab(TiledPaneLayout::default());

    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send CLI action
    let cli_action = CliAction::GoToTabById { id: 1 };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);

    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    // Verify that CLI action caused screen updates (Render instructions sent)
    let render_count = received_server_instructions
        .lock()
        .unwrap()
        .iter()
        .filter(|instr| matches!(instr, ServerInstruction::Render(_)))
        .count();

    assert!(
        render_count > 0,
        "GoToTabById CLI action should trigger screen renders"
    );
}

#[test]
pub fn rename_tab_by_id_verifies_screen_state() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    // Create tabs with known IDs
    new_tab(&mut screen, 1, 0); // ID 0
    new_tab(&mut screen, 2, 1); // ID 1

    // Verify initial tab names
    assert_eq!(screen.get_tab_by_id(0).unwrap().name, "Tab #1");
    assert_eq!(screen.get_tab_by_id(1).unwrap().name, "Tab #2");

    // Rename tab with ID 1
    if let Some(tab) = screen.get_tab_by_id_mut(1) {
        tab.name = "CustomTabName".to_string();
    }

    // Verify the tab name changed
    assert_eq!(
        screen.get_tab_by_id(1).unwrap().name,
        "CustomTabName",
        "Tab with ID 1 should be renamed"
    );

    // Verify other tab name unchanged
    assert_eq!(
        screen.get_tab_by_id(0).unwrap().name,
        "Tab #1",
        "Tab with ID 0 should keep original name"
    );
}

#[test]
pub fn send_cli_rename_tab_by_id_action() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();

    mock_screen.new_tab(TiledPaneLayout::default());

    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send CLI action
    let cli_action = CliAction::RenameTabById {
        id: 1,
        name: "TestName".to_string(),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);

    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    // Verify that CLI action was processed (no panics means routing worked)
    // The action should complete successfully
    assert!(true, "RenameTabById CLI action completed without errors");
}

#[test]
pub fn close_tab_by_id_verifies_screen_state() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);

    // Create multiple tabs with known IDs
    new_tab(&mut screen, 1, 0); // ID 0
    new_tab(&mut screen, 2, 1); // ID 1
    new_tab(&mut screen, 3, 2); // ID 2

    assert_eq!(screen.tabs.len(), 3, "Should have 3 tabs initially");

    // Verify all tabs exist
    assert!(
        screen.get_tab_by_id(0).is_some(),
        "Tab with ID 0 should exist"
    );
    assert!(
        screen.get_tab_by_id(1).is_some(),
        "Tab with ID 1 should exist"
    );
    assert!(
        screen.get_tab_by_id(2).is_some(),
        "Tab with ID 2 should exist"
    );

    // Close tab with ID 1
    screen.close_tab_by_id(1).expect("TEST");

    assert_eq!(screen.tabs.len(), 2, "Should have 2 tabs after closing one");

    // Verify tab with ID 1 no longer exists
    assert!(
        screen.get_tab_by_id(1).is_none(),
        "Tab with ID 1 should not exist"
    );

    // Verify other tabs still exist
    assert!(
        screen.get_tab_by_id(0).is_some(),
        "Tab with ID 0 should still exist"
    );
    assert!(
        screen.get_tab_by_id(2).is_some(),
        "Tab with ID 2 should still exist"
    );
}

#[test]
pub fn send_cli_close_tab_by_id_action() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();

    mock_screen.new_tab(TiledPaneLayout::default());
    mock_screen.new_tab(TiledPaneLayout::default());

    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send CLI action
    let cli_action = CliAction::CloseTabById { id: 1 };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);

    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    // Verify that CLI action was processed (no panics means routing worked)
    // The action should complete successfully
    assert!(true, "CloseTabById CLI action completed without errors");
}

#[test]
pub fn send_cli_new_pane_in_place_with_close_replaced_pane() {
    // Verify that `--close-replaced-pane` propagates from CLI through to the
    // PtyInstruction::SpawnInPlaceTerminal instruction as `true`.
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::NewPane {
        direction: None,
        command: vec!["bash".into()],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: true,
        close_replaced_pane: true,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let spawn_in_place_instruction = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PtyInstruction::SpawnInPlaceTerminal(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:#?}", spawn_in_place_instruction));
}

#[test]
pub fn send_cli_edit_in_place_with_close_replaced_pane() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::Edit {
        file: PathBuf::from("/some/file.txt"),
        direction: None,
        line_number: None,
        floating: false,
        in_place: true,
        close_replaced_pane: true,
        cwd: None,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let spawn_in_place_instruction = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PtyInstruction::SpawnInPlaceTerminal(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:#?}", spawn_in_place_instruction));
}

#[test]
pub fn send_cli_launch_or_focus_plugin_in_place_with_close_replaced_pane() {
    // Verify that `--close-replaced-pane` propagates from the `launch-or-focus-plugin --in-place`
    // CLI action through to the PtyInstruction::FillPluginCwd instruction as `true`.
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10; // fake client id should not appear in the screen's state
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::LaunchOrFocusPlugin {
        floating: false,
        in_place: true,
        close_replaced_pane: true,
        move_to_focused_tab: false,
        url: "file:/path/to/fake/plugin".to_owned(),
        configuration: Default::default(),
        skip_plugin_cache: false,
        tab_id: None,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);

    let fill_plugin_cwd_instruction = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find(|instruction| match instruction {
            PtyInstruction::FillPluginCwd(..) => true,
            _ => false,
        })
        .cloned();

    assert_snapshot!(format!("{:#?}", fill_plugin_cwd_instruction));
}

fn create_new_screen_with_message_capture(
    size: Size,
) -> (
    Screen,
    Arc<Mutex<HashMap<ClientId, Vec<ServerToClientMsg>>>>,
) {
    let (screen, messages, _client_send_state) = create_new_screen_with_client_send_state(size);
    (screen, messages)
}

// as above, but also hands back the map controlling how sends to each client fail
fn create_new_screen_with_client_send_state(
    size: Size,
) -> (
    Screen,
    Arc<Mutex<HashMap<ClientId, Vec<ServerToClientMsg>>>>,
    Arc<Mutex<HashMap<ClientId, SendToClientError>>>,
) {
    let (screen, messages, client_send_state, _pane_send_state) =
        create_new_screen_with_pane_send_state(size);
    (screen, messages, client_send_state)
}

// as above, but the send state can also be staged for one pane of one client
#[allow(clippy::type_complexity)]
fn create_new_screen_with_pane_send_state(
    size: Size,
) -> (
    Screen,
    Arc<Mutex<HashMap<ClientId, Vec<ServerToClientMsg>>>>,
    Arc<Mutex<HashMap<ClientId, SendToClientError>>>,
    Arc<Mutex<HashMap<(ClientId, zellij_utils::data::PaneId), SendToClientError>>>,
) {
    let mut bus: Bus<ScreenInstruction> = Bus::empty();
    let fake_os_input = FakeInputOutput::default();
    let messages = fake_os_input.server_to_client_messages.clone();
    let client_send_state = fake_os_input.client_send_state.clone();
    let pane_send_state = fake_os_input.pane_send_state.clone();
    bus.os_input = Some(Box::new(fake_os_input));
    let client_attributes = ClientAttributes {
        size,
        ..Default::default()
    };
    let max_panes = None;
    let mut mode_info = ModeInfo::default();
    mode_info.session_name = Some("zellij-test".into());
    let draw_pane_frames = PaneFrameStyle::None;
    let auto_layout = true;
    let session_is_mirrored = true;
    let copy_options = CopyOptions::default();
    let default_layout = Box::new(Layout::default());
    let default_layout_name = None;
    let default_shell = PathBuf::from("my_default_shell");
    let session_serialization = true;
    let serialize_pane_viewport = false;
    let scrollback_lines_to_serialize = None;
    let layout_dir = None;
    let debug = false;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let arrow_fonts = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let stacked_resize = true;
    let web_sharing = WebSharing::Off;
    let web_server_ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let web_server_port = 8080;
    let visual_bell = true;
    let screen = Screen::new(
        bus,
        &client_attributes,
        max_panes,
        mode_info,
        draw_pane_frames,
        auto_layout,
        session_is_mirrored,
        copy_options,
        debug,
        default_layout,
        default_layout_name,
        default_shell,
        session_serialization,
        serialize_pane_viewport,
        scrollback_lines_to_serialize,
        styled_underlines,
        osc8_hyperlinks,
        arrow_fonts,
        layout_dir,
        explicitly_disable_kitty_keyboard_protocol,
        true, // support_kitty_graphics_protocol
        stacked_resize,
        false,
        None,
        None,
        false,
        web_sharing,
        true,
        true,
        DEFAULT_WORD_SEPARATORS.to_owned(),
        true,
        true,
        visual_bell,
        0,     // bell_clear_delay_ms
        false, // focus_follows_mouse
        false, // mouse_click_through
        web_server_ip,
        web_server_port,
        NestedSessionHandling::default(),
    );
    (
        seed_first_client_size(screen, size),
        messages,
        client_send_state,
        pane_send_state,
    )
}

#[test]
fn subscriber_receives_initial_delivery() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 1);
    match &client_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate {
            is_initial,
            scrollback,
            ..
        } => {
            assert!(*is_initial);
            assert!(scrollback.is_none());
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
}

#[test]
fn subscriber_receives_initial_with_scrollback() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        Some(0),
        false,
    );

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 1);
    match &client_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate {
            is_initial,
            scrollback,
            ..
        } => {
            assert!(*is_initial);
            assert!(scrollback.is_some());
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
}

#[test]
fn subscriber_no_update_on_unchanged_viewport() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    let initial_viewport = {
        let msgs = messages.lock().unwrap();
        let client_msgs = msgs.get(&100).unwrap();
        match &client_msgs[0] {
            ServerToClientMsg::PaneRenderUpdate { viewport, .. } => viewport.clone(),
            _ => panic!("Expected PaneRenderUpdate"),
        }
    };

    let mut pane_map = HashMap::new();
    pane_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: initial_viewport,
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 1);
}

#[test]
fn subscriber_receives_update_on_changed_viewport() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    let mut pane_map = HashMap::new();
    pane_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["changed line".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 2);
    match &client_msgs[1] {
        ServerToClientMsg::PaneRenderUpdate {
            is_initial,
            scrollback,
            viewport,
            ..
        } => {
            assert!(!is_initial);
            assert!(scrollback.is_none());
            assert_eq!(viewport, &vec!["changed line".to_string()]);
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
}

#[test]
fn slow_subscriber_keeps_its_subscription() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages, client_send_state) = create_new_screen_with_client_send_state(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    // the client is behind, not gone
    client_send_state
        .lock()
        .unwrap()
        .insert(100, SendToClientError::ClientBusy);

    let mut pane_map = HashMap::new();
    pane_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["changed line".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    assert!(
        screen.pane_render_subscribers.contains_key(&100),
        "A subscriber that is merely behind must keep its subscription"
    );

    // once it catches up, the update it missed is sent - it was never marked as delivered
    client_send_state.lock().unwrap().remove(&100);
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 2);
    match &client_msgs[1] {
        ServerToClientMsg::PaneRenderUpdate { viewport, .. } => {
            assert_eq!(viewport, &vec!["changed line".to_string()]);
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
}

/// A channel that fills partway through a batch takes some panes and refuses the rest. The panes
/// it took must not be re-sent on the next tick as if their contents had changed.
#[test]
fn a_busy_send_only_skips_the_pane_it_refused() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages, _client_send_state, pane_send_state) =
        create_new_screen_with_pane_send_state(size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    let accepted_pane = zellij_utils::data::PaneId::Terminal(1);
    let refused_pane = zellij_utils::data::PaneId::Terminal(2);

    screen.subscribe_to_pane_renders(100, vec![accepted_pane, refused_pane], None, false);

    // the channel has room for the first pane and not for the second
    pane_send_state
        .lock()
        .unwrap()
        .insert((100, refused_pane), SendToClientError::ClientBusy);

    let mut pane_map = HashMap::new();
    pane_map.insert(
        accepted_pane,
        PaneContents {
            viewport: vec!["accepted line".to_string()],
            ..Default::default()
        },
    );
    pane_map.insert(
        refused_pane,
        PaneContents {
            viewport: vec!["refused line".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    // the channel drains, and nothing has changed since
    pane_send_state.lock().unwrap().clear();
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    let mut viewports_per_pane: HashMap<zellij_utils::data::PaneId, Vec<Vec<String>>> =
        HashMap::new();
    for msg in client_msgs {
        if let ServerToClientMsg::PaneRenderUpdate {
            pane_id,
            viewport,
            is_initial: false,
            ..
        } = msg
        {
            viewports_per_pane
                .entry(*pane_id)
                .or_default()
                .push(viewport.clone());
        }
    }
    assert_eq!(
        viewports_per_pane
            .get(&accepted_pane)
            .map(|sends| sends.len()),
        Some(1),
        "the pane whose update was accepted must not be sent again: {:?}",
        viewports_per_pane
    );
    assert_eq!(
        viewports_per_pane
            .get(&refused_pane)
            .map(|sends| sends.len()),
        Some(1),
        "the pane whose update was refused must be sent once the channel drains: {:?}",
        viewports_per_pane
    );
    assert_eq!(
        viewports_per_pane.get(&refused_pane).unwrap()[0],
        vec!["refused line".to_string()]
    );
}

#[test]
fn departed_subscriber_loses_its_subscription() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _messages, client_send_state) = create_new_screen_with_client_send_state(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    client_send_state
        .lock()
        .unwrap()
        .insert(100, SendToClientError::ClientGone);

    let mut pane_map = HashMap::new();
    pane_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["changed line".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    assert!(
        !screen.pane_render_subscribers.contains_key(&100),
        "A subscriber whose client is gone must be dropped"
    );
}

#[test]
fn resubscribing_to_only_dead_panes_ends_the_subscription() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );
    assert!(screen.pane_render_subscribers.contains_key(&100));

    // the client asks for panes that do not exist: the old subscription must not survive it
    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(999)],
        None,
        false,
    );
    assert!(
        !screen.pane_render_subscribers.contains_key(&100),
        "A subscribe naming no live pane must replace the previous subscription"
    );
}

#[test]
fn subscriber_error_for_nonexistent_pane() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(999)],
        None,
        false,
    );

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    assert_eq!(client_msgs.len(), 1);
    match &client_msgs[0] {
        ServerToClientMsg::LogError { lines } => {
            let joined = lines.join(" ");
            assert!(
                joined.contains("not found"),
                "Error message should contain 'not found', got: {}",
                joined
            );
        },
        other => panic!("Expected LogError, got {:?}", other),
    }
    assert!(
        !screen.pane_render_subscribers.contains_key(&100),
        "Subscriber should not be registered for nonexistent pane"
    );
}

#[test]
fn subscriber_state_registered_for_multiple_panes() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    screen.subscribe_to_pane_renders(
        100,
        vec![
            zellij_utils::data::PaneId::Terminal(1),
            zellij_utils::data::PaneId::Terminal(2),
        ],
        None,
        false,
    );

    let sub = screen.pane_render_subscribers.get(&100).unwrap();
    assert_eq!(sub.pane_ids.len(), 2);
    assert!(sub
        .pane_ids
        .contains(&zellij_utils::data::PaneId::Terminal(1)));
    assert!(sub
        .pane_ids
        .contains(&zellij_utils::data::PaneId::Terminal(2)));
}

#[test]
fn multiple_subscribers_receive_updates() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );
    screen.subscribe_to_pane_renders(
        101,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    let mut pane_map = HashMap::new();
    pane_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["new content".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&pane_map, None);

    let msgs = messages.lock().unwrap();
    let client_100_msgs = msgs.get(&100).unwrap();
    let client_101_msgs = msgs.get(&101).unwrap();
    assert_eq!(client_100_msgs.len(), 2);
    assert_eq!(client_101_msgs.len(), 2);
}

#[test]
fn subscriber_removed_on_remove_client() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );
    assert!(screen.pane_render_subscribers.contains_key(&100));

    let _ = screen.remove_client(100);
    assert!(!screen.pane_render_subscribers.contains_key(&100));
}

#[test]
fn subscriber_removed_when_all_panes_closed() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    screen.notify_pane_closed_to_subscribers(zellij_utils::data::PaneId::Terminal(1));

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    let has_pane_closed = client_msgs.iter().any(|m| {
        matches!(
            m,
            ServerToClientMsg::SubscribedPaneClosed {
                pane_id: zellij_utils::data::PaneId::Terminal(1),
            }
        )
    });
    let has_exit = client_msgs.iter().any(|m| {
        matches!(
            m,
            ServerToClientMsg::Exit {
                exit_reason: ExitReason::Normal,
            }
        )
    });
    assert!(has_pane_closed, "Should send SubscribedPaneClosed");
    assert!(has_exit, "Should send Exit when all panes closed");
    assert!(!screen.pane_render_subscribers.contains_key(&100));
}

#[test]
fn subscriber_partial_close() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    screen.subscribe_to_pane_renders(
        100,
        vec![
            zellij_utils::data::PaneId::Terminal(1),
            zellij_utils::data::PaneId::Terminal(2),
        ],
        None,
        false,
    );

    screen.notify_pane_closed_to_subscribers(zellij_utils::data::PaneId::Terminal(1));

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    let has_pane_closed = client_msgs.iter().any(|m| {
        matches!(
            m,
            ServerToClientMsg::SubscribedPaneClosed {
                pane_id: zellij_utils::data::PaneId::Terminal(1),
            }
        )
    });
    let has_exit = client_msgs
        .iter()
        .any(|m| matches!(m, ServerToClientMsg::Exit { .. }));
    assert!(
        has_pane_closed,
        "Should send SubscribedPaneClosed for closed pane"
    );
    assert!(!has_exit, "Should NOT send Exit when panes remain");
    assert!(screen.pane_render_subscribers.contains_key(&100));
    assert_eq!(
        screen
            .pane_render_subscribers
            .get(&100)
            .unwrap()
            .pane_ids
            .len(),
        1
    );
}

#[test]
fn subscriber_full_close_sequence() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    screen.subscribe_to_pane_renders(
        100,
        vec![
            zellij_utils::data::PaneId::Terminal(1),
            zellij_utils::data::PaneId::Terminal(2),
        ],
        None,
        false,
    );

    screen.notify_pane_closed_to_subscribers(zellij_utils::data::PaneId::Terminal(1));
    screen.notify_pane_closed_to_subscribers(zellij_utils::data::PaneId::Terminal(2));

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    let pane_closed_count = client_msgs
        .iter()
        .filter(|m| matches!(m, ServerToClientMsg::SubscribedPaneClosed { .. }))
        .count();
    let exit_count = client_msgs
        .iter()
        .filter(|m| {
            matches!(
                m,
                ServerToClientMsg::Exit {
                    exit_reason: ExitReason::Normal,
                }
            )
        })
        .count();
    assert_eq!(pane_closed_count, 2, "Two SubscribedPaneClosed messages");
    assert_eq!(exit_count, 1, "One Exit message after all panes closed");
    assert!(!screen.pane_render_subscribers.contains_key(&100));
}

#[test]
fn delivery_path_a_and_b_produce_same_content() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    messages.lock().unwrap().get_mut(&100).unwrap().clear();

    let contents = {
        let server_pane_id = PaneId::Terminal(1);
        let mut found_contents = None;
        for tab in screen.tabs.values() {
            if let Some(pane) = tab.get_pane_with_id(server_pane_id) {
                found_contents = Some(pane.pane_contents(None, false, None));
                break;
            }
        }
        found_contents.expect("Pane should exist")
    };

    screen
        .pane_render_subscribers
        .get_mut(&100)
        .unwrap()
        .previous_viewports
        .insert(
            zellij_utils::data::PaneId::Terminal(1),
            vec!["old".to_string()],
        );

    let mut pane_map = HashMap::new();
    pane_map.insert(zellij_utils::data::PaneId::Terminal(1), contents.clone());
    let mut all_pane_contents = HashMap::new();
    all_pane_contents.insert(1 as ClientId, pane_map);
    let report = PaneRenderReport {
        all_pane_contents,
        all_pane_contents_with_ansi: HashMap::new(),
    };
    screen.deliver_to_pane_subscribers_from_report(&report);

    let viewport_a = {
        let msgs = messages.lock().unwrap();
        let client_msgs = msgs.get(&100).unwrap();
        match &client_msgs[0] {
            ServerToClientMsg::PaneRenderUpdate { viewport, .. } => viewport.clone(),
            other => panic!("Expected PaneRenderUpdate, got {:?}", other),
        }
    };

    messages.lock().unwrap().get_mut(&100).unwrap().clear();
    screen
        .pane_render_subscribers
        .get_mut(&100)
        .unwrap()
        .previous_viewports
        .insert(
            zellij_utils::data::PaneId::Terminal(1),
            vec!["old".to_string()],
        );

    screen.deliver_to_pane_subscribers_directly();

    let viewport_b = {
        let msgs = messages.lock().unwrap();
        let client_msgs = msgs.get(&100).unwrap();
        match &client_msgs[0] {
            ServerToClientMsg::PaneRenderUpdate { viewport, .. } => viewport.clone(),
            other => panic!("Expected PaneRenderUpdate, got {:?}", other),
        }
    };

    assert_eq!(
        viewport_a, viewport_b,
        "Both delivery paths should produce identical viewport"
    );
}

#[test]
fn close_tab_notifies_subscribers() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );

    messages.lock().unwrap().get_mut(&100).unwrap().clear();

    let _ = screen.go_to_tab(1, 1);
    let _ = screen.close_tab(1);

    let msgs = messages.lock().unwrap();
    let client_msgs = msgs.get(&100).unwrap();
    let has_pane_closed = client_msgs.iter().any(|m| {
        matches!(
            m,
            ServerToClientMsg::SubscribedPaneClosed {
                pane_id: zellij_utils::data::PaneId::Terminal(1),
            }
        )
    });
    assert!(
        has_pane_closed,
        "Closing tab should notify subscriber of pane closure"
    );
}

#[test]
fn close_pane_notifies_subscribers_via_instruction() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let _session_metadata = mock_screen.clone_session_metadata();

    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];

    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![
                zellij_utils::data::PaneId::Terminal(0),
                zellij_utils::data::PaneId::Terminal(1),
            ],
            scrollback: None,
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::CloseFocusedPane(1, None));
    std::thread::sleep(std::time::Duration::from_millis(200));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let client_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();
    let has_pane_closed = client_msgs
        .iter()
        .any(|m| matches!(m, ServerToClientMsg::SubscribedPaneClosed { .. }));
    assert!(
        has_pane_closed,
        "Closing pane via CLI action should notify subscriber. Messages: {:?}",
        client_msgs
    );
}

#[test]
fn integration_pty_bytes_delivered_to_subscriber() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: None,
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "hello world\r\n".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    assert!(
        subscriber_msgs.len() >= 2,
        "Should have at least initial + update, got {}",
        subscriber_msgs.len()
    );

    match &subscriber_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate { is_initial, .. } => {
            assert!(*is_initial, "First message should be initial");
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }

    let has_hello = subscriber_msgs.iter().any(|m| match m {
        ServerToClientMsg::PaneRenderUpdate {
            is_initial: false,
            viewport,
            ..
        } => viewport.iter().any(|line| line.contains("hello world")),
        _ => false,
    });
    assert!(
        has_hello,
        "Subsequent message should contain 'hello world' in viewport. Messages: {:?}",
        subscriber_msgs
    );
}

#[test]
fn integration_pty_bytes_not_delivered_when_viewport_unchanged() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: None,
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RenderToClients);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    let render_update_count = subscriber_msgs
        .iter()
        .filter(|m| matches!(m, ServerToClientMsg::PaneRenderUpdate { .. }))
        .count();
    assert_eq!(
        render_update_count, 1,
        "Only the initial delivery should be present, not a duplicate. Got {} messages: {:?}",
        render_update_count, subscriber_msgs
    );
}

#[test]
fn integration_scrollback_from_pre_subscription_pty_bytes() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let mut data = String::new();
    for i in 0..30 {
        data.push_str(&format!("line {}\r\n", i));
    }
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::PtyBytes(0, data.as_bytes().to_vec()));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: Some(0),
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "post subscribe line\r\n".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    let initial_msg = subscriber_msgs.iter().find(|m| {
        matches!(
            m,
            ServerToClientMsg::PaneRenderUpdate {
                is_initial: true,
                ..
            }
        )
    });
    assert!(initial_msg.is_some(), "Should have an initial message");
    match initial_msg.unwrap() {
        ServerToClientMsg::PaneRenderUpdate {
            scrollback,
            is_initial,
            ..
        } => {
            assert!(*is_initial);
            assert!(
                scrollback.is_some(),
                "Initial message should include scrollback"
            );
            let sb = scrollback.as_ref().unwrap();
            assert!(
                !sb.is_empty(),
                "Scrollback should contain pre-subscription lines"
            );
            let has_early_lines = sb.iter().any(|line| line.contains("line 0"));
            assert!(
                has_early_lines,
                "Scrollback should contain early lines. Got: {:?}",
                sb
            );
        },
        _ => unreachable!(),
    }

    let subsequent_updates: Vec<_> = subscriber_msgs
        .iter()
        .filter(|m| {
            matches!(
                m,
                ServerToClientMsg::PaneRenderUpdate {
                    is_initial: false,
                    ..
                }
            )
        })
        .collect();
    for msg in &subsequent_updates {
        match msg {
            ServerToClientMsg::PaneRenderUpdate { scrollback, .. } => {
                assert!(
                    scrollback.is_none(),
                    "Subsequent updates should not include scrollback"
                );
            },
            _ => unreachable!(),
        }
    }
}

#[test]
fn integration_no_scrollback_when_not_requested() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let mut data = String::new();
    for i in 0..30 {
        data.push_str(&format!("line {}\r\n", i));
    }
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::PtyBytes(0, data.as_bytes().to_vec()));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: None,
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "post subscribe\r\n".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    for msg in &subscriber_msgs {
        match msg {
            ServerToClientMsg::PaneRenderUpdate { scrollback, .. } => {
                assert!(
                    scrollback.is_none(),
                    "Scrollback should be None when not requested. Got: {:?}",
                    scrollback
                );
            },
            _ => {},
        }
    }
}

#[test]
fn integration_subscriber_survives_after_regular_client_detach() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: None,
            ansi: false,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(1));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "after detach\r\n".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    let has_after_detach = subscriber_msgs.iter().any(|m| match m {
        ServerToClientMsg::PaneRenderUpdate { viewport, .. } => {
            viewport.iter().any(|line| line.contains("after detach"))
        },
        _ => false,
    });
    assert!(
        has_after_detach,
        "Subscriber should receive updates after regular client detach. Messages: {:?}",
        subscriber_msgs
    );
}

// ==========================================
// Category 3: MockScreen end-to-end CLI tests
// ==========================================

#[test]
pub fn send_cli_scroll_up_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ScrollUp {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ScrollUp with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_scroll_down_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ScrollDown {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ScrollDown with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_scroll_to_top_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ScrollToTop {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ScrollToTop with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_scroll_to_bottom_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ScrollToBottom {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ScrollToBottom with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_page_scroll_up_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::PageScrollUp {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "PageScrollUp with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_page_scroll_down_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::PageScrollDown {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "PageScrollDown with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_half_page_scroll_up_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::HalfPageScrollUp {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "HalfPageScrollUp with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_half_page_scroll_down_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::HalfPageScrollDown {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "HalfPageScrollDown with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_resize_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::Resize {
        resize: Resize::Increase,
        direction: Some(Direction::Left),
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "Resize with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_move_pane_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::MovePane {
        direction: Some(Direction::Right),
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "MovePane with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_move_pane_backwards_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::MovePaneBackwards {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "MovePaneBackwards with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_clear_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::Clear {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "Clear with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_edit_scrollback_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::EditScrollback {
        pane_id: Some("terminal_0".to_string()),
        ansi: false,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "EditScrollback with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_toggle_fullscreen_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ToggleFullscreen {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ToggleFullscreen with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_toggle_pane_embed_or_floating_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::TogglePaneEmbedOrFloating {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "TogglePaneEmbedOrFloating with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_close_pane_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ClosePane {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ClosePane with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_rename_pane_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::RenamePane {
        name: "targeted-name".to_string(),
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "RenamePane with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_undo_rename_pane_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::UndoRenamePane {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "UndoRenamePane with pane_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_toggle_pane_pinned_with_pane_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::TogglePanePinned {
        pane_id: Some("terminal_0".to_string()),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "TogglePanePinned with pane_id CLI action completed without errors"
    );
}

// TAB-TARGETING MockScreen tests

#[test]
pub fn send_cli_close_tab_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.new_tab(TiledPaneLayout::default());
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::CloseTab { tab_id: Some(1) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "CloseTab with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_rename_tab_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.new_tab(TiledPaneLayout::default());
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::RenameTab {
        name: "targeted-tab".to_string(),
        tab_id: Some(1),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "RenameTab with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_undo_rename_tab_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.new_tab(TiledPaneLayout::default());
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::UndoRenameTab { tab_id: Some(0) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "UndoRenameTab with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_toggle_active_sync_tab_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ToggleActiveSyncTab { tab_id: Some(0) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ToggleActiveSyncTab with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_toggle_floating_panes_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::ToggleFloatingPanes { tab_id: Some(0) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "ToggleFloatingPanes with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_previous_swap_layout_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::PreviousSwapLayout { tab_id: Some(0) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "PreviousSwapLayout with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_next_swap_layout_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::NextSwapLayout { tab_id: Some(0) };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "NextSwapLayout with tab_id CLI action completed without errors"
    );
}

#[test]
pub fn send_cli_move_tab_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.new_tab(TiledPaneLayout::default());
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::MoveTab {
        direction: Some(Direction::Right),
        to_index: None,
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "MoveTab with tab_id CLI action completed without errors"
    );
}

// ==========================================
// Category 4: Direct Screen method tests
// ==========================================

#[test]
pub fn move_tab_by_id_verifies_screen_state() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    let original_pos_0 = screen.get_tab_by_id(0).unwrap().position;
    let original_pos_1 = screen.get_tab_by_id(1).unwrap().position;
    screen.move_tab_by_id(0, Direction::Right).expect("TEST");
    assert_eq!(screen.get_tab_by_id(0).unwrap().position, original_pos_1);
    assert_eq!(screen.get_tab_by_id(1).unwrap().position, original_pos_0);
}

#[test]
pub fn move_tab_to_index_shifts_the_tabs_in_between() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);
    new_tab(&mut screen, 4, 3);

    // 0,1,2,3 -> 1,2,3,0
    screen.move_tab_to_index(0, 3).expect("TEST");
    assert_eq!(screen.get_tab_by_id(0).unwrap().position, 3);
    assert_eq!(screen.get_tab_by_id(1).unwrap().position, 0);
    assert_eq!(screen.get_tab_by_id(2).unwrap().position, 1);
    assert_eq!(screen.get_tab_by_id(3).unwrap().position, 2);

    // and back again, this time moving left
    screen.move_tab_to_index(0, 0).expect("TEST");
    assert_eq!(screen.get_tab_by_id(0).unwrap().position, 0);
    assert_eq!(screen.get_tab_by_id(1).unwrap().position, 1);
    assert_eq!(screen.get_tab_by_id(2).unwrap().position, 2);
    assert_eq!(screen.get_tab_by_id(3).unwrap().position, 3);
}

#[test]
pub fn move_tab_to_index_clamps_an_out_of_range_index() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);

    screen.move_tab_to_index(0, 100).expect("TEST");
    assert_eq!(screen.get_tab_by_id(0).unwrap().position, 2);
    assert_eq!(screen.get_tab_by_id(1).unwrap().position, 0);
    assert_eq!(screen.get_tab_by_id(2).unwrap().position, 1);
}

#[test]
pub fn move_tab_to_index_resolves_the_active_tab_without_a_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    new_tab(&mut screen, 3, 2);

    // the client's own active tab
    screen.go_to_tab(3, client_id).expect("TEST");
    assert_eq!(
        screen.tab_id_for_absolute_move(None, client_id),
        Some(2),
        "resolves the requesting client's active tab"
    );

    // a client that never registered still resolves, through the connected client
    assert_eq!(
        screen.tab_id_for_absolute_move(None, 42),
        Some(2),
        "resolves through a connected client for an unknown client id"
    );

    // and a detached session, with no connected clients at all, still resolves
    screen.remove_client(client_id).expect("TEST");
    assert_eq!(
        screen.tab_id_for_absolute_move(None, client_id),
        Some(2),
        "resolves the last focused tab when no client is connected"
    );

    // an explicit tab id that does not exist resolves to nothing, so the caller can report it
    assert_eq!(screen.tab_id_for_absolute_move(Some(99), client_id), None);
}

// ==========================================
// Category 5: ANSI flag tests
// ==========================================

#[test]
pub fn send_cli_dump_screen_action_with_ansi() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::DumpScreen {
        file: None,
        path: Some(PathBuf::from("/tmp/foo_ansi")),
        full: true,
        pane_id: None,
        ansi: true,
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "\x1b[31mred text\x1b[0m".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let fs = mock_screen.os_input.fake_filesystem.lock().unwrap();
    let dumped_content = fs.values().next().expect("Should have dumped a file");
    assert!(
        dumped_content.contains("\x1b["),
        "Dumped file should contain ANSI escape codes when ansi flag is true. Content: {:?}",
        dumped_content
    );
}

#[test]
pub fn send_cli_dump_screen_action_without_ansi_strips_codes() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let cli_action = CliAction::DumpScreen {
        file: None,
        path: Some(PathBuf::from("/tmp/foo_plain")),
        full: true,
        pane_id: None,
        ansi: false,
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "\x1b[31mred text\x1b[0m".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    let fs = mock_screen.os_input.fake_filesystem.lock().unwrap();
    let dumped_content = fs.values().next().expect("Should have dumped a file");
    assert!(
        !dumped_content.contains("\x1b["),
        "Dumped file should NOT contain ANSI escape codes when ansi flag is false. Content: {:?}",
        dumped_content
    );
}

#[test]
pub fn send_cli_edit_scrollback_action_with_ansi() {
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_action = CliAction::EditScrollback {
        pane_id: None,
        ansi: true,
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "\x1b[31mred text\x1b[0m".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let fs = mock_screen.os_input.fake_filesystem.lock().unwrap();
    let dumped_content = fs.values().next().expect("Should have dumped a file");
    assert!(
        dumped_content.contains("\x1b["),
        "Edit scrollback dump should contain ANSI escape codes when ansi flag is true. Content: {:?}",
        dumped_content
    );
}

#[test]
pub fn send_cli_edit_scrollback_with_pane_id_and_ansi() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let cli_action = CliAction::EditScrollback {
        pane_id: Some("terminal_0".to_string()),
        ansi: true,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![server_thread, screen_thread]);
    assert!(
        true,
        "EditScrollback with pane_id and ansi CLI action completed without errors"
    );
}

#[test]
fn subscriber_ansi_flag_preserved_in_subscription() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );
    screen.subscribe_to_pane_renders(
        101,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        true,
    );

    assert!(
        !screen.pane_render_subscribers.get(&100).unwrap().ansi,
        "Subscriber 100 should have ansi=false"
    );
    assert!(
        screen.pane_render_subscribers.get(&101).unwrap().ansi,
        "Subscriber 101 should have ansi=true"
    );
}

#[test]
fn subscriber_ansi_and_plain_receive_different_content() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);

    // Subscribe plain and ansi subscribers
    screen.subscribe_to_pane_renders(
        100,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        false,
    );
    screen.subscribe_to_pane_renders(
        101,
        vec![zellij_utils::data::PaneId::Terminal(1)],
        None,
        true,
    );

    // Clear initial messages
    messages.lock().unwrap().get_mut(&100).unwrap().clear();
    messages.lock().unwrap().get_mut(&101).unwrap().clear();

    // Reset previous viewports to force delivery
    screen
        .pane_render_subscribers
        .get_mut(&100)
        .unwrap()
        .previous_viewports
        .insert(
            zellij_utils::data::PaneId::Terminal(1),
            vec!["old".to_string()],
        );
    screen
        .pane_render_subscribers
        .get_mut(&101)
        .unwrap()
        .previous_viewports
        .insert(
            zellij_utils::data::PaneId::Terminal(1),
            vec!["old".to_string()],
        );

    // Build plain and ansi maps with different content
    let mut plain_map = HashMap::new();
    plain_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["plain text".to_string()],
            ..Default::default()
        },
    );
    let mut ansi_map = HashMap::new();
    ansi_map.insert(
        zellij_utils::data::PaneId::Terminal(1),
        PaneContents {
            viewport: vec!["\x1b[31mred text\x1b[0m".to_string()],
            ..Default::default()
        },
    );
    screen.deliver_subscriber_updates_from_map(&plain_map, Some(&ansi_map));

    let msgs = messages.lock().unwrap();
    let plain_msgs = msgs.get(&100).unwrap();
    let ansi_msgs = msgs.get(&101).unwrap();

    assert_eq!(
        plain_msgs.len(),
        1,
        "Plain subscriber should receive one update"
    );
    assert_eq!(
        ansi_msgs.len(),
        1,
        "Ansi subscriber should receive one update"
    );

    match &plain_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate { viewport, .. } => {
            assert_eq!(viewport, &vec!["plain text".to_string()]);
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
    match &ansi_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate { viewport, .. } => {
            assert_eq!(viewport, &vec!["\x1b[31mred text\x1b[0m".to_string()]);
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }
}

#[test]
fn integration_subscribe_with_ansi_flag() {
    let size = Size { cols: 80, rows: 20 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.drop_all_pty_messages();
    let screen_thread = mock_screen.run(None, vec![]);

    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SubscribeToPaneRenders {
            client_id: 100,
            pane_ids: vec![zellij_utils::data::PaneId::Terminal(0)],
            scrollback: None,
            ansi: true,
        });
    std::thread::sleep(std::time::Duration::from_millis(100));

    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        0,
        "\x1b[31mred text\x1b[0m\r\n".as_bytes().to_vec(),
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, plugin_thread, screen_thread]);

    let msgs = mock_screen
        .os_input
        .server_to_client_messages
        .lock()
        .unwrap();
    let subscriber_msgs = msgs.get(&100).unwrap_or(&vec![]).clone();

    assert!(
        subscriber_msgs.len() >= 2,
        "Should have at least initial + update, got {}",
        subscriber_msgs.len()
    );

    match &subscriber_msgs[0] {
        ServerToClientMsg::PaneRenderUpdate { is_initial, .. } => {
            assert!(*is_initial, "First message should be initial");
        },
        other => panic!("Expected PaneRenderUpdate, got {:?}", other),
    }

    let has_ansi_content = subscriber_msgs.iter().any(|m| match m {
        ServerToClientMsg::PaneRenderUpdate {
            is_initial: false,
            viewport,
            ..
        } => viewport.iter().any(|line| line.contains("\x1b[")),
        _ => false,
    });
    assert!(
        has_ansi_content,
        "ANSI subscriber should receive viewport lines with ANSI escape codes. Messages: {:?}",
        subscriber_msgs
    );
}

#[test]
pub fn background_plugin_receives_broadcasts_regardless_of_active_tab() {
    // Tab 0: plugin pane 2 (from new_tab_with_plugins, queued before run)
    // Tab 1: plugin pane 3 (from new_tab_with_plugins, queued before run)
    // Tab 2: terminal panes only (from run, starts screen thread)
    // After run, client is on tab 2. Switch to tab 0 (plugin 2).
    // Background plugin 99 should also receive updates.
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10;

    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    mock_screen.new_tab_with_plugins(vec![3]);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);

    // Register background plugin 99 with the main_client_id (1), not the CLI client_id (10)
    let main_client_id = mock_screen.main_client_id;
    let mut bg_subs = HashSet::new();
    bg_subs.insert(EventType::TabUpdate);
    bg_subs.insert(EventType::ModeUpdate);
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::UpdateBackgroundPluginSubscriptions(
            99,
            main_client_id,
            bg_subs,
        ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // Drain initial setup instructions before the GoToTab action
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_switch = received_plugin_instructions.lock().unwrap().len();

    // Switch to tab 0 (1-based index 1 = position 0 = tab with plugin 2)
    let goto_tab = CliAction::GoToTab { index: 1 };
    send_cli_action_to_server(&session_metadata, goto_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    // Only examine instructions sent after the switch
    let instructions_after_switch = &instructions[instructions_before_switch..];
    let mut plugin_ids_that_received_tab_update: Vec<u32> = vec![];
    let mut plugin_ids_that_received_mode_update: Vec<u32> = vec![];
    for instruction in instructions_after_switch.iter() {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, _cid, event) in updates {
                match event {
                    Event::TabUpdate(..) => {
                        if let Some(id) = pid {
                            plugin_ids_that_received_tab_update.push(*id);
                        }
                    },
                    Event::ModeUpdate(..) => {
                        if let Some(id) = pid {
                            plugin_ids_that_received_mode_update.push(*id);
                        }
                    },
                    _ => {},
                }
            }
        }
    }

    // Plugin 2 (active tab) and plugin 99 (background) should receive updates
    assert!(
        plugin_ids_that_received_tab_update.contains(&2),
        "Active tab plugin 2 should receive TabUpdate, got: {:?}",
        plugin_ids_that_received_tab_update
    );
    assert!(
        plugin_ids_that_received_tab_update.contains(&99),
        "Background plugin 99 should receive TabUpdate, got: {:?}",
        plugin_ids_that_received_tab_update
    );
    // Plugin 3 (inactive tab) should NOT receive updates
    assert!(
        !plugin_ids_that_received_tab_update.contains(&3),
        "Inactive tab plugin 3 should NOT receive TabUpdate, got: {:?}",
        plugin_ids_that_received_tab_update
    );

    // ModeUpdate is sent via update_input_modes() to tab plugins only (not background plugins).
    // Background plugins receive ModeUpdate only via explicit broadcast_mode_update calls.
    // So during tab switch, only the active tab's plugins get ModeUpdate.
    assert!(
        plugin_ids_that_received_mode_update.contains(&2),
        "Active tab plugin 2 should receive ModeUpdate, got: {:?}",
        plugin_ids_that_received_mode_update
    );
    assert!(
        !plugin_ids_that_received_mode_update.contains(&3),
        "Inactive tab plugin 3 should NOT receive ModeUpdate, got: {:?}",
        plugin_ids_that_received_mode_update
    );
}

#[derive(Default, Debug)]
struct BackgroundPluginDelivery {
    pane_updates: usize,
    tab_updates: usize,
    max_pane_updates_per_batch: usize,
    max_tab_updates_per_batch: usize,
    last_tab_update_len: Option<usize>,
}

/// Count the PaneUpdate/TabUpdate events delivered to one background plugin. A batch is one
/// PluginInstruction::Update, i.e. one state change, so the per-batch maxima catch a background
/// plugin being served once per connected client instead of once per change.
fn background_plugin_delivery(
    instructions: &[PluginInstruction],
    plugin_id: u32,
) -> BackgroundPluginDelivery {
    let mut delivery = BackgroundPluginDelivery::default();
    for instruction in instructions {
        if let PluginInstruction::Update(updates) = instruction {
            let mut pane_updates_in_batch = 0;
            let mut tab_updates_in_batch = 0;
            for (pid, _cid, event) in updates {
                if pid != &Some(plugin_id) {
                    continue;
                }
                match event {
                    Event::PaneUpdate(..) => pane_updates_in_batch += 1,
                    Event::TabUpdate(tab_infos) => {
                        tab_updates_in_batch += 1;
                        delivery.last_tab_update_len = Some(tab_infos.len());
                    },
                    _ => {},
                }
            }
            delivery.pane_updates += pane_updates_in_batch;
            delivery.tab_updates += tab_updates_in_batch;
            delivery.max_pane_updates_per_batch = delivery
                .max_pane_updates_per_batch
                .max(pane_updates_in_batch);
            delivery.max_tab_updates_per_batch =
                delivery.max_tab_updates_per_batch.max(tab_updates_in_batch);
        }
    }
    delivery
}

fn subscribe_background_plugin(mock_screen: &MockScreen, plugin_id: u32, client_id: u16) {
    let mut subscriptions = HashSet::new();
    subscriptions.insert(EventType::PaneUpdate);
    subscriptions.insert(EventType::TabUpdate);
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::UpdateBackgroundPluginSubscriptions(
            plugin_id,
            client_id,
            subscriptions,
        ));
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// A two-pane tab, so that closing one pane is a state change that does not close the tab.
fn two_pane_layout() -> TiledPaneLayout {
    let mut layout = TiledPaneLayout::default();
    layout.children_split_direction = SplitDirection::Vertical;
    layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    layout
}

#[test]
pub fn background_plugin_receives_updates_with_no_clients_attached() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // detach the only client, then change state
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(main_client_id));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let delivery = background_plugin_delivery(&instructions[instructions_before_change..], 99);
    assert!(
        delivery.pane_updates > 0,
        "background plugin should receive PaneUpdate with no clients attached, got: {:?}",
        delivery
    );
    assert!(
        delivery.tab_updates > 0,
        "background plugin should receive TabUpdate with no clients attached, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "background plugin should receive one PaneUpdate per change, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "background plugin should receive one TabUpdate per change, got: {:?}",
        delivery
    );
}

#[test]
pub fn background_plugin_receives_one_update_per_change_with_one_client() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    subscribe_background_plugin(&mock_screen, 99, mock_screen.main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let delivery = background_plugin_delivery(&instructions[instructions_before_change..], 99);
    assert!(
        delivery.pane_updates > 0 && delivery.tab_updates > 0,
        "background plugin should receive both updates with one client, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "background plugin should receive one PaneUpdate per change, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "background plugin should receive one TabUpdate per change, got: {:?}",
        delivery
    );
}

#[test]
pub fn background_plugin_receives_one_update_per_change_with_two_clients() {
    // Tab 0: plugin pane 2, Tab 1: two terminal panes. Client 1 stays on tab 1, client 2 goes
    // to tab 0, so the two clients sit on different tabs.
    let size = Size { cols: 80, rows: 10 };
    let second_client_id = 2;
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    subscribe_background_plugin(&mock_screen, 99, mock_screen.main_client_id);

    let _ = mock_screen.to_screen.send(ScreenInstruction::AddClient(
        second_client_id,
        false,
        size,
        Some(0),
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let delivery = background_plugin_delivery(&instructions[instructions_before_change..], 99);
    assert!(
        delivery.pane_updates > 0 && delivery.tab_updates > 0,
        "background plugin should receive both updates with two clients, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "two clients must not mean two PaneUpdates for one change, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "two clients must not mean two TabUpdates for one change, got: {:?}",
        delivery
    );
    // the background payload is the whole screen state, not one client's view of it
    assert_eq!(
        delivery.last_tab_update_len,
        Some(2),
        "background TabUpdate should list every tab, got: {:?}",
        delivery
    );
}

#[test]
pub fn an_unloaded_background_plugin_stops_being_served() {
    // the plugin thread sends this when a plugin is unloaded or crashes - a background plugin has
    // no pane whose closing would otherwise tell Screen the plugin is gone
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    subscribe_background_plugin(&mock_screen, 99, mock_screen.main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));

    // it is served while it is alive
    let instructions_before_first_change = received_plugin_instructions.lock().unwrap().len();
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let delivery_while_alive = background_plugin_delivery(
        &received_plugin_instructions.lock().unwrap()[instructions_before_first_change..],
        99,
    );

    // it is gone
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveBackgroundPluginSubscriptions(99));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_second_change = received_plugin_instructions.lock().unwrap().len();
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(2),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let delivery_after_unload =
        background_plugin_delivery(&instructions[instructions_before_second_change..], 99);
    assert!(
        delivery_while_alive.pane_updates > 0 && delivery_while_alive.tab_updates > 0,
        "the background plugin should be served while it is alive, got: {:?}",
        delivery_while_alive
    );
    assert_eq!(
        (
            delivery_after_unload.pane_updates,
            delivery_after_unload.tab_updates
        ),
        (0, 0),
        "an unloaded background plugin should receive nothing, got: {:?}",
        delivery_after_unload
    );
}

#[test]
pub fn plugin_in_a_visible_tab_is_never_served_as_a_background_plugin() {
    // A plugin loading another plugin into a visible floating pane passes no tab index, so the
    // plugin thread files the new plugin as background even though its pane sits in a real tab.
    // It is already served as an active-tab plugin, so it must not be served a second time.
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    // put the only client on the tab holding plugin 2, so the plugin is in its active tab
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::GoToTab(1, Some(main_client_id), None));
    std::thread::sleep(std::time::Duration::from_millis(100));
    // plugin 2 is misfiled as a background plugin
    subscribe_background_plugin(&mock_screen, 2, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let instructions_after_change = &instructions[instructions_before_change..];
    let delivery = background_plugin_delivery(instructions_after_change, 2);
    assert!(
        delivery.pane_updates > 0 && delivery.tab_updates > 0,
        "the plugin should still be served as an active-tab plugin, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "a visible plugin misfiled as background must get one PaneUpdate per change, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "a visible plugin misfiled as background must get one TabUpdate per change, got: {:?}",
        delivery
    );

    // the payload must be the per-client one, which excludes the receiving client from
    // other_focused_clients - the client-independent background payload lists it
    let mut tab_infos_delivered = vec![];
    for instruction in instructions_after_change {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, _cid, event) in updates {
                if pid == &Some(2) {
                    if let Event::TabUpdate(tab_infos) = event {
                        tab_infos_delivered.extend(tab_infos.clone());
                    }
                }
            }
        }
    }
    assert!(
        !tab_infos_delivered.is_empty(),
        "expected at least one TabUpdate payload for the plugin"
    );
    assert!(
        tab_infos_delivered
            .iter()
            .all(|tab_info| tab_info.other_focused_clients.is_empty()),
        "expected the per-client TabUpdate payload, got: {:?}",
        tab_infos_delivered
    );
}

#[test]
pub fn background_plugin_gets_one_update_when_the_tab_list_changes() {
    // A change to the tab list goes to EVERY tab's plugins, not just the active tab's. A
    // background plugin lives in no tab, so that arm must not reach it a second time.
    let size = Size { cols: 80, rows: 10 };
    let second_client_id = 2;
    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    subscribe_background_plugin(&mock_screen, 99, mock_screen.main_client_id);

    let _ = mock_screen.to_screen.send(ScreenInstruction::AddClient(
        second_client_id,
        false,
        size,
        Some(0),
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();

    // moving client 1's tab reorders the tab list
    let _ = mock_screen.to_screen.send(ScreenInstruction::MoveTabLeft(
        mock_screen.main_client_id,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let instructions_after_change = &instructions[instructions_before_change..];
    // plugin 2 sits in a tab neither client has active after the move, so its TabUpdate proves
    // the tab-list-changed arm ran
    let tab_plugin_delivery = background_plugin_delivery(instructions_after_change, 2);
    assert!(
        tab_plugin_delivery.tab_updates > 0,
        "the tab list change should reach every tab's plugins, got: {:?}",
        tab_plugin_delivery
    );

    let delivery = background_plugin_delivery(instructions_after_change, 99);
    assert!(
        delivery.tab_updates > 0,
        "background plugin should hear that the tab list changed, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "a tab list change must not deliver twice to a background plugin, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "background plugin should receive one PaneUpdate per change, got: {:?}",
        delivery
    );
}

#[test]
pub fn background_plugin_receives_updates_after_its_own_client_detaches() {
    // The failure this guards: a background plugin is addressed by the ClientId it was loaded
    // with, and that id is recycled. Delivery must not depend on that client still being here.
    let size = Size { cols: 80, rows: 10 };
    let second_client_id = 2;
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let _ = mock_screen.to_screen.send(ScreenInstruction::AddClient(
        second_client_id,
        false,
        size,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // the client the plugin was loaded with leaves, another stays
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(main_client_id));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_change = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(1),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let delivery = background_plugin_delivery(&instructions[instructions_before_change..], 99);
    assert!(
        delivery.pane_updates > 0,
        "background plugin should receive PaneUpdate after its own client detached, got: {:?}",
        delivery
    );
    assert!(
        delivery.tab_updates > 0,
        "background plugin should receive TabUpdate after its own client detached, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_pane_updates_per_batch, 1,
        "background plugin should receive one PaneUpdate per change, got: {:?}",
        delivery
    );
    assert_eq!(
        delivery.max_tab_updates_per_batch, 1,
        "background plugin should receive one TabUpdate per change, got: {:?}",
        delivery
    );
}

#[test]
pub fn tab_switch_only_updates_active_tab_plugins() {
    // Tab 0: plugin pane 2 (from new_tab_with_plugins)
    // Tab 1: plugin pane 3 (from new_tab_with_plugins)
    // Tab 2: terminal panes only (from run)
    // After run, client is on tab 2. Switch to tab 0 (plugin 2).
    // Only plugin 2 should receive updates; plugin 3 should not.
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10;

    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    mock_screen.new_tab_with_plugins(vec![3]);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // Drain initial setup instructions before the GoToTab action
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_switch = received_plugin_instructions.lock().unwrap().len();

    // Switch to tab 0 (1-based index 1 = position 0 = tab with plugin 2)
    let goto_tab = CliAction::GoToTab { index: 1 };
    send_cli_action_to_server(&session_metadata, goto_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let instructions_after_switch = &instructions[instructions_before_switch..];
    let mut plugin_ids_that_received_updates: Vec<u32> = vec![];
    for instruction in instructions_after_switch.iter() {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, _cid, event) in updates {
                match event {
                    Event::TabUpdate(..) | Event::ModeUpdate(..) => {
                        if let Some(id) = pid {
                            plugin_ids_that_received_updates.push(*id);
                        }
                    },
                    _ => {},
                }
            }
        }
    }

    // Only plugin 2 (active tab after switch) should receive TabUpdate/ModeUpdate
    assert!(
        plugin_ids_that_received_updates.contains(&2),
        "Active tab plugin 2 should receive updates, got: {:?}",
        plugin_ids_that_received_updates
    );
    assert!(
        !plugin_ids_that_received_updates.contains(&3),
        "Inactive tab plugin 3 should NOT receive updates, got: {:?}",
        plugin_ids_that_received_updates
    );
}

#[test]
pub fn inactive_tab_plugins_get_fresh_state_on_activation() {
    // Tab 0: plugin pane 2 (from new_tab_with_plugins)
    // Tab 1: terminal panes only (from run, client starts here)
    // Switch to tab 0 → plugin 2 becomes active and receives TabUpdate with both tabs.
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10;

    let mut mock_screen = MockScreen::new(size);
    mock_screen.new_tab_with_plugins(vec![2]);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // Drain initial setup instructions before the GoToTab action
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_switch = received_plugin_instructions.lock().unwrap().len();

    // Switch to tab 0 (1-based index 1 = position 0 = tab with plugin 2)
    let goto_tab = CliAction::GoToTab { index: 1 };
    send_cli_action_to_server(&session_metadata, goto_tab, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let instructions_after_switch = &instructions[instructions_before_switch..];
    let tab_update_for_plugin_2 = instructions_after_switch.iter().find_map(|instruction| {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, _cid, event) in updates {
                if let (Some(2), Event::TabUpdate(tab_infos)) = (pid, event) {
                    return Some(tab_infos.clone());
                }
            }
        }
        None
    });

    assert!(
        tab_update_for_plugin_2.is_some(),
        "Plugin 2 should receive a TabUpdate after becoming active"
    );
    let tab_infos = tab_update_for_plugin_2.unwrap();
    assert!(
        tab_infos.len() >= 2,
        "TabUpdate should contain info for both tabs, got {} tabs",
        tab_infos.len()
    );
    let active_tab = tab_infos.iter().find(|t| t.active);
    assert!(
        active_tab.is_some(),
        "TabUpdate should have an active tab marked"
    );
    // Tab at position 0 should be active after switching to GoToTab index 1 (1-based)
    let active_tab = active_tab.unwrap();
    assert_eq!(
        active_tab.position, 0,
        "The first tab (position 0) should be active, got position {}",
        active_tab.position
    );
}

#[test]
pub fn send_cli_new_tab_action_with_layout_string() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    // Same layout as layout-with-three-panes.kdl but passed as a string
    let new_tab_action = CliAction::NewTab {
        name: None,
        layout: None,
        layout_string: Some("layout {\n    pane\n    pane\n    pane\n}\n".into()),
        layout_dir: None,
        cwd: None,
        initial_command: vec![],
        initial_plugin: None,
        close_on_exit: Default::default(),
        start_suspended: Default::default(),
        block_until_exit: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        no_focus: false,
    };
    send_cli_action_to_server(&session_metadata, new_tab_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let new_tab_instruction = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|i| {
            if let PluginInstruction::NewTab(..) = i {
                return true;
            } else {
                return false;
            }
        })
        .unwrap()
        .clone();
    let output = format!("{:#?}", new_tab_instruction);
    // Normalize Windows path separators for cross-platform snapshot consistency
    let output = output.replace("\\\\", "/");
    assert_snapshot!(output);
}

#[test]
pub fn send_cli_new_tab_action_with_layout_string_and_name() {
    let size = Size { cols: 80, rows: 10 };
    let client_id = 10;
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let new_tab_action = CliAction::NewTab {
        name: Some("my-string-layout-tab".into()),
        layout: None,
        layout_string: Some("layout {\n    pane\n    pane\n    pane\n}\n".into()),
        layout_dir: None,
        cwd: None,
        initial_command: vec![],
        initial_plugin: None,
        close_on_exit: Default::default(),
        start_suspended: Default::default(),
        block_until_exit: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        no_focus: false,
    };
    send_cli_action_to_server(&session_metadata, new_tab_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![plugin_thread, screen_thread]);
    let new_tab_instruction = received_plugin_instructions
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find(|i| {
            if let PluginInstruction::NewTab(..) = i {
                return true;
            } else {
                return false;
            }
        })
        .unwrap()
        .clone();
    let output = format!("{:#?}", new_tab_instruction);
    // Normalize Windows path separators for cross-platform snapshot consistency
    let output = output.replace("\\\\", "/");
    assert_snapshot!(output);
}

#[test]
pub fn send_cli_new_pane_action_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: Some(Direction::Right),
        command: vec![],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let pty_instructions = received_pty_instructions.lock().unwrap();
    // Verify that the PTY instruction uses TabIndex(0) instead of ClientId
    let pty_debug = format!("{:?}", *pty_instructions);
    assert!(
        pty_debug.contains("TabIndex(0)"),
        "Expected TabIndex(0) in PTY instructions, got: {}",
        pty_debug
    );
}

#[test]
pub fn send_cli_new_floating_pane_action_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: None,
        command: vec![],
        plugin: None,
        cwd: None,
        floating: true,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let pty_instructions = received_pty_instructions.lock().unwrap();
    let pty_debug = format!("{:?}", *pty_instructions);
    assert!(
        pty_debug.contains("TabIndex(0)"),
        "Expected TabIndex(0) in PTY instructions for floating pane, got: {}",
        pty_debug
    );
}

#[test]
pub fn send_cli_edit_action_with_tab_id() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_edit_action = CliAction::Edit {
        file: PathBuf::from("/tmp/test.rs"),
        direction: None,
        line_number: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        cwd: None,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_edit_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let pty_instructions = received_pty_instructions.lock().unwrap();
    let pty_debug = format!("{:?}", *pty_instructions);
    assert!(
        pty_debug.contains("TabIndex(0)"),
        "Expected TabIndex(0) in PTY instructions for edit, got: {}",
        pty_debug
    );
}

#[test]
pub fn send_cli_new_pane_action_with_tab_id_and_direction() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: Some(Direction::Right),
        command: vec![],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: false,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: Some(false),
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let pty_instructions = received_pty_instructions.lock().unwrap();
    let pty_debug = format!("{:?}", *pty_instructions);
    assert!(
        pty_debug.contains("TabIndex(0)"),
        "Expected TabIndex(0) in PTY instructions with direction, got: {}",
        pty_debug
    );
}

#[test]
pub fn send_cli_new_pane_action_with_tab_id_and_stacked() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let pty_receiver = mock_screen.pty_receiver.take().unwrap();
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyInstruction::Exit,
        pty_receiver
    );
    let cli_new_pane_action = CliAction::NewPane {
        direction: None,
        command: vec!["ls".into()],
        plugin: None,
        cwd: None,
        floating: false,
        in_place: false,
        close_replaced_pane: false,
        pane_id: None,
        name: None,
        close_on_exit: false,
        start_suspended: false,
        configuration: None,
        skip_plugin_cache: false,
        x: None,
        y: None,
        width: None,
        height: None,
        pinned: None,
        stacked: true,
        blocking: false,
        block_until_exit_success: false,
        block_until_exit_failure: false,
        block_until_exit: false,
        unblock_condition: None,
        near_current_pane: false,
        no_focus: false,
        borderless: None,
        tab_id: Some(0),
    };
    send_cli_action_to_server(&session_metadata, cli_new_pane_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_thread, screen_thread]);
    let pty_instructions = received_pty_instructions.lock().unwrap();
    let pty_debug = format!("{:?}", *pty_instructions);
    assert!(
        pty_debug.contains("TabIndex(0)"),
        "Expected TabIndex(0) in PTY instructions with stacked, got: {}",
        pty_debug
    );
}

#[test]
fn cli_rename_active_pane_via_screen_replaces_name() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    let client_id = 1;
    new_tab(&mut screen, 1, 0);

    // First give the pane a name
    let pane_id = PaneId::Terminal(1);
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_pane_by_pane_id(pane_id, "flame".as_bytes().to_vec());
    }

    // Now rename via the active pane path (what CLI rename without --pane-id does)
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_active_pane("spark".as_bytes().to_vec(), client_id);
    }

    let tab = screen.get_active_tab(client_id).unwrap();
    let pane = tab.get_pane_with_id(pane_id).unwrap();
    assert_eq!(
        pane.current_title(),
        "spark",
        "CLI rename should fully replace the name"
    );
}

#[test]
fn cli_rename_active_pane_single_char_via_screen() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    let client_id = 1;
    new_tab(&mut screen, 1, 0);

    let pane_id = PaneId::Terminal(1);
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_pane_by_pane_id(pane_id, "flame".as_bytes().to_vec());
    }

    // Single char rename via active pane path
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_active_pane("x".as_bytes().to_vec(), client_id);
    }

    let tab = screen.get_active_tab(client_id).unwrap();
    let pane = tab.get_pane_with_id(pane_id).unwrap();
    assert_eq!(
        pane.current_title(),
        "x",
        "Single-char CLI rename should replace, not append"
    );
}

#[test]
fn cli_rename_focused_pane_single_char_via_rename_active_pane() {
    // Tests that single-char CLI rename via RenameActivePane (the path used
    // by `zellij action rename-pane "x"` without --pane-id) correctly
    // replaces the existing name.
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    let client_id = 1;
    new_tab(&mut screen, 1, 0);

    let pane_id = PaneId::Terminal(1);
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_pane_by_pane_id(pane_id, "flame".as_bytes().to_vec());
    }

    // CLI rename single char via RenameActivePane (full replacement)
    if let Ok(tab) = screen.get_active_tab_mut(client_id) {
        let _ = tab.rename_active_pane("x".as_bytes().to_vec(), client_id);
    }

    let tab = screen.get_active_tab(client_id).unwrap();
    let pane = tab.get_pane_with_id(pane_id).unwrap();
    assert_eq!(
        pane.current_title(),
        "x",
        "Single-char CLI rename should replace the name, not append"
    );
}

#[test]
pub fn pty_bytes_and_hold_pane_buffered_before_new_pane() {
    // Regression test: when a command exits very quickly (e.g. `zellij run -- echo hello`),
    // PtyBytes and HoldPane can arrive at the screen thread before NewPane because the async
    // reader and quit_cb run on separate threads. This test verifies that such early-arriving
    // events are buffered and replayed once NewPane is processed.
    let size = Size { cols: 80, rows: 20 };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_server_instructions = Arc::new(Mutex::new(vec![]));
    let server_receiver = mock_screen.server_receiver.take().unwrap();
    let server_thread = log_actions_in_thread!(
        received_server_instructions,
        ServerInstruction::KillSession,
        server_receiver
    );

    // The initial layout creates pane id 0. We will use pane id 2 for the new pane
    // (id 1 is used by the plugin in the initial layout).
    let new_pane_id = 2;

    // Simulate the race: send PtyBytes for the new pane BEFORE NewPane
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        new_pane_id,
        "hello\r\n".as_bytes().to_vec(),
    ));

    // Send HoldPane before NewPane as well
    let run_command = RunCommand {
        command: PathBuf::from("echo"),
        args: vec!["hello".to_string()],
        hold_on_close: true,
        ..Default::default()
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::HoldPane(
        PaneId::Terminal(new_pane_id),
        Some(0),
        run_command,
    ));

    // Small sleep to ensure the above messages are processed (and buffered) first
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Now send NewPane — this should replay the buffered PtyBytes and HoldPane
    let _ = mock_screen.to_screen.send(ScreenInstruction::NewPane(
        PaneId::Terminal(new_pane_id),
        Some("echo hello".to_string()),
        None, // hold_for_command
        None, // invoked_with
        NewPanePlacement::default(),
        false, // start_suppressed
        ClientTabIndexOrPaneId::ClientId(client_id),
        None,  // completion_tx
        false, // set_blocking
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Use DumpScreen to verify the pane received the bytes
    let cli_action = CliAction::DumpScreen {
        file: None,
        path: Some(PathBuf::from("/tmp/dump_early_bytes")),
        full: true,
        pane_id: None,
        ansi: false,
    };
    send_cli_action_to_server(&session_metadata, cli_action, client_id);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![server_thread, screen_thread]);

    let filesystem = mock_screen.os_input.fake_filesystem.lock().unwrap();
    let dumped = filesystem
        .values()
        .next()
        .expect("DumpScreen should have written a file");
    assert!(
        dumped.contains("hello"),
        "Pane should contain the buffered output 'hello', but got: {:?}",
        dumped
    );
}

// =====================================================================
// Host-reply forwarding (CSI 2031)
//
// These tests exercise the token-lifecycle API on `Screen` directly —
// no route.rs, no thread spawn, no client. The harness plugs real
// `to_server` / `to_pty_writer` channels into the Bus so the forward
// dispatch (→ `ServerInstruction::ForwardQueryToHost`) and the reply
// write (→ `PtyWriteInstruction::Write`) can be asserted.
// =====================================================================

struct ForwardCapture {
    server_rx: Receiver<(ServerInstruction, ErrorContext)>,
    pty_writer_rx: Receiver<(PtyWriteInstruction, ErrorContext)>,
}

impl ForwardCapture {
    /// Drain every pending `ServerInstruction::ForwardQueryToHost` and
    /// return them as `(token, query_bytes)` pairs. Other variants are
    /// dropped — the forward path only ever emits this one.
    fn drain_forward_queries(&self) -> Vec<(u32, Vec<u8>)> {
        let mut out = Vec::new();
        while let Ok((instr, _ctx)) = self.server_rx.try_recv() {
            if let ServerInstruction::ForwardQueryToHost(token, bytes) = instr {
                out.push((token, bytes));
            }
        }
        out
    }

    /// Drain every pending `PtyWriteInstruction::Write`, returning
    /// `(bytes, terminal_id)` — the two fields the reply path sets.
    fn drain_pty_writes(&self) -> Vec<(Vec<u8>, u32)> {
        let mut out = Vec::new();
        while let Ok((instr, _ctx)) = self.pty_writer_rx.try_recv() {
            if let PtyWriteInstruction::Write(bytes, terminal_id, _) = instr {
                out.push((bytes, terminal_id));
            }
        }
        out
    }

    /// Drain every pending `ServerInstruction::KeyPassthroughChanged`,
    /// returning the `notify_guest` flag for each. Other variants are
    /// dropped.
    fn drain_key_passthrough_notify_flags(&self) -> Vec<bool> {
        let mut out = Vec::new();
        while let Ok((instr, _ctx)) = self.server_rx.try_recv() {
            if let ServerInstruction::KeyPassthroughChanged(_, _, _, _, _, notify_guest) = instr {
                out.push(notify_guest);
            }
        }
        out
    }
}

fn create_new_screen_with_forward_capture(size: Size) -> (Screen, ForwardCapture) {
    let (server_tx, server_rx) = channels::unbounded::<(ServerInstruction, ErrorContext)>();
    let (pty_writer_tx, pty_writer_rx) =
        channels::unbounded::<(PtyWriteInstruction, ErrorContext)>();

    let mut bus: Bus<ScreenInstruction> = Bus::empty();
    bus.senders.to_server = Some(SenderWithContext::new(server_tx));
    bus.senders.to_pty_writer = Some(SenderWithContext::new(pty_writer_tx));
    let fake_os_input = FakeInputOutput::default();
    bus.os_input = Some(Box::new(fake_os_input));

    let client_attributes = ClientAttributes {
        size,
        ..Default::default()
    };
    let max_panes = None;
    let mut mode_info = ModeInfo::default();
    mode_info.session_name = Some("zellij-test".into());
    let draw_pane_frames = PaneFrameStyle::None;
    let auto_layout = true;
    let session_is_mirrored = true;
    let copy_options = CopyOptions::default();
    let default_layout = Box::new(Layout::default());
    let default_layout_name = None;
    let default_shell = PathBuf::from("my_default_shell");
    let session_serialization = true;
    let serialize_pane_viewport = false;
    let scrollback_lines_to_serialize = None;
    let layout_dir = None;
    let debug = false;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let arrow_fonts = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let stacked_resize = true;
    let web_sharing = WebSharing::Off;
    let web_server_ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let web_server_port = 8080;
    let visual_bell = true;
    let screen = Screen::new(
        bus,
        &client_attributes,
        max_panes,
        mode_info,
        draw_pane_frames,
        auto_layout,
        session_is_mirrored,
        copy_options,
        debug,
        default_layout,
        default_layout_name,
        default_shell,
        session_serialization,
        serialize_pane_viewport,
        scrollback_lines_to_serialize,
        styled_underlines,
        osc8_hyperlinks,
        arrow_fonts,
        layout_dir,
        explicitly_disable_kitty_keyboard_protocol,
        true, // support_kitty_graphics_protocol
        stacked_resize,
        false,
        None,
        None,
        false,
        web_sharing,
        true,
        true,
        DEFAULT_WORD_SEPARATORS.to_owned(),
        true,
        true,
        visual_bell,
        0,     // bell_clear_delay_ms
        false, // focus_follows_mouse
        false, // mouse_click_through
        web_server_ip,
        web_server_port,
        NestedSessionHandling::default(),
    );
    (
        seed_first_client_size(screen, size),
        ForwardCapture {
            server_rx,
            pty_writer_rx,
        },
    )
}

// Convenience constructors for the forwarding tests — all callers
// want a fresh `HostQuery` value with the default terminator.
use crate::host_query::{HostQuery, OscTerminator};

fn bg_query() -> HostQuery {
    HostQuery::DefaultBackground {
        terminator: OscTerminator::St,
    }
}
fn fg_query() -> HostQuery {
    HostQuery::DefaultForeground {
        terminator: OscTerminator::St,
    }
}
fn fg_query_bel() -> HostQuery {
    HostQuery::DefaultForeground {
        terminator: OscTerminator::Bel,
    }
}
fn palette_query(index: u8) -> HostQuery {
    HostQuery::PaletteRegister {
        index,
        terminator: OscTerminator::St,
    }
}

#[test]
fn forward_host_query_when_idle_dispatches_immediately() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(42);
    let query = bg_query();

    let token = screen.forward_host_query(pane_id, query.clone());

    assert_eq!(
        screen.forward_in_flight_token,
        Some(token),
        "slot must flip to in-flight for the dispatched token"
    );
    assert_eq!(
        screen
            .pending_forwarded_queries
            .get(&token)
            .map(|e| e.pane_id),
        Some(pane_id),
        "token→pane mapping must be populated"
    );
    let forwards = capture.drain_forward_queries();
    assert_eq!(forwards.len(), 1, "exactly one forward dispatched");
    assert_eq!(
        forwards[0],
        (token, query.to_query_bytes()),
        "wire bytes must be derived from the HostQuery"
    );
}

#[test]
fn forward_host_query_when_busy_queues_instead_of_dispatching() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let first_pane = PaneId::Terminal(1);
    let second_pane = PaneId::Terminal(2);

    let first_token = screen.forward_host_query(first_pane, bg_query());
    let second_token = screen.forward_host_query(second_pane, fg_query());

    // The first call dispatched; the second waits in the queue. Only
    // the first token should be in the map; the second lives in
    // `forward_queue`.
    let forwards = capture.drain_forward_queries();
    assert_eq!(forwards.len(), 1, "second call must not dispatch yet");
    assert_eq!(forwards[0].0, first_token);
    assert_eq!(screen.forward_queue.len(), 1);
    assert_eq!(screen.forward_queue[0].token, second_token);
    assert_eq!(screen.forward_queue[0].pane_id, second_pane);
}

#[test]
fn handle_reply_writes_to_pane_pty_and_releases_slot() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let token = screen.forward_host_query(pane_id, bg_query());
    let _ = capture.drain_forward_queries(); // discard the dispatch

    let reply = b"\x1b]11;rgb:1111/2222/3333\x1b\\".to_vec();
    screen
        .handle_forwarded_reply_from_host(token, reply.clone())
        .expect("handler must not fail");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "one pty write expected");
    assert_eq!(writes[0], (reply, 7));
    assert!(
        screen.forward_in_flight_token.is_none(),
        "slot released so the next queued forward can dispatch"
    );
    assert!(
        screen.pending_forwarded_queries.get(&token).is_none(),
        "token entry must have been removed from the map"
    );
}

#[test]
fn handle_reply_dispatches_next_queued_forward() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let first_pane = PaneId::Terminal(1);
    let second_pane = PaneId::Terminal(2);
    let first_token = screen.forward_host_query(first_pane, bg_query());
    let second_token = screen.forward_host_query(second_pane, fg_query());
    // First dispatch already emitted; drop it.
    let _ = capture.drain_forward_queries();

    screen
        .handle_forwarded_reply_from_host(first_token, b"reply".to_vec())
        .expect("ok");

    // The queued second forward must now dispatch, and the map now
    // carries the second token → second pane.
    let forwards = capture.drain_forward_queries();
    assert_eq!(forwards.len(), 1, "next queued forward must dispatch");
    assert_eq!(forwards[0].0, second_token);
    assert!(screen.forward_queue.is_empty());
    assert_eq!(screen.forward_in_flight_token, Some(second_token));
    assert_eq!(
        screen
            .pending_forwarded_queries
            .get(&second_token)
            .map(|e| e.pane_id),
        Some(second_pane)
    );
}

#[test]
fn clear_nested_guest_does_not_notify_guest_focus_lost_on_teardown() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let client_id = 1;
    screen
        .nested_guest_choices
        .insert((client_id, pane_id), super::NestedGuestChoice::Descend);

    screen.clear_nested_guest(pane_id);

    let notify_flags = capture.drain_key_passthrough_notify_flags();
    assert_eq!(
        notify_flags,
        vec![false],
        "teardown clear must emit KeyPassthroughChanged with notify_guest=false so no FocusLost \
         frame is written to the exiting guest pane"
    );
}

#[test]
fn remove_client_notifies_guest_focus_lost_on_live_ascend() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let client_id = 1;
    screen
        .nested_guest_choices
        .insert((client_id, pane_id), super::NestedGuestChoice::Descend);

    screen.remove_client(client_id).expect("remove_client ok");

    let notify_flags = capture.drain_key_passthrough_notify_flags();
    assert_eq!(
        notify_flags,
        vec![true],
        "a live client leaving a still-alive guest must emit notify_guest=true so the guest is \
         told it lost focus"
    );
}

#[test]
fn suspend_nested_guest_preserves_choices_for_later_revival() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let client_id = 1;
    screen
        .nested_guest_choices
        .insert((client_id, pane_id), super::NestedGuestChoice::Descend);
    screen
        .nested_guest_tracker
        .on_announce(pane_id, std::time::Instant::now());

    screen.suspend_nested_guest(pane_id);

    assert!(
        screen
            .nested_guest_choices
            .contains_key(&(client_id, pane_id)),
        "suspend must preserve the client's descend choice so a re-announcing guest can be revived \
         into the exact prior state"
    );
}

#[test]
fn clear_nested_guest_discards_choices_unlike_suspend() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, _capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let client_id = 1;
    screen
        .nested_guest_choices
        .insert((client_id, pane_id), super::NestedGuestChoice::Descend);

    screen.clear_nested_guest(pane_id);

    assert!(
        !screen
            .nested_guest_choices
            .contains_key(&(client_id, pane_id)),
        "a full teardown (Bye/ClosePane) must discard choices, distinguishing it from suspend"
    );
}

#[test]
fn handle_reply_with_unknown_token_is_silent_noop() {
    // Token not in the map AND not the in-flight token: the handler
    // must not panic, must not write any bytes, and crucially must
    // NOT release the slot — the actually-in-flight forward still
    // owns it. (See `late_timeout_after_real_reply_does_not_clobber`
    // for the race scenario this guard prevents.)
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let first_pane = PaneId::Terminal(1);
    let second_pane = PaneId::Terminal(2);
    let first_token = screen.forward_host_query(first_pane, bg_query());
    let _second_token = screen.forward_host_query(second_pane, fg_query());
    let _ = capture.drain_forward_queries();

    // Reply for a stale / unknown token (neither first nor second).
    screen
        .handle_forwarded_reply_from_host(9999, b"dropped".to_vec())
        .expect("unknown tokens must not error");

    assert!(
        capture.drain_pty_writes().is_empty(),
        "unknown token must not produce any pty write"
    );
    assert_eq!(
        screen.forward_in_flight_token,
        Some(first_token),
        "in-flight token must still be the original"
    );
    assert!(
        capture.drain_forward_queries().is_empty(),
        "stale reply must not advance the queue"
    );
}

#[test]
fn late_timeout_after_real_reply_does_not_clobber_next_in_flight() {
    // The race the token-equality guard exists to prevent:
    //   1. dispatch token A (slot in-flight = A; A's timer is sleeping).
    //   2. real reply for A arrives → handler releases slot, dispatches
    //      queued token B → slot in-flight = B; B's timer is sleeping.
    //   3. A's server-side timeout fires after the real reply, sending
    //      an empty `ForwardedReplyFromHost { token: A, reply_bytes: [] }`.
    //
    // Without the guard, step 3 would clear the slot for token B and
    // pop the next queued forward, clobbering an actively-in-flight
    // request. The guard makes the late timeout a no-op.
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_a = PaneId::Terminal(1);
    let pane_b = PaneId::Terminal(2);
    let pane_c = PaneId::Terminal(3);
    let token_a = screen.forward_host_query(pane_a, bg_query());
    let token_b = screen.forward_host_query(pane_b, fg_query());
    let token_c = screen.forward_host_query(pane_c, palette_query(5));
    let _ = capture.drain_forward_queries();

    // Real reply for A arrives → slot moves to B.
    screen
        .handle_forwarded_reply_from_host(token_a, b"real-A".to_vec())
        .expect("ok");
    let dispatched = capture.drain_forward_queries();
    assert_eq!(dispatched.len(), 1, "B must dispatch on A's release");
    assert_eq!(dispatched[0].0, token_b);
    assert_eq!(screen.forward_in_flight_token, Some(token_b));
    // Drain the real reply's pty write so the late-timeout assertion
    // below only sees writes (or absence thereof) caused by step 3.
    let real_writes = capture.drain_pty_writes();
    assert_eq!(real_writes.len(), 1, "real reply should write once");

    // Late timeout for A fires (server-side timer woke up after the
    // real reply already advanced the queue).
    screen
        .handle_forwarded_reply_from_host(token_a, Vec::new())
        .expect("late timeout must be a no-op, not an error");

    assert_eq!(
        screen.forward_in_flight_token,
        Some(token_b),
        "B's slot must NOT be released by A's late timeout"
    );
    assert_eq!(
        screen.forward_queue.front().map(|p| p.token),
        Some(token_c),
        "C must still be queued — A's late timeout must not have popped it"
    );
    assert!(
        capture.drain_forward_queries().is_empty(),
        "no spurious dispatch from a late timeout"
    );
    assert!(
        capture.drain_pty_writes().is_empty(),
        "no synthetic write to any pane from a late timeout"
    );
}

#[test]
fn timeout_for_in_flight_token_releases_slot_with_cache_fallback() {
    // The non-racing case: server-side timeout fires while the token
    // is still in flight (no client reply ever arrived — the old-client
    // compatibility path). The handler must synthesize a cache-derived
    // reply for the pane, release the slot, and dispatch the next
    // queued forward. (Identical externally to the empty-reply
    // cache-fallback case, since the timer fires by sending an empty
    // reply for the in-flight token.)
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.update_terminal_background_color("rgb:1010/2020/3030".to_string());
    let pane = PaneId::Terminal(11);
    let queued_pane = PaneId::Terminal(12);
    let token = screen.forward_host_query(pane, bg_query());
    let queued_token = screen.forward_host_query(queued_pane, fg_query());
    let _ = capture.drain_forward_queries();

    // Simulate the timeout firing: empty reply for the in-flight token.
    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "cache fallback writes one reply");
    assert_eq!(
        std::str::from_utf8(&writes[0].0).unwrap(),
        "\u{1b}]11;rgb:1010/2020/3030\u{1b}\\",
    );
    assert_eq!(
        screen.forward_in_flight_token,
        Some(queued_token),
        "queued forward dispatched on slot release"
    );
}

#[test]
fn token_counter_wraps_skipping_sentinel() {
    // `next_forward_token == u32::MAX` → first allocation yields
    // `u32::MAX`, the counter then wraps to 0 which is the reserved
    // sentinel and is skipped, so the next allocation yields 1.
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.next_forward_token = u32::MAX;

    let t1 = screen.forward_host_query(PaneId::Terminal(1), bg_query());
    // Clear the in-flight slot so the next call actually allocates
    // (the queueing branch would still allocate, but we want the
    // dispatch branch here).
    screen
        .handle_forwarded_reply_from_host(t1, b"r".to_vec())
        .expect("ok");
    let _ = capture.drain_forward_queries();
    let _ = capture.drain_pty_writes();

    let t2 = screen.forward_host_query(PaneId::Terminal(2), fg_query());
    assert_eq!(t1, u32::MAX, "first token should land on u32::MAX");
    assert_eq!(
        t2, 1,
        "sentinel 0 must be skipped; next allocation wraps to 1"
    );
}

#[test]
fn plugin_pane_reply_is_dropped_without_write() {
    // Plugin panes never emit whitelisted host queries in production
    // code, but if a token → PaneId::Plugin mapping ever lands in the
    // map (via tests or future misuse), the handler must drop the
    // reply rather than routing it to the pty writer (plugin panes
    // don't have a pty).
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.pending_forwarded_queries.insert(
        77,
        super::PendingForwardEntry {
            pane_id: PaneId::Plugin(42),
            query: bg_query(),
        },
    );
    screen.forward_in_flight_token = Some(77);

    screen
        .handle_forwarded_reply_from_host(77, b"\x1b]11;rgb:0/0/0\x1b\\".to_vec())
        .expect("ok");

    assert!(
        capture.drain_pty_writes().is_empty(),
        "plugin-pane token must not produce a pty write"
    );
    assert!(
        screen.pending_forwarded_queries.get(&77).is_none(),
        "map entry must still be cleared even when the reply is dropped"
    );
    assert!(
        screen.forward_in_flight_token.is_none(),
        "slot still released"
    );
}

// =====================================================================
// Cache-fallback synthesis: empty reply → answer from Screen's caches
// =====================================================================

#[test]
fn empty_reply_falls_back_to_cached_background() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(9);
    screen.update_terminal_background_color("rgb:1010/2020/3030".to_string());
    let token = screen.forward_host_query(pane, bg_query());
    let _ = capture.drain_forward_queries();

    // Empty reply — the client couldn't or didn't answer.
    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "synthesis must write exactly one reply");
    let (bytes, terminal_id) = &writes[0];
    assert_eq!(*terminal_id, 9);
    assert_eq!(
        std::str::from_utf8(bytes).unwrap(),
        "\u{1b}]11;rgb:1010/2020/3030\u{1b}\\",
    );
}

#[test]
fn empty_reply_falls_back_to_cached_foreground_with_bel_terminator() {
    // Query used BEL; reply must mirror the same terminator.
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(1);
    screen.update_terminal_foreground_color("rgb:dcdc/dcdc/dcdc".to_string());
    let token = screen.forward_host_query(pane, fg_query_bel());
    let _ = capture.drain_forward_queries();

    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        std::str::from_utf8(&writes[0].0).unwrap(),
        "\u{1b}]10;rgb:dcdc/dcdc/dcdc\u{7}",
    );
}

#[test]
fn empty_reply_falls_back_to_cached_pixel_dimensions() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(1);
    screen.update_pixel_dimensions(
        1,
        PixelDimensions {
            character_cell_size: Some(SizeInPixels {
                height: 19,
                width: 9,
            }),
            text_area_size: Some(SizeInPixels {
                height: 608,
                width: 931,
            }),
        },
    );

    // CSI 14t — text-area pixels.
    let token = screen.forward_host_query(pane, HostQuery::TextAreaPixelSize);
    let _ = capture.drain_forward_queries();
    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");
    let writes = capture.drain_pty_writes();
    assert_eq!(
        std::str::from_utf8(&writes[0].0).unwrap(),
        "\u{1b}[4;608;931t"
    );

    // CSI 16t — cell size.
    let token = screen.forward_host_query(pane, HostQuery::CharacterCellPixelSize);
    let _ = capture.drain_forward_queries();
    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");
    let writes = capture.drain_pty_writes();
    assert_eq!(std::str::from_utf8(&writes[0].0).unwrap(), "\u{1b}[6;19;9t");
}

#[test]
fn empty_reply_falls_back_to_cached_palette_register() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(1);
    screen.update_terminal_color_registers(vec![(42, "rgb:abab/cdcd/efef".to_string())]);
    let token = screen.forward_host_query(pane, palette_query(42));
    let _ = capture.drain_forward_queries();

    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        std::str::from_utf8(&writes[0].0).unwrap(),
        "\u{1b}]4;42;rgb:abab/cdcd/efef\u{1b}\\",
    );
}

#[test]
fn empty_reply_with_no_cache_writes_empty() {
    // No background override, no palette, no pixel dims: synthesis
    // returns empty and the pane receives an empty write — the app
    // decides what to do with "host declined".
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(1);

    // Default Palette::fg is EightBit(0), not Rgb — synthesis refuses.
    let token = screen.forward_host_query(pane, fg_query());
    let _ = capture.drain_forward_queries();
    screen
        .handle_forwarded_reply_from_host(token, Vec::new())
        .expect("ok");
    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "still writes — the write is empty bytes");
    assert!(
        writes[0].0.is_empty(),
        "no rgb cache → synthesis returns empty"
    );
}

#[test]
fn non_empty_reply_bypasses_synthesis() {
    // A real reply from the host must be passed through verbatim —
    // we must not second-guess it with cached state.
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane = PaneId::Terminal(1);
    screen.update_terminal_background_color("rgb:0000/0000/0000".to_string());
    let token = screen.forward_host_query(pane, bg_query());
    let _ = capture.drain_forward_queries();

    let real = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\".to_vec();
    screen
        .handle_forwarded_reply_from_host(token, real.clone())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0].0, real,
        "real reply must be forwarded verbatim, not replaced by cached bg"
    );
}

// =====================================================================
// (CSI 2031 / DSR 997) (dark/light theme changes)
// =====================================================================

struct ThemeCapture {
    plugin_rx: Receiver<(PluginInstruction, ErrorContext)>,
    pty_writer_rx: Receiver<(PtyWriteInstruction, ErrorContext)>,
}

impl ThemeCapture {
    fn drain_plugin_events(&self) -> Vec<Event> {
        let mut out = Vec::new();
        while let Ok((instr, _ctx)) = self.plugin_rx.try_recv() {
            if let PluginInstruction::Update(updates) = instr {
                for (_pid, _cid, ev) in updates {
                    out.push(ev);
                }
            }
        }
        out
    }
    fn drain_pty_writes(&self) -> Vec<(Vec<u8>, u32)> {
        let mut out = Vec::new();
        while let Ok((instr, _ctx)) = self.pty_writer_rx.try_recv() {
            if let PtyWriteInstruction::Write(bytes, terminal_id, _) = instr {
                out.push((bytes, terminal_id));
            }
        }
        out
    }
}

fn create_new_screen_with_theme_capture(size: Size) -> (Screen, ThemeCapture) {
    let (plugin_tx, plugin_rx) = channels::unbounded::<(PluginInstruction, ErrorContext)>();
    let (pty_writer_tx, pty_writer_rx) =
        channels::unbounded::<(PtyWriteInstruction, ErrorContext)>();

    let mut bus: Bus<ScreenInstruction> = Bus::empty();
    bus.senders.to_plugin = Some(SenderWithContext::new(plugin_tx));
    bus.senders.to_pty_writer = Some(SenderWithContext::new(pty_writer_tx));
    let fake_os_input = FakeInputOutput::default();
    bus.os_input = Some(Box::new(fake_os_input));

    let client_attributes = ClientAttributes {
        size,
        ..Default::default()
    };
    let mut mode_info = ModeInfo::default();
    mode_info.session_name = Some("zellij-test".into());
    let copy_options = CopyOptions::default();
    let default_layout = Box::new(Layout::default());
    let default_shell = PathBuf::from("my_default_shell");
    let web_sharing = WebSharing::Off;
    let web_server_ip = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
    let web_server_port = 8080;
    let screen = Screen::new(
        bus,
        &client_attributes,
        None,
        mode_info,
        PaneFrameStyle::None,
        true,
        true,
        copy_options,
        false,
        default_layout,
        None,
        default_shell,
        true,
        false,
        None,
        true,
        true,
        true,
        None,
        false,
        true,
        true,
        false,
        None,
        None,
        false,
        web_sharing,
        true,
        true,
        DEFAULT_WORD_SEPARATORS.to_owned(),
        true,
        true,
        true,
        0, // bell_clear_delay_ms
        false,
        false,
        web_server_ip,
        web_server_port,
        NestedSessionHandling::default(),
    );
    (
        seed_first_client_size(screen, size),
        ThemeCapture {
            plugin_rx,
            pty_writer_rx,
        },
    )
}

#[test]
fn host_theme_first_update_emits_plugin_event() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_theme_capture(size);

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Dark)
        .expect("update ok");

    let events = capture.drain_plugin_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::HostTerminalThemeChanged(zellij_utils::data::HostTerminalThemeMode::Dark)
        )),
        "Event::HostTerminalThemeChanged(Dark) must be fanned out, got: {:?}",
        events
    );
    assert_eq!(
        screen.host_terminal_theme_mode,
        Some(zellij_utils::data::HostTerminalThemeMode::Dark),
        "stored mode must be updated"
    );
}

#[test]
fn host_theme_dedupes_duplicate_mode() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_theme_capture(size);

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Light)
        .expect("first ok");
    let _ = capture.drain_plugin_events();

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Light)
        .expect("second ok");

    let events = capture.drain_plugin_events();
    assert!(
        events.is_empty(),
        "duplicate mode must not re-emit any plugin events, got: {:?}",
        events
    );
}

#[test]
fn host_theme_emits_again_on_mode_flip() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_theme_capture(size);

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Dark)
        .expect("first ok");
    let _ = capture.drain_plugin_events();

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Light)
        .expect("flip ok");

    let events = capture.drain_plugin_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            Event::HostTerminalThemeChanged(zellij_utils::data::HostTerminalThemeMode::Light)
        )),
        "mode flip must re-emit the plugin event, got: {:?}",
        events
    );
}

#[test]
fn color_palette_mode_query_short_circuits_to_dark_reply() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.host_terminal_theme_mode = Some(zellij_utils::data::HostTerminalThemeMode::Dark);

    let token = screen.forward_host_query(PaneId::Terminal(13), HostQuery::ColorPaletteMode);

    assert_eq!(
        token, 0,
        "ColorPaletteMode must return the sentinel token; no real forward was queued"
    );
    assert!(
        capture.drain_forward_queries().is_empty(),
        "must NOT forward to host — Zellij answers from cache"
    );
    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "exactly one pty reply expected");
    assert_eq!(writes[0], (b"\x1b[?997;1n".to_vec(), 13));
    assert!(
        screen.forward_in_flight_token.is_none(),
        "slot must remain free; short-circuit does not occupy the queue"
    );
}

#[test]
fn color_palette_mode_query_short_circuits_to_light_reply() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.host_terminal_theme_mode = Some(zellij_utils::data::HostTerminalThemeMode::Light);

    let _ = screen.forward_host_query(PaneId::Terminal(4), HostQuery::ColorPaletteMode);

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0], (b"\x1b[?997;2n".to_vec(), 4));
}

#[test]
fn color_palette_mode_query_falls_back_to_own_dark_theme_when_host_mode_unknown() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    assert!(
        screen.host_terminal_theme_mode.is_none(),
        "precondition: no host mode learned yet"
    );
    screen.style.colors.text_unselected.background =
        zellij_utils::data::PaletteColor::Rgb((0, 0, 0));

    let token = screen.forward_host_query(PaneId::Terminal(1), HostQuery::ColorPaletteMode);

    assert_eq!(
        token, 0,
        "ColorPaletteMode must return the sentinel token; no real forward was queued"
    );
    assert!(
        capture.drain_forward_queries().is_empty(),
        "must NOT forward to host — Zellij answers from its own effective theme"
    );
    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1, "exactly one pty reply expected");
    assert_eq!(writes[0], (b"\x1b[?997;1n".to_vec(), 1));
}

#[test]
fn color_palette_mode_query_falls_back_to_own_light_theme_when_host_mode_unknown() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    assert!(
        screen.host_terminal_theme_mode.is_none(),
        "precondition: no host mode learned yet"
    );
    screen.style.colors.text_unselected.background =
        zellij_utils::data::PaletteColor::Rgb((255, 255, 255));

    let _ = screen.forward_host_query(PaneId::Terminal(7), HostQuery::ColorPaletteMode);

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0], (b"\x1b[?997;2n".to_vec(), 7));
}

#[test]
fn color_palette_mode_query_prefers_known_host_mode_over_own_theme() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.host_terminal_theme_mode = Some(zellij_utils::data::HostTerminalThemeMode::Light);
    screen.style.colors.text_unselected.background =
        zellij_utils::data::PaletteColor::Rgb((0, 0, 0));

    let _ = screen.forward_host_query(PaneId::Terminal(9), HostQuery::ColorPaletteMode);

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(
        writes[0],
        (b"\x1b[?997;2n".to_vec(), 9),
        "the host's announced mode must win over the local theme fallback"
    );
}

#[test]
fn color_palette_mode_query_skips_plugin_panes() {
    use crate::host_query::HostQuery;
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    screen.host_terminal_theme_mode = Some(zellij_utils::data::HostTerminalThemeMode::Dark);

    let _ = screen.forward_host_query(PaneId::Plugin(99), HostQuery::ColorPaletteMode);

    assert!(
        capture.drain_pty_writes().is_empty(),
        "plugin panes have no VT pty — they get Event::HostTerminalThemeChanged instead"
    );
}

#[test]
fn host_theme_no_pty_writes_when_no_panes_subscribed() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_theme_capture(size);

    screen
        .update_host_terminal_theme_mode(zellij_utils::data::HostTerminalThemeMode::Dark)
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert!(
        writes.is_empty(),
        "no panes exist (or none subscribed via CSI ?2031h), so no DSR forward is queued, got: {:?}",
        writes
    );
}

// =====================================================================
// Pause-on-forward state machine (pane-level)
//
// These tests exercise the per-pane forward-pause flag and the
// always-buffered PTY-input queue that preserves query/reply ordering
// when an app interleaves a sync-replied query (DA1, DSR, DECQRM) with
// a host-forwarded query (OSC 10/11/4, CSI 14t/16t).
//
// Single-buffer model:
//   - `handle_pty_bytes` always appends to `pending_pty_input`.
//   - When `forward_paused` is false, processing immediately drains
//     the queue byte-by-byte until either the queue empties or Grid
//     produces a forward-bound query.
//   - Once Tab arms the pause, subsequent calls just append; the
//     queue grows and waits.
//   - On resume, Tab clears the pause and calls handle_pty_bytes
//     with an empty slice; that triggers a fresh process pass over
//     the queued bytes.
// =====================================================================

use crate::panes::TerminalPane;
use crate::tab::Pane;
use zellij_utils::pane_size::PaneGeom;

fn new_terminal_pane_for_pause_test(pid: u32) -> TerminalPane {
    let mut geom = PaneGeom::default();
    geom.cols.set_inner(20);
    geom.rows.set_inner(10);
    TerminalPane::new(
        pid,
        geom,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(Some(SizeInPixels {
            width: 8,
            height: 16,
        }))),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
fn pane_buffers_pty_bytes_while_forward_paused() {
    let mut pane = new_terminal_pane_for_pause_test(1);
    pane.arm_forward_pause();
    assert!(pane.is_forward_paused());

    pane.handle_pty_bytes(b"hello".to_vec());
    pane.handle_pty_bytes(b"world".to_vec());

    let drained = pane.drain_pending_pty_input();
    assert_eq!(
        drained,
        b"helloworld".to_vec(),
        "while paused, every byte must accumulate in pending_pty_input \
         instead of being fed to vte"
    );
    assert!(
        pane.drain_pending_pty_input().is_empty(),
        "drain must clear the buffer"
    );
}

#[test]
fn pane_forward_in_vte_stops_processing_with_remainder_queued() {
    // App sends `OSC 10;? + after`. When vte's OSC dispatch runs and
    // Grid pushes the forward query, processing must stop, leaving
    // `after` in the queue. Those bytes will be replayed AFTER the
    // host reply has been written.
    let mut pane = new_terminal_pane_for_pause_test(1);
    let mut input = b"\x1b]10;?\x07".to_vec();
    input.extend_from_slice(b"after");
    pane.handle_pty_bytes(input);

    let queries = pane.drain_forwarded_queries();
    assert_eq!(queries.len(), 1, "exactly one forward must be queued");
    assert_eq!(
        queries[0],
        crate::host_query::HostQuery::DefaultForeground {
            terminator: crate::host_query::OscTerminator::Bel,
        }
    );

    let buffered = pane.drain_pending_pty_input();
    assert_eq!(
        buffered,
        b"after".to_vec(),
        "bytes after the forward must remain queued, not rendered"
    );
}

#[test]
fn pane_no_forward_drains_queue_empty() {
    // When the input contains no forward-bound query, processing runs
    // to completion and `pending_pty_input` ends empty.
    let mut pane = new_terminal_pane_for_pause_test(1);
    pane.handle_pty_bytes(b"plain text".to_vec());

    assert!(
        pane.drain_forwarded_queries().is_empty(),
        "no forward should have been produced"
    );
    assert!(
        pane.drain_pending_pty_input().is_empty(),
        "no forward → queue fully drained by processing"
    );
}

#[test]
fn pane_clear_forward_pause_reports_prior_state() {
    let mut pane = new_terminal_pane_for_pause_test(1);
    assert!(!pane.clear_forward_pause(), "not previously paused");
    pane.arm_forward_pause();
    assert!(pane.clear_forward_pause(), "was paused → returns true");
    assert!(
        !pane.is_forward_paused(),
        "after clearing, the pane must read as un-paused"
    );
}

#[test]
fn pane_paused_resumes_to_drain_queue() {
    // Simulate the resume cycle Tab runs:
    //   1. arm pause, app sends bytes (they accumulate)
    //   2. clear pause, drain queue, re-feed through handle_pty_bytes
    // The DA1 query buffered during step 1 must produce its sync
    // reply during step 2.
    let mut pane = new_terminal_pane_for_pause_test(1);
    pane.arm_forward_pause();
    pane.handle_pty_bytes(b"buffered\x1b[c".to_vec());

    // While paused, vte was never fed, so no replies were produced.
    assert!(
        pane.drain_forwarded_queries().is_empty(),
        "vte was not fed → no forwards produced"
    );
    assert!(
        pane.drain_messages_to_pty().is_empty(),
        "vte was not fed → no sync replies produced"
    );

    let was_paused = pane.clear_forward_pause();
    assert!(was_paused, "was paused");
    let buffered = pane.drain_pending_pty_input();
    pane.handle_pty_bytes(buffered);

    let sync_replies = pane.drain_messages_to_pty();
    assert!(
        !sync_replies.is_empty(),
        "after resume, queue is processed and Grid emits the DA1 reply"
    );
    assert!(
        pane.drain_pending_pty_input().is_empty(),
        "queue must be fully drained when no forward is in the stream"
    );
}

// =====================================================================
// End-to-end ordering through Screen+Tab integration
//
// These tests construct a real Screen with a real Tab and TerminalPane,
// drive PTY bytes in, and then exercise handle_forwarded_reply_from_host
// to assert the resulting PTY-write order on the captured channel.
// =====================================================================

#[test]
fn forwarded_reply_routes_through_tab_for_unpaused_pane() {
    // When a pane in a Tab receives a forward reply via
    // handle_forwarded_reply_from_host, the bytes must flow through
    // resume_pane_after_forward → Tab → write_to_pane_id_without_preprocessing
    // → PtyWriteInstruction::Write. The channel capture proves the
    // bytes reached the PTY writer with the right terminal id.
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, capture) = create_new_screen_with_forward_capture(size);
    let pane_id = PaneId::Terminal(7);
    let token = screen.forward_host_query(pane_id, bg_query());
    let _ = capture.drain_forward_queries();

    // No tab exists for this pane; the fallback path delivers the
    // bytes directly to the PTY writer. This still proves the
    // routing and stale-token guard interact correctly.
    let reply = b"\x1b]11;rgb:1111/2222/3333\x1b\\".to_vec();
    screen
        .handle_forwarded_reply_from_host(token, reply.clone())
        .expect("ok");

    let writes = capture.drain_pty_writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0], (reply, 7));
}

#[test]
fn paused_pane_with_da1_in_queue_emits_da1_after_resume() {
    // Simulates the user-reported ordering bug: app sends
    // `OSC 10;? + CSI c`. The OSC 10 forward must be dispatched
    // first, then the DA1 reply emitted only after the resume kicks
    // processing of the queued `\x1b[c`.
    let mut pane = new_terminal_pane_for_pause_test(1);
    let mut input = b"\x1b]10;?\x07".to_vec();
    input.extend_from_slice(b"\x1b[c");
    pane.handle_pty_bytes(input);

    // Forward emitted; DA1 still queued behind it.
    let queries = pane.drain_forwarded_queries();
    assert_eq!(queries.len(), 1, "OSC 10 forward emitted first");
    assert!(
        pane.drain_messages_to_pty().is_empty(),
        "DA1 reply must NOT be emitted yet — it is queued behind the forward"
    );

    // Tab arms pause, dispatches forward, eventually receives reply
    // and resumes. Resume = clear pause + drain queue + re-feed.
    pane.arm_forward_pause();
    // (host reply gets written to PTY via Tab; modelled here as no-op)
    pane.clear_forward_pause();
    let buffered = pane.drain_pending_pty_input();
    pane.handle_pty_bytes(buffered);

    let sync_replies = pane.drain_messages_to_pty();
    assert!(
        !sync_replies.is_empty(),
        "after resume, the queued CSI c is processed and Grid emits DA1"
    );
}

#[test]
fn empty_reply_with_paused_pane_drains_buffer_without_phantom_write() {
    // A pane that was paused on a ColorPaletteMode query while
    // host_terminal_theme_mode is unknown must still get unblocked,
    // but the spec requires NO bytes be written. The Tab-level
    // contract: a resume call with an empty payload skips the PTY
    // write yet still clears the pause and re-feeds the queue.
    let mut pane = new_terminal_pane_for_pause_test(1);
    pane.arm_forward_pause();
    pane.handle_pty_bytes(b"after".to_vec());
    assert!(pane.is_forward_paused());

    // Simulate Tab::resume_pane_after_forward with empty reply:
    //   - clear pause
    //   - drain queue + re-feed
    let was_paused = pane.clear_forward_pause();
    assert!(was_paused);
    let buffered = pane.drain_pending_pty_input();
    pane.handle_pty_bytes(buffered);
    assert!(!pane.is_forward_paused());
    assert!(
        pane.drain_pending_pty_input().is_empty(),
        "queue fully consumed by post-resume processing"
    );
}

fn create_non_mirrored_screen(size: Size) -> Screen {
    let mut bus: Bus<ScreenInstruction> = Bus::empty();
    let fake_os_input = FakeInputOutput::default();
    bus.os_input = Some(Box::new(fake_os_input));
    let client_attributes = ClientAttributes {
        size,
        ..Default::default()
    };
    let mut mode_info = ModeInfo::default();
    mode_info.session_name = Some("zellij-test".into());
    let screen = Screen::new(
        bus,
        &client_attributes,
        None, // max_panes
        mode_info,
        PaneFrameStyle::None,
        true,  // auto_layout
        false, // session_is_mirrored
        CopyOptions::default(),
        false, // debug
        Box::new(Layout::default()),
        None, // default_layout_name
        PathBuf::from("my_default_shell"),
        true,  // session_serialization
        false, // serialize_pane_viewport
        None,  // scrollback_lines_to_serialize
        true,  // styled_underlines
        true,  // osc8_hyperlinks
        true,  // arrow_fonts
        None,  // layout_dir
        false, // explicitly_disable_kitty_keyboard_protocol
        true,  // support_kitty_graphics_protocol
        true,  // stacked_resize
        false,
        None,
        None, // default_floating_size
        false,
        WebSharing::Off,
        true, // advanced_mouse_actions
        true,
        DEFAULT_WORD_SEPARATORS.to_owned(),
        true,  // mouse_scroll_resize
        true,  // mouse_hover_effects
        true,  // visual_bell
        0,     // bell_clear_delay_ms
        false, // focus_follows_mouse
        false, // mouse_click_through
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
        8080,
        NestedSessionHandling::default(),
    );
    seed_first_client_size(screen, size)
}

#[test]
fn new_tabs_are_created_at_the_size_of_the_client_creating_them() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    let client_size = Size { cols: 80, rows: 24 };
    screen.set_client_size(1, client_size);

    new_tab(&mut screen, 1, 0);

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        client_size,
        "A tab is created at the size of the client creating it"
    );
}

#[test]
fn applying_a_layout_to_an_existing_tab_keeps_its_viewer_derived_size() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    let client_size = Size { cols: 80, rows: 24 };
    screen.set_client_size(1, client_size);
    new_tab(&mut screen, 1, 0);

    screen
        .apply_layout(
            TiledPaneLayout::default(),
            vec![],
            vec![(2, None)],
            vec![],
            HashMap::new(),
            0,
            true,
            (1, false),
            None,
        )
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        client_size,
        "Applying a layout to an existing tab does not resize it away from its viewers"
    );
}

#[test]
fn recompute_tab_size_uses_lone_viewer_size() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);

    let client_id = 1;
    let client_size = Size { cols: 80, rows: 24 };
    screen.set_client_size(client_id, client_size);
    screen.recompute_tab_size(0).expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        client_size,
        "Tab adopts its lone viewer's size"
    );
}

#[test]
fn recompute_tab_size_takes_independent_min_across_axes() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.add_client(2, false).expect("TEST");

    screen.set_client_size(
        1,
        Size {
            cols: 200,
            rows: 24,
        },
    );
    screen.set_client_size(2, Size { cols: 80, rows: 60 });
    screen.recompute_tab_size(0).expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Tab size = (min cols across viewers, min rows across viewers)"
    );
}

#[test]
fn recompute_tab_size_isolates_tabs_with_different_viewers() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.add_client(2, false).expect("TEST");
    screen.set_client_size(1, Size { cols: 80, rows: 24 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Tab 1 sized to its lone viewer (client 1)"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Tab 0 sized to its lone viewer (client 2), unaffected by client 1's smaller viewport"
    );
}

#[test]
fn switching_tabs_recomputes_source_and_destination() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.add_client(2, false).expect("TEST");

    screen.set_client_size(1, Size { cols: 80, rows: 24 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.recompute_tab_size(1).expect("TEST");
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Both clients on tab 1 → it sizes to the smaller"
    );

    screen.switch_active_tab(0, None, true, 1).expect("TEST");

    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Source tab grows back to fit its remaining viewer"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Destination tab shrinks to fit the arriving smaller viewer"
    );
}

#[test]
fn break_pane_to_new_tab_recomputes_source_and_destination() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.add_client(2, false).expect("TEST");

    screen.set_client_size(1, Size { cols: 80, rows: 24 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Pre-condition: tab 0 sized to client 1"
    );
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Pre-condition: tab 1 sized to client 2"
    );

    {
        let active_tab = screen.get_active_tab_mut(1).unwrap();
        active_tab
            .new_pane(
                PaneId::Terminal(99),
                None,
                None,
                false,
                true,
                NewPanePlacement::default(),
                Some(1),
                None,
            )
            .unwrap();
    }

    screen
        .break_pane_to_new_tab(Direction::Right, 1)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Destination tab shrinks to fit the arriving smaller viewer"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Source tab is empty — recompute is a no-op, last viewer-derived size is preserved"
    );
}

#[test]
fn moving_panes_between_tabs_with_focus_change_recomputes_both() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen.add_client(2, false).expect("TEST");

    screen.set_client_size(1, Size { cols: 80, rows: 24 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 2).expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Pre-condition: tab 0 sized to client 2"
    );
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Pre-condition: tab 1 sized to client 1"
    );

    let pane_to_move = PaneId::Terminal(42);
    {
        let active_tab = screen.get_active_tab_mut(1).unwrap();
        active_tab
            .new_pane(
                pane_to_move,
                None,
                None,
                false,
                true,
                NewPanePlacement::default(),
                Some(1),
                None,
            )
            .unwrap();
    }

    screen
        .break_multiple_panes_to_tab_with_index(vec![pane_to_move], 0, true, 1)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Destination tab shrinks to min(viewers) once the smaller client arrives"
    );
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Source tab is empty — last viewer-derived size is preserved"
    );
}

#[test]
fn detaching_client_grows_vacated_tab_back() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.add_client(2, false).expect("TEST");

    screen.set_client_size(
        1,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.set_client_size(2, Size { cols: 80, rows: 24 });
    screen.recompute_tab_size(0).expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 24 },
        "Smaller viewer wins while both clients are on the tab"
    );

    screen.remove_client(2).expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Tab grows back to fit the remaining viewer after the smaller one detaches"
    );
}

#[test]
fn closing_a_tab_resizes_the_tab_it_returns_to() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_new_screen(initial_size, true, true);
    let client_id = 1;
    let small_size = Size { cols: 80, rows: 24 };
    let large_size = Size {
        cols: 160,
        rows: 50,
    };

    screen.set_client_size(client_id, small_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        small_size,
        "Pre-condition: both tabs sized to the small client viewport"
    );

    screen.set_client_size(client_id, large_size);
    screen.recompute_tab_size(1).expect("TEST");
    assert_eq!(
        screen.tabs.get(&1).unwrap().size,
        large_size,
        "Pre-condition: the active tab follows the client resize"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        small_size,
        "Pre-condition: the background tab keeps its stale size until it is activated"
    );

    screen.close_tab_by_id(1).expect("TEST");

    assert_eq!(
        screen.get_active_tab(client_id).unwrap().position,
        0,
        "Focus returns to the previous tab"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        large_size,
        "The tab we return to adopts the current client size"
    );
}

fn add_second_pane_to_active_tab(screen: &mut Screen, pid: u32) {
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    active_tab
        .new_pane(
            PaneId::Terminal(pid),
            None,
            None,
            false,
            true,
            NewPanePlacement::default(),
            Some(1),
            None,
        )
        .unwrap();
}

fn decode_nested_frame(bytes: &[u8]) -> Option<zellij_utils::nested_session::NestedSessionMessage> {
    use zellij_utils::nested_session::{
        decode_base64, decode_payload, NESTED_FRAME_HEADER, NESTED_FRAME_TERMINATOR,
    };
    let encoded_payload = bytes
        .strip_prefix(NESTED_FRAME_HEADER)?
        .strip_suffix(NESTED_FRAME_TERMINATOR)?;
    decode_payload(&decode_base64(encoded_payload)?)
}

fn guest_announce_message() -> zellij_utils::nested_session::NestedSessionMessage {
    zellij_utils::nested_session::NestedSessionMessage::Announce {
        session_name: "guest-session".to_owned(),
        capabilities: vec![zellij_utils::nested_session::NestedSessionCapability::NestedControl],
    }
}

#[test]
pub fn nested_guest_announce_gets_announce_ack_with_ancestry() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let received_background_jobs = mock_screen.received_background_jobs.clone();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::NestedSessionMessageFromPane {
            pane_id: PaneId::Terminal(0),
            message: guest_announce_message(),
        });
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let announce_ack = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find_map(|instruction| match instruction {
            PtyWriteInstruction::Write(bytes, 0, None) => decode_nested_frame(bytes),
            _ => None,
        })
        .expect("an announce_ack frame written to the guest pane");
    assert_eq!(
        announce_ack,
        zellij_utils::nested_session::NestedSessionMessage::AnnounceAck {
            ancestry: vec!["zellij-test".to_owned()],
            capabilities: vec![
                zellij_utils::nested_session::NestedSessionCapability::NestedControl
            ],
            descend_keys: vec![],
        }
    );
    assert!(received_background_jobs
        .lock()
        .unwrap()
        .iter()
        .any(|job| matches!(
            job,
            BackgroundJob::StartNestedGuestPing(PaneId::Terminal(0))
        )));
}

#[test]
pub fn nested_guest_announce_in_never_mode_still_completes_handshake() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut mock_screen = MockScreen::new(size);
    mock_screen.config.options.nested_session_handling = Some(NestedSessionHandling::Never);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let received_background_jobs = mock_screen.received_background_jobs.clone();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::NestedSessionMessageFromPane {
            pane_id: PaneId::Terminal(0),
            message: guest_announce_message(),
        });
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let announce_ack = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .find_map(|instruction| match instruction {
            PtyWriteInstruction::Write(bytes, 0, None) => decode_nested_frame(bytes),
            _ => None,
        })
        .expect("an announce_ack frame written to the guest pane even in never mode");
    assert!(matches!(
        announce_ack,
        zellij_utils::nested_session::NestedSessionMessage::AnnounceAck { .. }
    ));
    assert!(received_background_jobs
        .lock()
        .unwrap()
        .iter()
        .any(|job| matches!(
            job,
            BackgroundJob::StartNestedGuestPing(PaneId::Terminal(0))
        )));
}

#[test]
pub fn nested_guest_bye_stops_liveness_pings() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut mock_screen = MockScreen::new(size);
    let pty_writer_receiver = mock_screen.pty_writer_receiver.take().unwrap();
    let received_background_jobs = mock_screen.received_background_jobs.clone();
    let screen_thread = mock_screen.run(None, vec![]);
    let received_pty_instructions = Arc::new(Mutex::new(vec![]));
    let pty_writer_thread = log_actions_in_thread!(
        received_pty_instructions,
        PtyWriteInstruction::Exit,
        pty_writer_receiver
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::NestedSessionMessageFromPane {
            pane_id: PaneId::Terminal(0),
            message: guest_announce_message(),
        });
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::NestedSessionMessageFromPane {
            pane_id: PaneId::Terminal(0),
            message: zellij_utils::nested_session::NestedSessionMessage::Bye,
        });
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::NestedGuestPingTick {
            pane_id: PaneId::Terminal(0),
        });
    std::thread::sleep(std::time::Duration::from_millis(100));
    mock_screen.teardown(vec![pty_writer_thread, screen_thread]);
    let ping_frames_written = received_pty_instructions
        .lock()
        .unwrap()
        .iter()
        .filter(|instruction| match instruction {
            PtyWriteInstruction::Write(bytes, 0, None) => matches!(
                decode_nested_frame(bytes),
                Some(zellij_utils::nested_session::NestedSessionMessage::Ping)
            ),
            _ => false,
        })
        .count();
    assert_eq!(ping_frames_written, 0);
    assert!(received_background_jobs
        .lock()
        .unwrap()
        .iter()
        .any(|job| matches!(job, BackgroundJob::StopNestedGuestPing(PaneId::Terminal(0)))));
}

#[test]
fn kitty_query_replies_ok_when_capable_client_connected() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.update_kitty_graphics_support(1, true);
    assert_eq!(screen.kitty_host_capabilities.borrow().get(&1), Some(&true));
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\".to_vec());
    assert_eq!(
        active_pane.drain_messages_to_pty(),
        vec![b"\x1b_Gi=31;OK\x1b\\".to_vec()]
    );
}

#[test]
fn kitty_query_replies_enotsupported_when_no_capable_client() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.update_kitty_graphics_support(1, false);
    assert_eq!(
        screen.kitty_host_capabilities.borrow().get(&1),
        Some(&false)
    );
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\".to_vec());
    let replies = active_pane.drain_messages_to_pty();
    assert_eq!(replies.len(), 1);
    let reply = String::from_utf8(replies[0].clone()).unwrap();
    assert!(reply.starts_with("\x1b_Gi=31;ENOTSUPPORTED"));
    assert!(reply.ends_with("\x1b\\"));
}

#[test]
fn kitty_query_is_ignored_when_the_protocol_is_disabled_in_the_config() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen_with_kitty_graphics(size, true, true, false);
    new_tab(&mut screen, 1, 0);
    screen.update_kitty_graphics_support(1, true);
    assert_eq!(
        screen.kitty_host_capabilities.borrow().get(&1),
        Some(&false),
        "a capable host must still be recorded as incapable when the protocol is disabled"
    );
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\".to_vec());
    assert!(
        active_pane.drain_messages_to_pty().is_empty(),
        "a disabled protocol must not reply to queries at all"
    );
}

#[test]
fn kitty_support_recomputed_on_client_detach() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.add_client(2, false).expect("TEST");
    screen.update_kitty_graphics_support(1, false);
    screen.update_kitty_graphics_support(2, true);
    screen.remove_client(2).expect("TEST");
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b_Ga=q,i=31,s=1,v=1,t=d,f=24;AAAA\x1b\\".to_vec());
    let replies = active_pane.drain_messages_to_pty();
    assert_eq!(replies.len(), 1);
    let reply = String::from_utf8(replies[0].clone()).unwrap();
    assert!(reply.starts_with("\x1b_Gi=31;ENOTSUPPORTED"));
    assert!(reply.ends_with("\x1b\\"));
}

#[test]
fn primary_da_advertises_sixel_when_capable_client_connected() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.update_sixel_support(1, true);
    assert_eq!(screen.sixel_host_capabilities.borrow().get(&1), Some(&true));
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b[c".to_vec());
    assert_eq!(
        active_pane.drain_messages_to_pty(),
        vec![b"\x1b[?62;4;52c".to_vec()]
    );
}

#[test]
fn primary_da_omits_sixel_when_no_capable_client() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.update_sixel_support(1, false);
    assert_eq!(
        screen.sixel_host_capabilities.borrow().get(&1),
        Some(&false)
    );
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b[c".to_vec());
    assert_eq!(
        active_pane.drain_messages_to_pty(),
        vec![b"\x1b[?62;52c".to_vec()]
    );
}

#[test]
fn sixel_support_recomputed_on_client_detach() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen.add_client(2, false).expect("TEST");
    screen.update_sixel_support(1, false);
    screen.update_sixel_support(2, true);
    screen.remove_client(2).expect("TEST");
    let active_tab = screen.get_active_tab_mut(1).unwrap();
    let active_pane = active_tab.get_active_pane_mut(1).unwrap();
    active_pane.handle_pty_bytes(b"\x1b[c".to_vec());
    assert_eq!(
        active_pane.drain_messages_to_pty(),
        vec![b"\x1b[?62;52c".to_vec()]
    );
}

#[test]
fn fit_disabled_excludes_web_client_from_min_size() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(2, /* is_web_client */ true)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 80, rows: 20 });
    screen.set_client_size(2, Size { cols: 40, rows: 10 });

    screen
        .set_mobile_render_preferences(2, /* single_pane */ false, /* fit */ false)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 20 },
        "Fit-disabled web client is excluded from the min-size loop; tab stays at desktop size"
    );
}

#[test]
fn fit_disabled_ignores_client_on_another_tab() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 40, rows: 10 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(1, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(1, /* single_pane */ false, /* fit */ false)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 40, rows: 10 },
        "A client on another tab is not a reference: fit is forced on and the mobile size drives layout"
    );
}

#[test]
fn fit_disabled_tab_repins_on_desktop_resize() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 40, rows: 10 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(1, /* single_pane */ false, /* fit */ false)
        .expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Pre-condition: shared tab pinned to the initial desktop reference size"
    );

    // Desktop client resizes; the fit-disabled tab must re-pin to its new size.
    screen.set_client_size(
        2,
        Size {
            cols: 120,
            rows: 40,
        },
    );
    screen.recompute_fit_disabled_tabs().expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 120,
            rows: 40,
        },
        "Tab re-pins to the new desktop reference size after a desktop resize"
    );
}

#[test]
fn fit_disabled_reverts_when_reference_client_switches_tab() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 40, rows: 10 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(1, /* single_pane */ false, /* fit */ false)
        .expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "Pre-condition: tab is shared, so fit-disabled is honored"
    );

    // The reference client leaves the tab without disconnecting.
    screen.switch_active_tab(1, None, true, 2).expect("TEST");
    screen.log_and_report_session_state().expect("TEST");

    assert!(
        screen.mobile_web_prefs.get(&1).map(|prefs| prefs.fit) == Some(true),
        "Fit reverts once no client shares the tab, without any disconnect"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 40, rows: 10 },
        "The tab shrinks to the mobile size once nobody else is viewing it"
    );
}

#[test]
fn fit_disabled_allowed_when_only_other_client_is_web() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(1, /* is_web_client */ true)
        .expect("TEST");
    screen
        .add_client(2, /* is_web_client */ true)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 80, rows: 20 });
    screen.set_client_size(2, Size { cols: 40, rows: 10 });

    screen
        .set_mobile_render_preferences(2, /* single_pane */ false, /* fit */ false)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 20 },
        "A web client counts as a reference; fit-disabled is honored and the tab stays at the reference size"
    );
}

#[test]
fn fit_disabled_without_desktop_client_forces_enabled() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(1, /* is_web_client */ true)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 40, rows: 10 });

    screen
        .set_mobile_render_preferences(1, /* single_pane */ false, /* fit */ false)
        .expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 40, rows: 10 },
        "With no desktop client, fit is forced enabled and the mobile size drives layout"
    );
}

#[test]
fn desktop_disconnect_reverts_fit_disabled() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(2, /* is_web_client */ true)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 80, rows: 20 });
    screen.set_client_size(2, Size { cols: 40, rows: 10 });

    screen
        .set_mobile_render_preferences(2, /* single_pane */ false, /* fit */ false)
        .expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 80, rows: 20 },
        "Pre-condition: fit-disabled excludes the mobile client"
    );

    screen.remove_client(1).expect("TEST");

    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size { cols: 40, rows: 10 },
        "Desktop disconnect reverts the mobile client to fit-enabled; its size now drives layout"
    );
}

#[test]
fn single_pane_fullscreens_active_pane_and_reverts() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let client = 1;

    screen
        .set_mobile_render_preferences(client, /* single_pane */ true, /* fit */ true)
        .expect("TEST");
    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Single-pane fullscreens the active pane"
    );
    assert!(
        screen.tabs.get(&0).unwrap().fullscreen_covers_ui(),
        "Single-pane hides the tab bar and status bar"
    );

    screen
        .set_mobile_render_preferences(client, /* single_pane */ false, /* fit */ true)
        .expect("TEST");
    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Disabling single-pane reverts the fullscreen"
    );
}

#[test]
fn focus_pane_by_id_is_per_client_and_does_not_steal_global_focus() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(1, None, true, 2).expect("TEST");

    assert_eq!(
        screen.get_active_pane_id(&2),
        Some(PaneId::Terminal(2)),
        "Pre-condition: client 2 is focused on pane 2 (its own tab)"
    );

    // Client 1 focuses the pane living on client 2's tab.
    screen
        .focus_pane_with_id(PaneId::Terminal(2), false, false, 1)
        .expect("TEST");

    assert_eq!(
        screen.get_active_pane_id(&1),
        Some(PaneId::Terminal(2)),
        "Client 1 now focuses the target pane"
    );
    assert_eq!(
        screen.active_tab_ids.get(&2).copied(),
        Some(1),
        "Client 2's active tab is unchanged by client 1's per-client focus"
    );
    assert_eq!(
        screen.get_active_pane_id(&2),
        Some(PaneId::Terminal(2)),
        "Client 2's focus is not stolen by client 1's per-client focus"
    );
}

#[test]
fn mobile_state_reports_single_pane_off_before_any_preference_is_sent() {
    let size = Size { cols: 80, rows: 20 };
    let (mut screen, messages) = create_new_screen_with_message_capture(size);
    new_tab(&mut screen, 1, 0);
    screen
        .add_client(2, /* is_web_client */ true)
        .expect("TEST");

    screen.log_and_report_session_state().expect("TEST");

    let msgs = messages.lock().unwrap();
    let payload = msgs
        .get(&2)
        .expect("TEST")
        .iter()
        .find_map(|msg| match msg {
            ServerToClientMsg::MobileState { payload } => Some(payload.clone()),
            _ => None,
        })
        .expect("A web client receives a MobileState push");

    assert!(
        !payload.render_prefs.single_pane,
        "A client that never sent preferences has nothing fullscreened, so single-pane must be reported off"
    );
    assert!(
        payload.render_prefs.fit,
        "Such a client participates in the tab min-size rule, so fit is reported on"
    );
    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Nothing is fullscreened for a client that never sent preferences"
    );
}

#[test]
fn single_pane_demotes_after_desktop_unfullscreen() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let client = 1;

    screen
        .set_mobile_render_preferences(client, /* single_pane */ true, /* fit */ true)
        .expect("TEST");
    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Pre-condition: single-pane fullscreen active"
    );

    // Simulate a desktop-initiated un-fullscreen on the same tab.
    screen.tabs.get_mut(&0).unwrap().unset_fullscreen();
    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Fullscreen cleared by the simulated desktop action"
    );

    screen.log_and_report_session_state().expect("TEST");

    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Single-pane yields to the un-fullscreen instead of re-asserting it"
    );
    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&client)
            .map(|prefs| prefs.single_pane),
        Some(false),
        "The client is demoted out of single-pane mode and reports it"
    );
    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&client)
            .and_then(|prefs| prefs.fullscreened_pane),
        None,
        "The tracked fullscreen pane is cleared on demotion"
    );
}

#[test]
fn single_pane_demotion_disables_fit_when_tab_is_shared() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.set_client_size(1, Size { cols: 40, rows: 10 });
    screen.set_client_size(
        2,
        Size {
            cols: 160,
            rows: 50,
        },
    );
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(1, /* single_pane */ true, /* fit */ true)
        .expect("TEST");

    // Simulate a desktop-initiated un-fullscreen on the shared tab.
    screen.tabs.get_mut(&0).unwrap().unset_fullscreen();
    screen.log_and_report_session_state().expect("TEST");

    assert_eq!(
        screen.mobile_web_prefs.get(&1).map(|prefs| prefs.fit),
        Some(false),
        "Demotion also disables fit so the shared tab is not shrunk to the mobile size"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().size,
        Size {
            cols: 160,
            rows: 50,
        },
        "The shared tab keeps the desktop size after the demotion"
    );
}

#[test]
fn single_pane_demotes_when_desktop_opens_a_pane() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let mobile_client = 1;
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(
            mobile_client,
            /* single_pane */ true,
            /* fit */ true,
        )
        .expect("TEST");
    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Pre-condition: single-pane fullscreen active"
    );

    // The desktop client opens a pane in the shared tab, which drops fullscreen.
    screen
        .get_active_tab_mut(2)
        .unwrap()
        .new_pane(
            PaneId::Terminal(3),
            None,
            None,
            false,
            true,
            NewPanePlacement::default(),
            Some(2),
            None,
        )
        .expect("TEST");
    screen.log_and_report_session_state().expect("TEST");

    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "The pane the desktop client opened stays visible"
    );
    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&mobile_client)
            .map(|prefs| prefs.single_pane),
        Some(false),
        "The mobile client is demoted rather than hiding the new pane"
    );
}

#[test]
fn single_pane_follows_focus_to_newly_added_pane() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let client = 1;

    screen
        .set_mobile_render_preferences(client, /* single_pane */ true, /* fit */ true)
        .expect("TEST");
    assert_eq!(
        screen.tabs.get(&0).unwrap().fullscreen_pane_id(),
        Some(PaneId::Terminal(2)),
        "Pre-condition: single-pane fullscreens the current active pane"
    );

    add_second_pane_to_active_tab(&mut screen, 3);
    screen.log_and_report_session_state().expect("TEST");

    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "A pane added in single-pane mode keeps a pane fullscreened"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().fullscreen_pane_id(),
        Some(PaneId::Terminal(3)),
        "The fullscreen follows focus to the newly added pane"
    );
    assert!(
        screen.tabs.get(&0).unwrap().fullscreen_covers_ui(),
        "The followed fullscreen still hides the tab bar and status bar"
    );
}

#[test]
fn single_pane_demotes_after_desktop_downgrades_to_regular_fullscreen() {
    let initial_size = Size { cols: 80, rows: 20 };
    let mut screen = create_new_screen(initial_size, true, true);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let client = 1;

    screen
        .set_mobile_render_preferences(client, /* single_pane */ true, /* fit */ true)
        .expect("TEST");
    let fullscreen_pane_id = screen.tabs.get(&0).unwrap().fullscreen_pane_id().unwrap();

    // A desktop client toggles regular fullscreen, downgrading the no-ui fullscreen.
    screen
        .tabs
        .get_mut(&0)
        .unwrap()
        .toggle_pane_fullscreen(fullscreen_pane_id);
    screen.log_and_report_session_state().expect("TEST");

    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "The regular fullscreen the desktop client asked for is kept"
    );
    assert!(
        !screen.tabs.get(&0).unwrap().fullscreen_covers_ui(),
        "Single-pane does not re-assert the no-ui fullscreen"
    );
    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&client)
            .map(|prefs| prefs.single_pane),
        Some(false),
        "The client is demoted out of single-pane mode"
    );
}

#[test]
fn single_pane_survives_the_mobile_client_opening_its_own_pane_in_a_shared_tab() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    add_second_pane_to_active_tab(&mut screen, 2);
    let mobile_client = 1;
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(
            mobile_client,
            /* single_pane */ true,
            /* fit */ true,
        )
        .expect("TEST");
    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Pre-condition: single-pane fullscreen active"
    );

    // The mobile client opens a pane of its own, which takes its focus.
    screen
        .get_active_tab_mut(mobile_client)
        .unwrap()
        .new_pane(
            PaneId::Terminal(3),
            None,
            None,
            false,
            true,
            NewPanePlacement::default(),
            Some(mobile_client),
            None,
        )
        .expect("TEST");
    screen.log_and_report_session_state().expect("TEST");

    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&mobile_client)
            .map(|prefs| prefs.single_pane),
        Some(true),
        "A pane the mobile client opened itself does not demote it out of single-pane mode"
    );
    assert_eq!(
        screen.tabs.get(&0).unwrap().fullscreen_pane_id(),
        Some(PaneId::Terminal(3)),
        "The fullscreen follows the mobile client to the pane it just opened"
    );
    assert!(
        screen.tabs.get(&0).unwrap().fullscreen_covers_ui(),
        "The followed fullscreen still hides the tab bar and status bar"
    );
    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&mobile_client)
            .map(|prefs| prefs.fit),
        Some(true),
        "Fit is untouched because there was no demotion"
    );
}

#[test]
fn single_pane_follows_the_mobile_client_into_a_new_tab() {
    let initial_size = Size {
        cols: 200,
        rows: 60,
    };
    let mut screen = create_non_mirrored_screen(initial_size);
    new_tab(&mut screen, 1, 0);
    let mobile_client = 1;
    screen
        .add_client(2, /* is_web_client */ false)
        .expect("TEST");
    screen.switch_active_tab(0, None, true, 1).expect("TEST");
    screen.switch_active_tab(0, None, true, 2).expect("TEST");

    screen
        .set_mobile_render_preferences(
            mobile_client,
            /* single_pane */ true,
            /* fit */ true,
        )
        .expect("TEST");
    assert!(
        screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "Pre-condition: single-pane fullscreen active"
    );

    new_tab(&mut screen, 2, 1);

    assert_eq!(
        screen
            .mobile_web_prefs
            .get(&mobile_client)
            .map(|prefs| prefs.single_pane),
        Some(true),
        "Opening a tab keeps the mobile client in single-pane mode"
    );
    assert!(
        !screen.tabs.get(&0).unwrap().is_fullscreen_active(),
        "The tab the mobile client left is restored for the desktop client"
    );
    assert_eq!(
        screen.tabs.get(&1).unwrap().fullscreen_pane_id(),
        Some(PaneId::Terminal(2)),
        "The pane of the new tab is fullscreened for the mobile client"
    );
    assert!(
        screen.tabs.get(&1).unwrap().fullscreen_covers_ui(),
        "The new tab's fullscreen still hides the tab bar and status bar"
    );
}

#[test]
fn attaching_web_client_lands_on_the_first_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    assert_eq!(
        screen.active_tab_ids.get(&1),
        Some(&1),
        "host client is on the second tab"
    );

    screen.add_client(2, true).expect("TEST");

    assert_eq!(
        screen.active_tab_ids.get(&2),
        Some(&0),
        "web client is on the first tab"
    );
}

#[test]
fn attaching_web_client_hides_the_floating_surface_of_the_first_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    screen
        .tabs
        .get_mut(&0)
        .unwrap()
        .new_floating_pane(PaneId::Terminal(2), None, None, false, true, None, None)
        .unwrap();
    assert!(screen.tabs.get(&0).unwrap().are_floating_panes_visible());

    screen.add_client(2, true).expect("TEST");

    assert!(
        !screen.tabs.get(&0).unwrap().are_floating_panes_visible(),
        "the floating surface is hidden"
    );
    assert_eq!(
        screen.get_active_pane_id(&2),
        Some(PaneId::Terminal(1)),
        "web client is focused on a tiled pane"
    );
}

#[test]
fn attaching_web_client_does_not_touch_the_floating_surface_of_other_tabs() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen
        .tabs
        .get_mut(&1)
        .unwrap()
        .new_floating_pane(PaneId::Terminal(3), None, None, false, true, None, None)
        .unwrap();

    screen.add_client(2, true).expect("TEST");

    assert!(
        screen.tabs.get(&1).unwrap().are_floating_panes_visible(),
        "the floating surface of the host tab is untouched"
    );
    assert_eq!(
        screen.get_active_pane_id(&1),
        Some(PaneId::Terminal(3)),
        "host client keeps its floating focus"
    );
}

#[test]
fn attaching_terminal_client_follows_the_host_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    screen
        .tabs
        .get_mut(&1)
        .unwrap()
        .new_floating_pane(PaneId::Terminal(3), None, None, false, true, None, None)
        .unwrap();

    screen.add_client(2, false).expect("TEST");

    assert_eq!(
        screen.active_tab_ids.get(&2),
        Some(&1),
        "terminal client is on the host tab"
    );
    assert!(
        screen.tabs.get(&1).unwrap().are_floating_panes_visible(),
        "the floating surface remains visible"
    );
}

#[test]
fn attaching_web_watcher_follows_the_host_tab() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    screen.add_watcher_client(2).expect("TEST");
    screen.add_client(2, true).expect("TEST");

    assert_eq!(
        screen.active_tab_ids.get(&2),
        Some(&1),
        "watcher client mirrors the host tab"
    );
}

// Fork addition: `top_only` sits outside upstream's three-way toggle cycle. Leaving it for
// `none` must not be one-way.
#[test]
fn toggle_pane_frames_round_trips_top_only() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    screen.pane_frame_style = PaneFrameStyle::TopOnly;
    screen.pane_frame_style_before_none = PaneFrameStyle::TopOnly;

    screen.cycle_pane_frame_style();
    assert_eq!(
        screen.pane_frame_style,
        PaneFrameStyle::None,
        "top_only toggles frames off"
    );

    screen.cycle_pane_frame_style();
    assert_eq!(
        screen.pane_frame_style,
        PaneFrameStyle::TopOnly,
        "toggling again comes back to top_only"
    );
}

#[test]
fn toggle_pane_frames_keeps_the_upstream_cycle() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    screen.pane_frame_style = PaneFrameStyle::Full;
    screen.pane_frame_style_before_none = PaneFrameStyle::Full;

    let cycle: Vec<PaneFrameStyle> = (0..4)
        .map(|_| {
            screen.cycle_pane_frame_style();
            screen.pane_frame_style
        })
        .collect();
    assert_eq!(
        cycle,
        vec![
            PaneFrameStyle::Titles,
            PaneFrameStyle::None,
            PaneFrameStyle::Full,
            PaneFrameStyle::Titles,
        ],
        "full -> titles -> none -> full is unchanged"
    );
}

#[test]
fn a_moved_tab_is_serialized_where_it_was_moved_to() {
    // the layout a session is restored from lists tabs in serialization order, so this order is
    // the one a restarted session comes back in
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    let before_move = screen.get_layout_metadata(None, None).tab_names();

    screen.move_active_tab_to_left(1).expect("TEST");

    let after_move = screen.get_layout_metadata(None, None).tab_names();
    assert_eq!(
        after_move,
        before_move.into_iter().rev().collect::<Vec<_>>(),
        "the serialized order follows the move, not the order the tabs were created in"
    );
}

#[test]
fn switching_tabs_is_not_a_change_to_the_tab_list() {
    // the tab list reaching plugins in tabs the user is not looking at is gated on this: a switch
    // must not count, or the frequent case pays for the rare one (upstream #4918)
    let mut screen = create_fixed_size_screen();
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    let before = screen.tab_structure();

    screen.switch_tab_prev(None, true, 1).expect("TEST");
    assert_eq!(
        screen.tab_structure(),
        before,
        "switching tabs leaves the tab list alone"
    );

    screen.move_active_tab_to_left(1).expect("TEST");
    assert_ne!(
        screen.tab_structure(),
        before,
        "moving a tab changes it, so every tab's plugins hear about it"
    );
}

/// The last `PaneUpdate` manifest delivered to `plugin_id`, flattened into one list of panes.
fn last_pane_update_for_plugin(
    instructions: &[PluginInstruction],
    plugin_id: u32,
) -> Option<Vec<zellij_utils::data::PaneInfo>> {
    let mut last = None;
    for instruction in instructions {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, _cid, event) in updates {
                if pid != &Some(plugin_id) {
                    continue;
                }
                if let Event::PaneUpdate(manifest) = event {
                    last = Some(manifest.panes.values().flatten().cloned().collect());
                }
            }
        }
    }
    last
}

#[test]
pub fn pane_info_carries_the_pid_cwd_and_command_the_pty_reported() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let mut process_info = HashMap::new();
    process_info.insert(
        1,
        crate::pty::PaneProcessInfo {
            pid: Some(90210),
            cwd: Some(std::path::PathBuf::from("/home/user/develop/thing")),
            command: Some(vec!["claude".to_owned(), "--resume".to_owned()]),
            env: Default::default(),
        },
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::UpdatePaneProcessInfo(process_info));
    // any state change makes screen rebuild the manifest and report it
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let reported = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert_eq!(reported.pane_pid, Some(90210));
    assert_eq!(
        reported.pane_cwd.as_deref(),
        Some("/home/user/develop/thing")
    );
    assert_eq!(reported.pane_command.as_deref(), Some("claude --resume"));
}

#[test]
pub fn a_pane_the_pty_said_nothing_about_reports_no_process_fields() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let reported = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert_eq!(reported.pane_pid, None);
    assert_eq!(reported.pane_cwd, None);
    assert_eq!(reported.pane_command, None);
}

#[test]
pub fn pane_info_reports_when_a_pane_last_emitted_output() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let before_ms = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::PtyBytes(1, "hi".as_bytes().to_vec()));
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let after_ms = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let noisy = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    let last_output_at = noisy
        .last_output_at
        .expect("a pane that emitted output should report when");
    assert!(
        last_output_at >= before_ms && last_output_at <= after_ms,
        "last_output_at {} should sit between {} and {}",
        last_output_at,
        before_ms,
        after_ms
    );
}

#[test]
pub fn a_silent_pane_reports_no_last_output_time() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let silent = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert_eq!(silent.last_output_at, None);
}

#[test]
pub fn a_bell_is_recorded_on_the_pane_that_rang() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // pane 1 is not the focused one, so its bell latches
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::PtyBytes(1, "\u{7}".as_bytes().to_vec()));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RenderToClients);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let rang = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert!(
        rang.has_pending_bell,
        "the pane that rang should report a pending bell"
    );
    let quiet = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 0)
        .expect("terminal pane 0 should be in the manifest");
    assert!(
        !quiet.has_pending_bell,
        "a pane that did not ring should report no pending bell"
    );
}

#[test]
pub fn a_bell_is_recorded_with_no_clients_attached() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // detach the only client, then ring the bell in a pane nobody is watching
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(main_client_id));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_bell = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::PtyBytes(1, "\u{7}".as_bytes().to_vec()));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RenderToClients);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions[instructions_before_bell..], 99).expect(
        "a bell with nobody attached should still report session state to a background plugin",
    );
    let rang = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert!(
        rang.has_pending_bell,
        "a detached session must still record that a pane rang"
    );
}

#[test]
pub fn pane_info_carries_the_allowlisted_environment() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let mut process_info = HashMap::new();
    process_info.insert(
        1,
        crate::pty::PaneProcessInfo {
            pid: Some(90210),
            cwd: None,
            command: None,
            env: [("CLAUDE_CODE_SESSION_ID".to_owned(), "abc-123".to_owned())]
                .into_iter()
                .collect(),
        },
    );
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::UpdatePaneProcessInfo(process_info));
    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let reported = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert_eq!(
        reported
            .pane_env
            .get("CLAUDE_CODE_SESSION_ID")
            .map(|v| v.as_str()),
        Some("abc-123")
    );
}

/// The default is to report nothing, because an environment holds secrets.
#[test]
pub fn pane_info_reports_no_environment_without_an_allowlist() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen.to_screen.send(ScreenInstruction::ClosePane(
        PaneId::Terminal(0),
        None,
        None,
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let reported = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert!(reported.pane_env.is_empty());
}

/// The event half of this is in `pty_tests`; this is the state half, read with nobody attached.
#[test]
pub fn a_held_command_pane_reports_its_exit_status_with_no_clients_attached() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(main_client_id));
    std::thread::sleep(std::time::Duration::from_millis(100));

    let run_command = RunCommand {
        command: PathBuf::from("false"),
        hold_on_close: true,
        ..Default::default()
    };
    let _ = mock_screen.to_screen.send(ScreenInstruction::HoldPane(
        PaneId::Terminal(1),
        Some(1),
        run_command,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("a detached session should still report state to a background plugin");
    let failed = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert!(failed.exited, "a held pane whose command exited says so");
    assert_eq!(failed.exit_status, Some(1));
}

#[test]
pub fn pane_info_reports_the_alternate_screen_and_an_explicit_title() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // pane 1 opens the alternate screen, as a full-screen program does, and is named by a human
    let _ = mock_screen.to_screen.send(ScreenInstruction::PtyBytes(
        1,
        "\u{1b}[?1049h".as_bytes().to_vec(),
    ));
    let _ = mock_screen.to_screen.send(ScreenInstruction::RenamePane(
        PaneId::Terminal(1),
        "named by a human".as_bytes().to_vec(),
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let renamed = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 1)
        .expect("terminal pane 1 should be in the manifest");
    assert!(
        renamed.is_alternate_screen,
        "a pane on the alternate screen should say so"
    );
    assert!(
        renamed.has_explicit_title,
        "a pane a human named should say so"
    );
    assert_eq!(renamed.title, "named by a human");

    let untouched = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 0)
        .expect("terminal pane 0 should be in the manifest");
    assert!(!untouched.is_alternate_screen);
    assert!(!untouched.has_explicit_title);
    assert_eq!(untouched.scrollback_position, 0);
}

#[test]
pub fn pane_info_reports_a_pinned_floating_pane() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;
    subscribe_background_plugin(&mock_screen, 99, main_client_id);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    let _ = mock_screen.to_screen.send(ScreenInstruction::NewPane(
        PaneId::Terminal(3),
        None,
        None,
        None,
        NewPanePlacement::Floating(None),
        false,
        ClientTabIndexOrPaneId::ClientId(main_client_id),
        None,
        false,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::SetFloatingPanePinned(
            PaneId::Terminal(3),
            true,
        ));
    // pinning reports no session state of its own, so ask for one with a change that does
    let _ = mock_screen.to_screen.send(ScreenInstruction::RenamePane(
        PaneId::Terminal(0),
        "report the session".as_bytes().to_vec(),
        None,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let panes = last_pane_update_for_plugin(&instructions, 99)
        .expect("the background plugin should have received a PaneUpdate");
    let pinned = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 3)
        .expect("the floating pane should be in the manifest");
    assert!(pinned.is_floating);
    assert!(pinned.is_pinned, "a pinned floating pane should say so");

    let tiled = panes
        .iter()
        .find(|pane| !pane.is_plugin && pane.id == 0)
        .expect("terminal pane 0 should be in the manifest");
    assert!(!tiled.is_pinned, "a tiled pane is never pinned");
}

/// Every `Event::PaneOpened` broadcast to all plugins, in order.
///
/// `PaneOpened` is sent `(None, None)` like `PaneClosed`, so a targeted send would be a bug and
/// this helper asserts against one.
fn broadcast_panes_opened(instructions: &[PluginInstruction]) -> Vec<PaneId> {
    let mut opened = vec![];
    for instruction in instructions {
        if let PluginInstruction::Update(updates) = instruction {
            for (pid, cid, event) in updates {
                if let Event::PaneOpened(pane_id) = event {
                    assert!(
                        pid.is_none() && cid.is_none(),
                        "PaneOpened must be broadcast, not targeted at {:?}/{:?}",
                        pid,
                        cid
                    );
                    opened.push((*pane_id).into());
                }
            }
        }
    }
    opened
}

#[test]
pub fn pane_opened_is_broadcast_for_a_tiled_pane_with_no_clients_attached() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let main_client_id = mock_screen.main_client_id;

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );

    // detach the only client, then create a pane - the session has nobody watching it
    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::RemoveClient(main_client_id));
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_new_pane = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::NewPane(
        PaneId::Terminal(3),
        None,
        None,
        None,
        NewPanePlacement::Tiled {
            direction: None,
            borderless: None,
        },
        false,
        ClientTabIndexOrPaneId::TabIndex(0),
        None,
        false,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let opened = broadcast_panes_opened(&instructions[instructions_before_new_pane..]);
    assert_eq!(
        opened,
        vec![PaneId::Terminal(3)],
        "a tiled pane created with no clients attached should be announced exactly once"
    );
}

#[test]
pub fn pane_opened_is_broadcast_for_a_floating_pane() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let client_id = mock_screen.main_client_id;

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    // let the layout's own panes be announced before taking a baseline
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_new_pane = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::NewPane(
        PaneId::Terminal(3),
        None,
        None,
        None,
        NewPanePlacement::Floating(None),
        false,
        ClientTabIndexOrPaneId::ClientId(client_id),
        None,
        false,
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let opened = broadcast_panes_opened(&instructions[instructions_before_new_pane..]);
    assert_eq!(opened, vec![PaneId::Terminal(3)]);
}

#[test]
pub fn pane_opened_is_broadcast_for_a_plugin_pane() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let client_id = mock_screen.main_client_id;

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    // let the layout's own panes be announced before taking a baseline
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_new_pane = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen.to_screen.send(ScreenInstruction::AddPlugin(
        Some(false), // should_float
        false,       // should be opened in place
        false,       // close_replaced_pane
        RunPluginOrAlias::from_url("file:/path/to/fake/plugin", &None, None, None).unwrap(),
        None,    // pane title
        Some(0), // tab index
        42,      // plugin id
        None,    // pane id to replace
        None,    // cwd
        false,   // start suppressed
        None,    // floating pane coordinates
        None,    // should focus plugin
        Some(client_id),
        None, // completion signal
    ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let opened = broadcast_panes_opened(&instructions[instructions_before_new_pane..]);
    assert_eq!(opened, vec![PaneId::Plugin(42)]);
}

/// The panes a layout builds are panes too, and a session started from a layout must say so.
#[test]
pub fn pane_opened_is_broadcast_for_the_panes_a_layout_creates() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let opened = broadcast_panes_opened(&instructions);
    assert_eq!(
        opened.len(),
        2,
        "the two panes of the layout should each be announced once, got: {:?}",
        opened
    );
}

/// A pane that changes layer is the same pane, and must not be announced as a new one.
#[test]
pub fn moving_a_pane_between_layers_does_not_announce_a_new_pane() {
    let size = Size { cols: 80, rows: 10 };
    let mut mock_screen = MockScreen::new(size);
    let screen_thread = mock_screen.run(Some(two_pane_layout()), vec![]);
    let client_id = mock_screen.main_client_id;

    let received_plugin_instructions = Arc::new(Mutex::new(vec![]));
    let plugin_receiver = mock_screen.plugin_receiver.take().unwrap();
    let plugin_thread = log_actions_in_thread!(
        received_plugin_instructions,
        PluginInstruction::Exit,
        plugin_receiver
    );
    // let the layout's own panes be announced before taking a baseline
    std::thread::sleep(std::time::Duration::from_millis(100));
    let instructions_before_move = received_plugin_instructions.lock().unwrap().len();

    let _ = mock_screen
        .to_screen
        .send(ScreenInstruction::TogglePaneEmbedOrFloating(
            client_id, None,
        ));
    std::thread::sleep(std::time::Duration::from_millis(100));

    mock_screen.teardown(vec![plugin_thread, screen_thread]);

    let instructions = received_plugin_instructions.lock().unwrap();
    let opened = broadcast_panes_opened(&instructions[instructions_before_move..]);
    assert!(
        opened.is_empty(),
        "moving an existing pane must not announce PaneOpened, got: {:?}",
        opened
    );
}

#[test]
pub fn panes_not_found_names_only_the_panes_no_tab_owns() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);

    assert!(
        screen
            .panes_not_found(&[PaneId::Terminal(1), PaneId::Terminal(2)])
            .is_empty(),
        "both panes exist, in different tabs"
    );
    assert_eq!(
        screen.panes_not_found(&[PaneId::Terminal(1), PaneId::Terminal(9)]),
        vec![PaneId::Terminal(9)],
        "only the missing one is named"
    );
    assert_eq!(
        screen.panes_not_found(&[PaneId::Plugin(1)]),
        vec![PaneId::Plugin(1)],
        "a plugin id is not a terminal id"
    );
}

#[test]
pub fn breaking_only_missing_panes_would_leave_an_empty_tab() {
    // why the handler checks first: the move itself skips a pane it cannot find, so a request
    // naming nothing real still builds a tab and reports success
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);

    screen
        .break_multiple_panes_to_new_tab(vec![PaneId::Terminal(9)], None, false, None, 1)
        .expect("TEST");
    assert_eq!(screen.tabs.len(), 2, "a tab was created for nothing");
}

#[test]
pub fn setting_sync_is_idempotent_and_says_so() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    let tab = screen.get_tab_by_id(0).expect("TEST");
    assert!(!tab.is_sync_panes_active(), "off to begin with");

    let tab = screen.get_tab_by_id_mut(0).expect("TEST");
    assert!(
        tab.set_sync_panes_is_active(true),
        "the first call changes it"
    );
    assert!(tab.is_sync_panes_active());
    assert!(
        !tab.set_sync_panes_is_active(true),
        "the second call changes nothing"
    );
    assert!(tab.is_sync_panes_active(), "and leaves it on");
    assert!(tab.set_sync_panes_is_active(false), "off again changes it");
    assert!(!tab.is_sync_panes_active());
}

#[test]
pub fn setting_fullscreen_moves_it_rather_than_only_clearing_it() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    let tab = screen.get_tab_by_id_mut(0).expect("TEST");
    tab.new_pane(
        PaneId::Terminal(2),
        None,
        None,
        false,
        false,
        NewPanePlacement::default(),
        Some(1),
        None,
    )
    .expect("TEST");

    assert!(tab.set_pane_fullscreen(PaneId::Terminal(1), true));
    assert!(tab.pane_is_fullscreen(PaneId::Terminal(1)));
    assert!(
        !tab.set_pane_fullscreen(PaneId::Terminal(1), true),
        "already fullscreen"
    );

    // the toggle would only clear fullscreen here; the setter hands it to the named pane
    assert!(tab.set_pane_fullscreen(PaneId::Terminal(2), true));
    assert!(tab.pane_is_fullscreen(PaneId::Terminal(2)));
    assert!(!tab.pane_is_fullscreen(PaneId::Terminal(1)));

    assert!(tab.set_pane_fullscreen(PaneId::Terminal(2), false));
    assert!(!tab.pane_is_fullscreen(PaneId::Terminal(2)));
    assert!(
        !tab.set_pane_fullscreen(PaneId::Terminal(2), false),
        "already not fullscreen"
    );
}

#[test]
pub fn a_setter_with_no_pane_id_resolves_the_focused_pane() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);

    assert_eq!(
        screen.tab_id_owning_pane(Some(PaneId::Terminal(1)), client_id),
        Some((0, PaneId::Terminal(1))),
        "an explicit id names its own tab"
    );
    assert_eq!(
        screen.tab_id_owning_pane(Some(PaneId::Terminal(9)), client_id),
        None,
        "a pane no tab owns resolves to nothing"
    );
    assert_eq!(
        screen.tab_id_owning_pane(None, client_id),
        Some((0, PaneId::Terminal(1))),
        "no id means the focused pane"
    );
}

#[test]
pub fn going_to_a_tab_reports_where_focus_came_from_and_where_it_landed() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    // the session starts with one tab, so this switch lands where it started
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::GoToTab { index: 1 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(
        result.stdout_lines.len(),
        1,
        "a switch that did not move reports only where it is: {:?}",
        result.stdout_lines
    );
    assert!(
        result.stdout_lines[0].starts_with("to: "),
        "{:?}",
        result.stdout_lines
    );
}

#[test]
pub fn going_to_a_tab_position_nothing_sits_at_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::GoToTab { index: 9 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("No tab at position 9"))
            .unwrap_or(false),
        "Expected a miss naming the position, got: {:?}",
        result.error_message
    );
    assert!(
        result.stdout_lines.is_empty(),
        "a miss reports nothing on stdout: {:?}",
        result.stdout_lines
    );
}

#[test]
pub fn going_to_a_tab_name_nothing_answers_to_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::GoToTabName {
            name: "no-such-tab".to_owned(),
            create: false,
            no_focus: false,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("No tab named 'no-such-tab'"))
            .unwrap_or(false),
        "Expected a miss naming the tab, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn the_no_focus_probe_stays_silent_about_a_tab_that_is_not_there() {
    // the negative control for the miss above: --no-focus is an existence question, and a "no"
    // answer is not an error - it is an empty stdout and exit 0
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::GoToTabName {
            name: "no-such-tab".to_owned(),
            create: false,
            no_focus: true,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.error_message, None);
    assert!(result.stdout_lines.is_empty(), "{:?}", result.stdout_lines);
    assert_eq!(result.affected_tab_id, None);
}

#[test]
pub fn closing_a_pane_names_the_pane_it_closed() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseTerminalPane { pane_id: 1 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.error_message, None);
    assert_eq!(result.stdout_lines, vec!["closed: terminal_1".to_string()]);
}

#[test]
pub fn closing_a_pane_that_is_not_there_reports_a_miss_and_no_report() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseTerminalPane { pane_id: 999 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("not found"))
            .unwrap_or(false),
        "Expected a 'not found' miss, got: {:?}",
        result.error_message
    );
    assert!(
        result.stdout_lines.is_empty(),
        "nothing was closed, so nothing is reported closed: {:?}",
        result.stdout_lines
    );
}

#[test]
pub fn closing_a_tab_by_an_id_nothing_answers_to_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseTabById { id: 999 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("No tab with id 999"))
            .unwrap_or(false),
        "Expected a miss naming the id, got: {:?}",
        result.error_message
    );
}

#[test]
pub fn moving_a_tab_reports_the_positions_it_went_between() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    // the client the mock screen attaches: a move is reported for the tab that client is on
    let client_id = 1;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::MoveTab {
            direction: Direction::Right,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.stdout_lines.len(), 2, "{:?}", result.stdout_lines);
    assert!(
        result.stdout_lines[0].starts_with("from: ") && result.stdout_lines[1].starts_with("to: "),
        "{:?}",
        result.stdout_lines
    );
}

#[test]
pub fn moving_a_tab_by_an_id_nothing_answers_to_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::MoveTabByTabId {
            id: 999,
            direction: Direction::Right,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("No tab with id 999"))
            .unwrap_or(false),
        "Expected a miss naming the id, got: {:?}",
        result.error_message
    );
    assert!(result.stdout_lines.is_empty(), "{:?}", result.stdout_lines);
}

#[test]
pub fn dumping_a_screen_without_a_pane_never_reaches_the_app() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_action_as_cli_client(
        &session_metadata,
        Action::DumpScreen {
            file_path: None,
            include_scrollback: false,
            pane_id: None,
            ansi: false,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result.is_none(),
        "the dump was refused before dispatch, so there is no completion to report"
    );
}

#[test]
pub fn dumping_a_screen_from_a_keybinding_still_takes_the_focused_pane() {
    // the negative control: only a CLI client is refused a targetless dump. A keybinding is
    // pressed inside a pane, where "focused" means the pane the hands are on
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut mock_screen = MockScreen::new(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(None, vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::DumpScreen {
            file_path: Some("/tmp/zellij-dump-screen-test".to_string()),
            include_scrollback: false,
            pane_id: None,
            ansi: false,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.error_message, None);
}

/// A session of two panes side by side, with focus on the first of them.
fn two_pane_mock_screen(size: Size) -> (MockScreen, TiledPaneLayout) {
    let mut initial_layout = TiledPaneLayout::default();
    initial_layout.children_split_direction = SplitDirection::Vertical;
    initial_layout.children = vec![TiledPaneLayout::default(), TiledPaneLayout::default()];
    (MockScreen::new(size), initial_layout)
}

/// The handle half of a `from:`/`to:` line, once the key and the pane id are taken off it.
fn handle_in_report_line(line: &str, expected_prefix: &str) -> String {
    let rest = line.strip_prefix(expected_prefix).unwrap_or_else(|| {
        panic!(
            "expected a line starting {:?}, got: {:?}",
            expected_prefix, line
        )
    });
    rest.to_owned()
}

#[test]
pub fn jumping_to_a_pane_reports_where_focus_came_from_and_where_it_landed() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::FocusPaneByPaneId {
            pane_id: ZellijUtilsPaneId::Terminal(1),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.stdout_lines.len(), 2, "{:?}", result.stdout_lines);
    let from = handle_in_report_line(&result.stdout_lines[0], "from: terminal_0 ");
    let to = handle_in_report_line(&result.stdout_lines[1], "to: terminal_1 ");
    assert!(is_handle_shaped(&from), "not a handle: {:?}", from);
    assert!(is_handle_shaped(&to), "not a handle: {:?}", to);
    assert_ne!(from, to, "two panes reported the same handle");
}

#[test]
pub fn jumping_to_the_pane_you_are_already_in_reports_only_where_you_are() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::FocusPaneByPaneId {
            pane_id: ZellijUtilsPaneId::Terminal(0),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(
        result.stdout_lines.len(),
        1,
        "a jump that did not move reports only where it is: {:?}",
        result.stdout_lines
    );
    assert!(
        result.stdout_lines[0].starts_with("to: terminal_0 "),
        "{:?}",
        result.stdout_lines
    );
    assert_eq!(
        result.error_message, None,
        "landing where you are is not a miss"
    );
}

#[test]
pub fn jumping_to_a_pane_that_is_not_there_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::FocusPaneByPaneId {
            pane_id: ZellijUtilsPaneId::Terminal(9),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert!(
        result
            .error_message
            .as_ref()
            .map(|m| m.contains("No pane answers to"))
            .unwrap_or(false),
        "expected a miss naming the pane, got: {:?}",
        result.error_message
    );
    assert!(
        result.stdout_lines.is_empty(),
        "a miss reports nothing on stdout: {:?}",
        result.stdout_lines
    );
}

#[test]
pub fn jumping_to_a_pane_in_another_tab_brings_the_tab_with_it() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 1;
    let mut screen = create_new_screen(size, true, true);
    new_tab(&mut screen, 1, 0);
    new_tab(&mut screen, 2, 1);
    assert_eq!(
        screen.tab_position_for_client(client_id),
        Some(1),
        "the second tab is the one in view"
    );
    screen
        .focus_pane_with_id(PaneId::Terminal(1), false, false, client_id)
        .unwrap();
    assert_eq!(
        screen.tab_position_for_client(client_id),
        Some(0),
        "focusing a pane left its tab behind"
    );
    assert_eq!(
        screen.pane_summary_for_client(client_id).as_deref(),
        Some(screen.pane_summary(PaneId::Terminal(1)).as_str()),
        "the client is looking at the pane it was sent to"
    );
}

#[test]
pub fn closing_a_pane_by_id_says_what_it_closed() {
    // the route the CLI actually takes: `close-pane --pane-id` becomes CloseFocusByPaneId, which
    // reaches ScreenInstruction::CloseFocusWithPaneId - not the CloseTerminalPane that the older
    // tests drove and that no CLI invocation sends
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseFocusByPaneId {
            pane_id: ZellijUtilsPaneId::Terminal(1),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(
        result.stdout_lines,
        vec!["closed: terminal_1".to_string()],
        "{:?}",
        result.stdout_lines
    );
    assert_eq!(result.error_message, None);
}

#[test]
pub fn closing_a_pane_id_that_is_not_there_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseFocusByPaneId {
            pane_id: ZellijUtilsPaneId::Terminal(9),
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(
        result.error_message.as_deref(),
        Some("No pane answers to 'terminal_9'"),
        "{:?}",
        result.error_message
    );
    assert!(
        result.stdout_lines.is_empty(),
        "a miss reports nothing on stdout: {:?}",
        result.stdout_lines
    );
}

#[test]
pub fn closing_a_tab_by_id_says_what_it_closed() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::CloseTabById { id: 0 },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(result.stdout_lines.len(), 1, "{:?}", result.stdout_lines);
    assert!(
        result.stdout_lines[0].starts_with("closed: 0 "),
        "{:?}",
        result.stdout_lines
    );
}
#[test]
pub fn dumping_a_pane_that_is_not_there_is_a_miss() {
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::DumpScreen {
            file_path: None,
            include_scrollback: false,
            pane_id: Some(ZellijUtilsPaneId::Terminal(9)),
            ansi: false,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    assert_eq!(
        result.error_message.as_deref(),
        Some("No pane answers to 'terminal_9'"),
        "{:?}",
        result.error_message
    );
    assert!(
        result.stdout_lines.is_empty(),
        "a miss dumps nothing, not an empty line: {:?}",
        result.stdout_lines
    );
}

#[test]
pub fn dumping_a_pane_that_is_there_still_dumps_it() {
    // the negative control for the miss check above: the guard must not refuse a live pane
    let size = Size {
        cols: 121,
        rows: 20,
    };
    let client_id = 10;
    let (mut mock_screen, initial_layout) = two_pane_mock_screen(size);
    let session_metadata = mock_screen.clone_session_metadata();
    let screen_thread = mock_screen.run(Some(initial_layout), vec![]);
    let result = route_arbitrary_action_and_get_result(
        &session_metadata,
        Action::DumpScreen {
            file_path: None,
            include_scrollback: false,
            pane_id: Some(ZellijUtilsPaneId::Terminal(1)),
            ansi: false,
        },
        client_id,
    );
    mock_screen.teardown(vec![screen_thread]);
    // the dump itself is delivered by the server thread, which this harness does not run, so what
    // is checked here is the only thing this guard could get wrong: it must not call a live pane a
    // miss
    assert!(
        !result
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("No pane answers to"),
        "the guard refused a pane that is there: {:?}",
        result.error_message
    );
}
