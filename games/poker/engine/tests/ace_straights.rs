//! Explicit rule fixtures, independent of the JS-generated differential oracle.
use riverproof_engine::{cards::parse_card, eval::evaluate};
use serde::Deserialize;

#[derive(Deserialize)]
struct Case {
    name: String,
    cards: String,
    score: [u8; 6],
}

#[test]
fn aces_high_low_best_five_and_no_wraparound() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("vectors/ace-straights.json")).unwrap();
    for case in cases {
        let mut cards: Vec<u8> = case.cards.split_whitespace().map(|c| parse_card(c).unwrap()).collect();
        // Card order cannot change a hand, including an ace at either end.
        for _ in 0..cards.len() {
            for reversed in [false, true] {
                let mut input = cards.clone();
                if reversed { input.reverse(); }
                assert_eq!(evaluate(&input).unwrap().0, case.score, "{}: {:?}", case.name, input);
            }
            cards.rotate_left(1);
        }
    }
}

#[test]
fn every_five_distinct_rank_combination_has_only_the_ten_legal_straights() {
    // All 1,287 five-rank sets, in both mixed suits and a single suit.
    // Legal rank masks are enumerated from the rules, not from an evaluator.
    let legal = [(0x100f, 5), (0x001f, 6), (0x003e, 7), (0x007c, 8),
        (0x00f8, 9), (0x01f0, 10), (0x03e0, 11), (0x07c0, 12),
        (0x0f80, 13), (0x1f00, 14)];
    let mut checked = 0;
    for mask in 0u16..(1 << 13) {
        if mask.count_ones() != 5 { continue; }
        let ranks: Vec<u8> = (0..13).filter(|r| mask & (1 << r) != 0).collect();
        let high = legal.iter().find(|(m, _)| *m == mask).map(|(_, high)| *high);
        for flush in [false, true] {
            let cards: Vec<u8> = ranks.iter().enumerate()
                .map(|(i, rank)| rank + if flush { 0 } else { (i % 4) as u8 * 13 }).collect();
            let score = evaluate(&cards).unwrap();
            assert_eq!(score.category(), if high.is_some() { if flush { 8 } else { 4 } }
                else if flush { 5 } else { 0 }, "rank mask {mask:#x}");
            if let Some(high) = high { assert_eq!(score.0[1], high); }
            checked += 1;
        }
    }
    assert_eq!(checked, 2574);
}
