#![cfg(feature="mainnet")]
//! Full mainnet candidate base settlement against captured Pump SBF programs.
//! Platform accounts are initialized by real instructions; the DLP commit boundary is injected.
//! Real token deposits, settlement, rewards, USDC payouts and withdrawals execute.
//! Administrative signatures are simulated without loading private keys.
//! This is not proof of native ER transport or a live deployment.
use acidpump_er::{self as p, engine::*, mainnet_pump::{SOL,USDC}, ownership::OwnershipTrader,
    Asset,CollateralFloor,CollateralGuard,Config,GasReserve,Inventory,OwnershipPool,Pool,Position};
use anchor_lang::{prelude::{Clock,Pubkey},AccountDeserialize,AccountSerialize,Discriminator,InstructionData,ToAccountMetas,
    solana_program::instruction::{AccountMeta,Instruction}};
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
const UNIT:u64=1_000_000;
const PX:u64=1_000_000_000_000;
fn candidate_binary()->String {std::env::var("ACIDPUMP_TEST_BINARY").unwrap_or_else(|_|".cache/acidpump-mainnet-candidate/acidpump_er.so".into())}
fn client_fixture_path()->String {std::env::var("ACIDPUMP_TEST_CLIENT_FIXTURE").unwrap_or_else(|_|".cache/acidpump-mainnet-client-instructions.json".into())}
fn report_path(name:&str)->String {if let Ok(dir)=std::env::var("ACIDPUMP_TEST_REPORT_DIR") {format!("{dir}/{name}.json")}else if cfg!(feature="mainnet") {format!("reports/acidpump-production-{name}.json")}else{format!("reports/acidpump-mainnet-{name}.json")}}
#[path="support/mainnet_bootstrap.rs"]
mod bootstrap;
#[path="support/mainnet_usdc_settlement.rs"]
mod usdc_settlement;
#[path="support/mainnet_economics.rs"]
mod economics;
#[path="support/mainnet_venues.rs"]
mod venues;
fn pk(s:&str)->Pubkey {Pubkey::from_str(s).unwrap()}
fn key(v:&Value,n:&str)->Pubkey {pk(v[n].as_str().unwrap())}
fn pda(s:&[&[u8]])->Pubkey {Pubkey::find_program_address(s,&p::ID).0}
fn bump(s:&[&[u8]])->u8 {Pubkey::find_program_address(s,&p::ID).1}
fn hash(path:impl AsRef<Path>)->String {format!("{:x}",Sha256::digest(std::fs::read(path).unwrap()))}
fn ix(a:impl ToAccountMetas,d:impl InstructionData)->Instruction {Instruction{program_id:p::ID,accounts:a.to_account_metas(None),data:d.data()}}
fn put<T:AccountSerialize>(s:&mut LiteSVM,k:Pubkey,v:&T,owner:Pubkey) {
    let mut data=vec![];v.try_serialize(&mut data).unwrap();
    s.set_account(k,Account{lamports:s.minimum_balance_for_rent_exemption(data.len()),data,owner,executable:false,rent_epoch:0}).unwrap();
}
fn get<T:AccountDeserialize>(s:&LiteSVM,k:Pubkey)->T {T::try_deserialize(&mut &s.get_account(&k).unwrap().data[..]).unwrap()}
fn token(s:&LiteSVM,k:Pubkey)->u64 {u64::from_le_bytes(s.get_account(&k).unwrap().data[64..72].try_into().unwrap())}
fn supply(s:&LiteSVM,k:Pubkey)->u64 {u64::from_le_bytes(s.get_account(&k).unwrap().data[36..44].try_into().unwrap())}
fn token_account(s:&mut LiteSVM,k:Pubkey,owner:Pubkey,mint:Pubkey,program:Pubkey,amount:u64) {
    let mut data=vec![0;165];data[..32].copy_from_slice(mint.as_ref());data[32..64].copy_from_slice(owner.as_ref());
    data[64..72].copy_from_slice(&amount.to_le_bytes());data[108]=1;
    let rent=s.minimum_balance_for_rent_exemption(165);let native=mint==SOL;
    if native {data[109..113].copy_from_slice(&1u32.to_le_bytes());data[113..121].copy_from_slice(&rent.to_le_bytes());}
    s.set_account(k,Account{lamports:rent+if native {amount}else{0},data,owner:program,executable:false,rent_epoch:0}).unwrap();
}
#[derive(Clone,Copy)]
struct Token {asset:Pubkey,mint:Pubkey,program:Pubkey,feed:Pubkey,pool:Pubkey,vault:Pubkey,inventory:Pubkey,wallet:Pubkey}
struct Bank {svm:LiteSVM,signer:Keypair,f:Value,config:Pubkey,pool:Pubkey,authority:Pubkey,market:Pubkey,
    market_feed:Pubkey,sol_vault:Pubkey,usd_vault:Pubkey,gas:Pubkey,gas_vault:Pubkey,ownership:Pubkey,
    custody:Pubkey,trader:Pubkey,guard:Pubkey,tokens:[Token;3],receipts:Vec<Value>}
