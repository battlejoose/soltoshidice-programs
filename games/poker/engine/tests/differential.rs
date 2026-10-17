//! Differential tests against the JavaScript oracle.
//!
//! `server/poker-engine.mjs` is the reference implementation. The vectors here
//! are dumped from it by `scripts/dump-engine-vectors.mjs` and record every
//! intermediate state of 80 randomized hands across 2-10 players, so a
//! divergence is caught at the exact action that caused it rather than at the
//! final chip count.
//!
//! A failure means this port is wrong. Do not "fix" it by regenerating vectors.

use riverproof_engine::cards::{parse_card, shuffle_deck, DECK_SIZE};
use riverproof_engine::eval::evaluate;
use riverproof_engine::pots::{build_side_pots, seats_of};
use riverproof_engine::table::{
    Action, Seat, Stage, Table, TableConfig, MAX_SEATS,
};

use serde::Deserialize;
use std::collections::BTreeMap;

const VECTORS: &str = include_str!("vectors/engine-vectors.json");

#[derive(Deserialize)]
struct Vectors {
    evaluator: Vec<EvaluatorCase>,
    #[serde(rename = "sidePots")]
    side_pots: Vec<SidePotCase>,
    decks: Vec<DeckCase>,
    hands: Vec<HandTrial>,
}

#[derive(Deserialize)]
struct EvaluatorCase {
    cards: Vec<String>,
    score: Vec<u8>,
    category: String,
}

#[derive(Deserialize)]
struct SidePotCase {
    contributions: BTreeMap<String, u64>,
    folded: Vec<u8>,
    pots: Vec<PotExpectation>,
}

#[derive(Deserialize)]
struct PotExpectation {
    amount: u64,
    contributors: Vec<u8>,
    eligible: Vec<u8>,
    refund: bool,
}

#[derive(Deserialize)]
struct DeckCase {
    seed: u64,
    deck: Vec<String>,
}

#[derive(Deserialize)]
struct HandTrial {
    id: u32,
    config: ConfigVector,
    #[serde(rename = "startingStacks")]
    starting_stacks: Vec<Option<u64>>,
    #[serde(rename = "startingChips")]
    starting_chips: u64,
    deck: Vec<String>,
    #[serde(rename = "dealerSeat")]
    dealer_seat: u8,
    #[serde(rename = "smallBlindSeat")]
    small_blind_seat: u8,
    #[serde(rename = "bigBlindSeat")]
    big_blind_seat: u8,
    #[serde(rename = "activeSeats")]
    active_seats: Vec<u8>,
    #[serde(rename = "holeCards")]
    hole_cards: Vec<Option<Vec<String>>>,
    #[serde(rename = "afterStart")]
    after_start: Snapshot,
    steps: Vec<Step>,
    result: ResultVector,
}

#[derive(Deserialize)]
struct ConfigVector {
    #[serde(rename = "smallBlind")]
    small_blind: u64,
    #[serde(rename = "bigBlind")]
    big_blind: u64,
    ante: u64,
    #[serde(rename = "maxPlayers")]
    max_players: u8,
    #[serde(rename = "buyIn")]
    buy_in: u64,
    #[serde(rename = "actionTimeMs")]
    action_time_ms: i64,
}

#[derive(Deserialize)]
struct Snapshot {
    stage: String,
    board: Vec<String>,
    #[serde(rename = "deckIndex")]
    deck_index: u8,
    pot: u64,
    #[serde(rename = "currentBet")]
    current_bet: u64,
    #[serde(rename = "minRaise")]
    min_raise: u64,
    #[serde(rename = "actorSeat")]
    actor_seat: Option<u8>,
    folded: Vec<u8>,
    #[serde(rename = "allIn")]
    all_in: Vec<u8>,
    acted: Vec<u8>,
    #[serde(rename = "streetBets")]
    street_bets: Vec<Option<u64>>,
    contributions: Vec<Option<u64>>,
    stacks: Vec<Option<u64>>,
}

#[derive(Deserialize)]
struct Step {
    seat: u8,
    legal: Option<LegalVector>,
    action: ActionVector,
    after: Snapshot,
}

