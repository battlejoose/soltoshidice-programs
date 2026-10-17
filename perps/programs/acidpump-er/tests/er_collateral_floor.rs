//! Actual SBF entrypoints in a local bank. ER state is injected; this does not
//! claim to exercise native delegation/commit transport on either cluster.
use acidpump_er::{self as p, engine::*, CollateralGuard, Config, Asset};
use anchor_lang::{prelude::{Clock,Pubkey}, AccountDeserialize, AccountSerialize, InstructionData,
    ToAccountMetas, solana_program::instruction::Instruction};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0,VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{path::Path,str::FromStr};
const NOW:i64=1_800_000_000;
const USD:u64=1_000_000;
const PX:u64=1_000_000_000_000;

#[test]
fn prepaid_gas_returns_only_excess_rent_from_a_returned_empty_owner_signed_ledger() {
    let mut s=LiteSVM::new();s.add_program(p::ID,&std::fs::read(candidate_binary()).unwrap()).unwrap();
    let publisher=Keypair::new_from_array([111;32]);let owner=Keypair::new_from_array([112;32]);let other=Keypair::new_from_array([113;32]);
    for k in [&publisher,&owner,&other]{s.airdrop(&k.pubkey(),1_000_000_000).unwrap();}
    let(config,cb)=key(&[b"config"]);let(pool,_)=key(&[b"pool",&0u16.to_le_bytes()]);
    let(trader,tb)=key(&[b"er-trader",owner.pubkey().as_ref()]);
    put(&mut s,config,&Config{admin:publisher.pubkey(),publisher:publisher.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:1});
    let empty=ErTrader{owner:owner.pubkey(),config,pool,next_nonce:1,bump:tb,..ErTrader::default()};
    put(&mut s,trader,&empty);
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:false,owner:anchor_lang::system_program::ID,rent_epoch:0}).unwrap();
    let ix=|user,authority|Instruction{program_id:p::ID,accounts:p::accounts::WithdrawPrepaidGas{
        owner:user,publisher:authority,config,er_trader:trader,execution:p::accounts::BaseExecution{magic_program:magic},
    }.to_account_metas(None),data:p::instruction::WithdrawPrepaidGas{}.data()};
    send(&mut s,&publisher,&other,ix(other.pubkey(),publisher.pubkey()),Some("Constraint"));
    send(&mut s,&publisher,&owner,ix(owner.pubkey(),owner.pubkey()),Some("ConstraintHasOne"));
    let mut delegated=s.get_account(&trader).unwrap();delegated.owner=p::basebridge::DELEGATION_OWNER;s.set_account(trader,delegated).unwrap();
    send(&mut s,&publisher,&owner,ix(owner.pubkey(),publisher.pubkey()),Some("AccountOwnedByWrongProgram"));
    for case in 0..4 {
        let mut blocked=empty.clone();
        match case {0=>blocked.slots[0].state=ER_OPEN,1=>blocked.slots[0].state=ER_PENDING,2=>blocked.core_stake.tokens=1,_=>blocked.assets[0].available=1};
        put(&mut s,trader,&blocked);
        send(&mut s,&publisher,&owner,ix(owner.pubkey(),publisher.pubkey()),Some(if case==3{"Inventory"}else{"Closed"}));
    }
    put(&mut s,trader,&empty);
    send(&mut s,&publisher,&owner,ix(owner.pubkey(),publisher.pubkey()),Some("Amount"));
    let mut funded=s.get_account(&trader).unwrap();let rent=funded.lamports;let data=funded.data.clone();funded.lamports+=2_000_000;s.set_account(trader,funded).unwrap();
    let before=s.get_account(&owner.pubkey()).unwrap().lamports;
    send(&mut s,&publisher,&owner,ix(owner.pubkey(),publisher.pubkey()),None);
    assert_eq!(s.get_account(&owner.pubkey()).unwrap().lamports,before+2_000_000);
    let after=s.get_account(&trader).unwrap();assert_eq!(after.lamports,rent);assert_eq!(after.data,data);
}
fn candidate_binary()->std::path::PathBuf {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    root.join(std::env::var("ACIDPUMP_TEST_BINARY").unwrap_or_else(|_|if cfg!(feature="mainnet") {
        ".cache/acidpump-mainnet-candidate/acidpump_er.so".into()
    } else {"target/deploy/acidpump_er.so".into()}))
}
fn key(seeds:&[&[u8]])->(Pubkey,u8){Pubkey::find_program_address(seeds,&p::ID)}
fn put<T:AccountSerialize>(s:&mut LiteSVM,k:Pubkey,v:&T){
    let mut data=vec![];v.try_serialize(&mut data).unwrap();
    s.set_account(k,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
}
fn get<T:AccountDeserialize>(s:&LiteSVM,k:Pubkey)->T{T::try_deserialize(&mut &s.get_account(&k).unwrap().data[..]).unwrap()}
fn time(s:&mut LiteSVM,t:i64){let mut c:Clock=s.get_sysvar();c.unix_timestamp=t;c.slot=1000;s.set_sysvar(&c);}
fn send(s:&mut LiteSVM,payer:&Keypair,signer:&Keypair,ix:Instruction,error:Option<&str>){
    s.expire_blockhash();
    let mut data=vec![1];data.extend_from_slice(&(128u32*1024).to_le_bytes());
    // Match the deployed ER builder's 400k CU / 128 KiB envelope. The default
    // 200k envelope is too small when decoding a full 50-slot wallet.
    let mut units=vec![2];units.extend_from_slice(&400_000u32.to_le_bytes());
    let compute=Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap();
    let ixs=[Instruction{program_id:compute,accounts:vec![],data},Instruction{program_id:compute,accounts:vec![],data:units},ix];
    let msg=v0::Message::try_compile(&payer.pubkey(),&ixs,&[],s.latest_blockhash()).unwrap();
    let mut signers=vec![payer];if signer.pubkey()!=payer.pubkey()&&ixs[2].accounts.iter().any(|a|a.is_signer&&a.pubkey==signer.pubkey()){signers.push(signer);}
    let before:Vec<_>=ixs[2].accounts.iter().filter(|a|a.pubkey!=payer.pubkey()).map(|a|(a.pubkey,s.get_account(&a.pubkey))).collect();
    match(s.send_transaction(VersionedTransaction::try_new(VersionedMessage::V0(msg),&signers).unwrap()),error){
        (Ok(meta),None)=>println!("collateral protection: {} CU",meta.compute_units_consumed),
        (Err(e),Some(reason))=>{assert!(e.meta.logs.iter().any(|l|l.contains(reason)),"expected {reason}: {e:?}");for(k,v)in before{assert_eq!(s.get_account(&k),v);}},
        (actual,expected)=>panic!("expected {expected:?}, got {actual:?}"),
    }
}

#[test]
fn core_staking_entrypoint_enforces_owner_locks_and_cumulative_coverage_debits() {
    use p::{Pool, stake_backing::{core_asset, CoreCoverageState}};
    let mut s=LiteSVM::new();
    s.add_program(p::ID,&std::fs::read(candidate_binary()).unwrap()).unwrap();
    let payer=Keypair::new_from_array([91;32]);let owner=Keypair::new_from_array([92;32]);let stranger=Keypair::new_from_array([93;32]);
    for k in [&payer,&owner,&stranger]{s.airdrop(&k.pubkey(),1_000_000_000).unwrap();}
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    let(config,cb)=key(&[b"config"]);let(pool,pb)=key(&[b"pool",&0u16.to_le_bytes()]);
    let(trader,tb)=key(&[b"er-trader",owner.pubkey().as_ref()]);let(custody,bump)=key(&[b"er-custody",owner.pubkey().as_ref()]);let(risk,rb)=key(&[b"er-risk"]);
    put(&mut s,config,&Config{admin:payer.pubkey(),publisher:payer.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:1});
    let pool_data=Pool{id:0,settlement:core_asset(),authority_bump:0,queue_head:0,queue_tail:0,queue_usd:0,trades:0,bump:pb};put(&mut s,pool,&pool_data);
    let mut trader_data=ErTrader{owner:owner.pubkey(),config,pool,next_nonce:1,bump:tb,..ErTrader::default()};
    trader_data.assets[0]=ErBalance{asset:core_asset(),available:100,credited_deposits:100,..ErBalance::default()};put(&mut s,trader,&trader_data);
    let mut custody_data=ErCustody{owner:owner.pubkey(),config,pool,bump,..ErCustody::default()};
    custody_data.assets[0]=CustodyAsset{asset:core_asset(),available:100,deposited_credits:100,..CustodyAsset::default()};put(&mut s,custody,&custody_data);
    put(&mut s,risk,&ErRisk{config,bump:rb,..ErRisk::default()});time(&mut s,NOW);
    let stake=|signer:Pubkey,amount,unstake|Instruction{program_id:p::ID,
        accounts:p::accounts::ChangeErCoreStake{owner:signer,config,pool,er_trader:trader,execution:p::accounts::ErExecution{magic_program:magic}}.to_account_metas(None),
        data:p::instruction::ErChangeCoreStake{amount,unstake}.data()};
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:false,owner:anchor_lang::system_program::ID,rent_epoch:0}).unwrap();
    send(&mut s,&payer,&owner,stake(owner.pubkey(),60,false),Some("ConstraintOwner"));
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:false,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
    send(&mut s,&payer,&owner,stake(owner.pubkey(),60,false),Some("ConstraintExecutable"));
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:true,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
    send(&mut s,&payer,&stranger,stake(stranger.pubkey(),60,false),Some("Constraint"));
    send(&mut s,&payer,&owner,stake(owner.pubkey(),60,false),None);
    let t:ErTrader=get(&s,trader);assert_eq!((t.core_stake.tokens,t.assets[0].available),(60,40));
    send(&mut s,&payer,&owner,stake(owner.pubkey(),41,false),Some("Inventory"));
    for state in [ER_OPEN,ER_PENDING] {
        let mut t:ErTrader=get(&s,trader);t.slots[0]=ErSlot{state,kind:ER_TRADE,nonce:1,..ErSlot::default()};put(&mut s,trader,&t);
        send(&mut s,&payer,&owner,stake(owner.pubkey(),1,true),Some("Closed"));
    }
    let mut t:ErTrader=get(&s,trader);t.slots[0]=ErSlot::default();put(&mut s,trader,&t);
    custody_data.core_coverage=CoreCoverageState{debited_tokens:20,..CoreCoverageState::default()};custody_data.assets[0].available=80;put(&mut s,custody,&custody_data);
    let sync=||Instruction{program_id:p::ID,accounts:p::accounts::SyncErTrader{er_trader:trader,er_risk:risk,er_custody:custody,execution:p::accounts::ErExecution{magic_program:magic}}.to_account_metas(None),data:p::instruction::ErSync{}.data()};
    for _ in 0..2 {send(&mut s,&payer,&payer,sync(),None);}
    let t:ErTrader=get(&s,trader);assert_eq!((t.core_stake.tokens,t.assets[0].available),(40,40));
    custody_data.core_coverage=CoreCoverageState::default();put(&mut s,custody,&custody_data);send(&mut s,&payer,&payer,sync(),None);
    assert_eq!(get::<ErTrader>(&s,trader).core_stake.tokens,40);
    let mut c:Config=get(&s,config);c.paused=true;put(&mut s,config,&c);
    send(&mut s,&payer,&owner,stake(owner.pubkey(),1,false),Some("Paused"));
    send(&mut s,&payer,&owner,stake(owner.pubkey(),40,true),None);
    let t:ErTrader=get(&s,trader);assert_eq!((t.core_stake.tokens,t.assets[0].available),(0,80));
}
#[test]
fn backed_entries_and_oldest_force_close_use_actual_instructions_without_collateral_history() {
    backing_entrypoints(Pubkey::new_unique(),false,false);
}

