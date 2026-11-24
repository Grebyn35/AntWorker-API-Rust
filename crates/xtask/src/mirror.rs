//! `mirror`, which `just e2e` runs before the end-to-end tests: it fills the store of the
//! registry that Compose runs as `mirror` with every image the tests' servers pull from Docker
//! Hub, for the servers' platform, and with the attestations that the image's index lists for
//! that platform's image, such as its provenance, which a server's Docker pulls with the image.
//! The servers pull those images from `mirror`, which has no upstream, and the API's and the
//! deployment's Postgres from Compose's registry, so no test reaches a registry outside
//! (TEST-06). It reads the images from the reference deployment's Compose files, those of curl
//! and yq, the tools of a deploy's import into a managed Grafana, from deploy/tools/compose.yaml,
//! and the stand-ins' for the bucket and the mail server from compose.yaml.
//!
//! It fetches them through `mirror-fill`, a cache of Docker Hub on the same store, which it
//! starts for the fill and removes after it. The store holds what one list of images filled it
//! with (OPS-21): once filled, its file [`FILLED`] holds the platform and the list, as
//! `mirror-images` prints them. A run whose platform and list the file holds fetches nothing,
//! and starts no cache. Any other run deletes everything in the store, fills it, writes the
//! file, and restarts `mirror`, which keeps in memory what its store held.

