//! Phase 6: a hand the players deal to each other, with the chain as referee.
//!
//! The division of labour is the point. Players shuffle, lock, prove, and
//! verify between themselves — thousands of bytes per round, none of which the
//! chain sees. The chain keeps a digest of each round so nobody can equivocate
//! about what they published, holds the twenty-five positions a hand actually
//! opens, and opens them.
//!
//! Opening is the part worth being careful about, and it is one multiplication.
//! Each player publishes the inverse of their key for a position; the program
//! multiplies it in; when the last one lands the point *is* a card point and
//! the program reads which card off it. Nobody tells the chain what a card is,
//! so nobody can tell it wrong: a bad share yields a point that is no card at
//! all, which stalls the hand rather than changing it.
//!
//! Board cards are gated on the engine being parked for that street. Without
//! that, every player could publish every board share during the deal and the
//! whole board would be public before the flop.

use anchor_lang::prelude::*;
use riverproof_engine::table::{self as engine, Awaiting, DealPositions, NO_POSITION};

use crate::bridge::{self, NO_SEAT};
use crate::errors::RiverproofError;
use crate::instructions::table::assert_chips_conserved;
use crate::mental::{unlock_slot, Slot, MAX_SLOTS, NO_CARD, NO_SEAT_OWNER};
use crate::state::{
    DealPhase, DealSlot, HandCards, HandState, MentalDeal, SessionKey, TableConfig, TableState,
    HAND_CARDS_SEED, HAND_SEED, MENTAL_DEAL_SEED, SESSION_SEED, TABLE_STATE_SEED,
};

/// Which round a commitment covers.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum DealRound {
    Shuffle,
    Lock,
}

fn now_ms() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp.saturating_mul(1_000))
}

/// How long a deal may go without progress before anyone may abandon it.
///
/// Generous on purpose. Every step here is one small transaction, so a live
/// table refreshes this constantly and never comes close; a minute of complete
/// silence means somebody has genuinely gone.
pub const DEAL_STALL_MS: i64 = 60_000;

/// Pushes the abandon deadline out, because something just happened.
///
/// Extends, never shortens. It used to assign, which let a shorter clock
/// replace a longer one: `slot_is_due` waves through any hole slot, and a
/// player who has folded still holds their own two hole shares unspent, so
/// publishing one mid-hand rewrote a betting round's allowance down to sixty
/// seconds. The next street then parked with the deadline already expired and
/// no grace at all for whoever was about to publish. Progress can only ever buy
/// a deal more time.
fn made_progress(deal: &mut MentalDeal, now: i64) {
    deal.deadline = deal.deadline.max(now.saturating_add(DEAL_STALL_MS));
}

/// Pushes it out far enough to sit through a betting round.
///
/// The deal is not what anybody is waiting on while the table bets, but its
/// clock kept running anyway — and a legal round can outlast `DEAL_STALL_MS`
/// easily, since every seat may burn a full action timer. That left the
/// deadline already expired at the moment the next street began to be awaited,
/// so the first person to notice could void a hand that was merely being played
/// slowly. Betting is bounded, so the allowance is computed rather than
/// guessed: one action timer per seat, plus the usual stall grace. The tight
/// deadline comes back the moment the next deal step lands.
fn betting_reopened(deal: &mut MentalDeal, now: i64) {
    let round = crate::rules::ACTION_TIME_MS
        .saturating_mul(i64::from(deal.players))
        .saturating_add(DEAL_STALL_MS);
    deal.deadline = now.saturating_add(round);
}

/// The positions a hand opens, in the one order every party indexes them by.
///
/// Each seat's two hole cards in seat order, then the five board cards. It is
/// `DealPositions::dealt()`'s order, and the client mirrors it — get this wrong
/// and a player peels the right card for the wrong seat.
fn slot_layout(positions: &DealPositions) -> ([(u8, u8); MAX_SLOTS], usize) {
    let mut layout = [(0u8, 0u8); MAX_SLOTS];
    let mut count = 0;
    for (seat, slots) in positions.hole.iter().enumerate() {
        for position in slots {
            if *position != NO_POSITION {
                layout[count] = (*position, seat as u8);
                count += 1;
            }
        }
    }
    for position in positions.board {
        layout[count] = (position, NO_SEAT_OWNER);
        count += 1;
    }
    (layout, count)
}

impl MentalDeal {
    /// Which player index a seat is, if it is in this deal.
    fn player_of(&self, seat: u8) -> Option<u8> {
        self.seats[..self.players as usize]
            .iter()
            .position(|candidate| *candidate == seat)
            .map(|index| index as u8)
    }

    fn slot_mut(&mut self, index: usize) -> Result<&mut DealSlot> {
        require!(index < self.slot_count as usize, RiverproofError::UnknownDealSlot);
        Ok(&mut self.slots[index])
    }
}

/// The accounts every mental instruction needs.
#[derive(Accounts)]
pub struct MentalPlay<'info> {
    /// A seated player's wallet, or a session key acting for one.
    pub signer: Signer<'info>,

    #[account(
        seeds = [SESSION_SEED, session.authority.as_ref(), config.key().as_ref()],
        bump = session.bump,
    )]
    pub session: Option<Account<'info, SessionKey>>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
        constraint = table_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        mut,
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
        constraint = hand_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        seeds = [HAND_CARDS_SEED, config.key().as_ref()],
        bump = hand_cards.bump,
        constraint = hand_cards.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_cards: Box<Account<'info, HandCards>>,

    #[account(
        mut,
        seeds = [MENTAL_DEAL_SEED, config.key().as_ref()],
        bump = deal.bump,
        constraint = deal.table == config.key() @ RiverproofError::InvalidTableConfig,
        // Everything on the deal is stale for any other hand.
        constraint = deal.hand_id == hand_state.hand_id @ RiverproofError::WrongHand,
    )]
    pub deal: Box<Account<'info, MentalDeal>>,
}

