//! Table and hand state machine.
//!
//! A port of the betting, street advancement, and settlement logic in
//! `server/poker-engine.mjs`. Behaviour must match the oracle exactly; the
//! differential tests in `tests/differential.rs` assert that against recorded
//! trace vectors.
//!
//! Two deliberate differences from the JavaScript, both forced by running
//! on-chain:
//!
//! * The deck is an input to [`Table::start_hand`] rather than something the
//!   engine shuffles. On-chain the ordering arrives from the Arcium MPC cluster.
//! * There is no tick loop. [`Table::force_timeout`] and [`Table::start_next_hand`]
//!   are permissionless cranks validated against the clock by the caller.

use crate::cards::DECK_SIZE;
use crate::eval::{evaluate, Score};
use crate::pots::seats_of;

pub const MAX_SEATS: usize = 10;

/// Fewest seats a [`DeckSource::Mental`] hand may be dealt to.
///
/// Board cards open on a threshold of the players and nobody else, so a table
/// of two has no workable threshold: 1 lets either player open the board early,
/// 2 lets either stall it forever. Three is the first size where a strict
/// majority is neither.
pub const MENTAL_MIN_SEATS: u32 = 3;

/// Grace period on a stale action, matching the oracle's one-second slack.
pub const ACTION_GRACE_MS: i64 = 1_000;
/// Delay before a completed hand clears, matching the oracle.
pub const HAND_COMPLETE_DELAY_MS: i64 = 6_000;
/// Delay before a fresh hand starts once two players are ready.
pub const NEXT_HAND_DELAY_MS: i64 = 2_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineError {
    NoActiveHand,
    WrongHand,
    StaleVersion,
    NotYourTurn,
    ActionExpired,
    CannotCheckFacingBet,
    NoBetToCall,
    RaiseNotReopened,
    RaiseUnavailable,
    RaiseBelowMinimum,
    RaiseExceedsStack,
    RaiseNotAboveCurrentBet,
    NotEnoughPlayers,
    /// A mental-poker hand was started with fewer than [`MENTAL_MIN_SEATS`].
    NotEnoughPlayersForMentalDeal,
    HandInProgress,
    SeatEmpty,
    NotAwaitingCards,
    WrongCardCount,
}

/// Where board cards come from.
///
/// `Local` is the synchronous path: the whole deck is known up front and
/// streets are dealt inline. Every differential test against the JavaScript
/// oracle runs in this mode, so it must stay byte-identical. Nothing on chain
/// uses it — no instruction can supply a deck — but it is what makes the rules
/// testable without a dealing scheme attached, and it is the baseline the
/// confidential path is checked against.
///
/// `Mental` is what ships. The players shuffle the deck themselves, under
/// commutative encryption, and open cards by publishing key material; the
/// program never holds a deck, so a completed betting round cannot deal itself.
/// The engine parks in [`Awaiting`] and resumes when [`Table::supply_street`]
/// or [`Table::supply_hole_cards`] delivers what the chain opened.
///
/// The one rule the engine owns here is [`MENTAL_MIN_SEATS`]: with two
/// participants a board threshold is either 1, so either player peeks early, or
/// 2, so either can stall. That is arithmetic rather than a parameter, so
/// heads-up is refused at the door.
///
/// There was a third source, `Mpc`, backed by an Arcium cluster. It worked and
/// it dealt heads-up, which this cannot — but it cost about 0.005 SOL per
/// computation against a floor of five a hand, which is dollars a hand, and it
/// was removed rather than kept warm. `docs/DEALING.md` has the measurements.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum DeckSource {
    #[default]
    Local,
    Mental,
}

impl DeckSource {
    /// Whether the engine holds the deck. When it does not, a finished betting
    /// round parks instead of dealing itself.
    pub fn is_confidential(self) -> bool {
        !matches!(self, DeckSource::Local)
    }
}

/// Which reveal the hand is parked waiting for, if any.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Awaiting {
    #[default]
    None,
    Flop,
    Turn,
    River,
    /// Everyone is all-in: the rest of the board is revealed in one go.
    RunOut,
    /// The board is complete and betting is over, but the contenders' hole
    /// cards are still sealed and evaluation cannot run without them.
    Showdown,
    /// Blinds are posted but nobody holds a card yet.
    ///
    /// Only a player-shuffled deal parks here. The players build the deck
    /// between themselves after the hand starts, which takes tens of seconds
    /// and dozens of transactions, and until it finishes there is nothing to
    /// bet on. Opening the betting first let a table play a whole preflop round
    /// blind, and spent the deal's own stall deadline on human thinking time —
    /// so a hand that was merely being played slowly was abandoned as stuck.
    Deal,
}

