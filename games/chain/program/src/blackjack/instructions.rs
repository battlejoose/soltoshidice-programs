//! Tags 80..94 are isolated from all existing dice/bank/reward instructions.
//! Common accounts: caller(s), table(w), bank, mint, bank vault, escrow, Token-2022.
//! Money-moving instructions require bank/vault/escrow writable; other operations
//! only write the table. The caller pays transaction fees, never chooses cards.
use super::{rules::{self, Stage, Action}, state::*};
use crate::*;

fn amount_add(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn amount_sub(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_sub(b).ok_or(ProgramError::InsufficientFunds) }
fn seat(data: &[u8]) -> Result<usize, ProgramError> {
    let i = *data.get(17).ok_or(ProgramError::InvalidInstructionData)? as usize;
    check(i < rules::SEATS, 822)?; Ok(i)
}
fn session(t: &mut Table, i: usize, data: &[u8], offset: usize, now: i64) -> ProgramResult {
    let key = Pubkey::new_from_array(bytes32(data, offset)?);
    let seconds = u64_at(data, offset + 32)?;
    let allowance = u64_at(data, offset + 40)?;
    let max = u64_at(data, offset + 48)?;
    check(key != Pubkey::default() && seconds > 0 && seconds <= SESSION_SECONDS as u64
        && max >= t.min_wager && max <= t.max_wager && allowance >= max, 825)?;
    let s = &mut t.seats[i];
    s.session = key; s.expires = now.checked_add(seconds as i64).ok_or(ProgramError::ArithmeticOverflow)?;
    s.allowance = allowance; s.max_wager = max; s.last_action = now; Ok(())
}

#[inline(never)]
fn initialize(program: &Pubkey, a: &[AccountInfo], data: &[u8], bank: &Bank) -> ProgramResult {
    check(data.len() == 25 && a.len() == 8, 9)?;
    check(*a[0].key == bank.authority, 11)?;
    let id = u64_at(data, 1)?; let min = u64_at(data, 9)?; let max = u64_at(data, 17)?;
    check(min > 0 && min % 2 == 0 && max >= min && max % 2 == 0 && max <= bank.max_wager && max <= rules::MAX_BET, 804)?;
    let (key, bump) = address(program, a[2].key, id);
    check(*a[1].key == key && *a[5].key == token_address(a[3].key, &key), 820)?;
    let t = Table::new(*a[2].key, *a[5].key, id, bump, min, max);
    create_pda(&a[0], &a[1], &a[7], program, TABLE_LEN,
        &[TABLE_SEED, a[2].key.as_ref(), &id.to_le_bytes(), &[bump]])?;
    write(&a[1], &t)
}

/// Adjust ONLY this table's liability. Player credits live in escrow, not in
/// bank equity. Round completion moves the exact native gross return to escrow.
#[inline(never)]
fn reconcile<'a>(a: &[AccountInfo<'a>], bank: &mut Bank, t: &Table,
    old_reserved: u64, old_stakes: u64, was_active: bool) -> ProgramResult {
    let new_reserved = t.round.reserved()?;
    let incoming = if was_active && t.round.active() { amount_sub(t.round.stakes()?, old_stakes)? } else { 0 };
    let outgoing = if was_active && !t.round.active() {
        t.round.players.iter().try_fold(0, |n, p| amount_add(n, p.returned))?
    } else { 0 };
    let reserved = amount_add(amount_sub(bank.reserved, old_reserved)?, new_reserved)?;
    let vault_before = token_account(&a[4], a[3].key, a[2].key)?;
    let vault_after = amount_sub(amount_add(vault_before, incoming)?, outgoing)?;
    check(vault_after >= reserved, 826)?;
    if incoming > 0 {
        transfer(&a[5], &a[4], &a[3], &a[1], &a[6], incoming, 6,
            Some(&[TABLE_SEED, t.bank.as_ref(), &t.id.to_le_bytes(), &[t.bump]]))?;
    }
    if outgoing > 0 {
        transfer(&a[4], &a[5], &a[3], &a[2], &a[6], outgoing, 6,
            Some(&[b"bank", bank.authority.as_ref(), bank.mint.as_ref(), &[bank.bump]]))?;
    }
    check(token_account(&a[5], a[3].key, a[1].key)? >= t.round.credits()?, 827)?;
    if reserved != bank.reserved { bank.reserved = reserved; write(&a[2], bank)?; }
    Ok(())
}

