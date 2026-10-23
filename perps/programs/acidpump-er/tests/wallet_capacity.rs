#![cfg(feature="mainnet")]
//! Execute account resizing against the candidate SBF. DLP's completed return
//! is an injected boundary; this does not claim to test validator transport.
use acidpump_er::{self as p,engine::*};
use anchor_lang::{prelude::{Pubkey,Space},AccountSerialize,InstructionData,ToAccountMetas};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0,VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use anchor_lang::solana_program::instruction::Instruction;
use std::str::FromStr;

fn bytes<T:AccountSerialize>(value:&T)->Vec<u8>{let mut data=vec![];value.try_serialize(&mut data).unwrap();data}
fn legacy(mut data:Vec<u8>,prefix:usize,item:usize)->Vec<u8>{data.drain(prefix+8*item..prefix+ER_SLOTS*item);data}
fn legacy_ledger(data:&[u8],prefix:usize,slot_bytes:usize,tail:usize)->Vec<u8> {
    let mut old=data[..prefix].to_vec();
    for i in 0..16 {let at=prefix+i*64;old.extend_from_slice(&data[at..at+56]);}
    let at=prefix+ER_ASSETS*64;old.extend_from_slice(&data[at..at+8*slot_bytes]);
    let at=at+ER_SLOTS*slot_bytes;old.extend_from_slice(&data[at..at+tail]);old
}
fn pda(seeds:&[&[u8]])->(Pubkey,u8){Pubkey::find_program_address(seeds,&p::ID)}
fn send(s:&mut LiteSVM,admin:&Keypair,ix:Instruction,error:Option<&str>){
    s.expire_blockhash();let compute=Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap();
    let mut heap=vec![1];heap.extend_from_slice(&(128*1024u32).to_le_bytes());
    let mut units=vec![2];units.extend_from_slice(&400_000u32.to_le_bytes());
    let old:Vec<_>=ix.accounts.iter().filter(|a|a.pubkey!=admin.pubkey()).map(|a|(a.pubkey,s.get_account(&a.pubkey))).collect();
    let ixs=[Instruction{program_id:compute,accounts:vec![],data:heap},Instruction{program_id:compute,accounts:vec![],data:units},ix];
    let message=v0::Message::try_compile(&admin.pubkey(),&ixs,&[],s.latest_blockhash()).unwrap();
    let result=s.send_transaction(VersionedTransaction::try_new(VersionedMessage::V0(message),&[admin]).unwrap());
    match error {
        None=>{let meta=result.unwrap_or_else(|e|panic!("migration failed: {e:?}"));println!("capacity migration: {} CU",meta.compute_units_consumed);},
        Some(reason)=>{let e=result.expect_err(reason);assert!(e.meta.logs.iter().any(|l|l.contains(reason)),"expected {reason}: {e:?}");
            for (key,before) in old {assert_eq!(s.get_account(&key),before,"failed migration must be atomic");}}
    }
}

