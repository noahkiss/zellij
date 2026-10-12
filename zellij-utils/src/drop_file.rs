//! `zellij action drop-file`: a file handed to a pane by reference.
//!
//! A front end - a browser, an iPad terminal, a script on another machine - has a file and a pane.
//! The pane's program can only read files on the machine the session runs on, so the file is
//! stored there first, and then the pane is handed the reference its program understands: `@path`
//! for a coding agent that reads `@` references, a plain path for one that does not, and a
//! shell-quoted path at a shell. The reference is pasted, never submitted.
//!
//! The store follows go-for-flight's upload store, so the two front ends share one contract: a
//! collision-safe name that keeps only a safe extension, a 25 MiB ceiling, a write to a temporary
//! name that is renamed into place only once it is whole, and anything older than seven days pruned
//! on every call, with no timer.

use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::consts::ZELLIJ_CACHE_DIR;

/// The largest file a drop accepts, in bytes: 25 MiB, the same ceiling as go-for-flight's uploads.
pub const MAX_DROP_BYTES: u64 = 25 * 1024 * 1024;

/// A drop older than this is deleted by the next call. Seven days, as in go-for-flight.
pub const MAX_DROP_AGE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Every stored drop's name starts with this. The prune deletes nothing else, so a store directory
/// that resolves somewhere unexpected cannot turn the prune into a delete of someone's files.
pub const DROP_PREFIX: &str = "drop-";

/// Where drops are stored: `drops/` in the zellij cache directory.
///
/// That is `$XDG_CACHE_HOME/zellij/drops` on Linux. On macOS the cache directory comes from the
/// `directories` crate, which ignores `XDG_CACHE_HOME`, so it is `~/Library/Caches/<zellij's
/// bundle id>/drops`.
pub fn drops_dir() -> PathBuf {
    ZELLIJ_CACHE_DIR.join("drops")
}

/// Where the bytes of a drop come from.
pub enum DropSource {
    /// A file on this machine.
    Path(PathBuf),
    /// Whatever arrives on stdin, to EOF.
    Stdin,
}

/// Why a drop was not stored.
#[derive(Debug, PartialEq, Eq)]
pub enum DropError {
    /// Nothing arrived. A well-formed request that changed nothing: exit 2.
    Empty,
    /// More than [`MAX_DROP_BYTES`] arrived: exit 1.
    TooLarge,
    /// The source cannot be read, or the store cannot be written: exit 1.
    Failed(String),
}

impl DropError {
    /// The process exit status for this refusal, per the CLI output convention.
    pub fn exit_status(&self) -> i32 {
        match self {
            DropError::Empty => 2,
            DropError::TooLarge | DropError::Failed(_) => 1,
        }
    }

    /// The sentence printed on stderr.
    pub fn message(&self) -> String {
        match self {
            DropError::Empty => "The file is empty, so nothing was stored or pasted.".to_owned(),
            DropError::TooLarge => format!(
                "The file is larger than {} bytes (25 MiB), so nothing was stored or pasted.",
                MAX_DROP_BYTES
            ),
            DropError::Failed(message) => message.clone(),
        }
    }
}

/// A stored drop: its absolute path and its size.
#[derive(Debug, PartialEq, Eq)]
pub struct StoredDrop {
    pub path: PathBuf,
    pub bytes: u64,
}

/// The extension of `name`, kept only when it is safe: one to ten ASCII letters or digits,
/// lower-cased, with its dot. Anything else is dropped, as go-for-flight's `safeExt` does.
pub fn safe_extension(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(|base| base.to_str())
        .unwrap_or("");
    match base.rfind('.') {
        // a leading dot is a hidden file's name, not an extension
        Some(dot) if dot > 0 => {
            let ext = &base[dot + 1..];
            if (1..=10).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                format!(".{}", ext.to_ascii_lowercase())
            } else {
                String::new()
            }
        },
        _ => String::new(),
    }
}

/// A fresh, collision-safe file name: the prefix, a v4 uuid, and the safe extension of `name`.
pub fn drop_file_name(name: Option<&str>) -> String {
    format!(
        "{}{}{}",
        DROP_PREFIX,
        uuid::Uuid::new_v4(),
        name.map(safe_extension).unwrap_or_default()
    )
}

