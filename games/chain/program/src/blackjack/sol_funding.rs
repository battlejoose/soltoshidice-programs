//! Opt-in v2 blackjack funding. House capital may fund only our closable SOL
//! escrow rent. ORAO receives player SOL; its refund identity survives closing
//! the escrow, leaving the table, and advancing to a different round.
use super::{rules::{self, Draw, Stage}, state::*};
use crate::*;

const ESCROW_SEED: &[u8] = b"bj-sol-deposit-v1";
const RESERVE_SEED: &[u8] = b"bj-rent-capital-v1";
const PAYER_SEED: &[u8] = b"bj-draw-payer-v1";
const MAGIC: [u8; 8] = *b"BJDEPS01";
pub const LEN: usize = 99;
pub const FEE_CAP: u64 = 20_000_000;
const PENDING_LEN: usize = 77 + 7 * 96;

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct Deposit {
    pub magic: [u8; 8], pub version: u8, pub table: Pubkey, pub wallet: Pubkey,
    pub house_rent: u64, pub round: u64, pub held_requests: u8,
    pub max_oracle_fee: u64, pub bump: u8,
}

fn add(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn sub(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_sub(b).ok_or(ProgramError::Custom(861)) }
pub fn deposit_address(program: &Pubkey, table: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ESCROW_SEED, table.as_ref(), wallet.as_ref()], program)
}
pub fn reserve_address(program: &Pubkey, table: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[RESERVE_SEED, table.as_ref()], program)
}
fn load(program: &Pubkey, a: &AccountInfo, table: &Pubkey, wallet: &Pubkey) -> Result<Deposit, ProgramError> {
    check(a.owner == program && a.data_len() == LEN, 860)?;
    let d = Deposit::try_from_slice(&a.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (key, bump) = deposit_address(program, table, wallet);
    check(*a.key == key && d.bump == bump && d.magic == MAGIC && d.version == 1
        && d.table == *table && d.wallet == *wallet && d.house_rent <= a.lamports()
        && d.max_oracle_fee <= FEE_CAP && d.held_requests <= 16, 860)?;
    Ok(d)
}
pub(super) fn reserve_valid(program: &Pubkey, a: &AccountInfo, table: &Pubkey) -> ProgramResult {
    check(*a.key == reserve_address(program, table).0 && a.owner == &system_program::id() && a.data_is_empty(), 860)
}
pub(super) fn reserve_transfer<'a>(program: &Pubkey, table: &Pubkey, from: &AccountInfo<'a>, to: &AccountInfo<'a>, system: &AccountInfo<'a>, amount: u64) -> ProgramResult {
    reserve_valid(program, from, table)?;
    check(*system.key == system_program::id() && from.key != to.key, 860)?;
    let bump = reserve_address(program, table).1;
    invoke_signed(&system_instruction::transfer(from.key, to.key, amount),
        &[from.clone(), to.clone(), system.clone()], &[&[RESERVE_SEED, table.as_ref(), &[bump]]])
}
fn move_player_sol(from: &AccountInfo, to: &AccountInfo, d: &Deposit, amount: u64) -> ProgramResult {
    check(from.key != to.key, 860)?;
    let left = sub(from.lamports(), amount)?;
    check(left >= d.house_rent.max(Rent::get()?.minimum_balance(LEN)), 861)?;
    **from.try_borrow_mut_lamports()? = left;
    **to.try_borrow_mut_lamports()? = add(to.lamports(), amount)?;
    Ok(())
}
fn budget(d: &Deposit, t: &Table) -> Result<u64, ProgramError> {
    add(if t.version >= 3 { 0 } else { Rent::get()?.minimum_balance(PENDING_LEN) }, d.max_oracle_fee)
}
fn fee(t: &Table, network: &AccountInfo) -> Result<u64, ProgramError> {
    if t.version >= 3 { super::magicblock::fee(network) } else { orao::request_fee(network) }
}
fn normalize(d: &mut Deposit, t: &Table) -> ProgramResult {
    check(d.round <= t.round.id, 860)?;
    if d.round < t.round.id || !t.round.active() { d.held_requests = 0; }
    d.round = t.round.id;
    Ok(())
}
fn backed(a: &AccountInfo, d: &Deposit, t: &Table) -> ProgramResult {
    let held = budget(d, t)?.checked_mul(u64::from(d.held_requests)).ok_or(ProgramError::ArithmeticOverflow)?;
    check(a.lamports() >= add(d.house_rent.max(Rent::get()?.minimum_balance(LEN)), held)?, 861)
}
fn close_deposit(account: &AccountInfo, reserve: &AccountInfo, wallet: &AccountInfo, d: &Deposit) -> ProgramResult {
    check(d.held_requests == 0 && account.key != reserve.key && account.key != wallet.key && reserve.key != wallet.key, 861)?;
    let player = sub(account.lamports(), d.house_rent)?;
    **reserve.try_borrow_mut_lamports()? = add(reserve.lamports(), d.house_rent)?;
    **wallet.try_borrow_mut_lamports()? = add(wallet.lamports(), player)?;
    **account.try_borrow_mut_lamports()? = 0;
    account.try_borrow_mut_data()?.fill(0);
    msg!("SDICE_BLACKJACK_SOL_CLOSED wallet={} house={} player={}", wallet.key, d.house_rent, player);
    Ok(())
}
pub fn close_after_leave(program: &Pubkey, table: &Pubkey, t: &Table, owner: &Pubkey,
    account: &AccountInfo, reserve: &AccountInfo, wallet: &AccountInfo) -> ProgramResult {
    check(*account.key == deposit_address(program, table, owner).0 && *wallet.key == *owner, 860)?;
    reserve_valid(program, reserve, table)?;
    if account.owner == &system_program::id() && account.data_is_empty() { return Ok(()); }
    let mut d = load(program, account, table, owner)?;
    normalize(&mut d, t)?;
    close_deposit(account, reserve, wallet, &d)
}

