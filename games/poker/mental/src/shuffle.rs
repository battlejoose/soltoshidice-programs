//! The shuffle: each player masks the whole deck and permutes it, in turn.
//!
//! Player `i` multiplies every card by one secret scalar `s_i` and reorders
//! them. After every player has had a turn the deck is `S * P` for the product
//! of all the masks, in the composition of all the permutations — so no player
//! knows where any card went unless every other player tells them.
//!
//! Every round is proved as it happens. A shuffler who substitutes a deck they
//! chose produces something that looks exactly like a shuffle and opens exactly
//! like one, so distinctness and decoding cannot tell the difference —
//! [`crate::shuffle_proof`] is what does, by proving that the published deck is
//! the deck this player was handed under one permutation and one scalar. That
//! is why [`verify_shuffle`] takes the input deck: without it there is nothing
//! to prove a permutation *of*.
//!
//! [`audit`] survives that, demoted. Both of its failure modes are now
//! unreachable in play, but it is the break-glass for the one failure an
//! argument cannot rule out — the two implementations of the verifier
//! disagreeing with each other — and it is the only executable statement of
//! what the shuffle guarantees.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::group::{
    basepoint, decode_deck, decode_point, encode_deck, encode_point, ordered_deck, Seed, CARD_COUNT,
};
use crate::shuffle_proof::{prove_shuffle, verify_shuffle_proof, ShuffleProof};
use crate::MentalError;

/// What one player publishes when they shuffle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShuffleRound {
    pub player: u8,
    /// This player's key-exchange public key, for the shares they will be sent
    /// when everybody locks.
    ///
    /// It rides here rather than on chain for two reasons, and the first is
    /// security. Share packages are encrypted to these keys, so a substituted
    /// one hands its holder the share dealt to that player, and enough of those
    /// rebuild a board card's key before its street. Inside a shuffle round it
    /// is covered by the digest its author already commits on chain, signed,
    /// before anybody locks - so nothing carrying it between players can change
    /// it.
    ///
    /// The second is that it does not fit anywhere else. Ten of these on
    /// `MentalDeal` push Anchor's generated account validation past the BPF
    /// stack limit and the program stops loading. Here it costs no account
    /// space at all.
    pub identity: [u8; 32],
    /// `s_i * G`. The shuffle argument proves the deck was scaled by exactly
    /// this scalar, and [`crate::lock::verify_lock`] later proves the same one
    /// was stripped — which is what makes the two rounds one chain of custody
    /// rather than two claims that happen to sit next to each other.
    pub mask: [u8; 32],
    pub deck: Vec<[u8; 32]>,
    /// That this deck is the deck handed in, permuted and scaled by `mask`.
    pub proof: ShuffleProof,
}

/// What that player keeps.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShuffleSecret {
    pub mask: Scalar,
    /// `order[j]` is the input position that ends up at output position `j`.
    pub order: Vec<u8>,
}

/// Every entry distinct, which is what makes a reordering a permutation.
///
/// A player who maps two positions to one input produces two equal points, and
/// two equal points are two of the same card. Catching that costs one sort.
pub fn distinct(deck: &[[u8; 32]]) -> Result<(), MentalError> {
    let mut sorted: Vec<&[u8; 32]> = deck.iter().collect();
    sorted.sort_unstable();
    if sorted.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(MentalError::DuplicateCard);
    }
    Ok(())
}

/// Checks that `order` really is an ordering of `len` positions.
pub fn valid_order(order: &[u8], len: usize) -> Result<(), MentalError> {
    if order.len() != len {
        return Err(MentalError::NotAPermutation);
    }
    let mut seen = vec![false; len];
    for position in order {
        let slot = seen
            .get_mut(*position as usize)
            .ok_or(MentalError::NotAPermutation)?;
        if *slot {
            return Err(MentalError::NotAPermutation);
        }
        *slot = true;
    }
    Ok(())
}

/// The ordering two rounds produce together: `result[j] = first[second[j]]`.
pub fn compose(first: &[u8], second: &[u8]) -> Vec<u8> {
    second.iter().map(|position| first[*position as usize]).collect()
}

/// The ordering that undoes this one.
pub fn invert_order(order: &[u8]) -> Vec<u8> {
    let mut inverse = vec![0u8; order.len()];
    for (output, input) in order.iter().enumerate() {
        inverse[*input as usize] = output as u8;
    }
    inverse
}

