//! Isolated tournament vaults and native-result accounting. No server-authored scores.
use super::*;
use solana_program::instruction::{AccountMeta, Instruction};
pub mod model;
pub mod games;
use model::*;

const ATA: Pubkey = solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
pub fn series_address(program: &Pubkey, bank: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"world-series",bank.as_ref()],program)
}
/// Dice retains its original address and layout. Other games have isolated series.
pub fn game_series_address(program: &Pubkey, bank: &Pubkey, game: u8) -> (Pubkey,u8) {
    if game == 0 { series_address(program, bank) }
    else { Pubkey::find_program_address(&[b"world-series", bank.as_ref(), &[game]], program) }
}
fn series_game(version: u8) -> Result<u8, ProgramError> {
    match version { 1..=3 => Ok(0), 4 => Ok(1), 5 => Ok(2), _ => Err(ProgramError::Custom(100)) }
}
pub fn event_address(program: &Pubkey, series: &Pubkey, nonce: u64) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"ws-event",series.as_ref(),&nonce.to_le_bytes()],program)
}
pub fn entry_address(program: &Pubkey, event: &Pubkey, wallet: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"ws-entry",event.as_ref(),wallet.as_ref()],program)
}
pub fn receipt_address(program: &Pubkey, bet: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"ws-bet",bet.as_ref()],program)
}
pub fn reserve_address(program: &Pubkey, series: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"ws-sol",series.as_ref()],program)
}
pub fn vault_address(program: &Pubkey, event: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"ws-vault",event.as_ref()],program)
}
fn load<T:BorshDeserialize>(program:&Pubkey, account:&AccountInfo) -> Result<T,ProgramError> {
    check(account.owner==program,100)?;
    T::try_from_slice(&account.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)
}
fn series(program:&Pubkey, account:&AccountInfo, bank:&Pubkey) -> Result<Series,ProgramError> {
    let s:Series=load(program,account)?;
    let (key,bump)=game_series_address(program,bank,series_game(s.version)?);
    check(*account.key==key&&s.bank==*bank&&s.bump==bump,100)?;
    Ok(s)
}
fn event(program:&Pubkey, account:&AccountInfo, series:&Pubkey) -> Result<Event,ProgramError> {
    let e:Event=load(program,account)?;
    let (key,bump)=event_address(program,series,e.nonce);
    check(*account.key==key&&(1..=5).contains(&e.version)&&e.series==*series&&e.bump==bump&&e.winners<=10,100)?;
    Ok(e)
}
fn entry(program:&Pubkey, account:&AccountInfo, event:&Pubkey, wallet:&Pubkey) -> Result<Entry,ProgramError> {
    check(account.owner==program,100)?;
    let mut bytes=account.try_borrow_data()?.to_vec();
    if bytes.len()==ENTRY_LEN&&bytes[0]==1 { bytes.extend_from_slice(&[0;5]); }
    let e=Entry::try_from_slice(&bytes).map_err(|_|ProgramError::InvalidAccountData)?;
    let (key,bump)=entry_address(program,event,wallet);
    check(*account.key==key&&[1,2].contains(&e.version)&&e.event==*event&&e.wallet==*wallet&&e.bump==bump,100)?;
    check(account.data_len()==if e.version==2{PAID_ENTRY_LEN}else{ENTRY_LEN},100)?;
    Ok(e)
}
fn write_entry(account:&AccountInfo,e:&Entry)->ProgramResult {
    let bytes=borsh::to_vec(e).map_err(|_|ProgramError::InvalidAccountData)?;
    let len=if e.version==2{PAID_ENTRY_LEN}else{ENTRY_LEN};
    check(account.data_len()==len,100)?;
    account.try_borrow_mut_data()?.copy_from_slice(&bytes[..len]);Ok(())
}
/// Only this wallet's entry fronts its temporary receipts; no shared operator float.
fn entry_cost()->Result<u64,ProgramError>{
    let rent=Rent::get()?;
    plus(plus(rent.minimum_balance(PAID_ENTRY_LEN),rent.minimum_balance(RECEIPT_LEN).checked_mul(u64::from(ENTRY_RECEIPTS)).ok_or(ProgramError::ArithmeticOverflow)?)?,ENTRY_REFUND_FEE)
}
fn enter(program:&Pubkey,a:&[AccountInfo],data:&[u8])->ProgramResult {
    check(a.len()==6&&data.len()==9,9)?;signer(&a[3])?;
    let s=series(program,&a[1],a[0].key)?;load_bank(program,&a[0])?;
    let mut e=event(program,&a[2],a[1].key)?;
    // Entry is available before a scheduled start; bets still use accepts().
    check(e.version>=2&&s.current==e.nonce&&e.status==OPEN&&Clock::get()?.unix_timestamp<e.end,102)?;
    let (key,bump)=entry_address(program,a[2].key,a[3].key);check(*a[4].key==key,100)?;
    if a[4].owner==program {entry(program,&a[4],a[2].key,a[3].key)?;return Ok(());}
    check(*a[5].key==system_program::id()&&a[3].owner==&system_program::id(),100)?;
    let cost=entry_cost()?;check(cost<=u64_at(data,1)?,104)?;
    // Include any unsolicited prefunding in the reserve, but never charge it twice.
    let needed=cost.saturating_sub(a[4].lamports());
    if needed>0 {invoke(&system_instruction::transfer(a[3].key,a[4].key,needed),&[a[3].clone(),a[4].clone(),a[5].clone()])?;}
    create_pda(&a[3],&a[4],&a[5],program,PAID_ENTRY_LEN,&[b"ws-entry",a[2].key.as_ref(),a[3].key.as_ref(),&[bump]])?;
    write_entry(&a[4],&Entry{version:2,event:*a[2].key,wallet:*a[3].key,score:0,bets:0,wins:0,pending:0,bump,open_receipts:0,refunded:false})?;
    e.participants=add32(e.participants,1)?;write(&a[2],&e)?;
    msg!("SDICE_WS_ENTER event={} wallet={} sol={}",a[2].key,a[3].key,needed);Ok(())
}
fn enable_entries(program:&Pubkey,a:&[AccountInfo],version:u8)->ProgramResult {
    check(a.len()==3,9)?;signer(&a[0])?;let b=load_bank(program,&a[1])?;
    check(b.authority==*a[0].key,101)?;let mut s=series(program,&a[2],a[1].key)?;
    check(s.current==0,102)?;s.version=s.version.max(version);write(&a[2],&s)
}
fn refund_entry(program:&Pubkey,a:&[AccountInfo])->ProgramResult {
    check(a.len()==6,9)?;signer(&a[5])?;load_bank(program,&a[0])?;
    series(program,&a[1],a[0].key)?;let e=event(program,&a[2],a[1].key)?;
    let mut en=entry(program,&a[4],a[2].key,a[3].key)?;
    check(en.version==2&&Clock::get()?.unix_timestamp>=e.end&&en.pending==0&&en.open_receipts==0,102)?;
    if en.refunded{return Ok(());}
    let excess=a[4].lamports().checked_sub(Rent::get()?.minimum_balance(PAID_ENTRY_LEN)).ok_or(ProgramError::InsufficientFunds)?;
    let returned=excess.checked_sub(ENTRY_REFUND_FEE).ok_or(ProgramError::InsufficientFunds)?;
    **a[4].try_borrow_mut_lamports()?-=excess;
    **a[3].try_borrow_mut_lamports()?=plus(a[3].lamports(),returned)?;
    **a[5].try_borrow_mut_lamports()?=plus(a[5].lamports(),ENTRY_REFUND_FEE)?;
    en.refunded=true;write_entry(&a[4],&en)?;
    msg!("SDICE_WS_ENTRY_REFUND event={} wallet={} sol={}",a[2].key,a[3].key,returned);Ok(())
}
fn reserve(program:&Pubkey, account:&AccountInfo, series:&Pubkey)->ProgramResult {
    check(*account.key==reserve_address(program,series).0&&account.owner==&system_program::id()&&account.data_is_empty(),100)
}
fn plus(a:u64,b:u64)->Result<u64,ProgramError>{a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow)}
fn add32(a:u32,b:u32)->Result<u32,ProgramError>{a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow)}