#[test]
fn canonical_sol_usdc_and_core_open_and_close_without_stake_or_fresh_core_quote() {
    #[cfg(feature="mainnet")]
    let mints=[p::mainnet_pump::SOL,p::mainnet_pump::USDC,p::mainnet_config::PRIMARY_MINT];
    #[cfg(not(feature="mainnet"))]
    let mints=[p::mainnet_pump::SOL,Pubkey::from_str("AcXG57AXp3dtddW9XwX5cvzbEVqNxmhuQJH2BiLHeaXT").unwrap(),Pubkey::from_str("CCpKQPW9eCMeEQ7wG8tbZPqWMHEBt9cTQEstFW5W6DaL").unwrap()];
    for mint in mints {backing_entrypoints(mint,true,false);}
}

#[cfg(feature="mainnet")]
#[test]
fn fifty_slots_open_close_reconcile_and_reuse_without_exposure_leak() {
    backing_entrypoints(p::mainnet_pump::USDC,true,true);
}

#[cfg(feature="mainnet")]
#[test]
fn usdc_quoted_collateral_publishes_opens_and_closes_in_micro_usd() {
    use p::{CollateralHistory,CollateralSample,MainnetCollateralContext,MainnetCollateralObservation,stake_backing::core_asset};
    let mut s=LiteSVM::new();s.add_program(p::ID,&std::fs::read(candidate_binary()).unwrap()).unwrap();
    let payer=Keypair::new_from_array([96;32]);let owner=Keypair::new_from_array([97;32]);
    for k in [&payer,&owner]{s.airdrop(&k.pubkey(),1_000_000_000).unwrap();}
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:true,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
    let(config,cb)=key(&[b"config"]);let(pool,_)=key(&[b"pool",&0u16.to_le_bytes()]);
    put(&mut s,config,&Config{admin:payer.pubkey(),publisher:payer.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:1});
    let mint=Pubkey::new_unique();let(asset,ab)=key(&[b"asset",mint.as_ref()]);
    put(&mut s,asset,&Asset{id:140,feed:Pubkey::new_unique(),mint,token_program:anchor_spl::token::ID,source_mint:mint,
        source_pool:Pubkey::new_unique(),quote_mint:p::mainnet_pump::USDC,dex_pool:Pubkey::new_unique(),collateral_enabled:true,decimals:6,bump:ab});
    let(quote,_)=key(&[b"er-quote",asset.as_ref()]);let(history,hb)=key(&[b"er-collateral-history",asset.as_ref()]);
    time(&mut s,NOW);
    send(&mut s,&payer,&payer,Instruction{program_id:p::ID,accounts:p::accounts::InitializeErQuote{admin:payer.pubkey(),config,asset,
        er_quote:quote,system_program:Pubkey::default()}.to_account_metas(None),data:p::instruction::ErInitializeQuote{}.data()},None);
    assert!(!get::<ErQuote>(&s,quote).is_quote);
    put(&mut s,history,&CollateralHistory{config,asset,sequence:0,source_slot:0,publish_time:0,last_gap:0,last_gap_start:0,
        is_quote:false,decimals:6,samples:vec![CollateralSample::default();64],bump:hb});
    let observation=MainnetCollateralObservation{base_reserve:1000*USD,quote_reserve:1000*USD,fee_bps:[20,5,0],publish_time:NOW,observation:1,
        context:MainnetCollateralContext{real_quote_reserve:1000*USD,pool_source_slot:1000,usd_per_quote_atom_e21:1_000_000_000_000_000,
            conversion_publish_time:NOW,conversion_source_slot:1000}};
    let publish=|o:MainnetCollateralObservation|Instruction{program_id:p::ID,accounts:p::accounts::PublishErQuote{publisher:payer.pubkey(),config,
        er_quote:quote,history,execution:p::accounts::ErExecution{magic_program:magic}}.to_account_metas(None),data:p::instruction::ErPublishMainnetQuote{observation:o}.data()};
    send(&mut s,&payer,&payer,publish(observation.clone()),None);
    send(&mut s,&payer,&payer,publish(observation.clone()),Some("Replay"));
    // $100 of base tokens has $90.68 of executable USDC value after slippage/fees.
    let expected=90_681_816u64;assert_eq!(get::<ErQuote>(&s,quote).value(100*USD,NOW).unwrap(),expected);
    let(trader,tb)=key(&[b"er-trader",owner.pubkey().as_ref()]);let(risk,rb)=key(&[b"er-risk"]);
    let(guard,gb)=key(&[b"er-collateral-guard",owner.pubkey().as_ref()]);let(core_quote,kb)=key(&[b"er-quote",core_asset().as_ref()]);
    let mut t=ErTrader{owner:owner.pubkey(),config,pool,next_nonce:1,bump:tb,..ErTrader::default()};
    t.assets[0]=ErBalance{asset,available:200*USD,..ErBalance::default()};t.core_stake.tokens=500*USD;put(&mut s,trader,&t);
    let mut r=ErRisk{config,bump:rb,..ErRisk::default()};
    r.markets[0]=ErMarket{price:100*PX,sequence:1,source_slot:1,publish_time:NOW,prices:[100*PX;64],long_cap:100_000*USD,..ErMarket::default()};
    put(&mut s,risk,&r);put(&mut s,guard,&CollateralGuard{owner:owner.pubkey(),config,floors:CollateralGuard::empty_floors(),bump:gb});
    put(&mut s,core_quote,&ErQuote{config,asset:core_asset(),base_reserve:1000*USD,quote_reserve:10_000_000_000,source_slot:1,publish_time:NOW,bump:kb,
        mainnet:MainnetCollateralContext{real_quote_reserve:10_000_000_000,pool_source_slot:1,usd_per_quote_atom_e21:100*PX,conversion_publish_time:NOW,conversion_source_slot:1},..ErQuote::default()});
    let open=Instruction{program_id:p::ID,accounts:p::accounts::OpenErPosition{owner:owner.pubkey(),config,er_trader:trader,er_risk:risk,asset,er_quote:quote,
        history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote,gas_authority:payer.pubkey()}.to_account_metas(None),
        data:p::instruction::ErOpen{slot:0,nonce:1,market:0,amount:100*USD,leverage:1000,long:true,limit_price:u64::MAX,min_value:0,max_value:u64::MAX,expires:NOW+60}.data()};
    let mut unpaid=open.clone();unpaid.accounts.iter_mut().find(|a|a.pubkey==payer.pubkey()).unwrap().pubkey=owner.pubkey();
    send(&mut s,&payer,&owner,unpaid,Some("Unauthorized"));
    time(&mut s,NOW+4);send(&mut s,&payer,&owner,open.clone(),Some("Stale"));time(&mut s,NOW);
    send(&mut s,&payer,&owner,open,None);
    assert_eq!(get::<ErTrader>(&s,trader).slots[0].entry_value,expected);
    let mut lower=observation.clone();lower.observation=2;lower.quote_reserve/=2;lower.context.real_quote_reserve/=2;
    lower.context.pool_source_slot+=1;lower.context.conversion_source_slot+=1;
    send(&mut s,&payer,&payer,publish(lower),None);
    assert_eq!(get::<ErQuote>(&s,quote).value(100*USD,NOW).unwrap(),45_340_907);
    // Market price is unchanged; only the collateral denomination declined.
    let close=Instruction{program_id:p::ID,accounts:p::accounts::CloseErPosition{caller:owner.pubkey(),er_trader:trader,er_risk:risk,er_quote:quote,
        history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote}.to_account_metas(None),
        data:p::instruction::ErClose{slot:0,nonce:1,expires:NOW+60}.data()};
    send(&mut s,&payer,&owner,close,None);
    let settled=get::<ErTrader>(&s,trader);assert_eq!(settled.slots[0].state,ER_PENDING);assert_eq!(settled.slots[0].entry_value,expected);
    assert_eq!(settled.slots[0].exit,100*PX);assert_eq!(settled.slots[0].margin,100*USD);
}

