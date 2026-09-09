//! The pane probe's clock and its parser, with no pane attached.
//!
//! Split out of [`session_doctor_macos`](crate::session_doctor_macos) so the deadlines can be
//! tested by handing them lines and an elapsed time instead of a running server, and compiled on
//! every platform for that reason alone - only the macOS doctor calls it.
//!
//! The probe writes two lines and the two are not worth the same. `manager=` is proof of life: it
//! says the server accepted the `run`, made the pane, and the pane's shell got as far as its first
//! `printf`. `fda=` is an answer to a question that can take its own time. Charging both to one
//! deadline made a slow answer look like a dead server.

// compiled everywhere so the tests below run everywhere; nothing outside macOS calls it
#![cfg_attr(not(target_os = "macos"), allow(dead_code))]

use std::time::Duration;

/// How long to wait for the pane to prove it is alive.
///
/// A pane that is going to answer at all writes its first line as fast as a shell starts. Waiting
/// longer would only lengthen the run on a machine where the session is wedged, which is a machine
/// with a worse problem that the checks above it have already reported.
pub(crate) const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// How long to wait for the Full Disk Access line, once the pane has proved it is alive.
///
/// Longer than the proof of life, for a reason particular to TCC: a machine that HOLDS the grant
/// has its `open(2)` allowed at once, and a machine that does not can sit for five to six seconds
/// before it is refused. Measured on a Mac without the grant: `manager=` immediately, `fda=no` at
/// about 5.5 to 5.8 seconds. A single five-second deadline was under that, so the one machine the
/// check exists for was the one machine it called wedged.
pub(crate) const FDA_TIMEOUT: Duration = Duration::from_secs(10);

/// How long to sleep between reads of the answer file.
pub(crate) const PROBE_POLL: Duration = Duration::from_millis(100);

/// What one pane came back with.
pub(crate) struct PaneAnswer {
    pub(crate) manager: Option<String>,
    pub(crate) full_disk_access: FullDiskAccess,
}

/// What the probe was able to say about Full Disk Access.
///
/// Four answers, because the last two used to be one. `Undetermined` is the pane telling us it
/// could not look - there is no `TCC.db` to open, so there is nothing to be refused by.
/// `Unanswered` is the pane never telling us anything, which on macOS is what a refusal in
/// progress looks like.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FullDiskAccess {
    /// `fda=yes` - the open was allowed.
    Granted,
    /// `fda=no` - the open was refused.
    Denied,
    /// `fda=unknown` - there was no `TCC.db` to open.
    Undetermined,
    /// No finished `fda=` line arrived before [`FDA_TIMEOUT`].
    Unanswered,
}

/// What the reader should do with the answer file as it stands.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ProbeStep {
    /// Both lines are in. Parse them and stop.
    Answered,
    /// The pane is alive and [`FDA_TIMEOUT`] passed without a Full Disk Access line. Everything
    /// the pane did say is still good; only that one answer is missing.
    FullDiskAccessUnanswered,
    /// Nothing arrived before [`PROBE_TIMEOUT`]. No pane ever wrote, so the server may be wedged.
    Wedged,
    /// Neither deadline has passed. Read again.
    KeepWaiting,
}

/// The reader's whole clock, as a function of what has been written and how long ago the client
/// was spawned.
///
/// The order of the questions is the point. A finished `fda=` line ends the wait whenever it
/// arrives, even past its own deadline - a late answer is still an answer, and the poll that finds
/// it is cheaper than the branch that would refuse it. A finished `manager=` line takes the wedged
/// verdict off the table for good: the server proved it is serving, so from then on the only thing
/// that can be missing is the second answer.
pub(crate) fn probe_step(written: &str, elapsed: Duration) -> ProbeStep {
    if finished_line(written, "fda=").is_some() {
        return ProbeStep::Answered;
    }
    if finished_line(written, "manager=").is_some() {
        if elapsed >= FDA_TIMEOUT {
            return ProbeStep::FullDiskAccessUnanswered;
        }
        return ProbeStep::KeepWaiting;
    }
    if elapsed >= PROBE_TIMEOUT {
        return ProbeStep::Wedged;
    }
    ProbeStep::KeepWaiting
}

/// The value of a `key=value` line that has been written in full, or `None` while it is still
/// being written.
///
/// The trailing newline is what says the line is finished. The pane writes its two answers with
/// two calls, so a read can land between them - and half of `fda=yes` is an answer that would
/// parse to "could not tell" on a machine that could have told.
fn finished_line<'a>(written: &'a str, key: &str) -> Option<&'a str> {
    written
        .split_inclusive('\n')
        .find_map(|line| line.strip_suffix('\n')?.trim().strip_prefix(key))
}

