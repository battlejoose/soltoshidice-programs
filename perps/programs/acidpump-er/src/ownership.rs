//! Pool-local ownership accounting. Amounts use the settlement mint's decimals.
//! These are the same non-transferable staking balances used by the original engine.
use crate::*;
pub const SCALE: u128 = 1_000_000_000_000_000_000;

#[account]
#[derive(InitSpace, Default)]
pub struct OwnershipPool {
    pub pool: Pubkey,
    pub supply: u128,
    pub total_staked: u128,
    pub acc_reward: u128,
    pub fee_pending: u64,
    pub reward_pool: u64,
    pub pending_liabilities: u64,
    pub dev_fees: u64,
    pub activated_at: i64,
    pub tail_progress_usd: u64,
    pub bump: u8,
}
#[account]
#[derive(InitSpace, Default)]
pub struct OwnershipTrader {
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub available: u128,
    pub staked: u128,
    pub issued: u128,
    pub reward_index: u128,
    pub pending: u64,
    pub claimed: u64,
    pub bump: u8,
}
#[account]
#[derive(InitSpace, Default)]
pub struct OwnershipReceipt {
    pub position: Pubkey,
    pub pool: Pubkey,
    pub owner: Pubkey,
    pub pool_tokens: u64,
    pub issued: u128,
    pub awarded: bool,
    pub bump: u8,
}

pub fn distribute(p: &mut OwnershipPool) -> Result<()> {
    if p.total_staked == 0 || p.fee_pending == 0 { return Ok(()); }
    let inc = (p.fee_pending as u128).checked_mul(SCALE).ok_or(MixError::Arithmetic)? / p.total_staked;
    let product = inc.checked_mul(p.total_staked).ok_or(MixError::Arithmetic)?;
    // Reserve enough for every account's fractional entitlement; never borrow payout inventory.
    let moved = u64::try_from(product / SCALE + u128::from(product % SCALE != 0)).map_err(|_| MixError::Arithmetic)?;
    p.acc_reward = p.acc_reward.checked_add(inc).ok_or(MixError::Arithmetic)?;
    p.fee_pending = p.fee_pending.checked_sub(moved).ok_or(MixError::Arithmetic)?;
    p.reward_pool = p.reward_pool.checked_add(moved).ok_or(MixError::Arithmetic)?;
    Ok(())
}
pub fn accrue(p: &mut OwnershipPool, t: &mut OwnershipTrader) -> Result<()> {
    let delta = p.acc_reward.checked_sub(t.reward_index).ok_or(MixError::Arithmetic)?;
    let earned = u64::try_from(t.staked.checked_mul(delta).ok_or(MixError::Arithmetic)? / SCALE).map_err(|_| MixError::Arithmetic)?;
    t.pending = t.pending.checked_add(earned).ok_or(MixError::Arithmetic)?;
    p.pending_liabilities = p.pending_liabilities.checked_add(earned).ok_or(MixError::Arithmetic)?;
    require!(p.pending_liabilities <= p.reward_pool, MixError::Inventory);
    // Fractions are forfeited on balance changes, preventing stake/unstake rounding farms.
    t.reward_index = p.acc_reward;
    Ok(())
}
pub fn award(p: &mut OwnershipPool, t: &mut OwnershipTrader, r: &mut OwnershipReceipt, tokens: u64) -> Result<()> {
    award_scaled(p,t,r,tokens,0,0,0)
}
pub fn award_scaled(p: &mut OwnershipPool, t: &mut OwnershipTrader, r: &mut OwnershipReceipt, tokens: u64, basis_usd: u64, tracked: i128, advance: u64) -> Result<()> {
    require!(!r.awarded, MixError::Replay);
    let (issued, highwater) = emissions::issue(tokens,basis_usd,tracked,advance,p.tail_progress_usd)?;
    p.tail_progress_usd = highwater;
    t.available = t.available.checked_add(issued).ok_or(MixError::Arithmetic)?;
    t.issued = t.issued.checked_add(issued).ok_or(MixError::Arithmetic)?;
    p.supply = p.supply.checked_add(issued).ok_or(MixError::Arithmetic)?;
    r.pool_tokens = tokens;
    r.issued = issued;
    r.awarded = true;
    emit!(OwnershipAwarded { position: r.position, pool: r.pool, owner: r.owner, pool_tokens: tokens, issued });
    Ok(())
}
pub fn fund_rewards(p: &mut OwnershipPool, receipts: u64) -> Result<()> {
    p.fee_pending = p.fee_pending.checked_add(receipts).ok_or(MixError::Arithmetic)?;
    // If nobody is staked yet the tokens stay reserved for the next distribution.
    distribute(p)
}
pub fn change_stake(p: &mut OwnershipPool, t: &mut OwnershipTrader, inv: &mut Inventory, amount: u128, unstake: bool) -> Result<()> {
    require!(amount > 0, MixError::Amount);
    distribute(p)?;
    accrue(p, t)?;
    if unstake {
        t.staked = t.staked.checked_sub(amount).ok_or(MixError::Amount)?;
        t.available = t.available.checked_add(amount).ok_or(MixError::Arithmetic)?;
        p.total_staked = p.total_staked.checked_sub(amount).ok_or(MixError::Arithmetic)?;
    } else {
        t.available = t.available.checked_sub(amount).ok_or(MixError::Amount)?;
        t.staked = t.staked.checked_add(amount).ok_or(MixError::Arithmetic)?;
        p.total_staked = p.total_staked.checked_add(amount).ok_or(MixError::Arithmetic)?;
    }
    if p.total_staked == 0 {
        // Everyone has accrued by their final unstake. Return unclaimable fractions to payouts.
        let dust = p.reward_pool.checked_sub(p.pending_liabilities).ok_or(MixError::Arithmetic)?;
        p.reward_pool -= dust;
        inv.treasury = inv.treasury.checked_add(dust).ok_or(MixError::Arithmetic)?;
    }
    distribute(p)?;
    Ok(())
}
pub fn sweep_surplus(p: &mut OwnershipPool, inv: &mut Inventory, queue_usd:u64, settlement:&Asset, valuation:&Valuation) -> Result<u64> {
    if queue_usd != 0 || p.total_staked == 0 { return Ok(0); }
    let curve=pump::value_curve(settlement,valuation)?;
    if curve.value(inv.treasury)? <= emissions::POOL_CAP { return Ok(0); }
    let mut lo=0u64; let mut hi=inv.treasury;
    while lo<hi {
        let mid=lo+(hi-lo)/2;
        if curve.value(mid)? >= emissions::POOL_CAP { hi=mid; } else { lo=mid+1; }
    }
    let excess=inv.treasury-lo;
    inv.treasury=lo;
    p.fee_pending=p.fee_pending.checked_add(excess).ok_or(MixError::Arithmetic)?;
    distribute(p)?;
    Ok(excess)
}
#[event]
pub struct OwnershipAwarded { pub position: Pubkey, pub pool: Pubkey, pub owner: Pubkey, pub pool_tokens: u64, pub issued: u128 }