/// Takes one turn at the deck.
///
/// `order` is supplied only by tests that need the joint shuffle to land on a
/// known deck; in play it is `None` and comes from the player's own seed, which
/// is the entire point.
pub fn shuffle(
    seed: &Seed,
    player: u8,
    input: &[RistrettoPoint],
    order: Option<&[u8]>,
) -> Result<(ShuffleRound, ShuffleSecret), MentalError> {
    if input.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }

    let order = match order {
        Some(order) => {
            valid_order(order, CARD_COUNT)?;
            order.to_vec()
        }
        None => seed.permutation("shuffle-order", CARD_COUNT),
    };
    let mask = seed.scalar("shuffle-mask", 0);
    // The same key `Participant` derives, so a player's identity is a function
    // of their seed like everything else and survives a reload.
    let identity = seed.scalar("identity", 0);

    let shuffled: Vec<RistrettoPoint> = order
        .iter()
        .map(|position| input[*position as usize] * mask)
        .collect();

    let mask_commitment = basepoint() * mask;
    let identity = encode_point(&(basepoint() * identity));
    let proof = prove_shuffle(
        seed,
        player,
        &identity,
        input,
        &shuffled,
        &mask_commitment,
        &mask,
        &order,
    )?;

    let round = ShuffleRound {
        player,
        identity,
        mask: encode_point(&mask_commitment),
        deck: encode_deck(&shuffled),
        proof,
    };
    Ok((round, ShuffleSecret { mask, order }))
}

/// What a round answers for on its own, with no reference to the deck it was
/// handed: fifty-two entries, every one a real group element, and no card
/// twice.
///
/// Split out because [`audit`] must not depend on the shuffle argument. The
/// audit exists for the one failure the argument cannot rule out — the two
/// implementations disagreeing about their own verifier — so an audit that
/// asked the argument first would be asking the suspect.
fn well_formed(round: &ShuffleRound) -> Result<Vec<RistrettoPoint>, MentalError> {
    distinct(&round.deck)?;
    decode_point(&round.identity)?;
    decode_deck(&round.deck)
}

/// Checks a round in full against the deck it was handed, and returns the deck
/// it produced.
///
/// `input` is what makes this a check rather than a formality. The deck being
/// the right size, every entry a real group element and no card appearing twice
/// are all true of a deck a cheat composed from scratch; only the argument
/// distinguishes that from a shuffle, and an argument needs both sides of the
/// statement.
pub fn verify_shuffle(
    round: &ShuffleRound,
    input: &[RistrettoPoint],
) -> Result<Vec<RistrettoPoint>, MentalError> {
    if input.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }
    let deck = well_formed(round)?;
    let mask = decode_point(&round.mask)?;
    verify_shuffle_proof(round.player, &round.identity, input, &deck, &mask, &round.proof)?;
    Ok(deck)
}

/// Settles a dispute by replaying every round against its revealed mask.
///
/// This is not a step of a hand, and running it costs the hand's privacy: with
/// every mask public, anybody can strip the deck and read every card in it,
/// including the ones players folded without showing. That price is only worth
/// paying once a card has already failed to open, when the hand is void anyway
/// and the question left is which player broke it.
///
/// Returns the player whose round was not a masked reordering of the round
/// before it. [`verify_shuffle`] refuses such a round at publish time now, so
/// reaching this means the two implementations of the argument disagree about
/// their own verifier — the one failure an argument cannot rule out, and the
/// reason this stays.
pub fn audit(rounds: &[ShuffleRound], masks: &[Scalar]) -> Result<(), MentalError> {
    if rounds.len() != masks.len() || rounds.is_empty() {
        return Err(MentalError::WrongPlayerCount);
    }

    let generator = basepoint();
    let mut previous = ordered_deck();

    for (round, mask) in rounds.iter().zip(masks) {
        if encode_point(&(generator * mask)) != round.mask {
            return Err(MentalError::MaskDoesNotMatchCommitment { player: round.player });
        }

        let deck = well_formed(round)?;
        let inverse = mask.invert();

        // Strip the mask and compare as multisets. Both sides are distinct, so
        // sorting the compressed bytes is enough: if the two agree, the round
        // reordered the deck it was handed and nothing else.
        let mut stripped: Vec<[u8; 32]> =
            deck.iter().map(|point| encode_point(&(point * inverse))).collect();
        let mut expected: Vec<[u8; 32]> = encode_deck(&previous);
        stripped.sort_unstable();
        expected.sort_unstable();
        if stripped != expected {
            return Err(MentalError::ShuffleNotAPermutation { player: round.player });
        }

        previous = deck;
    }
    Ok(())
}
