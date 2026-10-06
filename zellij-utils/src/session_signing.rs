//! Signing the pinned copy, so that a macOS permission grant survives a rebuild.
//!
//! macOS records a grant for a non-bundled program as an absolute path plus a `csreq` - a code
//! requirement the running process has to satisfy. An unsigned or ad-hoc-signed binary has no
//! identity to name, so the requirement macOS writes is a hash of the binary itself: change one
//! byte and the grant stops applying, silently, and every pane starts seeing "Operation not
//! permitted" in a directory that worked yesterday. Sign the binary with a certificate and the
//! requirement names the CERTIFICATE instead, which does not change when the binary does. That is
//! the whole of why any of this is here.
//!
//! **The release is signed once, in CI, and nothing here signs.** `release.yml` signs the macOS
//! binary with our Developer ID, and a pin refresh from that build copies it into place as it
//! arrived - see [`install_signed_build`]. Any other build - a local `cargo build`, a source-formula
//! build - is not ours to sign: it is pinned as a plain copy over a pin that holds no grants, and is
//! refused over one that does. Doctor reports either case and signs nothing. The per-Mac ladder
//! that used to re-sign every build (Apple Development, a minted certificate, the keychain password
//! that fed them) is gone; FORK.md has the history.
//!
//! Nothing in this file is gated on macOS. It reads and writes text and drives a [`Commander`],
//! which is what makes it testable at all: the machine that runs the suite has no `codesign`, no
//! `security` and no keychain, and a signing flow proven only on the Mac it finally breaks on is
//! not proven. The macOS-only part is which paths to hand it, and that lives with the other macOS
//! checks.

use std::path::{Path, PathBuf};

use crate::session_doctor::{Commander, DoctorMode, Finding};

/// The identifier every signature of the pinned copy carries.
///
/// CHANGING THIS VOIDS EVERY GRANT ON EVERY MACHINE. The identifier is part of the code
/// requirement macOS recorded when the user granted Full Disk Access, Accessibility or Screen
/// Recording, so a pin signed under a different identifier no longer satisfies the requirement and
/// no longer holds the grant - and nothing announces that. The user finds out when a pane cannot
/// read a directory. It is a constant and not a setting for that reason: a value nobody can set is
/// a value nobody can set wrongly.
pub const PIN_IDENTIFIER: &str = "org.zellij.nkmk";

/// The team whose Developer ID the release signs the macOS binary with, in `release.yml`.
///
/// A build carrying this team's Developer ID for [`PIN_IDENTIFIER`] is pinned exactly as it
/// arrived - see [`install_signed_build`]. Any other team's is not ours, whatever it is signed as,
/// and is treated like a local build.
pub const RELEASE_TEAM_ID: &str = "2Z88BYP37C";

/// What a release build has to satisfy before it is pinned as-is: Apple's Developer ID chain, the
/// Developer ID Application leaf, our identifier and our team.
///
/// The two `field` markers are the Developer ID intermediate and leaf, and they are what tell this
/// certificate from an Apple Development one of the same team - which carries neither, and which a
/// requirement naming only the team would accept. The text is what `codesign` derives for a
/// Developer ID signature, without the `/* exists */` comments it prints.
pub fn release_requirement() -> String {
    format!(
        "designated => identifier \"{}\" and anchor apple generic and certificate \
         1[field.1.2.840.113635.100.6.2.6] and certificate leaf[field.1.2.840.113635.100.6.1.13] \
         and certificate leaf[subject.OU] = \"{}\"",
        PIN_IDENTIFIER, RELEASE_TEAM_ID
    )
}

/// What the pinned copy's signature is, as `codesign` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinSignature {
    /// `codesign` would not answer: the file is not signed at all, or is not there.
    Unsigned,
    /// Signed, but the requirement names a hash of the CODE. The next build voids every grant.
    /// Ad-hoc signatures and unsigned-but-stamped binaries both land here.
    CodeHashed {
        identifier: String,
        designated: String,
    },
    /// Signed against something that outlives the build - a team id, a certificate. This is the
    /// state doctor exists to reach, and reaching it again would only change the requirement.
    Anchored {
        identifier: String,
        designated: String,
    },
}

impl PinSignature {
    pub fn identifier(&self) -> Option<&str> {
        match self {
            PinSignature::Unsigned => None,
            PinSignature::CodeHashed { identifier, .. }
            | PinSignature::Anchored { identifier, .. } => Some(identifier),
        }
    }

    pub fn designated(&self) -> Option<&str> {
        match self {
            PinSignature::Unsigned => None,
            PinSignature::CodeHashed { designated, .. }
            | PinSignature::Anchored { designated, .. } => Some(designated),
        }
    }
}

/// Read `codesign -d --verbose=2 -r- <path>` and say what the signature anchors on.
///
/// Both streams, because the answer is split across them: the requirement goes to stdout and the
/// `Identifier=` line that says the file HAS a signature goes to stderr.
///
/// The identifier line is required before anything else is believed, and that is the point of
/// reading `-r-` at all. Plain `codesign -d` prints nothing a grep can match, so a shell test
/// against it passes on an unsigned binary exactly as it does on a signed one - which is how the
/// script that came before this reported a signed pin on a machine that had never signed anything.
pub fn read_signature(combined_output: &str) -> PinSignature {
    let Some(identifier) = combined_output.lines().find_map(|line| {
        line.trim()
            .strip_prefix("Identifier=")
            .map(|value| value.trim().to_owned())
    }) else {
        return PinSignature::Unsigned;
    };
    let Some(designated) = combined_output
        .lines()
        .map(str::trim)
        .find(|line| line.starts_with("designated =>"))
        .map(|line| line.to_owned())
    else {
        // signed enough to carry an identifier, with no designated requirement to satisfy. Nothing
        // is anchored, so it is treated as the case that needs signing.
        return PinSignature::CodeHashed {
            identifier,
            designated: String::new(),
        };
    };
    if designated.contains("cdhash") {
        PinSignature::CodeHashed {
            identifier,
            designated,
        }
    } else {
        PinSignature::Anchored {
            identifier,
            designated,
        }
    }
}

/// The team id a designated requirement anchors on, which is what macOS recorded the grant against.
///
/// The `leaf[subject.OU]` in the text, read off the requirement rather than off any certificate:
/// this is the string TCC compares, so it is the one that says which team the grants belong to.
/// The requirement `codesign` derives for a Developer ID names the OU, and so did the one the
/// retired Apple Development rung wrote by hand.
///
/// `None` for a requirement anchored on anything else - a CN, a certificate hash, a code hash. Each
/// of those is a requirement that names no team, and guessing one for it would be a guess.
pub fn team_id_from_requirement(requirement: &str) -> Option<String> {
    let after = requirement.split("leaf[subject.OU]").nth(1)?;
    let (between, rest) = after.split_once('"')?;
    // `leaf[subject.OU] = "TEAM"`, and nothing but the operator in between. A quote reached across
    // some other expression would be some other certificate's field.
    if between.trim() != "=" {
        return None;
    }
    let (team, _) = rest.split_once('"')?;
    (!team.is_empty()).then(|| team.to_owned())
}

/// What a signing run does to the record of an owed re-grant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantRecord {
    /// No grant is at stake: the pin was never anchored, or the new signature satisfies the
    /// requirement the grants name and nothing was owed.
    Nothing,
    /// The new signature does not satisfy the requirement the grants name. Record it.
    Owe(String),
    /// An earlier run already recorded the debt, and this signature does not pay it either.
    StillOwed,
    /// The pin is back on what the grants name, so the recorded debt is gone.
    Paid,
}

/// Decide [`GrantRecord`] from the requirement the grants name, and what was just signed.
///
/// Pure, so the decision is tested here and not only on a Mac. `granted` is the recorded
/// requirement when there is one, else the requirement the pin carried before this run. `holds` is
/// whether the new signature satisfies `granted`, as `codesign --verify -R` answered it - the same
/// test macOS makes when it reads a grant.
pub fn judge_grants(
    granted: Option<&str>,
    recorded: bool,
    after: &str,
    holds: bool,
) -> GrantRecord {
    let Some(granted) = granted else {
        return GrantRecord::Nothing;
    };
    if granted == after || holds {
        return if recorded {
            GrantRecord::Paid
        } else {
            GrantRecord::Nothing
        };
    }
    if recorded {
        GrantRecord::StillOwed
    } else {
        GrantRecord::Owe(granted.to_owned())
    }
}

/// The temp file a half-finished signing run leaves behind.
///
/// Its own prefix, distinct from the pin's own temp file, so that sweeping one never removes the
/// other. Both live in the pin's directory because a rename has to stay inside one filesystem.
pub fn sign_temp_prefix() -> &'static str {
    ".zellij.sign."
}

/// Remove the temp files of runs that did not finish.
///
/// A failed run leaves a 46 MB copy of zellij in the pin directory, and the next failed run leaves
/// another. Nothing else ever removes them, so this is done first: sweeping AFTER a signing that
/// might itself fail would be a sweep that never runs on the machines that need it.
///
/// **Gated on the pid in the name, and on age.** This once removed every `.zellij.sign.*.tmp` in
/// the directory, which is the one thing a sweep must not do: the temp of a signing run happening
/// RIGHT NOW is named the same way, and taking it leaves that run renaming a name nothing holds -
/// and, on the copy path, `codesign` writing into a deleted inode. Both gates live with the pin's
/// sweep, in [`stale_temps`](crate::session_lifecycle::stale_temps), so the two prefixes cannot
/// drift apart on the question.
pub fn sweep_stale_temps(directory: &Path) -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        crate::session_lifecycle::sweep_stale_temps(
            directory,
            sign_temp_prefix(),
            crate::session_lifecycle::PIN_TEMP_MINIMUM_AGE,
        )
    }
    // signing is a macOS flow and the gates are `kill(pid, 0)`. Nowhere else has anything to sweep.
    #[cfg(not(unix))]
    {
        let _ = directory;
        Vec::new()
    }
}