/// Read the answer file into an answer.
///
/// A missing `fda=` line is [`FullDiskAccess::Unanswered`] rather than a parse failure, which is
/// what lets the same parser serve both the answered path and the deadline path: the reader only
/// reaches the second with no such line in the file.
pub(crate) fn parse_pane_answer(written: &str) -> PaneAnswer {
    PaneAnswer {
        manager: finished_line(written, "manager=")
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        full_disk_access: match finished_line(written, "fda=") {
            Some("yes") => FullDiskAccess::Granted,
            Some("no") => FullDiskAccess::Denied,
            Some(_) => FullDiskAccess::Undetermined,
            None => FullDiskAccess::Unanswered,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST: Duration = Duration::from_millis(200);
    /// Past the proof-of-life deadline, inside the Full Disk Access one. Where the bug lived.
    const SLOW_DENIAL: Duration = Duration::from_secs(6);

    #[test]
    fn a_pane_that_answers_at_once_is_read_at_once() {
        let written = "manager=Aqua\nfda=yes\n";
        assert_eq!(probe_step(written, FAST), ProbeStep::Answered);
        let answer = parse_pane_answer(written);
        assert_eq!(answer.manager.as_deref(), Some("Aqua"));
        assert_eq!(answer.full_disk_access, FullDiskAccess::Granted);
    }

    #[test]
    fn a_denial_that_takes_six_seconds_is_a_denial_and_not_a_wedged_server() {
        // the pane has proved it is alive, so the five-second deadline no longer applies to it
        assert_eq!(
            probe_step("manager=Aqua\n", SLOW_DENIAL),
            ProbeStep::KeepWaiting
        );
        let written = "manager=Aqua\nfda=no\n";
        assert_eq!(probe_step(written, SLOW_DENIAL), ProbeStep::Answered);
        assert_eq!(
            parse_pane_answer(written).full_disk_access,
            FullDiskAccess::Denied
        );
    }

    #[test]
    fn a_pane_that_only_ever_says_which_domain_it_is_in_is_alive_with_no_fda_answer() {
        let written = "manager=Aqua\n";
        assert_eq!(
            probe_step(written, FDA_TIMEOUT),
            ProbeStep::FullDiskAccessUnanswered
        );
        let answer = parse_pane_answer(written);
        assert_eq!(answer.manager.as_deref(), Some("Aqua"));
        assert_eq!(answer.full_disk_access, FullDiskAccess::Unanswered);
    }

    #[test]
    fn a_pane_that_writes_nothing_at_all_is_still_a_wedged_server() {
        assert_eq!(
            probe_step("", Duration::from_secs(4)),
            ProbeStep::KeepWaiting
        );
        assert_eq!(probe_step("", PROBE_TIMEOUT), ProbeStep::Wedged);
    }

    #[test]
    fn a_late_answer_is_still_an_answer() {
        let written = "manager=Aqua\nfda=no\n";
        assert_eq!(
            probe_step(written, FDA_TIMEOUT + Duration::from_secs(5)),
            ProbeStep::Answered
        );
    }

    #[test]
    fn half_a_line_is_not_a_line() {
        assert_eq!(probe_step("manager=Aq", FAST), ProbeStep::KeepWaiting);
        // and past the proof-of-life deadline it is still nothing, because nothing finished
        assert_eq!(probe_step("manager=Aq", SLOW_DENIAL), ProbeStep::Wedged);
        assert_eq!(
            probe_step("manager=Aqua\nfda=ye", SLOW_DENIAL),
            ProbeStep::KeepWaiting
        );
        assert_eq!(
            parse_pane_answer("manager=Aqua\nfda=ye").full_disk_access,
            FullDiskAccess::Unanswered
        );
    }

    #[test]
    fn a_pane_whose_launchctl_said_nothing_has_still_proved_the_server_is_alive() {
        assert_eq!(
            probe_step("manager=\n", SLOW_DENIAL),
            ProbeStep::KeepWaiting
        );
        assert_eq!(
            probe_step("manager=\n", FDA_TIMEOUT),
            ProbeStep::FullDiskAccessUnanswered
        );
        assert_eq!(parse_pane_answer("manager=\n").manager, None);
    }

    #[test]
    fn a_machine_with_no_tcc_database_is_told_apart_from_one_that_never_answered() {
        assert_eq!(
            parse_pane_answer("manager=Aqua\nfda=unknown\n").full_disk_access,
            FullDiskAccess::Undetermined
        );
    }
}
