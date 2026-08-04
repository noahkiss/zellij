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
