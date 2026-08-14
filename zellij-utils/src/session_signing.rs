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

/// The common name of the certificate we mint when the machine has no Apple one.
pub const SELF_SIGNED_COMMON_NAME: &str = "zellij self-signed code signing";

/// Twenty years. The certificate is never reissued - see [`mint_self_signed`] - so its lifetime
/// has to outlast the machine rather than the release.
const SELF_SIGNED_DAYS: &str = "7300";

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

/// One signing certificate the keychain will offer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The SHA-1 `codesign -s` is given. Passed rather than the name because two certificates can
    /// share a name and only one of them is the one that was chosen.
    pub hash: String,
    pub name: String,
}

/// Read `security find-identity -v -p codesigning`.
///
/// Only the numbered lines, and only the quoted name off each one. The trailing "N valid
/// identities found" is not a certificate and neither is a blank line, and a parser that took
/// every line would offer the summary as something to sign with.
pub fn parse_identities(output: &str) -> Vec<Identity> {
    output
        .lines()
        .filter_map(|line| {
            // the shape is `  1) <sha1> "<name>"`, and the leading number is what tells a
            // certificate line from the summary that follows the list
            let (index, rest) = line.trim().split_once(')')?;
            if index.is_empty() || !index.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            let (hash, name) = rest.trim().split_once(' ')?;
            let name = name.trim().strip_prefix('"')?.strip_suffix('"')?;
            (!hash.is_empty() && !name.is_empty()).then(|| Identity {
                hash: hash.to_owned(),
                name: name.to_owned(),
            })
        })
        .collect()
}

/// Which certificate to sign with, and what that choice implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rung {
    /// A Developer ID. The best case: its designated requirement is already anchored on the team
    /// id, and a timestamped signature outlives the certificate.
    DeveloperId(Identity),
    /// An Apple Development certificate, with the team id read off its name. The requirement has
    /// to be written by hand for this one - see [`requirement_for`].
    AppleDevelopment { identity: Identity, team: String },
    /// One we minted ourselves, once, and will never mint again.
    SelfSigned(Identity),
}

impl Rung {
    pub fn identity(&self) -> &Identity {
        match self {
            Rung::DeveloperId(identity)
            | Rung::AppleDevelopment { identity, .. }
            | Rung::SelfSigned(identity) => identity,
        }
    }

    /// Whether Apple's timestamp server will accept a signature from this certificate.
    ///
    /// Only a real chain can be timestamped. A self-signed certificate is refused, which is not a
    /// failure to report - it costs the signature nothing but its survival past an expiry the
    /// certificate does not have.
    pub fn can_timestamp(&self) -> bool {
        !matches!(self, Rung::SelfSigned(_))
    }

    pub fn description(&self) -> &'static str {
        match self {
            Rung::DeveloperId(_) => "Developer ID",
            Rung::AppleDevelopment { .. } => "Apple Development",
            Rung::SelfSigned(_) => "a certificate of our own",
        }
    }
}

/// The team id inside an identity name - the parenthesised code at the end of it.
///
/// `Apple Development: someone@example.com (A1B2C3D4E5)` gives `A1B2C3D4E5`. It is the same on
/// every machine signed into the same Apple ID, which is the whole reason the requirement is
/// anchored on it.
pub fn team_id(name: &str) -> Option<String> {
    let inside = name.rsplit_once('(')?.1.strip_suffix(')')?;
    (!inside.is_empty() && inside.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| inside.to_owned())
}

/// The first rung of the ladder this keychain can reach.
///
/// Order is the whole design. A Developer ID signature is accepted anywhere and survives its own
/// certificate's expiry; an Apple Development one is good on the machines signed into that Apple
/// ID; ours is good only here, and is the rung a corporate Mac with no developer account lands on.
/// Nothing below the third rung: an ad-hoc signature anchors on the code hash, which is the state
/// signing exists to leave.
pub fn choose_rung(identities: &[Identity]) -> Option<Rung> {
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.name.starts_with("Developer ID Application:"))
    {
        return Some(Rung::DeveloperId(identity.clone()));
    }
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.name.starts_with("Apple Development:"))
    {
        // without a team id there is nothing stable to anchor on, so this certificate is no better
        // than the code hash and the ladder keeps going down
        if let Some(team) = team_id(&identity.name) {
            return Some(Rung::AppleDevelopment {
                identity: identity.clone(),
                team,
            });
        }
    }
    identities
        .iter()
        .find(|identity| identity.name.contains(SELF_SIGNED_COMMON_NAME))
        .map(|identity| Rung::SelfSigned(identity.clone()))
}