/// Whether the pin refresh belongs to the signing transaction, and what to refresh from.
///
/// **One answer, asked by both sides.** The step that would otherwise copy the new build asks it to
/// decide whether to skip, and the signing step asks it to decide whether to copy - and if the two
/// ever disagreed in the "skip" direction the new build would never be pinned at all, with nothing
/// reporting it. So the decision lives here, where it compiles and is tested on every platform,
/// rather than in the macOS glue that supplies its inputs.
///
/// It defers only when there is something to lose. An **anchored** pin is a pin holding grants that
/// a plain copy of the new build would destroy - the fault this exists to prevent, see
/// [`SigningContext::refresh_from`]. A pin that is already ad-hoc holds no grant that survives a
/// rebuild, so refreshing it first costs nothing and pinning the new build is worth more than
/// protecting a signature that was never load-bearing.
///
/// Also `None` when this run may not act (`--dry-run`) or may not replace a signed pin
/// (`--no-sign`): in both cases the ordinary refresh has to happen on its own, because nothing is
/// coming after it.
pub fn refresh_belongs_to_signing(
    commander: &dyn Commander,
    pinned: &Path,
    mode: DoctorMode,
    current_exe: Option<PathBuf>,
    needs_refresh: bool,
) -> Option<PathBuf> {
    if !mode.fix || !mode.sign || !needs_refresh {
        return None;
    }
    let current_exe = current_exe?;
    pin_is_anchored(commander, pinned).then_some(current_exe)
}

/// Whether the pin carries an ANCHORED signature: one whose designated requirement names a
/// certificate rather than the pin's own code hash, and therefore one a macOS grant survives a
/// rebuild through.
///
/// **One predicate, asked by both sides of the pin.** The step that decides whose refresh it is
/// asks it, and so does [`install_pinned_exe`](crate::session_lifecycle::install_pinned_exe),
/// which must never copy over an answer of `true`. Two questions phrased two ways would eventually
/// give two answers, and the disagreement would be a destroyed signature.
///
/// `false` for an ad-hoc or unsigned pin - neither holds a grant a rebuild could take away - and
/// `false` wherever `codesign` cannot be run at all, which is every platform but macOS.
pub fn pin_is_anchored(commander: &dyn Commander, pinned: &Path) -> bool {
    let Ok(described) = commander.run(
        "codesign",
        &["-d", "--verbose=2", "-r-", &pinned.display().to_string()],
        None,
    ) else {
        return false;
    };
    matches!(
        read_signature(&described.combined()),
        PinSignature::Anchored { .. }
    )
}

/// What this run may do to a signed pin.
///
/// Facts the pin's writer cannot be handed by its callers. `install_pinned_exe` is reached from a
/// client launch, from `session up` and from doctor, and only the last of those has ever seen a
/// `--no-sign` flag; adding parameters to the sink would put the decision back in the hands of the
/// callers, which is the fault this whole path exists to close. So the run states its policy once,
/// and the sink reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinSigningPolicy {
    /// `false` only for `zellij session doctor --no-sign`. The sink then leaves an anchored pin
    /// exactly as it is, even for a release build - refusing to act is what `--no-sign` asks for,
    /// and copying the new build over the signature is not the other option, it is the fault.
    pub allowed: bool,
    /// `true` only for `zellij session doctor --regranted`: the operator says the grants were made
    /// again against the signature the pin carries now, so the re-grant doctor recorded as owed is
    /// paid. See [`SigningDir::regrant_owed`].
    pub regranted: bool,
}

impl Default for PinSigningPolicy {
    fn default() -> Self {
        PinSigningPolicy {
            allowed: true,
            regranted: false,
        }
    }
}

static PIN_SIGNING_POLICY: std::sync::Mutex<Option<PinSigningPolicy>> = std::sync::Mutex::new(None);

/// State what this run may do to a signature, before anything can write the pin.
pub fn set_pin_signing_policy(policy: PinSigningPolicy) {
    if let Ok(mut held) = PIN_SIGNING_POLICY.lock() {
        *held = Some(policy);
    }
}

/// The policy this run set, or the ordinary one: a release build may replace a signed pin. A
/// process that never states a policy is a plain `zellij` launch, and that is exactly the caller
/// that must keep the pin on the newest release.
pub fn pin_signing_policy() -> PinSigningPolicy {
    PIN_SIGNING_POLICY
        .lock()
        .ok()
        .and_then(|held| held.clone())
        .unwrap_or_default()
}

/// The one path [`sign_pin`] cannot work out for itself: where the record of an owed re-grant is
/// kept.
///
/// Built here rather than at each call site so that every door into the transaction reads and
/// writes the same record.
pub fn signing_context(refresh_from: Option<PathBuf>) -> Option<SigningContext> {
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    Some(SigningContext {
        signing_dir: SigningDir::new(home.join("Library/Application Support/zellij/signing")),
        refresh_from,
        regranted: pin_signing_policy().regranted,
    })
}

/// What a run with no `HOME` is told, wherever it reaches the signed pin.
pub const NO_HOME: &str = "no HOME, so there is nowhere to keep the record of an owed re-grant";

/// Put `source` at the pin's path over an anchored pin, as one transaction - or leave the pin.
///
/// The only way an anchored pin is ever replaced. Only a release build, carrying our Developer ID,
/// gets in: it is copied into a temp beside the pin, the copy is verified, and only then is it
/// `rename(2)`d over the pin - see [`install_signed_build`]. Any other build is refused, and so is
/// a copy that does not verify, which leaves the previous signed pin exactly where it was, on the
/// previous build, with every grant it holds intact. That is worth more than the new build: an
/// older server that can still read the user's files beats a newer one whose Full Disk Access can
/// only be given back through a GUI dialog at the machine.
///
/// `Err` is the refusal reason, quoted from the finding [`sign_pin`] gave.
///
/// Gated with the pin itself: `install_pinned_exe` and `pin_needs_refresh` are `cfg(unix)`, and a
/// platform with no pinned copy has no signature on it to protect.
#[cfg(unix)]
pub fn refresh_pin_through_signing(source: &Path, pinned: &Path) -> Result<(), String> {
    let policy = pin_signing_policy();
    if !policy.allowed {
        return Err("this run was told to leave the signed pin alone (`--no-sign`)".to_owned());
    }
    let mode = DoctorMode {
        fix: true,
        sign: true,
        dry_run: false,
    };
    let commander = crate::session_doctor::SystemCommander;
    // no HOME and a signed pin to protect: refusing is the answer, because falling through to the
    // plain copy is exactly the fault this exists to stop
    let Some(context) = signing_context(Some(source.to_path_buf())) else {
        return Err(NO_HOME.to_owned());
    };
    let run = sign_pin(&commander, pinned, mode, &context);
    // asked of the disk rather than read out of the findings: what matters is whether the new
    // build is at the path, and that is a fact the transaction leaves behind either way
    if crate::session_lifecycle::pin_needs_refresh(source, pinned) {
        Err(refusal_from(&run.findings))
    } else {
        Ok(())
    }
}

/// The reason the signing transaction gave, in one line.
///
/// The first finding that is not "already correct", message and notes joined - quoting it beats inventing a summary that will not match what
/// `zellij session doctor` says a moment later.
pub fn refusal_from(findings: &[Finding]) -> String {
    findings
        .iter()
        .find(|finding| finding.status == crate::session_doctor::Status::NeedsYou)
        .or_else(|| findings.last())
        .map(|finding| {
            let mut said = finding.message.clone();
            for note in &finding.notes {
                said.push_str("; ");
                said.push_str(note.trim());
            }
            said
        })
        .unwrap_or_else(|| "the signing step said nothing".to_owned())
}

/// What one pass over the pinned copy's signature came to.
pub struct SigningRun {
    pub findings: Vec<Finding>,
}

/// Judge the pinned copy's signature, and refresh it from a release build when one is pending.
///
/// Nothing here signs. The release is signed once, in CI, so there are three cases and none of
/// them needs a certificate on this machine.
///
/// 1. **A refresh from a release build** is copied into place as it arrived - see
///    [`install_signed_build`], which verifies the copy, renames it over the pin and settles the
///    grants. A refresh from any other build is refused, and the signed pin stays where it was.
/// 2. **An anchored pin** is read FIRST, and then VERIFIED. "Anchored" is a property of the
///    requirement's text and "holds a grant" is a property of the binary satisfying it, and a pin
///    can have the first without the second - so passing verification is what leaves it alone.
///    A pin anchored on a retired local certificate is left alone too: a release refreshes it,
///    and the re-grant that switch owes is recorded then.
/// 3. **An ad-hoc or unsigned pin** is a build the release did not sign. It holds no grant a
///    rebuild keeps, and the remedy is the release, not a signature made here.
///
/// A failure anywhere is a `Needs you` naming the recovery, never a fatal error: doctor has other
/// checks to make and a machine with an unsigned pin is still a machine worth reporting on.
pub fn sign_pin(
    commander: &dyn Commander,
    pin: &Path,
    mode: DoctorMode,
    context: &SigningContext,
) -> SigningRun {
    let mut findings = Vec::new();
    let pin_display = pin.display().to_string();

    let signature = match commander.run(
        "codesign",
        &["-d", "--verbose=2", "-r-", &pin_display],
        None,
    ) {
        Ok(output) => read_signature(&output.combined()),
        Err(reason) => {
            findings.push(
                Finding::needs_you("signing", format!("could not run codesign: {}", reason))
                    .note("Xcode or the Command Line Tools provide it:")
                    .note("  xcode-select --install"),
            );
            return SigningRun { findings };
        },
    };

    if context.regranted {
        findings.push(confirm_regrant(context, mode));
    }

    // A release build arrives already signed with our Developer ID, so a refresh from one puts it
    // in place as it came. Any other build is not ours to sign, and over a signed pin it is not
    // ours to copy either: the copy would carry no grant the pin holds.
    if let (true, true, Some(source)) = (mode.fix, mode.sign, context.refresh_from.as_deref()) {
        if let Some(installed) = install_signed_build(commander, pin, source, context, &signature) {
            findings.extend(installed);
            return SigningRun { findings };
        }
        findings.push(what_became_of_the_pin(
            Finding::needs_you(
                "signing",
                format!(
                    "{} is not a release build, so it was not pinned over the signed copy at {}",
                    source.display(),
                    pin_display
                ),
            )
            .note("only a build carrying the release's Developer ID replaces a signed pin;")
            .note("any other build would replace it with one that holds no grants"),
            context,
        ));
        return SigningRun { findings };
    }

    match &signature {
        PinSignature::Anchored {
            identifier,
            designated,
        } => match verify_signature(commander, &pin_display) {
            Ok(_) => {
                // A pin always satisfies its own requirement, so that alone says nothing about a
                // grant made against an earlier one. The record of a switch does.
                if let Some(owed) =
                    owed_on_a_signed_pin(commander, &pin_display, designated, context, mode)
                {
                    findings.push(owed);
                    return SigningRun { findings };
                }
                findings.push(
                    Finding::ok(
                        "signing",
                        format!("{} is signed as {}", pin_display, identifier),
                    )
                    .note(designated.clone())
                    .note("the requirement names no code hash, so a rebuild keeps every grant")
                    .note("and the pin satisfies it, so the grants recorded against it still hold"),
                );
            },
            Err(reason) => {
                findings.push(
                    Finding::needs_you(
                        "signing",
                        format!(
                            "{} is signed as {}, and does not satisfy its own requirement",
                            pin_display, identifier
                        ),
                    )
                    .note(designated.clone())
                    .note(reason)
                    .note("a signature that does not verify holds no grant, whatever it reads as")
                    .note("nothing is signed on this machine; pin the release build again:")
                    .note(format!(
                        "  remove {} and run `zellij session doctor --fix` from the brew release",
                        pin_display
                    )),
                );
            },
        },
        PinSignature::CodeHashed { .. } | PinSignature::Unsigned => {
            findings.push(not_a_release_build(
                &pin_display,
                signature == PinSignature::Unsigned,
            ));
        },
    }
    SigningRun { findings }
}