impl<'info> MentalPlay<'info> {
    /// The seat this signer may act for, wallet or session key.
    fn authorized_seat(&self, now_seconds: i64) -> Result<u8> {
        let signer = self.signer.key();
        let owner = match self.session.as_ref() {
            Some(session) if session.session_key == signer => {
                require!(session.table == self.config.key(), RiverproofError::SessionTableMismatch);
                require!(session.expiry > now_seconds, RiverproofError::SessionExpired);
                session.authority
            }
            _ => signer,
        };
        self.table_state
            .seats
            .iter()
            .position(|seat| seat.occupied && seat.owner == owner)
            .map(|index| index as u8)
            .ok_or_else(|| RiverproofError::NoSeat.into())
    }

    /// That seat's player index in this deal.
    fn authorized_player(&self, now_seconds: i64) -> Result<u8> {
        let seat = self.authorized_seat(now_seconds)?;
        self.deal.player_of(seat).ok_or_else(|| RiverproofError::UnknownDealPlayer.into())
    }
}

#[derive(Accounts)]
pub struct StartMentalHand<'info> {
    /// Permissionless, like every other way of starting a hand.
    #[account(mut)]
    pub cranker: Signer<'info>,

    /// Tournament blinds are frozen into this config for the entire new hand.
    #[account(mut)]
    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
        constraint = table_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        mut,
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
        constraint = hand_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        seeds = [HAND_CARDS_SEED, config.key().as_ref()],
        bump = hand_cards.bump,
        constraint = hand_cards.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_cards: Box<Account<'info, HandCards>>,

    /// Created on the first mental hand at this table and reused after.
    #[account(
        init_if_needed,
        payer = cranker,
        space = 8 + MentalDeal::INIT_SPACE,
        seeds = [MENTAL_DEAL_SEED, config.key().as_ref()],
        bump,
    )]
    pub deal: Box<Account<'info, MentalDeal>>,

    pub system_program: Program<'info, System>,
}

/// Starts a hand the players will shuffle themselves.
///
/// The sibling of `start_hand_confidential`, and deliberately its own
/// instruction rather than a flag on it: which scheme deals a hand is decided
/// once, when the hand starts, and never changes under it.
///
/// The engine is put in `DeckSource::Mental` before `start_hand` runs, so its
/// three-seat minimum is enforced here rather than checked afterwards. Two
/// players have no board threshold that is both private and live, and the
/// refusal belongs at the door.
pub fn start_hand_mental(ctx: Context<StartMentalHand>) -> Result<()> {
    crate::tournament::accounts::require_recorded(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Start)?;
    let now = now_ms()?;

    let scheduled = ctx.accounts.table_state.next_hand_at;
    if scheduled != 0 {
        require!(now >= scheduled, RiverproofError::NextHandNotDue);
    }

    crate::tournament::accounts::apply_starting_blinds(&mut ctx.accounts.config, ctx.remaining_accounts)?;

    // Marked before loading so the engine comes up knowing it holds no deck.
    ctx.accounts.hand_cards.confidential = true;

    // The engine's `Table` is the better part of a kilobyte and a BPF frame is
    // four, so it is scoped: everything below needs only the positions and the
    // seats, and holding the whole table alive alongside the deal's arrays is
    // what overflows.
    let (positions, active_seats) = {
        let mut table = bridge::load(
            &ctx.accounts.config,
            &ctx.accounts.table_state,
            Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
        );
        table.deck_source = engine::DeckSource::Mental;
        table
            .start_hand(riverproof_engine::cards::ordered_deck(), now)
            .map_err(RiverproofError::from)?;

        let positions = table.deal_positions().ok_or(RiverproofError::NoActiveHand)?;
        let active = table
            .hand
            .as_ref()
            .ok_or(RiverproofError::NoActiveHand)?
            .active_seats;

        bridge::store_table(&table, &mut ctx.accounts.table_state, now);
        bridge::store_hand(
            &table,
            &mut ctx.accounts.hand_state,
            &mut ctx.accounts.hand_cards,
        );
        (positions, active)
    };

    let cards = &mut ctx.accounts.hand_cards;
    cards.revealed = 0;
    cards.confidential = true;
    for hole in cards.hole_cards.iter_mut() {
        *hole = [0u8; 2];
    }

    let hand_state = &mut ctx.accounts.hand_state;
    hand_state.settled = false;
    hand_state.winner_count = 0;
    hand_state.revealed_holes = 0;
    for winner in hand_state.winners.iter_mut() {
        *winner = crate::state::WinnerRecord::default();
    }
    let hand_id = hand_state.hand_id;

    initialize_deal(
        &mut ctx.accounts.deal,
        ctx.accounts.config.key(),
        hand_id,
        ctx.bumps.deal,
        &positions,
        active_seats,
        now,
    );

    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Resets the deal account for a fresh hand.
///
/// Its own function on purpose. BPF gives each call a four-kilobyte frame, and
/// the slot layout plus this account's arrays do not share one comfortably with
/// the engine's `Table` - so this gets a frame of its own rather than being
/// inlined into a caller that is already full.
#[inline(never)]
fn initialize_deal(
    deal: &mut MentalDeal,
    table: Pubkey,
    hand_id: u64,
    bump: u8,
    positions: &DealPositions,
    active_seats: u16,
    now: i64,
) {
    deal.table = table;
    deal.hand_id = hand_id;
    deal.bump = bump;
    deal.players = active_seats.count_ones() as u8;
    // Every player but one, matching `riverproof_mental::board_threshold`.
    // Written here rather than called, because this crate does not depend on
    // the protocol crate at this point — so the two must be changed together.
    deal.threshold = deal.players.saturating_sub(1);
    deal.next_round = 0;
    deal.deadline = now.saturating_add(DEAL_STALL_MS);
    deal.phase = DealPhase::Shuffling;
    deal.sealed_slots = 0;

    // Cleared in place rather than by assigning fresh arrays: an array literal
    // is built on the stack before it is moved into the account.
    for seat in deal.seats.iter_mut() {
        *seat = NO_SEAT;
    }
    for (index, seat) in riverproof_engine::pots::seats_of(active_seats).enumerate() {
        deal.seats[index] = seat;
    }
    for entry in deal.shuffle_commitments.iter_mut() {
        *entry = [0u8; 32];
    }
    for entry in deal.lock_commitments.iter_mut() {
        *entry = [0u8; 32];
    }
    for slot in deal.slots.iter_mut() {
        *slot = DealSlot::default();
    }

    let (layout, count) = slot_layout(positions);
    deal.slot_count = count as u8;
    for (index, (position, owner)) in layout.iter().take(count).enumerate() {
        deal.slots[index].position = *position;
        deal.slots[index].owner = *owner;
        deal.slots[index].card = NO_CARD;
    }
}

/// Records that a player published a round, without recording the round.
///
/// A shuffle round is 8,481 bytes and a lock round 7,589, per player — far past
/// what a transaction carries and pointless to store, because every player has
/// to verify them in full anyway. What the chain needs is that a player cannot
/// afterwards claim they published something else, and a digest is that.
///
/// Rounds are sequential because each one masks the deck the last one produced,
/// so `next_round` is the whole ordering rule.
pub fn commit_deal_round(
    ctx: Context<MentalPlay>,
    round: DealRound,
    digest: [u8; 32],
) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let player = ctx.accounts.authorized_player(clock.unix_timestamp)?;
    let deal = &mut ctx.accounts.deal;

    require!(digest != [0u8; 32], RiverproofError::EmptyRoundCommitment);
    require!(player == deal.next_round, RiverproofError::NotYourDealTurn);

    let expected = match round {
        DealRound::Shuffle => DealPhase::Shuffling,
        DealRound::Lock => DealPhase::Locking,
    };
    require!(deal.phase == expected, RiverproofError::WrongDealPhase);

    // A player commits each round once.
    //
    // `next_round` wraps to zero as the last round of a phase lands, and
    // locking stays in `Locking` until the deck is sealed — so the wrap put
    // player zero back on their own turn with their own commitment already
    // recorded, and a second `Lock` round from them was accepted. It
    // *overwrote* `lock_commitments[0]`, which is the Merkle root
    // `verify_unlock_share` checks proved hole shares against: a player could
    // replace the root every other player had already verified that round
    // against off chain. It also pushed `next_round` off zero, so
    // `seal_mental_deck` then refused the deck until everybody re-committed.
    //
    // A round that has already landed is not lost work, so a client whose
    // retry arrives second is refused and has nothing to do about it — the
    // same shape as `UnlockShareAlreadyGiven`, and it is listed beside it in
    // `EXPECTED_REFUSALS`.
    let committed = match round {
        DealRound::Shuffle => deal.shuffle_commitments[player as usize],
        DealRound::Lock => deal.lock_commitments[player as usize],
    };
    require!(committed == [0u8; 32], RiverproofError::RoundAlreadyCommitted);

    match round {
        DealRound::Shuffle => deal.shuffle_commitments[player as usize] = digest,
        DealRound::Lock => deal.lock_commitments[player as usize] = digest,
    }

    made_progress(deal, clock.unix_timestamp.saturating_mul(1_000));
    deal.next_round += 1;
    if deal.next_round == deal.players {
        deal.next_round = 0;
        // Shuffling ends where locking begins; locking ends at a sealed deck,
        // which arrives with the last locker in `seal_mental_deck`.
        if round == DealRound::Shuffle {
            deal.phase = DealPhase::Locking;
        }
    }
    Ok(())
}

