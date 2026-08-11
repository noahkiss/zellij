use humantime::format_duration;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use zellij_tile::prelude::*;

use crate::ui::SessionUiInfo;

/// Which list a row came from.
///
/// A kind on each source rather than a flag on the picker, because the picker is expected to grow a
/// third list - the parked remote-session work is the obvious candidate - and a third kind should
/// cost one variant and one builder, not a rewrite of every `if is_snapshot` in the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerSourceKind {
    LiveSessions,
    Snapshots,
}

/// One pane of a previewed tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewPane {
    pub name: Option<String>,
    pub command: Option<String>,
}

impl PreviewPane {
    /// What to put on the pane's line, in the order a reader recognises it.
    ///
    /// The name is what the user chose to call the pane and wins when there is one; the command is
    /// what it runs. A pane with neither is the default shell and says so, rather than drawing an
    /// empty row that reads as a rendering fault.
    pub fn description(&self) -> String {
        match (self.name.as_ref(), self.command.as_ref()) {
            (Some(name), Some(command)) if name != command => format!("{} ({})", name, command),
            (Some(name), _) => name.clone(),
            (None, Some(command)) => command.clone(),
            (None, None) => String::from("shell"),
        }
    }
}

/// One tab of a previewed entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewTab {
    pub name: Option<String>,
    pub panes: Vec<PreviewPane>,
}

/// A row in the left pane, and everything the right pane draws for it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PickerEntry {
    /// The session name. For a snapshot, the name it was archived under.
    pub name: String,
    /// The archive id, for a snapshot. Empty for a live session.
    pub id: String,
    /// The right-hand column of the row: how old, how big, and why.
    pub summary: String,
    pub tabs: Vec<PreviewTab>,
    /// Why this entry cannot be acted on, if it cannot.
    ///
    /// Carried on the entry rather than decided at keypress time so the row can be marked before
    /// anyone presses anything: a snapshot whose name is taken by a running session, or whose
    /// layout no longer parses, should look different from one that will restore.
    pub blocked: Option<String>,
}

/// One list in the picker.
#[derive(Debug, Clone)]
pub struct PickerSource {
    pub kind: PickerSourceKind,
    pub label: String,
    pub entries: Vec<PickerEntry>,
}

/// The chooser behind `Ctrl+e`: every session shape worth reopening, live or archived, with the
/// tabs and panes of the selected one beside it.
///
/// Sources are a list rather than a pair of fields on purpose - see [`PickerSourceKind`]. Selection
/// runs over the rows of every source flattened together, so adding a source changes what is in the
/// list and nothing about how it is moved through.
#[derive(Default)]
pub struct SnapshotPicker {
    pub sources: Vec<PickerSource>,
    /// The index into the flattened rows of every source.
    pub selected_index: usize,
    pub search_term: String,
    /// The name to restore the selected snapshot under, while that prompt is open.
    pub restore_as: Option<String>,
    /// Whether the server has answered the snapshot request at least once.
    ///
    /// An empty archive before the first answer means "not asked yet"; after it, it means the
    /// archive really is empty. Saying the wrong one of those is the mistake this exists to avoid.
    pub answered: bool,
}

