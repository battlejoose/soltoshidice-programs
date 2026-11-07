//! USDC payouts with pool-local pPERPS ownership and purchased PERPS rewards.
//! A fresh namespace keeps every legacy USDC/SDICE balance and account intact.
use crate::*;

pub const MINT: Pubkey = pubkey!("51H7AzyyUAy6AkTPa2g1AMyfkoEoVXZjAKkqyHTopump");
pub const OPERATIONS: Pubkey = pubkey!("ELczEiAWnZ4WimPXBoArTeiq4c4x7Y52ud49tSr9q9R5");
pub const OPERATIONS_SECONDARY: Pubkey = pubkey!("8bCzV1vbWcGBVfdj85YJXtnuQTbJ6fBREsYieaiRPjTm");
pub const SEED: &[u8] = b"usdc-perps-v1";
pub fn config_address() -> Pubkey { Pubkey::find_program_address(&[b"config",SEED],&crate::ID).0 }
pub fn pool_address() -> Pubkey { venue::pool_address(config_address()) }
pub fn is_pool(pool: Pubkey) -> bool { pool == pool_address() }

#[account]
#[derive(InitSpace,Default)]
pub struct PerpsRevenue {
    pub pool: Pubkey,
    /// Reserved USDC already removed from payout inventory, awaiting conversion.
    pub rewards_usdc: u64,
    pub operations_usdc: u64,
    pub rewards_spent: u64,
    pub operations_spent: u64,
    pub perps_received: u64,
    pub operations_sol: u64,
    pub bump: u8,
    pub burn_usdc: u64,
    pub burn_spent: u64,
    pub perps_burned: u64,
}
impl PerpsRevenue {
    /// Reserve 5/2/1 or 50/20/5 for rewards/operations/PERPS burn. Lock the
    /// allocation at settlement, so later tier changes cannot relabel funds.
    pub fn reserve(&mut self,usdc:u64,pool_usd:u64)->Result<()> {
        let high=pool_usd>math::HIGH_POOL_THRESHOLD;
        let total=if high {75}else{8};
        let operations=(usdc as u128*if high {20}else{2}/total) as u64;
        let burn=(usdc as u128*if high {5}else{1}/total) as u64;
        self.operations_usdc=self.operations_usdc.checked_add(operations).ok_or(MixError::Arithmetic)?;
        self.burn_usdc=self.burn_usdc.checked_add(burn).ok_or(MixError::Arithmetic)?;
        self.rewards_usdc=self.rewards_usdc.checked_add(usdc-operations-burn).ok_or(MixError::Arithmetic)?;
        Ok(())
    }
    pub fn sweep(&mut self,p:&OwnershipPool,inv:&mut Inventory,queued:bool)->Result<u64> {
        if queued || p.total_staked==0 {return Ok(0);}
        let surplus=inv.treasury.saturating_sub(emissions::POOL_CAP);
        self.rewards_usdc=self.rewards_usdc.checked_add(surplus).ok_or(MixError::Arithmetic)?;
        inv.treasury-=surplus;
        Ok(surplus)
    }
}

/// Input retained as USDC, gas, and the remaining SDICE purchase allocation.
pub fn loss_split(loss:u64,pool_usd:u64)->(u64,u64,u64) {
    let gas=loss/100;
    let sdice=(loss as u128*if pool_usd>math::HIGH_POOL_THRESHOLD {14}else{1}/100) as u64;
    (loss-gas-sdice,gas,sdice)
}
/// No USDC burn: reserve the PERPS burn budget until purchase and burn succeed.
pub fn receipt_split(received:u64,pool_usd:u64)->(u64,u64,u64) {
    let high=pool_usd>math::HIGH_POOL_THRESHOLD;
    let reserved=(received as u128*if high {75}else{8}/if high {85}else{98}) as u64;
    (received-reserved,reserved,0)
}
pub fn sdice_quote_split(received:u64,pool_usd:u64)->(u64,u64) {
    let burn=if pool_usd>math::HIGH_POOL_THRESHOLD {(received as u128*4/14) as u64}else{0};
    (received-burn,burn)
}

