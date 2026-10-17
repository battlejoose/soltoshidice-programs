//! One global table per bank. Every accepted bet is immutable and fully reserved.
//! Queued rounds cannot start their clock until the previous playback has ended.
use super::*;

pub const TABLE_LEN: usize = 1 + 32 + 8 + 8 + 8 + 1;
pub const SHARED_ROUND_LEN: usize = 1 + 32 + 8 + 1 + 8 + 8 + 8 + 8 + 32 + 32 + 32 + 64 + 3 + 1 + 4 + 4 + 8 + 8 + 8 + 1 + 1;
pub const BET_LEN: usize = 1 + 32 + 32 + 8 + 4 + 1 + 1 + 3 + 1 + 1 + 8 + 1;
pub const BETTING_SECONDS: i64 = 60;
pub const REQUEST_TIMEOUT_SECONDS: i64 = 300;
pub const ORACLE_TIMEOUT_SECONDS: i64 = 180;
pub const PLAYBACK_LEAD_SECONDS: i64 = 3;
// One second introducing the shooter, then two seconds holding the final result.
pub const HAND_SECONDS: i64 = 3;
pub const THROW_SECONDS: i64 = 2;
pub const QUEUED: u8 = 0;
pub const BETTING: u8 = 1;
pub const RANDOMNESS: u8 = 2;
pub const SETTLING: u8 = 3;
pub const PLAYING: u8 = 4;
pub const FINISHED: u8 = 5;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Table {
    pub version: u8, pub bank: Pubkey, pub next_nonce: u64,
    pub current: u64, pub queued: u64, pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct SharedRound {
    pub version: u8, pub bank: Pubkey, pub nonce: u64, pub state: u8,
    pub opened_at: i64, pub closes_at: i64, pub requested_at: i64, pub resolve_deadline: i64,
    pub request: Pubkey, pub request_seed: [u8; 32], pub request_client: Pubkey,
    pub randomness: [u8; 64], pub bank_dice: [u8; 3], pub bank_throws: u8,
    pub bet_count: u32, pub resolved_count: u32, pub player_throws: u64,
    pub playback_start: i64, pub playback_end: i64, pub refunded: bool, pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Bet {
    pub version: u8, pub round: Pubkey, pub player: Pubkey, pub wager: u64,
    pub position: u32, pub settled: bool, pub claimed: bool,
    pub dice: [u8; 3], pub throws: u8, pub outcome: u8, pub payout: u64, pub bump: u8,
}

fn add_time(now: i64, seconds: i64) -> Result<i64, ProgramError> {
    now.checked_add(seconds).ok_or(ProgramError::ArithmeticOverflow)
}
pub fn round_address(program: &Pubkey, bank: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"shared-round", bank.as_ref(), &nonce.to_le_bytes()], program)
}
pub(super) fn load_table(program: &Pubkey, account: &AccountInfo, bank: &Pubkey) -> Result<Table, ProgramError> {
    check(account.owner == program, 2)?;
    let t = Table::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = Pubkey::find_program_address(&[b"table", bank.as_ref()], program);
    check(*account.key == expected && t.bank == *bank && (1..=4).contains(&t.version) && t.bump == bump, 3)?;
    Ok(t)
}
pub(super) fn load_shared(program: &Pubkey, account: &AccountInfo, bank: &Pubkey) -> Result<SharedRound, ProgramError> {
    check(account.owner == program, 2)?;
    let r = SharedRound::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = round_address(program, bank, r.nonce);
    check(*account.key == expected && r.bank == *bank && (1..=2).contains(&r.version) && r.bump == bump, 3)?;
    Ok(r)
}
pub(super) fn load_bet(program: &Pubkey, account: &AccountInfo, round: &Pubkey) -> Result<Bet, ProgramError> {
    check(account.owner == program, 2)?;
    let b = Bet::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = Pubkey::find_program_address(&[b"bet", round.as_ref(), b.player.as_ref()], program);
    check(*account.key == expected && b.round == *round && (1..=4).contains(&b.version) && b.bump == bump, 3)?;
    Ok(b)
}

