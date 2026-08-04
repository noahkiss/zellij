//! The session snapshot archive.
//!
//! Zellij keeps exactly one serialized shape per session name and overwrites it in place, so the
//! layout worth keeping is destroyed by its own replacement (and by `delete-session`, which is the
//! operation that motivates restoring in the first place). This module keeps a dated history
//! beside it.
//!
//! A snapshot is a **directory copy** of the live `session_info` folder plus a `snapshot.kdl`
//! sidecar. The copy has to be a directory rather than a single file because the layout parser
//! resolves the `initial_contents_<n>` files it references against the layout file's own parent
//! folder — so a self-contained directory replays through the existing parser unchanged.
//!
//! The archive lives under the state directory rather than the cache: it survives a cache wipe, an
//! upgrade and a client/server contract bump.

use crate::consts::{
    session_info_folder_for_session, CLIENT_SERVER_CONTRACT_VERSION, VERSION, ZELLIJ_SNAPSHOT_DIR,
};
use crate::data::SessionInfo;
use crate::input::options::Options;
use std::path::{Path, PathBuf};

/// Snapshots kept per session name before the oldest are pruned.
pub const DEFAULT_SESSION_SNAPSHOT_LIMIT: usize = 10;

pub const SNAPSHOT_SIDECAR_FILE_NAME: &str = "snapshot.kdl";
pub const SNAPSHOT_LAYOUT_FILE_NAME: &str = "session-layout.kdl";
pub const SNAPSHOT_METADATA_FILE_NAME: &str = "session-metadata.kdl";

/// What caused a snapshot to be cut. Recorded in the sidecar, and only ever additive: an unknown
/// reason read from a newer binary's snapshot is kept as-is rather than rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SnapshotReason {
    Shutdown,
    Manual,
    Delete,
    Promoted,
    Imported,
    Other(String),
}

impl SnapshotReason {
    pub fn as_str(&self) -> &str {
        match self {
            SnapshotReason::Shutdown => "shutdown",
            SnapshotReason::Manual => "manual",
            SnapshotReason::Delete => "delete",
            SnapshotReason::Promoted => "promoted",
            SnapshotReason::Imported => "imported",
            SnapshotReason::Other(other) => other.as_str(),
        }
    }
}

impl From<&str> for SnapshotReason {
    fn from(raw: &str) -> Self {
        match raw {
            "shutdown" => SnapshotReason::Shutdown,
            "manual" => SnapshotReason::Manual,
            "delete" => SnapshotReason::Delete,
            "promoted" => SnapshotReason::Promoted,
            "imported" => SnapshotReason::Imported,
            other => SnapshotReason::Other(other.to_owned()),
        }
    }
}

impl std::fmt::Display for SnapshotReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Where the archive lives and how much of it is kept.
#[derive(Debug, Clone)]
pub struct SnapshotSettings {
    pub dir: PathBuf,
    pub limit: usize,
}

impl Default for SnapshotSettings {
    fn default() -> Self {
        SnapshotSettings {
            dir: ZELLIJ_SNAPSHOT_DIR.clone(),
            limit: DEFAULT_SESSION_SNAPSHOT_LIMIT,
        }
    }
}

impl SnapshotSettings {
    pub fn from_options(options: Option<&Options>) -> Self {
        let mut settings = SnapshotSettings::default();
        if let Some(options) = options {
            if let Some(dir) = options.snapshot_dir.as_ref() {
                settings.dir = dir.clone();
            }
            if let Some(limit) = options.session_snapshot_limit {
                settings.limit = limit;
            }
        }
        settings
    }
    /// A limit of `0` turns archiving off entirely.
    pub fn enabled(&self) -> bool {
        self.limit > 0
    }
}

/// The `snapshot.kdl` sidecar: everything `snapshot list` needs without parsing the layout.
///
/// Read leniently on purpose. Unknown keys are ignored and missing keys take defaults, so an older
/// fork binary can read a snapshot written by a newer one. Fields are never repurposed or removed;
/// a new meaning gets a new key.
#[derive(Debug, Clone)]
pub struct SnapshotMeta {
    pub session_name: String,
    pub saved_at: u64,
    pub zellij_version: String,
    pub contract_version: usize,
    pub reason: SnapshotReason,
    pub tabs: usize,
    pub panes: usize,
    /// The directory this snapshot was imported from, for imported snapshots only.
    pub imported_from: Option<String>,
}