pub fn create(ctx:Context<CreatePerpsMarket>)->Result<()> {
    require!(cfg!(feature="mainnet"),MixError::Binding);
    require!(native_market::kind(&ctx.accounts.asset)?==Some(native_market::Kind::Usdc),MixError::Binding);
    #[cfg(feature="mainnet")]
    mainnet_config::validate_asset(&ctx.accounts.reward_asset)?;
    require!(ctx.accounts.reward_asset.quote_mint==mainnet_pump::SOL && ctx.accounts.reward_asset.decimals==6, MixError::Binding);
    let c=&mut ctx.accounts.config;
    c.admin=ctx.accounts.root.admin;c.publisher=ctx.accounts.root.publisher;
    c.quote_mint=mainnet_pump::USDC;c.paused=true;c.bump=ctx.bumps.config;c.next_settlement_seq=1;
    let p=&mut ctx.accounts.pool;
    p.id=0;p.settlement=ctx.accounts.asset.key();p.authority_bump=ctx.bumps.authority;p.bump=ctx.bumps.pool;
    let v=&mut ctx.accounts.venue;
    v.config=c.key();v.pool=p.key();v.mint=mainnet_pump::USDC;v.asset=ctx.accounts.asset.key();
    v.creator=ctx.accounts.admin.key();v.created_at=Clock::get()?.unix_timestamp;
    v.pool_bps=9000;v.staker_bps=500;v.burn_bps=0;v.gas_bps=100;v.buyback_bps=100;v.bump=ctx.bumps.venue;
    ctx.accounts.native_revenue.pool=p.key();ctx.accounts.native_revenue.bump=ctx.bumps.native_revenue;
    ctx.accounts.revenue.pool=p.key();ctx.accounts.revenue.bump=ctx.bumps.revenue;
    emit!(VenueCreated{config:c.key(),venue:v.key(),pool:p.key(),mint:v.mint,creator:v.creator});
    Ok(())
}

pub fn stake(ctx:Context<StakePerpsOwnership>,amount:u128,unstake:bool)->Result<()> {
    let dust=ownership::change_stake_balances(&mut ctx.accounts.ownership_pool,&mut ctx.accounts.ownership_trader,amount,unstake)?;
    // PERPS rounding dust stays PERPS; it can never become USDC payout inventory.
    let p=&mut ctx.accounts.ownership_pool;
    p.fee_pending=p.fee_pending.checked_add(dust).ok_or(MixError::Arithmetic)?;
    ownership::distribute(p)
}
pub fn claim(ctx:Context<ClaimPerpsRewards>)->Result<()> {
    let p=&mut ctx.accounts.ownership_pool;let t=&mut ctx.accounts.ownership_trader;
    ownership::distribute(p)?;ownership::accrue(p,t)?;
    let amount=t.pending;require!(amount>0,MixError::NoClaim);
    p.reward_pool=p.reward_pool.checked_sub(amount).ok_or(MixError::Inventory)?;
    p.pending_liabilities=p.pending_liabilities.checked_sub(amount).ok_or(MixError::Inventory)?;
    t.pending=0;t.claimed=t.claimed.checked_add(amount).ok_or(MixError::Arithmetic)?;
    pay(&ctx.accounts.pool,&ctx.accounts.vault,&ctx.accounts.destination,&ctx.accounts.mint,
        &ctx.accounts.token_program,&ctx.accounts.authority,amount)
}
pub fn sweep(ctx:Context<SweepPerpsSurplus>)->Result<()> {
    let p=&ctx.accounts.pool;
    ctx.accounts.revenue.sweep(&ctx.accounts.ownership_pool,&mut ctx.accounts.inventory,
        math::queue_pending(p.queue_usd,p.queue_head,p.queue_tail)?)?;
    Ok(())
}