/// Sponsored rent is independent of the player's fixed execution deposit.
fn sponsored_create<'a>(program:&Pubkey, series:&Pubkey, payer:&AccountInfo<'a>, account:&AccountInfo<'a>,
    system:&AccountInfo<'a>, owner:&Pubkey, len:usize, account_seeds:&[&[u8]])->ProgramResult {
    reserve(program,payer,series)?;
    check(*system.key==system_program::id()&&account.owner==&system_program::id()&&account.data_is_empty(),100)?;
    let bump=[reserve_address(program,series).1];
    let payer_seeds:&[&[u8]]=&[b"ws-sol",series.as_ref(),&bump];
    let rent=Rent::get()?.minimum_balance(len);
    let needed=rent.saturating_sub(account.lamports());
    if needed>0 {invoke_signed(&system_instruction::transfer(payer.key,account.key,needed),
        &[payer.clone(),account.clone(),system.clone()],&[payer_seeds])?;}
    invoke_signed(&system_instruction::allocate(account.key,len as u64),&[account.clone(),system.clone()],&[account_seeds])?;
    invoke_signed(&system_instruction::assign(account.key,owner),&[account.clone(),system.clone()],&[account_seeds])
}

// 60: authority, bank, table, series, intake, mint, System.
// 61 fund: sender, sender ATA, then the nine sync accounts below; data u64 amount.
// 62 sync: bank, series, event, intake, event vault, mint, Token2022, reserve, System.
// 63 score: bank, round, bet, receipt, event, entry.
// 64 finalize: same nine sync accounts.
// 65 pay: same nine sync accounts, wallet, destination ATA, ATA program; data u8 rank.
pub fn process(program:&Pubkey, accounts:&[AccountInfo], data:&[u8])->ProgramResult {
    match data.first().copied().ok_or(ProgramError::InvalidInstructionData)? {
        60=>initialize(program,accounts,data),
        61|69|115=>{
            let custom=data[0]==115;
            let scheduled=data[0]==69;
            check(data.len()==(if custom{25}else if scheduled{17}else{9})&&accounts.len()==11,9)?;
            let sender=&accounts[0]; signer(sender)?;
            let a=&accounts[2..];
            let s=series(program,&a[1],a[0].key)?;
            check(s.intake==*a[3].key&&s.mint==*a[5].key,100)?;
            let amount=u64_at(data,1)?;check(amount>0,101)?;
            let duration=if custom {
                let duration=i64::try_from(u64_at(data,17)?).map_err(|_|ProgramError::InvalidInstructionData)?;
                check((MIN_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&duration),108)?;
                // Explicit creation must never turn into a donation to another sponsor's event.
                check(s.version>=3&&s.current==0,106)?;
                check(plus(token_account(&a[3],&s.mint,a[1].key)?,amount)? >= MINIMUM_FUNDING,107)?;
                Some(duration)
            }else{None};
            let start=if scheduled || (custom && u64_at(data,9)? != 0){
                let start=i64::try_from(u64_at(data,9)?).map_err(|_|ProgramError::InvalidInstructionData)?;
                let now=Clock::get()?.unix_timestamp;
                check(s.version>=3&&s.current==0&&start>now&&start.saturating_sub(now)<=MAX_SCHEDULE_AHEAD_SECONDS,106)?;
                check(plus(token_account(&a[3],&s.mint,a[1].key)?,amount)? >= MINIMUM_FUNDING,107)?;
                Some(start)
            }else{None};
            token_account(&accounts[1],&s.mint,sender.key)?;
            let decimals=valid_mint(&a[5])?;
            transfer(&accounts[1],&a[3],&a[5],sender,&a[6],amount,decimals,None)?;
            synchronize_at(program,a,start,duration)
        },
        62=>{check(data.len()==1,9)?;synchronize(program,accounts)},
        63=>{check(data.len()==1&&accounts.len()==6,9)?;score(program,&accounts[0],&accounts[1],&accounts[2],&accounts[3],&accounts[4],&accounts[5])},
        64=>{check(data.len()==1,9)?;finalize(program,accounts)},
        65=>{check(data.len()==2,9)?;pay(program,accounts,usize::from(data[1]))},
        66=>enter(program,accounts,data),
        67=>{check(data.len()==1,9)?;enable_entries(program,accounts,2)},
        68=>{check(data.len()==1,9)?;refund_entry(program,accounts)},
        70=>{check(data.len()==1,9)?;enable_entries(program,accounts,3)},
        _=>Err(ProgramError::InvalidInstructionData),
    }
}

