#![cfg(not(feature="mainnet"))]
//! Real devnet SBF instruction validation in a local bank with injected ER-owned state
//! and a native-oracle fixture. This is not a live delegation/commit test.
use acidpump_er::{self as program, engine::*, math, oracle, Config};
use anchor_lang::{prelude::{Clock, Pubkey}, AccountDeserialize, AccountSerialize, InstructionData,
    ToAccountMetas, solana_program::instruction::{AccountMeta, Instruction}};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{path::Path, str::FromStr};

const NOW: i64 = 1_800_000_000;
const PX: u64 = 1_000_000_000_000;
#[test]
fn sol_collateral_is_sampled_from_magicblock_and_stored_at_open_by_actual_sbf() {
    let mut svm=LiteSVM::new();
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    svm.add_program(program::ID,&std::fs::read(root.join("target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let payer=Keypair::new_from_array([61;32]);let publisher=Keypair::new_from_array([62;32]);let owner=Keypair::new_from_array([63;32]);
    for key in [&payer,&publisher,&owner] {svm.airdrop(&key.pubkey(),1_000_000_000).unwrap();}
    let derive=|seeds:&[&[u8]]|Pubkey::find_program_address(seeds,&program::ID);
    let (config,config_bump)=derive(&[b"config"]);let (risk_key,risk_bump)=derive(&[b"er-risk"]);
    let asset=sol_collateral_asset();let (quote_key,quote_bump)=derive(&[b"er-quote",asset.as_ref()]);
    let (history_key,history_bump)=derive(&[b"er-collateral-history",asset.as_ref()]);
    let mut config_data=Config {admin:payer.pubkey(),publisher:publisher.pubkey(),quote_mint:Pubkey::new_unique(),paused:true,bump:config_bump,next_settlement_seq:1};
    put(&mut svm,config,&config_data);
    put(&mut svm,risk_key,&ErRisk {config,bump:risk_bump,..ErRisk::default()});
    put(&mut svm,quote_key,&ErQuote {config,asset,base_reserve:184049438,quote_reserve:1728954,fee_bps:[20,5,0],source_slot:999_999,publish_time:NOW-1,bump:quote_bump,..ErQuote::default()});
    put(&mut svm,history_key,&program::CollateralHistory {config,asset,sequence:0,source_slot:0,publish_time:0,last_gap:0,last_gap_start:0,is_quote:false,samples:vec![program::CollateralSample::default();64],bump:history_bump,
        #[cfg(feature="mainnet")] decimals:6});
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    svm.set_account(magic,Account {lamports:1_000_000,data:vec![],executable:true,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
    let feed=oracle::feed_address(2).unwrap();let mut data=vec![0u8;134];
    data[..8].copy_from_slice(&oracle::ORACLE_DISCRIMINATOR);data[40]=1;data[41..73].copy_from_slice(feed.as_ref());
    data[73..81].copy_from_slice(&10_950_000_000i64.to_le_bytes());data[89..93].copy_from_slice(&8i32.to_le_bytes());
    data[93..101].copy_from_slice(&NOW.to_le_bytes());data[125..133].copy_from_slice(&900u64.to_le_bytes());
    svm.set_account(feed,Account {lamports:10_000_000,data,owner:oracle::ORACLE_PROGRAM,executable:false,rent_epoch:0}).unwrap();
    let mut clock:Clock=svm.get_sysvar();clock.unix_timestamp=NOW;clock.slot=1000;svm.set_sysvar(&clock);
    let sample=Instruction {program_id:program::ID,accounts:program::accounts::SampleErPrices {
        publisher:publisher.pubkey(),config,er_risk:risk_key,btc_oracle:feed,zec_oracle:feed,sol_oracle:feed,eth_oracle:feed,
        execution:program::accounts::ErExecution {magic_program:magic},sol_quote:Some(quote_key),sol_history:Some(history_key)
    }.to_account_metas(None),data:program::instruction::ErSample {}.data()};
    send(&mut svm,&payer,&publisher,sample,None);
    let stored=svm.get_account(&quote_key).unwrap();let quote=ErQuote::try_deserialize(&mut &stored.data[..]).unwrap();
    assert_eq!(quote.value(2_000_000_000,NOW).unwrap(),219_000_000);
    assert_eq!(quote.source_slot,900,"native oracle source slot replaces the old base-chain domain");
    config_data.paused=false;put(&mut svm,config,&config_data);
    let (trader_key,trader_bump)=derive(&[b"er-trader",owner.pubkey().as_ref()]);
    let (guard_key,guard_bump)=derive(&[b"er-collateral-guard",owner.pubkey().as_ref()]);
    // Each venue's core stake is its pool's settlement token; this is the root pool.
    let (pool,pool_bump)=derive(&[b"pool",&0u16.to_le_bytes()]);
    put(&mut svm,pool,&program::Pool {id:0,settlement:program::stake_backing::core_asset(),authority_bump:0,queue_head:0,queue_tail:0,queue_usd:0,trades:0,bump:pool_bump});
    let mut trader=ErTrader {owner:owner.pubkey(),config,pool,next_nonce:1,bump:trader_bump,..ErTrader::default()};
    trader.assets[0]=ErBalance {asset,available:2_000_000_000,..ErBalance::default()};put(&mut svm,trader_key,&trader);
    put(&mut svm,guard_key,&program::CollateralGuard {owner:owner.pubkey(),config,floors:program::CollateralGuard::empty_floors(),bump:guard_bump});
    put(&mut svm,asset,&program::Asset {id:103,feed:Pubkey::default(),mint:anchor_spl::token::spl_token::native_mint::ID,
        token_program:anchor_spl::token::ID,source_mint:Pubkey::default(),source_pool:Pubkey::default(),
        quote_mint:config_data.quote_mint,dex_pool:Pubkey::default(),collateral_enabled:true,decimals:9,bump:0});
    let core=program::stake_backing::core_asset();
    let (core_quote,core_bump)=derive(&[b"er-quote",core.as_ref()]);
    put(&mut svm,core_quote,&ErQuote {config,asset:core,base_reserve:1_000_000_000,quote_reserve:1_000_000_000,fee_bps:[0;3],publish_time:NOW,source_slot:1,bump:core_bump,..ErQuote::default()});
    let mut backing:ErTrader=ErTrader::try_deserialize(&mut &svm.get_account(&trader_key).unwrap().data[..]).unwrap();
    backing.core_stake.tokens=1_000_000_000;put(&mut svm,trader_key,&backing);
    let open=Instruction {program_id:program::ID,accounts:program::accounts::OpenErPosition {
        owner:owner.pubkey(),config,er_trader:trader_key,er_risk:risk_key,asset,er_quote:quote_key,history:history_key,guard:guard_key,core_quote,
        execution:program::accounts::ErExecution {magic_program:magic},
        #[cfg(feature="mainnet")] gas_authority:publisher.pubkey(),
        pool,
    }.to_account_metas(None),data:program::instruction::ErOpen {slot:0,nonce:1,market:2,amount:2_000_000_000,leverage:1000,long:true,limit_price:u64::MAX,min_value:1,max_value:u64::MAX,expires:NOW+20}.data()};
    send(&mut svm,&payer,&owner,open,None);
    let stored=svm.get_account(&trader_key).unwrap();let trader=ErTrader::try_deserialize(&mut &stored.data[..]).unwrap();
    assert_eq!(trader.slots[0].entry_value,219_000_000);assert_eq!(trader.notional,219_000_000_000);
    assert_eq!(trader.slots[0].margin,2_000_000_000);assert_eq!(trader.slots[0].entry,109_500_000_000_000);
    let publish=Instruction {program_id:program::ID,accounts:program::accounts::PublishErQuote {publisher:publisher.pubkey(),config,er_quote:quote_key,history:history_key,execution:program::accounts::ErExecution {magic_program:magic}}.to_account_metas(None),
        data:program::instruction::ErPublishQuote {base_reserve:1,quote_reserve:999999999,fee_bps:[0;3],publish_time:NOW,source_slot:1001}.data()};
    send(&mut svm,&payer,&publisher,publish,Some("Binding"));
}
fn put<T: AccountSerialize>(svm: &mut LiteSVM, key: Pubkey, value: &T) {
    let mut data = Vec::new(); value.try_serialize(&mut data).unwrap();
    svm.set_account(key, Account { lamports: svm.minimum_balance_for_rent_exemption(data.len()),
        data, owner: program::ID, executable: false, rent_epoch: 0 }).unwrap();
}
fn send(svm: &mut LiteSVM, payer: &Keypair, signer: &Keypair, instruction: Instruction, expected: Option<&str>) {
    svm.expire_blockhash();
    let compute = Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap();
    let mut heap = vec![1u8]; heap.extend_from_slice(&(128u32 * 1024).to_le_bytes());
    let instructions = [Instruction { program_id: compute, accounts: vec![], data: heap }, instruction];
    let message = v0::Message::try_compile(&payer.pubkey(), &instructions, &[], svm.latest_blockhash()).unwrap();
    let mut signers = vec![payer];
    if instructions[1].accounts.iter().any(|a| a.pubkey == signer.pubkey() && a.is_signer) { signers.push(signer); }
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &signers).unwrap();
    let before: Vec<_> = instructions[1].accounts.iter().map(|a| (a.pubkey, svm.get_account(&a.pubkey))).collect();
    match (svm.send_transaction(tx), expected) {
        (Ok(meta), None) => println!("history gap owner forfeiture: {} CU", meta.compute_units_consumed),
        (Err(error), Some(reason)) => {
            assert!(error.meta.logs.iter().any(|log| log.contains(reason)), "expected {reason}: {error:?}");
            for (key, old) in before { assert_eq!(old, svm.get_account(&key), "failed instruction changed {key}"); }
        }
        (result, expected) => panic!("expected {expected:?}, got {result:?}"),
    }
}

#[test]
fn owner_gap_recovery_and_publisher_only_sampling_checked_by_actual_sbf() {
    let mut svm = LiteSVM::new();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    svm.add_program(program::ID, &std::fs::read(root.join("target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let payer = Keypair::new_from_array([51; 32]);
    let owner = Keypair::new_from_array([52; 32]);
    let stranger = Keypair::new_from_array([53; 32]);
    let publisher = Keypair::new_from_array([54; 32]);
    for key in [&payer, &owner, &stranger, &publisher] { svm.airdrop(&key.pubkey(), 1_000_000_000).unwrap(); }
    let (trader_key, trader_bump) = Pubkey::find_program_address(&[b"er-trader", owner.pubkey().as_ref()], &program::ID);
    let (risk_key, risk_bump) = Pubkey::find_program_address(&[b"er-risk"], &program::ID);
    let (config, config_bump) = Pubkey::find_program_address(&[b"config"], &program::ID);
    let config_data = Config { admin: Pubkey::new_unique(), publisher: publisher.pubkey(), quote_mint: Pubkey::new_unique(),
        paused: false, bump: config_bump, next_settlement_seq: 1 };
    put(&mut svm, config, &config_data);
    let mut trader = ErTrader { owner: owner.pubkey(), config, bump: trader_bump, next_nonce: 2, notional: 50_000_000, ..ErTrader::default() };
    trader.assets[0] = ErBalance { asset: Pubkey::new_unique(), available: 5_000_000, ..ErBalance::default() };
    trader.slots[0] = ErSlot { state: ER_OPEN, kind: ER_TRADE, nonce: 1, asset: trader.assets[0].asset,
        market: 0, margin: 5_000_000, entry_value: 5_000_000, entry: 100 * PX, bust: math::bust(100 * PX, 10, true).unwrap(),
        leverage: 10, long: true, cursor: 1, ..ErSlot::default() };
    let mut risk = ErRisk { config, bump: risk_bump, ..ErRisk::default() };
    risk.markets[0] = ErMarket { price: 105 * PX, publish_time: NOW, source_slot: 850, sequence: 66,
        prices: [105 * PX; 64], long_oi: 50_000_000, ..ErMarket::default() };
    put(&mut svm, trader_key, &trader); put(&mut svm, risk_key, &risk);
    let (guard_key,guard_bump)=Pubkey::find_program_address(&[b"er-collateral-guard",owner.pubkey().as_ref()],&program::ID);
    let mut guard=program::CollateralGuard {owner:owner.pubkey(),config,floors:program::CollateralGuard::empty_floors(),bump:guard_bump};
    guard.floors[0]=program::CollateralFloor {nonce:1,value:5_000_000,..program::CollateralFloor::default()};
    put(&mut svm,guard_key,&guard);
    let magic = ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    svm.set_account(magic, Account { lamports: 1_000_000, data: vec![], executable: true,
        owner: Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(), rent_epoch: 0 }).unwrap();
    let feed = oracle::feed_address(0).unwrap();
    let mut data = vec![0u8; 134];
    data[..8].copy_from_slice(&oracle::ORACLE_DISCRIMINATOR); data[40] = 1;
    data[41..73].copy_from_slice(feed.as_ref()); data[73..81].copy_from_slice(&10_600_000_000i64.to_le_bytes());
    data[89..93].copy_from_slice(&8i32.to_le_bytes()); data[93..101].copy_from_slice(&NOW.to_le_bytes());
    data[125..133].copy_from_slice(&900u64.to_le_bytes());
    let feed_account = Account { lamports: 10_000_000, data, owner: oracle::ORACLE_PROGRAM, executable: false, rent_epoch: 0 };
    svm.set_account(feed, feed_account.clone()).unwrap();
    let mut clock: Clock = svm.get_sysvar(); clock.unix_timestamp = NOW; clock.slot = 1000; svm.set_sysvar(&clock);
    let instruction = Instruction { program_id: program::ID,
        accounts: program::accounts::ForfeitErHistoryGap { owner: owner.pubkey(), er_trader: trader_key, er_risk: risk_key,
            guard:guard_key, oracle: feed, execution: program::accounts::ErExecution { magic_program: magic } }.to_account_metas(None),
        data: program::instruction::ErForfeitHistoryGap { slot: 0, nonce: 1, expires: NOW + 60 }.data() };
    let mut unsigned = instruction.clone(); unsigned.accounts[0].is_signer = false;
    send(&mut svm, &payer, &owner, unsigned, Some("AccountNotSigner"));
    let mut foreign = instruction.clone(); foreign.accounts[0] = AccountMeta::new_readonly(stranger.pubkey(), true);
    send(&mut svm, &payer, &stranger, foreign, Some("ConstraintSeeds"));
    risk.markets[0].sequence = 65; put(&mut svm, risk_key, &risk);
    send(&mut svm, &payer, &owner, instruction.clone(), Some("History"));
    risk.markets[0].sequence = 66; put(&mut svm, risk_key, &risk);
    let mut wrong_nonce = instruction.clone();
    wrong_nonce.data = program::instruction::ErForfeitHistoryGap { slot: 0, nonce: 2, expires: NOW + 60 }.data();
    send(&mut svm, &payer, &owner, wrong_nonce, Some("Replay"));
    let mut stale = feed_account.clone(); stale.data[93..101].copy_from_slice(&(NOW - 4).to_le_bytes());
    svm.set_account(feed, stale).unwrap(); send(&mut svm, &payer, &owner, instruction.clone(), Some("Stale"));
    let mut wrong_feed_owner = feed_account.clone(); wrong_feed_owner.owner = program::ID;
    svm.set_account(feed, wrong_feed_owner).unwrap(); send(&mut svm, &payer, &owner, instruction.clone(), Some("Binding"));
    svm.set_account(feed, feed_account).unwrap();
    let risk_before = svm.get_account(&risk_key).unwrap();
    send(&mut svm, &payer, &owner, instruction.clone(), None);
    let stored = svm.get_account(&trader_key).unwrap();
    let recovered = ErTrader::try_deserialize(&mut &stored.data[..]).unwrap();
    assert_eq!(recovered.slots[0].state, ER_PENDING);
    assert!(recovered.slots[0].liquidated && recovered.slots[0].bust_observed);
    assert_eq!(recovered.slots[0].exit, 106 * PX);
    assert_eq!(recovered.slots[0].margin, 5_000_000);
    assert_eq!(recovered.slots[0].close_seq, 0);
    assert_eq!(recovered.assets[0].available, 5_000_000);
    assert_eq!(recovered.notional, 50_000_000);
    assert_eq!(svm.get_account(&risk_key).unwrap(), risk_before);
    send(&mut svm, &payer, &owner, instruction, Some("Closed"));

    // Advancing the shared price ring is restricted to the configured publisher.
    // Otherwise an unrelated signer can force other traders past retained history.
    let sample = Instruction { program_id: program::ID,
        accounts: program::accounts::SampleErPrices { publisher: publisher.pubkey(), config, er_risk: risk_key,
            btc_oracle: feed, zec_oracle: feed, sol_oracle: feed, eth_oracle: feed,
            execution: program::accounts::ErExecution { magic_program: magic }, sol_quote: None, sol_history: None }.to_account_metas(None),
        data: program::instruction::ErSample {}.data() };
    let mut unsigned = sample.clone(); unsigned.accounts[0].is_signer = false;
    send(&mut svm, &payer, &publisher, unsigned, Some("AccountNotSigner"));
    let mut unauthorized = sample.clone(); unauthorized.accounts[0] = AccountMeta::new_readonly(stranger.pubkey(), true);
    send(&mut svm, &payer, &stranger, unauthorized, Some("ConstraintHasOne"));
    let forged_config = Pubkey::new_unique(); put(&mut svm, forged_config, &config_data);
    let mut forged = sample.clone(); forged.accounts[1].pubkey = forged_config;
    send(&mut svm, &payer, &publisher, forged, Some("ConstraintSeeds"));
    risk.config = forged_config; put(&mut svm, risk_key, &risk);
    send(&mut svm, &payer, &publisher, sample.clone(), Some("ConstraintHasOne"));
    risk.config = config; put(&mut svm, risk_key, &risk);
    send(&mut svm, &payer, &publisher, sample, None);
    let risk_after = svm.get_account(&risk_key).unwrap();
    let sampled = ErRisk::try_deserialize(&mut &risk_after.data[..]).unwrap();
    assert_eq!(sampled.markets[0].sequence, 67);
    assert_eq!(sampled.markets[0].price, 106 * PX);
    assert_eq!(sampled.markets[0].source_slot, 900);
}
