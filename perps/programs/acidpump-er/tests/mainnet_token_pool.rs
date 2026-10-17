#![cfg(feature="mainnet")]
//! Token-denominated custody CPI through captured PumpSwap SBF, not a mocked DEX.
use acidpump_er as p;
use anchor_lang::{prelude::{Clock,Pubkey},solana_program::instruction::{AccountMeta,Instruction}};
use base64::{engine::general_purpose::STANDARD,Engine};
use litesvm::LiteSVM;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0,AddressLookupTableAccount,VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{collections::BTreeSet,path::Path,str::FromStr};
fn pk(s:&str)->Pubkey{Pubkey::from_str(s).unwrap()}
fn key(v:&Value,k:&str)->Pubkey{pk(v[k].as_str().unwrap())}
fn n(v:&Value,k:&str)->u64{v[k].as_str().unwrap().parse().unwrap()}
fn digest(b:&[u8])->String{format!("{:x}",Sha256::digest(b))}
fn token(s:&LiteSVM,k:Pubkey)->u64{u64::from_le_bytes(s.get_account(&k).unwrap().data[64..72].try_into().unwrap())}
fn wire(v:&Value)->Instruction{Instruction{program_id:key(v,"program"),data:STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),
    accounts:v["keys"].as_array().unwrap().iter().map(|a|AccountMeta{pubkey:key(a,"key"),is_writable:a["writable"].as_bool().unwrap(),is_signer:a["signer"].as_bool().unwrap()}).collect()}}