fn initialize(program:&Pubkey,a:&[AccountInfo],data:&[u8])->ProgramResult {
    check((data.len()==1||data==[60,2]||data==[60,3])&&a.len()==7,9)?;signer(&a[0])?;
    let bank=load_bank(program,&a[1])?;
    let mut table=shared::load_table(program,&a[2],a[1].key)?;
    check(bank.authority==*a[0].key&&bank.version==4&&table.version==3&&table.current==0&&table.queued==0,101)?;
    check(bank.mint==*a[5].key,100)?;valid_mint(&a[5])?;
    let (key,bump)=series_address(program,a[1].key);check(*a[3].key==key,100)?;
    // The published funding address is the canonical Token-2022 ATA of this series.
    let intake=Pubkey::find_program_address(&[key.as_ref(),spl_token_2022::id().as_ref(),bank.mint.as_ref()],&ATA).0;
    check(*a[4].key==intake,100)?;token_account(&a[4],&bank.mint,&key)?;
    create_pda(&a[0],&a[3],&a[6],program,SERIES_LEN,&[b"world-series",a[1].key.as_ref(),&[bump]])?;
    write(&a[3],&Series{version:if data.len()==2{data[1]}else{1},bank:*a[1].key,mint:bank.mint,intake,current:0,next:1,carry:0,bump})?;
    table.version=4;write(&a[2],&table)
}

