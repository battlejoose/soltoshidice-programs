#![cfg(feature="mainnet")]
//! Actual mainnet candidate SBF; synthetic accounts, clock and ER builtin.
//! Tests instruction enforcement, not native MagicBlock transport or live trades.
use acidpump_er::{self as p, engine::*, oracle, Asset, CollateralGuard, CollateralHistory, Config, Pool};
use anchor_lang::{prelude::{Clock, Pubkey}, AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
    solana_program::instruction::Instruction};
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{path::Path, str::FromStr};
const NOW: i64 = 1_800_000_000;
const PX: u64 = 1_000_000_000_000;
fn derive(seeds: &[&[u8]]) -> (Pubkey,u8) { Pubkey::find_program_address(seeds,&p::ID) }
fn put<T:AccountSerialize>(svm:&mut LiteSVM,key:Pubkey,value:&T) {
    let mut data=vec![]; value.try_serialize(&mut data).unwrap();
    svm.set_account(key,Account{lamports:svm.minimum_balance_for_rent_exemption(data.len()),data,owner:p::ID,executable:false,rent_epoch:0}).unwrap();
}
fn read<T:AccountDeserialize>(svm:&LiteSVM,key:Pubkey)->T { T::try_deserialize(&mut &svm.get_account(&key).unwrap().data[..]).unwrap() }
struct Harness { svm:LiteSVM, owner:Keypair, config:Pubkey, pool:Pubkey, trader:Pubkey, risk:Pubkey, asset:Pubkey, quote:Pubkey, history:Pubkey, guard:Pubkey, magic:Pubkey }
impl Harness {
    fn new(venue:bool)->Self {
        let mut svm=LiteSVM::new();
        let binary=std::env::var("ACIDPUMP_TEST_BINARY").expect("select the exact candidate SBF");
        svm.add_program(p::ID,&std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(binary)).unwrap()).unwrap();
        let owner=Keypair::new_from_array([67;32]);svm.airdrop(&owner.pubkey(),1_000_000_000).unwrap();
        let (config,cb)=if venue {derive(&[b"venue-config",owner.pubkey().as_ref()])}else{derive(&[b"config"])};
        let ns=p::venue::ns(config);
        let (pool,pb)=derive(&[b"pool",ns.as_ref(),&0u16.to_le_bytes()]);
        let (trader,tb)=derive(&[b"er-trader",ns.as_ref(),owner.pubkey().as_ref()]);
        let (risk,rb)=derive(&[b"er-risk",ns.as_ref()]);
        let (asset,ab)=derive(&[b"asset",p::mainnet_pump::USDC.as_ref()]);
        let (quote,qb)=derive(&[b"er-quote",asset.as_ref()]);
        let (history,hb)=derive(&[b"er-collateral-history",asset.as_ref()]);
        let (guard,gb)=derive(&[b"er-collateral-guard",ns.as_ref(),owner.pubkey().as_ref()]);
        put(&mut svm,config,&Config{admin:owner.pubkey(),publisher:owner.pubkey(),quote_mint:p::mainnet_pump::USDC,paused:false,bump:cb,next_settlement_seq:1});
        put(&mut svm,pool,&Pool{id:0,settlement:asset,authority_bump:0,queue_head:0,queue_tail:0,queue_usd:0,trades:0,bump:pb});
        let mut t=ErTrader{owner:owner.pubkey(),config,pool,next_nonce:1,bump:tb,..ErTrader::default()};
        t.assets[0]=ErBalance{asset,available:100_000_000,..ErBalance::default()};put(&mut svm,trader,&t);
        put(&mut svm,risk,&ErRisk{config,bump:rb,..ErRisk::default()});
        put(&mut svm,asset,&Asset{id:104,feed:Pubkey::default(),mint:p::mainnet_pump::USDC,token_program:anchor_spl::token::ID,
            source_mint:Pubkey::default(),source_pool:Pubkey::default(),quote_mint:p::mainnet_pump::USDC,dex_pool:Pubkey::default(),collateral_enabled:true,decimals:6,bump:ab});
        put(&mut svm,quote,&ErQuote{config:p::ROOT_CONFIG,asset,is_quote:true,publish_time:NOW,source_slot:1,bump:qb,..ErQuote::default()});
        put(&mut svm,history,&CollateralHistory{config:p::ROOT_CONFIG,asset,sequence:0,source_slot:1,publish_time:NOW,last_gap:0,last_gap_start:0,is_quote:true,decimals:6,samples:vec![],bump:hb});
        put(&mut svm,guard,&CollateralGuard{owner:owner.pubkey(),config,floors:CollateralGuard::empty_floors(),bump:gb});
        let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
        svm.set_account(magic,Account{lamports:1_000_000,data:vec![],executable:true,owner:Pubkey::from_str("NativeLoader1111111111111111111111111111111").unwrap(),rent_epoch:0}).unwrap();
        let mut h=Self{svm,owner,config,pool,trader,risk,asset,quote,history,guard,magic};h.price(0,100*PX);h
    }
    fn price(&mut self,age:i64,price:u64) {
        let mut clock:Clock=self.svm.get_sysvar();clock.unix_timestamp=NOW+age;clock.slot+=1;self.svm.set_sysvar(&clock);
        let mut r:ErRisk=read(&self.svm,self.risk);r.markets[0].sample(oracle::OraclePrice{price,publish_time:NOW+age,posted_slot:clock.slot}).unwrap();put(&mut self.svm,self.risk,&r);
    }
    fn open(&mut self,long:bool) {
        let accounts=p::accounts::OpenErPosition{sol_backing_quote:None,usd_backing_quote:None,owner:self.owner.pubkey(),config:self.config,er_trader:self.trader,er_risk:self.risk,asset:self.asset,
            er_quote:self.quote,history:self.history,guard:self.guard,execution:p::accounts::ErExecution{magic_program:self.magic},core_quote:self.quote,
            gas_authority:self.owner.pubkey(),pool:self.pool,pool_exposure:None};
        self.send(Instruction{program_id:p::ID,accounts:accounts.to_account_metas(None),data:p::instruction::ErOpen{slot:0,nonce:1,market:0,amount:10_000_000,leverage:1000,long,
            limit_price:if long {u64::MAX}else{0},min_value:0,max_value:u64::MAX,expires:NOW+60}.data()},None);
    }
    fn close(&mut self,expected:Option<&str>) {
        let accounts=p::accounts::CloseErPosition{sol_backing_quote:None,usd_backing_quote:None,caller:self.owner.pubkey(),er_trader:self.trader,er_risk:self.risk,er_quote:self.quote,history:self.history,guard:self.guard,
            execution:p::accounts::ErExecution{magic_program:self.magic},core_quote:self.quote,config:self.config,pool:self.pool};
        self.send(Instruction{program_id:p::ID,accounts:accounts.to_account_metas(None),data:p::instruction::ErClose{slot:0,nonce:1,expires:NOW+60}.data()},expected);
    }
    fn send(&mut self,instruction:Instruction,expected:Option<&str>) {
        self.svm.expire_blockhash();
        let mut heap=vec![1];heap.extend_from_slice(&(128u32*1024).to_le_bytes());
        let ixs=[Instruction{program_id:Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(),accounts:vec![],data:heap},instruction];
        let before:Vec<_>=ixs[1].accounts.iter().map(|a|(a.pubkey,self.svm.get_account(&a.pubkey))).collect();
        let message=v0::Message::try_compile(&self.owner.pubkey(),&ixs,&[],self.svm.latest_blockhash()).unwrap();
        let result=self.svm.send_transaction(VersionedTransaction::try_new(VersionedMessage::V0(message),&[&self.owner]).unwrap());
        match expected {Some(reason)=>{
            let failure=result.unwrap_err();assert!(failure.meta.logs.iter().any(|l|l.contains(reason)),"{reason}: {failure:?}");
            for(key,old)in before {let new=self.svm.get_account(&key);assert_eq!(old.as_ref().map(|a|&a.data),new.as_ref().map(|a|&a.data));if key!=self.owner.pubkey(){assert_eq!(old,new);}}
        },None=>{result.unwrap();}}
    }
}
#[test]
fn root_and_venue_enforce_five_seconds_for_long_and_short() {
    for venue in [false,true] {for long in [false,true] {
        let mut h=Harness::new(venue);h.open(long);
        let opened:ErTrader=read(&h.svm,h.trader);
        assert_eq!(opened.slots[0].opened_at,NOW);
        assert_eq!(opened.slots[0].bust,if long {100*PX-PX/10}else{100*PX+PX/10});
        for age in [0,1,4] {h.price(age,100*PX);h.close(Some("MinimumHold"));}
        h.price(5,100*PX);h.close(None);
        let closed:ErTrader=read(&h.svm,h.trader);assert_eq!(closed.slots[0].state,ER_PENDING);assert_eq!(closed.slots[0].closed_at,NOW+5);
        h.close(Some("Closed"));
    }}
}
#[test]
fn root_and_venue_can_liquidate_during_countdown() {
    for venue in [false,true] {for long in [false,true] {
        let mut h=Harness::new(venue);h.open(long);
        h.price(1,if long {100*PX-PX/20}else{100*PX+PX/20});h.close(Some("MinimumHold"));
        h.price(2,if long {100*PX-PX/10}else{100*PX+PX/10});h.close(None);
        let closed:ErTrader=read(&h.svm,h.trader);assert!(closed.slots[0].liquidated);assert_eq!(closed.slots[0].closed_at,NOW+2);
    }}
}