/// Deletes the drops in `dir` last modified longer ago than `max_age`. Best effort: a file that
/// cannot be read or deleted is left where it is. Returns how many were deleted.
///
/// Only regular files whose name starts with [`DROP_PREFIX`] are considered, which includes a
/// `.part` file a crashed call left behind.
pub fn prune_drops(dir: &Path, max_age: Duration, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut pruned = 0;
    for entry in entries.flatten() {
        let is_drop = entry
            .file_name()
            .to_str()
            .map(|name| name.starts_with(DROP_PREFIX))
            .unwrap_or(false);
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !is_drop || !metadata.is_file() {
            continue;
        }
        let old = metadata
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .map(|age| age > max_age)
            .unwrap_or(false);
        if old && fs::remove_file(entry.path()).is_ok() {
            pruned += 1;
        }
    }
    pruned
}

/// Stores a drop in `dir` under a fresh name and returns where it went.
///
/// Prunes old drops first, so every call prunes, a refused one included. The bytes are copied to
/// `<name>.part`, counted as they go, and renamed into place only when the count is in bounds: a
/// reader of the final path sees the whole file or nothing. Reading stops one byte past the
/// ceiling, so an oversized stdin is refused without being read to the end.
///
/// `name` supplies the extension. Without it, a [`DropSource::Path`] uses the source's own name.
pub fn store_drop(
    dir: &Path,
    source: DropSource,
    name: Option<&str>,
) -> Result<StoredDrop, DropError> {
    prune_drops(dir, MAX_DROP_AGE, SystemTime::now());
    match source {
        DropSource::Path(path) => {
            let metadata = fs::metadata(&path).map_err(|e| {
                DropError::Failed(format!("Could not read '{}': {}", path.display(), e))
            })?;
            if !metadata.is_file() {
                return Err(DropError::Failed(format!(
                    "'{}' is not a file, so nothing was stored or pasted.",
                    path.display()
                )));
            }
            if metadata.len() > MAX_DROP_BYTES {
                return Err(DropError::TooLarge);
            }
            let file = fs::File::open(&path).map_err(|e| {
                DropError::Failed(format!("Could not read '{}': {}", path.display(), e))
            })?;
            let own_name = path.file_name().and_then(|n| n.to_str()).map(str::to_owned);
            store_from(dir, file, name.or(own_name.as_deref()))
        },
        DropSource::Stdin => store_from(dir, io::stdin().lock(), name),
    }
}

/// The copy-count-rename half of [`store_drop`], over any reader.
pub fn store_from<R: Read>(
    dir: &Path,
    reader: R,
    name: Option<&str>,
) -> Result<StoredDrop, DropError> {
    let failed = |what: &str, e: io::Error| DropError::Failed(format!("Could not {}: {}", what, e));
    fs::create_dir_all(dir).map_err(|e| failed(&format!("create {}", dir.display()), e))?;
    // a drop can be a screenshot of anything; the store is the user's alone
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(dir, fs::Permissions::from_mode(0o700));
    }
    let path = dir.join(drop_file_name(name));
    let mut part_name = path.as_os_str().to_owned();
    part_name.push(".part");
    let part = PathBuf::from(part_name);
    let result = (|| {
        let mut out = open_private(&part).map_err(|e| failed("write the drop", e))?;
        // one byte past the ceiling: a file exactly at it passes, and anything longer is caught
        // without reading the rest of it
        let copied = io::copy(&mut reader.take(MAX_DROP_BYTES + 1), &mut out)
            .map_err(|e| failed("write the drop", e))?;
        if copied == 0 {
            return Err(DropError::Empty);
        }
        if copied > MAX_DROP_BYTES {
            return Err(DropError::TooLarge);
        }
        out.flush().map_err(|e| failed("write the drop", e))?;
        out.sync_all().map_err(|e| failed("write the drop", e))?;
        let written = fs::metadata(&part)
            .map_err(|e| failed("read the drop back", e))?
            .len();
        if written != copied {
            return Err(DropError::Failed(format!(
                "The drop was truncated: {} of {} bytes reached the disk.",
                written, copied
            )));
        }
        fs::rename(&part, &path).map_err(|e| failed("move the drop into place", e))?;
        Ok(copied)
    })();
    match result {
        Ok(bytes) => Ok(StoredDrop { path, bytes }),
        Err(e) => {
            let _ = fs::remove_file(&part);
            Err(e)
        },
    }
}

fn open_private(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// How a pane is handed a stored file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceForm {
    /// `@/abs/path`: claude, opencode and pi read the file this names.
    At,
    /// The bare path: codex, whose `@` only opens its file search.
    Path,
    /// The path quoted for a POSIX shell.
    Shell,
}

