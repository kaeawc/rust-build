//! TLS configuration on rustls with the `ring` provider (no C toolchain beyond ring's, no
//! global provider install). Builds server configs (optionally requiring client certificates for
//! mutual TLS), client configs (webpki roots, a custom CA, or `insecure` for local dev), and
//! self-signed certificates for development.

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::server::WebPkiClientVerifier;
use rustls::{ClientConfig, DigitallySignedStruct, RootCertStore, ServerConfig, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use std::path::Path;
use std::sync::Arc;

pub use rustls;
pub use rustls_pki_types as pki;

pub const ALPN_H2: &[u8] = b"h2";
pub const ALPN_HTTP11: &[u8] = b"http/1.1";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("tls: {0}")]
    Rustls(#[from] rustls::Error),
    #[error("reading {path}: {source}")]
    Io { path: String, source: std::io::Error },
    #[error("pem: {0}")]
    Pem(String),
    #[error("certificate generation: {0}")]
    Generate(#[from] rcgen::Error),
    #[error("client verifier: {0}")]
    Verifier(#[from] rustls::server::VerifierBuilderError),
    #[error("no certificates in {0}")]
    NoCerts(String),
}

pub fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// A certificate chain and its private key.
pub struct Identity {
    pub certs: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Identity { certs: self.certs.clone(), key: self.key.clone_key() }
    }
}

impl Identity {
    /// Loads a PEM certificate chain and PEM private key (PKCS#8, PKCS#1 or SEC1).
    pub fn from_pem_files(cert: &Path, key: &Path) -> Result<Self, Error> {
        let cert_pem = read(cert)?;
        let key_pem = read(key)?;
        Self::from_pem(&cert_pem, &key_pem, &cert.display().to_string())
    }

    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8], label: &str) -> Result<Self, Error> {
        let certs = certs_from_pem(cert_pem, label)?;
        let key = PrivateKeyDer::from_pem_slice(key_pem).map_err(|e| Error::Pem(e.to_string()))?;
        Ok(Identity { certs, key })
    }
}

fn read(path: &Path) -> Result<Vec<u8>, Error> {
    std::fs::read(path).map_err(|source| Error::Io { path: path.display().to_string(), source })
}

pub fn certs_from_pem(pem: &[u8], label: &str) -> Result<Vec<CertificateDer<'static>>, Error> {
    let certs = CertificateDer::pem_slice_iter(pem).collect::<Result<Vec<_>, _>>().map_err(|e| Error::Pem(e.to_string()))?;
    if certs.is_empty() {
        return Err(Error::NoCerts(label.to_string()));
    }
    Ok(certs)
}

/// A self-signed development certificate, with its PEM forms for writing to disk.
pub struct SelfSigned {
    pub identity: Identity,
    pub cert_pem: String,
    pub key_pem: String,
}

/// Generates a self-signed certificate valid for `hosts` (DNS names and/or IP addresses).
pub fn self_signed(hosts: &[String]) -> Result<SelfSigned, Error> {
    let ck = rcgen::generate_simple_self_signed(hosts.to_vec())?;
    let cert_pem = ck.cert.pem();
    let key_pem = ck.signing_key.serialize_pem();
    let identity = Identity {
        certs: vec![ck.cert.der().clone()],
        key: PrivateKeyDer::try_from(ck.signing_key.serialize_der()).map_err(|e| Error::Pem(e.to_string()))?,
    };
    Ok(SelfSigned { identity, cert_pem, key_pem })
}

/// Server config advertising `alpn` (e.g. `[ALPN_H2, ALPN_HTTP11]`; empty for raw tunnels).
pub fn server_config(id: Identity, alpn: &[&[u8]]) -> Result<Arc<ServerConfig>, Error> {
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(id.certs, id.key)?;
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(Arc::new(cfg))
}

