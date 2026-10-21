#![allow(unexpected_cfgs, deprecated)]
//! Isolated RPS/fights tournament program. No bank or cash-game account is writable.
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{account_info::{next_account_info,AccountInfo},bpf_loader_upgradeable,
    clock::Clock,entrypoint::ProgramResult,hash::hashv,program::{invoke,invoke_signed},
    program_error::ProgramError,pubkey::Pubkey,rent::Rent,system_instruction,system_program,sysvar::Sysvar};
use spl_token_2022::{extension::{BaseStateWithExtensions,ExtensionType,StateWithExtensions},state::{Account as TokenAccount,Mint}};
pub mod rules;
use rules::*;
#[path="../../fight-program/src/combat.rs"] pub mod combat;
#[path="../../rps-program/src/rules.rs"] pub mod rps;
#[path="../../program/src/orao.rs"] mod orao;
mod tournament;
mod matches;
#[cfg(not(feature="no-entrypoint"))] solana_program::entrypoint!(process_instruction);

pub const CONFIG_LEN:usize=34;
pub const SERIES_LEN:usize=43;
pub const EVENT_LEN:usize=4096;
pub const ENTRY_LEN:usize=107;
pub const MATCH_LEN:usize=1280;
pub const STEP_FEE:u64=5_000;
pub const MOVE_FEE:u64=10_000;
pub const RPS_CREDIT:u64=5_000_000;

#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Config {pub version:u8,pub mint:Pubkey,pub bump:u8}
#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Series {pub version:u8,pub game:u8,pub next:u64,pub active:Pubkey,pub bump:u8}
#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Event {
    pub version:u8,pub game:u8,pub nonce:u64,pub sponsor:Pubkey,pub mint:Pubkey,
    pub prize:u64,pub entry_fee:u64,pub total:u64,pub starts:i64,pub closes:i64,pub deadline:i64,
    pub best_of:u8,pub capacity:u16,
    // 0 registration, 1 seeding VRF, 2 playing, 3 completed, 4 cancelled.
    pub phase:u8,pub bump:u8,pub match_credit:u64,pub players:Vec<Pubkey>,pub nodes:Vec<u8>,
    pub semifinal_losers:[u8;2],pub places:[u8;3],pub claimed:u8,pub refunded:u16,
    pub sponsor_refunded:bool,pub seed:[u8;32],pub random:[u8;64],pub oracle_floor:u64,
}
#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Entry {pub version:u8,pub event:Pubkey,pub player:Pubkey,pub session:Pubkey,pub paid:u64,pub refunded:bool,pub bump:u8}
#[derive(BorshSerialize,BorshDeserialize,Debug,Clone)]
pub struct Match {
    pub version:u8,pub event:Pubkey,pub index:u16,pub players:[u8;2],pub sessions:[Pubkey;2],
    // 0 fight ready, 1 RPS commit, 2 RPS reveal, 3 fight VRF pending, 4 fight combat, 5 decided, 6 recorded.
    pub phase:u8,pub bump:u8,pub wins:[u8;2],pub winner:u8,pub round:u32,pub deadline:i64,
    pub committed:u8,pub revealed:u8,pub hashes:[[u8;32];2],pub choices:[u8;2],
    pub transcript:[u8;32],pub last_moves:[u8;2],pub last_winner:u8,
    pub seed:[u8;32],pub random:[u8;64],pub oracle_floor:u64,pub first:u8,pub hp:[u8;2],pub strikes:Vec<combat::Strike>,
}

