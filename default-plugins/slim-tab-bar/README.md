# slim-tab-bar

A minimal single-line tab bar for [Zellij](https://zellij.dev) that **inherits your theme**.

No colour configuration, because it holds no colour literals. Every colour is read from the
active theme's `Styling` at render time — `ribbon_selected` and `ribbon_unselected` for tabs,
`text_unselected` for everything else. Switch themes and the bar follows — no rebuild, nothing to
keep in sync.

That is the point of it. The widely used `zj-status-bar` reads zellij's *legacy* `Palette` API and
has no configuration, so its active-tab colour is pinned to `Palette.green` and is unreachable
from a modern theme. This plugin is the small replacement.

## What it shows

```
session  tab  tab  tab                                           3:47 PM
```

Session name (plain text, no background block), tab names with the active one highlighted, and a
right-aligned clock.

That is deliberately all of it. No mode indicator, no keybind hints, no clock, no pane counts —
`zellij:status-bar` at the bottom already covers those.

## Build

A builtin of this fork: `cargo xtask build` builds it with the rest and embeds it in the binary,
so there is nothing to install. To iterate on it in a running session, point `builtin_plugin_dir`
at the build output and let `plugin_watch` reload it — see FORK.md.

```bash
cargo build --target wasm32-wasip1 --release -p slim-tab-bar
```

## Use

In your layout:

```kdl
layout {
    default_tab_template {
        pane size=1 borderless=true {
            plugin location="zellij:slim-tab-bar"
        }
        children
        pane size=1 borderless=true {
            plugin location="zellij:slim-keybinds"
        }
    }
}
```

### Options

| Option | Default | Meaning |
| --- | --- | --- |
| `show_index` | `false` | Prefix each tab with its 1-based index |
| `show_session` | `true` | Session name on the left |
| `max_tab_len` | unset | Hard cap on a tab name in characters (minimum 3) |
| `session_gap` | `2` | Spaces between the session name and the first tab |
| `session_alias` | unset | Comma-separated `from=to` display substitutions, e.g. `mysession=GFF`. Exact match only |
| `show_clock` | `true` | Clock on the right |
| `timezone` | `UTC` | IANA zone name, e.g. `America/New_York` |
| `clock_format` | `%-I:%M %p` | chrono strftime string; `%H:%M` for 24-hour |

All are optional, and unrecognised values fall back to the default rather than erroring.

Long tab lists are fitted to the terminal width by shortening the longest name first and
re-measuring, so a couple of verbose tabs don't force every short one into an unreadable stub.

## Clock

WASM/WASI exposes a UTC clock but no timezone, so set `timezone` to an IANA name. The IANA
database is compiled in via `chrono-tz`, so daylight saving is handled and the setting never
needs a seasonal edit — and, unlike shelling out to `date`, it costs no extra permission. The
trade is binary size: the zone database roughly doubles the `.wasm`.

Set `show_clock "false"` to drop it; the right-hand side is then empty.

## Mouse

Click a tab to focus it; scroll over the bar to move to the next or previous tab. Both match
zellij's built-in compact-bar.

This uses `ChangeApplicationState` alongside `ReadApplicationState`. A builtin is granted both
without asking, so there is no prompt. That is the plugin's full permission set — no command
execution, no filesystem, no network.

## Requirements

Zellij 0.44+ (uses the `Styling` component theme spec).

## License

MIT
