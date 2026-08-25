//! System vitals: one host probe per tick, parsed into a right-corner cluster.
//!
//!     cpu 12% │ ram 7.0/23G │ disk 412/931G │ bat 87% │ fda ok
//!
//! The probe/parse design is lifted from `~/develop/zellij-claude-bar` (`request_vitals_refresh`,
//! `parse_vitals`), which learned these the hard way:
//!
//! - **`run_command` spawns with an EMPTY environment.** Without an explicit `PATH` the shell
//!   finds none of these tools and the probe silently returns nothing — no error, just no vitals.
//! - **BSD and GNU userland differ.** `pgrep` has no `-c` on BSD; `df` column positions differ
//!   unless you ask for POSIX output.
//! - **macOS `top -l 2`'s FIRST sample is a since-boot average**, so the second one is the only
//!   usable reading. That costs ~1s, which is fine for an async probe on a 10s cadence.
//!
//! Two divergences from claude-bar, both deliberate — see the notes on `probe_command` and
//! `parse` below.

use std::collections::BTreeMap;
use std::path::PathBuf;
use zellij_tile::prelude::*;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Marks the `RunCommandResult` that belongs to us, so a future probe can be told apart.
pub const CONTEXT_SOURCE: &str = "vitals";

#[derive(Default, PartialEq)]
pub struct Vitals {
    /// 0-100, normalized. Resolved from the first probe on both platforms: each probe carries its
    /// own pair of samples, so nothing has to be remembered between ticks.
    pub cpu_pct: Option<u8>,
    /// Preformatted `used/totalG`.
    pub mem: Option<String>,
    /// Preformatted `used/totalG`, matching `mem` — see the DISK branch for why it is not free
    /// space.
    pub disk: Option<String>,
    /// 0-100. `None` on anything without a battery, which is the common case here (VMs,
    /// desktops) — the segment is omitted entirely rather than rendered as a zero or a dash.
    pub battery_pct: Option<u8>,
    /// Seconds since boot. The shell does the arithmetic on both platforms so the plugin never
    /// needs a wall clock of its own — WASI's is UTC-only and this is a duration, not a time.
    pub uptime_secs: Option<u64>,
    /// Whether a pane of this session can open a Full-Disk-Access-gated file. macOS only.
    ///
    /// `None` is "not answered" and is deliberately NOT "denied": the question goes unanswered on
    /// every platform without the permission, and on macOS when the gated file is not there at
    /// all. Same three-way shape as the server's `full_disk_access_granted`, and the same rule —
    /// a probe that could not answer is never reported as a refusal.
    pub full_disk_access: Option<bool>,
}

/// `(busy, total)` jiffies from a `/proc/stat` aggregate `cpu` line, without its tag.
///
/// Cumulative since boot, so a single one of these means nothing on its own — a percentage is the
/// delta between two, and both come from the same probe.
fn parse_proc_stat_cpu(fields: &str) -> Option<(u64, u64)> {
    let nums: Vec<u64> = fields
        .split_whitespace()
        .skip_while(|s| s.starts_with("cpu"))
        .filter_map(|s| s.parse().ok())
        .collect();
    if nums.len() < 5 {
        return None;
    }
    let total: u64 = nums.iter().sum();
    let idle = nums[3] + nums[4]; // idle + iowait
    Some((total - idle, total))
}

/// Uptime at one significant unit: days once there is at least one, then hours, then minutes.
///
/// The brief specified days-or-hours; minutes are the floor because `up 0h` for the first hour
/// after a reboot is the one window where the number matters most and reads as broken.
fn duration(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else {
        format!("{}m", secs / 60)
    }
}