fn vault_balance(program:&Pubkey,e:&AccountInfo,v:&AccountInfo,mint:&Pubkey)->Result<u64,ProgramError>{
    check(*v.key==vault_address(program,e.key).0,100)?;token_account(v,mint,e.key)
}
fn validate_sync<'a>(program:&Pubkey,a:&[AccountInfo<'a>])->Result<Series,ProgramError>{
    check(a.len()==9,9)?;
    let bank=load_bank(program,&a[0])?;let s=series(program,&a[1],a[0].key)?;
    check(s.mint==bank.mint&&s.mint==*a[5].key&&s.intake==*a[3].key&&*a[6].key==spl_token_2022::id(),100)?;
    reserve(program,&a[7],a[1].key)?;check(*a[8].key==system_program::id(),100)?;
    valid_mint(&a[5])?;token_account(&a[3],&s.mint,a[1].key)?;
    Ok(s)
}
fn synchronize(program:&Pubkey,a:&[AccountInfo])->ProgramResult{
    synchronize_at(program,a,None,None)
}
fn synchronize_at(program:&Pubkey,a:&[AccountInfo],scheduled:Option<i64>,duration:Option<i64>)->ProgramResult{
    let mut s=validate_sync(program,a)?;
    let now=Clock::get()?.unix_timestamp;
    let intake=token_account(&a[3],&s.mint,a[1].key)?;
    let mut e=if s.current==0 {
        // A no-winner carry does not endlessly start another unfunded event.
        if intake<=s.carry||(s.version>=3&&intake<MINIMUM_FUNDING){return Ok(());}
        let (key,bump)=event_address(program,a[1].key,s.next);check(*a[2].key==key,100)?;
        sponsored_create(program,a[1].key,&a[7],&a[2],&a[8],program,EVENT_LEN,&[b"ws-event",a[1].key.as_ref(),&s.next.to_le_bytes(),&[bump]])?;
        let (vault,vb)=vault_address(program,&key);check(*a[4].key==vault,100)?;
        sponsored_create(program,a[1].key,&a[7],&a[4],&a[8],&spl_token_2022::id(),165,&[b"ws-vault",key.as_ref(),&[vb]])?;
        invoke(&spl_token_2022::instruction::initialize_account3(a[6].key,a[4].key,a[5].key,&key)?,&[a[4].clone(),a[5].clone(),a[6].clone()])?;
        let start=scheduled.unwrap_or(now);
        let mut e=match duration {
            Some(seconds)=>Event::with_duration(*a[1].key,s.next,start,seconds,bump)?,
            None=>Event::new(*a[1].key,s.next,start,bump)?,
        };e.version=s.version;
        s.current=s.next;s.next=plus(s.next,1)?;s.carry=0;
        msg!("SDICE_WS_START event={} start={} end={}",key,e.start,e.end);
        e
    }else{
        let e=event(program,&a[2],a[1].key)?;check(e.nonce==s.current,100)?;e
    };
    // Scheduled pools can receive top-ups before scoring begins.
    if e.status!=OPEN||now>=e.end{return Ok(());}
    vault_balance(program,&a[2],&a[4],&s.mint)?;
    if intake>0{
        let game = [series_game(s.version)?]; let bump = [s.bump];
        let legacy: &[&[u8]] = &[b"world-series", s.bank.as_ref(), &bump];
        let separate: &[&[u8]] = &[b"world-series", s.bank.as_ref(), &game, &bump];
        transfer(&a[3],&a[4],&a[5],&a[1],&a[6],intake,valid_mint(&a[5])?,Some(if game[0]==0 {legacy} else {separate}))?;
    }
    e.prize=vault_balance(program,&a[2],&a[4],&s.mint)?;
    write(&a[2],&e)?;write(&a[1],&s)
}