/// What a pin the release did not sign is told: a local `cargo build`, a source-formula build.
///
/// Its signature is ad-hoc or absent, so the requirement macOS records names the binary's own
/// hash and the next build voids every grant made against it. Nothing on this machine can do
/// better - the release is signed once, in CI - so the finding names the release as the remedy.
fn not_a_release_build(pin: &str, unsigned: bool) -> Finding {
    Finding::needs_you(
        "signing",
        format!("{} is not a release build, so the pin holds no grants", pin),
    )
    .note(if unsigned {
        "it is not signed at all"
    } else {
        "its signature is ad-hoc: the requirement names its own code hash"
    })
    .note("nothing is signed on this machine; the release is signed once, in CI")
    .note("install the brew release to get a signed pin, then `zellij session doctor --fix`")
}

/// The signature `source` carries, when it is the release's own Developer ID for our identifier.
///
/// The same verification a freshly signed pin gets - [`verify_signature`], which reads the
/// requirement and then checks the binary satisfies it - and then [`release_requirement`], asked of
/// `codesign` rather than of the text. The team is read off the requirement first so that a local
/// build, or anybody else's Developer ID, is turned away before the one question that costs a
/// `codesign` run of its own.
///
/// `None` is "not ours", and it is never an error. A local `cargo build`, a stock upstream binary
/// and a source-formula build all land here, and none of them replaces a signed pin.
fn build_carries_our_developer_id(
    commander: &dyn Commander,
    source: &Path,
) -> Option<(String, String)> {
    let source_display = source.display().to_string();
    let PinSignature::Anchored {
        identifier,
        designated,
    } = verify_signature(commander, &source_display).ok()?
    else {
        return None;
    };
    if identifier != PIN_IDENTIFIER
        || team_id_from_requirement(&designated).as_deref() != Some(RELEASE_TEAM_ID)
    {
        return None;
    }
    satisfies(commander, &source_display, &release_requirement())
        .then_some((identifier, designated))
}

/// Put a release build at the pin's path with the signature it arrived with, and sign nothing.
///
/// **The release signs, so this machine does not have to.** `release.yml` signs the macOS binary
/// with our Developer ID - hardened runtime, timestamped, notarized - and Homebrew installs it
/// byte for byte. Until nkmk.30 a per-Mac ladder threw that signature away and put a local one in
/// its place: an Apple Development certificate on one Mac, a minted one on another, each its own
/// requirement and each needing the login keychain and, over SSH, its password.
///
/// So this is the whole of the refresh: a copy into a temp beside the pin, the same verification
/// any pin gets, and a `rename(2)`. No `codesign -s`, no keychain, no password. `None` means "not a
/// release build", and the caller refuses the refresh.
///
/// **The first such pin owes a re-grant, once.** A pin that was anchored on another requirement -
/// a retired Apple Development rung under another team, or a minted certificate's hash - held
/// grants this signature does not satisfy, so the switch is recorded through
/// [`settle_the_grants`], and doctor asks on every run until `--fix --regranted`. A pin already on
/// this requirement owes nothing, and neither does one that was never anchored.
///
/// A copy or rename that fails is the filesystem's fault, not the build's. It is reported with the
/// pin left as it was.
fn install_signed_build(
    commander: &dyn Commander,
    pin: &Path,
    source: &Path,
    context: &SigningContext,
    before: &PinSignature,
) -> Option<Vec<Finding>> {
    let (identifier, designated) = build_carries_our_developer_id(commander, source)?;
    let mut findings = Vec::new();
    let pin_display = pin.display().to_string();
    let directory = pin.parent().unwrap_or_else(|| Path::new("."));
    let refused = |findings: &mut Vec<Finding>, reason: String| {
        findings.push(what_became_of_the_pin(
            Finding::needs_you("signing", reason),
            context,
        ));
    };

    let swept = sweep_stale_temps(directory);
    if !swept.is_empty() {
        findings.push(Finding::changed(
            "signing",
            format!(
                "removed {} leftover temp {} from earlier signing runs",
                swept.len(),
                if swept.len() == 1 { "file" } else { "files" }
            ),
        ));
    }
    // the signing run's own temp name, so its sweep covers this one too - see `perform_signing`
    let pin_before = pin_identity(pin);
    let temporary = directory.join(format!("{}{}.tmp", sign_temp_prefix(), std::process::id()));
    let temporary_display = temporary.display().to_string();
    if let Err(error) = std::fs::copy(source, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        refused(
            &mut findings,
            format!("could not copy {} to pin it: {}", source.display(), error),
        );
        return Some(findings);
    }
    // the copy is what gets renamed, so the copy is what is verified - a short write would carry a
    // signature that no longer covers its own bytes
    if let Err(reason) = verify_signature(commander, &temporary_display) {
        let _ = std::fs::remove_file(&temporary);
        refused(
            &mut findings,
            format!(
                "{} carries our Developer ID, and the copy of it did not verify: {}",
                source.display(),
                reason
            ),
        );
        return Some(findings);
    }
    if let Err(reason) = crate::session_lifecycle::flush_pin_temp(&temporary) {
        let _ = std::fs::remove_file(&temporary);
        refused(&mut findings, reason);
        return Some(findings);
    }
    if !pin_unchanged_since(pin, &pin_before) {
        let _ = std::fs::remove_file(&temporary);
        refused(
            &mut findings,
            format!(
                "{} was replaced while the new build was being copied, so the copy was discarded \
                 rather than written over the newer one - run `zellij session doctor --fix` again",
                pin_display
            ),
        );
        return Some(findings);
    }
    if let Err(error) = std::fs::rename(&temporary, pin) {
        let _ = std::fs::remove_file(&temporary);
        refused(
            &mut findings,
            format!("could not put the new build at {}: {}", pin_display, error),
        );
        return Some(findings);
    }
    crate::session_lifecycle::flush_pin_directory(directory);
    crate::session_lifecycle::record_pin_refreshed_from(source, pin);

    findings.push(
        Finding::changed(
            "signing",
            format!(
                "refreshed {} with the build's own Developer ID signature; nothing was signed here",
                pin_display
            ),
        )
        .note(format!(
            "identifier {}, team {}",
            identifier, RELEASE_TEAM_ID
        ))
        .note(designated.clone())
        .note("the release signed and notarized it, and it was copied into place unchanged,")
        .note("so no certificate, keychain or password was needed on this machine"),
    );
    findings.extend(settle_the_grants(
        commander,
        &pin_display,
        context,
        before,
        &designated,
    ));
    Some(findings)
}

/// What a run that did not refresh left at the pin's path, said the same way wherever it is said.
///
/// Two outcomes and they are not interchangeable, which is why this is one function rather than a
/// sentence written at each site. With the refresh deferred into this transaction the pin still
/// holds the PREVIOUS build, signature and grants intact, and nothing about it changed; without a
/// deferred refresh it holds the build it already held. Saying "the pinned copy is untouched" in
/// the first case was true only of the temp file, and it was the sentence that hid a pin replaced
/// by an ad-hoc copy for two releases.
fn what_became_of_the_pin(finding: Finding, context: &SigningContext) -> Finding {
    if context.refresh_from.is_some() {
        finding
            .note("the pin was NOT refreshed, so the previously signed copy is still in place,")
            .note("on the previous build")
            .note("every grant it holds is intact, and a restart now starts that build")
    } else {
        finding.note("the pinned copy is untouched")
    }
}

/// Whether the requirement macOS recorded a grant against has just changed - and if so, why.
///
/// **Asked of the two requirements, and of nothing else.** It used to be inferred from the state of
/// the machine: a bundle sitting in the signing directory meant "this machine has signed with a
/// certificate of its own", so the note fired. On a Mac that had never used that rung - but had
/// leftovers from an older shell script in its keychain and its signing directory - doctor sent the
/// user to System Settings to re-grant three permissions against a requirement that was
/// character-for-character the one already there. A grant is keyed to the requirement text, so the
/// requirement text is the only thing that can answer this.
///
/// `None` means every grant carries over untouched, and that is worth being right about in both
/// directions: a spurious re-grant costs a person a trip through System Settings, and a missing one
/// costs them a session that silently cannot read their files.
fn requirement_changed(before: &PinSignature, after: &str) -> Option<String> {
    match before {
        PinSignature::Anchored { designated, .. } if designated == after => None,
        PinSignature::Anchored { .. } => Some(String::from(
            "this pin was anchored on a different certificate before now, so the requirement \
             macOS recorded every grant against is not the one it will evaluate from now on",
        )),
        // an ad-hoc or unsigned pin's requirement named the binary's own hash, so there was never
        // a grant that could survive a rebuild. This is the FIRST requirement worth recording.
        PinSignature::CodeHashed { .. } | PinSignature::Unsigned => Some(String::from(
            "the pin's requirement named its own code hash until now, which no rebuild could \
             satisfy - so the grants it holds were made against something already gone",
        )),
    }
}

