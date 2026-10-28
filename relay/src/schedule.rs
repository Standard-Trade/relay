//! Turning a manifest's "HH:MM" slots on a date into unix times, with jitter.

use anyhow::{bail, Result};
use sha2::{Digest, Sha256};

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

pub fn parse_date(s: &str) -> Result<(i64, i64, i64)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 3 {
        bail!("date must be YYYY-MM-DD, got {s}");
    }
    let (y, m, d) = (parts[0].parse()?, parts[1].parse()?, parts[2].parse()?);
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        bail!("bad date {s}");
    }
    Ok((y, m, d))
}

pub fn parse_slot(s: &str) -> Result<i64> {
    let (h, m) = s.split_once(':').ok_or_else(|| anyhow::anyhow!("slot must be HH:MM, got {s}"))?;
    let (h, m): (i64, i64) = (h.parse()?, m.parse()?);
    if !(0..24).contains(&h) || !(0..60).contains(&m) {
        bail!("bad slot {s}");
    }
    Ok(h * 3600 + m * 60)
}

/// Deterministic offset in [-jitter, +jitter] minutes, from the post id, so
/// re-importing a manifest gives the same times.
pub fn jitter_secs(post_id: &str, jitter_minutes: i64) -> i64 {
    if jitter_minutes <= 0 {
        return 0;
    }
    let h = Sha256::digest(post_id.as_bytes());
    let n = u64::from_le_bytes(h[..8].try_into().unwrap());
    let span = (2 * jitter_minutes * 60 + 1) as u64;
    (n % span) as i64 - jitter_minutes * 60
}

/// Unix time for a local date + slot.
pub fn slot_time(date: &str, slot: &str, utc_offset: i64) -> Result<i64> {
    let (y, m, d) = parse_date(date)?;
    Ok(days_from_civil(y, m, d) * 86_400 + parse_slot(slot)? - utc_offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epoch_and_known_dates() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(2026, 10, 25), 20_751);
    }

    #[test]
    fn slot_in_pdt() {
        // 2026-10-25 07:00 at UTC-7 is 14:00 UTC.
        assert_eq!(slot_time("2026-10-25", "07:00", -7 * 3600).unwrap(), 20_751 * 86_400 + 14 * 3600);
    }

    #[test]
    fn jitter_is_bounded_and_stable() {
        for id in ["a", "b", "golftips4-1", "x-99"] {
            let j = jitter_secs(id, 7);
            assert!((-420..=420).contains(&j));
            assert_eq!(j, jitter_secs(id, 7));
        }
        assert_eq!(jitter_secs("a", 0), 0);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(parse_slot("25:00").is_err());
        assert!(parse_slot("7").is_err());
        assert!(parse_date("2026-13-01").is_err());
    }
}
