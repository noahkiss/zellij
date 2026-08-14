use crate::panes::PaneId;
use crate::ClientId;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use zellij_utils::common_path::common_path_all;
use zellij_utils::pane_size::{PaneGeom, Size};
use zellij_utils::{
    data::{ClientInfo, LayoutMetadata, PaneMetadata, TabMetadata},
    input::command::RunCommand,
    input::layout::{Layout, Run, RunPlugin, RunPluginOrAlias},
    input::plugins::PluginAliases,
    resurrect_command_hints::ResurrectCommandHints,
    session_serialization::{
        extract_command_and_args, extract_edit_and_line_number, extract_plugin_and_config,
        GlobalLayoutManifest, PaneLayoutManifest, TabLayoutManifest,
    },
};

#[derive(Default, Debug, Clone)]
pub struct SessionLayoutMetadata {
    default_layout: Box<Layout>,
    global_cwd: Option<PathBuf>,
    pub default_shell: Option<PathBuf>,
    pub default_editor: Option<PathBuf>,
    tabs: Vec<TabLayoutMetadata>,
    /// Terminal size per attached client, as `Screen` last recorded it. Carried here because the
    /// client list is assembled in the plugin thread, which has no view of `Screen`'s state.
    client_sizes: BTreeMap<ClientId, Size>,
    /// Terminal device per attached client, same reason as `client_sizes`: assembled in the
    /// plugin thread, which has no view of `Screen`'s state.
    client_ttys: BTreeMap<ClientId, String>,
}

impl SessionLayoutMetadata {
    pub fn new(default_layout: Box<Layout>) -> Self {
        SessionLayoutMetadata {
            default_layout,
            ..Default::default()
        }
    }
    pub fn update_default_shell(&mut self, default_shell: PathBuf) {
        if self.default_shell.is_none() {
            self.default_shell = Some(default_shell);
        }
        for tab in self.tabs.iter_mut() {
            for tiled_pane in tab.tiled_panes.iter_mut() {
                if let Some(Run::Command(run_command)) = tiled_pane.run.as_mut() {
                    if Self::is_default_shell(
                        self.default_shell.as_ref(),
                        &run_command.command.display().to_string(),
                        &run_command.args,
                    ) {
                        tiled_pane.run = None;
                    }
                }
            }
            for floating_pane in tab.floating_panes.iter_mut() {
                if let Some(Run::Command(run_command)) = floating_pane.run.as_mut() {
                    if Self::is_default_shell(
                        self.default_shell.as_ref(),
                        &run_command.command.display().to_string(),
                        &run_command.args,
                    ) {
                        floating_pane.run = None;
                    }
                }
            }
        }
    }
    pub fn list_clients_metadata(&self) -> String {
        ClientMetadata::render_many(self.all_clients_metadata(), &self.default_editor)
    }
    /// The same client list as `list_clients_metadata`, as the `ClientInfo` array plugins already
    /// receive in `Event::ListClients`.
    ///
    /// `requesting_client_id` marks `is_current_client`. A `zellij action` client is its own
    /// short-lived client that is focused on no pane, so a CLI query marks no row.
    pub fn list_clients_metadata_json(&self, requesting_client_id: ClientId) -> String {
        let clients = self.client_infos(requesting_client_id);
        serde_json::to_string_pretty(&clients).unwrap_or_else(|_| "[]".to_string())
    }
    /// Every client this layout knows about, in the shape `Event::ListClients` uses.
    pub fn client_infos(&self, requesting_client_id: ClientId) -> Vec<ClientInfo> {
        self.all_clients_metadata()
            .into_iter()
            .map(|(client_id, client_metadata)| {
                ClientInfo::new(
                    client_id,
                    client_metadata.get_pane_id().into(),
                    client_metadata.stringify_command(&self.default_editor),
                    client_id == requesting_client_id,
                )
                .with_terminal_size(client_metadata.terminal_size())
                .with_tty(client_metadata.tty())
            })
            .collect()
    }
    /// Record the terminal size of every attached client, keyed by client id.
    pub fn set_client_sizes(&mut self, client_sizes: BTreeMap<ClientId, Size>) {
        self.client_sizes = client_sizes;
    }
    pub fn set_client_ttys(&mut self, client_ttys: BTreeMap<ClientId, String>) {
        self.client_ttys = client_ttys;
    }
    /// Every client this layout knows about, and the pane each one is focused on.
    ///
    /// Both pane layers are read, not just the one on screen: floating panes are visible per tab
    /// while focus is per client, so a client focused on a tiled pane in a tab where someone else
    /// has floating panes up is a client with no row at all if only one layer is read. The layer
    /// that is off screen goes in first, so a client that appears in both - its focus in the
    /// hidden layer being only a memory of where it will return - is described by the layer it is
    /// actually looking at.
    pub fn all_clients_metadata(&self) -> BTreeMap<ClientId, ClientMetadata> {
        let mut clients_metadata: BTreeMap<ClientId, ClientMetadata> = BTreeMap::new();
        for tab in &self.tabs {
            let (hidden_panes, visible_panes) = if tab.hide_floating_panes {
                (&tab.floating_panes, &tab.tiled_panes)
            } else {
                (&tab.tiled_panes, &tab.floating_panes)
            };
            for pane in hidden_panes.iter().chain(visible_panes.iter()) {
                for focused_client in &pane.focused_clients {
                    clients_metadata.insert(
                        *focused_client,
                        ClientMetadata {
                            pane_id: pane.id.clone(),
                            command: pane.run.clone(),
                            terminal_size: self.client_sizes.get(focused_client).copied(),
                            tty: self.client_ttys.get(focused_client).cloned(),
                        },
                    );
                }
            }
        }
        clients_metadata
    }
    pub fn is_dirty(&self) -> bool {
        // here we check to see if the serialized layout would be different than the base one, and
        // thus is "dirty". A layout is considered dirty if one of the following is true:
        // 1. The current number of panes is different than the number of panes in the base layout
        //    (meaning a pane was opened or closed)
        // 2. One or more terminal panes are running a command that is not the default shell
        // 3. The tabs no longer match the base layout's tabs - one was renamed, added, closed or
        //    MOVED
        let base_layout_pane_count = self.default_layout.pane_count();
        let current_pane_count = self.pane_count();
        if current_pane_count != base_layout_pane_count {
            return true;
        }
        if self.tabs_diverge_from_base_layout() {
            return true;
        }
        for tab in &self.tabs {
            for tiled_pane in &tab.tiled_panes {
                match tiled_pane.run.as_ref() {
                    Some(Run::Command(run_command)) => {
                        if !Self::is_default_shell(
                            self.default_shell.as_ref(),
                            &run_command.command.display().to_string(),
                            &run_command.args,
                        ) {
                            return true;
                        }
                    },
                    Some(Run::EditFile(_, _, _)) => return true,
                    _ => {},
                }
            }
            for floating_pane in &tab.floating_panes {
                match floating_pane.run.as_ref() {
                    Some(Run::Command(run_command)) => {
                        if !Self::is_default_shell(
                            self.default_shell.as_ref(),
                            &run_command.command.display().to_string(),
                            &run_command.args,
                        ) {
                            return true;
                        }
                    },
                    Some(Run::EditFile(_, _, _)) => return true,
                    _ => {},
                }
            }
        }
        false
    }
    /// Whether the tab list differs from the one the base layout describes - in count, in name, or
    /// in ORDER.
    ///
    /// Moving a tab changes none of the things `is_dirty` used to look at: the pane count is the
    /// same and so are the commands. A session that is otherwise clean therefore never writes its
    /// layout again after a move, the copy on disk keeps the pre-move order, and the next restart
    /// that resurrects from that copy hands the tab back where it started - silently, and every
    /// time, since nothing ever marks it dirty.
    ///
    /// A base layout with no tabs of its own says nothing about the tabs a session grew, so it is
    /// not compared. An unnamed base tab matches any name for the same reason: the layout did not
    /// ask for one, so a session that named it itself has not diverged. The cost of that wildcard
    /// is that a move between two tabs the layout left unnamed is not seen; the alternative -
    /// comparing against the default `Tab #n` names - calls every such session dirty forever.
    ///
    /// Note the check is only reached today when the base layout defines no tabs of its own beyond
    /// its template: `Layout::pane_count` adds the template's panes to the tabs' panes, so any
    /// layout parsed from KDL with explicit tabs already fails the pane count comparison above and
    /// `is_dirty` returns before this runs. This is the check the tab comparison would need, the
    /// moment that count is made to mean what it says.
    fn tabs_diverge_from_base_layout(&self) -> bool {
        let base_tabs = &self.default_layout.tabs;
        if base_tabs.is_empty() {
            return false;
        }
        if base_tabs.len() != self.tabs.len() {
            return true;
        }
        base_tabs
            .iter()
            .zip(self.tabs.iter())
            .any(|((base_name, _, _), tab)| match (base_name, &tab.name) {
                (Some(base_name), Some(name)) => base_name != name,
                (Some(_), None) => true,
                (None, _) => false,
            })
    }
    fn pane_count(&self) -> usize {
        let mut pane_count = 0;
        for tab in &self.tabs {
            for tiled_pane in &tab.tiled_panes {
                if !self.should_exclude_from_count(tiled_pane) {
                    pane_count += 1;
                }
            }
            for floating_pane in &tab.floating_panes {
                if !self.should_exclude_from_count(floating_pane) {
                    pane_count += 1;
                }
            }
        }
        pane_count
    }
    fn should_exclude_from_count(&self, pane: &PaneLayoutMetadata) -> bool {
        if let Some(Run::Plugin(run_plugin)) = &pane.run {
            let location_string = run_plugin.location_string();
            if location_string == "zellij:about" {
                return true;
            }
            if location_string == "zellij:session-manager" {
                return true;
            }
            if location_string == "zellij:plugin-manager" {
                return true;
            }
            if location_string == "zellij:configuration-manager" {
                return true;
            }
            if location_string == "zellij:share" {
                return true;
            }
        }
        false
    }
    fn is_default_shell(
        default_shell: Option<&PathBuf>,
        command_name: &String,
        args: &Vec<String>,
    ) -> bool {
        default_shell
            .as_ref()
            .map(|c| c.display().to_string())
            .as_ref()
            == Some(command_name)
            && args.is_empty()
    }
}

