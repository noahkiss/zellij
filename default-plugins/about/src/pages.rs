use zellij_tile::prelude::*;

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use crate::active_component::{ActiveComponent, ClickAction};

/// What the main screen says about the binary this session runs, as the server described it.
#[derive(Debug, Clone, Default)]
pub struct ServerBinary {
    /// Where the running server actually is, symlinks resolved
    pub running: String,
    /// A path that still names this program after an upgrade - the pinned copy, or a name on PATH
    /// leading to the same file. Absent when the running path is the steadiest one there is.
    pub stable: Option<String>,
    /// Set by a macOS host, where the path is pasted into Full Disk Access
    pub full_disk_access_hint: bool,
}

impl ServerBinary {
    /// The path `<c>` copies: the one the user is being told to act on.
    pub fn path_to_copy(&self) -> &str {
        self.stable.as_deref().unwrap_or(&self.running)
    }
}

#[derive(Debug)]
pub struct Page {
    title: Option<Text>,
    components_to_render: Vec<RenderedComponent>,
    /// Indices into `components_to_render` that a pane too short for the page must still show
    essential_components: HashSet<usize>,
    has_hover: bool,
    hovering_over_link: bool,
    menu_item_is_selected: bool,
    pub is_main_screen: bool,
}

impl Page {
    pub fn new_main_screen(
        link_executable: Rc<RefCell<String>>,
        zellij_version: String,
        _base_mode: Rc<RefCell<InputMode>>,
        is_release_notes: bool,
        server_binary: Option<ServerBinary>,
    ) -> Self {
        let page = Page::new()
            .main_screen()
            .with_title(main_screen_title(zellij_version.clone(), is_release_notes))
            .with_bulletin_list(BulletinList::new(whats_new_title()).with_items(vec![
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Windows Support",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Windows Support").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        move || Page::new_windows_support()
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Remote Sessions",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Remote Sessions").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        let link_executable = link_executable.clone();
                        move || Page::new_remote_sessions(link_executable.clone())
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Read-Only Session Sharing",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Read-Only Session Sharing").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        let link_executable = link_executable.clone();
                        move || Page::new_read_only_sharing(link_executable.clone())
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "CLI Automation",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("CLI Automation").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        let link_executable = link_executable.clone();
                        move || Page::new_cli_automation(link_executable.clone())
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Mouse Resize",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Mouse Resize").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page(move || {
                        Page::new_mouse_resize()
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Click-to-Open File Paths",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Click-to-Open File Paths").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        move || Page::new_click_to_open()
                    })),
                    ActiveComponent::new(TextOrCustomRender::Text(main_menu_item(
                        "Layout Manager",
                    )))
                    .with_hover(TextOrCustomRender::Text(
                        main_menu_item("Layout Manager").selected(),
                    ))
                    .with_left_click_action(ClickAction::new_change_page({
                        move || Page::new_layout_manager()
                    })),
                ]))
            .with_paragraph(vec![ComponentLine::new(vec![
                ActiveComponent::new(TextOrCustomRender::Text(Text::new("Full Changelog: "))),
                ActiveComponent::new(TextOrCustomRender::Text(changelog_link_unselected(
                    zellij_version.clone(),
                )))
                .with_hover(TextOrCustomRender::CustomRender(
                    Box::new(changelog_link_selected(zellij_version.clone())),
                    Box::new(changelog_link_selected_len(zellij_version.clone())),
                ))
                .with_left_click_action(ClickAction::new_open_link(
                    format!(
                        "https://github.com/zellij-org/zellij/releases/tag/v{}",
                        zellij_version.clone()
                    ),
                    link_executable.clone(),
                )),
            ])])
            .with_paragraph(vec![ComponentLine::new(vec![
                ActiveComponent::new(TextOrCustomRender::Text(support_the_developer_text())),
                ActiveComponent::new(TextOrCustomRender::Text(sponsors_link_text_unselected()))
                    .with_hover(TextOrCustomRender::CustomRender(
                        Box::new(sponsors_link_text_selected),
                        Box::new(sponsors_link_text_selected_len),
                    ))
                    .with_left_click_action(ClickAction::new_open_link(
                        "https://github.com/sponsors/imsnif".to_owned(),
                        link_executable.clone(),
                    )),
            ])]);
        // the binary the server is actually running, and the path to act on where the two differ.
        // The server sends the hint only from a macOS host, where the path is copied into System
        // Settings -> Privacy & Security -> Full Disk Access (Cmd+Shift+G in the file picker);
        // elsewhere the paths answer "which build is this session running" and where it stays put.
        let has_path_to_copy = server_binary.is_some();
        let page = match server_binary {
            // every path gets a line to itself: it is the part that is copied, and sharing a line
            // with a label costs it those columns and truncates a long path into a wrong one
            Some(server_binary) => {
                page.with_essential_paragraph(server_binary_lines(server_binary))
            },
            None => page,
        };
        page.with_help(if is_release_notes {
            Box::new(move |hovering_over_link, menu_item_is_selected| {
                release_notes_main_help(hovering_over_link, menu_item_is_selected, has_path_to_copy)
            })
        } else {
            Box::new(move |hovering_over_link, menu_item_is_selected| {
                main_screen_help_text(hovering_over_link, menu_item_is_selected, has_path_to_copy)
            })
        })
    }
    fn new_windows_support() -> Page {
        Page::new()
            .with_title(Text::new("Windows Support").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Zellij now runs natively on Windows."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Windows users can now enjoy the same workspace management, plugin ecosystem"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("and multiplayer capabilities that have been available on Linux and macOS."),
                ))]),
            ])
            .with_help(Box::new(|_hovering_over_link, _menu_item_is_selected| {
                esc_to_go_back_help()
            }))
    }
    pub fn new_remote_sessions(link_executable: Rc<RefCell<String>>) -> Page {
        Page::new()
            .with_title(Text::new("Remote Sessions").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Attach to remote Zellij sessions over HTTPS, directly from the terminal."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("The remote session needs to be running the Zellij web client."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Zellij will attach to it exactly as a browser would, through the same interface."),
                ))]),
            ])
            .with_bulletin_list(
                BulletinList::new(Text::new("Try it:").color_range(2, ..))
                    .with_items(vec![
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("Run the Zellij web server on one machine")
                                .color_substring(3, "Zellij web server"),
                        ))
                        .with_hover(TextOrCustomRender::Text(
                            Text::new("Run the Zellij web server on one machine")
                                .color_substring(3, "Zellij web server")
                                .selected(),
                        ))
                        .with_left_click_action(ClickAction::new_launch_plugin(
                            "zellij:share".to_owned(),
                        )),
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("From another: zellij attach https://<ip>/<session-name>")
                                .color_substring(3, "zellij attach")
                                .color_substring(2, "https://<ip>/<session-name>"),
                        )),
                    ]),
            )
            .with_paragraph(vec![ComponentLine::new(vec![
                ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Learn more about the web client: ").color_range(2, ..),
                )),
                ActiveComponent::new(TextOrCustomRender::Text(Text::new(
                    "https://zellij.dev/tutorials/web-client/",
                )))
                .with_hover(TextOrCustomRender::CustomRender(
                    Box::new(web_client_link_selected),
                    Box::new(web_client_link_selected_len),
                ))
                .with_left_click_action(ClickAction::new_open_link(
                    "https://zellij.dev/tutorials/web-client/".to_owned(),
                    link_executable.clone(),
                )),
            ])])
            .with_help(Box::new(|hovering_over_link, menu_item_is_selected| {
                esc_go_back_plus_link_hover(hovering_over_link, menu_item_is_selected)
            }))
    }
    fn new_read_only_sharing(link_executable: Rc<RefCell<String>>) -> Page {
        Page::new()
            .with_title(Text::new("Read-Only Session Sharing").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Sessions can now be shared in read-only mode."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new(
                        "Useful for demonstrations, teaching, monitoring and pair programming",
                    )
                    .color_substring(2, "demonstrations")
                    .color_substring(2, "teaching")
                    .color_substring(2, "monitoring")
                    .color_substring(2, "pair programming"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("where one participant should observe without interfering."),
                ))]),
            ])
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Create a read-only web token with:")
                        .color_substring(2, "read-only web token"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("zellij web --create-read-only-token").color_range(3, ..),
                ))]),
            ])
            .with_paragraph(vec![ComponentLine::new(vec![ActiveComponent::new(
                TextOrCustomRender::Text(Text::new(
                    "Share the token for view-only access without risk of unintended input.",
                )),
            )])])
            .with_paragraph(vec![ComponentLine::new(vec![
                ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Learn more: ").color_range(2, ..),
                )),
                ActiveComponent::new(TextOrCustomRender::Text(Text::new(
                    "https://zellij.dev/tutorials/web-client/",
                )))
                .with_hover(TextOrCustomRender::CustomRender(
                    Box::new(web_client_link_selected),
                    Box::new(web_client_link_selected_len),
                ))
                .with_left_click_action(ClickAction::new_open_link(
                    "https://zellij.dev/tutorials/web-client/".to_owned(),
                    link_executable.clone(),
                )),
            ])])
            .with_help(Box::new(|hovering_over_link, menu_item_is_selected| {
                esc_go_back_plus_link_hover(hovering_over_link, menu_item_is_selected)
            }))
    }
    fn new_cli_automation(link_executable: Rc<RefCell<String>>) -> Page {
        Page::new()
            .with_title(Text::new("CLI Automation").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("This release significantly expands the CLI's control surface,"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("enabling the building of powerful workspace automations."),
                ))]),
            ])
            .with_bulletin_list(
                BulletinList::new(Text::new("New and expanded capabilities:").color_range(2, ..))
                    .with_items(vec![
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("list-panes, list-tabs, dump-screen, dump-layout with --json output")
                                .color_substring(3, "list-panes")
                                .color_substring(3, "list-tabs")
                                .color_substring(3, "dump-screen")
                                .color_substring(3, "dump-layout")
                                .color_substring(3, "--json"),
                        )),
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("zellij run optionally blocks until success/failure")
                                .color_substring(3, "zellij run"),
                        )),
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("zellij subscribe can stream pane scrollback in real time")
                                .color_substring(3, "zellij subscribe"),
                        )),
                        ActiveComponent::new(TextOrCustomRender::Text(
                            Text::new("zellij send-keys/paste can send human readable keys to other panes or sessions")
                                .color_substring(3, "zellij send-keys/paste"),
                        )),
                    ]),
            )
            .with_paragraph(vec![ComponentLine::new(vec![
                ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Learn more: ").color_range(2, ..),
                )),
                ActiveComponent::new(TextOrCustomRender::Text(Text::new(
                    "https://zellij.dev/documentation/controlling-zellij-through-cli.html",
                )))
                .with_hover(TextOrCustomRender::CustomRender(
                    Box::new(cli_automation_link_selected),
                    Box::new(cli_automation_link_selected_len),
                ))
                .with_left_click_action(ClickAction::new_open_link(
                    "https://zellij.dev/documentation/controlling-zellij-through-cli.html".to_owned(),
                    link_executable.clone(),
                )),
            ])])
            .with_help(Box::new(|hovering_over_link, menu_item_is_selected| {
                esc_go_back_plus_link_hover(hovering_over_link, menu_item_is_selected)
            }))
    }
    fn new_mouse_resize() -> Page {
        Page::new()
            .with_title(Text::new("Mouse Resize").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Panes can now be resized by dragging their borders with the mouse."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Tiled panes can be resized with or without Ctrl held down.")
                        .color_substring(3, "Ctrl"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Floating panes require Ctrl+drag to resize.")
                        .color_substring(3, "Ctrl+drag"),
                ))]),
            ])
            .with_paragraph(vec![ComponentLine::new(vec![ActiveComponent::new(
                TextOrCustomRender::Text(
                    Text::new("Try it: Ctrl+drag on the borders of this pane.")
                        .color_substring(2, "Try it:")
                        .color_substring(3, "Ctrl+drag"),
                ),
            )])])
            .with_help(Box::new(|_hovering_over_link, _menu_item_is_selected| {
                esc_to_go_back_help()
            }))
    }
    fn new_click_to_open() -> Page {
        Page::new()
            .with_title(Text::new("Click-to-Open File Paths").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Zellij now detects file paths in the terminal viewport."),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Alt-Click on a file path to open it.")
                        .color_substring(3, "Alt-Click"),
                ))]),
            ])
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Useful for navigating compiler errors, grep results,")
                        .color_substring(2, "compiler errors")
                        .color_substring(2, "grep results"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("log files, or any output containing file paths.")
                        .color_substring(2, "log files"),
                ))]),
            ])
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Plugins can also highlight arbitrary text in the viewport,"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("opening possibilities for custom link handlers")
                        .color_substring(3, "custom link handlers"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("and interactive overlays.")
                        .color_substring(3, "interactive overlays"),
                ))]),
            ])
            .with_help(Box::new(|_hovering_over_link, _menu_item_is_selected| {
                esc_to_go_back_help()
            }))
    }
    fn new_layout_manager() -> Page {
        Page::new()
            .with_title(Text::new("Layout Manager").color_range(0, ..))
            .with_paragraph(vec![
                ComponentLine::new(vec![
                    ActiveComponent::new(TextOrCustomRender::Text(Text::new("A new "))),
                    ActiveComponent::new(TextOrCustomRender::Text(
                        Text::new("layout-manager interface").color_range(3, ..),
                    ))
                    .with_hover(TextOrCustomRender::Text(
                        Text::new("layout-manager interface")
                            .color_range(3, ..)
                            .selected(),
                    ))
                    .with_left_click_action(ClickAction::new_launch_plugin(
                        "zellij:layout-manager".to_owned(),
                    )),
                    ActiveComponent::new(TextOrCustomRender::Text(Text::new(
                        " allows overriding layouts at runtime.",
                    ))),
                ]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new(
                        "Workspaces can be reconfigured dynamically without restarting sessions.",
                    ),
                ))]),
            ])
            .with_paragraph(vec![
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("Access it through the session menu, or run:")
                        .color_substring(2, "session menu"),
                ))]),
                ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
                    Text::new("zellij plugin -- zellij:layout-manager").color_range(3, ..),
                ))]),
            ])
            .with_help(Box::new(|_hovering_over_link, _menu_item_is_selected| {
                esc_to_go_back_help()
            }))
    }
}

