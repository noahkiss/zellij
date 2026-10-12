#[cfg(test)]
#[cfg(feature = "web_server_capability")]
mod terminal_loop_tests;

#[cfg(test)]
mod teardown_tests;

/// A new session's server is started without any agent identity variable, so no pane of the new
/// session inherits the identity of the agent whose pane created it.
#[cfg(test)]
#[test]
fn a_new_server_is_started_without_the_agent_identity_variables() {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    let command = crate::server_command(
        PathBuf::from("/opt/zellij/bin/zellij"),
        Path::new("/tmp/zj/sock"),
        false,
    );
    let removed: Vec<&OsStr> = command
        .get_envs()
        .filter(|(_, value)| value.is_none())
        .map(|(name, _)| name)
        .collect();
    for name in zellij_utils::agent_detect::agent_identity_env_names() {
        assert!(
            removed.contains(&OsStr::new(name)),
            "{} reaches the new server",
            name
        );
    }
    assert!(removed.contains(&OsStr::new("AGENT_SESSION_ID")));
    assert!(removed.contains(&OsStr::new("AGENT_KIND")));
    let args: Vec<&OsStr> = command.get_args().collect();
    assert_eq!(args, [OsStr::new("--server"), OsStr::new("/tmp/zj/sock")]);
}