impl ReferenceForm {
    /// The form for a pane running `agent_kind` (a `list-agents` KIND), or a shell for `None`.
    ///
    /// An agent kind this build does not know gets the bare path: every harness accepts a path in
    /// its prompt, and a stray `@` could mean anything.
    pub fn for_agent(agent_kind: Option<&str>) -> Self {
        match agent_kind {
            None => ReferenceForm::Shell,
            Some("claude") | Some("opencode") | Some("pi") => ReferenceForm::At,
            Some(_) => ReferenceForm::Path,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            ReferenceForm::At => "at",
            ReferenceForm::Path => "path",
            ReferenceForm::Shell => "shell",
        }
    }
}

/// The reference to `path` in `form`, without the trailing space the paste adds.
///
/// A path with whitespace in it is double-quoted in the two agent forms - `@"/a b/c.png"`, the form
/// pi and claude complete to - and single-quoted at a shell. The store's own names never have any;
/// only the cache directory above them could.
pub fn reference(path: &Path, form: ReferenceForm) -> String {
    let path = path.to_string_lossy();
    let spaced = path.chars().any(char::is_whitespace);
    match form {
        ReferenceForm::At if spaced => format!("@\"{}\"", path),
        ReferenceForm::At => format!("@{}", path),
        ReferenceForm::Path if spaced => format!("\"{}\"", path),
        ReferenceForm::Path => path.into_owned(),
        ReferenceForm::Shell => shell_quote(&path),
    }
}

