//! Types shared by every module.

use std::fmt;

/// A value that is never printed (SEC-01). `Debug` shows a placeholder, and there is no
/// `Display` or serialization.
#[derive(Clone)]
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value`.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// The wrapped value, for the one place that uses it. Never log or display it.
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_01_a_secret_never_prints_its_value() {
        let secret = Secret::new("hunter2".to_owned());
        assert!(!format!("{secret:?}").contains("hunter2"));
        assert!(!format!("{secret:#?}").contains("hunter2"));
        assert_eq!(secret.expose(), "hunter2");
    }
}
