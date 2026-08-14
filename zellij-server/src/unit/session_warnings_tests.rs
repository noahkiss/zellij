use super::*;

#[test]
fn nothing_is_asked_for_when_nothing_is_wanted() {
    // the settings are a process-global recorded once at server start, so this exercises the
    // gating through the same door the server uses. FDA is opt-in; the build question is not
    record_settings(WarningSettings {
        expect_full_disk_access: false,
        stale_build_notice: false,
        pinned_exe: None,
    });
    assert!(
        current_warnings().is_empty(),
        "a session that asked for neither question gets neither warning"
    );
}

#[test]
fn codes_are_short_and_distinct() {
    // the badge shares a line with the tab list, so a code that grew would cost tab columns
    assert_eq!(SessionWarning::SupersededBuild.code(), "zj");
    assert_eq!(SessionWarning::MissingFullDiskAccess.code(), "TCC");
}

#[test]
fn the_drawing_order_is_the_variant_order() {
    // a bar showing both must not swap them between frames
    assert!(SessionWarning::SupersededBuild < SessionWarning::MissingFullDiskAccess);
}
