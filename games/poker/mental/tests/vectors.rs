//! The fixture both implementations are held to.
//!
//! Rust deals a hand here and the browser deals the same hand in
//! `tests/mental.test.mts`. Two implementations of a protocol drift, and the
//! drift is invisible until a hand will not open — so neither is trusted to
//! match a written specification. Both derive every secret from one seed
//! through the same PRF, which makes the whole transcript a function of the
//! seeds, and the fixture records what that function returns.
//!
//! Regenerate with:
//!
//! ```text
//! UPDATE_MENTAL_VECTORS=1 cargo test -p riverproof-mental --test vectors
//! ```
//!
//! Doing so is a protocol change, not a test fix. If the fixture moves without
//! the protocol moving, one of the two implementations has drifted and the
//! answer is to find out which.

use riverproof_engine::table::{Seat, Table, TableConfig, MAX_SEATS};

use riverproof_mental::group::{
    basepoint, card_point, decode_deck, encode_point, encode_scalar, multiscalar, ordered_deck,
    Seed, Transcript, CARD_COUNT,
};
use riverproof_mental::protocol::{deal_hand, lock_round_digest, shuffle_round_digest, Session};
use riverproof_mental::share::board_threshold;
use riverproof_mental::shuffle::{verify_shuffle, ShuffleRound};
use riverproof_mental::MentalError;

use curve25519_dalek::scalar::Scalar;
use serde_json::{json, Value};

const FIXTURE: &str = "../../tests/fixtures/mental-vectors.json";

/// The seeds the fixture is built from. Fixed on purpose: play draws 32 bytes
/// of real entropy per player, and a fixture needs the opposite.
fn seeds() -> Vec<Seed> {
    (0..3u8)
        .map(|player| {
            let mut bytes = [0u8; 32];
            bytes[0] = player;
            bytes[1] = 0xc0;
            bytes[31] = 0x0d;
            Seed::new(bytes)
        })
        .collect()
}

/// A three-handed table's board positions: six hole cards, then burn-and-deal.
const SHARED_POSITIONS: [u8; 5] = [7, 8, 9, 11, 13];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn built() -> Session {
    deal_hand(&seeds(), &SHARED_POSITIONS, None).expect("the fixture deal")
}

/// Deck positions taken from the engine itself, so the browser's mirror of
/// `Table::deal_positions` is pinned to the engine and not to a copy of it.
fn engine_positions(max_players: u8, occupied: &[u8]) -> (u8, Vec<u8>, Value) {
    let mut table = Table::new(TableConfig::new(50, 0, max_players, 5_000, 30_000));
    for seat in occupied {
        table.seats[*seat as usize] = Seat {
            occupied: true,
            stack: 5_000,
            ready: true,
            connected: true,
            leave_after_hand: false,
        };
    }
    table.start_hand(riverproof_engine::cards::ordered_deck(), 0).expect("a hand starts");

    let hand = table.hand.as_ref().expect("a hand");
    let dealer_seat = hand.dealer_seat;
    let active: Vec<u8> = riverproof_engine::pots::seats_of(hand.active_seats).collect();
    let positions = table.deal_positions().expect("a hand is in progress");

    let value = json!({
        "maxPlayers": max_players,
        "dealerSeat": dealer_seat,
        "activeSeats": active.clone(),
        "hole": (0..MAX_SEATS).map(|seat| positions.hole[seat].to_vec()).collect::<Vec<_>>(),
        "board": positions.board.to_vec(),
    });
    (dealer_seat, active, value)
}

/// One edit to a shuffle round, named the same way in both implementations.
struct Edit {
    field: &'static str,
    index: usize,
    value: [u8; 32],
}

/// A forged round, and the error each half must answer it with.
struct Forgery {
    name: &'static str,
    /// `"handed-in"` verifies against the deck the round really followed;
    /// `"ordered"` verifies it against the wrong deck entirely.
    against: &'static str,
    edits: Vec<Edit>,
    error: &'static str,
}

