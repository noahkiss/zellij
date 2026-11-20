# Upstream base, history rewrites and bisecting

How the fork's patch series sits on upstream, and how to check a rewrite of it. `AGENTS.md` has the
short form.

**Rebase onto a newer upstream tag** in four steps, on scratch branches. `main` takes only the
result.

1. **Condense.** Fold the series on the old base into one commit per area. Each area commit's body
   lists its ledger entries, one per line, and the tip tree does not change.
2. **Merge.** Merge the condensed tip into the new tag in one merge commit, conflict markers and
   all.
3. **Resolve.** Resolve the merge crate by crate, one commit per step, until the whole gate from
   "Build and test" passes on the tip.
4. **Carve.** Rebuild the resolved tree as one commit per area on the new tag. `git blame` through
   the merge's second parent names the area of each line in `git diff <new-tag> <resolved-tip>`. A
   line the resolution changed goes to the area that owns the rest of its hunk.
   `git diff <carved-tip> <resolved-tip>` must print nothing.

The carved series becomes `main`, and `FORK.md` names the new base. Keep the resolved merge on a
backup branch. Between rebases each patch lands as its own commit on top of the area commits.

**Upstream strategy.** The fork is a patch series on top of an upstream
**tag**, and it stays one. Move the base by the merge and carve above, and name the new base in
`FORK.md`. Between tags, take a single upstream commit only when we
want that fix now: cherry-pick it with the subject `chore(upstream): <original subject> (<sha>)`,
and expect it to drop out as empty at the next rebase. Never merge upstream into main, and never let the base become
"some upstream main commit plus picks" without naming that commit in `FORK.md`. Rebasing onto
upstream `main` between tags is allowed when it is small — it is still a named base.

**Reordered history does not bisect.** A logically grouped series (one commit per area or per
ledger entry, reordered) cannot keep every intermediate compiling — a file lands with its group,
and a definition can sit in a group placed after one of its callers. That is accepted: the tip and
the last commit of each release build, the middle may not. The chronological, bisectable series is
kept on backup branches: `backup/main-pre-squash-2026-08-16` for everything up to that squash,
`backup/main-pre-condense-2026-10-06` (old tip `8eb95917c`) for everything landed after it, up to
v0.45.1-nkmk.30, and one `backup/main-pre-<rewrite>-<date>` branch for each later rewrite. When a
bug needs `git bisect`, bisect there.

The 2026-10-06 condense kept the base (`v0.45.1`, `efd8fd5a8`) and the tip tree, and folded docs
follow-ups, test and snapshot touch-ups and later amendments into the commit of the ledger entry
they amend: 185 commits became 160. Each folded commit is named on an `Also:` line in its target's
body. Release bumps, changelogs and the `chore(upstream)` pick were not touched, and a fold that
would have needed a conflict resolution was left as its own commit. Every release tag up to
v0.45.1-nkmk.30 points into the pre-condense branch, not into `main`; the trees are identical.

**The current base is `v0.46.0` (`aedee21cb`).** The base before it, `v0.45.1` (`efd8fd5a8`), sits
on a release branch that v0.46.0 does not contain; the two meet at `18cb94e1c`. Across that move, a
three-way view whose base is the merge base shows code older than v0.45.1. Read the fork's intent
from `git diff v0.45.1 <old-tip>`, not from the merge base.

**After any history rewrite, check the series before you trust it.** Every latent bug a rewrite
has shipped so far was visible in the series itself on the day of the rewrite. Nobody looked. Two
kinds of rewrite, two checks:

- **Same base — squash, condense, reorder.** The tip tree must not change. Run
  `git diff <old-tip> <new-tip>` and expect no output. Any hunk it prints is one the rewrite dropped
  or mangled. Find it now, not two releases later.
- **New base — rebase.** Run `git range-diff <old-base>..<old-tip> <new-base>..<new-tip>`. It shows
  each patch's drift on its own, so a conflict resolved wrong stands out from the base moving under
  it. Read every patch it marks `!`. Then run the whole gate from "Build and test": the nextest
  suites, `cargo xtask integration-test`, the wasm check, and e2e — which runs on a pull request
  and on a push to `main`, never on a bare `rc/**` push.

Keep the pre-rewrite tip on a `backup/main-pre-<rewrite>-<date>` branch until both the checks and
the next release pass; it is the `<old-tip>` those commands need, and the only place to bisect.