/// Called only after successful native acceptance in the same transaction.
/// tail = series, current event (or System), entry (or System), receipt, reserve.
pub fn enroll<'a>(program:&Pubkey,bank:&AccountInfo,round:&AccountInfo,bet:&AccountInfo,system:&AccountInfo<'a>,tail:&[AccountInfo<'a>])->ProgramResult{
    check(tail.len()==5,9)?;
    let s=series(program,&tail[0],bank.key)?;
    check(series_game(s.version)? == 0, 100)?;
    check(*tail[3].key==receipt_address(program,bet.key).0,100)?;
    if s.current==0{return Ok(());}
    let mut e=event(program,&tail[1],tail[0].key)?;check(e.nonce==s.current,100)?;
    let now=Clock::get()?.unix_timestamp;if !e.accepts(now){return Ok(());}
    let b=shared::load_bet(program,bet,round.key)?;
    let r=shared::load_shared(program,round,bank.key)?;
    check(b.version==4&&!b.settled&&r.bank==*bank.key,101)?;
    let (entry_key,eb)=entry_address(program,tail[1].key,&b.player);check(entry_key==*tail[2].key,100)?;
    if e.version>=2&&tail[2].owner!=program{return Ok(());}
    let mut en=if tail[2].owner==program{entry(program,&tail[2],tail[1].key,&b.player)?}else{
        sponsored_create(program,tail[0].key,&tail[4],&tail[2],system,program,ENTRY_LEN,&[b"ws-entry",tail[1].key.as_ref(),b.player.as_ref(),&[eb]])?;
        e.participants=add32(e.participants,1)?;
        Entry{version:1,event:*tail[1].key,wallet:b.player,score:0,bets:0,wins:0,pending:0,bump:eb,open_receipts:0,refunded:false}
    };
    let rb=receipt_address(program,bet.key).1;
    if e.version>=2 {
        check(en.version==2&&!en.refunded&&en.open_receipts<ENTRY_RECEIPTS,105)?;
        check(tail[3].owner==&system_program::id()&&tail[3].data_is_empty()&&*system.key==system_program::id(),100)?;
        let rent=Rent::get()?;let needed=rent.minimum_balance(RECEIPT_LEN).saturating_sub(tail[3].lamports());
        let keep=plus(rent.minimum_balance(PAID_ENTRY_LEN),ENTRY_REFUND_FEE)?;
        check(tail[2].lamports()>=plus(keep,needed)?,105)?;
        invoke_signed(&system_instruction::allocate(tail[3].key,RECEIPT_LEN as u64),&[tail[3].clone(),system.clone()],&[&[b"ws-bet",bet.key.as_ref(),&[rb]]])?;
        invoke_signed(&system_instruction::assign(tail[3].key,program),&[tail[3].clone(),system.clone()],&[&[b"ws-bet",bet.key.as_ref(),&[rb]]])?;
        // Finish the direct lamport move after the CPIs, keeping each CPI's
        // account-balance synchronization balanced independently.
        **tail[2].try_borrow_mut_lamports()?-=needed;
        **tail[3].try_borrow_mut_lamports()?=plus(tail[3].lamports(),needed)?;
        en.open_receipts=add32(en.open_receipts,1)?;
    }else{
        sponsored_create(program,tail[0].key,&tail[4],&tail[3],system,program,RECEIPT_LEN,&[b"ws-bet",bet.key.as_ref(),&[rb]])?;
    }
    en.bets=add32(en.bets,1)?;en.pending=add32(en.pending,1)?;e.bets=plus(e.bets,1)?;e.pending=add32(e.pending,1)?;
    write(&tail[3],&Receipt{version:1,series:*tail[0].key,event:*tail[1].key,bet:*bet.key,wallet:b.player,wager:b.wager,accepted_at:now,scored:false,bump:rb})?;
    write_entry(&tail[2],&en)?;write(&tail[1],&e)
}

