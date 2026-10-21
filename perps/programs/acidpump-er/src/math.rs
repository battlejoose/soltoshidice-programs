use crate::{Feed, MixError};
use anchor_lang::prelude::*;
pub const PX: u128 = 1_000_000_000_000;
pub const HIGH_POOL_THRESHOLD: u64 = 2_000_000 * crate::risk::USD;

#[cfg(test)]
mod pool_size_tests {
    use super::*;
    #[test]
    fn boundary_and_high_pool_allocations() {
        assert_eq!(acid_split_at(9900,HIGH_POOL_THRESHOLD).unwrap(),(9000,700,200));
        assert_eq!(acid_split_at(9900,HIGH_POOL_THRESHOLD+1).unwrap(),(1000,7000,1900));
        assert_eq!(venue_loss_split_at(10000,HIGH_POOL_THRESHOLD).unwrap(),(9800,100,100));
        assert_eq!(venue_loss_split_at(10000,HIGH_POOL_THRESHOLD+1).unwrap(),(9000,100,900));
        assert_eq!(venue_token_split_at(9000,HIGH_POOL_THRESHOLD+1).unwrap(),(1000,7000,1000));
    }
    #[test]
    fn every_atom_is_accounted_for_in_both_regimes() {
        for value in [0,HIGH_POOL_THRESHOLD,HIGH_POOL_THRESHOLD+1,u64::MAX] {
            for loss in [0,1,99,100,101,9999,10000,123456789,u64::MAX] {
                let (tokens,gas,sdice)=venue_loss_split_at(loss,value).unwrap();
                let (pool,stakers,burn)=venue_token_split_at(tokens,value).unwrap();
                assert_eq!(pool+stakers+burn+gas+sdice,loss);
                let (tokens,gas)=loss_split(loss).unwrap();
                let (pool,stakers,burn)=acid_split_at(tokens,value).unwrap();
                assert_eq!(pool+stakers+burn+gas,loss);
            }
        }
    }
}
pub fn loss_split(loss:u64)->Result<(u64,u64)> {
    let gas=loss/100;
    Ok((loss-gas,gas))
}
/// Created venues convert 98% into their token, 1% into SOL gas, and 1% into
/// quote for a later SDICE payout-pool contribution (9% above $2M). Returns
/// (token conversion, gas, buyback); rounding stays with the token conversion.
pub fn venue_loss_split(loss:u64)->Result<(u64,u64,u64)> { venue_loss_split_at(loss,0) }
pub fn venue_loss_split_at(loss:u64,pool_usd:u64)->Result<(u64,u64,u64)> {
    let gas=loss/100;
    let buyback=(loss as u128*if pool_usd>HIGH_POOL_THRESHOLD {9}else{1}/100) as u64;
    Ok((loss-gas-buyback,gas,buyback))
}
/// Split the root's 99% receipt: 90/7/2 normally, 10/70/19 above $2M.
/// Integer dust remains in the pool. Existing accrued rewards are unaffected.
pub fn acid_split(received:u64)->Result<(u64,u64,u64)> { acid_split_at(received,0) }
pub fn acid_split_at(received:u64,pool_usd:u64)->Result<(u64,u64,u64)> {
    let high=pool_usd>HIGH_POOL_THRESHOLD;
    let stakers=(received as u128*if high {70}else{7}/99) as u64;
    let burn=(received as u128*if high {19}else{2}/99) as u64;
    Ok((received-stakers-burn,stakers,burn))
}
/// All realizable post-gas receipts fund claims while any FIFO debt exists.
pub fn queued_acid_split(received:u64,queue_usd:u64,head:u64,tail:u64,small:bool)->Result<(u64,u64,u64)> {
    require!(head<=tail,MixError::QueueOrder);
    if small || queue_usd>0 || head<tail {Ok((received,0,0))} else {acid_split(received)}
}
/// Created-venue receipt: 90/7/1 normally, 10/70/10 above $2M.
/// The separate 1% (9% above $2M) SDICE contribution is withheld before conversion.
pub fn venue_token_split(received:u64)->Result<(u64,u64,u64)> { venue_token_split_at(received,0) }
pub fn venue_token_split_at(received:u64,pool_usd:u64)->Result<(u64,u64,u64)> {
    let high=pool_usd>HIGH_POOL_THRESHOLD;
    let denominator=if high {90}else{98};
    let stakers=(received as u128*if high {70}else{7}/denominator) as u64;
    let burn=(received as u128*if high {10}else{1}/denominator) as u64;
    Ok((received-stakers-burn,stakers,burn))
}
pub fn queue_pending(queue_usd:u64,head:u64,tail:u64)->Result<bool> {
    require!(head<=tail,MixError::QueueOrder);
    Ok(queue_usd>0 || head<tail)
}
pub fn div(a: u128, b: u128, d: u128) -> Result<u64> {
    require!(d > 0, MixError::Arithmetic);
    u64::try_from(a.checked_mul(b).ok_or(MixError::Arithmetic)? / d)
        .map_err(|_| error!(MixError::Arithmetic))
}
pub fn ceil(a: u128, d: u128) -> Result<u64> {
    require!(d > 0, MixError::Arithmetic);
    u64::try_from(a / d + u128::from(a % d != 0)).map_err(|_| error!(MixError::Arithmetic))
}
pub fn bust(entry: u64, lev: u16, long: bool) -> Result<u64> {
    require!((1..=1000).contains(&lev), MixError::Leverage);
    // New positions use the full 1/L adverse move, rounded to price atoms.
    // Existing positions retain their stored bust price; never recompute it.
    let delta = (entry / u64::from(lev)).max(1);
    if long {
        entry.checked_sub(delta)
    } else {
        entry.checked_add(delta)
    }
    .ok_or_else(|| error!(MixError::Arithmetic))
}
pub fn crossed(price: u64, bust: u64, long: bool) -> bool {
    if long {
        price <= bust
    } else {
        price >= bust
    }
}
pub fn pnl(margin: u64, entry: u64, exit: u64, lev: u16, long: bool) -> Result<(bool, u64)> {
    let win = if long { exit > entry } else { exit < entry };
    let diff = entry.abs_diff(exit);
    let n = (margin as u128)
        .checked_mul(lev as u128)
        .ok_or(MixError::Arithmetic)?;
    if !win {
        return Ok((false, div(n, diff as u128, entry as u128)?.min(margin)));
    }
    let deadband = entry / 50_000;
    if diff <= deadband {
        return Ok((true, 0));
    }
    let gross = div(n, (diff - deadband) as u128, entry as u128)?;
    let movement = div(diff as u128, PX, entry as u128)?;
    Ok((
        true,
        div(
            gross as u128,
            movement as u128,
            movement as u128 + PX / 10_000,
        )?,
    ))
}
/// Full-size PumpSwap sale estimate, including each fee's independent ceiling.
/// Mainnet reserves and SOL/USD are relayed for this devnet-only experiment.
pub fn sell_usd(amount: u64, f: &Feed) -> Result<u64> {
    require!(
        f.base_reserve > 0 && f.quote_reserve > 0 && f.quote_usd > 0,
        MixError::Quote
    );
    let gross = div(
        amount as u128,
        f.quote_reserve as u128,
        f.base_reserve as u128 + amount as u128,
    )?;
    require!(gross <= f.real_quote_reserve, MixError::Quote);
    let mut fees = 0u64;
    for bps in f.fee_bps {
        fees = fees
            .checked_add(ceil(gross as u128 * bps as u128, 10_000)?)
            .ok_or(MixError::Arithmetic)?;
    }
    let net = gross.saturating_sub(fees);
    div(
        net as u128,
        f.quote_usd as u128,
        10u128.pow(f.quote_decimals as u32) * 1_000_000,
    )
}
pub fn usd_debit(amount: u64, price: u64, decimals: u8) -> Result<u64> {
    ceil(
        amount as u128 * price as u128,
        10u128.pow(decimals as u32) * 1_000_000,
    )
}
pub fn dust(left: u64, original: u64) -> bool {
    left > 0 && left <= 1 && left as u128 * 10_000 <= original as u128
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn loss_allocation_conserves_every_atom() {
        assert_eq!(loss_split(10_000).unwrap(), (9_900, 100));
        assert_eq!(acid_split(9_900).unwrap(), (9_000, 700, 200));
        for loss in [0,1,9,10,11,99,100,101,999,10_000_000,u64::MAX] {
            let (acid,gas)=loss_split(loss).unwrap();
            assert_eq!(acid as u128+gas as u128,loss as u128);
            assert_eq!(gas,loss/100);
            let (market,stakers,burn)=acid_split(acid).unwrap();
            assert_eq!(market as u128+stakers as u128+burn as u128,acid as u128);
            assert_eq!(stakers as u128,acid as u128*7/99);
            assert_eq!(burn as u128,acid as u128*2/99);
            assert!(market as u128*99>=acid as u128*90);
            for (usd,head,tail,small) in [(1,1,2,false),(0,1,2,false),(1,2,2,false),(0,2,2,true)] {
                assert_eq!(queued_acid_split(acid,usd,head,tail,small).unwrap(),(acid,0,0));
            }
            assert_eq!(queued_acid_split(acid,0,2,2,false).unwrap(),(market,stakers,burn));
        }
        assert!(queued_acid_split(99,0,2,1,false).is_err());
    }
    #[test]
    fn venue_allocation_is_90_7_1_burn_1_gas_1_buyback() {
        let (token,gas,buyback)=venue_loss_split(10_000).unwrap();
        assert_eq!((token,gas,buyback),(9_800,100,100));
        assert_eq!(venue_token_split(token).unwrap(),(9_000,700,100));
        for loss in [0,1,99,100,101,199,10_000_000,u64::MAX] {
            let (token,gas,buyback)=venue_loss_split(loss).unwrap();
            assert_eq!(token as u128+gas as u128+buyback as u128,loss as u128);
            assert_eq!((gas,buyback),(loss/100,loss/100));
            let (pool,stakers,burn)=venue_token_split(token).unwrap();
            assert_eq!(pool as u128+stakers as u128+burn as u128,token as u128);
        }
    }
    #[test]
    fn full_leverage_distance_without_buffer() {
        for lev in [1, 2, 5, 10, 100, 500, 1000] {
            let entry = 100_000_000;
            let delta = entry / u64::from(lev);
            assert_eq!(bust(entry, lev, true).unwrap(), entry - delta);
            assert_eq!(bust(entry, lev, false).unwrap(), entry + delta);
        }
        assert_eq!(bust(100_000_000, 1000, true).unwrap(), 99_900_000);
        assert_eq!(bust(100_000_000, 1000, false).unwrap(), 100_100_000);
        assert_eq!(bust(1, 1000, true).unwrap(), 0);
        assert_eq!(bust(1, 1000, false).unwrap(), 2);
        assert!(bust(u64::MAX, 1, false).is_err());
        assert!(bust(100, 0, true).is_err());
        assert!(bust(100, 1001, true).is_err());
        let p = pnl(100_000_000, 100_000_000, 101_000_000, 10, true)
            .unwrap()
            .1;
        assert_eq!(p, 9_881_188);
    }
    #[test]
    fn full_sale_and_fees() {
        let f = Feed {
            base_reserve: 1_000_000_000,
            quote_reserve: 10_000_000_000,
            real_quote_reserve: 10_000_000_000,
            quote_usd: 100 * PX as u64,
            quote_decimals: 9,
            fee_bps: [20, 5, 5],
            ..Feed::default()
        };
        assert_eq!(sell_usd(1_000_000_000, &f).unwrap(), 498_500_000);
        assert!(sell_usd(500_000_000, &f).unwrap() * 2 > sell_usd(1_000_000_000, &f).unwrap());
        assert_eq!(usd_debit(1, 1, 9).unwrap(), 1);
        assert!(!dust(2, 1_000_000));
        assert!(!dust(1, 100));
        assert!(dust(1, 10_000));
    }
}
