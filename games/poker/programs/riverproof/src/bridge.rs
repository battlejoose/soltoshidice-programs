//! Conversion between account state and the pure rules engine.
//!
//! Instructions load the engine's view of a table, run one operation, then write
//! the result back. Keeping the engine free of Anchor types is what lets it be
//! differentially tested against the JavaScript oracle natively.

use anchor_lang::prelude::*;
use riverproof_engine::table as engine;
use riverproof_engine::DECK_SIZE;

use crate::state::{
    HandCards, HandStage, HandState, TableConfig, TableState, WinnerRecord,
};

/// `Awaiting` as stored on `HandState`.
pub fn awaiting_to_account(awaiting: engine::Awaiting) -> u8 {
    match awaiting {
        engine::Awaiting::None => 0,
        engine::Awaiting::Flop => 1,
        engine::Awaiting::Turn => 2,
        engine::Awaiting::River => 3,
        engine::Awaiting::RunOut => 4,
        engine::Awaiting::Showdown => 5,
        engine::Awaiting::Deal => 6,
    }
}

pub fn awaiting_from_account(value: u8) -> engine::Awaiting {
    match value {
        1 => engine::Awaiting::Flop,
        2 => engine::Awaiting::Turn,
        3 => engine::Awaiting::River,
        4 => engine::Awaiting::RunOut,
        5 => engine::Awaiting::Showdown,
        6 => engine::Awaiting::Deal,
        _ => engine::Awaiting::None,
    }
}

pub const NO_SEAT: u8 = u8::MAX;
pub const NO_CATEGORY: u8 = u8::MAX;

pub fn stage_to_account(stage: engine::Stage) -> HandStage {
    match stage {
        engine::Stage::Preflop => HandStage::Preflop,
        engine::Stage::Flop => HandStage::Flop,
        engine::Stage::Turn => HandStage::Turn,
        engine::Stage::River => HandStage::River,
        engine::Stage::Complete => HandStage::Complete,
    }
}

pub fn stage_from_account(stage: HandStage) -> Option<engine::Stage> {
    match stage {
        HandStage::Idle => None,
        HandStage::Preflop => Some(engine::Stage::Preflop),
        HandStage::Flop => Some(engine::Stage::Flop),
        HandStage::Turn => Some(engine::Stage::Turn),
        HandStage::River => Some(engine::Stage::River),
        HandStage::Complete => Some(engine::Stage::Complete),
    }
}

pub fn engine_config(config: &TableConfig) -> engine::TableConfig {
    engine::TableConfig {
        small_blind: config.small_blind,
        big_blind: config.big_blind,
        ante: config.ante,
        max_players: config.max_players,
        buy_in: config.buy_in,
        action_time_ms: crate::rules::ACTION_TIME_MS,
    }
}

/// Builds the engine's view from the account trio.
///
/// `hand` is `None` between hands; otherwise both the betting state and the card
/// material are required, because the engine deals from the deck as streets run
/// out.
pub fn load(
    config: &TableConfig,
    state: &TableState,
    hand: Option<(&HandState, &HandCards)>,
) -> engine::Table {
    let mut table = engine::Table::new(engine_config(config));
    if let Some((_, cards)) = hand {
        if cards.confidential {
            table.deck_source = engine::DeckSource::Mental;
        }
    }

    for (index, record) in state.seats.iter().enumerate() {
        table.seats[index] = engine::Seat {
            occupied: record.occupied,
            stack: record.stack,
            ready: record.ready,
            // The legacy connected byte was always true for occupied native
            // seats. It now records manual-play=true / auto-check-fold=false.
            // Readiness still controls participation in encrypted dealing.
            connected: true,
            leave_after_hand: record.leave_after_hand,
        };
        if record.occupied && !record.connected {
            table.auto_check_fold |= 1u16 << index;
        }
    }
    table.dealer_seat = (state.dealer_seat != NO_SEAT).then_some(state.dealer_seat);
    table.hand_counter = state.hand_counter;
    table.next_hand_at = (state.next_hand_at != 0).then_some(state.next_hand_at);
    table.version = state.version;

    if let Some((hand_state, cards)) = hand {
        if let Some(stage) = stage_from_account(hand_state.stage) {
            table.hand = Some(engine::Hand {
                id: hand_state.hand_id,
                stage,
                version: hand_state.version,
                dealer_seat: hand_state.dealer_seat,
                small_blind_seat: hand_state.small_blind_seat,
                big_blind_seat: hand_state.big_blind_seat,
                active_seats: hand_state.active_seats,
                // Scratch only. Revealed cards are written into it by
                // `supply_street` and consumed by the dealing path inside the
                // same instruction; nothing about the deck is persisted.
                deck: [0u8; DECK_SIZE],
                deck_index: hand_state.deck_index,
                board: hand_state.board,
                board_len: hand_state.board_len,
                hole_cards: cards.hole_cards,
                street_bets: hand_state.street_bets,
                contributions: hand_state.contributions,
                folded: hand_state.folded,
                all_in: hand_state.all_in,
                acted: hand_state.acted,
                current_bet: hand_state.current_bet,
                min_raise: hand_state.min_raise,
                actor_seat: (hand_state.actor_seat != NO_SEAT).then_some(hand_state.actor_seat),
                action_deadline: hand_state.action_deadline,
                awaiting: awaiting_from_account(hand_state.awaiting),
                revealed_holes: hand_state.revealed_holes,
                timed_out: hand_state.timed_out,
                // Results are written, never read back: a completed hand's
                // winners are recorded on `HandState` in the same instruction
                // that settles them.
                result: None,
            });
        }
    }

    table
}

