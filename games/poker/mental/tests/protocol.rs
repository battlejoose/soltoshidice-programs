//! What the scheme claims, tested as claims rather than as code paths.
//!
//! Four things have to hold, and each is worth more as a failing test than as a
//! paragraph in a design document:
//!
//! * N players jointly shuffle, and the ordering is the composition of all of
//!   them — so no player, and no proper subset, knows where a card went.
//! * A hole card opens for its owner and for nobody else, including every other
//!   player working together.
//! * A board card opens for a threshold, and does not open for one fewer.
//! * Tampering is refused where it happens, not discovered later.

use riverproof_mental::group::{
    basepoint, card_of_point, card_point, decode_deck, encode_point, ordered_deck, Seed,
    CARD_COUNT,
};
use riverproof_mental::protocol::{deal_hand, steer_last_order, Session};
use riverproof_mental::share::{board_threshold, recover, Recipient};
use riverproof_mental::shuffle::{audit, compose, invert_order, shuffle};
use riverproof_mental::{MentalError, MIN_PLAYERS};

use curve25519_dalek::scalar::Scalar;

/// Distinct, reproducible seeds. Real play draws 32 bytes of entropy per player
/// and never writes them down.
fn seeds(count: usize) -> Vec<Seed> {
    (0..count)
        .map(|player| {
            let mut bytes = [0u8; 32];
            bytes[0] = player as u8;
            bytes[1] = 0xa7;
            Seed::new(bytes)
        })
        .collect()
}

/// Five board positions, as a hand with `players` seats would lay them out.
fn board_positions(players: usize) -> Vec<u8> {
    let dealt = (players * 2) as u8;
    vec![dealt + 1, dealt + 2, dealt + 3, dealt + 5, dealt + 7]
}

fn three_player_session() -> Session {
    deal_hand(&seeds(3), &board_positions(3), None).expect("a three-player deal")
}

#[test]
fn a_deal_produces_all_fifty_two_cards_exactly_once() {
    for players in MIN_PLAYERS as usize..=9 {
        let session = deal_hand(&seeds(players), &board_positions(players), None)
            .unwrap_or_else(|error| panic!("{players} players: {error:?}"));

        let mut deck = session.deck().expect("the deck opens");
        assert_eq!(deck.len(), CARD_COUNT, "{players} players: deck size");

        deck.sort_unstable();
        let ordered: Vec<u8> = (0..CARD_COUNT as u8).collect();
        assert_eq!(deck, ordered, "{players} players: not a permutation of the deck");
    }
}

/// The deck a shuffle produces is the composition of every player's ordering,
/// which is what "no single player knows the deck" actually rests on: a player
/// knows their own permutation and nothing about where it sends a card next.
#[test]
fn the_ordering_is_every_player_s_ordering_composed() {
    let players = 4;
    let session = deal_hand(&seeds(players), &board_positions(players), None)
        .expect("a four-player deal");

    // Recompute the composition from each player's own permutation, the way the
    // players together could and none of them alone can.
    let mut composed: Vec<u8> = (0..CARD_COUNT as u8).collect();
    for (index, seed) in seeds(players).iter().enumerate() {
        let (_, secret) = shuffle(seed, index as u8, &ordered_deck(), None).expect("a round");
        composed = compose(&composed, &secret.order);
    }

    assert_eq!(session.deck().expect("the deck opens"), composed);

    // And no player's own ordering is the answer on its own.
    let (_, first) = shuffle(&seeds(players)[0], 0, &ordered_deck(), None).expect("a round");
    assert_ne!(first.order, composed, "one player's ordering was the whole deal");
}

/// The steering the differential suite depends on. Every player but the last
/// shuffles freely; the last composes onto a chosen deck.
#[test]
fn the_joint_shuffle_can_be_steered_onto_a_chosen_deck() {
    let players = 3;
    let target: Vec<u8> = (0..CARD_COUNT as u8).rev().collect();

    let mut orders: Vec<Vec<u8>> = seeds(players - 1)
        .iter()
        .map(|seed| seed.permutation("shuffle-order", CARD_COUNT))
        .collect();
    orders.push(steer_last_order(&orders, &target));

    let session = deal_hand(&seeds(players), &board_positions(players), Some(&orders))
        .expect("a steered deal");
    assert_eq!(session.deck().expect("the deck opens"), target);

    // The players who were not steered still shuffled: their orderings are
    // their own, and neither is the target.
    for order in orders.iter().take(players - 1) {
        assert_ne!(*order, target);
    }
}

