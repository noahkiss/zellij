# Build and test

The build and test gate in detail, with the traps behind each rule. `AGENTS.md` has the short
form.

```
cargo check                                  # while editing
cargo fmt && cargo xtask format --check      # before every commit
cargo test -p zellij-utils --profile quick   # the loop: the area you changed
```

Local runs the quick loop. The branch push runs the full gate: `rust.yml` triggers on every
`feat/**`, `fix/**`, `rc/**` (and the other prefixes it lists) push and runs `cargo xtask build`
(the plugins from source, so wasm is covered), `cargo xtask test` on every crate, `cargo xtask
integration-test`, the `--no-web` pass and the assets check, on Linux and macOS. The pull request
adds the Docker e2e suite. Do not repeat that locally; read it with `gh run list` and
`gh run view --log-failed`. Local release builds are not a gate either: `release.yml` builds the
artifact that ships.

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
the second. And it is not what ships: never measure runtime performance against `quick`.

Whole-binary builds are the expensive ones, because they pull in `zellij-server` and its embedded
`wasmtime`. Prefer `cargo check` and a `-p <crate>` test while iterating, and leave the whole
binary to CI.

**Three test surfaces, and a release must see all of them green.** The nextest suites (`-p zellij-utils
-p zellij-server -p zellij-client`, plus `-p zellij` after a plugins-only build) are the fast loop.
`cargo xtask integration-test` is the whole-app suite the `Rust` workflow runs — snapshot changes
that touch pane titles or handles land here, and nothing in the fast loop covers it. The Docker
e2e suite (`src/tests/e2e`, the `End to End tests` workflow) runs on pushes to `main` and on pull
requests — **an `rc/**` push never exercises it**, which is why the RC opens a pull request. The
release gate is a green `Rust` run on the RC branch and green `Rust` and `End to End tests` runs
on its pull request, on the exact commit to be tagged, checked with `gh run list`. Run the
integration suite locally only when you expect a snapshot to change, because accepting a snapshot
needs a local run. The v16 release shipped with four stale integration snapshots and two stale
e2e snapshots because local gates covered only the first surface. v19 reached `main` with six
stale plugin-system snapshots for the same reason, fixed in `5fef6e6b1` before the tag.

**That e2e job is also the only runner of zellij-server's `--ignored` plugin-system suite.**
`cargo xtask ci e2e --test` runs the Docker e2e tests, then runs `cargo test -- --ignored` inside
`zellij-server` (`xtask/src/ci.rs:195-221`). Nothing else passes `--ignored` — not `cargo xtask
test`, not the nextest suites — so the fast loop never compiles those tests, let alone runs them.
Their snapshots capture `RunCommand`'s derived `Debug`, so a new field on it lands here and in no
other surface. Run them locally only when `RunCommand`'s `Debug` changes; without Docker:

```
cargo xtask ci e2e --build                                  # builds the fixture plugin
cargo test -p zellij-server -- --ignored --test-threads 1
```

`--build` compiles the plugins for `wasm32-wasip1` and copies them to
`<target dir>/e2e-data/plugins/`, where the tests look for
`fixture-plugin-for-tests.wasm`. `<target dir>` honours `CARGO_TARGET_DIR`, so the build and the
tests agree only when both see the same value.

CI also runs `cargo xtask build` and `cargo xtask test` on Linux and macOS, plus a `--no-web` test
pass. A change behind a feature flag still has to compile without it. **There is no Windows job**,
and Windows is not a shipped target — the release builds `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin` and nothing else. Do not spend effort on a Windows-only failure.

**A change to `zellij-utils` must also build for wasm, and nothing here checks that for you.**

```
cargo check -p session-manager --target wasm32-wasip1   # or any default plugin
```

(`cargo xtask build --plugins-only` catches the same class and is what CI actually runs — use it
when you want a real wasm build, the check when you want speed.)

**To refresh the checked-in assets, that command needs `--release`:**

```
cargo xtask build --plugins-only --release
```

Only the release path copies the built `.wasm` files into `zellij-utils/assets/plugins/`
(`xtask/src/build.rs`, `build_plugins_release_into_assets`). Without `--release` the plugins are
built to `target/wasm32-wasip1/debug` and the checked-in assets are left exactly as they were — so
a plugin change "builds" and still is not in the binary.

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
gh run list -R noahkiss/zellij --limit 10 --json workflowName,conclusion,headBranch
gh run view <id> -R noahkiss/zellij --log-failed | grep -iE "error\[|error:"
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
