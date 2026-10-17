use super::*;

fn hand(cards: &[u8], wager: u64, split: bool) -> Hand {
    let mut h = Hand { wager, split, ..Hand::default() };
    for &card in cards { h.push(card).unwrap(); }
    h
}
fn player_round(cards: &[u8], dealer: &[u8]) -> Round {
    let mut r = Round::default();
    r.stage = Stage::PlayerTurn; r.actor = 0; r.deadline = 100;
    r.players[0] = Player { credit: 900, original: 100, count: 1,
        hands: [hand(cards, 100, false), Hand::default(), Hand::default(), Hand::default()], returned: 0 };
    r.dealer = hand(dealer, 0, false);
    r
}

#[test]
fn aces_naturals_and_split_twenty_one() {
    assert_eq!(value(&[0, 13, 8], false).unwrap(), Value { total: 21, soft: true, natural: false });
    assert_eq!(value(&[0, 13, 8, 22], false).unwrap(), Value { total: 21, soft: false, natural: false });
    assert_eq!(value(&[0, 9], false).unwrap(), Value { total: 21, soft: true, natural: true });
    assert_eq!(value(&[0, 9], true).unwrap(), Value { total: 21, soft: true, natural: false });
    assert_eq!(value(&[5, 0], false).unwrap().total, 17);
    assert_eq!(value(&[5, 0], false).unwrap().soft, true);
    assert!(value(&[52], false).is_err());
}

#[test]
fn exact_returns_bust_push_natural_and_dealer_bust() {
    for (cards, dealer, credit, outcome) in [
        (vec![9, 8], vec![9, 7], 1100, Outcome::Win),
        (vec![9, 8], vec![9, 8], 1000, Outcome::Push),
        (vec![9, 8], vec![9, 9], 900, Outcome::Loss),
        (vec![0, 9], vec![9, 5, 4], 1150, Outcome::Blackjack),
        (vec![0, 9], vec![0, 9], 1000, Outcome::Push),
        (vec![9, 9, 1], vec![9, 9, 2], 900, Outcome::Loss),
        (vec![9, 8], vec![9, 9, 2], 1100, Outcome::Win),
    ] {
        let mut r = player_round(&cards, &dealer);
        let reserved = r.reserved().unwrap();
        r.settle().unwrap();
        assert_eq!(r.players[0].credit, credit);
        assert_eq!(r.players[0].hands[0].outcome, outcome);
        assert!(r.reserved().unwrap() <= reserved);
    }
    let mut r = player_round(&[0, 9], &[9, 5, 4]);
    r.players[0].hands[0].split = true;
    r.settle().unwrap();
    assert_eq!(r.players[0].credit, 1000); // Split 21 pushes dealer's three-card 21.
}

#[test]
fn original_bets_only_protects_added_stakes_from_dealer_blackjack() {
    let mut r = player_round(&[7, 20], &[0, 9]);
    let p = &mut r.players[0];
    p.credit = 400; p.count = 3;
    p.hands[0] = hand(&[7, 9, 8], 200, true); // Already busted doubled split hand.
    p.hands[1] = hand(&[20, 12], 200, true);
    p.hands[2] = hand(&[33, 25], 200, true);
    r.settle().unwrap();
    assert_eq!(r.players[0].credit, 900);
    assert_eq!(r.players[0].returned, 500);
}

#[test]
fn split_aces_receive_one_card_and_cannot_be_resplit_or_doubled() {
    let mut r = player_round(&[0, 13], &[8]);
    r.act(0, Action::Split, 1).unwrap();
    assert_eq!(r.players[0].count, 2);
    assert_eq!(r.players[0].credit, 800);
    assert_eq!(r.players[0].hands[1].count, 1);
    r.players[0].hands[0].push(9).unwrap();
    assert_eq!(r.players[0].hands[0].status, Status::Stood);
    assert!(!r.players[0].hands[0].value().unwrap().natural);
    assert!(!r.players[0].hands[0].can_split().unwrap());
    r.advance(2).unwrap();
    assert_eq!(r.hand, 1);
    assert_eq!(r.draw, Draw::SplitReplacement);
}

#[test]
fn actions_lock_before_entropy_and_double_draws_once() {
    let mut r = player_round(&[4, 5], &[7]);
    r.act(0, Action::Double, 1).unwrap();
    assert_eq!(r.players[0].credit, 800);
    assert_eq!(r.players[0].hands[0].wager, 200);
    assert_eq!(r.stage, Stage::NeedRandomness);
    let locked = r.clone();
    assert!(r.act(0, Action::Hit, 2).is_err());
    assert_eq!(r, locked);
    assert!(r.fulfill(&[5; 64], &[1; 32], 2).is_err());
    r.requested(2).unwrap();
    assert!(r.requested(2).is_err());
    r.fulfill(&[5; 64], &[1; 32], 3).unwrap();
    assert_eq!(r.players[0].hands[0].count, 3);
    assert_ne!(r.players[0].hands[0].status, Status::Playing);
    assert_eq!(r.draw, Draw::Dealer);
}

