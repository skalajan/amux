//! Self-signed TLS (RR-0022).
//!
//! Certificate + key persist under `~/.amux/tls/` so browsers that accepted
//! the cert once keep working across restarts. Regenerated automatically
//! when missing or unreadable.

use std::path::Path;

pub struct TlsMaterial {
    pub cert_pem: String,
    pub key_pem: String,
}

pub fn load_or_generate(dir: &Path) -> anyhow::Result<TlsMaterial> {
    let cert_path = dir.join("cert.pem");
    let key_path = dir.join("key.pem");
    if let (Ok(cert_pem), Ok(key_pem)) = (
        std::fs::read_to_string(&cert_path),
        std::fs::read_to_string(&key_path),
    ) {
        if !cert_pem.is_empty() && !key_pem.is_empty() {
            return Ok(TlsMaterial { cert_pem, key_pem });
        }
    }
    let mut params = rcgen::CertificateParams::new(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])?;
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "amux");
    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    let material = TlsMaterial {
        cert_pem: cert.pem(),
        key_pem: key_pair.serialize_pem(),
    };
    std::fs::create_dir_all(dir)?;
    std::fs::write(&cert_path, &material.cert_pem)?;
    std::fs::write(&key_path, &material.key_pem)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key_path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(material)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generates_then_reuses() {
        let dir = std::env::temp_dir().join(format!("amux-tls-test-{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        let m1 = load_or_generate(&dir).unwrap();
        assert!(m1.cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(m1.key_pem.contains("PRIVATE KEY"));
        let m2 = load_or_generate(&dir).unwrap();
        assert_eq!(m1.cert_pem, m2.cert_pem, "must reuse persisted cert");
        std::fs::remove_dir_all(&dir).ok();
    }
}

// ---------------------------------------------------------------------------
// SNI dual-cert serving (Tailscale parity with the Python server)
// ---------------------------------------------------------------------------

use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::sync::Arc;

/// SNI resolver: the REAL Tailscale Let's Encrypt cert for the tailnet
/// hostname, the self-signed fallback for localhost/IPs — byte-for-byte the
/// Python server's `_sni_cb` behavior (amux-server.py:77931), so
/// https://desktop.tail5ce8f5.ts.net:8824 carries a browser-trusted cert
/// and the service worker can register.
#[derive(Debug)]
pub struct SniCerts {
    pub fallback: Arc<CertifiedKey>,
    pub ts_hostname: Option<String>,
    pub ts_cert: Option<Arc<CertifiedKey>>,
}

impl ResolvesServerCert for SniCerts {
    fn resolve(&self, hello: ClientHello) -> Option<Arc<CertifiedKey>> {
        if let (Some(name), Some(ts), Some(cert)) =
            (hello.server_name(), &self.ts_hostname, &self.ts_cert)
        {
            if name.eq_ignore_ascii_case(ts) {
                return Some(cert.clone());
            }
        }
        Some(self.fallback.clone())
    }
}

fn load_certified_key(cert_pem: &str, key_pem: &str) -> anyhow::Result<CertifiedKey> {
    let certs: Vec<_> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<_, _>>()?;
    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())?
        .ok_or_else(|| anyhow::anyhow!("no private key in PEM"))?;
    let signing_key = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| anyhow::anyhow!("unsupported key type: {e}"))?;
    Ok(CertifiedKey::new(certs, signing_key))
}

/// Build the full rustls ServerConfig: self-signed fallback always; the
/// Tailscale cert layered in when `<host>.ts.net.crt/.key` exist in the TLS
/// dir (the same files `tailscale cert` writes and the Python server loads).
pub fn build_server_config(dir: &std::path::Path) -> anyhow::Result<rustls::ServerConfig> {
    let material = load_or_generate(dir)?;
    let fallback = Arc::new(load_certified_key(&material.cert_pem, &material.key_pem)?);

    let mut ts_hostname = None;
    let mut ts_cert = None;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if let Some(host) = name.strip_suffix(".crt") {
                if host.contains(".ts.net") {
                    let key_path = dir.join(format!("{host}.key"));
                    if let (Ok(c), Ok(k)) = (
                        std::fs::read_to_string(e.path()),
                        std::fs::read_to_string(&key_path),
                    ) {
                        match load_certified_key(&c, &k) {
                            Ok(ck) => {
                                tracing::info!(host, "tailscale cert loaded for SNI");
                                ts_hostname = Some(host.to_string());
                                ts_cert = Some(Arc::new(ck));
                            }
                            Err(err) => {
                                tracing::warn!(host, error = %err, "tailscale cert unusable — fallback only");
                            }
                        }
                    }
                }
            }
        }
    }

    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(SniCerts {
            fallback,
            ts_hostname,
            ts_cert,
        }));
    cfg.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// Plain-HTTP redirect on the TLS port (RR-0022's redirect requirement)
