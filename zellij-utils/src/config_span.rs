//! fork addition: a span of time written in a config value - `"30s"`, `"500ms"`, `"15m"`, `"1h"` -
//! held to the bounds its key allows.
//!
//! The words are [`crate::prompt::parse_duration`]'s, the parser `--deadline` uses, so the config
//! and the command line read a span the same way. What this adds is the bounds: a value under the
//! least a key allows is refused, and the caller keeps its default; a value over the most is cut
//! down to it, and said.

use std::time::Duration;

/// What one key allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpanBounds {
    /// The least span taken, zero aside. Anything shorter is refused.
    pub min: Duration,
    /// The most. Anything longer is cut down to it.
    pub max: Duration,
    /// Whether zero is a value of its own (`"0"`, `"0s"`). Without it zero is refused.
    pub zero: bool,
    /// Whether the span must be a whole number of seconds, for a reader that takes only those.
    pub whole_seconds: bool,
}

/// A span read from the config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub duration: Duration,
    /// The value was longer than the key allows and was cut down to [`SpanBounds::max`].
    pub capped: bool,
}

/// `value` as a span within `bounds`, or why it is not one.
pub fn parse_span(value: &str, bounds: SpanBounds) -> Result<Span, String> {
    let trimmed = value.trim();
    // `parse_duration` panics on a number no `Duration` holds; twelve digits of hours is past
    // every bound here and well inside one
    if trimmed.chars().filter(|c| c.is_ascii_digit()).count() > 12 {
        return Ok(Span {
            duration: bounds.max,
            capped: true,
        });
    }
    let duration = crate::prompt::parse_duration(trimmed)?;
    if duration.is_zero() {
        return if bounds.zero {
            Ok(Span {
                duration,
                capped: false,
            })
        } else {
            Err(format!(
                "{:?} is zero, which this setting does not take",
                trimmed
            ))
        };
    }
    if bounds.whole_seconds && duration.subsec_nanos() != 0 {
        return Err(format!("{:?} is not a whole number of seconds", trimmed));
    }
    if duration < bounds.min {
        return Err(format!(
            "{:?} is shorter than the least this setting takes, {}",
            trimmed,
            format_span(bounds.min)
        ));
    }
    if duration > bounds.max {
        return Ok(Span {
            duration: bounds.max,
            capped: true,
        });
    }
    Ok(Span {
        duration,
        capped: false,
    })
}

/// A span as the config writes it: whole seconds as `"90s"`, anything finer as `"1500ms"`.
pub fn format_span(duration: Duration) -> String {
    if duration.subsec_millis() == 0 {
        format!("{}s", duration.as_secs())
    } else {
        format!("{}ms", duration.as_millis())
    }
}

/// The config's own spelling of `value` once read: the value as written, or the bound it was cut
/// down to. What a parser keeps in `Options`, so a dump writes back what the build acts on.
pub fn canonical(value: &str, span: Span) -> String {
    if span.capped {
        format_span(span.duration)
    } else {
        value.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOUNDS: SpanBounds = SpanBounds {
        min: Duration::from_secs(5),
        max: Duration::from_secs(600),
        zero: false,
        whole_seconds: false,
    };

    #[test]
    fn a_span_inside_the_bounds_is_taken_as_written() {
        for (value, secs) in [("60s", 60), ("60", 60), ("2m", 120), (" 10m ", 600)] {
            assert_eq!(
                parse_span(value, BOUNDS).unwrap(),
                Span {
                    duration: Duration::from_secs(secs),
                    capped: false
                },
                "{}",
                value
            );
        }
        assert_eq!(canonical(" 2m ", parse_span("2m", BOUNDS).unwrap()), "2m");
    }

    #[test]
    fn too_short_and_zero_are_refused_and_too_long_is_capped() {
        assert!(parse_span("4s", BOUNDS).is_err());
        assert!(parse_span("0", BOUNDS).is_err());
        assert!(parse_span("soon", BOUNDS).is_err());
        let capped = parse_span("1h", BOUNDS).unwrap();
        assert_eq!(capped.duration, Duration::from_secs(600));
        assert!(capped.capped);
        assert_eq!(canonical("1h", capped), "600s");
        // a number no span holds is capped, never a panic
        assert!(parse_span("99999999999999999999h", BOUNDS).unwrap().capped);
    }

    #[test]
    fn zero_and_fractions_only_where_the_key_takes_them() {
        let zero_ok = SpanBounds {
            zero: true,
            ..BOUNDS
        };
        assert_eq!(parse_span("0s", zero_ok).unwrap().duration, Duration::ZERO);
        let whole = SpanBounds {
            min: Duration::from_secs(1),
            whole_seconds: true,
            ..BOUNDS
        };
        assert!(parse_span("1500ms", whole).is_err());
        assert!(parse_span("500ms", whole).is_err());
        assert_eq!(parse_span("2s", whole).unwrap().duration.as_secs(), 2);
        assert_eq!(format_span(Duration::from_millis(1500)), "1500ms");
        assert_eq!(format_span(Duration::from_secs(90)), "90s");
    }
}
