//! The clock every model-facing timestamp is rendered with.
//!
//! The model has no other source of "now": both the `<msg t="…">` headers and
//! the `runtime-context` time line come from here, so they must agree. The
//! offset is fixed by config (`[clock].utc_offset`) rather than read from the
//! host: a gateway may run anywhere, and a stable zone is what §11.2 asks for —
//! DST transitions inside one conversation are more confusing than useful.

use anyhow::{Result, anyhow};
use time::macros::format_description;
use time::{OffsetDateTime, UtcOffset};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Clock {
    offset: UtcOffset,
}

impl Default for Clock {
    fn default() -> Self {
        Self::utc()
    }
}

impl Clock {
    /// UTC — the default when `[clock]` is absent.
    pub const fn utc() -> Self {
        Self {
            offset: UtcOffset::UTC,
        }
    }

    /// Parse a fixed offset: `UTC` / `Z`, `+08:00`, `-05:30`, `UTC+8`, `+0800`.
    pub fn parse(spec: &str) -> Result<Self> {
        let raw = spec.trim();
        if raw.is_empty() {
            return Err(anyhow!(
                "empty clock offset (expected `UTC` or a fixed offset like `+08:00`)"
            ));
        }

        let upper = raw.to_ascii_uppercase();
        if matches!(upper.as_str(), "UTC" | "GMT" | "Z") {
            return Ok(Self::utc());
        }

        let rest = upper
            .strip_prefix("UTC")
            .or_else(|| upper.strip_prefix("GMT"))
            .unwrap_or(&upper);

        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => {
                return Err(anyhow!(
                    "`{spec}` is not a UTC offset (expected `UTC` or something like `+08:00`)"
                ));
            }
        };
        let body = &rest[1..];
        let (hours, minutes) = match body.split_once(':') {
            Some((hours, minutes)) => (hours, minutes),
            None if body.len() == 4 => (&body[..2], &body[2..]),
            None => (body, "0"),
        };

        let hours: i32 = hours
            .parse()
            .map_err(|_| anyhow!("`{spec}` has a malformed hour part"))?;
        let minutes: i32 = minutes
            .parse()
            .map_err(|_| anyhow!("`{spec}` has a malformed minute part"))?;

        if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
            return Err(anyhow!(
                "`{spec}` is out of range (hours 0-23, minutes 0-59)"
            ));
        }

        let seconds = sign * (hours * 3600 + minutes * 60);
        let offset = UtcOffset::from_whole_seconds(seconds)
            .map_err(|error| anyhow!("`{spec}` is not a valid UTC offset: {error}"))?;

        Ok(Self { offset })
    }

    pub fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc().to_offset(self.offset)
    }

    /// The `runtime-context` time line body: `2026-10-17 18:11:03 (UTC+08:00)`.
    pub fn now_line(&self) -> String {
        let time = self
            .now()
            .format(format_description!(
                "[year]-[month]-[day] [hour]:[minute]:[second]"
            ))
            .unwrap_or_else(|_| "unknown".to_string());

        format!("{time} ({})", self.label())
    }

    /// A block header's short stamp, in this clock's zone.
    pub fn format_time(&self, millis: i64) -> String {
        let Ok(datetime) =
            OffsetDateTime::from_unix_timestamp_nanos(i128::from(millis) * 1_000_000)
        else {
            return "--:--:--".to_string();
        };

        datetime
            .to_offset(self.offset)
            .format(format_description!("[hour]:[minute]:[second]"))
            .unwrap_or_else(|_| "--:--:--".to_string())
    }

    /// `UTC`, `UTC+08:00`, `UTC-05:30`.
    pub fn label(&self) -> String {
        let minutes = self.offset.whole_minutes();

        if minutes == 0 {
            "UTC".to_string()
        } else {
            let sign = if minutes < 0 { '-' } else { '+' };
            let absolute = minutes.abs();
            format!("UTC{sign}{:02}:{:02}", absolute / 60, absolute % 60)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_parse_in_the_documented_forms() {
        assert_eq!(Clock::parse("UTC").unwrap().label(), "UTC");
        assert_eq!(Clock::parse("z").unwrap().label(), "UTC");
        assert_eq!(Clock::parse("+08:00").unwrap().label(), "UTC+08:00");
        assert_eq!(Clock::parse("UTC+8").unwrap().label(), "UTC+08:00");
        assert_eq!(Clock::parse("GMT-05:30").unwrap().label(), "UTC-05:30");
        assert_eq!(Clock::parse("+0800").unwrap().label(), "UTC+08:00");
    }

    #[test]
    fn malformed_offsets_are_rejected_with_the_input_in_the_message() {
        assert_eq!(
            Clock::parse("Asia/Shanghai").unwrap_err().to_string(),
            "`Asia/Shanghai` is not a UTC offset (expected `UTC` or something like `+08:00`)"
        );
        assert_eq!(
            Clock::parse("+8:99").unwrap_err().to_string(),
            "`+8:99` is out of range (hours 0-23, minutes 0-59)"
        );
        assert_eq!(
            Clock::parse("").unwrap_err().to_string(),
            "empty clock offset (expected `UTC` or a fixed offset like `+08:00`)"
        );
    }

    #[test]
    fn the_same_instant_renders_in_the_clock_zone() {
        // 2023-11-14T22:13:20Z
        let millis = 1_700_000_000_000;

        assert_eq!(Clock::utc().format_time(millis), "22:13:20");
        assert_eq!(
            Clock::parse("+08:00").unwrap().format_time(millis),
            "06:13:20"
        );
        assert_eq!(
            Clock::parse("-05:30").unwrap().format_time(millis),
            "16:43:20"
        );
    }

    #[test]
    fn an_out_of_range_timestamp_degrades_to_dashes() {
        assert_eq!(Clock::utc().format_time(i64::MIN), "--:--:--");
    }
}
