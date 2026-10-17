//! Opt-in base-layer VRF; no delegation/rollup, player custody or bank CPI in callbacks.
//! Scoped ABI pinned to magicblock-labs/solana-vrf 52b103c5. Standard queue only.
use super::{rules::Stage, state::*, sol_funding};
use crate::*;
use solana_program::instruction::{AccountMeta, Instruction};

pub const PROGRAM: Pubkey = solana_program::pubkey!("Vrf1RNUjXmQGjmQrQLvJHs9SNkvDJEsRVFPkfSQUwGz");
pub const QUEUE: Pubkey = solana_program::pubkey!("Cuj97ggrhhidhbu39TijNVqE74xvKJ69gDervRUXAxGh");
pub const FEE: u64 = 500_000;
pub const CALLBACK: [u8; 8] = *b"BJMBCB01";
const SEED: &[u8] = b"bj-vrf-inbox-v1";
const PAYER: &[u8] = b"bj-vrf-fee-v1";
const MAGIC: [u8; 8] = *b"BJMBVR01";
pub const LEN: usize = 178;

#[derive(BorshSerialize, BorshDeserialize, Clone)]
pub struct Inbox {
    magic: [u8; 8], table: Pubkey, bank: Pubkey, seed: [u8; 32], random: [u8; 32],
    round: u64, nonce: u64, requested_slot: u64, fulfilled_slot: u64,
    status: u8, house_rent: u64, bump: u8,
}
#[derive(BorshSerialize)] struct Meta { pubkey: [u8; 32], is_signer: bool, is_writable: bool }
#[derive(BorshSerialize)] struct Request {
    caller_seed: [u8; 32], callback_program_id: Pubkey, callback_discriminator: Vec<u8>,
    callback_accounts_metas: Vec<Meta>, callback_args: Vec<u8>,
}
pub fn address(program: &Pubkey, table: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SEED, table.as_ref()], program)
}
pub fn payer(program: &Pubkey, table: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[PAYER, table.as_ref()], program)
}
pub fn fee(queue: &AccountInfo) -> Result<u64, ProgramError> {
    check(*queue.key == QUEUE && queue.owner == &PROGRAM && queue.data_len() == 30_000, 870)?;
    check(queue.try_borrow_data()?[17] == 0, 871)?;
    Ok(FEE)
}
fn load(program: &Pubkey, a: &AccountInfo, table: &Pubkey) -> Result<Inbox, ProgramError> {
    check(a.owner == program && a.data_len() == LEN, 872)?;
    let s = Inbox::try_from_slice(&a.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (key, bump) = address(program, table);
    check(*a.key == key && s.magic == MAGIC && s.table == *table && s.bump == bump
        && s.status <= 2 && a.lamports() >= s.house_rent, 872)?;
    Ok(s)
}
pub fn require_inbox(program: &Pubkey, a: &AccountInfo, table: &Pubkey) -> ProgramResult {
    load(program, a, table).map(|_| ())
}
pub fn result(program: &Pubkey, a: &AccountInfo, table: &Pubkey, t: &Table, seed: &[u8; 32]) -> Result<Option<[u8; 64]>, ProgramError> {
    let s = load(program, a, table)?;
    check(s.bank == t.bank, 872)?;
    if t.round.stage == Stage::NeedRandomness { return Ok(None); }
    check(s.round == t.round.id && s.nonce == t.round.draw_nonce && s.seed == *seed && s.status > 0, 873)?;
    if s.status == 1 { return Ok(None); }
    check(s.fulfilled_slot > s.requested_slot, 873)?;
    // Expand the provider's 32-byte result with domain-separated SHA-256. This
    // preserves the released card/transcript algorithm's 64-byte input shape.
    let mut random = [0; 64];
    for i in 0..2 { random[i*32..(i+1)*32].copy_from_slice(&hashv(&[
        b"sdice:blackjack:magicblock:v1", &s.random, &[i as u8]]).to_bytes()); }
    Ok(Some(random))
}
pub fn receipt(a: &AccountInfo) -> ProgramResult {
    let data = a.try_borrow_data()?;
    solana_program::log::sol_log_data(&[b"SDICE_BLACKJACK_VRF_V1", a.key.as_ref(), &data]);
    Ok(())
}
pub fn callback(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 2 && data.len() == 88 && data.starts_with(&CALLBACK), 9)?;
    let identity = Pubkey::find_program_address(&[b"identity", program.as_ref()], &PROGRAM).0;
    check(*a[0].key == identity && a[0].is_signer, 874)?;
    check(a[1].owner == program && a[1].data_len() == LEN && a[1].is_writable, 872)?;
    let table = Pubkey::new_from_array(bytes32(&a[1].try_borrow_data()?, 8)?);
    let mut s = load(program, &a[1], &table)?;
    check(s.status == 1 && u64_at(data, 40)? == s.round && u64_at(data, 48)? == s.nonce
        && data[56..88] == s.seed, 873)?;
    let slot = Clock::get()?.slot;
    check(slot > s.requested_slot, 873)?;
    s.random.copy_from_slice(&data[8..40]); s.fulfilled_slot = slot; s.status = 2;
    write(&a[1], &s)?;
    receipt(&a[1])
}