// ---------------------------------------------------------------------------

/// Build the 301 for a plain-HTTP request that arrived on the TLS port.
/// Without this, `http://host:8824/` receives raw TLS bytes and the browser
/// shows ERR_INVALID_HTTP_RESPONSE (Ethan hit exactly this). Pure so it is
/// testable: parses the request head for Host + path, falls back to the
/// listener's own address when absent.
pub fn http_redirect_response(head: &[u8], fallback_host: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(head);
    let mut lines = text.lines();
    let path = lines
        .next()
        .and_then(|req| req.split_whitespace().nth(1))
        .unwrap_or("/");
    let host = lines
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim(), v.trim())))
        .find(|(k, _)| k.eq_ignore_ascii_case("host"))
        .map(|(_, v)| v.to_string())
        .unwrap_or_else(|| fallback_host.to_string());
    let location = format!("https://{host}{path}");
    format!(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: {location}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    )
    .into_bytes()
}

#[cfg(test)]
mod redirect_tests {
    use super::*;

    #[test]
    fn redirects_preserve_host_and_path() {
        let head = b"GET /board?x=1 HTTP/1.1\r\nHost: desktop.tail5ce8f5.ts.net:8824\r\nAccept: */*\r\n\r\n";
        let resp = String::from_utf8(http_redirect_response(head, "fallback:1")).unwrap();
        assert!(resp.starts_with("HTTP/1.1 301"));
        assert!(resp.contains("Location: https://desktop.tail5ce8f5.ts.net:8824/board?x=1"));
    }

    #[test]
    fn missing_host_uses_fallback() {
        let resp = String::from_utf8(http_redirect_response(b"GET / HTTP/1.0\r\n\r\n", "127.0.0.1:8824")).unwrap();
        assert!(resp.contains("Location: https://127.0.0.1:8824/"));
    }
}

/// axum-server Acceptor that answers plain-HTTP requests on the TLS port
/// with a 301 to https:// (Chrome shows ERR_INVALID_HTTP_RESPONSE
/// otherwise), by peeking the first byte: a TLS ClientHello starts 0x16;
/// printable ASCII means an HTTP verb.
#[derive(Clone)]
pub struct RedirectingAcceptor {
    inner: axum_server::tls_rustls::RustlsAcceptor,
    fallback_host: String,
}

impl RedirectingAcceptor {
    pub fn new(inner: axum_server::tls_rustls::RustlsAcceptor, fallback_host: String) -> Self {
        Self { inner, fallback_host }
    }
}

impl<S> axum_server::accept::Accept<tokio::net::TcpStream, S> for RedirectingAcceptor
where
    S: Send + 'static,
{
    type Stream = tokio_rustls::server::TlsStream<tokio::net::TcpStream>;
    type Service = S;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<(Self::Stream, Self::Service)>>
                + Send,
        >,
    >;

    fn accept(&self, stream: tokio::net::TcpStream, service: S) -> Self::Future {
        let inner = self.inner.clone();
        let fallback = self.fallback_host.clone();
        Box::pin(async move {
            let mut first = [0u8; 1];
            let n = stream.peek(&mut first).await?;
            if n == 1 && first[0] != 0x16 {
                // Plain HTTP: read the head, answer the redirect, close.
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut stream = stream;
                let mut head = vec![0u8; 2048];
                let read = stream.read(&mut head).await.unwrap_or(0);
                let resp = http_redirect_response(&head[..read], &fallback);
                let _ = stream.write_all(&resp).await;
                let _ = stream.shutdown().await;
                return Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionAborted,
                    "plain http redirected",
                ));
            }
            axum_server::accept::Accept::accept(&inner, stream, service).await
        })
    }
}
