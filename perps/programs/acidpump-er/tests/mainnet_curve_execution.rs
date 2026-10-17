#![cfg(feature="mainnet")]
//! Official SDK transactions against captured Pump mainnet SBF in a local bank.
//! Synthetic, unfunded test signers; this is not yet the platform's settlement CPI.
use acidpump_er::mainnet_curve;
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
use std::{path::Path,str::FromStr};
fn pk(s:&str)->Pubkey{Pubkey::from_str(s).unwrap()}
fn key(v:&Value,k:&str)->Pubkey{pk(v[k].as_str().unwrap())}
fn amount(s:&LiteSVM,k:Pubkey)->u64{u64::from_le_bytes(s.get_account(&k).unwrap().data[64..72].try_into().unwrap())}

#[test]
fn captured_pump_program_executes_regular_and_mayhem_curve_sells() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bytes=std::fs::read(root.join(".cache/production-remediation-candidate/curve-execution.json")).unwrap();
    let f:Value=serde_json::from_slice(&bytes).unwrap();
    let source=std::fs::read(root.join(f["source"].as_str().unwrap())).unwrap();
    assert_eq!(format!("{:x}",Sha256::digest(source)),f["sourceSha256"].as_str().unwrap());
    let program:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-pump-curve-binary.json")).unwrap()).unwrap();
    let binary=std::fs::read(root.join(program["file"].as_str().unwrap())).unwrap();
    assert_eq!(format!("{:x}",Sha256::digest(&binary)),program["sha256"].as_str().unwrap());
    assert_eq!(key(&program,"program"),mainnet_curve::ID);
    let mut receipts=vec![];
    for row in f["swaps"].as_array().unwrap() {
        let mut svm=LiteSVM::new();
        let rent_bytes=STANDARD.decode(f["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=Rent{lamports_per_byte_year:u64::from_le_bytes(rent_bytes[..8].try_into().unwrap()),exemption_threshold:f64::from_le_bytes(rent_bytes[8..16].try_into().unwrap()),burn_percent:rent_bytes[16]};
        svm.set_sysvar(&rent);
        for(address,account) in f["accounts"].as_object().unwrap(){
            if address.starts_with("Sysvar"){continue;}
            svm.set_account(pk(address),Account{lamports:account["lamports"].as_u64().unwrap(),owner:key(account,"owner"),data:STANDARD.decode(account["data"].as_str().unwrap()).unwrap(),executable:false,rent_epoch:0}).unwrap();
        }
        let clock_bytes=STANDARD.decode(f["accounts"]["SysvarC1ock11111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        let mut clock:Clock=svm.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=i64::from_le_bytes(clock_bytes[32..40].try_into().unwrap());svm.set_sysvar(&clock);
        svm.add_program(mainnet_curve::ID,&binary).unwrap();
        let user=Keypair::new_from_array([119;32]);let payer=Keypair::new_from_array([120;32]);assert_eq!(user.pubkey(),key(&f,"user"));
        for address in [user.pubkey(),payer.pubkey(),key(row,"buyback")] {
            svm.set_account(address,Account{lamports:10_000_000_000,owner:Pubkey::default(),data:vec![],executable:false,rent_epoch:0}).unwrap();
        }
        let input=row["amount"].as_str().unwrap().parse::<u64>().unwrap();let expected=row["expected"].as_str().unwrap().parse::<u64>().unwrap();
        let mint=key(row,"mint");let base=key(row,"userBase");let curve=key(row,"curve");let volume=key(row,"volume");
        let mut data=vec![0u8;165];data[..32].copy_from_slice(mint.as_ref());data[32..64].copy_from_slice(user.pubkey().as_ref());data[64..72].copy_from_slice(&(input+123).to_le_bytes());data[108]=1;
        svm.set_account(base,Account{lamports:rent.minimum_balance(data.len()),owner:key(&f["accounts"][mint.to_string()],"owner"),data,executable:false,rent_epoch:0}).unwrap();
        let v=&row["instruction"];
        let trade=Instruction{program_id:key(v,"program"),data:STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),accounts:v["keys"].as_array().unwrap().iter().map(|a|AccountMeta{pubkey:key(a,"key"),is_writable:a["writable"].as_bool().unwrap(),is_signer:a["signer"].as_bool().unwrap()}).collect()};
        let mut instructions=vec![];
        for(tag,value)in [(1,128*1024u32),(2,400_000u32)]{let mut data=vec![tag];data.extend_from_slice(&value.to_le_bytes());instructions.push(Instruction{program_id:pk("ComputeBudget111111111111111111111111111111"),accounts:vec![],data});}
        if let Some(preparations)=row["prepare"].as_array(){
            for v in preparations {instructions.push(Instruction{program_id:key(v,"program"),data:STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),accounts:v["keys"].as_array().unwrap().iter().map(|a|AccountMeta{pubkey:key(a,"key"),is_writable:a["writable"].as_bool().unwrap(),is_signer:a["signer"].as_bool().unwrap()}).collect()});}
        }
        instructions.push(trade);
        let before=svm.get_account(&user.pubkey()).unwrap().lamports;
        let curve_size=svm.get_account(&curve).unwrap().data.len();
        let message=Message::new_with_blockhash(&instructions,Some(&payer.pubkey()),&svm.latest_blockhash());
        let tx=VersionedTransaction::try_new(VersionedMessage::Legacy(message),&[&payer,&user]).unwrap();
        let result=svm.send_transaction(tx).unwrap_or_else(|e|panic!("Curve sell {} failed: {e:?}",mint));
        let native=svm.get_account(&user.pubkey()).unwrap().lamports;let volume_rent=svm.get_account(&volume).unwrap().lamports;
        assert_eq!(svm.get_account(&curve).unwrap().data.len(),curve_size,"captured modern curve needs no extension");
        assert_eq!(amount(&svm,base),123,"exact input leaves existing custody dust untouched");
        assert_eq!(native+if row["prepare"].is_array(){0}else{volume_rent},before+expected,"net SOL output is separate from one-time volume-account rent");
        receipts.push(json!({"name":row["name"],"mint":mint.to_string(),"input":input,"outputLamports":expected,"volumeAccountRentLamports":volume_rent,"computeUnits":result.compute_units_consumed}));
    }
    std::fs::write(root.join("reports/acidpump-pump-curve-execution.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-pump-sbf","programSha256":program["sha256"],"fixtureSha256":format!("{:x}",Sha256::digest(&bytes)),
        "scope":"Direct official SDK sell_v3 calls; synthetic user balances, actual captured regular and Mayhem curve state; platform PDA settlement CPI remains pending",
        "transactionsSubmitted":0,"receipts":receipts})).unwrap()).unwrap();
}
