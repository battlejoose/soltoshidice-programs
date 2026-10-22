use super::*;

#[test]
fn afk_upgrade_preserves_an_already_waiting_mainnet_layout_hand() {
    let previous = std::env::var("POKER_PREVIOUS_PROGRAM_SO")
        .expect("set POKER_PREVIOUS_PROGRAM_SO to the verified October 14 mainnet binary");
    let (mut h, wallets, config) = seated(3);
    h.svm.add_program_from_file(program_id(), &previous).unwrap();
    let (session, _, _) = deal_to_hole_cards(&mut h,
        &Signing { keys: &wallets, sessions: None }, &config, &mut Meter::default());
    finish_betting(&mut h, &wallets, &[0, 1, 2], &config);
    let deal: MentalDeal = h.account(&mental_deal_pda(&config));
    assert_eq!(deal.next_round, 0);
    assert!(deal.deadline > h.now * 1_000 + 60_000);
    let addresses = [table_state_pda(&config), hand_pda(&config), mental_deal_pda(&config)];
    let before: Vec<_> = addresses.iter().map(|key| h.svm.get_account(key).unwrap()).collect();
    h.svm.add_program_from_file(program_id(), PROGRAM_SO).unwrap();
    for (i, key) in addresses.iter().enumerate() {
        assert_eq!(h.svm.get_account(key).unwrap().data, before[i].data);
        assert_eq!(h.svm.get_account(key).unwrap().lamports, before[i].lamports);
    }
    // Existing clients use the unchanged instruction and account formats.
    publish_board(&mut h, &wallets[0], &config, &session, &deal, 0, &[0, 1, 2]);
    publish_board(&mut h, &wallets[2], &config, &session, &deal, 2, &[0, 1, 2]);
    let after: MentalDeal = h.account(&mental_deal_pda(&config));
    assert_eq!(after.deadline, h.now * 1_000 + 60_000);
    h.advance(20);
    recover(&mut h, &wallets[0], &config, &session, &deal, 1, 0, false).unwrap();
    let hand: HandState = h.account(&hand_pda(&config));
    assert_eq!(hand.timed_out, 1 << 1);
    assert_ne!(hand.stage, HandStage::Complete);
    assert_eq!(h.vault_balance(&config), 3 * BUY_IN);
}

fn finish_betting(h: &mut Harness, wallets: &[Keypair], seats: &[u8], config: &Pubkey) {
    for _ in 0..30 {
        let hand: HandState = h.account(&hand_pda(config));
        if hand.awaiting != 0 || hand.stage == HandStage::Complete { return; }
        let player = seats.iter().position(|s| *s == hand.actor_seat).unwrap();
        act(h, &wallets[player], None, config, riverproof::instructions::PokerAction::Check)
            .or_else(|_| act(h, &wallets[player], None, config, riverproof::instructions::PokerAction::Call))
            .unwrap();
    }
    panic!("betting did not finish");
}

fn publish_board(h: &mut Harness, wallet: &Keypair, config: &Pubkey,
    session: &Session, deal: &MentalDeal, player: usize, offsets: &[u8]) {
    let from = deal.slot_count as usize - 5;
    let slots: Vec<u8> = offsets.iter().map(|o| (from + *o as usize) as u8).collect();
    let shares = slots.iter().map(|i| encode_scalar(&session.participants[player]
        .unlock(deal.slots[*i as usize].position).unwrap())).collect();
    mental_send(h, wallet, None, config, riverproof::instruction::PublishUnlocks {
        slots, shares, keys: vec![], paths: vec![], round_digest: [0; 32],
    }.data()).unwrap();
}

fn recover(h: &mut Harness, wallet: &Keypair, config: &Pubkey,
    session: &Session, deal: &MentalDeal, absent: usize, offset: u8, corrupt: bool) -> Result<(), String> {
    let position = deal.slots[deal.slot_count as usize - 5 + offset as usize].position;
    let lock = &session.deal.locks[absent];
    let mut key = lock.keys[position as usize];
    if corrupt { key[0] ^= 1; }
    mental_send(h, wallet, None, config, riverproof::instruction::RecoverUnlocks {
        absent: absent as u8, board: vec![offset],
        shares: vec![encode_scalar(&session.participants[absent].unlock(position).unwrap())],
        keys: vec![key], paths: riverproof_mental::merkle::path(&lock.keys, position as usize).to_vec(),
        round_digest: lock_round_digest(lock),
    }.data())
}