impl SessionLayoutMetadata {
    /// The tab names in the order they were added - which is the order a restored session gets
    /// its tabs back in.
    #[cfg(test)]
    pub fn tab_names(&self) -> Vec<String> {
        self.tabs
            .iter()
            .map(|tab| tab.name.clone().unwrap_or_default())
            .collect()
    }
    pub fn add_tab(
        &mut self,
        name: String,
        is_focused: bool,
        hide_floating_panes: bool,
        tiled_panes: Vec<PaneLayoutMetadata>,
        floating_panes: Vec<PaneLayoutMetadata>,
    ) {
        self.tabs.push(TabLayoutMetadata {
            name: Some(name),
            is_focused,
            hide_floating_panes,
            tiled_panes,
            floating_panes,
        })
    }
    pub fn all_terminal_ids(&self) -> Vec<u32> {
        let mut terminal_ids = vec![];
        for tab in &self.tabs {
            for pane_layout_metadata in &tab.tiled_panes {
                if let PaneId::Terminal(id) = pane_layout_metadata.id {
                    terminal_ids.push(id);
                }
            }
            for pane_layout_metadata in &tab.floating_panes {
                if let PaneId::Terminal(id) = pane_layout_metadata.id {
                    terminal_ids.push(id);
                }
            }
        }
        terminal_ids
    }
    pub fn all_plugin_ids(&self) -> Vec<u32> {
        let mut plugin_ids = vec![];
        for tab in &self.tabs {
            for pane_layout_metadata in &tab.tiled_panes {
                if let PaneId::Plugin(id) = pane_layout_metadata.id {
                    plugin_ids.push(id);
                }
            }
            for pane_layout_metadata in &tab.floating_panes {
                if let PaneId::Plugin(id) = pane_layout_metadata.id {
                    plugin_ids.push(id);
                }
            }
        }
        plugin_ids
    }
    pub fn remove_plugin_from_layout(&mut self, plugin_id_to_remove: u32) {
        for tab in &mut self.tabs {
            // Filter tiled panes
            tab.tiled_panes.retain(|pane| {
                if let PaneId::Plugin(id) = pane.id {
                    id != plugin_id_to_remove
                } else {
                    true
                }
            });

            // Filter floating panes
            tab.floating_panes.retain(|pane| {
                if let PaneId::Plugin(id) = pane.id {
                    id != plugin_id_to_remove
                } else {
                    true
                }
            });
        }
    }
    pub fn update_terminal_commands(
        &mut self,
        mut terminal_ids_to_commands: HashMap<u32, Vec<String>>,
    ) {
        let mut update_cmd_in_pane_metadata = |pane_layout_metadata: &mut PaneLayoutMetadata| {
            if let PaneId::Terminal(id) = pane_layout_metadata.id {
                if let Some(command) = terminal_ids_to_commands.remove(&id) {
                    let mut command_line = command.iter();
                    if let Some(command_name) = command_line.next() {
                        let args: Vec<String> = command_line.map(|c| c.to_owned()).collect();
                        if Self::is_default_shell(self.default_shell.as_ref(), &command_name, &args)
                        {
                            pane_layout_metadata.run = None;
                        } else {
                            let mut run_command = RunCommand::new(PathBuf::from(command_name));
                            run_command.args = args;
                            pane_layout_metadata.run = Some(Run::Command(run_command));
                        }
                    }
                }
            }
        };
        for tab in self.tabs.iter_mut() {
            for pane_layout_metadata in tab.tiled_panes.iter_mut() {
                update_cmd_in_pane_metadata(pane_layout_metadata);
            }
            for pane_layout_metadata in tab.floating_panes.iter_mut() {
                update_cmd_in_pane_metadata(pane_layout_metadata);
            }
        }
    }
    pub fn update_terminal_cwds(&mut self, mut terminal_ids_to_cwds: HashMap<u32, PathBuf>) {
        if let Some(common_path_between_cwds) =
            common_path_all(terminal_ids_to_cwds.values().map(|p| p.as_path()))
        {
            terminal_ids_to_cwds.values_mut().for_each(|p| {
                if let Ok(stripped) = p.strip_prefix(&common_path_between_cwds) {
                    *p = PathBuf::from(stripped)
                }
            });
            self.global_cwd = Some(PathBuf::from(common_path_between_cwds));
        }
        let mut update_cwd_in_pane_metadata = |pane_layout_metadata: &mut PaneLayoutMetadata| {
            if let PaneId::Terminal(id) = pane_layout_metadata.id {
                if let Some(cwd) = terminal_ids_to_cwds.remove(&id) {
                    pane_layout_metadata.cwd = Some(cwd);
                }
            }
        };
        for tab in self.tabs.iter_mut() {
            for pane_layout_metadata in tab.tiled_panes.iter_mut() {
                update_cwd_in_pane_metadata(pane_layout_metadata);
            }
            for pane_layout_metadata in tab.floating_panes.iter_mut() {
                update_cwd_in_pane_metadata(pane_layout_metadata);
            }
        }
    }
    pub fn update_plugin_cmds(&mut self, mut plugin_ids_to_run_plugins: HashMap<u32, RunPlugin>) {
        let mut update_cmd_in_pane_metadata = |pane_layout_metadata: &mut PaneLayoutMetadata| {
            if let PaneId::Plugin(id) = pane_layout_metadata.id {
                if let Some(run_plugin) = plugin_ids_to_run_plugins.remove(&id) {
                    pane_layout_metadata.run =
                        Some(Run::Plugin(RunPluginOrAlias::RunPlugin(run_plugin)));
                }
            }
        };
        for tab in self.tabs.iter_mut() {
            for pane_layout_metadata in tab.tiled_panes.iter_mut() {
                update_cmd_in_pane_metadata(pane_layout_metadata);
            }
            for pane_layout_metadata in tab.floating_panes.iter_mut() {
                update_cmd_in_pane_metadata(pane_layout_metadata);
            }
        }
    }
    pub fn update_default_editor(&mut self, default_editor: &Option<PathBuf>) {
        let default_editor = default_editor.clone().unwrap_or_else(|| {
            PathBuf::from(
                std::env::var("EDITOR")
                    .unwrap_or_else(|_| std::env::var("VISUAL").unwrap_or_else(|_| "vi".into())),
            )
        });
        self.default_editor = Some(default_editor);
    }
    pub fn detect_editor_panes(&mut self) {
        let default_editor = match &self.default_editor {
            Some(e) => e.clone(),
            None => return,
        };
        let editor_binary_name = default_editor
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let is_vim_family = |name: &str| matches!(name, "vim" | "nvim" | "emacs" | "nano" | "kak");
        let is_helix = |name: &str| matches!(name, "hx" | "helix");
        // Narrow vi/vim lineage used for cross-matching.
        // These are argument-compatible and commonly aliased to one another.
        let is_vi_vim = |name: &str| matches!(name, "vi" | "vim" | "nvim");

        let configured_is_vi_vim = is_vi_vim(&editor_binary_name);
        let configured_is_helix = is_helix(&editor_binary_name);

        let upgrade_pane = |pane: &mut PaneLayoutMetadata| {
            if let Some(Run::Command(run_command)) = &pane.run {
                let command_binary_name = run_command
                    .command
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string();
                let is_editor = !editor_binary_name.is_empty()
                    && (command_binary_name == editor_binary_name
                        || run_command.command == default_editor
                        || (configured_is_vi_vim && is_vi_vim(&command_binary_name))
                        || (configured_is_helix && is_helix(&command_binary_name)));
                if !is_editor {
                    return;
                }

                let args = &run_command.args;
                let binary = &command_binary_name;

                let edit_file: Option<(PathBuf, Option<usize>)> = if is_vim_family(binary) {
                    match args.len() {
                        1 => args
                            .first()
                            .filter(|f| !f.starts_with('-'))
                            .map(|f| (PathBuf::from(f), None)),
                        2 => match (args.first(), args.get(1)) {
                            (Some(line_arg), Some(file)) => line_arg
                                .strip_prefix('+')
                                .and_then(|n| n.parse::<usize>().ok())
                                .map(|line| (PathBuf::from(file), Some(line))),
                            _ => None,
                        },
                        _ => None,
                    }
                } else if is_helix(binary) {
                    if args.len() == 1 {
                        args.first().and_then(|arg| {
                            if let Some(colon_pos) = arg.rfind(':') {
                                let file_part = &arg[..colon_pos];
                                if let Some(line) = arg
                                    .get(colon_pos + 1..)
                                    .and_then(|s| s.parse::<usize>().ok())
                                {
                                    return Some((PathBuf::from(file_part), Some(line)));
                                }
                            }
                            Some((PathBuf::from(arg.as_str()), None))
                        })
                    } else {
                        None
                    }
                } else {
                    if args.len() == 1 {
                        args.first()
                            .filter(|f| !f.starts_with('-'))
                            .map(|f| (PathBuf::from(f), None))
                    } else {
                        None
                    }
                };

                if let Some((file_path, line_number)) = edit_file {
                    pane.run = Some(Run::EditFile(file_path, line_number, None));
                }
            }
        };

        for tab in self.tabs.iter_mut() {
            for pane in tab.tiled_panes.iter_mut() {
                upgrade_pane(pane);
            }
            for pane in tab.floating_panes.iter_mut() {
                upgrade_pane(pane);
            }
        }
    }
    /// Appends resume arguments to the recorded command of any pane a `resurrect_command_hints`
    /// entry applies to, so that the resurrected pane offers to resume the tool's session instead
    /// of starting a new one.
    ///
    /// The observed command line is kept whole - path, arguments and all. A hint only adds, so a
    /// resurrected pane always offers a command the pane really ran.
    ///
    /// `read_env` is the seam over the platform: it is handed a terminal id and a variable name and
    /// returns the value found in that pane's processes. Everything the hints decide is here;
    /// everything about processes is on the other side of that closure.
    ///
    /// A pane is left exactly as it was whenever anything does not line up - no hint matches, the
    /// variable is not set, the observed arguments already resume. There is no failure mode: a hint
    /// that does not resolve gives the same snapshot the feature would have produced by not
    /// existing.
    pub fn apply_resurrect_command_hints<F>(
        &mut self,
        hints: &ResurrectCommandHints,
        mut read_env: F,
    ) where
        F: FnMut(u32, &str) -> Option<String>,
    {
        if hints.is_empty() {
            return;
        }
        for tab in self.tabs.iter_mut() {
            for pane in tab
                .tiled_panes
                .iter_mut()
                .chain(tab.floating_panes.iter_mut())
            {
                let PaneId::Terminal(terminal_id) = pane.id else {
                    continue;
                };
                let Some(Run::Command(run_command)) = pane.run.as_ref() else {
                    continue;
                };
                let Some(hint) = hints.hint_for(&run_command.command.display().to_string()) else {
                    continue;
                };
                let Some(env_value) = read_env(terminal_id, &hint.env) else {
                    continue;
                };
                let Some(extra_args) = hint.resume_args_for(&run_command.args, &env_value) else {
                    log::debug!(
                        "resurrect_command_hints {:?}: resume_args {:?} add nothing to the observed \
                         command of terminal {}",
                        hint.name,
                        hint.resume_args,
                        terminal_id
                    );
                    continue;
                };
                let mut rewritten = run_command.clone();
                rewritten.args.extend(extra_args);
                log::debug!(
                    "resurrect_command_hints {:?}: recording {:?} {:?} for terminal {}",
                    hint.name,
                    rewritten.command,
                    rewritten.args,
                    terminal_id
                );
                pane.run = Some(Run::Command(rewritten));
            }
        }
    }
    pub fn update_plugin_aliases_in_default_layout(&mut self, plugin_aliases: &PluginAliases) {
        self.default_layout
            .populate_plugin_aliases_in_layout(&plugin_aliases);
    }
    pub fn to_layout_metadata(&self) -> LayoutMetadata {
        // Get current timestamp for both creation and update time
        let current_time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs().to_string())
            .unwrap_or_default();

