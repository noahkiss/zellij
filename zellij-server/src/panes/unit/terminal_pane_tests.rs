use super::super::TerminalPane;
use crate::panes::kitty_graphics::KittyImageStore;
use crate::panes::sixel::SixelImageStore;
use crate::panes::LinkHandler;
use crate::tab::Pane;
use insta::assert_snapshot;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use zellij_utils::{
    data::{Palette, Style},
    pane_size::{Dimension, Offset, PaneGeom, SizeInPixels},
    position::Position,
};

use std::fmt::Write;

fn read_fixture(fixture_name: &str) -> Vec<u8> {
    let mut path_to_file = std::path::PathBuf::new();
    path_to_file.push("../src");
    path_to_file.push("tests");
    path_to_file.push("fixtures");
    path_to_file.push(fixture_name);
    std::fs::read(path_to_file)
        .unwrap_or_else(|_| panic!("could not read fixture {:?}", &fixture_name))
}

#[test]
pub fn scrolling_inside_a_pane() {
    let fake_client_id = 1;
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let mut text_to_fill_pane = String::new();
    for i in 0..30 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    terminal_pane.scroll_up(10, fake_client_id);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
    terminal_pane.scroll_down(3, fake_client_id);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
    terminal_pane.clear_scroll();
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn sixel_image_inside_terminal_pane() {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let sixel_image_bytes = "\u{1b}Pq
        #0;2;0;0;0#1;2;100;100;0#2;2;0;100;0
        #1~~@@vv@@~~@@~~$
        #2??}}GG}}??}}??-
        #1!14@
        \u{1b}\\";

    terminal_pane.handle_pty_bytes(Vec::from(sixel_image_bytes.as_bytes()));
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn partial_sixel_image_inside_terminal_pane() {
    // here we test to make sure we partially render an image that is partially hidden in the
    // scrollbuffer
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let pane_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(pane_content);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn overflowing_sixel_image_inside_terminal_pane() {
    // here we test to make sure we properly render an image that overflows both in the width and
    // height of the pane
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(50);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let pane_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(pane_content);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn scrolling_through_a_sixel_image() {
    let fake_client_id = 1;
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let mut text_to_fill_pane = String::new();
    for i in 0..30 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    writeln!(&mut text_to_fill_pane, "\r").unwrap();
    let pane_sixel_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    terminal_pane.handle_pty_bytes(pane_sixel_content);
    terminal_pane.scroll_up(10, fake_client_id);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
    terminal_pane.scroll_down(3, fake_client_id);
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
    terminal_pane.clear_scroll();
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn multiple_sixel_images_in_pane() {
    let fake_client_id = 1;
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let mut text_to_fill_pane = String::new();
    for i in 0..5 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    writeln!(&mut text_to_fill_pane, "\r").unwrap();
    let pane_sixel_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(pane_sixel_content.clone()); // one image above text
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    terminal_pane.handle_pty_bytes(pane_sixel_content); // one image below text
    terminal_pane.scroll_up(20, fake_client_id); // scroll up to see both images
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn resizing_pane_with_sixel_images() {
    // here we test, for example, that sixel images don't wrap with other lines
    let fake_client_id = 1;
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let mut text_to_fill_pane = String::new();
    for i in 0..5 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    writeln!(&mut text_to_fill_pane, "\r").unwrap();
    let pane_sixel_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(pane_sixel_content.clone());
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    terminal_pane.handle_pty_bytes(pane_sixel_content);
    let mut new_win_size = PaneGeom::default();
    new_win_size.cols.set_inner(100);
    new_win_size.rows.set_inner(20);
    terminal_pane.set_geom(new_win_size);
    terminal_pane.scroll_up(20, fake_client_id); // scroll up to see both images
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn changing_character_cell_size_with_sixel_images() {
    let fake_client_id = 1;
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size.clone(),
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index
    let mut text_to_fill_pane = String::new();
    for i in 0..5 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    writeln!(&mut text_to_fill_pane, "\r").unwrap();
    let pane_sixel_content = read_fixture("sixel-image-500px.six");
    terminal_pane.handle_pty_bytes(pane_sixel_content.clone());
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    terminal_pane.handle_pty_bytes(pane_sixel_content);
    // here the new_win_size is the same as the old one, we just update the character_cell_size
    // which will be picked up upon resize (which is why we're doing set_geom below)
    let mut new_win_size = PaneGeom::default();
    new_win_size.cols.set_inner(121);
    new_win_size.rows.set_inner(20);
    *character_cell_size.borrow_mut() = Some(SizeInPixels {
        width: 8,
        height: 18,
    });
    terminal_pane.set_geom(new_win_size);
    terminal_pane.scroll_up(10, fake_client_id); // scroll up to see both images
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn keep_working_after_corrupted_sixel_image() {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index

    let sixel_image_bytes = "\u{1b}PI AM CORRUPTED BWAHAHAq
        #0;2;0;0;0#1;2;100;100;0#2;2;0;100;0
        #1~~@@vv@@~~@@~~$
        #2??}}GG}}??}}??-
        #1!14@
        \u{1b}\\";

    terminal_pane.handle_pty_bytes(Vec::from(sixel_image_bytes.as_bytes()));
    let mut text_to_fill_pane = String::new();
    for i in 0..5 {
        writeln!(&mut text_to_fill_pane, "\rline {}", i + 1).unwrap();
    }
    terminal_pane.handle_pty_bytes(text_to_fill_pane.into_bytes());
    assert_snapshot!(format!("{:?}", terminal_pane.grid));
}

#[test]
pub fn pane_with_frame_position_is_on_frame() {
    let mut fake_win_size = PaneGeom {
        x: 10,
        y: 10,
        ..PaneGeom::default()
    };
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index

    terminal_pane.set_content_offset(Offset::frame(1));

    // row above pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 129)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 131)));

    // first row:  border for 10 <= col <= 130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 9)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 10)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 11)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 129)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 131)));

    // second row: border only at col=10,130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 9)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(11, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(11, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 131)));

    // row in the middle: border only at col=10,130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 9)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(15, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(15, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 131)));

    // last row: border for 10 <= col <= 130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 9)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 10)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 11)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 131)));

    // row below pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 131)));
}

#[test]
pub fn pane_with_bottom_and_right_borders_position_is_on_frame() {
    let mut fake_win_size = PaneGeom {
        x: 10,
        y: 10,
        ..PaneGeom::default()
    };
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index

    terminal_pane.set_content_offset(Offset::shift(1, 1));

    // row above pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 129)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 131)));

    // first row: border only at col=130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 129)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(10, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 131)));

    // second row: border only at col=130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(11, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 131)));

    // row in the middle: border only at col=130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(15, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 131)));

    // last row: border for 10 <= col <= 130
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 9)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 10)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 11)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 70)));
    assert!(terminal_pane.position_is_on_frame(&Position::new(29, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 131)));

    // row below pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 131)));
}

fn make_terminal_pane_for_bell() -> TerminalPane {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    TerminalPane::new(
        1,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
pub fn bell_notification_state_set_and_cleared() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    assert!(
        !terminal_pane.get_bell_notification(),
        "Initially no bell notification"
    );

    terminal_pane.set_bell_notification(true);
    assert!(
        terminal_pane.get_bell_notification(),
        "Bell notification should be set"
    );

    terminal_pane.set_bell_notification(false);
    assert!(
        !terminal_pane.get_bell_notification(),
        "Bell notification should be cleared"
    );
}

#[test]
pub fn has_bell_reflects_grid_ring_bell() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    assert!(
        !terminal_pane.has_bell(),
        "Initially has_bell should be false"
    );

    terminal_pane.handle_pty_bytes(vec![7u8]);
    assert!(
        terminal_pane.has_bell(),
        "has_bell should be true after pty bell byte"
    );

    terminal_pane.consume_bell();
    assert!(
        !terminal_pane.has_bell(),
        "has_bell should be false after consume_bell"
    );
}

#[test]
pub fn osc7_payload_stored_from_pty_bytes() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    assert!(
        terminal_pane.osc7_payload().is_none(),
        "Initially osc7_payload should be None"
    );

    // Send OSC 7 sequence: \x1b]7;file://localhost/tmp\x1b\\
    let osc7_bytes = b"\x1b]7;file://localhost/tmp\x1b\\";
    terminal_pane.handle_pty_bytes(osc7_bytes.to_vec());

    assert_eq!(
        terminal_pane.drain_osc7_cwd(),
        Some(std::path::PathBuf::from("/tmp")),
        "OSC 7 should still update Zellij's internal CWD tracking"
    );
    assert!(terminal_pane.drain_osc7_cwd().is_none());
    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/tmp"),
        "The forwarding URI should persist after the internal CWD update is drained"
    );
}