/// The requirement to write into the signature, when the default one would not do.
///
/// Only Apple Development needs this. `codesign` derives that certificate's designated requirement
/// from `subject.CN`, and the CN carries the developer's email address - so it changes when the
/// certificate is reissued, and it differs between two machines belonging to the same person. The
/// OU is the team id: the same string on every machine signed into that Apple ID, and stable
/// across a reissue. A grant recorded against a CN-anchored requirement is a grant that expires
/// with the certificate.
///
/// Developer ID needs nothing written: `codesign` already anchors it on `subject.OU`. Self-signed
/// needs nothing either, and must not be given one - its default requirement names the certificate
/// by hash, which is exactly the anchor that makes it worth having.
pub fn requirement_for(rung: &Rung) -> Option<String> {
    match rung {
        Rung::AppleDevelopment { team, .. } => Some(format!(
            "identifier \"{}\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
            PIN_IDENTIFIER, team
        )),
        Rung::DeveloperId(_) | Rung::SelfSigned(_) => None,
    }
}

/// The argv `codesign` is given to sign `target`.
///
/// Split out from the running of it so a test can read it. The order is not cosmetic: `-f` has to
/// be there or a second run refuses a file that already carries a signature, and `--identifier`
/// has to be there or `codesign` derives one from the file name - which would change the
/// requirement the day somebody renames the pin.
pub fn sign_arguments<'a>(
    rung: &'a Rung,
    requirement: Option<&'a str>,
    timestamp: bool,
    target: &'a str,
) -> Vec<String> {
    let mut args = vec![
        String::from("-s"),
        rung.identity().hash.clone(),
        String::from("-f"),
        String::from("--identifier"),
        String::from(PIN_IDENTIFIER),
    ];
    if let Some(requirement) = requirement {
        args.push(format!("-r={}", requirement));
    }
    if timestamp {
        args.push(String::from("--timestamp"));
    }
    args.push(target.to_owned());
    args
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
pub fn sweep_stale_temps(directory: &Path) -> Vec<PathBuf> {
    let mut swept: Vec<PathBuf> = std::fs::read_dir(directory)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(sign_temp_prefix()) && name.ends_with(".tmp"))
        })
        .filter(|path| std::fs::remove_file(path).is_ok())
        .collect();
    swept.sort();
    swept
}

/// What one pass over the pinned copy's signature came to.
pub struct SigningRun {
    pub findings: Vec<Finding>,
}

/// Bring the pinned copy's signature to something that outlives the build.
///
/// The order of the steps is the design and it is worth stating why each one is where it is.
///
/// 1. Read the requirement FIRST. An already-anchored pin must not be signed again: the new
///    signature would carry a new certificate hash and void every grant it currently holds.
/// 2. Sweep before signing, not after. A sweep that runs after a step that can fail is a sweep
///    that never runs on the machine accumulating the files.
/// 3. Sign a COPY. `codesign` writes in place, a running server holds the pin open for execution,
///    and an in-place sign therefore fails `ETXTBSY` exactly when a session is up - which is every
///    time somebody would want to run this.
/// 4. Verify the copy twice before it is allowed near the pinned path. A signature that did not
///    take leaves the working pin untouched instead of replacing it with a broken one.
/// 5. `rename(2)` last, which is atomic and cannot fail against a running server.
/// 6. Re-stamp the source hash, or the next `session up` calls the signed pin stale and copies
///    over the signature within the minute.
///
/// A failure anywhere is a `Needs you` naming the recovery, never a fatal error: doctor has other
/// checks to make and a machine that cannot sign is still a machine worth reporting on.
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

    if let PinSignature::Anchored {
        identifier,
        designated,
    } = &signature
    {
        findings.push(
            Finding::ok(
                "signing",
                format!("{} is signed as {}", pin_display, identifier),
            )
            .note(designated.clone())
            .note("the requirement names no code hash, so a rebuild keeps every grant"),
        );
        return SigningRun { findings };
    }

    let unsigned = signature == PinSignature::Unsigned;
    if !mode.sign {
        findings.push(
            Finding::needs_you(
                "signing",
                if unsigned {
                    format!("{} is not signed", pin_display)
                } else {
                    format!("{}'s requirement names a code hash", pin_display)
                },
            )
            .note("every grant it holds is voided by the next build")
            .note("--sign lets doctor fix this; it was turned off for this run"),
        );
        return SigningRun { findings };
    }

    let mut rung = choose_rung(&find_identities(commander));

    // The third rung is not one the keychain offers - it is one we make. Only when the first two
    // are absent, only when doctor is allowed to act, and only once in the life of the machine:
    // `ensure_self_signed` re-imports an existing bundle rather than minting a second certificate.
    if rung.is_none() && mode.fix {
        match ensure_self_signed(
            commander,
            &context.signing_dir,
            &context.keychain,
            context.keychain_password.as_deref(),
        ) {
            Ok(minted) => {
                findings.extend(minted);
                findings.extend(back_up_identity(context));
                rung = choose_rung(&find_identities(commander));
            },
            Err(reason) => {
                findings.push(
                    Finding::needs_you("signing", reason)
                        .note("nothing was signed; the pinned copy is untouched"),
                );
                findings.push(xcode_steps(&pin_display));
                return SigningRun { findings };
            },
        }
    }

    let Some(rung) = rung else {
        findings.push(xcode_steps(&pin_display));
        return SigningRun { findings };
    };

    if !mode.fix {
        findings.push(Finding::ok(
            "signing",
            mode.describe(&format!("sign {} with {}", pin_display, rung.description())),
        ));
        return SigningRun { findings };
    }

    findings.extend(perform_signing(commander, pin, &rung));
    SigningRun { findings }
}

