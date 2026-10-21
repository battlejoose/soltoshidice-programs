//! Pool-size rewards. USD uses the existing executable payout-inventory valuation.
use crate::*;
pub const FLAT_THRESHOLD: u64 = 100_000 * risk::USD;
pub const MIN_REWARD_THRESHOLD: u64 = 1_000_000 * risk::USD;
pub const POOL_CAP: u64 = 5_000_000 * risk::USD;
pub const FP: u128 = 1_000_000_000_000;

/// Full rewards mean the existing 100 ownership atoms per contributed token atom.
/// Linear 100% -> 10%, then a 10% floor. Drawdowns restore the rate.
pub fn reward_factor(pool_usd: i128) -> u128 {
    if pool_usd <= FLAT_THRESHOLD as i128 { return FP; }
    if pool_usd >= MIN_REWARD_THRESHOLD as i128 { return FP / 10; }
    FP - (pool_usd - FLAT_THRESHOLD as i128) as u128 * (FP * 9 / 10)
        / (MIN_REWARD_THRESHOLD - FLAT_THRESHOLD) as u128
}

/// Sample the pool immediately before this settlement. Retain the historical
/// high-water field and function ABI; it no longer changes the reward rate.
pub fn issue(tokens: u64, _basis_usd: u64, pool_usd: i128, _advance: u64, hwm: u64) -> Result<(u128,u64)> {
    let issued = (tokens as u128 * 100).checked_mul(reward_factor(pool_usd))
        .ok_or(MixError::Arithmetic)? / FP;
    Ok((issued,hwm))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn full_linear_and_floor_rewards() {
        for (usd, percent) in [(0,100),(99_999,100),(100_000,100),(250_000,85),
            (550_000,55),(750_000,35),(1_000_000,10),(2_000_001,10)] {
            assert_eq!(issue(1_000_000,0,usd*risk::USD as i128,0,123).unwrap(),
                (1_000_000*percent,123));
        }
        assert_eq!(reward_factor(FLAT_THRESHOLD as i128+1),FP-1);
        assert_eq!(reward_factor(MIN_REWARD_THRESHOLD as i128-1),FP/10+1);
    }
    #[test]
    fn current_value_not_old_highwater_or_contribution_controls_rate() {
        assert_eq!(issue(1_000_000,u64::MAX,0,u64::MAX,u64::MAX).unwrap(),(100_000_000,u64::MAX));
        assert_eq!(issue(1_000_000,0,-1,0,0).unwrap().0,100_000_000);
        assert_eq!(issue(0,0,i128::MAX,0,7).unwrap(),(0,7));
        assert_eq!(issue(u64::MAX,0,0,0,0).unwrap().0,u64::MAX as u128*100);
        assert_eq!(issue(u64::MAX,0,i128::MAX,0,0).unwrap().0,u64::MAX as u128*10);
    }
}