#[test]
fn exact_card_multiplicity_and_complete_reset_between_rounds() {
    let mut shoe = [DECKS; 52]; let mut remaining = 52 * u16::from(DECKS);
    let mut source = CardSource { random: &[7; 64], domain: &[3; 32], counter: 0 };
    let mut seen = [0; 52];
    while remaining > 0 { seen[source.next(&mut shoe, &mut remaining).unwrap() as usize] += 1; }
    assert!(seen.iter().all(|&n| n == DECKS));
    assert!(source.next(&mut shoe, &mut remaining).is_err());
    let mut r = Round::default(); r.stage = Stage::Complete; r.shoe = shoe; r.remaining = 0;
    r.players[0].credit = 555;
    r.next_round().unwrap();
    assert_eq!(r.shoe, [DECKS; 52]); assert_eq!(r.id, 2);
    assert_eq!(r.players[0].credit, 555);
}

#[test]
fn no_early_timeout_or_wrong_seat_and_timeout_never_cashes_out() {
    let mut r = player_round(&[9, 5], &[7]);
    let saved = r.clone();
    assert!(r.act(1, Action::Stand, 1).is_err());
    assert!(r.timeout_turn(100).is_err());
    assert_eq!(r, saved);
    r.timeout_turn(101).unwrap();
    assert_eq!(r.players[0].credit, 900);
    assert_eq!(r.players[0].original, 100);
    assert_eq!(r.players[0].hands[0].status, Status::Stood);
    assert_eq!(r.draw, Draw::Dealer);
}

#[test]
fn ten_seats_lock_bets_and_reject_late_or_eleventh_player() {
    let mut r = Round::default();
    for p in r.players.iter_mut() { p.credit = 1000; }
    for seat in 0..SEATS { r.bet(seat, 100, 1).unwrap(); }
    assert!(r.bet(SEATS, 100, 1).is_err());
    assert!(r.bet(0, 100, 1).is_err());
    assert!(r.start(20).is_err());
    r.start(21).unwrap();
    assert!(r.bet(0, 100, 21).is_err());
    r.requested(21).unwrap();
    r.fulfill(&[5; 64], &[8; 32], 22).unwrap();
    assert!(r.players.iter().all(|p| p.hands[0].count == 2));
    assert_eq!(r.dealer.count, 1);
    assert_eq!(r.remaining, 52 * u16::from(DECKS) - 21);
}

#[test]
fn refunds_return_stakes_once_and_preserve_credit() {
    let mut r = player_round(&[7, 20], &[8]);
    r.act(0, Action::Split, 1).unwrap();
    let timeout = r.deadline + 1;
    r.refund_unavailable(timeout).unwrap();
    assert_eq!(r.players[0].credit, 1000);
    assert_eq!(r.reserved().unwrap(), 0);
    assert!(r.refund_unavailable(timeout).is_err());
}

#[test]
fn a_thousand_ten_player_rounds_conserve_cards_credits_and_bank_liability() {
    for trial in 0..1000u64 {
        let mut r = Round::default();
        for p in r.players.iter_mut() { p.credit = 10_000; }
        for seat in 0..SEATS { r.bet(seat, 100, 1).unwrap(); }
        r.start(21).unwrap();
        let mut step = 0u64;
        while r.active() {
            step += 1; assert!(step < 1000);
            match r.stage {
                Stage::NeedRandomness => r.requested(22).unwrap(),
                Stage::WaitingRandomness => {
                    let h = hashv(&[&trial.to_le_bytes(), &step.to_le_bytes()]).to_bytes();
                    let mut entropy = [0; 64]; entropy[..32].copy_from_slice(&h); entropy[32..].copy_from_slice(&h);
                    let liability = r.reserved().unwrap();
                    r.fulfill(&entropy, &h, 22).unwrap();
                    if r.stage == Stage::Complete {
                        let returned: u64 = r.players.iter().map(|p| p.returned).sum();
                        assert!(returned <= liability);
                    }
                }
                Stage::PlayerTurn => {
                    let p = &r.players[r.actor as usize];
                    let h = p.hands[r.hand as usize];
                    let choice = (trial + step) % 5;
                    let action = if h.can_split().unwrap() && p.count < HANDS as u8 && choice < 3 { Action::Split }
                        else if h.count == 2 && !h.split_aces && choice == 3 { Action::Double }
                        else if h.value().unwrap().total < 17 || choice == 0 { Action::Hit }
                        else { Action::Stand };
                    r.act(r.actor as usize, action, 22).unwrap();
                }
                _ => panic!("unexpected active stage"),
            }
        }
        assert_eq!(r.stage, Stage::Complete);
        assert_eq!(r.reserved().unwrap(), 0);
        let returned: u64 = r.players.iter().map(|p| p.returned).sum();
        assert_eq!(r.credits().unwrap() + r.stakes().unwrap() - returned, 100_000);
        let mut accounted = r.shoe;
        for p in r.players.iter() { for h in p.hands.iter().take(p.count as usize) {
            for &card in &h.cards[..h.count as usize] { accounted[card as usize] += 1; }
        }}
        for &card in &r.dealer.cards[..r.dealer.count as usize] { accounted[card as usize] += 1; }
        assert_eq!(accounted, [DECKS; 52]);
        assert!(r.dealer.value().unwrap().total >= 17);
    }
}

#[test]
fn shoe_cannot_exhaust_even_with_forty_maximal_player_hands() {
    // Counting every ace as one, a deck contains 340 points. A player hand
    // holds at most 30 (hit a 20 with a ten); dealer at most 26 (hit a 16).
    // Thus not all shoe cards can be consumed by 40 hands and the dealer.
    let points: u32 = (0..52).map(|c| u32::from(rank(c).unwrap().min(10))).sum();
    assert_eq!(points, 340);
    assert!(points * u32::from(DECKS) > (SEATS * HANDS * 30 + 26) as u32);
}
