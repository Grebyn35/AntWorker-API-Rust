//! TLS for the clients the API builds itself: rustls with its ring provider (SEC-16), verifying a
//! server against the operating system's certificates, as a verified database connection does
//! (D38).

use std::{path::Path, sync::Arc};

use rustls::pki_types::{CertificateDer, pem::PemObject};

/// A client's TLS configuration. A client needs one even for an `http` endpoint, which trusts no
/// certificate since it uses none, so `verified` is `false` there. A verified one trusts the
/// operating system's certificates, or the files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name,
/// and the certificates of the PEM file `also`, if one is given, such as a private authority's.
pub(crate) fn client_config(
    verified: bool,
    also: Option<&Path>,
) -> Result<rustls::ClientConfig, Box<dyn std::error::Error + Send + Sync>> {
    let mut roots = rustls::RootCertStore::empty();
    if let Some(path) = also {
        for certificate in CertificateDer::pem_file_iter(path)? {
            roots.add(certificate?)?;
        }
    }
    if verified {
        let found = rustls_native_certs::load_native_certs();
        if let Some(error) = found.errors.into_iter().next() {
            return Err(Box::new(error));
        }
        for certificate in found.certs {
            roots.add(certificate)?;
        }
    }
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth())
}
