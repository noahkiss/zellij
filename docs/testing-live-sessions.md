# Testing against a running zellij

How to run a test server without touching a real session.

**Never run a session-mutating command against a developer's real session.** `kill-session`,
`delete-session`, `new-pane`, `move-tab`, `save-session` and anything under `zellij action` act on
whatever session the environment resolves to. Assume a long-lived session is running and that losing
it is unacceptable.

Isolate every test run. Point the whole environment at a scratch directory and use a throwaway
session name:

```sh
export ZELLIJ_SOCKET_DIR=/tmp/zj-test/sock    # sockets, and therefore which server you reach
export ZELLIJ_CONFIG_DIR=/tmp/zj-test/config  # config.kdl, and the key permission grants use
export XDG_CACHE_HOME=/tmp/zj-test/cache      # session_info, plugin artifacts, permissions.kdl
export XDG_STATE_HOME=/tmp/zj-test/state      # the snapshot archive
```

`ZELLIJ_SOCKET_DIR` and `ZELLIJ_CONFIG_DIR` are read directly, and the fork reads `XDG_STATE_HOME`
by hand, so those three work on every platform. **`XDG_CACHE_HOME` does not.** The cache comes from
the `directories` crate, which honours the XDG variables on Linux and ignores them on macOS — so on
macOS the cache stays in the user's real one and only the session name keeps a test apart from live
state.

**So never run `delete-all-sessions` on macOS, scratch environment or not.** It does not read the
session list you set up. `delete_all_sessions` (`src/commands.rs:410`) takes its targets from
`scan_session_list_default_dirs` and `get_resurrectable_sessions`, both of which walk
`ZELLIJ_CACHE_DIR` — a `directories` crate path that ignores `XDG_CACHE_HOME` on macOS. The scratch
export moves nothing, so the command enumerates and deletes the real user's serialized sessions, and
`--force` kills the live ones with them. Delete scratch sessions by name instead:

```sh
zellij delete-session <throwaway-name> --force --yes
```

Then kill the sessions and confirm the servers are gone when you finish.

**Clear the agent's own identity before you start a scratch server from inside an agent.** A
server hands its environment to every pane it creates, and an agent's shell carries
`AGENT_SESSION_ID`, `AGENT_KIND` and the harness's own variables (`CLAUDE_CODE_*`,
`CLAUDECODE`, and the like). Started from there, the scratch server reports the calling agent's
session on every pane, and a test of agent detection passes or fails for the wrong reason:

```sh
env -u AGENT_SESSION_ID -u AGENT_KIND -u CLAUDECODE -u CLAUDE_CODE_SESSION_ID \
  zellij --session <throwaway-name>
```

`session_restart_drop_env` drops named variables when a session is created, but only those the
config names, and a scratch config may name none.

**The scratch path must be short.** A unix socket path maxes out near 104 bytes, and the session
socket sits several directories below `ZELLIJ_SOCKET_DIR`. A deep path fails with `IPC socket path
is too long` and the failure does not name the cause. Use something like `/tmp/zj-test`, not a path
under a home directory or a per-session temporary directory.

## Pitfalls in a detached session

A **fully detached** session — one no client has ever attached to — behaves differently, and this is
where automated tests usually go wrong:

- **Geometry is real, but sized for the terminal that is not there.** The layout pass does run, for
  the session's own tabs and for every tab a command makes afterwards. `pane_x`, `pane_y`,
  `pane_rows`, `pane_columns` and the stack fields are self-consistent, and tiled panes do not
  overlap. The viewport is the session's own size: a **50×50 default** for a session started
  detached, or **the size of the last client to leave** for one that has been attached to. So the
  numbers are right relative to each other, and right relative to the screen a client will bring
  only if that client brings the same terminal back. Verified on 0.45.0-nkmk.6: two tiled panes at
  `x=0` and `x=25`, 25 columns each, in a 48-row viewport; a stacked pair carried `stack_id=0` with
  `index_in_stack` 0 and 1.
- **A stack has no anchor.** `new-pane --stacked` has no focused pane to stack under, so it needs an
  explicit target: `ZELLIJ_PANE_ID=<id> zellij -s <name> action new-pane --stacked
  --near-current-pane`. Without one it fails loudly with a non-zero exit.
- **A focus-dependent verb refuses; it does not guess.** With no client attached there is no focused
  tab or pane, so `move-tab` without `--tab-id`, `focus-next-pane` and a bare `go-to-tab-name` each
  print what is missing, exit 2, and change nothing. Pass an explicit target, or attach first.
  **Creating is not focus-dependent**: `new-tab`, `new-tab --layout`, `new-pane --new-tab` and
  `go-to-tab-name --create` all work here and make a real tab.

`dump-screen` reads fresh content, but it is a focus-dependent verb too: pass `--pane-id` and it
returns the grid for a pane in a non-focused tab of a detached session, because the grid is
maintained from the pty whether or not the pane renders. Without one it exits 2 and lists the panes.
