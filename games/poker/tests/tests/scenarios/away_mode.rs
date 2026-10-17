use super::*;

fn auto(h: &mut Harness, wallet: &Keypair, config: &Pubkey, enabled: bool) -> Result<(), String> {
    h.send(Instruction { program_id: program_id(),
        accounts: play_accounts(&wallet.pubkey(), None, config).to_account_metas(None),
        data: riverproof::instruction::SetAutoCheckFold { enabled }.data() }, &[wallet])
}

#[test]
fn auto_check_fold_is_owner_authorized_persistent_and_immediate() {
    let (mut h,wallets,config)=seated(3);
    let outsider=h.player("outsider");
    assert!(auto(&mut h,&outsider,&config,true).is_err());
    let size=h.svm.get_account(&table_state_pda(&config)).unwrap().data.len();
    let signing=Signing{keys:&wallets,sessions:None};
    deal_to_hole_cards(&mut h,&signing,&config,&mut Meter::default());
    let before:HandState=h.account(&hand_pda(&config));
    let actor=before.actor_seat as usize;
    auto(&mut h,&wallets[actor],&config,true).unwrap();
    let after:HandState=h.account(&hand_pda(&config));
    let state:TableState=h.account(&table_state_pda(&config));
    assert_ne!(after.folded & (1<<actor),0,"mode folds immediately to a bet");
    assert!(state.seats[actor].ready,"away mode must retain deal participation");
    assert!(!state.seats[actor].connected,"legacy mode byte persists");
    assert_eq!(state.seats[actor].stack,BUY_IN);
    assert_eq!(after.timed_out & (1<<actor),0,"explicit mode is not a timeout");
    assert_eq!(h.svm.get_account(&table_state_pda(&config)).unwrap().data.len(),size);
    auto(&mut h,&wallets[actor],&config,false).unwrap();
    let state:TableState=h.account(&table_state_pda(&config));
    assert!(state.seats[actor].connected);
    assert_ne!(h.account::<HandState>(&hand_pda(&config)).folded & (1<<actor),0,"resume never undoes a fold");
}

#[test]
fn legacy_sit_out_posts_blinds_and_cannot_cash_out_live_contributions() {
    let (mut h,wallets,config)=seated(5);
    set_ready(&mut h,&wallets[1],&config,false);
    set_ready(&mut h,&wallets[2],&config,false);
    h.advance(3);
    start_hand_mental(&mut h,&wallets[0],&config).unwrap();
    let hand:HandState=h.account(&hand_pda(&config));
    let state:TableState=h.account(&table_state_pda(&config));
    let deal:MentalDeal=h.account(&mental_deal_pda(&config));
    assert_eq!(deal.players,3);
    assert_eq!(hand.active_seats,0b11001);
    assert_eq!((hand.small_blind_seat,hand.big_blind_seat),(1,2));
    assert_eq!(hand.contributions[1],SMALL_BLIND);
    assert_eq!(hand.contributions[2],SMALL_BLIND*2);
    assert_eq!(state.seats[2].stack,BUY_IN-SMALL_BLIND*2);
    let ix=Instruction{ program_id:program_id(),
        accounts:seat_accounts(&h,&wallets[2].pubkey(),&config).to_account_metas(None),
        data:riverproof::instruction::LeaveTable{}.data() };
    h.send(ix,&[&wallets[2]]).unwrap(); // request only, cannot withdraw committed chips
    let after:TableState=h.account(&table_state_pda(&config));
    assert!(after.seats[2].occupied && after.seats[2].leave_after_hand);
    assert_eq!(after.escrowed,state.escrowed);
    assert_eq!(after.seats[2].stack,state.seats[2].stack);
}

#[test]
fn dead_blind_fees_are_collected_from_winners_not_away_players() {
    let mut c=[0;10];c[3]=50;c[4]=100;
    let mut awards=[0;10];awards[2]=150;
    let fees=riverproof::pot_fees::pot_fees_with_absent(&c,0b11011,&awards,0b11000,0b100).unwrap();
    assert_eq!(fees[2].fee_chips,1);
    assert_eq!(fees[3].uncalled_chips,0);
    assert_eq!(fees[4].uncalled_chips,0);
}

#[test]
fn encrypted_deal_completes_with_an_absent_button_and_blind() {
    let (mut h,wallets,config)=seated(5);
    set_ready(&mut h,&wallets[0],&config,false);
    set_ready(&mut h,&wallets[1],&config,false);
    let signing=Signing{keys:&wallets[2..],sessions:None};
    h.advance(3);
    let (_,deal,_)=deal_to_hole_cards(&mut h,&signing,&config,&mut Meter::default());
    let hand:HandState=h.account(&hand_pda(&config));
    assert_eq!(deal.seats[..3],[2,3,4]);
    assert_eq!(hand.dealer_seat,0);
    assert_eq!(hand.contributions[1],SMALL_BLIND);
    assert_ne!(hand.folded & 0b11,0);
    assert_eq!(hand.stage,HandStage::Preflop);
    assert!(hand.actor_seat<10);
    assert_eq!(hand.action_deadline,h.now*1_000+40_000);
}
