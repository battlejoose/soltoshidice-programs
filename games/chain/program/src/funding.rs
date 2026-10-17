//! Player-funded execution. The signed bet deposits a fixed, bounded amount;
//! only successful state transitions earn fixed keeper payments. Unused SOL,
//! including these temporary accounts' rent, returns to the original bettor.
use super::*;
use solana_program::instruction::{AccountMeta, Instruction};

pub const DEPOSIT: u64 = 10_000_000;
pub const WORK_FEE: u64 = 5_000;
pub const SHARED_CAP: u64 = 7_000_000;
pub const BET_CAP: u64 = DEPOSIT - SHARED_CAP;
pub const ROUND_LEN: usize = 93;
pub const CONTRIBUTION_LEN: usize = 115;
const ATA: Pubkey = solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");

#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct RoundFunding {
    pub version: u8, pub bank: Pubkey, pub round: Pubkey,
    pub deposits: u32, pub closed: u32, pub oracle_spent: u64, pub work_spent: u64,
    pub requested: bool, pub oracle_final: bool, pub advance_paid: bool, pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Debug)]
pub struct Contribution {
    pub version: u8, pub funding: Pubkey, pub bet: Pubkey, pub player: Pubkey,
    pub spent: u64, pub returned: u64, pub refund_calls: u8, pub bump: u8,
}
pub fn round_address(program: &Pubkey, round: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"sol-funding", round.as_ref()], program)
}
pub fn contribution_address(program: &Pubkey, bet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"sol-deposit", bet.as_ref()], program)
}
pub fn payer_address(program: &Pubkey, round: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"round-sol", round.as_ref()], program)
}
pub fn rent_reserve_address(program: &Pubkey, bank: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"rent-reserve", bank.as_ref()], program)
}
fn valid_rent_reserve(program: &Pubkey, reserve: &AccountInfo, bank: &Pubkey) -> ProgramResult {
    check(*reserve.key == rent_reserve_address(program, bank).0 && reserve.owner == &system_program::id() && reserve.data_is_empty(), 83)
}
fn rent_transfer<'a>(program: &Pubkey, bank: &Pubkey, reserve: &AccountInfo<'a>, destination: &AccountInfo<'a>, system: &AccountInfo<'a>, amount: u64) -> ProgramResult {
    valid_rent_reserve(program, reserve, bank)?;
    check(*system.key == system_program::id() && reserve.key != destination.key, 83)?;
    let bump = rent_reserve_address(program, bank).1;
    invoke_signed(&system_instruction::transfer(reserve.key, destination.key, amount), &[reserve.clone(), destination.clone(), system.clone()],
        &[&[b"rent-reserve", bank.as_ref(), &[bump]]])
}
fn close_to(account: &AccountInfo, destination: &AccountInfo) -> ProgramResult {
    move_sol(account, destination, account.lamports(), true)?;
    account.try_borrow_mut_data()?.fill(0);
    Ok(())
}
fn plus(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn minus(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_sub(b).ok_or(ProgramError::InsufficientFunds) }
fn move_sol(from: &AccountInfo, to: &AccountInfo, amount: u64, close: bool) -> ProgramResult {
    check(from.key != to.key, 80)?;
    let left = minus(from.lamports(), amount)?;
    check(close || left >= Rent::get()?.minimum_balance(from.data_len()), 81)?;
    **to.try_borrow_mut_lamports()? = plus(to.lamports(), amount)?;
    **from.try_borrow_mut_lamports()? = left;
    Ok(())
}
fn load_round(program: &Pubkey, account: &AccountInfo, bank: &Pubkey, round: &Pubkey) -> Result<RoundFunding, ProgramError> {
    check(account.owner == program, 80)?;
    let r = RoundFunding::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = round_address(program, round);
    check(*account.key == expected && (1..=2).contains(&r.version) && r.bump == bump && r.bank == *bank && r.round == *round
        && r.closed <= r.deposits && plus(r.oracle_spent, r.work_spent)? <= SHARED_CAP, 80)?;
    Ok(r)
}
fn load_contribution(program: &Pubkey, account: &AccountInfo, funding: &Pubkey, bet: &Pubkey, player: &Pubkey) -> Result<Contribution, ProgramError> {
    check(account.owner == program, 80)?;
    let c = Contribution::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = contribution_address(program, bet);
    check(*account.key == expected && (1..=2).contains(&c.version) && c.bump == bump && c.funding == *funding && c.bet == *bet
        && c.player == *player && c.spent <= BET_CAP && c.returned <= DEPOSIT && c.refund_calls <= 2, 80)?;
    Ok(c)
}
fn charge_shared(r: &mut RoundFunding, funding: &AccountInfo, actor: &AccountInfo) -> ProgramResult {
    r.work_spent = plus(r.work_spent, WORK_FEE)?;
    check(plus(r.oracle_spent, r.work_spent)? <= SHARED_CAP && r.work_spent <= 3 * WORK_FEE, 81)?;
    move_sol(funding, actor, WORK_FEE, false)
}
fn charge_bet(c: &mut Contribution, funding: &AccountInfo, actor: &AccountInfo, amount: u64) -> ProgramResult {
    c.spent = plus(c.spent, amount)?;
    check(c.spent <= BET_CAP, 81)?;
    move_sol(funding, actor, amount, false)
}
pub fn share(total: u64, count: u32, position: u32) -> Result<u64, ProgramError> {
    check(count > 0 && position < count, 80)?;
    Ok(total / u64::from(count) + u64::from(u64::from(position) < total % u64::from(count)))
}
fn sweep<'a>(program: &Pubkey, round: &Pubkey, payer: &AccountInfo<'a>, funding: &AccountInfo<'a>, system: &AccountInfo<'a>) -> Result<u64, ProgramError> {
    let (expected, bump) = payer_address(program, round);
    check(*payer.key == expected && payer.owner == &system_program::id() && payer.data_is_empty() && *system.key == system_program::id(), 80)?;
    let amount = payer.lamports();
    if amount > 0 {
        invoke_signed(&system_instruction::transfer(payer.key, funding.key, amount),
            &[payer.clone(), funding.clone(), system.clone()], &[&[b"round-sol", round.as_ref(), &[bump]]])?;
    }
    Ok(amount)
}
fn collect_oracle_refund<'a>(program: &Pubkey, r: &mut RoundFunding, round: &shared::SharedRound,
    payer: &AccountInfo<'a>, funding: &AccountInfo<'a>, request: &AccountInfo<'a>, system: &AccountInfo<'a>) -> ProgramResult {
    if r.requested {
        check(round.request == *request.key, 80)?;
        r.oracle_final = orao::read_request(request, &round.request_seed, payer.key)?.is_some();
    }
    let returned = sweep(program, &r.round, payer, funding, system)?;
    r.oracle_spent = r.oracle_spent.saturating_sub(returned);
    Ok(())
}

