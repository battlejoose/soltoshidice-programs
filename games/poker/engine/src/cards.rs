//! Card encoding.
//!
//! A card is a single byte, `suit * 13 + rank_ordinal`, matching the ordering
//! `server/poker-engine.mjs` builds its deck in:
//!
//! ```text
//! SUITS.flatMap((suit) => RANKS.map((rank) => `${rank}${suit}`))
//! ```
//!
//! so rank ordinals run `2..A` as `0..12` and suits run `C D H S` as `0..3`.
//! `"2C"` is 0 and `"AS"` is 51.

pub const DECK_SIZE: usize = 52;

pub const RANK_CHARS: [u8; 13] = *b"23456789TJQKA";
pub const SUIT_CHARS: [u8; 4] = *b"CDHS";

/// Rank as the engine compares it: deuce is 2, ace is 14.
#[inline]
pub fn rank_value(card: u8) -> u8 {
    (card % 13) + 2
}

#[inline]
pub fn suit(card: u8) -> u8 {
    card / 13
}

/// Parses `"AS"`, `"Th"`, etc. Case-insensitive on both characters.
pub fn parse_card(text: &str) -> Option<u8> {
    let bytes = text.as_bytes();
    if bytes.len() != 2 {
        return None;
    }
    let rank = RANK_CHARS
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(&bytes[0]))?;
    let suit = SUIT_CHARS
        .iter()
        .position(|candidate| candidate.eq_ignore_ascii_case(&bytes[1]))?;
    Some((suit * 13 + rank) as u8)
}

/// Renders a card back to the two-character form the JavaScript engine uses.
pub fn format_card(card: u8) -> [u8; 2] {
    [
        RANK_CHARS[(card % 13) as usize],
        SUIT_CHARS[(card / 13) as usize],
    ]
}

/// An ordered deck, before shuffling.
pub fn ordered_deck() -> [u8; DECK_SIZE] {
    let mut deck = [0u8; DECK_SIZE];
    let mut index = 0;
    while index < DECK_SIZE {
        deck[index] = index as u8;
        index += 1;
    }
    deck
}

/// Fisher-Yates matching `makeDeck` in the JavaScript oracle: it walks from the
/// end and calls `random(index + 1)` for each swap.
///
/// On-chain the ordering arrives from the Arcium shuffle instead, so this exists
/// for local validator runs and for the differential tests.
pub fn shuffle_deck<F>(mut random: F) -> [u8; DECK_SIZE]
where
    F: FnMut(u32) -> u32,
{
    let mut deck = ordered_deck();
    let mut index = DECK_SIZE - 1;
    while index > 0 {
        let swap = random(index as u32 + 1) as usize;
        deck.swap(index, swap);
        index -= 1;
    }
    deck
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn card_encoding_round_trips() {
        assert_eq!(parse_card("2C"), Some(0));
        assert_eq!(parse_card("AS"), Some(51));
        assert_eq!(parse_card("TH"), Some(2 * 13 + 8));
        assert_eq!(rank_value(parse_card("AS").unwrap()), 14);
        assert_eq!(rank_value(parse_card("2C").unwrap()), 2);
        for card in 0..52u8 {
            let text = format_card(card);
            let text = core::str::from_utf8(&text).unwrap();
            assert_eq!(parse_card(text), Some(card));
        }
    }

    #[test]
    fn rejects_malformed_cards() {
        assert_eq!(parse_card(""), None);
        assert_eq!(parse_card("A"), None);
        assert_eq!(parse_card("1C"), None);
        assert_eq!(parse_card("AX"), None);
        assert_eq!(parse_card("ASX"), None);
    }
}