/// Called after the native bet/action checks in the same atomic transaction.
/// Initial and dealer draws each reserve one request; a split reserves both new
/// cards, including the replacement drawn automatically on a later turn.
pub fn hold(program: &Pubkey, t: &Table, table: &Pubkey, a: &AccountInfo, network: &AccountInfo, seat: usize, requests: u8) -> ProgramResult {
    if requests == 0 { return Ok(()); }
    let mut d = load(program, a, table, &t.seats[seat].wallet)?;
    normalize(&mut d, t)?;
    check(fee(t, network)? <= d.max_oracle_fee, 862)?;
    d.held_requests = d.held_requests.checked_add(requests).ok_or(ProgramError::ArithmeticOverflow)?;
    check(d.held_requests <= 16, 860)?;
    backed(a, &d, t)?;
    write(a, &d)
}

pub fn contributors(t: &Table) -> Result<[Pubkey; rules::SEATS], ProgramError> {
    let mut wallets = [Pubkey::default(); rules::SEATS];
    let shared = matches!(t.round.draw, Draw::Initial | Draw::Dealer);
    if !shared { check((t.round.actor as usize) < rules::SEATS, 860)?; }
    for (i, wallet) in wallets.iter_mut().enumerate() {
        if t.round.players[i].original > 0 && (shared || t.round.actor as usize == i) {
            *wallet = t.round_wallets[i];
            check(*wallet != Pubkey::default(), 860)?;
        }
    }
    validate_wallets(&wallets)?;
    Ok(wallets)
}
fn validate_wallets(wallets: &[Pubkey; rules::SEATS]) -> Result<usize, ProgramError> {
    let mut count = 0;
    for (i, w) in wallets.iter().enumerate() {
        if *w == Pubkey::default() { continue; }
        check(!wallets[..i].contains(w), 860)?;
        count += 1;
    }
    check(count > 0, 860)?;
    Ok(count)
}
fn wallet_bytes(wallets: &[Pubkey; rules::SEATS]) -> Vec<u8> {
    wallets.iter().flat_map(|w| w.to_bytes()).collect()
}
pub fn draw_payer(program: &Pubkey, table: &Pubkey, seed: &[u8; 32], wallets: &[Pubkey; rules::SEATS]) -> (Pubkey, u8) {
    let group = hashv(&[&wallet_bytes(wallets)]).to_bytes();
    Pubkey::find_program_address(&[PAYER_SEED, table.as_ref(), seed, &group], program)
}
pub fn client(program: &Pubkey, table: &Pubkey, t: &Table, seed: &[u8; 32]) -> Result<Pubkey, ProgramError> {
    Ok(if t.version == 1 { oracle_payer(program, table).0 }
        else { draw_payer(program, table, seed, &contributors(t)?).0 })
}