impl Awaiting {
    /// How many board cards the pending reveal must deliver.
    pub fn card_count(self, board_len: u8) -> usize {
        match self {
            Awaiting::None => 0,
            Awaiting::Flop => 3,
            Awaiting::Turn | Awaiting::River => 1,
            Awaiting::RunOut => 5usize.saturating_sub(board_len as usize),
            // Neither of these is waiting on a board card. Showdown wants hole
            // cards; Deal wants the whole deck, and asks for it through the
            // deal account rather than through a reveal.
            Awaiting::Showdown | Awaiting::Deal => 0,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Stage {
    Preflop,
    Flop,
    Turn,
    River,
    Complete,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Fold,
    Check,
    Call,
    AllIn,
    Raise { to: u64 },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ResultKind {
    Fold,
    Showdown,
    /// Nobody could finish the hand, so every chip went back.
    ///
    /// Only reachable when the *cards* stall rather than the betting: a
    /// player-shuffled deal that never completed, or a showdown nobody showed
    /// for. Never carried on chain - `HandState` records winners, and a voided
    /// hand has none.
    Voided,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Winner {
    pub seat: u8,
    pub amount: u64,
    /// Hand category index, absent when the pot was won uncontested.
    pub category: Option<u8>,
}

#[derive(Clone, Copy, Debug)]
pub struct HandResult {
    pub kind: ResultKind,
    winners: [Winner; MAX_SEATS],
    winner_count: usize,
}

impl HandResult {
    pub fn winners(&self) -> &[Winner] {
        &self.winners[..self.winner_count]
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Seat {
    pub occupied: bool,
    pub stack: u64,
    pub ready: bool,
    pub connected: bool,
    pub leave_after_hand: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TableConfig {
    pub small_blind: u64,
    pub big_blind: u64,
    pub ante: u64,
    pub max_players: u8,
    pub buy_in: u64,
    pub action_time_ms: i64,
}

impl TableConfig {
    /// Mirrors the normalization `createTable` applies to its arguments.
    pub fn new(small_blind: u64, ante: u64, max_players: u8, buy_in: u64, action_time_ms: i64) -> Self {
        let small_blind = small_blind.max(1);
        let max_players = max_players.clamp(2, MAX_SEATS as u8);
        Self {
            small_blind,
            big_blind: small_blind * 2,
            ante: ante.min(small_blind),
            max_players,
            buy_in: buy_in.max(small_blind * 40),
            action_time_ms,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Hand {
    pub id: u64,
    pub stage: Stage,
    pub version: u32,
    pub dealer_seat: u8,
    pub small_blind_seat: u8,
    pub big_blind_seat: u8,
    pub active_seats: u16,
    pub deck: [u8; DECK_SIZE],
    pub deck_index: u8,
    pub board: [u8; 5],
    pub board_len: u8,
    pub hole_cards: [[u8; 2]; MAX_SEATS],
    pub street_bets: [u64; MAX_SEATS],
    pub contributions: [u64; MAX_SEATS],
    pub folded: u16,
    pub all_in: u16,
    pub acted: u16,
    pub current_bet: u64,
    pub min_raise: u64,
    pub actor_seat: Option<u8>,
    pub action_deadline: i64,
    /// Non-`None` only in [`DeckSource::Mental`], while parked for a reveal.
    pub awaiting: Awaiting,
    /// Seats whose hole cards are known in the clear. Always the full set of
    /// contenders under [`DeckSource::Local`]; filled by `supply_hole_cards`
    /// under [`DeckSource::Mental`].
    pub revealed_holes: u16,
    /// Seats that were skipped by [`Table::force_timeout`] during this hand.
    ///
    /// Kept per hand rather than per seat because it answers a per-hand
    /// question: was this player at the keyboard *this* time. At
    /// [`Table::complete_hand`] every seat in here is un-readied, so a player
    /// who stopped answering sits out the next hand instead of being dealt in
    /// and skipped again — which is what a table full of absent players used
    /// to look like, one timeout per seat per hand, forever.
    pub timed_out: u16,
    pub result: Option<HandResult>,
}

impl Hand {
    pub fn board(&self) -> &[u8] {
        &self.board[..self.board_len as usize]
    }

    pub fn pot(&self) -> u64 {
        self.contributions.iter().sum()
    }

    /// Seats dealt into the hand that have not folded.
    pub fn live_seats(&self) -> u16 {
        self.active_seats & !self.folded
    }

    /// Live seats that can still put chips in.
    pub fn actionable_seats(&self) -> u16 {
        self.live_seats() & !self.all_in
    }

    fn needs_action(&self, seat: u8) -> bool {
        let bit = 1u16 << seat;
        self.active_seats & bit != 0
            && self.folded & bit == 0
            && self.all_in & bit == 0
            && (self.acted & bit == 0 || self.street_bets[seat as usize] != self.current_bet)
    }
}

/// Sentinel in [`DealPositions::hole`] for a seat that is not in the hand.
pub const NO_POSITION: u8 = u8::MAX;

/// The deck positions a hand draws from, in the order [`Table::start_hand`]
/// draws them.
///
/// Under [`DeckSource::Local`] this is an implementation detail. Under a
/// confidential source it is the interface: a dealer that never holds the deck
/// still has to be told which position is whose hole card and which five are
/// the board, and every party has to agree. Deriving it here rather than in the
/// caller keeps one implementation of the arithmetic that `start_hand` and
/// `burn_and_deal` already encode.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DealPositions {
    /// Each seat's two positions, or [`NO_POSITION`] if it is not in the hand.
    pub hole: [[u8; 2]; MAX_SEATS],
    /// The five board positions. Burns are skipped, never listed.
    pub board: [u8; 5],
}

impl DealPositions {
    /// Every position a card is actually dealt from, burns excluded.
    pub fn dealt(&self) -> impl Iterator<Item = u8> + '_ {
        self.hole
            .iter()
            .flatten()
            .copied()
            .filter(|position| *position != NO_POSITION)
            .chain(self.board.iter().copied())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LegalActions {
    pub to_call: u64,
    pub can_check: bool,
    pub can_fold: bool,
    pub can_call: bool,
    pub can_raise: bool,
    pub minimum_raise_to: Option<u64>,
    pub maximum_raise_to: Option<u64>,
    pub short_all_in_only: bool,
    pub can_all_in: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Table {
    pub config: TableConfig,
    pub seats: [Seat; MAX_SEATS],
    pub dealer_seat: Option<u8>,
    pub hand_counter: u64,
    pub hand: Option<Hand>,
    pub next_hand_at: Option<i64>,
    pub version: u64,
    pub deck_source: DeckSource,
    /// Explicit onchain away mode. Independent from readiness to shuffle.
    pub auto_check_fold: u16,
}

#[inline]
fn has(mask: u16, seat: u8) -> bool {
    mask & (1u16 << seat) != 0
}

#[inline]
fn add(mask: &mut u16, seat: u8) {
    *mask |= 1u16 << seat;
}

impl Table {
    pub fn new(config: TableConfig) -> Self {
        Self {
            config,
            seats: [Seat::default(); MAX_SEATS],
            dealer_seat: None,
            hand_counter: 0,
            hand: None,
            next_hand_at: None,
            version: 0,
            deck_source: DeckSource::Local,
            auto_check_fold: 0,
        }
    }

    /// Total chips at the table. Must not change during a hand.
    pub fn chip_total(&self) -> u64 {
        let stacks: u64 = self.seats.iter().map(|seat| seat.stack).sum();
        let escrowed = match &self.hand {
            Some(hand) if hand.stage != Stage::Complete => hand.pot(),
            _ => 0,
        };
        stacks + escrowed
    }

    fn ready_seats(&self) -> u16 {
        let mut mask = 0u16;
        for (index, seat) in self.seats.iter().enumerate().take(self.config.max_players as usize) {
            if seat.occupied && seat.ready && seat.stack > 0 && seat.connected {
                mask |= 1 << index;
            }
        }
        mask
    }

    pub fn ready_count(&self) -> u32 {
        self.ready_seats().count_ones()
    }

    /// Walks clockwise from `from`, returning the first seat matching `predicate`.
    fn next_seat<F: Fn(u8) -> bool>(&self, from: u8, predicate: F) -> Option<u8> {
        let max = self.config.max_players as u16;
        for offset in 1..=max {
            let candidate = ((from as u16 + offset) % max) as u8;
            if predicate(candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Seats in `candidates`, ordered clockwise starting after `after`.
    fn ordered_seats(&self, after: u8, candidates: u16) -> ([u8; MAX_SEATS], usize) {
        let max = self.config.max_players as u16;
        let mut result = [0u8; MAX_SEATS];
        let mut length = 0;
        for offset in 1..=max {
            let seat = ((after as u16 + offset) % max) as u8;
            if has(candidates, seat) {
                result[length] = seat;
                length += 1;
            }
        }
        (result, length)
    }

    /// The positions this hand deals from, or `None` between hands.
    pub fn deal_positions(&self) -> Option<DealPositions> {
        let hand = self.hand.as_ref()?;
        Some(self.positions_for(hand.dealer_seat, hand.active_seats))
    }

    /// Where each seat's hole cards and the five board cards sit in the deck.
    ///
    /// [`Table::start_hand`] deals from this rather than counting for itself, so
    /// a confidential dealer that asks the same question gets the same answer.
    fn positions_for(&self, dealer_seat: u8, active_seats: u16) -> DealPositions {
        let (order, order_len) = self.ordered_seats(dealer_seat, active_seats);

        // Two cards each, one at a time, starting left of the button: the first
        // round fills the first `order_len` positions, the second the next.
        let mut hole = [[NO_POSITION; 2]; MAX_SEATS];
        for (slot, seat) in order.iter().take(order_len).enumerate() {
            hole[*seat as usize] = [slot as u8, (order_len + slot) as u8];
        }

        // Then `burn_and_deal`: burn, flop three; burn, turn one; burn, river
        // one. The burns are positions nobody ever opens.
        let dealt = (order_len * 2) as u8;
        let board = [dealt + 1, dealt + 2, dealt + 3, dealt + 5, dealt + 7];

        DealPositions { hole, board }
    }

    fn draw_card(hand: &mut Hand) -> u8 {
        let card = hand.deck[hand.deck_index as usize];
        hand.deck_index += 1;
        card
    }

    fn burn_and_deal(hand: &mut Hand, count: usize) {
        Self::draw_card(hand);
        for _ in 0..count {
            let card = Self::draw_card(hand);
            hand.board[hand.board_len as usize] = card;
            hand.board_len += 1;
        }
    }

    /// Moves chips from a stack into the pot, capped at what the seat has.
    fn post_chips(&mut self, seat_index: u8, requested: u64, counts_for_street: bool) -> u64 {
        let index = seat_index as usize;
        let amount = requested.min(self.seats[index].stack);
        self.seats[index].stack -= amount;

        let hand = self.hand.as_mut().expect("hand in progress");
        hand.contributions[index] += amount;
        if counts_for_street {
            hand.street_bets[index] += amount;
        }
        if self.seats[index].stack == 0 {
            add(&mut hand.all_in, seat_index);
        }
        amount
    }

    /// Begins a hand with a caller-supplied deck ordering.
    ///
    /// On-chain the deck comes from the Arcium `shuffle_and_deal` callback; the
    /// engine never generates randomness itself.
    pub fn start_hand(&mut self, deck: [u8; DECK_SIZE], now: i64) -> Result<(), EngineError> {
        if matches!(&self.hand, Some(hand) if hand.stage != Stage::Complete) {
            return Err(EngineError::HandInProgress);
        }

        let active_seats = self.ready_seats();
        if active_seats.count_ones() < 2 {
            self.next_hand_at = None;
            return Err(EngineError::NotEnoughPlayers);
        }
        // Refused here rather than worked around: a two-player mental deal has
        // no threshold that is both private and live. See [`MENTAL_MIN_SEATS`].
        if self.deck_source == DeckSource::Mental && active_seats.count_ones() < MENTAL_MIN_SEATS {
            self.next_hand_at = None;
            return Err(EngineError::NotEnoughPlayersForMentalDeal);
        }

        // Every funded seat owes blinds, even when it cannot help deal.
        // Keep absent seats OUT of the encrypted roster: including an offline
        // encryption layer would stall every hand. They post and fold instead.
        // Local oracle fixtures retain their historical cash-game rules.
        let blind_seats = if self.deck_source == DeckSource::Mental {
            self.seats.iter().enumerate().take(self.config.max_players as usize)
                .fold(0u16, |mask, (i, s)| mask | if s.occupied && s.stack > 0 { 1 << i } else { 0 })
        } else { active_seats };
        let is_active = |seat: u8| has(blind_seats, seat);
        let highest_active = (0..self.config.max_players).filter(|s| is_active(*s)).next_back();
        let previous_dealer = self.dealer_seat.or(highest_active).ok_or(EngineError::NotEnoughPlayers)?;
        let dealer_seat = self.next_seat(previous_dealer, is_active).ok_or(EngineError::NotEnoughPlayers)?;
        // Heads-up: the button posts the small blind.
        let small_blind_seat = if blind_seats.count_ones() == 2 {
            dealer_seat
        } else {
            self.next_seat(dealer_seat, is_active).ok_or(EngineError::NotEnoughPlayers)?
        };
        let big_blind_seat = self
            .next_seat(small_blind_seat, is_active)
            .ok_or(EngineError::NotEnoughPlayers)?;

        self.hand_counter += 1;
        self.dealer_seat = Some(dealer_seat);
        self.next_hand_at = None;

        self.hand = Some(Hand {
            id: self.hand_counter,
            stage: Stage::Preflop,
            version: 0,
            dealer_seat,
            small_blind_seat,
            big_blind_seat,
            active_seats,
            deck,
            deck_index: 0,
            board: [0; 5],
            board_len: 0,
            hole_cards: [[0; 2]; MAX_SEATS],
            street_bets: [0; MAX_SEATS],
            contributions: [0; MAX_SEATS],
            folded: blind_seats & !active_seats,
            all_in: 0,
            acted: 0,
            current_bet: 0,
            min_raise: self.config.big_blind,
            actor_seat: None,
            action_deadline: 0,
            awaiting: Awaiting::None,
            revealed_holes: 0,
            timed_out: 0,
            result: None,
        });

        if self.config.ante > 0 {
            for seat in seats_of(blind_seats) {
                self.post_chips(seat, self.config.ante, false);
            }
        }

        let small_posted = self.post_chips(small_blind_seat, self.config.small_blind, true);
        let big_posted = self.post_chips(big_blind_seat, self.config.big_blind, true);

        let hand = self.hand.as_mut().expect("hand in progress");
        hand.current_bet = small_posted.max(big_posted);

        let positions = self.positions_for(dealer_seat, active_seats);
        let hand = self.hand.as_mut().expect("hand in progress");
        for (seat, slots) in positions.hole.iter().enumerate() {
            for (round, position) in slots.iter().enumerate() {
                if *position != NO_POSITION {
                    hand.hole_cards[seat][round] = hand.deck[*position as usize];
                }
            }
        }
        hand.deck_index = active_seats.count_ones() as u8 * 2;

        // Cards first, then betting — but only where the cards are not there
        // yet. Every other source supplies the deck as it starts the hand, so
        // they open the betting immediately, exactly as before.
        if self.deck_source == DeckSource::Mental {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.awaiting = Awaiting::Deal;
        } else {
            self.begin_or_advance_action(big_blind_seat, now);
        }
        self.version += 1;
        Ok(())
    }

    /// Opens the betting on a hand that was waiting for its cards.
    ///
    /// Called once the players' own deal has delivered every hole card, which
    /// on chain is the moment the last unlock share a seat needs to read its
    /// own two cards lands. The blinds were posted when the hand started, so
    /// this only has to decide who acts first — the same decision, taken later.
    pub fn open_betting(&mut self, now: i64) -> Result<(), EngineError> {
        let big_blind_seat = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            if hand.awaiting != Awaiting::Deal {
                return Err(EngineError::NotAwaitingCards);
            }
            hand.big_blind_seat
        };

        let hand = self.hand.as_mut().expect("hand in progress");
        hand.awaiting = Awaiting::None;
        self.begin_or_advance_action(big_blind_seat, now);
        self.version += 1;
        Ok(())
    }

    fn begin_or_advance_action(&mut self, mut after_seat: u8, now: i64) {
      loop {
        let hand = match self.hand.as_ref() {
            Some(hand) => hand,
            None => return,
        };

        if hand.live_seats().count_ones() == 1 {
            self.finish_by_fold(now);
            return;
        }

        let needs_any = (0..self.config.max_players).any(|seat| hand.needs_action(seat));
        if !needs_any {
            self.advance_street(now);
            return;
        }

        let hand_snapshot = *hand;
        let actor = self.next_seat(after_seat, |seat| hand_snapshot.needs_action(seat));
        if let Some(seat) = actor.filter(|seat| has(self.auto_check_fold, *seat)) {
            let hand = self.hand.as_mut().expect("hand in progress");
            if hand.street_bets[seat as usize] < hand.current_bet {
                add(&mut hand.folded, seat);
            }
            add(&mut hand.acted, seat);
            hand.version += 1;
            self.version += 1;
            after_seat = seat;
            // A check/fold never reopens betting. At most MAX_SEATS actions
            // are consumed before a manual actor, a reveal, or settlement.
            continue;
        }
        let deadline = now + self.config.action_time_ms;
        let hand = self.hand.as_mut().expect("hand in progress");
        hand.actor_seat = actor;
        hand.action_deadline = deadline;
        return;
      }
    }

    fn advance_street(&mut self, now: i64) {
        self.advance_street_inner(now, false);
    }

    /// Advances a street whose cards `supply_street` has just written in.
    ///
    /// Only the park for *this* street is skipped. Everything after it - the
    /// run-out check in particular - still sees the real deck source, which is
    /// the difference between a hand that waits for the rest of the board and
    /// one that deals it from whatever the deck array happens to hold.
    fn advance_supplied_street(&mut self, now: i64) {
        self.advance_street_inner(now, true);
    }

    fn advance_street_inner(&mut self, now: i64, supplied: bool) {
        let stage = match self.hand.as_ref() {
            Some(hand) => hand.stage,
            None => return,
        };

        if stage == Stage::River {
            self.finish_showdown(now);
            return;
        }

        // Confidentially dealt, the program does not hold the deck, so it cannot
        // deal here. Park and wait for whoever opens the cards to call
        // `supply_street` - an Arcium callback, or a threshold of the players.
        if !supplied && self.deck_source.is_confidential() {
            let pending = match stage {
                Stage::Preflop => Awaiting::Flop,
                Stage::Flop => Awaiting::Turn,
                Stage::Turn => Awaiting::River,
                _ => Awaiting::None,
            };
            if pending != Awaiting::None {
                let hand = self.hand.as_mut().expect("hand in progress");
                hand.awaiting = pending;
                hand.actor_seat = None;
                hand.action_deadline = 0;
                hand.version += 1;
                self.version += 1;
                return;
            }
        }

        {
            let hand = self.hand.as_mut().expect("hand in progress");
            match stage {
                Stage::Preflop => {
                    Self::burn_and_deal(hand, 3);
                    hand.stage = Stage::Flop;
                }
                Stage::Flop => {
                    Self::burn_and_deal(hand, 1);
                    hand.stage = Stage::Turn;
                }
                Stage::Turn => {
                    Self::burn_and_deal(hand, 1);
                    hand.stage = Stage::River;
                }
                _ => {}
            }

            hand.current_bet = 0;
            hand.min_raise = self.config.big_blind;
            hand.acted = 0;
            hand.street_bets = [0; MAX_SEATS];
            hand.version += 1;
        }

        let hand = self.hand.as_ref().expect("hand in progress");
        if hand.actionable_seats().count_ones() <= 1 {
            if self.deck_source.is_confidential() {
                let hand = self.hand.as_mut().expect("hand in progress");
                hand.awaiting = Awaiting::RunOut;
                hand.actor_seat = None;
                hand.action_deadline = 0;
                hand.version += 1;
                self.version += 1;
                return;
            }
            self.run_out_board();
            self.finish_showdown(now);
            return;
        }

        let dealer_seat = hand.dealer_seat;
        self.begin_or_advance_action(dealer_seat, now);
    }

    /// Delivers the board cards an Arcium reveal produced and resumes the hand.
    ///
    /// The cards are written into the deck at exactly the positions the normal
    /// dealing path is about to read - burns included - and then that same path
    /// runs. Nothing about street advancement is reimplemented for MPC, so the
    /// behaviour the differential suite verifies is the behaviour that runs.
    pub fn supply_street(&mut self, cards: &[u8], now: i64) -> Result<(), EngineError> {
        let (awaiting, deck_index, board_len) = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            (hand.awaiting, hand.deck_index, hand.board_len)
        };
        if awaiting == Awaiting::None {
            return Err(EngineError::NotAwaitingCards);
        }
        if cards.len() != awaiting.card_count(board_len) {
            return Err(EngineError::WrongCardCount);
        }

        {
            let hand = self.hand.as_mut().expect("hand in progress");
            // `burn_and_deal` reads the burn at `deck_index`, then the street
            // cards immediately after it. For a run-out there is a burn before
            // each remaining street, so the offsets are computed as we go.
            let mut cursor = deck_index as usize;
            let mut supplied = cards.iter();
            let mut remaining = 5 - hand.board_len as usize;
            let mut first = true;
            while remaining > 0 {
                let count = if first && hand.board_len == 0 { 3 } else { 1 };
                let count = count.min(remaining);
                cursor += 1; // the burn
                for slot in 0..count {
                    if let Some(card) = supplied.next() {
                        hand.deck[cursor + slot] = *card;
                    }
                }
                cursor += count;
                remaining -= count;
                first = false;
                if awaiting != Awaiting::RunOut {
                    break;
                }
            }
            hand.awaiting = Awaiting::None;
        }

        // Replay the verified path with the deck now populated.
        //
        // The deck source is deliberately *not* faked to `Local` here. It was
        // once, to stop `advance_street` parking again for the street it had
        // just been handed, and that also switched off the run-out park two
        // lines further down: an all-in hand supplied its flop and then dealt
        // its own turn and river out of a deck that on chain is all zeros. Only
        // the one park that has already been satisfied is skipped now.
        if awaiting == Awaiting::RunOut {
            self.run_out_board();
            self.finish_showdown(now);
        } else {
            self.advance_supplied_street(now);
        }
        Ok(())
    }

    /// Delivers one contender's hole cards, revealed at showdown.
    ///
    /// Resumes settlement once every live contender has been supplied. Folded
    /// players are never asked for, which is the point of having the cluster do
    /// the reveal: a fold never exposes cards.
    pub fn supply_hole_cards(
        &mut self,
        seat_index: u8,
        cards: [u8; 2],
        now: i64,
    ) -> Result<(), EngineError> {
        let contenders = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            if hand.awaiting != Awaiting::Showdown {
                return Err(EngineError::NotAwaitingCards);
            }
            hand.live_seats()
        };
        if !has(contenders, seat_index) {
            return Err(EngineError::SeatEmpty);
        }

        let complete = {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.hole_cards[seat_index as usize] = cards;
            add(&mut hand.revealed_holes, seat_index);
            hand.revealed_holes & contenders == contenders
        };

        if complete {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.awaiting = Awaiting::None;
            self.finish_showdown(now);
        }
        Ok(())
    }

    /// Contenders still owing a `reveal_hole` before settlement can run.
    pub fn pending_hole_reveals(&self) -> u16 {
        match self.hand.as_ref() {
            Some(hand) if hand.awaiting == Awaiting::Showdown => {
                hand.live_seats() & !hand.revealed_holes
            }
            _ => 0,
        }
    }

    /// The deck positions the pending reveal must fill, for the caller to pass
    /// to the Arcis circuit. Empty when the hand is not parked.
    pub fn pending_reveal(&self) -> Option<(Awaiting, usize)> {
        let hand = self.hand.as_ref()?;
        if hand.awaiting == Awaiting::None {
            return None;
        }
        Some((hand.awaiting, hand.awaiting.card_count(hand.board_len)))
    }

    fn run_out_board(&mut self) {
        let hand = self.hand.as_mut().expect("hand in progress");
        while hand.board_len < 5 {
            if hand.board_len == 0 {
                Self::burn_and_deal(hand, 3);
            } else {
                Self::burn_and_deal(hand, 1);
            }
        }
    }

    pub fn legal_actions(&self, seat_index: u8) -> Option<LegalActions> {
        let hand = self.hand.as_ref()?;
        if hand.stage == Stage::Complete || hand.actor_seat != Some(seat_index) {
            return None;
        }
        let seat = &self.seats[seat_index as usize];
        if !seat.occupied {
            return None;
        }

        let street_bet = hand.street_bets[seat_index as usize];
        let to_call = hand.current_bet.saturating_sub(street_bet);
        let maximum = street_bet + seat.stack;
        // A seat that already acted and is still short of the current bet was
        // re-raised by a short all-in; calling is allowed but raising is not.
        let raise_locked = has(hand.acted, seat_index) && street_bet < hand.current_bet;
        let minimum_raise_to = if hand.current_bet == 0 {
            self.config.big_blind
        } else {
            hand.current_bet + hand.min_raise
        };
        let can_raise = !raise_locked && maximum > hand.current_bet;

        Some(LegalActions {
            to_call: to_call.min(seat.stack),
            can_check: to_call == 0,
            can_fold: true,
            can_call: to_call > 0,
            can_raise,
            minimum_raise_to: can_raise.then(|| minimum_raise_to.min(maximum)),
            maximum_raise_to: can_raise.then_some(maximum),
            short_all_in_only: can_raise && maximum < minimum_raise_to,
            can_all_in: seat.stack > 0 && (maximum <= hand.current_bet || !raise_locked),
        })
    }

    fn pay_action(&mut self, seat_index: u8, amount: u64) -> u64 {
        let index = seat_index as usize;
        let paid = amount.min(self.seats[index].stack);
        self.seats[index].stack -= paid;

        let hand = self.hand.as_mut().expect("hand in progress");
        hand.street_bets[index] += paid;
        hand.contributions[index] += paid;
        if self.seats[index].stack == 0 {
            add(&mut hand.all_in, seat_index);
        }
        paid
    }

    /// Applies one betting action.
    ///
    /// `expected` carries the hand id and version the caller believed current,
    /// rejecting actions that raced a state change.
    pub fn apply_action(
        &mut self,
        seat_index: u8,
        action: Action,
        now: i64,
        expected: Option<(u64, u32)>,
    ) -> Result<(), EngineError> {
        let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
        if hand.stage == Stage::Complete {
            return Err(EngineError::NoActiveHand);
        }
        if let Some((hand_id, version)) = expected {
            if hand_id != hand.id {
                return Err(EngineError::WrongHand);
            }
            if version != hand.version {
                return Err(EngineError::StaleVersion);
            }
        }
        // Named rather than falling through to NotYourTurn: while a hand is
        // parked on its deal nobody's turn has come, and "not your turn" would
        // send a player looking for whose it was.
        if hand.awaiting == Awaiting::Deal {
            return Err(EngineError::NotAwaitingCards);
        }
        if hand.actor_seat != Some(seat_index) {
            return Err(EngineError::NotYourTurn);
        }
        if !self.seats[seat_index as usize].occupied {
            return Err(EngineError::SeatEmpty);
        }
        // Deliberately no deadline check. An expired timer does not bar the
        // actor from acting: it only entitles *somebody else* to skip them, via
        // `force_timeout`, which still requires the deadline to have passed.
        //
        // Barring them as well leaves a slow player with no way back into their
        // own hand — every action they take is rejected, and the hand only
        // moves if another player chooses to act on them. Late is better than
        // locked out, and the skip is still there when nobody comes back.

        let legal = self.legal_actions(seat_index).ok_or(EngineError::NotYourTurn)?;
        let street_bet = hand.street_bets[seat_index as usize];
        let to_call = hand.current_bet.saturating_sub(street_bet);

        match action {
            Action::Fold => {
                let hand = self.hand.as_mut().expect("hand in progress");
                add(&mut hand.folded, seat_index);
                add(&mut hand.acted, seat_index);
            }
            Action::Check => {
                if !legal.can_check {
                    return Err(EngineError::CannotCheckFacingBet);
                }
                let hand = self.hand.as_mut().expect("hand in progress");
                add(&mut hand.acted, seat_index);
            }
            Action::Call => {
                if !legal.can_call {
                    return Err(EngineError::NoBetToCall);
                }
                self.pay_action(seat_index, to_call);
                let hand = self.hand.as_mut().expect("hand in progress");
                add(&mut hand.acted, seat_index);
            }
            Action::AllIn => {
                if !legal.can_all_in {
                    return Err(EngineError::RaiseNotReopened);
                }
                let target = street_bet + self.seats[seat_index as usize].stack;
                if target <= self.hand.as_ref().expect("hand in progress").current_bet {
                    self.pay_action(seat_index, to_call);
                    let hand = self.hand.as_mut().expect("hand in progress");
                    add(&mut hand.acted, seat_index);
                } else {
                    self.apply_raise(seat_index, target)?;
                }
            }
            Action::Raise { to } => {
                if !legal.can_raise {
                    return Err(EngineError::RaiseUnavailable);
                }
                self.apply_raise(seat_index, to)?;
            }
        }

        let hand = self.hand.as_mut().expect("hand in progress");
        hand.version += 1;
        self.version += 1;
        self.begin_or_advance_action(seat_index, now);
        Ok(())
    }

    fn apply_raise(&mut self, seat_index: u8, target: u64) -> Result<(), EngineError> {
        let street_bet;
        let current_bet;
        let min_raise;
        {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            street_bet = hand.street_bets[seat_index as usize];
            current_bet = hand.current_bet;
            min_raise = hand.min_raise;
        }
        let maximum = street_bet + self.seats[seat_index as usize].stack;

        if target <= current_bet {
            return Err(EngineError::RaiseNotAboveCurrentBet);
        }
        if target > maximum {
            return Err(EngineError::RaiseExceedsStack);
        }

        let raise_size = target - current_bet;
        let full_raise = raise_size >= min_raise;
        // A short all-in is allowed but does not reopen the betting.
        if !full_raise && target != maximum {
            return Err(EngineError::RaiseBelowMinimum);
        }

        self.pay_action(seat_index, target - street_bet);

        let hand = self.hand.as_mut().expect("hand in progress");
        if full_raise {
            hand.min_raise = raise_size;
            hand.acted = 1u16 << seat_index;
        } else {
            add(&mut hand.acted, seat_index);
        }
        hand.current_bet = target;
        Ok(())
    }

    /// Permissionless crank: forces the acting seat to check or fold once its
    /// deadline has passed. Replaces the server's 500ms tick loop.
    pub fn force_timeout(&mut self, now: i64) -> Result<bool, EngineError> {
        let (actor, deadline, hand_id, version) = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            if hand.stage == Stage::Complete {
                return Err(EngineError::NoActiveHand);
            }
            match hand.actor_seat {
                Some(actor) => (actor, hand.action_deadline, hand.id, hand.version),
                None => return Ok(false),
            }
        };

        // Repair a hand whose recorded actor can no longer act.
        let hand = self.hand.as_ref().expect("hand in progress");
        let stale_actor = !self.seats[actor as usize].occupied
            || !has(hand.active_seats, actor)
            || has(hand.folded, actor)
            || has(hand.all_in, actor);
        if stale_actor {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.version += 1;
            self.version += 1;
            self.begin_or_advance_action(actor, now);
            return Ok(true);
        }

        let automatic = has(self.auto_check_fold, actor);
        if now < deadline && !automatic {
            return Ok(false);
        }

        let legal = self.legal_actions(actor).ok_or(EngineError::NotYourTurn)?;
        let action = if legal.can_check { Action::Check } else { Action::Fold };

        // Marked *before* the action, and that ordering is the whole of it.
        // `apply_action` can finish the hand — a timeout fold is often the one
        // that leaves a single player standing — and `complete_hand` reads this
        // mask on its way out. Setting the bit afterwards writes it to a hand
        // that has already completed, so the seat stays ready and is dealt in
        // again, which is the exact bug this feature exists to fix. The SVM
        // harness caught it; no unit test here did, because in a three-handed
        // hand the first timeout rarely ends it.
        //
        // Safe to mark ahead of the call: the actor is on the clock and past
        // their deadline by this point, and a failure aborts the transaction
        // that would have persisted this.
        if let Some(hand) = self.hand.as_mut().filter(|_| !automatic) {
            hand.timed_out |= 1u16 << actor;
        }
        self.apply_action(actor, action, now.min(deadline), Some((hand_id, version)))?;
        Ok(true)
    }

    fn finish_by_fold(&mut self, now: i64) {
        let (winner_seat, pot) = {
            let hand = self.hand.as_ref().expect("hand in progress");
            let winner = seats_of(hand.live_seats()).next().expect("a live seat");
            (winner, hand.pot())
        };
        self.seats[winner_seat as usize].stack += pot;

        let mut winners = [Winner::default(); MAX_SEATS];
        winners[0] = Winner { seat: winner_seat, amount: pot, category: None };
        self.complete_hand(
            HandResult { kind: ResultKind::Fold, winners, winner_count: 1 },
            now,
        );
    }

    fn finish_showdown(&mut self, now: i64) {
        self.run_out_board();

        // Evaluation needs every live contender's cards in the clear. Dealt
        // confidentially they are sealed until the reveals arrive - from the
        // cluster, or from each contender opening their own - so park rather
        // than scoring zeroed hole cards.
        if self.deck_source.is_confidential() {
            let hand = self.hand.as_ref().expect("hand in progress");
            let contenders = hand.live_seats();
            if hand.revealed_holes & contenders != contenders {
                let hand = self.hand.as_mut().expect("hand in progress");
                hand.awaiting = Awaiting::Showdown;
                hand.actor_seat = None;
                hand.action_deadline = 0;
                hand.version += 1;
                self.version += 1;
                return;
            }
        }

        let (contenders, contributions, folded, dealer_seat) = {
            let hand = self.hand.as_ref().expect("hand in progress");
            (hand.live_seats(), hand.contributions, hand.folded, hand.dealer_seat)
        };

        // Showdown cards are public, so evaluation runs in the clear.
        let mut scores: [Option<Score>; MAX_SEATS] = [None; MAX_SEATS];
        for seat in seats_of(contenders) {
            let hand = self.hand.as_ref().expect("hand in progress");
            let mut cards = [0u8; 7];
            cards[0] = hand.hole_cards[seat as usize][0];
            cards[1] = hand.hole_cards[seat as usize][1];
            cards[2..7].copy_from_slice(&hand.board[..5]);
            scores[seat as usize] = evaluate(&cards);
        }

        let mut awards = [0u64; MAX_SEATS];
        let absent = self.hand.as_ref().expect("hand").folded
            & !self.hand.as_ref().expect("hand").active_seats;
        let pots = crate::pots::build_side_pots_with_absent(&contributions, folded, absent, contenders);

        for pot in pots.as_slice() {
            if pot.refund {
                let seat = seats_of(pot.contributors).next().expect("a contributor");
                self.seats[seat as usize].stack += pot.amount;
                awards[seat as usize] += pot.amount;
                continue;
            }

            let mut best: Option<Score> = None;
            for seat in seats_of(pot.eligible) {
                if let Some(score) = scores[seat as usize] {
                    if best.is_none_or(|current| score > current) {
                        best = Some(score);
                    }
                }
            }
            let best = match best {
                Some(best) => best,
                None => continue,
            };

            let mut winner_mask = 0u16;
            let mut winner_count = 0u64;
            for seat in seats_of(pot.eligible) {
                if scores[seat as usize] == Some(best) {
                    winner_mask |= 1 << seat;
                    winner_count += 1;
                }
            }

            let share = pot.amount / winner_count;
            let mut remainder = pot.amount % winner_count;
            // Odd chips go clockwise from the button.
            let (order, order_len) = self.ordered_seats(dealer_seat, winner_mask);
            for seat in order.iter().take(order_len) {
                let award = share + u64::from(remainder > 0);
                remainder = remainder.saturating_sub(1);
                self.seats[*seat as usize].stack += award;
                awards[*seat as usize] += award;
            }
        }

        let mut winners = [Winner::default(); MAX_SEATS];
        let mut winner_count = 0usize;
        for (seat, amount) in awards.iter().copied().enumerate() {
            if amount > 0 {
                winners[winner_count] = Winner {
                    seat: seat as u8,
                    amount,
                    category: scores[seat].map(|score| score.category()),
                };
                winner_count += 1;
            }
        }
        // Largest award first, then by seat, matching the oracle's ordering.
        winners[..winner_count]
            .sort_by(|a, b| b.amount.cmp(&a.amount).then(a.seat.cmp(&b.seat)));

        self.complete_hand(
            HandResult { kind: ResultKind::Showdown, winners, winner_count },
            now,
        );
    }

    fn complete_hand(&mut self, result: HandResult, now: i64) {
        let (active_seats, timed_out) = {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.stage = Stage::Complete;
            hand.actor_seat = None;
            hand.action_deadline = 0;
            hand.result = Some(result);
            hand.version += 1;
            (hand.active_seats, hand.timed_out)
        };
        self.next_hand_at = Some(now + HAND_COMPLETE_DELAY_MS);

        for seat_index in seats_of(active_seats) {
            let seat = &mut self.seats[seat_index as usize];
            if !seat.occupied {
                continue;
            }
            // A seat that was skipped this hand sits out the next one. Being
            // skipped once is evidence nobody is there, and a table of absent
            // players otherwise burns a full action timer per seat per hand
            // forever. Coming back is `set_ready`, which costs one click.
            if seat.stack == 0 || !seat.connected || has(timed_out, seat_index) {
                seat.ready = false;
            }
        }
        self.version += 1;
    }

    /// Ends a hand nobody can finish, returning every chip to whoever put it in.
    ///
    /// The remedy of last resort for a confidential deal that stalled. A hand
    /// whose cards never arrived has no winner to find - not an unknown winner,
    /// none - so the only answer that is fair to everyone is the one that
    /// leaves every stack exactly as it started.
    ///
    /// It is safe from the obvious abuse because of *when* it can be reached. A
    /// deal completes before any player can read their own cards, so a player
    /// who stalls it stalls in ignorance: there is nothing yet to run from. By
    /// the time a player knows anything worth escaping, the remaining stalls
    /// have their own remedies - a board card opens on a threshold, and
    /// refusing to show at showdown is [`Table::muck_unrevealed`].
    pub fn void_hand(&mut self, now: i64) -> Result<(), EngineError> {
        let contributions = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            if hand.stage == Stage::Complete {
                return Err(EngineError::NoActiveHand);
            }
            hand.contributions
        };

        for (seat, amount) in contributions.iter().enumerate() {
            self.seats[seat].stack += amount;
        }
        {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.contributions = [0; MAX_SEATS];
            hand.street_bets = [0; MAX_SEATS];
            hand.awaiting = Awaiting::None;
        }

        self.complete_hand(
            HandResult {
                kind: ResultKind::Voided,
                winners: [Winner::default(); MAX_SEATS],
                winner_count: 0,
            },
            now,
        );
        Ok(())
    }

    /// Folds every contender who will not show, and settles among those who did.
    ///
    /// This is poker's own rule rather than a new one: at showdown, declining to
    /// show is mucking. So a player who stalls here is not holding the table
    /// hostage, they are folding, and the pot goes to whoever did show. If
    /// nobody shows at all there is no hand to award and the chips go back.
    pub fn muck_unrevealed(&mut self, now: i64) -> Result<(), EngineError> {
        let (contenders, revealed) = {
            let hand = self.hand.as_ref().ok_or(EngineError::NoActiveHand)?;
            if hand.awaiting != Awaiting::Showdown {
                return Err(EngineError::NotAwaitingCards);
            }
            (hand.live_seats(), hand.revealed_holes)
        };

        if contenders & revealed == 0 {
            return self.void_hand(now);
        }

        {
            let hand = self.hand.as_mut().expect("hand in progress");
            hand.folded |= contenders & !revealed;
            hand.awaiting = Awaiting::None;
        }
        self.finish_showdown(now);
        Ok(())
    }

    /// Permissionless crank: clears a completed hand and starts the next one.
    ///
    /// Returns `true` when the caller should now queue a fresh shuffle.
    pub fn start_next_hand(&mut self, now: i64) -> bool {
        let complete = matches!(&self.hand, Some(hand) if hand.stage == Stage::Complete);
        if !complete {
            return false;
        }
        match self.next_hand_at {
            Some(at) if now >= at => {}
            _ => return false,
        }

        // A seat that asked to leave stops playing, and keeps its chair until
        // something with token accounts can pay it out.
        //
        // This used to be `*seat = Seat::default()`, which zeroed the stack as
        // well — and off chain that is free, because `server/poker-engine.mjs`
        // has no vault and `chipTotal` simply stops counting a departed seat.
        // On chain the chips are really in escrow: `bridge::store_table` copies
        // the zeroed stack into `TableState` without reducing `escrowed`, and
        // `assert_chips_conserved` then rejects the whole instruction with
        // `VaultUnderfunded`. The transaction reverts, so nothing was ever
        // lost — but it reverts every time, and the table can never start
        // another hand. A player leaving mid-hand wedged the table for
        // everybody, permanently.
        //
        // `start_next_hand` cannot pay them: it is a permissionless crank and
        // the `Play` context carries no vault and no token account, by design.
        // So the seat is un-readied instead and `kick_idle` — which does have
        // those accounts, derives the destination from the seat's own recorded
        // owner, and is already tested — completes the departure. It waives its
        // idle grace for a seat that asked to leave, so this is immediate
        // rather than a two-minute wait.
        //
        // This is a deliberate divergence from the JavaScript oracle, which
        // keeps the old behaviour and is right to: it has no custody to
        // conserve. No differential vector can reach it — `differential.rs`
        // never calls `start_next_hand`, and `scripts/dump-engine-vectors.mjs`
        // does not import `requestLeave`, `tickTable` or `cleanDepartedSeats`.
        for seat in self.seats.iter_mut() {
            if seat.leave_after_hand {
                seat.ready = false;
            }
        }
        self.hand = None;
        // Two is the betting engine's own floor and it is not the one that
        // matters here: a mental deal needs `MENTAL_MIN_SEATS`, and scheduling
        // a hand this table cannot deal points every client at a moment that
        // arrives and then refuses them.
        self.next_hand_at = if self.deck_source == DeckSource::Mental {
            (self.ready_count() >= MENTAL_MIN_SEATS).then(|| now + NEXT_HAND_DELAY_MS)
        } else {
            (self.ready_count() >= 2).then(|| now + NEXT_HAND_DELAY_MS)
        };
        self.version += 1;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::ordered_deck;
    use crate::pots::seats_of;

    fn seated(count: usize) -> Table {
        let mut table = Table::new(TableConfig::new(50, 0, MAX_SEATS as u8, 5_000, 30_000));
        for seat in table.seats.iter_mut().take(count) {
            *seat = Seat { occupied: true, stack: 5_000, ready: true, connected: true, leave_after_hand: false };
        }
        table
    }

    /// A player who gets skipped sits out the next hand rather than being dealt
    /// in and skipped again.
    ///
    /// Without this a table of absent players burns a full action timer per
    /// seat per hand, forever, and nothing ever clears it: `ready` was only
    /// dropped for a bust or a disconnect, and a player who simply walked away
    /// is neither.
    #[test]
    fn a_skipped_player_sits_out_the_next_hand() {
        let mut table = seated(3);
        table.start_hand(ordered_deck(), 0).expect("a hand");

        let actor = table.hand.as_ref().unwrap().actor_seat.expect("somebody on the clock");
        let deadline = table.hand.as_ref().unwrap().action_deadline;

        // Before the deadline nothing happens, and nobody is marked.
        assert_eq!(table.force_timeout(deadline - 1), Ok(false));
        assert_eq!(table.hand.as_ref().unwrap().timed_out, 0);

        assert_eq!(table.force_timeout(deadline), Ok(true));
        assert!(has(table.hand.as_ref().unwrap().timed_out, actor));
        // Still in the hand: a skip is a check or a fold, not a sit-out yet.
        assert!(table.seats[actor as usize].ready, "un-readied mid-hand");

        // Play the hand out however it goes, then the mark is applied.
        let mut now = deadline;
        while table.hand.as_ref().map(|hand| hand.stage) != Some(Stage::Complete) {
            now += 1;
            let hand = table.hand.as_ref().unwrap();
            let Some(next) = hand.actor_seat else { break };
            let deadline = hand.action_deadline;
            table.force_timeout(deadline.max(now)).expect("a crank");
            now = deadline.max(now);
        }

        assert_eq!(table.hand.as_ref().unwrap().stage, Stage::Complete);
        assert!(
            !table.seats[actor as usize].ready,
            "a skipped player was left ready and will be dealt in again",
        );

        // And coming back is one call, not a rebuy or a reseat.
        table.seats[actor as usize].ready = true;
        assert!(table.seats[actor as usize].ready);
    }

    /// Folding on purpose is not being skipped.
    ///
    /// The mark is set by the timeout crank, never by `apply_action`, so a
    /// player folding every hand deliberately stays in the game.
    #[test]
    fn a_deliberate_fold_does_not_sit_a_player_out() {
        let mut table = seated(3);
        table.start_hand(ordered_deck(), 0).expect("a hand");

        let actor = table.hand.as_ref().unwrap().actor_seat.expect("an actor");
        table.apply_action(actor, Action::Fold, 1, None).expect("a fold");

        assert_eq!(table.hand.as_ref().unwrap().timed_out, 0);
        assert!(table.seats[actor as usize].ready, "a voluntary fold sat the player out");
    }

    /// The positions have to agree with what `start_hand` actually drew, or a
    /// confidential dealer would open the wrong card for the right seat.
    #[test]
    fn deal_positions_match_the_cards_start_hand_dealt() {
        for count in 2..=MAX_SEATS {
            let mut table = seated(count);
            table.start_hand(ordered_deck(), 0).expect("hand starts");

            let positions = table.deal_positions().expect("a hand is in progress");
            let hand = table.hand.as_ref().expect("a hand is in progress");

            for seat in 0..MAX_SEATS {
                let dealt_in = has(hand.active_seats, seat as u8);
                for round in 0..2 {
                    let position = positions.hole[seat][round];
                    if !dealt_in {
                        assert_eq!(position, NO_POSITION, "{count} players: seat {seat} is not in the hand");
                        continue;
                    }
                    assert_eq!(
                        hand.deck[position as usize], hand.hole_cards[seat][round],
                        "{count} players: seat {seat} card {round} came from another position",
                    );
                }
            }

            // Every position is claimed exactly once, and the burns are not.
            let mut claimed: Vec<u8> = positions.dealt().collect();
            claimed.sort_unstable();
            claimed.dedup();
            assert_eq!(claimed.len(), count * 2 + 5, "{count} players: positions collide");
            assert_eq!(hand.deck_index as usize, count * 2, "{count} players: deck index");
        }
    }

    /// Board positions are asserted against the board the engine deals, burns
    /// and all, rather than against the arithmetic that produced them.
    #[test]
    fn board_positions_match_the_board_the_engine_deals() {
        for count in 2..=MAX_SEATS {
            let mut table = seated(count);
            table.start_hand(ordered_deck(), 0).expect("hand starts");
            table.run_out_board();

            let positions = table.deal_positions().expect("a hand is in progress");
            let hand = table.hand.as_ref().expect("a hand is in progress");
            let expected: Vec<u8> = positions.board.iter().map(|at| hand.deck[*at as usize]).collect();
            assert_eq!(hand.board(), expected, "{count} players: board positions");
        }
    }

    /// A confidentially dealt hand where everyone is all-in must ask for the
    /// rest of the board, not deal it.
    ///
    /// `supply_street` used to replay the street it had been handed with the
    /// deck source faked to `Local`, which also switched off the run-out park
    /// sitting two lines below it. The flop arrived, and the turn and river
    /// were dealt from the deck array - which on chain is all zeros, so every
    /// all-in run-out ended on a board of two of clubs twice.
    #[test]
    fn an_all_in_run_out_asks_for_the_rest_of_the_board() {
        for source in [DeckSource::Mental] {
            let mut table = seated(3);
            table.deck_source = source;
            // What `bridge.rs` hands the engine: no card material at all.
            table.start_hand([0u8; DECK_SIZE], 0).expect("hand starts");
            // Arcium has its deck by the time the hand starts; a player-shuffled
            // one does not, so it parks until the deal delivers.
            if source == DeckSource::Mental {
                table.open_betting(0).expect("betting opens once the deal lands");
            }

            let mut now = 0i64;
            while let Some(actor) = table.hand.as_ref().and_then(|hand| hand.actor_seat) {
                now += 10;
                table.apply_action(actor, Action::AllIn, now, None).expect("all-in");
            }

            let (awaiting, count) = table.pending_reveal().expect("{source:?}: parked");
            assert_eq!(awaiting, Awaiting::Flop, "{source:?}: parked for the flop");
            table.supply_street(&[1, 2, 3][..count], now).expect("the flop");

            // And then for the rest of it, rather than dealing it itself.
            let (awaiting, count) = table.pending_reveal().expect("{source:?}: parked again");
            assert_eq!(awaiting, Awaiting::RunOut, "{source:?}: parked for the run-out");
            assert_eq!(count, 2, "{source:?}: turn and river");
            table.supply_street(&[4, 5], now).expect("the run-out");

            let hand = table.hand.as_ref().expect("a hand");
            assert_eq!(hand.board(), &[1, 2, 3, 4, 5], "{source:?}: board");
            // Still owed the contenders' hole cards before it can settle.
            assert_eq!(hand.awaiting, Awaiting::Showdown, "{source:?}: showdown park");
        }
    }

    /// Drives a three-handed mental hand to the showdown park, everyone checking.
    fn parked_at_showdown() -> Table {
        let mut table = seated(3);
        table.deck_source = DeckSource::Mental;
        table.start_hand(ordered_deck(), 0).expect("hand starts");
        // A player-shuffled hand parks until its cards exist; on chain the last
        // hole unlock opens the betting, and here nothing is sealed so the test
        // stands in for it.
        table.open_betting(0).expect("betting opens once the deal lands");

        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 64, "the hand never reached a showdown");
            let hand = table.hand.as_ref().expect("hand in progress");
            if hand.awaiting == Awaiting::Showdown {
                return table;
            }
            if let Some((_, count)) = table.pending_reveal() {
                // The deck is ordered, so the card at a position is that number.
                let hand = table.hand.as_ref().expect("hand in progress");
                let dealt = hand.board_len as usize;
                let positions = table.deal_positions().expect("positions");
                let cards: Vec<u8> = positions.board[dealt..dealt + count].to_vec();
                table.supply_street(&cards, 0).expect("supply_street");
                continue;
            }
            let actor = table.hand.as_ref().unwrap().actor_seat.expect("somebody acts");
            let legal = table.legal_actions(actor).expect("legal actions");
            let action = if legal.can_check { Action::Check } else { Action::Call };
            table.apply_action(actor, action, 0, None).expect("action");
        }
    }

    /// A deal that never finished leaves every stack exactly as it started.
    #[test]
    fn voiding_a_hand_returns_every_chip() {
        let mut table = seated(3);
        table.deck_source = DeckSource::Mental;
        let before = table.chip_total();
        table.start_hand(ordered_deck(), 0).expect("hand starts");

        // Blinds are in the pot, so at least two stacks are down.
        assert!(table.seats.iter().any(|seat| seat.stack < 5_000));
        assert!(table.hand.as_ref().unwrap().pot() > 0);

        table.void_hand(1_000).expect("void");

        let hand = table.hand.as_ref().expect("hand recorded");
        assert_eq!(hand.stage, Stage::Complete);
        assert_eq!(hand.result.as_ref().unwrap().kind, ResultKind::Voided);
        assert!(hand.result.as_ref().unwrap().winners().is_empty(), "a void has no winner");
        assert_eq!(hand.pot(), 0, "chips were left in the pot");
        for (seat, record) in table.seats.iter().enumerate().take(3) {
            assert_eq!(record.stack, 5_000, "seat {seat} was not made whole");
        }
        assert_eq!(table.chip_total(), before, "voiding changed the chip supply");

        // And the table moves on rather than sitting on a finished hand.
        assert!(table.next_hand_at.is_some());
        assert_eq!(table.void_hand(1_000), Err(EngineError::NoActiveHand));
    }

    /// Refusing to show is mucking, which is poker's rule and not a new one.
    #[test]
    fn mucking_at_showdown_pays_whoever_showed() {
        let mut table = parked_at_showdown();
        let before = table.chip_total();
        let pot = table.hand.as_ref().unwrap().pot();
        let contenders = table.hand.as_ref().unwrap().live_seats();
        let shower = seats_of(contenders).next().expect("a contender");

        // One player shows; the rest never will.
        let positions = table.deal_positions().expect("positions");
        let cards = positions.hole[shower as usize];
        table
            .supply_hole_cards(shower, [cards[0], cards[1]], 0)
            .expect("one contender shows");

        let stack_before = table.seats[shower as usize].stack;
        table.muck_unrevealed(2_000).expect("muck");

        let hand = table.hand.as_ref().expect("hand recorded");
        assert_eq!(hand.stage, Stage::Complete);
        assert_eq!(
            table.seats[shower as usize].stack,
            stack_before + pot,
            "the only player who showed did not take the pot",
        );
        assert_eq!(table.chip_total(), before, "mucking changed the chip supply");
    }

    /// If nobody shows there is no hand to award, so the chips go back.
    #[test]
    fn mucking_when_nobody_showed_voids_the_hand() {
        let mut table = parked_at_showdown();
        let before = table.chip_total();

        table.muck_unrevealed(2_000).expect("muck");

        let hand = table.hand.as_ref().expect("hand recorded");
        assert_eq!(hand.result.as_ref().unwrap().kind, ResultKind::Voided);
        assert_eq!(table.chip_total(), before);
        for (seat, record) in table.seats.iter().enumerate().take(3) {
            assert_eq!(record.stack, 5_000, "seat {seat} was not made whole");
        }
    }

    #[test]
    fn mucking_is_refused_unless_the_hand_is_at_a_showdown() {
        let mut table = seated(3);
        table.deck_source = DeckSource::Mental;
        table.start_hand(ordered_deck(), 0).expect("hand starts");
        assert_eq!(table.muck_unrevealed(0), Err(EngineError::NotAwaitingCards));
    }

    #[test]
    fn a_mental_deal_refuses_heads_up() {
        let mut table = seated(2);
        table.deck_source = DeckSource::Mental;
        assert_eq!(
            table.start_hand(ordered_deck(), 0),
            Err(EngineError::NotEnoughPlayersForMentalDeal),
        );

        // The same table with a third player is fine, and Arcium is unaffected.
        let mut table = seated(3);
        table.deck_source = DeckSource::Mental;
        assert!(table.start_hand(ordered_deck(), 0).is_ok());

        // Local still deals two, because it is not a dealing scheme with a
        // threshold in it — it is the deck the tests hand over directly.
        let mut table = seated(2);
        table.deck_source = DeckSource::Local;
        assert!(table.start_hand(ordered_deck(), 0).is_ok());
    }
}