#[test]
pub fn osc7_payload_updated_on_subsequent_osc7() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    let osc7_first = b"\x1b]7;file://localhost/first\x1b\\";
    terminal_pane.handle_pty_bytes(osc7_first.to_vec());
    assert_eq!(terminal_pane.osc7_payload(), Some("file://localhost/first"));

    let osc7_second = b"\x1b]7;file://localhost/second\x1b\\";
    terminal_pane.handle_pty_bytes(osc7_second.to_vec());
    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/second")
    );
}

#[test]
pub fn osc7_payload_preserves_percent_encoding() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    let osc7_bytes = b"\x1b]7;file://localhost/path%20with%20spaces\x1b\\";
    terminal_pane.handle_pty_bytes(osc7_bytes.to_vec());
    assert_eq!(
        terminal_pane.drain_osc7_cwd(),
        Some(std::path::PathBuf::from("/path with spaces")),
    );
    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/path%20with%20spaces"),
    );
}

#[test]
pub fn osc7_payload_works_with_bel_terminator() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    // BEL (\x07) is an alternative terminator for OSC sequences
    let osc7_bytes = b"\x1b]7;file://localhost/tmp\x07";
    terminal_pane.handle_pty_bytes(osc7_bytes.to_vec());
    assert_eq!(terminal_pane.osc7_payload(), Some("file://localhost/tmp"),);
}

#[test]
pub fn osc7_payload_rejects_c0_control_characters() {
    // Test the Grid's osc_dispatch directly, since the VTE parser strips
    // control characters before they reach osc_dispatch. This validates
    // the defense-in-depth guard in the b"7" handler.
    use crate::vt_parser::Handler;

    let mut terminal_pane = make_terminal_pane_for_bell();
    let params: &[&[u8]] = &[b"7", b"file://host/path\x01bad"];
    terminal_pane.grid.osc_end(params, false);
    assert!(
        terminal_pane.osc7_payload().is_none(),
        "URIs with C0 control characters should be rejected"
    );
}

