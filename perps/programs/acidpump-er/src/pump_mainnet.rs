//! Base-chain settlement adapter for mainnet deployments.
//! USD profit claims pay backing tokens at spot. Lost collateral routes through
//! USDC/SOL before the primary buy; unrelated intermediate balances are preserved.
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
use crate::{math,mainnet_pump::{self as live,SolPoolAccounts,TokenPoolAccounts,SolPoolQuote},Asset,Pool,MixError,Valuation};
pub const ID:Pubkey=pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const FEES_ID:Pubkey=pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub const USDC_POOL:Pubkey=pubkey!("Gf7sXMoP8iRw4iiXmJ1nq4vxcRycbGXy5RL8a8LnTd3v");
pub fn swap_quote_mint(_:Pubkey)->Pubkey {live::SOL}
/// Registration derives denomination from authenticated venue state, never a
/// caller-supplied price. validate_pool then checks the complete pool and vaults.
pub fn registration_quote_mint(mint:Pubkey,v:&Valuation)->Result<Pubkey> {
    if mint==live::SOL {return Ok(live::SOL);}
    let info=v.dex_pool.to_account_info();
    require!(!info.executable,MixError::Binding);
    let data=info.try_borrow_data()?;
    require!(*info.owner==ID && data.len()>=211 && data[..8]==[241,154,109,4,17,177,109,188],MixError::Binding);
    let quote=Pubkey::new_from_array(data[75..107].try_into().unwrap());
    require!(quote==live::SOL || quote==live::USDC,MixError::Binding);
    Ok(quote)
}
pub fn intermediate_seed()->&'static [u8] {b"sol-quote"}
pub fn is_quote(a:&Asset)->bool {a.id==104 && a.mint==live::USDC && a.decimals==6 && a.token_program==anchor_spl::token::ID}
fn is_sol(a:&Asset)->bool {a.id==103 && a.mint==live::SOL && a.decimals==9 && a.token_program==anchor_spl::token::ID}
pub fn token_amount(info:&AccountInfo)->Result<u64> {
    require!(!info.executable && (*info.owner==anchor_spl::token::ID || *info.owner==anchor_spl::token_2022::ID),MixError::Binding);
    let d=info.try_borrow_data()?;
    require!(d.len()>=165 && d[108]==1,MixError::Binding);
    Ok(u64::from_le_bytes(d[64..72].try_into().unwrap()))
}
fn source_quote(a:&Asset,v:&Valuation)->Result<SolPoolQuote> {
    source_pool_quote(a,v,false)
}
fn source_pool_quote(a:&Asset,v:&Valuation,allow_empty:bool)->Result<SolPoolQuote> {
    require!((a.quote_mint==live::SOL||a.quote_mint==live::USDC) && v.dex_mint.key()==a.mint && *v.dex_mint.owner==a.token_program,MixError::Binding);
    let pool=v.dex_pool.to_account_info();let mint=v.dex_mint.to_account_info();let base=v.dex_base.to_account_info();
    let quote=v.dex_quote.to_account_info();let global=v.source_global.to_account_info();let fees=v.source_fees.to_account_info();
    require_keys_eq!(pool.key(),a.dex_pool,MixError::Binding);
    let expected=a.dex_pool;
    let accounts=SolPoolAccounts{pool:&pool,mint:&mint,base_vault:&base,quote_vault:&quote,global:&global,fees:&fees};
    if a.quote_mint==live::USDC {
        return TokenPoolAccounts{pool:accounts,quote_mint:&v.usd_mint.to_account_info()}.validate(expected,a.mint,a.quote_mint,allow_empty);
    }
    if allow_empty {accounts.validate_collateral(expected,a.mint)} else {accounts.validate(expected,a.mint)}
}
fn payout_quote(v:&Valuation)->Result<SolPoolQuote> {
    SolPoolAccounts{pool:&v.usd_pool.to_account_info(),mint:&v.usd_mint.to_account_info(),base_vault:&v.usd_base.to_account_info(),
        quote_vault:&v.usd_quote.to_account_info(),global:&v.global_config.to_account_info(),fees:&v.fee_config.to_account_info()}.validate(USDC_POOL,live::USDC)
}
pub fn validate_pool(a:&Asset,v:&Valuation)->Result<(u64,u64,[u16;3])> {
    if is_sol(a) {require!(a.quote_mint==live::SOL,MixError::Binding);return Ok((0,0,[0;3]));}
    let quote=source_quote(a,v)?;Ok((quote.base,quote.effective_quote()?,quote.fees))
}
pub struct ValueCurve {source:Option<SolPoolQuote>,payout:SolPoolQuote,usd:bool,source_usdc:bool}
impl ValueCurve {
    pub fn maximum_input(&self,amount:u64)->Result<u64> {match self.source {Some(q)=>q.maximum_sell(amount),None=>Ok(amount)}}
    pub fn value(&self,amount:u64)->Result<u64> {
        if self.usd {return Ok(amount);}
        if amount==0 {return Ok(0);}
        let proceeds=match self.source {Some(q)=>q.collateral_quote_atoms(self.maximum_input(amount)?)?,None=>amount};
        if self.source_usdc {Ok(proceeds)} else {self.payout.buy(proceeds)}
    }
}
pub fn value_curve(a:&Asset,v:&Valuation)->Result<ValueCurve> {
    let payout=payout_quote(v)?;
    Ok(ValueCurve{source:if is_sol(a)||is_quote(a) {None}else{Some(source_pool_quote(a,v,true)?)},payout,usd:is_quote(a),source_usdc:a.quote_mint==live::USDC})
}
pub fn sell_value(amount:u64,a:&Asset,v:&Valuation)->Result<u64> {value_curve(a,v)?.value(amount)}
pub fn spot_curve(a:&Asset,v:&Valuation)->Result<crate::spot::SpotCurve> {
    use crate::spot::SpotCurve;
    if is_quote(a) {return Ok(SpotCurve{base:1,quote:1,usd_base:1,usd_quote:1});}
    let source=if is_sol(a) {None}else{Some(source_pool_quote(a,v,true)?)};
    let (base,quote)=match source {Some(q) if q.base==0 || q.real_quote==0 =>(1,0),Some(q)=>(q.base,q.effective_quote()?),None=>(1,1)};
    let (usd_base,usd_quote)=if a.quote_mint==live::USDC {(1,1)}else{let q=payout_quote(v)?;(q.base,q.effective_quote()?)};
    Ok(SpotCurve{base,quote,usd_base,usd_quote})
}
pub fn spot_value(amount:u64,a:&Asset,v:&Valuation)->Result<u64> {if amount==0 {return Ok(0);}spot_curve(a,v)?.value(amount)}
/// Quote the FULL trader loss in payout-token atoms before any platform split.
/// Uses authenticated pre-swap reserves. Same-token loss is exactly its atoms.
pub fn loss_reward_tokens(amount:u64,a:&Asset,v:&Valuation,to:&Asset,tv:&Valuation)->Result<u64> {
    if amount==0 || a.mint==to.mint {return Ok(amount);}
    require!(to.quote_mint==live::SOL,MixError::Binding);
    let sol=if is_sol(a) {amount} else if is_quote(a) {payout_quote(v)?.collateral_quote_atoms(amount)?}
        else {let q=source_pool_quote(a,v,true)?;let quote=q.collateral_quote_atoms(q.maximum_sell(amount)?)?;
            if a.quote_mint==live::USDC {payout_quote(v)?.collateral_quote_atoms(quote)?} else {quote}};
    if sol==0 {return Ok(0);}
    if is_sol(to) {return Ok(sol);}
    if is_quote(to) {return payout_quote(tv)?.buy(sol);}
    source_pool_quote(to,tv,true)?.buy(sol)
}
/// Entry/closing backing uses the complete collateral sale into SOL, valued by
/// the shared SOL/USD feed. USDC payout routing is a separate executable curve.
pub struct CollateralCurve { source: Option<SolPoolQuote>, usd: bool, source_usdc:bool, sol_usd: u64 }
impl CollateralCurve {
    pub fn value(&self, amount: u64) -> Result<u64> {
        if self.usd { return Ok(amount); }
        crate::mainnet_quote_path::collateral_usd(amount, self.source.as_slice(),
            if self.source_usdc {1_000_000_000_000_000} else {self.sol_usd})
    }
}
pub fn collateral_curve(a:&Asset,v:&Valuation,sol_usd:u64)->Result<CollateralCurve> {
    live::sol_to_usd_micro(0, sol_usd)?;
    Ok(CollateralCurve { source: if is_sol(a)||is_quote(a) { None } else { Some(source_pool_quote(a,v,true)?) }, usd:is_quote(a), source_usdc:a.quote_mint==live::USDC, sol_usd })
}
pub fn collateral_value(amount:u64,a:&Asset,v:&Valuation,sol_usd:u64)->Result<u64> { collateral_curve(a,v,sol_usd)?.value(amount) }
// ER legacy arithmetic remains available for the devnet-shaped snapshot method;
// mainnet ER publication uses explicit SOL/USD fields rather than this shortcut.
pub fn sell_from_reserves(amount:u64,base:u64,quote:u64,fees:[u16;3])->Result<u64> {
    if base==0||quote==0 {return Ok(0);}
    let gross=math::div(amount as u128,quote as u128,base as u128+amount as u128)?;
    let costs=fees.iter().try_fold(0u64,|sum,fee|->Result<u64>{sum.checked_add(math::ceil(gross as u128*(*fee as u128),10_000)?).ok_or_else(||error!(MixError::Arithmetic))})?;
    Ok(gross.saturating_sub(costs))
}
pub fn move_tokens<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,source:&InterfaceAccount<'info,TokenAccount>,dest:&InterfaceAccount<'info,TokenAccount>,program:&AccountInfo<'info>,amount:u64)->Result<()> {
    use anchor_lang::solana_program::program::invoke_signed;
    require!(program.key()==anchor_spl::token::ID && source.owner==authority.key() && dest.owner==authority.key() && source.mint==dest.mint && source.key()!=dest.key(),MixError::Binding);
    let before=token_amount(&source.to_account_info())?;let out0=token_amount(&dest.to_account_info())?;
    let ix=anchor_spl::token::spl_token::instruction::transfer(&program.key(),&source.key(),&dest.key(),&authority.key(),&[],amount)?;
    let key=p.key();let seeds:&[&[u8]]=&[b"authority",key.as_ref(),&[p.authority_bump]];
    invoke_signed(&ix,&[source.to_account_info(),dest.to_account_info(),authority.to_account_info(),program.clone()],&[seeds])?;
    require!(token_amount(&source.to_account_info())?==before.checked_sub(amount).ok_or(MixError::Arithmetic)? && token_amount(&dest.to_account_info())?==out0.checked_add(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    Ok(())
}
pub(crate) fn leg<'info>(p:&Account<Pool>,a:&Asset,base:Pubkey,quote:Pubkey,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],buy:bool,amount:u64,min_out:u64)->Result<u64> {
    require!(program.key()==ID && program.executable,MixError::Binding);
    if a.quote_mint==live::USDC {
        // USDC-quoted collateral exits into the ordinary SOL intermediate. The
        // registration gate still excludes token-quoted primary assets, so this
        // branch cannot be used as an unverified primary buy route.
        require!(!buy,MixError::Binding);
        return usdc_collateral_leg(p,a,base,quote,accounts,amount,min_out);
    }
    require!(a.quote_mint==live::SOL,MixError::Binding);
    let token_index=if accounts.len()==17 {9}else{11};
    require!(accounts.len()>12 && accounts[5].key()==base && accounts[6].key()==quote && accounts[token_index].key()==a.token_program,MixError::Binding);
    require_keys_eq!(accounts[0].key(),a.dex_pool,MixError::Binding);
    let expected=a.dex_pool;
    Ok(live::execute_sol_leg(p.key(),p.authority_bump,expected,a.mint,accounts,buy,amount,min_out.max(1))?.1)
}
fn usdc_collateral_leg<'info>(p:&Account<Pool>,a:&Asset,base:Pubkey,sol:Pubkey,accounts:&[AccountInfo<'info>],amount:u64,min_out:u64)->Result<u64> {
    require!(accounts.len()>=34 && a.quote_mint==live::USDC,MixError::Binding);
    let count=live::sol_leg_accounts_len(accounts,false)?;
    require!(count<accounts.len(),MixError::Binding);
    let source=&accounts[..count];let conversion=&accounts[count..];
    let (source_i,quote_i)=(5,6);
    let token_i=if count==17{9}else{11};
    require!(source[source_i].key()==base && conversion.len()>=17 &&
        source[token_i].key()==a.token_program &&
        conversion[5].key()==source[quote_i].key() && conversion[6].key()==sol,MixError::Binding);
    require_keys_eq!(source[0].key(),a.dex_pool,MixError::Binding);
    let q0=token_amount(&source[quote_i])?;
    let usdc=live::execute_token_leg(p.key(),p.authority_bump,a.dex_pool,a.mint,live::USDC,source,false,amount,1)?.1;
    // Only the proceeds of this sale may be sold for SOL. Exact-input selling
    // leaves every pre-existing USDC atom untouched, including pending claims.
    let out=live::execute_sol_leg(p.key(),p.authority_bump,USDC_POOL,live::USDC,conversion,false,usdc,min_out.max(1))?.1;
    require!(token_amount(&source[quote_i])?==q0,MixError::Binding);
    Ok(out)
}
fn sol_program<'a,'info>(accounts:&'a [AccountInfo<'info>])->Result<&'a AccountInfo<'info>> {
    accounts.iter().find(|a|a.key()==anchor_spl::token::ID && a.executable).ok_or_else(||error!(MixError::Binding))
}
pub fn swap_loss<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,from:&Account<Asset>,to:&Account<Asset>,source:&InterfaceAccount<'info,TokenAccount>,dest:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],sell_count:u8,amount:u64,min_out:u64)->Result<u64> {
    require!(source.mint==from.mint && dest.mint==to.mint && source.owner==authority.key() && dest.owner==authority.key() && quote.mint==live::SOL && quote.owner==authority.key(),MixError::Binding);
    let count=sell_count as usize;
    if is_sol(to) {
        require!(!is_sol(from) && count==accounts.len(),MixError::Binding);
        return leg(p,from,source.key(),dest.key(),program,accounts,false,amount,min_out);
    }
    require!(count<accounts.len(),MixError::Binding);
    let q0=token_amount(&quote.to_account_info())?;
    let received=if is_sol(from) {
        require!(count==0,MixError::Binding);
        move_tokens(p,authority,source,quote,sol_program(accounts)?,amount)?;amount
    } else {
        require!(count>0,MixError::Binding);
        leg(p,from,source.key(),quote.key(),program,&accounts[..count],false,amount,1)?
    };
    let out=leg(p,to,dest.key(),quote.key(),program,&accounts[count..],true,received,min_out)?;
    require!(token_amount(&quote.to_account_info())?>=q0,MixError::Binding);
    Ok(out)
}
pub fn swap_fees<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,from:&Account<Asset>,perp:&Account<Asset>,gas:&Account<Asset>,source:&InterfaceAccount<'info,TokenAccount>,perp_vault:&InterfaceAccount<'info,TokenAccount>,gas_vault:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],sell_count:u8,perp_count:u8,amount:u64,gas_tokens:u64,min_perp:u64,min_gas:u64)->Result<(u64,u64)> {
    require!(perp.id==102 && is_sol(gas) && !is_sol(from) && from.mint!=perp.mint && gas_tokens<=amount,MixError::Binding);
    require!(source.mint==from.mint && perp_vault.mint==perp.mint && gas_vault.mint==live::SOL && quote.mint==live::SOL && source.owner==authority.key() && perp_vault.owner==authority.key() && gas_vault.owner==authority.key() && quote.owner==authority.key(),MixError::Binding);
    let count=sell_count as usize;
    require!(count>0 && perp_count>0 && count+perp_count as usize==accounts.len(),MixError::Binding);
    let q0=token_amount(&quote.to_account_info())?;
    let received=leg(p,from,source.key(),quote.key(),program,&accounts[..count],false,amount,1)?;
    let gas_out=math::div(received as u128,gas_tokens as u128,amount as u128)?;
    if gas_out>0 {move_tokens(p,authority,quote,gas_vault,sol_program(accounts)?,gas_out)?;}
    require!(gas_out>=min_gas,MixError::Quote);
    let acid_out=leg(p,perp,perp_vault.key(),quote.key(),program,&accounts[count..],true,received-gas_out,min_perp)?;
    require!(token_amount(&quote.to_account_info())?>=q0,MixError::Binding);
    Ok((acid_out,gas_out))
}
pub fn redeem_usdc<'info>(ctx:Context<'info,crate::RedeemUsdc<'info>>)->Result<()> {
    let x=&mut ctx.accounts.position;let pool=&mut ctx.accounts.pool;
    require!(x.state!=0 && x.remaining>0,MixError::NoClaim);
    require!(x.queue_seq==pool.queue_head,MixError::QueueOrder);
    require!(ctx.accounts.config.quote_mint==live::USDC && ctx.accounts.quote_mint.key()==live::USDC && ctx.accounts.sol_vault.mint==live::SOL && ctx.accounts.sol_vault.owner==ctx.accounts.authority.key(),MixError::Binding);
    let curve=value_curve(&ctx.accounts.asset,&ctx.accounts.valuation)?;
    let available=curve.maximum_input(ctx.accounts.inventory.treasury)?;
    require!(available>0,MixError::Inventory);
    let mut lo=0;let mut hi=available;
    while lo<hi {let mid=lo+(hi-lo)/2+(hi-lo)%2;if curve.value(mid)?<=x.remaining {lo=mid;} else {hi=mid-1;}}
    let expected=curve.value(lo)?;
    require!(lo>0 && expected>0,MixError::Inventory);
    let sell_count=live::sol_leg_accounts_len(ctx.remaining_accounts,false)?;
    require!(ctx.remaining_accounts.len()>sell_count,MixError::Binding);
    let buy_count=live::sol_leg_accounts_len(&ctx.remaining_accounts[sell_count..],true)?;
    require!(ctx.remaining_accounts.len()==sell_count+buy_count,MixError::Binding);
    let q0=token_amount(&ctx.accounts.sol_vault.to_account_info())?;
    let sol=leg(pool,&ctx.accounts.asset,ctx.accounts.vault.key(),ctx.accounts.sol_vault.key(),&ctx.accounts.pump_program,&ctx.remaining_accounts[..sell_count],false,lo,1)?;
    let buy=&ctx.remaining_accounts[sell_count..];
    require!(buy[5].key()==ctx.accounts.quote_vault.key() && buy[6].key()==ctx.accounts.sol_vault.key(),MixError::Binding);
    let (_,out)=live::execute_sol_leg(pool.key(),pool.authority_bump,USDC_POOL,live::USDC,buy,true,sol,1)?;
    require!(out<=x.remaining && token_amount(&ctx.accounts.sol_vault.to_account_info())?>=q0,MixError::ClaimLimit);
    let left=x.remaining-out;let debit=if math::dust(left,x.profit) {x.remaining}else{out};
    crate::pay(pool,&ctx.accounts.quote_vault,&ctx.accounts.destination,&ctx.accounts.quote_mint,&ctx.accounts.quote_program,&ctx.accounts.authority,out)?;
    ctx.accounts.inventory.treasury-=lo;ctx.accounts.inventory.paid=ctx.accounts.inventory.paid.checked_add(lo).ok_or(MixError::Arithmetic)?;
    x.remaining-=debit;pool.queue_usd=pool.queue_usd.checked_sub(debit).ok_or(MixError::Arithmetic)?;
    if x.remaining==0 {pool.queue_head=pool.queue_head.checked_add(1).ok_or(MixError::Arithmetic)?;}
    emit!(crate::Redeemed{position:x.key(),asset:ctx.accounts.asset.key(),amount:lo,debit_usd:debit,converted_out:out});
    Ok(())
}