fn token_account(s:&mut LiteSVM,k:Pubkey,owner:Pubkey,mint:Pubkey,program:Pubkey,amount:u64){
    let mut data=vec![0;165];data[..32].copy_from_slice(mint.as_ref());data[32..64].copy_from_slice(owner.as_ref());data[64..72].copy_from_slice(&amount.to_le_bytes());data[108]=1;
    s.set_account(k,Account{lamports:s.minimum_balance_for_rent_exemption(165),data,owner:program,executable:false,rent_epoch:0}).unwrap();
}
fn send(s:&mut LiteSVM,payer:&Keypair,mut instructions:Vec<Instruction>,reject:bool)->u64{
    let touched:Vec<_>=instructions.iter().flat_map(|i|i.accounts.iter().map(|a|a.pubkey)).collect::<BTreeSet<_>>().into_iter().map(|k|(k,s.get_account(&k))).collect();
    for(tag,n)in [(1,128*1024u32),(2,700_000)]{let mut data=vec![tag];data.extend_from_slice(&n.to_le_bytes());instructions.insert(0,Instruction{program_id:pk("ComputeBudget111111111111111111111111111111"),accounts:vec![],data});}
    let table=AddressLookupTableAccount{key:Pubkey::new_from_array([91;32]),addresses:touched.iter().map(|(k,_)|*k).collect()};
    let mut data=vec![0;56];data[..4].copy_from_slice(&1u32.to_le_bytes());data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    for k in &table.addresses{data.extend_from_slice(k.as_ref());}
    s.set_account(table.key,Account{lamports:100_000_000,data,owner:pk("AddressLookupTab1e1111111111111111111111111"),executable:false,rent_epoch:0}).unwrap();
    s.expire_blockhash();let message=v0::Message::try_compile(&payer.pubkey(),&instructions,&[table],s.latest_blockhash()).unwrap();
    let tx=VersionedTransaction::try_new(VersionedMessage::V0(message),&[payer]).unwrap();
    assert!(1+tx.signatures.len()*64+tx.message.serialize().len()<=1232,"fixture exceeds packet size");
    let result=s.send_transaction(tx);
    if reject{
        let error=result.expect_err("invalid token-pool custody operation executed");
        assert!(error.meta.logs.iter().any(|l|l==&format!("Program {} invoke [1]",p::ID)),"must reach custody helper: {error:?}");
        for(k,before)in touched{if k!=payer.pubkey(){assert_eq!(s.get_account(&k),before,"failed operation changed {k}");}}0
    }else{result.unwrap_or_else(|e|panic!("token-pool custody execution failed: {e:?}")).compute_units_consumed}
}
#[test]
fn token_pools_execute_both_directions_without_drawing_operating_sol(){
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes=std::fs::read(root.join(".cache/production-remediation-candidate/token-pool-custody.json")).unwrap();
    let f:Value=serde_json::from_slice(&bytes).unwrap();assert_eq!(key(&f,"program"),p::ID);
    assert_eq!(digest(&std::fs::read(root.join(f["source"].as_str().unwrap())).unwrap()),f["sourceSha256"].as_str().unwrap());
    let proxy=std::fs::read(root.join(".cache/production-remediation-candidate/acidpump_mainnet_adapter_test.so")).unwrap();
    let binaries:Vec<_>=f["programs"].as_array().unwrap().iter().map(|a|{
        let bytes=std::fs::read(root.join(format!(".cache/acidpump-mainnet-execution/{}.so",a["name"].as_str().unwrap()))).unwrap();
        assert_eq!(digest(&bytes),a["sha256"].as_str().unwrap());(key(a,"program"),bytes)
    }).collect();
    let payer=Keypair::new_from_array([120;32]);assert_eq!(payer.pubkey(),key(&f,"payer"));let authority=key(&f,"authority");let mut receipts=vec![];
    for row in f["cases"].as_array().unwrap(){for trade_row in row["swaps"].as_array().unwrap(){
        println!("token pool {} buy={}",row["name"],trade_row["buy"]);
        let mut s=LiteSVM::new();
        for(address,a)in row["accounts"].as_object().unwrap(){if !address.starts_with("Sysvar"){
            s.set_account(pk(address),Account{lamports:a["lamports"].as_u64().unwrap(),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),owner:key(a,"owner"),executable:false,rent_epoch:0}).unwrap();
        }}
        let mut clock:Clock=s.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=f["now"].as_i64().unwrap();s.set_sysvar(&clock);
        for(k,b)in &binaries{s.add_program(*k,b).unwrap();}s.add_program(p::ID,&proxy).unwrap();
        for address in [payer.pubkey(),authority]{s.set_account(address,Account{lamports:10_000_000_000,owner:Pubkey::default(),data:vec![],executable:false,rent_epoch:0}).unwrap();}
        let base=key(row,"base");let quote=key(row,"quote");let buy=trade_row["buy"].as_bool().unwrap();
        let initial_base=if buy{123}else{n(trade_row,"amount")+123};let initial_quote=if buy{n(trade_row,"amount")+456}else{456};
        token_account(&mut s,base,authority,key(row,"mint"),key(row,"baseProgram"),initial_base);
        token_account(&mut s,quote,authority,key(row,"quoteMint"),key(row,"quoteProgram"),initial_quote);
        let trade=wire(&trade_row["instruction"]);let native0=s.get_account(&authority).unwrap().lamports;
        send(&mut s,&payer,vec![trade.clone()],true);
        let prep:Vec<_>=row["preparation"].as_array().unwrap().iter().map(wire).collect();
        for ix in &prep{send(&mut s,&payer,vec![ix.clone()],false);}
        let tracked:Vec<_>=prep.iter().flat_map(|i|i.accounts.iter().map(|a|a.pubkey)).collect::<BTreeSet<_>>().into_iter().filter(|k|*k!=payer.pubkey()).map(|k|(k,s.get_account(&k))).collect();
        for ix in &prep{send(&mut s,&payer,vec![ix.clone()],false);}
        for(k,before)in tracked{assert_eq!(s.get_account(&k),before,"setup must be idempotent");}
        assert_eq!(s.get_account(&authority).unwrap().lamports,native0);
        for index in 0..trade.accounts.len(){let mut bad=trade.clone();bad.accounts[index].pubkey=payer.pubkey();send(&mut s,&payer,vec![bad],true);}
        let mut bad=trade.clone();bad.data[106..114].copy_from_slice(&(n(trade_row,"expected")+1).to_le_bytes());send(&mut s,&payer,vec![bad],true);
        let mut bad=trade.clone();bad.data[114..146].copy_from_slice(key(row,"mint").as_ref());send(&mut s,&payer,vec![bad],true);
        let mut bad=trade.clone();bad.accounts[0].is_writable=false;send(&mut s,&payer,vec![bad],true);
        // Real quote-vault state is part of the authenticated route; a frozen
        // account, substituted mint or retained-fee deficit cannot be priced.
        let quote_vault=trade.accounts[8].pubkey;let vault_before=s.get_account(&quote_vault).unwrap();
        let mut frozen=vault_before.clone();frozen.data[108]=2;s.set_account(quote_vault,frozen).unwrap();send(&mut s,&payer,vec![trade.clone()],true);
        let mut wrong_mint=vault_before.clone();wrong_mint.data[..32].copy_from_slice(key(row,"mint").as_ref());s.set_account(quote_vault,wrong_mint).unwrap();send(&mut s,&payer,vec![trade.clone()],true);
        s.set_account(quote_vault,vault_before).unwrap();
        let dex=key(row,"pool");let pool_before=s.get_account(&dex).unwrap();
        let mut overdrawn=pool_before.clone();overdrawn.data[271..279].copy_from_slice(&u64::MAX.to_le_bytes());s.set_account(dex,overdrawn).unwrap();send(&mut s,&payer,vec![trade.clone()],true);s.set_account(dex,pool_before).unwrap();
        let units=send(&mut s,&payer,vec![trade.clone()],false);
        let (spent,received)=if buy{(initial_quote-token(&s,quote),token(&s,base)-initial_base)}else{(initial_base-token(&s,base),token(&s,quote)-initial_quote)};
        assert!(spent>0&&spent<=n(trade_row,"amount"));if !buy{assert_eq!(spent,n(trade_row,"amount"));}
        assert_eq!(received,n(trade_row,"expected"));assert_eq!(s.get_account(&authority).unwrap().lamports,native0);
        assert!(token(&s,base)>=123&&token(&s,quote)>=456);
        receipts.push(json!({"name":row["name"],"buy":buy,"quoteMint":row["quoteMint"],"spent":spent.to_string(),"received":received.to_string(),
            "computeUnits":units,"accounts":trade.accounts.len(),"rejectedSubstitutions":trade.accounts.len(),"nativeBalanceUnchanged":true,"setupIdempotent":true}));
    }}
    assert_eq!(receipts.len(),56);
    std::fs::write(root.join("reports/acidpump-token-pool-custody.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-pumpswap-sbf","fixtureSha256":digest(&bytes),"proxySha256":digest(&proxy),"transactionsSubmitted":0,
        "scope":f["scope"],"receipts":receipts})).unwrap()).unwrap();
}
