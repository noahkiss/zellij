# Working on this fork

Notes for anyone — human or agent — who changes code here. [FORK.md](FORK.md) says *what* the fork
changes and how to install and release it. This file says *how to work in it*.

## What this repo is

A permanent personal fork of [zellij](https://github.com/zellij-org/zellij), based on the `v0.44.3`
tag. **Upstreaming is not a goal.** No patch here has been sent upstream, and the repo takes no
issues or pull requests.

A patch is judged by its value to a real consumer of this build — a person at a terminal, or a
program that drives zellij over the CLI. It is not judged by whether upstream would accept it. That
frees a patch to change CLI output or add a flag, but it does not free it to be large: the fork is
carried by rebasing onto a newer upstream tag, and every line of divergence is paid for again at
each rebase.

So:

- Keep patches **surgical and additive**. Add a field, add a flag, add a branch. Avoid rewriting an
  upstream function when a new one beside it will do.
- Keep each patch its **own commit** on top of the upstream tag. A rebase then replays them one at a
  time, and a patch that upstream has since fixed can be dropped whole.
- Prefer the cheap surface. A CLI-only struct costs nothing to carry; the protobuf contracts cost a
  lot (see below).

## Where things live

| Path | What it is |
|---|---|
| `FORK.md` | The patch ledger and the release runbook. Every behavioural patch has an entry. |
| `docs/web-api-assessment.md` | A design that was assessed and deliberately not built. |
| `.github/workflows/release.yml` | Replaces upstream's release job. Builds the two shipped targets. |

Private design notes and the candidate-patch wishlist are kept in a gitignored directory outside the
tracked tree. They are **not** part of this repo and must never be copied into it.

That directory is ignored by a machine-level rule, not by this repo's `.gitignore`. Do not rely on
the ignore: run `git status` before you commit and confirm no private note is staged.

## Conventions

- **Conventional commits**, scoped to the area: `feat(sessions):`, `fix(panes):`, `docs:`,
  `chore:`, `style:`, `test:`. Read `git log --oneline -20` and match it.
- **Never add an AI or agent attribution footer** to a commit — no `Co-Authored-By` for a model, no
  "generated with" line. Ever.
- **Update `FORK.md` in the same commit as the change it describes.** A patch that changes
  behaviour and does not touch the ledger is incomplete.
- **Run `cargo fmt` before you commit.** CI runs `cargo xtask format --check` and fails on drift.
- Do not bump the version as part of a feature commit. The version bump is its own `chore:` commit,
  made when a release is cut.
- **A release is not done until the notes match the tag.** Cutting a version means writing
  `changelogs/v<version>.md` in the same breath, and updating any out-of-repo notes that record the
  current version — the operator's tooling docs go stale silently and nothing in CI catches it.

## Branch, RC, squash-merge, rebase

One patch, one branch, one commit on main. The branch is where the work is messy; main keeps the
patch ledger readable, because the rebase onto a newer upstream tag replays it commit by commit.

1. **Branch per patch.** Never commit a patch to main directly.
2. **Prove it with a release candidate, on the branch.** Tag `v<version>-rc.1`, `-rc.2`, … and push
   the tag. `release.yml` builds and publishes an RC exactly like a final tag, with two differences:
   the GitHub release is marked `--prerelease`, and the tap bump rewrites the separate
   `zellij-nkmk-rc` formula. A Mac proof agent then installs the candidate without disturbing the
   formula every machine runs:

   ```
   brew update && brew unlink zellij-nkmk
   brew install noahkiss/tap/zellij-nkmk-rc
   # ... prove the patch ...
   brew uninstall zellij-nkmk-rc && brew link zellij-nkmk
   ```

   An RC tag carries a suffix the binary does not: the branch bumps `Cargo.toml` to the final
   version, so `zellij --version` prints `0.45.0-nkmk.12` under a `v0.45.0-nkmk.12-rc.1` tag. Both
   the tap's install check and the rc formula's test block strip `-rc.N` before comparing.
3. **Land by squash-merge**, so one ledger entry is one commit. `FORK.md` is updated in that commit,
   not in a follow-up.
4. **Tag the final release from main** — `v<version>`, no suffix. It bumps `zellij-nkmk` as before.
   RC tags are proof, never install targets; nothing outside the rc formula points at one.
5. **Rebase onto a newer upstream tag** with `git rebase --onto <new-base> <old-base> main`, then
   record the new base in `FORK.md`.

**Upstream strategy — the operating rule.** The fork is a patch series on top of an upstream
**tag**, and it stays one. Move the base by rebasing, one patch commit at a time, and name the new
base in `FORK.md`. Between tags, take a single upstream commit only when we want that fix now:
cherry-pick it with the subject `chore(upstream): <original subject> (<sha>)`, and expect it to drop
out as empty at the next rebase. Never merge upstream into main, and never let the base become
"some upstream main commit plus picks" without naming that commit in `FORK.md`. Rebasing onto
upstream `main` between tags is allowed when it is small — it is still a named base.

**Reordered history does not bisect.** A logically grouped series (one commit per ledger entry,
reordered) cannot keep every intermediate compiling — a file lands with its group, and a definition
can sit in a group placed after one of its callers. That is accepted: the tip and the last commit
of each release build, the middle may not. The last chronological, bisectable series is kept at
`backup/main-pre-squash-2026-08-16`; when a bug needs `git bisect`, bisect there.

## Build and test

```
cargo check                                  # while editing
cargo test -p zellij-utils --profile quick   # the loop
cargo build --release                        # once, before committing
```

`rust-toolchain.toml` pins the toolchain; rustup installs it on the first cargo call.

**Never build without optimizations.** A debug build expects the WASM plugins to be built from
source; an optimized build uses the prebuilt assets in `zellij-utils/assets/plugins/`. So plain
`cargo test` fails on the assets, and "just drop `--release` to save time" is not the shortcut it
looks like.

**Use `--profile quick` to iterate, not `--release`.** `quick` inherits `release`, so debug
assertions stay off and the prebuilt assets are still used, but it drops full LTO and raises
`codegen-units` — the two settings that make a release rebuild slow and that only earn their keep
in the artifact that ships. `--release` re-optimizes the whole program at every link and confines
each crate's codegen to one core; in an edit-test loop that is most of the wall-clock, and it buys
nothing a test run can observe.

Two things to know before reaching for it. `quick` gets its own `target/` subtree, so the first
build after switching is cold and slower than the release build it replaces — it pays back from
the second. And it is not what ships: build `--release` once before you commit, and never measure
runtime performance against `quick`.

Whole-binary builds are the expensive ones, because they pull in `zellij-server` and its embedded
`wasmtime`. Prefer `cargo check` and a `-p <crate>` test while iterating, and keep the full
`cargo build --release` for the end.

CI also runs `cargo xtask build` and `cargo xtask test` on Linux and macOS, plus a `--no-web` test
pass. A change behind a feature flag still has to compile without it. **There is no Windows job**,
and Windows is not a shipped target — the release builds `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin` and nothing else. Do not spend effort on a Windows-only failure.

**A change to `zellij-utils` must also build for wasm, and nothing here checks that for you.**

```
cargo check -p session-manager --target wasm32-wasip1   # or any default plugin
```

(`cargo xtask build --plugins-only` catches the same class and is what CI actually runs — use it
when you want the real artifacts, the check when you want speed.)

The default plugins are wasm crates that depend on `zellij-utils`, and their `.wasm` artifacts are
**checked in prebuilt** under `zellij-utils/assets/plugins/`. Every *local* build — `cargo check`,
`--profile quick`, `--release` — uses those prebuilt files and never compiles the plugin crates. So
`zellij-utils` can stop building for wasm entirely while everything you run locally stays green.

That is not hypothetical: it happened at nkmk.7 and survived two releases. `5198a3ebd` made
`session_service` read `DEFAULT_TERM` from `session_lifecycle`, which is
`#[cfg(not(target_family = "wasm"))]`. `session_service` is **not** gated, because `kdl/mod.rs`
parses its config block and is built for wasm — so the reference crossed the gate, and no plugin
could be rebuilt from then until `fa6bb9bc6`.

**CI caught it immediately. Nobody looked.** The `Rust` workflow builds plugins from source via
`cargo xtask build`, so every run from nkmk.7 on failed with this exact error on ubuntu, on macOS
and in the `--no-web` pass — most of the workflow red for two releases while the `Release` workflow,
which ships prebuilt assets, stayed green and made it look fine. So:

```
gh run list -R <fork> --limit 10 --json workflowName,conclusion,headBranch
gh run view <id> -R <fork> --log-failed | grep -iE "error\[|error:"
```

**Check the runs after pushing, and before cutting a release.** A green `Release` says the artifact
built, not that the tree is healthy — those two workflows fail independently and only one of them
compiles the plugins.

The trap is the asymmetric gating in `zellij-utils/src/lib.rs`. Before referencing another module
from an ungated one, check which side of `#[cfg(not(target_family = "wasm"))]` it sits on. If a
shared value is wanted on both sides, it belongs in an ungated module such as `shared` — see
`DEFAULT_TERM`, which lives there for exactly this reason. Gating the *consumer* instead is usually
wrong; it was tried first for `session_service` and broke `kdl`.

Run the wasm check before any release that touches `zellij-utils`, and always before shipping a
change to a default plugin — otherwise the plugin change silently is not in the artifact.

## Testing against a running zellij

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

Then kill the sessions and confirm the servers are gone when you finish.

**The scratch path must be short.** A unix socket path maxes out near 104 bytes, and the session
socket sits several directories below `ZELLIJ_SOCKET_DIR`. A deep path fails with `IPC socket path
is too long` and the failure does not name the cause. Use something like `/tmp/zj-test`, not a path
under a home directory or a per-session temporary directory.

### Pitfalls in a detached session

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

## Adding a field, and which contract it crosses

Three surfaces look similar and cost very different amounts.

- **`PaneListEntry` is CLI-only.** It exists to shape `list-panes --json`. Add a field to the struct,
  populate it, done. No protobuf, no contract change, no plugin visibility.
- **`PaneInfo` crosses the plugin API.** A new field means: the field in `zellij-utils/src/data.rs`,
  a tag in `zellij-utils/src/plugin_api/event.proto`, regenerated prost output (`cargo xtask
  build`), **both** `TryFrom` implementations in `zellij-utils/src/plugin_api/event.rs`, and every
  exhaustive struct literal elsewhere in the tree, tests included.
- **The client/server contract** (`zellij-utils/src/client_server_contract/`) is the expensive one. A
  new `Action` adds a message tag to it. That is tolerable without a contract-version bump — a fork
  client against a stock server of the same contract simply gets nothing for that one action — but a
  changed or reordered existing message is not. Do not touch the numbering of what is already there.

Sockets are scoped by contract version, not by version string, so a fork build and a stock build of
the same contract share sessions. Keep it that way.

## When a launcher becomes the thing that creates the session

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

## Rolling a new config key out before every machine has the binary

**An unknown TOP-LEVEL key in `config.kdl` is ignored, not an error.** Verified by running a config
carrying three new keys through the previous release's binary: `setup --check` still reported
`CONFIG FILE: Well defined`.

**The rule stops at the top level. A block that parses its own children rejects an unknown one, and
that fails the WHOLE config, not just the block.** `session_service` is the known example:

```
× Failed to parse Zellij configuration
╰── Unknown session_service entry: "manage_my_session" (expected systemd, launchd, pin_exe, managed_session or restart_via_launchd)
```

(`managed_session` was the key that taught this lesson, in 2026-08. It has since shipped, so it is
no longer an example of an unknown one — the rule it proved is unchanged.)

So a key nested inside such a block CANNOT be rolled out ahead of the binary — it has to ship with
the code that accepts it. Do not generalise a top-level probe to a nested key: test the key you
actually intend to add, in the position you intend to add it, and re-run `zellij setup --check`
**before committing**. Getting this wrong breaks every machine that pulls the config, not just the
one that is behind.

That decides the rollout order for a config-only feature. The key can be added to a shared config
first and the binaries upgraded afterwards, in any order — machines still on the old build ignore it
until they catch up. There is no need to hold the config back, and no need to gate it per machine.

The reverse is not true and is the trap: a shell alias, a service unit, or a script that calls a
**new subcommand** breaks immediately on any machine that has not upgraded. Config is forgiving;
the command surface is not. When a change spans both, land the config early and the callers last.

Worth re-verifying rather than assuming if a future release changes config parsing — the check is
two minutes: fetch the previous release's binary and run `setup --check` against the new config.

<!-- Optional local rules, untracked. The import is skipped when the file is absent. -->
@AGENTS.local.md
