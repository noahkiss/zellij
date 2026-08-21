//! What `zellij session doctor` finds, and how it says it.
//!
//! Doctor is a checklist that repairs what it can and names what it cannot. The whole of that is
//! expressed here as one shape - a [`Finding`] - so that a check's author never decides how a
//! result is printed or how it counts, and adding a check cannot quietly add a new way to fail.
//!
//! Three outcomes and no more. `Changed` is something doctor did; `AlreadyCorrect` is something it
//! looked at and left alone; `NeedsYou` is work that a program is not allowed to do on its own -
//! clicking a permission toggle, installing Xcode, restarting a session full of the user's work.
//! The exit code follows from that split alone: zero when nothing is waiting on a person, which is
//! what makes doctor usable from a script without parsing its output.
//!
//! Everything a check learns from outside this process comes through [`Commander`], which exists
//! so the checks can be tested. `codesign`, `security` and `launchctl` cannot run on the machine
//! that runs the test suite, and a signing ladder nobody can test is a signing ladder that is
//! wrong on the machine it finally runs on.

use std::collections::HashMap;
use std::fmt;
use std::sync::Mutex;

/// What one check came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Doctor acted, and the thing is now right. Also what a dry run records for a fix it would
    /// have made: the report says "would", and the run still exits zero, because nothing is
    /// waiting on a person.
    Changed,
    /// Looked at, found right, left alone. Reported rather than dropped - a check whose passes are
    /// silent is a check nobody can tell from one that never ran.
    AlreadyCorrect,
    /// A person has to do it. The only outcome that makes doctor exit non-zero.
    NeedsYou,
}

/// One line of the report, and the unit every check returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The left column. Short, lowercase, and repeated across platforms so that `pin` means the
    /// same thing in a Linux report and a macOS one.
    pub check: String,
    pub status: Status,
    /// One line, saying what is true rather than what was asked.
    pub message: String,
    /// Continuation lines: a path to click on, a command to run, the reason a fix was refused.
    pub notes: Vec<String>,
}

impl Finding {
    pub fn changed(check: &str, message: impl Into<String>) -> Self {
        Finding::new(check, Status::Changed, message)
    }

    pub fn ok(check: &str, message: impl Into<String>) -> Self {
        Finding::new(check, Status::AlreadyCorrect, message)
    }

    pub fn needs_you(check: &str, message: impl Into<String>) -> Self {
        Finding::new(check, Status::NeedsYou, message)
    }

    fn new(check: &str, status: Status, message: impl Into<String>) -> Self {
        Finding {
            check: check.to_owned(),
            status,
            message: message.into(),
            notes: Vec::new(),
        }
    }

    /// Add a continuation line. Chained at the call site so a check reads as one expression.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        self.notes.push(note.into());
        self
    }

    /// Add a continuation line only when there is one, so a caller with an `Option` does not need
    /// a branch around it.
    pub fn maybe_note(self, note: Option<String>) -> Self {
        match note {
            Some(note) => self.note(note),
            None => self,
        }
    }
}

/// Every finding of one run, in the order the checks made them.
#[derive(Debug, Default, Clone)]
pub struct Report {
    findings: Vec<Finding>,
}

impl Report {
    pub fn new() -> Self {
        Report::default()
    }