impl Page {
    pub fn new() -> Self {
        Page {
            title: None,
            components_to_render: vec![],
            essential_components: HashSet::new(),
            has_hover: false,
            hovering_over_link: false,
            menu_item_is_selected: false,
            is_main_screen: false,
        }
    }
    pub fn main_screen(mut self) -> Self {
        self.is_main_screen = true;
        self
    }
    pub fn with_title(mut self, title: Text) -> Self {
        self.title = Some(title);
        self
    }
    pub fn with_bulletin_list(mut self, bulletin_list: BulletinList) -> Self {
        self.components_to_render
            .push(RenderedComponent::BulletinList(bulletin_list));
        self
    }
    pub fn with_paragraph(mut self, paragraph: Vec<ComponentLine>) -> Self {
        self.components_to_render
            .push(RenderedComponent::Paragraph(paragraph));
        self
    }
    /// A paragraph that a pane too short for the whole page keeps at the cost of the rest.
    pub fn with_essential_paragraph(mut self, paragraph: Vec<ComponentLine>) -> Self {
        self.essential_components
            .insert(self.components_to_render.len());
        self.with_paragraph(paragraph)
    }
    pub fn with_help(mut self, help_text_fn: Box<dyn Fn(bool, bool) -> Text>) -> Self {
        self.components_to_render
            .push(RenderedComponent::HelpText(help_text_fn));
        self
    }
    pub fn handle_key(&mut self, key: KeyWithModifier) -> bool {
        let mut should_render = false;
        if key.bare_key == BareKey::Down && key.has_no_modifiers() {
            self.move_selection_down();
            should_render = true;
        } else if key.bare_key == BareKey::Up && key.has_no_modifiers() {
            self.move_selection_up();
            should_render = true;
        }
        should_render
    }
    pub fn handle_mouse_left_click(&mut self, x: usize, y: usize) -> Option<Page> {
        for rendered_component in &mut self.components_to_render {
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    let page_to_render = bulletin_list.handle_left_click_at_position(x, y);
                    if page_to_render.is_some() {
                        return page_to_render;
                    }
                },
                RenderedComponent::Paragraph(paragraph) => {
                    for component_line in paragraph {
                        let page_to_render = component_line.handle_left_click_at_position(x, y);
                        if page_to_render.is_some() {
                            return page_to_render;
                        }
                    }
                },
                _ => {},
            }
        }
        None
    }
    pub fn handle_selection(&mut self) -> Option<Page> {
        for rendered_component in &mut self.components_to_render {
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    let page_to_render = bulletin_list.handle_selection();
                    if page_to_render.is_some() {
                        return page_to_render;
                    }
                },
                _ => {},
            }
        }
        None
    }
    pub fn handle_mouse_hover(&mut self, x: usize, y: usize) -> bool {
        let hover_cleared = self.clear_hover(); // TODO: do the right thing if the same component was hovered from
                                                // previous motion
        for rendered_component in &mut self.components_to_render {
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    let should_render = bulletin_list.handle_hover_at_position(x, y);
                    if should_render {
                        self.has_hover = true;
                        self.menu_item_is_selected = true;
                        return should_render;
                    }
                },
                RenderedComponent::Paragraph(paragraph) => {
                    for component_line in paragraph {
                        let should_render = component_line.handle_hover_at_position(x, y);
                        if should_render {
                            self.has_hover = true;
                            self.hovering_over_link = true;
                            return should_render;
                        }
                    }
                },
                _ => {},
            }
        }
        hover_cleared
    }
    fn move_selection_up(&mut self) {
        match self.position_of_active_bulletin() {
            Some(position_of_active_bulletin) if position_of_active_bulletin > 0 => {
                self.clear_active_bulletins();
                self.set_active_bulletin(position_of_active_bulletin.saturating_sub(1));
            },
            Some(0) => {
                self.clear_active_bulletins();
            },
            _ => {
                self.clear_active_bulletins();
                self.set_last_active_bulletin();
            },
        }
    }
    fn move_selection_down(&mut self) {
        match self.position_of_active_bulletin() {
            Some(position_of_active_bulletin) => {
                self.clear_active_bulletins();
                self.set_active_bulletin(position_of_active_bulletin + 1);
            },
            None => {
                self.set_active_bulletin(0);
            },
        }
    }
    fn position_of_active_bulletin(&self) -> Option<usize> {
        self.components_to_render.iter().find_map(|c| match c {
            RenderedComponent::BulletinList(bulletin_list) => {
                bulletin_list.active_component_position()
            },
            _ => None,
        })
    }
    fn clear_active_bulletins(&mut self) {
        self.components_to_render.iter_mut().for_each(|c| {
            match c {
                RenderedComponent::BulletinList(bulletin_list) => {
                    Some(bulletin_list.clear_active_bulletins())
                },
                _ => None,
            };
        });
    }
    fn set_active_bulletin(&mut self, active_bulletin_position: usize) {
        self.components_to_render.iter_mut().for_each(|c| {
            match c {
                RenderedComponent::BulletinList(bulletin_list) => {
                    bulletin_list.set_active_bulletin(active_bulletin_position)
                },
                _ => {},
            };
        });
    }
    fn set_last_active_bulletin(&mut self) {
        self.components_to_render.iter_mut().for_each(|c| {
            match c {
                RenderedComponent::BulletinList(bulletin_list) => {
                    bulletin_list.set_last_active_bulletin()
                },
                _ => {},
            };
        });
    }
    fn clear_hover(&mut self) -> bool {
        let had_hover = self.has_hover;
        self.menu_item_is_selected = false;
        self.hovering_over_link = false;
        for rendered_component in &mut self.components_to_render {
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    bulletin_list.clear_hover();
                },
                RenderedComponent::Paragraph(paragraph) => {
                    for active_component in paragraph {
                        active_component.clear_hover();
                    }
                },
                _ => {},
            }
        }
        self.has_hover = false;
        had_hover
    }
    pub fn ui_column_count(&mut self) -> usize {
        let mut column_count = 0;
        for rendered_component in &self.components_to_render {
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    column_count = std::cmp::max(column_count, bulletin_list.column_count());
                },
                RenderedComponent::Paragraph(paragraph) => {
                    for active_component in paragraph {
                        column_count = std::cmp::max(column_count, active_component.column_count());
                    }
                },
                RenderedComponent::HelpText(_text) => {}, // we ignore help text in column
                                                          // calculation because it's always left
                                                          // justified
            }
        }
        column_count
    }
    /// The rows the page wants, given the components a short pane made it give up.
    fn ui_row_count_without(&self, hidden: &HashSet<usize>) -> usize {
        let mut row_count = 0;
        if self.title.is_some() {
            row_count += 1;
        }
        for (index, rendered_component) in self.components_to_render.iter().enumerate() {
            if hidden.contains(&index) {
                continue;
            }
            match rendered_component {
                RenderedComponent::BulletinList(bulletin_list) => {
                    row_count += bulletin_list.len();
                },
                RenderedComponent::Paragraph(paragraph) => {
                    row_count += paragraph.len();
                },
                RenderedComponent::HelpText(_text) => {}, // we ignore help text as it is outside
                                                          // the UI container
            }
        }
        row_count += self.components_to_render.len() - hidden.len();
        row_count
    }
    /// Which components to leave out of a pane too short to hold the page.
    ///
    /// The page is a fixed block centered in the pane, so whatever does not fit falls off the
    /// bottom and is never seen - which is fine for a decorative list and not fine for a line a
    /// user opened this page to copy. Give up the biggest thing that is not essential first, so
    /// the "What's new" list goes before a one-line paragraph does, until the rest fits.
    fn components_to_hide(&self, rows: usize) -> HashSet<usize> {
        let mut hidden = HashSet::new();
        while self.ui_row_count_without(&hidden) > rows {
            let biggest_expendable = self
                .components_to_render
                .iter()
                .enumerate()
                .filter(|(index, component)| {
                    !hidden.contains(index)
                        && !self.essential_components.contains(index)
                        && !matches!(component, RenderedComponent::HelpText(_))
                })
                .max_by_key(|(index, component)| (component.row_count(), *index));
            match biggest_expendable {
                Some((index, _)) => {
                    hidden.insert(index);
                },
                // everything left is essential: it overflows, which beats hiding the point of the
                // page
                None => break,
            }
        }
        hidden
    }
    pub fn render(&mut self, rows: usize, columns: usize, error: &Option<String>) {
        let hidden = self.components_to_hide(rows);
        let base_x = columns.saturating_sub(self.ui_column_count()) / 2;
        let base_y = rows.saturating_sub(self.ui_row_count_without(&hidden)) / 2;
        let mut current_y = base_y;
        if let Some(title) = &self.title {
            print_text_with_coordinates(
                title.clone(),
                base_x,
                current_y,
                Some(columns),
                Some(rows),
            );
            current_y += 2;
        }
        for (index, rendered_component) in self.components_to_render.iter_mut().enumerate() {
            if hidden.contains(&index) {
                // a component that did not render must not stay clickable where it used to be
                rendered_component.clear_rendered_coordinates();
                continue;
            }
            let is_help = match rendered_component {
                RenderedComponent::HelpText(_) => true,
                _ => false,
            };
            if is_help {
                if let Some(error) = error {
                    render_error(error, rows);
                    continue;
                }
            }
            let y = if is_help { rows } else { current_y };
            let columns = if is_help {
                columns
            } else {
                columns.saturating_sub(base_x * 2)
            };
            let rendered_rows = rendered_component.render(
                base_x,
                y,
                rows,
                columns,
                self.hovering_over_link,
                self.menu_item_is_selected,
            );
            current_y += rendered_rows + 1; // 1 for the line space between components
        }
    }
}