/// Buy from the canonical SOL-denominated pool, spending only the supplied
/// quote amount. The adapter validates the pool, mint, vaults and CPI programs.
pub fn buy_into<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,to:&Account<Asset>,dest:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],amount:u64,min_out:u64)->Result<()> {
    require!(amount>0 && min_out>0 && dest.owner==authority.key() && quote.owner==authority.key()
        && dest.mint==to.mint && quote.mint==live::SOL && to.quote_mint==live::SOL,MixError::Binding);
    let before=token_amount(&quote.to_account_info())?;
    leg(p,to,dest.key(),quote.key(),program,accounts,true,amount,min_out)?;
    require!(token_amount(&quote.to_account_info())?==before.checked_sub(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    Ok(())
}
/// One collateral sale, with SOL proceeds isolated from pre-existing quote.
/// 98:1:1 normally; 99:1:0 while the venue owes FIFO claims. Native SOL's gas
/// share is retained by the caller, so that path arrives with gas_part=0.
pub fn route_venue<'info>(r:&crate::venue::VenueRoute<'_,'info>,accounts:&[AccountInfo<'info>],sell_count:u8,perp_count:u8,amount:u64,perp_part:u64,gas_part:u64,buyback_part:u64,min_perp:u64,min_gas:u64)->Result<(u64,u64,u64)> {
    let (p,authority)=(r.pool,r.authority);
    require!(r.program.key()==ID && r.program.executable && is_sol(r.gas) && r.perp.quote_mint==live::SOL,MixError::Binding);
    require!(r.source.owner==authority.key() && r.perp_vault.owner==authority.key() && r.gas_vault.owner==authority.key()
        && r.quote.owner==authority.key() && r.buyback.owner==authority.key(),MixError::Binding);
    require!(r.source.mint==r.from.mint && r.perp_vault.mint==r.perp.mint && r.gas_vault.mint==live::SOL
        && r.quote.mint==live::SOL && r.buyback.mint==live::SOL,MixError::Binding);
    require!(r.source.key()!=r.quote.key() && r.quote.key()!=r.gas_vault.key() && r.buyback.key()!=r.quote.key()
        && r.buyback.key()!=r.source.key() && r.buyback.key()!=r.gas_vault.key(),MixError::Binding);
    require!(amount>0 && perp_part.checked_add(gas_part).and_then(|n|n.checked_add(buyback_part))==Some(amount),MixError::Amount);
    let count=sell_count as usize;
    require!(count+perp_count as usize==accounts.len() && (perp_part>0 && !is_sol(r.perp))==(perp_count>0)
        && (count==0)==is_sol(r.from),MixError::Binding);
    let q0=token_amount(&r.quote.to_account_info())?;
    let token_program=sol_program(accounts)?;
    let received=if is_sol(r.from) {
        move_tokens(p,authority,r.source,r.quote,token_program,amount)?;amount
    } else {leg(p,r.from,r.source.key(),r.quote.key(),r.program,&accounts[..count],false,amount,1)?};
    let gas_out=math::div(received as u128,gas_part as u128,amount as u128)?;
    let buyback_out=math::div(received as u128,buyback_part as u128,amount as u128)?;
    if gas_out>0 {move_tokens(p,authority,r.quote,r.gas_vault,token_program,gas_out)?;}
    if gas_part>0 {require!(gas_out>=min_gas,MixError::Quote);}
    if buyback_out>0 {move_tokens(p,authority,r.quote,r.buyback,token_program,buyback_out)?;}
    let token_quote=received.checked_sub(gas_out).and_then(|n|n.checked_sub(buyback_out)).ok_or(MixError::Arithmetic)?;
    let perp_out=if perp_part>0 {
        require_keys_neq!(r.source.key(),r.perp_vault.key(),MixError::Binding);
        if is_sol(r.perp) {
            require!(token_quote>=min_perp.max(1),MixError::Quote);
            move_tokens(p,authority,r.quote,r.perp_vault,token_program,token_quote)?;token_quote
        } else {
            leg(p,r.perp,r.perp_vault.key(),r.quote.key(),r.program,&accounts[count..],true,token_quote,min_perp.max(1))?
        }
    } else {0};
    // When only fees are sold, at most one atom of split rounding remains.
    require!(token_amount(&r.quote.to_account_info())?>=q0,MixError::Binding);
    Ok((perp_out,gas_out,buyback_out))
}
