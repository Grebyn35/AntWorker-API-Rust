//! The settings that are numbers: whole numbers in a range, and durations in whole seconds, and
//! the reading of each number that OPS-01 lists.

use std::{ffi::OsString, io, ops::RangeInclusive, path::Path, str::FromStr, time::Duration};

use super::{ConfigError, Settings, error::Problem};

/// The number that `text` writes in ASCII digits alone, with no sign and no leading zero, if it
/// is one that `T` holds (OPS-01).
pub(crate) fn whole_number<T: FromStr>(text: &str) -> Option<T> {
    let digits = text.bytes().all(|byte| byte.is_ascii_digit());
    let leading_zero = text.len() > 1 && text.starts_with('0');
    (digits && !leading_zero)
        .then(|| text.parse().ok())
        .flatten()
}

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// A whole number from the setting `name`, written as [`whole_number`] reads one, or
    /// `default` when it is unset (OPS-01).
    pub fn number(
        &self,
        name: &str,
        default: u32,
        range: RangeInclusive<u32>,
    ) -> Result<u32, ConfigError> {
        let Some(value) = self.value(name)? else {
            return Ok(default);
        };
        whole_number(&value)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    /// The names of Rust's primitive integer types.
    const INTEGERS: [&str; 12] = [
        "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize",
    ];

    /// What a line that parses text holds: `.parse`, `::parse` or `from_str`.
    const PARSES: [&str; 3] = [".parse", "::parse", "from_str"];

    /// The lines of each `.rs` file of `src/config/` but this one, and of `src/encryption.rs`,
    /// that hold one of `PARSES` and, as a word, a run of ASCII letters, digits and `_`, the name
    /// of one of Rust's primitive integer types, such as `value.parse::<i16>()`,
    /// `let port: u16 = port.parse()` or `u16::from_str(port)`, each as its path and its line's
    /// number. A parse whose integer type stands on another line, or whose type a generic
    /// function infers, as `Settings::required` infers its `T`, isn't one of them (OPS-01).
    #[expect(
        clippy::disallowed_methods,
        reason = "a test may block (CODE-05), and reads the sources of the settings"
    )]
    fn integer_parses() -> Vec<String> {
        let sources = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files: Vec<PathBuf> = std::fs::read_dir(sources.join("config"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.extension().is_some_and(|extension| extension == "rs")
                    && path.file_name().is_some_and(|name| name != "numbers.rs")
            })
            .collect();
        files.push(sources.join("encryption.rs"));
        let mut found = Vec::new();
        for path in files {
            let text = std::fs::read_to_string(&path).unwrap();
            for (index, line) in text.lines().enumerate() {
                let parses = PARSES.iter().any(|parse| line.contains(parse));
                let integer = line
                    .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .any(|word| INTEGERS.contains(&word));
                if parses && integer {
                    found.push(format!("{}:{}", path.display(), index + 1));
                }
            }
        }
        found
    }

    #[test]
    fn ops_01_no_reader_of_a_setting_but_whole_number_parses_an_integer() {
        let found = integer_parses();

        assert_eq!(found, Vec::<String>::new());
    }
}