fn render_error(error: &str, y: usize) {
    print_text_with_coordinates(
        Text::new(format!("ERROR: {}", error)).color_range(3, ..),
        0,
        y,
        None,
        None,
    );
}

/// The label above the server binary path, saying what the path is for when there is something to
/// say. Only a macOS host sends the hint, because only macOS has a panel to paste the path into.
fn server_binary_label(with_full_disk_access_hint: bool) -> Text {
    if with_full_disk_access_hint {
        Text::new("Server binary (macOS: grant Full Disk Access to this path):").color_range(2, ..)
    } else {
        Text::new("Server binary:").color_range(2, ..)
    }
}

/// The label above the upgrade-proof path, which is shown only when it names a second file.
///
/// It is the path worth writing down, so it says what it is good for: on macOS a permission grant
/// follows the file, and a versioned path loses the grant at every upgrade.
fn stable_binary_label(with_full_disk_access_hint: bool) -> Text {
    if with_full_disk_access_hint {
        Text::new("Grant Full Disk Access to this path instead - it survives upgrades:")
            .color_range(2, ..)
    } else {
        Text::new("Stable path (survives upgrades):").color_range(2, ..)
    }
}

/// The lines of the server binary paragraph: one label and one path per binary named.
fn server_binary_lines(server_binary: ServerBinary) -> Vec<ComponentLine> {
    let ServerBinary {
        running,
        stable,
        full_disk_access_hint,
    } = server_binary;
    let mut lines = vec![
        ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
            // with a steadier path below it, this line answers "which build is running" only
            match stable {
                Some(_) => Text::new("Server binary (running):").color_range(2, ..),
                None => server_binary_label(full_disk_access_hint),
            },
        ))]),
        ComponentLine::new(vec![ActiveComponent::new(TextOrCustomRender::Text(
            Text::new(running),
        ))]),
    ];
    if let Some(stable) = stable {
        lines.push(ComponentLine::new(vec![ActiveComponent::new(
            TextOrCustomRender::Text(stable_binary_label(full_disk_access_hint)),
        )]));
        lines.push(ComponentLine::new(vec![ActiveComponent::new(
            TextOrCustomRender::Text(Text::new(stable)),
        )]));
    }
    lines
}