/// Base thirteen request accounts followed by one deposit for each nonempty
/// contributor, in seat order. No house reserve is accepted in this instruction.
pub fn request<'a>(program: &Pubkey, a: &[AccountInfo<'a>], t: &Table, seed: &[u8; 32]) -> ProgramResult {
    let wallets = contributors(t)?;
    let count = validate_wallets(&wallets)?;
    check(a.len() == 13 + count, 9)?;
    let (payer, bump) = draw_payer(program, a[1].key, seed, &wallets);
    check(*a[7].key == payer && a[7].owner == &system_program::id() && a[7].data_is_empty(), 860)?;
    let fee = orao::request_fee(&a[8])?;
    let cost = add(fee, Rent::get()?.minimum_balance(PENDING_LEN))?;
    let mut position = 0;
    for wallet in wallets.iter().filter(|w| **w != Pubkey::default()) {
        let account = &a[13 + position];
        let mut d = load(program, account, a[1].key, wallet)?;
        check(d.round == t.round.id && d.held_requests > 0 && fee <= d.max_oracle_fee, 862)?;
        d.held_requests -= 1;
        let charge = crate::funding::share(cost, count as u32, position as u32)?;
        move_player_sol(account, &a[7], &d, charge)?;
        backed(account, &d, t)?;
        write(account, &d)?;
        position += 1;
    }
    let group = hashv(&[&wallet_bytes(&wallets)]).to_bytes();
    orao::request_fresh_signed_many(&a[7], &a[8], &a[9], &a[10], &a[11], &a[12], seed,
        Some(&[PAYER_SEED, a[1].key.as_ref(), seed, &group, &[bump]]), &a[13..])?;
    // This descriptor remains sufficient for native refund recovery after all
    // our funding accounts close. ORAO's immutable client anchors its recipients.
    solana_program::log::sol_log_data(&[b"SDICE_BLACKJACK_SOL_V1", a[1].key.as_ref(), seed, &wallet_bytes(&wallets), &cost.to_le_bytes()]);
    Ok(())
}

/// Standard MagicBlock takes only its fee. No per-request rent or third-party
/// refund claim exists; the exact lamports are split among the original bettors.
pub(super) fn charge_magicblock(program: &Pubkey, table: &Pubkey, t: &Table,
    payer: &AccountInfo, deposits: &[AccountInfo], cost: u64) -> ProgramResult {
    check(t.version >= 3 && *payer.key == super::magicblock::payer(program, table).0 && cost == super::magicblock::FEE, 860)?;
    let wallets = contributors(t)?; let count = validate_wallets(&wallets)?;
    check(deposits.len() == count, 9)?;
    for (position, wallet) in wallets.iter().filter(|w| **w != Pubkey::default()).enumerate() {
        let a = &deposits[position]; let mut d = load(program, a, table, wallet)?;
        check(d.round == t.round.id && d.held_requests > 0 && cost <= d.max_oracle_fee, 862)?;
        d.held_requests -= 1;
        let charge = crate::funding::share(cost, count as u32, position as u32)?;
        move_player_sol(a, payer, &d, charge)?; backed(a, &d, t)?; write(a, &d)?;
        msg!("SDICE_BLACKJACK_VRF_CHARGE wallet={} lamports={} round={} draw={}", wallet, charge, t.round.id, t.round.draw_nonce);
    }
    Ok(())
}