/// Server config that requires a client certificate signed by one of `client_cas` (mTLS).
pub fn server_config_mtls(id: Identity, client_cas: &[CertificateDer<'static>], alpn: &[&[u8]]) -> Result<Arc<ServerConfig>, Error> {
    let mut roots = RootCertStore::empty();
    for ca in client_cas {
        roots.add(ca.clone())?;
    }
    let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider()).build()?;
    let mut cfg = ServerConfig::builder_with_provider(provider())
        .with_safe_default_protocol_versions()?
        .with_client_cert_verifier(verifier)
        .with_single_cert(id.certs, id.key)?;
    cfg.alpn_protocols = alpn.iter().map(|p| p.to_vec()).collect();
    Ok(Arc::new(cfg))
}

/// How a client decides to trust servers.
#[derive(Clone, Default)]
pub enum Trust {
    /// Mozilla's root set (webpki-roots), compiled in.
    #[default]
    WebPki,
    /// Only these CAs (e.g. a self-signed dev cert).
    Custom(Vec<CertificateDer<'static>>),
    /// Both the webpki roots and these CAs.
    WebPkiPlus(Vec<CertificateDer<'static>>),
    /// Accept any certificate. Local development only.
    Insecure,
}

#[derive(Clone, Default)]
pub struct ClientOptions {
    pub trust: Trust,
    pub alpn: Vec<Vec<u8>>,
    /// Client certificate for mutual TLS.
    pub identity: Option<Identity>,
}

pub fn client_config(opts: ClientOptions) -> Result<Arc<ClientConfig>, Error> {
    let p = provider();
    let builder = ClientConfig::builder_with_provider(p.clone()).with_safe_default_protocol_versions()?;
    let builder = match opts.trust {
        Trust::Insecure => builder.dangerous().with_custom_certificate_verifier(Arc::new(NoVerify(p))),
        trust => {
            let mut roots = RootCertStore::empty();
            let extra = match trust {
                Trust::WebPki => Vec::new(),
                Trust::Custom(cas) => cas,
                Trust::WebPkiPlus(cas) => {
                    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                    cas
                }
                Trust::Insecure => unreachable!("handled above"),
            };
            if extra.is_empty() && roots.is_empty() {
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            }
            for ca in extra {
                roots.add(ca)?;
            }
            builder.with_root_certificates(roots)
        }
    };
    let mut cfg = match opts.identity {
        Some(id) => builder.with_client_auth_cert(id.certs, id.key)?,
        None => builder.with_no_client_auth(),
    };
    cfg.alpn_protocols = opts.alpn;
    Ok(Arc::new(cfg))
}

/// Parses a host into the name rustls verifies against (DNS name or IP).
pub fn server_name(host: &str) -> Result<ServerName<'static>, Error> {
    let host = host.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(host.to_string()).map_err(|e| Error::Pem(format!("invalid server name {host:?}: {e}")))
}

#[derive(Debug)]
struct NoVerify(Arc<CryptoProvider>);

impl ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_round_trips_through_pem_and_builds_configs() {
        let ss = self_signed(&["localhost".into(), "127.0.0.1".into()]).expect("generate");
        let id = Identity::from_pem(ss.cert_pem.as_bytes(), ss.key_pem.as_bytes(), "mem").expect("parse");
        assert_eq!(id.certs.len(), 1);
        let server = server_config(id.clone(), &[ALPN_H2, ALPN_HTTP11]).expect("server");
        assert_eq!(server.alpn_protocols, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
        server_config_mtls(id.clone(), &id.certs, &[]).expect("mtls server");
        client_config(ClientOptions { trust: Trust::Custom(id.certs.clone()), ..Default::default() }).expect("custom");
        client_config(ClientOptions::default()).expect("webpki");
        client_config(ClientOptions { trust: Trust::Insecure, identity: Some(id), ..Default::default() }).expect("insecure");
    }

    #[test]
    fn rejects_empty_pem_and_parses_server_names() {
        assert!(matches!(certs_from_pem(b"", "x"), Err(Error::NoCerts(_))));
        assert!(server_name("localhost").is_ok());
        assert!(server_name("[::1]").is_ok());
        assert!(server_name("127.0.0.1").is_ok());
    }
}
