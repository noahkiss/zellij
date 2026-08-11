use crate::plugins::plugin_map::{
    PluginEnv, PluginMap, RunningPlugin, VecDequeInputStream, WriteOutputStream,
};
use crate::plugins::plugin_worker::{plugin_worker, RunningWorker};
use crate::plugins::wasm_bridge::{LoadingContext, PluginCache};
use crate::plugins::zellij_exports::{wasi_write_object, zellij_exports};
use crate::plugins::PluginId;
use prost::Message;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use wasmi::{Engine, Instance, Linker, Module, Store, StoreLimits};
use wasmi_wasi::sync::WasiCtxBuilder;
use wasmi_wasi::wasi_common::pipe::{ReadPipe, WritePipe};
use wasmi_wasi::Dir;
use wasmi_wasi::WasiCtx;

use crate::{
    logging_pipe::LoggingPipe, thread_bus::ThreadSenders,
    ui::loading_indication::LoadingIndication, ClientId,
};

use zellij_utils::plugin_api::action::ProtobufPluginConfiguration;
use zellij_utils::{
    consts::ZELLIJ_TMP_DIR, data::InputMode, errors::prelude::*, input::command::TerminalAction,
    input::keybinds::Keybinds, input::layout::PluginUserConfiguration,
    input::layout::RunPluginLocation, input::permission::PluginPermissions,
    input::plugins::PluginConfig, pane_size::Size, session_lifecycle::own_executable_path,
};

/// The configuration key the `zellij:about` plugin reads the server's own binary path from.
const SERVER_EXE_CONFIG_KEY: &str = "zellij_exe";

/// The configuration key that asks the about plugin to say what the path is FOR.
///
/// Only macOS has a Full Disk Access panel to paste the path into, and only the host running the
/// server knows whether this is macOS - the client may well be somewhere else. So the server, not
/// the plugin, decides whether the hint is worth a line. The plugin owns the wording.
const SERVER_EXE_HINT_CONFIG_KEY: &str = "zellij_exe_hint";

/// The hint the about plugin renders on macOS hosts.
const FULL_DISK_ACCESS_HINT: &str = "full_disk_access";

/// The path of the server binary, resolved once per server process.
///
/// `current_exe()` is asked on the SERVER side on purpose: the about plugin exists to tell a macOS
/// user which binary to hand Full Disk Access to, and TCC grants follow the process that actually
/// opens the file - the server - never the client that launched it.
fn server_exe_path() -> Option<&'static str> {
    static SERVER_EXE: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    SERVER_EXE
        .get_or_init(|| own_executable_path().map(|path| path.display().to_string()))
        .as_deref()
}

/// Hand a plugin the configuration it should see at load time.
///
/// The stored `PluginConfig` is left alone: its configuration is half of the key the plugin map
/// dedupes and focuses instances by, so injecting into it would make every launch-or-focus of the
/// about plugin miss the running one and open another pane. Only the copy sent to `load()` grows a
/// key, and only for the plugin that asked for it.
fn configuration_for_load(plugin_config: &PluginConfig) -> PluginUserConfiguration {
    let mut configuration = plugin_config.initial_userspace_configuration.clone();
    let is_about_plugin = matches!(
        &plugin_config.location,
        RunPluginLocation::Zellij(tag) if tag.to_string() == "about"
    );
    if is_about_plugin && !configuration.inner().contains_key(SERVER_EXE_CONFIG_KEY) {
        if let Some(server_exe) = server_exe_path() {
            configuration.insert(SERVER_EXE_CONFIG_KEY, server_exe);
            if cfg!(target_os = "macos")
                && !configuration
                    .inner()
                    .contains_key(SERVER_EXE_HINT_CONFIG_KEY)
            {
                configuration.insert(SERVER_EXE_HINT_CONFIG_KEY, FULL_DISK_ACCESS_HINT);
            }
        }
    }
    configuration
}

