//! Core-token backing policy for the replacement mainnet collateral model.
//! Amounts are token atoms; values are USD micro-units. Quote callbacks must use
//! one validated, fresh pool snapshot, including fees and full-size price impact.
//! Entry capacity and base coverage settlement share the ER stake ledger.
//! Cumulative base debits are acknowledged before pending slots are released.
use crate::{engine::{ER_SLOTS, ER_TRADE, ER_FREE, ErTrader, ErCustody}, Inventory, MixError};
use anchor_lang::prelude::*;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq, Eq)]
pub struct CoreStakeState {
    pub tokens: u64,
    pub acknowledged_debits: u64,
    pub incurred_usd: u64,
    pub repaid_usd: u64,
}
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, Debug, PartialEq, Eq)]
pub struct CoreCoverageState {
    pub debited_tokens: u64,
    pub incurred_usd: u64,
    pub repaid_usd: u64,
}
impl CoreStakeState {
    pub fn uncovered_usd(&self) -> Result<u64> {
        self.incurred_usd.checked_sub(self.repaid_usd).ok_or_else(|| error!(MixError::Binding))
    }
    /// Apply each base charge once. Older cloned snapshots are harmless.
    pub fn sync(&mut self, source: CoreCoverageState) -> Result<()> {
        require!(source.repaid_usd <= source.incurred_usd, MixError::Binding);
        let debit = source.debited_tokens.saturating_sub(self.acknowledged_debits);
        let tokens = self.tokens.checked_sub(debit).ok_or(MixError::Inventory)?;
        let incurred = self.incurred_usd.max(source.incurred_usd);
        let repaid = self.repaid_usd.max(source.repaid_usd);
        require!(repaid <= incurred, MixError::Binding);
        self.tokens = tokens;
        self.acknowledged_debits = self.acknowledged_debits.max(source.debited_tokens);
        self.incurred_usd = incurred;
        self.repaid_usd = repaid;
        Ok(())
    }
}

pub fn core_asset() -> Pubkey {
    #[cfg(feature="mainnet")]
    let mint=crate::mainnet_config::PRIMARY_MINT;
    #[cfg(not(feature="mainnet"))]
    let mint=pubkey!("CCpKQPW9eCMeEQ7wG8tbZPqWMHEBt9cTQEstFW5W6DaL");
    Pubkey::find_program_address(&[b"asset", mint.as_ref()], &crate::ID).0
}

/// Identities, never tickers: wrapped SOL, the deployment's USDC and its core token.
pub fn exempt_assets() -> [Pubkey;3] {
    #[cfg(feature="mainnet")]
    let usd=crate::mainnet_pump::USDC;
    #[cfg(not(feature="mainnet"))]
    let usd=pubkey!("AcXG57AXp3dtddW9XwX5cvzbEVqNxmhuQJH2BiLHeaXT");
    [core_asset(),crate::engine::sol_collateral_asset(),Pubkey::find_program_address(&[b"asset",usd.as_ref()],&crate::ID).0]
}
pub fn requires_backing(asset:Pubkey)->bool { !exempt_assets().contains(&asset) }

pub fn positions(trader: &ErTrader) -> Vec<BackedPosition> {
    let exempt=exempt_assets();
    trader.slots.iter().filter(|s| s.kind == ER_TRADE && s.state != ER_FREE && !exempt.contains(&s.asset))
        .map(|s| BackedPosition { nonce: s.nonce, opened_at: s.opened_at,
            entry_usd: s.entry_value, closing_usd: if s.state == crate::engine::ER_OPEN { None } else { Some(0) } }).collect()
}

pub fn check_entry(trader: &ErTrader, core: &crate::engine::ErQuote, value: u64, now: i64) -> Result<()> {
    require!(core.asset == core_asset() && core.config == trader.config && !core.is_quote, MixError::Binding);
    check_open(core.value(trader.core_stake.tokens, now)?, &positions(trader), value, trader.core_stake.uncovered_usd()?)
}

