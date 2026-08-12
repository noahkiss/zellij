//! Plugins configuration metadata
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

use serde::{Deserialize, Serialize};
use url::Url;

use super::layout::{PluginUserConfiguration, RunPlugin, RunPluginLocation};
#[cfg(not(target_family = "wasm"))]
use crate::consts::ASSET_MAP;
use crate::consts::BUILTIN_PLUGIN_NAMES;
pub use crate::data::PluginTag;
use crate::errors::prelude::*;

/// The configured `builtin_plugin_dir`, set once by the server as it starts.
///
/// A `OnceLock` rather than an argument threaded through `resolve_wasm_bytes`, because the two
/// places that need it - loading a built-in and watching its file - reach it from different
/// threads and neither carries the config.
#[cfg(not(target_family = "wasm"))]
static BUILTIN_PLUGIN_DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Record the configured `builtin_plugin_dir`. The first call wins; later ones are ignored.
#[cfg(not(target_family = "wasm"))]
pub fn set_builtin_plugin_dir(dir: Option<PathBuf>) {
    let _ = BUILTIN_PLUGIN_DIR.set(dir);
}

/// The overriding `.wasm` for this built-in name, if one is configured AND on disk.
///
/// Absent file means no override: overriding one built-in must not break the others, and a
/// directory holding a single bar is the normal case.
#[cfg(not(target_family = "wasm"))]
pub fn builtin_plugin_override(name: &str) -> Option<PathBuf> {
    builtin_plugin_override_in(BUILTIN_PLUGIN_DIR.get()?.as_ref()?, name)
}

/// The same lookup against a given directory. Split out because the configured one is a `OnceLock`
/// and a test process only gets to set it once.
#[cfg(not(target_family = "wasm"))]
pub fn builtin_plugin_override_in(dir: &Path, name: &str) -> Option<PathBuf> {
    if !BUILTIN_PLUGIN_NAMES.contains(&name) {
        return None;
    }
    let path = dir.join(name).with_extension("wasm");
    path.is_file().then_some(path)
}