/// Publishes the locked deck at the positions this hand will open.
///
/// Only the twenty-five dealt positions, in slot order. The burns are dealt
/// from and never opened, so putting them on chain would be paying rent to
/// store cards nobody may look at.
///
/// Sent by the player who locked last, because they are the one who produced
/// this deck: if it is wrong, the transcript and their own lock commitment say
/// so. Every other player has verified it off chain before any of them
/// publishes a share, and a wrong deck simply never opens.
pub fn seal_mental_deck(
    ctx: Context<MentalPlay>,
    offset: u8,
    locked: Vec<[u8; 32]>,
) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let player = ctx.accounts.authorized_player(clock.unix_timestamp)?;
    let deal = &mut ctx.accounts.deal;

    require!(deal.phase == DealPhase::Locking, RiverproofError::WrongDealPhase);
    // Every lock round is in, which is what resets `next_round` to zero.
    require!(deal.next_round == 0, RiverproofError::DealNotLocked);
    require!(player == deal.players - 1, RiverproofError::NotYourDealTurn);
    require!(!locked.is_empty(), RiverproofError::WrongDealSlotCount);

    // In order and contiguous. A chunk that could land anywhere would make a
    // half-sealed deck ambiguous, and `sealed_slots` is what says a resend is a
    // resend rather than a second helping.
    require!(offset == deal.sealed_slots, RiverproofError::WrongDealSlotCount);
    let end = offset as usize + locked.len();
    require!(end <= deal.slot_count as usize, RiverproofError::WrongDealSlotCount);

    for (slot, point) in deal.slots[offset as usize..end].iter_mut().zip(locked.iter()) {
        require!(*point != [0u8; 32], RiverproofError::InvalidUnlockShare);
        slot.point = *point;
        slot.unlocked = 0;
        slot.card = NO_CARD;
    }
    deal.sealed_slots = end as u8;
    made_progress(deal, clock.unix_timestamp.saturating_mul(1_000));

    // Nothing can be opened until the whole deck is down.
    if deal.sealed_slots == deal.slot_count {
        deal.phase = DealPhase::Sealed;
    }
    Ok(())
}

/// Whether a slot may be unlocked yet.
///
/// Hole cards, always: a player publishing their own share early only exposes
/// their own hand, which is theirs to do. Board cards, only while the engine is
/// parked for the street they belong to — otherwise everyone could publish
/// every board share during the deal and the board would be public before a
/// single bet.
fn slot_is_due(slot: &DealSlot, hand_state: &HandState, board_slots_from: usize, index: usize) -> bool {
    if slot.owner != NO_SEAT_OWNER {
        return true;
    }
    let board_index = index - board_slots_from;
    let dealt = hand_state.board_len as usize;
    let pending = match crate::bridge::awaiting_from_account(hand_state.awaiting) {
        Awaiting::None | Awaiting::Showdown => 0,
        other => other.card_count(hand_state.board_len),
    };
    board_index >= dealt && board_index < dealt + pending
}