#[cfg(test)]
mod tests {
    use super::*;
    fn inventory(amount: u64) -> Inventory { Inventory { pool: Pubkey::default(), asset: Pubkey::default(), locked: 999, treasury: amount, losses: 0, paid: 0, dust: 0, bump: 0 } }
    #[test]
    fn actual_receipts_fund_stakers_without_touching_payouts_or_legacy_developer_fees() {
        let mut p = OwnershipPool { dev_fees: 7, ..OwnershipPool::default() };
        let mut t = OwnershipTrader::default(); let mut r = OwnershipReceipt::default(); let mut i = inventory(9000);
        fund_rewards(&mut p,800).unwrap();
        assert_eq!((i.treasury,p.fee_pending,p.dev_fees,i.locked),(9000,800,7,999));
        award(&mut p,&mut t,&mut r,9000).unwrap(); assert_eq!(r.issued,900_000);
        assert!(award(&mut p,&mut t,&mut r,9000).is_err());
        change_stake(&mut p,&mut t,&mut i,900_000,false).unwrap(); accrue(&mut p,&mut t).unwrap();
        assert!(t.pending<=800 && t.pending>=799); assert_eq!(p.pending_liabilities,t.pending);
        change_stake(&mut p,&mut t,&mut i,900_000,true).unwrap();
        assert_eq!(i.treasury+p.reward_pool+p.fee_pending,9800);
    }
    #[test]
    fn large_issuance_and_zero_receipts_are_safe() {
        let mut p=OwnershipPool::default();let mut t=OwnershipTrader::default();let mut r=OwnershipReceipt::default();
        award(&mut p,&mut t,&mut r,u64::MAX).unwrap();assert_eq!(r.issued,u64::MAX as u128*100);
        let mut r=OwnershipReceipt::default();award(&mut p,&mut t,&mut r,0).unwrap();assert_eq!(r.issued,0);assert!(r.awarded);
    }
    #[test]
    fn stake_churn_cannot_take_another_stakers_rewards_or_mint_rounding() {
        let mut p=OwnershipPool::default();let mut a=OwnershipTrader::default();let mut b=OwnershipTrader::default();let mut i=inventory(0);
        a.available=700;b.available=300;
        change_stake(&mut p,&mut a,&mut i,700,false).unwrap();change_stake(&mut p,&mut b,&mut i,300,false).unwrap();
        for _ in 0..100 {
            i.treasury+=9000;fund_rewards(&mut p,800).unwrap();
            change_stake(&mut p,&mut b,&mut i,300,true).unwrap();change_stake(&mut p,&mut b,&mut i,300,false).unwrap();
        }
        change_stake(&mut p,&mut a,&mut i,700,true).unwrap();change_stake(&mut p,&mut b,&mut i,300,true).unwrap();
        assert_eq!((a.pending,b.pending),(56_000,24_000));assert_eq!(p.dev_fees,0);
        assert_eq!(i.treasury+p.reward_pool+p.fee_pending,980_000);
    }
}
