//! Irreversible primary-token contribution to the root payout pool.
//! Owner consent and token-source authority are separate so a future bank CPI
//! can debit an authorized stake atomically and award its owner, not the bank.
use crate::*;

#[account]
#[derive(InitSpace, Default)]
pub struct ContributionLedger {
    pub pool:Pubkey,
    pub owner:Pubkey,
    pub next_nonce:u64,
    pub contributed:u128,
    pub issued:u128,
    pub bump:u8,
}
pub fn reward(amount:u64,pool_usd:u64,queued:bool)->Result<u128> {
    // Twice the equivalent loss reward, including its FIFO bonus.
    emissions::issue(amount,0,pool_usd as i128,0,0)?.0.checked_mul(if queued {4}else{2}).ok_or_else(||error!(MixError::Arithmetic))
}
pub fn contribute(ctx:Context<ContributePool>,amount:u64,nonce:u64,min_issued:u128,expires:i64)->Result<()> {
    let a=ctx.accounts;let now=Clock::get()?.unix_timestamp;
    require!(amount>0 && min_issued>0,MixError::Amount);
    require!(expires>=now && expires<=now.saturating_add(120),MixError::QuoteExpired);
    require!(!a.config.paused,MixError::Paused);
    require_keys_eq!(a.source.owner,a.funding_authority.key(),MixError::Unauthorized);
    require!(a.source.key()!=a.vault.key(),MixError::Binding);
    if a.ledger.owner==Pubkey::default() {
        a.ledger.pool=a.pool.key();a.ledger.owner=a.owner.key();a.ledger.bump=ctx.bumps.ledger;
    }
    require!(a.ledger.owner==a.owner.key() && a.ledger.pool==a.pool.key() && a.ledger.next_nonce==nonce,MixError::Replay);
    if a.ownership_trader.owner==Pubkey::default() {
        a.ownership_trader.pool=a.pool.key();a.ownership_trader.owner=a.owner.key();
        a.ownership_trader.bump=ctx.bumps.ownership_trader;
    }
    require!(a.ownership_trader.owner==a.owner.key() && a.ownership_trader.pool==a.pool.key(),MixError::Binding);
    let pool_usd=pump::spot_value(a.inventory.treasury,&a.asset,&a.valuation)?;
    let queued=math::queue_pending(a.pool.queue_usd,a.pool.queue_head,a.pool.queue_tail)?;
    let issued=reward(amount,pool_usd,queued)?;
    require!(issued>=min_issued,MixError::PriceLimit);
    let before=a.vault.amount;let source_before=a.source.amount;
    token_interface::transfer_checked(CpiContext::new(a.token_program.key(),TransferChecked {
        from:a.source.to_account_info(),mint:a.mint.to_account_info(),to:a.vault.to_account_info(),authority:a.funding_authority.to_account_info()
    }),amount,a.mint.decimals)?;
    a.vault.reload()?;a.source.reload()?;
    require!(a.vault.amount==before.checked_add(amount).ok_or(MixError::Arithmetic)? &&
        a.source.amount==source_before.checked_sub(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    a.inventory.treasury=a.inventory.treasury.checked_add(amount).ok_or(MixError::Arithmetic)?;
    a.ownership_pool.supply=a.ownership_pool.supply.checked_add(issued).ok_or(MixError::Arithmetic)?;
    a.ownership_trader.available=a.ownership_trader.available.checked_add(issued).ok_or(MixError::Arithmetic)?;
    a.ownership_trader.issued=a.ownership_trader.issued.checked_add(issued).ok_or(MixError::Arithmetic)?;
    a.ledger.next_nonce=a.ledger.next_nonce.checked_add(1).ok_or(MixError::Arithmetic)?;
    a.ledger.contributed=a.ledger.contributed.checked_add(amount as u128).ok_or(MixError::Arithmetic)?;
    a.ledger.issued=a.ledger.issued.checked_add(issued).ok_or(MixError::Arithmetic)?;
    emit!(PoolContributed{pool:a.pool.key(),owner:a.owner.key(),funding_authority:a.funding_authority.key(),
        source:a.source.key(),nonce,amount,pool_usd_before:pool_usd,issued});
    Ok(())
}
#[derive(Accounts)]
pub struct ContributePool<'info> {
    #[account(mut)] pub payer:Signer<'info>,
    /// Beneficiary explicitly signs even when a bank PDA supplies the tokens.
    pub owner:Signer<'info>,
    pub funding_authority:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump)] pub config:Box<Account<'info,Config>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=pool.bump)] pub pool:Box<Account<'info,Pool>>,
    #[account(address=pool.settlement)] pub asset:Box<Account<'info,Asset>>,
    /// CHECK: Canonical payout-vault authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=asset.token_program)] pub token_program:Interface<'info,TokenInterface>,
    #[account(mut,token::mint=mint,token::authority=funding_authority,token::token_program=token_program)]
    pub source:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump,has_one=pool,has_one=asset)]
    pub inventory:Box<Account<'info,Inventory>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)]
    pub ownership_pool:Box<Account<'info,OwnershipPool>>,
    #[account(init_if_needed,payer=payer,space=8+OwnershipTrader::INIT_SPACE,seeds=[b"ownership-trader",pool.key().as_ref(),owner.key().as_ref()],bump)]
    pub ownership_trader:Box<Account<'info,OwnershipTrader>>,
    #[account(init_if_needed,payer=payer,space=8+ContributionLedger::INIT_SPACE,seeds=[b"pool-contribution",pool.key().as_ref(),owner.key().as_ref()],bump)]
    pub ledger:Box<Account<'info,ContributionLedger>>,
    pub valuation:Valuation<'info>,
    pub execution:BaseExecution<'info>,
    pub system_program:Program<'info,System>,
}
#[event]
pub struct PoolContributed {pub pool:Pubkey,pub owner:Pubkey,pub funding_authority:Pubkey,pub source:Pubkey,
    pub nonce:u64,pub amount:u64,pub pool_usd_before:u64,pub issued:u128}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn contribution_is_exactly_double_the_full_loss_reward() {
        for pool in [0,100_000*risk::USD,550_000*risk::USD,1_000_000*risk::USD,3_000_000*risk::USD] {
            for amount in [1,100_000_000,u64::MAX] {
                for queued in [false,true] {
                    assert_eq!(reward(amount,pool,queued).unwrap(),if queued {4}else{2}*emissions::issue(amount,0,pool as i128,0,0).unwrap().0);
                }
            }
        }
        assert_eq!(reward(100_000_000,0,false).unwrap(),20_000_000_000);
        assert_eq!(reward(100_000_000,0,true).unwrap(),40_000_000_000);
    }
}