    pub fn push(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    pub fn extend(&mut self, findings: impl IntoIterator<Item = Finding>) {
        self.findings.extend(findings);
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// Whether anything is waiting on a person.
    pub fn needs_you(&self) -> bool {
        self.findings
            .iter()
            .any(|finding| finding.status == Status::NeedsYou)
    }

    /// Zero when nothing is waiting on a person, whatever else happened.
    ///
    /// A run that fixed nine things and found the tenth beyond it is a run that still needs
    /// somebody, and a run that changed everything it touched succeeded. Neither the count of
    /// changes nor the count of checks belongs in this answer.
    pub fn exit_code(&self) -> i32 {
        if self.needs_you() {
            1
        } else {
            0
        }
    }

    /// The report, in three sections.
    ///
    /// Sections in the order a reader wants them: what happened to their machine, then what was
    /// already fine, then what is left for them. The last section is last because it is the one
    /// they act on, and the bottom of the output is where a terminal leaves the cursor.
    ///
    /// An empty section is omitted rather than printed with nothing under it - "Changed" over
    /// nothing reads as a failed change.
    pub fn render(&self) -> String {
        let sections = [
            ("Changed", Status::Changed),
            ("Already correct", Status::AlreadyCorrect),
            ("Needs you", Status::NeedsYou),
        ];
        let mut out = String::new();
        for (title, status) in sections {
            let mut findings = self
                .findings
                .iter()
                .filter(|finding| finding.status == status)
                .peekable();
            if findings.peek().is_none() {
                continue;
            }
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(title);
            out.push('\n');
            for finding in findings {
                out.push_str(&format!("  {:9} {}\n", finding.check, finding.message));
                for note in &finding.notes {
                    out.push_str(&format!("  {:9} {}\n", "", note));
                }
            }
        }
        out
    }
}

impl fmt::Display for Report {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.render())
    }
}

/// What a run is allowed to do.
///
/// Carried rather than consulted from globals so that a check states its intent in its signature,
/// and so that the whole of "may I act" is one value a test can construct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DoctorMode {
    /// Whether to act at all. `--dry-run` forces this off, which is the whole of what dry-run
    /// means: the checks are identical, only the acting is withheld.
    pub fix: bool,
    /// Whether the signing ladder may reach for a certificate. Signing is the one fix that writes
    /// to the user's keychain, which is why it has its own switch on top of `fix`.
    pub sign: bool,
    /// Only so the report can say `--dry-run` was asked for. The tense comes off `fix`, which
    /// `--dry-run` clears, so nothing has to branch on this.
    pub dry_run: bool,
}

impl DoctorMode {
    /// What the three flags come to.
    ///
    /// Here rather than at the call site because one invariant lives in it: `--dry-run` implies
    /// `--no-fix`, and it does so by clearing `fix` once, in one place. Every fix site asks `fix`
    /// and nothing asks `dry_run`, so no check can act in a dry run by forgetting to ask.
    pub fn from_flags(dry_run: bool, no_fix: bool, no_sign: bool) -> Self {
        DoctorMode {
            fix: !no_fix && !dry_run,
            sign: !no_sign,
            dry_run,
        }
    }

    /// Say what a fix did, or what it would have done, in the same words either way.
    ///
    /// The two phrasings live together here because they have to stay the same sentence: a dry run
    /// whose description of a fix has drifted from the fix is worse than no dry run, since the
    /// whole point of it is to be believed.
    ///
    /// "Would" is decided by `fix` and not by `dry_run`, because `--no-fix` withholds the acting
    /// just as completely: describing an unmade change in the past tense would have the report
    /// claim work that was never done.
    pub fn describe(&self, done: &str) -> String {
        if self.fix {
            done.to_owned()
        } else {
            format!("would {}", done)
        }
    }
}

impl Default for DoctorMode {
    fn default() -> Self {
        DoctorMode {
            fix: true,
            sign: true,
            dry_run: false,
        }
    }
}

/// What one external command came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl CommandOutput {
    /// Both streams together, for the several tools that split one answer across them.
    ///
    /// `codesign` is the reason this exists: `-d -r-` writes the requirement to stdout and the
    /// `Identifier=` block that says whether the requirement can be believed to stderr, and a
    /// parser given only one of them is parsing half an answer.
    pub fn combined(&self) -> String {
        let mut combined = self.stdout.clone();
        if !self.stderr.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&self.stderr);
        }
        combined
    }
}

