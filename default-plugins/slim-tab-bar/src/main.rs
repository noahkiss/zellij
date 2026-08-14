//! slim-tab-bar — a single-line Zellij tab bar that inherits the active theme.
//!
//! WHY THIS EXISTS
//!
//! zj-status-bar (cristiand391), a compact-bar fork, reads only zellij's LEGACY `Palette`
//! API (fg/bg/black/red/green/...) and exposes no configuration. Its active-tab colour is
//! therefore whatever `Palette.green` resolves to, and nothing in a modern theme's component
//! spec (`ribbon_selected` and friends) can reach it. Recolouring the active tab was
//! impossible without replacing the plugin.
//!
//! HOW THIS AVOIDS REPEATING THAT
//!
//! It holds no colour literals at all. Every colour is read at render time from
//! `ModeInfo.style.colors` — a `Styling`, the NEW component spec — so changing the theme
//! changes the bar with no rebuild and no colour config duplicated into the layout.
//!
//! WHY RAW ANSI RATHER THAN THE COMPONENT API
//!
//! In 0.44 exact widths and the component API are mutually exclusive. A ribbon is emitted as a
//! DCS escape that the zellij CLIENT expands, so a plugin can never measure what it drew, and
//! coordinate-positioned components do not combine with flow components on one line. Drawing
//! the ribbons ourselves means we know every segment's visible width, which is what the flex
//! layout needs:
//!
//!     [session] [tabs, natural width] ←flex spacer→ [right element]
//!
//! The drawing follows zellij's own `default-plugins/compact-bar` (`tab.rs`, `line.rs`) so the
//! result reads like the native bar: one space, label, one space, with the
//! line filled in `text_unselected.background`.

use chrono::{Timelike, Utc};
use chrono_tz::Tz;
use std::collections::BTreeMap;
use std::str::FromStr;
use unicode_width::UnicodeWidthStr;
use zellij_tile::prelude::*;

/// Spaces between the session name and the first tab.
const DEFAULT_SESSION_GAP: usize = 2;

/// Never truncate a tab name below this — past it the label stops being identifiable.
const MIN_LABEL: usize = 3;

/// 12-hour, no leading zero, e.g. `3:47 PM`. Matches what `zellij-claude-bar` shows one row
/// below; `%H:%M` gives 24-hour instead. Any chrono strftime string works.
const DEFAULT_CLOCK_FORMAT: &str = "%-I:%M %p";

/// `7/30` — month without a leading zero, day with one. Kept SEPARATE from `clock_format`
/// rather than folded into it, because the two degrade independently when the line fills up:
/// the date goes first, the time second. Setting `date_format ""` in the layout turns the date
/// off, which is why this needs no second `show_date` option.
const DEFAULT_DATE_FORMAT: &str = "%-m/%d";

/// One column of bar background at each end of the line, so nothing sits flush against the
/// terminal edge. Counted in the fit budget and skipped when hit-testing clicks.
const EDGE_PAD: usize = 1;

const RESET: &str = "\u{1b}[m";

/// A drawn segment plus its VISIBLE width. The escapes make `part.len()` meaningless, so the
/// width is tracked alongside — the whole reason for the raw-ANSI rewrite. Same shape as
/// zellij's own `LinePart` and `zellij-claude-bar`'s `(String, usize)` pairs.
struct Segment {
    part: String,
    len: usize,
}

/// Where a drawn tab ended up on the line, so a click can be mapped back to it.
///
/// Recorded during `render` rather than recomputed on click: the columns depend on the fitted
/// (possibly truncated) labels and the terminal width at draw time, and recomputing risks
/// disagreeing with what is actually on screen.
struct TabHit {
    /// Column range the tab occupies, `start..end`, excluding the gap that follows it.
    start: usize,
    end: usize,
    /// 0-based `TabInfo.position`.
    position: usize,
}

