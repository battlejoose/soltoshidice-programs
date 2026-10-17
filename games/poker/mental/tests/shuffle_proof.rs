//! What the shuffle argument claims, and what it refuses.
//!
//! The honest path is covered by every other suite in this crate, because a
//! deal will not verify without it. What is only covered here is the half that
//! matters for soundness — the rounds it must *reject* — and the one property
//! that is invisible in a single honest transcript: that two rounds by one
//! player under one seed do not cancel each other's blinding.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::Identity;

use riverproof_mental::group::{
    basepoint, card_point, decode_deck, encode_point, multiscalar, ordered_deck, Seed, Transcript,
    CARD_COUNT,
};
use riverproof_mental::shuffle::{shuffle, verify_shuffle, ShuffleRound};
use riverproof_mental::shuffle_proof::{generators, verify_shuffle_proof, BRIDGE_COUNT};
use riverproof_mental::MentalError;

fn seed(byte: u8) -> Seed {
    let mut bytes = [0u8; 32];
    bytes[0] = byte;
    bytes[1] = 0x5e;
    Seed::new(bytes)
}

/// One honest round, and the deck it was handed.
fn round(byte: u8) -> (ShuffleRound, Vec<RistrettoPoint>) {
    let input = ordered_deck();
    let (round, _) = shuffle(&seed(byte), 0, &input, None).expect("a round");
    (round, input)
}

#[test]
fn an_honest_round_verifies_against_the_deck_it_was_handed() {
    let (round, input) = round(1);
    assert!(verify_shuffle(&round, &input).is_ok());
}

/// The seam the whole change turns on. `verify_shuffle` used to take only the
/// round, so it could not tell a shuffle from a deck composed out of thin air.
#[test]
fn a_round_is_refused_against_a_deck_it_was_not_handed() {
    let (round, _) = round(2);

    // A different deck of the same shape: every entry a real card point, none
    // repeated, fifty-two of them.
    let elsewhere: Vec<RistrettoPoint> =
        (0..CARD_COUNT as u8).map(|card| card_point(card) * Scalar::from(7u8)).collect();

    assert_eq!(
        verify_shuffle(&round, &elsewhere).err(),
        Some(MentalError::BadProof),
    );
}

/// A shuffler who publishes a deck of their own choosing, scaled by the very
/// mask they committed to. This is the forgery the audit used to be the only
/// answer to.
#[test]
fn a_substituted_deck_cannot_be_proved() {
    let (mut round, input) = round(3);
    let mask = seed(3).scalar("shuffle-mask", 0);

    let chosen: Vec<[u8; 32]> = (0..CARD_COUNT as u16)
        .map(|card| encode_point(&(card_point(((card * 11 + 5) % 52) as u8) * mask)))
        .collect();
    round.deck = chosen;

    assert_eq!(verify_shuffle(&round, &input).err(), Some(MentalError::BadProof));
}

/// The mask is part of the statement, so the argument and the lock round that
/// follows it cannot be about two different scalars.
#[test]
fn the_argument_is_bound_to_the_mask_the_round_committed_to() {
    let (mut round, input) = round(4);
    round.mask = encode_point(&(basepoint() * seed(9).scalar("shuffle-mask", 0)));
    assert_eq!(verify_shuffle(&round, &input).err(), Some(MentalError::BadProof));
}

/// A round cannot be lifted into another seat, or have its key-exchange key
/// swapped for somebody else's — which matters because share packages are
/// encrypted to that key.
#[test]
fn a_round_cannot_be_replayed_by_another_player_or_under_another_key() {
    let (round, input) = round(5);

    let mut moved = round.clone();
    moved.player = 1;
    assert_eq!(verify_shuffle(&moved, &input).err(), Some(MentalError::BadProof));

    let mut swapped = round.clone();
    swapped.identity = encode_point(&(basepoint() * seed(6).scalar("identity", 0)));
    assert_eq!(verify_shuffle(&swapped, &input).err(), Some(MentalError::BadProof));
}

