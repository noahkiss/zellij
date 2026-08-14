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
    /// Only so the report can say "would" instead of "did". Nothing branches on it.
    pub dry_run: bool,
}

impl DoctorMode {
    /// Say what a fix did, or what it would have done, in the same words either way.
    ///
    /// The two phrasings live together here because they have to stay the same sentence: a dry run
    /// whose description of a fix has drifted from the fix is worse than no dry run, since the
    /// whole point of it is to be believed.
    pub fn describe(&self, done: &str) -> String {
        if self.dry_run {
            format!("would {}", done)
        } else {
            done.to_owned()
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
        let mut child = command
            .spawn()
            .map_err(|e| format!("could not run {}: {}", program, e))?;
        if let Some(stdin) = stdin {
            // Nothing writes here yet. It is the pipe a secret would go down instead of argv,
            // where `ps` shows it to every other process on the machine - and the one secret
            // doctor handles, the keychain password, cannot use it: `security
            // set-key-partition-list` reads its password from `-k` and from nowhere else. See
            // `session_signing::import_identity`, which says the same thing from the other end.
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
        }
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
}

/// Read the last run out of `systemctl show`.
///
/// `Result=` is the property that answers this and `ActiveState=` is not: a oneshot that ran,
/// failed and exited is `inactive` in exactly the same way as one that has never run at all. The
/// exit status comes along because "exit-code" without the code sends the reader back to the
/// journal for the one number they needed.
pub fn last_start_result(properties: &HashMap<String, String>) -> StartResult {
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
            dry_run: true,
            ..DoctorMode::default()
        };
        let wet = DoctorMode::default();
        assert_eq!(dry.describe("signed the pin"), "would signed the pin");
        assert_eq!(wet.describe("signed the pin"), "signed the pin");
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

    /// Recorded from `systemctl --user show zellij-go-for-flight.service` on a healthy machine.
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

    #[test]
    fn a_healthy_unit_reports_success() {
        let properties = parse_show_properties(HEALTHY_SHOW);
        assert_eq!(last_start_result(&properties), StartResult::Success);
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