#[test]
pub fn osc7_payload_rejects_c1_control_characters() {
    // C1 control characters (U+0080..U+009F) include ST (U+009C) which
    // could terminate the OSC sequence in the parent terminal.
    use crate::vt_parser::Handler;

    let mut terminal_pane = make_terminal_pane_for_bell();
    // U+009C (ST) in UTF-8 is 0xC2 0x9C
    let params: &[&[u8]] = &[b"7", "file://host/path\u{9c}bad".as_bytes()];
    terminal_pane.grid.osc_end(params, false);
    assert!(
        terminal_pane.osc7_payload().is_none(),
        "URIs with C1 control characters should be rejected"
    );
}

#[test]
pub fn osc7_payload_rejects_an_empty_payload() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    terminal_pane.handle_pty_bytes(b"\x1b]7;file://localhost/tmp\x1b\\".to_vec());
    terminal_pane.handle_pty_bytes(b"\x1b]7;\x1b\\".to_vec());

    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/tmp"),
        "An empty OSC 7 is rejected and the previous URI is retained"
    );
}

#[test]
pub fn osc7_payload_rejects_invalid_utf8() {
    use crate::vt_parser::Handler;

    let mut terminal_pane = make_terminal_pane_for_bell();
    terminal_pane.handle_pty_bytes(b"\x1b]7;file://localhost/tmp\x1b\\".to_vec());

    let params: &[&[u8]] = &[b"7", b"file://localhost/\xff\xfe"];
    terminal_pane.grid.osc_end(params, false);

    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/tmp"),
        "A non-UTF-8 OSC 7 is rejected and the previous URI is retained"
    );
}

#[test]
pub fn osc7_payload_joins_semicolon_separated_segments() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    terminal_pane.handle_pty_bytes(b"\x1b]7;file://localhost/a;b;c\x1b\\".to_vec());

    assert_eq!(
        terminal_pane.osc7_payload(),
        Some("file://localhost/a;b;c"),
        "A URI containing semicolons is reassembled from the split params"
    );
}

#[test]
pub fn osc7_payload_is_cleared_by_a_terminal_reset() {
    let mut terminal_pane = make_terminal_pane_for_bell();

    terminal_pane.handle_pty_bytes(b"\x1b]7;file://localhost/tmp\x1b\\".to_vec());
    assert_eq!(terminal_pane.osc7_payload(), Some("file://localhost/tmp"));

    terminal_pane.handle_pty_bytes(b"\x1bc".to_vec());

    assert!(
        terminal_pane.osc7_payload().is_none(),
        "RIS clears the reported working directory"
    );
}

#[test]
pub fn frameless_pane_position_is_on_frame() {
    let mut fake_win_size = PaneGeom {
        x: 10,
        y: 10,
        ..PaneGeom::default()
    };
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let character_cell_size = Rc::new(RefCell::new(Some(SizeInPixels {
        width: 8,
        height: 21,
    })));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        fake_win_size,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        character_cell_size,
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    ); // 0 is the pane index

    terminal_pane.set_content_offset(Offset::default());

    // row above pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 129)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(9, 131)));

    // first row: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 129)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(10, 131)));

    // second row: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(11, 131)));

    // random row in the middle: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(15, 131)));

    // last row: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 9)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(29, 131)));

    // row below pane: no border
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 10)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 11)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 70)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 130)));
    assert!(!terminal_pane.position_is_on_frame(&Position::new(30, 131)));
}

