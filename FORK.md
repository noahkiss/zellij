# zellij (noahkiss fork)

A personal fork of [zellij](https://github.com/zellij-org/zellij), rebased onto upstream `main` at
commit **f42ca3c79** (upstream workspace version **0.45.0**), carrying a curated patch set aimed at
the plugin development loop and a few session-lifecycle papercuts.

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
  `<upstream>-nkmk.<n>` upgrade normally from then on.
- **Untrusted-tap gate.** Newer Homebrew refuses to load formulae from a third-party tap until
  it is trusted: `brew trust noahkiss/tap` (or `brew trust --formula noahkiss/tap/zellij-nkmk`).

Switching binaries does not migrate running sessions — the old server keeps running under the old
binary until it exits. Restart your sessions to pick up the fork.

Sessions themselves are portable across the swap: sockets are scoped by client/server *contract*
(`$XDG_RUNTIME_DIR/zellij/contract_version_1/`), not by version string, so the fork CLI attaches to
and manages sessions started by a stock build of the same contract, and the reverse.

## Versioning

`<upstream version>-nkmk.<fork counter>`, e.g. `0.45.0-nkmk.1`. `zellij --version` reports the fork
version, so an install can be verified. The counter resets when the upstream workspace version
moves; a rebase within the same upstream version does not reset it.

Because the version keys `$ZELLIJ_CACHE_DIR/<version>`, the fork does not share plugin artifact or
release-note caches with an upstream build of the same version.

**The version string is not orderable, so do not gate features on it.** Because the counter resets,
`0.45.0-nkmk.4` is newer than `0.44.3-nkmk.7` while `4 < 7` — a consumer comparing counters switches
features OFF on an upgrade, silently. Ask the binary what it has instead:
[`zellij setup --check --json`](#zellij-setup---check---json).

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

### A plugin whose file is missing stops erroring on every tick

A pane whose `.wasm` is gone — what a snapshot taken before the plugin was deleted restores — used
to log `Plugin with id: N not found` on every layout dump, so on every serialization tick, for the
life of the session. The pane also serialized with no plugin at all, so the next snapshot recorded
a plain pane where the plugin had been.

The fork remembers what a plugin that failed to load was asked to run, for as long as its pane
lives. The error is logged once, the pane keeps the loading-error state it already shows, and the
plugin stays in the serialized layout. Reloading that pane does not bring the plugin back once the
file returns; restoring the layout does, because the layout still names it.

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

This adds an `Action` and therefore a message to the client/server contract (tag 148), without
bumping the contract version: a fork client talking to a stock server of the same contract simply
gets nothing for this one action, and every other action keeps working.

### The process behind a pane: `pane_pid`, `pane_cwd` and `pane_command`

```
zellij action list-panes --all --json
```

Terminal panes carry three fields describing what is running in them:

- `pane_pid` — the pid of the process zellij spawned for the pane. That is the pane's own child, the
  shell, not whatever the shell is running; walk down from it to find a tool inside the pane.
- `pane_cwd` — its working directory.
- `pane_command` — the foreground command if there is one, otherwise the pane's shell.

All three are omitted for plugin panes, for a pane the pty thread has not reported on yet, and
for a pane that is being held open after its command exited - the process is gone, and the pid
it ran with is the OS's to hand out again. Re-running the command gives the pane a new one.

**They are on `PaneInfo`, so plugins get them too.** They were CLI-only for a reason: the CLI
resolved each one with a blocking round trip to the pty thread per pane, three per pane at a 100ms
timeout each, which is not something an event path can carry. The pty thread already refreshes a cwd
and command cache once a second for panes that produced output, and already holds every pane's pid —
so it now reports that warm cache to Screen on the same tick, and Screen stamps it onto every
`PaneInfo` it builds. The CLI reads the same stamped fields, and its per-pane blocking round trips
are gone: `list-panes --json` no longer re-probes the OS for what was measured moments earlier.

The JSON keys and their meaning are unchanged; they simply now come from the manifest rather than
from a separate enrichment pass, and appear on plugin `PaneUpdate`/`SessionUpdate` as well.

Both fields have a push complement that already existed and still fires: `Event::CwdChanged` and
`Event::CommandChanged`, both broadcast client-independently. Subscribe to those for changes; read
these fields for the state of the world when a consumer starts, which is what an event stream cannot
tell it. The stamped values follow the pty ticker, so they lag a `cd` by up to a second.

The fields cross the plugin API, so `event.proto` and its generated Rust are regenerated
(`cargo xtask proto`), taking tags 32-34. They also round-trip through `session-metadata.kdl`, so a
plugin reading a peer session gets them too.

### `last_output_at`: when a pane last produced output

`PaneInfo.last_output_at` is the time a terminal pane last emitted bytes, in milliseconds since the
Unix epoch. It is `null` for plugin panes and for a pane that has produced nothing since the server
started.

It means output, not attention: a human typing does not move it, nor does focusing the pane. That
makes it the cheapest liveness signal in the tree — "is this pane still working" answered without
reading a cell of its content, and without spawning a process per pane to scrape one.

The server already recorded the instant on every read from a pty, for the mobile view, and threw the
wall-clock meaning away. Reporting it costs one clock read per manifest.

Nothing pushes on output alone, so a consumer sees the value move on the once-a-second session tick
rather than the moment bytes land. That is still an order of magnitude better than sweeping panes
with `dump-screen`.

Related fix on the same path: a shell emitting OSC 7 (a cwd report) used to clear the pane's
activity flag in the pty thread. That flag gates the whole once-a-second refresh for the pane,
command discovery included, so a shell announcing its cwd at every prompt could starve its own
`pane_command` of updates. The flag now stays set — the pane did produce output — and the extra cwd
read is one the tick performs for any active pane anyway.

Proto tag 35.

### `has_pending_bell`, and bells that are processed while detached

`PaneInfo.has_pending_bell` is `true` for a pane that rang the terminal bell while it was not
focused and has not been looked at since. It is the same condition that puts a ` [!]` suffix on the
pane's `title` — read the field, and leave the title to display, which is what it is for.

The field alone would have shipped a lie. Bell processing sat inside the branch that renders for
connected clients, so **a fully detached session processed no bells at all**: a pane rang, the grid
latched the bit, and nothing looked at it until a human attached. Recording a bell is session state,
not a client-side effect, so the sweep now runs before that gate. What genuinely needs a client —
flashing the pane and the tab, forwarding an ANSI BEL to the terminal — stays inside it, and a bell
that changes recorded state reports the session with or without a client.

The field is always `false` when `visual_bell` is off in the configuration. That setting turns off
per-pane bell notification entirely, and reporting state the rest of zellij does not keep would be
inventing it.

For a consumer, this turns "is that tool waiting for me" from a periodic scrape of pane contents into
something the session pushes — as long as the tool in the pane rings the bell.

Proto tag 36.

### `report_pane_env`: named environment variables on every pane

```kdl
report_pane_env "CLAUDE_CODE_SESSION_ID" "MY_TOOL_ID"
```

`PaneInfo.pane_env` reports the variables this list names, found in the pane's processes. It answers
the question a pid alone cannot: *which* instance of a tool is this pane — which agent session, which
job — without a consumer having to read `/proc` itself or the pane having to announce anything.

**It is an allowlist, and only an allowlist.** Unset means report nothing, there are no default
entries, and the names are exact — no patterns, because a pattern is how a key nobody meant to
publish gets published. An environment is full of secrets; the whole of one is never reported.

The value is looked for on the pane's own child and then its descendants, nearest first, so a tool
running inside the pane's shell answers for a name they both export. That walk is the one
`resurrect_command_hints` already does, and it is the same code: `/proc/<pid>/environ` on Linux,
`sysctl(KERN_PROCARGS2)` on macOS, nothing anywhere else. Both platforms are supported.

Cost: nothing at all with the list unset, which is the default. With it set, the pty thread reads
the process table once per second — once for the tick, not once per pane — and then one environment
read per process it walks, **per pane, every tick**. The walk stops early only when every named
variable has been found. So the cheap case is a pane whose own child exports all of them: one read.
The expensive case is a name that is **not** there — an agent variable on a pane that is not running
that agent — which costs a read of the pane's whole process subtree, every second, for as long as
the pane lives. On macOS each of those reads allocates a `KERN_ARGMAX` buffer, about 1MB, because
the kernel gives no way to ask how big the blob is. Keep the list short, and expect the panes that
do not carry the names to be the ones paying for it.

`pane_env` is the one new field that does **not** round-trip through `session-metadata.kdl`. Putting
it on the event path is something a configuration opted into; writing the values into a file every
session on the box can read, and which outlives the server, is a different exposure and was not.
A peer session therefore reports an empty `pane_env` — read it from the session that owns the pane.

Proto tag 37.

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

**An empty listing names the directories it looked in.** "No active zellij sessions found" is true
of a directory, not of a machine, and the two come apart routinely — a session created under an
exported `ZELLIJ_SOCKET_DIR` this shell does not have is running, reachable, and completely absent
from the list. The bare sentence sent a reader looking for a dead session that was not dead. It now
prints the resolved directory, then the derived alternatives, and says plainly that a directory
nothing here was told about cannot be on that list: `zellij session up <name>` scans the process
table and does see such a server, which is the command to reach for. Fixing the candidate set
itself is not possible — an arbitrary value exported in another shell is not derivable from this
one — so naming what *was* searched is the whole of the honest answer.

### `zellij setup --check --json`

```
zellij setup --check --json | jq -e '.capabilities | index("pane-uuid")'
```

The build says what it can do, in a form a consumer can read. `--check` alone prints prose and a
version string, and a version string is the one thing a consumer must not decide on: the fork
counter resets on an upstream bump, so any comparison of counters gets an upgrade backwards. This
has already happened — two capabilities went silently dark in a consumer that gated on the counter.

```json
{
  "scope": "binary",
  "version": "0.45.0-nkmk.4",
  "base_version": "0.45.0",
  "fork": "nkmk",
  "fork_counter": 4,
  "capabilities": ["capabilities-json", "session-lifecycle", "pane-uuid", "..."],
  "features": ["vendored_curl", "web_server_capability"],
  "directories": { "cache_dir": "...", "socket_dir": "...", "config_file": "...", "...": "..." }
}
```

- **Gate on the capability names.** They are lower-case, hyphenated, name a surface rather than a
  patch, and never change meaning: a renamed feature gets a new name and keeps the old one until
  nothing reads it. `capabilities-json` is in the list so a consumer can tell "this build has no
  fork features" from "this build is too old to answer".
- **The version arrives as a pair**, `base_version` plus `fork_counter`, which is the only correct
  comparison: the base orders normally, the counter orders only within one base. An upstream build
  omits `fork` and `fork_counter` entirely — the keys are absent, not null, so `jq -e 'has("fork")'`
  is the test.
- **The answer describes the installed binary, not a running server** — that is what `"scope":
  "binary"` says. Most of these capability names are server-side surfaces, and a session keeps
  running the server it started with. During the fork's normal upgrade window — a new binary
  installed, sessions from the old one still up — the binary reports capabilities its own sessions
  do not have. A consumer gating **per session** must treat the list as an **upper bound** until
  that session restarts (`zellij session restart <name>`), and fall back when a call the list
  promised is refused. A consumer gating on "what will the next session have" can read it straight.
- `--json` reports `--check`'s directories too, so a consumer that needs the socket directory or the
  config file stops parsing `[SOCKET DIR]` out of prose.

Output is one JSON document on stdout and nothing else, so `| jq` works. `--json` without `--check`
is a usage error rather than a silent no-op.

The list is a single const, `CAPABILITIES` in `zellij-utils/src/capabilities.rs`. A new fork feature
adds one line to it.

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

One upstream quirk to expect when reading a stack: with the 0.45 stack-list rendering, a collapsed
member of a stack reports `is_suppressed = true`. That is upstream's own accounting of what is
currently drawn, not a fork behaviour — read `stack_id` and `index_in_stack` for membership.

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

#### A plugin can read the archive

```rust
subscribe(&[EventType::ListSnapshots]);
list_snapshots();                            // answered by Event::ListSnapshots
```

`PluginCommand::ListSnapshots` asks the server for the archive and the answer arrives as
`Event::ListSnapshots(Vec<SessionSnapshotInfo>)`, newest first. Each entry carries its sidecar — id,
session name, when and why it was cut, the version that wrote it, and its tab and pane counts — plus
the tabs and panes of the saved layout, each pane with its name and command.

A **request** rather than a field on `SessionUpdate`, which is what the plan first proposed. Reading
the archive walks a directory tree and parses every layout in it; `SessionUpdate` is broadcast to
every subscribed plugin about once a second, and that work does not belong there. `ListClients` had
already set the shape for an answer only the plugin that asked for it receives.

The counts come from the sidecar and the tabs from the layout, which is why both are carried. A
snapshot whose layout no longer parses still reports its size and arrives with `layout_error` set,
rather than reading as an empty session: the same reasoning as `snapshot list` marking those entries
instead of failing.

Only a directory holding no `session-layout.kdl` at all is left out, and it says so:

```
WARN  snapshot <path> cannot be listed: it has no session-layout.kdl
```

One line as a directory enters that state and another only if the reason changes, using the same
gating map as the dropped-session warning below — a picker left open would otherwise log an
identical line per poll.

New plugin-API tags only, nothing upstream renumbered: `EventType::ListSnapshots = 50` with payload
`44`, and `CommandName::ListSnapshots = 230`. The command carries `ReadApplicationState`, alongside
`ListClients` and `GetSessionList`.

#### A plugin can restore one

```rust
restore_snapshot("1754251200000-a1b2c3d4", Some("a-new-name"));
```

`PluginCommand::RestoreSnapshot` is `zellij snapshot restore` reached from inside a session. The id
is resolved against the archive, `session_name` restores under a different name exactly as
`--session` does, and the layout file is handed to the same `SwitchSession` the resurrection path
already uses — a restore has always been an attach that takes its layout from the archive rather
than from the cache.

**Restoring into a name that is already running is refused.** Left alone it would not fail, it would
succeed as the wrong thing: the session exists, so the switch attaches to it and the layout is
ignored, and the user gets the running session in place of the shape they picked. `attach --restore`
refuses the same case for the same reason. The picker refuses it a second time, before the keypress,
by marking the row.

The server also trial-parses the layout before switching. Once the client has been told to switch
there is no screen left to report a broken layout to, and the session it lands in would be empty.

Refusals come back as `Event::SnapshotRestoreFailed` and are logged. A restore that works is not
reported at all, because the client is already leaving.

New tags only: `CommandName::RestoreSnapshot = 231` with payload `173`, and
`EventType::SnapshotRestoreFailed = 51` with payload `45`. The command carries
`ChangeApplicationState`, alongside `SwitchSession`.

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

The wait for the created server **backs off**. Each poll forks `ps` to walk the whole process
table, and a fixed 100 ms interval spent the same hundred forks on the session that came up in
200 ms and on the one that never would — and the second is the case a launcher repeats every minute
for as long as the fault lasts. The gap doubles from 50 ms to 1.5 s over the same ten seconds, which
is about eight times fewer forks on the failing machine and no difference on the healthy one.
Nothing gives up early: what would escalate is the watchdog switching itself off, and that is the
one state a person cannot recover from without a shell on the machine. The failure is already loud —
the post-condition and its diagnostics go to the journal, or to the log the plist names.

`up` takes an advisory `flock` on `<socket-dir>/.<name>.up.lock` and holds it across both the check
and the creation. Without it the two are separate steps, so a `restart` typed by hand overlapping
the watchdog's minute tick had both sides find no server and both create one — two servers for a
name that allows one, reported by `assert_up` on both sides and cleaned up by neither, after which
every later `up` refused until somebody killed a server by hand. With the lock the second one waits
and then reports the session already running. A lock that cannot be taken in 30 seconds is a wedged
holder rather than a busy one, so it is named and the `up` goes ahead: no session at all is a worse
outcome than the race.

`restart` takes that lock **once, for its `down` and its `up` together**, and the inner `up`
re-enters the hold rather than waiting for it. Scoped to `up` alone the lock left the window between
the two steps open, and the watchdog's tick fitted in it: the tick's `up` took the lock first and
built the session fresh from the layout, and the restart's `up` then found a healthy session — and
either reported "already running" and exited 0, or refused to restore into it. Both discard the
snapshot the restart existed to bring back, and the case it is wanted for most is restoring a shape
after a reboot, which is exactly when a watchdog is ticking. Re-entrancy is per thread and held in
memory only, so a restart that dies mid-hold leaves nothing to clean up: the kernel releases the
`flock` when the process goes. At the default `--wait-timeout` the whole restart fits inside the
lock's own 30 seconds with room to spare; raise that timeout past about twenty seconds and a waiting
`up` can give up on a restart that is only slow, which is the race put back by hand.

### `zellij setup --generate-service <systemd|launchd>`

Writes a user-level systemd unit or launchd plist whose only job is to call `zellij session up`.
Supervision belongs to the init system; session correctness belongs to the binary.

Two things the generated files deliberately do:

- **They name a stable binary path.** Neither init system looks anything up on `PATH`, so the unit
  needs an absolute one — and the generator canonicalises the running binary to get it, which on a
  package manager with a versioned install prefix yields a path that disappears at the next
  upgrade. (`current_exe()` itself returns the path zellij was invoked through, symlink and all;
  the resolution is the generator's.) So it prefers a `PATH` entry whose canonical target is this
  binary — the stable symlink — falling back to the resolved path with a warning. `--exe`
  overrides, and [`pin_exe`](#a-pinned-copy-of-the-binary-pin_exe) beats both of the derived
  answers. On macOS the choice also decides identity: permission grants are recorded against the
  file, so a versioned path re-asks for every permission after every upgrade.
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

So the job is now identified by what it **does**. Both platforms enumerate every directory the init
system loads from — the user's own first, then the read-only system ones: `/Library/LaunchAgents`
and `/System/Library/LaunchAgents` on macOS, `/etc/systemd/user`, `/run/systemd/user` and the two
`lib/systemd/user` paths on Linux. A job a package or an MDM profile installed keeps a session up
exactly as a hand-written one does, and scanning only the user's directory called it absent. A
nearer directory shadows a file of the same name, which is the init systems' own precedence.
**Only the user's directory is ever written to** — installing anywhere else would need root and
would be somebody else's file.

`/System/Library/LaunchAgents` holds several hundred of Apple's agents, most in the binary plist
format, so converting each one would fork `plutil` hundreds of times per pass — on a command a
watchdog runs every minute. A plist whose raw bytes never contain `session` cannot match however it
is parsed, because the subcommand *is* the identity and both plist formats store strings literally.
That check costs a read and runs before any conversion.

Each file is then read for `Label` and `ProgramArguments`, or for `ExecStart`, and matches when its
arguments run `session up <name>` for this session. argv[0] is not looked at: a unit may exec zellij through a wrapper, and a renamed or
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

### The timer is found through the service it starts

The patch above found the **service** by behaviour. The timer line one row below it still derived a
name, so one output block was governed by two rules — and on a machine with a hand-written pair
`session status` printed this, while `systemctl --user list-timers` showed the timer firing every
minute:

```
service  ~/.config/systemd/user/zellij-session-my-session.service - installed under a different name: my-session.service (~/.config/systemd/user/my-session.service)
timer    ~/.config/systemd/user/zellij-session-my-session.timer - missing
```

Not cosmetic either: a reader concludes the watchdog is not armed, installs one, and lands in the
two-schedulers-racing state `enable` now refuses.

A timer cannot be matched the way a service is — it runs nothing, so there is no `session up` in it.
What it has is a pointer: `Unit=`. So once the service has been found by behaviour, the timer is the
one whose `Unit=` names **that** service. The derived timer name is fallen back on only when the
service was itself found under the derived name; otherwise the search would pair somebody else's
service with this build's own timer, which is the mismatch it exists to end.

`Unit=` is **optional**, and a hand-written timer very often omits it because the pair was named to
make systemd's default do the work — an absent `Unit=` means the same basename with `.service`. That
default is applied when the file is read, so both forms are found. A commented-out directive is not
one, and an empty assignment resets it, the same two readings [`ExecStart`](#the-installed-job-is-found-by-what-it-runs-not-by-what-it-is-called)
already gets right.

It is a second filter over the directory `installed_jobs` already enumerates — the same
`~/.config/systemd/user`, the other extension — not a second place to look. systemd only: launchd
has no timer concept, the agent carries `StartInterval` itself.

The timer found is named the way the service is, and its state joins the `loaded` line, because
whether the watchdog is *armed* is the fact the whole block is read for:

```
service  ~/.config/systemd/user/zellij-session-my-session.service - installed under a different name: my-session.service (~/.config/systemd/user/my-session.service)
timer    ~/.config/systemd/user/zellij-session-my-session.timer - installed under a different name: my-session.timer (~/.config/systemd/user/my-session.timer)
loaded   yes (my-session.service enabled, my-session.timer enabled and armed)
```

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

### A dangling `SSH_AUTH_SOCK` is dropped when a session is created

A stale agent socket is worse than none. With no `SSH_AUTH_SOCK` at all, `ssh` and `git push` fall
through to the keys on disk and ask for a passphrase — awkward, but it works and the reason is
legible. With one that names a socket that has gone, every agent-backed command in **every** pane
fails with `Permission denied (publickey)` for the life of the session, while a terminal opened
beside it works. A graphical login exports `/tmp/ssh-XXXX/agent.<pid>`, a new path at every login,
so a session created from an old shell hands out the previous login's path — and the server gives
its environment to every pane, so the wrong value outlives whatever set it.

`session up` drops the variable when the path it names is not there. It does **not** invent one:
this binary cannot know where an agent would be on your machine, and a wrong path is the fault
being removed. A machine that runs an agent at a fixed path can say so itself, as a
[`session_service`](#extra-unit-directives-from-the-config-session_service) extra:

```kdl
session_service {
    systemd {
        service "Environment=SSH_AUTH_SOCK=%t/ssh-agent.socket"
    }
}
```

`%t` is systemd's `$XDG_RUNTIME_DIR`, which is where a user-level agent unit conventionally puts its
socket. There is no launchd equivalent yet: `EnvironmentVariables` is a key the generator owns, and
a bare `SSH_AUTH_SOCK` under `launchd { keys { ... } }` would be a top-level plist key, which
launchd ignores in silence.

### A warning when `copy_command` has no display to talk to

`copy_command` runs **in the server**, not in the pane that copied, so it inherits the environment
the session was *created* with and keeps it for the session's whole life. A launcher has no
`DISPLAY` and no `WAYLAND_DISPLAY`, so `wl-copy` or `xclip` in a launcher-created session finds no
display and exits non-zero — and the only place that goes is `log::error!`. From inside, copy does
nothing and says nothing, in a session where everything else works.

`session up` now says so on the path that creates the session, naming the command and the missing
variables. The wording is conditional because the fact being reported is about the environment, not
about the command: a `copy_command` that writes a file or speaks OSC 52 wants neither variable and
is not broken. Not on macOS, where the ordinary `copy_command` is `pbcopy` and neither variable
exists on any machine — a warning that is wrong on every Mac is a warning nobody reads.

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
server pid comes from the existing process scan, not a second one.

The two are then compared on the strongest evidence either of them carries, and only a step that
PROVES a mismatch is allowed to report one.

Device and inode come first: the stat is already paid for, and one inode is one file. That settles
the case the installed name creates — a symlink into a versioned directory, two spellings for one
build, which a path comparison alone would cry wolf over every time. But unequal inodes prove
nothing, because a **copy** of a build is a different file holding the same program — which is
exactly what a binary pinned at a stable path is.

So where the inodes differ, the identity the linker stamped into the file decides it, both ways:
the Mach-O `LC_UUID` on macOS, the GNU build-id note on Linux. Both sit in the first few kilobytes,
reached by reading the header and then the load commands or the `PT_NOTE` segments — never the
whole file, which is around 40 MB and would be read on every CLI invocation. The note is found
through the program headers rather than by section name, since sections are what `strip` is
entitled to discard and segments are not.

Not every toolchain emits one. Where the stamp is missing on either side, a file **size** that
differs is still proof of two builds. Where the sizes agree as well, nothing has been established
and the answer is silence: a wrong "your session is stale" sends someone to restart a session that
did not need it, which costs more than a mismatch nobody was told about. An executable that cannot
be read, a platform that cannot be asked, and two servers for one name all produce no warning for
that same reason.

`rustc` does not ask the linker for a build-id by default, so this tree asks for one itself —
`.cargo/config.toml` adds `-Wl,--build-id=sha1` for Linux targets, and macOS gets an `LC_UUID`
without being asked. A binary built by a toolchain that does neither still falls through to the size
comparison.

**Verified on a release artifact, not only reasoned about**, because the release profile sets
`strip = true` and two things could have gone wrong. Checked on a `--release` build of this tree:
the `.note.gnu.build-id` **section** is indeed gone, the `PT_NOTE` **segment** carrying it is not,
and the fork's own reader returns the same id `readelf` does. So a future simplification that looked
the note up by section name would find nothing on every shipping binary while working perfectly in a
debug build — the segment walk is load-bearing, not fastidiousness.

The release workflow keeps the flag. It passes `rustflags: ""` to the toolchain action, and an empty
string there means *do not export `RUSTFLAGS` at all* rather than *export an empty one* — which
matters, because Cargo drops config-file `rustflags` entirely whenever that variable is set, even to
nothing. Do not "tidy" that empty string away.

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
pin       off (no `pin_exe` in session_service)
running   yes, in /run/user/1000/zellij/contract_version_1
```

A file with no job was never loaded; a job with no session is a unit that is failing; a session
with no job will not come back. It exits 0 when the unit is installed and loaded, whatever the
session is doing — repairing the session is the unit's job and `session up` is what reports on it.

**Linux gets a timer.** The plist has `StartInterval`, so macOS had a watchdog and Linux did not: a
session that died overnight came back at the next login there and within a minute here. `enable`
writes and enables a paired `.timer` at the same interval, `disable` removes both. One unit name
per session (`zellij-session-<name>.service`), for the reason the launchd label is per session.

**The state root is written down at `enable` time.** The snapshot archive and `restart.log` both
hang off `$XDG_STATE_HOME`, and a launcher has none while a login shell that exports one does — so
the two resolved *different* state roots. A `down` typed in a pane archived the session's shape to
one; the launcher's `up --restore latest` looked in the other, found nothing, and came back from the
layout instead of the shape that was saved. Nothing failed and nothing warned; the session simply
came back wrong. The generated unit now carries the absolute path the enabling shell resolved
(`Environment=XDG_STATE_HOME=…`, or an `XDG_STATE_HOME` entry inside launchd's
`EnvironmentVariables`), which is the same principle as the binary path: resolve once, record the
result, never re-derive at run time. It is a **default** — a config that sets its own replaces it —
and it is absent entirely when the enabling environment has no absolute one, because then both
sides fall through to the same `HOME`-derived directory and there is nothing to disagree about.
A unit enabled before this change carries no such line — which is exactly the state
[the drift check](#the-config-and-the-installed-unit-are-compared) reports.

**The unit directory is asked for, not derived** (systemd). `~/.config/systemd/user` is
`$XDG_CONFIG_HOME/systemd/user`, and the `XDG_CONFIG_HOME` that matters is the *manager's*, not the
calling shell's. The manager keeps the environment it was started with; a shell that exports its own
afterwards is a different answer, and a unit written into that directory is a file the manager never
looks at — `enable` reports success, `daemon-reload` finds nothing, and every symptom points at the
unit rather than at where it was put. `enable` and `status` now read `XDG_CONFIG_HOME` out of
`systemctl --user show-environment`, install against that, and **name both directories** when they
differ, because a file that is not where its owner expects is the beginning of a second install
beside the first. A value systemd chose to quote is left unread rather than half-unquoted, and a
machine with no user manager to ask falls back to the derived path exactly as before.

**The timer is enabled before the service, and neither failure hides the other.** `enable --now`
starts a unit as well as enabling it, and the service is a `oneshot` running `session up` — so on a
machine where `up` is failing, enabling the service fails too. Doing that first and returning on it
left the timer never enabled, on exactly the machine whose session most needed re-checking: the one
thing that would have retried was the casualty of the first attempt. `disable` collects its failures
the same way, so a unit that will not disable no longer leaves the other one enabled and removes
neither file.

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

### The config and the installed unit are compared

Edit the config and the loaded job does not change with it. The file on disk is stale, the init
system is still running the definition it was handed, and every angle you can look from is
internally consistent — it is only wrong when the two are compared. That is the same shape as a
pinned path the launcher does not run, and it is reported the same way.

`status` gains a `drift` line, and drift counts against its exit code:

```
drift     ~/.config/systemd/user/zellij-session-my-session.service is NOT what this config would write now
drift     run `zellij session enable my-session` to rewrite and reload it
```

`up` says it once per invocation, on the same pass that keeps the pinned copy current — so it
reaches a machine nobody is looking at, through the journal or the log the plist names. Silent
unless something zellij wrote is installed *and* differs: a machine with no launcher has nothing to
report, and reporting it every minute would be worse than saying nothing.

**The remedy has to be `session enable`, not a reload by hand**, and launchd is why: a plist whose
*content* changed needs `bootout` then `bootstrap`. `launchctl kickstart` restarts the job from the
definition launchd already holds, so the obvious command runs the old plist and looks like the edit
did nothing. `session enable` does the right pair, which is the whole reason it exists.

A job installed under another name is not compared. It is somebody else's file and was never
generated from this config.

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

The block also carries [`pin_exe`](#a-pinned-copy-of-the-binary-pin_exe), which is not a directive:
it decides which binary the unit names rather than what the unit contains.

A generated unit cannot know the local facts. systemd's answer is a drop-in directory, which is a
poor answer for the tool that generated the unit: the drop-in is invisible to it, so `status`
cannot report it, and someone reading only the config has no idea it exists. Configuration a tool
generates from belongs where the tool can see it — so it lives here, and `status` lists it.

**Raw passthrough.** A systemd entry is a literal directive line appended to the section that names
it (`unit` → `[Unit]`, `service` → `[Service]`, `install` → `[Install]`); a launchd entry is a
plist key with any plist value, XML-escaped. zellij models neither schema: a copy of two
specifications that already exist would be worse than both, and would reject every key added to
them after it was written.

**Arrays and dictionaries, not only scalars.** A scalar was enough for as long as the plists were
written by hand — anything the config could not express, you wrote yourself. Once the plist is
*generated*, this block is the only route any local knowledge has into the file, so the ceiling
became the real limit, and the keys beyond it are the ones people actually reach for: `WatchPaths`
is an array, `KeepAlive` in its useful form is a dictionary, `StartCalendarInterval` is either.

A scalar is the node's argument; a container is the node's **children**, and which container it is
follows from what those children are called. Every child named `-` makes an array, anything else
makes a dictionary. Nothing has to be declared, the two cannot be confused, and a block mixing them
is refused rather than resolved one way.

```kdl
launchd {
    keys {
        WatchPaths {
            - "~/.config/zellij/config.kdl"
        }
        KeepAlive {
            SuccessfulExit false
        }
        StartCalendarInterval {
            - {
                Hour 3
                Minute 30
            }
        }
    }
}
```

The guard below reads **every** string inside a value, at any depth: a nesting that could hide
`ZELLIJ_SOCKET_DIR` from it would be a guard worth nothing.

What it will not carry is what the generator owns — `ExecStart`, the three plist keys that are what
the unit *is* (`Label`, `ProgramArguments`, `EnvironmentVariables`), and anything that **sets**
`TMPDIR` or `ZELLIJ_SOCKET_DIR`. Those are config errors
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

The generator's own **defaults** are the exception, and deliberately so — `TERM`, `PATH`,
`XDG_STATE_HOME`, and on launchd `WorkingDirectory`, `StandardOutPath`, `StandardErrorPath`,
`LimitLoadToSessionType`, `RunAtLoad` and `StartInterval`. They are values the generator supplies,
not parts of the unit it owns, so an entry setting one replaces that default instead of being
refused, and the key is never written twice. `TERM`, `PATH` and `XDG_STATE_HOME` are environment
variables rather than plist keys, so on launchd a configured one is routed inside
`EnvironmentVariables`, where it means something, rather than left beside it as a top-level key
launchd ignores in silence.

The three **scheduling** keys joined that list when the hand-written plists were audited against
the generator. They had been the generator's outright, so a machine whose agent ticked on any
interval but 60 s, or loaded into any session type but `Aqua`, could not have been reproduced from
a config — which was survivable while you wrote the plist yourself and a blocker the moment it is
generated for you. A watchdog interval is a local fact, and this block is now the only route a
local fact has into the file. A test asserts that every key the hand-written agents carried is
expressible, and that each lands in the plist exactly once.

### A pinned copy of the binary (`pin_exe`)

```kdl
session_service {
    pin_exe true                        // the platform's canonical location
    pin_exe "/opt/zellij/bin/zellij"    // or a path you name
}
```

Unset by default, and unset is the whole feature off: nothing is copied and nothing is reported.

**What it does.** `session enable` and every `session up` install a **real copy** of the running
binary at a fixed path, and the generated unit names that path instead of the package's. `pin_exe
true` resolves to `~/Library/Application Support/zellij/bin/zellij` on macOS and
`$XDG_DATA_HOME/zellij/bin/zellij` on Linux — the platform's own per-user data directory, from the
`directories` crate, deliberately **not** zellij's `XDG_*`-derived paths, which an unusual
environment can point somewhere no other machine has.

**Why a copy and not a symlink.** A package manager installs each release into its own versioned
directory, so the path a launcher was told about is a path the next upgrade deletes. On macOS it
costs more than a broken unit: TCC keys a path-based client on its absolute path, so every upgrade
is a client the system has never seen, holding none of the grants the last one earned — the
mechanism behind [the file-access section below](#macos-decides-about-file-access-at-server-start-not-weeks-later-macos-only).
Measured on one machine, with a session at a fixed path and a grant given to it:

- a **symlink never holds a grant** — macOS resolves it and records the versioned target, both when
  launchd runs it and when the path is added by hand in System Settings;
- a **real file keeps its grant when a different build is written over it** — a changed code
  signature did not revoke it, and the stored requirement is not enforced for an ad-hoc-signed
  client;
- it is **not cached state**: it survived `killall tccd`, a fresh server process, and a reboot.

So the refresh writes **over the same file**, and never unlinks and replaces it: a new inode at the
same path is a new client with none of the grants. Linux gets the same treatment for the plainer
half of the problem — a versioned path that disappears on upgrade.

**What decides whether to copy.** [Build identity](#a-warning-when-the-running-session-is-a-different-build),
not a timestamp. The pinned copy is a copy, so it is a different file from the binary it came from
and only the id the linker stamped in can say whether it is the same build. Same build, nothing is
written — which is every pass but the first after an upgrade, and the binary is around 40 MB while
`session up` runs from a watchdog every minute. A refresh says so once:

```
      refreshed the pinned copy at /home/<user>/.local/share/zellij/bin/zellij
```

A copy that is **being executed** cannot be written over, which is the ordinary case of a session
that is already up on the pinned build. That is reported, not swallowed: a server keeps the binary
it started with anyway, so the restart the message asks for is what the new build was wanted for.

**The unit records the path, and the refresh uses the recorded one.** `up` reads the binary out of
the installed unit rather than deriving the path again. The canonical directory honours
`XDG_DATA_HOME`, and a launcher's environment is not the calling shell's — so a re-derived path can
name a different file from the one the launcher execs, and refreshing that one would leave the
running copy stale while reporting success. One principle, applied here and in the unit generally:
**resolve once at enable time, record the absolute path, never re-derive at run time.**

**Turning the key on after `session enable` is a real state, and it is reported.** The config asks
for a pinned copy, the launcher still runs the package's path, nothing changes, and it looks like
the feature does not work. `session status` names **both** paths and exits non-zero:

```
pin       /home/<user>/.local/share/zellij/bin/zellij - NOT what the launcher runs
pin       the launcher runs /opt/homebrew/bin/zellij - `zellij session enable work` re-points it
```

and `session up` says it once rather than silently copying nothing:

```
warning: `pin_exe` asks for a copy of zellij at
           /home/<user>/.local/share/zellij/bin/zellij
         but the launcher for 'work' runs
           /opt/homebrew/bin/zellij
         Nothing was copied: what `session up` keeps current is the binary the launcher
         actually runs. Run `zellij session enable work` to point it at the pinned path.
         On macOS a file-access grant is recorded against the exact path, so the grant has
         to name the path the launcher runs.
```

Both name the pinned path deliberately. The grant is keyed to that exact path and
[auto-registration could not be reproduced](#macos-decides-about-file-access-at-server-start-not-weeks-later-macos-only),
so the user may have to add it by hand — and a message that does not name it leaves them hunting a
versioned directory.

`session enable` installs the copy **before** it writes the unit that names it, because a unit whose
binary does not exist is one the init system cannot run — and the command that would have created
the copy is the command that unit runs.

The explicit-path form is for a launcher written by hand: the launcher names a path, and the pin has
to be that same one or the two disagree. `--exe` still beats it, being typed for the one command.

### A missing `TMPDIR` is an error, not a panic

```
$ TMPDIR=/nonexistent/path zellij list-sessions
thread 'main' panicked at zellij-utils/src/logging.rs:26:41:
called `Result::unwrap()` on an `Err` value: Os { code: 2, kind: NotFound, message: "No such file or directory" }
```

Hit on a real machine. `configure_logger` is the first thing `main` runs, and it created the log
directory with an `.unwrap()`. The directory it creates sits under the temporary directory, which
`TMPDIR` chooses — and on macOS that same variable decides the **socket** directory, because
`runtime_dir()` is `None` there. So it is the variable most likely to be wrong, and the failure it
produced was a backtrace naming a line of zellij rather than the directory it could not use.

It now names the path, the temporary directory in force, the variable that chose it, and what to do:

```
zellij: could not create the temporary directory for logging
  path           : /nonexistent/path/zellij-501
  temp directory : /nonexistent/path
  TMPDIR         : /nonexistent/path
  reason         : No such file or directory (os error 2)
  The log directory sits under the temporary directory, which TMPDIR chooses. Create that
  directory, or unset TMPDIR to fall back to the system default.
```

Exit code 1, no backtrace. `logging.rs` is upstream code, so the patch is one function and three
`if let Err`: its own commit, droppable whole at a rebase.

### `Ctrl+k` kills a session where `Delete` cannot be typed

The session manager's kill/delete was bound to `Delete` alone, which a Mac keyboard without a
numeric pad does not have. The key labelled "delete" above Backspace sends Backspace, and
`fn+Backspace` — which macOS documents as forward-delete — does not arrive as `BareKey::Delete`. So
on two of this fork's three machines the only destructive action in the plugin was unreachable.

`Ctrl+k` for "kill" now works everywhere `Delete` did — the session list, the resurrect list, and
the single-screen view. `Delete` is unchanged.

No config option: it adds a key where one was missing rather than changing an existing binding, and
per-plugin keybinding config does not exist to extend.

`Ctrl+k` is the only unclaimed mnemonic — `Ctrl+w/f/c/r/d/x/a`, Tab, Esc, Enter, the arrows and
Backspace are all bound in that plugin, and every *unmodified* character is filter input. That last
point is why this is a guard function and not an or-pattern: `BareKey::Delete | BareKey::Char('k')`
under one shared guard would accept a bare `k`, so the first search for a session with a k in its
name would delete one instead. Each key tests its own modifiers.

The widest tier of each help line advertises `<Del/Ctrl k>`; narrower tiers keep `<Del>` rather than
drop a whole entry to fit it.

### The session manager says why its list is empty

An empty session list used to be indistinguishable from a broken one. The plugin polls the server
once a second for the list, and the poll's failure went into `Err(_) => return false` — the message
naming what went wrong was thrown away, and the list stayed empty forever with nothing on screen to
suggest a failure had happened at all. This fork's own machines have shown that empty list for
months without ever producing a clue.

The list area now carries the reason, since it is empty anyway:

```
Session list unavailable: Session-scan state not initialized
```

The message is the server's own, whatever it is, and it is logged as well. It is drawn as a plain
line rather than through the plugin's error modal on purpose: that modal swallows the next keypress,
and a failure that re-arms every second would make the plugin untypeable.

A successful poll that simply returns nothing gets a line too:

```
No sessions to show: the server's scan of its socket and session-info dirs returned 0 live and 0 exited.
```

That case is the more likely one, and it is not an error anywhere in the server. The scan pairs each
socket with a `session-metadata.kdl` under the session-info cache directory, and drops any session
whose file is missing or does not parse — silently, reporting success with an empty list. `zellij
ls`, which reads the sockets alone, keeps listing the session throughout. A user seeing a full
`zellij ls` and an empty picker is looking at exactly that mismatch.

The line reports the two counts the scan produced rather than asserting a cause. It used to claim
the scan had returned nothing, which it could not know: the renderer was free to drop rows the scan
*had* returned, and did. Those now have a notice of their own:

```
2 sessions hidden: the list drew fewer rows than the search returned.
```

The render cache counts the results it turns into rows and subtracts, so a row lost between the
search and the screen reads as a bug instead of as an absence. This was the silent case that made
the empty list unfalsifiable for so long.

The welcome screen is exempt: it hides the current session deliberately, so an empty list there is
normal.

**The session you are in is drawn like any other row**, tagged `<CURRENT>` (`<C>` in a narrow pane)
where the `[ATTACH]` tag would be. The single-screen list used to drop it in the render cache, so a
user looking for their own session found nothing and could not tell a wrong list from a missing
session. Upstream leaves it out on purpose — the single-screen session manager
([#4821](https://github.com/zellij-org/zellij/pull/4821)) inherited the welcome screen's rule of
never listing the session you are already in. That is right for a welcome screen and wrong for a
picker someone opened to manage sessions.

The row is fully selectable. An earlier pass made it visible but unselectable, which traded one dead
end for another: rename, kill and the client actions all route through the selection, so a row the
cursor refused to land on could not be acted on at all, and skipping past it read as a glitch. Only
the one action that cannot work is refused — `ENTER` says `Already attached to this session.` and
stays put. Kill, rename and `Ctrl+x` keep their meanings; Tab-complete still passes over it.

A third silence has the same cure. The list gets whatever rows remain after the prompt and the help
lines, and in a short pane that is none — the list vanished with no hint that height was the reason:

```
2 sessions hidden: the pane is too short to show the list.
```

Both list views say it, and neither says it when the list is genuinely empty or a filter matched
nothing.

The server names the sessions it drops, too, since the plugin can only report that something was
missing and not which session or why:

```
WARN  session my-session is running but cannot be listed: its metadata file does not parse: ...
```

One line per session as it enters that state, and another only if the reason changes — the scan runs
on every poll of every session manager, roughly once a second each, so an unfiltered warning would
be thousands of identical lines an hour. A session listed successfully again is forgotten, so a
recurrence is reported rather than swallowed.

### The session manager lists the attached clients, and detaches one of them

`Ctrl+l` in the session manager shows every client attached to the current session:

```
Clients attached to this session: 2

     Client              Focused pane   Running
<↓↑> 1 (this client)     terminal 14    zsh
     2                   terminal 7     nvim FORK.md
```

`↓↑` selects, `d` detaches the selected client, `ESC` goes back. A detached client keeps its
session; it is the same outcome as that client pressing its own detach key.

Before this the only thing the plugin could do about a second client was `Ctrl+x`, which
disconnects *all* of them. Which client to keep was not a question it could ask, because nothing
told the two apart.

**The current client is listed but cannot be detached from here.** Pressing `d` on it says so
instead. The session's own detach binding already does that job, and this is the one row where the
screen reporting the outcome would vanish along with the client that was reading it.

**The row identifies a client by what it is looking at, not by where it came from.** `pane_id` plus
the command running in that pane is enough to tell two clients apart, and it is what a user
recognises: the laptop is the one in `nvim FORK.md`. A tty name or `SSH_CONNECTION` was considered
and rejected — a zellij client is always local to its server, so an ssh or mosh attach would report
the server's own host, and the plumbing to carry either is much wider than what it would add.

The list is polled once a second while it is open, because clients attach and detach with no event
of their own. It clears on open rather than showing the previous answer: a stale client list is
exactly the sort of thing someone would act on.

Server side, `ClientInfo` and `Event::ListClients` already existed and nothing consumed them. What
was missing was a way to detach a *named* client: the plugin API had `Detach` (yourself) and
`DisconnectOtherClients` (everyone else) and nothing in between. `PluginCommand::DetachClients`
fills that gap and reaches the same `ServerInstruction::DetachSession` that `Action::Detach` uses,
under the `ChangeApplicationState` permission alongside the other two, as
`CommandName::DetachClients = 229` with payload `172`.

### The session manager picks a session out of the archive

`Ctrl+e` in the session manager opens a chooser over every session shape worth reopening — the ones
running now and the ones only the snapshot archive still has — with the tabs and panes of the
selected one beside it:

```
Reopen a session (4): _

Live sessions                    my-session
  my-session                     2 tabs, 6 panes · this session
Snapshots
> a-session                      a-session
  another-one                    3h 12m ago · 3 tabs, 7 panes · shutdown
                                 1754251200000-a1b2c3d4

                                 editing
                                   nvim FORK.md
                                   shell
                                 logs
                                   tail -f /var/log/syslog
```

Type to filter by name, `↓↑` selects, `ENTER` opens the selected row — attaching to a live session,
or rebuilding an archived one — `Ctrl+r` restores a snapshot under a different name, and `ESC` goes
back. Every key is bound only while the picker is up, so the help line replaces the usual one.

**The preview is the point.** Until now the archive was a list of ids and timestamps in a terminal
somewhere else; `zellij snapshot show` prints a whole layout file, which answers "what is in this"
by making you read KDL. A snapshot's value is entirely in what it would give back, and that is a
list of tabs and the panes in them.

**Restoring over a running session is refused, and the row says so before the key is pressed.** The
picker knows which names are live, so such a row is drawn dimmed and `ENTER` on it explains and
points at `Ctrl+r`. The server refuses it a second time, because the picker's list is a poll old and
the failure it prevents is silent: the switch would attach to the running session and discard the
layout, handing back the wrong thing rather than nothing. A snapshot whose layout does not parse is
refused the same way and for the same reason — it would land in an empty session.

`Ctrl+r` is the way out, and it is `snapshot restore --session` under another name. It also makes a
snapshot a template: restore the same shape beside a session that is still running.

**The sources are a list, not two fields.** Adding another place to reopen a session from — the
parked remote-session work is the obvious candidate — costs one variant and one builder, not a
rewrite of every branch in the file. Selection runs over the rows of every source flattened
together, so a new source changes what is in the list and nothing about moving through it.

The archive is re-read once a second while the picker is open, because a snapshot is cut by a server
this one knows nothing about: another session shutting down, a `delete-session`, a `save-session
--archive` elsewhere. The selection is carried by name and id across those reads, so a snapshot
arriving at the top of the list does not move the row under the cursor. Below 60 columns the preview
column is dropped rather than squeezed — two unreadable columns are worse than one readable one.

`Ctrl+e` is not a mnemonic and there was no mnemonic left to take. `s` for snapshot, `h` for history
and `a` for archive all switch modes in the default keybindings before a key reaches the plugin, and
`r`, `d`, `x`, `w`, `k` and `l` are already bound inside it. The label in the help line does the work
the letter cannot.

### Focus wraps around the ends of a stack

Moving focus up from the top pane of a stack lands on the bottom pane, and down from the bottom
lands on the top. Previously focus simply stopped: `progress_stack_up_if_in_stack` looks for a
stacked pane *directly above* the current one, finds none at the top, and `move_focus_up` returns
`false` having done nothing. With `alt+j`/`alt+k` as the way through a stack, that stop is felt on
every pass — you arrive at one end and the key goes dead.

No config option. This fires only where nothing happened before, so there is no previous behaviour
to preserve and nothing to opt out of.

The wrap is a **third** fallback, after the ordinary directional move and the in-stack progression.
Order matters: a stack sitting below another pane still lets you leave it upwards rather than
wrapping, because `next_selectable_pane_id_above` is consulted first. Only a genuine dead end wraps.

A one-pane stack does not wrap onto itself — that would report a focus change that never happened
and make `move_focus_up` return `true` having moved nothing.

`expand_pane` does the move rather than `move_up`/`move_down`: those step between neighbours, and
this jump crosses the whole stack at once. Three tests in `stacked_panes_tests.rs`.

The wrap covers **both** stack renderings. Upstream 0.45 added stack lists (`stacked_pane_list`,
default on), which keep every member but the visible one in `Tab::suppressed_panes` rather than in
the grid — so `wrap_stack_focus` sees a stack of one there and does nothing. `Tab` gets its own
`wrap_focus_within_stack_list`, in the same third position: `Tab::move_focus_up`/`_down` try the
in-list step, then the grid move, then the wrap. Three more tests in `tab_integration_tests.rs`, one
of them holding that order — a stack list with a pane above it still leaves upwards.

### macOS decides about file access at server start, not weeks later (macOS only)

Found on a real machine. In a pane of a launcher-created session:

```
$ codex --yolo resume --last
Error: Operation not permitted (os error 1)
$ ls ~/Downloads/some-project
ls: cannot open directory '/Users/<user>/Downloads/some-project': Operation not permitted
```

Nothing to do with either program. macOS attributes a pane's file access to the **responsible
process**, and for a session created by a launcher that is the zellij executable — not the shell,
not what the user ran. The recorded denial named zellij's path, not `ls`'s:

```
kTCCServiceSystemPolicyDownloadsFolder | …/zellij-nkmk/0.44.3-nkmk.6/bin/zellij | 2  (allowed)
kTCCServiceSystemPolicyDownloadsFolder | …/zellij-nkmk/0.44.3-nkmk.8/bin/zellij | 0  (denied)
```

That attribution is the whole problem, because TCC keys a path-based client on its **absolute
path**, and a package manager puts each release in its own versioned directory. Every upgrade is a
client macOS has never seen, holding none of the grants the last one earned. So it asks again — from
a background process, at some unpredictable moment — and a prompt dismissed in passing is recorded
as a refusal. Files & Folders then denies **permanently and silently**: TCC never asks twice. The
user connects none of it; they upgraded zellij, and a week later an unrelated tool stopped working
in a directory that was fine yesterday.

This is a cost the fork itself introduced. Before session creation moved into a launcher, the
responsible process was the terminal emulator, which had held its grants for years.

`start_server` now opens each protected location once, so macOS decides while the user is still
looking at the upgrade:

- **Downloads, Desktop, Documents** — promptable. With no decision on record the probe raises the
  ordinary consent dialog. One click restores what the previous version had.
- **Full Disk Access** — *not* promptable; Apple offers no API to request it. Attempting it was
  **not observed to register the client** either. An earlier version of this section said the
  attempt is what lists a program in that settings pane; four controlled tests do not support that.
  On a clean slate — every entry removed, machine rebooted — the probe ran, was refused, and no
  `AllFiles` row appeared, from the server process and from a pane descendant alike. There is no
  hidden table: in the system database only `access` has rows.

  One observation keeps this open rather than simply false. A machine running nkmk.8 carries an
  `AllFiles` row at `auth_value 0`. Nobody writes a *denied* row by hand, so registration happened
  at least once. It has not been reproduced and no mechanism is claimed for it.

  So the FDA half of the probe has no demonstrated effect, and the versioned path still has to be
  typed into that pane by hand. It is kept because the attempt costs nothing and the log line names
  the path to type.

The warning names the **resolved** executable. A package manager installs the binary in a versioned
directory and puts a symlink on `PATH`; `current_exe()` returns the symlink, while TCC records the
target. Printing the symlink sends the reader looking for a settings entry that will never exist.

Best-effort throughout, and silent about success: a refusal is logged, never raised. Nothing here
can intercept the failure that actually bites, because that happens in a pane's process later; the
probe's job is to make the decision happen at a moment that explains itself.

Recovery differs by pane, which the log line says: Full Disk Access is toggled on, while a Files &
Folders refusal is permanent until its **entry is removed**, after which the prompt returns.

**The probe runs on its own thread, and must.** A promptable location with no decision on record
blocks inside the open until someone answers the dialog. On the server's main thread that means the
session never appears — about 100 seconds on one machine, and once until it was rebooted — while
every retry is refused as a second server, and the log stops at a line nowhere near the cause.

That gives up an ordering guarantee: panes now spawn while a decision may still be pending. It costs
nothing, because TCC coalesces. Measured on a machine with no decision on record: two processes
touching the same protected directory, one pending dialog, and **both waited** — neither was refused.
A pane that touches a protected directory in those first seconds waits alongside the probe and
proceeds once the user answers.

`zellij-server/src/lib.rs` gets one call; the probe and its classifier live in
`session_lifecycle.rs`, which is fork-owned. No-op off macOS.

### `pane_frame_style "top_only"`

A fourth value for upstream's `pane_frame_style`, next to `full`, `titles` and `none`:

```kdl
pane_frame_style "top_only"
```

Each pane gets one title line and no box around it. The fork used to ship this as a standalone
`top_only` option that overrode the frame code; upstream 0.45 added `pane_frame_style` with a
`titles` mode built on the same frames-off path, so the override is gone and this variant replaces
it. Anything that reads `pane_frame_style` — the CLI `SetPaneFrameStyle`, the keybinding, the plugin
API — takes `top_only` as well.

This is a fork-only *value* of an upstream key, which is stricter than a fork-only key. A stock
build ignores a key it does not know, but rejects a value it does not know: stock 0.45 fails the
whole config with `Invalid value for pane_frame_style: 'top_only'`. Keep it out of a config file
that a stock 0.45 build also reads.

`top_only` **is** `titles`, with four differences:

1. **The title line is a horizontal rule.** `titles` leaves the line blank around the title unless
   the pane is stacked; `top_only` fills it with `─` always. One condition in
   `compose_bracketed_title`.
2. **No separators between panes.** `titles` draws `│` and `├─` from `Boundaries` along every pane
   edge; `top_only` draws none. `render_pane_boundaries` returns early.
3. **A single pane still gets its rule.** `titles` hides the title row when a tab holds one
   selectable non-borderless tiled pane; `top_only` promises one rule per pane, so it keeps it.
   Two conditions in `tiled_panes/mod.rs` — `single_selectable_tiled_pane` in `set_pane_frames`
   (the row the layout reserves) and `omit_pane_title` in `render` (the row it draws).
4. **The title stays on the left.** Upstream's frames-off renderer centers the pane title
   (`compose_bracketed_title`, new in 0.45's #5318); `top_only` keeps the pre-0.45 left-aligned
   title, one arm in the same function. `titles` keeps upstream's centered look.

Everywhere else the two behave identically, deliberately: `draws_titles()` answers `true` for both,
so `top_only` follows the `titles` branch through the layout, offset and stacking code without a
second path to keep in step.

What that costs: the layout still reserves the column the separators would have used, so a pane with
a neighbour to its right ends its rule one column short. Closing that gap means re-entering
upstream's pane-layout arithmetic, which is the code most likely to move under us at the next sync.
Add it later if the ragged edge grates.

`TogglePaneFrames` cycles `full → titles → none → full`, which has no seat for a fourth style.
`top_only` toggles to `none` and back to `top_only`: `Screen` remembers the last style that was
not `none` (`pane_frame_style_before_none`) and `cycle_pane_frame_style` returns to it when it
was `top_only`. Every other starting point keeps upstream's cycle.

`PaneFrameStyle::TopOnly` carries protobuf tag **100** in both `pane_frame_style.proto` and
`event.proto` — a fork-reserved number, far from the next one upstream would take.

**Floating panes obey it too**, which they did not at first. A floating pane draws its frame through
its own render path, which builds a `PaneContentsAndUi` of its own and never called
`set_top_only_frames` — so the title row was reserved and a full frame's rule was drawn into it. The
floating path now passes the style it already had in scope, exactly as the tiled path does.

### A dwell before a focused pane's bell clears (`bell_clear_delay_ms`)

```kdl
bell_clear_delay_ms 1000
```

A pane that rings the bell shows `[!]` in its title until it is focused, and upstream clears that
the instant the pane takes focus. Cycling through panes to find the one that rang therefore erases
every notification on the way. With `bell_clear_delay_ms` set, the `[!]` only clears once the pane
has been focused, without interruption, for that many milliseconds - counted from the later of the
focus and the last ring. Flicking past a pane keeps its notification; a pane that rings again while
you sit on it restarts the dwell, so the ring is never lost.

Default is 0, which is upstream's behaviour to the byte: the focus paths clear the bell
synchronously as they always have, and no timer is involved.

How it works: `Screen` keeps a `BellDwellTracker` (`zellij-server/src/bell_dwell.rs`) of what each
client has focused since when, and of the last ring per pane. The focus paths, and a ring on a pane
that already shows a bell, ask the background-jobs thread for a `ClearPaneBellAfterDwell` job. When
the job comes due, the tracker decides: it clears only if some client has held that pane focused for
the whole dwell and it has not rung inside it. A job overtaken by a refocus or a new ring finds the
answer is no and does nothing. Multiple clients count independently - any one of them dwelling is
enough.

### A pane uuid that outlives id reuse

```
zellij action list-panes --all --json
```

Every pane - terminal and plugin - is given a uuid when it is created, and reports it as `uuid` in
`list-panes --json` and in `PaneInfo`. A pane id does not identify a pane on its own. It counts up
from 0 per server, so the id a script wrote down names a different pane in the next session, and
after a restart or a resurrection of the same session; terminal and plugin ids are separate
sequences, so pane 0 is two panes at once. A uuid is handed out once, ever, which buys two things:
re-targeting a pane you created without matching on its title, and an existence check - if the uuid
is no longer in the listing, the pane is gone.

(Within one running server the terminal id counter is monotonic, so a closed pane's id is not
reissued while that server lives. The uuid is what makes that guarantee legible, and it is the only
thing that still holds across the restart.)

What the uuid promises, exactly:

- It is stable for the pane's whole life inside one running server: renaming, moving between tiled
  and floating, stacking, suppressing and hiding all keep it.
- It does **not** survive a server restart, a session resurrection, or a snapshot restore. What
  comes back is a new pane with new state, and it says so with a new uuid. A pane read out of a
  saved layout (the snapshot listing) reports an empty uuid, because it is a description of a pane
  rather than a pane.

The uuid is generated in `TerminalPane::new` and `PluginPane::new`, so there is no creation path
that can forget it, and it is read through the `Pane::pane_uuid` trait method.

#### What a restored pane continues (`restored_from`)

The rule above - a restored pane is a new pane and says so with a new uuid - is what stops a
consumer's pre-restart state reattaching to a pane it no longer describes. It also loses the link
entirely, and some consumers do want it: "this is the pane that WAS my build shell" is a reasonable
thing to know after `zellij session restart`.

So the link is reported separately. A pane rebuilt from a serialized session reports
`restored_from`, the uuid of the pane it continues, alongside its own new `uuid`. Empty for a pane
that was never restored.

```
before a restart   uuid f1e5dce9…   restored_from ""
after it           uuid 9c2fa401…   restored_from "f1e5dce9…"
```

Provenance, not identity. Nothing keys off it by default: a consumer that wants continuity opts in,
and one that does not keeps the safe behaviour for free. **Neither is a key across a restart on its
own** - ids repeat there too - so pair it with the session's creation time, which changes on exactly
the events that invalidate both id spaces.

**One hop.** A pane restored twice names the incarnation directly before it, not the whole chain
back to the first. The serialized layout records the pane's uuid at the moment it was written, so
each restart records one link, and a consumer that wants the full chain keeps it itself.

The uuid travels in the serialized layout as a `pane_uuid` property on the pane node, which is why
the KDL says `pane_uuid` and the pane reports `restored_from`: at write time it IS the pane's uuid,
and calling it `restored_from` there would read as a promise that the pane comes back under it.
`pane_uuid` is written by serialization and never by hand; a hand-written layout simply omits it.

It is deliberately NOT carried on the plugin API's or the client/server contract's layout messages.
Provenance is something the server assigns when it rebuilds a pane, never something a sender
declares - a layout that could name its own lineage could lie about it.

`PaneInfo` gains `string restored_from = 31`, so `list-panes --json` reports it for free.

`PaneInfo` crosses the plugin API, so `event.proto` gains `string uuid = 30` and its generated Rust
is regenerated (`cargo xtask build`). `list-panes --json` gets the field for free - `PaneListEntry`
flattens `PaneInfo`. The client/server contract has no `PaneInfo` and is untouched.

### The about page names the binary macOS must trust

`Ctrl+o` then `a` now ends with the path of the executable the **server** is running:

```
Server binary (macOS: grant Full Disk Access to this path):
/Users/<user>/.local/share/zellij/zellij-pinned
```

The section above explains why that path matters on macOS: TCC keys a path-based client on its
absolute path, so the string that has to be typed into Full Disk Access is a specific file, not
"zellij". Until now the only way to learn it was a log line the user had to know to look for. It is
on the one page whose whole job is telling you what you are running.

The path is **resolved**. `current_exe()` returns whatever was invoked — usually a package manager's
symlink on `PATH` — while TCC records the target, so the server canonicalizes before it hands the
value over (`own_executable_path` in `session_lifecycle.rs`, shared with the startup probe rather
than copied).

**It arrives as plugin configuration, injected at the load seam.** `configuration_for_load`
(`plugins/plugin_loader.rs`) adds the key `zellij_exe` to the configuration copy written into the
plugin's memory immediately before `load()`. Not to the stored `PluginConfig`: that
`initial_userspace_configuration` is half the key the plugin map dedupes and focuses instances by,
so injecting there would make every `launch-or-focus-plugin zellij:about` miss the running instance
and open a second pane. The load seam is the funnel every instantiation path goes through, including
the clone made for each additional client. No `PluginCommand`, no `.proto`, no `zellij-tile` change.

**The macOS hint is decided by the server, not the plugin**, through a second key
`zellij_exe_hint`. The host running the server is the one TCC has an opinion about, and a client can
be somewhere else entirely; the plugin has no way to ask. Off macOS the label is just
`Server binary:`. Both variants cost the same two rows, and the path keeps its own line on either —
sharing a line with the label costs it those columns, and the page truncates rather than wraps,
which would hand the user a wrong path to paste.

**It survives a short pane.** The page is a fixed block centred in whatever rows it has, and
anything past the bottom is never drawn — the main screen wants exactly 18 rows, and one pane frame
or a nested session's bars is enough to push the last component off. A paragraph can now be marked
essential; `components_to_hide` drops the largest non-essential component first — the nine-row
"What's new" list long before a one-line paragraph — until the page fits, and never drops an
essential one or the help text. The path stays visible down to five interior rows. A component that
did not draw also clears its rendered coordinates, so it stops answering clicks where it used to be.

One limit: the about pane is a fixed 90 columns, so a path beyond roughly 78 characters is cut. Real
install paths fit; a development build's path inside a deep worktree does not.

**A second line names the path that survives an upgrade.** The resolved path answers "which build is
this session running", and that is all it is good for: a package manager installs into a versioned
directory, so the path the about page showed is gone at the next upgrade — and on macOS the Full
Disk Access grant went with it, because TCC records the grant against the file. So the page shows
both, when they differ:

```
Server binary (running):
/home/linuxbrew/.linuxbrew/Cellar/zellij-nkmk/0.45.0-nkmk.4/bin/zellij
Grant Full Disk Access to this path instead - it survives upgrades:
/home/<user>/.local/share/zellij/bin/zellij
```

The second path is chosen by `resolve_service_exe` — the same function, in the same order, that
decides what a generated service unit execs: the [pinned copy](#a-pinned-copy-of-the-binary-pin_exe)
first, then a name on `PATH` whose canonical target is this same file. Nothing steadier found, or a
pinned path with no file at it, and the line is left off entirely rather than pointing at something
that does not exist; off macOS the label is `Stable path (survives upgrades):`.

`pin_exe` is a config key and the plugin thread holds no config, so the server records it once at
startup (`record_configured_pinned_exe`), where it already reads `config_options`. The plugin gets
the answer as a third configuration key, `zellij_exe_stable`, through the same load seam.

**`<c>` copies the path.** Nothing in the about pane could be selected with the mouse, so the one
value on it a user has to act on had to be retyped by hand. `<c>` on the main screen copies the
stable path, or the running one where there is no stable path, and the help line says so — the key's
columns are computed from the line it is appended to rather than counted out, so a reworded help
line cannot leave the colour pointing at the wrong characters. Built-in plugins are granted every
permission, so `WriteToClipboard` costs no prompt.

### The session manager's client list actually lists clients

`Ctrl+l` reported no attached clients, whatever the truth was — for one client and for two, on every
machine, since the list was added:

```
Clients attached to this session: 0
The server reports no attached clients
```

The server builds that answer from a layout snapshot, and the plugin-facing path removed the
requesting plugin's own pane from the snapshot first. That rule belongs to the layout *dump*, which
nobody wants the session manager written into, and this path inherited it by passing
`Some(plugin_id)` where `zellij action list-clients` passes `None` — which is exactly why the CLI
was right all along. The session manager is a floating pane, and zellij focuses a new floating pane
for every client in the tab, so that one pane was the focused pane of *every* attached client.
Removing it removed the whole list.

The list also read one pane layer per tab — floating when floating panes were on screen, tiled
otherwise. Floating visibility is per tab while focus is per client, so a client focused on a tiled
pane in a tab where someone else had floating panes up had no row anywhere. Both layers are read
now, the off-screen one first, so a client remembered by both is described by the layer it is
actually looking at.

**Each row also carries that client's terminal size**, because a session is sized to its smallest
attached client and nothing said which client that was:

```
     Client          Focused pane   Size    Running
<↓↑> 1               terminal 14    160x40  zsh
     2 (this client) terminal 7     100x28  nvim FORK.md
```

The smallest client by area is drawn in the emphasis colour — the same one the `(this client)`
marker uses — so the terminal shrinking everyone else's grid is the one that stands out. The mark is
withheld when every sized client is the same size, since a highlight that always fires says nothing,
and a client whose size the server never recorded never wins the comparison.

The value is `Screen`'s own `client_sizes` map, the same one `recompute_tab_size` takes the minimum
of, so the number on screen is the number that decides the grid. It rides to the plugin thread on
`SessionLayoutMetadata`, which already made that trip for `ListClients` — no new instruction, no new
thread message.

`ClientInfo` crosses the plugin protobuf, which this fork normally avoids. The two fields take
fork-reserved tags **100** and **101** in `message ClientInfo`, leaving the low numbers free for
upstream, and both are optional and read **both-or-none**: an older plugin against a newer server,
or the reverse, reports no size rather than half of one.

### Resume hints for serialized commands (`resurrect_command_hints`)

A resurrected pane holds the command it was running, and `ENTER` re-runs it. For a tool keyed by a
session id — a coding agent, a REPL that keeps state — re-running the bare command starts a *new*
session and the old work becomes unreachable. The pane comes back; what was in it does not.

A hint names a command, the environment variable that tool exports, and what to record instead when
that variable is found among the pane's processes:

```kdl
resurrect_command_hints {
    claude {
        match "claude"
        env "CLAUDE_CODE_SESSION_ID"
        rewrite "claude --resume {}"
    }
    opencode {
        match "opencode"
        env "OPENCODE_SESSION_ID"
        rewrite "opencode --session {}"
    }
}
```

`match` compares against the **basename** of the recorded command, exactly, so one hint covers both
`claude` and `/opt/homebrew/bin/claude` and does not cover `claude-code`. `rewrite` is split on
whitespace into a command and its arguments — it is not a shell string, so quoting and pipes mean
nothing — and must contain `{}`, checked at parse time so `zellij setup --check` reports a hint that
could never apply. The first matching hint wins; the block names are labels only.

The variable is read from the pane's pid and every process under it, breadth first, first one that
has it wins: `/proc/<pid>/environ` on Linux, `sysctl(KERN_PROCARGS2)` on macOS — the same call
`ps -E` makes, which the kernel serves for a process of the same uid — and nothing at all on any
other platform. One process-table read serves the whole pass.

Best-effort throughout: no hint, no variable, or no platform each record the command unchanged, at
debug log level. Nothing here can fail a serialization.

Config-file only and unset by default, so a config without the block behaves exactly as before. It
reaches the pty thread through the existing `Reconfigure` message, so **a config save applies to the
next serialization** without restarting the session.

Note the KDL constraint that bites here: every node needs a `;` or a newline after it, the last one
before a closing brace included, so the one-line form `{ match "x"; env "Y"; rewrite "z" }` does not
parse. Use the multi-line form above.

### What a pane is running, when the shell has no job control

`resurrect_command_hints` above, and the plain "this pane was running X" line under it, both rest on
zellij knowing what a pane runs. On a machine whose shell has job control off (`setopt no_monitor`,
`set +m`) it never knew: **no pane in any snapshot carried a command**, and no hint ever fired.

Discovery asks the terminal for its foreground process group (`tcgetpgrp` on the pty master) and
drops the answer when it equals the pane's shell pid. That test is right when job control is on — an
idle shell IS the foreground group, and a foreground job gets a group of its own. With job control
off the shell never moves a job out of its own group, so a pane running an agent for hours is
indistinguishable, by process group alone, from a pane sitting at a prompt. Everything then fell
through to the shell itself, which discovery recognises as the default shell and records as nothing.

Those panes are now asked about their **children** instead — the same ppid-based answer the Windows
arm already gives, for the same reason (Windows has no controlling-terminal foreground group). An
idle shell has no children and still records nothing; a shell running something has one. Where a
shell has several, the newest wins: the one a user is looking at is the one they started last.

The process-group lookup stays first and unchanged, so nothing about a job-controlled shell changes.

### Tabs come back in the order they were left in

A session restored from a snapshot got its tabs back in **creation** order, so every tab that had
ever been moved jumped back to where it started — once per restart, quietly, with the serialized
layout agreeing with the wrong order.

`screen.tabs` is keyed by stable tab id; the display order lives in `tab.position`. The two agree
until a tab is moved. `get_layout_metadata` iterated the map, so what it wrote was id order — and a
layout recreates tabs in the order it lists them. `query-tab-names` read the same map the same way,
which is worse than it sounds: it is the command you would reach for to check the order, and it
confirmed the wrong one. Both now sort by position.

### A tab bar in a tab you are not looking at

Moving, renaming, adding or closing a tab left every *other* tab's tab-bar plugin drawing the old
list. Arriving at one of those tabs drew the stale list for a frame (~10ms, the repaint debounce)
before it corrected itself — visible as a flicker, since the tab names shift.

`targeted_plugin_ids` sends `TabUpdate` to the active tab's plugins only, which is upstream
`12ee60753` (#4918) and correct for the frequent case: the event fires on nearly every state change,
and updating every tab's plugins each time was a measured regression when switching tabs. It is only
wrong for the rare one. The update now reaches every tab's plugins when the tab list actually
changed — decided by comparing `(id, position, name)` per tab against what was last reported — and
the active tab's alone otherwise. A switch changes none of those, so switching stays exactly as
cheap as upstream made it. Measured on a 5-tab session: the frame drawn on arrival is correct, and
takes the same ~20ms to appear as before.

The active-tab **highlight** is still one frame behind on arrival, and is left that way. The plugin
only learns it is active after the switch, and making the frame wait for that redraw does not work:
a switch also fires `ModeUpdate` and `PaneUpdate`, the plugin redraws for those first, and that
redraw satisfies the wait before the tab-aware one exists. Fixing it needs a render to say which
event it answers, which `PluginRenderAsset` does not carry.

### Notices the server draws over the viewport

Two facts are true of a whole session, actionable, and invisible everywhere else:

```
                        ⚠ Full Disk Access not granted for /Users/<user>/Library/…/zellij/bin/zellij
                        ⚠ session 'main' runs a superseded build - `zellij session restart main`
```

They are drawn by the **server**, top-right, after the panes have rendered. That placement is the
whole design:

- **It wins every frame.** Composited after pane rendering, so an alt-screen application repainting
  underneath cannot clobber it.
- **`dump-screen` never sees it.** Nothing is written into any pane's grid, so transcript reads and
  content matching are exactly what they were.
- **It costs no layout.** No pane gives up a row.
- **A plugin could not do it.** Almost all zellij chrome is plugins - tab bar, status bar - and a
  user who has replaced theirs would never see a notice living in a bundled one.

Narrow viewports truncate with an ellipsis and, below 24 columns, draw nothing: a notice that
wrapped across the top of the panes would be worse than no notice.

Both questions are re-asked every 30 seconds, because both answers change under a running server -
an FDA toggle takes effect immediately, and an upgrade can replace the binary at any time. When the
answer changes, every tab is forced to re-render: output is diffed between frames, so a notice that
simply stopped being drawn would leave its glyphs on screen.

**Full Disk Access** (`expect_full_disk_access true`, macOS, off by default) opens the same
FDA-gated file the startup probe uses last and reports a refusal. Opt-in, because only the user
knows whether they mean zellij to hold that permission - and where they do, its absence IS the
actionable fact whether or not it was ever granted. The notice names the path, because the grant is
keyed to that exact file and [auto-registration was not observed to
happen](#the-about-page-names-the-binary-macos-must-trust).

**A superseded build** (`stale_build_notice`, on by default) is asked of the path this server was
STARTED FROM: the file being gone, or holding a different build than the one running, is proof.
Comparing against whatever `zellij` is on `PATH` would call a deliberately-mixed setup stale
forever. Two platform details make this precise rather than lucky: a package manager's upgrade
deletes the old versioned directory, and Linux reports a deleted binary's path with a ` (deleted)`
suffix, so the file "not existing" is exactly the case that matters. A binary that is merely
RENAMED is followed, and correctly says nothing.

The one addition is for `pin_exe`: a pinned copy cannot be written over while it is being executed,
so an upgrade can never change it under a running server and the rule above can never fire. There -
and only there - the binary on `PATH` is the intended source of that copy, so it is what gets
compared.

### `pin_exe` covers a session you started by hand

[`pin_exe`](#a-pinned-copy-of-the-binary-pin_exe) keeps a copy of the binary at a path zellij owns,
so macOS records its permission grants against a file an upgrade does not move. For a while that
covered only the generated service unit — and a session started by typing `zellij` therefore ran a
server from the package manager's versioned path, which is a client TCC has never seen. Full Disk
Access was re-granted after every upgrade for every session the launcher did not start, which is
most of them. The feature looked broken because it was, for the way sessions actually get created.

The **server** is redirected, and only the server: it owns the panes and opens the files, so it is
the process the grant has to name, and the client is gone from TCC's point of view the moment it has
spawned one. What the user typed keeps running as the client, and `zellij --version` still answers
for the binary on `PATH`.

The copy is brought up to date first, by the same `install_pinned_exe` `session up` uses — written
over in place, because a new inode at that path is a new client with none of the grants. When it
cannot be updated, the current binary is used and the reason is printed. The ordinary cause is
another session's server already running the pinned copy, and the fallback is not politeness: a
pinned copy of a different build is a server that would not speak to its client.

The path is decided once, in `start_client`, where the config is, and reaches `spawn_server` through
a `OnceLock` rather than through the `ClientOsApi` trait and its test fake.

### `default_floating_size` — a bigger default for floating panes

A floating pane that carries no coordinates of its own lands at half the viewport
(`half_size_middle_geom`, `floating_pane_grid.rs`). Half a viewport is not enough for the plugins
that open that way: the session manager truncates session names and its client list, the plugin
manager truncates paths. The information is there; the pane is too small to show it.

Nothing in config.kdl reached that geometry, and a keybinding cannot reach it either.
`LaunchOrFocusPlugin` and `LaunchPlugin` have no coordinates on the action at all, and the KDL action
parser reads `x`/`y`/`width`/`height` only inside the `Run` branch. A keybind that spells out
`width "90%"` passes `setup --check` cleanly and is then ignored — the worst kind of no.

```kdl
default_floating_size {
    width "90%"
    height "85%"
}
```

Each axis is optional and independent, and a value is either a percent of the viewport or a fixed
column/row count. Zero, over 100%, non-numeric and unknown keys are all parse errors, so a typo is
reported rather than dropped. The block fills in only the axes the caller left open, inside
`Tab::add_floating_pane`, where every floating pane is born — a pane that asked for its own width
still gets it, and the pane recentres on whatever size it ends up with.

The value reaches tabs as an `Rc<RefCell<_>>` shared with `Screen`, the way `stacked_resize` does, so
**a config save applies** to panes opened after it, with no per-tab update chain.

An absent block leaves the coordinates untouched, so upstream behaviour is byte-identical. Safe in a
config a stock build also reads: this is a fork-only *key*, which stock zellij ignores.

Two floating panes stay their own size on purpose: the about page and the first-run wizard resize
themselves to 90×20 after opening, through `change_floating_panes_coordinates`, which never passes
through `add_floating_pane`.

### Moving focus inside a stack no longer makes the shells reprint

Every focus move between the members of a stack made **both** shells print a fresh prompt. Three
moves down and back left four prompts in each pane, and a stack used as a working set of shells
filled with them.

Nothing was written into the panes: the reprints were the shells answering `SIGWINCH`. A focus move
inside a stack collapses one member to its one-row header and expands another, and both of those row
counts were pushed to the panes' ptys. A collapsed member was told it had **one** row, and the pane
it handed the space to was told the size it already had; the collapse alone is enough, and each move
signalled two shells.

The stack arithmetic is not the culprit, and neither is the 0.45 rework
(#5331, #5337, #5342, #5379): a collapsed member has been one fixed row since long before it, and
`set_pane_frames` has always ended by resizing every pane's pty. What decides the outcome is the *content offset*. With
`full` frames the single row is entirely frame, so the member's content height comes out at zero, and
a zero never reaches the terminal — the unix `set_terminal_size` is gated on non-zero rows. Drop the
box and that same row is one row of content, which does reach it. So the bug is upstream's, older
than 0.45 and reported against 0.43 (zellij-org/zellij#4047), and frames-on was only ever protected
by accident.

What 0.45 changed is who lands on which path. `PaneFrameStyle::from_options` reads `titles` for
everything except `pane_frames false` and an explicit `pane_frame_style "full"` — so the default, and
even a deliberate `pane_frames true`, now takes the frames-off path that was always broken. Measured
on this fork before the fix, three focus moves in a two-pane stack:

| frame setting | prompts per pane |
| --- | --- |
| default (`titles`) | 4 |
| `pane_frames true` | 4 |
| `pane_frame_style "full"` | 1 |
| `pane_frame_style "top_only"` | 4 |
| `pane_frames false` | 4 |

Both stack renderings reprinted, classic and stack list. The fork's 0.44 `top_only` was clean for the
same accidental reason: it rode the old frames-on override, where 0.45's `top_only` is a `titles`
variant.

The fix says what the accident said, on purpose. A pane collapsed to its stack header has no content
area, so `set_pane_frames` takes its content rows to zero and leaves its pty alone. The pane keeps —
in its grid and in its terminal — the size it had while expanded, so re-expanding to the same
geometry writes the same size back and the shell hears nothing. A stack that genuinely changed size
while the pane was collapsed (a column resize, a moved stack) is picked up on the way out of the
header, as one legitimate resize.

`PaneGeom::is_collapsed_stack_member` names the state — stacked, fixed, one row — and the branch sits
at the single point every layout pass goes through, so no caller has to remember it and every frame
style is covered by construction, `full` included: it now skips the resize outright instead of
sending a zero for the os layer to drop. Both stack renderings end up quiet, classic and stack list;
the list's row-swap is untouched.

Two tests in `tab_integration_tests.rs`, both driving a real stack through the mock pty writer: one
runs a series of focus moves in each of the four frame styles and holds that every member is given
exactly one size, never a one-row size; the other holds the other side, that a tab resized while a
member was collapsed reaches that member when it is focused.

### Two bars ship in the binary (`slim-tab-bar`, `slim-keybinds`)

`default-plugins/slim-tab-bar/` and `default-plugins/slim-keybinds/` are builtin plugins of this
fork, built by xtask, embedded in the binary and named in `BUILTIN_PLUGIN_NAMES`. A layout asks for
them the way it asks for any builtin:

```kdl
pane size=1 borderless=true {
    plugin location="zellij:slim-tab-bar"
}
```

They arrived as separate repos (`zj-slim-bar`, `zj-slim-keybinds`) loaded through `file:` paths, and
they are recreations of zellij's own `tab-bar` and `status-bar`/`compact-bar` — replacements for
builtins, in the same category as the things they replace. Sources moved in, not artifacts: bundled
has to mean rebuildable. `zj-slim-bar` became `slim-tab-bar` on the way, because the `zj-` prefix
namespaced an external repo and there is nothing left to namespace.

What that buys: no `.wasm` to track in dotfiles, no plugin directory to keep in step, a bar that
cannot go stale against the binary that loads it, a bar on a bare machine with no dotfiles, and one
rebase instead of three. It costs about 3.6MB of binary, most of it the IANA timezone database the
tab bar's clock carries.

**Neither bar calls `request_permission` any more, and that is the point.** A builtin's permission
checks short-circuit to `Granted` (`wasm_bridge.rs`, `zellij_exports.rs`), so a request only ever
raised a prompt — in a bar, a pane nobody can focus to answer it. Zellij's own bars have never had
one. The permission cache is no longer written for either bar.

### A builtin can be developed like any other plugin (`builtin_plugin_dir`)

Bundling would otherwise trade away this fork's headline feature for exactly the plugins most likely
to be edited: a builtin lives in the binary, so `plugin_watch` had nothing to watch.

`builtin_plugin_dir "<path>"` makes the server load `<path>/<name>.wasm` for a builtin when that
file is there, and `plugin_watch` then watches it like any `file:` plugin — edit, `cargo build
--target wasm32-wasip1 --release`, and the running bar swaps to the new code.

- A builtin with no file in the directory still loads its embedded copy, so overriding one plugin
  leaves the rest alone.
- An unreadable override falls back to the embedded copy with a warning instead of failing the load,
  because a half-written `.wasm` mid-build must not take a bar down; the watcher reloads it when the
  build finishes.
- Only names in `BUILTIN_PLUGIN_NAMES` can be overridden — the directory must not silently shadow
  anything else.
- config.kdl only, and a development override: production runs the embedded copy, which is what
  makes a builtin version-locked to its binary in the first place.

The configured directory is recorded once in a `OnceLock` (`input/plugins.rs`) because the two places
that need it — resolving a builtin's bytes and watching its file — sit on different threads and
neither carries the config.

### Eight more things a pane already knew about itself

```
zellij action list-panes --all --json
```

Every one of these was a method on the server's `Pane` trait, answered for terminal and plugin panes
alike, and readable nowhere outside the server. They are now stamped onto `PaneInfo` beside the
rest, so they reach the CLI, plugins and peer sessions on the same path:

- `is_alternate_screen` — the program is drawing on the alternate screen, i.e. a full-screen editor,
  pager or TUI owns the pane rather than a shell sitting at a prompt. The cheapest way to tell those
  apart, and the reason a pane's scrollback stops growing.
- `scrollback_position` / `scrollback_length` — how far the pane is scrolled back from the bottom,
  and how many lines it can scroll through. `0`/`0` for a plugin pane.
- `is_pinned` — a floating pane pinned above the tiled layer. Always `false` for a tiled pane.
- `logical_position` — the pane's position in the layout that placed it, which survives resizing and
  reordering. `null` for a pane no layout placed.
- `is_borderless` — the pane is drawn with no frame, so it shows no title.
- `exclude_from_sync` — the pane is left out when the tab syncs input to all its panes.
- `has_explicit_title` — a human named this pane. `title` is the display title whatever its source
  and `program_title` is what the program called itself; neither could answer "did someone type
  this name", which is what a consumer needs before overwriting it.

**`frame_color_override` was assessed and skipped.** The survey read it as "the server already marks
panes it considers failed and nothing outside sees it", but the mark is a one-second flash: the
background job that sets it clears it again after `LONG_FLASH_DURATION_MS`
(`background_jobs.rs`), and the same field also carries the multi-select highlight. A field sampled
by a once-a-second manifest would report it at random, and the error text that would make it
meaningful is not on the trait at all. If a "this pane failed" signal is wanted it should be its own
recorded state, not a render override read sideways.

`session-metadata.kdl` carries six of the eight, so a peer session reports them too. It does not
carry `scrollback_position` or `scrollback_length`: those describe where one session's own viewport
sits in a buffer that grows with every line of output, which a reader of another session can neither
act on nor keep up with, and writing them would rewrite the file for every pane that produced a line.

Proto tags 38-45.

### A crashed plugin says so (`Event::PluginDied`)

When a plugin panics, zellij pushes a loading-error indication to the plugin's pane. A background
plugin has no pane. So a background plugin - the kind that watches a session and forwards what it
sees - died behind one `log::error!` and nothing else changed: no event, nothing to poll, and a feed
that has gone quiet looks exactly like a session with nothing to say.

`Event::PluginDied(plugin_id, message)` is broadcast to every subscribed plugin when a plugin
crashes, carrying the error it died with. A supervisor plugin can therefore notice, and a consumer
downstream of one can be told.

- **Nothing restarts the plugin.** This is a signal, not a policy; reloading is the caller's
  decision and belongs where the caller's configuration is.
- **Each plugin id is announced once.** Announcing a crash sends an event, and delivering an event
  is how a plugin crashes, so two plugins that die on `PluginDied` would otherwise keep each other
  crashing forever. A reloaded plugin gets a new id and can be announced again.
- The pane indicator is unchanged for plugins that have a pane.

`SessionInfo.plugins` deliberately does **not** mark dead plugins, because it does not list live
ones either: `Screen` builds every `SessionInfo` with an empty map, `populate_plugin_list` has no
callers anywhere in the tree, and the KDL codec drops the field. Marking a list nobody fills would
be a fiction; populating it is a separate piece of work.

Proto: `EventType` 54, event payload 48.

### `Event::PaneExited`, so a failed command pane tells someone

A command pane that fails had exactly one way to say so, and it only worked for one kind of caller:
`Event::CommandPaneExited` goes to the plugin that opened the pane. A pane started by a layout or by
`zellij action new-pane -- <command>` has no such plugin, so its exit status was read in the pty
thread and thrown away. `PaneClosed` fired, carrying no status. A build in a background tab could
fail and nothing outside that tab ever knew.

`Event::PaneExited(PaneId, Option<i32>)` is the same news told to everyone. It is broadcast like
`PaneClosed`, from every path that spawns a terminal - the interactive one, the layout one and the
re-run of a held command pane - so it works in a fully detached session. `CommandPaneExited` still
goes to the originating plugin exactly as before.

- The status is `None` when the process was killed by a signal or the status could not be read.
  `None` is not zero, and a consumer deciding whether a job succeeded must not treat it as such.
- It fires for a plain shell exiting too, which is the same event from zellij's point of view.
- A pane that holds after exiting keeps reporting `exited` and `exit_status` on `PaneInfo`; a pane
  configured to close is announced by `PaneClosed` immediately afterwards.

Proto: `EventType` 53, event payload 47.

### `Event::PaneOpened`, the counterpart of `PaneClosed`

`PaneClosed` was the only structural lifecycle event zellij had. It fires from every close path and
reaches every subscribed plugin whether or not a client is attached, so pane *removal* was already
observable in a detached session — while pane *creation* could only be recovered by diffing a pane
manifest against the last one a consumer happened to see.

`Event::PaneOpened(PaneId)` closes the asymmetry. It carries the same payload as `PaneClosed`, is
broadcast the same way, and fires from every creation path: tiled, floating, stacked and
no-preference panes, in-place and editor panes, the panes a layout builds when a tab is created,
terminals and plugins alike.

What it deliberately does not fire for:

- **A pane that moved.** Toggling a pane between tiled and floating, breaking it out to another tab
  or restacking it is the same pane with the same id and uuid. Only creation is announced.
- **A pane that was not created.** A creation that finds no room for the pane drops it; the event is
  sent only after the pane is found in the tab, because announcing one that does not exist is worse
  than announcing nothing.

Pair it with `PaneClosed` for a complete pane lifecycle without polling, and with `PaneInfo.uuid`
when a consumer needs to tell a restored pane from the one it continues.

Proto: `EventType` 52, event payload 46.

### Idempotent setters for the four remaining toggles

```
zellij action set-fullscreen on --pane-id terminal_3
zellij action set-pane-pinned off --pane-id terminal_3
zellij action set-pane-floating on
zellij action set-sync-tab off --tab-id 2
```

Fullscreen, pinned, embed/float and tab sync could only be toggled. A controller that lost track of
the state could not converge on it without reading first, and the read races anything else touching
the session. Each now has a set-form, the shape the fork already gave floating-panes, borderless and
the theme.

The value is positional and boolish: `on`/`off`, `true`/`false`, `yes`/`no`, `1`/`0`. `--pane-id` and
`--tab-id` are optional and default to what the calling client is focused on, so the interactive use
stays short; naming the target explicitly is what makes the call work in a detached session. The
toggles are untouched.

Exit status follows `show-floating-panes`: **0** the state changed, **2** it was already so. A target
that does not exist prints the reason on stderr and exits non-zero — every failing `zellij action`
exits 2, because the client turns any error message into that one code, so **the message on stderr,
not the exit status, is what separates a miss from a no-op**. `set-pane-floating` reports a refused
move — the last tiled pane may not float, and an embed needs room — the same way, because reporting
"already so" for a move that did not happen would be a lie. `set-fullscreen on` differs from the
toggle in one more way: when another pane holds fullscreen, it hands fullscreen to the named pane
instead of merely clearing it.

Four `Action`s, contract tags 163-166.

### Move a pane between tabs from the CLI (`break-pane`)

```
zellij action break-pane                                          # the focused pane, new tab
zellij action break-pane --pane-id terminal_3 --name build --no-focus
zellij action break-pane-to-tab --pane-id terminal_3 --tab-id 2
zellij action break-pane-right                                    # focused pane, new tab to the right
zellij action break-pane-left
```

`Action::BreakPane*` existed and plugins had all three `break_panes_to_*` calls, but the word `break`
appeared nowhere in `cli.rs`, so reorganising a session across tabs was the one structural edit the
CLI could not make. All of it is exposed now.

`break-pane` moves panes into a new tab. Without `--pane-id` it moves the focused pane, which is what
the keybinding does. With one or more `--pane-id` it moves exactly those, which is what makes it work
in a detached session. `--name` names the new tab and `--no-focus` leaves focus where it is.
`break-pane-to-tab` moves them into an existing tab and requires both `--pane-id` and `--tab-id`.
Both print the affected tab's id.

**A pane that no tab owns is now an error rather than a silent skip.** `break_multiple_panes_*`
drops a pane id it cannot find, so a request naming only stale ids used to move nothing, report
success, and — for a new tab — leave an empty tab behind. Both instructions now check every pane
first and fail naming the first miss, changing nothing. A missing `--tab-id` fails the same way.
This applies to the plugin calls that share these instructions too: a plugin passing a stale pane id
now gets an error instead of a partial move.

Two `Action`s, contract tags 161 and 162.

### `signal-pane` — signal the process in a pane

```
zellij action signal-pane --pane-id terminal_3               # SIGINT
zellij action signal-pane --pane-id terminal_3 --signal kill
```

Plugins could send SIGINT and SIGKILL to a pane; the CLI could not, and `write-chars $'\003'` is not
the same thing — it asks whatever is reading the pty to interpret a keystroke, which a program that
has turned off canonical input, or a pane whose reader has wedged, will not do. `--signal` takes
`int`, `hup` or `kill`, the three the server can already deliver, and defaults to `int`.

`--pane-id` is required: a signal is destructive enough that it should never fall back to whatever
happens to be focused, and requiring it makes the command safe in a detached session. Naming a pane
that does not exist, or a plugin pane, which runs no process, fails with the reason on stderr rather
than warning into the log. A pane held open after its command exited fails the same way: its child
was reaped, and the pid it ran with may since have been given to an unrelated process.

The signal goes to the process zellij spawned for the pane — the pane's shell — which is what the
plugin API has always done. A shell without job control runs its command in that same process, so
this reaches the command; a shell with job control will handle the signal itself.

This adds an `Action` and therefore a message to the client/server contract. Fork action messages
start at **tag 160**, leaving 149-159 for upstream, so an upstream bump does not have to renumber
anything the fork added.

### `zellij ls --json`

```
zellij ls --json
```

The session listing was three prose formats — coloured, `--no-formatting`, `--short` — and a consumer
had to parse one of them. `--json` prints an array instead, and it also reports what the human
listing never did: `ls` reads the socket directory alone, while each live session writes a
`session-metadata.kdl` the listing ignored.

Each entry carries `name`, `created_seconds_ago` (the number the human listing formats as "x ago"),
`is_current` and `is_dead`, plus `connected_clients`, `web_client_count`, `web_clients_allowed`,
`tab_count` and `pane_count` from that metadata. The metadata fields are omitted, not null, for a
dead session — it has no server to have written any — and for a live session whose metadata does not
parse.

`--json` overrides `--short` and `--no-formatting`; `--reverse` still orders the array. No sessions
prints `[]` on stdout and keeps the existing exit status 1 and the existing note on stderr, so
nothing but the array ever reaches stdout.

### `list-clients --json`

```
zellij action list-clients --json
```

The client list was a human table only, so a controller had to parse fixed-width columns to learn
who is attached and whose terminal is shrinking the grid. `--json` prints the `ClientInfo` array
plugins already receive in `Event::ListClients` — `client_id`, `pane_id`, `running_command`,
`is_current_client`, and the fork's `terminal_size` and `tty` where the server knows them. Nothing
else goes to stdout, and no clients prints `[]`.

`pane_id` is the serde form of the enum, `{"Terminal": 3}`, because this is the plugin-facing struct
verbatim rather than a second shape to keep in step. `is_current_client` marks the client that asked;
`zellij action` is its own short-lived client focused on no pane, so a CLI query marks no row. The
flag rides on the existing `ListClientsAction` message (field 1) and adds nothing to the contract.

### `go-to-tab-name --no-focus`

```
zellij action go-to-tab-name build --create --no-focus
```

"Make sure a tab named X exists" used to be impossible without stealing focus: `--create` focused the
tab it made, and naming a tab that already existed focused that one. `--no-focus` makes the call
idempotent in the way a controller wants — the tab is there afterwards, and whoever was looking at
something else still is. `--create` still prints the new tab's id, so a script can create the tab and
then address it.

Without `--create` the flag reduces the command to an existence probe, and **stdout carries the
answer, not the exit code**: the command exits 0 whether or not the tab is there, printing the tab's
id if it exists and nothing at all if it does not. A script tests the output, not `$?`:

```bash
if [ -n "$(zellij action go-to-tab-name build --no-focus)" ]; then echo "the tab is there"; fi
```

The flag rides on the existing
`GoToTabNameAction` message (field 3), so it adds no message to the client/server contract; the
plugin API's `focus_or_create_tab` is unchanged and always focuses.

## Assessed and deliberately not built

- **An HTTP/WS API on the embedded web server.** Everything it would have exposed already ships
  upstream as a CLI/IPC surface — `zellij subscribe --format json` pushes per-pane render updates as
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

`upstream` points at `zellij-org/zellij`. Each patch is its own commit on top of the recorded
upstream base, currently `f42ca3c79` on upstream `main`. Moving to a newer base is
`git fetch upstream && git rebase --onto upstream/main f42ca3c79`, then recording the new base
here. The two config-surface patches (the watcher and the permission grants) share plumbing and
land as one commit.

```
cargo build --release
cargo test -p zellij-utils -p zellij-server
```

The toolchain is pinned by `rust-toolchain.toml`; rustup installs it on the first cargo invocation.
Debug builds expect the WASM plugins to be built from source — use `--release`, which uses the
prebuilt assets checked into `zellij-utils/assets/plugins/`.

## Releasing

Each release gets a changelog at `changelogs/v<version>.md` — the commit range plus authored notes
saying what changed and why. Write it before tagging; it is the single source of truth the `zellij`
skill's references point at.

`.github/workflows/release.yml` replaces upstream's release job. Pushing a `v*` tag builds two
targets — `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` — and attaches
`zellij-nkmk-<version>-<target>.tar.gz` (the bare binary) plus a `.sha256` to that tag's release,
creating the release if it does not exist. Nothing else is published: no musl, no linux arm64, no
intel mac, no Windows.

1. Land the patches, bump the workspace version in `Cargo.toml` (and the `zellij-client` /
   `zellij-server` pins), `cargo build --release` once so `Cargo.lock` is current, commit.
2. `git push origin main`, wait for the **`Rust`** workflow to go green — it builds the plugins
   from source, which `Release` does not — then `git tag v<version> && git push origin v<version>`.
   Tags are immutable once a formula pins them — never move one.
3. Watch the run: `gh run watch -R noahkiss/zellij $(gh run list -R noahkiss/zellij --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')`.
4. Bump `Formula/zellij-nkmk.rb` in the tap: `version`, the URLs, and the `sha256`
   values. The shas are on the release as `<asset>.sha256`. `-D -` does **not** stream to stdout —
   it creates a directory literally named `-` — so download to a temp dir and read them:

   ```
   d=$(mktemp -d) && gh release download v<version> -R noahkiss/zellij -p '*.sha256' -D "$d" \
     && cat "$d"/*.sha256
   ```

   Better still, verify rather than transcribe: download the tarballs alongside them and run
   `sha256sum -c *.sha256` in that directory, so a wrong value cannot reach the formula.
   `Formula/zellij-nkmk-source.rb` takes the tag tarball's sha instead.

A pour of the prebuilt formula reinstalls in about 2 seconds. If a test install takes minutes
instead, it fell through to a source build because `brew` read a **stale local tap clone** — run
`brew update` (or pull the tap checkout) before testing a formula change made in the same session.

The release job builds only the two targets above. Intel macOS was dropped deliberately; if it is
ever restored, the runner label is `macos-15-intel` — GitHub retired `macos-13` in December 2025.

To rebuild an existing tag (workflow changes, a lost asset):

```
gh workflow run release.yml -R noahkiss/zellij -f tag=v0.45.0-nkmk.1
```
