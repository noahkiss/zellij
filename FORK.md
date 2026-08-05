# zellij (noahkiss fork)

A personal fork of [zellij](https://github.com/zellij-org/zellij), based on **v0.44.3**, carrying a
small patch queue aimed at the plugin development loop and a few session-lifecycle papercuts.

**This fork is not accepting issues or pull requests, and none of these patches have been submitted
upstream.** If you found it by accident, you want [the real thing](https://github.com/zellij-org/zellij).
Upstream's README, documentation and license apply to everything except the patches below.

## Install

```
brew install noahkiss/tap/zellij-nkmk
```

That pours a prebuilt binary from the fork's GitHub releases (glibc linux x86_64, macOS arm64).
Anywhere else — musl, linux arm64, intel macs — use `noahkiss/tap/zellij-nkmk-source`, which is
the same version built from the tag tarball.

It installs a binary named `zellij`, so it conflicts with the `zellij` formula. Unlink that first:

```
brew unlink zellij
brew install noahkiss/tap/zellij-nkmk
```

Two things can make an install or upgrade fail in ways the output does not explain:

- **`brew upgrade` refuses on a legacy keg.** Early installs landed as keg version `3`, and
  Homebrew compares `3` against `0.44.3-nkmk.4` token by token — `3 > 0`, so it decides the
  installed copy is newer and prints `… 3 already installed` instead of upgrading. Check with
  `brew list --versions zellij-nkmk`: if it reports a bare number, the one-time fix is
  `brew uninstall zellij-nkmk && brew install noahkiss/tap/zellij-nkmk`. Kegs named
  `0.44.3-nkmk.<n>` upgrade normally from then on.
- **Untrusted-tap gate.** Newer Homebrew refuses to load formulae from a third-party tap until
  it is trusted: `brew trust noahkiss/tap` (or `brew trust --formula noahkiss/tap/zellij-nkmk`).

Switching binaries does not migrate running sessions — the old server keeps running under the old
binary until it exits. Restart your sessions to pick up the fork.

Sessions themselves are portable across the swap: sockets are scoped by client/server *contract*
(`$XDG_RUNTIME_DIR/zellij/contract_version_1/`), not by version string, so the fork CLI attaches to
and manages sessions started by a stock build of the same contract, and the reverse.

## Versioning

`<upstream version>-nkmk.<fork counter>`, e.g. `0.44.3-nkmk.1`. `zellij --version` reports the fork
version, so an install can be verified. The counter resets when the upstream base moves.

Because the version keys `$ZELLIJ_CACHE_DIR/<version>`, the fork does not share plugin artifact or
release-note caches with an upstream build of the same version.

## The patch queue

### Plugin hot-reload (`plugin_watch`, default **on**)

Loaded `file:` plugins have their `.wasm` watched; a change reloads every running instance with the
module cache skipped, so a rebuild swaps the code under the pane without touching the CLI. The
containing directory is watched rather than the file, because build tools replace a `.wasm` by
rename, and paths are canonicalized so a plugin loaded through a symlink is matched against the
file that actually changes.

```kdl
plugin_watch false   // to turn it off
```

This is a config-file setting only. It is read by the server at session start, so there is no CLI
flag for it.

### Declarative plugin permissions

```kdl
plugin_permissions {
    "/home/you/.config/zellij/plugins/my_plugin.wasm" {
        ReadApplicationState
        ChangeApplicationState
        MessageAndLaunchOtherPlugins
    }
}
```

Consulted before the interactive prompt and never written back to `permissions.kdl`, so a later
deny cannot prune it. The key is the plugin location as zellij renders it — for a `file:` plugin
that is the bare path; a `file:`-prefixed key is accepted and normalized.

Only explicitly enumerated local paths work. Wildcards and remote URLs are rejected at parse time:
a pre-grant means trusting whatever wasm ever sits at that path, which is precisely the thing the
hot-reload loop keeps changing, so the set of trusted code has to stay something you wrote down.

This also makes background `load_plugins` plugins grantable at all — they have no pane to show a
prompt in.

Separately, `PermissionCache::cache` now merges rather than replaces. Previously any new permission
request dropped every permission granted before it, and a deny wrote an empty list that wiped the
lot.

### Plugin reload no longer wedges

Three bugs in one path: pending state was created before the checks that can refuse a reload (an
orphaned entry then starved the running instance); a plugin parked on an unanswered permission
prompt counted as "currently loading" and silently parked every later reload for that location for
the rest of the session; and a reload required an exact `(location, configuration)` match, so
`start-or-reload-plugin` without a matching `-c` found nothing and spawned a stray pane instead.

### `dump-screen` works on plugin panes

`zellij action dump-screen --pane-id plugin_N`, with or without `--ansi`, returns the pane's content
instead of an empty string.

### `delete-session` no longer leaves a ghost

The delete waits for the server to stop answering before removing the session_info folder, then
sweeps it briefly. Previously the dying server's final snapshot landed after the delete and the
session reappeared in `zellij ls` with a stale layout.

### `delete-session` and `kill-session` wait for the server to actually be gone

```
zellij delete-session my-session --force              # returns when the server is gone
zellij kill-session my-session --wait-timeout 30      # allow longer than the 10s default
zellij delete-session my-session --force --no-wait    # old behaviour: send and return
```

Both commands now block until the server process has exited, and exit **1** with
`session '<name>': server still running after <n>s` on stderr if it has not. `--no-wait` returns as
soon as the server acknowledges the kill. `kill-all-sessions` and `delete-all-sessions` take the
same flags, attempt every session, and exit non-zero if any of them did not go.

The kill itself now awaits the server's acknowledgement instead of being fire-and-forget. The wait
watches the server process where the kernel names it (the socket reports its peer's PID), and the
socket file otherwise; "stopped answering" is no longer accepted as an answer.

Server teardown was fixed to match. A killed server used to be able to linger for minutes still
parenting its pane shells: it dropped its session while holding the write lock every route thread
needs, joined the screen thread (which can block on a channel nobody drains after shutdown starts)
before the pty thread that kills the shells, and hung up each shell alone with a single SIGHUP.
Teardown now drains that channel, releases the lock first, joins pty first, signals each pane's
process **group**, and escalates to SIGKILL after 200ms. A caller that polls `zellij ls` to decide
"it is safe to rebuild now" finally gets a true answer to the question it is asking.

### `zellij ls -s` marks exited sessions

Dead sessions get an `(EXITED)` suffix. The name is still the first whitespace-separated field, so
`cut`/`awk` parsing is unaffected.

### `attach --no-resurrect`

```
zellij attach my-session -c --no-resurrect
```

Builds the session fresh from the layout instead of from whatever shape it had when it died, so
layout edits apply deterministically. The snapshot is discarded rather than ignored — leaving it
would keep the name taken by a dead session.

### Top-only pane frames (`ui { pane_frames { top_only true } }`)

```kdl
ui {
    pane_frames {
        top_only true
    }
}
```

Draws only the top frame line (which runs edge to edge and carries the title); the side and
bottom frame cells stay reserved but blank, so panes keep their 1-cell padding ring without the
box. The resize-hint undertitle is suppressed with it; the held-pane `[ EXIT CODE ]` / re-run
undertitle and hover tooltips still draw. Rides the same live config-reload path as
`rounded_corners`, so toggling it is a config.kdl edit — no restart. Off by default (stock
rendering); plugins never see the flag (it is not part of the protobuf wire format).

### Absolute tab reorder (`move-tab --to-index`)

```
zellij action move-tab --to-index 0 --tab-id 3
zellij action move-tab --to-index 2            # the focused tab
```

Moves a tab to an absolute 0-based position in one call, instead of a run of relative
`move-tab left|right` calls that races anything else changing the session. The relative form is
unchanged and still the positional argument; the two are mutually exclusive. Unlike the relative
form, which swaps the tab with its neighbour, this shifts the tabs in between — dragging a tab from
position 0 to 3 gives `1,2,3,0`, not a 0↔3 swap. An index past the last tab is clamped to the last
position rather than rejected, so a drag past the end lands the tab at the end.

This adds an `Action` and therefore a message to the client/server contract (tag 140), without
bumping the contract version: a fork client talking to a stock server of the same contract simply
gets nothing for this one action, and every other action keeps working.

### `pane_pid` in `list-panes --json`

```
zellij action list-panes --all --json
```

Terminal panes carry `pane_pid`, the pid of the process zellij spawned for the pane. The field is
omitted for plugin panes and for any pane the pty does not answer for, exactly like the neighbouring
`pane_command` and `pane_cwd`. This only exposes what the pty thread already knew — the pid was
reachable from plugins and nowhere else — so consumers no longer have to scan `/proc/*/environ` for
`ZELLIJ_PANE_ID` to map a pane to a process.

`PaneListEntry` is a CLI-only struct, so this is JSON output only: no protobuf, no contract change,
and plugins see nothing new.

### The socket directory is visible, and `ls` warns about sessions outside it

Every session operation is scoped to one socket directory, resolved from the environment
(`$ZELLIJ_SOCKET_DIR`, else `$XDG_RUNTIME_DIR/zellij`, else `$TMPDIR/zellij-<uid>`, else
`/tmp/zellij-<uid>`). Two clients with different environments therefore build their own servers,
see none of each other's sessions, and never error — a session can read `EXITED - attach to
resurrect` while its server is alive under another path. Two additions make that visible:

- `zellij setup --check` prints `[SOCKET DIR]` beside the existing `[CACHE DIR]`.
- `zellij ls` scans the socket roots this build would have resolved to under a different
  environment, and warns on stderr, naming the directory and the sessions, when it finds live ones
  of the current contract version. Silent when there is nothing to report.

The scan is read-only. Unlike the listing of the directory in use, it does not delete a socket that
refuses a connection — that socket belongs to another environment.

### Pane identity and stack membership in `PaneInfo`

```
zellij action list-panes --all --json
```

`PaneInfo` carries four more fields, so a programmatic consumer can tell panes apart and mirror what
a stack looks like on screen.

- `program_title` — the title the program set for itself with OSC 0/2. zellij records that title
  unconditionally, but `title` returns the user-assigned pane name instead as soon as one is set, so
  naming a pane used to make the program's own title unreachable. The two are now reported side by
  side: `title` for display, `program_title` for identity. `null` for plugin panes and for terminals
  whose program never set a title.
- `stack_id` — the id of the stack a tiled pane belongs to, `null` for panes that are not stacked.
- `index_in_stack` — the pane's position in its stack, counted from the top.
- `is_expanded_in_stack` — `true` for the one member of a stack that is not collapsed. A stack's
  expanded member is the one with a flexible height, which is what this tests; the collapsed members
  are pinned to a single row.

Floating and suppressed panes report `null`/`false` for all three stack fields.

This is not the same thing as the neighbouring `index_in_pane_group`, which tracks the multi-select
grouping feature (Ctrl+click marking, `TogglePaneInGroup`) and is empty unless the user has staged
panes for a bulk action. It never carried stack membership, and its behaviour is unchanged here.

The fields cross the plugin API, so `event.proto` and its generated Rust are regenerated
(`cargo xtask build`). The client/server contract has no `PaneInfo` and is untouched.

### The session snapshot archive

Zellij keeps exactly one serialized shape per session name and overwrites it in place, so relaunching
a session under the same name destroys the layout worth keeping within one serialization interval —
and `delete-session`, the operation that most often motivates rebuilding a session, removes it
outright. Snapshots keep a dated history beside the live file.

A snapshot is a directory copy of the session's `session_info` folder plus a `snapshot.kdl` sidecar:

```
<state dir>/zellij/snapshots/<session name>/<epoch ms>-<short id>/
    session-layout.kdl
    session-metadata.kdl
    initial_contents_1 …
    snapshot.kdl
```

The copy is a directory rather than a file because the layout parser resolves the
`initial_contents_<n>` files it references against the layout file's own parent folder, so a
self-contained directory replays through the stock parser unchanged. The sidecar records the session
name, the epoch, the producing zellij and client/server contract versions, why the snapshot was cut,
and the tab and pane counts, so listing needs no layout parse.

One is cut on each of four events:

| Trigger | Reason |
|---|---|
| Graceful server shutdown | `shutdown` |
| `zellij action save-session --archive` | `manual` |
| `zellij delete-session` | `delete` |
| Server start, for any `session_info` folder whose server is gone | `promoted` |

Shutdown serializes once more before archiving, so what it captures is the shape the session had when
it was killed rather than one up to a whole serialization interval old. The startup sweep is the
SIGKILL and crash path, where the periodic file survives and is promoted rather than lost to the next
session of the same name. The periodic serializer itself never touches the archive.

Shutdown and delete both fire on an ordinary teardown, so a copy identical to the newest snapshot for
that name is skipped: `delete-session --force` leaves one snapshot, not two. Sameness is judged on
the layout and pane contents, not on `session-metadata.kdl`, which carries client counts and
timestamps that change on every write.

The archive lives under the **state** directory rather than the cache, because a snapshot is state
and a cache directory is by definition disposable. It also sits outside both the version and
`contract_version_<n>` directories, so neither an upgrade nor a contract bump orphans history.
Resolution order, on every platform:

1. `snapshot_dir "<path>"` in config.kdl
2. `$XDG_STATE_HOME/zellij/snapshots`, if set to an absolute path
3. the platform state directory (`~/.local/state/zellij/snapshots` on Linux)
4. the platform data directory (macOS and Windows, which have no state directory)

`$XDG_STATE_HOME` is checked by hand because the `directories` crate honours the XDG variables on
Linux but ignores them on macOS. Explicit configuration wins; platform convention is the fallback.

```kdl
snapshot_dir "/path/to/snapshots"   // optional, defaults as above
session_snapshot_limit 10           // per session name, oldest pruned first; 0 disables archiving
```

Both are config-file only. Snapshots inherit `serialize_pane_viewport` rather than adding a flag of
their own — with it off (the default) a snapshot is two KDL files.

Bare `save-session` keeps its in-place behaviour; `--archive` is handled by the CLI client after the
existing action returns, so nothing here crosses the client/server contract and plugins see nothing
new.

#### Reading and restoring the archive

```
zellij snapshot list [--session <name>] [--json]
zellij snapshot show <id>
zellij snapshot restore <id|latest> [--session <new-name>]
zellij snapshot rm <id>
zellij snapshot prune [--keep <n>]
zellij attach <name> --restore [<id>|latest]
```

`<id>` is the directory name, and any unique prefix of one is accepted, like a git short SHA.
`latest` means the newest snapshot of the named session for `attach --restore`, and the newest in
the archive for `snapshot restore`; to restore the newest of one session under a different name,
name the session in `attach --restore` or give the id.

Listing reads directories and sidecars only, so it works with no server running and with no session
of that name left. It trial-parses each layout and marks the ones that no longer parse rather than
failing, because a layout a newer binary rejects is still a text file a human can repair, and
finding that out at list time beats finding it out at restore time.

`restore` points the existing resurrection path at the archive directory instead of the cache, so it
is the ordinary new-session path with a different layout file. `--session` restores under another
name, which makes a snapshot a reusable template and lets a restore be rehearsed beside a live
session. Restoring into a name that is currently running is refused; restoring a snapshot written by
a different upstream base is reported and then done anyway.

`--restore` on `attach` is the counterpart to `--no-resurrect`: three explicit behaviours for a dead
name — resurrect from the in-place file (default), start clean (`--no-resurrect`), or rebuild from a
chosen snapshot (`--restore`).

A restored layout runs its recorded commands, exactly as resurrection does today. A snapshot from
weeks ago is a sharper edge than a session that died minutes ago, so `snapshot show` prints the
layout before `restore` acts on it. There is deliberately no confirmation prompt.

#### Adopting layouts the archive never saw

```
zellij snapshot import [--from <dir>] [--dry-run] [--prune-source]
```

Scans the `session_info` directories in the cache that this binary does not use — `$cache/<version>/`
from before 0.44.0 moved session state to contract scoping, and `$cache/contract_version_*/` for the
day upstream bumps the contract — and copies every folder holding a `session-layout.kdl` into the
archive with reason `imported`, stamped with the directory it came from. `--from` takes either a
`session_info` directory or a single session folder. Importing is idempotent: a folder whose shape is
already archived under that session name is skipped, so re-running adds nothing.

Nothing is ever swept automatically. Silently relocating a user's files is the kind of helpfulness
that is indistinguishable from data loss when it goes wrong, so `snapshot list` prints one line when
it sees adoptable layouts and leaves the decision to the user.

The same contract bump that strands a `session_info` directory also makes a running session
unreachable: the socket path is contract-scoped and the wire format genuinely differs, so a
mismatched client attaching to a live server is a protocol violation rather than a path problem.
Attaching to a name that only exists under another contract now says so, and names the way out:

```
No session with the name 'my-session' found!
Session 'my-session' is running under client/server contract 1; this binary speaks 2.
Its layout was captured - rebuild it with:
    zellij snapshot restore latest --session my-session
```

The last two lines appear only when a snapshot for that name exists. Nothing is probed — another
contract's server would not understand the question — so this is a socket-file check, not a
connection.

### `--stacked` fails instead of quietly creating nothing

A stack needs an anchor: either a pane to stack under, or a client whose focused pane the new pane
can join. `zellij action new-pane --stacked` against a session **nobody is attached to** supplies
neither, because there is no focus to read. Upstream logged an internal error, dropped the pane and
returned success — so the CLI printed a terminal id and exited 0 while creating no pane, and the pty
that had already been spawned for it stayed alive as a shell with no pane, for the life of the
session.

Now that case is refused before the pane is built, the orphaned pty is closed, and the reason says
what to do instead:

```
cannot stack a pane: no client is attached and no target pane was given.
Pass --near-current-pane with ZELLIJ_PANE_ID set to the pane to stack under.
```

That invocation is the working one for a detached consumer — it names the pane to stack under
explicitly rather than relying on focus:

```
ZELLIJ_PANE_ID=<id> zellij -s <session> action new-pane --stacked --near-current-pane
```

The message reaches the CLI on stderr with a non-zero exit, `--blocking` or not. That needs saying
because zellij normally reports a new pane as soon as its pty spawns — before the tab has decided
whether it can place it — so a refusal that only the tab knows about would arrive after the CLI had
already exited 0. The refusal is therefore signalled from the screen, where the completion channel
still belongs to the caller, while the tab still does the refusing and closes the pty. Panes that
are placed successfully are unaffected and still return immediately.

Nothing guesses a stack target. Picking some pane to stack under when the caller did not name one
would put a confidently wrong answer where an error belongs.

### Session lifecycle: `zellij session up|down|restart`

```
zellij session up      [NAME] [--restore [ID]]
zellij session down    [NAME] [--wait-timeout SECS]
zellij session restart [NAME] [--fresh | --restore ID]
```

`up` is idempotent, creates the session detached, and then **asserts its own post-condition**:
exactly one server for the name, on the socket this binary resolved, present and listed. It refuses
to create a second server when one already serves the name but fails that check.

The assertion is the point. Launchers used to declare an intended environment and trust it; nothing
verified the result, so a launcher whose environment differed from a login shell's did not create a
misplaced session but an **invisible** one, and the next client silently built a second server. The
diagnostics name every server on the machine, other contract versions, and other socket
directories, because the thing that explains a failure is usually a session you cannot see.

`restart` exists because a teardown kills every pane shell in the session, including the one running
the command — so `down && up` typed inside a session never reaches the `up`. It daemonizes first
(the same double-fork the server itself uses), so it outlives the process group that the teardown
signals, then runs both halves. Default restores the shape the teardown archived; `--fresh` comes
back from the layout, which is how a layout edit is applied.

`down` reports success when the session is already gone — you asked for it to be down and it is.
Only a session that exists and cannot be removed is a failure. The top-level `delete-session` and
`kill-session` keep their previous exit codes.

None of these read `ZELLIJ_SOCKET_DIR`. The binary resolves its own socket directory, so there is no
environment variable for a launcher to get wrong, and none for a long-lived shell to hold a stale
copy of.

### `zellij setup --generate-service <systemd|launchd>`

Writes a user-level systemd unit or launchd plist whose only job is to call `zellij session up`.
Supervision belongs to the init system; session correctness belongs to the binary.

Two things the generated files deliberately do:

- **They name a stable binary path.** `current_exe()` resolves symlinks, so on a package manager
  with a versioned install prefix it yields a path that disappears on the next upgrade. The
  generator prefers a `PATH` entry whose canonical target is this binary — the stable symlink —
  falling back with a warning, and `--exe` overrides. On macOS this also matters for identity:
  permission grants are recorded against the executable image, so a versioned path re-asks for
  every permission after every upgrade.
- **The plist sets `LimitLoadToSessionType Aqua`**, and its install line bootstraps into `gui/`.
  The bootstrap target is what actually puts the job in the graphical login session — a job
  bootstrapped into `gui/` reports the Aqua domain with or without the key. The key restricts which
  session types the job may auto-load into, so at login it cannot come up anywhere else. See below.

They deliberately set no `TMPDIR` and no `ZELLIJ_SOCKET_DIR`, and no `ProcessType` — that last one
is a throttling hint, and panes inherit the server's QoS.

What they *do* set is the short list of things a launcher supplies to nobody and a pane cannot
re-derive. Every one is a **default**: a key or directive of the same name in the config's
`session_service` block replaces it, and is never written beside it.

| | systemd | launchd | why |
|---|---|---|---|
| `TERM` | `Environment=TERM=` | in `EnvironmentVariables` | a unit has none; every pane would be `dumb` |
| `PATH` | `Environment=PATH=` | in `EnvironmentVariables` | see below |
| working directory | — (defaults to `$HOME`) | `WorkingDirectory` | launchd gives none, so panes open in `/` |
| job output | — (stderr is the journal) | `StandardOutPath`, `StandardErrorPath` | launchd sends it to `/dev/null` |

**`PATH` was asymmetric, and the symptom is not the obvious one.** The plist pinned a
Homebrew-shaped `PATH` and the unit pinned none at all. The **server** resolves a layout `command`,
a `zellij run --`, a `zellij edit` and a `copy_command` against its own `PATH` — once, fixed for the
life of the session — so in a launcher-created session on Linux those failed with "Command not
found" while an interactive pane in the same session worked: the rc chain had fixed the *shell's*
`PATH`, not the server's. Both generators now write one, derived rather than hardcoded: the
directory the unit's own binary was found in, then the platform default. A package-manager prefix
arrives that way on its own.

**The launchd log paths are what the whole design rests on.** `session up` asserts the session it
created is really there and prints why when it is not; launchd sends the output of a job naming no
path to `/dev/null`, so on a Mac a session that never came back after login left no evidence
anywhere. They default to `$XDG_STATE_HOME/zellij/session-<name>.{out,err}.log` — the same state
directory as the restart log and the snapshot archive, per-user, and surviving the reboot the log is
about. `session enable` creates that directory before loading the job, because launchd will not, and
a job whose stdout cannot be opened does not run.

### `session up` will not create a session in the wrong macOS session domain

macOS puts every process in a session domain, and only the graphical (`Aqua`) one carries the
context for TCC-gated resources, the login keychain, the pasteboard and notifications. A process
cannot ask for that context: it is conferred by the domain a job is loaded into, inherited by
children, fixed when the server is created, and **never changed by attaching later**.

That turns an idempotent `up` into a race. Whoever creates the session first wins the domain
permanently, and a connection over SSH runs in the `Background` domain — so connecting over SSH
before the launchd agent has run leaves a session that can never reach those resources, with
nothing reporting it.

So on macOS, when the current domain is not `Aqua` and the session does not exist, `up` asks
launchd to start the job in the graphical domain rather than creating the session itself, then
waits for the socket and asserts as usual. With no job installed it creates the session and warns
that GUI-gated access will be unavailable for the life of the server. With no graphical session at
all it says so instead of quietly creating a crippled one. Linux has no analogue and is unaffected.

### The installed job is found by what it runs, not by what it is called

The guard above, and `session status`, first asked the init system for the name **this build**
would have installed — `dev.zellij.session.<name>`, `zellij-session-<name>.service`. On a machine
whose agent was written by hand, or by anything older than these commands, the name is whatever its
author chose. The lookup missed it and reported "no agent installed" while the agent was installed,
loaded, and doing the job. That is not cosmetic: the guard exists to stop a permanently crippled
session being created, and a guard that cannot see the job falls through to creating one — for
everybody whose agent it did not install itself. Found on a real machine, where `session restart`
over SSH created a `Background`-domain session past a loaded agent.

So the job is now identified by what it **does**. Both platforms enumerate the user's own
directory — `~/Library/LaunchAgents/*.plist`, `~/.config/systemd/user/*.service` — read `Label` and
`ProgramArguments`, or `ExecStart`, and match a job whose arguments run `session up <name>` for this
session. argv[0] is not looked at: a unit may exec zellij through a wrapper, and a renamed or
symlinked build is the same program. The subcommand sequence is the identity. A job for another
session, or one running another subcommand, does not match.

The files are read rather than the init system asked, and for launchd that is deliberate:
`launchctl list` gives labels with no command line, so the command would need one `launchctl print`
per label — hundreds of subprocesses, over output whose format is undocumented and differs between
releases. A plist holds both keys in a documented format. Whether launchd currently *holds* the job
is a separate question, asked by label afterwards, so nothing rests on the file being the whole
truth; and the derived label is still tried when the scan finds nothing.

An exact name match wins, so an install this build wrote is never reported as an oddity. One match
under another name is used, and named in the output. Several matches are all named before one is
acted on, rather than one being picked in silence. `status` reports the job it found instead of
calling the file missing:

```
session   my-session
init      launchd (user)
agent     ~/Library/LaunchAgents/dev.zellij.session.my-session.plist - installed under a different name: com.example.my-terminal (~/Library/LaunchAgents/com.example.my-terminal.plist)
loaded    yes (gui/501/com.example.my-terminal)
```

**A command line inside one argument counts too.** The match started as two adjacent argv elements,
which reads a unit that execs zellij directly and misses the commonest hand-written form of all:
`ProgramArguments = ["/bin/sh", "-c", "exec zellij session up my-session"]`, where the whole command
is ONE element. That is exactly the population this scan was written for — an agent predating these
subcommands could not have called them, so it calls something that does. A second pass therefore
reads inside each argument, quotes included, when the windowed match finds nothing.

What it still cannot see is a job whose plist names a wrapper script that reaches `session up`
somewhere inside itself. So the warning says what the scan can support — *no loaded launch agent was
found naming `session up <name>`; one that reaches it through a wrapper may not be recognisable from
its plist* — rather than the stronger claim that no agent runs it whatever its label.

### A pane's shell comes from the passwd entry when nothing exported `SHELL`

Same shape again. A launcher hands the server no `SHELL`, the server hands its own environment to
every pane, and the fallback was `/bin/sh` with a `log::warn!` nobody reads: every pane of a
launcher-created session would be a bare `sh` — no prompt, no aliases, none of the rc chain — while
the session itself looks perfectly up. The user's login shell is written down in the passwd
database, and it is right whether or not anything exported it, so that is consulted (`getpwuid_r`,
via `libc` — this crate builds `nix` without the features that would expose it) before `/bin/sh`.
Only reached when the config sets no `default_shell`, which already answers the question for anyone
who has set one.

### A session created by a launcher gets a usable `TERM`

A launcher — a launch agent, a systemd user unit — is not a login shell and has **no `TERM`**. The
server hands its own environment to every pane shell it spawns, so a session created by one came up
with `TERM=dumb` in every pane: keystrokes repeating, and programs reporting `TERM environment
variable not set.` Measured on two machines, not inferred. It stayed hidden for as long as a login
shell always won the race to create the session, because a shell always has `TERM` — and appeared
the moment the launcher became the creator, which is exactly what `session enable` arranges.

So `session up` sets `TERM=xterm-256color` when what it has is unset, empty, or `dumb`, on the path
that CREATES the session and nowhere else: an `up` that finds the session healthy has already
returned, and `restart` ends in the same function and is covered by it. `dumb` counts as absent
because it is what an environment with no terminal type produces, and it is never what a pane wants.
Any other value is left alone — a real terminal knows what it is better than this does.

The generated units set it too (`Environment=TERM=`, a `TERM` key in `EnvironmentVariables`). Belt
and braces: it makes the unit correct on its own terms, and it is visible to whoever reads the unit,
which the in-binary default is not. Unlike `ExecStart` and the label, this one is a **default**: a
`session_service` entry setting `TERM` replaces it rather than joining it — a plist dictionary
cannot carry one key twice, and two systemd assignments of one variable are a unit nobody can read
with confidence. On launchd the config's `TERM` goes inside `EnvironmentVariables`, where it means
something, rather than beside it as a top-level key launchd would ignore.

### `COLORTERM`, and the `ZELLIJ_*` variables, on the same creating path

Three more corrections in the one place `up` builds a session's environment, all of them the same
rule as `TERM`: what is set there is set in every pane for the life of the session, and only the
facts a pane's rc chain **cannot re-derive** belong there.

- **`COLORTERM=truecolor`** when nothing set it. This is not a preference like a locale: zellij's
  own renderer emits 24-bit colour to the pane, so the value is true of the thing on the other end
  of the pty whoever created the session. Without it nvim colourschemes, `delta`, `bat` and `eza`
  fall back to 256 colours in a launcher-created session and look right in a terminal opened beside
  it. A value already set is never overridden.
- **`ZELLIJ`, `ZELLIJ_SESSION_NAME`, `ZELLIJ_PANE_ID` are unset.** `restart` already scrubbed them,
  for its own reason. `up` did not, and a launcher can be handed them: `systemctl --user
  import-environment` and `dbus-update-activation-environment --systemd`, both ordinary desktop
  idioms, copy a pane's environment into the user manager, and the unit inherits it from there.
  zellij would then believe it is running inside a session, refuse to attach, and the timer would
  repeat that every 60s forever. This is what makes an `UnsetEnvironment=` line in the unit
  unnecessary rather than merely permitted.
- **The configured drop-list** (`session_restart_drop_env`, above) is applied here too.

### Configurable terminal title

```kdl
terminal_title_template "{host} - {session} | {pane}"
session_aliases {
    my-session "MS"
}
```

The OSC 0 title was hardcoded to `<session> | <pane>`. Placeholders are `{host}`, `{session}` and
`{pane}`; `{session}` resolves through the alias map. A placeholder that comes out empty takes the
literal text around it, so the default template still renders exactly what the hardcoded format
did, and an unresolvable hostname does not leave a dangling separator. Unknown placeholders stay
literal. The format is fixed once the first client connects and read from there while panes render,
so the per-frame cost is unchanged.

### Environment variables dropped when a session is created

```kdl
session_restart_drop_env "MY_VAR" "MY_PREFIX_*"
```

A restart triggered from inside a pane inherits that pane's environment, and the rebuilt session
then hands it to **every** pane — so a tool that marks its own environment leaks that mark into
panes it has nothing to do with. Names match exactly, or by prefix with a trailing `*`; a `*`
anywhere else is a literal character. Empty or absent means nothing is dropped.

The key name reads restart-specific; the rule is not. It applies to any session **this binary
creates**, so `session up other-session` typed in an agent's pane drops them too — that command has
exactly the same problem, and `restart` ends in `up` anyway. Dropped after a restart has
daemonized, and in `up` immediately before the server is asked for, which is before the server
captures the environment it will hand out.

### `~` and `$VAR` in config paths

Layouts have always expanded `~` in a plugin location, because layout parsing runs it through
`shellexpand`. Config did not, so the same path written in `config.kdl` was taken literally — and a
`plugin_permissions` key copied out of a layout silently never matched, leaving the plugin to prompt
for a permission it had already been granted.

The path-valued options (`default_shell`, `default_cwd`, `default_layout`, `layout_dir`,
`theme_dir`, `scrollback_editor`, `snapshot_dir`, `web_server_cert`, `web_server_key`) and
`plugin_permissions` keys now expand the same way layouts do. Expansion failure falls back to the
literal string rather than failing config parsing. `plugin_permissions` expands *before* its
wildcard and remote-URL rejections, so a variable cannot smuggle a `*` past a check whose whole
purpose is explicit listing.

### A warning when the running session is a different build

A server keeps the binary it started with for the whole life of the session. Upgrading the package
does nothing to a session that is already up, and nothing anywhere said so — so a machine sits on a
superseded build for days while everyone believes the upgrade took effect, and the bug that was
fixed keeps happening. This has actually happened here.

A client that reaches an existing server now says it once, on stderr:

```
warning: session 'work' is running a different build of zellij than this binary.
  running: /opt/zellij/1.2.3/bin/zellij
  this:    /opt/zellij/1.2.4/bin/zellij
  A server keeps the binary it started with, so an upgrade does not reach a running session.
  Run `zellij session restart work` to bring it onto this build.
```

Said on `zellij action ...`, on an attach to an existing session, and by `session up` when it finds
the session already running — that last one being the report that otherwise hides the problem, since
it says "ok" about a server built before the binary saying it. Once per invocation, never fatal,
and it changes no exit code.

The server's executable comes from `/proc/<pid>/exe` on Linux and from `proc_pidpath` on macOS,
which has no `/proc` (`ps -o comm=` is truncated at the column width and is not a substitute). The
server pid comes from the existing process scan, not a second one. Both executables are compared by
device and inode where they can be stat'ed, because the installed name is a symlink into a versioned
directory and two spellings routinely mean one build; a path comparison alone would cry wolf every
time. Where an inode is missing on either side, only agreement is trusted and disagreement is
reported as nothing at all: a wrong "your session is stale" sends someone to restart a session that
did not need it, which costs more than silence. An executable that cannot be read, a platform that
cannot be asked, and two servers for one name all produce no warning.

Nothing about this reaches `SessionInfo` or the status bar. That would put a version on the plugin
API contract, which is far more than a warning is worth.

### `zellij session enable|disable|status`

```
zellij session enable  [NAME] [--exe PATH] [--force]
zellij session disable [NAME]
zellij session status  [NAME]
```

Absorbs the install that `setup --generate-service` left to the reader. `enable` writes the unit
into the user's own directory (`~/.config/systemd/user`, `~/Library/LaunchAgents`) and loads it —
`systemctl --user daemon-reload` then `enable --now`, or `launchctl bootstrap gui/<uid>`. No root,
no system domain.

`disable` is the half worth having. Both init systems keep the definition they were handed rather
than the file it came from, so removing the file first leaves a job that still runs from a
definition nothing on disk describes — and launchd's `bootout` needs the label, which was in the
file just deleted. So it unloads first, then removes, then tells systemd to look again.

Both are idempotent, following `session up`/`session down`: `enable` over an unchanged install
reports it and touches nothing, `disable` with nothing installed is the state that was asked for.
`enable` over a *changed* one rewrites and reloads, because a rewritten file that was not reloaded
is a lie on disk.

`status` reports the facts separately, because they come apart and the difference is the diagnosis:

```
session   my-session
init      systemd (user)
service   ~/.config/systemd/user/zellij-session-my-session.service - installed
timer     ~/.config/systemd/user/zellij-session-my-session.timer - installed
loaded    yes (service enabled, timer enabled and armed)
config    [Unit] After=network.target
running   yes, in /run/user/1000/zellij/contract_version_1
```

A file with no job was never loaded; a job with no session is a unit that is failing; a session
with no job will not come back. It exits 0 when the unit is installed and loaded, whatever the
session is doing — repairing the session is the unit's job and `session up` is what reports on it.

**Linux gets a timer.** The plist has `StartInterval`, so macOS had a watchdog and Linux did not: a
session that died overnight came back at the next login there and within a minute here. `enable`
writes and enables a paired `.timer` at the same interval, `disable` removes both. One unit name
per session (`zellij-session-<name>.service`), for the reason the launchd label is per session.

`setup --generate-service` is unchanged and still prints the service to stdout, from the same
generator.

**`enable` and `disable` see a job installed under another name.** Both used to work from the file
names this build derives, so both were blind to the job [`status` already reported](#the-installed-job-is-found-by-what-it-runs-not-by-what-it-is-called) — and the two
commands contradicted each other over the same machine: `status` said "installed under a different
name: com.example.my-terminal" while `disable` said "no service installed; nothing to remove".

- `enable` now **refuses** when something else already runs `session up <name>`, naming the file and
  exiting non-zero. Two launchers for one session is not redundancy: both start at login
  (`RunAtLoad`, `After=default.target`) and race — one creates the server, the other reaches
  `session up`, finds a server serving that name and refuses to create a second, and on systemd is
  left in `failed`. That failed unit is what someone eventually investigates, and it is not where
  the fault is. `--force` installs beside it anyway and warns.
- `disable` **names** the surviving job instead of claiming nothing is installed, and **does not
  remove it**. What it removes stays exactly what `enable` wrote: a job written by hand is somebody
  else's file, and a command that deleted it because a session name matched would be one nobody
  could trust.
- `disable` **exits non-zero whenever a job it did not write is still there**, whether or not it
  removed one of its own. The question the command answers is "will this session come back", and
  while another launcher runs `session up` the answer is yes. Exiting 0 would tell a script the
  session had been switched off, and the next boot would disagree — worse after a partial removal,
  because the session then returns from something the command has just made harder to find. The
  detail is on stdout either way; the exit code is for the caller that reads nothing else.

### Extra unit directives from the config (`session_service`)

```kdl
session_service {
    systemd {
        unit "After=network.target" "Before=some-other.service"
        service "Nice=-5"
    }
    launchd {
        keys {
            ProcessType "Interactive"
            Nice 5
        }
    }
}
```

A generated unit cannot know the local facts. systemd's answer is a drop-in directory, which is a
poor answer for the tool that generated the unit: the drop-in is invisible to it, so `status`
cannot report it, and someone reading only the config has no idea it exists. Configuration a tool
generates from belongs where the tool can see it — so it lives here, and `status` lists it.

**Raw passthrough.** A systemd entry is a literal directive line appended to the section that names
it (`unit` → `[Unit]`, `service` → `[Service]`, `install` → `[Install]`); a launchd entry is a
plist key with a string, integer or boolean value, XML-escaped. zellij models neither schema: a
copy of two specifications that already exist would be worse than both, and would reject every key
added to them after it was written.

What it will not carry is what the generator owns — `ExecStart`, the plist keys zellij writes
itself (`Label`, `ProgramArguments`, `LimitLoadToSessionType`, `EnvironmentVariables`, `RunAtLoad`,
`StartInterval`), and anything that **sets** `TMPDIR` or `ZELLIJ_SOCKET_DIR`. Those are config errors
naming the offending entry, reported at parse time so `setup --check` catches them; a unit that
pins either variable builds a session no terminal can see, which is the failure this whole design
exists to prevent. A duplicate key is not an override either: a dict with the same key twice is not
a plist.

**A mention is not an assignment.** The check used to read the whole directive as text, so
`UnsetEnvironment=ZELLIJ ZELLIJ_SESSION_NAME ZELLIJ_PANE_ID ZELLIJ_SOCKET_DIR` — a unit doing
exactly what the guard wants — was refused, and because the check runs at KDL parse time the whole
config failed with it: `setup --check` and every other command, not only `session enable`. The
directives are now read the way systemd reads them. `Environment=`/`DefaultEnvironment=` are
checked per `NAME=` word (quoted words included), `PassEnvironment=` per name, and
`EnvironmentFile=` stays strict because the file is opaque from here. `UnsetEnvironment=` may name
whatever it likes. On the launchd side a key is still refused when its name or its string value
names one of the two: a plist key cannot be read the way a directive can.

The generator's own **defaults** are the exception, and deliberately so — `TERM`, `PATH`, and on
launchd `WorkingDirectory`, `StandardOutPath` and `StandardErrorPath`. They are values the
generator supplies, not parts of the unit it owns, so an entry setting one replaces that default
instead of being refused, and the key is never written twice. `TERM` and `PATH` are environment
variables rather than plist keys, so on launchd a configured one is routed inside
`EnvironmentVariables`, where it means something, rather than left beside it as a top-level key
launchd ignores in silence.

## Assessed and deliberately not built

- **An HTTP/WS API on the embedded web server.** Everything it would have exposed already ships in
  v0.44.3 as a CLI/IPC surface — `zellij subscribe --format json` pushes per-pane render updates as
  they happen (no polling, works on plugin panes, `--ansi` and `--scrollback` supported), and
  `list-panes --json` / `list-tabs --json` / `zellij action` cover the tree and mutations. See
  [docs/web-api-assessment.md](docs/web-api-assessment.md) for the seams, per-endpoint costs, and
  the security constraints if it is ever revisited.

## Not done

- **`--plugin-watch` as a CLI flag.** `Options` crosses the client/server protobuf contract, and
  carrying a new field over it means regenerating the checked-in generated Rust. The setting is
  config-file only rather than pay that cost.
- **Unwatching a plugin's `.wasm` when it unloads.** Watches accumulate for the life of the
  session; a change to a no-longer-loaded plugin resolves to no running instances and does nothing.

## Working on this fork

`upstream` points at `zellij-org/zellij`. Each patch is its own commit on top of the `v0.44.3` tag,
so moving to a newer upstream tag is a rebase onto that tag. The two config-surface patches (the
watcher and the permission grants) share plumbing and land as one commit.

```
cargo build --release
cargo test -p zellij-utils -p zellij-server
```

The toolchain is pinned by `rust-toolchain.toml`; rustup installs it on the first cargo invocation.
Debug builds expect the WASM plugins to be built from source — use `--release`, which uses the
prebuilt assets checked into `zellij-utils/assets/plugins/`.

## Releasing

`.github/workflows/release.yml` replaces upstream's release job. Pushing a `v*` tag builds two
targets — `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` — and attaches
`zellij-nkmk-<version>-<target>.tar.gz` (the bare binary) plus a `.sha256` to that tag's release,
creating the release if it does not exist. Nothing else is published: no musl, no linux arm64, no
intel mac, no Windows.

1. Land the patches, bump the workspace version in `Cargo.toml` (and the `zellij-client` /
   `zellij-server` pins), `cargo build --release` once so `Cargo.lock` is current, commit.
2. `git tag v<version> && git push origin main --tags`. Tags are immutable once a formula pins
   them — never move one.
3. Watch the run: `gh run watch -R noahkiss/zellij $(gh run list -R noahkiss/zellij --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')`.
4. Bump `Formula/zellij-nkmk.rb` in the tap: `version`, the URLs, and the `sha256`
   values. The shas are on the release as `<asset>.sha256` — `gh release download v<version>
   -R noahkiss/zellij -p '*.sha256' -D - 2>/dev/null` or just read them off the release page.
   `Formula/zellij-nkmk-source.rb` takes the tag tarball's sha instead.

A pour of the prebuilt formula reinstalls in about 2 seconds. If a test install takes minutes
instead, it fell through to a source build because `brew` read a **stale local tap clone** — run
`brew update` (or pull the tap checkout) before testing a formula change made in the same session.

The release job builds only the two targets above. Intel macOS was dropped deliberately; if it is
ever restored, the runner label is `macos-15-intel` — GitHub retired `macos-13` in December 2025.

To rebuild an existing tag (workflow changes, a lost asset):

```
gh workflow run release.yml -R noahkiss/zellij -f tag=v0.44.3-nkmk.3
```