fn changelog_link_unselected(version: String) -> Text {
    let full_changelog_text = format!(
        "https://github.com/zellij-org/zellij/releases/tag/v{}",
        version
    );
    Text::new(full_changelog_text)
}

fn changelog_link_selected(version: String) -> Box<dyn Fn(usize, usize) -> usize> {
    Box::new(move |x, y| {
        print!(
            "\u{1b}[{};{}H\u{1b}[m\u{1b}[1;4mhttps://github.com/zellij-org/zellij/releases/tag/v{}",
            y + 1,
            x + 1,
            version
        );
        51 + version.chars().count()
    })
}

fn changelog_link_selected_len(version: String) -> Box<dyn Fn() -> usize> {
    Box::new(move || 51 + version.chars().count())
}

fn sponsors_link_text_unselected() -> Text {
    Text::new("https://github.com/sponsors/imsnif")
}

fn sponsors_link_text_selected(x: usize, y: usize) -> usize {
    print!(
        "\u{1b}[{};{}H\u{1b}[m\u{1b}[1;4mhttps://github.com/sponsors/imsnif",
        y + 1,
        x + 1
    );
    34
}

fn sponsors_link_text_selected_len() -> usize {
    34
}

fn cli_automation_link_selected(x: usize, y: usize) -> usize {
    print!(
        "\u{1b}[{};{}H\u{1b}[m\u{1b}[1;4mhttps://zellij.dev/documentation/controlling-zellij-through-cli.html",
        y + 1,
        x + 1
    );
    68
}