/// Everything doctor learns by running something else.
///
/// A trait and not a function because the machine that runs the tests has no `codesign`, no
/// keychain and no `launchctl`, and the parts of doctor most worth testing are the ones that read
/// those tools' output. With this, a test hands the ladder a recorded transcript and checks which
/// rung it picked; without it, the ladder is proven only on the Mac it eventually breaks on.
pub trait Commander {
    /// Run `program` with `args`, optionally writing `stdin` to it, and report what came back.
    ///
    /// `Err` is reserved for the command not running at all - not installed, not executable. A
    /// command that ran and failed is `Ok` with `success: false`, because its stderr is the answer
    /// and a caller that gets an `Err` will not look at it.
    fn run(
        &self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<CommandOutput, String>;
}

/// The real one.
pub struct SystemCommander;

impl Commander for SystemCommander {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        stdin: Option<&str>,
    ) -> Result<CommandOutput, String> {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let mut command = Command::new(program);
        command
            .args(args)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            });
        // Run every child in a session of its own, so that none of them can ever prompt.
        //
        // **A null stdin is not enough, and assuming it was cost a release.** `security(1)` does
        // not read a password from stdin: it opens `/dev/tty` and writes the prompt straight to
        // the controlling terminal, which stdin cannot reach. So `security set-key-partition-list`
        // with no `-k` blocked forever on a real Mac at 0.45.0-nkmk.8 - inside a graphical
        // session, where the comment in `session_signing` had assumed a dialog would appear - and
        // doctor produced an empty report, no output, no timeout, and one line on the pane's
        // terminal:
        //
        // ```text
        // (deprecated) password to unlock /Users/…/login.keychain-db:
        // ```
        //
        // `setsid` puts the child in a new session with NO controlling terminal, so `/dev/tty`
        // cannot be opened, the prompt cannot be written, and the tool fails fast and says why
        // instead of waiting for a person who may not be watching. Doctor is run from launchd,
        // from a pane and over SSH; none of those can answer a prompt, and a doctor that hangs is
        // worse than every failure it exists to report.
        //
        // The error is ignored on purpose: `setsid` fails only when the child is already a process
        // group leader, which after `fork` it is not - and if it somehow were, the child already
        // has the property this asks for.
        #[cfg(unix)]
        unsafe {
            use std::os::unix::process::CommandExt;

            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not run {}: {}", program, e))?;
        if let Some(stdin) = stdin {
            // One caller writes here: `session_signing::team_id_from_keychain` pipes a
            // certificate into `openssl`. It is also the pipe a secret would go down instead of
            // argv, where `ps` shows it to every other process on the machine - and the one secret
            // doctor handles, the keychain password, cannot use it: `security
            // set-key-partition-list` reads its password from `-k` and from nowhere else. See
            // `session_signing::allow_codesign_to_reach_the_key`, which says the same thing from
            // the other end.
            let mut pipe = child
                .stdin
                .take()
                .ok_or_else(|| format!("could not write to {}", program))?;
            pipe.write_all(stdin.as_bytes())
                .map_err(|e| format!("could not write to {}: {}", program, e))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|e| format!("could not wait for {}: {}", program, e))?;
        Ok(CommandOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// A `Commander` that answers from a script and remembers what it was asked.
///
/// Both halves matter. The answers let a test drive the ladder down a rung it could not reach on
/// this machine; the record lets it assert the ORDER - that the pin was verified before it was
/// renamed over, that nothing was signed before the stale temp files were swept. Those are the
/// properties that a wrong signing flow breaks, and neither shows up in a return value.
pub struct RecordedCommander {
    answers: HashMap<String, CommandOutput>,
    fallback: CommandOutput,
    calls: Mutex<Vec<String>>,
    creates: Vec<(String, std::path::PathBuf)>,
}

impl RecordedCommander {
    /// Keyed by the whole command line, space-joined, which is how the assertions read too.
    pub fn new(answers: &[(&str, CommandOutput)]) -> Self {
        RecordedCommander {
            answers: answers
                .iter()
                .map(|(line, output)| ((*line).to_owned(), output.clone()))
                .collect(),
            // an unscripted command FAILS rather than succeeding emptily: a test that forgot to
            // record a step should see the step go wrong, not see it silently pass
            fallback: CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: String::from("not recorded"),
            },
            calls: Mutex::new(Vec::new()),
            creates: Vec::new(),
        }
    }

    /// Have a scripted command also CREATE a file, the way the real one would.
    ///
    /// `openssl` writes a key and a bundle, and the code that goes on to lock those files down
    /// cannot be reached by a script that only returns text - so a test could drive a mint up to
    /// its failure and never past it. Opt-in, so every commander that does not ask for this still
    /// touches nothing.
    pub fn creating(mut self, needle: &str, path: std::path::PathBuf) -> Self {
        self.creates.push((needle.to_owned(), path));
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    /// Whether some call had `needle` in it, for assertions that do not care about the exact argv.
    pub fn called_with(&self, needle: &str) -> bool {
        self.calls().iter().any(|call| call.contains(needle))
    }

    /// Where `needle` first appears in the call record, so a test can assert an ordering.
    pub fn position_of(&self, needle: &str) -> Option<usize> {
        self.calls().iter().position(|call| call.contains(needle))
    }
}

impl Commander for RecordedCommander {
    fn run(
        &self,
        program: &str,
        args: &[&str],
        _stdin: Option<&str>,
    ) -> Result<CommandOutput, String> {
        let line = std::iter::once(program)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        self.calls.lock().unwrap().push(line.clone());
        for (needle, path) in &self.creates {
            if line.contains(needle.as_str()) {
                let _ = std::fs::write(path, b"");
            }
        }
        Ok(self
            .answers
            .get(&line)
            // a prefix match too, so a test can script `codesign -d --verbose=2 -r-` without
            // spelling out the temp path that changes on every run
            .or_else(|| {
                self.answers
                    .iter()
                    .find(|(recorded, _)| line.starts_with(recorded.as_str()))
                    .map(|(_, output)| output)
            })
            .cloned()
            .unwrap_or_else(|| self.fallback.clone()))
    }
}

/// The `KEY=value` lines of `systemctl show`, as a map.
///
/// A value may itself hold an `=`, so the split is on the FIRST one only. systemd omits a property
/// it has no answer for rather than printing it empty, so a missing key and an empty value are
/// different things and the caller is left to tell them apart.
pub fn parse_show_properties(output: &str) -> HashMap<String, String> {
    output
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(name, value)| (name.trim().to_owned(), value.to_owned()))
        .collect()
}

/// How the unit's last run ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartResult {
    /// systemd is happy with it, whatever the session is doing.
    Success,
    /// systemd is not. `result` is its own word for why, which is worth quoting verbatim - it is
    /// the term the journal and the manual both use.
    Failed {
        result: String,
        exit_status: Option<String>,
    },
    /// Nothing to judge: the unit has never run, or systemd was not there to ask.
    Unknown,
    /// systemd has no definition to judge a run by. `load_state` is its own word - `not-found` for
    /// a unit that was never installed, `masked` for one that cannot start at all.
    NotLoaded { load_state: String },
}

