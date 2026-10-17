//! The API on apid's local socket, for services on this device.
//!
//! A caller on [`micad_settings::APID_LOCAL_SOCKET_PATH`] carries no
//! credential: the kernel names its uid (`SO_PEERCRED`), the uid names a
//! [`LocalScope`], and the scope names the routes it may call. Nothing a
//! caller sends can widen what it is.
//!
//! Two scopes: root, which may call the whole API (a command line on the
//! device), and `mica-bled`, which may call the routes the phone app tunnels
//! (`mica-app:docs/protocol.md` section 7.1), the Bluetooth read the PIN comes
//! from, and the PIN write. The socket is `0660` with the `mica-bled` group,
//! or `0600` on a root without one, so nobody else can connect; a uid without
//! a scope that does is refused everything anyway.

use axum::extract::connect_info::{ConnectInfo, Connected};
use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::serve::IncomingStream;

use super::*;

/// The user and group `mica-bled` runs as (mica-pkgs' `mica-bled` package).
pub(crate) const BLED_USER: &str = "mica-bled";

/// The routes `mica-bled` may call, under `/api`. `{name}` is one non-empty
/// path segment.
const BLED_ROUTES: &[(Method, &str)] = &[
    (Method::GET, "/v1/system/info"),
    (Method::GET, "/v1/network/status"),
    (Method::GET, "/v1/network"),
    (Method::PUT, "/v1/network"),
    (Method::PUT, "/v1/network/{iface}"),
    (Method::DELETE, "/v1/network/{iface}"),
    (Method::GET, "/v1/wifi/client"),
    (Method::PUT, "/v1/wifi/client"),
    (Method::GET, "/v1/wifi/client/networks"),
    (Method::POST, "/v1/wifi/client/networks"),
    (Method::PUT, "/v1/wifi/client/networks/{ssid}"),
    (Method::DELETE, "/v1/wifi/client/networks/{ssid}"),
    (Method::POST, "/v1/wifi/client/scan"),
    (Method::GET, "/v1/wifi/ap"),
    (Method::PUT, "/v1/wifi/ap"),
    (Method::GET, "/v1/bluetooth"),
    (Method::PUT, "/v1/settings/bluetooth.pin"),
];

/// The peer on the other end of a local connection, as the kernel reports it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalPeer {
    /// `None` when the kernel would not say, which no scope matches.
    pub(crate) uid: Option<u32>,
}

impl Connected<IncomingStream<'_, tokio::net::UnixListener>> for LocalPeer {
    fn connect_info(stream: IncomingStream<'_, tokio::net::UnixListener>) -> Self {
        Self {
            uid: stream.io().peer_cred().ok().map(|cred| cred.uid()),
        }
    }
}

/// What a local caller may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LocalScope {
    /// root: the whole API.
    Root,
    /// `mica-bled`: [`BLED_ROUTES`].
    Bled,
}

/// `mica-bled`'s uid and gid, resolved once at start: the root's users are
/// fixed with its image.
#[derive(Debug, Clone, Default)]
pub(crate) struct LocalUsers {
    pub(crate) bled: Option<u32>,
    pub(crate) bled_group: Option<u32>,
}

impl LocalUsers {
    /// The ids `passwd` and `group` (the text of `/etc/passwd` and
    /// `/etc/group`) give `mica-bled`.
    pub(crate) fn from_files(passwd: &str, group: &str) -> Self {
        // Both files are `name:password:id:...`.
        let id_of = |text: &str| {
            text.lines().find_map(|line| {
                let mut fields = line.split(':');
                (fields.next() == Some(BLED_USER))
                    .then(|| fields.nth(1).and_then(|id| id.parse().ok()))
                    .flatten()
            })
        };
        Self {
            bled: id_of(passwd),
            bled_group: id_of(group),
        }
    }

    fn scope(&self, uid: Option<u32>) -> Option<LocalScope> {
        match uid {
            Some(0) => Some(LocalScope::Root),
            Some(_) if uid == self.bled => Some(LocalScope::Bled),
            _ => None,
        }
    }
}

/// The credential a local caller's request carries once [`local_gate`] let it
/// through; only that gate inserts it, and only on the local socket's router.
#[derive(Debug, Clone, Copy)]
pub(crate) struct LocalCredential;

/// The API for the local socket: [`app`] behind [`local_gate`].
pub(crate) fn local_app(state: AppState, users: LocalUsers) -> Router {
    app(state).layer(axum::middleware::from_fn_with_state(
        Arc::new(users),
        local_gate,
    ))
}

/// Let a request through when its caller's scope covers the route.
async fn local_gate(
    State(users): State<Arc<LocalUsers>>,
    mut request: Request,
    next: Next,
) -> Response {
    let uid = request
        .extensions()
        .get::<ConnectInfo<LocalPeer>>()
        .and_then(|info| info.0.uid);
    let Some(scope) = users.scope(uid) else {
        return local_refusal("this caller has no scope on apid's local socket");
    };
    let allowed = scope == LocalScope::Root
        || request
            .uri()
            .path()
            .strip_prefix(API)
            .is_some_and(|path| scope_allows(scope, request.method(), path));
    if !allowed {
        return local_refusal("this route is not in the caller's scope");
    }
    request.extensions_mut().insert(LocalCredential);
    next.run(request).await
}

fn local_refusal(message: &str) -> Response {
    api_response(
        StatusCode::FORBIDDEN,
        ApiError::apid("not_allowed", message.to_string()),
    )
}

/// Whether `scope` covers `method path`, `path` without the `/api` prefix.
pub(crate) fn scope_allows(scope: LocalScope, method: &Method, path: &str) -> bool {
    match scope {
        LocalScope::Root => true,
        LocalScope::Bled => BLED_ROUTES
            .iter()
            .any(|(allowed, pattern)| allowed == method && route_matches(pattern, path)),
    }
}

/// Whether `path` matches `pattern` segment by segment; a `{name}` segment is
/// any one segment other than empty, `.` and `..`.
fn route_matches(pattern: &str, path: &str) -> bool {
    let mut want = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (want.next(), got.next()) {
            (None, None) => return true,
            (Some(w), Some(g)) if w.starts_with('{') => {
                if g.is_empty() || g == "." || g == ".." {
                    return false;
                }
            }
            (Some(w), Some(g)) if w == g => {}
            _ => return false,
        }
    }
}
