//! The committed OpenAPI document (API-17).

use crate::support::run_binary;

/// The document as committed at the repository's root.
const COMMITTED: &str = include_str!("../../../../openapi.json");

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the file the binary wrote"
)]
async fn api_17_the_committed_openapi_json_is_the_one_the_binary_writes() {
    let path = std::env::temp_dir().join(format!("openapi-{}.json", std::process::id()));

    let output = run_binary(&["openapi", path.to_str().unwrap()]).await;

    assert!(output.status.success(), "{output:?}");
    let written = std::fs::read_to_string(&path).unwrap();
    std::fs::remove_file(&path).unwrap();
    assert!(
        written == COMMITTED.replace("\r\n", "\n"),
        "openapi.json is out of date; run `just openapi`"
    );
}
