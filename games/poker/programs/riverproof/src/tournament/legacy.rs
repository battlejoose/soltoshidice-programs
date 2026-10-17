//! Frozen production layouts. Never expand these arrays or reinterpret an old
//! event as a new forty-entry event: unpaid historical prizes remain claimable.
use anchor_lang::prelude::*;
use crate::errors::RiverproofError;
use super::{accounts::{CashRules, PokerArena, PokerTournament},
    model::{Entrant, EntryStatus, Phase, FinishReason, PrizeAward, CAPACITY}};

pub const LEGACY_ARENA_LEN: usize = 475;
pub const LEGACY_TOURNAMENT_LEN: usize = 2608;

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LegacyPokerArena {
    pub operator: Pubkey,
    pub mint: Pubkey,
    pub tables: [Pubkey; 3],
    pub bank_program: Pubkey,
    pub bank: Pubkey,
    pub cash_rules: [CashRules; 3],
    pub running: bool,
    pub ends_at: i64,
    pub reconciled: [u64; 3],
    pub decision_counter: u64,
    pub completed_hand: [u64; 3],
    pub completion_order: [u64; 3],
    pub active: Pubkey,
    pub next_id: u64,
    pub starts_at: i64,
    pub cash_closes_at: i64,
    pub drained: u8,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LegacyTournamentState {
    pub creator: Pubkey,
    pub prize_sdice: u64,
    pub buy_in: u64,
    pub starts_at: i64,
    pub cash_closes_at: i64,
    pub entry_closes_at: i64,
    pub ends_at: i64,
    pub phase: Phase,
    pub drained_tables: u8,
    pub registered: u8,
    pub entered: u8,
    pub elimination_order: u64,
    pub rake_paid: u64,
    pub last_hand: [u64; 3],
    pub entries: Vec<Entrant>,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone)]
pub struct LegacyPokerTournament {
    pub arena: Pubkey,
    pub id: u64,
    pub state: LegacyTournamentState,
    pub finish_reason: Option<FinishReason>,
    pub finalized_at: i64,
    pub refunded: u32,
    pub prizes_paid: u16,
    pub awards: Vec<PrizeAward>,
    pub bump: u8,
    pub vault_bump: u8,
}

pub fn read_legacy_arena(data: &[u8]) -> Result<LegacyPokerArena> {
    require!(data.len() == LEGACY_ARENA_LEN && data[..8] == *PokerArena::DISCRIMINATOR,
        RiverproofError::InvalidTournament);
    LegacyPokerArena::deserialize(&mut &data[8..]).map_err(Into::into)
}

/// The complete source representation is retained so a claim only changes its
/// paid bit. In particular old refund masks, standings and table indexes survive.
pub enum PrizeClaimAccount {
    Legacy(Box<LegacyPokerTournament>),
    Current(Box<PokerTournament>),
}

pub struct PrizeClaimView<'a> {
    pub arena: Pubkey,
    pub id: u64,
    pub phase: Phase,
    pub prize_sdice: u64,
    pub prizes_paid: u16,
    pub awards: &'a [PrizeAward],
    pub bump: u8,
    pub vault_bump: u8,
}

impl PrizeClaimAccount {
    pub fn read(data: &[u8]) -> Result<Self> {
        require!(data.len() >= 8 && data[..8] == *PokerTournament::DISCRIMINATOR,
            RiverproofError::InvalidTournament);
        let account = if data.len() == LEGACY_TOURNAMENT_LEN {
            let t = LegacyPokerTournament::deserialize(&mut &data[8..])?;
            require!(t.state.entries.len() == 30 && t.state.registered <= 30
                && t.state.entered <= t.state.registered && t.refunded >> 30 == 0,
                RiverproofError::InvalidTournament);
            Self::Legacy(Box::new(t))
        } else {
            require!(data.len() == 8 + PokerTournament::INIT_SPACE, RiverproofError::InvalidTournament);
            let t = PokerTournament::try_deserialize(&mut &data[..])?;
            require!(t.state.entries.len() == CAPACITY && t.state.registered as usize <= CAPACITY
                && t.state.entered <= t.state.registered && t.refunded >> CAPACITY == 0,
                RiverproofError::InvalidTournament);
            Self::Current(Box::new(t))
        };
        let view = account.view();
        require!(matches!(view.phase, Phase::Finishing | Phase::Complete | Phase::Cancelled)
            && !view.awards.is_empty() && view.awards.len() <= 10
            && view.prizes_paid & !((1u16 << view.awards.len()) - 1) == 0,
            RiverproofError::InvalidTournament);
        let total = view.awards.iter().try_fold(0u64, |sum, award| {
            require!(award.wallet != Pubkey::default(), RiverproofError::InvalidTournament);
            sum.checked_add(award.amount).ok_or_else(|| error!(RiverproofError::InvalidTournament))
        })?;
        require!(total == crate::sdice::base_units(view.prize_sdice)?, RiverproofError::InvalidTournament);
        // A refunded seat keeps its recorded historical stack. Nothing in this
        // reader changes entrant status or supplies a second balance refund.
        let entries = match &account { Self::Legacy(t) => &t.state.entries, Self::Current(t) => &t.state.entries };
        require!(entries.iter().filter(|e| e.status != EntryStatus::Empty).all(|e| e.wallet != Pubkey::default()),
            RiverproofError::InvalidTournament);
        Ok(account)
    }

    pub fn view(&self) -> PrizeClaimView<'_> {
        match self {
            Self::Legacy(t) => PrizeClaimView { arena: t.arena, id: t.id, phase: t.state.phase,
                prize_sdice: t.state.prize_sdice, prizes_paid: t.prizes_paid, awards: &t.awards,
                bump: t.bump, vault_bump: t.vault_bump },
            Self::Current(t) => PrizeClaimView { arena: t.arena, id: t.id, phase: t.state.phase,
                prize_sdice: t.state.prize_sdice, prizes_paid: t.prizes_paid, awards: &t.awards,
                bump: t.bump, vault_bump: t.vault_bump },
        }
    }

    pub fn mark_paid(&mut self, award: u8, data: &mut [u8]) -> Result<()> {
        require!(award < 10, RiverproofError::InvalidTournament);
        match self {
            Self::Legacy(t) => {
                require!(data.len() == LEGACY_TOURNAMENT_LEN, RiverproofError::InvalidTournament);
                t.prizes_paid |= 1 << award;
                // Leave the old discriminator and unused allocation bytes intact.
                t.serialize(&mut &mut data[8..])?;
            }
            Self::Current(t) => {
                require!(data.len() == 8 + PokerTournament::INIT_SPACE, RiverproofError::InvalidTournament);
                t.prizes_paid |= 1 << award;
                t.try_serialize(&mut &mut data[..])?;
            }
        }
        Ok(())
    }
}
