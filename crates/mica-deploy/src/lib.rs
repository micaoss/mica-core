//! Authenticated file deployments and early boot.
#![forbid(unsafe_code)]

pub mod acquisition;
pub mod board;
pub mod boot;
pub mod catalog;
pub mod chunks;
pub mod components;
pub mod core_set;
pub mod debian_version;
pub mod defaults;
pub mod deployments;
pub mod driver_ext;
pub mod extensions;
pub mod factory_reset;
pub mod firmware;
pub mod fit_env;
pub mod owner_certs;
pub mod set_state;
pub mod ubi;
pub mod update_set;
