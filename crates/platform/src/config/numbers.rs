//! The settings that are numbers: whole numbers in a range, and durations in whole seconds
//! (OPS-01).

use std::{ffi::OsString, io, ops::RangeInclusive, path::Path, time::Duration};

use super::{ConfigError, Settings, error::Problem};

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// A whole number from the setting `name`, written in ASCII digits with no sign and no
    /// leading zero, or `default` when it is unset (OPS-01).
    pub fn number(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<u32, ConfigError> {
        let Some(value) = self.value(name)? else {
            return Ok(default);
        };
        let digits = value.bytes().all(|byte| byte.is_ascii_digit());
        let leading_zero = value.len() > 1 && value.starts_with('0');
        (digits && !leading_zero)
            .then(|| value.parse().ok())
            .flatten()
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
}
