use super::*;
use super::native_markets::{setup,seed,settlement};
use p::perps_rewards::{PerpsRevenue,MINT,OPERATIONS,OPERATIONS_SECONDARY};
fn ledger(h:&Bank)->Pubkey {pda(&[b"perps-revenue",h.pool.as_ref()])}
fn loss(h:&mut Bank,f:&Value,nonce:u64) {
    let usd=h.tokens[0];h.send("fund USDC loss",vec![h.deposit(usd,100*UNIT)],false);
    h.committed(usd,nonce,100*UNIT,true,false);h.send("settle pPERPS market loss",settlement(f,nonce),false);
}
#[test]
fn perps_loss_tiers_and_fifo_preserve_the_complete_allocation() {
    for high in [false,true] {for queued in [false,true] {
        let(mut h,f,root)=setup("PERPS");if high {seed(&mut h,3_000_000*UNIT);}
        if queued {let mut q:Pool=get(&h.svm,h.pool);q.queue_usd=1;q.queue_tail=1;put(&mut h.svm,h.pool,&q,p::ID);}
        let before=get::<Inventory>(&h.svm,h.tokens[0].inventory).treasury;loss(&mut h,&f,20);
        let r:PerpsRevenue=get(&h.svm,ledger(&h));let o:OwnershipPool=get(&h.svm,h.ownership);
        assert_eq!(r.rewards_usdc,UNIT*if queued{0}else if high{50}else{5});
        assert_eq!(r.operations_usdc,UNIT*if queued{0}else if high{20}else{2});
        assert_eq!(r.burn_usdc,UNIT*if queued{0}else if high{5}else{1});
        assert_eq!(get::<Inventory>(&h.svm,h.tokens[0].inventory).treasury-before,UNIT*if queued{99}else if high{10}else{90});
        assert_eq!(o.reward_pool+o.fee_pending,0,"reserved USDC cannot become claimable PERPS");
        let n:p::NativeRevenue=get(&h.svm,pda(&[b"native-revenue",h.pool.as_ref()]));
        assert_eq!(n.rewards_quote>0,!queued);assert_eq!(n.burn_quote>0,high&&!queued);
        assert_eq!(n.burn_quote,if high {(n.rewards_quote+n.burn_quote)*4/14}else{0});
        if !queued {
            let root_ownership=pda(&[b"ownership-pool",p::ROOT_POOL.as_ref()]);
            let prior:OwnershipPool=get(&h.svm,root_ownership);let supply_before=supply(&h.svm,root.mint);
            h.send("revised share rewards pSDICE holders and burns only SDICE allocation",Bank::client_instructions(&f["buyback"]),false);
            let delivered:p::NativeRevenue=get(&h.svm,pda(&[b"native-revenue",h.pool.as_ref()]));
            let after:OwnershipPool=get(&h.svm,root_ownership);
            assert_eq!(after.reward_pool+after.fee_pending-prior.reward_pool-prior.fee_pending,delivered.sdice_rewarded);
            assert_eq!(supply_before-supply(&h.svm,root.mint),delivered.sdice_burned);
            assert_eq!(delivered.sdice_burned>0,high);
            assert_eq!(get::<PerpsRevenue>(&h.svm,ledger(&h)).burn_usdc,r.burn_usdc);
        }
    }}
}
#[test]
fn perps_buy_claim_and_operations_use_measured_receipts_with_atomic_rollback() {
    let(mut h,f,_)=setup("PERPS");loss(&mut h,&f,20);
    let ledger=ledger(&h);let source=h.tokens[0].vault;let before=token(&h.svm,source);
    h.svm.airdrop(&p::mainnet_config::PUBLISHER,1_000_000_000).unwrap();
    h.send("stake earned pPERPS",Bank::client_instructions(&f["stake"]),false);
    let owner=h.signer.pubkey();let ownership_trader=pda(&[b"ownership-trader",h.pool.as_ref(),owner.as_ref()]);
    let usd=h.tokens[0];
    h.send("legacy claim cannot spend USDC for PERPS rewards",vec![ix(p::accounts::ClaimOwnershipRewards{
        owner,pool:h.pool,ownership_pool:h.ownership,ownership_trader,asset:usd.asset,authority:h.authority,
        mint:usd.mint,token_program:usd.program,vault:usd.vault,destination:usd.wallet},p::instruction::ClaimOwnershipRewards{})],true);
    h.send("legacy unstake cannot mix PERPS dust into USDC",vec![ix(p::accounts::StakeOwnership{
        owner,pool:h.pool,ownership_pool:h.ownership,ownership_trader,inventory:usd.inventory},p::instruction::StakeOwnership{amount:1,unstake:true})],true);
    let mut queued:Pool=get(&h.svm,h.pool);queued.queue_usd=1;queued.queue_tail=1;put(&mut h.svm,h.pool,&queued,p::ID);
    h.send("FIFO blocks pending PERPS conversion",Bank::client_instructions(&f["rewards"]),true);
    queued.queue_usd=0;queued.queue_head=1;put(&mut h.svm,h.pool,&queued,p::ID);
    let mut bad=Bank::client_instructions(&f["rewards"]);let n=bad[0].data.len();
    bad[0].data[n-9..n-1].copy_from_slice(&u64::MAX.to_le_bytes());
    h.send("failed PERPS minimum output rolls back reservation",bad,true);
    assert_eq!(token(&h.svm,source),before);
    h.send("buy partial PERPS rewards",Bank::client_instructions(&f["partialRewards"]),false);
    assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).rewards_usdc,5*UNIT-10000);
    h.send("buy remaining PERPS rewards",Bank::client_instructions(&f["rewards"]),false);
    let r:PerpsRevenue=get(&h.svm,ledger);assert_eq!(r.rewards_usdc,0);assert!(r.perps_received>0);
    assert_eq!(token(&h.svm,source),before-5*UNIT);
    assert_eq!(get::<OwnershipPool>(&h.svm,h.ownership).reward_pool,r.perps_received);
    let program=key(&f["reward"],"tokenProgram");let owner=h.signer.pubkey();
    let wallet=Pubkey::find_program_address(&[owner.as_ref(),program.as_ref(),MINT.as_ref()],&anchor_spl::associated_token::ID).0;
    token_account(&mut h.svm,wallet,owner,MINT,program,0);
    // A newly queued winner pauses conversion, but not already earned rewards.
    let mut q:Pool=get(&h.svm,h.pool);q.queue_usd=1;q.queue_tail+=1;put(&mut h.svm,h.pool,&q,p::ID);
    h.send("FIFO blocks operations conversion",Bank::client_instructions(&f["operations"]),true);
    h.send("claim purchased PERPS while FIFO waits",Bank::client_instructions(&f["claim"]),false);
    assert_eq!(token(&h.svm,wallet),r.perps_received);
    h.send("PERPS claim cannot replay",Bank::client_instructions(&f["claim"]),true);
    q.queue_usd=0;q.queue_head=q.queue_tail;put(&mut h.svm,h.pool,&q,p::ID);
    for recipient in [OPERATIONS,OPERATIONS_SECONDARY] {
        let mut bad=Bank::client_instructions(&f["operations"]);
        bad[0].accounts.iter_mut().find(|a|a.pubkey==recipient).unwrap().pubkey=owner;
        h.send("neither operations destination can be substituted",bad,true);
        assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).operations_usdc,2*UNIT);
        assert_eq!(token(&h.svm,source),before-5*UNIT);
    }
    let ops=h.svm.get_balance(&OPERATIONS).unwrap_or(0);
    let ops_secondary=h.svm.get_balance(&OPERATIONS_SECONDARY).unwrap_or(0);
    let output=pda(&[b"perps-output",h.pool.as_ref(),SOL.as_ref()]);
    // Anyone can send SOL to a deterministic address. It must not permanently
    // block the operations route or become publisher revenue.
    let donation=3_000_000;
    h.svm.airdrop(&output,donation).unwrap();
    let donated_tokens=donation-h.svm.minimum_balance_for_rent_exemption(165);
    h.send("convert operations USDC and pay native SOL",Bank::client_instructions(&f["operations"]),false);
    let r:PerpsRevenue=get(&h.svm,ledger);assert_eq!(r.operations_usdc,0);assert_eq!(r.operations_spent,2*UNIT);
    assert!(r.operations_sol>0);let total=r.operations_sol+donated_tokens;
    assert_eq!(h.svm.get_balance(&OPERATIONS).unwrap()-ops,total/2);
    assert_eq!(h.svm.get_balance(&OPERATIONS_SECONDARY).unwrap()-ops_secondary,total-total/2);
    assert_eq!(token(&h.svm,source),before-7*UNIT);
    assert!(h.svm.get_account(&output).is_none_or(|a|a.lamports==0));
    h.send("operations conversion cannot replay",Bank::client_instructions(&f["operations"]),true);
    h.send("PERPS conversion cannot replay",Bank::client_instructions(&f["rewards"]),true);
}
#[test]
fn perps_operations_split_both_tiers_partial_delivery_and_rounding() {
    for high in [false,true] {for odd_donation in [false,true] {
        let(mut h,f,_)=setup("PERPS");if high {seed(&mut h,3_000_000*UNIT);}
        loss(&mut h,&f,20);
        h.svm.airdrop(&p::mainnet_config::PUBLISHER,1_000_000_000).unwrap();
        let ledger=ledger(&h);let source=h.tokens[0].vault;
        let pending=get::<PerpsRevenue>(&h.svm,ledger).operations_usdc;
        assert_eq!(pending,if high {20*UNIT}else{2*UNIT});
        let mut sent=0;
        for (name,spent) in [("partialOperations",UNIT),("operations",pending-UNIT)] {
            let first=h.svm.get_balance(&OPERATIONS).unwrap_or(0);
            let second=h.svm.get_balance(&OPERATIONS_SECONDARY).unwrap_or(0);
            let previous=get::<PerpsRevenue>(&h.svm,ledger).operations_sol;
            let source_before=token(&h.svm,source);
            let mut bad=Bank::client_instructions(&f[name]);let n=bad[0].data.len();
            bad[0].data[n-9..n-1].copy_from_slice(&u64::MAX.to_le_bytes());
            h.send("failed operations swap pays neither recipient",bad,true);
            assert_eq!(h.svm.get_balance(&OPERATIONS).unwrap_or(0),first);
            assert_eq!(h.svm.get_balance(&OPERATIONS_SECONDARY).unwrap_or(0),second);
            assert_eq!(token(&h.svm,source),source_before);
            assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).operations_usdc,pending-sent);
            // Two identical runs differing by one donated lamport exercise
            // both parities of the final proceeds without predicting swap fees.
            let donated=1_000_000+u64::from(odd_donation);
            let output=pda(&[b"perps-output",h.pool.as_ref(),SOL.as_ref()]);
            h.svm.airdrop(&output,h.svm.minimum_balance_for_rent_exemption(165)+donated).unwrap();
            h.send("split operations SOL equally after conversion",Bank::client_instructions(&f[name]),false);
            let r:PerpsRevenue=get(&h.svm,ledger);let received=r.operations_sol-previous;
            let a=h.svm.get_balance(&OPERATIONS).unwrap()-first;
            let b=h.svm.get_balance(&OPERATIONS_SECONDARY).unwrap()-second;
            assert!(received>0);assert_eq!(a+b,received+donated);
            assert!(b==a||b==a+1);assert_eq!(a,(received+donated)/2);
            sent+=spent;assert_eq!(r.operations_spent,sent);assert_eq!(r.operations_usdc,pending-sent);
            assert_eq!(source_before-token(&h.svm,source),spent);
        }
        h.send("fully delivered operations cannot replay",Bank::client_instructions(&f["operations"]),true);
    }}
}
#[test]
fn perps_surplus_requires_stakers_and_clear_fifo_and_never_funds_operations() {
    let(mut h,f,_)=setup("PERPS");loss(&mut h,&f,20);seed(&mut h,5_000_001*UNIT);
    let ledger=ledger(&h);
    h.send("surplus without stakers stays in USDC pool",Bank::client_instructions(&f["sweep"]),false);
    assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).rewards_usdc,5*UNIT);
    h.send("stake pPERPS for surplus",Bank::client_instructions(&f["stake"]),false);
    let mut q:Pool=get(&h.svm,h.pool);q.queue_usd=1;q.queue_tail=1;put(&mut h.svm,h.pool,&q,p::ID);
    h.send("queued FIFO prevents surplus allocation",Bank::client_instructions(&f["sweep"]),false);
    assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).rewards_usdc,5*UNIT);
    q.queue_usd=0;q.queue_head=q.queue_tail;put(&mut h.svm,h.pool,&q,p::ID);
    h.send("surplus reserved for PERPS purchase",Bank::client_instructions(&f["sweep"]),false);
    let r:PerpsRevenue=get(&h.svm,ledger);assert_eq!(r.rewards_usdc,6*UNIT);assert_eq!(r.operations_usdc,2*UNIT);assert_eq!(r.burn_usdc,UNIT);
    assert_eq!(get::<Inventory>(&h.svm,h.tokens[0].inventory).treasury,5_000_000*UNIT);
}
#[test]
fn perps_buy_and_burn_preserves_staker_tokens_across_tiers_fifo_failures_and_retries() {
    let(mut h,f,_)=setup("PERPS");loss(&mut h,&f,20);
    h.svm.airdrop(&p::mainnet_config::PUBLISHER,1_000_000_000).unwrap();
    h.send("stake pPERPS before separate purchase and burn",Bank::client_instructions(&f["stake"]),false);
    h.send("purchase PERPS owed to stakers",Bank::client_instructions(&f["rewards"]),false);
    let output=pda(&[b"perps-output",h.pool.as_ref(),MINT.as_ref()]);
    let protected=token(&h.svm,output);assert!(protected>0);
    let resize=|h:&mut Bank,amount:u64| {
        let usd=h.tokens[0];let mut inv:Inventory=get(&h.svm,usd.inventory);
        let held=token(&h.svm,usd.vault)-inv.treasury;
        inv.treasury=amount;put(&mut h.svm,usd.inventory,&inv,p::ID);
        token_account(&mut h.svm,usd.vault,h.authority,usd.mint,usd.program,amount+held);
    };
    resize(&mut h,3_000_000*UNIT);loss(&mut h,&f,24);resize(&mut h,0);
    let ledger=ledger(&h);let before:PerpsRevenue=get(&h.svm,ledger);assert_eq!(before.burn_usdc,6*UNIT);
    assert_eq!(before.rewards_usdc,50*UNIT);assert_eq!(before.operations_usdc,22*UNIT);
    let source=h.tokens[0].vault;let usdc_before=token(&h.svm,source);let supply_before=supply(&h.svm,MINT);
    let mut q:Pool=get(&h.svm,h.pool);q.queue_usd=1;q.queue_tail=1;put(&mut h.svm,h.pool,&q,p::ID);
    h.send("FIFO blocks already reserved PERPS burns",Bank::client_instructions(&f["burn"]),true);
    q.queue_usd=0;q.queue_head=1;put(&mut h.svm,h.pool,&q,p::ID);
    let mut bad=Bank::client_instructions(&f["burn"]);let n=bad[0].data.len();
    bad[0].data[n-9..n-1].copy_from_slice(&u64::MAX.to_le_bytes());
    h.send("failed burn swap rolls back reservation and mint supply",bad,true);
    assert_eq!(get::<PerpsRevenue>(&h.svm,ledger).burn_usdc,6*UNIT);
    assert_eq!(supply(&h.svm,MINT),supply_before);assert_eq!(token(&h.svm,source),usdc_before);
    h.send("partial PERPS purchase and actual token burn",Bank::client_instructions(&f["partialBurn"]),false);
    let partial:PerpsRevenue=get(&h.svm,ledger);assert_eq!(partial.burn_usdc,6*UNIT-10000);assert!(partial.perps_burned>0);
    h.send("remaining burn uses original allocations after tier change",Bank::client_instructions(&f["burn"]),false);
    let after:PerpsRevenue=get(&h.svm,ledger);assert_eq!(after.burn_usdc,0);assert_eq!(after.burn_spent,6*UNIT);
    assert_eq!(supply_before-supply(&h.svm,MINT),after.perps_burned);
    assert_eq!(token(&h.svm,output),protected);assert_eq!(get::<OwnershipPool>(&h.svm,h.ownership).reward_pool,protected);
    assert_eq!(token(&h.svm,source),usdc_before-6*UNIT);
    assert_eq!(after.rewards_usdc,before.rewards_usdc);assert_eq!(after.operations_usdc,before.operations_usdc);
    assert_eq!(after.perps_received,before.perps_received);
    h.send("completed PERPS burn cannot replay",Bank::client_instructions(&f["burn"]),true);
}
#[test]
fn perps_market_winning_claims_still_pay_usdc() {
    let(mut h,f,_)=setup("PERPS");seed(&mut h,100*UNIT);let usd=h.tokens[0];
    h.send("fund USDC winning bet",vec![h.deposit(usd,10*UNIT)],false);
    h.committed(usd,20,10*UNIT,false,true);h.send("settle USDC winner",settlement(&f,20),false);
    let before=token(&h.svm,usd.wallet);h.send("pay winner in USDC",Bank::client_instructions(&f["payout"]),false);
    assert!(token(&h.svm,usd.wallet)>before);assert_eq!(get::<PerpsRevenue>(&h.svm,ledger(&h)).perps_received,0);
}
