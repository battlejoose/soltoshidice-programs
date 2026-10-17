//! Real public Pump curve snapshots with independent official-SDK sell quotes.
//! These tests cover valuation, not CPI execution or live migration transport.
use acidpump_er::{mainnet_curve::{self,SolCurveAccounts},mainnet_pump::SolPoolQuote};
use anchor_lang::prelude::{AccountInfo,Pubkey,Rent};
use base64::{engine::general_purpose::STANDARD,Engine};
use serde_json::Value;
use std::{collections::BTreeMap,str::FromStr};

fn pk(s:&str)->Pubkey{Pubkey::from_str(s).unwrap()}
fn global()->Pubkey{Pubkey::find_program_address(&[b"global"],&mainnet_curve::ID).0}
fn fees()->Pubkey{Pubkey::find_program_address(&[b"fee_config",mainnet_curve::ID.as_ref()],&mainnet_curve::FEES_ID).0}
struct Row{key:Pubkey,owner:Pubkey,data:Vec<u8>,lamports:u64,executable:bool}
struct Fixture{curve:Pubkey,mint:Pubkey,vault:Pubkey,rows:BTreeMap<Pubkey,Row>,rent:Rent,reference:Value}
impl Fixture {
    fn load(index:usize)->Self {
        let source:Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/mainnet-pump-curves.json")).unwrap();
        let row=&source["candidates"][index];
        let rows:BTreeMap<_,_>=source["accounts"].as_object().unwrap().iter().map(|(key,a)|{
            let key=pk(key);(key,Row{key,owner:pk(a["owner"].as_str().unwrap()),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),lamports:a["lamports"].as_u64().unwrap(),executable:a["executable"].as_bool().unwrap()})
        }).collect();
        let rent=&rows[&pk("SysvarRent111111111111111111111111111111111")].data;
        Self{curve:pk(row["curve"].as_str().unwrap()),mint:pk(row["mint"].as_str().unwrap()),vault:pk(row["vault"].as_str().unwrap()),
            rent:Rent{lamports_per_byte_year:u64::from_le_bytes(rent[..8].try_into().unwrap()),exemption_threshold:f64::from_le_bytes(rent[8..16].try_into().unwrap()),burn_percent:rent[16]},
            rows,reference:source["quoteReference"]["curves"][index].clone()}
    }
    fn quote(&mut self,allow_empty:bool)->anchor_lang::Result<SolPoolQuote> {
        let infos:BTreeMap<_,_>=self.rows.values_mut().map(|r|(r.key,AccountInfo::new(&r.key,false,false,&mut r.lamports,&mut r.data,&r.owner,r.executable))).collect();
        SolCurveAccounts{curve:&infos[&self.curve],mint:&infos[&self.mint],base_vault:&infos[&self.vault],global:&infos[&global()],fees:&infos[&fees()]}
            .validate(self.curve,self.mint,&self.rent,allow_empty)
    }
}
#[test]
fn real_regular_and_mayhem_curves_match_21_independent_sdk_quotes() {
    for index in 0..3 {
        let mut f=Fixture::load(index);let q=f.quote(false).unwrap();
        assert_eq!(q.fees,[0,95,30]);
        for sample in f.reference["sells"].as_array().unwrap() {
            let amount=sample["amount"].as_str().unwrap().parse().unwrap();
            if sample["executable"].as_bool().unwrap() {
                let expected=sample["netLamports"].as_str().unwrap().parse::<u64>().unwrap();
                assert_eq!(q.sell(amount).unwrap(),expected);assert_eq!(q.collateral_lamports(amount).unwrap(),expected);
            } else {assert!(q.sell(amount).is_err());assert_eq!(q.collateral_lamports(amount).unwrap(),0);}
        }
    }
}
#[test]
fn fees_donations_and_rent_are_not_executable_curve_liquidity() {
    let mut f=Fixture::load(1);let q=f.quote(false).unwrap();
    let data=&f.rows[&f.curve].data;assert_eq!(q.real_quote,u64::from_le_bytes(data[32..40].try_into().unwrap()));
    assert_eq!(q.effective_quote().unwrap(),u64::from_le_bytes(data[16..24].try_into().unwrap()));
    f.rows.get_mut(&f.curve).unwrap().lamports+=10_000_000_000;assert_eq!(f.quote(false).unwrap(),q);
    let row=f.rows.get_mut(&f.curve).unwrap();
    row.lamports=q.real_quote+u64::from_le_bytes(row.data[125..133].try_into().unwrap())+u64::from_le_bytes(row.data[133..141].try_into().unwrap());
    assert!(f.quote(false).is_err(),"account rent cannot pay a sale");
}
#[test]
fn drained_curve_is_zero_but_complete_curve_requires_migration_route() {
    let mut f=Fixture::load(1);f.rows.get_mut(&f.curve).unwrap().data[32..40].fill(0);
    assert!(f.quote(false).is_err());let q=f.quote(true).unwrap();assert_eq!(q.fees,[0;3]);assert_eq!(q.collateral_lamports(u64::MAX).unwrap(),0);
    f.rows.get_mut(&f.curve).unwrap().data[48]=1;assert!(f.quote(true).is_err());
}
#[test]
fn legacy_curve_and_spl_layouts_preserve_quotes_without_partial_fields() {
    for size in [49,81,82,83,115,123,124,125,133,141,151,166,180] {
        let mut f=Fixture::load(1);f.rows.get_mut(&f.curve).unwrap().data.resize(size,0);
        let mint=f.rows.get_mut(&f.mint).unwrap();mint.owner=anchor_spl::token::ID;mint.data.truncate(82);
        let mut vault=f.rows.remove(&f.vault).unwrap();vault.owner=anchor_spl::token::ID;vault.data.truncate(165);
        vault.key=Pubkey::find_program_address(&[f.curve.as_ref(),anchor_spl::token::ID.as_ref(),f.mint.as_ref()],&anchor_spl::associated_token::ID).0;
        f.vault=vault.key;f.rows.insert(vault.key,vault);
        let q=f.quote(false).unwrap();assert_eq!(q.fees,if size<81{[0,95,0]}else{[0,95,30]});assert!(q.sell(1_000_000_000).unwrap()>0);
    }
    for size in [48,50,80,84,114,116,122,126,132,134,140] {
        let mut f=Fixture::load(1);f.rows.get_mut(&f.curve).unwrap().data.truncate(size);assert!(f.quote(false).is_err(),"partial {size}");
    }
}
#[test]
fn curve_fee_tiers_use_live_supply_only_for_mayhem() {
    let mut f=Fixture::load(1);
    let data=&f.rows[&f.curve].data;
    let virtual_base=u64::from_le_bytes(data[8..16].try_into().unwrap());let virtual_quote=u64::from_le_bytes(data[16..24].try_into().unwrap());
    let threshold=750_000_000_000_000u128*virtual_quote as u128/virtual_base as u128;
    f.rows.get_mut(&f.mint).unwrap().data[36..44].copy_from_slice(&500_000_000_000_000u64.to_le_bytes());
    let fee_data=&mut f.rows.get_mut(&fees()).unwrap().data;let old=fee_data.clone();
    let mut next=old[..65].to_vec();next.extend_from_slice(&2u32.to_le_bytes());next.extend_from_slice(&old[69..109]);
    next.extend_from_slice(&threshold.to_le_bytes());for value in [0u64,200,10]{next.extend_from_slice(&value.to_le_bytes());}next.extend_from_slice(&old[109..]);*fee_data=next;
    assert_eq!(f.quote(false).unwrap().fees,[0,200,10],"regular curve uses fixed one-billion supply even after burns");
    f.rows.get_mut(&f.curve).unwrap().data[81]=1;assert_eq!(f.quote(false).unwrap().fees,[0,95,30],"Mayhem curve uses live supply");
}
#[test]
fn curve_account_substitution_modes_and_mint_authorities_are_rejected() {
    let f=Fixture::load(1);
    for key in [f.curve,f.mint,f.vault,global(),fees()] {
        let mut f=Fixture::load(1);f.rows.get_mut(&key).unwrap().owner=Pubkey::default();assert!(f.quote(false).is_err());
        let mut f=Fixture::load(1);f.rows.get_mut(&key).unwrap().executable=true;assert!(f.quote(false).is_err());
        let mut f=Fixture::load(1);f.rows.get_mut(&key).unwrap().data.truncate(7);assert!(f.quote(false).is_err());
    }
    for at in [48,81,82,123,124] {let mut f=Fixture::load(1);f.rows.get_mut(&f.curve).unwrap().data[at]=2;assert!(f.quote(false).is_err());}
    for at in [0,46] {let mut f=Fixture::load(1);f.rows.get_mut(&f.mint).unwrap().data[at..at+4].copy_from_slice(&1u32.to_le_bytes());assert!(f.quote(false).is_err());}
    let mut exotic=Fixture::load(1);exotic.rows.get_mut(&exotic.curve).unwrap().data[83..115].copy_from_slice(global().as_ref());assert!(exotic.quote(false).is_err());
    let mut f=Fixture::load(1);f.rows.get_mut(&f.vault).unwrap().data[108]=2;assert!(f.quote(false).is_err());
}
#[test]
fn creator_override_cashback_and_holder_reward_flags_preserve_curve_fees() {
    for flag in [82,124] {
        let mut f=Fixture::load(1);let d=&mut f.rows.get_mut(&f.curve).unwrap().data;d[flag]=1;d[115..123].copy_from_slice(&42u64.to_le_bytes());
        assert_eq!(f.quote(false).unwrap().fees,[0,95,42]);
        f.rows.get_mut(&global()).unwrap().data[1045]=0;assert_eq!(f.quote(false).unwrap().fees,[0,95,30]);
        f.rows.get_mut(&f.curve).unwrap().data[49..81].fill(0);assert_eq!(f.quote(false).unwrap().fees,[0,95,0]);
    }
    let mut f=Fixture::load(1);f.rows.get_mut(&f.curve).unwrap().data[115..123].copy_from_slice(&9999u64.to_le_bytes());assert!(f.quote(false).is_err());
}
