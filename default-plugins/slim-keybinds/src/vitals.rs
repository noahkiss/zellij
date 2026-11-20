//! System vitals: one probing bar per session, one probe in flight at most, parsed into a
//! right-corner cluster.
//!
//!     cpu 12% │ ram 7.0/23G │ disk 412/931G │ bat 87% │ fda ok
//!
//! The probe/parse design comes from an earlier standalone status-bar plugin
//! (`request_vitals_refresh`, `parse_vitals`), which learned these the hard way:
//!
//! - **`run_command` spawns with an EMPTY environment.** Without an explicit `PATH` the shell
//!   finds none of these tools and the probe silently returns nothing — no error, just no vitals.
//! - **BSD and GNU userland differ.** `pgrep` has no `-c` on BSD; `df` column positions differ
//!   unless you ask for POSIX output.
//! - **A CPU reading needs two samples, and the first of a pair is a since-boot average.** Both
//!   platforms take the pair inside one probe, a second apart: macOS with `iostat -c 2 -w 1`, Linux
//!   with two reads of `/proc/stat`. That costs ~1s, which is fine for an async probe on a 10s
//!   cadence. macOS used `top -l 2`, which walks every process twice: about 0.4 s of CPU per probe,
//!   against none for `iostat`.
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
/// its own.** `ls` opens the system TCC directory, `/Library/Application Support/com.apple.TCC`,
/// which every Mac has and Full Disk Access has always gated. `[ -r ]` would call `access(2)`,
/// which reads the permission bits while TCC refuses at `open(2)` instead — so a test on the bits
/// answers "readable" on a machine holding no grant at all. `[ -d ]` first, because TCC gates the
/// open and not the stat: a directory that is not there is not a refusal and is reported apart.
/// The same idiom `zellij session doctor` runs in a pane (`session_doctor_macos.rs`), and nothing
/// listed is kept. The per-user `TCC.db` the probe opened before is gone on macOS 27. Only the
/// Darwin branch asks it — nowhere else has the permission to be missing.
pub fn probe_command() -> String {
    guarded(&probe_body(), PROBE_WINDOW_S)
}

/// How long a probe may run before it kills itself, in seconds. Under the ten-second interval, so
/// a probe never outlives the tick that would otherwise start the next one. A healthy probe takes
/// about a second on either platform.
pub const PROBE_WINDOW_S: u32 = 8;

/// `body`, run so that it cannot outlive `window_s` seconds, in portable `/bin/sh`.
///
/// macOS has no `timeout(1)`, and killing the shell alone would leave its children - the `top` a
/// slow machine is stuck in - running on as orphans. So the body runs as a background job, and a
/// watchdog job kills the body's whole process tree when the window closes: each process is
/// stopped first so it cannot start another, then its children are killed the same way
/// (`pgrep -P`, on both platforms), then the process itself. The watchdog writes nothing, so it
/// never holds the probe's output open, and it is killed the same way once the body ends in time.
/// A killed body exits non-zero, and the bar keeps its last numbers.
pub fn guarded(body: &str, window_s: u32) -> String {
    format!(
        "_k() {{ kill -STOP \"$1\" 2>/dev/null; \
           for _c in $(pgrep -P \"$1\"); do _k \"$_c\"; done; \
           kill -KILL \"$1\" 2>/dev/null; }}; \
         ( {body} ) & _b=$!; \
         ( sleep {window_s}; _k \"$_b\" ) >/dev/null 2>&1 & _w=$!; \
         wait \"$_b\"; _r=$?; \
         _k \"$_w\"; \
         exit $_r",
        body = body,
        window_s = window_s
    )
}