#[derive(Default)]
struct SlimBar {
    tabs: Vec<TabInfo>,
    session_name: Option<String>,
    /// Theme colours. `None` until the first `ModeUpdate`; nothing renders before then rather
    /// than guessing a colour.
    colors: Option<Styling>,
    /// Column ranges of the tabs as last drawn, for click hit-testing.
    tab_hits: Vec<TabHit>,
    /// `None` means UTC — the crate stays location-agnostic and the layout supplies the zone.
    timezone: Option<Tz>,
    clock_format: String,
    date_format: String,
    /// The clock as last formatted. Rendering reads this rather than calling the clock itself,
    /// so what is drawn can never disagree with what the timer compared against.
    clock: String,
    /// The date as last formatted; empty when the date is switched off. Held apart from
    /// `clock` so the render can drop one without the other.
    date: String,
    show_index: bool,
    show_session: bool,
    max_tab_len: Option<usize>,
    session_gap: usize,
    /// Display substitutions for session names, e.g. `mysession` -> `GFF`.
    session_aliases: Vec<(String, String)>,
    /// Session-wide conditions the server is reporting, as of the last `ModeUpdate`. Empty is the
    /// normal case and draws nothing at all.
    warnings: Vec<SessionWarning>,
}

/// The badge for a set of warnings, or an empty string when there are none.
///
/// One triangle however many conditions are live: two triangles side by side read as two separate
/// widgets rather than one thing that is wrong. Codes are space-separated in the order given, which
/// is the order the server produces and therefore stable between frames.
///
/// A free function rather than a method: it is the whole of the badge's logic and nothing about it
/// depends on the bar's state.
fn badge_text(warnings: &[SessionWarning]) -> String {
    if warnings.is_empty() {
        return String::new();
    }
    let codes: Vec<&str> = warnings.iter().map(|warning| warning.code()).collect();
    format!("⚠ {}", codes.join(" "))
}

/// `PaletteColor` as an SGR colour parameter. The enum has exactly two variants and both are
/// handled here; anything else is a compile error, which is the point.
fn sgr_color(color: PaletteColor, foreground: bool) -> String {
    let channel = if foreground { 38 } else { 48 };
    match color {
        PaletteColor::Rgb((r, g, b)) => format!("{};2;{};{};{}", channel, r, g, b),
        PaletteColor::EightBit(n) => format!("{};5;{}", channel, n),
    }
}

/// Wrap `text` in one fg/bg run. Resets after every run, as ansi_term does, so a segment can
/// never leak style into the next one.
fn paint(text: &str, fg: PaletteColor, bg: PaletteColor, bold: bool) -> String {
    format!(
        "\u{1b}[{}{};{}m{}{}",
        if bold { "1;" } else { "" },
        sgr_color(fg, true),
        sgr_color(bg, false),
        text,
        RESET
    )
}

/// Truncate to `max` columns, using an ellipsis when that actually saves room.
/// Operates on chars, not bytes — tab and session names are user-supplied and may be non-ASCII.
fn truncate(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Parse a KDL config value as a bool, with an explicit default.
/// Anything unrecognised falls back to the default rather than erroring: a status bar that
/// refuses to render because of a config typo is worse than one that ignores it.
fn parse_bool(v: Option<&String>, default: bool) -> bool {
    match v.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("true") | Some("yes") | Some("1") => true,
        Some("false") | Some("no") | Some("0") => false,
        _ => default,
    }
}

impl SlimBar {
    /// Columns a drawn tab occupies: the label plus one pad column either side.
    fn tab_width(&self, label: &str) -> usize {
        label.width() + 2
    }

    /// Draw one tab as a block in its ribbon colours: one pad column, the label, one more.
    ///
    /// No separators and no gap — tabs sit flush against each other, told apart by their
    /// background colours alone. `label + 2` is the same total as the old bar, so the width
    /// is unchanged from the gapped version; only where the two spare columns go has moved.
    fn draw_tab(&self, label: &str, active: bool, colors: &Styling) -> Segment {
        let ribbon = if active {
            colors.ribbon_selected
        } else {
            colors.ribbon_unselected
        };
        Segment {
            part: paint(
                &format!(" {} ", label),
                ribbon.base,
                ribbon.background,
                true,
            ),
            len: self.tab_width(label),
        }
    }