/// Force one oldest close, then reconcile its real base charge before forcing
/// another. Pending entry reservations must not trigger a cascade of unnecessary
/// closes, and estimated shortfalls must never release backing.
pub fn check_forced_close(trader: &ErTrader, core: &crate::engine::ErQuote, slot: u8, now: i64) -> Result<()> {
    require!(core.asset == core_asset() && core.config == trader.config && !core.is_quote, MixError::Binding);
    let positions = positions(trader);
    require!(!positions.iter().any(|p| p.closing_usd.is_some()), MixError::Closed);
    let oldest = trader.slots.iter().enumerate().filter(|(_,s)| s.state == crate::engine::ER_OPEN && positions.iter().any(|p|p.nonce==s.nonce))
        .min_by_key(|(_,s)| (s.opened_at, s.nonce)).map(|(i,_)| i).ok_or(MixError::Closed)?;
    require!(oldest == slot as usize, MixError::QueueOrder);
    require!(core.value(trader.core_stake.tokens, now)? < reserved_usd(&positions)?, MixError::Exposure);
    Ok(())
}

pub fn available_stake(custody: &ErCustody, committed: &ErTrader) -> Result<u64> {
    let pending = custody.core_coverage.debited_tokens.checked_sub(committed.core_stake.acknowledged_debits)
        .ok_or(MixError::Replay)?;
    committed.core_stake.tokens.checked_sub(pending).ok_or_else(|| error!(MixError::Inventory))
}

/// Tokens are already held in base custody. Reserving them on the canonical ER
/// trader removes them from spendable collateral without issuing new credits.
pub fn change_stake(trader: &mut ErTrader, amount: u64, unstake: bool) -> Result<()> {
    require!(amount > 0, MixError::Amount);
    let index = trader.asset_index(core_asset())?;
    if unstake {
        require!(positions(trader).is_empty(), MixError::Closed);
        let staked = trader.core_stake.tokens.checked_sub(amount).ok_or(MixError::Inventory)?;
        let available = trader.assets[index].available.checked_add(amount).ok_or(MixError::Arithmetic)?;
        trader.core_stake.tokens = staked;
        trader.assets[index].available = available;
    } else {
        let available = trader.assets[index].available.checked_sub(amount).ok_or(MixError::Inventory)?;
        let staked = trader.core_stake.tokens.checked_add(amount).ok_or(MixError::Arithmetic)?;
        trader.assets[index].available = available;
        trader.core_stake.tokens = staked;
    }
    Ok(())
}

