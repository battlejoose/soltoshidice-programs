//! Seven-card hand evaluation.
//!
//! A direct port of `rankFive` / `evaluateSeven` / `compareRanks` from
//! `server/poker-engine.mjs`. Evaluation runs in the clear: showdown cards are
//! public anyway, so only shuffling and dealing need confidentiality.
//!
//! The JavaScript scores are variable-length arrays compared element-wise with
//! missing entries treated as zero. The longest is six elements (flush and high
//! card carry all five ranks), so a zero-padded `[u8; 6]` compares identically.

use crate::cards::{rank_value, suit};

pub const SCORE_LEN: usize = 6;

pub const CATEGORY_NAMES: [&str; 9] = [
    "High card",
    "One pair",
    "Two pair",
    "Three of a kind",
    "Straight",
    "Flush",
    "Full house",
    "Four of a kind",
    "Straight flush",
];

/// Zero-padded hand score. Element 0 is the category, the rest are tiebreakers
/// in descending significance.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Score(pub [u8; SCORE_LEN]);

impl Score {
    #[inline]
    pub fn category(&self) -> u8 {
        self.0[0]
    }

    pub fn category_name(&self) -> &'static str {
        CATEGORY_NAMES[self.0[0] as usize]
    }
}

impl PartialOrd for Score {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Score {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

/// Matches `compareRanks`: negative, zero, or positive.
pub fn compare_scores(left: &Score, right: &Score) -> core::cmp::Ordering {
    left.cmp(right)
}

fn score_from(parts: &[u8]) -> Score {
    let mut score = [0u8; SCORE_LEN];
    let length = parts.len().min(SCORE_LEN);
    score[..length].copy_from_slice(&parts[..length]);
    Score(score)
}

/// Ranks exactly five cards.
fn rank_five(cards: [u8; 5]) -> Score {
    let mut ranks = [0u8; 5];
    for (slot, card) in ranks.iter_mut().zip(cards.iter()) {
        *slot = rank_value(*card);
    }
    ranks.sort_unstable_by(|a, b| b.cmp(a));

    // counts[rank] for rank 2..=14
    let mut counts = [0u8; 15];
    for rank in ranks {
        counts[rank as usize] += 1;
    }

    // Groups sorted by count descending, then rank descending, matching
    // `[...counts.entries()].sort(([rA, cA], [rB, cB]) => cB - cA || rB - rA)`.
    let mut groups: [(u8, u8); 5] = [(0, 0); 5];
    let mut group_count = 0usize;
    for rank in (2u8..=14).rev() {
        if counts[rank as usize] > 0 {
            groups[group_count] = (rank, counts[rank as usize]);
            group_count += 1;
        }
    }
    let groups = &mut groups[..group_count];
    groups.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));

    // Distinct ranks descending, with the wheel's low ace appended last.
    let mut distinct = [0u8; 6];
    let mut distinct_count = 0usize;
    for rank in (2u8..=14).rev() {
        if counts[rank as usize] > 0 {
            distinct[distinct_count] = rank;
            distinct_count += 1;
        }
    }
    if distinct_count > 0 && distinct[0] == 14 {
        distinct[distinct_count] = 1;
        distinct_count += 1;
    }

    let mut straight_high = 0u8;
    if distinct_count >= 5 {
        for index in 0..=(distinct_count - 5) {
            if distinct[index] - distinct[index + 4] == 4 {
                straight_high = distinct[index];
                break;
            }
        }
    }

    let first_suit = suit(cards[0]);
    let flush = cards.iter().all(|card| suit(*card) == first_suit);

