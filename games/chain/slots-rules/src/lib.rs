//! Pure rules consumed by the bank's slots candidate and offline verifier.
//! This crate itself has no entrypoint, custody, callback or deployment.
use sha2::{Digest, Sha256};
pub const DOMAIN: &[u8] = b"sdice:slots:classic-96:v1";
pub const REELS: [[u8; 20]; 3] = [
    [0, 1, 3, 2, 4, 0, 3, 1, 2, 5, 0, 4, 2, 3, 1, 0, 2, 4, 1, 3],
    [2, 0, 4, 1, 3, 2, 0, 3, 1, 4, 2, 0, 5, 3, 1, 2, 4, 0, 3, 1],
    [3, 2, 0, 1, 4, 3, 2, 0, 4, 1, 3, 2, 0, 1, 5, 4, 3, 2, 0, 1],
];
pub const MAX_MULTIPLIER: u64 = 150;
pub const SDICE_UNIT: u64 = 1_000_000;
pub const MAX_WAGER: u64 = 100_000 * SDICE_UNIT;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rewards {
    pub direct: u64,
    pub staking: u64,
    pub tree: u64,
    pub total: u64,
}
pub fn valid_wager(wager: u64) -> bool {
    wager > 0 && wager <= MAX_WAGER
}
/// Match economy::bet_fees() exactly, including indivisible base units.
pub fn rewards(wager: u64) -> Option<Rewards> {
    if !valid_wager(wager) {
        return None;
    }
    let total = wager.checked_mul(3)? / 100;
    let direct = wager / 100;
    Some(Rewards {
        direct,
        staking: direct,
        tree: total - direct * 2,
        total,
    })
}
pub fn liability(wager: u64) -> Option<u64> {
    wager
        .checked_mul(MAX_MULTIPLIER)?
        .checked_add(rewards(wager)?.total)
}
/// Reserve the full gross jackpot AND deferred reward credits. Keep this in
/// the shared bank reserve so another game or a stake withdrawal cannot spend it.
pub fn reserve_spin(vault: u64, reserved: u64, wager: u64) -> Option<u64> {
    if reserved > vault {
        return None;
    }
    let next = reserved.checked_add(liability(wager)?)?;
    if vault.checked_add(wager)? < next {
        return None;
    }
    Some(next)
}

pub fn multiplier(stops: [u8; 3]) -> Option<u64> {
    if stops.iter().any(|s| *s >= 20) {
        return None;
    }
    let [a, b, c] = [
        REELS[0][stops[0] as usize],
        REELS[1][stops[1] as usize],
        REELS[2][stops[2] as usize],
    ];
    Some(if a == b && b == c {
        [10, 5, 10, 20, 30, 150][a as usize]
    } else if a == 0 {
        if b == 0 {
            5
        } else {
            2
        }
    } else {
        0
    })
}
pub fn gross_return(wager: u64, stops: [u8; 3]) -> Option<u64> {
    if !valid_wager(wager) {
        return None;
    }
    wager.checked_mul(multiplier(stops)?)
}
pub fn stops_from_randomness(randomness: &[u8; 32], seed: &[u8; 32]) -> Option<[u8; 3]> {
    let mut out = [0; 3];
    let mut n = 0;
    for block in 0..256u32 {
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(randomness);
        hash.update(seed);
        hash.update(block.to_le_bytes());
        for byte in hash.finalize() {
            if byte < 240 {
                out[n] = byte % 20;
                n += 1;
                if n == 3 {
                    return Some(out);
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_eight_thousand_outcomes_have_exact_96_percent_return() {
        let (mut gross, mut hits, mut jackpots) = (0, 0, 0);
        for a in 0..20 {
            for b in 0..20 {
                for c in 0..20 {
                    let payout = gross_return(1, [a, b, c]).unwrap();
                    gross += payout;
                    hits += u32::from(payout > 0);
                    jackpots += u32::from(payout == 150);
                }
            }
        }
        assert_eq!((gross, hits, jackpots), (7680, 1820, 1));
    }
    #[test]
    fn strips_have_the_published_frequencies() {
        for reel in REELS {
            let mut counts = [0; 6];
            for s in reel {
                counts[s as usize] += 1;
            }
            assert_eq!(counts, [4, 4, 4, 4, 3, 1]);
        }
    }
    #[test]
    fn invalid_stops_and_excess_wagers_are_rejected() {
        assert_eq!(multiplier([20, 0, 0]), None);
        assert_eq!(gross_return(0, [0, 0, 0]), None);
        assert_eq!(gross_return(u64::MAX / 150 + 1, [9, 12, 14]), None);
        assert_eq!(
            gross_return(MAX_WAGER, [9, 12, 14]),
            Some(15_000_000 * SDICE_UNIT)
        );
        assert_eq!(gross_return(MAX_WAGER + 1, [9, 12, 14]), None);
    }
    #[test]
    fn every_accepted_byte_has_the_same_number_of_mappings() {
        let mut counts = [0; 20];
        for byte in 0u8..240 {
            counts[(byte % 20) as usize] += 1;
        }
        assert_eq!(counts, [12; 20]);
    }
    #[test]
    fn shared_typescript_vectors() {
        for line in include_str!("../vectors.txt").lines() {
            let values: Vec<u8> = line.split(',').map(|s| s.parse().unwrap()).collect();
            assert_eq!(
                stops_from_randomness(&[values[0]; 32], &[values[1]; 32]),
                Some([values[2], values[3], values[4]])
            );
        }
    }
    #[test]
    fn rewards_preserve_player_return_and_leave_one_percent_expected_bank_margin() {
        for wager in [100 * SDICE_UNIT, MAX_WAGER] {
            let f = rewards(wager).unwrap();
            assert_eq!(f.direct, wager / 100);
            assert_eq!(f.direct, f.staking);
            assert_eq!(f.direct, f.tree);
            let mut gross = 0u128;
            for a in 0..20 {
                for b in 0..20 {
                    for c in 0..20 {
                        gross += u128::from(gross_return(wager, [a, b, c]).unwrap());
                    }
                }
            }
            let turnover = u128::from(wager) * 8000;
            assert_eq!(gross * 100, turnover * 96);
            assert_eq!(
                turnover - gross - u128::from(f.total) * 8000,
                turnover / 100
            );
        }
        for wager in 1..1000 {
            let f = rewards(wager).unwrap();
            assert_eq!(f.total, wager * 3 / 100);
            assert_eq!(f.direct + f.tree + f.staking, f.total);
        }
        assert_eq!(rewards(0), None);
        assert_eq!(rewards(MAX_WAGER + 1), None);
    }
    #[test]
    fn bank_reserves_jackpot_plus_rewards_and_preserves_other_games() {
        let other = 400_000 * SDICE_UNIT;
        let vault = other + 14_903_000 * SDICE_UNIT;
        let liability = liability(MAX_WAGER).unwrap();
        assert_eq!(liability, 15_003_000 * SDICE_UNIT);
        assert_eq!(
            reserve_spin(vault, other, MAX_WAGER),
            Some(other + liability)
        );
        assert_eq!(reserve_spin(vault - 1, other, MAX_WAGER), None);
        assert_eq!(reserve_spin(vault, vault + 1, 1), None);
        assert_eq!(reserve_spin(u64::MAX, 0, 1), None);
        assert_eq!(
            vault + MAX_WAGER
                - gross_return(MAX_WAGER, [9, 12, 14]).unwrap()
                - rewards(MAX_WAGER).unwrap().total,
            other
        );
    }
}
