//! Game-specific enrollment is recorded with the native wager, before its result.
//! Scores never accept amounts supplied by a caller. Prize funds stay in the
//! existing isolated tournament vaults; no access to bank capital is added.
use super::*;
use crate::blackjack::{rules::{self, Outcome, Stage}, state};

pub const BLACKJACK: u8 = 1;
pub const SLOTS: u8 = 2;
pub const TRACKER_SEED: &[u8] = b"bj-series-v1";
pub const TRACKER_LEN: usize = 1 + 32 * 2 + 8 + 2 + 32 * rules::SEATS + 1;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct BlackjackTracker {
    pub version: u8,
    pub bank: Pubkey,
    pub table: Pubkey,
    pub round: u64,
    pub pending: u16,
    pub events: [Pubkey; rules::SEATS],
    pub bump: u8,
}
pub fn tracker_address(program: &Pubkey, table: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[TRACKER_SEED, table.as_ref()], program)
}
fn tracker(program: &Pubkey, a: &AccountInfo, table: &Pubkey, bank: &Pubkey) -> Result<BlackjackTracker, ProgramError> {
    check(a.data_len() == TRACKER_LEN, 1100)?;
    let t: BlackjackTracker = load(program, a)?;
    let (key, bump) = tracker_address(program, table);
    check(*a.key == key && t.version == 1 && t.bank == *bank && t.table == *table
        && t.bump == bump && t.pending < (1 << rules::SEATS), 1100)?;
    Ok(t)
}

