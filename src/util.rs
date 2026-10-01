use std::io::Read;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Civil UTC date-time from a Unix timestamp (Howard Hinnant's days-from-civil inverse).
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day, (rem / 3600) as u32, (rem % 3600 / 60) as u32, (rem % 60) as u32)
}

pub fn iso8601(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Compact timestamp for generated file names, e.g. 20260927T073320.
pub fn stamp(secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil(secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}")
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut buf)).is_ok();
    if !filled {
        // No /dev/urandom (Windows): time- and pid-seeded xorshift is enough for ids and jitter.
        let mut x = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
            ^ (u64::from(std::process::id()) << 32)
            | 1;
        for byte in buf.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *byte = x as u8;
        }
    }
    buf
}

/// Random UUID-shaped id (version 4 layout).
pub fn random_id() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

/// Uniform float in [0, 1) for retry jitter.
pub fn random_unit() -> f64 {
    (u64::from_le_bytes(random_bytes::<8>()) >> 11) as f64 / (1u64 << 53) as f64
}

/// FNV-1a 64: a fingerprint to notice a changed file, not a security hash.
pub fn fnv1a64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_timestamps() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_767_323_045), "2026-01-02T03:04:05Z");
        assert_eq!(stamp(1_767_323_045), "20260102T030405");
        assert_eq!(iso8601(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn fnv1a64_matches_the_reference_values() {
        assert_eq!((fnv1a64(b""), fnv1a64(b"a")), (0xcbf2_9ce4_8422_2325, 0xaf63_dc4c_8601_ec8c));
    }

    #[test]
    fn random_id_is_uuid_shaped() {
        let id = random_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, random_id());
    }
}
