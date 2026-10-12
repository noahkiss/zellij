//! `zellij view-images`, and the `zellij action show` that opens it in a floating pane.
//!
//! The viewer is an ordinary program in a pane. It speaks the kitty graphics protocol to its own
//! pty, and the server - which already parses kitty graphics from any pane and re-sends them to
//! every attached terminal that can draw them - does the rest. So nothing here touches the server,
//! the plugin API or the client/server contract, and an image reaches a terminal on another
//! machine the same way any other pane output does: in the stream.

pub mod decode;
pub mod render;
#[cfg(unix)]
mod terminal;

use std::path::{Path, PathBuf};

use clap::{CommandFactory, FromArgMatches};
use zellij_utils::cli::{CliAction, CliArgs, ImageMode};

/// Draw `files` in this terminal until the person closes the viewer. The exit code: 0 once
/// closed, 1 when this is not a terminal.
pub fn run(files: Vec<PathBuf>, mode: ImageMode) -> i32 {
    #[cfg(unix)]
    {
        terminal::run(files, mode)
    }
    #[cfg(not(unix))]
    {
        let _ = (files, mode);
        eprintln!("view-images needs a unix terminal");
        1
    }
}

/// The absolute path of each file, or the first one that is not a file.
///
/// Checked here, before any pane is made: the viewer would only say so inside a pane that then
/// has to be closed, and a caller would get a pane id for a viewer that has nothing to show.
pub fn resolve_files(files: &[PathBuf], base: &Path) -> Result<Vec<PathBuf>, String> {
    files
        .iter()
        .map(|file| {
            let path = if file.is_absolute() {
                file.clone()
            } else {
                base.join(file)
            };
            if path.is_file() {
                Ok(path)
            } else if path.exists() {
                Err(format!("{} is not a file.", path.display()))
            } else {
                Err(format!("{} does not exist.", path.display()))
            }
        })
        .collect()
}

