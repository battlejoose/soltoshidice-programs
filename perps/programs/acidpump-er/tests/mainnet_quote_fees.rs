//! Fee decoding parity against two official SDKs, including old padded layouts.
use acidpump_er::mainnet_pump::{quote_fees, SOL, USDC};
use anchor_lang::prelude::Pubkey;
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use std::str::FromStr;
fn fixture()->Value {serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/pump-quote-fees.json")).unwrap()}
#[test]
fn all_quote_schedules_match_both_official_sdks_across_layout_versions(){
    let source=fixture();let mut checked=0;
    for f in source["fixture"].as_array().unwrap(){
        let data=STANDARD.decode(f["data"].as_str().unwrap()).unwrap();
        for row in f["rows"].as_array().unwrap(){
            let actual=quote_fees(&data,row["canonical"].as_bool().unwrap(),Pubkey::from_str(row["quoteMint"].as_str().unwrap()).unwrap(),row["marketCap"].as_str().unwrap().parse().unwrap()).unwrap();
            let expected:[u16;3]=std::array::from_fn(|i|row["expected"][i].as_u64().unwrap() as u16);
            assert_eq!(actual,expected,"{} {row}",f["name"]);checked+=1;
        }
    }
    assert_eq!(checked,1152);
}
#[test]
fn corrupted_vectors_discriminators_and_fee_values_never_become_a_quote(){
    let source=fixture();let original=STANDARD.decode(source["fixture"][4]["data"].as_str().unwrap()).unwrap();
    for quote in [SOL,USDC,Pubkey::new_unique()]{
        for canonical in [false,true]{
            assert!(quote_fees(&original[..2511],canonical,quote,0).is_err());
            let mut bad=original.clone();bad[0]^=1;assert!(quote_fees(&bad,canonical,quote,0).is_err());
            for at in [65,189]{
                let mut bad=original.clone();bad[at..at+4].copy_from_slice(&51u32.to_le_bytes());
                assert!(quote_fees(&bad,canonical,quote,0).is_err());
                bad[at..at+4].copy_from_slice(&u32::MAX.to_le_bytes());assert!(quote_fees(&bad,canonical,quote,0).is_err());
            }
        }
    }
    for (quote,at) in [(SOL,85),(USDC,209),(Pubkey::new_unique(),273)]{
        let mut bad=original.clone();bad[at..at+8].copy_from_slice(&10_000u64.to_le_bytes());
        assert!(quote_fees(&bad,true,quote,0).is_err());
        bad[at..at+8].copy_from_slice(&9_999u64.to_le_bytes());assert!(quote_fees(&bad,true,quote,0).is_err());
    }
    let mut duplicate=original.clone();duplicate[109..125].copy_from_slice(&100u128.to_le_bytes());
    assert!(quote_fees(&duplicate,true,SOL,0).is_err());
    let mut descending=original;descending[233..249].copy_from_slice(&1u128.to_le_bytes());
    assert!(quote_fees(&descending,true,USDC,0).is_err());
}
