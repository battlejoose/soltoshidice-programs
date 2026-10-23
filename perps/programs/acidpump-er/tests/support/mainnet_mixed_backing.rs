//! Real candidate and PumpSwap binaries, captured pools, no live wagers.
//! Only the delegated ER commit checkpoint is injected by the harness.
use super::*;

#[test]
fn pre_upgrade_open_trade_migrates_then_closes_and_settles_without_changing_terms() {
    for winning in [false,true] {
        let mut h=Bank::new();let usd=h.tokens[1];let owner=h.signer.pubkey();
        let nonce=if winning {4}else{1};let now=h.f["now"].as_i64().unwrap();
        h.send("fund migration trade",vec![h.deposit(usd,10*UNIT)],false);
        let expected=h.committed(usd,nonce,10*UNIT,!winning,winning);
        let mut t:ErTrader=get(&h.svm,h.trader);
        t.slots[0].state=ER_OPEN;t.slots[0].exit=0;t.slots[0].closed_at=0;t.slots[0].close_seq=0;t.slots[0].liquidated=false;
        t.notional=expected.entry_value*expected.leverage as u64;
        let c:ErCustody=get(&h.svm,h.custody);
        t.custody_generation=c.generation;
        for (dst,src) in t.assets.iter_mut().zip(c.assets.iter()) {
            *dst=ErBalance{asset:src.asset,available:src.available,credited_deposits:src.deposited_credits,
                credited_returns:src.returned_credits,generation:src.generation};
            if dst.asset==usd.asset {dst.available-=10*UNIT;}
        }
        let mut g:CollateralGuard=get(&h.svm,h.guard);g.floors[0].closed_at=0;g.floors[0].exit=0;
        put(&mut h.svm,h.trader,&t,p::ID);put(&mut h.svm,h.guard,&g,p::ID);
        let original=h.svm.get_account(&h.trader).unwrap().data;
        // Model the completed canonical DLP return of deployed 10-slot bytes.
        // The captured DLP CPI and transport are exercised separately.
        for (key,extra) in [(h.trader,64),(h.custody,48)] {
            let mut a=h.svm.get_account(&key).unwrap();a.data.truncate(a.data.len()-extra);
            a.lamports=h.svm.minimum_balance_for_rent_exemption(a.data.len());h.svm.set_account(key,a).unwrap();
        }
        h.admin("pause for append-only migration",vec![ix(p::accounts::Admin{admin:p::mainnet_config::ADMIN,config:h.config},p::instruction::Pause{paused:true})],false);
        h.admin("migrate with original position still open",vec![ix(p::accounts::MigrateWalletCapacity{
            admin:p::mainnet_config::ADMIN,payer:p::mainnet_config::ADMIN,config:h.config,owner,er_trader:h.trader,
            custody:h.custody,guard:h.guard,system_program:Pubkey::default(),
            execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},p::instruction::MigrateWalletCapacity{})],false);
        assert_eq!(h.svm.get_account(&h.trader).unwrap().data,original);
        assert_eq!(get::<ErCustody>(&h.svm,h.custody).generation,c.generation);
        h.admin("resume preserved position",vec![ix(p::accounts::Admin{admin:p::mainnet_config::ADMIN,config:h.config},p::instruction::Pause{paused:false})],false);
        t=get(&h.svm,h.trader);
        let mut risk=ErRisk{config:h.config,next_close_seq:get::<Config>(&h.svm,h.config).next_settlement_seq,..ErRisk::default()};
        risk.markets[2].price=expected.exit;risk.markets[2].publish_time=now;risk.markets[2].sequence=1;
        risk.markets[2].long_oi=t.notional;
        let quote=ErQuote{config:h.config,asset:usd.asset,..ErQuote::default()};
        close_slot(&mut t,&mut risk,&quote,0,nonce,now+60,owner,now).unwrap();
        assert_eq!(t.slots[0].entry,expected.entry);assert_eq!(t.slots[0].bust,expected.bust);
        assert_eq!(t.slots[0].opened_at,expected.opened_at);assert_eq!(t.slots[0].liquidated,!winning);
        g.floors[0].closed_at=now;g.floors[0].exit=t.slots[0].exit;
        put(&mut h.svm,h.trader,&t,p::basebridge::DELEGATION_OWNER);
        put(&mut h.svm,h.guard,&g,p::basebridge::DELEGATION_OWNER);
        let settle=h.settlement(usd,t.slots[0],1);
        h.send("settle preserved pre-upgrade position",settle.clone(),false);
        let after:ErCustody=get(&h.svm,h.custody);assert!(after.receipts[0].settled);
        sync_custody(&mut t,&mut risk,&after).unwrap();assert_eq!(t.slots[0].state,ER_FREE);assert_eq!(t.notional,0);
        let available=t.assets.iter().map(|a|a.available).collect::<Vec<_>>();
        sync_custody(&mut t,&mut risk,&after).unwrap();assert_eq!(t.assets.iter().map(|a|a.available).collect::<Vec<_>>(),available);
        h.send("preserved trade cannot settle twice",settle,true);
    }
}

