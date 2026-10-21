//! Created venues execute real token CPIs and client-generated instructions.
//! Only the already-tested ER commit boundary is injected in this offline bank.
use super::*;

fn fixture()->Value {
    serde_json::from_slice(&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/perps-economics-modular/venue-instructions.json")).unwrap()).unwrap()
}

#[test]
fn large_venue_uses_10_pool_70_stakers_10_burn_9_sdice_1_sol() {
    let(mut h,f,root_primary)=make_venue();let own=h.tokens[0];
    super::pool_tokenomics::seed_high_pool(&mut h,own);
    h.send("fund high-value venue loss",vec![h.deposit(own,10_000*UNIT)],false);
    let treasury=get::<Inventory>(&h.svm,own.inventory).treasury;
    let own_supply=supply(&h.svm,own.mint);let root_supply=supply(&h.svm,root_primary.mint);
    h.committed(own,20,10_000*UNIT,true,false);
    h.send("settle high-value venue loss",settlement(&f,20),false);
    assert_eq!(get::<Inventory>(&h.svm,own.inventory).treasury-treasury,1_000*UNIT);
    assert_eq!(own_supply-supply(&h.svm,own.mint),1_000*UNIT);
    let rewards:OwnershipPool=get(&h.svm,h.ownership);assert_eq!(rewards.fee_pending+rewards.reward_pool,7_000*UNIT);
    let v:p::Venue=get(&h.svm,key(&f["venue"],"venue"));assert!(v.buyback_received>0);
    let before=token(&h.svm,root_primary.vault);
    h.send("contribute high-value venue SDICE purchase to root",Bank::client_instructions(&f["buyback"]),false);
    assert!(token(&h.svm,root_primary.vault)>before);assert_eq!(supply(&h.svm,root_primary.mint),root_supply);
}
fn make_venue()->(Bank,Value,Token) {
    let mut h=Bank::new();let root_primary=h.tokens[0];let own=h.register_backed_collateral("FARTCOIN");let f=fixture();
    let create=Bank::client_instructions(&f["create"]);
    let mut wrong=create.clone();wrong[0].accounts[0].pubkey=h.signer.pubkey();
    h.send("unprivileged venue creation rejected",wrong,true);
    h.admin("create venue and isolated SOL and USDC vaults",create,false);
    let config=key(&f["venue"],"config");assert!(get::<Config>(&h.svm,config).paused);
    for step in f["setup"].as_array().unwrap() {h.admin(step["name"].as_str().unwrap(),Bank::client_instructions(&step["instructions"]),false);}
    h.config=config;h.pool=key(&f["venue"],"pool");h.authority=pda(&[b"authority",h.pool.as_ref()]);
    h.svm.airdrop(&h.authority,100_000_000).unwrap();
    h.market=pda(&[b"market",config.as_ref(),h.market_feed.as_ref()]);
    h.sol_vault=pda(&[b"sol-quote",h.pool.as_ref()]);h.usd_vault=pda(&[b"quote",h.pool.as_ref()]);
    h.gas_vault=pda(&[b"gas-vault",h.pool.as_ref()]);h.ownership=pda(&[b"ownership-pool",h.pool.as_ref()]);
    let owner=h.signer.pubkey();h.custody=pda(&[b"er-custody",config.as_ref(),owner.as_ref()]);
    h.trader=pda(&[b"er-trader",config.as_ref(),owner.as_ref()]);h.guard=pda(&[b"er-collateral-guard",config.as_ref(),owner.as_ref()]);
    h.tokens=[own,h.tokens[1],h.tokens[2]].map(|a|Token{inventory:pda(&[b"inventory",h.pool.as_ref(),a.asset.as_ref()]),vault:pda(&[b"vault",h.pool.as_ref(),a.asset.as_ref()]),..a});
    h.send("initialize venue custody from real client",Bank::client_instructions(&f["initialize"]),false);
    h.send("deposit rejected while paused",vec![h.deposit(h.tokens[0],100*UNIT)],true);
    h.admin("enable completely provisioned venue",vec![ix(p::accounts::Admin{admin:p::mainnet_config::ADMIN,config},p::instruction::Pause{paused:false})],false);
    (h,f,root_primary)
}
fn settlement(f:&Value,nonce:u64)->Vec<Instruction> {
    Bank::client_instructions(&f["settlements"].as_array().unwrap().iter().find(|r|r["nonce"].as_u64()==Some(nonce)).unwrap()["instructions"])
}

#[test]
fn created_venue_burns_own_token_and_credits_bought_sdice_to_root_inventory() {
    let(mut h,f,root_primary)=make_venue();let own=h.tokens[0];
    let root_inventory_before:Inventory=get(&h.svm,root_primary.inventory);
    let root_vault_before=token(&h.svm,root_primary.vault);
    let own_supply=supply(&h.svm,own.mint);let sdice_supply=supply(&h.svm,root_primary.mint);
    let gas_before=get::<GasReserve>(&h.svm,h.gas).sol_received;
    h.send("fund venue with its own token",vec![h.deposit(own,100_000*UNIT)],false);
    h.committed(own,20,10_000*UNIT,true,false);
    h.send("venue normal loss 90 pool 7 stakers 1 own burn 1 gas 1 SDICE buyback",settlement(&f,20),false);
    assert_eq!(own_supply-supply(&h.svm,own.mint),100*UNIT);
    let inventory:Inventory=get(&h.svm,own.inventory);assert_eq!(inventory.treasury,9_000*UNIT);
    let rewards:OwnershipPool=get(&h.svm,h.ownership);assert_eq!(rewards.fee_pending+rewards.reward_pool,700*UNIT);
    let venue_key=key(&f["venue"],"venue");let venue:p::Venue=get(&h.svm,venue_key);
    assert!(venue.buyback_received>0);assert_eq!(venue.sdice_burned,0);
    assert!(get::<GasReserve>(&h.svm,h.gas).sol_received>gas_before);
    h.send("buy SDICE into root payouts with isolated venue proceeds",Bank::client_instructions(&f["buyback"]),false);
    let after:p::Venue=get(&h.svm,venue_key);assert_eq!(after.sdice_burned,0);assert_eq!(after.buyback_spent,venue.buyback_received);
    assert_eq!(supply(&h.svm,root_primary.mint),sdice_supply);
    let added=token(&h.svm,root_primary.vault)-root_vault_before;assert!(added>0);
    let inventory_after:Inventory=get(&h.svm,root_primary.inventory);
    assert_eq!(inventory_after.treasury,root_inventory_before.treasury+added);
    assert_eq!(inventory_after.locked,root_inventory_before.locked);
    h.send("empty contribution cannot be replayed",Bank::client_instructions(&f["buyback"]),true);
}

#[test]
fn venue_queue_suspends_burns_and_staker_fees_but_not_loss_rewards_or_sol_fees() {
    let(mut h,f,root_primary)=make_venue();let own=h.tokens[0];let usd=h.tokens[1];
    h.send("fund venue loss and winner",vec![h.deposit(own,100_000*UNIT),h.deposit(usd,20*UNIT)],false);
    // Accumulate buyback quote before the queue; it too must wait.
    h.committed(own,20,10_000*UNIT,true,false);h.send("normal venue loss",settlement(&f,20),false);
    h.committed(usd,22,10*UNIT,false,true);h.send("queue real venue winning claim",settlement(&f,22),false);
    let claim_key=pda(&[b"position",h.pool.as_ref(),h.signer.pubkey().as_ref(),&22u64.to_le_bytes()]);
    let claim:Position=get(&h.svm,claim_key);assert!(claim.remaining>0);
    let venue_key=key(&f["venue"],"venue");let before:p::Venue=get(&h.svm,venue_key);
    let primary_supply=supply(&h.svm,own.mint);let sdice_supply=supply(&h.svm,root_primary.mint);
    let rewards=h.svm.get_account(&h.ownership).unwrap().data;
    let treasury=get::<Inventory>(&h.svm,own.inventory).treasury;
    let gas=get::<GasReserve>(&h.svm,h.gas).sol_received;
    h.send("pending venue claims defer previously allocated SDICE burn",Bank::client_instructions(&f["buyback"]),true);
    // Same nonce is intentionally new only in this fresh committed fixture.
    // Use the SOL loss row to exercise a second real route without replacing a closed receipt.
    let sol=h.tokens[2];h.send("fund native SOL venue loss",vec![h.deposit(sol,1_000_000_000)],false);
    h.committed(sol,23,100_000_000,true,false);h.send("queued venue loss routes 99 pool and 1 SOL",settlement(&f,23),false);
    let after:p::Venue=get(&h.svm,venue_key);assert_eq!(after.buyback_received,before.buyback_received);
    assert_eq!(supply(&h.svm,own.mint),primary_supply);assert_eq!(supply(&h.svm,root_primary.mint),sdice_supply);
    let after_rewards:OwnershipPool=get(&h.svm,h.ownership);let before_rewards=OwnershipPool::try_deserialize(&mut &rewards[..]).unwrap();
    assert_eq!(after_rewards.fee_pending,before_rewards.fee_pending);assert_eq!(after_rewards.reward_pool,before_rewards.reward_pool);
    assert!(get::<Inventory>(&h.svm,own.inventory).treasury>treasury);assert!(get::<GasReserve>(&h.svm,h.gas).sol_received>gas);
    assert_eq!(get::<Position>(&h.svm,claim_key).remaining,claim.remaining);
    let user_before=token(&h.svm,usd.wallet);
    h.send("venue claim redeems USDC from its own pool",Bank::client_instructions(&f["payout"]),false);
    assert!(token(&h.svm,usd.wallet)>user_before);
}

#[test]
fn venue_cannot_substitute_root_buyback_accounts() {
    let(mut h,f,root_primary)=make_venue();let own=h.tokens[0];
    h.send("fund substitution fixture",vec![h.deposit(own,100_000*UNIT)],false);h.committed(own,20,10_000*UNIT,true,false);
    let mut ixs=settlement(&f,20);let buyback=pda(&[b"buyback",h.pool.as_ref()]);
    for account in &mut ixs.last_mut().unwrap().accounts {if account.pubkey==buyback {account.pubkey=pda(&[b"sol-quote",p::ROOT_POOL.as_ref()]);}}
    h.send("root quote cannot stand in for venue buyback vault",ixs,true);
    assert_eq!(get::<Inventory>(&h.svm,root_primary.inventory).treasury,0);
}

#[test]
fn venue_converts_usdc_sol_and_sdice_collateral_without_mixing_custody() {
    for nonce in [21u64,22,23] {
        let(mut h,f,root_primary)=make_venue();
        let asset=if nonce==21 {Token{inventory:pda(&[b"inventory",h.pool.as_ref(),root_primary.asset.as_ref()]),
            vault:pda(&[b"vault",h.pool.as_ref(),root_primary.asset.as_ref()]),..root_primary}}
            else if nonce==22 {h.tokens[1]}else{h.tokens[2]};
        let amount=if nonce==21 {10_000*UNIT}else if nonce==22 {10*UNIT}else{100_000_000};
        let root_vault=token(&h.svm,root_primary.vault);let treasury=get::<Inventory>(&h.svm,h.tokens[0].inventory).treasury;
        h.send("deposit collateral into created venue only",vec![h.deposit(asset,amount)],false);
        h.committed(asset,nonce,amount,true,false);h.send("convert venue collateral through captured mainnet adapter",settlement(&f,nonce),false);
        assert!(get::<Inventory>(&h.svm,h.tokens[0].inventory).treasury>treasury);
        assert_eq!(token(&h.svm,root_primary.vault),root_vault);
        assert_eq!(get::<Inventory>(&h.svm,asset.inventory).locked,0);
        assert!(get::<p::Venue>(&h.svm,key(&f["venue"],"venue")).buyback_received>0);
    }
}

#[test]
fn root_fifo_accepts_created_venue_sdice_contribution_without_changing_claims() {
    let(mut h,f,root_primary)=make_venue();let own=h.tokens[0];
    h.send("fund root queue buyback fixture",vec![h.deposit(own,100_000*UNIT)],false);
    h.committed(own,20,10_000*UNIT,true,false);h.send("accumulate buyback proceeds",settlement(&f,20),false);
    let mut pool:Pool=get(&h.svm,p::ROOT_POOL);pool.queue_usd=1;pool.queue_tail=1;
    put(&mut h.svm,p::ROOT_POOL,&pool,p::ID);
    let before=token(&h.svm,root_primary.vault);let minted=supply(&h.svm,root_primary.mint);
    h.send("root unpaid claims accept incoming venue contribution",Bank::client_instructions(&f["buyback"]),false);
    assert!(token(&h.svm,root_primary.vault)>before);
    assert_eq!(supply(&h.svm,root_primary.mint),minted);
    let after:Pool=get(&h.svm,p::ROOT_POOL);assert_eq!(after.queue_usd,1);assert_eq!(after.queue_tail,1);
}
