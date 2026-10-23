//! Mixed backing arithmetic. Callers authenticate asset identities and fresh
//! quote snapshots before using this policy; this module never transfers funds.
//! The existing live instructions remain single-asset until migration and
//! settlement integration are complete.
use anchor_lang::prelude::*;
use crate::{MixError, stake_backing::{CoreStakeState, CoreCoverageState, Coverage,
    BackedPosition, coverage_tokens, check_unlock, reserved_usd}};

pub const ASSETS: usize = 3;

/// Stable order: market token, wrapped SOL, USDC. The primary slot wins when
/// a native market repeats SOL or USDC; an auxiliary duplicate must stay empty.
#[derive(Clone, Copy, Debug)]
pub struct Assets(pub [Pubkey; ASSETS]);
impl Assets {
    pub fn new(primary: Pubkey, sol: Pubkey, usd: Pubkey) -> Result<Self> {
        require!(sol != usd && ![primary, sol, usd].contains(&Pubkey::default()), MixError::Binding);
        Ok(Self([primary, sol, usd]))
    }
    pub fn index(&self, asset: Pubkey) -> Result<usize> {
        self.0.iter().position(|key| *key == asset).ok_or_else(|| error!(MixError::Binding))
    }
    pub fn active(&self, i: usize) -> bool {
        i < ASSETS && !self.0[..i].contains(&self.0[i])
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Basket { pub stakes: [CoreStakeState; ASSETS] }
impl Basket {
    pub fn validate(&self, assets: &Assets) -> Result<()> {
        for (i, stake) in self.stakes.iter().enumerate() {
            require!(assets.active(i) || *stake == CoreStakeState::default(), MixError::Binding);
            stake.uncovered_usd()?;
        }
        Ok(())
    }
    /// Validate the entire update before applying any acknowledgement.
    pub fn sync(&mut self, sources: [CoreCoverageState; ASSETS], assets: &Assets) -> Result<()> {
        self.validate(assets)?;
        let mut next = *self;
        for (i, source) in sources.into_iter().enumerate() {
            require!(assets.active(i) || source == CoreCoverageState::default(), MixError::Binding);
            next.stakes[i].sync(source)?;
        }
        *self = next;
        Ok(())
    }
    /// `available` is the matching authenticated ER balance, never a second
    /// credit. The caller writes this and the reserve in the same instruction.
    pub fn change(&mut self, assets: &Assets, asset: Pubkey, available: &mut u64,
        amount: u64, unlock: bool, positions: &[BackedPosition]) -> Result<()> {
        self.validate(assets)?;
        require!(amount > 0, MixError::Amount);
        let i = assets.index(asset)?;
        let (balance, tokens) = if unlock {
            check_unlock(positions, 0)?;
            (available.checked_add(amount).ok_or(MixError::Arithmetic)?,
                self.stakes[i].tokens.checked_sub(amount).ok_or(MixError::Inventory)?)
        } else {
            (available.checked_sub(amount).ok_or(MixError::Inventory)?,
                self.stakes[i].tokens.checked_add(amount).ok_or(MixError::Arithmetic)?)
        };
        *available = balance;
        self.stakes[i].tokens = tokens;
        Ok(())
    }
    /// Empty assets require no price. A nonempty asset with an unavailable
    /// quote fails the calculation; it is never silently valued at zero.
    pub fn values<F>(&self, assets: &Assets, quote: &mut F) -> Result<[u64; ASSETS]>
    where F: FnMut(usize, u64) -> Result<u64> {
        self.validate(assets)?;
        let mut values = [0; ASSETS];
        for (i, stake) in self.stakes.iter().enumerate() {
            if stake.tokens > 0 { values[i] = quote(i, stake.tokens)?; }
        }
        Ok(values)
    }
    pub fn capacity<F>(&self, assets: &Assets, positions: &[BackedPosition], quote: &mut F) -> Result<(u64, u64)>
    where F: FnMut(usize, u64) -> Result<u64> {
        let value = sum(self.values(assets, quote)?)?;
        Ok((value, value.saturating_sub(reserved_usd(positions)?)))
    }
}

fn sum(values: [u64; ASSETS]) -> Result<u64> {
    values.into_iter().try_fold(0u64, |a, b| a.checked_add(b).ok_or_else(|| error!(MixError::Arithmetic)))
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Plan {
    pub debits: [Coverage; ASSETS],
    /// USD not covered after exhausting all currently usable reserve value.
    pub uncovered_usd: u64,
}

/// Allocate the required coverage by current full-amount executable USD value,
/// then size whole-atom debits with the existing conservative inverse quote.
/// No asset can be charged beyond its acknowledged available stake.
pub fn coverage<F>(available: [u64; ASSETS], assets: &Assets, target: u64, quote: &mut F) -> Result<Plan>
where F: FnMut(usize, u64) -> Result<u64> {
    let mut values = [0; ASSETS];
    for i in 0..ASSETS {
        require!(assets.active(i) || available[i] == 0, MixError::Binding);
        if target > 0 && available[i] > 0 { values[i] = quote(i, available[i])?; }
    }
    let total = sum(values)?;
    if target == 0 { return Ok(Plan::default()); }
    if total == 0 { return Ok(Plan { uncovered_usd: target, ..Plan::default() }); }
    let covered = target.min(total);
    let mut shares = [0; ASSETS];
    for i in 0..ASSETS { shares[i] = ((covered as u128 * values[i] as u128) / total as u128) as u64; }
    let mut remainder = covered - sum(shares)?;
    for i in 0..ASSETS {
        if remainder > 0 && shares[i] < values[i] { shares[i] += 1; remainder -= 1; }
    }
    require!(remainder == 0, MixError::Arithmetic);
    let mut plan = Plan { uncovered_usd: target - covered, ..Plan::default() };
    for i in 0..ASSETS {
        plan.debits[i] = if shares[i]>0 && shares[i]==values[i] {
            Coverage{tokens:available[i],quoted_usd:values[i],uncovered_usd:0}
        } else {coverage_tokens(available[i], shares[i], &mut |n| quote(i, n))?};
        require!(plan.debits[i].uncovered_usd == 0, MixError::Quote);
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn assets() -> Assets { Assets::new(Pubkey::new_unique(), Pubkey::new_unique(), Pubkey::new_unique()).unwrap() }
    fn position(pending: bool) -> BackedPosition { BackedPosition { nonce: 1, opened_at: 1, entry_usd: 200,
        closing_usd: if pending { Some(0) } else { None } } }
    fn basket(tokens: [u64; 3]) -> Basket { Basket { stakes: tokens.map(|tokens| CoreStakeState {tokens, ..CoreStakeState::default()}) } }
    #[test] fn all_three_reserve_together_without_spending_wager_collateral() {
        let ids = assets(); let mut b = basket([0; 3]); let mut balances = [100, 2, 100];
        for i in 0..3 { let amount=balances[i]; b.change(&ids, ids.0[i], &mut balances[i], amount, false, &[]).unwrap(); }
        assert_eq!(balances, [0; 3]);
        let quote = &mut |i, n| Ok(n * [1, 50, 1][i]);
        assert_eq!(b.capacity(&ids, &[position(false)], quote).unwrap(), (300, 100));
        assert_eq!(b.capacity(&ids, &[position(true)], quote).unwrap(), (300, 100));
        for i in 0..3 { let before=b; assert!(b.change(&ids, ids.0[i], &mut balances[i], 1, true, &[position(true)]).is_err()); assert_eq!(b,before); }
    }
    #[test] fn duplicate_native_primary_is_never_double_counted() {
        let sol=Pubkey::new_unique(); let usd=Pubkey::new_unique();
        for (primary, duplicate) in [(sol,1),(usd,2)] {
            let ids=Assets::new(primary,sol,usd).unwrap();
            assert_eq!(ids.index(primary).unwrap(),0);
            let mut b=basket([10,0,0]); assert_eq!(b.capacity(&ids,&[],&mut |_,n|Ok(n)).unwrap(),(10,10));
            b.stakes[duplicate].tokens=10; assert!(b.values(&ids,&mut |_,n|Ok(n)).is_err());
            assert!(coverage([10,10,10],&ids,1,&mut |_,n|Ok(n)).is_err());
        }
    }
    #[test] fn coverage_proportions_and_rounding_do_not_lose_value() {
        let ids=assets();
        for available in [[100,100,100],[1,1,1],[0,100,10],[11,5,0]] {
            for target in 0..=350 {
                let p=coverage(available,&ids,target,&mut |_,n|Ok(n)).unwrap();
                assert_eq!(p.debits.iter().map(|d|d.quoted_usd).sum::<u64>() + p.uncovered_usd,target);
                for i in 0..3 {assert!(p.debits[i].tokens<=available[i]);}
            }
        }
        assert_eq!(coverage([100;3],&ids,60,&mut |_,n|Ok(n)).unwrap().debits.map(|d|d.tokens),[20;3]);
    }
    #[test] fn quote_errors_cannot_release_reservations_and_empty_assets_do_not_need_quotes() {
        let ids=assets(); let b=basket([0,10,0]);
        assert_eq!(b.capacity(&ids,&[],&mut |i,n|{assert_eq!(i,1);Ok(n)}).unwrap(),(10,10));
        assert!(b.capacity(&ids,&[],&mut |_,_|err!(MixError::Stale)).is_err());
        assert!(coverage([0,10,0],&ids,1,&mut |_,_|err!(MixError::Stale)).is_err());
        assert!(basket([u64::MAX,1,0]).capacity(&ids,&[],&mut |_,n|Ok(n)).is_err());
    }
    #[test] fn acknowledgements_are_once_only_and_updates_are_atomic() {
        let ids=assets(); let mut b=basket([100;3]);
        let sources=[10,20,30].map(|debited_tokens|CoreCoverageState{debited_tokens,..CoreCoverageState::default()});
        b.sync(sources,&ids).unwrap(); assert_eq!(b.stakes.map(|s|s.tokens),[90,80,70]);
        b.sync(sources,&ids).unwrap(); b.sync([CoreCoverageState::default();3],&ids).unwrap();
        assert_eq!(b.stakes.map(|s|s.tokens),[90,80,70]);
        let before=b; let mut bad=sources; bad[0].debited_tokens+=1; bad[2].debited_tokens=101;
        assert!(b.sync(bad,&ids).is_err()); assert_eq!(b,before);
    }
    #[test] fn unknown_asset_and_failed_unlock_do_not_mutate_balances() {
        let ids=assets(); let mut b=basket([1;3]); let mut available=u64::MAX;
        assert!(b.change(&ids,Pubkey::new_unique(),&mut available,1,false,&[]).is_err());
        let before=b; assert!(b.change(&ids,ids.0[0],&mut available,1,true,&[]).is_err());
        assert_eq!(b,before); assert_eq!(available,u64::MAX);
    }
}