/// The design's first claim: a hole card is encrypted to its owner alone.
#[test]
fn only_the_owner_opens_a_hole_card() {
    let session = three_player_session();
    let deck = session.deck().expect("the deck opens");

    for owner in 0..3u8 {
        for position in [owner, owner + 3] {
            assert_eq!(
                session.open_hole(owner, position).expect("the owner opens it"),
                deck[position as usize],
            );

            // Every other player, together, peels everything they have. What is
            // left is a point that is not a card, because the owner's key is
            // still on it - and there is no threshold, no coalition, and no
            // share of it anywhere.
            let residual = session.residual(owner, position).expect("a residual");
            assert_eq!(card_of_point(&residual), Err(MentalError::NotACard));

            // Nor is it in any share package: hole positions were never split.
            for player in 0..3u8 {
                assert_eq!(
                    session.deal.share_package(player, position).err(),
                    Some(MentalError::PositionOutOfRange),
                    "seat {player} split a hole card's key",
                );
            }
        }
    }
}

/// The second claim: the board belongs to nobody, so all but one open it and a
/// player who walks away does not take it with them.
#[test]
fn a_threshold_opens_the_board_and_one_fewer_does_not() {
    for players in MIN_PLAYERS as usize..=9 {
        let positions = board_positions(players);
        let session = deal_hand(&seeds(players), &positions, None)
            .unwrap_or_else(|error| panic!("{players} players: {error:?}"));
        let deck = session.deck().expect("the deck opens");
        let threshold = board_threshold(players as u8) as usize;
        assert_eq!(session.deal.threshold as usize, threshold);

        let present: Vec<u8> = (0..threshold as u8).collect();
        let short: Vec<u8> = present[..threshold - 1].to_vec();

        for position in &positions {
            assert_eq!(
                session.open_board(*position, &present).expect("all but one opens it"),
                deck[*position as usize],
                "{players} players: position {position}",
            );
            assert_eq!(
                session.open_board(*position, &short),
                Err(MentalError::NotEnoughShares),
                "{players} players: a minority opened the board",
            );
        }
    }
}

/// One short of the threshold is refused by the arithmetic, not only by the
/// count: interpolating fewer points than the polynomial's degree recovers a
/// different scalar, which then fails its commitment.
#[test]
fn too_few_shares_recover_the_wrong_key() {
    let session = three_player_session();
    let position = session.deal.shared_positions[0];
    let threshold = session.deal.threshold;

    let absent = 2u8;
    let package = session.deal.share_package(absent, position).expect("a package");
    let dealer_public = session.deal.public_key(absent).expect("a public key");

    let shares: Vec<(u8, Scalar)> = (0..threshold)
        .map(|holder| {
            let value = session.participants[holder as usize]
                .read_share(package, &dealer_public)
                .expect("a valid share");
            (holder, value)
        })
        .collect();

    let commitment = session.deal.key_commitment(absent, position).expect("a commitment");
    let recovered = recover(&shares, threshold).expect("a threshold recovers");
    assert_eq!(encode_point(&(basepoint() * recovered)), commitment);

    // Drop one and interpolate anyway, ignoring the count check. The result is
    // a perfectly good scalar that is not the key.
    let short = recover(&shares[..threshold as usize - 1], threshold - 1)
        .expect("fewer points still interpolate");
    assert_ne!(encode_point(&(basepoint() * short)), commitment);
}

