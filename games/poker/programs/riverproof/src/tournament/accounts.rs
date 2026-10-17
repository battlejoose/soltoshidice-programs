use anchor_lang::prelude::*;
use crate::{errors::RiverproofError, state::TableConfig};
use super::model::{TournamentState, FinishReason, PrizeAward, TABLES};

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Default)]
pub struct CashRules {
    pub buy_in: u64, pub small_blind: u64, pub big_blind: u64, pub ante: u64,
}

pub const ARENA_SEED: &[u8] = b"poker-arena";
pub const TOURNAMENT_SEED: &[u8] = b"poker-tournament";
pub const PRIZE_SEED: &[u8] = b"poker-prize";

#[account]
#[derive(InitSpace)]
pub struct PokerArena {
    pub operator: Pubkey,
    pub mint: Pubkey,
    pub tables: [Pubkey; TABLES],
    pub bank_program: Pubkey,
    pub bank: Pubkey,
    pub cash_rules: [CashRules; TABLES],
    /// Cached by native activation/reconciliation, so every private-dealing
    /// packet need not carry the full tournament account as an extra key.
    pub running: bool,
    pub ends_at: i64,
    pub reconciled: [u64; TABLES],
    /// Native decision order, captured with the actual hand completion rather
    /// than whichever keeper submits the later fee/score reconciliation first.
    pub decision_counter: u64,
    pub completed_hand: [u64; TABLES],
    pub completion_order: [u64; TABLES],
    pub active: Pubkey,
    pub next_id: u64,
    pub starts_at: i64,
    pub cash_closes_at: i64,
    pub drained: u8,
    pub bump: u8,
}

#[account]
#[derive(InitSpace)]
pub struct PokerTournament {
    pub arena: Pubkey,
    pub id: u64,
    pub state: TournamentState,
    pub finish_reason: Option<FinishReason>,
    pub finalized_at: i64,
    /// Entry-index mask. Ranking stacks remain frozen after their cash-out.
    pub refunded: u64,
    pub prizes_paid: u16,
    #[max_len(10)]
    pub awards: Vec<PrizeAward>,
    pub bump: u8,
    pub vault_bump: u8,
}

pub fn arena_address(operator: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[ARENA_SEED, operator.as_ref()], &crate::ID).0
}

impl PokerArena {
    pub fn require_current_results(&self) -> Result<()> {
        require!(self.completed_hand.iter().zip(self.reconciled).all(|(completed, recorded)| *completed <= recorded),
            RiverproofError::TournamentResultPending);
        Ok(())
    }
    pub fn next_decision(&mut self) -> Result<u64> {
        self.decision_counter = self.decision_counter.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
        Ok(self.decision_counter)
    }
    pub fn note_hand_completion(&mut self, table: usize, hand: u64) -> Result<()> {
        require!(table < TABLES && hand >= self.completed_hand[table], RiverproofError::InvalidTournament);
        if hand == self.completed_hand[table] { return Ok(()); }
        self.completion_order[table] = self.next_decision()?;
        self.completed_hand[table] = hand;
        Ok(())
    }
}

/// Called by native play/dealing paths in the transaction which settles a hand.
/// No new accounts: the existing arena meta must be writable for tournament
/// actions which can complete a hand. Cash play and unfinished hands are no-ops.
pub fn note_native_completion(config: &Account<TableConfig>, hand: &crate::state::HandState, remaining: &[AccountInfo]) -> Result<()> {
    if !is_managed(config) || hand.stage != crate::state::HandStage::Complete { return Ok(()); }
    let key = arena_address(&config.creator);
    let info = remaining.iter().find(|a| *a.key == key).ok_or(RiverproofError::TournamentAccountRequired)?;
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidTournament);
    let mut arena = PokerArena::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    if arena.active == Pubkey::default() || !arena.running { return Ok(()); }
    let index = arena.tables.iter().position(|t| *t == config.key()).ok_or(RiverproofError::InvalidTournament)?;
    if arena.completed_hand[index] == hand.hand_id { return Ok(()); }
    require!(info.is_writable, RiverproofError::TournamentAccountRequired);
    arena.note_hand_completion(index, hand.hand_id)?;
    arena.try_serialize(&mut &mut info.try_borrow_mut_data()?[..])?;
    Ok(())
}

