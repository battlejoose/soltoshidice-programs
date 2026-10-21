//! Synthetic deep liquidity, real candidate SBF and captured Pump CPIs. No RPC.
use super::*;

fn set_amount(h:&mut Bank,key:Pubkey,amount:u64) {
    let mut a=h.svm.get_account(&key).unwrap();
    let old=u64::from_le_bytes(a.data[64..72].try_into().unwrap());
    if a.data[..32]==SOL.to_bytes() {a.lamports=a.lamports-old+amount;}
    a.data[64..72].copy_from_slice(&amount.to_le_bytes());h.svm.set_account(key,a).unwrap();
}
pub(super) fn seed_high_pool(h:&mut Bank,primary:Token) {
    let v=h.valuation(primary);
    // Source is $100/token with deep liquidity. A 50K-token payout inventory
    // realizes more than $2M and less than $5M through the real two-hop route.
    set_amount(h,v.dex_base,1_000_000*UNIT);
    set_amount(h,v.dex_quote,1_000_000_000_000_000);
    set_amount(h,v.usd_base,100_000_000*UNIT);
    set_amount(h,v.usd_quote,1_000_000_000_000_000);
    let mut inventory:Inventory=get(&h.svm,primary.inventory);
    inventory.treasury=50_000*UNIT;put(&mut h.svm,primary.inventory,&inventory,p::ID);
    let old=token(&h.svm,primary.vault);set_amount(h,primary.vault,old+50_000*UNIT);
}
#[test]
fn large_root_pool_uses_10_70_19_1_and_ten_percent_rewards() {
    for queued in [false,true] {
        let mut h=Bank::new();let primary=h.tokens[0];let owner=h.signer.pubkey();
        seed_high_pool(&mut h,primary);
        if queued {let mut pool:Pool=get(&h.svm,h.pool);pool.queue_usd=1;pool.queue_tail=1;put(&mut h.svm,h.pool,&pool,p::ID);}
        h.send("fund high-pool test position",vec![h.deposit(primary,10_000*UNIT)],false);
        let before=get::<Inventory>(&h.svm,primary.inventory).treasury;let minted=supply(&h.svm,primary.mint);
        let gas=get::<GasReserve>(&h.svm,h.gas).sol_received;
        let intent=h.committed(primary,6,10_000*UNIT,true,false);
        h.send("high pool settlement honors FIFO priority",h.settlement(primary,intent,1),false);
        let pool_tokens=if queued {9_900*UNIT}else{1_000*UNIT};
        assert_eq!(get::<Inventory>(&h.svm,primary.inventory).treasury-before,pool_tokens);
        assert_eq!(minted-supply(&h.svm,primary.mint),if queued {0}else{1_900*UNIT});
        let rewards:OwnershipPool=get(&h.svm,h.ownership);
        assert_eq!(rewards.fee_pending+rewards.reward_pool,if queued {0}else{7_000*UNIT});
        let pos=pda(&[b"position",owner.as_ref(),&6u64.to_le_bytes()]);
        let award:p::OwnershipReceipt=get(&h.svm,pda(&[b"ownership-receipt",pos.as_ref()]));
        assert_eq!(award.issued,10_000*UNIT as u128*10*if queued {2}else{1});
        assert!(get::<GasReserve>(&h.svm,h.gas).sol_received>gas);
        assert_eq!(get::<Pool>(&h.svm,h.pool).queue_usd,if queued {1}else{0});
    }
}
