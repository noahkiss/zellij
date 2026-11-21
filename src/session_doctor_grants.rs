//! The macOS privacy grants the server needs, and the hidden probe that asks for them.
//!
//! A grant belongs to the RESPONSIBLE process: the server, running the pinned binary under its
//! launch agent. A program started in one of its panes is judged against the server's grants, so
//! the doctor's pane probe runs this binary as `zellij session doctor --grant-probe check`, and the
//! answers it prints are the server's. Run from a terminal, the same probe answers for the
//! terminal instead.
//!
//! Five grants, and macOS treats each one differently:
//!
//! | grant | check | request |
//! |---|---|---|
//! | Full Disk Access | opening the system TCC directory (the pane script) | none: Settings only |
//! | Accessibility | `AXIsProcessTrusted` | `AXIsProcessTrustedWithOptions`, prompt on |
//! | Screen Recording | `CGPreflightScreenCaptureAccess` | `CGRequestScreenCaptureAccess` |
//! | App Management | none: unknown | none: Settings only |
//! | Microphone | `+[AVCaptureDevice authorizationStatusForMediaType:]` | none: information only |
//!
//! A request call lists the server in its Settings pane and shows macOS's own dialog. It never
//! turns the grant on: only a person can, in System Settings.
//!
//! The Microphone line is information only. The server does not need the grant: a program in a
//! pane that wants the microphone asks for it itself, and macOS prompts for it from a pane. So the
//! line never needs a person, never changes doctor's exit code, and `--fix` never asks for it.
//!
//! The parsing and the choice of which grant to act on are compiled everywhere so their tests run
//! everywhere; only macOS calls them.

#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Duration;

use zellij_utils::session_doctor::{DoctorMode, Finding};

use crate::session_doctor_probe::finished_line;

/// The `--grant-probe` value that prints every state this probe can check.
pub(crate) const CHECK: &str = "check";

/// How long one framework call may take before the probe prints `unknown` for it.
///
/// Well inside the pane probe's own deadline: the grant lines are written before the Full Disk
/// Access line, so a call that stalled would otherwise read as a Full Disk Access refusal.
pub(crate) const GRANT_CALL_TIMEOUT: Duration = Duration::from_secs(2);

/// One privacy grant the server needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Grant {
    FullDiskAccess,
    Accessibility,
    ScreenRecording,
    AppManagement,
    /// Information only: never in [`Grant::FIXABLE`].
    Microphone,
}

impl Grant {
    /// The grants a `--fix` acts on, in the order it acts on them: one per run.
    pub(crate) const FIXABLE: [Grant; 3] = [
        Grant::FullDiskAccess,
        Grant::Accessibility,
        Grant::ScreenRecording,
    ];

    /// The report's check name.
    pub(crate) fn check(self) -> &'static str {
        match self {
            Grant::FullDiskAccess => "fda",
            Grant::Accessibility => "a11y",
            Grant::ScreenRecording => "screen",
            Grant::AppManagement => "appmgmt",
            Grant::Microphone => "mic",
        }
    }

    /// The grant's name in System Settings.
    pub(crate) fn title(self) -> &'static str {
        match self {
            Grant::FullDiskAccess => "Full Disk Access",
            Grant::Accessibility => "Accessibility",
            Grant::ScreenRecording => "Screen Recording",
            Grant::AppManagement => "App Management",
            Grant::Microphone => "Microphone",
        }
    }

    /// The grant's pane as a person finds it in System Settings. Newer macOS renamed two panes, so
    /// the line names both names; the deep link opens the same pane on either.
    pub(crate) fn settings_name(self) -> &'static str {
        match self {
            Grant::Accessibility => {
                "Accessibility (named Device Control & Data Access on macOS 27)"
            },
            Grant::ScreenRecording => {
                "Screen Recording (Screen & System Audio Recording on newer macOS)"
            },
            other => other.title(),
        }
    }

    /// The deep link to the grant's pane in System Settings.
    pub(crate) fn settings_url(self) -> &'static str {
        match self {
            Grant::FullDiskAccess => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
            },
            Grant::Accessibility => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"
            },
            Grant::ScreenRecording => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
            },
            Grant::AppManagement => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_AppBundles"
            },
            Grant::Microphone => {
                "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone"
            },
        }
    }

    /// The `--grant-probe` value that asks macOS for this grant, where macOS has a request call.
    pub(crate) fn request_arg(self) -> Option<&'static str> {
        match self {
            Grant::Accessibility => Some("request-accessibility"),
            Grant::ScreenRecording => Some("request-screen-recording"),
            Grant::FullDiskAccess | Grant::AppManagement | Grant::Microphone => None,
        }
    }

    /// The key the probe prints this grant's state under, where it can check it.
    fn key(self) -> Option<&'static str> {
        match self {
            Grant::Accessibility => Some("a11y="),
            Grant::ScreenRecording => Some("screen="),
            // read by `parse_microphone`, which has four answers
            Grant::FullDiskAccess | Grant::AppManagement | Grant::Microphone => None,
        }
    }
}

