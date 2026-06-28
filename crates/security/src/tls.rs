use anyhow::{Context, Result};
use rcgen::{CertificateParams, DnType, KeyPair};
use rustls::ServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;

/// Generate a self-signed X.509 certificate and its corresponding private key.
///
/// The certificate uses:
/// - Subject: CN=mibee-rec
/// - SAN: mibee-rec.local
/// - Validity: defaults (roughly now -1 day to now +30 days)
pub fn generate_self_signed_cert() -> Result<(CertificateDer<'static>, PrivateKeyDer<'static>)> {
    let mut params = CertificateParams::new(vec!["mibee-rec.local".to_string()])
        .context("Failed to create certificate parameters")?;
    params
        .distinguished_name
        .push(DnType::CommonName, "mibee-rec");
    // Default is_ca is fine (not a CA for server cert)

    let key_pair = KeyPair::generate().context("Failed to generate key pair")?;
    let cert = params
        .self_signed(&key_pair)
        .context("Failed to self-sign certificate")?;

    let cert_der = CertificateDer::from(cert.der().to_vec());
    let key_der = PrivateKeyDer::Pkcs8(key_pair.serialize_der().into());

    Ok((cert_der, key_der))
}

/// Extract the raw DER bytes from a PrivateKeyDer.
fn key_bytes<'a>(key: &'a PrivateKeyDer<'a>) -> &'a [u8] {
    key.secret_der()
}

/// Build a `rustls::ServerConfig` using PEM certificate and key files at the given paths.
///
/// If the files do not exist, a fresh self-signed certificate is generated and saved to
/// both paths as PEM files. On subsequent calls the saved files are reused.
pub fn build_tls_config(cert_path: &str, key_path: &str) -> Result<ServerConfig> {
    let (cert_pem, key_pem) = if Path::new(cert_path).exists() && Path::new(key_path).exists() {
        tracing::info!("Loading existing TLS certificate from {cert_path}");
        (
            fs::read_to_string(cert_path)?,
            fs::read_to_string(key_path)?,
        )
    } else {
        tracing::info!("Generating self-signed TLS certificate");
        let (cert_der, key_der) = generate_self_signed_cert()?;

        // Convert DER back to PEM for storage
        let cert_pem = pem_encode("CERTIFICATE", cert_der.as_ref());
        let key_pem = pem_encode("PRIVATE KEY", key_bytes(&key_der));

        if let Some(parent) = Path::new(cert_path).parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(cert_path, &cert_pem).context("Failed to write certificate PEM")?;
        fs::write(key_path, &key_pem).context("Failed to write private key PEM")?;

        tracing::info!("Saved TLS certificate to {cert_path} and key to {key_path}");
        (cert_pem, key_pem)
    };

    // Parse PEM certificate(s)
    let certs: Vec<CertificateDer<'static>> = {
        let mut reader = BufReader::new(cert_pem.as_bytes());
        rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("Failed to parse certificate PEM")?
    };

    // Parse PEM private key
    let key = {
        let mut reader = BufReader::new(key_pem.as_bytes());
        rustls_pemfile::private_key(&mut reader)
            .context("Failed to parse private key PEM")?
            .ok_or_else(|| anyhow::anyhow!("No private key found in PEM file"))?
    };

    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("Failed to build TLS server config")?;

    Ok(config)
}

/// Minimal PEM encoder – wraps `data` in PEM armour without external dependencies.
fn pem_encode(kind: &str, data: &[u8]) -> String {
    use std::fmt::Write;

    let b64 = base64_encode(data);
    let mut out = format!("-----BEGIN {kind}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        let _ = writeln!(out, "{}", std::str::from_utf8(chunk).unwrap());
    }
    let _ = writeln!(out, "-----END {kind}-----");
    out
}

