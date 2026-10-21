//! BLE provisioning, protocol 1 (`docs/ble-provisioning.md`): a phone sends
//! the Wi-Fi network to join over BLE and, on an unclaimed device, claims it.

pub mod channel;
pub mod spake2plus;
pub mod store;

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

#[cfg(test)]
mod tests;