// 40 enable on an idle v4 bank; 41 one-approval wager+deposit.
// 42 request, 43 resolve, 44 record, 45 payout, 46 advance, 47 ancestor credits,
// 48 timeout, 49 return unused deposit. Operations 42..49 have nine common metas:
// actor, funding, contribution (or System), bank, round, bet (or System),
// round SOL payer, System, oracle request (or System), then their ordinary metas.
pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let tag = *data.first().ok_or(ProgramError::InvalidInstructionData)?;
    // Explicit opt-in on an idle table. This never changes existing deposits.
    // The authority supplies recyclable SOL capital, not a fee paid by bettors.
    if tag == 50 {
        check(data.len() == 9 && accounts.len() == 5, 9)?;
        let actor = &accounts[0]; signer(actor)?;
        let bank = load_bank(program, &accounts[1])?;
        let mut table = shared::load_table(program, &accounts[2], accounts[1].key)?;
        check(bank.authority == *actor.key && bank.version == 4 && table.version == 2 && table.current == 0 && table.queued == 0, 83)?;
        valid_rent_reserve(program, &accounts[3], accounts[1].key)?;
        check(*accounts[4].key == system_program::id(), 83)?;
        invoke(&system_instruction::transfer(actor.key, accounts[3].key, u64_at(data, 1)?), &[actor.clone(), accounts[3].clone(), accounts[4].clone()])?;
        let rent = Rent::get()?;
        check(accounts[3].lamports() >= plus(plus(rent.minimum_balance(shared::SHARED_ROUND_LEN), rent.minimum_balance(shared::BET_LEN))?, rent.minimum_balance(economy::RECEIPT_LEN))?, 83)?;
        table.version = 3;
        return write(&accounts[2], &table);
    }
    if tag == 53 {
        check(data.len() == 9 && accounts.len() == 4, 9)?;
        signer(&accounts[0])?;
        check(load_bank(program, &accounts[1])?.authority == *accounts[0].key, 11)?;
        valid_rent_reserve(program, &accounts[2], accounts[1].key)?;
        // Only available reserve capital; SOL held by active game accounts is inaccessible.
        return rent_transfer(program, accounts[1].key, &accounts[2], &accounts[0], &accounts[3], u64_at(data, 1)?);
    }
    if tag == 40 {
        check(data.len() == 1 && accounts.len() == 3, 9)?;
        let actor = &accounts[0]; signer(actor)?;
        let bank = load_bank(program, &accounts[1])?;
        let mut table = shared::load_table(program, &accounts[2], accounts[1].key)?;
        check(bank.authority == *actor.key && bank.version == 4 && table.version < 3 && table.current == 0 && table.queued == 0, 80)?;
        table.version = 2;
        return write(&accounts[2], &table);
    }
    if tag == 41 || tag == 51 || tag == 54 { return deposit(program, accounts, data); }
    check(accounts.len() >= 9, 9)?;
    let actor = &accounts[0]; signer(actor)?;
    check(actor.owner == &system_program::id() && actor.data_is_empty(), 80)?;
    let funding = &accounts[1]; let contribution = &accounts[2];
    let bank_account = &accounts[3]; let round_account = &accounts[4]; let bet_account = &accounts[5];
    let payer = &accounts[6]; let system = &accounts[7]; let request = &accounts[8];
    load_bank(program, bank_account)?;
    let before = shared::load_shared(program, round_account, bank_account.key)?;
    let mut r = load_round(program, funding, bank_account.key, round_account.key)?;
    check(r.deposits == before.bet_count && *payer.key == payer_address(program, round_account.key).0 && *system.key == system_program::id(), 80)?;
    let inner = &accounts[9..];
    if tag == 49 || tag == 52 {
        return refund(program, accounts, data, r, before);
    }
    if tag == 42 {
        check(data.len() == 33 && inner.len() == 9 && !r.requested, 80)?;
        check(inner[0].key == payer.key && inner[1].key == bank_account.key && inner[3].key == round_account.key && inner[6].key == request.key, 80)?;
        let network = inner[4].try_borrow_data()?;
        let cost = plus(Rent::get()?.minimum_balance(749), u64_at(&network, 72)?)?;
        drop(network);
        check(cost <= SHARED_CAP - 3 * WORK_FEE, 81)?;
        // Dust sent to the system PDA cannot block a round or become keeper income.
        sweep(program, round_account.key, payer, funding, system)?;
        move_sol(funding, payer, cost, false)?;
        let mut ix = data.to_vec(); ix[0] = 10;
        let mut request_accounts = inner.to_vec(); request_accounts.push(funding.clone());
        shared::process_funded(program, &request_accounts, &ix)?;
        let unused = sweep(program, round_account.key, payer, funding, system)?;
        r.oracle_spent = cost.saturating_sub(unused); r.requested = true; r.oracle_final = false;
        charge_shared(&mut r, funding, actor)?;
    } else if tag == 43 || tag == 48 {
        check(data.len() == 1 && inner.len() == 3 && inner[0].key == bank_account.key && inner[1].key == round_account.key && inner[2].key == request.key, 80)?;
        collect_oracle_refund(program, &mut r, &before, payer, funding, request, system)?;
        shared::process(program, inner, &[if tag == 43 {11} else {15}])?;
        let after = shared::load_shared(program, round_account, bank_account.key)?;
        if before.state != after.state { charge_shared(&mut r, funding, actor)?; }
    } else if tag == 46 {
        check(data.len() == 1 && inner.len() == 4 && inner[0].key == bank_account.key && inner[2].key == round_account.key, 80)?;
        let table_before = shared::load_table(program, &inner[1], bank_account.key)?;
        check(table_before.current == before.nonce, 80)?;
        shared::process(program, inner, &[14])?;
        check(!r.advance_paid, 80)?;
        r.advance_paid = true;
        charge_shared(&mut r, funding, actor)?;
    } else {
        check(data.len() == 1 && matches!(tag, 44 | 45 | 47), 9)?;
        let bet_before = shared::load_bet(program, bet_account, round_account.key)?;
        let mut c = load_contribution(program, contribution, funding.key, bet_account.key, &bet_before.player)?;
        check(c.version == r.version, 80)?;
        if tag == 44 || tag == 45 {
            let count = if tag == 44 {3} else {12};
            check(inner.len() == count + if tag == 45 {2} else {0}, 9)?;
            check(inner[0].key == bank_account.key && inner[1].key == round_account.key && inner[2].key == bet_account.key, 80)?;
            if tag == 45 && !bet_before.claimed {
                collect_oracle_refund(program, &mut r, &before, payer, funding, request, system)?;
                create_payout_ata(program, &r.round, funding, payer, system, &inner[3], &inner[5], &inner[6], &inner[12], &inner[13], &bet_before.player, &mut c)?;
            }
            shared::process(program, &inner[..count], &[if tag == 44 {12} else {13}])?;
            let after = shared::load_bet(program, bet_account, round_account.key)?;
            if (tag == 44 && !bet_before.settled && after.settled) || (tag == 45 && !bet_before.claimed && after.claimed) {
                charge_bet(&mut c, funding, actor, WORK_FEE)?;
            }
        } else {
            check(inner.len() >= 4 && inner.len() <= 11 && inner[0].key == bank_account.key, 9)?;
            let receipt_before = economy::load_receipt(program, &inner[2], bank_account.key, bet_account.key, &bet_before.player)?;
            economy::process(program, inner, &[25])?;
            let after = economy::load_receipt(program, &inner[2], bank_account.key, bet_account.key, &bet_before.player)?;
            if after.remaining < receipt_before.remaining { charge_bet(&mut c, funding, actor, WORK_FEE)?; }
        }
        write(contribution, &c)?;
    }
    write(funding, &r)
}

