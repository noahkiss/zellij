# zellij (noahkiss fork)

A personal fork of [zellij](https://github.com/zellij-org/zellij), rebased onto the upstream
**`v0.46.0`** tag (upstream workspace version **0.46.0**, commit `aedee21cb`), carrying a curated
patch set aimed at the plugin development loop and a few session-lifecycle papercuts.

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

The tap carries a third formula, `zellij-nkmk-rc`. It exists to prove a patch before it lands and
points at whatever candidate is being tested — see [Releasing](#releasing). Do not install it
unless you are the one proving the patch.

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

## Agent changes wake a supervising agent

Start the child, then wait for agent-change in the background; never poll the screen.

```sh
zellij action --json wait --for agent-change --pane sunny-otter --pane quiet-fox --timeout 300
```

Omit `--pane` to watch every agent in the session except the caller.
The wait records state, name, identity, appearance, and exit changes.
Exit 0 reports a change; exit 2 reports a timeout or session failure.
The JSON record includes pane handle, tab, kind, name, old/new states, and one last reply line.
The reply comes from the harness transcript when a turn ends, when available.
The client samples internally; the supervising agent runs one blocking command.
Without OSC 7501, a small screen detector recognizes trust gates, rate-limit gates, and question menus.
The harness-map evaluator will replace this fallback.
MCP exposes the same wait through `zellij_wait_for until=agent_change`; `pane` remains optional for this mode.
The capability is `wait-agent-change`.

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

**The version bump also sets `SERVER_UNCHANGED.declared_for`** to the new version, and says whether
the release changed the server. A unit test fails until it does. See
[whether a restart is needed](#session-restart---when-idle-a-pre-flight-and-whether-a-restart-is-needed).

## The CLI output convention

Every command the fork touches answers in one of three shapes, so that reading one command teaches
you how to read the next.

- **A single record is `key: value` lines.** One key per line, the key in `snake_case`, a single
  space after the colon. `zellij action new-pane` answers `pane_id: terminal_7` and `handle:
  sunny-otter`, not a bare number.
- **A list of like things is a table** with a header row of `UPPER_SNAKE` column names.
- **A nesting of like things is an outline.** Indentation carries the nesting, and each line names
  its own fields as `key: value` pairs two spaces apart, so a line read on its own still says what
  it is. Only `list-tree` answers in this shape, because it is the only command with something to
  nest — a table cannot nest and a record cannot repeat.
- **All three are append-only.** A key or a column may be added in any release; none is ever renamed or
  removed. A value that carries a given name means the same thing in every command that prints it,
  which is why `pane_id` is `terminal_7` everywhere and never `7` in one place and `terminal_7` in
  another.
- **Where `--json` exists it carries the same information, structured.** JSON is the interface for
  programs; the default output is for a human or an agent reading a shell. Every `zellij action`
  verb that reports a record now has it, in front of the verb — see [`zellij action
  --json`](#zellij-action---json-answers-the-mutations-too). The verbs that were already answering
  in JSON keep their own trailing flag, and for those the two spellings are one call: `ls`,
  `list-panes`, `list-tabs`, `list-tree`, `list-clients`, `list-agents`,
  `current-tab-info`. The payload commands refuse it and always will.
- **Results go to stdout, diagnostics go to stderr.** A command whose output you are capturing never
  mixes an explanation into it.
- **Exit codes are `0` acted, `1` error, `2` the command changed nothing.** The `2` is one bucket
  with several doors into it: a **miss** — a well-formed request about something that is not there,
  a pane that no longer exists, a tab by a name nothing answers to; a **refusal** by one of the
  three classes below; a **confirm** nothing could answer, or that you declined; a **wait** that
  timed out; and a call clap would not accept in the first place. None of those is a failure, and
  each prints its sentence on stderr like one so that `set -e` scripts stop either way. A `1` is
  narrower: the call could not be carried out at all — a regex that does not compile, a handle a
  live pane already holds, a target the command cannot address, text too large or not UTF-8, a
  server that failed. **The sentence on stderr, not the exit code, says which door it came
  through.**
- **A payload command prints the payload and nothing else.** `dump-screen` writes screen content to
  stdout; it does not introduce it. Asked for [several panes at
  once](#dump-screen-sweeps-many-panes-in-one-invocation) it prints one JSON object per pane
  instead, because a payload cannot say which pane it came from — one pane is still the bare
  content, and `--json` is still refused.

**A session nothing answers to is a miss, and answers like one.** A wrong name, a name whose server
is gone, or no name at all where the CLI cannot choose one prints its sentence and the live session
names **on stderr**, writes nothing to stdout, and exits **2**:

```
$ zellij -s no-such-session action list-panes --json
                                      # nothing on stdout
Session 'no-such-session' not found. The following sessions are active:   # stderr
work
notes                                 # exit 2
```

It used to exit **0** with the `ls` table on stdout, so a script that mistyped a session name got a
successful answer of the wrong shape. The cause was that every one of these paths printed its
sentence and then called `list_sessions`, which writes the table to stdout and ends in
`process::exit(0)` — making the `exit(1)` written on the next line unreachable. The same held for
`zellij attach` with no session to choose from. Only the listing that was **asked** for, `zellij ls`,
still goes to stdout.

A mutation reports what it changed rather than acknowledging that it ran: `close-pane` prints
`closed: terminal_3`, `move-tab` prints the `from:` and `to:` positions, `go-to-tab` prints the tab
it left and the tab it landed on. The point is that the answer is usable — a script that moved a tab
knows where to move it back — and that a command which changed nothing says so with exit 2 instead
of a silent success.

## Reading the surface in one call

```
zellij setup --dump-surface          # the outline convention
zellij setup --dump-surface --json   # the same map, structured
```

Every command in the tree — the `action` verbs, the session lifecycle, `setup` itself — with its
band, its one-liner, its aliases, and each flag's type, whether it is required, repeatable or
positional, its default and its help. Then what clap cannot know: what the command prints, in the
shape [the convention](#the-cli-output-convention) names, and the keys or columns it prints under.

```
command: zellij action list-tree  group: read
  about: List every tab with its panes nested beneath it
  prints: outline  keys: tab_id position name active / handle pane_id title command focused
  arg: --json  type: flag  about: Output as JSON
```

**A command with no `prints:` line prints nothing when it succeeds.** That is the fork's default
rather than a gap in the map, and the dump's header says so, so the absence can be read.

Only the bands and the `prints:` table are written down; everything else is read out of the same
clap tree that parses the call, and a test walks a command's arguments out of clap and requires
each one by name. A flag added tomorrow is in the map tomorrow, and cannot quietly miss it.

The dump runs before the configuration is read, like `--dump-config` and `--dump-layout` beside it:
what the CLI accepts does not depend on a config file, and a broken one must not take the map away.
`zellij setup --json` now answers to `--check` or to `--dump-surface`; with neither it says which
ones it takes, on stderr, and exits 2 — a usage error, like the ones clap raises itself.

### `zellij action --help`

The same information, for a reader rather than a parser. The page opens with the conventions above
— the shapes, `--json`, stdout against stderr, the exit codes, the refusal, and what a handle is —
and then lists the verbs in five bands rather than one alphabetical column of eighty-seven:

| Band | What is in it |
|---|---|
| `read` | asks the session something and changes nothing |
| `navigate` | moves focus or the view; changes no content |
| `create` | makes a pane or a tab, and reports the id and handle of what it made |
| `mutate` | changes a pane or a tab, or what runs in one |
| `session` | acts on the whole session, or on every client attached to it |

clap renders subcommands as one flat list and has no heading to split them by, so the listing is
built from the command tree and slotted into the help template. The names, one-liners and aliases
come out of that tree, so a command cannot appear in the listing saying something its own `--help`
does not; only the band membership is written down, and a command that nobody put in a band fails
the build.

Every subcommand's `--help` was swept against one test: an agent reading only this text uses the
command correctly. Two things that failed it are worth naming, because both are load-bearing.
`--tab-id` is the **stable** tab id — the `TAB_ID` column of `list-tabs` — while `go-to-tab` takes
the 1-based display position, and the help said only "Target a specific tab by ID" on sixteen
commands. And a `new-pane --block-until-exit` prints no `pane_id:` at all: the exit status is the
answer, only one message reaches the CLI, and a caller parsing for the id would wait for a line
that never comes.

The blocking flags are not one thing, and the difference matters to a script. `--block-until-exit`
and its `-success`/`-failure` siblings wait for the **command**; bare `-b/--blocking` waits for the
**pane**. A pane whose command has ended is held open by default, so `-b` — and a `-success` wait a
failing command never satisfies — keeps waiting until something closes the pane. Add
`--close-on-exit` when the script wants the status and not the pane.

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
plugin stays in the serialized layout. [Reloading that pane brings the plugin back once the file
returns](#reloading-a-pane-whose-plugin-never-loaded).

### Reloading a pane whose plugin never loaded

```
$ zellij action start-or-reload-plugin file:/tmp/p.wasm   # the pane says ERROR IN PLUGIN
```

The loop this is for is: a plugin fails to load, you fix the file, you reload the pane. It bailed,
twice over, and the only way back was restoring the layout — which throws the session's shape away
to fix one pane.

Both halves read the `plugin_map`, and a pane whose plugin never loaded is in no plugin map:

- **The pane could not be found.** `reload_plugin` looks a location up in the map, misses, and the
  caller falls through to *starting* the plugin — a stray second pane beside the broken one, which
  is still broken.
- **The pane could not be reloaded.** `reload_plugin_with_id` reads the plugin's config, size, tab
  index and cwd off the RUNNING instance. A pane that has none bails at the first of them.

So the fork answers both from outside the map. The failure record that already remembers what a
failed pane was meant to run is now also searched by location, so the reload finds the broken pane
instead of spawning a stray one. And a **pane placement** — size, tab index, cwd — is recorded when
a load is started, kept current by resizes, and dropped when the pane closes, so a pane with no
running instance can still say where it is. A running instance is asked first in every case, because
it is the current answer; the placement answers only for a pane that has none.

The config is the half that makes recovery actually recover: it is rebuilt from the `RunPlugin` with
`PluginConfig::from_run_plugin`, which **re-resolves the location** — so the file that has since
come back is the one the reload loads.

Verified end to end: a layout naming a `file:` plugin that is not there leaves a pane showing
`ERROR IN PLUGIN`; copying a real `.wasm` to that path and running `start-or-reload-plugin` brings
that same pane up running the plugin, at the pane's own size, with no second pane made.

This loop needed a client attached when it was written, because `reload_plugin_with_id` bailed with
"No connected clients" for a healthy plugin as much as for a failed one. It does not any more — see
[a plugin reloads in a session nobody is attached to](#a-plugin-reloads-in-a-session-nobody-is-attached-to).

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

### The commands the output convention changed

Everything below follows [the CLI output convention](#the-cli-output-convention); this is the list
of what moved to get there.

| Command | Now |
|---|---|
| `list-panes` | every column, always, plus `HANDLE`. `--all` keeps only its row-set meaning: include the panes you cannot select |
| `list-tabs` | every column, always. The gating flags stay accepted and do nothing |
| `list-clients` | gains `TTY`, `SIZE` and `CURRENT` — the fields that were reachable only through `--json` |
| `ls` | a table: `NAME STATUS CURRENT CLIENTS CREATED`. `-s` is untouched |
| `go-to-tab`, `go-to-tab-name`, `go-to-tab-by-id` | print `from:` and `to:`, each `<tab id> <tab name>`. A target nothing answers to exits 2 — including with nobody attached, where `go-to-tab` used to queue the switch for a tab that was never coming and exit 0, and where `go-to-tab-by-id` asked about the client first and so reported its miss only while somebody was attached. A tab that is really there and nothing attached to move to it is not a miss but is still nothing done: `go-to-tab-name` and `go-to-tab-by-id` both say they have no client to move, exit 2. `--no-focus` stays the existence probe, answering `id: <n>` |
| `close-pane` | `closed: terminal_3`, with or without `--pane-id`. A pane id nothing answers to exits 2 with `No pane answers to 'terminal_9'`. Without `--pane-id` on a session nothing is attached to, nothing holds the focus, so that too is a miss |
| `close-tab`, `close-tab-by-id` | `closed: <tab id> <tab name>` |
| `move-tab` | `from:` and `to:` display positions |
| `new-pane`, `new-tab`, `edit` | `pane_id:`, `handle:` and `tab_id:` where each applies, instead of a bare id. A pane no tab took is not reported at all: [the miss says so and exits 2](#a-pane-is-reported-only-once-a-tab-has-taken-it). A `new-pane` or `new-tab` `--cwd` that names no directory (joined to the caller's own directory, as the action joins it) exits 2 with `--cwd <path> is not a directory. Nothing was created.`, checked by the client before the action is sent; it used to open the pane in the session's own cwd and exit 0 |
| `launch-plugin`, `launch-or-focus-plugin` | nothing, exit 0: the plugin's pane is built after the action is answered, so there is no id to report |
| `are-floating-panes-visible` | `visible: true` or `visible: false`, exit 0 either way. Only a tab nothing answers to is a miss |
| `dump-screen` | takes its path as an argument as well as `--path`. Without `--pane-id` it prints the panes it could have dumped, on stderr, and exits 2. A `--pane-id` nothing answers to is a miss too, rather than an empty dump |
| `rename-tab --tab-id`, `edit-scrollback --pane-id`, `toggle-pane-borderless --pane-id` | a target nothing answers to exits 2 with `No tab with id 99` / `No pane answers to 'terminal_99'`, like every sibling. All three used to log the miss server-side and exit 0, which reads as a rename, an editor and a toggle that happened |
| `query-tab-names` | gone. `list-tabs` answers it |

### What a verb does when you do not tell it what to act on

A `zellij action` client is attached to nothing, so "the focused pane" resolves against whichever
client the server can find. From inside a pane that is right and is the point. From a script it is
a pane the caller has never seen. Which of those matters depends on what the verb DOES, so the
verbs are in three classes, and a new verb is put in one when it is written:

| Class | What it means with no target | Verbs | Which of them confirm |
|---|---|---|---|
| 1 | Works anywhere | focus movement, `go-to-next-tab`/`go-to-previous-tab`, the scroll and page family, `toggle-fullscreen`, `toggle-floating-panes`, `toggle-pane-frames`, `toggle-pane-borderless`, `switch-mode`, and every creating verb (`new-pane`, `new-tab`, `run`, `edit`) | no |
| 2 | The focused thing, inside the session only | `rename-pane`, `rename-tab`, `undo-rename-pane`, `undo-rename-tab`, `edit-scrollback`, `resize`, `move-pane`, `move-pane-backwards`, `move-tab`, `toggle-pane-embed-or-floating`, `toggle-pane-pinned`, `clear` | `clear` |
| 3 | Refused: name it | `close-pane`, `close-tab`, `write`, `write-chars`, `send-keys`, `paste` | `close-pane`, `close-tab` |

**Class 1 is additive**, and placement relative to wherever you are is the whole point of it. A
script calling `new-pane` is asking for exactly that. A verb whose target clap already requires is
class 1 too — `toggle-pane-borderless` cannot be called without one, so there is no absence left for
a class to reason about.

**Class 2 is the recoverable half.** Inside a pane, "focused" is the pane your hands are on and
nothing changes. From a script it exits 2 — a refusal changes nothing — and names the `--pane-id`
or `--tab-id` that answers it.

**Class 3 always names its target, from inside too** — that is what moved. Run from inside a pane,
"the focused pane" is the shell that ran the command: a targetless `write-chars` types into the
very shell you typed it in, and a targetless `close-pane` closes it. Nobody means either.
**`--focused`** (or `--current`) is how you say you did, and it is valid only where a focus exists —
from a script it is refused, naming `--pane-id`, because there are no hands there to mean it.

None of this governs keybindings. A key is pressed by somebody looking at the thing it acts on,
which is the case all three classes reason about the absence of.

### Anything that cannot be undone asks first

```
$ zellij action close-pane --pane-id build
close-pane kills whatever is running in that pane. Are you sure? [y/N]

$ zellij action close-pane --pane-id build < /dev/null
`close-pane` kills whatever is running in that pane, and cannot be undone. Nothing here can answer
a prompt, so pass `--yes` to say you meant it.
```

One rule, one implementation, one wording: **a verb whose effect cannot be undone confirms first.**
On a terminal it asks, defaulting to no. Anywhere else — a pipe, a cron job, an agent — it refuses
and names `--yes`. A script must never meet a prompt it cannot answer: a command that blocks forever
waiting for a keypress nobody is there to press is worse than either answer.

**A refusal and a declined prompt both exit 2**, on stderr, because each is a well-formed request
that changed nothing — which is [what a 2 means](#the-cli-output-convention). `kill-all-sessions` and
`delete-all-sessions` answered a declined prompt with 1 before, and now answer it with 2 like the
rest.

It covers `close-pane`, `close-tab`, `clear`, `kill-session`, `delete-session`, `snapshot rm` and
`snapshot prune`. `kill-all-sessions` and `delete-all-sessions` already prompted, and now do it
through the same helper, so the wording and the behaviour off a terminal are the same everywhere.

Each verb's sentence — what it destroys — has one home, and the `action` verbs that confirm are one
table rather than a check spread through the call sites. That is what makes the exempt list below
readable: it is the same table, read for what is missing from it.

For `close-pane`, `close-tab` and `clear` the confirm runs after the guards the client can settle
by itself — the targetless refusal and the cross-session one — so a call that was never going to be
sent is never asked about. It runs **before** the server is reached, so a `--pane-id` that no live
pane answers to is confirmed first and reported as a miss afterwards. Both exit 2 — a refusal and a
miss are two doors into the same bucket — and the sentence on stderr is what tells them apart,
which is the rule the whole `action` band already follows.

`close-pane` and `close-tab` keep their class-3 target requirement **and** gain the confirm, and
both are wanted in a script: the target proves *what*, `--yes` proves the destruction is meant.

What deliberately does not confirm:

- **`session down`** — the fork takes a snapshot first, so it is recoverable.
- **The write family** — it injects input but destroys nothing of its own; class-3 targeting is the
  guard it needs.
- **`dump-screen --path` over an existing file.** It overwrites, and that is documented rather than
  confirmed. The file is outside zellij's own state, every tool that takes an output path behaves
  this way, and `dump-screen` is a `read`-band verb — a prompt there would be the only one in a
  band whose whole meaning is "changes nothing".

### Where a new pane goes: `--new-tab`, `--near`, `--in-tab`

`new-pane` placed a pane beside the focused one, or in the tab `--tab-id` named. Three flags say it
without needing the focus to be anywhere in particular, which is what a script has:

```
zellij action new-pane --new-tab build -- cargo test    # a tab of its own, made now
zellij action new-pane --near sunny-otter -- htop       # beside that pane, wherever it lives
zellij action new-pane --in-tab logs -- tail -f app.log # into that tab, without going there
```

**`--new-tab [NAME]`** makes the tab and puts the pane in it, and reports both.

```
$ zellij action new-pane --new-tab build -- cargo test
tab_id: 4
pane_id: terminal_9
handle: sunny-otter
```

A tab arrives with a pane in it, so this is one action and not two: the command is handed to the new
tab as its first pane, which is why the tab holds the command's pane rather than a shell with the
command's pane beside it. The tab is built from the session's own new-tab template like every other
tab, so it keeps its status bars and its panes are written into the saved layout. Bare, zellij names
the tab as it names any new tab. `--cwd`, `--close-on-exit`, `--start-suspended`, `--plugin` and
`--no-focus` all mean what they mean elsewhere; `--name` and `--borderless` do not travel, because
the first pane of a new tab is described by its command and nothing else - both are refused rather
than dropped. Rename the pane once it is there.

`--new-tab` is the one placement flag that **does** move the focus: whoever is attached is taken to
the new tab, the way `new-tab` itself takes them. That is what a caller opening a tab usually means,
and `--no-focus` is how to say otherwise. An agent's `--new-tab` leaves the focus alone unless it
passes `--focus` (see "An agent's new pane or tab leaves the person's focus alone"). `--near` and
`--in-tab` never move anybody.

The flags that say where a pane goes in an existing tab - `--direction`,
`--stacked`, `--floating`, `--in-place`, `--tab-id`, `--near-current-pane` - are refused, because
`--new-tab` has already answered that question. So is bare `--blocking`: it waits for a pane to
close and cannot name a pane in a tab that does not exist yet. `--block-until-exit` and its two
siblings do work - they wait on the tab's first pane, which is this one.

**`--near <pane>`** takes any pane target — `terminal_1`, a bare integer, a handle, a uuid — and
opens the new pane beside that one, in whatever tab it lives in:

```
$ zellij action new-pane --near sunny-otter -- htop
pane_id: terminal_10
handle: quiet-pangolin
```

It is `--near-current-pane` generalised. That flag anchors to `$ZELLIJ_PANE_ID`, the pane the
command was typed in, which is the right answer from inside a pane and no answer at all from a
script that is not running in one. `--near` names the anchor instead, and the anchor's tab is where
the pane lands — so `--near` conflicts with `--near-current-pane` and with every flag that names a
tab. A target no live pane answers to is a miss, exit 2, in the resolver's own words.

The anchor must be a **terminal** pane. It travels to the server as the pane the command came from,
and that message carries a terminal id, so a target that resolves to a plugin pane is refused with a
message and exit 1 rather than quietly placed somewhere else. `--in-tab` is the way to put a pane in
a plugin pane's tab.

**`--in-tab <name-or-id>`** puts the pane in a tab that already exists:

```
$ zellij action new-pane --in-tab logs -- tail -f app.log
pane_id: terminal_11
handle: merry-narwhal
```

A value that is all digits is read as the stable `TAB_ID`, and anything else as a tab name — both
are looked up in the same `list-tabs` answer, so an id no tab holds is a miss exactly like a name no
tab has: exit 2, nothing created. A tab *named* `3` is reachable by its own id rather than by that
name, which is the one case the two forms disagree about; the names are the caller's to change.

**Nothing moves the focus** — not the caller's, and not that of whoever is attached. `--in-tab` is
`--tab-id` with a name lookup and `--no-focus` built in, because a script that puts a pane in
another tab has not asked to be taken there, and a `zellij action` client that "focuses" something
is moving a focus that belongs to somebody else. `--tab-id` is the spelling for a caller that does
want the view to follow.

### Text on stdin: `write-chars` and `paste`

```
cat prompt.txt | zellij action write-chars --pane-id sunny-otter
zellij action paste --pane-id sunny-otter -    # `-` reads stdin even from a terminal
```

The text these two write is a positional argument, and both now do without it: given none, they
read stdin to EOF. Text that reaches a pane through an argument is escaped twice — once for the
shell that types the command, once for the shell in the pane — and a multi-line prompt or a here-doc
does not survive that. A pipe carries it as it is, newlines and quotes included.

- **No argument and a pipe** reads the pipe. **No argument and a terminal** is an error, exit 1,
  rather than a command that appears to hang while it waits for a Ctrl-D nobody expected.
- **`-` always reads stdin**, terminal or not. It is how you say you meant it.
- **Empty stdin writes nothing and exits 2** — a well-formed request that changed nothing, which is
  what a miss is everywhere else in the fork.
- **The bound is 1 MiB**, and more than that is an error, exit 1. The text is delivered as
  keystrokes, so the pane's program reads every byte of it: the bound is what keeps a mistyped
  redirect from wedging a shell.
- Not valid UTF-8 is an error, exit 1, naming `zellij action write` as the command that takes raw
  bytes.
- The read happens after the refusal above, so a `write-chars` with no `--pane-id` from outside a
  pane is refused without draining the pipe.

### `wait`: blocking on a pane instead of polling it

```
$ zellij action wait build --for exit
waited_ms: 41207
exit_status: 0
```

The command a script writes instead of `send-keys`, `sleep 2`, `dump-screen`, look, `sleep 2` again.
That loop is what every agent and every CI wrapper around this fork was writing, and it is wrong in
both directions at once: too slow when the thing finished immediately, and too quick when it did
not. `wait` blocks until the pane does what you named, and then says how long that took.

Six conditions, and a `--timeout` on all of them:

- **`--for exit`** — the pane's command ends. It prints `exit_status:`, and `-` where the pane
  closed and took its status with it.
- **`--for quiet`** — the pane produces nothing for `--quiet-ms`, 500 by default. What a shell
  looks like when it has finished printing and is waiting for you again.
- **`--for match --match <regex>`** — a line the pane delivers matches. Rust regex syntax,
  unanchored.
- **`--for ready`** — the pane's shell is ready for input. It prints `ready_by:`, and says which
  signal fired. See [below](#--for-ready-a-shell-that-is-ready-for-input).
- **`--for agent-idle`** and **`--for agent-waiting`** — the coding agent in the pane stopped
  working, or is blocked on a person. From nkmk.34; see
  [`zellij_wait_for until=agent_idle`](#zellij_wait_for-untilagent_idle-wait-for-an-agent-to-stop-working).

**The exit code is about the wait, not about the command.** Met is 0, whatever `exit_status` says;
a timeout is 2, the fork's miss, and prints nothing on stdout; a regex that does not compile is 1.
This is deliberately unlike `new-pane --block-until-exit`, which exits with the command's own
status — that one made the pane and owns it, while `wait` is a question about somebody else's, and
a script has to be able to tell "the tests failed" from "I never saw them finish". A pane that
closes while a `--for quiet` or `--for match` is waiting is a miss too: the condition can no longer
happen.

**`--timeout` is 300 seconds unless you say otherwise, and `--timeout 0` waits forever.** A wait
with no bound is a hang, and a script gets one only by asking for it in those words.

What `--for match` can see is worth knowing before you write a pattern against it:

- **It matches the rendered screen, line by line, not the byte stream.** A line the terminal wrapped
  arrives as two lines, and a pattern spanning the wrap matches neither half. Anchor on a short
  distinctive string — `test result:`, not the whole summary line.
- **Lines already on screen when the wait began are the baseline**, and do not match. Only a line
  the pane delivers afterwards does. Use `dump-screen` for what is already there.
- **A line identical to one already on screen is not new.** Each render carries the whole viewport
  rather than a delta, so "new" is worked out by comparing against the last one, and on a rendered
  screen a prompt printed twice is the same line twice. Run the same `echo` twice and the second
  wait times out, however right the regex is.
- **The pattern is a regex, tested one screen line at a time**, so `^` and `$` anchor to that line.
  A timed-out `--for match` names the pattern and the two rules above in its message (from
  nkmk.35), because a miss caused by them otherwise reads as a wrong regex. The MCP
  `zellij_wait_for until=match` description says the same.

`--for exit` is the one condition that is a poll rather than a subscription — every 250ms, and
nothing about it reaches the protocol. The render stream reports a pane *closing*, and a command
pane that ends is held open instead by default: same event to a script, no message at all on that
stream. Asking the session is what covers both, and it covers a pane that closed outright as well.

`wait` is in the `read` band. It changes nothing — the band says what a verb does to the session,
not how long it takes.

#### `--for ready`: a shell that is ready for input

```
$ zellij action wait sunny-otter --for ready
waited_ms: 212
ready_by: prompt
```

The question a script asks before it types into a pane it just made: has the shell finished its
rc files and drawn a prompt? Typing earlier loses keystrokes to a shell that is still starting, or
hands them to whatever the rc chain runs. Two signals answer it, and the exact one wins:

| `ready_by` | Signal | When |
|---|---|---|
| `prompt` | The shell's OSC 133 prompt marker: `command_state: prompt` in `list-panes`. | At once, on the next 250 ms look at the session. |
| `quiet` | The pane drew a non-blank line, then nothing for `--quiet-ms` (300 by default here). | For a shell that writes no OSC 133 markers. |

- **A command that is running is not ready**, however long it is silent. When `command_state`
  reads `running`, the quiet fallback is off.
- **A missing `command_state` does not switch the fallback off.** A clear resets the pane's
  markers (`grid.rs`, the full reset), so a shell that writes them reads as having none until its
  next prompt. One that redraws its prompt without running its hooks, as zsh's `clear-screen`
  widget does, stays that way. The wait keeps both signals in play rather than latching "this shell
  has markers" and hanging. `done`, an end marker with no prompt after it, is treated the same.
- **The timeout is 10 seconds unless you say otherwise**, not 300. A shell that has drawn nothing
  in 10 seconds is a failure to report, and a caller behind an MCP request deadline cannot afford
  five minutes.
- **A pane that closes or whose command exits first is a miss**, exit 2, with the exit status when
  the pane kept one.
- **Only a prompt drawn after the last input counts** (from nkmk.33). See [below](#--for-ready-after-typing-a-command-the-prompt-must-be-fresh).

The prompt signal needs the shell to write OSC 133. zsh does when a `precmd` hook prints
`\e]133;A\a`; without one every answer is `quiet`, which is still right for an idle shell and is
fooled only by an rc file that pauses for longer than the window after printing something.

`zellij-client/src/cli_client.rs` (`ReadyWatch`, `wait_for_ready`, `pane_look`), `WaitFor::Ready`
in `zellij-utils/src/cli.rs`, and `ready_by` in the surface map. Tests: the decisions from scripted
renders and looks (prompt at once; output then quiet; a running command never quiet; a clear that
wiped the markers), the pane-list read, and the defaults.

#### `--for ready` after typing a command: the prompt must be fresh

```
$ zellij action write-chars --pane-id=terminal_7 -- 'cargo test'; zellij action send-keys --pane-id=terminal_7 -- Enter
$ zellij action wait terminal_7 --for ready --timeout 600
waited_ms: 48213
ready_by: prompt
```

Typing a command and then waiting for the shell to be ready again is "wait for the command I typed
to finish". Through nkmk.32 that had a race. Right after the Enter, the shell's last OSC 133 marker
is still the prompt it drew BEFORE the typing, and `command_state` reads `prompt` until the
shell's preexec hook writes the output marker. A wait that looked in that window answered `ready`
for a command that had not started.

From nkmk.33 the server records, per terminal pane, whether input was typed after the shell's last
prompt marker. A key, a paste or `write-chars` sets it; the next prompt marker clears it.
`list-panes --json` carries it as `"input_since_prompt": true`, written only when true and only
for a shell that writes markers, so no existing row changes. The ready wait reads such a stale
prompt as `running`: neither ready, nor quiet.

- **A fast command is not missed.** The wait needs no baseline and no transition it might sleep
  through. A command that finished before the wait began has already drawn its fresh prompt, and
  the first look answers.
- **Typed input without an Enter is not ready.** The shell has taken input and has not drawn a
  prompt after it, so the wait runs to its timeout. That is the honest answer: the pane is not
  ready for a new command.
- **A new pane is unchanged.** Nothing has been typed into it, so its first prompt counts, as
  before. `--wait-ready` and `zellij_create` behave exactly as they did.
- **What counts as input**: the bytes a key, a paste, `write-chars` or `send-keys` puts on the
  pane's pty. The terminal's own replies to the shell's queries, and focus reports, do not count.
  A mouse report does, when the program at the prompt asked for mouse tracking.
- **A shell without markers is judged as before**, by quiet. The flag is not consulted, because
  no prompt marker will ever clear it.

`zellij-server/src/panes/grid.rs` (`input_since_prompt`, set by `note_typed_input`, cleared by the
OSC 133 prompt marker), `zellij-server/src/panes/terminal_pane.rs` (set where typed bytes reach
the pty), `zellij-server/src/screen.rs` and `PaneListEntry` in `zellij-utils/src/data.rs`, and
`pane_look` in `zellij-client/src/cli_client.rs`. Tests: the grid flag across typing, a command's
output and end, and the next prompt; a typed line through `adjust_input_to_terminal` and a dropped
paste marker that marks nothing; the pane-list read of a stale prompt, which is neither ready nor
quiet, and of a fresh one, which is ready at once.

#### `--wait-ready` on the verbs that make a pane

```
$ zellij action new-pane --wait-ready
pane_id: terminal_4
handle: sunny-otter
ready_by: prompt
ready_ms: 252
```

`action new-pane`, `run`, `action edit`, `edit` and `action new-tab` take `--wait-ready`. Once the
report names the pane, the client runs the `--for ready` wait on it before it answers, and adds two
lines: `ready_by:` (`prompt` or `quiet`, as above) and `ready_ms:`, the time from the pane being
reported to it being ready. `--ready-timeout <SECONDS>` bounds the wait, 10 by default, and is an
error without `--wait-ready`.

- **A pane that is not ready in time is still reported.** The caller made it and has to be able to
  close it, so `pane_id:` and `handle:` print, the reason goes to stderr, and the exit is 2. A pane
  whose command exits first is the same miss.
- **`--wait-ready` is refused beside `--blocking` and `--block-until-exit*`**, exit 2. Those wait
  for the command to end and answer with its status in place of the report; a ready pane is one you
  type into. The refusal is in `CliAction::take_wait_ready`, not clap, because the blocking flags
  differ by verb and `run` arrives with them already folded into one condition.
- **The quiet fallback waits 1.5 s, not 300 ms.** A new shell runs its rc files before its first
  prompt, and one that prints a line and then pauses to load plugins looked quiet at 300 ms. The
  command typed next landed in the plugin load. A shell that writes OSC 133 marks is ready at its
  first prompt, so only a shell without marks pays the longer wait. `wait --for ready` keeps 300 ms:
  it follows a command, in a shell that is already up.
- **It is the client's to hold**: taken off the request before the action is built, so nothing about it reaches the action or the client/server contract.
- `action --json` carries both lines: `"ready_by": "prompt"`, `"ready_ms": 252`.

There is no `run --shell`. Running a command inside the user's interactive shell is what the MCP
`zellij_create` does by default — make a shell pane with `--wait-ready`, then type the command —
and that covers the case without a second way to spawn a command pane.

`zellij-utils/src/cli.rs` (`ReadyFlags`, flattened into the five verbs, and
`CliAction::take_wait_ready`), `src/commands.rs` and `src/lib.rs` to plumb it, and
`zellij-client/src/cli_client.rs` (`WaitReady`, `with_ready`, the wait in `start_cli_client`). The
surface map promises `ready_by ready_ms` for `new-pane`, `edit` and `new-tab`. Tests: the flag is
taken from every creating verb and only once; the refusals beside each blocking flag, and beside a
`run` whose flags were already folded; `run` and `edit` parse it; the report assembly.

### Pane handles

Every pane carries a two-word handle — `sunny-otter` — assigned when the pane is created and unique
among the session's live panes. It is the pane's **address**: the word you type to reach it, and the
word the fork shows you when it has to name a pane.

```
zellij action go-to-pane sunny-otter
zellij action dump-screen --pane-id sunny-otter
```

- **Every `--pane-id` takes one**, alongside `terminal_1`, `plugin_2`, a bare integer and a pane
  uuid. One parser serves all four forms, so a handle works anywhere an id does.
- **The pane names itself.** No creating command takes a handle: every one reports the handle the
  pane generated, and the caller addresses the pane by it. A layout may name one; see
  [handles in a layout](#handles-in-a-layout) below.
- **It survives a restore, and the uuid does not.** The handle is serialized into the session
  snapshot and the restored pane comes back under it. The uuid is the pane's *lineage* and rotates,
  because a restored pane is a new process (`restored_from` links it to the old one). Address and
  lineage answer different questions, which is why the fork keeps both.
- **It is reused after the pane closes.** Uniqueness is over live panes only. A handle you wrote
  down last week names at most one pane today, and not necessarily the same one.
- **It names a pane in one session.** `switch-session --pane-id` is the one flag that names a pane
  in a *different* session, and it takes the id forms only. A handle or a uuid would be read against
  the session you are leaving, and the number it resolved to would land on whatever pane happens to
  wear it in the session you are joining — so it is refused, with a message and exit 1, rather than
  answered wrongly. Reading it in the right session would need a cross-session query the protocol
  does not carry.
- **Where you see it**: the `HANDLE` column of `list-panes`, the `handle:` key of every creation
  command, the `list-tree` outline, both halves of a `go-to-pane` report, and the pane frame.

#### Fixed width, and no shared words

A generated handle's two words always sum to 11 letters — `ace-aardvark`, `coy-backpack` — so every
generated handle is the same 12-character width on the frame, and the title beside it does not creep
sideways from one pane to the next. `zellij-utils/src/pane_handle.rs` builds this as a filtered pair
space, lazily and once: every `(adjective, noun)` index pair whose word lengths add to 11, over the
504 adjectives and 594 nouns vendored in `zellij-utils/src/vendored/petname/mod.rs` — 58,057 pairs.
`random_handle` draws from it uniformly; `nth_handle` and `handle_space_size` enumerate the same
space in the same fixed order, so the deterministic testing handles below still hold, and each still
wraps past the end of the (now smaller, filtered) space rather than panicking.

No two live panes share a word, not just a whole handle. `zellij-server/src/pane_handles.rs` builds
the reserved-word set from every live and reserved handle's dash-separated segments, dropping a
purely numeric one — the `-2` of a suffix fallback is not a word — and a fresh draw is refused if
either of its words is already in that set (`shares_a_word`, replacing the old exact-match
`is_spoken_for` for generation). A restored handle, or one a layout names, is exempt from this check — it is taken
verbatim by `HeldHandle::claim` — so reservation is a property of *generation*, not of the registry
as a whole. One consequence worth knowing: since each generated pane spends one adjective and one
noun, a session can hold at most 504 word-reservation-generated panes at once (the shorter of the two
lists) before every adjective is spoken for and generation falls to the suffix ladder below.

`generate_handle` tries three rungs before it gives up on the fixed-width space: up to 32 random
draws against the caller's predicate, then a deterministic sweep of the whole pair space starting at
a random offset (so an unlucky run of rerolls still finds a free pair, in bounded time), and only if
every pair in the space is refused does it fall back to the pre-existing numeric suffix — `-2`,
`-3` and so on — so generation always returns something.

#### Handles in a layout

```kdl
layout {
    pane handle="build" command="cargo" { args "watch" }
}
```

No CLI verb and no MCP parameter chooses a handle: a pane names itself, and the creation report
says what it chose. A layout is the one place a name is written ahead of the pane.

- **The grammar** is up to four lowercase words joined by dashes, each at most 16 characters, at
  most 40 in all, at least one letter somewhere. That is what keeps one `--pane-id` able to take
  four forms: `terminal_1` has an underscore, `7` is all digits, a uuid is five groups. A name whose
  first word is `terminal` or `plugin` is refused too — `terminal-1` beside `terminal_1` is a name
  that gets typed wrong on the day it matters.
- **It is stored like any other handle**, which means it survives a restore: the snapshot carries
  `pane_handle="build"` and the pane comes back at that address. It also survives the trip to the
  server, which is what makes `new-tab --layout` and `zellij --layout` honour a `handle` a layout
  wrote: the client/server contract carries it alongside the pane's other declared properties.
  `restored_from` deliberately does not cross - that is provenance the server assigns, and a sender
  declaring it would be claiming a history it does not have.
- **`handle` is the spelling a person writes and `pane_handle` is what serialization writes**; both
  reach the same field, and a saved layout keeps working. A layout that names a handle another pane
  in the same layout already took loses the tie rather than failing the restore — a session coming
  back is not a place to refuse work — so the last pane to ask gets a generated name.

#### On the frame

The handle is drawn at the right of the pane's title row, the mirror of the title at the left, in
both frame styles — the full frame and the one-line row that `pane_frame_style "titles"` and
`top_only` share. It is the rightmost element and the last one offered room:

- The title is measured first and takes what it needs. A narrow frame loses the handle rather than
  truncating the name a human is reading.
- The scroll and pin indications are measured next; the handle takes what is left, joined to them by
  the same `|` separator they already use.
- It is never truncated. Half an address reaches no pane, so a row too narrow for the whole handle
  shows none of it — there is no short form.
- A floating pane is the one exception to "rightmost": its pin checkbox is a click target found by
  counting back from the right edge, so the pin keeps that edge and the handle sits to its left.

#### Testing

A random handle drawn in the frame makes a rendered-screen snapshot a coin toss, so the e2e harness
(`src/tests/e2e/remote_runner.rs`, `setup_remote_environment`) exports
`ZELLIJ_SEQUENTIAL_PANE_HANDLES=1` into the ssh shell it drives, asking every zellij server it starts
for in-order handles instead of random ones (see `names_in_order` in
`zellij-server/src/pane_handles.rs`). The `quit_and_resurrect_session` and `resize_terminal_window`
e2e snapshots, and 72 `zellij-server` plugin-system snapshots that had gone stale for unrelated
reasons (the `GoToTabName` instruction's `no_focus` field; the `stdout_message` →
`stdout_lines` rename), track that fork output. Those plugin tests are `#[ignore]`d and run only
in the End to End workflow, so a red workflow hides new drift — check it after every push.

Fixing every generated handle's width to 11 letters (above) moved which pairs `nth_handle` produces,
so it moved the deterministic output every one of those `ZELLIJ_SEQUENTIAL_PANE_HANDLES` snapshots
was pinned to. 275 `zellij-server` snapshots refreshed with the new sequence — the local
`tab_integration_tests` and `screen_tests` unit-test golden frames, mostly. The 150 `#[ignore]`d
plugin-system snapshots did not need it: none of them render a pane frame or otherwise embed handle
text. The `resize_terminal_window` e2e snapshot does: it splits a pane, so its frames carry the
handles `apt-anteater` and `dry-backyard`. Upstream's copy of that snapshot has no handles, so a
merge that takes it fails the End to End workflow; keep the fork's frame line.

The `#[ignore]`d plugin-system snapshots that Debug-print a spawn or layout instruction carry the
fork's fields too: `stdout_lines` (not upstream's `stdout_message`), `resurrected` on a
`RunCommand`, and `restored_from` and `pane_handle` on a layout pane. A merge that takes upstream's
side of such a snapshot drops them and fails the End to End workflow. Keep upstream's new fields and
add the fork's back; differences beyond those fields are real drift.

### Making a tab does not need somebody attached

```
$ zellij -s build-box action new-tab --name logs
tab_id: 1
pane_id: terminal_1
handle: chief-thrush
```

A tab is built by applying a layout, and a layout needs a size to be laid out in. On a session
nobody is attached to there was no size at all, so the whole tab-creating family — `new-tab`,
`new-tab --layout`, `new-pane --new-tab`, `go-to-tab-name --create` — made an **empty** tab,
reported a `pane_id:` for a pane that never existed, and had the tab thrown away by the next client
to attach. A script got a success and an id, and nothing was there. For two releases they refused
instead, honestly, with exit 2.

**The size was the whole of it.** Applying a layout never needed a client: the layout applier reads
the tab's own viewport, and the client id it is handed picks which pane ends up focused and nothing
else. What it needed was a number, and the only numbers the session had were in `client_sizes`,
which is keyed by client — so the last client to detach took the session's only size with it,
every tab built afterwards came out 0×0, and a 0×0 tab holds no panes.

So the session keeps a size of its own. It starts as the size the session was started with — a
detached start passes a static 50×50 — and thereafter tracks the last real client, so a detached
session lays out against the terminal it last had rather than against nothing. A tab made this way
holds its panes, survives an attach, and is resized to the terminal that turns up:

```
$ zellij -s build-box action new-tab --layout two.kdl --name split   # nothing attached
$ zellij -s build-box action list-panes
4  4  split  terminal_4  artistic-skylark  ...  0   0  40  80
4  4  split  terminal_5  relaxed-bobcat    ...  80  0  40  80
```

Tabs that already exist were never affected and still are not: a tab keeps its size when the last
client leaves, because the resize pass has no client to resize it for and does nothing.

**`go-to-tab-name` is the one place the two halves part company.** Making the tab no longer needs a
client; moving the focus still does, and a detached session has no focused tab to move. So
`--create` is answered — it makes the tab, or reports the id of the one already there — and the
vacuous focus move is passed over. A bare `go-to-tab-name`, which is *only* a focus move, has
nothing left to do and says so with exit 2. `--no-focus` is the probe form and answers with the
tab's id either way.

```
$ zellij -s build-box action go-to-tab-name logs
`go-to-tab-name` has no client to move: nothing is attached to this session.
$ zellij -s build-box action go-to-tab-name --no-focus logs
id: 1
```

The session's own startup is untouched: it built its tabs against that same session size before any
client attached, and always did.

### Pane notes: a failed pane keeps a mark

A short line the server draws on a pane's frame, saying what happened in that pane. The handle
answers "which pane is this"; the note answers "what is going on in it". Only the server sets one.

- **A failed command pane is marked.** A command pane whose command exits non-zero and is held open
  is marked `exit 7`, in the theme's error colour. The frame already said `EXIT CODE: 7` while the
  pane was held, but that line goes when the pane is scrolled or re-run, and nothing outside the
  server could read it at all. The note is the durable mark, and `list-panes` prints it. It is
  cleared when the pane is re-run.
- **A failed plugin pane is marked too**: see [A plugin that failed says so
  afterwards](#a-plugin-that-failed-says-so-afterwards).
- **`list-panes` has a `NOTE` column and `list-tree` a `note:` field**, both `error:exit 7` —
  colour and words together, because the colour is the meaning and a table without it cannot tell a
  pane that finished from one that failed. A pane with no note prints `-`. `list-panes --json`
  carries `note` and `note_color`.
- **A note is not saved into a snapshot.** It describes the pane's live state. A restored pane comes
  back unmarked, deliberately, which is the opposite of what a handle does.

On the frame it sits immediately left of the handle, and is the last element offered room:

- The title takes what it needs, then the scroll and pin indications, then the handle, then the
  note. A narrowing frame loses the note first.
- **It is truncated where the handle is dropped**, with a `…`. Half an address reaches no pane;
  half a sentence still says something.
- A floating pane's pin checkbox still owns the right edge, for the same reason it always did — it
  is a click target found by counting back from there.

### `list-tree`

```
$ zellij action list-tree
tab_id: 0  position: 0  name: develop  active: true
  handle: secure-wildcat  pane_id: terminal_0  title: zsh  command: /bin/zsh  focused: true
  handle: rapid-bass  pane_id: plugin_0  title: zellij:bars  command: zellij:bars  focused: false
tab_id: 1  position: 1  name: logs  active: false
  handle: cunning-filly  pane_id: terminal_1  title: Pane #1  command: -  focused: false
```

Every tab with its panes nested beneath it — the join of `list-tabs` and `list-panes` that you
otherwise had to do by tab id yourself. A tab with no panes still gets its line.

`--json` is the same join rather than a summary of it: each tab exactly as `list-tabs --json`
reports it, with its `list-panes --json` entries under a `panes` key. Asking for the tree never
returns less than asking the two questions separately would.

### `reopen-pane`: bring back a pane closed by mistake

```
$ zellij action reopen-pane --list
HANDLE       TAB     KIND     CWD               PREFILL
sunny-otter  agents  agent    /home/user/work   claude --resume 8f2c...
calm-fox     Tab #1  command  /home/user/work   cargo test
$ zellij action reopen-pane            # the newest; same as --last
$ zellij action reopen-pane --handle calm-fox
```

The server keeps the last 10 closed terminal panes of the session, in memory only. A restart
forgets them, and no scrollback is kept. A reopen brings one back:

- **Where it was.** Stacked in its old tab, under the focused tiled pane of a client looking at
  that tab, else under the tab's first tiled pane. If the tab closed too, a new tab under the old
  tab's name, built from the session's new-tab template. The stack anchor is explicit, so
  `--handle` and `--last` work with nobody attached.
- **In its directory, under its old handle** when no live pane holds that handle now. A taken
  handle leaves the pane with a fresh one.
- **As a shell, with one line typed and not run.** An agent pane gets its bare resume: the session
  the agent held when the pane closed (`claude --resume <id>`, `codex resume <id>`,
  `opencode --session <id>`, `pi --session <id>`), read from the same per-pane agent record
  `list-agents` reports. With no session known it gets the argv's id or the picker, by the rule a
  restored session uses. A command pane gets its own command line. A plain shell gets nothing. The
  line goes through the `resurrect_commands "prefill"` path, so it is pasted once the line editor
  turns bracketed paste on, and `list-panes` reports it as `prefilled_command`.
- **Once.** The entry leaves the list when it is reopened.

What is remembered: every terminal pane closed by `close-pane`, the close key, a plugin, or with its
tab. A pane whose own process exited is remembered only when it was an agent or a command pane - a
shell that exited was ended on purpose. Plugin panes are never remembered.

`--list` changes nothing and prints newest first; `--list --json` gives the same rows with `handle`,
`tab_id`, `tab_name`, `title`, `cwd`, `kind` (`agent`, `command` or `shell`), `agent`,
`agent_session_id` and `prefill`.

Both built-in keybinding presets bind pane mode `u` to it: `bind "u" { ReopenPane; SwitchToMode
"Normal"; }` (`"Locked"` in `unlock-first`). Upstream binds pane-mode `u` to `MutePane`; the fork
moves `MutePane` to pane-mode `Shift u`. Bare `m` is free in `default` but enters Move mode from
pane mode in `unlock-first`, so `Shift u` is the one key free in both.
`ReopenPane "<handle>"` reopens a named pane from a key. The slim-keybinds and compact-bar hints
show it. The MCP server's `zellij_create` takes `reopen: "last"` or `reopen: "<handle>"`.

Contracts: `ReopenPaneAction` is a new message at tag 906 in the fork block of
`common_types.proto`. A stock server of the same contract ignores it. The plugin API carries a
bound reopen too, so a hint plugin can see the key: `ActionName.ReopenPane = 900` and
`reopen_pane_payload = 900` in `plugin_api/action.proto`, the fork block there. A plugin
built before this drops the action from the bind, as it drops any action it cannot decode, and
keeps the rest. So an old status-bar read pane mode's `u` as a bare `SwitchToMode "Normal"`, took
it for pane mode's exit key, and lost the `Ctrl +` prefix on every hint. Every default plugin is
rebuilt in this commit for that reason; a third-party plugin built before it shows the same
symptom until it is rebuilt.

### `go-to-pane`

```
$ zellij action go-to-pane sunny-otter
from: terminal_2 tender-orca
to: terminal_0 sunny-otter
```

A visible alias for upstream's `focus-pane-id`, so the `go-to-*` family is complete. It takes any
pane target, focuses the pane **and the tab it lives in**, and reports what it left and where it
landed as `<pane_id> <handle>` — the pane mirror of what `go-to-tab` prints. A jump that landed
where it started prints only `to:` and exits 0. A target no live pane answers to exits 2 with the
resolver's own sentence.

`--no-focus` turns it into the existence probe `go-to-tab-name` already had, which is what a handle
written down last week needs before anything is aimed at it:

```
$ zellij action go-to-pane sunny-otter --no-focus
id: terminal_0
handle: sunny-otter
```

Nothing moves, and the exit code is 0 whether or not the pane is there — stdout is the whole answer,
and it is empty for a pane that is gone. A target that names no pane in any form is still malformed
input and still exits 1.

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

Upstream v0.46.0 has the same move as `Action::MoveTabToPosition`, which names its tab by id and has
no CLI verb. The flag maps onto it, so the fork adds no action and no contract message (tag 904 is
reserved). Without `--tab-id` the client first asks the session for the focused tab: the first tab
`list-tabs --json` marks `active`, else the first by position. A session with no tab exits 2 with
"The session has no tab to move." A `--tab-id` no tab answers to exits 1 with "No tab with id N".

`zellij-utils/src/input/actions.rs` (the `MoveTab` arm of `actions_from_cli`), `src/commands.rs`
and `resolve_focused_tab` in `zellij-client/src/cli_client.rs`.

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
(`cargo xtask proto`), taking tags 906-908. They also round-trip through `session-metadata.kdl`, so a
plugin reading a peer session gets them too.

### `last_output_at`: when a pane last produced output

`PaneInfo.last_output_at` is the time a terminal pane last emitted bytes, in milliseconds since the
Unix epoch. It is `null` for plugin panes and for a pane that has produced nothing since the server
started.

It means output, not attention: a human typing does not move it, nor does focusing the pane. That
makes it the cheapest liveness signal in the tree — "is this pane still working" answered without
reading a cell of its content, and without spawning a process per pane to scrape one.

The server already recorded the instant on every read from a pty, for the mobile view, and threw the
wall-clock meaning away. It now takes the wall-clock millisecond at the same moment and stores it
beside the instant, and every read reports that stored value verbatim.

**The value is stable: two reads of an idle pane return the same number.** A consumer can compare
two reads for equality and take a change to mean output. The first version derived the stamp at
read time, as the wall clock now minus the monotonic time since the output, and truncated each to
the millisecond on its own. So an idle pane's stamp moved by a millisecond between reads, and a
consumer diffing the tree saw every idle pane as busy on every tick.

Nothing pushes on output alone, so a consumer sees the value move on the once-a-second session tick
rather than the moment bytes land. That is still an order of magnitude better than sweeping panes
with `dump-screen`.

Related fix on the same path: a shell emitting OSC 7 (a cwd report) used to clear the pane's
activity flag in the pty thread. That flag gates the whole once-a-second refresh for the pane,
command discovery included, so a shell announcing its cwd at every prompt could starve its own
`pane_command` of updates. The flag now stays set — the pane did produce output — and the extra cwd
read is one the tick performs for any active pane anyway.

Proto tag 909.

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

Proto tag 910.

### `is_focused` names a client that is still here

A pane is reported focused only while a **connected** client focuses it. `zellij action list-panes`
used to answer `FOCUSED=true` for a pane the last client had been on before it detached — and on a
session nobody had ever attached to, because `session up` creates one and leaves. A consumer asking
"is anyone looking at this pane" got a yes from a session with no clients at all. Upstream
[#5468](https://github.com/zellij-org/zellij/issues/5468).

The stale answer and the useful behaviour come from the same map. `Tab::remove_client` drops the
client from `connected_clients` and deliberately leaves its entry in `ActivePanes`, which is what
puts a re-attaching client back on the pane it left. So the map is right and the question asked of
it was wrong: a CLI row now asks
`ActivePanes::pane_id_is_focused_by_connected_client`, which is the same lookup with the connected
set applied. Upstream's own PR for this issue deleted the entry instead and re-attach went back to
the first pane in the tab; nothing here moves, and a detach-and-re-attach still lands on the pane
it left.

- **`is_fullscreen` stays on the unfiltered answer.** Fullscreen is the tab's shape, not a claim
  about who is looking: a detached tab left fullscreen still is one.
- **The session snapshot is untouched.** `PaneLayoutMetadata` builds its own `focused_clients` from
  the same map without the filter, so `session down` still records where the focus was.
- **The filter is on the CLI rows only.** `PaneInfo`, which plugins read, keeps upstream's
  remembered focus: a background tab reports the pane its client last focused. `collect_pane_list`
  in `zellij-server/src/screen.rs` applies the connected-client filter where it builds the
  `list-panes`, `list-tree` and `action watch` rows. No field is added and `event.proto` is not
  touched.
- **A focus move inside a tab refreshes the manifest.** Moving focus between panes of one tab only
  renders, so `is_focused` described the old focus until something else reported. The render path
  now rebuilds the pane state when a connected client's focus moved
  (`Screen::focus_moved_since_last_report`).

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
new. With archiving off (`session_snapshot_limit 0`) `--archive` exits 2 and names the setting,
rather than reporting `Nothing new to archive.`

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

#### A session under another contract

A contract bump makes a running session
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

### An action sent while the session is still starting waits for it

`zellij attach --create-background S` returns once the server is spawned, before the session is
set up. An action sent in that moment - `zellij -s S action new-tab` right after it - reached a
server with nothing to route it to, and upstream dropped it without a word: nothing was made, and
the CLI printed nothing and exited 0. The server now parks such an action and runs it once the
session is ready, the way it already holds a resize or a key that arrives early (up to the same
ten seconds). The caller gets the usual answer, `tab_id:` and all
(`zellij-server/src/route.rs`, the `ClientToServerMsg::Action` arm).

### A pane is reported only once a tab has taken it

Upstream v0.45 reported a new pane **as soon as its pty spawned**, which was before any tab had
agreed to hold it. A tab that declined did so by dropping the pty rather than by failing, so the CLI
printed `pane_id:` and `handle:` and exited 0 for a pane that was never made, and the next command
a script ran missed:

```
$ zellij -s S action break-pane --pane-id terminal_0 --no-focus   # before break-pane was removed
tab_id: 1
$ zellij -s S action new-pane --in-tab 1
pane_id: terminal_1
handle: brave-lynx                    # exit 0, and terminal_1 is in no `list-panes`
$ zellij -s S action wait brave-lynx --for exit
No pane answers to 'brave-lynx'
```

It is one rule in two halves, and both are needed. The **session** stops asserting an id it only
asked for. Upstream v0.46.0 now does this half: `finish_pane_creation` writes the id into the
report only for a pane some tab holds, and otherwise sends the tab's reason
(`PaneNotCreatedReason`, such as "Tab not found") with exit 1. The fork carries no session-side
reporting of its own. The **client** half is the fork's: it holds each pane-making verb to answering
with a pane. No `pane_id:` line from a verb whose whole point is a pane means no pane, which is a
miss rather than a silent success:

```
No pane was created: the session took the request and reported no pane.
```

That miss exits 2. Successful creations are untouched and still print the id and the handle.

This is not a fix for one tab. An unsized tab is how it showed up — a tab no client had ever
attached to had no size to place a pane in, and `break-pane` (since removed) made one on a detached session — but a
targetless `new-pane` on such a session ghosted the same way, and so did `new-pane --plugin`. Any
future route that declines a pane is reported honestly without knowing about this.

The unsized tab itself is gone — [a detached session sizes its own
tabs](#making-a-tab-does-not-need-somebody-attached) — so that door no longer produces a miss. The
guard is what makes the next one safe, and it stays.

The client's exit is **2**: the session changed nothing the caller can address, so it sits in the
same bucket as every other miss (see [the CLI output convention](#the-cli-output-convention)). A
reason the session sent exits 1, as upstream does. The stderr sentence says which door it came
through.

#### The miss has to be a real one

Holding the client to a report makes a lost report indistinguishable from a pane that was never
made, so a report the session wrote and the client never received now reads as a miss. That is what
`new-pane` with a chosen handle (a flag since removed) did, about one call in ten: the pane was there
in `list-panes` under a generated handle while the CLI said none had been made.

Nothing about the pane was at fault. The chosen handle was checked against the live panes before
anything was created, and that check was a whole extra connection, made and closed immediately
before the one that carried the `new-pane`. Upstream v0.45 gave a new connection the lowest client
id not in use, so in a session nothing was attached to both connections were client 1 — and a route
thread announced its client's removal twice, once as the client left and once as the thread ended.
The first announcement freed the id, the next connection was given it, and the second announcement
removed **that** connection's sender. Dropping a sender writes an `Exit { Disconnect }` down its
socket, which the waiting client read as the answer to its `new-pane`.

Upstream v0.46.0 retires a removed client's id, so a stray announcement reaches no new connection.
The fork's route thread still announces its client's removal once, after its last act. The client
also stops reading a disconnect as an empty report: it is a dropped connection, said out loud, and
exits 1 — the command may well have run, and that is a different thing from a session that
answered with nothing.

The same preflight is made by `--in-tab` and by `--near`, and `zellij run`, `zellij edit` and
`--plugin` all travel the same path, so all of them were exposed to it and all of them are covered.

### Session lifecycle: `zellij session up|down|restart`

```
zellij session up      [NAME] [--fresh | --restore [ID]]
zellij session down    [NAME] [--wait-timeout SECS] [--force]
zellij session restart [NAME] [--fresh | --restore ID] [--force | --when-idle [--deadline DUR]] [--dry-run]
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

`up` run **by the init system's own job** on a session that is already healthy prints nothing. That
pass is the watchdog tick, and its `ok ... already running` line was its only output: on Linux a
journal line every 15 seconds (about 5,800 a day), on macOS an append to the
job's `.out.log` that nothing ever rotates. A user unit cannot filter it with `LogLevelMax=` —
systemd honours that for the manager's own messages about the unit, but journald reads the
per-unit level from `/run/systemd/units/`, which only the system manager writes, so the stream
output of a `--user` unit passes through at any level. What still prints is everything that means
something: the build-mismatch warning and every failure go to stderr, and a tick that creates the
session prints `up`. `up` from a shell still says `ok`.

The job says the build-mismatch warning **once per build pair**, not once per tick. While a restart
is owed, every tick wrote the same four lines to stderr: 25,615 copies, 10 MB of one Mac's
`.err.log`. The job now records the pair it warned about — the running server's executable
identity and its own, the same identities `compare_builds` decides on — in
`<state-dir>/build-warned/<session>`, and stays silent until either side changes. An upgrade or a
restart onto another stale build is a new pair and warns again. The stamp lives in the state
directory, not the cache, because a launcher's `XDG_CACHE_HOME` can differ from a shell's. A stamp
that cannot be written costs the old behaviour, never the warning. `up` from a shell warns every
time and never reads or writes the stamp, and `session status` and `session doctor` are unchanged.

None of these read `ZELLIJ_SOCKET_DIR`. The binary resolves its own socket directory, so there is no
environment variable for a launcher to get wrong, and none for a long-lived shell to hold a stale
copy of.

The wait for the created server **backs off**. Each poll forks `ps` to walk the whole process
table, and a fixed 100 ms interval spent the same hundred forks on the session that came up in
200 ms and on the one that never would — and the second is the case a launcher repeats every minute
for as long as the fault lasts. The gap doubles from 50 ms to 1.5 s over the same thirty seconds,
which is about an order of magnitude fewer forks on the failing machine and no difference on the
healthy one. Thirty, not ten: launchd was measured at 15 to 20 seconds on the fleet's Macs, and ten
reported a post-condition failure on sessions that were up moments later.
Nothing gives up early: what would escalate is the watchdog switching itself off, and that is the
one state a person cannot recover from without a shell on the machine. The failure is already loud —
the post-condition and its diagnostics go to the journal, or to the log the plist names.

`up` takes an advisory `flock` on `<socket-dir>/.<name>.up.lock` and holds it across both the check
and the creation. Without it the two are separate steps, so a `restart` typed by hand overlapping
the watchdog's minute tick had both sides find no server and both create one — two servers for a
name that allows one, reported by `assert_up` on both sides and cleaned up by neither, after which
every later `up` refused until somebody killed a server by hand. With the lock the second one waits
and then reports the session already running. A lock that cannot be taken in 90 seconds is a wedged
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
lock's own 90 seconds with room to spare — the longest legitimate hold is a 10-second down plus a
30-second wait for the server. Raise `--wait-timeout` past about a minute and a waiting `up` can
give up on a restart that is only slow, which is the race put back by hand.

**The lock could not carry the shape across a handover (nkmk.26, 2026-10-02).** When a launch agent
is loaded, the restart's inner `up` does not create the session: it releases the lock and asks
launchd to. The job it wakes runs a bare
`session up`, and a bare `up` resumes — so `restart --fresh` came back from the snapshot its own
teardown had just archived, and `restart.log` reported success. On a Mac with an agent this was not
a race but every time. `restart` now writes its shape to `<socket-dir>/.<name>.up.pending` before
the teardown, and again once the teardown is done. Whichever `up` creates the session next takes the
file and builds that shape; an explicit `--fresh` or `--restore` on that `up` still wins, and an
`up` that hands over leaves the file for the job it woke. The socket directory, beside the lock,
because the restart and the job are two environments and that is the one path both resolve the same
way. A file older than two minutes is removed and ignored: the age runs from the end of the
teardown, the job reaches its `up` in 15 to 20 seconds, and nothing waits on the lock past 90, so an
older file belongs to a restart that died. After its `up`, `restart` reads the file once more. Gone
means the shape was built, and it says `ok '<name>' came back fresh from the layout`. Still there
means whatever created the session never took it — a build older than this one, or a session that
was already up — so it removes the file, says the session did not come back as asked, and exits 1.
Unit tests: the marker's round trip, garbage refused, take-once, expiry, the precedence between the
marker and an explicit flag, and that the marker sits beside the lock.

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
`PATH`, not the server's. Both generators now write one. It was first derived — the directory the
unit's own binary was found in, then the platform default, on the reasoning that a package-manager
prefix would arrive that way on its own. [`pin_exe` broke exactly that
reasoning](#a-generated-unit-carries-the-path-of-the-shell-that-enabled-it), and the value is now
**recorded** instead.

**The launchd log paths are what the whole design rests on.** `session up` asserts the session it
created is really there and prints why when it is not; launchd sends the output of a job naming no
path to `/dev/null`, so on a Mac a session that never came back after login left no evidence
anywhere. They default to `$XDG_STATE_HOME/zellij/session-<name>.{out,err}.log` — the same state
directory as the restart log and the snapshot archive, per-user, and surviving the reboot the log is
about. `session enable` creates that directory before loading the job, because launchd will not, and
a job whose stdout cannot be opened does not run.

### A generated unit carries the `PATH` of the shell that enabled it

The entry above gave a generated unit the `PATH` `<directory of the binary the unit execs>` followed
by the platform default, on the reasoning that the directory a binary was found in is the one
directory a machine is known to keep terminal software in — so a package-manager prefix arrived on
its own and nothing had to be hardcoded.

[`pin_exe`](#a-pinned-copy-of-the-binary-pin_exe) makes that reasoning false. The binary the unit
execs is then zellij's own pin directory, which holds zellij and **nothing else**, so the package
prefix drops out along with `~/.local/bin` and every npm or bun global bin:

```
Environment="PATH=/home/user/.local/share/zellij/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
```

A server started from that unit cannot resolve `claude`, `codex` or `opencode`. What it costs:
every layout `command` pane, every `zellij run --`, `zellij edit`, `copy_command`, and every command
a snapshot resurrects — each failing with "command not found" beside an interactive pane in the same
session that works, because the rc chain fixes the *shell's* `PATH` and never the server's. A
resurrected command pane reports it as a pane held at exit status 2.

**So the `PATH` is recorded rather than reasoned about**, the same principle as the state root and
the binary path: resolve once, at `enable` time, in the environment that ran it. The unit now
carries the directory of the binary it execs, followed by the whole `PATH` of the shell that enabled
it — what that shell could resolve, the server it installs can resolve too:

```
Environment="PATH=/home/user/.local/share/zellij/bin:/opt/homebrew/bin:/home/user/.local/bin:/usr/bin:/bin"
```

Both generators take it from the same function, so the plist's `PATH` entry inside
`EnvironmentVariables` is the same string. Three rules make it predictable:

- **The binary's own directory leads**, pinned or not — the unit must be able to reach the build it
  execs whatever the recorded `PATH` says. Where the recorded `PATH` already names that directory,
  its own ordering stands: hoisting a directory the operator placed deliberately would change which
  build of *everything else* the server resolves. [The pin directory is the one
  exception](#the-generated-units-path-leads-with-the-pin-directory), because it holds nothing else.
- **Order is preserved and repeats are dropped**, so the recorded value reads as the operator's own
  `PATH` and resolves the same way. An empty entry — the `.` a `::` means to every exec — is dropped
  rather than recorded.
- **An environment with no `PATH` falls back to the platform default**, so a unit generated from a
  degenerate environment still starts a server that can run something.

It stays a **default**, so `launchd { env { PATH … } }`, a `PATH` through the launchd `keys` hatch,
and `systemd { service "Environment=PATH=…" }` each still replace it and are never written beside
it. A machine that wants to state its own `PATH` is unaffected by any of this.

**The rollout caveat: a recorded value is a snapshot.** A unit enabled before this change carries
the old narrow `PATH`, and nothing rewrites it in place. From nkmk.25 [the drift
check](#the-config-and-the-installed-unit-are-compared) no longer reports it: drift reads the
recorded `PATH` back rather than comparing it with the asking shell's (see [the read-back
entry](#the-recorded-xdg_state_home-is-read-back-not-re-derived)). The remedy is what it always
is:

```
zellij session enable <name>     # re-records this shell's PATH into the unit
zellij session restart <name>    # the server keeps the PATH it was created with
```

Both halves are needed. A server's `PATH` is fixed for the life of the server, so rewriting the unit
changes nothing until the session is created again. And the snapshot goes stale the same way
afterwards: install a new package prefix, and the unit learns about it at the next `session enable`,
not before. Until nkmk.25 drift said so, by regenerating from the asking shell's `PATH`. That was
not safe: an SSH shell's thinner `PATH` read as drift, and following the report recorded it. Now
`session enable` prints each key it changes, `PATH` among them, so a re-record is seen when it is
made. From nkmk.27 an `enable` from a thinner shell keeps the recorded `PATH` instead, and from
nkmk.32 it merges the recorded directories the shell lacks into the shell's own; see [the
entry below](#session-enable-from-a-thinner-shell-keeps-the-recorded-path).

### The generated unit's `PATH` leads with the pin directory

The entry above says the binary's own directory leads, and that where the installing `PATH` already
names that directory its own ordering stands. The second half is right for a directory the operator
placed, and wrong for the one directory zellij places itself.

A shell that carries a package prefix ahead of the pin directory records the pin directory behind
it, so the unit `exec`s the pinned build while every `zellij` the **server** resolves by name — a
`zellij run --`, a `zellij edit`, a resurrected command — is the package one. Seen on a shell whose
`PATH` put the pin directory eighth:

```
Environment="PATH=/home/user/.bun/bin:…:/home/user/.local/share/zellij/bin:…"
```

So the rule splits, and the split is narrow:

- **The canonical `pin_exe` directory is hoisted to the front**, even when the installing `PATH`
  already names it further down. That directory is the per-user data directory zellij owns, and it
  holds zellij and nothing else — so the hoist changes which `zellij` the server resolves by name
  and changes nothing else at all.
- **Any other directory keeps its place**, exactly as the entry above says. A package prefix, a
  per-user bin, a directory an operator put where they wanted it: none of them moves, because moving
  one would change which build of *everything else* the server resolves.

A hoist is the only reordering. The pin directory's later occurrence is dropped by the same dedup
that drops any other repeat, and every other entry keeps its place and its relative order. Both
generators take the value from the same function, so the plist's `PATH` inside `EnvironmentVariables`
moves with the unit's. A machine that cannot say where its own data directory is has no pin directory
to recognise, and every exe directory there takes the second rule.

It is still a **default**. A machine whose config states its own `PATH` — `launchd { env { PATH … }
}`, the launchd `keys` hatch, or `systemd { service "Environment=PATH=…" }` — is unaffected, because
none of them is generated.

A unit enabled before this change carries the old order and nothing rewrites it in place, so [the
drift check](#the-config-and-the-installed-unit-are-compared) reports it and `session enable`
re-records it — and then `session restart`, because a server keeps the `PATH` it was created with.
Only a machine that pins its binary *and* whose shell names the pin directory somewhere other than
first sees that drift. Everywhere else the generated value is byte-identical and nothing is reported.

### `session enable` from a thinner shell keeps the recorded `PATH`

The release runbook runs `zellij session enable` over SSH on each Mac. A non-interactive SSH shell
skips most of the rc chain, so its `PATH` is thinner than a terminal's. On one Mac at nkmk.26,
`enable` rewrote the launch agent's `PATH` from 17 entries to 13. It dropped `/usr/local/bin`,
`~/.rbenv/shims`, fnm's per-shell directory, `~/.orbstack/bin`, `/System/Cryptexes/…` and
`/Library/Apple/usr/bin`. An `enable` from a terminal put them back, until the next release. The
change was printed (`PATH: 17 entries -> 13 entries`), and `enable` went on all the same.

A server resolves commands against the `PATH` it was created with, so each release left the next
server unable to find what those directories hold.

`enable` now **merges**. A directory the installed unit's `PATH` records, that this shell's `PATH`
lacks, and that still exists on the disk, is kept:

- A shell that **adds nothing** keeps the recorded `PATH`, in its own order.
- A shell that **adds a directory** is written first, and the directories it lacks follow it in
  recorded order.
- A shell that drops only directories gone from the disk is cleaning up, not shrinking. It
  re-records, as before.
- fnm's per-shell `fnm_multishells/<id>/bin` is never kept. It outlives its shell on disk, so
  "still exists" says nothing about whether anything wants it.

```
$ ssh mac zellij session enable mysession
      kept 2 directories from the installed PATH that this shell lacks and that exist:
        /usr/local/bin
        /Users/user/.rbenv/shims
      to drop one, run `session disable`, then `session enable` from a shell without it
ok    service for 'mysession' is already enabled
```

The first version of this rule (nkmk.27) kept the recorded `PATH` only when the shell **added
nothing**, and re-recorded otherwise. It read one addition as permission to drop everything else.
At nkmk.31 a dotfile change put mise's shims on every shell's `PATH`. The next `enable` over SSH
added that one directory, and rewrote one Mac's agent `PATH` from 20 entries to 14. It dropped
seven that exist, among them `/usr/local/bin` and `~/.orbstack/bin`. Nothing was printed, because
the rule had stood down. From nkmk.32 the same `enable` writes the 14, then the six it lacks that
exist; the stale fnm directory is the seventh, and it goes.

Only the `PATH` is kept. Everything else in the unit is rendered as before, so a new binary or a
changed config is still written. Drift reads the `PATH` back from the installed unit, so a kept
`PATH` compares as current and nothing asks for another `enable`. A config that states its own
`PATH` (`launchd { env { PATH … } }`, the `keys` hatch, `systemd { service "Environment=PATH=…" }`)
writes that value whatever the shell, and nothing is reported.

To drop a directory that still exists on purpose, `session disable` and then `session enable` from
the shell that lacks it. A unit that is not installed has no `PATH` to keep. The state root moves
the same way.

`zellij-utils/src/session_service.rs` (`path_to_keep` and its `KeptPath`, `recorded_for_enable`,
and an `enable_files` rendering mode beside `service_files` and `comparison_files`) and
`src/session_commands.rs` for the message. Tests: the decision table for `path_to_keep` (a thinner
shell; a shell that adds one and drops several that exist; one that adds one and drops only missing
directories; the fnm directory), the 20-to-14 vector verbatim but for the home directory, an
`enable` from the SSH `PATH` rendered over an interactive unit on both platforms, which keeps the
recorded `PATH` and reports no drift, and a configured `PATH` that is written as stated.

### `session enable --dry-run`

`session enable` acts on the real init system. On macOS it bootstraps into the real `gui/<uid>`
domain, whatever `HOME` and config the caller set. So an RC proof could not run it, and a rollout
could not see the unit before it was written. `--dry-run` (`-n`) prints what `enable` would do and
does none of it:

```
$ zellij session enable mysession --dry-run
dry run: what `zellij session enable mysession` would do. Nothing here was done.
exe   /home/user/.local/share/zellij/bin/zellij
      the pinned copy that `pin_exe` asks for
pin   would refresh /home/user/.local/share/zellij/bin/zellij from this build (/usr/local/bin/zellij)
path  kept: the installed unit's PATH, because this shell adds nothing to it; this shell lacks
        /home/user/extra-bin
      PATH=/home/user/.local/share/zellij/bin:/usr/local/bin:/usr/bin:/bin:/home/user/extra-bin
file  /home/user/.config/systemd/user/zellij-session-mysession.service (would be rewritten: PATH differs)
        PATH: 4 entries -> 5 entries, first: /home/user/.local/share/zellij/bin
file  /home/user/.config/systemd/user/zellij-session-mysession.timer (unchanged)
load  `enable` would run, unless every file is unchanged and the job is loaded:
        systemctl --user daemon-reload
        systemctl --user enable --now zellij-session-mysession.timer
        systemctl --user enable --now zellij-session-mysession.service

----- /home/user/.config/systemd/user/zellij-session-mysession.service -----
...the whole unit...
```

| Line | What it says |
|---|---|
| `exe` | The binary the unit runs, and why: `--exe`, the pinned copy, the stable name on `PATH`, or this binary's own path. |
| `pin` | What `enable` would do to the pinned copy: nothing (it already holds this build), pin, refresh, refresh through the signing transaction (an anchored macOS pin), or leave it alone (a release candidate, unless `ZELLIJ_PIN_ALLOW_RC=1` lets it write a pin outside the home). |
| `path` | Where the unit's `PATH` comes from: pinned by the config, this shell's, kept, or merged. Kept and merged list the directories carried over. Then the `PATH` the unit would carry. |
| `file` | Each file and its target path: new, unchanged, or rewritten, with the keys that change. |
| `load` | The init-system commands `enable` would run. |

Then every file, in full.

- **It touches nothing.** No file is written and no directory is made. No `systemctl` or
  `launchctl` command that changes state runs, and the pin is not refreshed. The one command it may
  run is `systemctl --user show-environment`, which `enable` also asks, to find the directory the
  user's manager reads units from. On macOS, the `pin` line may ask `codesign -d`, which only reads.
- **It exits 1 when `enable` would refuse**: another job already runs `session up` for the session
  and `--force` is absent, or a release candidate has nothing at the pin to fall back on.
- **The decisions are the real ones.** The plan renders the files with the same function `enable`
  writes from, and asks the pin questions in the order the pin's writer asks them.

`zellij-utils/src/session_service.rs` (`plan_enable`, `EnablePlan`, `PathDecision`,
`PlannedChange`, and `rendered_files_in`, which renders into a given directory),
`zellij-utils/src/cli.rs` (`--dry-run` on `Enable`) and `src/session_commands.rs`
(`enable_dry_run`, `pin_plan`). Tests: a plan over a temporary unit directory leaves every file's
bytes and mtime as they were, and names the PATH decision for nothing installed, a kept `PATH`, a
merged `PATH`, and a `PATH` the config states, on both platforms. The flag parses as `--dry-run`
and `-n`.

### `session up` will not create a session in the wrong macOS session domain

macOS puts every process in a session domain, and only the graphical (`Aqua`) one carries the
context for TCC-gated resources, the login keychain, the pasteboard and notifications. A process
cannot ask for that context: it is conferred by the domain a job is loaded into, inherited by
children, fixed when the server is created, and **never changed by attaching later**.

That turns an idempotent `up` into a race. Whoever creates the session first wins the domain
permanently, and a connection over SSH runs in the `Background` domain — so connecting over SSH
before the launchd agent has run leaves a session that can never reach those resources, with
nothing reporting it.

So on macOS, when the session does not exist and a loaded launch agent for it exists, `up` asks
launchd to start that job rather than creating the session itself, then waits for the socket and
asserts as usual. With no job installed and no graphical domain it creates the session and warns
that GUI-gated access will be unavailable for the life of the server. With no graphical session at
all it says so instead of quietly creating a crippled one. Linux has no analogue and is unaffected.

**A shell that is already in the `Aqua` domain hands the work to launchd too**, which it did not
until [`session up` always creates the session through the launch
agent](#session-up-always-creates-the-session-through-the-launch-agent). The domain is only half of
what creating a server settles; the other half is which executable macOS holds responsible for it,
and that one a graphical shell gets wrong.

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

**The agent identity variables are always dropped**, whatever the config says. From nkmk.33 every
server this binary starts for a new session is started without them:

| Variable | Why |
|---|---|
| `AGENT_SESSION_ID`, `AGENT_KIND` | The agent-session contract that `zellij setup --agent-hooks` exports. |
| `CLAUDE_CODE_SESSION_ID`, `CLAUDE_SESSION_ID` | claude's own identity variables. |
| `CODEX_SESSION_ID`, `CODEX_THREAD_ID` | codex's. |
| `OPENCODE_SESSION_ID`, `OPENCODE_SESSION` | opencode's. |
| `PI_SESSION_ID` | pi's. |

Each one names the one agent whose pane created the session. A server started from a claude pane
handed that pane's `AGENT_SESSION_ID` to every pane of the new session, and a claude pane in it
reported the creator's session id. Agent detection reads these variables from a pane's process
tree, the pane's shell first, so a leaked one attributes every agent in the session to the wrong
session. The list is the identity variables the detection table reads, plus `AGENT_KIND`, so a
harness added to that table is dropped too.

The drop is in the client's server spawn, so it covers every way a session is created: `session
up`, `restart`, `attach --create`, `attach --create-background` and a plain `zellij -s`. It is
silent: it describes no choice of the operator's. A variable the detection does not read, such as
`CLAUDECODE` or `CLAUDE_CODE_ENTRYPOINT`, is not on the list; drop it with
`session_restart_drop_env`, which still adds to this list.

`zellij-utils/src/agent_detect.rs` (`agent_identity_env_names`) and
`zellij-client/src/server_launch.rs` (`server_command`, which wraps upstream's
`ServerLaunch::command`). Tests: the list holds the contract's two and every identity variable the
table reads, and nothing else; the server command removes every one of them.

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
socket. The launchd equivalent is the
[`env` block](#the-launchd-env-block-an-environment-variable-a-launch-agent-can-actually-read),
not `launchd { keys { ... } }`: a bare `SSH_AUTH_SOCK` there would be a top-level plist key, which
launchd ignores in silence.

```kdl
session_service {
    launchd {
        env {
            SSH_AUTH_SOCK "/private/tmp/com.apple.launchd.XXXX/Listeners"
        }
    }
}
```

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

#### Strict mode: a refusal instead of the warning

A warning on stderr is right for a person and wrong for a program. A controller that drives zellij
over the CLI reads stdout and the exit code, so a session on another build looks healthy to it, and
it acts on a server that may not have the fix it depends on.

**Strict mode is opt-in.** Set `ZELLIJ_STRICT_VERSION=1`, or pass the global flag
`--strict-version`. Then every command that would print the warning refuses instead:

```
error: session 'work' is running a different build of zellij than this binary, and ZELLIJ_STRICT_VERSION is set, so this command did not run.
  running: /opt/zellij/1.2.3/bin/zellij
  this:    /opt/zellij/1.2.4/bin/zellij
  Run `zellij session restart work` to bring it onto this build, or unset ZELLIJ_STRICT_VERSION (and drop --strict-version) to go ahead with a warning.
```

- **Exit code 3.** The CLI uses 0, 1 and 2 for its own answers (see the conventions at the top), so
  3 means this and nothing else. Only a pane command's own status passed through by
  `--block-until-exit` can also be 3; the stderr sentence tells them apart.
- **The default stays a warning.** An upgrade leaves every running server on the old build until
  it restarts. A refusal by default would lock the operator out of their own sessions in that
  window.
- **Only a proven mismatch refuses.** The evidence rule above is unchanged: "cannot tell" is
  silence, in strict mode too.
- **Gated:** every `zellij action` verb, `run`, `edit`, `plugin`, `pipe`, `list-agents` (with
  `--all-sessions`, each live session is checked and the first proven mismatch refuses), an attach
  to an existing session, `subscribe`, and `watch`. `subscribe` and `watch` did not check before;
  they now warn by default as well.
- **Exempt:** `ls` / `list-sessions`, `kill-session`, `delete-session`, `kill-all-sessions`,
  `delete-all-sessions`, and every `zellij session ...` subcommand (`up`, `down`, `restart`,
  `status`, `doctor`, `enable`, `disable`). They are how a caller gets out of skew, so strict mode
  never refuses them. `session up` still warns about a superseded build. Commands that never
  checked the build (`snapshot`, `list-aliases`, `web`, `setup`) are unchanged.
- **A new session does not inherit it.** The server is started with `ZELLIJ_STRICT_VERSION`
  removed from its environment, like the agent identity variables. Strict mode describes the
  caller that asked, and a pane that inherited it would refuse the operator's own commands through
  the next upgrade.
- **The flag sets the variable for the process,** so a `zellij --strict-version mcp` hands strict
  mode to every zellij it runs.

**Over ssh.** The remote CLI runs the same check against its own server, so nothing new is needed
there. But ssh does not carry the local environment: only what `SendEnv` names crosses, and the
fork's ssh invocation names nothing. So `zellij mcp`'s remote path sets it on the far side itself:
when strict mode is on locally, the remote `sh -c` script starts with
`ZELLIJ_STRICT_VERSION=1; export ZELLIJ_STRICT_VERSION`. It sets the variable, not the flag,
because an older remote binary ignores an unknown variable and rejects an unknown flag. A
controller that runs its own `ssh host zellij ...` must pass it the same way.

**The soft check** is `ls --json`: its live rows carry `server_build` (`same`, `different`,
`unknown`) and `server_version` — see [Which binary each session is actually
on](#which-binary-each-session-is-actually-on). `list-agents` and `list-panes --json` carry
neither. Strict mode adds nothing to any of them.

### `zellij session enable|disable|status`

```
zellij session enable  [NAME] [--exe PATH] [--force] [--dry-run]
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

**nkmk.25, 2026-09-29: `status` gains a `build` line.** It reported the unit, the load, the pin,
drift and whether the session runs, and nothing about which build serves it. That was left to
`session doctor`, so a script reading `status` after an upgrade saw exit 0 while a restart was
still owed. The line uses the comparison doctor's `build` line makes, `build_verdict`:

```
build     the running server is this build
build     DIFFERENT - the server runs /Users/<user>/.local/share/zellij/bin/zellij
build     this is /opt/homebrew/bin/zellij - `zellij session restart my-session` brings it onto this build
build     cannot tell - the running server's executable could not be read, or more than one server serves the name
build     no server running to compare
```

`DIFFERENT` fails the exit code, like drift and a pin mismatch: each is a step still owed. `cannot
tell` does not, because an unread identity is not proof of a stale server. CLI-side only.

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
drift     ~/.config/systemd/user/zellij-session-my-session.service is NOT what this config would write now: ExecStart differs
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

The block also carries two entries that are not directives, because each decides something about
how the install is USED rather than what the unit contains:
[`pin_exe`](#a-pinned-copy-of-the-binary-pin_exe), which decides which binary the unit names, and
`restart_via_launchd`, which decides whether a graphical `session up` defers to the agent at all —
see [`session up` always creates the session through the launch
agent](#session-up-always-creates-the-session-through-the-launch-agent).

A generated unit cannot know the local facts. systemd's answer is a drop-in directory, which is a
poor answer for the tool that generated the unit: the drop-in is invisible to it, so `status`
cannot report it, and someone reading only the config has no idea it exists. Configuration a tool
generates from belongs where the tool can see it — so it lives here, and `status` lists it.

**Raw passthrough.** A systemd entry is a literal directive line appended to the section that names
it (`unit` → `[Unit]`, `service` → `[Service]`, `install` → `[Install]`); a launchd entry is a
plist key with any plist value, XML-escaped. zellij models neither schema: a copy of two
specifications that already exist would be worse than both, and would reject every key added to
them after it was written.

**Memory and swap limits are directives too.** A session's cgroup limits go in `service`, beside
any other directive, and the generated unit carries them in `[Service]`:

```kdl
session_service {
    systemd {
        service "MemoryHigh=16G" "MemoryMax=19G" "MemorySwapMax=3G"
    }
}
```

`MemorySwapMax` matters as much as the other two: without it the cgroup may swap without limit, and a
runaway process drains the machine's swap before `MemoryMax` kills it. `session status` reports a
changed or missing limit as drift, like any directive. Every build passes the string through, so an
older build reads the same config unchanged. Set a limit here, not with `systemctl --user
set-property`: that writes a drop-in under `~/.config/systemd/user.control/`, which the generator
and `session status` do not see and which outlives a config edit. launchd has no swap limit, so
macOS has no equivalent.

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

### An unknown key in `session_service` warns instead of failing the config

An unknown TOP-LEVEL key in `config.kdl` has always been ignored, which is what lets a config reach
every machine before the binary that reads it does. A block that parses its own children lost that
property: `session_service` answered an unknown child with a config error, and a config error is
the WHOLE file, not the block.

```
× Failed to parse Zellij configuration
╰── Unknown session_service entry: "managed_session" (expected systemd, launchd, pin_exe, ...)
```

So a nested key could not be seeded ahead of the binary, and a shared config carrying one would not
load on any machine that was behind. `managed_session` (since removed) and `launchd { env }` each
paid that price.

A name this build does not know is now **kept, logged and ignored**, at every depth this block
parses — an entry of `session_service`, a section of `systemd`, a child of `launchd`:

```kdl
session_service {
    restart_via_launchd true                   // read
    some_key_from_a_later_build "whatever"     // ignored, with a warning
    systemd {
        service "Nice=-5"                      // read
        sockets "ListenStream=1234"            // ignored, with a warning
    }
}
```

**Only the unknown-NAME case softened.** A name this build knows, given a value it cannot mean, is
still the whole config failing to parse: a wrong type, a
`pin_exe` that is neither a switch nor a path, a systemd line that is not `Key=value`, and every
guard against an entry taking over what the generator owns. The operator meant that setting and is
not getting it, which is worth stopping for. An unknown name means the opposite — nobody who typed
it expected THIS build to act on it.

The warning reaches two places, because the question is asked in two. `zellij setup --check` prints
it under the config it just read, which is where someone looks when a setting appears to do
nothing:

```
[CONFIG FILE]: Well defined.
[CONFIG WARNING]: unknown session_service entry "some_key_from_a_later_build", ignored (expected
systemd, launchd, pin_exe or restart_via_launchd)
```

and the parse also logs it at `warn`, so a session already running says it too. Every warning names
the entry it read and what the block does accept, so a misspelling reads as a misspelling.

An ignored entry is **not** written back out by `setup --dump-config`. It is not part of this
build's configuration, and a dump that re-emitted it would claim otherwise.

The rollout order this restores is the one `docs/config-compatibility.md` gives for a top-level
key, now with no exception for a nested one: the config can go everywhere first and the binaries
follow in any order. It does not extend to the **command surface** — a script or a unit
calling a new subcommand still breaks on every machine that has not upgraded.

Scope is `session_service` and its children only. Upstream's own strict blocks are untouched. The
fork's other block, `resurrect_command_hints`, had the same rollout problem waiting in it until
[the entry below](#resurrect_command_hints-warns-on-a-key-it-does-not-know) softened it the same
way.

### `resurrect_command_hints` warns on a key it does not know

The rule the entry above wrote for `session_service`, now applied to the fork's other block that
parses its own children. It was the last place where a nested key could not be seeded into a
shared config ahead of the binary, because an unknown child failed the WHOLE file.

A name this build does not know is now **kept, logged and ignored**:

```kdl
resurrect_command_hints {
    claude {
        match "claude"                         // read
        env "CLAUDE_CODE_SESSION_ID"           // read
        resume_args "--continue"               // read
        resume "yes"                           // ignored, with a warning
    }
}
```

A malformed value still fails the config: a hint whose `resume_args` is not a string, or a hint
that names no `match`, `env` or `resume_args` at all. An unknown name means nobody who typed it
expected THIS build to act on it, so it is ignored.

`rewrite` stays a **known** name in a hint, not an unknown one. It is retired, and it has its own
warning saying what replaced it and whether the hint was skipped; reporting it as merely unknown
would lose that.

The warnings reach the same two places as `session_service`'s, printed as one run under the config
`zellij setup --check` just read, and logged at `warn` for a session already running:

```
[CONFIG FILE]: Well defined.
[CONFIG WARNING]: unknown resurrect_command_hints entry "resume" in hint "claude", ignored
(expected match, env or resume_args)
```

A hint's warning names the hint it was read under, because a config carries several and the entry
name alone would not say which one is short a setting. An ignored entry is **not** written back out
by `setup --dump-config`.

One implementation note worth keeping: in a hint, the unknown name is checked BEFORE the entry's
value is read. Every entry this build knows carries a string, and the old code enforced that first,
so an unknown entry with a number or a bool in it would have failed the config on a rule invented
for it on its way to being ignored.

With this, every block the fork added parses its own children the way an unknown top-level key has
always behaved. Upstream's own strict blocks are still untouched — until the entry below.

### A setting this build does not know warns and is skipped, on every config surface

The two entries above made the fork's own blocks tolerant. The rest of `config.kdl` was still
strict, and one shared config reaches machines on different builds. On nkmk.35 a single bind
naming an action a newer build added failed the whole file:

```
× Failed to parse Zellij configuration
╰── Unsupported action: ReopenPane
```

From this release one rule covers every surface. **An unknown name, or an unknown enum value, is
warned about and skipped.** The parser skips the smallest unit it cannot keep in part:

| Surface | What is skipped |
|---|---|
| An unknown top-level setting | nothing; it was always ignored, and now it is named |
| An unknown action in a `bind` | the whole bind: the actions left would do something else |
| `SwitchToMode` or `SetPaneFrameStyle` with an unknown value | the whole bind |
| An unknown key spelling in `bind` or `unbind` | that key; the other keys of the bind are bound |
| An unknown mode block in `keybinds` | that block |
| An unknown mode in `shared_except` or `shared_among` | that mode name; the block applies to the rest |
| An unknown instruction in a mode block (not `bind` or `unbind`) | that node |
| An unknown value for an enum option (`default_mode`, `pane_frame_style`, `on_force_close`, `copy_clipboard`, `web_sharing`, `explicit_theme_hue`, `nested_session_handling`, `input_while_scrolled`, `command_pane_on_clean_exit`, `host_notification_protocol`, `new_pane_default`) | the option, which keeps its default |
| An unknown child of `default_floating_size`, `ui`, `ui { pane_frames }` or `web_client` | that child |
| An unknown `cursor_style` or `cursor_inactive_style` in `web_client` | the option, which keeps its default |
| An unknown component of a theme, or an unknown color slot in a component | that entry |
| An unknown permission in `plugin_permissions` | that permission; the others are granted |

**A malformed known thing still fails the config.** That covers a wrong type for a known key, bad
syntax, and a known action with the wrong arguments (`MoveFocus "Sideways"`) — even when an unknown
action sits before it in the same bind. A typo in something real must still show.

Each warning names the file, the line and the thing skipped, in file order. `zellij setup --check`
prints them under a config it still calls well defined, and the parse logs them at `warn`:

```
[CONFIG FILE]: Well defined.
[CONFIG WARNING]: /home/<user>/.config/zellij/config.kdl:42: Unsupported action: ReopenPane in bind "Alt z", the bind is skipped
```

Layout files take the same path for the settings they carry, because a layout's top level is
parsed as config too. The layout grammar itself (`layout`, `pane`, `tab` and their properties) is
not covered and stays as strict as upstream.

Not plain warn-and-skip, on purpose:

- **`session_service` values.** No key there takes a value from a closed set: a string `pin_exe` is a
  path, and every other check is a type (`restart_via_launchd 1`). Those are malformed
  known settings and still fail. Its unknown names warn, as before.
- **`Direction` and `Resize` arguments of an action.** These are closed geometric sets, so a bad
  one is a wrong argument, not a newer name.
- **`plugin_permissions` wildcards and remote locations.** That is a security rule, not an unknown
  name.
- **An unknown property on a known action.** It is ignored silently, as before. Each action reads
  its own properties by name, and no list of what each one accepts exists to warn against.

A skipped thing is not written back out by `setup --dump-config`.

This is tolerance from this release onward only. A machine on an older build still rejects what
its own build did not know, so the oldest binary that reads a shared config still gates a change
to it. See `docs/config-compatibility.md`.

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
- it is **not cached state**: it survived `killall tccd`, a fresh server process, and a reboot.

**The refresh renames a finished copy over the pinned path.** It writes a temp file in the same
directory, makes it executable, and `rename(2)`s it into place. An earlier version of this feature
wrote through the existing file instead, on the belief that TCC keyed the grant to the inode. It
does not: `TCC.db` has no inode column, and a non-bundled client is keyed by absolute path plus a
recorded code requirement. What the in-place write did cost was real — the kernel kept the OLD
cdhash for that vnode, so the next launch died with `OS_REASON_CODESIGNING` while
`codesign --verify` called the file valid on disk, and the session simply never came back. The
rename is also atomic, and it does not fail `ETXTBSY` against a server that is executing the copy.
Linux gets the same treatment for the plainer half of the problem — a versioned path that
disappears on upgrade.

**What decides whether to copy.** The SOURCE the copy was made from, recorded beside it in
`<pin>.source-sha256` — not a timestamp, and not a comparison of the two files. The pin is not
required to stay byte-identical to its source: signing it on macOS changes it on purpose. A pin
judged by its own contents would therefore read stale forever, and `session up` runs from a watchdog
every minute, so the signature would last about that long. The pin is stale when the stamp is
missing, unreadable, or names a different source, or when the pin is no longer the file zellij wrote
beside it (see [A build copied over the pin no longer reads as current](#a-build-copied-over-the-pin-no-longer-reads-as-current)); a source that cannot be hashed at all falls back
to [build identity](#a-warning-when-the-running-session-is-a-different-build). Nothing is written on
every pass but the first after an upgrade, and the binary is around 40 MB. A refresh says so once:

```
      refreshed the pinned copy at /home/<user>/.local/share/zellij/bin/zellij
```

A refresh under a **running session** is allowed and does not disturb it. The rename leaves the busy
file in place under a name nobody holds, so the server keeps executing the build it started with
until it is restarted, and the next start picks up the new copy.

**The copy is flushed to the disk before the rename, and a flush that fails stops the refresh.**
Atomic against other processes and atomic against the power going out are different guarantees, and
`rename(2)` only gives the first. It orders nothing against the disk: the directory entry can land
while the 40 MB it names is still in page cache, and the machine that comes back up has a pinned
path holding a short file. Nothing above the pin could tell — the stamp beside it describes the
SOURCE, which is intact — so a truncated pin would read as current and be executed at every start
until somebody upgraded. So the temp file is `fsync`ed after its mode is set and before the rename,
and the directory is `fsync`ed after it, which is what puts the new NAME on the disk. The file
flush is the one operation here that is not best-effort: a copy that may not have reached the disk
must not be renamed over the only working binary there is, and refusing costs a refresh.

**The hash is cached against the source's identity, in `<pin>.source-key`.** Deciding not to copy
still meant reading 40 MB to hash the source, on every `session up` — every minute, from the
watchdog — and on every interactive launch. That was around 75 ms of each one, spent to learn that
nothing had happened. The key file records the hash the stamp carries, next to what the source
looked like to `stat` when it was taken: device, inode, size and mtime to the nanosecond. When all
five still match, the hash is not taken again.

It is a **cache, and never the answer**. The stamp remains the only thing that decides staleness,
and the key can only skip the hash, never supply one. Everything it is unsure about falls through
to hashing, which is exactly what happened before it existed: no key file, a key that does not
parse, a key recorded against a hash the stamp does not carry, a source that will not `stat`. The
stamp is written first and the key second, so a crash between the two leaves a key nothing believes.

**The `stat` in the key is the one taken BEFORE the hash, never a fresh one taken after the copy.**
The key's whole claim is "this hash came from a source that looked like this", and the two halves
have to describe the same moment. A source that changed while it was being read then leaves a key
the next pass cannot match, so the hash is taken again and the change is caught. Re-`stat`ing after
the copy looks safer and is the opposite: it files the OLD hash under the NEW identity, the next
pass matches, skips the hash, and calls the pin current for as long as the source sits still.

Two consequences worth stating.

- **A separate file, not a second line in the stamp.** `<pin>.source-sha256` is still exactly
  `<hash>\n`, compared with a `trim()`. A build from before this change reads it as it always did.
  Appending to the stamp instead would have made every older binary on the machine call the pin
  stale and copy 40 MB to correct it.
- **The blind spot.** A source rewritten IN PLACE — same size, same inode, mtime put back — is one
  the key cannot tell from the source it recorded, so the pin is left alone though its source now
  holds different bytes. Nothing short of hashing every pass can see that; the trade is the 75 ms.
  It takes a writer that preserves all three together, which no package manager does: `install` and
  `cp` do not, and `cp -p` keeps the mtime but only writes through the same inode when the target
  was not unlinked first. Deleting `<pin>.source-key` puts the pin back under the hash's judgement,
  and `zellij session doctor --fix` then settles it. An upgrade is caught the ordinary way, because
  an unpacked build is a new file and a new file has a new inode however its mtime was preserved.

**Once the launcher runs the pin, the watchdog cannot be what notices an upgrade.** This is the
ordinary configuration — it is what `pin_exe` is for — and the consequence is easy to read past.
`session up` refreshes the pin from the binary it is itself running. Started by the launcher, that
binary IS the pin, so the stamp is compared against the file it was taken from and always agrees.
Every pass of the minutely watchdog therefore says the pin is current, however old it is, and it is
right to: nothing in that process has ever seen the package.

An upgrade reaches the pin from **any zellij run off another path**, which in practice means it
reaches it quickly:

- an interactive `zellij` — the launch resolves the server binary through the pin and refreshes it
  on the way past, which is the common case and needs nobody to remember anything;
- `zellij session up` or `zellij session doctor --fix`, typed in a shell, where the binary on `PATH`
  is the new one;
- `zellij session enable`, which installs the copy before it writes the unit.

The refreshed copy still does not reach the **running** server, which keeps the build it started
with until `zellij session restart`. So the honest summary of `pin_exe` on an upgraded machine is:
the package is new, the pin becomes new the next time a shell runs zellij, and the session becomes
new when it is restarted. Nothing here detects an upgrade on its own; every step is driven by
something the user did.

**A source that is the target is refused outright**, and on macOS that is not a nicety. The pin is
signed there, so it deliberately differs from the source the stamp was taken from. Allowed through,
the self-compare finds a stamp that does not match the pin's own bytes, calls the signed pin stale,
copies it over itself — and re-stamps it with the signed copy's hash. The stamp then names the pin
instead of the package, so the next run off `PATH`, with the package binary unchanged and no
upgrade anywhere, reads it as stale and copies the unsigned package over the signature. Every grant
that signature held goes with it, silently, and the machine has to be signed again. Refusing the
self-compare costs nothing — there was never a copy worth making — and it is what makes the
paragraph above true rather than nearly true.

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

Since nkmk.25, a client with no pin recorded starts a server from `own_exe_for_spawn()` rather than
`current_exe()`, so a client that outlives an upgrade on Linux does not exec a ` (deleted)` path.
The choice of binary (`record_pinned_server_exe`, `server_exe`) lives in
`zellij-client/src/server_launch.rs`, beside upstream's `ServerLaunch`.

### `zellij session doctor`

```
zellij session doctor [NAME] [-n|--dry-run] [--fix|--no-fix] [--sign|--no-sign] [--regranted] [--exe PATH]
```

Everything that has to hold for one session to come up and stay up, checked in one pass. It repairs
what a program is allowed to repair and names what only a person can:

```
Changed
  pin       refreshed the pinned copy at ~/.local/share/zellij/bin/zellij
            the running session keeps the old copy until it is restarted
Already correct
  path      /opt/homebrew/bin/zellij leads to this binary
  config    ~/.config/zellij/config.kdl parses
  socket    /var/folders/xy/.../T/zellij-501/contract_version_1
  agent     org.zellij.session.work is loaded in gui/501
  domain    the server is in the graphical session, so its panes can reach TCC
Needs you
  fda       the server does NOT have Full Disk Access
            every pane sees "Operation not permitted" in a protected directory,
            whatever it runs. No program can grant this — Apple offers no API for it.
```

Exit 0 when `Needs you` is empty, 1 otherwise. That is the whole of the contract, so a script can
call doctor and read the answer without parsing it. There is no `--json`; no `session` verb has one.

**It never takes the session down.** A pin refresh and a rewritten plist both take effect at the
next start, so doctor makes the change and *says* a restart is needed. `zellij session
restart` is that command and it belongs to the person whose panes are in there. For the same reason
a drifted unit is reported rather than rewritten: on launchd a rewrite means `bootout` then
`bootstrap`, which stops the job.

Nothing is re-implemented. The unit, its load state and its drift come from the same calls `session
status` makes; the session from the same `SessionFacts`; the pin from the same `install_pinned_exe`.
The two commands cannot describe one machine differently.

**Everywhere**: which `zellij` a shell runs and whether it is this one; whether the config loads;
the socket directory, and any server serving this name from another one or under another contract
version; leftover wrapper scripts; the unit, its load state and its drift; one server and only one;
whether a dead session's saved layout is holding the name; whether the running server is this build; the pin, and the temp copies an interrupted refresh left
beside it.

From nkmk.30, the `path` line compares builds with `compare_builds`, as the `pin` line does, not
paths. Doctor run from the pin finds the package's binary first: another file, the same build. It
used to call that "a DIFFERENT build"; now it says "the same build as this binary".

The `config` line names the file the load beneath it read: `--config`, else `--config-dir` (which
is how `ZELLIJ_CONFIG_DIR` arrives), else the default search. Until nkmk.24 the name came from the
default search alone while the load honoured `--config-dir`, so a doctor run under a scratch
`ZELLIJ_CONFIG_DIR` reported the real config as the one that parsed.

**nkmk.25, 2026-09-29: the `build` line says "cannot tell" when it cannot.** It used to be OK
whenever `build_mismatch_warning` came back empty. That silence covers two answers: the same build,
and a comparison that could not be made. So doctor said "the running server is this build" about a
server whose executable it never read. `check_build` now asks `build_verdict`, which calls
`compare_builds` directly. Only `Same` is OK. `Different` reads as before. A comparison that could
not be made is its own `Needs you` line, the way an unanswered Full Disk Access probe is:

```
Needs you
  build     cannot tell whether the running server is this build of zellij
            the running server's executable could not be read, or more than one server serves the name
            `zellij session restart work` puts it on this build either way
```

The note names what could not be read: this binary, the server's executable, this binary's own
file replaced since it started, or two files that neither a build id nor a size tells apart. It
costs a `BuildVerdict` enum beside `build_mismatch_warning` in `session_lifecycle`, and nothing on
any contract.

A leftover is narrow on purpose. A script in `~/bin` that merely calls zellij is a companion tool,
not a fault, and a `zellij` there that resolves to this very binary is where zellij is installed —
neither is reported. Two shapes are: a different build taking the name, and a script that sets
`ZELLIJ_SOCK_DIR` before zellij can resolve it. Both are reported and never removed; the `rm` is
printed for the user to run.

**Doctor is what sweeps the abandoned pin temps.** A refresh writes `.zellij.pin.<pid>.tmp` and
renames it. Killed between the two — an OOM kill, a reboot, a power cut — it leaves 40 MB behind
for good, because the next refresh writes a new one under a new pid rather than reusing it. Nothing
else on the machine knows what the file is.

```
Changed
  pin       removed 2 abandoned temp copies in ~/.local/share/zellij/bin, holding 79.4 MB
```

Two gates, and the first is the one that matters. **A temp whose pid is still running is never
touched**, because it belongs to a refresh that is still copying into it, and removing it would
leave that refresh renaming a name nothing holds; `kill(pid, 0)` answers that, and `EPERM` counts
as alive. **A temp younger than an hour is left alone** as a second belt, for a temp being written
by a process nothing has observed yet. Neither gate covers a recycled pid, and the age gate is not
the one that would: a pid recycled onto a live process makes `kill(pid, 0)` say yes at any age, so
that one temp is kept for good. The sweep reclaims space; it does not promise to.

**The `.zellij.sign.` temps of the macOS signing flow ask the same two questions.** A separate
prefix and a separate call site, so neither sweep takes the other's files, but one implementation
of the gates. The signing sweep used to remove every `.zellij.sign.*.tmp` it found, which is the
one thing a sweep must not do — the temp of a signing run happening right now is named the same
way, and taking it leaves `codesign` writing into a deleted inode and that run renaming a name
nothing holds.

It is doctor's job and **not `install_pinned_exe`'s**. The install path runs before anything takes
a lock, on every `session up` and every interactive launch, so two of them overlap routinely — and
a sweep there would be one refresh deleting another's temp file mid-copy. Doctor is the pin path a
person runs on purpose, one at a time.

**Linux** adds what only systemd knows: whether the timer is armed — loaded and armed are different
states, and a disarmed timer beside a healthy install is how a session stops coming back — and how
the last run ended, read from `Result=` rather than `ActiveState=`, which cannot tell a failed run
from one that has not happened. Twenty journal lines are quoted under a failure and none under a
success. Signing and TCC report `n/a on Linux` rather than being left out: an absent line reads as
"checked and fine".

**macOS** adds `TMPDIR` against `getconf DARWIN_USER_TEMP_DIR` — reported, never fixed, because the
value came from whatever started this shell — the launch agent's label, and two questions asked from
inside a floating pane that closes itself: which session domain the server is in, and whether it has
Full Disk Access (and, from nkmk.41, the other privacy grants: see [The privacy
grants](#the-privacy-grants-macos)). Both are asked in a pane because neither is about doctor's own process: macOS
judges a terminal-launched process against the terminal's grants, while a pane inherits the server's
domain and is attributed to the server's executable. They are skipped when the session is down,
where the answers would be this terminal's.

Full Disk Access is asked by OPENING the system TCC directory,
`/Library/Application Support/com.apple.TCC` (`ls` it). `[ -r ]` calls `access(2)`, which reads
the permission bits, while TCC refuses at `open(2)` — so a test on the bits answers "readable" on a
machine holding no grant at all. A directory that is not there is reported as unknown rather than
as a refusal, and neither the pane nor the client that opens it can outlast the
probe's five-second deadline: a client talking to a wedged server never returns on its own.

`--dry-run` withholds every fix, and the pane probe is the one thing it still does: it is a
question, not a repair, and the answer cannot be had any other way. Expect a floating pane to
appear and close itself on a `-n` run against a live session. Nothing else on that path writes —
the pin is compared and not copied, and nothing is signed.

#### The privacy grants (macOS)

From nkmk.41 the pane probe also asks the server's other privacy grants. A grant belongs to the
responsible process, the server, and a program started in a pane is judged against the server's
grants. So the probe runs doctor's own binary in the pane as `zellij session doctor --grant-probe
check` (a hidden flag), between the `manager=` and `fda=` lines. It writes `a11y=` and `screen=`,
each `yes`, `no` or `unknown`. Each framework call gets two seconds, so a stalled call never
reads as a Full Disk Access refusal.

| Line | Grant | Check | Request |
|---|---|---|---|
| `fda` | Full Disk Access | open the system TCC directory | none |
| `a11y` | Accessibility | `AXIsProcessTrusted` | `AXIsProcessTrustedWithOptions`, prompt on |
| `screen` | Screen Recording | `CGPreflightScreenCaptureAccess` | `CGRequestScreenCaptureAccess` |
| `appmgmt` | App Management | none | none |

- **A missing grant is `Needs you`**, with the Settings path and its
  `x-apple.systempreferences:com.apple.preference.security?Privacy_*` link. A request never turns
  a grant on, so the line stays `Needs you` after one.
- **An unknown grant is `Already correct`** with the reason, as an undetermined Full Disk Access
  is. App Management is always unknown: macOS has no check for it, and the only probe is a write
  into another app's bundle, which doctor never tries. Its line names the pane to look at.
- **Automation (Apple Events) is not checked.** macOS asks for it per target app at first use,
  and no target is configured anywhere.
- **`--fix` acts on one grant per run**: the first missing one in the order of the table. Where
  macOS has a request call, doctor runs `--grant-probe request-accessibility` (or
  `request-screen-recording`) in a second floating pane, which lists the server in that pane and
  shows macOS's dialog. Then it runs `open` on the grant's Settings link. The other missing grants
  say they come after it.
- **A run with no terminal on stdin opens nothing**, as the setup script never did: nobody may be
  there to answer the dialog. Its note says to run `--fix` from a terminal. `--dry-run` and
  `--no-fix` say what the fix would do.
- **The fix is the default**, like every doctor fix: a plain `zellij session doctor` from a
  terminal opens a missing grant's pane.
- **Newer macOS renamed two panes.** macOS 27 shows Accessibility as Device Control & Data Access,
  and Screen Recording as Screen & System Audio Recording. The Settings line names both, and each
  link opens the pane on either version. All four links were checked on macOS 27.
- **What was proved live, and what was not.** On a macOS 27 Mac, each check answered as the TCC
  database says for three responsible processes: an ssh session, Terminal, and a scratch server
  through `doctor --dry-run`. `--grant-probe request-screen-recording`, run in Terminal there,
  showed macOS's dialog, so `CGRequestScreenCaptureAccess` works on macOS 27. The Accessibility
  request and a full `--fix` run in a server pane were not fired live. Unit tests cover which grant
  a run acts on and what it says.
- A call that does not return in time prints `unknown`. A binary that cannot run in the pane prints
  no grant lines, and both grants read as unknown.

#### Signing the pinned copy (macOS)

macOS keys a grant for a non-bundled program to an absolute path plus a code requirement. An
unsigned or ad-hoc-signed binary has no identity to name, so the requirement is a hash of the
*code* — and the next build voids every grant, silently, until a pane fails in a directory that
worked yesterday. A signature anchored on a certificate ends that.

**From nkmk.30 the release is signed once, in CI, and nothing on a Mac signs.** `release.yml`
signs the macOS binary with the fork's Developer ID (see [Releasing](#releasing)). Doctor and the
pin's writer only judge signatures and copy builds:

| The build being pinned | What happens |
|---|---|
| a release build, carrying our Developer ID | pinned as it arrived: copy, verify, rename — see [A release build is pinned with its own Developer ID](#a-release-build-is-pinned-with-its-own-developer-id-and-nothing-re-signs-it) |
| anything else (a local `cargo build`, a source-formula build) over an ad-hoc or unsigned pin | a plain copy, with whatever signature the linker gave it |
| anything else over an anchored pin | refused; the signed pin stays, on the previous build |

And what doctor says about the pin it finds:

| The pin | Doctor |
|---|---|
| anchored, and it verifies | `Already correct` — unless a re-grant is owed, see [the entry below](#doctor-keeps-the-certificate-the-grants-name-and-remembers-a-switch-until-the-re-grant-is-made) |
| anchored, and it does not verify | `Needs you`: it holds no grant; remove the pin and run `doctor --fix` from the brew release |
| ad-hoc or unsigned | `Needs you`: not a release build, so the pin holds no grants; install the brew release |

The `Needs you` for a build the release did not sign reads:

```
Needs you
  signing   <pin> is not a release build, so the pin holds no grants
            its signature is ad-hoc: the requirement names its own code hash
            nothing is signed on this machine; the release is signed once, in CI
            install the brew release to get a signed pin, then `zellij session doctor --fix`
```

**A pin anchored on a retired local certificate is left as it is.** Until nkmk.30 doctor signed
every refreshed pin itself (see the nkmk.30 ledger entry for what went). Those pins still verify,
and doctor does not fight them: the next release build refreshes the pin as it arrived, and that
switch records the owed re-grant through the same record.

The identifier is the constant `org.zellij.nkmk`. **Changing it voids every grant on every machine**,
because it is part of the requirement macOS recorded — which is why it is a constant and not a
setting. The release signs with that identifier.

**Verification runs `codesign --verify --strict --verbose=2`, and the verbosity is load-bearing.**
Plain `codesign -v <path>` returned 0 on a pin that `codesign -v --verbose=2 <path>` rejected with
`does not satisfy its designated Requirement` and exit 3: the designated-requirement check is what
the second verbosity level adds. The message is matched as well as the exit status, because the
exit status is the half already observed reporting success wrongly. Two questions are asked
because they fail apart: a signature can verify while its requirement still names the code hash,
and a requirement can read perfectly while the binary does not satisfy it. The same verification
is run on the pin before it is called healthy, and on the copy of a release build before it is
renamed into place.

**`codesign --verify` exiting 0 says nothing about whether a grant survived.** It answers "is this
signature intact", not "is this the requirement macOS recorded" — an ad-hoc pin verifies perfectly
and holds nothing past its next rebuild. The requirement is what doctor prints and compares.

Every child doctor runs is started with `setsid(2)`, in a session of its own with no controlling
terminal. `security(1)` writes its password prompt to the controlling terminal, not to stdin, so at
0.45.0-nkmk.8 a `security` call blocked forever in a pane with one line on the pane's terminal. No
signing step runs `security` any more, but a tool that would prompt still fails fast and says why.

**The refresh and the verification are one transaction, and they have to be.** Doctor once
refreshed the pin first and signed it second, so a run that could not sign had already replaced a
properly anchored pin with a fresh ad-hoc copy, and then reported `the pinned copy is untouched`.
Measured on a real Mac at 0.45.0-nkmk.9. So when the pin is **anchored** and out of date, the copy
is handed to the signing step: it copies a release build into its own temp, verifies it, and
renames only on success. Any other build is refused there. A refusal leaves the previous signed
pin exactly where it was — previous build, signature intact, grants intact — and says so. The
deferral is decided by one function that both steps ask, because a disagreement in the "skip"
direction would drop the refresh with nothing reporting it. A pin that is already **ad-hoc** is
refreshed first, by the plain copy: it holds no grant a rebuild could keep.

**One writer, and it never replaces an anchored signature with anything but a release.** The rule
lives at the write, in `install_pinned_exe`, because the pin has two callers: `assert_pinned_exe`
(`session up`) and `server_exe_for_interactive_launch`, which runs on every interactive launch. The
rule was first written into one caller, and on a real machine the other overwrote an Apple
Development signature in the same command. `install_pinned_exe` asks `pin_is_anchored` — the same
predicate `refresh_belongs_to_signing` asks — the moment it is about to overwrite an existing pin,
and there is no parameter with which a caller can ask for anything else:

| What the pin is | What ANY caller gets |
|---|---|
| current, or ad-hoc, or unsigned | the plain copy, unchanged |
| anchored and stale, the new build is the release's own Developer ID | installed as it came, nothing signed (`PinOutcome::AsReleased`) |
| anchored and stale, any other build, or `--no-sign` | **nothing is written**; the existing pin's path is returned (`PinOutcome::Kept`) |

`session up` and doctor print the second row as `refreshed the pinned copy with the release's own
signature at <pin>`. In the last row the session comes up on the PREVIOUS build with its grants
intact, and the writer says so in the signing step's own words — quoted rather than summarised, so
the line matches what `zellij session doctor` says next. An older build that can still read the
files beats a newer one whose grants can only be given back through a GUI dialog. The refusal is
said ONCE per pin per process: `session up` asserts the pin and then launches a client that
resolves the server binary through the pin again.

What the writer cannot work out for itself is stated once per run instead of passed as a parameter:
`PinSigningPolicy` carries whether a signed pin may be replaced (`false` only for `session doctor
--no-sign`) and whether the operator confirmed an owed re-grant (`--regranted`). A run that states
no policy — a plain `zellij` launch — lets a release replace the pin. **`--sign` and `--no-sign`
stay**, with the meaning narrowed: nothing signs, so `--no-sign` means "leave a signed pin exactly
as it is, even for a release build". `--sign` is the default and is accepted so that scripts which
pass it keep working.

On Linux and anywhere without `codesign`, nothing changes: no pin reads as anchored, and the copy
runs as it always did.

**The rename is not coordinated with `session up`, but it does not lose quietly.** The pin's length
and mtime are read before the copy and compared immediately before the rename. A pin that was
replaced underneath — or removed, or created where there was none — makes the run discard its temp
and refuse, naming `zellij session doctor --fix` as the way to finish. Deliberately a comparison
and not a lock: ordering two renames into one directory would need a lock both commands take, for
a race that needs the two commands typed seconds apart.

**A re-grant is asked for only when the requirement actually changed**, and the question is put to
the two requirement TEXTS — the one read off the pin before and the one the new build carries. A
release over a pin already on the release's requirement carries the same requirement, so the
ordinary case, an upgrade on a machine already set up, needs only the restart. When it did change,
the follow-up is given in the order that makes it one pass: re-grant Full Disk Access,
Accessibility and Screen Recording for the pin's exact path **first**, then `zellij session
restart`, so the new server comes up already holding them.

**Doctor run from the pin looks past the pin (nkmk.25, 2026-09-29).** With `pin_exe` on, the pin
directory is first on `PATH`, so a bare `zellij session doctor` usually runs the pin itself. The
`pin` line then compared the pin with itself and said it "was made from this build", which is true
of every pin. After a package upgrade that line read as clean while the pin held the old build.

Run from the pin, the line now asks the first `zellij` on `PATH` that is not the pin and not in the
pin's directory, the same lookup the tab-bar badge uses:

```
Needs you
  pin       <pin> holds an older build than the one installed at <package path>
            `<package path> session restart mysession` refreshes the pin from that build
            this command runs from the pin, so --fix here would copy the pin onto itself
```

With no such `zellij`, it says the pin cannot be judged from itself, and exits zero. The note on
the launcher line no longer offers "this command with --fix" as the way to reach the pin when this
command IS the pin, because that copy would be the pin onto itself. Two unit tests, on a scratch
`PATH`: a newer build past the pin, and a `PATH` that holds only the pin.

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
drop a whole entry to fit it. The sessions browser's all-keys list (`F1`) names `<Del/Ctrl k>` too.
Upstream's bulk delete of exited sessions (`Shift Del`) does not cover this: it deletes exited
sessions only, and it needs a Delete key as well.

### The server names the sessions its scan cannot list

The scan behind the plugin session list and `zellij ls` pairs each socket with a
`session-metadata.kdl` under the session-info cache directory. It drops any session whose file is
missing or does not parse, silently, and reports success with a shorter list. The server now names
each session it drops:

```
WARN  session my-session is running but cannot be listed: its metadata file does not parse: ...
```

One line per session as it enters that state, and another only if the reason changes. The scan runs
on every poll of every session manager, roughly once a second each, so an unfiltered warning would
be thousands of identical lines an hour. A session listed successfully again is forgotten, so a
recurrence is reported rather than swallowed. `report_dropped_sessions` in
`zellij-utils/src/sessions.rs`.

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

The pinned copy is the answer to the versioned path, and it only works if the pin is what starts
the server — see [`session up` always creates the session through the launch
agent](#session-up-always-creates-the-session-through-the-launch-agent), which is what makes that
true for a `restart` typed at a terminal.

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

`PaneFrameStyle::TopOnly` carries protobuf value **900** in both `pane_frame_style.proto` and
`event.proto`, in the fork's reserved block.

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

`PaneInfo` gains `string restored_from = 905`, so `list-panes --json` reports it for free.

`PaneInfo` crosses the plugin API, so `event.proto` gains `string uuid = 904` and its generated Rust
is regenerated (`cargo xtask build`). `list-panes --json` gets the field for free - `PaneListEntry`
flattens `PaneInfo`. The client/server contract has no `PaneInfo` and is untouched.

### `ListClients` lists every attached client, with its terminal size

The plugin-facing `ListClients` answered with no clients, whatever the truth was. The server builds
that answer from a layout snapshot, and the plugin path removed the requesting plugin's own pane
from the snapshot first. That rule belongs to the layout *dump*, and this path inherited it by
passing `Some(plugin_id)` where `zellij action list-clients` passes `None`. A plugin opened as a
floating pane is the focused pane of every client in its tab, so removing it removed the whole list.

The list also read one pane layer per tab — floating when floating panes were on screen, tiled
otherwise. Floating visibility is per tab while focus is per client, so a client focused on a tiled
pane in a tab where someone else had floating panes up had no row anywhere. Both layers are read
now, the off-screen one first, so a client remembered by both is described by the layer it is
actually looking at.

Each client's terminal size rides to the list on `SessionLayoutMetadata`, from `Screen`'s own
`client_sizes` map — the one `recompute_tab_size` takes the minimum of. `zellij action
list-clients` prints it as `SIZE` and as `terminal_size` in `--json`. The plugin API does not carry
it: `ClientInfo` tags 100 and 101 in `event.proto` are reserved; `ClientInfo.tty` is tag 900.

### Resume hints for serialized commands (`resurrect_command_hints`)

A resurrected pane holds the command it was running, and `ENTER` re-runs it. For a tool keyed by a
session id — a coding agent, a REPL that keeps state — re-running the bare command starts a *new*
session and the old work becomes unreachable. The pane comes back; what was in it does not.

A hint names a command, the environment variable that tool exports, and the arguments to **add** to
the command line when that variable is found among the pane's processes:

```kdl
resurrect_command_hints {
    claude {
        match "claude"
        env "CLAUDE_CODE_SESSION_ID"
        resume_args "--continue"
    }
    opencode {
        match "opencode"
        env "OPENCODE_SESSION_ID"
        resume_args "--session {}"
    }
}
```

`match` compares against the **basename** of the recorded command, exactly, so one hint covers both
`claude` and `/opt/homebrew/bin/claude` and does not cover `claude-code`. `resume_args` is split on
whitespace and appended to the observed argument list — it is not a shell string, so quoting and
pipes mean nothing — with `{}` standing for the variable's value. The placeholder is optional: a
resume flag that needs no id carries none. The first matching hint wins; the block names are labels
only.

**The observed command line is the ground truth and is never replaced.** A hint only appends, so the
pane comes back holding a command it really ran, flags and interpreter path included. It appends
nothing when any of its words is already in the observed arguments: a pane started as
`claude --continue`, or as `claude --resume <id>`, already says how it resumes, and the argv beats
anything a hint could reconstruct.

That holds for a command this build has no session words for. For a known harness - `claude`,
`codex`, `opencode`, `pi` - a hint's words replace the whole observed argv from nkmk.32: the pane
holds argv0 and the hint's words, nothing else. See [A restored session offers to resume its
agents, and only its agents](#a-restored-session-offers-to-resume-its-agents-and-only-its-agents).

That is also why the variable is a **detector**, not a source. It proves the pane is running the
tool; its value reaches the command only through an explicit `{}`. `CLAUDE_CODE_SESSION_ID` in
particular is not the id `claude --resume` takes — the resumable id is the transcript file name, and
the variable can carry an internal or subagent session id read from any process under the pane. A
hint that fed it to `--resume` recorded an id that resumed nothing. `--continue` resumes the newest
session for the recorded cwd and needs no id at all.

#### Migrating from `rewrite`

`resume_args` replaces the earlier `rewrite` entry, which held a whole command line. **Edit every
config that still says `rewrite`** — the two blocks in this fork's own documented example become:

| block | was | becomes |
|---|---|---|
| `claude` | `rewrite "claude --resume {}"` | `resume_args "--continue"` |
| `opencode` | `rewrite "opencode --session {}"` | `resume_args "--session {}"` |

Note the value changes shape, not just the key name: `rewrite "claude --resume {}"` carried the
command name, and `resume_args` must not — the command comes from the pane.

Until that edit lands, an upgraded binary **warns and skips the hint**, and loads the rest of the
config normally. The warning names `resume_args` and goes to the zellij log. A hint block carrying
both keys uses `resume_args` and warns about the `rewrite` beside it.

Retiring the key this way, rather than refusing it, is deliberate. A config error in this block is
not degradable: `Config::from_kdl` fails the whole file, and every path into a session — `zellij`,
`zellij attach -c`, the session unit — prints the parse error and exits before the terminal is
touched. Refusing `rewrite` would therefore have stopped every machine whose config still carried
it, over a key that only decides how nicely a pane comes back. A skipped hint costs a resumable
command; a refused config costs the session. That argument is now the whole block's: [an entry this
build does not know](#resurrect_command_hints-warns-on-a-key-it-does-not-know) is
warned about and ignored rather than refused. `rewrite` is not one of those — it is a name this
build knows, and the warning above is its own.

A `{}` keeps one residual risk, which is why the shipped `claude` hint carries none. The variable is
read from the pane's whole process subtree, so the value can come from a child — a subagent, a hook —
rather than from the tool the pane is running. As a detector that does not matter; substituted into
the command, it records whatever answered first.

The variable is read from the pane's pid and every process under it, breadth first, first one that
has it wins: `/proc/<pid>/environ` on Linux, `sysctl(KERN_PROCARGS2)` on macOS — the same call
`ps -E` makes, which the kernel serves for a process of the same uid — and nothing at all on any
other platform. One process-table read serves the whole pass.

Two later changes narrow the rules above. For `claude`, `codex`, `opencode` and `pi`, a hint now
REPLACES the session the argv picked rather than appending beside it, so the command picks exactly
one. And for `claude`, the id no longer comes from the variable when Claude Code's own session
record is there. Both are in [A restored session offers to resume its agents, and only its
agents](#a-restored-session-offers-to-resume-its-agents-and-only-its-agents).

From nkmk.31, every known agent is pinned to its own session the same way, not only `claude`, and
an agent with no session found is never recorded with `--continue` or `resume --last`. See the
nkmk.31 paragraphs in that entry. A configured hint still appends what it says; the shipped
`claude` hint's `--continue` is replaced by the agent's own id whenever one is found, and by the
picker when none is.

Best-effort throughout: no hint, no variable, or no platform each record the command unchanged, at
debug log level. Nothing here can fail a serialization.

Config-file only and unset by default, so a config without the block behaves exactly as before. It
reaches the pty thread through the existing `Reconfigure` message, so **a config save applies to the
next serialization** without restarting the session.

Note the KDL constraint that bites here: every node needs a `;` or a newline after it, the last one
before a closing brace included, so the one-line form `{ match "x"; env "Y"; resume_args "z" }` does
not parse. Use the multi-line form above.

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

### A restored session offers to resume its agents, and only its agents

```kdl
resurrect_commands "prefill"   // or "agents", or "all" (the default)
```

From nkmk.28. Three reports after `zellij session restart`, with the shipped `claude` hint in place:

1. A claude pane offered `claude --resume <id>` for the FIRST session that pane ever ran, not the
   one live at the restart.
2. Panes offered commands that are not agents at all: the starship prompt, linuxbrew, other
   short-lived processes.
3. A pane offered `claude --dangerously-skip-permissions --continue --resume <id>`: two session
   choices on one line.

**Why the claude id was the first one.** The claude process does not carry
`CLAUDE_CODE_SESSION_ID` in its own environment. Its children do, each with the value from the
moment it was spawned, and an environment never changes after `exec`. The hint walks the pane's
processes breadth first in pid order, so it answers from the oldest child: an MCP server claude
started with, still naming the session from before every `/clear`. Verified on a live machine: a
claude whose own record named its current session, with every MCP child still naming the first.
A pane restored once made it worse: its argv carried the old `--resume <id>`, the hint's "already
resumes" guard saw `--resume` and added nothing, and the old id survived every later restart.

**The fix reads Claude Code's own record.** Claude Code keeps `sessions/<pid>.json` under its
config directory (`$CLAUDE_CONFIG_DIR`, else `~/.claude`), and rewrites `sessionId` in it whenever
the process moves to another session. The serialization pass takes the first process under the
pane that has a record whose `pid` is its own, and records `--resume <sessionId>` in place of any
session the argv picked. It runs AFTER the configured hints, so the live id replaces one a hint
read from a stale environment, and a pane with no record keeps what the hint recorded. It runs
when `resurrect_commands` is `"agents"` or `"prefill"`, or when a configured hint matches
`claude`, so the shipped hint is fixed with no config change. (From nkmk.34 it runs in every
mode: see "in every mode" below.)

**Exactly one session choice.** For a harness this build knows, a recorded command keeps every
flag except the words that pick a session, and then gets exactly one. (From nkmk.32 it keeps no
flag at all: see "only the bare resume" below.) A configured hint for one of
these REPLACES the session the argv picked instead of appending beside it; a hint for any other
command keeps the old append-only rule, because this build does not know that tool's words. The
words, each from the CLI's own `--help`:

| agent | session words removed | newest-session resume added | source |
|---|---|---|---|
| `claude` | `-c`/`--continue`, `-r`/`--resume [id]`, `--session-id <id>`, `--fork-session`, `--from-pr [n]` | `--continue` | `claude --help` |
| `codex` | `resume [id]`, `fork [id]`, `--last`, `--all` | `resume --last`, straight after argv0 | `codex resume --help` |
| `opencode` | `-c`/`--continue`, `-s`/`--session <id>`, `--fork` | `--continue` | opencode's CLI reference; not installed where this was built |
| `pi` | `-c`/`--continue`, `-r`/`--resume`, `--session <id>`, `--session-id <id>`, `--fork <id>`, `--no-session` | `--continue` | `pi --help` |

A value that may be left out (`--resume`) is taken only when the next word is not a flag; a
`--flag=value` form is removed whole. The newest-session resume is used only when nothing else
named a session, and it resumes the newest session in the pane's directory, where the restored
pane starts. `codex resume` takes the same options as `codex`, so the observed flags still applied
until nkmk.32 dropped them.

**From nkmk.31, each pane resumes its own session, and never the newest.** The newest-session
resume was wrong whenever two agent panes shared a directory: both came back holding whichever
session was newest. Operator report of 2026-10-06. Every known agent is now pinned to the session
its own process holds, in that agent's own form, and the fallback is the agent's picker.

The sources, in order of preference: the agent's own report on its pane (`osc`, below), the
session file its process holds open (`open-file`), claude's per-process record (`record`), an id
already in the argv (`argv`, recorded as the bare resume of that id from nkmk.32), and last the
picker (`picker-fallback`).

| agent | own session found from | recorded with an id | recorded with no id |
|---|---|---|---|
| `claude` | `sessions/<pid>.json` (`record`) | `--resume <id>` | `--resume`, which opens the picker |
| `codex` | the `rollout-*-<uuid>.jsonl` it holds open (`open-file`) | `resume <id>`, after argv0 | `resume`, which opens the picker |
| `opencode` | nothing | `--session <id>` | the bare command: it has no picker flag |
| `pi` | nothing | `--session <id>` | `--resume`, which opens the picker |

The pid rule: walk breadth first from the pane's own child and take the NEAREST process that
answers. A shell pane's agent is the shell's child, a command pane's agent is the pane's own
process, an npm wrapper puts the native `codex` one level down, and a subagent sits deeper.

- **claude**: the server's own config directory first. Then each process under the pane is asked
  for its own `CLAUDE_CONFIG_DIR` or `HOME`, because a launcher-started server never sees what a
  pane's rc file exports. On Linux a record whose `procStart` is not the live process's start time
  (field 22 of `/proc/<pid>/stat`) is a crashed claude's record on a reused pid, and is ignored.
- **codex**: the open files of each process under the pane. Linux reads `/proc/<pid>/fd`; macOS asks
  libproc (`PROC_PIDLISTFDS`, then `PROC_PIDFDVNODEPATHINFO` per vnode), the calls `lsof` is built
  on, in-process: no subprocess, no half-second `lsof` start-up, no privacy prompt for a process of
  the same user. codex creates the file on the first message, so a codex nobody has talked to yet
  has no session to resume and gets the picker.
- **opencode, pi**: no per-process handle exists. opencode keeps every session in one shared
  sqlite database; pi appends with a call that closes the file at once, and exports
  `PI_SESSION_ID` only to its bash-tool children while a tool runs. Both get their picker unless
  an identity variable or a configured hint names the session.

**Never the newest session, without exceptions.** With no id found, an argv that carries a
newest-session flag - `claude --continue`/`-c`, `codex resume --last`, `opencode --continue`/`-c`,
`pi --continue`/`-c` - is recorded with the agent's picker instead; it is not kept as run. Only an
id the argv names survives, as the bare resume of that id. The same holds for a configured hint whose
`resume_args` add `--continue`: with no id found, the pane still ends at the picker. The bare
`--resume` and `--continue` of the report were the operator's own: shell abbreviations run
`claude --resume` and `claude --continue`, and nkmk.27 recorded them verbatim.

**From nkmk.32, a restored agent pane holds only the bare resume.** Operator report of
2026-10-11: a pane launched as `claude --model opus --dangerously-skip-permissions '<launch
prompt>'` came back as `claude --model opus --dangerously-skip-permissions '<launch prompt>'
--resume <id>`. Only the session words were stripped, so the positional prompt survived, and Enter
would have resumed the session AND sent the prompt again as a new turn. A known harness is now
recorded as argv0, path and all, plus its session words and nothing else:

| what named the session | recorded |
|---|---|
| the live session (`osc`, `record`, `open-file`, `env`) | `claude --resume <id>`, `codex resume <id>`, `opencode --session <id>`, `pi --session <id>` |
| an id in the argv, nothing live | the same form, with the argv's id |
| nothing | the picker: `claude --resume`, `codex resume`, `pi --resume`, bare `opencode` |
| a configured hint | argv0 and the hint's words |

Launch flags (`--model`, `--dangerously-skip-permissions`) are dropped with the prompt. The
operator chose that over a per-harness table of which flags take a value, which is what telling a
flag's value from a prompt would need. A hint for a command this build does not know still only
appends, because this build does not know that tool's words. `"prefill"` also types the bare
resume of whatever a snapshot holds, so a snapshot written before nkmk.32 does not type its old
prompt either.

**From nkmk.34, in every mode.** The pass above ran only with `"agents"` or `"prefill"`, or a
configured hint for the harness. Under the default `"all"` with no hint, a known agent was recorded
with its whole argv, launch prompt included, so a restore offered to send the prompt again. Found
on an agent started with a prompt: `session-layout.kdl` held
`args "--model" "haiku" "--" "<the prompt>"`. The operator's rule is that a restored agent pane
holds only the bare resume, in any mode, so the live-session pass and the bare resume now run for
every known harness whatever `resurrect_commands` says. `"all"` still keeps every other command
whole: only claude, codex, opencode and pi change. A known agent with no session found gets its
picker in `"all"` too, never its newest session, so a `claude --continue` command pane is recorded
as `claude --resume`.

What `"all"` does not do: rewrite a snapshot written before nkmk.34 at restore. A pane restored
from one offers the command that snapshot holds; the next serialization records the bare form.
Only `"prefill"` types the bare form of an old snapshot.

In the same patch, the `list-panes` table folds a newline, carriage return or tab in any cell to a
space, as the `list-agents` table does (`dashed` in `zellij-utils/src/agent_detect.rs`), so an agent started with a multi-line prompt keeps its row on one
line. `--json` keeps every value as it is.

Files: `zellij-server/src/pty.rs` (`apply_resurrect_command_hints`),
`zellij-server/src/session_layout_metadata.rs` (`runs_a_known_agent`, which keeps the process-table
read off a session with no agent), `zellij-server/src/route.rs` (the table row),
`zellij-utils/src/resurrect_command_hints.rs` (the mode's doc). Tests: the flightdeck pane under
`"all"` with a reported session and with none, an agent started with a prompt, a non-agent pane
keeping its argv, a `--continue` command pane, and a multi-line command on one table row.

**The agent's own report: `OSC 1337 SetUserVar=agent_session=<base64 id>`.** The iTerm2 and
WezTerm user-variable convention, printed by a hook in the agent to its pane's terminal. The grid
had no `OSC 1337` arm before; it now reads this one user variable and ignores every other. The id
must be a plain token - letters, digits, `-_.:`, at most 128 - because it becomes a word of a
recorded command line; an empty value clears the report. The pty thread keeps it on that pane and
drops it when the pane closes or is respawned, and when the pane has been seen running an agent
and then is back at its shell. Not "when the pane is not running an agent right now": the hook
fires at start-up, usually before the once-a-second tick has seen the agent, and that rule would
drop every fresh report. A new report from the next agent replaces the old one.

`zellij setup --agent-hooks` prints the hook for each agent and the file it goes in. It edits no
harness settings file. From nkmk.36, `--write` also writes the shared script; see
[The binary keeps agent-session.sh current](#the-binary-keeps-agent-sessionsh-current). A build that reads the report lists `agent-session-osc` in
`zellij setup --check --json`, so dotfiles can install the hooks only where they do something.

**The hooks follow the shared agent-session contract.** On every session start - a claude
`/clear` or a pi `/new` starts a new id in the same process - each hook does three things, and
only the last is limited to zellij:

1. exports `AGENT_SESSION_ID` (the harness's own native id) and `AGENT_KIND` to the commands the
   agent runs, where the harness can;
2. runs `agent-sessions register --id <id> --kind <kind>` when that command exists, and
   `agent-sessions unregister --id <id>` from an end hook where the harness has one. The registry
   CLI and its file format belong to another tool on these machines, not to zellij;
3. inside zellij only (`ZELLIJ` set), prints the `OSC 1337` report.

| agent | file | export | register / unregister | report |
|---|---|---|---|---|
| `claude` | `settings.json` `SessionStart` and `SessionEnd`, calling `~/.config/zellij/agent-session.sh claude start` and `claude end` | not here: bm's hook | `SessionStart` / `SessionEnd` (from nkmk.36) | yes |
| `codex` | `config.toml` `[[hooks.SessionStart]]` and `[[hooks.SessionEnd]]`, the same script | none: codex 0.160 has no way | `SessionStart` / `SessionEnd` | yes |
| `opencode` (v1 and v2) | `~/.config/opencode/plugins/agent-session-env.ts` (server) and `~/.config/opencode/plugins/agent-session/tui.ts`, a TUI plugin directory; v1 also names it in `tui.json` | the server plugin | session opened in the TUI / TUI exit | yes |
| `pi` | `~/.pi/agent/extensions/agent-session.ts` | its own `process.env`, which every command it runs inherits | `session_start` / `session_shutdown` | yes |

From nkmk.33 the codex hook also reports codex's state over `OSC 7501`, from four more hook events;
see [What an agent says it is doing](#what-an-agent-says-it-is-doing-osc-7501-and-state-on-list-agents).
From nkmk.34 the opencode TUI plugin does the same for opencode, from opencode's own events; see
[opencode reports its state from its TUI plugin](#opencode-reports-its-state-from-its-tui-plugin).

claude's export is owned by bm's SessionStart hook, and the zellij snippet does not duplicate it.
Up to nkmk.35 that hook also owned register and unregister, and the zellij snippet ran
`claude osc`, because a second `agent-sessions register` replaced the whole entry. From nkmk.36
`register` merges into an existing entry, so the zellij snippet registers with `claude start` and
unregisters with `claude end`, and bm's hook may register the same id again to add `bm_project`.

On the reading side, `AGENT_SESSION_ID` is the first identity variable for every harness, before
its native names. When `AGENT_KIND` names another harness than the one detected - a codex pane
whose process tree carries a nested claude's variables - the shared id is ignored for that pane.
The pty files its own live id with `AGENT_KIND` set to the detected harness, so a nested agent's
variable never disowns it.

**A claude hook has no controlling terminal.** Verified with Claude Code 2.1 under a pty: inside
the hook `tty` says "not a tty" and `ps -o tty=` prints `?`, so a write to `/dev/tty` fails, and
the hook's stdout is claude's, not the terminal. The shared script walks up to the agent itself -
the nearest ancestor whose executable or argv0 is named like the agent - and writes the report
to that agent's terminal. It stops there: a first version took the first ancestor with any
terminal, and a `codex exec` with none of its own reported into the unrelated pane that started
it. opencode and pi run the snippet inside the agent's own process, which owns the terminal, so
they write to `/dev/tty`.

**opencode v2 keeps its sessions in a shared background service**, not in the pane: the TUI in
the pane is a client, and one service serves every pane (`--standalone` gives a pane a private
server, as a child of its TUI). So the hook is a CLI plugin, which runs in the TUI, reads the
session the TUI shows (`context.ui.router.current()`), and reports and registers from the pane's
own process. A plugin in `plugins/` runs in the service instead, which has no pane and no single
session, so the env export lives there. Its shell hook (`create.before`) is not told the session,
and opencode sets its own `OPENCODE_SESSION_ID` only after that hook, so the export pairs two
hooks: the tool hook (`execute.before`), which does carry the session, remembers it per command,
and the shell hook sets `AGENT_SESSION_ID` and `AGENT_KIND=opencode` for that command. The pair
matters: opencode's shell passes on a parent claude's `CLAUDE_CODE_SESSION_ID` when nested, and
without its own pair a nested opencode reads as its parent. `list-agents` detects the TUI by its
argv0, `opencode`, and never takes a `CLAUDE_*` name for it.

**One file per surface serves opencode v1 and v2.** The two versions load a plugin differently,
but each reads only its own key from the module's default export and ignores the rest: v2 wants
`id` and `setup` (the server checks a schema that ignores extra keys, the TUI checks for the two
keys), and v1 wants `server` or `tui`. So each snippet is one object with both keys, `setup` for
v2 and `server` or `tui` for v1, and imports nothing from opencode, because neither version can
resolve the other's plugin package. The v1 server sets the pair in `shell.env`, which runs for
every command and is told its session. The v1 TUI reads `api.route.current`, a property, where v2
calls `context.ui.router.current()`, and it unregisters from `api.lifecycle.onDispose`.

The TUI package lives in `plugins/agent-session/`, not beside the config. v2's TUI loads every
directory in `plugins/` by itself, and v2's server skips a directory with no server entry, so v2
needs no config line at all. v1's server loads only `plugins/*.{ts,js}`, so it skips the
directory, and v1's TUI loads only what `tui.json` names, so v1 needs `"./plugins/agent-session"`
in its `plugin` list. The nkmk.31 snippet put the package in `~/.config/opencode/agent-session/`
and named it in `cli.json`. That file is v2's own: v2 writes it once, migrated from `tui.json`,
and never again reads `tui.json` after that. So a machine that runs both versions over one config
directory had to keep two plugin lists in step. The move removes the `cli.json` line.

Proven on Linux, in scratch config, state and data directories: opencode 2.0.25 and 1.19.0, the
last v1 release. Each `opencode run` bash tool printed `AGENT_SESSION_ID` equal to its session
and `AGENT_KIND=opencode`. Each TUI, opened on that session in a scratch zellij session,
registered its own process, reported over `OSC 1337` (`session_source` `osc`), and unregistered on
exit. v2 found the TUI package with no config line.

Proven on Linux, in scratch config directories: claude (the report reaches the pty with the
hook's `session_id`); pi 1.1 (`session_start` fires with no model call, `register` resolves the
pi process itself, `/quit` unregisters); `codex exec` 0.160 (`SessionStart` and `SessionEnd`
fire even with no auth, `register` resolves the codex process, no report without a terminal);
opencode 2.0.25 (`--standalone --session <id>` on a session made without a model call: the
report carries the id, `register` resolves the TUI process, quitting unregisters; and in
`opencode run --standalone` its bash tool printed `AGENT=<id> KIND=opencode` with the id equal
to its own `OPENCODE_SESSION_ID`). Unproven: the
interactive codex TUI, which stopped at sign-in and never started a session.

A miss is logged at `warn` once per pane, when its reason changes, with the pid, the agent and the
reason, so the server log answers "why did this pane come back with a picker". A pane held open
after its agent exited is recorded with the id its agent last held, not its picker: see [A held
pane forgets its dead pid](#a-held-pane-forgets-its-dead-pid). The serialized
layout carries the id in the pane's arguments and no new key: the layout parser rejects a pane
property it does not know, so a key here would make a nkmk.31 snapshot unrestorable on a rollback.

**Why non-agents came back.** On a pane whose foreground process group is the shell's own,
discovery records the shell's NEWEST CHILD ([the job-control fallback
above](#what-a-pane-is-running-when-the-shell-has-no-job-control)). A prompt is drawn by a command
substitution, which runs in the shell's own group, so a tick that lands while a prompt redraws -
on a resize, on the detach a restart starts with - records `starship prompt`. A shell init step
such as `brew shellenv` is caught the same way. And upstream records every long-lived foreground
command anyway: `htop`, `tail -f`, an editor.

**`resurrect_commands` picks what a restored session offers.**

| value | serialized | restored |
|---|---|---|
| `"all"`, or unset | every command, as upstream | a command pane, `Waiting to run` |
| `"agents"` | agents only, with their resume | a command pane, `Waiting to run` |
| `"prefill"` | agents only, with their resume | a plain shell with the resume typed at its prompt, not run |

An agent is a harness `list-agents` already knows - `claude`, `codex`, `opencode`, `pi`, by
basename - or a command a `resurrect_command_hints` entry names. In `"agents"` and `"prefill"`
every other terminal pane is recorded with no command, editors included, so it comes back as a
shell in its directory.

Upstream v0.46.0 adds `zellij action set-resurrect-command`, which sets one pane's serialized
command. An explicit override from it wins over the hints and over this mode. The pty thread's
field for the mode is `resurrect_mode`, beside upstream's `resurrect_commands` map of overrides.

**How `"prefill"` types the command.** Only a pane the session layout brought back is touched: its
`RunCommand::resurrected` mark (see [the entry on clean
exits](#a-resurrected-command-pane-drops-to-the-shell-when-its-command-exits-cleanly)) is the
test. The pty thread opens the default shell in the command's directory instead of a held pane, and
hands the quoted command line to the screen right after the layout, before any byte of that pane
is read. The screen writes it to the pane as one bracketed paste - `ESC[200~`, the line,
`ESC[201~`, no newline - the first time the pane's terminal has turned bracketed paste on. A paste
and not keystrokes, because a line editor binds widgets to plain keys: an abbreviation expander on
space would rewrite a typed line, and inside a paste ZLE and readline insert it literally. Control
characters are dropped from the line, so it can neither end its paste early nor run itself. A line editor - zsh's ZLE, bash's readline, fish - turns it on when it
starts reading a line, so that is the moment the shell sits at a prompt. Bytes written earlier
would sit in a cooked-mode tty: the kernel echoes them above the prompt, and a shell that flushes
input at start drops them. A shell that never turns bracketed paste on gets nothing typed and is
still a working shell.

No pane is held, so none of this goes near the held-pane path, and Enter runs the line the way any
typed command runs. The screen builds the pane from a layout that names the shell, not the
command, so the pane is not titled or listed as running something it is not. A non-agent command
in an older snapshot comes back as a shell with nothing typed. A pane whose line is never run is
recorded as a shell at the next serialization, the same cost the ESC on a held pane paid. A
layout override (`override-layout`) reaches the screen batched with other work, so it types
nothing.

**`list-panes --json` names the line while it waits.** `restored_from` marks every restored pane,
so a consumer could not tell a prefilled shell from any other. `prefilled_command` can: it holds
the line the screen pasted, from the moment the paste is written - not while it waits for the line
editor - until anything touches it. Every route that writes user bytes to a terminal pane ends in
one place, the pane's input adjustment: a key, a paste, `action write` and `write-chars`, MCP
`write_input`, a plugin's write. The first non-empty input there clears it, and so does the
shell's `OSC 133;C`, the line having run. A prompt redraw (`133;A`) does not, because a redraw is
not the line being used. The field is on the CLI-only `PaneListEntry`, so `list-tree --json`
carries it too, with no protobuf tag. A consumer reads it as "Enter resumes this pane".

**A top-level key, not one inside `resurrect_command_hints`.** Every child of that block is parsed
as a hint, and a hint with no children fails the whole config on every build. A top-level key is
ignored by a build that does not know it; nkmk.25 reports a config carrying one as well defined.
A value this build does not know, or one that is not a string, is warned about and ignored, which
means `"all"`. So the key can go into a shared config before every machine has the binary.

### A command pane is recorded as its own command, not its newest child

From nkmk.29. A pane can run an agent directly: a layout's `command "claude"`, or a restored pane after Enter.
Then the pane's own process IS the agent. It leads its own process group, so the terminal names the
pane's pid as the foreground group, and discovery treated it like an idle shell: it recorded the
newest child ([the job-control fallback](#what-a-pane-is-running-when-the-shell-has-no-job-control)).
For an agent that is an MCP server, a tool shell or a `sleep`. In `"agents"` and `"prefill"` mode
the filter then dropped the pane as a non-agent; in `"all"` mode it came back running the child.

The screen already knows which panes run their own command: a command pane or an editor pane was
opened with `Run::Command` or `Run::EditFile`. Serialization now asks about those panes separately.
When the terminal names another process group, that group is recorded as before. When it names
the pane's own process, the pane records that process's own argv. Shell panes are asked exactly as
before, so the job-control fallback is unchanged for them.

| pane | recorded before | recorded now |
|---|---|---|
| `command "claude"` with MCP children | the newest child | `claude ...`, then the agent resume |
| an editor pane with a language server | the language server | the editor and its file |
| a shell, idle or running a job | unchanged | unchanged |

This changes `"all"` snapshots too: a command pane with children comes back running its command.
From nkmk.30, the live pane list (`list-panes`, `list-agents`) asks the same way too, so such a
pane is listed as its own command.

### Tabs come back in the order they were left in

A session restored from a snapshot got its tabs back in **creation** order, so every tab that had
ever been moved jumped back to where it started — once per restart, quietly, with the serialized
layout agreeing with the wrong order.

`screen.tabs` is keyed by stable tab id; the display order lives in `tab.position`. The two agree
until a tab is moved. `get_layout_metadata` iterated the map, so what it wrote was id order — and a
layout recreates tabs in the order it lists them. `query-tab-names` read the same map the same way,
which is worse than it sounds: it is the command you would reach for to check the order, and it
confirmed the wrong one. Both now sort by position.

### A layout's pane count is the panes it produces

A session serializes itself to `session-layout.kdl` every minute so a dead server can be
resurrected, and `is_dirty` decides whether that tick has anything to write: it compares the panes
the session has against the panes its layout describes.

`Layout::pane_count` answered that second question wrong. A layout carries tabs AND a template,
and the template is what a tab is expanded FROM - the parser fills it in even for a layout that
declares its own tabs. Counting both added the template's panes on top of the tabs they had
already built, so a real layout file reported more panes than any session it could produce, and
every session grown from one was dirty from the moment it started. One layout in daily use claimed
40 panes for the 36-pane session it built.

The count now branches the way the spawn loop branches: tabs if the layout has tabs, the template
if it has none. Swap layouts are alternative arrangements of panes that already exist, so they
still add nothing.

Two things follow from a session finally being able to be clean.

**A clean session stops rewriting the cache.** That is the point. It also makes the rest of
`is_dirty` reachable: the checks after the pane count - the commands panes are running, and whether
the tab list still matches the layout - never ran for a layout with tabs, because the count
returned first every time.

**A session that returns to its base shape rewrites it once more.** Opening a pane and closing it
again leaves the session clean and the file on disk diverged, and nothing would ever overwrite it -
the next resurrection would hand back the pane that was closed. The pty thread keeps one bool for
this: a tick writes if the session is dirty OR if the tick before it was. The same bool starts
`true`, so a session that never diverges from its layout still writes its base shape once instead
of never being resurrectable.

### What a clean session still has to write

Being able to be clean brought a new way to be wrong. `is_dirty` asks whether the session has
diverged from the layout that built it; the cache holds a copy of the SESSION, not of the layout.
A session can be clean and still have changed. Rename a pane and the pane count is the same, the
tabs are the same and the commands are the same - so the session is dirty by nothing, writes
nothing, and the copy on disk keeps the old name for as long as the session stays clean. Every
serialized attribute the dirty checks do not look at has that shape: cwd, geometry, pinning,
borderlessness, focus, uuid, handle, colours, viewport contents. Before the pane count was fixed
this could not happen, because a layout with tabs was dirty from birth and rewrote every tick.

So a tick now also writes when the layout it WOULD write differs from the one it last wrote,
decided by a fingerprint: a hash of every field that reaches `GlobalLayoutManifest`, taken from the
metadata the tick has already gathered. Nothing is serialized to compute it. Both metadata structs
are destructured field by field, so a field added later fails to compile until someone says which
side of the fingerprint it belongs on. Two are deliberately outside it - the base layout, which
`Screen` hands to every tick unchanged, and the editor, which is not serialized and whose only
effect is already in a pane's recorded command.

The geometry is hashed through its `Debug` rendering rather than its `Hash`: `PaneGeom` compares
and hashes without `is_pinned` on purpose, and the serialized layout writes `pinned`.

A clean, unchanging session is still silent - its fingerprint is the one already on disk - so the
saving the pane-count fix was made for is intact. Note also that the disk write was already
deduplicated by content, so what any of this saves is CPU, never IO.

A pane's NOTE is not covered, because a note is not serialized at all. That is a gap in what the
snapshot holds, not staleness in what it holds.

### A tab bar in a tab you are not looking at

Moving, renaming, adding or closing a tab left every *other* tab's tab-bar plugin drawing the old
list. Arriving at one of those tabs drew the stale list for a frame (~10ms, the repaint debounce)
before it corrected itself — visible as a flicker, since the tab names shift.

Upstream sends `TabUpdate` to the active tab's plugins only (`12ee60753`, #4918). That is correct
for the frequent case: the event fires on nearly every state change, and updating every tab's
plugins each time was a measured regression when switching tabs. It is only wrong for the rare one.
The update now reaches every tab's plugins when the tab list actually changed — decided by
comparing `(id, position, name)` per tab against what was last reported — and the active tab's
alone otherwise. A switch changes none of those, so switching stays exactly as
cheap as upstream made it. Measured on a 5-tab session: the frame drawn on arrival is correct, and
takes the same ~20ms to appear as before.

The active-tab **highlight** is still one frame behind on arrival, and is left that way. The plugin
only learns it is active after the switch, and making the frame wait for that redraw does not work:
a switch also fires `ModeUpdate` and `PaneUpdate`, the plugin redraws for those first, and that
redraw satisfies the wait before the tab-aware one exists. Fixing it needs a render to say which
event it answers, which `PluginRenderAsset` does not carry.

### One warning badge in the tab bar

Two facts are true of a whole session, actionable, and invisible everywhere else. The bar says so
in the space beside the clock:

```
  mysession  1 edit  2 build  3 logs                              ⚠ zj TCC  7/30 3:47 PM
```

`zj` is a superseded build, `TCC` is missing Full Disk Access, and one triangle covers however many
are live — two triangles side by side read as two widgets rather than one thing that is wrong. The
codes are space-separated in a fixed order, so a badge showing both never swaps them between
frames. **No warnings renders nothing at all**, not a space, so a healthy session's bar is
byte-identical to what it was.

It sits **left of the clock** rather than at the outer edge, so the clock keeps its column when a
warning appears or clears: the clock is what gets read at a glance, and a badge that shoved it
sideways twice a session would be worse than the badge is good. It is drawn in `exit_code_error`
from the active theme — the colour the theme itself picked for "something is wrong" — bold, and it
never blinks: it reports a fact that has been true for minutes and will stay true until someone acts
on it. In the bar's overflow order the badge is in every form, so it outlives the clock; a bar too
narrow to say something is wrong is exactly the bar that would hide it forever.

**This replaced a pair of full-width lines the server composited over the top-right of the
viewport.** That placement bought real things — an alt-screen repaint could not clobber it, it cost
no pane rows, and `dump-screen` never saw it — but the price was a sentence and a half of prose
across the top of the panes for a fact that does not change for hours. What is left of the old
design is the part that mattered: the **server** still answers both questions, because the answers
need `current_exe`, the `PATH` and a real `open` against a TCC-gated file, none of which a wasm
plugin can reach without spawning a process. Asking once in the server costs one probe per session
per tick however many bars draw it.

The answers ride **`ModeInfo.session_warnings`**, which is the smallest honest transport available:
it is the one thing every bar already reads, it already reaches every tab's plugins, and this fork
already extends it for `pane_frame_style`, `session_dimmed` and `session_ancestry`. No new event
type, no new subscription. An unknown code arriving over protobuf is dropped rather than fatal, so a
bar built against an older contract badges what it understands.

Both questions are re-asked every 30 seconds, because both answers change under a running server —
an FDA toggle takes effect immediately, and an upgrade can replace the binary at any time. A change
sends one `ModeUpdate`; no answer moving sends nothing.

The trade is stated plainly: a user who has replaced **both** bundled bars sees neither code. That
is the cost of the badge, and it is accepted — `zellij session doctor` reports both conditions in
full prose, and it is the tool you reach for when something is actually wrong.

**Full Disk Access** (`expect_full_disk_access true`, macOS, off by default) opens the same
FDA-gated file the startup probe uses last and reports a refusal — a real `open`, because `access(2)`
tests permission bits while TCC denies at open. Opt-in, because only the user knows whether they
mean zellij to hold that permission - and where they do, its absence IS the actionable fact whether
or not it was ever granted. A probe that cannot answer — the file missing, or a failure that is not
a permission one — is never reported as a denial. The badge is a flag, not an instruction:
`session doctor` names the exact path, because the grant is keyed to that file and
auto-registration was not observed to happen.
Non-macOS builds have no such permission, so `TCC` cannot appear.

**A superseded build** (`stale_build_notice`, on by default) is asked of the path this server was
STARTED FROM: the file being gone, or holding a different build than the one running, is proof.
Comparing against whatever `zellij` is on `PATH` would call a deliberately-mixed setup stale
forever. Two platform details make this precise rather than lucky: a package manager's upgrade
deletes the old versioned directory, and Linux reports a deleted binary's path with a ` (deleted)`
suffix, so the file "not existing" is exactly the case that matters. A binary that is merely
RENAMED is followed, and correctly says nothing.

The one addition is for `pin_exe`: an upgrade reaches the pinned copy only when something runs
`session up`, so until it does the pinned path still holds the build the server is running and the
rule above stays silent. There — and only there — the binary on `PATH` is the intended source of
that copy, so it is what gets compared. Once the refresh runs it renames over the pinned path, which
unlinks the file the server started from, and the ` (deleted)` rule answers on its own.

**The server knows the build it started as (nkmk.25, 2026-09-29).** The paragraphs above held on
Linux only by accident, and never on macOS. With `pin_exe` on, the launcher runs the pin and the
unit puts the pin directory first on the server's `PATH`. An upgrade renames a new build over the pin
under the live server:

| Platform | What the server saw | Badge |
|---|---|---|
| Linux | `current_exe()` is `<pin> (deleted)` | lit, through the suffix alone |
| macOS | `current_exe()` is the pin, which exists again and holds the new build | never lit |

On macOS the server read its own identity off the new file and compared the new pin with itself.
The pin rule was broken on both platforms: it took the first `zellij` on `PATH`, and that was the
pin itself.

The server now records its identity once, at startup, before anything can write over the file: the
canonical path with any ` (deleted)` suffix removed, the identity read off it, its mtime, and the
session socket. Each 30-second tick asks four rules of that record, in order:

1. the start path no longer exists;
2. the file at the start path was written after the socket was bound, and no longer carries the
   mtime recorded at startup. Without a socket the snapshot's own time is the reference;
3. the file at the start path is a different build from the recorded one;
4. the start path is the pin, and the first `zellij` on `PATH` that is not the pin and not in the
   pin's directory is a different build from the recorded one.

Rule 2 is the one that lights the badge on macOS. It costs one `stat` of the executable and one of the socket per tick, plus the read the rules
already made. `zellij-utils/src/session_lifecycle.rs` holds the record and the rules;
`zellij-server` records it in `start_server` and reads it in the warnings. Six unit
tests: a rewrite after the socket, an untouched file, a removed file, a newer build past the pin, a
`PATH` that leads only to the pin, and the badge lit through the warnings.

### The keybind bar says whether Full Disk Access is still there

The vitals cluster `slim-keybinds` draws gets one more segment, at the right-hand end:

```
 Ctrl +  LOCK  PANE  TAB  RESIZE        cpu 12% | ram 7.0/23G | disk 412/931G | fda no
```

A Mac that loses Full Disk Access loses it quietly. Nothing announces the revocation, and the first
sign of it is whatever needed the permission failing, hours later. [The tab bar's warning
badge](#one-warning-badge-in-the-tab-bar) answers the same question from the server, but only where
`expect_full_disk_access` is set and only in the *other* bar. This puts the fact where the eye
already is on a bar whose cluster is switched on.

**No new spawn, and no new permission.** The reading rides the one `/bin/sh` the cluster already
runs every ten seconds, as one more tagged line in the probe's Darwin branch. The cluster costs
exactly what it cost before, and `RunCommands` is the permission it already held. Vitals are off by
default (`show_vitals`), so a bar that never asked for the cluster is unaffected.

**The question is a real `open`.** `ls` against `/Library/Application Support/com.apple.TCC` —
the same directory, and the same shell idiom, `zellij session doctor` runs in a pane. TCC refuses
at `open(2)` while `access(2)` reads only the permission bits, so `[ -r ]` answers "readable" on a
machine holding no grant at all. Nothing listed is kept.

**Three answers, not two.** `fda ok` when the open succeeded, `fda no` when it was refused, and
**nothing at all** when the question went unanswered: the gated directory is not there, or the host is
not macOS and has no such permission to be missing. That is the server's `full_disk_access_granted`
rule — an `Option<bool>`, where `None` is never a denial — carried into the bar. A Linux session's
cluster is therefore byte-identical to what it drew before.

`fda no` is painted in `exit_code_error` from the active theme, bold: the colour and the weight the
tab bar's badge uses, so one wrong thing looks the same in both bars. `fda ok` is a plain reading
like the rest of the cluster. The answer is re-asked on every tick rather than remembered, because
an FDA toggle takes effect on a live process — a session refused at startup can be granted while it
runs, and a cached answer would go on reporting a problem the user has already fixed.

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

The copy is brought up to date first, by the same `install_pinned_exe` `session up` uses — a temp
file renamed over the pinned path. When it cannot be updated, the current binary is used and the
reason is printed. The ordinary cause is a directory this user cannot write, and the fallback is not
politeness: a pinned copy of a different build is a server that would not speak to its client.

The path is decided once, in `start_client`, where the config is, and reaches `spawn_server` through
a `OnceLock` rather than through the `ClientOsApi` trait and its test fake.

### `default_floating_size` — a bigger default for floating panes

A floating pane that carries no coordinates of its own lands at half the viewport
(`half_size_middle_geom`, `floating_pane_grid.rs`). Half a viewport is not enough for the plugins
that open that way: the session manager truncates session names, the plugin manager truncates
paths. The information is there; the pane is too small to show it.

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

The initial fix says what the accident said, on purpose. A pane collapsed to its stack header has no content
area, so `set_pane_frames` takes its content rows to zero and leaves its pty alone. The pane keeps —
in its grid and in its terminal — the size it had while expanded, so re-expanding to the same
geometry writes the same size back and the shell hears nothing. A stack that genuinely changed size
while the pane was collapsed (a column resize, a moved stack) is picked up on the way out of the
header, as one legitimate resize.

The later "Collapsed stack members use the expanded terminal size" patch supersedes that deferred resize.
It updates every member immediately when stack dimensions change and sizes new panes before rendering.

**The grid half of that sentence was not true at first, and the gap corrupted panes.** Taking
the pty out of the layout pass was not enough, because the grid is resized by a different route:
every `set_content_offset` and every `set_geom` calls `TerminalPane::reflow_lines`, which resizes the
grid to whatever the current geometry and offset imply. A collapsed member was handed two offsets in
one pass — the upstream branch first, which reserves no title row for a member that is not flexible
and so implies one content row over a one-row geometry, and the fork's clamp second, which implies
zero. Only the second is a no-op, because `Grid::change_size` returns early on zero rows. The first
one really squashed the grid to a single row.

So the pane's program was told it still had N rows, kept drawing N rows, and drew them into a grid
one row tall. Every absolute cursor move past row 1 scrolled instead of overwriting, and each redraw
was shredded into the scrollback. Re-expanding grew the grid back and left it holding the tail of one
frame above the head of the next — a torn pane full of characters from two different redraws, which
scrolling up and back down appeared to repair because it pulled whole rows back out of the
scrollback. Reproduced against a real client at 200×50 on 0.45.0-nkmk.12: a collapsed member's grid
held one line while 45 sat in its scrollback, and re-expanding showed two frames at once.

`reflow_lines` is the single point every one of those routes passes through, so that is where the
invariant is stated: a collapsed stack member's grid is frozen, by the same
`is_collapsed_stack_member` predicate that frees its pty. Guarding the layout pass instead would only
have covered the offsets, and left `set_geom` — which reflows at the moment the geometry becomes one
row — to squash the grid on its own.

`PaneGeom::is_collapsed_stack_member` names the state — stacked, fixed, one row — and the branch sits
at the single point every layout pass goes through, so no caller has to remember it and every frame
style is covered by construction, `full` included: it now skips the resize outright instead of
sending a zero for the os layer to drop. Both stack renderings end up quiet, classic and stack list;
the list's row-swap is untouched.

Two tests in `tab_integration_tests.rs`, both driving a real stack through the mock pty writer: one
runs a series of focus moves in each of the four frame styles and holds that every member is given
exactly one size, never a one-row size; the other holds the other side, that a tab resized while a
member was collapsed reaches that member when it is focused.

One integration snapshot moved with the fix, and the direction is the point:
`disabling_stacked_pane_list_restores_the_classic_stack` recorded a collapsed member wearing a
`[ SCROLL: 0/1 ]` badge on its title row. That badge was the bug — the member's one line of content
had been pushed into its scrollback by the squash. A collapsed member now has nothing in scrollback
and no badge.

`collapsed_stack_member_keeps_its_grid`, in `terminal_pane_tests.rs`, holds the grid half. It writes
19 lines into a 20-row stack member, collapses it, and replays both of the offsets a layout pass
applies to a collapsed member, in order — the intermediate one included, since that is the one that
squashed the grid. It then asserts the grid's rows, columns and contents survive the collapse and the
re-expansion. Replaying only the clamped offset passes with or without the guard, and tests nothing.

### Collapsed stack members use the expanded terminal size

Classic stack collapse changes visible geometry only. Every terminal in a stack uses the expanded
member's content rows and columns, including frame offsets. The grid uses the same size as the pty.
Resizing a stack updates collapsed terminals immediately. Switching the expanded member keeps that
size, so focus changes do not require a terminal resize.

`list-panes` keeps visible geometry: a collapsed member reports one pane row. Its content rows remain
zero because only its title renders. `dump-screen --pane-id` returns the full terminal grid.
No CLI or plugin contract fields change.

`TiledPanes` synchronizes terminal sizes after computing visible frame offsets and after direct solver resizing.
`TerminalPane` stores the stack terminal size separately. The pty resize macro reads terminal dimensions.
Both stack insertion paths reapply frames before returning, so new terminals receive their size without
waiting for a client render. Previously, an unfocused insertion could leave its pty at zero rows and columns.

The stack unit test captures pty sizes before rendering. It checks both insertion paths with and without focus.
It also checks collapsed geometry, viewport and solver resizing, screen dumping, expansion, and fullscreen restoration.

### Two bars ship in the binary (`slim-tab-bar`, `slim-keybinds`)

`default-plugins/slim-tab-bar/` and `default-plugins/slim-keybinds/` are builtin plugins of this
fork, built by xtask, embedded in the binary and named in `BUILTIN_PLUGIN_NAMES`. A layout asks for
them the way it asks for any builtin:

```kdl
pane size=1 borderless=true {
    plugin location="zellij:slim-tab-bar"
}
```

They arrived as separate plugin repos loaded through `file:` paths, and
they are recreations of zellij's own `tab-bar` and `status-bar`/`compact-bar` — replacements for
builtins, in the same category as the things they replace. Sources moved in, not artifacts: bundled
has to mean rebuildable. The tab bar took the name `slim-tab-bar` on the way: inside the fork
there is no external repo left to namespace.

What that buys: no `.wasm` to track in dotfiles, no plugin directory to keep in step, a bar that
cannot go stale against the binary that loads it, a bar on a bare machine with no dotfiles, and one
rebase instead of three. It costs about 3.6MB of binary, most of it the IANA timezone database the
tab bar's clock carries.

**Neither bar calls `request_permission` any more, and that is the point.** A builtin's permission
checks short-circuit to `Granted` (`wasm_bridge.rs`, `zellij_exports.rs`), so a request only ever
raised a prompt — in a bar, a pane nobody can focus to answer it. Zellij's own bars have never had
one. The permission cache is no longer written for either bar.

### Breaking out a pane no tab owns is an error

`break_multiple_panes_*` drops a pane id it cannot find, so a request naming only stale ids used to
move nothing, report success, and — for a new tab — leave an empty tab behind. The screen's
`BreakPanesToNewTab`, `BreakPanesToTabWithId` and `BreakPanesToTabWithIndex` instructions now check
every pane first and fail naming the first miss, changing nothing. A missing tab fails the same
way. The plugin calls `break_panes_to_new_tab`, `break_panes_to_tab_with_id` and
`break_panes_to_tab_with_index` reach these instructions, so a plugin passing a stale pane id gets
an error instead of a partial move.

`zellij-server/src/screen.rs` (`panes_not_found` and the instruction handlers). Tests in
`zellij-server/src/unit/screen_tests.rs`.

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

#### Which binary each session is actually on

A server keeps the binary it started with, so a fleet drifts: upgrade every machine and the
sessions that were already up stay where they were. [A warning when the running session is a
different build](#a-warning-when-the-running-session-is-a-different-build) tells one person about
one session at the moment they touch it. It does not answer "which of these sessions is on which
build", which is the question a script asks.

`--json` entries for a **live** session now carry that:

* `server_exe` — the executable the server is running, read out of the OS by pid.
* `server_build_id` — hex of what the linker stamped in: the Mach-O `LC_UUID`, the GNU build-id.
  **This, not the path, is what names a build.** A pinned copy and the binary it was copied from are
  two files holding one program, and this is the only field where they agree.
* `server_build` — `same`, `different` or `unknown` against the binary running the command.
  `unknown` is an answer, not a failure: `compare_builds` reports it rather than guess.
* `server_exe_replaced` — present only when it is true: the file the server started from is no
  longer at its path. Every platform can say so since nkmk.23, which compares the file's mtime with
  the socket's, and a missing file counts since nkmk.25.
* `server_version` — the version the server is running: what its `zellij --version` prints after
  "zellij ", such as `0.45.1-nkmk.36`. Absent when it is not known.

All five are absent for a dead session, and for a live one with two servers for its name. The first
four are also absent for a live one whose executable the platform will not answer about. Each is
omitted rather than null, like the metadata fields beside them, so a consumer that sees `server_exe`
can trust it.

**Nothing here crosses a contract**, which is the whole design. The answer is read from the OS on
the side that runs `ls`, by the same `server_executable` the stale-build warning uses — no
`SessionInfo` field, no protobuf tag, no client/server message. The decision not to put a version on
the plugin API stands; this never asks the session anything.

**The version is never asked of the binary.** Spawning every server's executable to ask its version
turns a read of the socket directory into an execution of whatever those paths point at. It also
hangs while one of them sits on a mount that has gone away. `server_version` comes from one of two
places instead:

1. `server_build` is `same`. Then the version is the listing binary's own.
2. Otherwise, a stamp the server writes as it starts: `server-version` in its session_info folder,
   holding its pid and then its version. `ls` trusts the stamp only when that pid is the pid
   serving the session now, so a stamp an earlier server left under the same name never counts.

With neither, the field is absent. A wrong version is worse than none. The stamp is a file, not a
message, so it crosses no contract either. A server that exits cleanly removes its own stamp.
Snapshots neither hash nor copy it, since it names a process, not a session's shape. A server
older than this release writes no stamp, so its version is known only when it is the same build.

The build id stays the answer to "which build": two builds of one version have different build ids,
and a version string cannot tell them apart.

### `list-clients --json`

```
zellij action list-clients --json
```

The client list was a human table only, so a controller had to parse fixed-width columns to learn
who is attached and whose terminal is shrinking the grid. `--json` prints the `ClientInfo` array
plugins receive in `Event::ListClients` — `client_id`, `pane_id`, `running_command`,
`is_current_client`, and the fork's `terminal_size` (CLI only) and `tty` where the server knows
them. Nothing else goes to stdout, and no clients prints `[]`.

`pane_id` is the serde form of the enum, `{"Terminal": 3}`, because this is the plugin-facing struct
verbatim rather than a second shape to keep in step. `is_current_client` marks the client that asked;
`zellij action` is its own short-lived client focused on no pane, so a CLI query marks no row. The
flag rides on the existing `ListClientsAction` message (field 1) and adds nothing to the contract.

#### `rootshell_pane`: which rootshell tab a client is attached from

From nkmk.33. rootshell exports a stable UUID for each of its tabs over ssh as `LC_ROOTSHELL_PANE`.
sshd's default `AcceptEnv LC_*` passes it, so it is in the environment of the `zellij` client that
tab runs. A notification about something a client should see - an agent waiting on a person - can
only reach the right iPad tab if the session knows that UUID. It did not.

```
$ zellij action list-clients --json
[{"client_id": 1, "pane_id": {"Terminal": 3}, ..., "tty": "/dev/pts/4",
  "rootshell_pane": "00000000-0000-4000-8000-000000000001"}]
```

- **The client sends it in the map it already sends.** `host_terminal_env` is the allowlist of
  host-terminal variables the client puts in its `CliAssets` on every attach (`TERM`,
  `KITTY_WINDOW_ID` and the rest), carried by the contract's existing `host_terminal_env` map
  (tag 12). `LC_ROOTSHELL_PANE` joins the allowlist. No new message, no new tag, no contract change.
  An older server ignores the extra entry.
- **The row is a CLI-only shape.** `list-clients --json` now prints `ClientListEntry`: the
  `ClientInfo` keys, flattened, plus `rootshell_pane`. `ClientInfo` itself is unchanged, so plugins
  see nothing new and no protobuf tag is spent. The key is absent, not null, for a client that had
  no such variable.
- **The value is checked before it is repeated.** It is whatever the client's environment held,
  so it is trimmed, and dropped when it is empty, over 128 characters, or has a control character
  in it. A UUID passes.
- **It names the tab a client attached from, not the tab it is in now.** A client re-sends the map
  on every attach, so a reattach from another tab updates it. The value lives for as long as the
  client stays attached.

Capability: `client-rootshell-pane`.

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

### `subscribe --timestamps`

```
$ zellij subscribe --pane-id sunny-otter --timestamps
2026-08-14T18:03:12.345Z $ cargo test
2026-08-14T18:03:12.345Z test result: ok. 37 passed
$ zellij subscribe --pane-id sunny-otter --format json --timestamps
{"event":"pane_update","pane_id":"terminal_3",...,"ts":"2026-08-14T18:03:12.345Z"}
```

A stream of pane renders with no times in it cannot answer "how long did that take" or "did this
appear before or after that", and a watcher that has to stamp the lines itself has already lost the
moment they arrived. The flag puts the time on each line: a prefix and a space in raw mode, a `ts`
key in json.

- **The format is RFC3339, UTC, to the millisecond** — `2026-08-14T18:03:12.345Z`. It sorts as text
  and every log tool already reads it.
- **It is a print time, not a server time.** The clock is read by this client as the line leaves it,
  which is after the pane produced the output by however long the render and the socket took. The
  server does not stamp the update, so there is no time here that could be compared against another
  machine's.
- **One stamp per update.** Every line of one render carries the same time, because they are printed
  in one go; a stamp that crept forward between the lines of a frame would be describing something
  that did not happen.
- Without the flag the output is byte for byte what it was, and in json the `ts` key is simply
  absent — added, like every key in the fork, never renamed or removed.

### Which panes are running a coding agent (`list-agents`, and an `AGENT` column)

```
$ zellij action list-agents
TAB_ID  TAB_NAME  PANE_ID  HANDLE       KIND    AGENT_ID  SOURCE       TITLE    COMMAND  CWD
1       develop   3        sunny-otter  claude  9f3c1a2b  command+env  claude   claude   /home/u/src
2       review    7        brisk-heron  codex   -         command      codex    codex    /home/u/src
```

"Send that to the develop agent" needs a way to turn a harness into a pane, and until now there was
none: a caller had to read `list-panes`, guess which commands were agents, and read `/proc` itself
for the identity. This makes it a query. `list-panes` gains an `AGENT` column carrying the harness
name, `list-panes --json` gains an `agent` object on each entry, and `list-agents` is the same walk
filtered to the panes that have one.

**Detection is two-phase, and the split is why it is on by default.**

- *Phase one costs nothing.* A pane's command is recorded for every terminal pane already,
  configuration or not. Matching its basename against the harness table - `claude`, `opencode`,
  `codex`, `pi` - is a string compare. So "which panes run an agent" needs no work from the
  operating system at all.
- *Phase two runs only for a pane phase one matched.* The harness's own session id lives in the
  environment of the pane's processes, and that means walking the subtree - the same walk
  `resurrect_command_hints` does, through the same code. **A session with no agent pane in it does
  not read the process table at all.**
- *And it runs once per pane, not once per second.* A walk is a full process-table read plus one
  environment read per descendant, and its answer does not change while the pane runs the same
  program. So a pane is walked when it appears, when its process is replaced, and - only while
  nothing has been found - once every thirty seconds. A pane whose harness has been identified is
  never walked again. The walk is also asked only for the matched harness's own variable names, not
  for every harness's, which is what lets it stop at the process that has them instead of reading
  the whole subtree every time.

  Measured on a session with one agent pane, the pty thread went from **9,800 read syscalls and
  2.1 MB a second** to the same numbers as the same session with detection turned off - the walk had
  been running on every tick.

`COMMAND` names the command line the row was decided on - the pane's live argv when that is what
matched, the line the pane was STARTED with when the fallback answered. The column exists to make a
wrong row obvious, and it cannot do that while showing a line the row was not decided on.

`SOURCE` says which phase answered: `command` when only the pane's command matched, `command+env`
when an identity variable was found too. A reader that needs to know whether a missing `AGENT_ID`
means "the harness does not export one" or "we never looked" reads that column rather than guessing.
The identity variable names are best effort - a harness that renames its own is still detected, just
without an id.

**It is on the cheap surface, deliberately.** `agent` is a field on `PaneListEntry`, which is
CLI-only, and **not** on `PaneInfo`: no protobuf tag, no plugin contract, nothing to carry through a
rebase. `list-agents` adds no `Action` and nothing to the client/server contract either - it is
answered by the client from one `list-panes --json`, the way `wait` is. The identity variables are
kept in a server-internal field and never put on `PaneInfo`, which is published over the plugin
API.

Turn it off with the top-level key, on a machine where reading a process's environment is unwanted:

```kdl
detect_agents false
```

**Off means off, both phases.** The pty tick stops reading identity variables and the pane list
stops matching commands, so `list-agents` prints its header and nothing else, `list-panes` prints
`-` in `AGENT`, and `list-panes --json` carries no `agent` key on any entry. Both halves of the
answer live in different threads, so the option travels to the pane list with the process info it
governs, and a config reload flips it in either direction without a restart.

Being top-level, it is ignored by a binary that predates it, so it can go into a shared config ahead
of the upgrade.

### `list-agents` names the session claude holds now

From nkmk.29. `AGENT_ID` for a claude pane came from `CLAUDE_CODE_SESSION_ID`. The claude process does not carry
that variable; its children do, each frozen at its spawn. So after a `/clear` the column still named
the first session, for as long as the pane lived. The cause is the one [the resume
entry](#a-restored-session-offers-to-resume-its-agents-and-only-its-agents) describes.

The walk now also reads Claude Code's own record, `sessions/<pid>.json` under `$CLAUDE_CONFIG_DIR`
(else `~/.claude`), with the same code serialization uses. A record's `sessionId` wins over the
variable; a pane with no record keeps the variable's value. `SOURCE` stays `command+env`.
From nkmk.30, a layout `command "claude"` pane gets an `AGENT_ID` too: the live list reports it as
its own command, [not its newest child](#a-command-pane-is-recorded-as-its-own-command-not-its-newest-child),
so detection matches it.

The walk still runs once per pane. It remembers which claude pid's record answered, and every tick
re-reads that one file: a few hundred bytes, not a process-table read. That is how a `/clear` is
seen without a new process. When the record is gone, that claude has exited, and the pane is walked
again.

`agent_id` is a field of the CLI-only `PaneListEntry` and `AgentListEntry`. Only its value
changes: no protobuf tag, no plugin API, no client/server contract.

From nkmk.31, the same walk finds codex's session from the rollout file it holds open, an agent's
own `OSC 1337` report beats both, and every agent row in `list-agents --json` carries
`session_source`: `osc`, `record`, `open-file`, `env`, `argv`
(the id the agent was started with, when nothing live named one), or `picker-fallback` (nothing
named the session, and a restore would offer the agent's picker). The codex handle is re-read
every tick like claude's record, so a `/new` is seen without a new process. `session_source` is a
new field on the CLI-only `PaneAgent`, omitted when unset: still no protobuf tag, no plugin API,
no client/server contract. The table keeps its columns.

### What an agent says it is doing: `OSC 7501`, and `state` on `list-agents`

From nkmk.33. `list-agents` said which pane runs which agent, not whether that agent is working,
waiting on a person, or finished. A controller had to read the screen and guess.

```
$ zellij action list-agents --json
[{"handle": "sunny-otter", "kind": "claude", ..., "state": "blocked",
  "state_kind": "permission", "state_app": "claude-code",
  "state_msg": "Allow Bash: cargo test?", "state_at": 1760000000000}]
```

The answer comes from the agent. The Program Status Protocol (`OSC 7501`,
[specification](https://www.superlogical.com/rex/docs/build/program-status)) lets a program tell
its terminal its state: `ESC ] 7501 ; state=blocked:kind=permission:app=claude-code:msg=<base64> ST`.

**zellij answers the protocol's query, and that is the whole switch.** Claude Code (2.1.295 and
later) and pi (1.1) send `OSC 7501 ; ?` at start-up and report nothing until a terminal answers.
zellij is the terminal of every pane, so the grid answers the query itself, in the query's own
terminator: `ESC ] 7501 ; ?:states=idle,working,done,blocked,error:kinds=permission,question,auth`.

**Upstream v0.46.0 parses the protocol too**, for its own attention tracker, and answers a bare
`?`. The fork carries no parser of its own. The grid's `7501` arm reads each sequence with
upstream's parser and feeds the result to both upstream's tracker and the pane's own records
(`read` and `from_upstream` in `zellij-server/src/program_status.rs`). What the fork adds:

- the answer to a query with pairs after the `?`, which upstream's parser does not take;
- the per-pane records below, and the flat `state*` keys they feed;
- the guarded `send-message` draft, which reads the same records.

Two parse details follow upstream's parser. A pair whose value holds a byte outside the value set
is skipped, and the rest of the report stands; a pair whose base64 does not decode still discards
the report. An `id` takes only upstream's byte set.

The grid keeps the reports as the specification says a terminal should:

- One record per `id`. A report replaces its record whole. `state=clear` removes the record named
  and its children, or every record when it names none.
- A new prompt (`OSC 133;A`) and the pane's process exiting remove `working`, `blocked` and `idle`.
  A `done` or `error` result stays while that program, or the prompt after it, is in the pane.
- **A result does not outlive the next program.** The specification leaves this to the terminal,
  and without it a pane that ran claude to `done` showed that `done` on the next agent started in
  it, until that agent first reported. Two signals start a new program:
  - A command start (`OSC 133;C`) removes every record reported before the last `133;A`. A record
    reported after it stays. So the shell running a new command drops the last program's result,
    and claude, which writes `133;A` as a turn starts and `133;C` as it ends, keeps its own.
    Before the first `133;A` a command start removes nothing.
  - The feature query (`OSC 7501 ; ?`) removes every record. Claude and pi ask once, as they
    start, so this covers a shell that writes no marks.
  - A change of the pane's foreground process is not a signal: the grid does not see it. A
    program that neither asks nor follows a marked prompt (codex through its hooks, under a shell
    without marks) shows the last result until its first report.
- A full reset (`ESC c`) removes every record. A soft reset does not.
- At most 64 records. Past that the least recently updated goes.
- A report upstream's parser rejects is discarded: no state, an unknown state, an id or a text
  field over its limit, base64 that does not decode, control characters in the text.

**The pane's most urgent record is the one reported**: `blocked`, then `error`, `done`, `working`
and `idle`, the newest first within one state. That is the rule rootshell applies, so the two read
one pane the same way. It rides on `list-panes --json` and `list-agents --json`, flattened:

| key | value |
|---|---|
| `state` | `idle`, `working`, `done`, `blocked` or `error` |
| `state_kind` | why a `blocked` program waits: `permission`, `question` or `auth` |
| `state_app` | the program's name: `claude-code`, `pi`, `codex`, `opencode`. A child record takes its parent's |
| `state_title` | the task label, decoded |
| `state_msg` | one line, decoded: the question, the error |
| `state_progress` | 0-100 for `working` or `blocked`. Absent means indeterminate |
| `state_at` | when the record was reported, in milliseconds since the epoch - the unit of `last_output_at` |
| `state_working` | how many of the pane's records say `working`, background subagents included. Absent when zero. See [the teardown guard](#session-down-and-session-restart-refuse-while-an-agent-is-working) |

Every key is absent, not null, when nothing was reported. The keys carry a `state` prefix because
the row already has a `title`. `zellij_overview` with `scope=agents` reads `list-agents --json`, so
the MCP answer carries the same keys.

The `list-agents` table gains a `STATE` column, **last**, because a column is never put before one a
reader already counts to: `blocked:permission`, `working`, `-`.

**Which agents report.** Claude Code and pi report natively once answered. codex 0.162 and
opencode 2.0 do not speak the protocol. For codex, `zellij setup --agent-hooks` now has
`agent-session.sh` report for it: `sh agent-session.sh codex state <state> [kind]` prints the
`OSC 7501` to codex's terminal, from `UserPromptSubmit` and `PostToolUse` (`working`),
`PermissionRequest` (`blocked`, `permission`) and `Stop` (`done`). Like the session report, it runs
only inside zellij. From nkmk.34 the opencode TUI plugin reports opencode's, from opencode's own
events: see [opencode reports its state from its TUI
plugin](#opencode-reports-its-state-from-its-tui-plugin).

**It is on the cheap surface.** The records live on the pane's grid in the screen thread, and the
summary goes onto the CLI-only `PaneListEntry` and `AgentListEntry`. No `PaneInfo` field, no
protobuf tag, no plugin API, no client/server contract. Nothing is forwarded to the host terminal:
the host keeps one set of records per window, and a client shows many panes.

Capability: `program-status-osc`.

### `list-agents --all-sessions`

From nkmk.33. `list-agents` answered for one session. A controller that drives agents across
sessions had to run `ls --json` and ask each session in turn.

```
$ zellij action list-agents --all-sessions
TAB_ID  TAB_NAME  PANE_ID  HANDLE       KIND    ...  STATE               SESSION
1       develop   3        sunny-otter  claude  ...  blocked:permission  work
0       Tab #1    1        brisk-heron  codex   ...  working             notes
```

**`--all-sessions`** asks every live session in the socket directory, in name order, and each row
gains `session`. The table puts `SESSION` after `STATE`, last, as the append-only rule says. It is
answered before a session is chosen, so it needs no `-s`, ignores one, and with no session running
prints the header alone and exits 0. A session that does not answer is named on stderr and left
out: one wedged server should not hide every other agent on the machine. It exits 1 only when there
were sessions and none answered.

To follow agents as they change, read [`zellij action watch`](#zellij-action-watch-the-session-pushes-its-changes)
or block on `action wait --for agent-change`.

`AgentListEntry` gains `session`, absent outside this mode. Still CLI-only: no protobuf tag, no
plugin API.

Capability: `list-agents-all-sessions`.

### `zellij action watch`: the session pushes its changes

```
$ zellij action watch --json
{"event":"hello","server_version":"0.45.1-nkmk.39","server_build_id":"9f3c...","session":"work","seq":0,"epoch":"5be1c0a2d4f7e913","ts":1761900000000,"server_build":"same"}
{"event":"agent","self":false,"handle":"sunny-otter","pane_id":3,...,"seq":1,...}
{"event":"pane_opened","pane_id":"terminal_3","handle":"sunny-otter","tab_id":0,"seq":2,...}
{"event":"tab_opened","tab_id":0,"seq":3,...}
{"event":"synced","seq":4,...}
{"event":"pane_output","pane_id":"terminal_3","handle":"sunny-otter","last_output_at":1761900001234,"seq":5,...}
{"event":"pane_exited","pane_id":"terminal_5","handle":"calm-fox","exit_status":1,"seq":6,...}
{"event":"pane_changed","pane_id":"terminal_3","handle":"sunny-otter","tab_id":0,"title":"~/src","program_title":"~/src","pane_cwd":"/home/user/src","pane_command":"zsh","terminal_command":null,"command_state":"prompt","seq":7,...}
```

A controller that drives zellij over the CLI, locally or over ssh, had to poll: `list-tree` for the
shape of the session, `list-agents` for its agents, `list-panes` for output. This is one
long-lived command the server writes to as things change, one JSON object per line.

- **Every line** carries `seq`, `epoch` and `ts`. `seq` counts this stream's lines from 0, with no
  gaps. `epoch` is a random id that is new each time a server starts, so a reader that reconnects
  knows whether `seq` restarted because the server did. `ts` is milliseconds since the Unix epoch.
- **First comes the snapshot.** `hello` (`server_version`, `server_build_id`, `session`, and
  `server_build`: `same`, `different` or `unknown` against the binary reading it), then one `agent`
  line per agent, one `pane_opened` line per pane, one `tab_opened` line per tab, then `synced`. A
  reader needs no separate first call.
- **`agent`** carries the whole `list-agents --json` row; **`gone`** (`pane_id`, `handle`) drops
  it. The change filter: identity (`handle`, `kind`, `agent_id`, `name`)
  or any `state` key. A title change is not a change. A handle renamed under a live agent is a
  `gone` for the old handle and an `agent` for the new one.
- **Pane lines are ids only**: `pane_opened` (`pane_id`, `handle`, `tab_id`), `pane_exited`
  (`exit_status`, null when killed by a signal or unreadable), `pane_closed`. What a pane says
  about itself is on `pane_changed`, below. **`handle` is on every pane line**, closed and exited included. A pane
  is identified across looks by its uuid, so a reused id is a close and an open, never a
  continuation. A pane whose handle is renamed keeps its identity and carries the new handle on
  its later lines; there is no rename line.
- **Tab lines**: `tab_opened`, `tab_closed`, `tab_renamed` (`name`). `tab_id` is the stable id,
  not the position.
- **`pane_output`** (`pane_id`, `handle`, `last_output_at`) says a pane produced output: at most
  one line per pane per second, and the newest output is never lost. A burst inside the second is
  reported once the second has passed, by the stream's own tick. Output from before the stream
  started is not reported.
- **`pane_changed`** carries what a pane says about itself: `pane_id`, `handle`, `tab_id`, and
  the fields of its `list-panes --json` row under the same names - `title`, `program_title`,
  `pane_cwd`, `pane_command`, `terminal_command`, `command_state`, `last_command_exit_code`, and
  the `OSC 7501` keys `state`, `state_kind`, `state_app`, `state_title`, `state_msg`,
  `state_progress`, `state_at`, and upstream's `attention` object, nested under `attention`
  exactly as on a `list-tree` row and always whole. A key is absent, or null, exactly when the
  `list-panes` row has it absent or null. The line carries every field, not only the one that
  moved. It is written when any of those fields, or `tab_id`, changes; a change in `attention`
  alone (its `status`, or `seen` flipping) is one. A new `state_at` alone is not a change, as on an
  `agent` line, and neither is a renamed handle. `input_since_prompt`, `input_since_state` and
  `prefilled_command` are not carried: typing flips them. **A pane moved to another tab** is a
  `pane_changed` with the new `tab_id`. **At most one line per pane per second**, like
  `pane_output`: an agent animating its title is one line a second, the newest values win, and the
  trailing change is never lost, because the stream's tick writes it once the second has passed.
  A change undone inside the second is no line at all.
- **The snapshot carries no `pane_changed`**, and `pane_opened` stays ids only. A reader gets the
  fields by reading `list-tree` (or `list-panes --json`) once, after `synced`; from then on the
  stream keeps them current. A pane opened after `synced` is followed by its own `pane_changed`
  in the same look, so the one read is enough. A change that lands between `synced` and that read
  may be told twice; the later line wins.
- **Order inside one change**: a tab opens before its panes, a pane opens before its agent, an
  agent's `gone` comes before its pane closes, a pane closes before its tab. `pane_changed` and
  then `pane_output` lines come last.
- **Watching is not looking.** Neither a `watch` client nor a one-shot `zellij action` call is a
  viewer: upstream's `attention.seen` flips only for an attached terminal client showing the pane.
- **Exit 0** when the reader closes the pipe, noticed even on a quiet session (the client asks its
  stdout every half second). **Exit 2** when the session goes away. **Exit 2 at once**, within five
  seconds and with the reason on stderr, against a server that predates the command.

**How the server knows.** Nothing new is tracked. The server builds the same pane list
`list-panes` answers with, and the agents `list-agents` projects from
it, and each stream remembers what it last told its reader and sends the difference. It looks when
the pane manifest is reported to plugins (every open, close, exit, move and rename), when the pty
reports new process information (an agent's identity), after a render (OSC 7501 state and
output), at most once per 100ms there, and on a 250ms tick that runs only while a stream exists.
A session nobody watches pays one empty-map check per render. `pane_output` costs nothing on the
render path: the per-pane output stamp already existed for `last_output_at`.

**The contract.** Two messages, appended, nothing renumbered:

| Direction | Message | Tag | Fields |
|---|---|---|---|
| client to server | `ForkSubscribeToSessionEventsMsg fork_subscribe_to_session_events` | 900 | `bool agents_only = 1` |
| server to client | `ForkSessionEventMsg fork_session_event` | 900 | `string line = 1` |

The server writes each line's JSON itself, so the contract carries one string per line and a new
event or field changes no message. `agents_only` asks for the `hello`, agent and `synced` lines alone: this build's client never
sets it, and the server honours it for an older client's `list-agents --watch`. A stock server, or a fork
server from before this, reads tag 900 as an unknown message, logs it and ignores it, so the client
times out waiting for `hello` rather than hanging. The verb is not an `Action`: it opens its own
connection, as `subscribe` does.

Capability: `action-watch`.

### `zellij mcp`: the CLI served over the Model Context Protocol

```json
{ "mcpServers": { "zellij": { "command": "zellij", "args": ["mcp"] } } }
```

From nkmk.35 the server also drives sessions on other machines: see [`zellij mcp` reaches sessions
on other machines over ssh](#zellij-mcp-reaches-sessions-on-other-machines-over-ssh).

`setup --dump-surface` already describes the whole command tree in one call, which is most of what
an agent needs. What it cannot do is help a harness that **cannot shell out**, and it cannot be
gated per verb - an MCP client allows or denies each tool by name, which on a machine where "read a
pane" and "close a tab" want different answers is the whole point. Hence a server, in the binary, so
it is version-synced and there is nothing to install.

**Six tools, not eighty-seven.** `zellij_overview` (panes, agents, or the sessions on this
machine), `zellij_read_pane`, `zellij_wait_for`, `zellij_write_input`, `zellij_create`,
`zellij_close`. `zellij_close` is the one that cannot be undone, kept apart so a client can allow
the others without it. Session lifecycle -
`up`, `down`, `restart`, `enable` - is deliberately not among them: those start and stop the thing
the server is talking to. A test fails if the surface grows past six tools, or a tool past nine
parameters (eight until nkmk.32, when `zellij_create` took `run_in` and `wait_ready`).

**The descriptions are generated from the surface map.** A tool's `Returns:` line is built from the
same `OUTPUTS` row `--dump-surface` prints, and each input-schema property that stands for a real
flag carries clap's own help for it. Rename a flag in `cli.rs` and the build fails rather than the
description going stale; add a column to a table and the tool says so on the next build. Only the
routing prose - what a tool is for, what it is *not* for, which tool to reach for next - is written
by hand, because that is the part that decides whether a tool is called at all.

**Every tool runs this same binary as a child process**, rather than calling the action path
in-process. That path prints to stdout and exits on a miss; on a stdio server stdout is the protocol
stream and the process is the session, so one missed pane would corrupt the first and end the
second. A child process also maps the fork's exit convention straight through - **0 a result, 1 an
error, 2 a miss** - which is what lets `zellij_create` be honest: a pane that was not made reports
the CLI's own refusal, and there is no id anywhere for it to invent. What cannot be undone lives in
`zellij_close`, where `close_pane` passes the `--yes` the CLI would otherwise refuse to run without
and `close_tab` has no confirmation to pass.

Which session a call is about: the tool's own `session`, else `ZELLIJ_SESSION_NAME` in the server's
environment.

**An abandoned call costs nothing.** MCP clients time out, disconnect and restart, and they do it
without telling the server, which simply sees the future dropped. Every child is spawned with
`kill_on_drop`, so a dropped call takes its child with it - at cancellation and at shutdown alike,
because the runtime drops its pending tasks when stdin reaches EOF. `zellij_wait_for` is bounded
too: without `timeout_s` it gives up after 300 seconds rather than blocking for the life of the
pane, which is the default its own schema has always advertised. That bound is the server's, not
the client's: a client whose request deadline is shorter (often 60 seconds) gives up first, so
`timeout_s` tells the caller to stay under that deadline and wait again.

**Every tool declares the shape of what it returns**, and it is the same shape for all six: the
CLI's exit code, what it printed - parsed when that was JSON - what it wrote to stderr, and on a
failure whether it was a miss or an error. The per-operation part stays in the `Returns:` line,
which is generated. A tool that multiplexes several operations says what each of them returns
rather than promising the first one's shape for all of them.

**The tool list carries `ttlMs` and `cacheScope`**, which protocol version `2026-07-28` requires of
every list result (SEP-2549). rmcp models both as optional, because one Rust type has to serve the
older revisions too, so a server that never sets them compiles and then emits a list the newer
schema rejects. A client that opens with `server/discover` - Claude Code does - is told the server
speaks `2026-07-28`, sends `tools/list` in that era, and refuses the reply: the connection succeeds
and **no tools reach the model**, which reads as a missing server rather than a protocol fault. A
client that negotiates `2025-11-25` through `initialize` never sees it. The fields are set rather
than the advertised versions narrowed, so the server conforms to the era it claims instead of
opting out of it, and a test asserts both are on the wire.

New dependency: `rmcp` (the official Rust MCP SDK) with `server` and `transport-io` only - eight
crates, no HTTP stack, and nothing on any wasm plugin crate. `Cargo.lock` holds it at 3.1.2, the
version the fork is tested with; a fresh resolve takes a newer 3.x, so move it on purpose.

Two rows were added to the surface map while wiring this up: `snapshot list` and `snapshot show`
print a table and a payload respectively, and the map had been claiming they printed nothing.

**A tool call survives an upgrade (nkmk.25, 2026-09-29).** The MCP server is long-lived and spawned
`current_exe()` for every call. On Linux an upgrade that renames a new build over the binary turns
that path into `<path> (deleted)`, so every tool call failed with `ENOENT` until the harness
restarted the server. Calls now spawn `own_exe_for_spawn()`: the same path without the suffix, or,
when the versioned directory is gone, the first `zellij` on `PATH`. A call after an upgrade runs the
new build. Two unit tests cover the suffix and the fallback.

**The key and handle docs say what the CLI accepts (nkmk.26, 2026-10-02).** `zellij_write_input` taught
`C-c` and `Escape`, and the key parser refuses both: it takes `Ctrl c` and `Esc`. It also said
`keys` took several keys, but the whole string is one `send-keys` argument, so it is one key per
call. A test now parses every backticked key in the tool's text.

### `session up` comes back with the shape the session had

```
zellij session up      [NAME] [--fresh | --restore [ID]]
session_up_resume true   # config.kdl, top level, default true
```

A bare `session up` used to mean two different things depending on a file nobody looks at. `up`
ends in `attach --create`, and `attach` resurrects a dead name from the in-place cache
(`session_info/<id>/session-layout.kdl`, the folder of the session's id) by itself — so a crash, a
SIGKILL and a reboot already came back with the previous panes. `session down` and `delete-session` **remove** that file after
archiving a snapshot of it, so the same command after a `down` built the session from the layout
instead. The archive held the shape and nothing but an explicit `--restore` ever read it.

That gap is not a rare one, because the watchdog runs `session up` every minute: a `session down`
followed by a minute of doing something else came back as a default layout, and the shape was still
sitting in the archive.

`up` now resolves one of three sources, in this order:

| What is on disk | What `up` builds from |
|---|---|
| the in-place cache | that (unchanged — `attach` does it) |
| no cache, a snapshot for this name | the **newest snapshot**, named with its age in the output |
| neither | the layout (unchanged) |

`--fresh` is the way to the layout, and it discards the in-place cache rather than ignoring it, the
same as `attach --no-resurrect`. The archived snapshot survives that, so a shape thrown away by
`--fresh` is still reachable with `session up --restore`.

The three states are an enum (`Resume`, `Fresh`, `Snapshot(id)`), and that is load-bearing rather
than tidy. `session restart --fresh` used to say "come back from no snapshot" by passing `None`,
which under a resuming default is now indistinguishable from "come back from whatever you find".
The enum is what keeps `restart --fresh` on the layout. When the restart hands creation to a launch
agent or unit, whose `up` is a bare one, a marker carries the shape across instead — see "The lock
could not carry the shape across a handover" above.

Two behaviours follow from the difference between a shape somebody **named** and a shape `up`
**derived**:

- `--restore` into a running session still exits 2 — there is nothing to restore into. A bare `up`
  that finds the session running reports "already running" as it always did, and does not start
  exiting 2 at the watchdog once a minute.
- A snapshot that no longer parses fails the command when it was named (`attach --restore` exits 2
  rather than starting something the caller did not ask for), and only **warns** when `up` picked
  it. A watchdog tick that leaves the machine with no session at all is a worse answer than a
  session from the layout and a line saying why. The unreadable snapshot is left on disk.

`session_up_resume false` in `config.kdl` goes back to the old behaviour. It is top level, so a
binary that predates it ignores it rather than failing the whole config.

The cost of the new default is that a `down` followed by an `up` a fortnight later rebuilds a
fortnight-old shape. That is the requested semantic — the shape is what the archive is for — and it
is why the output names the snapshot and how old it is rather than restoring quietly.

### The server serializes and archives on SIGTERM

A session used to die badly on the ordinary ways a machine ends one. `systemctl --user stop`, a
logout and a reboot all send the server SIGTERM, and the server had no handler for it: the process
went at once, so the newest shape on disk was whatever the periodic serializer last wrote — up to a
whole `serialization_interval` old, 60 seconds by default — and **no snapshot was archived at all**.
The one moment a person most wants the shape back is the reboot, and it was the one path that cut
nothing.

The server now listens for SIGTERM on its own thread and sends itself `ServerInstruction::KillSession`,
which is the same graceful path `zellij kill-session` has always taken: serialize once more, tell
every client, then archive on the way out. Nothing new can hang in it that could not hang there.

The thread is not started for the in-process server the integration tests run — that one shares the
harness's process, and a test runner's signals are not its to answer.

It pairs with `session up` resuming by default: the reboot now leaves a current snapshot, and the
`up` that follows it finds one.

### Creating a session never takes a live server's socket

`zellij attach --create` on a session that is running created a **second** session on top of it, and
the first one survived the experience only as a process nobody could reach. This is not theoretical:
it happened on 2026-08-16 to a session with 10 tabs and 41 panes.

The mechanism is a liveness probe that answers the wrong question for a destructive decision.
`session_exists` -> `get_sessions` -> `assert_socket` connects to the socket, sends `ConnStatus` and
waits for `Connected`; a server that **accepts the connection but does not complete that exchange**
is reported as absent. For a listing that is a defensible answer. For `--create` it is fatal, because
the create that follows binds the same path, and `ipc_bind` is preceded by an unconditional
`remove_file`. The old server keeps running, keeps its panes and keeps its listening socket -- but a
unix socket cannot be given its pathname back, so nothing can ever reach it again. The session is
gone while every one of its processes is still alive.

So the fork asks a smaller question before any create: **is anything bound**. A bare `connect()`
that succeeds means a live process is listening, whatever it goes on to say, and that is enough to
refuse. `sessions::socket_is_occupied` is that question, and it now guards three places:

- **`attach --create`** and its siblings, before the branch that resurrects or restores as well as
  the one that starts fresh -- all three start a server on that socket. The check lives in
  `zellij-client/src/session_resolution.rs`, beside upstream's attach resolution.
- **`zellij --session <name>`** (`assert_session_ne`), the plain create.
- **`start_server`**, the last thing that can still refuse. The client's check can be raced or
  bypassed; the bind cannot.

A stale socket a dead server left behind still gets cleaned up and replaced, because `connect()` to
it fails. Only a socket with something alive behind it is protected.

Upstream v0.46.0 names a new session's socket and `session_info` folder by a random id and maps
names to ids in a session index. Every fork path built from a session name resolves the id first,
through `zellij_utils::sessions::session_id_for`, so a fork build and a stock build of the same
contract find the same sockets. The fork's `.{name}.up.pending` marker stays keyed by name: only
the fork reads it.

The refusal names the socket, says why the session is not in `zellij ls`, and points at
`zellij attach` to reach it or `zellij session restart` to replace it deliberately.

### OSC 8 hyperlinks are terminated with BEL, not ST

A hyperlink zellij draws now ends with BEL (`\x07`) instead of the 7-bit string terminator
(`ESC \`). `TERMINATOR` in `zellij-server/src/panes/link_handler.rs` is the one place that decides
it, and both the link start and the link end read it.

The reason is mosh. `mosh-server` up to and including 1.4.0 does not parse OSC at all: its
terminal emulator hands the whole sequence to an unknown-escape path, and that path clears the
pending-wrap flag as a side effect (mobile-shell/mosh#687). Pending-wrap is what remembers that the
cursor has written the last column and must wrap before the next character, so clearing it discards
the cell waiting there. That mechanism is real: a synthetic byte stream fed through mosh 1.4.0
reproduces the lost cell, and BEL in place of ST prevents it. Zellij emits an OSC 8 link *end* at
the start of essentially every rendered row, so it emitted the trigger byte constantly.

**This patch is hygiene, not a proven fix.** Zellij's own render path was never shown to hit the
mechanism. Zellij writes each row as a chunk that begins with an absolute cursor move, and that
move clears mosh's pending-wrap flag before any OSC 8 terminator in the chunk can. So the trigger
byte is gone, but no observed symptom is known to depend on it. In particular this does not explain
the missing characters seen down the right edge over mosh in the field; that evidence still points
at the terminal's font shaper, not at the terminator.

Nothing is lost by the change. BEL is the older of the two OSC terminators, every terminal this fork
targets accepts it, and the terminals that prefer ST accept BEL as well — zellij already ends its
own window-title OSC with BEL. `osc8_hyperlinks true`, which is the default, stays usable
everywhere it was before.

The two unit tests in `link_handler.rs` build their expected strings from `TERMINATOR`, so they
follow the constant rather than pinning the old bytes. The `zellij-terminal` test models compare
the server's grid with the client's, and the reference projection reads each link's URI from
`LinkHandler::uri` rather than parsing it back out of the emitted sequence. Upstream's version
stripped only `ESC \`, so every hyperlink comparison saw the URI with a trailing BEL.

Proved end to end as well: a pty client attached to a scratch session captured 174 OSC 8
sequences, all BEL-terminated and none ST-terminated, with the only remaining `ESC \` in the
stream belonging to the OSC 10/11 colour queries and a kitty graphics probe.

Deliberately unchanged: `zellij-utils/src/setup.rs` still writes ST-terminated links. That is the
`zellij setup --check` report, printed by the CLI to its own stdout rather than drawn through a
pane, so it is not on the path that loses the cell.

### `session up` always creates the session through the launch agent

On macOS, `zellij session up` and `zellij session restart` now ask launchd to start the installed
launch agent whenever one is loaded — including when they are typed at a terminal, which is to say
from inside the `Aqua` domain. Until now the graphical domain was treated as settling the question,
so a `restart` typed in a pane spawned the new server itself and never went near the agent.

The domain it got that way was right. What was wrong is the other thing creating a server settles:
which executable macOS holds **responsible** for it. A server spawned by the client is attributed
to the client's binary, and under Homebrew that is a versioned Cellar path — `.../Cellar/
zellij-nkmk/0.45.0-nkmk.13/bin/zellij` — which is a different path after every upgrade. TCC decides
about Desktop, Documents and Downloads at server start, against the responsible path, so the Full
Disk Access grant made against the pinned copy at `~/Library/Application Support/zellij/bin/zellij`
was never consulted and the prompts came back at every upgrade. The launch agent already runs the
pin. Routing creation through it keeps every server on one path, for ever.

The job runs `zellij session up <name>` itself, so it has to know not to hand the work back. Two
signals answer that:

- **`XPC_SERVICE_NAME`**, which launchd sets in the job's own environment to the job's label. This
  is the whole guard on every ordinary machine, and **nothing about the plist changes for it** —
  confirmed by reading it out of `launchctl print` on a Mac whose agent was written two releases
  ago. An earlier version of this patch added a `ZELLIJ_SESSION_SERVICE_JOB` key to the generated
  plist to carry the same fact; it worked, and it cost every Mac a `zellij session enable` before
  the guard could see anything. launchd's own variable costs nothing.
- **`launchctl print gui/<uid>/<label>`**, whose `pid` is compared with this process's and with its
  parent's. The fallback, for an agent that reaches zellij through a wrapper script — there the
  variable belongs to the wrapper's environment and may not reach us.

The environment test is an **equality against the label, never a presence check**. An ordinary
process carries `XPC_SERVICE_NAME=0` rather than nothing, so a presence check would call every
caller the job and nothing would ever kickstart again.

`getppid() == 1` is deliberately **not** one of them. `session restart` daemonizes before it does
any of this, so a restart typed at a terminal has a parent of 1 as well, and reading that as "I am
the job" would stop the exact command this entry is about.

Handing the work over hands the up-lock over with it. `up` holds `lock_up(name)` across its check
and its creation, and `restart` holds it across its `down` and its `up` — both right for as long as
this process is the creator, and both exactly wrong the moment launchd becomes one. The kickstarted
job's own `session up` waits for that lock while this process waits for the session that job was
going to create; nothing on either side can see that it is waiting for itself. It ends with the
30-second wait running out, `restart` reporting a post-condition failure, and the session appearing
anyway once the lock is finally released. So the `Kickstart` branch calls
`session_lifecycle::hand_over_up_lock` before it waits, which releases the `flock` and drops this
thread's record of it while the `UpLock` values up the stack stay alive as handles over nothing.

Unchanged: a plist that is on disk but **not loaded** is still not a job to defer to, and `up`
creates the session itself — now with one line saying so and naming `session enable`. A graphical
shell on a machine with no agent at all still creates the session itself with no warning, because
nothing is missing there. `session down` does not unload the agent, so a `restart` always has a
loaded job to kickstart.

There is an off switch, in the block the unit is generated from:

```kdl
session_service {
    restart_via_launchd false
}
```

It turns off **that one paragraph** and nothing else: a graphical shell goes back to creating the
session in place, because the domain there was always right. The non-graphical branch is the older
guard against a session created in a domain it can never leave, and the config does not get to
disable it. Default is on, which is the load-bearing half — the machines this matters for are the
ones whose config nobody has touched, and a key they must add to get the fix would reach none of
them. It exists because this path runs at login and at every watchdog tick on a machine nobody is
watching, and a Mac that cannot start its session is a bad place to need a new release.

It is a key **nested inside a block that parses its own children**, so it cannot be rolled out
ahead of the binary — an older build fails the whole config on it, not just the block. Ship the
binary first; the key is only ever needed after. (That is what every build up to and including
`0.45.0-nkmk.17` does. From [the entry that softened
it](#an-unknown-key-in-session_service-warns-instead-of-failing-the-config) on, a build ignores a
key it does not know — but the builds this had to be rolled out past are the older ones.)

Operator-visible, on upgrading each Mac and with **no `session enable` re-run and no plist change**:
`zjr` goes through launchd rather than spawning a server from the Cellar path, and the TCC prompts
stop coming back at every `brew upgrade`.

### A saved layout with `expanded` outside a stack still loads

```
# a resurrection layout an older build saved
tab name="work" {
    pane expanded=true command="zsh" ...
}
# before: "An expanded pane must be part of a stack"; no server, `session up` waits and
#         reports only the missing process, and the launch agent retries every few seconds
# after:  the session comes back; the log says
#         "Ignoring expanded=true on a pane that is not part of a stack (at byte 290)"
```

0.45.1-nkmk.40 could save `expanded=true` on a pane it did not wrap in a stack. Upstream v0.46.0
stops writing that (its serializer writes `expanded` only inside a stack), but its parser still
refused it, so a session saved by the older build could not come back.

- **The parser ignores `expanded` on a pane outside a stack**, with a warning in the log, instead
  of refusing the layout. Inside a stack it means what it did. The serializer is upstream's and is
  not touched.
- **`session up` names the saved layout when it is why no server came.** When the post-condition
  fails, no server process appeared, and the `up` resumed from the saved layout, it parses that
  layout again and prints the error, with `session up --fresh` as the way past it. On macOS this is
  the `up` that handed creation to the launch agent: the agent's own `up` printed the parse error to
  its log, and the waiting `up` printed only "no 'zellij --server' process". On Linux the client's
  own parse error already reached the caller and still does.

`zellij-utils/src/kdl/kdl_layout_parser.rs` (`parse_pane_node`), `src/session_commands.rs`
(`saved_layout_problem`). Tests: the issue's shape and a nested one load with no pane expanded, a
stack keeps its expanded pane, and the line `session up` prints. No contract change.

### The pin is not stale against itself

`pin_needs_refresh` — the question `session doctor` and its dry run ask, and the one
`refresh_pin_through_signing` acts on — lacked the same-file guard that `install_pinned_exe` has
had all along. Doctor run FROM the pinned copy, which is the ordinary case once the pin directory
is on `PATH`, was therefore told to refresh the pin with itself: it copied the file over itself,
re-signed it, and stamped it with its own hash. The next run from the package path read a stamp
naming the wrong source, called the pin stale, and copied back over the signature just made. The
two alternated for ever, and each half of the cycle threw away the grants the other half's
signature had earned.

The writer always answered this correctly, which is exactly what hid it: `install_pinned_exe`
refused while the question said yes, so only the callers that act on the *question* saw it.
`pin_needs_refresh` now asks `is_the_same_file` first, in the same order the writer does — which is
the property that function's doc comment already claimed.

### A release build is pinned with its own Developer ID, and nothing re-signs it

```
Changed
  signing   refreshed <pin> with the build's own Developer ID signature; nothing was signed here
            identifier org.zellij.nkmk, team 2Z88BYP37C
            designated => identifier "org.zellij.nkmk" and anchor apple generic and certificate 1[...
            the release signed and notarized it, and it was copied into place unchanged,
            so no certificate, keychain or password was needed on this machine
```

From nkmk.30. `release.yml` signs the macOS binary with the fork's Developer ID: team `2Z88BYP37C`,
identifier `org.zellij.nkmk`, hardened runtime, timestamped, notarized. A candidate is signed the
same way. Homebrew installs that binary byte for byte, so the keg binary already carries the
signature. The pin refresh then threw it away. It re-signed the build with a local rung: Apple
Development on one Mac, the minted certificate on another. Each rung wrote its own requirement and
needed the login keychain, which is locked over SSH. So an upgrade asked for
`ZELLIJ_KEYCHAIN_PASSWORD` to replace the best signature the ladder could have written.

Now a refresh first asks whether the new build is the release's own:

1. `verify_signature`, the same two-step check a freshly signed pin gets. The requirement must be
   anchored, and the binary must satisfy it under `codesign --verify --strict`.
2. The identifier must be `org.zellij.nkmk`, and `team_id_from_requirement` must read
   `2Z88BYP37C`. A local build or another team's Developer ID stops here.
3. `codesign --verify -R=<release_requirement()>`: Apple's Developer ID intermediate and leaf
   markers, the identifier and the team. The markers tell a Developer ID from an Apple Development
   certificate of the same team, which carries neither.

A build that passes is copied into a temp beside the pin (the signing run's `.zellij.sign.` name, so
the same sweep covers it). The copy is verified again, flushed, checked against a pin replaced
meanwhile, and renamed into place. The stamp records the build. Nothing runs `codesign -s` or
`security`, and no password is read. A copy or rename that fails leaves the previous pin in place
and does not fall to the ladder, because the ladder would meet the same filesystem.

Any other build went down the ladder exactly as before: a local `cargo build`, a stock upstream
binary, a source-formula build. An anchored pin with no refresh pending is still left alone.
(Superseded later in nkmk.30: the ladder is gone, and such a build is refused over an anchored
pin. See the next entry.)

**The first release pin owes a re-grant, once.** One Mac's pin is anchored on Apple Development
under another team, and another's on our minted certificate (`certificate leaf = H"…"`). The release
signature satisfies neither, so the switch is written to `signing/regrant-owed` through the nkmk.27
machinery. Every doctor run then asks for the re-grant until `doctor --fix --regranted`. A second
release over a pin already on the release requirement owes nothing and records nothing, and a
record already owed stays as it was. An unsigned or ad-hoc pin owes nothing, as before.

Every door into an anchored pin's refresh reaches this through the shared transaction.
`session up`, `session enable` and the interactive launch's `pin_exe` refresh go through
`install_pinned_exe` → `guard_anchored_pin` → `refresh_pin_through_signing` → `sign_pin`.
`doctor --fix` hands the refresh to its signing check, which calls `sign_pin` directly. Their pin
lines said "refreshed and signed", because the outcome was `PinOutcome::Signed`; the next entry
renames it `PinOutcome::AsReleased` and fixes the wording. A pin that is not anchored gets the plain copy, as before, and a plain
copy of a release build keeps its signature too.

`zellij-utils/src/session_signing.rs` only. `install_signed_build` and
`build_carries_our_developer_id` sit beside the ladder, and the grants tail of `perform_signing`
moved unchanged into `settle_the_grants` so both paths write the record the same way. Four
`sign_pin` tests, all on a machine with no keychain: a release build is pinned with no `codesign -s`
and no `security` call; the first release over another team's pin records the re-grant once and a
later run still asks; a release over a pin on the release requirement records nothing; and another
team's Developer ID, an Apple Development certificate of the release team, and an ad-hoc build all
still go down the ladder.

### The per-Mac signing ladder is gone; a release is signed once, in CI

```
Needs you
  signing   <pin> is not a release build, so the pin holds no grants
            its signature is ad-hoc: the requirement names its own code hash
            nothing is signed on this machine; the release is signed once, in CI
            install the brew release to get a signed pin, then `zellij session doctor --fix`
```

From nkmk.30. The entry above pins a release build as it arrived. That left the per-Mac ladder
doing one job: re-signing every build that was NOT a release. Nothing on the fleet needs that, and
the ladder was the largest single block of divergence in the fork: about 4,000 lines of
`session_signing.rs`, its tests, and the keychain handling around it. It is removed.

What went, from `zellij-utils/src/session_signing.rs` and its callers:

- the rungs: Apple Development (`requirement_for`, the team read off the certificate with
  `team_id_from_keychain` and `team_id_from_subject`), the minted self-signed certificate
  (`ensure_self_signed`, `mint_self_signed`, `pkcs12_arguments`, `import_bundle`, `set_aside`,
  `back_up_identity`) and the Developer ID rung that signed locally;
- the walk: `rung_ladder`, `choose_rung`, `sign_down_the_ladder`, `perform_signing`,
  `ladder_for_the_anchor`, `judge_fall`, `find_identities`, `parse_identities`,
  `foreign_zellij_identities`, `judge_reimport`, `sign_arguments`, `xcode_steps`;
- the keychain: `ZELLIJ_KEYCHAIN_PASSWORD`, `security unlock-keychain`,
  `security set-key-partition-list`, `default_keychain`, and the key-access remedies;
- the signing bundle: doctor no longer creates, imports, backs up or sets aside anything under
  `~/Library/Application Support/zellij/signing/` except `regrant-owed`. Any `id.p12`, `cert.pem`,
  `key.pem` or `zellij-signing-id.p12` an older doctor left is ignored, and left in place.
  `PinSigningPolicy::backup_dir` and `SigningContext`'s `keychain`, `keychain_password` and
  `backup_dir` went with it.

What stayed: the pin and its one writer, the RC refusal, `verify_signature`, `read_signature`,
`team_id_from_requirement`, the as-is install (`build_carries_our_developer_id`,
`install_signed_build`, `settle_the_grants`), the `regrant-owed` record and `--regranted`, the
leave-alone branch for an anchored pin that verifies, and the domain, launch-agent and
Full Disk Access checks.

The decision behind it, recorded 2026-10-06 at the operator's direction: from nkmk.30 the only
supported pin is the published, CI-signed build — a release or a release candidate poured by brew.
Nothing on a Mac signs anything, so a local `cargo build` can hold no grant, and doctor says so
plainly instead of trying to earn one.

What a build that is not a release gets now:

| Where | Before | Now |
|---|---|---|
| over an ad-hoc or unsigned pin | plain copy, then doctor signed it with the best rung | plain copy; doctor reports the `Needs you` above and signs nothing |
| over an anchored pin | signed into place by the ladder | refused: the signed pin stays, on the previous build (`PinOutcome::Kept`) |
| an anchored pin that does not verify | signed again | `Needs you`: remove the pin and run `doctor --fix` from the brew release |

**A pin on a retired rung is not fought.** One Mac's pin is anchored on Apple Development under
another team, and another's on the minted certificate. Both still verify, so doctor leaves them alone
and reports them `Already correct`. The next release build refreshes each pin as it arrived, and the
switch records the owed re-grant (see the entry above).

**`--sign` and `--no-sign` stay, with a narrower meaning.** Nothing signs, so `--no-sign` now means
"leave a signed pin exactly as it is, even for a release build". `--sign` is the default and is
still accepted, because older setup scripts pass it.

Two wording fixes from the rc.1 Mac proof:

- `session up`, `session enable` and doctor's `pin` line said "refreshed and signed the pinned
  copy" after an as-is install, because the outcome was `PinOutcome::Signed`. The outcome is now
  `PinOutcome::AsReleased`, and they say "refreshed the pinned copy with the release's own
  signature at <pin>". `PinOutcome::Signed` and `PinRefresh::Signed` are gone; nothing returns them.
- The owed re-grant said "an earlier run re-signed it with another certificate". After an as-is
  install it now says "an earlier run switched this pin to the release's Developer ID requirement,
  and the grants were made against the old one". A record from any other switch says "moved this
  pin to another requirement".

The warning a refused refresh prints on stderr now ends "Install the brew release to finish the
upgrade" instead of sending the reader to `doctor --fix` from a desktop terminal, which could no
longer sign anything.

Tests: about 90 tests of removed code went with it. Kept and adjusted: the signature parsing, the
re-grant record (`a_switch_stays_owed_on_later_runs_until_the_operator_says_it_is_made` now switches
through a release install), the refresh deferral, the temp sweep and the four as-is tests. New or
rewritten: an ad-hoc or unsigned pin is reported as not a release build with nothing run but
`codesign -d`; an anchored pin that does not verify is a `Needs you` and nothing signs it; and
another team's Developer ID, an Apple Development certificate of the release team and an ad-hoc
build are each refused over a signed pin, leaving its bytes and recording nothing.

### Panes can ask for the microphone (macOS only)

A program run in a pane asks for macOS permissions as the zellij server, the process macOS holds
responsible. Full Disk Access, Accessibility and Screen Recording worked that way, but the
microphone did not: macOS refused at once, without a prompt, and the Microphone pane in System
Settings has no "+" to add the grant by hand. Two things were missing.

- **An Info.plist.** macOS prompts for the microphone only for a binary whose Info.plist carries
  `NSMicrophoneUsageDescription`, and a bare Mach-O has none. `src/build.rs` embeds
  `assets/macos/Info.plist` into the `zellij` binary as its `__TEXT,__info_plist` section
  (`-Wl,-sectcreate`) on every macOS build, local or CI. Its `CFBundleIdentifier` is the signing
  identifier, `org.zellij.nkmk`.
- **The entitlement.** Under the hardened runtime the release is signed with, the microphone also
  needs `com.apple.security.device.audio-input`. `release.yml` signs with
  `assets/macos/zellij.entitlements`, and fails the job when the binary lacks the plist section or
  the signature lacks the entitlement.

Neither changes the signing identifier or team, so the designated requirement every existing grant
records is unchanged: no re-grant is owed. The first program that asks gets the system prompt once;
after that every pane inherits the answer.

### `connect_to_server` gives up on a session that answers to no server

The CLI client's socket connect retried every 50ms forever. That is the right answer while a
server it just asked to spawn is still binding its socket, and the wrong one for a session that
answers to no server at all — a crashed process, a socket file left behind, a name that was never
going to come up. Every verb against that session hung instead of failing: `action`, `subscribe`,
`attach`, all of them, because they all go through the same trait method.

The connect now gives up after five seconds and returns `Err("session '<name>' is not running (or
its server is unreachable): <io error>")`. The socket file is named by the session's id, so the
message names the session through `display_name_for_session_id`. The bound lives in `connect_ipc`
(`zellij-client/src/os_input_output_unix.rs`); `connect_to_server` keeps upstream's signature.
An interactive attach prints upstream's "Error: cannot connect to the session: " before the
sentence; the CLI verbs print it as it is.

Each of the CLI entry points reports that message on stderr and exits non-zero —
`start_cli_client` returns `1` (a server that failed to answer is the narrower failure class, not a
miss), `ask` and `wait_on_renders` fold it into the `Result` they already return, and
`start_subscribe_client` prints and calls `process::exit(1)` like its neighbouring failures do. The
web client's per-connection `spawn_session_if_needed` logs the error instead of propagating it —
`send_to_server` already tolerates an unconnected client by dropping the message and warning, so a
failed connect there costs one browser tab its session rather than the process.

This is a different timeout from `SERVER_APPEARANCE_TIMEOUT` in `src/session_commands.rs`: that one
is `session up`'s own wait for a server it just spawned, backs off up to 1.5s over ten seconds, and
is unrelated to this path — a slow `launchd` start (15-20s, measured) never reaches
`connect_to_server` until `wait_for_server` has already decided the server is up.

### A plugin pinning a pane did not tell anyone it had

`PluginCommand::SetFloatingPanePinned` reaches `Screen::set_floating_pane_pinned`, but its
`ScreenInstruction` handler stopped at the mutation - no render, no `log_and_report_session_state`.
It now ends with `screen.render(None)?; screen.log_and_report_session_state()?;` like its
neighbours. Before, the pin stayed invisible to every subscriber - `subscribe`, a plugin's own
pane manifest - until some unrelated change touched the session and dragged a report along with it.
The fork's `PaneInfo.is_pinned` field, and the test that worked around the missing report with a
`RenamePane` after the pin, have since been removed (see `docs/removed-patches.md`, "Eight pane
state fields"). The handler still reports, so any later change to pin state reaches subscribers.

The two **toggles** were missing the same two lines and now have them:
`TogglePanePinned`, which is the keybinding's route, and `TogglePanePinnedWithPaneId`, which is
`zellij action toggle-pane-pinned --pane-id`. Both mutated pin state and stopped, so a toggled pin
was as invisible to a subscriber as a plugin-set one was. No test workaround stood on these — the
only tests that reach them are a `MockScreen` smoke test that asserts nothing about the report and a
`Tab`-level unit test below the handler — so unlike the plugin path there was nothing to delete as
proof.

### A failed remote plugin download parked every later reload of it

Loading a plugin from a `Remote` location downloads it before the loader ever runs, on a path
separate from every other way a plugin can fail to start. Every one of those others reaches
`execute_plugin_load`, whose closure always finishes by sending `ApplyCachedEvents` - the only
thing that takes a `plugin_id` out of `loading_plugins`, whether the load succeeded or not. A
failed download `return`ed before that closure was ever entered, so its `plugin_id` never left.

`loading_plugins` is also what `plugin_is_currently_being_loaded` answers from, and `reload_plugin`
asks that question before doing anything else: told a location is still loading, it queues the
reload in `pending_plugin_reloads` and returns - and nothing was ever going to drain that queue,
because the drain also lives in `apply_cached_events`. A remote plugin that failed to download once
was stuck refusing every reload for the rest of the session, silently: no error, no retry, just a
location that stopped answering to `zellij action start-or-reload-plugin`.

The download-failure branch now sends `ApplyCachedEvents` itself before returning, matching what
every other failure path already does further down. A new test,
`a_failed_remote_download_does_not_park_later_reloads`, proves it end to end against a loopback
connection refusal - no real network, no e2e fixture needed, since the failure happens before
wasmtime is ever reached: a `Reload` sent after the failed `Load` should start a second attempt at
the same location, and does now.

That second attempt reuses the failed pane's plugin id rather than taking a new one, because
[reloading a pane whose plugin never loaded](#reloading-a-pane-whose-plugin-never-loaded) recovers
the pane in place instead of letting the caller fall through to spawning a stray one beside it. The
two patches were written apart and met here: the test asserts a
`StartPluginLoadingIndication` for the failed pane's own id, which `reload_plugin_with_id` sends
only after every refusal check has passed, so it still fails the moment this `ApplyCachedEvents`
is taken back out.

### `-s` reaches `attach` too, not just the no-subcommand form

The top-level `--session`/`-s` already named which session `zellij action` and `zellij subscribe`
target - both take it as their `requested_session_name`. `zellij attach` never read it: it has its
own positional `session_name`, and the dispatch used that field alone, `opts.session` unconsulted.
`zellij -s myname attach -c` therefore ignored `-s`, found no active session to attach `create`'s
fallback to, and minted a fresh random name instead - `-s` looked accepted (clap does not reject an
unused global flag) and silently did nothing.

`attach`'s own positional still wins when both are given - `zellij -s foo attach bar` means `bar`,
the more specific of the two - but now falls back to `-s` when it is empty, the same way `action`
and `subscribe` already do. Upstream v0.46.0 keeps attach's flags in `AttachArgs` and its
resolution in `zellij-client/src/session_resolution.rs`; the fallback fills
`AttachArgs.session_name`, and `--no-resurrect` and `--restore` are `AttachArgs` fields.

### The recorded `XDG_STATE_HOME` is read back, not re-derived

A generated unit records the `XDG_STATE_HOME` of the shell that enabled it, because the snapshot
archive and `restart.log` both hang off it and a launcher has none. That much was right. What was
wrong is that *every* later comparison regenerated the unit from **its own** environment and then
compared bytes — so the answer depended on who asked.

`zellij session status work` from a context that exports no `XDG_STATE_HOME` — cron, a systemd
timer, `ssh host zellij session status work` — regenerated a unit with no state-home line, reported
drift on a perfectly good install, and exited 1. `session up` printed the same warning at every
watchdog tick. Both told the reader to run `session enable`, and doing so from that same shell
rewrote the unit *without* the state root: from then on the launcher's `up --restore latest` read a
different archive from the one a `down` typed in a pane writes. The session comes back from the
layout instead of the shape that was saved, and nothing fails.

The state root is now treated exactly as the binary path already is, and for the reason `PinState`
gives: **the recorded value is the one the launcher actually uses, and this process's environment is
not the one that installed it.** `state_home_for_unit` reads the value out of the unit that keeps
the session up — found by behaviour, so a job installed under another name answers too — and falls
back to the environment only when nothing is recorded. `service_files` resolves it once, so writing
a unit and comparing against one see the same value.

Generation no longer reads the environment at all: `service_unit` takes the state root as an
argument. That also makes the generators pure, which is what lets the round trip be tested — a unit
regenerated from the value parsed back out of it is byte-identical, and the same unit regenerated
from an empty environment is not.

Moving the state root is now a `session disable` followed by an `enable` from a shell that exports
the new one. A plain `enable` keeps what is recorded, which is the safe direction: the failure this
replaces was an `enable` silently dropping it.

`zellij setup --generate-service` prints the same thing `enable` would install, so it reads the
recorded value too.

**nkmk.25, 2026-09-29: PATH and the launchd log paths are read back too.** The fix above covered
one caller-derived value and left two. `session doctor` and `session status` run over SSH on a Mac
reported the launch agent as "not what this config writes now". The same commands with the plist's
own PATH said `drift none`. The comparison rendered PATH from the asking shell, and
`StandardOutPath` and `StandardErrorPath` from its state directory. Following the report recorded
the SSH shell's thinner PATH for the next server.

Drift is now judged by `comparison_files`. It renders the unit from the config as before, but takes
PATH and the two log paths from the unit already on disk, where it records them. The recorded PATH
goes through the same hoist rule as a fresh one, so the same binary gives the same bytes and a new
binary still leads with its own directory. `ProgramArguments`, TERM, the scheduling keys and every
config extra are still rendered from the config, so a real change still reads as drift. `status`,
`doctor` and `up`'s warning all use it. systemd logs to the journal, so PATH is
the only value it gains.

Drift now names what differs. Doctor's note and the `status` line end in `ProgramArguments differs`
or `PATH and TERM differ`, and `only its comments differ` when no key does. The key compare is
line-based over the two formats this code writes: a systemd `Name=value` line, keyed by variable
for `Environment=`, and a plist `<key>` with the element lines under it.

An explicit `session enable` still records the calling shell's PATH. When it rewrites an existing
unit it now prints each changed key, and PATH as `PATH: 14 entries -> 6 entries, first: <dir>`. A
rewrite from a poor shell is visible when it happens. Cost: a new public `comparison_files`, a
`files` field in place of `paths` on `UnitDrift::Drifted`, and a `changed` field on
`EnableOutcome::Enabled`. All three are CLI-side; no contract or protobuf moves.

### `session disable` can finish over a half-install

`disable` unloads the job and then removes the files, in that order and for a good reason. What it
did not survive is a machine where only one of the two systemd files is there.

`systemctl --user disable --now <unit>` **fails** for a unit that does not exist — `Failed to
disable unit: … does not exist`, exit 1 — and `disable` returned on that failure before it removed
anything. A service written before the paired timer existed, an `enable` that died between its two
`fs::write` calls, or a file deleted by hand was enough: the surviving unit stayed installed and
enabled, and every retry did exactly the same. `session disable` could not reach a clean state
without an `rm` by hand, and nothing said which file to remove.

Two changes, and each is needed on its own.

The init system is now asked to let go of only the files that are **on disk**. A half-install is a
real state rather than an error, and the half that is missing needs no disabling. Nothing on disk at
all still sends the whole list, because a job can be loaded from a file somebody has since deleted —
and that job is precisely the one worth booting out.

Removal then proceeds **even when the unload did not**. A unit that refuses to disable for some
other reason, or a `daemon-reload` that fails, no longer stops the files being removed. What went
wrong is carried out with the outcome as `DisableOutcome::Disabled { unload_error, .. }` rather than
returned in place of it: the files are gone either way, and a caller told only "it failed" would
report a machine that is not the one in front of it. `session disable` prints the removals, then the
problem, and exits non-zero — the same shape it already used for a foreign job it left alone.

### Doctor stops reporting on a systemd it never reached

Two findings in the Linux doctor's start check, both of them the same fault: an answer that reads as
a fact this code never established.

**`Result=success` is what systemd says about a unit it has never heard of.** Verified on a Linux
box: `systemctl --user show zellij-session-nosuch-xyz.service --property=Result,ExecMainStatus,\
LoadState` prints `Result=success`, `ExecMainStatus=0`, `LoadState=not-found`, and exits 0. So on a
machine with nothing installed, doctor filed "the last run of the unit succeeded" under **Already
correct** — the one line a reader would take as proof the launcher works. `last_start_result` now
consults `LoadState` first and returns `StartResult::NotLoaded`, keeping systemd's own word for it
(`not-found`, `masked`), and the report says there is no run to judge. A caller that does not ask
for `LoadState` is unaffected.

**A `systemctl` that ran and failed was read as one that had nothing to report.** `Err` from the
commander means only that there is no `systemctl` to run; a `systemctl` that ran and exited 1 comes
back `Ok` with `success: false` and an empty stdout, and `shown.success` was never looked at. An
empty property map then read as `Unknown`, and doctor stated that the unit had never run. The
ordinary way to reach it is a context with no user bus — a bare SSH login, a container — which is
exactly where the timer check already reports "no answer from systemd". One report contradicted
itself about the same machine. The start check now reports the same thing, with systemd's stderr
under it.

### `session down` takes the up-lock it shares with the watchdog

`up` holds the up-lock across its check and its creation, and `restart` holds it across both halves.
A bare `down` took nothing, so the one command that removes a session raced the one thing that
puts it back.

Both orderings misreport. With the watchdog's tick inside `start_client` when the operator types
`down`, the teardown kills the server the tick has just created and the tick prints `post-condition
FAILED` with diagnostics about a machine that is fine. The other way round, `down` prints `removed`
and the tick's in-flight `up` puts the session straight back — the teardown silently did not stick,
and the exit code says it did.

`down` now takes `lock_up(name)` for the whole of its body, so the check and the removal are one
step against the same lock every other lifecycle verb uses. A `restart` calls `down` while already
holding it, which is why the lock's re-entrancy within a thread is load-bearing rather than a
convenience — a second `flock` there would wait ninety seconds for a hold this thread owns and then
proceed unlocked, which is the two-servers race the lock exists to close. That is now asserted.

### `session down` and `session restart` refuse while an agent is working

A teardown kills every pane, and an agent in one of them loses its turn. A release rollout
restarted live sessions on two Macs and cut a Claude session off mid-turn. So before either verb
kills anything, it asks the running session for its agents, the rows `list-agents --json`
returns, and refuses when one of them is working:

```
$ zellij session restart work
session restart: refusing - 1 agent(s) in 'work' are working:
  tab "develop"  sunny-otter  claude  done, but 2 background task(s) still working
Nothing was torn down. Wait for them to finish, or pass --force to restart anyway.
Exit status 4: a caller that started this in a pane that closes on exit must check it.
```

**What counts as working:**

| The agent's row | Busy? |
|---|---|
| `state` is `working` | yes |
| `state` is `blocked`, of any kind (`permission`, `question`, `auth`) | yes. The agent is mid-turn, waiting for the operator, and a teardown cuts that turn off |
| `state` is one this build does not know | yes. An unknown state fails closed |
| `state_working` above 0, under any state | yes. See below |
| `state` is `idle`, `done` or `error` | no. The turn is over |
| no `state` reported | no, unless the screen detector finds a gate or a question, which reads as `blocked` |
| the caller's own pane | never. An agent that restarts its own session has chosen to |

- **`--force`** goes ahead as before. `--yes` is not an override, and neither verb takes one.
- **Exit code 4.** 1 is an error, 2 is a refusal that changed nothing, and 3 is the strict-version
  refusal. A caller must be able to tell "wait for the agents" from all three.
- **Only the explicit verbs are guarded.** `session up`, the watchdog tick and the launchd and
  systemd units never tear a session down, so they are unchanged.
- **A session that does not answer goes ahead**, with a warning, after a second try half a second
  later. A wedged server is what a restart is for. A session that is not running is not asked.
- With `detect_agents false` there are no agent rows, so nothing refuses.

**Background subagents: `state_working`.** A Claude turn that ends while its background subagents
still run reports `done` (or `idle`). Claude Code already reports each running background task
(a subagent, a remote agent, a teammate, a workflow) over `OSC 7501` as a record of its own,
`state=working:id=<task id>`, and clears it when the task ends. The pane kept those records, but
its summary shows only the most urgent one, and `done` outranks `working`. So the summary now also
carries `state_working`: how many of the pane's records say `working`, the reported one included.
It is on `list-panes --json` and `list-agents --json`, and absent when zero. The table is
unchanged, and `action watch` does not compare it.

No hook was needed and none was added. The count is server-side and rides the existing JSON:
no new `OSC` key, no protobuf tag, no client/server contract. Against an older server the field
is absent and reads as 0. Two limits:

- Claude reports agents, not shells. A background Bash task has no record and is not counted.
- A new prompt (`OSC 133;A`) drops `working` records, and Claude resends only the records that
  changed. So a subagent still running when the next turn starts drops out of the count. The
  case this guard is for, a turn that ended with subagents running and nobody typing, is counted.

**A refusal in a pane that closes on exit.** A restart is usually launched inside the session:
`zellij --session S run --floating --close-on-exit -- zellij session restart`. `run` returns at
once, so its caller never sees the exit status, and the pane would close with the refusal in it.
Two things keep it visible:

- Run inside the session it names, with a terminal on stdin, the refusal waits for Enter. The
  floating pane stays on screen. A script, an agent's tool call and an SSH shell have no terminal
  there and never wait.
- `restart` writes the refusal to its log, `<state dir>/restart.log`, where a successful restart
  writes its output. A remote caller reads it after the `run`.

Nothing in this repo calls either verb; `session doctor` only prints the command. The operator's
macOS setup script calls both. Its `session restart` after a re-grant now fails while an agent
works, and the script already reports a failed restart. Its `session down` before a launch-agent
kickstart ignores the exit status, so a refusal leaves the old session running and the kickstart
finds it up. That script must check for exit 4, or pass `--force` when it means to.

The flag is new on two existing commands, and `state_working` is a new key. Neither is an
`Action`, a protobuf tag or a client/server message.

Capabilities: `session-agent-guard`, `program-status-working-count`.

### `session restart --when-idle`, a pre-flight, and whether a restart is needed

```
zellij session restart [NAME] --when-idle [--deadline 1h]
zellij session restart [NAME] --dry-run
```

Three additions that make an upgrade cheaper to take. None is an `Action`, a protobuf tag or a
client/server message.

**`--when-idle` waits instead of refusing.** The agent guard above exits 4 the moment an agent is
working, and a restart can stay owed for hours behind an agent that is never idle when somebody
looks. `--when-idle` asks the session for its agents every 5 seconds, with the same busy rule, and
restarts the first time none is working. It prints the working agents once, and again when they
change.

- **Deadline.** `--deadline` takes `30s`, `30m`, `2h` (the `prompt --timeout` syntax). Without it the
  wait is 1 hour. At the deadline it prints the guard's refusal, writes it to `restart.log`, and
  exits 4. Nothing was torn down.
- **The wait holds no lock.** It runs before the restart daemonizes and before it takes the up lock,
  so the watchdog's tick is never held up by it.
- **A missed answer.** A session that never answers goes ahead after a second try, as a plain
  restart does: a wedged server is what a restart is for. A session that stops answering after it
  reported a working agent is waited on, to the deadline. Many polls give many chances to miss one,
  and going ahead on a miss would cut off the agent just seen.
- `--when-idle` conflicts with `--force`, and `--deadline` requires `--when-idle`.

**The pre-flight.** Before anything is torn down, `restart` runs each build that may serve the
restarted session with `--version` and `setup --check` (with this command's `--config` and
`--config-dir`), 20 seconds each, its output read as it runs so no amount of it can stall the
build. A build that fails either, or hangs, refuses the restart with exit
2, the reason on stderr and in `restart.log`, and the old server left running:

```
session restart: refusing - the build that would serve 'work' failed its pre-flight:
  /usr/local/bin/zellij: `setup --check` failed: exit status: 1 (× Failed to parse Zellij configuration)
Nothing was torn down.
```

The builds checked are this binary, and the installed launcher's binary when it is a different file
and not the pin. With `pin_exe` the pin itself is not checked: `up` refreshes it from this binary
after the teardown, so this binary is what the pin will hold. A failure that appears only in the
copy (a signature the copy does not carry on macOS) is outside the pre-flight.

**Whether a restart is needed.** Most releases change the server; some change only the CLI, a
plugin or the MCP server, and a restart onto them cuts every pane for nothing. A release states it
in `SERVER_UNCHANGED` (`zellij-utils/src/session_lifecycle.rs`): the version it was declared for,
and the earlier versions whose server is the same. `session status` and `session restart --dry-run`
print a `restart` line from it and the running server's version (the one `ls --json` reports as
`server_version`):

| Running server | `restart` line |
|---|---|
| this build | `not needed - the session already runs this build` |
| version unknown | `needed` |
| a version in `same_server_as` | `not needed` |
| any other version | `needed` |
| `declared_for` is not this build's version | `needed`: the statement belongs to another release |

So a release that forgets the const fails closed, and a unit test fails on the version bump until
`declared_for` names the new version. The release that cuts a version sets `declared_for` to it;
it lists earlier versions only when nothing a server runs has changed since them, and it empties
the list when the server changed. Versions are compared by equality, never by order.

**`--dry-run`** changes nothing: no daemonize, no lock, no pin refresh. It prints the `restart`
line, the agents a restart would refuse on, and each build's pre-flight. Exit 0 when a restart would
go ahead, 4 when an agent would stop it, 2 when a build fails its pre-flight.

Capability: `session-restart-when-idle`.

### A pin the launcher does not run is inert everywhere, not only in the message

`session up` reports a pin the installed launcher does not run as inert, in as many words: *"Nothing
was copied: what `session up` keeps current is the binary the launcher actually runs."* That is
`PinState::Mismatch`, and the ordinary way to reach it is turning `pin_exe` on after
`session enable`, so the unit still names `/opt/homebrew/bin/zellij`.

`server_exe_for_interactive_launch` never asked. It took the CONFIGURED pin and called
`install_pinned_exe` unconditionally — so one `zellij session up work` printed that warning and then,
in the same process, copied 40 MB to the pin and served the session from it. The operator was told
the pin was doing nothing while it was the only thing running, and `session up` and `session status`
described the same machine differently.

It now asks. The function takes the session name, reads what the installed launcher runs the way
`pin_state` already does, and returns `None` on a mismatch — the server then starts from the binary
the user typed, which is what the warning describes. A launcher that runs the pin, and a machine
with no launcher at all, are both unchanged: the pin is still the right path there, and a session
with no name yet has no launcher to disagree with.

The name comes from the invocation, in the two spellings that carry one: `opts.session`, and the
`Attach` command `session up` builds — it clears `opts.session` and puts the name there, which is
exactly the path the finding was about.

### `session enable` makes the launchd log directory even when it changes nothing else

launchd opens the paths a plist names and runs neither the job nor a directory-creating step when it
cannot, so `enable` has always created the log directory. It created it after the already-enabled
short-circuit, which is the one path where it matters most.

The plist can be byte-identical and the job still held while the directory has gone — a cleanup
script, a relocated state root, a volume that did not mount. `zellij session enable work` printed
"already enabled" and created nothing; at the next login the job did not run, and the one command
the reader is told to run had told them everything was fine.

The `create_dir_all` now happens before that return, in one place (`ensure_launchd_log_dir`) rather
than inline at the end. It is idempotent, so the ordinary `enable` pays a `stat` for it.
`EnableOutcome::AlreadyEnabled` therefore no longer means "touched nothing" in the strictest sense,
and its doc says so.

### The pane probe reports what the client said instead of charging five seconds for it

The macOS doctor opens a floating pane that writes two answers to a file, and waits up to five
seconds for them. The wait was the only thing it did: the client's stderr went to `/dev/null`, and
its exit code was asked for once, in `reap`, after the deadline had already passed.

So a client that failed **at once** — the wrong socket directory, which is the very fault
`check_tmpdir` reports two lines above, or a refused `run`, or a permission error — cost the full
five seconds and came back as `Needs you: probe  the pane probe did not answer in time`, with the
reason already discarded. It was a Needs-you hiding a real error, and indistinguishable from the
wedged server the deadline exists for.

The loop now asks `try_wait()` on every pass and keeps stderr. The distinction that makes this work
is that a client which **succeeds** is not an answer: `zellij run` returns as soon as the pane is
created, long before its shell has written anything, so a zero exit is a reason to keep waiting and
only a non-zero one ends the wait. `run_pane_probe` returns a `ProbeFailure` rather than `None` —
timed out, the client failed with its status and up to six lines of its own stderr, or it could not
be started at all — and the report says which.

Verified by compiling it: the module is `#[cfg(target_os = "macos")]` and this fork's Linux box
cannot cross-compile the tree (a vendored C dependency), so it was type-checked by ungating the
module and its two macOS-only imports for one `cargo check` and then putting the gates back. The
behaviour itself is still worth a run on a Mac.

### The process scan asks `ps` not to truncate

`running_servers` ran `ps -eo pid=,command=`. BSD `ps` — which is macOS's — sizes its output from
`COLUMNS` or a `TIOCGWINSZ` and cuts every line to it; only `-ww` makes the width unlimited.

A server's argv is `/opt/homebrew/bin/zellij --server /var/folders/xx/…/T/zellij-501/
zellij-<contract>/<id>`, well past eighty columns, and the socket path is the **last** field. A
cut line therefore does not merely lose detail: `parse_server_processes` reads a truncated socket,
`servers_for_session` finds nothing for a healthy session, `session status` prints `running no`, and
the guard that refuses to create a second server for a name stops guarding.

`-ww` is accepted by procps and by BSD `ps` alike. The argv is now a named constant that the test
runs, so a platform whose `ps` rejects it fails the suite rather than the field.

Whether Apple's `ps` truncates for a piped, non-tty run was never established — Rust's
`Command::output()` pipes both streams, and which branch Apple's `ps` takes there is unverified.
The flag costs nothing either way, so this is a fix made without waiting for the measurement.

### The systemd generator quotes what it writes

`ExecStart=` and `Environment=` are both split into space-separated words, and the generator quoted
neither. One space in a path broke both at once.

With `pin_exe "/home/u/My Tools/zellij"` — or a `--exe` with a space, or a `$HOME` with one —
`ExecStart=/home/u/My Tools/zellij session up work` execs `/home/u/My`, which is the visible half.
The other half is worse because it does not fail: `Environment=PATH=/home/u/My Tools:/usr/local/bin:…`
is read as a list of `NAME=VALUE` words, so systemd sets `PATH=/home/u/My`, logs *"Ignoring invalid
environment assignment"* for the rest, and the session comes up with a one-entry PATH that does not
exist. Every layout `command`, `zellij run --` and `copy_command` then fails with "command not
found" beside an interactive pane that works — the exact symptom the PATH default was added to end.

Both are now quoted, and unconditionally rather than only when a space is present, so the bytes do
not depend on what a path happens to contain. `\` and `"` are escaped, because inside systemd's
double quotes those are the two characters that mean something. The reader side already coped:
`word_spans` takes quotes off, so `installed_session_exe` and the scan for a launcher under another
name answer the same as before — asserted.

Proved against real systemd rather than argued. `systemd-analyze verify` on a generated unit with a
spaced exe passes clean; the same unit with the quotes stripped produces
`Invalid environment assignment, ignoring: tools:/usr/local/bin:…` and
`Command /tmp/…/my is not executable`.

**Every installed unit's bytes change**, so the first `session status` or `session up` after
upgrading reports drift and asks for a `session enable`. That is the intended path and it is a
one-time cost.

### A plugin that failed says so afterwards

A command pane that fails is marked `error:exit 7` and stays marked. A plugin pane that fails was
not marked at all. It printed `ERROR IN PLUGIN` into its own body and flashed its frame red for a
second, and neither of those is a mark: the body text goes the moment anything is drawn over it,
the flash is gone before anyone reading a session back could see it, and nothing outside the server
could read either. A session with a broken plugin looked, to `list-panes`, exactly like a session
without one.

It gets the same note the failed command pane gets — `error:plugin failed`, on the frame and in
`list-panes`, `list-tree` and `PaneInfo`. The wording says the kind of failure rather than the
error itself, because the loader's error is a formatted chain several lines long and a note is one
short line; the pane's body still carries the whole of it.

- **A runtime panic is marked too**, not only a load that never started. A crash is reported
  through the same path a failed load is, so a plugin that came up and later died carries the mark
  as well.
- **The mark belongs to the attempt that ended.** It is cleared when the next load attempt starts,
  which is where a re-run command pane drops its `exit 7`, and not when one succeeds — so a reload
  that fails again re-marks the pane a moment later, and a reload that hangs leaves no stale claim
  of failure standing.

The frame's red flash (`AddRedPaneFrameColorOverride`) was the obvious hook and is the wrong one
twice over: it is a one-second animation, and it is the same field the multi-select highlight uses,
so a durable mark stored there would be wiped by unrelated UI and suppressed for as long as the
pane sits in a selection group.

### A plugin reloads in a session nobody is attached to

```
$ zellij -s build action start-or-reload-plugin file:/tmp/p.wasm   # no client attached
```

`reload_plugin_with_id` asked for a connected client and gave up when there was none —
`No connected clients, cannot reload plugin`. So the reload worked from inside a session and not
from outside one, which is backwards: a detached session is exactly where a plugin is edited and
reloaded without a terminal in the way, and every other `zellij action` verb that does not need a
focus reaches one.

Nothing about the reload needs a client to be *attached*. The id is the key the plugin's instance
sits under in the plugin map, nothing more, and the instance already has one — whoever loaded the
plugin, still recorded there after everybody detached. So a connected client is preferred, as
everywhere else that picks one, and the map answers when there is none.

Asking the map rather than inventing an id is the whole of the correctness argument: a load inserts
at `(plugin_id, client_id)`, so a fresh id would have left a second instance beside the one it was
meant to replace.

- A plugin that is not in the map has no id to fall back to and still declines, which is right —
  there is nothing there to reload.
- Nothing else changes for an attached session: the connected client is still preferred, and the
  clone-for-other-clients pass sees the same empty list it always saw when nobody is attached.

Proved by running it both ways on one tree: with the fallback, `start-or-reload-plugin` against a
session with zero clients reloads the plugin; with the fallback alone removed, the same command on
the same session never reaches the loader.

### A background plugin hears the session with nobody attached

A plugin loaded without a pane subscribes to `PaneUpdate` and `TabUpdate` like any other. Examples
are a `load_plugins` entry and the background slot of a shared instance, such as the `zellij:bars`
slot in upstream's default config. Upstream addresses these updates per attached client. So in a
detached session a background plugin hears nothing, and a subscription goes when its client
detaches.

The fork serves each background subscription once per change, whether or not a client is attached:

- **`PaneUpdate`** carries the session's pane manifest.
- **`TabUpdate` for an attached client** is exactly the payload that client's own tab plugins hear:
  its tab names, its one active tab, its bell flags. A shared instance draws that client's tab bar
  from what its background slot hears, so the two must agree.
- **`TabUpdate` for a client that is not attached** is the screen-wide tab state. A tab is `active`
  when any client is on it. The addressed client is never one of the `other_focused_clients`.
- **A subscription outlives its client's detach.** It ends when the plugin unloads.
- **A plugin whose pane lives in a tab** is served as a tab plugin, never also as a background one.
- **A new subscription** gets the current state, and only that plugin hears the report.

`generate_and_report_tab_state`, `generate_and_report_pane_state`,
`background_plugin_ids_subscribed_to` and `keep_only_state_report_target` in
`zellij-server/src/screen.rs`. Tests: the `background_plugin_*` tests,
`background_tab_update_for_an_attached_client_is_that_clients_own_view` and
`tab_update_never_lists_its_own_client_as_another_focused_client` in `screen_tests.rs`.

### The restart log stopped eating the front of its own warnings

`session restart` daemonizes and points both of the detached process's streams at
`restart.log`. It opened the file twice: stdout through `File::create` and stderr with
`append(true)`. Only one of those follows the end of the file. A `File::create` descriptor carries
an offset of its own, starting at zero, so the two cursors diverged the moment the streams were
used in anything but strict alternation — stderr's line landed at EOF, and stdout's next write went
down at its own lower offset, on top of the front of it.

Deterministic, cosmetic, and it ate exactly the part of a warning that said what the warning was
about: `warning: the pin was NOT refreshed: Security: SecKeychainUnlock…` reached the log as
`ecurity: SecKeychainUnlock…`, which reads as noise rather than as a pin that did not refresh.

Both handles are append-mode now, so each write seeks to the end as part of the write and the two
streams interleave by line. Truncating for the new generation is its own step rather than a side
effect of `create`. `open_restart_log` exists so this is a testable function rather than four lines
inside a `-> !`: the regression test writes the fleet's own warning on stderr and a shorter line on
stdout, and fails against the old shape with the fleet's own truncated output.

### The title template and the snapshot settings follow a live reload

`terminal_title_template`, `session_aliases` and the `snapshot_*` settings were read once, when the
first client connected, and never again. Editing any of them and reloading — the thing every other
option here responds to — did nothing until the session was recreated, which is the one action a
person adjusting snapshot retention is least likely to want.

Both are process globals, and for a reason: a pane reads the title format while it renders, and the
snapshot settings are read on the way out, after the session data has been dropped. Neither has a
per-client home to travel in, which is why they never rode `ScreenInstruction::Reconfigure` with
`bell_clear_delay_ms` and the rest. The fault was the container rather than the placement — a
`OnceLock`, whose setter silently drops every write after the first.

Both are `RwLock`s now, and both are rewritten from `propagate_configuration_changes`, which is the
single point all three reload paths funnel through. They are taken from the first config of the
change set and are therefore the same for every client, which is exactly what they already were.

### A stable name on PATH counts under any spelling

`resolve_service_exe` writes the steadiest path it can find into a generated unit, and a name on
PATH beats the versioned file it points at because the versioned one is what the next upgrade
deletes. It looked for that name by probing `<dir>/<the running binary's own file name>`, which is
right for as long as the build being installed is the one `zellij` on PATH leads to.

Two builds on one machine is the case that breaks. With a stock `zellij` linked, `<dir>/zellij` is
somebody else's file, the probe fails, and the unit is written with this build's versioned path —
while this build's own stable name sat on the same PATH the whole time under another spelling. The
warning fired, so it was never silent; it was just wrong about there being nothing better.

A failed same-name probe is now followed by a scan of the same directories for any entry that
resolves to this binary, in PATH order and then alphabetically so the generated unit is
byte-identical from one run to the next. The scan is only paid for in the case that used to give
the wrong answer, and a directory holding nothing that leads here still ends at the honest
`Resolved` that warns.

### A close verb resolves its target before it asks

`close-pane --pane-id terminal_99` on a terminal asked whether to close the pane, waited for the
answer, and only then reported that no pane answers to `terminal_99`. The prompt is put by the
client and the pane list belongs to the server, so the question came first and the answer to it came
last.

The target is now resolved against the session before the prompt, and a miss exits 2 without asking
anything. An id form is looked up like a handle or a uuid: it needs nothing from the server to be
*understood*, which is a different question from whether a pane answers to it — the same distinction
`wait` already makes for the same reason.

`close-tab` is left as it was. Its `--tab-id` is a stable id rather than a name to resolve, and
answering it means a different query against a different list, for no change in what the prompt is
about.

### `--version` says which upstream base it was cut from

The version string names the fork counter and nothing else, so `0.45.0-nkmk.16` answers *which
fork build* and leaves *which upstream* to whoever remembers. That is the question asked when a
bug might be upstream's, and the answer used to live only here, in prose.

```
$ zellij --version
zellij 0.45.0-nkmk.16
upstream v0.45.0 @ 13e1c25a2
```

**The base is two literal consts, not a `git describe`.** `UPSTREAM_BASE_TAG` and
`UPSTREAM_BASE_COMMIT` sit beside `VERSION` in `consts.rs`. Deriving them at build time would make
the answer depend on the checkout — a tarball, a shallow clone and a full clone would each report
something different, and none of them would be wrong in a way the build could detect. A literal is
the same answer everywhere, at the price of one line in the rebase runbook.

**It is a second line, and that is the whole safety argument.** `zellij-mac-setup` reads the
version as the last whitespace-separated field of `zellij --version | head -1` and feeds it to a
numeric comparison. A parenthetical appended to the first line would be read as the version number,
the comparison would decide the script predates the binary, and the script would refuse to run on
every Mac. So the first line stays byte-identical to what it has always been, and everything new
goes below it, where a `head -1` reader never sees it. A test asserts exactly that, naming the
script.

Nothing else that reads a version changes. `VersionInfo::from_version_string` is fed the `VERSION`
const, not the printed string, so `setup --check --json`, the capabilities document and `ls --json`
are untouched; the tap formulas match a substring; `session doctor` compares build identities and
has never parsed `--version`. `setup --check` gains an `[Upstream base]:` line of its own and
leaves `[Version]:` alone.

**The build date is opt-in.** `option_env!("ZELLIJ_BUILD_DATE")`, unset in an ordinary
`cargo build`, so a local rebuild stays byte-reproducible and the line reads `upstream v0.45.0 @
13e1c25a2`. The release workflow exports the day it builds, so a shipped artifact reads `…,
built 2026-08-21`. A `build.rs` was the alternative and is worse: `zellij-utils` has none, it
compiles for wasm, and stamping a date there would make the checked-in plugin assets differ on
every rebuild.

### The launchd `env` block: an environment variable a launch agent can actually read

The systemd side has always been able to say this. An `Environment=` line is a directive like any
other, so `session_service { systemd { service "Environment=SSH_AUTH_SOCK=…" } }` works and is
documented above. The launchd side could not, and the failure was silent: `launchd { keys { … } }`
writes TOP-LEVEL plist keys, launchd has no top-level key named after an environment variable, and
so `keys { SSH_AUTH_SOCK "…" }` parsed, generated, installed, loaded, and did nothing at all.

```kdl
session_service {
    launchd {
        env {
            SSH_AUTH_SOCK "/private/tmp/com.apple.launchd.XXXX/Listeners"
            HOMEBREW_PREFIX "/opt/homebrew"
        }
    }
}
```

Entries go into the plist's `EnvironmentVariables` dictionary, in the order the config gives them,
after the three the generator writes itself.

**A separate block rather than a rule inside `keys`.** Guessing which of the extras is an
environment variable is not a guess the generator can make: `ProcessType` and `PATH` look alike, the
only difference is what launchd does with them, and being wrong in either direction writes a plist
that is accepted and ignored. So the config states which it means, and the two blocks sit beside
each other under `launchd`.

**Values are strings, not `PlistValue`s.** That dictionary holds strings; an array there is not a
shape launchd understands, so the type refuses it at the parser instead of letting it reach a file.

**TERM, PATH and XDG_STATE_HOME resolve in three steps**: the `env` block, then a name put through
the `keys` hatch, then the generator's built-in default. `env` wins because it names the dictionary
launchd actually reads, which is the more specific of the two statements.
Whichever half loses is still TAKEN OUT of the extras, so the variable is written exactly once and
never also as a top-level key. A repeated name inside one `env` block is the later one, for the same
reason: a dictionary with one key twice is not a plist, and a config file is read top to bottom.

`TMPDIR` and `ZELLIJ_SOCKET_DIR` are refused here as they are in `keys`. A new route to the two
names the whole design forbids would be the one way this patch could do real harm.

Unset, the generated plist is byte-identical to what it was — asserted by comparing the output
against a default block. `session status` lists the entries as `env NAME = value`, prefixed because
they are not plist keys and a listing that showed them the same way would say they were.

Nested key, so the rule of the day applied with no exceptions: the `launchd` block parses its own
children, and an older build fails the **whole** config on `env`, not just the block. It could not
be seeded into a shared config ahead of the binary; it shipped with the code that accepts it, and
every machine took the binary first. This is the second of the two keys that bought [the entry that
softened that rule](#an-unknown-key-in-session_service-warns-instead-of-failing-the-config).

### The checked-in plugin assets no longer name the machine that built them

The fifteen `.wasm` files under `zellij-utils/assets/plugins/` are committed, and rustc bakes an
absolute path into each one for every `panic!` location and every `include!`d file. So the assets
published whoever last built them. Every file in the tracked tree carried the builder's home
directory, their cargo registry, and the directory they keep the checkout in.

`cargo xtask build --plugins-only --release` now passes three `--remap-path-prefix` flags to the
plugin build, derived from the environment rather than written down:

| From | To | What it covers |
|---|---|---|
| `$HOME` | `~` | anything else under the home directory |
| `$CARGO_HOME`, else `$HOME/.cargo` | `/cargo` | the registry sources, which are most of them |
| the project root | `/zellij` | the generated prost files, `include!`d by absolute path |

Afterwards the only absolute paths left are rustc's own `/rustc/<hash>/` and `/rust/`, which upstream
already remaps for the same reason. Workspace sources were never affected: cargo runs from the
workspace root and records those relative.

**Order is the part that is easy to get wrong.** rustc applies the LAST prefix that matches, not the
first, so the list runs generic to specific. Reversed, `$HOME` would swallow the other two and the
registry would read `~/.cargo/registry/…`.

**Scoped to the wasm target, through `--config`, not `RUSTFLAGS`.** A bare `RUSTFLAGS` replaces the
config-file flags for every target, which on a machine that configures a linker for its native
triple silently drops it. `--config target.wasm32-wasip1.rustflags=[…]` touches one triple, and
cargo APPENDS it to whatever a config file already set there rather than replacing it — which is
also why the appended-last flags win the precedence rule above. The debug plugin build takes the
same flags, so the two paths cannot drift.

Nothing about this is machine-specific, so CI runs it too and the runner's paths get remapped as
well. A rebuild is byte-identical: recompiling all fifteen plugins from touched sources reproduces
the committed assets exactly.

### The watchdog ticks every fifteen seconds

`CHECK_INTERVAL_SECS` drops from 60 to 15, so a session that dies comes back within fifteen
seconds instead of a minute, on both platforms at once — the systemd timer's `OnBootSec=`/
`OnUnitActiveSec=` and the plist's `StartInterval` all read the same constant.

Measured before changing: a pass over a healthy session costs 0.16–0.17 s, so at fifteen seconds
the watchdog spends about one percent of its interval working, and the up-lock it takes is held
for that sliver. `launchd { keys { StartInterval N } }` still retimes macOS; Linux has no override.

A machine that installed its units under the old default shows the timer as drifted in
`session status` until `session enable` is re-run there. That is the drift check doing its job:
the installed file says 60, the generator now says 15.

### The tab bar ignores the wheel

`slim-tab-bar` used to move one tab per wheel notch, so a trackpad flick over the bar walked the
tab strip. The scroll arms are gone: switching tabs is deliberate only — a click on the tab, or a
keybind. The stock `tab-bar` and `compact-bar` keep their upstream behaviour; only the fork's own
bar changed.

### An RC binary reports the RC

A release candidate is tagged `v0.45.0-nkmk.17-rc.1` from a branch whose `Cargo.toml` reads
`0.45.0-nkmk.17`, because the branch is bumped to the version it is heading for. The suffix
therefore lived on the tag and nowhere else, and the artifact it produced said `0.45.0-nkmk.17` —
the same string the final release would say. Two checks were written around that rather than against
it: the tap's install verification cut `-rc.N` off before comparing, and the rc formula's `test`
block did the same. Between them, a Mac on `zellij-nkmk-rc` was running unproven code that answered
`zellij --version` with the name of a release that did not exist yet, and nothing anywhere could
tell the two apart.

`release.yml` now patches the version to the whole tag before it builds a candidate. It is the same
edit a `chore: bump version` commit makes — the workspace version and the path-dependency pins in
`Cargo.toml` and `zellij-integration-tests/Cargo.toml`, plus the seven package stanzas in
`Cargo.lock`, which has to move too because the build is `--locked`. `sed -i` is spelled differently
on the two runners, so it writes through a temp file instead.

**It refuses a tag that is not a candidate for the version in the checkout.** `v…-nkmk.17-rc.1` on a
branch still reading `nkmk.16` is a tag cut before the bump, and rewriting every pin to a version no
crate claims is the wrong way to find that out.

A candidate is the only build that patches its own checkout. A final tag builds what main says,
untouched, which is why nothing here needs a matching un-patch.

Two consequences worth knowing. `VersionInfo::from_version_string` split the fork counter on the
last `.`, so `nkmk.17-rc.1` read as fork `nkmk.17-rc`, counter `1`; it now takes a trailing
`-rc.<n>` off first, and a candidate reports the counter it is a candidate **for** while `version`
keeps the candidate number. And `ZELLIJ_PLUGIN_ARTIFACT_DIR` is keyed by the version string, so a
candidate extracts its plugins into its own directory rather than into the final release's —
correct, and the reason a candidate never leaves an artifact behind that the release would reuse.

The tap changes with it: the install check and the rc formula's test block assert the full rc
version instead of stripping it. They are in `noahkiss/homebrew-tap`, not here.

### `input_while_scrolled`: typing into a scrolled pane no longer eats the key

Upstream 0.45 puts the client into Scroll mode whenever the focused pane is scrolled — when you
scroll it, and again when you focus back onto one you left scrolled. The mode then decides what a
key means, and `Keybinds::default_action_for_mode` returns `Action::Write` only for `Locked` and
for the client's own default mode. So a printable key typed into a scrolled pane is not sent
somewhere wrong; it is **discarded**. `e` and `s` are worse than discarded — the default scroll
keymap binds them to `EditScrollback` and `EnterSearch`. Read a log, scroll up, start typing the
next command: the first few characters vanish and the fourth opens an editor.

```kdl
input_while_scrolled "scroll-mode"
```

Three values, and the default is the upstream behaviour, so an unset config is 0.45 exactly:

| Value | Implicit mode switch | A key typed into a scrolled pane |
|---|---|---|
| `scroll-mode` (default) | yes, as upstream | resolved in Scroll mode; an unbound key is discarded |
| `jump` | no | clears the pane's scroll, flushes the held output, then reaches the pty |
| `stay` | no | reaches the pty; the viewport does not move |

`jump` is the 0.44 feel. Upstream `c428ae93b` deleted the branch of `change_mode` that cleared the
active pane's scroll on the way out of Scroll mode, which is what used to snap the viewport back;
this puts the snap on the keypress instead of on the mode change, where it is the same gesture a
terminal user already expects. `stay` is for reading while a pane is busy: send the key, keep
looking at what you were looking at.

**The option governs only the *implicit* sync — both halves of it.** An explicit `SwitchToMode
"Scroll"` behaves identically under all three values, because it goes through `change_mode` and
nothing here touches that function. Under `jump` and `stay` the implicit *exit* is off too: a
client who chose Scroll deliberately is not dropped out of it by scrolling back to the bottom.
That is the point of turning the automation off, not an oversight.

The guard is one early return behind a named predicate (`implicit_scroll_mode_sync_enabled`), in
`sync_scroll_mode_on_focus` — the function that holds upstream's derive-the-mode-from-`is_scrolled`
logic. Every call site reaches that function: the focus verbs directly, the scroll verbs through
`sync_scroll_mode_if_scroll_changed`, and the pane-id-targeted ones through
`sync_scroll_mode_for_pane_id`. So one guard disables the lot and no call site is edited. `jump`'s other half is a single branch in `Tab::write_to_pane_id`, inside
the `AdjustedInput::WriteBytesToTerminal` arm and nowhere else: only that arm is a byte actually
reaching the pty, and re-run, close and drop-to-shell must not move the viewport. It flushes
before the write, because the held output belongs above the echo of the key being sent.

**`stay` inherits upstream's wart, and does not fix it.** Output arriving at a scrolled pane is
held in `pending_vte_events` rather than rendered, and the queue is capped at
`MAX_PENDING_VTE_EVENTS` — 7000 pty reads, not 7000 bytes. Reach the cap and the pane clears its
own scroll and drains, yanking the viewport to the bottom. That is the design that came with
upstream's buffered-output hold (#4850) and every value here lives with it; under `stay` a chatty
pane left scrolled will eventually jump on its own. Scroll down, or clear the scroll, to flush on
purpose.

**Rollout is free.** It is a TOP-LEVEL key, so a binary that predates it ignores it rather than
failing the config — unlike a key nested in a block that parses its own children. The config can
be seeded on every machine before any of them takes the binary, in any order.

It never crosses the wire. The field is `#[clap(skip)]`, so it stays out of `CliAssets`; no
`message Options` field is added, and `protobuf_conversion.rs` reads it as `None` in the
proto→Options direction like every other fork option. The server loads the config itself, and the
value reaches tabs as an `Rc<RefCell<_>>` shared with `Screen`, the way `default_floating_size`
does, so a config reload applies to tabs opened before it with no per-tab update chain.

### A write that names a pane leaves every viewport alone

`zellij action write --pane-id`, and the `write-chars`, `send-keys` and `paste` forms of the same
thing, used to drop somebody else's scrollback to the bottom. Scroll up in the pane you are reading,
let a script write into a pane in another tab, and your viewport jumped — and under the default
`input_while_scrolled "scroll-mode"` you were dropped out of Scroll mode with it. Upstream
[#5476](https://github.com/zellij-org/zellij/issues/5476), no maintainer reply.

The cause is one instruction sent too widely. Every writing verb in `route_action` sent a
`ScreenInstruction::ClearScroll(client_id)` ahead of the write, and `ClearScroll` resolves "the
client" through `active_tab_and_connected_client_id!`, which falls back to whichever client the
server can find. A CLI caller is not one of them, so the clear landed on a bystander.

- **The clear is asked once now, behind `clears_the_asking_clients_scroll`**, instead of by each of
  the five arms that wrote it out. A verb that names a pane answers `false`.
- **The untargeted half is unchanged.** `write`, `write-chars` and a `paste` with no `--pane-id`
  still clear, because there the pane being written to *is* the caller's own — typing scrolls you
  back down to see what you typed, which is the gesture a terminal user expects.
- **The written-to pane still answers for its own scroll**, in `Tab::write_to_pane_id`, under
  [`input_while_scrolled`](#input_while_scrolled-typing-into-a-scrolled-pane-no-longer-eats-the-key).
  That option is the one place that decides what an incoming byte does to the target's viewport, and
  it did not need to change.

Nothing crosses a contract: no `Action` is added, no message is renumbered, and `ClearScroll` is a
`ScreenInstruction`, which is server-internal Rust.

### Closing a tab with more than one pane asks first

The tab-mode close key and `zellij action close-tab` used to be the same button. One is pressed by
somebody who meant to close a *pane* and reached one menu too far; the other is typed on purpose.
Now only the key is gated: a fork action, `CloseTabOrConfirm`, and a bundled plugin,
`zellij:confirm-close-tab`, that asks.

The default binding is no longer `CloseTab`, in both presets (`default` and `unlock-first`; the
latter leaves to `"Locked"`, where every key reaches the prompt too):

```
bind "x" { CloseTabOrConfirm; SwitchToMode "Normal"; }
```

- **A tab with one pane closes with no prompt at all, and no pane.** The server decides, for the
  client that pressed the key, before anything is drawn (`zellij-server/src/close_tab_or_confirm.rs`).
  It counts the selectable panes in that client's tab, both layers, hidden floating panes
  included: the same counts the plugin reads from `TabInfo`, and the same rule as its
  `TabShape::decide`. One at most closes the tab as `CloseTab` does, at once on the same step
  rather than through the screen queue, where a tab switch already queued for that client would
  run first and leave the close to hit the tab it switched to. More launches the plugin,
  floating and moved to that client's tab, as the old binding did. Another client's tab is never
  consulted, and a client with no tab of its own changes nothing.
- **Why the server and not the plugin.** The binding used to launch the plugin every time, and the
  plugin closed a worthless tab from `load`. Its floating pane was on screen for a frame or two
  first: a visible flash on every close of a one-pane tab. A plugin cannot decide before it has a
  pane, and a keybinding that messages an already-running plugin reaches every client's instance
  with no word of which client pressed it, so the decision moved to the one place that knows the
  presser.
- **A tab with more than one asks**, in a small floating pane: `close tab? 3 panes open  [y/N/p]`.
  `y` closes the tab. `n`, `Esc` and `Enter` cancel. `p` closes only the pane you were in and
  leaves the tab, which is what the miss-press usually meant.
- **`SwitchToMode "Normal"` is part of the binding, not decoration.** `y`, `n`, `p`, `Esc` and
  `Enter` are all unbound in the default Normal mode, so they reach the prompt as key events. Left
  in tab mode, `n` would open a new tab.

**The CLI is exempt structurally, not by a check.** `zellij action close-tab` confirms client-side
in `confirm_or_exit`, before the action is ever sent, and it never launches a plugin. A keybind
gate and the CLI gate cannot collide, and `--yes` still means what it meant. This is the doctrine
in "Anything that cannot be undone asks first" seen from the other end: a key is pressed by a
person who is looking at the thing it acts on, so the *key* is where a look-again belongs.

**`p` closes the previously focused tiled pane.** The tiled and floating layers keep separate
focus, so the pane you were in is still marked focused in the pane manifest while the prompt holds
the floating layer's focus — which is exactly why a previously focused *floating* pane cannot be
recovered. If you were in a floating pane, `p` falls back to the focused tiled pane. The prompt
offers `[y/N]` instead of `[y/N/p]` when there is no pane it can name. The manifest is the only
source for this: by the time the plugin runs, `get_focused_pane_info` already names the prompt's
own pane.

**One case it cannot put back: a tab whose floating panes were hidden.** Opening a floating pane
unhides the tab's whole floating layer, and by the time the plugin runs, that has already
happened — `are_floating_panes_visible` reads `true` at its `load`, whatever it was a moment
earlier, and nothing exposes the value from before. The common case needs no help: zellij hides
the layer again by itself when the last floating pane in a tab closes, so a tab that had no
floating panes is left exactly as it was found. A tab that had hidden ones keeps them on screen
after a cancel, until the floating toggle is pressed. Detecting this would take a server-side
change, not a plugin one.

**With two clients in one tab, `p` may pick the other client's pane.** `PaneInfo.is_focused` is
not per-client, so "the focused tiled pane" is whoever focused it. The count and the tab are right
— `TabUpdate` is per client — and `p` becomes exact if per-client focus lands on `PaneInfo`. `y`
and cancel are unaffected.

**Only the instance whose client pressed the key may act.** Zellij loads a plugin once per
connected client and runs `load` in every one of them, each seeing its own client's focus
(`clone_instance_for_other_clients` in `zellij-server/src/plugins/plugin_loader.rs`; the plugin map
is keyed by plugin id *and* client id). An instance that does not find its client focused on the
prompt's own pane is inert for the rest of its life: it closes nothing and renders nothing.

Without that guard one keypress closed one tab per attached client. The instances run in turn and
`close_focused_tab` blocks until the server confirms, so the second instance woke to a session that
had already lost the first tab, found its client focused on the *next* tab, decided that tab was
worth nothing either, and closed it too. With two tabs open and two clients that emptied the
session and took the server down with it. The cost of the guard is small and visible only to a
second client: it sees an empty floating pane until the client that asked answers the prompt.

**The plugin keeps its own rule.** A config that still binds `x` to `LaunchOrFocusPlugin
"zellij:confirm-close-tab"` works as before, flash included: the plugin closes a worthless tab
from `load`.

**Rollout order is the reverse of the usual one.** The binding lives in the binary's own presets
(`zellij-utils/assets/keybinds/`), so nothing has to be rolled out at all: an older binary keeps
its own binding and a newer one gets `CloseTabOrConfirm`. A user `config.kdl` that already
overrides tab-mode `x` keeps its own binding and sees no change until it is rewritten — and it must
not name `CloseTabOrConfirm` ahead of the binary: a build without the action rejects the whole
config, and a tolerant one warns and skips the action.

**What it costs.** One client/server `Action` tag (`close_tab_or_confirm`, 907) and one plugin
`ActionName` (`CloseTabOrConfirm`, 901), so a hint plugin sees the key: both in the fork block. A
plugin built before this drops the unknown action from its keybinding table, so its close hint has
no key; the checked-in plugins are rebuilt with it.

### The tab-mode close hint follows the key that launches the prompt

`x  Close` is back on the tab-mode hint line. It dropped out when `x` was rebound to
[the confirmation prompt](#closing-a-tab-with-more-than-one-pane-asks-first): the status bar
resolves a hint by matching the ACTION LIST a key is bound to — `[CloseTab, SwitchToMode "Normal"]`
— and the binding is now a `LaunchOrFocusPlugin`, which matches no pattern there at all. A bar that
stops naming a key the mode still has is worse than one that names nothing, because the key works.

One lookup answers it, in `default-plugins/bars/src/status_bar/mod.rs` and called from both hint tables
(`second_line.rs` and `one_line_ui.rs`): the first key in the mode bound to exactly two actions,
where the first is `CloseTabOrConfirm` or launches `zellij:confirm-close-tab` and the second leaves
the mode. It runs only as
a **fallback**, when the direct `CloseTab` lookup found nothing — a config that still binds
`CloseTab` is answered exactly as before, and no hint ever lists two keys for one verb.

**The matcher names the plugin, and that is the point.** `action_key` compares actions with
`shallow_eq`, which looks at the variant and not at its fields, so a pattern of "some
`LaunchOrFocusPlugin`, then leave the mode" would claim whichever plugin launch happened to be
bound first in tab mode. This is a repair for the one keybind this fork moved, not the start of a
general way to hint plugins — that needs the hint to come from the plugin, and is a different
patch.

**The other two bars answer it their own way.** `slim-keybinds` — the bar this fork actually draws
— and upstream's `compact-bar` do not match action lists at all. Each walks an ordered list of
predicates over single actions (`derive.rs`, `keybind_utils.rs`), takes the first bind whose first
action matches, and then gathers every other key in the mode whose first action projects to the
same `ActionType`. Two edits per bar follow from that shape:

- the tab-mode close predicate becomes `matches!(a, Action::CloseTab | Action::CloseTabOrConfirm)
  || a.launches_plugin("zellij:confirm-close-tab")`, so the key is found at all;
- `ActionType::from_action` maps `CloseTabOrConfirm` and that same launch to
  `ActionType::CloseTab`, so the key is *labelled* `close` and gathered into the close hint rather
  than into a hint of its own.

The second edit is the one that does the work, and it sits beside the four rows already there for
`session-manager`, `configuration`, `plugin-manager` and `zellij:about` — a `LaunchOrFocusPlugin`
has always had to be classified by the plugin it names, because the variant alone says nothing.

**Where this differs from the status bar, and why.** There the confirm-close-tab lookup is a
*fallback*: it runs only when the direct `CloseTab` lookup found nothing, so a hint never lists two
keys for one verb. Here a config binding both `CloseTab` and the prompt gets one hint listing both
keys — because collapsing several keys for one verb into a single hint is what `ActionType` exists
to do, and is how `h`, `j`, `k` and `l` already become one `focus` hint. Making it a fallback would
be new behaviour in these bars, not parity with the status bar.

The plugin is still named exactly, for the same reason as in the status bar: a predicate matching
any plugin launch would claim whichever one is bound first in tab mode.

### Synchronised output is assumed, not withheld until a terminal proves it

Upstream wraps a rendered frame in synchronised output (DEC private mode 2026 — the terminal buffers
the whole frame and paints it in one go) in two ways. It queries the host at client startup with
`CSI ?2026$p` and switches the wrapper on when the reply says the mode is known, and until that
reply lands it uses a `TERM` match that names exactly one terminal:

```rust
match os_input.env_variable("TERM").as_deref() {
    Some("alacritty") => Some(SyncOutput::DCS),
    _ => None,
}
```

The query is the part that matters, and it works. What is wrong is the default it falls back on: a
terminal is presumed not to support 2026 until it says otherwise, so anything that supports the mode
but never answers a DECRQM — and anything at all during the round trip at startup — renders
unsynchronised. Zellij's server sends only the lines that changed and clears its output buffer the
moment it has, so a frame that reaches the terminal in pieces is not just a flicker: the terminal
draws part of it at the wrong cursor position and nothing re-sends those lines until they change
again. Over SSH that drift accumulates into duplicated pane borders until a detach and re-attach
forces a full redraw.

The default is now the other way round. Every terminal but alacritty starts on the CSI form, and the
host's DECRPM reply is still the authority in both directions — a reply of `0` (mode unrecognised)
or `4` (permanently reset) turns synchronisation back off. Assuming support is safe because a
terminal that does not implement a DEC private mode still parses the sequence and ignores it; the
cost of being wrong is sixteen bytes a frame that nothing acts on, against a corrupted screen for
being wrong the other way. The remote-attach client gets the change too, and needs it more: its
binding is immutable and never sees the DECRPM reply at all, so the default is the only value it
will ever have.

`ZELLIJ_SYNC_OUTPUT` is the escape hatch for the one host this cannot fix by itself — one that
mishandles mode 2026 and stays silent when asked about it. `off` (or `none`) never synchronises,
`csi` and `dcs` force an implementation, any other value is ignored. It is read once per client, so
it is exported before `zellij` rather than set inside a pane.

**Upstream will not take this.** [#4693](https://github.com/zellij-org/zellij/issues/4693) reports
the SSH corruption and [#4694](https://github.com/zellij-org/zellij/pull/4694) proposes the same
two-line flip, but argues that the CSI path was "never made the default" — which is untrue, the
query has made it the default on a replying terminal since #2798 — and it was rejected on that
point. The disagreement is about the pre-reply default, not about detection, and a fork that runs
on known terminals can take the optimistic side of it.

### The server raises its own open-file limit, and a refused connection is not fatal

A five-day-old session with about twenty-five tabs died and took every pane with it:

```
Error occurred in server:

  × Thread 'server_listener' panicked.
  ├─▶ At zellij-server/src/lib.rs:935:29
  ╰─▶ err Os { code: 24, kind: Uncategorized, message: "Too many open files" }
```

Two separate faults, both fixed here.

**The soft limit was whatever the shell handed over.** macOS still defaults that to 256, and the
server keeps what it inherits. A session's descriptor use grows with what is in it — a pty per
pane, a tab-bar and a status-bar plugin instance per tab, the mounts and caches each of those holds
— so a long-lived session walks into the ceiling and everything downstream of a descriptor starts
failing at once: plugins fail to load, session serialization fails in a loop, and eventually an
`accept()` fails. The server now raises its own soft limit at startup, before it opens anything, to
**`min(hard limit, 10240)`**. It never lowers a soft limit that is already higher and never touches
the hard limit, which would need privileges it does not have. 10240 rather than the hard limit
because macOS reports an unlimited one and honours nothing above `kern.maxfilesperproc`; a failed
`getrlimit` or `setrlimit` is logged and the server runs with what it had. Unix only — Windows has
no equivalent to raise.

Measured on Linux with a server started from a shell holding a soft limit of 1024: the log says
`raised the open-file limit from 1024 to 10240` and `/proc/<pid>/limits` agrees. A second or two
later, once the session has finished starting, the soft limit there rises again to the hard limit —
something in session startup asks for it and Linux grants it, which is why this was never a Linux
crash. macOS is where it bites: the reporter's server sat at 256 for five days.

**A connection that could not be accepted killed the session.** The listener's error arm was
`panic!`, and the panic hook takes the server down, so one client's failed attach cost every pane
in the session. It now logs and backs off: 50 ms after the first failure, doubling to a ceiling of
one second, reset by the next connection that succeeds. EMFILE is transient by nature — a
descriptor is returned, or the limit is raised — so a client that is merely unlucky retries into a
wait it cannot notice. A failure that is not transient costs a log line and a sleeping thread
instead of the session, and the log thins out after the first five so a run of them does not bury
everything else: every one of the first five, then one a minute at the ceiling.

The launcher-owned session this fork exists to serve is exactly the victim of both. It is created
once and lives until the machine reboots, so it is the one that reaches the ceiling, and it holds
the work that a panic in an accept loop throws away.

Reported upstream as [#5474](https://github.com/zellij-org/zellij/issues/5474); nothing has landed
there.

### A plugin's `.wasm` watch goes away when the plugin does

[`plugin_watch`](#plugin-hot-reload-plugin_watch-default-on) registered a watch when a plugin
loaded and never removed it, so a session that opens and closes plugin panes all day accumulates
watches until it exits. Nothing misbehaved — a change to a `.wasm` nothing is running resolves to
no instances and reloads nothing — but the inotify watches, the debouncer's cache entries and the
map of watched files all grew for the life of the server. They are now released in
`unload_plugin`, beside the subscription and the failure record that are already dropped there.

**The watch belongs to the file, not to the plugin id.** One `.wasm` backs every id loaded from it
— the same plugin in two panes, or one instance per connected client — so the watch is released
only when the last of them is gone. Three things count as still wanting it: a running instance, one
that is still loading (the watch is registered before the instance exists), and one that *failed*
to load. That last is the one worth naming: a plugin whose `.wasm` is broken is exactly the pane
the hot-reload loop exists to rescue, so its watch has to survive until the pane does not.

**The entry is found by comparison, not by path.** Unwatching by re-resolving the plugin's path
would leak a watch for a plugin whose `.wasm` was deleted while it was loaded, because
`canonicalize` fails on the way back out. The directory the watch actually sits on is released
separately, once no watched file is left in it.

### A stack list obeys `top_only` as well

The third render path to be told the frame style, after the tiled one and the floating one. A stack
list's rows are drawn by `Tab::render_stack_list_headers`, which builds a `PaneContentsAndUi` of its
own and never called `set_top_only_frames` — so under
[`pane_frame_style "top_only"`](#pane_frame_style-top_only) every pane in the tab wore a rule and
the stack list sat among them on blank lines. It was the one place the setting visibly did not
hold.

**One of the four differences carries over, and it is the visible one.** A stack list row *is* the
title line of the pane it names, so `top_only`'s rule fills the room around the entry instead of
leaving it blank — difference 1, the same one condition. Difference 2 costs nothing here (these
rows draw no boundaries to suppress) and difference 3 does not arise.

**The title's left alignment (difference 4) deliberately does not.** The entry stays centred. Its
start is also the budget the multi-client focus indicator is drawn into, so moving the entry to the
left edge would drop that indicator; and the entry is a bracketed list item rather than a bare
title, which is the thing difference 4 is about. What is inside the brackets stays blank for the
same reason it does on a title line: a placed element is not a gap. The `>` on the selected row
outranks the fill and is still drawn.

### A title the program sets for itself reaches plugins

A pane title set with OSC 0/2 updated zellij's own pane state but produced no `PaneUpdate`, so a
plugin's cached titles went stale for exactly the panes whose program renames them — which is most
of them. `list-panes --json` reported the new title immediately, and a plugin subscribed to
`EventType::PaneUpdate` heard nothing until some unrelated pane event happened to rebuild the
manifest. A plugin could only see the change by polling `get_pane_info` on a timer. Upstream
[#5482](https://github.com/zellij-org/zellij/issues/5482).

The cause is that the OSC title is the one pane state change that arrives as pty output rather than
as an instruction. Every other change runs through `log_and_report_session_state`; pty bytes go
straight into the grid and never touch the reporting path.

An OSC title change now reports the pane state, so subscribers get a `PaneUpdate` carrying both
`title` and `program_title`. **A change of value, not a write:** most shells rewrite the same title
on every prompt, so the grid flags the change only when the new title differs from the one it
already held. OSC 22/23 title push/pop counts, since it changes what is reported.

Only the pane manifest is rebuilt, not the whole session state — a title change is a per-prompt
event and does not need the session info regenerated with it. No plugin API field is added:
`program_title` already ships (see
[Pane identity and stack membership in `PaneInfo`](#pane-identity-and-stack-membership-in-paneinfo)),
so `event.proto` and the client/server contract are untouched.

### Which tab a pane is in, on the pane

`PaneManifest` is keyed by tab **position**, and a position moves whenever tabs are reordered. So a
plugin holding a manifest could not say which tab a pane was in: the key it had was not an identity,
and turning it into one meant holding the latest `TabUpdate` too and correlating two events that are
not delivered together. Everything that reports "pane X in tab Y" rebuilt that join by hand.

`PaneInfo` now carries `tab_id`, the same stable id `TabInfo.tab_id` reports. The join is one field
lookup, it survives a reorder, and it needs no second event.

```
zellij action list-panes --all --json | jq '.[] | {id, tab_id, tab_position}'
```

**`PaneListEntry` no longer carries a `tab_id` of its own.** It flattens `PaneInfo` into the same
JSON object, so keeping both would write the key twice — and on the way back in, the outer field
would swallow it and leave `PaneInfo.tab_id` at zero. The CLI output is unchanged: the same
`tab_id`, with the same value, now coming from the flattened `PaneInfo`. `tab_position` and
`tab_name` stay where they are.

The field crosses the plugin API: `event.proto` tag 912, regenerated prost output, both `TryFrom`
implementations. It is also carried by the KDL codec behind `session-metadata.kdl`, written beside
`id` and read back optionally so metadata from an older build still parses. The client/server
contract has no `PaneInfo` and is untouched.

### `zellij pipe` answers, and gives up when nothing does

```
zellij pipe --name build-done -- ok        # gives up after 30 idle seconds
zellij pipe --timeout 5 --name build-done -- ok
zellij pipe --timeout 0 --name build-done -- ok   # waits for a plugin however long it takes
```

Two things were wrong on this path, and each hid the other.

**Every CLI pipe reported an error it had no reason to report.** `Action::CliPipe` drops its
completion channel on purpose — a pipe is released by the plugin that received it, on the plugin's
own messages, not by the route thread — but the route thread then reads a dropped channel as "the
action reported nothing" and sends the caller `Action CliPipe ended without reporting a result`.
That message reached the client ahead of the plugin's real answer, so `zellij pipe` exited **2**
about fifty milliseconds after it was called, whether or not anything was wrong. The pipe arm now
returns where it finishes, the way [every other action that answers for itself
does](#the-cli-output-convention), so nothing is sent on its behalf.

**With that gone, the hang underneath it is reachable.** Only a plugin that received the message
can unblock a pipe, so a session with none listening — a bare layout, or a `--plugin` that was
launched by this very call and is still loading when the message is dispatched — never answers, and
the client waited on it forever. A hook that shells out to `zellij pipe` inherited that wait.
`--timeout` bounds it: exit **2**, and a sentence on stderr naming the pipe and saying why a pipe
goes unanswered, which is [the `2` door for a wait that ran
out](#the-cli-output-convention).

**The window is idle time, not a budget for the call.** It measures how long the *session* has said
nothing, and any message restarts it. That is what makes a default safe: `tail -f | zellij pipe`
spends its time blocked on `read_line`, which is outside the window entirely, and a plugin that
streams output restarts it on every send. Verified — a producer emitting a line every two seconds
for six ran to completion under `--timeout 3`.

Nothing bounds that read, which is the whole reason [only a pipe that *is* its
stdin](#a-payload-on-the-command-line-is-the-whole-pipe) is allowed to reach it.

**So it defaults to 30 seconds rather than to nothing.** A flag-only patch would have turned
today's fast (if wrong) exit into an unbounded hang for every existing caller, which is a worse
trade than the one this makes. Thirty seconds is generous enough for the slow legitimate case, a
first plugin load that compiles wasm; `--timeout 0` restores the unbounded wait for a caller who
wants it, and says so in the timeout message.

The flag never reaches the session. It is taken off the request by `CliAction::take_pipe_timeout`
and held by the client, so `Action::CliPipe` and the client/server contract are untouched.

Upstream v0.46.0's client pipe path is `drive_pipe` and `drive_streaming_pipe` in
`zellij-client/src/cli_client.rs`, and it lets the plugin set the exit code
(`UnblockCliPipeInput.exit_code`). The fork's idle window sits on it as `PipeWait`. A lost session
still exits 1 with the fork's sentence, where upstream exits 2 silently.

### A payload on the command line is the whole pipe

```
zellij pipe --timeout 5 --name claim -- payload      # answers, then exits - whatever stdin is
echo data | zellij pipe --name logs                  # unchanged: this one IS its stdin
```

**`zellij pipe -- payload` got its answer and then blocked forever**, on any stdin that is neither a
terminal nor ever at EOF. The client sends the payload, prints what the plugin says, and then loops
for more to send; the test for whether there is more asked only whether stdin was a terminal. A
non-terminal it never intended to read is the *usual* case for a program that shells out — a command
run over ssh, or a child of a long-lived server, inherits a handle that stays open for the life of
the parent. Measured on a session with nothing listening: `< /dev/null` answered in 25ms, the same
call with an inherited handle sat there until it was killed, and `( sleep 30 ) | zellij pipe
--timeout 5 -- payload` spent the whole thirty seconds.

**`--timeout` could not end it**, because the read is outside the window by design — that is what
makes the window safe for `tail -f | zellij pipe`. So the flag that exists to bound a pipe was
powerless against the one way a pipe hangs by accident, and `< /dev/null` was the only cure. A
caller should not have to know that.

**A payload given on the command line is now a reason not to read stdin, the same as a terminal is.**
The pipe sends it, waits for the answer under `--timeout`, sends the end-of-pipe message and stops —
which is exactly where `zellij pipe -- payload < /dev/null` has always ended. Both shapes above now
return in about 20ms. Nothing changes for a pipe that means its stdin: `echo data | zellij pipe
--name logs` with no payload still reads the stream to EOF, and a terminal still exits on the
plugin's answer.

The one behaviour traded away is `echo data | zellij pipe --name n -- payload`, which used to send
both and now sends only the payload. An invocation that says its message twice never had an obvious
meaning; the one that means the stream is the one that leaves the payload off, and `pipe --help` now
says so on the argument itself.

Client-side, and one branch of it: `pipe_reads_stdin` in `zellij-client/src/cli_client.rs`. No
action, no message and no contract changed.

### `zellij action --json` answers the mutations too

```
$ zellij action --json new-tab
{
  "tab_id": 2,
  "pane_id": "terminal_4",
  "handle": "hefty-magpie"
}
$ zellij action --json close-pane --pane-id sunny-otter --yes
{
  "closed": "terminal_3"
}
$ zellij action --json toggle-floating-panes
{}
```

[The convention](#the-cli-output-convention) said `--json` was the direction for the mutations and
not yet a promise. It is one now. A verb that reports a record answers with one object; a verb that
reports nothing answers `{}`, because a parser that asked for an object should get one rather than
an empty stream.

**The flag goes in front of the verb, and that is deliberate.** The seven verbs that were already
answering in JSON own the trailing `--json`, and a second flag by the same name on the same command
is not something clap will take. Rather than invent a second spelling, the parent takes it — and
for those seven `zellij action --json list-panes` and `zellij action list-panes --json` are the
**same call**, because the parent flag simply turns the verb's own one on. One name, one meaning,
two positions only where both already existed.

**A payload refuses rather than answering something wrong.** `dump-screen` and `dump-layout` print
what a pane or a layout holds, which has no keys; the call exits **2** and says so. Which verbs
those are is read from the same `prints:` table `zellij setup --dump-surface` publishes, so the flag
and the map cannot drift apart.

**The values are typed the way the other JSON surfaces type them.** A tab id is a number, as it is
in `list-tabs --json`; a pane id is a string, as it is in `list-panes --json`; `true` and `false`
are booleans; and `-`, which is how the fork's output says a field has no value, is `null`. The keys
come out in the order the command prints them rather than alphabetised, so the object reads like the
record it replaces.

**Nothing about the plain output moved.** Without the flag every verb prints exactly the bytes it
printed before: the whole feature is in the client's printer, so no action, no message and no part
of the client/server contract changed. `zellij run`, `zellij edit` and `zellij pipe` are their own
top-level commands rather than `action` verbs, and do not take it.

### A `subscribe` feed does not narrow when somebody attaches

```
zellij subscribe --pane-id sunny-otter --pane-id brave-heron --format json
```

Two panes in two different tabs. With nobody attached, both feeds ran. The moment a human attached,
the pane in the tab they were not looking at went silent — and it came back when they switched tabs.
A consumer therefore got *more* data while nobody was watching, and the feed rearranged itself
around a human's tab.

The cause is that the two delivery paths read from different places. With a client attached,
subscribers were served out of the render report, which carries only what was rendered:
`Tab::render` returns early for a tab whose `connected_clients` set is empty, so panes in every
other tab are simply not in it. With no client attached nothing renders, and the fallback path
queried every subscribed pane from its tab directly — which is why the detached feed was the
complete one.

There is now one path. It takes the render report when there is one, and queries whatever the
report is missing from the pane's own tab. Attached and detached feeds are the same feed, a tab
switch changes nothing, and a subscription made before a client attached keeps every pane it named.

- **Floating panes are covered by the same fix.** They are rendered only while they are visible, so
  a subscribed floating pane went silent when its tab hid it. It no longer does.
- **A subscribed plugin pane is read through a client.** A plugin pane keeps a grid per client, and
  the direct query passed `None`, which matches no grid and returns nothing — so the fallback path
  served empty contents for one. It now reads through a regular client, the way the initial
  delivery at subscribe time always did. With nobody attached there is no client to read through
  and a plugin pane still reports empty; a terminal pane is unaffected either way.
- The cost is unchanged. All of it is behind the `is_empty()` gate on the subscriber map, so a
  session with no subscribers does the same work it did before.

### A plugin's `BeforeClose` handler gets a window to finish in

`Event::BeforeClose` is a plugin's one chance to say anything on the way out — flush a queue, post
a last state, tell whatever it reports to that it is going. It was delivered, and then the session
did not wait for it.

Two separate things cut it off, and a plugin hit both at once on a session teardown:

- **Nothing waited for the handler to run.** `WasmBridge::cleanup` unloads every plugin, and each
  unload queues its `BeforeClose` call on a worker thread and returns. The plugin thread then
  returned as well, the server followed, and what the handler had managed by then was whatever the
  scheduler had got round to — on a fast teardown, nothing at all.
- **Its outbound work had nowhere left to go.** A `web_request` or a `run_command` from a plugin is
  dispatched by the background-jobs thread, and that thread was told to exit at the same moment as
  every other one — long before a handler running on a worker thread could reach it.

So the teardown now holds the door open:

- `cleanup` waits for the queued handlers, up to **two seconds**. A plugin that hangs in its
  handler cannot hang the session; when the window closes the teardown proceeds and that plugin's
  last words are lost, with the timeout in the session log.
- **Background jobs are told to exit last**, after the plugin thread has joined, so a request a
  handler issues still reaches the dispatcher.
- The background-jobs thread then **waits up to three seconds for the web requests already in
  flight**, which is shorter than the 30s a request itself is allowed: one that has not answered by
  now is not worth holding a closing session open for. A session whose plugins had nothing
  outstanding pays none of this.

**A pane closing on its own is unchanged.** The wait is in `cleanup`, which runs once, at session
teardown. Closing one plugin pane in a live session queues its handler exactly as before and blocks
nobody.

**SIGKILL stays lossy, and always will.** There is no `BeforeClose` under `SIGKILL` because there
is no code left to run one — the process is gone between one instruction and the next. A plugin
that must survive that has to write down what it knows as it goes rather than at the end, and a
consumer that must detect it has to notice the session stop rather than wait to be told. `SIGTERM`
is the signal that gets the graceful path (see [The server serializes and archives on
SIGTERM](#the-server-serializes-and-archives-on-sigterm)), and `zellij session down` sends that one.

### Whether a pane's command is still running (`command_state`)

```
$ zellij action list-panes --all --json | jq '.[] | {handle, command_state, last_command_exit_code}'
{"handle":"sunny-otter","command_state":"running"}
{"handle":"brave-heron","command_state":"done","last_command_exit_code":1}
{"handle":"quiet-lark","command_state":"prompt"}
```

A shell that emits OSC 133 says exactly where its prompt starts, where a command's output starts,
and where the command ended — with its exit code, when it reports one. zellij parses those
markers on the input path and keeps them on the row. Upstream used them for the triple-click that
selects a command and its output, and v0.46.0 adds its own tracker over OSC 133 and 633. The fork
reads the row markers; it carries no OSC 133 parser of its own.

They are the only exact answer to "is this pane busy". Everything else in the tree infers it: the
foreground process group (wrong when the shell has job control off — see [What a pane is running,
when the shell has no job control](#what-a-pane-is-running-when-the-shell-has-no-job-control)), the
newest child process, `last_output_at` going quiet. Each of those is a guess about a shell that
knows the answer and is already telling us.

- `command_state` is `prompt` (the shell has drawn a prompt, nothing is running), `running` (a
  command is producing output and has reported no end) or `done` (the last one ended).
- `last_command_exit_code` comes with a `done` and only with a `done`, and only when the shell put
  a code in its end marker.
- `input_since_prompt` is `true` when input was typed after the shell's last prompt marker, so a
  `prompt` is the one from before that input (from nkmk.33). It is absent otherwise. See [`--for
  ready` after typing a command](#--for-ready-after-typing-a-command-the-prompt-must-be-fresh).
- **A shell that emits no markers gets `command_state` from upstream's foreground-process
  tracker** (`command_state_detail.reports_markers` is `false` there): `running` while a
  foreground program runs, `done` once the shell is back. The tracker cannot see a prompt without
  markers, so it never says `prompt`, and before the first command the key is absent.
  `last_command_exit_code` stays absent: the tracker has no exit code to give. A command pane
  without markers reads `running` until its command exits, then `done`. A pane whose shell writes
  markers is answered by its markers alone, as before. `wait --for ready` reads only the markers'
  word, so a program that never ends still falls back to quiet.
- **Both keys are absent for a plugin pane**, and `command_state` is absent for a shell without
  markers that has run nothing yet. Absent means "this pane cannot answer", which is a different
  thing from `prompt`.
- The state is read from the last marker in the grid, walking back from the newest row, so it costs
  a short scan per pane on a command that already builds every pane's info.
- **`command_state` stays a string.** Upstream v0.46.0 has its own `PaneInfo.command_state`, a
  struct. On the JSON rows it is serialized as `command_state_detail`, so the fork's key keeps its
  name and type. Upstream's other new row keys (`attention`, `program_status`,
  `nested_session_name`, `is_collapsed`) are additive.

#### Why this surface and not a plugin event

Both were assessed. The consumer is a program driving zellij over the CLI, and it already polls
`list-panes --json` — for the pane list, the handles, `last_output_at` and the agent column. Two
more keys on a row it is already reading cost that consumer nothing to adopt and cost this fork
nothing to carry: `PaneListEntry` is CLI-only, so there is no protobuf tag, no regenerated prost
output, no second `TryFrom` and no plugin-visible contract. It is the cheapest of the three
surfaces AGENTS.md ranks, and it answers the question that was asked.

A `CommandStateChanged` event was the alternative. It was not built, for two reasons. It would
cost an `EventType` and a payload tag to tell plugins something no plugin here wants, and the CLI
consumer — the one that does want it — cannot subscribe to plugin events at all. And a push event
has to be *detected*, which means diffing every pane's marker state on a path that runs at render
frequency, where nothing else reports it. If a plugin ever needs it, the event is a separate patch on top of this one, and this
patch's grid accessor is what it would read.

### A client whose terminal dies detaches instead of crashing

Kill the terminal window a client is attached to, or drop the ssh connection carrying one, and the
tty starts answering EIO. The client used to panic on the way out, because its exit teardown
unwrapped calls that only a live terminal can satisfy:

```rust
os_input.disable_mouse().non_fatal();      // already tolerant
os_input.unset_raw_mode().unwrap();        // EIO here on linux
stdout.write_all(goodbye_message.as_bytes()).unwrap();   // EIO here on macos
stdout.flush().unwrap();
```

Which line goes first is the platform: after a hangup linux fails `tcsetattr` too, macOS still
lets it through and falls over on the write. Upstream v0.46.0 ignores these errors in
`start_client`'s teardown. In `start_remote_client`, where upstream still unwraps, the fork makes
the calls of both teardown blocks `.context(...).non_fatal()`: the errors are logged, not
discarded. The **startup** counterparts are deliberately left alone: there the terminal is expected
live, and a failure means the client cannot run at all.

The panic was worse than it looks, and not only because it is noisy. `handle_panic` printed its
report with `println!`, which itself panics when stdout is gone — and a panic raised inside a panic
hook aborts the process. That is the `Abort trap: 6` in the crash reports, and it is why the same
event exits 101 on linux (where the hook's `unset_raw_mode()` guard fails first, so `handle_panic`
never runs) and SIGABRTs on macOS. That `println!` is now a best-effort write.

**The render path is upstream's.** In v0.46.0 a failed render write sends `ClientExited`, prints
"Failed to write to the terminal", and exits 1. The fork carries no render handling of its own, so
an interactive client whose terminal dies exits 1, not 0.

The reported symptom was two things, and only one of them reproduced. The client crash reproduces
exactly — spawn a client on a pty, close the master, and the hangup that starts a normal detach is
the same event that makes the tty answer EIO. The *server* being left unattachable does not: after
the crash, `zellij ls` reported `CLIENTS 0` and the next `attach` came up on a session with its
panes and scrollback intact. The client sends `ClientExited` before the teardown block, and
`route.rs` announces `RemoveClient` whenever a route thread's loop ends, which a closed socket
forces — so nothing in this path can strand a client server-side. The reporter said themselves they
could not tie the two together.

What the fork adds over upstream v0.46.0: the remote client's teardown above, and the best-effort
write in `handle_panic` (`zellij-utils/src/errors.rs`).

### `--cwd` reaches a pane that runs no command

```
$ zellij action new-pane --cwd /etc -- ls    # always opened in /etc
$ zellij action new-pane --cwd /etc          # opened wherever the focused pane was; now /etc
```

`zellij action new-pane` has two halves, and only one of them ever honoured `--cwd`. A pane that
runs a command carries its working directory inside `RunCommandAction`, the struct that says what
to run. A pane that runs no command has no such struct — it gets the session's default shell —
and none of the four `Action::New*Pane` variants has a field for a directory. The CLI resolved
`--cwd`, built the action, and dropped the value on the floor. It was silent: no warning, no
error, a pane in the wrong directory.

Every command-less placement was affected — plain, `--floating`, `--stacked` and `--in-place`.
Three neighbours were not, and they are why this read as a fork bug rather than an upstream one: a
`new-pane` **with** a command works, `new-pane --blocking` works (`Action::NewBlockingPane` is a
fork addition and carries a `RunCommandAction`), and `new-tab --cwd` works (`Action::NewTab` has
carried a `cwd` all along). So four of seven ways to name a directory did what they said.

**The fix is one new action, not a field on four old ones.** `Action::NewShellPane` is the
command-less half of `new-pane`: a placement, a name, a directory, and the three routing flags.
`NewPanePlacement` — the enum `NewBlockingPane` already uses — says which of the four this stands
in for, so one action replaces four and the CLI reads the same way for the blocking and the
non-blocking case.

It had to be a new action. `NewFloatingPaneAction`, `NewTiledPaneAction`, `NewStackedPaneAction`
and `NewInPlacePaneAction` are upstream messages in the versioned client/server contract, and
adding a field to one is the thing "Adding a field, and which contract it crosses" in
[AGENTS.md](AGENTS.md) forbids outright. A new message in the fork's reserved tag block costs a
tag and nothing else. `NewShellPaneAction` takes **905** in the fork block from 900.

**No `--cwd` still means "wherever the focused pane is".** This is the part that had to be got
right, and the CLI made it easy to get wrong: `new-pane` defaults its `cwd` to the caller's own
directory before it builds the action, because a *command* pane has always started where the
caller stood. A command-less pane has not — it inherits from the pane you were looking at, which
`Pty::fill_cwd` does by filling a directory that is still `None`. So the new action carries only a
directory the caller actually named. Nothing is filled in on their behalf, and a bare
`zellij action new-pane` opens exactly where it did before.

**`--borderless` now reaches `--stacked` too.** The old `Action::NewStackedPane` had no field for
it, so `new-pane --stacked --borderless` ignored the flag while `new-pane --stacked --borderless
--blocking` honoured it. `NewPanePlacement` carries it, so both agree now. `--in-place` still
ignores it: that pane is spawned through `SpawnInPlaceTerminal`, which has no borderless argument,
and giving it one is a different patch.

**Mixed versions: this action degrades to nothing, and the command-less `new-pane` degrades with
it.** Sockets are scoped by contract version, not by version string, so a fork client can reach a
stock server of the same contract. Tag 173 means nothing there: the server decodes an `Action`
with no `action_type` set and makes no pane at all. Before this patch the same command sent an
upstream message and opened a pane in the wrong directory. So against a stock server, a
command-less `zellij action new-pane` goes from "works, ignores `--cwd`" to "does nothing" —
naming a command, or `--blocking`, still works. Nothing else is affected, and no existing message
was renumbered or reshaped.

### One session that will not answer no longer hangs every command that lists sessions

A server whose teardown wedges keeps its listening socket, and a socket that accepts a connection
and then says nothing used to hang **every** command that enumerates sessions — for every session
in the directory, not just its own. `zellij ls`, `attach`, `delete-session`, `kill-session`,
`session up` and the "did you mean" name suggester all go through `get_sessions`, which probes each
socket in turn and used to block in `recv` with no deadline. One bad tab close took session
enumeration out machine-wide until the pid was killed by hand.

Measured on the tip before this patch, with one unresponsive socket sitting beside a healthy
session in the same directory: `zellij ls` had not returned twenty seconds later, and the healthy
session was never listed. After: 2.1 seconds, the healthy session listed, the unresponsive one left
alone.

Four changes, and they are four because the failure has four parts.

**The probe is bounded.** The liveness probe gives a server two seconds to answer, in both
directions, and treats silence as "not a session". That is the answer it already gave to a socket
that refused the connection, so nothing downstream had to learn a new case. A probe that gives up
costs a session its row in a listing and nothing more — the *destructive* paths do not ask this
question. `socket_is_occupied`, which is what stands between a new session and a live server's
name, asks only whether anything is listening and waits for no reply at all. The unresponsive
socket is deliberately left in place: unlinking it would orphan a server whose panes are still
running, which is the failure that function exists to prevent.

**The accept loop stops making clients once the session is over.** The listener runs until the
process does, and every connection it accepted was turned into a client id, a router thread and a
sender thread. After the main loop has broken there is nobody left to serve any of them, so a late
connection left its caller waiting on a reply that was never coming and left threads behind —
upstream measured exactly `+1 router +1 listener` per probe, climbing without limit. A connection
accepted during shutdown is now closed instead. Under a storm of connections arriving through a
last-tab close, the census two seconds after the close went from forty sender threads left behind
to none; the router threads exit on their own here, the sender threads did not.

**A pipe whose session went away stops instead of spinning.** `zellij pipe --timeout 0` waits with
a blocking read, and a read that returns nothing because the socket closed fell through to the top
of the same loop and read again — a spin at 100% of a core, for as long as the process lived. A
session ending under fifty in-flight pipes left fifty of them behind. It now says the session
stopped answering and exits 1, which is what the `--timeout` path has always done by way of its
reader thread closing its channel. Closing a connection accepted during shutdown, rather than
registering it, is what made this easy to hit rather than occasional.

**Every wait in teardown is bounded.** The five joins in `SessionMetaData::Drop` and the write lock
the session is taken out of were all unbounded, and the socket file is unlinked only after they
return — so one stuck thread meant a server that stays alive forever holding a socket that answers
nothing, which is the orphan the first two changes are cleaning up after. Each join now waits five
seconds and then leaves the thread behind with a log line saying which one it was. Five separate
budgets rather than one shared: the plugin thread's `BeforeClose` window and the background-jobs
thread's web-request drain are each bounded already, and a shared budget would let a slow pty spend
the window one of those was promised. The lock is taken on a helper thread and waited for over a
channel, because polling `try_write` would livelock against the route threads reading the same
lock.

The wedge itself did not reproduce here — a last-tab close under fifty in-flight `zellij pipe`
clients, then under two hundred and sixty, then under a continuous storm of them arriving through
the close, exited cleanly every time, which the accept-loop and `BeforeClose` patches above have a
lot to do with. The leak and the hang both did reproduce, and they are what turns a wedge anywhere
in teardown into a machine-wide outage. This patch is about the blast radius rather than the
trigger.

Reported upstream as [#5440](https://github.com/zellij-org/zellij/issues/5440). Upstream v0.46.0
bounds the probe's `recv` in `assert_socket` (`fa131c8b4`, #5481). The fork's `socket_answers`
bounds both directions and covers that, so the fork keeps its own probe; the other three changes
are the fork's alone.

### `session status` ends quietly when the reader stops reading

```
$ zellij session status mysession | head -3
session   mysession
init      systemd
service   /home/user/.config/systemd/user/zellij-mysession.service - installed
thread 'main' panicked at library/std/src/io/stdio.rs:1165:9:
failed printing to stdout: Broken pipe (os error 32)
```

`head` closes the pipe once it has its three lines, and the fourth `println!` gets `EPIPE` back.
`println!` has one answer to a failed write, which is to panic — so a reader doing something
entirely ordinary produced a crash report, a `101` exit, and a stack trace in the middle of a
script.

The report now goes through a handle. `print_status` takes a `Write` and returns `io::Result<i32>`,
its three stdout sub-printers (`print_unit_drift`, `print_pin_state`, `print_configured_extras`) do
the same, and `report_exit_code` turns the outcome into a status: `BrokenPipe` exits **0** and says
nothing, any other write error is reported on stderr and exits 1. A reader that left is not this
command's failure; a full disk on a redirected stdout still is.

Two things it deliberately does not do. It does not touch the process-wide `SIGPIPE` disposition —
this binary is also the server and every pane in it, and restoring the default disposition would
change what happens to a pane's process group, not just to this printer. And it does not sweep the
rest of the CLI: `print_unit_dir_disagreement` is the one printer beside these that `status` calls,
it writes to **stderr**, and stderr is not the handle `| head` closes.

The exit code is 0 even when the report that was cut short was going to be 1. Nothing read the
answer, so there is no answer to fail with, and a `| head` that exits non-zero half the time is the
next bug report.

### `dump-screen` sweeps many panes in one invocation

```
$ zellij action dump-screen -p sunny-otter -p terminal_3 -p brisk-heron
{"pane_id":"terminal_1","handle":"sunny-otter","content":"$ cargo test\nrunning 4 tests\n"}
{"pane_id":"terminal_3","handle":"quiet-fern","content":"$ \n"}
{"pane_id":"terminal_9","handle":"brisk-heron","content":"error[E0308]\n"}

$ zellij action dump-screen --all | jq -r 'select(.content | test("error")) | .handle'
brisk-heron
```

`--pane-id` repeats, and `--all` is every pane the session has. A caller reading a session over ssh
pays for the **round trip**, not for the dump: a sweep of ten panes was ten ssh invocations, ten
process starts and ten connections to read ten screens the session already holds. It is now one of
each, and ten local messages.

**It is the same request, asked N times down one connection.** Nothing new travels: the client
sends the per-pane `DumpScreen` the session has always answered, once per pane, and wraps each
answer as it arrives. No new action, no message tag, no protobuf, nothing added to the client/server
contract — the same reason `list-agents` and `wait` are client-side.

**One pane is what it always was, byte for byte.** `dump-screen --pane-id sunny-otter` prints the
bare content and nothing else; a consumer parsing it raw sees no change at all. The shape follows
what was asked for, so it turns into JSON only when more than one pane is named, or with `--all`.
A caller that wants the JSON shape for exactly one pane has to ask for the sweep some other way —
`--all` on a one-pane session — and that is the one rough edge here: a script that builds
`--pane-id` from a list must know that a list of one answers differently from a list of two.

**A line per pane, not one array.** A `--full` sweep of a long-lived session is megabytes, and a
line is written the moment that pane answers — so the caller can read the sweep while it is still
running and nothing has to hold every dump at once. Each line stands alone, which means a sweep cut
short is still a sequence of whole answers. `jq -s .` collects them into an array for a caller that
wants one document. It is the shape `subscribe --format json` already prints, for the same reason.

**Misses are per pane, and the sweep finishes.** A named pane nobody answers to gets the sentence
every `--pane-id` verb gives it — `No pane answers to 'terminal_99'` on stderr — the panes beside it
are still dumped, and the invocation exits **2** at the end. One bad name costs that one pane rather
than the nine that were there, which is what makes a sweep worth building a script on. A target that
names no pane in any *form* is malformed input rather than a miss: it is read before anything is
asked, so it exits **1** with nothing dumped. `--all` on a session with no panes to dump prints
nothing and exits **0** — nothing was asked for by name, so nothing is missing.

`--full` and `--ansi` shape every dump in the sweep. `--file` (and `--path`) writes the same bytes
it would have printed, and a reader that stops reading — `| head -1` — ends the sweep quietly at 0,
the answer [`session status`](#session-status-ends-quietly-when-the-reader-stops-reading) gives.
`--all` sweeps the panes `list-panes` itself reports, in its order, so the two commands cannot
disagree about what the session holds.

**The trap, for the next client that sends more than one message.** The session answers a CLI
message with its report *and then* an `UnblockInputThread`, sent from the bottom of the route loop.
A client that asks one question and exits can ignore the second message; a client that asks several
cannot, because the unblock it left behind is the first thing the next question reads — and every
answer after that is one question late, which reads as an empty dump for a pane that is plainly
there. `ask_in_sequence` reads *through* the unblock and leaves the connection on a message
boundary.

`Action::actions_from_cli` refuses a sweep rather than building one action out of it. Nothing on
the CLI path reaches that — the client takes the sweep first — but the integration harness builds
actions directly, and the alternative was dumping the first of several panes and saying nothing.

### A resurrected command pane drops to the shell when its command exits cleanly

```
$ zellij session up mysession      # the pane comes back holding `codex resume`
                                   # ENTER runs it, you work, you quit codex
                                   # before: EXIT CODE: 0, and an ESC to reach the shell
                                   # now:    the shell, in the directory codex ran in
```

A pane that a session resurrection brought back used to end its command the way every command pane
does: it held, printed `EXIT CODE: 0`, and offered `<ENTER> run, <ESC> drop to shell, <Ctrl-c>
exit`. For a tool you quit on purpose - a coding agent, a REPL - the answer was always ESC, so the
banner was a keypress between finishing and having a prompt back. It now takes that path itself.

**It is the ESC path, not a copy of it.** `TerminalPane::handle_held_drop_to_shell` was split into
a `drop_held_to_shell` on the `Pane` trait - it leaves the held state, resets the grid, drops the
banner and reports the directory the command ran in - and the key handler now calls that. `Tab`
calls the same method, then sends the same `PtyInstruction::DropToShellInPane` with the same
default shell that ESC sends. The pane is still held first and released a moment later, rather than
never held, so the sequence is the one a keypress produces and nothing new was taught to the
screen thread.

**A failure still holds.** Only `Some(0)` takes this path. A non-zero status holds and prints its
code as before, and so does a signal - a killed child reports `None`, never `Some(0)`. The pane
that fails is the pane you want left on screen.

**This entry's scope is exactly panes whose command came back with a session.** Every other command
pane held as before, until the entry below it gave them the same drop behind a config key. A
resurrected pane still needs no key and reads none.

**How a pane is known to be resurrected.** `RunCommand` gains a `resurrected` bool, set on every
command in the layout when that layout is loaded, and read once where the exit is handled. Both pty
quit callbacks funnel through `ScreenInstruction::HoldPane`, so `Tab::hold_pane` is the only place
that had to change - the flag reaches it whether the command was spawned with the session
(`attach --force-run-commands`) or run by the ENTER a suspended pane offers.

Which layouts get marked is decided by the layout's **file name**. Only a path reaches the server -
the client resolves which file to resurrect from and hands over a `LayoutInfo::File` - and both
resurrection sources write one name, `session-layout.kdl`: the in-place cache under the session's
info folder, and a snapshot. So a hand-written layout would have to be called `session-layout.kdl`
to be taken for a serialized session, and if it were, it would be one.

**Nothing crosses a contract.** `resurrected` is `#[serde(skip)]` and is not in either protobuf, so
it reaches neither disk, the plugin API, nor the client/server contract. It is provenance, and it is
kept out of `PartialEq` for the same reason: the tree compares `Option<Run>` in half a dozen places
to decide whether a pane already running something is the pane a layout means, and a marked pane has
to keep matching the entry it always matched. `RunCommand`'s `PartialEq` is therefore hand-written,
with the fields destructured exhaustively so that a field added upstream fails to compile rather
than dropping out of equality unnoticed.

**What the pane gives up.** Two things, and they are the two ESC already gave up. The ENTER that
re-runs the command is gone with the banner, so running it again means typing it. And the pane is
now running a shell, which is what the next serialization records - a held pane keeps the command
in its recorded layout, because no child is left to observe, while a pane at a prompt records the
default shell as nothing. So the next resurrection brings that pane back as a plain shell in the
same directory, rather than offering the command again. That is the cost of not pressing ESC, paid
at the same moment ESC used to pay it.

### A command pane can drop to the shell when its command exits cleanly

```
// config.kdl
command_pane_on_clean_exit "shell"

$ zellij run -- cargo test           # before: EXIT CODE: 0, and an ESC to reach the shell
                                     # now:    the shell, in the directory cargo ran in
```

The entry above gave a resurrected pane that behaviour and stopped there. But the panes an operator
actually ends up staring at come from the CLI - `zellij run --`, `zellij action new-pane -- cmd`,
the same call an agent makes - and each of those still held on success and wanted the same ESC. The
new option `command_pane_on_clean_exit` extends the drop to all of them.

**Two values, and the default is the old behaviour.** `"hold"` is the default and is upstream: every
command pane holds on `EXIT CODE: 0` and waits for `<ENTER> run, <ESC> drop to shell, <Ctrl-c>
exit`. `"shell"` makes a clean exit take the ESC path itself. An unknown value fails the config, as
every other enum-valued top-level key does.

**It is one global switch, and layout command panes come along.** A per-origin distinction would
need a field on `RunCommand`, which is the expensive shape the entry above paid for once and this
one declines to pay again. So a layout `command` pane under `"shell"` also drops, losing the
ENTER-to-re-run its banner offered. That is the price of the cheap surface, and it is the operator's
to accept by setting the key.

**A resurrected pane is unconditional.** `run_command.resurrected` still drops on its own, under
either value. `"hold"` restores upstream behaviour for everything else and changes nothing about a
pane that came back with a session.

**A failure still holds, under both values.** Only `Some(0)` qualifies. A non-zero status prints its
code and waits, and a signal reports `None` and does the same.

**A suppressed pane is never dropped.** `Tab::drop_held_pane_to_shell` resolves its pane through
`get_pane_with_id_mut`, which also reaches the suppressed map - the scrollback-editor path, where a
pane stands aside for the editor that replaced it. The entry above was safe there only because
`resurrected` is never set outside a resurrection layout; a config key that any command pane can hit
is not. So the decision is now taken inside the tiled and floating branches of `Tab::hold_pane` and
is `false` in the suppressed branch, and a suppressed pane keeps its hold so the pane it stands in
for is restored.

**Nothing crosses a contract.** It is an `Options` field, and fork `Options` fields are absent from
the client/server protobuf and set to `None` in the `TryFrom`. No protobuf tag, no plugin API, no
snapshot format. The value is shared from `Screen` to every `Tab` as an `Rc<RefCell<…>>`, the way
`input_while_scrolled` is, so a config reload reaches every tab without a per-tab update.

**The consumer sees nothing new.** A dropped pane lands exactly where ESC leaves one today:
`is_held` false, `exited` false, and the pane running a shell. `list-panes --json` keeps its shape,
and every reader that already handles an ESC-dropped pane handles this one.

**Roll the config out first.** `command_pane_on_clean_exit` is a top-level key, and an unknown
top-level key is ignored rather than rejected, so it can be seeded in a shared config before any
machine has the binary. A machine still on an older build ignores it and holds; a machine that has
caught up drops. There is no need to gate it per machine.

### A slow Full Disk Access denial is not a wedged server

```
$ zellij session doctor mysession        # before, on a healthy Mac without the grant
Needs you
  probe     the pane probe did not answer in time
            the pane was opened, so the server may be wedged
            the session domain and Full Disk Access could not be checked
```

Nothing was wedged. The macOS probe opens a floating pane that writes two lines to a file, and the
two are not worth the same. `manager=` is written the moment the pane's shell starts. `fda=` is
written after an `open(2)` on the TCC directory, and **TCC is fast to allow and slow to refuse**: on a Mac
that holds the grant the open returns at once, and on a Mac that does not it sits. Measured on a
rollout: `manager=` immediately, `fda=no` at about 5.5 to 5.8 seconds.

The deadline was five seconds, and the reader waited for a finished `fda=` line specifically. So
every Mac without Full Disk Access — the one machine the check exists for — was told its server may
be wedged, five seconds after it had already proved otherwise.

**Proof of life is the first line, not the last.** A finished `manager=` line says the server
accepted the `run`, made the pane, and the pane's shell ran. From that line on, `probe` cannot be a
Needs-you whatever becomes of the second answer, and the domain finding is reported from it as
before.

**The two lines now have two deadlines.** `PROBE_TIMEOUT` stays five seconds and now buys only
proof of life. `FDA_TIMEOUT` is ten seconds — comfortably past the observed refusal, and still a
bounded run. A finished `fda=` line ends the wait whenever it arrives, even past its own deadline:
a late answer is still an answer.

**A fourth answer, for a check that never finished.** `Option<bool>` became a four-state
`FullDiskAccess`: `Granted`, `Denied`, `Undetermined` (`fda=unknown` — there is no TCC directory
to open, so there is nothing to be refused by) and the new `Unanswered`.

```
$ zellij session doctor mysession        # after, when the second line never lands
Needs you
  fda       the Full Disk Access check did not finish in time
            the pane answered, so the server is serving - it is this one check that
            hung, and on macOS that is what a REFUSAL looks like: a machine holding
            the grant is let in at once, a machine without it waits to be told no.
            Read it as not granted until it answers. System Settings > Privacy &
            Security > Full Disk Access, and add the EXACT path the server runs.
```

**It is a Needs-you, the same as `fda=no`, and deliberately so.** The severity matches the answer
it most likely is: an `open(2)` that has not returned in ten seconds is an open that is being
refused, not one that is being allowed. Calling that "already correct" would put the machine the
check exists for back into the section a reader skips. It is worded as an unfinished check rather
than as a denial, so nobody acts on it as a fact — but it counts against the exit code, because a
doctor that exits 0 on a machine it could not clear is a doctor nobody has to read.

Everything else is untouched: `fda=yes`, `fda=no` and `fda=unknown` print exactly the sentences
they printed, `--dry-run` renders the same lines (the probe reads and changes nothing, so it runs
either way), and a probe that produces no line at all is still `TimedOut` in the same words.

The clock and the parser moved to `src/session_doctor_probe.rs`, which is compiled on every
platform while `session_doctor_macos` stays `#[cfg(target_os = "macos")]`. `probe_step(written,
elapsed)` is a pure function of the file's contents and the time since the client was spawned, so
the four cases that matter — answered fast, denied at six seconds, alive with no answer, and
nothing at all — are unit tests that run on Linux instead of behaviour only a Mac can reach. The
completeness rule the reader and the parser share lives there too, in one `finished_line`: a
trailing newline is what says a line is finished, and half of `fda=yes` used to parse as "could not
tell" on a machine that could have told.

Verified by compiling it, as [the probe's last
patch](#the-pane-probe-reports-what-the-client-said-instead-of-charging-five-seconds-for-it) was:
this fork's Linux box cannot cross-compile the tree to `aarch64-apple-darwin` (a vendored C
dependency, `libnghttp2-sys`, wants a macOS C toolchain), so the macOS module was type-checked by
ungating it and its two macOS-only imports for one `cargo check` and then putting the gates back.
The timings themselves came from a Mac, and are what the deadline is set against.

### Full Disk Access is asked of the system TCC directory

Every Full Disk Access probe used to open the per-user database,
`~/Library/Application Support/com.apple.TCC/TCC.db`. macOS 27 has no such file: the whole
`com.apple.TCC` directory under the user's Application Support is gone. So on a macOS 27 Mac every
probe found nothing to open, and `zellij session doctor` reported "the probe could not tell whether
Full Disk Access is granted" on a server that held the grant. The keybind bar's `fda` reading went
blank the same way, and the tab bar's `TCC` badge could never fire.

The probes now open the system TCC directory, `/Library/Application Support/com.apple.TCC`. Every
Mac has it, and its database has always needed Full Disk Access, so one target serves every macOS
version and there is no fallback to the old path. Measured on a macOS 27 Mac: from a pane of a
server holding the grant, `ls` lists the directory; from a process without it, `ls` fails with
"Operation not permitted". The per-user path answered "No such file or directory" either way.

All four probes moved together, because they must agree:

| Probe | Where | How it opens |
|---|---|---|
| doctor's pane probe | `src/session_doctor_macos.rs` | `[ -d ]`, then `ls` |
| keybind bar vitals | `default-plugins/slim-keybinds/src/vitals.rs` | `[ -d ]`, then `ls` |
| server's live check (`full_disk_access_granted`) | `zellij-utils/src/session_lifecycle.rs` | `read_dir` |
| startup probe (`probe_protected_locations_now`) | `zellij-utils/src/session_lifecycle.rs` | `read_dir` |

The open is still the question. Listing a directory calls `opendir`, a real `open(2)` that TCC can
refuse, not an `access(2)` on the permission bits. The three answers keep their meaning: a
directory that is not there is `unknown`, never a refusal. The doctor script no longer needs
`$HOME`, so its `eval echo ~user` fallback is gone.

### `zellij_create` hands its command to a shell, and refuses a `cwd` it cannot find

```
# before - MCP zellij_create {"cwd": "~/work", "command": "cd /tmp && echo \"a b\" $HOME | cat"}
action new-pane --cwd ~/work -- cd /tmp && echo "a b" $HOME | cat   # 9 argv words, no shell
  pane: Command not found: cd          # and the pane is in the session's directory, not ~/work
  tool: success

# after
action new-pane --cwd=/home/<user>/work -- /bin/zsh -c 'cd /tmp && echo "a b" $HOME | cat'
  pane: a b /home/<user>
```

**The command string was argv, and reading it as one was the whole bug.** The tool split it on
whitespace and the server execs what follows `--` itself, with nothing in between, so `&&`, a pipe,
a quote and a `$VAR` all became literal arguments of the first word. `cd x && y` reported `Command
not found: cd`; `echo "hello world"` printed the quotes. It is now handed to one shell as one word -
`$SHELL -c <command>`, and `/bin/sh` when `$SHELL` is unset or empty - so operators, quoting, globs,
`~` and `$VAR` mean what they mean in a shell. Nothing in the tool parses the command; the shell
does, and it is **non-interactive**, so no rc file, alias or line-editor rewrite gets a say.

**`cwd` promised more than it did.** `--cwd` reaches the server as a path and nothing expands it
there, so `~/x`, `$HOME/x` and a relative path were dropped without a word and the pane opened in
the session's directory - a wrong answer that looked like a right one. The tool now expands `~` and
`$VAR`/`${VAR}` against the MCP server's own environment, resolves a relative path against the
server's own directory, and passes an absolute path. One that is not a directory ends the call
before the CLI runs:

```
`cwd` /home/<user>/nope is not a directory.
```

**Success still means the pane was made**, and nothing more. What the command did is read back from
`zellij_wait_for` or `list-panes`, exactly as before.

**The cost, and it is visible: the pane's title and its `terminal_command` now name the shell** -
an unnamed pane read `/path/to/zsh -c sleep 30` where it used to read `sleep`. The tool now names a
command pane after its command (below); the consumer repo never makes command panes, so nothing
there reads either field.

CLI-side only: `src/mcp/invoke.rs` and one dependency the tree already carried (`shellexpand`). No
protobuf, no contract change, and `zellij action new-pane` itself is untouched.

### The MCP surface routes an agent to one tab, one pane, and honest exit codes

```
# before - zellij_create {"command": "cargo test"}
action new-pane -- /bin/zsh -c cargo test     # beside whichever pane the PERSON is focused on,
                                              # titled "/bin/zsh -c cargo test"
# after
action new-pane --in-tab=work-zj --no-focus --name='cargo test' -- /bin/zsh -c cargo test
```

**Placement was whatever the person happened to be looking at.** `zellij_create`'s `kind` had two
values, `pane` and `tab`, and `pane` ran a bare `new-pane` - which lands beside the **focused**
pane. So the same call landed somewhere different every time, and every time it shrank the pane the
person was reading. `tab` was the other half of the complaint: a tab per command.

`kind` is now `agent_tab | pane | tab`, defaulting to **`agent_tab`**. That is the agent's own tab,
named after the tab the MCP server's own pane is in with **`-zj`** on the end - `work` gives
`work-zj` - made the first time and reused after. A name that already ends in `-zj` is reused as it
stands, so an agent working inside one does not nest a second; a server started outside any pane has
no tab to be named after and gets `zj`. The name tells a person whose tab it is and that closing it
costs them nothing.

**It never takes the focus.** `--in-tab` places a pane without going there, and `--new-tab`
would switch the client to what it made, so the create passes `--no-focus` on both. Proved with a
client attached: the active tab and the focused pane are the same before and after, both when the
agent's tab is made and when it is reused.

`kind: pane` now passes `--near-current-pane`: beside the **agent's own** pane, not the person's.
`kind: tab` is unchanged.

Two consequences of `--new-tab` worth naming, both from clap: it refuses `--name`, so a pane that
arrives with its tab is named in a follow-up `rename-pane` keyed on the `pane_id:` the create
printed; and it refuses `--floating`, so `floating` with `agent_tab` is refused outright rather than
working or failing depending on whether the tab happened to exist.

**One create, and the tab is looked up rather than guessed at.** The call that finds the tab the
agent is in - `action list-panes --json` - already lists every pane with the name of the tab it is
in, so whether the agent's own tab exists is a second read of a payload the create was already
fetching. The create then runs once, `--in-tab=` or `--new-tab=` decided before anything ran. The
alternative was written first and does not work: asking with `--in-tab` and reading the refusal as
the signal to make the tab cannot tell the two exit **2**s apart, because the CLI exits 2 for a tab
nothing answers to *and* for a command line clap will not parse. A pane name beginning with a
dash was the second, and it made a **duplicate** `work-zj` tab beside the first - nothing in
the server dedupes tab names.

**Every value-taking flag is passed as `--flag=value`.** `--name --force` is a usage error;
`--name=--force` is a name. The tool builds `--name=`, `--cwd=`, `--in-tab=`, `--new-tab=` and
`--pane-id=` that way, so a value beginning with a dash - the command a pane is named after -
reaches the CLI as a value.

**Two agents can still both make the tab.** Their panes have to share a tab, so they resolve the
same `-zj` name, and one agent's own calls are serialized. Both then see no tab and both create
one; the server has no duplicate-name check. Two tabs of one name is the cost, and it is visible in
the tab bar.

**A command pane is titled with its command.** The shell wrap above is why it needed saying: the
pane's title was `/path/to/zsh -c cargo test`. The command is now the name, whitespace collapsed
and cut to 48 characters with an ellipsis. The caller does not choose a pane's name.

**`close_pane` and `close_tab` have their own tool.** Per-verb gating is the reason this
server exists at all - this file's own words, "an MCP client allows or denies each tool by name" -
so the two irreversible closes live in `zellij_close`, the one tool flagged destructive, and no
other tool offers a close. The cap test pins the surface shut.

**`close_tab` never worked.** It built `action close-tab-by-id --tab-id <n> --yes`, and that
command takes a positional `<ID>` and has no `--yes` - a clap error on every call since the tool
was written. Nothing caught it because the builder is pure and no test ran the CLI. `zellij_close`'s
`tab` now names the real argument, so the build-time check guards it like every other.

**A parameter may say what it is in the caller's terms.** Clap's help was inherited unconditionally,
and it is written for a command line: the rendered schema told an agent to put a command "after a
`--`", to pass keys "one per argument", to pass `-` to read text from stdin - which this server
nulls - and that a pane defaults to "the focused pane", which does not exist here. A non-empty
`describe` now wins, and `from` goes on guarding the flag's name, so drift protection is kept.

**Three smaller honesty fixes.** `zellij_wait_for`'s notes now say the thing that matters most on
this surface: its own `exit_code` answers whether the *wait* worked, and a command that failed still
returns 0 - the command's status is `exit_status` inside the result. `zellij_read_pane` says that a
held pane's `[ EXIT CODE: n ]` banner is frame chrome and is not in the grid it returns.
`zellij_overview`'s `Returns:` line promised the `list-panes` columns for all three scopes while
two of them run `list-agents` and `list-sessions`; it now names all three, and a `Returns:` line
that would repeat itself says the shared sentence once for the commands that share it. Naming
`list-sessions` found a second bug under it: the surface map's row for the sessions table was keyed
on the **alias** `ls`, while every reader looks a command up by the canonical path, so the row had
never matched anything - `--dump-surface` printed no `prints:` line for `list-sessions`, and the
tool description said scope=sessions returns nothing. The row is rekeyed, and a test now requires
every `OUTPUTS` row to name a command the tree has.

Two more description fixes in the same pass. `zellij_close` said both its verbs are confirmed for a
person and that the tool passes the confirmation; only `close_pane` has a `--yes`. `close_tab` has
no confirmation at all and takes every pane in the tab with it, which is what the tip now says.
And `zellij_create` claimed a session with no client attached cannot lay out a new tab - creating
is not focus-dependent, and a detached session makes a real tab.

CLI-side only: `src/mcp/`, and one row of `zellij-utils/src/cli_surface.rs`. No protobuf, no
contract change, and no CLI flag added - every flag this uses (`--in-tab`, `--new-tab`,
`--no-focus`, `--near-current-pane`) already shipped.

### `zellij_create` types its command into a ready interactive shell

```
# before - zellij_create {"command": "secret-cli read …"}
action new-pane --in-tab=work-zj --no-focus --name=… -- /bin/zsh -c 'secret-cli …'
  # `zsh -c` reads .zshenv only: no .zshrc, no aliases, no `mise activate`

# after
action new-pane --in-tab=work-zj --no-focus --name=… --wait-ready --ready-timeout=10
action write-chars --pane-id=terminal_7 -- 'secret-cli …'
action send-keys --pane-id=terminal_7 -- Enter
  # result: ready: true, ready_by: prompt, ready_ms: 212, input_sent: true
```

**A command now runs where a person would run it.** `$SHELL -c` is non-interactive, so a
command from the MCP tool missed everything the interactive rc chain sets up, and a command typed
by hand in the pane beside it did not. Two parameters change that, and the default is the new
behaviour:

| Parameter | Default | Effect |
|---|---|---|
| `run_in` | `shell` | `shell` makes a plain shell pane, waits for it to be ready, then types `command` and presses Enter through the same two verbs `zellij_write_input` uses. The pane stays a shell when the command ends. `exec` is the old behaviour, `$SHELL -c`, for a one-shot command whose exit status the caller wants from `zellij_wait_for until=exit`. |
| `wait_ready` | `true` | Passes `--wait-ready --ready-timeout=10` to the create. Applies in `shell` mode, with or without a `command`. `false` types at once, into a shell that may not be listening yet. |

- **A readiness timeout types nothing.** The create exits 2 and still names its pane; the tool
  reports `reason: not_ready`, `ready: false`, `input_sent: false` and the pane's `pane_id` and
  `handle`, so the caller can write to it later or close it. It is not reported as the ordinary
  "nothing matched" miss, because the pane is there.
- **A write that fails is the call's failure.** A pane that is ready and empty is not a command
  that ran.
- A created pane that waited carries `ready`, `ready_by` and `ready_ms` in the structured result;
  one that had a command to type also carries `input_sent`.
- `exec` ignores `wait_ready`: a one-shot command can end before it ever looks ready, which the wait
  would report as a miss.

The 10-second bound sits well under the request deadline an MCP client keeps. The tool cap went
from eight parameters to nine to make room; the alternative was folding `floating` into `kind`,
which breaks every caller that passes it.

`src/mcp/invoke.rs` (`Plan`, `TypeAfter`, `run_in`, the `finish` step of the create),
`src/mcp/mod.rs` (`type_into_created_pane`, the readiness fields in `report`) and
`src/mcp/tools.rs` (the two parameters, the tips, and a boolean default written as a boolean).
Tests: the shell-mode command lines for each `kind`; `exec` unchanged with either `wait_ready`; the
plan's write and Enter; nothing typed after a timeout; the `not_ready` and ready result shapes.

### `zellij_wait_for until=ready`: the command I typed has finished

```
zellij_write_input {"pane": "sunny-otter", "text": "cargo test"}
zellij_write_input {"pane": "sunny-otter", "keys": "Enter"}
zellij_wait_for    {"pane": "sunny-otter", "until": "ready", "timeout_s": 600}
  # action wait sunny-otter --for ready --timeout 600
```

`until` takes `ready` beside `exit`, `match` and `quiet`, and maps to `action wait --for ready`.
It is the wait for "the command I just typed into a shell has finished", which `exit` cannot be
(the shell does not exit) and `quiet` only guesses at (a silent build is quiet).

Ready means the shell drew a prompt AFTER the last input typed into the pane. The prompt still
on screen right after the typing does not count, so a wait started a moment after
`zellij_write_input` does not answer before the command has even begun; see [`--for ready` after
typing a command](#--for-ready-after-typing-a-command-the-prompt-must-be-fresh). `ready_by` says
which signal answered: `prompt`, or `quiet` for a shell without OSC 133 markers, where `quiet_ms`
sets the window. The tool's tips say so.

No parameter was added: `zellij_wait_for` keeps its six. `src/mcp/tools.rs` (the enum value, the
summary, the tips and the `quiet_ms` note) and `src/mcp/invoke.rs` (the accepted value). Tests:
the command line for `until=ready`, with and without `quiet_ms` and `timeout_s`, and the refusal
that names the four values.

### A refreshed pin is no longer mistaken for the build the server is running

```
$ zellij session enable mysession        # refreshes the pinned copy to the new build
      refreshed the pinned copy at /Users/<user>/.local/share/zellij/bin/zellij
$ zellij session doctor mysession        # before
Already correct
  build     the running server is this build of zellij
```

It was not. The server had been serving since two days earlier, on the previous build, and a
restart was still owed. Seen on a rollout, twice, and it is the worst answer this report can give:
the `build` line is the one an operator and an agent both read to decide whether a rollout
finished, so a false clean there ends the rollout one step early and leaves the machine on the old
server with no signal at all.

**Nothing was wrong with the check's shape.** [The stale-build
warning](#a-warning-when-the-running-session-is-a-different-build) asks the live process, by pid,
which is the right question: `/proc/<pid>/exe` on Linux, `proc_pidpath` on macOS. It was wrong one
level down, at the step everything here shares — the answer is a PATH, and the build is then read
off **the file that path now holds**. That file is the running build only while nobody has written
over it, and `session enable` writes over it: it renames the new build onto the pinned path under a
live server, so from that moment the path holds a build the server is not running. Doctor compared
the new pin with itself and found, correctly, that it was itself.

Linux escaped it by accident. `/proc/<pid>/exe` gains a " (deleted)" suffix once the file is renamed
away, and `identify_executable` has always read that. **macOS says nothing** — `proc_pidpath`
reports the path, and the path holds the replacement — which is why the two Linux machines reported
the difference correctly and the Mac did not. The other Mac reported it correctly too, for a reason
that is worth knowing rather than trusting: there the refresh happened inside `doctor --fix`, and
`check_build` runs before `check_pin`, so the pin was read in the one moment it was still the old
build.

**The reference time is the session's own socket.** The server binds it as it comes up and nothing
writes it again, so an executable modified after that socket was bound is not the file the process
started from — whatever the platform does or does not say about it. `server_executable` now asks
that question and, when the answer is yes, throws away everything it read off the file, keeping the
path and setting the `replaced` flag that Linux's " (deleted)" already sets. Every reader downstream
is unchanged and correct from there: `compare_builds` reports `Different`, `ls --json` carries
`server_exe_replaced: true`, the client's stderr warning fires, and doctor's `build` line says a
restart is owed.

It costs one `stat`, spawns no process and parses no date — the alternatives were `ps -o lstart=`
per session with a date parser, or `proc_pidinfo` FFI that this fork's Linux box cannot compile. The
order it depends on is causal rather than lucky: `session up` refreshes the pin and **then** starts
the server, so a pin the running server did start from is always older than that server's socket.
Both ways of not knowing answer "not replaced" — a socket or an executable that cannot be stat'ed
leaves the identity exactly as it was read, because a wrong "your session is stale" sends someone to
restart a session that was fine.

What it answers exactly is "this file is not the one the process started from", so a rewrite that
did not change the build would be reported as a build that did. That is bounded by the pin having
one writer, which refuses to write unless the build differs — signing included, since the signing
transaction runs only as part of a refresh. Nothing in the steady state moves that mtime: the
watchdog's `session up` hands the pin to itself every minute and returns without touching it.

`zellij-utils/src/session_lifecycle.rs` only, and no new dependency. Three unit tests: the refresh
under a running server, which first asserts that the files alone read as `Same` and then that the
check calls it what it is; the ordinary session whose pin predates its socket; and a socket that
cannot be read. An overwrite **in place** needs no test — the kernel refuses it with `ETXTBSY`
while the server is executing the file, which is why the pin's writer renames.

**A missing file is a replaced one too (nkmk.25, 2026-09-29).** The mtime rule needs a file to read.
When an upgrade removes the server's executable outright, macOS marks nothing with a
` (deleted)` suffix, and the identity came back with nothing read and `replaced` unset.
`compare_builds` then said `Unknown`, and nothing warned. `server_executable` now treats a path that
does not exist as replaced, on every platform, so `ls --json` says `server_build: different` with
`server_exe_replaced: true`, and doctor's `build` line says a restart is owed. The rule sits in
`server_executable` and not in `identify_executable`, whose other callers may identify a path that
is legitimately absent. On macOS, `proc_pidpath` fails with `ENOENT` once the file is unlinked,
so without more the server had no path at all and the rule never ran. The fallback reads the
exec-time path from `KERN_PROCARGS2`, which the kernel keeps for the life of the process, and that
keeps the answer. Unit tests: the same identity before and after its file is removed; the
`KERN_PROCARGS2` parse, on every unix; and, on macOS only, a deleted copy of `/bin/sleep` that is
still named by its exec path.

### A release candidate never becomes the pin

```
$ zellij session up mysession        # from a candidate, with the rc formula linked
warning: /Users/<user>/.local/share/zellij/bin/zellij was left alone: this build is a
         release candidate (0.45.1-nkmk.23-rc.1), and the pin is the build a restart comes
         back on.
```

AGENTS.md is unambiguous about what an RC tag is for: **proof, never an install target.** The pin
did not know that. `install_pinned_exe` asks one question about the build in hand — is it different
from the pin — and a candidate is certainly different, so proving one on a machine copied it onto
the path the launcher runs and a restart comes back on. The next reboot then brought a session up on
a build that was never released, and nothing said so; the RC formula is unlinked at the end of a
proof, which takes the candidate off `PATH` and leaves it exactly where it does the most harm.

**It is asked at the pin's one writer**, beside the rule that an anchored signature is never
overwritten, and for the reason that rule is written there: a rule a caller can be written without
is a rule that will be written without. Three callers reach it — `session up` and `session enable`,
`session doctor --fix`, and `server_exe_for_interactive_launch` on every interactive launch — and
none of them can ask for anything else, because there is no parameter to ask with.

- The line is printed **once per pin per process**, like the signing refusal, because `session up`
  asserts the pin and then launches a client that resolves the server binary through it again.
- It is asked **after** the staleness checks, so a candidate whose build already is the pin still
  reports `UpToDate` and says nothing. The line is for a write that was prevented.
- It is asked **before** the signing guard, so a candidate never opens a keychain dialog for a
  build that is not going to be installed whatever the answer.
- **An interactive launch from a candidate serves the session from the candidate**, not from the
  pin. Falling back to the pin would quietly start the *previous* build under an operator who is
  holding a candidate and watching for its behaviour — a proof of the wrong thing.
- **`session enable` refuses** when it would have created the pin and there is nothing there yet. It
  is about to write a unit naming that path, and a launcher pointed at a file that does not exist
  fails at boot, where nobody is watching.
- Doctor reports it in `Already correct`, not `Needs you`: nothing is waiting on a person, and an
  exit code saying otherwise would make a doctor nobody reads.

`-rc.` is the whole test. `release.yml` patches the entire tag into `Cargo.toml` before it builds —
that is what makes an RC binary report `0.45.1-nkmk.23-rc.1` rather than the version it is heading
for — so the suffix is guaranteed rather than conventional, and it is the one thing that separates a
candidate from the release it is a candidate for. Unit tested both ways, including the spellings
(`-rcx`, `-rc-`) that must not be mistaken for one.

This does not make the pin safe from every build. Any unreleased binary that creates a session takes
the pin the same way — a developer's local build included, which is how this was reproduced. The
guard covers the case the release process actually walks into.

**`doctor --fix` was a door the refusal did not cover, until nkmk.33.** Over an anchored macOS pin,
doctor hands the refresh to the signing transaction (`refresh_belongs_to_signing`, the deferred
path). That transaction writes the pin itself and never reaches `install_pinned_exe`, so it never
asked the candidate question. An RC carries the release's Developer ID, so the transaction took it,
and `doctor --fix` from an RC put the candidate at the pin. The claim above that doctor is covered
was wrong for that case.

Now the deferral itself asks. `refresh_belongs_to_signing` takes the candidate answer and does not
defer for a refused candidate; the refresh goes to `install_pinned_exe`, which refuses it, and
doctor reports `… was left alone: this build is a release candidate` in `Already correct`. The
signing check still judges the pin as it is. Both doors ask one predicate,
`release_candidate_refused_for`, so they cannot disagree again. Doctor's `--dry-run` asks it too.

**One override, for the RC proof's scratch pin.** The release proof refreshes a scratch copy of the
real pin from the RC, to prove the as-is signature path before a release exists. With the door
closed that proof needs a way back in, and it is deliberately narrow:

```
ZELLIJ_PIN_ALLOW_RC=1 zellij session doctor --fix proof-session   # with pin_exe "/tmp/…/zellij"
warning: pinning release candidate 0.45.1-nkmk.33-rc.1 at /tmp/…/zellij: ZELLIJ_PIN_ALLOW_RC=1 is
         set and the pin is outside this account's home directory.
```

- It needs `ZELLIJ_PIN_ALLOW_RC=1` **and** a pin outside the account's home directory.
- The home directory comes from the passwd database, not from `HOME`: a proof points `HOME` at a
  scratch directory, and the real pin lives under the real home.
- Every spelling is checked, as given and resolved through symlinks. A link outside the home that
  leads into it is the home, and so is a link inside the home that leads out of it: a `~/.cache`
  or `~/.local/share` on another disk still holds the account's own paths.
- It applies at every door, `session up` included, and says so once per pin per process.

`zellij-utils/src/session_lifecycle.rs` (`release_candidate_refused_for`, `candidate_may_pin`,
`PIN_ALLOW_RC_ENV`), `zellij-utils/src/session_signing.rs` (`refresh_belongs_to_signing`'s new
`candidate_refused`), `src/session_doctor_macos.rs` (`deferred_refresh`) and
`src/session_doctor_command.rs` (the dry-run line). Tests: a candidate is never deferred to signing
over an anchored pin; the override's rule (told or not, a scratch pin, the real pin, no known home,
a link into the home, a link out of it).

### A pane that dropped to the shell stops naming the command that exited

```
$ zellij action list-panes --all --json | jq '.[] | {handle, terminal_command, is_held, exited}'
{"handle":"sunny-otter","terminal_command":"sh -c npm test","is_held":false,"exited":false}  # before
{"handle":"sunny-otter","is_held":false,"exited":false}                                      # after
```

The pane had finished `npm test` and fallen back to the shell. `is_held` and `exited` said so, and
`terminal_command` named a command that was not running — which is the field a consumer keys on to
decide what a pane is doing, so a pane at a prompt read as a pane still working. True of an
ESC-dropped pane as long as ESC has offered the drop; [the clean-exit
drop](#a-command-pane-can-drop-to-the-shell-when-its-command-exits-cleanly) only made it the common
case.

The pty swapped the dead command for the default shell and nothing told the pane, so `invoked_with`
— which is where `terminal_command` comes from — still held the `Run::Command` the pane was opened
with. `Tab` now records the drop on the pane at both sites that send
`PtyInstruction::DropToShellInPane`: the ESC key handler, and the clean-exit path.

**`None` rather than the name of the shell.** That is what an ordinary shell pane carries — a pane
opened with the default shell is normalized to `None` at creation — and after the drop this is an
ordinary shell pane. Both callers hand the pty the tab's own `default_shell`, so there is no other
shell it could be. A consumer reading `terminal_command` for "what is this pane running" now gets
the same answer for a dropped pane as for a shell pane that never ran anything, which is the truth.

**The snapshot does not change, and was already right.** `update_terminal_commands` overwrites every
pane's recorded command with what the OS says its child process is running before a layout is
serialized, and already wrote `None` for a pane sitting in the default shell — as [the resurrection
entry](#a-resurrected-command-pane-drops-to-the-shell-when-its-command-exits-cleanly) says in as many
words. This patch brings the live report into line with what the snapshot has always recorded.

**`command_state` was considered first and is not a substitute.** A shell that emits OSC 133 does
answer this — it draws a prompt after the drop, so `command_state` reads `prompt` — but the key is
**absent for a shell that emits no markers**, which is exactly the consumer's fallback case, and a
field that names a command that is not running is wrong whether or not another field is right.

`zellij-server` only: a `set_invoked_with` on the `Pane` trait, defaulting to a no-op for plugin
panes, and two call sites. No protobuf, no contract change — `PaneInfo::terminal_command` already
exists and this changes only what fills it. Two tab unit tests: a dropped pane names nothing, and a
pane that still holds still names its command, because that one can be re-run with ENTER.

### Doctor keeps the certificate the grants name, and remembers a switch until the re-grant is made

**Removed in nkmk.30.** The certificate half went with the ladder. The `regrant-owed` record and
`--regranted` stay, and the as-is release install writes it. See [The per-Mac signing ladder is
gone](#the-per-mac-signing-ladder-is-gone-a-release-is-signed-once-in-ci).

Fixed in nkmk.27; seen on a Mac at nkmk.26. The pin was anchored on our own minted certificate. The user then added
an Apple Development identity to the keychain. The next `session doctor --fix` refreshed the pin and
signed it with the Apple identity, and asked for Full Disk Access, Accessibility and Screen
Recording to be granted again. Seven minutes later a `--dry-run` said the grants "still hold". Two
faults, one in each run.

**The ladder dropped our certificate whenever an Apple one was offered.** `sign_down_the_ladder`
removed the self-signed rung if any Apple rung existed, whatever the pin was anchored on. That rule
is right for a pin that was never anchored. It is wrong for a pin whose grants name our certificate:
our certificate still signs, and signing with it keeps every grant. `ladder_for_the_anchor` now
reads the requirement the grants were made against. When that requirement names an offered
certificate by hash (`certificate leaf = H"<sha1>"`, the same SHA-1 `find-identity` prints), that
rung is the only one tried. A refusal from it is a `Needs you`. It is not a fall to the Apple
certificate, by the same rule that stops a fall to another team. When the keychain no longer offers
that certificate, doctor still switches to the best rung left, and says so.

**Nothing recorded the switch, so the next run compared the pin with itself.** The run that signed
compared the requirement before and after, and asked for the re-grant. The next run had only the
pin. A pin always satisfies its own requirement, so doctor called the grants held. Now a switch is
written to `~/Library/Application Support/zellij/signing/regrant-owed`. The file holds the
requirement the grants were made against: the old one, not the new one. Every later run that finds
the file reports `Needs you` with both requirements and the three steps:

```
Needs you
  signing   <pin> is signed as org.zellij.nkmk, and the grants were made against another requirement
            now:     designated => identifier "org.zellij.nkmk" and anchor apple generic and ...
            granted: designated => identifier "org.zellij.nkmk" and certificate leaf = H"..."
            ...
            1. re-grant Full Disk Access, Accessibility and Screen Recording for <pin>
            2. `zellij session doctor --fix --regranted`, so doctor records it as made
            3. THEN `zellij session restart`
```

Two things clear the record:

- **`zellij session doctor --fix --regranted`.** The operator says the grants were made again.
  Doctor takes the flag as the proof, because the grants live in a TCC database that only a process
  with Full Disk Access can read. With `--dry-run` the flag reports what it would clear and clears
  nothing.
- **A pin that satisfies the recorded requirement again.** Doctor asks `codesign --verify
  -R=<requirement> <pin>`, which is the test macOS makes when it reads a grant. A later refresh
  signed back onto the granted certificate passes it, and the record goes.

The same `-R` test now decides whether a change of requirement owes a re-grant at all. A Developer
ID and an Apple Development certificate of one team write different texts, and both satisfy the
team requirement. A run that switched between them asked for a re-grant on the text alone. It now
asks only when the new signature fails the old requirement. A test that cannot run counts as a
failure, so doubt reports the re-grant as owed.

A pin that was never anchored (ad-hoc or unsigned) writes no record. Its first anchored signature
still asks for the grants once, as before.

`zellij-utils/src/session_signing.rs`, plus the `--regranted` flag through `cli.rs`,
`session_commands.rs` and `session_doctor_command.rs` into `PinSigningPolicy`. The decisions are
pure functions (`requirement_names_certificate`, `ladder_for_the_anchor`, `judge_grants`) with their
own tests. Four `sign_pin` tests drive the Mac's flow with a recorded `codesign`: the nkmk.26
keychain keeps our certificate; a refusal from it does not fall to the Apple one; a forced
switch stays owed through a later dry run until `--fix --regranted`; and a pin back on the granted
requirement clears the record, but only in a run that may act.

### A held pane forgets its dead pid

```
$ zellij run -- sh -c 'sleep 1'     # exits, and the pane is held
$ zellij run -- sleep 999           # its pty takes the held pane's fd number
$ zellij action list-panes --json | jq -c '.[] | select(.is_held) | {terminal_command, pane_pid}'
{"terminal_command":"sh -c sleep 1","pane_pid":null}
$ zellij action dump-layout | grep -A1 'name="sh -c sleep 1"'
pane command="sleep" name="sh -c sleep 1" ... {             # before: the other pane's program
    args "999"
pane command="sh" name="sh -c sleep 1" ... {                # after: its own
    args "-c" "sleep 1"
```

From nkmk.31. Seen on a Mac at nkmk.31-rc.1: a held claude pane serialized as another pane's
program, and `list-agents` still listed it with its dead pid. A restore would have brought the
pane back running the wrong command.

**The held pane never forgot its pid.** The thread that reaps a held pane's child tells the pty
thread to forget the pid (`PtyInstruction::ChildProcessExited`, since `19542ba8b`). It sends on the
pty thread's own bus, and that bus had no sender to the pty thread: upstream's wiring, because
upstream's pty thread never messaged itself. The send failed, the failure was ignored, and
`forget_child_pid` never ran outside a unit test. nkmk.30 had the same fault.

**What the stale pid leaked.**

- **Another pane's command into a restore.** Serialization still counted the held pane as live and
  asked its pty fd for the foreground process group. That fd had closed with the process, and the
  next pane's pty reuses the lowest free fd number - so the held pane answered with the newer
  pane's argv. A pane with no later pane did not leak, which is why this hid.
- **A signal to a recycled pid.** `close-pane` sent SIGHUP to the dead pid, session teardown sent
  SIGHUP then SIGKILL to its group. Once the OS hands that pid out again, either can kill an
  unrelated process.

**The fix.** The pty thread's bus carries a sender to itself (`pty_thread_bus` in `lib.rs`, built
in one place so a test builds the same bus). On `ChildProcessExited` the pane also forgets its pty
fd, so a reused fd number is never read as the held pane. A resize of a held pane then logs one
non-fatal line; a re-run records a fresh fd.

**The held pane keeps its agent's session.** With no process, a held agent pane would serialize
with its picker. The pty thread now freezes, per pane, the session id it last resolved while the
process lived - the `OSC 1337` report, claude's record, the open file, or an identity variable -
with its source. Serialization records `--resume <that id>`. The picker remains the answer only
when nothing was ever resolved. The frozen id goes when the pane re-runs or closes.

**Visible change.** A held pane reports `pane_pid` null and no `pane_command`; `pane_cwd` stays.
`list-agents` still lists a held agent pane, by the command it was started with, with `pid: null`
and the frozen `agent_id` and `session_source` as they were. No new JSON field: `pid: null` already
says the pane is held.

`zellij-server` only: `lib.rs` (the bus) and `pty.rs` (`HeldPane`, `forget_child_pid`, the pin pass,
the process report). Six `pty_tests`: the bus from `pty_thread_bus` delivers the reaper's message,
and the existing forget test now runs on that bus; a held pane serializes its own command when its
fd answers for another pane; a held agent pane records its reported session, its tick-found
session with its source, or its picker when it had none; and a held pane lists with no pid and no
command.

The integration harness's fake pty modelled a cleared terminal as a gone one: its
`clear_terminal_id` drops the terminal's whole state, so a held pane's re-run panicked with
"re-run for unknown terminal id" (`send_command_through_the_cli`). A real pty only forgets the fd
and records a fresh one on the re-run, so the fake's re-run now makes the state again from the
command it is given.

### `OSC 9;4` progress reaches the terminal, for the pane you are looking at

```
$ printf '\e]9;4;3\a'; sleep 5; printf '\e]9;4;0\a'   # Ghostty draws its bar, then clears it
```

From nkmk.32. A program reports progress with ConEmu's `OSC 9;4;<state>[;<value>]`: 0 clear,
1 a value from 0 to 100, 2 error, 3 indeterminate, 4 paused. Claude Code sends it, and Ghostty,
Windows Terminal and ConEmu draw it as a bar. Upstream drops it on purpose: the `OSC 9` arm treats
every ConEmu subcommand as unsupported, so the bar never showed, not even locally.

**One bar per window, many panes per client.** Each terminal pane keeps the last report it got,
and a counter of reports. After each render the server sends every client the report of the pane
that client has focused:

- **A new report from the focused pane** is forwarded, a repeat of the same state included. A
  program refreshes its report to keep a bar alive, and a host may drop a bar that is not
  refreshed.
- **Focus moves to another pane or tab:** the client gets that pane's report, or `9;4;0` when it
  has none.
- **The pane's program exits:** the pane drops its report, as if the program had sent its own clear.
  A closed pane takes its report with it, and focus moving off it does the rest.

**A clear is sent only over a bar.** A client is sent `9;4;0` only when it was last sent a bar. A
client whose panes never report progress is never sent an `OSC 9` from this path, because some
terminals show any `OSC 9` as a desktop notification.

**The render path, not the contract.** The report rides the render output as a post-render
instruction, the way a forwarded bell and a desktop notification do. Nothing crosses the
client/server contract, and watcher clients are left out.

Not covered: a client that detaches leaves its host with whatever bar it was last sent, until
the host times the bar out.

`zellij-server` only: `terminal_progress.rs` (new: the parse and the per-client decision),
`panes/grid.rs` (the `9;4` arm, `set_progress`, `clear_progress`), `panes/terminal_pane.rs`
(`Pane::progress`, the clear on `hold`), `tab/mod.rs` (the trait default) and `screen.rs`
(`sync_progress_to_clients` in `render_to_clients`). Eight `terminal_progress` tests, four grid
tests and two screen tests: the focused pane's report reaches the client and a quiet render sends
nothing; a report in another tab waits until that tab is focused, and focusing back clears the bar.

### `trz` and `tsz` work through zellij behind `tssh`

```
::TRZSZ:TRANSFER:R:1.2.0:1760180000000:41235    # what trz prints
::TRZSZ:TRANSFER:R:1.2.0:1760180000020:41235    # what the pane's grid holds
```

From nkmk.32. [trzsz](https://github.com/trzsz/trzsz-go) starts a transfer with a plain-text
trigger, not an escape sequence: `ESC[s::TRZSZ:TRANSFER:<mode>:<version>:<id>:<port>`. The
client - `tssh`, or a trzsz-aware terminal - watches its output for it and opens a transfer.

**zellij re-sent the trigger.** It prints the trigger onto the grid like any other text, so every
later redraw of that row sends it to the client again. The file arrived, then the next redraw
started a second transfer, and the client typed its failure (`#fail:...`) into the shell. This is
trzsz-ssh issue #135.

**trzsz already knows a multiplexer's mark.** The client starts a transfer for an id ending `00`
every time it appears, and starts any other id once. Inside tmux, `trz` adds 20 to its id, and the
trzsz relay rewrites a trailing `00` to `20` (`rewriteTrzszTrigger` in trzsz-go's `comm.go`).
zellij now does the same rewrite on a pane's output, before the parser sees it, with the same
pattern: `::TRZSZ:TRANSFER:[SRDF]:<x.y.z>:<13 or more digits>`. The tunnel handshake drops the
id's last two digits, so the rewritten id still connects. An id ending `20` or `10` is left alone.

Not covered: a trigger split across two pty reads is not rewritten. `trz` writes it in one call.
A transfer with no tunnel - `trzsz ssh`, or a terminal's own trigger - still fails with "Receive
data timeout"; that needs a raw byte channel through the client/server contract, which is not
worth its cost.

`zellij-server` only: `trzsz.rs` (new) and the call at the top of `TerminalPane::handle_pty_bytes`.
Six `trzsz` tests, and a terminal-pane test that the trigger lands on the grid with the rewritten
id.

### A build copied over the pin no longer reads as current

```
$ cp <other build> ~/.local/share/zellij/bin/zellij
$ zellij session status mysession
pin       /home/<user>/.local/share/zellij/bin/zellij was replaced outside zellij - `zellij
          session doctor --fix`, run from the installed build, refreshes it
$ zellij session up mysession
warning: /home/<user>/.local/share/zellij/bin/zellij was replaced outside zellij: it is not
         the file zellij wrote from the build its stamp names, so it is treated as stale.
      refreshed the pinned copy at /home/<user>/.local/share/zellij/bin/zellij
```

From nkmk.33. The stamp, `<pin>.source-sha256`, says which source build the pin was made from. It
said nothing about the pin itself. A `cp`, `mv` or `install` of another build over a stamped pin
left the stamp vouching for the replacement, and `session up`, `session status`, doctor and every
interactive launch treated that pin as current. The nkmk.33 RC proof hit this: an RC-stamped
scratch pin with an older build copied over it was never refreshed.

**A third file now records the pin itself: `<pin>.pin-identity`.** Both writers write it after the
stamp: `install_pinned_exe` and the macOS signing transaction (`record_pin_refreshed_from`). One
line holds:

- the stamp hash it was recorded beside;
- the pin's SHA-256;
- the pin's `stat` identity: device, inode, size, mtime to the nanosecond;
- the stamp file's own `stat` identity.

**The check costs two `stat`s on nearly every pass.** A pin whose `stat` matches the record is the
file zellij wrote. Only a pin whose `stat` moved is hashed, once:

| The pin's `stat` | Its bytes | Result |
|---|---|---|
| matches | not read | the file zellij wrote |
| moved | same as recorded (`touch`) | the file zellij wrote; the writer records the new `stat` |
| moved | different | replaced outside zellij: stale |

`cp` writes in place and moves the mtime. `cp -p` keeps the mtime but moves the size unless both
builds are one length. `mv` and `install` bring a new inode. The blind spot is the one the source key
already accepts: a writer that keeps device, inode, size and mtime together.

**A replaced pin is handled like a stale one.** `install_pinned_exe` refreshes it, and still asks
the release-candidate and anchored-signature guards first. `pin_needs_refresh` reports it, so
doctor's dry run and the macOS deferred refresh agree with the fix. The writer says the line once
per pin per process. `session status` reports it and exits 1. Doctor's dry run adds a note.

**An old stamp is unverified, not broken.** The record is believed only when it names the stamp in
force AND the stamp file's `stat` still matches. Otherwise the stamp alone decides, as before:

- No record: a pin stamped by nkmk.32 or older.
- A record whose stamp was rewritten since: an older build refreshed the pin, or signed it in place
  and re-stamped it, without re-recording. Trusting that record would call a pin zellij wrote
  "replaced" and copy over it.

An unverified pin gets its record at its next write, which is the next release. The writer no
longer rewrites a stamp that already holds the right line, so a pass that finds the pin current
keeps the record bound.

`zellij-utils/src/session_lifecycle.rs` (the record, the check, both writers), the status line in
`src/session_commands.rs`, and the dry-run note in `src/session_doctor_command.rs`. Six tests: an
in-place `cp`; a rename with size and mtime kept; a `touch`; no record; a record from before a
re-stamp; a current pass keeps the record valid. Three signing tests now record their simulated
signature through `record_pin_refreshed_from`, as the signing transaction does.

### An image goes to the client as the PNG the program sent, when that is smaller

```
\e_Ga=t,q=2,f=32,t=d,i=2000000000,s=410,v=520,m=1;...        # before: raw RGBA, 1,140,720 bytes
\e_Ga=t,q=2,f=32,o=z,t=d,i=2000000000,s=410,v=520,m=1;...    # zlib, for a client that takes it: 277,554 bytes
\e_Ga=t,q=2,f=100,t=d,i=2000000000,m=1;...                    # nkmk.34: the PNG, 243,527 bytes
```

From nkmk.33. zellij decodes every kitty image a pane sends into RGBA, and sends that RGBA on to
each kitty-capable client as base64. A program that sent a 180 KB PNG still cost the client 1.1 MB
for a 410x520 image, every time a client needed it. Over ssh or `tssh` that is the slow part of
showing an image.

**Upstream v0.46.0 now compresses**, for a client that confirmed it takes zlib
(`KittyHostCapability.zlib`), and sends raw RGBA to any other. Its header order is
`f=32,o=z,t=d,...`. The fork adds no compression rule of its own. The payload is cached per image,
per scaled size and per compression.

Measured with a fake kitty host on a pty, showing a 410x520 crop of a real screenshot sent as
`f=100`: the transmit fell from 1,140,720 bytes to 277,554, and the client decoded the same RGBA
that went in.

From nkmk.34, a PNG a program sent (`f=100`) goes through as that PNG. The parser keeps the PNG
beside its decode when the PNG is smaller than the RGBA. On the first transmit of the image at its
own size, the server compares the PNG with the payload that client would get, and sends the
smaller one. A PNG goes as `f=100` without `s`/`v`, because a PNG carries its own size. The image
above fell from 277,554 bytes to 243,527 (-12%), the payload was byte-identical to the program's
PNG, and it decoded to the same RGBA.

The RGBA stays in the store: placing an image at a cell size (`c`/`r`) crops and scales it, and a
resize does it again. So a scaled placement still goes as RGBA, and only the image at its own size
can use the PNG. The PNG is the extra copy, and it is held only until that first
transmit: the transmit moves it into the cached payload and drops it. While held it counts against
the store quota. After it, the image costs what it cost in nkmk.33, a little less, because the
cached base64 is the smaller PNG. An image never sent at its own size keeps its PNG until the image
is freed.

`zellij-server` only: `DecodedImage.png` in `kitty_graphics/parser.rs`, `source_png`,
`TransmitPayload::or_png` and `transmit_payload_for` in `kitty_graphics/store.rs`, and
`emit_kitty_transmit` in `output/mod.rs`. Output tests: the PNG header without `s`/`v`, and a
410x520 PNG that goes in through the parser, comes out as `f=100`, and decodes through the parser to
the same pixels. Store tests: the PNG counts against the quota until the first unscaled transmit, a
scaled transmit leaves it, and a PNG larger than the zlib payload loses. A parser test: a PNG
smaller than its RGBA is kept.

### A floating pane learns what the attached terminals can draw

From nkmk.33. `Tab::new_floating_pane` never applied the tab's kitty and sixel host support to the
pane it made. Every other way of making a pane does. A floating pane therefore kept the grid's
default, "kitty supported", and answered a kitty query `OK` in a session no attached terminal could
draw one in. The fix is the same two `if let` lines the other paths have.

`zellij-server/src/tab/mod.rs` (`new_floating_pane`).

### opencode reports its state from its TUI plugin

From nkmk.34. opencode 2.0 and 1.x do not speak `OSC 7501`, so `list-agents` showed no `state` for
an opencode pane. The opencode TUI plugin from `zellij setup --agent-hooks`
(`~/.config/opencode/plugins/agent-session/tui.ts`) now reports it, the way the codex hook does for
codex:

| opencode event | v2 (`context.data.on`, body in `data`) | v1 (`api.event.on`, body in `properties`) | report |
|---|---|---|---|
| a turn starts | `session.execution.started` | `session.status` `busy` or `retry` | `working` |
| a permission is asked | `permission.asked` | `permission.asked` | `blocked`, `permission`, the request as `msg` |
| a question is asked | `form.created` | `question.asked` | `blocked`, `question`, the question as `msg` |
| it is answered | `permission.replied`, `form.replied`, `form.cancelled` | `permission.replied`, `question.replied`, `question.rejected` | back to the state under it |
| the turn ends | `session.execution.succeeded`, `.interrupted` | `session.status` `idle` | `done` |
| the turn fails | `session.execution.failed` | `session.error` | `error`, the message as `msg` |

**The TUI, not the server.** The v2 server is a background service shared by every pane, with no
pane terminal of its own. The TUI runs in the pane and already writes the `OSC 1337` session
report to `/dev/tty`, so it writes the `OSC 7501` there too. Both versions ship a builtin
notifications plugin in the TUI that listens to the same events; this one mirrors it.

**What the pane shows is the session the TUI shows, with its subagents.** A TUI on a shared
service hears every session, so each event is filed under its top session (v2:
`data.session.root`; v1: up the `parentID` chain through `api.state.session.get`). The plugin
tracks every top session it hears of, not only the one on screen: a first prompt starts its session
running before the TUI shows it, and a first version that ignored unseen sessions missed a whole
turn that way. When the TUI moves to another session, the pane gets that session's state, or a
`clear` for a session that has not run. Within one session `blocked` wins over `working`,
`working` over `error`, and `error` over `done`. Only the top session's failure is an `error`;
a user's abort (v2 `interrupted`, v1 `MessageAbortedError`) is `done`.

**A report is sent only when it changes, and only inside zellij.** The protocol's query is not
needed: zellij takes a report without one, as it does from the codex hook. The text is flattened
to one line and cut to 400 characters before it is encoded, because zellij discards a whole report
whose `msg` holds a control character, and a shell command in a permission request often does.

Proven on Linux in a scratch zellij session, with the plugin text from this build installed in
scratch opencode config directories, with `bash` set to `ask`:

- **opencode 2.0.25** (`--standalone`; its model credentials are reachable only through its own
  data directory, so that one was the real one, and the test sessions were deleted after):
  `list-agents --json` went `working`, `blocked` / `permission` / "shell: echo hi", `working` after
  "Allow once", then `done`, with `state_app` `opencode`.
- **opencode 1.19.0**, every directory scratch, a free model: the same four states ("bash: echo
  hi"), then a question-tool prompt gave `working`, `blocked` / `question` / "red or blue?",
  `working` after the answer, and `done`.

`error` is not proven live. The one live failure, a deprecated default model, came before the
race fix above and reported nothing. The `error` path is covered by running the plugin against
fake v1 and v2 hosts.

Only the TUI snippet changes. The server snippet, `agent-session.sh` and the zellij side are as
they were. A machine picks this up by re-installing the TUI plugin file. Tests: the snippet test
checks each version's events, the zellij-only guard, the states and kinds, the control-character
flattening and the per-session tracking.

### `zellij_wait_for until=agent_idle`: wait for an agent to stop working

From nkmk.34, from the agent-orchestration plan. A session that drives agents in other panes had to
poll `list-agents` and read `state` itself, and the first poll after sending a message read the
`done` of the turn before.

```
zellij_write_input {"pane": "sunny-otter", "text": "Run the tests and say what failed.", ...}
zellij_wait_for    {"pane": "sunny-otter", "until": "agent_idle", "timeout_s": 900}
  # action wait sunny-otter --for agent-idle --timeout 900
waited_ms: 48210
idle_by: state
state: done
```

`until` takes `agent_idle` and `agent_waiting`, and maps to `action wait --for agent-idle` and
`--for agent-waiting`. Both read the `OSC 7501` state the agent reports, the `state` key of
`list-agents --json`.

- **`agent-idle` is met when the agent stops working**: `idle`, `done` or `error`, or `blocked` on
  a person. A supervisor that waits for the end of a turn must also hear that the agent stopped to
  ask, or it sleeps until the timeout while the agent waits for an answer. `state` says which.
- **`agent-waiting` is met only by `blocked`.** An agent that ends its turn without asking is a
  miss, exit 2, rather than a wait that runs to its timeout: it will not ask now.
- **The report** is `waited_ms`, `idle_by` (`state` or `quiet`), `state`, and `state_kind` and
  `state_msg` when the agent gave them. A message on several lines is folded onto one.
- **Only a state reported after the last input counts.** The grid keeps a per-pane flag beside the
  `OSC 7501` records: typed input sets it (the same bytes that set `input_since_prompt`), and any
  report or clear resets it. `list-panes --json` carries `"input_since_state": true`, written only
  when true and only beside a `state`, so no existing row changes. While it is set the wait reads
  the state as stale. This is the `--for ready` rule applied to the agent's own word, and it needs
  no baseline: an agent that finished before the wait began has already reported.
- **An agent that reports no state** (opencode; codex without the hooks `zellij setup
  --agent-hooks` prints) is judged by silence for `agent-idle`: `--quiet-ms`, 3000 by default for
  these two modes, after the pane has drawn something. The report says `idle_by: quiet`.
  `agent-waiting` has no fallback, and misses with a message that names the harnesses that report.
  Claude between `133;A` and its next report also reads as no state for a moment; its spinner
  keeps the pane from being quiet, so the silence fallback does not answer early there.
- **A pane that closes or whose command exits first is a miss**, exit 2.

Capability: `wait-agent-state`.

Limits:

- An Enter that the agent ignores (an empty prompt) sets the flag and gets no report, so the wait
  runs to its timeout.
- A keystroke typed into an agent's prompt without submitting it makes the state stale until the
  agent reports again.

MCP: the enum values, the summary, the tips and the `quiet_ms` note. The tool keeps its six
parameters. Files: `zellij-server/src/program_status.rs` (the flag), `panes/grid.rs`,
`panes/terminal_pane.rs`, `tab/mod.rs`, `screen.rs`; `zellij-utils/src/data.rs`
(`input_since_state`), `cli.rs` (the two `WaitFor` values and the 3000 ms default),
`cli_surface.rs` (the wait's keys); `zellij-client/src/cli_client.rs` (`wait_for_agent`,
`AgentWatch`); `src/commands.rs`; `src/mcp/`. Tests: the flag's life in `program_status`, the
judge on scripted looks (stale, working, each stop, the silence fallback, the `agent-waiting`
misses), the row reading, the report lines, the clap defaults and the MCP command lines.

### Checking in on an agent: `last-reply` and `submit`

From nkmk.34, from the agent-orchestration plan. A session that drives agents in other panes read
their whole screen to learn what they said, and typed a message as keystrokes whose newlines sent
it early.

```
zellij_write_input {"pane": "sunny-otter", "text": "Fix the failing test.\nThen run it.", "submit": true}
zellij_wait_for    {"pane": "sunny-otter", "until": "agent_idle"}
zellij_read_pane   {"pane": "sunny-otter", "last_reply": true}
  # action last-reply --pane-id sunny-otter --json
{"pane_id": "terminal_3", "handle": "sunny-otter", "kind": "claude", "agent_id": "…",
 "transcript": "/home/u/.claude/projects/-home-u-src/….jsonl", "state": "done",
 "reply": "Fixed: the fixture path was relative. The test passes."}
```

**`zellij action last-reply --pane-id <pane> [--json]`** prints the agent's last reply, read from
its harness's own transcript rather than from the screen. Each harness draws a turn its own way,
wraps it to the pane and scrolls the start of a long answer away; the transcript holds the text as
the model wrote it.

- **Which transcript**: the session id `list-agents` already shows (`agent_id`, from nkmk.31's
  resolution: claude's per-process record, codex's open rollout file, or the agent's own `OSC 1337`
  report). The file is found by that id under the harness's directory, newest first:

  | harness | file |
  |---|---|
  | claude | `$CLAUDE_CONFIG_DIR` or `~/.claude`, `projects/*/<id>.jsonl` |
  | codex | `$CODEX_HOME` or `~/.codex`, `sessions/YYYY/MM/DD/rollout-*-<id>.jsonl` |
  | pi | `$PI_CODING_AGENT_DIR` or `~/.pi/agent`, `sessions/*/*_<id>.jsonl` |

- **The last reply** is the text the agent wrote after the last prompt, tool call or tool result:
  the answer at the end of a turn, without the "let me look" narration between tool calls. Thinking
  is never part of it. claude's subagent lines (`isSidechain`) and injected context (`isMeta`)
  are skipped. Only the last 8 MiB of a transcript is read.
- **A miss, exit 2, says why**: the pane runs no agent; no session id is known yet (a codex or pi
  that has not been sent a message); opencode, whose sessions live in a database this build does
  not read; no transcript under the directory; or nothing written since the last prompt (the
  report names the agent's `state`). The pane is resolved like every addressed verb.
- **It is the client's**, like `list-agents`: one `list-panes`, then a file read on this machine.
  No action, no contract change. The directories come from the CLI's own environment, not the
  agent's: an agent started with a different `CLAUDE_CONFIG_DIR` is looked for in the wrong place.

**`zellij_read_pane` gains `last_reply`**, which runs `last-reply --json`. It refuses `full` and
`ansi` beside it.

**`zellij_write_input` gains `submit`.** With `text` it runs `action paste` (one bracketed paste,
so a newline in the message does not send it early) and then `send-keys Enter`, 150 ms apart. A
TUI that reads a paste as it arrives can take an Enter right behind it as part of the paste.

Capability: `last-reply`. Both MCP tools stay within the parameter cap: `zellij_read_pane` has
five, `zellij_write_input` five. Files: `zellij-client/src/agent_reply.rs` (new: the transcript
roots, the find, the tail read and the per-harness walk), `cli_client.rs`
(`start_last_reply_client`, `last_reply_of`), `zellij-utils/src/cli.rs` (`LastReply`),
`input/actions.rs` (the refusal arm), `cli_surface.rs` (the `read` band, the payload row),
`capabilities.rs`; `src/commands.rs`; `src/mcp/` (`Call::Sequence`, `write_input_call`, the two
tools). Tests: the walk on claude, codex and pi transcripts
(narration, tool steps, mid-tool, sidechain, meta), the roots and their variables, the find at each
harness's depth, the tail read, every miss of `last_reply_of`, and the paste-then-Enter sequence.

### `zellij_overview scope=my_agents`: the supervision view

From nkmk.34, from the agent-orchestration plan. A session that started several agents had no one
call that said which of them were still working, which were waiting on it, and which had gone
quiet.

```
zellij_overview {"scope": "my_agents"}
  # action list-agents --json, kept to the caller's own tab
[{"handle": "brisk-heron", "kind": "claude", "cwd": "/home/u/src/app",
  "state": "blocked", "state_kind": "permission", "state_msg": "approve Bash: make",
  "state_at": 1791821058267, "state_age_s": 42,
  "last_output_at": 1791821058301, "output_age_s": 42, ...}]
```

- **`list-agents --json` rows gain `last_output_at`**, the pane's own (milliseconds since the
  epoch), so "how long since this agent drew anything" needs no second walk. A `--watch` does not
  compare it: it moves with every frame an agent draws, and a watch prints changes of identity and
  state. The table keeps its columns.
- **`zellij_overview` adds the ages** to every agent row of `scope=agents`: `state_age_s`, the
  whole seconds since the agent last reported a state, and `output_age_s`, since its pane last drew
  anything. A reader asked which agent has stalled should not have to subtract 13-digit numbers.
  The raw keys stay beside them. A working agent whose `output_age_s` keeps growing has stalled.
- **`command` is cut to 120 characters there**, on one line. An agent started with a prompt carries
  the whole prompt in its command line, and a supervisor polling the view would be handed every
  prompt on every call. `list-agents --json` keeps it whole.
- **`scope=my_agents`** is the same answer kept to the agents in the caller's own `-zj` tab: the
  tab `zellij_create` puts every pane in by default, and so every agent the caller started there.
  It is found the way `zellij_create` finds it, from the tab the server's own
  pane is in. An agent a person moved out of that tab is no longer counted as the caller's, and
  one moved in is. `scope=agents` lists every agent in the session.

`zellij_overview` keeps three parameters. Files: `zellij-utils/src/data.rs`, `agent_detect.rs`;
`src/mcp/invoke.rs` (`AgentsView`, the `my_agents` plan), `src/mcp/mod.rs`, `src/mcp/tools.rs`.
Tests: the filter, the ages from an envelope and a bare array, the command line, and a watch that
ignores a moving `last_output_at`.

### `zellij mcp` reaches sessions on other machines over ssh

From nkmk.35, from the agent-orchestration plan. One MCP server drives the sessions on every
machine of a small fleet, the same way it drives local ones. Every tool already runs the CLI, so a
remote call is the same command line, run by the zellij on the other machine through ssh.

```kdl
// config.kdl - top level
remote_hosts {
    from "~/.config/fleet/machines.tsv" column="hostname" where="zellij_peer=yes"
    host "boxa"
    host "boxb" bin="/opt/homebrew/bin/zellij"   // optional: the zellij binary on that machine
}
```

```
zellij_overview {"scope": "fleet"}
  # text: how_to_message, then the agents grouped by host, tab and pane (see "Agent listings are
  # grouped by host, tab and pane"); structure: {agents, hosts}, one row per agent

zellij_read_pane {"session": "boxb/work", "pane": "calm-otter"}
```

- **The block is the whitelist.** A host it does not list is refused before ssh runs, and the
  refusal names the config file and the listed hosts. A `host` takes one name, whatever ssh takes:
  an alias from `~/.ssh/config`, a hostname, or `user@host`. A name with whitespace, a leading dash
  or a `/` is skipped with a warning. `bin=` must be an absolute path.
- **`from` reads the list out of a table** the fleet already keeps, so the config needs no second
  copy. The file is tab-separated, with a header line and no comments. A leading `~` is the home
  directory. `column` names the header column that holds the ssh host, `hostname` by default.
  `where="col=value"` keeps the rows whose `col` is `value`, ignoring case; without it every row
  counts. A name from a table is lower-cased before ssh sees it: a table may record `BOXA` as the
  machine reports it, and ssh matches `Host` patterns case-sensitively, so `BOXA` would miss the
  `Host boxa` entry. An inline `host` with a table row's name replaces that row, which is how one
  host gets `bin=`; inline hosts no table names are added after the table rows. A file that is
  missing or unreadable, a column the header lacks, or a `where` that is not `col=value` is a
  warning and no hosts from that line, never a failed call. The file is read on every call that
  needs the list, so an edit applies without a restart.
- **One config for every machine.** The list names the machine that reads it too. That entry is
  skipped: a call to it stays local, with no ssh. A host is this machine when its first label
  matches the short hostname, ignoring case and any `user@`, so `boxa` matches `BOXA.local`.
- **Addressing is the `session` argument, `host/session`.** No tool gained a parameter.
  `boxa/work` is session `work` on `boxa`. `boxa/` names no session, and the remote CLI resolves
  one the way a terminal there would, which works when one session runs there. `scope=sessions`
  takes `boxa/` to ask that machine. A session name cannot contain `/`
  (`validate_session_name`), so the split is never ambiguous. A pane handle alone is always
  local: name its host through `session`.
- **The transport.** `ssh -T -o BatchMode=yes -o ConnectTimeout=5 -o ForwardAgent=no -o
  ForwardX11=no`, never interactive and never forwarding. A connection is shared for 60 seconds
  through `ControlMaster`, with its socket in `/tmp/zellij-mcp-<uid>` (private, and short: a
  `$TMPDIR` on macOS is too long for a socket path). The remote line is
  `sh -c '<script>' sh '<bin>' '<argv>'...`, every word single-quoted, so the remote login shell
  parses nothing in the argv. The script uses `bin=` when it is given. Otherwise it tries
  `command -v zellij`, then `/opt/homebrew/bin`, `/usr/local/bin`, `/home/linuxbrew/.linuxbrew/bin`,
  `~/.local/bin` and `~/.cargo/bin`: a non-interactive ssh shell on macOS may not have Homebrew on
  `PATH`. `ZELLIJ_MCP_SSH` names a different ssh program, for tests. Exit 255 is ssh's own failure
  and is reported as "ssh could not reach <host>".
- **`zellij_overview scope=fleet`** runs `list-agents --all-sessions --json` here
  and on every remote host at once, each bounded by 10 seconds. Every row gains `host`. A host that does not
  answer is one host entry with its `error` and `error_kind` `unreachable`, and one that answers
  with an error has `error_kind` `error`, never a failed call. A host on a build from before `--all-sessions` gets `ls --json` and one
  `list-agents` per session instead, so a fleet halfway through an upgrade still lists. The
  structured result is `{agents, hosts}`, where `hosts` says per host how many agents it has, or
  its error. It takes no `session`. State and output ages are computed against this machine's
  clock.
- **What changes for a create on another machine.** `kind: stacked` is refused:
  it needs the caller's own pane, which is not in that session. `agent_tab` and `tab` work. The
  agent tab is named after the caller's own tab on this machine, so a caller has one tab name on
  every host, and the remote session is asked whether it has it yet. `cwd` must be absolute; `~`,
  `$VAR` and a relative path are refused, because they would mean this machine. `run_in=exec` runs
  through `/bin/sh -c`, not this machine's `$SHELL`.
- **`scope=my_agents` with `host/session`** lists the agents in that remote session's tab of the
  caller's tab name. Without a host it is this machine's session, as before. No call lists "my
  agents" on every host at once; `scope=fleet` lists every agent, with its tab.
- **A remote result says so:** the structured result carries `host`. `command` is the command
  line as that machine's zellij ran it.
- **Capability `mcp-remote-hosts`.**

What is not supported: a host outside the block, password or host-key prompts (`BatchMode` fails
instead), and a remote `kind: stacked`. Cancelling a remote `zellij_wait_for` kills the
local ssh. The remote `wait` runs on until its own `--timeout`, because a `-T` session sends it no
hangup.

**The block is read by `zellij mcp` alone**, from the config file the CLI would use (`--config`,
`ZELLIJ_CONFIG_FILE`, `ZELLIJ_CONFIG_DIR`, then the default). It is read again on every call that
names a host, so an edit needs no restart. It is not in `Options`, because `Options` crosses the
client/server contract and no other process needs the list. An older binary ignores an unknown
top-level key, so the block can be in a shared config before every machine runs this build. An
unknown entry or property warns on stderr at startup and in the fleet answer's `diagnostics`. One
gap follows from not being in `Options`. A configuration-plugin save that writes `config.kdl`
from the parsed config (`write_config_to_disk`) drops the block, as it drops comments.

`src/mcp/remote.rs` (new: the block, `route`, quoting, the remote script, ssh's argv), `src/mcp/fleet.rs` (new: the parallel ask, the older-build fallback, the merge),
`src/mcp/invoke.rs` (`run_on`, `CallEnv.remote`, the remote arms of create and write,
and the agent tab), `src/mcp/mod.rs` (routing, `with_host`, the instructions),
`src/mcp/tools.rs` (the `fleet` scope and the `session` text), `src/lib.rs` (the config path),
`zellij-utils/src/capabilities.rs`. Tests: the block, the `from` table (column, where, a missing file, an unknown column, an inline override, an upper-case local host) and their warnings, routing and the
whitelist refusal, the local-host match, quoting through a real `sh`, the probe with `bin=` and
on `PATH`, ssh's argv, the remote argv of every create arm,
and the fleet merge through a stand-in ssh with one host down, one too old for
`--all-sessions` and one past the bound.

Proved from a Linux machine against two macOS machines and a scratch session reached over real
ssh. The fleet answer listed the agents on the Linux machine and on a Mac still on nkmk.31,
through the fallback, and "no agents" for a Mac with none. Against the scratch session, a create
into the agent tab, a `submit`, a `wait --for match`, a `dump-screen` and a close all answered as
they do locally.

### CLI panes can default to stacks; MCP creation uses stacks or tabs

Set this top-level key in `config.kdl`:

```kdl
new_pane_default "stacked"
```

`"split"` keeps upstream placement. An absent key also keeps upstream placement.
Older binaries ignore this top-level key.

The client applies the default to `run`, `action new-pane`, and `edit`.
An unplaced pane stacks onto the terminal pane named by `ZELLIJ_PANE_ID`.
Without a numeric anchor in the selected session, the client keeps a split and explains why on stderr.
Explicit placement flags keep their meaning: direction, floating, stacked, in-place,
near, near-current-pane, and tab targets. Plugin creation keeps its explicit behavior.
The setting stays outside `Options` and never changes the protobuf contract.

A stacked edit resolves its editor from the client's config, then `EDITOR`, `VISUAL`, or `vi`.
It preserves editor arguments, file paths, working directories, and supported line syntax.
The existing stacked command contract cannot carry the editor's retry without line arguments.
Ordinary edits retain that retry.

MCP `zellij_create` accepts `stacked`, `agent_tab`, and `tab`.
`agent_tab` remains the default and reuses the caller's `-zj` tab.
`stacked` uses `--stacked --near-current-pane` and requires the caller's local pane.
The tool rejects the removed `pane` and `floating` kinds and the `floating` parameter.
Placement validation runs before any session query.

Tests cover config parsing and rendering, client-only serialization, CLI placement,
missing-anchor warnings, editor commands, and the MCP kinds and schema.
The five active setup snapshots include the unset client default.
CLI parser tests use the shared large-stack helper required by the unoptimized CI build.

### Live agent names, pane frames and caller context

`zellij list-agents` aliases `zellij action list-agents`, including watch and all-session options.
`list-agents` shows each agent's messaging name. Its JSON carries `name` and `self`.
The caller's row shows `(you)` in the table, using `ZELLIJ_PANE_ID` and the caller's session.
`list-panes --json` carries the name under `agent` and marks the caller with `self`.
MCP overview scopes `agents`, `my_agents` and `fleet` preserve these fields.

Claude names come from its live process's session record.
The server verifies the record's pid and the Claude process before trusting its name.
The server refreshes names once per second, including restarts and name removal.
The detection tick discovers agent commands even when their panes produce no output.
A silent agent can therefore acquire its record identity and messaging name while waiting at a prompt.
The server caches record contents and checks their modification time on each tick.
It re-reads the record when its modification time or foreground process changes.
Name and session-id changes use the same cached record.
Identity follows the foreground process, including shells with job control disabled.
Configured default shells remain shell panes, so their agent children participate in detection.
Other harnesses return `name: null`; session ids never substitute for names.
`ZELLIJ_CLAUDE_SESSIONS_DIR` overrides the record directory for isolated tests.
The default follows `CLAUDE_CONFIG_DIR`, then `~/.claude/sessions`.

Enable names before pane titles with this top-level config key:

```kdl
pane_frame_agent_name true
```

The key defaults to false and reloads live.
A named frame reads `project-1 · title`; changing the name refreshes the frame.

`zellij action whoami` prints the caller's session, host, tab, pane handle, and agent name.
It also lists other agents with names, tabs, handles, and states.
`--json` returns the same context as JSON.
The command requires `ZELLIJ_PANE_ID`.
The grouped action help and `--dump-surface` expose `whoami` as a read command.
The dump also describes the top-level `list-agents` alias.

`zellij setup --agent-hooks` includes a Claude SessionStart context hook.
Install the emitted script and merge the emitted hooks into Claude's settings.
The context hook stays silent outside zellij and bounds its lookup to 150 ms.
The watchdog closes its output pipes, so a fast context response returns immediately.
Its context ends with:

> Agent list changes; run `zellij list-agents` before addressing another agent.
> To talk to an agent that has a name, message it by that name with your harness's messaging tool;
> type into its pane only when it has no messaging.

MCP offers an `agents` prompt and a `zellij://agents` JSON resource.
Each request reads the current session's live list, including names, tabs, handles, and caller markers.
Neither adds a tool or changes the tool parameter limit.
After successful `zellij_write_input`, a named target adds this hint:

```text
this pane runs claude `project-1`; prefer messaging it by name
```

The hint never blocks input. Which agents get one, and with which address, is in "The
`zellij_write_input` hint names the address that reaches the agent".

### The forwarding panic test follows the build's assertion mode

The client forwarding guard uses `debug_assert!`.
Its panic test now compiles only when debug assertions are enabled.
This keeps the quick-profile release gate valid without changing production behavior.

### A missing fixture plugin fails the blocking plugin tests at once

The `#[ignore]`d plugin-system tests load `fixture-plugin-for-tests.wasm` and block on its output.
Without the fixture, upstream's tests hang. The fork's tests panic at once and name the build
command (`cargo xtask ci e2e --build`). `plugin_unloaded_while_loading_does_not_run_once_loaded`
is the exception. It polls with a timeout and runs in `cargo xtask test`, where CI builds no
fixture. It reads the fixture path without the check, as upstream does, so it passes there.
`zellij-server/src/plugins/unit/plugin_tests.rs` (`PLUGIN_FIXTURE`).

### Nextest serializes tests that share the web token database

Web client tests and token tests share a database and delete it between tests.
Their `serial_test` locks apply only within one process.
A nextest group now runs these tests one at a time across test processes.
Other tests retain parallel execution. Production authentication behavior stays unchanged.

### `setup --harness-map`: shared harness facts and screen rules

`zellij setup --harness-map [--json]` prints one inventory for Claude, Codex, OpenCode, and Pi.
Each fact carries a source, revision, and verification marker.
The JSON includes verbatim detector TOML and separate fork supplements.
The command needs no session and changes no client/server contract.
`setup --check --json` advertises the `harness-map` capability.

The zellij fork stores the files under `zellij-utils/assets/harness/`.
Paseo commit `e135e9c101700b31671fc374325d17b608467a91` supplies native title facts.
Herdr commit `87e5c13c6028df72405f63388dde585212ad3019` supplies four verbatim manifests and the evaluator semantics.
The vendored files retain Apache-2.0 LICENSE and source/modification notices.
The pure Rust evaluator preserves regions, priorities, nested gates, exclusions, and engine requirements.
The public wrapper returns unknown when no rule matches and preserves transcript-viewer skip semantics.
It distinguishes explicit question footers where evidence permits.
Shared permission/question screens remain blocked.
No server or MCP wiring changes in this patch.

Codex rate-limit, hook-trust, and Pi project-trust screen rules remain unverified against installed releases.
The inventory also marks uncertain keys, process aliases, and storage assumptions.
Synthetic fixtures test those rules without claiming a live capture.

To update Herdr, change the pinned commit, re-copy manifests and LICENSE, and review each file's notices.
Compare engine changes before raising the supported engine version.
Then update source revisions, re-run fixtures, run the wasm check, and read Rust CI.
To update Paseo facts, review each provider at the new pinned commit and update the affected facts.
The [harness README](zellij-utils/assets/harness/README.md) documents the schema and integration API.

### The binary keeps agent-session.sh current

Every claude and codex hook calls `~/.config/zellij/agent-session.sh` by path. Nothing wrote that
file: the operator copied it out of `setup --agent-hooks`. So a machine kept an older build's copy
for weeks, and the `context` and `state` modes never ran. An unknown mode exits zero, so nothing
said so. The binary now owns the file.

- `zellij setup --agent-hooks --write` writes this build's `SESSION_SCRIPT` to that path. It
  creates the directory, writes a temporary file beside the script and renames it over, so a hook
  never reads half a script. A symlink is written through, so a dotfiles link stays a link. A
  current copy is left alone. The first line of output says what it did; the snippets follow as
  before. `--write` without `--agent-hooks` is a usage error. No harness settings file is touched.
- `zellij session doctor` checks the script under `hooks`. A missing or stale copy is one finding,
  and the default fix rewrites it. A dry run or `--no-fix` says `would write` or `would rewrite`.
  The finding tells an older build's script from an edited one, for example
  `rewrote ~/.config/zellij/agent-session.sh: it held local edits, which are gone`. Rewriting is
  the fix either way, because the binary owns the file.
- To tell the two apart, `EARLIER_SESSION_SCRIPTS` in `zellij-utils/src/agent_hooks.rs` holds the
  sha256 of every script an earlier build shipped. A test pins the current hash, so a change to
  the script fails until the old hash is appended.
- The path follows `HOME` only, because the hooks name it with `~`. `ZELLIJ_CONFIG_DIR` and
  `XDG_CONFIG_HOME` do not move it.
- The claude snippet now runs `claude start` and `claude context` on `SessionStart`, and
  `claude end` on `SessionEnd`. See the agent-session contract above for why.

`setup --check --json` lists `agent-hooks-write`. No client/server contract changes.

### `zellij_create` reads its pane back before it says `input_sent`

`input_sent: true` used to mean only that `write-chars` and `send-keys` exited 0. That is the
server taking the keystrokes, not the pane showing them. A report from a Mac had a tab open in the
session's default directory with nothing typed, while the tool answered `ready: true`,
`input_sent: true`. The report could not be reproduced on Linux with nkmk.30, .35 or .37 as the
server. The answer now comes from the pane:

- **Before typing**, one `list-panes --json` reads the fresh shell's `pane_cwd`, before the
  typed command can `cd`. The result carries `pane_cwd`. A create that passed `cwd` also carries
  `cwd_honoured`. When it is `false`, the result has a note naming both directories. On this
  machine the two are compared through the filesystem, so a symlinked `/tmp` is not a mismatch.
  On a remote host they are compared as text.
- **After typing**, the pane is read for up to 2 seconds. The command counts as seen when the
  shell's OSC 133 state is `running` or `done`, or when the first 24 characters of the command's
  first line are on the pane's screen.
- `input_sent` turns `false` only when the pane was read and showed neither. The result then has a
  note: the command was handed to the pane, so read the pane before you type it again. A pane that
  could not be read at all keeps the write's own answer.

`src/mcp/mod.rs` (`type_and_read_back`, `typed_command_seen`, `terminal_pane_row`) and
`src/mcp/invoke.rs` (`TypeAfter::read_back_argvs`). Tests: what counts as seen, the requested
`cwd` and the directory comparison, the pane row by reported id, and nothing read back from a pane
that was not ready.

The same change gives `zellij_wait_for`'s `timeout_s` a description of its own, replacing the CLI
help it borrowed. Every wait answers a fraction of a second after `timeout_s`, and many MCP clients
cut a request at 60 seconds. So `timeout_s: 60` lost to that deadline every time: the caller got
`MCP error -32001: Request timed out` instead of the bounded miss. Concurrent waits do not
serialize: rmcp runs each request in its own task, and each wait is its own child process. The
description now tells the caller to stay at least 5 seconds under the client's deadline and wait
again. The 300-second default is unchanged: lowering it would cut short a long wait in a client
whose deadline is long, such as Claude Code.

### `new-pane --plugin` opens in the tab it was asked for

`new-pane --plugin` with `--tab-id`, `--in-tab` or `--new-tab` put the plugin somewhere else. An
agent testing a plugin could not load it in its own tab, away from the person's.

```
zellij action new-pane --in-tab=work-zj --no-focus --plugin=file:/tmp/bar.wasm \
  --configuration=data_file=/tmp/d.json
pane_id: plugin_12
handle: brave-lynx
```

Two server fixes:

- **A plugin pane asked for in another tab opens there.** `new-pane --plugin --tab-id` and
  `--in-tab` were ignored. `AddPlugin` took the client's active tab whenever a client was named,
  and a plugin load always names one. So the pane opened in the focused tab, or, with no client
  attached, nowhere: `No pane was created`. `AddPlugin` now uses the requested tab when it is a
  different tab from the client's. Where the two agree, nothing changes.
- **`new-pane --plugin --new-tab` reports the plugin.** The report took the first plugin of a
  `HashMap`, so it could name the tab template's tab bar or status bar. The plugin thread now names the pane it put into the template's empty slot, and
  `ApplyLayout` keeps that name instead of picking one.

Files: `zellij-server/src/screen.rs` (`AddPlugin`, `ApplyLayout`), `zellij-server/src/plugins/mod.rs`
(`NewTab`), `zellij-server/src/route.rs` (`NotificationEnd::has_affected_pane_id`). Tests: a
plugin pane opening in a tab the client is not in, and a new tab that keeps the pane the plugin
thread named. No client/server contract changes.

### `zellij_create kind=tab` names the tab

```
zellij_create {"kind": "tab", "name": "build"}
action new-tab --no-focus --name=build --wait-ready --ready-timeout=10
```

Without `name`, a tab an agent makes is called `Tab #N`. `name` names the tab, and only the tab: it
is accepted with `kind: tab` alone. Any other kind, and `reopen`, refuse it with one line before a
session is asked anything. A pane still keeps its generated handle, and a command pane is still
named after its command.

`src/mcp/tools.rs` (the `name` parameter and the tool's tips) and `src/mcp/invoke.rs`
(`create_kind_allowed`, the `tab` arm of `argv_in`). Tests: the tab's name on the command line, a
name that starts with a dash, and the refusals. CLI-side only; no contract change.

### A pane stacked in the background leaves the focused pane expanded

```
# the person is focused on terminal_0; an agent in terminal_0's stack runs
ZELLIJ_PANE_ID=0 zellij action new-pane --near-current-pane --stacked
# before: terminal_1 expanded, terminal_0 collapsed (hidden in a stack list) and still focused
# after:  terminal_0 expanded and focused, terminal_1 waits collapsed
```

A pane that joins a stack always took the expanded slot. When it joined without focus - an
anchored `--near-current-pane` stack, or `--stacked --no-focus` - the pane the person was focused
on collapsed but stayed the active pane. With stack lists (`stacked_pane_list`, on by default) it
was hidden outright. The active-pane highlight sat on a pane nobody could see, and a click on it did
nothing, because the server held it as active already.

The rule now: a pane stacked without focus does not take the expanded slot from a pane a client is
focused on. That pane stays expanded and focused, and the new pane waits collapsed behind it. In a
stack nobody is focused in, the new pane expands as before. The fix runs before the stack list is
synced, which also fixes an anchored stack WITH focus: there the client's focus used to stay on the
member the list had just hidden.

`zellij-server/src/panes/tiled_panes/mod.rs` (`keep_focused_pane_expanded_in_stack_of`) and
`zellij-server/src/tab/mod.rs` (`add_stacked_pane_to_pane_id`, `add_stacked_pane_to_active_pane`).
Tests in `tab_tests.rs`, each with and without stack lists: the anchored and the active-pane route
keep the focused pane shown, a stack nobody is focused in shows the new pane, and a focused stack
create takes both focus and the expanded slot. No contract change.

A tiled pane made without focus in a tab with no room left takes a different path; see "A pane
made without focus in a full tab leaves the focus alone".

### An agent's new pane or tab leaves the person's focus alone

```
# from an agent's shell (AGENT_SESSION_ID is set)
zellij action new-tab                 # before: the person is taken to the new tab
                                      # after:  the tab is made in the background
zellij action new-tab --focus         # the agent asked: the person is taken there
```

`zellij action new-pane`, `zellij action new-tab` and `zellij run` took the focus unless the caller
passed `--no-focus`. An agent that made a pane moved the person's cursor, and the person's next
keystrokes went to the agent's pane. Now the caller decides the default:

| Caller | No flag | `--no-focus` | `--focus` |
|---|---|---|---|
| a person | takes the focus (upstream) | leaves it | takes it |
| an agent | **leaves it** | leaves it | takes it |

The caller is an agent when any variable from `agent_identity_env_names` is set and not empty:
`AGENT_SESSION_ID`, `AGENT_KIND`, or a harness's own session variable such as
`CLAUDE_CODE_SESSION_ID` or `CODEX_THREAD_ID`. A session this binary creates drops all of them, so
a person's shell in a session an agent started is not taken for an agent.

`--focus` and `--no-focus` together are a clap error, and so are `--focus` and `--in-tab`, which
never moves anybody. The `zellij plugin` shorthand reaches the same `new-pane` path and takes the
same `--focus`. `zellij --layout` inside a session reaches the `new-tab` path, so an agent's call
there leaves the focus alone too; it has no `--focus`, because it is a top-level option, not a
subcommand. An agent that wants the person taken to the tab runs `zellij action new-tab --layout
<layout> --focus`. `zellij edit` and `action edit` are unchanged.

A no-focus `new-pane` run from inside a pane opens beside that pane (`ZELLIJ_PANE_ID`), not beside
the person's focused pane, as `--no-focus` always has. So an agent's bare `new-pane --stacked` now
stacks onto the agent's own pane in the background. With "A pane stacked in the background leaves
the focused pane expanded", the person's focused pane stays expanded.

An agent's bare `new-pane` or `zellij run` in a tab with no room to split joins a stack; "A pane
made without focus in a full tab leaves the focus alone" keeps the person's focus there too.

MCP `zellij_create kind=tab` passes `--no-focus` itself, so it does not depend on the environment
the MCP server was started with.

The decision is made on the CLI side before the action is built (`leaves_focus_alone` and
`CliAction::settle_focus_for_caller` in `zellij-utils/src/cli.rs`, `caller_is_agent` in
`zellij-utils/src/agent_detect.rs`, called from `send_action_to_session` in `src/commands.rs`). The
action carries `no_focus` as before, so the client/server contract does not change. Tests: the
decision table, the settled `new-pane` and `new-tab`, the clap conflicts, `zellij run --focus` and `zellij plugin --focus`, and
agent detection from the environment.

### A pane made without focus in a full tab leaves the focus alone

```
# the person is focused on terminal_12, in a tab with no room left to split
AGENT_SESSION_ID=x zellij action new-pane      # or: zellij action new-pane --no-focus
# before: the new pane joins the stack and the person's focus moves to it
# after:  the new pane joins the stack collapsed; terminal_12 stays focused and expanded
```

A tiled pane that finds no room to split falls back into a stack. That fallback expanded the new
pane with `expand_pane_in_stack`, which moves every client focused in the stack onto it. So
`--no-focus` was not honoured there, and an agent's bare `new-pane` or `zellij run` took the
person's typing focus.

The fallback now keeps the pane a client is focused on expanded and moves nobody
(`keep_focused_pane_expanded_in_stack_of`, from "A pane stacked in the background leaves the
focused pane expanded"). A
focusing create is unchanged: `Tab::add_tiled_pane` focuses the new pane afterwards, and focusing a
stacked pane expands it and moves the clients in that stack with it.

`zellij-server/src/panes/tiled_panes/mod.rs` (`add_pane_without_stacked_resize`). Tests in
`tab_tests.rs`, each with and without stack lists: background panes in a full tab leave the focus and
the focused pane's expansion alone, and a focused create in a full tab takes both. No contract
change.

### A resumed session's new tabs open where they are asked to

```
# a session that came back from its serialized layout (`session up`, `session restart`)
zellij action new-pane --new-tab=work --cwd=<default_cwd>/project
zellij action new-tab --cwd=<default_cwd>/project
# before: the tab's shell starts in <default_cwd>
# after:  it starts in <default_cwd>/project
```

A serialized session layout names a global `cwd` for its tabs and carries a `new_tab_template`.
The parser stamped that global `cwd` onto the template's panes as an absolute path. A new tab
joins its own cwd onto the template's, and joining onto an absolute path leaves the absolute path,
so the session's directory beat `--cwd`, and beat the caller's own directory for a plain
`new-tab`. A fresh session did not do it; only a resumed one did. The next serialization wrote the
stamp back as `pane cwd="..."`, so every later resume kept it.

The parser no longer applies the global `cwd` to `new_tab_template`: it says where the written
tabs hang from, not where a new tab opens. A template pane whose cwd equals the global `cwd` is
taken for that stamp, as an older build wrote it, and dropped too, so a session heals on its first
resume onto this build without `--fresh`. A template cwd that differs from the global one is kept.

`zellij-utils/src/kdl/kdl_layout_parser.rs` (`populate_new_tab_template`, `drop_cwd_equal_to`).
Test in `zellij-utils/src/input/unit/layout_test.rs`: the fresh and the stamped template take no
cwd, a template's own cwd stays, and the tabs still hang from the global `cwd`. No contract change.

### A scratch environment cannot rewrite the real pin

```
$ ZELLIJ_SOCKET_DIR=/tmp/zj-test/sock zellij session up scratch    # config has `pin_exe true`
refusing to write the pin <home>/.local/share/zellij/bin/zellij: ZELLIJ_SOCKET_DIR points at a
scratch socket directory, but the pin is the real one under this account's home; set XDG_DATA_HOME
to a scratch directory too
$ echo $?
2
```

A test run moves its sockets with `ZELLIJ_SOCKET_DIR`. With the real config's `pin_exe`, the pin
did not move with them: `session up` copied the dev build over the pin the real launcher runs.

The rule: when `ZELLIJ_SOCKET_DIR` names a directory other than this user's default socket
directory, no pin under the account's home is written. The home comes from the passwd database, as
in the release-candidate guard, so a scratch `HOME` does not unlock it. A pin outside that home,
such as `XDG_DATA_HOME=/tmp/zj-test/data` on Linux or a scratch `pin_exe` path, is written as
before.

- **It cannot refuse a real install.** A generated unit never sets `ZELLIJ_SOCKET_DIR`; the
  generator refuses it (`FORBIDDEN_ENV_NAMES`). A shell that exports the default value is not a
  scratch environment. A shell that exports another value already sees other sessions than its
  launcher does.
- **The verbs refuse first:** `session up`, `session restart`, `session enable` and
  `session doctor --fix` print one line, exit 2 and start nothing.
- **The writer refuses too.** `install_pinned_exe` returns the same line as an error before it reads
  the pin or its stamp. An interactive launch then serves the session from the build it is, with
  the warning it prints for any pin it cannot write.
- On macOS the canonical pin stays under `~/Library/Application Support`; `XDG_DATA_HOME` does not
  move it. The line says to drop `pin_exe` from the scratch config or name a scratch path.

`zellij-utils/src/session_lifecycle.rs` (`scratch_pin_refusal`, `scratch_may_write_pin`,
`socket_dir_redirects`), `src/session_commands.rs` (`refuse_a_scratch_pin`). Tests in
`session_lifecycle.rs`. No contract change.

### The vitals probe never overlaps itself, outlives its window, or runs once per tab

`slim-keybinds` draws its vitals cluster from a `/bin/sh` probe every ten seconds. On macOS the
probe runs `top -l 2 -n 0 -s 1`, which takes a second when the machine is idle and far longer when it
is not. The timer fired whether or not the last probe had answered, so under load the probes piled
up, each `top` slowed the others, and one Mac reached a load average near 300. Every tab has its own
bar, and every bar ran its own probe. A later report found `top` probes piling up in a detached
session until the Mac ran out of process slots: a probe given up as lost was never killed.

- **A probe kills itself when its window closes.** The probe runs as a background job, and a
  watchdog job kills its whole process tree after eight seconds, under the ten-second interval:
  each process is stopped, then its children are killed the same way (`pgrep -P`), then the
  process. It is plain `/bin/sh`, because macOS has no `timeout(1)`, and it kills the children
  too, because killing the shell alone would leave `top` running as an orphan. The watchdog writes
  nothing, so it never holds the output open, and it is killed once the probe ends in time. A
  killed probe exits non-zero and the bar keeps its last numbers. Linux runs the same wrapper.
- **One probe in flight per bar.** A tick that finds a probe still out is skipped. A probe
  unanswered for three ticks counts as lost, and the next tick starts a new one, so a result that
  never comes cannot stop the cluster.
- **One bar per session probes; the others draw its numbers.** The bar that probes sends its
  output on a pipe (`slim-keybinds-vitals`) to every plugin of the session, and every other bar
  parses and draws it. The bars pick the prober among themselves: the lowest plugin id, then the
  lowest client id, since a plugin runs one instance per attached client under one id. A bar that
  hears from a lower one defers for two ticks; one that stops hearing, because that bar's tab
  closed or it went hidden, takes the probe up within twenty seconds. A probe that fails, or that
  its watchdog kills, still sends, with no payload: the other bars keep deferring, and no tick
  starts a probe in every bar at once. This was chosen over
  "probe only from a bar whose tab has a client" because it needs no knowledge of tabs or clients,
  and `Visible` alone did not hold the bars back: a three-tab scratch session on v0.46.0 with one
  client attached ran three probes every tick. With the pipe, the same session probes once per
  tick (measured), plus about one probe per bar in the second a client attaches, before any bar
  has heard from another.
- **Only a bar someone can see probes.** The bar still subscribes to `Visible`. A hidden bar skips
  its probes; a bar switched to probes at once, unless a probe is already out or a lower bar
  probes. A fully detached scratch session on v0.46.0 ran no probes at all (measured), and a
  client that attaches gets fresh numbers at once.
- **The macOS probe samples CPU with `iostat -n 0 -c 2 -w 1`, not `top -l 2 -n 0 -s 1`.** Both
  take a since-boot line and then a one-second sample, and the probe keeps the second. `top` walks
  every process twice: about 0.4 s of CPU a probe, and two more programs launched, on a Mac where
  an endpoint scanner inspects each launch. `iostat` costs no measurable CPU. The line is tagged
  `CPUIO`; the busy share is 100 minus its third field.

`default-plugins/slim-keybinds/src/vitals.rs` (`guarded`, `Lease`, `bar_key`, `ProbeGate`),
`default-plugins/slim-keybinds/src/main.rs` (`pipe`). Tests in `vitals.rs`, one of which runs the
watchdog against a hung body. The checked-in `slim-keybinds.wasm` is rebuilt. No contract change.
A native test build leaves out `register_plugin!`. The macro exports a `#[no_mangle] fn pipe`,
which shadows libc's `pipe(2)` there, and macOS `std::process::Command` calls `pipe(2)`. With the
macro in, the watchdog test ran the plugin's `pipe` and panicked reading stdin.

### `list-agents --json` rows say how to message the agent

```
zellij action list-agents --json
[{"handle": "brisk-heron", "kind": "claude", "name": "project-1", "agent_id": "…", ...,
  "message_to": "bridge:session_01AbC", "message_route": "harness",
  "chat_started_at": 1795082400000},
 {"handle": "calm-otter", "kind": "codex", ..., "message_route": "pane"}]
```

A supervisor told "send this to the agent in tab X on boxa" had a pane handle and no way to know
whether the agent there could take a message from its harness. Three keys are appended to every
JSON row. No key is renamed or removed, the table keeps its columns, and `action watch` never
carries them.

- **`message_to`**: the address a harness's messaging tool takes, from any machine. A claude
  connected to Remote Control writes `{"type":"bridge-session","bridgeSessionId":"cse_<X>"}` into
  its transcript, repeated through the file. The last one is current, and `cse_<X>` becomes
  `bridge:session_<X>`. Absent when there is no such record, when the id is not a plain token, and
  for every other harness: codex, opencode and pi have no bridge.
- **`message_route`**: `harness` when `message_to` is there. Otherwise `pane`: type into the pane,
  addressed by host/session and handle.
- **`chat_started_at`**: milliseconds since the epoch. The first `timestamp` in the transcript's
  first 64 KiB, or the file's birth time when that has none.

There is no context size. Every current harness has a large window, so a token count would mislead
a reader more than it helps.

All of it is undocumented harness output, read defensively: a missing, malformed or unreadable
record is a missing key, never an error. The read is bounded. The transcript is found the way
`last-reply` finds it, trying the project directory the agent's `cwd` maps to first. Its tail is
walked backwards in 256 KiB reads, at most 4 MiB, stopping once the address record is found. Lines are
matched as text before any is parsed. All transcripts are read on one thread, given 1.5 seconds
in total; a row not read by then goes out without its chat keys.

The keys are read on the machine that runs `list-agents`, from that CLI's `CLAUDE_CONFIG_DIR` or
`~/.claude`. `--all-sessions` and the MCP fleet, where each host runs its own `list-agents` over ssh,
carry them per host. A host on an older build simply lacks them.

`zellij-client/src/agent_reply.rs` (`ChatMeta`, `chat_meta`, `bridge_address`,
`find_claude_transcript`), `zellij-client/src/cli_client.rs` (`AgentRow`, `agent_rows`, in
`print_agents`). Tests: the address, the last of several records, a missing and a malformed record,
the head timestamp, a tail longer than one read, the cap, a line
longer than the cap, the cwd lookup, and rows with and without time to read. CLI-side only; no
contract change.

### `remote_hosts` takes a short alias for a host

```kdl
remote_hosts {
    from "~/.config/fleet/machines.tsv" column="hostname" alias_column="short" where="zellij_peer=yes"
    host "boxc.example.net" alias="boxc"
}
```

```
zellij_read_pane {"session": "boxc/work", "pane": "calm-otter"}   # reaches boxc.example.net
```

A host was reachable only by the name the block listed, so an agent that wrote the short name
everyone uses for a machine was refused. Now a host can carry one alias:

- **`alias="<name>"`** on a `host` line, and **`alias_column="<column>"`** on a `from` line. A table
  alias is lower-cased like the host name, and an empty cell is no alias. An inline `host` line
  that overrides a table row keeps the row's alias unless it gives its own.
- **`session` = `<alias>/<name>`** resolves to the host, ignoring case. ssh is always given the full
  name. An alias of this machine stays local, like its name.
- **An alias names one host.** One that matches a host's name, or an earlier host's alias, is
  dropped with a warning, and so is one ssh could not take as a host name. A missing alias column
  warns and costs the aliases, not the hosts. The hosts themselves always load.
- **Answers name the canonical host**, with `host_alias` beside it when there is one: on every
  `scope=fleet` row and `hosts` entry, and on any result from a remote call. A refusal for an
  unlisted host lists each host as `name (alias a)`.

**An older build** warns about `alias` and `alias_column` as unknown properties and ignores them:
`unknown remote_hosts host property "alias", ignored (expected bin)`. Its hosts still load; only the
alias does not resolve there. Checked against nkmk.40: `setup --check` still reports the config well
defined, since the main config parser does not read inside `remote_hosts`. A build from before the
block ignores it as a whole.

`src/mcp/remote.rs` (`RemoteHost.alias`, the two properties, `find`, `alias_of`, `route`, the
clash check), `src/mcp/fleet.rs` (`merge`), `src/mcp/mod.rs` (`with_host`). Tests: an inline alias,
a clash with a name and with another alias, a bad alias, routing by alias, a local alias, the
refusal's list, a table's alias column, an inline override keeping the row's alias, and a missing
alias column. MCP-side only; no contract change.

### Agent listings are grouped by host, tab and pane, and say how to message a row

```
zellij_overview {"scope": "fleet"}
To message an agent, call zellij_send with to = host/tab, host/handle or the agent's name ...
[{"host": "boxa", "host_alias": "a", "tabs": [
   {"session": "work", "tab": "develop", "panes": [
     {"handle": "brisk-heron", "kind": "claude", "name": "develop-1", "state": "idle", ...,
      "display_title": "Fix the build",
      "last_activity_age_s": 240, "chat_age_s": 5400,
      "address": {"session": "boxa/work", "pane": "brisk-heron"}}]}]},
 {"host": "boxb", "error": "ssh could not reach boxb (exit 255): ...", "error_kind": "unreachable",
  "tabs": []}]
```

An agent asked to "send this to develop on boxa" had a flat list to search and no rule for which
way a message goes. `zellij_overview` with `scope=agents`, `my_agents` and `fleet` now answers
grouped the way a person names an agent: host, then tab (with its session), then pane.

- **The text** is one instruction, `how_to_message`, then the grouped view as JSON. The
  instruction is static: message an agent with `zellij_send` (see "Agent messages wait until the
  agent is idle and nobody is typing"); "X on <machine>" is `to=<machine>/X`; do not wait for the
  answer, which arrives typed into your own prompt.
- **Each pane entry keeps only the keys that help a decision** (table below). `display_title` is
  the title, or `new session` when it is empty or only the harness's name, like claude's `Claude
  Code`. `address` is the `session` and `pane` the tools take. This machine's rows are addressed by
  their bare session, another host's as `host/session`. `scope=agents` rows gain `host` (and
  `host_alias`) in the grouped view.
- **The structure keeps its flat answer.** `result` is unchanged: the array for `agents` and
  `my_agents`, `{agents, hosts}` for `fleet`. It gains `grouped` and `how_to_message`. The flat
  rows stay for a program, and the prompt and resource `zellij://agents` still return them.
- **`scope=fleet` loses its one-line-per-agent text.** A silent host is now a host entry with
  `error` and `error_kind` (`unreachable` or `error`), in the text and in `result.hosts`.

| Pane key | The decision it helps |
|---|---|
| `handle` | Which pane to send to or type into. |
| `address` | The `session` and `pane` a tool takes, ready to pass. |
| `self` | Do not message yourself. |
| `kind` | Which harness: whether it reports state, and which routes it has. |
| `name` | The word a person uses for the agent; a `to` target. |
| `cwd` | Which project it works in; its directory name is a `to` target. |
| `display_title` | What it is working on now. |
| `state`, `state_kind`, `state_msg` | Busy, idle, or blocked on what: will a message be held. |
| `state_age_s` | Stuck in one state for too long. |
| `output_age_s` | Stalled while it says it is working. |
| `last_activity_age_s` | Idle long enough to hand it work, or to restart under it. The later of the end of its last turn and its last output. |
| `chat_age_s` | A long-running chat or a fresh one. |
| `queued` | Messages already wait for it; do not pile on. |

Every other row key stays in the flat `result` only. `pane_id`, `tab_id`, `tab_position`,
`tab_name`, `session`, `host` and `host_alias` are the groups above the pane. `title` becomes
`display_title`. `state_at`, `last_output_at` and `chat_started_at` become ages. `message_to`
and `message_route` stay flat: `zellij_send` reaches every agent, and a bridge id in the grouped
view read as a `to` target. `command`, `pid`, `source`, `session_source`, `agent_id`, `state_app`,
`state_title`, `state_progress` and `state_working` decide nothing a reader of the grouped view
does.

`src/mcp/fleet.rs` (`HOW_TO_MESSAGE`, `GROUPED_PANE_KEYS`, `grouped`, `display_title`, `with_grouped`, `merge`),
`src/mcp/mod.rs` (`group_agents`), `src/mcp/tools.rs` (the tip). Tests: the grouping order by
host, session and tab, the kept and dropped keys, the titles, the ages, local and remote addresses, a
row with no session, and the fleet merge's text and structure. MCP-side only; no contract change.

### The `zellij_write_input` hint names the address that reaches the agent

```text
this pane runs claude `project-1`; to message it, use zellij_send to=brisk-heron: it waits until the agent is idle and nobody is typing there, or your harness's messaging tool at `bridge:session_01AbC`
```

After a successful `zellij_write_input` into a pane that runs a coding agent, the hint points at
`zellij_send`, the guarded route, for every harness and every machine. A target on another machine
adds "with the same session". A row with `message_to` names that address as a second route. A
pane with no agent gets no hint. The hint reads the target's `list-agents --json` row; a pane
addressed by uuid is looked up by its handle through one `list-panes`, since an agent row has no
uuid.

`src/mcp/mod.rs` (`input_hint`, `agent_hint`, `looks_like_uuid`). Tests: an agent with and
without `message_to`, a local and a remote target, and the uuid shape. MCP-side only; no contract
change.

### Agent messages wait until the agent is idle and nobody is typing

```
zellij_send {"to": "boxa/develop", "text": "The build is green; go ahead."}
  # ssh boxa: zellij action send-message --to develop --all-sessions --from here/main/brisk-heron ...
{"id": "3", "status": "queued", "reason": "draft", "pane_id": "terminal_3",
 "handle": "sunny-otter", "session": "work", "agent": "claude", "from": "here/main/brisk-heron",
 "from_name": "builder", "from_tab": "main", "queued_at": 1795120865694,
 "expires_at": 1795122665694}
```

Typing a message into another agent's pane (`zellij_write_input submit`) pasted it into whatever
was in that prompt. A person halfway through a sentence there had the message and their sentence
sent as one prompt. The session that owns the pane now owns delivery: it types a message in only
when that cannot mix with a person's typing, and otherwise holds it.

**`zellij action send-message --to <target> [--reply-to <id>] [--from-person] -- <text>`** (`-`
reads stdin).

- **The target** is one word matched against `list-agents`: a pane handle or pane id, a tab name,
  an agent name, or the name of the agent's project directory, compared without case.
  `session/word` keeps the search to one session; `--all-sessions` searches every session on the
  machine. A handle or id wins outright. Otherwise the sender's own agent never answers, and
  exactly one other agent must: none or several
  is a miss, exit 2, with the candidates on stderr. A pane with no agent is refused: type into it
  with `zellij_write_input`. With `default_harness` in the config, a tab or pane with no agent
  gets one started instead; see [below](#a-target-with-no-agent-starts-the-default-harness).
- **The guard** runs on the server, and the message goes in only when all three hold:

  | check | default | why |
  |---|---|---|
  | the agent's `OSC 7501` state is `idle`, `done` or `error` | - | its turn is over; `error` reads as idle, as `wait --for agent-idle` reads it |
  | no unsent text in its prompt | - | the original failure |
  | no key reached the pane lately | 5 s | outlasts the pauses of a person typing; a draft holds a message however old it is, so this only covers the moment around a key |

  `working` holds (`reason: working`), `blocked` holds (`blocked`: a permission or question
  prompt is open and an Enter would answer it). An agent of any harness that has not reported
  a state yet holds (`starting`) for its first minute in the pane (`agent_mail::STARTUP_MS`,
  60 s: claude reports at its first prompt, after its MCP servers start, which can take half a
  minute on a cold machine). An agent past that minute that has never reported is refused
  (`no_state`), and a held message for it is settled as refused rather than kept to its expiry:
  its hooks are missing and nothing will ever say it is idle. The refusal names `zellij setup
  --agent-hooks`, which prints the hooks for every harness. The session starts the clock when
  its process tick first sees the agent in the pane, or at the first message to it, whichever
  comes first. The clock starts again when the pane's program asks `OSC 7501 ; ?` (it is
  starting) or its foreground command changes; another harness in the same pane is a new agent.
  An agent that has reported a state even once is never refused as `no_state`: a query, a full
  reset (RIS) or the harness's own `state=clear` leaves it with no state, and a message to it is
  held as `starting` until it reports again. A state the pane already shows when the session
  first sees the agent counts as one of its reports, so an agent that reported before the first
  process tick is never refused as silent either.
- **The draft** is read from the bytes that reach the pane, the same ones that set
  `input_since_state`: characters and pastes add, Backspace and Delete take away, Ctrl c and
  Ctrl u clear, cursor moves change nothing, and a key whose effect cannot be known (Up, Tab, Esc
  on an idle agent, an Alt chord) makes the draft unknown, which holds until the agent next
  starts working. Enter on text holds for up to 10 s until the agent says it took the line.
  Only the start of a turn takes a draft up: a harness that reports `working` again mid-turn
  (codex after every tool call) leaves text typed meanwhile pending.
  Both keyboard encodings are read, legacy and the kitty protocol.
- **Atomic.** The check, the paste and the Enter are one step on the screen thread, where every
  key a client sends is also handled. The paste goes out as one write, and the Enter 150 ms later
  as a write the pty writer holds back; any write to that pane queued after it - a key pressed
  meanwhile - waits behind the Enter. Other panes are not held.
- **Held messages** are kept by the receiving session, one JSON file each under
  `$XDG_STATE_HOME/zellij/messages/<session>/`, so they survive a restart of its server. The server
  tries them once a second, in order, one per pane per round; a message just delivered counts as
  the agent's turn until it reports one. A held message expires after `--expire-s` (1800 by
  default); `0` means deliver now or refuse. A pane is found again by its handle, so a restored
  pane gets its messages. The last 100 settled messages are kept for a week, for `--reply-to`.
- **The envelope** is one line, marked as another agent's (Claude Code gets it as attributes,
  below):

  ```text
  [message 3 from builder (tab develop, boxa); not from the person. Reply: zellij_send reply_to=3, or to=boxa/work/brisk-heron]
  ```

  It names the sender as the grouped agent listing does: the agent's name (its pane's handle when
  it has none), its tab and its machine. It offers two ways back: `reply_to=<id>`, which the
  delivering session resolves, and a `to=` address, `host/session/handle`, which resolves without
  the id and names exactly one pane. A sender outside any pane gets `reply_to` only. The tab comes
  from the sender: `--from-tab`, by default the sending pane's own tab. `zellij_send` passes it,
  and sends again without it to a machine whose build does not know the flag; that envelope names
  no tab. Control characters other than newline and tab are stripped from the text and from every
  name, so neither can end the paste or press a key.
- **The origin.** `--from-person` says the text is the person's own words or request, relayed by
  the sending agent. The sending CLI names the person from its own config, `person_name "Sam"`
  (a top-level key; unset, the name is `user`), and the envelope says so in place of "not from
  the person": `[message 7 from builder (tab develop, boxa); directly from Sam, sent through
  another session. Reply: ...]` (for Claude Code, `origin` and `note` attributes). The record
  carries it as `from_person`. `zellij_send` takes it as `from_person: true`; its description
  says to set it only for words the person asked to relay.
  A machine whose build refuses the flag is sent the message again without it, as for
  `--from-tab`, and that envelope says "not from the person". The session never reads
  `person_name`: it is not in the protobuf `Options`.
- **The shape follows the receiving agent's kind.** Every kind but Claude Code (codex, opencode,
  pi, and an agent of unknown kind) gets the envelope line with the text below it. Claude Code
  gets a cross-session wrapper, with the whole envelope in attributes and the text alone inside:

  ```text
  <cross-session-message from="zellij:boxa/work/brisk-heron" from-name="builder" from-tab="develop" origin="Sam" note="directly from Sam, sent through another session" message="7" answering="6" reply="zellij_send reply_to=7" reply-to="boxa/work/brisk-heron">
  Run the tests.
  </cross-session-message>
  ```

  Claude Code's UI reads a paste as a message from another session only when the opening tag is
  the very first thing in it. It collapses a one-line message to "Message from @builder"; a
  message of several lines shows the raw wrapper, which the model still reads correctly. So the
  wrapper holds the text and nothing else.
  - `from` is `zellij:` and the sender's address, or `zellij:command-line` for a sender outside
    any pane. `from-name` is the name the envelope line gives.
  - `from-tab` is there when the sender named its tab, `origin` and `note` only with
    `--from-person`, `answering` only for a reply, and `reply-to` only for a sender in a pane.
  - The way back is two attributes, `reply` and `reply-to`. An attribute value holding two `=`
    stops the UI recognising the tag, so `reply_to=7, or to=...` is never one value.
  - An attribute value loses its newlines, and `"`, `<` and `>` become `&quot;`, `&lt;` and
    `&gt;`. A `</cross-session-message` in the text, in any case, becomes
    `&lt;/cross-session-message`, so the text cannot close the wrapper.
- **Message ids** are a small number counted per receiving session: `1`, `2`, ... `reply_to` is
  resolved by the session that delivered the message, so an id only has to be unique there. The
  counter is saved beside the held messages (`next-id`) before an id is used, so a restart of the
  server never gives one twice; a lost counter file restarts past the highest id still on disk.
  An id an older build gave (`m3k9x2q`) still resolves.
- **The answer** is one record (`--json`, or a line): `delivered`, `queued` with a `reason`,
  `refused`, or `expired`. Exit 0 for delivered and queued, 2 otherwise. `--wait <s>` blocks until
  a held message goes in. It waits for delivery only, never for an answer.
- **Do not wait for the answer.** The reply comes back as a message too, typed into the sender's
  own prompt once the sender is idle. A sender that blocks on `zellij_wait_for`, or polls, freezes
  itself for the whole round trip. The `zellij_send` tips, `how_to_message` and the server
  instructions carry one static sentence that says so; the tool names no follow-up. **`zellij action message-status [<id>]`** prints the held records, then
  the settled ones. `--reply-to <id>` alone looks up that message's sender in this session and
  sends there; a sender on another machine is refused with the command to run (the MCP tool
  routes it). "This session" is `--session`, or the replying pane's own `ZELLIJ_SESSION_NAME`,
  never the one session that happens to be running: ids repeat from session to session, so an
  id missing there is a miss (exit 2), not a match in another session.
- **The listings** show held messages: `list-panes --json` rows gain `queued_messages`, and
  `list-agents --json` rows gain `queued`, each only when there are some. The pane frame shows
  nothing: the frame is drawn from state the screen does not push for this, and the listing is
  where a supervisor looks.

**`zellij_send`** is the seventh MCP tool: `to`, `text`, `reply_to`, `expire_s`, `wait_s`,
`from_person` and `session`. `to` may start with a host or its `remote_hosts` alias -
`boxa/develop`, `boxa/work/develop` - and the tool runs `send-message` on that machine over ssh, so the message is
held there. With no session named, every session there is searched. The sender is this server's own
pane (`host/session/handle` and the agent's name). `reply_to` alone looks the message up in this
session - the server's own pane's, else the one it was started for, never another - and routes
the reply to its sender's host. With no session of its own it refuses (`reason: miss`). A machine whose zellij has no `send-message`
is refused (`reason: remote_too_old`) rather than typed into: the unguarded paste is the failure
this exists to stop. The server instructions and `how_to_message` name `zellij_send` as the way to
message an agent; `message_to` stays in the flat `list-agents` rows as the harness's own route.
The record's own `reason` (`working`, `typing`, `not_agent`, ...) goes at the top of the answer,
so the tool's output schema lists those values beside the failure kinds and `remote_too_old`. A
queued message once failed the schema check, and the caller read a delivered-later message as an
error.

Contract: two `Action` messages, `SendMessageAction` (tag 900) and `MessageStatusAction` (tag 901),
in the fork block from 900, clear of upstream's numbering. A stock client
cannot send them; a fork client against a stock server of the same contract gets nothing for these
two actions. `SendMessageAction` gained `optional string from_tab = 7`, a field of a message the
fork owns; a server from before it ignores the field and names no tab. It gained `optional
string from_person = 8` the same way; a server from before it ignores the field and says "not
from the person". No plugin-API change.
Capability: `agent-send-message`.

Limits:

- The draft is an estimate from bytes. Text that reaches the prompt without a key - a harness
  restoring a draft on its own, a paste into the host terminal that bypasses zellij - is not seen.
- A program that forges an `OSC 7501` `idle` over a dialog can let a message's Enter answer it.
- A held message names its pane by handle; a session renamed while messages are held leaves them
  in the old name's directory.
- An agent that never reports again (a codex whose hooks were removed) keeps a held message until
  it expires.

Files: `zellij-utils/src/agent_mail.rs` (record, envelope, guard, address),
`input/actions.rs`, `cli.rs`, `cli_surface.rs`, `capabilities.rs`, `data.rs`
(`queued_messages`), `client_server_contract/common_types.proto`, `ipc/protobuf_conversion.rs`;
`zellij-server/src/agent_mail.rs` (`Draft`, `Mailbox`), `pty_writer.rs` (`WriteNotBefore`),
`program_status.rs`, `panes/grid.rs`, `panes/terminal_pane.rs`, `tab/mod.rs` (`AgentPrompt`),
`screen.rs` (send, drain, ticker), `route.rs`; `zellij-client/src/agent_send.rs`,
`cli_client.rs`; `src/commands.rs`; `src/mcp/send.rs`, `mod.rs`, `tools.rs`, `fleet.rs`,
`invoke.rs`; `input/options.rs` and `kdl/mod.rs` (`person_name`). Tests: the guard table, the
envelope and text cleaning, the origin, the Claude wrapper and its escaping, the shape per kind,
the draft on every key class, the mailbox's save, restore, expiry and pruning, the pty writer's held write, target resolution
(names, tabs, projects, ambiguity, sessions, a shell pane), the MCP route, sender, command line
and old-build detection, and on the screen: delivery to an idle agent, a key after it, a draft
held and left alone then delivered after its turn, a cleared draft, the quiet window, and the
working, blocked, shell and no-wait cases.

### A target with no agent starts the default harness

```kdl
default_harness "claude"
```

Without the key, `send-message` refuses a target that names no agent, as above. With it, a target
that names **one tab, or one pane, with no agent in its tab** gets the named harness started there,
and the message becomes its first prompt:

1. **Where.** A named pane that is an idle shell takes it. Otherwise the tab's one idle shell takes
   it. With no idle shell, or with several, a new pane opens in that tab without taking anyone's
   focus, and its shell types the command once it is ready. A new tab is never made: a tab name
   that matches no tab is refused as before.
2. **An idle shell** is one whose own `OSC 133` markers say it is at a prompt, with nothing typed
   since (`input_since_prompt` false) and no restored command waiting (`prefilled_command`). A shell
   without markers never qualifies: it cannot say whether a person typed at it. Two idle shells are
   not a pick, so they get a new pane.
3. **Typing.** The command is the bare name and Enter, as a person would type it. The pane is read
   again just before the write, and a shell that is no longer idle gets a new pane instead.
4. **Then the message.** The harness has 30 seconds to show up as an agent in that pane. The
   message then goes to it through the guard above, which holds it as `starting` until the harness
   reports `idle` and types it in. stderr names what was started and where. A harness that does
   not report within the guard's startup minute has the message refused as `no_state`
   (`message-status` shows it), and it stays running in its pane.

Refused, with nothing started (exit 2):

- a tab that already runs an agent, the caller's own included;
- a name that matches several tabs, or none;
- a harness this build does not recognise.

Any harness the build recognises may be started: with the hooks from `zellij setup
--agent-hooks` installed, opencode and codex report state as claude and pi do.

The key is read by name from the config file, the way the MCP server reads `remote_hosts`. It is a
top-level key, so an older build ignores it (a stock build) or warns and skips it (a fork build
since the tolerant-config release); see `docs/config-compatibility.md`. The value is the harness
name only; the fork never adds flags to it. `zellij_send` runs `send-message` on the receiving
machine, so that machine's `default_harness` is the one used.

Limits:

- The re-read and the write are two requests to the server, not one step. A key that lands in
  that shell between them mixes with the command.
- A shell that redraws its prompt resets `input_since_prompt`. bash redraws on a resize, so a line
  typed but not entered, followed by a resize (a new pane in the tab is one), reads as idle.
- The first message waits out the draft the typed command leaves (about 10 seconds), then goes in.

`send-message` also stopped listing an agent twice when a plugin pane and its terminal pane share
a number: the agent row is matched to terminal panes only.

Capability: `send-default-harness`.

### Fork protocol tags live in a reserved block

Every protobuf tag the fork owns sits at 900 or above, so a rebase onto a newer upstream never
renumbers one. Upstream numbers its tags upward from 1; a fork tag just above upstream's maximum
collides as soon as upstream adds the next message or field.

The rule covers every fork field or value inside an upstream message or enum, in both contracts.
Field numbers inside a message the fork owns outright (`ReopenPaneAction`, `NewShellPaneAction`)
stay small: no upstream message shares their numbering.

| Where | Was | Now |
|---|---|---|
| client/server `Action`: `resolve_pane_target`, `list_tree`, `new_shell_pane`, `reopen_pane` | 167, 168, 173, 174 | 902, 903, 905, 906 |
| client/server `Action`: `move_tab_to_index` | 172, then 904 | reserved: `move-tab --to-index` maps onto upstream's `MoveTabToPosition` |
| client/server `Action`: `send_message`, `message_status` | 900, 901 | unchanged |
| client/server `Action`: `close_tab_or_confirm` | new | 907 |
| `ClientToServerMsg.fork_subscribe_to_session_events` | 28 | 900 |
| `ServerToClientMsg.fork_session_event` | 20 | 900 |
| `ListClientsAction.output_json`, `FocusPaneByPaneIdAction.no_focus`, `GoToTabNameAction.no_focus` | 1, 2, 3 | 900 |
| `CliAssets.tty` | 100 | 900 |
| `TiledPaneLayout.pane_handle`, `FloatingPaneLayout.pane_handle` | 16, 15 | 900 |
| plugin `PaneInfo`: `program_title` through `has_pending_bell`, `handle`, `tab_id` | 26-36, 46, 47 | 900-912, in declaration order |
| plugin `ModeUpdatePayload.session_warnings` | 23 | 900 |
| plugin `ClientInfo.tty` | 102 | 900 |
| plugin `Action.reopen_pane_payload`, `ActionName.ReopenPane` | 200 | 900 |
| plugin `ActionName.CloseTabOrConfirm` | new | 901 |
| `PaneFrameStyle.TopOnly` (`event.proto` and `pane_frame_style.proto`) | 100 | 900 |

The two session-event messages are renamed `ForkSubscribeToSessionEventsMsg` and
`ForkSessionEventMsg`, because upstream later takes the name `SessionEventMsg`.

Each freed number sits on a fork-commented `reserved` line of its own, never on upstream's line.
Most freed numbers lie in upstream's range. At a rebase, drop each one that upstream has taken. No
reserved line holds a number that v0.46.0 uses.

**This breaks the fork's contract numbering.** A fork client from before this cannot use a fork
action, `action watch` or `list-agents --watch` against a server from after it, and the reverse.
Upstream actions keep working in both directions. A plugin built before this reads none of the
fork's `PaneInfo` fields and no `top_only` style; the checked-in plugins are rebuilt with it.

### A change that adds a home path, a private address or a tailnet name fails CI

The `Leak scan` workflow (`.github/workflows/leak-scan.yml`) runs on every pull request and every
push to `main`, with no path filter: a doc is where such a leak most often lands. It runs
`.github/scripts/leak-scan.sh` over the lines the change ADDS and over the message of every
commit in it, so content already in the tree never fails it:

- a home directory path, `/home/<name>` or `/Users/<name>`, with a slash after it or ending the
  path (end of line, a quote, a space, a colon);
- a private IPv4 address (10/8, 172.16/12, 192.168/16) or a Tailscale CGNAT one (100.64/10);
- a `*.ts.net` name.

The rules are generic; the repo is public. `leak-scan-allowlist.txt` beside the script lets
placeholder names and upstream's own fixtures through, one regex per line against `<path>:<match>`.
Snapshots get no blanket pass: upstream's authors' paths pass by name, so a snapshot regenerated
on another machine fails. A pull request scans its merge commit against the base; a push scans
from the commit before it, so a forced push (a rebase onto upstream) scans the whole rewrite, and
a branch's first push scans everything it holds that the default branch does not. Upstream
content it flags then goes into the allowlist. The scan fails closed: a `git diff` that fails, or
a push base the workflow cannot fetch, fails the job rather than scanning less.
`leak-scan-test.sh` runs first and checks the scanner against crafted diffs and a scratch
repository.

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
- **Truncating `zellij_overview scope=panes`.** It hands back the whole `list-panes --json`, around
  thirty keys per pane including geometry nobody asked for, and it is the tool the server's own
  instructions say to call first. A `verbosity` parameter, or a default projection down to the keys
  that address a pane, would be the fix. Not done because it is a new shape for the tool to return
  rather than a flag to pass through, and every drift gate here rests on a tool returning exactly
  what its CLI command prints.
- **A plugin column on `zellij ls --json`.** The metadata `ls` reads now carries each session's
  plugins, but `SessionListEntry` does not, so nothing is printed. Three parsers read that array
  and the question `ls` answers is which sessions exist, not what runs inside one — the detail is
  on `SessionInfo`, where a caller that wants it already is.

## Working on this fork

`upstream` points at `zellij-org/zellij`. Between rebases each patch is its own commit on top of
the recorded upstream base, currently the **`v0.46.0`** tag. Moving to a newer base merges the
series onto the new tag, resolves the merge, and carves the result into one commit per area on the
tag (`docs/fork-history.md`). Then it records the new base here.

**A moved base is also two consts.** `UPSTREAM_BASE_TAG` and `UPSTREAM_BASE_COMMIT` in
`zellij-utils/src/consts.rs` are what `zellij --version` prints, and nothing derives them — update
both when the base moves. The commit is `git rev-parse --short '<new-tag>^{commit}'`; upstream
tags are annotated, so `--short <new-tag>` gives the tag object instead and is the wrong answer.

The base is the **`v0.46.0`** tag (`aedee21cb`). `v0.45.1` sits on a release branch that v0.46.0
does not contain; the two meet at `18cb94e1c`. Where upstream now does what a fork patch did, the
fork keeps upstream's code and its entry here says what the fork still adds. A patch with nothing
left moved to [docs/removed-patches.md](docs/removed-patches.md).

Upstream has its own opt-out for scroll-mode sync (`748a6fa9a`, #5532), a `scroll_mode_sync`
boolean, where the fork carries the three-value `input_while_scrolled`. Both are kept and both are
checked, upstream's first (`implicit_scroll_mode_sync_enabled` in `zellij-server/src/screen.rs`);
each defaults to upstream's behaviour.

```
cargo build --release
cargo test -p zellij-utils -p zellij-server
```

The toolchain is pinned by `rust-toolchain.toml`; rustup installs it on the first cargo invocation.
Debug builds expect the WASM plugins to be built from source — use `--release`, which uses the
prebuilt assets checked into `zellij-utils/assets/plugins/`.

## Releasing

Each release gets a changelog at `changelogs/v<version>.md` — the commit range plus authored notes
saying what changed and why. Write it before tagging; it is the single source of truth for what
a release holds.

A release tag is never moved. The series on `main` was condensed on 2026-10-06 (`docs/fork-history.md`,
"Reordered history does not bisect"), so every tag up to `v0.45.1-nkmk.30` points at a commit on
`backup/main-pre-condense-2026-10-06` rather than on `main`. The tree at each tag is the tree
`main` had at that release; `git diff v0.45.1-nkmk.30 <condensed tip>` was empty when the rewrite
was pushed.

`.github/workflows/release.yml` replaces upstream's release job. Pushing a `v*` tag builds two
targets — `x86_64-unknown-linux-gnu` and `aarch64-apple-darwin` — and attaches
`zellij-nkmk-<version>-<target>.tar.gz` (the bare binary) plus a `.sha256` to that tag's release,
creating the release if it does not exist. Nothing else is published: no musl, no linux arm64, no
intel mac, no Windows.

**The macOS binary is signed and notarized by CI.** The `aarch64-apple-darwin` leg signs it as
`Developer ID Application: NKMK Digital Co. (2Z88BYP37C)` with identifier `org.zellij.nkmk` and the
hardened runtime, submits it to Apple's notary service, and fails unless the verdict is `Accepted`.
Then it packages the signed file; the tarball's name and layout do not change. A bare binary cannot
be stapled, so Gatekeeper fetches the ticket online. RC tags take the same path, so a candidate
proved on a Mac carries the real signature. The secrets live in the Actions environment `release`,
whose policy admits `v*` tags only: `MACOS_CERT_P12`, `MACOS_CERT_PASSWORD`, `ASC_KEY_P8`,
`ASC_KEY_ID` and `ASC_ISSUER_ID`. Only the macOS leg enters that environment, and only its signing
steps read them. From nkmk.30, a pin refresh installs a build that already carries this signature
as-is, with no local re-signing. After the one re-grant each Mac owes at the switch, TCC grants hold
across releases.

**`Rust` runs on the branch, not only on main.** Its push trigger lists the conventional-commit
prefixes this fork names branches after — `feat/`, `fix/`, `ci/`, `chore/`, `docs/`, `perf/`,
`refactor/`, `style/`, `test/`, `rc/` — so a patch goes green where it is written rather than after
it lands, which is what "prove it on the branch" needs. The list is enumerated rather than `**` so
that `backup/` snapshots and `rebase/` scratch branches do not each burn a full matrix, and a
`rust-${{ github.ref }}` concurrency group cancels a superseded push. From nkmk.31 that applies to
topic branches only: a run on `main` or a tag is never cancelled, in `rust.yml` or `e2e.yml`,
because a release needs green runs on the exact tagged commit. `e2e.yml` is deliberately not
on that list — it builds a binary and drives it through a docker terminal, and it is the slowest
thing here — but it now has a `workflow_dispatch`, so it can be aimed at a branch on demand.

**Dependabot opens no pull requests.** Upstream's `.github/dependabot.yml` is kept, with
`open-pull-requests-limit: 0` on every entry. A bot commit does not fit a patch series: dependencies
move when the base moves to a newer upstream tag. The file is edited rather than deleted, so an
upstream change to it conflicts on one line instead of on the whole file. Vulnerability alerts stay
on; automated security fixes are off in the repository settings.

A patch is proved as a **release candidate** before it lands. On its branch, bump the version it is
heading for, then tag `v<version>-rc.1` and push the tag. The pipeline runs exactly as below, with
three differences: the workspace version is patched to the whole tag before the build, the GitHub
release is marked `--prerelease`, and the tap bump rewrites `zellij-nkmk-rc` instead of
`zellij-nkmk`. So the candidate can be installed on a real Mac while every machine keeps running the
last final release:

```
brew update && brew unlink zellij-nkmk
brew install noahkiss/tap/zellij-nkmk-rc
zellij --version
```

and, when the proof is done:

```
brew uninstall zellij-nkmk-rc && brew link zellij-nkmk
```

`-rc.2`, `-rc.3` follow the same way. The version the binary reports is the whole tag, `-rc.N` and
all — see [An RC binary reports the RC](#an-rc-binary-reports-the-rc). Once the candidate holds up,
squash-merge the branch and cut the final tag from main:

1. Land the patches, bump the workspace version in `Cargo.toml` (and the `zellij-client` /
   `zellij-server` pins), `cargo build --release` once so `Cargo.lock` is current, commit.
2. `git push origin main`, wait for the **`Rust`** workflow to go green — it builds the plugins
   from source, which `Release` does not — then `git tag v<version> && git push origin v<version>`.
   Tags are immutable once a formula pins them — never move one.
3. Watch the run: `gh run watch -R noahkiss/zellij $(gh run list -R noahkiss/zellij --workflow=release.yml --limit 1 --json databaseId --jq '.[0].databaseId')`.
4. The tap bump runs itself. The `bump-tap` job dispatches `bump-zellij.yml` in
   `noahkiss/homebrew-tap` once `finalize` has published the release, then waits for it. That
   workflow rewrites the formulae from the release's `.sha256` assets and proves them with a real
   `brew install` on macOS and Linux before it commits. No sha is transcribed by hand. Which
   formula it rewrites comes from the tag: `zellij-nkmk-rc` for a `-rc.` tag, `zellij-nkmk`
   otherwise. The tap re-derives the same rule and refuses a dispatch that disagrees.

   The dispatch needs `HOMEBREW_TAP_TOKEN` — a PAT with `workflow` scope on the tap, because this
   repo's `GITHUB_TOKEN` cannot dispatch another repo. Without it the job prints this fallback and
   passes, so a release never fails for want of the token:

   ```
   gh workflow run bump-zellij.yml -R noahkiss/homebrew-tap -f tag=v<version> -f formula=zellij-nkmk
   ```

5. Update the docs that live **outside** this repo. The operator's skill files record the current
   version and the install steps, and nothing in CI compares them against the tag — a stale one
   fails silently, describing the previous release while reporting success. They are part of the
   release, not tooling around it.

A pour of the prebuilt formula reinstalls in about 2 seconds. If a test install takes minutes
instead, it fell through to a source build because `brew` read a **stale local tap clone** — run
`brew update` (or pull the tap checkout) before testing a formula change made in the same session.

The release job builds only the two targets above. Intel macOS was dropped deliberately; if it is
ever restored, the runner label is `macos-15-intel` — GitHub retired `macos-13` in December 2025.

To rebuild an existing tag (a lost asset, a notary outage), dispatch it **on the tag**:

```
gh workflow run release.yml -R noahkiss/zellij --ref v0.45.0-nkmk.1 -f tag=v0.45.0-nkmk.1
```

The `release` environment admits tags only, so a dispatch from `main` fails the macOS leg and leaves
the release a draft. A dispatch on the tag runs the workflow file at that tag, not main's: a
workflow fix reaches a release through a new tag. `tap_only` skips the build, so it still works from
`main`.