/// The requirement an earlier run recorded the grants against, unless this run was told the
/// re-grant is made (`--regranted`).
fn recorded_grant(context: &SigningContext) -> Option<String> {
    if context.regranted {
        return None;
    }
    std::fs::read_to_string(context.signing_dir.regrant_owed())
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}

/// The requirement the grants on this machine were made against, as far as doctor can know it:
/// the recorded one, else the one the pin carried before this run.
fn granted_requirement(context: &SigningContext, before: &PinSignature) -> Option<String> {
    recorded_grant(context).or_else(|| match before {
        PinSignature::Anchored { designated, .. } => Some(designated.clone()),
        PinSignature::CodeHashed { .. } | PinSignature::Unsigned => None,
    })
}

/// Whether `target` satisfies a designated requirement it may not carry itself.
///
/// `codesign --verify -R` is the test macOS makes when it reads a grant: does this code satisfy
/// the requirement recorded with it. Anything but a clean pass counts as `false`, so a check that
/// could not run reports the re-grant as owed rather than as paid.
fn satisfies(commander: &dyn Commander, target: &str, designated: &str) -> bool {
    let designated = designated.trim();
    let expression = designated
        .strip_prefix("designated =>")
        .map(str::trim)
        .unwrap_or(designated);
    // `-R` takes a FILE unless its value opens with `=`, which makes the rest of it the requirement
    // text itself. One argv of `-R=<text>` hands `codesign` the value `=<text>`, its inline form.
    let requirement = format!("-R={}", expression);
    matches!(
        commander.run("codesign", &["--verify", &requirement, target], None),
        Ok(output) if output.success
    )
}

/// `--regranted`: the operator says the grants were made again, so forget the recorded debt.
///
/// Taken on the operator's word. Doctor cannot read the grants themselves - they live in a TCC
/// database only a process holding Full Disk Access can open - so the flag is the proof.
fn confirm_regrant(context: &SigningContext, mode: DoctorMode) -> Finding {
    let path = context.signing_dir.regrant_owed();
    let Some(granted) = std::fs::read_to_string(&path)
        .ok()
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
    else {
        return Finding::ok(
            "signing",
            "--regranted: no re-grant was owed, so there was nothing to record",
        );
    };
    if !mode.fix {
        return Finding::changed(
            "signing",
            mode.describe("record the owed re-grant as made (--regranted)"),
        )
        .note(format!("it is owed against: {}", granted));
    }
    match std::fs::remove_file(&path) {
        Ok(()) => Finding::changed(
            "signing",
            "recorded the owed re-grant as made, on your word (--regranted)",
        )
        .note(format!("it was owed against: {}", granted)),
        Err(error) => Finding::needs_you(
            "signing",
            format!("could not remove {}: {}", path.display(), error),
        ),
    }
}

/// The re-grant an earlier run recorded, when the pin it judges still does not pay it.
///
/// `None` when nothing is recorded, or when the pin satisfies the recorded requirement again - in
/// which case a run that may act forgets the record.
fn owed_on_a_signed_pin(
    commander: &dyn Commander,
    pin: &str,
    designated: &str,
    context: &SigningContext,
    mode: DoctorMode,
) -> Option<Finding> {
    let granted = recorded_grant(context)?;
    if granted == designated || satisfies(commander, pin, &granted) {
        if mode.fix {
            let _ = std::fs::remove_file(context.signing_dir.regrant_owed());
        }
        return None;
    }
    Some(
        Finding::needs_you(
            "signing",
            format!(
                "{} is signed as {}, and the grants were made against another requirement",
                pin, PIN_IDENTIFIER
            ),
        )
        .note(format!("now:     {}", designated))
        .note(format!("granted: {}", granted))
        .note(
            if team_id_from_requirement(designated).as_deref() == Some(RELEASE_TEAM_ID) {
                "an earlier run switched this pin to the release's Developer ID requirement, and"
            } else {
                "an earlier run moved this pin to another requirement, and"
            },
        )
        .note("the grants were made against the old one. macOS evaluates every grant against the")
        .note("requirement it recorded, which this pin does not satisfy")
        .note(format!(
            "1. re-grant Full Disk Access, Accessibility and Screen Recording for {}",
            pin
        ))
        .note("2. `zellij session doctor --fix --regranted`, so doctor records it as made")
        .note("3. THEN `zellij session restart`"),
    )
}

/// Everything [`sign_pin`] needs that only the platform can name.
///
/// Carried in rather than derived here so that this whole file stays testable on a machine with no
/// keychain: a test builds one of these over a temp directory and drives the same code the Mac
/// runs.
#[derive(Debug, Clone)]
pub struct SigningContext {
    pub signing_dir: SigningDir,
    /// The build to put at the pin's path, when the refresh was handed to this transaction rather
    /// than done before it.
    ///
    /// **The refresh and the verification have to be one step or neither is safe.** Doctor once
    /// copied the new build over the pin and signed it afterwards, so a run that could not sign
    /// replaced a properly anchored pin with a fresh ad-hoc one and then reported `the pinned copy
    /// is untouched`. Copying to a temp, verifying it and renaming only on success means a refusal
    /// leaves the previous signed pin exactly where it was, holding its grants, on the previous
    /// build.
    pub refresh_from: Option<PathBuf>,
    /// The operator confirmed the owed re-grant (`--regranted`). See [`PinSigningPolicy::regranted`].
    pub regranted: bool,
}

/// Enough of a file to notice it being replaced: its length and when it was last written.
///
/// `None` for a pin that is not there, which is a real state.
fn pin_identity(pin: &Path) -> Option<(u64, std::time::SystemTime)> {
    let metadata = std::fs::metadata(pin).ok()?;
    Some((metadata.len(), metadata.modified().ok()?))
}

/// Whether the pin is still the file this run decided about.
///
/// Deliberately a comparison and not a lock. Two renames into the same directory cannot be ordered
/// from here without a lock both `session up` and doctor would have to take, and the failure this
/// guards is rare enough (the two commands typed seconds apart) that a lock on the pin path would
/// be new machinery carrying new ways to wedge. Comparing is enough to stop the silent half of the
/// fault: a run that would clobber a newer pin stops and says so instead.
///
/// A pin that cannot be `stat`ed now, having been readable before, counts as changed. So does one
/// that appeared where there was none.
fn pin_unchanged_since(pin: &Path, before: &Option<(u64, std::time::SystemTime)>) -> bool {
    pin_identity(pin) == *before
}

/// What a new signature at the pin's path does to the grants, recorded and said.
///
/// The tail of a build installed with its own signature - see [`install_signed_build`]. It puts a
/// requirement at the pin that the grants may not name, and the record of an owed re-grant is what
/// the next run reads.
fn settle_the_grants(
    commander: &dyn Commander,
    pin_display: &str,
    context: &SigningContext,
    before: &PinSignature,
    after: &str,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let recorded = recorded_grant(context);
    let granted = granted_requirement(context, before);
    let holds = match granted.as_deref() {
        Some(granted) if granted != after => satisfies(commander, pin_display, granted),
        _ => true,
    };
    let verdict = judge_grants(granted.as_deref(), recorded.is_some(), after, holds);
    let owed = matches!(verdict, GrantRecord::Owe(_) | GrantRecord::StillOwed);
    let changed = match verdict {
        GrantRecord::Owe(granted) => {
            let path = context.signing_dir.regrant_owed();
            let written = std::fs::create_dir_all(&context.signing_dir.root)
                .and_then(|()| std::fs::write(&path, format!("{}\n", granted)));
            if let Err(error) = written {
                findings.push(
                    Finding::needs_you(
                        "signing",
                        format!(
                            "could not record the owed re-grant at {}: {}",
                            path.display(),
                            error
                        ),
                    )
                    .note("the next doctor run cannot see that a re-grant is owed; make it now"),
                );
            }
            requirement_changed(before, after)
        },
        GrantRecord::StillOwed => Some(String::from(
            "an earlier run already moved this pin off the requirement the grants were made \
             against, and this signature does not satisfy it either",
        )),
        GrantRecord::Paid => {
            let _ = std::fs::remove_file(context.signing_dir.regrant_owed());
            None
        },
        // a pin that was never anchored held no grant a rebuild could keep, so this is the first
        // requirement worth granting against; an anchored one whose grants ride through needs none
        GrantRecord::Nothing => match before {
            PinSignature::Anchored { .. } => None,
            PinSignature::CodeHashed { .. } | PinSignature::Unsigned => {
                requirement_changed(before, after)
            },
        },
    };
    let mut next = follow_up(pin_display, changed);
    if owed {
        next = next
            .note("doctor keeps asking until `zellij session doctor --fix --regranted` records")
            .note("the re-grant as made");
    }
    findings.push(next);
    findings
}

/// The first line of a tool's complaint, which is the part worth quoting in a report.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("").trim()
}

