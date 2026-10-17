#![allow(unexpected_cfgs, deprecated)]
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{
    account_info::{next_account_info, AccountInfo}, bpf_loader_upgradeable,
    clock::Clock, entrypoint::ProgramResult, hash::hashv, log::sol_log_data,
    program::{invoke, invoke_signed}, program_error::ProgramError, pubkey::Pubkey,
    rent::Rent, system_instruction, system_program, sysvar::Sysvar,
};
use spl_token_2022::{extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions}, state::{Account as TokenAccount, Mint}};

pub mod combat;
// Use the existing fixed ORAO adapter without changing the deployed dice program.
#[path = "../../program/src/orao.rs"]
mod orao;

#[cfg(not(feature = "no-entrypoint"))]
solana_program::entrypoint!(process_instruction);

pub const CONFIG_LEN: usize = 99;
pub const COUNTER_LEN: usize = 42;
pub const FIGHT_LEN: usize = 239 + combat::MAX_STRIKES * 9;
pub const MAX_WAGER: u64 = 10_000_000_000_000;
pub const MAX_SIDE_BUDGET: u64 = 10_000_000;
pub const STEP_REWARD: u64 = 5_000;
pub const EXECUTION_RESERVE: u64 = 50_000;
pub const ORACLE_TIMEOUT: i64 = 180;
pub const RETENTION: i64 = 3600;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Config {
    pub version: u8, pub mint: Pubkey, pub bank_vault: Pubkey, pub bank_owner: Pubkey,
    pub decimals: u8, pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Counter { pub version: u8, pub player: Pubkey, pub next: u64, pub bump: u8 }
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Fight {
    pub version: u8, pub challenger: Pubkey, pub opponent: Pubkey,
    pub nonce: u64, pub wager: u64, pub side_budget: u64, pub credit: u64,
    pub oracle_cost: u64, pub expires: i64, pub accepted_at: i64, pub finished_at: i64,
    // 0 offered, 1 oracle pending, 2 combat (including knockout), 3 paid, 4 refunded.
    pub state: u8, pub bump: u8, pub first: u8, pub count: u8, pub hp: [u8;2],
    pub random: [u8;64], pub oracle_floor: u64,
    pub oracle_seed: [u8;32],
    pub strikes: [combat::Strike; combat::MAX_STRIKES],
}

fn check(ok: bool, code: u32) -> ProgramResult { if ok { Ok(()) } else { Err(ProgramError::Custom(code)) } }
fn signer(a: &AccountInfo) -> ProgramResult { check(a.is_signer, 1) }
fn u64_at(data: &[u8], offset: usize) -> Result<u64, ProgramError> {
    Ok(u64::from_le_bytes(data.get(offset..offset+8).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))
}
fn write<T: BorshSerialize>(account: &AccountInfo, value: &T) -> ProgramResult {
    value.serialize(&mut &mut account.try_borrow_mut_data()?[..]).map_err(|_|ProgramError::InvalidAccountData)
}
fn read<T: BorshDeserialize>(program: &Pubkey, account: &AccountInfo) -> Result<T, ProgramError> {
    check(account.owner == program, 2)?;
    T::try_from_slice(&account.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)
}
fn config(program: &Pubkey, a: &AccountInfo) -> Result<Config, ProgramError> {
    let c: Config = read(program,a)?;
    let (key,bump) = Pubkey::find_program_address(&[b"fight-config-v1"],program);
    check(*a.key == key && c.bump == bump && c.version == 1,3)?; Ok(c)
}
fn fight(program: &Pubkey, a: &AccountInfo) -> Result<Fight, ProgramError> {
    let f: Fight = read(program,a)?;
    let (key,bump) = Pubkey::find_program_address(&[b"fight-v1",f.challenger.as_ref(),&f.nonce.to_le_bytes()],program);
    check(*a.key == key && f.bump == bump && f.version == 1,3)?; Ok(f)
}
fn valid_mint(a: &AccountInfo) -> Result<u8, ProgramError> {
    check(*a.owner == spl_token_2022::id(),4)?;
    let data = a.try_borrow_data()?;
    let mint = StateWithExtensions::<Mint>::unpack(&data)?;
    check(mint.base.decimals == 6 && mint.base.freeze_authority.is_none()
        && mint.get_extension_types()?.iter().all(|e| matches!(e,ExtensionType::MetadataPointer | ExtensionType::TokenMetadata)),5)?;
    Ok(mint.base.decimals)
}
fn token_account(a: &AccountInfo, mint: &Pubkey, owner: &Pubkey) -> Result<u64,ProgramError> {
    check(*a.owner == spl_token_2022::id(),4)?;
    let data = a.try_borrow_data()?;
    let t = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    check(t.base.mint == *mint && t.base.owner == *owner && !t.base.is_frozen()
        && t.base.delegate.is_none() && t.base.close_authority.is_none(),6)?;
    Ok(t.base.amount)
}
fn create_pda<'a>(payer: &AccountInfo<'a>, account: &AccountInfo<'a>, system: &AccountInfo<'a>, owner: &Pubkey, len: usize, seeds: &[&[u8]]) -> ProgramResult {
    check(*system.key == system_program::id(),7)?;
    check(account.data_is_empty() && *account.owner == system_program::id(),8)?;
    let rent = Rent::get()?.minimum_balance(len);
    if account.lamports() == 0 {
        invoke_signed(&system_instruction::create_account(payer.key,account.key,rent,len as u64,owner),&[payer.clone(),account.clone(),system.clone()],&[seeds])
    } else {
        let needed = rent.saturating_sub(account.lamports());
        if needed > 0 { invoke(&system_instruction::transfer(payer.key,account.key,needed),&[payer.clone(),account.clone(),system.clone()])?; }
        invoke_signed(&system_instruction::allocate(account.key,len as u64),&[account.clone(),system.clone()],&[seeds])?;
        invoke_signed(&system_instruction::assign(account.key,owner),&[account.clone(),system.clone()],&[seeds])
    }
}
fn lamports(from: &AccountInfo, to: &AccountInfo, amount: u64) -> ProgramResult {
    check(from.key != to.key,6)?;
    let a = from.lamports().checked_sub(amount).ok_or(ProgramError::InsufficientFunds)?;
    let b = to.lamports().checked_add(amount).ok_or(ProgramError::ArithmeticOverflow)?;
    **from.try_borrow_mut_lamports()? = a; **to.try_borrow_mut_lamports()? = b; Ok(())
}
fn transfer<'a>(from: &AccountInfo<'a>, to: &AccountInfo<'a>, mint: &AccountInfo<'a>, authority: &AccountInfo<'a>, token: &AccountInfo<'a>, amount: u64, seeds: Option<&[&[u8]]>) -> ProgramResult {
    check(*token.key == spl_token_2022::id() && from.key != to.key,4)?;
    let ix = spl_token_2022::instruction::transfer_checked(token.key,from.key,mint.key,to.key,authority.key,&[],amount,6)?;
    let infos = [from.clone(),mint.clone(),to.clone(),authority.clone(),token.clone()];
    if let Some(s) = seeds { invoke_signed(&ix,&infos,&[s]) } else { invoke(&ix,&infos) }
}
fn escrow(program: &Pubkey, a: &AccountInfo, f: &AccountInfo, mint: &Pubkey) -> ProgramResult {
    check(*a.key == Pubkey::find_program_address(&[b"fight-vault-v1",f.key.as_ref()],program).0,3)?;
    token_account(a,mint,f.key)?; Ok(())
}
fn seed(program: &Pubkey, f: &Pubkey, salt: &[u8]) -> [u8;32] {
    hashv(&[b"hooddice:fight:oracle:v1",program.as_ref(),f.as_ref(),salt]).to_bytes()
}
fn oracle_payer(program: &Pubkey, f: &Pubkey) -> (Pubkey,u8) {
    Pubkey::find_program_address(&[b"fight-oracle-v1",f.as_ref()],program)
}
fn credit_reward(f: &mut Fight, account: &AccountInfo, keeper: &AccountInfo) -> ProgramResult {
    f.credit = f.credit.checked_sub(STEP_REWARD).ok_or(ProgramError::InsufficientFunds)?;
    lamports(account,keeper,STEP_REWARD)
}
fn destination(a: &AccountInfo, mint: &Pubkey, owner: &Pubkey) -> ProgramResult {
    check(*a.owner == spl_token_2022::id(),4)?;
    let data = a.try_borrow_data()?;
    let t = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    // A user's optional delegate/close authority cannot hold the opponent's payout
    // hostage. Transfers still go only to a token account owned by the bound wallet.
    check(t.base.mint == *mint && t.base.owner == *owner && !t.base.is_frozen(),6)
}
fn recover_oracle<'a>(program: &Pubkey, f: &mut Fight, account: &AccountInfo<'a>, payer: &AccountInfo<'a>, system: &AccountInfo<'a>) -> Result<u64,ProgramError> {
    let (pk,pb) = oracle_payer(program,account.key);
    check(*payer.key == pk && payer.owner == &system_program::id() && payer.data_is_empty() && *system.key == system_program::id(),3)?;
    let rebate = payer.lamports().saturating_sub(f.oracle_floor).min(f.oracle_cost);
    if rebate > 0 {
        invoke_signed(&system_instruction::transfer(payer.key,account.key,rebate),&[payer.clone(),account.clone(),system.clone()],&[&[b"fight-oracle-v1",account.key.as_ref(),&[pb]]])?;
        f.oracle_cost -= rebate; f.credit += rebate;
    }
    Ok(rebate)
}
fn ensure_destination<'a>(program: &Pubkey, f: &mut Fight, account: &AccountInfo<'a>, destination_account: &AccountInfo<'a>, owner: &AccountInfo<'a>, mint: &AccountInfo<'a>, payer: &AccountInfo<'a>, system: &AccountInfo<'a>, token: &AccountInfo<'a>, associated: &AccountInfo<'a>) -> ProgramResult {
    if destination_account.owner == &spl_token_2022::id() {
        return destination(destination_account,mint.key,owner.key);
    }
    let ata_id = solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
    check(*associated.key == ata_id && associated.executable && *token.key == spl_token_2022::id()
        && *system.key == system_program::id(),4)?;
    check(*destination_account.key == Pubkey::find_program_address(&[owner.key.as_ref(),token.key.as_ref(),mint.key.as_ref()],&ata_id).0
        && destination_account.owner == &system_program::id() && destination_account.data_is_empty(),6)?;
    let (pk,pb) = oracle_payer(program,account.key);
    check(*payer.key == pk && payer.owner == &system_program::id() && payer.data_is_empty(),3)?;
    // Token-2022 ATAs include ImmutableOwner. Recreate only inside atomic final
    // settlement, so repeatedly closing an empty ATA cannot drain fight credit.
    let allowance = Rent::get()?.minimum_balance(170).saturating_sub(destination_account.lamports());
    check(f.credit >= allowance + if f.accepted_at != 0 { STEP_REWARD } else { 0 },102)?;
    let before = payer.lamports();
    if allowance > 0 { lamports(account,payer,allowance)?; }
    use solana_program::instruction::{AccountMeta,Instruction};
    let ix = Instruction { program_id:ata_id, data:vec![1], accounts:vec![
        AccountMeta::new(*payer.key,true),AccountMeta::new(*destination_account.key,false),
        AccountMeta::new_readonly(*owner.key,false),AccountMeta::new_readonly(*mint.key,false),
        AccountMeta::new_readonly(*system.key,false),AccountMeta::new_readonly(*token.key,false),
        AccountMeta::new(*account.key,false),
    ]};
    let ps: &[&[u8]] = &[b"fight-oracle-v1",account.key.as_ref(),&[pb]];
    invoke_signed(&ix,&[payer.clone(),destination_account.clone(),owner.clone(),mint.clone(),system.clone(),token.clone(),account.clone(),associated.clone()],&[ps])?;
    let cost = before.checked_add(allowance).and_then(|v|v.checked_sub(payer.lamports())).ok_or(ProgramError::ArithmeticOverflow)?;
    check(cost <= allowance,102)?;
    if allowance > cost {
        invoke_signed(&system_instruction::transfer(payer.key,account.key,allowance-cost),&[payer.clone(),account.clone(),system.clone()],&[ps])?;
    }
    f.credit -= cost;
    destination(destination_account,mint.key,owner.key)
}

