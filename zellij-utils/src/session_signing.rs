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

/// The passphrase on the bundle we mint. It is a literal on purpose, and it is not a secret.
///
/// **An empty passphrase does not work, and that is a format problem rather than a policy one.**
/// Apple's importer cannot verify the MAC of a PKCS#12 written with no password - OpenSSL and
/// Apple disagree about how an empty password is encoded before it is hashed - and it reports that
/// as the one thing that is not wrong:
///
/// ```text
/// security: SecKeychainItemImport: MAC verification failed during PKCS12 import (wrong password?)
/// ```
///
/// Proven on a real Mac at 0.45.0-nkmk.7 by changing nothing else: same key, same certificate, same
/// `-macalg sha1 -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES`, same LibreSSL. With `-passout
/// pass:` the import fails as above; with `-passout pass:zellij` and `-P zellij` it reports
/// `1 identity imported.`
///
/// So the bundle needs A passphrase, and a passphrase written down beside the file it opens
/// protects nothing - which is the point. The protection is the 0700 directory and the 0600 file,
/// exactly as before. A passphrase the user would have to remember would instead be a way to lose
/// the one certificate this machine may ever have.
pub const IDENTITY_PASSPHRASE: &str = "zellij";

/// The same passphrase in the spelling `openssl pkcs12 -passout` wants.
const PKCS12_PASSOUT: &str = "pass:zellij";

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
///
/// The name is taken between the FIRST and LAST quote rather than by stripping one off each end,
/// because `find-identity` without `-v` writes the reason an identity is not valid after the
/// closing quote:
///
/// ```text
///   1) 5D3A... "zellij self-signed code signing" (CSSMERR_TP_NOT_TRUSTED)
/// ```
///
/// A parser that required the line to END in a quote dropped exactly that line, which is the one
/// [`find_identities`] reads the untrusted listing to find.
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
            let name = name.trim();
            let opened = name.find('"')?;
            let closed = name.rfind('"')?;
            let name = name.get(opened + 1..closed)?;
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
    /// An Apple Development certificate, with the team id read off the CERTIFICATE - never off the
    /// name. The requirement has to be written by hand for this one - see [`requirement_for`].
    ///
    /// `team` is `None` until [`sign_down_the_ladder`] has asked the keychain for the certificate,
    /// and stays `None` on a machine where that question cannot be answered. A rung with no team
    /// writes no requirement and takes the one `codesign` derives, which is worse but is never
    /// wrong - see [`requirement_for`].
    AppleDevelopment {
        identity: Identity,
        team: Option<String>,
    },
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

