//! fork addition: how long a call to another machine waits, from `remote_hosts { connect_timeout
//! fleet_timeout }`, and the bounds every reader derives from those two.
//!
//! ```kdl
//! remote_hosts {
//!     connect_timeout "5s"
//!     fleet_timeout "10s"
//!     host "boxa"
//! }
//! ```
//!
//! Three readers share them. The MCP server's ssh and `scope=fleet` overview, `send-message
//! --host`, and the session server, which bounds the `send-message --host` it runs for a notice.
//! Each bound sits above the one inside it, so a slower one never cuts off a faster one:
//! connect < `send-message --host` < the server's notice deadline, and connect < fleet.

use kdl::{KdlDocument, KdlNode, KdlValue};

use crate::config_span::{format_span, parse_span, SpanBounds};

/// ssh's `ConnectTimeout` when the block names none, in seconds.
pub const DEFAULT_CONNECT_TIMEOUT_S: u64 = 5;

/// How long `scope=fleet` waits for any one host when the block names none, in seconds.
pub const DEFAULT_FLEET_TIMEOUT_S: u64 = 10;

/// What either key takes: whole seconds (ssh takes no less), 1 s to 2 min. Zero is refused: ssh
/// reads `ConnectTimeout=0` as no timeout at all.
pub const TIMEOUT_BOUNDS: SpanBounds = SpanBounds {
    min: std::time::Duration::from_secs(1),
    max: std::time::Duration::from_secs(120),
    zero: false,
    whole_seconds: true,
};

/// What `send-message --host` gives the other machine past the connection: its own
/// `send-message`, which answers at once unless `--wait` asks it to wait.
const SEND_ON_HOST_PAST_CONNECT_S: u64 = 15;

/// What the session server's notice deadline gives past `send-message --host`'s own bound.
const NOTICE_PAST_SEND_S: u64 = 10;

/// The least `fleet_timeout` exceeds `connect_timeout` by: the remote `list-agents` itself.
const FLEET_PAST_CONNECT_S: u64 = 5;

/// The two keys, as every reader uses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RemoteTimeouts {
    pub connect_s: u64,
    pub fleet_s: u64,
}

impl Default for RemoteTimeouts {
    fn default() -> Self {
        RemoteTimeouts {
            connect_s: DEFAULT_CONNECT_TIMEOUT_S,
            fleet_s: DEFAULT_FLEET_TIMEOUT_S,
        }
    }
}

impl RemoteTimeouts {
    /// The timeouts from what the block set, and a warning when `fleet` had to be raised. An unset
    /// `fleet` follows a longer `connect` without a word; a set one that does not exceed it is
    /// raised to `connect` plus 5 s.
    pub fn new(connect_s: Option<u64>, fleet_s: Option<u64>) -> (Self, Option<String>) {
        let connect_s = connect_s.unwrap_or(DEFAULT_CONNECT_TIMEOUT_S);
        let least_fleet = connect_s + FLEET_PAST_CONNECT_S;
        let (fleet_s, warning) = match fleet_s {
            Some(fleet) if fleet <= connect_s => (
                least_fleet,
                Some(format!(
                    "remote_hosts: fleet_timeout {}s does not exceed connect_timeout {}s, so {}s is \
                     used",
                    fleet, connect_s, least_fleet
                )),
            ),
            Some(fleet) => (fleet, None),
            None => (DEFAULT_FLEET_TIMEOUT_S.max(least_fleet), None),
        };
        (RemoteTimeouts { connect_s, fleet_s }, warning)
    }

    /// How long `send-message --host` waits for the other machine, before any `--wait`: 20 s at
    /// the defaults.
    pub fn send_on_host_s(&self) -> u64 {
        self.connect_s + SEND_ON_HOST_PAST_CONNECT_S
    }

    /// How long the session server lets the `send-message --host` it runs for a notice take
    /// before it kills it: 30 s at the defaults.
    pub fn notice_deadline_s(&self) -> u64 {
        self.send_on_host_s() + NOTICE_PAST_SEND_S
    }
}

/// Whether a child of `remote_hosts` is one of the timeout keys, which [`from_block`] reads.
pub fn is_timeout_entry(name: &str) -> bool {
    matches!(name, "connect_timeout" | "fleet_timeout")
}