/// Everything the ladder needs that only the platform can name.
///
/// Carried in rather than derived here so that this whole file stays testable on a machine with no
/// keychain: a test builds one of these over a temp directory and drives the same code the Mac
/// runs.
#[derive(Debug, Clone)]
pub struct SigningContext {
    pub signing_dir: SigningDir,
    /// The keychain to import into - the user's default, which is where `codesign` looks.
    pub keychain: String,
    /// `ZELLIJ_KEYCHAIN_PASSWORD`, for a run over SSH where no dialog can be answered.
    pub keychain_password: Option<String>,
    /// Where to keep a second copy of the minted identity. zellij's own resolved config directory,
    /// so it lands wherever `ZELLIJ_CONFIG_DIR` or XDG says the user's config lives.
    pub backup_dir: Option<PathBuf>,
}

/// What the keychain will offer, or nothing if it cannot be asked.
fn find_identities(commander: &dyn Commander) -> Vec<Identity> {
    match commander.run(
        "security",
        &["find-identity", "-v", "-p", "codesigning"],
        None,
    ) {
        Ok(output) => parse_identities(&output.stdout),
        Err(_) => Vec::new(),
    }
}

/// Keep a second copy of the minted identity where the user's other zellij files are.
///
/// The `.p12` is the only way back if the keychain loses the certificate, and the signing
/// directory it lives in is not somewhere anyone thinks to back up. Copied silently and reported
/// as a note rather than announced: it is a private key, and the one thing worth saying about it
/// is where it now is.
fn back_up_identity(context: &SigningContext) -> Option<Finding> {
    let backup_dir = context.backup_dir.as_ref()?;
    let source = context.signing_dir.identity_bundle();
    let target = backup_dir.join("zellij-signing-id.p12");
    std::fs::create_dir_all(backup_dir).ok()?;
    std::fs::copy(&source, &target).ok()?;
    Some(
        Finding::changed(
            "signing",
            format!("kept a copy of the identity at {}", target.display()),
        )
        .note("losing both copies means minting a second certificate, which voids every grant"),
    )
}

/// Steps 3 through 6, once a certificate has been chosen.
fn perform_signing(commander: &dyn Commander, pin: &Path, rung: &Rung) -> Vec<Finding> {
    let mut findings = Vec::new();
    let pin_display = pin.display().to_string();
    let directory = pin.parent().unwrap_or_else(|| Path::new("."));

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

    let temporary = directory.join(format!("{}{}.tmp", sign_temp_prefix(), std::process::id()));
    let temporary_display = temporary.display().to_string();
    if let Err(error) = std::fs::copy(pin, &temporary) {
        let _ = std::fs::remove_file(&temporary);
        findings.push(Finding::needs_you(
            "signing",
            format!("could not copy {} to sign it: {}", pin_display, error),
        ));
        return findings;
    }

    let requirement = requirement_for(rung);
    let mut timestamped = rung.can_timestamp();
    let mut signed = run_codesign(
        commander,
        rung,
        requirement.as_deref(),
        timestamped,
        &temporary_display,
    );
    if !signed.0 && timestamped {
        // Apple's timestamp server needs a real chain, and refuses one we minted. Losing the
        // timestamp costs a signature nothing it had: our certificate outlives the machine.
        timestamped = false;
        signed = run_codesign(
            commander,
            rung,
            requirement.as_deref(),
            false,
            &temporary_display,
        );
    }
    if !signed.0 {
        let _ = std::fs::remove_file(&temporary);
        findings.push(
            Finding::needs_you(
                "signing",
                format!("codesign refused to sign with {}", rung.description()),
            )
            .note(signed.1)
            .note("the pinned copy is untouched"),
        );
        return findings;
    }

    if let Err(reason) = verify_signature(commander, &temporary_display) {
        let _ = std::fs::remove_file(&temporary);
        findings.push(
            Finding::needs_you(
                "signing",
                format!("the new signature did not hold: {}", reason),
            )
            .note("the pinned copy is untouched"),
        );
        return findings;
    }

    if let Err(error) = std::fs::rename(&temporary, pin) {
        let _ = std::fs::remove_file(&temporary);
        findings.push(Finding::needs_you(
            "signing",
            format!(
                "could not put the signed copy at {}: {}",
                pin_display, error
            ),
        ));
        return findings;
    }

    findings.push(
        Finding::changed(
            "signing",
            format!(
                "signed {} with {}{}",
                pin_display,
                rung.description(),
                if timestamped { ", timestamped" } else { "" }
            ),
        )
        .note(format!("identifier {}", PIN_IDENTIFIER)),
    );
    findings.push(follow_up(&pin_display));
    findings
}

