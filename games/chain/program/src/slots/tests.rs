use super::*;
fn pending() -> Spin {
    Spin {
        magic: *b"SLOTSS01",
        bank: Pubkey::new_unique(),
        player: Pubkey::new_unique(),
        destination: Pubkey::new_unique(),
        config: Pubkey::new_unique(),
        nonce: 17,
        wager: rules::MAX_WAGER,
        liability: rules::liability(rules::MAX_WAGER).unwrap(),
        seed: [1; 32],
        random: [0; 32],
        requested_slot: 100,
        fulfilled_slot: 0,
        deadline: 1180,
        status: PENDING,
        stops: [0; 3],
        gross: 0,
        bump: 255,
    }
}
#[test]
fn layouts_and_unique_receipts_match_the_wire_contract() {
    assert_eq!(borsh::to_vec(&pending()).unwrap().len(), SPIN_LEN);
    assert_eq!(
        borsh::to_vec(&Config {
            magic: *b"SLOTSC01",
            bank: Pubkey::default(),
            enabled: false,
            max_wager: rules::MAX_WAGER,
            bump: 1
        })
        .unwrap()
        .len(),
        CONFIG_LEN
    );
    assert_eq!(
        borsh::to_vec(&Cursor {
            magic: *b"SLOTSP01",
            bank: Pubkey::default(),
            player: Pubkey::default(),
            next_nonce: 0,
            pending: false,
            bump: 1
        })
        .unwrap()
        .len(),
        CURSOR_LEN
    );
    let program = Pubkey::new_unique();
    let s = pending();
    let cursor = cursor_address(&program, &s.bank, &s.player).0;
    assert_ne!(
        spin_address(&program, &cursor, 17).0,
        spin_address(&program, &cursor, 18).0
    );
    assert_ne!(
        cursor,
        cursor_address(&program, &s.bank, &Pubkey::new_unique()).0
    );
}
#[test]
fn all_reward_allocations_match_the_existing_dice_ledger() {
    for wager in (1..1000).chain([100 * rules::SDICE_UNIT, rules::MAX_WAGER]) {
        let actual = economy::bet_fees(wager).unwrap();
        let expected = rules::rewards(wager).unwrap();
        assert_eq!(
            (
                actual.direct,
                actual.staking,
                actual.tree,
                actual.gross - actual.net
            ),
            (
                expected.direct,
                expected.staking,
                expected.tree,
                expected.total
            )
        );
    }
}
#[test]
fn invalid_or_duplicate_callbacks_cannot_replace_an_accepted_outcome() {
    for (nonce, seed, now, slot) in [
        (18, [1; 32], 1100, 101),
        (17, [2; 32], 1100, 101),
        (17, [1; 32], 1181, 101),
        (17, [1; 32], 1100, 100),
    ] {
        let mut s = pending();
        assert!(callback_result(&mut s, nonce, &seed, &[2; 32], now, slot).is_err());
        assert_eq!(s.status, PENDING);
    }
    let mut s = pending();
    callback_result(&mut s, 17, &[1; 32], &[2; 32], 1180, 101).unwrap();
    assert!(callback_result(&mut s, 17, &[1; 32], &[3; 32], 1180, 102).is_err());
    assert_eq!(s.random, [2; 32]);
}
#[test]
fn completed_spins_settle_once_and_cannot_be_refunded_even_after_timeout() {
    for n in 0..=255u8 {
        let mut s = pending();
        callback_result(&mut s, 17, &[1; 32], &[n; 32], 1100, 101).unwrap();
        let other = 500 * rules::SDICE_UNIT;
        let all = other + s.liability;
        assert!(finish(&mut s.clone(), all, true, 9999).is_err());
        let next = finish(&mut s, all, false, 9999).unwrap();
        assert_eq!(next, other);
        assert_eq!(s.status, SETTLED);
        assert_eq!(Some(s.gross), rules::gross_return(s.wager, s.stops));
        assert!(s.gross + rules::rewards(s.wager).unwrap().total <= s.liability);
        assert!(finish(&mut s, all, false, 9999).is_err());
        assert!(finish(&mut s, all, true, 9999).is_err());
    }
}
#[test]
fn unfulfilled_spins_refund_once_only_after_the_fixed_callback_window() {
    let mut s = pending();
    let total = s.liability + 999;
    assert!(finish(&mut s.clone(), total, true, 1180).is_err());
    assert!(finish(&mut s.clone(), total, false, 1100).is_err());
    assert_eq!(finish(&mut s, total, true, 1181).unwrap(), 999);
    assert_eq!(s.status, REFUNDED);
    assert_eq!(s.gross, s.wager);
    assert!(callback_result(&mut s, 17, &[1; 32], &[2; 32], 1181, 102).is_err());
    assert!(finish(&mut s, total, true, 1182).is_err());
}