/// Applies one edit. Deliberately small and dull: it is hand-mirrored in
/// TypeScript, so anything clever here is a second place for the two halves to
/// disagree.
fn apply(round: &mut ShuffleRound, edit: &Edit) {
    match edit.field {
        "player" => round.player = edit.value[0],
        "identity" => round.identity = edit.value,
        "mask" => round.mask = edit.value,
        "deck" => round.deck[edit.index] = edit.value,
        "permutation" => round.proof.permutation[edit.index] = edit.value,
        "bridges" => round.proof.bridges[edit.index] = edit.value,
        "challenge" => round.proof.challenge = edit.value,
        "responses" => round.proof.responses[edit.index] = edit.value,
        "bridge-responses" => round.proof.bridge_responses[edit.index] = edit.value,
        "blind-response" => round.proof.blind_response = edit.value,
        "ones-response" => round.proof.ones_response = edit.value,
        "mask-response" => round.proof.mask_response = edit.value,
        other => panic!("no such field: {other}"),
    }
}

fn named(error: &MentalError) -> String {
    match error {
        MentalError::MaskDoesNotMatchCommitment { .. } => "MaskDoesNotMatchCommitment".into(),
        MentalError::ShuffleNotAPermutation { .. } => "ShuffleNotAPermutation".into(),
        other => format!("{other:?}"),
    }
}