/// The team id out of a certificate's subject line, which is the `OU`.
///
/// **The parenthesised code in the NAME is not the team id, and reading it there was a bug that
/// shipped twice.** On a Developer ID Application certificate the two happen to be the same string,
/// which is what let the mistake survive; on an Apple Development certificate they are not. A real
/// one, from a machine at 0.45.0-nkmk.7:
///
/// ```text
/// UID=7472L5G3Y6/CN=Apple Development: someone (DY7JA3K8QZ)/OU=U2VEDWFUF3/O=Someone/C=US
/// ```
///
/// The CN parenthetical is the per-developer id, the OU is the team, and a requirement written
/// against the first is a requirement the signed binary does not satisfy: `codesign` signs, and
/// then `codesign --verify --verbose=2` says `does not satisfy its designated Requirement`. So the
/// team id is read HERE, from the certificate, and nowhere else.
///
/// Both spellings of a subject line are accepted because both are on these machines. LibreSSL -
/// which is `/usr/bin/openssl` on macOS - writes `/OU=VALUE/`, and OpenSSL 3 - which a Homebrew
/// install puts first on `PATH` - writes `OU = VALUE,`. Matching the key and then taking the
/// alphanumeric run after the `=` covers both without knowing which ran.
pub fn team_id_from_subject(subject: &str) -> Option<String> {
    let bytes = subject.as_bytes();
    let mut from = 0;
    while let Some(found) = subject.get(from..)?.find("OU") {
        let at = from + found;
        from = at + 2;
        // `OU` has to be a key of its own and not the tail of a longer one, or an attribute such
        // as `businessCategoryOU` would answer for the team
        if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'.') {
            continue;
        }
        let Some(value) = subject[at + 2..].trim_start().strip_prefix('=') else {
            continue;
        };
        let value: String = value
            .trim_start()
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

/// Ask the keychain for a certificate and read the team id out of it.
///
/// Two commands and a pipe: `security` writes the certificate as PEM, `openssl` turns it into a
/// subject line. Both ship with macOS, and `openssl` is already needed by the rung below this one.
///
/// `None` on anything going wrong, and that is deliberate rather than an error path: a rung with no
/// team id still signs, it just takes the requirement `codesign` derives instead of ours. Failing
/// the rung over an unreadable certificate would be strictly worse than signing it slightly less
/// well.
fn team_id_from_keychain(commander: &dyn Commander, keychain: &str, name: &str) -> Option<String> {
    let pem = commander
        .run(
            "security",
            &["find-certificate", "-c", name, "-p", keychain],
            None,
        )
        .ok()
        .filter(|output| output.success)?;
    let subject = commander
        .run(
            "openssl",
            &["x509", "-noout", "-subject"],
            Some(&pem.stdout),
        )
        .ok()
        .filter(|output| output.success)?;
    team_id_from_subject(&subject.stdout)
}

/// The first rung of the ladder this keychain can reach.
///
/// Order is the whole design. A Developer ID signature is accepted anywhere and survives its own
/// certificate's expiry; an Apple Development one is good on the machines signed into that Apple
/// ID; ours is good only here, and is the rung a corporate Mac with no developer account lands on.
/// Nothing below the third rung: an ad-hoc signature anchors on the code hash, which is the state
/// signing exists to leave.
pub fn choose_rung(identities: &[Identity]) -> Option<Rung> {
    rung_ladder(identities).into_iter().next()
}

/// Every rung this keychain can reach, best first.
///
/// [`choose_rung`] is the head of this list, and the tail is what a refusal falls back to. A
/// certificate the keychain OFFERS is not a certificate that SIGNS: `codesign` can refuse the
/// requirement, the keychain can decline to release the key, and Apple's timestamp server can be
/// unreachable. A run that took only the head reported `Needs you` and left the pin ad-hoc-signed
/// with a working rung standing unused beneath it.
pub fn rung_ladder(identities: &[Identity]) -> Vec<Rung> {
    let mut ladder = Vec::new();
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.name.starts_with("Developer ID Application:"))
    {
        ladder.push(Rung::DeveloperId(identity.clone()));
    }
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.name.starts_with("Apple Development:"))
    {
        // the team id is NOT read here. It comes off the certificate, which needs the keychain,
        // and this function stays pure so that every rung-order test can drive it with a list.
        ladder.push(Rung::AppleDevelopment {
            identity: identity.clone(),
            team: None,
        });
    }
    if let Some(identity) = identities
        .iter()
        .find(|identity| identity.name.contains(SELF_SIGNED_COMMON_NAME))
    {
        ladder.push(Rung::SelfSigned(identity.clone()));
    }
    ladder
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
/// **The team id must come from [`team_id_from_subject`], never from the identity's name.** Writing
/// the CN's parenthesised code into `leaf[subject.OU]` produces a requirement the freshly signed
/// binary fails, which is a worse outcome than writing none: the signature lands, doctor reports
/// success, and every grant made against it is void. It shipped at 0.45.0-nkmk.7 and is what this
/// `Option` exists to make unrepresentable.
///
/// A rung with **no** team id writes no requirement at all and takes the CN-anchored one `codesign`
/// derives. That is the lesser of the two: it survives every rebuild, which is what grants actually
/// need, and only breaks when the certificate is reissued.
///
/// Developer ID needs nothing written: `codesign` already anchors it on `subject.OU`. Self-signed
/// needs nothing either, and must not be given one - its default requirement names the certificate
/// by hash, which is exactly the anchor that makes it worth having.
///
/// **It is a requirement SET, not a bare expression, and the `designated =>` is load-bearing.**
/// `codesign -r` parses what it is handed as a set of `<tag> => <expression>` pairs, so a text
/// that opens with `identifier` puts a reserved word where a tag belongs and the whole thing is
/// refused before signing starts:
///
/// ```text
/// invalid or corrupted code requirement(s)
/// Requirement syntax error(s): line 1:1: unexpected token: identifier
/// ```
///
/// Seen on a real Mac at 0.45.0-nkmk.6, on the one rung that writes a requirement at all - which
/// is why it survived: the other two rungs pass `None` and never reach this parser. `designated`
/// is also the only tag worth writing, because it is the requirement macOS records a grant
/// against, and it is what [`read_signature`] reads back.
pub fn requirement_for(rung: &Rung) -> Option<String> {
    match rung {
        Rung::AppleDevelopment {
            team: Some(team), ..
        } => Some(format!(
            "designated => identifier \"{}\" and anchor apple generic and certificate \
             leaf[subject.OU] = \"{}\"",
            PIN_IDENTIFIER, team
        )),
        Rung::AppleDevelopment { team: None, .. } | Rung::DeveloperId(_) | Rung::SelfSigned(_) => {
            None
        },
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
        // `-r` takes a FILE unless its value opens with `=`, which makes the rest of it the
        // requirement text itself. One argv of `-r=<text>` hands `codesign` the value `=<text>`,
        // which is that inline form - the text still has to be a requirement SET, see
        // [`requirement_for`].
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

/// What one pass over the pinned copy's signature came to.
pub struct SigningRun {
    pub findings: Vec<Finding>,
}

/// Bring the pinned copy's signature to something that outlives the build.
///
/// The order of the steps is the design and it is worth stating why each one is where it is.
///
/// 1. Read the requirement FIRST, and then VERIFY it. An already-anchored pin must not be signed
///    again: the new signature would carry a new certificate hash and void every grant it
///    currently holds. But "anchored" is a property of the requirement's text and "holds a grant"
///    is a property of the binary satisfying it, and a pin can have the first without the second -
///    so the requirement being anchored is what makes the pin a candidate for being left alone,
///    and passing verification is what actually leaves it alone.
/// 2. Sweep before signing, not after. A sweep that runs after a step that can fail is a sweep
///    that never runs on the machine accumulating the files.
/// 3. Sign a COPY. `codesign` writes in place, a running server holds the pin open for execution,
///    and an in-place sign therefore fails `ETXTBSY` exactly when a session is up - which is every
///    time somebody would want to run this.
/// 4. Verify the copy twice before it is allowed near the pinned path. A signature that did not
///    take leaves the working pin untouched instead of replacing it with a broken one.
/// 5. `rename(2)` last, which is atomic and cannot fail against a running server.
/// 6. Nothing re-stamps, and nothing may be made to. The stamp beside the pin records the hash of
///    the SOURCE binary, which signing does not touch, so it still agrees and the next
///    `session up` leaves the signature alone. A "re-stamp" written here would have to hash the
///    signed pin, and a stamp naming the pin's own bytes is exactly the comparison
///    [`pin_is_stale`](crate::session_lifecycle::install_pinned_exe) exists to avoid: it would
///    call every signed pin stale and copy over the signature within the minute.
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

    // An anchored-LOOKING pin is not a healthy pin, and reading the requirement is not checking it.
    // A pin signed with a requirement the binary does not satisfy reads exactly like a good one -
    // same identifier, same anchored text, no code hash anywhere - and doctor called that state
    // healthy for two releases while `codesign --verify --verbose=2` rejected it. So the pin gets
    // the same verification a freshly signed copy does, and a pin that fails it is signed again.
    let mut broken_anchor = None;
    if let PinSignature::Anchored {
        identifier,
        designated,
    } = &signature
    {
        match verify_signature(commander, &pin_display) {
            Ok(()) => {
                findings.push(
                    Finding::ok(
                        "signing",
                        format!("{} is signed as {}", pin_display, identifier),
                    )
                    .note(designated.clone())
                    .note("the requirement names no code hash, so a rebuild keeps every grant")
                    .note("and the pin satisfies it, so the grants recorded against it still hold"),
                );
                return SigningRun { findings };
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
                    .note(reason.clone())
                    .note("a signature that does not verify holds no grant, whatever it reads as"),
                );
                broken_anchor = Some(reason);
            },
        }
    }

    let unsigned = signature == PinSignature::Unsigned;
    if !mode.sign {
        // a broken anchor has just been reported in full, so this says what is left to say about
        // it and does not state the fault a second time
        findings.push(if broken_anchor.is_some() {
            Finding::needs_you(
                "signing",
                "signing it again is the only recovery, and --sign was turned off for this run",
            )
        } else {
            Finding::needs_you(
                "signing",
                if unsigned {
                    format!("{} is not signed", pin_display)
                } else {
                    format!("{}'s requirement names a code hash", pin_display)
                },
            )
            .note("every grant it holds is voided by the next build")
            .note("--sign lets doctor fix this; it was turned off for this run")
        });
        return SigningRun { findings };
    }

    let mut ladder = rung_ladder(&find_identities(commander));

    // A run that is not acting stops here and says which rung it would have taken - including the
    // one that does not exist yet. Falling through to the Xcode steps would have a dry run report
    // `Needs you` on the machine the real run repairs by itself, which is every machine with no
    // Apple account: the commonest case, and the one where the dry run is read most carefully.
    if ladder.is_empty() && !mode.fix {
        findings.push(
            Finding::changed(
                "signing",
                mode.describe(&format!(
                    "mint a certificate of zellij's own and sign {} with it",
                    pin_display
                )),
            )
            .note("the keychain offers no Apple certificate, so this is the rung the run takes")
            .note("it needs `openssl` and the login keychain, and mints once and never again"),
        );
        return SigningRun { findings };
    }

    // The third rung is not one the keychain offers - it is one we make. Only when the first two
    // are absent, only when doctor is allowed to act, and only once in the life of the machine:
    // `ensure_self_signed` re-imports an existing bundle rather than minting a second certificate.
    if ladder.is_empty() && mode.fix {
        match ensure_self_signed(
            commander,
            &context.signing_dir,
            &context.keychain,
            context.keychain_password.as_deref(),
        ) {
            Ok(minted) => {
                findings.extend(minted);
                findings.extend(back_up_identity(context));
                ladder = rung_ladder(&find_identities(commander));
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

    if ladder.is_empty() {
        // with no rung and no acting, the ladder never reached its third step - so the honest
        // report is what doctor WOULD do, not the "no certificate anywhere" the fix path reaches
        if !mode.fix {
            let bundle = context.signing_dir.identity_bundle();
            findings.push(Finding::ok(
                "signing",
                mode.describe(&format!(
                    "{} a certificate of our own and sign {} with it",
                    if bundle.exists() {
                        format!("re-import {} -", bundle.display())
                    } else {
                        String::from("mint")
                    },
                    pin_display
                )),
            ));
            return SigningRun { findings };
        }
        findings.push(xcode_steps(&pin_display));
        return SigningRun { findings };
    }

    if !mode.fix {
        findings.push(Finding::ok(
            "signing",
            mode.describe(&format!(
                "sign {} with {}",
                pin_display,
                ladder[0].description()
            )),
        ));
        return SigningRun { findings };
    }

    findings.extend(sign_down_the_ladder(commander, pin, context, ladder));
    SigningRun { findings }
}

/// Sign with the best rung that will actually sign, and say which ones would not.
///
/// A refusal walks DOWN the ladder rather than stopping on it. Stopping was the old behaviour and
/// it is the worse of two bad outcomes: `session up` refreshes the pin ad-hoc-signed, doctor is
/// what makes it anchored, and a doctor that gave up on the first refusal left the machine with
/// the exact signature this whole file exists to remove - while a certificate that would have
/// worked sat one rung below.
///
/// **The walk stops at the Apple rungs, and that boundary is the point.** Developer ID and Apple
/// Development are interchangeable to a grant WHEN both name the same team: `codesign` derives
/// `identifier ... and anchor apple generic and certificate leaf[subject.OU] = "TEAM"` for the
/// first, and [`requirement_for`] writes the same text by hand for the second, so falling from one
/// to the other keeps the requirement macOS recorded the grant against. That is the whole reason
/// the second rung writes a requirement at all, and it holds only while the team id is read off
/// the CERTIFICATE - a rung whose certificate could not be read falls back to the CN-anchored
/// requirement `codesign` derives, and that one is not interchangeable with anything. The
/// certificate we mint is not either - its requirement is its own hash - so walking into it would
/// void every grant on the machine.
///
/// That matters because a refusal is not always the certificate's fault. `errSecInternalComponent`,
/// a keychain locked over SSH, a "Deny" on the key-access dialog: each one is transient, and each
/// one would otherwise demote the pin permanently. Permanently, because a self-signed signature IS
/// anchored, so the next doctor run reads the pin as already correct and never climbs back.
///
/// So a machine that holds an Apple certificate and cannot use it gets a `Needs you` naming what
/// each certificate said. It does not get a different requirement behind its back, and it does not
/// get a certificate minted that it would never otherwise have had.
fn sign_down_the_ladder(
    commander: &dyn Commander,
    pin: &Path,
    context: &SigningContext,
    mut ladder: Vec<Rung>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut refusals: Vec<String> = Vec::new();
    let mut asked_for_the_key = false;
    let apple_offered = ladder
        .iter()
        .any(|rung| !matches!(rung, Rung::SelfSigned(_)));
    if apple_offered {
        // the certificate we mint is not a rung below an Apple one. It is the rung a machine with
        // no Apple certificate lands on, and only that.
        ladder.retain(|rung| !matches!(rung, Rung::SelfSigned(_)));
    }
    let mut index = 0;

    while index < ladder.len() {
        // the team id is read off the certificate HERE and not when the ladder was built: it costs
        // two keychain reads, and only the rung actually being attempted should pay for them
        let rung = match ladder[index].clone() {
            Rung::AppleDevelopment { identity, .. } => {
                let team = team_id_from_keychain(commander, &context.keychain, &identity.name);
                Rung::AppleDevelopment { identity, team }
            },
            other => other,
        };
        if matches!(rung, Rung::SelfSigned(_)) && !asked_for_the_key {
            // BEFORE signing, and on every run rather than only the one that minted. The ACL that
            // lets `codesign` reach the key is a property of the keychain, not of the certificate,
            // and it used to be granted only on the minting run - so the very next run found a
            // ready rung, signed with it, and was refused by a key nothing had ever approved.
            // Proven on a real Mac: running the partition list by hand made the identical
            // `codesign` succeed. It is cheap and idempotent when the ACL is already there.
            //
            // Only for our own certificate. An Apple one comes with its own ACL, and re-writing
            // the partition list of a key we did not create is not doctor's business.
            asked_for_the_key = true;
            findings.extend(allow_codesign_to_reach_the_key(
                commander,
                &context.keychain,
                context.keychain_password.as_deref(),
            ));
        }
        let changed_certificate = changes_certificate(
            &rung,
            context.signing_dir.identity_bundle().exists(),
            apple_offered,
            !refusals.is_empty(),
        );
        let attempt = perform_signing(commander, pin, &rung, changed_certificate, &refusals);
        findings.extend(attempt.findings);
        let Some(refusal) = attempt.refusal else {
            return findings;
        };
        if attempt.fatal {
            // the filesystem said no, not the certificate. Another rung would write the same
            // error a second time and still leave the pin as it found it.
            findings
                .push(Finding::needs_you("signing", refusal).note("the pinned copy is untouched"));
            return findings;
        }
        refusals.push(refusal);
        index += 1;
    }

    let mut exhausted = Finding::needs_you(
        "signing",
        format!(
            "{} {} refused to sign {}",
            refusals.len(),
            if refusals.len() == 1 {
                "certificate"
            } else {
                "certificates"
            },
            pin.display()
        ),
    );
    for refusal in refusals {
        exhausted = exhausted.note(refusal);
    }
    exhausted = exhausted.note("the pinned copy is untouched");
    if apple_offered {
        // this machine HAS a certificate, so the Xcode steps would send it after one it already
        // holds. What refuses a certificate it can see is usually the key, not the certificate.
        exhausted = exhausted
            .note("nothing was signed with a different certificate: that would change the")
            .note("requirement macOS recorded, and void every grant this path holds");
    }
    // The key, not the certificate, is what usually refuses - a keychain that will not release it,
    // or a key-access dialog nobody was there to answer. Both remedies are given on EVERY
    // exhausted ladder, including the rung we mint: a certificate doctor made itself is exactly
    // the one whose key has never been approved.
    exhausted = exhausted
        .note("a locked keychain or an unanswered key-access dialog refuses like this")
        .note(KEY_ACCESS_REMEDIES[0])
        .note(KEY_ACCESS_REMEDIES[1])
        .note(KEY_ACCESS_REMEDIES[2]);
    findings.push(exhausted);
    if !apple_offered {
        // an Apple certificate is still a real alternative to one we minted, so the steps stay -
        // but they come AFTER the key-access remedies, which are what usually applies.
        findings.push(xcode_steps(&pin.display().to_string()));
    }
    findings
}

/// Whether signing with `rung` changes WHICH CERTIFICATE the requirement names.
///
/// Not a cosmetic distinction: a changed certificate is a changed requirement, and every grant
/// recorded against the old one stops applying. `follow_up` says so, and a user who is told to
/// re-grant is owed the reason.
///
/// The pin being re-signed is ad-hoc or unsigned - an anchored one is left alone before the ladder
/// is reached - so the previous certificate cannot be read off the pin. Two signals stand in for
/// it: the bundle on disk says this machine has signed with one of ours, and the keychain says it
/// has an Apple one to have used.
fn changes_certificate(
    rung: &Rung,
    ours_on_disk: bool,
    apple_offered: bool,
    fell_from_a_higher_rung: bool,
) -> Option<&'static str> {
    let ours_before = "this machine had signed with a certificate of its own before now, so the \
                       requirement changed with the certificate";
    match rung {
        // An Apple Development certificate whose team id could not be read writes no requirement
        // and takes the CN-anchored one `codesign` derives. That one is not interchangeable with
        // the OU-anchored requirement the rung above it writes, so falling into it changes what
        // macOS recorded - and the user is owed the reason rather than a bare "re-grant".
        Rung::AppleDevelopment { team: None, .. } if fell_from_a_higher_rung => Some(
            "the rung above this one would not sign, and this certificate could not be read for \
             its team id - so the requirement is anchored differently and every grant changes with it",
        ),
        // an Apple certificate on a machine that had minted its own
        Rung::DeveloperId(_) | Rung::AppleDevelopment { .. } => {
            ours_on_disk.then_some(ours_before)
        },
        // ours on a machine that has an Apple certificate. The ladder no longer walks into this
        // rung from an Apple one, so this arm should not be reachable - it is written out rather
        // than assumed away, because the day it becomes reachable is the day it must say so.
        Rung::SelfSigned(_) => apple_offered.then_some(ours_before),
    }
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
    /// `ZELLIJ_KEYCHAIN_PASSWORD`, read from the environment when the user has set it and never
    /// asked for. Not an SSH-only escape hatch: `security` prompts on the controlling terminal
    /// wherever it runs, so a run with no password and no person watching stalls in a pane and
    /// under launchd exactly as it does over SSH.
    pub keychain_password: Option<String>,
    /// Where to keep a second copy of the minted identity. zellij's own resolved config directory,
    /// so it lands wherever `ZELLIJ_CONFIG_DIR` or XDG says the user's config lives.
    pub backup_dir: Option<PathBuf>,
}

/// What the keychain will offer, or nothing if it cannot be asked.
///
/// **Two listings, and the second one is not redundant.** `-v` means "valid only", and validity
/// there is a TRUST decision: a certificate we minted ourselves has no chain to a trusted root, so
/// the login keychain reports it as `(CSSMERR_TP_NOT_TRUSTED)` and `find-identity -v` ends with
/// `0 valid identities found` - on a machine that holds it, has the key, and can sign with it
/// perfectly well. That is not hypothetical: it is what a real Mac did at 0.45.0-nkmk.7, and it
/// would leave doctor minting a certificate it already has, or reporting no rung at all.
///
/// Signing does not need trust and neither does a grant. Requirement evaluation ignores the trust
/// status of the chain unless the requirement says `trusted`, and ours never does - so the fix is
/// to stop asking the question, not to answer it. **NO TRUSTED ROOT IS ADDED.** `add-trusted-cert`
/// would change what Gatekeeper accepts on the whole machine, and needs an administrator, to buy
/// nothing that a grant reads.
///
/// The untrusted listing is filtered to OUR certificate by name. An Apple certificate that the
/// keychain calls invalid is invalid for a reason - expired, revoked, no key - and taking it off
/// this list would put the ladder on a rung that cannot sign.
fn find_identities(commander: &dyn Commander) -> Vec<Identity> {
    let mut identities = match commander.run(
        "security",
        &["find-identity", "-v", "-p", "codesigning"],
        None,
    ) {
        Ok(output) => parse_identities(&output.stdout),
        Err(_) => Vec::new(),
    };
    if identities
        .iter()
        .any(|identity| identity.name.contains(SELF_SIGNED_COMMON_NAME))
    {
        return identities;
    }
    if let Ok(output) = commander.run("security", &["find-identity", "-p", "codesigning"], None) {
        identities.extend(
            parse_identities(&output.stdout)
                .into_iter()
                .find(|identity| identity.name.contains(SELF_SIGNED_COMMON_NAME)),
        );
    }
    identities
}

/// Keep a second copy of the minted identity where the user's other zellij files are.
///
/// The `.p12` is the only way back if the keychain loses the certificate, and the signing
/// directory it lives in is not somewhere anyone thinks to back up. Copied silently and reported
/// as a note rather than announced: it is a private key, and the one thing worth saying about it
/// is where it now is.
///
/// A copy that did not happen is a `Needs you` and never a silence. The certificate has just been
/// minted and cannot be minted again, so "there is a second copy of it" is exactly the kind of
/// thing a user must not be left believing wrongly.
fn back_up_identity(context: &SigningContext) -> Option<Finding> {
    let backup_dir = context.backup_dir.as_ref()?;
    let source = context.signing_dir.identity_bundle();
    let target = backup_dir.join("zellij-signing-id.p12");
    let copied = std::fs::create_dir_all(backup_dir)
        .and_then(|()| std::fs::copy(&source, &target))
        .and_then(|_| restrict(&target, 0o600));
    match copied {
        Ok(()) => Some(
            Finding::changed(
                "signing",
                format!("kept a copy of the identity at {}", target.display()),
            )
            .note("losing both copies means minting a second certificate, which voids every grant"),
        ),
        Err(error) => Some(
            Finding::needs_you(
                "signing",
                format!(
                    "could not copy the identity to {}: {}",
                    target.display(),
                    error
                ),
            )
            .note(format!("it exists only at {}", source.display()))
            .note("copy it somewhere safe by hand: losing it means minting a second")
            .note("certificate, and that voids every grant recorded against the first"),
        ),
    }
}

/// Keep a private key out of every other account on the machine.
///
/// Anything that can read the key can sign as this machine's zellij, and the grants are recorded
/// against that signature. Neither `create_dir_all` nor the file `openssl` writes asks for
/// anything narrower than the umask allows, which on a stock account is world-readable - and the
/// bundle's passphrase is a constant in this file, so the permissions are the whole of its
/// protection. See [`IDENTITY_PASSPHRASE`] for why it cannot be one nobody knows.
#[cfg(unix)]
fn restrict(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn restrict(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// What one rung's attempt on the pin came to.
struct SignAttempt {
    /// What is worth reporting whatever happens next: the sweep, and the signature if there was
    /// one. A refusal's own wording is NOT in here - the caller decides whether it is a `Needs
    /// you` or a note on the rung below that worked.
    findings: Vec<Finding>,
    /// Why this rung did not sign. `None` is a signed pin.
    refusal: Option<String>,
    /// Whether a lower rung would hit the same wall. A `codesign` refusal is the certificate's
    /// fault and the next rung may do better; a copy or a rename that failed is the filesystem's,
    /// and trying again only writes the same error a second time.
    fatal: bool,
}

/// Steps 3 through 6, once a certificate has been chosen.
///
/// It reports rather than decides: a refusal comes back as a `refusal` for the caller to weigh
/// against the rungs below, because whether a refusal is the end of the run depends on what else
/// the keychain holds - which is [`sign_down_the_ladder`]'s question, not this one's.
fn perform_signing(
    commander: &dyn Commander,
    pin: &Path,
    rung: &Rung,
    changed_certificate: Option<&str>,
    earlier_refusals: &[String],
) -> SignAttempt {
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
        return SignAttempt {
            findings,
            refusal: Some(format!(
                "could not copy {} to sign it: {}",
                pin_display, error
            )),
            fatal: true,
        };
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
    let mut refused_timestamp = None;
    if !signed.0 && timestamped {
        // Apple's timestamp server needs a real chain, and refuses one we minted - and it is not
        // reachable at all on a machine that is offline. Losing the timestamp costs a signature
        // nothing it had, so the fall-back is silent about failing and loud about which happened.
        timestamped = false;
        refused_timestamp = Some(signed.1.clone());
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
        return SignAttempt {
            findings,
            refusal: Some(format!(
                "codesign refused to sign with {}: {}",
                rung.description(),
                the_complaint(&signed.1)
            )),
            fatal: false,
        };
    }

    if let Err(reason) = verify_signature(commander, &temporary_display) {
        let _ = std::fs::remove_file(&temporary);
        return SignAttempt {
            findings,
            refusal: Some(format!(
                "{} signed, but the signature did not hold: {}",
                rung.description(),
                reason
            )),
            fatal: false,
        };
    }

    if let Err(error) = std::fs::rename(&temporary, pin) {
        let _ = std::fs::remove_file(&temporary);
        return SignAttempt {
            findings,
            refusal: Some(format!(
                "could not put the signed copy at {}: {}",
                pin_display, error
            )),
            fatal: true,
        };
    }

    let mut done = Finding::changed(
        "signing",
        format!(
            "signed {} with {}{}",
            pin_display,
            rung.description(),
            if timestamped { ", timestamped" } else { "" }
        ),
    )
    .note(format!("identifier {}", PIN_IDENTIFIER));
    for earlier in earlier_refusals {
        // the rungs above this one that would not sign. Silence here would leave a machine with a
        // Developer ID wondering why its pin carries a certificate of ours.
        done = done.note(format!("a rung above it did not sign: {}", earlier));
    }
    if let Some(refusal) = refused_timestamp {
        done = done
            .note("the timestamp was refused, so it was signed without one:")
            .note(format!("  {}", the_complaint(&refusal)));
    }
    findings.push(done);
    findings.push(follow_up(&pin_display, changed_certificate));
    SignAttempt {
        findings,
        refusal: None,
        fatal: false,
    }
}

/// The first line of a tool's complaint, which is the part worth quoting in a report.
fn first_line(message: &str) -> &str {
    message.lines().next().unwrap_or("").trim()
}

/// What `codesign` actually complained about, which is NOT its first line.
///
/// **`-f` makes the first line noise, and quoting it hid every real failure.** Re-signing a pin
/// that already carries a signature - which is every run after the first - makes `codesign` say so
/// before it says anything else:
///
/// ```text
/// /Users/…/zellij: replacing existing signature
/// /Users/…/zellij: errSecInternalComponent
/// ```
///
/// A report that quoted line 1 told the user their signing run had "failed: replacing existing
/// signature", which names the one thing that went right. Seen on a real Mac at 0.45.0-nkmk.8,
/// where the actual fault was a key ACL that had never been granted.
///
/// So the informational line is dropped and everything else is kept, joined and capped. Kept
/// rather than reduced to the last line: `codesign` can emit several, and the one that explains a
/// refusal is not reliably the last one either.
fn the_complaint(message: &str) -> String {
    let kept: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.contains("replacing existing signature"))
        .collect();
    // nothing but the informational line is still worth quoting - a refusal with no words is
    // harder to act on than a redundant one
    let complaint = if kept.is_empty() {
        first_line(message).to_owned()
    } else {
        kept.join("; ")
    };
    const ROOM: usize = 300;
    match complaint.char_indices().nth(ROOM) {
        Some((at, _)) => format!("{}...", &complaint[..at]),
        None => complaint,
    }
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
    Ok(())
}

/// What to do next, in the order that makes it one pass instead of two.
///
/// Re-granting FIRST and restarting SECOND is the whole of the advice. The grants are recorded
/// against the pin's path and the signature it now carries; a server started before they are
/// re-granted comes up not holding them, and the user ends up restarting twice.
fn follow_up(pin: &str, changed_certificate: Option<&str>) -> Finding {
    let mut finding = Finding::needs_you(
        "signing",
        "the signature is in place; two things left, in this order",
    )
    .note(format!(
        "1. re-grant Full Disk Access, Accessibility and Screen Recording for {}",
        pin
    ))
    .note("   in System Settings > Privacy & Security - once, for the new signature")
    .note("2. THEN `zellij session restart`, so the new server comes up already holding them");
    if let Some(why) = changed_certificate {
        // the requirement macOS recorded is not the one it will evaluate from now on. Saying which
        // certificate changed, and why, is the difference between one re-grant and a hunt.
        finding = finding
            .note(why)
            .note("- that is why the re-grant is needed, and not only the restart");
    }
    finding
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
///
/// **A bundle that will not import is not a bundle worth keeping, and reusing one forever was a
/// bug that stranded a machine.** At 0.45.0-nkmk.6 the bundle was written with a passphrase Apple
/// cannot read (see [`IDENTITY_PASSPHRASE`]); at nkmk.7 the file was still there, so this function
/// took the re-import path on every run, failed on every run, and never reached the minting code
/// that had just been fixed. The machine could not repair itself with any future release.
///
/// So a failed import now mints again - but ONLY when the keychain has never held the certificate.
/// That condition is what keeps "mint once" true rather than weakening it: a bundle that never
/// imported was never signed with, so no grant on the machine names it and replacing it costs
/// nothing. A bundle whose certificate IS in the keychain is a different story entirely, and an
/// import that fails then is reported rather than answered with a second certificate.
pub fn ensure_self_signed(
    commander: &dyn Commander,
    dir: &SigningDir,
    keychain: &str,
    keychain_password: Option<&str>,
) -> Result<Vec<Finding>, String> {
    let mut findings = Vec::new();
    std::fs::create_dir_all(&dir.root)
        .map_err(|e| format!("could not create {}: {}", dir.root.display(), e))?;
    restrict(&dir.root, 0o700)
        .map_err(|e| format!("could not lock down {}: {}", dir.root.display(), e))?;

    let bundle = dir.identity_bundle();
    let mut mint = !bundle.exists();
    if !mint {
        findings.push(Finding::ok(
            "signing",
            format!(
                "{} already holds this machine's certificate; re-importing it",
                bundle.display()
            ),
        ));
        if let Err(reason) = import_bundle(commander, &bundle, keychain) {
            if !unreadable_bundle(&reason) || keychain_holds_our_certificate(commander, keychain) {
                // NOT the proven case, or a keychain that has held this certificate before. Either
                // way a second certificate fixes nothing and costs every grant.
                return Err(reason);
            }
            let aside = set_aside(&bundle)?;
            findings.push(
                Finding::changed(
                    "signing",
                    format!(
                        "{} could not be imported, so it was set aside",
                        aside.display()
                    ),
                )
                .note(reason)
                .note("the keychain has never held it, so nothing on this machine was signed")
                .note("with it and no grant names it - a new one costs nothing here"),
            );
            mint = true;
        }
    }
    if mint {
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
        import_bundle(commander, &bundle, keychain)?;
    }

    // The partition list is NOT run here. It belongs to signing, not to minting: a machine that
    // minted last month signs today without coming through this function at all, and the ACL it
    // needs is granted per keychain and not per certificate. `sign_down_the_ladder` runs it before
    // every signature made with our own certificate - see there.
    let _ = keychain_password;
    Ok(findings)
}

/// Whether an import failure is the one that a new certificate actually answers.
///
/// **The gate is narrow on purpose, and widening it is how a machine loses its grants.** Only one
/// failure means "this FILE cannot be read": Apple refusing a PKCS#12 whose MAC it cannot verify,
/// which it reports as a password problem. Every other failure - a locked keychain, a run before
/// graphical login, an SSH session with no dialog to answer - means the KEYCHAIN would not answer,
/// and the bundle in hand may be the machine's one certificate with every grant recorded against
/// it. Minting there would void them all to fix nothing.
fn unreadable_bundle(reason: &str) -> bool {
    let reason = reason.to_lowercase();
    reason.contains("mac verification failed") || reason.contains("wrong password")
}

/// Has this keychain ever held the certificate we mint?
///
/// **`find-identity` cannot answer this, and that is the whole reason for a second question.** It
/// lists identities the keychain calls VALID, so it folds "never imported" together with "imported
/// and untrusted" and with "the keychain will not answer right now" - and it is asked immediately
/// before this code runs, from the one caller, having already returned nothing. Re-asking it would
/// re-read the same answer and gate on a constant.
///
/// `find-certificate` asks about the certificate itself. It needs no trust decision and no access
/// to the private key, so a locked or unanswering keychain that holds the certificate still says
/// so - which is exactly the case the gate must not mistake for a machine that has never had one.
fn keychain_holds_our_certificate(commander: &dyn Commander, keychain: &str) -> bool {
    matches!(
        commander.run(
            "security",
            &["find-certificate", "-c", SELF_SIGNED_COMMON_NAME, keychain],
            None,
        ),
        Ok(output) if output.success
    )
}

/// Move a bundle out of the way, keeping it rather than removing it.
///
/// A private key is never deleted here even when it is useless, because "useless" is this code's
/// reading of an import failure and the file is the only copy of something that cannot be made
/// again. The timestamp keeps a second failure from overwriting the first one's evidence.
fn set_aside(bundle: &Path) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let mut target = bundle.as_os_str().to_owned();
    target.push(format!(".broken-{}", stamp));
    let target = PathBuf::from(target);
    std::fs::rename(bundle, &target).map_err(|error| {
        format!(
            "could not move {} out of the way: {}",
            bundle.display(),
            error
        )
    })?;
    Ok(target)
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
    let mut refusal = String::new();
    let mut bundled = false;
    for legacy in [true, false] {
        let output = commander
            .run(
                "openssl",
                &pkcs12_arguments(&key, &certificate, &bundle, legacy),
                None,
            )
            .map_err(|reason| format!("could not run openssl: {}", reason))?;
        if output.success {
            bundled = true;
            break;
        }
        if refusal.is_empty() {
            refusal = output.stderr.trim().to_owned();
        }
    }
    if !bundled {
        return Err(format!(
            "openssl could not bundle the certificate: {}",
            refusal
        ));
    }
    for private in [dir.private_key(), dir.identity_bundle()] {
        restrict(&private, 0o600)
            .map_err(|e| format!("could not lock down {}: {}", private.display(), e))?;
    }
    Ok(())
}

/// The argv that bundles the key and the certificate into a p12 macOS will actually import.
///
/// **Every algorithm here is named on purpose.** OpenSSL 3 defaults a PKCS#12 file to a SHA-256
/// MAC and AES-based PBE, and Apple's importer accepts neither - it reports the MAC it cannot
/// verify as a password it was not given, which points at the one thing that is not wrong:
///
/// ```text
/// security: SecKeychainItemImport: MAC verification failed during PKCS12 import (wrong password?)
/// ```
///
/// So the MAC is SHA-1 and both PBEs are `PBE-SHA1-3DES`, which is what `security import` has
/// always read. `-legacy` is the flag that lets OpenSSL 3 emit them at all, and it is tried FIRST
/// and dropped on failure rather than probed for: macOS ships LibreSSL as `/usr/bin/openssl` and
/// LibreSSL has no `-legacy`, while an OpenSSL 3 from Homebrew may be first on `PATH` instead. A
/// version probe would have to parse `openssl version` and be right about two projects' numbering;
/// running the command and reading its exit status is the same answer with nothing to be wrong
/// about.
///
/// The passphrase is [`IDENTITY_PASSPHRASE`], and it has to be non-empty for the same importer to
/// read the file at all - see that constant, which is where the evidence is.
fn pkcs12_arguments<'a>(
    key: &'a str,
    certificate: &'a str,
    bundle: &'a str,
    legacy: bool,
) -> Vec<&'a str> {
    let mut args = vec![
        "pkcs12",
        "-export",
        "-inkey",
        key,
        "-in",
        certificate,
        "-out",
        bundle,
        "-name",
        SELF_SIGNED_COMMON_NAME,
        "-macalg",
        "sha1",
        "-certpbe",
        "PBE-SHA1-3DES",
        "-keypbe",
        "PBE-SHA1-3DES",
        "-passout",
        PKCS12_PASSOUT,
    ];
    if legacy {
        args.push("-legacy");
    }
    args
}

/// Put the bundle in the keychain.
///
/// `-T /usr/bin/codesign` names the one program allowed to use it, rather than `-A`, which would
/// let anything on the machine sign with this machine's identity.
///
/// Split from [`allow_codesign_to_reach_the_key`] because the two fail for unrelated reasons and
/// only this one is a reason to mint again: a refused import means the FILE cannot be read, while
/// a refused partition list means the KEYCHAIN would not answer, and a second certificate fixes
/// neither of those but is only ever justified by the first.
fn import_bundle(commander: &dyn Commander, bundle: &Path, keychain: &str) -> Result<(), String> {
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
                IDENTITY_PASSPHRASE,
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
    Ok(())
}

/// Let `codesign` reach the key without a dialog.
///
/// `set-key-partition-list` is a CONVENIENCE and not a precondition, and treating it as one cost a
/// release. Without it macOS asks for the key once per signature, through the standard key-access
/// dialog, instead of never - which is worse but is not a failure, and `codesign` is what raises
/// that dialog. So a refusal here returns a `Needs you` and the run goes on to sign.
///
/// **It can also be the step that hangs.** With no `-k`, `security` asks for the keychain password
/// on the controlling terminal - not through the window server, and not on stdin - so at
/// 0.45.0-nkmk.8 it blocked forever inside a graphical session with one line on the pane's
/// terminal and an empty report. Every child doctor runs is now put in a session of its own, which
/// leaves it nothing to prompt on; see `SystemCommander::run`. This step therefore fails fast
/// rather than waiting, which is exactly why its failure has to be survivable.
///
/// NO TRUSTED ROOT IS ADDED anywhere
/// here, deliberately - requirement evaluation does not consult trust unless the requirement says
/// `trusted`, and ours never does. What signing needs is access to the key, which is exactly what
/// this grants and nothing more. See [`find_identities`] for the other half of that argument, which
/// is where the temptation to add trust actually comes from.
fn allow_codesign_to_reach_the_key(
    commander: &dyn Commander,
    keychain: &str,
    keychain_password: Option<&str>,
) -> Option<Finding> {
    let mut args = vec![
        String::from("set-key-partition-list"),
        String::from("-S"),
        String::from("apple-tool:,apple:,codesign:"),
        String::from("-s"),
    ];
    if let Some(password) = keychain_password {
        // ZELLIJ_KEYCHAIN_PASSWORD, when the user has set it. It goes in argv because `security`
        // reads it nowhere else, which puts it in this machine's process table for the life of one
        // command - the reason this is an escape hatch and not the default path. It is never
        // asked for: doctor prompts for nothing, anywhere.
        args.push(String::from("-k"));
        args.push(password.to_owned());
    }
    args.push(keychain.to_owned());
    let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
    let refusal = match commander.run("security", &borrowed, None) {
        Ok(output) if output.success => return None,
        Ok(output) => first_line(output.stderr.trim()).to_owned(),
        Err(reason) => reason,
    };
    Some(
        Finding::needs_you(
            "signing",
            format!("codesign may ask for the key: {}", refusal),
        )
        .note("without the partition list, macOS asks once per signature instead of never")
        .note(KEY_ACCESS_REMEDIES[0])
        .note(KEY_ACCESS_REMEDIES[1])
        .note(KEY_ACCESS_REMEDIES[2]),
    )
}

/// The two ways a person can give `codesign` the key, in the order worth trying them.
///
/// Written once because they belong to two findings that are reached separately: the partition
/// list refusing (where they are a warning) and every rung refusing (where they are the answer).
/// Neither of them is "let doctor ask you for your password" - doctor prompts for nothing. That is
/// not a style choice: `security` prompts on the CONTROLLING TERMINAL, which a launchd job and a
/// detached pane do not have and an SSH session cannot answer unattended, and a prompt nobody sees
/// is a program that hangs. Every child doctor runs is put in its own session for that reason; see
/// `SystemCommander::run`.
const KEY_ACCESS_REMEDIES: [&str; 3] = [
    "either run doctor from a terminal in the desktop session and click Always Allow",
    "  on the key-access dialog macOS raises, once, for this certificate",
    "or set ZELLIJ_KEYCHAIN_PASSWORD and run it again - doctor reads it, never asks",
];

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
    fn the_team_id_comes_off_the_certificate_and_never_off_the_name() {
        // the real subject of the certificate that produced a broken pin at 0.45.0-nkmk.7, in
        // LibreSSL's spelling - which is what `/usr/bin/openssl` writes on macOS
        assert_eq!(
            team_id_from_subject(
                "subject= /UID=7472L5G3Y6/CN=Apple Development: someone (DY7JA3K8QZ)\
                 /OU=U2VEDWFUF3/O=Someone/C=US"
            )
            .as_deref(),
            Some("U2VEDWFUF3")
        );
        // OpenSSL 3's spelling of the same line. A Homebrew install puts it first on PATH.
        assert_eq!(
            team_id_from_subject(
                "subject=UID = 7472L5G3Y6, CN = Apple Development: someone (DY7JA3K8QZ), \
                 OU = U2VEDWFUF3, O = Someone, C = US"
            )
            .as_deref(),
            Some("U2VEDWFUF3")
        );
        // the whole of the nkmk.7 bug in one assertion: the code in the CN is NOT the team id
        assert_ne!(
            team_id_from_subject(
                "subject= /CN=Apple Development: someone (DY7JA3K8QZ)/OU=U2VEDWFUF3"
            )
            .as_deref(),
            Some("DY7JA3K8QZ")
        );
        // a subject with no OU has no team id, and inventing one is what got us here
        assert_eq!(
            team_id_from_subject("subject= /CN=zellij self-signed code signing"),
            None
        );
        // `OU` has to be a key and not the tail of another one
        assert_eq!(
            team_id_from_subject("subject= /CN=Someone/businessOU=NOTATEAM"),
            None
        );
    }

    /// The certificate is read through the keychain, and the argv that reads it is part of the
    /// contract: a `find-certificate` that named the wrong thing would silently answer `None`, and
    /// `None` signs with a worse requirement rather than failing.
    #[test]
    fn the_certificate_is_read_from_the_keychain_by_the_identity_name() {
        let name = "Apple Development: someone@example.com (F6G7H8I9J0)";
        let commander = RecordedCommander::new(&[
            (
                format!("security find-certificate -c {} -p login.keychain-db", name).as_str(),
                recorded("-----BEGIN CERTIFICATE-----\nMII...\n-----END CERTIFICATE-----\n"),
            ),
            (
                "openssl x509 -noout -subject",
                recorded("subject= /CN=Apple Development: someone (F6G7H8I9J0)/OU=A1B2C3D4E5\n"),
            ),
        ]);
        assert_eq!(
            team_id_from_keychain(&commander, "login.keychain-db", name).as_deref(),
            Some("A1B2C3D4E5")
        );
    }

    /// A keychain that will not answer is not a failure. The rung still signs; it takes the
    /// requirement `codesign` derives instead of ours, which is worse than ours and better than a
    /// requirement the binary does not satisfy.
    #[test]
    fn a_certificate_that_cannot_be_read_gives_no_team_and_writes_no_requirement() {
        let commander = RecordedCommander::new(&[]);
        assert_eq!(
            team_id_from_keychain(
                &commander,
                "login.keychain-db",
                "Apple Development: someone"
            ),
            None
        );
        let rung = Rung::AppleDevelopment {
            identity: Identity {
                hash: String::from("AAAA"),
                name: String::from("Apple Development: someone@example.com (F6G7H8I9J0)"),
            },
            team: None,
        };
        assert_eq!(requirement_for(&rung), None);
        let args = sign_arguments(&rung, requirement_for(&rung).as_deref(), true, "/tmp/pin");
        assert!(
            !args.iter().any(|argument| argument.starts_with("-r")),
            "{:?}",
            args
        );
    }

    /// The CN carries an email and changes on reissue; the OU is the team id and does not. A
    /// requirement anchored on the CN is a grant that expires with the certificate.
    #[test]
    fn apple_development_is_anchored_on_the_team_id_and_never_on_the_name() {
        let rung = apple_development_rung("A1B2C3D4E5");
        let requirement = requirement_for(&rung).unwrap();
        assert!(requirement.contains("subject.OU"), "{}", requirement);
        assert!(!requirement.contains("subject.CN"), "{}", requirement);
        assert!(
            !requirement.contains("someone@example.com"),
            "{}",
            requirement
        );
        // and never the code out of the name, which is the per-developer id and not the team
        assert!(!requirement.contains("F6G7H8I9J0"), "{}", requirement);
    }

    /// The rung as the ladder builds it once the certificate has been read.
    fn apple_development_rung(team: &str) -> Rung {
        Rung::AppleDevelopment {
            identity: Identity {
                hash: String::from("AAAA"),
                name: String::from("Apple Development: someone@example.com (F6G7H8I9J0)"),
            },
            team: Some(String::from(team)),
        }
    }

    /// `codesign -r` reads a requirement SET. A text opening with `identifier` puts a reserved
    /// word where a tag belongs, and the rung refuses with `line 1:1: unexpected token:
    /// identifier` - which is the whole rung lost on a machine that has an Apple Development
    /// certificate and nothing better.
    #[test]
    fn the_apple_development_requirement_is_a_set_and_not_a_bare_expression() {
        let rung = apple_development_rung("F6G7H8I9J0");
        let requirement = requirement_for(&rung).unwrap();
        // spelled out in full: a botched string continuation would still pass a `starts_with`
        assert_eq!(
            requirement,
            "designated => identifier \"org.zellij.nkmk\" and anchor apple generic and \
             certificate leaf[subject.OU] = \"F6G7H8I9J0\""
        );
        // and it reaches codesign as inline text, which is what the leading `=` of `-r=` buys
        let args = sign_arguments(&rung, Some(&requirement), false, "/tmp/pin");
        let passed = args
            .iter()
            .find(|argument| argument.starts_with("-r="))
            .unwrap_or_else(|| panic!("{:?}", args));
        assert_eq!(passed, &format!("-r={}", requirement));
    }

    /// A certificate the keychain offers is not a certificate that signs. The run that stopped on
    /// the first refusal left the pin ad-hoc-signed - the state signing exists to remove - with a
    /// working rung one step below it.
    #[test]
    fn a_rung_that_refuses_falls_through_to_the_one_below_it() {
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
            // the Developer ID is the head of the ladder, and this machine's cannot sign
            (
                "codesign -s 0011223344556677889900AABBCCDDEEFF001122",
                recorded_failure("A1B2C3D4E5: no identity found"),
            ),
            (
                "codesign -s A1B2C3D4E5F60718293A4B5C6D7E8F9001122334",
                recorded(""),
            ),
            ("codesign -d --verbose=2 -r- ", recorded(APPLE_DEVELOPMENT)),
            ("codesign --verify ", recorded("")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        let signed = run
            .findings
            .iter()
            .find(|finding| {
                finding.status == Status::Changed && finding.message.contains("Apple Development")
            })
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(
            signed
                .notes
                .iter()
                .any(|note| note.contains("Developer ID")),
            "{:?}",
            signed
        );
        // and the refusal is not also reported as a failure of the run
        assert!(
            !run.findings.iter().any(|finding| finding
                .message
                .contains("no certificate on this machine would sign")),
            "{:?}",
            run.findings
        );
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .flatten()
                .all(|entry| entry.file_name() == "zellij"),
            "a temp file was left behind"
        );
    }

    /// Recorded from a corporate Mac: an Apple Development certificate, and one of ours from an
    /// earlier run that had none.
    const APPLE_AND_OURS: &str = "\
  1) A1B2C3D4E5F60718293A4B5C6D7E8F9001122334 \"Apple Development: someone@example.com (F6G7H8I9J0)\"
  2) 7F8C0B1A2D3E4F5061728394A5B6C7D8E9F00112 \"zellij self-signed code signing\"
     2 valid identities found