/// How long an absent seat gets before the table may rebuild its board key.
///
/// The threshold is the security boundary, not this: only the absent seat, or
/// every other seat together, can produce a witness that survives
/// `verify_unlock_share`, and that coalition could already open a board card
/// early — `board_threshold` is `players - 1` and the docs say so. What the
/// grace protects is the *ordinary* case. Without it, `recover_unlocks` would
/// be the fastest way to open a board card rather than the last, and a seat
/// that was merely slow would find its key rebuilt every hand.
///
/// It is enforced here rather than in the clients because a client-side grace
/// binds only honest clients, and invariant 12 says cranks are clock-gated.
/// Measured against `deal.deadline`, which `made_progress` only ever extends —
/// so this is at least `RECOVERY_GRACE_MS` after the last thing that happened,
/// and longer while a betting allowance is still running, which errs the safe
/// way.
pub const RECOVERY_GRACE_MS: i64 = 20_000;

/// Puts an absent seat's layer on the board, without that seat.
///
/// The remedy for the one stall the rest of this file cannot fix. `Slot::is_open`
/// counts every player, so a board card locked under a seat that has stopped
/// answering can never open — and a folded seat still holds its layer, so the
/// betting-side timeouts do not help. Before this, the only way out was
/// `abandon_mental_deal` and a refund.
///
/// # Why this cannot open a hole card
///
/// `board` names cards by their offset within the board, `0..BOARD_SLOTS`, and
/// never by slot index. A hole card is therefore not expressible in this
/// instruction's argument space at all: there is no value of `board[i]` that
/// addresses one. That is deliberate, and it is stronger than a `require!` —
/// a check can be dropped in a refactor, an argument space cannot.
///
/// The cryptography agrees independently: `deal_shares` runs only over the
/// board positions, so no seat holds any share of anybody's hole key and there
/// is nothing to interpolate. Two barriers, neither relying on the other.
///
/// # What convinces the program
///
/// Nothing here counts shares, and it cannot: there is no scalar arithmetic on
/// chain and no reduce-mod-l syscall, so Lagrange interpolation cannot run in
/// the program. It does not need to. `verify_unlock_share` proves the submitted
/// share is the inverse of the key the *absent* seat committed, by opening
/// `k * G` from a Merkle path against `deal.lock_commitments[absent]`. Only
/// that seat, or a threshold of the others, can produce such a witness — so the
/// proof is the authorization, and it is a stronger boundary than a count.
#[inline(never)]
pub fn recover_unlocks(
    ctx: Context<MentalPlay>,
    absent: u8,
    board: Vec<u8>,
    shares: Vec<[u8; 32]>,
    keys: Vec<[u8; 32]>,
    paths: Vec<[u8; 32]>,
    round_digest: [u8; 32],
) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let player = ctx.accounts.authorized_player(clock.unix_timestamp)?;
    let now = clock.unix_timestamp.saturating_mul(1_000);

    // Shape first: nothing below means anything against ragged vectors.
    require!(!board.is_empty(), RiverproofError::WrongDealSlotCount);
    require!(
        board.len() <= crate::mental::BOARD_SLOTS,
        RiverproofError::WrongDealSlotCount
    );
    require!(board.len() == shares.len(), RiverproofError::WrongDealSlotCount);
    require!(board.len() == keys.len(), RiverproofError::WrongDealSlotCount);
    require!(
        paths.len() == keys.len() * crate::mental::MERKLE_DEPTH,
        RiverproofError::WrongDealSlotCount
    );

    let hand_state = ctx.accounts.hand_state.clone();
    let deal = &mut ctx.accounts.deal;

    require!(deal.phase == DealPhase::Sealed, RiverproofError::WrongDealPhase);
    require!(absent < deal.players, RiverproofError::UnknownDealPlayer);
    // Your own layer goes through `publish_unlocks`, which proves it is yours by
    // deriving you from the signer. This path is for the other case, and keeping
    // them apart keeps that guard unconditional.
    require!(absent != player, RiverproofError::CannotRecoverYourOwnKey);
    require!(
        now >= deal.deadline.saturating_sub(DEAL_STALL_MS).saturating_add(RECOVERY_GRACE_MS),
        RiverproofError::RecoveryNotDue
    );

    let commitment = deal.lock_commitments[absent as usize];
    require!(commitment != [0u8; 32], RiverproofError::DealNotLocked);
    require!(
        deal.slot_count as usize >= crate::mental::BOARD_SLOTS,
        RiverproofError::UnknownDealSlot
    );

    let players = deal.players;
    let board_from = deal.slot_count as usize - crate::mental::BOARD_SLOTS;

    for (offset, entry) in board.iter().enumerate() {
        let index = board_from
            .checked_add(*entry as usize)
            .ok_or(RiverproofError::UnknownDealSlot)?;
        require!(
            (*entry as usize) < crate::mental::BOARD_SLOTS,
            RiverproofError::UnknownDealSlot
        );

        let (due, deck_position) = {
            let slot = deal.slot_mut(index)?;
            // Belt to the argument space's braces. A board slot always has no
            // owner; if that ever stops being true this refuses rather than
            // recovering somebody's hole card.
            require!(slot.owner == NO_SEAT_OWNER, RiverproofError::UnknownDealSlot);
            (
                slot_is_due(slot, &hand_state, board_from, index),
                slot.position,
            )
        };
        require!(due, RiverproofError::BoardCardNotDue);

        let from = offset * crate::mental::MERKLE_DEPTH;
        let siblings: [[u8; 32]; crate::mental::MERKLE_DEPTH] = paths
            [from..from + crate::mental::MERKLE_DEPTH]
            .try_into()
            .map_err(|_| RiverproofError::WrongDealSlotCount)?;
        crate::mental::verify_unlock_share(
            &shares[offset],
            &keys[offset],
            deck_position,
            &siblings,
            &round_digest,
            &commitment,
        )?;

        let record = deal.slot_mut(index)?;
        let mut working = Slot {
            position: record.position,
            owner: record.owner,
            point: record.point,
            unlocked: record.unlocked,
            card: record.card,
        };
        // Attributed to the seat whose key it is, not to whoever sent it. That
        // is the whole point: `is_open` counts layers, and this is that seat's.
        unlock_slot(&mut working, absent, &shares[offset], players)?;
        record.point = working.point;
        record.unlocked = working.unlocked;
        record.card = working.card;
    }

    // Deliberately not recorded on the account, and the reason is deployment
    // rather than design. A `recovered` bitmask here would be the one
    // uninterested record that a seat stopped answering — a recovered layer is
    // otherwise identical to a self-published one — but `MentalDeal` is created
    // `init_if_needed` and reused for the life of a table, so a longer struct
    // fails to deserialize against every account already out there. Adding an
    // instruction does not break a live table; adding a field does. Worth doing
    // as its own change, with the table clear-out that implies.
    made_progress(deal, now);
    Ok(())
}

