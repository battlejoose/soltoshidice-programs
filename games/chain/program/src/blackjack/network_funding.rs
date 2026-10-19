//! Player-funded network fees, including failed transactions. Each round uses
//! an initialized System nonce account as its fee payer. Its signing key cannot
//! withdraw SOL: only this program's canonical purse PDA is nonce authority.
//! The runtime preserves nonce rent when deducting fees. Unused execution SOL
//! and recorded house rent return to their original owners after cleanup.
use super::{state::*, sol_funding, rules};
use crate::*;

const CONFIG_SEED: &[u8] = b"bj-network-config-v1";
const PURSE_SEED: &[u8] = b"bj-network-round-v1";
const CONFIG_MAGIC: [u8;8] = *b"BJNETC01";
const PURSE_MAGIC: [u8;8] = *b"BJNETP01";
pub const CONFIG_LEN: usize = 117;
pub const PURSE_LEN: usize = 537;
const NONCE_LEN: usize = 80;
pub const BET_BUDGET: u64 = 150_000;
pub const DRAW_BUDGET: u64 = 20_000;

#[derive(BorshSerialize, BorshDeserialize)]
pub struct Config { magic: [u8;8], table: Pubkey, bank: Pubkey, issuer: Pubkey, rent: u64, open_purses: u32, bump: u8 }
#[derive(BorshSerialize, BorshDeserialize)]
pub struct Purse {
    magic: [u8;8], table: Pubkey, bank: Pubkey, nonce: Pubkey,
    round: u64, house_rent: u64, nonce_rent: u64, nonce_loan: u64,
    wallets: [Pubkey;10], credits: [u64;10], bump: u8,
}
fn add(a:u64,b:u64)->Result<u64,ProgramError>{a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow)}
pub fn config_address(p:&Pubkey,t:&Pubkey)->(Pubkey,u8){Pubkey::find_program_address(&[CONFIG_SEED,t.as_ref()],p)}
pub fn purse_address(p:&Pubkey,t:&Pubkey,r:u64)->(Pubkey,u8){Pubkey::find_program_address(&[PURSE_SEED,t.as_ref(),&r.to_le_bytes()],p)}
fn config(p:&Pubkey,a:&AccountInfo,t:&Pubkey,bank:&Pubkey)->Result<Config,ProgramError>{
    check(a.owner==p && a.data_len()==CONFIG_LEN,880)?;
    let s=Config::try_from_slice(&a.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)?;
    let (key,bump)=config_address(p,t);
    check(*a.key==key && s.magic==CONFIG_MAGIC && s.table==*t && s.bank==*bank && s.bump==bump && a.lamports()>=s.rent,880)?;
    Ok(s)
}
fn purse(p:&Pubkey,a:&AccountInfo,t:&Pubkey,r:u64)->Result<Purse,ProgramError>{
    check(a.owner==p && a.data_len()==PURSE_LEN,880)?;
    let s=Purse::try_from_slice(&a.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)?;
    let (key,bump)=purse_address(p,t,r);
    check(*a.key==key && s.magic==PURSE_MAGIC && s.table==*t && s.round==r && s.bump==bump && a.lamports()>=s.house_rent,880)?;
    for i in 0..10 {check((s.wallets[i]==Pubkey::default())==(s.credits[i]==0)
        && (s.credits[i]==0 || !s.wallets[..i].contains(&s.wallets[i])),880)?;}
    Ok(s)
}
fn nonce(a:&AccountInfo,authority:&Pubkey,rent:u64)->ProgramResult{
    check(a.owner==&system_program::id() && a.data_len()==NONCE_LEN && a.lamports()>=rent.max(Rent::get()?.minimum_balance(NONCE_LEN)),880)?;
    let b=a.try_borrow_data()?;
    // bincode Versions::Current(State::Initialized(Data{authority,...})).
    check(b[..8]==[1,0,0,0,1,0,0,0] && b[8..40]==authority.to_bytes(),880)
}