#[derive(Deserialize)]
struct LegalVector {
    #[serde(rename = "toCall")]
    to_call: u64,
    #[serde(rename = "canCheck")]
    can_check: bool,
    #[serde(rename = "canCall")]
    can_call: bool,
    #[serde(rename = "canRaise")]
    can_raise: bool,
    #[serde(rename = "minimumRaiseTo")]
    minimum_raise_to: Option<u64>,
    #[serde(rename = "maximumRaiseTo")]
    maximum_raise_to: Option<u64>,
    #[serde(rename = "shortAllInOnly")]
    short_all_in_only: bool,
    #[serde(rename = "canAllIn")]
    can_all_in: bool,
}

#[derive(Deserialize)]
struct ActionVector {
    #[serde(rename = "type")]
    kind: String,
    amount: Option<u64>,
}

#[derive(Deserialize)]
struct ResultVector {
    #[serde(rename = "type")]
    kind: String,
    board: Vec<String>,
    winners: Vec<WinnerVector>,
    #[serde(rename = "finalStacks")]
    final_stacks: Vec<Option<u64>>,
    #[serde(rename = "finalChips")]
    final_chips: u64,
}

#[derive(Deserialize)]
struct WinnerVector {
    seat: u8,
    amount: u64,
    category: Option<String>,
}

fn vectors() -> Vectors {
    serde_json::from_str(VECTORS).expect("engine-vectors.json parses")
}

fn cards(list: &[String]) -> Vec<u8> {
    list.iter()
        .map(|text| parse_card(text).unwrap_or_else(|| panic!("bad card {text}")))
        .collect()
}

fn mask(seats: &[u8]) -> u16 {
    seats.iter().fold(0u16, |acc, seat| acc | (1 << seat))
}

fn stage_of(text: &str) -> Stage {
    match text {
        "preflop" => Stage::Preflop,
        "flop" => Stage::Flop,
        "turn" => Stage::Turn,
        "river" => Stage::River,
        "complete" => Stage::Complete,
        other => panic!("unknown stage {other}"),
    }
}

fn filled(values: &[Option<u64>]) -> [u64; MAX_SEATS] {
    let mut result = [0u64; MAX_SEATS];
    for (index, value) in values.iter().enumerate() {
        result[index] = value.unwrap_or(0);
    }
    result
}

#[test]
fn evaluator_matches_the_javascript_oracle() {
    let vectors = vectors();
    assert!(vectors.evaluator.len() >= 700, "vector file looks truncated");

    for (index, case) in vectors.evaluator.iter().enumerate() {
        let hand = cards(&case.cards);
        let score = evaluate(&hand)
            .unwrap_or_else(|| panic!("case {index}: {:?} did not evaluate", case.cards));

        // The oracle's variable-length score array zero-pads to six elements.
        let mut expected = [0u8; 6];
        for (slot, value) in expected.iter_mut().zip(case.score.iter()) {
            *slot = *value;
        }

        assert_eq!(
            score.0, expected,
            "case {index}: {:?} scored {:?}, oracle said {:?}",
            case.cards, score.0, case.score
        );
        assert_eq!(
            score.category_name(),
            case.category,
            "case {index}: {:?} categorized wrongly",
            case.cards
        );
    }
}

#[test]
fn side_pots_match_the_javascript_oracle() {
    let vectors = vectors();

    for (index, case) in vectors.side_pots.iter().enumerate() {
        let mut contributions = [0u64; MAX_SEATS];
        for (seat, amount) in &case.contributions {
            contributions[seat.parse::<usize>().expect("numeric seat key")] = *amount;
        }

        let pots = build_side_pots(&contributions, mask(&case.folded));
        assert_eq!(
            pots.len(),
            case.pots.len(),
            "case {index}: pot count differs for {:?}",
            case.contributions
        );

        for (layer, (actual, expected)) in pots.as_slice().iter().zip(case.pots.iter()).enumerate() {
            assert_eq!(actual.amount, expected.amount, "case {index} layer {layer}: amount");
            assert_eq!(
                seats_of(actual.contributors).collect::<Vec<_>>(),
                expected.contributors,
                "case {index} layer {layer}: contributors"
            );
            assert_eq!(
                seats_of(actual.eligible).collect::<Vec<_>>(),
                expected.eligible,
                "case {index} layer {layer}: eligible"
            );
            assert_eq!(actual.refund, expected.refund, "case {index} layer {layer}: refund");
        }
    }
}