/// Multiplies this player's unlock shares into the slots they name.
///
/// Batched because a player owes a share for every other player's hole cards at
/// once — sixteen of them at a nine-handed table — and one transaction is
/// cheaper than sixteen.
/// Publishes this player's unlock shares.
///
/// `keys` and `paths` carry a proof for every slot that belongs to *another*
/// player's hole cards, in the order those slots appear in `slots`, and are
/// empty for a batch that has none.
///
/// Only those slots need one. A share is checked when the last layer comes off
/// and the point either is a card or is not, so a wrong share against a board
/// slot or your own hole slot fails in front of everybody and blames nobody.
/// Somebody else's hole slot is the exception: its owner's layer is last by
/// design, so a forgery sits unnoticed until showdown and it is then the owner
/// whose transaction fails and the owner whom `muck_unrevealed` folds for
/// declining to show. Those are the shares worth 224 bytes of proof.
pub fn publish_unlocks(
    ctx: Context<MentalPlay>,
    slots: Vec<u8>,
    shares: Vec<[u8; 32]>,
    keys: Vec<[u8; 32]>,
    paths: Vec<[u8; 32]>,
    round_digest: [u8; 32],
) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let player = ctx.accounts.authorized_player(clock.unix_timestamp)?;

    require!(slots.len() == shares.len(), RiverproofError::WrongDealSlotCount);
    require!(!slots.is_empty(), RiverproofError::WrongDealSlotCount);
    require!(
        paths.len() == keys.len() * crate::mental::MERKLE_DEPTH,
        RiverproofError::WrongDealSlotCount
    );

    let hand_state = ctx.accounts.hand_state.clone();
    let deal = &mut ctx.accounts.deal;
    require!(deal.phase == DealPhase::Sealed, RiverproofError::WrongDealPhase);

    let board_slots_from = deal.slot_count as usize - crate::mental::BOARD_SLOTS;
    let players = deal.players;
    let my_seat = deal.seats[player as usize];
    let commitment = deal.lock_commitments[player as usize];
    let mut proof = 0usize;

    for (index, share) in slots.iter().zip(shares.iter()) {
        let position = *index as usize;
        let (due, owner, deck_position) = {
            let slot = deal.slot_mut(position)?;
            (
                slot_is_due(slot, &hand_state, board_slots_from, position),
                slot.owner,
                slot.position,
            )
        };
        require!(due, RiverproofError::BoardCardNotDue);

        // Somebody else's hole card: prove the share before it is applied.
        if owner != NO_SEAT_OWNER && owner != my_seat {
            require!(proof < keys.len(), RiverproofError::WrongDealSlotCount);
            let from = proof * crate::mental::MERKLE_DEPTH;
            let siblings: [[u8; 32]; crate::mental::MERKLE_DEPTH] = paths
                [from..from + crate::mental::MERKLE_DEPTH]
                .try_into()
                .map_err(|_| RiverproofError::WrongDealSlotCount)?;
            crate::mental::verify_unlock_share(
                share,
                &keys[proof],
                deck_position,
                &siblings,
                &round_digest,
                &commitment,
            )?;
            proof += 1;
        }

        let record = deal.slot_mut(position)?;
        let mut working = Slot {
            position: record.position,
            owner: record.owner,
            point: record.point,
            unlocked: record.unlocked,
            card: record.card,
        };
        unlock_slot(&mut working, player, share, players)?;
        record.point = working.point;
        record.unlocked = working.unlocked;
        record.card = working.card;
    }
    require!(proof == keys.len(), RiverproofError::WrongDealSlotCount);
    made_progress(deal, clock.unix_timestamp.saturating_mul(1_000));

    // The deal is finished the moment every hole card can be read.
    //
    // A seat opens its own two cards once every *other* player has published a
    // share for them, so "dealt" is exactly this: no hole slot still waiting on
    // somebody. Only then does the betting open — before it, a table could bet
    // a whole preflop round holding nothing, and the deal's own stall deadline
    // was being spent on players thinking rather than on the deal.
    let board_from = deal.slot_count as usize - crate::mental::BOARD_SLOTS;
    let players = deal.players;
    let dealt = deal.slots[..board_from].iter().all(|slot| {
        // `owner` is a seat; `unlocked` is a mask indexed by player. The two
        // coincide only when the table happens to be filled from seat zero
        // upwards — which is exactly what the scripted three-handed test does,
        // so it never caught this. A real table seating players at 0, 4 and 5
        // computed the wrong mask and waited for a share nobody owed.
        let owner_player = deal.seats[..players as usize]
            .iter()
            .position(|seat| *seat == slot.owner);
        match owner_player {
            // Everyone but the owner: the owner's own layer is the one that
            // stays on the card until they choose to show it.
            Some(index) => {
                let owed = ((1u16 << players) - 1) & !(1u16 << index);
                slot.unlocked & owed == owed
            }
            // A hole slot whose owner is not in the deal is not a hole slot
            // this deal can finish.
            None => false,
        }
    });

    if dealt {
        let mut table = bridge::load(
            &ctx.accounts.config,
            &ctx.accounts.table_state,
            Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
        );
        // Idempotent by construction: `open_betting` refuses unless the hand is
        // parked on its deal, and every later unlock finds it already open.
        let now = now_ms()?;
        if table.open_betting(now).is_ok() {
            betting_reopened(&mut ctx.accounts.deal, now);
            bridge::store_table(&table, &mut ctx.accounts.table_state, now);
            bridge::store_hand(
                &table,
                &mut ctx.accounts.hand_state,
                &mut ctx.accounts.hand_cards,
            );
        }
    }
    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    Ok(())
}

