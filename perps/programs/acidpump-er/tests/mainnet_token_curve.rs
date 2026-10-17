//! Token-denominated curve valuation. Offline variants, not live execution evidence.
use acidpump_er::{mainnet_curve::{self,SolCurveAccounts,TokenCurveAccounts},mainnet_pump::{PoolQuote,USDC}};
use anchor_lang::prelude::{AccountInfo,Pubkey};
use base64::{engine::general_purpose::STANDARD,Engine};
use serde_json::Value;
use std::{collections::BTreeMap,str::FromStr};
fn pk(s:&str)->Pubkey{Pubkey::from_str(s).unwrap()}
fn source()->Value{serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/pump-token-curve-quotes.json")).unwrap()}
struct Row{key:Pubkey,owner:Pubkey,data:Vec<u8>,lamports:u64,executable:bool}
struct Fixture{source:Value,rows:BTreeMap<Pubkey,Row>}
impl Fixture{
    fn new(source:&Value)->Self{Self{source:source.clone(),rows:source["accounts"].as_object().unwrap().iter().map(|(key,a)|{
        let key=pk(key);(key,Row{key,owner:pk(a["owner"].as_str().unwrap()),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),lamports:a["lamports"].as_u64().unwrap(),executable:a["executable"].as_bool().unwrap()})
    }).collect()}}
    fn key(&self,name:&str)->Pubkey{pk(self.source[name].as_str().unwrap())}
    fn quote(&mut self,empty:bool)->anchor_lang::Result<PoolQuote>{
        let (curve,mint,base,quote,vault)=(self.key("curve"),self.key("mint"),self.key("baseVault"),self.key("quoteMint"),self.key("quoteVault"));
        let global=Pubkey::find_program_address(&[b"global"],&mainnet_curve::ID).0;
        let fees=Pubkey::find_program_address(&[b"fee_config",mainnet_curve::ID.as_ref()],&mainnet_curve::FEES_ID).0;
        let infos:BTreeMap<_,_>=self.rows.values_mut().map(|r|(r.key,AccountInfo::new(&r.key,false,false,&mut r.lamports,&mut r.data,&r.owner,r.executable))).collect();
        TokenCurveAccounts{curve:SolCurveAccounts{curve:&infos[&curve],mint:&infos[&mint],base_vault:&infos[&base],global:&infos[&global],fees:&infos[&fees]},
            quote_mint:&infos[&quote],quote_vault:&infos[&vault]}.validate(curve,mint,quote,empty)
    }
}
#[test]
fn usdc_and_token2022_quote_atoms_match_official_sdk_for_both_base_programs_and_modes(){
    let source=source();let mut count=0;
    for row in source["rows"].as_array().unwrap(){
        let mut f=Fixture::new(row);let q=f.quote(false).unwrap();
        assert_eq!(q.fees,if f.key("quoteMint")==USDC{[0,15,25]}else{[0,20,40]});
        for sample in row["samples"].as_array().unwrap(){
            let amount=sample["amount"].as_str().unwrap().parse().unwrap();
            if sample["executable"].as_bool().unwrap(){
                let net=sample["netQuoteAtoms"].as_str().unwrap().parse::<u64>().unwrap();
                assert_eq!(q.sell(amount).unwrap(),net);assert_eq!(q.collateral_lamports(amount).unwrap(),net);
            }else{assert!(q.sell(amount).is_err());assert_eq!(q.collateral_lamports(amount).unwrap(),0);}
            count+=1;
        }
    }
    assert_eq!(count,60);
}
#[test]
fn captured_mainnet_usdc_curves_match_executable_sdk_quotes_and_dust_liquidity_bounds(){
    let captured:Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/mainnet-pump-usdc-curves.json")).unwrap();
    let mut count=0;let mut executable=0;
    for row in captured["rows"].as_array().unwrap(){
        let mut input=row.clone();input["accounts"]=captured["accounts"].clone();
        let mut f=Fixture::new(&input);let q=f.quote(false).unwrap();
        let expected:[u16;3]=std::array::from_fn(|i|row["feeBps"][i].as_u64().unwrap() as u16);
        assert_eq!(q.fees,expected);assert_eq!(q.real_quote,row["realQuoteAtoms"].as_str().unwrap().parse::<u64>().unwrap());
        for sample in row["samples"].as_array().unwrap(){
            let amount=sample["amount"].as_str().unwrap().parse().unwrap();
            if sample["executable"].as_bool().unwrap(){
                assert_eq!(q.sell(amount).unwrap(),sample["netQuoteAtoms"].as_str().unwrap().parse::<u64>().unwrap());executable+=1;
            }else{assert!(q.sell(amount).is_err());assert_eq!(q.collateral_lamports(amount).unwrap(),0);}
            count+=1;
        }
    }
    assert_eq!(count,15);assert_eq!(executable,6);
}
#[test]
fn only_real_quote_tokens_fund_collateral_not_lamports_donations_or_retained_fees(){
    for row in source()["rows"].as_array().unwrap(){
        let mut f=Fixture::new(row);let q=f.quote(false).unwrap();let curve=f.key("curve");let vault=f.key("quoteVault");
        f.rows.get_mut(&curve).unwrap().lamports=0;assert_eq!(f.quote(false).unwrap(),q);
        f.rows.get_mut(&curve).unwrap().lamports=u64::MAX;
        f.rows.get_mut(&vault).unwrap().data[64..72].copy_from_slice(&u64::MAX.to_le_bytes());assert_eq!(f.quote(false).unwrap(),q);
        f.rows.get_mut(&vault).unwrap().data[64..72].copy_from_slice(&(q.real_quote+24_999).to_le_bytes());assert!(f.quote(false).is_err());
        f.rows.get_mut(&curve).unwrap().data[32..40].fill(0);
        assert!(f.quote(false).is_err());assert_eq!(f.quote(true).unwrap().collateral_lamports(u64::MAX).unwrap(),0);
        f.rows.get_mut(&vault).unwrap().data[64..72].fill(0);assert!(f.quote(true).is_err(),"retained fees must still be covered");
    }
}
#[test]
fn quote_mint_and_vault_are_authenticated_even_for_zero_collateral(){
    for row in source()["rows"].as_array().unwrap(){
        for field in ["quoteMint","quoteVault"]{
            for change in 0..3{
                let mut f=Fixture::new(row);let key=f.key(field);let a=f.rows.get_mut(&key).unwrap();
                match change{0=>a.owner=Pubkey::default(),1=>a.executable=true,_=>a.data.truncate(7)};
                assert!(f.quote(true).is_err());
            }
        }
        for offset in [0,32]{let mut f=Fixture::new(row);let vault=f.key("quoteVault");f.rows.get_mut(&vault).unwrap().data[offset..offset+32].fill(0);assert!(f.quote(true).is_err());}
        let mut f=Fixture::new(row);let vault=f.key("quoteVault");f.rows.get_mut(&vault).unwrap().data[108]=2;assert!(f.quote(true).is_err());
        let mut f=Fixture::new(row);let curve=f.key("curve");f.rows.get_mut(&curve).unwrap().data[83..115].fill(0);assert!(f.quote(true).is_err());
        if f.key("quoteMint")==USDC{let mut f=Fixture::new(row);f.rows.get_mut(&USDC).unwrap().data[44]=9;assert!(f.quote(true).is_err());}
    }
}