        // Convert all tabs
        let tabs = self.tabs.iter().map(|tab| tab.to_tab_metadata()).collect();

        LayoutMetadata {
            tabs,
            creation_time: current_time.clone(),
            update_time: current_time,
        }
    }
}

impl Into<GlobalLayoutManifest> for SessionLayoutMetadata {
    fn into(self) -> GlobalLayoutManifest {
        GlobalLayoutManifest {
            default_layout: self.default_layout,
            default_shell: self.default_shell,
            global_cwd: self.global_cwd,
            tabs: self
                .tabs
                .into_iter()
                .map(|t| (t.name.clone().unwrap_or_default(), t.into()))
                .collect(),
        }
    }
}

impl Into<TabLayoutManifest> for TabLayoutMetadata {
    fn into(self) -> TabLayoutManifest {
        TabLayoutManifest {
            tiled_panes: self.tiled_panes.into_iter().map(|t| t.into()).collect(),
            floating_panes: self.floating_panes.into_iter().map(|t| t.into()).collect(),
            is_focused: self.is_focused,
            hide_floating_panes: self.hide_floating_panes,
        }
    }
}

impl TabLayoutMetadata {
    fn to_tab_metadata(&self) -> TabMetadata {
        let mut panes = Vec::new();

        // Extract pane metadata from tiled panes
        for pane in &self.tiled_panes {
            panes.push(pane.to_pane_metadata());
        }

        // Extract pane metadata from floating panes
        for pane in &self.floating_panes {
            panes.push(pane.to_pane_metadata());
        }

        TabMetadata {
            panes,
            name: self.name.clone(),
        }
    }
}

