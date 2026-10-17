//! Full atomic position settlement with USDC proceeds. Actual registration,
//! quote initialization and platform/DEX SBF; ER commit transport is injected.
use super::*;

#[test]
fn usdc_registration_fee_covers_delegation_and_complete_lookup_reserve() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file=root.join(".cache/production-remediation-candidate/usdc-settlement.json");
    let fixture:Value=serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    let dlp:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-50-slot-dlp-fixture.json")).unwrap()).unwrap();
    let binary=root.join(dlp["binary"].as_str().unwrap());assert_eq!(hash(&binary),dlp["sha256"].as_str().unwrap());
    let mut receipts=vec![];
    for row in fixture["cases"].as_array().unwrap() {
        let mut h=Bank::new();let admin=p::mainnet_config::ADMIN;
        let raw=STANDARD.decode(row["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=anchor_lang::prelude::Rent{lamports_per_byte_year:u64::from_le_bytes(raw[..8].try_into().unwrap()),
            exemption_threshold:f64::from_le_bytes(raw[8..16].try_into().unwrap()),burn_percent:raw[16]};h.svm.set_sysvar(&rent);
        for(address,r)in row["accounts"].as_object().unwrap(){if !address.starts_with("Sysvar") {
            h.svm.set_account(pk(address),Account{lamports:r["lamports"].as_u64().unwrap(),data:STANDARD.decode(r["data"].as_str().unwrap()).unwrap(),
                owner:key(r,"owner"),executable:false,rent_epoch:0}).unwrap();
        }}
        h.svm.add_program(p::basebridge::DELEGATION_OWNER,&std::fs::read(&binary).unwrap()).unwrap();
        let paid=Bank::client_instructions(&row["paidRegistration"]);
        let mut bad=paid.clone();
        // Wrong registered source fails after the fee instruction; all 0.05 SOL
        // must roll back, along with every newly created setup account.
        bad[2].data[40..72].fill(0);
        h.admin("invalid registration rolls back the complete payment",bad,true);
        let before=h.svm.get_account(&admin).unwrap().lamports;
        h.admin("paid USDC registration delegates quote and history",paid,false);
        let after=h.svm.get_account(&admin).unwrap().lamports;
        let setup_cost=50_000_000+before-after;
        let lookup=row["lookupReserveAtCapturedRent"].as_u64().unwrap();
        assert!(setup_cost+lookup<=50_000_000,"{} setup {setup_cost} + lookup {lookup} exceeds user payment",row["name"]);
        let asset=pda(&[b"asset",key(&row["asset"],"mint").as_ref()]);
        for seed in [b"er-quote".as_slice(),b"er-collateral-history".as_slice()] {
            let account=pda(&[seed,asset.as_ref()]);
            assert_eq!(h.svm.get_account(&account).unwrap().owner,p::basebridge::DELEGATION_OWNER);
            let record=Pubkey::find_program_address(&[b"delegation",account.as_ref()],&p::basebridge::DELEGATION_OWNER).0;
            assert_eq!(&h.svm.get_account(&record).unwrap().data[8..40],ER_VALIDATOR.as_ref());
        }
        receipts.push(json!({"name":row["name"],"setupLamports":setup_cost,"lookupReserveLamports":lookup,
            "remainingLamports":50_000_000-setup_cost-lookup,"registrationBytes":row["registrationBytes"],"operations":h.receipts}));
    }
    std::fs::write(root.join("reports/acidpump-usdc-registration.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-paid-registration","candidateSha256":hash(root.join(candidate_binary())),"fixtureSha256":hash(file),
        "dlpSha256":hash(binary),"transactionsSubmitted":0,"scope":"Actual client payment and seven setup instructions with captured DLP SBF; signatures simulated; no asynchronous validator transport", "receipts":receipts})).unwrap()).unwrap();
}

