//! `mirror`, which `just e2e` runs before the end-to-end tests: it fills the cache of Docker Hub
//! that Compose runs as `mirror` with every image the tests' servers pull from Docker Hub, for the
//! servers' platform, so no test reaches a registry outside (TEST-07). The servers pull those
//! images through the cache, and the API's and the deployment's Postgres from Compose's registry.
//! It reads the images from the reference deployment's Compose files, and the stand-in bucket's
//! from compose.yaml.

use std::{
    path::Path,
    process::{Command, ExitCode, Stdio},
};

use crate::{repo::Repo, stack};

/// The Compose files of the reference deployment, whose images a server pulls when it deploys.
const DEPLOYMENT: [&str; 2] = ["deploy/compose.yaml", "deploy/observability/compose.yaml"];
/// The services of compose.yaml that the tests run on a server.
const ON_SERVER: [&str; 1] = ["bucket"];

/// Fills the cache with the images the servers pull, for `platform`, such as linux/amd64.
pub fn run(platform: &str) -> ExitCode {
    let images = match Repo::load()
        .map_err(|error| error.to_string())
        .and_then(|repo| images(&repo))
    {
        Ok(images) => images,
        Err(error) => return fail(&format!("can't list the servers' images: {error}")),
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for image in &images {
        // crane fetches the image's index, and its manifest and layers for `platform`, through
        // the cache, which keeps them.
        let fetched = Command::new("docker")
            .args(["compose", "run", "--rm", "--no-TTY", "crane", "--insecure"])
            .args(["--platform", platform, "validate", "--remote"])
            .arg(format!("mirror:5000/{image}"))
            .current_dir(&root)
            .stdout(Stdio::null())
            .status();
        match fetched {
            Ok(status) if status.success() => report(&format!("mirror: {image}")),
            Ok(_) => return fail(&format!("can't fetch {image} through the mirror")),
            Err(error) => return fail(&format!("can't run docker compose: {error}")),
        }
    }
    ExitCode::SUCCESS
}

/// The images that the servers pull from Docker Hub, by their paths in the cache, or why they
/// can't be listed, such as an image of another registry, which no server could pull through it.
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
    if let Some(compose) = stack::yaml(repo, "compose.yaml", &mut findings) {
        for service in ON_SERVER {
            match compose["services"][service]["image"].as_str() {
                Some(image) => named.push(image.to_owned()),
                None => return Err(format!("compose.yaml has no image for {service}")),
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
            "the mirror caches Docker Hub's images pinned by digest, and {} isn't one",
            others.join(", ")
        ));
    }
    Ok(named.iter().filter_map(|image| cached(image)).collect())
}

/// The path in the cache of `image`, a reference to Docker Hub pinned by digest, such as
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
        Repo::of(&[
            ("deploy/compose.yaml", deployment),
            ("deploy/observability/compose.yaml", observability),
            ("compose.yaml", compose),
        ])
    }

    #[test]
    fn test_07_an_image_of_docker_hub_has_its_path_in_the_cache() {
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
    fn test_07_the_servers_images_are_the_deployment_s_and_the_bucket_s() {
        let repo = repo(
            &format!(
                "services:\n  caddy:\n    image: caddy:2.11.7@{DIGEST}\n  api-blue:\n    image: ${{API_IMAGE_BLUE:?the image}}\n  cosign:\n    extends: {{ file: tools/compose.yaml, service: cosign }}\n"
            ),
            &format!("services:\n  grafana:\n    image: grafana/grafana:13.2.4@{DIGEST}\n"),
            &format!(
                "services:\n  bucket:\n    image: chrislusf/seaweedfs:4.50@{DIGEST}\n  grype:\n    image: anchore/grype:v0.121.0@{DIGEST}\n"
            ),
        );

        assert_eq!(
            images(&repo),
            Ok(vec![
                format!("library/caddy@{DIGEST}"),
                format!("chrislusf/seaweedfs@{DIGEST}"),
                format!("grafana/grafana@{DIGEST}"),
            ])
        );
    }

    #[test]
    fn test_07_an_image_the_mirror_can_t_serve_is_refused() {
        let repo = repo(
            &format!("services:\n  caddy:\n    image: ghcr.io/caddy/caddy:2@{DIGEST}\n"),
            "services: {}\n",
            &format!("services:\n  bucket:\n    image: chrislusf/seaweedfs:4.50@{DIGEST}\n"),
        );

        let refused = images(&repo).unwrap_err();

        assert!(refused.contains("ghcr.io/caddy/caddy:2@"), "{refused}");
    }
}