pub fn process(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let tag = *data.first().ok_or(ProgramError::InvalidInstructionData)?;
    if tag == 98 { return claim(program, a, data); }
    check(a.len() >= 5, 9)?; signer(&a[0])?;
    let bank = load_bank(program, &a[2])?;
    let mut t = load_table(program, &a[1], a[2].key)?;
    if tag == 95 {
        check(a.len() == 5 && data.len() == 9 && t.version == 1 && !t.enabled && bank.version == 4
            && (!t.round.active() || (t.round.stage == Stage::Betting && t.round.betting_players() == 0))
            && t.seats.iter().all(|s| s.wallet == Pubkey::default())
            && bank.authority == *a[0].key, 863)?;
        reserve_valid(program, &a[3], a[1].key)?;
        check(*a[4].key == system_program::id(), 860)?;
        invoke(&system_instruction::transfer(a[0].key, a[3].key, u64_at(data, 1)?),
            &[a[0].clone(), a[3].clone(), a[4].clone()])?;
        t.version = 2; t.changed()?;
        return write(&a[1], &*t);
    }
    check((2..=4).contains(&t.version), 863)?;
    if tag == 99 {
        check(a.len() == 5 && data.len() == 9 && bank.authority == *a[0].key, 11)?;
        return reserve_transfer(program, a[1].key, &a[3], &a[0], &a[4], u64_at(data, 1)?);
    }
    check(a.len() == 7, 9)?;
    reserve_valid(program, &a[4], a[1].key)?;
    check(*a[5].key == system_program::id(), 860)?;
    if tag == 96 {
        check(data.len() == 17 && t.seats.iter().any(|s| s.wallet == *a[0].key), 822)?;
        let amount = u64_at(data, 1)?; let cap = u64_at(data, 9)?;
        check(amount > 0 && cap <= FEE_CAP && fee(&t, &a[6])? <= cap, 862)?;
        let (key, bump) = deposit_address(program, a[1].key, a[0].key);
        check(*a[3].key == key, 860)?;
        let mut d = if a[3].owner == program { load(program, &a[3], a[1].key, a[0].key)? }
        else {
            check(a[3].owner == &system_program::id() && a[3].data_is_empty(), 860)?;
            let loan = Rent::get()?.minimum_balance(LEN).saturating_sub(a[3].lamports());
            let before = a[0].lamports();
            reserve_transfer(program, a[1].key, &a[4], &a[0], &a[5], loan)?;
            create_pda(&a[0], &a[3], &a[5], program, LEN,
                &[ESCROW_SEED, a[1].key.as_ref(), a[0].key.as_ref(), &[bump]])?;
            check(a[0].lamports() == before, 860)?;
            Deposit { magic: MAGIC, version: 1, table: *a[1].key, wallet: *a[0].key,
                house_rent: loan, round: t.round.id, held_requests: 0, max_oracle_fee: cap, bump }
        };
        normalize(&mut d, &t)?;
        check(d.held_requests == 0 || cap >= d.max_oracle_fee, 862)?;
        d.max_oracle_fee = cap;
        invoke(&system_instruction::transfer(a[0].key, a[3].key, amount),
            &[a[0].clone(), a[3].clone(), a[5].clone()])?;
        backed(&a[3], &d, &t)?;
        return write(&a[3], &d);
    }
    check(tag == 97 && data.len() == 9, 9)?;
    let mut d = load(program, &a[3], a[1].key, a[6].key)?;
    normalize(&mut d, &t)?;
    let amount = u64_at(data, 1)?;
    // A third party may clean up only after the original wallet has left.
    // While seated, only that wallet may change its spendable SOL balance.
    check(*a[0].key == d.wallet || (amount == 0 && !t.seats.iter().any(|s| s.wallet == d.wallet)), 822)?;
    if amount > 0 {
        move_player_sol(&a[3], &a[6], &d, amount)?;
        backed(&a[3], &d, &t)?;
        return write(&a[3], &d);
    }
    close_deposit(&a[3], &a[4], &a[6], &d)
}
fn load_table(program: &Pubkey, account: &AccountInfo, bank: &Pubkey) -> Result<Box<Table>, ProgramError> {
    super::state::load(program, account, bank)
}