/// Read the last run out of `systemctl show`.
///
/// `Result=` is the property that answers this and `ActiveState=` is not: a oneshot that ran,
/// failed and exited is `inactive` in exactly the same way as one that has never run at all. The
/// exit status comes along because "exit-code" without the code sends the reader back to the
/// journal for the one number they needed.
///
/// `LoadState` is consulted FIRST, and it has to be: systemd answers `Result=success` and
/// `ExecMainStatus=0` for a unit it has never heard of, exit 0. Verified -
/// `systemctl --user show zellij-session-nosuch-xyz.service` prints exactly that beside
/// `LoadState=not-found`. Without this, a machine with nothing installed had "the last run of the
/// unit succeeded" filed under **Already correct**. A caller that does not ask for `LoadState` gets
/// the older behaviour rather than a wrong one.
pub fn last_start_result(properties: &HashMap<String, String>) -> StartResult {
    if let Some(load_state) = properties.get("LoadState") {
        if load_state != "loaded" {
            return StartResult::NotLoaded {
                load_state: load_state.to_owned(),
            };
        }
    }
    let Some(result) = properties.get("Result") else {
        return StartResult::Unknown;
    };
    if result == "success" {
        return StartResult::Success;
    }
    StartResult::Failed {
        result: result.to_owned(),
        exit_status: properties
            .get("ExecMainStatus")
            .filter(|status| status.as_str() != "0")
            .cloned(),
    }
}

/// Shorthand for a recorded success.
pub fn recorded(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_owned(),
        stderr: String::new(),
    }
}

