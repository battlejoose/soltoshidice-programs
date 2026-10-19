#![allow(unexpected_cfgs, deprecated)]
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{account_info::{next_account_info, AccountInfo}, clock::Clock, entrypoint::ProgramResult, hash::hashv, msg, program::{invoke, invoke_signed}, program_error::ProgramError, pubkey::Pubkey, rent::Rent, system_instruction, system_program, sysvar::Sysvar};
use spl_token_2022::{extension::{BaseStateWithExtensions, StateWithExtensions, ExtensionType}, state::{Account as TokenAccount, Mint}};
mod orao;
pub mod shared;
pub mod economy;
mod banking;
pub mod funding;
pub mod tournament;
pub mod blackjack;
pub mod slots;
#[cfg(not(feature="no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

// Fixed layouts are consumed by chain/client.ts. This program is a devnet prototype.
pub const BANK_LEN: usize = 1+32+32+32+8+8+1;
pub const ROUND_LEN: usize = 1+32+32+8+8+32+32+32+1+8+3+3+1;
pub const BANK_REVEAL_SECONDS: i64 = 120;
pub const PLAYER_REVEAL_SECONDS: i64 = 180;

#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Bank { pub version:u8, pub authority:Pubkey, pub mint:Pubkey, pub vault:Pubkey, pub reserved:u64, pub max_wager:u64, pub bump:u8 }
#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Round { pub version:u8, pub bank:Pubkey, pub player:Pubkey, pub nonce:u64, pub wager:u64, pub bank_hash:[u8;32], pub player_hash:[u8;32], pub bank_seed:[u8;32], pub state:u8, pub deadline:i64, pub bank_dice:[u8;3], pub player_dice:[u8;3], pub outcome:u8 }