fn create_guest_modal_pane() -> TerminalPane {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(80);
    fake_win_size.rows.set_inner(24);
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    TerminalPane::new(
        1,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

fn press_key(
    pane: &mut TerminalPane,
    bare_key: zellij_utils::data::BareKey,
    client_id: crate::ClientId,
) -> Option<crate::tab::AdjustedInput> {
    let key = zellij_utils::data::KeyWithModifier::new(bare_key);
    pane.adjust_input_to_terminal(&Some(key), vec![], false, Some(client_id))
}

#[test]
pub fn guest_modal_navigation_wraps() {
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;
    pane.set_guest_modal(&[client_id]);
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
    press_key(&mut pane, BareKey::Up, client_id);
    assert_eq!(pane.guest_modal_selection(client_id), Some(1));
    press_key(&mut pane, BareKey::Down, client_id);
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
    press_key(&mut pane, BareKey::Down, client_id);
    assert_eq!(pane.guest_modal_selection(client_id), Some(1));
    press_key(&mut pane, BareKey::Up, client_id);
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
}

#[test]
pub fn guest_modal_tab_is_ignored() {
    use zellij_utils::data::{BareKey, KeyWithModifier};
    let mut pane = create_guest_modal_pane();
    let client_id = 1;
    pane.set_guest_modal(&[client_id]);
    press_key(&mut pane, BareKey::Tab, client_id);
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
    let key = KeyWithModifier::new(BareKey::Tab).with_shift_modifier();
    pane.adjust_input_to_terminal(&Some(key), vec![], false, Some(client_id));
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
}

#[test]
pub fn guest_modal_enter_confirms_selection() {
    use crate::tab::AdjustedInput;
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;

    pane.set_guest_modal(&[client_id]);
    let outcome = press_key(&mut pane, BareKey::Enter, client_id);
    assert!(matches!(outcome, Some(AdjustedInput::GuestModalZoom)));

    pane.set_guest_modal(&[client_id]);
    press_key(&mut pane, BareKey::Down, client_id);
    let outcome = press_key(&mut pane, BareKey::Enter, client_id);
    assert!(matches!(outcome, Some(AdjustedInput::GuestModalDescend)));
}

#[test]
pub fn guest_modal_digit_shortcuts() {
    use crate::tab::AdjustedInput;
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;

    pane.set_guest_modal(&[client_id]);
    let outcome = press_key(&mut pane, BareKey::Char('1'), client_id);
    assert!(matches!(outcome, Some(AdjustedInput::GuestModalZoom)));

    pane.set_guest_modal(&[client_id]);
    let outcome = press_key(&mut pane, BareKey::Char('2'), client_id);
    assert!(matches!(outcome, Some(AdjustedInput::GuestModalDescend)));
}

#[test]
pub fn guest_modal_esc_descends() {
    use crate::tab::AdjustedInput;
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;
    pane.set_guest_modal(&[client_id]);
    let outcome = press_key(&mut pane, BareKey::Esc, client_id);
    assert!(matches!(outcome, Some(AdjustedInput::GuestModalDescend)));
}

#[test]
pub fn guest_modal_swallows_other_input() {
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;
    pane.set_guest_modal(&[client_id]);
    let outcome = press_key(&mut pane, BareKey::Char('x'), client_id);
    assert!(outcome.is_none());
    assert_eq!(pane.guest_modal_selection(client_id), Some(0));
}

#[test]
pub fn guest_modal_is_per_client() {
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_a = 1;
    let client_b = 2;
    pane.set_guest_modal(&[client_a, client_b]);
    press_key(&mut pane, BareKey::Down, client_a);
    assert_eq!(pane.guest_modal_selection(client_a), Some(1));
    assert_eq!(pane.guest_modal_selection(client_b), Some(0));
}

#[test]
pub fn guest_modal_no_modal_passes_input_through() {
    use zellij_utils::data::BareKey;
    let mut pane = create_guest_modal_pane();
    let client_id = 1;
    let outcome = press_key(&mut pane, BareKey::Char('x'), client_id);
    assert!(matches!(
        outcome,
        Some(crate::tab::AdjustedInput::WriteBytesToTerminal(_))
    ));
}

fn mirror_source_pane(cols: usize, rows: usize) -> TerminalPane {
    let mut geom = PaneGeom::default();
    geom.cols.set_inner(cols);
    geom.rows.set_inner(rows);
    TerminalPane::new(
        1,
        geom,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
pub fn a_mirror_snapshot_carries_styles_wide_characters_links_and_the_cursor() {
    use zellij_utils::structured_render::ATTR_BOLD;
    let mut pane = mirror_source_pane(20, 4);
    pane.handle_pty_bytes(
        "\u{1b}[1mbold\u{1b}[0m 中 \u{1b}]8;;https://zellij.dev\u{1b}\\link\u{1b}]8;;\u{1b}\\\r\nnext"
            .as_bytes()
            .to_vec(),
    );
    let screen = pane
        .mirror_screen()
        .expect("a terminal pane can be mirrored");
    assert_eq!((screen.cols, screen.rows), (20, 4));
    let bold = screen.cell(0, 0).unwrap();
    assert_eq!(bold.cell.ch, 'b' as u32);
    assert!(bold.cell.has(ATTR_BOLD));
    assert!(!screen.cell(5, 0).unwrap().cell.has(ATTR_BOLD));
    let wide = screen.cell(5, 0).unwrap();
    assert_eq!(wide.cell.ch, '中' as u32);
    assert_eq!(wide.cell.width, 2);
    let link = screen.cell(8, 0).unwrap();
    assert_eq!(link.cell.ch, 'l' as u32);
    assert_eq!(link.link.as_deref(), Some("https://zellij.dev"));
    assert_eq!(screen.cell(0, 1).unwrap().cell.ch, 'n' as u32);
    assert_eq!((screen.cursor.x, screen.cursor.y), (4, 1));
    assert!(screen.cursor.visible);
    assert!(!screen.wants_mouse);
}

#[test]
pub fn a_mirror_snapshot_reports_mouse_tracking_and_survives_encoding() {
    use zellij_utils::pane_mirror::{MirrorFrameEncoder, MirrorScreen};
    let mut pane = mirror_source_pane(10, 3);
    pane.handle_pty_bytes(b"\x1b[?1000hone\r\ntwo".to_vec());
    let first = pane.mirror_screen().unwrap();
    assert!(first.wants_mouse);
    let mut encoder = MirrorFrameEncoder::new();
    let full = encoder.encode(&first).unwrap();
    assert!(full.full);
    let mut viewer = MirrorScreen::from_frame(&full.bytes).unwrap();
    assert_eq!(viewer, first);
    pane.handle_pty_bytes(b"\r\nthree".to_vec());
    let second = pane.mirror_screen().unwrap();
    let delta = encoder.encode(&second).unwrap();
    assert!(!delta.full);
    let applied = viewer.apply_frame(&delta.bytes).unwrap();
    assert_eq!(
        applied.changed_rows.into_iter().collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(viewer, second);
    assert_eq!(encoder.encode(&pane.mirror_screen().unwrap()), None);
}

fn rendered_chunks(pane: &mut TerminalPane) -> Vec<crate::output::CharacterChunk> {
    pane.set_should_render(true);
    pane.render(None)
        .unwrap()
        .map(|(chunks, _, _, _)| chunks)
        .unwrap_or_default()
}

fn chunk_text(chunk: &crate::output::CharacterChunk) -> String {
    chunk
        .terminal_characters
        .iter()
        .map(|character| character.character)
        .collect()
}

fn chunk_width(chunk: &crate::output::CharacterChunk) -> usize {
    chunk
        .terminal_characters
        .iter()
        .map(|character| character.width())
        .sum()
}

fn pinned(cols: usize, rows: usize) -> Option<zellij_utils::pane_size::Size> {
    Some(zellij_utils::pane_size::Size { cols, rows })
}

#[test]
pub fn a_pinned_pane_gives_its_program_the_pinned_size_until_released() {
    let mut pane = mirror_source_pane(20, 4);
    assert_eq!((pane.program_columns(), pane.program_rows()), (20, 4));
    assert!(pane.set_mirror_pinned_size(pinned(10, 2)));
    assert!(!pane.set_mirror_pinned_size(pinned(10, 2)));
    assert_eq!((pane.program_columns(), pane.program_rows()), (10, 2));
    assert_eq!(
        (pane.get_content_columns(), pane.get_content_rows()),
        (20, 4)
    );
    let screen = pane.mirror_screen().unwrap();
    assert_eq!((screen.cols, screen.rows), (10, 2));
    let mut geom = pane.position_and_size();
    geom.rows.set_inner(1);
    pane.set_geom(geom);
    assert_eq!(
        (pane.program_columns(), pane.program_rows()),
        (10, 2),
        "a layout change does not undo the pin, even when the pane collapses"
    );
    assert_eq!(pane.mirror_screen().unwrap().rows, 2);
    assert!(pane.set_mirror_pinned_size(None));
    assert_eq!((pane.program_columns(), pane.program_rows()), (20, 1));
    assert_eq!(pane.mirror_screen().unwrap().cols, 20);
}

#[test]
pub fn a_pinned_program_larger_than_its_pane_is_cropped() {
    let mut pane = mirror_source_pane(10, 3);
    pane.set_mirror_pinned_size(pinned(20, 5));
    pane.handle_pty_bytes(b"abcdefghijklmnopqrst\r\nline two\r\n\r\n\r\nfifth".to_vec());
    let chunks = rendered_chunks(&mut pane);
    assert!(!chunks.is_empty());
    for chunk in &chunks {
        assert!(chunk.y < 3, "a row below the pane was drawn: {}", chunk.y);
        assert!(chunk.x + chunk_width(chunk) <= 10, "a chunk is too wide");
    }
    let first_row = chunks.iter().find(|chunk| chunk.y == 0).unwrap();
    assert_eq!(chunk_text(first_row), "abcdefghij");
    assert!(!chunks
        .iter()
        .any(|chunk| chunk_text(chunk).contains("fifth")));
}

#[test]
pub fn a_wide_character_on_the_crop_edge_becomes_a_blank() {
    let mut pane = mirror_source_pane(5, 2);
    pane.set_mirror_pinned_size(pinned(10, 2));
    pane.handle_pty_bytes("abcd中ef".as_bytes().to_vec());
    let chunks = rendered_chunks(&mut pane);
    let first_row = chunks.iter().find(|chunk| chunk.y == 0).unwrap();
    assert_eq!(chunk_text(first_row), "abcd ");
    assert_eq!(chunk_width(first_row), 5);
}

#[test]
pub fn a_pinned_program_smaller_than_its_pane_is_padded_on_full_redraws() {
    let mut pane = mirror_source_pane(10, 4);
    pane.set_mirror_pinned_size(pinned(6, 2));
    pane.handle_pty_bytes(b"left".to_vec());
    let chunks = rendered_chunks(&mut pane);
    for y in 0..2 {
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.y == y && chunk.x == 6 && chunk_width(chunk) == 4),
            "the right side of row {} is not padded",
            y
        );
    }
    for y in 2..4 {
        assert!(
            chunks
                .iter()
                .any(|chunk| chunk.y == y && chunk.x == 0 && chunk_width(chunk) == 10),
            "row {} below the program is not padded",
            y
        );
    }
    pane.handle_pty_bytes(b"!".to_vec());
    let partial = rendered_chunks(&mut pane);
    assert!(partial.iter().all(|chunk| chunk.y < 2 && chunk.x < 6));
}

#[test]
pub fn the_cursor_of_a_cropped_pane_is_hidden_outside_the_visible_area() {
    let mut pane = mirror_source_pane(10, 3);
    pane.set_mirror_pinned_size(pinned(20, 5));
    pane.handle_pty_bytes(b"\x1b[1;16H".to_vec());
    assert_eq!(Pane::cursor_coordinates(&pane, None), None);
    pane.handle_pty_bytes(b"\x1b[5;2H".to_vec());
    assert_eq!(Pane::cursor_coordinates(&pane, None), None);
    pane.handle_pty_bytes(b"\x1b[2;3H".to_vec());
    assert_eq!(Pane::cursor_coordinates(&pane, None), Some((2, 1, true)));
    pane.set_mirror_pinned_size(None);
    pane.handle_pty_bytes(b"\x1b[3;8H".to_vec());
    assert_eq!(Pane::cursor_coordinates(&pane, None), Some((7, 2, true)));
}

#[test]
pub fn the_change_counter_moves_only_when_the_pane_changes() {
    let mut pane = mirror_source_pane(10, 3);
    let before = pane.mirror_change_counter();
    assert_eq!(pane.mirror_change_counter(), before);
    let _ = pane.mirror_screen();
    let _ = rendered_chunks(&mut pane);
    let after_render = pane.mirror_change_counter();
    let _ = pane.render(None);
    assert_eq!(pane.mirror_change_counter(), after_render);
    pane.handle_pty_bytes(b"x".to_vec());
    let after_output = pane.mirror_change_counter();
    assert_ne!(after_output, after_render);
    pane.set_mirror_pinned_size(pinned(5, 2));
    assert_ne!(pane.mirror_change_counter(), after_output);
}

fn pane_for_split_test() -> TerminalPane {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(40);
    fake_win_size.rows.set_inner(10);
    TerminalPane::new(
        1,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
fn output_split_at_any_byte_renders_the_same_as_output_in_one_piece() {
    let stream: &[u8] = b"\x1b[31mred\x1b[0m plain \x1b_Gi=1,a=q;AAAA\x1b\\after\x1b[1;32mgreen\x1b[m\r\n\x1b[4mund\x1b[24m end\x1b";
    let mut whole = pane_for_split_test();
    whole.handle_pty_bytes(stream.to_vec());
    whole.handle_pty_bytes(b"[7mrev".to_vec());
    let expected = format!("{:?}", whole.grid);
    for split in 1..stream.len() {
        let mut pane = pane_for_split_test();
        pane.handle_pty_bytes(stream[..split].to_vec());
        pane.handle_pty_bytes(stream[split..].to_vec());
        pane.handle_pty_bytes(b"[7mrev".to_vec());
        assert_eq!(
            format!("{:?}", pane.grid),
            expected,
            "split at byte {}",
            split
        );
    }
}

/// A pane with the given pid, built the way the tests above build one.
#[cfg(test)]
fn pane_with_pid(pid: u32) -> TerminalPane {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(121);
    fake_win_size.rows.set_inner(20);
    TerminalPane::new(
        pid,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
fn a_pane_uuid_is_unique_to_the_pane() {
    // the uuid exists to name a pane across a restart, so two panes must never share one
    let first = pane_with_pid(1);
    let second = pane_with_pid(2);
    assert_ne!(first.pane_uuid(), second.pane_uuid());
    assert_ne!(first.pane_uuid(), uuid::Uuid::nil());
}

#[test]
fn a_pane_uuid_outlives_the_reuse_of_its_id() {
    // zellij hands a closed pane's id to the next pane. The uuid is what tells them apart, so a
    // second pane with the SAME pid must still get its own
    let first = pane_with_pid(7);
    let first_uuid = first.pane_uuid();
    drop(first);
    let reused = pane_with_pid(7);
    assert_ne!(reused.pane_uuid(), first_uuid);
}

#[test]
fn a_pane_uuid_does_not_change_under_the_pane() {
    // it is read on every serialization pass; a value that moved would name a different pane each
    // time it was written down
    let mut pane = pane_with_pid(1);
    let at_creation = pane.pane_uuid();
    pane.handle_pty_bytes(b"some output\r\n".to_vec());
    pane.set_geom(PaneGeom::default());
    pane.rename(b"renamed".to_vec());
    assert_eq!(pane.pane_uuid(), at_creation);
}

fn a_terminal_pane(pid: u32) -> TerminalPane {
    TerminalPane::new(
        pid,
        PaneGeom::default(),
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    )
}

#[test]
fn every_pane_has_a_handle_from_the_moment_it_is_built() {
    // the invariant the rest of the addressing surface rests on: there is no window in which a
    // pane exists without a name to reach it by
    let pane = a_terminal_pane(1);
    let handle = pane.pane_handle();
    assert!(!handle.is_empty(), "a pane was built without a handle");
    assert!(
        zellij_utils::pane_handle::is_handle_shaped(&handle),
        "not a handle: {}",
        handle
    );
}

#[test]
fn a_command_that_failed_leaves_its_own_note() {
    // the durable "this pane failed" mark: the frame's EXIT CODE line goes when the pane is rerun
    // or scrolled past, and nothing outside the server could ever read it
    use zellij_utils::data::NoteColor;
    use zellij_utils::input::command::RunCommand;
    let run_command = RunCommand {
        command: std::path::PathBuf::from("false"),
        ..Default::default()
    };
    let mut pane = a_terminal_pane(1);
    assert_eq!(pane.pane_note(), None, "a pane starts unmarked");
    pane.hold(Some(7), false, run_command.clone());
    assert_eq!(
        pane.pane_note(),
        Some(("exit 7".to_owned(), NoteColor::Error))
    );
    // and it goes when the pane is asked to run again: the mark belongs to the run that ended
    pane.rerun();
    assert_eq!(pane.pane_note(), None);
}

#[test]
fn a_command_that_succeeded_is_not_marked_and_neither_is_one_that_has_not_run() {
    // the negative controls. A pane held before its first run has nothing to report, and a
    // command that exited 0 did what it was asked - marking either would make the mark noise
    use zellij_utils::input::command::RunCommand;
    let run_command = RunCommand {
        command: std::path::PathBuf::from("true"),
        ..Default::default()
    };
    let mut succeeded = a_terminal_pane(1);
    succeeded.hold(Some(0), false, run_command.clone());
    assert_eq!(succeeded.pane_note(), None);
    let mut first_run = a_terminal_pane(2);
    first_run.hold(Some(7), true, run_command);
    assert_eq!(first_run.pane_note(), None);
}

#[test]
fn two_live_panes_do_not_share_a_handle() {
    let first = a_terminal_pane(1);
    let second = a_terminal_pane(2);
    assert_ne!(first.pane_handle(), second.pane_handle());
}

#[test]
fn a_restored_pane_is_put_back_under_the_handle_it_had() {
    // this is the whole point of the handle: the uuid rotates across a restore, the address does
    // not, so `zellij action ... --pane-id sunny-otter` still reaches the same pane afterwards
    let mut pane = a_terminal_pane(1);
    let generated = pane.pane_handle();
    pane.set_pane_handle("sunny-otter");
    assert_eq!(pane.pane_handle(), "sunny-otter");
    assert_ne!(pane.pane_handle(), generated);
}

#[test]
fn a_restored_pane_never_ends_up_at_a_handle_a_live_pane_holds() {
    let live = a_terminal_pane(1);
    let taken = live.pane_handle();
    let mut restoring = a_terminal_pane(2);
    restoring.set_pane_handle(&taken);
    assert_ne!(
        restoring.pane_handle(),
        taken,
        "a restore put two live panes at one address"
    );
    assert!(!restoring.pane_handle().is_empty());
}

#[test]
pub fn collapsed_stack_member_keeps_its_grid() {
    // A stack member that is collapsed to its title row keeps its pty at the size it had while
    // expanded, so its grid has to keep that size too. If the grid is reflowed down to the
    // one-row collapsed geometry, the program in the pane goes on drawing full-height frames
    // into a one-row grid and every redraw is scrolled into the scrollback instead of
    // overwriting the screen.
    let mut expanded = PaneGeom::default();
    expanded.cols.set_inner(80);
    expanded.rows.set_inner(20);
    expanded.stacked = Some(0);

    let pid = 1;
    let style = Style::default();
    let sixel_image_store = Rc::new(RefCell::new(SixelImageStore::default()));
    let terminal_emulator_colors = Rc::new(RefCell::new(Palette::default()));
    let terminal_emulator_color_codes = Rc::new(RefCell::new(HashMap::new()));
    let debug = false;
    let arrow_fonts = true;
    let styled_underlines = true;
    let osc8_hyperlinks = true;
    let explicitly_disable_kitty_keyboard_protocol = false;
    let mut terminal_pane = TerminalPane::new(
        pid,
        expanded,
        style,
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        sixel_image_store,
        Rc::new(RefCell::new(KittyImageStore::default())),
        terminal_emulator_colors,
        terminal_emulator_color_codes,
        None,
        None,
        debug,
        arrow_fonts,
        styled_underlines,
        osc8_hyperlinks,
        explicitly_disable_kitty_keyboard_protocol,
        None,
    );

    // one row of the stack goes to the member's title, as it does in the titles frame styles
    terminal_pane.set_content_offset(Offset::shift_right_top_and_bottom(0, 1, 0));

    let mut content = String::new();
    for line in 0..19 {
        write!(&mut content, "line {:02}\n\r", line).unwrap();
    }
    terminal_pane.handle_pty_bytes(content.into_bytes());

    let expanded_rows = terminal_pane.grid.height;
    let expanded_cols = terminal_pane.grid.width;
    let expanded_screen = terminal_pane.dump_screen(false, None);
    assert_eq!(expanded_rows, 19, "19 content rows under a 1 row title");
    assert!(
        expanded_screen.contains("line 18"),
        "the pane drew its last line before collapsing"
    );

    // collapse it: a fixed single row, which is what a stack gives a member it is not showing
    let mut collapsed = expanded;
    collapsed.rows = Dimension::fixed(1);
    assert!(
        collapsed.is_collapsed_stack_member(),
        "the test collapses the pane the way a stack does"
    );
    terminal_pane.set_geom(collapsed);
    // Replay what `TiledPanes::set_pane_frames` does to a collapsed member under a titles frame
    // style, in order. A fixed-height member is not flexible, so the upstream branch reserves no
    // title row for it and hands it an offset with `top: 0` - one content row over a one row
    // geometry. The fork then clamps `top` to 1 so the member has no content rows at all and its
    // pty is left alone. It is the first of those two offsets that used to squash the grid.
    terminal_pane.set_content_offset(Offset::default());
    terminal_pane.set_content_offset(Offset::shift_right_top_and_bottom(0, 1, 0));

    assert_eq!(
        terminal_pane.grid.height, expanded_rows,
        "a collapsed member keeps its grid rows"
    );
    assert_eq!(
        terminal_pane.grid.width, expanded_cols,
        "a collapsed member keeps its grid columns"
    );
    assert_eq!(
        terminal_pane.dump_screen(false, None),
        expanded_screen,
        "a collapsed member keeps its grid contents"
    );

    // ... and re-expanding to the same geometry gets the same screen back
    terminal_pane.set_geom(expanded);
    terminal_pane.set_content_offset(Offset::shift_right_top_and_bottom(0, 1, 0));

    assert_eq!(terminal_pane.grid.height, expanded_rows);
    assert_eq!(terminal_pane.grid.width, expanded_cols);
    assert_eq!(
        terminal_pane.dump_screen(false, None),
        expanded_screen,
        "re-expanding a collapsed member gets its screen back"
    );
}

#[test]
pub fn dump_screen_returns_logical_lines_not_the_wrapped_grid() {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(20);
    fake_win_size.rows.set_inner(10);

    let mut terminal_pane = TerminalPane::new(
        1,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    );

    // 50 characters into a 20 column pane: the grid holds three rows, two of them continuations
    let long_line = "a".repeat(50);
    terminal_pane.handle_pty_bytes(format!("{}\n\rshort", long_line).into_bytes());

    let dump = terminal_pane.dump_screen(false, None);
    let lines: Vec<&str> = dump.lines().collect();
    assert_eq!(
        lines,
        vec![long_line.as_str(), "short"],
        "dump-screen already unwraps: it breaks a line only where the grid says the line began"
    );
}

#[test]
pub fn a_trzsz_trigger_lands_on_the_grid_marked_as_multiplexed() {
    let mut fake_win_size = PaneGeom::default();
    fake_win_size.cols.set_inner(80);
    fake_win_size.rows.set_inner(10);

    let mut terminal_pane = TerminalPane::new(
        1,
        fake_win_size,
        Style::default(),
        0,
        String::new(),
        Rc::new(RefCell::new(LinkHandler::new())),
        Rc::new(RefCell::new(None)),
        Rc::new(RefCell::new(SixelImageStore::default())),
        Rc::new(RefCell::new(KittyImageStore::default())),
        Rc::new(RefCell::new(Palette::default())),
        Rc::new(RefCell::new(HashMap::new())),
        None,
        None,
        false,
        true,
        true,
        true,
        false,
        None,
    );

    terminal_pane
        .handle_pty_bytes(b"\x1b[s::TRZSZ:TRANSFER:R:1.2.0:1760180000000:41235\r\n".to_vec());

    let dump = terminal_pane.dump_screen(false, None);
    assert!(
        dump.contains("::TRZSZ:TRANSFER:R:1.2.0:1760180000020:41235"),
        "a redraw of this row sends an id the client starts only once, got {:?}",
        dump
    );
}

/// Typed input that reaches the shell marks the pane's last prompt as drawn before it, and the next
/// prompt marker clears that. A bracketed-paste marker the pane drops reaches nothing, and marks
/// nothing.
#[test]
pub fn typed_input_marks_the_last_prompt_as_stale_until_the_next_one() {
    let mut pane = create_guest_modal_pane();
    pane.handle_pty_bytes("\x1b]133;A\x07$ \x1b]133;B\x07".as_bytes().to_vec());
    assert!(!pane.input_since_prompt());

    pane.adjust_input_to_terminal(&None, b"\x1b[200~".to_vec(), false, Some(1));
    assert!(
        !pane.input_since_prompt(),
        "a paste marker the shell does not take is not input"
    );

    pane.adjust_input_to_terminal(&None, b"ls\r".to_vec(), false, Some(1));
    assert!(pane.input_since_prompt());

    pane.handle_pty_bytes(
        "\x1b]133;C\x07\r\nfile\r\n\x1b]133;D;0\x07\x1b]133;A\x07$ \x1b]133;B\x07"
            .as_bytes()
            .to_vec(),
    );
    assert!(!pane.input_since_prompt());
}

/// A prefilled line is held until the pane's input touches it. A bracketed-paste marker the pane
/// drops reaches nothing and leaves it; a key clears it, and so does the shell starting a command.
#[test]
pub fn a_prefilled_command_is_kept_until_input_or_a_command_start() {
    let mut pane = create_guest_modal_pane();
    assert_eq!(pane.prefilled_command(), None);

    pane.set_prefilled_command(Some("claude --resume abc".to_owned()));
    pane.adjust_input_to_terminal(&None, b"\x1b[200~".to_vec(), false, Some(1));
    assert_eq!(
        pane.prefilled_command().as_deref(),
        Some("claude --resume abc"),
        "a paste marker the shell does not take is not input"
    );
    pane.adjust_input_to_terminal(&None, b"\r".to_vec(), false, Some(1));
    assert_eq!(pane.prefilled_command(), None, "a key clears it");

    pane.set_prefilled_command(Some("claude --resume abc".to_owned()));
    pane.handle_pty_bytes("\x1b]133;C\x07".as_bytes().to_vec());
    assert_eq!(pane.prefilled_command(), None, "a command start clears it");
}