/// Common seven game accounts, fee payer, caller identity, queue, reusable
/// inbox, system, slot hashes, VRF program, then contributors in seat order.
pub fn request<'a>(program: &Pubkey, a: &[AccountInfo<'a>], t: &Table, seed: &[u8; 32]) -> ProgramResult {
    check(a.len() >= 15, 9)?;
    let cost = fee(&a[9])?;
    let (pay, pay_bump) = payer(program, a[1].key);
    let (identity, identity_bump) = Pubkey::find_program_address(&[b"identity"], program);
    check(*a[7].key == pay && a[7].owner == &system_program::id() && a[7].data_is_empty()
        && *a[8].key == identity && *a[11].key == system_program::id()
        && *a[12].key == solana_program::sysvar::slot_hashes::id()
        && *a[13].key == PROGRAM && a[13].executable, 870)?;
    let mut s = load(program, &a[10], a[1].key)?;
    check(s.bank == t.bank && !(s.round == t.round.id && s.nonce == t.round.draw_nonce && s.status > 0), 873)?;
    let payer_before = a[7].lamports();
    sol_funding::charge_magicblock(program, a[1].key, t, &a[7], &a[14..], cost)?;
    // Solana must synchronize BOTH sides of direct account-lamport changes at
    // a CPI boundary. MagicBlock accepts exactly five accounts, so it cannot
    // receive the extra deposit accounts as ORAO does. A zero-value system
    // transfer synchronizes the complete balanced set without spending SOL.
    let mut sync = system_instruction::transfer(a[7].key, a[9].key, 0);
    let mut sync_infos = vec![a[7].clone(), a[9].clone(), a[11].clone()];
    for deposit in &a[14..] { sync.accounts.push(AccountMeta::new(*deposit.key, false)); sync_infos.push(deposit.clone()); }
    invoke_signed(&sync, &sync_infos, &[&[PAYER, a[1].key.as_ref(), &[pay_bump]]])?;
    s.seed = *seed; s.random = [0; 32]; s.round = t.round.id; s.nonce = t.round.draw_nonce;
    s.requested_slot = Clock::get()?.slot; s.fulfilled_slot = 0; s.status = 1;
    write(&a[10], &s)?;
    let mut args = s.round.to_le_bytes().to_vec(); args.extend_from_slice(&s.nonce.to_le_bytes()); args.extend_from_slice(seed);
    let req = Request { caller_seed: *seed, callback_program_id: *program,
        callback_discriminator: CALLBACK.to_vec(), callback_accounts_metas: vec![Meta {
            pubkey: a[10].key.to_bytes(), is_signer: false, is_writable: true }], callback_args: args };
    let mut data = vec![0u8; 8]; data[0] = 10; data.extend(borsh::to_vec(&req)?);
    let infos = vec![a[7].clone(), a[8].clone(), a[9].clone(), a[11].clone(), a[12].clone(), a[13].clone()];
    invoke_signed(&Instruction { program_id: PROGRAM, accounts: vec![
        AccountMeta::new(pay, true), AccountMeta::new_readonly(identity, true), AccountMeta::new(QUEUE, false),
        AccountMeta::new_readonly(system_program::id(), false), AccountMeta::new_readonly(solana_program::sysvar::slot_hashes::id(), false)], data },
        &infos,
        &[&[PAYER, a[1].key.as_ref(), &[pay_bump]], &[b"identity", &[identity_bump]]])?;
    // An upstream fee change must fail atomically, never consume reserve/dust.
    check(a[7].lamports() == payer_before, 875)?;
    msg!("SDICE_BLACKJACK_MAGICBLOCK_REQUEST table={} round={} draw={} fee={}", a[1].key, s.round, s.nonce, cost);
    Ok(())
}

/// Authority-only activation/close on a paused empty table; no token transfers.
pub fn configure(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 6 && data.len() == 9 && (data[0] == 100 || data[0] == 101), 9)?;
    signer(&a[0])?; let bank = load_bank(program, &a[2])?;
    let mut t = super::state::load(program, &a[1], a[2].key)?;
    check(bank.authority == *a[0].key && bank.version == 4 && !t.enabled
        && (!t.round.active() || (t.round.stage == Stage::Betting && t.round.betting_players() == 0))
        && t.seats.iter().all(|s| s.wallet == Pubkey::default()), 863)?;
    sol_funding::reserve_valid(program, &a[3], a[1].key)?;
    check(*a[4].key == system_program::id(), 870)?;
    if data[0] == 101 {
        check(t.version == 3 && u64_at(data, 1)? == 0, 863)?;
        let s = load(program, &a[5], a[1].key)?;
        **a[3].try_borrow_mut_lamports()? = a[3].lamports().checked_add(a[5].lamports()).ok_or(ProgramError::ArithmeticOverflow)?;
        **a[5].try_borrow_mut_lamports()? = 0; a[5].try_borrow_mut_data()?.fill(0);
        msg!("SDICE_BLACKJACK_MAGICBLOCK_RENT_RETURN amount={}", s.house_rent);
        return Ok(());
    }
    let amount = u64_at(data, 1)?;
    if amount > 0 { invoke(&system_instruction::transfer(a[0].key, a[3].key, amount), &[a[0].clone(), a[3].clone(), a[4].clone()])?; }
    let (key, bump) = address(program, a[1].key);
    check(*a[5].key == key && a[5].owner == &system_program::id() && a[5].data_is_empty(), 872)?;
    let loan = Rent::get()?.minimum_balance(LEN).saturating_sub(a[5].lamports());
    let before = a[0].lamports();
    sol_funding::reserve_transfer(program, a[1].key, &a[3], &a[0], &a[4], loan)?;
    create_pda(&a[0], &a[5], &a[4], program, LEN, &[SEED, a[1].key.as_ref(), &[bump]])?;
    check(a[0].lamports() == before, 872)?;
    write(&a[5], &Inbox { magic: MAGIC, table: *a[1].key, bank: *a[2].key, seed: [0;32], random: [0;32],
        round: 0, nonce: 0, requested_slot: 0, fulfilled_slot: 0, status: 0, house_rent: loan, bump })?;
    t.version = 3; t.changed()?; write(&a[1], &*t)
}