// Instruction tags: 0 initialize(u64 max); 1 offer(u64 nonce,u64 wager,[u8;32] commitment);
// 2 accept([u8;32] player commitment); 3 bank reveal([u8;32]); 4 player reveal([u8;32]);
// 5 timeout; 6 withdraw(u64); 7 close finished/expired offer.
fn check(ok:bool,code:u32)->ProgramResult { if ok {Ok(())}else{Err(ProgramError::Custom(code))} }
fn signer(a:&AccountInfo)->ProgramResult {check(a.is_signer,1)}
fn u64_at(data:&[u8],offset:usize)->Result<u64,ProgramError>{Ok(u64::from_le_bytes(data.get(offset..offset+8).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))}
fn bytes32(data:&[u8],offset:usize)->Result<[u8;32],ProgramError>{data.get(offset..offset+32).ok_or(ProgramError::InvalidInstructionData)?.try_into().map_err(|_|ProgramError::InvalidInstructionData)}
fn write<T:BorshSerialize>(account:&AccountInfo,value:&T)->ProgramResult{value.serialize(&mut &mut account.try_borrow_mut_data()?[..]).map_err(|_|ProgramError::InvalidAccountData)}
fn load_bank(program:&Pubkey,a:&AccountInfo)->Result<Bank,ProgramError>{
 check(a.owner==program,2)?;let b=Bank::try_from_slice(&a.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)?;
 let (expected,bump)=Pubkey::find_program_address(&[b"bank",b.authority.as_ref(),b.mint.as_ref()],program);check(expected==*a.key&&b.bump==bump&&(1..=4).contains(&b.version),3)?;Ok(b)
}
fn load_round(program:&Pubkey,a:&AccountInfo,bank:&Pubkey)->Result<Round,ProgramError>{
 check(a.owner==program,2)?;let r=Round::try_from_slice(&a.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)?;
 let (expected,_)=Pubkey::find_program_address(&[b"round",bank.as_ref(),&r.nonce.to_le_bytes()],program);check(expected==*a.key&&r.bank==*bank&&r.version==1,3)?;Ok(r)
}
fn valid_mint(a:&AccountInfo)->Result<u8,ProgramError>{
 check(*a.owner==spl_token_2022::id(),4)?;let data=a.try_borrow_data()?;let m=StateWithExtensions::<Mint>::unpack(&data)?;
 // Display metadata does not change transfers or balances. Every other extension
 // remains rejected, including transfer fees, hooks, delegates and pausing.
 check(m.get_extension_types()?.iter().all(|e| matches!(e, ExtensionType::MetadataPointer | ExtensionType::TokenMetadata))&&m.base.freeze_authority.is_none(),5)?;Ok(m.base.decimals)
}
fn token_account(a:&AccountInfo,mint:&Pubkey,owner:&Pubkey)->Result<u64,ProgramError>{
 check(*a.owner==spl_token_2022::id(),4)?;let data=a.try_borrow_data()?;let t=StateWithExtensions::<TokenAccount>::unpack(&data)?;
 check(t.base.mint==*mint&&t.base.owner==*owner&&t.base.delegate.is_none()&&t.base.close_authority.is_none(),6)?;check(!t.base.is_frozen(),6)?;Ok(t.base.amount)
}
fn create_pda<'a>(payer:&AccountInfo<'a>,account:&AccountInfo<'a>,system:&AccountInfo<'a>,program:&Pubkey,len:usize,seeds:&[&[u8]])->ProgramResult{
 check(*system.key==system_program::id(),7)?;check(account.data_is_empty()&&account.owner==&system_program::id(),8)?;
 let rent=Rent::get()?.minimum_balance(len);
 // Supports prefunded PDAs instead of allowing a dust transfer to permanently grief creation.
 if account.lamports()==0{invoke_signed(&system_instruction::create_account(payer.key,account.key,rent,len as u64,program),&[payer.clone(),account.clone(),system.clone()],&[seeds])}
 else{let needed=rent.saturating_sub(account.lamports());if needed>0{invoke(&system_instruction::transfer(payer.key,account.key,needed),&[payer.clone(),account.clone(),system.clone()])?;}invoke_signed(&system_instruction::allocate(account.key,len as u64),&[account.clone(),system.clone()],&[seeds])?;invoke_signed(&system_instruction::assign(account.key,program),&[account.clone(),system.clone()],&[seeds])}
}
fn transfer<'a>(from:&AccountInfo<'a>,to:&AccountInfo<'a>,mint:&AccountInfo<'a>,authority:&AccountInfo<'a>,token:&AccountInfo<'a>,amount:u64,decimals:u8,seeds:Option<&[&[u8]]>)->ProgramResult{
 check(*token.key==spl_token_2022::id(),4)?;let ix=spl_token_2022::instruction::transfer_checked(token.key,from.key,mint.key,to.key,authority.key,&[],amount,decimals)?;
 let infos=[from.clone(),mint.clone(),to.clone(),authority.clone(),token.clone()];if let Some(s)=seeds{invoke_signed(&ix,&infos,&[s])}else{invoke(&ix,&infos)}
}
pub fn score(mut d:[u8;3])->u16{d.sort();let [a,b,c]=d;if a==c {200+a as u16}else if d==[4,5,6]{100}else if d==[1,2,3]{1}else if a==b{10+c as u16}else if b==c{10+a as u16}else{0}}
pub fn dice_for(bank_seed:&[u8;32],player_seed:&[u8;32],bank:&Pubkey,player:&Pubkey,nonce:u64,side:u8)->[u8;3]{
 for attempt in 0..32u8{let mut dice=[0u8;3];let mut n=0;
  for block in 0..8u8{let h=hashv(&[b"hooddice:onchain:v1",bank_seed,player_seed,bank.as_ref(),player.as_ref(),&nonce.to_le_bytes(),&[side,attempt,block]]).to_bytes();for b in h{if b<252&&n<3{dice[n]=b%6+1;n+=1;}}if n==3{break;}}
  if n<3{return [0,0,0];}if score(dice)>0{return dice;}
 }[0,0,0]
}
pub fn result(bank:[u8;3],player:[u8;3])->u8{if bank==[0,0,0]||player==[0,0,0]{2}else{let b=score(bank);let p=score(player);if p>b{1}else if p==b{2}else{0}}}
fn payout(wager:u64,outcome:u8)->Result<u64,ProgramError>{match outcome{0=>Ok(0),1=>wager.checked_mul(2).ok_or(ProgramError::ArithmeticOverflow),2=>Ok(wager),_=>Err(ProgramError::InvalidArgument)}}

