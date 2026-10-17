#![cfg(feature="mainnet")]
use acidpump_er::{engine::*,stake_backing::core_asset};
use anchor_lang::prelude::*;

fn snapshot(c:&ErCustody)->ErCustody {
    let mut data=vec![];c.try_serialize(&mut data).unwrap();ErCustody::try_deserialize(&mut data.as_slice()).unwrap()
}
fn full()->(ErTrader,ErRisk,ErCustody) {
    let owner=Pubkey::new_unique();let config=Pubkey::new_unique();let pool=Pubkey::new_unique();
    let mut t=ErTrader{owner,config,pool,next_nonce:1,..ErTrader::default()};
    let mut c=ErCustody{owner,config,pool,generation:ER_ASSETS as u64,..ErCustody::default()};
    for i in 0..ER_ASSETS {
        let asset=if i==0{core_asset()}else{Pubkey::new_unique()};
        c.assets[i]=CustodyAsset{asset,generation:i as u64+1,deposited_credits:100,available:0,..CustodyAsset::default()};
        t.assets[i]=ErBalance{asset,generation:i as u64+1,credited_deposits:100,available:0,..ErBalance::default()};
    }
    t.custody_generation=c.generation;
    (t,ErRisk{config,..ErRisk::default()},c)
}

#[test]
fn many_token_lifetimes_reuse_empty_records_without_replaying_old_credits() {
    assert!(ER_ASSETS>=ER_SLOTS+1);
    let(mut t,mut r,mut c)=full();let retired=c.assets[1].asset;
    let first=snapshot(&c);let mut history=vec![];
    for n in 0..200 {
        // Revisit the same retired token, with a smaller fresh deposit. A
        // ticker/pubkey match must not reuse that token's former cursor.
        let asset=if n%2==0{retired}else{Pubkey::new_unique()};
        let index=c.asset_index_or_insert(asset).unwrap();assert_eq!(index,1);
        c.assets[index].available=3;c.assets[index].deposited_credits+=3;
        sync_custody(&mut t,&mut r,&c).unwrap();
        let i=t.asset_index(asset).unwrap();assert_eq!(t.assets[i].available,3);
        let current=snapshot(&c);sync_custody(&mut t,&mut r,&current).unwrap();
        sync_custody(&mut t,&mut r,&first).unwrap();
        for old in history.iter().rev().take(3){sync_custody(&mut t,&mut r,old).unwrap();}
        assert_eq!(t.assets[i].available,3);
        let nonce=t.next_nonce;withdraw_slot(&mut t,&mut r,asset,(ER_SLOTS-1)as u8,nonce,3,110,100).unwrap();
        c.assets[index].available=0;c.receipts[ER_SLOTS-1]=ErReceipt{nonce,kind:ER_WITHDRAW,settled:true,..ErReceipt::default()};
        // A following deposit may replace the exhausted base record before
        // ER observes the withdrawal receipt. Sync releases it before reuse.
        history.push(snapshot(&c));
    }
    sync_custody(&mut t,&mut r,&c).unwrap();
    assert!(t.slots.iter().all(|s|s.state==ER_FREE));
    assert!(t.assets.iter().all(|a|a.available==0));
    assert_eq!(t.assets[0].asset,core_asset());
    assert_eq!(c.assets[0].asset,core_asset());
}

#[test]
fn generations_distinguish_a_token_readded_before_er_observes_its_removal() {
    let(mut t,mut r,mut c)=full();let a=c.assets[1].asset;
    let b=Pubkey::new_unique();let bi=c.asset_index_or_insert(b).unwrap();c.assets[bi].available=5;c.assets[bi].deposited_credits=5;
    let ai=c.asset_index_or_insert(a).unwrap();assert_ne!(ai,bi);c.assets[ai].available=2;c.assets[ai].deposited_credits=2;
    sync_custody(&mut t,&mut r,&c).unwrap();
    assert_eq!(t.assets[t.asset_index(a).unwrap()].available,2);
    assert_eq!(t.assets[t.asset_index(b).unwrap()].available,5);
    sync_custody(&mut t,&mut r,&c).unwrap();
    assert_eq!(t.assets.iter().map(|a|a.available).sum::<u64>(),7);
}

#[test]
fn funded_backing_and_live_or_unacknowledged_positions_cannot_be_discarded() {
    let(mut t,mut r,mut c)=full();
    for a in c.assets.iter_mut().skip(1){a.available=1;}
    // Core is zero but pinned; every other record holds real custody.
    assert!(c.asset_index_or_insert(Pubkey::new_unique()).is_err());
    c.assets[1].available=0;let old=c.assets[1].asset;
    c.asset_index_or_insert(Pubkey::new_unique()).unwrap();
    t.assets[1].available=1;assert!(sync_custody(&mut t,&mut r,&c).is_err());
    t.assets[1].available=0;
    t.slots[0]=ErSlot{state:ER_OPEN,kind:ER_TRADE,asset:old,nonce:1,..ErSlot::default()};
    assert!(sync_custody(&mut t,&mut r,&c).is_err());
    t.slots[0].state=ER_PENDING;assert!(sync_custody(&mut t,&mut r,&c).is_err());
    t.slots[0]=ErSlot::default();sync_custody(&mut t,&mut r,&c).unwrap();
    assert_eq!(t.assets[0].asset,core_asset());
}

#[test]
fn generation_overflow_fails_without_changing_a_record() {
    let(_,_,mut c)=full();c.generation=u64::MAX;let before=c.assets[1];
    assert!(c.asset_index_or_insert(Pubkey::new_unique()).is_err());
    assert_eq!(c.assets[1].asset,before.asset);assert_eq!(c.assets[1].deposited_credits,before.deposited_credits);
    assert_eq!(c.generation,u64::MAX);
}

#[test]
fn legacy_record_order_reconciles_once_without_changing_balances() {
    let(mut t,mut r,mut c)=full();t.assets.rotate_left(7);
    c.assets[11].available=3;c.assets[11].deposited_credits+=3;
    sync_custody(&mut t,&mut r,&c).unwrap();sync_custody(&mut t,&mut r,&c).unwrap();
    assert_eq!(t.assets[t.asset_index(c.assets[11].asset).unwrap()].available,3);
    assert_eq!(t.assets.iter().map(|a|a.available).sum::<u64>(),3);
}

#[test]
fn released_wallet_recovery_handles_recycled_records_without_resetting_credit_protection() {
    let(mut t,_,mut c)=full();let before=snapshot(&c);let asset=Pubkey::new_unique();
    let index=c.asset_index_or_insert(asset).unwrap();c.assets[index].available=3;c.assets[index].deposited_credits=3;
    acidpump_er::recovery::debit(&mut t,&mut c,asset,3).unwrap();
    assert_eq!(c.assets[index].available,0);assert_eq!(t.assets[t.asset_index(asset).unwrap()].available,0);
    assert_eq!(t.custody_generation,c.generation);
    assert!(acidpump_er::recovery::debit(&mut t,&mut c,asset,1).is_err());
    let mut stale=before;assert!(acidpump_er::recovery::debit(&mut t,&mut stale,core_asset(),1).is_err());
}