    /// Label for one tab, before width budgeting.
    ///
    /// Never returns an empty string. While a tab is being renamed its name is `""`, which
    /// would draw as a two-space block with no way to tell which tab it is; falling back to the
    /// index keeps it identifiable.
    fn tab_label(&self, tab: &TabInfo) -> String {
        let name = tab.name.trim();
        if name.is_empty() {
            return (tab.position + 1).to_string();
        }
        let mut name = name.to_string();
        if let Some(max) = self.max_tab_len {
            name = truncate(&name, max);
        }
        if self.show_index {
            format!("{} {}", tab.position + 1, name)
        } else {
            name
        }
    }

    /// Shrink tab labels until they fit `budget`.
    ///
    /// Truncates the longest label first and re-measures, rather than applying one cap to
    /// every tab: with a mix of short and long names, an even cap needlessly mangles the short
    /// ones. Bails at MIN_LABEL and accepts overflow rather than emitting useless stubs —
    /// zellij clips the line, which is a better failure than a row of single letters.
    fn fit(&self, mut labels: Vec<String>, budget: usize) -> Vec<String> {
        let width = |ls: &[String]| -> usize { ls.iter().map(|l| self.tab_width(l)).sum() };
        while width(&labels) > budget {
            let longest = labels
                .iter()
                .enumerate()
                .max_by_key(|(_, l)| l.chars().count())
                .map(|(i, _)| i);
            match longest {
                Some(i) if labels[i].chars().count() > MIN_LABEL => {
                    let n = labels[i].chars().count() - 1;
                    labels[i] = truncate(&labels[i], n);
                },
                _ => break,
            }
        }
        labels
    }

