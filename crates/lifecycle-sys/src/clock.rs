//! The kernel's clock-synchronization bit, what timedated reports as
//! `NTPSynchronized`: `adjtimex(2)` read with no modes, `STA_UNSYNC` clear.

use rustix::io;

/// `STA_UNSYNC` of `<linux/timex.h>`.
const STA_UNSYNC: i32 = 0x0040;

const _: () = assert!(libc::STA_UNSYNC == STA_UNSYNC);

/// Whether a time daemon has told the kernel its clock is synchronized.
#[allow(unsafe_code)]
pub fn clock_synchronized() -> io::Result<bool> {
    // SAFETY: timex is plain integers, so all-zero is a valid value, and
    // modes = 0 makes adjtimex a read: the kernel only writes the struct,
    // which this frame owns for the synchronous call.
    let mut value: libc::timex = unsafe { std::mem::zeroed() };
    // SAFETY: a valid, exclusively borrowed timex, as above.
    let result = unsafe { libc::adjtimex(&mut value) };
    if result < 0 {
        return Err(io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ));
    }
    Ok(value.status & STA_UNSYNC == 0)
}
