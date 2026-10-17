//! Authenticated Pump.fun bonding-curve reserves, before PumpSwap migration.
//! Curve real reserves exclude rent and retained fees; virtual SOL cannot pay a sale.
use anchor_lang::prelude::*;
use anchor_spl::{token, token_2022};
use crate::{mainnet_pump::{self, PoolQuote, SolPoolQuote}, MixError};

pub const ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
pub const FEES_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
fn bytes<const N:usize>(data:&[u8], at:usize)->Result<[u8;N]> {
    data.get(at..at.checked_add(N).ok_or(MixError::Arithmetic)?)
        .and_then(|s|s.try_into().ok()).ok_or_else(||error!(MixError::Binding))
}
fn appended<const N:usize>(data:&[u8], at:usize)->Result<[u8;N]> {
    if data.len()<=at {Ok([0;N])} else {bytes(data,at)}
}
fn num(data:&[u8],at:usize)->Result<u64>{Ok(u64::from_le_bytes(bytes(data,at)?))}
fn bound(info:&AccountInfo,address:Pubkey,owner:Pubkey)->Result<()> {
    require!(info.key()==address && *info.owner==owner && !info.executable,MixError::Binding);Ok(())
}
pub fn address(mint:Pubkey)->Pubkey {Pubkey::find_program_address(&[b"bonding-curve",mint.as_ref()],&ID).0}
pub fn migrated_address(mint:Pubkey)->Pubkey {
    migrated_token_address(mint,mainnet_pump::SOL)
}
pub fn migrated_token_address(mint:Pubkey,quote:Pubkey)->Pubkey {
    let creator=Pubkey::find_program_address(&[b"pool-authority",mint.as_ref()],&ID).0;
    Pubkey::find_program_address(&[b"pool",&0u16.to_le_bytes(),creator.as_ref(),mint.as_ref(),quote.as_ref()],
        &pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA")).0
}
/// Existing AMMs stay pinned. Only a registered Pump curve may transition to
/// its canonical Pump-created SOL pool; the caller still validates that pool.
pub fn bind_amm_route(registered:Pubkey,mint:Pubkey,actual:Pubkey)->Result<Pubkey> {
    bind_token_amm_route(registered,mint,mainnet_pump::SOL,actual)
}
pub fn bind_token_amm_route(registered:Pubkey,mint:Pubkey,quote:Pubkey,actual:Pubkey)->Result<Pubkey> {
    require!(actual==registered || registered==address(mint)&&actual==migrated_token_address(mint,quote),MixError::Binding);
    Ok(actual)
}

#[derive(Clone,Copy,Debug)]
pub struct CurveState {
    pub virtual_base:u64,pub virtual_quote:u64,pub real_base:u64,pub real_quote:u64,
    pub complete:bool,pub creator:Pubkey,pub mayhem:bool,pub cashback:bool,
    pub quote_mint:Pubkey,pub creator_fee:u64,pub retained_fees:u64,
}
impl CurveState {
    pub fn read(d:&[u8])->Result<Self> {
        require!(d.len()>=49 && bytes::<8>(d,0)?==[23,183,248,55,96,216,172,96],MixError::Binding);
        for at in [48,81,82,123,124] {require!(appended::<1>(d,at)?[0]<=1,MixError::Binding);}
        let retained_fees=u64::from_le_bytes(appended(d,125)?).checked_add(u64::from_le_bytes(appended(d,133)?)).ok_or(MixError::Arithmetic)?;
        Ok(Self {virtual_base:num(d,8)?,virtual_quote:num(d,16)?,real_base:num(d,24)?,real_quote:num(d,32)?,
            complete:d[48]==1,creator:Pubkey::new_from_array(appended(d,49)?),mayhem:appended::<1>(d,81)?[0]==1,
            cashback:appended::<1>(d,82)?[0]==1,quote_mint:Pubkey::new_from_array(appended(d,83)?),
            creator_fee:u64::from_le_bytes(appended(d,115)?),retained_fees})
    }
}
pub struct SolCurveAccounts<'a,'info> {
    pub curve:&'a AccountInfo<'info>,pub mint:&'a AccountInfo<'info>,pub base_vault:&'a AccountInfo<'info>,
    pub global:&'a AccountInfo<'info>,pub fees:&'a AccountInfo<'info>,
}
/// Non-native curve reserves live in the curve's quote ATA, not its lamports.
/// The quote mint is bound to the caller's registered route, not inferred as SOL.
pub struct TokenCurveAccounts<'a,'info> {
    pub curve:SolCurveAccounts<'a,'info>,
    pub quote_mint:&'a AccountInfo<'info>,pub quote_vault:&'a AccountInfo<'info>,
}
impl TokenCurveAccounts<'_,'_> {
    pub fn validate(&self,expected_curve:Pubkey,expected_mint:Pubkey,expected_quote:Pubkey,allow_empty:bool)->Result<PoolQuote> {
        require!(expected_quote!=Pubkey::default() && expected_quote!=mainnet_pump::SOL &&
            expected_quote!=token_2022::spl_token_2022::native_mint::ID && self.quote_mint.key()==expected_quote,MixError::Binding);
        self.curve.validate_inner(expected_curve,expected_mint,None,Some((self.quote_mint,self.quote_vault)),allow_empty)
    }
}
impl SolCurveAccounts<'_,'_> {
    /// `rent` must be the current bank's Rent sysvar, not a hardcoded launch rate.
    pub fn validate(&self,expected_curve:Pubkey,expected_mint:Pubkey,rent:&Rent,allow_empty:bool)->Result<SolPoolQuote> {
        self.validate_inner(expected_curve,expected_mint,Some(rent),None,allow_empty)
    }
    fn validate_inner(&self,expected_curve:Pubkey,expected_mint:Pubkey,rent:Option<&Rent>,quote:Option<(&AccountInfo,&AccountInfo)>,allow_empty:bool)->Result<PoolQuote> {
        use token_2022::spl_token_2022::{extension::{BaseStateWithExtensions,ExtensionType,StateWithExtensions},state::Mint};
        require_keys_eq!(expected_curve,address(expected_mint),MixError::Binding);
        bound(self.curve,expected_curve,ID)?;
        require!(self.mint.key()==expected_mint && !self.mint.executable &&
            (*self.mint.owner==token::ID || *self.mint.owner==token_2022::ID),MixError::Binding);
        let mint_data=self.mint.try_borrow_data()?;
        let mint=StateWithExtensions::<Mint>::unpack(&mint_data)?;
        require!(mint.base.is_initialized && mint.base.decimals<=9 && mint.base.mint_authority.is_none() && mint.base.freeze_authority.is_none(),MixError::Binding);
        for extension in mint.get_extension_types()? {
            require!(matches!(extension,ExtensionType::MetadataPointer|ExtensionType::TokenMetadata),MixError::Binding);
        }
        let data=self.curve.try_borrow_data()?;
        let state=CurveState::read(&data)?;
        require!(!state.complete,MixError::Binding);
        let vault=Pubkey::find_program_address(&[expected_curve.as_ref(),self.mint.owner.as_ref(),expected_mint.as_ref()],&anchor_spl::associated_token::ID).0;
        require_keys_eq!(self.base_vault.key(),vault,MixError::Binding);
        require!(mainnet_pump::token_balance(self.base_vault,expected_mint,expected_curve,*self.mint.owner)? >= state.real_base,MixError::Binding);
        let required=state.real_quote.checked_add(state.retained_fees).ok_or(MixError::Arithmetic)?;
        if let Some((quote_mint,quote_vault))=quote {
            require!(quote_mint.key()==state.quote_mint && !quote_mint.executable &&
                (*quote_mint.owner==token::ID || *quote_mint.owner==token_2022::ID),MixError::Binding);
            let quote_data=quote_mint.try_borrow_data()?;
            let quote_state=StateWithExtensions::<Mint>::unpack(&quote_data)?;
            require!(quote_state.base.is_initialized && quote_state.base.decimals<=9,MixError::Binding);
            if state.quote_mint==mainnet_pump::USDC {require!(*quote_mint.owner==token::ID && quote_state.base.decimals==6,MixError::Binding);}
            for extension in quote_state.get_extension_types()? {
                require!(matches!(extension,ExtensionType::MetadataPointer|ExtensionType::TokenMetadata),MixError::Binding);
            }
            let quote_ata=Pubkey::find_program_address(&[expected_curve.as_ref(),quote_mint.owner.as_ref(),state.quote_mint.as_ref()],&anchor_spl::associated_token::ID).0;
            require_keys_eq!(quote_vault.key(),quote_ata,MixError::Binding);
            require!(mainnet_pump::token_balance(quote_vault,state.quote_mint,expected_curve,*quote_mint.owner)? >= required,MixError::Quote);
        } else {
            require!(state.quote_mint==Pubkey::default() || state.quote_mint==mainnet_pump::SOL,MixError::Binding);
            let required=required.checked_add(rent.ok_or(MixError::Binding)?.minimum_balance(data.len())).ok_or(MixError::Arithmetic)?;
            require!(self.curve.lamports()>=required,MixError::Quote);
        }
        bound(self.global,Pubkey::find_program_address(&[b"global"],&ID).0,ID)?;
        let global=self.global.try_borrow_data()?;
        require!(global.len()>=113 && bytes::<8>(&global,0)?==[167,232,232,177,200,108,114,127],MixError::Binding);
        for at in [8,145,450,515,740,1045,1086] {require!(appended::<1>(&global,at)?[0]<=1,MixError::Binding);}
        bound(self.fees,Pubkey::find_program_address(&[b"fee_config",ID.as_ref()],&FEES_ID).0,FEES_ID)?;
        // Curve sells use actual supply in Mayhem and the fixed 1B-token supply
        // otherwise; this differs from PumpSwap's Mayhem rule.
        let supply=if state.mayhem {mint.base.supply}else{1_000_000_000_000_000};
        let market_cap=supply as u128*state.virtual_quote as u128/(state.virtual_base.max(1) as u128);
        let mut fees=mainnet_pump::quote_fees(&self.fees.try_borrow_data()?,true,state.quote_mint,market_cap)?;
        fees[0]=0; // Bonding curves have no LP fee, even if that schedule field is nonzero.
        if appended::<1>(&global,1045)?[0]==1 && state.creator_fee>0 {
            require!(state.creator_fee<10_000,MixError::Quote);fees[2]=state.creator_fee as u16;
        }
        if state.creator==Pubkey::default(){fees[2]=0;}
        require!(fees.iter().map(|f|*f as u32).sum::<u32>()<10_000,MixError::Quote);
        let empty=state.virtual_base==0 || state.real_quote==0;
        let result=SolPoolQuote {base:state.virtual_base,real_quote:state.real_quote,
            virtual_quote:state.virtual_quote as i128-state.real_quote as i128,fees:if allow_empty && empty {[0;3]}else{fees}};
        if !(allow_empty && empty){result.effective_quote()?;}
        Ok(result)
    }
}

/// Sell custody tokens through Pump's SOL curve, then wrap only this sale's
/// native proceeds. Volume/curve account rent must be paid by preparation
/// instructions, before this call; operating SOL and existing wrapped SOL are
/// never spent on Pump account setup or included in a user's settlement.
///
/// Pump's accounts are followed by one local wrapped-SOL destination. Cashback
/// curves use sell_v2's 26 accounts; other curves use sell_v3's 17 accounts.
pub fn sol_sell_accounts_len(data:&[u8])->Result<usize> {
    Ok(if CurveState::read(data)?.cashback {27}else{18})
}
pub fn execute_sol_sell<'info>(
    pool:Pubkey,authority_bump:u8,expected_curve:Pubkey,expected_mint:Pubkey,
    accounts:&[AccountInfo<'info>],amount:u64,min_out:u64,
)->Result<(u64,u64)> {
    execute_sell(pool,authority_bump,expected_curve,expected_mint,mainnet_pump::SOL,accounts,amount,min_out)
}
/// Token-quoted proceeds stay in the authority's quote ATA. The service must
/// prepare all rent-bearing accounts before this atomic custody operation.
pub fn execute_token_sell<'info>(
    pool:Pubkey,authority_bump:u8,expected_curve:Pubkey,expected_mint:Pubkey,expected_quote:Pubkey,
    accounts:&[AccountInfo<'info>],amount:u64,min_out:u64,
)->Result<(u64,u64)> {
    require!(expected_quote!=Pubkey::default() && expected_quote!=mainnet_pump::SOL &&
        expected_quote!=token_2022::spl_token_2022::native_mint::ID,MixError::Binding);
    execute_sell(pool,authority_bump,expected_curve,expected_mint,expected_quote,accounts,amount,min_out)
}
fn execute_sell<'info>(
    pool:Pubkey,authority_bump:u8,expected_curve:Pubkey,expected_mint:Pubkey,expected_quote:Pubkey,
    accounts:&[AccountInfo<'info>],amount:u64,min_out:u64,
)->Result<(u64,u64)> {
    use anchor_lang::solana_program::{instruction::{AccountMeta,Instruction},program::{invoke,invoke_signed},system_instruction};
    require!(amount>0 && min_out>0,MixError::Amount);
    let native=expected_quote==mainnet_pump::SOL;
    let extra=usize::from(native);
    require!(accounts.len()==17+extra||accounts.len()==26+extra,MixError::Binding);
    let v2=accounts.len()==26+extra;
    let (curve_i,vault_i,user_i,source_i,user_quote_i,volume_i,fees_i,buyback_i,system_i,event_i,program_i)=
        if v2 {(10,11,13,14,15,19,21,8,23,24,25)}else{(5,6,8,9,10,11,12,13,14,15,16)};
    let pump_len=if v2{26}else{17};
    let destination_i=if native{pump_len}else{user_quote_i};
    let seeds:&[&[u8]]=&[b"authority",pool.as_ref(),&[authority_bump]];
    let authority=Pubkey::create_program_address(seeds,&crate::ID).map_err(|_|error!(MixError::Binding))?;
    bound(&accounts[user_i],authority,anchor_lang::system_program::ID)?;
    require!(accounts[user_i].data_is_empty(),MixError::Binding);
    let route=SolCurveAccounts {curve:&accounts[curve_i],mint:&accounts[1],base_vault:&accounts[vault_i],global:&accounts[0],fees:&accounts[fees_i]};
    let quote_vault_i=if v2{12}else{7};
    let expected=if native {route.validate(expected_curve,expected_mint,&Rent::get()?,false)?}
        else {TokenCurveAccounts{curve:route,quote_mint:&accounts[2],quote_vault:&accounts[quote_vault_i]}
            .validate(expected_curve,expected_mint,expected_quote,false)?}.sell(amount)?;
    require!(expected>=min_out,MixError::Quote);
    let curve=accounts[curve_i].try_borrow_data()?;let state=CurveState::read(&curve)?;
    require!((v2||!state.cashback) && curve.len()>=166,MixError::Binding);
    drop(curve);
    let pda=|seeds:&[&[u8]]|Pubkey::find_program_address(seeds,&ID).0;
    let quote_program=if native{token::ID}else{*accounts[2].owner};
    let ata=|owner:Pubkey|Pubkey::find_program_address(&[owner.as_ref(),quote_program.as_ref(),expected_quote.as_ref()],&anchor_spl::associated_token::ID).0;
    for(index,address)in [(2,expected_quote),(3,*accounts[1].owner),(4,quote_program),
        (quote_vault_i,ata(expected_curve)),(user_quote_i,ata(authority)),(volume_i,pda(&[b"user_volume_accumulator",authority.as_ref()])),
        (system_i,anchor_lang::system_program::ID),(event_i,pda(&[b"__event_authority"])),(program_i,ID)] {
        require_keys_eq!(accounts[index].key(),address,MixError::Binding);
    }
    for index in [3,4,system_i,program_i] {require!(accounts[index].executable,MixError::Binding);}
    require!(*accounts[volume_i].owner==ID && !accounts[volume_i].executable && accounts[volume_i].data_len()>=137,MixError::Binding);
    let global=accounts[0].try_borrow_data()?;
    let buyback=accounts[buyback_i].key();let mut valid=false;
    for i in 0..8 {
        let wallet=Pubkey::new_from_array(bytes(&global,741+i*32)?);
        valid |= wallet!=Pubkey::default() && wallet!=authority && buyback==if native||v2{wallet}else{ata(wallet)};
    }
    require!(valid,MixError::Binding);
    if v2 {
        let recipient=accounts[6].key();let mut fee_valid=false;
        for i in 0..8 {
            let at=if state.mayhem {if i==0{483}else{516+(i-1)*32}}else{if i==0{41}else{162+(i-1)*32}};
            fee_valid|=recipient!=Pubkey::default() && recipient==Pubkey::new_from_array(bytes(&global,at)?);
        }
        require!(fee_valid && recipient!=authority,MixError::Binding);
        let creator=pda(&[b"creator-vault",state.creator.as_ref()]);
        let sharing=Pubkey::find_program_address(&[b"sharing-config",expected_mint.as_ref()],&FEES_ID).0;
        for(index,address)in [(5,anchor_spl::associated_token::ID),(7,ata(recipient)),(9,ata(buyback)),
            (16,creator),(17,ata(creator)),(18,sharing),(20,ata(accounts[volume_i].key())),(22,FEES_ID)] {
            require_keys_eq!(accounts[index].key(),address,MixError::Binding);
        }
        require!(accounts[5].executable && accounts[22].executable,MixError::Binding);
    }
    drop(global);
    let source=&accounts[source_i];let destination=&accounts[destination_i];
    require!(source.key()!=destination.key() && source.key()!=accounts[vault_i].key() && destination.key()!=ata(expected_curve),MixError::Binding);
    let base0=mainnet_pump::token_balance(source,expected_mint,authority,*accounts[1].owner)?;
    mainnet_pump::token_balance(destination,expected_quote,authority,quote_program)?;
    require!(u32::from_le_bytes(bytes(&destination.try_borrow_data()?,109)?)==u32::from(native),MixError::Binding);
    require!(base0>=amount,MixError::Inventory);
    // Normalize any earlier native-SOL donation before measuring this swap.
    let sync=token::spl_token::instruction::sync_native(&token::ID,&destination.key())?;
    if native {invoke(&sync,&[destination.clone(),accounts[4].clone()])?;}
    let quote0=mainnet_pump::token_balance(destination,expected_quote,authority,quote_program)?;
    let native0=accounts[user_i].lamports();
    let mut data=if v2{vec![93,246,130,60,231,233,64,178]}else{vec![28,146,222,119,38,196,105,213]};
    data.extend_from_slice(&amount.to_le_bytes());data.extend_from_slice(&min_out.to_le_bytes());
    let instruction=Instruction {program_id:ID,data,accounts:accounts[..pump_len].iter().map(|a|AccountMeta {
        pubkey:a.key(),is_writable:a.is_writable,is_signer:a.key()==authority,
    }).collect()};
    invoke_signed(&instruction,&accounts[..pump_len],&[seeds])?;
    let received=if native {accounts[user_i].lamports().checked_sub(native0).ok_or(MixError::Binding)?}
        else {mainnet_pump::token_balance(destination,expected_quote,authority,quote_program)?.checked_sub(quote0).ok_or(MixError::Binding)?};
    require!(base0.checked_sub(mainnet_pump::token_balance(source,expected_mint,authority,*accounts[1].owner)?)==Some(amount)
        && received==expected && received>=min_out,MixError::Quote);
    if native {
        let wrap=system_instruction::transfer(&authority,&destination.key(),received);
        invoke_signed(&wrap,&[accounts[user_i].clone(),destination.clone(),accounts[system_i].clone()],&[seeds])?;
        invoke(&sync,&[destination.clone(),accounts[4].clone()])?;
    }
    require!(accounts[user_i].lamports()==native0 && mainnet_pump::token_balance(destination,expected_quote,authority,quote_program)?
        ==quote0.checked_add(received).ok_or(MixError::Arithmetic)?,MixError::Binding);
    Ok((amount,received))
}