impl SnapshotPicker {
    /// Rebuild both lists from the newest live sessions and the newest archive read.
    ///
    /// Rebuilt whole rather than patched, since both inputs arrive as complete lists, and the
    /// selection is then carried by name so a poll does not move it out from under the user.
    pub fn update(&mut self, live_sessions: &[SessionUiInfo], snapshots: &[SessionSnapshotInfo]) {
        let selected_key = self
            .selected()
            .map(|entry| (entry.name.clone(), entry.id.clone()));
        let live_names: Vec<&str> = live_sessions
            .iter()
            .map(|session| session.name.as_str())
            .collect();
        self.sources = vec![
            PickerSource {
                kind: PickerSourceKind::LiveSessions,
                label: String::from("Live sessions"),
                entries: live_sessions.iter().map(live_entry).collect(),
            },
            PickerSource {
                kind: PickerSourceKind::Snapshots,
                label: String::from("Snapshots"),
                entries: snapshots
                    .iter()
                    .map(|snapshot| snapshot_entry(snapshot, &live_names))
                    .collect(),
            },
        ];
        self.restore_selection(selected_key);
    }
    /// Note that the archive has been read, so an empty list can be reported as empty.
    pub fn mark_answered(&mut self) {
        self.answered = true;
    }
    pub fn clear(&mut self) {
        self.sources.clear();
        self.selected_index = 0;
        self.search_term.clear();
        self.restore_as = None;
        self.answered = false;
    }
    /// The rows the filter leaves, in source order, each with the source it came from.
    pub fn visible_rows(&self) -> Vec<(&PickerSource, &PickerEntry)> {
        let search_term = self.search_term.to_lowercase();
        self.sources
            .iter()
            .flat_map(|source| {
                source
                    .entries
                    .iter()
                    .filter(|entry| {
                        search_term.is_empty() || entry.name.to_lowercase().contains(&search_term)
                    })
                    .map(move |entry| (source, entry))
            })
            .collect()
    }
    pub fn selected(&self) -> Option<&PickerEntry> {
        self.selected_row().map(|(_source, entry)| entry)
    }
    pub fn selected_row(&self) -> Option<(&PickerSource, &PickerEntry)> {
        self.visible_rows().get(self.selected_index).copied()
    }
    pub fn move_selection_down(&mut self) {
        let row_count = self.visible_rows().len();
        if self.selected_index + 1 < row_count {
            self.selected_index += 1;
        }
    }
    pub fn move_selection_up(&mut self) {
        self.selected_index = self.selected_index.saturating_sub(1);
    }
    pub fn update_search_term(&mut self, search_term: String) {
        self.search_term = search_term;
        // a narrower filter can leave fewer rows than the selection was pointing at
        self.clamp_selection();
    }
    fn clamp_selection(&mut self) {
        let row_count = self.visible_rows().len();
        self.selected_index = self.selected_index.min(row_count.saturating_sub(1));
    }
    /// Put the selection back on the row it was on, or on the nearest one that is left.
    ///
    /// The lists are rebuilt on every poll, so an index alone would slide the selection whenever a
    /// session appeared or a snapshot was cut.
    fn restore_selection(&mut self, selected_key: Option<(String, String)>) {
        let Some((name, id)) = selected_key else {
            self.clamp_selection();
            return;
        };
        let position = self
            .visible_rows()
            .iter()
            .position(|(_source, entry)| entry.name == name && entry.id == id);
        match position {
            Some(position) => self.selected_index = position,
            None => self.clamp_selection(),
        }
    }
}

/// How wide the left-hand list gets before the preview takes the rest.
///
/// A share rather than a fixed width, bounded at both ends: a narrow pane must still leave the
/// preview something to draw in, and a very wide one should not give half a terminal to session
/// names. Below `MIN_TWO_COLUMN_WIDTH` there is no preview column at all and the list takes
/// everything - a two-column layout in 40 columns is two unreadable columns.
const MIN_TWO_COLUMN_WIDTH: usize = 60;
const MIN_LIST_WIDTH: usize = 24;
const MAX_LIST_WIDTH: usize = 44;

pub fn list_width(columns: usize) -> usize {
    if columns < MIN_TWO_COLUMN_WIDTH {
        return columns;
    }
    (columns * 2 / 5).clamp(MIN_LIST_WIDTH, MAX_LIST_WIDTH)
}

