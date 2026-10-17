#![cfg(feature="mainnet")]
//! Real contract valuation entry points with captured/synthetic public accounts.
//! This does not admit USDC routes or substitute for full settlement execution.
use acidpump_er::{self as p,mainnet_pump::{SOL,USDC},mainnet_curve};
use anchor_lang::prelude::{AccountInfo,Pubkey,UncheckedAccount};
use base64::{engine::general_purpose::STANDARD,Engine};
use serde_json::{json,Value};
use std::{collections::BTreeMap,str::FromStr};
fn pk(s:&str)->Pubkey{Pubkey::from_str(s).unwrap()}
fn key(v:&Value,k:&str)->Pubkey{pk(v[k].as_str().unwrap())}
struct Row{key:Pubkey,owner:Pubkey,data:Vec<u8>,lamports:u64,executable:bool}
#[test]
fn base_collateral_and_payout_valuation_preserve_usdc_denomination(){
    let source:Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/mainnet-execution.json")).unwrap();
    let pools:Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/pump-token-pools.json")).unwrap();
    let curves:Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/pump-token-curve-quotes.json")).unwrap();
    let usd_pool=source["swaps"].as_array().unwrap().iter().find(|r|key(r,"mint")==USDC).unwrap();
    let usd_data=STANDARD.decode(source["accounts"][key(usd_pool,"pool").to_string()]["data"].as_str().unwrap()).unwrap();
    let usd_base=Pubkey::new_from_array(usd_data[139..171].try_into().unwrap());let usd_quote=Pubkey::new_from_array(usd_data[171..203].try_into().unwrap());
    let global=Pubkey::find_program_address(&[b"global_config"],&p::pump::ID).0;
    let fees=Pubkey::find_program_address(&[b"fee_config",p::pump::ID.as_ref()],&p::pump::FEES_ID).0;
    let mut count=0;let mut rejected=0;
    for(curve,cases)in [(true,&curves["rows"]),(false,&pools["cases"])]{for case in cases.as_array().unwrap().iter().filter(|r|key(r,"quoteMint")==USDC){
        let mint=key(case,"mint");let pool=key(case,if curve{"curve"}else{"pool"});
        let (base,quote,source_global,source_fees,amount,expected)=if curve{
            let sample=&case["samples"][2];
            (key(case,"baseVault"),key(case,"quoteVault"),Pubkey::find_program_address(&[b"global"],&mainnet_curve::ID).0,
                Pubkey::find_program_address(&[b"fee_config",mainnet_curve::ID.as_ref()],&mainnet_curve::FEES_ID).0,
                sample["amount"].as_str().unwrap().parse::<u64>().unwrap(),sample["netQuoteAtoms"].as_str().unwrap().parse::<u64>().unwrap())
        }else{
            let data=STANDARD.decode(case["accounts"][pool.to_string()]["data"].as_str().unwrap()).unwrap();let sample=&case["swaps"][0];
            (Pubkey::new_from_array(data[139..171].try_into().unwrap()),Pubkey::new_from_array(data[171..203].try_into().unwrap()),global,fees,
                sample["amount"].as_str().unwrap().parse().unwrap(),sample["expected"].as_str().unwrap().parse().unwrap())
        };
        let mut accounts=source["accounts"].as_object().unwrap().clone();accounts.extend(case["accounts"].as_object().unwrap().clone());
        let mut rows:BTreeMap<_,_>=accounts.iter().map(|(k,a)|{let key=pk(k);(key,Row{key,owner:key_of(a),data:STANDARD.decode(a["data"].as_str().unwrap()).unwrap(),lamports:a["lamports"].as_u64().unwrap(),executable:false})}).collect();
        let token_program=rows[&mint].owner;let decimals=rows[&mint].data[44];
        let infos:BTreeMap<_,_>=rows.values_mut().map(|r|(r.key,AccountInfo::new(&r.key,false,false,&mut r.lamports,&mut r.data,&r.owner,r.executable))).collect();
        let get=|k:Pubkey|UncheckedAccount::try_from(&infos[&k]);
        let v=p::Valuation{dex_pool:get(pool),dex_base:get(base),dex_quote:get(quote),fee_config:get(fees),dex_mint:get(mint),global_config:get(global),
            usd_pool:get(p::pump::USDC_POOL),usd_mint:get(USDC),usd_base:get(usd_base),usd_quote:get(usd_quote),source_global:get(source_global),source_fees:get(source_fees)};
        let a=p::Asset{id:105,feed:Pubkey::default(),mint,token_program,source_mint:mint,source_pool:pool,quote_mint:USDC,dex_pool:pool,collateral_enabled:true,decimals,bump:0};
        if curve {
            assert!(p::pump::registration_quote_mint(mint,&v).is_err());
            assert!(p::pump::validate_pool(&a,&v).is_err());
            assert!(p::pump::collateral_value(amount,&a,&v,110_000_000_000_000).is_err());
            rejected+=1;continue;
        }
        assert_eq!(p::pump::registration_quote_mint(mint,&v).unwrap(),USDC);
        p::pump::validate_pool(&a,&v).unwrap();
        for sol_price in [1,110_000_000_000_000,990_000_000_000_000]{assert_eq!(p::pump::collateral_value(amount,&a,&v,sol_price).unwrap(),expected);}
        assert_eq!(p::pump::sell_value(amount,&a,&v).unwrap(),expected);
        assert_eq!(p::pump::collateral_value(0,&a,&v,110_000_000_000_000).unwrap(),0);
        count+=1;
    }}
    assert_eq!(count,14);assert_eq!(rejected,6);
    println!("{}",json!({"usdcValuationCases":count,"noSolRepricing":true,"wireLayoutChanged":false}));
}
fn key_of(v:&Value)->Pubkey{key(v,"owner")}