fn backing_entrypoints(mint:Pubkey,exempt:bool,fill_wallet:bool) {
    use p::{CollateralHistory, CollateralSample, stake_backing::core_asset};
    #[cfg(feature="mainnet")]
    use p::MainnetCollateralContext;
    let mut s=LiteSVM::new();
    s.add_program(p::ID,&std::fs::read(candidate_binary()).unwrap()).unwrap();
    let payer=Keypair::new_from_array([94;32]);let owner=Keypair::new_from_array([95;32]);
    for k in [&payer,&owner]{s.airdrop(&k.pubkey(),1_000_000_000).unwrap();}
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    s.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:true,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
    let(config,cb)=key(&[b"config"]);let(pool,_)=key(&[b"pool",&0u16.to_le_bytes()]);
    put(&mut s,config,&Config{admin:payer.pubkey(),publisher:payer.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:1});
    let(asset,ab)=key(&[b"asset",mint.as_ref()]);
    put(&mut s,asset,&Asset{id:104,feed:Pubkey::new_unique(),mint,token_program:anchor_spl::token::ID,source_mint:Pubkey::default(),source_pool:Pubkey::default(),quote_mint:p::mainnet_pump::SOL,dex_pool:Pubkey::from_str("Gf7sXMoP8iRw4iiXmJ1nq4vxcRycbGXy5RL8a8LnTd3v").unwrap(),collateral_enabled:true,decimals:6,bump:ab});
    let(trader,tb)=key(&[b"er-trader",owner.pubkey().as_ref()]);let(risk,rb)=key(&[b"er-risk"]);
    let(quote,qb)=key(&[b"er-quote",asset.as_ref()]);let(core_quote,kb)=key(&[b"er-quote",core_asset().as_ref()]);
    let(history,hb)=key(&[b"er-collateral-history",asset.as_ref()]);let(guard,gb)=key(&[b"er-collateral-guard",owner.pubkey().as_ref()]);
    let mut t=ErTrader{owner:owner.pubkey(),config,pool,next_nonce:1,bump:tb,..ErTrader::default()};
    t.assets[0]=ErBalance{asset,available:200*USD,..ErBalance::default()};put(&mut s,trader,&t);
    let mut r=ErRisk{config,bump:rb,..ErRisk::default()};r.markets[0]=ErMarket{price:100*PX,sequence:1,source_slot:1,publish_time:NOW,prices:[100*PX;64],long_cap:100_000*USD,..ErMarket::default()};put(&mut s,risk,&r);
    put(&mut s,quote,&ErQuote{config,asset,is_quote:true,publish_time:NOW,source_slot:1,bump:qb,..ErQuote::default()});
    let mut q=ErQuote{config,asset:core_asset(),base_reserve:1_000*USD,quote_reserve:if cfg!(feature="mainnet"){10_000_000_000}else{1_000*USD},source_slot:1,publish_time:NOW,bump:kb,
        #[cfg(feature="mainnet")]
        mainnet:MainnetCollateralContext{real_quote_reserve:10_000_000_000,pool_source_slot:1,usd_per_quote_atom_e21:100*PX,conversion_publish_time:NOW,conversion_source_slot:1},..ErQuote::default()};put(&mut s,core_quote,&q);
    // Empty and gapped collateral history deliberately cannot prove any low.
    put(&mut s,history,&CollateralHistory{config,asset,sequence:0,source_slot:0,publish_time:0,last_gap:99,last_gap_start:NOW-100,is_quote:true,
        #[cfg(feature="mainnet")]
        decimals:6,samples:vec![CollateralSample::default();64],bump:hb});
    put(&mut s,guard,&CollateralGuard{owner:owner.pubkey(),config,floors:CollateralGuard::empty_floors(),bump:gb});time(&mut s,NOW);
    let open=|slot,nonce,amount|Instruction{program_id:p::ID,accounts:p::accounts::OpenErPosition{owner:owner.pubkey(),config,er_trader:trader,er_risk:risk,asset,er_quote:quote,history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote,gas_authority:payer.pubkey()}.to_account_metas(None),data:p::instruction::ErOpen{slot,nonce,market:0,amount,leverage:1000,long:true,limit_price:u64::MAX,min_value:0,max_value:u64::MAX,expires:NOW+60}.data()};
    if fill_wallet {
        assert_eq!(ER_SLOTS,10);
        for slot in 0..ER_SLOTS {send(&mut s,&payer,&owner,open(slot as u8,slot as u64+1,USD),None);}
        let t:ErTrader=get(&s,trader);assert!(t.slots.iter().all(|x|x.state==ER_OPEN));
        assert_eq!(t.notional,10_000*USD);assert_eq!(t.assets[0].available,190*USD);
        send(&mut s,&payer,&owner,open(9,11,USD),Some("Closed"));
        send(&mut s,&payer,&owner,open(10,11,USD),Some("Closed"));
        // A full wallet catching up on an entire retained market window must
        // still fit the production transaction envelope, not only fresh opens.
        let mut latest:ErRisk=get(&s,risk);latest.markets[0].sequence=65;latest.markets[0].source_slot=65;put(&mut s,risk,&latest);
        let observe=Instruction{program_id:p::ID,accounts:p::accounts::ObserveErTrader{er_trader:trader,er_risk:risk,
            execution:p::accounts::ErExecution{magic_program:magic}}.to_account_metas(None),data:p::instruction::ErObserve{}.data()};
        send(&mut s,&payer,&payer,observe,None);
        assert!(get::<ErTrader>(&s,trader).slots.iter().all(|x|x.cursor==65));
        let close=|slot,nonce|Instruction{program_id:p::ID,accounts:p::accounts::CloseErPosition{caller:owner.pubkey(),er_trader:trader,er_risk:risk,
            er_quote:quote,history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote}.to_account_metas(None),
            data:p::instruction::ErClose{slot,nonce,expires:NOW+60}.data()};
        send(&mut s,&payer,&owner,close(9,10),None);
        assert_eq!(get::<ErTrader>(&s,trader).notional,10_000*USD,"a close intent must retain exposure until settlement");
        let(custody,b)=key(&[b"er-custody",owner.pubkey().as_ref()]);
        let mut c=ErCustody{owner:owner.pubkey(),config,pool,bump:b,..ErCustody::default()};
        c.assets[0]=CustodyAsset{asset,returned_credits:USD,..CustodyAsset::default()};
        c.receipts[9]=ErReceipt{nonce:10,kind:ER_TRADE,settled:true,position:Pubkey::new_unique()};put(&mut s,custody,&c);
        let sync=Instruction{program_id:p::ID,accounts:p::accounts::SyncErTrader{er_trader:trader,er_risk:risk,er_custody:custody,
            execution:p::accounts::ErExecution{magic_program:magic}}.to_account_metas(None),data:p::instruction::ErSync{}.data()};
        send(&mut s,&payer,&payer,sync.clone(),None);send(&mut s,&payer,&payer,sync,None);
        let t:ErTrader=get(&s,trader);assert_eq!(t.notional,9_000*USD);assert_eq!(t.assets[0].available,191*USD);
        assert_eq!(get::<ErRisk>(&s,risk).markets[0].long_oi,9_000*USD);
        send(&mut s,&payer,&owner,open(9,11,USD),None);
        let mut latest:ErRisk=get(&s,risk);latest.markets[0].sequence=67;latest.markets[0].source_slot=67;
        latest.markets[0].prices[2]=99*PX;put(&mut s,risk,&latest);
        for slot in 0..ER_SLOTS {send(&mut s,&payer,&owner,close(slot as u8,if slot==9 {11}else{slot as u64+1}),None);}
        let t:ErTrader=get(&s,trader);assert!(t.slots.iter().all(|x|x.state==ER_PENDING && x.liquidated));assert_eq!(t.notional,10_000*USD);
        return;
    }
    if exempt {
        // A distinct core quote can be stale. For a core-token bet the core
        // quote is itself the collateral quote, which must remain fresh.
        if asset!=core_asset() {q.publish_time=NOW-30;put(&mut s,core_quote,&q);}
        else {put(&mut s,quote,&ErQuote{config,asset,is_quote:true,publish_time:NOW,source_slot:1,bump:qb,..ErQuote::default()});}
        send(&mut s,&payer,&owner,open(0,1,40*USD),None);
        let close=|forced|Instruction{program_id:p::ID,accounts:p::accounts::CloseErPosition{caller:if forced {payer.pubkey()}else{owner.pubkey()},er_trader:trader,er_risk:risk,er_quote:quote,history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote}.to_account_metas(None),data:if forced {p::instruction::ErForceCoreClose{slot:0,nonce:1,expires:NOW+60}.data()}else{p::instruction::ErClose{slot:0,nonce:1,expires:NOW+60}.data()}};
        send(&mut s,&payer,&payer,close(true),Some(if asset==core_asset(){"Binding"}else{"Closed"}));
        send(&mut s,&payer,&owner,close(false),None);
        let t:ErTrader=get(&s,trader);assert_eq!(t.slots[0].state,ER_PENDING);assert_eq!(t.core_stake.tokens,0);
        return;
    }
    send(&mut s,&payer,&owner,open(0,1,40*USD),Some("Exposure"));
    t.core_stake.tokens=100*USD;put(&mut s,trader,&t);
    send(&mut s,&payer,&owner,open(0,1,40*USD),None);
    send(&mut s,&payer,&owner,open(1,2,40*USD),None);
    assert_eq!(get::<ErTrader>(&s,trader).notional,80_000*USD);
    send(&mut s,&payer,&owner,open(2,3,20*USD),Some("Exposure"));
    let close=|slot,nonce,forced|Instruction{program_id:p::ID,accounts:p::accounts::CloseErPosition{caller:if forced {payer.pubkey()}else{owner.pubkey()},er_trader:trader,er_risk:risk,er_quote:quote,history,guard,execution:p::accounts::ErExecution{magic_program:magic},core_quote}.to_account_metas(None),data:if forced {p::instruction::ErForceCoreClose{slot,nonce,expires:NOW+60}.data()}else{p::instruction::ErClose{slot,nonce,expires:NOW+60}.data()}};
    send(&mut s,&payer,&payer,close(0,1,true),Some("Exposure"));
    q.quote_reserve/=2;
    #[cfg(feature="mainnet")]
    {q.mainnet.real_quote_reserve/=2;}
    put(&mut s,core_quote,&q);
    send(&mut s,&payer,&payer,close(1,2,true),Some("QueueOrder"));
    q.publish_time=NOW-4;put(&mut s,core_quote,&q);
    send(&mut s,&payer,&payer,close(0,1,true),Some("Stale"));
    q.publish_time=NOW;put(&mut s,core_quote,&q);
    send(&mut s,&payer,&payer,close(0,1,true),None);
    let t:ErTrader=get(&s,trader);assert_eq!(t.slots[0].state,ER_PENDING);assert_eq!(t.slots[1].state,ER_OPEN);
    send(&mut s,&payer,&payer,close(1,2,true),Some("Closed"));
    send(&mut s,&payer,&owner,open(2,3,USD),Some("Exposure"));
    // Manual concurrent closes remain available while the first settles, even
    // when the collateral history is empty and cannot be recovered.
    send(&mut s,&payer,&owner,close(1,2,false),None);
    assert_eq!(get::<ErTrader>(&s,trader).slots[1].state,ER_PENDING);
}
