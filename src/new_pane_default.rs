use std::path::PathBuf;
use zellij_utils::{
    cli::CliAction,
    input::{actions::Action, command::OpenFilePayload, config::Config},
};

/// Convert an unplaced edit into a command in the caller's stack.
pub(crate) fn stacked_edit(
    action: CliAction,
    config: Option<&Config>,
    current_dir: PathBuf,
) -> Result<Action, String> {
    let CliAction::Edit {
        mut file,
        line_number,
        cwd,
        no_focus,
        ..
    } = action
    else {
        return Err("Expected an edit action".into());
    };
    let cwd = cwd.map(|cwd| current_dir.join(cwd)).unwrap_or(current_dir);
    if file.is_relative() {
        file = cwd.join(file);
    }
    let editor = config.and_then(|config| config.options.scrollback_editor.clone());
    let pane_name = Some(format!("Editing: {}", file.display()));
    let (command, _) = OpenFilePayload::new(file, line_number, Some(cwd)).editor_command(editor);
    Ok(Action::NewStackedPane {
        command: Some(command.into()),
        pane_name,
        near_current_pane: true,
        no_focus,
        tab_id: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use zellij_utils::cli::ReadyFlags;

    #[test]
    fn stacked_edit_preserves_editor_line_cwd_and_focus() {
        let mut config = Config::default();
        config.options.scrollback_editor = Some(PathBuf::from("nvim -u NONE"));
        let action = CliAction::Edit {
            file: PathBuf::from("notes.txt"),
            direction: None,
            line_number: Some(12),
            floating: false,
            in_place: false,
            close_replaced_pane: false,
            cwd: Some(PathBuf::from("docs")),
            x: None,
            y: None,
            width: None,
            height: None,
            pinned: None,
            near_current_pane: false,
            no_focus: true,
            borderless: None,
            tab_id: None,
            handle: None,
            ready: ReadyFlags::default(),
        };
        let rewritten = stacked_edit(action, Some(&config), PathBuf::from("/project")).unwrap();
        let Action::NewStackedPane {
            command: Some(command),
            near_current_pane,
            no_focus,
            tab_id,
            ..
        } = rewritten
        else {
            panic!("Expected a stacked pane");
        };
        assert_eq!(command.command, PathBuf::from("nvim"));
        assert_eq!(
            command.args,
            ["-u", "NONE", "+12", "/project/docs/notes.txt"]
        );
        assert_eq!(command.cwd, Some(PathBuf::from("/project/docs")));
        assert!(!command.hold_on_close);
        assert!(!command.hold_on_start);
        assert!(near_current_pane);
        assert!(no_focus);
        assert_eq!(tab_id, None);
    }
}
