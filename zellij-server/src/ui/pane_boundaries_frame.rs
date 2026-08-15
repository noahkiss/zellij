use crate::output::CharacterChunk;
use crate::panes::{
    AnsiCode, CharacterStyles, RcCharacterStyles, TerminalCharacter, EMPTY_TERMINAL_CHARACTER,
};
use crate::tab::GuestChoiceIndicator;
use crate::ui::boundaries::boundary_type;
use crate::ui::hint_text::{
    exit_code_segments, hover_segments, rerun_segments, resize_segments, HintExitStatus, HintLevel,
    HintSegment, HintTier,
};
use crate::ClientId;
use zellij_utils::data::{client_id_to_colors, NoteColor, PaletteColor, Style};
use zellij_utils::errors::prelude::*;
use zellij_utils::pane_size::{Offset, PaneGeom, Viewport};
use zellij_utils::position::Position;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

fn styled_characters(
    characters: &str,
    style_character: impl Fn(&mut CharacterStyles),
) -> Vec<TerminalCharacter> {
    characters
        .chars()
        .map(|character| {
            let mut styles = RcCharacterStyles::reset();
            styles.update(|styles| style_character(styles));
            TerminalCharacter::new_styled(character, styles)
        })
        .collect()
}

fn foreground_color(characters: &str, color: Option<PaletteColor>) -> Vec<TerminalCharacter> {
    styled_characters(characters, |styles| {
        styles.bold = Some(AnsiCode::On);
        if let Some(palette_color) = color {
            styles.foreground = Some(AnsiCode::from(palette_color));
        }
    })
}

/// How much of the title row an element takes, separator included, and nothing for one that is not
/// there.
fn taken(element: &Option<(Vec<TerminalCharacter>, usize)>) -> usize {
    element.as_ref().map(|(_, length)| length + 1).unwrap_or(0)
}

fn dimmed_foreground_color(
    characters: &str,
    color: Option<PaletteColor>,
) -> Vec<TerminalCharacter> {
    styled_characters(characters, |styles| {
        styles.dim = Some(AnsiCode::On);
        if let Some(palette_color) = color {
            styles.foreground = Some(AnsiCode::from(palette_color));
        }
    })
}

