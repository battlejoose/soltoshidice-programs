//! The lock: one key per card position, replacing the shuffle masks.
//!
//! After the shuffle every card carries the same product of masks, which is
//! useless for dealing — removing it would open all fifty-two at once. So each
//! player takes a second turn, this time without reordering anything: they
//! strip their own mask off every position and put back a *different* fresh key
//! per position. Positions stay put, so from here on everyone can talk about
//! "position 7" and mean the same card.
//!
//! What that buys is the design's central claim. To open one card you need
//! every player's key for that one position, and those keys say nothing about
//! any other position. Publishing the key to somebody else's hole card is
//! therefore safe and total: it hands over exactly one card's worth of secret,
//! to exactly one player, and leaves your own cards untouched.
//!
//! This round is proved as it happens, and proved cheaply. Both steps are
//! statements about discrete logarithms with no permutation hiding inside them,
//! so a Chaum-Pedersen proof covers each: one that the mask stripped is the
//! mask that was committed to, one that each position was locked under the key
//! that was committed to. That is the difference from the shuffle, which is
//! also proved now but needs a whole permutation argument to do it
//! ([`crate::shuffle_proof`]) — not that this round is proved and that one is
//! not, which is what this comment used to say.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::group::{
    basepoint, decode_deck, decode_point, encode_deck, Seed, Transcript, CARD_COUNT,
};
use crate::proof::{
    prove_per_index, prove_same_scalar, verify_per_index, verify_same_scalar, PerIndexProof,
    SameScalarProof,
};
use crate::share::{deal_shares, shared_key_commitment, Recipient, SharePackage};
use crate::MentalError;

/// What one player publishes when they lock the deck.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockRound {
    pub player: u8,
    /// The deck with this player's shuffle mask stripped and nothing yet put
    /// back. Published because the two proofs meet here: one ends at it, the
    /// other starts from it.
    pub unmasked: Vec<[u8; 32]>,
    pub deck: Vec<[u8; 32]>,
    /// `k_j * G` for every position.
    pub keys: Vec<[u8; 32]>,
    pub unmask_proof: SameScalarProof,
    pub lock_proof: PerIndexProof,
    /// One package per board position, in the order the positions were given.
    pub shares: Vec<SharePackage>,
}

/// What that player keeps: the key to every position, one per card.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct LockSecret {
    pub keys: Vec<Scalar>,
}

/// The transcript both sides build, so a proof means the same thing to each.
fn transcript(player: u8, input: &[RistrettoPoint]) -> Transcript {
    let mut transcript = Transcript::new("lock");
    transcript.absorb_u8("player", player);
    transcript.absorb_points("input", input);
    transcript
}

/// Takes one turn locking the deck.
pub fn lock(
    seed: &Seed,
    player: u8,
    mask: &Scalar,
    identity_key: &Scalar,
    input: &[RistrettoPoint],
    recipients: &[Recipient],
    shared_positions: &[u8],
    threshold: u8,
) -> Result<(LockRound, LockSecret), MentalError> {
    if input.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }

    let generator = basepoint();
    let inverse = mask.invert();
    let unmasked: Vec<RistrettoPoint> = input.iter().map(|card| card * inverse).collect();

    let keys: Vec<Scalar> = (0..CARD_COUNT)
        .map(|position| seed.scalar("lock-key", position as u32))
        .collect();
    let commitments: Vec<RistrettoPoint> = keys.iter().map(|key| generator * key).collect();
    let locked: Vec<RistrettoPoint> = unmasked
        .iter()
        .zip(&keys)
        .map(|(card, key)| card * key)
        .collect();

    // The mask being stripped is the mask that was committed to: one exponent,
    // the basepoint and all fifty-two unmasked cards as bases.
    let mut transcript = transcript(player, input);
    let mut unmask_bases = vec![generator];
    unmask_bases.extend_from_slice(&unmasked);
    let mut unmask_images = vec![generator * mask];
    unmask_images.extend_from_slice(input);
    let unmask_proof =
        prove_same_scalar(&mut transcript, seed, mask, &unmask_bases, &unmask_images);

    // Each position locked under the key committed to for that position.
    let lock_proof =
        prove_per_index(&mut transcript, seed, &keys, &unmasked, &commitments, &locked);

    let mut shares = Vec::with_capacity(shared_positions.len());
    for position in shared_positions {
        let key = keys
            .get(*position as usize)
            .ok_or(MentalError::PositionOutOfRange)?;
        shares.push(deal_shares(
            seed,
            player,
            identity_key,
            *position,
            key,
            recipients,
            threshold,
        )?);
    }

    let round = LockRound {
        player,
        unmasked: encode_deck(&unmasked),
        deck: encode_deck(&locked),
        keys: encode_deck(&commitments),
        unmask_proof,
        lock_proof,
        shares,
    };
    Ok((round, LockSecret { keys }))
}

/// Checks a lock round in full, and returns the deck it produced.
///
/// One thing here is *not* checked, and cannot be: whether the encrypted shares
/// hold what the commitments say. Only the player a share is addressed to can
/// open it, so only they can tell — [`crate::share::open_share`] is where a bad
/// share is caught, by the one player it was meant to reach.
pub fn verify_lock(
    round: &LockRound,
    mask_commitment: &[u8; 32],
    input: &[RistrettoPoint],
    recipients: &[Recipient],
    shared_positions: &[u8],
    threshold: u8,
) -> Result<Vec<RistrettoPoint>, MentalError> {
    if input.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }
    crate::shuffle::distinct(&round.deck)?;

    let generator = basepoint();
    let unmasked = decode_deck(&round.unmasked)?;
    let locked = decode_deck(&round.deck)?;
    let commitments = decode_deck(&round.keys)?;

    let mut transcript = transcript(round.player, input);
    let mut unmask_bases = vec![generator];
    unmask_bases.extend_from_slice(&unmasked);
    let mut unmask_images = vec![decode_point(mask_commitment)?];
    unmask_images.extend_from_slice(input);
    verify_same_scalar(
        &mut transcript,
        &unmask_bases,
        &unmask_images,
        &round.unmask_proof,
    )?;

    verify_per_index(
        &mut transcript,
        &unmasked,
        &commitments,
        &locked,
        &round.lock_proof,
    )?;

    if round.shares.len() != shared_positions.len() {
        return Err(MentalError::BadShare);
    }
    for (package, position) in round.shares.iter().zip(shared_positions) {
        if package.dealer != round.player || package.position != *position {
            return Err(MentalError::BadShare);
        }
        if package.commitments.len() != threshold as usize {
            return Err(MentalError::BadThreshold);
        }
        // The split has to be of the key this position was actually locked
        // under, or a recovered key would open nothing.
        if shared_key_commitment(package)? != commitments[*position as usize] {
            return Err(MentalError::BadShare);
        }
        if package.shares.len() != recipients.len()
            || !recipients
                .iter()
                .all(|recipient| package.shares.iter().any(|s| s.holder == recipient.player))
        {
            return Err(MentalError::BadShare);
        }
    }

    Ok(locked)
}
