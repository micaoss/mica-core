//! Static asset hosting: path resolution, content classification, and the
//! router that applies both.
//!
//! [`serve`] applies the fallback for the one rejection that permits it
//! ([`path::Rejection::eligible_for_fallback`]); the `/api/` row is
//! [`mime::CacheClass::NoStore`], applied by the reservation in
//! `crate::routes`.

pub mod builtin;
pub mod mime;
pub mod path;
pub mod serve;