/// The overriding `.wasm` for a plugin location, if it is a built-in and one is configured.
#[cfg(not(target_family = "wasm"))]
pub fn builtin_override_for_location(location: &RunPluginLocation) -> Option<PathBuf> {
    match location {
        RunPluginLocation::Zellij(tag) => builtin_plugin_override(&tag.to_string()),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct PluginAliases {
    pub aliases: BTreeMap<String, RunPlugin>,
}

impl PluginAliases {
    pub fn merge(&mut self, other: Self) {
        self.aliases.extend(other.aliases);
    }
    pub fn from_data(aliases: BTreeMap<String, RunPlugin>) -> Self {
        PluginAliases { aliases }
    }
    pub fn list(&self) -> Vec<String> {
        self.aliases.keys().cloned().collect()
    }
}

/// Plugin metadata
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct PluginConfig {
    /// Path of the plugin, see resolve_wasm_bytes for resolution semantics
    pub path: PathBuf,
    /// Allow command execution from plugin
    pub _allow_exec_host_cmd: bool,
    /// Original location of the
    pub location: RunPluginLocation,
    /// Custom configuration for this plugin
    pub initial_userspace_configuration: PluginUserConfiguration,
    /// plugin initial working directory
    pub initial_cwd: Option<PathBuf>,
}

impl PluginConfig {
    pub fn from_run_plugin(run_plugin: &RunPlugin) -> Option<PluginConfig> {
        match &run_plugin.location {
            RunPluginLocation::File(path) => Some(PluginConfig {
                path: path.clone(),
                _allow_exec_host_cmd: run_plugin._allow_exec_host_cmd,
                location: run_plugin.location.clone(),
                initial_userspace_configuration: run_plugin.configuration.clone(),
                initial_cwd: run_plugin.initial_cwd.clone(),
            }),
            RunPluginLocation::Zellij(tag) => {
                let tag = tag.to_string();
                if BUILTIN_PLUGIN_NAMES.contains(&tag.as_str()) {
                    Some(PluginConfig {
                        path: PathBuf::from(&tag),
                        _allow_exec_host_cmd: run_plugin._allow_exec_host_cmd,
                        location: RunPluginLocation::parse(&format!("zellij:{}", tag), None)
                            .ok()?,
                        initial_userspace_configuration: run_plugin.configuration.clone(),
                        initial_cwd: run_plugin.initial_cwd.clone(),
                    })
                } else {
                    None
                }
            },
            RunPluginLocation::Remote(_) => Some(PluginConfig {
                path: PathBuf::new(),
                _allow_exec_host_cmd: run_plugin._allow_exec_host_cmd,
                location: run_plugin.location.clone(),
                initial_userspace_configuration: run_plugin.configuration.clone(),
                initial_cwd: run_plugin.initial_cwd.clone(),
            }),
        }
    }
    /// Resolve wasm plugin bytes for the plugin path and given plugin directory.
    ///
    /// If zellij was built without the 'disable_automatic_asset_installation' feature, builtin
    /// plugins (Starting with 'zellij:' in the layout file) are loaded directly from the
    /// binary-internal asset map. Otherwise:
    ///
    /// Attempts to first resolve the plugin path as an absolute path, then adds a ".wasm"
    /// extension to the path and resolves that, then the plugin directory joined with the path
    /// with an appended ".wasm" extension, and finally the system data directory joined with
    /// "plugins" and the same file name. So if our path is "tab-bar" and the given plugin dir is
    /// "/home/bob/.local/share/zellij/plugins" the lookup chain will be this:
    ///
    /// ```bash
    ///   tab-bar
    ///   tab-bar.wasm
    ///   /home/bob/.local/share/zellij/plugins/tab-bar.wasm
    ///   /usr/share/zellij/plugins/tab-bar.wasm
    /// ```
    ///
    pub fn resolve_wasm_bytes(&self, plugin_dir: &Path) -> Result<Vec<u8>> {
        let err_context =
            |err: std::io::Error, path: &PathBuf| format!("{}: '{}'", err, path.display());

        // Locations we check for valid plugins
        #[allow(unused_mut)]
        let mut paths: Vec<PathBuf> = vec![
            self.path.clone(),
            self.path.with_extension("wasm"),
            plugin_dir.join(&self.path).with_extension("wasm"),
        ];
        #[cfg(not(target_family = "wasm"))]
        paths.push(
            crate::home::system_data_dir()
                .join("plugins")
                .join(&self.path)
                .with_extension("wasm"),
        );
        // Throw out dupes, because it's confusing to read that zellij checked the same plugin
        // location multiple times. Do NOT sort the vector here, because it will break the lookup!
        paths.dedup();

        // This looks weird and usually we would handle errors like this differently, but in this
        // case it's helpful for users and developers alike. This way we preserve all the lookup
        // errors and can report all of them back. We must initialize `last_err` with something,
        // and since the user will only get to see it when loading a plugin failed, we may as well
        // spell it out right here.
        let mut last_err: Result<Vec<u8>> = Err(anyhow!("failed to load plugin from disk"));

        // A configured `builtin_plugin_dir` outranks the embedded copy, so that a bundled plugin
        // can be developed the way a `file:` one is. Read failures fall through to the embedded
        // asset rather than failing the load: a half-written .wasm mid-build must not take the bar
        // down, and the watcher will reload it when the build finishes.
        #[cfg(not(target_family = "wasm"))]
        if let Some(override_path) = builtin_override_for_location(&self.location) {
            match fs::read(&override_path) {
                Ok(bytes) => {
                    log::debug!(
                        "Loaded builtin plugin '{}' from {}",
                        self.path.display(),
                        override_path.display()
                    );
                    return Ok(bytes);
                },
                Err(e) => log::warn!(
                    "Cannot read builtin plugin override {}, using the embedded copy: {}",
                    override_path.display(),
                    e
                ),
            }
        }

        for path in paths {
            // Check if the plugin path matches an entry in the asset map. If so, load it directly
            // from memory, don't bother with the disk.
            #[cfg(not(target_family = "wasm"))]
            if !cfg!(feature = "disable_automatic_asset_installation") && self.is_builtin() {
                let asset_path = PathBuf::from("plugins").join(&path);
                if let Some(bytes) = ASSET_MAP.get(&asset_path) {
                    log::debug!("Loaded plugin '{}' from internal assets", path.display());

                    if plugin_dir.join(&path).with_extension("wasm").exists() {
                        log::info!(
                            "Plugin '{}' exists in the 'PLUGIN DIR' at '{}' but is being ignored",
                            path.display(),
                            plugin_dir.display()
                        );
                    }

                    return Ok(bytes.to_vec());
                }
            }

            // Try to read from disk
            match fs::read(&path) {
                Ok(val) => {
                    log::debug!("Loaded plugin '{}' from disk", path.display());
                    return Ok(val);
                },
                Err(err) => {
                    last_err = last_err.with_context(|| err_context(err, &path));
                },
            }
        }

        // Not reached if a plugin is found!
        #[cfg(not(target_family = "wasm"))]
        if self.is_builtin() {
            // Layout requested a builtin plugin that wasn't found
            let plugin_path = self.path.with_extension("wasm");

            if cfg!(feature = "disable_automatic_asset_installation") && self.is_builtin_name() {
                return Err(ZellijError::BuiltinPluginMissing {
                    plugin_path,
                    plugin_dir: plugin_dir.to_owned(),
                    source: last_err.unwrap_err(),
                })
                .context("failed to load a plugin");
            } else {
                return Err(ZellijError::BuiltinPluginNonexistent {
                    plugin_path,
                    source: last_err.unwrap_err(),
                })
                .context("failed to load a plugin");
            }
        }

        return last_err;
    }

    pub fn is_builtin(&self) -> bool {
        matches!(self.location, RunPluginLocation::Zellij(_))
    }

    pub fn is_builtin_name(&self) -> bool {
        self.path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(|name| BUILTIN_PLUGIN_NAMES.contains(&name))
            .unwrap_or(false)
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum PluginsConfigError {
    #[error("Duplication in plugin tag names is not allowed: '{}'", String::from(.0.clone()))]
    DuplicatePlugins(PluginTag),
    #[error("Failed to parse url: {0:?}")]
    InvalidUrl(#[from] url::ParseError),
    #[error("Only 'file:', 'http(s):' and 'zellij:' url schemes are supported for plugin lookup. '{0}' does not match either.")]
    InvalidUrlScheme(Url),
    #[error("Could not find plugin at the path: '{0:?}'")]
    InvalidPluginLocation(PathBuf),
}

#[cfg(all(test, not(target_family = "wasm")))]
mod builtin_override_test {
    use super::*;

    fn temp_dir_with(files: &[&str]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "zellij-builtin-override-{}-{}",
            std::process::id(),
            files.join("-")
        ));
        let _ = fs::create_dir_all(&dir);
        for file in files {
            let _ = fs::write(dir.join(file), b"not really wasm");
        }
        dir
    }

    #[test]
    fn a_file_in_the_directory_overrides_the_embedded_plugin() {
        let dir = temp_dir_with(&["tab-bar.wasm"]);
        assert_eq!(
            builtin_plugin_override_in(&dir, "tab-bar"),
            Some(dir.join("tab-bar.wasm"))
        );
    }

    #[test]
    fn a_builtin_with_no_file_there_keeps_the_embedded_one() {
        // overriding one plugin must not disturb the rest, so an absent file is not an error
        let dir = temp_dir_with(&["tab-bar.wasm"]);
        assert_eq!(builtin_plugin_override_in(&dir, "status-bar"), None);
    }

    #[test]
    fn only_builtin_names_can_be_overridden() {
        // otherwise the directory would silently shadow plugins loaded from anywhere else
        let dir = temp_dir_with(&["not-a-builtin.wasm"]);
        assert_eq!(builtin_plugin_override_in(&dir, "not-a-builtin"), None);
    }

    #[test]
    fn the_bundled_bars_are_builtin_names() {
        assert!(BUILTIN_PLUGIN_NAMES.contains(&"slim-tab-bar"));
        assert!(BUILTIN_PLUGIN_NAMES.contains(&"slim-keybinds"));
    }
}
