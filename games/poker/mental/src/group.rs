//! The group the deck lives in, and the deterministic randomness that drives it.
//!
//! ristretto255: a prime-order group, so every non-identity element is a
//! generator and there is no cofactor to reason about. Scalar multiplication
//! commutes, which is the whole trick — `a * (b * P) == b * (a * P)` means two
//! players can each lock a card and unlock it in either order.
//!
//! Every value here is derived, never sampled. A player's secrets all come from
//! one 32-byte seed through a labelled PRF, so the same seed produces the same
//! transcript byte for byte in Rust and in TypeScript. In play the seed is 32
//! bytes of real entropy that never leaves the browser; in tests it is a
//! constant, which is what makes the two implementations comparable at all.

use curve25519_dalek::constants::RISTRETTO_BASEPOINT_POINT;
use curve25519_dalek::ristretto::{CompressedRistretto, RistrettoPoint};
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::{Identity, VartimeMultiscalarMul};
use sha2::{Digest, Sha512};
use std::sync::OnceLock;

use crate::MentalError;

/// Cards in a deck. The deck is also the position space: position `j` is deck
/// index `j` as the engine counts them.
pub const CARD_COUNT: usize = 52;

const CARD_DOMAIN: &[u8] = b"riverproof/mental/card/v1";
const HAND_DOMAIN: &[u8] = b"riverproof/mental/hand-seed/v1";
const PRF_DOMAIN: &[u8] = b"riverproof/mental/prf/v1";
const NONCE_DOMAIN: &[u8] = b"riverproof/mental/nonce/v1";
const TRANSCRIPT_DOMAIN: &[u8] = b"riverproof/mental/transcript/v1";

/// The generator every commitment is taken against.
pub fn basepoint() -> RistrettoPoint {
    RISTRETTO_BASEPOINT_POINT
}

/// `Σ scalars[i] * points[i]`, in one pass.
///
/// Verification is where a deck-sized sum actually shows up — a table runs one
/// round verification per player per round, so ten-handed is ten rounds of
/// fifty-two-term sums per player. Straus-Pippenger turns that into roughly one
/// point addition per term instead of a full scalar multiplication.
///
/// **Variable time, and only ever handed public values.** A prover's nonce is
/// not a public value, so [`crate::shuffle_proof`] adds its nonce commitments
/// one point at a time and calls this only from its verifier.
pub fn multiscalar(points: &[RistrettoPoint], scalars: &[Scalar]) -> RistrettoPoint {
    RistrettoPoint::vartime_multiscalar_mul(scalars.iter(), points.iter())
}

/// The point standing for a card.
///
/// Hashed into the group rather than derived as `d * G`, and the difference is
/// not cosmetic. If the card points had known discrete logs to each other, a
/// shuffled deck could be un-shuffled with no secret at all: for a candidate
/// assignment of cards `a`, `b` to positions `j`, `k`, the test
/// `d_b * D[j] == d_a * D[k]` holds exactly when the guess is right. Hashing
/// leaves nobody knowing those relations, so the test cannot be written.
pub fn card_point(card: u8) -> RistrettoPoint {
    let mut hasher = Sha512::new();
    hasher.update(CARD_DOMAIN);
    hasher.update([card]);
    RistrettoPoint::from_uniform_bytes(&hasher.finalize().into())
}

/// The fifty-two card points, derived once.
///
/// A cache rather than a constant only because the derivation is the
/// specification: `card_point` is what the TypeScript player implements, and a
/// baked table would let the two drift without saying so. The on-chain build
/// wants the opposite trade — a constant table, checked against this derivation
/// by a test — and that is a change for the session that puts this on chain.
fn card_table() -> &'static [RistrettoPoint; CARD_COUNT] {
    static TABLE: OnceLock<[RistrettoPoint; CARD_COUNT]> = OnceLock::new();
    TABLE.get_or_init(|| core::array::from_fn(|card| card_point(card as u8)))
}

/// The ordered deck, before anyone shuffles it: card `j` at position `j`.
pub fn ordered_deck() -> Vec<RistrettoPoint> {
    card_table().to_vec()
}

