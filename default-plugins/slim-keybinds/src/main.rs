//! slim-keybinds — a single-line Zellij keybind-hint bar that inherits the active theme.
//!
//! WHAT IT REPLACES
//!
//! The bottom `zellij:status-bar` pane. That plugin exposes exactly ONE config key (`classic`),
//! so there is no way to drop its mode-indicator ribbons or its right-hand secondary-info text —
//! both of which this bar deliberately does not draw. The hints ARE the bar.
//!
//! THE DERIVATION RULE
//!
//! Every hint comes from `ModeInfo`'s live keybind table (see `derive.rs`), never from a literal
//! list. Unbinding a key in config.kdl therefore removes its hint automatically. A hardcoded hint
//! table would keep advertising a key that no longer does anything, which is worse than showing
//! nothing.
//!
//! THE COLOUR RULE
//!
//! This crate contains no colour literals. Every colour is read at render time from
//! `ModeInfo.style.colors` — a `Styling`, zellij 0.44's component spec — so changing the theme
//! changes the bar with no rebuild. Do not add a colour config option and do not fall back to the
//! legacy `Palette`: that is the exact bug `slim-tab-bar` was written to escape.
//!
//! WHY RAW ANSI RATHER THAN THE COMPONENT API
//!
//! Same reason as `slim-tab-bar`: a ribbon is a DCS escape the zellij CLIENT expands, so a plugin
//! can never measure what it drew, and the overflow decision here needs exact widths.

mod action_types;
mod derive;
mod vitals;

use derive::{hints_for_mode, Hints};
use std::collections::BTreeMap;
use unicode_width::UnicodeWidthStr;
use vitals::Vitals;
use zellij_tile::prelude::*;

/// One column of bar background at each end, so nothing sits flush against the terminal edge.
/// Same convention as `slim-tab-bar`.
const EDGE_PAD: usize = 1;

const RESET: &str = "\u{1b}[m";

/// Separator inside the vitals cluster, drawn dim. Same glyph as `zellij-claude-bar`'s cluster,
/// so the two bars read as one system.
const VITALS_SEP: &str = " \u{2502} ";

/// How often the vitals probe runs, once it has settled. Fast enough that a spinning CPU shows
/// up while you are still looking at it, slow enough that a `/bin/sh` spawn per tick is noise.
const VITALS_INTERVAL: f64 = 10.0;

/// A drawn segment plus its VISIBLE width. The escapes make `part.len()` meaningless, so the
/// width is tracked alongside — the whole reason for drawing raw ANSI. Same shape as zellij's
/// own `LinePart`.
struct Segment {
    part: String,
    len: usize,
}

/// When to uppercase hint labels.
#[derive(Clone, Copy, PartialEq, Default)]
enum Caps {
    #[default]
    /// Uppercase only the mode-ENTRY hints, i.e. while sitting in the base mode. Those are
    /// proper nouns for modes (`PANE`, `TAB`) and read as a menu; the hints inside a mode are
    /// verbs (`fullscreen`, `rename`) and shouting them is noise.
    Auto,
    Always,
    Never,
}

#[derive(Default)]
struct SlimKeybinds {
    mode_info: ModeInfo,
    /// `false` until the first `ModeUpdate`. Nothing renders before then: `style.colors` on a
    /// default `ModeInfo` is zellij's built-in palette, and drawing it would be the colour
    /// literal this crate refuses to contain.
    have_mode: bool,
    /// `ModeUpdate` can arrive with an EMPTY keybind table, so the table from `InitialKeybinds`
    /// is cached and patched back in. Straight from `status-bar/src/main.rs:218-230`; without it
    /// the bar intermittently renders nothing at all.
    cached_keybinds: KeybindsVec,
    /// `base_mode == Some(Locked)` is the "Unlock First" keybind preset. Recorded so Locked mode
    /// can keep its unlock hint even when `show_locked_hints` is off — on that preset Locked is
    /// where you START, and an empty bar there is a dead end.
    base_mode_is_locked: bool,
    hide: Vec<String>,
    rename: Vec<(String, String)>,
    show_locked_hints: bool,
    caps: Caps,
    max_hints: Option<usize>,
    /// Off by default: it costs the `RunCommands` permission and a `/bin/sh` spawn every 10s,
    /// neither of which a hint bar should pay for unasked.
    show_vitals: bool,
    vitals: Vitals,
}

