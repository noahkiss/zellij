# Working on this fork

Notes for anyone — human or agent — who changes code here. [FORK.md](FORK.md) says *what* the fork
changes and how to install and release it. This file says *how to work in it*.

## What this repo is

A permanent personal fork of [zellij](https://github.com/zellij-org/zellij), carried as a patch
series on an upstream base. The base moves; do not record it here. FORK.md's opening lines name
the current one. **Upstreaming is not a goal.** No patch here has been sent upstream, and the repo
takes no issues or pull requests.

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
| `docs/build-and-test.md` | The build and test gate in detail, and the traps behind each rule. |
| `docs/testing-live-sessions.md` | How to test without touching a real session; detached-session pitfalls. |
| `docs/fork-history.md` | The upstream base, history rewrites, backup branches for `git bisect`. |
| `docs/launcher-environment.md` | What a launchd or systemd unit must carry that a login shell supplies. |
| `docs/config-compatibility.md` | What an older build does with a config key it does not know. |
| `docs/web-api-assessment.md` | A design that was assessed and deliberately not built. |
| `.github/workflows/release.yml` | Replaces upstream's release job. Builds the two shipped targets. |

Read the matching file before you change that area.

## Conventions

- **Conventional commits**, scoped to the area: `feat(sessions):`, `fix(panes):`, `docs:`,
  `chore:`, `style:`, `test:`. Read `git log --oneline -20` and match it.
- **Never add an AI or agent attribution footer** to a commit: no `Co-Authored-By` for a model, no
  "generated with" line.
- **Update `FORK.md` in the same commit as the change it describes.** A patch that changes
  behaviour and does not touch the ledger is incomplete.
- **Run `cargo fmt` before you commit.** CI runs `cargo xtask format --check` and fails on drift.
- The version bump is its own `chore:` commit, made when a release is cut, never part of a feature.
- **A release is not done until the notes match the tag.** Cutting a version means writing
  `changelogs/v<version>.md` in the same change.
- Run `git status` before you commit and stage only files that belong to the change.

## Branch, RC, squash-merge

One patch, one branch, one commit on main. A rebase onto a newer upstream tag replays main commit
by commit, so main keeps one commit per ledger entry.

1. **Branch per patch.** Never commit a patch to main directly.
2. **Prove it with a release candidate, on the branch.** Tag `v<version>-rc.N` and push the tag by
   name (`git push origin v<version>-rc.N`); `push.followTags` is off. `release.yml` publishes an
   RC as a `--prerelease` and bumps the separate `zellij-nkmk-rc` formula, so a candidate installs
   beside the release:

   ```
   brew update && brew unlink zellij-nkmk
   brew install noahkiss/tap/zellij-nkmk-rc
   # ... prove the patch ...
   brew uninstall zellij-nkmk-rc && brew link zellij-nkmk
   ```

   An RC binary reports the RC (`zellij --version` prints `0.45.0-nkmk.12-rc.1`). RC tags are
   proof, never install targets.
3. **Open a pull request when the RC tag goes up.** The `End to End tests` workflow runs only on
   a pull request and on a push to `main`. Do not tag the release until it is green. The PR is
   never merged: land as below and close it.
4. **Land by squash-merge**, with `FORK.md` updated in that commit.
5. **Tag the final release from main** (`v<version>`, no suffix) and push it by name.

The upstream base, rebases, and the checks after any history rewrite are in
`docs/fork-history.md`. Never merge upstream into main.

## Build and test

```
cargo check                                  # while editing
cargo fmt && cargo xtask format --check      # before every commit
cargo test -p zellij-utils --profile quick   # the loop: the area you changed
cargo check -p session-manager --target wasm32-wasip1   # after any zellij-utils change
```

- **Never build without optimizations.** A debug build expects the WASM plugins built from
  source. Use `--profile quick` to iterate; `--release` only for what ships.
- **A change to `zellij-utils` must also build for wasm.** The plugins are checked in prebuilt,
  so local builds stay green while CI's `cargo xtask build` fails.
- **A release needs three green surfaces:** the nextest suites, `cargo xtask integration-test`
  (the `Rust` workflow), and the Docker e2e suite (`End to End tests`, on the pull request).
  Check the runs with `gh run list` after you push.
- **To refresh the checked-in plugin assets** run `cargo xtask build --plugins-only --release`.
- Windows is not a shipped target. Do not spend effort on a Windows-only failure.

## Testing against a running zellij

**Never run a session-mutating command against a real session.** Point the environment at a short
scratch directory and use a throwaway session name:

```sh
export ZELLIJ_SOCKET_DIR=/tmp/zj-test/sock
export ZELLIJ_CONFIG_DIR=/tmp/zj-test/config
export XDG_CACHE_HOME=/tmp/zj-test/cache
export XDG_STATE_HOME=/tmp/zj-test/state
```

- On macOS the cache ignores `XDG_CACHE_HOME`. **Never run `delete-all-sessions` on macOS**: it
  deletes the real user's sessions. Delete scratch sessions by name.
- From inside an agent, start the scratch server with the agent variables unset
  (`env -u AGENT_SESSION_ID -u AGENT_KIND -u CLAUDECODE -u CLAUDE_CODE_SESSION_ID`).
- `docs/testing-live-sessions.md` has the reasons and the detached-session pitfalls.

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
