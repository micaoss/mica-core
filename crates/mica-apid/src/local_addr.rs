//! The address of this device a connection arrived on.
//!
//! Both listeners put a [`LocalAddr`] extension on every request: the console
//! compares it with the observed interface addresses to tell which interface
//! carries the browser's own session.

use std::convert::Infallible;
use std::io;
use std::net::SocketAddr;

use axum::Extension;
use axum::Router;
use axum::extract::connect_info::ConnectInfo;
use axum::middleware::AddExtension;
use axum::serve::IncomingStream;
use tokio::net::{TcpListener, TcpStream};
use tower::Layer as _;

/// The local end of the connection a request came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalAddr(pub SocketAddr);

/// Serve `router` on plain HTTP with [`LocalAddr`] and the peer's
/// [`ConnectInfo`] on every request.
///
/// The accepted socket's own address is read, not the listener's: a wildcard
/// bind is `0.0.0.0`, which names no interface.
pub async fn serve_http(listener: TcpListener, router: Router) -> io::Result<()> {
    // The bound address stands in only if the kernel cannot name the socket's.
    let bound = listener.local_addr()?;
    let make = tower::service_fn(move |incoming: IncomingStream<'_, TcpListener>| {
        let local = incoming.io().local_addr().unwrap_or(bound);
        let service = Extension(LocalAddr(local))
            .layer(Extension(ConnectInfo(*incoming.remote_addr())).layer(router.clone()));
        std::future::ready(Ok::<_, Infallible>(service))
    });
    axum::serve(listener, make).await
}

/// An `axum_server` acceptor that adds [`LocalAddr`] before TLS.
#[derive(Debug, Clone, Copy, Default)]
pub struct LocalAddrAcceptor;

impl<S> axum_server::accept::Accept<TcpStream, S> for LocalAddrAcceptor {
    type Stream = TcpStream;
    type Service = AddExtension<S, LocalAddr>;
    type Future = std::future::Ready<io::Result<(Self::Stream, Self::Service)>>;

    fn accept(&self, stream: TcpStream, service: S) -> Self::Future {
        std::future::ready(
            stream
                .local_addr()
                .map(|local| (stream, Extension(LocalAddr(local)).layer(service))),
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::get;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    use super::*;

    async fn echo(
        Extension(LocalAddr(local)): Extension<LocalAddr>,
        ConnectInfo(remote): ConnectInfo<SocketAddr>,
    ) -> String {
        format!("{local} {remote}")
    }

    // A wildcard listener still reports the address the client dialled.
    #[tokio::test]
    async fn plain_http_carries_both_ends_of_the_connection() {
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(serve_http(listener, Router::new().route("/", get(echo))));

        let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let client = stream.local_addr().unwrap();
        stream
            .write_all(b"GET / HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(
            response.ends_with(&format!("127.0.0.1:{port} {client}")),
            "{response}"
        );
    }

    #[tokio::test]
    async fn the_acceptor_tags_the_service_with_the_socket_address() {
        use axum_server::accept::Accept as _;
        use tower::ServiceExt as _;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let client = tokio::spawn(TcpStream::connect(addr));
        let (stream, _) = listener.accept().await.unwrap();
        let (_, service) = LocalAddrAcceptor
            .accept(
                stream,
                Router::new().route(
                    "/",
                    get(
                        |Extension(LocalAddr(local)): Extension<LocalAddr>| async move {
                            local.to_string()
                        },
                    ),
                ),
            )
            .await
            .unwrap();
        let response = service
            .oneshot(axum::http::Request::new(axum::body::Body::empty()))
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 1024)
            .await
            .unwrap();
        assert_eq!(body, addr.to_string());
        drop(client);
    }
}