pub fn score(program:&Pubkey,bank:&AccountInfo,round:&AccountInfo,bet:&AccountInfo,receipt:&AccountInfo,event_account:&AccountInfo,entry_account:&AccountInfo)->ProgramResult{
    check(*receipt.key==receipt_address(program,bet.key).0,100)?;
    // Negative proof for a bet accepted while no tournament was open.
    if receipt.owner==&system_program::id()&&receipt.data_is_empty(){return Ok(());}
    let mut receipt_data:Receipt=load(program,receipt)?;
    check(receipt_data.version==1&&receipt_data.bet==*bet.key&&receipt_data.bump==receipt_address(program,bet.key).1
        &&receipt_data.series==series_address(program,bank.key).0&&receipt_data.event==*event_account.key,100)?;
    if receipt_data.scored{return Ok(());}
    let r=shared::load_shared(program,round,bank.key)?;
    let b=shared::load_bet(program,bet,round.key)?;
    check(b.version==4&&b.settled&&b.player==receipt_data.wallet&&b.wager==receipt_data.wager,101)?;
    let mut e=event(program,event_account,&receipt_data.series)?;
    check(e.status==OPEN&&receipt_data.accepted_at>=e.start&&receipt_data.accepted_at<e.end,102)?;
    let mut en=entry(program,entry_account,event_account.key,&b.player)?;
    let profit=winning_profit(b.wager,b.payout,b.outcome,r.refunded)?;
    en.score=en.score.checked_add(u128::from(profit)).ok_or(ProgramError::ArithmeticOverflow)?;
    en.wins=add32(en.wins,u32::from(profit>0))?;
    en.pending=en.pending.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?;
    e.pending=e.pending.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?;
    e.rank(b.player,en.score);receipt_data.scored=true;
    write(receipt,&receipt_data)?;write_entry(entry_account,&en)?;write(event_account,&e)?;
    msg!("SDICE_WS_SCORE event={} wallet={} profit={} total={}",event_account.key,b.player,profit,en.score);
    Ok(())
}

/// A v4 table cannot close native bet evidence until its tournament score is safe.
pub fn before_cleanup(program:&Pubkey,bank:&AccountInfo,round:&AccountInfo,bet:&AccountInfo,tail:&[AccountInfo],closing:bool)->ProgramResult{
    check(tail.len()==4,9)?;
    score(program,bank,round,bet,&tail[0],&tail[1],&tail[2])?;
    let series=series_address(program,bank.key).0;reserve(program,&tail[3],&series)?;
    if closing&&tail[0].owner==program{
        let receipt:Receipt=load(program,&tail[0])?;
        let mut en=entry(program,&tail[2],tail[1].key,&receipt.wallet)?;
        let destination=if en.version==2{
            en.open_receipts=en.open_receipts.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?;
            write_entry(&tail[2],&en)?;&tail[2]
        }else{&tail[3]};
        **destination.try_borrow_mut_lamports()?=plus(destination.lamports(),tail[0].lamports())?;
        **tail[0].try_borrow_mut_lamports()?=0;tail[0].try_borrow_mut_data()?.fill(0);
    }
    Ok(())
}

