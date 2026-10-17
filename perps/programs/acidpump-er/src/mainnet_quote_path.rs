//! Shared amount-dependent Pump sale composition. Venue and mint bindings are
//! validated by the caller; every hop receives the previous hop's net atoms.
use anchor_lang::prelude::*;
use crate::{mainnet_pump::PoolQuote, math, MixError};

pub const MAX_QUOTE_LEGS: usize = 3;

pub fn collateral_atoms(amount: u64, legs: &[PoolQuote]) -> Result<u64> {
    require!(legs.len() <= MAX_QUOTE_LEGS, MixError::Binding);
    // Validate every leg before zero can short-circuit sale arithmetic.
    for leg in legs {
        require!(leg.fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
        if leg.base > 0 && leg.real_quote > 0 { leg.effective_quote()?; }
    }
    legs.iter().try_fold(amount, |output, leg| leg.collateral_quote_atoms(output))
}

pub fn collateral_usd(amount: u64, legs: &[PoolQuote], usd_per_final_atom_e21: u64) -> Result<u64> {
    require!(usd_per_final_atom_e21 > 0 && usd_per_final_atom_e21 <= 1_000_000_000_000_000_000, MixError::Quote);
    math::div(collateral_atoms(amount, legs)? as u128, usd_per_final_atom_e21 as u128, 1_000_000_000_000_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pool(base:u64, quote:u64, fees:[u16;3])->PoolQuote {
        PoolQuote{base,real_quote:quote,virtual_quote:0,fees}
    }
    #[test]
    fn each_hop_uses_net_atoms_and_full_amount_price_impact() {
        let first=pool(1_000_000_000,50_000_000,[0,20,80]);
        let second=pool(100_000_000,5_000_000_000,[20,5,95]);
        let third=pool(30_000_000_000,100_000_000,[20,5,95]);
        for amount in [0,1,100,10_000,500_000_000,1_000_000_000_000] {
            let expected=third.collateral_quote_atoms(second.collateral_quote_atoms(first.collateral_quote_atoms(amount).unwrap()).unwrap()).unwrap();
            assert_eq!(collateral_atoms(amount,&[first,second,third]).unwrap(),expected);
            assert_eq!(collateral_usd(amount,&[first,second,third],1_000_000_000_000_000).unwrap(),expected);
        }
        let bag=500_000_000;
        let exact=collateral_atoms(bag,&[first,second]).unwrap();
        let spot_conversion=first.collateral_quote_atoms(bag).unwrap() as u128 * second.real_quote as u128 / second.base as u128;
        assert!(spot_conversion>exact as u128);
    }
    #[test]
    fn empty_or_insufficient_intermediate_liquidity_is_zero_but_invalid_data_fails() {
        let first=pool(1_000_000,10_000_000,[0,5,95]);
        let empty=pool(1_000,0,[0,0,0]);
        let shallow=PoolQuote{base:100,real_quote:10,virtual_quote:1_000_000,fees:[20,5,95]};
        assert_eq!(collateral_atoms(100_000,&[first,empty]).unwrap(),0);
        assert_eq!(collateral_atoms(100_000,&[first,shallow]).unwrap(),0);
        assert!(collateral_atoms(0,&[empty,pool(1,1,[10_000,0,0])]).is_err());
        assert!(collateral_atoms(0,&[first;4]).is_err());
        assert!(collateral_usd(0,&[],0).is_err());
        assert_eq!(collateral_usd(123_456,&[],1_000_000_000_000_000).unwrap(),123_456);
        assert_eq!(collateral_usd(1_000_000_000,&[],110_000_000_000_000).unwrap(),110_000_000);
    }
    #[test]
    fn composed_paths_match_independent_sdk_fixture_at_every_hop() {
        let fixture:serde_json::Value=serde_json::from_str(include_str!("../../../projects/acidpump-er/src/fixtures/pump-quote-paths.json")).unwrap();
        let rows=fixture["rows"].as_array().unwrap();
        assert_eq!(rows.len(),72);
        for row in rows {
            let number=|v:&serde_json::Value|v.as_str().unwrap().parse::<u64>().unwrap();
            let amount=number(&row["amount"]);
            let legs:Vec<PoolQuote>=row["legs"].as_array().unwrap().iter().map(|v| {
                let real=number(&v["realQuoteReserve"]);
                PoolQuote{base:number(&v["baseReserve"]),real_quote:real,
                    virtual_quote:number(&v["effectiveQuoteReserve"]) as i128-real as i128,
                    fees:std::array::from_fn(|i|v["feeBps"][i].as_u64().unwrap() as u16)}
            }).collect();
            for end in 1..=legs.len() {
                assert_eq!(collateral_atoms(amount,&legs[..end]).unwrap(),number(&row["hopOutputs"][end-1]),"{}",row["name"]);
            }
            assert_eq!(collateral_usd(amount,&legs,1_000_000_000_000_000).unwrap(),number(&row["expected"]));
        }
    }
}
