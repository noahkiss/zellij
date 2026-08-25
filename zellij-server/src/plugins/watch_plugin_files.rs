use super::PluginInstruction;
use crate::thread_bus::ThreadSenders;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Duration;

use notify_debouncer_full::{
    new_debouncer,
    notify::{EventKind, RecommendedWatcher, RecursiveMode},
    DebounceEventResult, Debouncer, RecommendedCache,
};
use zellij_utils::{
    errors::prelude::Result,
    input::layout::{RunPlugin, RunPluginLocation},
    input::plugins::builtin_override_for_location,
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
    // `RecommendedCache` is `NoCache` on Linux and `FileIdMap` everywhere else, so naming the
    // concrete type here builds on one platform and not the other.
    debouncer: Debouncer<RecommendedWatcher, RecommendedCache>,
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

    /// Start watching this plugin's .wasm, if one is on disk. Idempotent.
    ///
    /// A built-in normally lives in the binary and has nothing to watch. It gets a file here only
    /// when `builtin_plugin_dir` points at one - the development override, which is what lets a
    /// bundled bar hot-reload the way an external plugin does.
    pub fn watch(&mut self, run_plugin: &RunPlugin) {
        let path = match &run_plugin.location {
            RunPluginLocation::File(path) => path.clone(),
            location => match builtin_override_for_location(location) {
                Some(path) => path,
                None => return,
            },
        };
        let canonical_path = match std::fs::canonicalize(&path) {
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

    /// Stop watching this plugin's .wasm. The inverse of [`PluginFileWatcher::watch`], and
    /// idempotent in the same way: a plugin that is not watched is a no-op.
    ///
    /// Deciding that a plugin is really gone is the caller's job. One `RunPlugin` backs every
    /// plugin id loaded from that file - the same plugin in two panes, or one instance per
    /// connected client - and the watch belongs to the file, so it has to outlive all of them but
    /// the last.
    ///
    /// The watched entry is found by comparison rather than by path, so a plugin whose .wasm has
    /// since been deleted still releases its watch: `canonicalize` would fail on the way back out
    /// and leave the entry behind forever.
    pub fn unwatch(&mut self, run_plugin: &RunPlugin) {
        // `RunPlugin` derives `Hash` over fields its `Eq` ignores, so a set lookup can miss an
        // entry a comparison finds - walk the set instead of removing by hash.
        for run_plugins in self.watched_files.values_mut() {
            run_plugins.retain(|watched| watched != run_plugin);
        }
        self.watched_files
            .retain(|_path, run_plugins| !run_plugins.is_empty());

        // the directory is what is watched, not the file, so it stays until nothing in it is
        let unwatched_dirs: Vec<PathBuf> = self
            .watched_dirs
            .iter()
            .filter(|dir| {
                !self
                    .watched_files
                    .keys()
                    .any(|path| path.parent() == Some(dir.as_path()))
            })
            .cloned()
            .collect();
        for dir in unwatched_dirs {
            if let Err(e) = self.debouncer.unwatch(&dir) {
                log::warn!(
                    "Failed to unwatch plugin directory {}: {}",
                    dir.display(),
                    e
                );
            }
            self.watched_dirs.remove(&dir);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use zellij_utils::input::layout::PluginUserConfiguration;

    fn run_plugin_for(path: &Path, instance: &str) -> RunPlugin {
        // the configuration is what tells two plugins loaded from one file apart: `RunPlugin`'s
        // `Eq` reads the location and the configuration and nothing else
        let mut configuration = PluginUserConfiguration::default();
        configuration.insert("instance", instance);
        RunPlugin {
            _allow_exec_host_cmd: false,
            location: RunPluginLocation::File(path.to_path_buf()),
            configuration,
            initial_cwd: None,
        }
    }

    fn watcher() -> PluginFileWatcher {
        PluginFileWatcher::new(ThreadSenders {
            should_silently_fail: true,
            ..Default::default()
        })
        .unwrap()
    }

    #[test]
    fn unwatch_releases_the_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("plugin.wasm");
        std::fs::write(&wasm, b"").unwrap();
        let run_plugin = run_plugin_for(&wasm, "one");

        let mut watcher = watcher();
        watcher.watch(&run_plugin);
        assert_eq!(watcher.watched_files.len(), 1);
        assert_eq!(watcher.watched_dirs.len(), 1);

        watcher.unwatch(&run_plugin);
        assert!(watcher.watched_files.is_empty());
        assert!(watcher.watched_dirs.is_empty());
    }

    #[test]
    fn unwatch_keeps_a_file_another_plugin_is_still_loaded_from() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("plugin.wasm");
        std::fs::write(&wasm, b"").unwrap();
        let first = run_plugin_for(&wasm, "one");
        let second = run_plugin_for(&wasm, "two");

        let mut watcher = watcher();
        watcher.watch(&first);
        watcher.watch(&second);

        watcher.unwatch(&first);
        assert_eq!(watcher.watched_files.len(), 1);
        assert_eq!(watcher.watched_dirs.len(), 1);
        assert_eq!(
            watcher.plugins_for_changed_paths(&[wasm.clone()]),
            vec![second.clone()]
        );

        watcher.unwatch(&second);
        assert!(watcher.watched_files.is_empty());
        assert!(watcher.watched_dirs.is_empty());
    }

    #[test]
    fn unwatch_keeps_a_directory_another_plugin_is_watched_in() {
        let dir = tempfile::tempdir().unwrap();
        let first_wasm = dir.path().join("first.wasm");
        let second_wasm = dir.path().join("second.wasm");
        std::fs::write(&first_wasm, b"").unwrap();
        std::fs::write(&second_wasm, b"").unwrap();
        let first = run_plugin_for(&first_wasm, "one");
        let second = run_plugin_for(&second_wasm, "two");

        let mut watcher = watcher();
        watcher.watch(&first);
        watcher.watch(&second);
        assert_eq!(watcher.watched_dirs.len(), 1);

        watcher.unwatch(&first);
        assert_eq!(watcher.watched_files.len(), 1);
        assert_eq!(watcher.watched_dirs.len(), 1);
    }

    #[test]
    fn unwatch_releases_a_plugin_whose_wasm_is_gone_from_disk() {
        let dir = tempfile::tempdir().unwrap();
        let wasm = dir.path().join("plugin.wasm");
        std::fs::write(&wasm, b"").unwrap();
        let run_plugin = run_plugin_for(&wasm, "one");

        let mut watcher = watcher();
        watcher.watch(&run_plugin);
        std::fs::remove_file(&wasm).unwrap();

        watcher.unwatch(&run_plugin);
        assert!(watcher.watched_files.is_empty());
        assert!(watcher.watched_dirs.is_empty());
    }
}