/// `text` as one POSIX shell word: bare when every character is one no shell treats specially,
/// single-quoted otherwise, with each `'` written as `'\''`.
pub fn shell_quote(text: &str) -> String {
    let plain = !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%=".contains(c));
    if plain {
        text.to_owned()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary directory")
    }

    #[test]
    fn a_safe_extension_is_kept_lower_cased() {
        assert_eq!(safe_extension("Screen Shot.PNG"), ".png");
        assert_eq!(safe_extension("/some/dir/a.tar.gz"), ".gz");
        assert_eq!(safe_extension("photo.jpeg"), ".jpeg");
    }

    #[test]
    fn an_unsafe_or_missing_extension_is_dropped() {
        assert_eq!(safe_extension("README"), "");
        assert_eq!(safe_extension(".bashrc"), "");
        assert_eq!(safe_extension("a.verylongextension"), "");
        assert_eq!(safe_extension("a.p-g"), "");
        assert_eq!(safe_extension("a."), "");
        assert_eq!(safe_extension("x.png/../../etc"), "");
    }

    #[test]
    fn a_drop_name_is_the_prefix_a_uuid_and_the_extension() {
        let name = drop_file_name(Some("shot.png"));
        assert!(name.starts_with(DROP_PREFIX));
        assert!(name.ends_with(".png"));
        assert_eq!(name.len(), DROP_PREFIX.len() + 36 + 4);
        assert_ne!(name, drop_file_name(Some("shot.png")));
        assert_eq!(drop_file_name(None).len(), DROP_PREFIX.len() + 36);
    }

    #[test]
    fn a_drop_is_stored_whole_under_a_fresh_name() {
        let dir = scratch();
        let stored = store_from(dir.path(), &b"hello"[..], Some("note.txt")).unwrap();
        assert_eq!(stored.bytes, 5);
        assert_eq!(stored.path.parent(), Some(dir.path()));
        assert_eq!(fs::read(&stored.path).unwrap(), b"hello");
        assert!(stored.path.to_string_lossy().ends_with(".txt"));
        // no temporary file is left behind
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_drop_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch();
        let store = dir.path().join("drops");
        let stored = store_from(&store, &b"x"[..], None).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&stored.path), 0o600);
        assert_eq!(mode(&store), 0o700);
    }

    #[test]
    fn an_empty_drop_is_refused_and_leaves_nothing() {
        let dir = scratch();
        assert_eq!(
            store_from(dir.path(), &b""[..], Some("a.png")),
            Err(DropError::Empty)
        );
        assert_eq!(DropError::Empty.exit_status(), 2);
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn a_drop_at_the_ceiling_passes_and_one_byte_over_is_refused() {
        let dir = scratch();
        let at = io::repeat(7).take(MAX_DROP_BYTES);
        assert_eq!(
            store_from(dir.path(), at, None).unwrap().bytes,
            MAX_DROP_BYTES
        );
        let over = io::repeat(7).take(MAX_DROP_BYTES + 1);
        assert_eq!(store_from(dir.path(), over, None), Err(DropError::TooLarge));
        assert_eq!(DropError::TooLarge.exit_status(), 1);
        // only the first one is there: the refused one left no temporary file
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let dir = scratch();
        let source = dir.path().join("big.bin");
        let file = fs::File::create(&source).unwrap();
        file.set_len(MAX_DROP_BYTES + 1).unwrap();
        let store = dir.path().join("drops");
        assert_eq!(
            store_drop(&store, DropSource::Path(source), None),
            Err(DropError::TooLarge)
        );
    }

    #[test]
    fn a_path_source_lends_its_extension_unless_a_name_is_given() {
        let dir = scratch();
        let source = dir.path().join("Shot.PNG");
        fs::write(&source, b"png").unwrap();
        let store = dir.path().join("drops");
        let own = store_drop(&store, DropSource::Path(source.clone()), None).unwrap();
        assert!(own.path.to_string_lossy().ends_with(".png"));
        let named = store_drop(&store, DropSource::Path(source), Some("x.jpg")).unwrap();
        assert!(named.path.to_string_lossy().ends_with(".jpg"));
    }

    #[test]
    fn a_directory_is_not_a_file() {
        let dir = scratch();
        let store = dir.path().join("drops");
        match store_drop(&store, DropSource::Path(dir.path().to_owned()), None) {
            Err(DropError::Failed(message)) => assert!(message.contains("is not a file")),
            other => panic!("expected a refusal, got {:?}", other),
        }
    }

    #[test]
    fn the_prune_deletes_old_drops_and_nothing_else() {
        let dir = scratch();
        let now = SystemTime::now();
        let old = now - MAX_DROP_AGE - Duration::from_secs(60);
        let make = |name: &str, modified: SystemTime| {
            let path = dir.path().join(name);
            let file = fs::File::create(&path).unwrap();
            file.set_modified(modified).unwrap();
            path
        };
        let old_drop = make("drop-old.png", old);
        let old_part = make("drop-crashed.png.part", old);
        let fresh_drop = make("drop-fresh.png", now);
        let old_stranger = make("keep-me.png", old);
        fs::create_dir(dir.path().join("drop-a-directory")).unwrap();

        assert_eq!(prune_drops(dir.path(), MAX_DROP_AGE, now), 2);
        assert!(!old_drop.exists());
        assert!(!old_part.exists());
        assert!(fresh_drop.exists());
        assert!(old_stranger.exists());
        assert!(dir.path().join("drop-a-directory").exists());
    }

    #[test]
    fn a_missing_store_prunes_nothing() {
        let dir = scratch();
        assert_eq!(
            prune_drops(&dir.path().join("absent"), MAX_DROP_AGE, SystemTime::now()),
            0
        );
    }

    #[test]
    fn each_agent_gets_the_form_it_reads() {
        assert_eq!(ReferenceForm::for_agent(Some("claude")), ReferenceForm::At);
        assert_eq!(
            ReferenceForm::for_agent(Some("opencode")),
            ReferenceForm::At
        );
        assert_eq!(ReferenceForm::for_agent(Some("pi")), ReferenceForm::At);
        assert_eq!(ReferenceForm::for_agent(Some("codex")), ReferenceForm::Path);
        assert_eq!(
            ReferenceForm::for_agent(Some("some-new-agent")),
            ReferenceForm::Path
        );
        assert_eq!(ReferenceForm::for_agent(None), ReferenceForm::Shell);
    }

    #[test]
    fn references_are_spelled_per_form() {
        let plain = Path::new("/c/zellij/drops/drop-1.png");
        assert_eq!(
            reference(plain, ReferenceForm::At),
            "@/c/zellij/drops/drop-1.png"
        );
        assert_eq!(
            reference(plain, ReferenceForm::Path),
            "/c/zellij/drops/drop-1.png"
        );
        assert_eq!(
            reference(plain, ReferenceForm::Shell),
            "/c/zellij/drops/drop-1.png"
        );
        let spaced = Path::new("/Users/a b/drops/drop-1.png");
        assert_eq!(
            reference(spaced, ReferenceForm::At),
            "@\"/Users/a b/drops/drop-1.png\""
        );
        assert_eq!(
            reference(spaced, ReferenceForm::Path),
            "\"/Users/a b/drops/drop-1.png\""
        );
        assert_eq!(
            reference(spaced, ReferenceForm::Shell),
            "'/Users/a b/drops/drop-1.png'"
        );
    }

    #[test]
    fn a_shell_word_quotes_what_a_shell_would_read() {
        assert_eq!(shell_quote("/tmp/a.png"), "/tmp/a.png");
        assert_eq!(shell_quote("/tmp/it's"), "'/tmp/it'\\''s'");
        assert_eq!(shell_quote("/tmp/$HOME"), "'/tmp/$HOME'");
        assert_eq!(shell_quote("/tmp/*"), "'/tmp/*'");
        assert_eq!(shell_quote(""), "''");
    }
}