impl Into<PaneLayoutManifest> for PaneLayoutMetadata {
    fn into(self) -> PaneLayoutManifest {
        PaneLayoutManifest {
            geom: self.geom,
            run: self.run,
            cwd: self.cwd,
            is_borderless: self.is_borderless,
            title: self.title,
            is_focused: self.is_focused,
            pane_contents: self.pane_contents,
            default_fg: self.default_fg,
            default_bg: self.default_bg,
            pane_uuid: self.pane_uuid,
        }
    }
}

#[derive(Default, Debug, Clone)]
pub struct TabLayoutMetadata {
    name: Option<String>,
    tiled_panes: Vec<PaneLayoutMetadata>,
    floating_panes: Vec<PaneLayoutMetadata>,
    is_focused: bool,
    hide_floating_panes: bool,
}

#[derive(Debug, Clone)]
pub struct PaneLayoutMetadata {
    id: PaneId,
    geom: PaneGeom,
    run: Option<Run>,
    cwd: Option<PathBuf>,
    is_borderless: bool,
    title: Option<String>,
    is_focused: bool,
    pane_contents: Option<String>,
    focused_clients: Vec<ClientId>,
    default_fg: Option<String>,
    default_bg: Option<String>,
    /// This pane's uuid, so a pane restored from the serialized layout can name what it continues.
    pane_uuid: Option<String>,
}