/// Open a directory as a `File` handle for WASI pre-opening.
/// On Windows, `FILE_FLAG_BACKUP_SEMANTICS` is required to open directories.
#[cfg(not(windows))]
fn open_dir(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    std::fs::File::open(path)
}

#[cfg(windows)]
fn open_dir(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(0x02000000) // FILE_FLAG_BACKUP_SEMANTICS
        .open(path)
}

fn create_plugin_fs_entries(plugin_own_data_dir: &PathBuf, plugin_own_cache_dir: &PathBuf) {
    // Create filesystem entries mounted into WASM.
    // We create them here to get expressive error messages in case they fail.
    if let Err(e) = fs::create_dir_all(&plugin_own_data_dir) {
        log::error!("Failed to create plugin data dir: {}", e);
    };
    if let Err(e) = fs::create_dir_all(&plugin_own_cache_dir) {
        log::error!("Failed to create plugin cache dir: {}", e);
    }
    if let Err(e) = fs::create_dir_all(ZELLIJ_TMP_DIR.as_path()) {
        log::error!("Failed to create plugin tmp dir: {}", e);
    }
}

pub struct PluginLoader<'a> {
    skip_cache: bool,
    plugin_id: PluginId,
    client_id: ClientId,
    plugin_cwd: PathBuf,
    plugin_own_data_dir: PathBuf,
    plugin_own_cache_dir: PathBuf,
    plugin_config: PluginConfig,
    tab_index: Option<usize>,
    path_to_default_shell: PathBuf,
    session_env_vars: std::collections::BTreeMap<String, String>,
    default_shell: Option<TerminalAction>,
    layout_dir: Option<PathBuf>,
    default_mode: InputMode,
    keybinds: Keybinds,
    plugin_permissions: Arc<PluginPermissions>,
    plugin_dir: PathBuf,
    size: Size,
    loading_indication: LoadingIndication,
    senders: ThreadSenders,
    engine: Engine,
    plugin_cache: PluginCache,
    plugin_map: &'a mut PluginMap, // we receive a mutable reference rather than the Arc so that it
    // will be held for the lifetime of this struct and thus loading
    // plugins for all connected clients will be one transaction
    connected_clients: Option<Arc<Mutex<Vec<ClientId>>>>,
}