#[test]
fn mixed_reserve_entry_unlock_and_stale_quote_rules_execute_in_sbf() {
    for venue in [false,true] {
        let mut h=Harness::new(venue);let core=h.asset;let core_quote=h.quote;
        let mint=Pubkey::new_unique();let(asset,ab)=derive(&[b"asset",mint.as_ref()]);let(quote,qb)=derive(&[b"er-quote",asset.as_ref()]);
        let(history,hb)=derive(&[b"er-collateral-history",asset.as_ref()]);
        let sol=sol_collateral_asset();let(sol_quote,sb)=derive(&[b"er-quote",sol.as_ref()]);
        put(&mut h.svm,asset,&Asset{id:110,feed:Pubkey::default(),mint,token_program:anchor_spl::token::ID,source_mint:mint,source_pool:Pubkey::default(),
            quote_mint:p::mainnet_pump::SOL,dex_pool:Pubkey::default(),collateral_enabled:true,decimals:6,bump:ab});
        put(&mut h.svm,quote,&ErQuote{config:p::ROOT_CONFIG,asset,base_reserve:1_000_000_000,quote_reserve:10_000_000_000,publish_time:NOW,source_slot:1,bump:qb,
            mainnet:p::MainnetCollateralContext{real_quote_reserve:10_000_000_000,pool_source_slot:1,usd_per_quote_atom_e21:100*PX,conversion_publish_time:NOW,conversion_source_slot:1},..ErQuote::default()});
        put(&mut h.svm,history,&CollateralHistory{config:p::ROOT_CONFIG,asset,sequence:0,source_slot:1,publish_time:NOW,last_gap:0,last_gap_start:0,is_quote:false,decimals:6,samples:vec![],bump:hb});
        put(&mut h.svm,sol_quote,&ErQuote{config:p::ROOT_CONFIG,asset:sol,quote_reserve:100*PX,publish_time:NOW,source_slot:1,bump:sb,..ErQuote::default()});
        let mut t:ErTrader=read(&h.svm,h.trader);t.assets[1]=ErBalance{asset:sol,available:200_000_000,..ErBalance::default()};
        t.assets[2]=ErBalance{asset,available:100_000_000,..ErBalance::default()};put(&mut h.svm,h.trader,&t);
        let change=|h:&Harness,asset,amount,unstake|Instruction{program_id:p::ID,
            accounts:p::accounts::ChangeErCoreStake{owner:h.owner.pubkey(),config:h.config,pool:h.pool,er_trader:h.trader,execution:p::accounts::ErExecution{magic_program:h.magic}}.to_account_metas(None),
            data:p::instruction::ErChangeBacking{asset,amount,unstake}.data()};
        h.send(change(&h,core,10_000_000,false),None);h.send(change(&h,sol,200_000_000,false),None);
        let open=|h:&Harness,sol_quote|Instruction{program_id:p::ID,accounts:p::accounts::OpenErPosition{owner:h.owner.pubkey(),config:h.config,pool:h.pool,
            er_trader:h.trader,er_risk:h.risk,asset,er_quote:quote,history,guard:h.guard,core_quote,sol_backing_quote:sol_quote,usd_backing_quote:None,
            gas_authority:h.owner.pubkey(),pool_exposure:None,execution:p::accounts::ErExecution{magic_program:h.magic}}.to_account_metas(None),
            data:p::instruction::ErOpen{slot:0,nonce:1,market:0,amount:25_000_000,leverage:10,long:true,limit_price:u64::MAX,min_value:0,max_value:u64::MAX,expires:NOW+60}.data()};
        h.send(open(&h,None),Some("Stale"));
        let mut stale:ErQuote=read(&h.svm,sol_quote);stale.publish_time=NOW-10;put(&mut h.svm,sol_quote,&stale);
        h.send(open(&h,Some(sol_quote)),Some("Stale"));stale.publish_time=NOW;put(&mut h.svm,sol_quote,&stale);
        h.send(open(&h,Some(sol_quote)),None);
        let t:ErTrader=read(&h.svm,h.trader);assert!(t.slots[0].entry_value>20_000_000&&t.slots[0].entry_value<30_000_000);
        assert_eq!(t.core_stake.tokens,10_000_000);assert_eq!(t.auxiliary_stakes[0].tokens,200_000_000);assert_eq!(t.auxiliary_stakes[1].tokens,0);
        h.send(change(&h,core,1,true),Some("Closed"));h.send(change(&h,sol,1,true),Some("Closed"));
        h.send(change(&h,Pubkey::new_unique(),1,false),Some("Binding"));
    }
}
