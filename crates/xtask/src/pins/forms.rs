//! The forms of a pin that `check-pins` reads: an image's digest, an exact version, a runner's
//! versioned label, and a variable that the deploy sets to a digest.

/// The variables of the images that `write_deploy_env` in `deploy/tasks/apply.sh` writes into
/// `state/deploy.env`.
pub(super) const DEPLOY_IMAGES: [&str; 4] = [
    "API_IMAGE_BLUE",
    "API_IMAGE_GREEN",
    "API_IMAGE_NEW",
    "POSTGRES_IMAGE",
];

/// Whether `value` is one variable of [`DEPLOY_IMAGES`], `${NAME}`, `${NAME?message}` or
/// `${NAME:?message}`, with no default it could fall back to, whose first `}` is its last
/// character, which `check-pins` takes for a variable that the deploy sets to a digest.
pub(super) fn is_deploy_variable(value: &str) -> bool {
    let Some(inner) = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        .filter(|inner| !inner.contains('}'))
    else {
        return false;
    };
    let (name, rest) = inner.split_at(
        inner
            .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .unwrap_or(inner.len()),
    );
    DEPLOY_IMAGES.contains(&name)
        && (rest.is_empty() || rest.starts_with(":?") || rest.starts_with('?'))
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
