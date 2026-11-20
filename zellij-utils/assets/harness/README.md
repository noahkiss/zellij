# Harness map

The zellij fork keeps harness facts and screen rules in this directory.

`zellij setup --harness-map` prints the inventory and rules.
Add `--json` to read the same data as a schema-versioned object.
The command needs no running session and reads no harness files.
Each fact contains `key`, `value`, `source`, `revision`, and `verified`.
Optional `notes` describe limits and source conflicts.
A verified fact describes its cited source, not every installed release.

## Files

| Directory | Contents |
|---|---|
| `inventory/` | One TOML inventory for Claude, Codex, OpenCode, and Pi. |
| `herdr/` | Four verbatim detection manifests, Apache-2.0 license, notices, and regression screens. |
| `local/` | Fork supplements for explicit question footers and first-run gates. |

Paseo revision `e135e9c101700b31671fc374325d17b608467a91` supplies native title facts.
Herdr revision `87e5c13c6028df72405f63388dde585212ad3019` supplies the detection engine and manifests.
Other facts cite their own source revisions.
Documentation citations record their retrieval date.

## Evaluation

The zellij fork exposes `zellij_utils::harness_map::HarnessDetector`.
Compile each detector once with `HarnessDetector::new(label)`.
Call `evaluate` with rendered screen text and optional OSC title/progress strings.
`evaluate_screen(label, screen)` provides the screen-only convenience function.

The evaluator preserves Herdr regions, nested gates, exclusions, priorities, and engine requirements.
Equal upstream priorities retain the first matching rule.
Local question rules can refine equally ranked blocked rules.
Stronger upstream rules retain priority over local rules.
Transcript viewers return unknown with `skip_state_update=true`.
Callers must preserve their previous state when that flag is true.

The upstream evaluator defaults an unmatched known harness to idle.
Its result marks that decision with `idle_fallback=true` and `visible_idle=false`.
For that fallback, the public detector returns unknown.
It never treats a question mark in prose as an interactive question.
Some permission and question screens remain indistinguishable and return blocked.

Use this detector only when the harness supplies no state.
Native OSC 7501 and hook state retain authority.
No server, agent detection, transcript reader, or MCP tool uses the map yet.

## Unverified screens

`local_verified=false` marks the exported supplemental layer.
Codex rate-limit and hook-trust screens lack installed-version captures.
Pi project-trust wording also lacks an installed-version capture.
Question refinements reuse upstream footer evidence but need live verification.
OpenCode has no local question rule because its shared footer cannot prove request type.
Fixture files explicitly label synthetic screens.
The region tests also retain an Apache-2.0 upstream test fixture.

Codex approval facts describe agent-deck's displayed-choice checks.
A consumer must recheck the selected row and affirmative/negative choices before sending a digit.
It must check that the overlay disappears afterward.
The map does not send keys or accept trust gates.

## Updating sources

1. Choose a new immutable upstream commit.
2. Copy the four manifests and LICENSE from that commit without editing them.
3. Check file notices and update `herdr/NOTICE`.
4. Compare upstream engine semantics with the Rust evaluator before raising its engine version.
5. Update exported source revisions and affected inventory facts.
6. Add or refresh screen fixtures. Label synthetic screens and sanitize real captures.
7. Run the harness-map tests and a default plugin's wasm check.
8. Update the fork ledger and read the branch's Rust CI result.

Keep fork additions in `local/`.
Never change the vendored manifests to add a fork rule.
When updating Paseo facts, review its four providers at the new pinned commit.
Recheck native title precedence, storage overrides, and resume handles.

Fact sources beginning with `https://` link to external primary sources.
Other fact sources name paths in the zellij fork at the recorded revision.
These repository paths keep fork integration provenance independent of a private checkout or account.