/// A share that was tampered with, or addressed elsewhere, is caught by the
/// holder before it can be used - which is the point of Feldman over Shamir.
#[test]
fn a_tampered_share_is_refused_by_its_holder() {
    let session = three_player_session();
    let position = session.deal.shared_positions[0];
    let dealer = 0u8;
    let dealer_public = session.deal.public_key(dealer).expect("a public key");
    let package = session.deal.share_package(dealer, position).expect("a package");

    assert!(session.participants[1].read_share(package, &dealer_public).is_ok());

    let mut tampered = package.clone();
    tampered.shares[1].ciphertext[0] ^= 1;
    assert_eq!(
        session.participants[1].read_share(&tampered, &dealer_public),
        Err(MentalError::BadShare),
    );

    // Somebody else's share does not open for the wrong holder either.
    let mut misaddressed = package.clone();
    misaddressed.shares[1].holder = 2;
    assert_eq!(
        session.participants[2].read_share(&misaddressed, &dealer_public),
        Err(MentalError::BadShare),
    );
}

#[test]
fn a_peel_that_does_not_match_its_commitment_is_refused() {
    let session = three_player_session();
    let position = session.deal.shared_positions[0];

    let honest = session.participants[1].peel(position).expect("a peel");
    assert!(session.deal.check_peel(&honest).is_ok());

    // The right player, the right position, a key they never committed to.
    let mut forged = honest;
    forged.key = riverproof_mental::group::encode_scalar(&Seed::new([9u8; 32]).scalar("x", 0));
    assert_eq!(session.deal.check_peel(&forged), Err(MentalError::BadPeel));

    // Their real key, offered for a position it does not open.
    let mut misfiled = honest;
    misfiled.position = position + 1;
    assert_eq!(session.deal.check_peel(&misfiled), Err(MentalError::BadPeel));
}

#[test]
fn a_lock_round_with_a_doctored_proof_is_refused() {
    let session = three_player_session();

    for player in 0..3usize {
        let mut deal = session.deal.clone();
        deal.locks[player].lock_proof.responses[0][0] ^= 1;
        assert!(
            matches!(deal.verify(), Err(MentalError::BadProof)),
            "seat {player}: a doctored lock proof verified",
        );

        let mut deal = session.deal.clone();
        deal.locks[player].unmask_proof.challenge[0] ^= 1;
        assert!(
            matches!(deal.verify(), Err(MentalError::BadProof)),
            "seat {player}: a doctored unmask proof verified",
        );
    }
}

/// A player who locks a position under a key they did not commit to breaks the
/// proof, so the round never gets accepted in the first place.
#[test]
fn a_key_commitment_that_does_not_match_the_lock_is_refused() {
    let session = three_player_session();
    let mut deal = session.deal.clone();
    deal.locks[1].keys[7] = encode_point(&(basepoint() * Seed::new([3u8; 32]).scalar("x", 0)));
    assert_eq!(deal.verify(), Err(MentalError::BadProof));
}

/// The cheap shuffle cheat - one card in two places - costs nothing to catch,
/// so it is caught at the round rather than at the audit.
#[test]
fn a_duplicated_card_is_refused_at_the_round() {
    let session = three_player_session();
    let mut deal = session.deal.clone();
    deal.shuffles[1].deck[4] = deal.shuffles[1].deck[9];
    assert_eq!(deal.verify(), Err(MentalError::DuplicateCard));
}

#[test]
fn a_deal_refuses_fewer_than_three_players() {
    assert_eq!(
        deal_hand(&seeds(2), &board_positions(2), None).err(),
        Some(MentalError::TooFewPlayers),
    );
    assert!(deal_hand(&seeds(3), &board_positions(3), None).is_ok());
}