// Owner-requested cleanup of the completed September 5 devnet capacity test.
// This is deliberately bound to one existing account, not an admin skip for games.
const LOAD_TEST_PROGRAM: Pubkey = solana_program::pubkey!("A2zoMVpFhSiqTZZwQgsPTZrNtJFv6jGhjpr18g6jKtRj");
const LOAD_TEST_ROUND: Pubkey = solana_program::pubkey!("tQk5XrpbGc335rmPBoqnUfCmCcJ3GJ38ueiAE3XtqEg");
fn finish_devnet_load_test(program: &Pubkey, round_key: &Pubkey, authority: &Pubkey,
    bank: &Bank, table: &mut Table, round: &mut SharedRound, now: i64) -> ProgramResult {
    check(*authority == bank.authority, 11)?;
    check(*program == LOAD_TEST_PROGRAM && *round_key == LOAD_TEST_ROUND, 45)?;
    check(table.current == 11 && table.queued == 0 && table.next_nonce == 12
        && round.nonce == 11 && round.state == PLAYING && !round.refunded
        && round.bet_count == 500 && round.resolved_count == 500
        && bank.reserved == 0 && now >= round.playback_start, 45)?;
    // No token/bet/randomness writes: all liabilities have already been paid.
    round.state = FINISHED;
    round.playback_end = now;
    table.current = 0;
    Ok(())
}

impl SharedRound {
    pub fn empty(bank: Pubkey, nonce: u64, bump: u8) -> Self {
        Self { version: 1, bank, nonce, state: QUEUED, opened_at: 0, closes_at: 0,
            requested_at: 0, resolve_deadline: 0, request: Pubkey::default(), request_seed: [0; 32],
            request_client: Pubkey::default(), randomness: [0; 64], bank_dice: [0; 3], bank_throws: 0,
            bet_count: 0, resolved_count: 0, player_throws: 0, playback_start: 0, playback_end: 0,
            refunded: false, bump }
    }
    pub fn open(&mut self, now: i64) -> ProgramResult {
        check(self.state == QUEUED && self.bet_count > 0, 30)?;
        self.opened_at = now;
        self.closes_at = add_time(now, BETTING_SECONDS)?;
        self.state = BETTING;
        Ok(())
    }
    pub fn accepts(&self, now: i64) -> bool {
        self.state == BETTING && now < self.closes_at
    }
    pub fn can_request(&self, now: i64) -> ProgramResult {
        check(self.state == BETTING && self.bet_count > 0 && now >= self.closes_at
            && now <= add_time(self.closes_at, REQUEST_TIMEOUT_SECONDS)?, 30)
    }
    pub fn finish_recording(&mut self, now: i64) -> ProgramResult {
        check(self.state == SETTLING && self.resolved_count == self.bet_count && self.bet_count > 0, 30)?;
        if self.refunded {
            self.state = FINISHED;
            self.playback_end = now;
        } else {
            let hands = if self.version == 2 && banking::instant(self.bank_dice).is_some() { 1 } else { i64::from(self.bet_count) + 1 };
            let throws = i64::try_from(self.player_throws).map_err(|_| ProgramError::ArithmeticOverflow)?
                .checked_add(i64::from(self.bank_throws)).ok_or(ProgramError::ArithmeticOverflow)?;
            let duration = hands.checked_mul(HAND_SECONDS).and_then(|x| throws.checked_mul(THROW_SECONDS).and_then(|y| x.checked_add(y)))
                .ok_or(ProgramError::ArithmeticOverflow)?;
            self.playback_start = add_time(now, PLAYBACK_LEAD_SECONDS)?;
            self.playback_end = add_time(self.playback_start, duration)?;
            self.state = PLAYING;
        }
        Ok(())
    }
    pub fn finish_playback(&mut self, now: i64) -> ProgramResult {
        check((self.state == PLAYING && now >= self.playback_end) || self.state == FINISHED, 30)?;
        self.state = FINISHED;
        Ok(())
    }
}

/// Domain separation makes the bank stream independent of any bettor or wager.
/// Player streams are bound to the immutable wallet/round before randomness exists.
pub fn shared_dice(randomness: &[u8; 64], round: &Pubkey, player: Option<&Pubkey>) -> ([u8; 3], u8) {
    let identity = player.copied().unwrap_or_default();
    let side = if player.is_some() { 1 } else { 0 };
    for attempt in 0..32u8 {
        let mut dice = [0; 3];
        let mut count = 0;
        for block in 0..8u8 {
            let hash = hashv(&[b"hooddice:shared:v1", randomness, round.as_ref(), identity.as_ref(), &[side, attempt, block]]).to_bytes();
            for byte in hash { if byte < 252 && count < 3 { dice[count] = byte % 6 + 1; count += 1; } }
            if count == 3 { break; }
        }
        if count < 3 { return ([0; 3], attempt + 1); }
        if score(dice) > 0 { return (dice, attempt + 1); }
    }
    ([0; 3], 32)
}

