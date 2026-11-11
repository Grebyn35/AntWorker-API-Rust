//! The settings that are numbers: whole numbers in a range, durations in whole seconds, and a
//! duration that another setting bounds (OPS-01).

use std::{ffi::OsString, io, ops::RangeInclusive, path::Path, time::Duration};

use super::{ConfigError, Settings, error::Problem};

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// A whole number from the setting `name`, or `default` when it is unset (OPS-01).
    pub fn number(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<u32, ConfigError> {
        let Some(value) = self.value(name)? else {
            return Ok(default);
        };
        value
            .parse()
            .ok()
            .filter(|number| range.contains(number))
            .ok_or_else(|| {
                ConfigError::new(name, Problem::OutOfRange(*range.start(), *range.end()))
            })
    }

    /// A duration in whole seconds from the setting `name`, or `default` seconds when it is
    /// unset.
    pub fn duration(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<Duration, ConfigError> {
        Ok(Duration::from_secs(
            self.number(name, default, range)?.into(),
        ))
    }

    /// A duration as [`Settings::duration`] reads it from the setting `name`, refused unless it is
    /// at most `limit`, the duration that the setting `limit_name` gives, so two settings that
    /// contradict each other fail at startup too (OPS-01).
    pub fn duration_at_most(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
        (limit_name, limit): (&'static str, Duration),
    ) -> Result<Duration, ConfigError> {
        let duration = self.duration(name, default, range)?;
        if duration > limit {
            return Err(ConfigError::new(name, Problem::AtMost(limit_name)));
        }
        Ok(duration)
    }
}