/// Conversion is deferred to its own atomic transaction, so a slow PERPS route
/// cannot block a trader's settlement. Pending USDC is never claimed as PERPS.
pub fn route<'info>(ctx:Context<'info,RoutePerpsRevenue<'info>>,operations:bool,amount:u64,min_out:u64,sell_count:u8)->Result<()> {
    route_to(ctx,if operations {RevenueKind::Operations}else{RevenueKind::Rewards},amount,min_out,sell_count)
}
pub fn burn_revenue<'info>(ctx:Context<'info,RoutePerpsRevenue<'info>>,amount:u64,min_out:u64,sell_count:u8)->Result<()> {
    route_to(ctx,RevenueKind::Burn,amount,min_out,sell_count)
}
#[derive(Clone,Copy,PartialEq,Eq)]
enum RevenueKind {Rewards,Operations,Burn}
fn route_to<'info>(mut ctx:Context<'info,RoutePerpsRevenue<'info>>,kind:RevenueKind,amount:u64,min_out:u64,sell_count:u8)->Result<()> {
    let a=&mut ctx.accounts;
    let operations=kind==RevenueKind::Operations;
    require!(!math::queue_pending(a.pool.queue_usd,a.pool.queue_head,a.pool.queue_tail)?,MixError::QueueOrder);
    require_keys_eq!(a.target.mint,if operations {mainnet_pump::SOL}else{MINT},MixError::Binding);
    if operations {require!(native_market::kind(&a.target)?==Some(native_market::Kind::Sol),MixError::Binding);}
    else {require!(a.target.decimals==6,MixError::Binding);}
    let pending=match kind {RevenueKind::Operations=>a.revenue.operations_usdc,RevenueKind::Rewards=>a.revenue.rewards_usdc,RevenueKind::Burn=>a.revenue.burn_usdc};
    let amount=if amount==0 {pending}else{amount};
    require!(amount>0 && amount<=pending && min_out>0,MixError::Amount);
    let source_before=pump::token_amount(&a.source.to_account_info())?;
    let before=pump::token_amount(&a.output.to_account_info())?;
    // Unsolicited SOL/WSOL at this deterministic address must not block routing.
    // Any donated balance goes to operations too, outside revenue accounting.
    let received=pump::swap_loss(&a.pool,&a.authority,&a.usdc,&a.target,&a.source,&a.output,&a.quote_vault,
        &a.pump_program,ctx.remaining_accounts,sell_count,amount,min_out)?;
    require!(source_before.checked_sub(pump::token_amount(&a.source.to_account_info())?)==Some(amount)
        && pump::token_amount(&a.output.to_account_info())?.checked_sub(before)==Some(received),MixError::Binding);
    if operations {
        let pool_key=a.pool.key();let seeds:&[&[u8]]=&[b"authority",pool_key.as_ref(),&[a.pool.authority_bump]];
        // Refund account rent to the signing publisher. Forward the measured
        // output plus any unsolicited balance equally to both fixed recipients.
        // Native SOL is indivisible below one lamport; the second receives an
        // odd remainder. Both transfers and the swap succeed or revert together.
        token_interface::close_account(CpiContext::new_with_signer(a.token_program.key(),token_interface::CloseAccount{
            account:a.output.to_account_info(),destination:a.publisher.to_account_info(),authority:a.authority.to_account_info(),
        },&[seeds]))?;
        let total=received.checked_add(before).ok_or(MixError::Arithmetic)?;
        let first=total/2;
        anchor_lang::system_program::transfer(CpiContext::new(a.system_program.key(),anchor_lang::system_program::Transfer{
            from:a.publisher.to_account_info(),to:a.operations.to_account_info(),
        }),first)?;
        anchor_lang::system_program::transfer(CpiContext::new(a.system_program.key(),anchor_lang::system_program::Transfer{
            from:a.publisher.to_account_info(),to:a.operations_secondary.to_account_info(),
        }),total-first)?;
        a.revenue.operations_usdc-=amount;
        a.revenue.operations_spent=a.revenue.operations_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
        a.revenue.operations_sol=a.revenue.operations_sol.checked_add(received).ok_or(MixError::Arithmetic)?;
    } else if kind==RevenueKind::Burn {
        let pool_key=a.pool.key();let seeds:&[&[u8]]=&[b"authority",pool_key.as_ref(),&[a.pool.authority_bump]];
        // Burn only this swap's measured output. Previously purchased staking
        // rewards and unsolicited PERPS in the shared vault remain untouched.
        token_interface::burn(CpiContext::new_with_signer(a.token_program.key(),token_interface::Burn{
            mint:a.mint.to_account_info(),from:a.output.to_account_info(),authority:a.authority.to_account_info(),
        },&[seeds]),received)?;
        require!(pump::token_amount(&a.output.to_account_info())?==before,MixError::Binding);
        a.revenue.burn_usdc-=amount;
        a.revenue.burn_spent=a.revenue.burn_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
        a.revenue.perps_burned=a.revenue.perps_burned.checked_add(received).ok_or(MixError::Arithmetic)?;
    } else {
        a.revenue.rewards_usdc-=amount;
        a.revenue.rewards_spent=a.revenue.rewards_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
        a.revenue.perps_received=a.revenue.perps_received.checked_add(received).ok_or(MixError::Arithmetic)?;
        ownership::fund_rewards(&mut a.ownership_pool,received)?;
    }
    if kind==RevenueKind::Burn {emit!(PerpsRevenueBurned{pool:a.pool.key(),usdc_spent:amount,perps_burned:received});}
    else {emit!(PerpsRevenueDelivered{pool:a.pool.key(),operations,usdc_spent:amount,received});}
    Ok(())
}
#[event]
pub struct PerpsRevenueDelivered {pub pool:Pubkey,pub operations:bool,pub usdc_spent:u64,pub received:u64}
#[event]
pub struct PerpsRevenueBurned {pub pool:Pubkey,pub usdc_spent:u64,pub perps_burned:u64}

