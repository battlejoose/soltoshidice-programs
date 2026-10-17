//! Currency-aware collateral observations for the mainnet deployment.
//! Pool state and the quote-to-USD conversion are attested by the publisher.
//! Observation numbers are journal sequences, not slots from either chain.
use anchor_lang::prelude::*;
use crate::{engine::*, mainnet_pump::SolPoolQuote, oracle, MixError};

/// The existing publisher signs this price in the settlement transaction itself.
/// Pool reserves are read on chain; no separate base price-update transaction is
/// needed. This preserves the same SOL/USD convention used for ER entry quotes.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct SolUsdQuote { pub price: u64, pub publish_time: i64 }
impl SolUsdQuote {
    pub fn validate(&self, now: i64) -> Result<()> {
        require!(self.price > 0 && self.price <= 1_000_000_000_000_000_000, MixError::Quote);
        require!(self.publish_time > 0 && self.publish_time <= now.saturating_add(2)
            && now.saturating_sub(self.publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
        Ok(())
    }
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq, Eq)]
pub struct MainnetCollateralContext {
    pub real_quote_reserve: u64,
    pub pool_source_slot: u64,
    /// USD per quote-token atom, scaled by 1e21. For SOL this equals the old
    /// SOL/USD e12 field exactly; for USDC it is 1e15. Layout and existing bytes
    /// are unchanged. Other quote tokens need an executable conversion route;
    /// a spot multiplier must not replace a multi-pool collateral sale.
    pub usd_per_quote_atom_e21: u64,
    pub conversion_publish_time: i64,
    pub conversion_source_slot: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Debug)]
pub struct MainnetCollateralObservation {
    pub base_reserve: u64,
    /// Real reserve plus the signed virtual adjustment, in quote-token atoms.
    pub quote_reserve: u64,
    pub fee_bps: [u16; 3],
    pub publish_time: i64,
    pub observation: u64,
    pub context: MainnetCollateralContext,
}

impl MainnetCollateralContext {
    /// USD/token in 1e12 units. Decimals normalize token atoms; neither supply
    /// nor historical execution fees enter the relative price-floor formula.
    pub fn spot_price(&self, base:u64, effective:u64, observed_at:i64, decimals:u8) -> Result<u64> {
        self.validate(base,effective,[0;3],observed_at)?;
        require!(decimals<=9, MixError::Quote);
        if base==0 || self.real_quote_reserve==0 { return Ok(0); }
        crate::math::div(effective as u128,self.usd_per_quote_atom_e21 as u128,
            base as u128 * 10u128.pow((9-decimals) as u32))
    }
    pub fn validate(&self, base: u64, effective: u64, fees: [u16; 3], observed_at: i64) -> Result<()> {
        require!(self.pool_source_slot > 0 && self.conversion_source_slot > 0, MixError::Quote);
        require!(self.usd_per_quote_atom_e21 > 0 && self.usd_per_quote_atom_e21 <= 1_000_000_000_000_000_000, MixError::Quote);
        // Backlogs keep their original timestamps. A historical reserve sample
        // must never be multiplied by a future or stale conversion rate.
        require!(self.conversion_publish_time > 0 && self.conversion_publish_time <= observed_at
            && observed_at.saturating_sub(self.conversion_publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
        require!(fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
        if base > 0 && self.real_quote_reserve > 0 { require!(effective > 0, MixError::Quote); }
        Ok(())
    }
    pub fn value(&self, base: u64, effective: u64, fees: [u16; 3], observed_at: i64, amount: u64) -> Result<u64> {
        self.validate(base, effective, fees, observed_at)?;
        let quote = SolPoolQuote { base, real_quote: self.real_quote_reserve,
            virtual_quote: effective as i128 - self.real_quote_reserve as i128, fees };
        crate::mainnet_quote_path::collateral_usd(amount, &[quote], self.usd_per_quote_atom_e21)
    }
}

#[cfg(feature="mainnet")]
impl ErQuote {
    pub fn publish_mainnet(&mut self, next: MainnetCollateralObservation, now: i64) -> Result<()> {
        require!(!self.is_quote && self.asset != sol_collateral_asset(), MixError::Binding);
        next.context.validate(next.base_reserve, next.quote_reserve, next.fee_bps, next.publish_time)?;
        if self.source_slot > 0 {
            require!(next.context.pool_source_slot >= self.mainnet.pool_source_slot
                && next.context.conversion_source_slot >= self.mainnet.conversion_source_slot
                && next.context.conversion_publish_time >= self.mainnet.conversion_publish_time, MixError::Replay);
            if next.context.pool_source_slot == self.mainnet.pool_source_slot {
                require!(next.base_reserve == self.base_reserve && next.quote_reserve == self.quote_reserve
                    && next.fee_bps == self.fee_bps && next.context.real_quote_reserve == self.mainnet.real_quote_reserve, MixError::Replay);
            }
            if next.context.conversion_source_slot == self.mainnet.conversion_source_slot {
                require!(next.context.usd_per_quote_atom_e21 == self.mainnet.usd_per_quote_atom_e21
                    && next.context.conversion_publish_time == self.mainnet.conversion_publish_time, MixError::Replay);
            }
        }
        self.publish(next.base_reserve, next.quote_reserve, next.fee_bps, next.publish_time, next.observation, now)?;
        self.mainnet = next.context;
        Ok(())
    }
}

pub fn publish(ctx: Context<PublishErQuote>, observation: MainnetCollateralObservation) -> Result<()> {
    #[cfg(feature="mainnet")]
    {
        ctx.accounts.er_quote.publish_mainnet(observation, Clock::get()?.unix_timestamp)?;
        ctx.accounts.history.record(&ctx.accounts.er_quote)
    }
    #[cfg(not(feature="mainnet"))]
    { let _ = (ctx, observation); err!(MixError::ProductionDisabled) }
}