/// The regression for the sharpest edge in the argument.
///
/// Both the permutation commitment and the bridging chain are *linear* in the
/// value they hide. Blindings drawn from the seed alone would be the same in
/// every round a player ever makes, so two rounds over different decks would
/// subtract to `Slot_π(j) − Slot_π'(j)` and to `(b_i − b'_i)·blind` — and since
/// the weights are public, a 52×52 table reads the ordering straight off.
///
/// Drawing them from the transcript is what stops that, and the ordering being
/// identical across the two rounds is what makes the test sharp: the seeds are
/// the same, so `π` is the same, and only the statement-bound blinding can be
/// making the commitments differ.
#[test]
fn two_rounds_under_one_seed_do_not_cancel_each_other_s_blinding() {
    let hand = seed(7);

    let first_input = ordered_deck();
    let second_input: Vec<RistrettoPoint> =
        first_input.iter().map(|card| card * Scalar::from(3u8)).collect();

    let (first, first_secret) = shuffle(&hand, 0, &first_input, None).expect("a round");
    let (second, second_secret) = shuffle(&hand, 0, &second_input, None).expect("a round");

    assert_eq!(
        first_secret.order, second_secret.order,
        "one seed, one ordering — otherwise this test proves nothing",
    );
    assert_eq!(first_secret.mask, second_secret.mask, "and one mask");

    for position in 0..CARD_COUNT {
        assert_ne!(
            first.proof.permutation[position], second.proof.permutation[position],
            "position {position} committed identically twice: the blinding is not \
             bound to the statement, and the two rounds subtract to the ordering",
        );
    }
    for position in 0..BRIDGE_COUNT {
        assert_ne!(
            first.proof.bridges[position], second.proof.bridges[position],
            "bridge {position} repeated: the chain's blinding is not bound either",
        );
    }
}

/// A permutation commitment that is not a permutation: one column repeated, so
/// the committed matrix has a zero row and a doubled one.
#[test]
fn a_commitment_that_is_not_a_permutation_matrix_is_refused() {
    let (mut round, input) = round(8);
    round.proof.permutation[1] = round.proof.permutation[0];
    assert_eq!(verify_shuffle(&round, &input).err(), Some(MentalError::BadProof));
}

/// Every field of the proof is covered by the challenge, so moving any one of
/// them breaks it. Stated as a sweep rather than a test each, because the point
/// is that there is no field which is *not* covered.
#[test]
fn every_field_of_the_proof_is_covered_by_its_own_challenge() {
    let (round, input) = round(9);
    let other_point = encode_point(&(basepoint() * Scalar::from(9u8)));
    let other_scalar = *Seed::new([3u8; 32]).scalar("x", 0).as_bytes();

    let mutations: Vec<(&str, Box<dyn Fn(&mut ShuffleRound)>)> = vec![
        ("permutation", Box::new(move |r: &mut ShuffleRound| r.proof.permutation[17] = other_point)),
        ("bridges", Box::new(move |r: &mut ShuffleRound| r.proof.bridges[17] = other_point)),
        ("challenge", Box::new(move |r: &mut ShuffleRound| r.proof.challenge = other_scalar)),
        ("responses", Box::new(move |r: &mut ShuffleRound| r.proof.responses[17] = other_scalar)),
        ("bridge-responses", Box::new(move |r: &mut ShuffleRound| r.proof.bridge_responses[17] = other_scalar)),
        ("blind-response", Box::new(move |r: &mut ShuffleRound| r.proof.blind_response = other_scalar)),
        ("ones-response", Box::new(move |r: &mut ShuffleRound| r.proof.ones_response = other_scalar)),
        ("mask-response", Box::new(move |r: &mut ShuffleRound| r.proof.mask_response = other_scalar)),
    ];

    for (name, mutate) in mutations {
        let mut forged = round.clone();
        mutate(&mut forged);
        assert_eq!(
            verify_shuffle(&forged, &input).err(),
            Some(MentalError::BadProof),
            "moving `{name}` did not break the proof",
        );
    }
}

/// The identity and non-canonical encodings are refused by name rather than as
/// a failed check, so an adversarial fixture can say which branch it hit.
#[test]
fn degenerate_encodings_inside_a_proof_are_named() {
    let (round, input) = round(10);
    let zero = encode_point(&RistrettoPoint::identity());

    let mut identity_commitment = round.clone();
    identity_commitment.proof.permutation[0] = zero;
    assert_eq!(
        verify_shuffle(&identity_commitment, &input).err(),
        Some(MentalError::IdentityElement),
    );

    let mut identity_bridge = round.clone();
    identity_bridge.proof.bridges[0] = zero;
    assert_eq!(
        verify_shuffle(&identity_bridge, &input).err(),
        Some(MentalError::IdentityElement),
    );

    // A scalar with a second spelling, and the zero scalar, are both bad
    // proofs rather than bad encodings: inside a proof there is no difference.
    let mut noncanonical = round.clone();
    noncanonical.proof.responses[0] = [0xff; 32];
    assert_eq!(
        verify_shuffle(&noncanonical, &input).err(),
        Some(MentalError::BadProof),
    );

    let mut zero_response = round.clone();
    zero_response.proof.responses[0] = [0u8; 32];
    assert_eq!(
        verify_shuffle(&zero_response, &input).err(),
        Some(MentalError::BadProof),
    );

    let mut not_a_point = round.clone();
    not_a_point.proof.permutation[0] = [0xff; 32];
    assert_eq!(
        verify_shuffle(&not_a_point, &input).err(),
        Some(MentalError::NotAGroupElement),
    );
}