";

    /// The walk stops at the Apple rungs. A certificate of ours has a different requirement - its
    /// own hash - so signing with it after an Apple certificate refused would void every grant on
    /// the machine, and it would stick: a self-signed signature is anchored, so the next run reads
    /// the pin as already correct and never climbs back.
    #[test]
    fn an_apple_certificate_that_refuses_is_never_demoted_to_one_of_ours() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(APPLE_AND_OURS)),
            (
                "codesign -s A1B2C3D4E5F60718293A4B5C6D7E8F9001122334",
                recorded_failure("errSecInternalComponent"),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        assert!(
            !commander.called_with("codesign -s 7F8C0B1A2D3E4F5061728394A5B6C7D8E9F00112"),
            "{:?}",
            commander.calls()
        );
        let refused = run
            .findings
            .iter()
            .find(|finding| finding.message.contains("refused to sign"))
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert_eq!(refused.status, Status::NeedsYou);
        assert!(
            refused.message.starts_with("1 certificate "),
            "{:?}",
            refused
        );
        // and the machine is not sent to Xcode for a certificate it already has
        assert!(
            !run.findings
                .iter()
                .any(|finding| finding.message.contains("no signing certificate")),
            "{:?}",
            run.findings
        );
        assert_eq!(std::fs::read(&pin).unwrap(), b"the working copy".to_vec());
    }

    /// The other side of that boundary: with no Apple certificate anywhere, ours is the rung, and
    /// a refusal there really is a machine that cannot sign.
    #[test]
    fn a_refusal_on_the_rung_we_mint_still_reaches_the_xcode_steps() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(ONLY_OURS)),
            (
                "codesign -s 7F8C0B1A2D3E4F5061728394A5B6C7D8E9F00112",
                recorded_failure("the keychain is locked"),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        assert!(
            run.findings
                .iter()
                .any(|finding| finding.message.contains("no signing certificate")),
            "{:?}",
            run.findings
        );
    }

    /// A changed certificate is a changed requirement, and the re-grant note that says so has to
    /// name the change in whichever direction it happened.
    #[test]
    fn a_changed_certificate_is_named_in_both_directions() {
        let apple = choose_rung(&parse_identities(TWO_IDENTITIES)).unwrap();
        let ours = choose_rung(&parse_identities(ONLY_OURS)).unwrap();
        // an Apple certificate on a machine that had minted its own
        assert!(changes_certificate(&apple, true, true, false).is_some());
        assert!(changes_certificate(&apple, false, true, false).is_none());
        // ours on a machine that has an Apple certificate
        assert!(changes_certificate(&ours, true, true, false).is_some());
        assert!(changes_certificate(&ours, true, false, false).is_none());
    }

    /// A fall from Developer ID onto an Apple Development certificate keeps the requirement only
    /// while the second one's team id could be read. When it could not, the rung writes no
    /// requirement and takes a CN-anchored one, which is a different requirement and therefore a
    /// re-grant - so the user gets told why rather than a bare "re-grant".
    #[test]
    fn a_fall_onto_a_rung_with_no_team_id_says_the_requirement_changed() {
        let no_team = Rung::AppleDevelopment {
            identity: Identity {
                hash: String::from("AAAA"),
                name: String::from("Apple Development: someone@example.com (F6G7H8I9J0)"),
            },
            team: None,
        };
        // reached by falling from the rung above: the requirement is not the one it was
        let why = changes_certificate(&no_team, false, true, true).unwrap();
        assert!(why.contains("team id"), "{}", why);
        assert!(follow_up("/tmp/pin", Some(why))
            .notes
            .iter()
            .any(|note| note.contains("team id")));
        // reached as the FIRST rung, which is the ordinary machine with only an Apple Development
        // certificate: nothing above it signed, so nothing changed
        assert!(changes_certificate(&no_team, false, true, false).is_none());
    }

    /// Apple's importer reads a SHA-1 MAC and 3DES PBE and nothing newer. OpenSSL 3 defaults to
    /// neither, and reports the MAC it cannot verify as a password that was not given.
    #[test]
    fn the_bundle_is_written_with_the_algorithms_apples_importer_reads() {
        let legacy = pkcs12_arguments("/k.pem", "/c.pem", "/id.p12", true);
        for expected in [
            "-macalg",
            "sha1",
            "-certpbe",
            "PBE-SHA1-3DES",
            "-keypbe",
            "PBE-SHA1-3DES",
        ] {
            assert!(legacy.contains(&expected), "{:?}", legacy);
        }
        assert!(legacy.contains(&"-legacy"), "{:?}", legacy);
        // LibreSSL is `/usr/bin/openssl` on macOS and has no `-legacy`, so there has to be a
        // second form that names the algorithms without it
        assert!(
            !pkcs12_arguments("/k.pem", "/c.pem", "/id.p12", false).contains(&"-legacy"),
            "the fall-back still carries -legacy"
        );
    }

    /// The algorithms were right at 0.45.0-nkmk.7 and the file still would not import. An empty
    /// passphrase is the reason: Apple's importer cannot verify the MAC of one, and says so as
    /// `wrong password?`. Both ends of the pair are asserted, because a bundle written with a
    /// passphrase and imported without it fails exactly as the empty one did.
    #[test]
    fn the_bundle_carries_a_passphrase_because_an_empty_one_will_not_import() {
        let args = pkcs12_arguments("/k.pem", "/c.pem", "/id.p12", true);
        assert!(args.contains(&"pass:zellij"), "{:?}", args);
        assert!(!args.contains(&"pass:"), "{:?}", args);

        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"the one certificate").unwrap();
        let commander = RecordedCommander::new(&[
            ("security import", recorded("")),
            ("security set-key-partition-list", recorded("")),
        ]);
        ensure_self_signed(&commander, &dir, "login.keychain-db", None).unwrap();
        let import = commander
            .calls()
            .into_iter()
            .find(|call| call.starts_with("security import"))
            .unwrap();
        assert!(import.contains(" -P zellij "), "{}", import);
    }

    /// A bundle that will not import is not a bundle to keep re-importing. This is what stranded a
    /// real Mac: the nkmk.6 file was unimportable, the nkmk.7 run re-imported it rather than
    /// minting, and the minting code that had just been fixed never ran.
    #[test]
    fn a_bundle_that_will_not_import_is_set_aside_and_minted_again() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"a bundle apple cannot read").unwrap();

        let commander = RecordedCommander::new(&[
            (
                "security import",
                recorded_failure(
                    "security: SecKeychainItemImport: MAC verification failed during PKCS12 \
                     import (wrong password?)",
                ),
            ),
            // the keychain has never held it, so nothing is anchored on it. `find-certificate`
            // and not `find-identity`: the second folds "never had it" together with "will not
            // answer", and the caller has already had that answer.
            (
                "security find-certificate",
                recorded_failure(
                    "security: SecKeychainSearchCopyNext: The specified item \
                                  could not be found in the keychain.",
                ),
            ),
            ("openssl req", recorded("")),
            ("openssl pkcs12", recorded("")),
            ("security set-key-partition-list", recorded("")),
        ]);
        // the run still ends in an error - `openssl` is recorded rather than run, so it writes no
        // key for the mint to lock down - and that is fine. What this test pins is that the run
        // reached the mint at all, which is what nkmk.7 never did.
        let outcome = ensure_self_signed(&commander, &dir, "login.keychain-db", None);
        assert!(outcome.is_err(), "{:?}", outcome.map(|f| f.len()));
        assert!(
            commander.called_with("openssl req"),
            "{:?}",
            commander.calls()
        );

        let aside: Vec<_> = std::fs::read_dir(directory.path())
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.contains(".broken-"))
            .collect();
        assert_eq!(aside.len(), 1, "{:?}", aside);
        // kept, never deleted: it is the only copy of a key that cannot be made again
        assert_eq!(
            std::fs::read(directory.path().join(&aside[0])).unwrap(),
            b"a bundle apple cannot read".to_vec()
        );
    }

    /// The other half of the same rule. An import can fail for reasons that have nothing to do
    /// with the file, and if the certificate is already in the keychain then grants name it - so a
    /// second certificate would void them. That case reports and mints nothing.
    #[test]
    fn a_failed_import_of_a_certificate_the_keychain_already_holds_mints_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"the one certificate").unwrap();

        let commander = RecordedCommander::new(&[
            (
                "security import",
                recorded_failure(
                    "security: SecKeychainItemImport: MAC verification failed during PKCS12 \
                     import (wrong password?)",
                ),
            ),
            (
                "security find-certificate",
                recorded("-----BEGIN CERTIFICATE-----"),
            ),
        ]);
        let outcome = ensure_self_signed(&commander, &dir, "login.keychain-db", None);

        assert!(outcome.is_err(), "{:?}", outcome.map(|f| f.len()));
        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
        assert!(dir.identity_bundle().exists(), "the bundle was moved aside");
    }

    /// The 0.45.0-nkmk.8 hang, at the level a unit test can reach it. `security
    /// set-key-partition-list` with no `-k` blocked forever on a real Mac, and when it is made to
    /// fail instead, the run must CONTINUE: the partition list only decides whether macOS asks for
    /// the key once per signature or never, and stopping there left a machine with a certificate
    /// it had just imported and a pin it never signed.
    #[test]
    fn a_partition_list_that_refuses_does_not_stop_the_signing() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();

        let commander = signing_with_our_own(
            &pin,
            recorded_failure(
                "SecKeychainItemSetAccessWithPassword: User interaction is not allowed.",
            ),
        );
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        // it went on to sign, which is the whole point: `codesign` is what raises the dialog the
        // partition list exists to avoid
        assert!(
            commander.called_with("codesign -s "),
            "{:?}",
            commander.calls()
        );
        let findings = run.findings;
        let warned = findings
            .iter()
            .find(|finding| finding.message.contains("codesign may ask for the key"))
            .unwrap_or_else(|| panic!("{:?}", findings));
        // both remedies, and neither of them is doctor prompting for anything
        assert!(
            warned
                .notes
                .iter()
                .any(|note| note.contains("Always Allow")),
            "{:?}",
            warned.notes
        );
        assert!(
            warned
                .notes
                .iter()
                .any(|note| note.contains("ZELLIJ_KEYCHAIN_PASSWORD")),
            "{:?}",
            warned.notes
        );
    }

    /// `-f` makes `codesign` announce that it is replacing a signature before it says anything
    /// else, so quoting its FIRST line reported every real failure as "replacing existing
    /// signature" - the one thing that had gone right. Seen on a real Mac, where it hid a key ACL
    /// that had never been granted.
    #[test]
    fn a_refusal_quotes_the_error_and_not_codesigns_own_chatter() {
        assert_eq!(
            the_complaint(
                "/Users/someone/zellij: replacing existing signature\n\
                 /Users/someone/zellij: errSecInternalComponent"
            ),
            "/Users/someone/zellij: errSecInternalComponent"
        );
        // several real lines are all kept, because the one that explains a refusal is not reliably
        // the last one either
        assert_eq!(
            the_complaint("zellij: replacing existing signature\nfirst thing\nsecond thing"),
            "first thing; second thing"
        );
        // and a complaint that is ONLY the chatter still says something
        assert_eq!(
            the_complaint("zellij: replacing existing signature"),
            "zellij: replacing existing signature"
        );
        assert!(the_complaint(&"x".repeat(400)).ends_with("..."));

        // end to end, through the report a person actually reads
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(ONLY_OURS)),
            ("security set-key-partition-list", recorded("")),
            (
                "codesign -s ",
                recorded_failure(
                    "/tmp/pin: replacing existing signature\n/tmp/pin: errSecInternalComponent",
                ),
            ),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );
        let said = format!("{:?}", run.findings);
        assert!(said.contains("errSecInternalComponent"), "{}", said);
    }

    /// And when `codesign` then refuses too - no window server, or a denied dialog - the report
    /// names both remedies rather than only the SSH one it used to.
    #[test]
    fn a_ladder_that_refuses_names_both_ways_to_give_codesign_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(ONLY_OURS)),
            ("codesign -s ", recorded_failure("errSecInternalComponent")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        let exhausted = run
            .findings
            .iter()
            .find(|finding| finding.message.contains("refused to sign"))
            .unwrap_or_else(|| panic!("{:?}", run.findings));
        assert!(
            exhausted
                .notes
                .iter()
                .any(|note| note.contains("Always Allow")),
            "{:?}",
            exhausted.notes
        );
        assert!(
            exhausted
                .notes
                .iter()
                .any(|note| note.contains("ZELLIJ_KEYCHAIN_PASSWORD")),
            "{:?}",
            exhausted.notes
        );
    }

    /// The machine that hung has its certificate IMPORTED already - the hang came after that - so
    /// the next run must find it and sign, not mint a second one. `find-identity -v` still cannot
    /// see it, because it is untrusted; the non-`-v` listing is what makes this work.
    #[test]
    fn a_machine_whose_import_already_succeeded_signs_rather_than_minting_again() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        // the bundle from the run that hung is still on disk, which is what a re-mint would take
        let signing = scratch.path().join("signing");
        std::fs::create_dir_all(&signing).unwrap();
        std::fs::write(signing.join("id.p12"), b"the one certificate").unwrap();

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            // untrusted, so `-v` reports nothing at all
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
            (
                "security find-identity -p codesigning",
                recorded(
                    "  1) BBBB \"zellij self-signed code signing\" (CSSMERR_TP_NOT_TRUSTED)\n     \
                     1 identities found\n",
                ),
            ),
            ("codesign -s BBBB", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(SELF_SIGNED)),
            ("codesign --verify ", recorded("")),
        ]);
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        assert!(
            !commander.called_with("openssl"),
            "it minted again: {:?}",
            commander.calls()
        );
        assert!(
            !commander.called_with("security import"),
            "{:?}",
            commander.calls()
        );
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.status == Status::Changed
                    && finding.message.contains("signed")),
            "{:?}",
            run.findings
        );
    }

    /// The gate that b5bde6cb6's first cut got wrong. It asked `find-identity`, which the ONLY
    /// caller has just asked and had answer "nothing" - so the check gated on a constant, and any
    /// import failure at all set the machine's one certificate aside and minted a second. A locked
    /// keychain is the case that makes that fatal: it holds the certificate every grant names, and
    /// it fails the import for a reason a new certificate does not fix.
    #[test]
    fn a_keychain_that_will_not_answer_is_not_a_keychain_that_never_held_the_certificate() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::write(dir.identity_bundle(), b"the one certificate").unwrap();

        let commander = RecordedCommander::new(&[
            (
                "security import",
                recorded_failure(
                    "security: SecKeychainItemImport: User interaction is not allowed.",
                ),
            ),
            // even with the certificate nowhere to be found, the error is not the proven one
            (
                "security find-certificate",
                recorded_failure("security: could not be found in the keychain."),
            ),
            // and `find-identity` agrees with the caller that there is no valid identity, which is
            // exactly the answer the broken gate mistook for permission to mint
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
        ]);
        let outcome = ensure_self_signed(&commander, &dir, "login.keychain-db", None);

        assert!(outcome.is_err(), "{:?}", outcome.map(|f| f.len()));
        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
        assert_eq!(
            std::fs::read(dir.identity_bundle()).unwrap(),
            b"the one certificate".to_vec(),
            "the machine's only certificate was set aside"
        );
        assert!(
            std::fs::read_dir(directory.path())
                .unwrap()
                .filter_map(|entry| entry.ok())
                .all(|entry| !entry.file_name().to_string_lossy().contains(".broken-")),
            "something was set aside"
        );
    }

    /// Which failures count, spelled out. Widening this predicate is how a machine loses its
    /// grants, so it is pinned rather than left to the call site.
    #[test]
    fn only_a_bundle_apple_cannot_read_justifies_a_second_certificate() {
        assert!(unreadable_bundle(
            "security: SecKeychainItemImport: MAC verification failed during PKCS12 import \
             (wrong password?)"
        ));
        assert!(!unreadable_bundle(
            "security: SecKeychainItemImport: User interaction is not allowed."
        ));
        assert!(!unreadable_bundle("security: the keychain is locked"));
        assert!(!unreadable_bundle("could not run security: no such file"));
    }

    /// A certificate we minted has no chain to a trusted root, so `find-identity -v` - where `-v`
    /// means "valid", and valid means TRUSTED - can report `0 valid identities found` on a machine
    /// that holds it and can sign with it. Seen on a real Mac. The answer is the untrusted
    /// listing, NOT a trusted root: signing and grants both ignore trust.
    #[test]
    fn our_own_certificate_is_found_even_when_the_keychain_calls_it_untrusted() {
        let commander = RecordedCommander::new(&[
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
            (
                "security find-identity -p codesigning",
                recorded(
                    "  1) BBBB \"zellij self-signed code signing\" (CSSMERR_TP_NOT_TRUSTED)\n     \
                     1 identities found\n",
                ),
            ),
        ]);
        let identities = find_identities(&commander);
        assert_eq!(identities.len(), 1, "{:?}", identities);
        assert_eq!(identities[0].name, SELF_SIGNED_COMMON_NAME);
        assert_eq!(identities[0].hash, "BBBB");
        assert!(matches!(
            choose_rung(&identities),
            Some(Rung::SelfSigned(_))
        ));
        assert!(
            !commander.called_with("add-trusted-cert"),
            "{:?}",
            commander.calls()
        );
    }

    /// The untrusted listing is filtered to OUR name. An Apple certificate the keychain calls
    /// invalid is invalid for a reason - expired, revoked, no private key - and taking it would
    /// put the ladder on a rung that cannot sign.
    #[test]
    fn an_apple_certificate_the_keychain_calls_invalid_is_not_taken_off_the_untrusted_listing() {
        let commander = RecordedCommander::new(&[
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
            (
                "security find-identity -p codesigning",
                recorded(
                    "  1) CCCC \"Apple Development: someone@example.com (F6G7H8I9J0)\" \
                     (CSSMERR_TP_CERT_EXPIRED)\n     1 identities found\n",
                ),
            ),
        ]);
        assert!(find_identities(&commander).is_empty());
    }

    /// `-legacy` is what lets OpenSSL 3 write those algorithms and it is what LibreSSL refuses to
    /// parse. Both openssls are reachable as `openssl`, so the flag is tried and dropped rather
    /// than decided from a version string.
    #[test]
    fn a_bundle_refused_with_legacy_is_written_again_without_it() {
        let directory = tempfile::tempdir().unwrap();
        let dir = SigningDir::new(directory.path().to_path_buf());
        std::fs::create_dir_all(&dir.root).unwrap();
        let key = dir.private_key().display().to_string();
        let certificate = dir.certificate().display().to_string();
        let bundle = dir.identity_bundle().display().to_string();
        let line = |legacy: bool| {
            format!(
                "openssl {}",
                pkcs12_arguments(&key, &certificate, &bundle, legacy).join(" ")
            )
        };

        let commander = RecordedCommander::new(&[
            ("openssl req", recorded("")),
            (
                line(true).as_str(),
                recorded_failure("openssl: Unknown option: -legacy"),
            ),
            (line(false).as_str(), recorded("")),
        ]);
        // the recorded openssl writes nothing, so the files it would have made are put here
        std::fs::write(dir.certificate(), b"certificate").unwrap();
        std::fs::write(dir.private_key(), b"key").unwrap();
        std::fs::write(dir.identity_bundle(), b"bundle").unwrap();
        mint_self_signed(&commander, &dir).unwrap();

        assert!(commander.called_with("-legacy"), "{:?}", commander.calls());
        assert!(
            commander
                .calls()
                .iter()
                .any(|call| call.contains("-macalg sha1") && !call.contains("-legacy")),
            "{:?}",
            commander.calls()
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
    /// exited 0 for two releases, so the machine could never repair itself.
    #[test]
    fn an_anchored_pin_that_does_not_verify_is_not_healthy_and_is_signed_again() {
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
            // the rung taken from these is Developer ID, which reads no certificate
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(DEVELOPER_ID)),
            ("codesign --verify ", recorded("")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode::default(),
            &context(scratch.path()),
        );

        assert!(
            run.findings
                .iter()
                .any(|finding| finding.status == Status::NeedsYou
                    && finding
                        .message
                        .contains("does not satisfy its own requirement")),
            "{:?}",
            run.findings
        );
        assert!(
            commander.called_with("codesign -s "),
            "a pin that fails verification was left as it was: {:?}",
            commander.calls()
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

    /// The case a dry run is read most carefully in, and the one it used to get backwards: with no
    /// Apple certificate on the machine, the real run mints one and signs, so the dry run has to
    /// say that rather than print the Xcode steps and exit non-zero over work doctor does itself.
    #[test]
    fn a_dry_run_with_no_certificate_names_the_one_it_would_mint() {
        let commander = RecordedCommander::new(&[
            ("codesign -d --verbose=2 -r- /tmp/pin", recorded(AD_HOC)),
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
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
        assert_eq!(run.findings.len(), 1, "{:?}", run.findings);
        assert_eq!(run.findings[0].status, Status::Changed);
        assert!(
            run.findings[0].message.starts_with("would mint"),
            "{:?}",
            run.findings[0]
        );
        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
        assert!(!commander.called_with("-s "), "{:?}", commander.calls());
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
        // a stale temp from a run that did not finish, which the sweep has to take. Both gates
        // have to be open for it: a pid nothing is using, and an mtime older than the age gate.
        let stale = a_signing_temp(
            directory.path(),
            a_pid_that_has_finished(),
            std::time::Duration::from_secs(48 * 60 * 60),
        );

        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin_display).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(DEVELOPER_ID)),
            ("codesign --verify ", recorded("")),
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
            .filter(|(_, call)| call.starts_with("codesign --verify "))
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
            ("codesign --verify ", recorded("")),
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

    /// The second copy is the whole of the machine's insurance, so it lands where the user's other
    /// zellij files are - and it lands readable by nobody else. The bundle carries no passphrase.
    #[test]
    fn the_backup_copy_goes_to_the_config_directory_and_nobody_else_can_read_it() {
        let scratch = tempfile::tempdir().unwrap();
        let mut context = context(scratch.path());
        std::fs::create_dir_all(&context.signing_dir.root).unwrap();
        std::fs::write(
            context.signing_dir.identity_bundle(),
            b"the one certificate",
        )
        .unwrap();
        context.backup_dir = Some(scratch.path().join("config"));

        let finding = back_up_identity(&context).unwrap();
        assert_eq!(finding.status, Status::Changed);
        let copy = scratch.path().join("config/zellij-signing-id.p12");
        assert_eq!(
            std::fs::read(&copy).unwrap(),
            b"the one certificate".to_vec()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    /// A copy that did not happen has to be said out loud: the certificate cannot be minted again,
    /// and a user who believes there is a second copy of it will not make one.
    #[test]
    fn a_backup_that_could_not_be_written_is_reported_rather_than_swallowed() {
        let scratch = tempfile::tempdir().unwrap();
        let mut context = context(scratch.path());
        std::fs::create_dir_all(&context.signing_dir.root).unwrap();
        std::fs::write(context.signing_dir.identity_bundle(), b"x").unwrap();
        // a FILE where the directory would go, so creating it cannot succeed
        let blocked = scratch.path().join("blocked");
        std::fs::write(&blocked, b"not a directory").unwrap();
        context.backup_dir = Some(blocked.join("config"));

        let finding = back_up_identity(&context).unwrap();
        assert_eq!(finding.status, Status::NeedsYou);
        assert!(
            finding
                .notes
                .iter()
                .any(|note| note.contains("voids every grant")),
            "{:?}",
            finding
        );
    }

    /// A dry run must describe the run it is standing in for. Minting is what doctor does by
    /// default on a machine with no Apple certificate, so "no signing certificate" would be a
    /// report of a different command.
    #[test]
    fn a_dry_run_with_no_certificate_says_it_would_mint_one() {
        let commander = RecordedCommander::new(&[
            ("codesign -d --verbose=2 -r- /tmp/pin", recorded(AD_HOC)),
            (FIND_IDENTITY, recorded(NO_IDENTITIES)),
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
            run.findings[0].message.starts_with("would mint"),
            "{:?}",
            run.findings
        );
        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
    }

    /// Over SSH there is no dialog to answer, so the password has to come from somewhere or the
    /// run hangs. It is passed only when it was given.
    #[test]
    fn the_keychain_password_is_passed_only_when_there_is_one() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let commander = signing_with_our_own(&pin, recorded(""));
        let scratch = tempfile::tempdir().unwrap();
        let mut context = context(scratch.path());
        context.keychain_password = Some(String::from("hunter2"));
        sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context,
        );
        assert!(
            commander.called_with("-k hunter2"),
            "{:?}",
            commander.calls()
        );
    }

    /// A run that signs with our own certificate, with the partition list answering `partition`.
    fn signing_with_our_own(pin: &Path, partition: CommandOutput) -> RecordedCommander {
        RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(ONLY_OURS)),
            ("security set-key-partition-list", partition),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(SELF_SIGNED)),
            ("codesign --verify ", recorded("")),
        ])
    }

    /// The certificate is minted once and signed with for years, so granting the key ACL only on
    /// the run that MINTED left every later run signing with a key nothing had approved. Proven on
    /// a real Mac: the same `codesign` succeeded once the partition list had been run by hand.
    #[test]
    fn the_key_acl_is_granted_before_every_signature_with_our_own_certificate() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        // no bundle on disk and nothing minted in this run: the certificate is simply already in
        // the keychain, which is every run after the first
        let commander = signing_with_our_own(&pin, recorded(""));
        let scratch = tempfile::tempdir().unwrap();
        let run = sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );

        assert!(!commander.called_with("openssl"), "{:?}", commander.calls());
        let calls = commander.calls();
        let partition = commander
            .position_of("security set-key-partition-list")
            .unwrap_or_else(|| panic!("the key ACL was never granted: {:?}", calls));
        let signed = commander.position_of("codesign -s ").unwrap();
        assert!(partition < signed, "granted after it signed: {:?}", calls);
        assert!(
            run.findings
                .iter()
                .any(|finding| finding.status == Status::Changed
                    && finding.message.contains("signed")),
            "{:?}",
            run.findings
        );
    }

    /// And it is NOT written for a certificate we did not create. An Apple certificate comes with
    /// its own ACL, and rewriting the partition list of someone else's key is not doctor's job.
    #[test]
    fn the_key_acl_is_left_alone_on_an_apple_certificate() {
        let directory = tempfile::tempdir().unwrap();
        let pin = directory.path().join("zellij");
        std::fs::write(&pin, b"the working copy").unwrap();
        let commander = RecordedCommander::new(&[
            (
                format!("codesign -d --verbose=2 -r- {}", pin.display()).as_str(),
                recorded(AD_HOC),
            ),
            (FIND_IDENTITY, recorded(TWO_IDENTITIES)),
            ("codesign -s ", recorded("")),
            ("codesign -d --verbose=2 -r- ", recorded(DEVELOPER_ID)),
            ("codesign --verify ", recorded("")),
        ]);
        let scratch = tempfile::tempdir().unwrap();
        sign_pin(
            &commander,
            &pin,
            DoctorMode {
                fix: true,
                ..DoctorMode::default()
            },
            &context(scratch.path()),
        );
        assert!(
            !commander.called_with("security set-key-partition-list"),
            "{:?}",
            commander.calls()
        );
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
}
