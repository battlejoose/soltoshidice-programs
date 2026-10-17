//! The eighty recorded hands, dealt by the players instead of by a cluster.
//!
//! `program/engine/tests/differential.rs` pins `DeckSource::Local` to the
//! JavaScript oracle action for action. This file is the other half: every
//! trial is played twice against the same deck and the same actions, once
//! locally and once with the deck shuffled, locked, and opened by the players
//! themselves — and the two must agree on the board, every stack, and the
//! result. Since Arcium was removed this is the only confidential comparison
//! there is, and the only one there needs to be.
//!
//! Two things make it a real comparison rather than a plausible one.
//!
//! The mental table is started with a deck of zeros. The engine holds no card
//! material at all under this source, so a hand that came out right came out
//! right through the seam and not through a deck that happened to be lying
//! around.
//!
//! And the joint shuffle is steered onto the trial's recorded ordering: every
//! player but the last shuffles from their own seed, and the last composes onto
//! the target. So the protocol really runs — masks, locks, proofs, peels,
//! thresholds — and still lands on a deck the oracle has an answer for.
//!
//! Heads-up trials are asserted to be *refused*. That is the design, not a gap:
//! two players have no board threshold that is both private and live.

use riverproof_engine::cards::{parse_card, DECK_SIZE};
use riverproof_engine::pots::seats_of;
use riverproof_engine::table::{
    Action, Awaiting, DeckSource, EngineError, Seat, Stage, Table, TableConfig, MAX_SEATS,
};

use riverproof_mental::group::{Seed, CARD_COUNT};
use riverproof_mental::protocol::{deal_hand, steer_last_order, Session};

use serde::Deserialize;

const VECTORS: &str = include_str!("../../engine/tests/vectors/engine-vectors.json");