// 112: game:u8; authority(S,W), bank, series(W), intake, mint, System.
// 113: authority(S,W), bank, paused idle blackjack table(W), tracker(W), System.
// 114: seat:u8; bank, terminal blackjack table, tracker(W), series, event(W), entry(W).
pub fn process(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    match data.first().copied() {
        Some(112) => initialize(program, a, data),
        Some(113) => enable_blackjack(program, a, data),
        Some(114) => score_blackjack(program, a, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
fn initialize(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 6 && data.len() == 2 && [BLACKJACK, SLOTS].contains(&data[1]), 9)?;
    signer(&a[0])?;
    let b = load_bank(program, &a[1])?;
    check(b.version == 4 && b.authority == *a[0].key && b.mint == *a[4].key, 101)?;
    check(valid_mint(&a[4])? == 6, 100)?;
    let game = data[1];
    let (key, bump) = game_series_address(program, a[1].key, game);
    check(*a[2].key == key, 100)?;
    let intake = Pubkey::find_program_address(&[key.as_ref(), spl_token_2022::id().as_ref(), b.mint.as_ref()], &ATA).0;
    check(*a[3].key == intake, 100)?;
    token_account(&a[3], &b.mint, &key)?;
    create_pda(&a[0], &a[2], &a[5], program, SERIES_LEN,
        &[b"world-series", a[1].key.as_ref(), &[game], &[bump]])?;
    write(&a[2], &Series { version: game + 3, bank: *a[1].key, mint: b.mint,
        intake, current: 0, next: 1, carry: 0, bump })
}
fn enable_blackjack(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 5 && data == [113], 9)?;
    signer(&a[0])?;
    let b = load_bank(program, &a[1])?;
    let mut t = state::load(program, &a[2], a[1].key)?;
    check(b.version == 4 && b.authority == *a[0].key && t.version == 4 && !t.enabled
        && (!t.round.active() || (t.round.stage == Stage::Betting && t.round.betting_players() == 0)), 1101)?;
    if t.competitions() {
        tracker(program, &a[3], a[2].key, a[1].key)?;
        return Ok(());
    }
    let (key, bump) = tracker_address(program, a[2].key);
    check(*a[3].key == key, 1100)?;
    create_pda(&a[0], &a[3], &a[4], program, TRACKER_LEN, &[TRACKER_SEED, a[2].key.as_ref(), &[bump]])?;
    write(&a[3], &BlackjackTracker { version: 1, bank: *a[1].key, table: *a[2].key,
        round: t.round.id, pending: 0, events: [Pubkey::default(); rules::SEATS], bump })?;
    t.enable_competitions();
    t.changed()?;
    write(&a[2], &*t)
}

/// Called only inside native wager acceptance. Missing/unregistered entrants do
/// not participate; an accepted wager can never be enrolled after its outcome.
/// tail: canonical series, current event (or System), wallet entry (or System).
pub fn enroll(program: &Pubkey, bank: &Pubkey, game: u8, wallet: &Pubkey, now: i64,
    tail: &[AccountInfo]) -> Result<Pubkey, ProgramError> {
    check(tail.len() == 3 && [BLACKJACK, SLOTS].contains(&game), 9)?;
    let s = series(program, &tail[0], bank)?;
    check(series_game(s.version)? == game, 100)?;
    if s.current == 0 { return Ok(Pubkey::default()); }
    let e = event(program, &tail[1], tail[0].key)?;
    check(e.nonce == s.current && e.version == s.version, 100)?;
    enroll_event(program, bank, game, wallet, now, &tail[1], &tail[2])
}
/// An event is program-owned and derived from the game-specific series. Only
/// one event can be open per series; closed/expired events cannot enroll bets.
/// Blackjack omits the redundant series account to fit the first bet's packet.
fn enroll_event(program: &Pubkey, bank: &Pubkey, game: u8, wallet: &Pubkey, now: i64,
    event_account: &AccountInfo, entry_account: &AccountInfo) -> Result<Pubkey, ProgramError> {
    let series_key = game_series_address(program, bank, game).0;
    let mut e = event(program, event_account, &series_key)?;
    check(e.version == game + 3, 100)?;
    if !e.accepts(now) { return Ok(Pubkey::default()); }
    check(*entry_account.key == entry_address(program, event_account.key, wallet).0, 100)?;
    if entry_account.owner != program {
        check(entry_account.owner == &system_program::id() && entry_account.data_is_empty(), 100)?;
        return Ok(Pubkey::default());
    }
    let mut en = entry(program, entry_account, event_account.key, wallet)?;
    check(en.version == 2 && !en.refunded, 102)?;
    en.bets = add32(en.bets, 1)?;
    en.pending = add32(en.pending, 1)?;
    e.bets = plus(e.bets, 1)?;
    e.pending = add32(e.pending, 1)?;
    write_entry(entry_account, &en)?;
    write(event_account, &e)?;
    Ok(*event_account.key)
}
/// The caller has already validated a unique native pending wager/round marker.
pub fn finish(program: &Pubkey, bank: &Pubkey, game: u8, wallet: &Pubkey,
    expected_event: &Pubkey, profit: u64, tail: &[AccountInfo]) -> ProgramResult {
    check(tail.len() == 3 && *expected_event != Pubkey::default(), 9)?;
    let s = series(program, &tail[0], bank)?;
    check(series_game(s.version)? == game && *tail[1].key == *expected_event, 100)?;
    let mut e = event(program, &tail[1], tail[0].key)?;
    check(e.version == s.version && e.nonce == s.current && e.status == OPEN, 102)?;
    let mut en = entry(program, &tail[2], tail[1].key, wallet)?;
    check(en.version == 2 && !en.refunded, 102)?;
    en.score = en.score.checked_add(u128::from(profit)).ok_or(ProgramError::ArithmeticOverflow)?;
    en.wins = add32(en.wins, u32::from(profit > 0))?;
    en.pending = en.pending.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?;
    e.pending = e.pending.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?;
    e.rank(*wallet, en.score);
    write_entry(&tail[2], &en)?;
    write(&tail[1], &e)?;
    msg!("SDICE_SERIES_SCORE game={} event={} wallet={} profit={} total={}", game, tail[1].key, wallet, profit, en.score);
    Ok(())
}
/// After the native blackjack bet has succeeded. tail: event, entry, tracker.
pub fn enroll_blackjack(program: &Pubkey, bank: &Pubkey, table: &Pubkey,
    t: &state::Table, seat: usize, now: i64, tail: &[AccountInfo]) -> ProgramResult {
    check(tail.len() == 3 && seat < rules::SEATS, 9)?;
    let mut tr = tracker(program, &tail[2], table, bank)?;
    if tr.round != t.round.id {
        check(tr.pending == 0 && tr.round < t.round.id, 1102)?;
        tr.round = t.round.id;
        tr.events = [Pubkey::default(); rules::SEATS];
    }
    check(tr.events[seat] == Pubkey::default() && tr.pending & (1 << seat) == 0, 1102)?;
    let event = if *tail[0].key == system_program::id() {
        check(*tail[1].key == system_program::id(), 100)?;
        Pubkey::default()
    } else { enroll_event(program, bank, BLACKJACK, &t.round_wallets[seat], now, &tail[0], &tail[1])? };
    if event != Pubkey::default() {
        tr.events[seat] = event;
        tr.pending |= 1 << seat;
    }
    write(&tail[2], &tr)
}
pub fn require_blackjack_scored(program: &Pubkey, bank: &Pubkey, table: &Pubkey,
    round: u64, account: &AccountInfo) -> ProgramResult {
    let tr = tracker(program, account, table, bank)?;
    check(tr.round <= round && tr.pending == 0, 1102)
}
pub fn blackjack_profit(player: &rules::Player, stage: Stage) -> Result<u64, ProgramError> {
    check(matches!(stage, Stage::Complete | Stage::Refunded), 1103)?;
    if stage == Stage::Refunded { return Ok(0); }
    check(usize::from(player.count) <= rules::HANDS, 1103)?;
    player.hands[..usize::from(player.count)].iter().try_fold(0u64, |total, h| {
        let profit = if matches!(h.outcome, Outcome::Win | Outcome::Blackjack) {
            h.returned.checked_sub(h.wager).ok_or(ProgramError::ArithmeticOverflow)?
        } else { 0 };
        plus(total, profit)
    })
}
fn score_blackjack(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 6 && data.len() == 2 && usize::from(data[1]) < rules::SEATS, 9)?;
    load_bank(program, &a[0])?;
    let t = state::load(program, &a[1], a[0].key)?;
    check(t.competitions() && !t.round.active(), 1103)?;
    let mut tr = tracker(program, &a[2], a[1].key, a[0].key)?;
    check(tr.round == t.round.id, 1102)?;
    let seat = usize::from(data[1]);
    if tr.pending & (1 << seat) == 0 { return Ok(()); }
    let profit = blackjack_profit(&t.round.players[seat], t.round.stage)?;
    finish(program, a[0].key, BLACKJACK, &t.round_wallets[seat], &tr.events[seat], profit, &a[3..])?;
    tr.pending &= !(1 << seat);
    write(&a[2], &tr)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn games_have_distinct_series_and_legacy_dice_keeps_its_address() {
        let p = Pubkey::new_unique(); let b = Pubkey::new_unique();
        assert_eq!(game_series_address(&p, &b, 0), series_address(&p, &b));
        assert_ne!(game_series_address(&p, &b, BLACKJACK), game_series_address(&p, &b, SLOTS));
        assert_ne!(game_series_address(&p, &b, BLACKJACK), series_address(&p, &b));
        assert_eq!(series_game(3).unwrap(), 0);
        assert_eq!(series_game(4).unwrap(), BLACKJACK);
        assert_eq!(series_game(5).unwrap(), SLOTS);
        assert!(series_game(6).is_err());
    }
    #[test]
    fn blackjack_counts_only_positive_hand_profit_not_stakes_losses_or_refunds() {
        let mut p = rules::Player::default(); p.count = 4;
        for (h, (wager, returned, outcome)) in p.hands.iter_mut().zip([
            (100, 250, Outcome::Blackjack), (200, 400, Outcome::Win),
            (100, 0, Outcome::Loss), (100, 100, Outcome::Push)]) {
            h.wager = wager; h.returned = returned; h.outcome = outcome;
        }
        assert_eq!(blackjack_profit(&p, Stage::Complete).unwrap(), 350);
        assert_eq!(blackjack_profit(&p, Stage::Refunded).unwrap(), 0);
        assert!(blackjack_profit(&p, Stage::PlayerTurn).is_err());
        p.hands[0].returned = 99;
        assert!(blackjack_profit(&p, Stage::Complete).is_err());
    }
    #[test]
    fn tracker_layout_is_fixed() {
        let t = BlackjackTracker {version:1,bank:Pubkey::default(),table:Pubkey::default(),
            round:1,pending:0,events:[Pubkey::default();rules::SEATS],bump:0};
        assert_eq!(borsh::to_vec(&t).unwrap().len(), TRACKER_LEN);
    }
}