use std::{
    io::Write as _,
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use serde_json::Value;

use crate::{repo::Repo, stack};

/// The Compose files of the reference deployment, whose images a server pulls when it deploys.
const DEPLOYMENT: &[&str] = &[
    "deploy/compose.yaml",
    "deploy/observability/compose.yaml",
    // part: files
    "deploy/scanner/compose.yaml",
    // end part: files
];
/// The services of other Compose files that the tests run on a server: the stand-ins for the
/// bucket and the mail server, and the tools of a deploy's import into a managed Grafana. The
/// tools that verify a release come from other registries, and a test's deploy doesn't verify one.
const ON_SERVER: [(&str, &[&str]); 2] = [
    ("compose.yaml", &["bucket", "mailpit"]),
    ("deploy/tools/compose.yaml", &["curl", "yq"]),
];

/// The file of the store that holds what filled it: the platform, then each image, as
/// [`filled`] writes them.
const FILLED: &str = "/var/lib/registry/filled";

/// The address, on Compose's network, of the cache of Docker Hub that fills the store.
const CACHE: &str = "mirror-fill:5000";

/// Prints the platform and the images that `run` fills the store with for `platform`, as the
/// store's file [`FILLED`] holds them once it has.
pub fn list(platform: &str) -> ExitCode {
    match Repo::load()
        .map_err(|error| error.to_string())
        .and_then(|repo| images(&repo))
    {
        Ok(images) => {
            report(filled(platform, &images).trim_end());
            ExitCode::SUCCESS
        }
        Err(error) => fail(&format!("can't list the servers' images: {error}")),
    }
}

/// What the store's file [`FILLED`] holds once `images` have filled the store for `platform`:
/// a line with the platform, then a line with each image.
fn filled(platform: &str, images: &[String]) -> String {
    std::iter::once(platform)
        .chain(images.iter().map(String::as_str))
        .fold(String::new(), |lines, line| lines + line + "\n")
}

/// Fills the store with the images the servers pull, for `platform`, such as linux/amd64,
/// unless they filled it before.
pub fn run(platform: &str) -> ExitCode {
    let images = match Repo::load()
        .map_err(|error| error.to_string())
        .and_then(|repo| images(&repo))
    {
        Ok(images) => images,
        Err(error) => return fail(&format!("can't list the servers' images: {error}")),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let filled = filled(platform, &images);
    match holds(&root, &filled) {
        Ok(true) => {
            report("mirror: the store holds the images, which filled it before");
            return ExitCode::SUCCESS;
        }
        Ok(false) => {}
        Err(error) => return fail(&error),
    }
    // The store as another list left it, or a fill that stopped, goes first; then the cache
    // fetches into it, and goes whether the fill ended well or not.
    let emptied = steps(
        &root,
        &[
            &[
                "exec",
                "--no-TTY",
                "mirror",
                "find",
                "/var/lib/registry",
                "-mindepth",
                "1",
                "-delete",
            ],
            &["up", "--detach", "--wait", "mirror-fill"],
        ],
    );
    let fetched = emptied.and_then(|()| fetch(&root, platform, &images));
    let removed = steps(&root, &[&["rm", "--stop", "--force", "mirror-fill"]]);
    let recorded = fetched.and(removed).and_then(|()| record(&root, &filled));
    // `mirror` read the store before the fill, so it starts again on what the store holds now.
    match recorded.and_then(|()| {
        steps(
            &root,
            &[
                &["restart", "mirror"],
                &["up", "--detach", "--wait", "mirror"],
            ],
        )
    }) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

/// `docker compose` with `arguments`, in the repository at `root`.
fn compose(root: &Path, arguments: &[&str]) -> Command {
    let mut compose = Command::new("docker");
    compose.arg("compose").args(arguments).current_dir(root);
    compose
}

/// Runs `docker compose` with each of `steps`, in order, and stops at the first that fails.
fn steps(root: &Path, steps: &[&[&str]]) -> Result<(), String> {
    for step in steps {
        let done = compose(root, step)
            .stdout(Stdio::null())
            .status()
            .map_err(|error| format!("can't run docker compose: {error}"))?;
        if !done.success() {
            return Err(format!("`docker compose {}` failed", step.join(" ")));
        }
    }
    Ok(())
}

/// Whether the store's file [`FILLED`] holds `filled`, read through `mirror`'s container.
fn holds(root: &Path, filled: &str) -> Result<bool, String> {
    let held = compose(root, &["exec", "--no-TTY", "mirror", "cat", FILLED])
        .stderr(Stdio::null())
        .output()
        .map_err(|error| format!("can't run docker compose: {error}"))?;
    Ok(held.status.success() && held.stdout == filled.as_bytes())
}

/// Writes `filled` to the store's file [`FILLED`], once the store holds every image.
fn record(root: &Path, filled: &str) -> Result<(), String> {
    let run = |error: std::io::Error| format!("can't run docker compose: {error}");
    let write = format!("cat > {FILLED}");
    let mut child = compose(
        root,
        &[
            "exec",
            "--no-TTY",
            "--interactive",
            "mirror",
            "sh",
            "-c",
            &write,
        ],
    )
    .stdin(Stdio::piped())
    .spawn()
    .map_err(run)?;
    if let Some(mut input) = child.stdin.take() {
        input.write_all(filled.as_bytes()).map_err(run)?;
    }
    if child.wait().map_err(run)?.success() {
        Ok(())
    } else {
        Err(format!("can't write {FILLED} in the mirror's store"))
    }
}

/// Fetches each of `images` through the cache, for `platform`, with its attestations.
fn fetch(root: &Path, platform: &str, images: &[String]) -> Result<(), String> {
    let run = |error: std::io::Error| format!("can't run docker compose: {error}");
    let crane = || {
        let mut crane = compose(root, &["run", "--rm", "--no-TTY", "crane", "--insecure"]);
        crane.stdin(Stdio::null());
        crane
    };
    for image in images {
        // crane fetches the image's index, and its manifest and layers for `platform`, through
        // the cache, which keeps them.
        let fetched = crane()
            .args(["--platform", platform, "validate", "--remote"])
            .arg(format!("{CACHE}/{image}"))
            .stdout(Stdio::null())
            .status()
            .map_err(run)?;
        if !fetched.success() {
            return Err(format!("can't fetch {image} through the cache"));
        }
        // The index, which the cache now holds, lists the attestations of the platform's image,
        // which crane left out, and which a server's Docker pulls with the image.
        let index = crane()
            .arg("manifest")
            .arg(format!("{CACHE}/{image}"))
            .stderr(Stdio::null())
            .output()
            .map_err(run)?;
        if !index.status.success() {
            return Err(format!("can't read the index of {image} from the cache"));
        }
        attest(
            &crane,
            image,
            &String::from_utf8_lossy(&index.stdout),
            platform,
        )?;
        report(&format!("mirror: {image}"));
    }
    Ok(())
}

/// Fetches through the cache the attestations that `index` lists for `image`'s image of
/// `platform`: the manifest of each, then the blobs that it names, since crane validates an
/// image's layers, which an attestation's statements aren't.
fn attest(
    crane: &dyn Fn() -> Command,
    image: &str,
    index: &str,
    platform: &str,
) -> Result<(), String> {
    let repository = image.split_once('@').map_or(image, |(name, _)| name);
    let run = |error: std::io::Error| format!("can't run docker compose: {error}");
    for attestation in attestations(index, platform) {
        let reference = format!("{CACHE}/{repository}@{attestation}");
        let manifest = crane()
            .args(["manifest", &reference])
            .stderr(Stdio::null())
            .output()
            .map_err(run)?;
        if !manifest.status.success() {
            return Err(format!(
                "can't fetch {reference}, an attestation of {image}"
            ));
        }
        for blob in blobs(&String::from_utf8_lossy(&manifest.stdout)) {
            let fetched = crane()
                .arg("blob")
                .arg(format!("{CACHE}/{repository}@{blob}"))
                .stdout(Stdio::null())
                .status()
                .map_err(run)?;
            if !fetched.success() {
                return Err(format!("can't fetch {blob}, a blob of {reference}"));
            }
        }
    }
    Ok(())
}

/// The digests of the blobs that `manifest`, an image manifest as JSON, names: its configuration
/// and its layers, which for an attestation are its statements.
fn blobs(manifest: &str) -> Vec<String> {
    let Ok(manifest) = serde_json::from_str::<Value>(manifest) else {
        return Vec::new();
    };
    std::iter::once(&manifest["config"])
        .chain(manifest["layers"].as_array().into_iter().flatten())
        .filter_map(|blob| blob["digest"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The digests of the attestations that `index`, an image index as JSON, lists for the image of
/// `platform`, such as linux/amd64: each manifest that the index annotates as an attestation of
/// that image's manifest. None for a manifest that is no index, or an index without them.
fn attestations(index: &str, platform: &str) -> Vec<String> {
    let Ok(index) = serde_json::from_str::<Value>(index) else {
        return Vec::new();
    };
    let manifests: Vec<&Value> = index["manifests"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    let mut wanted = platform.split('/');
    let (os, architecture) = (wanted.next(), wanted.next());
    let images: Vec<&str> = manifests
        .iter()
        .filter(|manifest| {
            manifest["platform"]["os"].as_str() == os
                && manifest["platform"]["architecture"].as_str() == architecture
        })
        .filter_map(|manifest| manifest["digest"].as_str())
        .collect();
    manifests
        .iter()
        .filter(|manifest| {
            let annotations = &manifest["annotations"];
            annotations["vnd.docker.reference.type"] == "attestation-manifest"
                && annotations["vnd.docker.reference.digest"]
                    .as_str()
                    .is_some_and(|image| images.contains(&image))
        })
        .filter_map(|manifest| manifest["digest"].as_str())
        .map(str::to_owned)
        .collect()
}

/// The images that the servers pull from Docker Hub, by their paths in the mirror, or why they
/// can't be listed, such as an image of another registry, which no server could pull from it.
pub fn images(repo: &Repo) -> Result<Vec<String>, String> {
    let mut findings = Vec::new();
    let mut named = Vec::new();
    for path in DEPLOYMENT {
        let Some(compose) = stack::yaml(repo, path, &mut findings) else {
            continue;
        };
        let services = compose["services"].as_object().into_iter().flatten();
        // An image from a variable is the API's or the deployment's Postgres, which the deploy
        // names by digest in Compose's registry.
        named.extend(
            services
                .filter_map(|(_, service)| service["image"].as_str())
                .filter(|image| !image.starts_with("${"))
                .map(str::to_owned),
        );
    }
    for (path, services) in ON_SERVER {
        let Some(compose) = stack::yaml(repo, path, &mut findings) else {
            continue;
        };
        for service in services {
            match compose["services"][service]["image"].as_str() {
                Some(image) => named.push(image.to_owned()),
                None => return Err(format!("{path} has no image for {service}")),
            }
        }
    }
    if let Some(finding) = findings.first() {
        return Err(finding.to_string());
    }
    named.sort_unstable();
    named.dedup();
    let others: Vec<&str> = named
        .iter()
        .filter(|image| cached(image).is_none())
        .map(String::as_str)
        .collect();
    if !others.is_empty() {
        return Err(format!(
            "the mirror holds Docker Hub's images pinned by digest, and {} isn't one",
            others.join(", ")
        ));
    }
    Ok(named.iter().filter_map(|image| cached(image)).collect())
}

/// The path in the mirror of `image`, a reference to Docker Hub pinned by digest, such as
/// `library/caddy@sha256:…` for `caddy:2.11.7@sha256:…`; none for any other image.
fn cached(image: &str) -> Option<String> {
    let (name, digest) = image.split_once('@')?;
    let name = match name.split_once('/') {
        Some((host, path)) if host.contains(['.', ':']) || host == "localhost" => {
            (host == "docker.io").then_some(path)?
        }
        _ => name,
    };
    let repository = name
        .split_once(':')
        .map_or(name, |(repository, _)| repository);
    if repository.contains('/') {
        Some(format!("{repository}@{digest}"))
    } else {
        Some(format!("library/{repository}@{digest}"))
    }
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the error is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "sha256:504a44d7d74bd2bc28a66afc4774912fc21223ca8a67734af1c54ab2f999b489";

    fn repo(deployment: &str, observability: &str, compose: &str) -> Repo {
        let tools = format!(
            "services:
  cosign:
    image: ghcr.io/sigstore/cosign/cosign:v3.1.4@{DIGEST}
  curl:
    image: curlimages/curl:8.23.0@{DIGEST}
  yq:
    image: mikefarah/yq:4.54.1@{DIGEST}
"
        );
        // part: files
        let scanner = format!(
            "services:
  scanner:
    image: clamav/clamav-debian:1.5.4_base@{DIGEST}
"
        );
        // end part: files
        Repo::of(&[
            ("deploy/compose.yaml", deployment),
            ("deploy/observability/compose.yaml", observability),
            // part: files
            ("deploy/scanner/compose.yaml", &scanner),
            // end part: files
            ("deploy/tools/compose.yaml", &tools),
            ("compose.yaml", compose),
        ])
    }

    #[test]
    fn test_06_an_image_of_docker_hub_has_its_path_in_the_mirror() {
        assert_eq!(
            cached(&format!("caddy:2.11.7@{DIGEST}")),
            Some(format!("library/caddy@{DIGEST}"))
        );
        assert_eq!(
            cached(&format!("grafana/grafana:13.2.4@{DIGEST}")),
            Some(format!("grafana/grafana@{DIGEST}"))
        );
        assert_eq!(
            cached(&format!("docker.io/prom/prometheus:v3@{DIGEST}")),
            Some(format!("prom/prometheus@{DIGEST}"))
        );
        assert_eq!(
            cached(&format!("ghcr.io/sigstore/cosign:v3@{DIGEST}")),
            None
        );
        assert_eq!(cached(&format!("registry:5000/image@{DIGEST}")), None);
        assert_eq!(cached("caddy:2.11.7"), None);
    }

    #[test]
    fn test_06_the_servers_images_are_the_deployment_s_the_import_s_and_the_stand_ins() {
        let repo = repo(
            &format!(
                "services:\n  caddy:\n    image: caddy:2.11.7@{DIGEST}\n  api-blue:\n    image: ${{API_IMAGE_BLUE:?the image}}\n  cosign:\n    extends: {{ file: tools/compose.yaml, service: cosign }}\n"
            ),
            &format!("services:\n  grafana:\n    image: grafana/grafana:13.2.4@{DIGEST}\n"),
            &format!(
                "services:\n  bucket:\n    image: chrislusf/seaweedfs:4.50@{DIGEST}\n  mailpit:\n    image: axllent/mailpit:v1.31.6@{DIGEST}\n  grype:\n    image: anchore/grype:v0.121.0@{DIGEST}\n"
            ),
        );

        assert_eq!(
            images(&repo),
            Ok(vec![
                format!("axllent/mailpit@{DIGEST}"),
                format!("library/caddy@{DIGEST}"),
                format!("chrislusf/seaweedfs@{DIGEST}"),
                // part: files
                format!("clamav/clamav-debian@{DIGEST}"),
                // end part: files
                format!("curlimages/curl@{DIGEST}"),
                format!("grafana/grafana@{DIGEST}"),
                format!("mikefarah/yq@{DIGEST}"),
            ])
        );
    }

    #[test]
    fn ops_21_the_store_s_record_names_the_platform_and_each_image_that_filled_it() {
        let images = [
            format!("library/caddy@{DIGEST}"),
            format!("grafana/loki@{DIGEST}"),
        ];

        let record = filled("linux/amd64", &images);

        assert_eq!(
            record,
            format!("linux/amd64\nlibrary/caddy@{DIGEST}\ngrafana/loki@{DIGEST}\n")
        );
        // Another platform's fill, or another list's, writes another record, so a run for
        // either empties the store.
        assert_ne!(record, filled("linux/arm64", &images));
        assert_ne!(record, filled("linux/amd64", &images[..1]));
    }

    #[test]
    fn test_06_the_attestations_to_fetch_are_those_of_the_platform_s_image() {
        let index = r#"{"manifests": [
            {"digest": "sha256:aa", "platform": {"architecture": "amd64", "os": "linux"}},
            {"digest": "sha256:bb", "platform": {"architecture": "arm64", "os": "linux"}},
            {"digest": "sha256:cc", "platform": {"architecture": "unknown", "os": "unknown"},
             "annotations": {"vnd.docker.reference.type": "attestation-manifest",
                             "vnd.docker.reference.digest": "sha256:aa"}},
            {"digest": "sha256:dd", "platform": {"architecture": "unknown", "os": "unknown"},
             "annotations": {"vnd.docker.reference.type": "attestation-manifest",
                             "vnd.docker.reference.digest": "sha256:bb"}}
        ]}"#;

        assert_eq!(attestations(index, "linux/amd64"), ["sha256:cc"]);
        assert_eq!(attestations(index, "linux/arm64"), ["sha256:dd"]);
        assert!(attestations(index, "linux/riscv64").is_empty());
        // A single image's manifest, which lists none.
        assert!(attestations(r#"{"layers": []}"#, "linux/amd64").is_empty());
    }

    #[test]
    fn test_06_an_attestation_s_blobs_are_its_configuration_and_its_statements() {
        let manifest = r#"{"config": {"digest": "sha256:aa"},
            "layers": [{"digest": "sha256:bb"}, {"digest": "sha256:cc"}]}"#;

        assert_eq!(blobs(manifest), ["sha256:aa", "sha256:bb", "sha256:cc"]);
    }

    #[test]
    fn test_06_an_image_the_mirror_can_t_serve_is_refused() {
        let repo = repo(
            &format!("services:\n  caddy:\n    image: ghcr.io/caddy/caddy:2@{DIGEST}\n"),
            "services: {}\n",
            &format!(
                "services:\n  bucket:\n    image: chrislusf/seaweedfs:4.50@{DIGEST}\n  mailpit:\n    image: axllent/mailpit:v1.31.6@{DIGEST}\n"
            ),
        );

        let refused = images(&repo).unwrap_err();

        assert!(refused.contains("ghcr.io/caddy/caddy:2@"), "{refused}");
    }
}