fn cli_automation_link_selected_len() -> usize {
    68
}

fn web_client_link_selected(x: usize, y: usize) -> usize {
    print!(
        "\u{1b}[{};{}H\u{1b}[m\u{1b}[1;4mhttps://zellij.dev/tutorials/web-client/",
        y + 1,
        x + 1
    );
    40
}

fn web_client_link_selected_len() -> usize {
    40
}

// Text components
fn whats_new_title() -> Text {
    Text::new("What's new?")
}

fn main_screen_title(version: String, is_release_notes: bool) -> Text {
    if is_release_notes {
        let title_text = format!("Hi there, welcome to Zellij {}!", &version);
        Text::new(title_text).color_range(2, 21..=27 + version.chars().count())
    } else {
        let title_text = format!("Zellij {}", &version);
        Text::new(title_text).color_range(2, ..)
    }
}

/// What the copy binding adds to a help line, when the page has a path worth copying.
const COPY_PATH_HELP: &str = ", <c> - Copy Path";

/// Add the copy hint to a help line and colour its key where it lands.
///
/// The columns come from the line it is appended to, so a reworded help line cannot leave the
/// colour pointing at the wrong characters.
fn with_copy_path_help(
    help_text: String,
    has_path_to_copy: bool,
) -> (String, Option<(usize, usize)>) {
    if !has_path_to_copy {
        return (help_text, None);
    }
    let key_start = help_text.chars().count() + 2; // past the ", " that leads the hint
    (
        format!("{}{}", help_text, COPY_PATH_HELP),
        Some((key_start, key_start + 2)), // "<c>"
    )
}

