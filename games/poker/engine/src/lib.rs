//! Hash Holdem poker rules engine.
//!
//! A faithful Rust port of `server/poker-engine.mjs`, the server-authoritative
//! JavaScript engine Hash Holdem shipped before the on-chain rewrite. That file
//! does not ship on-chain but is kept as the differential-test oracle: the
//! vectors in `tests/vectors/engine-vectors.json` are dumped from it by
//! `scripts/dump-engine-vectors.mjs`, and `tests/differential.rs` asserts this
//! crate reproduces them action for action.
//!
//! The crate has no Solana dependency so it can be tested natively. The Anchor
//! program wraps it.

pub mod cards;
pub mod eval;
pub mod pots;
pub mod table;

pub use cards::{format_card, parse_card, shuffle_deck, DECK_SIZE};
pub use eval::{evaluate, Score, CATEGORY_NAMES};
pub use pots::{build_side_pots, seats_of, SidePot, SidePots};
pub use table::{
    Action, Awaiting, DealPositions, DeckSource, EngineError, Hand, HandResult, LegalActions,
    ResultKind, Seat, Stage, Table, TableConfig, Winner, MAX_SEATS, MENTAL_MIN_SEATS, NO_POSITION,
};
