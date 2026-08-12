use super::*;

fn viewport(cols: usize, rows: usize) -> Size {
    Size { cols, rows }
}

fn notices() -> StatusNotices {
    StatusNotices::new(vec![
        "⚠ Full Disk Access not granted for /Users/x/bin/zellij".to_owned(),
        "⚠ session 'main' runs an old build".to_owned(),
    ])
}

#[test]
fn notices_sit_against_the_right_edge() {
    let style = Style::default();
    let chunks = notices().character_chunks(viewport(100, 30), &style);
    assert_eq!(chunks.len(), 2, "one chunk per notice");
    for (row, chunk) in chunks.iter().enumerate() {
        assert_eq!(chunk.y, row, "notices stack from the top");
        let width: usize = chunk.terminal_characters.len();
        assert_eq!(
            chunk.x + width,
            100 - RIGHT_MARGIN,
            "the line ends one column short of the edge"
        );
    }
}

#[test]
fn a_narrow_viewport_truncates_rather_than_wraps() {
    let style = Style::default();
    let chunks = notices().character_chunks(viewport(30, 30), &style);
    for chunk in &chunks {
        assert!(
            chunk.terminal_characters.len() <= 30 - RIGHT_MARGIN,
            "a notice never runs past the viewport"
        );
    }
    let first: String = chunks[0]
        .terminal_characters
        .iter()
        .map(|character| character.character)
        .collect();
    assert!(
        first.ends_with('…'),
        "a cut line says it was cut: {}",
        first
    );
}

#[test]
fn a_very_narrow_viewport_draws_nothing() {
    // wrapping across the top of the panes is worse than saying nothing
    let style = Style::default();
    assert!(notices()
        .character_chunks(viewport(MINIMUM_COLUMNS - 1, 30), &style)
        .is_empty());
    assert_eq!(notices().rows_covered(viewport(MINIMUM_COLUMNS - 1, 30)), 0);
}

#[test]
fn no_notices_draw_nothing_and_cover_nothing() {
    let style = Style::default();
    let empty = StatusNotices::default();
    assert!(empty.is_empty());
    assert!(empty.character_chunks(viewport(100, 30), &style).is_empty());
    assert_eq!(empty.rows_covered(viewport(100, 30)), 0);
}

#[test]
fn more_notices_than_rows_are_capped_by_the_viewport() {
    let style = Style::default();
    let chunks = notices().character_chunks(viewport(100, 1), &style);
    assert_eq!(chunks.len(), 1, "a notice never draws past the last row");
    assert_eq!(notices().rows_covered(viewport(100, 1)), 1);
}

#[test]
fn nothing_is_asked_for_when_nothing_is_wanted() {
    // the settings are a process-global recorded once at server start, so this exercises the
    // gating through the same door the server uses. FDA is opt-in; the build notice is not
    let settings = NoticeSettings {
        expect_full_disk_access: false,
        stale_build_notice: false,
        pinned_exe: None,
    };
    record_settings(settings);
    assert!(
        current_notices("main").is_empty(),
        "a session that asked for neither notice gets neither"
    );
}