    /// Session name, if enabled and non-empty, drawn as plain text on the line background —
    /// no ribbon block behind it.
    fn session_segment(&self, colors: &Styling) -> Option<Segment> {
        if !self.show_session {
            return None;
        }
        let name = self
            .session_name
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())?;
        // Exact match only. Substring matching would surprise on sessions that merely share
        // a prefix, and this exists to shorten a handful of known names, not to be clever.
        let shown = self
            .session_aliases
            .iter()
            .find(|(from, _)| from == name)
            .map(|(_, to)| to.as_str())
            .unwrap_or(name);
        let text = format!("{}{}", shown, " ".repeat(self.session_gap));
        Some(Segment {
            len: text.width(),
            part: paint(
                &text,
                colors.text_unselected.base,
                colors.text_unselected.background,
                false,
            ),
        })
    }

    /// The clock, formatted now.
    ///
    /// WASI hands a plugin a correct UTC instant but no timezone, so the conversion is done
    /// here against chrono-tz's compiled-in IANA database. That is what makes this DST-correct
    /// without a fixed UTC offset to flip twice a year, and without shelling out to `date`.
    fn now_text(&self) -> String {
        self.format_now(&self.clock_format)
    }

    /// Same instant through a different strftime string, for the date half.
    fn date_text(&self) -> String {
        if self.date_format.is_empty() {
            return String::new();
        }
        self.format_now(&self.date_format)
    }

    fn format_now(&self, fmt: &str) -> String {
        Utc::now()
            .with_timezone(&self.timezone.unwrap_or(Tz::UTC))
            .format(fmt)
            .to_string()
    }

    /// Seconds until the next minute boundary, plus enough slack to land just past it.
    ///
    /// A minutes-resolution clock only needs one render per minute; polling every second to
    /// discover the same string 59 times is what this avoids. Recomputed after every tick, so
    /// it self-corrects rather than accumulating drift.
    fn secs_to_next_minute() -> f64 {
        (60 - Utc::now().second()) as f64 + 0.5
    }

    /// Right-hand element: the warning badge, the date and the clock, in that order.
    ///
    /// This is the single place that decides what sits on the right. Everything else works in
    /// terms of the returned segment's width. The render calls this several times, widest form
    /// first, and takes the first one that fits.
    ///
    /// The badge sits to the LEFT of the clock rather than at the outer edge, so the clock keeps
    /// its column when a warning appears or clears — the clock is what gets read at a glance, and
    /// a badge that shoved it sideways twice a session would be worse than the badge is good.
    /// It is drawn in the palette's error colour and bold, but never blinks: it reports a fact
    /// that has been true for minutes and will stay true until someone acts on it.
    ///
    /// No padding spaces of its own: the flex spacer supplies the gap on the left and the
    /// line's edge pad supplies the single column on the right.
    fn right_segment(
        &self,
        colors: &Styling,
        with_date: bool,
        with_clock: bool,
    ) -> Option<Segment> {
        let badge = badge_text(&self.warnings);
        let clock = if with_clock { self.clock.as_str() } else { "" };
        let date = if with_date && with_clock {
            self.date.as_str()
        } else {
            ""
        };
        if badge.is_empty() && clock.is_empty() {
            return None;
        }
        // painted in two runs, because the badge and the time are different colours; the widths
        // add up because `paint` resets after each run
        let time: String = [date, clock]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        let mut part = String::new();
        let mut len = 0;
        if !badge.is_empty() {
            part.push_str(&paint(
                &badge,
                colors.exit_code_error.base,
                colors.text_unselected.background,
                true,
            ));
            len += badge.width();
        }
        if !time.is_empty() {
            let gap = if badge.is_empty() { "" } else { " " };
            let text = format!("{}{}", gap, time);
            part.push_str(&paint(
                &text,
                colors.text_unselected.base,
                colors.text_unselected.background,
                false,
            ));
            len += text.width();
        }
        Some(Segment { part, len })
    }

    /// 1-based index of the active tab, which is what `switch_tab_to` wants.
    fn active_tab(&self) -> Option<usize> {
        self.tabs.iter().position(|t| t.active).map(|p| p + 1)
    }

    /// Focus the tab under `col`, if the click landed on one and it isn't already active.
    ///
    /// Columns come from `tab_hits`, recorded by the last `render`. Clicks on the session
    /// name, the spacer or the right-hand element deliberately do nothing.
    fn click(&self, col: usize) {
        let Some(hit) = self.tab_hits.iter().find(|h| col >= h.start && col < h.end) else {
            return;
        };
        if Some(hit.position + 1) != self.active_tab() {
            switch_tab_to((hit.position + 1) as u32);
        }
    }

    /// Wheel over the bar moves one tab, matching zellij's own compact-bar: up is the next
    /// tab, down the previous, and both stop at the ends rather than wrapping.
    fn scroll(&self, up: bool) {
        let Some(active) = self.active_tab() else {
            return;
        };
        let target = if up {
            (active + 1).min(self.tabs.len())
        } else {
            active.saturating_sub(1).max(1)
        };
        if target != active {
            switch_tab_to(target as u32);
        }
    }

    /// Blank fill in the line background, so the bar is one continuous strip.
    fn spacer(&self, width: usize, colors: &Styling) -> Segment {
        let bg = colors.text_unselected.background;
        Segment {
            part: paint(&" ".repeat(width), bg, bg, false),
            len: width,
        }
    }
}

