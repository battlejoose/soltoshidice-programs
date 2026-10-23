use super::*;

fn setup(symbol:&str)->(Bank,Value,Token) {
    let file=std::env::var("ACIDPUMP_NATIVE_TEST_FIXTURE").unwrap_or_else(|_|".cache/mixed-backing/native-instructions.json".into());
    let rows:Value=serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(file)).unwrap()).unwrap();
    let f=rows["markets"].as_array().unwrap().iter().find(|m|m["asset"]["symbol"]==symbol).unwrap().clone();
    let mut h=Bank::new();let root=h.tokens[0];let own=if symbol=="SOL"{h.tokens[2]}else{h.tokens[1]};
    h.admin("create canonical native market",Bank::client_instructions(&f["create"]),false);
    for step in f["setup"].as_array().unwrap(){h.admin(step["name"].as_str().unwrap(),Bank::client_instructions(&step["instructions"]),false);}
    let config=key(&f["venue"],"config");h.config=config;h.pool=key(&f["venue"],"pool");h.authority=pda(&[b"authority",h.pool.as_ref()]);
    h.svm.airdrop(&h.authority,100_000_000).unwrap();h.market=pda(&[b"market",config.as_ref(),h.market_feed.as_ref()]);
    h.sol_vault=pda(&[b"sol-quote",h.pool.as_ref()]);h.usd_vault=pda(&[b"quote",h.pool.as_ref()]);
    h.gas_vault=pda(&[b"gas-vault",h.pool.as_ref()]);h.ownership=pda(&[b"ownership-pool",h.pool.as_ref()]);
    let owner=h.signer.pubkey();h.custody=pda(&[b"er-custody",config.as_ref(),owner.as_ref()]);h.trader=pda(&[b"er-trader",config.as_ref(),owner.as_ref()]);
    h.guard=pda(&[b"er-collateral-guard",config.as_ref(),owner.as_ref()]);
    h.tokens=[own,h.tokens[1],h.tokens[2]].map(|a|Token{inventory:pda(&[b"inventory",h.pool.as_ref(),a.asset.as_ref()]),vault:pda(&[b"vault",h.pool.as_ref(),a.asset.as_ref()]),..a});
    h.send("initialize native custody",Bank::client_instructions(&f["initialize"]),false);
    h.admin("enable initialized native market",vec![ix(p::accounts::Admin{admin:p::mainnet_config::ADMIN,config},p::instruction::Pause{paused:false})],false);
    (h,f,root)
}
fn settlement(f:&Value,nonce:u64)->Vec<Instruction> {
    Bank::client_instructions(&f["settlements"].as_array().unwrap().iter().find(|r|r["nonce"].as_u64()==Some(nonce)).unwrap()["instructions"])
}
fn seed(h:&mut Bank,amount:u64) {
    let a=h.tokens[0];let mut inv:Inventory=get(&h.svm,a.inventory);inv.treasury=amount;
    token_account(&mut h.svm,a.vault,h.authority,a.mint,a.program,inv.locked+amount);put(&mut h.svm,a.inventory,&inv,p::ID);
}
#[test]
fn native_markets_fund_sdice_stakers_and_burn_sdice_above_two_million() {
    for symbol in ["SOL","USDC"] {for high in [false,true] {for queued in [false,true] {
        let(mut h,f,root)=setup(symbol);let own=h.tokens[0];let amount=if symbol=="SOL"{1_000_000_000}else{100*UNIT};
        if high {seed(&mut h,if symbol=="SOL"{1_000_000_000_000_000}else{3_000_000*UNIT});}
        if queued {let mut pool:Pool=get(&h.svm,h.pool);pool.queue_usd=1;pool.queue_tail=1;put(&mut h.svm,h.pool,&pool,p::ID);}
        h.send("fund native loss",vec![h.deposit(own,amount)],false);
        let treasury=get::<Inventory>(&h.svm,own.inventory).treasury;
        let mint_supply=supply(&h.svm,own.mint);let root_supply=supply(&h.svm,root.mint);
        h.committed(own,20,amount,true,false);h.send("settle native-market loss",settlement(&f,20),false);
        let expected=if queued{99}else if high{10}else{90};
        assert_eq!(get::<Inventory>(&h.svm,own.inventory).treasury-treasury,amount*expected/100);
        let rewards:OwnershipPool=get(&h.svm,h.ownership);
        assert_eq!(rewards.reward_pool+rewards.fee_pending,amount*if queued{0}else if high{70}else{7}/100);
        assert_eq!(supply(&h.svm,own.mint),mint_supply);assert_eq!(supply(&h.svm,root.mint),root_supply);
        let venue:p::Venue=get(&h.svm,key(&f["venue"],"venue"));assert_eq!(venue.burn_bps,0);assert_eq!(venue.buyback_bps,200);
        if queued {assert_eq!(venue.buyback_received,0);}else {
            assert!(venue.buyback_received>0);let before=get::<Inventory>(&h.svm,root.inventory).treasury;
            let ledger=pda(&[b"native-revenue",h.pool.as_ref()]);
            let allocation:p::NativeRevenue=get(&h.svm,ledger);
            assert_eq!(allocation.rewards_quote+allocation.burn_quote,venue.buyback_received);
            assert_eq!(allocation.burn_quote,if high {venue.buyback_received*9/19}else{0});
            let root_ownership=pda(&[b"ownership-pool",p::ROOT_POOL.as_ref()]);
            let prior:OwnershipPool=get(&h.svm,root_ownership);
            let mut held=Bank::client_instructions(&f["buyback"]);
            // An incorrect reward recipient must roll back even the token buy.
            for account in &mut held[0].accounts {if account.pubkey==root_ownership {account.pubkey=h.ownership;}}
            let quote=token(&h.svm,pda(&[b"buyback",h.pool.as_ref()]));
            h.send("reject rewards redirected to source market",held,true);
            assert_eq!(token(&h.svm,pda(&[b"buyback",h.pool.as_ref()])),quote);
            h.send("native fee share rewards SDICE stakers and burns SDICE",Bank::client_instructions(&f["buyback"]),false);
            let after:p::NativeRevenue=get(&h.svm,ledger);
            assert_eq!(after.rewards_quote+after.burn_quote,0);
            let reward:OwnershipPool=get(&h.svm,root_ownership);
            assert_eq!(reward.fee_pending+reward.reward_pool-prior.fee_pending-prior.reward_pool,after.sdice_rewarded);
            assert!(after.sdice_rewarded>0);
            assert_eq!(get::<Inventory>(&h.svm,root.inventory).treasury,before);
            assert_eq!(root_supply-supply(&h.svm,root.mint),after.sdice_burned);
            assert_eq!(after.sdice_burned>0,high);
            assert_eq!(get::<p::Venue>(&h.svm,key(&f["venue"],"venue")).sdice_burned,after.sdice_burned);
            h.send("native distribution cannot replay",Bank::client_instructions(&f["buyback"]),true);
        }
    }}}
}

