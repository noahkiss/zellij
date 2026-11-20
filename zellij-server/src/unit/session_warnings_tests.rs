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

/// The superseded path end to end, on a scratch build rather than the test binary: a pin that
/// something renamed a new build over after the server started. This is the macOS shape of the
/// bug, where the path exists again and only the recorded identity and the mtime tell them apart.
#[test]
#[cfg(unix)]
fn a_pin_refreshed_under_the_server_lights_the_badge() {
    use zellij_utils::session_lifecycle::{build_superseded_since, RunningBuild};

    let scratch = tempfile::tempdir().unwrap();
    let pin = scratch.path().join("zellij");
    std::fs::write(&pin, b"the build the server started as").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(600);
    std::fs::File::options()
        .write(true)
        .open(&pin)
        .unwrap()
        .set_modified(old)
        .unwrap();
    let socket = scratch.path().join("mysession");
    std::fs::write(&socket, b"").unwrap();
    let build = RunningBuild::taken_now(pin.clone(), Some(socket));
    let settings = WarningSettings {
        expect_full_disk_access: false,
        stale_build_notice: true,
        pinned_exe: Some(pin.clone()),
    };
    let dirs = vec![scratch.path().to_path_buf()];
    assert!(
        warnings_for(&settings, |pinned| build_superseded_since(
            &build, pinned, &dirs
        ))
        .is_empty(),
        "the pin still holds the build the server started as"
    );

    let newer = scratch.path().join("zellij-newer");
    std::fs::write(&newer, b"a newer build, written by the upgrade").unwrap();
    std::fs::rename(&newer, &pin).unwrap();
    assert_eq!(
        warnings_for(&settings, |pinned| build_superseded_since(
            &build, pinned, &dirs
        )),
        vec![SessionWarning::SupersededBuild]
    );
}
