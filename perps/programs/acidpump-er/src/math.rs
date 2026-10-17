use crate::{Feed, MixError};
use anchor_lang::prelude::*;
pub const PX: u128 = 1_000_000_000_000;
pub fn loss_split(loss:u64)->Result<(u64,u64)> {
    let gas=loss/50;
    Ok((loss-gas,gas))
}
/// Split the single 98% ACIDPUMP receipt, retaining all rounding in the pool.
pub fn acid_split(received:u64)->Result<(u64,u64,u64)> {
    let stakers=(received as u128*7/98) as u64;
    let burn=received/98;
    Ok((received-stakers-burn,stakers,burn))
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
    // Preserve the original engine's 5 bps price buffer: 1/L - 0.0005.
    let delta = div(
        entry as u128,
        10_000 - 5 * lev as u128,
        10_000 * lev as u128,
    )?
    .max(1);
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
        assert_eq!(loss_split(10_000).unwrap(), (9_800, 200));
        assert_eq!(acid_split(9_800).unwrap(), (9_000, 700, 100));
        for loss in [0,1,9,10,11,99,100,101,999,10_000_000,u64::MAX] {
            let (acid,gas)=loss_split(loss).unwrap();
            assert_eq!(acid as u128+gas as u128,loss as u128);
            assert_eq!(gas,loss/50);
            let (market,stakers,burn)=acid_split(acid).unwrap();
            assert_eq!(market as u128+stakers as u128+burn as u128,acid as u128);
            assert_eq!(stakers as u128,acid as u128*7/98);
            assert_eq!(burn as u128,acid as u128/98);
            assert!(market as u128*98>=acid as u128*90);
        }
    }
    #[test]
    fn leverage_and_buffer() {
        assert_eq!(bust(100_000_000, 1000, true).unwrap(), 99_950_000);
        assert_eq!(bust(100_000_000, 100, true).unwrap(), 99_050_000);
        assert_eq!(bust(100_000_000, 1, true).unwrap(), 50_000);
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