#[test]
fn shuffle_matches_the_javascript_oracle() {
    let vectors = vectors();

    for case in &vectors.decks {
        // The same linear congruential generator the oracle's dumper uses.
        let mut state = case.seed as u32;
        let deck = shuffle_deck(|maximum| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state % maximum
        });

        assert_eq!(
            deck.to_vec(),
            cards(&case.deck),
            "seed {} produced a different ordering",
            case.seed
        );
    }
}

fn assert_snapshot(table: &Table, expected: &Snapshot, context: &str) {
    let hand = table.hand.as_ref().unwrap_or_else(|| panic!("{context}: no hand"));

    assert_eq!(hand.stage, stage_of(&expected.stage), "{context}: stage");
    assert_eq!(hand.board(), cards(&expected.board).as_slice(), "{context}: board");
    assert_eq!(hand.deck_index, expected.deck_index, "{context}: deck index");
    assert_eq!(hand.pot(), expected.pot, "{context}: pot");
    assert_eq!(hand.current_bet, expected.current_bet, "{context}: current bet");
    assert_eq!(hand.min_raise, expected.min_raise, "{context}: min raise");
    assert_eq!(hand.actor_seat, expected.actor_seat, "{context}: actor");
    assert_eq!(
        seats_of(hand.folded).collect::<Vec<_>>(),
        expected.folded,
        "{context}: folded"
    );
    assert_eq!(
        seats_of(hand.all_in).collect::<Vec<_>>(),
        expected.all_in,
        "{context}: all-in"
    );
    assert_eq!(
        seats_of(hand.acted).collect::<Vec<_>>(),
        expected.acted,
        "{context}: acted"
    );
    assert_eq!(hand.street_bets, filled(&expected.street_bets), "{context}: street bets");
    assert_eq!(
        hand.contributions,
        filled(&expected.contributions),
        "{context}: contributions"
    );

    for (index, stack) in expected.stacks.iter().enumerate() {
        if let Some(stack) = stack {
            assert_eq!(
                table.seats[index].stack, *stack,
                "{context}: seat {index} stack"
            );
        }
    }
}