/// The forged rounds both halves are held to.
///
/// An honest transcript pins only that the two implementations accept the same
/// valid thing. A permutation argument's soundness lives entirely in what it
/// refuses, and two verifiers can agree on every honest round while disagreeing
/// about which forgeries they reject — a missing identity check on one side, a
/// challenge absorbed one step out of order in a rejection branch. So the
/// forgeries are fixture data too, with the error each half must name.
fn forgeries(session: &Session) -> Vec<Forgery> {
    // Seat 1's round, which followed seat 0's deck.
    let round = &session.deal.shuffles[1];
    let mask = session.masks().expect("the masks")[1];
    let elsewhere = encode_point(&(basepoint() * Scalar::from(9u8)));
    let scalar = *Seed::new([3u8; 32]).scalar("adversarial", 0).as_bytes();
    let identity_point = encode_point(&curve25519_dalek::ristretto::RistrettoPoint::default());

    let point_edit = |field, index, value| Edit { field, index, value };

    let mut forgeries = vec![
        Forgery {
            name: "an honest round, against the deck it was handed",
            against: "handed-in",
            edits: vec![],
            error: "Ok",
        },
        Forgery {
            name: "the same round, against a deck it never saw",
            against: "ordered",
            edits: vec![],
            error: "BadProof",
        },
        Forgery {
            name: "one card swapped for another real card",
            against: "handed-in",
            edits: vec![point_edit("deck", 7, encode_point(&(card_point(19) * mask)))],
            error: "BadProof",
        },
        Forgery {
            name: "one card duplicated",
            against: "handed-in",
            edits: vec![point_edit("deck", 7, round.deck[8])],
            error: "DuplicateCard",
        },
        Forgery {
            name: "a mask that is not the one the argument was made under",
            against: "handed-in",
            edits: vec![point_edit("mask", 0, elsewhere)],
            error: "BadProof",
        },
        Forgery {
            name: "somebody else's key-exchange key",
            against: "handed-in",
            edits: vec![point_edit("identity", 0, elsewhere)],
            error: "BadProof",
        },
        Forgery {
            name: "the round lifted into another seat",
            against: "handed-in",
            edits: vec![Edit { field: "player", index: 0, value: [2u8; 32] }],
            error: "BadProof",
        },
        Forgery {
            name: "a permutation commitment with a column repeated",
            against: "handed-in",
            edits: vec![point_edit("permutation", 1, round.proof.permutation[0])],
            error: "BadProof",
        },
        Forgery {
            name: "a permutation commitment that is the identity",
            against: "handed-in",
            edits: vec![point_edit("permutation", 0, identity_point)],
            error: "IdentityElement",
        },
        Forgery {
            name: "a permutation commitment that is not a group element",
            against: "handed-in",
            edits: vec![point_edit("permutation", 0, [0xff; 32])],
            error: "NotAGroupElement",
        },
        Forgery {
            name: "a bridging commitment moved",
            against: "handed-in",
            edits: vec![point_edit("bridges", 17, elsewhere)],
            error: "BadProof",
        },
        Forgery {
            name: "a bridging commitment that is the identity",
            against: "handed-in",
            edits: vec![point_edit("bridges", 0, identity_point)],
            error: "IdentityElement",
        },
        Forgery {
            name: "the challenge moved",
            against: "handed-in",
            edits: vec![point_edit("challenge", 0, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "one weight response moved",
            against: "handed-in",
            edits: vec![point_edit("responses", 17, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "one bridge response moved",
            against: "handed-in",
            edits: vec![point_edit("bridge-responses", 17, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "the commitment blinding response moved",
            against: "handed-in",
            edits: vec![point_edit("blind-response", 0, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "the row-sum response moved",
            against: "handed-in",
            edits: vec![point_edit("ones-response", 0, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "the mask response moved",
            against: "handed-in",
            edits: vec![point_edit("mask-response", 0, scalar)],
            error: "BadProof",
        },
        Forgery {
            name: "a response with a second spelling",
            against: "handed-in",
            edits: vec![point_edit("responses", 0, [0xff; 32])],
            error: "BadProof",
        },
        Forgery {
            name: "a response of zero",
            against: "handed-in",
            edits: vec![point_edit("responses", 0, [0u8; 32])],
            error: "BadProof",
        },
    ];

    // Seat 1 discards the deck it was handed and publishes fifty-two cards it
    // chose, scaled by the mask it committed to. The forgery the audit used to
    // be the only answer to.
    forgeries.push(Forgery {
        name: "a whole deck of the shuffler's own choosing",
        against: "handed-in",
        edits: (0..CARD_COUNT)
            .map(|index| {
                point_edit(
                    "deck",
                    index,
                    encode_point(&(card_point(((index * 11 + 5) % 52) as u8) * mask)),
                )
            })
            .collect(),
        error: "BadProof",
    });
    forgeries
}

fn adversarial(session: &Session) -> Value {
    let honest = &session.deal.shuffles[1];
    let handed_in = decode_deck(&session.deal.shuffles[0].deck).expect("seat 0's deck");

    let cases: Vec<Value> = forgeries(session)
        .iter()
        .map(|forgery| {
            let mut forged = honest.clone();
            for edit in &forgery.edits {
                apply(&mut forged, edit);
            }
            let input = match forgery.against {
                "handed-in" => handed_in.clone(),
                "ordered" => ordered_deck(),
                other => panic!("no such input: {other}"),
            };
            let outcome = match verify_shuffle(&forged, &input) {
                Ok(_) => "Ok".to_string(),
                Err(error) => named(&error),
            };
            assert_eq!(
                outcome, forgery.error,
                "`{}` was answered with {outcome}, not {}",
                forgery.name, forgery.error,
            );
            json!({
                "name": forgery.name,
                "against": forgery.against,
                "edits": forgery.edits.iter().map(|edit| json!({
                    "field": edit.field,
                    "index": edit.index,
                    "value": hex(&edit.value),
                })).collect::<Vec<_>>(),
                "error": forgery.error,
            })
        })
        .collect();

    json!({
        "note": concat!(
            "Forged shuffle rounds and the MentalError each implementation must ",
            "answer them with. Built from deal.shuffles[1]; `against` says which ",
            "deck to verify the result against, and `edits` are applied in order ",
            "before verifying. An honest transcript pins only what both halves ",
            "accept; this pins what they refuse.",
        ),
        "round": 1,
        "cases": cases,
    })
}

fn build_fixture() -> Value {
    let session = built();
    let deal = &session.deal;
    let seeds = seeds();
    let deck = session.deck().expect("the deck opens");

    // A transcript with one of every absorb, so the encoding itself is pinned
    // and not only its use inside a proof. Read in a fixed order, because a
    // challenge folds its own digest back in and the value therefore depends on
    // whether it has been drawn yet - which is the behaviour worth recording.
    let mut transcript = Transcript::new("fixture");
    transcript.absorb("bytes", b"riverproof");
    transcript.absorb_u8("player", 2);
    transcript.absorb_point("point", &card_point(17));
    let before_challenge = transcript.digest();
    let challenge = transcript.challenge("challenge");
    let after_challenge = transcript.digest();

    let mut hole_opens: Vec<Value> = Vec::new();
    for owner in 0..3u8 {
        for position in [owner, owner + 3] {
            hole_opens.push(json!({
                "owner": owner,
                "position": position,
                "card": session.open_hole(owner, position).expect("the owner opens it"),
            }));
        }
    }

    let threshold = board_threshold(3) as usize;
    let mut board_opens: Vec<Value> = Vec::new();
    for (index, position) in SHARED_POSITIONS.iter().enumerate() {
        // Rotate which players do the opening, so share recovery is exercised
        // for every dealer rather than only the last.
        let present: Vec<u8> = (0..threshold).map(|slot| ((index + slot) % 3) as u8).collect();
        board_opens.push(json!({
            "position": position,
            "present": present.clone(),
            "card": session.open_board(*position, &present).expect("a threshold opens it"),
        }));
    }

    let (_, _, three_handed) = engine_positions(3, &[0, 1, 2]);
    let (_, _, sparse) = engine_positions(10, &[0, 4, 7, 9]);
    let (_, _, six_handed) = engine_positions(6, &[1, 2, 3, 4, 5]);

    json!({
        "note": concat!(
            "Generated by program/mental/tests/vectors.rs. Regenerate with ",
            "UPDATE_MENTAL_VECTORS=1 cargo test -p riverproof-mental --test vectors. ",
            "A change here is a protocol change; see that file.",
        ),
        "cardPoints": (0..CARD_COUNT as u8)
            .map(|card| hex(&encode_point(&card_point(card))))
            .collect::<Vec<_>>(),
        "prf": {
            "seed": hex(seeds[0].as_bytes()),
            "scalars": [
                { "label": "shuffle-mask", "index": 0,
                  "value": hex(&encode_scalar(&seeds[0].scalar("shuffle-mask", 0))) },
                { "label": "lock-key", "index": 51,
                  "value": hex(&encode_scalar(&seeds[0].scalar("lock-key", 51))) },
                { "label": "identity", "index": 0,
                  "value": hex(&encode_scalar(&seeds[0].scalar("identity", 0))) },
            ],
            "permutation": {
                "label": "shuffle-order",
                "order": seeds[0].permutation("shuffle-order", CARD_COUNT),
            },
            "forHand": {
                "context": "table-A/hand-1",
                "seed": hex(seeds[0].for_hand(b"table-A/hand-1").as_bytes()),
            },
        },
        "transcript": {
            "digest": hex(&before_challenge),
            "challenge": hex(&encode_scalar(&challenge)),
            "digestAfterChallenge": hex(&after_challenge),
        },
        "deal": {
            "seeds": seeds.iter().map(|seed| hex(seed.as_bytes())).collect::<Vec<_>>(),
            "sharedPositions": SHARED_POSITIONS.to_vec(),
            "threshold": deal.threshold,
            // One value over the entire published transcript. Anything that
            // differs between the two implementations differs here.
            "digest": hex(&deal.digest()),
            "publicKeys": deal.recipients.iter()
                .map(|recipient| hex(&recipient.public_key)).collect::<Vec<_>>(),
            // Spot values, so a mismatch says roughly where rather than only
            // that the digests differ.
            // What each player commits on chain. The rounds themselves never
            // leave the players, so these are the only trace of them there.
            "shuffleRoundDigests": deal.shuffles.iter()
                .map(|round| hex(&shuffle_round_digest(round))).collect::<Vec<_>>(),
            "lockRoundDigests": deal.locks.iter()
                .map(|round| hex(&lock_round_digest(round))).collect::<Vec<_>>(),
            "shuffleMasks": deal.shuffles.iter()
                .map(|round| hex(&round.mask)).collect::<Vec<_>>(),
            "shuffledFirst": deal.shuffles.iter()
                .map(|round| hex(&round.deck[0])).collect::<Vec<_>>(),
            "unmaskChallenges": deal.locks.iter()
                .map(|round| hex(&round.unmask_proof.challenge)).collect::<Vec<_>>(),
            "lockChallenges": deal.locks.iter()
                .map(|round| hex(&round.lock_proof.challenge)).collect::<Vec<_>>(),
            // Named separately from the digests so a disagreement inside the
            // shuffle argument says so, rather than surfacing as an opaque
            // round digest that moved for reasons nobody can see.
            "shuffleProofChallenges": deal.shuffles.iter()
                .map(|round| hex(&round.proof.challenge)).collect::<Vec<_>>(),
            "shuffleProofFirsts": deal.shuffles.iter().map(|round| json!({
                "permutation": hex(&round.proof.permutation[0]),
                "bridge": hex(&round.proof.bridges[0]),
                "response": hex(&round.proof.responses[0]),
                "bridgeResponse": hex(&round.proof.bridge_responses[0]),
                "blindResponse": hex(&round.proof.blind_response),
                "onesResponse": hex(&round.proof.ones_response),
                "maskResponse": hex(&round.proof.mask_response),
            })).collect::<Vec<_>>(),
            "lockedLast": deal.locks.iter()
                .map(|round| hex(&round.deck[CARD_COUNT - 1])).collect::<Vec<_>>(),
            "shareCiphertext": {
                "dealer": 0, "position": SHARED_POSITIONS[0], "holder": 1,
                "value": hex(&deal.share_package(0, SHARED_POSITIONS[0])
                    .expect("a package").shares[1].ciphertext),
            },
            "deck": deck,
        },
        "opens": { "hole": hole_opens, "board": board_opens },
        // The batched primitive both halves verify with. Two libraries agreeing
        // on a sum of one term says nothing; this is fifty-two.
        "multiscalar": {
            "label": "msm",
            "value": hex(&encode_point(&multiscalar(
                &ordered_deck(),
                &(0..CARD_COUNT)
                    .map(|index| Seed::new([9u8; 32]).scalar("msm", index as u32))
                    .collect::<Vec<Scalar>>(),
            ))),
        },
        "adversarial": adversarial(&session),
        "dealPositions": [three_handed, six_handed, sparse],
    })
}

#[test]
fn the_fixture_matches_what_the_protocol_produces() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
    let fixture = build_fixture();
    let rendered = format!("{}\n", serde_json::to_string_pretty(&fixture).expect("json"));

    if std::env::var_os("UPDATE_MENTAL_VECTORS").is_some() {
        std::fs::write(&path, &rendered).expect("the fixture is writable");
        return;
    }

    let committed = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{}: {error}. Generate it with \
             UPDATE_MENTAL_VECTORS=1 cargo test -p riverproof-mental --test vectors",
            path.display(),
        )
    });
    let committed: Value = serde_json::from_str(&committed).expect("the fixture parses");

    // Compared field by field, so a failure names the piece that moved.
    for key in [
        "cardPoints", "prf", "transcript", "deal", "opens", "dealPositions", "multiscalar",
        "adversarial",
    ] {
        assert_eq!(
            committed.get(key),
            fixture.get(key),
            "the committed fixture's `{key}` is not what this protocol produces. \
             If the protocol changed on purpose, regenerate it; if it did not, \
             something drifted",
        );
    }
}

/// The fixture is only worth anything if the deal it records is a valid one.
#[test]
fn the_fixture_deal_verifies_and_opens() {
    let session = built();
    assert!(session.deal.verify().is_ok());

    let mut deck = session.deck().expect("the deck opens");
    deck.sort_unstable();
    assert_eq!(deck, (0..CARD_COUNT as u8).collect::<Vec<_>>());
    assert_eq!(session.deal.threshold, 2, "three players, all but one");
}