// Tags: 8 initialize table; 9 immutable bet(nonce,wager); 10 request VRF(salt32);
// 11 read VRF; 12 record bet outcome; 13 claim payout; 14 advance table; 15 failure refund.
// 16 one-time, bank-signed cleanup of the fully paid devnet load-test round only.
pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    process_inner(program, accounts, data, false)
}
pub(super) fn process_funded(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    process_inner(program, accounts, data, true)
}
fn process_inner(program: &Pubkey, accounts: &[AccountInfo], data: &[u8], funded: bool) -> ProgramResult {
    let tag = *data.first().ok_or(ProgramError::InvalidInstructionData)?;
    let mut iter = accounts.iter();
    match tag {
        8 => {
            check(data.len() == 1, 9)?;
            let authority = next_account_info(&mut iter)?;
            let bank_account = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            let system = next_account_info(&mut iter)?;
            signer(authority)?;
            let bank = load_bank(program, bank_account)?;
            check(bank.authority == *authority.key, 11)?;
            let (expected, bump) = Pubkey::find_program_address(&[b"table", bank_account.key.as_ref()], program);
            check(*table_account.key == expected, 3)?;
            create_pda(authority, table_account, system, program, TABLE_LEN, &[b"table", bank_account.key.as_ref(), &[bump]])?;
            write(table_account, &Table { version: 1, bank: *bank_account.key, next_nonce: 1, current: 0, queued: 0, bump })
        },
        9 => {
            check(data.len() == 17 || data.len() == 18, 9)?;
            let player = next_account_info(&mut iter)?;
            let bank_account = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let bet_account = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?;
            let vault = next_account_info(&mut iter)?;
            let source = next_account_info(&mut iter)?;
            let token = next_account_info(&mut iter)?;
            let system = next_account_info(&mut iter)?;
            let current_account = next_account_info(&mut iter)?;
            signer(player)?;
            let mut bank = load_bank(program, bank_account)?;
            // The signature commits to v4 rules; old approvals cannot opt in silently.
            check(if bank.version == 4 { data.len() == 18 && data[17] == 4 } else { data.len() == 17 }, 72)?;
            let mut table = load_table(program, table_account, bank_account.key)?;
            check((table.version >= 2) == funded, 80)?;
            let now = Clock::get()?.unix_timestamp;
            let nonce = u64_at(data, 1)?;
            let wager = u64_at(data, 9)?;
            check(wager > 0 && wager <= bank.max_wager, 10)?;
            check(*mint.key == bank.mint && *vault.key == bank.vault && *source.key != *vault.key, 14)?;
            let decimals = valid_mint(mint)?;
            let balance = token_account(vault, mint.key, bank_account.key)?;
            token_account(source, mint.key, player.key)?;
            let rewards = if bank.version >= 3 { let f = economy::bet_fees(wager)?; f.gross - f.net } else { 0 };
            let maximum = if bank.version == 4 { 3 } else { 2 };
            // Cover the maximum prize plus rewards before accepting the stake.
            let required = wager.checked_mul(maximum - 1).and_then(|x| x.checked_add(rewards)).ok_or(ProgramError::ArithmeticOverflow)?;
            check(balance.checked_sub(bank.reserved).ok_or(ProgramError::InsufficientFunds)? >= required, 15)?;
            let current_open = if table.current == 0 { false } else {
                let current = load_shared(program, current_account, bank_account.key)?;
                check(current.nonce == table.current, 3)?;
                current.accepts(now)
            };
            let target = if current_open { table.current } else if table.queued != 0 { table.queued } else { table.next_nonce };
            // A stale signed transaction fails rather than silently changing its round.
            check(nonce == target, 31)?;
            let (expected, bump) = round_address(program, bank_account.key, nonce);
            check(*round_account.key == expected, 3)?;
            let mut round = if target == table.next_nonce {
                create_pda(player, round_account, system, program, SHARED_ROUND_LEN,
                    &[b"shared-round", bank_account.key.as_ref(), &nonce.to_le_bytes(), &[bump]])?;
                table.next_nonce = table.next_nonce.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
                if table.current == 0 { table.current = nonce; } else { table.queued = nonce; }
                let mut created = SharedRound::empty(*bank_account.key, nonce, bump);
                if bank.version == 4 { created.version = 2; }
                created
            } else { load_shared(program, round_account, bank_account.key)? };
            check((bank.version == 4) == (round.version == 2), 72)?;
            check(round.state == QUEUED || round.accepts(now), 30)?;
            let (expected_bet, bet_bump) = Pubkey::find_program_address(&[b"bet", round_account.key.as_ref(), player.key.as_ref()], program);
            check(*bet_account.key == expected_bet, 3)?;
            // One immutable wager per wallet per round; duplicates cannot debit twice.
            create_pda(player, bet_account, system, program, BET_LEN,
                &[b"bet", round_account.key.as_ref(), player.key.as_ref(), &[bet_bump]])?;
            transfer(source, vault, mint, player, token, wager, decimals, None)?;
            if bank.version >= 2 {
                let pool = next_account_info(&mut iter)?; let profile = next_account_info(&mut iter)?;
                let receipt = next_account_info(&mut iter)?;
                economy::prepare_bet(program, bank_account, bet_account, player, system, pool, profile, receipt, wager)?;
                if bank.version >= 3 {
                    let fee_vault = next_account_info(&mut iter)?; let direct = next_account_info(&mut iter)?;
                    economy::fund_bet_rewards(program, &bank, bank_account, bet_account, player.key, wager,
                        mint, vault, token, pool, fee_vault, receipt, profile, direct)?;
                }
            }
            let bet = Bet { version: bank.version, round: *round_account.key, player: *player.key, wager,
                position: round.bet_count, settled: false, claimed: false, dice: [0; 3], throws: 0,
                outcome: 255, payout: 0, bump: bet_bump };
            round.bet_count = round.bet_count.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            if round.state == QUEUED && table.current == nonce { round.open(now)?; }
            bank.reserved = bank.reserved.checked_add(wager.checked_mul(maximum).ok_or(ProgramError::ArithmeticOverflow)?)
                .ok_or(ProgramError::ArithmeticOverflow)?;
            write(bet_account, &bet)?;
            write(round_account, &round)?;
            write(table_account, &table)?;
            write(bank_account, &bank)
        },
        10 => {
            check(data.len() == 33, 9)?;
            let payer = next_account_info(&mut iter)?;
            let bank_account = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let network = next_account_info(&mut iter)?;
            let treasury = next_account_info(&mut iter)?;
            let request = next_account_info(&mut iter)?;
            let system = next_account_info(&mut iter)?;
            let oracle = next_account_info(&mut iter)?;
            load_bank(program, bank_account)?;
            let table = load_table(program, table_account, bank_account.key)?;
            check((table.version >= 2) == funded, 80)?;
            let mut round = load_shared(program, round_account, bank_account.key)?;
            check(table.current == round.nonce, 30)?;
            let now = Clock::get()?.unix_timestamp;
            round.can_request(now)?;
            let salt = bytes32(data, 1)?;
            let seed = hashv(&[b"hooddice:oracle:v1", round_account.key.as_ref(), &salt]).to_bytes();
            if funded {
                let (expected, bump) = funding::payer_address(program, round_account.key);
                check(*payer.key == expected, 80)?;
                let reserve = next_account_info(&mut iter)?;
                check(*reserve.key == funding::round_address(program, round_account.key).0 && reserve.owner == program, 80)?;
                orao::request_fresh_signed(payer, network, treasury, request, system, oracle, &seed,
                    Some(&[b"round-sol", round_account.key.as_ref(), &[bump]]), Some(reserve))?;
            } else { orao::request_fresh(payer, network, treasury, request, system, oracle, &seed)?; }
            round.state = RANDOMNESS;
            round.request = *request.key;
            round.request_seed = seed;
            round.request_client = *payer.key;
            round.requested_at = now;
            round.resolve_deadline = add_time(now, ORACLE_TIMEOUT_SECONDS)?;
            write(round_account, &round)
        },
        11 => {
            check(data.len() == 1, 9)?;
            let bank_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let request = next_account_info(&mut iter)?;
            load_bank(program, bank_account)?;
            let mut round = load_shared(program, round_account, bank_account.key)?;
            check(round.state == RANDOMNESS && *request.key == round.request, 30)?;
            round.randomness = orao::read_request(request, &round.request_seed, &round.request_client)?.ok_or(ProgramError::Custom(43))?;
            (round.bank_dice, round.bank_throws) = shared_dice(&round.randomness, round_account.key, None);
            if round.version == 2 && round.bank_dice == [0;3] { round.refunded = true; }
            round.state = SETTLING;
            write(round_account, &round)
        },
        12 => {
            check(data.len() == 1, 9)?;
            let bank_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let bet_account = next_account_info(&mut iter)?;
            let mut bank = load_bank(program, bank_account)?;
            let mut round = load_shared(program, round_account, bank_account.key)?;
            let mut bet = load_bet(program, bet_account, round_account.key)?;
            if bet.settled { return Ok(()); }
            check(round.state == SETTLING && bet.position < round.bet_count, 30)?;
            check((bet.version == 4) == (round.version == 2), 72)?;
            if round.refunded { bet.outcome = 2; } else {
                if bet.version != 4 || banking::instant(round.bank_dice).is_none() {
                    (bet.dice, bet.throws) = shared_dice(&round.randomness, round_account.key, Some(&bet.player));
                }
                bet.outcome = if bet.version == 4 { banking::outcome(round.bank_dice, bet.dice) } else { result(round.bank_dice, bet.dice) };
            }
            let gross = if bet.version == 4 { banking::gross(bet.wager, bet.outcome, bet.dice)? } else { payout(bet.wager, bet.outcome)? };
            bet.payout = if bet.version == 2 { economy::fees(bet.wager, bet.outcome)?.net } else { gross };
            bet.settled = true;
            bet.claimed = bet.payout == 0;
            // Retain the exact unclaimed payout in reserve. A missing recipient ATA
            // cannot hold the table's animation or other bettors' settlements hostage.
            let release = bet.wager.checked_mul(if bet.version == 4 { 3 } else { 2 }).and_then(|x| x.checked_sub(gross)).ok_or(ProgramError::ArithmeticOverflow)?;
            bank.reserved = bank.reserved.checked_sub(release).ok_or(ProgramError::ArithmeticOverflow)?;
            round.resolved_count = round.resolved_count.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            round.player_throws = round.player_throws.checked_add(u64::from(bet.throws)).ok_or(ProgramError::ArithmeticOverflow)?;
            if round.resolved_count == round.bet_count { round.finish_recording(Clock::get()?.unix_timestamp)?; }
            write(bet_account, &bet)?;
            write(round_account, &round)?;
            write(bank_account, &bank)
        },
        13 => {
            check(data.len() == 1, 9)?;
            let bank_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let bet_account = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?;
            let vault = next_account_info(&mut iter)?;
            let destination = next_account_info(&mut iter)?;
            let token = next_account_info(&mut iter)?;
            let mut bank = load_bank(program, bank_account)?;
            load_shared(program, round_account, bank_account.key)?;
            let mut bet = load_bet(program, bet_account, round_account.key)?;
            check(bet.settled, 30)?;
            if bet.claimed { return Ok(()); }
            check(*mint.key == bank.mint && *vault.key == bank.vault && *destination.key != *vault.key, 14)?;
            let decimals = valid_mint(mint)?;
            token_account(vault, mint.key, bank_account.key)?;
            token_account(destination, mint.key, &bet.player)?;
            let gross = if bet.version == 2 { economy::fees(bet.wager, bet.outcome)?.gross } else { bet.payout };
            bank.reserved = bank.reserved.checked_sub(gross).ok_or(ProgramError::ArithmeticOverflow)?;
            transfer(vault, destination, mint, bank_account, token, bet.payout, decimals,
                Some(&[b"bank", bank.authority.as_ref(), bank.mint.as_ref(), &[bank.bump]]))?;
            if bet.version >= 2 {
                let pool = next_account_info(&mut iter)?; let fees = next_account_info(&mut iter)?;
                let receipt = next_account_info(&mut iter)?; let player_profile = next_account_info(&mut iter)?;
                let direct_profile = next_account_info(&mut iter)?;
                economy::collect_fees(program, &bank, bank_account, bet_account, &bet, mint, vault, token,
                    pool, fees, receipt, player_profile, direct_profile)?;
            }
            bet.claimed = true;
            write(bet_account, &bet)?;
            write(bank_account, &bank)
        },
        14 => {
            check(data.len() == 1, 9)?;
            let bank_account = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let queued_account = next_account_info(&mut iter)?;
            load_bank(program, bank_account)?;
            let mut table = load_table(program, table_account, bank_account.key)?;
            if table.current == 0 { return Ok(()); }
            let mut round = load_shared(program, round_account, bank_account.key)?;
            check(round.nonce == table.current, 30)?;
            let now = Clock::get()?.unix_timestamp;
            round.finish_playback(now)?;
            write(round_account, &round)?;
            if table.queued == 0 { table.current = 0; } else {
                let mut queued = load_shared(program, queued_account, bank_account.key)?;
                check(queued.nonce == table.queued, 30)?;
                queued.open(now)?;
                table.current = table.queued;
                table.queued = 0;
                write(queued_account, &queued)?;
            }
            write(table_account, &table)
        },
        15 => {
            check(data.len() == 1, 9)?;
            let bank_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let request = next_account_info(&mut iter)?;
            load_bank(program, bank_account)?;
            let mut round = load_shared(program, round_account, bank_account.key)?;
            let now = Clock::get()?.unix_timestamp;
            match round.state {
                BETTING => check(now > add_time(round.closes_at, REQUEST_TIMEOUT_SECONDS)?, 30)?,
                RANDOMNESS => {
                    check(now > round.resolve_deadline && *request.key == round.request, 30)?;
                    // A fulfilled result MUST be settled, even after the timeout.
                    // Neither the house nor a bettor can choose a refund after seeing it.
                    check(orao::read_request(request, &round.request_seed, &round.request_client)?.is_none(), 44)?;
                },
                _ => return Err(ProgramError::Custom(30)),
            }
            round.refunded = true;
            round.state = SETTLING;
            write(round_account, &round)
        },
        16 => {
            check(data.len() == 1, 9)?;
            let authority = next_account_info(&mut iter)?;
            signer(authority)?;
            let bank_account = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            let round_account = next_account_info(&mut iter)?;
            let bank = load_bank(program, bank_account)?;
            let mut table = load_table(program, table_account, bank_account.key)?;
            let mut round = load_shared(program, round_account, bank_account.key)?;
            finish_devnet_load_test(program, round_account.key, authority.key,
                &bank, &mut table, &mut round, Clock::get()?.unix_timestamp)?;
            write(round_account, &round)?;
            write(table_account, &table)
        },
        17 => {
            check(data.len() == 9, 9)?;
            let authority = next_account_info(&mut iter)?;
            let bank_account = next_account_info(&mut iter)?;
            signer(authority)?;
            let mut bank = load_bank(program, bank_account)?;
            check(bank.authority == *authority.key, 11)?;
            let max = u64_at(data, 1)?;
            check(max > 0 && max <= u64::MAX / 4, 10)?;
            // Only future bet acceptance reads this field. Existing reservations,
            // accepted stakes, deadlines, randomness, and payouts are untouched.
            bank.max_wager = max;
            write(bank_account, &bank)
        },
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wager_limit_update_requires_authority_and_preserves_bank_state() {
        let program = Pubkey::new_unique();
        let authority = Pubkey::new_unique();
        let mint = Pubkey::new_unique();
        let system = system_program::id();
        let (bank_key, bump) = Pubkey::find_program_address(
            &[b"bank", authority.as_ref(), mint.as_ref()], &program);
        let bank = Bank { version: 1, authority, mint, vault: Pubkey::new_unique(),
            reserved: 123_000_000, max_wager: 2_500_000_000, bump };
        let original = borsh::to_vec(&bank).unwrap();
        for (signed, correct_authority, correct_owner, max, valid_length, succeeds) in [
            (true, true, true, 1_000_000_000_000u64, true, true),
            (true, true, true, 1u64, true, true),
            (false, true, true, 1_000_000_000_000u64, true, false),
            (true, false, true, 1_000_000_000_000u64, true, false),
            (true, true, false, 1_000_000_000_000u64, true, false),
            (true, true, true, 0u64, true, false),
            (true, true, true, u64::MAX / 4 + 1, true, false),
            (true, true, true, 1_000_000_000_000u64, false, false),
        ] {
            let actor = if correct_authority { authority } else { Pubkey::new_unique() };
            let owner = if correct_owner { program } else { system };
            let mut actor_lamports = 1;
            let mut actor_data = [];
            let mut bank_lamports = 1;
            let mut bank_data = original.clone();
            let accounts = [
                AccountInfo::new(&actor, signed, false, &mut actor_lamports,
                    &mut actor_data, &system, false, 0),
                AccountInfo::new(&bank_key, false, true, &mut bank_lamports,
                    &mut bank_data, &owner, false, 0),
            ];
            let mut data = vec![17];
            data.extend_from_slice(&max.to_le_bytes());
            if !valid_length { data.push(0); }
            assert_eq!(super::super::process_instruction(&program, &accounts, &data).is_ok(), succeeds);
            let mut expected = original.clone();
            if succeeds { expected[105..113].copy_from_slice(&max.to_le_bytes()); }
            assert_eq!(&**accounts[1].try_borrow_data().unwrap(), &expected[..]);
        }
    }
    fn round() -> SharedRound { SharedRound::empty(Pubkey::new_from_array([1; 32]), 1, 2) }
    fn load_test() -> (Bank, Table, SharedRound) {
        let bank = Bank { version: 1, authority: Pubkey::new_unique(), mint: Pubkey::new_unique(),
            vault: Pubkey::new_unique(), reserved: 0, max_wager: 100, bump: 1 };
        let table = Table { version: 1, bank: Pubkey::new_unique(), next_nonce: 12, current: 11, queued: 0, bump: 1 };
        let mut r = round();
        r.nonce = 11; r.state = PLAYING; r.bet_count = 500; r.resolved_count = 500;
        r.playback_start = 100; r.playback_end = 3000;
        (bank, table, r)
    }
    #[test]
    fn paid_load_test_cleanup_preserves_results_and_cannot_repeat() {
        let (bank, mut table, mut r) = load_test();
        let random = r.randomness; let dice = r.bank_dice;
        finish_devnet_load_test(&LOAD_TEST_PROGRAM, &LOAD_TEST_ROUND, &bank.authority,
            &bank, &mut table, &mut r, 200).unwrap();
        assert_eq!(table.current, 0); assert_eq!(table.next_nonce, 12);
        assert_eq!(r.state, FINISHED); assert_eq!(r.playback_end, 200);
        assert_eq!(r.randomness, random); assert_eq!(r.bank_dice, dice);
        assert!(finish_devnet_load_test(&LOAD_TEST_PROGRAM, &LOAD_TEST_ROUND, &bank.authority,
            &bank, &mut table, &mut r, 201).is_err());
    }
    #[test]
    fn load_test_cleanup_rejects_other_games_liabilities_and_queue() {
        for case in 0..8 {
            let (mut bank, mut table, mut r) = load_test();
            let mut program = LOAD_TEST_PROGRAM; let mut key = LOAD_TEST_ROUND;
            let mut authority = bank.authority;
            match case {
                0 => program = Pubkey::new_unique(), 1 => key = Pubkey::new_unique(),
                2 => authority = Pubkey::new_unique(), 3 => bank.reserved = 1,
                4 => table.queued = 12, 5 => r.resolved_count = 499,
                6 => { table.current = 12; r.nonce = 12; }, 7 => r.state = SETTLING,
                _ => unreachable!(),
            }
            let before_table = borsh::to_vec(&table).unwrap();
            let before_round = borsh::to_vec(&r).unwrap();
            assert!(finish_devnet_load_test(&program, &key, &authority,
                &bank, &mut table, &mut r, 200).is_err());
            assert_eq!(borsh::to_vec(&table).unwrap(), before_table);
            assert_eq!(borsh::to_vec(&r).unwrap(), before_round);
        }
    }
    #[test]
    fn empty_table_never_starts_and_first_bet_gets_full_minute() {
        let mut r = round();
        assert!(r.open(100).is_err());
        assert_eq!(r.closes_at, 0);
        r.bet_count = 1;
        r.open(100).unwrap();
        assert_eq!(r.closes_at, 160);
        assert!(r.accepts(159));
        assert!(!r.accepts(160));
        assert!(r.can_request(159).is_err());
        r.can_request(160).unwrap();
        assert!(r.open(130).is_err());
        assert_eq!(r.closes_at, 160);
    }
    #[test]
    fn queue_waits_for_every_throw_before_its_full_minute() {
        let mut current = round();
        current.state = SETTLING;
        current.bet_count = 1000;
        current.resolved_count = 999;
        current.bank_throws = 2;
        current.player_throws = 1700;
        assert!(current.finish_recording(200).is_err());
        current.resolved_count = 1000;
        current.finish_recording(201).unwrap();
        assert_eq!(current.playback_start, 204);
        assert_eq!(current.playback_end, 204 + 1001 * 3 + 1702 * 2);
        let mut queued = round();
        queued.nonce = 2;
        queued.bet_count = 700;
        assert_eq!(queued.closes_at, 0);
        assert!(current.finish_playback(current.playback_end - 1).is_err());
        let end = current.playback_end;
        current.finish_playback(end).unwrap();
        queued.open(end).unwrap();
        assert_eq!(queued.opened_at, end);
        assert_eq!(queued.closes_at, end + 60);
    }
    #[test]
    fn final_result_pause_is_included_for_bank_and_each_player() {
        let mut r = round();
        r.version = 2;
        r.state = SETTLING;
        r.bet_count = 2; r.resolved_count = 2;
        r.bank_dice = [2, 2, 4];
        r.bank_throws = 2; r.player_throws = 3;
        r.finish_recording(100).unwrap();
        assert_eq!(r.playback_start, 103);
        assert_eq!(r.playback_end, 122); // Three 3s introductions/results + five 2s throws.
        assert!(r.finish_playback(121).is_err());
        r.finish_playback(122).unwrap();

        // An instant bank result holds once, with no phantom player sequences.
        r.state = SETTLING; r.bank_dice = [4, 5, 6];
        r.bank_throws = 1; r.player_throws = 0;
        r.finish_recording(200).unwrap();
        assert_eq!(r.playback_end, 208);
        assert!(r.finish_playback(207).is_err());

        // Upgrading timing never stretches a round that already has a deadline.
        r.playback_end = 207;
        r.finish_playback(207).unwrap();
    }
    #[test]
    fn request_timeout_does_not_allow_late_randomness_selection() {
        let mut r = round();
        r.bet_count = 1;
        r.open(100).unwrap();
        r.can_request(460).unwrap();
        assert!(r.can_request(461).is_err());
        r.state = RANDOMNESS;
        assert!(r.can_request(200).is_err());
        r.state = SETTLING;
        r.refunded = true;
        r.resolved_count = 1;
        r.finish_recording(470).unwrap();
        assert_eq!(r.state, FINISHED);
        assert_eq!(r.playback_end, 470);
        assert_eq!(r.playback_start, 0);
    }
    #[test]
    fn common_bank_and_distinct_reproducible_player_streams() {
        let r = Pubkey::new_from_array([3; 32]);
        let random = [9; 64];
        let bank = shared_dice(&random, &r, None);
        let mut outcomes = std::collections::BTreeSet::new();
        for i in 1..=255u8 {
            let player = Pubkey::new_from_array([i; 32]);
            let hand = shared_dice(&random, &r, Some(&player));
            assert_eq!(hand, shared_dice(&random, &r, Some(&player)));
            assert_eq!(bank, shared_dice(&random, &r, None));
            assert!(score(hand.0) > 0 && hand.1 >= 1 && hand.1 <= 32);
            outcomes.insert(hand.0);
        }
        assert!(outcomes.len() > 50);
    }
    #[test]
    fn shared_layout_lengths() {
        assert_eq!(borsh::to_vec(&round()).unwrap().len(), SHARED_ROUND_LEN);
        let table = Table { version: 1, bank: Pubkey::default(), next_nonce: 1, current: 0, queued: 0, bump: 1 };
        assert_eq!(borsh::to_vec(&table).unwrap().len(), TABLE_LEN);
        let bet = Bet { version: 1, round: Pubkey::default(), player: Pubkey::default(), wager: 1,
            position: 0, settled: false, claimed: false, dice: [0; 3], throws: 0, outcome: 255, payout: 0, bump: 1 };
        assert_eq!(borsh::to_vec(&bet).unwrap().len(), BET_LEN);
    }
}
