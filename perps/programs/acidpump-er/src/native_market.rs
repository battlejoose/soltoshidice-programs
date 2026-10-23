//! Canonical native-market identities and loss allocation. A token's symbol
//! never selects these rules. SOL/USDC fund SDICE stakers and SDICE burns.
use anchor_lang::prelude::*;
use crate::{Asset, MixError, math};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Sol, Usdc }

pub fn is_native_mint(mint: Pubkey) -> bool {
    mint == crate::mainnet_pump::SOL || mint == crate::mainnet_pump::USDC
}

/// Allocate actual SOL proceeds at settlement, before a later price movement
/// can change the tier. Rounding belongs to stakers; native assets are not burned.
pub fn sdice_quote_split(received: u64, pool_usd: u64) -> (u64, u64) {
    let burn = if pool_usd > math::HIGH_POOL_THRESHOLD {
        (received as u128 * 9 / 19) as u64
    } else { 0 };
    (received - burn, burn)
}

pub fn kind(asset: &Asset) -> Result<Option<Kind>> {
    if asset.mint == crate::mainnet_pump::SOL || asset.id == 103 {
        require!(asset.id == 103 && asset.mint == crate::mainnet_pump::SOL && asset.decimals == 9
            && asset.token_program == anchor_spl::token::ID && asset.quote_mint == crate::mainnet_pump::SOL
            && asset.dex_pool == Pubkey::default(), MixError::Binding);
        return Ok(Some(Kind::Sol));
    }
    if asset.mint == crate::mainnet_pump::USDC || asset.id == 104 {
        require!(asset.id == 104 && asset.mint == crate::mainnet_pump::USDC && asset.decimals == 6
            && asset.token_program == anchor_spl::token::ID && asset.quote_mint == crate::mainnet_pump::SOL
            && asset.dex_pool == pubkey!("Gf7sXMoP8iRw4iiXmJ1nq4vxcRycbGXy5RL8a8LnTd3v"), MixError::Binding);
        return Ok(Some(Kind::Usdc));
    }
    Ok(None)
}

/// Returns (primary-asset input, gas input, SDICE contribution input).
/// The FIFO branch is selected by the same validated queue as all other venues.
pub fn loss_split(loss: u64, pool_usd: u64, queued: bool) -> Result<(u64,u64,u64)> {
    let gas=loss/100;
    let sdice=if queued {0} else {(loss as u128*if pool_usd>math::HIGH_POOL_THRESHOLD {19}else{2}/100) as u64};
    Ok((loss-gas-sdice,gas,sdice))
}

/// Split the actual converted primary receipts; fees/slippage are not invented
/// token receipts. Rounding stays in the payout pool. Nothing is burned.
pub fn receipt_split(received:u64,pool_usd:u64,queued:bool)->Result<(u64,u64,u64)> {
    if queued {return Ok((received,0,0));}
    let high=pool_usd>math::HIGH_POOL_THRESHOLD;
    let stakers=(received as u128*if high {70}else{7}/if high {80}else{97}) as u64;
    Ok((received-stakers,stakers,0))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn asset(kind:Kind)->Asset {
        let sol=kind==Kind::Sol;
        Asset{id:if sol {103}else{104}, feed:Pubkey::default(),
            mint:if sol {crate::mainnet_pump::SOL}else{crate::mainnet_pump::USDC},
            token_program:anchor_spl::token::ID,source_mint:Pubkey::default(),source_pool:Pubkey::default(),
            quote_mint:crate::mainnet_pump::SOL,
            dex_pool:if sol {Pubkey::default()}else{pubkey!("Gf7sXMoP8iRw4iiXmJ1nq4vxcRycbGXy5RL8a8LnTd3v")},
            collateral_enabled:true,decimals:if sol {9}else{6},bump:0}
    }
    #[test] fn native_rules_bind_all_identity_fields() {
        for expected in [Kind::Sol,Kind::Usdc] {
            assert_eq!(kind(&asset(expected)).unwrap(),Some(expected));
            for field in 0..6 {
                let mut a=asset(expected);
                match field {0=>a.id=500,1=>a.mint=Pubkey::new_unique(),2=>a.decimals+=1,
                    3=>a.token_program=Pubkey::new_unique(),4=>a.quote_mint=Pubkey::new_unique(),_=>a.dex_pool=Pubkey::new_unique()}
                assert!(kind(&a).is_err());
            }
        }
    }
    #[test] fn native_allocations_match_confirmed_economics() {
        for (pool,expected) in [(math::HIGH_POOL_THRESHOLD,(9000,700,200,100)),
            (math::HIGH_POOL_THRESHOLD+1,(1000,7000,1900,100))] {
            let (input,gas,sdice)=loss_split(10000,pool,false).unwrap();
            let (treasury,stakers,burn)=receipt_split(input,pool,false).unwrap();
            assert_eq!((treasury,stakers,sdice,gas),expected);assert_eq!(burn,0);
            assert_eq!(loss_split(10000,pool,true).unwrap(),(9900,100,0));
            assert_eq!(receipt_split(9900,pool,true).unwrap(),(9900,0,0));
        }
    }
    #[test] fn every_atom_is_accounted_for_without_burning_sol_or_usdc() {
        for pool in [0,math::HIGH_POOL_THRESHOLD,math::HIGH_POOL_THRESHOLD+1,u64::MAX] {
            for loss in [0,1,99,100,101,9999,10000,123456789,u64::MAX] {
                for queued in [false,true] {
                    let (input,gas,sdice)=loss_split(loss,pool,queued).unwrap();
                    let (treasury,stakers,burn)=receipt_split(input,pool,queued).unwrap();
                    assert_eq!(treasury as u128+stakers as u128+gas as u128+sdice as u128,loss as u128);
                    assert_eq!(burn,0);
                }
            }
        }
    }
    #[test] fn sdice_stakers_receive_two_percent_or_ten_of_nineteen() {
        for received in [0,1,18,19,20,1900,u64::MAX] {
            assert_eq!(sdice_quote_split(received,math::HIGH_POOL_THRESHOLD),(received,0));
            let (rewards,burn)=sdice_quote_split(received,math::HIGH_POOL_THRESHOLD+1);
            assert_eq!(rewards as u128+burn as u128,received as u128);
            assert_eq!(burn,(received as u128*9/19) as u64);
        }
        assert_eq!(sdice_quote_split(1900,math::HIGH_POOL_THRESHOLD+1),(1000,900));
    }
}