fn background_color(characters: &str, color: Option<PaletteColor>) -> Vec<TerminalCharacter> {
    styled_characters(characters, |styles| {
        if let Some(palette_color) = color {
            styles.background = Some(AnsiCode::from(palette_color));
            styles.bold(Some(AnsiCode::On));
        }
    })
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ExitStatus {
    Code(i32),
    Exited,
}

#[derive(Clone, PartialEq)]
pub struct StackListEntry {
    pub width: usize,
    pub label: String,
    pub is_selected: bool,
    pub is_emphasized: bool,
    pub stack_is_focused: bool,
}

pub struct FrameParams {
    pub focused_client: Option<ClientId>,
    pub is_main_client: bool, // more accurately: is_focused_for_main_client
    pub other_focused_clients: Vec<ClientId>,
    pub style: Style,
    pub color: Option<PaletteColor>,
    pub other_cursors_exist_in_session: bool,
    pub pane_is_stacked_under: bool,
    pub pane_is_stacked_over: bool,
    pub pane_is_stacked: bool,
    pub should_draw_pane_frames: bool,
    pub pane_is_floating: bool,
    pub content_offset: Offset,
    pub mouse_is_hovering_over_pane: bool,
    pub pane_is_selectable: bool,
    pub show_help_text: bool,
    pub highlight_tooltip: Option<String>,
    pub omit_title: bool,
    pub frame_geom_override: Option<PaneGeom>,
    pub stack_list_entry: Option<StackListEntry>,
    pub blank_title: bool,
    // fork addition: the handle this pane is addressed by, drawn at the right of the title row
    pub pane_handle: String,
    // fork addition: the note somebody left on this pane, drawn beside the handle
    pub pane_note: Option<(String, NoteColor)>,
    // fork addition: `pane_frame_style top_only`
    pub top_only_frames: bool,
    pub mouse_scroll_resize: bool,
    pub mouse_hover_tips: bool,
    pub dimmed: bool,
    pub guest_choice_indicator: Option<GuestChoiceIndicator>,
}

#[derive(Default, PartialEq)]
pub struct PaneFrame {
    pub geom: Viewport,
    pub title: String,
    pub scroll_position: (usize, usize), // (position, length)
    pub style: Style,
    pub color: Option<PaletteColor>,
    pub focused_client: Option<ClientId>,
    pub is_main_client: bool,
    pub other_cursors_exist_in_session: bool,
    pub other_focused_clients: Vec<ClientId>,
    exit_status: Option<ExitStatus>,
    is_first_run: bool,
    pane_is_stacked_over: bool,
    pane_is_stacked_under: bool,
    pane_is_stacked: bool,
    should_draw_pane_frames: bool,
    is_pinned: bool,
    is_floating: bool,
    content_offset: Offset,
    mouse_is_hovering_over_pane: bool,
    is_selectable: bool,
    show_help_text: bool,
    highlight_tooltip: Option<String>,
    omit_title: bool,
    stack_list_entry: Option<StackListEntry>,
    color_override: Option<PaletteColor>,
    mouse_scroll_resize: bool,
    mouse_hover_tips: bool,
    dimmed: bool,
    guest_choice_indicator: Option<GuestChoiceIndicator>,
    // fork addition: the handle this pane is addressed by, drawn at the right of the title row
    pane_handle: String,
    // fork addition: the note somebody left on this pane, drawn beside the handle
    pane_note: Option<(String, NoteColor)>,
    // fork addition: `pane_frame_style top_only`
    top_only_frames: bool,
}

impl PaneFrame {
    pub fn new(
        geom: Viewport,
        scroll_position: (usize, usize),
        main_title: String,
        frame_params: FrameParams,
    ) -> Self {
        PaneFrame {
            geom,
            title: main_title,
            scroll_position,
            style: frame_params.style,
            color: frame_params.color,
            focused_client: frame_params.focused_client,
            is_main_client: frame_params.is_main_client,
            other_focused_clients: frame_params.other_focused_clients,
            other_cursors_exist_in_session: frame_params.other_cursors_exist_in_session,
            exit_status: None,
            is_first_run: false,
            pane_is_stacked_over: frame_params.pane_is_stacked_over,
            pane_is_stacked_under: frame_params.pane_is_stacked_under,
            pane_is_stacked: frame_params.pane_is_stacked,
            should_draw_pane_frames: frame_params.should_draw_pane_frames,
            is_pinned: false,
            is_floating: frame_params.pane_is_floating,
            content_offset: frame_params.content_offset,
            mouse_is_hovering_over_pane: frame_params.mouse_is_hovering_over_pane,
            is_selectable: frame_params.pane_is_selectable,
            show_help_text: frame_params.show_help_text,
            highlight_tooltip: frame_params.highlight_tooltip,
            omit_title: frame_params.omit_title,
            stack_list_entry: frame_params.stack_list_entry,
            color_override: None,
            mouse_scroll_resize: frame_params.mouse_scroll_resize,
            mouse_hover_tips: frame_params.mouse_hover_tips,
            dimmed: frame_params.dimmed,
            guest_choice_indicator: frame_params.guest_choice_indicator,
            pane_handle: frame_params.pane_handle,
            pane_note: frame_params.pane_note,
            top_only_frames: frame_params.top_only_frames,
        }
    }
    pub fn is_pinned(mut self, is_pinned: bool) -> Self {
        self.is_pinned = is_pinned;
        self
    }
    pub fn add_exit_status(&mut self, exit_status: Option<i32>) {
        self.exit_status = match exit_status {
            Some(exit_status) => Some(ExitStatus::Code(exit_status)),
            None => Some(ExitStatus::Exited),
        };
    }
    pub fn indicate_first_run(&mut self) {
        self.is_first_run = true;
    }
    pub fn override_color(&mut self, color: PaletteColor) {
        self.color = Some(color);
        self.color_override = Some(color);
    }
    fn client_cursor(&self, client_id: ClientId) -> Vec<TerminalCharacter> {
        let color = client_id_to_colors(client_id, self.style.colors.multiplayer_user_colors);
        background_color(" ", color.map(|c| c.0))
    }
    fn get_corner(&self, corner: &'static str) -> &'static str {
        let corner = if !self.should_draw_pane_frames
            && (corner == boundary_type::TOP_LEFT || corner == boundary_type::TOP_RIGHT)
        {
            boundary_type::HORIZONTAL
        } else if self.pane_is_stacked_under && corner == boundary_type::TOP_RIGHT {
            boundary_type::BOTTOM_RIGHT
        } else if self.pane_is_stacked_under && corner == boundary_type::TOP_LEFT {
            boundary_type::BOTTOM_LEFT
        } else {
            corner
        };
        if self.style.rounded_corners {
            match corner {
                boundary_type::TOP_RIGHT => boundary_type::TOP_RIGHT_ROUND,
                boundary_type::TOP_LEFT => boundary_type::TOP_LEFT_ROUND,
                boundary_type::BOTTOM_RIGHT => boundary_type::BOTTOM_RIGHT_ROUND,
                boundary_type::BOTTOM_LEFT => boundary_type::BOTTOM_LEFT_ROUND,
                _ => corner,
            }
        } else {
            corner
        }
    }
    fn render_title_right_side(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        // fork addition: the handle sits at the far right of the title row, the mirror of the
        // title at the far left, with the note beside it. Room is offered in that order, so a
        // narrowing frame loses the note first, then the handle, and never the title
        let indications = self.render_title_right_side_inner(max_length);
        let handle = self.render_handle_indication(max_length.saturating_sub(taken(&indications)));
        let note = self.render_note_indication(
            max_length.saturating_sub(taken(&indications) + taken(&handle)),
        );
        // the pin checkbox is a click target, and `clicked_on_pinned` finds it by counting back
        // from the right edge of the pane. So on a pane that draws one, the pin keeps the right
        // edge and everything else goes to its left - otherwise the handle pushes the checkbox out
        // from under the place a click looks for it, and the button a user can see stops working
        let order = if self.draws_pin_indication() {
            vec![note, handle, indications]
        } else {
            vec![indications, note, handle]
        };
        self.join_right_side(order)
    }
    /// Joins what the title row's right side holds, with the separator those elements already use.
    fn join_right_side(
        &self,
        elements: Vec<Option<(Vec<TerminalCharacter>, usize)>>,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let mut characters: Vec<TerminalCharacter> = Vec::new();
        let mut length = 0;
        for (mut element, element_length) in elements.into_iter().flatten() {
            if !characters.is_empty() {
                characters.append(&mut foreground_color("|", self.color));
                length += 1;
            }
            characters.append(&mut element);
            length += element_length;
        }
        (!characters.is_empty()).then_some((characters, length))
    }
    /// The colour a note of this meaning is drawn in, taken from the theme rather than chosen.
    ///
    /// A note is drawn inside somebody else's colour scheme, so the four meanings map onto colours
    /// the theme has already picked for exactly those things.
    fn note_color(&self, note_color: NoteColor) -> Option<PaletteColor> {
        let colors = &self.style.colors;
        Some(match note_color {
            NoteColor::Error => colors.exit_code_error.base,
            NoteColor::Ok => colors.exit_code_success.base,
            NoteColor::Warn => colors.text_unselected.emphasis_0,
            NoteColor::Info => colors.text_unselected.emphasis_1,
        })
    }
    /// The note left on this pane, cut to fit.
    ///
    /// Unlike the handle it IS truncated: a note is prose, and half a sentence still says
    /// something, while half an address reaches no pane.
    fn render_note_indication(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        let (note, note_color) = self
            .pane_note
            .as_ref()
            .filter(|(note, _)| !note.is_empty())?;
        // the space either side of it, which is what separates it from the handle beside it
        let room = max_length.checked_sub(2).filter(|room| *room > 0)?;
        let shown = if note.width() <= room {
            note.clone()
        } else {
            let mut truncated = String::new();
            let mut width = 0;
            for character in note.chars() {
                let character_width = character.width().unwrap_or(0);
                if width + character_width > room.saturating_sub(1) {
                    break;
                }
                truncated.push(character);
                width += character_width;
            }
            format!("{}…", truncated)
        };
        let text = format!(" {} ", shown);
        let length = text.width();
        (length <= max_length).then(|| {
            (
                foreground_color(&text, self.note_color(*note_color)),
                length,
            )
        })
    }
    /// Whether this frame draws the pin checkbox, which is a click target and so owns the right
    /// edge of the title row - see `clicked_on_pinned`.
    fn draws_pin_indication(&self) -> bool {
        self.is_floating && self.is_selectable
    }
    /// The pane's handle, whole or not at all.
    ///
    /// Unlike the title, a handle is never truncated: it is an address, and half an address reaches
    /// no pane. A frame row too narrow to hold it simply does not show it, which is also why the
    /// handle is the last thing offered room - the title, which a human reads, always wins.
    fn render_handle_indication(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        if self.pane_handle.is_empty() {
            return None;
        }
        let full_indication = format!(" {} ", self.pane_handle);
        let full_indication_len = full_indication.width();
        (full_indication_len <= max_length).then(|| {
            (
                foreground_color(&full_indication, self.color),
                full_indication_len,
            )
        })
    }
    fn render_title_right_side_inner(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        // string and length because of color
        let has_scroll = self.scroll_position.0 > 0 || self.scroll_position.1 > 0;
        if has_scroll && self.is_selectable {
            // TODO: don't show SCROLL at all for plugins
            let pin_indication = if self.draws_pin_indication() {
                self.render_pinned_indication(max_length)
            } else {
                None
            }; // no pin indication for tiled panes
            let space_for_scroll_indication = pin_indication
                .as_ref()
                .map(|(_, length)| max_length.saturating_sub(*length + 1))
                .unwrap_or(max_length);
            let scroll_indication = self.render_scroll_indication(space_for_scroll_indication);
            match (pin_indication, scroll_indication) {
                (
                    Some((mut pin_indication, pin_indication_len)),
                    Some((mut scroll_indication, scroll_indication_len)),
                ) => {
                    let mut characters: Vec<_> = scroll_indication.drain(..).collect();
                    let mut separator = foreground_color(&format!("|"), self.color);
                    characters.append(&mut separator);
                    characters.append(&mut pin_indication);
                    Some((characters, pin_indication_len + scroll_indication_len + 1))
                },
                (Some(pin_indication), None) => Some(pin_indication),
                (None, Some(scroll_indication)) => Some(scroll_indication),
                _ => None,
            }
        } else if self.draws_pin_indication() {
            self.render_pinned_indication(max_length)
        } else {
            None
        }
    }
    fn render_scroll_indication(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let prefix = " SCROLL: ";
        let full_indication = format!(" {}/{} ", self.scroll_position.0, self.scroll_position.1);
        let short_indication = format!(" {} ", self.scroll_position.0);
        let full_indication_len = full_indication.chars().count();
        let short_indication_len = short_indication.chars().count();
        let prefix_len = prefix.chars().count();
        if prefix_len + full_indication_len <= max_length {
            Some((
                foreground_color(&format!("{}{}", prefix, full_indication), self.color),
                prefix_len + full_indication_len,
            ))
        } else if full_indication_len <= max_length {
            Some((
                foreground_color(&full_indication, self.color),
                full_indication_len,
            ))
        } else if short_indication_len <= max_length {
            Some((
                foreground_color(&short_indication, self.color),
                short_indication_len,
            ))
        } else {
            None
        }
    }
    fn render_pinned_indication(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let is_checked = if self.is_pinned { '+' } else { ' ' };
        let full_indication = format!(" PIN [{}] ", is_checked);
        let full_indication_len = full_indication.chars().count();
        if full_indication_len <= max_length {
            Some((
                foreground_color(&full_indication, self.color),
                full_indication_len,
            ))
        } else {
            None
        }
    }
    fn render_my_focus(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        let mut left_separator = foreground_color(boundary_type::VERTICAL_LEFT, self.color);
        let mut right_separator = foreground_color(boundary_type::VERTICAL_RIGHT, self.color);
        let full_indication_text = "MY FOCUS";
        let mut full_indication = vec![];
        full_indication.append(&mut left_separator);
        full_indication.push(EMPTY_TERMINAL_CHARACTER);
        full_indication.append(&mut foreground_color(full_indication_text, self.color));
        full_indication.push(EMPTY_TERMINAL_CHARACTER);
        full_indication.append(&mut right_separator);
        let full_indication_len = full_indication_text.width() + 4; // 2 for separators 2 for padding
        let short_indication_text = "ME";
        let mut short_indication = vec![];
        short_indication.append(&mut left_separator);
        short_indication.push(EMPTY_TERMINAL_CHARACTER);
        short_indication.append(&mut foreground_color(short_indication_text, self.color));
        short_indication.push(EMPTY_TERMINAL_CHARACTER);
        short_indication.append(&mut right_separator);
        let short_indication_len = short_indication_text.width() + 4; // 2 for separators 2 for padding
        if full_indication_len <= max_length {
            Some((full_indication, full_indication_len))
        } else if short_indication_len <= max_length {
            Some((short_indication, short_indication_len))
        } else {
            None
        }
    }
    fn render_my_and_others_focus(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let mut left_separator = foreground_color(boundary_type::VERTICAL_LEFT, self.color);
        let mut right_separator = foreground_color(boundary_type::VERTICAL_RIGHT, self.color);
        let full_indication_text = "MY FOCUS AND:";
        let short_indication_text = "+";
        let mut full_indication = foreground_color(full_indication_text, self.color);
        let mut full_indication_len = full_indication_text.width();
        let mut short_indication = foreground_color(short_indication_text, self.color);
        let mut short_indication_len = short_indication_text.width();
        for client_id in &self.other_focused_clients {
            let mut text = self.client_cursor(*client_id);
            full_indication_len += 2;
            full_indication.push(EMPTY_TERMINAL_CHARACTER);
            full_indication.append(&mut text.clone());
            short_indication_len += 2;
            short_indication.push(EMPTY_TERMINAL_CHARACTER);
            short_indication.append(&mut text);
        }
        if full_indication_len + 4 <= max_length {
            // 2 for separators, 2 for padding
            let mut ret = vec![];
            ret.append(&mut left_separator);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut full_indication);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut right_separator);
            Some((ret, full_indication_len + 4))
        } else if short_indication_len + 4 <= max_length {
            // 2 for separators, 2 for padding
            let mut ret = vec![];
            ret.append(&mut left_separator);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut short_indication);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut right_separator);
            Some((ret, short_indication_len + 4))
        } else {
            None
        }
    }
    fn render_other_focused_users(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let mut left_separator = foreground_color(boundary_type::VERTICAL_LEFT, self.color);
        let mut right_separator = foreground_color(boundary_type::VERTICAL_RIGHT, self.color);
        let full_indication_text = if self.other_focused_clients.len() == 1 {
            "FOCUSED USER:"
        } else {
            "FOCUSED USERS:"
        };
        let middle_indication_text = "U:";
        let mut full_indication = foreground_color(full_indication_text, self.color);
        let mut full_indication_len = full_indication_text.width();
        let mut middle_indication = foreground_color(middle_indication_text, self.color);
        let mut middle_indication_len = middle_indication_text.width();
        let mut short_indication = vec![];
        let mut short_indication_len = 0;
        for client_id in &self.other_focused_clients {
            let mut text = self.client_cursor(*client_id);
            full_indication_len += 2;
            full_indication.push(EMPTY_TERMINAL_CHARACTER);
            full_indication.append(&mut text.clone());
            middle_indication_len += 2;
            middle_indication.push(EMPTY_TERMINAL_CHARACTER);
            middle_indication.append(&mut text.clone());
            short_indication_len += 2;
            short_indication.push(EMPTY_TERMINAL_CHARACTER);
            short_indication.append(&mut text);
        }
        if full_indication_len + 4 <= max_length {
            // 2 for separators, 2 for padding
            let mut ret = vec![];
            ret.append(&mut left_separator);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut full_indication);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut right_separator);
            Some((ret, full_indication_len + 4))
        } else if middle_indication_len + 4 <= max_length {
            // 2 for separators, 2 for padding
            let mut ret = vec![];
            ret.append(&mut left_separator);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut middle_indication);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut right_separator);
            Some((ret, middle_indication_len + 4))
        } else if short_indication_len + 3 <= max_length {
            // 2 for separators, 1 for padding
            let mut ret = vec![];
            ret.append(&mut left_separator);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut short_indication);
            ret.push(EMPTY_TERMINAL_CHARACTER);
            ret.append(&mut right_separator);
            Some((ret, short_indication_len + 3))
        } else {
            None
        }
    }
    fn render_title_middle(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        // string and length because of color
        let renders_exit_status_in_title = self.pane_is_stacked_under
            || self.pane_is_stacked_over
            || !self.should_draw_pane_frames;
        if renders_exit_status_in_title && self.exit_status.is_some() {
            let (first_part, first_part_len) = self.first_exited_held_title_part_full();
            return if first_part_len <= max_length {
                Some((first_part, first_part_len))
            } else {
                None
            };
        }
        if self.is_main_client
            && self.other_focused_clients.is_empty()
            && !self.other_cursors_exist_in_session
        {
            None
        } else if self.is_main_client
            && self.other_focused_clients.is_empty()
            && self.other_cursors_exist_in_session
        {
            self.render_my_focus(max_length)
        } else if self.is_main_client && !self.other_focused_clients.is_empty() {
            self.render_my_and_others_focus(max_length)
        } else if !self.other_focused_clients.is_empty() {
            self.render_other_focused_users(max_length)
        } else {
            None
        }
    }
    fn render_title_left_side(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        let middle_truncated_sign = "[..]";
        let middle_truncated_sign_long = "[...]";
        let full_text = format!(" {} ", &self.title);
        if max_length <= 6 || self.title.is_empty() {
            None
        } else if full_text.width() <= max_length {
            Some((foreground_color(&full_text, self.color), full_text.width()))
        } else {
            let length_of_each_half = (max_length - middle_truncated_sign.width()) / 2;

            let mut first_part: String = String::new();
            for char in full_text.chars() {
                if first_part.width() + char.width().unwrap_or(0) > length_of_each_half {
                    break;
                } else {
                    first_part.push(char);
                }
            }

            let mut second_part: String = String::new();
            for char in full_text.chars().rev() {
                if second_part.width() + char.width().unwrap_or(0) > length_of_each_half {
                    break;
                } else {
                    second_part.insert(0, char);
                }
            }

            let (title_left_side, title_length) = if first_part.width()
                + middle_truncated_sign.width()
                + second_part.width()
                < max_length
            {
                // this means we lost 1 character when dividing the total length into halves
                (
                    format!(
                        "{}{}{}",
                        first_part, middle_truncated_sign_long, second_part
                    ),
                    first_part.width() + middle_truncated_sign_long.width() + second_part.width(),
                )
            } else {
                (
                    format!("{}{}{}", first_part, middle_truncated_sign, second_part),
                    first_part.width() + middle_truncated_sign.width() + second_part.width(),
                )
            };
            Some((foreground_color(&title_left_side, self.color), title_length))
        }
    }
    fn three_part_title_line(
        &self,
        mut left_side: Vec<TerminalCharacter>,
        left_side_len: &usize,
        mut middle: Vec<TerminalCharacter>,
        middle_len: &usize,
        mut right_side: Vec<TerminalCharacter>,
        right_side_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut title_line = vec![];
        let left_side_start_position = self.geom.x + 1;
        let middle_start_position = self.geom.x + (total_title_length / 2) - (middle_len / 2) + 1;
        let right_side_start_position =
            (self.geom.x + self.geom.cols - 1).saturating_sub(*right_side_len);

        let mut col = self.geom.x;
        loop {
            if col == self.geom.x {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_LEFT),
                    self.color,
                ));
            } else if col == self.geom.x + self.geom.cols - 1 {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_RIGHT),
                    self.color,
                ));
            } else if col == left_side_start_position {
                title_line.append(&mut left_side);
                col += left_side_len;
                continue;
            } else if col == middle_start_position {
                title_line.append(&mut middle);
                col += middle_len;
                continue;
            } else if col == right_side_start_position {
                title_line.append(&mut right_side);
                col += right_side_len;
                continue;
            } else {
                title_line.append(&mut foreground_color(boundary_type::HORIZONTAL, self.color));
            }
            if col == self.geom.x + self.geom.cols - 1 {
                break;
            }
            col += 1;
        }
        title_line
    }
    fn left_and_middle_title_line(
        &self,
        mut left_side: Vec<TerminalCharacter>,
        left_side_len: &usize,
        mut middle: Vec<TerminalCharacter>,
        middle_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut title_line = vec![];
        let left_side_start_position = self.geom.x + 1;
        let middle_start_position = self.geom.x + (total_title_length / 2) - (*middle_len / 2) + 1;

        let mut col = self.geom.x;
        loop {
            if col == self.geom.x {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_LEFT),
                    self.color,
                ));
            } else if col == self.geom.x + self.geom.cols - 1 {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_RIGHT),
                    self.color,
                ));
            } else if col == left_side_start_position {
                title_line.append(&mut left_side);
                col += *left_side_len;
                continue;
            } else if col == middle_start_position {
                title_line.append(&mut middle);
                col += *middle_len;
                continue;
            } else {
                title_line.append(&mut foreground_color(boundary_type::HORIZONTAL, self.color));
            }
            if col == self.geom.x + self.geom.cols - 1 {
                break;
            }
            col += 1;
        }
        title_line
    }
    fn middle_only_title_line(
        &self,
        mut middle: Vec<TerminalCharacter>,
        middle_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut title_line = vec![];
        let middle_start_position = self.geom.x + (total_title_length / 2) - (*middle_len / 2) + 1;

        let mut col = self.geom.x;
        loop {
            if col == self.geom.x {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_LEFT),
                    self.color,
                ));
            } else if col == self.geom.x + self.geom.cols - 1 {
                title_line.append(&mut foreground_color(
                    self.get_corner(boundary_type::TOP_RIGHT),
                    self.color,
                ));
            } else if col == middle_start_position {
                title_line.append(&mut middle);
                col += *middle_len;
                continue;
            } else {
                title_line.append(&mut foreground_color(boundary_type::HORIZONTAL, self.color));
            }
            if col == self.geom.x + self.geom.cols - 1 {
                break;
            }
            col += 1;
        }
        title_line
    }
    fn two_part_title_line(
        &self,
        mut left_side: Vec<TerminalCharacter>,
        left_side_len: &usize,
        mut right_side: Vec<TerminalCharacter>,
        right_side_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_RIGHT), self.color);
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut middle = String::new();
        for _ in (left_side_len + right_side_len)..total_title_length {
            middle.push_str(boundary_type::HORIZONTAL);
        }
        let mut ret = vec![];
        ret.append(&mut left_boundary);
        ret.append(&mut left_side);
        ret.append(&mut foreground_color(&middle, self.color));
        ret.append(&mut right_side);
        ret.append(&mut right_boundary);
        ret
    }
    fn left_only_title_line(
        &self,
        mut left_side: Vec<TerminalCharacter>,
        left_side_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_RIGHT), self.color);
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut middle_padding = String::new();
        for _ in *left_side_len..total_title_length {
            middle_padding.push_str(boundary_type::HORIZONTAL);
        }
        let mut ret = vec![];
        ret.append(&mut left_boundary);
        ret.append(&mut left_side);
        ret.append(&mut foreground_color(&middle_padding, self.color));
        ret.append(&mut right_boundary);
        ret
    }
    fn empty_title_line(&self) -> Vec<TerminalCharacter> {
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::TOP_RIGHT), self.color);
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut middle_padding = String::new();
        for _ in 0..total_title_length {
            middle_padding.push_str(boundary_type::HORIZONTAL);
        }
        let mut ret = vec![];
        ret.append(&mut left_boundary);
        ret.append(&mut foreground_color(&middle_padding, self.color));
        ret.append(&mut right_boundary);
        ret
    }
    fn title_line_with_middle(
        &self,
        middle: Vec<TerminalCharacter>,
        middle_len: &usize,
    ) -> Vec<TerminalCharacter> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let length_of_each_side = total_title_length.saturating_sub(*middle_len + 2) / 2;
        let left_side = self.render_title_left_side(length_of_each_side);
        let right_side = self.render_title_right_side(length_of_each_side);

        match (left_side, right_side) {
            (Some((left_side, left_side_len)), Some((right_side, right_side_len))) => self
                .three_part_title_line(
                    left_side,
                    &left_side_len,
                    middle,
                    middle_len,
                    right_side,
                    &right_side_len,
                ),
            (Some((left_side, left_side_len)), None) => {
                self.left_and_middle_title_line(left_side, &left_side_len, middle, middle_len)
            },
            _ => self.middle_only_title_line(middle, middle_len),
        }
    }
    fn title_line_without_middle(&self) -> Vec<TerminalCharacter> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let left_side = self.render_title_left_side(total_title_length);
        let right_side = left_side.as_ref().and_then(|(_left_side, left_side_len)| {
            let space_left = total_title_length.saturating_sub(*left_side_len + 1); // 1 for a middle separator
            self.render_title_right_side(space_left)
        });
        match (left_side, right_side) {
            (Some((left_side, left_side_len)), Some((right_side, right_side_len))) => {
                self.two_part_title_line(left_side, &left_side_len, right_side, &right_side_len)
            },
            (Some((left_side, left_side_len)), None) => {
                self.left_only_title_line(left_side, &left_side_len)
            },
            _ => self.empty_title_line(),
        }
    }
    fn render_title(&self) -> Result<Vec<TerminalCharacter>> {
        let total_title_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners

        self.render_title_middle(total_title_length)
            .map(|(middle, middle_length)| self.title_line_with_middle(middle, &middle_length))
            .or_else(|| Some(self.title_line_without_middle()))
            .with_context(|| format!("failed to render title '{}'", self.title))
    }
    fn render_stack_list_entry(&self, entry: &StackListEntry) -> Vec<TerminalCharacter> {
        let usable_cols = self.geom.cols;
        let selection_marker = "> ";
        let marker_width = selection_marker.width();
        let corner_padding = 1;
        let corner_overhead = 2 * (1 + corner_padding);
        let inner_width = entry
            .width
            .min(usable_cols.saturating_sub(marker_width + corner_overhead));
        let content = if entry.label.width() <= inner_width {
            entry.label.clone()
        } else {
            let truncation_budget = inner_width.saturating_sub(1);
            let mut truncated = String::new();
            for character in entry.label.chars() {
                if truncated.width() + character.width().unwrap_or(0) > truncation_budget {
                    break;
                }
                truncated.push(character);
            }
            truncated.push('…');
            truncated
        };
        let full_width_entry_length = marker_width + corner_overhead + inner_width;
        let entry_start = usable_cols.saturating_sub(full_width_entry_length) / 2;
        let left_budget = entry_start;
        let (mut focus_part, focus_length) = self
            .bracketed_focus_indicator(left_budget)
            .unwrap_or((vec![], 0));
        let mut line = Vec::with_capacity(usable_cols);
        line.append(&mut focus_part);
        for _ in focus_length..left_budget {
            line.push(EMPTY_TERMINAL_CHARACTER);
        }
        let unfocused_color = self.style.colors.frame_unselected.map(|frame| frame.base);
        let style_entry_text = |text: &str| {
            if entry.is_selected || entry.is_emphasized {
                foreground_color(text, self.color)
            } else if entry.stack_is_focused {
                foreground_color(text, unfocused_color)
            } else {
                dimmed_foreground_color(text, unfocused_color)
            }
        };
        if entry.is_selected {
            line.append(&mut foreground_color(selection_marker, None));
        } else {
            for _ in 0..marker_width {
                line.push(EMPTY_TERMINAL_CHARACTER);
            }
        }
        line.append(&mut foreground_color(boundary_type::VERTICAL, None));
        for _ in 0..corner_padding {
            line.push(EMPTY_TERMINAL_CHARACTER);
        }
        line.append(&mut style_entry_text(&content));
        let content_padding = inner_width.saturating_sub(content.width());
        for _ in 0..content_padding {
            line.push(EMPTY_TERMINAL_CHARACTER);
        }
        for _ in 0..corner_padding {
            line.push(EMPTY_TERMINAL_CHARACTER);
        }
        line.append(&mut foreground_color(boundary_type::VERTICAL, None));
        let mut occupied_columns = entry_start + full_width_entry_length;
        let (mut scroll_part, scroll_length) = self
            .bracketed_scroll_indicator(usable_cols.saturating_sub(occupied_columns))
            .unwrap_or((vec![], 0));
        if self.exit_status.is_some() {
            let (mut exit_part, exit_length) = self.first_exited_held_title_part_full();
            if occupied_columns + 1 + exit_length + scroll_length <= usable_cols {
                line.push(EMPTY_TERMINAL_CHARACTER);
                line.append(&mut exit_part);
                occupied_columns += 1 + exit_length;
            }
        }
        while occupied_columns < usable_cols.saturating_sub(scroll_length) {
            line.push(EMPTY_TERMINAL_CHARACTER);
            occupied_columns += 1;
        }
        line.append(&mut scroll_part);
        line
    }
    fn render_one_line_title(&self) -> Result<Vec<TerminalCharacter>> {
        if self.should_draw_pane_frames {
            let total_title_length = self.geom.cols.saturating_sub(2);
            return self
                .render_title_middle(total_title_length)
                .map(|(middle, middle_length)| self.title_line_with_middle(middle, &middle_length))
                .or_else(|| Some(self.title_line_without_middle()))
                .with_context(|| format!("failed to render title '{}'", self.title));
        }

        let width = self.geom.cols;
        let title = self.bracketed_pane_title(width);
        let title_length = title.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let side_budget = width.saturating_sub(title_length) / 2;
        let focus = self.bracketed_focus_indicator(side_budget);
        let focus_length = focus.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let right_budget = width.saturating_sub(focus_length + title_length);
        let right = self.bracketed_right_side(right_budget);
        Ok(self.compose_bracketed_title(focus, title, right))
    }
    fn bracketed_title_part(&self, content: &str) -> (Vec<TerminalCharacter>, usize) {
        let text = format!(" [ {} ] ", content);
        (foreground_color(&text, self.color), text.width())
    }
    fn bracketed_title_part_from_characters(
        &self,
        mut content: Vec<TerminalCharacter>,
        content_length: usize,
    ) -> (Vec<TerminalCharacter>, usize) {
        let mut part = foreground_color(" [ ", self.color);
        part.append(&mut content);
        part.append(&mut foreground_color(" ] ", self.color));
        (part, content_length + 6)
    }
    fn plain_title_part(&self, content: &str) -> (Vec<TerminalCharacter>, usize) {
        let text = format!(" {} ", content);
        (foreground_color(&text, self.color), text.width())
    }
    fn bracketed_pane_title(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        let title_padding = 2;
        if self.title.is_empty() || max_length <= title_padding {
            return None;
        }
        let exit_part = self
            .exit_status
            .is_some()
            .then(|| self.first_exited_held_title_part_full());
        let exit_length = exit_part.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let content_max_length = max_length.saturating_sub(title_padding + exit_length);
        let content = if self.title.width() <= content_max_length {
            self.title.clone()
        } else {
            let truncation_budget = content_max_length.saturating_sub(1);
            let mut truncated = String::new();
            for character in self.title.chars() {
                if truncated.width() + character.width().unwrap_or(0) > truncation_budget {
                    break;
                }
                truncated.push(character);
            }
            truncated.push('…');
            truncated
        };
        let (mut part, mut length) = self.plain_title_part(&content);
        if let Some((mut exit, exit_length)) = exit_part {
            part.append(&mut exit);
            length += exit_length;
        }
        Some((part, length))
    }
    fn bracketed_scroll_indicator(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let has_scroll = self.scroll_position.0 > 0 || self.scroll_position.1 > 0;
        if !(has_scroll && self.is_selectable) {
            return None;
        }
        let full_indication = format!(
            "SCROLL: {}/{}",
            self.scroll_position.0, self.scroll_position.1
        );
        let (full_part, full_length) = self.bracketed_title_part(&full_indication);
        if full_length <= max_length {
            return Some((full_part, full_length));
        }
        let short_indication = format!("{}/{}", self.scroll_position.0, self.scroll_position.1);
        let (short_part, short_length) = self.bracketed_title_part(&short_indication);
        if short_length <= max_length {
            return Some((short_part, short_length));
        }
        None
    }
    /// Everything the one-line title row puts at its right edge, the handle last.
    ///
    /// The same order and the same precedence as the full frame's right side: the scroll indicator
    /// is offered room first and the handle takes what is left, so a narrow row loses the address
    /// before it loses anything a human is reading.
    fn bracketed_right_side(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        let scroll = self.bracketed_scroll_indicator(max_length);
        let scroll_length = scroll.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let handle = self.bracketed_handle_indicator(max_length.saturating_sub(scroll_length));
        let handle_length = handle.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let note =
            self.bracketed_note_indicator(max_length.saturating_sub(scroll_length + handle_length));
        let mut characters: Vec<TerminalCharacter> = Vec::new();
        let mut length = 0;
        // the same order as the full frame: what a human reads first is offered room first, and
        // the note - the newest thing on this row - is the first to be left out
        for (mut element, element_length) in [scroll, note, handle].into_iter().flatten() {
            characters.append(&mut element);
            length += element_length;
        }
        (!characters.is_empty()).then_some((characters, length))
    }
    /// The pane's note in the one-line row, cut to fit - see `render_note_indication`.
    fn bracketed_note_indicator(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let (note, note_color) = self
            .pane_note
            .as_ref()
            .filter(|(note, _)| !note.is_empty())?;
        // ` [ ` and ` ] ` around it, which is what this row puts around every element
        let room = max_length.checked_sub(6).filter(|room| *room > 0)?;
        let shown: String = note.chars().take(room).collect();
        let mut content = foreground_color(&shown, self.note_color(*note_color));
        let content_length = shown.width();
        let (part, length) =
            self.bracketed_title_part_from_characters(content.drain(..).collect(), content_length);
        (length <= max_length).then_some((part, length))
    }
    /// The pane's handle in the one-line row, whole or not at all - see `render_handle_indication`.
    fn bracketed_handle_indicator(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        if self.pane_handle.is_empty() {
            return None;
        }
        let (part, length) = self.bracketed_title_part(&self.pane_handle);
        (length <= max_length).then_some((part, length))
    }
    fn bracketed_focus_indicator(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        if self.is_main_client
            && self.other_focused_clients.is_empty()
            && !self.other_cursors_exist_in_session
        {
            None
        } else if self.is_main_client
            && self.other_focused_clients.is_empty()
            && self.other_cursors_exist_in_session
        {
            let (full_part, full_length) = self.bracketed_title_part("MY FOCUS");
            if full_length <= max_length {
                Some((full_part, full_length))
            } else {
                let (short_part, short_length) = self.bracketed_title_part("ME");
                (short_length <= max_length).then_some((short_part, short_length))
            }
        } else if self.is_main_client && !self.other_focused_clients.is_empty() {
            self.bracketed_focused_users("MY FOCUS AND:", "+", max_length)
        } else if !self.other_focused_clients.is_empty() {
            let full_label = if self.other_focused_clients.len() == 1 {
                "FOCUSED USER:"
            } else {
                "FOCUSED USERS:"
            };
            self.bracketed_focused_users(full_label, "U:", max_length)
        } else {
            None
        }
    }
    fn focused_users_part(&self, label: &str) -> (Vec<TerminalCharacter>, usize) {
        let mut content = foreground_color(label, self.color);
        let mut content_length = label.width();
        for client_id in &self.other_focused_clients {
            content.push(EMPTY_TERMINAL_CHARACTER);
            content.append(&mut self.client_cursor(*client_id));
            content_length += 2;
        }
        self.bracketed_title_part_from_characters(content, content_length)
    }
    fn focused_users_cursors_part(&self) -> (Vec<TerminalCharacter>, usize) {
        let mut content = vec![];
        let mut content_length = 0;
        for client_id in &self.other_focused_clients {
            if content_length > 0 {
                content.push(EMPTY_TERMINAL_CHARACTER);
                content_length += 1;
            }
            content.append(&mut self.client_cursor(*client_id));
            content_length += 1;
        }
        self.bracketed_title_part_from_characters(content, content_length)
    }
    fn bracketed_focused_users(
        &self,
        full_label: &str,
        short_label: &str,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let (full_part, full_length) = self.focused_users_part(full_label);
        if full_length <= max_length {
            return Some((full_part, full_length));
        }
        let (short_part, short_length) = self.focused_users_part(short_label);
        if short_length <= max_length {
            return Some((short_part, short_length));
        }
        let (cursors_part, cursors_length) = self.focused_users_cursors_part();
        (cursors_length <= max_length).then_some((cursors_part, cursors_length))
    }
    fn compose_bracketed_title(
        &self,
        left: Option<(Vec<TerminalCharacter>, usize)>,
        middle: Option<(Vec<TerminalCharacter>, usize)>,
        right: Option<(Vec<TerminalCharacter>, usize)>,
    ) -> Vec<TerminalCharacter> {
        let width = self.geom.cols;
        let left_length = left.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let middle_length = middle.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let right_length = right.as_ref().map(|(_, length)| *length).unwrap_or(0);
        let centered_start = (width / 2)
            .saturating_sub(middle_length / 2)
            .max(left_length);
        let middle_start = if middle.is_some() {
            // fork addition: `top_only` keeps the pre-0.45 left-aligned title
            if self.top_only_frames {
                left_length
            } else if right_length > 0 {
                let scroll_start = width.saturating_sub(right_length);
                let centered_end = centered_start + middle_length;
                if centered_end > scroll_start {
                    scroll_start.saturating_sub(middle_length).max(left_length)
                } else {
                    centered_start
                }
            } else {
                centered_start
            }
        } else {
            left_length
        };
        let middle_end = if middle.is_some() {
            middle_start + middle_length
        } else {
            left_length
        };
        let right_start = width.saturating_sub(right_length).max(middle_end);
        // fork addition: `top_only` fills the title line with a rule, like a stacked pane does
        let fill_character = if self.pane_is_stacked || self.top_only_frames {
            foreground_color(boundary_type::HORIZONTAL, self.color)
                .into_iter()
                .next()
                .unwrap_or(EMPTY_TERMINAL_CHARACTER)
        } else {
            EMPTY_TERMINAL_CHARACTER
        };
        let mut placements: Vec<(usize, Vec<TerminalCharacter>, usize)> = vec![];
        if let Some((characters, length)) = left {
            placements.push((0, characters, length));
        }
        if let Some((characters, length)) = middle {
            placements.push((middle_start, characters, length));
        }
        if let Some((characters, length)) = right {
            placements.push((right_start, characters, length));
        }
        let mut title_line = vec![];
        let mut col = 0;
        while col < width {
            if let Some(index) = placements.iter().position(|(start, _, _)| *start == col) {
                let (_, mut characters, length) = placements.remove(index);
                title_line.append(&mut characters);
                col += length;
            } else {
                title_line.push(fill_character.clone());
                col += 1;
            }
        }
        title_line
    }
    fn render_highlight_tooltip_undertitle(&self) -> Result<Vec<TerminalCharacter>> {
        let max_undertitle_length = self.geom.cols.saturating_sub(2);

        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_RIGHT), self.color);

        let tooltip_text = self.highlight_tooltip.as_deref().unwrap_or("");
        let text = format!(" Alt <Click> - {} ", tooltip_text);
        let text_len = text.chars().count();
        if text_len > max_undertitle_length {
            return Ok(self.empty_undertitle(max_undertitle_length));
        }

        let mut text_characters = foreground_color(&text, self.color);

        let padding_len = max_undertitle_length.saturating_sub(text_len);
        let mut padding = String::new();
        for _ in 0..padding_len {
            padding.push_str(boundary_type::HORIZONTAL);
        }

        let mut ret = vec![];
        ret.append(&mut left_boundary);
        ret.append(&mut text_characters);
        ret.append(&mut foreground_color(&padding, self.color));
        ret.append(&mut right_boundary);
        Ok(ret)
    }
    fn render_help_text_undertitle(&self) -> Result<Vec<TerminalCharacter>> {
        let max_undertitle_length = self.geom.cols.saturating_sub(2);

        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_RIGHT), self.color);

        // Try different versions of the help text from longest to shortest
        let (mut help_text_characters, help_text_len) = if let Some((chars, len)) =
            self.help_text_version_full(max_undertitle_length)
        {
            (chars, len)
        } else if let Some((chars, len)) = self.help_text_version_medium(max_undertitle_length) {
            (chars, len)
        } else if let Some((chars, len)) = self.help_text_version_short(max_undertitle_length) {
            (chars, len)
        } else {
            return Ok(self.empty_undertitle(max_undertitle_length));
        };

        let padding_len = max_undertitle_length.saturating_sub(help_text_len);
        let mut padding = String::new();
        for _ in 0..padding_len {
            padding.push_str(boundary_type::HORIZONTAL);
        }

        let mut ret = vec![];
        ret.append(&mut left_boundary);
        ret.append(&mut help_text_characters);
        ret.append(&mut foreground_color(&padding, self.color));
        ret.append(&mut right_boundary);
        Ok(ret)
    }

    fn help_text_version_full(&self, max_length: usize) -> Option<(Vec<TerminalCharacter>, usize)> {
        self.help_text_version(max_length, HintTier::Full)
    }

    fn help_text_version_medium(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        self.help_text_version(max_length, HintTier::Medium)
    }

    fn help_text_version_short(
        &self,
        max_length: usize,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        self.help_text_version(max_length, HintTier::Minimal)
    }

    fn help_text_version(
        &self,
        max_length: usize,
        tier: HintTier,
    ) -> Option<(Vec<TerminalCharacter>, usize)> {
        let segments = resize_segments(self.is_floating, self.mouse_scroll_resize, tier);
        let (characters, length) = self.render_hint_segments(&segments);
        (length <= max_length).then_some((characters, length))
    }

    fn render_held_undertitle(&self) -> Result<Vec<TerminalCharacter>> {
        let max_undertitle_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let (mut first_part, first_part_len) = self.first_exited_held_title_part_full();
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_RIGHT), self.color);
        let res = if self.is_main_client {
            let (mut second_part, second_part_len) = self.second_held_title_part_full();
            let full_text_len = first_part_len + second_part_len;
            if full_text_len <= max_undertitle_length {
                // render exit status and tips
                let mut padding = String::new();
                for _ in full_text_len..max_undertitle_length {
                    padding.push_str(boundary_type::HORIZONTAL);
                }
                let mut ret = vec![];
                ret.append(&mut left_boundary);
                ret.append(&mut first_part);
                ret.append(&mut second_part);
                ret.append(&mut foreground_color(&padding, self.color));
                ret.append(&mut right_boundary);
                ret
            } else if first_part_len <= max_undertitle_length {
                // render only exit status
                let mut padding = String::new();
                for _ in first_part_len..max_undertitle_length {
                    padding.push_str(boundary_type::HORIZONTAL);
                }
                let mut ret = vec![];
                ret.append(&mut left_boundary);
                ret.append(&mut first_part);
                ret.append(&mut foreground_color(&padding, self.color));
                ret.append(&mut right_boundary);
                ret
            } else {
                self.empty_undertitle(max_undertitle_length)
            }
        } else {
            if first_part_len <= max_undertitle_length {
                // render first part
                let full_text_len = first_part_len;
                let mut padding = String::new();
                for _ in full_text_len..max_undertitle_length {
                    padding.push_str(boundary_type::HORIZONTAL);
                }
                let mut ret = vec![];
                ret.append(&mut left_boundary);
                ret.append(&mut first_part);
                ret.append(&mut foreground_color(&padding, self.color));
                ret.append(&mut right_boundary);
                ret
            } else {
                self.empty_undertitle(max_undertitle_length)
            }
        };
        Ok(res)
    }
    fn render_mouse_shortcuts_undertitle(&self) -> Result<Vec<TerminalCharacter>> {
        let max_undertitle_length = self.geom.cols.saturating_sub(2); // 2 for the left and right corners
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_RIGHT), self.color);
        let res = if self.is_main_client {
            self.empty_undertitle(max_undertitle_length)
        } else {
            let (mut hover_shortcuts, hover_shortcuts_len) = self.hover_shortcuts_part_full();
            if hover_shortcuts_len <= max_undertitle_length {
                // render exit status and tips
                let mut padding = String::new();
                for _ in hover_shortcuts_len..max_undertitle_length {
                    padding.push_str(boundary_type::HORIZONTAL);
                }
                let mut ret = vec![];
                ret.append(&mut left_boundary);
                ret.append(&mut hover_shortcuts);
                ret.append(&mut foreground_color(&padding, self.color));
                ret.append(&mut right_boundary);
                ret
            } else {
                self.empty_undertitle(max_undertitle_length)
            }
        };
        Ok(res)
    }
    pub fn clicked_on_pinned(&mut self, position: Position) -> bool {
        if self.is_floating {
            // TODO: this is not entirely accurate because our relative position calculation in
            // itself isn't - when that is fixed, we should adjust this as well
            let checkbox_center_position = self.geom.cols.saturating_sub(5);
            let checkbox_position_start = checkbox_center_position.saturating_sub(1);
            let checkbox_position_end = checkbox_center_position + 1;
            if position.line() == -1
                && (position.column() >= checkbox_position_start
                    && position.column() <= checkbox_position_end)
            {
                return true;
            }
        }
        false
    }
    pub fn render(&self) -> Result<(Vec<CharacterChunk>, Option<String>)> {
        let err_context = || "failed to render pane frame";
        let mut character_chunks = vec![];
        if self.omit_title {
            return Ok((character_chunks, None));
        }
        if let Some(entry) = &self.stack_list_entry {
            character_chunks.push(CharacterChunk::new(
                self.render_stack_list_entry(entry),
                self.geom.x,
                self.geom.y,
            ));
            return Ok((character_chunks, None));
        }
        if self.geom.rows == 1 || !self.should_draw_pane_frames {
            // we do this explicitly when not drawing pane frames because this should only happen
            // if this is a stacked pane with pane frames off (and it doesn't necessarily have only
            // 1 row because it could also be a flexible stacked pane)
            // in this case we should always draw the pane title line, and only the title line
            let mut one_line_title = self.render_one_line_title().with_context(err_context)?;

            if self.content_offset.right != 0 && !self.should_draw_pane_frames {
                // here what happens is that the title should be offset to the right
                // in order to give room to the boundaries between the panes to be drawn
                one_line_title.pop();
            }
            let y_coords_of_title = if self.pane_is_stacked_under && !self.should_draw_pane_frames {
                // we only want to use the bottom offset in this case because panes that are
                // stacked above the flexible pane should actually appear exactly where they are on
                // screen, the content offset being "absorbed" by the flexible pane below them
                self.geom.y.saturating_sub(self.content_offset.bottom)
            } else {
                self.geom.y
            };

            character_chunks.push(CharacterChunk::new(
                one_line_title,
                self.geom.x,
                y_coords_of_title,
            ));
        } else {
            for row in 0..self.geom.rows {
                if row == 0 {
                    // top row
                    let title = self.render_title().with_context(err_context)?;
                    let x = self.geom.x;
                    let y = self.geom.y + row;
                    character_chunks.push(CharacterChunk::new(title, x, y));
                } else if row == self.geom.rows - 1 {
                    // bottom row
                    if self.highlight_tooltip.is_some() && self.is_main_client {
                        let x = self.geom.x;
                        let y = self.geom.y + row;
                        character_chunks.push(CharacterChunk::new(
                            self.render_highlight_tooltip_undertitle()
                                .with_context(err_context)?,
                            x,
                            y,
                        ));
                    } else if self.show_help_text && self.is_main_client && self.mouse_hover_tips {
                        let x = self.geom.x;
                        let y = self.geom.y + row;
                        character_chunks.push(CharacterChunk::new(
                            self.render_help_text_undertitle()
                                .with_context(err_context)?,
                            x,
                            y,
                        ));
                    } else if self.mouse_is_hovering_over_pane
                        && !self.is_main_client
                        && self.mouse_hover_tips
                    {
                        let x = self.geom.x;
                        let y = self.geom.y + row;
                        character_chunks.push(CharacterChunk::new(
                            self.render_mouse_shortcuts_undertitle()
                                .with_context(err_context)?,
                            x,
                            y,
                        ));
                    } else if self.exit_status.is_some() || self.is_first_run {
                        let x = self.geom.x;
                        let y = self.geom.y + row;
                        character_chunks.push(CharacterChunk::new(
                            self.render_held_undertitle().with_context(err_context)?,
                            x,
                            y,
                        ));
                    } else {
                        let mut bottom_row = vec![];
                        for col in 0..self.geom.cols {
                            let boundary = if col == 0 {
                                // bottom left corner
                                self.get_corner(boundary_type::BOTTOM_LEFT)
                            } else if col == self.geom.cols - 1 {
                                // bottom right corner
                                self.get_corner(boundary_type::BOTTOM_RIGHT)
                            } else {
                                boundary_type::HORIZONTAL
                            };

                            let mut boundary_character = foreground_color(boundary, self.color);
                            bottom_row.append(&mut boundary_character);
                        }
                        let x = self.geom.x;
                        let y = self.geom.y + row;
                        character_chunks.push(CharacterChunk::new(bottom_row, x, y));
                    }
                } else {
                    let boundary_character_left =
                        foreground_color(boundary_type::VERTICAL, self.color);
                    let boundary_character_right =
                        foreground_color(boundary_type::VERTICAL, self.color);

                    let x = self.geom.x;
                    let y = self.geom.y + row;
                    character_chunks.push(CharacterChunk::new(boundary_character_left, x, y));

                    let x = (self.geom.x + self.geom.cols).saturating_sub(1);
                    let y = self.geom.y + row;
                    character_chunks.push(CharacterChunk::new(boundary_character_right, x, y));
                }
            }
        }
        Ok((character_chunks, None))
    }
    fn render_hint_segments(&self, segments: &[HintSegment]) -> (Vec<TerminalCharacter>, usize) {
        let mut characters = vec![];
        let mut length = 0;
        for segment in segments {
            let color = match segment.level {
                HintLevel::Plain => self.color,
                HintLevel::Emphasis => Some(self.style.colors.text_unselected.emphasis_0),
                HintLevel::Emphasis2 => Some(self.style.colors.text_unselected.emphasis_2),
                HintLevel::Error => Some(self.style.colors.exit_code_error.base),
                HintLevel::Success => Some(self.style.colors.exit_code_success.base),
            };
            characters.append(&mut foreground_color(&segment.text, color));
            length += segment.text.width();
        }
        (characters, length)
    }
    fn first_exited_held_title_part_full(&self) -> (Vec<TerminalCharacter>, usize) {
        match self.exit_status {
            Some(ExitStatus::Code(exit_code)) => {
                self.render_hint_segments(&exit_code_segments(HintExitStatus::Code(exit_code)))
            },
            Some(ExitStatus::Exited) => {
                self.render_hint_segments(&exit_code_segments(HintExitStatus::Exited))
            },
            None => (foreground_color(boundary_type::HORIZONTAL, self.color), 1),
        }
    }
    fn second_held_title_part_full(&self) -> (Vec<TerminalCharacter>, usize) {
        self.render_hint_segments(&rerun_segments(self.is_first_run, HintTier::Full))
    }
    fn hover_shortcuts_part_full(&self) -> (Vec<TerminalCharacter>, usize) {
        self.render_hint_segments(&hover_segments(HintTier::Full))
    }
    fn empty_undertitle(&self, max_undertitle_length: usize) -> Vec<TerminalCharacter> {
        let mut left_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_LEFT), self.color);
        let mut right_boundary =
            foreground_color(self.get_corner(boundary_type::BOTTOM_RIGHT), self.color);
        let mut ret = vec![];
        let mut padding = String::new();
        for _ in 0..max_undertitle_length {
            padding.push_str(boundary_type::HORIZONTAL);
        }
        ret.append(&mut left_boundary);
        ret.append(&mut foreground_color(&padding, self.color));
        ret.append(&mut right_boundary);
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::data::Style;
    use zellij_utils::pane_size::{Offset, Viewport};

    fn pane_frame_with(mouse_scroll_resize: bool, is_floating: bool, cols: usize) -> PaneFrame {
        PaneFrame::new(
            Viewport {
                x: 0,
                y: 0,
                cols,
                rows: 10,
            },
            (0, 0),
            String::new(),
            FrameParams {
                focused_client: None,
                is_main_client: true,
                other_focused_clients: vec![],
                style: Style::default(),
                color: None,
                other_cursors_exist_in_session: false,
                pane_is_stacked_over: false,
                pane_is_stacked_under: false,
                pane_is_stacked: false,
                should_draw_pane_frames: true,
                pane_is_floating: is_floating,
                content_offset: Offset::default(),
                mouse_is_hovering_over_pane: false,
                pane_is_selectable: true,
                show_help_text: true,
                highlight_tooltip: None,
                omit_title: false,
                frame_geom_override: None,
                stack_list_entry: None,
                blank_title: false,
                pane_handle: String::new(),
                pane_note: None,
                top_only_frames: false,
                mouse_scroll_resize,
                mouse_hover_tips: true,
                dimmed: false,
                guest_choice_indicator: None,
            },
        )
    }

    fn characters_to_string(chars: &[TerminalCharacter]) -> String {
        chars.iter().map(|c| c.character).collect()
    }

    /// A frame with a title, a handle and an optional scroll position, in either frame style.
    fn frame_with_handle(
        cols: usize,
        title: &str,
        handle: &str,
        scroll_position: (usize, usize),
        draws_full_frame: bool,
    ) -> PaneFrame {
        PaneFrame::new(
            Viewport {
                x: 0,
                y: 0,
                cols,
                rows: 10,
            },
            scroll_position,
            title.to_owned(),
            FrameParams {
                focused_client: None,
                is_main_client: true,
                other_focused_clients: vec![],
                style: Style::default(),
                color: None,
                other_cursors_exist_in_session: false,
                pane_is_stacked_over: false,
                pane_is_stacked_under: false,
                pane_is_stacked: false,
                should_draw_pane_frames: draws_full_frame,
                pane_is_floating: false,
                content_offset: Offset::default(),
                mouse_is_hovering_over_pane: false,
                pane_is_selectable: true,
                show_help_text: true,
                highlight_tooltip: None,
                omit_title: false,
                frame_geom_override: None,
                stack_list_entry: None,
                blank_title: false,
                pane_handle: handle.to_owned(),
                pane_note: None,
                // `top_only` is the fork's frames-off style, so it renders the one-line row
                top_only_frames: !draws_full_frame,
                mouse_scroll_resize: false,
                mouse_hover_tips: false,
                dimmed: false,
                guest_choice_indicator: None,
            },
        )
    }

    /// The same frame, carrying a note as well.
    fn frame_with_note(
        cols: usize,
        title: &str,
        handle: &str,
        note: &str,
        draws_full_frame: bool,
    ) -> PaneFrame {
        let mut frame = frame_with_handle(cols, title, handle, (0, 0), draws_full_frame);
        frame.pane_note = Some((note.to_owned(), NoteColor::Error));
        frame
    }

    fn full_frame_title_with_note(cols: usize, title: &str, handle: &str, note: &str) -> String {
        characters_to_string(
            &frame_with_note(cols, title, handle, note, true)
                .render_title()
                .unwrap(),
        )
    }

    fn one_line_title_with_note(cols: usize, title: &str, handle: &str, note: &str) -> String {
        characters_to_string(
            &frame_with_note(cols, title, handle, note, false)
                .render_one_line_title()
                .unwrap(),
        )
    }

    #[test]
    fn a_note_sits_beside_the_handle_and_leaves_it_the_right_edge() {
        // the handle is still the last thing on the row: the note is what a pane is DOING and the
        // handle is what it is CALLED, and the address is the one a reader copies
        let title_row = full_frame_title_with_note(60, "Pane #1", "sunny-otter", "exit 7");
        assert!(title_row.starts_with("┌ Pane #1 "), "got: {}", title_row);
        assert!(title_row.contains("exit 7"), "got: {}", title_row);
        assert!(title_row.ends_with(" sunny-otter ┐"), "got: {}", title_row);
        assert_eq!(title_row.width(), 60);
        let one_line = one_line_title_with_note(60, "Pane #1", "sunny-otter", "exit 7");
        assert!(one_line.contains("exit 7"), "got: {}", one_line);
        assert!(
            one_line.trim_end().ends_with("[ sunny-otter ]"),
            "got: {}",
            one_line
        );
        assert_eq!(one_line.width(), 60);
    }

    #[test]
    fn a_narrowing_row_loses_the_note_before_the_handle_and_the_handle_before_the_title() {
        // the width contest, in the order the elements are offered room. 30 columns holds the
        // title and the handle but not the note
        let narrow = full_frame_title_with_note(30, "Pane #1", "sunny-otter", "exit 7");
        assert!(!narrow.contains("exit 7"), "got: {}", narrow);
        assert!(narrow.ends_with(" sunny-otter ┐"), "got: {}", narrow);
        // narrower still, and the handle goes too - the title is what a human is reading
        let narrower = full_frame_title_with_note(24, "Pane #1", "sunny-otter", "exit 7");
        assert!(!narrower.contains("sunny-otter"), "got: {}", narrower);
        assert!(narrower.contains("Pane #1"), "got: {}", narrower);
        assert_eq!(narrower.width(), 24);
    }

    #[test]
    fn a_note_too_long_for_its_room_is_cut_rather_than_dropped() {
        // unlike the handle: half an address reaches no pane, but half a sentence still says
        // something
        let long = "waiting on the review that was promised on tuesday";
        let title_row = full_frame_title_with_note(60, "Pane #1", "sunny-otter", long);
        assert!(!title_row.contains(long), "got: {}", title_row);
        assert!(title_row.contains("waiting on"), "got: {}", title_row);
        assert!(title_row.contains('…'), "got: {}", title_row);
        assert_eq!(title_row.width(), 60);
    }

    #[test]
    fn a_pane_with_no_note_draws_the_row_it_drew_before() {
        // the negative control: nothing about the row changes for the panes that have no note,
        // which is nearly all of them
        assert_eq!(
            full_frame_title_with_note(60, "Pane #1", "sunny-otter", ""),
            full_frame_title(60, "Pane #1", "sunny-otter"),
            "an empty note takes no room"
        );
    }

    fn full_frame_title(cols: usize, title: &str, handle: &str) -> String {
        characters_to_string(
            &frame_with_handle(cols, title, handle, (0, 0), true)
                .render_title()
                .unwrap(),
        )
    }

    fn one_line_title(cols: usize, title: &str, handle: &str) -> String {
        characters_to_string(
            &frame_with_handle(cols, title, handle, (0, 0), false)
                .render_one_line_title()
                .unwrap(),
        )
    }

    #[test]
    fn the_handle_sits_at_the_right_of_a_full_frame_title_row() {
        // the mirror of the title: the title opens the row, the handle closes it
        let title_row = full_frame_title(40, "Pane #1", "sunny-otter");
        assert!(title_row.starts_with("┌ Pane #1 "), "got: {}", title_row);
        assert!(title_row.ends_with(" sunny-otter ┐"), "got: {}", title_row);
        assert_eq!(title_row.width(), 40);
    }

    #[test]
    fn the_handle_sits_at_the_right_of_a_one_line_title_row() {
        let title_row = one_line_title(40, "Pane #1", "sunny-otter");
        assert!(
            title_row.trim_end().ends_with("[ sunny-otter ]"),
            "got: {}",
            title_row
        );
        assert_eq!(title_row.width(), 40);
    }

    #[test]
    fn the_title_wins_the_width_contest_against_the_handle() {
        // a row with room for one of the two shows the title, in both frame styles. 24 columns is
        // also the case where a TRUNCATED handle would have fit: it is dropped rather than cut,
        // because half an address reaches no pane
        for title_row in [
            full_frame_title(24, "Pane #1", "sunny-otter"),
            one_line_title(24, "Pane #1", "sunny-otter"),
        ] {
            assert!(title_row.contains("Pane #1"), "got: {}", title_row);
            assert!(!title_row.contains("sunny"), "got: {}", title_row);
            assert_eq!(title_row.width(), 24);
        }
    }

    #[test]
    fn a_handle_never_moves_what_the_frame_already_drew() {
        // the negative control: a pane with no handle renders exactly what it rendered before
        // handles existed, and a pane with one leaves the scroll indication where it was
        let without = characters_to_string(
            &frame_with_handle(50, "Pane #1", "", (1, 2), true)
                .render_title()
                .unwrap(),
        );
        assert!(without.ends_with(" SCROLL:  1/2 ┐"), "got: {}", without);
        let with = characters_to_string(
            &frame_with_handle(50, "Pane #1", "sunny-otter", (1, 2), true)
                .render_title()
                .unwrap(),
        );
        assert!(
            with.contains(" SCROLL:  1/2 | sunny-otter ┐"),
            "got: {}",
            with
        );
        assert_eq!(without.width(), 50);
        assert_eq!(with.width(), 50);
    }

    #[test]
    fn the_scroll_indication_outranks_the_handle_when_only_one_fits() {
        // the handle is the last element offered room, so it is the first to go without
        let title_row = characters_to_string(
            &frame_with_handle(30, "Pane #1", "sunny-otter", (1, 2), true)
                .render_title()
                .unwrap(),
        );
        assert!(title_row.contains("1/2"), "got: {}", title_row);
        assert!(!title_row.contains("sunny"), "got: {}", title_row);
    }

    /// A floating frame, which is the only kind that draws the pin checkbox.
    fn floating_frame_with_handle(cols: usize, title: &str, handle: &str) -> PaneFrame {
        let mut frame = frame_with_handle(cols, title, handle, (0, 0), true);
        frame.is_floating = true;
        frame
    }

    #[test]
    fn a_handle_does_not_move_the_pin_checkbox_out_from_under_a_click() {
        // the pin is a click target and `clicked_on_pinned` finds it by counting back from the
        // right edge, so the handle must not take that edge from it. Without this the button a
        // user can see stops working and the handle text toggles the pin instead
        let cols = 60;
        let mut frame = floating_frame_with_handle(cols, "Pane #1", "sunny-otter");
        let title_row = characters_to_string(&frame.render_title().unwrap());
        // every character on a frame row is one column wide, so a char index is a column
        let drawn_checkbox = title_row
            .chars()
            .position(|character| character == '[')
            .map(|index| index + 1)
            .unwrap_or_else(|| panic!("no checkbox drawn: {}", title_row));
        assert!(
            title_row.contains("sunny-otter"),
            "the handle is still drawn: {}",
            title_row
        );
        assert!(
            frame.clicked_on_pinned(Position::new(-1, drawn_checkbox as u16)),
            "a click on the drawn checkbox at column {} did not toggle the pin: {}",
            drawn_checkbox,
            title_row
        );
    }

    #[test]
    fn a_pane_with_no_pin_still_draws_its_handle_last() {
        // the negative control: only a pin moves the handle off the right edge
        let title_row = full_frame_title(40, "Pane #1", "sunny-otter");
        assert!(
            title_row.ends_with(" sunny-otter \u{2510}"),
            "got: {}",
            title_row
        );
    }

    #[test]
    fn help_text_full_includes_ctrl_scroll_when_enabled_for_tiled_pane() {
        let frame = pane_frame_with(true, false, 80);
        let (chars, _) = frame.help_text_version_full(80).unwrap();
        let text = characters_to_string(&chars);
        assert!(text.contains("Ctrl <MouseScroll>"));
        assert!(text.contains("<drag borders>"));
    }

    #[test]
    fn help_text_full_omits_ctrl_scroll_when_disabled_for_tiled_pane() {
        let frame = pane_frame_with(false, false, 80);
        let (chars, _) = frame.help_text_version_full(80).unwrap();
        let text = characters_to_string(&chars);
        assert!(!text.contains("MouseScroll"));
        assert!(text.contains("<drag borders> to resize"));
    }

    #[test]
    fn help_text_full_includes_ctrl_scroll_when_enabled_for_floating_pane() {
        let frame = pane_frame_with(true, true, 80);
        let (chars, _) = frame.help_text_version_full(80).unwrap();
        let text = characters_to_string(&chars);
        assert!(text.contains("Ctrl <MouseScroll>"));
        assert!(text.contains("Ctrl <drag borders>"));
    }

    #[test]
    fn help_text_full_omits_ctrl_scroll_when_disabled_for_floating_pane() {
        let frame = pane_frame_with(false, true, 80);
        let (chars, _) = frame.help_text_version_full(80).unwrap();
        let text = characters_to_string(&chars);
        assert!(!text.contains("MouseScroll"));
        assert!(text.contains("Ctrl <drag borders> to resize"));
    }

    #[test]
    fn help_text_medium_omits_ctrl_scroll_when_disabled() {
        let frame = pane_frame_with(false, false, 40);
        let (chars, _) = frame.help_text_version_medium(40).unwrap();
        let text = characters_to_string(&chars);
        assert!(!text.contains("MouseScroll"));
        assert!(text.contains("<drag borders> resize"));
    }

    #[test]
    fn help_text_short_omits_ctrl_scroll_when_disabled() {
        let frame = pane_frame_with(false, false, 20);
        let (chars, _) = frame.help_text_version_short(20).unwrap();
        let text = characters_to_string(&chars);
        assert!(!text.contains("MouseScroll"));
        assert!(text.contains("<drag borders>"));
    }
}
