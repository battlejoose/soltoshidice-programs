//! USD exposure limits; token quantity is never substituted for dollar notional.
use crate::*;
pub const USD: u64 = 1_000_000;
pub const MAX_NOTIONAL: u64 = if cfg!(feature="mainnet") {
    crate::mainnet_config::MAX_WALLET_NOTIONAL_USD
} else { 10_000_000 * USD };
pub const MAX_POSITION_NOTIONAL: u64 = if cfg!(feature="mainnet") {
    crate::deployment_config::MAX_POSITION_NOTIONAL_USD
} else { 10_000_000 * USD };
pub const OI_HEADROOM: u64 = if cfg!(feature="mainnet") {
    crate::deployment_config::MARKET_HEADROOM_USD
} else { 5_000_000 * USD };
pub const REFRESH_SECONDS: i64 = if cfg!(feature="mainnet") {crate::deployment_config::CAP_REFRESH_SECONDS as i64} else {30};
pub const ROLLING_CAPACITY: bool = !cfg!(feature="mainnet") || crate::deployment_config::ROLLING_CAPACITY;
pub const HYSTERESIS_BPS: u64 = if cfg!(feature="mainnet") {crate::deployment_config::CAP_HYSTERESIS_BPS} else {1000};
pub const MIN_ENTRY: u64 = if cfg!(feature="mainnet") {crate::deployment_config::MIN_ENTRY_USD} else {USD};
pub const MAX_LEVERAGE: u64 = if cfg!(feature="mainnet") {crate::deployment_config::MAX_LEVERAGE} else {1000};

/// Per-position AND combined per-owner/venue notional admission limit.
/// Pool value is gross executable payout inventory, before admitting the bet.
/// A falling limit only restricts new entries: it never closes existing trades.
pub fn pool_exposure_limit(pool_usd: u64) -> u64 {
    let low_pool = 100_000 * USD;
    let high_pool = 1_000_000 * USD;
    let low_limit = 1_000_000 * USD;
    let high_limit = 10_000_000 * USD;
    if pool_usd <= low_pool { return low_limit; }
    if pool_usd >= high_pool { return high_limit; }
    low_limit + ((pool_usd - low_pool) as u128 * (high_limit - low_limit) as u128
        / (high_pool - low_pool) as u128) as u64
}

pub fn check_pool_entry(value:u64, leverage:u16, existing:u64, pool_usd:u64) -> Result<u64> {
    let notional = check_entry(value, leverage, existing)?;
    let limit = pool_exposure_limit(pool_usd);
    require!(notional <= limit && existing.checked_add(notional).ok_or(MixError::Arithmetic)? <= limit,
        MixError::Exposure);
    Ok(notional)
}

pub fn check_entry(value: u64, leverage: u16, existing: u64) -> Result<u64> {
    require!(value >= MIN_ENTRY && leverage > 0 && leverage as u64 <= MAX_LEVERAGE, MixError::Exposure);
    #[cfg(feature="mainnet")]
    require!(value <= crate::mainnet_config::MAX_ENTRY_USD, MixError::Exposure);
    let notional = value.checked_mul(leverage as u64).ok_or(MixError::Arithmetic)?;
    require!(notional <= MAX_POSITION_NOTIONAL && existing.checked_add(notional).ok_or(MixError::Arithmetic)? <= MAX_NOTIONAL, MixError::Exposure);
    Ok(notional)
}
pub fn refreshed_cap(oi: u64, cap: u64) -> Result<u64> {
    // Only the explicit historical private policy has a fixed directional cap.
    if !ROLLING_CAPACITY { return Ok(OI_HEADROOM); }
    let room = cap.saturating_sub(oi);
    let lower = (OI_HEADROOM as u128 * (10_000 - HYSTERESIS_BPS) as u128 / 10_000) as u64;
    let upper = (OI_HEADROOM as u128 * (10_000 + HYSTERESIS_BPS) as u128 / 10_000) as u64;
    if room < lower || room > upper {
        oi.checked_add(OI_HEADROOM).ok_or_else(|| error!(MixError::Arithmetic))
    } else { Ok(cap) }
}