impl SnapshotPicker {
    pub fn render(&self, rows: usize, columns: usize, x: usize, y: usize) {
        if rows == 0 || columns == 0 {
            return;
        }
        if let Some(restore_as) = self.restore_as.as_ref() {
            self.render_restore_prompt(restore_as, columns, x, y);
            return;
        }
        let list_width = list_width(columns);
        let preview_width = columns.saturating_sub(list_width + 2);
        self.render_search_line(list_width, x, y);
        let list_rows = rows.saturating_sub(3);
        self.render_list(list_rows, list_width, x, y + 2);
        if preview_width > 0 {
            self.render_preview(list_rows, preview_width, x + list_width + 2, y + 2);
        }
    }
    fn render_search_line(&self, columns: usize, x: usize, y: usize) {
        let row_count = self.visible_rows().len();
        let title = format!("Reopen a session ({}): {}_", row_count, self.search_term);
        print_text_with_coordinates(
            Text::new(title).color_range(2, ..16),
            x,
            y,
            Some(columns),
            None,
        );
    }
    fn render_list(&self, rows: usize, columns: usize, x: usize, y: usize) {
        if rows == 0 {
            return;
        }
        let visible_rows = self.visible_rows();
        if visible_rows.is_empty() {
            let notice = if !self.answered {
                "Reading the snapshot archive..."
            } else if self.search_term.is_empty() {
                "No sessions and no snapshots to reopen."
            } else {
                "Nothing matches this filter."
            };
            print_text_with_coordinates(
                Text::new(notice.to_owned()).color_range(3, ..),
                x,
                y,
                Some(columns),
                None,
            );
            return;
        }
        // a header is printed each time the source changes, so a source with no rows left after
        // the filter takes up no room at all
        let mut printed = 0;
        let mut last_source: Option<PickerSourceKind> = None;
        for (index, (source, entry)) in visible_rows.iter().enumerate() {
            if printed >= rows {
                break;
            }
            if last_source != Some(source.kind) {
                if printed + 1 >= rows {
                    break;
                }
                print_text_with_coordinates(
                    Text::new(source.label.clone()).color_range(2, ..),
                    x,
                    y + printed,
                    Some(columns),
                    None,
                );
                printed += 1;
                last_source = Some(source.kind);
            }
            let is_selected = index == self.selected_index;
            let marker = if is_selected { "> " } else { "  " };
            let mut line = Text::new(format!(
                "{}{}",
                marker,
                truncate(&entry.name, columns.saturating_sub(2))
            ))
            .color_range(0, 2..);
            if entry.blocked.is_some() {
                line = line.color_range(3, 2..);
            }
            if is_selected {
                line = line.selected();
            }
            print_text_with_coordinates(line, x, y + printed, Some(columns), None);
            printed += 1;
        }
        let hidden = visible_rows.len().saturating_sub(printed);
        if hidden > 0 && printed > 0 {
            print_text_with_coordinates(
                Text::new(format!("+ {} more", hidden)).color_range(3, ..),
                x,
                y + printed.saturating_sub(1),
                Some(columns),
                None,
            );
        }
    }
    /// The selected entry, tab by tab: what reopening it would actually give back.
    fn render_preview(&self, rows: usize, columns: usize, x: usize, y: usize) {
        let Some(entry) = self.selected() else {
            return;
        };
        let mut printed = 0;
        print_text_with_coordinates(
            Text::new(truncate(&entry.name, columns)).color_range(0, ..),
            x,
            y + printed,
            Some(columns),
            None,
        );
        printed += 1;
        if printed < rows {
            print_text_with_coordinates(
                Text::new(truncate(&entry.summary, columns)).color_range(3, ..),
                x,
                y + printed,
                Some(columns),
                None,
            );
            printed += 1;
        }
        if !entry.id.is_empty() && printed < rows {
            print_text_with_coordinates(
                Text::new(truncate(&entry.id, columns)).color_range(1, ..),
                x,
                y + printed,
                Some(columns),
                None,
            );
            printed += 1;
        }
        if let Some(blocked) = entry.blocked.as_ref() {
            if printed + 1 < rows {
                printed += 1;
                print_text_with_coordinates(
                    Text::new(truncate(blocked, columns)).color_range(3, ..),
                    x,
                    y + printed,
                    Some(columns),
                    None,
                );
                printed += 1;
            }
        }
        if entry.tabs.is_empty() {
            if printed + 1 < rows {
                printed += 1;
                print_text_with_coordinates(
                    Text::new(String::from("No tabs recorded.")).color_range(3, ..),
                    x,
                    y + printed,
                    Some(columns),
                    None,
                );
            }
            return;
        }
        printed += 1;
        for (position, tab) in entry.tabs.iter().enumerate() {
            if printed >= rows {
                break;
            }
            let tab_name = tab
                .name
                .clone()
                .unwrap_or_else(|| format!("Tab #{}", position + 1));
            print_text_with_coordinates(
                Text::new(truncate(&tab_name, columns)).color_range(2, ..),
                x,
                y + printed,
                Some(columns),
                None,
            );
            printed += 1;
            for pane in &tab.panes {
                if printed >= rows {
                    break;
                }
                print_text_with_coordinates(
                    Text::new(format!(
                        "  {}",
                        truncate(&pane.description(), columns.saturating_sub(2))
                    ))
                    .color_range(1, 2..),
                    x,
                    y + printed,
                    Some(columns),
                    None,
                );
                printed += 1;
            }
        }
    }
    fn render_restore_prompt(&self, restore_as: &str, columns: usize, x: usize, y: usize) {
        let name = self
            .selected()
            .map(|entry| entry.name.clone())
            .unwrap_or_default();
        print_text_with_coordinates(
            Text::new(format!("Restore '{}' as a new session", name)).color_range(2, ..),
            x,
            y,
            Some(columns),
            None,
        );
        print_text_with_coordinates(
            Text::new(format!("New session name: {}_", restore_as)).color_range(0, ..18),
            x,
            y + 2,
            Some(columns),
            None,
        );
    }
}