/// Colour the copy key, if the hint was added at all.
fn color_copy_path_help(text: Text, copy_key: Option<(usize, usize)>) -> Text {
    match copy_key {
        Some((start, end)) => text.color_range(1, start..=end),
        None => text,
    }
}

fn main_screen_help_text(
    hovering_over_link: bool,
    menu_item_is_selected: bool,
    has_path_to_copy: bool,
) -> Text {
    if hovering_over_link {
        let help_text = format!("Help: Click or Shift-Click to open in browser");
        Text::new(help_text)
            .color_range(3, 6..=10)
            .color_range(3, 15..=25)
    } else if menu_item_is_selected {
        let help_text = format!("Help: <↓↑> - Navigate, <ENTER> - Learn More, <ESC> - Dismiss");
        let (help_text, copy_key) = with_copy_path_help(help_text, has_path_to_copy);
        color_copy_path_help(
            Text::new(help_text)
                .color_range(1, 6..=9)
                .color_range(1, 23..=29)
                .color_range(1, 45..=49),
            copy_key,
        )
    } else {
        let help_text = format!("Help: <↓↑> - Navigate, <ESC> - Dismiss, <?> - Usage Tips");
        let (help_text, copy_key) = with_copy_path_help(help_text, has_path_to_copy);
        color_copy_path_help(
            Text::new(help_text)
                .color_range(1, 6..=9)
                .color_range(1, 23..=27)
                .color_range(1, 40..=42),
            copy_key,
        )
    }
}

fn release_notes_main_help(
    hovering_over_link: bool,
    menu_item_is_selected: bool,
    has_path_to_copy: bool,
) -> Text {
    if hovering_over_link {
        let help_text = format!("Help: Click or Shift-Click to open in browser");
        Text::new(help_text)
            .color_range(3, 6..=10)
            .color_range(3, 15..=25)
    } else if menu_item_is_selected {
        let help_text = format!("Help: <↓↑> - Navigate, <ENTER> - Learn More, <ESC> - Dismiss");
        let (help_text, copy_key) = with_copy_path_help(help_text, has_path_to_copy);
        color_copy_path_help(
            Text::new(help_text)
                .color_range(1, 6..=9)
                .color_range(1, 23..=29)
                .color_range(1, 45..=49),
            copy_key,
        )
    } else {
        let help_text = format!("Help: <↓↑> - Navigate, <ESC> - Dismiss");
        let (help_text, copy_key) = with_copy_path_help(help_text, has_path_to_copy);
        color_copy_path_help(
            Text::new(help_text)
                .color_range(1, 6..=9)
                .color_range(1, 23..=27),
            copy_key,
        )
    }
}

fn esc_go_back_plus_link_hover(hovering_over_link: bool, _menu_item_is_selected: bool) -> Text {
    if hovering_over_link {
        let help_text = format!("Help: Click or Shift-Click to open in browser");
        Text::new(help_text)
            .color_range(3, 6..=10)
            .color_range(3, 15..=25)
    } else {
        let help_text = format!("Help: <ESC> - Go back");
        Text::new(help_text).color_range(1, 6..=10)
    }
}

fn esc_to_go_back_help() -> Text {
    let help_text = format!("Help: <ESC> - Go back");
    Text::new(help_text).color_range(1, 6..=10)
}

fn main_menu_item(item_name: &str) -> Text {
    Text::new(item_name).color_range(0, ..)
}

fn support_the_developer_text() -> Text {
    let support_text = format!("Please support the Zellij developer <3: ");
    Text::new(support_text).color_range(3, ..)
}

pub enum TextOrCustomRender {
    Text(Text),
    CustomRender(
        Box<dyn Fn(usize, usize) -> usize>, // (rows, columns) -> text_len (render function)
        Box<dyn Fn() -> usize>,             // length of rendered component
    ),
}

impl TextOrCustomRender {
    pub fn len(&self) -> usize {
        match self {
            TextOrCustomRender::Text(text) => text.len(),
            TextOrCustomRender::CustomRender(_render_fn, len_fn) => len_fn(),
        }
    }
    pub fn render(&mut self, x: usize, y: usize, rows: usize, columns: usize) -> usize {
        match self {
            TextOrCustomRender::Text(text) => {
                print_text_with_coordinates(text.clone(), x, y, Some(columns), Some(rows));
                text.len()
            },
            TextOrCustomRender::CustomRender(render_fn, _len_fn) => render_fn(x, y),
        }
    }
}

impl std::fmt::Debug for TextOrCustomRender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TextOrCustomRender::Text(text) => write!(f, "Text {{ {:?} }}", text),
            TextOrCustomRender::CustomRender(..) => write!(f, "CustomRender"),
        }
    }
}

enum RenderedComponent {
    HelpText(Box<dyn Fn(bool, bool) -> Text>),
    BulletinList(BulletinList),
    Paragraph(Vec<ComponentLine>),
}

impl std::fmt::Debug for RenderedComponent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RenderedComponent::HelpText(_) => write!(f, "HelpText"),
            RenderedComponent::BulletinList(bulletinlist) => write!(f, "{:?}", bulletinlist),
            RenderedComponent::Paragraph(component_list) => write!(f, "{:?}", component_list),
        }
    }
}

