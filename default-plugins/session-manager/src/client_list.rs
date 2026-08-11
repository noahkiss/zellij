use zellij_tile::prelude::*;

/// The clients attached to the CURRENT session, and the selection inside that list.
///
/// The server has always known this - `ClientInfo` and `Event::ListClients` shipped long ago and
/// nothing consumed them. Two clients on one session were previously indistinguishable: the only
/// thing the plugin could do about a second client was `Ctrl+x`, which disconnects all of them.
#[derive(Default)]
pub struct ClientList {
    pub clients: Vec<ClientInfo>,
    pub selected_index: usize,
    /// Whether the server has answered at least once.
    ///
    /// An empty list before the first answer means "not asked yet"; after it, it means the server
    /// really reported no clients. Saying the wrong one of those is the mistake this exists to
    /// avoid.
    pub answered: bool,
}

impl ClientList {
    pub fn update(&mut self, mut clients: Vec<ClientInfo>) {
        // the server iterates a map, so the order is not stable between polls - sort it, or the
        // rows shuffle under the selection once a second
        clients.sort_by_key(|c| c.client_id);
        self.clients = clients;
        self.answered = true;
        self.clamp_selection();
    }
    pub fn clear(&mut self) {
        self.clients.clear();
        self.selected_index = 0;
        self.answered = false;
    }
    /// Drop a row we have just detached instead of waiting up to a second for the poll to say so.
    pub fn remove(&mut self, client_id: ClientId) {
        self.clients.retain(|c| c.client_id != client_id);
        self.clamp_selection();
    }
    pub fn move_selection_down(&mut self) {
        if self.clients.is_empty() {
            return;
        }
        if self.selected_index + 1 < self.clients.len() {
            self.selected_index += 1;
        }
    }
    pub fn move_selection_up(&mut self) {
        self.selected_index = self.selected_index.saturating_sub(1);
    }
    pub fn selected(&self) -> Option<&ClientInfo> {
        self.clients.get(self.selected_index)
    }
    fn clamp_selection(&mut self) {
        self.selected_index = self
            .selected_index
            .min(self.clients.len().saturating_sub(1));
    }
    /// Cell count of the smallest client, when marking it would tell the user something.
    ///
    /// A session is sized to its smallest client, so that client is the one throttling everyone
    /// else. `None` when no client reports a size, or when every client reporting one is the same
    /// size - then nothing is being throttled and a highlight would be noise.
    fn throttling_area(&self) -> Option<usize> {
        let mut areas = self.clients.iter().filter_map(|c| c.terminal_area());
        let first = areas.next()?;
        let (smallest, largest) = areas.fold((first, first), |(small, large), area| {
            (small.min(area), large.max(area))
        });
        (smallest < largest).then_some(smallest)
    }
    pub fn render(&self, rows: usize, columns: usize, x: usize, y: usize) {
        if rows == 0 || columns == 0 {
            return;
        }
        let title = format!("Clients attached to this session: {}", self.clients.len());
        print_text_with_coordinates(
            Text::new(title).color_range(2, ..),
            x,
            y,
            Some(columns),
            None,
        );
        if self.clients.is_empty() {
            let notice = if self.answered {
                "The server reports no attached clients."
            } else {
                "Asking the server for the client list..."
            };
            print_text_with_coordinates(
                Text::new(notice.to_owned()).color_range(3, ..),
                x,
                y + 2,
                Some(columns),
                None,
            );
            return;
        }
        let max_rows = rows.saturating_sub(3);
        let throttling_area = self.throttling_area();
        let mut table =
            Table::new().add_row(vec![" ", "Client", "Focused pane", "Size", "Running"]);
        for (i, client) in self.clients.iter().enumerate() {
            if i >= max_rows {
                break;
            }
            let is_selected = i == self.selected_index;
            let arrow_cell = if is_selected {
                Text::new("<↓↑>").selected().color_range(3, ..)
            } else {
                Text::new("    ")
            };
            let client_text = if client.is_current_client {
                format!("{} (this client)", client.client_id)
            } else {
                format!("{}", client.client_id)
            };
            let id_len = client.client_id.to_string().chars().count();
            let mut client_cell = Text::new(client_text).color_range(0, ..id_len);
            if client.is_current_client {
                // the marker has to survive a glance: this is the row the user is typing in, and
                // the one row `Detach` refuses
                client_cell = client_cell.color_range(2, id_len..);
            }
            let mut pane_cell = Text::new(pane_description(&client.pane_id)).color_range(1, ..);
            // the smallest client sizes the whole session, so it gets the same emphasis colour
            // the current client marker uses - it is the row a user is looking for
            let is_throttling =
                throttling_area.is_some() && client.terminal_area() == throttling_area;
            let mut size_cell = Text::new(size_description(client.terminal_size));
            size_cell = if is_throttling {
                size_cell.color_range(2, ..)
            } else {
                size_cell.color_range(0, ..)
            };
            let mut command_cell =
                Text::new(truncate(&client.running_command, command_budget(columns)))
                    .color_range(3, ..);
            if is_selected {
                client_cell = client_cell.selected();
                pane_cell = pane_cell.selected();
                size_cell = size_cell.selected();
                command_cell = command_cell.selected();
            }
            table = table.add_styled_row(vec![
                arrow_cell,
                client_cell,
                pane_cell,
                size_cell,
                command_cell,
            ]);
        }
        print_table_with_coordinates(table, x, y + 2, Some(columns), None);
        let hidden = self.clients.len().saturating_sub(max_rows);
        if hidden > 0 {
            print_text_with_coordinates(
                Text::new(format!(
                    "{} more client(s) hidden: the pane is too short.",
                    hidden
                ))
                .color_range(3, ..),
                x,
                y + 2 + max_rows.saturating_add(1),
                Some(columns),
                None,
            );
        }
    }
}