fn setup(symbol:&str,winning:bool)->(Bank,ErTrader,ErCustody,Vec<Instruction>,u64) {
    let mut h=Bank::new();let a=h.register_backed_collateral(symbol);let nonce=if symbol=="FARTCOIN"{7}else{8};
    let primary=h.tokens[0];let sol=h.tokens[2];let usd=h.tokens[1];
    for (asset,amount) in [(a,100*UNIT),(primary,500_000*UNIT),(sol,1_000_000_000),(usd,100*UNIT)] {
        h.send("fund mixed-backing custody",vec![h.deposit(asset,amount)],false);
    }
    let before:ErCustody=get(&h.svm,h.custody);
    let intent=h.committed(a,nonce,100*UNIT,true,winning);
    let mut t:ErTrader=get(&h.svm,h.trader);
    if winning {t.slots[0].liquidated=false;t.slots[0].bust_observed=false;}
    t.core_stake.tokens=500_000*UNIT;t.auxiliary_stakes[0].tokens=1_000_000_000;t.auxiliary_stakes[1].tokens=100*UNIT;
    t.custody_generation=before.generation;t.notional=intent.entry_value*intent.leverage as u64;
    for(target,source)in t.assets.iter_mut().zip(before.assets.iter()) {
        *target=ErBalance{asset:source.asset,credited_deposits:source.deposited_credits,
            credited_returns:source.returned_credits,available:0,generation:source.generation};
    }
    put(&mut h.svm,h.trader,&t,p::basebridge::DELEGATION_OWNER);
    let fixture=h.client_fixture();let row=fixture["mixedSettlements"].as_array().unwrap().iter().find(|r|r["nonce"].as_u64()==Some(nonce)).unwrap();
    let instructions=Bank::client_instructions(&row["instructions"]);
    // Empty the wager token's executable quote reserves, preserving collected fees.
    let valuation=h.valuation(a);let mut quote=h.svm.get_account(&valuation.dex_quote).unwrap();
    let pool=h.svm.get_account(&a.pool).unwrap().data;
    let retained=[271,279].iter().map(|&at|pool.get(at..at+8).map(|b|u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0)).sum::<u64>();
    quote.data[64..72].copy_from_slice(&retained.to_le_bytes());quote.lamports=h.svm.minimum_balance_for_rent_exemption(quote.data.len())+retained;
    h.svm.set_account(valuation.dex_quote,quote).unwrap();
    (h,t,before,instructions,nonce)
}

#[test]
fn mixed_backing_converts_both_native_reserves_and_acknowledges_once() {
    for symbol in ["FARTCOIN","USDF"] {
        let(mut h,mut t,before,ixs,nonce)=setup(symbol,false);let primary=h.tokens[0];
        let vault=token(&h.svm,primary.vault);let inv:Inventory=get(&h.svm,primary.inventory);
        h.send("mixed native backing covers drained collateral",ixs.clone(),false);
        let c:ErCustody=get(&h.svm,h.custody);let x:Position=get(&h.svm,pda(&[b"position",h.signer.pubkey().as_ref(),&nonce.to_le_bytes()]));
        assert_eq!(x.uncovered_usd,0);assert_eq!(x.coverage_usd,10*UNIT);
        assert!(c.core_coverage.debited_tokens>0);
        for i in 0..2 {
            let charge=c.auxiliary_coverage[i].debited_tokens;assert!(charge>0&&charge<t.auxiliary_stakes[i].tokens);
            let asset=if i==0 {h.tokens[2]}else{h.tokens[1]};
            assert_eq!(c.assets[c.asset_index(asset.asset).unwrap()].available,before.assets[before.asset_index(asset.asset).unwrap()].available-charge);
            assert_eq!(get::<Inventory>(&h.svm,asset.inventory).locked,t.auxiliary_stakes[i].tokens-charge);
        }
        let bought=token(&h.svm,primary.vault)-vault;assert!(bought>0);
        assert_eq!(x.coverage_tokens,c.core_coverage.debited_tokens+bought);
        assert_eq!(get::<Inventory>(&h.svm,primary.inventory).treasury,inv.treasury+x.coverage_tokens);
        h.send("mixed close replay rejected",ixs,true);
        let mut risk=ErRisk{config:h.config,..ErRisk::default()};risk.markets[2].long_oi=t.notional;
        sync_custody(&mut t,&mut risk,&c).unwrap();let saved=t.auxiliary_stakes;
        sync_custody(&mut t,&mut risk,&c).unwrap();sync_custody(&mut t,&mut risk,&before).unwrap();
        assert_eq!(t.auxiliary_stakes,saved);assert_eq!(t.slots[0].state,ER_FREE);
    }
}

#[test]
fn failed_auxiliary_conversion_and_substituted_inventory_roll_back_close() {
    let(mut h,_,_,ixs,_)=setup("FARTCOIN",false);
    let mut bad=ixs.clone();let close=bad.last_mut().unwrap();
    // Last route minimum is the last u64 argument; a failed USDC conversion
    // must roll back the earlier SOL conversion and all settlement writes.
    let n=close.data.len();close.data[n-8..].copy_from_slice(&u64::MAX.to_le_bytes());
    h.send("failed second backing conversion rolls back first",bad,true);
    let mut bad=ixs.clone();let source=h.tokens[2].inventory;
    let account=bad.last_mut().unwrap().accounts.iter_mut().find(|a|a.pubkey==source).unwrap();account.pubkey=h.tokens[1].inventory;
    h.send("foreign auxiliary inventory rejected",bad,true);
    h.send("unmodified mixed settlement succeeds after failures",ixs,false);
}

#[test]
fn winning_position_does_not_consume_any_mixed_backing() {
    let(mut h,_,before,ixs,nonce)=setup("FARTCOIN",true);
    h.send("winner retains all three backing reserves",ixs,false);
    let c:ErCustody=get(&h.svm,h.custody);
    assert_eq!(c.core_coverage,before.core_coverage);assert_eq!(c.auxiliary_coverage,before.auxiliary_coverage);
    let x:Position=get(&h.svm,pda(&[b"position",h.signer.pubkey().as_ref(),&nonce.to_le_bytes()]));assert_eq!(x.coverage_tokens,0);
}
