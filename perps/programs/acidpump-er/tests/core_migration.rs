#![cfg(not(feature="mainnet"))]
use ::acidpump_er::{self as p,Config,ErTrader,ErCustody,ErBalance,CustodyAsset,Position,ER_OPEN,stake_backing};
use anchor_lang::{prelude::{Pubkey,Space},AccountSerialize,AccountDeserialize,InstructionData,ToAccountMetas,solana_program::instruction::Instruction};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_message::{v0,VersionedMessage};
use solana_transaction::versioned::VersionedTransaction;
fn key(seeds:&[&[u8]])->Pubkey{Pubkey::find_program_address(seeds,&p::ID).0}
fn put<T:AccountSerialize>(s:&mut LiteSVM,key:Pubkey,value:&T,trim:usize){
    let mut data=vec![];value.try_serialize(&mut data).unwrap();data.truncate(data.len()-trim);
    s.set_account(key,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
}
fn send(s:&mut LiteSVM,payer:&Keypair,ix:Instruction,ok:bool){
    s.expire_blockhash();
    let mut heap=vec![1];heap.extend_from_slice(&(128u32*1024).to_le_bytes());
    let compute=Instruction{program_id:"ComputeBudget111111111111111111111111111111".parse().unwrap(),accounts:vec![],data:heap};
    let msg=v0::Message::try_compile(&payer.pubkey(),&[compute,ix],&[],s.latest_blockhash()).unwrap();
    let result=s.send_transaction(VersionedTransaction::try_new(VersionedMessage::V0(msg),&[payer]).unwrap());
    assert_eq!(result.is_ok(),ok,"{result:?}");
}
#[test]
fn paused_migration_preserves_every_existing_byte_and_is_idempotent(){
    let mut s=LiteSVM::new();s.add_program(p::ID,&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),"/../../target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let payer=Keypair::new_from_array([17;32]);let admin=payer.pubkey();s.airdrop(&admin,1_000_000_000).unwrap();
    let config=key(&[b"config"]);let owner=Pubkey::new_unique();let pool=key(&[b"pool",&0u16.to_le_bytes()]);
    let cb=Pubkey::find_program_address(&[b"config"],&p::ID).1;
    let mut c=Config{admin,publisher:admin,quote_mint:Pubkey::new_unique(),paused:false,bump:cb,next_settlement_seq:9};put(&mut s,config,&c,0);
    let trader=key(&[b"er-trader",owner.as_ref()]);let custody=key(&[b"er-custody",owner.as_ref()]);let position=key(&[b"position",owner.as_ref(),&7u64.to_le_bytes()]);
    let mut t=ErTrader{owner,config,pool,next_nonce:12,..ErTrader::default()};t.assets[0]=ErBalance{asset:stake_backing::core_asset(),available:1_234_567,credited_deposits:9_999_999,credited_returns:44};
    let mut u=ErCustody{owner,config,pool,..ErCustody::default()};u.assets[0]=CustodyAsset{asset:stake_backing::core_asset(),available:1_234_567,deposited_credits:9_999_999,returned_credits:44};
    let mut x=Position::try_deserialize_unchecked(&mut vec![0;8+Position::INIT_SPACE].as_slice()).unwrap();x.owner=owner;x.pool=pool;x.nonce=7;x.state=1;x.profit=123;x.remaining=87;
    put(&mut s,trader,&t,32);put(&mut s,custody,&u,24);put(&mut s,position,&x,32);
    let instruction=|target,kind|Instruction{program_id:p::ID,accounts:p::accounts::MigrateCoreAccount{admin,payer:admin,config,target,system_program:Pubkey::default(),execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}}.to_account_metas(None),data:p::instruction::MigrateCoreAccount{kind}.data()};
    let before=s.get_account(&trader).unwrap();send(&mut s,&payer,instruction(trader,0),false);assert_eq!(before.data,s.get_account(&trader).unwrap().data);
    c.paused=true;put(&mut s,config,&c,0);
    for(k,kind,size)in[(trader,0,8+ErTrader::INIT_SPACE),(custody,1,8+ErCustody::INIT_SPACE),(position,2,8+Position::INIT_SPACE)] {
        let before=s.get_account(&k).unwrap();send(&mut s,&payer,instruction(k,kind),true);let after=s.get_account(&k).unwrap();
        assert_eq!(after.data.len(),size);assert_eq!(&after.data[..before.data.len()],before.data.as_slice());
        assert!(after.data[before.data.len()..].iter().all(|b|*b==0));
        send(&mut s,&payer,instruction(k,kind),true);assert_eq!(after,s.get_account(&k).unwrap());
    }
    let got=ErTrader::try_deserialize(&mut s.get_account(&trader).unwrap().data.as_slice()).unwrap();assert_eq!(got.assets[0].available,1_234_567);assert_eq!(got.core_stake.tokens,0);
    let wrong=Pubkey::new_unique();put(&mut s,wrong,&t,32);send(&mut s,&payer,instruction(wrong,0),false);
    t.slots[0].state=ER_OPEN;put(&mut s,trader,&t,32);send(&mut s,&payer,instruction(trader,0),false);
}

