//! fork addition: friendly focus - how long an agent's focus change waits for the person.
//!
//! An agent's call that moves a client's focus waits until that client's input has been idle for
//! `focus_wait_idle` ("2s" unset), and applies anyway after `focus_wait_max` ("5m" unset).
//! Either one at zero takes the old path: the change applies at once.

use crate::config_span::{parse_span, SpanBounds};
use std::time::Duration;

/// `focus_wait_idle` when the config does not say.
pub const DEFAULT_IDLE_MS: u64 = 2_000;
/// `focus_wait_max` when the config does not say.
pub const DEFAULT_MAX_MS: u64 = 5 * 60 * 1_000;

/// What `focus_wait_idle` takes: zero (no wait), or a span up to a minute.
pub const IDLE_BOUNDS: SpanBounds = SpanBounds {
    min: Duration::from_millis(1),
    max: Duration::from_secs(60),
    zero: true,
    whole_seconds: false,
};

/// What `focus_wait_max` takes: zero (no wait), or a span up to an hour.
pub const MAX_BOUNDS: SpanBounds = SpanBounds {
    min: Duration::from_millis(1),
    max: Duration::from_secs(60 * 60),
    zero: true,
    whole_seconds: false,
};

fn configured(value: Option<&str>, bounds: SpanBounds, default_ms: u64) -> u64 {
    value
        .and_then(|value| parse_span(value, bounds).ok())
        .map_or(default_ms, |span| span.duration.as_millis() as u64)
}

/// `focus_wait_idle` in milliseconds; [`DEFAULT_IDLE_MS`] when unset or not a span.
pub fn configured_idle_ms(value: Option<&str>) -> u64 {
    configured(value, IDLE_BOUNDS, DEFAULT_IDLE_MS)
}

/// `focus_wait_max` in milliseconds; [`DEFAULT_MAX_MS`] when unset or not a span.
pub fn configured_max_ms(value: Option<&str>) -> u64 {
    configured(value, MAX_BOUNDS, DEFAULT_MAX_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_and_bad_values_take_the_defaults_and_zero_is_kept() {
        assert_eq!(configured_idle_ms(None), DEFAULT_IDLE_MS);
        assert_eq!(configured_max_ms(Some("soon")), DEFAULT_MAX_MS);
        assert_eq!(configured_idle_ms(Some("0")), 0);
        assert_eq!(configured_max_ms(Some("0")), 0);
        assert_eq!(configured_idle_ms(Some("750ms")), 750);
        assert_eq!(configured_max_ms(Some("2h")), 60 * 60 * 1_000);
    }
}