/// Instructions and accounts are documented in chain/fight-client.ts.
/// Every variable-length instruction has an exact length; accounts are independently bound.
pub fn process_instruction(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let tag = *data.first().ok_or(ProgramError::InvalidInstructionData)?;
    let mut iter = accounts.iter();
    if tag == 0 {
        check(data.len() == 1,9)?;
        let authority = next_account_info(&mut iter)?;
        let cfg = next_account_info(&mut iter)?;
        let mint = next_account_info(&mut iter)?;
        let bank_vault = next_account_info(&mut iter)?;
        let bank_owner = next_account_info(&mut iter)?;
        let program_data = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        signer(authority)?;
        check(program_data.owner == &bpf_loader_upgradeable::id()
            && *program_data.key == Pubkey::find_program_address(&[program.as_ref()],&bpf_loader_upgradeable::id()).0,100)?;
        {
            let d = program_data.try_borrow_data()?;
            check(d.len() >= 45 && d[..4] == 3u32.to_le_bytes() && d[12] == 1 && &d[13..45] == authority.key.as_ref(),100)?;
        }
        let decimals = valid_mint(mint)?;
        token_account(bank_vault,mint.key,bank_owner.key)?;
        let (expected,bump) = Pubkey::find_program_address(&[b"fight-config-v1"],program);
        check(*cfg.key == expected,3)?;
        create_pda(authority,cfg,system,program,CONFIG_LEN,&[b"fight-config-v1",&[bump]])?;
        return write(cfg,&Config { version:1,mint:*mint.key,bank_vault:*bank_vault.key,bank_owner:*bank_owner.key,decimals,bump });
    }
    if tag == 1 {
        check(data.len() == 33,9)?;
        let player = next_account_info(&mut iter)?;
        let opponent = next_account_info(&mut iter)?;
        let cfg = next_account_info(&mut iter)?;
        let counter = next_account_info(&mut iter)?;
        let account = next_account_info(&mut iter)?;
        let vault = next_account_info(&mut iter)?;
        let mint = next_account_info(&mut iter)?;
        let source = next_account_info(&mut iter)?;
        let token = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        signer(player)?;
        check(player.key != opponent.key && *opponent.key != Pubkey::default(),101)?;
        let c = config(program,cfg)?;
        check(*mint.key == c.mint && *token.key == spl_token_2022::id(),4)?;
        valid_mint(mint)?; token_account(source,mint.key,player.key)?;
        let nonce = u64_at(data,1)?;
        let wager = u64_at(data,9)?;
        let budget = u64_at(data,17)?;
        let expires = i64::from_le_bytes(data[25..33].try_into().unwrap());
        let now = Clock::get()?.unix_timestamp;
        check(wager > 0 && wager <= MAX_WAGER,10)?;
        check(budget >= Rent::get()?.minimum_balance(170) + EXECUTION_RESERVE && budget <= MAX_SIDE_BUDGET,102)?;
        check(expires > now && expires <= now + 900,103)?;
        let (counter_key,cb) = Pubkey::find_program_address(&[b"fight-counter-v1",player.key.as_ref()],program);
        check(*counter.key == counter_key,3)?;
        let mut n = if counter.owner == program {
            let n: Counter = read(program,counter)?;
            check(n.version == 1 && n.player == *player.key && n.bump == cb,3)?; n
        } else {
            create_pda(player,counter,system,program,COUNTER_LEN,&[b"fight-counter-v1",player.key.as_ref(),&[cb]])?;
            Counter { version:1, player:*player.key, next:0, bump:cb }
        };
        check(n.next == nonce,104)?;
        n.next = n.next.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        let (expected,bump) = Pubkey::find_program_address(&[b"fight-v1",player.key.as_ref(),&nonce.to_le_bytes()],program);
        check(*account.key == expected,3)?;
        create_pda(player,account,system,program,FIGHT_LEN,&[b"fight-v1",player.key.as_ref(),&nonce.to_le_bytes(),&[bump]])?;
        let (vk,vb) = Pubkey::find_program_address(&[b"fight-vault-v1",account.key.as_ref()],program);
        check(*vault.key == vk,3)?;
        create_pda(player,vault,system,token.key,165,&[b"fight-vault-v1",account.key.as_ref(),&[vb]])?;
        invoke(&spl_token_2022::instruction::initialize_account3(token.key,vault.key,mint.key,account.key)?,&[vault.clone(),mint.clone(),token.clone()])?;
        transfer(source,vault,mint,player,token,wager,None)?;
        invoke(&system_instruction::transfer(player.key,account.key,budget),&[player.clone(),account.clone(),system.clone()])?;
        write(counter,&n)?;
        let f = Fight { version:1,challenger:*player.key,opponent:*opponent.key,nonce,wager,side_budget:budget,credit:budget,
            oracle_cost:0,expires,accepted_at:0,finished_at:0,state:0,bump,first:0,count:0,hp:[100,100],
            random:[0;64],oracle_floor:0,oracle_seed:[0;32],strikes:[combat::Strike::default();combat::MAX_STRIKES] };
        sol_log_data(&[b"FightOfferedV1",account.key.as_ref(),&borsh::to_vec(&f)?[..239]]) ;
        return write(account,&f);
    }
    if tag == 2 {
        check(data.len() == 49,9)?;
        let player = next_account_info(&mut iter)?;
        let cfg = next_account_info(&mut iter)?;
        let account = next_account_info(&mut iter)?;
        let vault = next_account_info(&mut iter)?;
        let mint = next_account_info(&mut iter)?;
        let source = next_account_info(&mut iter)?;
        let token = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        let payer = next_account_info(&mut iter)?;
        let network = next_account_info(&mut iter)?;
        let treasury = next_account_info(&mut iter)?;
        let request = next_account_info(&mut iter)?;
        let oracle = next_account_info(&mut iter)?;
        signer(player)?;
        let c = config(program,cfg)?;
        let mut f = fight(program,account)?;
        let now = Clock::get()?.unix_timestamp;
        check(f.state == 0 && now < f.expires,105)?;
        check(*player.key == f.opponent,101)?;
        check(u64_at(data,1)? == f.wager && u64_at(data,9)? == f.side_budget,106)?;
        check(*mint.key == c.mint,4)?; valid_mint(mint)?;
        escrow(program,vault,account,mint.key)?;
        token_account(source,mint.key,player.key)?;
        transfer(source,vault,mint,player,token,f.wager,None)?;
        invoke(&system_instruction::transfer(player.key,account.key,f.side_budget),&[player.clone(),account.clone(),system.clone()])?;
        f.credit = f.side_budget * 2;
        let (pk,pb) = oracle_payer(program,account.key);
        check(*payer.key == pk && payer.owner == &system_program::id() && payer.data_is_empty(),3)?;
        let before = payer.lamports();
        let allowance = f.credit - EXECUTION_RESERVE;
        lamports(account,payer,allowance)?;
        let ps: &[&[u8]] = &[b"fight-oracle-v1",account.key.as_ref(),&[pb]];
        // Acceptance adds unpredictable salt, so a spectator cannot reserve the
        // public ORAO address while the challenge waits. Fresh-request checks still
        // reject any salt whose outcome was requested or known before acceptance.
        f.oracle_seed = seed(program,account.key,&data[17..49]);
        orao::request_fresh_signed(payer,network,treasury,request,system,oracle,&f.oracle_seed,Some(ps),Some(account))?;
        let cost = before.checked_add(allowance).and_then(|v| v.checked_sub(payer.lamports())).ok_or(ProgramError::ArithmeticOverflow)?;
        check(cost <= allowance,102)?;
        // Even a hand-crafted challenge must fund both closed payout accounts.
        // Otherwise one participant could close their ATA during oracle timeout
        // and prevent the other participant from recovering their stake.
        check(f.credit - cost >= EXECUTION_RESERVE + Rent::get()?.minimum_balance(170) * 2,102)?;
        // Return only our unused allowance; pre-existing dust is never spent or claimed.
        let unused = allowance-cost;
        if unused > 0 {
            invoke_signed(&system_instruction::transfer(payer.key,account.key,unused),&[payer.clone(),account.clone(),system.clone()],&[ps])?;
        }
        f.credit -= cost; f.oracle_cost = cost; f.oracle_floor = before; f.accepted_at = now; f.state = 1;
        sol_log_data(&[b"FightAcceptedV1",account.key.as_ref(),request.key.as_ref(),&now.to_le_bytes(),&cost.to_le_bytes()]);
        return write(account,&f);
    }
    if tag == 3 {
        check(data.len() == 1,9)?;
        let keeper = next_account_info(&mut iter)?;
        let account = next_account_info(&mut iter)?;
        let request = next_account_info(&mut iter)?;
        let payer = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        signer(keeper)?;
        let mut f = fight(program,account)?;
        check(f.state == 1 || f.state == 2,105)?;
        check(!f.hp.contains(&0),105)?;
        let random = orao::read_request(request,&f.oracle_seed,&oracle_payer(program,account.key).0)?.ok_or(ProgramError::Custom(107))?;
        recover_oracle(program,&mut f,account,payer,system)?;
        if f.state == 1 {
            f.random = random; f.first = combat::initiative(&random,account.key); f.state = 2;
            sol_log_data(&[b"FightRandomV1",account.key.as_ref(),&random]);
        } else { check(random == f.random,40)?; }
        let start = f.count;
        for _ in 0..8 {
            check((f.count as usize) < combat::MAX_STRIKES,110)?;
            let roll = combat::dice(&f.random,account.key,f.count)?;
            let s = combat::strike(f.hp,(f.first+f.count)%2,f.count,roll)?;
            f.strikes[f.count as usize] = s; f.count += 1; f.hp = s.hp;
            if f.hp.contains(&0) { break; }
        }
        credit_reward(&mut f,account,keeper)?;
        sol_log_data(&[b"FightStrikesV1",account.key.as_ref(),&[start],&borsh::to_vec(&f.strikes[start as usize..f.count as usize])?]);
        return write(account,&f);
    }
    if tag == 4 || tag == 5 {
        check(data.len() == 1,9)?;
        let keeper = next_account_info(&mut iter)?;
        let cfg = next_account_info(&mut iter)?;
        let account = next_account_info(&mut iter)?;
        let vault = next_account_info(&mut iter)?;
        let mint = next_account_info(&mut iter)?;
        let challenger = next_account_info(&mut iter)?;
        let opponent = next_account_info(&mut iter)?;
        let challenger_tokens = next_account_info(&mut iter)?;
        let opponent_tokens = next_account_info(&mut iter)?;
        let bank_vault = next_account_info(&mut iter)?;
        let token = next_account_info(&mut iter)?;
        let request = next_account_info(&mut iter)?;
        let payer = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        let associated = next_account_info(&mut iter)?;
        signer(keeper)?;
        let c = config(program,cfg)?;
        let mut f = fight(program,account)?;
        check(f.state < 3,105)?;
        check(*mint.key == c.mint && *bank_vault.key == c.bank_vault,4)?;
        valid_mint(mint)?;
        check(*challenger.key == f.challenger && *opponent.key == f.opponent,101)?;
        token_account(bank_vault,mint.key,&c.bank_owner)?;
        escrow(program,vault,account,mint.key)?;
        let now = Clock::get()?.unix_timestamp;
        let nonce = f.nonce.to_le_bytes(); let bump = [f.bump];
        let player_key = f.challenger;
        let fs: &[&[u8]] = &[b"fight-v1",player_key.as_ref(),&nonce,&bump];
        let accepted = f.state != 0;
        let mut payout = 0;
        let mut fee = 0;
        let mut winner = 255u8;
        if tag == 4 {
            check(f.state == 2 && f.hp.contains(&0) && f.hp != [0,0],105)?;
            let pot = f.wager * 2; fee = pot / 100; payout = pot-fee;
            winner = if f.hp[0] == 0 { 1 } else { 0 };
            let recipient = if winner == 0 { challenger_tokens } else { opponent_tokens };
            ensure_destination(program,&mut f,account,recipient,if winner == 0 { challenger } else { opponent },mint,payer,system,token,associated)?;
            transfer(vault,recipient,mint,account,token,payout,Some(fs))?;
            if fee > 0 { transfer(vault,bank_vault,mint,account,token,fee,Some(fs))?; }
            f.state = 3;
        } else {
            if !accepted {
                check(*keeper.key == f.challenger || *keeper.key == f.opponent || now >= f.expires,108)?;
            } else {
                check(f.state == 1 && now >= f.accepted_at + ORACLE_TIMEOUT,108)?;
                // Fulfilled randomness must settle, even after the timeout deadline.
                check(orao::read_request(request,&f.oracle_seed,&oracle_payer(program,account.key).0)?.is_none(),109)?;
            }
            ensure_destination(program,&mut f,account,challenger_tokens,challenger,mint,payer,system,token,associated)?;
            transfer(vault,challenger_tokens,mint,account,token,f.wager,Some(fs))?;
            if accepted {
                ensure_destination(program,&mut f,account,opponent_tokens,opponent,mint,payer,system,token,associated)?;
                transfer(vault,opponent_tokens,mint,account,token,f.wager,Some(fs))?;
            }
            f.state = 4;
        }
        // Third-party token donations cannot grief closing a settled vault. They are
        // returned to the vault creator and are never included in prize/fee accounting.
        let dust = token_account(vault,mint.key,account.key)?;
        if dust > 0 {
            ensure_destination(program,&mut f,account,challenger_tokens,challenger,mint,payer,system,token,associated)?;
            transfer(vault,challenger_tokens,mint,account,token,dust,Some(fs))?;
        }
        invoke_signed(&spl_token_2022::instruction::close_account(token.key,vault.key,challenger.key,account.key,&[])?,&[vault.clone(),challenger.clone(),account.clone(),token.clone()],&[fs])?;
        // Perform direct lamport writes after the final CPI, so the runtime sees
        // both sides of every balance change at the same invocation boundary.
        if accepted { credit_reward(&mut f,account,keeper)?; }
        let opponent_refund = if accepted { f.credit/2 } else { 0 };
        let challenger_refund = f.credit-opponent_refund;
        if challenger_refund > 0 { lamports(account,challenger,challenger_refund)?; }
        if opponent_refund > 0 { lamports(account,opponent,opponent_refund)?; }
        f.credit = 0; f.finished_at = now;
        sol_log_data(&[b"FightFinishedV1",account.key.as_ref(),&[f.state,winner],&payout.to_le_bytes(),&fee.to_le_bytes(),&challenger_refund.to_le_bytes(),&opponent_refund.to_le_bytes()]);
        return write(account,&f);
    }
    if tag == 6 {
        check(data.len() == 1,9)?;
        let account = next_account_info(&mut iter)?;
        let challenger = next_account_info(&mut iter)?;
        let request = next_account_info(&mut iter)?;
        let payer = next_account_info(&mut iter)?;
        let f = fight(program,account)?;
        check(f.state >= 3 && f.credit == 0 && Clock::get()?.unix_timestamp >= f.finished_at + RETENTION,108)?;
        check(*challenger.key == f.challenger,101)?;
        if f.accepted_at != 0 {
            // Preserve late oracle-rent recovery after a timeout refund.
            check(orao::read_request(request,&f.oracle_seed,&oracle_payer(program,account.key).0)?.is_some(),107)?;
            check(*payer.key == oracle_payer(program,account.key).0
                && payer.lamports().saturating_sub(f.oracle_floor).min(f.oracle_cost) == 0,111)?;
        }
        lamports(account,challenger,account.lamports())?;
        account.resize(0)?; account.assign(&system_program::id());
        return Ok(());
    }
    if tag == 7 {
        check(data.len() == 1,9)?;
        let account = next_account_info(&mut iter)?;
        let payer = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        let request = next_account_info(&mut iter)?;
        let challenger = next_account_info(&mut iter)?;
        let opponent = next_account_info(&mut iter)?;
        let mut f = fight(program,account)?;
        check(f.state >= 3 && f.accepted_at != 0 && f.credit == 0,105)?;
        check(*challenger.key == f.challenger && *opponent.key == f.opponent,101)?;
        check(orao::read_request(request,&f.oracle_seed,&oracle_payer(program,account.key).0)?.is_some(),107)?;
        recover_oracle(program,&mut f,account,payer,system)?;
        let second = f.credit/2;
        if f.credit-second > 0 { lamports(account,challenger,f.credit-second)?; }
        if second > 0 { lamports(account,opponent,second)?; }
        f.credit = 0;
        return write(account,&f);
    }
    Err(ProgramError::InvalidInstructionData)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn state_layout_is_fixed_for_the_client() {
        let f = Fight { version:1,challenger:Pubkey::default(),opponent:Pubkey::default(),nonce:0,wager:1,side_budget:0,credit:0,oracle_cost:0,
            expires:0,accepted_at:0,finished_at:0,state:0,bump:0,first:0,count:0,hp:[100,100],random:[0;64],oracle_floor:0,oracle_seed:[0;32],strikes:[combat::Strike::default();64] };
        assert_eq!(borsh::to_vec(&f).unwrap().len(),FIGHT_LEN);
        assert_eq!(borsh::to_vec(&Config {version:1,mint:Pubkey::default(),bank_vault:Pubkey::default(),bank_owner:Pubkey::default(),decimals:6,bump:0}).unwrap().len(),CONFIG_LEN);
        assert_eq!(borsh::to_vec(&Counter {version:1,player:Pubkey::default(),next:0,bump:0}).unwrap().len(),COUNTER_LEN);
        assert!(EXECUTION_RESERVE >= (combat::MAX_STRIKES as u64/8+1)*STEP_REWARD);
    }
}