fn deposit(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let tournament = data[0] == 54;
    let reclaim = data[0] == 51 || tournament;
    check(data.len() == 26 && accounts.len() == if tournament {24} else if reclaim {19} else {18} && data[17] == 4 && u64_at(data, 18)? == DEPOSIT, 80)?;
    let inner = &accounts[..16]; let funding = &accounts[16]; let contribution = &accounts[17];
    let player = &inner[0]; let bank = &inner[1]; let round_account = &inner[3]; let bet_account = &inner[4]; let system = &inner[9];
    let table = shared::load_table(program, &inner[2], bank.key)?;
    check(table.version == if tournament {4} else if reclaim {3} else {2} && load_bank(program, bank)?.version == 4, 80)?;
    let player_before = player.lamports();
    if reclaim {
        signer(player)?;
        valid_rent_reserve(program, &accounts[18], bank.key)?;
        let mut advance = 0u64;
        for (account, size) in [(round_account, shared::SHARED_ROUND_LEN), (bet_account, shared::BET_LEN), (&inner[13], economy::RECEIPT_LEN)] {
            if account.owner != program {
                check(account.owner == &system_program::id() && account.data_is_empty(), 83)?;
                advance = plus(advance, Rent::get()?.minimum_balance(size).saturating_sub(account.lamports()))?;
            }
        }
        // Front rent before the CPIs so a wallet needs only its deposit + network fee.
        // All addresses and creates are checked below; a failure rolls this advance back.
        rent_transfer(program, bank.key, &accounts[18], player, system, advance)?;
    }
    let mut ix = data[..18].to_vec(); ix[0] = 9;
    shared::process_funded(program, inner, &ix)?;
    if reclaim { check(player.lamports() == player_before, 83)?; }
    let round = shared::load_shared(program, round_account, bank.key)?;
    let (expected, bump) = round_address(program, round_account.key);
    check(*funding.key == expected, 80)?;
    let mut rent_paid = 0;
    let mut r = if funding.owner != program {
        check(round.bet_count == 1, 80)?;
        let before = player.lamports();
        create_pda(player, funding, system, program, ROUND_LEN, &[b"sol-funding", round_account.key.as_ref(), &[bump]])?;
        rent_paid = before - player.lamports();
        RoundFunding { version: if reclaim {2} else {1}, bank: *bank.key, round: *round_account.key, deposits: 0, closed: 0,
            oracle_spent: 0, work_spent: 0, requested: false, oracle_final: true, advance_paid: false, bump }
    } else { load_round(program, funding, bank.key, round_account.key)? };
    check(r.version == if reclaim {2} else {1}, 80)?;
    check(r.deposits + 1 == round.bet_count && !r.requested && r.closed == 0, 80)?;
    let (expected, bump) = contribution_address(program, bet_account.key);
    check(*contribution.key == expected, 80)?;
    let before = player.lamports();
    create_pda(player, contribution, system, program, CONTRIBUTION_LEN, &[b"sol-deposit", bet_account.key.as_ref(), &[bump]])?;
    rent_paid = plus(rent_paid, before - player.lamports())?;
    let transfer_amount = minus(DEPOSIT, rent_paid)?;
    invoke(&system_instruction::transfer(player.key, funding.key, transfer_amount), &[player.clone(), funding.clone(), system.clone()])?;
    r.deposits += 1;
    write(funding, &r)?;
    write(contribution, &Contribution { version: r.version, funding: *funding.key, bet: *bet_account.key, player: *player.key, spent: 0, returned: 0, refund_calls: 0, bump })?;
    if tournament { super::tournament::enroll(program, bank, round_account, bet_account, system, &accounts[19..])?; }
    Ok(())
}

