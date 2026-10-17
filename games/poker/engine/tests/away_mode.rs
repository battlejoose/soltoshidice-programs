use riverproof_engine::{cards::ordered_deck, table::{Action, Awaiting, DeckSource, Seat, Stage, Table, TableConfig}};

fn table(count: usize) -> Table {
    let mut t = Table::new(TableConfig::new(50, 0, 10, 5_000, 40_000));
    t.deck_source = DeckSource::Mental;
    for s in &mut t.seats[..count] {
        *s = Seat { occupied: true, stack: 5_000, ready: true, connected: true, leave_after_hand: false };
    }
    t
}
fn act(t: &mut Table, action: Action, now: i64) {
    let seat=t.hand.as_ref().unwrap().actor_seat.unwrap();
    t.apply_action(seat, action, now, None).unwrap();
}

#[test]
fn away_seats_pay_both_blinds_without_becoming_encryption_participants() {
    let mut t=table(5);
    t.seats[3].ready=false;
    t.seats[4].ready=false;
    t.seats[4].leave_after_hand=true; // Requesting cash-out cannot park a free seat.
    t.dealer_seat=Some(1);
    let total=t.chip_total();
    t.start_hand(ordered_deck(), 0).unwrap();
    let h=t.hand.as_ref().unwrap();
    assert_eq!((h.dealer_seat,h.small_blind_seat,h.big_blind_seat),(2,3,4));
    assert_eq!(h.active_seats,0b00111);
    assert_eq!(h.folded,0b11000);
    assert_eq!((t.seats[3].stack,t.seats[4].stack),(4_950,4_900));
    assert_eq!(t.chip_total(),total);
    t.open_betting(1_000).unwrap();
    act(&mut t,Action::Fold,1_001);
    act(&mut t,Action::Fold,1_002);
    assert_eq!(t.hand.as_ref().unwrap().stage,Stage::Complete);
    assert_eq!(t.seats[2].stack,5_150);
    assert_eq!(t.chip_total(),total);
}

#[test]
fn auto_checks_for_free_and_folds_to_a_bet_without_unreadying() {
    let mut t=table(3);
    t.auto_check_fold=1<<2;
    t.start_hand(ordered_deck(),0).unwrap();
    t.open_betting(1_000).unwrap();
    assert_eq!(t.hand.as_ref().unwrap().action_deadline,41_000);
    act(&mut t,Action::Call,1_001);
    act(&mut t,Action::Call,1_002);
    assert_eq!(t.hand.as_ref().unwrap().awaiting,Awaiting::Flop);
    assert_eq!(t.hand.as_ref().unwrap().folded,0);
    t.supply_street(&[20,25,30],2_000).unwrap();
    assert_eq!(t.hand.as_ref().unwrap().actor_seat,Some(1));
    act(&mut t,Action::Raise{to:100},2_001);
    assert_ne!(t.hand.as_ref().unwrap().folded & (1<<2),0);
    assert_eq!(t.hand.as_ref().unwrap().actor_seat,Some(0));
    act(&mut t,Action::Fold,2_002);
    assert!(t.seats[2].ready);
    assert_eq!(t.auto_check_fold,1<<2);
    assert_eq!(t.hand.as_ref().unwrap().timed_out,0);
}

#[test]
fn all_automatic_seats_terminate_without_a_turn_timer_or_chip_creation() {
    let mut t=table(10);
    t.auto_check_fold=0x3ff;
    let total=t.chip_total();
    t.start_hand(ordered_deck(),0).unwrap();
    t.open_betting(1_000).unwrap();
    assert_eq!(t.hand.as_ref().unwrap().stage,Stage::Complete);
    assert_eq!(t.chip_total(),total);
    assert!(t.seats.iter().all(|s|s.ready));
}

#[test]
fn absent_short_stacks_are_capped_and_voided_deals_refund_every_contributor() {
    let mut t=table(5);
    t.seats[3].ready=false; t.seats[3].stack=17;
    t.seats[4].ready=false; t.seats[4].stack=33;
    t.dealer_seat=Some(1);
    let total=t.chip_total();
    t.start_hand(ordered_deck(),0).unwrap();
    assert_eq!((t.seats[3].stack,t.seats[4].stack),(0,0));
    assert_eq!(t.chip_total(),total);
    t.void_hand(60_000).unwrap();
    assert_eq!((t.seats[3].stack,t.seats[4].stack),(17,33));
    assert_eq!(t.chip_total(),total);
}

#[test]
fn dead_blind_side_pots_do_not_return_money_to_absent_seats() {
    use riverproof_engine::pots::build_side_pots_with_absent;
    let mut contributions=[0;10]; contributions[..5].copy_from_slice(&[10,20,20,50,100]);
    let pots=build_side_pots_with_absent(&contributions,0b11000,0b11000,0b111);
    assert_eq!(pots.as_slice().iter().map(|p|p.amount).sum::<u64>(),200);
    assert!(pots.as_slice().iter().all(|p| !p.refund && p.eligible != 0 && p.eligible & 0b11000 == 0));
    assert_eq!(pots.as_slice().last().unwrap().eligible,0b110);
}

#[test]
fn absence_does_not_relax_the_private_dealing_threshold() {
    let mut t=table(4);t.seats[2].ready=false;t.seats[3].ready=false;
    let total=t.chip_total();
    assert!(t.start_hand(ordered_deck(),0).is_err());
    assert_eq!(t.chip_total(),total);
}

#[test]
fn a_running_turn_keeps_its_deadline_while_new_turns_use_forty_seconds() {
    let mut t=table(3);
    t.start_hand(ordered_deck(),0).unwrap();t.open_betting(1_000).unwrap();
    t.hand.as_mut().unwrap().action_deadline=61_000; // legacy hand at upgrade
    assert!(!t.force_timeout(41_000).unwrap());
    assert!(t.force_timeout(61_000).unwrap());
    assert_eq!(t.hand.as_ref().unwrap().action_deadline,101_000);
}

#[test]
fn an_away_seat_remains_in_the_blind_rotation_over_multiple_hands() {
    let mut t=table(5);t.seats[4].ready=false;
    let total=t.chip_total();
    let mut paid=0;
    for round in 0..10 {
        let now=round*10_000;
        if round==0 {t.start_hand(ordered_deck(),now).unwrap();}
        else {assert!(t.start_next_hand(now));t.start_hand(ordered_deck(),now).unwrap();}
        paid+=t.hand.as_ref().unwrap().contributions[4];
        t.open_betting(now+1).unwrap();
        while t.hand.as_ref().unwrap().stage!=Stage::Complete {act(&mut t,Action::Fold,now+2);}
        assert_eq!(t.chip_total(),total);
    }
    assert_eq!(paid,300,"two complete rotations charge two small and two big blinds");
    assert_eq!(t.seats[4].stack,4_700);
}