/// What the probe said about one grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum GrantState {
    Granted,
    Missing,
    /// No answer: no line, a line that is not `yes` or `no`, or a grant macOS cannot be asked about.
    Unknown,
}

/// Read one grant's state from the pane probe's answer file.
pub(crate) fn parse_grant(written: &str, grant: Grant) -> GrantState {
    let Some(key) = grant.key() else {
        return GrantState::Unknown;
    };
    match finished_line(written, key) {
        Some("yes") => GrantState::Granted,
        Some("no") => GrantState::Missing,
        _ => GrantState::Unknown,
    }
}

/// What the probe said about the Microphone: the four `AVAuthorizationStatus` answers that matter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MicState {
    /// `mic=yes`: authorized.
    Granted,
    /// `mic=ask`: macOS has never asked, so the first program that asks gets its prompt.
    NotAsked,
    /// `mic=no`: denied, or restricted by policy.
    Denied,
    /// No answer: no line, a line it does not know, or a call that did not return.
    Unknown,
}

/// The key the probe prints the Microphone answer under.
const MIC_KEY: &str = "mic=";

/// The `mic=` value for an `AVAuthorizationStatus`, as
/// `+[AVCaptureDevice authorizationStatusForMediaType:]` returns it.
pub(crate) fn av_status_answer(status: isize) -> &'static str {
    match status {
        0 => "ask",    // notDetermined
        1 | 2 => "no", // restricted, denied
        3 => "yes",    // authorized
        _ => "unknown",
    }
}

/// Read the Microphone answer from the pane probe's answer file.
pub(crate) fn parse_microphone(written: &str) -> MicState {
    match finished_line(written, MIC_KEY) {
        Some("yes") => MicState::Granted,
        Some("ask") => MicState::NotAsked,
        Some("no") => MicState::Denied,
        _ => MicState::Unknown,
    }
}

/// The Microphone line. Always `Already correct`: it informs, and asks nothing of a person.
pub(crate) fn microphone_finding(state: MicState) -> Finding {
    let grant = Grant::Microphone;
    match state {
        MicState::Granted => Finding::ok(
            grant.check(),
            "granted: programs in panes can use the microphone",
        ),
        MicState::NotAsked => Finding::ok(
            grant.check(),
            "not asked yet: the first program that asks gets the macOS prompt",
        ),
        MicState::Denied => Finding::ok(
            grant.check(),
            "denied: programs in panes cannot use the microphone; turn the server on in",
        )
        .note(format!(
            "System Settings > Privacy & Security > {} ({})",
            grant.settings_name(),
            grant.settings_url()
        )),
        MicState::Unknown => Finding::ok(grant.check(), "unknown"),
    }
}

