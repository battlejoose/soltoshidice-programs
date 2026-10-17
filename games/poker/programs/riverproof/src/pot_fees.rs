//! Fee arithmetic at the native table's whole-SDICE accounting boundary.
//! Uncalled excess is a refund, not a pot win. The engine's award records
//! include both, so charging every returned chip would overcharge players.
use riverproof_engine::{pots::seats_of, MAX_SEATS};

const TOKEN_SCALE: u64 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeeError { Arithmetic, AwardsNotConserved, RefundNotReturned }

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct PotFee {
    pub award_chips: u64,
    pub uncalled_chips: u64,
    pub fee_chips: u64,
    pub net_chips: u64,
    /// All four destinations use Token-2022 base units, preserving fractional
    /// SDICE within each quarter even though playing chips are whole SDICE.
    pub bank_base_units: u64,
    pub staking_base_units: u64,
    pub direct_base_units: u64,
    pub tree_base_units: u64,
}

/// Called once per completed, non-void hand, before payouts become withdrawable.
/// Awards must be aggregated by seat, including every side pot and odd chip.
/// The total rate is 1%, rounded DOWN to whole chips per winner; never 1% for
/// each destination. The existing engine does not support fractional stacks.
pub fn pot_fees(
    contributions: &[u64; MAX_SEATS],
    folded: u16,
    awards: &[u64; MAX_SEATS],
) -> Result<[PotFee; MAX_SEATS], FeeError> {
    pot_fees_with_absent(contributions, folded, awards, 0, 0)
}

pub fn pot_fees_with_absent(
    contributions: &[u64; MAX_SEATS], folded: u16,
    awards: &[u64; MAX_SEATS], absent: u16, live: u16,
) -> Result<[PotFee; MAX_SEATS], FeeError> {
    let sum = |values: &[u64; MAX_SEATS]| values.iter().try_fold(0u64,
        |total, &n| total.checked_add(n).ok_or(FeeError::Arithmetic));
    let total = sum(contributions)?;
    if sum(awards)? != total { return Err(FeeError::AwardsNotConserved); }
    // This also bounds build_side_pots' intermediate arithmetic by `total`.
    total.checked_mul(TOKEN_SCALE).ok_or(FeeError::Arithmetic)?;
    let mut refunds = [0u64; MAX_SEATS];
    for pot in riverproof_engine::pots::build_side_pots_with_absent(contributions, folded, absent, live).as_slice() {
        if pot.refund {
            let seat = seats_of(pot.contributors).next().ok_or(FeeError::RefundNotReturned)?;
            refunds[usize::from(seat)] = refunds[usize::from(seat)]
                .checked_add(pot.amount).ok_or(FeeError::Arithmetic)?;
        }
    }
    let mut result = [PotFee::default(); MAX_SEATS];
    for seat in 0..MAX_SEATS {
        let taxable = awards[seat].checked_sub(refunds[seat]).ok_or(FeeError::RefundNotReturned)?;
        let fee = taxable / 100;
        let base = fee.checked_mul(TOKEN_SCALE).ok_or(FeeError::Arithmetic)?;
        let quarter = base / 4;
        result[seat] = PotFee {
            award_chips: awards[seat], uncalled_chips: refunds[seat],
            fee_chips: fee, net_chips: awards[seat] - fee,
            bank_base_units: base - 3 * quarter,
            staking_base_units: quarter, direct_base_units: quarter, tree_base_units: quarter,
        };
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seats(values: &[u64]) -> [u64; MAX_SEATS] {
        let mut result = [0; MAX_SEATS]; result[..values.len()].copy_from_slice(values); result
    }
    #[test]
    fn an_uncalled_raise_is_refunded_without_rake() {
        let fees = pot_fees(&seats(&[1000, 100, 100]), 0b110, &seats(&[1200])).unwrap();
        let winner = fees[0];
        assert_eq!(winner.uncalled_chips, 900);
        assert_eq!(winner.fee_chips, 3);
        assert_eq!(winner.net_chips, 1197);
        assert_eq!(winner.bank_base_units, 750_000);
        assert_eq!(winner.staking_base_units, 750_000);
        assert_eq!(winner.direct_base_units, 750_000);
        assert_eq!(winner.tree_base_units, 750_000);
    }
    #[test]
    fn split_pot_and_side_pot_awards_keep_the_total_fee_at_one_percent() {
        let fees = pot_fees(&seats(&[100, 500, 500]), 0, &seats(&[300, 800])).unwrap();
        assert_eq!(fees[0].fee_chips, 3);
        assert_eq!(fees[1].fee_chips, 8);
        assert_eq!(fees.iter().map(|f| f.fee_chips + f.net_chips).sum::<u64>(), 1100);
        let split = pot_fees(&seats(&[200, 200, 200]), 0, &seats(&[300, 300])).unwrap();
        assert_eq!(split.iter().map(|f| f.fee_chips).sum::<u64>(), 6);
        assert_eq!(split.iter().map(|f| f.bank_base_units).sum::<u64>(), 1_500_000);
    }
    #[test]
    fn whole_chip_rounding_cannot_take_more_than_one_percent() {
        for contribution in 1..=1000 {
            let amount = contribution * 3;
            let fees = pot_fees(&seats(&[contribution; 3]), 0b110, &seats(&[amount])).unwrap();
            let f = fees[0];
            assert!(f.fee_chips * 100 <= amount);
            assert!(amount - f.fee_chips * 100 < 100);
            assert_eq!(f.net_chips + f.fee_chips, amount);
            assert_eq!(f.bank_base_units + f.staking_base_units + f.direct_base_units + f.tree_base_units, f.fee_chips * TOKEN_SCALE);
        }
    }
    #[test]
    fn inconsistent_or_overflowing_native_results_fail_closed() {
        assert_eq!(pot_fees(&seats(&[100, 100]), 0, &seats(&[201])), Err(FeeError::AwardsNotConserved));
        assert_eq!(pot_fees(&seats(&[1000, 100]), 0, &seats(&[100, 1000])), Err(FeeError::RefundNotReturned));
        assert_eq!(pot_fees(&seats(&[u64::MAX, 1]), 0, &seats(&[u64::MAX])), Err(FeeError::Arithmetic));
    }
}