pub fn process_instruction(program:&Pubkey,accounts:&[AccountInfo],data:&[u8])->ProgramResult{
 if data.starts_with(&slots::CALLBACK) { return slots::callback(program, accounts, data); }
 if data.starts_with(&blackjack::magicblock::CALLBACK) { return blackjack::magicblock::callback(program, accounts, data); }
 let tag=*data.first().ok_or(ProgramError::InvalidInstructionData)?;
 if (112..=114).contains(&tag) { return tournament::games::process(program, accounts, data); }
 if (106..=111).contains(&tag) { return slots::process(program, accounts, data); }
 if (102..=105).contains(&tag) { return blackjack::network_funding::process(program, accounts, data); }
 if tag == 100 || tag == 101 { return blackjack::magicblock::configure(program, accounts, data); }
 if (95..=99).contains(&tag) { return blackjack::sol_funding::process(program, accounts, data); }
 if (80..=94).contains(&tag) { return blackjack::process(program, accounts, data); }
 if (60..=70).contains(&tag) || tag == 115 { return tournament::process(program, accounts, data); }
 if (40..=54).contains(&tag) { return funding::process(program, accounts, data); }
 if tag>=18 { return economy::process(program, accounts, data); }
 if tag>=8 { return shared::process(program, accounts, data); }
 let mut iter=accounts.iter();
 if tag==0{
  check(data.len()==9,9)?;let authority=next_account_info(&mut iter)?;let bank_account=next_account_info(&mut iter)?;let mint=next_account_info(&mut iter)?;let vault=next_account_info(&mut iter)?;let system=next_account_info(&mut iter)?;
  signer(authority)?;valid_mint(mint)?;let max=u64_at(data,1)?;check(max>0&&max<=u64::MAX/4,10)?;
  let (expected,bump)=Pubkey::find_program_address(&[b"bank",authority.key.as_ref(),mint.key.as_ref()],program);check(expected==*bank_account.key,3)?;token_account(vault,mint.key,bank_account.key)?;
  create_pda(authority,bank_account,system,program,BANK_LEN,&[b"bank",authority.key.as_ref(),mint.key.as_ref(),&[bump]])?;
  return write(bank_account,&Bank{version:1,authority:*authority.key,mint:*mint.key,vault:*vault.key,reserved:0,max_wager:max,bump});
 }
 if tag==1{
  check(data.len()==49,9)?;let authority=next_account_info(&mut iter)?;let bank_account=next_account_info(&mut iter)?;let round_account=next_account_info(&mut iter)?;let player=next_account_info(&mut iter)?;let system=next_account_info(&mut iter)?;
  signer(authority)?;let bank=load_bank(program,bank_account)?;check(bank.authority==*authority.key&&bank.version==1,11)?;
  let nonce=u64_at(data,1)?;let wager=u64_at(data,9)?;check(wager>0&&wager<=bank.max_wager,10)?;let bank_hash=bytes32(data,17)?;
  let (expected,bump)=Pubkey::find_program_address(&[b"round",bank_account.key.as_ref(),&nonce.to_le_bytes()],program);check(expected==*round_account.key,3)?;
  create_pda(authority,round_account,system,program,ROUND_LEN,&[b"round",bank_account.key.as_ref(),&nonce.to_le_bytes(),&[bump]])?;
  return write(round_account,&Round{version:1,bank:*bank_account.key,player:*player.key,nonce,wager,bank_hash,player_hash:[0;32],bank_seed:[0;32],state:0,deadline:Clock::get()?.unix_timestamp+120,bank_dice:[0;3],player_dice:[0;3],outcome:255});
 }
 if tag==3{
  check(data.len()==33,9)?;let authority=next_account_info(&mut iter)?;let bank_account=next_account_info(&mut iter)?;let round_account=next_account_info(&mut iter)?;signer(authority)?;
  let bank=load_bank(program,bank_account)?;check(bank.authority==*authority.key,11)?;let mut r=load_round(program,round_account,bank_account.key)?;
  check(r.state==1&&Clock::get()?.unix_timestamp<=r.deadline,12)?;let seed=bytes32(data,1)?;check(hashv(&[&seed]).to_bytes()==r.bank_hash,13)?;
  r.bank_seed=seed;r.state=2;r.deadline=Clock::get()?.unix_timestamp+PLAYER_REVEAL_SECONDS;return write(round_account,&r);
 }
 if tag==7{
  let authority=next_account_info(&mut iter)?;let bank_account=next_account_info(&mut iter)?;let round_account=next_account_info(&mut iter)?;signer(authority)?;
  let bank=load_bank(program,bank_account)?;check(bank.authority==*authority.key,11)?;let r=load_round(program,round_account,bank_account.key)?;
  check(r.state==3||(r.state==0&&Clock::get()?.unix_timestamp>r.deadline),12)?;
  **authority.try_borrow_mut_lamports()?=authority.lamports().checked_add(round_account.lamports()).ok_or(ProgramError::ArithmeticOverflow)?;
  **round_account.try_borrow_mut_lamports()?=0;round_account.try_borrow_mut_data()?.fill(0);return Ok(());
 }
 // Common money-moving accounts: actor, bank, [round for 2/4/5], mint, vault, player token, Token-2022.
 let actor=next_account_info(&mut iter)?;let bank_account=next_account_info(&mut iter)?;
 let round_account=if tag!=6{Some(next_account_info(&mut iter)?)}else{None};
 let mint=next_account_info(&mut iter)?;let vault=next_account_info(&mut iter)?;let destination=next_account_info(&mut iter)?;let token=next_account_info(&mut iter)?;
 let mut bank=load_bank(program,bank_account)?;check(*mint.key==bank.mint&&*vault.key==bank.vault,14)?;check(*token.key==spl_token_2022::id(),4)?;
 let decimals=valid_mint(mint)?;let balance=token_account(vault,mint.key,bank_account.key)?;
 let authority_key=bank.authority;let mint_key=bank.mint;let bump=[bank.bump];let seeds:&[&[u8]]=&[b"bank",authority_key.as_ref(),mint_key.as_ref(),&bump];
 if tag==6{
  check(bank.version==1,64)?;
  check(data.len()==9,9)?;signer(actor)?;check(*actor.key==bank.authority,11)?;let amount=u64_at(data,1)?;
  token_account(destination,mint.key,actor.key)?;check(balance.checked_sub(bank.reserved).ok_or(ProgramError::InsufficientFunds)? >= amount,15)?;
  return transfer(vault,destination,mint,bank_account,token,amount,decimals,Some(seeds));
 }
 let round_account=round_account.ok_or(ProgramError::NotEnoughAccountKeys)?;let mut r=load_round(program,round_account,bank_account.key)?;let now=Clock::get()?.unix_timestamp;
 token_account(destination,mint.key,&r.player)?;
 match tag{
  2=>{
   check(bank.version==1,64)?;
   check(data.len()==33,9)?;signer(actor)?;check(*actor.key==r.player,11)?;check(r.state==0&&now<=r.deadline,12)?;
   check(balance.checked_sub(bank.reserved).ok_or(ProgramError::InsufficientFunds)?>=r.wager,15)?;
   let liability=r.wager.checked_mul(2).ok_or(ProgramError::ArithmeticOverflow)?;bank.reserved=bank.reserved.checked_add(liability).ok_or(ProgramError::ArithmeticOverflow)?;
   r.player_hash=bytes32(data,1)?;r.state=1;r.deadline=now+BANK_REVEAL_SECONDS;
   transfer(destination,vault,mint,actor,token,r.wager,decimals,None)?;
  },
  4|5=>{
   if tag==4{
    check(data.len()==33,9)?;check(r.state==2&&now<=r.deadline,12)?;let seed=bytes32(data,1)?;check(hashv(&[&seed]).to_bytes()==r.player_hash,13)?;
    // Anyone may relay a valid seed; destination is fixed to the committed player's token account.
    r.bank_dice=dice_for(&r.bank_seed,&seed,bank_account.key,&r.player,r.nonce,0);r.player_dice=dice_for(&r.bank_seed,&seed,bank_account.key,&r.player,r.nonce,1);r.outcome=result(r.bank_dice,r.player_dice);
   }else{check(data.len()==1,9)?;check((r.state==1||r.state==2)&&now>r.deadline,12)?;r.outcome=if r.state==1{1}else{0};}
   bank.reserved=bank.reserved.checked_sub(r.wager.checked_mul(2).ok_or(ProgramError::ArithmeticOverflow)?).ok_or(ProgramError::ArithmeticOverflow)?;
   let amount=payout(r.wager,r.outcome)?;if amount>0{transfer(vault,destination,mint,bank_account,token,amount,decimals,Some(seeds))?;}r.state=3;
   msg!("HOODDICE_SETTLED nonce={} outcome={} wager={} bank={:?} player={:?}",r.nonce,r.outcome,r.wager,r.bank_dice,r.player_dice);
  },
  _=>return Err(ProgramError::InvalidInstructionData)
 }
 write(bank_account,&bank)?;write(round_account,&r)
}