/// Hands the street the engine is parked on to the engine.
///
/// Permissionless: the cards are already open on chain, so whoever sends this
/// is only paying the fee to move the hand along. The engine writes them into
/// the deck at the positions its normal dealing path is about to read and then
/// runs that path, exactly as the Arcium reveal does — street advancement is
/// not reimplemented for either scheme.
pub fn reveal_mental_street(ctx: Context<MentalPlay>) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let now = now_ms()?;

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );
    let (awaiting, count) = table.pending_reveal().ok_or(RiverproofError::NotAwaitingCards)?;
    require!(awaiting != Awaiting::Showdown, RiverproofError::NotAwaitingCards);

    let deal = &ctx.accounts.deal;
    let board_from = deal.slot_count as usize - crate::mental::BOARD_SLOTS;
    let dealt = ctx.accounts.hand_state.board_len as usize;

    let mut cards = Vec::with_capacity(count);
    for offset in 0..count {
        let slot = &deal.slots[board_from + dealt + offset];
        require!(slot.card != NO_CARD, RiverproofError::CardNotOpenYet);
        cards.push(slot.card);
    }

    table.supply_street(&cards, now).map_err(RiverproofError::from)?;

    // Betting reopens on the cards just supplied, unless the hand ran out to
    // showdown with everyone already all-in — in which case the deal is still
    // what the table is waiting on and the tight clock is right.
    let still_waiting = table
        .hand
        .as_ref()
        .is_some_and(|hand| hand.awaiting != Awaiting::None);
    if still_waiting {
        made_progress(&mut ctx.accounts.deal, now);
    } else {
        betting_reopened(&mut ctx.accounts.deal, now);
    }
    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(
        &table,
        &mut ctx.accounts.hand_state,
        &mut ctx.accounts.hand_cards,
    );
    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Hands one contender's hole cards to the engine at showdown.
///
/// Only reachable once that seat's own share is in, which only that seat can
/// publish. Refusing to publish it is refusing to show a hand, and poker has
/// always scored that as a fold — so there is nothing here to force, and
/// nothing anybody else can do about it.
pub fn reveal_mental_hole(ctx: Context<MentalPlay>, seat: u8) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let now = now_ms()?;

    // A seat shows once.
    //
    // This instruction is permissionless — anyone may turn a contender's cards
    // over once the keys are public, which is what stops a showdown resting on
    // the loser's cooperation. It also calls `made_progress`, and
    // `supply_hole_cards` was happy to accept the same seat twice: it rewrites
    // the cards, re-ors a bit that is already set, and returns `Ok`. So the
    // two together were a lever anyone could pull, seated or not, to push the
    // abandon deadline out by another sixty seconds for the price of one
    // transaction a minute — holding the showdown open indefinitely and
    // freezing every stack at the table, because `muck_unrevealed` can only run
    // once that deadline passes. The player it protected is precisely the one
    // the muck rule exists to fold: a contender who knows they are beaten.
    require!(
        ctx.accounts.hand_state.revealed_holes & (1u16 << seat) == 0,
        RiverproofError::HoleAlreadyRevealed
    );

    let deal = &ctx.accounts.deal;
    let mut cards = [NO_CARD; 2];
    let mut found = 0;
    for slot in deal.slots.iter().take(deal.slot_count as usize) {
        if slot.owner == seat {
            require!(slot.card != NO_CARD, RiverproofError::CardNotOpenYet);
            require!(found < 2, RiverproofError::UnknownDealSlot);
            cards[found] = slot.card;
            found += 1;
        }
    }
    require!(found == 2, RiverproofError::UnknownDealSlot);

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );
    table
        .supply_hole_cards(seat, cards, now)
        .map_err(RiverproofError::from)?;

    made_progress(&mut ctx.accounts.deal, now);
    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(
        &table,
        &mut ctx.accounts.hand_state,
        &mut ctx.accounts.hand_cards,
    );
    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Permissionless crank: unsticks a hand whose cards stopped arriving.
///
/// Every other way a hand can stall already has a remedy. A player who will not
/// bet is skipped by `force_timeout`. A player who will not open a board card
/// stalls it — `Slot::is_open` wants every player's layer and no instruction
/// can supply one on somebody else's behalf, so the threshold that could route
/// around them exists only in the off-chain protocol. What was left uncovered
/// is the case
/// where the *cards themselves* stop: a deal that never finishes, or a showdown
/// nobody settles. Neither has anybody on the clock and neither ever completes,
/// so `force_timeout` returns nothing to do and `start_next_hand` never fires.
/// The blinds sit in the pot and the table is finished forever.
///
/// The remedy depends on what anyone could already know, which is what keeps it
/// from being abusable:
///
/// * **At a showdown**, whoever declined to show is mucked and the pot goes to
///   whoever did. That is poker's own rule, and it means stalling at the one
///   point where a player knows they are beaten gains them nothing.
/// * **Anywhere earlier**, the hand is void and every chip goes back. Safe
///   because a deal finishes before any player can read their own cards, so a
///   player stalling before that is stalling in ignorance - there is nothing yet
///   to run from.
/// Which seats owed this deal something and did not provide it.
///
/// A hand is abandoned because cards stopped arriving, and somebody stopped
/// sending them. Naming that seat is what lets the table recover: it is marked
/// `timed_out`, `complete_hand` un-readies every seat a hand found absent, and
/// `bridge::store_table` stamps the takeover clock through `set_ready`. Two
/// minutes later anyone at the table can free the chair with `kick_idle`, which
/// refunds the stack in full.
///
/// Before this, nothing in the abandon path dropped `ready` from anyone. A
/// player who closed their browser while readied — or an agent whose process
/// died — left a seat that counted toward `ready_count`, made the table offer
/// to deal, stalled every deal it started, got the hand voided, and then did it
/// again for ever. There was no instruction that could free it, because
/// `kick_idle` refuses a ready seat and `set_ready` zeroes the very clock it
/// checks. The engine has a `!seat.connected` branch meant for exactly this and
/// it has never once run: nothing in the program sets `connected` false.
///
/// Its own frame: `abandon_mental_deal` already holds the engine's `Table`, and
/// this walks every slot.
#[inline(never)]
fn stalled_seats(deal: &MentalDeal, hand_state: &HandState) -> u16 {
    let players = deal.players;
    if players == 0 || players as usize > riverproof_engine::MAX_SEATS {
        return 0;
    }
    let seat_of = |player: u8| -> u16 {
        match deal.seats.get(player as usize) {
            Some(&seat) if (seat as usize) < riverproof_engine::MAX_SEATS => 1u16 << seat,
            _ => 0,
        }
    };
    let all = ((1u32 << players) - 1) as u16;

    match deal.phase {
        // Rounds are sequential — `commit_deal_round` refuses any player but
        // `next_round` — so exactly one player is ever owing one.
        DealPhase::Shuffling => seat_of(deal.next_round),
        // The same, with one ambiguity at the end: `next_round` wraps to zero
        // when the last lock round lands, and the phase stays `Locking` until
        // the deck is sealed. `lock_commitments[0]` tells the two apart. It is
        // empty when player zero still owes the first lock round, and set once
        // they have committed it — after the wrap what is owed is the sealed
        // deck, which `seal_mental_deck` takes only from the last locker.
        DealPhase::Locking => {
            if deal.next_round == 0 && deal.lock_commitments[0] != [0u8; 32] {
                seat_of(players.saturating_sub(1))
            } else {
                seat_of(deal.next_round)
            }
        }
        // Past the rounds it is per slot and per player. `owner` is a seat
        // index and `unlocked` is a mask indexed by player; conflating the two
        // is the bug `publish_unlocks` already carries a comment about, where a
        // table seated at 0, 4 and 5 waited on a share nobody owed.
        DealPhase::Sealed => {
            let board_from = (deal.slot_count as usize).saturating_sub(crate::mental::BOARD_SLOTS);
            let mut stalled = 0u16;
            for index in 0..deal.slot_count as usize {
                let slot = &deal.slots[index];
                if !slot_is_due(slot, hand_state, board_from, index) {
                    continue;
                }
                let owed = if slot.owner == NO_SEAT_OWNER {
                    all
                } else {
                    // Everybody but the owner. Their own layer stays on until
                    // they show, and refusing to show is `muck_unrevealed`'s
                    // business rather than this function's — blaming them here
                    // would name every contender during an ordinary deal.
                    match deal.seats[..players as usize].iter().position(|s| *s == slot.owner) {
                        Some(owner) => all & !(1u16 << owner),
                        None => 0,
                    }
                };
                let missing = owed & !slot.unlocked;
                for player in 0..players {
                    if missing & (1u16 << player) != 0 {
                        stalled |= seat_of(player);
                    }
                }
            }
            stalled
        }
    }
}

