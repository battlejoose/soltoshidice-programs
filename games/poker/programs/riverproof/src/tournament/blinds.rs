//! One schedule for the arena; a table snapshots it only when a new hand starts.
use super::model::{DURATION_SECONDS, PolicyError};

pub const BLIND_LEVEL_SECONDS: i64 = 10 * 60;

/// Add the ORIGINAL small blind each level, never compound the current blind.
/// Clamp terminal/history queries to the final playable level. Native admission
/// and start guards separately enforce the tournament's actual time bounds.
pub fn blinds_at(buy_in: u64, starts_at: i64, now: i64) -> Result<(u64, u64), PolicyError> {
    let elapsed = now.saturating_sub(starts_at).clamp(0, DURATION_SECONDS - 1);
    let level = 1 + (elapsed / BLIND_LEVEL_SECONDS) as u64;
    let small = (buy_in / 100).max(1).checked_mul(level).ok_or(PolicyError::Arithmetic)?;
    let big = small.checked_mul(2).ok_or(PolicyError::Arithmetic)?;
    Ok((small, big))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linear_levels_follow_the_scheduled_start_and_cap_at_three_hours() {
        let start = 1_800_000_000;
        assert_eq!(blinds_at(10_000, start, start - 1).unwrap(), (100, 200));
        for level in 1..=18 {
            let at = start + (level - 1) * 600;
            let expected = (level as u64 * 100, level as u64 * 200);
            assert_eq!(blinds_at(10_000, start, at).unwrap(), expected);
            assert_eq!(blinds_at(10_000, start, at + 599).unwrap(), expected);
        }
        assert_eq!(blinds_at(10_000, start, start + 10800).unwrap(), (1800, 3600));
        assert_eq!(blinds_at(10_000, start, i64::MAX).unwrap(), (1800, 3600));
    }

    #[test]
    fn buy_in_scaling_preserves_whole_tokens_and_handles_large_values() {
        assert_eq!(blinds_at(1_000_000, 0, 1200).unwrap(), (30_000, 60_000));
        assert_eq!(blinds_at(40, 0, 600).unwrap(), (2, 4));
        assert_eq!(blinds_at(10_099, 0, 1200).unwrap(), (300, 600));
        let base = u64::MAX / 100;
        assert_eq!(blinds_at(u64::MAX, 0, 10799).unwrap(), (base * 18, base * 36));
    }
}