impl SnapshotMeta {
    pub fn to_kdl_string(&self) -> String {
        let mut out = String::from("snapshot {\n");
        out.push_str(&format!("    session_name {:?}\n", self.session_name));
        out.push_str(&format!("    saved_at {}\n", self.saved_at));
        out.push_str(&format!("    zellij_version {:?}\n", self.zellij_version));
        out.push_str(&format!("    contract_version {}\n", self.contract_version));
        out.push_str(&format!("    reason {:?}\n", self.reason.as_str()));
        out.push_str(&format!("    tabs {}\n", self.tabs));
        out.push_str(&format!("    panes {}\n", self.panes));
        if let Some(imported_from) = self.imported_from.as_ref() {
            out.push_str(&format!("    imported_from {:?}\n", imported_from));
        }
        out.push_str("}\n");
        out
    }
    pub fn from_kdl_string(raw: &str, session_name_fallback: &str) -> Self {
        let mut meta = SnapshotMeta {
            session_name: session_name_fallback.to_owned(),
            saved_at: 0,
            zellij_version: String::new(),
            contract_version: 0,
            reason: SnapshotReason::Other(String::new()),
            tabs: 0,
            panes: 0,
            imported_from: None,
        };
        let Ok(document) = raw.parse::<kdl::KdlDocument>() else {
            return meta;
        };
        let Some(children) = document
            .nodes()
            .iter()
            .find(|node| node.name().value() == "snapshot")
            .and_then(|node| node.children())
        else {
            return meta;
        };
        for node in children.nodes() {
            let string_value = node
                .entries()
                .iter()
                .next()
                .and_then(|e| e.value().as_string());
            let int_value = node
                .entries()
                .iter()
                .next()
                .and_then(|e| e.value().as_i64());
            match node.name().value() {
                "session_name" => {
                    if let Some(value) = string_value {
                        meta.session_name = value.to_owned();
                    }
                },
                "saved_at" => {
                    if let Some(value) = int_value {
                        meta.saved_at = value.max(0) as u64;
                    }
                },
                "zellij_version" => {
                    if let Some(value) = string_value {
                        meta.zellij_version = value.to_owned();
                    }
                },
                "contract_version" => {
                    if let Some(value) = int_value {
                        meta.contract_version = value.max(0) as usize;
                    }
                },
                "reason" => {
                    if let Some(value) = string_value {
                        meta.reason = SnapshotReason::from(value);
                    }
                },
                "tabs" => {
                    if let Some(value) = int_value {
                        meta.tabs = value.max(0) as usize;
                    }
                },
                "panes" => {
                    if let Some(value) = int_value {
                        meta.panes = value.max(0) as usize;
                    }
                },
                "imported_from" => {
                    if let Some(value) = string_value {
                        meta.imported_from = Some(value.to_owned());
                    }
                },
                _ => {}, // unknown keys are ignored: the sidecar is additive-only
            }
        }
        meta
    }
}

/// One archived snapshot on disk.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The directory name, `<epoch_ms>-<short_hash>`. Accepted by any unique prefix, like a git
    /// short SHA.
    pub id: String,
    pub session_name: String,
    pub path: PathBuf,
    pub meta: SnapshotMeta,
}

impl Snapshot {
    pub fn layout_file(&self) -> PathBuf {
        self.path.join(SNAPSHOT_LAYOUT_FILE_NAME)
    }
    /// The saved layout, parsed. `Err` when the layout no longer parses with this binary - listing
    /// reports that rather than failing, so the raw KDL stays on disk for a human to repair.
    pub fn layout(&self) -> Result<crate::input::layout::Layout, String> {
        let layout_file = self.layout_file();
        let raw = std::fs::read_to_string(&layout_file)
            .map_err(|e| format!("failed to read {}: {}", layout_file.display(), e))?;
        crate::input::layout::Layout::from_kdl(
            &raw,
            Some(layout_file.display().to_string()),
            None,
            None,
        )
        .map_err(|e| format!("{}", e))
    }
    pub fn saved_at_description(&self) -> String {
        let now = now_in_millis();
        let elapsed = now.saturating_sub(self.meta.saved_at);
        let elapsed = std::time::Duration::from_millis(elapsed);
        format!(
            "{} ago",
            humantime::format_duration(std::time::Duration::from_secs(elapsed.as_secs()))
        )
    }
}

pub fn now_in_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

fn session_archive_dir(settings: &SnapshotSettings, session_name: &str) -> PathBuf {
    settings.dir.join(session_name)
}

