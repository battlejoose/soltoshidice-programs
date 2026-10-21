use super::*;

#[test]
fn direct_profit_respects_fifo_custody_recipient_and_readonly_spot() {
    let mut h=Bank::new();let primary=h.tokens[0];let usd=h.tokens[1];let owner=h.signer.pubkey();
    h.send("deposit winner collateral",vec![h.deposit(usd,10*UNIT)],false);
    let win=h.committed(usd,4,10*UNIT,false,true);
    let position=pda(&[b"position",owner.as_ref(),&4u64.to_le_bytes()]);
    h.send("create pre-existing USD claim",h.settlement(usd,win,1),false);
    let claim:Position=get(&h.svm,position);let debt=claim.remaining;assert!(debt>0);
    let locked=12_345;let capital=1;
    let mut inventory:Inventory=get(&h.svm,primary.inventory);
    inventory.locked=locked;inventory.treasury=capital;
    token_account(&mut h.svm,primary.vault,h.authority,primary.mint,primary.program,locked+capital);
    put(&mut h.svm,primary.inventory,&inventory,p::ID);
    let baseline=h.svm.get_account(&position).unwrap().data;
    // Same mint but someone else's wallet must not receive the user's claim.
    let wrong=Pubkey::new_unique();token_account(&mut h.svm,wrong,Pubkey::new_unique(),primary.mint,primary.program,0);
    let mut bad=h.payout_raw(position);bad.accounts.iter_mut().find(|a|a.pubkey==primary.wallet).unwrap().pubkey=wrong;
    h.send("reject wrong recipient",vec![bad],true);
    let mut bad=h.payout_raw(position);bad.accounts.iter_mut().find(|a|a.pubkey==primary.wallet).unwrap().pubkey=usd.wallet;
    h.send("reject wrong mint",vec![bad],true);
    let mut bad=h.payout_raw(position);bad.accounts.iter_mut().find(|a|a.pubkey==primary.pool).unwrap().is_writable=true;
    h.send("reject transaction-writable price reserves",vec![bad],true);
    let mut pool:Pool=get(&h.svm,h.pool);pool.queue_head+=1;put(&mut h.svm,h.pool,&pool,p::ID);
    h.send("reject FIFO skip",vec![h.payout_raw(position)],true);
    pool.queue_head-=1;put(&mut h.svm,h.pool,&pool,p::ID);
    assert_eq!(h.svm.get_account(&position).unwrap().data,baseline);
    // One token atom may be sub-dollar dust; it must never burn claim debt.
    h.send("reject sub-micro-dollar inventory",vec![h.payout_raw(position)],true);
    inventory.treasury=100*UNIT;
    token_account(&mut h.svm,primary.vault,h.authority,primary.mint,primary.program,locked+inventory.treasury);
    put(&mut h.svm,primary.inventory,&inventory,p::ID);
    let owner_before=token(&h.svm,primary.wallet);let usd_before=token(&h.svm,usd.wallet);
    let reserves_before=h.svm.get_account(&primary.pool).unwrap();
    h.send("pay partial old USD claim in tokens",vec![h.payout_raw(position)],false);
    let partial:Position=get(&h.svm,position);
    assert!(partial.remaining>0 && partial.remaining<debt);
    assert_eq!(token(&h.svm,primary.wallet)-owner_before,inventory.treasury);
    assert_eq!(token(&h.svm,primary.vault),locked);assert_eq!(get::<Inventory>(&h.svm,primary.inventory).locked,locked);
    assert_eq!(token(&h.svm,usd.wallet),usd_before);assert_eq!(h.svm.get_account(&primary.pool).unwrap().data,reserves_before.data);
    assert_eq!(get::<Pool>(&h.svm,h.pool).queue_head,claim.queue_seq);
    // Refill treasury only; prior payment cannot be charged twice.
    inventory=get(&h.svm,primary.inventory);inventory.treasury=1_000_000*UNIT;
    token_account(&mut h.svm,primary.vault,h.authority,primary.mint,primary.program,locked+inventory.treasury);
    put(&mut h.svm,primary.inventory,&inventory,p::ID);
    h.send("pay only remaining debt",vec![h.payout_raw(position)],false);
    assert_eq!(get::<Position>(&h.svm,position).remaining,0);
    assert_eq!(get::<Pool>(&h.svm,h.pool).queue_head,claim.queue_seq+1);
    assert_eq!(get::<Inventory>(&h.svm,primary.inventory).locked,locked);
    assert_eq!(token(&h.svm,usd.wallet),usd_before);
    h.send("reject duplicate final payout",vec![h.payout_raw(position)],true);
}