/// `PaletteColor` as an SGR colour parameter. The enum has exactly two variants and both are
/// handled; anything else is a compile error, which is the point.
fn sgr_color(color: PaletteColor, foreground: bool) -> String {
    let channel = if foreground { 38 } else { 48 };
    match color {
        PaletteColor::Rgb((r, g, b)) => format!("{};2;{};{};{}", channel, r, g, b),
        PaletteColor::EightBit(n) => format!("{};5;{}", channel, n),
    }
}

/// Wrap `text` in one fg/bg run, resetting after it so no segment can leak style into the next.
///
/// `attrs` is a bare SGR rendition parameter — `""`, `"1"` (bold) or `"2"` (faint). Renditions
/// are not colours, so using one does not breach the no-colour-literals rule: "dim" here means
/// `text_unselected.base` at reduced intensity, which still tracks the theme.
fn paint(text: &str, fg: PaletteColor, bg: PaletteColor, attrs: &str) -> String {
    format!(
        "\u{1b}[{}{};{}m{}{}",
        if attrs.is_empty() {
            String::new()
        } else {
            format!("{};", attrs)
        },
        sgr_color(fg, true),
        sgr_color(bg, false),
        text,
        RESET
    )
}

/// Parse a KDL config value as a bool, with an explicit default.
/// Anything unrecognised falls back to the default rather than erroring: a bar that refuses to
/// render because of a config typo is worse than one that ignores it.
fn parse_bool(v: Option<&String>, default: bool) -> bool {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("true") | Some("yes") | Some("1") => true,
        Some("false") | Some("no") | Some("0") => false,
        _ => default,
    }
}

/// Split a comma-separated config value, trimming and dropping empties.
fn parse_list(v: Option<&String>) -> Vec<String> {
    v.map(|raw| {
        raw.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    })
    .unwrap_or_default()
}

impl SlimKeybinds {
    /// Hints for the current mode, after `hide` and `rename` have been applied.
    fn visible_hints(&self) -> Hints {
        let mut hints = hints_for_mode(&self.mode_info, self.mode_info.mode);
        // Matched against the ORIGINAL description, before renaming, so a `rename` entry can
        // never silently un-hide something. Substring so "session" catches "session-manager".
        hints.hints.retain(|h| {
            let label = h.label.to_ascii_lowercase();
            !self.hide.iter().any(|needle| label.contains(needle))
        });
        for hint in &mut hints.hints {
            if let Some((_, to)) = self
                .rename
                .iter()
                .find(|(from, _)| from.eq_ignore_ascii_case(&hint.label))
            {
                hint.label = to.clone();
            }
        }
        if let Some(max) = self.max_hints {
            hints.hints.truncate(max);
        }
        hints
    }

    fn uppercase(&self) -> bool {
        match self.caps {
            Caps::Always => true,
            Caps::Never => false,
            Caps::Auto => matches!(self.mode_info.mode, InputMode::Normal | InputMode::Locked),
        }
    }

    /// One hint as ` key label `, key in an emphasis colour and bold, label in the base colour.
    ///
    /// A pad column on each side and nothing else: adjacent blocks therefore sit two columns
    /// apart, which separates them without a glyph. No arrow separators, ever — same rule as
    /// `slim-tab-bar`, and not a capability question.
    fn draw_hint(&self, keys: &str, label: &str, colors: &Styling) -> Segment {
        let style = colors.text_unselected;
        Segment {
            part: format!(
                "{}{}{}",
                paint(" ", style.base, style.background, ""),
                paint(keys, style.emphasis_0, style.background, "1"),
                paint(&format!(" {} ", label), style.base, style.background, ""),
            ),
            len: 1 + keys.width() + 1 + label.width() + 1,
        }
    }

    /// The shared modifier, drawn once on the left as ` Ctrl +`.
    fn draw_prefix(&self, prefix: &str, colors: &Styling) -> Segment {
        let style = colors.text_unselected;
        let text = format!(" {} +", prefix);
        Segment {
            len: text.width(),
            part: paint(&text, style.emphasis_0, style.background, "1"),
        }
    }

    /// Blank fill in the line background, so the bar is one continuous strip.
    fn spacer(&self, width: usize, colors: &Styling) -> Segment {
        let bg = colors.text_unselected.background;
        Segment {
            part: paint(&" ".repeat(width), bg, bg, ""),
            len: width,
        }
    }