/// A stable hash of the *shape* a session_info folder describes: the layout and the pane contents
/// it references, but not `session-metadata.kdl`, which carries connected client counts and
/// timestamps that change on every write and would make every save look like a new shape.
///
/// Used to give a snapshot its short id, to keep the same shape from being archived twice in a row,
/// and to make `snapshot import` idempotent.
fn hash_directory_contents(dir: &Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    let mut hasher = Sha256::new();
    for file in files {
        if let Some(file_name) = file.file_name() {
            if file_name == SNAPSHOT_SIDECAR_FILE_NAME || file_name == SNAPSHOT_METADATA_FILE_NAME {
                continue;
            }
            hasher.update(file_name.to_string_lossy().as_bytes());
        }
        hasher.update(std::fs::read(&file)?);
    }
    let hash = format!("{:x}", hasher.finalize());
    Ok(hash[..8].to_owned())
}

fn copy_directory(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue; // session_info folders are flat
        }
        std::fs::copy(entry.path(), to.join(entry.file_name()))?;
    }
    Ok(())
}

fn count_leaf_panes(pane: &crate::input::layout::TiledPaneLayout) -> usize {
    if pane.children.is_empty() {
        1
    } else {
        pane.children.iter().map(count_leaf_panes).sum()
    }
}

/// The tab and pane counts for the sidecar, taken from the layout rather than from the session
/// metadata: the server deletes `session-metadata.kdl` on its way out, so on the shutdown path the
/// layout is the only thing left to count, and it is what a restore rebuilds anyway.
fn count_tabs_and_panes(session_info_folder: &Path, session_name: &str) -> (usize, usize) {
    let layout_file = session_info_folder.join(SNAPSHOT_LAYOUT_FILE_NAME);
    if let Ok(raw) = std::fs::read_to_string(&layout_file) {
        if let Ok(layout) = crate::input::layout::Layout::from_kdl(
            &raw,
            Some(layout_file.display().to_string()),
            None,
            None,
        ) {
            let tabs = layout.tabs();
            let panes = tabs
                .iter()
                .map(|(_name, tiled, floating)| count_leaf_panes(tiled) + floating.len())
                .sum();
            return (tabs.len(), panes);
        }
    }
    let metadata_file = session_info_folder.join(SNAPSHOT_METADATA_FILE_NAME);
    let Ok(raw) = std::fs::read_to_string(&metadata_file) else {
        return (0, 0);
    };
    match SessionInfo::from_string(&raw, session_name) {
        Ok(session_info) => {
            let panes = session_info
                .panes
                .panes
                .values()
                .map(|panes_in_tab| panes_in_tab.len())
                .sum();
            (session_info.tabs.len(), panes)
        },
        Err(_) => (0, 0),
    }
}

/// Copy a session's live `session_info` folder into the archive.
///
/// Returns `Ok(None)` when there was nothing to archive (no folder, no layout in it, archiving
/// turned off) or when the newest snapshot for this session already holds the identical shape -
/// shutdown and `delete-session` both fire on the same teardown, and one copy of that shape is
/// enough.
pub fn archive_session_info(
    session_name: &str,
    reason: SnapshotReason,
    settings: &SnapshotSettings,
) -> Result<Option<Snapshot>, String> {
    let source = session_info_folder_for_session(session_name);
    archive_session_info_folder(&source, session_name, reason, settings, None)
}

