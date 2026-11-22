//! TLS for the clients the API builds itself: rustls with its ring provider (SEC-16), verifying a
//! server against the operating system's certificates, as a verified database connection does
//! (D38).

use std::sync::Arc;

// part: files
use std::path::Path;

use rustls::pki_types::{CertificateDer, pem::PemObject};
// end part: files

/// What building a client's TLS configuration fails with.
type Error = Box<dyn std::error::Error + Send + Sync>;

/// A client's TLS configuration. A client needs one even for an `http` endpoint, which trusts no
/// certificate since it uses none, so `verified` is `false` there. A verified one trusts the
/// operating system's certificates, or the files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name.
pub(crate) fn client_config(verified: bool) -> Result<rustls::ClientConfig, Error> {
    configured(system_roots(verified)?)
}

// part: files
/// A client's TLS configuration, as [`client_config`] gives it, that trusts the certificates of
/// the PEM file `also` too, if one is given, such as a private authority's.
pub(crate) fn client_config_trusting(
    verified: bool,
    also: Option<&Path>,
) -> Result<rustls::ClientConfig, Error> {
    let mut roots = system_roots(verified)?;
    if let Some(path) = also {
        for certificate in CertificateDer::pem_file_iter(path)? {
            roots.add(certificate?)?;
        }
    }
    configured(roots)
}
// end part: files

/// The operating system's certificates, when `verified`, or none.
fn system_roots(verified: bool) -> Result<rustls::RootCertStore, Error> {
    let mut roots = rustls::RootCertStore::empty();
    if verified {
        let found = rustls_native_certs::load_native_certs();
        if let Some(error) = found.errors.into_iter().next() {
            return Err(Box::new(error));
        }
        for certificate in found.certs {
            roots.add(certificate)?;
        }
    }
    Ok(roots)
}

/// The configuration of a client that trusts `roots`, through the ring provider.
fn configured(roots: rustls::RootCertStore) -> Result<rustls::ClientConfig, Error> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}