    /// The right-corner vitals cluster: `cpu 12% │ ram 7.0/23G │ disk 412/931G │ bat 87%`.
    ///
    /// Dim spelled-out labels, plain values — the same label language as `zellij-claude-bar`'s
    /// cluster, so the two bars read as one system. `None` when vitals are off or no probe has
    /// landed yet, which is also what keeps the bar from flashing a half-built cluster on
    /// startup.
    ///
    /// A segment the probe marked as an alert takes `exit_code_error` and bold — the same colour
    /// and weight `slim-tab-bar` gives its warning badge, from the same theme, so one wrong thing
    /// looks the same in both bars. Still no colour literal: it is the colour the theme itself
    /// picked for "something is wrong".
    fn vitals_segment(&self, colors: &Styling) -> Option<Segment> {
        if !self.show_vitals {
            return None;
        }
        let segments = self.vitals.segments();
        if segments.is_empty() {
            return None;
        }
        let style = colors.text_unselected;
        let mut part = String::new();
        let mut len = 0usize;
        for (label, value, alert) in segments {
            if len > 0 {
                part.push_str(&paint(VITALS_SEP, style.base, style.background, "2"));
                len += VITALS_SEP.width();
            }
            part.push_str(&paint(
                &format!("{} ", label),
                style.base,
                style.background,
                "2",
            ));
            let (fg, attrs) = if alert {
                (colors.exit_code_error.base, "1")
            } else {
                (style.base, "")
            };
            part.push_str(&paint(&value, fg, style.background, attrs));
            len += label.width() + 1 + value.width();
        }
        Some(Segment { part, len })
    }
}

impl ZellijPlugin for SlimKeybinds {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        // No `request_permission` here, and none of zellij's own bars has one either: a builtin
        // is checked against the binary it ships in, so every permission short-circuits to
        // Granted and a request would only raise a prompt in a pane nobody can focus to answer.
        // The permission cache, and the trimming problem an instance-dependent request set used
        // to cause there, stop applying with it.
        //
        // What this bar uses is ReadApplicationState (ModeUpdate, InitialKeybinds) and
        // RunCommands (the vitals probe, and nothing else). It never CHANGES application state.
        // A bar is not a pane you focus:
        set_selectable(false);
        subscribe(&[
            EventType::ModeUpdate,
            EventType::InitialKeybinds,
            EventType::Timer,
            EventType::RunCommandResult,
            EventType::PermissionRequestResult,
        ]);