/// The general form of [`archive_session_info`], for folders that are not this machine's live
/// `session_info` folder for that name (the legacy locations `snapshot import` walks).
pub fn archive_session_info_folder(
    source: &Path,
    session_name: &str,
    reason: SnapshotReason,
    settings: &SnapshotSettings,
    imported_from: Option<String>,
) -> Result<Option<Snapshot>, String> {
    if !settings.enabled() {
        return Ok(None);
    }
    if !source.join(SNAPSHOT_LAYOUT_FILE_NAME).exists() {
        return Ok(None);
    }
    let hash = hash_directory_contents(source)
        .map_err(|e| format!("failed to read {}: {}", source.display(), e))?;
    let existing = snapshots_for_session(settings, session_name);
    let is_import = imported_from.is_some();
    let already_archived = if is_import {
        // import is idempotent against the whole archive, so re-running it adds nothing
        existing.iter().any(|snapshot| snapshot.id.ends_with(&hash))
    } else {
        existing
            .last()
            .map_or(false, |newest| newest.id.ends_with(&hash))
    };
    if already_archived {
        return Ok(None);
    }

    let (tabs, panes) = count_tabs_and_panes(source, session_name);
    let meta = SnapshotMeta {
        session_name: session_name.to_owned(),
        saved_at: now_in_millis(),
        zellij_version: VERSION.to_owned(),
        contract_version: CLIENT_SERVER_CONTRACT_VERSION,
        reason,
        tabs,
        panes,
        imported_from,
    };
    let id = format!("{}-{}", meta.saved_at, hash);
    let destination = session_archive_dir(settings, session_name).join(&id);
    copy_directory(source, &destination)
        .map_err(|e| format!("failed to write {}: {}", destination.display(), e))?;
    std::fs::write(
        destination.join(SNAPSHOT_SIDECAR_FILE_NAME),
        meta.to_kdl_string(),
    )
    .map_err(|e| format!("failed to write the snapshot sidecar: {}", e))?;

    prune_session(settings, session_name, settings.limit);

    Ok(Some(Snapshot {
        id,
        session_name: session_name.to_owned(),
        path: destination,
        meta,
    }))
}

/// Every snapshot for one session name, oldest first.
pub fn snapshots_for_session(settings: &SnapshotSettings, session_name: &str) -> Vec<Snapshot> {
    let session_dir = session_archive_dir(settings, session_name);
    let Ok(entries) = std::fs::read_dir(&session_dir) else {
        return vec![];
    };
    let mut snapshots: Vec<Snapshot> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_dir())
        .filter_map(|path| {
            let id = path.file_name()?.to_string_lossy().to_string();
            let raw_meta =
                std::fs::read_to_string(path.join(SNAPSHOT_SIDECAR_FILE_NAME)).unwrap_or_default();
            let meta = SnapshotMeta::from_kdl_string(&raw_meta, session_name);
            Some(Snapshot {
                id,
                session_name: session_name.to_owned(),
                path,
                meta,
            })
        })
        .collect();
    snapshots.sort_by_key(|snapshot| sort_key(snapshot));
    snapshots
}

fn sort_key(snapshot: &Snapshot) -> (u64, String) {
    // the epoch in the directory name is authoritative; the sidecar is a fallback for a snapshot
    // whose directory was renamed by hand
    let epoch_from_id = snapshot
        .id
        .split('-')
        .next()
        .and_then(|epoch| epoch.parse::<u64>().ok());
    (
        epoch_from_id.unwrap_or(snapshot.meta.saved_at),
        snapshot.id.clone(),
    )
}