    if flush && straight_high > 0 {
        return score_from(&[8, straight_high]);
    }
    if groups[0].1 == 4 {
        return score_from(&[7, groups[0].0, groups[1].0]);
    }
    if groups[0].1 == 3 && group_count > 1 && groups[1].1 == 2 {
        return score_from(&[6, groups[0].0, groups[1].0]);
    }
    if flush {
        return score_from(&[5, ranks[0], ranks[1], ranks[2], ranks[3], ranks[4]]);
    }
    if straight_high > 0 {
        return score_from(&[4, straight_high]);
    }
    if groups[0].1 == 3 {
        let mut kickers = [groups[1].0, groups[2].0];
        kickers.sort_unstable_by(|a, b| b.cmp(a));
        return score_from(&[3, groups[0].0, kickers[0], kickers[1]]);
    }
    if groups[0].1 == 2 && group_count > 1 && groups[1].1 == 2 {
        let mut pairs = [groups[0].0, groups[1].0];
        pairs.sort_unstable_by(|a, b| b.cmp(a));
        return score_from(&[2, pairs[0], pairs[1], groups[2].0]);
    }
    if groups[0].1 == 2 {
        let mut kickers = [groups[1].0, groups[2].0, groups[3].0];
        kickers.sort_unstable_by(|a, b| b.cmp(a));
        return score_from(&[1, groups[0].0, kickers[0], kickers[1], kickers[2]]);
    }
    score_from(&[0, ranks[0], ranks[1], ranks[2], ranks[3], ranks[4]])
}

/// Best five-card score out of five to seven cards.
///
/// Returns `None` for inputs outside that range, where the JavaScript oracle
/// throws.
pub fn evaluate(cards: &[u8]) -> Option<Score> {
    let length = cards.len();
    if !(5..=7).contains(&length) {
        return None;
    }

    let mut best: Option<Score> = None;
    for a in 0..length.saturating_sub(4) {
        for b in (a + 1)..length.saturating_sub(3) {
            for c in (b + 1)..length.saturating_sub(2) {
                for d in (c + 1)..length.saturating_sub(1) {
                    for e in (d + 1)..length {
                        let candidate =
                            rank_five([cards[a], cards[b], cards[c], cards[d], cards[e]]);
                        // Strictly greater, so the first of equal scores wins,
                        // exactly as the oracle's `compareRanks(...) > 0` does.
                        if best.is_none_or(|current| candidate > current) {
                            best = Some(candidate);
                        }
                    }
                }
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::parse_card;

    fn hand(cards: &str) -> Vec<u8> {
        cards.split(' ').map(|c| parse_card(c).unwrap()).collect()
    }

    #[test]
    fn recognizes_every_hand_class() {
        let straight_flush = evaluate(&hand("AS KS QS JS TS 2C 3D")).unwrap();
        let quads = evaluate(&hand("AH AD AC AS KD 2C 3D")).unwrap();
        let full_house = evaluate(&hand("KH KD KC 2S 2D AC 3D")).unwrap();
        let wheel = evaluate(&hand("AS 2D 3H 4C 5S KD QD")).unwrap();

        assert_eq!(straight_flush.category_name(), "Straight flush");
        assert_eq!(quads.category_name(), "Four of a kind");
        assert_eq!(full_house.category_name(), "Full house");
        assert_eq!(wheel.category_name(), "Straight");
        assert!(straight_flush > quads);
        assert!(quads > full_house);
    }

    #[test]
    fn wheel_straight_ranks_below_six_high() {
        let wheel = evaluate(&hand("AS 2D 3H 4C 5S")).unwrap();
        let six_high = evaluate(&hand("2D 3H 4C 5S 6D")).unwrap();
        assert!(six_high > wheel);
        // The wheel's high card is the five, not the ace.
        assert_eq!(wheel.0[1], 5);
    }

    #[test]
    fn steel_wheel_is_a_straight_flush() {
        let steel = evaluate(&hand("AS 2S 3S 4S 5S")).unwrap();
        assert_eq!(steel.category_name(), "Straight flush");
        assert_eq!(steel.0[1], 5);
    }

    #[test]
    fn rejects_hands_outside_five_to_seven_cards() {
        assert!(evaluate(&hand("AS KS QS JS")).is_none());
        assert!(evaluate(&[0, 1, 2, 3, 4, 5, 6, 7]).is_none());
    }
}
