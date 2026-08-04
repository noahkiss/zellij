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

## Build and test

```
cargo build --release
cargo test -p zellij-utils -p zellij-server
```

`rust-toolchain.toml` pins the toolchain; rustup installs it on the first cargo call. **Use
`--release`.** A debug build expects the WASM plugins to be built from source; the release build
uses the prebuilt assets in `zellij-utils/assets/plugins/`.

CI also runs `cargo xtask build` and `cargo xtask test` on Linux, macOS and Windows, plus a
`--no-web` test pass. A change behind a feature flag still has to compile without it.

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

- **No layout pass runs.** Pane geometry fields (`pane_x`, `pane_y`, `pane_rows`, `pane_columns`)
  and everything derived from them, including the stack fields, hold placeholders. Panes report
  identical overlapping geometry. The values become correct after a client attaches once.
- **A stack has no anchor.** `new-pane --stacked` has no focused pane to stack under, so it needs an
  explicit target: `ZELLIJ_PANE_ID=<id> zellij -s <name> action new-pane --stacked
  --near-current-pane`. Without one it now fails loudly with a non-zero exit.
- **Focus is not stable.** Each transient CLI client resets focus to the first tab. A focus-dependent
  action such as `move-tab` with no `--tab-id` therefore acts on tab 1, not on the tab you expect.
  Always pass an explicit target in a test.

`dump-screen` is not affected by any of this. The grid is maintained from the pty whether or not the
pane renders, so it returns fresh content for a pane in a non-focused tab of a detached session.

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