/// Both halves of "did that take": the requirement no longer names a code hash, and the binary
/// SATISFIES the requirement it now carries.
///
/// Two questions and not one, because they fail apart in both directions. A signature can verify
/// while its requirement still names the code hash - a run that reported success and fixed nothing.
/// And a requirement can read perfectly while the binary does not satisfy it, which is worse,
/// because the first question is the one a text search answers and it says yes.
///
/// **`--verbose=2` is load-bearing, and this is the whole of the nkmk.7 failure.** Plain
/// `codesign -v <path>` returned 0 on a pin that `codesign -v --verbose=2 <path>` rejected with
/// `does not satisfy its designated Requirement` and exit 3 - the designated-requirement check is
/// what the second verbosity level adds. So the verbosity is not for the log: without it this
/// function passes exactly the signature it exists to catch. `--strict` costs nothing here and
/// refuses a few more things.
///
/// The message is matched as well as the exit status. Both were seen together on the machine this
/// was found on, and a check that rests on the exit status alone rests on the half that had
/// already been observed reporting success wrongly.
fn verify_signature(commander: &dyn Commander, target: &str) -> Result<PinSignature, String> {
    let described = commander
        .run("codesign", &["-d", "--verbose=2", "-r-", target], None)
        .map_err(|reason| reason)?;
    let signature = read_signature(&described.combined());
    match &signature {
        PinSignature::Anchored { .. } => {},
        PinSignature::CodeHashed { .. } => {
            return Err(String::from(
                "the requirement still names a code hash, so a rebuild would void every grant",
            ))
        },
        PinSignature::Unsigned => {
            return Err(String::from("codesign reports the copy as unsigned"))
        },
    }
    let verified = commander
        .run(
            "codesign",
            &["--verify", "--strict", "--verbose=2", target],
            None,
        )
        .map_err(|reason| reason)?;
    let said = verified.combined();
    if said.to_lowercase().contains("does not satisfy") {
        return Err(String::from(
            "the signature does not satisfy its own designated requirement, so it holds no grant",
        ));
    }
    if !verified.success {
        return Err(first_line(said.trim()).to_owned());
    }
    Ok(signature)
}

/// What to do next, in the order that makes it one pass instead of two.
///
/// Re-granting FIRST and restarting SECOND is the advice WHEN a re-grant is needed. The grants are
/// recorded against the pin's path and the requirement it now carries; a server started before
/// they are re-granted comes up not holding them, and the user ends up restarting twice.
///
/// **When the requirement did not change there is nothing to re-grant, and saying otherwise is not
/// a harmless extra step.** It sends a person into System Settings to revoke and re-add three
/// permissions that were already correct, and it teaches them that doctor's advice can be ignored.
/// A release pinned over a pin already on the release's requirement carries the same requirement -
/// that is the whole reason the pin is signed at all - so the ordinary case, an upgrade on a
/// machine already set up, needs only the restart.
fn follow_up(pin: &str, changed_requirement: Option<String>) -> Finding {
    let Some(why) = changed_requirement else {
        return Finding::needs_you("signing", "the signature is in place; one thing left")
            .note("`zellij session restart`, so the new server comes up running it")
            .note(format!(
                "the requirement is the one macOS already recorded for {}, so every grant",
                pin
            ))
            .note("it holds carries over and there is nothing to re-grant");
    };
    Finding::needs_you(
        "signing",
        "the signature is in place; two things left, in this order",
    )
    .note(format!(
        "1. re-grant Full Disk Access, Accessibility and Screen Recording for {}",
        pin
    ))
    .note("   in System Settings > Privacy & Security - once, for the new signature")
    .note("2. THEN `zellij session restart`, so the new server comes up already holding them")
    .note(why)
    .note("- that is why the re-grant is needed, and not only the restart")
}

/// Where doctor keeps what it records about the pin's signature.
///
/// `~/Library/Application Support/zellij/signing/`. Until nkmk.30 it also held the certificate the
/// ladder minted; doctor no longer reads, writes or backs up anything there but the record below,
/// and leaves any older file in it alone.
#[derive(Debug, Clone)]
pub struct SigningDir {
    pub root: PathBuf,
}

impl SigningDir {
    pub fn new(root: PathBuf) -> Self {
        SigningDir { root }
    }