/// Run `codesign`, reporting whether it worked and what it said if it did not.
fn run_codesign(
    commander: &dyn Commander,
    rung: &Rung,
    requirement: Option<&str>,
    timestamp: bool,
    target: &str,
) -> (bool, String) {
    let owned = sign_arguments(rung, requirement, timestamp, target);
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    match commander.run("codesign", &args, None) {
        Ok(output) if output.success => (true, String::new()),
        Ok(output) => (false, output.stderr.trim().to_owned()),
        Err(reason) => (false, reason),
    }
}

/// Both halves of "did that take": the requirement no longer names a code hash, and the signature
/// itself passes.
///
/// Two questions and not one, because they fail apart. A signature can verify and still leave a
/// requirement anchored on the code hash - which is a run that reported success and fixed nothing.
fn verify_signature(commander: &dyn Commander, target: &str) -> Result<(), String> {
    let described = commander
        .run("codesign", &["-d", "--verbose=2", "-r-", target], None)
        .map_err(|reason| reason)?;
    match read_signature(&described.combined()) {
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
    match commander.run("codesign", &["-v", target], None) {
        Ok(output) if output.success => Ok(()),
        Ok(output) => Err(output.stderr.trim().to_owned()),
        Err(reason) => Err(reason),
    }
}

/// What to do next, in the order that makes it one pass instead of two.
///
/// Re-granting FIRST and restarting SECOND is the whole of the advice. The grants are recorded
/// against the pin's path and the signature it now carries; a server started before they are
/// re-granted comes up not holding them, and the user ends up restarting twice.
fn follow_up(pin: &str) -> Finding {
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
}

/// The last rung: no certificate, and nothing doctor may do about it.
///
/// An ad-hoc signature is NOT the fallback. It anchors the requirement on the code hash, which is
/// the state this whole file exists to leave, so signing ad-hoc would report a fix and deliver the
/// fault under a new name.
fn xcode_steps(pin: &str) -> Finding {
    Finding::needs_you("signing", format!("no signing certificate for {}", pin))
        .note("without one, every grant this path holds is voided by the next build.")
        .note("Any ONE of these gives doctor something to sign with:")
        .note("  - sign in to Xcode with an Apple ID: Xcode > Settings > Accounts,")
        .note("    then Manage Certificates > + > Apple Development")
        .note("  - a Developer ID Application certificate, if the account has one")
        .note("  - let doctor mint one of its own; it needs `openssl` and the login keychain")
        .note("Nothing is signed ad-hoc: that anchors on the code hash, which is the fault.")
}

/// Mint the certificate of last resort, ONCE.
///
/// Never twice. The designated requirement of a self-signed signature is a hash of the
/// CERTIFICATE, so a second certificate is a second requirement and every grant recorded against
/// the first stops applying. `id.p12` is therefore not a convenience: it is the record that this
/// machine already has one, and a keychain that has lost the certificate is re-imported from it
/// rather than given a new one.
///
/// The extensions are alt-tab-macos's, which is the shape Apple's own tools accept for code
/// signing: the code-signing EKU, marked critical, plus Apple's own code-signing OID. `-addext`
/// is not used - macOS ships LibreSSL as `/usr/bin/openssl` and it has no such flag - so the
/// extensions go through a config file, which every version understands.
pub fn openssl_config() -> String {
    format!(
        "[ req ]\n\
         distinguished_name = dn\n\
         x509_extensions = ext\n\
         prompt = no\n\
         \n\
         [ dn ]\n\
         CN = {}\n\
         \n\
         [ ext ]\n\
         basicConstraints = critical,CA:false\n\
         keyUsage = critical,digitalSignature\n\
         extendedKeyUsage = critical,1.3.6.1.5.5.7.3.3\n\
         1.2.840.113635.100.6.1.14 = critical,DER:05:00\n",
        SELF_SIGNED_COMMON_NAME
    )
}

/// Where the minted certificate and its key live, and what they are called.
#[derive(Debug, Clone)]
pub struct SigningDir {
    pub root: PathBuf,
}

impl SigningDir {
    pub fn new(root: PathBuf) -> Self {
        SigningDir { root }
    }

    /// The certificate and key together, which is the file that must never be lost: it is the
    /// proof this machine already minted one, and the only way back if the keychain forgets.
    pub fn identity_bundle(&self) -> PathBuf {
        self.root.join("id.p12")
    }

    pub fn certificate(&self) -> PathBuf {
        self.root.join("cert.pem")
    }

    pub fn private_key(&self) -> PathBuf {
        self.root.join("key.pem")
    }

    pub fn openssl_config_file(&self) -> PathBuf {
        self.root.join("self-signed.cnf")
    }
}

/// Get a certificate of our own into the keychain, minting one only if this machine has never had
/// one.
///
/// Two paths in and they are deliberately different. With an `id.p12` on disk this machine has
/// already minted its certificate and the keychain has merely lost it, so the bundle is IMPORTED -
/// same certificate, same hash, same requirement, every grant intact. With no bundle, a new
/// certificate is minted and the grants start from nothing, which is why that path is taken only
/// once in the life of the machine.
pub fn ensure_self_signed(
    commander: &dyn Commander,
    dir: &SigningDir,
    keychain: &str,
    keychain_password: Option<&str>,
) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    std::fs::create_dir_all(&dir.root)
        .map_err(|e| format!("could not create {}: {}", dir.root.display(), e))?;

    let bundle = dir.identity_bundle();
    if !bundle.exists() {
        mint_self_signed(commander, dir)?;
        findings.push(
            Finding::changed(
                "signing",
                format!(
                    "minted a certificate of our own, kept at {}",
                    bundle.display()
                ),
            )
            .note("it is never minted again: its hash IS the requirement every grant records"),
        );
    } else {
        findings.push(Finding::ok(
            "signing",
            format!(
                "{} already holds this machine's certificate; re-importing it",
                bundle.display()
            ),
        ));
    }

    import_identity(commander, &bundle, keychain, keychain_password)?;
    Ok(findings)
}