impl<'a> PluginLoader<'a> {
    pub fn new(
        skip_cache: bool,
        loading_context: LoadingContext,
        senders: ThreadSenders,
        engine: Engine,
        plugin_cache: PluginCache,
        plugin_map: &'a mut PluginMap,
        connected_clients: Arc<Mutex<Vec<ClientId>>>,
    ) -> Self {
        let loading_indication = LoadingIndication::new("".into());
        create_plugin_fs_entries(
            &loading_context.plugin_own_data_dir,
            &loading_context.plugin_own_cache_dir,
        );
        Self {
            plugin_id: loading_context.plugin_id,
            client_id: loading_context.client_id,
            plugin_cwd: loading_context.plugin_cwd,
            plugin_own_data_dir: loading_context.plugin_own_data_dir,
            plugin_own_cache_dir: loading_context.plugin_own_cache_dir,
            plugin_config: loading_context.plugin_config,
            tab_index: loading_context.tab_index,
            path_to_default_shell: loading_context.path_to_default_shell,
            session_env_vars: loading_context.session_env_vars,
            default_shell: loading_context.default_shell,
            layout_dir: loading_context.layout_dir,
            default_mode: loading_context.default_mode,
            keybinds: loading_context.keybinds,
            plugin_permissions: loading_context.plugin_permissions,
            plugin_dir: loading_context.plugin_dir,
            size: loading_context.size,

            skip_cache,
            senders,
            engine,
            plugin_cache,
            plugin_map,
            connected_clients: Some(connected_clients),
            loading_indication,
        }
    }
    pub fn without_connected_clients(mut self) -> Self {
        self.connected_clients = None;
        self
    }
    pub fn start_plugin(&mut self) -> Result<()> {
        let module = if self.skip_cache {
            self.interpret_module()?
        } else {
            self.load_module_from_memory()
                .or_else(|_e| self.interpret_module())?
        };
        let (store, instance) = self.create_plugin_environment(module)?;
        self.load_plugin_instance(store, &instance)?;
        self.clone_instance_for_other_clients()?;
        Ok(())
    }
    fn interpret_module(&mut self) -> Result<Module> {
        self.loading_indication.override_previous_error();
        let wasm_bytes = self.plugin_config.resolve_wasm_bytes(&self.plugin_dir)?;
        let timer = std::time::Instant::now();
        let module = Module::new(&self.engine, &wasm_bytes)?;
        log::info!(
            "Loaded plugin '{}' in {:?}",
            self.plugin_config.path.display(),
            timer.elapsed()
        );
        Ok(module)
    }
    fn load_module_from_memory(&mut self) -> Result<Module> {
        let module = self
            .plugin_cache
            .lock()
            .unwrap()
            .remove(&self.plugin_config.path) // TODO: do we still bring it back later?
            // maybe we can forgo this dance?
            .ok_or(anyhow!("Plugin is not stored in memory"))?;
        Ok(module)
    }
    fn load_plugin_instance(
        &mut self,
        mut store: Store<PluginEnv>,
        instance: &Instance,
    ) -> Result<()> {
        let err_context = || format!("failed to load plugin from instance {instance:#?}");
        let main_user_instance = instance.clone();
        let start_function = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .with_context(err_context)?;
        let load_function = instance
            .get_typed_func::<(), ()>(&mut store, "load")
            .with_context(err_context)?;
        let mut workers = HashMap::new();
        for function_name in instance
            .exports(&mut store)
            .filter_map(|export| export.clone().into_func().map(|_| export.name()))
        {
            if function_name.ends_with("_worker") {
                let (mut store, instance) =
                    self.create_plugin_instance_and_wasi_env_for_worker()?;
                let start_function_for_worker = instance
                    .get_typed_func::<(), ()>(&mut store, "_start")
                    .with_context(err_context)?;
                start_function_for_worker
                    .call(&mut store, ())
                    .with_context(err_context)?;

                let worker = RunningWorker::new(store, instance, &function_name);
                let worker_sender = plugin_worker(worker);
                workers.insert(function_name.into(), worker_sender);
            }
        }

        let subscriptions = store.data().subscriptions.clone();
        let plugin = Arc::new(Mutex::new(RunningPlugin::new(
            store,
            main_user_instance,
            self.size.rows,
            self.size.cols,
        )));
        self.plugin_map.insert(
            self.plugin_id,
            self.client_id,
            plugin.clone(),
            subscriptions,
            workers,
        );

        start_function
            .call(&mut plugin.lock().unwrap().store, ())
            .with_context(err_context)?;

        let protobuf_plugin_configuration: ProtobufPluginConfiguration =
            configuration_for_load(&self.plugin_config)
                .try_into()
                .map_err(|e| anyhow!("Failed to serialize user configuration: {:?}", e))?;
        let protobuf_bytes = protobuf_plugin_configuration.encode_to_vec();
        wasi_write_object(plugin.lock().unwrap().store.data(), &protobuf_bytes)
            .with_context(err_context)?;
        load_function
            .call(&mut plugin.lock().unwrap().store, ())
            .with_context(err_context)?;

        Ok(())
    }
    pub fn create_plugin_environment(
        &self,
        module: Module,
    ) -> Result<(Store<PluginEnv>, Instance)> {
        let err_context = || {
            format!(
                "Failed to create instance, plugin env and subscriptions for plugin {}",
                self.plugin_id
            )
        };
        let stdin_pipe = Arc::new(Mutex::new(VecDeque::new()));
        let stdout_pipe = Arc::new(Mutex::new(VecDeque::new()));

        let wasi_ctx = PluginLoader::create_wasi_ctx(
            &self.plugin_cwd,
            &self.plugin_own_data_dir,
            &self.plugin_own_cache_dir,
            &ZELLIJ_TMP_DIR,
            &self.plugin_config.location.to_string(),
            self.plugin_id,
            stdin_pipe.clone(),
            stdout_pipe.clone(),
        )?;
        let plugin_path = self.plugin_config.path.clone();
        let plugin_env = PluginEnv {
            plugin_id: self.plugin_id,
            client_id: self.client_id,
            plugin: self.plugin_config.clone(), // TODO: change field name in PluginEnv to plugin_config
            permissions: Arc::new(Mutex::new(None)),
            senders: self.senders.clone(),
            wasi_ctx,
            plugin_own_data_dir: self.plugin_own_data_dir.clone(),
            plugin_own_cache_dir: self.plugin_own_cache_dir.clone(),
            tab_index: self.tab_index,
            path_to_default_shell: self.path_to_default_shell.clone(),
            default_shell: self.default_shell.clone(),
            plugin_cwd: self.plugin_cwd.clone(),
            session_env_vars: self.session_env_vars.clone(),
            input_pipes_to_unblock: Arc::new(Mutex::new(HashSet::new())),
            input_pipes_to_block: Arc::new(Mutex::new(HashSet::new())),
            layout_dir: self.layout_dir.clone(),
            default_mode: self.default_mode.clone(),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            keybinds: self.keybinds.clone(),
            plugin_permissions: self.plugin_permissions.clone(),
            intercepting_key_presses: false,
            stdin_pipe,
            stdout_pipe,
            store_limits: create_optimized_store_limits(),
        };
        let mut store = Store::new(&self.engine, plugin_env);

        // Apply optimized resource limits for memory efficiency
        store.limiter(|plugin_env| &mut plugin_env.store_limits);

        let mut linker = Linker::new(&self.engine);
        wasmi_wasi::add_to_linker(&mut linker, |plugin_env: &mut PluginEnv| {
            &mut plugin_env.wasi_ctx
        })?;
        zellij_exports(&mut linker);

        let instance = linker
            .instantiate_and_start(&mut store, &module)
            .with_context(err_context)?;

        if let Some(func) = instance.get_func(&mut store, "_initialize") {
            if let Ok(typed_func) = func.typed::<(), ()>(&store) {
                let _ = typed_func.call(&mut store, ());
            }
        }

        self.plugin_cache
            .lock()
            .unwrap()
            .insert(plugin_path.clone(), module);
        Ok((store, instance))
    }
    pub fn clone_instance_for_other_clients(&mut self) -> Result<()> {
        let Some(connected_clients) = self.connected_clients.as_ref() else {
            return Ok(());
        };
        let connected_clients: Vec<ClientId> =
            connected_clients.lock().unwrap().iter().copied().collect();
        if !connected_clients.is_empty() {
            self.connected_clients = None; // so we don't have infinite loops
            for client_id in connected_clients {
                if client_id == self.client_id {
                    // don't reload the plugin once more for ourselves
                    continue;
                }
                self.client_id = client_id;
                self.start_plugin()?;
            }
        }
        Ok(())
    }
    pub fn create_plugin_instance_and_wasi_env_for_worker(
        &self,
    ) -> Result<(Store<PluginEnv>, Instance)> {
        let plugin_id = self.plugin_id;
        let err_context = || {
            format!(
                "Failed to create instance and plugin env for worker {}",
                plugin_id
            )
        };
        let module = self
            .plugin_cache
            .lock()
            .unwrap()
            .get(&self.plugin_config.path)
            .with_context(err_context)?
            .clone();
        let (store, instance) = self.create_plugin_instance_env(&module)?;
        Ok((store, instance))
    }
    fn create_plugin_instance_env(&self, module: &Module) -> Result<(Store<PluginEnv>, Instance)> {
        let err_context = || {
            format!(
                "Failed to create instance, plugin env and subscriptions for plugin {}",
                self.plugin_id
            )
        };
        let stdin_pipe = Arc::new(Mutex::new(VecDeque::new()));
        let stdout_pipe = Arc::new(Mutex::new(VecDeque::new()));

        let wasi_ctx = PluginLoader::create_wasi_ctx(
            &self.plugin_cwd,
            &self.plugin_own_data_dir,
            &self.plugin_own_cache_dir,
            &ZELLIJ_TMP_DIR,
            &self.plugin_config.location.to_string(),
            self.plugin_id,
            stdin_pipe.clone(),
            stdout_pipe.clone(),
        )?;
        let plugin_config = self.plugin_config.clone();
        let plugin_env = PluginEnv {
            plugin_id: self.plugin_id,
            client_id: self.client_id,
            plugin: plugin_config,
            permissions: Arc::new(Mutex::new(None)),
            senders: self.senders.clone(),
            wasi_ctx,
            plugin_own_data_dir: self.plugin_own_data_dir.clone(),
            plugin_own_cache_dir: self.plugin_own_cache_dir.clone(),
            tab_index: self.tab_index,
            path_to_default_shell: self.path_to_default_shell.clone(),
            default_shell: self.default_shell.clone(),
            plugin_cwd: self.plugin_cwd.clone(),
            session_env_vars: self.session_env_vars.clone(),
            input_pipes_to_unblock: Arc::new(Mutex::new(HashSet::new())),
            input_pipes_to_block: Arc::new(Mutex::new(HashSet::new())),
            layout_dir: self.layout_dir.clone(),
            default_mode: self.default_mode.clone(),
            subscriptions: Arc::new(Mutex::new(HashSet::new())),
            keybinds: self.keybinds.clone(),
            plugin_permissions: self.plugin_permissions.clone(),
            intercepting_key_presses: false,
            stdin_pipe,
            stdout_pipe,
            store_limits: create_optimized_store_limits(),
        };
        let mut store = Store::new(&self.engine, plugin_env);

        // Apply optimized resource limits for memory efficiency
        store.limiter(|plugin_env| &mut plugin_env.store_limits);

        let mut linker = Linker::new(&self.engine);
        wasmi_wasi::add_to_linker(&mut linker, |plugin_env: &mut PluginEnv| {
            &mut plugin_env.wasi_ctx
        })?;
        zellij_exports(&mut linker);

        let instance = linker
            .instantiate_and_start(&mut store, module)
            .with_context(err_context)?;

        if let Some(func) = instance.get_func(&mut store, "_initialize") {
            if let Ok(typed_func) = func.typed::<(), ()>(&store) {
                let _ = typed_func.call(&mut store, ());
            }
        }

        Ok((store, instance))
    }
    pub fn create_wasi_ctx(
        host_dir: &PathBuf,
        data_dir: &PathBuf,
        cache_dir: &PathBuf,
        tmp_dir: &PathBuf,
        plugin_url: &String,
        plugin_id: PluginId,
        stdin_pipe: Arc<Mutex<VecDeque<u8>>>,
        stdout_pipe: Arc<Mutex<VecDeque<u8>>>,
    ) -> Result<WasiCtx> {
        let _err_context = || format!("Failed to create wasi_ctx");
        let dirs = vec![
            ("/host".to_owned(), host_dir.clone()),
            ("/data".to_owned(), data_dir.clone()),
            ("/cache".to_owned(), cache_dir.clone()),
            ("/tmp".to_owned(), tmp_dir.clone()),
        ];
        let dirs = dirs.into_iter().filter(|(_dir_name, dir)| {
            // note that this does not protect against TOCTOU errors
            // eg. if one or more of these folders existed at the time of check but was deleted
            // before we mounted in in the wasi environment, we'll crash
            // when we move to a new wasi environment, we should address this with locking if
            // there's no built-in solution
            dir.try_exists().ok().unwrap_or(false)
        });

        let mut builder = WasiCtxBuilder::new();
        builder.inherit_env()?;

        // Mount directories using the builder
        for (guest_path, host_path) in dirs {
            match open_dir(&host_path) {
                Ok(dir_file) => {
                    let dir = Dir::from_std_file(dir_file);
                    builder.preopened_dir(dir, guest_path)?;
                },
                Err(e) => {
                    log::warn!("Failed to mount directory {:?}: {}", host_path, e);
                },
            }
        }

        let ctx = builder.build();

        // Set up custom stdin/stdout/stderr
        ctx.set_stdin(Box::new(ReadPipe::new(VecDequeInputStream(
            stdin_pipe.clone(),
        ))));
        ctx.set_stdout(Box::new(WritePipe::new(WriteOutputStream(
            stdout_pipe.clone(),
        ))));
        ctx.set_stderr(Box::new(WritePipe::new(WriteOutputStream(Arc::new(
            Mutex::new(LoggingPipe::new(plugin_url, plugin_id)),
        )))));

        Ok(ctx)
    }
}

