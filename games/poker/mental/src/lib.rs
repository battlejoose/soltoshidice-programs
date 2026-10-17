//! Mental poker: a deck the players shuffle, and open, themselves.
//!
//! Hash Holdem's cards are dealt today by an Arcium MPC cluster. It works, and
//! it costs about 0.005 SOL per computation against a floor of five fixed
//! computations a hand — dollars per hand, which is not a slope problem that
//! batching fixes but an intercept problem that a different scheme fixes.
//! `docs/DEALING.md` has the measurements. This crate is the different scheme.
//!
//! # The shape of a hand
//!
//! 1. **Shuffle.** Each player in turn multiplies all fifty-two cards by one
//!    secret scalar and reorders them ([`shuffle`]). Scalar multiplication
//!    commutes, so the layers can come off in any order later. Afterwards the
//!    deck carries the product of every mask in the composition of every
//!    permutation, and no player knows where a card went.
//! 2. **Lock.** Each player takes a second turn ([`lock`]), this time without
//!    reordering: they strip their own mask and put back a separate fresh key
//!    for every position. Positions are now stable and each card's secret is
//!    fifty-two independent keys rather than one shared one.
//! 3. **Deal.** To give player `p` a hole card, every *other* player publishes
//!    their key for that position ([`deal`]). What is left is locked under
//!    `p`'s key alone. To turn a board card over, everyone publishes — or a
//!    threshold rebuilds the keys of whoever did not, from shares split during
//!    the lock ([`share`]).
//!
//! # What this is and is not
//!
//! Held: a hole card cannot be opened without its owner, by any coalition,
//! including all the other players together. A board card cannot be opened
//! without all but one of the players. No party — operator included — holds a share it
//! could combine with one player's to open anything.
//!
//! Also held: a player who substitutes a deck they chose is caught at publish
//! time rather than afterwards. Every shuffle round carries a zero-knowledge
//! argument that it is the deck its author was handed under one permutation and
//! one scalar ([`shuffle_proof`]) — a Terelius-Wikström permutation-matrix
//! argument specialised to the *common-exponent* statement this scheme actually
//! makes, one scalar across the whole deck rather than the re-encryption
//! shuffle the well-known arguments target. [`shuffle::audit`] remains, demoted
//! from remedy to break-glass.
//!
//! Not held: the chain does not check the argument. Every player does, before a
//! chip is committed, so a cheat is refused and attributable; but an honest
//! client's only lever is to stop, which stalls the deal into a refund for
//! everyone. `docs/SHUFFLE_PROOF.md` states that trade in full.
//!
//! # Determinism
//!
//! Nothing here samples randomness. Every secret is derived from one 32-byte
//! seed through a labelled PRF, so the same seeds produce the same transcript
//! byte for byte — which is how the TypeScript player in `app/chain/mental/`
//! is held to the same protocol as this one, by a shared fixture rather than by
//! two readings of a specification.

#![forbid(unsafe_code)]

pub mod deal;
pub mod group;
pub mod lock;
pub mod merkle;
pub mod proof;
pub mod protocol;
pub mod share;
pub mod shuffle;
pub mod shuffle_proof;

pub use deal::{open, strip, verify_peel, Peel};
pub use group::{
    card_of_point, card_point, multiscalar, ordered_deck, Seed, Transcript, CARD_COUNT,
};
pub use lock::{lock, verify_lock, LockRound, LockSecret};
pub use protocol::{deal_hand, Deal, Participant, Session};
pub use share::{board_threshold, recover, verify_share, Recipient, SharePackage};
pub use shuffle::{audit, shuffle, verify_shuffle, ShuffleRound, ShuffleSecret};
pub use shuffle_proof::{prove_shuffle, verify_shuffle_proof, ShuffleProof, BRIDGE_COUNT};

/// Fewest players a hand can be dealt to.
///
/// Mirrors `riverproof_engine::MENTAL_MIN_SEATS`, and for the same reason: with
/// two players the board threshold is either 1, so either player opens the
/// board early, or 2, so either stalls it forever. There is no third option to
/// tune. The engine refuses the table; this crate refuses the deal.
pub const MIN_PLAYERS: u8 = 3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MentalError {
    /// A deck that was not fifty-two cards.
    WrongDeckSize,
    /// Thirty-two bytes that are not a ristretto255 point.
    NotAGroupElement,
    /// The identity: a card that survives every key, or a proof base that
    /// proves nothing.
    IdentityElement,
    /// A point that opened to none of the fifty-two cards.
    NotACard,
    /// One card in two positions.
    DuplicateCard,
    /// An ordering that was not a permutation.
    NotAPermutation,
    /// A proof that did not check out: a Chaum-Pedersen proof in
    /// [`proof`], or the shuffle argument in [`shuffle_proof`].
    BadProof,
    /// A published key that did not match the commitment made for it.
    BadPeel,
    /// A share that did not match the commitments it came with.
    BadShare,
    /// Fewer shares than the threshold.
    NotEnoughShares,
    /// The same holder's share offered twice.
    DuplicateShare,
    /// A threshold below two, or above the number of players.
    BadThreshold,
    /// A position outside the deck.
    PositionOutOfRange,
    /// The zero scalar: a key that erases a card rather than hiding it.
    ZeroScalar,
    /// A scalar with a second spelling.
    NonCanonicalScalar,
    /// A transcript with the wrong number of rounds for its players.
    WrongPlayerCount,
    /// Fewer than [`MIN_PLAYERS`].
    TooFewPlayers,
    /// A player who is not in this hand.
    UnknownPlayer,
    /// Lock rounds that did not follow their own shuffle rounds.
    RoundOutOfOrder,
    /// Dispute: a revealed mask was not the one committed to.
    MaskDoesNotMatchCommitment { player: u8 },
    /// Dispute: a round was not a masked reordering of the round before it.
    ShuffleNotAPermutation { player: u8 },
}