fn create_payout_ata<'a>(program: &Pubkey, round: &Pubkey, funding: &AccountInfo<'a>, payer: &AccountInfo<'a>, system: &AccountInfo<'a>,
    mint: &AccountInfo<'a>, destination: &AccountInfo<'a>, token: &AccountInfo<'a>, wallet: &AccountInfo<'a>, ata_program: &AccountInfo<'a>,
    player: &Pubkey, c: &mut Contribution) -> ProgramResult {
    check(*wallet.key == *player && *ata_program.key == ATA && ata_program.executable && *token.key == spl_token_2022::id(), 80)?;
    let expected = Pubkey::find_program_address(&[player.as_ref(), token.key.as_ref(), mint.key.as_ref()], &ATA).0;
    check(*destination.key == expected, 80)?;
    if destination.owner == token.key { return Ok(()); }
    check(destination.owner == &system_program::id() && destination.data_is_empty(), 80)?;
    // Plain Token-2022 ATAs have the ImmutableOwner extension (170 bytes).
    valid_mint(mint)?;
    let cost = Rent::get()?.minimum_balance(170).saturating_sub(destination.lamports());
    check(plus(c.spent, cost)? <= BET_CAP - 2 * WORK_FEE, 81)?;
    move_sol(funding, payer, cost, false)?;
    let ix = Instruction { program_id: ATA, data: vec![1], accounts: vec![
        AccountMeta::new(*payer.key, true), AccountMeta::new(*destination.key, false),
        AccountMeta::new_readonly(*player, false), AccountMeta::new_readonly(*mint.key, false),
        AccountMeta::new_readonly(*system.key, false), AccountMeta::new_readonly(*token.key, false), AccountMeta::new(*funding.key, false)] };
    let bump = payer_address(program, round).1;
    invoke_signed(&ix, &[payer.clone(), destination.clone(), wallet.clone(), mint.clone(), system.clone(), token.clone(), ata_program.clone(), funding.clone()],
        &[&[b"round-sol", round.as_ref(), &[bump]]])?;
    let unused = sweep(program, round, payer, funding, system)?;
    c.spent = plus(c.spent, cost.saturating_sub(unused))?;
    Ok(())
}