/// Write the config, the key and the certificate, and bundle the last two together.
fn mint_self_signed(commander: &dyn Commander, dir: &SigningDir) -> Result<(), String> {
    let config = dir.openssl_config_file();
    std::fs::write(&config, openssl_config())
        .map_err(|e| format!("could not write {}: {}", config.display(), e))?;

    let certificate = dir.certificate().display().to_string();
    let key = dir.private_key().display().to_string();
    let output = commander
        .run(
            "openssl",
            &[
                "req",
                "-x509",
                "-newkey",
                "rsa:2048",
                "-nodes",
                "-days",
                SELF_SIGNED_DAYS,
                "-config",
                &config.display().to_string(),
                "-keyout",
                &key,
                "-out",
                &certificate,
            ],
            None,
        )
        .map_err(|reason| format!("could not run openssl: {}", reason))?;
    if !output.success {
        return Err(format!(
            "openssl could not make a certificate: {}",
            output.stderr.trim()
        ));
    }

    let bundle = dir.identity_bundle().display().to_string();
    let output = commander
        .run(
            "openssl",
            &[
                "pkcs12",
                "-export",
                "-inkey",
                &key,
                "-in",
                &certificate,
                "-out",
                &bundle,
                "-name",
                SELF_SIGNED_COMMON_NAME,
                // no passphrase: the file's protection is the directory it sits in, and a
                // passphrase nobody can type is a passphrase that loses the certificate
                "-passout",
                "pass:",
            ],
            None,
        )
        .map_err(|reason| format!("could not run openssl: {}", reason))?;
    if !output.success {
        return Err(format!(
            "openssl could not bundle the certificate: {}",
            output.stderr.trim()
        ));
    }
    Ok(())
}