        self.hide = parse_list(configuration.get("hide"))
            .into_iter()
            .map(|s| s.to_ascii_lowercase())
            .collect();
        self.rename = parse_list(configuration.get("rename"))
            .into_iter()
            .filter_map(|pair| {
                let (from, to) = pair.split_once('=')?;
                let (from, to) = (from.trim(), to.trim());
                (!from.is_empty() && !to.is_empty()).then(|| (from.to_string(), to.to_string()))
            })
            .collect();
        self.show_locked_hints = parse_bool(configuration.get("show_locked_hints"), true);
        self.caps = match configuration
            .get("uppercase")
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            Some("true") | Some("yes") | Some("1") | Some("always") => Caps::Always,
            Some("false") | Some("no") | Some("0") | Some("never") => Caps::Never,
            _ => Caps::Auto,
        };
        self.max_hints = configuration
            .get("max_hints")
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n > 0);
        self.show_vitals = parse_bool(configuration.get("show_vitals"), false);
        if self.show_vitals {
            // First tick soon rather than after a full interval, so the bar is not blank for
            // ten seconds on startup. Every later arm happens in the Timer arm — arming from
            // two places would fork the timer chain and double the probe rate.
            set_timeout(1.0);
        }

        // One line per instance into zellij's log ($TMPDIR/zellij-$UID/zellij-log/zellij.log,
        // and TMPDIR is ~/tmp here — the /tmp copy is months stale). This is the only proof from
        // outside the pane that a given build actually loaded, and it disambiguates "the option
        // was never passed" from "the option was passed and rejected".
        eprintln!(
            "slim-keybinds: config {:?} -> hide {:?} rename {:?} show_locked_hints {} max_hints {:?} show_vitals {}",
            configuration,
            self.hide,
            self.rename,
            self.show_locked_hints,
            self.max_hints,
            self.show_vitals
        );
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            // Arrives once, early, and is the only reliable source of the keybind table.
            Event::InitialKeybinds(keybinds) => {
                self.cached_keybinds = keybinds;
                if !self.cached_keybinds.is_empty() {
                    self.mode_info.keybinds = self.cached_keybinds.clone();
                }
                true
            },
            // ModeUpdate fires far more often than any of this changes, so re-render only on an
            // actual difference — otherwise the bar repaints constantly, which is visible
            // flicker over a slow link.
            Event::ModeUpdate(mut mode_info) => {
                if mode_info.keybinds.is_empty() {
                    mode_info.keybinds = self.cached_keybinds.clone();
                } else {
                    self.cached_keybinds = mode_info.keybinds.clone();
                }
                let changed = !self.have_mode || self.mode_info != mode_info;
                self.base_mode_is_locked = mode_info.base_mode == Some(InputMode::Locked);
                self.mode_info = mode_info;
                self.have_mode = true;
                changed
            },

            // The ONLY place the timer is re-armed, so the chain can never fork. No warm-up any
            // more: the probe carries both CPU samples itself, so the first result is already a
            // real reading and there is nothing to prime.
            Event::Timer(_) => {
                if !self.show_vitals {
                    return false;
                }
                set_timeout(VITALS_INTERVAL);
                vitals::request();
                false
            },

            // A grant can land after the first probe was already refused, so re-probe rather
            // than waiting out an interval with an empty cluster.
            Event::PermissionRequestResult(_) => {
                if self.show_vitals {
                    vitals::request();
                }
                false
            },

            Event::RunCommandResult(exit_code, stdout, _stderr, context) => {
                if context.get("source").map(|s| s.as_str()) != Some(vitals::CONTEXT_SOURCE) {
                    return false;
                }
                // A non-zero exit means a partial or missing probe; keeping the previous
                // numbers beats blanking the cluster on one bad tick.
                exit_code == Some(0) && self.vitals.parse(&stdout)
            },

            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        if !self.have_mode {
            return;
        }
        let colors = self.mode_info.style.colors;

        // Locked mode is the one place hints can be switched off wholesale — it is the mode you
        // are in when you want the terminal quiet. On the Unlock-First preset it is also the
        // BASE mode, so suppressing it there would leave no way to discover the unlock key;
        // hence the exception rather than a plain boolean.
        if self.mode_info.mode == InputMode::Locked
            && !self.show_locked_hints
            && !self.base_mode_is_locked
        {
            print!("{}", self.spacer(cols, &colors).part);
            return;
        }

        let hints = self.visible_hints();
        let upper = self.uppercase();

        let prefix = hints.prefix.as_ref().map(|p| self.draw_prefix(p, &colors));
        let blocks: Vec<Segment> = hints
            .hints
            .iter()
            .map(|hint| {
                let label = if upper {
                    hint.label.to_uppercase()
                } else {
                    hint.label.clone()
                };
                self.draw_hint(&hint.keys, &label, &colors)
            })
            .collect();

        // VITALS DROP FIRST. Measured against the hints at their FULL natural width, the same
        // widest-first rule `slim-tab-bar` uses for its clock: the cluster is only kept if
        // everything the bar is actually FOR still fits beside it. Hints always win — a narrow
        // terminal loses the vitals whole rather than losing a keybind the user needs.
        let natural: usize = prefix.as_ref().map(|p| p.len).unwrap_or(0)
            + blocks.iter().map(|b| b.len).sum::<usize>();
        let vitals = self
            .vitals_segment(&colors)
            .filter(|v| natural + v.len + 2 * EDGE_PAD <= cols);
        let budget = cols.saturating_sub(vitals.as_ref().map(|v| v.len).unwrap_or(0));

        let mut out = String::new();
        let mut used = EDGE_PAD;
        out.push_str(&self.spacer(EDGE_PAD, &colors).part);

        if let Some(seg) = &prefix {
            if used + seg.len + EDGE_PAD <= budget {
                used += seg.len;
                out.push_str(&seg.part);
            }
        }

        // Overflow is simply "stop when the next block does not fit". Hints are already in
        // priority order (the predicate list decides it), so dropping from the right drops the
        // least important ones — no truncation of individual labels, which would produce
        // unreadable stubs in a place where every word is load-bearing.
        for seg in &blocks {
            if used + seg.len + EDGE_PAD > budget {
                break;
            }
            used += seg.len;
            out.push_str(&seg.part);
        }

        // Flex spacer, then the right-aligned cluster and the trailing edge pad. Filling to the
        // end keeps the bar one continuous strip rather than text floating on the terminal
        // background.
        let tail = vitals.as_ref().map(|v| v.len).unwrap_or(0) + EDGE_PAD;
        out.push_str(&self.spacer(cols.saturating_sub(used + tail), &colors).part);
        if let Some(v) = vitals {
            out.push_str(&v.part);
        }
        out.push_str(&self.spacer(EDGE_PAD, &colors).part);
        print!("{}", out);
    }
}

register_plugin!(SlimKeybinds);