fn truncate(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_owned();
    }
    let mut truncated: String = text.chars().take(budget.saturating_sub(1)).collect();
    truncated.push('…');
    truncated
}

fn live_entry(session: &SessionUiInfo) -> PickerEntry {
    let pane_count: usize = session.tabs.iter().map(|tab| tab.panes.len()).sum();
    let mut summary = format!(
        "{}, {}",
        count(session.tabs.len(), "tab"),
        count(pane_count, "pane")
    );
    if session.is_current_session {
        summary.push_str(" · this session");
    } else if session.connected_users > 0 {
        summary.push_str(&format!(" · {}", count(session.connected_users, "client")));
    }
    PickerEntry {
        name: session.name.clone(),
        id: String::new(),
        summary,
        tabs: session
            .tabs
            .iter()
            .map(|tab| PreviewTab {
                name: Some(tab.name.clone()),
                panes: tab
                    .panes
                    .iter()
                    .map(|pane| PreviewPane {
                        name: Some(pane.name.clone()),
                        command: None,
                    })
                    .collect(),
            })
            .collect(),
        blocked: session
            .is_current_session
            .then(|| String::from("This is the session you are in.")),
    }
}

fn snapshot_entry(snapshot: &SessionSnapshotInfo, live_names: &[&str]) -> PickerEntry {
    let summary = format!(
        "{} · {}, {} · {}",
        elapsed_since(snapshot.saved_at),
        count(snapshot.tab_count, "tab"),
        count(snapshot.pane_count, "pane"),
        snapshot.reason,
    );
    // a layout that will not parse is refused before a restore can fail on it; a name that is
    // taken is refused because attaching to the running session is what would happen instead, and
    // the snapshot would be silently ignored
    let blocked = if snapshot.layout_error.is_some() {
        Some(format!(
            "Its layout does not parse with this build: {}",
            snapshot.layout_error.clone().unwrap_or_default()
        ))
    } else if live_names.contains(&snapshot.session_name.as_str()) {
        Some(format!(
            "Session '{}' is running, so there is nothing to restore into.",
            snapshot.session_name
        ))
    } else {
        None
    };
    PickerEntry {
        name: snapshot.session_name.clone(),
        id: snapshot.id.clone(),
        summary,
        tabs: snapshot
            .tabs
            .iter()
            .map(|tab| PreviewTab {
                name: tab.name.clone(),
                panes: tab
                    .panes
                    .iter()
                    .map(|pane| PreviewPane {
                        name: pane.name.clone(),
                        command: pane.command.clone(),
                    })
                    .collect(),
            })
            .collect(),
        blocked,
    }
}