#[cfg(test)]mod tests{use super::*;
 #[test]fn rankings(){assert!(score([1,1,1])>score([4,5,6]));assert!(score([4,5,6])>score([1,1,6]));assert!(score([2,2,1])>score([1,2,3]));assert_eq!(score([1,3,5]),0);assert_eq!(score([2,2,4]),score([4,6,6]));}
 #[test]fn every_hand(){let mut counts=[0;5];for a in 1..=6{for b in 1..=6{for c in 1..=6{let d=[a,b,c];let s=score(d);assert_eq!(s,score([c,a,b]));counts[if s==0{0}else if s==1{1}else if s==100{2}else if s>200{3}else{4}]+=1;}}}assert_eq!(counts,[108,6,6,6,90]);}
 #[test]fn deterministic_and_valid(){let bank=Pubkey::new_from_array([3;32]);let player=Pubkey::new_from_array([4;32]);for n in 0..100{let d=dice_for(&[1;32],&[2;32],&bank,&player,n,0);assert!(d.iter().all(|v|*v>=1&&*v<=6));assert!(score(d)>0);assert_eq!(d,dice_for(&[1;32],&[2;32],&bank,&player,n,0));}}
 #[test]fn payouts_conserve(){for wager in [1,10,500,1000000]{assert_eq!(payout(wager,0).unwrap(),0);assert_eq!(payout(wager,1).unwrap(),2*wager);assert_eq!(payout(wager,2).unwrap(),wager);}assert!(payout(u64::MAX,1).is_err());}
 #[test]fn layout_lengths(){let b=Bank{version:1,authority:Pubkey::default(),mint:Pubkey::default(),vault:Pubkey::default(),reserved:0,max_wager:1,bump:1};assert_eq!(borsh::to_vec(&b).unwrap().len(),BANK_LEN);let r=Round{version:1,bank:Pubkey::default(),player:Pubkey::default(),nonce:0,wager:1,bank_hash:[0;32],player_hash:[0;32],bank_seed:[0;32],state:0,deadline:0,bank_dice:[0;3],player_dice:[0;3],outcome:255};assert_eq!(borsh::to_vec(&r).unwrap().len(),ROUND_LEN);}
}
