#![allow(unexpected_cfgs, deprecated)]
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo}, bpf_loader_upgradeable,
    clock::Clock, entrypoint::ProgramResult, hash::hashv, log::sol_log_data,
    program::{invoke, invoke_signed}, program_error::ProgramError, pubkey::Pubkey,
    rent::Rent, system_instruction, system_program, sysvar::{self, Sysvar},
};
use spl_token_2022::{extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions}, state::{Account as TokenAccount, Mint}};
pub mod rules;
mod common;
use common::*;
use rules::*;
#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub const CONFIG_LEN: usize = 99;
pub const COUNTER_LEN: usize = 42;
pub const MATCH_LEN: usize = 379;
pub const MAX_WAGER: u64 = 10_000_000_000_000;
pub const MAX_SIDE_BUDGET: u64 = 10_000_000;
pub const RETENTION: i64 = 3600;
pub const CLOSE_FEE: u64 = 5_000;
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Config { pub version:u8, pub mint:Pubkey, pub bank_vault:Pubkey, pub bank_owner:Pubkey, pub decimals:u8, pub bump:u8 }
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Counter { pub version:u8, pub player:Pubkey, pub next:u64, pub bump:u8 }
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Match {
    pub version:u8, pub challenger:Pubkey, pub opponent:Pubkey, pub sessions:[Pubkey;2],
    pub nonce:u64, pub wager:u64, pub side_budget:u64, pub credit:u64,
    pub expires:i64, pub accepted_at:i64, pub deadline:i64, pub finished_at:i64,
    // 0 offered, 1 commit, 2 reveal, 3 decided, 4 paid, 5 refunded.
    pub state:u8, pub bump:u8, pub best_of:u8, pub wins:[u8;2], pub winner:u8, pub reason:u8,
    pub round:u32, pub committed:u8, pub revealed:u8, pub hashes:[[u8;32];2],
    pub choices:[u8;2], pub salts:[[u8;32];2], pub last_moves:[u8;2], pub last_winner:u8,
    pub transcript:[u8;32], pub resolved_at:i64,
}
fn reserve() -> Result<u64,ProgramError> { Ok(Rent::get()?.minimum_balance(170)*2 + SETTLE_FEE + CLOSE_FEE) }
fn reward(f:&mut Match, account:&AccountInfo, keeper:&AccountInfo, amount:u64) -> ProgramResult {
    f.credit=f.credit.checked_sub(amount).ok_or(ProgramError::InsufficientFunds)?;
    lamports(account,keeper,amount)
}
fn decide(f:&mut Match, winner:u8, reason:u8, now:i64) { f.state=3;f.winner=winner;f.reason=reason;f.finished_at=now; }
fn word(data:&[u8],start:usize)->Result<usize,ProgramError>{Ok(u16::from_le_bytes(data.get(start..start+2).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()) as usize)}
/// The immediately preceding Ed25519 instruction is executed by Solana, not trusted RPC metadata.
fn authorization_checked(program:&Pubkey, account:&AccountInfo, f:&Match, side:usize, action:u8, payload:&[u8], instructions:&AccountInfo)->ProgramResult {
    check(*instructions.key==sysvar::instructions::id(),120)?;
    let index=sysvar::instructions::load_current_index_checked(instructions)?;
    check(index>0,120)?;
    let ix=sysvar::instructions::load_instruction_at_checked(index as usize-1,instructions)?;
    check(ix.program_id==solana_program::ed25519_program::id() && ix.data.len()>=16 && ix.data[0]==1 && ix.data[1]==0,120)?;
    // Offsets must refer to the same verified instruction, never another instruction's data.
    check(word(&ix.data,4)?==65535 && word(&ix.data,8)?==65535 && word(&ix.data,14)?==65535,120)?;
    let signature=word(&ix.data,2)?;let public=word(&ix.data,6)?;let message=word(&ix.data,10)?;let length=word(&ix.data,12)?;
    check(signature>=16 && public>=16 && message>=16 && ix.data.get(signature..signature+64).is_some(),120)?;
    let key=ix.data.get(public..public+32).ok_or(ProgramError::InvalidInstructionData)?;
    let player=if side==0 {f.challenger} else {f.opponent};
    check(key==f.sessions[side].as_ref() || key==player.as_ref(),121)?;
    let expected=authorization(program,account.key,&player,f.round,action,payload);
    check(length==expected.len() && ix.data.get(message..message+length)==Some(expected.as_slice()),122)
}
fn destination(a:&AccountInfo,mint:&Pubkey,owner:&Pubkey)->ProgramResult {
    check(*a.owner==spl_token_2022::id(),4)?;
    let data=a.try_borrow_data()?;let t=StateWithExtensions::<TokenAccount>::unpack(&data)?;
    check(t.base.mint==*mint && t.base.owner==*owner && !t.base.is_frozen(),6)
}
fn ensure_destination<'a>(f:&mut Match,account:&AccountInfo<'a>,dest:&AccountInfo<'a>,owner:&AccountInfo<'a>,mint:&AccountInfo<'a>,keeper:&AccountInfo<'a>,system:&AccountInfo<'a>,token:&AccountInfo<'a>,associated:&AccountInfo<'a>)->ProgramResult {
    let ata_id=solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    check(*associated.key==ata_id && associated.executable && *token.key==spl_token_2022::id() && *system.key==system_program::id(),4)?;
    check(*dest.key==Pubkey::find_program_address(&[owner.key.as_ref(),token.key.as_ref(),mint.key.as_ref()],&ata_id).0,6)?;
    if dest.owner==&spl_token_2022::id(){return destination(dest,mint.key,owner.key);}
    check(dest.owner==&system_program::id() && dest.data_is_empty(),6)?;
    let allowance=Rent::get()?.minimum_balance(170).saturating_sub(dest.lamports());
    check(f.credit>=allowance+SETTLE_FEE+CLOSE_FEE,102)?;
    let before=keeper.lamports();
    if allowance>0 {lamports(account,keeper,allowance)?;}
    use solana_program::instruction::{AccountMeta,Instruction};
    let ix=Instruction{program_id:ata_id,data:vec![1],accounts:vec![
        AccountMeta::new(*keeper.key,true),AccountMeta::new(*dest.key,false),AccountMeta::new_readonly(*owner.key,false),
        AccountMeta::new_readonly(*mint.key,false),AccountMeta::new_readonly(*system.key,false),AccountMeta::new_readonly(*token.key,false),
        // Include the reserve debit in the CPI balance boundary as well as its payer credit.
        AccountMeta::new(*account.key,false),
    ]};
    invoke(&ix,&[keeper.clone(),dest.clone(),owner.clone(),mint.clone(),system.clone(),token.clone(),account.clone(),associated.clone()])?;
    let cost=before.checked_add(allowance).and_then(|v|v.checked_sub(keeper.lamports())).ok_or(ProgramError::ArithmeticOverflow)?;
    check(cost<=allowance,102)?;
    if allowance>cost {invoke(&system_instruction::transfer(keeper.key,account.key,allowance-cost),&[keeper.clone(),account.clone(),system.clone()])?;}
    f.credit-=cost;
    destination(dest,mint.key,owner.key)
}