#[test]
fn usdc_collateral_settles_atomically_without_spending_other_usdc() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file=root.join(".cache/production-remediation-candidate/usdc-settlement.json");
    let fixture:Value=serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(key(&fixture,"program"),p::ID);
    assert_eq!(fixture["sourceSha256"].as_str().unwrap(),hash(root.join(fixture["source"].as_str().unwrap())));
    assert_eq!(fixture["poolSourceSha256"].as_str().unwrap(),hash(root.join(fixture["poolSource"].as_str().unwrap())));
    let pump_report:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-pump-curve-binary.json")).unwrap()).unwrap();
    let pump_file=root.join(pump_report["file"].as_str().unwrap());assert_eq!(hash(&pump_file),pump_report["sha256"].as_str().unwrap());
    let mut receipts=vec![];
    for row in fixture["cases"].as_array().unwrap() {
      for outcome in ["partial","liquidated","winner","drained"] {
        println!("USDC settlement {} {outcome}",row["name"]);
        let mut h=Bank::new();let owner=h.signer.pubkey();let primary=h.tokens[0];
        let raw=STANDARD.decode(row["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=anchor_lang::prelude::Rent{lamports_per_byte_year:u64::from_le_bytes(raw[..8].try_into().unwrap()),
            exemption_threshold:f64::from_le_bytes(raw[8..16].try_into().unwrap()),burn_percent:raw[16]};h.svm.set_sysvar(&rent);
        for(address,r)in row["accounts"].as_object().unwrap(){if !address.starts_with("Sysvar") {
            h.svm.set_account(pk(address),Account{lamports:r["lamports"].as_u64().unwrap(),data:STANDARD.decode(r["data"].as_str().unwrap()).unwrap(),
                owner:key(r,"owner"),executable:false,rent_epoch:0}).unwrap();
        }}
        h.svm.add_program(p::mainnet_curve::ID,&std::fs::read(&pump_file).unwrap()).unwrap();
        let asset=&row["asset"];let mint=key(asset,"mint");let program=key(asset,"tokenProgram");let dex=key(asset,"dexPool");
        let asset_key=pda(&[b"asset",mint.as_ref()]);let feed=pda(&[b"feed",&140u16.to_le_bytes()]);
        h.admin("register USDC quoted collateral and initialize ER quote",Bank::client_instructions(&row["registration"]),false);
        let stored:Asset=get(&h.svm,asset_key);
        assert_eq!((stored.mint,stored.quote_mint,stored.dex_pool,stored.source_pool),(mint,USDC,dex,dex));
        assert_eq!((stored.token_program,stored.decimals),(program,asset["decimals"].as_u64().unwrap() as u8));
        let er_quote:p::engine::ErQuote=get(&h.svm,pda(&[b"er-quote",asset_key.as_ref()]));
        assert_eq!(er_quote.asset,asset_key);assert!(!er_quote.is_quote,"USDC-quoted collateral is not fixed-value USDC");
        let wallet=Pubkey::find_program_address(&[owner.as_ref(),program.as_ref(),mint.as_ref()],&anchor_spl::associated_token::ID).0;
        let a=Token{asset:asset_key,mint,program,feed,pool:dex,wallet,vault:pda(&[b"vault",h.pool.as_ref(),asset_key.as_ref()]),
            inventory:pda(&[b"inventory",h.pool.as_ref(),asset_key.as_ref()])};
        let margin=1_000_000*UNIT;let staked=500_000*UNIT;
        token_account(&mut h.svm,wallet,owner,mint,program,margin);
        h.send("deposit USDC quoted collateral and backing",vec![h.deposit(a,margin),h.deposit(primary,staked)],false);
        let before:ErCustody=get(&h.svm,h.custody);
        let winning=outcome=="winner";let liquidated=outcome=="liquidated"||outcome=="drained";
        let intent=h.committed(a,40,margin,liquidated,winning);
        let mut trader:ErTrader=get(&h.svm,h.trader);trader.core_stake.tokens=staked;trader.custody_generation=before.generation;trader.notional=intent.entry_value*intent.leverage as u64;
        for(target,source)in trader.assets.iter_mut().zip(before.assets.iter()) {
            *target=ErBalance{asset:source.asset,credited_deposits:source.deposited_credits,credited_returns:source.returned_credits,available:0,generation:source.generation};
        }
        put(&mut h.svm,h.trader,&trader,p::basebridge::DELEGATION_OWNER);
        if outcome=="drained" {
            let mut reserve=h.svm.get_account(&dex).unwrap();
            if row["kind"]=="curve" {reserve.data[32..40].fill(0);} else {
                let vault=Pubkey::new_from_array(reserve.data[171..203].try_into().unwrap());
                let mut quote=h.svm.get_account(&vault).unwrap();quote.data[64..72].fill(0);h.svm.set_account(vault,quote).unwrap();
                reserve.data[245..253].fill(0);
            }
            h.svm.set_account(dex,reserve).unwrap();
        }
        let intermediate=key(row,"intermediate");let dust=5_000_007;
        token_account(&mut h.svm,intermediate,h.authority,USDC,anchor_spl::token::ID,dust);
        let sol0=token(&h.svm,h.sol_vault);let custody_usd0=token(&h.svm,h.usd_vault);
        let payer=Keypair::new_from_array([121;32]);h.svm.airdrop(&payer.pubkey(),2_000_000_000).unwrap();h.signer=payer;
        let packets:Vec<_>=row["packets"].as_array().unwrap().iter().map(Bank::client_instructions).collect();
        let position=pda(&[b"position",owner.as_ref(),&40u64.to_le_bytes()]);let final_packet=packets.last().unwrap().clone();
        for (index,packet) in packets.iter().enumerate() {
            if index+1==packets.len()&&outcome=="partial" {
                let mut failed=packet.clone();failed.last_mut().unwrap().data[20..28].copy_from_slice(&u64::MAX.to_le_bytes());
                h.send(if row["kind"]=="curve" {"failed curve final buy rolls back settlement"}else{"failed final swap rolls back complete settlement"},failed,true);
                assert_eq!(token(&h.svm,intermediate),dust);assert_eq!(token(&h.svm,a.vault),margin);
                let mut wrong=packet.clone();let close=wrong.last_mut().unwrap();
                let conversion_index=close.accounts.iter().rposition(|m|m.pubkey==intermediate).unwrap();
                // Reuse an already-locked USDC reserve so the deliberately
                // invalid transaction reaches the adapter even on the 64-key
                // cashback route. Introducing a 65th key is rejected earlier.
                let payout=h.svm.get_account(&p::pump::USDC_POOL).unwrap();
                close.accounts[conversion_index].pubkey=Pubkey::new_from_array(payout.data[139..171].try_into().unwrap());
                h.send("USDC conversion cannot substitute another vault",wrong,true);
            }
            h.send("settle USDC collateral through canonical conversion",packet.clone(),false);
            if index+1<packets.len(){assert_eq!(token(&h.svm,a.vault),margin);assert_eq!(get::<Position>(&h.svm,position).state,0);}
        }
        let x:Position=get(&h.svm,position);let after:ErCustody=get(&h.svm,h.custody);
        let loss=if winning{0}else if liquidated{margin}else{margin/10};
        assert_eq!((x.loss,x.returned),(loss,margin-loss));assert_eq!(after.assets[after.asset_index(a.asset).unwrap()].available,margin-loss);
        assert_eq!(token(&h.svm,intermediate),dust,"unrelated intermediate USDC must remain untouched");
        assert_eq!(token(&h.svm,h.usd_vault),custody_usd0,"USDC claims must not fund the swap");
        assert!(token(&h.svm,h.sol_vault)>=sol0);assert_eq!(token(&h.svm,a.vault),margin-if outcome=="drained"{0}else{loss});
        let reward:p::OwnershipReceipt=get(&h.svm,pda(&[b"ownership-receipt",position.as_ref()]));
        assert!(reward.awarded);assert_eq!(reward.pool_tokens,x.swapped_out+x.coverage_tokens);assert_eq!(reward.issued,reward.pool_tokens as u128*100);
        if winning {assert!(x.profit>0);assert_eq!((x.swapped_out,x.coverage_tokens,x.uncovered_usd),(0,0,0));}
        else if outcome=="drained" {assert_eq!((x.collateral_close_value,x.swapped_out),(0,0));assert!(x.coverage_tokens>0);}
        else {assert!(x.swapped_out>0);}
        h.send("USDC collateral settlement cannot replay",final_packet,true);
        let mut risk=ErRisk{config:h.config,..ErRisk::default()};risk.markets[intent.market as usize].long_oi=trader.notional;
        sync_custody(&mut trader,&mut risk,&after).unwrap();sync_custody(&mut trader,&mut risk,&after).unwrap();
        assert_eq!(trader.slots[0].state,ER_FREE);assert_eq!(trader.core_stake.tokens,staked-x.coverage_tokens);
        receipts.push(json!({"name":row["name"],"outcome":outcome,"loss":x.loss,"returned":x.returned,"swappedOut":x.swapped_out,
            "coverageTokens":x.coverage_tokens,"ownership":reward.issued.to_string(),"packets":packets.len(),"operations":h.receipts}));
      }
    }
    assert_eq!(fixture["cases"].as_array().unwrap().len(),4);
    assert_eq!(receipts.len(),16);
    std::fs::write(root.join("reports/acidpump-usdc-settlement.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-full-base-settlement","candidateSha256":hash(root.join(candidate_binary())),"fixtureSha256":hash(file),
        "transactionsSubmitted":0,"scope":"Actual client registration, quote initialization and platform/PumpSwap SBF with graduated USDC pools; ER commit boundary injected; delegation/live rollout pending","receipts":receipts})).unwrap()).unwrap();
}
