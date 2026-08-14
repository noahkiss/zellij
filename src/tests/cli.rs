use std::net::{IpAddr, Ipv4Addr};

use clap::{CommandFactory, Parser};
use zellij_utils::cli::{on_big_stack, CliArgs, Command, SessionLifecycleCli, Sessions};

/// Parse a command line on a thread with a real stack.
///
/// Building the clap tree overflows the stack a test thread gets, so every test in this module goes
/// through [`on_big_stack`]. See its doc comment in `zellij-utils/src/cli.rs`.
fn parse_cli(args: &[&str]) -> Result<CliArgs, clap::Error> {
    let args: Vec<String> = args.iter().map(|a| a.to_string()).collect();
    on_big_stack(move || CliArgs::try_parse_from(args))
}

#[test]
fn verify_cli() {
    on_big_stack(|| CliArgs::command().debug_assert());
}

#[test]
fn web_cli_status_alone_works() {
    let args = parse_cli(&["zellij", "web", "--status"]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert!(web.timeout.is_none());
    } else {
        panic!("Expected Web command");
    }
}

#[test]
fn web_cli_status_with_timeout_works() {
    let args = parse_cli(&["zellij", "web", "--status", "--timeout", "5"]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert_eq!(web.timeout, Some(5));
    } else {
        panic!("Expected Web command");
    }
}

#[test]
fn web_cli_timeout_with_status_works() {
    // Test with --timeout before --status (order shouldn't matter)
    let args = parse_cli(&["zellij", "web", "--timeout", "10", "--status"]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert_eq!(web.timeout, Some(10));
    } else {
        panic!("Expected Web command");
    }
}

#[test]
fn web_cli_timeout_without_status_fails() {
    let args = parse_cli(&["zellij", "web", "--timeout", "5"]);
    assert!(args.is_err());
}

#[test]
fn web_cli_status_with_start_fails() {
    let args = parse_cli(&["zellij", "web", "--status", "--start"]);
    assert!(args.is_err());
}

#[test]
fn web_cli_status_with_stop_fails() {
    let args = parse_cli(&["zellij", "web", "--status", "--stop"]);
    assert!(args.is_err());
}