#[test]
fn whole_hands_replay_exactly() {
    let vectors = vectors();
    assert_eq!(vectors.hands.len(), 80, "expected 80 recorded trials");

    for trial in &vectors.hands {
        let id = trial.id;
        let config = TableConfig {
            small_blind: trial.config.small_blind,
            big_blind: trial.config.big_blind,
            ante: trial.config.ante,
            max_players: trial.config.max_players,
            buy_in: trial.config.buy_in,
            action_time_ms: trial.config.action_time_ms,
        };

        let mut table = Table::new(config);
        for (index, stack) in trial.starting_stacks.iter().enumerate() {
            if let Some(stack) = stack {
                table.seats[index] = Seat {
                    occupied: true,
                    stack: *stack,
                    ready: true,
                    connected: true,
                    leave_after_hand: false,
                };
            }
        }
        assert_eq!(table.chip_total(), trial.starting_chips, "trial {id}: starting chips");

        let mut deck = [0u8; DECK_SIZE];
        deck.copy_from_slice(&cards(&trial.deck));
        table
            .start_hand(deck, 100)
            .unwrap_or_else(|error| panic!("trial {id}: start_hand failed: {error:?}"));

        {
            let hand = table.hand.as_ref().expect("hand started");
            assert_eq!(hand.dealer_seat, trial.dealer_seat, "trial {id}: dealer");
            assert_eq!(hand.small_blind_seat, trial.small_blind_seat, "trial {id}: small blind");
            assert_eq!(hand.big_blind_seat, trial.big_blind_seat, "trial {id}: big blind");
            assert_eq!(
                seats_of(hand.active_seats).collect::<Vec<_>>(),
                trial.active_seats,
                "trial {id}: active seats"
            );

            for (index, expected) in trial.hole_cards.iter().enumerate() {
                if let Some(expected) = expected {
                    assert_eq!(
                        hand.hole_cards[index].to_vec(),
                        cards(expected),
                        "trial {id}: seat {index} hole cards"
                    );
                }
            }
        }
        assert_snapshot(&table, &trial.after_start, &format!("trial {id} after start"));

        let mut now = 100i64;
        for (number, step) in trial.steps.iter().enumerate() {
            let context = format!("trial {id} step {number} (seat {})", step.seat);

            if let Some(expected) = &step.legal {
                let legal = table
                    .legal_actions(step.seat)
                    .unwrap_or_else(|| panic!("{context}: no legal actions"));
                assert_eq!(legal.to_call, expected.to_call, "{context}: to call");
                assert_eq!(legal.can_check, expected.can_check, "{context}: can check");
                assert_eq!(legal.can_call, expected.can_call, "{context}: can call");
                assert_eq!(legal.can_raise, expected.can_raise, "{context}: can raise");
                assert_eq!(
                    legal.minimum_raise_to, expected.minimum_raise_to,
                    "{context}: minimum raise"
                );
                assert_eq!(
                    legal.maximum_raise_to, expected.maximum_raise_to,
                    "{context}: maximum raise"
                );
                assert_eq!(
                    legal.short_all_in_only, expected.short_all_in_only,
                    "{context}: short all-in"
                );
                assert_eq!(legal.can_all_in, expected.can_all_in, "{context}: can all-in");
            }

            let action = match step.action.kind.as_str() {
                "fold" => Action::Fold,
                "check" => Action::Check,
                "call" => Action::Call,
                "allin" => Action::AllIn,
                "raise" => Action::Raise {
                    to: step.action.amount.expect("raise carries an amount"),
                },
                other => panic!("{context}: unknown action {other}"),
            };

            now += 10;
            table
                .apply_action(step.seat, action, now, None)
                .unwrap_or_else(|error| panic!("{context}: rejected: {error:?}"));

            assert_snapshot(&table, &step.after, &context);
        }

        let hand = table.hand.as_ref().expect("hand present");
        assert_eq!(hand.stage, Stage::Complete, "trial {id}: hand did not complete");
        assert_eq!(hand.board(), cards(&trial.result.board).as_slice(), "trial {id}: final board");

        let result = hand.result.as_ref().expect("result recorded");
        let kind = match trial.result.kind.as_str() {
            "fold" => riverproof_engine::ResultKind::Fold,
            "showdown" => riverproof_engine::ResultKind::Showdown,
            other => panic!("trial {id}: unknown result {other}"),
        };
        assert_eq!(result.kind, kind, "trial {id}: result kind");

        let winners = result.winners();
        assert_eq!(
            winners.len(),
            trial.result.winners.len(),
            "trial {id}: winner count"
        );
        for (index, (actual, expected)) in winners.iter().zip(trial.result.winners.iter()).enumerate()
        {
            assert_eq!(actual.seat, expected.seat, "trial {id}: winner {index} seat");
            assert_eq!(actual.amount, expected.amount, "trial {id}: winner {index} amount");
            let category = actual
                .category
                .map(|index| riverproof_engine::CATEGORY_NAMES[index as usize].to_string());
            assert_eq!(
                category, expected.category,
                "trial {id}: winner {index} category"
            );
        }

        for (index, stack) in trial.result.final_stacks.iter().enumerate() {
            if let Some(stack) = stack {
                assert_eq!(
                    table.seats[index].stack, *stack,
                    "trial {id}: seat {index} final stack"
                );
            }
        }

        // The invariant the JavaScript engine guards with chipTotal().
        assert_eq!(
            table.chip_total(),
            trial.result.final_chips,
            "trial {id}: chip supply changed"
        );
        assert_eq!(
            table.chip_total(),
            trial.starting_chips,
            "trial {id}: chips were created or destroyed"
        );
    }
}

// The confidential path is pinned in `program/mental/tests/differential.rs`.
//
// There used to be a second comparison here, replaying every trial through
// `DeckSource::Mpc` — the Arcium cluster — against the local run. Arcium is
// gone, and the mental crate's differential suite is the same comparison for
// the source that ships: the same eighty hands, once locally and once with the
// deck shuffled, locked and opened by the players, asserting the same board,
// the same stacks and the same result. It lives there rather than here because
// it needs the protocol, and this crate has no dependency on it.
