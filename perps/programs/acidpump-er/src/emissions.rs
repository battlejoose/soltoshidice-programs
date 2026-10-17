//! Token-based awards, with the original USD pool thresholds and ratcheted tail.
use crate::*;
pub const FLAT_THRESHOLD: u64 = 2_000_000 * risk::USD;
pub const POOL_CAP: u64 = 5_000_000 * risk::USD;
pub const TAIL_SCALE: u128 = 120_000_000u128 * risk::USD as u128;
pub const FP: u128 = 1_000_000_000_000;

fn tail_factor(h0: u128, h1: u128) -> u128 {
    (TAIL_SCALE * FP / (TAIL_SCALE + h0)) * TAIL_SCALE / (TAIL_SCALE + h1)
}
pub fn issue(tokens: u64, basis_usd: u64, tracked: i128, advance: u64, hwm: u64) -> Result<(u128,u64)> {
    if tokens == 0 { return Ok((0,hwm)); }
    let basis = basis_usd as u128;
    let threshold = FLAT_THRESHOLD as i128;
    if basis == 0 { return Ok(((tokens as u128 * 100) * if tracked < threshold { FP } else { tail_factor(hwm as u128,hwm as u128) } / FP,hwm)); }
    let mut pos=tracked;
    let mut rem=advance.min(basis_usd) as u128;
    let mut h=hwm as u128;
    let mut weighted=0u128;
    if pos < threshold && rem>0 {
        let take=rem.min((threshold-pos) as u128);
        weighted += take * FP; pos += take as i128; rem -= take;
    }
    if rem>0 {
        let mut progress=(pos-threshold) as u128;
        let below=rem.min(h.saturating_sub(progress));
        weighted += below * tail_factor(h,h); progress+=below; rem-=below;
        if rem>0 {
            let h0=h.max(progress); h=h0.checked_add(rem).ok_or(MixError::Arithmetic)?;
            weighted += rem * tail_factor(h0,h); progress=h;
        }
        pos=threshold+progress as i128;
    }
    let residual=basis-advance.min(basis_usd) as u128;
    weighted += residual * if pos<threshold {FP} else {tail_factor(h,h)};
    // Divide before multiplying by token quantity to bound intermediates. Fractions mint nothing.
    let average=weighted/basis;
    let issued=(tokens as u128 * 100).checked_mul(average).ok_or(MixError::Arithmetic)?/FP;
    Ok((issued,u64::try_from(h).map_err(|_|MixError::Arithmetic)?))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn flat_and_underwater_awards_use_actual_tokens_not_usd() {
        assert_eq!(issue(1_000_000,50_000_000,0,50_000_000,0).unwrap(),(100_000_000,0));
        assert_eq!(issue(1_000_000,50_000_000,-10_000_000_000,50_000_000,0).unwrap(),(100_000_000,0));
    }
    #[test]
    fn crosses_threshold_and_freezes_tail_on_recovery() {
        let (first,h)=issue(2_000_000,2*risk::USD,FLAT_THRESHOLD as i128-risk::USD as i128,2*risk::USD,0).unwrap();
        assert!(first<200_000_000 && first>199_999_990); assert_eq!(h,risk::USD);
        let (later,h2)=issue(1_000_000,risk::USD,FLAT_THRESHOLD as i128,0,120_000_000*risk::USD).unwrap();
        assert_eq!(later,25_000_000); assert_eq!(h2,120_000_000*risk::USD);
        assert_eq!(issue(0,0,0,0,h2).unwrap(),(0,h2));
    }
    #[test]
    fn token_amounts_and_highwater_do_not_overflow() {
        let (n,h)=issue(u64::MAX,u64::MAX,0,u64::MAX,0).unwrap();
        assert!(n<=u64::MAX as u128*100); assert!(h>0);
    }
}