/// Which card a point stands for, or `NotACard` if it stands for none.
///
/// This is the protocol's error detector as much as its decoder. A player who
/// locked with a key they did not commit to, or shuffled in a card that was
/// never in the deck, produces a point that is not any of the fifty-two — so a
/// card that fails to open is a cheat that failed, not a card that got lost.
pub fn card_of_point(point: &RistrettoPoint) -> Result<u8, MentalError> {
    card_table()
        .iter()
        .position(|candidate| candidate == point)
        .map(|card| card as u8)
        .ok_or(MentalError::NotACard)
}

/// Compresses a point to the 32 bytes that travel between players.
pub fn encode_point(point: &RistrettoPoint) -> [u8; 32] {
    point.compress().to_bytes()
}

/// Parses a point, refusing the identity.
///
/// The identity is refused everywhere, not only where it is obviously wrong. A
/// deck entry of `0` survives every scalar multiplication and opens to nothing,
/// and an identity proof base makes a sigma proof satisfiable without knowing
/// anything at all. Both are cheap to inject and cheap to reject.
pub fn decode_point(bytes: &[u8; 32]) -> Result<RistrettoPoint, MentalError> {
    let point = CompressedRistretto(*bytes)
        .decompress()
        .ok_or(MentalError::NotAGroupElement)?;
    if point == RistrettoPoint::identity() {
        return Err(MentalError::IdentityElement);
    }
    Ok(point)
}

/// Parses a whole deck.
pub fn decode_deck(bytes: &[[u8; 32]]) -> Result<Vec<RistrettoPoint>, MentalError> {
    if bytes.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }
    bytes.iter().map(decode_point).collect()
}

/// Serialises a whole deck.
pub fn encode_deck(points: &[RistrettoPoint]) -> Vec<[u8; 32]> {
    points.iter().map(encode_point).collect()
}

/// Serialises a scalar as the canonical little-endian 32 bytes.
pub fn encode_scalar(scalar: &Scalar) -> [u8; 32] {
    scalar.to_bytes()
}

/// Parses a scalar, refusing non-canonical encodings and zero.
///
/// Zero is refused because it is the one key that destroys a card instead of
/// hiding it, and non-canonical encodings because two spellings of one scalar
/// would let a published key match a commitment twice over.
pub fn decode_scalar(bytes: &[u8; 32]) -> Result<Scalar, MentalError> {
    let scalar = Option::<Scalar>::from(Scalar::from_canonical_bytes(*bytes))
        .ok_or(MentalError::NonCanonicalScalar)?;
    if scalar == Scalar::ZERO {
        return Err(MentalError::ZeroScalar);
    }
    Ok(scalar)
}

/// A player's private seed. Every secret they use in a hand comes from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Seed([u8; 32]);

impl Seed {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// A seed for one hand, derived from a longer-lived one.
    ///
    /// Every secret in this crate is a function of the seed and the label, and
    /// of nothing else — so two hands dealt from one seed reuse every key, and
    /// a key published to open a hole card in the first hand opens the same
    /// position in the second. That is the sharpest edge in the whole scheme.
    ///
    /// Deriving per hand is the defence, and the reason [`Seed`] means "one
    /// hand's seed" rather than "a player's key". `context` should be whatever
    /// makes a hand unique to its table — the table address and the hand id.
    pub fn for_hand(&self, context: &[u8]) -> Seed {
        let mut hasher = Sha512::new();
        hasher.update(HAND_DOMAIN);
        hasher.update(self.0);
        hasher.update((context.len() as u32).to_le_bytes());
        hasher.update(context);
        let digest = hasher.finalize();
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest[..32]);
        Seed(bytes)
    }

    /// The PRF the whole protocol's randomness is drawn from.
    ///
    /// `SHA-512(domain || seed || label || index)`, with the label
    /// length-prefixed so no two labels can run together into a third.
    fn stream(&self, label: &str, index: u32) -> [u8; 64] {
        let mut hasher = Sha512::new();
        hasher.update(PRF_DOMAIN);
        hasher.update(self.0);
        hasher.update([label.len() as u8]);
        hasher.update(label.as_bytes());
        hasher.update(index.to_le_bytes());
        hasher.finalize().into()
    }

    /// A secret scalar. Walks the index forward on the ~2^-250 chance of zero,
    /// so the caller never has to handle a key that would erase a card.
    pub fn scalar(&self, label: &str, index: u32) -> Scalar {
        let mut index = index;
        loop {
            let scalar = Scalar::from_bytes_mod_order_wide(&self.stream(label, index));
            if scalar != Scalar::ZERO {
                return scalar;
            }
            index = index.wrapping_add(1 << 24);
        }
    }

    /// A permutation of `len` items, by Fisher-Yates from the top.
    ///
    /// `result[j]` is the input position that ends up at output position `j`.
    /// The modulo is biased by about one part in 2^58, which is far below
    /// anything a card game can express and keeps the two implementations
    /// trivially identical.
    pub fn permutation(&self, label: &str, len: usize) -> Vec<u8> {
        let mut order: Vec<u8> = (0..len as u8).collect();
        for index in (1..len).rev() {
            let block = self.stream(label, index as u32);
            let mut draw = [0u8; 8];
            draw.copy_from_slice(&block[..8]);
            let swap = (u64::from_le_bytes(draw) % (index as u64 + 1)) as usize;
            order.swap(index, swap);
        }
        order
    }
}

