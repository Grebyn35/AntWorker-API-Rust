//! The forms of a pin that `check-pins` reads: an image's digest, an exact version, a runner's
//! versioned label, and a variable that Compose requires.

/// Whether `value` is one variable that Compose requires, `${NAME}` or `${NAME:?message}`, with
/// no default it could fall back to.
pub(super) fn is_required_variable(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
    else {
        return false;
    };
    let (name, rest) = inner.split_at(
        inner
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(inner.len()),
    );
    !name.is_empty() && (rest.is_empty() || rest.starts_with(":?") || rest.starts_with('?'))
}

/// Whether `image` names its digest: `@sha256:` and 64 hexadecimal digits.
pub(super) fn has_digest(image: &str) -> bool {
    image.rsplit_once("@sha256:").is_some_and(|(_, digest)| {
        digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// Whether `version` is three whole numbers, such as `1.99.1`.
pub(super) fn is_exact_version(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether `runner` is a hosted runner's versioned label, such as `ubuntu-24.04` or
/// `ubuntu-24.04-arm`.
pub(super) fn is_versioned_runner(runner: &str) -> bool {
    let runner = runner.strip_suffix("-arm").unwrap_or(runner);
    ["ubuntu-", "windows-", "macos-"].iter().any(|os| {
        runner.strip_prefix(os).is_some_and(|version| {
            !version.is_empty()
                && version
                    .split('.')
                    .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}
