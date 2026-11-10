# When a launcher creates the session

What a login shell supplies that an init-system launcher (launchd, systemd) does not, and what a
generated unit should carry.

Moving session creation out of a shell wrapper and into `zellij session up` surfaced three
separate bugs in one day, and all three had the same shape: **something a login shell had always
supplied, which an init-system launcher does not.** Each stayed invisible for as long as a shell
happened to create the session first, and appeared the moment the launcher won the race.

- **`TERM`.** launchd and systemd give a unit none. The server hands its own environment to every
  pane shell, so a launcher-created session had `TERM=dumb` in every pane — keystrokes repeating,
  `TERM environment variable not set`.
- **systemd `KillMode`.** `session up` daemonizes and returns, so the default `control-group` mode
  tore down the cgroup when the oneshot deactivated, killing the server it had just started.
  `KillMode=process` fixes it. launchd has no equivalent reaping.
- **A launch agent under a different name.** The macOS domain guard looked up the label it would
  itself have installed, so it could not see a hand-written agent doing the same job, and fell
  through to creating a Background-domain session — the exact outcome it exists to prevent.

The rule that predicts the first one, and is worth applying to anything similar. It has two
halves, and the second is easy to miss:

> **What a shell re-derives.** A pane shell sources the rc chain, so it rebuilds most of its own
> environment — `LANG`, `EDITOR`, and its own `PATH`. Verified: absent from the launcher's
> environment, present in an interactive pane. These self-heal, and a launcher should not hold an
> opinion about them.
>
> **What nothing re-derives.** Anything describing the *connection* rather than the user's
> preferences. `TERM` is the clear case. The socket directory was the other, which is why it is
> now resolved inside the binary instead of exported.
>
> **What the binary resolves once, for itself.** This is the half that looks like the first and
> behaves like the second. `consts.rs` resolves the cache, session-info, plugin-artifact and
> state/snapshot directories from `XDG_*` **in whatever environment the binary was launched in**,
> and nothing ever re-derives them. A pane shell fixing its own `XDG_CACHE_HOME` does not move
> the directory the *server* already chose.

That third case is worth spelling out because its symptoms point away from the cause. When a
launcher's `XDG_*` differs from a login shell's — and it does; a systemd user manager is seeded by
PAM long before any rc file runs — plugin permission grants are written to a `permissions.kdl` no
other invocation reads, so plugins re-prompt after every restart; `list-sessions` reads a stale
`session_info`; and `snapshot list` looks in a different archive from the one `session down` wrote.
Each of those reads as a bug in the feature, not as an environment split.

The same trap applies to `PATH` in a way the first half hides. The server resolves commands
against **its own** `PATH`, fixed at creation, so a layout `command` pane, `zellij run --` and
`copy_command` all fail with "command not found" in a launcher-created session while an
interactive pane beside them works — because the rc chain fixed the *shell's* `PATH` and never
touched the server's.

So a generated unit should hold **no opinion about the environment except the values that cannot
be re-derived, plus the ones the binary resolves for itself.** `TERM` earns its place; so do
`PATH` and anything feeding a directory in `consts.rs`. A hardcoded locale does not.

The general lesson for the next change of this kind: when moving work from a shell into the
binary, do not only test the path where a shell is still involved. Test the path where the
launcher is the sole creator — reboot-like conditions — because that is where the assumptions
that were silently carried by the shell all fail at once.
