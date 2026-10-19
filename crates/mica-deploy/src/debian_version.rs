//! Debian package versions, `[epoch:]upstream[-revision]`, in dpkg's order: the
//! versions a root `provides` and a core component `requires`.
use std::cmp::Ordering;

/// The longest version read.
const MAX_LEN: usize = 64;

/// A version's three parts.
struct Parts<'a> {
    epoch: u64,
    upstream: &'a str,
    revision: &'a str,
}

fn parts(version: &str) -> Option<Parts<'_>> {
    if version.is_empty() || version.len() > MAX_LEN {
        return None;
    }
    let (epoch, rest) = match version.split_once(':') {
        Some((epoch, rest)) => {
            if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) || epoch.len() > 9 {
                return None;
            }
            (epoch.parse().ok()?, rest)
        }
        None => (0, version),
    };
    let (upstream, revision) = rest.rsplit_once('-').unwrap_or((rest, ""));
    let upstream_ok = upstream.bytes().next().is_some_and(|b| b.is_ascii_digit())
        && upstream
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'~' | b'-'));
    let revision_ok = (revision.is_empty() && !rest.contains('-'))
        || (!revision.is_empty()
            && revision
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'+' | b'~')));
    (upstream_ok && revision_ok).then_some(Parts {
        epoch,
        upstream,
        revision,
    })
}

/// Whether `version` is a Debian version this reader orders.
pub fn valid(version: &str) -> bool {
    parts(version).is_some()
}

/// dpkg's weight of one character of a non-digit run: `~` before the end,
/// the end before letters, letters before everything else.
fn weight(byte: Option<u8>) -> i32 {
    match byte {
        None => 0,
        Some(b'~') => -1,
        Some(b) if b.is_ascii_alphabetic() => i32::from(b),
        Some(b) => i32::from(b) + 256,
    }
}

/// dpkg's `verrevcmp`: alternating non-digit and digit runs.
fn compare_part(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    while !a.is_empty() || !b.is_empty() {
        loop {
            let (x, y) = (a.first().copied(), b.first().copied());
            let x_text = x.is_some_and(|c| !c.is_ascii_digit());
            let y_text = y.is_some_and(|c| !c.is_ascii_digit());
            if !x_text && !y_text {
                break;
            }
            let (wx, wy) = (weight(x.filter(|_| x_text)), weight(y.filter(|_| y_text)));
            if wx != wy {
                return wx.cmp(&wy);
            }
            if x_text {
                a = &a[1..];
            }
            if y_text {
                b = &b[1..];
            }
        }
        let digits = |s: &[u8]| s.iter().take_while(|c| c.is_ascii_digit()).count();
        let (na, nb) = (digits(a), digits(b));
        let trim = |s: &[u8]| {
            let zeros = s.iter().take_while(|c| **c == b'0').count();
            s[zeros..].to_vec()
        };
        let (da, db) = (trim(&a[..na]), trim(&b[..nb]));
        let order = da.len().cmp(&db.len()).then_with(|| da.cmp(&db));
        if order != Ordering::Equal {
            return order;
        }
        a = &a[na..];
        b = &b[nb..];
    }
    Ordering::Equal
}

/// `a` against `b` in dpkg's order; `None` when either is not a version.
pub fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a, b) = (parts(a)?, parts(b)?);
    Some(
        a.epoch
            .cmp(&b.epoch)
            .then_with(|| compare_part(a.upstream, b.upstream))
            .then_with(|| compare_part(a.revision, b.revision)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_as_dpkg_orders_them() {
        for (a, b) in [
            ("6.1.4-5", "6.1.4-7"),
            ("6.1.4-7", "6.1.10-1"),
            ("6.1.4~rc1-1", "6.1.4-1"),
            ("6.1.4-1", "6.1.4+b1-1"),
            ("6.1.4", "6.1.4a"),
            ("0.0.16-1", "1:0.0.1-1"),
            ("1.0-1", "1.0-1.1"),
            ("1.0~~-1", "1.0~-1"),
        ] {
            assert_eq!(compare(a, b), Some(Ordering::Less), "{a} < {b}");
            assert_eq!(compare(b, a), Some(Ordering::Greater), "{b} > {a}");
        }
        assert_eq!(compare("6.1.04-7", "6.1.4-7"), Some(Ordering::Equal));
        assert_eq!(compare("0:1.0", "1.0"), Some(Ordering::Equal));
    }

    #[test]
    fn what_is_not_a_version_is_refused() {
        for bad in [
            "",
            "a1.0",
            "1.0-",
            ":1.0",
            "x:1.0",
            "1.0 1",
            "1.0-a_b",
            &"1".repeat(65),
        ] {
            assert!(!valid(bad), "{bad:?}");
            assert_eq!(compare(bad, "1.0"), None, "{bad:?}");
        }
        for good in ["1", "6.1.4-7", "2:1.0~rc1+dfsg-3ubuntu1", "1.0-1-2"] {
            assert!(valid(good), "{good:?}");
        }
    }
}
