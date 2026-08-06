use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::ServerName;

use super::errors::{FetchError, FetchErrorKind};

/// A connection whose peer address was already validated by the SSRF policy.
/// The wire protocol stays HTTP/1.1 with the original hostname pinned in the
/// Host header and, for https, as the TLS SNI and certificate identity.
pub struct SafeStream {
    io: Box<dyn SafeIo + Send>,
}

trait SafeIo: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin> SafeIo for T {}

impl hyper::rt::Read for SafeStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        mut buf: hyper::rt::ReadBufCursor<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let io = unsafe { self.map_unchecked_mut(|this| &mut *this.io) };
        let filled = {
            let mut inner = tokio::io::ReadBuf::uninit(unsafe { buf.as_mut() });
            match tokio::io::AsyncRead::poll_read(io, cx, &mut inner) {
                std::task::Poll::Ready(Ok(())) => inner.filled().len(),
                other => return other,
            }
        };
        unsafe { buf.advance(filled) };
        std::task::Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for SafeStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let io = unsafe { self.map_unchecked_mut(|this| &mut *this.io) };
        tokio::io::AsyncWrite::poll_write(io, cx, buf)
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let io = unsafe { self.map_unchecked_mut(|this| &mut *this.io) };
        tokio::io::AsyncWrite::poll_flush(io, cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        let io = unsafe { self.map_unchecked_mut(|this| &mut *this.io) };
        tokio::io::AsyncWrite::poll_shutdown(io, cx)
    }
}

/// Dial a pre-validated address list. DNS happened earlier inside the fetch
/// pipeline; the connector never resolves again, so a rebinding response
/// cannot replace the validated peer.
#[derive(Clone)]
pub struct SafeConnector {
    pub addresses: Arc<Vec<std::net::SocketAddr>>,
    pub tls: Option<Arc<tokio_rustls::TlsConnector>>,
    pub server_name: Option<ServerName<'static>>,
    pub connect_timeout: Duration,
}

impl SafeConnector {
    pub async fn connect(&self) -> std::io::Result<SafeStream> {
        let mut last_error: Option<std::io::Error> = None;
        for address in self.addresses.iter() {
            match connect_one(
                *address,
                self.tls.as_deref(),
                self.server_name.as_ref(),
                self.connect_timeout,
            )
            .await
            {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }
        Err(last_error.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotConnected, "no address to dial")
        }))
    }
}

async fn connect_one(
    address: std::net::SocketAddr,
    tls: Option<&tokio_rustls::TlsConnector>,
    server_name: Option<&ServerName<'static>>,
    connect_timeout: Duration,
) -> std::io::Result<SafeStream> {
    let tcp = tokio::time::timeout(connect_timeout, tokio::net::TcpStream::connect(address))
        .await
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("connect to {address} exceeded budget"),
            )
        })??;
    let io: Box<dyn SafeIo + Send> = match (tls, server_name) {
        (Some(tls), Some(server_name)) => {
            let tls_stream =
                tokio::time::timeout(connect_timeout, tls.connect(server_name.clone(), tcp))
                    .await
                    .map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            format!("TLS handshake with {address} exceeded budget"),
                        )
                    })??;
            Box::new(tls_stream)
        }
        _ => Box::new(tcp),
    };
    Ok(SafeStream { io })
}

/// Prepare the TLS identity for a hostname or literal IP.
pub fn server_name_for(host: &str) -> Option<ServerName<'static>> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        Some(ServerName::IpAddress(ip.into()))
    } else {
        ServerName::try_from(host.to_string()).ok()
    }
}

/// Build the rustls client config: system roots plus configured extra CAs,
/// falling back to webpki roots when the platform store is empty. ALPN is
/// pinned to HTTP/1.1 to match the connector.
pub fn tls_config(
    extra_ca_certificates: &[Vec<u8>],
) -> Result<Arc<rustls::ClientConfig>, FetchError> {
    // Both rustls providers are compiled into this dependency graph (the
    // provider client pulls in aws-lc-rs through hyper-rustls); pick ring,
    // which the workspace already links for its crypto.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut store = rustls::RootCertStore::empty();
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = store.add(certificate);
    }
    for pem in extra_ca_certificates {
        let mut reader = std::io::Cursor::new(pem.as_slice());
        let certificates = rustls_pemfile::certs(&mut reader).collect::<Result<Vec<_>, _>>();
        for certificate in certificates.map_err(|error| {
            FetchError::new(
                FetchErrorKind::Transport,
                format!("invalid extra CA certificate PEM: {error}"),
            )
        })? {
            let _ = store.add(certificate);
        }
    }
    if store.is_empty() {
        store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(store)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}
