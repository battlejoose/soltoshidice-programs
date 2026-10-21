//! Venue-local admission observations. The existing collateral publisher attests
//! base payout-inventory value; traders cannot supply or refresh these values.
//! Missing observations allow only the minimum cap. Stale supplied observations
//! reject new entries, never exits or liquidation monitoring.
use crate::*;
use ephemeral_rollups_sdk::anchor::delegate;
use ephemeral_rollups_sdk::cpi::DelegateConfig;

pub const MAX_AGE: i64 = 15;
#[account]
#[derive(InitSpace, Default)]
pub struct PoolExposure {
    pub config: Pubkey,
    pub pool: Pubkey,
    pub usd: u64,
    pub source_slot: u64,
    pub observed_at: i64,
    pub bump: u8,
}
impl PoolExposure {
    pub fn publish(&mut self, usd:u64, source_slot:u64, observed_at:i64, now:i64)->Result<()> {
        require!(source_slot>0 && observed_at>0 && observed_at<=now && now-observed_at<=MAX_AGE,MixError::Stale);
        require!(source_slot>=self.source_slot && observed_at>=self.observed_at,MixError::Stale);
        // Re-reading a cached slot cannot refresh its age or change its value.
        if source_slot==self.source_slot {
            require!(usd==self.usd && observed_at==self.observed_at,MixError::Replay);
        }
        self.usd=usd;self.source_slot=source_slot;self.observed_at=observed_at;Ok(())
    }
    pub fn value(&self,now:i64)->Result<u64> {
        require!(self.source_slot>0 && self.observed_at>0 && self.observed_at<=now && now-self.observed_at<=MAX_AGE,MixError::Stale);
        Ok(self.usd)
    }
}
pub fn initialize(ctx:Context<InitializePoolExposure>)->Result<()> {
    *ctx.accounts.pool_exposure=PoolExposure{config:ctx.accounts.config.key(),pool:ctx.accounts.pool.key(),
        bump:ctx.bumps.pool_exposure,..PoolExposure::default()};Ok(())
}
pub fn delegate(ctx:Context<DelegatePoolExposure>)->Result<()> {
    let pool=ctx.accounts.pool.key();
    ctx.accounts.delegate_pool_exposure(&ctx.accounts.publisher,&[b"pool-exposure",pool.as_ref()],
        DelegateConfig{validator:Some(engine::ER_VALIDATOR),commit_frequency_ms:u32::MAX})?;Ok(())
}
pub fn publish(ctx:Context<PublishPoolExposure>,usd:u64,source_slot:u64,observed_at:i64)->Result<()> {
    ctx.accounts.pool_exposure.publish(usd,source_slot,observed_at,Clock::get()?.unix_timestamp)
}
#[derive(Accounts)]
pub struct InitializePoolExposure<'info> {
    #[account(mut)] pub payer:Signer<'info>,
    pub config:Account<'info,Config>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)] pub pool:Account<'info,Pool>,
    #[account(init,payer=payer,space=8+PoolExposure::INIT_SPACE,seeds=[b"pool-exposure",pool.key().as_ref()],bump)]
    pub pool_exposure:Account<'info,PoolExposure>,
    pub system_program:Program<'info,System>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegatePoolExposure<'info> {
    #[account(mut)] pub publisher:Signer<'info>,
    #[account(has_one=publisher)] pub config:Account<'info,Config>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)] pub pool:Account<'info,Pool>,
    /// CHECK: Canonical account; delegation program checks ownership.
    #[account(mut,del,seeds=[b"pool-exposure",pool.key().as_ref()],bump)] pub pool_exposure:UncheckedAccount<'info>,
}
#[derive(Accounts)]
pub struct PublishPoolExposure<'info> {
    pub publisher:Signer<'info>,
    #[account(has_one=publisher)] pub config:Account<'info,Config>,
    #[account(mut,seeds=[b"pool-exposure",pool_exposure.pool.as_ref()],bump=pool_exposure.bump,has_one=config)]
    pub pool_exposure:Account<'info,PoolExposure>,
    pub execution:ErExecution<'info>,
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn old_missing_future_and_retimed_observations_fail_closed() {
        let mut p=PoolExposure::default();assert!(p.value(100).is_err());
        p.publish(250_000*risk::USD,42,100,100).unwrap();
        assert_eq!(p.value(115).unwrap(),250_000*risk::USD);
        assert!(p.value(116).is_err());assert!(p.value(99).is_err());
        assert!(p.publish(0,41,101,101).is_err());
        assert!(p.publish(p.usd,42,101,101).is_err());
        assert!(p.publish(p.usd+1,42,100,101).is_err());
        assert!(p.publish(0,43,103,102).is_err());
        p.publish(0,43,102,102).unwrap();assert_eq!(p.value(102).unwrap(),0);
    }
}