#[test]
fn web_cli_status_with_ip_works() {
    let args = parse_cli(&["zellij", "web", "--status", "--ip", "127.0.0.1"]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert_eq!(web.ip, Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
    } else {
        panic!("Expected Web command");
    }
}

#[test]
fn web_cli_status_with_port_works() {
    let args = parse_cli(&["zellij", "web", "--status", "--port", "9000"]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert_eq!(web.port, Some(9000));
    } else {
        panic!("Expected Web command");
    }
}

#[test]
fn web_cli_status_with_ip_and_port_works() {
    let args = parse_cli(&[
        "zellij", "web", "--status", "--ip", "0.0.0.0", "--port", "9000",
    ]);
    assert!(args.is_ok());
    if let Ok(CliArgs {
        command: Some(Command::Web(web)),
        ..
    }) = args
    {
        assert!(web.status);
        assert_eq!(web.ip, Some(IpAddr::V4(Ipv4Addr::new(0, 0, 0, 0))));
        assert_eq!(web.port, Some(9000));
    } else {
        panic!("Expected Web command");
    }
}

fn session_lifecycle_from(args: &[&str]) -> SessionLifecycleCli {
    match parse_cli(args) {
        Ok(CliArgs {
            command: Some(Command::Sessions(Sessions::Session(cli))),
            ..
        }) => cli,
        other => panic!("Expected a session lifecycle command, got {:?}", other),
    }
}

#[test]
fn session_up_comes_up_fresh_unless_a_restore_is_asked_for() {
    // the distinction the flag exists for: no --restore means the layout, which is what makes a
    // layout edit apply
    match session_lifecycle_from(&["zellij", "session", "up", "work"]) {
        SessionLifecycleCli::Up {
            session_name,
            restore,
        } => {
            assert_eq!(session_name.as_deref(), Some("work"));
            assert_eq!(restore, None);
        },
        other => panic!("Expected `up`, got {:?}", other),
    }
}

#[test]
fn session_up_restore_defaults_to_the_newest_snapshot() {
    match session_lifecycle_from(&["zellij", "session", "up", "work", "--restore"]) {
        SessionLifecycleCli::Up { restore, .. } => assert_eq!(restore.as_deref(), Some("latest")),
        other => panic!("Expected `up`, got {:?}", other),
    }
    match session_lifecycle_from(&["zellij", "session", "up", "work", "--restore", "abc123"]) {
        SessionLifecycleCli::Up { restore, .. } => assert_eq!(restore.as_deref(), Some("abc123")),
        other => panic!("Expected `up`, got {:?}", other),
    }
}

/// The three switches doctor is driven by, and the shape of each one's default.
///
/// `--fix` and `--sign` are the defaults, so the flags that matter are their negations - which is
/// why the command reads `no_fix` and `no_sign` rather than a pair of booleans that could disagree
/// with each other.
#[test]
fn session_doctor_fixes_and_signs_unless_told_otherwise() {
    match session_lifecycle_from(&["zellij", "session", "doctor", "work"]) {
        SessionLifecycleCli::Doctor {
            session_name,
            dry_run,
            no_fix,
            no_sign,
            exe,
            ..
        } => {
            assert_eq!(session_name.as_deref(), Some("work"));
            assert!(!dry_run);
            assert!(!no_fix);
            assert!(!no_sign);
            assert_eq!(exe, None);
        },
        other => panic!("Expected `doctor`, got {:?}", other),
    }
}

#[test]
fn session_doctor_takes_its_negations_and_the_short_dry_run() {
    match session_lifecycle_from(&["zellij", "session", "doctor", "-n", "--no-sign"]) {
        SessionLifecycleCli::Doctor {
            dry_run, no_sign, ..
        } => {
            assert!(dry_run);
            assert!(no_sign);
        },
        other => panic!("Expected `doctor`, got {:?}", other),
    }
}

#[test]
fn a_session_name_is_optional_everywhere() {
    // it falls back to the config's session_name at run time
    match session_lifecycle_from(&["zellij", "session", "down"]) {
        SessionLifecycleCli::Down { session_name, .. } => assert_eq!(session_name, None),
        other => panic!("Expected `down`, got {:?}", other),
    }
}

#[test]
fn session_restart_cannot_be_both_fresh_and_restored() {
    assert!(parse_cli(&[
        "zellij",
        "session",
        "restart",
        "work",
        "--fresh",
        "--restore",
        "abc123",
    ])
    .is_err());
}

/// The order the two halves of a switch are typed in decides it, which is what `overrides_with`
/// buys and what a plain pair of booleans would not: with both spelled out, the last one wins in
/// both directions, and neither flag can be silently ignored.
#[test]
fn the_last_of_fix_and_no_fix_wins_whichever_it_is() {
    match session_lifecycle_from(&["zellij", "session", "doctor", "--fix", "--no-fix"]) {
        SessionLifecycleCli::Doctor { fix, no_fix, .. } => {
            assert!(no_fix);
            assert!(!fix);
        },
        other => panic!("Expected `doctor`, got {:?}", other),
    }
    match session_lifecycle_from(&["zellij", "session", "doctor", "--no-fix", "--fix"]) {
        SessionLifecycleCli::Doctor { fix, no_fix, .. } => {
            assert!(fix);
            assert!(!no_fix);
        },
        other => panic!("Expected `doctor`, got {:?}", other),
    }
    match session_lifecycle_from(&["zellij", "session", "doctor", "--no-sign", "--sign"]) {
        SessionLifecycleCli::Doctor { sign, no_sign, .. } => {
            assert!(sign);
            assert!(!no_sign);
        },
        other => panic!("Expected `doctor`, got {:?}", other),
    }
}