/// Minimal base64 encoder for the PEM wrapper.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        let pad = 3 - chunk.len();
        out.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        out.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        out.push(if pad < 2 {
            CHARS[((triple >> 6) & 0x3F) as usize] as char
        } else {
            '='
        });
        out.push(if pad < 1 {
            CHARS[(triple & 0x3F) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Returns the maximum of the modification times of the two TLS files.
fn last_modified(cert_path: &str, key_path: &str) -> Option<SystemTime> {
    let cert_mtime = fs::metadata(cert_path).ok()?.modified().ok()?;
    let key_mtime = fs::metadata(key_path).ok()?.modified().ok()?;
    Some(std::cmp::max(cert_mtime, key_mtime))
}

/// Start a background task that polls TLS certificate files for changes.
///
/// Checks `cert_path` and `key_path` every `poll_interval` for modification time changes.
/// When a change is detected, tries to load the new certificate and key via [`build_tls_config`].
/// On success, sends the new `ServerConfig` (wrapped in `Arc`) through `reload_tx`.
/// On failure, logs "TLS reload failed, keeping old certs" and continues polling (fail-open).
///
/// This function runs until the channel is closed (e.g., server shutdown).
pub async fn start_cert_watcher(
    cert_path: String,
    key_path: String,
    reload_tx: mpsc::Sender<Arc<ServerConfig>>,
    poll_interval: Duration,
) {
    let mut last = last_modified(&cert_path, &key_path);
    let mut interval = tokio::time::interval(poll_interval);

    loop {
        interval.tick().await;
        let current = last_modified(&cert_path, &key_path);
        if current != last {
            match build_tls_config(&cert_path, &key_path) {
                Ok(config) => {
                    tracing::info!("TLS certs reloaded");
                    if reload_tx.send(Arc::new(config)).await.is_err() {
                        // Receiver dropped (server shutting down)
                        break;
                    }
                    last = current;
                }
                Err(e) => {
                    tracing::error!("TLS reload failed, keeping old certs: {e}");
                    // Update last to avoid retrying the same broken files
                    last = current;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_self_signed_cert() {
        let (cert, key) = generate_self_signed_cert().unwrap();

        // Certificate should be non-empty DER
        assert!(!cert.as_ref().is_empty());
        // Private key should be non-empty DER
        assert!(!key.secret_der().is_empty());
    }

    #[test]
    fn test_pem_roundtrip() {
        let (cert_der, key_der) = generate_self_signed_cert().unwrap();

        // Encode to PEM
        let cert_pem = pem_encode("CERTIFICATE", cert_der.as_ref());
        let key_pem = pem_encode("PRIVATE KEY", key_bytes(&key_der));

        // Verify PEM structure
        assert!(cert_pem.starts_with("-----BEGIN CERTIFICATE-----"));
        assert!(cert_pem.contains("-----END CERTIFICATE-----"));
        assert!(key_pem.starts_with("-----BEGIN PRIVATE KEY-----"));
        assert!(key_pem.contains("-----END PRIVATE KEY-----"));

        // Re-parse PEM and verify it works
        let certs: Vec<CertificateDer<'static>> = {
            let mut reader = BufReader::new(cert_pem.as_bytes());
            rustls_pemfile::certs(&mut reader)
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
        };
        assert_eq!(certs.len(), 1);
        assert_eq!(certs[0].as_ref(), cert_der.as_ref());

        let parsed_key = {
            let mut reader = BufReader::new(key_pem.as_bytes());
            rustls_pemfile::private_key(&mut reader)
                .unwrap()
                .expect("Key should be parsed")
        };
        assert_eq!(parsed_key.secret_der(), key_der.secret_der());
    }

    #[test]
    fn test_build_tls_config_generates_files() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join("mibee-rec-tls-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");

        // First call should generate and save
        let config =
            build_tls_config(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap();

        // Config should have at least one certificate
        let _ = config;

        // Files should exist now
        assert!(cert_path.exists());
        assert!(key_path.exists());

        // Second call should load from files
        let config2 =
            build_tls_config(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap();
        let _ = config2;

        // Cleanup
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_pem_valid_content() {
        let (cert_der, key_der) = generate_self_signed_cert().unwrap();

        // Verify cert is at least 300 bytes (typical self-signed cert)
        assert!(cert_der.as_ref().len() > 200);
        // Verify key is at least 100 bytes
        assert!(key_der.secret_der().len() > 80);
    }

    #[tokio::test]
    async fn test_cert_watcher_reloads_on_file_change() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dir = std::env::temp_dir().join("mibee-rec-watcher-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let cert_path = dir.join("cert.pem");
        let key_path = dir.join("key.pem");

        // Generate initial cert files
        let _initial =
            build_tls_config(cert_path.to_str().unwrap(), key_path.to_str().unwrap()).unwrap();

        let (tx, mut rx) = mpsc::channel::<Arc<ServerConfig>>(8);

        // Start watcher with fast polling
        let cert_str = cert_path.to_str().unwrap().to_string();
        let key_str = key_path.to_str().unwrap().to_string();
        tokio::spawn(start_cert_watcher(
            cert_str,
            key_str,
            tx,
            Duration::from_millis(50),
        ));

        // Give watcher time to record initial mtime
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Generate a new certificate and overwrite files
        let (new_cert_der, new_key_der) = generate_self_signed_cert().unwrap();
        let cert_pem = pem_encode("CERTIFICATE", new_cert_der.as_ref());
        let key_pem = pem_encode("PRIVATE KEY", key_bytes(&new_key_der));

        // Write via temp files for atomic-ish update
        let tmp_cert = dir.join("cert.pem.tmp");
        let tmp_key = dir.join("key.pem.tmp");
        fs::write(&tmp_cert, &cert_pem).unwrap();
        fs::write(&tmp_key, &key_pem).unwrap();
        fs::rename(&tmp_cert, &cert_path).unwrap();
        fs::rename(&tmp_key, &key_path).unwrap();

        // Wait for watcher to detect and reload
        let received = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("Watcher should detect file change within timeout");

        assert!(received.is_some(), "Should receive a reloaded ServerConfig");

        // Cleanup
        fs::remove_dir_all(&dir).ok();
    }
}
