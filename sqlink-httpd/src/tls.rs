//! TLS setup. Supports two modes:
//!
//!   --tls-self-signed             generate an rcgen cert valid for
//!                                  localhost + the bind address; key
//!                                  + cert live for the process lifetime
//!   --tls-cert PATH --tls-key PATH load PEM-encoded cert + private
//!                                  key from disk
//!
//! No client-cert auth in v1; that's the next-natural-feature when
//! someone needs mTLS.

use anyhow::{anyhow, bail, Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;

pub fn config_from_files(cert_path: &Path, key_path: &Path) -> Result<Arc<ServerConfig>> {
    let cert_bytes =
        fs::read(cert_path).with_context(|| format!("read tls cert {}", cert_path.display()))?;
    let key_bytes =
        fs::read(key_path).with_context(|| format!("read tls key {}", key_path.display()))?;

    let certs = parse_certs(&cert_bytes)?;
    let key = parse_key(&key_bytes)?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| anyhow!("rustls: {e}"))?;
    Ok(Arc::new(cfg))
}

pub fn config_self_signed(hostnames: Vec<String>) -> Result<Arc<ServerConfig>> {
    // rcgen 0.13: generate_simple_self_signed returns a Certificate
    // value that exposes serialize_pem / serialize_der + a key pair.
    let cert = rcgen::generate_simple_self_signed(hostnames).map_err(|e| anyhow!("rcgen: {e}"))?;
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
        .map_err(|e| anyhow!("self-signed key: {e}"))?;
    let cfg = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .map_err(|e| anyhow!("rustls: {e}"))?;
    Ok(Arc::new(cfg))
}

fn parse_certs(pem_bytes: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
    let mut reader = BufReader::new(pem_bytes);
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<std::result::Result<_, _>>()
        .map_err(|e| anyhow!("parse cert PEM: {e}"))?;
    if certs.is_empty() {
        bail!("no certificates found in PEM input");
    }
    Ok(certs)
}

fn parse_key(pem_bytes: &[u8]) -> Result<PrivateKeyDer<'static>> {
    let mut reader = BufReader::new(pem_bytes);
    let key = rustls_pemfile::private_key(&mut reader)
        .map_err(|e| anyhow!("parse key PEM: {e}"))?
        .ok_or_else(|| anyhow!("no private key found in PEM input"))?;
    Ok(key)
}
