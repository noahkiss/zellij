# slim-keybinds

A minimal single-line keybind-hint bar for [Zellij](https://zellij.dev) 0.44, meant to replace the
bottom `zellij:status-bar` pane.

```
 Ctrl + g LOCK  p PANE  t TAB  n RESIZE  h MOVE  o SESSION    cpu 15% │ ram 7.2/23G │ disk 89.8/293G │ up 32d
```

Hints only — no mode-indicator ribbon, no secondary-info text, no arrow separators — plus an
optional system-vitals cluster in the right corner.

## What makes it different

**Hints are derived from the live keybind table, not from a list.** Unbind a key in `config.kdl`
and its hint disappears on its own. Rebind it to a different key and the hint follows.

**No colour literals.** Every colour comes from the active theme's component spec
(`ModeInfo.style.colors`), so changing the theme changes the bar with no rebuild and no colours
duplicated into the layout.

**Permissions: `ReadApplicationState` + `RunCommands`.** The second is only for the vitals probe.
The plugin never changes application state. A builtin is granted what it uses without asking, so
there is no prompt and nothing to seed — which matters here, because the pane calls
`set_selectable(false)` and a prompt rendered in it could never be answered.

## Build

A builtin of this fork: `cargo xtask build` builds it with the rest and embeds it in the binary,
so there is nothing to install. To iterate on it in a running session, point `builtin_plugin_dir`
at the build output and let `plugin_watch` reload it — see FORK.md.

```bash
cargo build --target wasm32-wasip1 --release -p slim-keybinds
```

## Use

In a layout:

```kdl
pane size=1 borderless=true {
    plugin location="zellij:slim-keybinds" {
        hide               "search,plugin-manager"
        rename             "session=sess,resize=size"
        show_locked_hints  "true"
        uppercase          "auto"
        max_hints          "12"
        show_vitals        "true"
    }
}
```

| Option | Default | Meaning |
|---|---|---|
| `hide` | — | Comma-separated case-insensitive substrings matched against hint labels. Hides the hint; the keybind still works. |
| `rename` | — | Comma-separated `from=to` label substitutions. `from` matches the label exactly, case-insensitively. |
| `show_locked_hints` | `true` | Draw hints in Locked mode. Ignored on the Unlock-First preset, where Locked is the base mode and hiding the unlock hint would be a dead end. |
| `uppercase` | `auto` | `auto` uppercases only the mode-entry hints. `true`/`false` force it. |
| `max_hints` | — | Cap on the number of hints. |
| `show_vitals` | `false` | Right-corner `cpu 37% │ ram 7.1/23G │ disk 89.8/293G │ bat 87% │ up 32d`. The battery segment is omitted entirely on machines without one; uptime shows one unit (`32d` / `11h` / `7m`). Refreshed every 10s by one `/bin/sh` probe. |

All options are optional and every unrecognised value falls back to the default rather than
erroring. There is deliberately no colour option — colours come from the theme.

When the line is too narrow, the vitals cluster is dropped first, then whole hints from the right
(lowest priority first). Labels are never truncated.

## Credit

The hint-derivation model is ported from zellij's own `default-plugins/compact-bar`
(`keybind_utils.rs`, `action_types.rs`) at v0.44.3; the shared-modifier prefix follows
`default-plugins/status-bar`'s `superkey`. Sibling builtin:
`slim-tab-bar`, the matching top bar.

MIT.