/// One `/bin/sh -c` probe, branching on `uname`.
///
/// **Every line is TAGGED by the shell.** claude-bar's parser tells `hw.memsize` from the process
/// count by "it's a bare number larger than 1e9", which works but is a heuristic sitting one
/// unusual machine away from being wrong. Emitting `MEMTOTAL <n>` costs nothing and makes the
/// parse total.
///
/// **`$HOME` is resolved via `~$(id -un)`, not `$HOME`.** The environment is empty, so `$HOME` is
/// the empty string and `df -Pk ""` fails. Tilde-with-username expansion reads the passwd
/// database directly and is POSIX, so it works on both platforms without a `getent`/`dscl` split.
///
/// **`kern.boottime` is split on commas before the `sed`.** Its output is
/// `{ sec = 1753900000, usec = 123456 } Wed Jul 30 …`, and the obvious
/// `sed 's/.*sec = \([0-9]*\).*/\1/'` is GREEDY — the leading `.*` runs forward to the `sec` inside
/// `usec`, so it captures the microseconds and reports an uptime of a few days no matter how long
/// the machine has been up. Splitting on `,` puts `{ sec = N` on its own line and `head -1` takes
/// it, which does not depend on the leading brace either. Caught by testing the `sed` against a
/// synthetic string; it would have been invisible on Linux.
///
/// **`df -Pk`, not `df -k`.** POSIX output mode guarantees one row per filesystem; plain `df`
/// wraps a long device name onto a second line, which shifts every column index by one and
/// silently misreports the numbers. Fields are then `1` = 1K-blocks total and `3` = available on
/// both Linux and macOS.
///
/// **The Full-Disk-Access question is a real `open`, and it is asked here rather than spawned on
/// its own.** `head -c 1` opens the file; `[ -r ]` would call `access(2)`, which reads the
/// permission bits while TCC refuses at `open(2)` instead — so a test on the bits answers
/// "readable" on a machine holding no grant at all. `[ -e ]` first, because TCC gates the open and
/// not the stat: a file that is not there is not a refusal and is reported apart. The same idiom
/// `zellij session doctor` runs in a pane (`session_doctor_macos.rs`), and one byte is all that is
/// read out of the file. Only the Darwin branch asks it — nowhere else has the permission to be
/// missing.
pub fn probe_command() -> String {
    "case \"$(uname)\" in \
       Darwin) \
         echo \"CPUMAC $(top -l 2 -n 0 -s 1 | grep 'CPU usage' | tail -1)\"; \
         echo \"MEMTOTAL $(sysctl -n hw.memsize)\"; \
         vm_stat; \
         echo \"BAT $(pmset -g batt | grep -Eo '[0-9]+%' | head -1)\"; \
         _db=\"$(eval echo ~\"$(id -un)\")/Library/Application Support/com.apple.TCC/TCC.db\"; \
         if [ ! -e \"$_db\" ]; then echo \"FDA unknown\"; \
         elif head -c 1 \"$_db\" >/dev/null 2>&1; then echo \"FDA yes\"; \
         else echo \"FDA no\"; fi; \
         _b=$(sysctl -n kern.boottime | tr ',' '\\n' | \
              sed -n 's/.*sec = \\([0-9]*\\).*/\\1/p' | head -1); \
         [ -n \"$_b\" ] && echo \"UP $(( $(date +%s) - _b ))\";; \
       *) \
         echo \"CPU1 $(head -1 /proc/stat)\"; sleep 1; echo \"CPU2 $(head -1 /proc/stat)\"; \
         grep -E '^(MemTotal|MemAvailable):' /proc/meminfo; \
         echo \"BAT $(cat /sys/class/power_supply/BAT*/capacity 2>/dev/null | head -1)\"; \
         echo \"UP $(cut -d' ' -f1 /proc/uptime)\";; \
     esac; \
     echo \"DISK $(df -Pk \"$(eval echo ~\"$(id -un)\")\" | tail -1)\""
        .to_string()
}

/// Fire the probe. Requires `RunCommands`.
pub fn request() {
    let ctx = BTreeMap::from([("source".to_string(), CONTEXT_SOURCE.to_string())]);
    // Without this PATH the shell finds nothing and the probe returns empty. This is the single
    // most expensive lesson in claude-bar's vitals work.
    let env = BTreeMap::from([(
        "PATH".to_string(),
        "/usr/bin:/bin:/usr/sbin:/sbin".to_string(),
    )]);
    run_command_with_env_variables_and_cwd(
        &["/bin/sh", "-c", &probe_command()],
        env,
        PathBuf::from("/tmp"),
        ctx,
    );
}

/// GiB with one decimal below 100, integer at or above it — so the wide numbers that dominate a
/// disk reading stay narrow and the small ones keep their resolution.
fn gib(v: f64) -> String {
    if v < 100.0 {
        format!("{:.1}", v)
    } else {
        format!("{:.0}", v)
    }
}