/// A Fiat-Shamir transcript: everything a proof depends on, hashed in order.
///
/// Absorbing is length-prefixed at both the label and the payload, so no two
/// different sequences of absorbs can produce the same bytes. A challenge folds
/// the running digest back in, which is what stops two challenges drawn from
/// the same statement from being the same challenge.
#[derive(Clone)]
pub struct Transcript {
    hasher: Sha512,
}

impl Transcript {
    pub fn new(label: &str) -> Self {
        let mut transcript = Self { hasher: Sha512::new() };
        transcript.hasher.update(TRANSCRIPT_DOMAIN);
        transcript.absorb("transcript", label.as_bytes());
        transcript
    }

    pub fn absorb(&mut self, label: &str, bytes: &[u8]) {
        self.hasher.update([label.len() as u8]);
        self.hasher.update(label.as_bytes());
        self.hasher.update((bytes.len() as u32).to_le_bytes());
        self.hasher.update(bytes);
    }

    pub fn absorb_u8(&mut self, label: &str, value: u8) {
        self.absorb(label, &[value]);
    }

    pub fn absorb_point(&mut self, label: &str, point: &RistrettoPoint) {
        self.absorb(label, &encode_point(point));
    }

    pub fn absorb_points(&mut self, label: &str, points: &[RistrettoPoint]) {
        let mut flat = Vec::with_capacity(points.len() * 32);
        for point in points {
            flat.extend_from_slice(&encode_point(point));
        }
        self.absorb(label, &flat);
    }

    /// The running digest, without disturbing it.
    pub fn digest(&self) -> [u8; 64] {
        self.hasher.clone().finalize().into()
    }

    pub fn challenge(&mut self, label: &str) -> Scalar {
        self.absorb(label, b"");
        let digest = self.digest();
        self.absorb("challenge", &digest);
        Scalar::from_bytes_mod_order_wide(&digest)
    }

    /// A vector of challenges, all drawn from one statement.
    ///
    /// Each draw folds the running digest back in, so the scalars differ even
    /// though the label does not, and the count is absorbed first so a verifier
    /// expecting a different length cannot be walked onto the same bytes.
    pub fn challenge_vector(&mut self, label: &str, count: usize) -> Vec<Scalar> {
        self.absorb("count", &(count as u32).to_le_bytes());
        (0..count).map(|_| self.challenge(label)).collect()
    }

    /// A proof nonce bound to both the prover's seed and the statement so far.
    ///
    /// Deterministic in the manner of RFC 6979: there is no randomness to get
    /// wrong, and two different statements cannot share a nonce, which is the
    /// mistake that leaks the secret outright.
    pub fn nonce(&self, seed: &Seed, label: &str, index: u32) -> Scalar {
        let mut hasher = Sha512::new();
        hasher.update(NONCE_DOMAIN);
        hasher.update(seed.as_bytes());
        hasher.update(self.digest());
        hasher.update([label.len() as u8]);
        hasher.update(label.as_bytes());
        hasher.update(index.to_le_bytes());
        Scalar::from_bytes_mod_order_wide(&hasher.finalize().into())
    }
}