/// How much room the command column gets before it has to be cut.
///
/// The other four columns are short and bounded; the command is the one that can run to hundreds
/// of characters and push the table past the pane.
fn command_budget(columns: usize) -> usize {
    columns.saturating_sub(45).max(10)
}

/// A client's terminal size, as a human writes one: columns first.
///
/// Blank rather than a placeholder when the server has no size for the client - an empty cell
/// reads as "not known", where a `0x0` would read as a real and alarming measurement.
fn size_description(terminal_size: Option<Size>) -> String {
    match terminal_size {
        Some(size) => format!("{}x{}", size.cols, size.rows),
        None => String::new(),
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

/// What the client is looking at, in the two terms a user can act on.
fn pane_description(pane_id: &PaneId) -> String {
    match pane_id {
        PaneId::Terminal(id) => format!("terminal {}", id),
        PaneId::Plugin(id) => format!("plugin {}", id),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(id: ClientId, is_current: bool) -> ClientInfo {
        ClientInfo::new(
            id,
            PaneId::Terminal(id as u32),
            "zsh".to_owned(),
            is_current,
        )
    }

    fn sized_client(id: ClientId, cols: usize, rows: usize) -> ClientInfo {
        client(id, false).with_terminal_size(Some(Size { rows, cols }))
    }

    #[test]
    fn clients_are_ordered_by_id() {
        let mut list = ClientList::default();
        list.update(vec![client(3, false), client(1, true), client(2, false)]);
        let ids: Vec<ClientId> = list.clients.iter().map(|c| c.client_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
    }

    #[test]
    fn an_empty_list_is_only_answered_after_the_server_answers() {
        let mut list = ClientList::default();
        assert!(!list.answered);
        list.update(vec![]);
        assert!(list.answered);
        list.clear();
        assert!(!list.answered);
    }

    #[test]
    fn the_selection_stays_inside_the_list() {
        let mut list = ClientList::default();
        list.update(vec![client(1, true), client(2, false)]);
        list.move_selection_down();
        list.move_selection_down();
        assert_eq!(list.selected_index, 1, "cannot move past the last client");
        list.move_selection_up();
        list.move_selection_up();
        assert_eq!(list.selected_index, 0, "cannot move above the first client");
    }

    #[test]
    fn a_shorter_list_pulls_the_selection_back() {
        let mut list = ClientList::default();
        list.update(vec![client(1, true), client(2, false), client(3, false)]);
        list.move_selection_down();
        list.move_selection_down();
        list.update(vec![client(1, true)]);
        assert_eq!(list.selected_index, 0);
        assert_eq!(list.selected().map(|c| c.client_id), Some(1));
    }

    #[test]
    fn a_detached_client_leaves_the_list_at_once() {
        let mut list = ClientList::default();
        list.update(vec![client(1, true), client(2, false)]);
        list.move_selection_down();
        list.remove(2);
        assert_eq!(list.clients.len(), 1);
        assert_eq!(
            list.selected().map(|c| c.client_id),
            Some(1),
            "the selection follows the rows that are left"
        );
    }

    #[test]
    fn the_command_column_is_cut_rather_than_wrapped() {
        let long = "a".repeat(200);
        let cut = truncate(&long, command_budget(80));
        assert_eq!(cut.chars().count(), command_budget(80));
        assert!(cut.ends_with('…'));
        assert_eq!(truncate("zsh", command_budget(80)), "zsh");
    }

    #[test]
    fn a_narrow_pane_still_leaves_room_for_a_command() {
        assert_eq!(command_budget(20), 10);
    }

    #[test]
    fn a_size_is_written_columns_first() {
        assert_eq!(
            size_description(Some(Size {
                rows: 40,
                cols: 160
            })),
            "160x40"
        );
    }

    #[test]
    fn an_unknown_size_is_blank_rather_than_zero() {
        assert_eq!(size_description(None), "");
    }

    #[test]
    fn the_smallest_client_is_the_one_marked() {
        let mut list = ClientList::default();
        list.update(vec![
            sized_client(1, 160, 40),
            sized_client(2, 80, 24),
            sized_client(3, 200, 50),
        ]);
        assert_eq!(list.throttling_area(), Some(80 * 24));
    }

    #[test]
    fn clients_of_one_size_throttle_nobody() {
        let mut list = ClientList::default();
        list.update(vec![sized_client(1, 160, 40), sized_client(2, 160, 40)]);
        assert_eq!(
            list.throttling_area(),
            None,
            "nothing is being shrunk, so nothing is marked"
        );
    }

    #[test]
    fn a_client_of_unknown_size_never_wins_the_comparison() {
        let mut list = ClientList::default();
        list.update(vec![client(1, true), sized_client(2, 80, 24)]);
        assert_eq!(
            list.throttling_area(),
            None,
            "one known size is not a comparison"
        );
        list.update(vec![
            client(1, true),
            sized_client(2, 80, 24),
            sized_client(3, 160, 40),
        ]);
        assert_eq!(list.throttling_area(), Some(80 * 24));
    }
}
