#![cfg(feature="mainnet")]
//! Actual production helper via the local-only PDA proxy and captured Pump SBF.
//! This proves the swap boundary, not yet full position/ER settlement integration.
use acidpump_er::{self as p,mainnet_curve,mainnet_pump::SOL};
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
fn wire(v:&Value)->Instruction {
    Instruction{program_id:key(v,"program"),data:STANDARD.decode(v["data"].as_str().unwrap()).unwrap(),
        accounts:v["keys"].as_array().unwrap().iter().map(|a|AccountMeta{pubkey:key(a,"key"),is_writable:a["writable"].as_bool().unwrap(),is_signer:a["signer"].as_bool().unwrap()}).collect()}
}
fn token_account(s:&mut LiteSVM,k:Pubkey,owner:Pubkey,mint:Pubkey,program:Pubkey,amount:u64,donation:u64) {
    let mut data=vec![0;165];data[..32].copy_from_slice(mint.as_ref());data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());data[108]=1;
    let rent=s.minimum_balance_for_rent_exemption(165);
    if mint==SOL {data[109..113].copy_from_slice(&1u32.to_le_bytes());data[113..121].copy_from_slice(&rent.to_le_bytes());}
    s.set_account(k,Account{lamports:rent+if mint==SOL{amount+donation}else{0},data,owner:program,executable:false,rent_epoch:0}).unwrap();
}
fn send(s:&mut LiteSVM,payer:&Keypair,mut instructions:Vec<Instruction>,reject:bool)->u64 {
    let read_only_curve=instructions.iter().any(|i|i.program_id==p::ID && i.accounts.get(if i.accounts.len()==27{10}else{5}).is_some_and(|a|!a.is_writable));
    let touched:Vec<_>=instructions.iter().flat_map(|i|i.accounts.iter().map(|a|a.pubkey)).collect::<BTreeSet<_>>().into_iter().map(|k|(k,s.get_account(&k))).collect();
    let compute=pk("ComputeBudget111111111111111111111111111111");
    for(tag,n)in [(1,128*1024u32),(2,400_000)]{let mut data=vec![tag];data.extend_from_slice(&n.to_le_bytes());instructions.insert(0,Instruction{program_id:compute,accounts:vec![],data});}
    s.expire_blockhash();
    let message=Message::new_with_blockhash(&instructions,Some(&payer.pubkey()),&s.latest_blockhash());
    let tx=VersionedTransaction::try_new(VersionedMessage::Legacy(message),&[payer]).unwrap();
    // Conservative exact legacy wire length: short-vector counts here are <128.
    assert!(1+tx.signatures.len()*64+tx.message.serialize().len()<=1232);
    let result=s.send_transaction(tx);
    if reject {
        let error=result.expect_err("invalid curve route executed");
        // The intentionally minimal proxy returns Anchor's numeric errors,
        // without the full platform entrypoint's descriptive error logging.
        let reason=format!("{:?}",error.err);
        if read_only_curve {
            assert!(reason.contains("Custom(2000)") && error.meta.logs.iter().any(|l|l.contains("Instruction: SyncNative"))
                && error.meta.logs.iter().any(|l|l.contains("Instruction: SellV")),"must fail in Pump after wrapping sync: {error:?}");
        }else {assert!(["Custom(6002)","Custom(6004)"].iter().any(|code|reason.contains(code)),"must reject inside the adapter: {error:?}");}
        for(k,before)in touched {if k!=payer.pubkey(){assert_eq!(s.get_account(&k),before,"failed transaction changed {k}");}}
        0
    } else {result.unwrap_or_else(|e|panic!("curve custody execution failed: {e:?}")).compute_units_consumed}
}

