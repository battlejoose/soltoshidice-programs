#![cfg(feature="mainnet")]
//! Actual captured Pump SBF, invoked by the local-only production-helper proxy.
//! Does not prove final platform settlement integration or submit mainnet trades.
use acidpump_er::{self as p,mainnet_curve};
use anchor_lang::{prelude::{Clock,Pubkey,Rent},solana_program::instruction::{AccountMeta,Instruction}};
use base64::{engine::general_purpose::STANDARD,Engine};
use litesvm::LiteSVM;
use serde_json::{json,Value};
use sha2::{Digest,Sha256};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{Message,VersionedMessage};
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
    for(tag,n)in [(1,128*1024u32),(2,500_000)]{let mut data=vec![tag];data.extend_from_slice(&n.to_le_bytes());instructions.insert(0,Instruction{program_id:pk("ComputeBudget111111111111111111111111111111"),accounts:vec![],data});}
    s.expire_blockhash();let message=Message::new_with_blockhash(&instructions,Some(&payer.pubkey()),&s.latest_blockhash());
    let tx=VersionedTransaction::try_new(VersionedMessage::Legacy(message),&[payer]).unwrap();
    assert!(1+tx.signatures.len()*64+tx.message.serialize().len()<=1232,"fixture exceeds packet size");
    let result=s.send_transaction(tx);
    if reject{
        let error=result.expect_err("invalid token-curve custody operation executed");
        assert!(error.meta.logs.iter().any(|l|l==&format!("Program {} invoke [1]",p::ID)),"must reach the custody helper: {error:?}");
        let reason=format!("{:?}",error.err);
        assert!(["Custom(6002)","Custom(6004)","Custom(2000)"].iter().any(|c|reason.contains(c)),"must reject binding, quote or Pump writable constraint: {error:?}");
        for(k,before)in touched{if k!=payer.pubkey(){assert_eq!(s.get_account(&k),before,"failed operation changed {k}");}}0
    }else{result.unwrap_or_else(|e|panic!("token-curve custody execution failed: {e:?}")).compute_units_consumed}
}
#[test]
fn token_curve_custody_uses_exact_quote_tokens_and_separate_setup_rent(){
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes=std::fs::read(root.join(".cache/production-remediation-candidate/token-curve-custody.json")).unwrap();
    let f:Value=serde_json::from_slice(&bytes).unwrap();
    for(file,hash)in f["sources"].as_object().unwrap(){assert_eq!(digest(&std::fs::read(root.join(file)).unwrap()),hash.as_str().unwrap());}
    assert_eq!(key(&f,"program"),p::ID);
    let pump_report:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-pump-curve-binary.json")).unwrap()).unwrap();
    let pump=std::fs::read(root.join(pump_report["file"].as_str().unwrap())).unwrap();assert_eq!(digest(&pump),pump_report["sha256"].as_str().unwrap());
    let proxy=std::fs::read(root.join(".cache/production-remediation-candidate/acidpump_mainnet_adapter_test.so")).unwrap();
    let old:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/fixture.json")).unwrap()).unwrap();
    let fees=std::fs::read(root.join(".cache/acidpump-mainnet-execution/fees.so")).unwrap();
    assert_eq!(digest(&fees),old["programs"].as_array().unwrap().iter().find(|r|r["name"]=="fees").unwrap()["sha256"].as_str().unwrap());
    let payer=Keypair::new_from_array([120;32]);assert_eq!(payer.pubkey(),key(&f,"payer"));let authority=key(&f,"authority");let mut receipts=vec![];
    for row in f["cases"].as_array().unwrap(){
        println!("token curve {}",row["name"]);
        let mut s=LiteSVM::new();let raw=STANDARD.decode(row["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=Rent{lamports_per_byte_year:u64::from_le_bytes(raw[..8].try_into().unwrap()),exemption_threshold:f64::from_le_bytes(raw[8..16].try_into().unwrap()),burn_percent:raw[16]};s.set_sysvar(&rent);
        for(address,a)in row["accounts"].as_object().unwrap(){if !address.starts_with("Sysvar"){
            s.set_account(pk(address),Account{lamports:a["lamports"].as_u64().unwrap(),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),owner:key(a,"owner"),executable:false,rent_epoch:0}).unwrap();
        }}
        let raw=STANDARD.decode(row["accounts"]["SysvarC1ock11111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        let mut clock:Clock=s.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=i64::from_le_bytes(raw[32..40].try_into().unwrap());s.set_sysvar(&clock);
        s.add_program(mainnet_curve::ID,&pump).unwrap();s.add_program(mainnet_curve::FEES_ID,&fees).unwrap();s.add_program(p::ID,&proxy).unwrap();
        let cashback=row["cashback"].as_bool().unwrap();
        for address in [payer.pubkey(),authority,key(row,"buyback")].into_iter().chain(if cashback{Some(key(row,"recipient"))}else{None}){
            s.set_account(address,Account{lamports:10_000_000_000,owner:Pubkey::default(),data:vec![],executable:false,rent_epoch:0}).unwrap();
        }
        let mint=key(row,"mint");let base=key(row,"base");let quote=key(row,"quote");let curve=key(row,"curve");let volume=key(row,"volume");
        token_account(&mut s,base,authority,mint,key(&row["accounts"][mint.to_string()],"owner"),n(row,"amount")+123);
        token_account(&mut s,quote,authority,key(row,"quoteMint"),key(row,"quoteProgram"),n(row,"initialQuote"));
        let native0=s.get_account(&authority).unwrap().lamports;let trade=wire(&row["instruction"]);
        send(&mut s,&payer,vec![trade.clone()],true);
        let prep:Vec<_>=row["preparation"].as_array().unwrap().iter().map(wire).collect();
        // Prepare in separate bounded transactions. All rent belongs to the service.
        for ix in &prep{send(&mut s,&payer,vec![ix.clone()],false);}
        let balances:Vec<_>=prep.iter().flat_map(|i|i.accounts.iter().map(|a|a.pubkey)).collect::<BTreeSet<_>>().into_iter().filter(|k|*k!=payer.pubkey()).map(|k|(k,s.get_account(&k))).collect();
        for ix in &prep{send(&mut s,&payer,vec![ix.clone()],false);}
        for(k,before)in balances{assert_eq!(s.get_account(&k),before,"setup repeats must not add rent or reset counters");}
        assert_eq!(s.get_account(&authority).unwrap().lamports,native0);assert!(s.get_account(&curve).unwrap().data.len()>=166);
        for index in 0..trade.accounts.len(){let mut bad=trade.clone();bad.accounts[index].pubkey=payer.pubkey();send(&mut s,&payer,vec![bad],true);}
        let mut bad=trade.clone();bad.data[106..114].copy_from_slice(&(n(row,"expected")+1).to_le_bytes());send(&mut s,&payer,vec![bad],true);
        let mut bad=trade.clone();bad.data[114..146].copy_from_slice(mint.as_ref());send(&mut s,&payer,vec![bad],true);
        let mut bad=trade.clone();bad.accounts[if cashback{10}else{5}].is_writable=false;send(&mut s,&payer,vec![bad],true);
        // An unexpected rent top-up inside Pump must roll back, rather than
        // silently drawing operating SOL and understating settlement cost.
        let curve_before=s.get_account(&curve).unwrap();let mut underfunded=curve_before.clone();underfunded.lamports=1;s.set_account(curve,underfunded).unwrap();
        send(&mut s,&payer,vec![trade.clone()],true);s.set_account(curve,curve_before).unwrap();
        let units=send(&mut s,&payer,vec![trade.clone()],false);
        assert_eq!(token(&s,base),123);assert_eq!(token(&s,quote),n(row,"initialQuote")+n(row,"expected"));
        assert_eq!(s.get_account(&authority).unwrap().lamports,native0,"swap must not pay setup or draw operating SOL");
        let v=s.get_account(&volume).unwrap();
        receipts.push(json!({"name":row["name"],"captured":row["captured"],"quoteMint":row["quoteMint"],"input":row["amount"],"receivedQuoteAtoms":row["expected"],
            "computeUnits":units,"accounts":trade.accounts.len(),"rejectedSubstitutions":trade.accounts.len(),"preservedNativeLamports":native0,
            "volumeBytes":v.data.len(),"volumeData":STANDARD.encode(&v.data)}));
    }
    assert_eq!(receipts.len(),14);
    std::fs::write(root.join("reports/acidpump-token-curve-custody.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-pump-sbf","fixtureSha256":digest(&bytes),"proxySha256":digest(&proxy),"pumpSha256":digest(&pump),
        "transactionsSubmitted":0,"scope":"Custody helper only; synthetic and captured quote-token state, local bank, actual Pump SBF. Platform settlement integration and live rollout remain pending.","receipts":receipts})).unwrap()).unwrap();
}