impl RenderedComponent {
    pub fn render(
        &mut self,
        x: usize,
        y: usize,
        rows: usize,
        columns: usize,
        hovering_over_link: bool,
        menu_item_is_selected: bool,
    ) -> usize {
        let mut rendered_rows = 0;
        match self {
            RenderedComponent::HelpText(text) => {
                rendered_rows += 1;
                print_text_with_coordinates(
                    text(hovering_over_link, menu_item_is_selected),
                    0,
                    y,
                    Some(columns),
                    Some(rows),
                );
            },
            RenderedComponent::BulletinList(bulletin_list) => {
                rendered_rows += bulletin_list.len();
                bulletin_list.render(x, y, rows, columns);
            },
            RenderedComponent::Paragraph(paragraph) => {
                let mut paragraph_rendered_rows = 0;
                for component_line in paragraph {
                    component_line.render(
                        x,
                        y + paragraph_rendered_rows,
                        rows.saturating_sub(paragraph_rendered_rows),
                        columns,
                    );
                    rendered_rows += 1;
                    paragraph_rendered_rows += 1;
                }
            },
        }
        rendered_rows
    }
    /// Rows this takes inside the UI container. Help text sits outside it and so costs none.
    pub fn row_count(&self) -> usize {
        match self {
            RenderedComponent::HelpText(_) => 0,
            RenderedComponent::BulletinList(bulletin_list) => bulletin_list.len(),
            RenderedComponent::Paragraph(paragraph) => paragraph.len(),
        }
    }
    pub fn clear_rendered_coordinates(&mut self) {
        match self {
            RenderedComponent::HelpText(_) => {},
            RenderedComponent::BulletinList(bulletin_list) => {
                bulletin_list.clear_rendered_coordinates()
            },
            RenderedComponent::Paragraph(paragraph) => {
                for component_line in paragraph {
                    component_line.clear_rendered_coordinates();
                }
            },
        }
    }
}

#[derive(Debug)]
pub struct BulletinList {
    title: Text,
    items: Vec<ActiveComponent>,
}

impl BulletinList {
    pub fn new(title: Text) -> Self {
        BulletinList {
            title,
            items: vec![],
        }
    }
    pub fn with_items(mut self, items: Vec<ActiveComponent>) -> Self {
        self.items = items;
        self
    }
    pub fn len(&self) -> usize {
        self.items.len() + 1 // 1 for the title
    }
    pub fn clear_rendered_coordinates(&mut self) {
        for item in &mut self.items {
            item.clear_rendered_coordinates();
        }
    }
    pub fn column_count(&self) -> usize {
        let mut column_count = 0;
        for item in &self.items {
            column_count = std::cmp::max(column_count, item.column_count());
        }
        column_count
    }
    pub fn handle_left_click_at_position(&mut self, x: usize, y: usize) -> Option<Page> {
        for component in &mut self.items {
            let page_to_render = component.handle_left_click_at_position(x, y);
            if page_to_render.is_some() {
                return page_to_render;
            }
        }
        None
    }
    pub fn handle_selection(&mut self) -> Option<Page> {
        for component in &mut self.items {
            let page_to_render = component.handle_selection();
            if page_to_render.is_some() {
                return page_to_render;
            }
        }
        None
    }
    pub fn handle_hover_at_position(&mut self, x: usize, y: usize) -> bool {
        for component in &mut self.items {
            let should_render = component.handle_hover_at_position(x, y);
            if should_render {
                return should_render;
            }
        }
        false
    }
    pub fn clear_hover(&mut self) {
        for component in &mut self.items {
            component.clear_hover();
        }
    }
    pub fn active_component_position(&self) -> Option<usize> {
        self.items.iter().position(|i| i.is_active)
    }
    pub fn clear_active_bulletins(&mut self) {
        self.items.iter_mut().for_each(|i| {
            i.is_active = false;
        });
    }
    pub fn set_active_bulletin(&mut self, new_index: usize) {
        self.items.get_mut(new_index).map(|i| {
            i.is_active = true;
        });
    }
    pub fn set_last_active_bulletin(&mut self) {
        self.items.last_mut().map(|i| {
            i.is_active = true;
        });
    }
    pub fn render(&mut self, x: usize, y: usize, rows: usize, columns: usize) {
        print_text_with_coordinates(self.title.clone(), x, y, Some(columns), Some(rows));
        let mut item_bulletin = 1;
        let mut running_y = y + 1;
        for item in &mut self.items {
            let mut item_bulletin_text = Text::new(format!("{}. ", item_bulletin));
            if item.is_active {
                item_bulletin_text = item_bulletin_text.selected();
            }
            let item_bulletin_text_len = item_bulletin_text.len();
            print_text_with_coordinates(
                item_bulletin_text,
                x,
                running_y,
                Some(item_bulletin_text_len),
                Some(rows),
            );
            item.render(
                x + item_bulletin_text_len,
                running_y,
                rows,
                columns.saturating_sub(item_bulletin_text_len),
            );
            running_y += 1;
            item_bulletin += 1;
        }
    }
}

#[derive(Debug)]
pub struct ComponentLine {
    components: Vec<ActiveComponent>,
}