    /// The requirement the grants were made against, kept once a signature stopped satisfying it.
    ///
    /// **Without this file a switch is visible for exactly one run.** The run that signs compares
    /// the requirement before with the one after and says "re-grant". The next run has only the
    /// pin, and a pin always satisfies its own requirement - so it reported that the grants
    /// "still hold" seven minutes after doctor had asked for them to be made again (m1p, nkmk.26).
    /// The file is that missing "before". It holds one designated requirement and nothing else.
    pub fn regrant_owed(&self) -> PathBuf {
        self.root.join("regrant-owed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_doctor::{
        recorded, recorded_failure, CommandOutput, RecordedCommander, Status,
    };

    /// Recorded from a pin signed with a Developer ID certificate.
    const DEVELOPER_ID: &str = "\
designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \"A1B2C3D4E5\"
Executable=/Users/someone/.local/share/zellij/bin/zellij
Identifier=org.zellij.nkmk
Format=Mach-O thin (arm64)
Signature=Developer ID Application: Someone (A1B2C3D4E5)
TeamIdentifier=A1B2C3D4E5
";

    /// Recorded from a pin signed with an Apple Development certificate and our own requirement.
    const APPLE_DEVELOPMENT: &str = "\
designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate leaf[subject.OU] = \"A1B2C3D4E5\"
Executable=/Users/someone/.local/share/zellij/bin/zellij
Identifier=org.zellij.nkmk
Signature=Apple Development: someone@example.com (F6G7H8I9J0)
";

    /// Recorded from a pin signed with a certificate we minted.
    const SELF_SIGNED: &str = "\
designated => identifier \"org.zellij.nkmk\" and certificate leaf = H\"7f8c0b1a2d3e4f5061728394a5b6c7d8e9f00112\"
Executable=/Users/someone/.local/share/zellij/bin/zellij
Identifier=org.zellij.nkmk
Signature=zellij self-signed code signing
";

    /// Recorded from a pin `codesign -s -` had signed - the state signing exists to leave.
    const AD_HOC: &str = "\
designated => identifier \"org.zellij.nkmk\" and cdhash H\"a1b2c3d4e5f60718293a4b5c6d7e8f9001122334\"
Executable=/Users/someone/.local/share/zellij/bin/zellij
Identifier=org.zellij.nkmk
Signature=adhoc
";

    /// Recorded from a pin nothing had ever signed.
    const UNSIGNED: &str = "\
/Users/someone/.local/share/zellij/bin/zellij: code object is not signed at all
";

    /// A context over a scratch directory, so a test drives the same code the Mac runs without
    /// going near a real home directory.
    fn context(root: &Path) -> SigningContext {
        SigningContext {
            signing_dir: SigningDir::new(root.join("signing")),
            refresh_from: None,
            regranted: false,
        }
    }

    #[test]
    fn a_developer_id_signature_is_anchored() {
        assert!(matches!(
            read_signature(DEVELOPER_ID),
            PinSignature::Anchored { .. }
        ));
    }

    #[test]
    fn an_apple_development_signature_is_anchored() {
        assert!(matches!(
            read_signature(APPLE_DEVELOPMENT),
            PinSignature::Anchored { .. }
        ));
    }

    #[test]
    fn a_self_signed_signature_anchors_on_the_certificate_not_the_code() {
        let signature = read_signature(SELF_SIGNED);
        assert!(matches!(signature, PinSignature::Anchored { .. }));
        assert!(signature.designated().unwrap().contains("certificate leaf"));
    }

    #[test]
    fn an_ad_hoc_signature_is_read_as_the_fault_it_is() {
        assert!(matches!(
            read_signature(AD_HOC),
            PinSignature::CodeHashed { .. }
        ));
    }

    #[test]
    fn an_unsigned_pin_is_not_mistaken_for_a_signed_one() {
        assert_eq!(read_signature(UNSIGNED), PinSignature::Unsigned);
    }

    /// The fault the identifier line exists to catch: output with no `Identifier=` in it must
    /// never be read as a signature, however much else it holds.
    #[test]
    fn output_without_an_identifier_line_is_never_believed() {
        assert_eq!(
            read_signature("designated => identifier \"org.zellij.nkmk\" and anchor apple\n"),
            PinSignature::Unsigned
        );
    }

    /// The team the pin in these fixtures is anchored on. Invented: a real team id in a fixture is a
    /// real team id in the repository.
    const GRANTED_TEAM: &str = "A1B2C3D4E5";

    /// The team a grant belongs to is the one in the requirement macOS recorded, so that is where
    /// it is read from - not from any certificate.
    #[test]
    fn the_team_a_grant_belongs_to_is_read_off_the_requirement_it_was_recorded_against() {
        // the requirement `codesign` derives for a Developer ID, with its two certificate fields
        assert_eq!(
            team_id_from_requirement(read_signature(DEVELOPER_ID).designated().unwrap()).as_deref(),
            Some(GRANTED_TEAM)
        );
        // and the one the retired Apple Development rung wrote by hand, which pins still carry
        assert_eq!(
            team_id_from_requirement(OTHER_TEAM_REQUIREMENT).as_deref(),
            Some("U2VEDWFUF3")
        );
        assert_eq!(
            team_id_from_requirement(&release_requirement()).as_deref(),
            Some(RELEASE_TEAM_ID)
        );
        // a requirement anchored on anything but the OU names no team
        assert_eq!(
            team_id_from_requirement(read_signature(SELF_SIGNED).designated().unwrap()),
            None
        );
        assert_eq!(
            team_id_from_requirement(read_signature(AD_HOC).designated().unwrap()),
            None
        );
        assert_eq!(
            team_id_from_requirement(
                "designated => identifier \"org.zellij.nkmk\" and anchor apple generic and \
                 certificate leaf[subject.CN] = \"Apple Development: someone@example.com\""
            ),
            None
        );
    }

    /// A re-grant is asked for when the requirement moved, and only then.
    #[test]
    fn a_re_grant_is_asked_for_only_when_the_requirement_actually_changed() {
        let anchored = |text: &str| PinSignature::Anchored {
            identifier: String::from(PIN_IDENTIFIER),
            designated: String::from(text),
        };
        let same = "designated => identifier \"org.zellij.nkmk\" and anchor apple generic";

        // The observed case, and the whole of finding 3: a pin refreshed onto the requirement it
        // already carried. Nothing to re-grant, and saying otherwise sent a user to System
        // Settings to redo three permissions that were already right.
        assert_eq!(requirement_changed(&anchored(same), same), None);
        // a different anchor IS a different requirement
        assert!(requirement_changed(&anchored(same), "designated => something else").is_some());
        // and an ad-hoc or unsigned pin never held a requirement worth keeping
        assert!(requirement_changed(
            &PinSignature::CodeHashed {
                identifier: String::from("zellij-1234"),
                designated: String::from("designated => cdhash H\"abc\""),
            },
            same
        )
        .is_some());
        assert!(requirement_changed(&PinSignature::Unsigned, same).is_some());

        // and the advice follows it: one step when nothing changed, two when something did
        let unchanged = follow_up("/tmp/pin", None);
        assert!(
            unchanged
                .notes
                .iter()
                .any(|note| note.contains("carries over")),
            "{:?}",
            unchanged.notes
        );
        assert!(
            !unchanged
                .notes
                .iter()
                .any(|note| note.contains("re-grant Full Disk Access")),
            "{:?}",
            unchanged.notes
        );
        assert!(follow_up("/tmp/pin", Some(String::from("because")))
            .notes
            .iter()
            .any(|note| note.contains("re-grant Full Disk Access")));
    }

    /// Which runs defer and which do not. A pin with no signature to lose is refreshed as before -
    /// pinning the new build is worth more than protecting an ad-hoc signature that no rebuild
    /// could satisfy anyway.
    #[test]
    fn only_an_anchored_pin_is_worth_deferring_a_refresh_for() {
        let acting = DoctorMode {
            fix: true,
            ..DoctorMode::default()
        };
        let exe = PathBuf::from("/usr/local/bin/zellij");
        let anchored = RecordedCommander::new(&[("codesign -d ", recorded(DEVELOPER_ID))]);
        let ad_hoc = RecordedCommander::new(&[("codesign -d ", recorded(AD_HOC))]);

        assert_eq!(
            refresh_belongs_to_signing(
                &anchored,
                Path::new("/tmp/pin"),
                acting,
                Some(exe.clone()),
                true
            ),
            Some(exe.clone())
        );
        assert_eq!(
            refresh_belongs_to_signing(
                &ad_hoc,
                Path::new("/tmp/pin"),
                acting,
                Some(exe.clone()),
                true
            ),
            None
        );
        // nothing to refresh
        assert_eq!(
            refresh_belongs_to_signing(
                &anchored,
                Path::new("/tmp/pin"),
                acting,
                Some(exe.clone()),
                false
            ),
            None
        );
        // a dry run refreshes nothing, and a --no-sign run has nothing coming after the refresh
        for mode in [
            DoctorMode {
                fix: false,
                ..DoctorMode::default()
            },
            DoctorMode {
                sign: false,
                ..DoctorMode::default()
            },
        ] {
            assert_eq!(
                refresh_belongs_to_signing(
                    &anchored,
                    Path::new("/tmp/pin"),
                    mode,
                    Some(exe.clone()),
                    true
                ),
                None
            );
        }
    }

    #[test]
    fn an_already_anchored_pin_is_left_alone_and_never_signed_again() {
        let commander = RecordedCommander::new(&[
            (
                "codesign -d --verbose=2 -r- /tmp/pin",
                recorded(DEVELOPER_ID),
            ),
            (
                "codesign --verify --strict --verbose=2 /tmp/pin",
                recorded("/tmp/pin: valid on disk"),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            Path::new("/tmp/pin"),
            DoctorMode::default(),
            &context(scratch.path()),
        );
        assert_eq!(run.findings[0].status, Status::AlreadyCorrect);
        assert!(!commander.called_with("-s "), "{:?}", commander.calls());
        // reading the requirement is not checking it, so the pin it leaves alone is a pin it
        // actually verified
        assert!(
            commander.called_with("codesign --verify --strict --verbose=2 /tmp/pin"),
            "{:?}",
            commander.calls()
        );
    }

    /// The nkmk.7 failure, in the state it left a real Mac in: a pin whose requirement reads
    /// perfectly and whose binary does not satisfy it. doctor called that `AlreadyCorrect` and
    /// exited 0 for two releases. It is a `Needs you` now, and nothing on this machine signs it.
    #[test]
    fn an_anchored_pin_that_does_not_verify_is_not_healthy_and_nothing_is_signed() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"a pretend 46 MB binary").unwrap();
        let pin_display = pin.display().to_string();

        let commander = RecordedCommander::new(&[
            // the pin reads as anchored - identifier, no cdhash anywhere
            (
                format!("codesign -d --verbose=2 -r- {}", pin_display).as_str(),
                recorded(APPLE_DEVELOPMENT),
            ),
            // and fails the check that reading it cannot make
            (
                format!("codesign --verify --strict --verbose=2 {}", pin_display).as_str(),
                recorded_failure(&format!(
                    "{}: valid on disk\n{}: does not satisfy its designated Requirement",
                    pin_display, pin_display
                )),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );

        let finding = run
            .findings
            .iter()
            .find(|finding| {
                finding.status == Status::NeedsYou
                    && finding
                        .message
                        .contains("does not satisfy its own requirement")
            })
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(
            finding
                .notes
                .iter()
                .any(|note| note.contains("brew release")),
            "{:?}",
            finding.notes
        );
        assert!(
            !commander.called_with("codesign -s"),
            "something signed the pin: {:?}",
            commander.calls()
        );
        assert_eq!(
            std::fs::read(&pin).unwrap(),
            b"a pretend 46 MB binary".to_vec()
        );
    }

    /// A verification that fails only at `--verbose=2` is the one that matters, because that is
    /// the level at which the designated requirement is checked at all.
    #[test]
    fn a_signature_that_does_not_satisfy_its_own_requirement_refuses_the_rung() {
        let commander = RecordedCommander::new(&[
            (
                "codesign --verify --strict --verbose=2 /tmp/pin",
                recorded_failure("/tmp/pin: does not satisfy its designated Requirement"),
            ),
            (
                "codesign -d --verbose=2 -r- /tmp/pin",
                recorded(DEVELOPER_ID),
            ),
        ]);
        let refusal = verify_signature(&commander, "/tmp/pin").unwrap_err();
        assert!(
            refusal.contains("does not satisfy its own designated requirement"),
            "{}",
            refusal
        );
    }

    /// A local build - `cargo build`, a source formula - carries the linker's ad-hoc signature.
    /// Doctor says what that costs and names the release as the remedy. It signs nothing, and it
    /// asks no keychain anything.
    #[test]
    fn a_pin_that_is_not_a_release_build_is_reported_and_nothing_is_signed() {
        for (signature, said) in [(AD_HOC, "ad-hoc"), (UNSIGNED, "not signed at all")] {
            let commander = RecordedCommander::new(&[(
                "codesign -d --verbose=2 -r- /tmp/pin",
                recorded(signature),
            )]);
            let scratch = tempfile::tempdir().unwrap();
            let run = sign_pin(
                &commander,
                Path::new("/tmp/pin"),
                DoctorMode::default(),
                &context(scratch.path()),
            );
            assert_eq!(run.findings.len(), 1, "{:?}", run.findings);
            let finding = &run.findings[0];
            assert_eq!(finding.status, Status::NeedsYou);
            assert_eq!(
                finding.message,
                "/tmp/pin is not a release build, so the pin holds no grants"
            );
            assert!(
                finding.notes.iter().any(|note| note.contains(said)),
                "{:?}",
                finding.notes
            );
            assert!(
                finding
                    .notes
                    .iter()
                    .any(|note| note.contains("install the brew release to get a signed pin")),
                "{:?}",
                finding.notes
            );
            assert_eq!(commander.calls().len(), 1, "{:?}", commander.calls());
        }
    }

    /// `--no-sign` leaves a signed pin as it is, even for a release build: the refresh is not
    /// attempted, and the pin is judged as it stands.
    #[test]
    fn no_sign_reports_the_fault_and_touches_nothing() {
        let (_directory, pin, build, scratch) = a_pin_and_a_release_build();
        let commander = refreshing_from_the_release(&pin, &build, AD_HOC, &[]);
        let mut context = context(scratch.path());
        context.refresh_from = Some(build);
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                sign: false,
                ..DoctorMode::default()
            },
            &context,
        );
        assert_eq!(run.findings[0].status, Status::NeedsYou);
        assert_eq!(std::fs::read(&pin).unwrap(), b"the OLD build".to_vec());
        assert_eq!(commander.calls().len(), 1, "{:?}", commander.calls());
    }

    /// The check that keeps a signing run from being the one that wins by accident. `session up`
    /// writes the pin by its own copy-then-rename, so a newer build landing while doctor was
    /// signing would be undone by doctor's rename of the older one - silently, because a rename
    /// over a file reports nothing about what was there.
    #[test]
    fn a_pin_replaced_while_it_was_being_signed_is_noticed() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the build doctor decided about").unwrap();

        let before = pin_identity(&pin);
        assert!(before.is_some(), "the pin is there to begin with");
        assert!(
            pin_unchanged_since(&pin, &before),
            "an untouched pin is unchanged"
        );

        // `session up` lands a newer build over it
        std::fs::write(&pin, b"a newer build, landed by session up").unwrap();
        assert!(!pin_unchanged_since(&pin, &before));

        // and a pin that has gone counts as changed rather than as "nothing to compare"
        std::fs::remove_file(&pin).unwrap();
        assert!(!pin_unchanged_since(&pin, &before));
    }

    /// The other direction, which is the ordinary first signing on a machine: there was no pin, so
    /// there is nothing to be replaced, and one appearing underneath is still a change.
    #[test]
    fn a_pin_that_appears_under_a_signing_run_is_a_change_too() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");

        let before = pin_identity(&pin);
        assert_eq!(before, None, "there is no pin yet");
        assert!(pin_unchanged_since(&pin, &before));