#[test]
fn magicblock_undelegation_callback_works_with_default_32k_heap(){
    for extra in [0,32] {
        // The DLP buffer signer is injected at this CPI boundary. Signature
        // verification is off only to represent that program-derived signer.
        let mut s=LiteSVM::new().with_sigverify(false);
        s.add_program(p::ID,&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),"/../../target/deploy/acidpump_er.so")).unwrap()).unwrap();
        let payer=Keypair::new_from_array([19;32]);s.airdrop(&payer.pubkey(),1_000_000_000).unwrap();
        let owner=Pubkey::new_unique();let trader=key(&[b"er-trader",owner.as_ref()]);
        let buffer=Pubkey::find_program_address(&[b"undelegate-buffer",trader.as_ref()],&ephemeral_rollups_sdk::id()).0;
        let t=ErTrader{owner,next_nonce:17,..ErTrader::default()};let mut data=vec![];t.try_serialize(&mut data).unwrap();data.truncate(data.len()-extra);
        s.set_account(buffer,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data:data.clone(),owner:ephemeral_rollups_sdk::id(),executable:false,rent_epoch:0}).unwrap();
        let mut accounts=p::accounts::InitializeAfterUndelegation{base_account:trader,buffer,payer:payer.pubkey(),system_program:Pubkey::default()}.to_account_metas(None);
        accounts.iter_mut().find(|a|a.pubkey==buffer).unwrap().is_signer=true;
        accounts.iter_mut().find(|a|a.pubkey==payer.pubkey()).unwrap().is_signer=true;
        let ix=Instruction{program_id:p::ID,accounts,data:p::instruction::ProcessUndelegation{account_seeds:vec![b"er-trader".to_vec(),owner.to_bytes().to_vec()]}.data()};
        // No RequestHeapFrame: this reproduces the live MagicBlock callback.
        let message=VersionedMessage::V0(v0::Message::try_compile(&payer.pubkey(),&[ix],&[],s.latest_blockhash()).unwrap());
        let tx=VersionedTransaction{signatures:vec![Default::default();message.header().num_required_signatures as usize],message};
        let result=s.send_transaction(tx);assert!(result.is_ok(),"default-heap callback failed: {result:?}");
        let restored=s.get_account(&trader).unwrap();assert_eq!(restored.owner,p::ID);assert_eq!(restored.data,data);
    }
}