/// A shuffler who substitutes a deck they chose is refused where it happens.
///
/// This test used to assert the opposite half of its own name: that the forged
/// round "passes every check a hand can run", with the audit as the only
/// remedy. The shuffle argument is what closed that, so the assertion inverts —
/// and the audit stays, because it is still the only executable statement of
/// what the shuffle guarantees and the only lever left if the two
/// implementations of the verifier ever disagree.
#[test]
fn a_substituted_deck_is_refused_at_publish_time() {
    let session = three_player_session();
    let masks = session.masks().expect("the masks");

    assert_eq!(audit(&session.deal.shuffles, &masks), Ok(()));

    // Seat 1 discards what it was handed and puts back a deck it composed
    // itself: fifty-two real cards, distinct, in an order it knows, and scaled
    // by the very mask it committed to. Everything the round can be checked
    // against on its own terms still holds.
    let mut deal = session.deal.clone();
    let chosen: Vec<u8> = (0..CARD_COUNT as u16).map(|card| ((card * 7 + 3) % 52) as u8).collect();
    let forged: Vec<[u8; 32]> = chosen
        .iter()
        .map(|card| encode_point(&(card_point(*card) * masks[1])))
        .collect();
    deal.shuffles[1].deck = forged;

    let handed_in = decode_deck(&session.deal.shuffles[0].deck).expect("seat 0's deck");
    assert_eq!(
        riverproof_mental::shuffle::distinct(&deal.shuffles[1].deck),
        Ok(()),
        "the forgery is fifty-two distinct real cards",
    );
    assert_eq!(
        riverproof_mental::shuffle::verify_shuffle(&deal.shuffles[1], &handed_in).err(),
        Some(MentalError::BadProof),
        "the argument refuses a deck that is not the one handed in",
    );
    assert_eq!(deal.verify().err(), Some(MentalError::BadProof));

    // And the audit still names the same seat, at the price it always cost.
    assert_eq!(
        audit(&deal.shuffles, &masks),
        Err(MentalError::ShuffleNotAPermutation { player: 1 }),
    );

    // A mask that is not the one committed to is named too.
    let mut wrong = masks.clone();
    wrong[2] = Seed::new([5u8; 32]).scalar("x", 0);
    assert_eq!(
        audit(&session.deal.shuffles, &wrong),
        Err(MentalError::MaskDoesNotMatchCommitment { player: 2 }),
    );
}

/// The audit's price, stated as a test so it cannot be forgotten: revealing the
/// masks reveals the deck, folded hands included. That is why it is a dispute
/// procedure and not a step of every hand.
#[test]
fn the_audit_costs_the_hand_its_privacy() {
    let session = three_player_session();
    let masks = session.masks().expect("the masks");

    // With every mask public, the deck after the shuffle rounds strips to the
    // card points - no lock keys needed, nobody's cooperation needed.
    let shuffled = decode_deck(&session.deal.shuffles[2].deck).expect("the shuffled deck");
    let combined = masks.iter().fold(Scalar::ONE, |product, mask| product * mask);
    let exposed: Vec<u8> = shuffled
        .iter()
        .map(|card| card_of_point(&(card * combined.invert())).expect("a card"))
        .collect();

    assert_eq!(exposed, session.deck().expect("the deck opens"));
}

#[test]
fn composing_and_inverting_orderings_agree() {
    let order = Seed::new([1u8; 32]).permutation("shuffle-order", CARD_COUNT);
    let identity: Vec<u8> = (0..CARD_COUNT as u8).collect();
    assert_eq!(compose(&order, &invert_order(&order)), identity);
    assert_eq!(compose(&invert_order(&order), &order), identity);
}

/// Reusing a seed across hands reuses every key, and `for_hand` is the defence.
///
/// This is the sharpest edge in the scheme and it is silent: a hand dealt from
/// a stale seed looks entirely normal, while the key a player published to open
/// somebody's hole card last hand opens the same position this hand. The first
/// half of the test is the hazard, so it cannot be argued away; the second is
/// the supported way out.
#[test]
fn a_hand_seed_is_derived_per_hand_or_every_key_repeats() {
    let root = Seed::new([0x11; 32]);

    // The hazard, stated plainly.
    assert_eq!(root.scalar("lock-key", 7), root.scalar("lock-key", 7));

    let first = root.for_hand(b"table-A/hand-1");
    let second = root.for_hand(b"table-A/hand-2");
    let other_table = root.for_hand(b"table-B/hand-1");

    for position in 0..CARD_COUNT as u32 {
        assert_ne!(first.scalar("lock-key", position), second.scalar("lock-key", position));
        assert_ne!(first.scalar("lock-key", position), other_table.scalar("lock-key", position));
        assert_ne!(first.scalar("lock-key", position), root.scalar("lock-key", position));
    }
    assert_ne!(first.scalar("shuffle-mask", 0), second.scalar("shuffle-mask", 0));
    assert_ne!(first.permutation("shuffle-order", CARD_COUNT), second.permutation("shuffle-order", CARD_COUNT));

    // Deterministic, so both halves of a hand derive the same seed.
    assert_eq!(first, root.for_hand(b"table-A/hand-1"));

    // And two hands really do deal differently end to end.
    let hands: Vec<Vec<u8>> = ["hand-1", "hand-2"]
        .iter()
        .map(|label| {
            let seeds: Vec<Seed> = seeds(3)
                .iter()
                .map(|seed| seed.for_hand(label.as_bytes()))
                .collect();
            deal_hand(&seeds, &board_positions(3), None)
                .expect("a deal")
                .deck()
                .expect("the deck opens")
        })
        .collect();
    assert_ne!(hands[0], hands[1], "two hands from one root seed dealt the same deck");
}