fn check(ok:bool,code:u32)->ProgramResult {if ok {Ok(())}else{Err(ProgramError::Custom(code))}}
fn signer(a:&AccountInfo)->ProgramResult {check(a.is_signer,1)}
fn u64_at(d:&[u8],o:usize)->Result<u64,ProgramError>{Ok(u64::from_le_bytes(d.get(o..o+8).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))}
fn read<T:BorshDeserialize>(program:&Pubkey,a:&AccountInfo,len:usize)->Result<T,ProgramError>{
    check(a.owner==program && a.data_len()==len,2)?;
    T::deserialize(&mut &a.try_borrow_data()?[..]).map_err(|_|ProgramError::InvalidAccountData)
}
fn write<T:BorshSerialize>(a:&AccountInfo,v:&T)->ProgramResult{
    let bytes=borsh::to_vec(v)?;check(bytes.len()<=a.data_len(),2)?;
    let mut d=a.try_borrow_mut_data()?;d.fill(0);d[..bytes.len()].copy_from_slice(&bytes);Ok(())
}
fn series(program:&Pubkey,a:&AccountInfo)->Result<Series,ProgramError>{
    let s:Series=read(program,a,SERIES_LEN)?;let(k,b)=Pubkey::find_program_address(&[b"bracket-series-v1",&[s.game]],program);
    check(s.version==1 && s.game<2 && *a.key==k && b==s.bump,3)?;Ok(s)
}
fn event(program:&Pubkey,a:&AccountInfo)->Result<Event,ProgramError>{
    let e:Event=read(program,a,EVENT_LEN)?;let(k,b)=Pubkey::find_program_address(&[b"bracket-event-v1",&[e.game],&e.nonce.to_le_bytes()],program);
    check(e.version==1 && e.game<2 && *a.key==k && b==e.bump && e.players.len()<=MAX_PLAYERS && e.nodes.len()<=256,3)?;Ok(e)
}
fn entry(program:&Pubkey,a:&AccountInfo,e:&Pubkey,p:&Pubkey)->Result<Entry,ProgramError>{
    let x:Entry=read(program,a,ENTRY_LEN)?;let(k,b)=Pubkey::find_program_address(&[b"bracket-entry-v1",e.as_ref(),p.as_ref()],program);
    check(x.version==1 && x.event==*e && x.player==*p && *a.key==k && x.bump==b,3)?;Ok(x)
}
fn game(program:&Pubkey,a:&AccountInfo,e:&Pubkey)->Result<Match,ProgramError>{
    let m:Match=read(program,a,MATCH_LEN)?;let(k,b)=Pubkey::find_program_address(&[b"bracket-match-v1",e.as_ref(),&m.index.to_le_bytes()],program);
    check(m.version==1 && m.event==*e && *a.key==k && m.bump==b && m.strikes.len()<=64,3)?;Ok(m)
}
fn valid_mint(a:&AccountInfo)->ProgramResult{
    check(*a.owner==spl_token_2022::id(),4)?;let d=a.try_borrow_data()?;let m=StateWithExtensions::<Mint>::unpack(&d)?;
    check(m.base.decimals==6 && m.base.freeze_authority.is_none() && m.get_extension_types()?.iter().all(|e|matches!(e,ExtensionType::MetadataPointer|ExtensionType::TokenMetadata)),5)
}
fn tokens(a:&AccountInfo,mint:&Pubkey,owner:&Pubkey,escrow:bool)->Result<u64,ProgramError>{
    check(*a.owner==spl_token_2022::id(),4)?;let d=a.try_borrow_data()?;let t=StateWithExtensions::<TokenAccount>::unpack(&d)?;
    check(t.base.mint==*mint && t.base.owner==*owner && !t.base.is_frozen() && (!escrow || t.base.delegate.is_none() && t.base.close_authority.is_none()),6)?;Ok(t.base.amount)
}
fn vault(program:&Pubkey,a:&AccountInfo,e:&AccountInfo,mint:&Pubkey)->ProgramResult{
    check(*a.key==Pubkey::find_program_address(&[b"bracket-vault-v1",e.key.as_ref()],program).0,3)?;tokens(a,mint,e.key,true)?;Ok(())
}
fn create<'a>(payer:&AccountInfo<'a>,a:&AccountInfo<'a>,system:&AccountInfo<'a>,owner:&Pubkey,len:usize,seeds:&[&[u8]])->ProgramResult{
    signer(payer)?;check(*system.key==system_program::id() && a.owner==&system_program::id() && a.data_is_empty(),7)?;
    let required=Rent::get()?.minimum_balance(len).saturating_sub(a.lamports());
    if required>0 {invoke(&system_instruction::transfer(payer.key,a.key,required),&[payer.clone(),a.clone(),system.clone()])?;}
    invoke_signed(&system_instruction::allocate(a.key,len as u64),&[a.clone(),system.clone()],&[seeds])?;
    invoke_signed(&system_instruction::assign(a.key,owner),&[a.clone(),system.clone()],&[seeds])
}
fn spend(from:&AccountInfo,to:&AccountInfo,n:u64)->ProgramResult{
    check(from.key!=to.key && from.lamports().saturating_sub(Rent::get()?.minimum_balance(from.data_len()))>=n,8)?;
    let b=to.lamports().checked_add(n).ok_or(ProgramError::ArithmeticOverflow)?;
    **from.try_borrow_mut_lamports()?-=n;**to.try_borrow_mut_lamports()?=b;Ok(())
}
fn transfer<'a>(from:&AccountInfo<'a>,to:&AccountInfo<'a>,mint:&AccountInfo<'a>,authority:&AccountInfo<'a>,token:&AccountInfo<'a>,amount:u64,seeds:Option<&[&[u8]]>)->ProgramResult{
    check(*token.key==spl_token_2022::id() && from.key!=to.key,4)?;
    let ix=spl_token_2022::instruction::transfer_checked(token.key,from.key,mint.key,to.key,authority.key,&[],amount,6)?;
    let infos=[from.clone(),mint.clone(),to.clone(),authority.clone(),token.clone()];
    if let Some(s)=seeds {invoke_signed(&ix,&infos,&[s])}else{invoke(&ix,&infos)}
}
fn oracle_payer(program:&Pubkey,owner:&Pubkey)->(Pubkey,u8){Pubkey::find_program_address(&[b"bracket-oracle-v1",owner.as_ref()],program)}
fn request<'a>(program:&Pubkey,owner:&AccountInfo<'a>,payer:&AccountInfo<'a>,network:&AccountInfo<'a>,treasury:&AccountInfo<'a>,oracle:&AccountInfo<'a>,random:&AccountInfo<'a>,system:&AccountInfo<'a>,seed:&[u8;32])->Result<u64,ProgramError>{
    let(k,b)=oracle_payer(program,owner.key);check(*payer.key==k && payer.owner==&system_program::id() && payer.data_is_empty(),3)?;
    let floor=payer.lamports();let allowance=orao::request_fee(network)?+Rent::get()?.minimum_balance(1500);
    spend(owner,payer,allowance)?;let ps:&[&[u8]]=&[b"bracket-oracle-v1",owner.key.as_ref(),&[b]];
    orao::request_fresh_signed(payer,network,treasury,random,system,oracle,seed,Some(ps),Some(owner))?;
    let unused=payer.lamports().checked_sub(floor).ok_or(ProgramError::ArithmeticOverflow)?;
    if unused>0 {invoke_signed(&system_instruction::transfer(payer.key,owner.key,unused),&[payer.clone(),owner.clone(),system.clone()],&[ps])?;}
    Ok(floor)
}
fn recover<'a>(program:&Pubkey,owner:&AccountInfo<'a>,payer:&AccountInfo<'a>,system:&AccountInfo<'a>,floor:u64)->ProgramResult{
    let(k,b)=oracle_payer(program,owner.key);check(*payer.key==k && payer.owner==&system_program::id() && payer.data_is_empty() && *system.key==system_program::id(),3)?;
    let n=payer.lamports().saturating_sub(floor);
    if n>0 {invoke_signed(&system_instruction::transfer(payer.key,owner.key,n),&[payer.clone(),owner.clone(),system.clone()],&[&[b"bracket-oracle-v1",owner.key.as_ref(),&[b]]])?;}Ok(())
}
fn active(e:&Event,now:i64)->ProgramResult {check(e.phase==2 && now<e.deadline,20)}

pub fn process_instruction(program:&Pubkey,accounts:&[AccountInfo],data:&[u8])->ProgramResult{
    let tag=*data.first().ok_or(ProgramError::InvalidInstructionData)?;
    if [5,6,7,8,9,10,11,17].contains(&tag){matches::process(program,accounts,data)}else{tournament::process(program,accounts,data)}
}
