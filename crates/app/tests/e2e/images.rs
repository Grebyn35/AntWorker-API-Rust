//! The images under test, as the registry that Compose runs holds them (OPS-14).

use std::path::Path;

use serde_json::Value;

use crate::docker::compose;

/// One image under test: the image for the platform `E2E_PLATFORM` names, of the index that
/// `just image` wrote to target/, as `just e2e` pushed it. A container runs it by its own digest,
/// not the index's: Docker's classic image store holds one platform under a digest, and refuses
/// to pull a second one under the same.
pub struct Pushed {
    name: String,
    digest: String,
    platform: String,
}

impl Pushed {
    /// The API's image.
    pub fn api() -> Self {
        Self::find("")
    }

    /// The reference deployment's Postgres image (D44).
    pub fn postgres() -> Self {
        Self::find("-postgres")
    }

    /// The image whose name, and whose layout in target/, end in `suffix`.
    fn find(suffix: &str) -> Self {
        let platform = std::env::var("E2E_PLATFORM").expect("E2E_PLATFORM; run `just e2e`");
        let name =
            std::env::var("E2E_IMAGE_NAME").expect("E2E_IMAGE_NAME; run `just e2e`") + suffix;
        let index = index_digest(&format!("image{suffix}"));
        let digest = compose(&[
            "run",
            "--rm",
            "--no-TTY",
            "crane",
            "digest",
            "--insecure",
            "--platform",
            &platform,
            &format!("registry:5000/{name}@{index}"),
        ]);
        Self {
            name,
            digest,
            platform,
        }
    }

    /// The image's reference from this machine, through the registry's published port.
    pub fn on_host(&self) -> String {
        let registry = compose(&["port", "registry", "5000"]);
        format!("{registry}/{}@{}", self.name, self.digest)
    }

    /// The image's reference on Compose's network, where the registry is `registry:5000`.
    pub fn in_network(&self) -> String {
        format!("registry:5000/{}@{}", self.name, self.digest)
    }

    /// The platform the image is for, such as linux/amd64.
    pub fn platform(&self) -> &str {
        &self.platform
    }
}

/// The digest of the index in target/`layout`.
#[expect(
    clippy::disallowed_methods,
    reason = "tests may block, each on a runtime of its own (CODE-05)"
)]
fn index_digest(layout: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(layout)
        .join("index.json");
    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(&path).expect("the index that `just image` writes to target"),
    )
    .expect("an OCI index");
    index["manifests"][0]["digest"]
        .as_str()
        .expect("the image's digest")
        .to_owned()
}