/// The grant a `--fix` run acts on: the first missing one in [`Grant::FIXABLE`] order.
///
/// One per run, so a person faces one Settings pane and one dialog at a time.
pub(crate) fn grant_to_fix(states: &[(Grant, GrantState)]) -> Option<Grant> {
    Grant::FIXABLE.into_iter().find(|grant| {
        states
            .iter()
            .any(|(candidate, state)| candidate == grant && *state == GrantState::Missing)
    })
}

/// The notes a missing grant's finding carries about the fix.
///
/// `acted` is what the fix did, and is read only when this run was allowed to act: `--fix` and a
/// terminal on stdin. A run with no terminal opens nothing, as the setup script never did, since
/// nobody may be there to answer the dialog.
pub(crate) fn fix_notes(
    grant: Grant,
    chosen: Option<Grant>,
    mode: DoctorMode,
    interactive: bool,
    acted: Vec<String>,
) -> Vec<String> {
    let mut notes = vec![
        format!(
            "System Settings > Privacy & Security > {}:",
            grant.settings_name()
        ),
        String::from("add the EXACT path the server runs, at:"),
        grant.settings_url().to_owned(),
    ];
    if chosen != Some(grant) {
        if let Some(first) = chosen {
            notes.push(format!(
                "`--fix` acts on one grant per run; this one comes after {}",
                first.title()
            ));
        }
        return notes;
    }
    if !mode.fix {
        notes.push(mode.describe(&fix_description(grant)));
    } else if !interactive {
        notes.push(String::from(
            "nothing was opened: no terminal to answer. Run `--fix` from a terminal",
        ));
    } else {
        notes.extend(acted);
    }
    notes
}

/// What the fix for `grant` does, in the words `DoctorMode::describe` tenses.
fn fix_description(grant: Grant) -> String {
    match grant.request_arg() {
        Some(_) => format!(
            "ask macOS to list the server under {} and open that pane",
            grant.title()
        ),
        None => format!("open System Settings at {}", grant.title()),
    }
}

/// `zellij session doctor --grant-probe <what>`: print the states, or make one request.
///
/// `check` prints `a11y=` and `screen=` lines, each `yes`, `no` or `unknown`, then a `mic=` line,
/// `yes`, `ask` (never asked), `no` or `unknown`. A request prints
/// `requested=<check>` once the call returns. Exit 0, or 2 for a value it does not know or a
/// platform with no such grants.
pub(crate) fn run_grant_probe(what: &str) -> i32 {
    #[cfg(target_os = "macos")]
    {
        let line = |key: &str, answer: Option<bool>| {
            let state = match answer {
                Some(true) => "yes",
                Some(false) => "no",
                None => "unknown",
            };
            println!("{}{}", key, state);
        };
        match what {
            CHECK => {
                line("a11y=", bounded(ffi::accessibility));
                line("screen=", bounded(ffi::screen_recording));
                let mic = bounded(ffi::microphone_status).flatten();
                println!("{}{}", MIC_KEY, mic.map_or("unknown", av_status_answer));
                0
            },
            "request-accessibility" => {
                let _ = bounded(ffi::request_accessibility);
                println!("requested={}", Grant::Accessibility.check());
                0
            },
            "request-screen-recording" => {
                let _ = bounded(ffi::request_screen_recording);
                println!("requested={}", Grant::ScreenRecording.check());
                0
            },
            other => {
                eprintln!("unknown grant probe: {}", other);
                2
            },
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("--grant-probe {}: there are no such grants here", what);
        2
    }
}

/// One framework call, given [`GRANT_CALL_TIMEOUT`] to return. `None` when it did not.
#[cfg(target_os = "macos")]
fn bounded<T: Send + 'static>(call: fn() -> T) -> Option<T> {
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(call());
    });
    receiver.recv_timeout(GRANT_CALL_TIMEOUT).ok()
}

#[cfg(target_os = "macos")]
mod ffi {
    use std::ffi::{c_void, CStr};

    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    type CFTypeRef = *const c_void;