/// Writes seat stacks, the button, and hand scheduling back to the table.
///
/// `now` is only for the seat clock: `SeatRecord::set_ready` stamps when a seat
/// stopped being ready, which is what the takeover grace period is measured
/// from. The engine drops `ready` on its own for a bust, a disconnect, or a
/// player skipped by the timeout crank, and those are exactly the seats a
/// takeover is for — so the stamp has to happen here rather than only in the
/// instructions that set readiness explicitly.
pub fn store_table(table: &engine::Table, state: &mut TableState, now: i64) {
    for (index, seat) in table.seats.iter().enumerate() {
        let record = &mut state.seats[index];
        record.occupied = seat.occupied;
        record.stack = seat.stack;
        record.set_ready(seat.ready, now);
        record.connected = table.auto_check_fold & (1u16 << index) == 0;
        record.leave_after_hand = seat.leave_after_hand;
        if !seat.occupied {
            record.owner = Pubkey::default();
            record.sitting_out_since = 0;
        }
    }
    state.dealer_seat = table.dealer_seat.unwrap_or(NO_SEAT);
    state.hand_counter = table.hand_counter;
    state.next_hand_at = table.next_hand_at.unwrap_or(0);
    state.version = table.version;
}

/// Writes the betting state and card material back.
pub fn store_hand(table: &engine::Table, hand_state: &mut HandState, cards: &mut HandCards) {
    let hand = match table.hand.as_ref() {
        Some(hand) => hand,
        None => {
            hand_state.stage = HandStage::Idle;
            return;
        }
    };

    hand_state.hand_id = hand.id;
    hand_state.stage = stage_to_account(hand.stage);
    hand_state.version = hand.version;
    hand_state.dealer_seat = hand.dealer_seat;
    hand_state.small_blind_seat = hand.small_blind_seat;
    hand_state.big_blind_seat = hand.big_blind_seat;
    hand_state.active_seats = hand.active_seats;
    hand_state.folded = hand.folded;
    hand_state.all_in = hand.all_in;
    hand_state.acted = hand.acted;
    hand_state.street_bets = hand.street_bets;
    hand_state.contributions = hand.contributions;
    hand_state.current_bet = hand.current_bet;
    hand_state.min_raise = hand.min_raise;
    hand_state.actor_seat = hand.actor_seat.unwrap_or(NO_SEAT);
    hand_state.action_deadline = hand.action_deadline;
    hand_state.board = hand.board;
    hand_state.board_len = hand.board_len;
    hand_state.deck_index = hand.deck_index;
    hand_state.awaiting = awaiting_to_account(hand.awaiting);
    hand_state.revealed_holes = hand.revealed_holes;
    hand_state.timed_out = hand.timed_out;

    cards.hand_id = hand.id;
    cards.hole_cards = hand.hole_cards;

    if let Some(result) = hand.result.as_ref() {
        hand_state.winners = [WinnerRecord::default(); riverproof_engine::MAX_SEATS];
        for (index, winner) in result.winners().iter().enumerate() {
            hand_state.winners[index] = WinnerRecord {
                seat: winner.seat,
                amount: winner.amount,
                category: winner.category.unwrap_or(NO_CATEGORY),
            };
        }
        hand_state.winner_count = result.winners().len() as u8;
        hand_state.settled = true;
        // Showdown makes every live contender's cards public.
        cards.revealed = hand.live_seats();
    }
}