/// The probe itself, without its watchdog.
fn probe_body() -> String {
    "case \"$(uname)\" in \
       Darwin) \
         echo \"CPUIO $(iostat -n 0 -c 2 -w 1 | tail -1)\"; \
         echo \"MEMTOTAL $(sysctl -n hw.memsize)\"; \
         vm_stat; \
         echo \"BAT $(pmset -g batt | grep -Eo '[0-9]+%' | head -1)\"; \
         _tcc=\"/Library/Application Support/com.apple.TCC\"; \
         if [ ! -d \"$_tcc\" ]; then echo \"FDA unknown\"; \
         elif ls \"$_tcc\" >/dev/null 2>&1; then echo \"FDA yes\"; \
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

/// How many timer ticks a probe may stay unanswered before it counts as lost. Three intervals, so a
/// slow probe under load gets time to finish, and a result that never comes cannot stop the cluster
/// for good.
pub const LOST_AFTER_TICKS: u32 = 3;

/// The pipe a probing bar sends its probe's output on, to every other bar of the session.
pub const PIPE_NAME: &str = "slim-keybinds-vitals";

/// How many ticks a bar keeps deferring to a lower-numbered bar it last heard from. Two intervals,
/// so one late probe does not hand the job over, and a bar that went away (its tab closed, or
/// hidden) is replaced within twenty seconds.
pub const LEASE_TICKS: u32 = 2;

/// The argument of a [`PIPE_NAME`] message that names the sending bar: its [`bar_key`].
pub const FROM_ARG: &str = "from";

/// What orders the bars: the plugin id, then the client the instance serves. A plugin runs one
/// instance per attached client, all under one plugin id, so the id alone cannot tell them apart.
pub fn bar_key(plugin_id: u32, client_id: u32) -> u64 {
    ((plugin_id as u64) << 32) | client_id as u64
}

/// One bar per session probes; every other bar draws the numbers it sends.
///
/// Every tab has its own bar, and each ran its own probe. The bars pick one without a word about
/// tabs or clients: the bar with the lowest [`bar_key`] probes, and sends its output on
/// [`PIPE_NAME`] to every plugin of the session. A bar that hears from a lower key defers to it for
/// [`LEASE_TICKS`] ticks; one that stops hearing takes the probe up again. Output from any other
/// bar is drawn, whichever bar sent it.
#[derive(Default, Debug, PartialEq)]
pub struct Lease {
    /// The lower key this bar last heard from, and the ticks left before it stops deferring.
    deferring_to: Option<(u64, u32)>,
}

impl Lease {
    /// Another bar sent its output. `own` is this bar's [`bar_key`].
    pub fn heard(&mut self, from: u64, own: u64) {
        if from < own {
            let lowest = self
                .deferring_to
                .map_or(from, |(current, _)| current.min(from));
            self.deferring_to = Some((lowest, LEASE_TICKS));
        }
    }

    /// One timer tick passed.
    pub fn tick(&mut self) {
        self.deferring_to = match self.deferring_to {
            Some((id, left)) if left > 1 => Some((id, left - 1)),
            _ => None,
        };
    }

    /// Whether this bar should run the probe itself.
    pub fn probes(&self) -> bool {
        self.deferring_to.is_none()
    }
}

/// At most one probe in flight per bar.
///
/// The timer fires every interval whether or not the last probe has answered. Under load a probe
/// outlived the interval, the next one started beside it, and they piled up: `top -l 2` runs at
/// high CPU, so each new one made the others slower. This gate skips a tick while a probe is out.
#[derive(Default, Debug, PartialEq)]
pub struct ProbeGate {
    /// `Some(ticks skipped)` while a probe is out.
    in_flight: Option<u32>,
}

impl ProbeGate {
    /// Whether to start a probe now. A started probe is marked in flight. A probe out for
    /// [`LOST_AFTER_TICKS`] ticks is taken for lost, and a new one starts.
    pub fn start(&mut self) -> bool {
        match self.in_flight {
            Some(skipped) if skipped + 1 < LOST_AFTER_TICKS => {
                self.in_flight = Some(skipped + 1);
                false
            },
            _ => {
                self.in_flight = Some(0);
                true
            },
        }
    }

    /// The probe answered, well or badly. The next tick may start another.
    pub fn landed(&mut self) {
        self.in_flight = None;
    }

    /// Whether a probe is out. A tab switched to probes only when none is, so switching tabs
    /// cannot count down a slow probe's ticks.
    pub fn is_out(&self) -> bool {
        self.in_flight.is_some()
    }
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
                // The macOS branch below does the same: `iostat -c 2 -w 1`, the second line.
                if let (Some((pb, pt)), Some((busy, total))) =
                    (cpu_first, parse_proc_stat_cpu(rest))
                {
                    let (db, dt) = (busy.saturating_sub(pb), total.saturating_sub(pt));
                    if dt > 0 {
                        self.cpu_pct = Some((db as f64 * 100.0 / dt as f64).round() as u8);
                    }
                }
            } else if let Some(rest) = line.strip_prefix("CPUIO ") {
                // macOS: the SECOND line of `iostat -n 0 -c 2 -w 1`, "us sy id 1m 5m 15m", e.g.
                // "29 12 59  2.47 1.98 1.61". The first line is a since-boot average.
                if let Some(idle) = rest
                    .split_whitespace()
                    .nth(2)
                    .and_then(|v| v.parse::<f64>().ok())
                {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_macos_cpu_reading_is_the_busy_share_of_the_second_iostat_line() {
        let mut vitals = Vitals::default();
        assert!(vitals.parse(b"CPUIO  29 12 59  24.74 9.82 6.47\n"));
        assert_eq!(vitals.cpu_pct, Some(41));
        // a missing or short line keeps the last reading
        assert!(!vitals.parse(b"CPUIO \n"));
        assert_eq!(vitals.cpu_pct, Some(41));
    }

    #[test]
    fn a_probe_in_flight_skips_the_tick() {
        let mut gate = ProbeGate::default();
        assert!(gate.start());
        assert!(!gate.start());
        gate.landed();
        assert!(gate.start());
    }

    #[test]
    fn a_lost_probe_is_given_up_after_a_few_ticks() {
        let mut gate = ProbeGate::default();
        assert!(gate.start());
        for _ in 1..LOST_AFTER_TICKS {
            assert!(!gate.start());
        }
        assert!(gate.start(), "taken for lost");
        assert!(!gate.start(), "the new probe is in flight");
    }

    #[test]
    fn the_lowest_bar_probes_and_the_others_defer_while_they_hear_from_it() {
        let mut lease = Lease::default();
        assert!(lease.probes(), "a bar that has heard nothing probes");
        // a higher id is not deferred to
        lease.heard(9, 5);
        assert!(lease.probes());
        // its own key is not a lower one
        lease.heard(5, 5);
        assert!(lease.probes());
        lease.heard(3, 5);
        assert!(!lease.probes());
        // heard again every tick: still deferring
        for _ in 0..5 {
            lease.tick();
            lease.heard(3, 5);
            assert!(!lease.probes());
        }
        // the lower bar went quiet: this one takes over after the lease
        for _ in 0..LEASE_TICKS {
            assert!(!lease.probes());
            lease.tick();
        }
        assert!(lease.probes());
        // two instances of one plugin, one per client, are told apart by the client
        assert!(bar_key(2, 1) < bar_key(2, 3));
        assert!(bar_key(2, u32::MAX) < bar_key(3, 0));
    }

    /// The watchdog, run for real: a body that hangs is killed with its children when the window
    /// closes, and a body that finishes in time is not held up by it.
    #[cfg(unix)]
    #[test]
    fn a_probe_that_outlives_its_window_kills_itself_and_its_children() {
        use std::process::Command;
        use std::time::{Duration, Instant};
        let marker = format!("{}", 7000 + std::process::id() % 1000);
        let hung = guarded(&format!("echo started; sleep {}; echo never", marker), 1);
        let start = Instant::now();
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(&hung)
            .output()
            .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "started\n");
        assert!(!out.status.success());
        let left = Command::new("pgrep")
            .args(["-f", &format!("sleep {}", marker)])
            .output()
            .unwrap();
        assert!(left.stdout.is_empty(), "left running: {:?}", left);

        let quick = guarded("echo done", 30);
        let start = Instant::now();
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(&quick)
            .output()
            .unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{:?}",
            start.elapsed()
        );
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done\n");
        assert!(out.status.success());
        // the probe keeps its own exit status
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(guarded("exit 3", 30))
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3));
    }

    #[test]
    fn a_probe_out_is_reported() {
        let mut gate = ProbeGate::default();
        assert!(!gate.is_out());
        gate.start();
        assert!(gate.is_out());
        gate.landed();
        assert!(!gate.is_out());
    }
}
