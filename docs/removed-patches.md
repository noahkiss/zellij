# Removed patches

Features this fork once carried and removed. Each row says where the code still lives, so it
can be brought back. `git revert <removal commit>` restores one while the base is unchanged;
after a rebase, port it from the last release that had it.

| Feature | What it did | Last release with it | Removed in | Where to find it | Why removed |
|---|---|---|---|---|---|
| `pane_privacy` | A `pane_privacy { patterns_file pattern match_fields on_unknown_cwd tab_rule }` config block (or `ZELLIJ_PANE_PRIVACY_FILE`) hid matching panes and tabs from `list-panes`, `list-tabs`, `list-tree`, `list-agents`, `action watch` and the MCP, with a `withheld` count behind `--report-withheld`. | `v0.45.1-nkmk.40` | (this commit) | `zellij-server/src/pane_privacy.rs`, `zellij-utils/src/pane_privacy.rs`, `pane_privacy_refusal` and `strip_withheld_cwds` in `zellij-server/src/route.rs`; FORK.md "`pane_privacy`: panes the session keeps to itself" and "`pane_privacy` and `resurrect_command_hints` warn on a key they do not know" | No machine configured a policy, and its guard over every action in `route.rs` was a large divergence to carry at each rebase. |