/// The timeouts out of the `remote_hosts` block's children, and a warning for each value it could
/// not take. A value that is no span, zero, or a fraction of a second keeps that key's default; one
/// past 2 minutes is cut to 2 minutes. `None` when the block sets neither key.
pub fn from_block(children: &KdlDocument) -> (Option<RemoteTimeouts>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut connect = None;
    let mut fleet = None;
    let mut named = false;
    for node in children.nodes() {
        let name = node.name().value();
        if !is_timeout_entry(name) {
            continue;
        }
        named = true;
        let seconds = timeout_from_node(name, node, &mut warnings);
        match name {
            "connect_timeout" => connect = seconds.or(connect),
            _ => fleet = seconds.or(fleet),
        }
    }
    if !named {
        return (None, warnings);
    }
    let (timeouts, raised) = RemoteTimeouts::new(connect, fleet);
    warnings.extend(raised);
    (Some(timeouts), warnings)
}

/// [`from_block`] over a whole config file's text. A file that does not parse sets nothing.
pub fn from_config_text(text: &str) -> (Option<RemoteTimeouts>, Vec<String>) {
    let Ok(document) = text.parse::<KdlDocument>() else {
        return (None, Vec::new());
    };
    match document
        .get("remote_hosts")
        .and_then(|block| block.children())
    {
        Some(children) => from_block(children),
        None => (None, Vec::new()),
    }
}

fn timeout_from_node(name: &str, node: &KdlNode, warnings: &mut Vec<String>) -> Option<u64> {
    let value = match node.entries().first().map(|entry| entry.value()) {
        Some(KdlValue::String(value)) => value.clone(),
        // a bare number is seconds, as ssh's own option is
        Some(KdlValue::Base10(n)) => format!("{}s", n),
        Some(other) => other.to_string(),
        None => String::new(),
    };
    match parse_span(&value, TIMEOUT_BOUNDS) {
        Ok(span) => {
            if span.capped {
                warnings.push(format!(
                    "remote_hosts: {} {:?} is longer than {}, which is used instead",
                    name,
                    value,
                    format_span(span.duration)
                ));
            }
            Some(span.duration.as_secs())
        },
        Err(why) => {
            warnings.push(format!(
                "remote_hosts: {} {:?} ignored, the default kept: {} (expected whole seconds from \
                 1s to 120s, such as \"5s\")",
                name, value, why
            ));
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(text: &str) -> (Option<RemoteTimeouts>, Vec<String>) {
        from_config_text(&format!(
            "remote_hosts {{\n{}\n    host \"boxa\"\n}}\n",
            text
        ))
    }

    #[test]
    fn the_defaults_are_the_old_constants() {
        let defaults = RemoteTimeouts::default();
        assert_eq!((defaults.connect_s, defaults.fleet_s), (5, 10));
        assert_eq!(defaults.send_on_host_s(), 20);
        assert_eq!(defaults.notice_deadline_s(), 30);
        assert_eq!(block(""), (None, Vec::new()));
        assert_eq!(
            from_config_text("default_shell \"zsh\""),
            (None, Vec::new())
        );
    }

    #[test]
    fn every_bound_stays_above_the_one_inside_it() {
        for connect in [1, 5, 30, 120] {
            for fleet in [None, Some(1), Some(10), Some(120)] {
                let (t, _) = RemoteTimeouts::new(Some(connect), fleet);
                assert!(t.fleet_s > t.connect_s, "{:?}", t);
                assert!(t.send_on_host_s() > t.connect_s, "{:?}", t);
                assert!(t.notice_deadline_s() > t.send_on_host_s(), "{:?}", t);
            }
        }
    }

    #[test]
    fn values_are_read_clamped_or_refused() {
        let (t, warnings) = block("connect_timeout \"8s\"\nfleet_timeout \"30s\"");
        assert_eq!(
            t,
            Some(RemoteTimeouts {
                connect_s: 8,
                fleet_s: 30
            })
        );
        assert!(warnings.is_empty(), "{:?}", warnings);
        // a longer connect raises an unset fleet without a word
        let (t, warnings) = block("connect_timeout 12");
        assert_eq!(t.unwrap().fleet_s, 17);
        assert!(warnings.is_empty(), "{:?}", warnings);
        // a set fleet that does not exceed connect is raised, and said
        let (t, warnings) = block("connect_timeout \"20s\"\nfleet_timeout \"10s\"");
        assert_eq!(t.unwrap().fleet_s, 25);
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
        // past two minutes is cut down, and said
        let (t, warnings) = block("connect_timeout \"10m\"");
        assert_eq!(t.unwrap().connect_s, 120);
        assert_eq!(warnings.len(), 1, "{:?}", warnings);
        // zero, a fraction and garbage keep the default
        for value in ["\"0\"", "\"0s\"", "\"500ms\"", "\"soon\"", "true"] {
            let (t, warnings) = block(&format!("connect_timeout {}", value));
            assert_eq!(t.unwrap().connect_s, 5, "{}", value);
            assert_eq!(warnings.len(), 1, "{} {:?}", value, warnings);
            assert!(warnings[0].contains("connect_timeout"), "{}", warnings[0]);
        }
    }
}