fn finalize(program:&Pubkey,a:&[AccountInfo])->ProgramResult{
    let mut s=validate_sync(program,a)?;
    let mut e=event(program,&a[2],a[1].key)?;
    if [PAYING,COMPLETE,CARRIED].contains(&e.status){return Ok(());}
    check(s.current==e.nonce,100)?;
    let balance=vault_balance(program,&a[2],&a[4],&s.mint)?;
    e.freeze(Clock::get()?.unix_timestamp,balance)?;
    if e.status==CARRIED{
        event_transfer(&e,&a[2],&a[4],&a[3],&a[5],&a[6],balance)?;
        s.carry=plus(s.carry,balance)?;s.current=0;write(&a[1],&s)?;
    }
    msg!("SDICE_WS_FINAL event={} prize={} winners={}",a[2].key,balance,e.winners);
    write(&a[2],&e)
}
fn event_transfer<'a>(e:&Event,event:&AccountInfo<'a>,from:&AccountInfo<'a>,to:&AccountInfo<'a>,mint:&AccountInfo<'a>,token:&AccountInfo<'a>,amount:u64)->ProgramResult{
    transfer(from,to,mint,event,token,amount,valid_mint(mint)?,Some(&[b"ws-event",e.series.as_ref(),&e.nonce.to_le_bytes(),&[e.bump]]))
}
fn pay(program:&Pubkey,a:&[AccountInfo],rank:usize)->ProgramResult{
    check(a.len()==12&&rank<MAX_WINNERS,9)?;
    let mut s=validate_sync(program,&a[..9])?;let mut e=event(program,&a[2],a[1].key)?;
    check(rank<usize::from(e.winners)&&e.top[rank].wallet==*a[9].key,103)?;
    let expected=Pubkey::find_program_address(&[a[9].key.as_ref(),spl_token_2022::id().as_ref(),s.mint.as_ref()],&ATA).0;
    check(*a[10].key==expected&&*a[11].key==ATA&&a[11].executable,103)?;
    if e.paid_mask&(1u16<<rank)!=0{return Ok(());}
    check(e.status==PAYING&&s.current==e.nonce,102)?;
    vault_balance(program,&a[2],&a[4],&s.mint)?;
    let amount=e.awards[rank];
    if amount>0{
        if a[10].owner!=&spl_token_2022::id(){
            let ix=Instruction{program_id:ATA,data:vec![1],accounts:vec![
                AccountMeta::new(*a[7].key,true),AccountMeta::new(*a[10].key,false),AccountMeta::new_readonly(*a[9].key,false),
                AccountMeta::new_readonly(*a[5].key,false),AccountMeta::new_readonly(*a[8].key,false),AccountMeta::new_readonly(*a[6].key,false)]};
            invoke_signed(&ix,&[a[7].clone(),a[10].clone(),a[9].clone(),a[5].clone(),a[8].clone(),a[6].clone(),a[11].clone()],
                &[&[b"ws-sol",a[1].key.as_ref(),&[reserve_address(program,a[1].key).1]]])?;
        }
        token_account(&a[10],&s.mint,a[9].key)?;
        event_transfer(&e,&a[2],&a[4],&a[10],&a[5],&a[6],amount)?;
    }
    e.paid=plus(e.paid,amount)?;e.paid_mask|=1u16<<rank;
    if e.paid_mask==(1u16<<e.winners)-1{
        check(e.paid==e.prize,103)?;
        // Direct donations to the event vault after freeze belong to the next pool.
        let extra=vault_balance(program,&a[2],&a[4],&s.mint)?;
        if extra>0{event_transfer(&e,&a[2],&a[4],&a[3],&a[5],&a[6],extra)?;}
        e.status=COMPLETE;s.current=0;write(&a[1],&s)?;
    }
    msg!("SDICE_WS_PAID event={} wallet={} rank={} amount={}",a[2].key,a[9].key,rank+1,amount);
    write(&a[2],&e)
}