#[test]
fn ten_slot_migration_preserves_funds_requires_returned_accounts_and_is_idempotent(){
    migrate_fixture(false,false,false,false);
}
#[test]
fn fifty_to_ten_slot_shrink_preserves_funds_and_rejects_unfinished_tail(){
    migrate_fixture(true,false,false,false);
}
#[test]
fn current_ten_slot_backing_extension_preserves_every_byte_in_root_and_created_markets(){
    migrate_fixture(false,true,false,false);migrate_fixture(false,true,true,false);
}
#[test]
fn append_only_migration_preserves_open_trades_notional_and_liquidation_fields_in_both_namespaces(){
    migrate_fixture(false,true,false,true);migrate_fixture(false,true,true,true);
}
fn migrate_fixture(previous_fifty:bool,old_ten:bool,venue:bool,open:bool){
    let mut s=LiteSVM::new();let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary=std::env::var("ACIDPUMP_TEST_BINARY").expect("explicit candidate binary required");
    s.add_program(p::ID,&std::fs::read(root.join(binary)).unwrap()).unwrap();
    let admin=Keypair::new_from_array([46;32]);s.airdrop(&admin.pubkey(),10_000_000_000).unwrap();let owner=Pubkey::new_unique();
    let(config,cb)=if venue {pda(&[b"venue-config",Pubkey::new_unique().as_ref()])}else{pda(&[b"config"])};
    let pool=p::venue::pool_address(config);
    let wallet_pda=|seed:&[u8]|if venue {pda(&[seed,config.as_ref(),owner.as_ref()])}else{pda(&[seed,owner.as_ref()])};
    let(trader,tb)=wallet_pda(b"er-trader");let(custody,bb)=wallet_pda(b"er-custody");
    let(guard,gb)=wallet_pda(b"er-collateral-guard");
    let mut t=ErTrader{owner,config,pool,next_nonce:35,bump:tb,
        core_stake:p::stake_backing::CoreStakeState{tokens:49_000_000,acknowledged_debits:71,incurred_usd:19,repaid_usd:0},..ErTrader::default()};
    let mut c=ErCustody{owner,config,pool,bump:bb,
        core_coverage:p::stake_backing::CoreCoverageState{debited_tokens:71,incurred_usd:19,repaid_usd:0},..ErCustody::default()};
    for i in 0..16 {
        let asset=if i==0 {p::stake_backing::core_asset()}else{Pubkey::new_unique()};
        t.assets[i]=ErBalance{asset,available:1_000_000,credited_deposits:50_000_000,credited_returns:93,..ErBalance::default()};
        c.assets[i]=CustodyAsset{asset,available:50_000_000,deposited_credits:50_000_000,returned_credits:93,..CustodyAsset::default()};
    }
    let mut g=p::CollateralGuard{owner,config,floors:p::CollateralGuard::empty_floors(),bump:gb};
    for i in 0..8 {c.receipts[i]=ErReceipt{nonce:i as u64+1,position:Pubkey::new_unique(),kind:ER_TRADE,settled:true};
        g.floors[i]=p::CollateralFloor{nonce:i as u64+1,value:800,entry_price:95,closed_at:86,..p::CollateralFloor::default()};}
    if open {
        for (i,leverage,long) in [(0,5,false),(9,10,true)] {
            t.slots[i]=ErSlot{state:ER_OPEN,kind:ER_TRADE,nonce:20+i as u64,asset:t.assets[i].asset,market:2,
                margin:12345,entry_value:70_000_000,entry:100_000_000_000,bust:if long {90_000_000_000}else{120_000_000_000},
                leverage,long,opened_at:123456,opened_source_slot:98765,cursor:88,bust_observed:i==9,..ErSlot::default()};
            t.notional+=t.slots[i].entry_value*leverage as u64;
            g.floors[i]=p::CollateralFloor{nonce:t.slots[i].nonce,value:67_000_000,entry_price:95,cursor:51,..p::CollateralFloor::default()};
        }
    }
    let full=[bytes(&t),bytes(&c),bytes(&g)];let old=if old_ten {
        [full[0][..full[0].len()-64].to_vec(),full[1][..full[1].len()-48].to_vec(),full[2].clone()]
    }else if previous_fifty {
        let wide=|data:&[u8],prefix:usize,item:usize| {
            let mut b=data[..prefix+16*64].to_vec();b.resize(prefix+51*64,0);
            // Preserve a token held in the highest previous asset record.
            let last=b[prefix+15*64..prefix+16*64].to_vec();b[prefix+15*64..prefix+16*64].fill(0);
            b[prefix+50*64..prefix+51*64].copy_from_slice(&last);
            let at=prefix+16*64;b.extend_from_slice(&data[at..at+ER_SLOTS*item]);
            b.resize(prefix+51*64+50*item,0);b.extend_from_slice(&data[at+ER_SLOTS*item..]);b
        };
        let mut guard=full[2][..full[2].len()-1].to_vec();guard.resize(72+50*p::CollateralFloor::INIT_SPACE,0);guard.push(gb);
        [wide(&full[0][..full[0].len()-64],120,ErSlot::INIT_SPACE),wide(&full[1][..full[1].len()-48],104,ErReceipt::INIT_SPACE),guard]
    }else{[legacy_ledger(&full[0],8+96+16,ErSlot::INIT_SPACE,33),
        legacy_ledger(&full[1],8+96,ErReceipt::INIT_SPACE,25),legacy(full[2].clone(),8+64,p::CollateralFloor::INIT_SPACE)]};
    let keys=[trader,custody,guard];let mut originals=vec![];
    for i in 0..3 {let account=Account{lamports:s.minimum_balance_for_rent_exemption(old[i].len()),data:old[i].clone(),owner:p::ID,executable:false,rent_epoch:0};
        s.set_account(keys[i],account.clone()).unwrap();originals.push(account);}
    let mut cfg=p::Config{admin:admin.pubkey(),publisher:admin.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:72};
    let put_config=|s:&mut LiteSVM,cfg:&p::Config|{let data=bytes(cfg);s.set_account(config,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();};
    put_config(&mut s,&cfg);
    let ix=Instruction{program_id:p::ID,accounts:p::accounts::MigrateWalletCapacity{admin:admin.pubkey(),payer:admin.pubkey(),config,owner,er_trader:trader,custody,guard,
        system_program:Pubkey::default(),execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}}.to_account_metas(None),data:p::instruction::MigrateWalletCapacity{}.data()};
    send(&mut s,&admin,ix.clone(),Some("Paused"));cfg.paused=true;put_config(&mut s,&cfg);
    // Stale copies on base cannot be resized until both delegations finish.
    for i in [0,2] {let mut delegated=originals[i].clone();delegated.owner=p::basebridge::DELEGATION_OWNER;s.set_account(keys[i],delegated).unwrap();
        send(&mut s,&admin,ix.clone(),Some("ConstraintOwner"));s.set_account(keys[i],originals[i].clone()).unwrap();}
    let asset_bytes=if previous_fifty {51*64}else if old_ten {16*64}else{16*56};
    let mut active=originals[0].clone();active.data[120+asset_bytes]=ER_PENDING;s.set_account(trader,active).unwrap();
    send(&mut s,&admin,ix.clone(),Some("Closed"));s.set_account(trader,originals[0].clone()).unwrap();
    let mut pending=originals[1].clone();pending.data[104+asset_bytes+41]=0;s.set_account(custody,pending).unwrap();
    send(&mut s,&admin,ix.clone(),Some("Closed"));s.set_account(custody,originals[1].clone()).unwrap();
    if previous_fifty {
        let mut tail=originals[0].clone();tail.data[120+asset_bytes+49*ErSlot::INIT_SPACE]=ER_OPEN;s.set_account(trader,tail).unwrap();
        send(&mut s,&admin,ix.clone(),Some("Closed"));s.set_account(trader,originals[0].clone()).unwrap();
        let mut tail=originals[1].clone();let at=104+asset_bytes+49*ErReceipt::INIT_SPACE;
        tail.data[at..at+8].copy_from_slice(&800u64.to_le_bytes());s.set_account(custody,tail).unwrap();
        send(&mut s,&admin,ix.clone(),Some("Closed"));s.set_account(custody,originals[1].clone()).unwrap();
        let mut mixed=originals[2].clone();mixed.data=full[2].clone();s.set_account(guard,mixed).unwrap();
        send(&mut s,&admin,ix.clone(),Some("Binding"));s.set_account(guard,originals[2].clone()).unwrap();
    }
    cfg.admin=Pubkey::new_unique();put_config(&mut s,&cfg);send(&mut s,&admin,ix.clone(),Some("ConstraintHasOne"));
    cfg.admin=admin.pubkey();put_config(&mut s,&cfg);
    send(&mut s,&admin,ix.clone(),None);
    let mut migrated=vec![];
    for i in 0..3 {let after=s.get_account(&keys[i]).unwrap();assert_eq!(after.data,full[i]);assert_eq!(after.owner,p::ID);
        assert_eq!(after.lamports,originals[i].lamports.max(s.minimum_balance_for_rent_exemption(full[i].len())));migrated.push(after);}
    send(&mut s,&admin,ix,None);for i in 0..3 {assert_eq!(s.get_account(&keys[i]).unwrap(),migrated[i]);}
}