impl Bank {
    fn new()->Self {
        Self::create(true)
    }
    fn create(initialize:bool)->Self {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let f:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/fixture.json")).unwrap()).unwrap();
        let mut svm=LiteSVM::new();
        for(address,row)in f["accounts"].as_object().unwrap() {
            if address.starts_with("Sysvar") {continue;}
            svm.set_account(pk(address),Account{lamports:row["lamports"].as_u64().unwrap(),
                data:STANDARD.decode(row["data"].as_str().unwrap()).unwrap(),owner:key(row,"owner"),executable:false,rent_epoch:0}).unwrap();
        }
        for row in f["programs"].as_array().unwrap() {
            let file=root.join(format!(".cache/acidpump-mainnet-execution/{}.so",row["name"].as_str().unwrap()));
            assert_eq!(hash(&file),row["sha256"].as_str().unwrap());svm.add_program(key(row,"program"),&std::fs::read(file).unwrap()).unwrap();
        }
        svm.add_program(p::ID,&std::fs::read(root.join(candidate_binary())).unwrap()).unwrap();
        let signer=Keypair::new_from_array([72;32]);let owner=signer.pubkey();
        svm.set_account(owner,Account{lamports:100_000_000_000,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
        svm.airdrop(&p::mainnet_config::ADMIN,100_000_000_000).unwrap();
        let config=pda(&[b"config"]);let pool=pda(&[b"pool",&0u16.to_le_bytes()]);let authority=pda(&[b"authority",pool.as_ref()]);
        svm.airdrop(&authority,100_000_000_000).unwrap();
        let rows=f["swaps"].as_array().unwrap();
        let descriptors=[(102,key(&rows[0],"mint"),key(&rows[0],"tokenProgram"),key(&rows[0],"pool")),
            (104,USDC,anchor_spl::token::ID,p::pump::USDC_POOL),(103,SOL,anchor_spl::token::ID,Pubkey::default())];
        let tokens=descriptors.map(|(id,mint,program,dex)| {
            let asset=pda(&[b"asset",mint.as_ref()]);let feed=pda(&[b"feed",&(id as u16).to_le_bytes()]);
            let inventory=pda(&[b"inventory",pool.as_ref(),asset.as_ref()]);let vault=pda(&[b"vault",pool.as_ref(),asset.as_ref()]);
            let wallet=Pubkey::find_program_address(&[owner.as_ref(),program.as_ref(),mint.as_ref()],&anchor_spl::associated_token::ID).0;
            token_account(&mut svm,wallet,owner,mint,program,1_000_000*UNIT);
            Token{asset,mint,program,feed,pool:dex,vault,inventory,wallet}
        });
        let market_feed=pda(&[b"feed",&2u16.to_le_bytes()]);let market=pda(&[b"market",market_feed.as_ref()]);
        let sol_vault=pda(&[b"sol-quote",pool.as_ref()]);let usd_vault=pda(&[b"quote",pool.as_ref()]);
        let gas_vault=pda(&[b"gas-vault",pool.as_ref()]);let gas=pda(&[b"gas-reserve"]);let ownership=pda(&[b"ownership-pool",pool.as_ref()]);
        let custody=pda(&[b"er-custody",owner.as_ref()]);let trader=pda(&[b"er-trader",owner.as_ref()]);let guard=pda(&[b"er-collateral-guard",owner.as_ref()]);
        let mut clock:Clock=svm.get_sysvar();clock.slot=f["slot"].as_u64().unwrap();clock.unix_timestamp=f["now"].as_i64().unwrap();svm.set_sysvar(&clock);
        let mut bank=Self{svm,signer,f,config,pool,authority,market,market_feed,sol_vault,usd_vault,gas,gas_vault,ownership,custody,trader,guard,tokens,receipts:vec![]};
        if initialize {bank.bootstrap();}bank
    }
    fn send(&mut self,label:&str,instructions:Vec<Instruction>,reject:bool) {
        // The fixed publisher's signature is simulated; contract signer/address
        // checks still execute. Test wallet signatures otherwise remain real.
        let publisher=instructions.iter().any(|ix|ix.accounts.iter().any(|a|a.is_signer&&a.pubkey==p::mainnet_config::PUBLISHER));
        self.transact(label,instructions,reject,publisher);
    }
    fn admin(&mut self,label:&str,instructions:Vec<Instruction>,reject:bool) { self.transact(label,instructions,reject,true); }
    fn transact(&mut self,label:&str,mut instructions:Vec<Instruction>,reject:bool,simulate_admin_signature:bool) {
        let compute=pk("ComputeBudget111111111111111111111111111111");
        let units=if instructions.iter().any(|i|i.program_id==p::ID&&i.data.starts_with(p::instruction::CloseBacked::DISCRIMINATOR)){1_200_000}
            else if instructions.iter().any(|i|i.program_id==p::ID&&i.data.starts_with(p::instruction::ErDelegateQuote::DISCRIMINATOR)){500_000}else{1_000_000};
        for(tag,n)in [(1,128*1024u32),(2,units)] {let mut data=vec![tag];data.extend_from_slice(&n.to_le_bytes());instructions.insert(0,Instruction{program_id:compute,accounts:vec![],data});}
        let addresses:Vec<_>=instructions.iter().flat_map(|i|i.accounts.iter().map(|a|a.pubkey)).collect::<BTreeSet<_>>().into_iter().collect();
        let table=AddressLookupTableAccount{key:Pubkey::new_from_array([92;32]),addresses};
        let mut data=vec![0;56];data[..4].copy_from_slice(&1u32.to_le_bytes());data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        for k in &table.addresses {data.extend_from_slice(k.as_ref());}
        self.svm.set_account(table.key,Account{lamports:100_000_000,data,owner:pk("AddressLookupTab1e1111111111111111111111111"),executable:false,rent_epoch:0}).unwrap();
        self.svm.expire_blockhash();let message=v0::Message::try_compile(&self.signer.pubkey(),&instructions,&[table],self.svm.latest_blockhash()).unwrap();
        let locks=message.account_keys.len()+message.address_table_lookups.iter().map(|t|t.writable_indexes.len()+t.readonly_indexes.len()).sum::<usize>();
        assert!(locks<=64,"{label}: {locks} account locks exceed the current mainnet limit");
        let before:Vec<_>=instructions.iter().flat_map(|i|i.accounts.iter()).map(|a|(a.pubkey,self.svm.get_account(&a.pubkey))).collect();
        let message=VersionedMessage::V0(message);
        let tx=if simulate_admin_signature {
            VersionedTransaction{signatures:vec![Default::default();message.header().num_required_signatures as usize],message}
        } else {VersionedTransaction::try_new(message,&[&self.signer]).unwrap()};
        assert!(bincode_size(&tx)<=1232,"{label}: transaction exceeds network packet limit");
        // Offline administrator/publisher transactions bypass Ed25519 verification.
        // Anchor's signer and address checks still execute in the actual SBF.
        let mut bank=std::mem::replace(&mut self.svm,LiteSVM::new()).with_sigverify(!simulate_admin_signature);
        let result=bank.send_transaction(tx);self.svm=bank.with_sigverify(true);
        if reject {
            let error=result.expect_err(label);
            let expected=match label {
                "stranger cannot initialize"|"wallet cannot admit itself"=>Some("Unauthorized"),
                "wrong publisher rejected"|"wrong quote rejected"|"wrong source rejected"|"wrong primary rejected"=>Some("Binding"),
                "missing administrator signature rejected"=>Some("AccountNotSigner"),
                "non-admin cannot withdraw fee surplus"=>Some("ConstraintHasOne"),
                "missing surplus authority signature rejected"=>Some("AccountNotSigner"),
                "surplus cannot take seed funding or operating floor"|"zero surplus withdrawal rejected"|
                "cannot repeat a withdrawal beyond remaining earned surplus"=>Some("GasBudget"),
                "ER bank cannot initialize base config"=>Some("Unauthorized"),
                "deposit rejected while paused"=>Some("Paused"),
                _=>None,
            };
            if let Some(reason)=expected {assert!(error.meta.logs.iter().any(|l|l.contains(reason)),"{label}: expected {reason}: {error:?}");}
            if label=="failed final swap rolls back complete settlement" {
                let invokes=error.meta.logs.iter().filter(|l|l.starts_with(&format!("Program {} invoke",p::pump::ID))).count();
                assert!(invokes>=2,"must reach both real swap hops: {error:?}");
            }
            if label=="failed curve final buy rolls back settlement" {
                for program in [p::pump::ID,p::mainnet_curve::ID] {
                    assert!(error.meta.logs.iter().any(|l|l.starts_with(&format!("Program {program} invoke"))),"must reach the real curve and AMM hops: {error:?}");
                }
            }
            for(k,old)in before {let new=self.svm.get_account(&k);assert_eq!(old.as_ref().map(|a|&a.data),new.as_ref().map(|a|&a.data),"{label}: partial state mutation");if k!=self.signer.pubkey(){assert_eq!(old.map(|a|a.lamports),new.map(|a|a.lamports),"{label}: partial SOL mutation");}}
            self.receipts.push(json!({"name":label,"rejected":true,"error":format!("{:?}",error.err)}));
        }else {let meta=result.unwrap_or_else(|e|panic!("{label}: {e:?}"));println!("{label}: {} CU",meta.compute_units_consumed);self.receipts.push(json!({"name":label,"computeUnits":meta.compute_units_consumed,"feeLamports":meta.fee}));}
    }
    fn valuation(&self,a:Token)->p::accounts::Valuation {
        let src=if a.mint==SOL {self.tokens[1]}else{a};
        let vaults=|pool| {let d=&self.svm.get_account(&pool).unwrap().data;(Pubkey::new_from_array(d[139..171].try_into().unwrap()),Pubkey::new_from_array(d[171..203].try_into().unwrap()))};
        let (dex_base,dex_quote)=vaults(src.pool);let(usd_base,usd_quote)=vaults(self.tokens[1].pool);
        p::accounts::Valuation{dex_pool:src.pool,dex_base,dex_quote,fee_config:Pubkey::find_program_address(&[b"fee_config",p::pump::ID.as_ref()],&p::pump::FEES_ID).0,
            dex_mint:src.mint,global_config:Pubkey::find_program_address(&[b"global_config"],&p::pump::ID).0,
            usd_pool:self.tokens[1].pool,usd_mint:USDC,usd_base,usd_quote,
            source_global:Pubkey::find_program_address(&[b"global_config"],&p::pump::ID).0,
            source_fees:Pubkey::find_program_address(&[b"fee_config",p::pump::ID.as_ref()],&p::pump::FEES_ID).0}
    }
    fn leg(&self,a:Token,buy:bool,base:Pubkey,quote:Pubkey)->Vec<AccountMeta> {
        let row=self.f["swaps"].as_array().unwrap().iter().find(|r|key(r,"mint")==a.mint&&r["buy"].as_bool()==Some(buy)).unwrap();
        let volume=|owner:Pubkey|Pubkey::find_program_address(&[b"user_volume_accumulator",owner.as_ref()],&p::pump::ID).0;
        let cashback=|owner:Pubkey|Pubkey::find_program_address(&[volume(owner).as_ref(),anchor_spl::token::ID.as_ref(),SOL.as_ref()],&anchor_spl::associated_token::ID).0;
        let legacy:Vec<AccountMeta>=row["instruction"]["keys"].as_array().unwrap().iter().map(|k|{
            let mut address=key(k,"key");for(before,after)in[(key(&self.f,"user"),self.authority),(key(&self.f,"userQuote"),quote),
                (key(row,"base"),base),(volume(key(&self.f,"user")),volume(self.authority)),(cashback(key(&self.f,"user")),cashback(self.authority))] {if address==before {address=after;}}
            AccountMeta{pubkey:address,is_writable:k["writable"].as_bool().unwrap(),is_signer:false}
        }).collect();
        if self.svm.get_account(&a.pool).unwrap().data.get(244).copied().unwrap_or(0)==1 {return legacy;}
        let mut compact=legacy[..9].to_vec();
        compact.extend([legacy[11].clone(),legacy[12].clone(),legacy[13].clone(),AccountMeta::new(volume(self.authority),false),
            legacy[if buy{21}else{19}].clone(),legacy.last().unwrap().clone(),legacy[15].clone(),legacy[16].clone()]);
        compact
    }
    fn deposit(&self,a:Token,amount:u64)->Instruction {ix(p::accounts::DepositCollateral{owner:self.signer.pubkey(),config:self.config,pool:self.pool,custody:self.custody,
        asset:a.asset,mint:a.mint,token_program:a.program,inventory:a.inventory,authority:self.authority,source:a.wallet,vault:a.vault,
        execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},p::instruction::DepositCollateral{amount})}
    fn committed(&mut self,a:Token,nonce:u64,margin:u64,liquidated:bool,winning:bool)->ErSlot {
        let now=self.f["now"].as_i64().unwrap();
        let intent=ErSlot{state:ER_PENDING,kind:ER_TRADE,nonce,asset:a.asset,market:2,margin,entry_value:10*UNIT,
            entry:100*PX,bust:90*PX,leverage:10,long:true,opened_at:now-10,opened_source_slot:1,cursor:1,bust_observed:liquidated,
            exit:if winning {101*PX}else if liquidated {89*PX}else{99*PX},closed_at:now,close_seq:if winning {get::<Config>(&self.svm,self.config).next_settlement_seq}else{0},liquidated};
        let mut trader=ErTrader{owner:self.signer.pubkey(),config:self.config,pool:self.pool,next_nonce:nonce+1,
            bump:bump(&[b"er-trader",p::venue::ns(self.config).as_ref(),self.signer.pubkey().as_ref()]),..ErTrader::default()};trader.slots[0]=intent;
        put(&mut self.svm,self.trader,&trader,p::basebridge::DELEGATION_OWNER);
        let mut guard=CollateralGuard{owner:self.signer.pubkey(),config:self.config,floors:CollateralGuard::empty_floors(),bump:bump(&[b"er-collateral-guard",p::venue::ns(self.config).as_ref(),self.signer.pubkey().as_ref()])};
        guard.floors[0]=CollateralFloor{nonce,value:intent.entry_value,cursor:1,incomplete:false,closed_at:now,exit:intent.exit,covered_at:now+1,entry_price:1_000_000_000_000};
        put(&mut self.svm,self.guard,&guard,p::basebridge::DELEGATION_OWNER);intent
    }
    fn client_fixture(&self)->Value {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let data:Value=serde_json::from_slice(&std::fs::read(root.join(client_fixture_path())).unwrap()).unwrap();
        assert_eq!(data["programId"].as_str().unwrap(),p::ID.to_string());
        assert_eq!(data["owner"].as_str().unwrap(),self.signer.pubkey().to_string());
        assert_eq!(data["sourceFixtureSha256"].as_str().unwrap(),hash(root.join(".cache/acidpump-mainnet-execution/fixture.json")));
        data
    }
    fn register_backed_collateral(&mut self,symbol:&str)->Token {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let path=root.join("projects/acidpump-er/src/fixtures/pump-generations.json");
        let f:Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let fixture=self.client_fixture();
        assert_eq!(fixture["generationFixtureSha256"].as_str().unwrap(),hash(&path));
        let sample=f["samples"].as_array().unwrap().iter().find(|s|s["symbol"]==symbol).unwrap();
        let mint=key(sample,"mint");let program=key(sample,"tokenProgram");
        let pool=if symbol=="FARTCOIN" {pk("AmmpSnW5xVeKHTAU9fMjyKEMPgrzmUj3ah5vgvHhAB5J")} else {key(sample,"canonicalPool")};
        let pool_data=STANDARD.decode(f["accounts"][pool.to_string()]["data"][0].as_str().unwrap()).unwrap();
        for address in [mint,pool,Pubkey::new_from_array(pool_data[139..171].try_into().unwrap()),Pubkey::new_from_array(pool_data[171..203].try_into().unwrap())] {
            let row=&f["accounts"][address.to_string()];
            self.svm.set_account(address,Account{lamports:row["lamports"].as_u64().unwrap(),data:STANDARD.decode(row["data"][0].as_str().unwrap()).unwrap(),
                owner:key(row,"owner"),executable:false,rent_epoch:0}).unwrap();
        }
        let id=if symbol=="FARTCOIN" {110u16}else{111};let owner=self.signer.pubkey();let admin=p::mainnet_config::ADMIN;
        let asset=pda(&[b"asset",mint.as_ref()]);let feed=pda(&[b"feed",&id.to_le_bytes()]);
        let inventory=pda(&[b"inventory",self.pool.as_ref(),asset.as_ref()]);let vault=pda(&[b"vault",self.pool.as_ref(),asset.as_ref()]);
        let wallet=Pubkey::find_program_address(&[owner.as_ref(),program.as_ref(),mint.as_ref()],&anchor_spl::associated_token::ID).0;
        token_account(&mut self.svm,wallet,owner,mint,program,1_000_000*UNIT);
        let token=Token{asset,mint,program,feed,pool,vault,inventory,wallet};
        self.admin("register captured backed collateral",vec![
            ix(p::accounts::CreateFeed{admin,config:self.config,feed,system_program:Pubkey::default()},p::instruction::CreateFeed{id,symbol:[0;12]}),
            ix(p::accounts::RegisterAsset{admin,config:self.config,feed,asset,mint,token_program:program,valuation:self.valuation(token),system_program:Pubkey::default()},
                p::instruction::RegisterAsset{source_mint:mint,source_pool:pool,collateral_enabled:true})],false);
        self.send("create backed collateral custody vault",vec![ix(p::accounts::CreateInventory{payer:owner,pool:self.pool,authority:self.authority,
            asset,mint,token_program:program,inventory,vault,system_program:Pubkey::default()},p::instruction::CreateInventory{})],false);
        assert!(p::stake_backing::requires_backing(asset));token
    }
    fn client_instructions(rows:&Value)->Vec<Instruction> {
        rows.as_array().unwrap().iter().map(|row|Instruction{program_id:key(row,"program"),data:STANDARD.decode(row["data"].as_str().unwrap()).unwrap(),
            accounts:row["accounts"].as_array().unwrap().iter().map(|a|AccountMeta{pubkey:key(a,"key"),is_signer:a["signer"].as_bool().unwrap(),is_writable:a["writable"].as_bool().unwrap()}).collect()}).collect()
    }
    fn register_curve_collateral(&mut self,index:usize)->Token {
        let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let file=root.join("projects/acidpump-er/src/fixtures/mainnet-pump-curves.json");
        let f:Value=serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        let client=self.client_fixture();assert_eq!(client["curveFixtureSha256"].as_str().unwrap(),hash(&file));
        let sample=&f["candidates"][index];let mint=key(sample,"mint");let curve=key(sample,"curve");
        for(address,row)in f["accounts"].as_object().unwrap() {
            if address.starts_with("Sysvar"){continue;}
            self.svm.set_account(pk(address),Account{lamports:row["lamports"].as_u64().unwrap(),data:STANDARD.decode(row["data"].as_str().unwrap()).unwrap(),
                owner:key(row,"owner"),executable:false,rent_epoch:0}).unwrap();
        }
        let raw=STANDARD.decode(f["accounts"]["SysvarRent111111111111111111111111111111111"]["data"].as_str().unwrap()).unwrap();
        #[allow(deprecated)]
        let rent=anchor_lang::prelude::Rent{lamports_per_byte_year:u64::from_le_bytes(raw[..8].try_into().unwrap()),exemption_threshold:f64::from_le_bytes(raw[8..16].try_into().unwrap()),burn_percent:raw[16]};
        self.svm.set_sysvar(&rent);
        let binary:Value=serde_json::from_slice(&std::fs::read(root.join("reports/acidpump-pump-curve-binary.json")).unwrap()).unwrap();
        let file=root.join(binary["file"].as_str().unwrap());assert_eq!(hash(&file),binary["sha256"].as_str().unwrap());
        self.svm.add_program(p::mainnet_curve::ID,&std::fs::read(file).unwrap()).unwrap();
        let row=&client["curves"][index];assert_eq!(key(&row["asset"],"mint"),mint);
        self.admin("register curve with the real valuation adapter",Self::client_instructions(&row["registration"]),false);
        let id=(120+index)as u16;let owner=self.signer.pubkey();let program=key(&f["accounts"][mint.to_string()],"owner");
        let asset=pda(&[b"asset",mint.as_ref()]);let wallet=Pubkey::find_program_address(&[owner.as_ref(),program.as_ref(),mint.as_ref()],&anchor_spl::associated_token::ID).0;
        token_account(&mut self.svm,wallet,owner,mint,program,1_000_000*UNIT);
        Token{asset,mint,program,feed:pda(&[b"feed",&id.to_le_bytes()]),pool:curve,wallet,
            inventory:pda(&[b"inventory",self.pool.as_ref(),asset.as_ref()]),vault:pda(&[b"vault",self.pool.as_ref(),asset.as_ref()])}
    }
    fn settlement(&self,a:Token,intent:ErSlot,min_perp:u64)->Vec<Instruction> {
        let owner=self.signer.pubkey();let position=pda(&[b"position",owner.as_ref(),&intent.nonce.to_le_bytes()]);let primary=self.tokens[0];
        let trader=pda(&[b"ownership-trader",self.pool.as_ref(),owner.as_ref()]);let receipt=pda(&[b"ownership-receipt",position.as_ref()]);
        let prepare=ix(p::accounts::PrepareSettlement{payer:owner,owner,config:self.config,pool:self.pool,market:self.market,asset:a.asset,trader:self.trader,custody:self.custody,position,system_program:Pubkey::default()},p::instruction::PrepareSettlement{nonce:intent.nonce,slot:0});
        let ownership=ix(p::accounts::InitializeOwnership{payer:owner,position,ownership_trader:trader,ownership_receipt:receipt,system_program:Pubkey::default()},p::instruction::InitializeOwnership{});
        let same=a.asset==primary.asset;let(loss,_)=(if intent.liquidated {intent.margin}else if intent.close_seq>0 {0}else{p::math::pnl(intent.margin,intent.entry,intent.exit,intent.leverage,intent.long).unwrap().1},0);
        let mut accounts=vec![];let mut sell_count=0;let mut buy_count=0;
        if loss>0 {
            if a.mint!=SOL {accounts=self.leg(a,false,a.vault,if same {self.gas_vault}else{self.sol_vault});sell_count=accounts.len() as u8;}
            if !same {let buy=self.leg(primary,true,primary.vault,self.sol_vault);buy_count=buy.len() as u8;accounts.extend(buy);}
        }
        let mut close=ix(p::accounts::Close {
            quote_publisher:p::mainnet_config::PUBLISHER,
            pool_accounts:p::accounts::ClosePoolAccounts{caller:owner,pool:self.pool,authority:self.authority,valuation:self.valuation(a)},
            settlement:primary.asset,settlement_inventory:if same {None}else{Some(primary.inventory)},settlement_vault:if same {None}else{Some(primary.vault)},
            quote_vault:self.sol_vault,pump_program:p::pump::ID,market:self.market,
            #[cfg(feature="mainnet")] economics:pda(&[b"economics-v1"]),
            #[cfg(not(feature="mainnet"))] feed:self.feed,
            asset:a.asset,position,
            ownership:p::accounts::CloseOwnership{exposure:pda(&[b"exposure",owner.as_ref()]),ownership_pool:self.ownership,ownership_trader:trader,ownership_receipt:receipt,settlement_valuation:self.valuation(primary)},
            mint:a.mint,token_program:a.program,inventory:a.inventory,vault:a.vault,
            fees:p::accounts::LossRouting{gas_reserve:self.gas,perp:primary.asset,perp_mint:primary.mint,perp_program:primary.program,perp_vault:primary.vault,
                main_pool:None,main_ownership_pool:None,main_inventory:None,main_vault:None,gas_asset:self.tokens[2].asset,gas_mint:SOL,gas_vault:self.gas_vault,gas_program:anchor_spl::token::ID,system_program:Pubkey::default(),venue:None,buyback_vault:None},
            bridge:p::accounts::ErCloseBridge{config:self.config,custody:self.custody,trader:self.trader,guard:self.guard},
        },p::instruction::CloseBacked{sell_count:0,min_swap_out:1,market_accounts:0,fee_sell_count:sell_count,perp_buy_count:buy_count,min_perp,min_gas:0,
            sol_quote:p::SolUsdQuote{price:110*PX,publish_time:self.f["now"].as_i64().unwrap()}});
        close.accounts.extend(accounts);
        let fixture=self.client_fixture();let row=fixture["settlements"].as_array().unwrap().iter().find(|r|r["nonce"].as_u64()==Some(intent.nonce)).unwrap();
        assert_eq!(row["mint"].as_str().unwrap(),a.mint.to_string());
        let mut built=Self::client_instructions(&row["instructions"]);
        // Mutate only the explicit minimum used by the atomic-failure test.
        built.last_mut().unwrap().data[20..28].copy_from_slice(&min_perp.to_le_bytes());
        // A warped offline bank uses its current authenticated SOL quote time.
        let end=built.last().unwrap().data.len();
        built.last_mut().unwrap().data[end-8..].copy_from_slice(&self.f["now"].as_i64().unwrap().to_le_bytes());
        assert_eq!(built[0],prepare,"client prepare instruction differs from Rust ABI");
        assert_eq!(built[1],ownership,"client ownership instruction differs from Rust ABI");
        if loss>0 {assert_eq!(*built.last().unwrap(),close,"client loss route differs from the independently built Rust instruction");}
        built
    }
    fn payout_raw(&self,position:Pubkey)->Instruction {
        let a=self.tokens[0];let mut out=ix(p::accounts::RedeemUsdc{caller:self.signer.pubkey(),config:self.config,pool:self.pool,authority:self.authority,position,
            asset:a.asset,valuation:self.valuation(a),mint:a.mint,token_program:a.program,inventory:a.inventory,vault:a.vault,quote_mint:USDC,quote_program:anchor_spl::token::ID,
            quote_vault:self.usd_vault,destination:self.tokens[1].wallet,pump_program:p::pump::ID,sol_vault:self.sol_vault},p::instruction::RedeemUsdc{});
        out.accounts.extend(self.leg(a,false,a.vault,self.sol_vault));out.accounts.extend(self.leg(self.tokens[1],true,self.usd_vault,self.sol_vault));
        out
    }
    fn payout(&self,position:Pubkey)->Vec<Instruction> {
        let out=self.payout_raw(position);
        let fixture=self.client_fixture();let row=fixture["payouts"].as_array().unwrap().iter().find(|r|key(r,"position")==position).unwrap();
        let built=Self::client_instructions(&row["instructions"]);
        assert_eq!(*built.last().unwrap(),out,"client payout route differs from Rust ABI");built
    }
}

#[test]
fn fifo_payouts_support_primary_pool_modes_and_old_layouts() {
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let modes:Value=serde_json::from_slice(&std::fs::read(root.join(".cache/acidpump-mainnet-execution/modes.json")).unwrap()).unwrap();
    let mut cases=vec![];
    for case in modes["cases"].as_array().unwrap() {
        let mut h=Bank::new();let primary=h.tokens[0];let usd=h.tokens[1];let owner=h.signer.pubkey();
        h.send("fund winning user's actual USDC deposit",vec![h.deposit(usd,10*UNIT)],false);
        let win=h.committed(usd,4,10*UNIT,false,true);
        let position=pda(&[b"position",owner.as_ref(),&4u64.to_le_bytes()]);
        h.send("create actual FIFO claim",h.settlement(usd,win,1),false);
        let original:Position=get(&h.svm,position);assert!(original.profit>0);
        // This test supplies explicit treasury capital in its local bank. The
        // actual production payout then validates and debits that inventory.
        let capital=100_000*UNIT;
        token_account(&mut h.svm,primary.vault,h.authority,primary.mint,primary.program,capital);
        let mut inventory:Inventory=get(&h.svm,primary.inventory);inventory.treasury=capital;put(&mut h.svm,primary.inventory,&inventory,p::ID);
        let quote_vault=Pubkey::new_from_array(h.svm.get_account(&primary.pool).unwrap().data[171..203].try_into().unwrap());
        for address in [primary.pool,primary.mint,quote_vault] {
            let row=&case["fixture"]["accounts"][address.to_string()];
            let mut account=h.svm.get_account(&address).unwrap();account.data=STANDARD.decode(row["data"].as_str().unwrap()).unwrap();account.lamports=row["lamports"].as_u64().unwrap();
            h.svm.set_account(address,account).unwrap();
        }
        let mut swaps=case["fixture"]["swaps"].as_array().unwrap().clone();
        swaps.extend(h.f["swaps"].as_array().unwrap().iter().filter(|r|key(r,"mint")!=primary.mint).cloned());
        h.f["swaps"]=json!(swaps);
        let before=token(&h.svm,usd.wallet);let sol_before=token(&h.svm,h.sol_vault);
        h.send("pay actual FIFO claim using versioned pool route",vec![h.payout_raw(position)],false);
        let paid=token(&h.svm,usd.wallet)-before;let after:Position=get(&h.svm,position);
        assert!(paid>0 && paid<=original.profit);assert_eq!(after.remaining,0);assert!(original.profit-paid<=1);
        assert!(token(&h.svm,h.sol_vault)>=sol_before);assert_eq!(get::<Pool>(&h.svm,h.pool).queue_usd,0);
        let remaining=get::<Inventory>(&h.svm,primary.inventory).treasury;
        assert_eq!(remaining,token(&h.svm,primary.vault));assert!(remaining<capital);
        h.send("mode payout cannot replay",vec![h.payout_raw(position)],true);
        cases.push(json!({"mode":case["name"],"paidUsdc":paid,"claimUsdc":original.profit,"treasuryTokensSpent":capital-remaining,"receipts":h.receipts}));
    }
    std::fs::write(root.join(report_path("mode-payouts")),serde_json::to_vec_pretty(&json!({
        "scope":"Actual production SBF FIFO payout with real Pump programs, synthetic treasury and injected ER commit boundary",
        "localSimulation":true,"mainnetTransactionsSubmitted":0,"binarySha256":hash(root.join(candidate_binary())),"cases":cases
    })).unwrap()).unwrap();
}
// Versioned transactions use compact-u16 vectors. This is the exact packet
// length for our v0 messages (one signature); no unchecked unlimited-bank tests.
fn bincode_size(tx:&VersionedTransaction)->usize {1+64*tx.signatures.len()+tx.message.serialize().len()}

#[test]
#[cfg(feature="mainnet")]
fn released_collateral_withdrawal_rejects_stale_active_and_replayed_state() {
    let mut h=Bank::new();let owner=h.signer.pubkey();let a=h.tokens[1];
    h.send("fund recovery custody",vec![h.deposit(a,3*UNIT)],false);
    let mut t:ErTrader=get(&h.svm,h.trader);
    let instruction=|h:&Bank,amount|ix(p::accounts::WithdrawReleasedCollateral{
        caller:owner,owner,config:h.config,pool:h.pool,trader:h.trader,custody:h.custody,asset:a.asset,mint:a.mint,
        token_program:a.program,inventory:a.inventory,authority:h.authority,vault:a.vault,destination:a.wallet,
        execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},
        p::instruction::WithdrawReleasedCollateral{amount});
    h.send("recovery requires pause",vec![instruction(&h,UNIT)],true);
    h.admin("pause for graceful recovery",vec![ix(p::accounts::Admin{admin:p::mainnet_config::ADMIN,config:h.config},p::instruction::Pause{paused:true})],false);
    put(&mut h.svm,h.trader,&t,p::basebridge::DELEGATION_OWNER);
    h.send("delegated idle-looking base copy cannot withdraw",vec![instruction(&h,UNIT)],true);
    // Model the authenticated DLP callback boundary; native return is a separate drill.
    put(&mut h.svm,h.trader,&t,p::ID);
    for state in [ER_OPEN,ER_PENDING] {
        t.slots[0].state=state;put(&mut h.svm,h.trader,&t,p::ID);
        h.send("open or unsettled slots cannot escape",vec![instruction(&h,UNIT)],true);
    }
    t.slots[0]=ErSlot::default();t.notional=1;put(&mut h.svm,h.trader,&t,p::ID);
    h.send("nonzero exposure cannot escape",vec![instruction(&h,UNIT)],true);
    t.notional=0;t.core_stake.tokens=UNIT;put(&mut h.svm,h.trader,&t,p::ID);
    h.send("locked backing cannot escape",vec![instruction(&h,UNIT)],true);
    t.core_stake.tokens=0;put(&mut h.svm,h.trader,&t,p::ID);
    let before=token(&h.svm,a.wallet);
    let mut forged=instruction(&h,UNIT);forged.accounts.iter_mut().find(|k|k.pubkey==a.wallet).unwrap().pubkey=h.tokens[0].wallet;
    h.send("wrong withdrawal destination rejected",vec![forged],true);
    h.send("zero withdrawal rejected",vec![instruction(&h,0)],true);
    h.send("overdraw rejected",vec![instruction(&h,4*UNIT)],true);
    t.assets[0]=ErBalance{asset:a.asset,available:UNIT,..ErBalance::default()};put(&mut h.svm,h.trader,&t,p::ID);
    h.send("inconsistent ER and base credits rejected",vec![instruction(&h,UNIT)],true);
    t.assets[0]=ErBalance::default();put(&mut h.svm,h.trader,&t,p::ID);
    h.send("owner withdraws reconciled released balance",vec![instruction(&h,3*UNIT)],false);
    assert_eq!(token(&h.svm,a.wallet)-before,3*UNIT);
    assert_eq!(get::<ErCustody>(&h.svm,h.custody).assets[0].available,0);
    let after:ErTrader=get(&h.svm,h.trader);assert_eq!(after.assets[0].available,0);assert_eq!(after.assets[0].credited_deposits,3*UNIT);
    h.send("released withdrawal cannot replay",vec![instruction(&h,3*UNIT)],true);
    assert_eq!(get::<Inventory>(&h.svm,a.inventory).locked,0);
}

#[test]
fn unsigned_operator_bootstrap_plan_initializes_paused_sdice_and_psdice() {
    let mut bank=Bank::create(false);
    let fixture=bank.client_fixture();
    let steps=fixture["bootstrap"].as_array().unwrap();
    assert_eq!(steps.len(),31);
    let mut created=BTreeSet::new();
    for step in steps {
        let keys:Vec<_>=step["creates"].as_array().unwrap().iter().map(|s|pk(s.as_str().unwrap())).collect();
        for key in &keys {assert!(created.insert(*key));assert!(bank.svm.get_account(key).is_none());}
        bank.admin(step["name"].as_str().unwrap(),Bank::client_instructions(&step["instructions"]),false);
        for key in keys {assert!(bank.svm.get_account(&key).is_some());}
        let config:Config=get(&bank.svm,bank.config);assert!(config.paused);
    }
    let pool:Pool=get(&bank.svm,bank.pool);assert_eq!(pool.settlement,bank.tokens[0].asset);
    let _:OwnershipPool=get(&bank.svm,bank.ownership);
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(root.join(report_path("bootstrap")),serde_json::to_vec_pretty(&json!({
        "programId":p::ID.to_string(),"binarySha256":hash(root.join(candidate_binary())),
        "clientFixtureSha256":hash(root.join(client_fixture_path())),
        "localSimulation":true,"mainnetTransactionsSubmitted":0,"initializedPaused":true,
        "primary":"SDICE","secondary":"pSDICE","secondarySplMintCreated":false,
        "stepsExecuted":steps.len(),"accountsCreated":created.len(),"receipts":bank.receipts
    })).unwrap()).unwrap();
}

#[test]
fn winners_use_current_collateral_quote_without_charging_stake_or_using_history() {
    let mut h=Bank::new();let primary=h.tokens[0];let owner=h.signer.pubkey();
    h.send("deposit collateral for executable backing cap",vec![h.deposit(primary,100*UNIT)],false);
    // Inject the already committed ER boundary: entry backing was $10, and a
    // historical 40% price dip reduced the protected value to $6. The captured
    // live pool can now realize less than $6 from the remaining 100 tokens.
    let intent=h.committed(primary,6,100*UNIT,false,true);
    let mut guard:CollateralGuard=get(&h.svm,h.guard);guard.floors[0].value=6*UNIT;
    put(&mut h.svm,h.guard,&guard,p::basebridge::DELEGATION_OWNER);
    let position=pda(&[b"position",owner.as_ref(),&6u64.to_le_bytes()]);
    h.send("cap winning claim at current executable backing",h.settlement(primary,intent,1),false);
    let x:Position=get(&h.svm,position);
    assert!(x.close_value>0 && x.close_value<6*UNIT);
    assert_eq!(x.coverage_tokens,0);
    assert_eq!(x.uncovered_usd,0);
    assert_eq!(get::<ErCustody>(&h.svm,h.custody).core_coverage.incurred_usd,x.uncovered_usd);
    assert_eq!(x.exit,intent.exit);assert_eq!(x.closed_at,intent.closed_at);
    assert_eq!(x.profit,p::math::pnl(x.close_value,intent.entry,intent.exit,intent.leverage,true).unwrap().1);
    assert_eq!(x.returned,intent.margin);assert_eq!(x.loss,0);
    h.send("capped winning claim cannot replay",h.settlement(primary,intent,1),true);
}

#[test]
fn drained_collateral_pool_does_not_trap_returned_tokens_or_invent_profit() {
    for winning in [false,true] {
        let mut h=Bank::new();let a=h.tokens[0];let owner=h.signer.pubkey();let margin=100*UNIT;
        h.send("deposit before pool loses liquidity",vec![h.deposit(a,margin)],false);
        let intent=h.committed(a,if winning {6}else{2},margin,false,winning);
        let instructions=h.settlement(a,intent,1);
        let valuation=h.valuation(a);let mut vault=h.svm.get_account(&valuation.dex_quote).unwrap();
        vault.data[64..72].fill(0);vault.lamports=h.svm.minimum_balance_for_rent_exemption(vault.data.len());
        h.svm.set_account(valuation.dex_quote,vault).unwrap();
        let before=token(&h.svm,a.vault);
        h.send("close after collateral pool has no real SOL",instructions.clone(),false);
        let position=pda(&[b"position",owner.as_ref(),&intent.nonce.to_le_bytes()]);let x:Position=get(&h.svm,position);
        let custody:ErCustody=get(&h.svm,h.custody);let inv:Inventory=get(&h.svm,a.inventory);
        assert_ne!(x.state,0);assert_eq!(x.collateral_close_value,0);assert_eq!(x.profit,0);
        assert_eq!(x.returned,if winning {margin}else{margin-margin/10});
        assert_eq!(custody.assets[custody.asset_index(a.asset).unwrap()].available,x.returned);
        assert!(custody.receipts[0].settled);assert_eq!(token(&h.svm,a.vault),before);
        assert_eq!(inv.locked,x.returned);assert_eq!(inv.treasury,x.loss);
        h.send("drained close cannot replay",instructions,true);
    }
}

#[test]
fn drained_non_primary_collateral_closes_and_awards_consumed_backing_once() {
    // Actual captured SPL and Token-2022 mints/pools, registered with real
    // candidate instructions. Only the ER commit boundary and depleted source
    // liquidity are injected; all base custody/settlement instructions execute.
    for symbol in ["FARTCOIN","USDF"] {
        for (staked,liquidated,winning) in [(500_000*UNIT,false,false),(50*UNIT,false,false),
            (0,true,false),(50*UNIT,true,false),(500_000*UNIT,true,false),(500_000*UNIT,false,true)] {
            let mut h=Bank::new();let a=h.register_backed_collateral(symbol);let primary=h.tokens[0];let owner=h.signer.pubkey();
            let margin=100*UNIT;let nonce=if symbol=="FARTCOIN" {7}else{8};
            h.send("deposit non-primary collateral",vec![h.deposit(a,margin)],false);
            if staked>0 {h.send("deposit core backing",vec![h.deposit(primary,staked)],false);}
            let before_custody:ErCustody=get(&h.svm,h.custody);
            let intent=h.committed(a,nonce,margin,liquidated,winning);
            let mut trader:ErTrader=get(&h.svm,h.trader);trader.core_stake.tokens=staked;trader.custody_generation=before_custody.generation;
            trader.notional=intent.entry_value*intent.leverage as u64;
            for (target,source) in trader.assets.iter_mut().zip(before_custody.assets.iter()) {
                *target=ErBalance{asset:source.asset,credited_deposits:source.deposited_credits,
                    credited_returns:source.returned_credits,available:0,generation:source.generation};
            }
            put(&mut h.svm,h.trader,&trader,p::basebridge::DELEGATION_OWNER);
            let fixture=h.client_fixture();let row=fixture["settlements"].as_array().unwrap().iter().find(|r|r["nonce"].as_u64()==Some(nonce)).unwrap();
            assert_eq!(key(row,"mint"),a.mint);let instructions=Bank::client_instructions(&row["instructions"]);
            let valuation=h.valuation(a);let mut quote=h.svm.get_account(&valuation.dex_quote).unwrap();
            // Retained protocol/creator fees still belong in the vault even
            // when all spendable liquidity is gone. Removing those too would
            // create an invalid pool snapshot, not an empty executable pool.
            let pool_bytes=h.svm.get_account(&a.pool).unwrap().data;
            let retained=[271,279].iter().map(|&at|pool_bytes.get(at..at+8).map(|b|u64::from_le_bytes(b.try_into().unwrap())).unwrap_or(0)).sum::<u64>();
            quote.data[64..72].copy_from_slice(&retained.to_le_bytes());quote.lamports=h.svm.minimum_balance_for_rent_exemption(quote.data.len())+retained;
            h.svm.set_account(valuation.dex_quote,quote).unwrap();
            let primary_before:Inventory=get(&h.svm,primary.inventory);
            let primary_vault_before=token(&h.svm,primary.vault);let collateral_vault_before=token(&h.svm,a.vault);
            let core_supply=supply(&h.svm,primary.mint);let gas_before:GasReserve=get(&h.svm,h.gas);
            h.send("settle drained backed collateral",instructions.clone(),false);
            let position=pda(&[b"position",owner.as_ref(),&nonce.to_le_bytes()]);let x:Position=get(&h.svm,position);
            let c:ErCustody=get(&h.svm,h.custody);let inv:Inventory=get(&h.svm,a.inventory);let core:Inventory=get(&h.svm,primary.inventory);
            let loss=if winning {0}else if liquidated {margin}else{margin/10};
            let target=if winning {0}else if liquidated {intent.entry_value}else{intent.entry_value/10};
            assert_eq!(x.state,if liquidated {2}else{1});assert_eq!((x.loss,x.returned),(loss,margin-loss));
            assert_eq!((x.collateral_close_value,x.profit,x.close_value,x.swapped_out),(0,0,0,0));
            assert_eq!(x.coverage_usd+x.uncovered_usd,target);assert!(x.coverage_tokens<=staked);
            if winning {assert_eq!((x.coverage_tokens,x.uncovered_usd),(0,0));}
            else if staked==500_000*UNIT {assert!(x.coverage_tokens>0 && x.coverage_tokens<staked);assert_eq!(x.uncovered_usd,0);}
            else {assert_eq!(x.coverage_tokens,staked);assert!(x.uncovered_usd>0);}
            assert_eq!(c.assets[c.asset_index(a.asset).unwrap()].available,margin-loss);
            assert_eq!((inv.locked,inv.dust,inv.treasury),(margin-loss,loss,0));
            assert_eq!(core.locked,primary_before.locked-x.coverage_tokens);
            assert_eq!(core.treasury,primary_before.treasury+x.coverage_tokens);
            assert_eq!((c.core_coverage.debited_tokens,c.core_coverage.incurred_usd),(x.coverage_tokens,x.uncovered_usd));
            if staked>0 {assert_eq!(c.assets[c.asset_index(primary.asset).unwrap()].available,staked-x.coverage_tokens);}
            assert!(c.receipts[0].settled);assert_eq!(c.receipts[0].position,position);
            // Backing is an internal transfer to payout inventory. No swap,
            // burn or gas share may spend unrelated balances when output is zero.
            assert_eq!(token(&h.svm,a.vault),collateral_vault_before);assert_eq!(token(&h.svm,primary.vault),primary_vault_before);
            assert_eq!(supply(&h.svm,primary.mint),core_supply);assert_eq!(get::<GasReserve>(&h.svm,h.gas).sol_received,gas_before.sol_received);
            let reward:p::OwnershipReceipt=get(&h.svm,pda(&[b"ownership-receipt",position.as_ref()]));
            let ownership:OwnershipTrader=get(&h.svm,pda(&[b"ownership-trader",h.pool.as_ref(),owner.as_ref()]));
            assert!(reward.awarded);assert_eq!(reward.pool_tokens,x.coverage_tokens);assert_eq!(reward.issued,x.coverage_tokens as u128*100);
            assert_eq!((ownership.available,ownership.issued),(reward.issued,reward.issued));
            h.send("drained backed settlement cannot replay",instructions,true);
            // Exercise receipt reconciliation directly; ER transport itself is
            // outside this local base-chain harness. Pool shortfall is not debt.
            let mut risk=ErRisk{config:h.config,..ErRisk::default()};risk.markets[intent.market as usize].long_oi=trader.notional;
            sync_custody(&mut trader,&mut risk,&c).unwrap();sync_custody(&mut trader,&mut risk,&c).unwrap();
            sync_custody(&mut trader,&mut risk,&before_custody).unwrap();
            assert_eq!(trader.slots[0].state,ER_FREE);assert_eq!(trader.notional,0);assert_eq!(trader.core_stake.tokens,staked-x.coverage_tokens);
            assert_eq!(trader.assets[trader.asset_index(a.asset).unwrap()].available,margin-loss);
            p::stake_backing::check_unlock(&p::stake_backing::positions(&trader,&p::stake_backing::root_backing()),trader.core_stake.uncovered_usd().unwrap()).unwrap();
        }
    }
}

#[test]
fn winning_collateral_decline_never_consumes_core_stake() {
    let mut h=Bank::new();let primary=h.tokens[0];let owner=h.signer.pubkey();
    h.send("deposit separate stake and collateral",vec![h.deposit(primary,500_100*UNIT)],false);
    let intent=h.committed(primary,6,100*UNIT,false,true);
    let mut trader:ErTrader=get(&h.svm,h.trader);trader.core_stake.tokens=500_000*UNIT;
    put(&mut h.svm,h.trader,&trader,p::basebridge::DELEGATION_OWNER);
    // An unusable old collateral guard must not haircut or block this model.
    let mut guard:CollateralGuard=get(&h.svm,h.guard);guard.floors[0].incomplete=true;guard.floors[0].value=0;
    put(&mut h.svm,h.guard,&guard,p::basebridge::DELEGATION_OWNER);
    let position=pda(&[b"position",owner.as_ref(),&6u64.to_le_bytes()]);
    let instructions=h.settlement(primary,intent,1);
    let mut stale=instructions.clone();let end=stale.last().unwrap().data.len();
    stale.last_mut().unwrap().data[end-8..].copy_from_slice(&(h.f["now"].as_i64().unwrap()-4).to_le_bytes());
    h.send("stale settlement SOL quote rejected atomically",stale,true);
    let mut unsigned=instructions.clone();
    unsigned.last_mut().unwrap().accounts.iter_mut().find(|a|a.pubkey==p::mainnet_config::PUBLISHER).unwrap().is_signer=false;
    h.send("unsigned settlement quote rejected atomically",unsigned,true);
    let mut stranger=instructions.clone();
    stranger.last_mut().unwrap().accounts.iter_mut().find(|a|a.pubkey==p::mainnet_config::PUBLISHER).unwrap().pubkey=owner;
    h.send("wrong quote publisher rejected atomically",stranger,true);
    let inv:Inventory=get(&h.svm,primary.inventory);let custody:ErCustody=get(&h.svm,h.custody);
    let vault=token(&h.svm,primary.vault);let rewards:OwnershipPool=get(&h.svm,h.ownership);
    h.send("cover collateral decline with existing core stake",instructions.clone(),false);
    let x:Position=get(&h.svm,position);let after:Inventory=get(&h.svm,primary.inventory);let c:ErCustody=get(&h.svm,h.custody);
    assert_eq!(x.coverage_tokens,0);assert_eq!(x.uncovered_usd,0);assert_eq!(x.close_value,x.collateral_close_value);
    assert_eq!(x.coverage_usd,0);
    assert_eq!(x.profit,p::math::pnl(x.close_value,intent.entry,intent.exit,intent.leverage,true).unwrap().1);
    assert_eq!((x.loss,x.returned,x.swapped_out),(0,intent.margin,0));
    assert_eq!(after.locked,inv.locked-x.coverage_tokens);assert_eq!(after.treasury,inv.treasury+x.coverage_tokens);
    assert_eq!(token(&h.svm,primary.vault),vault);
    assert_eq!(c.assets[0].available,custody.assets[0].available-x.coverage_tokens);
    assert_eq!(c.core_coverage.debited_tokens,x.coverage_tokens);
    let reward_after:OwnershipPool=get(&h.svm,h.ownership);
    assert_eq!((reward_after.supply,reward_after.fee_pending),(rewards.supply,rewards.fee_pending));
    assert!(c.receipts[0].settled);h.send("coverage settlement cannot replay",instructions,true);
}

#[test]
fn primary_token_losses_never_consume_stake_or_record_a_backing_shortfall() {
    for(staked,liquidated)in[(500_000*UNIT,false),(50*UNIT,true)] {
        let mut h=Bank::new();let primary=h.tokens[0];let owner=h.signer.pubkey();let margin=100*UNIT;
        h.send("deposit stake and independent bet",vec![h.deposit(primary,staked+margin)],false);
        let intent=h.committed(primary,6,margin,liquidated,false);
        let mut t:ErTrader=get(&h.svm,h.trader);t.core_stake.tokens=staked;put(&mut h.svm,h.trader,&t,p::basebridge::DELEGATION_OWNER);
        let position=pda(&[b"position",owner.as_ref(),&6u64.to_le_bytes()]);
        let before:Inventory=get(&h.svm,primary.inventory);let vault=token(&h.svm,primary.vault);
        let ixs=h.settlement(primary,intent,1);h.send("loss coverage settles with bounded stake",ixs.clone(),false);
        let x:Position=get(&h.svm,position);let after:Inventory=get(&h.svm,primary.inventory);let c:ErCustody=get(&h.svm,h.custody);
        let target=p::stake_backing::loss_shortfall(intent.entry_value,x.collateral_close_value,x.loss,margin).unwrap();
        assert!(target>0,"collateral did depreciate");assert_eq!((x.coverage_tokens,x.coverage_usd,x.uncovered_usd),(0,0,0));assert_eq!(x.profit,0);
        assert_eq!(after.locked,before.locked-x.loss-x.coverage_tokens);
        assert_eq!(after.treasury,before.treasury+x.swapped_out+x.coverage_tokens);
        assert_eq!(c.assets[0].available,staked+margin-x.loss-x.coverage_tokens);
        assert_eq!(c.core_coverage.incurred_usd,x.uncovered_usd);assert!(c.receipts[0].settled);
        let receipt:p::OwnershipReceipt=get(&h.svm,pda(&[b"ownership-receipt",position.as_ref()]));assert_eq!(receipt.pool_tokens,x.swapped_out);
        // Coverage is an accounting transfer within custody, so only ordinary
        // gas conversion and burns can remove tokens from this primary vault.
        assert!(token(&h.svm,primary.vault)>=vault-x.loss);
        assert_eq!(get::<ErTrader>(&h.svm,h.trader).core_stake.tokens,staked);
        if !liquidated {assert_eq!(x.loss,margin/10);}
        h.send("loss coverage cannot replay",ixs,true);
    }
}

#[test]
fn real_deposits_reuse_exhausted_records_and_er_rejects_retired_snapshot_credits() {
    let mut h=Bank::new();let core=h.tokens[0];let usd=h.tokens[1];let sol=h.tokens[2];
    let mut c:ErCustody=get(&h.svm,h.custody);let mut t:ErTrader=get(&h.svm,h.trader);
    for i in 0..ER_ASSETS {
        let asset=if i==0{core.asset}else if i==1{usd.asset}else{Pubkey::new_unique()};
        c.assets[i]=CustodyAsset{asset,deposited_credits:1000,..CustodyAsset::default()};
        t.assets[i]=ErBalance{asset,credited_deposits:1000,..ErBalance::default()};
    }
    put(&mut h.svm,h.custody,&c,p::ID);put(&mut h.svm,h.trader,&t,p::ID);
    let original=h.svm.get_account(&h.custody).unwrap();let sol_before=token(&h.svm,sol.wallet);let usd_before=token(&h.svm,usd.wallet);
    h.send("failed deposit cannot retire a record or advance its generation",vec![h.deposit(sol,sol_before+1)],true);
    assert_eq!(get::<ErCustody>(&h.svm,h.custody).generation,0);
    h.send("real SOL deposit replaces an exhausted non-core record",vec![h.deposit(sol,7)],false);
    let middle=h.svm.get_account(&h.custody).unwrap();
    h.send("revisited USDC starts a fresh credit generation",vec![h.deposit(usd,3)],false);
    let latest=h.svm.get_account(&h.custody).unwrap();
    assert_eq!(token(&h.svm,sol.wallet),sol_before-7);assert_eq!(token(&h.svm,usd.wallet),usd_before-3);
    // Inject only the ER execution boundary. The actual compiled ErSync runs
    // against the real custody bytes produced by both token-transfer CPIs.
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    h.svm.set_account(magic,Account{lamports:1_000_000,data:vec![],owner:pk("NativeLoader1111111111111111111111111111111"),executable:true,rent_epoch:0}).unwrap();
    let sync=ix(p::accounts::SyncErTrader{er_trader:h.trader,er_risk:pda(&[b"er-risk"]),er_custody:h.custody,
        execution:p::accounts::ErExecution{magic_program:magic}},p::instruction::ErSync{});
    h.send("reconcile fresh lifetimes on the compiled ER ledger",vec![sync.clone()],false);
    for old in [original,middle,latest.clone()] {
        h.svm.set_account(h.custody,old).unwrap();h.send("older or repeated generations cannot create credit",vec![sync.clone()],false);
        let actual:ErTrader=get(&h.svm,h.trader);assert_eq!(actual.custody_generation,2);
        assert_eq!(actual.assets[actual.asset_index(usd.asset).unwrap()].available,3);
        assert_eq!(actual.assets[actual.asset_index(sol.asset).unwrap()].available,7);
        assert_eq!(actual.assets.iter().map(|a|a.available).sum::<u64>(),10);
        assert_eq!(actual.assets[0].asset,core.asset);
    }
    h.svm.set_account(h.custody,latest).unwrap();
    h.send("three full-ledger sync instructions fit the runtime budget",vec![sync;3],false);
    assert!(h.receipts.last().unwrap()["computeUnits"].as_u64().unwrap()<=1_200_000);
}

#[test]
fn complete_base_loss_routes_rewards_fifo_payout_and_withdrawal() {
    let mut h=Bank::new();let primary=h.tokens[0];let usd=h.tokens[1];let sol=h.tokens[2];let owner=h.signer.pubkey();
    for(a,amount)in[(usd,50*UNIT),(primary,100_000*UNIT),(sol,1_000_000_000)] {h.send("deposit real token collateral",vec![h.deposit(a,amount)],false);}
    h.admin("pause new entries while completing existing liabilities",vec![ix(p::accounts::Admin{
        admin:p::mainnet_config::ADMIN,config:h.config},p::instruction::Pause{paused:true})],false);
    h.send("deposit rejected while paused",vec![h.deposit(usd,UNIT)],true);
    for(nonce,a,margin,liquidated)in[(1,usd,10*UNIT,true),(2,primary,10_000*UNIT,true),(3,sol,1_000_000_000,false)] {
        let intent=h.committed(a,nonce,margin,liquidated,false);let position=pda(&[b"position",owner.as_ref(),&nonce.to_le_bytes()]);
        if nonce==1 {h.send("failed final swap rolls back complete settlement",h.settlement(a,intent,u64::MAX),true);assert!(h.svm.get_account(&position).is_none());}
        let before=get::<ErCustody>(&h.svm,h.custody);let supply_before=supply(&h.svm,primary.mint);
        let reward_before=get::<OwnershipPool>(&h.svm,h.ownership).fee_pending;let pool_before=get::<Inventory>(&h.svm,primary.inventory).treasury;
        let gas_before=get::<GasReserve>(&h.svm,h.gas).sol_received;let lamports_before=h.svm.get_account(&h.gas).unwrap().lamports;
        h.send("settle loss with actual receipts",h.settlement(a,intent,1),false);
        let x:Position=get(&h.svm,position);assert_eq!(x.loss,if liquidated {margin}else{margin/10});assert_eq!(x.returned,margin-x.loss);
        let after:ErCustody=get(&h.svm,h.custody);let index=before.asset_index(a.asset).unwrap();
        assert_eq!(before.assets[index].available-after.assets[index].available,x.loss);
        assert_eq!(after.assets[index].returned_credits-before.assets[index].returned_credits,x.returned);
        let burned=supply_before-supply(&h.svm,primary.mint);let reward=get::<OwnershipPool>(&h.svm,h.ownership).fee_pending-reward_before;
        let pool_added=get::<Inventory>(&h.svm,primary.inventory).treasury-pool_before;let total=pool_added+reward+burned;
        assert_eq!((pool_added,reward,burned),p::math::acid_split(total).unwrap());assert!(burned>0&&reward>0);
        if a.mint==primary.mint {assert_eq!(total,x.loss-x.loss/100);}
        let gas_delta=get::<GasReserve>(&h.svm,h.gas).sol_received-gas_before;assert!(gas_delta>0);
        assert_eq!(h.svm.get_account(&h.gas).unwrap().lamports-lamports_before,gas_delta);assert_eq!(token(&h.svm,h.gas_vault),0);
        assert!(token(&h.svm,h.sol_vault)>=10_000);assert_eq!(token(&h.svm,h.usd_vault),100);
        h.send("settlement cannot replay",h.settlement(a,intent,1),true);
    }
    // The mainnet server uses a separate sponsor. Execute its actual two client
    // instructions and prove a failed forwarding transfer rolls back the refill.
    let publisher=p::mainnet_config::PUBLISHER;
    h.svm.set_account(publisher,Account{lamports:1_000_000,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
    let refill=h.client_fixture()["sponsorRefill"].clone();
    let amount=refill["amount"].as_u64().unwrap();assert_eq!(amount,1_000_000);
    let refill_ix=Bank::client_instructions(&refill["instructions"]);assert_eq!(refill_ix.len(),2);
    let mut failed=refill_ix.clone();failed[1].data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
    h.admin("failed sponsor forwarding rolls back gas refill",failed,true);
    let gas_before=h.svm.get_account(&h.gas).unwrap().lamports;
    let spent_before=get::<GasReserve>(&h.svm,h.gas).sol_spent;
    let sponsor_before=h.svm.get_account(&owner).unwrap().lamports;
    h.admin("forward authorized gas refill to separate sponsor",refill_ix,false);
    assert_eq!(h.svm.get_account(&publisher).unwrap().lamports,1_000_000);
    assert_eq!(h.svm.get_account(&h.gas).unwrap().lamports,gas_before-amount);
    assert_eq!(get::<GasReserve>(&h.svm,h.gas).sol_spent,spent_before+amount);
    assert_eq!(h.svm.get_account(&owner).unwrap().lamports,sponsor_before+amount-10_000);
    let ownership_trader=pda(&[b"ownership-trader",h.pool.as_ref(),owner.as_ref()]);
    let earned=get::<OwnershipTrader>(&h.svm,ownership_trader).available;assert!(earned>0);
    h.send("stake actual earned ownership",vec![ix(p::accounts::StakeOwnership{owner,pool:h.pool,ownership_pool:h.ownership,ownership_trader,inventory:primary.inventory},p::instruction::StakeOwnership{amount:earned,unstake:false})],false);
    let before=token(&h.svm,primary.wallet);let reserved=get::<OwnershipPool>(&h.svm,h.ownership).reward_pool;
    h.send("claim funded staker rewards",vec![ix(p::accounts::ClaimOwnershipRewards{owner,pool:h.pool,ownership_pool:h.ownership,ownership_trader,
        asset:primary.asset,authority:h.authority,mint:primary.mint,token_program:primary.program,vault:primary.vault,destination:primary.wallet},p::instruction::ClaimOwnershipRewards{})],false);
    let rewards=token(&h.svm,primary.wallet)-before;assert!(rewards>0&&rewards<=reserved);
    let win=h.committed(usd,4,10*UNIT,false,true);let position=pda(&[b"position",owner.as_ref(),&4u64.to_le_bytes()]);
    h.send("settle winning FIFO claim",h.settlement(usd,win,1),false);let x:Position=get(&h.svm,position);
    assert_eq!(x.close_value,win.entry_value);assert_eq!(x.profit,p::math::pnl(win.entry_value,win.entry,win.exit,win.leverage,true).unwrap().1);assert!(x.profit>0);
    let next=h.committed(usd,5,10*UNIT,false,true);let next_position=pda(&[b"position",owner.as_ref(),&5u64.to_le_bytes()]);
    h.send("queue second winning claim",h.settlement(usd,next,1),false);
    h.send("second claim cannot skip FIFO head",h.payout(next_position),true);
    let balance=token(&h.svm,usd.wallet);let reserve=token(&h.svm,h.sol_vault);
    h.send("pay FIFO claim through SDICE SOL USDC",h.payout(position),false);
    let paid=token(&h.svm,usd.wallet)-balance;let settled:Position=get(&h.svm,position);
    assert!(paid>0&&paid<=x.profit);assert!(x.profit-paid-settled.remaining<=1);assert_eq!(settled.remaining,0);
    assert_eq!(get::<Pool>(&h.svm,h.pool).queue_usd,x.profit);assert!(token(&h.svm,h.sol_vault)>=reserve);assert_eq!(token(&h.svm,h.usd_vault),100);
    h.send("paid FIFO claim cannot replay",h.payout(position),true);
    h.send("pay next FIFO claim",h.payout(next_position),false);assert_eq!(get::<Pool>(&h.svm,h.pool).queue_usd,0);
    // Return the SOL left after the losing position, using its committed withdrawal intent.
    let c:ErCustody=get(&h.svm,h.custody);let left=c.assets[c.asset_index(sol.asset).unwrap()].available;
    let mut t:ErTrader=get(&h.svm,h.trader);t.next_nonce=7;t.slots[0]=ErSlot{state:ER_PENDING,kind:ER_WITHDRAW,nonce:6,asset:sol.asset,margin:left,..ErSlot::default()};
    put(&mut h.svm,h.trader,&t,p::basebridge::DELEGATION_OWNER);let before=token(&h.svm,sol.wallet);
    let withdrawal=ix(p::accounts::SettleWithdrawal{caller:owner,config:h.config,pool:h.pool,custody:h.custody,trader:h.trader,asset:sol.asset,mint:SOL,token_program:sol.program,inventory:sol.inventory,authority:h.authority,vault:sol.vault,destination:sol.wallet},p::instruction::SettleWithdrawal{nonce:6,slot:0});
    h.send("withdraw remaining native collateral",vec![withdrawal.clone()],false);assert_eq!(token(&h.svm,sol.wallet)-before,left);
    h.send("withdrawal cannot replay",vec![withdrawal],true);
    // All primary vault tokens are accounted for, including still-locked deposits.
    let inv:Inventory=get(&h.svm,primary.inventory);let rewards:OwnershipPool=get(&h.svm,h.ownership);
    assert_eq!(token(&h.svm,primary.vault),inv.locked+inv.treasury+inv.dust+rewards.fee_pending+rewards.reward_pool);
    // Read allocated bytes from accounts created by the actual candidate. The
    // IDL coder's generic Vec size is not the contract's fixed-capacity rent
    // allocation (notably ErRisk and CollateralHistory).
    let first_position=pda(&[b"position",owner.as_ref(),&1u64.to_le_bytes()]);
    let account_sizes:Vec<_>=[
        ("config",h.config),("pool",h.pool),("ownershipPool",h.ownership),("gasReserve",h.gas),
        ("feed",h.market_feed),("market",h.market),("asset",primary.asset),("inventory",primary.inventory),
        ("erRisk",pda(&[b"er-risk"])),("erFeePayer",pda(&[b"er-fee-payer"])),
        ("erQuote",pda(&[b"er-quote",primary.asset.as_ref()])),
        ("collateralHistory",pda(&[b"er-collateral-history",primary.asset.as_ref()])),
        ("erTrader",h.trader),("erCustody",h.custody),("collateralGuard",h.guard),
        ("ownershipTrader",ownership_trader),("position",first_position),
        ("ownershipReceipt",pda(&[b"ownership-receipt",first_position.as_ref()])),
        ("userExposure",pda(&[b"exposure",owner.as_ref()])),
    ].iter().map(|(name,address)| {
        let account=h.svm.get_account(address).unwrap();assert!(!account.executable);
        assert!(account.owner==p::ID||account.owner==p::basebridge::DELEGATION_OWNER);
        json!({"name":name,"address":address.to_string(),"bytes":account.data.len()})
    }).collect();
    let token_account_sizes:Vec<_>=[("primaryCollateral",primary.vault),("usdcCollateral",usd.vault),
        ("solCollateral",sol.vault),("usdcPayout",h.usd_vault),("solIntermediate",h.sol_vault),("gasConversion",h.gas_vault)]
        .iter().map(|(name,address)| {
            let account=h.svm.get_account(address).unwrap();
            assert!(account.owner==anchor_spl::token::ID||account.owner==anchor_spl::token_2022::ID);
            json!({"name":name,"address":address.to_string(),"bytes":account.data.len(),"tokenProgram":account.owner.to_string()})
        }).collect();
    let root=Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    std::fs::write(root.join(report_path("base-settlement")),serde_json::to_vec_pretty(&json!({
        "mainnetTransactionsSubmitted":0,"localSimulation":true,"programId":p::ID.to_string(),
        "binarySha256":hash(root.join(candidate_binary())),
        "fixtureSha256":hash(root.join(".cache/acidpump-mainnet-execution/fixture.json")),"receipts":h.receipts,
        "scope":"Actual mainnet candidate initialization, configured wallet admission, deposits, 3 loss routes, staking, rewards, profit/FIFO USDC payout and withdrawal; only native ER commit boundary injected",
        "administrativeSignaturesSimulated":true,"publisherSignaturesSimulated":true,"privateKeysLoaded":false,
        "clientInstructionsExecuted":true,"clientFixtureSha256":hash(root.join(client_fixture_path())),
        "accountSizes":account_sizes,"tokenAccountSizes":token_account_sizes,
        "nativeErLifecycleVerified":false,"initializationVerified":true,
        "admissionVerified":true,"pausedSettlementVerified":true,
        "productionPolicy":if p::risk::ROLLING_CAPACITY {serde_json::from_str::<Value>(include_str!("../../../projects/acidpump-er/src/production-policy.json")).unwrap()} else {Value::Null},
        "testLimits":if p::risk::ROLLING_CAPACITY {Value::Null}else{json!({"maximumEntryUsd":p::mainnet_config::MAX_ENTRY_USD/UNIT,
            "maximumWalletNotionalUsd":p::risk::MAX_NOTIONAL/UNIT,
            "maximumMarketSideNotionalUsd":p::risk::OI_HEADROOM/UNIT})}
    })).unwrap()).unwrap();
}
