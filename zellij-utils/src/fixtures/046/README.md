# Golden JSON from the 0.46.0 fork build

`action list-tree --json`, `action watch --json` and `ls --json`, written by a quick-profile build
of this tree in a detached scratch session, as the nkmk40 fixtures were made. The session held a
plain bash pane with no OSC 133 that ran a command for over ten seconds (attention `Done`,
`command_state` `done` from the foreground-process tracker), an OSC 133 bash left after `133;D;3`,
one running a command that set its title, one with a `done` report and input typed after its
prompt, a stand-in `claude` that reported `working`, then `blocked` beside a working subagent
record, then `done`, a stacked pane, a floating pane, a command pane that exited 3, and a second
tab that was renamed and had a pane closed. The watch sample runs from `hello` to the agent's
pane being closed. Paths, the host name and the user name were replaced with placeholders.

`src/golden_json_keys.rs` holds these to `../nkmk40/expected-keys.txt`: every nkmk.40 key path is
still written, and none is retyped.