pub fn process_instruction(program:&Pubkey,accounts:&[AccountInfo],data:&[u8])->ProgramResult {
    let tag=*data.first().ok_or(ProgramError::InvalidInstructionData)?;
    let mut it=accounts.iter();
    if tag==0 {
        check(data.len()==1,9)?;
        let authority=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;let mint=next_account_info(&mut it)?;
        let bank_vault=next_account_info(&mut it)?;let bank_owner=next_account_info(&mut it)?;let program_data=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(authority)?;
        check(program_data.owner==&bpf_loader_upgradeable::id() && *program_data.key==Pubkey::find_program_address(&[program.as_ref()],&bpf_loader_upgradeable::id()).0,100)?;
        {let d=program_data.try_borrow_data()?;check(d.len()>=45 && d[..4]==3u32.to_le_bytes() && d[12]==1 && &d[13..45]==authority.key.as_ref(),100)?;}
        let decimals=valid_mint(mint)?;token_account(bank_vault,mint.key,bank_owner.key)?;
        let (expected,bump)=Pubkey::find_program_address(&[b"rps-config-v1"],program);check(*cfg.key==expected,3)?;
        create_pda(authority,cfg,system,program,CONFIG_LEN,&[b"rps-config-v1",&[bump]])?;
        return write(cfg,&Config{version:1,mint:*mint.key,bank_vault:*bank_vault.key,bank_owner:*bank_owner.key,decimals,bump});
    }
    if tag==1 {
        check(data.len()==66,9)?;
        let player=next_account_info(&mut it)?;let opponent=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;
        let counter=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;let vault=next_account_info(&mut it)?;
        let mint=next_account_info(&mut it)?;let source=next_account_info(&mut it)?;let token=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(player)?;check(player.key!=opponent.key && *opponent.key!=Pubkey::default(),101)?;
        let c=config(program,cfg)?;check(*mint.key==c.mint && *token.key==spl_token_2022::id(),4)?;
        valid_mint(mint)?;token_account(source,mint.key,player.key)?;
        let nonce=u64_at(data,1)?;let wager=u64_at(data,9)?;let budget=u64_at(data,17)?;
        let expires=i64::from_le_bytes(data[25..33].try_into().unwrap());let best_of=data[33];let session=Pubkey::new_from_array(data[34..66].try_into().unwrap());
        let now=Clock::get()?.unix_timestamp;
        check(wager>0 && wager<=MAX_WAGER && target(best_of).is_some(),10)?;
        check(session!=Pubkey::default() && session!=*opponent.key,101)?;
        check(budget>=Rent::get()?.minimum_balance(170)+MOVES_PER_SIDE_BUDGET+SETTLE_FEE+CLOSE_FEE && budget<=MAX_SIDE_BUDGET,102)?;
        check(expires>now && expires<=now+900,103)?;
        let (ck,cb)=Pubkey::find_program_address(&[b"rps-counter-v1",player.key.as_ref()],program);check(*counter.key==ck,3)?;
        let mut n=if counter.owner==program {let n:Counter=read(program,counter)?;check(n.version==1 && n.player==*player.key && n.bump==cb,3)?;n}
        else {create_pda(player,counter,system,program,COUNTER_LEN,&[b"rps-counter-v1",player.key.as_ref(),&[cb]])?;Counter{version:1,player:*player.key,next:0,bump:cb}};
        check(n.next==nonce,104)?;n.next=n.next.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        let (expected,bump)=Pubkey::find_program_address(&[b"rps-v1",player.key.as_ref(),&nonce.to_le_bytes()],program);check(*account.key==expected,3)?;
        create_pda(player,account,system,program,MATCH_LEN,&[b"rps-v1",player.key.as_ref(),&nonce.to_le_bytes(),&[bump]])?;
        let (vk,vb)=Pubkey::find_program_address(&[b"rps-vault-v1",account.key.as_ref()],program);check(*vault.key==vk,3)?;
        create_pda(player,vault,system,token.key,165,&[b"rps-vault-v1",account.key.as_ref(),&[vb]])?;
        invoke(&spl_token_2022::instruction::initialize_account3(token.key,vault.key,mint.key,account.key)?,&[vault.clone(),mint.clone(),token.clone()])?;
        transfer(source,vault,mint,player,token,wager,None)?;
        invoke(&system_instruction::transfer(player.key,account.key,budget),&[player.clone(),account.clone(),system.clone()])?;
        let f=Match{version:1,challenger:*player.key,opponent:*opponent.key,sessions:[session,Pubkey::default()],nonce,wager,side_budget:budget,credit:budget,
            expires,accepted_at:0,deadline:0,finished_at:0,state:0,bump,best_of,wins:[0,0],winner:NO_WINNER,reason:0,round:0,committed:0,revealed:0,
            hashes:[[0;32];2],choices:[255;2],salts:[[0;32];2],last_moves:[255;2],last_winner:255,transcript:[0;32],resolved_at:0};
        write(counter,&n)?;write(account,&f)?;
        sol_log_data(&[b"RpsOfferedV1",account.key.as_ref(),&borsh::to_vec(&f)?]);return Ok(());
    }
    if tag==2 {
        check(data.len()==33,9)?;
        let player=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;let vault=next_account_info(&mut it)?;
        let mint=next_account_info(&mut it)?;let source=next_account_info(&mut it)?;let token=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;
        signer(player)?;let c=config(program,cfg)?;let mut f=game(program,account)?;let now=Clock::get()?.unix_timestamp;
        check(f.state==0 && now<f.expires && *player.key==f.opponent,105)?;
        let session=Pubkey::new_from_array(data[1..33].try_into().unwrap());
        check(session!=Pubkey::default() && session!=f.challenger && session!=f.sessions[0],101)?;
        check(*mint.key==c.mint,4)?;valid_mint(mint)?;token_account(source,mint.key,player.key)?;escrow(program,vault,account,mint.key)?;
        transfer(source,vault,mint,player,token,f.wager,None)?;
        invoke(&system_instruction::transfer(player.key,account.key,f.side_budget),&[player.clone(),account.clone(),system.clone()])?;
        f.credit=f.credit.checked_add(f.side_budget).ok_or(ProgramError::ArithmeticOverflow)?;
        f.sessions[1]=session;f.state=1;f.accepted_at=now;f.deadline=now+PHASE_SECONDS;
        write(account,&f)?;sol_log_data(&[b"RpsAcceptedV1",account.key.as_ref(),&borsh::to_vec(&f)?]);return Ok(());
    }
    if tag==3 || tag==7 {
        let action=if tag==3 {0} else {1};check(data.len()==if tag==3 {38} else {39},9)?;
        let keeper=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;let instructions=next_account_info(&mut it)?;
        signer(keeper)?;let mut f=game(program,account)?;
        let round=u32::from_le_bytes(data[1..5].try_into().unwrap());let side=data[5] as usize;let now=Clock::get()?.unix_timestamp;
        check(side<2 && round==f.round && now<f.deadline,123)?;
        check(f.credit>=reserve()?+MOVE_FEE,102)?;
        authorization_checked(program,account,&f,side,action,&data[6..],instructions)?;
        let bit=1<<side;
        if tag==3 {
            check(f.state==1 && f.committed&bit==0,124)?;
            check(f.committed!=0 || f.credit>=reserve()?+4*MOVE_FEE,102)?;
            f.hashes[side]=data[6..38].try_into().unwrap();f.committed|=bit;
            if f.committed==3 {f.state=2;f.deadline=now+PHASE_SECONDS;}
            sol_log_data(&[b"RpsCommitV1",account.key.as_ref(),&round.to_le_bytes(),&[side as u8],&f.hashes[side]]);
        } else {
            check(f.state==2 && f.committed==3 && f.revealed&bit==0 && data[6]<3,124)?;
            let choice=data[6];let salt:[u8;32]=data[7..39].try_into().unwrap();let player=if side==0 {f.challenger} else {f.opponent};
            check(commitment(program,account.key,&player,round,choice,&salt)==f.hashes[side],125)?;
            f.choices[side]=choice;f.salts[side]=salt;f.revealed|=bit;
            sol_log_data(&[b"RpsRevealV1",account.key.as_ref(),&round.to_le_bytes(),&[side as u8,choice],&salt]);
            if f.revealed==3 {
                let w=winner(f.choices[0],f.choices[1]).ok_or(ProgramError::InvalidInstructionData)?;
                f.last_moves=f.choices;f.last_winner=w;f.resolved_at=now;
                f.transcript=hashv(&[b"hooddice:rps:transcript:v1",&f.transcript,account.key.as_ref(),&round.to_le_bytes(),&f.hashes[0],&f.hashes[1],&f.choices,&f.salts[0],&f.salts[1]]).to_bytes();
                if w<2 {f.wins[w as usize]+=1;}
                f.round=f.round.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
                if w<2 && f.wins[w as usize]>=target(f.best_of).ok_or(ProgramError::InvalidAccountData)? {decide(&mut f,w,0,now);}
                else if f.credit<reserve()?+5*MOVE_FEE {decide(&mut f,NO_WINNER,4,now);}
                else {f.state=1;f.deadline=now+PHASE_SECONDS;f.committed=0;f.revealed=0;f.hashes=[[0;32];2];f.choices=[255;2];f.salts=[[0;32];2];}
                sol_log_data(&[b"RpsRoundV1",account.key.as_ref(),&round.to_le_bytes(),&f.last_moves,&[w],&f.wins,&f.transcript]);
            }
        }
        reward(&mut f,account,keeper,MOVE_FEE)?;return write(account,&f);
    }
    if tag==5 {
        check(data.len()==1,9)?;let caller=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;signer(caller)?;
        let mut f=game(program,account)?;let now=Clock::get()?.unix_timestamp;
        if f.state==0 {check(now>=f.expires || *caller.key==f.challenger || *caller.key==f.opponent,108)?;decide(&mut f,NO_WINNER,3,now);}
        else {
            check([1,2].contains(&f.state) && now>=f.deadline,108)?;
            let w=forfeit(if f.state==1 {f.committed} else {f.revealed});let reason=if f.state==1 {1} else {2};decide(&mut f,w,reason,now);
        }
        return write(account,&f);
    }
    if tag==4 {
        check(data.len()==1,9)?;
        let keeper=next_account_info(&mut it)?;let cfg=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;let vault=next_account_info(&mut it)?;
        let mint=next_account_info(&mut it)?;let challenger=next_account_info(&mut it)?;let opponent=next_account_info(&mut it)?;
        let ct=next_account_info(&mut it)?;let ot=next_account_info(&mut it)?;let bank=next_account_info(&mut it)?;
        let token=next_account_info(&mut it)?;let system=next_account_info(&mut it)?;let associated=next_account_info(&mut it)?;
        signer(keeper)?;check(keeper.owner==&system_program::id() && keeper.data_is_empty(),1)?;
        let c=config(program,cfg)?;let mut f=game(program,account)?;check(f.state==3,105)?;
        check(*mint.key==c.mint && *bank.key==c.bank_vault && *challenger.key==f.challenger && *opponent.key==f.opponent,4)?;
        valid_mint(mint)?;token_account(bank,mint.key,&c.bank_owner)?;escrow(program,vault,account,mint.key)?;
        let nonce=f.nonce.to_le_bytes();let bump=[f.bump];let creator=f.challenger;
        let seeds:&[&[u8]]=&[b"rps-v1",creator.as_ref(),&nonce,&bump];
        let accepted=f.accepted_at!=0;let mut payout=0;let mut fee=0;
        if f.winner<2 {
            check(accepted,105)?;let dest=if f.winner==0 {ct} else {ot};let owner=if f.winner==0 {challenger} else {opponent};
            ensure_destination(&mut f,account,dest,owner,mint,keeper,system,token,associated)?;
            let pot=f.wager.checked_mul(2).ok_or(ProgramError::ArithmeticOverflow)?;fee=pot/100;payout=pot-fee;
            transfer(vault,dest,mint,account,token,payout,Some(seeds))?;
            if fee>0 {transfer(vault,bank,mint,account,token,fee,Some(seeds))?;}f.state=4;
        } else {
            ensure_destination(&mut f,account,ct,challenger,mint,keeper,system,token,associated)?;
            transfer(vault,ct,mint,account,token,f.wager,Some(seeds))?;
            if accepted {ensure_destination(&mut f,account,ot,opponent,mint,keeper,system,token,associated)?;transfer(vault,ot,mint,account,token,f.wager,Some(seeds))?;}f.state=5;
        }
        let dust=token_account(vault,mint.key,account.key)?;
        if dust>0 {ensure_destination(&mut f,account,ct,challenger,mint,keeper,system,token,associated)?;transfer(vault,ct,mint,account,token,dust,Some(seeds))?;}
        invoke_signed(&spl_token_2022::instruction::close_account(token.key,vault.key,challenger.key,account.key,&[])?,&[vault.clone(),challenger.clone(),account.clone(),token.clone()],&[seeds])?;
        reward(&mut f,account,keeper,SETTLE_FEE)?;
        let unused=f.credit.checked_sub(CLOSE_FEE).ok_or(ProgramError::InsufficientFunds)?;
        let refund1=if accepted {unused/2} else {0};let refund0=unused-refund1;
        if refund0>0 {lamports(account,challenger,refund0)?;}if refund1>0 {lamports(account,opponent,refund1)?;}f.credit=CLOSE_FEE;
        write(account,&f)?;sol_log_data(&[b"RpsPaidV1",account.key.as_ref(),&[f.winner,f.state,f.reason],&payout.to_le_bytes(),&fee.to_le_bytes(),&refund0.to_le_bytes(),&refund1.to_le_bytes()]);return Ok(());
    }
    if tag==6 {
        check(data.len()==1,9)?;let keeper=next_account_info(&mut it)?;let account=next_account_info(&mut it)?;let challenger=next_account_info(&mut it)?;signer(keeper)?;
        let f=game(program,account)?;check([4,5].contains(&f.state) && Clock::get()?.unix_timestamp>=f.finished_at+RETENTION && *challenger.key==f.challenger && f.credit==CLOSE_FEE,109)?;
        lamports(account,keeper,CLOSE_FEE)?;lamports(account,challenger,account.lamports())?;
        account.realloc(0,false)?;account.assign(&system_program::id());return Ok(());
    }
    Err(ProgramError::InvalidInstructionData)
}

#[cfg(test)]mod layout_tests {
    use super::*;
    #[test]fn match_serialized_size(){let f=Match::try_from_slice(&vec![0;MATCH_LEN]).unwrap();assert_eq!(borsh::to_vec(&f).unwrap().len(),MATCH_LEN);}
}
