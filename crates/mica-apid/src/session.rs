//! HMAC-signed session cookies backed by an in-process session table.
//!
//! A cookie value is `<id>.<mac>` where `id` is 16 random bytes hex-encoded
//! (128 bits) and `mac` is the hex HMAC-SHA256 of `id` under the persistent
//! signing key. Sessions live in memory only and expire after 24 hours, so an
//! apid restart logs everyone out.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use aws_lc_rs::hmac;
use axum::http::HeaderMap;

/// Session cookie name.
pub const COOKIE_NAME: &str = "apid_session";
const SESSION_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Signed-cookie session table.
///
/// Lock acquisitions recover from poisoning rather than propagating it: the
/// table is a plain map with no invariant a mid-update panic could half
/// establish, and treating poison as fatal would convert one panic while
/// holding the lock into a panic on every later login and session check — a
/// permanent denial of management out of a transient bug.
pub struct SessionStore {
    key: hmac::Key,
    sessions: Mutex<HashMap<String, StoredSession>>,
    generation: AtomicU64,
}

struct StoredSession {
    expires_at: Instant,
    csrf_token: String,
}

/// Credentials created for one authenticated browser session.
pub struct CreatedSession {
    /// The signed value stored in the HttpOnly cookie.
    pub cookie: String,
    /// The request token the SPA sends on state-changing API calls.
    pub csrf_token: String,
}

impl SessionStore {
    /// Store signing cookies with `key`.
    pub fn new(key: [u8; 32]) -> Self {
        Self {
            key: hmac::Key::new(hmac::HMAC_SHA256, &key),
            sessions: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
        }
    }

    fn tag(&self, id: &str) -> hmac::Tag {
        hmac::sign(&self.key, id.as_bytes())
    }

    /// Create a session and return its cookie and CSRF credentials.
    pub fn create(&self) -> CreatedSession {
        self.create_in(
            &mut self
                .sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }

    /// Capture before reading the password used to authenticate a login.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    /// Issue only if no credential rotation completed during authentication.
    pub fn create_if_current(&self, generation: u64) -> Option<CreatedSession> {
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (self.generation() == generation).then(|| self.create_in(&mut sessions))
    }

    fn create_in(&self, sessions: &mut HashMap<String, StoredSession>) -> CreatedSession {
        let id = hex::encode(micad_settings::random_bytes::<16>());
        let csrf_token = hex::encode(micad_settings::random_bytes::<32>());
        let mac = hex::encode(self.tag(&id));
        sessions.insert(
            id.clone(),
            StoredSession {
                expires_at: Instant::now() + SESSION_TTL,
                csrf_token: csrf_token.clone(),
            },
        );
        CreatedSession {
            cookie: format!("{id}.{mac}"),
            csrf_token,
        }
    }

    /// Split a cookie value into its id, verifying the signature.
    fn verify_signature(&self, value: &str) -> Option<String> {
        let (id, mac_hex) = value.split_once('.')?;
        let mac_bytes = hex::decode(mac_hex).ok()?;
        hmac::verify(&self.key, id.as_bytes(), &mac_bytes).ok()?;
        Some(id.to_string())
    }

    /// True when `value` is well-signed and names a live session.
    pub fn verify(&self, value: &str) -> bool {
        let Some(id) = self.verify_signature(value) else {
            return false;
        };
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match sessions.get(&id) {
            Some(session) if session.expires_at > Instant::now() => true,
            Some(_) => {
                sessions.remove(&id);
                false
            }
            None => false,
        }
    }

    /// Return the CSRF token for a live, signed session cookie.
    pub fn csrf_token(&self, value: &str) -> Option<String> {
        let id = self.verify_signature(value)?;
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match sessions.get(&id) {
            Some(session) if session.expires_at > Instant::now() => {
                Some(session.csrf_token.clone())
            }
            Some(_) => {
                sessions.remove(&id);
                None
            }
            None => None,
        }
    }

    /// Verify a CSRF token without an early-exit string comparison.
    pub fn verify_csrf(&self, value: &str, presented: &str) -> bool {
        let Some(expected) = self.csrf_token(value) else {
            return false;
        };
        hmac::verify(
            &self.key,
            presented.as_bytes(),
            self.tag(&expected).as_ref(),
        )
        .is_ok()
    }

    /// Drop every session except the one named by `value`.
    ///
    /// The password-change path calls this: every other session was minted
    /// under the old credential, and the one performing the change is the one
    /// proof of possession the new credential has. A `value` that does not
    /// verify keeps nothing, which errs closed.
    pub fn remove_all_except(&self, value: &str) {
        let keep = self.verify_signature(value);
        let mut sessions = self
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The check and insertion in create_if_current take the same lock.
        // A login either predates this removal or sees the new generation.
        self.generation.fetch_add(1, Ordering::Release);
        match keep {
            Some(id) => sessions.retain(|key, _| *key == id),
            None => sessions.clear(),
        }
    }

    /// Drop the session named by `value`, if any.
    pub fn remove(&self, value: &str) {
        if let Some(id) = self.verify_signature(value) {
            self.sessions
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .remove(&id);
        }
    }
}

/// `Set-Cookie` value establishing a session; `Secure` when served over HTTPS.
pub fn session_cookie(value: &str, secure: bool) -> String {
    format!(
        "{COOKIE_NAME}={value}; Path=/; HttpOnly;{} SameSite=Lax; Max-Age=86400",
        if secure { " Secure;" } else { "" }
    )
}

/// `Set-Cookie` value clearing the session cookie.
pub fn clear_cookie(secure: bool) -> String {
    format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly;{} SameSite=Lax; Max-Age=0",
        if secure { " Secure;" } else { "" }
    )
}

/// Extract the session cookie value from request headers.
pub fn cookie_from_headers(headers: &HeaderMap) -> Option<String> {
    let prefix = format!("{COOKIE_NAME}=");
    for header in headers.get_all(axum::http::header::COOKIE) {
        let Ok(text) = header.to_str() else { continue };
        for pair in text.split(';') {
            if let Some(value) = pair.trim().strip_prefix(&prefix) {
                return Some(value.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_tamper() {
        let store = SessionStore::new([1u8; 32]);
        let session = store.create();
        let value = session.cookie;
        assert!(store.verify(&value));
        assert!(store.verify_csrf(&value, &session.csrf_token));
        assert!(!store.verify_csrf(&value, "wrong"));

        let mut tampered = value.clone().into_bytes();
        let last = tampered.last_mut().unwrap();
        *last = if *last == b'a' { b'b' } else { b'a' };
        assert!(!store.verify(&String::from_utf8(tampered).unwrap()));

        store.remove(&value);
        assert!(!store.verify(&value));
    }

    #[test]
    fn remove_all_except_keeps_only_the_named_session() {
        let store = SessionStore::new([1u8; 32]);
        let kept = store.create().cookie;
        let dropped = store.create().cookie;
        store.remove_all_except(&kept);
        assert!(store.verify(&kept));
        assert!(!store.verify(&dropped));

        // A value that does not verify keeps nothing.
        store.remove_all_except("not-a-cookie");
        assert!(!store.verify(&kept));
    }
}
