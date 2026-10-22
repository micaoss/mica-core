//! BLE provisioning, protocol 1 (`docs/ble-provisioning.md`): a phone sends
//! a device its network, Wi-Fi or wired, over BLE, and nothing else.

pub mod bluez;
pub mod channel;
pub mod engine;
pub mod runtime;
pub mod spake2plus;
pub mod store;
pub mod window;

/// `info.protocol`.
pub const PROTOCOL: u32 = 1;
/// The SPAKE2+ identities.
pub const ID_PROVER: &[u8] = b"mica-provisioning phone";
pub const ID_VERIFIER: &[u8] = b"mica-provisioning device";
/// The SPAKE2+ contexts, one per mode.
pub const CONTEXT_PASSWORD: &[u8] = b"mica-provisioning/1 password";
pub const CONTEXT_TEMP_CODE: &[u8] = b"mica-provisioning/1 temp-code";

/// The service and its characteristics.
pub const SERVICE_UUID: &str = "a7f50001-aa05-4fbe-ae8a-39652ca94ed5";
pub const INFO_UUID: &str = "a7f50002-aa05-4fbe-ae8a-39652ca94ed5";
pub const SESSION_UUID: &str = "a7f50003-aa05-4fbe-ae8a-39652ca94ed5";
pub const COMMAND_UUID: &str = "a7f50004-aa05-4fbe-ae8a-39652ca94ed5";
pub const EVENT_UUID: &str = "a7f50005-aa05-4fbe-ae8a-39652ca94ed5";

pub use runtime::Provisioning;

/// Whether a provisioning window holds the radio: the Bluetooth reconciler
/// keeps the adapter running for it even when Bluetooth is switched off.
static RADIO_HELD: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// See [`RADIO_HELD`].
#[must_use]
pub fn radio_held() -> bool {
    RADIO_HELD.load(std::sync::atomic::Ordering::SeqCst)
}

/// See [`RADIO_HELD`].
pub fn set_radio_held(held: bool) {
    RADIO_HELD.store(held, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(test)]
mod tests;
