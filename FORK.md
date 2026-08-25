# zellij (noahkiss fork)

A personal fork of [zellij](https://github.com/zellij-org/zellij), rebased onto the upstream
**`v0.45.0`** tag (upstream workspace version **0.45.0**), carrying a curated patch set aimed at
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
  programs; the default output is for a human or an agent reading a shell. The goal is `--json` on
  every query and on every mutation that reports something. Seven have it — `ls`, `list-panes`,
  `list-tabs`, `list-tree`, `list-clients`, `list-events`, `current-tab-info`; `wait` and
  `are-floating-panes-visible` do not, and the payload commands never will. The mutations do not
  yet, so for those this bullet is a direction rather than a promise you can write a script
  against.
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
  stdout; it does not introduce it.

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
| `new-pane`, `new-tab`, `edit` | `pane_id:`, `handle:` and `tab_id:` where each applies, instead of a bare id. A pane no tab took is not reported at all: [the miss says so and exits 2](#a-pane-is-reported-only-once-a-tab-has-taken-it) |
| `break-pane` | `tab_id:` — the tab it made. The pane it moved kept the id and handle it already had |
| `launch-plugin`, `launch-or-focus-plugin` | nothing, exit 0. See [choosing a handle](#choosing-a-handle) for why |
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
| 2 | The focused thing, inside the session only | `rename-pane`, `rename-tab`, `undo-rename-pane`, `undo-rename-tab`, `edit-scrollback`, `resize`, `move-pane`, `move-pane-backwards`, `move-tab`, the `break-pane` family, `toggle-pane-embed-or-floating`, `toggle-pane-pinned`, `clear` | `clear` |
| 3 | Refused: name it | `close-pane`, `close-tab`, `write`, `write-chars`, `send-keys`, `paste` | `close-pane`, `close-tab` |

**Class 1 is additive**, and placement relative to wherever you are is the whole point of it. A
script calling `new-pane` is asking for exactly that. A verb whose target clap already requires is
class 1 too — `toggle-pane-borderless` cannot be called without one, so there is no absence left for
a class to reason about.

**Class 2 is the recoverable half.** Inside a pane, "focused" is the pane your hands are on and
nothing changes. From a script it exits 2 — a refusal changes nothing — and names the `--pane-id`
or `--tab-id` that answers it.
`break-pane-right` and `break-pane-left` cannot name a target at all, so from outside they are
refused outright, pointing at `break-pane --pane-id`.

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

- **`signal-pane`** — the signal's NAME is the stated intent, and the target is already explicit.
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
the tab as it names any new tab. `--cwd`, `--close-on-exit`, `--start-suspended`, `--plugin`,
`--handle` and `--no-focus` all mean what they mean elsewhere; `--name` and `--borderless` do not
travel, because the first pane of a new tab is described by its command and nothing else - both are
refused rather than dropped. Name the pane with `--handle`, or rename it once it is there.

`--new-tab` is the one placement flag that **does** move the focus: whoever is attached is taken to
the new tab, the way `new-tab` itself takes them. That is what a caller opening a tab usually means,
and `--no-focus` is how to say otherwise. `--near` and `--in-tab` never move anybody.

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

Three conditions, and a `--timeout` on all of them:

- **`--for exit`** — the pane's command ends. It prints `exit_status:`, and `-` where the pane
  closed and took its status with it.
- **`--for quiet`** — the pane produces nothing for `--quiet-ms`, 500 by default. What a shell
  looks like when it has finished printing and is waiting for you again.
- **`--for match --match <regex>`** — a line the pane delivers matches. Rust regex syntax,
  unanchored.

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
  screen a prompt printed twice is the same line twice.

`--for exit` is the one condition that is a poll rather than a subscription — every 250ms, and
nothing about it reaches the protocol. The render stream reports a pane *closing*, and a command
pane that ends is held open instead by default: same event to a script, no message at all on that
stream. Asking the session is what covers both, and it covers a pane that closed outright as well.

`wait` is in the `read` band. It changes nothing — the band says what a verb does to the session,
not how long it takes — which is also why the [audit ring](#list-events) does not record it.

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
- **It can be chosen** — `zellij action new-pane --handle build`, or `handle "build"` on a pane in a
  layout — and otherwise the pane names itself. See [choosing one](#choosing-a-handle) below.
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
`is_spoken_for` for generation). A restored or chosen handle is exempt from this check — it is taken
verbatim by `HeldHandle::claim` — so reservation is a property of *generation*, not of the registry
as a whole. One consequence worth knowing: since each generated pane spends one adjective and one
noun, a session can hold at most 504 word-reservation-generated panes at once (the shorter of the two
lists) before every adjective is spoken for and generation falls to the suffix ladder below.

`generate_handle` tries three rungs before it gives up on the fixed-width space: up to 32 random
draws against the caller's predicate, then a deterministic sweep of the whole pair space starting at
a random offset (so an unlucky run of rerolls still finds a free pair, in bounded time), and only if
every pair in the space is refused does it fall back to the pre-existing numeric suffix — `-2`,
`-3` and so on — so generation always returns something.

#### Choosing a handle

```
zellij action new-pane --handle build -- cargo watch
zellij run --handle build -- cargo watch
zellij edit --handle notes ~/notes.md
zellij plugin --handle board -- file:/path/to.wasm
```
```kdl
layout {
    pane handle="build" command="cargo" { args "watch" }
}
```

**Every command that reports the pane it made takes it**, on the same terms: `new-pane` and `edit`
under `zellij action`, and the `run`, `edit` and `plugin` shorthands beside them. A caller should
not have to know which verb made the pane to know whether it could have named it.

`action launch-plugin` and `action launch-or-focus-plugin` are the exception, and the reason is
worth writing down: **they print nothing at all.** A plugin's pane is built on the plugin thread
after the action has already been answered, so nothing comes back carrying an id - and a chosen
handle is applied to the pane the report names. The surface map used to promise `pane_id:` and
`handle:` for both; it never printed either, and the promise has been removed rather than the
reader being taught a report that never arrives. `zellij plugin` goes through the pane-creating
path instead, reports what it made, and takes `--handle`.

A generated handle is memorable but not predictable, and a script that wants to reach the pane it
just made has to read the id out of the report and carry it. A chosen handle is the other way round:
the caller decides the address before the pane exists, and every later command already knows it.

- **The grammar** is up to four lowercase words joined by dashes, each at most 16 characters, at
  most 40 in all, at least one letter somewhere. That is what keeps one `--pane-id` able to take
  four forms: `terminal_1` has an underscore, `7` is all digits, a uuid is five groups. A name that
  reads like one of those is refused when it is typed, not when it is used. So is a name whose first
  word is `terminal` or `plugin` — `terminal-1` beside `terminal_1` is a name that gets typed wrong
  on the day it matters.
- **A handle a live pane already holds is an error**, exit 1, and nothing is created. A generated
  handle rerolls around a collision; a chosen one must not, because the caller asked for *this*
  name and a different one would answer a question nobody put. The check happens before the pane is
  made, so the refusal costs nothing.
- **The pane names itself first.** A pane is born with a generated handle and the chosen one is
  given to it immediately after, by the client that holds the report — so `handle:` in the report is
  always the name the caller asked for. This is also why `--handle` cannot ride with the blocking
  family: those answer with an exit status instead of a report naming a pane.
- **It is stored like any other handle**, which means it survives a restore: the snapshot carries
  `pane_handle="build"` and the pane comes back at that address. It also survives the trip to the
  server, which is what makes `new-tab --layout` and `zellij --layout` honour a `handle` a layout
  wrote: the client/server contract carries it alongside the pane's other declared properties.
  `restored_from` deliberately does not cross - that is provenance the server assigns, and a sender
  declaring it would be claiming a history it does not have.
- **In a layout**, `handle` is the spelling a person writes and `pane_handle` is what serialization
  writes; both reach the same field, and a saved layout keeps working. A layout that names a handle
  another pane in the same layout already took loses the tie rather than failing the restore — a
  session coming back is not a place to refuse work — so the last pane to ask gets a generated name.

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
text. The `quit_and_resurrect_session` and `resize_terminal_window` e2e snapshots almost certainly do
need it too, since they cross a real frame render, but this box has no `musl-gcc` to build the e2e
binary that would prove it — confirm and refresh them in CI before relying on that pair.

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

### Pane notes

```
$ zellij action set-pane-note build --color warn "waiting on review"
note: waiting on review
color: warn

$ zellij action set-pane-note build          # no text clears it
note: -
```

A short line drawn on the pane's frame, saying what is happening in that pane. The handle answers
"which pane is this"; the note answers "what is going on in it", which is the question a session
full of agent-driven panes leaves a human asking.

- **Four colours, named for what they mean** — `--color error|warn|ok|info`, `info` by default.
  They are not colour values: the note is drawn inside whatever theme the reader is using, and each
  name maps onto the colour that theme already picked for that meaning, so a note is legible in all
  of them.
- **The server leaves one itself.** A command pane whose command exits non-zero and is held open is
  marked `exit 7`, in `error`. The frame already said `EXIT CODE: 7` while the pane was held, but
  that line goes when the pane is scrolled or re-run, and nothing outside the server could read it
  at all. The note is the durable mark, and `list-panes` prints it. It is cleared when the pane is
  re-run, or by hand.
- **`list-panes` has a `NOTE` column and `list-tree` a `note:` field**, both `error:exit 7` —
  colour and words together, because the colour is the meaning and a table without it cannot tell a
  pane that finished from one that failed. A pane with no note prints `-`.
- **A note is not saved into a snapshot.** It describes the pane's live state, and a session
  restored tomorrow would come back carrying yesterday's "waiting on review". A restored pane comes
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
  handle: rapid-bass  pane_id: plugin_0  title: zellij:link  command: zellij:link  focused: false
tab_id: 1  position: 1  name: logs  active: false
  handle: cunning-filly  pane_id: terminal_1  title: Pane #1  command: -  focused: false
```

Every tab with its panes nested beneath it — the join of `list-tabs` and `list-panes` that you
otherwise had to do by tab id yourself. A tab with no panes still gets its line.

`--json` is the same join rather than a summary of it: each tab exactly as `list-tabs --json`
reports it, with its `list-panes --json` entries under a `panes` key. Asking for the tree never
returns less than asking the two questions separately would.

### `list-events`

```
$ zellij action list-events
AT                        VERB        TARGET                  ORIGIN    COUNT
2026-08-14T18:03:12.345Z  go-to-tab   tab_1 logs              client 1  1
2026-08-14T18:03:14.902Z  scroll-up   terminal_3 sunny-otter  client 1  38
2026-08-14T18:03:19.881Z  close-pane  terminal_3              cli       1
```

Who moved my tab. In a session a person and several agents are all driving at once, something
changes and nobody knows which of them did it — and every other query in this fork answers what the
session *is*, not what happened to it. The server keeps the last 256 things that changed, in
memory, and this reads them back oldest first, so the end of the table is now.

- **`ORIGIN` names the three ways an action can arrive**: `client 1` is somebody's keyboard,
  `cli` is a `zellij action` call, `plugin 3` is a plugin. All three pass through the one function
  that routes an action, so the ring sees keyboard and script alike rather than only the half a
  script produces.
- **`TARGET` is the name that was true at the time** — `terminal_3 sunny-otter`, `tab_1 logs`,
  resolved as the action completed rather than when you read it. A pane the action closed has no
  handle left to print, and shows its id alone. `-` means the action named nothing.
- **A run of the same verb, on the same target, from the same origin is one row with a `COUNT`.**
  A held scroll key is 38, not 38 rows: without that, the ring's whole capacity is one keypress and
  the tab move you came looking for has already fallen out of it.
- **`--json` carries the same rows**, structured.

What is deliberately not in it:

- **The `read` band**, which by definition changed nothing. The bands `zellij action --help` is
  grouped by are the same list this reads, so the two can never disagree about which verbs matter.
- **Keystrokes typed into a pane.** Every one is a `write`, and they would be the whole ring. The
  same verbs *from the CLI* are recorded: a script writing into a pane is an event with an author
  worth finding.
- **Actions that failed.** The ring remembers what happened, not what was asked for.
- **The CLI's own plumbing** — the target lookup every addressed call makes before it acts, and the
  naming step that finishes a `new-pane --handle`. Each would put a row beside every real one,
  describing it.

`VERB` is the action the session ran, spelled the way the CLI spells its verbs wherever the two
agree. One CLI verb can become several actions — `new-pane` is a `new-tiled-pane` or a
`new-floating-pane`, and the ring says which — so it is not always the exact word that was typed.

One thing to know about creations: a pane made with `--handle build` is recorded under the name it
was *born* with, because the chosen name is given to it a moment later, by the client holding the
report. The `set-pane-note` or the `close-pane` that comes after says `build`.

This is a ring, not a log: it is bounded, it is in memory, nothing about it is written to disk, and
it is gone when the server stops. It is for the question you are asking now.

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

This adds an `Action` and therefore a message to the client/server contract (tag 172), without
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

### `is_focused` names a client that is still here

A pane is reported focused only while a **connected** client focuses it. `zellij action list-panes`
used to answer `FOCUSED=true` for a pane the last client had been on before it detached — and on a
session nobody had ever attached to, because `session up` creates one and leaves. A consumer asking
"is anyone looking at this pane" got a yes from a session with no clients at all. Upstream
[#5468](https://github.com/zellij-org/zellij/issues/5468).

The stale answer and the useful behaviour come from the same map. `Tab::remove_client` drops the
client from `connected_clients` and deliberately leaves its entry in `ActivePanes`, which is what
puts a re-attaching client back on the pane it left. So the map is right and the question asked of
it was wrong: `pane_info` now asks
`ActivePanes::pane_id_is_focused_by_connected_client`, which is the same lookup with the connected
set applied. Upstream's own PR for this issue deleted the entry instead and re-attach went back to
the first pane in the tab; nothing here moves, and a detach-and-re-attach still lands on the pane
it left.

- **`is_fullscreen` stays on the unfiltered answer.** Fullscreen is the tab's shape, not a claim
  about who is looking: a detached tab left fullscreen still is one.
- **The session snapshot is untouched.** `PaneLayoutMetadata` builds its own `focused_clients` from
  the same map without the filter, so `session down` still records where the focus was.
- **This is `PaneInfo`, so plugins see it too** — the honest value, on a field they already read.
  No field is added, `event.proto` is not touched, and no `TryFrom` moves.

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

### A pane is reported only once a tab has taken it

The `--stacked` refusal above closes one case of a general problem, and this closes the rest of it.
Upstream reports a new pane **as soon as its pty spawns**, which is before any tab has agreed to
hold it. A tab that declines does so by dropping the pty rather than by failing, so the CLI printed
`pane_id:` and `handle:` and exited 0 for a pane that was never made, and the next command a script
ran missed:

```
$ zellij -s S action break-pane --pane-id terminal_0 --no-focus
tab_id: 1
$ zellij -s S action new-pane --in-tab 1 --handle ghost1
pane_id: terminal_1
handle: ghost1                        # exit 0, and terminal_1 is in no `list-panes`
$ zellij -s S action wait ghost1 --for exit
No pane answers to 'ghost1'
```

The id is now written into the report **after** the placement, and only for a pane some tab really
holds. A miss says so and exits 2:

```
$ zellij -s S action new-pane --in-tab 1 --handle ghost1
No pane was created: the session took the request and reported no pane.
```

It is one rule in two halves, and both are needed. The **session** stops asserting an id it only
asked for — `new-pane` in every placement, the in-place replacement, and a plugin pane. The
**client** holds each pane-making verb to answering with a pane: no `pane_id:` line from a verb
whose whole point is a pane means no pane, which is a miss rather than a silent success. Successful
creations are untouched and still print the id and the handle.

This is not a fix for one tab. An unsized tab is how it showed up — a tab no client had ever
attached to had no size to place a pane in, and `break-pane` makes one on a detached session — but a
targetless `new-pane` on such a session ghosted the same way, and so did `new-pane --plugin`. Any
future route that declines a pane is reported honestly without knowing about this.

The unsized tab itself is gone — [a detached session sizes its own
tabs](#making-a-tab-does-not-need-somebody-attached) — so that door no longer produces a miss. The
guard is what makes the next one safe, and it stays.

The exit is **2**: the session changed nothing the caller can address, so it sits in the same
bucket as every other miss (see [the CLI output convention](#the-cli-output-convention)). The stderr sentence says
which door it came through.

#### The miss has to be a real one

Holding the client to a report makes a lost report indistinguishable from a pane that was never
made, so a report the session wrote and the client never received now reads as a miss. That is what
`new-pane --handle` did, about one call in ten: the pane was there in `list-panes` under a generated
handle while the CLI said none had been made.

Nothing about the pane was at fault. `--handle` is checked against the live panes before anything is
created, and that check is a whole extra connection, made and closed immediately before the one that
carries the `new-pane`. A client id is the lowest number not in use, so in a session nothing is
attached to both connections are client 1 — and a route thread announced its client's removal twice,
once as the client left and once as the thread ended. The first announcement freed the id, the next
connection was given it, and the second announcement removed **that** connection's sender. Dropping
a sender writes an `Exit { Disconnect }` down its socket, which the waiting client read as the
answer to its `new-pane`.

So a route thread now announces its client's removal exactly once, after its last act, and the id
cannot be handed out while that thread can still reach it. The client also stops reading a
disconnect as an empty report: it is a dropped connection, said out loud, and exits 1 — the command
may well have run, and that is a different thing from a session that answered with nothing.

The same preflight is made by `--in-tab` and by `--near`, and `zellij run`, `zellij edit` and
`--plugin` all travel the same path, so all of them were exposed to it and all of them are covered.

### Session lifecycle: `zellij session up|down|restart`

```
zellij session up      [NAME] [--fresh | --restore [ID]]
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

`StartInterval` later grew a platform-neutral sibling, `watchdog_interval_secs`, which sets the
same interval on both init systems at once; this hatch still wins over it on macOS, and the entry
for that key says why.

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
load on any machine that was behind. `managed_session` and `launchd { env }` each paid that price,
and each entry above says so.

A name this build does not know is now **kept, logged and ignored**, at every depth this block
parses — an entry of `session_service`, a section of `systemd`, a child of `launchd`:

```kdl
session_service {
    managed_session true                       // read
    some_key_from_a_later_build "whatever"     // ignored, with a warning
    systemd {
        service "Nice=-5"                      // read
        sockets "ListenStream=1234"            // ignored, with a warning
    }
}
```

**Only the unknown-NAME case softened.** A name this build knows, given a value it cannot mean, is
still the whole config failing to parse: a wrong type, a `watchdog_interval_secs` of zero, a
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
systemd, launchd, pin_exe, managed_session, restart_via_launchd or watchdog_interval_secs)
```

and the parse also logs it at `warn`, so a session already running says it too. Every warning names
the entry it read and what the block does accept, so a misspelling reads as a misspelling.

An ignored entry is **not** written back out by `setup --dump-config`. It is not part of this
build's configuration, and a dump that re-emitted it would claim otherwise.

The rollout order this restores is the one AGENTS.md gives for a top-level key, now with no
exception for a nested one: the config can go everywhere first and the binaries follow in any
order. It does not extend to the **command surface** — a script or a unit calling a new subcommand
still breaks on every machine that has not upgraded.

Scope is `session_service` and its children only. Upstream's own strict blocks are untouched, and
so are the fork's other two — `pane_privacy` and `resurrect_command_hints` still refuse a child
they do not know, and have the same rollout problem waiting in them.

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
missing, unreadable, or names a different source; a source that cannot be hashed at all falls back
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

### `zellij session doctor`

```
zellij session doctor [NAME] [-n|--dry-run] [--fix|--no-fix] [--sign|--no-sign] [--exe PATH]
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

**It never takes the session down.** A pin refresh, a signature and a rewritten plist all take
effect at the next start, so doctor makes the change and *says* a restart is needed. `zellij session
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
Full Disk Access. Both are asked in a pane because neither is about doctor's own process: macOS
judges a terminal-launched process against the terminal's grants, while a pane inherits the server's
domain and is attributed to the server's executable. They are skipped when the session is down,
where the answers would be this terminal's.

Full Disk Access is asked by OPENING the user's `TCC.db` for one byte. `[ -r ]` calls `access(2)`,
which reads the permission bits, while TCC refuses at `open(2)` — so a test on the bits answers
"readable" on a machine holding no grant at all. A database that is not there is reported as
unknown rather than as a refusal, and neither the pane nor the client that opens it can outlast the
probe's five-second deadline: a client talking to a wedged server never returns on its own.

`--dry-run` withholds every fix, and the pane probe is the one thing it still does: it is a
question, not a repair, and the answer cannot be had any other way. Expect a floating pane to
appear and close itself on a `-n` run against a live session. Nothing else on that path writes —
the pin is compared and not copied, the keychain is not touched, and no certificate is minted.

#### Signing the pinned copy (macOS)

macOS keys a grant for a non-bundled program to an absolute path plus a code requirement. An
unsigned or ad-hoc-signed binary has no identity to name, so the requirement is a hash of the
*code* — and the next build voids every grant, silently, until a pane fails in a directory that
worked yesterday. A signature anchored on a certificate ends that, and it runs by default;
`--no-sign` opts out.

Four rungs, best first — and a rung that **refuses** is walked past, not stopped on:

1. **Developer ID Application** — `codesign` already anchors it on the team id. Timestamped.
2. **Apple Development** — the requirement is written by hand, anchored on `subject.OU`. Never on
   the CN: the CN carries an email that changes on reissue and differs between two of one person's
   machines, while the OU is the team id and is the same everywhere that Apple ID is. Timestamped.
   The text is a requirement **set** — `designated => identifier "…" and anchor apple generic and
   certificate leaf[subject.OU] = "…"` — and the `designated =>` is not decoration. `codesign -r`
   parses what it is handed as a set of `tag => expression` pairs, so a text opening with
   `identifier` puts a reserved word where a tag belongs and the whole rung is refused before
   signing starts, with `Requirement syntax error(s): line 1:1: unexpected token: identifier`. It
   reaches `codesign` as one argv, `-r=<text>`: the leading `=` is what makes the value inline
   text rather than a path to a file.

   **The team id is read off the CERTIFICATE, and reading it off the identity's NAME was a bug
   that shipped twice.** `security find-certificate -c "<name>" -p` piped into `openssl x509
   -noout -subject` gives the subject, and the `OU` in it is the team. The parenthesised code in
   the CN is *not*: on a Developer ID Application certificate the two are the same string, which
   is what let the mistake through, and on an Apple Development certificate they are not. A real
   subject, off the machine this was found on:

   ```text
   UID=7472L5G3Y6/CN=Apple Development: someone (DY7JA3K8QZ)/OU=U2VEDWFUF3/O=Someone/C=US
   ```

   Both spellings of that line are parsed, because both are in play: LibreSSL is `/usr/bin/openssl`
   and writes `/OU=VALUE/`, while a Homebrew OpenSSL 3 may be first on `PATH` and writes
   `OU = VALUE,`.

   The lookup asks the **default keychain only** — the one `codesign` itself looks in. An Apple
   identity that `find-identity` sees on the search list but that lives in another keychain yields
   no team id, and degrades to the no-requirement case below rather than to a wrong one.

   A certificate that cannot be read gives **no** team id, and a rung with no team id writes no
   requirement at all and takes the CN-anchored one `codesign` derives. That is the lesser of two
   evils by a wide margin: the derived requirement survives every rebuild, which is what a grant
   actually needs, and only breaks when the certificate is reissued — whereas a requirement built
   from the wrong field is one the signed binary does not satisfy, which voids the grant
   immediately while looking correct. Writing a wrong requirement is worse than writing none.
3. **One we mint**, kept 0700/0600 in `~/Library/Application Support/zellij/signing/`, with a copy
   of `id.p12` in zellij's resolved config directory — and a copy that could not be written is a
   `Needs you`, because the bundle cannot be minted a second time. Minted
   **once**: its own hash is the requirement, so a second certificate voids every grant recorded
   against the first — a keychain that lost it is re-imported from the bundle rather than given a
   new one. The one exception is a bundle that **will not import**: it is moved to
   `id.p12.broken-<epoch>` and a new one is minted — but only behind **two** gates, and both are
   needed. The import error has to name the proven case (`MAC verification failed` / `wrong
   password`), and `security find-certificate -c "<our CN>" <keychain>` has to find nothing. Any
   other failure is reported and mints nothing. That is what keeps "mint once" true rather than
   weakening it: a bundle that never imported was never signed with, so no grant on the machine
   names it, whereas a locked keychain or a run with no dialog to answer fails the import while
   holding the certificate every grant does name.

   `find-identity` cannot be the second gate, and the first cut of this used it. It lists
   identities the keychain calls *valid*, so it folds "never imported" together with "imported but
   untrusted" and with "the keychain will not answer right now" — and it has just been asked, by
   the only caller, and answered nothing. `find-certificate` asks about the certificate itself,
   needing neither a trust decision nor access to the private key, which is exactly the distinction
   the gate needs. Never timestamped: Apple's server needs a real chain and
   would only refuse. The `.p12`
   is written with a SHA-1 MAC and `PBE-SHA1-3DES` for both key and certificate, because that is
   what `security import` reads — OpenSSL 3 defaults to neither and macOS reports the MAC it
   cannot verify as a password it was not given: `SecKeychainItemImport: MAC verification failed
   during PKCS12 import (wrong password?)`. `-legacy` is what lets OpenSSL 3 write those
   algorithms, and it is tried first and dropped on failure rather than decided from a version
   string: macOS ships LibreSSL as `/usr/bin/openssl` and LibreSSL has no such flag, while a
   Homebrew OpenSSL 3 may be first on `PATH` instead.

   **The bundle also needs a non-empty passphrase, and that is a format requirement rather than a
   security one.** The algorithms above are necessary and not sufficient: Apple's importer cannot
   verify the MAC of a PKCS#12 written with an empty password either, and reports it with the same
   misleading `wrong password?`. Proven on a real Mac by changing nothing else — same key, same
   certificate, same algorithms, same LibreSSL — `-passout pass:` fails and `-passout pass:zellij`
   with `security import -P zellij` reports `1 identity imported.` So the passphrase is the
   constant `zellij`, written in the source beside the file it opens. It protects nothing and is
   not meant to: the protection is still the 0700 directory and the 0600 file. A passphrase nobody
   could look up would instead be a way to lose the one certificate the machine may ever have.
4. **Nothing** — the Xcode steps, as a `Needs you`.

A certificate the keychain OFFERS is not a certificate that SIGNS, so the two Apple rungs are
walked and not merely sampled. When one refuses, the other is tried, and the signature that lands
says which rung above it would not sign. Stopping on the first refusal was the old behaviour and it
was the worse of two outcomes: `session up` refreshes the pin ad-hoc-signed and doctor is what
makes it anchored, so a doctor that gave up left the machine in exactly the state this rung exists
to remove, with a working certificate standing one step below. Only a failure the certificate
cannot explain — a copy or a rename that the filesystem refused — stops the walk, because another
rung would write the same error a second time.

**The walk stops at the Apple rungs, and that boundary is the point.** Falling from a Developer ID
to an Apple Development certificate of the same team keeps the requirement: `codesign` derives the
same `identifier … and anchor apple generic and certificate leaf[subject.OU] = "TEAM"` for the
first that we write by hand for the second — which holds only while that team id comes off the
certificate, and not at all for a rung that fell back to the derived CN-anchored requirement. That
fall is allowed rather than blocked, because an anchored requirement that survives every rebuild
still beats leaving the pin ad-hoc; what it must not be is silent, so the follow-up names the team
id it could not read as the reason the re-grant is needed. The certificate we mint does **not** —
its requirement
is its own hash — so walking into it would void every grant on the machine. And a refusal is not
always the certificate's fault: `errSecInternalComponent`, a keychain locked over SSH, a "Deny" on
the key-access dialog. Each is transient, and each would otherwise demote the pin permanently —
permanently, because a self-signed signature *is* anchored, so the next doctor run reads the pin as
already correct and never climbs back. So rung 3 is reached only when the keychain offers no Apple
certificate at all. A machine that holds one and cannot use it gets a `Needs you` naming what each
certificate said, and nothing is minted that the machine would never otherwise have had.

Nothing is ever signed ad-hoc: that anchors on the code hash, which is the fault under a new name.
No trusted root is ever added — requirement evaluation does not consult trust unless the requirement
says `trusted`, and ours never does. What signing needs is keychain ACL access, and
`ZELLIJ_KEYCHAIN_PASSWORD` is how a run gets it without a person present. It is read from the
environment when it is set, and doctor never asks for it.

**That variable is not an SSH-only escape hatch, and calling it one was wrong twice over.**
`security(1)` writes its password prompt to the **controlling terminal** — not through the window
server, and not to stdin — so being inside a graphical session buys nothing. At 0.45.0-nkmk.8
`security set-key-partition-list` with no `-k` blocked forever in a pane on a real Mac: no
SecurityAgent process, no dialog, no timeout, an empty report, and one line on the pane's terminal:

```text
(deprecated) password to unlock /Users/…/login.keychain-db:
```

So every child doctor runs is now started with `setsid(2)`, in a session of its own with no
controlling terminal. A tool that would have prompted fails fast and says why instead. A null stdin
had always been set and does not help — the prompt never goes there.

`set-key-partition-list` runs before **every** signature made with our own certificate, and not
only on the run that minted it. The ACL it grants belongs to the keychain, not to the certificate,
and a certificate is minted once and signed with for years — so granting it at minting time meant
the very next run found a ready rung, signed with it, and was refused by a key nothing had ever
approved. Proven on a real Mac: running the partition list by hand made the identical `codesign`
succeed. It is cheap and idempotent when the ACL is already there. It is never run for an Apple
certificate, which comes with its own.

`set-key-partition-list` is also no longer allowed to end the run. It decides whether macOS asks
for the key once per signature or never; `codesign` raises that dialog itself, and a person at the
desktop can answer it with **Always Allow**, once. So a refusal is reported and signing continues,
and a `codesign` that then refuses too names both remedies: run doctor from a terminal in the
desktop session and click Always Allow, or set `ZELLIJ_KEYCHAIN_PASSWORD`.

**The password unlocks the keychain on the Apple rungs too, and until now it did not.** It reached
`security` only as `-k` on `set-key-partition-list`, and that command runs only for the certificate
we mint — so a machine signing with an Apple Development or Developer ID certificate ran no
`security` command at all before `codesign`, and `ZELLIJ_KEYCHAIN_PASSWORD` did nothing on it. A
locked keychain — every launchd run, and every machine reached over SSH that nobody has typed into —
then refused the key with `errSecInternalComponent`, while the remedy printed beside the refusal
told the reader to set the one variable the run had ignored. The workaround was a
`security unlock-keychain -p` typed by hand before each doctor run. Doctor now takes that step
itself: on the first rung that is not ours, and only when the variable is set, it runs
`security unlock-keychain -p <password> <keychain>` against the same default keychain it signs from.
Once per run and not once per rung — a password the keychain rejected is rejected again one rung
down, and saying so twice is noise. A refusal there is survivable in the same way the partition
list's is: it is a `Needs you`, and the run goes on to sign. Our own rung is untouched, because `-k`
already unlocks the keychain on its way past. The password stays out of every finding; what is
quoted is `security`'s own stderr, which names the keychain and not the password. The remedy now
says what setting the variable buys — it unlocks the keychain, whichever certificate the run signs
with — because on the rungs that most needed it, that sentence used to be a lie.

That rule has a consequence in the *discovery* step, and it is not obvious. `security find-identity
-v -p codesigning` lists valid identities, and validity there is a **trust** decision — so a
certificate we minted, which chains to nothing, is reported as `(CSSMERR_TP_NOT_TRUSTED)` and the
listing ends `0 valid identities found` on a machine that holds it, holds its key, and signs with it
without complaint. Seen on a real Mac. The answer is a second listing without `-v`, filtered to our
own common name, and **not** `add-trusted-cert`: adding a trusted root would change what Gatekeeper
accepts across the whole machine, need an administrator, and buy a grant nothing it does not already
have. The filter matters too — an Apple certificate the keychain calls invalid is invalid for a
reason, and taking it off that listing would put the ladder on a rung that cannot sign.

The identifier is the constant `org.zellij.nkmk`. **Changing it voids every grant on every machine**,
because it is part of the requirement macOS recorded — which is why it is a constant and not a
setting.

On the two timestamped rungs the attempt can still fail — an offline machine has no timestamp
server — and the signature is then made without one and the report says so, quoting the refusal. A
signature that silently carries no timestamp looks exactly like one that was never asked for.

The round trip is a copy, a sign, two verifications and a rename. `codesign` writes in place and a
running server holds the pin open, so an in-place sign fails `ETXTBSY` exactly when a session is up.
The two verifications are two questions because they fail apart: a signature can verify while its
requirement still names the code hash, which is a run that reported success and fixed nothing — and
a requirement can read perfectly while the binary does not satisfy it, which is worse, because the
first question is the one a text search answers and it says yes.

**The verification runs `codesign --verify --strict --verbose=2`, and the verbosity is
load-bearing.** Plain `codesign -v <path>` returned 0 on a pin that `codesign -v --verbose=2 <path>`
rejected with `does not satisfy its designated Requirement` and exit 3: the designated-requirement
check is what the second verbosity level adds. The message is matched as well as the exit status,
because the exit status is the half already observed reporting success wrongly.

**The same verification is run on a pin doctor did not sign**, before it is called healthy. Reading
a requirement is not checking it: an anchored-looking pin has an identifier, an anchored text and no
code hash anywhere whether or not the binary satisfies it, and a pin that fails verification is
signed again rather than reported as `Already correct`.
Anything that goes wrong leaves the working pin untouched, and a run that reached the bottom of the
ladder reports a `Needs you` naming every rung that refused and what each one said — a machine that
cannot sign is still a machine worth reporting on. The Xcode steps come with it only when the
keychain held no Apple certificate; a machine whose certificate merely refused is pointed at the
key and the keychain instead, which is what refuses like that.

**The refresh and the signature are one transaction, and they have to be.** Doctor used to refresh
the pin first and sign it second. A run where every rung refused — a locked keychain, which is
*every* unattended launchd run — had therefore already replaced a properly anchored pin with a fresh
ad-hoc copy of the new build, and then reported `the pinned copy is untouched`. Both halves were
wrong: the pin had been replaced, and every grant on the machine was void from the next restart,
which is a symptom that surfaces later and somewhere else. Measured on a real Mac at 0.45.0-nkmk.9,
where the requirement a dry run had confirmed sixty seconds earlier was simply gone.

So when the pin is **anchored** and out of date, the copy is handed to the signing step: it writes
the new build into its own temp, signs THAT, verifies it, and renames only on success. A refusal
leaves the previous signed pin exactly where it was — previous build, signature intact, grants
intact — and says so. The deferral is decided by one function that both steps ask, because a
disagreement in the "skip" direction would drop the refresh with nothing reporting it. A pin that is
already **ad-hoc** is refreshed first as before: it holds no grant a rebuild could keep, so pinning
the new build is worth more than protecting a signature that was never load-bearing.

**One writer, and it never replaces an anchored signature without signing.** The pin used to be
written by the plain `install_pinned_exe` copy → rename with no signing step, so an upgrade whose
first command was `session up` — rather than `session doctor` — put an ad-hoc pin in place exactly
as the old doctor did, and the grants went with it. That is the one path an upgraded machine
actually takes: the watchdog runs `session up` every minute, so it wins the race with any shell.

The rule was first written into `assert_pinned_exe`, the caller `session up` goes through. **There
is a second caller**, and it is the one that runs on every interactive launch:
`server_exe_for_interactive_launch` (`zellij-utils/src/session_service.rs`) resolves the server
binary for `start_client`, and it called the copy directly. On a real machine the refusal from the
first caller printed, and the second caller then overwrote the Apple Development signature with an
ad-hoc copy of the new build in the same command.

So the rule lives at the write. `install_pinned_exe` asks `pin_is_anchored` — the same predicate
`refresh_belongs_to_signing` asks, so the two cannot disagree — the moment it knows it is about to
overwrite an existing pin, and there is no parameter with which a caller can ask for anything else:

| What the pin is | What ANY caller gets |
|---|---|
| current, or ad-hoc, or unsigned | the plain copy, unchanged |
| anchored and stale, signing works | refreshed and signed as one transaction (`PinOutcome::Signed`) |
| anchored and stale, signing refuses | **nothing is written**; the existing pin's path is returned (`PinOutcome::Kept`) |

The third row is the point. A locked keychain is every unattended launchd run, and there the session
comes up on the PREVIOUS build with its grants intact, and says so in the signing step's own words
— quoted rather than summarised, so the line matches what `zellij session doctor` says next. An
older build that can still read the files beats a newer one whose grants can only be given back
through a GUI dialog. The socket is scoped by contract version rather than by version string, so a
client one build ahead still speaks to that server.

The refusal is said ONCE per pin per process, by the writer. `session up` asserts the pin and then
launches a client that resolves the server binary through the pin again, so the same refusal is
reached twice in one command; the second call answers from the first one's decision and never asks
the keychain again.

What the writer cannot work out for itself is stated once per run instead of passed as a parameter,
which would put the decision back in the callers' hands: `PinSigningPolicy` carries whether this run
may sign at all (`false` only for `session doctor --no-sign`) and which config directory holds the
certificate backup. A run that states no policy — a plain `zellij` launch — signs, which is exactly
the caller that must. `--no-sign` now leaves an anchored pin BOTH unsigned and unreplaced; it used
to fall through to the copy and destroy the signature.

The cost is that a machine stuck in the third row runs the signing ladder on every watchdog tick and
warns every minute. That is deliberate: the state ends the moment somebody runs
`zellij session doctor --fix` at the machine, and a warning nobody sees is how the old fault lasted
two releases.

On Linux and anywhere without a signing context, nothing changes: `codesign` cannot answer, no pin
reads as anchored, and the copy runs as it always did.

**The rename is still not coordinated with `session up`, but it no longer loses quietly.** Signing
is copy → sign → verify → `rename`, and `install_pinned_exe` does its own copy → rename. A
`session up` that lands a newer pin while doctor is mid-run used to be undone by doctor's signed
copy of the older one, and a rename over a file says nothing about what was there, so the machine
went back a build in silence.

The pin's length and mtime are now read before the copy and compared immediately before the rename.
A pin that was replaced underneath — or removed, or created where there was none — makes the run
discard its signed temp and refuse, naming `zellij session doctor --fix` as the way to finish.
Deliberately a comparison and not a lock: ordering two renames into one directory would need a lock
that both commands take, and that is new machinery with new ways to wedge, for a race that needs the
two commands typed seconds apart. What is bought is that the silent half is gone — the run that
would have clobbered says so, and the recovery is the same one command it always was.

**Known limitation: the same-team assumption is not enforced.** Falling from a Developer ID to an
Apple Development certificate keeps the requirement only while both carry the same team, and the
walk compares neither the team id nor the requirement it would derive. A keychain holding
certificates from two different teams would fall to a different `certificate leaf[subject.OU]`,
which changes the requirement and drops every grant exactly as a demotion would. One machine
holding both, from two teams, is rare enough that this is recorded rather than guarded.

**A re-grant is asked for only when the requirement actually changed**, and the question is put to
the two requirement TEXTS — the one read off the pin before signing and the one read off the signed
copy after. It used to be inferred from the state of the machine: a bundle sitting in the signing
directory meant "this machine has signed with a certificate of its own". On a Mac that had never
used that rung, but carried leftovers from an older shell script, that sent the user into System
Settings to redo three permissions against a requirement that was character-for-character the one
already there. A pin re-signed with the same certificate carries the same requirement — that is the
entire premise of this feature — so the ordinary case, a rebuild on a machine already set up, needs
only the restart.

**`codesign --verify` exiting 0 says nothing about whether a grant survived.** It answers "is this
signature intact", not "is this the requirement macOS recorded" — an ad-hoc pin verifies perfectly
and holds nothing past its next rebuild. The requirement is what doctor prints and compares, and
verification is a second question asked alongside it, never instead of it.

**A refusal quotes `codesign`'s error and not its first line.** `-f` makes it announce
`<path>: replacing existing signature` before anything else on every run after the first, so a
report that quoted line 1 told the user their run had failed with the one thing that had gone
right — hiding, on a real Mac, a key ACL that had never been granted. The informational line is
dropped and the rest is kept, joined and capped.

After a signing, the follow-up is given in the order that makes it one pass: re-grant Full Disk
Access, Accessibility and Screen Recording for the pin's exact path **first**, then `zellij session
restart`, so the new server comes up already holding them.

**What 0.45.0-nkmk.7 actually did on two real Macs, since the ledger should say.** It fixed the two
faults it set out to fix and shipped three more, and every one of them was a rung no test machine
had reached before. On the machine with an Apple Development certificate the rung signed — and the
signature failed its own designated requirement, because the team id came off the identity's name;
doctor did not verify what it had written, so the next run read the requirement's text, called the
pin healthy and exited 0. It reported success, told the user to re-grant three permissions against a
requirement the binary fails, and would never have signed again. On the machine with none, the
minted rung still could not import: the algorithms were right and the empty passphrase was not, and
because a bundle on disk was re-imported rather than re-minted, the code that fixed the algorithms
never ran there at all.

The lesson is narrower than "test more". **A signing flow is not proven by the rung the developer's
machine happens to hold**, and each of the three had been sitting behind one: the requirement bug
behind the Apple Development rung, the passphrase behind the minted one, the reuse behind a machine
that had already failed once. The verification step is the general guard, because it does not care
which rung produced the signature — and it is also the reason the first two were invisible: doctor
asked whether the requirement *read* correctly, which is a question a broken signature answers
yes to.

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
stable path, or the running one where there is no stable path, and the help line says so. Upstream
builds that line by appending to a string and colouring each key by substring, so the hint is one
more `push_str` and the colour cannot land on the wrong characters. Built-in plugins are granted
every permission, so `WriteToClipboard` costs no prompt.

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
command; a refused config costs the session.

A `{}` keeps one residual risk, which is why the shipped `claude` hint carries none. The variable is
read from the pane's whole process subtree, so the value can come from a child — a subagent, a hook —
rather than from the tool the pane is running. As a detector that does not matter; substituted into
the command, it records whatever answered first.

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
a permission one — is never reported as a denial. The badge is a flag, not an instruction: the
[about page](#the-about-page-names-the-binary-macos-must-trust) and `session doctor` name the exact
path, because the grant is keyed to that file and auto-registration was not observed to happen.
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
* `server_exe_replaced` — present only when it is true, and only Linux can say so: an upgrade wrote
  over the running file in place.

All four are absent for a dead session, and for a live one whose server the platform will not answer
about or which has two servers for its name. Omitted rather than null, like the metadata fields
beside them, so a consumer that sees `server_exe` can trust it.

**Nothing here crosses a contract**, which is the whole design. The answer is read from the OS on
the side that runs `ls`, by the same `server_executable` the stale-build warning uses — no
`SessionInfo` field, no protobuf tag, no client/server message. The decision not to put a version on
the plugin API stands; this never asks the session anything.

**There is no version string, and that is deliberate.** A version can only be had by running the
binary, and `ls` is a listing. Spawning every server's executable to ask it turns a read of the
socket directory into an execution of whatever those paths point at, and it hangs for as long as one
of them sits on a mount that has gone away. The build id is cheaper — a few kilobytes off the front
of the file, never the 40 MB — and it is the better answer to the question being asked, since two
builds of one version have different build ids and one version string could not tell them apart.

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
  `report_pane_env` does, on the same tick, through the same code. **A session with no agent pane in
  it does not read the process table at all.**
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
kept in a server-internal field and never folded into `PaneInfo.pane_env`, because that map is
published over the plugin API and the `report_pane_env` allowlist that governs it never asked for
them.

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

### `zellij mcp`: the CLI served over the Model Context Protocol

```json
{ "mcpServers": { "zellij": { "command": "zellij", "args": ["mcp"] } } }
```

`setup --dump-surface` already describes the whole command tree in one call, which is most of what
an agent needs. What it cannot do is help a harness that **cannot shell out**, and it cannot be
gated per verb - an MCP client allows or denies each tool by name, which on a machine where "read a
pane" and "close a tab" want different answers is the whole point. Hence a server, in the binary, so
it is version-synced and there is nothing to install.

**Seven tools, not eighty-seven.** `zellij_overview` (panes, agents, or the sessions on this
machine), `zellij_read_pane`, `zellij_wait_for`, `zellij_write_input`, `zellij_create`,
`zellij_arrange`, `zellij_snapshot`. Session lifecycle - `up`, `down`, `restart`, `enable` - is
deliberately not among them: those start and stop the thing the server is talking to. A test fails
if the surface grows past eight tools, or a tool past eight parameters.

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
the CLI's own refusal, and there is no id anywhere for it to invent. What cannot be undone
(`close_pane`, `close_tab`) passes the confirmation the CLI would otherwise refuse to run without.

Which session a call is about: the tool's own `session`, else `ZELLIJ_SESSION_NAME` in the server's
environment.

**An abandoned call costs nothing.** MCP clients time out, disconnect and restart, and they do it
without telling the server, which simply sees the future dropped. Every child is spawned with
`kill_on_drop`, so a dropped call takes its child with it - at cancellation and at shutdown alike,
because the runtime drops its pending tasks when stdin reaches EOF. `zellij_wait_for` is bounded
too: without `timeout_s` it gives up after 300 seconds rather than blocking for the life of the
pane, which is the default its own schema has always advertised.

**Every tool declares the shape of what it returns**, and it is the same shape for all seven: the
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
crates, no HTTP stack, and nothing on any wasm plugin crate.

Two rows were added to the surface map while wiring this up: `snapshot list` and `snapshot show`
print a table and a payload respectively, and the map had been claiming they printed nothing.

### `session up` comes back with the shape the session had

```
zellij session up      [NAME] [--fresh | --restore [ID]]
session_up_resume true   # config.kdl, top level, default true
```

A bare `session up` used to mean two different things depending on a file nobody looks at. `up`
ends in `attach --create`, and `attach` resurrects a dead name from the in-place cache
(`session_info/<name>/session-layout.kdl`) by itself — so a crash, a SIGKILL and a reboot already
came back with the previous panes. `session down` and `delete-session` **remove** that file after
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
The enum is what keeps `restart --fresh` on the layout.

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

### `pane_privacy`: panes the session keeps to itself

Some work is nobody's business but the person at the terminal's. A session can be told which panes
those are, and the panes then stop existing as far as the command surface is concerned: their rows
are gone from `list-panes`, `list-tabs`, `list-tree` and `list-agents`, and any command that names
one answers exactly as it answers for a pane that was never there.

```kdl
pane_privacy {
    patterns_file "/path/to/private-paths.txt"
    pattern "some-extra-regex"
    match_fields "cwd" "command"
    on_unknown_cwd "withhold"
    tab_rule "any"
}
```

Every entry is optional. `patterns_file` names a file of one extended regular expression per line,
`#` comments and blank lines ignored. `pattern` is the same thing written inline, and repeats. The
environment variable `ZELLIJ_PANE_PRIVACY_FILE` overrides `patterns_file`, because which
directories are private is a fact about a machine and one `config.kdl` is shared across several.

Patterns are matched case-insensitively and are **not anchored**: a bare fragment matches anywhere
in the value, which is what makes an existing list of private paths usable as-is. With no file, no
pattern and no environment variable there is no policy, and the whole feature costs one
`is_active()` per call.

`match_fields` chooses which columns of a pane row a pattern is tried against — `cwd`, `command`,
`title`, `tab_name`. The default is `cwd command`, where a private path lands on its own; a title
and a tab name are the user's own text and are opt-in. `on_unknown_cwd` decides a terminal pane
whose cwd is not known yet — `pane_cwd` is empty for about a second after a pane is created, and
`withhold` (the default) closes that window. A plugin pane has no cwd by construction and is never
withheld for lacking one. `tab_rule` decides how a tab inherits its panes' verdict: `any` (the
default) withholds the whole tab when one pane matches, because a tab is the unit of work and its
name alone can say what the work is.

**Withholding is not redaction.** A withheld row is dropped, and the only thing left behind is a
count. A redacted row still says where the private work is.

Being top-level, the block is ignored by a binary that predates it, so it can go into a shared
config ahead of the upgrade. Its own children are strict, so it cannot be rolled out one key at a
time.

#### What it does to each command

| Command | With a policy in force |
|---|---|
| `list-panes`, `list-tree` | withheld rows dropped; the table gains a `withheld: n` footer |
| `list-panes --json` | still a bare array — `--report-withheld` switches it to `{"panes": [...], "withheld": n}` |
| `list-agents` | the same, over the same walk; `--report-withheld` gives `{"agents": [...], "withheld": n}` |
| `list-tabs` | withheld tabs dropped |
| `dump-screen`, `wait`, `send-keys`, `write-chars`, `paste`, `close-pane`, `move-pane`, `stack-panes`, `break-pane`, `rename-pane`, and every other `--pane-id` verb | `No pane answers to '<target>'`, exit 2 — the unknown-pane answer |
| a verb naming a withheld tab | that verb's own no-such-tab answer |
| `new-pane --cwd`, `new-tab --cwd`, `run --cwd` under a matching directory | the directory is dropped and the command succeeds, as it does for a directory that does not exist |
| `dump-layout`, `snapshot show`, `snapshot restore` | refused whole while any policy is active |
| `snapshot list`, `ls` | unchanged |

`new-pane --cwd DIR` with **no command** never reached the server with a directory in the first
place: upstream drops the directory when there is no command to run in it, so the pane opens in the
calling pane's cwd. The forms that do carry one — `new-pane --cwd DIR -- CMD`, `new-tab --cwd DIR`,
`run --cwd DIR -- CMD` — are the ones the policy strips.

**A refusal is the ordinary miss, byte for byte.** That is the guarantee, and it is stronger than
"the message says nothing": a message that said "withheld" would be a yes/no oracle on whatever
string the caller chose to pass, and because patterns are unanchored substrings, a loop over that
oracle recovers the pattern list itself — the one thing the filter exists to hide. So:

- a withheld pane id, handle or uuid gets `No pane answers to '<target>'` and exit 2, the same
  sentence and the same exit code an id nothing holds gets;
- a withheld tab gets whichever no-such-tab answer that verb already gives — `No tab with id 3`,
  `No tab at position 3`, `Tab with id 3 not found`, or, for `rename-tab` by position, the silent
  no-op that verb answers a miss with;
- a `--cwd` the policy withholds is **dropped from the request** rather than refused. zellij
  already ignores a directory it cannot use: a `--cwd` that does not exist makes the pane anyway,
  in the server's own cwd, and `list-panes` reports it there. A withheld directory is treated as
  that kind of directory, so the answer does not depend on whether the path matched.

The one thing that still admits a policy is running is the aggregate `withheld: n` count on
`--report-withheld` and in the MCP's `zellij_overview`. That is deliberate: a caller is entitled to
know its view is partial. It is a count and never a name, so it says how much is missing and
nothing about what. `dump-layout`, `snapshot show` and `snapshot restore` keep a plain refusal —
they are whole documents with no per-target answer to imitate, so there is no oracle to build out
of them, and their message names no pattern and no path.

Three things the filter does not make indistinguishable, and all three are tab-shaped — the space of
guesses there is the small integers, `list-tabs` has already said which tabs are missing, and none
of them leaks a pattern:

- `move-tab --tab-id N --to-index M` reports its ordinary miss with exit 1; the refusal is exit 2.
- `go-to-tab-id N` **with no client attached** does nothing at all for a tab that is not there, so
  the refusal speaks where the miss is silent. Attached, the two agree.
- `go-to-tab-name` is not covered: a caller that guesses the *name* of a withheld tab reaches it.

Each pane verb, by contrast, is matched exactly, and there are two sentences to match:
`dump-screen`, `close-pane`, `focus-pane-id`, `toggle-pane-borderless` and `edit-scrollback` answer
`No pane answers to 'terminal_9'`; everything else answers `Pane with id Terminal(9) not found`;
`set-pane-borderless` says nothing at all. `pane_miss_refusal` in `route.rs` holds that mapping, and
an E2E over 25 verbs is what found it — the two sentences are not interchangeable.

`wait` was the one verb with no miss to imitate. An id form used to resolve to itself without
anyone asking whether a pane held it, so `wait terminal_99 --for exit` on a pane that never existed
returned `waited_ms: 0` and exit 0. `Screen::resolve_pane_target` now resolves an id against the
pane list, like a handle, so an unknown id and a withheld one both answer
`No pane answers to '<target>'` at exit 2.

`list-panes --json` keeps the bare array because three parsers already read it: the `wait` poll,
`list-agents`, and the MCP server. A caller that needs to tell a partial answer from a complete one
asks for the envelope by name.

#### Where the decision is made, and why there is only one

**The server decides, in `route.rs`.** That is where every CLI verb arrives, so it is also where
the `zellij mcp` server arrives — every MCP tool runs this binary's CLI in a child process. The
filter therefore covers an agent holding the MCP tools and an agent holding a shell, without being
written twice. There is no flag to turn it off for one call.

Two points carry it:

- **`Action::ResolvePaneTarget`.** Every `--pane-id` verb resolves its target through the server
  before it acts. One refusal there covers all of them, `wait` included — which matters more than
  it looks. `wait --for exit` polls `list-panes` and reads a pane that is not in the list as gone,
  so a silently filtered pane would have been reported as `exit_status: -`: a wrong answer, not a
  refusal. `wait` now asks the server to resolve even an id form, which it used to short-circuit,
  and a withheld pane gets the same `No pane answers to '<target>'` an unknown one gets.
- **A guard at the top of `route_action`**, over `privacy_target_of`. It catches anything that
  reached the server with a pane or tab already resolved, and answers with that action's own miss.
  A withheld `--cwd` is handled just before it, by `strip_withheld_cwds`, which edits the request
  rather than refusing it.

`snapshot show` and `snapshot restore` are the one decision outside the server, and it is a
different decision: a snapshot is a file on disk with no session behind it to ask, so the only
question is whether a policy exists at all.

#### The wasm split

`zellij-utils/src/pane_privacy.rs` holds the settings and nothing else — no regex, no file read, no
environment. It is parsed by `kdl/mod.rs`, which builds for `wasm32-wasip1` along with every default
plugin. The matcher lives in `zellij-server/src/pane_privacy.rs`, which never builds for wasm and
already had `regex`. This is the same shape as `session_service`, for the same reason.

One workspace dependency changed: `regex` gained the `unicode-case` feature. Without it the crate
rejects `(?i)` outright, and the failure is a runtime parse error rather than a build error.

#### Failing closed

A `patterns_file` that cannot be read, or a pattern that does not compile, makes the policy
**broken**: every pane is withheld and every targeted call answers as a miss, with the reason in the
session log. A privacy filter that fails open is a filter that silently is not there.

#### What it does not cover

**The plugin API.** A plugin subscribed to `Event::PaneUpdate` receives every pane, withheld ones
included. The filter sits in the CLI's route handlers, and the plugin event path does not pass
through them. That is the right boundary for what this is for - a plugin is code the user installed,
and the status bar has to know the panes exist to draw them - but it is not a sandbox, and a plugin
is not a place to put something the policy is meant to hide from.

**An attached client.** Anyone looking at the terminal sees the panes. The filter is about what the
command surface answers, not about what is on screen.

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
  the one that starts fresh -- all three start a server on that socket.
- **`zellij --session <name>`** (`assert_session_ne`), the plain create.
- **`start_server`**, the last thing that can still refuse. The client's check can be raced or
  bypassed; the bind cannot.

A stale socket a dead server left behind still gets cleaned up and replaced, because `connect()` to
it fails. Only a socket with something alive behind it is protected.

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
follow the constant rather than pinning the old bytes. Proved end to end as well: a pty client
attached to a scratch session captured 174 OSC 8 sequences, all BEL-terminated and none
ST-terminated, with the only remaining `ESC \` in the stream belonging to the OSC 10/11 colour
queries and a kitty graphics probe.

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

### Doctor sets aside a signing bundle that is not its own

`zellij session doctor --sign` re-imports `signing/id.p12` rather than minting a second
certificate, because the certificate's hash **is** the requirement every grant on the machine
records. That rule is right and stays. What was missing is that a clean import proves the file was
readable and proves nothing about whose certificate came out of it.

A `signing/` directory written by an older `zellij-mac-setup` holds a certificate under its own
common name. It imports without complaint, so nothing mints; the ladder still finds no
`zellij self-signed code signing` identity; and doctor falls through to the Xcode steps with "no
signing certificate for &lt;pin&gt;" — on every run, for ever, on a machine it could repair in one
pass. `refresh_pin_through_signing` fails with it, so the pin stays on the superseded build after
an upgrade and the grants keep pointing at a binary nobody runs.

So `ensure_self_signed` now asks the keychain again after a **successful** import. If our
certificate is still not on offer the bundle is set aside as `id.p12.foreign-<timestamp>` — kept,
never deleted, exactly as an unreadable one is — and a new certificate is minted. The set-aside name
says which fault it was: `.broken-` is a bundle Apple could not read, `.foreign-` is one that read
perfectly and was not ours.

Minting means the grants for the pinned copy have to be made once more, which the finding says. It
is the only way out: doctor cannot sign with a certificate whose private key it does not control the
naming of, and the alternative is the deadlock above.

The report also names the impostor. When the ladder is empty and the keychain holds a
zellij-titled certificate that is not ours, doctor prints its name and hash beside the "no signing
certificate" line, so the reader is told they have the wrong certificate rather than none — the
version of this that sent a real machine off to Xcode over a certificate that was never going to
work.

### `connect_to_server` gives up on a session that answers to no server

The CLI client's socket connect retried every 50ms forever. That is the right answer while a
server it just asked to spawn is still binding its socket, and the wrong one for a session that
answers to no server at all — a crashed process, a socket file left behind, a name that was never
going to come up. Every verb against that session hung instead of failing: `action`, `subscribe`,
`attach`, all of them, because they all go through the same trait method.

`connect_to_server` now gives up after five seconds and returns `Err("session '<name>' is not
running (or its server is unreachable): <io error>")`, naming the session from the socket path's
own file name. Each of the CLI entry points reports that message on stderr and exits non-zero —
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

`PluginCommand::SetFloatingPanePinned` reaches the same `Screen::set_floating_pane_pinned` the
CLI's `set-pane-pinned` action does, but its `ScreenInstruction` handler stopped at the mutation -
no render, no `log_and_report_session_state`. `set-pane-pinned` and its two CLI neighbours,
`set-pane-floating` and `set-sync-tab`, all end the same handler with `screen.render(None)?;
screen.log_and_report_session_state()?;`; this one was missing both. `PaneInfo.is_pinned` stayed
wrong for every subscriber - `list-panes --json`, `subscribe`, a plugin's own pane manifest - until
some unrelated change touched the session and dragged a report along with it. A test already in the
tree, `pane_info_reports_a_pinned_floating_pane`, worked around exactly this: it sent a `RenamePane`
right after the pin, whose only job was to be a change that *does* report, so the pin's effect would
show up in the pane manifest the test then inspected. The workaround is gone now that pinning
reports on its own.

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
and `subscribe` already do.

### `snapshot import --prune-source` prunes only what it archived

Archiving answered three different questions with the same word. `archive_session_info_folder`
returned `Ok(None)` for "the archive already holds this exact shape", for "this folder has no
layout", and for "archiving is switched off" — and one of those three is the only one where the
source folder is safe to delete.

`snapshot import --prune-source` read all three as the first. With `session_snapshot_limit 0` it
printed `skipped <name> - already in the archive` for every legacy `session_info` folder it found,
which was false, and then removed each one. Nothing had been archived, and nothing was left. The
same conflation made `save-session --archive` report `Nothing new to archive.` on a machine where
archiving was simply off.

The return type now says which case it is. `ArchiveOutcome` has an `Archived` variant carrying the
snapshot, `AlreadyArchived`, `Disabled` and `NothingToArchive`, and `source_is_archived()` answers
the one question a pruning caller actually has — is this folder in the archive now, whether or not
this call is what put it there. Only `Archived` and `AlreadyArchived` say yes, so the prune is
guarded by the fact rather than by the absence of a snapshot.

`snapshot import` also refuses up front when archiving is off, before it walks anything, rather
than reporting per folder on work it was never going to do. It exits 2 and names the setting. The
dry run refuses with it, which is the point: `--dry-run` used to promise imports that a real run
would not have made either. `save-session --archive` grew the same distinction and exits 2 instead
of claiming there was nothing new.

The four fire-and-forget callers in the server and in `delete-session` are unchanged; they only
ever looked at the `Err`.

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

### A certificate that was minted is reported and backed up, whatever the import did

`ensure_self_signed` mints once in the life of a machine, and the certificate it makes cannot be
made again: its hash **is** the requirement every grant on the machine records. The import that
follows the mint was a `?`.

So on a machine with no Apple certificate, with `doctor --fix` run over SSH, `openssl` minted
perfectly and `security import` failed on a locked keychain — and the `?` threw away the whole
findings list, the "minted a certificate of our own" line with it. The report said "no signing
certificate" and pointed at Xcode. `back_up_identity` runs on the caller's `Ok` arm, so there was no
second copy either. The machine's one and only certificate existed at a single path, unmentioned,
one `rm -rf` from being lost for good.

`ensure_self_signed` now fails with a `SelfSignedFailure` instead of a bare `String`: the findings
it had already made, a `minted` flag, and the reason. The caller reports the findings first, backs
the identity up when something was minted, and only then says what went wrong.

The flag names the **file**, not the command. It is read from `bundle.exists()` before the `?`, so a
mint that died before writing anything does not send the caller off to copy a file that is not
there — which would print a `Needs you` about a certificate that does not exist.

Testing this needed one new thing: `RecordedCommander::creating`, which lets a scripted command
create a file the way the real one does. Without it a test could drive a mint as far as its first
failure and never past it, because the recorded `openssl` writes no key for the lockdown step to
find. Opt-in, so every existing commander still touches nothing.

### The process scan asks `ps` not to truncate

`running_servers` ran `ps -eo pid=,command=`. BSD `ps` — which is macOS's — sizes its output from
`COLUMNS` or a `TIOCGWINSZ` and cuts every line to it; only `-ww` makes the width unlimited.

A server's argv is `/opt/homebrew/bin/zellij --server /var/folders/xx/…/T/zellij-501/
zellij-<contract>/<name>`, well past eighty columns, and the socket path is the **last** field. A
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

### The key ACL grant names our own key

`security set-key-partition-list -S apple-tool:,apple:,codesign: -s <keychain>` was passed no match
term. `-s` selects every private key in the named keychain that can sign, and the keychain named is
the user's **login** keychain — so this rewrote the partition list of every signing key on the
machine, not ours.

The `Rung::SelfSigned` guard at the call site scopes **when** the command runs, never what it
touches, and the module's own comment two lines above it says that re-writing a key we did not
create is not doctor's business. A Mac holding an Apple Development key whose certificate the ladder
refused falls to the self-signed rung, so this is not a hypothetical arrangement: doctor replaced
the partition list on the Xcode-managed key, and Xcode began raising key-access dialogs on builds
that used to be silent.

`-l <the certificate's common name>` now scopes it, matching the friendly name `openssl pkcs12
-name` writes into the bundle, with `-s` kept beside it so the match is narrower than either alone.

**The flag is the part still worth confirming on a Mac.** If the label does not match, the command
grants nothing rather than the wrong thing, and doctor's existing handling covers that exactly:
this step is a convenience, a refusal is a `Needs you` saying "codesign may ask for the key", and
the run goes on to sign. Losing the convenience is a worse outcome than before for us and a better
one for everybody else's keys, which is why the change lands without waiting for the measurement.

### `managed_session`: the init system owns one session, and every create defers to it

`session up` was already the only path that consulted the launch agent. Everything else that makes
a session — typing `zellij`, `zellij -s <name>`, `zellij attach --create` — built a server here,
from whatever binary was on `PATH`, in whatever session domain the shell happened to be in. The
guards for all of that existed and were one caller short: `ensure_gui_session_domain` had exactly
one, and it was `session up`.

One key turns that around:

```kdl
session_name "the-one-session"
session_service {
    managed_session true
}
```

Unset, nothing changes for anybody: no unit is written that nobody asked for, and every command
creates a session exactly as it did before. Set, the unit for **`session_name`** becomes
unconditional, and three things follow.

**The name is `session_name`, matched whole.** No new key was needed — the name was already there
and `session enable|status|up` already default to it. It is compared as a whole string, never as a
prefix or a pattern, so `zellij -s scratch` on a machine whose `the-one-session` is managed is an
ordinary ad-hoc session and stays one. Two DIFFERENT gates come out of that, and the split is the
design: **installing** a unit needs the name to be the configured one, while **handing a create
over** needs a loaded unit for that exact name. A machine can only ever grow units for the name it
named.

**`session enable` stops being a step somebody has to remember.** A thing the init system owns is
not a thing to opt into once per machine and then keep in step by hand, so the command that is
about to need the unit writes it: no unit, or a unit that is not what this config would generate,
and it is installed or rewritten right there. `session enable` keeps working and now means "do it
now" rather than "opt in". `session disable` still removes the unit, and now says out loud that the
key will put it back — removing the key is how a removal is made to stick.

**Every create goes through the unit.** On macOS that is `ensure_gui_session_domain` finally
getting its second caller, unchanged: it kickstarts the loaded agent, and its `Aqua` arm is already
the recursion guard for the agent's own `session up`. Linux gets the twin it never had —
`systemctl --user start` on the generated service, whose `Type=oneshot` means the call returns when
the `session up` it runs has finished. The guard there reads `/proc/self/cgroup` rather than an
environment variable, for the same reason the macOS one reads launchd's own `XPC_SERVICE_NAME`: it
is the init system's record of what it started, so it answers on units that are already installed
and costs no change to a generated file. It compares a whole path **segment**, so
`zellij-session-my.service` cannot answer for `zellij-session-mysession.service`.

The interception is one function shadowing the `zellij_client::start_client` import, rather than
two lines repeated at each of the six client call sites — a seventh call added later that quietly
did not have them is the exact shape of the bug this entry is about.

Two cases are deliberately not handed over. **`--restore <id>`** names a snapshot the unit was
never given and could not honour, so it keeps its shape and is built here; the ordinary
resurrection from the in-place cache IS handed over, and has to be — a managed session is created
after a reboot or a crash, by which time there is a cache for it, so leaving it out would have left
the feature almost never firing. And **the unit's own process** never asks the init system for
anything, which is the whole of the recursion story.

Failure behaves differently on the two paths, on purpose. The client path falls back to creating
the session here, loudly: somebody is waiting at a terminal, and the fault this prevents is a
session with fewer capabilities than it should have, not a session that does not exist. `session
up` is stricter and reports a post-condition failure, because nobody is waiting for it.

Nested key, so the rule of the day applied with no exceptions: `session_service` parses its own
children, and an older build fails the **whole** config on `managed_session`, not just the block.
Verified against `0.45.0-nkmk.15`, which answers `Unknown session_service entry: "managed_session"`
and refuses the file. It could not be seeded into a shared config ahead of the binary; it shipped
with the code that accepts it. This is one of the two keys that bought [the entry that softened
that rule](#an-unknown-key-in-session_service-warns-instead-of-failing-the-config).

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
- Clearing on a reload takes a note somebody set by hand on that pane with it. That is what
  re-running a command pane does to its note, and the two behaving differently would be worse.

The frame's red flash (`AddRedPaneFrameColorOverride`) was the obvious hook and is the wrong one
twice over: it is a one-second animation, and it is the same field the multi-select highlight uses,
so a durable mark stored there would be wiped by unrelated UI and suppressed for as long as the
pane sits in a selection group.

### `SessionInfo.plugins` says which plugins a session is running

The field has been on `SessionInfo` all along, it has a protobuf tag, and both conversions were
written. It was empty in every answer anyone could get to. Two independent gaps did it, and each one
alone was enough:

- **Nothing joined the two halves before the file was written.** Screen builds the session's
  metadata and has no idea which plugins run, so it left the map empty and said the wasm thread
  would fill it in. The wasm thread does report the list — to the background job that writes
  `session-metadata.kdl` — and the writer read the metadata and the plugin list out of two
  variables without ever putting them together. They are joined now at write time rather than when
  either half arrives, because the two are reported independently and only the freshest of each
  belongs in the file.
- **The KDL codec dropped the field outright**, with a comment saying so. So even a correct map went
  to disk as nothing.

What that cost: a session could be told its own plugins, by a patch-up applied in memory after the
read, and every *other* session on the machine came back claiming to run none. So the
`Event::SessionUpdate` a plugin subscribes to, which carries every session on the machine, was
right about exactly one of them.
`zellij ls` reads the same metadata and does not go through that patch-up at all, so what it read
held no plugins for any session, its own included.

The wire format is `plugins { plugin id=0 location="zellij:tab-bar" { configuration { … } } }`, and
a plugin with no configuration writes no `configuration` block. **Metadata written by an earlier
build has no `plugins` node and reads back as a session running none**, rather than as a parse
failure — the file on disk outlives the binary that wrote it, and a machine mid-upgrade has both.

Nothing about the plugin contract changed: the field, its tag and both `TryFrom` implementations
were already there, so this is a field that starts arriving populated rather than a new one.

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

### A keychain that will not answer is not a keychain with nothing in it

`find_identities` read `security find-identity` and returned a list. Both ways that command can fail
came back as an empty one. The `Err` arm — the command not running at all — was mapped to
`Vec::new()` outright, and the `Ok` arm never looked at `success`, which is where the real failure
lands: the `Commander` contract reserves `Err` for a program that could not be started, so a
`security` that ran and refused is `Ok` with `success: false` and an empty stdout. That parses to
the same empty list a machine with no certificates at all produces.

The consequence is the one thing on this path that cannot be undone. An empty list means an empty
ladder, an empty ladder means the self-signed rung, and the self-signed rung mints. A Mac whose
login keychain was locked or wedged — `User interaction is not allowed` — was therefore told it had
no Apple certificate, and doctor would mint one over the identity it already held.

`find_identities` now returns `Result<Vec<Identity>, String>`, and each of its three callers says
what it does with an unanswered question. `sign_pin` stops before the ladder with a `Needs you`
quoting `security`'s own refusal, saying in as many words that this is not the same as holding no
certificate and that nothing was minted or signed. The re-listing after a mint falls back to an
empty ladder, which is the Xcode-steps `Needs you` — the right place for a keychain that went away
mid-run. And `judge_reimport` refuses to judge: an unanswered listing reads as `NotOurs`, which
would have set aside the bundle that had just imported perfectly well, so a failure to ask now
leaves the file where it is and says so.

### A dry run's "would sign" is a repair, not a reassurance

`session doctor --dry-run` reaching a rung it could sign with filed the finding as `Already
correct`. The branch is only reached because the pin's signature is wrong — unsigned, ad-hoc, or
naming a code hash — so the line read as reassurance about the single thing that was broken. An
ad-hoc pin reported "would sign &lt;pin&gt; with an Apple Development certificate" under the heading
for what needs nothing done to it.

It is a `Changed` now, which is what that status is for: doctor's own definition is "acted, and the
thing is now right — also what a dry run records for a fix it would have made". The exit code does
not move, because a dry run of work doctor does by itself is not waiting on a person. This is
already what the sibling branch does for the certificate it would mint; the two now agree.

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

### `watchdog_interval_secs`: one key retimes the watchdog on both platforms

macOS could already be retimed and Linux could not. `StartInterval` is a plist key, so it went
through the `launchd { keys { … } }` hatch like any other; the systemd timer's `OnBootSec=` and
`OnUnitActiveSec=` were written from a constant, and the `systemd` sub-block has no `timer` section
to reach them through — deliberately, since everything in that block writes into the *service*.
So the one file the interval actually lives in on Linux was the one file the config could not
touch.

```kdl
session_service {
    watchdog_interval_secs 15
}
```

Seconds, a whole number, at least 1. It is one neutral key rather than a pair because the two
watchdogs are meant to tick together — the systemd timer exists so that Linux matches what
`StartInterval` already gave macOS, and two keys would let a config drift the platforms apart while
reading as if it had set *the* interval. Seconds and a bare integer because that is what launchd's
key means and what systemd reads a unitless time span as; nothing here writes `15min`.

**Linux**: both `OnBootSec=` and `OnUnitActiveSec=`, since the boot pass and the steady-state pass
are the same check. The service file's own comment quotes the same number — it explains what the
paired timer does, and a comment saying 60 beside a timer saying 15 is how the next person learns
the wrong number.

**macOS**: it becomes the DEFAULT for `StartInterval`, not an override of it. Resolution order is
an explicit `launchd { keys { StartInterval N } }`, then this key, then the built-in default
(15 since nkmk.17). The extra
wins because it names launchd's own spelling and therefore one platform, which is the more specific
of the two statements; the key is the fleet-wide default it falls back to. The key is still taken
out of the extras and written once, so the plist is a plist.

Unset, every generated file is byte-identical to what it was — asserted by comparing the output of
all three generators against a default block.

**A changed interval is drift, not an upgrade.** Nothing rewrites a unit because a config file
changed: `session status` compares the installed files against what this build would generate, and
the timer is one of the files `service_files` yields, so a machine whose config now says 15 reports
the timer as drifted until `session enable` is re-run there. That is per machine, and it is the
same shape as every other key in this block.

`zellij setup --dump-service` still prints the service or the plist and never the timer, which is
left exactly as it was. The consequence is worth knowing rather than fixing: on macOS the dump
shows the interval, because `StartInterval` is a key on the job itself, while on Linux it appears
only in the comment line — the file that carries it is not the file that command dumps.

Nested key, so the rule of the day applied with no exceptions: `session_service` parses its own
children, and an older build fails the **whole** config on `watchdog_interval_secs`, not just the
block. It could not be seeded into a shared config ahead of the binary; it shipped with the code
that accepts it, and every machine took the binary first. A key added after [the entry that
softened that rule](#an-unknown-key-in-session_service-warns-instead-of-failing-the-config) is no
longer held back this way.

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
launchd actually reads, which is the more specific of the two statements — the same argument that
makes `StartInterval` beat [`watchdog_interval_secs`](#watchdog_interval_secs-one-key-retimes-the-watchdog-on-both-platforms).
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
for that sliver. `watchdog_interval_secs` still overrides in either direction.

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
`sync_scroll_mode_from_scroll_state` — the function that holds upstream's derive-the-mode-from-
`is_scrolled` logic. Every one of the 37 call sites reaches that function: the focus verbs through
`sync_scroll_mode_on_focus`, the scroll verbs through `sync_scroll_mode_if_scroll_changed`, and the
pane-id-targeted ones through `sync_scroll_mode_for_pane_id`. So one guard disables the lot and no
call site is edited. `jump`'s other half is a single branch in `Tab::write_to_pane_id`, inside
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

### Each pane keeps its own scroll and search mode across focus changes

Upstream 0.45 stores the input mode per client, so it is one mode for every pane that client looks
at. Scroll position, search term, search results and the search toggles are already per pane — the
mode flag is the only thing that is not. The result is the complaint in upstream
[#5419](https://github.com/zellij-org/zellij/issues/5419) and
[#5470](https://github.com/zellij-org/zellij/issues/5470): search a log in one pane, glance at
another, come back, and the pane is still where you left it while your mode is not. Neither issue
has a maintainer reply.

Now a pane remembers the mode it was left in, and focusing it puts the client back in that mode.

- **Only the scroll/search family is remembered** — `Scroll`, `Search`, `EnterSearch`. Every other
  mode is a transient menu that belongs to the client, not to a pane, and returning to the default
  mode clears the memory rather than storing the default as a mode of its own.
- **Focusing a pane that remembers nothing takes you out of the family**, back to the default mode.
  Otherwise you would sit in `Search` on a pane that has no search, which is half of what the
  issues are complaining about.
- **The restore works under all three `input_while_scrolled` values.** That option governs the mode
  zellij *derives* from a pane being scrolled; a mode you picked for a pane is not its to undo.

The memory is **per pane, not per (pane, client)**, matching the scroll and search state it sits
beside. It lives on the pane itself, behind two default-implemented `Pane` trait methods
(`remembered_input_mode` / `set_remembered_input_mode`) that only `TerminalPane` overrides. A map
on `Screen` would have had to be pruned on every close path or leak for the life of the session;
on the pane, a closed pane's mode dies with it and a pane that moves between tabs carries its mode
along. Plugin panes and the test mocks keep the defaults and needed no edit.

**Detach and re-attach keep the memory, one step later than you would guess.** `AddClient` does
call the focus sync, but a client that has just joined has no mode yet, so the sync returns
without doing anything — and the `ChangeMode` that `AttachClient` sends next, putting the client
in its default mode, would have *cleared* the focused pane's memory. So a client's **first** mode
change never clears one. Somebody joining a session cannot wipe the mode the people already there
are using, the memory survives a detach, and it is restored on the re-attached client's next focus
change rather than on the attach itself.

**One guard was needed, and it is the whole reason this is more than a two-file patch.** Focus
moves *first*, and the mode change is a round trip through the server thread — the screen cannot
change how a key resolves on its own, so it sends `ServerInstruction::ChangeMode` and the change
lands later. By then the client's active pane is the pane it just arrived at, which is the wrong
pane for `change_mode`'s "leaving the search family clears the search" branch to act on. Left
alone, focusing away from a scrolled pane would silently wipe the search of the pane you focused.
So both `ServerInstruction::ChangeMode` and `ScreenInstruction::ChangeMode` carry an
`is_focus_restore` flag, and `change_mode` skips the clear when it is set. A user's own
`SwitchToMode` passes `false` and still clears the search exactly as upstream does. Both enums are
server-internal Rust; neither appears in a `.proto`, so the flag costs nothing on the contract.

**Nothing crosses the plugin API.** `PaneInfo` gains no field, `event.proto` is untouched, and no
`TryFrom` impl moves. The per-pane mode is server-internal state projected onto the existing
per-client `ModeInfo` on every focus change, so `ModeUpdate` keeps firing with the client's mode —
which now happens to equal the focused pane's — and the status bar is right with no plugin change.
A plugin that wants to *read* a pane's mode would be a separate patch and a separate cost.

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
Now only the key is gated, by a new bundled plugin, `zellij:confirm-close-tab`.

The default binding is no longer `CloseTab`:

```
bind "x" {
    LaunchOrFocusPlugin "zellij:confirm-close-tab" {
        floating true
        move_to_focused_tab true
    };
    SwitchToMode "Normal"
}
```

- **A tab with one pane closes with no prompt at all.** The plugin decides in `load`, from
  `get_focused_pane_info` and `get_tab_info`, before it has rendered a frame. There is nothing to
  confirm when closing the tab loses nothing, and the key stays as fast as it was.
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

**Known gap: the tab-mode hint line no longer lists the close key.** The status bar finds the key
for a hint by matching the action list it is bound to — `[CloseTab, SwitchToMode "Normal"]`, in
`default-plugins/status-bar/src/second_line.rs` and `one_line_ui.rs`. The binding is now a
`LaunchOrFocusPlugin`, which nothing there matches, so `x  Close` has dropped out of the tab-mode
line entirely. Teaching the status bar to recognise the launch is a separate patch.

**Rollout order is the reverse of the usual one.** The binding lives in the binary's own
`default.kdl`, so nothing has to be rolled out at all: an older binary keeps `CloseTab` and a newer
one gets the prompt. A user `config.kdl` that already overrides tab-mode `x` keeps its own binding
and sees no change until it is rewritten — and it must not be rewritten ahead of the binary,
because `zellij:confirm-close-tab` is a plugin location an older build does not have.

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

`upstream` points at `zellij-org/zellij`. Each patch is its own commit on top of the recorded
upstream base, currently the **`v0.45.0`** tag. Moving to a newer base is
`git fetch upstream --tags && git rebase --onto <new-tag> v0.45.0`, then recording the new base
here.

**A moved base is also two consts.** `UPSTREAM_BASE_TAG` and `UPSTREAM_BASE_COMMIT` in
`zellij-utils/src/consts.rs` are what `zellij --version` prints, and nothing derives them — update
both in the rebase commit. The commit is `git rev-parse --short '<new-tag>^{commit}'`; upstream
tags are annotated, so `--short <new-tag>` gives the tag object instead and is the wrong answer.

The base moved from `98a083707` (an upstream `main` commit) to the **`v0.45.0`** tag and took twelve
upstream commits. Five of them met a patch here.

Upstream now puts tabs in order of actual position when it serializes a session (`85cc8b1fe`,
#5224) — the identical fix this fork carried, so the fork's copy is dropped and upstream's is what
runs. Upstream also grew its own `IpcReceiveError { Disconnected, Undecodable }` and a
`try_recv_client_msg` returning `Result`, which is the fork's `IpcRecvError { Disconnected,
Malformed }` under other names; the fork's enum, its route-thread arm and its socket test are
dropped in favour of upstream's, whose test covers strictly more.

Upstream took client/server contract Action tags **148-151** for the scrollback-prompt actions. The
fork's `move_tab_to_index` was on 148 — its one tag outside the fork-reserved block — and moves to
**172**, next to the rest of the fork's actions at 160-171. No other tag changed, and the contract
version is unchanged.

Upstream's new scroll-mode test snapshot is re-recorded, for the same reason it was at the last
rebase: a pane frame here also carries the pane's fixed-width handle, so the scroll indicator sits
further left than an upstream frame puts it.

Upstream reworked the about plugin (`18cb94e1c`, `4156b5023`): a keybinding-migration feature with
its own `<u>`/`<y>`/`<n>` keys, `main_screen()` and `main_screen_builder()` helpers, and help lines
assembled as a string and coloured by substring instead of by hardcoded column ranges. The fork's
server-binary paragraph and `<c>` copy key are threaded through the reworked functions as one more
parameter; the fork's two helpers that computed where `<c>` landed are deleted, because the
substring colouring makes that arithmetic unnecessary.

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

**`Rust` runs on the branch, not only on main.** Its push trigger lists the conventional-commit
prefixes this fork names branches after — `feat/`, `fix/`, `ci/`, `chore/`, `docs/`, `perf/`,
`refactor/`, `style/`, `test/`, `rc/` — so a patch goes green where it is written rather than after
it lands, which is what "prove it on the branch" needs. The list is enumerated rather than `**` so
that `backup/` snapshots and `rebase/` scratch branches do not each burn a full matrix, and a
`rust-${{ github.ref }}` concurrency group cancels a superseded push. `e2e.yml` is deliberately not
on that list — it builds a binary and drives it through a docker terminal, and it is the slowest
thing here — but it now has a `workflow_dispatch`, so it can be aimed at a branch on demand.

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

To rebuild an existing tag (workflow changes, a lost asset):

```
gh workflow run release.yml -R noahkiss/zellij -f tag=v0.45.0-nkmk.1
```
