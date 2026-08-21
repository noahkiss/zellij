//! What only systemd can answer about a session that is meant to stay up.
//!
//! `session status` reports whether the unit is installed and loaded. That is the install; this is
//! the RUNNING of it, and the two come apart in the way that matters most: a timer that is loaded
//! but not armed never fires, and a service whose last start failed is `inactive` in exactly the
//! same way as one that has not been asked to run yet. Neither state is visible from `is-enabled`,
//! and both leave a machine with no session and nothing complaining about it.
//!
//! The journal comes last and only on a failure, because it is the one output here that is somebody
//! else's words rather than a fact this code established, and pasting twenty lines of it under a
//! healthy unit would bury the report it belongs to.

use zellij_utils::session_doctor::{
    last_start_result, parse_show_properties, Commander, Finding, Report, StartResult,
    SystemCommander,
};
use zellij_utils::session_service::{systemctl, systemd_service_name, systemd_timer_name};

/// How many journal lines to quote under a failed start.
///
/// Enough to hold a panic and the line before it, few enough that the report stays a report. The
/// user is told the command that prints the rest.
const JOURNAL_LINES: &str = "20";

pub(crate) fn checks(report: &mut Report, name: &str) {
    let service = systemd_service_name(name);
    let timer = systemd_timer_name(name);

    check_timer(report, &timer, name);
    check_last_start(report, &service);
    report.push(
        Finding::ok("signing", "n/a on Linux - nothing here signs a binary")
            .note("the pin is a plain copy, and no permission is keyed to its signature"),
    );
    report.push(Finding::ok(
        "tcc",
        "n/a on Linux - there is no such permission",
    ));
}

/// Whether the timer that repairs the session is armed.
///
/// Loaded and armed are different states. `systemctl --user is-enabled` answers for the install and
/// says nothing about whether the timer will actually fire, which is what a watchdog is for - and a
/// disarmed timer beside a healthy-looking install is the shape a session takes when it silently
/// stops coming back after a reboot.
fn check_timer(report: &mut Report, timer: &str, name: &str) {
    if systemctl::is_active(timer) {
        report.push(Finding::ok("timer", format!("{} is armed", timer)));
        return;
    }
    match systemctl::is_enabled(timer) {
        // installed, enabled, and still not armed - the state a `daemon-reload` away from working
        Some(state) if state == "enabled" => report.push(
            Finding::needs_you("timer", format!("{} is enabled but NOT armed", timer))
                .note("nothing will bring the session back until it fires")
                .note(format!("`zellij session enable {}` re-arms it", name)),
        ),
        Some(state) => report.push(
            Finding::needs_you("timer", format!("{} is {}", timer, state)).note(format!(
                "`zellij session enable {}` installs and arms it",
                name
            )),
        ),
        None => report.push(
            Finding::needs_you("timer", format!("{} - no answer from systemd", timer))
                .note("there may be no user manager to ask, which is ordinary in a container")
                .note("or over a bare SSH login - and there the session has no watchdog either"),
        ),
    }
}

/// How the last run of the service ended, and what the journal said if it ended badly.
fn check_last_start(report: &mut Report, service: &str) {
    let commander = SystemCommander;
    let Ok(shown) = commander.run(
        "systemctl",
        &[
            "--user",
            "show",
            service,
            // LoadState, because systemd answers Result=success for a unit it has never heard of
            "--property=Result,ExecMainStatus,LoadState",
        ],
        None,
    ) else {
        report.push(Finding::ok(
            "start",
            "no systemctl here to ask about the last run",
        ));
        return;
    };
    // A systemctl that RAN and failed is `Ok` with an empty stdout, and an empty property map reads
    // as "has not run yet" - which states as fact something this could not establish. The ordinary
    // way to get here is a context with no user bus: a bare SSH login, or a container.
    if !shown.success {
        report.push(
            Finding::needs_you("start", "could not ask systemd about the last run")
                .note(shown.stderr.trim().to_owned())
                .note("there may be no user manager to ask, which is ordinary in a container")
                .note("or over a bare SSH login - and there the session has no watchdog either"),
        );
        return;
    }
    let properties = parse_show_properties(&shown.stdout);
    match last_start_result(&properties) {
        StartResult::Success => {
            report.push(Finding::ok("start", "the last run of the unit succeeded"))
        },
        StartResult::Unknown => report.push(Finding::ok(
            "start",
            format!("{} has not run yet, so there is nothing to judge", service),
        )),
        // Not a fault of its own: what installs the unit is `session enable`, and the checks that
        // report on the install say so already. This one simply has no run to judge.
        StartResult::NotLoaded { load_state } => report.push(Finding::ok(
            "start",
            format!(
                "systemd has no loaded {} ({}), so there is no run to judge",
                service, load_state
            ),
        )),
        StartResult::Failed {
            result,
            exit_status,
        } => {
            let mut finding = Finding::needs_you(
                "start",
                match &exit_status {
                    Some(status) => format!("the last run FAILED ({}, exit {})", result, status),
                    None => format!("the last run FAILED ({})", result),
                },
            );
            for line in journal_tail(&commander, service) {
                finding = finding.note(line);
            }
            finding = finding.note(format!(
                "`journalctl --user -u {} -e` has the rest",
                service
            ));
            report.push(finding);
        },
    }
}

/// The last few journal lines for the unit, if journalctl will give them.
///
/// Empty rather than an error when it will not. A missing journal is an ordinary state - a
/// container, a machine with a volatile journal, a user session that has been logged out and back
/// in - and a report that turned that into a second failure would be reporting on itself.
fn journal_tail(commander: &SystemCommander, service: &str) -> Vec<String> {
    let Ok(output) = commander.run(
        "journalctl",
        &[
            "--user",
            "-u",
            service,
            "-n",
            JOURNAL_LINES,
            "--no-pager",
            "-o",
            "cat",
        ],
        None,
    ) else {
        return Vec::new();
    };
    if !output.success {
        return Vec::new();
    }
    output
        .stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| format!("| {}", line))
        .collect()
}