impl Vitals {
    /// Parse the probe's stdout. Returns whether anything displayed actually changed.
    pub fn parse(&mut self, stdout: &[u8]) -> bool {
        let text = String::from_utf8_lossy(stdout);
        let before = (
            self.cpu_pct,
            self.mem.clone(),
            self.disk.clone(),
            self.battery_pct,
            self.uptime_secs.map(duration),
            self.full_disk_access,
        );

        let mut memtotal_kb: Option<f64> = None;
        let mut memavail_kb: Option<f64> = None;
        let mut memsize_bytes: Option<f64> = None;
        let mut vm_page = 4096.0f64;
        let mut vm_used_pages = 0.0f64;
        let mut saw_vm_stat = false;
        // Sample one of the Linux CPU pair. Local, not a field: both samples arrive in the same
        // probe result, so nothing survives the call.
        let mut cpu_first: Option<(u64, u64)> = None;

        for raw in text.lines() {
            let line = raw.trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix("CPU1 ") {
                cpu_first = parse_proc_stat_cpu(rest);
            } else if let Some(rest) = line.strip_prefix("CPU2 ") {
                // Both samples come from ONE probe, a second apart, so the window is
                // self-contained. It used to be a delta against a sample held on the struct
                // across ticks, which looked cheaper and was wrong: this plugin is instantiated
                // per TAB, so every tab kept its own previous sample on its own timer phase and
                // computed a different window. Three tabs reported three different numbers for
                // one machine-wide quantity, and a window short enough to land on the probe's own
                // shell spawns read as 100%. Nothing is carried between ticks now, so every
                // instance measures the same kind of interval and they agree.
                //
                // This is what the macOS branch above always did (`top -l 2 -s 1`, take the
                // second) — the two platforms now work the same way.
                if let (Some((pb, pt)), Some((busy, total))) =
                    (cpu_first, parse_proc_stat_cpu(rest))
                {
                    let (db, dt) = (busy.saturating_sub(pb), total.saturating_sub(pt));
                    if dt > 0 {
                        self.cpu_pct = Some((db as f64 * 100.0 / dt as f64).round() as u8);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("CPUMAC ") {
                // macOS: "CPU usage: 4.16% user, 8.33% sys, 87.50% idle" — from the SECOND
                // `top` sample; the first is a since-boot average and is useless here.
                if let Some(idle) = rest.split(',').find_map(|p| {
                    p.trim()
                        .strip_suffix("idle")?
                        .trim()
                        .strip_suffix('%')?
                        .parse::<f64>()
                        .ok()
                }) {
                    self.cpu_pct = Some((100.0 - idle).clamp(0.0, 100.0).round() as u8);
                }
            } else if let Some(v) = line.strip_prefix("MemTotal:") {
                memtotal_kb = v.trim().trim_end_matches("kB").trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("MemAvailable:") {
                memavail_kb = v.trim().trim_end_matches("kB").trim().parse().ok();
            } else if let Some(v) = line.strip_prefix("MEMTOTAL ") {
                memsize_bytes = v.trim().parse().ok();
            } else if line.starts_with("Mach Virtual Memory Statistics") {
                saw_vm_stat = true;
                if let Some(n) = line
                    .split("page size of")
                    .nth(1)
                    .and_then(|s| s.split_whitespace().next())
                    .and_then(|s| s.parse().ok())
                {
                    vm_page = n;
                }
            } else if line.starts_with("Pages active:")
                || line.starts_with("Pages wired down:")
                || line.starts_with("Pages occupied by compressor:")
            {
                // active + wired + compressor is roughly the "used" figure Activity Monitor
                // shows, which is what a person comparing the two will expect.
                if let Some(n) = line
                    .rsplit(':')
                    .next()
                    .and_then(|s| s.trim().trim_end_matches('.').parse::<f64>().ok())
                {
                    vm_used_pages += n;
                }
            } else if let Some(rest) = line.strip_prefix("BAT ") {
                // Empty on anything without a battery — the probe always emits the tag, and an
                // empty value is the signal to omit the segment.
                self.battery_pct = rest.trim().trim_end_matches('%').parse().ok();
            } else if let Some(rest) = line.strip_prefix("FDA ") {
                // Darwin only, and re-read on every tick rather than remembered: an FDA toggle
                // takes effect on a live process, so a session refused at startup can be granted
                // while it runs. `unknown` — the gated file missing — clears the reading back to
                // "not answered" instead of falling through to a denial.
                self.full_disk_access = match rest.trim() {
                    "yes" => Some(true),
                    "no" => Some(false),
                    _ => None,
                };
            } else if let Some(rest) = line.strip_prefix("UP ") {
                // Linux gives a float ("1384059.35"), macOS an integer — parse as f64 and
                // truncate so one branch handles both.
                self.uptime_secs = rest.trim().parse::<f64>().ok().map(|s| s as u64);
            } else if let Some(rest) = line.strip_prefix("DISK ") {
                // Field 2 is USED, not `total - avail`: a filesystem reserves blocks for root, so
                // those three never add up and `total - avail` overstates what is actually in
                // use. Field 2 is the same number `df -h` prints.
                //
                // Used rather than available, because the segment beside it (`ram`) has always
                // been used/total, and two adjacent `x/yG` readings in one cluster meaning
                // opposite things is a trap for whoever reads the bar - including the person who
                // wrote it.
                let f: Vec<&str> = rest.split_whitespace().collect();
                if let (Some(total), Some(used)) = (
                    f.get(1).and_then(|s| s.parse::<f64>().ok()),
                    f.get(2).and_then(|s| s.parse::<f64>().ok()),
                ) {
                    self.disk = Some(format!(
                        "{}/{}G",
                        gib(used * 1024.0 / GIB),
                        gib(total * 1024.0 / GIB)
                    ));
                }
            }
        }

        // Ram keeps claude-bar's exact `{:.1}/{:.0}G` rather than `gib()`: a used figure wants
        // its decimal at any size, and total RAM is always a round-ish number.
        if let (Some(total), Some(avail)) = (memtotal_kb, memavail_kb) {
            self.mem = Some(format!(
                "{:.1}/{:.0}G",
                (total - avail) * 1024.0 / GIB,
                total * 1024.0 / GIB
            ));
        } else if let (true, Some(total)) = (saw_vm_stat, memsize_bytes) {
            self.mem = Some(format!(
                "{:.1}/{:.0}G",
                vm_used_pages * vm_page / GIB,
                total / GIB
            ));
        }

        // Compared on the FORMATTED uptime, not the raw seconds — those change every tick and
        // would force a repaint every 10s for a string that only changes once an hour.
        before
            != (
                self.cpu_pct,
                self.mem.clone(),
                self.disk.clone(),
                self.battery_pct,
                self.uptime_secs.map(duration),
                self.full_disk_access,
            )
    }

    /// `(label, value, alert)` triples in display order. Empty until the first probe lands, and
    /// each segment is skipped individually when its probe returned nothing — a machine with no
    /// battery shows no `bat`, rather than `bat ?`.
    ///
    /// `alert` marks a value the theme should paint as wrong rather than as a reading. Only the
    /// Full-Disk-Access segment ever sets it: the others are quantities, and a quantity being
    /// large is not the bar's business.
    pub fn segments(&self) -> Vec<(&'static str, String, bool)> {
        let mut out = Vec::new();
        if let Some(c) = self.cpu_pct {
            out.push(("cpu", format!("{}%", c), false));
        }
        if let Some(m) = &self.mem {
            out.push(("ram", m.clone(), false));
        }
        if let Some(d) = &self.disk {
            out.push(("disk", d.clone(), false));
        }
        if let Some(b) = self.battery_pct {
            out.push(("bat", format!("{}%", b), false));
        }
        if let Some(u) = self.uptime_secs {
            out.push(("up", duration(u), false));
        }
        // Last, and only when the probe actually answered. It is the odd segment out — a
        // permission rather than a quantity — and it is the one worth reading at the far edge of
        // the bar when it says `no`.
        if let Some(granted) = self.full_disk_access {
            out.push((
                "fda",
                if granted { "ok" } else { "no" }.to_string(),
                !granted,
            ));
        }
        out
    }
}
