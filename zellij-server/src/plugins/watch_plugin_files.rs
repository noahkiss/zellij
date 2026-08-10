use super::PluginInstruction;
use crate::thread_bus::ThreadSenders;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use notify_debouncer_full::{
    new_debouncer,
    notify::{EventKind, RecommendedWatcher, RecursiveMode},
    DebounceEventResult, Debouncer, NoCache,
};
use zellij_utils::{
    errors::prelude::Result,
    input::layout::{RunPlugin, RunPluginLocation},
};

/// A build writes the .wasm once, but a replace-by-rename still surfaces as several events, and a
/// slow linker can leave a truncated file readable for a moment.
const DEBOUNCE_DURATION_MS: u64 = 300;

/// Watches the .wasm files of loaded `file:` plugins and asks the plugin thread to reload them when
/// they change on disk.
///
/// The directory containing each plugin is watched rather than the file itself: build tools
/// routinely replace a .wasm by writing a temporary file and renaming it over the target, which
/// invalidates an inode-level watch on the original. Paths are canonicalized first so that a plugin
/// loaded through a symlink (a yadm-managed plugins directory, say) is matched against the real
/// file that actually changes.
pub struct PluginFileWatcher {
    debouncer: Debouncer<RecommendedWatcher, NoCache>,
    /// canonical .wasm path -> the plugins loaded from it
    watched_files: HashMap<PathBuf, HashSet<RunPlugin>>,
    watched_dirs: HashSet<PathBuf>,
}

impl PluginFileWatcher {
    pub fn new(senders: ThreadSenders) -> Result<Self> {
        let debouncer = new_debouncer(
            Duration::from_millis(DEBOUNCE_DURATION_MS),
            None,
            move |result: DebounceEventResult| match result {
                Ok(events) => {
                    let mut changed_paths: Vec<PathBuf> = vec![];
                    for event in events {
                        if matches!(event.kind, EventKind::Modify(_) | EventKind::Create(_)) {
                            changed_paths.extend(event.paths.iter().cloned());
                        }
                    }
                    if !changed_paths.is_empty() {
                        let _ = senders
                            .send_to_plugin(PluginInstruction::PluginFilesChanged(changed_paths));
                    }
                },
                Err(errors) => errors
                    .iter()
                    .for_each(|error| log::error!("plugin file watch error: {error:?}")),
            },
        )?;
        Ok(PluginFileWatcher {
            debouncer,
            watched_files: HashMap::new(),
            watched_dirs: HashSet::new(),
        })
    }

    /// Start watching this plugin's .wasm, if it is a local file. Idempotent.
    pub fn watch(&mut self, run_plugin: &RunPlugin) {
        let RunPluginLocation::File(path) = &run_plugin.location else {
            return;
        };
        let canonical_path = match std::fs::canonicalize(path) {
            Ok(canonical_path) => canonical_path,
            Err(e) => {
                log::warn!(
                    "Not watching plugin file {}, cannot resolve it: {}",
                    path.display(),
                    e
                );
                return;
            },
        };
        let Some(parent_dir) = canonical_path.parent().map(|p| p.to_path_buf()) else {
            return;
        };
        if self.watched_dirs.insert(parent_dir.clone()) {
            if let Err(e) = self
                .debouncer
                .watch(&parent_dir, RecursiveMode::NonRecursive)
            {
                log::error!(
                    "Failed to watch plugin directory {}: {}",
                    parent_dir.display(),
                    e
                );
                self.watched_dirs.remove(&parent_dir);
                return;
            }
        }
        self.watched_files
            .entry(canonical_path)
            .or_default()
            .insert(run_plugin.clone());
    }

    /// The watched plugins whose .wasm is among `changed_paths`.
    pub fn plugins_for_changed_paths(&self, changed_paths: &[PathBuf]) -> Vec<RunPlugin> {
        let mut changed_plugins: Vec<RunPlugin> = vec![];
        for path in changed_paths {
            let canonical_path = std::fs::canonicalize(path).unwrap_or_else(|_| path.clone());
            let Some(run_plugins) = self.watched_files.get(&canonical_path) else {
                continue;
            };
            for run_plugin in run_plugins {
                if !changed_plugins.contains(run_plugin) {
                    changed_plugins.push(run_plugin.clone());
                }
            }
        }
        changed_plugins
    }

    pub fn stop(self) {
        self.debouncer.stop_nonblocking();
    }
}
