//! Runs captured mainnet PumpSwap + fee program ELF locally with synthetic user
//! balances. This proves route execution, not deployment or the ER lifecycle.
use anchor_lang::prelude::{Clock, Pubkey};
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use base64::{engine::general_purpose::STANDARD, Engine};
use litesvm::LiteSVM;
use serde_json::{json, Value};
use sha2::{Digest,Sha256};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, AddressLookupTableAccount, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{collections::BTreeSet, path::Path, str::FromStr};

fn pk(s: &str) -> Pubkey { Pubkey::from_str(s).unwrap() }
fn key(v: &Value, name: &str) -> Pubkey { pk(v[name].as_str().unwrap()) }
fn quantity(v: &Value, name: &str) -> u64 { v[name].as_str().unwrap().parse().unwrap() }
fn hash(path:impl AsRef<Path>)->String {format!("{:x}",Sha256::digest(std::fs::read(path).unwrap()))}
fn proxy_path()->std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(std::env::var("ACIDPUMP_TEST_PROXY_BINARY")
        .unwrap_or_else(|_|".cache/acidpump-mainnet-execution/acidpump_mainnet_adapter_test.so".to_owned()))
}
fn wire(v: &Value) -> Instruction {
    Instruction { program_id:key(v,"program"), data:STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),
        accounts:v["keys"].as_array().unwrap().iter().map(|k| AccountMeta {pubkey:key(k,"key"),is_writable:k["writable"].as_bool().unwrap(),is_signer:k["signer"].as_bool().unwrap()}).collect() }
}
fn token(svm: &LiteSVM, address: Pubkey) -> u64 {
    u64::from_le_bytes(svm.get_account(&address).unwrap().data[64..72].try_into().unwrap())
}
fn wallet_token(svm: &mut LiteSVM, address: Pubkey, owner: Pubkey, mint: Pubkey, program: Pubkey, amount: u64, native: bool) {
    let mut data = vec![0u8;165];
    data[..32].copy_from_slice(mint.as_ref()); data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes()); data[108]=1;
    let rent = svm.minimum_balance_for_rent_exemption(data.len());
    if native { data[109..113].copy_from_slice(&1u32.to_le_bytes()); data[113..121].copy_from_slice(&rent.to_le_bytes()); }
    svm.set_account(address,Account {lamports:rent+if native {amount}else{0},data,owner:program,executable:false,rent_epoch:0}).unwrap();
}
struct Bank { svm: LiteSVM, signer: Keypair }
impl Bank {
    fn new(f: &Value) -> Self {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.cache/acidpump-mainnet-execution");
        let mut svm=LiteSVM::new();
        for (address,row) in f["accounts"].as_object().unwrap() {
            if address.starts_with("Sysvar") {continue;}
            let data=STANDARD.decode(row["data"].as_str().unwrap()).unwrap();
            svm.set_account(pk(address),Account {lamports:row["lamports"].as_u64().unwrap(),data,owner:key(row,"owner"),executable:false,rent_epoch:0}).unwrap();
        }
        for p in f["programs"].as_array().unwrap() {
            let file=root.join(format!("{}.so",p["name"].as_str().unwrap()));
            assert_eq!(hash(&file),p["sha256"].as_str().unwrap(),"captured program bytes changed");
            svm.add_program(key(p,"program"),&std::fs::read(file).unwrap()).unwrap();
        }
        let signer=Keypair::new_from_array([71;32]); assert_eq!(signer.pubkey(),key(f,"user"));
        // Deterministic test keys may exist on mainnet under another owner.
        // This bank intentionally uses a synthetic system-owned wallet.
        svm.set_account(signer.pubkey(),Account {lamports:100_000_000_000,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
        svm.airdrop(&signer.pubkey(),100_000_000_000).unwrap();
        wallet_token(&mut svm,key(f,"userQuote"),signer.pubkey(),acidpump_er::mainnet_pump::SOL,anchor_spl::token::ID,10_000_000_000,true);
        for row in f["swaps"].as_array().unwrap().iter().filter(|row| !row["buy"].as_bool().unwrap()) {
            wallet_token(&mut svm,key(row,"base"),signer.pubkey(),key(row,"mint"),key(row,"tokenProgram"),1_000_000_000_000,false);
        }
        let mut clock:Clock=svm.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=f["now"].as_i64().unwrap();svm.set_sysvar(&clock);
        Self {svm,signer}
    }
    fn send(&mut self, mut ixs: Vec<Instruction>, reject: bool) -> u64 {
        let compute=pk("ComputeBudget111111111111111111111111111111");
        let mut heap=vec![1];heap.extend_from_slice(&(128*1024u32).to_le_bytes());
        ixs.insert(0,Instruction{program_id:compute,accounts:vec![],data:heap});
        let mut data=vec![2];data.extend_from_slice(&1_400_000u32.to_le_bytes());
        ixs.insert(0,Instruction{program_id:compute,accounts:vec![],data});
        let addresses=ixs.iter().flat_map(|ix| ix.accounts.iter().map(|a| a.pubkey)).collect::<BTreeSet<_>>().into_iter().collect::<Vec<_>>();
        let table=AddressLookupTableAccount {key:Pubkey::new_from_array([91;32]),addresses};
        let mut data=vec![0;56];data[..4].copy_from_slice(&1u32.to_le_bytes());data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        for address in &table.addresses {data.extend_from_slice(address.as_ref());}
        self.svm.set_account(table.key,Account{lamports:100_000_000,data,owner:pk("AddressLookupTab1e1111111111111111111111111"),executable:false,rent_epoch:0}).unwrap();
        self.svm.expire_blockhash();
        let message=v0::Message::try_compile(&self.signer.pubkey(),&ixs,&[table],self.svm.latest_blockhash()).unwrap();
        let tx=VersionedTransaction::try_new(VersionedMessage::V0(message),&[&self.signer]).unwrap();
        let result=self.svm.send_transaction(tx);
        if reject {assert!(result.is_err(),"invalid route unexpectedly executed");return 0;}
        assert!(result.is_ok(),"{result:?}");result.unwrap().compute_units_consumed
    }
}

fn cpi_bank(f:&Value)->(Bank,Pubkey,u8,Pubkey,Pubkey) {
    let mut bank=Bank::new(f);
    let pool=Pubkey::new_from_array([81;32]);
    let (authority,bump)=Pubkey::find_program_address(&[b"authority",pool.as_ref()],&acidpump_er::ID);
    let ata=|mint:Pubkey,program:Pubkey| Pubkey::find_program_address(&[authority.as_ref(),program.as_ref(),mint.as_ref()],&anchor_spl::associated_token::ID).0;
    let quote=ata(acidpump_er::mainnet_pump::SOL,anchor_spl::token::ID);
    bank.svm.airdrop(&authority,100_000_000_000).unwrap();
    wallet_token(&mut bank.svm,quote,authority,acidpump_er::mainnet_pump::SOL,anchor_spl::token::ID,10_000_000_000,true);
    for row in f["swaps"].as_array().unwrap().iter().filter(|row| !row["buy"].as_bool().unwrap()) {
        wallet_token(&mut bank.svm,ata(key(row,"mint"),key(row,"tokenProgram")),authority,key(row,"mint"),key(row,"tokenProgram"),1_000_000_000_000,false);
    }
    let proxy=proxy_path();
    bank.svm.add_program(acidpump_er::ID,&std::fs::read(proxy).expect("Build the local-only mainnet-proxy first")).unwrap();
    (bank,pool,bump,authority,quote)
}
fn cpi_instruction(f:&Value,row:&Value,pool:Pubkey,bump:u8,authority:Pubkey,quote:Pubkey)->Instruction {
    let mut ix=wire(&row["instruction"]);
    let base=Pubkey::find_program_address(&[authority.as_ref(),key(row,"tokenProgram").as_ref(),key(row,"mint").as_ref()],&anchor_spl::associated_token::ID).0;
    let volume=|owner:Pubkey| Pubkey::find_program_address(&[b"user_volume_accumulator",owner.as_ref()],&acidpump_er::pump::ID).0;
    let cashback=|owner:Pubkey| Pubkey::find_program_address(&[volume(owner).as_ref(),anchor_spl::token::ID.as_ref(),acidpump_er::mainnet_pump::SOL.as_ref()],&anchor_spl::associated_token::ID).0;
    for account in &mut ix.accounts {
        for (before,after) in [(key(f,"user"),authority),(key(f,"userQuote"),quote),(key(row,"base"),base),(volume(key(f,"user")),volume(authority)),(cashback(key(f,"user")),cashback(authority))] {
            if account.pubkey==before {account.pubkey=after;}
        }
        account.is_signer=false;
    }
    let mut data=pool.to_bytes().to_vec();data.push(bump);data.extend_from_slice(key(row,"mint").as_ref());data.extend_from_slice(key(row,"pool").as_ref());
    data.push(u8::from(row["buy"].as_bool().unwrap()));data.extend_from_slice(&quantity(row,"amount").to_le_bytes());data.extend_from_slice(&1u64.to_le_bytes());
    Instruction {program_id:acidpump_er::ID,accounts:ix.accounts,data}
}
#[test]
fn compact_v2_routes_match_quotes_preserve_custody_and_reject_redirects() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let file=root.join(".cache/production-remediation-candidate/pumpswap-v2.json");
    let fixtures:Value=serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
    assert_eq!(hash(root.join(fixtures["source"].as_str().unwrap())),fixtures["sourceSha256"].as_str().unwrap());
    let proxy=std::env::var("ACIDPUMP_TEST_PROXY_BINARY").expect("Use the current local-only adapter proxy");
    let mut receipts=vec![];
    for case in fixtures["cases"].as_array().unwrap() {
        let f=&case["fixture"];
        for row in f["swaps"].as_array().unwrap() {
            let (mut bank,pool,bump,authority,quote)=cpi_bank(f);
            let ix=cpi_instruction(f,row,pool,bump,authority,quote);assert_eq!(ix.accounts.len(),17);
            let base=ix.accounts[5].pubkey;let b0=token(&bank.svm,base);let q0=token(&bank.svm,quote);
            let tracked=ix.accounts.iter().map(|a|(a.pubkey,bank.svm.get_account(&a.pubkey))).collect::<Vec<_>>();
            // Every account substitution must reject and roll back pool, fee,
            // volume and custody changes, not merely fail a client assertion.
            for index in 0..17 {
                let mut bad=ix.clone();bad.accounts[index].pubkey=bank.signer.pubkey();bank.send(vec![bad],true);
                for(k,before)in &tracked {assert_eq!(bank.svm.get_account(k),*before,"account changed after rejected substitution {index}");}
            }
            let mut bad=ix.clone();bad.data[106..114].copy_from_slice(&u64::MAX.to_le_bytes());bank.send(vec![bad],true);
            for(k,before)in &tracked {assert_eq!(bank.svm.get_account(k),*before,"slippage failure did not roll back {k}");}
            let dex=key(row,"pool");let old=bank.svm.get_account(&dex).unwrap();
            let mut cashback=old.clone();cashback.data.resize(cashback.data.len().max(245),0);cashback.data[244]=1;
            bank.svm.set_account(dex,cashback).unwrap();bank.send(vec![ix.clone()],true);bank.svm.set_account(dex,old).unwrap();
            let units=bank.send(vec![ix],false);let buy=row["buy"].as_bool().unwrap();
            let (spent,out)=if buy {(q0-token(&bank.svm,quote),token(&bank.svm,base)-b0)}else{(b0-token(&bank.svm,base),token(&bank.svm,quote)-q0)};
            assert!(spent>0&&spent<=quantity(row,"amount"));if !buy {assert_eq!(spent,quantity(row,"amount"));}
            assert_eq!(out,quantity(row,"expected"),"v2 quote mismatch for {} buy={buy}",case["name"]);
            receipts.push(json!({"mode":case["name"],"buy":buy,"spent":spent.to_string(),"received":out.to_string(),"accounts":17,
                "computeUnits":units,"rejectedAccountSubstitutions":17,"cashbackRejected":true,"failedSwapRolledBack":true}));
        }
    }
    std::fs::write(root.join("reports/acidpump-pumpswap-v2-execution.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-pda-cpi","scope":fixtures["scope"],"mainnetTransactionsSubmitted":0,"fixtureSha256":hash(file),
        "proxySha256":hash(root.join(proxy)),"adapterSourceSha256":hash(root.join("programs/acidpump-er/src/mainnet_pump.rs")),"receipts":receipts})).unwrap()).unwrap();
}
#[test]
fn pool_modes_and_historical_layouts_execute_canonical_cpi_routes() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixtures:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/modes.json")).expect("Build offline mainnet mode fixtures first")).unwrap();
    let mut receipts=vec![];
    for case in fixtures["cases"].as_array().unwrap() {
        let f=&case["fixture"];
        for row in f["swaps"].as_array().unwrap() {
            let (mut bank,pool,bump,authority,quote)=cpi_bank(f);
            let ix=cpi_instruction(f,row,pool,bump,authority,quote);
            let base=ix.accounts[5].pubkey;
            let base0=token(&bank.svm,base);let quote0=token(&bank.svm,quote);
            // Every fee, creator, event, volume and optional destination is bound.
            let buy=row["buy"].as_bool().unwrap();
            let fee_index=if buy {21}else{19};
            let mut attack_indices=vec![9,10,15,17,18];
            if buy {attack_indices.extend([19,20]);}
            attack_indices.extend((fee_index+2)..ix.accounts.len());
            for index in &attack_indices {
                let mut bad=ix.clone();bad.accounts[*index].pubkey=authority;
                bank.send(vec![bad],true);
                assert_eq!(token(&bank.svm,base),base0);assert_eq!(token(&bank.svm,quote),quote0);
            }
            let units=bank.send(vec![ix],false);
            let (spent,received)=if buy {(quote0-token(&bank.svm,quote),token(&bank.svm,base)-base0)} else {(base0-token(&bank.svm,base),token(&bank.svm,quote)-quote0)};
            assert!(spent>0 && spent<=quantity(row,"amount"));
            if !buy {assert_eq!(spent,quantity(row,"amount"));}
            assert_eq!(received,quantity(row,"expected"),"variant {} buy={buy}",case["name"]);
            receipts.push(json!({"mode":case["name"],"buy":buy,"spent":spent.to_string(),"received":received.to_string(),"computeUnits":units,"rejectedAccountSubstitutions":attack_indices.len()}));
        }
    }
    std::fs::write(root.join("reports/acidpump-production-pump-modes.json"),serde_json::to_vec_pretty(&json!({
        "scope":fixtures["scope"],"localSimulation":true,"mainnetTransactionsSubmitted":0,"receipts":receipts,
        "fixtureSha256":hash(root.join(".cache/acidpump-mainnet-execution/modes.json")),
        "adapterSourceSha256":hash(root.join("programs/acidpump-er/src/mainnet_pump.rs")),
        "testProxySha256":hash(proxy_path())
    })).unwrap()).unwrap();
}
#[test]
fn production_adapter_cpi_checks_binding_and_custody_deltas() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let f:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/fixture.json")).unwrap()).unwrap();
    let mut receipts=vec![];
    for row in f["swaps"].as_array().unwrap() {
        let (mut bank,pool,bump,authority,quote)=cpi_bank(&f);
        let ix=cpi_instruction(&f,row,pool,bump,authority,quote);
        let base=ix.accounts[5].pubkey;let base0=token(&bank.svm,base);let quote0=token(&bank.svm,quote);
        let mut wrong_mint=ix.clone();wrong_mint.data[33..65].fill(0);
        bank.send(vec![wrong_mint],true);
        let mut wrong_authority=ix.clone();wrong_authority.data[0]^=1;
        bank.send(vec![wrong_authority],true);
        let mut wrong_pool=ix.clone();wrong_pool.data[65..97].fill(0);
        bank.send(vec![wrong_pool],true);
        let mut wrong_source=ix.clone();wrong_source.accounts[5].pubkey=key(row,"base");
        bank.send(vec![wrong_source],true);
        let mut impossible=ix.clone();impossible.data[106..114].copy_from_slice(&u64::MAX.to_le_bytes());
        bank.send(vec![impossible],true);
        assert_eq!(token(&bank.svm,base),base0);assert_eq!(token(&bank.svm,quote),quote0);
        let cu=bank.send(vec![ix],false);
        let (spent,received)=if row["buy"].as_bool().unwrap() {(quote0-token(&bank.svm,quote),token(&bank.svm,base)-base0)} else {(base0-token(&bank.svm,base),token(&bank.svm,quote)-quote0)};
        assert!(spent>0 && spent<=quantity(row,"amount"));
        assert_eq!(received,quantity(row,"expected"));
        receipts.push(json!({"mint":row["mint"],"buy":row["buy"],"input":spent.to_string(),"received":received.to_string(),"computeUnits":cu,"rejectedBindingAndSlippageAttacks":5}));
    }
    std::fs::write(root.join("reports/acidpump-mainnet-adapter-execution.json"),serde_json::to_vec_pretty(&json!({
        "localSimulation":true,"mainnetTransactionsSubmitted":0,"slot":f["slot"],"programs":f["programs"],"receipts":receipts,
        "fixtureSha256":hash(root.join(".cache/acidpump-mainnet-execution/fixture.json")),
        "adapterSourceSha256":hash(root.join("programs/acidpump-er/src/mainnet_pump.rs")),
        "testProxySha256":hash(proxy_path()),
        "scope":"Production mainnet swap helper via local-only PDA proxy; full platform settlement and ER lifecycle remain untested"
    })).unwrap()).unwrap();
}
#[test]
fn mainnet_programs_execute_sdice_sol_and_usdc_routes_and_rollback_failed_hop() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let f:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/fixture.json")).expect("Run capture-mainnet-swaps.ts first")).unwrap();
    let mut receipts=vec![];
    for row in f["swaps"].as_array().unwrap() {
        let mut bank=Bank::new(&f);
        let base=key(row,"base");let quote=key(&f,"userQuote");
        let b0=token(&bank.svm,base);let q0=token(&bank.svm,quote);
        let cu=bank.send(vec![wire(&row["instruction"])],false);
        let (spent,received)=if row["buy"].as_bool().unwrap() {(q0-token(&bank.svm,quote),token(&bank.svm,base)-b0)} else {(b0-token(&bank.svm,base),token(&bank.svm,quote)-q0)};
        assert!(spent>0 && spent<=quantity(row,"amount"));
        if !row["buy"].as_bool().unwrap() {assert_eq!(spent,quantity(row,"amount"));}
        assert_eq!(received,quantity(row,"expected"),"SDK vs executed receipt: {row}");
        receipts.push(json!({"mint":row["mint"],"buy":row["buy"],"input":spent.to_string(),"received":received.to_string(),"computeUnits":cu}));
    }
    // Sell SDICE, then buy USDC in one atomic transaction. Failed second legs
    // must return the source bag and all fees by rolling back the first leg.
    for reverse in [false,true] {
        let mut bank=Bank::new(&f);
        let swaps=f["swaps"].as_array().unwrap();
        let source=&swaps[if reverse {2}else{0}];let destination=&swaps[if reverse {1}else{3}];
        let source_key=key(source,"base");let dest_key=key(destination,"base");let quote=key(&f,"userQuote");
        let source0=token(&bank.svm,source_key);let dest0=token(&bank.svm,dest_key);let q0=token(&bank.svm,quote);
        let first=wire(&source["instruction"]);let mut second=wire(&destination["instruction"]);
        second.data[8..16].copy_from_slice(&quantity(source,"expected").to_le_bytes());
        second.data[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        bank.send(vec![first.clone(),second.clone()],true);
        assert_eq!(token(&bank.svm,source_key),source0);assert_eq!(token(&bank.svm,dest_key),dest0);assert_eq!(token(&bank.svm,quote),q0);
        second.data[16..24].copy_from_slice(&1u64.to_le_bytes());
        let cu=bank.send(vec![first,second],false);
        assert_eq!(source0-token(&bank.svm,source_key),quantity(source,"amount"));
        assert!(token(&bank.svm,dest_key)>dest0);
        assert!(token(&bank.svm,quote)>=q0,"route consumed pre-existing SOL");
        receipts.push(json!({"route":if reverse {"USDC-SOL-SDICE"}else{"SDICE-SOL-USDC"},"received":(token(&bank.svm,dest_key)-dest0).to_string(),"computeUnits":cu,"failedSecondHopRolledBack":true}));
    }
    std::fs::write(root.join("reports/acidpump-mainnet-swap-execution.json"),serde_json::to_vec_pretty(&json!({
        "localSimulation":true,"mainnetTransactionsSubmitted":0,"slot":f["slot"],"programs":f["programs"],"receipts":receipts,
        "fixtureSha256":hash(root.join(".cache/acidpump-mainnet-execution/fixture.json")),
        "scope":"Real captured PumpSwap and fee program SBF with synthetic wallet balances; not Acidpump custody or ER lifecycle"
    })).unwrap()).unwrap();
}