/// An off-curve program-owned gate replaces the zero join key ONLY through
/// bind_poker_arena. Private password gates retain their existing signer check.
pub fn is_managed(config: &TableConfig) -> bool {
    config.join_key == arena_address(&config.creator)
}

#[derive(Clone, Copy)]
pub enum CashOperation { Start, Enter, Continue, Exit, Close }

/// Called only inside start_hand_mental, after the arena guards. The config is
/// persisted atomically with the new hand and stays fixed through all streets,
/// shuffles, unlocks and retries. Cash tables retain their saved cash blinds.
pub fn apply_starting_blinds(config: &mut Account<TableConfig>, remaining: &[AccountInfo]) -> Result<()> {
    if !is_managed(config) { return Ok(()); }
    let key = arena_address(&config.creator);
    let info = remaining.iter().find(|a| *a.key == key)
        .ok_or(RiverproofError::TournamentAccountRequired)?;
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidTournament);
    let arena = PokerArena::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require!(arena.operator == config.creator && arena.mint == config.mint
        && arena.tables.contains(&config.key()), RiverproofError::InvalidTournament);
    if arena.active != Pubkey::default() && arena.running {
        let now = Clock::get()?.unix_timestamp;
        require!(now >= arena.starts_at && now < arena.ends_at, RiverproofError::TournamentTableReserved);
        let (small, big) = super::blinds::blinds_at(config.buy_in, arena.starts_at, now)
            .map_err(|_| error!(RiverproofError::InvalidTournament))?;
        config.small_blind = small;
        config.big_blind = big;
    }
    Ok(())
}

/// The marker is in the existing config, so omitting the extra account cannot
/// opt out of a funded schedule. Table configs and hand layouts stay unchanged;
/// the arena is expanded only by the explicit operator migration.
pub fn guard_cash(config: &Account<TableConfig>, remaining: &[AccountInfo], op: CashOperation) -> Result<bool> {
    if !is_managed(config) { return Ok(false); }
    let key = arena_address(&config.creator);
    let info = remaining.iter().find(|a| *a.key == key)
        .ok_or(RiverproofError::TournamentAccountRequired)?;
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidTournament);
    let arena = PokerArena::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require!(arena.operator == config.creator && arena.mint == config.mint
        && arena.tables.contains(&config.key()), RiverproofError::InvalidTournament);
    // Registered tables cannot be deleted underneath a funded schedule.
    require!(!matches!(op, CashOperation::Close), RiverproofError::TournamentTableReserved);
    if arena.active != Pubkey::default() {
        let now = Clock::get()?.unix_timestamp;
        if arena.running {
            if matches!(op, CashOperation::Start) { arena.require_current_results()?; }
            require!(!matches!(op, CashOperation::Enter | CashOperation::Exit)
                && now < arena.ends_at, RiverproofError::TournamentTableReserved);
            return Ok(true);
        }
        let deadline = if matches!(op, CashOperation::Start | CashOperation::Enter) {
            arena.cash_closes_at
        } else { arena.starts_at };
        require!(now < deadline, RiverproofError::TournamentTableReserved);
    }
    Ok(true)
}

/// Do not overwrite a tournament hand until its net native balances and
/// eliminations have been reconciled exactly once into the tournament.
pub fn require_recorded(config: &Account<TableConfig>, hand: &crate::state::HandState, remaining: &[AccountInfo]) -> Result<()> {
    if !is_managed(config) || hand.stage != crate::state::HandStage::Complete { return Ok(()); }
    let key = arena_address(&config.creator);
    let info = remaining.iter().find(|a| *a.key == key).ok_or(RiverproofError::TournamentAccountRequired)?;
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidTournament);
    let arena = PokerArena::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    if arena.active != Pubkey::default() && arena.running {
        let index = arena.tables.iter().position(|t| *t == config.key()).ok_or(RiverproofError::InvalidTournament)?;
        require!(arena.reconciled[index] >= hand.hand_id, RiverproofError::TournamentResultPending);
    }
    Ok(())
}