#[derive(Accounts)]
pub struct CreatePerpsMarket<'info> {
    #[account(mut)] pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=root.bump,has_one=admin)] pub root:Box<Account<'info,Config>>,
    #[account(seeds=[b"asset",mainnet_pump::USDC.as_ref()],bump=asset.bump)] pub asset:Box<Account<'info,Asset>>,
    #[account(seeds=[b"asset",MINT.as_ref()],bump=reward_asset.bump,constraint=reward_asset.mint==MINT @ MixError::Binding)]
    pub reward_asset:Box<Account<'info,Asset>>,
    #[account(init,payer=admin,space=8+Config::INIT_SPACE,seeds=[b"config",SEED],bump)] pub config:Box<Account<'info,Config>>,
    #[account(init,payer=admin,space=8+Venue::INIT_SPACE,seeds=[b"venue",config.key().as_ref()],bump)] pub venue:Box<Account<'info,Venue>>,
    #[account(init,payer=admin,space=8+Pool::INIT_SPACE,seeds=[b"pool",config.key().as_ref(),&0u16.to_le_bytes()],bump)] pub pool:Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=mainnet_pump::SOL)] pub sol_mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=mainnet_pump::USDC)] pub usdc_mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=anchor_spl::token::ID)] pub token_program:Interface<'info,TokenInterface>,
    #[account(init,payer=admin,seeds=[b"sol-quote",pool.key().as_ref()],bump,token::mint=sol_mint,token::authority=authority,token::token_program=token_program)]
    pub quote_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(init,payer=admin,seeds=[b"buyback",pool.key().as_ref()],bump,token::mint=sol_mint,token::authority=authority,token::token_program=token_program)]
    pub buyback_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(init,payer=admin,seeds=[b"quote",pool.key().as_ref()],bump,token::mint=usdc_mint,token::authority=authority,token::token_program=token_program)]
    pub payout_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(init,payer=admin,space=8+NativeRevenue::INIT_SPACE,seeds=[b"native-revenue",pool.key().as_ref()],bump)] pub native_revenue:Box<Account<'info,NativeRevenue>>,
    #[account(init,payer=admin,space=8+PerpsRevenue::INIT_SPACE,seeds=[b"perps-revenue",pool.key().as_ref()],bump)] pub revenue:Box<Account<'info,PerpsRevenue>>,
    pub system_program:Program<'info,System>,
    pub execution:BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct StakePerpsOwnership<'info> {
    pub owner:Signer<'info>,
    #[account(address=pool_address())] pub pool:Box<Account<'info,Pool>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)] pub ownership_pool:Box<Account<'info,OwnershipPool>>,
    #[account(mut,seeds=[b"ownership-trader",pool.key().as_ref(),owner.key().as_ref()],bump=ownership_trader.bump,has_one=pool,has_one=owner)] pub ownership_trader:Box<Account<'info,OwnershipTrader>>,
}
#[derive(Accounts)]
pub struct ClaimPerpsRewards<'info> {
    pub owner:Signer<'info>,
    #[account(address=pool_address())] pub pool:Box<Account<'info,Pool>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)] pub ownership_pool:Box<Account<'info,OwnershipPool>>,
    #[account(mut,seeds=[b"ownership-trader",pool.key().as_ref(),owner.key().as_ref()],bump=ownership_trader.bump,has_one=pool,has_one=owner)] pub ownership_trader:Box<Account<'info,OwnershipTrader>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=MINT,mint::token_program=token_program)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    pub token_program:Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"perps-output",pool.key().as_ref(),MINT.as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)] pub vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,token::mint=mint,token::authority=owner,token::token_program=token_program)] pub destination:Box<InterfaceAccount<'info,TokenAccount>>,
    pub execution:BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct SweepPerpsSurplus<'info> {
    pub caller:Signer<'info>,
    #[account(address=pool_address())] pub pool:Box<Account<'info,Pool>>,
    #[account(seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)] pub ownership_pool:Box<Account<'info,OwnershipPool>>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),pool.settlement.as_ref()],bump=inventory.bump,has_one=pool)] pub inventory:Box<Account<'info,Inventory>>,
    #[account(mut,seeds=[b"perps-revenue",pool.key().as_ref()],bump=revenue.bump,has_one=pool)] pub revenue:Box<Account<'info,PerpsRevenue>>,
    pub execution:BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct RoutePerpsRevenue<'info> {
    #[account(mut)] pub publisher:Signer<'info>,
    #[account(seeds=[b"config",SEED],bump=config.bump,has_one=publisher)] pub config:Box<Account<'info,Config>>,
    #[account(address=pool_address())] pub pool:Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=pool.settlement,constraint=usdc.mint==mainnet_pump::USDC @ MixError::Binding)] pub usdc:Box<Account<'info,Asset>>,
    #[account(seeds=[b"asset",target.mint.as_ref()],bump=target.bump)] pub target:Box<Account<'info,Asset>>,
    #[account(mut,address=target.mint,mint::token_program=token_program)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=target.token_program)] pub token_program:Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),usdc.key().as_ref()],bump,
        constraint=source.mint==mainnet_pump::USDC && source.owner==authority.key() @ MixError::Binding)] pub source:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,seeds=[b"sol-quote",pool.key().as_ref()],bump,
        constraint=quote_vault.mint==mainnet_pump::SOL && quote_vault.owner==authority.key() @ MixError::Binding)] pub quote_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(init_if_needed,payer=publisher,seeds=[b"perps-output",pool.key().as_ref(),mint.key().as_ref()],bump,
        token::mint=mint,token::authority=authority,token::token_program=token_program)] pub output:Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,seeds=[b"perps-revenue",pool.key().as_ref()],bump=revenue.bump,has_one=pool)] pub revenue:Box<Account<'info,PerpsRevenue>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)] pub ownership_pool:Box<Account<'info,OwnershipPool>>,
    /// CHECK: Fixed first recipient; half of SOL proceeds plus donated WSOL.
    #[account(mut,address=OPERATIONS)] pub operations:UncheckedAccount<'info>,
    /// CHECK: Fixed second recipient; half of SOL proceeds plus donated WSOL,
    /// including any indivisible one-lamport remainder.
    #[account(mut,address=OPERATIONS_SECONDARY)] pub operations_secondary:UncheckedAccount<'info>,
    /// CHECK: Executable canonical PumpSwap program; route accounts checked by CPI adapter.
    #[account(address=pump::ID,executable)] pub pump_program:UncheckedAccount<'info>,
    pub system_program:Program<'info,System>,
    pub execution:BaseExecution<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn both_tiers_preserve_pool_rewards_operations_and_gas() {
        for (size,expected) in [(0,(9000,500,200,100,100,0,100)),(math::HIGH_POOL_THRESHOLD,(9000,500,200,100,100,0,100)),
            (math::HIGH_POOL_THRESHOLD+1,(1000,5000,2000,500,1000,400,100))] {
            let (input,gas,sdice)=loss_split(10000,size);
            let (pool,reserved,_)=receipt_split(input,size);
            let mut r=PerpsRevenue::default();r.reserve(reserved,size).unwrap();
            let (sdice_rewards,sdice_burn)=sdice_quote_split(sdice,size);
            assert_eq!((pool,r.rewards_usdc,r.operations_usdc,r.burn_usdc,sdice_rewards,sdice_burn,gas),expected);
        }
        for size in [0,math::HIGH_POOL_THRESHOLD+1] {for amount in [0,1,6,7,8,75,u64::MAX] {
            let (input,gas,sdice)=loss_split(amount,size);
            assert_eq!(input as u128+gas as u128+sdice as u128,amount as u128);
            let mut r=PerpsRevenue::default();r.reserve(amount,size).unwrap();
            assert_eq!(r.rewards_usdc as u128+r.operations_usdc as u128+r.burn_usdc as u128,amount as u128);
        }}
    }
    #[test] fn namespace_never_reuses_root_or_legacy_usdc_accounts() {
        let old=Pubkey::find_program_address(&[b"config",mainnet_pump::USDC.as_ref()],&crate::ID).0;
        assert_ne!(config_address(),old);assert_ne!(pool_address(),ROOT_POOL);
        assert_ne!(pool_address(),venue::pool_address(old));
    }
}
