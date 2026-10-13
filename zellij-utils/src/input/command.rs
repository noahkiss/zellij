//! Trigger a command
use crate::data::{Direction, OriginatingPlugin};
use serde::{Deserialize, Serialize};
use std::{env, path::PathBuf};

#[derive(Debug, Clone)]
pub enum TerminalAction {
    OpenFile(OpenFilePayload),
    RunCommand(RunCommand),
}

impl TerminalAction {
    pub fn change_cwd(&mut self, new_cwd: PathBuf) {
        match self {
            TerminalAction::OpenFile(open_file_payload) => {
                open_file_payload.cwd = Some(new_cwd);
            },
            TerminalAction::RunCommand(run_command) => {
                run_command.cwd = Some(new_cwd);
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct OpenFilePayload {
    pub path: PathBuf,
    pub line_number: Option<usize>,
    pub cwd: Option<PathBuf>,
    pub originating_plugin: Option<OriginatingPlugin>,
}

impl Default for OpenFilePayload {
    fn default() -> Self {
        OpenFilePayload {
            path: PathBuf::new(),
            line_number: None,
            cwd: None,
            originating_plugin: None,
        }
    }
}

impl OpenFilePayload {
    pub fn new(path: PathBuf, line_number: Option<usize>, cwd: Option<PathBuf>) -> Self {
        OpenFilePayload {
            path,
            line_number,
            cwd,
            originating_plugin: None,
        }
    }
    pub fn with_originating_plugin(mut self, originating_plugin: OriginatingPlugin) -> Self {
        self.originating_plugin = Some(originating_plugin);
        self
    }
}

#[derive(Clone, Debug, Deserialize, Default, Serialize, Eq)]
pub struct RunCommand {
    #[serde(alias = "cmd")]
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub hold_on_close: bool,
    #[serde(default)]
    pub hold_on_start: bool,
    #[serde(default)]
    pub originating_plugin: Option<OriginatingPlugin>,
    #[serde(default)]
    pub use_terminal_title: bool,
    /// fork addition: this command came back with a session, rather than out of a layout a person
    /// wrote. A pane running it drops to the shell when the command exits cleanly, instead of
    /// holding the exit banner and waiting for the ESC that offers the same thing.
    ///
    /// Provenance, not identity: it is set once, when the resurrection layout is loaded (see
    /// `CliAssets::load_config_and_layout`), and it is `#[serde(skip)]` so it never reaches disk,
    /// the plugin API or the client/server contract.
    #[serde(skip)]
    pub resurrected: bool,
}

/// fork addition: `PartialEq` is hand-written so that `resurrected` stays out of it.
///
/// The flag says where a command came from, not what it runs, and the tree compares `RunCommand`s
/// - and `Option<Run>`s built from them - to decide whether a pane already running something is
/// the pane a layout means. A resurrected pane must keep matching the same layout entry it always
/// did, so the flag is excluded and every existing comparison answers exactly as before.
///
/// The fields are destructured exhaustively on purpose: a field added upstream fails to compile
/// here rather than dropping silently out of equality.
impl PartialEq for RunCommand {
    fn eq(&self, other: &Self) -> bool {
        let RunCommand {
            command,
            args,
            cwd,
            hold_on_close,
            hold_on_start,
            originating_plugin,
            use_terminal_title,
            resurrected: _,
        } = self;
        command == &other.command
            && args == &other.args
            && cwd == &other.cwd
            && hold_on_close == &other.hold_on_close
            && hold_on_start == &other.hold_on_start
            && originating_plugin == &other.originating_plugin
            && use_terminal_title == &other.use_terminal_title
    }
}

impl std::fmt::Display for RunCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut command: String = self
            .command
            .as_path()
            .as_os_str()
            .to_string_lossy()
            .to_string();
        for arg in &self.args {
            command.push(' ');
            command.push_str(arg);
        }
        write!(f, "{}", command)
    }
}

/// Intermediate representation
#[derive(Clone, Debug, Deserialize, Default, Serialize, PartialEq, Eq)]
pub struct RunCommandAction {
    #[serde(rename = "cmd")]
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub direction: Option<Direction>,
    #[serde(default)]
    pub hold_on_close: bool,
    #[serde(default)]
    pub hold_on_start: bool,
    #[serde(default)]
    pub originating_plugin: Option<OriginatingPlugin>,
    #[serde(default)]
    pub use_terminal_title: bool,
}

impl From<RunCommandAction> for RunCommand {
    fn from(action: RunCommandAction) -> Self {
        RunCommand {
            command: action.command,
            args: action.args,
            cwd: action.cwd,
            hold_on_close: action.hold_on_close,
            hold_on_start: action.hold_on_start,
            originating_plugin: action.originating_plugin,
            use_terminal_title: action.use_terminal_title,
            resurrected: false,
        }
    }
}

impl From<RunCommand> for RunCommandAction {
    fn from(run_command: RunCommand) -> Self {
        RunCommandAction {
            command: run_command.command,
            args: run_command.args,
            cwd: run_command.cwd,
            direction: None,
            hold_on_close: run_command.hold_on_close,
            hold_on_start: run_command.hold_on_start,
            originating_plugin: run_command.originating_plugin,
            use_terminal_title: run_command.use_terminal_title,
        }
    }
}

impl RunCommandAction {
    pub fn new(mut command: Vec<String>) -> Self {
        if command.is_empty() {
            Default::default()
        } else {
            RunCommandAction {
                command: PathBuf::from(command.remove(0)),
                args: command,
                ..Default::default()
            }
        }
    }
    pub fn populate_originating_plugin(&mut self, originating_plugin: OriginatingPlugin) {
        self.originating_plugin = Some(originating_plugin);
    }
}

impl RunCommand {
    pub fn new(command: PathBuf) -> Self {
        RunCommand {
            command,
            ..Default::default()
        }
    }
    pub fn with_cwd(mut self, cwd: PathBuf) -> Self {
        self.cwd = Some(cwd);
        self
    }
}

// this is a utility method to separate the arguments from a pathbuf before we turn it into a
// Command. eg. "/usr/bin/vim -e" ==> "/usr/bin/vim" + "-e" (the latter will be pushed to args)
fn separate_command_arguments(command: &mut PathBuf, args: &mut Vec<String>) {
    let mut parts = vec![];
    let mut current_part = String::new();
    for part in command.display().to_string().split_ascii_whitespace() {
        current_part.push_str(part);
        if current_part.ends_with('\\') {
            let _ = current_part.pop();
            current_part.push(' ');
        } else {
            let current_part = std::mem::replace(&mut current_part, String::new());
            parts.push(current_part);
        }
    }
    if !parts.is_empty() {
        *command = PathBuf::from(parts.remove(0));
        args.append(&mut parts);
    }
}

impl OpenFilePayload {
    /// Build the editor command and its optional fallback without a line argument.
    pub fn editor_command(
        self,
        default_editor: Option<PathBuf>,
    ) -> (RunCommand, Option<RunCommand>) {
        let mut payload = self;
        let mut failover_cmd_args = None;
        if payload.path.is_relative() {
            if let Some(cwd) = payload.cwd.as_ref() {
                payload.path = cwd.join(payload.path);
            }
        }
        let mut command = default_editor.unwrap_or_else(|| {
            PathBuf::from(
                env::var("EDITOR")
                    .unwrap_or_else(|_| env::var("VISUAL").unwrap_or_else(|_| "vi".into())),
            )
        });

        let mut args = vec![];

        if !command.is_dir() {
            separate_command_arguments(&mut command, &mut args);
        }
        let file_to_open = payload
            .path
            .into_os_string()
            .into_string()
            .expect("Not valid Utf8 Encoding");
        if let Some(line_number) = payload.line_number {
            if command.ends_with("vim")
                || command.ends_with("nvim")
                || command.ends_with("emacs")
                || command.ends_with("nano")
                || command.ends_with("kak")
            {
                failover_cmd_args = Some(vec![file_to_open.clone()]);
                args.push(format!("+{}", line_number));
                args.push(file_to_open);
            } else if command.ends_with("hx") || command.ends_with("helix") {
                // at the time of writing, helix only supports this syntax
                // and it might be a good idea to leave this here anyway
                // to keep supporting old versions
                args.push(format!("{}:{}", file_to_open, line_number));
            } else {
                args.push(file_to_open);
            }
        } else {
            args.push(file_to_open);
        }
        let cmd = RunCommand {
            command,
            args,
            cwd: payload.cwd,
            hold_on_close: false,
            hold_on_start: false,
            ..Default::default()
        };
        let failover_cmd = failover_cmd_args.map(|args| {
            let mut failover = cmd.clone();
            failover.args = args;
            failover
        });
        (cmd, failover_cmd)
    }
}

#[cfg(test)]
mod editor_command_tests {
    use super::*;

    #[test]
    fn vim_line_and_fallback_match_open_file_commands() {
        let payload = OpenFilePayload::new("notes.txt".into(), Some(8), Some("/project".into()));
        let (command, fallback) = payload.editor_command(Some("vim -R".into()));
        assert_eq!(command.command, PathBuf::from("vim"));
        assert_eq!(command.args, ["-R", "+8", "/project/notes.txt"]);
        assert_eq!(fallback.unwrap().args, ["/project/notes.txt"]);
    }

    #[test]
    fn helix_uses_file_colon_line() {
        let payload = OpenFilePayload::new("/project/notes.txt".into(), Some(8), None);
        let (command, fallback) = payload.editor_command(Some("hx".into()));
        assert_eq!(command.args, ["/project/notes.txt:8"]);
        assert!(fallback.is_none());
    }

    #[test]
    fn other_editors_ignore_line_and_keep_arguments() {
        let payload = OpenFilePayload::new("/project/notes.txt".into(), Some(8), None);
        let (command, fallback) = payload.editor_command(Some("code --wait".into()));
        assert_eq!(command.args, ["--wait", "/project/notes.txt"]);
        assert!(fallback.is_none());
    }
}