/// The session names the archive holds snapshots for.
pub fn archived_session_names(settings: &SnapshotSettings) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(&settings.dir) else {
        return vec![];
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// Every snapshot in the archive, or every snapshot for one session, newest first.
pub fn list_snapshots(settings: &SnapshotSettings, session_name: Option<&str>) -> Vec<Snapshot> {
    let mut snapshots: Vec<Snapshot> = match session_name {
        Some(session_name) => snapshots_for_session(settings, session_name),
        None => archived_session_names(settings)
            .iter()
            .flat_map(|session_name| snapshots_for_session(settings, session_name))
            .collect(),
    };
    snapshots.sort_by_key(|snapshot| sort_key(snapshot));
    snapshots.reverse();
    snapshots
}

/// Resolve `latest`, an exact id, or a unique id prefix.
///
/// `latest` is scoped to `session_name` when there is one, since "the newest snapshot of this
/// session" is what asking for it beside a session name means. An id is looked up across the whole
/// archive either way: ids are unique, and restoring one session's snapshot under another name is
/// exactly what makes a snapshot a reusable template.
pub fn resolve_snapshot(
    settings: &SnapshotSettings,
    id: &str,
    session_name: Option<&str>,
) -> Result<Snapshot, String> {
    if id == "latest" {
        return list_snapshots(settings, session_name)
            .into_iter()
            .next()
            .ok_or_else(|| match session_name {
                Some(session_name) => format!("No snapshots for session '{}'.", session_name),
                None => "No snapshots in the archive.".to_owned(),
            });
    }
    let matches: Vec<Snapshot> = list_snapshots(settings, None)
        .into_iter()
        .filter(|snapshot| snapshot.id == id || snapshot.id.starts_with(id))
        .collect();
    match &matches[..] {
        [] => Err(format!("No snapshot matching '{}'.", id)),
        [_] => Ok(matches.into_iter().next().unwrap()),
        _ => {
            if let Some(exact) = matches.iter().find(|snapshot| snapshot.id == id) {
                return Ok(exact.clone());
            }
            let ambiguous: Vec<String> = matches
                .iter()
                .map(|snapshot| format!("{} ({})", snapshot.id, snapshot.session_name))
                .collect();
            Err(format!(
                "'{}' matches more than one snapshot:\n  {}",
                id,
                ambiguous.join("\n  ")
            ))
        },
    }
}

pub fn remove_snapshot(snapshot: &Snapshot) -> std::io::Result<()> {
    std::fs::remove_dir_all(&snapshot.path)?;
    // leave no empty session folder behind
    let _ = std::fs::remove_dir(
        snapshot
            .path
            .parent()
            .unwrap_or_else(|| Path::new("/nonexistent")),
    );
    Ok(())
}

/// Keep the newest `keep` snapshots for one session, delete the rest. Returns what was deleted.
pub fn prune_session(
    settings: &SnapshotSettings,
    session_name: &str,
    keep: usize,
) -> Vec<Snapshot> {
    let snapshots = snapshots_for_session(settings, session_name); // oldest first
    let to_remove = snapshots.len().saturating_sub(keep);
    let mut removed = vec![];
    for snapshot in snapshots.into_iter().take(to_remove) {
        if let Err(e) = remove_snapshot(&snapshot) {
            log::error!("Failed to prune snapshot {}: {:?}", snapshot.id, e);
        } else {
            removed.push(snapshot);
        }
    }
    removed
}

/// Prune every session in the archive.
pub fn prune_all(settings: &SnapshotSettings, keep: usize) -> Vec<Snapshot> {
    archived_session_names(settings)
        .iter()
        .flat_map(|session_name| prune_session(settings, session_name, keep))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session_info_folder(dir: &Path, layout: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(SNAPSHOT_LAYOUT_FILE_NAME), layout).unwrap();
    }

    fn temp_settings(name: &str) -> (SnapshotSettings, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "zellij-snapshot-test-{}-{}",
            name,
            uuid::Uuid::new_v4()
        ));
        let settings = SnapshotSettings {
            dir: root.join("snapshots"),
            limit: DEFAULT_SESSION_SNAPSHOT_LIMIT,
        };
        (settings, root)
    }

    #[test]
    fn sidecar_roundtrips() {
        let meta = SnapshotMeta {
            session_name: "a-session".to_owned(),
            saved_at: 1754251200000,
            zellij_version: "0.44.3-nkmk.4".to_owned(),
            contract_version: 1,
            reason: SnapshotReason::Shutdown,
            tabs: 12,
            panes: 31,
            imported_from: None,
        };
        let parsed = SnapshotMeta::from_kdl_string(&meta.to_kdl_string(), "fallback");
        assert_eq!(parsed.session_name, "a-session");
        assert_eq!(parsed.saved_at, 1754251200000);
        assert_eq!(parsed.zellij_version, "0.44.3-nkmk.4");
        assert_eq!(parsed.contract_version, 1);
        assert_eq!(parsed.reason, SnapshotReason::Shutdown);
        assert_eq!(parsed.tabs, 12);
        assert_eq!(parsed.panes, 31);
    }

    #[test]
    fn sidecar_ignores_unknown_keys_and_defaults_missing_ones() {
        let raw = r#"
            snapshot {
                session_name "a-session"
                reason "from-the-future"
                something_new "that this binary has never heard of"
            }
        "#;
        let parsed = SnapshotMeta::from_kdl_string(raw, "fallback");
        assert_eq!(parsed.session_name, "a-session");
        assert_eq!(
            parsed.reason,
            SnapshotReason::Other("from-the-future".to_owned())
        );
        assert_eq!(parsed.tabs, 0);
        assert_eq!(parsed.zellij_version, "");
    }

    #[test]
    fn sidecar_of_an_unparseable_file_is_all_defaults() {
        let parsed = SnapshotMeta::from_kdl_string("this is not kdl {{{", "fallback");
        assert_eq!(parsed.session_name, "fallback");
        assert_eq!(parsed.saved_at, 0);
    }

    #[test]
    fn archiving_copies_the_folder_and_writes_a_sidecar() {
        let (settings, root) = temp_settings("archive");
        let source = root.join("session_info").join("a-session");
        write_session_info_folder(&source, "layout {\n    tab\n}\n");
        std::fs::write(source.join("initial_contents_1"), "some pane content").unwrap();

        let snapshot = archive_session_info_folder(
            &source,
            "a-session",
            SnapshotReason::Manual,
            &settings,
            None,
        )
        .unwrap()
        .expect("a snapshot should have been written");

        assert!(snapshot.path.join(SNAPSHOT_LAYOUT_FILE_NAME).exists());
        assert!(snapshot.path.join("initial_contents_1").exists());
        assert!(snapshot.path.join(SNAPSHOT_SIDECAR_FILE_NAME).exists());
        assert_eq!(snapshot.meta.reason, SnapshotReason::Manual);
        assert!(snapshot.id.starts_with(&snapshot.meta.saved_at.to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_folder_with_no_layout_is_not_archived() {
        let (settings, root) = temp_settings("no-layout");
        let source = root.join("session_info").join("a-session");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join(SNAPSHOT_METADATA_FILE_NAME), "").unwrap();

        let snapshot = archive_session_info_folder(
            &source,
            "a-session",
            SnapshotReason::Manual,
            &settings,
            None,
        )
        .unwrap();
        assert!(snapshot.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn archiving_is_disabled_by_a_limit_of_zero() {
        let (mut settings, root) = temp_settings("disabled");
        settings.limit = 0;
        let source = root.join("session_info").join("a-session");
        write_session_info_folder(&source, "layout {\n    tab\n}\n");

        let snapshot = archive_session_info_folder(
            &source,
            "a-session",
            SnapshotReason::Manual,
            &settings,
            None,
        )
        .unwrap();
        assert!(snapshot.is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_same_shape_is_not_archived_twice_in_a_row() {
        let (settings, root) = temp_settings("dedupe");
        let source = root.join("session_info").join("a-session");
        write_session_info_folder(&source, "layout {\n    tab\n}\n");

        assert!(archive_session_info_folder(
            &source,
            "a-session",
            SnapshotReason::Shutdown,
            &settings,
            None
        )
        .unwrap()
        .is_some());
        assert!(
            archive_session_info_folder(
                &source,
                "a-session",
                SnapshotReason::Delete,
                &settings,
                None
            )
            .unwrap()
            .is_none(),
            "the teardown pair should leave one snapshot, not two"
        );
        assert_eq!(snapshots_for_session(&settings, "a-session").len(), 1);

        write_session_info_folder(&source, "layout {\n    tab\n    tab\n}\n");
        assert!(
            archive_session_info_folder(
                &source,
                "a-session",
                SnapshotReason::Delete,
                &settings,
                None
            )
            .unwrap()
            .is_some(),
            "a changed shape is a new snapshot"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn retention_prunes_oldest_first() {
        let (mut settings, root) = temp_settings("retention");
        settings.limit = 3;
        let source = root.join("session_info").join("a-session");
        let mut ids = vec![];
        for i in 0..5 {
            write_session_info_folder(&source, &format!("layout {{\n    tab name=\"{}\"\n}}\n", i));
            let snapshot = archive_session_info_folder(
                &source,
                "a-session",
                SnapshotReason::Manual,
                &settings,
                None,
            )
            .unwrap()
            .unwrap();
            ids.push(snapshot.id);
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let kept: Vec<String> = snapshots_for_session(&settings, "a-session")
            .into_iter()
            .map(|snapshot| snapshot.id)
            .collect();
        assert_eq!(kept.len(), 3);
        assert_eq!(kept, ids[2..].to_vec());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn snapshots_resolve_by_latest_exact_id_and_unique_prefix() {
        let (settings, root) = temp_settings("resolve");
        let source = root.join("session_info").join("a-session");
        let mut ids = vec![];
        for i in 0..3 {
            write_session_info_folder(&source, &format!("layout {{\n    tab name=\"{}\"\n}}\n", i));
            ids.push(
                archive_session_info_folder(
                    &source,
                    "a-session",
                    SnapshotReason::Manual,
                    &settings,
                    None,
                )
                .unwrap()
                .unwrap()
                .id,
            );
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert_eq!(
            resolve_snapshot(&settings, "latest", None).unwrap().id,
            *ids.last().unwrap()
        );
        assert_eq!(
            resolve_snapshot(&settings, &ids[0], None).unwrap().id,
            ids[0]
        );
        let prefix = &ids[0][..ids[0].len() - 4];
        assert_eq!(
            resolve_snapshot(&settings, prefix, None).unwrap().id,
            ids[0]
        );
        assert!(resolve_snapshot(&settings, "nope", None).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