/// A proof of the wrong shape is refused before any arithmetic runs.
#[test]
fn a_proof_of_the_wrong_shape_is_refused() {
    let (round, input) = round(11);
    let mask = riverproof_mental::group::decode_point(&round.mask).expect("the mask");
    let deck = decode_deck(&round.deck).expect("the deck");

    let mut short = round.proof.clone();
    short.bridges.pop();
    assert_eq!(
        verify_shuffle_proof(round.player, &round.identity, &input, &deck, &mask, &short).err(),
        Some(MentalError::BadProof),
    );

    let mut long = round.proof.clone();
    long.responses.push([1u8; 32]);
    assert_eq!(
        verify_shuffle_proof(round.player, &round.identity, &input, &deck, &mask, &long).err(),
        Some(MentalError::BadProof),
    );
}

/// The size quoted in the documentation, checked rather than asserted in prose.
#[test]
fn a_proof_costs_what_the_documentation_says() {
    let (round, _) = round(12);
    let proof = &round.proof;
    let points = proof.permutation.len() + proof.bridges.len();
    let scalars =
        1 + proof.responses.len() + proof.bridge_responses.len() + 3;

    assert_eq!(points, 103);
    assert_eq!(scalars, 108);
    assert_eq!((points + scalars) * 32, 6_752, "the shuffle argument changed size");
}

/// The generators are hashed into the group, not derived from each other, and
/// nothing in the argument may assume otherwise.
#[test]
fn the_generators_are_distinct_and_none_is_the_identity() {
    let generators = generators();
    let mut all = vec![generators.blind, generators.bridge];
    all.extend(generators.slots.iter().copied());

    assert_eq!(all.len(), CARD_COUNT + 2);
    assert!(all.iter().all(|point| *point != RistrettoPoint::identity()));

    let mut encoded: Vec<[u8; 32]> = all.iter().map(encode_point).collect();
    encoded.sort_unstable();
    encoded.dedup();
    assert_eq!(encoded.len(), CARD_COUNT + 2, "two generators collided");

    assert_eq!(
        generators.slot_sum,
        generators.slots.iter().sum::<RistrettoPoint>(),
        "the cached sum is not the sum",
    );
}

/// The batched primitive both halves verify with has to agree with the sum it
/// replaces, or one half accepts what the other refuses.
#[test]
fn multiscalar_agrees_with_adding_one_point_at_a_time() {
    let deck = ordered_deck();
    let scalars: Vec<Scalar> = (0..CARD_COUNT)
        .map(|index| Seed::new([9u8; 32]).scalar("msm", index as u32))
        .collect();

    let batched = multiscalar(&deck, &scalars);
    let one_at_a_time: RistrettoPoint =
        deck.iter().zip(&scalars).map(|(point, scalar)| point * scalar).sum();
    assert_eq!(batched, one_at_a_time);

    // Zero scalars and an empty sum are both legitimate here — rebuilding a
    // prover's commitments can land on either.
    assert_eq!(multiscalar(&[], &[]), RistrettoPoint::identity());
    assert_eq!(
        multiscalar(&deck[..2], &[Scalar::ZERO, Scalar::ZERO]),
        RistrettoPoint::identity(),
    );
}

/// Fifty-two challenges drawn from one statement have to be fifty-two different
/// challenges, or the weights collapse and the product identity proves nothing.
#[test]
fn a_challenge_vector_is_not_one_challenge_repeated() {
    let mut transcript = Transcript::new("test");
    transcript.absorb("statement", b"riverproof");
    let weights = transcript.challenge_vector("weights", CARD_COUNT);

    assert_eq!(weights.len(), CARD_COUNT);
    let mut seen: Vec<[u8; 32]> = weights.iter().map(|weight| weight.to_bytes()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), CARD_COUNT);

    // And the count is absorbed, so a verifier drawing a different number of
    // them does not walk onto the same bytes.
    let mut shorter = Transcript::new("test");
    shorter.absorb("statement", b"riverproof");
    let fewer = shorter.challenge_vector("weights", CARD_COUNT - 1);
    assert_ne!(fewer[0], weights[0]);
}