fn refund(program: &Pubkey, accounts: &[AccountInfo], data: &[u8], mut r: RoundFunding, round: shared::SharedRound) -> ProgramResult {
    let reclaim = r.version == 2;
    check(data.len() == 1 && data[0] == if reclaim {52} else {49} && accounts.len() >= if reclaim {13} else {12}, 9)?;
    // Cleanup evidence must be discoverable as a direct instruction in history.
    // A wrapper CPI must not close accounts whose proof readers cannot locate.
    if reclaim {
        check(solana_program::instruction::get_stack_height()
            == solana_program::instruction::TRANSACTION_LEVEL_STACK_HEIGHT, 84)?;
    }
    let actor = &accounts[0]; let funding = &accounts[1]; let contribution = &accounts[2];
    let bank = &accounts[3]; let round_account = &accounts[4]; let bet_account = &accounts[5];
    let player = &accounts[9]; let table = shared::load_table(program, &accounts[11], bank.key)?;
    check(accounts.len() == if table.version == 4 {17} else if reclaim {13} else {12}, 9)?;
    let bet = shared::load_bet(program, bet_account, round_account.key)?;
    let mut c = load_contribution(program, contribution, funding.key, bet_account.key, &bet.player)?;
    check(c.version == r.version, 80)?;
    if reclaim { valid_rent_reserve(program, &accounts[12], bank.key)?; }
    let receipt = economy::load_receipt(program, &accounts[10], bank.key, bet_account.key, &bet.player)?;
    check(round.state == shared::FINISHED && table.current != round.nonce && table.queued != round.nonce
        && bet.claimed && receipt.complete && *player.key == bet.player, 82)?;
    collect_oracle_refund(program, &mut r, &round, &accounts[6], funding, &accounts[8], &accounts[7])?;
    if table.version == 4 {
        check(reclaim, 80)?;
        super::tournament::before_cleanup(program, bank, round_account, bet_account, &accounts[13..], r.oracle_final)?;
    }
    // An oracle timeout can return the unused portion immediately. Keep the
    // temporary receipts only until a late oracle fulfillment returns its rent.
    let retained = if r.oracle_final {0} else {plus(contribution.lamports(), share(Rent::get()?.minimum_balance(ROUND_LEN), r.deposits, bet.position)?)?};
    let shared = share(plus(r.oracle_spent, r.work_spent)?, r.deposits, bet.position)?;
    let entitlement = minus(minus(minus(DEPOSIT, c.spent)?, shared)?, retained)?;
    let available = entitlement.saturating_sub(c.returned);
    check(available > WORK_FEE && c.refund_calls < 2, 82)?;
    c.spent = plus(c.spent, WORK_FEE)?; check(c.spent <= BET_CAP, 81)?;
    c.refund_calls += 1;
    let returned = available - WORK_FEE;
    c.returned = plus(c.returned, returned)?;
    // Move receipt rent back into the pool before its final distribution.
    if r.oracle_final {
        if reclaim {
            // Permanent transaction logs preserve the exact native outcome/proof after
            // account closure. The table's monotonic nonce prevents old bets being recreated.
            solana_program::log::sol_log_data(&[b"SDICE_FINAL_BET_V1", &round_account.try_borrow_data()?, &bet_account.try_borrow_data()?]);
            close_to(bet_account, &accounts[12])?;
            close_to(&accounts[10], &accounts[12])?;
        }
        move_sol(contribution, funding, contribution.lamports(), true)?;
        contribution.try_borrow_mut_data()?.fill(0);
        r.closed += 1;
    }
    let closing = r.oracle_final && r.closed == r.deposits;
    move_sol(funding, actor, WORK_FEE, closing)?;
    move_sol(funding, player, returned, closing)?;
    msg!("SDICE_SOL_RETURN player={} deposited={} processing={} returned={}", player.key, DEPOSIT, plus(c.spent, shared)?, c.returned);
    if closing {
        if reclaim { close_to(round_account, &accounts[12])?; }
        // Only unsolicited donations/rounding dust can remain; never a keeper withdrawal.
        move_sol(funding, player, funding.lamports(), true)?;
        funding.try_borrow_mut_data()?.fill(0);
    } else { write(funding, &r)?; }
    if !r.oracle_final { write(contribution, &c)?; }
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn exact_sharing() {
        for count in [1, 2, 3, 10, 100] { for total in [0, 1, 17, 2_178_245, SHARED_CAP] {
            assert_eq!((0..count).map(|p| share(total, count, p).unwrap()).sum::<u64>(), total);
        }}
        assert!(share(1, 0, 0).is_err()); assert!(share(1, 3, 3).is_err());
    }
    #[test] fn layout_and_single_bettor_cap() {
        let r = RoundFunding {version:1,bank:Pubkey::default(),round:Pubkey::default(),deposits:1,closed:0,oracle_spent:0,work_spent:0,requested:false,oracle_final:true,advance_paid:false,bump:0};
        let c = Contribution {version:1,funding:Pubkey::default(),bet:Pubkey::default(),player:Pubkey::default(),spent:0,returned:0,refund_calls:0,bump:0};
        assert_eq!(borsh::to_vec(&r).unwrap().len(), ROUND_LEN);
        assert_eq!(borsh::to_vec(&c).unwrap().len(), CONTRIBUTION_LEN);
        assert_eq!(SHARED_CAP + BET_CAP, DEPOSIT);
        assert!(DEPOSIT > Rent::default().minimum_balance(137) + 500_000 + 6 * WORK_FEE);
    }
}