impl ComponentLine {
    pub fn handle_left_click_at_position(&mut self, x: usize, y: usize) -> Option<Page> {
        for active_component in &mut self.components {
            let page_to_render = active_component.handle_left_click_at_position(x, y);
            if page_to_render.is_some() {
                return page_to_render;
            }
        }
        None
    }
    pub fn handle_hover_at_position(&mut self, x: usize, y: usize) -> bool {
        for active_component in &mut self.components {
            let should_render = active_component.handle_hover_at_position(x, y);
            if should_render {
                return should_render;
            }
        }
        false
    }
    pub fn clear_hover(&mut self) {
        for active_component in &mut self.components {
            active_component.clear_hover();
        }
    }
    pub fn column_count(&self) -> usize {
        let mut column_count = 0;
        for active_component in &self.components {
            column_count += active_component.column_count()
        }
        column_count
    }
    pub fn render(&mut self, x: usize, y: usize, rows: usize, columns: usize) {
        let mut current_x = x;
        let mut columns_left = columns;
        for component in &mut self.components {
            let component_len = component.render(current_x, y, rows, columns_left);
            current_x += component_len;
            columns_left = columns_left.saturating_sub(component_len);
        }
    }
}

impl ComponentLine {
    pub fn new(components: Vec<ActiveComponent>) -> Self {
        ComponentLine { components }
    }
    pub fn clear_rendered_coordinates(&mut self) {
        for component in &mut self.components {
            component.clear_rendered_coordinates();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERVER_EXE: &str = "/opt/homebrew/Cellar/zellij/0.45.0/bin/zellij";
    const STABLE_EXE: &str = "/Users/someone/Library/Application Support/zellij/bin/zellij";

    fn server_binary(stable: Option<&str>) -> ServerBinary {
        ServerBinary {
            running: String::from(SERVER_EXE),
            stable: stable.map(String::from),
            full_disk_access_hint: true,
        }
    }

    fn main_screen() -> Page {
        main_screen_with(Some(server_binary(None)))
    }

    fn main_screen_with(server_binary: Option<ServerBinary>) -> Page {
        Page::new_main_screen(
            Rc::new(RefCell::new(String::from("open"))),
            String::from("0.45.0"),
            Rc::new(RefCell::new(InputMode::Normal)),
            false,
            server_binary,
        )
    }

    /// The index of the paragraph holding the server binary path, which is the last one added
    fn server_binary_paragraph(page: &Page) -> usize {
        *page
            .essential_components
            .iter()
            .next()
            .expect("the main screen marks the server binary paragraph essential")
    }

    #[test]
    fn a_pane_that_fits_the_page_hides_nothing() {
        let page = main_screen();
        assert_eq!(page.ui_row_count_without(&HashSet::new()), 18);
        assert!(page.components_to_hide(18).is_empty());
        assert!(page.components_to_hide(40).is_empty());
    }

    #[test]
    fn a_short_pane_gives_up_the_whats_new_list_first() {
        let page = main_screen();
        let hidden = page.components_to_hide(17);
        assert_eq!(hidden.len(), 1, "one component is enough at 17 rows");
        assert!(!hidden.contains(&server_binary_paragraph(&page)));
        assert!(page.ui_row_count_without(&hidden) <= 17);
    }

    #[test]
    fn the_server_binary_survives_a_pane_too_short_for_anything_else() {
        let page = main_screen();
        for rows in 1..=17 {
            let hidden = page.components_to_hide(rows);
            assert!(
                !hidden.contains(&server_binary_paragraph(&page)),
                "the server binary paragraph was hidden at {} rows",
                rows
            );
        }
        // title, spacing and the two lines of the paragraph itself, and nothing else left to drop
        assert_eq!(page.ui_row_count_without(&page.components_to_hide(1)), 5);
    }

    #[test]
    fn a_page_without_a_server_binary_still_trims() {
        let page = main_screen_with(None);
        assert!(page.essential_components.is_empty());
        assert_eq!(page.ui_row_count_without(&HashSet::new()), 15);
        // the list alone buys 9 rows; below that the two remaining paragraphs go too
        assert_eq!(page.components_to_hide(6).len(), 1);
        assert_eq!(page.components_to_hide(3).len(), 3);
    }

    #[test]
    fn a_stable_path_is_a_second_labelled_line() {
        let page = main_screen_with(Some(server_binary(Some(STABLE_EXE))));
        let rendered = format!("{:?}", page.components_to_render);
        assert!(rendered.contains(SERVER_EXE), "the running binary is shown");
        assert!(rendered.contains(STABLE_EXE), "the stable path is shown");
        // two labels and two paths, where one binary gets one label and one path
        assert_eq!(page.ui_row_count_without(&HashSet::new()), 20);
    }

    #[test]
    fn both_paths_survive_a_pane_too_short_for_anything_else() {
        let page = main_screen_with(Some(server_binary(Some(STABLE_EXE))));
        let paragraph = server_binary_paragraph(&page);
        for rows in 1..=19 {
            assert!(
                !page.components_to_hide(rows).contains(&paragraph),
                "the server binary paragraph was hidden at {} rows",
                rows
            );
        }
        // title, spacing and the four lines of the paragraph itself, and nothing else left to drop
        assert_eq!(page.ui_row_count_without(&page.components_to_hide(1)), 7);
    }

    #[test]
    fn the_stable_path_is_the_one_copied() {
        assert_eq!(server_binary(Some(STABLE_EXE)).path_to_copy(), STABLE_EXE);
        assert_eq!(server_binary(None).path_to_copy(), SERVER_EXE);
    }

    #[test]
    fn the_copy_hint_is_shown_only_with_a_path_to_copy() {
        let with_path = format!("{:?}", main_screen_help_text(false, false, true));
        let without_path = format!("{:?}", main_screen_help_text(false, false, false));
        assert!(with_path.contains("Copy Path"));
        assert!(!without_path.contains("Copy Path"));
    }

    #[test]
    fn the_hint_is_the_only_difference_between_hosts() {
        let with_hint = server_binary_label(true);
        let without_hint = server_binary_label(false);
        assert!(format!("{:?}", with_hint).contains("Full Disk Access"));
        assert!(!format!("{:?}", without_hint).contains("Full Disk Access"));
    }
}
