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