#[account]
#[derive(InitSpace, Default)]
pub struct UserExposure {
    pub owner: Pubkey,
    pub notional: u64,
    pub bump: u8,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pool_limits_clamp_and_interpolate_without_overflow() {
        for (pool, limit) in [(0,1_000_000),(99_999,1_000_000),(100_000,1_000_000),
            (250_000,2_500_000),(550_000,5_500_000),(1_000_000,10_000_000),(9_000_000,10_000_000)] {
            assert_eq!(pool_exposure_limit(pool * USD),limit * USD);
        }
        assert_eq!(pool_exposure_limit(100_000 * USD + 1),1_000_000 * USD + 10);
        assert_eq!(pool_exposure_limit(u64::MAX),10_000_000 * USD);
    }
    #[test]
    fn pool_limit_counts_existing_exposure_and_blocks_only_new_entries() {
        assert_eq!(check_pool_entry(1_000 * USD,1000,0,0).unwrap(),1_000_000 * USD);
        assert!(check_pool_entry(1_000 * USD + 1,1000,0,0).is_err());
        assert_eq!(check_pool_entry(100 * USD,1000,900_000 * USD,0).unwrap(),100_000 * USD);
        assert!(check_pool_entry(100 * USD + 1,1000,900_000 * USD,0).is_err());
        assert!(check_pool_entry(USD,1,2_000_000 * USD,0).is_err());
        assert_eq!(check_pool_entry(100 * USD,1000,5_400_000 * USD,550_000 * USD).unwrap(),100_000 * USD);
        assert!(check_pool_entry(USD,1000,u64::MAX,1_000_000 * USD).is_err());
    }
    #[test]
    fn caps_dollars_at_entry_and_combines_all_markets() {
        assert_eq!(check_entry(MAX_NOTIONAL/1000,1000,0).unwrap(),MAX_NOTIONAL);
        assert!(check_entry(MAX_NOTIONAL/1000+USD,1000,0).is_err());
        assert!(check_entry(USD,1000,MAX_NOTIONAL).is_err());
        assert!(check_entry(USD-1,100,0).is_err());
        if !ROLLING_CAPACITY { assert!(check_entry(MAX_NOTIONAL,1,0).is_err()); }
        else { assert_eq!(check_entry(MAX_NOTIONAL,1,0).unwrap(),MAX_NOTIONAL); }
    }
    #[test]
    fn headroom_rebalances_only_outside_original_hysteresis_band() {
        if !ROLLING_CAPACITY {
            assert_eq!(refreshed_cap(OI_HEADROOM,OI_HEADROOM).unwrap(),OI_HEADROOM);
            assert_eq!(refreshed_cap(0,u64::MAX).unwrap(),OI_HEADROOM);
            return;
        }
        assert_eq!(refreshed_cap(500_000*USD,OI_HEADROOM).unwrap(),OI_HEADROOM);
        assert_eq!(refreshed_cap(500_001*USD,OI_HEADROOM).unwrap(),5_500_001*USD);
        assert_eq!(refreshed_cap(0,6_000_000*USD).unwrap(),OI_HEADROOM);
    }
    #[test]
    fn production_accepts_large_entries_but_rejects_combined_exposure_and_overflow() {
        if !ROLLING_CAPACITY {return;}
        assert_eq!(check_entry(10_000*USD,1000,0).unwrap(),10_000_000*USD);
        assert!(check_entry(10_000*USD+1,1000,0).is_err());
        assert!(check_entry(100_000*USD,100,0).is_ok());
        assert!(check_entry(101*USD,1000,0).is_ok());
        assert!(check_entry(USD,1000,MAX_NOTIONAL-999*USD).is_err());
        assert!(check_entry(u64::MAX,1000,0).is_err());
        assert!(refreshed_cap(u64::MAX,0).is_err());
        assert_eq!(refreshed_cap(0,100_000*USD).unwrap(),OI_HEADROOM);
    }
}
