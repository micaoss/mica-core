//! The kernel's keyrings: adding an X.509 certificate to a keyring, as
//! keyutils adds an `asymmetric` key read from its input, and sealing it, as
//! `keyctl restrict_keyring <ring>` does.

use rustix::io;

/// `KEYCTL_RESTRICT_KEYRING` of `<linux/keyctl.h>`.
const KEYCTL_RESTRICT_KEYRING: libc::c_int = 29;

/// The serial of the keyring named `name` in `/proc/keys` text, which lists
/// the keys the reader may view: `<serial hex> <flags> <usage> <timeout>
/// <perm> <uid> <gid> <type> <description>: <summary>`.
#[must_use]
pub fn keyring_named(proc_keys: &str, name: &str) -> Option<i32> {
    proc_keys.lines().find_map(|line| {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let (serial, kind, description) = (fields.first()?, fields.get(7)?, fields.get(8)?);
        (*kind == "keyring" && description.strip_suffix(':') == Some(name))
            .then(|| u32::from_str_radix(serial, 16).ok())
            .flatten()
            .and_then(|serial| i32::try_from(serial).ok())
    })
}

fn result(value: libc::c_long) -> io::Result<libc::c_long> {
    if value >= 0 {
        Ok(value)
    } else {
        Err(io::Errno::from_raw_os_error(
            std::io::Error::last_os_error().raw_os_error().unwrap_or(0),
        ))
    }
}

/// Add the DER X.509 certificate `der` to the keyring `keyring` as an
/// `asymmetric` key, and answer its serial. The kernel parses and checks the
/// certificate and refuses one it cannot use (`EBADMSG`, `ENOPKG`), and a
/// sealed keyring refuses every key.
#[allow(unsafe_code)]
pub fn add_asymmetric_key(keyring: i32, der: &[u8]) -> io::Result<i32> {
    // SAFETY: add_key(2) reads the NUL-terminated type, no description (NULL
    // lets the asymmetric type derive one) and `der.len()` bytes at
    // `der.as_ptr()`, all borrowed for the synchronous call, and writes
    // nothing of ours.
    let serial = result(unsafe {
        libc::syscall(
            libc::SYS_add_key,
            c"asymmetric".as_ptr(),
            std::ptr::null::<libc::c_char>(),
            der.as_ptr(),
            der.len(),
            keyring,
        )
    })?;
    i32::try_from(serial).map_err(|_| io::Errno::RANGE)
}

/// Seal the keyring `keyring`: a restriction with no type rejects every key
/// added or linked from now on. A keyring sealed already answers `EEXIST`.
#[allow(unsafe_code)]
pub fn restrict_keyring(keyring: i32) -> io::Result<()> {
    // SAFETY: keyctl(KEYCTL_RESTRICT_KEYRING) takes the keyring serial and
    // two NULL strings (no type, no restriction); nothing is written.
    result(unsafe {
        libc::syscall(
            libc::SYS_keyctl,
            KEYCTL_RESTRICT_KEYRING,
            keyring,
            std::ptr::null::<libc::c_char>(),
            std::ptr::null::<libc::c_char>(),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_keyring_is_found_by_its_name_and_type() {
        let keys = "\
0d9b8a3c I------     1 perm 1f0b0000     0     0 keyring   .builtin_trusted_keys: 1
1a2b3c4d I------     1 perm 3f0f0000     0     0 keyring   .mica_owner: empty
2b3c4d5e I------     1 perm 1f030000     0     0 asymmetric .mica_owner: X509.rsa 0123abcd []
3c4d5e6f I------     1 perm 3f0f0000     0     0 keyring   .mica_owner_other: empty
";
        assert_eq!(keyring_named(keys, ".mica_owner"), Some(0x1a2b_3c4d));
        assert_eq!(
            keyring_named(keys, ".builtin_trusted_keys"),
            Some(0x0d9b_8a3c)
        );
        assert_eq!(keyring_named(keys, ".machine"), None);
        assert_eq!(keyring_named("", ".mica_owner"), None);
        assert_eq!(
            keyring_named(
                "zz I 1 perm 1 0 0 keyring .mica_owner: empty",
                ".mica_owner"
            ),
            None
        );
    }
}
