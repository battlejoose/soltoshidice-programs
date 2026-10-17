//! Exact-in PumpSwap execution. This devnet adapter deliberately supports only
//! registered, unboosted, noncanonical test pools quoted in a six-decimal test USD.
//! Mainnet Pump-origin/fee-tier adapters require a separate reviewed release.
use anchor_lang::{prelude::*, solana_program::{instruction::{AccountMeta, Instruction}, program::invoke_signed}};
use anchor_spl::token_interface::TokenAccount;
use crate::{Asset, Pool, MixError, math, Valuation};
pub const ID: Pubkey = pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
pub const FEES_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
pub fn swap_quote_mint(config_quote:Pubkey)->Pubkey {config_quote}
pub fn intermediate_seed()->&'static [u8] {b"quote"}
pub struct ValueCurve {base:u64,quote:u64,fees:[u16;3]}
impl ValueCurve {pub fn value(&self,amount:u64)->Result<u64> {sell_from_reserves(amount,self.base,self.quote,self.fees)}}
pub fn value_curve(a:&Asset,v:&Valuation)->Result<ValueCurve> {let (base,quote,fees)=validate_pool(a,v)?;Ok(ValueCurve{base,quote,fees})}
pub fn collateral_curve(a:&Asset,v:&Valuation,_sol_price:u64)->Result<ValueCurve> {value_curve(a,v)}
pub fn collateral_value(amount:u64,a:&Asset,v:&Valuation,sol_price:u64)->Result<u64> {
    if a.mint==anchor_spl::token::spl_token::native_mint::ID {
        return crate::engine::sol_collateral_value(amount,sol_price);
    }
    sell_value(amount,a,v)
}
const POOL_DISC: [u8;8] = [241,154,109,4,17,177,109,188];
fn key(d:&[u8],i:usize)->Pubkey {Pubkey::new_from_array(d[i..i+32].try_into().unwrap())}
fn num(d:&[u8],i:usize)->u64 {u64::from_le_bytes(d[i..i+8].try_into().unwrap())}
pub fn token_amount(info:&AccountInfo)->Result<u64>{
    let d=info.try_borrow_data()?;
    require!(d.len()>=165 && (*info.owner==anchor_spl::token::ID || *info.owner==anchor_spl::token_2022::ID),MixError::Binding);
    Ok(num(&d,64))
}
pub fn validate_pool(a:&Asset,v:&Valuation)->Result<(u64,u64,[u16;3])>{
    require_keys_eq!(*v.dex_pool.owner,ID,MixError::Binding);
    require_keys_eq!(v.dex_pool.key(),a.dex_pool,MixError::Binding);
    let d=v.dex_pool.try_borrow_data()?;
    require!(d.len()>=261 && d[..8]==POOL_DISC,MixError::Binding);
    require!(key(&d,43)==a.mint && key(&d,75)==a.quote_mint,MixError::Binding);
    let derived=Pubkey::find_program_address(&[b"pool",&d[9..11],&d[11..43],a.mint.as_ref(),a.quote_mint.as_ref()],&ID).0;
    require_keys_eq!(derived,a.dex_pool,MixError::Binding);
    let canonical=Pubkey::find_program_address(&[b"pool-authority",a.mint.as_ref()],&pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P")).0;
    require!(key(&d,11)!=canonical && key(&d,211)==Pubkey::default()
        && d[243]==0 && d[244]==0 && d[245..261].iter().all(|x|*x==0),MixError::Binding);
    // Reject future per-pool creator overrides; these test pools have no creator.
    require_keys_eq!(v.dex_base.key(),key(&d,139),MixError::Binding);
    require_keys_eq!(v.dex_quote.key(),key(&d,171),MixError::Binding);
    for (info,mint,program) in [(&v.dex_base,a.mint,a.token_program),(&v.dex_quote,a.quote_mint,anchor_spl::token::ID)] {
        require_keys_eq!(*info.owner,program,MixError::Binding);
        let b=info.try_borrow_data()?;
        require!(b.len()>=165 && key(&b,0)==mint && key(&b,32)==a.dex_pool,MixError::Binding);
    }
    require_keys_eq!(*v.fee_config.owner,FEES_ID,MixError::Binding);
    require_keys_eq!(v.fee_config.key(),Pubkey::find_program_address(&[b"fee_config",ID.as_ref()],&FEES_ID).0,MixError::Binding);
    let f=v.fee_config.try_borrow_data()?;
    require!(f.len()>=65 && f[..8]==[143,52,146,187,219,123,76,155],MixError::Binding);
    let lp=num(&f,41);let protocol=num(&f,49);
    require!(lp+protocol<10000,MixError::Quote);
    Ok((token_amount(&v.dex_base)?,token_amount(&v.dex_quote)?,[lp as u16,protocol as u16,0]))
}
pub fn sell_value(amount:u64,a:&Asset,v:&Valuation)->Result<u64>{
    if is_quote(a) {return Ok(amount);}
    let (base,quote,fees)=validate_pool(a,v)?;
    sell_from_reserves(amount,base,quote,fees)
}
pub fn is_quote(a:&Asset)->bool {
    a.id==104 && a.mint==a.quote_mint && a.decimals==6 && a.token_program==anchor_spl::token::ID
}
/// Exact same-mint transfer under the pool authority, with custody delta checks.
pub fn move_tokens<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,source:&InterfaceAccount<'info,TokenAccount>,dest:&InterfaceAccount<'info,TokenAccount>,program:&AccountInfo<'info>,amount:u64)->Result<()> {
    require!(program.key()==anchor_spl::token::ID && source.owner==authority.key() && dest.owner==authority.key() && source.mint==dest.mint && source.key()!=dest.key(),MixError::Binding);
    let before=token_amount(&source.to_account_info())?; let out0=token_amount(&dest.to_account_info())?;
    let ix=anchor_spl::token::spl_token::instruction::transfer(&program.key(),&source.key(),&dest.key(),&authority.key(),&[],amount)?;
    let key=p.key(); let seeds:&[&[u8]]=&[b"authority",key.as_ref(),&[p.authority_bump]];
    invoke_signed(&ix,&[source.to_account_info(),dest.to_account_info(),authority.to_account_info(),program.clone()],&[seeds])?;
    require!(token_amount(&source.to_account_info())?==before.checked_sub(amount).ok_or(MixError::Arithmetic)? && token_amount(&dest.to_account_info())?==out0.checked_add(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    Ok(())
}
pub fn sell_from_reserves(amount:u64,base:u64,quote:u64,fees:[u16;3])->Result<u64>{
    if base==0 || quote==0 {return Ok(0);}
    let gross=math::div(amount as u128,quote as u128,base as u128+amount as u128)?;
    let cost=fees.iter().try_fold(0u64,|s,b|->Result<u64>{s.checked_add(math::ceil(gross as u128*(*b as u128),10000)?).ok_or_else(||error!(MixError::Arithmetic))})?;
    Ok(gross.saturating_sub(cost))
}
fn leg<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,a:&Account<Asset>,base:&AccountInfo<'info>,quote:&AccountInfo<'info>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],sell:bool,amount:u64,min_out:u64)->Result<()> {
    require!(accounts.len()>=if sell {21}else{23},MixError::Binding);
    for (i,k) in [(0,a.dex_pool),(1,authority.key()),(3,a.mint),(4,a.quote_mint),(5,base.key()),(6,quote.key()),(11,a.token_program),(12,anchor_spl::token::ID),(16,ID)] {
        require_keys_eq!(accounts[i].key(),k,MixError::Binding);
    }
    // Fixed entrypoints and exact amounts: callers never provide executable CPI bytes.
    let mut data=if sell {vec![51,230,133,164,1,127,131,173]}else{vec![198,46,21,82,180,217,232,112]};
    data.extend_from_slice(&amount.to_le_bytes());data.extend_from_slice(&min_out.to_le_bytes());
    if !sell {data.push(0);}
    let metas=accounts.iter().map(|a|AccountMeta{pubkey:a.key(),is_writable:a.is_writable,is_signer:a.key()==authority.key()}).collect();
    let instruction=Instruction{program_id:ID,accounts:metas,data};
    let key=p.key();let seeds:&[&[u8]]=&[b"authority",key.as_ref(),&[p.authority_bump]];
    let mut infos=accounts.to_vec();infos.push(program.to_account_info());
    invoke_signed(&instruction,&infos,&[seeds])?;
    Ok(())
}
/// Sell only payout inventory into the configured USD quote. Never spend an existing quote balance.
pub fn sell_to_quote<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,from:&Account<Asset>,source:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],amount:u64,min_out:u64)->Result<u64>{
    require!(quote.mint==from.quote_mint && quote.owner==authority.key(),MixError::Binding);
    let before=token_amount(&quote.to_account_info())?;
    let base_before=token_amount(&source.to_account_info())?;
    leg(p,authority,from,&source.to_account_info(),&quote.to_account_info(),program,accounts,true,amount,min_out)?;
    require!(base_before.checked_sub(token_amount(&source.to_account_info())?)==Some(amount),MixError::Binding);
    let received=token_amount(&quote.to_account_info())?.checked_sub(before).ok_or(MixError::Arithmetic)?;
    require!(received>=min_out,MixError::Quote);
    Ok(received)
}
pub fn swap_loss<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,from:&Account<Asset>,to:&Account<Asset>,source:&InterfaceAccount<'info,TokenAccount>,dest:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],sell_count:u8,amount:u64,min_out:u64)->Result<u64>{
    require_keys_eq!(program.key(),ID,MixError::Binding);
    require!(program.executable && from.quote_mint==to.quote_mint && source.owner==authority.key() && dest.owner==authority.key() && quote.owner==authority.key()
        && source.mint==from.mint && dest.mint==to.mint && quote.mint==from.quote_mint, MixError::Binding);
    let count=sell_count as usize;
    require!(if is_quote(from) {count==0 && !is_quote(to) && !accounts.is_empty()} else {count>0 && count<accounts.len()},MixError::Binding);
    let before=token_amount(&source.to_account_info())?;
    let q0=token_amount(&quote.to_account_info())?;
    let out0=token_amount(&dest.to_account_info())?;
    if is_quote(from) {
        let token_program=accounts.iter().find(|a|a.key()==anchor_spl::token::ID).ok_or(MixError::Binding)?;
        move_tokens(p,authority,source,quote,token_program,amount)?;
    } else {
        leg(p,authority,from,&source.to_account_info(),&quote.to_account_info(),program,&accounts[..count],true,amount,1)?;
    }
    require!(token_amount(&source.to_account_info())?==before.checked_sub(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    let received=token_amount(&quote.to_account_info())?.checked_sub(q0).ok_or(MixError::Arithmetic)?;
    require!(received>0,MixError::Quote);
    leg(p,authority,to,&dest.to_account_info(),&quote.to_account_info(),program,&accounts[count..],false,received,min_out)?;
    // Pre-existing quote dust and locked collateral cannot fund the conversion.
    require!(token_amount(&quote.to_account_info())?>=q0,MixError::Binding);
    let out=token_amount(&dest.to_account_info())?.checked_sub(out0).ok_or(MixError::Arithmetic)?;
    require!(out>=min_out && out>0,MixError::Quote);
    Ok(out)
}

/// Sell the reserved gas allocation into native SOL. Quote rounding stays in the pool.
pub fn swap_fees<'info>(p:&Account<Pool>,authority:&UncheckedAccount<'info>,from:&Account<Asset>,perp:&Account<Asset>,gas:&Account<Asset>,source:&InterfaceAccount<'info,TokenAccount>,perp_vault:&InterfaceAccount<'info,TokenAccount>,gas_vault:&InterfaceAccount<'info,TokenAccount>,quote:&InterfaceAccount<'info,TokenAccount>,program:&UncheckedAccount<'info>,accounts:&[AccountInfo<'info>],sell_count:u8,perp_count:u8,amount:u64,gas_tokens:u64,min_perp:u64,min_gas:u64)->Result<(u64,u64)> {
    require!(program.key()==ID && program.executable && perp.id==102 && gas.mint==anchor_spl::token::spl_token::native_mint::ID && gas.token_program==anchor_spl::token::ID, MixError::Binding);
    require!(from.quote_mint==perp.quote_mint && from.quote_mint==gas.quote_mint && source.owner==authority.key() && perp_vault.owner==authority.key() && gas_vault.owner==authority.key() && quote.owner==authority.key(),MixError::Binding);
    require!(source.mint==from.mint && perp_vault.mint==perp.mint && gas_vault.mint==gas.mint && quote.mint==from.quote_mint,MixError::Binding);
    let a=sell_count as usize; let b=a+perp_count as usize;
    require!((a>0 || is_quote(from)) && b>a && b<accounts.len(),MixError::Binding);
    require!(!is_quote(from) || a==0,MixError::Binding);
    let source0=token_amount(&source.to_account_info())?;
    let q0=token_amount(&quote.to_account_info())?;
    let p0=token_amount(&perp_vault.to_account_info())?;
    let g0=token_amount(&gas_vault.to_account_info())?;
    if is_quote(from) {
        let token_program=accounts.iter().find(|a|a.key()==anchor_spl::token::ID).ok_or(MixError::Binding)?;
        move_tokens(p,authority,source,quote,token_program,amount)?;
    } else {
        leg(p,authority,from,&source.to_account_info(),&quote.to_account_info(),program,&accounts[..a],true,amount,1)?;
    }
    require!(token_amount(&source.to_account_info())?==source0.checked_sub(amount).ok_or(MixError::Arithmetic)?,MixError::Binding);
    let received=token_amount(&quote.to_account_info())?.checked_sub(q0).ok_or(MixError::Arithmetic)?;
    require!(gas_tokens<=amount,MixError::Amount);
    let gas_quote=math::div(received as u128,gas_tokens as u128,amount as u128)?;
    let perp_quote=math::div(received as u128,(amount-gas_tokens) as u128,amount as u128)?;
    if perp_quote>0 {leg(p,authority,perp,&perp_vault.to_account_info(),&quote.to_account_info(),program,&accounts[a..b],false,perp_quote,min_perp.max(1))?;}
    if gas_quote>0 {leg(p,authority,gas,&gas_vault.to_account_info(),&quote.to_account_info(),program,&accounts[b..],false,gas_quote,min_gas.max(1))?;}
    require!(token_amount(&quote.to_account_info())?>=q0,MixError::Binding);
    Ok((token_amount(&perp_vault.to_account_info())?.checked_sub(p0).ok_or(MixError::Arithmetic)?,token_amount(&gas_vault.to_account_info())?.checked_sub(g0).ok_or(MixError::Arithmetic)?))
}