/// Put the bundle in the keychain and let `codesign` reach the key.
///
/// `-T /usr/bin/codesign` names the one program allowed to use it, rather than `-A`, which would
/// let anything on the machine sign with this machine's identity.
///
/// `set-key-partition-list` is the other half and it is not optional: without it every signing run
/// raises a GUI dialog for the key's ACL. NO TRUSTED ROOT IS ADDED anywhere here, deliberately -
/// requirement evaluation does not consult trust unless the requirement says `trusted`, and ours
/// never does. What signing needs is access to the key, which is exactly what this grants and
/// nothing more.
fn import_identity(
    commander: &dyn Commander,
    bundle: &Path,
    keychain: &str,
    keychain_password: Option<&str>,
) -> Result<(), String> {
    let bundle = bundle.display().to_string();
    let output = commander
        .run(
            "security",
            &[
                "import",
                &bundle,
                "-k",
                keychain,
                "-P",
                "",
                "-T",
                "/usr/bin/codesign",
            ],
            None,
        )
        .map_err(|reason| format!("could not run security: {}", reason))?;
    // an identity already in the keychain is the ordinary case on every run after the first
    if !output.success && !output.stderr.contains("already exists") {
        return Err(format!(
            "could not import the certificate: {}",
            output.stderr.trim()
        ));
    }

    let mut args = vec![
        String::from("set-key-partition-list"),
        String::from("-S"),
        String::from("apple-tool:,apple:,codesign:"),
        String::from("-s"),
    ];
    if let Some(password) = keychain_password {
        // ZELLIJ_KEYCHAIN_PASSWORD, and only over SSH: with no window server there is no dialog to
        // answer, so a run that would not prompt is a run that hangs. It goes in argv because
        // `security` reads it nowhere else, which puts it in this machine's process table for the
        // life of one command - the reason this is an escape hatch and not the default path.
        args.push(String::from("-k"));
        args.push(password.to_owned());
    }
    args.push(keychain.to_owned());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let output = commander
        .run("security", &borrowed, None)
        .map_err(|reason| format!("could not run security: {}", reason))?;
    if !output.success {
        return Err(format!(
            "could not let codesign reach the key: {}. Over SSH there is no dialog to answer; \
             set ZELLIJ_KEYCHAIN_PASSWORD or run this from a terminal on the machine",
            output.stderr.trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_doctor::{recorded, recorded_failure, RecordedCommander, Status};

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

    /// Recorded from `security find-identity -v -p codesigning` on a machine with both.
    const TWO_IDENTITIES: &str = "\
  1) A1B2C3D4E5F60718293A4B5C6D7E8F9001122334 \"Apple Development: someone@example.com (F6G7H8I9J0)\"
  2) 0011223344556677889900AABBCCDDEEFF001122 \"Developer ID Application: Someone (A1B2C3D4E5)\"
     2 valid identities found
";

    const ONLY_OURS: &str = "\
  1) 7F8C0B1A2D3E4F5061728394A5B6C7D8E9F00112 \"zellij self-signed code signing\"
     1 valid identities found
";

    const NO_IDENTITIES: &str = "     0 valid identities found\n";

    const FIND_IDENTITY: &str = "security find-identity -v -p codesigning";

    /// A context over a scratch directory, so a test drives the same code the Mac runs without
    /// going near a real keychain.
    fn context(root: &Path) -> SigningContext {
        SigningContext {
            signing_dir: SigningDir::new(root.join("signing")),
            keychain: String::from("login.keychain-db"),
            keychain_password: None,
            backup_dir: None,
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

    #[test]
    fn every_identity_on_the_keychain_is_read_and_the_summary_line_is_not() {
        let identities = parse_identities(TWO_IDENTITIES);
        assert_eq!(identities.len(), 2);
        assert_eq!(
            identities[0].hash,
            "A1B2C3D4E5F60718293A4B5C6D7E8F9001122334"
        );
        assert_eq!(
            identities[1].name,
            "Developer ID Application: Someone (A1B2C3D4E5)"
        );
    }

    #[test]
    fn a_developer_id_beats_an_apple_development_certificate_whatever_the_order() {
        let rung = choose_rung(&parse_identities(TWO_IDENTITIES)).unwrap();
        assert!(matches!(rung, Rung::DeveloperId(_)));
    }

    #[test]
    fn ours_is_the_rung_a_machine_with_no_apple_account_lands_on() {
        let rung = choose_rung(&parse_identities(ONLY_OURS)).unwrap();
        assert!(matches!(rung, Rung::SelfSigned(_)));
    }

    #[test]
    fn a_keychain_with_nothing_in_it_offers_no_rung_rather_than_an_ad_hoc_one() {
        assert_eq!(choose_rung(&parse_identities(NO_IDENTITIES)), None);
    }

    #[test]
    fn the_team_id_comes_off_the_name_and_the_email_does_not() {
        assert_eq!(
            team_id("Apple Development: someone@example.com (F6G7H8I9J0)").as_deref(),
            Some("F6G7H8I9J0")
        );
        assert_eq!(team_id("zellij self-signed code signing"), None);
    }

    /// The CN carries an email and changes on reissue; the OU is the team id and does not. A
    /// requirement anchored on the CN is a grant that expires with the certificate.
    #[test]
    fn apple_development_is_anchored_on_the_team_id_and_never_on_the_name() {
        let rung = choose_rung(&parse_identities(
            "  1) AAAA \"Apple Development: someone@example.com (F6G7H8I9J0)\"\n",
        ))
        .unwrap();
        let requirement = requirement_for(&rung).unwrap();
        assert!(requirement.contains("subject.OU"), "{}", requirement);
        assert!(!requirement.contains("subject.CN"), "{}", requirement);
        assert!(
            !requirement.contains("someone@example.com"),
            "{}",
            requirement
        );
    }

    /// codesign already anchors a Developer ID on the team id, and a self-signed certificate on
    /// its own hash. Writing a requirement over either would replace a good anchor with ours.
    #[test]
    fn the_other_two_rungs_keep_the_requirement_codesign_derives() {
        for output in [TWO_IDENTITIES, ONLY_OURS] {
            let rung = choose_rung(&parse_identities(output)).unwrap();
            assert_eq!(requirement_for(&rung), None);
        }
    }

    #[test]
    fn only_a_real_chain_can_be_timestamped() {
        assert!(choose_rung(&parse_identities(TWO_IDENTITIES))
            .unwrap()
            .can_timestamp());
        assert!(!choose_rung(&parse_identities(ONLY_OURS))
            .unwrap()
            .can_timestamp());
    }

    /// `-f` or a second run refuses a file that already carries a signature; `--identifier` or
    /// codesign derives one from the file name, and renaming the pin would then change the
    /// requirement every grant records.
    #[test]
    fn a_signature_always_forces_and_always_names_its_identifier() {
        let rung = choose_rung(&parse_identities(ONLY_OURS)).unwrap();
        let args = sign_arguments(&rung, None, false, "/tmp/pin");
        assert!(args.contains(&String::from("-f")), "{:?}", args);
        assert!(args.contains(&String::from("--identifier")), "{:?}", args);
        assert!(args.contains(&String::from(PIN_IDENTIFIER)), "{:?}", args);
        assert!(!args.contains(&String::from("--timestamp")), "{:?}", args);
    }

    #[test]
    fn an_already_anchored_pin_is_left_alone_and_never_signed_again() {
        let commander = RecordedCommander::new(&[(
            "codesign -d --verbose=2 -r- /tmp/pin",
            recorded(DEVELOPER_ID),
        )]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            Path::new("/tmp/pin"),
            DoctorMode::default(),
            &context(scratch.path()),
        );
        assert_eq!(run.findings[0].status, Status::AlreadyCorrect);
        assert!(!commander.called_with("-s "), "{:?}", commander.calls());
    }

    #[test]
    fn a_machine_with_no_certificate_is_told_about_xcode_and_nothing_is_signed_ad_hoc() {
        let commander = RecordedCommander::new(&[
            ("codesign -d --verbose=2 -r- /tmp/pin", recorded(AD_HOC)),
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
            // no openssl and no keychain here, so the mint fails and the ladder runs out
            (
                "openssl req",
                recorded_failure("no openssl on this machine"),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            Path::new("/tmp/pin"),
            DoctorMode::default(),
            &context(scratch.path()),
        );
        let last = run.findings.last().unwrap();
        assert_eq!(last.status, Status::NeedsYou);
        assert!(last.notes.iter().any(|note| note.contains("Xcode")));
        assert!(!commander.called_with("-s -"), "{:?}", commander.calls());
    }

    #[test]
    fn no_sign_reports_the_fault_and_touches_nothing() {
        let commander =
            RecordedCommander::new(&[("codesign -d --verbose=2 -r- /tmp/pin", recorded(AD_HOC))]);
        let run = sign_pin(
            &commander,
            Path::new("/tmp/pin"),
            DoctorMode {
                sign: false,
                ..DoctorMode::default()
            },
            &context(tempfile::tempdir().unwrap().path()),
        );
        assert_eq!(run.findings[0].status, Status::NeedsYou);
        assert_eq!(commander.calls().len(), 1);
    }

    #[test]
    fn a_dry_run_names_the_rung_it_would_have_used_and_signs_nothing() {
        let commander = RecordedCommander::new(&[
            ("codesign -d --verbose=2 -r- /tmp/pin", recorded(AD_HOC)),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            Path::new("/tmp/pin"),
            DoctorMode {
                fix: false,
                dry_run: true,
                sign: true,
            },
            &context(scratch.path()),
        );
        assert!(
            run.findings[0].message.starts_with("would sign"),
            "{:?}",
            run.findings[0]
        );
        assert!(
            !commander.called_with("codesign -s "),
            "{:?}",
            commander.calls()
        );
    }

    #[test]
    fn a_signing_run_copies_verifies_and_only_then_renames() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"a pretend 46 MB binary").unwrap();
        let pin_display = pin.display().to_string();
        // a stale temp from a run that did not finish, which the sweep has to take
        let stale = directory.path().join(".zellij.sign.999.tmp");
        std::fs::write(&stale, b"leftover").unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin_display).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(DEVELOPER_ID)),
            ("codesign -v ", recorded("")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );

        assert!(!stale.exists(), "the stale temp was not swept");
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.status == Status::Changed
                    && finding.message.contains("signed")),
            "{:?}",
            run.findings
        );
        // the pin is still there, under its own name, holding what the temp held
        assert!(pin.exists());
        assert_eq!(
            std::fs::read(&pin).unwrap(),
            b"a pretend 46 MB binary".to_vec()
        );
        // nothing was left behind
        assert!(!directory
            .path()
            .join(format!(".zellij.sign.{}.tmp", std::process::id()))
            .exists());

        let calls = commander.calls();
        let signed = commander.position_of("codesign -s ").unwrap();
        let verified = calls
            .iter()
            .enumerate()
            .filter(|(_, call)| call.starts_with("codesign -v "))
            .map(|(index, _)| index)
            .next()
            .unwrap();
        assert!(signed < verified, "verified before it signed: {:?}", calls);
    }

    #[test]
    fn a_signature_that_does_not_take_leaves_the_working_pin_alone() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let pin_display = pin.display().to_string();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin_display).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded_failure("errSecInternalComponent")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );

        assert!(run
            .findings
            .iter()
            .any(|finding| finding.status == Status::NeedsYou));
        assert_eq!(std::fs::read(&pin).unwrap(), b"the working copy".to_vec());
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| entry.file_name() == "zellij"),
            "a temp file was left behind"
        );
    }

    /// A signature can verify while its requirement still names the code hash. That is a run that
    /// reported success and fixed nothing, so the rename must not happen.
    #[test]
    fn a_signature_that_verifies_but_stays_code_hashed_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let pin_display = pin.display().to_string();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin_display).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(AD_HOC)),
            ("codesign -v ", recorded("")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );

        assert!(run
            .findings
            .iter()
            .any(|finding| finding.status == Status::NeedsYou));
        assert_eq!(std::fs::read(&pin).unwrap(), b"the working copy".to_vec());
    }

    /// Apple's timestamp server needs a real chain and refuses ours. Losing the timestamp is not
    /// a failure; giving up on the signature would be.
    #[test]
    fn a_refused_timestamp_falls_back_rather_than_failing() {
        let rung = choose_rung(&parse_identities(TWO_IDENTITIES)).unwrap();
        let timestamped = sign_arguments(&rung, None, true, "/tmp/pin");
        let plain = sign_arguments(&rung, None, false, "/tmp/pin");
        assert!(timestamped.contains(&String::from("--timestamp")));
        assert!(!plain.contains(&String::from("--timestamp")));
    }

    #[test]
    fn the_openssl_config_carries_the_code_signing_extensions() {
        let config = openssl_config();
        assert!(config.contains("1.3.6.1.5.5.7.3.3"), "{}", config);
        assert!(config.contains("1.2.840.113635.100.6.1.14"), "{}", config);
        assert!(config.contains(SELF_SIGNED_COMMON_NAME), "{}", config);
    }

    /// The bundle is the record that this machine already minted a certificate. A run that found
    /// one and minted another would give the machine a second requirement and void every grant
    /// recorded against the first.
    #[test]
    fn an_existing_bundle_is_re_imported_and_never_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"the one certificate").unwrap();

        let commander = RecordedCommander::new(&[
            ("security import", recorded("")),
            ("security set-key-partition-list", recorded("")),
        ]);
        let findings = ensure_self_signed(&commander, &dir, "login.keychain-db", None).unwrap();

        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
        assert!(commander.called_with("security import"));
        assert_eq!(
            std::fs::read(dir.identity_bundle()).unwrap(),
            b"the one certificate".to_vec()
        );
        assert!(findings
            .iter()
            .all(|finding| finding.status != Status::Changed));
    }

    /// Requirement evaluation does not consult trust unless the requirement says `trusted`, and
    /// ours never does - so a trusted root would buy nothing and cost the user a machine that
    /// trusts a certificate zellij made.
    #[test]
    fn nothing_ever_adds_a_trusted_root() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"the one certificate").unwrap();
        let commander = RecordedCommander::new(&[
            ("security import", recorded("")),
            ("security set-key-partition-list", recorded("")),
        ]);
        ensure_self_signed(&commander, &dir, "login.keychain-db", None).unwrap();
        assert!(
            !commander.called_with("add-trusted-cert"),
            "{:?}",
            commander.calls()
        );
    }

    /// Over SSH there is no dialog to answer, so the password has to come from somewhere or the
    /// run hangs. It is passed only when it was given.
    #[test]
    fn the_keychain_password_is_passed_only_when_there_is_one() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"x").unwrap();
        let commander = RecordedCommander::new(&[
            ("security import", recorded("")),
            ("security set-key-partition-list", recorded("")),
        ]);
        ensure_self_signed(&commander, &dir, "login.keychain-db", Some("hunter2")).unwrap();
        assert!(
            commander.called_with("-k hunter2"),
            "{:?}",
            commander.calls()
        );
    }
}