#[test]
fn curve_custody_preserves_operating_sol_wraps_exact_proceeds_and_rejects_substitution() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture_bytes=std::fs::read(root.join(".cache/production-remediation-candidate/curve-custody.json")).unwrap();
    let f:Value=serde_json::from_slice(&fixture_bytes).unwrap();
    assert_eq!(digest(&std::fs::read(root.join(f["source"].as_str().unwrap())).unwrap()),f["sourceSha256"].as_str().unwrap());
    assert_eq!(key(&f,"program"),p::ID);
    let report:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-pump-curve-binary.json")).unwrap()).unwrap();
    let pump=std::fs::read(root.join(report["file"].as_str().unwrap())).unwrap();assert_eq!(digest(&pump),report["sha256"].as_str().unwrap());
    let proxy=std::fs::read(root.join(".cache/production-remediation-candidate/acidpump_mainnet_adapter_test.so")).unwrap();
    let payer=Keypair::new_from_array([120;32]);assert_eq!(payer.pubkey(),key(&f,"payer"));
    let authority=key(&f,"authority");let mut receipts=vec![];
    for row in f["cases"].as_array().unwrap() {
        let cashback=row["cashback"].as_bool().unwrap_or(false);let count=if cashback{27}else{18};
        let mut s=LiteSVM::new();
        let raw=STANDARD.decode(row["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=Rent{lamports_per_byte_year:u64::from_le_bytes(raw[..8].try_into().unwrap()),exemption_threshold:f64::from_le_bytes(raw[8..16].try_into().unwrap()),burn_percent:raw[16]};s.set_sysvar(&rent);
        for(address,a)in row["accounts"].as_object().unwrap(){if !address.starts_with("Sysvar") {
            s.set_account(pk(address),Account{lamports:a["lamports"].as_u64().unwrap(),owner:key(a,"owner"),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),executable:false,rent_epoch:0}).unwrap();
        }}
        let raw=STANDARD.decode(row["accounts"]["SysvarC1ock11111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        let mut clock:Clock=s.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=i64::from_le_bytes(raw[32..40].try_into().unwrap());s.set_sysvar(&clock);
        s.add_program(mainnet_curve::ID,&pump).unwrap();s.add_program(p::ID,&proxy).unwrap();
        let old:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/fixture.json")).unwrap()).unwrap();
        let fee=std::fs::read(root.join(".cache/acidpump-mainnet-execution/fees.so")).unwrap();
        let capture=old["programs"].as_array().unwrap().iter().find(|p|p["name"]=="fees").unwrap();
        assert_eq!(digest(&fee),capture["sha256"].as_str().unwrap());s.add_program(mainnet_curve::FEES_ID,&fee).unwrap();
        for address in [payer.pubkey(),authority,key(row,"buyback")] {s.set_account(address,Account{lamports:10_000_000_000,owner:Pubkey::default(),data:vec![],executable:false,rent_epoch:0}).unwrap();}
        if cashback {s.set_account(key(row,"recipient"),Account{lamports:10_000_000_000,owner:Pubkey::default(),data:vec![],executable:false,rent_epoch:0}).unwrap();}
        let mint=key(row,"mint");let base=key(row,"base");let quote=key(row,"quote");let curve=key(row,"curve");
        token_account(&mut s,base,authority,mint,key(&row["accounts"][mint.to_string()],"owner"),n(row,"amount")+123,0);
        token_account(&mut s,quote,authority,SOL,anchor_spl::token::ID,n(row,"initialWrapped"),n(row,"unsyncedDonation"));
        let trade=wire(&row["instruction"]);let native0=s.get_account(&authority).unwrap().lamports;
        send(&mut s,&payer,vec![trade.clone()],true); // Unfunded setup cannot eat proceeds.
        let mut prep:Vec<_>=row["preparation"].as_array().unwrap().iter().map(wire).collect();
        prep.push(prep.last().unwrap().clone()); // The separate-payer init is idempotent.
        send(&mut s,&payer,prep,false);
        assert_eq!(s.get_account(&authority).unwrap().lamports,native0,"service pays setup");
        assert!(s.get_account(&curve).unwrap().data.len()>=166,"historical extension reaches the current layout");
        for index in 0..count {
            let mut bad=trade.clone();bad.accounts[index].pubkey=payer.pubkey();send(&mut s,&payer,vec![bad],true);
        }
        // Fail inside Pump's CPI after our sync_native ran. The earlier sync,
        // including an unsolicited donation, must roll back with the whole tx.
        let mut readonly=trade.clone();readonly.accounts[if cashback{10}else{5}].is_writable=false;send(&mut s,&payer,vec![readonly],true);
        let mut bad=trade.clone();bad.data[106..114].copy_from_slice(&(n(row,"expected")+1).to_le_bytes());send(&mut s,&payer,vec![bad],true);
        if !cashback {
            let original=s.get_account(&curve).unwrap();let mut altered=original.clone();altered.data[82]=1;s.set_account(curve,altered).unwrap();
            send(&mut s,&payer,vec![trade.clone()],true);s.set_account(curve,original).unwrap();
        }
        let original=s.get_account(&quote).unwrap();let mut nonnative=original.clone();nonnative.data[109..113].fill(0);s.set_account(quote,nonnative).unwrap();
        send(&mut s,&payer,vec![trade.clone()],true);s.set_account(quote,original).unwrap();
        let units=send(&mut s,&payer,vec![trade],false);
        assert_eq!(token(&s,base),123);
        assert_eq!(token(&s,quote),n(row,"initialWrapped")+n(row,"unsyncedDonation")+n(row,"expected"));
        assert_eq!(s.get_account(&authority).unwrap().lamports,native0);
        receipts.push(json!({"name":row["name"],"mint":mint.to_string(),"input":row["amount"],"receivedLamports":row["expected"],
            "computeUnits":units,"cashback":cashback,
            "rejectedAccountSubstitutions":count,"preservedOperatingLamports":native0,"volumeRentLamports":s.get_account(&key(row,"volume")).unwrap().lamports}));
    }
    std::fs::write(root.join("reports/acidpump-pump-curve-custody.json"),serde_json::to_vec_pretty(&json!({
        "status":"passed-local-pda-cpi","fixtureSha256":digest(&fixture_bytes),"proxySha256":digest(&proxy),"pumpSha256":digest(&pump),
        "adapterSourceSha256":digest(&std::fs::read(root.join("programs/acidpump-er/src/mainnet_curve.rs")).unwrap()),
        "scope":"Production curve-sell helper via local-only PDA proxy; captured regular/Mayhem state plus synthetic historical SPL layout; full settlement entrypoint and graduation pending",
        "mainnetTransactionsSubmitted":0,"receipts":receipts})).unwrap()).unwrap();
}