/// Extra action accounts: purse, nonce. Creating the first round purse also
/// needs config, issuer signer, house reserve, system, recent-blockhashes, rent.
/// The issuer endorses only the new nonce key. Neither key may withdraw it.
pub fn fund<'a>(p:&Pubkey, table:&AccountInfo<'a>, t:&Table, caller:&AccountInfo<'a>, deposit:&AccountInfo<'a>,
    extra:&[AccountInfo<'a>], seat:usize, amount:u64)->ProgramResult{
    check(t.version==4 && amount>0 && (extra.len()==3 || extra.len()==8),9)?;
    let mut s=if extra[0].owner==p {purse(p,&extra[0],table.key,t.round.id)?} else {
        check(extra.len()==8,880)?;
        let mut c=config(p,&extra[3],table.key,&t.bank)?;
        check(*extra[4].key==c.issuer && extra[4].is_signer && extra[1].is_signer
            && extra[1].key!=extra[4].key && extra[1].key!=caller.key
            && extra[1].owner==&system_program::id() && extra[1].data_is_empty(),880)?;
        sol_funding::reserve_valid(p,&extra[5],table.key)?;
        check(*extra[2].key==system_program::id() && *extra[6].key==solana_program::sysvar::recent_blockhashes::id()
            && *extra[7].key==solana_program::sysvar::rent::id(),880)?;
        let (key,bump)=purse_address(p,table.key,t.round.id);
        check(*extra[0].key==key,880)?;
        let loan=Rent::get()?.minimum_balance(PURSE_LEN).saturating_sub(extra[0].lamports());
        let nonce_loan=Rent::get()?.minimum_balance(NONCE_LEN).saturating_sub(extra[1].lamports());
        let before=caller.lamports();
        sol_funding::reserve_transfer(p,table.key,&extra[5],caller,&extra[2],add(loan,nonce_loan)?)?;
        create_pda(caller,&extra[0],&extra[2],p,PURSE_LEN,&[PURSE_SEED,table.key.as_ref(),&t.round.id.to_le_bytes(),&[bump]])?;
        if nonce_loan>0 {invoke(&system_instruction::transfer(caller.key,extra[1].key,nonce_loan),&[caller.clone(),extra[1].clone(),extra[2].clone()])?;}
        invoke(&system_instruction::allocate(extra[1].key,NONCE_LEN as u64),&[extra[1].clone(),extra[2].clone()])?;
        invoke(&system_instruction::create_nonce_account(caller.key,extra[1].key,extra[0].key,0)[1],
            &[extra[1].clone(),extra[6].clone(),extra[7].clone(),extra[2].clone()])?;
        check(caller.lamports()==before,880)?;
        c.open_purses=c.open_purses.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;write(&extra[3],&c)?;
        Purse{magic:PURSE_MAGIC,table:*table.key,bank:t.bank,nonce:*extra[1].key,round:t.round.id,
            house_rent:loan,nonce_rent:extra[1].lamports(),nonce_loan,wallets:[Pubkey::default();10],credits:[0;10],bump}
    };
    check(s.bank==t.bank && s.nonce==*extra[1].key && (s.wallets[seat]==Pubkey::default() || s.wallets[seat]==t.round_wallets[seat]),880)?;
    nonce(&extra[1],extra[0].key,s.nonce_loan)?;
    sol_funding::network_debit(p,table.key,t,deposit,&extra[1],&t.round_wallets[seat],amount)?;
    s.wallets[seat]=t.round_wallets[seat];s.credits[seat]=add(s.credits[seat],amount)?;
    write(&extra[0],&s)?;
    let mut sync=system_instruction::transfer(caller.key,extra[1].key,0);
    sync.accounts.push(solana_program::instruction::AccountMeta::new(*deposit.key,false));
    invoke(&sync,&[caller.clone(),extra[1].clone(),deposit.clone(),extra[2].clone()])?;
    // Synchronize the direct debit before later token/reward CPIs.
    msg!("SDICE_BLACKJACK_NETWORK_RESERVED round={} wallet={} lamports={} purse={}",s.round,s.wallets[seat],amount,extra[0].key);
    Ok(())
}