    /// `CFDictionaryKeyCallBacks` and `CFDictionaryValueCallBacks`: a version and five function
    /// pointers. Only their addresses are passed.
    #[repr(C)]
    struct CFDictionaryCallBacks {
        _fields: [usize; 6],
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        static kCFBooleanTrue: CFTypeRef;
        static kCFTypeDictionaryKeyCallBacks: CFDictionaryCallBacks;
        static kCFTypeDictionaryValueCallBacks: CFDictionaryCallBacks;
        fn CFDictionaryCreate(
            allocator: CFTypeRef,
            keys: *const CFTypeRef,
            values: *const CFTypeRef,
            count: isize,
            key_callbacks: *const CFDictionaryCallBacks,
            value_callbacks: *const CFDictionaryCallBacks,
        ) -> CFTypeRef;
        fn CFRelease(value: CFTypeRef);
    }

    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        static kAXTrustedCheckOptionPrompt: CFTypeRef;
        fn AXIsProcessTrusted() -> u8;
        fn AXIsProcessTrustedWithOptions(options: CFTypeRef) -> u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub(super) fn accessibility() -> bool {
        unsafe { AXIsProcessTrusted() != 0 }
    }

    /// Lists the responsible process under Accessibility and shows macOS's dialog, when it is not
    /// trusted yet.
    pub(super) fn request_accessibility() -> bool {
        unsafe {
            let keys = [kAXTrustedCheckOptionPrompt];
            let values = [kCFBooleanTrue];
            let options = CFDictionaryCreate(
                std::ptr::null(),
                keys.as_ptr(),
                values.as_ptr(),
                1,
                &kCFTypeDictionaryKeyCallBacks,
                &kCFTypeDictionaryValueCallBacks,
            );
            let trusted = AXIsProcessTrustedWithOptions(options) != 0;
            if !options.is_null() {
                CFRelease(options);
            }
            trusted
        }
    }

