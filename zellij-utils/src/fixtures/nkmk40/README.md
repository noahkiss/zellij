# Golden JSON from 0.45.1-nkmk.40

What the released `0.45.1-nkmk.40` build wrote for `action list-tree --json`, `action list-panes
--json`, `action list-agents --json` and `action watch --json`. A program that drives zellij over the
CLI reads these keys, so a later build must keep them. `src/golden_json_keys.rs` tests them.

How they were made: the released binary served a detached scratch session (`attach
--create-background`, every scratch variable set, the agent variables unset). The session held a
login shell, a stand-in `claude` script that printed `OSC 1337 SetUserVar=agent_session` and two
`OSC 7501` reports (`working`, then `blocked` with kind, progress, title and message), a bash pane
that printed `OSC 133` A/B/C/D marks and an `OSC 7501` `done` and then had input typed after its
prompt, a bash pane left after `133;D;3`, one left after `133;C`, a stacked pane, a floating pane
and a command pane that exited 3. `watch.jsonl` is two short `watch` samples. During them panes
were opened, renamed and closed, a tab was renamed, the stand-in agent reported `done` and exited,
and its pane was closed. Paths, the host name and the user name were replaced with placeholders;
every other value is as written.

| File | What it holds |
|---|---|
| `expected-keys.txt` | Every key path in each fixture with the JSON types seen there. `[]` is any array element; `watch.jsonl` is keyed by `event`. |
| `not-in-this-build.txt` | The `list-panes` and `list-agents` keys this tree's row types no longer write: the fields removed after nkmk.40. |

Rewrite both files with `ZELLIJ_UPDATE_GOLDEN_KEYS=1` and review the diff.

The row-type test proves that this tree's types can carry the keys. It does not prove that a
server fills them: that needs the same capture from the new build, compared against
`expected-keys.txt`.