pub fn abandon_mental_deal(ctx: Context<MentalPlay>) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let now = now_ms()?;
    require!(now >= ctx.accounts.deal.deadline, RiverproofError::TimerNotExpired);

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );

    // Only a hand that is actually waiting on cards. `Awaiting::None` is a
    // betting round, where nothing is owed by the deal and the remedy for a
    // slow player is `force_timeout` — the clock that has somebody's name on
    // it. Without this the deal's own stall deadline doubled as a betting
    // deadline, and any seated player, including one who was losing, could void
    // a live hand back to everyone's contributions.
    let awaiting = table
        .hand
        .as_ref()
        .map(|hand| hand.awaiting)
        .unwrap_or(Awaiting::None);
    require!(awaiting != Awaiting::None, RiverproofError::NotAwaitingCards);

    // A street that has only just parked is not a stalled deal.
    //
    // `deal.deadline` runs unattended through a betting round — the `Play`
    // context carries no deal account, so nothing about betting can touch it —
    // and a betting round has no bound: a full raise resets `acted`, so every
    // seat must act again. `betting_reopened` budgets one action timer per
    // seat, which a re-raise war outruns comfortably. That left the clock
    // already expired at the instant the next street parked, and the player
    // who closed the round could void the hand in the *same transaction*
    // — `[act, abandon_mental_deal]` — before anybody had a slot in which to
    // publish a share. A losing player got their whole contribution back, for
    // nothing, at a moment of their own choosing.
    //
    // Nothing published against the entire street means nobody has had their
    // chance yet, so this is not silence, it is the street beginning. Restart
    // the clock and refuse. Sixty seconds later the same call either finds
    // shares, or finds a deal that really has stopped.
    //
    // Once per street, and the "once" is the whole of it. This used to restart
    // the clock on every call with nothing to distinguish the second from the
    // first, so sixty seconds later it simply found the same empty street and
    // granted the same grace again — for ever. The one case it could never
    // reach was the one it was written for: a street nobody ever publishes to,
    // which is every player closing their tab, and which is unrecoverable
    // because the per-hand seed lives in `sessionStorage` and is not a function
    // of anything they can sign. The pot and every stack behind it stayed in
    // the vault with no instruction in this program able to release them —
    // `force_timeout` no-ops with nobody on the clock, `start_next_hand` needs
    // a completed hand, and `leave_table` and `kick_idle` both refuse a live
    // one. The instruction that exists to break exactly that deadlock was the
    // one thing holding it shut.
    //
    // The marker is `next_round`, which is dead once the deck is sealed: rounds
    // are over, `commit_deal_round` refuses to run outside `Shuffling` and
    // `Locking`, and `stalled_seats` reads it only in those two phases. It
    // holds `board_len + 1` for the street that has had its grace, so zero — the
    // value `initialize_deal` leaves and the value it wraps to — means none has.
    // Reusing it rather than adding a field is deliberate: `MentalDeal` is
    // created `init_if_needed` and Anchor does not resize an account that
    // already exists, so a longer struct would fail to deserialize at every
    // table that has ever dealt a hand, which is a worse failure than the bug.
    if ctx.accounts.deal.phase == DealPhase::Sealed && ctx.accounts.deal.slot_count >= 5 {
        let hand_state = ctx.accounts.hand_state.clone();
        let deal = &ctx.accounts.deal;
        let board_from = deal.slot_count as usize - crate::mental::BOARD_SLOTS;
        let mut due_slots = false;
        let mut anyone_published = false;
        for index in board_from..deal.slot_count as usize {
            let slot = &deal.slots[index];
            if !slot_is_due(slot, &hand_state, board_from, index) {
                continue;
            }
            due_slots = true;
            if slot.unlocked != 0 {
                anyone_published = true;
                break;
            }
        }
        if due_slots && !anyone_published {
            let street = hand_state.board_len.saturating_add(1);
            if ctx.accounts.deal.next_round != street {
                ctx.accounts.deal.next_round = street;
                made_progress(&mut ctx.accounts.deal, now);
                return Ok(());
            }
            // Its grace is spent. Sixty more seconds have passed with nothing
            // published against this street, so it really has stopped.
        }
    }

    let at_showdown = matches!(
        table.hand.as_ref().map(|hand| hand.awaiting),
        Some(Awaiting::Showdown)
    );

    // Whoever stopped answering stops being ready for the next hand.
    //
    // Marked on the hand rather than on the seats directly, because
    // `complete_hand` is already the one place a seat this hand found absent
    // loses `ready`, and it is reached by both branches below. At a showdown
    // the delinquents are the contenders who would not show, which
    // `muck_unrevealed` folds anyway; everywhere else they are the seats that
    // owed a round or a share and never sent it.
    if let Some(hand) = table.hand.as_mut() {
        hand.timed_out |= if at_showdown {
            hand.live_seats() & !hand.revealed_holes
        } else {
            stalled_seats(&ctx.accounts.deal, &ctx.accounts.hand_state)
        };
    }

    if at_showdown {
        table.muck_unrevealed(now).map_err(RiverproofError::from)?;
    } else {
        table.void_hand(now).map_err(RiverproofError::from)?;
    }

    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(
        &table,
        &mut ctx.accounts.hand_state,
        &mut ctx.accounts.hand_cards,
    );
    ctx.accounts.hand_state.settled = true;

    // The deal is finished with either way, and leaving it addressable would
    // let a share arrive for a hand that no longer exists.
    let deal = &mut ctx.accounts.deal;
    deal.phase = DealPhase::Shuffling;
    deal.slot_count = 0;
    deal.sealed_slots = 0;

    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use riverproof_engine::table::{Seat, Table, TableConfig as EngineConfig};

    /// The slot order is the order every party indexes by, and it has to match
    /// what the engine actually dealt or a player peels the wrong card.
    #[test]
    fn slot_layout_covers_every_dealt_position_once() {
        for players in 3..=riverproof_engine::MAX_SEATS {
            let mut table = Table::new(EngineConfig::new(50, 0, players as u8, 5_000, 30_000));
            for seat in table.seats.iter_mut().take(players) {
                *seat = Seat {
                    occupied: true,
                    stack: 5_000,
                    ready: true,
                    connected: true,
                    leave_after_hand: false,
                };
            }
            table.deck_source = engine::DeckSource::Mental;
            table.start_hand(riverproof_engine::cards::ordered_deck(), 0).expect("a hand");

            let positions = table.deal_positions().expect("positions");
            let (layout, count) = slot_layout(&positions);
            assert_eq!(count, players * 2 + 5, "{players} players: slot count");

            // Same order as the engine's own, which is what the client mirrors.
            let dealt: Vec<u8> = positions.dealt().collect();
            let ours: Vec<u8> = layout.iter().take(count).map(|(position, _)| *position).collect();
            assert_eq!(ours, dealt, "{players} players: slot order");

            // Every position once, the last five owned by nobody.
            let mut sorted = ours.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), count, "{players} players: positions collide");
            for (_, owner) in layout.iter().take(count).skip(players * 2) {
                assert_eq!(*owner, NO_SEAT_OWNER, "{players} players: a board card has an owner");
            }
            for (index, (_, owner)) in layout.iter().take(players * 2).enumerate() {
                assert!(*owner < players as u8, "{players} players: hole slot {index} owner");
            }
        }
    }

    /// A board share published before its street would put the board on chain
    /// before anybody had bet on it.
    #[test]
    fn board_slots_are_only_due_on_their_own_street() {
        let mut hand = HandState {
            table: Pubkey::default(),
            hand_id: 1,
            stage: crate::state::HandStage::Preflop,
            version: 0,
            dealer_seat: 0,
            small_blind_seat: 0,
            big_blind_seat: 0,
            active_seats: 0b111,
            folded: 0,
            all_in: 0,
            acted: 0,
            street_bets: [0; riverproof_engine::MAX_SEATS],
            contributions: [0; riverproof_engine::MAX_SEATS],
            current_bet: 0,
            min_raise: 0,
            actor_seat: NO_SEAT,
            action_deadline: 0,
            board: [0; 5],
            board_len: 0,
            deck_index: 6,
            settled: false,
            awaiting: 0,
            revealed_holes: 0,
            timed_out: 0,
            winners: [crate::state::WinnerRecord::default(); riverproof_engine::MAX_SEATS],
            winner_count: 0,
            bump: 0,
        };

        let board = DealSlot { position: 7, owner: NO_SEAT_OWNER, ..DealSlot::default() };
        let hole = DealSlot { position: 0, owner: 1, ..DealSlot::default() };
        let from = 6; // three players: six hole slots, then five board slots.

        // Nothing pending: no board card is due, but a hole card always is.
        for offset in 0..5 {
            assert!(!slot_is_due(&board, &hand, from, from + offset), "board {offset} before the flop");
        }
        assert!(slot_is_due(&hole, &hand, from, 0));

        // Parked for the flop: the first three, and only those.
        hand.awaiting = 1;
        assert!(slot_is_due(&board, &hand, from, from));
        assert!(slot_is_due(&board, &hand, from, from + 2));
        assert!(!slot_is_due(&board, &hand, from, from + 3), "the turn came due with the flop");

        // Parked for the turn, with the flop already out.
        hand.awaiting = 2;
        hand.board_len = 3;
        assert!(!slot_is_due(&board, &hand, from, from + 2), "a dealt flop card came due again");
        assert!(slot_is_due(&board, &hand, from, from + 3));
        assert!(!slot_is_due(&board, &hand, from, from + 4), "the river came due with the turn");

        // An all-in run-out asks for everything still owed, at once.
        hand.awaiting = 4;
        assert!(slot_is_due(&board, &hand, from, from + 3));
        assert!(slot_is_due(&board, &hand, from, from + 4));
        assert!(!slot_is_due(&board, &hand, from, from + 2));

        // At showdown the board is finished and nothing more is due.
        hand.awaiting = 5;
        hand.board_len = 5;
        for offset in 0..5 {
            assert!(!slot_is_due(&board, &hand, from, from + offset), "board {offset} at showdown");
        }
    }
}