#[test]
#[ignore = "Requires captured public devnet DLP program and metadata in CORE_DLP_FIXTURE_DIR"]
fn owner_request_recovers_interrupted_undelegation_without_changing_balance_bytes(){
    use base64::Engine;
    let dir=std::env::var("CORE_DLP_FIXTURE_DIR").expect("public DLP fixture directory");
    let fixture:serde_json::Value=serde_json::from_slice(&std::fs::read(format!("{dir}/dlp-fixture.json")).unwrap()).unwrap();
    let mut s=LiteSVM::new();
    s.add_program(p::ID,&std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"),"/../../target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let dlp=ephemeral_rollups_sdk::id();s.add_program(dlp,&std::fs::read(format!("{dir}/dlp.so")).unwrap()).unwrap();
    let payer=Keypair::new_from_array([21;32]);let admin=payer.pubkey();s.airdrop(&admin,1_000_000_000).unwrap();
    let owner:Pubkey=fixture["owner"].as_str().unwrap().parse().unwrap();
    let trader=key(&[b"er-trader",owner.as_ref()]);let config=key(&[b"config"]);
    for (i,row) in fixture["accounts"].as_array().unwrap().iter().enumerate(){
        let mut data=base64::engine::general_purpose::STANDARD.decode(row["data"].as_str().unwrap()).unwrap();
        // Only substitute the original rent payer so this offline fixture can sign.
        if i==2 {let at=data.len()-32;data[at..].copy_from_slice(admin.as_ref());}
        s.set_account(row["key"].as_str().unwrap().parse().unwrap(),Account{lamports:row["lamports"].as_u64().unwrap(),data,owner:dlp,executable:false,rent_epoch:0}).unwrap();
    }
    let request=Pubkey::find_program_address(&[b"undelegation-request",trader.as_ref()],&dlp).0;
    let mut c=Config{admin,publisher:admin,quote_mint:Pubkey::new_unique(),paused:false,bump:Pubkey::find_program_address(&[b"config"],&p::ID).1,next_settlement_seq:0};put(&mut s,config,&c,0);
    let ix=Instruction{program_id:p::ID,accounts:p::accounts::RequestCoreUndelegation{admin,payer:admin,config,owner,er_trader:trader,owner_program:p::ID,undelegation_request:request,delegation_record:fixture["accounts"][1]["key"].as_str().unwrap().parse().unwrap(),delegation_metadata:fixture["accounts"][2]["key"].as_str().unwrap().parse().unwrap(),delegation_program:dlp,system_program:Pubkey::default(),execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}}.to_account_metas(None),data:p::instruction::RequestCoreUndelegation{}.data()};
    let before=s.get_account(&trader).unwrap();send(&mut s,&payer,ix.clone(),false);assert!(s.get_account(&request).is_none());
    c.paused=true;put(&mut s,config,&c,0);
    let mut unauthorized=ix.clone();unauthorized.accounts[0].pubkey=Pubkey::new_unique();unauthorized.accounts[0].is_signer=false;send(&mut s,&payer,unauthorized,false);
    send(&mut s,&payer,ix.clone(),true);assert_eq!(before,s.get_account(&trader).unwrap());assert_eq!(s.get_account(&request).unwrap().owner,dlp);
    send(&mut s,&payer,ix,true);assert_eq!(before,s.get_account(&trader).unwrap());
    let state=Pubkey::find_program_address(&[b"state-diff",trader.as_ref()],&dlp).0;
    let record=Pubkey::find_program_address(&[b"commit-state-record",trader.as_ref()],&dlp).0;
    let request_accounts=p::accounts::RequestCoreUndelegation{admin,payer:admin,config,owner,er_trader:trader,owner_program:p::ID,undelegation_request:request,delegation_record:fixture["accounts"][1]["key"].as_str().unwrap().parse().unwrap(),delegation_metadata:fixture["accounts"][2]["key"].as_str().unwrap().parse().unwrap(),delegation_program:dlp,system_program:Pubkey::default(),execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}};
    let accounts=p::accounts::RecoverCoreTrader{request:request_accounts,commit_state:state,commit_record:record}.to_account_metas(None);
    let digest=ephemeral_rollups_sdk::dlp_api::solana_program::hash::hash(&before.data).to_bytes();
    let recovery=|expected_hash|Instruction{program_id:p::ID,accounts:accounts.clone(),data:p::instruction::RecoverCoreTrader{expected_hash}.data()};
    send(&mut s,&payer,recovery(digest),false);assert_eq!(before,s.get_account(&trader).unwrap());
    s.warp_to_slot(10_000);
    send(&mut s,&payer,recovery([0;32]),false);
    s.set_account(state,Account{lamports:1_000_000,data:vec![1;8],owner:dlp,executable:false,rent_epoch:0}).unwrap();
    send(&mut s,&payer,recovery(digest),false);assert_eq!(before,s.get_account(&trader).unwrap());
    s.set_account(state,Account{lamports:0,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
    send(&mut s,&payer,recovery(digest),true);
    let after=s.get_account(&trader).unwrap();assert_eq!(after.owner,p::ID);assert_eq!(after.data,before.data);assert_eq!(after.lamports,before.lamports);
}