impl PaneLayoutMetadata {
    pub fn new(
        id: PaneId,
        geom: PaneGeom,
        is_borderless: bool,
        run: Option<Run>,
        title: Option<String>,
        is_focused: bool,
        pane_contents: Option<String>,
        focused_clients: Vec<ClientId>,
        default_fg: Option<String>,
        default_bg: Option<String>,
        pane_uuid: Option<String>,
    ) -> Self {
        PaneLayoutMetadata {
            id,
            geom,
            run,
            cwd: None,
            is_borderless,
            title,
            is_focused,
            pane_contents,
            focused_clients,
            default_fg,
            default_bg,
            pane_uuid,
        }
    }
    fn to_pane_metadata(&self) -> PaneMetadata {
        // Try to extract a meaningful name from the pane
        // Priority: explicit title > command name > file name > plugin location
        let name = self.title.clone().or_else(|| {
            self.run.as_ref().and_then(|run| match run {
                Run::Command(cmd) => Some(cmd.command.display().to_string()),
                Run::EditFile(path, _, _) => {
                    path.file_name().map(|n| n.to_string_lossy().to_string())
                },
                Run::Plugin(plugin) => Some(plugin.location_string()),
                Run::Cwd(_) => None,
            })
        });

        let is_plugin = matches!(self.id, PaneId::Plugin(_));

        // Detect if this is a builtin plugin
        let is_builtin_plugin = self
            .run
            .as_ref()
            .map(|run| match run {
                Run::Plugin(plugin) => plugin.is_builtin_plugin(),
                _ => false,
            })
            .unwrap_or(false);

        PaneMetadata {
            name,
            is_plugin,
            is_builtin_plugin,
        }
    }
}