impl ZellijPlugin for SlimBar {
    fn load(&mut self, configuration: BTreeMap<String, String>) {
        // No `request_permission` here, and none of zellij's own bars has one either: a builtin
        // is checked against the binary it ships in, so every permission short-circuits to
        // Granted and a request would only raise a prompt in a pane nobody can focus to answer.
        // What this bar uses is still ReadApplicationState (TabUpdate, ModeUpdate) and
        // ChangeApplicationState (`switch_tab_to`, i.e. clicking a tab to focus it). No
        // RunCommands, no filesystem, no web.
        // A bar is not a pane you focus, and saying so is what makes click-to-focus work on
        // the FIRST click. Zellij routes a plain left press on a SELECTABLE pane that isn't
        // already focused to `MouseAction::FocusPane`, which moves focus and drops the event;
        // the plugin only ever sees the click once its own pane is the focused one, so every
        // click had to be paid for twice. For an unselectable pane the same path instead calls
        // `start_selection`, and `PluginPane::start_selection` is what forwards
        // `Event::Mouse(LeftClick)` to the plugin. Zellij's own compact-bar calls this in
        // `load` for exactly this reason. Needs no permission.
        set_selectable(false);
        // ModeUpdate carries the session name and the theme colours — everything this bar
        // needs that isn't a tab. Mouse is click-to-focus and the wheel.
        subscribe(&[
            EventType::TabUpdate,
            EventType::ModeUpdate,
            EventType::Mouse,
            EventType::Timer,
        ]);

        self.show_index = parse_bool(configuration.get("show_index"), false);
        self.show_session = parse_bool(configuration.get("show_session"), true);
        self.max_tab_len = configuration
            .get("max_tab_len")
            .and_then(|s| s.trim().parse::<usize>().ok())
            .filter(|n| *n >= MIN_LABEL);
        self.session_gap = configuration
            .get("session_gap")
            .and_then(|s| s.trim().parse::<usize>().ok())
            .unwrap_or(DEFAULT_SESSION_GAP);
        // IANA zone name, e.g. "America/New_York". Deliberately no default beyond UTC: baking
        // one machine's zone into the crate would be a literal about its author. An
        // unrecognised name falls back to UTC rather than erroring, like every option here.
        self.timezone = configuration
            .get("timezone")
            .and_then(|s| Tz::from_str(s.trim()).ok());
        // A chrono strftime string. Validated up front because chrono PANICS on an invalid
        // specifier at format time, and `panic = "abort"` would take the whole plugin down —
        // a typo in a layout should cost you the default format, not the bar.
        self.clock_format = configuration
            .get("clock_format")
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .filter(|s| chrono::format::StrftimeItems::new(s).parse().is_ok())
            .unwrap_or(DEFAULT_CLOCK_FORMAT)
            .to_string();
        // Same validation, and an explicit empty string is meaningful here: it switches the
        // date off without needing a second option for it.
        self.date_format = match configuration.get("date_format").map(|s| s.trim()) {
            Some("") => String::new(),
            Some(s) if chrono::format::StrftimeItems::new(s).parse().is_ok() => s.to_string(),
            _ => DEFAULT_DATE_FORMAT.to_string(),
        };
        if parse_bool(configuration.get("show_clock"), true) {
            self.clock = self.now_text();
            self.date = self.date_text();
            set_timeout(Self::secs_to_next_minute());
        }
        // Comma-separated `from=to` pairs, e.g. "mysession=GFF,some-other=SO".
        // Malformed entries are skipped rather than fatal, same as every other option here.
        self.session_aliases = configuration
            .get("session_alias")
            .map(|raw| {
                raw.split(',')
                    .filter_map(|pair| {
                        let (from, to) = pair.split_once('=')?;
                        let (from, to) = (from.trim(), to.trim());
                        (!from.is_empty() && !to.is_empty())
                            .then(|| (from.to_string(), to.to_string()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // One line per instance into zellij's log, because "the clock is in UTC" has exactly
        // two causes and they look identical on screen: the layout never passed `timezone`,
        // or it passed a name chrono-tz did not recognise. Printing both the raw config and
        // what it resolved to tells them apart without a rebuild. Zellij captures plugin
        // stderr; find it in `$TMPDIR/zellij-$UID/zellij-log/zellij.log`.
        eprintln!(
            "slim-tab-bar: config {:?} -> timezone {}",
            configuration,
            self.timezone
                .map(|tz| tz.name().to_string())
                .unwrap_or_else(|| "UTC (unset or unrecognised)".to_string())
        );
    }

    fn update(&mut self, event: Event) -> bool {
        match event {
            // Re-render only on an actual change. TabUpdate fires on pane focus moves too,
            // and repainting the bar on every one of those is visible flicker over mosh.
            Event::TabUpdate(tabs) => {
                let changed = self.tabs != tabs;
                self.tabs = tabs;
                changed
            },
            // Same reasoning: ModeUpdate fires far more often than any of this changes.
            Event::ModeUpdate(mode_info) => {
                let colors = Some(mode_info.style.colors);
                let changed = self.session_name != mode_info.session_name
                    || self.colors != colors
                    || self.warnings != mode_info.session_warnings;
                self.session_name = mode_info.session_name;
                self.colors = colors;
                // the server re-asks its session-wide questions on a slow timer and sends a
                // ModeUpdate when an answer moves, so the badge follows a live change — an FDA
                // grant or a `session restart` — without the bar probing anything itself
                self.warnings = mode_info.session_warnings;
                changed
            },
            // Re-arm unconditionally, but only repaint when the displayed string actually
            // changed — the general rule here, and it matters more for a timer than for
            // anything else, since this fires forever.
            Event::Timer(_) => {
                let (now, today) = (self.now_text(), self.date_text());
                let changed = self.clock != now || self.date != today;
                self.clock = now;
                self.date = today;
                set_timeout(Self::secs_to_next_minute());
                changed
            },
            // Never returns true: switching tabs produces its own TabUpdate, and repainting
            // here as well would draw the old active tab for one frame.
            Event::Mouse(mouse) => {
                match mouse {
                    Mouse::LeftClick(_, col) => self.click(col),
                    Mouse::ScrollUp(_) => self.scroll(true),
                    Mouse::ScrollDown(_) => self.scroll(false),
                    _ => {},
                }
                false
            },
            _ => false,
        }
    }

    fn render(&mut self, _rows: usize, cols: usize) {
        let Some(colors) = self.colors else { return };
        if self.tabs.is_empty() {
            return;
        }

        let session = self.session_segment(&colors);
        let session_len = session.as_ref().map(|s| s.len).unwrap_or(0);
        // The two edge pads and the session name are never given up; everything else competes
        // for what is left.
        let avail = cols.saturating_sub(2 * EDGE_PAD + session_len);

        // OVERFLOW ORDER, widest-first: drop the date, then the whole clock, and only then
        // start truncating tab names. Rationale: the tab list is what the bar is for, and the
        // time is one row below in the claude bar anyway. Measured against the tabs at their
        // NATURAL width, so a narrow terminal hides the clock rather than mangling labels to
        // keep it — which is what makes this adapt to tab count instead of a fixed breakpoint.
        //
        // The warning badge is in every form, so it outlives the clock: it is a few columns, it
        // is only ever there when something is wrong, and a bar too narrow to say so is exactly
        // the bar that would hide it forever.
        let natural: Vec<String> = self.tabs.iter().map(|t| self.tab_label(t)).collect();
        let natural_width: usize = natural.iter().map(|l| self.tab_width(l)).sum();
        let right = [(true, true), (false, true), (false, false)]
            .into_iter()
            .find_map(|(with_date, with_clock)| {
                self.right_segment(&colors, with_date, with_clock)
                    .filter(|r| natural_width + r.len <= avail)
            });

        let labels = self.fit(
            natural,
            avail.saturating_sub(right.as_ref().map(|r| r.len).unwrap_or(0)),
        );
        let tabs: Vec<Segment> = self
            .tabs
            .iter()
            .zip(&labels)
            .map(|(tab, label)| self.draw_tab(label, tab.active, &colors))
            .collect();

        // Record where each tab landed, for click hit-testing. The left edge pad, the session
        // name and its gap all offset the first tab; tabs are flush after that, so every column
        // from the first tab to the last belongs to exactly one of them.
        let mut col = EDGE_PAD + session_len;
        self.tab_hits = self
            .tabs
            .iter()
            .zip(&tabs)
            .map(|(tab, seg)| {
                let hit = TabHit {
                    start: col,
                    end: col + seg.len,
                    position: tab.position,
                };
                col += seg.len;
                hit
            })
            .collect();

        let used = col + EDGE_PAD + right.as_ref().map(|r| r.len).unwrap_or(0);

        let mut out = String::new();
        out.push_str(&self.spacer(EDGE_PAD, &colors).part);
        if let Some(s) = session {
            out.push_str(&s.part);
        }
        for tab in &tabs {
            out.push_str(&tab.part);
        }
        let remaining = cols.saturating_sub(used);
        if remaining > 0 {
            out.push_str(&self.spacer(remaining, &colors).part);
        }
        if let Some(r) = right {
            out.push_str(&r.part);
        }
        out.push_str(&self.spacer(EDGE_PAD, &colors).part);

        print!("{}", out);
    }
}

register_plugin!(SlimBar);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_warnings_draw_nothing_at_all() {
        // zero width, not a space: the badge must cost the tab list nothing on a healthy session
        assert_eq!(badge_text(&[]), "");
    }

    #[test]
    fn a_superseded_build_is_one_code() {
        assert_eq!(badge_text(&[SessionWarning::SupersededBuild]), "⚠ zj");
    }

    #[test]
    fn missing_full_disk_access_is_one_code() {
        assert_eq!(
            badge_text(&[SessionWarning::MissingFullDiskAccess]),
            "⚠ TCC"
        );
    }

    #[test]
    fn both_conditions_share_one_triangle() {
        assert_eq!(
            badge_text(&[
                SessionWarning::SupersededBuild,
                SessionWarning::MissingFullDiskAccess
            ]),
            "⚠ zj TCC"
        );
    }

    #[test]
    fn the_order_given_is_the_order_drawn() {
        // the server always produces the same order, and the badge must not re-sort it into a
        // different one - a badge whose codes swap between frames reads as a flicker
        assert_eq!(
            badge_text(&[
                SessionWarning::MissingFullDiskAccess,
                SessionWarning::SupersededBuild
            ]),
            "⚠ TCC zj"
        );
    }

    #[test]
    fn the_badge_stays_narrow() {
        // it shares a line with the tab list, and the render only drops it after the clock
        let both = badge_text(&[
            SessionWarning::SupersededBuild,
            SessionWarning::MissingFullDiskAccess,
        ]);
        assert!(
            both.width() <= 10,
            "the widest badge is still a badge: {} columns",
            both.width()
        );
    }

    fn bar_with(warnings: Vec<SessionWarning>, clock: &str, date: &str) -> SlimBar {
        SlimBar {
            warnings,
            clock: clock.to_string(),
            date: date.to_string(),
            ..Default::default()
        }
    }

    /// The visible text of a drawn segment, with the SGR escapes stripped.
    fn visible(segment: &Segment) -> String {
        let mut out = String::new();
        let mut in_escape = false;
        for character in segment.part.chars() {
            match character {
                '\u{1b}' => in_escape = true,
                'm' if in_escape => in_escape = false,
                _ if in_escape => {},
                _ => out.push(character),
            }
        }
        out
    }

    #[test]
    fn the_badge_sits_immediately_left_of_the_clock() {
        let colors = Styling::default();
        let bar = bar_with(vec![SessionWarning::SupersededBuild], "3:47 PM", "7/30");
        let segment = bar.right_segment(&colors, true, true).unwrap();
        assert_eq!(visible(&segment), "⚠ zj 7/30 3:47 PM");
        assert_eq!(segment.len, "⚠ zj 7/30 3:47 PM".width(), "width is tracked");
    }

    #[test]
    fn a_healthy_session_draws_the_clock_exactly_as_before() {
        let colors = Styling::default();
        let bar = bar_with(vec![], "3:47 PM", "7/30");
        let segment = bar.right_segment(&colors, true, true).unwrap();
        assert_eq!(visible(&segment), "7/30 3:47 PM");
        assert_eq!(segment.len, "7/30 3:47 PM".width());
    }

    #[test]
    fn the_badge_survives_the_clock_being_dropped() {
        // the narrowest right-hand form the render tries still says something is wrong
        let colors = Styling::default();
        let bar = bar_with(
            vec![SessionWarning::MissingFullDiskAccess],
            "3:47 PM",
            "7/30",
        );
        let segment = bar.right_segment(&colors, false, false).unwrap();
        assert_eq!(visible(&segment), "⚠ TCC");
    }

    #[test]
    fn nothing_to_say_and_no_clock_is_no_segment() {
        let colors = Styling::default();
        let bar = bar_with(vec![], "3:47 PM", "7/30");
        assert!(bar.right_segment(&colors, false, false).is_none());
    }
}