#[test]
fn native_sdice_allocation_survives_tier_changes_and_partial_delivery() {
    for symbol in ["SOL","USDC"] {
        let(mut h,f,root)=setup(symbol);let own=h.tokens[0];let amount=if symbol=="SOL"{1_000_000_000}else{100*UNIT};
        let ledger=pda(&[b"native-revenue",h.pool.as_ref()]);
        for (nonce,high) in [(20,false),(24,true)] {
            if high {seed(&mut h,if symbol=="SOL"{1_000_000_000_000_000}else{3_000_000*UNIT});}
            h.send("fund tiered loss",vec![h.deposit(own,amount)],false);
            h.committed(own,nonce,amount,true,false);h.send("settle tiered loss",settlement(&f,nonce),false);
        }
        let before:p::NativeRevenue=get(&h.svm,ledger);
        assert!(before.rewards_quote>before.burn_quote && before.burn_quote>0);
        // Current source value no longer determines what was promised previously.
        seed(&mut h,0);
        let treasury=get::<Inventory>(&h.svm,root.inventory).treasury;
        h.send("partial distribution",Bank::client_instructions(&f["partialBuyback"]),false);
        let partial:p::NativeRevenue=get(&h.svm,ledger);
        assert_eq!(before.rewards_quote+before.burn_quote-partial.rewards_quote-partial.burn_quote,1_000_000);
        assert!(partial.sdice_rewarded>0 && partial.sdice_burned>0);
        h.send("remaining distribution",Bank::client_instructions(&f["buyback"]),false);
        let after:p::NativeRevenue=get(&h.svm,ledger);
        assert_eq!(after.rewards_quote+after.burn_quote,0);
        assert!(after.sdice_rewarded>partial.sdice_rewarded && after.sdice_burned>partial.sdice_burned);
        assert_eq!(get::<Inventory>(&h.svm,root.inventory).treasury,treasury);
    }
}
#[test]
fn native_market_profit_claims_pay_primary_asset_at_spot() {
    for symbol in ["SOL","USDC"] {
        let(mut h,f,_)=setup(symbol);let own=h.tokens[0];let amount=if symbol=="SOL"{1_000_000_000}else{10*UNIT};
        seed(&mut h,amount*10);h.send("fund native winning position",vec![h.deposit(own,amount)],false);
        h.committed(own,20,amount,false,true);h.send("record native profit claim",settlement(&f,20),false);
        let position=pda(&[b"position",h.pool.as_ref(),h.signer.pubkey().as_ref(),&20u64.to_le_bytes()]);
        let claim:Position=get(&h.svm,position);assert!(claim.remaining>0);
        let before=token(&h.svm,own.wallet);
        h.send("redeem native primary profit",Bank::client_instructions(&f["payout"]),false);
        assert_eq!(get::<Position>(&h.svm,position).remaining,0);assert!(token(&h.svm,own.wallet)>before);
        h.send("native profit cannot replay",Bank::client_instructions(&f["payout"]),true);
    }
}