#[test]
fn returned_legacy_records_cannot_be_redelegated_before_migration(){
    let mut s=LiteSVM::new();let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary=std::env::var("ACIDPUMP_TEST_BINARY").expect("explicit candidate binary required");
    s.add_program(p::ID,&std::fs::read(root.join(binary)).unwrap()).unwrap();
    let payer=Keypair::new_from_array([47;32]);s.airdrop(&payer.pubkey(),10_000_000_000).unwrap();
    let owner=payer.pubkey();let (config,_)=pda(&[b"config"]);let(pool,_)=pda(&[b"pool",&0u16.to_le_bytes()]);
    let(trader,tb)=pda(&[b"er-trader",owner.as_ref()]);let(guard,gb)=pda(&[b"er-collateral-guard",owner.as_ref()]);
    let t=ErTrader{owner,config,pool,bump:tb,..ErTrader::default()};
    let g=p::CollateralGuard{owner,config,floors:p::CollateralGuard::empty_floors(),bump:gb};
    // Delegation contexts bind the trader's config (root or created market).
    let config_bump=pda(&[b"config"]).1;
    let cfg=p::Config{admin:Pubkey::new_unique(),publisher:Pubkey::new_unique(),quote_mint:Pubkey::new_unique(),paused:true,bump:config_bump,next_settlement_seq:1};
    s.set_account(config,Account{lamports:s.minimum_balance_for_rent_exemption(bytes(&cfg).len()),data:bytes(&cfg),owner:p::ID,executable:false,rent_epoch:0}).unwrap();
    let old_trader=legacy_ledger(&bytes(&t),8+96+16,ErSlot::INIT_SPACE,33);
    let old_guard=legacy(bytes(&g),8+64,p::CollateralFloor::INIT_SPACE);
    let delegation=p::basebridge::DELEGATION_OWNER;
    // The layout rejection must occur before the delegation CPI. This account
    // only satisfies address lookup; no DLP execution is claimed by this test.
    s.set_account(delegation,Account{lamports:1_000_000,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
    let aux=|key:Pubkey| (pda(&[b"buffer",key.as_ref()]).0,
        Pubkey::find_program_address(&[b"delegation",key.as_ref()],&delegation).0,
        Pubkey::find_program_address(&[b"delegation-metadata",key.as_ref()],&delegation).0);
    let(buffer,record,metadata)=aux(trader);
    let trader_ix=Instruction{program_id:p::ID,accounts:p::accounts::DelegateErTrader{payer:owner,owner,config,er_trader:trader,
        buffer_er_trader:buffer,delegation_record_er_trader:record,delegation_metadata_er_trader:metadata,
        owner_program:p::ID,delegation_program:delegation,system_program:Pubkey::default()}.to_account_metas(None),data:p::instruction::ErDelegateTrader{}.data()};
    let(buffer,record,metadata)=aux(guard);
    let guard_ix=Instruction{program_id:p::ID,accounts:p::accounts::DelegateCollateralGuard{payer:owner,owner,config,guard,
        buffer_guard:buffer,delegation_record_guard:record,delegation_metadata_guard:metadata,
        owner_program:p::ID,delegation_program:delegation,system_program:Pubkey::default()}.to_account_metas(None),data:p::instruction::ErDelegateCollateralGuard{}.data()};
    for(key,old,full,ix) in [(trader,old_trader,bytes(&t),trader_ix),(guard,old_guard,bytes(&g),guard_ix)] {
        let mut oversized=full.clone();oversized.push(0);
        for data in [old,full[..full.len()-1].to_vec(),oversized] {
            s.set_account(key,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
            send(&mut s,&payer,ix.clone(),Some("Binding"));
        }
        s.set_account(key,Account{lamports:s.minimum_balance_for_rent_exemption(full.len()),data:full,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
        send(&mut s,&payer,ix,Some("Paused"));
    }
}

#[test]
fn paused_admin_redelegates_current_idle_trader_with_backing_using_captured_mainnet_dlp(){
    redelegate_fixture(false,false);
}
#[test]
fn paused_admin_redelegates_preserved_open_trades_with_captured_mainnet_dlp(){
    redelegate_fixture(true,false);redelegate_fixture(true,true);
}
fn redelegate_fixture(open:bool,venue:bool){
    let mut s=LiteSVM::new();let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary=std::env::var("ACIDPUMP_TEST_BINARY").expect("explicit candidate binary required");
    let dlp=std::env::var("ACIDPUMP_TEST_DLP_BINARY").expect("explicit captured mainnet DLP required");
    s.add_program(p::ID,&std::fs::read(root.join(binary)).unwrap()).unwrap();
    let delegation=p::basebridge::DELEGATION_OWNER;
    s.add_program(delegation,&std::fs::read(root.join(dlp)).unwrap()).unwrap();
    let admin=Keypair::new_from_array([48;32]);s.airdrop(&admin.pubkey(),10_000_000_000).unwrap();
    let owner=Pubkey::new_unique(); // No owner private key or signature is used.
    let(config,cb)=if venue {pda(&[b"venue-config",Pubkey::new_unique().as_ref()])}else{pda(&[b"config"])};
    let pool=p::venue::pool_address(config);
    let(trader,tb)=if venue {pda(&[b"er-trader",config.as_ref(),owner.as_ref()])}else{pda(&[b"er-trader",owner.as_ref()])};
    let mut t=ErTrader{owner,config,pool,next_nonce:800,bump:tb,..ErTrader::default()};
    t.assets[0]=ErBalance{asset:p::stake_backing::core_asset(),credited_deposits:9100,available:100,credited_returns:22,generation:8};
    t.core_stake.tokens=9000;t.custody_generation=8;
    if open {
        t.notional=350;
        t.slots[0]=ErSlot{state:ER_OPEN,kind:ER_TRADE,nonce:5,asset:t.assets[0].asset,entry_value:70,
            margin:90,entry:100,bust:120,leverage:5,opened_at:123,cursor:45,bust_observed:true,..ErSlot::default()};
    }
    let original=bytes(&t);
    let put=|s:&mut LiteSVM,key:Pubkey,data:Vec<u8>|s.set_account(key,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
    put(&mut s,trader,original.clone());
    let mut cfg=p::Config{admin:admin.pubkey(),publisher:Pubkey::new_unique(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:5};
    put(&mut s,config,bytes(&cfg));
    let buffer=pda(&[b"buffer",trader.as_ref()]).0;
    let record=Pubkey::find_program_address(&[b"delegation",trader.as_ref()],&delegation).0;
    let metadata=Pubkey::find_program_address(&[b"delegation-metadata",trader.as_ref()],&delegation).0;
    let ix=Instruction{program_id:p::ID,accounts:p::accounts::RedelegateCoreTrader{admin:admin.pubkey(),payer:admin.pubkey(),config,owner,er_trader:trader,
        buffer_er_trader:buffer,delegation_record_er_trader:record,delegation_metadata_er_trader:metadata,
        owner_program:p::ID,delegation_program:delegation,system_program:Pubkey::default()}.to_account_metas(None),data:p::instruction::RedelegateCoreTrader{}.data()};
    send(&mut s,&admin,ix.clone(),Some("Paused"));cfg.paused=true;put(&mut s,config,bytes(&cfg));
    cfg.admin=Pubkey::new_unique();put(&mut s,config,bytes(&cfg));send(&mut s,&admin,ix.clone(),Some("ConstraintHasOne"));
    cfg.admin=admin.pubkey();put(&mut s,config,bytes(&cfg));
    t.slots[ER_SLOTS-1].state=ER_OPEN;put(&mut s,trader,bytes(&t));send(&mut s,&admin,ix.clone(),Some("Closed"));
    t.slots[ER_SLOTS-1].state=ER_FREE;t.notional=1;put(&mut s,trader,bytes(&t));send(&mut s,&admin,ix.clone(),Some("Closed"));
    t.notional=0;t.owner=Pubkey::new_unique();put(&mut s,trader,bytes(&t));send(&mut s,&admin,ix.clone(),Some("Binding"));t.owner=owner;
    put(&mut s,trader,legacy_ledger(&bytes(&t),120,ErSlot::INIT_SPACE,33));send(&mut s,&admin,ix.clone(),Some("Binding"));
    put(&mut s,trader,original.clone());send(&mut s,&admin,ix,None);
    let held=s.get_account(&trader).unwrap();assert_eq!(held.owner,delegation);assert_eq!(held.data,original);
    let delegated=s.get_account(&record).unwrap();assert_eq!(delegated.owner,delegation);
    assert_eq!(&delegated.data[8..40],ER_VALIDATOR.as_ref());
    // This executes the current DLP CPI but not a live validator's asynchronous transport.
}

#[test]
fn capacity_undelegation_callback_preserves_old_and_new_records_with_default_heap(){
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary=std::env::var("ACIDPUMP_TEST_BINARY").expect("explicit candidate binary required");
    let owner=Pubkey::new_unique();let config=pda(&[b"config"]).0;
    let(trader,tb)=pda(&[b"er-trader",owner.as_ref()]);let(guard,gb)=pda(&[b"er-collateral-guard",owner.as_ref()]);
    let mut t=ErTrader{owner,config,bump:tb,next_nonce:91,..ErTrader::default()};t.core_stake.tokens=123456;
    let g=p::CollateralGuard{owner,config,floors:p::CollateralGuard::empty_floors(),bump:gb};
    for(key,seed,full,old)in[(trader,b"er-trader".to_vec(),bytes(&t),legacy_ledger(&bytes(&t),120,ErSlot::INIT_SPACE,33)),
        (guard,b"er-collateral-guard".to_vec(),bytes(&g),legacy(bytes(&g),72,p::CollateralFloor::INIT_SPACE))] {
        for data in [old,full] {
            // Only the authenticated callback signer is injected at the DLP boundary.
            let mut s=LiteSVM::new().with_sigverify(false);
            s.add_program(p::ID,&std::fs::read(root.join(&binary)).unwrap()).unwrap();
            let payer=Keypair::new_from_array([49;32]);s.airdrop(&payer.pubkey(),1_000_000_000).unwrap();
            let buffer=Pubkey::find_program_address(&[b"undelegate-buffer",key.as_ref()],&p::basebridge::DELEGATION_OWNER).0;
            s.set_account(buffer,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data:data.clone(),owner:p::basebridge::DELEGATION_OWNER,executable:false,rent_epoch:0}).unwrap();
            let mut accounts=p::accounts::InitializeAfterUndelegation{base_account:key,buffer,payer:payer.pubkey(),system_program:Pubkey::default()}.to_account_metas(None);
            accounts.iter_mut().find(|a|a.pubkey==buffer).unwrap().is_signer=true;
            accounts.iter_mut().find(|a|a.pubkey==payer.pubkey()).unwrap().is_signer=true;
            let ix=Instruction{program_id:p::ID,accounts,data:p::instruction::ProcessUndelegation{account_seeds:vec![seed.clone(),owner.to_bytes().to_vec()]}.data()};
            let message=VersionedMessage::V0(v0::Message::try_compile(&payer.pubkey(),&[ix],&[],s.latest_blockhash()).unwrap());
            let tx=VersionedTransaction{signatures:vec![Default::default();message.header().num_required_signatures as usize],message};
            let meta=s.send_transaction(tx).unwrap_or_else(|e|panic!("default-heap callback {} bytes failed: {e:?}",data.len()));
            println!("default-heap capacity callback {} bytes: {} CU",data.len(),meta.compute_units_consumed);
            let restored=s.get_account(&key).unwrap();assert_eq!(restored.owner,p::ID);assert_eq!(restored.data,data);
        }
    }
}