/// `1 tab` rather than `1 tabs`, which is the sort of thing a reader notices instead of the number.
fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {}", noun)
    } else {
        format!("{} {}s", n, noun)
    }
}

/// How long ago an epoch-millisecond timestamp was, in the same words the rest of the plugin uses.
///
/// A snapshot carries when it was cut rather than how old it is, because the archive is read at
/// arbitrary times and an age would be stale by the time it was drawn. A clock the plugin cannot
/// read gives "just now", which is wrong but harmless beside a row that also names its id.
fn elapsed_since(saved_at_millis: u64) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since_epoch| since_epoch.as_millis() as u64)
        .unwrap_or_default();
    let elapsed = Duration::from_millis(now.saturating_sub(saved_at_millis));
    // seconds are dropped, as everywhere else in this plugin: "1h 3m 12s ago" is noise
    let coarse: Vec<String> = format_duration(Duration::from_secs(elapsed.as_secs()))
        .to_string()
        .split_whitespace()
        .filter(|part| !part.ends_with('s') || part.ends_with("ms"))
        .map(|part| part.to_owned())
        .collect();
    if coarse.is_empty() {
        String::from("just now")
    } else {
        format!("{} ago", coarse.join(" "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(id: &str, session_name: &str) -> SessionSnapshotInfo {
        SessionSnapshotInfo {
            id: id.to_owned(),
            session_name: session_name.to_owned(),
            saved_at: 0,
            reason: "shutdown".to_owned(),
            zellij_version: "0.44.3".to_owned(),
            tab_count: 1,
            pane_count: 2,
            tabs: vec![SnapshotTabInfo {
                name: Some("editing".to_owned()),
                panes: vec![
                    SnapshotPaneInfo::default(),
                    SnapshotPaneInfo {
                        name: None,
                        command: Some("nvim".to_owned()),
                        is_floating: false,
                    },
                ],
            }],
            layout_error: None,
        }
    }

    fn live(name: &str, is_current: bool) -> SessionUiInfo {
        SessionUiInfo {
            name: name.to_owned(),
            tabs: vec![],
            connected_users: 1,
            is_current_session: is_current,
            creation_time: Duration::from_secs(0),
        }
    }

    #[test]
    fn every_source_contributes_rows_in_order() {
        let mut picker = SnapshotPicker::default();
        picker.update(&[live("running", false)], &[snapshot("aaa", "archived")]);
        let kinds: Vec<PickerSourceKind> = picker
            .visible_rows()
            .iter()
            .map(|(source, _entry)| source.kind)
            .collect();
        assert_eq!(
            kinds,
            vec![PickerSourceKind::LiveSessions, PickerSourceKind::Snapshots],
            "the sources are listed in the order the picker holds them"
        );
    }

    #[test]
    fn a_snapshot_of_a_running_session_is_blocked() {
        let mut picker = SnapshotPicker::default();
        picker.update(
            &[live("shared-name", false)],
            &[snapshot("aaa", "shared-name")],
        );
        let rows = picker.visible_rows();
        let snapshot_row = rows
            .iter()
            .find(|(source, _entry)| source.kind == PickerSourceKind::Snapshots)
            .expect("the snapshot is listed");
        assert!(
            snapshot_row.1.blocked.is_some(),
            "restoring over a running session is refused before it can be attempted"
        );
    }

    #[test]
    fn a_snapshot_whose_layout_does_not_parse_is_blocked() {
        let mut picker = SnapshotPicker::default();
        let mut broken = snapshot("aaa", "archived");
        broken.layout_error = Some("unexpected token".to_owned());
        picker.update(&[], &[broken]);
        assert!(picker.selected().unwrap().blocked.is_some());
    }

    #[test]
    fn the_filter_matches_session_names_across_sources() {
        let mut picker = SnapshotPicker::default();
        picker.update(
            &[live("alpha", false), live("beta", false)],
            &[snapshot("aaa", "alpha-archived"), snapshot("bbb", "gamma")],
        );
        picker.update_search_term("alpha".to_owned());
        let names: Vec<String> = picker
            .visible_rows()
            .iter()
            .map(|(_source, entry)| entry.name.clone())
            .collect();
        assert_eq!(names, vec!["alpha", "alpha-archived"]);
    }

    #[test]
    fn the_selection_stays_inside_the_visible_rows() {
        let mut picker = SnapshotPicker::default();
        picker.update(&[live("alpha", false)], &[snapshot("aaa", "beta")]);
        picker.move_selection_down();
        picker.move_selection_down();
        assert_eq!(picker.selected_index, 1, "cannot move past the last row");
        picker.update_search_term("alpha".to_owned());
        assert_eq!(
            picker.selected_index, 0,
            "a narrower filter pulls the selection back"
        );
        picker.move_selection_up();
        assert_eq!(picker.selected_index, 0, "cannot move above the first row");
    }

    #[test]
    fn a_poll_keeps_the_selection_on_the_same_row() {
        let mut picker = SnapshotPicker::default();
        picker.update(&[], &[snapshot("aaa", "one"), snapshot("bbb", "two")]);
        picker.move_selection_down();
        assert_eq!(picker.selected().unwrap().id, "bbb");
        // a new snapshot arrives at the top of the archive
        picker.update(
            &[],
            &[
                snapshot("ccc", "three"),
                snapshot("aaa", "one"),
                snapshot("bbb", "two"),
            ],
        );
        assert_eq!(
            picker.selected().unwrap().id,
            "bbb",
            "the selection follows the row, not the index"
        );
    }

    #[test]
    fn an_empty_archive_is_only_empty_after_the_server_answers() {
        let mut picker = SnapshotPicker::default();
        assert!(!picker.answered);
        picker.mark_answered();
        assert!(picker.answered);
        picker.clear();
        assert!(!picker.answered);
    }

    #[test]
    fn the_preview_describes_a_pane_by_what_a_reader_recognises() {
        assert_eq!(
            PreviewPane {
                name: Some("editor".to_owned()),
                command: Some("nvim".to_owned()),
            }
            .description(),
            "editor (nvim)"
        );
        assert_eq!(
            PreviewPane {
                name: None,
                command: Some("nvim".to_owned()),
            }
            .description(),
            "nvim"
        );
        assert_eq!(
            PreviewPane {
                name: Some("nvim".to_owned()),
                command: Some("nvim".to_owned()),
            }
            .description(),
            "nvim",
            "a name that repeats the command is not worth saying twice"
        );
        assert_eq!(PreviewPane::default().description(), "shell");
    }

    #[test]
    fn the_preview_carries_the_tabs_and_panes_of_a_snapshot() {
        let mut picker = SnapshotPicker::default();
        picker.update(&[], &[snapshot("aaa", "archived")]);
        let entry = picker.selected().unwrap();
        assert_eq!(entry.tabs.len(), 1);
        assert_eq!(entry.tabs[0].name, Some("editing".to_owned()));
        let panes: Vec<String> = entry.tabs[0]
            .panes
            .iter()
            .map(|pane| pane.description())
            .collect();
        assert_eq!(panes, vec!["shell", "nvim"]);
    }

    #[test]
    fn a_narrow_pane_gives_the_whole_width_to_the_list() {
        assert_eq!(
            list_width(40),
            40,
            "two columns in 40 is two unreadable columns"
        );
        assert_eq!(list_width(80), MIN_LIST_WIDTH.max(32));
        assert!(
            list_width(200) <= MAX_LIST_WIDTH,
            "a very wide pane does not give half a terminal to session names"
        );
        assert!(
            list_width(60) >= MIN_LIST_WIDTH,
            "the list keeps room for a name at the narrowest two-column width"
        );
    }

    #[test]
    fn counts_are_singular_when_there_is_one_of_something() {
        assert_eq!(count(1, "tab"), "1 tab");
        assert_eq!(count(0, "tab"), "0 tabs");
        assert_eq!(count(2, "pane"), "2 panes");
    }
}
