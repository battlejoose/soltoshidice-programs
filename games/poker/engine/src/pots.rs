//! Side pot construction.
//!
//! A port of `buildSidePots`. Contributions are indexed by seat; seats are held
//! as bitmasks rather than vectors so the result fits a fixed-size account.

use crate::table::MAX_SEATS;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct SidePot {
    pub amount: u64,
    /// Seats that put chips into this layer.
    pub contributors: u16,
    /// Contributors still live at showdown.
    pub eligible: u16,
    /// A single contributor means uncalled excess returning to its owner.
    pub refund: bool,
}

/// At most one pot per distinct contribution level, so one per seat.
pub const MAX_POTS: usize = MAX_SEATS;

#[derive(Clone, Copy, Debug, Default)]
pub struct SidePots {
    pots: [SidePot; MAX_POTS],
    length: usize,
}

impl SidePots {
    pub fn as_slice(&self) -> &[SidePot] {
        &self.pots[..self.length]
    }

    pub fn len(&self) -> usize {
        self.length
    }

    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}

/// Builds the pot layers for one hand.
///
/// `contributions` is indexed by seat and zero for seats that never posted.
/// `folded` is a bitmask of seats that folded but whose chips stay in the pot.
pub fn build_side_pots(contributions: &[u64; MAX_SEATS], folded: u16) -> SidePots {
    // Distinct positive contribution levels, ascending.
    let mut levels = [0u64; MAX_SEATS];
    let mut level_count = 0usize;
    for amount in contributions.iter().copied() {
        if amount == 0 {
            continue;
        }
        if !levels[..level_count].contains(&amount) {
            levels[level_count] = amount;
            level_count += 1;
        }
    }
    levels[..level_count].sort_unstable();

    let mut result = SidePots::default();
    let mut previous = 0u64;

    for level in levels[..level_count].iter().copied() {
        let mut contributors = 0u16;
        let mut contributor_count = 0u64;
        for (seat, amount) in contributions.iter().copied().enumerate() {
            if amount > 0 && amount >= level {
                contributors |= 1 << seat;
                contributor_count += 1;
            }
        }

        let amount = level.saturating_sub(previous) * contributor_count;
        if amount > 0 {
            result.pots[result.length] = SidePot {
                amount,
                contributors,
                eligible: contributors & !folded,
                refund: contributor_count == 1,
            };
            result.length += 1;
        }
        previous = level;
    }

    result
}

/// Absent seats' compulsory blinds are dead money, never an uncalled raise
/// returned to the absent seat. A layer containing only dead money belongs to
/// the nearest eligible lower pot (or the live contenders if no lower pot).
/// With no absent seats this is exactly the original side-pot calculation.
pub fn build_side_pots_with_absent(
    contributions: &[u64; MAX_SEATS], folded: u16, absent: u16, live: u16,
) -> SidePots {
    let mut result = build_side_pots(contributions, folded);
    let mut eligible = live;
    for pot in result.pots[..result.length].iter_mut() {
        if pot.eligible != 0 { eligible = pot.eligible; }
        if absent != 0 && pot.eligible == 0 {
            pot.refund = false;
            pot.eligible = eligible;
        }
    }
    result
}

/// Expands a seat bitmask to ascending seat indexes.
pub fn seats_of(mask: u16) -> impl Iterator<Item = u8> {
    (0..MAX_SEATS as u8).filter(move |seat| mask & (1 << seat) != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contributions(values: &[(usize, u64)]) -> [u64; MAX_SEATS] {
        let mut result = [0u64; MAX_SEATS];
        for (seat, amount) in values {
            result[*seat] = *amount;
        }
        result
    }

    fn mask(seats: &[u8]) -> u16 {
        seats.iter().fold(0, |acc, seat| acc | (1 << seat))
    }

    #[test]
    fn preserves_folded_contributions_and_isolates_all_in_layers() {
        let pots = build_side_pots(
            &contributions(&[(0, 100), (1, 250), (2, 250), (3, 500)]),
            mask(&[2]),
        );

        assert_eq!(
            pots.as_slice(),
            &[
                SidePot {
                    amount: 400,
                    contributors: mask(&[0, 1, 2, 3]),
                    eligible: mask(&[0, 1, 3]),
                    refund: false,
                },
                SidePot {
                    amount: 450,
                    contributors: mask(&[1, 2, 3]),
                    eligible: mask(&[1, 3]),
                    refund: false,
                },
                SidePot {
                    amount: 250,
                    contributors: mask(&[3]),
                    eligible: mask(&[3]),
                    refund: true,
                },
            ]
        );
    }

    #[test]
    fn a_lone_contributor_is_refunded() {
        let pots = build_side_pots(&contributions(&[(0, 1_000)]), 0);
        assert_eq!(pots.len(), 1);
        assert!(pots.as_slice()[0].refund);
        assert_eq!(pots.as_slice()[0].amount, 1_000);
    }

    #[test]
    fn no_contributions_produce_no_pots() {
        assert!(build_side_pots(&[0u64; MAX_SEATS], 0).is_empty());
    }
}
