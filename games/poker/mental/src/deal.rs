//! Opening cards: who publishes what, and what it opens.
//!
//! A locked card at position `j` is the card point multiplied by every player's
//! key for that position. Taking a layer off is publishing the key — the scalar
//! itself, not a proof about it — and anybody can then divide it out. That
//! sounds alarming and is not: a key opens one position and says nothing about
//! any other, and a card only turns over once the *last* layer comes off.
//!
//! So the two cases fall out of one mechanism.
//!
//! A hole card belonging to player `p`: everybody except `p` publishes. What is
//! left is locked under `p`'s key alone, and `p` is the only party in the world
//! who can finish it. Refusing to publish at showdown is refusing to show a
//! hand, which poker has always scored as a fold.
//!
//! A board card: everybody publishes, or a threshold rebuilds the keys of
//! whoever did not. Nobody owns the board, so nobody gets to hold it hostage.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::group::{basepoint, card_of_point, decode_scalar, encode_scalar};
use crate::MentalError;

/// One player's key to one position, published so the rest can divide it out.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Peel {
    pub player: u8,
    pub position: u8,
    pub key: [u8; 32],
}

impl Peel {
    pub fn new(player: u8, position: u8, key: &Scalar) -> Self {
        Self { player, position, key: encode_scalar(key) }
    }
}

/// Checks a published key against the commitment made when the deck was locked.
///
/// Without this a player could publish any scalar at all: the card would come
/// out as a point that is not a card, the hand would stall, and there would be
/// nothing to say whose fault it was. With it, a wrong key is refused where it
/// was published.
pub fn verify_peel(peel: &Peel, commitment: &[u8; 32]) -> Result<Scalar, MentalError> {
    let key = decode_scalar(&peel.key).map_err(|_| MentalError::BadPeel)?;
    if crate::group::encode_point(&(basepoint() * key)) != *commitment {
        return Err(MentalError::BadPeel);
    }
    Ok(key)
}

/// Divides the given keys out of a locked card.
///
/// Order does not matter, which is the property the whole scheme rests on:
/// scalar multiplication commutes, so players can peel in whatever order they
/// happen to act in.
pub fn strip(locked: &RistrettoPoint, keys: &[Scalar]) -> RistrettoPoint {
    let mut point = *locked;
    for key in keys {
        point *= key.invert();
    }
    point
}

/// Divides out every key and reads the card underneath.
///
/// `NotACard` here means the layers were wrong, not that the card was lost:
/// some key did not match what was committed, or the deck was tampered with
/// upstream. Either way the hand is broken and [`crate::shuffle::audit`] is how
/// it gets attributed.
pub fn open(locked: &RistrettoPoint, keys: &[Scalar]) -> Result<u8, MentalError> {
    card_of_point(&strip(locked, keys))
}