        std::fs::write(&pin, b"somebody else got there first").unwrap();
        assert!(!pin_unchanged_since(&pin, &before));
    }

    /// A pid that is beyond argument finished: spawned, waited for, and reaped.
    #[cfg(unix)]
    fn a_pid_that_has_finished() -> u32 {
        // `/bin/sh`, not `/bin/true`: POSIX puts a shell at that path on every unix, while macOS
        // keeps `true` in `/usr/bin` and has nothing at `/bin/true` to spawn.
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("every unix has a shell");
        let pid = child.id();
        child.wait().expect("it exits at once");
        pid
    }

    /// A `.zellij.sign.<pid>.tmp` of a chosen age. `utimensat`, because `std::fs` cannot set an
    /// mtime and the age gate cannot be tested without going back an hour.
    #[cfg(unix)]
    fn a_signing_temp(directory: &Path, pid: u32, age: std::time::Duration) -> PathBuf {
        use std::ffi::CString;

        let path = directory.join(format!("{}{}.tmp", sign_temp_prefix(), pid));
        std::fs::write(&path, b"a pretend 46 MB copy").unwrap();
        let when = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            - age;
        let raw = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        let stamp = libc::timespec {
            tv_sec: when.as_secs() as i64,
            tv_nsec: 0,
        };
        let times = [stamp, stamp];
        let set = unsafe { libc::utimensat(libc::AT_FDCWD, raw.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(set, 0, "could not age the temp file");
        path
    }

    /// The sweep took every `.zellij.sign.*.tmp` it found, with no gate of any kind - including
    /// the one a signing run happening right now was about to `codesign` and rename.
    ///
    /// Four files, one for each answer the sweep has to get right. The pin temp is given THE SAME
    /// dead pid as the swept file on purpose: any other pid and the liveness gate would be what
    /// spares it, and the prefix - the thing that keeps the two sweeps out of each other's files -
    /// would go untested.
    #[test]
    #[cfg(unix)]
    fn a_signing_temp_is_swept_only_when_its_run_is_gone_and_it_is_old() {
        let directory = tempfile::tempdir().unwrap();
        let old = std::time::Duration::from_secs(48 * 60 * 60);
        let finished = a_pid_that_has_finished();

        let abandoned = a_signing_temp(directory.path(), finished, old);
        let in_flight = a_signing_temp(directory.path(), std::process::id(), old);
        let young = a_signing_temp(
            directory.path(),
            a_pid_that_has_finished(),
            std::time::Duration::from_secs(60),
        );
        let pin_temp = directory
            .path()
            .join(format!(".zellij.pin.{}.tmp", finished));
        std::fs::write(&pin_temp, b"the pin's own temp").unwrap();

        assert_eq!(sweep_stale_temps(directory.path()), vec![abandoned.clone()]);
        assert!(!abandoned.exists(), "the abandoned copy is still there");
        assert!(
            in_flight.exists(),
            "a signing run in flight had its temp deleted under it"
        );
        assert!(young.exists(), "a temp younger than the gate was taken");
        assert!(pin_temp.exists(), "the pin's own temp was taken");
    }

    /// The line `session up` prints when the transaction refused has to be the transaction's own
    /// reason. A summary written here would drift from what `session doctor` says a minute later,
    /// and the two disagreeing is worse than either being terse.
    #[test]
    fn a_refusal_quotes_the_finding_the_transaction_gave() {
        let findings = vec![
            Finding::ok("signing", "the pin is signed"),
            Finding::needs_you("signing", "the new build is not a release build")
                .note("the pin was NOT refreshed, so the previously signed copy is still in place,")
                .note("on the previous build"),
        ];
        let said = refusal_from(&findings);
        assert!(said.starts_with("the new build is not a release build"));
        assert!(said.contains("the pin was NOT refreshed"));
    }

    /// A run where nothing needed a person still has to say something: the caller only reaches
    /// this when the pin did not move, so silence there would be a warning with no reason in it.
    #[test]
    fn a_refusal_with_nothing_needing_a_person_still_says_something() {
        assert_eq!(
            refusal_from(&[Finding::ok("signing", "left alone")]),
            "left alone"
        );
        assert_eq!(refusal_from(&[]), "the signing step said nothing");
    }

    /// The designated requirement a certificate of ours writes, as `SELF_SIGNED` carries it.
    const OURS_REQUIREMENT: &str = "designated => identifier \"org.zellij.nkmk\" and certificate leaf = H\"7f8c0b1a2d3e4f5061728394a5b6c7d8e9f00112\"";

    /// The one an Apple Development certificate writes, as `APPLE_DEVELOPMENT` carries it.
    const APPLE_REQUIREMENT: &str = "designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate leaf[subject.OU] = \"A1B2C3D4E5\"";

    /// What `satisfies` runs to test `target` against a requirement it may not carry.
    fn test_requirement(requirement: &str, target: &Path) -> String {
        format!(
            "codesign --verify -R={} {}",
            requirement.trim_start_matches("designated =>").trim(),
            target.display()
        )
    }

    #[test]
    fn the_record_of_an_owed_re_grant_follows_what_the_grants_name() {
        // never anchored: nothing a grant could hold
        assert_eq!(
            judge_grants(None, false, APPLE_REQUIREMENT, false),
            GrantRecord::Nothing
        );
        // same requirement, or a new one the old requirement still accepts: nothing owed
        assert_eq!(
            judge_grants(Some(APPLE_REQUIREMENT), false, APPLE_REQUIREMENT, false),
            GrantRecord::Nothing
        );
        assert_eq!(
            judge_grants(Some(APPLE_REQUIREMENT), false, "designated => other", true),
            GrantRecord::Nothing
        );
        // a switch the old requirement does not accept is recorded, against the OLD one
        assert_eq!(
            judge_grants(Some(OURS_REQUIREMENT), false, APPLE_REQUIREMENT, false),
            GrantRecord::Owe(String::from(OURS_REQUIREMENT))
        );
        // a recorded debt stays until the pin satisfies it again
        assert_eq!(
            judge_grants(Some(OURS_REQUIREMENT), true, APPLE_REQUIREMENT, false),
            GrantRecord::StillOwed
        );
        assert_eq!(
            judge_grants(Some(OURS_REQUIREMENT), true, OURS_REQUIREMENT, false),
            GrantRecord::Paid
        );
        assert_eq!(
            judge_grants(Some(OURS_REQUIREMENT), true, "designated => other", true),
            GrantRecord::Paid
        );
    }

    /// The reason the record exists: the release that switches the pin asks for the re-grant, and
    /// so does every run after it, until the operator says it is made. The pin here was anchored
    /// on a minted certificate, as one Mac's was before nkmk.30.
    #[test]
    fn a_switch_stays_owed_on_later_runs_until_the_operator_says_it_is_made() {
        let (_directory, pin, build, scratch) = a_pin_and_a_release_build();

        // 1. the switching run: a release refreshed over a pin anchored on our old certificate
        let switching = refreshing_from_the_release(
            &pin,
            &build,
            SELF_SIGNED,
            &[(
                test_requirement(OURS_REQUIREMENT, &pin).as_str(),
                recorded_failure(
                    "test-requirement: code failed to satisfy specified code requirement(s)",
                ),
            )],
        );
        let mut first = context(scratch.path());
        first.refresh_from = Some(build);
        let run = sign_pin(&switching, &pin, DoctorMode::default(), &first);
        assert!(
            !switching.called_with("codesign -s"),
            "{:?}",
            switching.calls()
        );
        let follow = run
            .findings
            .iter()
            .find(|finding| finding.message.contains("two things left"))
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(
            follow.notes.iter().any(|note| note.contains("--regranted")),
            "{:?}",
            follow.notes
        );
        let owed_path = first.signing_dir.regrant_owed();
        assert_eq!(
            std::fs::read_to_string(&owed_path).unwrap().trim(),
            OURS_REQUIREMENT,
            "the record must name the requirement the grants were made against, not the new one"
        );

        // 2. a later dry run: the pin satisfies its own requirement, and that is not enough
        let later = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(RELEASE_SIGNED),
            ),
            ("codesign --verify --strict", recorded("")),
            (
                test_requirement(OURS_REQUIREMENT, &pin).as_str(),
                recorded_failure(
                    "test-requirement: code failed to satisfy specified code requirement(s)",
                ),
            ),
        ]);
        let dry = DoctorMode::from_flags(true, false, false);
        let run = sign_pin(&later, &pin, dry, &context(scratch.path()));
        let owed = run
            .findings
            .iter()
            .find(|finding| finding.status == Status::NeedsYou)
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(owed.message.contains("another requirement"), "{:?}", owed);
        // the pin is on the release's requirement now, and the finding says so
        assert!(
            owed.notes
                .iter()
                .any(|note| note.contains("switched this pin to the release's Developer ID")),
            "{:?}",
            owed.notes
        );
        assert!(
            !owed
                .notes
                .iter()
                .any(|note| note.contains("re-signed it with another certificate")),
            "{:?}",
            owed.notes
        );
        assert!(
            !run.findings
                .iter()
                .any(|finding| finding.notes.iter().any(|note| note.contains("still hold"))),
            "a later run called the grants held again: {:?}",
            run.findings
        );

        // 3. --regranted in a dry run says what it would do and keeps the record
        let mut confirming = context(scratch.path());
        confirming.regranted = true;
        let run = sign_pin(&later, &pin, dry, &confirming);
        assert!(owed_path.exists());
        assert!(
            run.findings.iter().any(|finding| finding
                .message
                .starts_with("would record the owed re-grant")),
            "{:?}",
            run.findings
        );

        // 4. --fix --regranted pays it, and the pin reads as holding its grants again
        let run = sign_pin(&later, &pin, DoctorMode::default(), &confirming);
        assert!(!owed_path.exists());
        assert!(
            run.findings
                .iter()
                .all(|finding| finding.status != Status::NeedsYou),
            "{:?}",
            run.findings
        );
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.notes.iter().any(|note| note.contains("still hold"))),
            "{:?}",
            run.findings
        );
    }

    /// A pin that satisfies the recorded requirement again - signed back onto our certificate -
    /// pays the debt without being told. Only a run that may act forgets the record.
    #[test]
    fn a_pin_back_on_the_granted_requirement_pays_the_record() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the build").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let context = context(scratch.path());
        std::fs::create_dir_all(&context.signing_dir.root).unwrap();
        let owed_path = context.signing_dir.regrant_owed();
        std::fs::write(&owed_path, format!("{}\n", APPLE_REQUIREMENT)).unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(DEVELOPER_ID),
            ),
            ("codesign --verify --strict", recorded("")),
            // a Developer ID of the same team satisfies the Apple Development requirement
            (
                test_requirement(APPLE_REQUIREMENT, &pin).as_str(),
                recorded(""),
            ),
        ]);
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::from_flags(true, false, false),
            &context,
        );
        assert!(
            run.findings
                .iter()
                .all(|finding| finding.status != Status::NeedsYou),
            "{:?}",
            run.findings
        );
        assert!(owed_path.exists(), "a dry run removed the record");

        sign_pin(&commander, &pin, DoctorMode::default(), &context);
        assert!(!owed_path.exists());
    }

    /// Recorded from the release binary: our Developer ID, signed in `release.yml`.
    const RELEASE_SIGNED: &str = "\
designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate 1[field.1.2.840.113635.100.6.2.6] /* exists */ and certificate leaf[field.1.2.840.113635.100.6.1.13] /* exists */ and certificate leaf[subject.OU] = \"2Z88BYP37C\"
Executable=/opt/homebrew/Cellar/zellij-nkmk/0.45.1-nkmk.30/bin/zellij
Identifier=org.zellij.nkmk
Signature=Developer ID Application: NKMK Digital Co. (2Z88BYP37C)
TeamIdentifier=2Z88BYP37C
";

    /// A pin signed with Apple Development under another team, with our own requirement.
    const OTHER_TEAM_APPLE_DEVELOPMENT: &str = "\
designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate leaf[subject.OU] = \"U2VEDWFUF3\"
Identifier=org.zellij.nkmk
Signature=Apple Development: someone@example.com (DY7JA3K8QZ)
";

    const OTHER_TEAM_REQUIREMENT: &str = "designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate leaf[subject.OU] = \"U2VEDWFUF3\"";

    /// Apple Development under the RELEASE team: the team matches and the certificate is not a
    /// Developer ID, so it is not the release's signature.
    const RELEASE_TEAM_APPLE_DEVELOPMENT: &str = "\
designated => identifier \"org.zellij.nkmk\" and anchor apple generic and certificate leaf[subject.OU] = \"2Z88BYP37C\"
Identifier=org.zellij.nkmk
Signature=Apple Development: someone@example.com (F6G7H8I9J0)
";

    /// The pin, its directory, a release build to refresh from, and a scratch signing dir.
    fn a_pin_and_a_release_build() -> (tempfile::TempDir, PathBuf, PathBuf, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the OLD build").unwrap();
        let build = directory.path().join("new-zellij");
        std::fs::write(&build, b"the release build").unwrap();
        (directory, pin, build, tempfile::tempdir().unwrap())
    }

    /// A commander for a refresh from a release build, over a pin carrying `before`.
    fn refreshing_from_the_release(
        pin: &Path,
        build: &Path,
        before: &str,
        extra: &[(&str, CommandOutput)],
    ) -> RecordedCommander {
        let pin_described = format!("codesign -d --verbose=2 -r- {}", pin.display());
        let release_tested = test_requirement(&release_requirement(), build);
        let mut answers = vec![
            (pin_described.as_str(), recorded(before)),
            // the build, and then the copy of it beside the pin
            ("codesign -d --verbose=2 -r- ", recorded(RELEASE_SIGNED)),
            ("codesign --verify --strict", recorded("")),
            (release_tested.as_str(), recorded("")),
        ];
        answers.extend(extra.iter().map(|(line, output)| (*line, output.clone())));
        RecordedCommander::new(&answers)
    }

    /// (a) A release build goes in as it came. No `codesign -s`, no keychain, and the pin is the
    /// build's own bytes, which is what keeps its signature.
    #[test]
    fn a_release_build_is_pinned_as_it_came_and_nothing_is_signed() {
        let (_directory, pin, build, scratch) = a_pin_and_a_release_build();
        let commander = refreshing_from_the_release(&pin, &build, DEVELOPER_ID, &[]);
        let mut context = context(scratch.path());
        context.refresh_from = Some(build.clone());
        let run = sign_pin(&commander, &pin, DoctorMode::default(), &context);

        assert_eq!(std::fs::read(&pin).unwrap(), b"the release build".to_vec());
        assert!(
            !commander.called_with("codesign -s"),
            "a release build was signed again: {:?}",
            commander.calls()
        );
        assert!(
            !commander.called_with("security"),
            "the keychain was asked: {:?}",
            commander.calls()
        );
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.message.contains("nothing was signed here")),
            "{:?}",
            run.findings
        );
        // the copy is verified before it is renamed, exactly as a signed copy is
        assert!(
            commander.called_with(&format!(
                "codesign -d --verbose=2 -r- {}",
                pin.parent().unwrap().join(sign_temp_prefix()).display()
            )),
            "{:?}",
            commander.calls()
        );
        // the stamp names the build, so the next pass does not refresh it again
        #[cfg(unix)]
        assert!(!crate::session_lifecycle::pin_needs_refresh(&build, &pin));
    }

    /// (b) The first release over a pin anchored on another requirement owes the re-grant, and it
    /// is recorded once: a second release over it leaves the record as it was, and a later run
    /// with nothing to refresh still asks.
    #[test]
    fn the_first_release_over_another_requirement_records_the_re_grant_once() {
        let (_directory, pin, build, scratch) = a_pin_and_a_release_build();
        let other_team_tested = test_requirement(OTHER_TEAM_REQUIREMENT, &pin);
        let unsatisfied = recorded_failure(
            "test-requirement: code failed to satisfy specified code requirement(s)",
        );
        let commander = refreshing_from_the_release(
            &pin,
            &build,
            OTHER_TEAM_APPLE_DEVELOPMENT,
            &[(other_team_tested.as_str(), unsatisfied.clone())],
        );
        let mut first = context(scratch.path());
        first.refresh_from = Some(build.clone());
        let run = sign_pin(&commander, &pin, DoctorMode::default(), &first);
        assert!(!commander.called_with("codesign -s"));
        let owed_path = first.signing_dir.regrant_owed();
        assert_eq!(
            std::fs::read_to_string(&owed_path).unwrap().trim(),
            OTHER_TEAM_REQUIREMENT,
            "the record must name the requirement the grants were made against"
        );
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.message.contains("two things left")),
            "{:?}",
            run.findings
        );

        // a second release: the pin now carries the release's own requirement, and the debt is
        // still the one recorded, not a new one
        std::fs::write(&build, b"the next release build").unwrap();
        let second = refreshing_from_the_release(
            &pin,
            &build,
            RELEASE_SIGNED,
            &[(other_team_tested.as_str(), unsatisfied.clone())],
        );
        sign_pin(&second, &pin, DoctorMode::default(), &first);
        assert_eq!(
            std::fs::read_to_string(&owed_path).unwrap().trim(),
            OTHER_TEAM_REQUIREMENT
        );

        // and a run with nothing to refresh keeps asking until `--regranted`
        let later = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(RELEASE_SIGNED),
            ),
            ("codesign --verify --strict", recorded("")),
            (other_team_tested.as_str(), unsatisfied),
        ]);
        let run = sign_pin(
            &later,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.status == Status::NeedsYou
                    && finding.message.contains("another requirement")),
            "{:?}",
            run.findings
        );
    }

    /// (c) A pin already on the release's requirement - the second release since the switch, with
    /// the re-grant made - owes nothing and records nothing.
    #[test]
    fn a_release_over_a_pin_of_the_same_team_records_nothing() {
        let (_directory, pin, build, scratch) = a_pin_and_a_release_build();
        let commander = refreshing_from_the_release(&pin, &build, RELEASE_SIGNED, &[]);
        let mut context = context(scratch.path());
        context.refresh_from = Some(build);
        let run = sign_pin(&commander, &pin, DoctorMode::default(), &context);

        assert!(!commander.called_with("codesign -s"));
        assert!(!context.signing_dir.regrant_owed().exists());
        let follow = run
            .findings
            .iter()
            .find(|finding| finding.message.contains("one thing left"))
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(
            follow
                .notes
                .iter()
                .any(|note| note.contains("nothing to re-grant")),
            "{:?}",
            follow.notes
        );
    }

    /// (d) Anything that is not the release's Developer ID does not replace a signed pin: another
    /// team's Developer ID, an Apple Development certificate that names the release team but is
    /// not a Developer ID at all, and a local ad-hoc build. Nothing is signed, the pin keeps its
    /// bytes, and the finding names the build and says the pin was not refreshed.
    #[test]
    fn a_build_that_is_not_the_releases_is_not_pinned_over_a_signed_pin() {
        for (build_signature, why) in [
            (DEVELOPER_ID, "another team's Developer ID"),
            (
                RELEASE_TEAM_APPLE_DEVELOPMENT,
                "the release team without the Developer ID markers",
            ),
            (AD_HOC, "an ad-hoc local build"),
        ] {
            let (_directory, pin, build, scratch) = a_pin_and_a_release_build();
            let commander = RecordedCommander::new(&[
                (
                    format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                    recorded(DEVELOPER_ID),
                ),
                (
                    format!("codesign -d --verbose=2 -r- {}", build.display()).as_str(),
                    recorded(build_signature),
                ),
                // only `--strict`: the release requirement is not recorded, so asking it fails,
                // as it does on a Mac for every build the release did not sign
                ("codesign --verify --strict", recorded("")),
            ]);
            let mut context = context(scratch.path());
            context.refresh_from = Some(build.clone());
            let run = sign_pin(&commander, &pin, DoctorMode::default(), &context);

            assert!(
                !commander.called_with("codesign -s"),
                "{}: {:?}",
                why,
                commander.calls()
            );
            assert!(
                !commander.called_with("security"),
                "{}: {:?}",
                why,
                commander.calls()
            );
            let refused = run
                .findings
                .iter()
                .find(|finding| finding.status == Status::NeedsYou)
                .unwrap_or_else(|| panic!("{}: {:?}", why, run.findings));
            assert!(
                refused.message.contains("is not a release build")
                    && refused.message.contains(&build.display().to_string()),
                "{}: {:?}",
                why,
                refused
            );
            assert!(
                refused
                    .notes
                    .iter()
                    .any(|note| note.contains("the pin was NOT refreshed")),
                "{}: {:?}",
                why,
                refused.notes
            );
            assert_eq!(
                std::fs::read(&pin).unwrap(),
                b"the OLD build".to_vec(),
                "{}",
                why
            );
            assert!(!context.signing_dir.regrant_owed().exists(), "{}", why);
        }
    }
}