/// No house-funded receipt has to stay alive for a late ORAO refund. The supplied
/// descriptor must derive the *actual ORAO client*, whose signature only our
/// native request path can produce. A replay only drains newly arrived SOL,
/// always to the same original wallets (or their verified deposit accounts).
fn claim<'a>(program: &Pubkey, a: &[AccountInfo<'a>], data: &[u8]) -> ProgramResult {
    check(data.len() == 385 && a.len() >= 5, 9)?; signer(&a[0])?;
    let table = Pubkey::new_from_array(bytes32(data, 1)?);
    let seed = bytes32(data, 33)?;
    let mut wallets = [Pubkey::default(); rules::SEATS];
    for (i, wallet) in wallets.iter_mut().enumerate() { *wallet = Pubkey::new_from_array(bytes32(data, 65 + i * 32)?); }
    let count = validate_wallets(&wallets)?;
    check(a.len() == 4 + count && *a[3].key == system_program::id(), 9)?;
    let (payer, bump) = draw_payer(program, &table, &seed, &wallets);
    check(*a[1].key == payer && a[1].owner == &system_program::id() && a[1].data_is_empty(), 860)?;
    check(orao::read_request(&a[2], &seed, &payer)?.is_some(), 833)?;
    let amount = a[1].lamports();
    let group = hashv(&[&wallet_bytes(&wallets)]).to_bytes();
    for (position, wallet) in wallets.iter().filter(|w| **w != Pubkey::default()).enumerate() {
        let target = &a[4 + position];
        if *target.key != *wallet { load(program, target, &table, wallet)?; }
        check(target.key != a[1].key, 860)?;
        let share = crate::funding::share(amount, count as u32, position as u32)?;
        if share > 0 { invoke_signed(&system_instruction::transfer(a[1].key, target.key, share),
            &[a[1].clone(), target.clone(), a[3].clone()], &[&[PAYER_SEED, table.as_ref(), &seed, &group, &[bump]]])?; }
    }
    msg!("SDICE_BLACKJACK_ORACLE_RETURN payer={} amount={}", payer, amount);
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn layout_and_refund_identity() {
        let d = Deposit { magic: MAGIC, version: 1, table: Pubkey::new_unique(), wallet: Pubkey::new_unique(), house_rent: 1,
            round: 1, held_requests: 2, max_oracle_fee: 500_000, bump: 1 };
        assert_eq!(borsh::to_vec(&d).unwrap().len(), LEN);
        let program = Pubkey::new_unique(); let seed = [1; 32];
        let mut wallets = [Pubkey::default(); rules::SEATS]; wallets[0] = d.wallet;
        let original = draw_payer(&program, &d.table, &seed, &wallets).0;
        wallets[1] = wallets[0]; assert!(validate_wallets(&wallets).is_err());
        wallets[1] = Pubkey::new_unique(); assert_ne!(original, draw_payer(&program, &d.table, &seed, &wallets).0);
        wallets.swap(0, 1); assert_ne!(original, draw_payer(&program, &d.table, &seed, &wallets).0);
        assert_ne!(original, draw_payer(&program, &d.table, &[2; 32], &wallets).0);
        assert_ne!(original, reserve_address(&program, &d.table).0);
        assert_ne!(original, deposit_address(&program, &d.table, &d.wallet).0);
    }
}

pub(super) fn network_debit(p:&Pubkey,table:&Pubkey,t:&Table,a:&AccountInfo,to:&AccountInfo,wallet:&Pubkey,amount:u64)->ProgramResult {
    let d=load(p,a,table,wallet)?;
    move_player_sol(a,to,&d,amount)?;
    backed(a,&d,t)
}
