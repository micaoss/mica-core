//! The API on apid's local socket, for a command line on this device.
//!
//! A caller on [`micad_settings::APID_LOCAL_SOCKET_PATH`] carries no
//! credential: the kernel names its uid (`SO_PEERCRED`), and only root may
//! call the API, all of it. The socket is `0600`, so nobody else can connect;
//! a uid that does is refused everything anyway.

use axum::extract::Request;
use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use axum::serve::IncomingStream;

use super::*;

/// The peer on the other end of a local connection, as the kernel reports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalPeer {
    /// `None` when the kernel would not say, which is not root.
    pub(crate) uid: Option<u32>,
}

impl Connected<IncomingStream<'_, tokio::net::UnixListener>> for LocalPeer {
    fn connect_info(stream: IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Self {
            uid: stream.io().peer_cred().ok().map(|cred| cred.uid()),
        }
    }
}

/// The credential a local caller's request carries once [`local_gate`] let it
/// through; only that gate inserts it, and only on the local socket's router.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalCredential;

/// The API for the local socket: [`app`] behind [`local_gate`].
pub(crate) fn local_app(state: AppState) -> Router {
    app(state).layer(axum::middleware::from_fn(local_gate))
}

/// Let root's requests through.
async fn local_gate(mut request: Request, next: Next) -> Response {
    let uid = request
        .extensions()
        .get::<ConnectInfo<LocalPeer>>()
        .and_then(|info| info.0.uid);
    if uid != Some(0) {
        return api_response(
            StatusCode::FORBIDDEN,
            ApiError::apid(
                "not_allowed",
                "only root may call apid's local socket".to_string(),
            ),
        );
    }
    request.extensions_mut().insert(LocalCredential);
    next.run(request).await
}
