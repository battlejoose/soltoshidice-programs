//! Paused, graceful ER exit. This never restores a stale snapshot or rolls back
//! trades. Base withdrawals require DLP to have returned the canonical account.
use crate::*;
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder,MagicIntentBundleBuilder};

pub fn idle(trader:&ErTrader)->Result<()> {
    require!(trader.notional==0 && trader.slots.iter().all(|s|s.state==ER_FREE)
        && trader.core_stake.tokens==0,MixError::Closed);
    Ok(())
}
pub fn release(ctx:Context<ReleaseLegacyTrader>)->Result<()> {
    require!(ctx.accounts.config.paused,MixError::Paused);
    let bytes=ctx.accounts.er_trader.try_borrow_data()?;
    require!(bytes.len()==8+ErTrader::INIT_SPACE,MixError::Binding);
    let trader=Box::new(ErTrader::try_deserialize(&mut bytes.as_ref())?);
    require!(trader.owner==ctx.accounts.owner.key() && trader.config==ctx.accounts.config.key()
        && trader.pool==crate::venue::pool_address(ctx.accounts.config.key()),MixError::Binding);
    idle(&trader)?;drop(bytes);
    MagicIntentBundleBuilder::new(ctx.accounts.er_fee_payer.to_account_info(),ctx.accounts.magic_context.to_account_info(),ctx.accounts.magic_program.to_account_info())
        .magic_fee_vault(ctx.accounts.magic_fee_vault.to_account_info())
        .commit_and_undelegate(&[ctx.accounts.er_trader.to_account_info()])
        .build_and_invoke_signed(&[&[b"er-fee-payer",&[ctx.accounts.er_fee_payer.bump]]])?;
    Ok(())
}

/// A request made on Solana asks the validator to commit and return the current
/// account. It does not authorize timeout rollback or make custody spendable.
pub fn request_release(ctx:Context<RequestCoreUndelegation>)->Result<()> {
    let a=&ctx.accounts;
    require!(a.config.paused,MixError::Paused);
    let bytes=a.er_trader.try_borrow_data()?;
    require!(bytes.len()==8+ErTrader::INIT_SPACE,MixError::Binding);
    let trader=Box::new(ErTrader::try_deserialize(&mut bytes.as_ref())?);
    require!(trader.owner==a.owner.key() && trader.config==a.config.key()
        && trader.pool==crate::venue::pool_address(a.config.key()),MixError::Binding);
    // Require an already idle committed checkpoint. The returned account is
    // checked again before any withdrawal; newer open trades cannot be skipped.
    idle(&trader)?;drop(bytes);
    let ix=ephemeral_rollups_sdk::instruction_builder::request_undelegation(a.payer.key(),a.er_trader.key(),crate::ID);
    let (n,bump,owner)=(crate::venue::ns(a.config.key()),[ctx.bumps.er_trader],a.owner.key());
    let seeds=crate::venue::ns_seeds(b"er-trader",&n,&[owner.as_ref(),&bump]);
    anchor_lang::solana_program::program::invoke_signed(&ix,&[
        a.payer.to_account_info(),a.er_trader.to_account_info(),a.owner_program.to_account_info(),
        a.undelegation_request.to_account_info(),a.delegation_record.to_account_info(),
        a.delegation_metadata.to_account_info(),a.system_program.to_account_info(),a.delegation_program.to_account_info(),
    ],&[&seeds])?;
    Ok(())
}

/// Reconcile monotone base credits after a *completed* undelegation. Requiring
/// equality with custody rules out skipping an unsettled loss or reservation.
pub fn debit(trader:&mut ErTrader,custody:&mut ErCustody,asset:Pubkey,amount:u64)->Result<()> {
    require!(amount>0,MixError::Amount);
    require!(trader.owner==custody.owner&&trader.config==custody.config&&trader.pool==custody.pool,MixError::Binding);
    idle(trader)?;
    require!(trader.core_stake.acknowledged_debits==custody.core_coverage.debited_tokens,MixError::Binding);
    #[cfg(feature="mainnet")]
    require!(custody.generation>=trader.custody_generation,MixError::Replay);
    // No live/pending slot or backing remains, so reconciliation cannot alter
    // market exposure. Use the same generation/replay checks as the ER path.
    let mut idle_risk=ErRisk{config:trader.config,..ErRisk::default()};
    sync_custody(trader,&mut idle_risk,custody)?;
    let c=custody.assets.iter_mut().find(|a|a.asset==asset).ok_or(MixError::Inventory)?;
    let index=trader.asset_index(asset)?;
    let t=&mut trader.assets[index];
    require!(t.available==c.available,MixError::Binding);
    let remaining=t.available.checked_sub(amount).ok_or(MixError::Inventory)?;
    t.available=remaining;
    c.available=remaining;
    Ok(())
}
pub fn withdraw(mut ctx:Context<WithdrawReleasedCollateral>,amount:u64)->Result<()> {
    let a=&mut ctx.accounts;
    require!(a.config.paused,MixError::Paused);
    debit(&mut a.trader,&mut a.custody,a.asset.key(),amount)?;
    a.inventory.locked=a.inventory.locked.checked_sub(amount).ok_or(MixError::Inventory)?;
    pay(&a.pool,&a.vault,&a.destination,&a.mint,&a.token_program,&a.authority,amount)?;
    Ok(())
}
#[derive(Accounts)]
pub struct WithdrawReleasedCollateral<'info> {
    pub caller:Signer<'info>,
    /// CHECK: Bound to the canonical trader/custody and token destination. A
    /// keeper can pay for recovery but can only return funds to this owner.
    pub owner:UncheckedAccount<'info>,
    pub config:Box<Account<'info,Config>>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)] pub pool:Box<Account<'info,Pool>>,
    // Account<T> enforces this program's ownership: a still-delegated account,
    // even with an idle-looking old base copy, cannot pass this constraint.
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump=trader.bump,has_one=owner,has_one=config,has_one=pool)] pub trader:Box<Account<'info,ErTrader>>,
    #[account(mut,seeds=[b"er-custody",vns!(config.key()),owner.key().as_ref()],bump=custody.bump,has_one=owner,has_one=config,has_one=pool)] pub custody:Box<Account<'info,ErCustody>>,
    pub asset:Box<Account<'info,Asset>>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=asset.token_program)] pub token_program:Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump,has_one=pool,has_one=asset)] pub inventory:Box<Account<'info,Inventory>>,
    /// CHECK: Canonical vault authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)] pub vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,token::mint=mint,token::authority=owner,token::token_program=token_program)] pub destination:Box<InterfaceAccount<'info,TokenAccount>>,
    pub execution:BaseExecution<'info>,
}