pub fn process(p:&Pubkey,a:&[AccountInfo],data:&[u8])->ProgramResult{
    check(a.len()>=7 && (data.len()==9 || (data.first()==Some(&105) && data.len()==17)),9)?;signer(&a[0])?;
    let bank=load_bank(p,&a[2])?;
    let mut t=super::state::load(p,&a[1],a[2].key)?;
    sol_funding::reserve_valid(p,&a[5],a[1].key)?;
    check(*a[6].key==system_program::id(),880)?;
    if data[0]==105 {
        check(a.len()==7 && t.version==4,9)?;
        let round=u64_at(data,1)?;let amount=u64_at(data,9)?;
        check(amount>0 && amount<=100_000_000,880)?;
        let mut s=purse(p,&a[3],a[1].key,round)?;
        check(s.bank==*a[2].key && s.nonce==*a[4].key,880)?;
        nonce(&a[4],a[3].key,s.nonce_loan)?;
        let seat=s.wallets.iter().position(|w|*w==*a[0].key).ok_or(ProgramError::Custom(880))?;
        // Recovery needs only the original wallet, even after its seat/session
        // or deposit is closed. This is an explicit additional player deposit.
        invoke(&system_instruction::transfer(a[0].key,a[4].key,amount),&[a[0].clone(),a[4].clone(),a[6].clone()])?;
        s.credits[seat]=add(s.credits[seat],amount)?;write(&a[3],&s)?;
        if t.round.id==round {t.changed()?;write(&a[1],&*t)?;}
        msg!("SDICE_BLACKJACK_NETWORK_RESERVED round={} wallet={} lamports={} purse={}",round,a[0].key,amount,a[3].key);
        return Ok(());
    }
    if data[0]==102 {
        check(a.len()==7 && t.version==3 && !t.enabled && bank.authority==*a[0].key
            && t.seats.iter().all(|s|s.wallet==Pubkey::default())
            && (!t.round.active() || (t.round.stage==rules::Stage::Betting && t.round.betting_players()==0))
            && *a[4].key!=bank.authority && *a[4].key!=Pubkey::default(),863)?;
        let (key,bump)=config_address(p,a[1].key);check(key==*a[3].key,880)?;
        let loan=Rent::get()?.minimum_balance(CONFIG_LEN).saturating_sub(a[3].lamports());
        let before=a[0].lamports();
        sol_funding::reserve_transfer(p,a[1].key,&a[5],&a[0],&a[6],loan)?;
        create_pda(&a[0],&a[3],&a[6],p,CONFIG_LEN,&[CONFIG_SEED,a[1].key.as_ref(),&[bump]])?;
        check(a[0].lamports()==before,880)?;
        write(&a[3],&Config{magic:CONFIG_MAGIC,table:*a[1].key,bank:*a[2].key,issuer:*a[4].key,rent:loan,open_purses:0,bump})?;
        t.version=4;t.changed()?;return write(&a[1],&*t);
    }
    if data[0]==103 {
        check(a.len()==7 && t.version==4 && !t.competitions() && !t.enabled && bank.authority==*a[0].key && t.seats.iter().all(|s|s.wallet==Pubkey::default()),863)?;
        let c=config(p,&a[3],a[1].key,a[2].key)?;check(c.open_purses==0,881)?;
        **a[5].try_borrow_mut_lamports()?=add(a[5].lamports(),a[3].lamports())?;
        **a[3].try_borrow_mut_lamports()?=0;a[3].try_borrow_mut_data()?.fill(0);
        t.version=3;t.changed()?;return write(&a[1],&*t);
    }
    check(data[0]==104 && t.version==4 && a.len()>=10,9)?;
    let mut cfg=config(p,&a[9],a[1].key,a[2].key)?;
    let r=u64_at(data,1)?;
    let s=purse(p,&a[3],a[1].key,r)?;
    check(s.bank==*a[2].key && *a[4].key==s.nonce && t.round.id>r
        && *a[7].key==solana_program::sysvar::recent_blockhashes::id()
        && *a[8].key==solana_program::sysvar::rent::id(),881)?;
    nonce(&a[4],a[3].key,s.nonce_loan)?;
    let members=s.wallets.iter().filter(|w|**w!=Pubkey::default()).count();
    check(members>0 && a.len()==10+members*2,9)?;
    let total=s.credits.iter().try_fold(0u64,|n,v|add(n,*v))?;
    let unused=a[4].lamports().saturating_sub(s.nonce_rent).min(total);
    let mut pos=0;
    for wallet in s.wallets.iter().filter(|w|**w!=Pubkey::default()) {
        let event=economy::game_rewards::event_address(p,a[2].key,a[1].key,r,wallet).0;
        let receipt=economy::receipt_address(p,&event).0;
        check(*a[10+2*pos].key==*wallet && *a[11+2*pos].key==receipt
            && a[11+2*pos].owner==&system_program::id() && a[11+2*pos].data_is_empty(),882)?;
        pos+=1;
    }
    // Fee deduction has already happened, including the fee of this cleanup.
    // Withdraw to the program-owned purse, then conserve every lamport.
    let ix=system_instruction::withdraw_nonce_account(a[4].key,a[3].key,a[3].key,a[4].lamports());
    invoke_signed(&ix,&[a[4].clone(),a[3].clone(),a[7].clone(),a[8].clone(),a[6].clone()],
        &[&[PURSE_SEED,a[1].key.as_ref(),&r.to_le_bytes(),&[s.bump]]])?;
    let mut allocated=0u64;let mut weight=0u64;pos=0;
    for i in 0..10 {if s.credits[i]==0{continue;}
        weight=add(weight,s.credits[i])?;
        let through=(u128::from(unused)*u128::from(weight)/u128::from(total)) as u64;
        let refund=through-allocated;allocated=through;
        **a[10+2*pos].try_borrow_mut_lamports()?=add(a[10+2*pos].lamports(),refund)?;
        **a[3].try_borrow_mut_lamports()?-=refund;
        msg!("SDICE_BLACKJACK_NETWORK_REFUND round={} wallet={} reserved={} returned={} spent={}",r,s.wallets[i],s.credits[i],refund,s.credits[i]-refund);
        pos+=1;
    }
    check(allocated==unused && a[3].lamports()>=add(s.house_rent,s.nonce_loan)?,880)?;
    // Donations do not count as player spend or decrease recovered principal.
    **a[5].try_borrow_mut_lamports()?=add(a[5].lamports(),a[3].lamports())?;
    **a[3].try_borrow_mut_lamports()?=0;a[3].try_borrow_mut_data()?.fill(0);
    cfg.open_purses=cfg.open_purses.checked_sub(1).ok_or(ProgramError::InvalidAccountData)?;write(&a[9],&cfg)?;
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test]fn fixed_layouts(){
        assert_eq!(borsh::to_vec(&Config{magic:CONFIG_MAGIC,table:Pubkey::default(),bank:Pubkey::default(),issuer:Pubkey::default(),rent:0,open_purses:0,bump:0}).unwrap().len(),CONFIG_LEN);
        assert_eq!(borsh::to_vec(&Purse{magic:PURSE_MAGIC,table:Pubkey::default(),bank:Pubkey::default(),nonce:Pubkey::default(),round:0,house_rent:0,nonce_rent:0,nonce_loan:0,wallets:[Pubkey::default();10],credits:[0;10],bump:0}).unwrap().len(),PURSE_LEN);
    }
}