/// Shorthand for a recorded failure, whose stderr is the answer.
pub fn recorded_failure(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 0.45.0-nkmk.8 hang, at its root. `security(1)` writes its password prompt to the
    /// CONTROLLING TERMINAL, not to stdin, so a null stdin - which this has always had - does not
    /// stop it. A child in a session of its own has no controlling terminal to write to.
    ///
    /// Linux only, because it reads the session id out of `ps`, and the two `ps` implementations
    /// spell that column differently. The behaviour it checks is the same on macOS: `setsid(2)` is
    /// POSIX, and the `pre_exec` that calls it is gated on `unix`, not on Linux.
    #[cfg(target_os = "linux")]
    #[test]
    fn every_child_runs_in_a_session_of_its_own_so_none_can_stop_at_a_prompt() {
        let ours = unsafe { libc::getsid(0) };
        let output = SystemCommander
            .run("sh", &["-c", "ps -o sid= -p $$"], None)
            .expect("could not run sh");
        assert!(output.success, "{:?}", output);
        let theirs: i32 = output
            .stdout
            .trim()
            .parse()
            .unwrap_or_else(|_| panic!("{:?}", output.stdout));
        assert_ne!(
            theirs, ours,
            "the child shares our session, so it can still be handed a terminal to prompt on"
        );
    }

    #[test]
    fn a_report_with_nothing_waiting_on_a_person_exits_zero() {
        let mut report = Report::new();
        report.push(Finding::changed("pin", "refreshed the pinned copy"));
        report.push(Finding::ok("path", "zellij resolves here"));
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn one_needs_you_among_many_changes_still_exits_one() {
        let mut report = Report::new();
        for _ in 0..9 {
            report.push(Finding::changed("pin", "refreshed the pinned copy"));
        }
        report.push(Finding::needs_you("fda", "grant Full Disk Access"));
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn an_empty_report_exits_zero_and_prints_nothing() {
        let report = Report::new();
        assert_eq!(report.exit_code(), 0);
        assert_eq!(report.render(), "");
    }

    #[test]
    fn sections_are_ordered_and_empty_ones_are_left_out() {
        let mut report = Report::new();
        report.push(Finding::needs_you("fda", "grant Full Disk Access"));
        report.push(Finding::changed("pin", "refreshed the pinned copy"));
        let rendered = report.render();
        assert!(rendered.starts_with("Changed\n"), "{}", rendered);
        assert!(!rendered.contains("Already correct"), "{}", rendered);
        assert!(
            rendered.find("Changed") < rendered.find("Needs you"),
            "{}",
            rendered
        );
    }

    #[test]
    fn a_note_lines_up_under_its_finding() {
        let mut report = Report::new();
        report.push(Finding::needs_you("fda", "not granted").note("System Settings > Privacy"));
        assert_eq!(
            report.render(),
            "Needs you\n  fda       not granted\n            System Settings > Privacy\n"
        );
    }

    #[test]
    fn a_dry_run_says_would_in_the_same_words() {
        let dry = DoctorMode {
            fix: false,
            dry_run: true,
            ..DoctorMode::default()
        };
        let wet = DoctorMode::default();
        assert_eq!(dry.describe("sign the pin"), "would sign the pin");
        assert_eq!(wet.describe("sign the pin"), "sign the pin");
    }

    #[test]
    fn a_dry_run_never_fixes_however_the_other_flags_read() {
        let dry = DoctorMode::from_flags(true, false, false);
        assert!(!dry.fix, "--dry-run has to imply --no-fix");
        assert!(dry.dry_run);
        // signing is gated on `fix` above it, so `--dry-run` alone leaves this on and nothing acts
        assert!(dry.sign);
        assert!(!DoctorMode::from_flags(true, true, true).fix);
        assert!(!DoctorMode::from_flags(false, true, false).fix);
        assert!(DoctorMode::from_flags(false, false, false).fix);
        assert!(!DoctorMode::from_flags(false, false, true).sign);
    }

    /// `--no-fix` acts no more than `--dry-run` does, so it has to read the same way. A report that
    /// said "sign the pin" under it would be claiming a signature nobody made.
    #[test]
    fn no_fix_says_would_as_well_even_without_dry_run() {
        let no_fix = DoctorMode {
            fix: false,
            ..DoctorMode::default()
        };
        assert!(!no_fix.dry_run);
        assert_eq!(no_fix.describe("sign the pin"), "would sign the pin");
    }

    #[test]
    fn a_recorded_commander_answers_and_remembers() {
        let commander = RecordedCommander::new(&[("codesign -v /pin", recorded("ok"))]);
        let answer = commander.run("codesign", &["-v", "/pin"], None).unwrap();
        assert!(answer.success);
        assert_eq!(answer.stdout, "ok");
        assert_eq!(commander.calls(), vec!["codesign -v /pin".to_owned()]);
    }

    #[test]
    fn an_unscripted_command_fails_rather_than_passing_emptily() {
        let commander = RecordedCommander::new(&[]);
        let answer = commander.run("codesign", &["-v", "/pin"], None).unwrap();
        assert!(!answer.success);
    }

    /// Recorded from `systemctl --user show zellij-session-mysession.service` on a healthy machine.
    const HEALTHY_SHOW: &str = "\
Result=success
ActiveState=inactive
SubState=dead
ExecMainStatus=0
NRestarts=0
";

    /// The same, from a machine whose session had not been coming up.
    const FAILED_SHOW: &str = "\
Result=exit-code
ActiveState=failed
SubState=failed
ExecMainStatus=1
NRestarts=0
Environment=PATH=/usr/bin:/bin
";

    /// Recorded from `systemctl --user show <a name nothing installed>.service`, which exits 0.
    const MISSING_SHOW: &str = "\
Result=success
ExecMainStatus=0
LoadState=not-found
ActiveState=inactive
";

    #[test]
    fn a_healthy_unit_reports_success() {
        let properties = parse_show_properties(HEALTHY_SHOW);
        assert_eq!(last_start_result(&properties), StartResult::Success);
    }

    #[test]
    fn a_unit_systemd_never_heard_of_is_not_a_successful_run() {
        // systemd answers Result=success and exit 0 for a name nothing installed, so a report that
        // reads only Result files "the launcher ran fine" for a machine with no launcher
        let properties = parse_show_properties(MISSING_SHOW);
        assert_eq!(
            properties.get("Result").map(String::as_str),
            Some("success")
        );
        assert_eq!(
            last_start_result(&properties),
            StartResult::NotLoaded {
                load_state: String::from("not-found")
            }
        );
    }

    #[test]
    fn a_masked_unit_keeps_systemds_own_word_for_it() {
        let properties = parse_show_properties("Result=success\nLoadState=masked\n");
        assert_eq!(
            last_start_result(&properties),
            StartResult::NotLoaded {
                load_state: String::from("masked")
            }
        );
    }

    #[test]
    fn a_loaded_unit_is_still_judged_by_its_result() {
        let properties =
            parse_show_properties("Result=exit-code\nExecMainStatus=1\nLoadState=loaded\n");
        assert_eq!(
            last_start_result(&properties),
            StartResult::Failed {
                result: String::from("exit-code"),
                exit_status: Some(String::from("1")),
            }
        );
    }

    #[test]
    fn a_failed_unit_keeps_systemds_own_word_and_the_exit_code() {
        let properties = parse_show_properties(FAILED_SHOW);
        assert_eq!(
            last_start_result(&properties),
            StartResult::Failed {
                result: String::from("exit-code"),
                exit_status: Some(String::from("1")),
            }
        );
    }

    #[test]
    fn a_property_whose_value_holds_an_equals_sign_survives_the_split() {
        let properties = parse_show_properties(FAILED_SHOW);
        assert_eq!(
            properties.get("Environment").map(String::as_str),
            Some("PATH=/usr/bin:/bin")
        );
    }

    #[test]
    fn a_unit_that_has_never_run_is_unknown_rather_than_failed() {
        assert_eq!(
            last_start_result(&parse_show_properties("ActiveState=inactive\n")),
            StartResult::Unknown
        );
    }

    #[test]
    fn both_streams_come_back_together_for_a_tool_that_splits_its_answer() {
        let output = CommandOutput {
            success: true,
            stdout: String::from("designated => identifier \"x\""),
            stderr: String::from("Identifier=x"),
        };
        assert_eq!(
            output.combined(),
            "designated => identifier \"x\"\nIdentifier=x"
        );
    }
}
