//! Direct backing-token FIFO payments. USD debt stays in the existing accounts,
//! so existing and partially paid claims retain exactly their unpaid entitlement.
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint,TokenAccount,TokenInterface};
use crate::{Asset,Inventory,Pool,Position,Valuation,MixError,pump,pay,Redeemed};
use crate::{ValuationBumps,__client_accounts_valuation,__cpi_client_accounts_valuation,BaseExecution,BaseExecutionBumps,__client_accounts_base_execution,__cpi_client_accounts_base_execution};

#[derive(Accounts)]
pub struct RedeemProfitTokens<'info> {
    pub caller:Signer<'info>,
    pub execution:BaseExecution<'info>,
    #[account(mut)] pub pool:Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool authority; only the bound token transfer uses it.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority:UncheckedAccount<'info>,
    #[account(mut,has_one=pool)] pub position:Box<Account<'info,Position>>,
    #[account(address=pool.settlement)] pub asset:Box<Account<'info,Asset>>,
    pub valuation:Valuation<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=asset.token_program)] pub token_program:Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump,has_one=pool,has_one=asset)]
    pub inventory:Box<Account<'info,Inventory>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,token::mint=mint,token::token_program=token_program,constraint=destination.owner==position.owner @ MixError::Unauthorized)]
    pub destination:Box<InterfaceAccount<'info,TokenAccount>>,
}
pub fn redeem(ctx:Context<RedeemProfitTokens>)->Result<()> {
    require!(ctx.remaining_accounts.is_empty(),MixError::Binding);
    // A top-level redemption must read prices from reserves which are read-only
    // throughout this transaction. This prevents flash swaps / earlier CPIs
    // from repricing a claim and restoring the reserves atomically afterward.
    require!(anchor_lang::solana_program::instruction::get_stack_height()==1,MixError::Binding);
    let v=&ctx.accounts.valuation;
    for info in v.to_account_infos() {require!(!info.is_writable,MixError::Binding);}
    let a=ctx.accounts;let x=&mut a.position;let pool=&mut a.pool;
    require!(x.state!=0 && x.remaining>0,MixError::NoClaim);
    require!(x.queue_seq==pool.queue_head,MixError::QueueOrder);
    let (amount,debit)=pump::spot_curve(&a.asset,&a.valuation)?.payout(x.remaining,a.inventory.treasury)?;
    pay(pool,&a.vault,&a.destination,&a.mint,&a.token_program,&a.authority,amount)?;
    a.inventory.treasury=a.inventory.treasury.checked_sub(amount).ok_or(MixError::Arithmetic)?;
    a.inventory.paid=a.inventory.paid.checked_add(amount).ok_or(MixError::Arithmetic)?;
    x.remaining=x.remaining.checked_sub(debit).ok_or(MixError::Arithmetic)?;
    pool.queue_usd=pool.queue_usd.checked_sub(debit).ok_or(MixError::Arithmetic)?;
    if x.remaining==0 {pool.queue_head=pool.queue_head.checked_add(1).ok_or(MixError::Arithmetic)?;}
    // Existing event layout remains readable. The instruction name identifies
    // direct-token receipts; converted_out=0 never claims USDC was transferred.
    emit!(Redeemed{position:x.key(),asset:a.asset.key(),amount,debit_usd:debit,converted_out:0});
    Ok(())
}
