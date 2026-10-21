//! Authenticated reserve ratios, without executing a sale or subtracting fees.
//! Values are micro-USD. Intermediate quote atoms round down, consistently in
//! the browser and program; final claim sizing rounds up to the first token atom
//! covering the remaining entitlement. No DEX liquidity cap applies to transfers.
use anchor_lang::prelude::*;
use crate::{math, MixError};

#[derive(Clone, Copy)]
pub struct SpotCurve { pub base:u64, pub quote:u64, pub usd_base:u64, pub usd_quote:u64 }
impl SpotCurve {
    pub fn value(&self,amount:u64)->Result<u64> {
        if amount==0 {return Ok(0);}
        require!(self.base>0 && self.usd_base>0 && self.usd_quote>0,MixError::Quote);
        // Authenticated depleted source liquidity is worth zero. Loss settlement
        // must still return collateral; redemption separately rejects zero value.
        if self.quote==0 {return Ok(0);}
        let quote=(amount as u128).checked_mul(self.quote as u128).ok_or(MixError::Arithmetic)? / self.base as u128;
        math::div(quote,self.usd_base as u128,self.usd_quote as u128)
    }
    /// Transfer the minimum whole-atom amount covering the claim, or all
    /// available treasury for a partial payment. Never spend locked collateral.
    pub fn payout(&self,remaining:u64,available:u64)->Result<(u64,u64)> {
        require!(remaining>0,MixError::NoClaim);
        require!(available>0,MixError::Inventory);
        let value=self.value(available)?;
        require!(value>0,MixError::Inventory);
        if value<remaining {return Ok((available,value));}
        let (mut lo,mut hi)=(1,available);
        while lo<hi {
            let mid=lo+(hi-lo)/2;
            if self.value(mid)? >= remaining {hi=mid;} else {lo=mid+1;}
        }
        Ok((lo,remaining))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn curve()->SpotCurve {SpotCurve{base:1_000_000_000,quote:2_000_000_000,usd_base:100_000_000,usd_quote:1_000_000_000}}
    #[test] fn no_sale_impact_even_when_pool_exceeds_dex_reserves(){
        let c=curve();assert_eq!(c.value(1_000_000_000).unwrap(),200_000_000);
        assert_eq!(c.value(10_000_000_000).unwrap(),2_000_000_000);
        assert_eq!(c.payout(100_000_000,10_000_000_000).unwrap(),(500_000_000,100_000_000));
    }
    #[test] fn partial_claim_preserves_unpaid_usd_and_reprices_only_remainder(){
        let c=curve();let (tokens,paid)=c.payout(100_000_000,100_000_000).unwrap();
        assert_eq!((tokens,paid),(100_000_000,20_000_000));
        let next=SpotCurve{quote:4_000_000_000,..c};
        assert_eq!(next.payout(100_000_000-paid,1_000_000_000).unwrap(),(200_000_000,80_000_000));
    }
    #[test] fn smallest_atom_which_covers_claim_and_no_zero_value_debits(){
        for amount in [1,7,1_000_001,999_999_999] {let c=curve();let (n,d)=c.payout(amount,10_000_000_000).unwrap();
            assert_eq!(d,amount);assert!(c.value(n).unwrap()>=amount);assert!(c.value(n-1).unwrap()<amount);}
        assert!(curve().payout(1,1).is_err());assert!(curve().payout(0,100).is_err());
        assert!(SpotCurve{base:0,..curve()}.value(1).is_err());
        assert!(SpotCurve{base:1,quote:u64::MAX,usd_base:u64::MAX,usd_quote:1}.value(u64::MAX).is_err());
    }
}