pub struct ClientMetadata {
    pane_id: PaneId,
    command: Option<Run>,
    terminal_size: Option<Size>,
    tty: Option<String>,
}
impl ClientMetadata {
    pub fn terminal_size(&self) -> Option<Size> {
        self.terminal_size
    }
    pub fn tty(&self) -> Option<String> {
        self.tty.clone()
    }
    pub fn stringify_pane_id(&self) -> String {
        match self.pane_id {
            PaneId::Terminal(terminal_id) => format!("terminal_{}", terminal_id),
            PaneId::Plugin(plugin_id) => format!("plugin_{}", plugin_id),
        }
    }
    pub fn stringify_command(&self, editor: &Option<PathBuf>) -> String {
        let stringified = match &self.command {
            Some(Run::Command(..)) => {
                let (command, args) = extract_command_and_args(&self.command);
                command.map(|c| format!("{} {}", c, args.join(" ")))
            },
            Some(Run::EditFile(..)) => {
                let (file_to_edit, _line_number) = extract_edit_and_line_number(&self.command);
                editor.as_ref().and_then(|editor| {
                    file_to_edit
                        .map(|file_to_edit| format!("{} {}", editor.display(), file_to_edit))
                })
            },
            Some(Run::Plugin(..)) => {
                let (plugin, _plugin_config) = extract_plugin_and_config(&self.command);
                plugin.map(|p| format!("{}", p))
            },
            _ => None,
        };
        stringified.unwrap_or("N/A".to_owned())
    }
    pub fn get_pane_id(&self) -> PaneId {
        self.pane_id
    }
    pub fn render_many(
        clients_metadata: BTreeMap<ClientId, ClientMetadata>,
        default_editor: &Option<PathBuf>,
    ) -> String {
        let mut lines = vec![];
        lines.push(String::from("CLIENT_ID ZELLIJ_PANE_ID RUNNING_COMMAND"));

        for (client_id, client_metadata) in clients_metadata.iter() {
            // 9 - CLIENT_ID, 14 - ZELLIJ_PANE_ID, 15 - RUNNING_COMMAND
            lines.push(format!(
                "{} {} {}",
                format!("{0: <9}", client_id),
                format!("{0: <14}", client_metadata.stringify_pane_id()),
                format!(
                    "{0: <15}",
                    client_metadata.stringify_command(default_editor)
                )
            ));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::PaneId as ZellijPaneId;
    use zellij_utils::input::layout::TiledPaneLayout;
    use zellij_utils::pane_size::PaneGeom;

    fn make_command_pane(terminal_id: u32, command: &str, args: Vec<&str>) -> PaneLayoutMetadata {
        let mut run_command = RunCommand::new(PathBuf::from(command));
        run_command.args = args.into_iter().map(|s| s.to_string()).collect();
        PaneLayoutMetadata::new(
            PaneId::Terminal(terminal_id),
            PaneGeom::default(),
            false,
            Some(Run::Command(run_command)),
            None,
            false,
            None,
            vec![],
            None,
            None,
            None,
        )
    }

    fn make_plain_pane(terminal_id: u32) -> PaneLayoutMetadata {
        PaneLayoutMetadata::new(
            PaneId::Terminal(terminal_id),
            PaneGeom::default(),
            false,
            None,
            None,
            false,
            None,
            vec![],
            None,
            None,
            None,
        )
    }

    /// A session in exactly the shape of the layout it was built from: one plain pane per tab, the
    /// tabs named and ordered as the layout names and orders them. Nothing about it is dirty.
    fn session_matching_layout(tab_names: &[&str]) -> SessionLayoutMetadata {
        let mut layout = Layout::default();
        layout.tabs = tab_names
            .iter()
            .map(|name| (Some(name.to_string()), TiledPaneLayout::default(), vec![]))
            .collect();
        let mut session_layout_metadata = SessionLayoutMetadata::new(Box::new(layout));
        for (i, name) in tab_names.iter().enumerate() {
            session_layout_metadata.add_tab(
                name.to_string(),
                i == 0,
                false,
                vec![make_plain_pane(i as u32)],
                vec![],
            );
        }
        session_layout_metadata
    }

    fn make_edit_file_pane(
        terminal_id: u32,
        path: &str,
        line_number: Option<usize>,
    ) -> PaneLayoutMetadata {
        PaneLayoutMetadata::new(
            PaneId::Terminal(terminal_id),
            PaneGeom::default(),
            false,
            Some(Run::EditFile(PathBuf::from(path), line_number, None)),
            None,
            false,
            None,
            vec![],
            None,
            None,
            None,
        )
    }

    fn session_with_editor(editor: &str, panes: Vec<PaneLayoutMetadata>) -> SessionLayoutMetadata {
        let mut meta = SessionLayoutMetadata::default();
        meta.default_editor = Some(PathBuf::from(editor));
        meta.add_tab("tab1".to_string(), true, false, panes, vec![]);
        meta
    }

    fn get_first_tiled_run(meta: &SessionLayoutMetadata) -> Option<&Run> {
        meta.tabs[0].tiled_panes[0].run.as_ref()
    }

    #[test]
    fn detects_editor_pane_no_line_number() {
        let pane = make_command_pane(1, "nvim", vec!["file.txt"]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    #[test]
    fn detects_editor_pane_with_line_number_vim_family() {
        let pane = make_command_pane(1, "nvim", vec!["+50", "file.txt"]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), Some(50), None))
        );
    }

    #[test]
    fn detects_editor_pane_with_line_number_helix() {
        let pane = make_command_pane(1, "hx", vec!["file.txt:50"]);
        let mut meta = session_with_editor("hx", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), Some(50), None))
        );
    }

    #[test]
    fn detects_editor_pane_helix_no_line_number() {
        let pane = make_command_pane(1, "helix", vec!["file.txt"]);
        let mut meta = session_with_editor("helix", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    #[test]
    fn skips_non_editor_command() {
        let pane = make_command_pane(1, "grep", vec!["pattern", "file.txt"]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        // Run::Command(grep ...) unchanged
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("grep"));
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn skips_editor_with_no_args() {
        let pane = make_command_pane(1, "nvim", vec![]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("nvim"));
                assert!(rc.args.is_empty());
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn skips_editor_with_multiple_files() {
        let pane = make_command_pane(1, "nvim", vec!["a.txt", "b.txt"]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("nvim"));
                assert_eq!(rc.args, vec!["a.txt", "b.txt"]);
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn detects_editor_matching_by_binary_name() {
        // configured as /usr/bin/nvim, running as nvim
        let pane = make_command_pane(1, "nvim", vec!["file.txt"]);
        let mut meta = session_with_editor("/usr/bin/nvim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    #[test]
    fn does_not_affect_existing_edit_file_run() {
        let pane = make_edit_file_pane(1, "file.txt", Some(10));
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), Some(10), None))
        );
    }

    #[test]
    fn detects_vi_when_editor_is_vim() {
        // configured as vim, pane running vi (common alias)
        let pane = make_command_pane(1, "vi", vec!["file.txt"]);
        let mut meta = session_with_editor("vim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    #[test]
    fn detects_nvim_when_editor_is_vi() {
        // configured as vi, pane running nvim
        let pane = make_command_pane(1, "nvim", vec!["file.txt"]);
        let mut meta = session_with_editor("vi", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    #[test]
    fn does_not_cross_match_vim_with_emacs() {
        // configured as emacs, pane running vim — NOT cross-matched
        let pane = make_command_pane(1, "vim", vec!["file.txt"]);
        let mut meta = session_with_editor("emacs", vec![pane]);
        meta.detect_editor_panes();
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => assert_eq!(rc.command, PathBuf::from("vim")),
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn detects_hx_when_editor_is_helix() {
        let pane = make_command_pane(1, "hx", vec!["file.txt"]);
        let mut meta = session_with_editor("helix", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(PathBuf::from("file.txt"), None, None))
        );
    }

    fn hints(entries: &[(&str, &str, &str)]) -> ResurrectCommandHints {
        let mut hints = ResurrectCommandHints::default();
        for (match_command, env, resume_args) in entries {
            hints.push(
                zellij_utils::resurrect_command_hints::ResurrectCommandHint {
                    name: match_command.to_string(),
                    match_command: match_command.to_string(),
                    env: env.to_string(),
                    resume_args: resume_args.to_string(),
                },
            );
        }
        hints
    }

    fn session_with_panes(panes: Vec<PaneLayoutMetadata>) -> SessionLayoutMetadata {
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, panes, vec![]);
        meta
    }

    /// The env-reading seam every test below stands in for: a fixed answer, so that what is
    /// exercised is the decision and not the platform.
    fn env_always(value: Option<&str>) -> impl FnMut(u32, &str) -> Option<String> {
        let value = value.map(|v| v.to_string());
        move |_terminal_id, _var| value.clone()
    }

    #[test]
    fn appends_resume_args_when_the_variable_is_found() {
        let mut meta = session_with_panes(vec![make_command_pane(1, "claude", vec![])]);
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(Some("abc-123")),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("claude"));
                assert_eq!(rc.args, vec!["--continue".to_owned()]);
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    /// The regression: a hint used to REPLACE the command line, so the recorded command lost the
    /// path and every flag the pane really ran. What comes back must be the observed argv plus the
    /// resume flag, and nothing else.
    #[test]
    fn keeps_the_observed_path_and_arguments() {
        let mut meta = session_with_panes(vec![make_command_pane(
            1,
            "/opt/homebrew/bin/claude",
            vec!["--dangerously-skip-permissions"],
        )]);
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(Some("abc-123")),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("/opt/homebrew/bin/claude"));
                assert_eq!(
                    rc.args,
                    vec![
                        "--dangerously-skip-permissions".to_owned(),
                        "--continue".to_owned()
                    ]
                );
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    /// A pane that already ran a resume flag is recorded exactly as it ran. The environment value
    /// is a detector here, and must not become a second, contradictory resume argument.
    #[test]
    fn observed_arguments_that_already_resume_are_left_alone() {
        let mut meta = session_with_panes(vec![make_command_pane(
            1,
            "claude",
            vec!["--dangerously-skip-permissions", "--continue"],
        )]);
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(Some("an-internal-session-id")),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("claude"));
                assert_eq!(
                    rc.args,
                    vec![
                        "--dangerously-skip-permissions".to_owned(),
                        "--continue".to_owned()
                    ]
                );
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn leaves_the_command_alone_when_the_variable_is_missing() {
        let mut meta = session_with_panes(vec![make_command_pane(1, "claude", vec!["--verbose"])]);
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(None),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("claude"));
                assert_eq!(rc.args, vec!["--verbose".to_owned()]);
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn leaves_an_unmatched_command_alone_and_never_reads_its_environment() {
        let mut meta = session_with_panes(vec![make_command_pane(1, "htop", vec![])]);
        let mut reads = 0;
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            |_terminal_id, _var| {
                reads += 1;
                Some("abc-123".to_owned())
            },
        );
        assert_eq!(reads, 0, "an unmatched pane must cost no process lookup");
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => assert_eq!(rc.command, PathBuf::from("htop")),
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn expands_a_placeholder_that_sits_in_an_argument() {
        let mut meta = session_with_panes(vec![make_command_pane(1, "opencode", vec![])]);
        meta.apply_resurrect_command_hints(
            &hints(&[("opencode", "OPENCODE_SESSION_ID", "--session={} --continue")]),
            env_always(Some("xyz")),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("opencode"));
                assert_eq!(
                    rc.args,
                    vec!["--session=xyz".to_owned(), "--continue".to_owned()]
                );
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn no_hints_configured_changes_nothing() {
        let mut meta = session_with_panes(vec![make_command_pane(1, "claude", vec![])]);
        meta.apply_resurrect_command_hints(
            &ResurrectCommandHints::default(),
            env_always(Some("abc-123")),
        );
        match get_first_tiled_run(&meta) {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.command, PathBuf::from("claude"));
                assert!(rc.args.is_empty());
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn a_pane_running_no_command_is_untouched() {
        let mut pane = make_command_pane(1, "claude", vec![]);
        pane.run = None;
        let mut meta = session_with_panes(vec![pane]);
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(Some("abc-123")),
        );
        assert_eq!(get_first_tiled_run(&meta), None);
    }

    #[test]
    fn floating_panes_get_resume_args_too() {
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab(
            "tab1".to_string(),
            true,
            false,
            vec![],
            vec![make_command_pane(1, "claude", vec![])],
        );
        meta.apply_resurrect_command_hints(
            &hints(&[("claude", "CLAUDE_CODE_SESSION_ID", "--continue")]),
            env_always(Some("abc-123")),
        );
        match meta.tabs[0].floating_panes[0].run.as_ref() {
            Some(Run::Command(rc)) => {
                assert_eq!(rc.args, vec!["--continue".to_owned()])
            },
            other => panic!("expected Command, got {:?}", other),
        }
    }

    #[test]
    fn absolute_file_path_preserved() {
        let pane = make_command_pane(1, "nvim", vec!["/home/user/file.txt"]);
        let mut meta = session_with_editor("nvim", vec![pane]);
        meta.detect_editor_panes();
        assert_eq!(
            get_first_tiled_run(&meta),
            Some(&Run::EditFile(
                PathBuf::from("/home/user/file.txt"),
                None,
                None
            ))
        );
    }

    fn make_focused_pane(id: PaneId, focused_clients: Vec<ClientId>) -> PaneLayoutMetadata {
        PaneLayoutMetadata::new(
            id,
            PaneGeom::default(),
            false,
            None,
            None,
            !focused_clients.is_empty(),
            None,
            focused_clients,
            None,
            None,
            None,
        )
    }

    fn focused_pane_ids(meta: &SessionLayoutMetadata) -> Vec<(ClientId, PaneId)> {
        meta.all_clients_metadata()
            .into_iter()
            .map(|(client_id, client_metadata)| (client_id, client_metadata.get_pane_id()))
            .collect()
    }

    #[test]
    fn client_list_covers_both_pane_layers() {
        // client 2 is focused on a tiled pane while the tab shows floating panes: floating
        // visibility is per tab, focus is per client, so this is not a contradiction
        let tiled = make_focused_pane(PaneId::Terminal(1), vec![2]);
        let floating = make_focused_pane(PaneId::Plugin(3), vec![1]);
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, vec![tiled], vec![floating]);
        assert_eq!(
            focused_pane_ids(&meta),
            vec![(1, PaneId::Plugin(3)), (2, PaneId::Terminal(1))]
        );
    }

    #[test]
    fn client_list_describes_a_client_by_the_layer_it_sees() {
        // the same client is remembered by both layers - only the one on screen is where it is
        let tiled = make_focused_pane(PaneId::Terminal(1), vec![1]);
        let floating = make_focused_pane(PaneId::Terminal(2), vec![1]);

        let mut floating_shown = SessionLayoutMetadata::default();
        floating_shown.add_tab(
            "tab1".to_string(),
            true,
            false,
            vec![tiled.clone()],
            vec![floating.clone()],
        );
        assert_eq!(
            focused_pane_ids(&floating_shown),
            vec![(1, PaneId::Terminal(2))]
        );

        let mut floating_hidden = SessionLayoutMetadata::default();
        floating_hidden.add_tab("tab1".to_string(), true, true, vec![tiled], vec![floating]);
        assert_eq!(
            focused_pane_ids(&floating_hidden),
            vec![(1, PaneId::Terminal(1))]
        );
    }

    #[test]
    fn rendered_client_list_names_every_client() {
        let tiled = make_focused_pane(PaneId::Terminal(1), vec![2]);
        let floating = make_focused_pane(PaneId::Plugin(3), vec![1]);
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, vec![tiled], vec![floating]);
        let rendered = meta.list_clients_metadata();
        assert_eq!(rendered.lines().count(), 3, "a header and two clients");
        assert!(rendered.contains("plugin_3"), "{}", rendered);
        assert!(rendered.contains("terminal_1"), "{}", rendered);
    }

    #[test]
    fn json_client_list_carries_every_client() {
        let tiled = make_focused_pane(PaneId::Terminal(1), vec![2]);
        let floating = make_focused_pane(PaneId::Plugin(3), vec![1]);
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, vec![tiled], vec![floating]);

        let clients = meta.client_infos(2);
        assert_eq!(clients.len(), 2);
        assert_eq!(clients[0].client_id, 1);
        assert_eq!(clients[0].pane_id, ZellijPaneId::Plugin(3));
        assert!(!clients[0].is_current_client);
        assert_eq!(clients[1].client_id, 2);
        assert_eq!(clients[1].pane_id, ZellijPaneId::Terminal(1));
        assert!(clients[1].is_current_client, "the requesting client");
    }

    #[test]
    fn json_client_list_is_an_array_and_nothing_else() {
        let tiled = make_focused_pane(PaneId::Terminal(1), vec![2]);
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, vec![tiled], vec![]);
        let rendered = meta.list_clients_metadata_json(2);
        let parsed: Vec<ClientInfo> = serde_json::from_str(&rendered).expect("valid JSON array");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].client_id, 2);
    }

    #[test]
    fn json_client_list_is_empty_when_nobody_is_attached() {
        let meta = SessionLayoutMetadata::default();
        assert_eq!(meta.list_clients_metadata_json(1), "[]");
    }

    #[test]
    fn removing_a_plugin_takes_the_clients_focused_on_it() {
        // why the client list must not drop the plugin that asked for it: the pane it would drop
        // is the pane its clients are looking at
        let floating = make_focused_pane(PaneId::Plugin(3), vec![1, 2]);
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab("tab1".to_string(), true, false, vec![], vec![floating]);
        assert_eq!(meta.all_clients_metadata().len(), 2);
        meta.remove_plugin_from_layout(3);
        assert!(meta.all_clients_metadata().is_empty());
    }

    fn pane_focused_by(terminal_id: u32, focused_clients: Vec<ClientId>) -> PaneLayoutMetadata {
        PaneLayoutMetadata::new(
            PaneId::Terminal(terminal_id),
            PaneGeom::default(),
            false,
            None,
            None,
            !focused_clients.is_empty(),
            None,
            focused_clients,
            None,
            None,
            None,
        )
    }

    #[test]
    fn each_client_carries_the_size_of_its_own_terminal() {
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab(
            "tab1".to_string(),
            true,
            true,
            vec![pane_focused_by(1, vec![1]), pane_focused_by(2, vec![2])],
            vec![],
        );
        let mut client_sizes = BTreeMap::new();
        client_sizes.insert(
            1,
            Size {
                rows: 50,
                cols: 200,
            },
        );
        client_sizes.insert(2, Size { rows: 20, cols: 60 });
        meta.set_client_sizes(client_sizes);

        let clients = meta.all_clients_metadata();
        assert_eq!(
            clients.get(&1).and_then(|c| c.terminal_size()),
            Some(Size {
                rows: 50,
                cols: 200
            })
        );
        assert_eq!(
            clients.get(&2).and_then(|c| c.terminal_size()),
            Some(Size { rows: 20, cols: 60 })
        );
    }

    #[test]
    fn a_client_the_server_has_not_sized_reports_no_size() {
        let mut meta = SessionLayoutMetadata::default();
        meta.add_tab(
            "tab1".to_string(),
            true,
            true,
            vec![pane_focused_by(1, vec![1])],
            vec![],
        );
        let clients = meta.all_clients_metadata();
        assert_eq!(clients.get(&1).and_then(|c| c.terminal_size()), None);
    }

    #[test]
    fn a_session_in_the_shape_of_its_layout_is_not_dirty() {
        let meta = session_matching_layout(&["one", "two", "three"]);
        assert!(
            !meta.is_dirty(),
            "nothing has changed since the layout built it"
        );
    }

    #[test]
    fn a_moved_tab_makes_the_layout_dirty() {
        // a move changes neither the pane count nor any command, so it is invisible to every other
        // dirty check - and a layout that is never rewritten hands the tab back where it started
        let mut meta = session_matching_layout(&["one", "two", "three"]);
        meta.tabs.swap(1, 2);
        assert_eq!(
            meta.tab_names(),
            vec!["one".to_string(), "three".to_string(), "two".to_string()],
            "the tab really moved"
        );
        assert!(
            meta.is_dirty(),
            "the order on disk no longer matches the order the user sees"
        );
    }

    #[test]
    fn a_renamed_tab_makes_the_layout_dirty() {
        let mut meta = session_matching_layout(&["one", "two"]);
        meta.tabs[1].name = Some("renamed".to_string());
        assert!(meta.is_dirty(), "the name on disk is the old one");
    }

    #[test]
    fn a_layout_that_names_no_tabs_of_its_own_leaves_the_session_clean() {
        // the base layout describes a template rather than tabs, so it says nothing about the tabs
        // this session has - and a session that has diverged from nothing is not dirty
        let mut layout = Layout::default();
        layout.template = Some((TiledPaneLayout::default(), vec![]));
        let mut meta = SessionLayoutMetadata::new(Box::new(layout));
        meta.add_tab(
            "Tab #1".to_string(),
            true,
            false,
            vec![make_plain_pane(0)],
            vec![],
        );
        assert!(
            !meta.is_dirty(),
            "a template-only layout must not make every session dirty"
        );
    }

    #[test]
    fn a_moved_tab_diverges_from_a_layout_parsed_from_kdl() {
        // the shape a real session is built from: named tabs over a default_tab_template, parsed
        // by the same parser that reads the layout file
        let raw = "layout {\n \
                   default_tab_template {\n \
                   pane size=1 borderless=true {\n \
                   plugin location=\"zellij:tab-bar\"\n \
                   }\n \
                   children\n \
                   }\n \
                   tab name=\"console\"\n \
                   tab name=\"develop\"\n \
                   }";
        let layout = Layout::from_str(raw, "test".into(), None, None).unwrap();
        let mut meta = SessionLayoutMetadata::new(Box::new(layout));
        for (i, name) in ["console", "develop"].iter().enumerate() {
            meta.add_tab(
                name.to_string(),
                i == 0,
                false,
                vec![
                    make_plain_pane(i as u32 * 2),
                    make_plain_pane(i as u32 * 2 + 1),
                ],
                vec![],
            );
        }
        assert!(
            !meta.tabs_diverge_from_base_layout(),
            "the session is in the shape the layout describes"
        );
        // the reason the comparison is read directly rather than through `is_dirty`:
        // `Layout::pane_count` counts the template's panes on top of the tabs it already expanded
        // them into, so this session - in exactly the shape of the layout that built it - already
        // fails the pane count comparison and is dirty before its tabs are ever looked at
        assert!(
            meta.is_dirty(),
            "a parsed layout counts its template twice, so nothing here is ever clean"
        );
        meta.tabs.swap(0, 1);
        assert!(
            meta.tabs_diverge_from_base_layout(),
            "the tabs are no longer in the order the layout lists them"
        );
    }
}
