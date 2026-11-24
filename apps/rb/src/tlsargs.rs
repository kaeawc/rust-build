//! TLS flags shared by commands.

use anyhow::Context as _;
use clap::Args;
use std::path::PathBuf;

/// How a client trusts servers.
#[derive(Args, Clone, Debug, Default)]
pub struct ClientTls {
    /// Trust this CA (PEM) in addition to the built-in webpki roots.
    #[arg(long)]
    pub ca: Option<PathBuf>,
    /// Skip certificate verification (local development only).
    #[arg(short = 'k', long)]
    pub insecure: bool,
    /// Client certificate (PEM) for mutual TLS.
    #[arg(long, requires = "client_key")]
    pub client_cert: Option<PathBuf>,
    /// Client private key (PEM) for mutual TLS.
    #[arg(long, requires = "client_cert")]
    pub client_key: Option<PathBuf>,
}

impl ClientTls {
    pub fn options(&self) -> anyhow::Result<rb_tls::ClientOptions> {
        let trust = if self.insecure {
            rb_tls::Trust::Insecure
        } else if let Some(ca) = &self.ca {
            let pem = std::fs::read(ca).with_context(|| format!("reading {}", ca.display()))?;
            rb_tls::Trust::WebPkiPlus(rb_tls::certs_from_pem(&pem, &ca.display().to_string())?)
        } else {
            rb_tls::Trust::WebPki
        };
        let identity = match (&self.client_cert, &self.client_key) {
            (Some(c), Some(k)) => Some(rb_tls::Identity::from_pem_files(c, k)?),
            _ => None,
        };
        Ok(rb_tls::ClientOptions { trust, alpn: Vec::new(), identity })
    }
}

/// Where a server's certificate comes from.
#[derive(Args, Clone, Debug, Default)]
pub struct ServerTls {
    /// Certificate chain (PEM).
    #[arg(long, requires = "key")]
    pub cert: Option<PathBuf>,
    /// Private key (PEM).
    #[arg(long, requires = "cert")]
    pub key: Option<PathBuf>,
    /// Generate an in-memory self-signed certificate for these hosts (comma-separated).
    #[arg(long, value_delimiter = ',', num_args = 0..=1, default_missing_value = "localhost,127.0.0.1", conflicts_with = "cert")]
    pub self_signed: Option<Vec<String>>,
    /// Require client certificates signed by this CA (PEM): mutual TLS.
    #[arg(long)]
    pub client_ca: Option<PathBuf>,
}

impl ServerTls {
    pub fn enabled(&self) -> bool {
        self.cert.is_some() || self.self_signed.is_some()
    }

    /// Builds a server config, or `None` when no TLS flags were given.
    pub fn config(&self, alpn: &[&[u8]]) -> anyhow::Result<Option<std::sync::Arc<rb_tls::rustls::ServerConfig>>> {
        let identity = match (&self.cert, &self.key, &self.self_signed) {
            (Some(c), Some(k), _) => rb_tls::Identity::from_pem_files(c, k)?,
            (_, _, Some(hosts)) => {
                let hosts = if hosts.is_empty() { vec!["localhost".to_string()] } else { hosts.clone() };
                tracing::warn!(?hosts, "serving a throwaway self-signed certificate");
                rb_tls::self_signed(&hosts)?.identity
            }
            _ => return Ok(None),
        };
        let cfg = self.config_for(identity, alpn)?;
        Ok(Some(cfg))
    }

    /// A config for `identity` with this server's client-auth policy (`--client-ca`), so every
    /// certificate a server presents enforces the same mTLS rule.
    pub fn config_for(&self, identity: rb_tls::Identity, alpn: &[&[u8]]) -> anyhow::Result<std::sync::Arc<rb_tls::rustls::ServerConfig>> {
        Ok(match &self.client_ca {
            Some(ca) => {
                let pem = std::fs::read(ca).with_context(|| format!("reading {}", ca.display()))?;
                let cas = rb_tls::certs_from_pem(&pem, &ca.display().to_string())?;
                rb_tls::server_config_mtls(identity, &cas, alpn)?
            }
            None => rb_tls::server_config(identity, alpn)?,
        })
    }
}