/// What a deal costs to publish, measured rather than estimated.
///
/// The numbers in `docs/DEALING.md` come from here. They matter because the
/// scheme's weakness is chattiness: it is free per hand and it is a lot of
/// bytes per hand, which is why it only makes sense alongside gasless
/// execution. If these move, that document is wrong.
#[test]
fn a_deal_costs_what_the_documentation_says() {
    let session = three_player_session();

    let shuffle_bytes = |round: &riverproof_mental::ShuffleRound| {
        // Every field counts, and this sum is hand-written, so every field
        // added to the round has to be added here too. `identity` once was not:
        // this measured 1,697 while the TypeScript half measured the 1,729 it
        // actually encodes — two green assertions, thirty-two bytes apart, each
        // correct about a different structure. The proof is the second time
        // that trap was available, and `a_proof_costs_what_the_documentation
        // _says` counts it independently for the same reason.
        let proof = &round.proof;
        let argument = (proof.permutation.len() + proof.bridges.len()) * 32
            + proof.challenge.len()
            + (proof.responses.len() + proof.bridge_responses.len()) * 32
            + proof.blind_response.len()
            + proof.ones_response.len()
            + proof.mask_response.len();
        1 + round.identity.len() + round.mask.len() + round.deck.len() * 32 + argument
    };
    let lock_bytes = |round: &riverproof_mental::LockRound| {
        let shares: usize = round
            .shares
            .iter()
            .map(|package| {
                2 + package.commitments.len() * 32 + package.shares.len() * (1 + 32)
            })
            .sum();
        1 + (round.unmasked.len() + round.deck.len() + round.keys.len()) * 32
            + 64
            + 32
            + round.lock_proof.responses.len() * 32
            + shares
    };

    let shuffle = shuffle_bytes(&session.deal.shuffles[0]);
    let lock = lock_bytes(&session.deal.locks[0]);
    // 1,729 bytes of round and 6,752 of argument.
    assert_eq!(shuffle, 8_481, "a shuffle round's published size changed");
    assert_eq!(lock, 7_578, "a three-player lock round's published size changed");

    // Every round is the same size: the protocol does not get cheaper or dearer
    // depending on who is shuffling.
    for round in &session.deal.shuffles {
        assert_eq!(shuffle_bytes(round), shuffle);
    }
    for round in &session.deal.locks {
        assert_eq!(lock_bytes(round), lock);
    }
}

#[test]
fn recipients_are_addressed_by_their_own_keys() {
    let session = three_player_session();
    let recipients: Vec<Recipient> = session.deal.recipients.clone();
    assert_eq!(recipients.len(), 3);
    for (index, recipient) in recipients.iter().enumerate() {
        assert_eq!(recipient.player, index as u8);
        assert_eq!(
            session.deal.public_key(index as u8).expect("a public key"),
            recipient.public_key,
        );
    }
    // Three distinct keys, so no two players can read each other's shares.
    for left in 0..3 {
        for right in left + 1..3 {
            assert_ne!(recipients[left].public_key, recipients[right].public_key);
        }
    }
}