fn create_optimized_store_limits() -> StoreLimits {
    use wasmi::StoreLimitsBuilder;
    StoreLimitsBuilder::new()
        .instances(1) // One instance per plugin
        .memories(4) // Max 4 linear memories per plugin
        .memory_size(16 * 1024 * 1024) // 16MB per memory maximum
        .tables(16) // Small table element limit
        .trap_on_grow_failure(true) // Fail fast on resource exhaustion
        .build()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin_config(location: &str) -> PluginConfig {
        PluginConfig {
            path: PathBuf::new(),
            _allow_exec_host_cmd: false,
            location: RunPluginLocation::parse(location, None).unwrap(),
            initial_userspace_configuration: Default::default(),
            initial_cwd: None,
        }
    }

    #[test]
    fn about_plugin_is_told_the_server_binary_path() {
        let configuration = configuration_for_load(&plugin_config("zellij:about"));
        let injected = configuration.inner().get(SERVER_EXE_CONFIG_KEY);
        assert_eq!(injected.map(|path| path.as_str()), server_exe_path());
    }

    #[test]
    fn an_explicit_value_is_left_alone() {
        let mut plugin_config = plugin_config("zellij:about");
        plugin_config
            .initial_userspace_configuration
            .insert(SERVER_EXE_CONFIG_KEY, "/somewhere/else");
        let configuration = configuration_for_load(&plugin_config);
        assert_eq!(
            configuration.inner().get(SERVER_EXE_CONFIG_KEY),
            Some(&String::from("/somewhere/else"))
        );
    }

    #[test]
    fn other_plugins_are_untouched() {
        let configuration = configuration_for_load(&plugin_config("zellij:status-bar"));
        assert!(configuration.inner().is_empty());
    }

    #[test]
    fn the_full_disk_access_hint_is_a_macos_thing() {
        let configuration = configuration_for_load(&plugin_config("zellij:about"));
        let hint = configuration.inner().get(SERVER_EXE_HINT_CONFIG_KEY);
        if cfg!(target_os = "macos") {
            assert_eq!(hint, Some(&String::from(FULL_DISK_ACCESS_HINT)));
        } else {
            assert_eq!(hint, None);
        }
    }

    #[test]
    fn an_explicit_hint_is_left_alone() {
        let mut plugin_config = plugin_config("zellij:about");
        plugin_config
            .initial_userspace_configuration
            .insert(SERVER_EXE_HINT_CONFIG_KEY, "something_else");
        let configuration = configuration_for_load(&plugin_config);
        assert_eq!(
            configuration.inner().get(SERVER_EXE_HINT_CONFIG_KEY),
            Some(&String::from("something_else"))
        );
    }
}