/// The viewer pane's name: the first file's name, and how many more there are.
pub fn pane_name(files: &[PathBuf]) -> String {
    let first = files
        .first()
        .map(|file| {
            file.file_name()
                .unwrap_or(file.as_os_str())
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_default();
    match files.len() {
        0 | 1 => format!("images: {}", first),
        n => format!("images: {} +{}", first, n - 1),
    }
}

/// The `new-pane` command line that `action show` stands for: a floating pane, closed when the
/// viewer exits, running `exe view-images` over `files`.
pub fn show_argv(exe: &Path, files: &[PathBuf], mode: ImageMode, blocking: bool) -> Vec<String> {
    let mut argv: Vec<String> = [
        "new-pane",
        "--floating",
        "--close-on-exit",
        "--x=10%",
        "--y=10%",
        "--width=80%",
        "--height=80%",
    ]
    .iter()
    .map(|word| (*word).to_owned())
    .collect();
    // a command pane is titled with its command line and ignores the title the program sets, so
    // without a name the frame would show the binary's full path
    argv.push(format!("--name={}", pane_name(files)));
    if blocking {
        argv.push("--blocking".to_owned());
    }
    argv.push("--".to_owned());
    argv.push(exe.to_string_lossy().into_owned());
    argv.push("view-images".to_owned());
    argv.push(format!("--mode={}", mode.as_str()));
    // a file whose name begins with a dash is still a file
    argv.push("--".to_owned());
    argv.extend(files.iter().map(|file| file.to_string_lossy().into_owned()));
    argv
}

/// `action show`, rewritten into the `new-pane` that does it. Parsed by clap from the command line
/// [`show_argv`] builds, so every flag `new-pane` has keeps the default it has there, including
/// the ones added after this was written.
pub fn show_as_new_pane(
    files: &[PathBuf],
    mode: ImageMode,
    blocking: bool,
) -> Result<CliAction, String> {
    let base = std::env::current_dir().map_err(|e| format!("cannot read this directory: {}", e))?;
    let files = resolve_files(files, &base)?;
    let exe = zellij_utils::session_lifecycle::own_exe_for_spawn()
        .map_err(|e| format!("cannot find the zellij binary to run the viewer: {}", e))?;
    new_pane_action(&show_argv(&exe, &files, mode, blocking))
}

/// `zellij action <argv>` as clap reads it.
fn new_pane_action(argv: &[String]) -> Result<CliAction, String> {
    let words = ["zellij", "action"]
        .into_iter()
        .map(String::from)
        .chain(argv.iter().cloned());
    let matches = CliArgs::command()
        .try_get_matches_from(words)
        .map_err(|e| e.to_string())?;
    let action = matches
        .subcommand_matches("action")
        .ok_or_else(|| "`action` did not parse".to_owned())?;
    CliAction::from_arg_matches(action).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// clap builds the whole command tree, which overflows a test thread's default stack in a
    /// build without optimisations.
    fn on_big_stack<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(64 * 1024 * 1024)
            .spawn(f)
            .expect("thread")
            .join()
            .expect("no panic")
    }

    #[test]
    fn a_relative_path_is_taken_from_the_base_and_a_missing_one_is_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("a.png"), b"x").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        assert_eq!(
            resolve_files(&[PathBuf::from("a.png")], dir.path()).expect("a file"),
            vec![dir.path().join("a.png")]
        );
        let missing = resolve_files(
            &[PathBuf::from("a.png"), PathBuf::from("b.png")],
            dir.path(),
        )
        .unwrap_err();
        assert!(missing.ends_with("b.png does not exist."), "{}", missing);
        let not_file = resolve_files(&[PathBuf::from("sub")], dir.path()).unwrap_err();
        assert!(not_file.ends_with("sub is not a file."), "{}", not_file);
    }

    #[test]
    fn show_is_a_floating_pane_that_closes_with_the_viewer() {
        let argv = show_argv(
            Path::new("/opt/zellij"),
            &[PathBuf::from("/s/-odd.png"), PathBuf::from("/s/b.png")],
            ImageMode::Blocks,
            false,
        );
        assert_eq!(
            argv.join(" "),
            "new-pane --floating --close-on-exit --x=10% --y=10% --width=80% --height=80% \
             --name=images: -odd.png +1 -- /opt/zellij view-images --mode=blocks -- /s/-odd.png \
             /s/b.png"
        );
        assert_eq!(pane_name(&[PathBuf::from("/s/a.png")]), "images: a.png");
        let blocking = show_argv(
            Path::new("/z"),
            &[PathBuf::from("/a")],
            ImageMode::Auto,
            true,
        );
        assert!(blocking.contains(&"--blocking".to_owned()));
    }

    #[test]
    fn the_command_line_parses_into_the_new_pane_it_describes() {
        let action = on_big_stack(|| {
            new_pane_action(&show_argv(
                Path::new("/opt/zellij"),
                &[PathBuf::from("/s/a.png")],
                ImageMode::Kitty,
                false,
            ))
        })
        .expect("parses");
        match action {
            CliAction::NewPane {
                floating,
                close_on_exit,
                command,
                width,
                height,
                blocking,
                name,
                ..
            } => {
                assert!(floating);
                assert!(close_on_exit);
                assert_eq!(name.as_deref(), Some("images: a.png"));
                assert!(!blocking);
                assert_eq!(width.as_deref(), Some("80%"));
                assert_eq!(height.as_deref(), Some("80%"));
                assert_eq!(
                    command,
                    vec![
                        "/opt/zellij",
                        "view-images",
                        "--mode=kitty",
                        "--",
                        "/s/a.png"
                    ]
                );
            },
            other => panic!("not a new-pane: {:?}", other),
        }
    }

    #[test]
    fn the_viewer_command_line_parses_back_into_the_viewer() {
        // what the pane runs has to be a command this same binary takes
        let parsed = on_big_stack(|| {
            CliArgs::command().try_get_matches_from([
                "zellij",
                "view-images",
                "--mode=blocks",
                "--",
                "-odd.png",
                "b.png",
            ])
        })
        .expect("parses");
        let view = parsed
            .subcommand_matches("view-images")
            .expect("the subcommand");
        let files: Vec<&PathBuf> = view.get_many::<PathBuf>("files").unwrap().collect();
        assert_eq!(
            files,
            vec![&PathBuf::from("-odd.png"), &PathBuf::from("b.png")]
        );
        assert_eq!(
            view.get_one::<ImageMode>("mode").copied(),
            Some(ImageMode::Blocks)
        );
    }
}
