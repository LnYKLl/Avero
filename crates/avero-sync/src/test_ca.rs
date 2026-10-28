//! Test builds only (`--features test-ca`, never enabled by avero-app):
//! trusts one extra certificate authority, read from the PEM file named by
//! `AVERO_TEST_CA_PEM`, so the end-to-end tests can reach a local server
//! holding a throwaway certificate (see `docs/e2e-local.md`). Without the
//! variable nothing changes, and normal builds do not contain this code.

use lettre::transport::smtp::client::{Certificate, TlsParameters};
use tokio_rustls::rustls::RootCertStore;
use tokio_rustls::rustls::pki_types::CertificateDer;
use tokio_rustls::rustls::pki_types::pem::PemObject;

/// Environment variable naming the PEM file of the extra authority.
pub const ENV: &str = "AVERO_TEST_CA_PEM";

/// The PEM bytes of the extra authority, if the variable is set. Panics
/// when it names an unreadable file: a broken test setup, not a runtime case.
fn pem() -> Option<Vec<u8>> {
    let path = std::env::var_os(ENV)?;
    let bytes =
        std::fs::read(&path).unwrap_or_else(|e| panic!("{ENV} = {}: {e}", path.to_string_lossy()));
    Some(bytes)
}

/// Adds the extra authority to the IMAP root store.
pub fn add_to(roots: &mut RootCertStore) {
    let Some(pem) = pem() else { return };
    for cert in CertificateDer::pem_slice_iter(&pem) {
        let cert = cert.unwrap_or_else(|e| panic!("{ENV}: invalid PEM: {e}"));
        roots
            .add(cert)
            .unwrap_or_else(|e| panic!("{ENV}: unusable certificate: {e}"));
    }
}

/// SMTP TLS parameters trusting the system roots plus the extra
/// authority; `None` when the variable is not set.
pub fn smtp_parameters(host: &str) -> Option<TlsParameters> {
    let pem = pem()?;
    let cert = Certificate::from_pem(&pem).unwrap_or_else(|e| panic!("{ENV}: invalid PEM: {e}"));
    let parameters = TlsParameters::builder(host.to_string())
        .add_root_certificate(cert)
        .build()
        .unwrap_or_else(|e| panic!("{ENV}: {e}"));
    Some(parameters)
}