#[derive(Deserialize)]
struct Vectors {
    hands: Vec<HandTrial>,
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
struct Step {
    seat: u8,
    action: ActionVector,
}

#[derive(Deserialize)]
struct ActionVector {
    #[serde(rename = "type")]
    kind: String,
    amount: Option<u64>,
}

#[derive(Deserialize)]
struct ResultVector {
    board: Vec<String>,
    winners: Vec<WinnerVector>,
    #[serde(rename = "finalStacks")]
    final_stacks: Vec<Option<u64>>,
}

#[derive(Deserialize)]
struct WinnerVector {
    seat: u8,
    amount: u64,
}

fn cards(list: &[String]) -> Vec<u8> {
    list.iter()
        .map(|text| parse_card(text).unwrap_or_else(|| panic!("bad card {text}")))
        .collect()
}

fn action_of(step: &Step) -> Action {
    match step.action.kind.as_str() {
        "fold" => Action::Fold,
        "check" => Action::Check,
        "call" => Action::Call,
        "allin" => Action::AllIn,
        "raise" => Action::Raise { to: step.action.amount.expect("a raise carries an amount") },
        other => panic!("unknown action {other}"),
    }
}

fn seed_for(trial: u32, player: usize) -> Seed {
    let mut bytes = [0u8; 32];
    bytes[..4].copy_from_slice(&trial.to_le_bytes());
    bytes[4] = player as u8;
    bytes[5] = 0x5e;
    Seed::new(bytes)
}

fn table_for(trial: &HandTrial, source: DeckSource) -> Table {
    let config = TableConfig {
        small_blind: trial.config.small_blind,
        big_blind: trial.config.big_blind,
        ante: trial.config.ante,
        max_players: trial.config.max_players,
        buy_in: trial.config.buy_in,
        action_time_ms: trial.config.action_time_ms,
    };
    let mut table = Table::new(config);
    table.deck_source = source;
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
    table
}

/// Deals the trial's deck through the protocol, with every player but the last
/// shuffling from their own seed.
fn deal_trial(trial: &HandTrial, players: usize, board_positions: &[u8]) -> Session {
    let target = cards(&trial.deck);
    let seeds: Vec<Seed> = (0..players).map(|player| seed_for(trial.id, player)).collect();

    let mut orders: Vec<Vec<u8>> = seeds[..players - 1]
        .iter()
        .map(|seed| seed.permutation("shuffle-order", CARD_COUNT))
        .collect();
    orders.push(steer_last_order(&orders, &target));

    let session = deal_hand(&seeds, board_positions, Some(&orders))
        .unwrap_or_else(|error| panic!("trial {}: the deal failed: {error:?}", trial.id));
    assert_eq!(
        session.deck().expect("the deck opens"),
        target,
        "trial {}: the steered shuffle did not land on the recorded deck",
        trial.id,
    );
    session
}

/// The threshold that opens a board card, rotated so different subsets of
/// players do the work across the suite rather than always the first few.
fn openers(players: usize, threshold: usize, offset: usize) -> Vec<u8> {
    (0..threshold)
        .map(|index| ((offset + index) % players) as u8)
        .collect()
}

#[test]
fn the_mental_path_reproduces_the_synchronous_path() {
    let vectors: Vectors = serde_json::from_str(VECTORS).expect("engine-vectors.json parses");
    assert_eq!(vectors.hands.len(), 80, "expected 80 recorded trials");

    let mut dealt = 0usize;
    let mut refused = 0usize;
    let mut board_reveals = 0usize;
    let mut hole_reveals = 0usize;

    for trial in &vectors.hands {
        let id = trial.id;
        let seated = trial.starting_stacks.iter().filter(|stack| stack.is_some()).count();

        // Two players is refused at the door, by the engine, for the reason the
        // design gives: there is no threshold between peeking and stalling.
        if seated < 3 {
            let mut table = table_for(trial, DeckSource::Mental);
            let mut deck = [0u8; DECK_SIZE];
            deck.copy_from_slice(&cards(&trial.deck));
            assert_eq!(
                table.start_hand(deck, 100),
                Err(EngineError::NotEnoughPlayersForMentalDeal),
                "trial {id}: a heads-up mental hand was dealt",
            );
            refused += 1;
            continue;
        }
        dealt += 1;

        let mut local = table_for(trial, DeckSource::Local);
        let mut mental = table_for(trial, DeckSource::Mental);

        let mut deck = [0u8; DECK_SIZE];
        deck.copy_from_slice(&cards(&trial.deck));
        local.start_hand(deck, 100).expect("the local hand starts");
        // Zeros: the engine is handed no card material at all on this path.
        mental.start_hand([0u8; DECK_SIZE], 100).expect("the mental hand starts");

        let positions = mental.deal_positions().expect("a hand is in progress");
        let players: Vec<u8> = seats_of(mental.hand.as_ref().unwrap().active_seats).collect();
        assert_eq!(players.len(), seated, "trial {id}: seated players");

        let session = deal_trial(trial, players.len(), &positions.board);
        let threshold = session.deal.threshold as usize;

        // Every player reads their own hole cards, and only their own. The
        // residual after everybody else has peeled is not a card, which is the
        // whole of "encrypted to their owner alone".
        for (player, seat) in players.iter().enumerate() {
            let expected = local.hand.as_ref().unwrap().hole_cards[*seat as usize];
            for (round, position) in positions.hole[*seat as usize].iter().enumerate() {
                assert_eq!(
                    session.open_hole(player as u8, *position).expect("the owner opens it"),
                    expected[round],
                    "trial {id}: seat {seat} card {round}",
                );
                assert!(
                    riverproof_mental::card_of_point(
                        &session.residual(player as u8, *position).expect("a residual")
                    )
                    .is_err(),
                    "trial {id}: seat {seat} card {round} opened without its owner",
                );
            }
        }

        // Every hole card is now readable by its owner, which is what "dealt"
        // means for this scheme — so the betting opens, exactly as
        // `publish_unlocks` opens it on chain once the last share lands. Before
        // this the hand is parked and refuses actions, which is the point: a
        // table used to be able to bet a whole preflop round holding nothing.
        mental.open_betting(100).expect("the deal is complete, so betting opens");

        let mut now = 100i64;
        let mut street = 0usize;
        for step in &trial.steps {
            let action = action_of(step);
            now += 10;
            local.apply_action(step.seat, action, now, None).expect("the local action");
            mental.apply_action(step.seat, action, now, None).expect("the mental action");

            // Parked for a board reveal: a threshold of the players opens the
            // positions the engine is about to read, and hands them back.
            while let Some((awaiting, count)) = mental.pending_reveal() {
                if awaiting == Awaiting::Showdown {
                    break;
                }
                let dealt_so_far = mental.hand.as_ref().unwrap().board_len as usize;
                let opening = openers(players.len(), threshold, id as usize + street);
                let revealed: Vec<u8> = positions.board[dealt_so_far..dealt_so_far + count]
                    .iter()
                    .map(|position| {
                        board_reveals += 1;
                        session
                            .open_board(*position, &opening)
                            .expect("a threshold opens the board")
                    })
                    .collect();
                street += 1;
                mental.supply_street(&revealed, now).expect("supply_street");
            }

            // Parked at showdown: each contender opens their own cards. A
            // folded seat is never asked, and could refuse if it were.
            let mut pending = mental.pending_hole_reveals();
            while pending != 0 {
                for seat in seats_of(pending) {
                    let player = players.iter().position(|s| *s == seat).expect("a seated player");
                    let slots = positions.hole[seat as usize];
                    let cards = [
                        session.open_hole(player as u8, slots[0]).expect("the owner opens it"),
                        session.open_hole(player as u8, slots[1]).expect("the owner opens it"),
                    ];
                    hole_reveals += 1;
                    mental.supply_hole_cards(seat, cards, now).expect("supply_hole_cards");
                }
                pending = mental.pending_hole_reveals();
            }
        }

        let local_hand = local.hand.as_ref().expect("a local hand");
        let mental_hand = mental.hand.as_ref().expect("a mental hand");
        assert_eq!(mental_hand.stage, Stage::Complete, "trial {id}: the hand did not complete");
        assert_eq!(mental_hand.stage, local_hand.stage, "trial {id}: stage");
        assert_eq!(mental_hand.board(), local_hand.board(), "trial {id}: board");
        for seat in 0..MAX_SEATS {
            assert_eq!(
                mental.seats[seat].stack, local.seats[seat].stack,
                "trial {id}: seat {seat} stack differs between mental and local",
            );
        }
        assert_eq!(
            mental.chip_total(),
            trial.starting_chips,
            "trial {id}: the mental path changed the chip supply",
        );

        let local_result = local_hand.result.as_ref().expect("a local result");
        let mental_result = mental_hand.result.as_ref().expect("a mental result");
        assert_eq!(mental_result.kind, local_result.kind, "trial {id}: result kind");
        assert_eq!(
            mental_result.winners(),
            local_result.winners(),
            "trial {id}: winners differ between mental and local",
        );

        // And against the oracle's own recording, so this is pinned to the
        // JavaScript engine rather than merely to the Rust one.
        assert_eq!(
            mental_hand.board(),
            cards(&trial.result.board).as_slice(),
            "trial {id}: final board differs from the oracle",
        );
        assert_eq!(
            mental_result.winners().len(),
            trial.result.winners.len(),
            "trial {id}: winner count differs from the oracle",
        );
        for (actual, expected) in mental_result.winners().iter().zip(&trial.result.winners) {
            assert_eq!(actual.seat, expected.seat, "trial {id}: winner seat");
            assert_eq!(actual.amount, expected.amount, "trial {id}: winner amount");
        }
        for (index, stack) in trial.result.final_stacks.iter().enumerate() {
            if let Some(stack) = stack {
                assert_eq!(
                    mental.seats[index].stack, *stack,
                    "trial {id}: seat {index} final stack differs from the oracle",
                );
            }
        }
    }

    // The suite covers both halves of the design, so neither can quietly become
    // dead code.
    assert!(dealt >= 40, "expected plenty of three-or-more-player trials, saw {dealt}");
    assert!(refused > 0, "expected some heads-up trials to be refused, saw none");
    assert!(board_reveals > 50, "expected the board to be opened often, saw {board_reveals}");
    assert!(hole_reveals > 10, "expected showdowns to open hole cards, saw {hole_reveals}");
}