pub fn process(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() >= 7, 9)?; signer(&a[0])?;
    let mut bank = load_bank(program, &a[2])?;
    check(bank.version == 4 && *a[3].key == bank.mint && *a[4].key == bank.vault, 14)?;
    check(*a[6].key == spl_token_2022::id() && valid_mint(&a[3])? == 6, 4)?;
    token_account(&a[4], a[3].key, a[2].key)?;
    token_account(&a[5], a[3].key, a[1].key)?;
    let tag = *data.first().ok_or(ProgramError::InvalidInstructionData)?;
    if tag == 80 { return initialize(program, a, data, &bank); }
    let mut t = load(program, &a[1], a[2].key)?;
    check(*a[5].key == t.escrow, 820)?;
    if tag == 85 {
        // Independent bets can be signed against the same table observation.
        // The round is still pinned; bet() enforces the open deadline, one bet
        // per seat, current credit and amount. All turn decisions stay exact.
        check(u64_at(data, 1)? == t.round.id && u64_at(data, 9)? <= t.round.revision, 821)?;
    } else if tag == 84 {
        // Wallet approval may remain open while other seats and the keeper play.
        // Renewal is pinned to the wallet's previous session below, not to
        // unrelated table revisions. No card, wager, balance or turn changes.
        let observed_round = u64_at(data, 1)?;
        check(observed_round <= t.round.id
            && (observed_round < t.round.id || u64_at(data, 9)? <= t.round.revision), 821)?;
    } else { t.guard(data)?; }
    let now = Clock::get()?.unix_timestamp;
    let old_reserved = t.round.reserved()?; let old_stakes = t.round.stakes()?; let was_active = t.round.active();
    match tag {
        81 => { // Join: round/revision, seat, chips, scoped session/seconds/budget/max.
            check(data.len() == 82 && a.len() == 9, 9)?; let i = seat(data)?;
            check(t.enabled && t.seats[i].wallet == Pubkey::default() && t.idle(i), 828)?;
            check(!t.seats.iter().any(|s| s.wallet == *a[0].key), 829)?;
            economy::load_profile(program, &a[8], a[2].key, a[0].key)?;
            let amount = u64_at(data, 18)?; check(amount >= t.min_wager, 804)?;
            check(t.round.players[i].credit == 0, 827)?;
            token_account(&a[7], a[3].key, a[0].key)?;
            t.seats[i].wallet = *a[0].key; session(&mut t, i, data, 26, now)?;
            t.round.players[i].credit = amount;
            transfer(&a[7], &a[5], &a[3], &a[0], &a[6], amount, 6, None)?; t.changed()?;
        }
        82 => { // Add chips with wallet signature only, including during a round.
            check(data.len() == 26 && a.len() == 8, 9)?; let i = seat(data)?;
            check(t.seats[i].wallet == *a[0].key, 822)?;
            let amount = u64_at(data, 18)?; check(amount > 0, 804)?;
            token_account(&a[7], a[3].key, a[0].key)?;
            t.round.players[i].credit = amount_add(t.round.players[i].credit, amount)?;
            transfer(&a[7], &a[5], &a[3], &a[0], &a[6], amount, 6, None)?;
            t.seats[i].last_action = now; t.changed()?;
        }
        83 | 94 => { // Wallet cash-out or a manually requested, five-minute idle kick.
            check(data.len() == 18 && a.len() == if t.version >= 2 {11} else {8}, 9)?; let i = seat(data)?;
            let s = t.seats[i]; check(s.wallet != Pubkey::default() && t.idle(i), 830)?;
            if tag == 83 { check(*a[0].key == s.wallet, 822)?; }
            else {
                check(t.seats.iter().any(|s| s.wallet == *a[0].key)
                    && *a[0].key != s.wallet && now >= s.last_action.checked_add(IDLE_SECONDS).ok_or(ProgramError::ArithmeticOverflow)?, 831)?;
            }
            check(*a[7].key == token_address(a[3].key, &s.wallet), 827)?;
            token_account(&a[7], a[3].key, &s.wallet)?;
            let chips = t.round.players[i].credit;
            if chips > 0 { transfer(&a[5], &a[7], &a[3], &a[1], &a[6], chips, 6,
                Some(&[TABLE_SEED, t.bank.as_ref(), &t.id.to_le_bytes(), &[t.bump]]))?; }
            t.round.players[i].credit = 0; t.seats[i] = Seat::default(); t.changed()?;
            if t.version >= 2 { super::sol_funding::close_after_leave(program, a[1].key, &t, &s.wallet, &a[8], &a[9], &a[10])?; }
        }
        84 => { // Renew without touching the hand, pending draw or wallet identity.
            check(data.len() == 114 && a.len() == 7, 9)?; let i = seat(data)?;
            check(t.seats[i].wallet == *a[0].key, 822)?;
            check(bytes32(data, 74)? == t.seats[i].session.to_bytes()
                && data[106..114] == t.seats[i].expires.to_le_bytes(), 821)?;
            session(&mut t, i, data, 18, now)?; t.changed()?;
        }
        85 => {
            check(data.len() == 26 && (if t.version==4 {a.len()==20 || a.len()==25} else {a.len()==if t.version>=2 {17} else {15}}) && t.enabled, 9)?; let i = seat(data)?;
            let amount = u64_at(data, 18)?;
            check(amount >= t.min_wager && amount <= t.max_wager, 804)?;
            t.authorize(&a[0], i, now, amount)?;
            t.round.bet(i, amount, now)?; t.round_wallets[i] = t.seats[i].wallet;
            if t.version >= 2 { super::sol_funding::hold(program, &t, a[1].key, &a[15], &a[16], i, 2)?; }
            if t.version==4 {super::network_funding::fund(program,&a[1],&t,&a[0],&a[15],&a[17..],i,super::network_funding::BET_BUDGET)?;}
        }
        86 => { check(data.len() == 17 && a.len() == 7, 9)?; t.round.start(now)?; }
        87 => {
            check(data.len() == 17 && a.len() >= 13, 9)?;
            let seed = t.seed(program, a[1].key);
            t.round.requested(now)?;
            if t.version >= 3 { super::magicblock::request(program, a, &t, &seed)?; }
            else if t.version == 2 { super::sol_funding::request(program, a, &t, &seed)?; }
            else {
                check(a.len() == 13, 9)?;
                let (payer, bump) = oracle_payer(program, a[1].key);
                check(*a[7].key == payer && a[7].owner == &system_program::id() && a[7].data_is_empty(), 832)?;
                orao::request_fresh_signed(&a[7], &a[8], &a[9], &a[10], &a[11], &a[12], &seed,
                    Some(&[ORACLE_SEED, a[1].key.as_ref(), &[bump]]), None)?;
            }
            msg!("SDICE_BLACKJACK_REQUEST table={} round={} draw={} seed={:?}", a[1].key, t.round.id, t.round.draw_nonce, seed);
        }
        88 => {
            check(data.len() == 17 && a.len() == 8, 9)?;
            let seed = t.seed(program, a[1].key);
            let random = if t.version >= 3 {
                let result = super::magicblock::result(program, &a[7], a[1].key, &t, &seed)?.ok_or(ProgramError::Custom(833))?;
                super::magicblock::receipt(&a[7])?; result
            } else {
                let payer = super::sol_funding::client(program, a[1].key, &t, &seed)?;
                orao::read_request(&a[7], &seed, &payer)?.ok_or(ProgramError::Custom(833))?
            };
            t.round.fulfill(&random, &seed, now)?;
        }
        89 => {
            check(data.len() == 19 && (if t.version==4 {a.len()==12} else {a.len()==if t.version>=2 {9} else {7}}), 9)?; let i = seat(data)?;
            let action = match data[18] { 0 => Action::Hit, 1 => Action::Stand, 2 => Action::Double, 3 => Action::Split,
                _ => return Err(ProgramError::InvalidInstructionData) };
            let h = t.round.players[i].hands.get(t.round.hand as usize).ok_or(ProgramError::InvalidAccountData)?;
            let spent = if matches!(action, Action::Double | Action::Split) { h.wager } else { 0 };
            t.authorize(&a[0], i, now, spent)?; t.round.act(i, action, now)?;
            if t.version >= 2 { super::sol_funding::hold(program, &t, a[1].key, &a[7], &a[8], i,
                match action { Action::Stand => 0, Action::Split => 2, _ => 1 })?; }
            if t.version==4 && !matches!(action,Action::Stand) {super::network_funding::fund(program,&a[1],&t,&a[0],&a[7],&a[9..],i,
                super::network_funding::DRAW_BUDGET * if matches!(action,Action::Split){2}else{1})?;}
        }
        90 => { check(data.len() == 17 && a.len() == 7, 9)?; t.round.timeout_turn(now)?; }
        91 => {
            check(data.len() == 17 && a.len() == 8, 9)?;
            let seed = t.seed(program, a[1].key);
            if t.version >= 3 {
                check(super::magicblock::result(program, &a[7], a[1].key, &t, &seed)?.is_none(), 834)?;
            } else if t.round.stage == Stage::WaitingRandomness {
                check(orao::read_request(&a[7], &seed, &super::sol_funding::client(program, a[1].key, &t, &seed)?)?.is_none(), 834)?;
            } else {
                check(t.round.stage == Stage::NeedRandomness && *a[7].key == orao::request_address(&seed)
                    && a[7].owner == &system_program::id() && a[7].data_is_empty(), 834)?;
            }
            t.round.refund_unavailable(now)?;
        }
        92 => {
            check(data.len() == 17 && a.len() == 7, 9)?;
            check(now >= t.finished_at.checked_add(RESULT_SECONDS).ok_or(ProgramError::ArithmeticOverflow)?, 835)?;
            t.round.next_round()?; t.round_wallets = [Pubkey::default(); rules::SEATS]; t.finished_at = 0;
        }
        93 => {
            check(data.len() == 18 && a.len() == if t.version >= 3 && data[17] == 1 {8} else {7} && data[17] <= 1, 9)?;
            if t.version >= 3 && data[17] == 1 { super::magicblock::require_inbox(program, &a[7], a[1].key)?; }
            check(*a[0].key == bank.authority, 11)?;
            t.enabled = data[17] == 1; t.changed()?;
        }
        _ => return Err(ProgramError::InvalidInstructionData),
    }
    if matches!(tag, 85 | 88 | 89 | 91) { reconcile(a, &mut bank, &t, old_reserved, old_stakes, was_active)?; }
    if tag == 85 { super::rewards::fund(program, &a[..15], &bank, &t, seat(data)?)?; }
    if was_active && !t.round.active() {
        t.finished_at = now;
        // One bounded binary receipt retains ALL hands/cards/identities after
        // the next round replaces the table. Consumers must require tx success.
        let receipt = borsh::to_vec(&*t).map_err(|_| ProgramError::InvalidAccountData)?;
        solana_program::log::sol_log_data(&[b"SDICE_BLACKJACK_RESULT_V1", a[1].key.as_ref(), &receipt]);
    } else if tag == 88 {
        // Retain the exact post-draw state for independent proof replay even if
        // a player acts before the keeper's next account read. Emit only one
        // large snapshot per transaction so settlement stays below the log cap.
        let receipt = borsh::to_vec(&*t).map_err(|_| ProgramError::InvalidAccountData)?;
        solana_program::log::sol_log_data(&[b"SDICE_BLACKJACK_DRAW_V1", a[1].key.as_ref(), &receipt]);
    }
    check(token_account(&a[5], a[3].key, a[1].key)? >= t.round.credits()?, 827)?;
    msg!("SDICE_BLACKJACK_STATE table={} round={} revision={} phase={:?} actor={} hand={} draw={}",
        a[1].key, t.round.id, t.round.revision, t.round.stage, t.round.actor, t.round.hand, t.round.draw_nonce);
    write(&a[1], &*t)
}