/// Locked core collateral and payout inventory share the canonical core vault.
/// Moving accounting from locked to treasury therefore needs no market swap.
/// Caller must authenticate the committed ER trader and consume the position's
/// settlement receipt in the SAME instruction; this is not a public entrypoint.
pub fn allocate_coverage(custody: &mut ErCustody, committed: &ErTrader, inventory: &mut Inventory,
    debit: Coverage) -> Result<()> {
    require!(custody.owner == committed.owner && custody.config == committed.config
        && custody.pool == committed.pool && inventory.pool == custody.pool, MixError::Binding);
    if debit.tokens==0 {
        custody.core_coverage.incurred_usd=custody.core_coverage.incurred_usd.checked_add(debit.uncovered_usd).ok_or(MixError::Arithmetic)?;
        return Ok(());
    }
    require!(inventory.asset == core_asset(),MixError::Binding);
    let available = available_stake(custody, committed)?;
    require!(debit.tokens <= available, MixError::Inventory);
    let index = custody.assets.iter().position(|a| a.asset == core_asset()).ok_or(MixError::Inventory)?;
    let balance = custody.assets[index].available.checked_sub(debit.tokens).ok_or(MixError::Inventory)?;
    let locked = inventory.locked.checked_sub(debit.tokens).ok_or(MixError::Inventory)?;
    let treasury = inventory.treasury.checked_add(debit.tokens).ok_or(MixError::Arithmetic)?;
    let debited = custody.core_coverage.debited_tokens.checked_add(debit.tokens).ok_or(MixError::Arithmetic)?;
    let incurred = custody.core_coverage.incurred_usd.checked_add(debit.uncovered_usd).ok_or(MixError::Arithmetic)?;
    custody.assets[index].available = balance;
    inventory.locked = locked;
    inventory.treasury = treasury;
    custody.core_coverage.debited_tokens = debited;
    custody.core_coverage.incurred_usd = incurred;
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BackedPosition {
    pub nonce: u64,
    pub opened_at: i64,
    pub entry_usd: u64,
    /// None while open. Some is the full original bag's closing quote estimate;
    /// it cannot release any backing before the base settlement receipt arrives.
    pub closing_usd: Option<u64>,
}

pub fn shortfall(entry_usd: u64, closing_usd: u64) -> u64 {
    entry_usd.saturating_sub(closing_usd)
}

/// Only the lost fraction consumes stake. Full-bag quotes use the same size at
/// entry and close; allocate their decline pro rata to the forfeited tokens.
/// Ceiling is at most one USD micro-unit and never exceeds the bag's decline.
pub fn loss_shortfall(entry_usd:u64, closing_usd:u64, lost:u64, margin:u64)->Result<u64> {
    require!(margin>0 && lost<=margin,MixError::Amount);
    crate::math::ceil(shortfall(entry_usd,closing_usd) as u128*lost as u128,margin as u128)
}

fn validate(positions: &[BackedPosition]) -> Result<()> {
    require!(positions.len() <= ER_SLOTS, MixError::Exposure);
    for (i, position) in positions.iter().enumerate() {
        require!(position.nonce > 0 && position.opened_at > 0 && position.entry_usd > 0, MixError::Binding);
        require!(!positions[..i].iter().any(|p| p.nonce == position.nonce), MixError::Replay);
    }
    Ok(())
}

/// Reserve entry value BEFORE leverage, including for frozen closes. The actual
/// quote/coverage charge is finalized at base settlement, so an ER close quote
/// must not make backing reusable before its matching receipt is acknowledged.
pub fn reserved_usd(positions: &[BackedPosition]) -> Result<u64> {
    validate(positions)?;
    positions.iter().try_fold(0u64, |sum, p| {
        sum.checked_add(p.entry_usd)
            .ok_or_else(|| error!(MixError::Arithmetic))
    })
}

pub fn check_open(stake_quote_usd: u64, positions: &[BackedPosition], entry_usd: u64, _recorded_pool_shortfall: u64) -> Result<()> {
    require!(entry_usd > 0, MixError::Exposure);
    let required = reserved_usd(positions)?.checked_add(entry_usd).ok_or(MixError::Arithmetic)?;
    require!(required <= stake_quote_usd, MixError::Exposure);
    Ok(())
}

/// Pending settlement is still locked, even when the frozen shortfall is zero.
pub fn check_unlock(positions: &[BackedPosition], _recorded_pool_shortfall: u64) -> Result<()> {
    validate(positions)?;
    require!(positions.is_empty(), MixError::Closed);
    Ok(())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Coverage {
    pub tokens: u64,
    pub quoted_usd: u64,
    pub uncovered_usd: u64,
}

/// Size a core-token amount whose fresh executable quote covers the shortfall.
/// Verify the result explicitly: independent fee ceilings can produce tiny
/// non-monotonic rounding steps, so this does not promise the global minimum atom.
/// Binary search evaluates cached pool arithmetic; it must not perform RPC calls.
/// A depleted stake is exhausted and the uncovered amount remains explicit.
pub fn coverage_tokens<F>(available: u64, target_usd: u64, quote: &mut F) -> Result<Coverage>
where F: FnMut(u64) -> Result<u64> {
    if target_usd == 0 { return Ok(Coverage::default()); }
    if available == 0 { return Ok(Coverage { uncovered_usd: target_usd, ..Coverage::default() }); }
    require!(quote(0)? == 0, MixError::Quote);
    let maximum = quote(available)?;
    if maximum < target_usd {
        return Ok(Coverage { tokens: available, quoted_usd: maximum, uncovered_usd: target_usd - maximum });
    }
    let mut low = 1u64;
    let mut high = available;
    while low < high {
        let mid = low + (high - low) / 2;
        if quote(mid)? >= target_usd { high = mid; } else { low = mid + 1; }
    }
    let quoted_usd = quote(low)?;
    require!(quoted_usd >= target_usd, MixError::Quote);
    Ok(Coverage { tokens: low, quoted_usd, uncovered_usd: 0 })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ClosePlan {
    /// Strict oldest-first order, with nonce as the deterministic tie breaker.
    pub nonces: Vec<u64>,
    pub reserved_tokens: u64,
    pub remaining_stake_usd: u64,
    pub remaining_open_usd: u64,
    pub uncovered_usd: u64,
}

/// Revalue remaining stake after each projected coverage charge. Merely
/// subtracting a closed bet's value would miss the stake consumed by its loss.
/// This is a quote-based preview, not authority to close the entire plan at once.
/// The keeper must recheck eligibility and oldest order atomically for each close
/// and reconcile actual base coverage debits. Pending quotes never free capacity.
pub fn plan_oldest_closes<F>(positions: &[BackedPosition], closing_quotes: &[(u64, u64)], staked_tokens: u64, quote: &mut F) -> Result<ClosePlan>
where F: FnMut(u64) -> Result<u64> {
    validate(positions)?;
    require!(closing_quotes.len() <= ER_SLOTS, MixError::Binding);
    for (i, (nonce, _)) in closing_quotes.iter().enumerate() {
        require!(positions.iter().any(|p| p.nonce == *nonce && p.closing_usd.is_none())
            && !closing_quotes[..i].iter().any(|(n, _)| n == nonce), MixError::Binding);
    }
    let pending = positions.iter().filter_map(|p| p.closing_usd.map(|q| shortfall(p.entry_usd, q)))
        .try_fold(0u64, |sum, value| sum.checked_add(value).ok_or(MixError::Arithmetic))?;
    let reserved = coverage_tokens(staked_tokens, pending, quote)?;
    let mut remaining = staked_tokens - reserved.tokens;
    let mut open: Vec<_> = positions.iter().filter(|p| p.closing_usd.is_none()).collect();
    open.sort_by_key(|p| (p.opened_at, p.nonce));
    let mut plan = ClosePlan {
        reserved_tokens: reserved.tokens,
        uncovered_usd: reserved.uncovered_usd,
        remaining_open_usd: open.iter().try_fold(0u64, |sum, p| sum.checked_add(p.entry_usd).ok_or(MixError::Arithmetic))?,
        ..ClosePlan::default()
    };
    for position in open {
        let value = if remaining == 0 { 0 } else { quote(remaining)? };
        if value >= plan.remaining_open_usd { break; }
        let closing = closing_quotes.iter().find(|(nonce, _)| *nonce == position.nonce)
            .map(|(_, value)| *value).ok_or(MixError::Stale)?;
        let debit = coverage_tokens(remaining, shortfall(position.entry_usd, closing), quote)?;
        remaining -= debit.tokens;
        plan.reserved_tokens = plan.reserved_tokens.checked_add(debit.tokens).ok_or(MixError::Arithmetic)?;
        plan.uncovered_usd = plan.uncovered_usd.checked_add(debit.uncovered_usd).ok_or(MixError::Arithmetic)?;
        plan.remaining_open_usd -= position.entry_usd;
        plan.nonces.push(position.nonce);
    }
    plan.remaining_stake_usd = if remaining == 0 { 0 } else { quote(remaining)? };
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    const USD: u64 = 1_000_000;
    fn open(nonce: u64, dollars: u64) -> BackedPosition {
        BackedPosition { nonce, opened_at: 100 + nonce as i64, entry_usd: dollars * USD, closing_usd: None }
    }
    fn dollar_per_atom(amount: u64) -> Result<u64> { amount.checked_mul(USD).ok_or_else(|| error!(MixError::Arithmetic)) }

    fn ledger() -> (ErTrader, ErCustody, crate::engine::ErRisk, Inventory) {
        use crate::engine::*;
        let owner = Pubkey::new_unique(); let config = Pubkey::new_unique(); let pool = Pubkey::new_unique();
        let mut trader = ErTrader { owner, config, pool, next_nonce: 1, ..ErTrader::default() };
        let mut custody = ErCustody { owner, config, pool, ..ErCustody::default() };
        custody.assets[0] = CustodyAsset { asset: core_asset(), deposited_credits: 100, available: 100, ..CustodyAsset::default() };
        let mut risk = ErRisk { config, ..ErRisk::default() };
        sync_custody(&mut trader, &mut risk, &custody).unwrap();
        let inventory = Inventory { pool, asset: core_asset(), locked: 100, treasury: 50, losses: 0, paid: 0, dust: 0, bump: 0 };
        (trader, custody, risk, inventory)
    }

    #[test]
    fn exempt_assets_never_reserve_backing_or_prevent_unlock() {
        use crate::engine::*;
        let (mut trader, _, _, _) = ledger();
        change_stake(&mut trader, 100, false).unwrap();
        for (index, asset) in exempt_assets().into_iter().enumerate() {
            assert!(!requires_backing(asset));
            trader.slots[index] = ErSlot {asset, state: if index==0 {ER_PENDING} else {ER_OPEN},
                kind:ER_TRADE, nonce:index as u64+1,entry_value:100*USD,opened_at:100+index as i64,..ErSlot::default()};
        }
        assert!(positions(&trader).is_empty());
        change_stake(&mut trader, 100, true).unwrap();
        let asset=Pubkey::new_unique();assert!(requires_backing(asset));
        trader.slots[3]=ErSlot{asset,state:ER_OPEN,kind:ER_TRADE,nonce:4,entry_value:USD,opened_at:104,..ErSlot::default()};
        assert_eq!(reserved_usd(&positions(&trader)).unwrap(),USD);
        let core=ErQuote{config:trader.config,asset:core_asset(),source_slot:1,publish_time:110,
            base_reserve:1_000,quote_reserve:1_000,
            #[cfg(feature="mainnet")]
            mainnet:crate::MainnetCollateralContext{real_quote_reserve:1_000,usd_per_quote_atom_e21:100_000_000_000_000,conversion_publish_time:110,conversion_source_slot:1,pool_source_slot:1},..ErQuote::default()};
        check_forced_close(&trader,&core,3,110).unwrap();
        assert!(check_forced_close(&trader,&core,1,110).is_err());
        change_stake(&mut trader, 100, false).unwrap();
        assert!(change_stake(&mut trader, 100, true).is_err());
    }

    #[test]
    fn only_lost_tokens_need_coverage_and_winners_never_consume_stake() {
        assert_eq!(loss_shortfall(100*USD,50*USD,20,100).unwrap(),10*USD);
        assert_eq!(loss_shortfall(100*USD,40*USD,0,100).unwrap(),0);
        assert_eq!(loss_shortfall(100*USD,40*USD,100,100).unwrap(),60*USD);
        assert_eq!(loss_shortfall(100*USD,110*USD,20,100).unwrap(),0);
        assert_eq!(loss_shortfall(100,99,1,100).unwrap(),1);
        assert!(loss_shortfall(100,50,101,100).is_err());
    }
    #[test]
    fn stake_is_removed_from_spendable_collateral_and_withdrawals() {
        use crate::engine::*;
        let (mut trader, custody, mut risk, _) = ledger();
        change_stake(&mut trader, 60, false).unwrap();
        assert_eq!((trader.core_stake.tokens, trader.assets[0].available), (60, 40));
        assert!(withdraw_slot(&mut trader, &mut risk, core_asset(), 0, 1, 41, 110, 100).is_err());
        sync_custody(&mut trader, &mut risk, &custody).unwrap();
        assert_eq!(trader.assets[0].available, 40, "base custody cannot credit staked tokens again");
        assert!(change_stake(&mut trader, 41, false).is_err());
        change_stake(&mut trader, 60, true).unwrap();
        assert_eq!((trader.core_stake.tokens, trader.assets[0].available), (0, 100));
    }

    #[test]
    fn both_open_and_pending_trades_block_actual_stake_unlocking() {
        use crate::engine::*;
        let (mut trader, _, _, _) = ledger();
        change_stake(&mut trader, 100, false).unwrap();
        for state in [ER_OPEN, ER_PENDING] {
            trader.slots[0] = ErSlot { state, kind: ER_TRADE, nonce: 1, ..ErSlot::default() };
            assert!(change_stake(&mut trader, 1, true).is_err());
            assert_eq!((trader.core_stake.tokens, trader.assets[0].available), (100, 0));
        }
        trader.slots[0] = ErSlot::default();
        change_stake(&mut trader, 100, true).unwrap();
    }

    #[test]
    fn base_coverage_moves_locked_tokens_to_pool_and_syncs_once() {
        use crate::engine::sync_custody;
        let (mut trader, mut custody, mut risk, mut inventory) = ledger();
        change_stake(&mut trader, 60, false).unwrap();
        let old = custody.core_coverage;
        allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 20, quoted_usd: 20 * USD, uncovered_usd: 0 }).unwrap();
        assert_eq!((custody.assets[0].available, inventory.locked, inventory.treasury), (80, 80, 70));
        assert_eq!(inventory.locked + inventory.treasury, 150);
        let latest = custody.core_coverage;
        sync_custody(&mut trader, &mut risk, &custody).unwrap();
        assert_eq!((trader.core_stake.tokens, trader.assets[0].available), (40, 40));
        for state in [latest, old, latest] {
            custody.core_coverage = state;
            sync_custody(&mut trader, &mut risk, &custody).unwrap();
            assert_eq!((trader.core_stake.tokens, trader.assets[0].available), (40, 40));
        }
        change_stake(&mut trader, 40, true).unwrap();
        assert_eq!(trader.assets[0].available, 80);
    }

    #[test]
    fn concurrent_settlements_cannot_reuse_stake_from_an_old_commit() {
        let (mut trader, mut custody, _, mut inventory) = ledger();
        change_stake(&mut trader, 60, false).unwrap();
        allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 40, quoted_usd: 40 * USD, uncovered_usd: 0 }).unwrap();
        assert!(allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 21, quoted_usd: 21 * USD, uncovered_usd: 0 }).is_err());
        assert_eq!((custody.assets[0].available, inventory.treasury), (60, 90));
        allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 20, quoted_usd: 20 * USD, uncovered_usd: 0 }).unwrap();
        assert_eq!((custody.core_coverage.debited_tokens, inventory.locked, inventory.treasury), (60, 40, 110));
    }

    #[test]
    fn recorded_pool_shortfall_survives_old_snapshots_without_user_debt() {
        let (mut trader, mut custody, mut risk, mut inventory) = ledger();
        change_stake(&mut trader, 20, false).unwrap();
        allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 20, quoted_usd: 20 * USD, uncovered_usd: 10 * USD }).unwrap();
        crate::engine::sync_custody(&mut trader, &mut risk, &custody).unwrap();
        trader.core_stake.sync(CoreCoverageState::default()).unwrap();
        assert_eq!(trader.core_stake.uncovered_usd().unwrap(), 10 * USD);
        change_stake(&mut trader, 5, false).unwrap();
        change_stake(&mut trader, 5, true).unwrap();
        assert!(trader.core_stake.sync(CoreCoverageState { debited_tokens: 26, ..custody.core_coverage }).is_err());
        assert_eq!(trader.core_stake.tokens, 0);
    }

    #[test]
    fn coverage_cannot_charge_another_trader_or_another_vault() {
        let (mut trader, mut custody, _, mut inventory) = ledger();
        change_stake(&mut trader, 50, false).unwrap();
        inventory.asset = Pubkey::new_unique();
        assert!(allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 1, ..Coverage::default() }).is_err());
        inventory.asset = core_asset(); custody.owner = Pubkey::new_unique();
        assert!(allocate_coverage(&mut custody, &trader, &mut inventory,
            Coverage { tokens: 1, ..Coverage::default() }).is_err());
        assert_eq!(inventory.locked, 100);
    }

    #[test]
    fn stake_caps_bets_before_leverage_across_all_positions() {
        let mut positions = vec![open(1, 60)];
        check_open(100 * USD, &positions, 40 * USD, 0).unwrap();
        // The independent market risk guard still sees the 1000x notional.
        assert_eq!(crate::risk::check_entry(40 * USD, 1000, 60_000 * USD).unwrap(), 40_000 * USD);
        positions.push(open(2, 40));
        assert!(check_open(100 * USD, &positions, 1, 0).is_err());
        check_open(1_000 * USD, &positions, USD, 1).unwrap();
    }

    #[test]
    fn only_open_and_close_quotes_determine_the_shortfall() {
        assert_eq!(shortfall(100 * USD, 60 * USD), 40 * USD);
        assert_eq!(shortfall(100 * USD, 100 * USD), 0);
        assert_eq!(shortfall(100 * USD, 150 * USD), 0);
        // No observation or historical-low input exists in this policy.
        let cover = coverage_tokens(100, 40 * USD, &mut dollar_per_atom).unwrap();
        assert_eq!(cover, Coverage { tokens: 40, quoted_usd: 40 * USD, uncovered_usd: 0 });
    }

    #[test]
    fn pending_closes_reserve_the_debit_and_cannot_unlock() {
        let mut pending = open(1, 100);
        pending.closing_usd = Some(60 * USD);
        assert_eq!(reserved_usd(&[pending]).unwrap(), 100 * USD);
        assert!(check_open(100 * USD, &[pending], 1, 0).is_err());
        check_open(150 * USD, &[pending], 50 * USD, 0).unwrap();
        assert!(check_unlock(&[pending], 0).is_err());
        pending.closing_usd = Some(100 * USD);
        assert!(check_open(100 * USD, &[pending], 1, 0).is_err(), "even a zero-shortfall estimate cannot free backing early");
        assert!(check_unlock(&[pending], 0).is_err());
        check_unlock(&[], 1).unwrap();
        check_unlock(&[], 0).unwrap();
    }

    #[test]
    fn falling_stake_closes_oldest_and_stops_when_covered() {
        let positions = [open(3, 30), open(1, 40), open(2, 30)];
        let plan = plan_oldest_closes(&positions, &[(1, 40 * USD)], 75, &mut dollar_per_atom).unwrap();
        assert_eq!(plan.nonces, vec![1]);
        assert_eq!((plan.remaining_open_usd, plan.remaining_stake_usd), (60 * USD, 75 * USD));
        assert_eq!(plan.reserved_tokens, 0);
    }

    #[test]
    fn closing_shortfall_can_require_another_oldest_close() {
        let positions = [open(3, 30), open(1, 40), open(2, 30)];
        let plan = plan_oldest_closes(&positions, &[(1, 10 * USD), (2, 30 * USD)], 75, &mut dollar_per_atom).unwrap();
        assert_eq!(plan.nonces, vec![1, 2]);
        assert_eq!((plan.remaining_open_usd, plan.remaining_stake_usd), (30 * USD, 45 * USD));
        assert_eq!(plan.reserved_tokens, 30);
    }

    #[test]
    fn pending_shortfalls_are_reserved_before_deciding_which_open_trade_to_close() {
        let mut pending = open(1, 40); pending.closing_usd = Some(10 * USD);
        let plan = plan_oldest_closes(&[pending, open(2, 30), open(3, 30)], &[(2, 30 * USD)], 75, &mut dollar_per_atom).unwrap();
        assert_eq!(plan.nonces, vec![2]);
        assert_eq!((plan.reserved_tokens, plan.remaining_open_usd), (30, 30 * USD));
    }

    #[test]
    fn stake_exhaustion_is_explicit_and_does_not_keep_trades_open() {
        let plan = plan_oldest_closes(&[open(1, 60), open(2, 40)], &[(1, 0), (2, 0)], 20, &mut dollar_per_atom).unwrap();
        assert_eq!(plan.nonces, vec![1, 2]);
        assert_eq!((plan.reserved_tokens, plan.uncovered_usd, plan.remaining_open_usd), (20, 80 * USD, 0));
        let empty = plan_oldest_closes(&[open(1, 10)], &[(1, 10 * USD)], 0, &mut dollar_per_atom).unwrap();
        assert_eq!(empty.nonces, vec![1]);
    }

    #[test]
    fn no_quote_does_not_mean_zero_and_nonce_breaks_equal_timestamp_ties() {
        let mut newer = open(2, 50); newer.opened_at = 101;
        assert!(plan_oldest_closes(&[newer, open(1, 50)], &[(2, 50 * USD)], 50, &mut dollar_per_atom).is_err());
        let plan = plan_oldest_closes(&[newer, open(1, 50)], &[(1, 50 * USD)], 50, &mut dollar_per_atom).unwrap();
        assert_eq!(plan.nonces, vec![1]);
        assert!(coverage_tokens(10, USD, &mut |_| err!(MixError::Stale)).is_err());
    }

    #[test]
    fn real_curve_shape_is_used_instead_of_spot_price_times_amount() {
        // Constant-product quote with independent integer fee rounding.
        let quote = |tokens: u64| -> Result<u64> {
            let gross = tokens as u128 * 1000 * USD as u128 / (1000 + tokens as u128);
            Ok((gross - (gross * 30 + 9999) / 10000) as u64)
        };
        let coverage = coverage_tokens(1000, 100 * USD, &mut { quote }).unwrap();
        assert!(coverage.tokens > 100, "large orders must include price impact");
        assert!(quote(coverage.tokens).unwrap() >= 100 * USD);
        assert!(quote(coverage.tokens - 1).unwrap() < 100 * USD);
        assert_eq!(coverage.uncovered_usd, 0);
    }

    #[test]
    fn boundary_rounding_overflow_and_duplicate_positions_fail_safely() {
        let coverage = coverage_tokens(u64::MAX, u64::MAX - 1, &mut Ok).unwrap();
        assert_eq!(coverage.tokens, u64::MAX - 1);
        assert_eq!(coverage_tokens(0, 1, &mut Ok).unwrap().uncovered_usd, 1);
        let huge = BackedPosition { entry_usd: u64::MAX, ..open(1, 1) };
        assert!(reserved_usd(&[huge, open(2, 1)]).is_err());
        assert!(reserved_usd(&[open(1, 1), open(1, 1)]).is_err());
        assert!(coverage_tokens(5, 1, &mut |_| Ok(1)).is_err());
    }
}