    pub(super) fn screen_recording() -> bool {
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    /// Lists the responsible process under Screen Recording and shows macOS's dialog, when it is
    /// not granted yet.
    pub(super) fn request_screen_recording() -> bool {
        unsafe { CGRequestScreenCaptureAccess() }
    }

    const AV_FOUNDATION: &CStr = c"/System/Library/Frameworks/AVFoundation.framework/AVFoundation";

    /// `+[AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio]`, the responsible
    /// process's `AVAuthorizationStatus`. `None` when AVFoundation, the constant or the class is
    /// missing.
    ///
    /// AVFoundation is opened here, at run time, so that no other launch of zellij loads it.
    pub(super) fn microphone_status() -> Option<isize> {
        unsafe {
            // never closed: the probe exits right after, and the class lives in the image
            let handle = libc::dlopen(AV_FOUNDATION.as_ptr(), libc::RTLD_LAZY);
            if handle.is_null() {
                return None;
            }
            // the symbol is the address of an `NSString *const`
            let symbol =
                libc::dlsym(handle, c"AVMediaTypeAudio".as_ptr()) as *const *const AnyObject;
            let audio = symbol.as_ref()?.as_ref()?;
            let class = AnyClass::get(c"AVCaptureDevice")?;
            let status: isize = msg_send![class, authorizationStatusForMediaType: audio];
            Some(status)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(fda: GrantState, a11y: GrantState, screen: GrantState) -> Vec<(Grant, GrantState)> {
        vec![
            (Grant::FullDiskAccess, fda),
            (Grant::Accessibility, a11y),
            (Grant::ScreenRecording, screen),
        ]
    }

    #[test]
    fn the_probe_lines_parse_to_states_and_anything_else_is_unknown() {
        let written = "manager=Aqua\na11y=yes\nscreen=no\nfda=yes\n";
        assert_eq!(
            parse_grant(written, Grant::Accessibility),
            GrantState::Granted
        );
        assert_eq!(
            parse_grant(written, Grant::ScreenRecording),
            GrantState::Missing
        );
        assert_eq!(
            parse_grant("a11y=unknown\n", Grant::Accessibility),
            GrantState::Unknown
        );
        // a binary that could not run in the pane writes no grant lines at all
        assert_eq!(
            parse_grant("manager=Aqua\nfda=yes\n", Grant::Accessibility),
            GrantState::Unknown
        );
        // half a line is not an answer
        assert_eq!(
            parse_grant("a11y=ye", Grant::Accessibility),
            GrantState::Unknown
        );
        // macOS cannot be asked about these two in the probe
        assert_eq!(
            parse_grant(written, Grant::AppManagement),
            GrantState::Unknown
        );
        assert_eq!(
            parse_grant(written, Grant::FullDiskAccess),
            GrantState::Unknown
        );
    }

    #[test]
    fn fix_acts_on_the_first_missing_grant_only() {
        use GrantState::*;
        assert_eq!(grant_to_fix(&states(Granted, Granted, Granted)), None);
        assert_eq!(
            grant_to_fix(&states(Missing, Missing, Missing)),
            Some(Grant::FullDiskAccess)
        );
        assert_eq!(
            grant_to_fix(&states(Granted, Missing, Missing)),
            Some(Grant::Accessibility)
        );
        assert_eq!(
            grant_to_fix(&states(Unknown, Unknown, Missing)),
            Some(Grant::ScreenRecording)
        );
        // App Management is never acted on: there is nothing to ask
        assert_eq!(grant_to_fix(&[(Grant::AppManagement, Missing)]), None);
    }

    #[test]
    fn a_dry_run_says_what_the_fix_would_do_and_does_nothing() {
        let dry = DoctorMode::from_flags(true, false, false);
        let acted = vec![String::from("must not appear")];
        let notes = fix_notes(
            Grant::Accessibility,
            Some(Grant::Accessibility),
            dry,
            true,
            acted.clone(),
        );
        assert!(notes.iter().any(|note| note
            == "would ask macOS to list the server under Accessibility and open that pane"));
        assert!(!notes.contains(&acted[0]));
        let notes = fix_notes(
            Grant::FullDiskAccess,
            Some(Grant::FullDiskAccess),
            DoctorMode::from_flags(false, true, false),
            true,
            vec![],
        );
        assert!(notes
            .iter()
            .any(|note| note == "would open System Settings at Full Disk Access"));
    }

    #[test]
    fn a_fix_with_no_terminal_opens_nothing_and_says_so() {
        let notes = fix_notes(
            Grant::ScreenRecording,
            Some(Grant::ScreenRecording),
            DoctorMode::default(),
            false,
            vec![String::from("must not appear")],
        );
        assert!(notes.iter().any(|note| note.contains("no terminal")));
        assert!(!notes.iter().any(|note| note == "must not appear"));
    }

    #[test]
    fn a_fix_from_a_terminal_reports_what_it_did_and_defers_the_rest() {
        let acted = vec![String::from("opened System Settings at Accessibility")];
        let notes = fix_notes(
            Grant::Accessibility,
            Some(Grant::Accessibility),
            DoctorMode::default(),
            true,
            acted.clone(),
        );
        assert!(notes.contains(&acted[0]));
        assert!(notes
            .iter()
            .any(|note| note.contains("Privacy_Accessibility")));
        let later = fix_notes(
            Grant::ScreenRecording,
            Some(Grant::Accessibility),
            DoctorMode::default(),
            true,
            vec![],
        );
        assert!(later
            .iter()
            .any(|note| note.contains("this one comes after Accessibility")));
    }

    #[test]
    fn the_mic_line_has_four_answers() {
        let written = "manager=Aqua\na11y=yes\nscreen=yes\nmic=ask\nfda=yes\n";
        assert_eq!(parse_microphone(written), MicState::NotAsked);
        assert_eq!(parse_microphone("mic=yes\n"), MicState::Granted);
        assert_eq!(parse_microphone("mic=no\n"), MicState::Denied);
        assert_eq!(parse_microphone("mic=unknown\n"), MicState::Unknown);
        // an older binary in the pane prints no mic line at all
        assert_eq!(
            parse_microphone("a11y=yes\nscreen=yes\n"),
            MicState::Unknown
        );
        // half a line is not an answer
        assert_eq!(parse_microphone("mic=ye"), MicState::Unknown);
        // the Microphone is not read as a grant
        assert_eq!(parse_grant(written, Grant::Microphone), GrantState::Unknown);
    }

    #[test]
    fn every_av_status_maps_to_a_mic_answer() {
        // notDetermined, restricted, denied, authorized
        let answers: Vec<_> = (0..=3).map(av_status_answer).collect();
        assert_eq!(answers, ["ask", "no", "no", "yes"]);
        assert_eq!(av_status_answer(4), "unknown");
        assert_eq!(av_status_answer(-1), "unknown");
        // every defined status prints a line the doctor reads back as an answer
        for status in 0..=3 {
            let line = format!("{}{}\n", MIC_KEY, av_status_answer(status));
            assert_ne!(parse_microphone(&line), MicState::Unknown, "{}", status);
        }
    }

    #[test]
    fn the_mic_line_is_information_and_never_needs_a_person() {
        use zellij_utils::session_doctor::Status;
        for state in [
            MicState::Granted,
            MicState::NotAsked,
            MicState::Denied,
            MicState::Unknown,
        ] {
            let finding = microphone_finding(state);
            assert_eq!(finding.check, "mic");
            assert_eq!(finding.status, Status::AlreadyCorrect, "{:?}", state);
        }
        assert_eq!(
            microphone_finding(MicState::Granted).message,
            "granted: programs in panes can use the microphone"
        );
        assert_eq!(
            microphone_finding(MicState::NotAsked).message,
            "not asked yet: the first program that asks gets the macOS prompt"
        );
        let denied = microphone_finding(MicState::Denied);
        assert!(denied
            .message
            .starts_with("denied: programs in panes cannot use the microphone"));
        assert!(denied.notes[0].contains("Privacy & Security > Microphone"));
        assert!(denied.notes[0].contains("Privacy_Microphone"));
        assert_eq!(microphone_finding(MicState::Unknown).message, "unknown");
        // and --fix never acts on it
        assert!(!Grant::FIXABLE.contains(&Grant::Microphone));
        assert_eq!(Grant::Microphone.request_arg(), None);
        assert_eq!(
            grant_to_fix(&[(Grant::Microphone, GrantState::Missing)]),
            None
        );
    }

    #[test]
    fn every_grant_names_its_own_settings_pane() {
        for (grant, anchor) in [
            (Grant::FullDiskAccess, "Privacy_AllFiles"),
            (Grant::Accessibility, "Privacy_Accessibility"),
            (Grant::ScreenRecording, "Privacy_ScreenCapture"),
            (Grant::AppManagement, "Privacy_AppBundles"),
            (Grant::Microphone, "Privacy_Microphone"),
        ] {
            assert!(grant.settings_url().ends_with(anchor), "{:?}", grant);
            assert!(
                grant.check().len() <= 9,
                "{:?} fits the report column",
                grant
            );
        }
    }

    #[test]
    fn the_settings_line_names_a_renamed_pane_by_both_its_names() {
        let notes = fix_notes(
            Grant::Accessibility,
            None,
            DoctorMode::default(),
            true,
            vec![],
        );
        assert!(notes[0].contains("Accessibility"));
        assert!(notes[0].contains("Device Control & Data Access"));
        assert!(Grant::ScreenRecording
            .settings_name()
            .contains("Screen & System Audio Recording"));
        assert_eq!(
            Grant::FullDiskAccess.settings_name(),
            Grant::FullDiskAccess.title()
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_probe_refuses_off_macos() {
        assert_eq!(run_grant_probe(CHECK), 2);
    }
}