#[test]
fn afk_board_recovery_uses_each_street_clock_and_marks_only_the_absent_seat() {
    // Non-contiguous seats catch confusing a deal-player bit with a seat bit.
    let seats = [0u8, 4, 5];
    let (mut h, wallets, config) = seated_at(&seats);
    let (session, _, _) = deal_to_hole_cards(&mut h,
        &Signing { keys: &wallets, sessions: None }, &config, &mut Meter::default());
    let hand_size = h.svm.get_account(&hand_pda(&config)).unwrap().data.len();
    let deal_size = h.svm.get_account(&mental_deal_pda(&config)).unwrap().data.len();
    let absent = 1usize;

    for offsets in [vec![0u8, 1, 2], vec![3], vec![4]] {
        finish_betting(&mut h, &wallets, &seats, &config);
        let deal: MentalDeal = h.account(&mental_deal_pda(&config));
        assert!(deal.deadline > h.now * 1_000 + 60_000, "fixture must reproduce unused betting time");
        for player in [0usize, 2] {
            publish_board(&mut h, &wallets[player], &config, &session, &deal, player, &offsets);
        }
        let after: MentalDeal = h.account(&mental_deal_pda(&config));
        assert_eq!(after.deadline, h.now * 1_000 + 60_000);
        assert!(abandon(&mut h, &wallets[0], &config).is_err(), "new street retains its stall grace");
        h.advance(19);
        assert!(recover(&mut h, &wallets[0], &config, &session, &deal, absent, offsets[0], false)
            .unwrap_err().contains("RecoveryNotDue"));
        h.advance(1);
        let before: HandState = h.account(&hand_pda(&config));
        assert!(recover(&mut h, &wallets[0], &config, &session, &deal, absent, offsets[0], true).is_err());
        let rejected: HandState = h.account(&hand_pda(&config));
        assert_eq!(before.timed_out, rejected.timed_out, "bad proof must not mark a player absent");
        // Each successful recovery is real progress. Separate transactions for
        // the next proof still obey the existing recovery-grace contract.
        for (i, offset) in offsets.iter().enumerate() {
            if i > 0 { h.advance(20); }
            recover(&mut h, &wallets[0], &config, &session, &deal, absent, *offset, false).unwrap();
        }
        let marked: HandState = h.account(&hand_pda(&config));
        assert_eq!(marked.timed_out, 1 << seats[absent]);
        assert_eq!(marked.folded, before.folded, "recovery does not fold or disqualify a contender");
        assert_eq!(marked.contributions, before.contributions);
        mental_send(&mut h, &wallets[0], None, &config,
            riverproof::instruction::RevealMentalStreet {}.data()).unwrap();
    }
    finish_betting(&mut h, &wallets, &seats, &config);
    let showdown: HandState = h.account(&hand_pda(&config));
    assert_eq!(showdown.awaiting, 5);
    // Returning players may still show and win this hand. Only their readiness
    // for the next deal changes; no tokens are paid to a keeper or withdrawn.
    let deal: MentalDeal = h.account(&mental_deal_pda(&config));
    for player in 0..3 {
        let slots: Vec<u8> = (0..deal.slot_count as usize - 5)
            .filter(|i| deal.slots[*i].owner == seats[player]).map(|i| i as u8).collect();
        let shares = slots.iter().map(|i| encode_scalar(&session.participants[player]
            .unlock(deal.slots[*i as usize].position).unwrap())).collect();
        mental_send(&mut h, &wallets[player], None, &config, riverproof::instruction::PublishUnlocks {
            slots, shares, keys: vec![], paths: vec![], round_digest: [0; 32],
        }.data()).unwrap();
        mental_send(&mut h, &wallets[player], None, &config,
            riverproof::instruction::RevealMentalHole { seat: seats[player] }.data()).unwrap();
    }
    let hand: HandState = h.account(&hand_pda(&config));
    let table: TableState = h.account(&table_state_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete);
    assert!(!table.seats[seats[absent] as usize].ready);
    assert!(table.seats[seats[absent] as usize].occupied);
    assert_eq!(table.seats.iter().map(|s| s.stack).sum::<u64>(), 3 * BUY_IN);
    assert_eq!(h.vault_balance(&config), 3 * BUY_IN);
    assert_eq!(hand_size, h.svm.get_account(&hand_pda(&config)).unwrap().data.len());
    assert_eq!(deal_size, h.svm.get_account(&mental_deal_pda(&config)).unwrap().data.len());
}
