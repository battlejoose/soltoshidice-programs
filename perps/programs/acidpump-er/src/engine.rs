//! Persistent ER trading ledger. Base custody and FIFO claims remain authoritative.
//! Collateral reserve snapshots are explicitly publisher-attested devnet data;
//! underlying market prices are read from canonical MagicBlock oracle accounts.
use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::{commit, delegate};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder, MagicIntentBundleBuilder};
use crate::{math, oracle, pump, risk, Asset, Config, MixError, Pool, CollateralGuard, CollateralHistory};
use crate::venue::{ns, shared};
use crate::stake_backing::Backing;
use crate::{BaseExecution,BaseExecutionBumps,__client_accounts_base_execution,__cpi_client_accounts_base_execution};

#[cfg(feature="mainnet")]
pub const ER_SLOTS: usize = 10;
#[cfg(not(feature="mainnet"))]
pub const ER_SLOTS: usize = 8;
#[cfg(feature="mainnet")]
// Keep the original 16 reusable token records. Ten positions do not require
// preallocating the 51 records used by the previous 50-position release.
pub const ER_ASSETS: usize = 16;
#[cfg(not(feature="mainnet"))]
pub const ER_ASSETS: usize = 16;
pub const ER_MARKETS: usize = 4;
pub const ER_FREE: u8 = 0;
pub const ER_OPEN: u8 = 1;
pub const ER_PENDING: u8 = 2;
pub const ER_TRADE: u8 = 1;
pub const ER_WITHDRAW: u8 = 2;
#[cfg(not(feature="mainnet"))]
pub const ER_VALIDATOR: Pubkey = crate::BUILD_ER_VALIDATOR;
#[cfg(feature="mainnet")]
pub const ER_VALIDATOR: Pubkey = crate::deployment_config::VALIDATOR;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct ErBalance {
    pub asset: Pubkey,
    pub credited_deposits: u64,
    pub credited_returns: u64,
    pub available: u64,
    #[cfg(feature="mainnet")]
    pub generation: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct CustodyAsset {
    pub asset: Pubkey,
    pub deposited_credits: u64,
    pub returned_credits: u64,
    /// Total user custody, including collateral reserved by open ER positions.
    pub available: u64,
    #[cfg(feature="mainnet")]
    pub generation: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct ErReceipt {
    pub nonce: u64,
    pub position: Pubkey,
    pub kind: u8,
    pub settled: bool,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct ErSlot {
    pub state: u8,
    pub kind: u8,
    pub nonce: u64,
    pub asset: Pubkey,
    pub market: u16,
    pub margin: u64,
    pub entry_value: u64,
    pub entry: u64,
    pub bust: u64,
    pub leverage: u16,
    pub long: bool,
    pub opened_at: i64,
    pub opened_source_slot: u64,
    pub cursor: u64,
    pub bust_observed: bool,
    pub exit: u64,
    pub closed_at: i64,
    pub close_seq: u64,
    pub liquidated: bool,
}

#[account]
pub struct ErTrader {
    pub owner: Pubkey,
    pub config: Pubkey,
    pub pool: Pubkey,
    pub next_nonce: u64,
    pub notional: u64,
    #[borsh(deserialize_with="crate::heap::deserialize_fixed")]
    pub assets: Box<[ErBalance; ER_ASSETS]>,
    #[borsh(deserialize_with="crate::heap::deserialize_fixed")]
    pub slots: Box<[ErSlot; ER_SLOTS]>,
    pub bump: u8,
    pub core_stake: crate::stake_backing::CoreStakeState,
    #[cfg(feature="mainnet")]
    pub custody_generation: u64,
    /// Auxiliary backing, in canonical SOL / USDC order. When primary is the
    /// same asset its auxiliary slot stays zero (the primary reserve wins).
    pub auxiliary_stakes: [crate::stake_backing::CoreStakeState; 2],
}
impl Default for ErTrader {
    fn default()->Self {
        Self {owner:Pubkey::default(),config:Pubkey::default(),pool:Pubkey::default(),next_nonce:0,notional:0,
            assets:vec![ErBalance::default();ER_ASSETS].into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()),
            slots:vec![ErSlot::default();ER_SLOTS].into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()),
            bump:0,core_stake:crate::stake_backing::CoreStakeState::default(),
            #[cfg(feature="mainnet")] custody_generation:0,
            auxiliary_stakes:[crate::stake_backing::CoreStakeState::default();2]}
    }
}
impl Space for ErTrader {
    const INIT_SPACE: usize = 3 * 32 + 2 * 8 + ER_ASSETS * ErBalance::INIT_SPACE
        + ER_SLOTS * ErSlot::INIT_SPACE + 1
        + 3 * crate::stake_backing::CoreStakeState::INIT_SPACE + if cfg!(feature="mainnet"){8}else{0};
}

#[account]
pub struct ErCustody {
    pub owner: Pubkey,
    pub config: Pubkey,
    pub pool: Pubkey,
    #[borsh(deserialize_with="crate::heap::deserialize_fixed")]
    pub assets: Box<[CustodyAsset; ER_ASSETS]>,
    #[borsh(deserialize_with="crate::heap::deserialize_fixed")]
    pub receipts: Box<[ErReceipt; ER_SLOTS]>,
    pub bump: u8,
    pub core_coverage: crate::stake_backing::CoreCoverageState,
    #[cfg(feature="mainnet")]
    pub generation: u64,
    pub auxiliary_coverage: [crate::stake_backing::CoreCoverageState; 2],
}
impl Default for ErCustody {
    fn default()->Self {
        Self {owner:Pubkey::default(),config:Pubkey::default(),pool:Pubkey::default(),
            assets:vec![CustodyAsset::default();ER_ASSETS].into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()),
            receipts:vec![ErReceipt::default();ER_SLOTS].into_boxed_slice().try_into().unwrap_or_else(|_| unreachable!()),
            bump:0,core_coverage:crate::stake_backing::CoreCoverageState::default(),
            #[cfg(feature="mainnet")] generation:0,
            auxiliary_coverage:[crate::stake_backing::CoreCoverageState::default();2]}
    }
}
impl Space for ErCustody {
    const INIT_SPACE: usize = 3 * 32 + ER_ASSETS * CustodyAsset::INIT_SPACE
        + ER_SLOTS * ErReceipt::INIT_SPACE + 1
        + 3 * crate::stake_backing::CoreCoverageState::INIT_SPACE + if cfg!(feature="mainnet"){8}else{0};
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, InitSpace)]
pub struct ErMarket {
    pub price: u64,
    pub publish_time: i64,
    pub source_slot: u64,
    pub sequence: u64,
    /// Sequence s is stored at s % 64. The current sequence identifies exactly
    /// which 64 entries remain, so storing a second u64 per entry is redundant.
    pub prices: [u64; 64],
    pub long_oi: u64,
    pub short_oi: u64,
    pub long_cap: u64,
    pub short_cap: u64,
}
impl Default for ErMarket {
    fn default() -> Self {
        Self { price: 0, publish_time: 0, source_slot: 0, sequence: 0,
            prices: [0; 64], long_oi: 0, short_oi: 0,
            long_cap: risk::OI_HEADROOM, short_cap: risk::OI_HEADROOM }
    }
}

#[account]
pub struct ErRisk {
    pub config: Pubkey,
    pub next_close_seq: u64,
    pub cap_refreshed_at: i64,
    /// Always initialized with exactly ER_MARKETS elements. Vec deserializes
    /// each market directly onto the heap instead of materializing a large array.
    pub markets: Vec<ErMarket>,
    pub bump: u8,
}
impl Space for ErRisk {
    const INIT_SPACE: usize = 32 + 2 * 8 + 4 + ER_MARKETS * ErMarket::INIT_SPACE + 1;
}
impl Default for ErRisk {
    fn default() -> Self {
        Self { config: Pubkey::default(), next_close_seq: 1, cap_refreshed_at: 0,
            markets: vec![ErMarket::default(); ER_MARKETS], bump: 0 }
    }
}

#[account]
#[derive(InitSpace, Default)]
pub struct ErQuote {
    pub config: Pubkey,
    pub asset: Pubkey,
    pub is_quote: bool,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub fee_bps: [u16; 3],
    pub publish_time: i64,
    pub source_slot: u64,
    pub bump: u8,
    #[cfg(feature="mainnet")]
    pub mainnet: crate::mainnet_collateral::MainnetCollateralContext,
}

#[account]
#[derive(InitSpace, Default)]
pub struct ErFeePayer {
    pub config: Pubkey,
    pub bump: u8,
}

/// Native MagicBlock exists only on ER. Merely owning an initialized Trader on
/// base must never permit execution against a forked ledger before delegation.
#[derive(Accounts)]
pub struct ErExecution<'info> {
    /// CHECK: Fixed executable native builtin, absent from the Solana base chain.
    #[account(address=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID,executable,
        owner=pubkey!("NativeLoader1111111111111111111111111111111"))]
    pub magic_program: UncheckedAccount<'info>,
}

impl ErTrader {
    pub fn asset_index(&self, asset: Pubkey) -> Result<usize> {
        self.assets.iter().position(|a| a.asset == asset)
            .ok_or_else(|| error!(MixError::Inventory))
    }
}
impl ErCustody {
    pub fn asset_index(&self, asset: Pubkey) -> Result<usize> {
        self.assets.iter().position(|a| a.asset == asset)
            .ok_or_else(|| error!(MixError::Inventory))
    }
    pub fn asset_index_or_insert(&mut self, asset: Pubkey) -> Result<usize> {
        require!(asset != Pubkey::default(), MixError::Binding);
        if let Some(index) = self.assets.iter().position(|a| a.asset == asset) { return Ok(index); }
        let empty = self.assets.iter().position(|a| a.asset == Pubkey::default());
        #[cfg(feature="mainnet")]
        let empty = empty.or_else(|| self.assets.iter().position(|a|
            a.available == 0 && a.asset != crate::stake_backing::core_asset()));
        let index = empty.ok_or(MixError::Inventory)?;
        #[cfg(feature="mainnet")]
        {
            self.generation = self.generation.checked_add(1).ok_or(MixError::Arithmetic)?;
            self.assets[index] = CustodyAsset {asset,generation:self.generation,..CustodyAsset::default()};
        }
        #[cfg(not(feature="mainnet"))]
        { self.assets[index].asset = asset; }
        Ok(index)
    }
}

impl ErQuote {
    pub fn fresh(&self, now: i64) -> Result<()> {
        require!(self.source_slot > 0 && self.publish_time > 0
            && self.publish_time <= now.saturating_add(2)
            && now.saturating_sub(self.publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
        #[cfg(feature="mainnet")]
        if !self.is_quote && self.asset != sol_collateral_asset() {
            require!(now.saturating_sub(self.mainnet.conversion_publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
        }
        Ok(())
    }
    pub fn value(&self, amount: u64, now: i64) -> Result<u64> {
        self.fresh(now)?;
        if self.is_quote { return Ok(amount); }
        if self.asset == sol_collateral_asset() {
            require!(self.base_reserve == 0 && self.fee_bps == [0; 3], MixError::Quote);
            return sol_collateral_value(amount, self.quote_reserve);
        }
        #[cfg(feature="mainnet")]
        { self.mainnet.value(self.base_reserve, self.quote_reserve, self.fee_bps, self.publish_time, amount) }
        #[cfg(not(feature="mainnet"))]
        { pump::sell_from_reserves(amount, self.base_reserve, self.quote_reserve, self.fee_bps) }
    }
    pub fn publish(&mut self, base: u64, quote: u64, fees: [u16; 3], timestamp: i64, slot: u64, now: i64) -> Result<()> {
        require!(slot > self.source_slot && timestamp >= self.publish_time, MixError::Replay);
        // A durable publisher outbox may replay an older observed reserve dip.
        // Open/close still require fresh quotes; publishing never refreshes time.
        require!(timestamp > 0 && timestamp <= now.saturating_add(2), MixError::Stale);
        require!(fees.iter().map(|n| *n as u64).sum::<u64>() < 10_000, MixError::Quote);
        self.base_reserve = base;
        self.quote_reserve = quote;
        self.fee_bps = fees;
        self.publish_time = timestamp;
        self.source_slot = slot;
        Ok(())
    }
}

pub fn sol_collateral_asset() -> Pubkey {
    Pubkey::find_program_address(&[b"asset", anchor_spl::token::spl_token::native_mint::ID.as_ref()], &crate::ID).0
}
pub fn usd_collateral_asset(mint:Pubkey)->Pubkey {
    Pubkey::find_program_address(&[b"asset",mint.as_ref()],&crate::ID).0
}
/// Lamports * USD price (12 decimals) -> USD (6 decimals), rounded down.
pub fn sol_collateral_value(amount: u64, price: u64) -> Result<u64> {
    require!(price > 0, MixError::Quote);
    math::div(amount as u128, price as u128, 1_000_000_000_000_000)
}
/// Reuses the existing oracle sampler. For SOL only, the existing sample fields
/// encode base=0, quote=oracle price, fees=0; no account reallocation is needed.
pub fn sample_sol_collateral(quote: &mut ErQuote, history: &mut CollateralHistory, price: oracle::OraclePrice, paused: bool) -> Result<()> {
    require!(quote.asset == sol_collateral_asset() && !quote.is_quote
        && history.asset == quote.asset && history.config == quote.config && !history.is_quote, MixError::Binding);
    if quote.base_reserve != 0 || quote.source_slot == 0 {
        // Deployment switches price domains only with entries paused and all
        // previous positions drained. A normal sample can never reset history.
        require!(paused, MixError::Paused);
        quote.source_slot = 0; quote.publish_time = 0;
        history.sequence = 0; history.source_slot = 0; history.publish_time = 0;
        history.last_gap = 0; history.last_gap_start = 0;
        history.samples.fill(crate::CollateralSample::default());
    }
    if price.posted_slot <= quote.source_slot { return Ok(()); }
    quote.publish(0, price.price, [0; 3], price.publish_time, price.posted_slot, price.publish_time)?;
    history.record(quote)
}

impl ErMarket {
    pub fn fresh(&self, now: i64) -> Result<()> {
        require!(self.price > 0 && self.publish_time > 0 && self.publish_time <= now.saturating_add(2)
            && now.saturating_sub(self.publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
        Ok(())
    }
    /// Same source update is an idempotent no-op. Price repetitions with a new
    /// source slot still enter the ring; source time is never replaced by now.
    pub fn sample(&mut self, price: oracle::OraclePrice) -> Result<()> {
        require!(price.posted_slot >= self.source_slot && price.publish_time >= self.publish_time, MixError::Replay);
        if price.posted_slot == self.source_slot {
            require!(price.price == self.price && price.publish_time == self.publish_time, MixError::Replay);
            return Ok(());
        }
        let sequence = self.sequence.checked_add(1).ok_or(MixError::Arithmetic)?;
        self.price = price.price;
        self.publish_time = price.publish_time;
        self.source_slot = price.posted_slot;
        self.sequence = sequence;
        self.prices[(sequence % 64) as usize] = price.price;
        Ok(())
    }
    pub fn crossed(&self, position: &ErSlot) -> bool {
        if position.bust_observed || math::crossed(self.price, position.bust, position.long) {
            return true;
        }
        if position.cursor >= self.sequence { return false; }
        let first = position.cursor.saturating_add(1).max(self.sequence.saturating_sub(63)).max(1);
        (first..=self.sequence).any(|sequence|
            math::crossed(self.prices[(sequence % 64) as usize], position.bust, position.long))
    }
}

#[derive(Clone, Copy)]
pub struct OpenRequest {
    pub slot: u8, pub nonce: u64, pub market: u16, pub amount: u64,
    pub leverage: u16, pub long: bool, pub limit_price: u64,
    pub min_value: u64, pub max_value: u64, pub expires: i64,
}

fn valid_expiry(expires: i64, now: i64) -> Result<()> {
    require!(expires >= now && expires <= now.saturating_add(120), MixError::QuoteExpired);
    Ok(())
}
fn free_slot(trader: &ErTrader, slot: u8, nonce: u64) -> Result<usize> {
    let index = slot as usize;
    require!(index < ER_SLOTS && trader.slots[index].state == ER_FREE, MixError::Closed);
    require!(nonce == trader.next_nonce && nonce > 0, MixError::Replay);
    Ok(index)
}

pub fn open_slot(trader: &mut ErTrader, engine: &mut ErRisk, quote: &ErQuote, request: OpenRequest, now: i64) -> Result<()> {
    valid_expiry(request.expires, now)?;
    let slot = free_slot(trader, request.slot, request.nonce)?;
    require!((request.market as usize) < ER_MARKETS, MixError::Binding);
    require!(request.amount > 0 && request.amount <= 1_000_000_000_000_000, MixError::Amount);
    require!(shared(trader.config, quote.config) && engine.config == trader.config, MixError::Binding);
    let asset_index = trader.asset_index(quote.asset)?;
    require!(trader.assets[asset_index].available >= request.amount, MixError::Inventory);
    let market = &engine.markets[request.market as usize];
    market.fresh(now)?;
    require!(if request.long { market.price <= request.limit_price } else { market.price >= request.limit_price }, MixError::PriceLimit);
    let value=quote.value(request.amount,now)?;
    require!(value >= request.min_value && value <= request.max_value, MixError::Exposure);
    let notional = risk::check_entry(value, request.leverage, trader.notional)?;
    let old_oi = if request.long { market.long_oi } else { market.short_oi };
    let new_oi = old_oi.checked_add(notional).ok_or(MixError::Arithmetic)?;
    require!(new_oi <= if request.long { market.long_cap } else { market.short_cap }, MixError::Exposure);
    let bust = math::bust(market.price, request.leverage, request.long)?;
    let next_nonce = trader.next_nonce.checked_add(1).ok_or(MixError::Arithmetic)?;
    let new_notional = trader.notional.checked_add(notional).ok_or(MixError::Arithmetic)?;
    trader.slots[slot] = ErSlot { state: ER_OPEN, kind: ER_TRADE, nonce: request.nonce,
        asset: quote.asset, market: request.market, margin: request.amount, entry_value: value,
        entry: market.price, bust, leverage: request.leverage, long: request.long,
        opened_at: now, opened_source_slot: quote.source_slot, cursor: market.sequence,
        ..ErSlot::default() };
    trader.next_nonce = next_nonce;
    trader.notional = new_notional;
    trader.assets[asset_index].available -= request.amount;
    let market = &mut engine.markets[request.market as usize];
    if request.long { market.long_oi = new_oi; } else { market.short_oi = new_oi; }
    Ok(())
}

/// A missed ring cannot erase an earlier crossing. Slots with a gap and no
/// provable crossing remain frozen for closing; other slots still advance.
pub fn observe_slots(trader: &mut ErTrader, engine: &ErRisk) -> Result<()> {
    require_keys_eq!(trader.config, engine.config, MixError::Binding);
    for slot in trader.slots.iter_mut() {
        if slot.state != ER_OPEN || slot.bust_observed { continue; }
        let market = engine.markets.get(slot.market as usize).ok_or(MixError::Binding)?;
        if market.sequence < slot.cursor { continue; }
        if market.crossed(slot) {
            slot.bust_observed = true;
            slot.cursor = market.sequence;
        } else if market.sequence - slot.cursor <= 64 {
            slot.cursor = market.sequence;
        }
    }
    Ok(())
}

pub const MIN_MANUAL_HOLD_SECONDS: i64 = 5;

fn require_manual_hold(opened_at: i64, now: i64) -> Result<()> {
    require!(now.checked_sub(opened_at).is_some_and(|age| age >= MIN_MANUAL_HOLD_SECONDS), MixError::MinimumHold);
    Ok(())
}

pub fn close_slot(trader: &mut ErTrader, engine: &mut ErRisk, quote: &ErQuote, slot: u8, nonce: u64, expires: i64, caller: Pubkey, now: i64) -> Result<()> {
    close_slot_authorized(trader, engine, quote, slot, nonce, expires, caller, now, false)
}
fn close_slot_authorized(trader: &mut ErTrader, engine: &mut ErRisk, quote: &ErQuote, slot: u8, nonce: u64, expires: i64, caller: Pubkey, now: i64, forced: bool) -> Result<()> {
    valid_expiry(expires, now)?;
    require!(trader.config == engine.config && shared(trader.config, quote.config), MixError::Binding);
    let position = trader.slots.get_mut(slot as usize).ok_or(MixError::Binding)?;
    require!(position.state == ER_OPEN && position.kind == ER_TRADE, MixError::Closed);
    require!(position.nonce == nonce, MixError::Replay);
    require_keys_eq!(position.asset, quote.asset, MixError::Binding);
    let market = engine.markets.get(position.market as usize).ok_or(MixError::Binding)?;
    market.fresh(now)?;
    let liquidated = market.crossed(position);
    require!(liquidated || (market.sequence >= position.cursor && market.sequence - position.cursor <= 64), MixError::History);
    require!(liquidated || caller == trader.owner || forced, MixError::Unauthorized);
    // Owner closes (including profit-waiver recovery) must wait. Liquidation
    // and insufficient-backing protection must remain available immediately.
    if !liquidated && !forced { require_manual_hold(position.opened_at, now)?; }
    // No collateral swap or payout happens here. Freeze the fresh market exit
    // immediately; the guard and base settlement verify collateral separately.
    let wins = !liquidated && if position.long { market.price > position.entry } else { market.price < position.entry };
    let next_seq = if wins { engine.next_close_seq.checked_add(1).ok_or(MixError::Arithmetic)? } else { engine.next_close_seq };
    position.state = ER_PENDING;
    position.exit = market.price;
    position.closed_at = now;
    position.close_seq = if wins { engine.next_close_seq } else { 0 };
    position.liquidated = liquidated;
    position.bust_observed |= liquidated;
    engine.next_close_seq = next_seq;
    // OI and collateral remain reserved until the base receipt acknowledges the close.
    Ok(())
}

/// An owner may surrender the entire margin when missing price history makes
/// an ordinary close unknowable. This never reconstructs a price or releases
/// collateral: the committed intent must still execute the real base loss swap.
pub fn forfeit_history_gap_slot(trader: &mut ErTrader, engine: &ErRisk, slot: u8, nonce: u64,
    expires: i64, owner: Pubkey, exit: oracle::OraclePrice, now: i64) -> Result<()> {
    valid_expiry(expires, now)?;
    require_keys_eq!(trader.owner, owner, MixError::Unauthorized);
    require_keys_eq!(trader.config, engine.config, MixError::Binding);
    let position = trader.slots.get_mut(slot as usize).ok_or(MixError::Binding)?;
    require!(position.state == ER_OPEN && position.kind == ER_TRADE, MixError::Closed);
    require!(position.nonce == nonce && nonce > 0, MixError::Replay);
    let market = engine.markets.get(position.market as usize).ok_or(MixError::Binding)?;
    require!(market.sequence > position.cursor && market.sequence - position.cursor > 64
        && !market.crossed(position), MixError::History);
    require!(exit.price > 0 && exit.posted_slot >= market.source_slot
        && exit.publish_time >= market.publish_time, MixError::Replay);
    require!(exit.publish_time > 0 && exit.publish_time <= now.saturating_add(2)
        && now.saturating_sub(exit.publish_time) <= oracle::MAX_ORACLE_AGE, MixError::Stale);
    require_manual_hold(position.opened_at, now)?;
    position.state = ER_PENDING;
    position.exit = exit.price;
    position.closed_at = now;
    position.close_seq = 0;
    position.liquidated = true;
    position.bust_observed = true;
    // Preserve margin, cursor, OI and credits until the matching base receipt.
    Ok(())
}

#[event]
pub struct ErHistoryGapForfeited {
    pub owner: Pubkey,
    pub slot: u8,
    pub nonce: u64,
    pub margin: u64,
    pub exit: u64,
    pub cursor: u64,
    pub sequence: u64,
}

pub fn withdraw_slot(trader: &mut ErTrader, engine: &mut ErRisk, asset: Pubkey, slot: u8, nonce: u64, amount: u64, expires: i64, now: i64) -> Result<()> {
    valid_expiry(expires, now)?;
    require_keys_eq!(trader.config, engine.config, MixError::Binding);
    let index = free_slot(trader, slot, nonce)?;
    let asset_index = trader.asset_index(asset)?;
    require!(amount > 0 && trader.assets[asset_index].available >= amount, MixError::Inventory);
    let next_nonce = trader.next_nonce.checked_add(1).ok_or(MixError::Arithmetic)?;
    trader.slots[index] = ErSlot { state: ER_PENDING, kind: ER_WITHDRAW, nonce, asset,
        margin: amount, closed_at: now, close_seq: 0, ..ErSlot::default() };
    trader.assets[asset_index].available -= amount;
    trader.next_nonce = next_nonce;
    Ok(())
}

/// Cumulative credits allow independent base deposits and settlement receipts.
/// Repeated or older base snapshots never duplicate credits or release a slot.
pub fn sync_custody(trader: &mut ErTrader, engine: &mut ErRisk, custody: &ErCustody) -> Result<()> {
    require!(trader.owner == custody.owner && trader.config == custody.config
        && trader.pool == custody.pool && engine.config == trader.config, MixError::Binding);
    // Retired record generations must never be resurrected by an older clone.
    #[cfg(feature="mainnet")]
    if custody.generation < trader.custody_generation { return Ok(()); }
    // A settlement must debit the stake before its receipt releases a slot.
    trader.core_stake.sync(custody.core_coverage)?;
    for i in 0..2 { trader.auxiliary_stakes[i].sync(custody.auxiliary_coverage[i])?; }
    acknowledge_receipts(trader, engine, custody)?;
    #[cfg(feature="mainnet")]
    {
        for (index,target) in trader.assets.iter_mut().enumerate() {
            let matches=|s:&CustodyAsset|s.asset==target.asset && s.generation==target.generation;
            // The normal ledger keeps base/ER records in the same index.
            // Check that index first; retain the full search for legacy order.
            if target.asset == Pubkey::default() || matches(&custody.assets[index])
                || custody.assets.iter().any(matches) {continue;}
            // Base can retire only zero custody: no available ER balance,
            // unreconciled reservation or backing may be silently discarded.
            require!(custody.generation>trader.custody_generation && target.available==0
                && target.asset!=crate::stake_backing::core_asset()
                && trader.slots.iter().all(|s|s.state==ER_FREE || s.asset!=target.asset),MixError::Inventory);
            *target=ErBalance::default();
        }
    }
    for (source_index,source) in custody.assets.iter().enumerate() {
        if source.asset == Pubkey::default() { continue; }
        let index = match (trader.assets[source_index].asset==source.asset).then_some(source_index)
            .or_else(||trader.assets.iter().position(|a| a.asset == source.asset)) {
            Some(index) => index,
            None => trader.assets.iter().position(|a| a.asset == Pubkey::default()).ok_or(MixError::Inventory)?,
        };
        let target = &mut trader.assets[index];
        #[cfg(feature="mainnet")]
        {
            require!(source.generation<=custody.generation &&
                (target.asset==Pubkey::default() || target.generation==source.generation),MixError::Binding);
            target.generation=source.generation;
        }
        // An older cloned receipt is harmless: keep each monotonic cursor.
        let deposits = source.deposited_credits.saturating_sub(target.credited_deposits);
        let returns = source.returned_credits.saturating_sub(target.credited_returns);
        let available = target.available.checked_add(deposits).and_then(|n| n.checked_add(returns)).ok_or(MixError::Arithmetic)?;
        target.asset = source.asset;
        target.available = available;
        target.credited_deposits = target.credited_deposits.max(source.deposited_credits);
        target.credited_returns = target.credited_returns.max(source.returned_credits);
    }
    #[cfg(feature="mainnet")]
    { trader.custody_generation=custody.generation; }
    Ok(())
}

fn acknowledge_receipts(trader: &mut ErTrader, engine: &mut ErRisk, custody: &ErCustody) -> Result<()> {
    for (slot, receipt) in trader.slots.iter_mut().zip(custody.receipts.iter()) {
        if slot.state != ER_PENDING || !receipt.settled || receipt.nonce != slot.nonce || receipt.kind != slot.kind { continue; }
        if slot.kind == ER_TRADE {
            require!(receipt.position != Pubkey::default(), MixError::Binding);
            let notional = slot.entry_value.checked_mul(slot.leverage as u64).ok_or(MixError::Arithmetic)?;
            let market = engine.markets.get_mut(slot.market as usize).ok_or(MixError::Binding)?;
            let oi = if slot.long { &mut market.long_oi } else { &mut market.short_oi };
            *oi = oi.checked_sub(notional).ok_or(MixError::Arithmetic)?;
            trader.notional = trader.notional.checked_sub(notional).ok_or(MixError::Arithmetic)?;
        }
        *slot = ErSlot::default();
    }
    Ok(())
}

pub fn initialize_risk(ctx: Context<InitializeErRisk>) -> Result<()> {
    **ctx.accounts.er_risk = ErRisk { config: ctx.accounts.config.key(),
        cap_refreshed_at: Clock::get()?.unix_timestamp, bump: ctx.bumps.er_risk, ..ErRisk::default() };
    Ok(())
}
pub fn initialize_quote(ctx: Context<InitializeErQuote>) -> Result<()> {
    *ctx.accounts.er_quote = ErQuote { config: ctx.accounts.config.key(), asset: ctx.accounts.asset.key(),
        is_quote: pump::is_quote(&ctx.accounts.asset), bump: ctx.bumps.er_quote, ..ErQuote::default() };
    Ok(())
}
pub fn initialize_trader(ctx: Context<InitializeErTrader>) -> Result<()> {
    **ctx.accounts.er_trader = ErTrader { owner: ctx.accounts.owner.key(), config: ctx.accounts.config.key(),
        pool: ctx.accounts.pool.key(), next_nonce: 1, bump: ctx.bumps.er_trader, ..ErTrader::default() };
    Ok(())
}
pub fn initialize_fee_payer(ctx: Context<InitializeErFeePayer>, lamports: u64) -> Result<()> {
    *ctx.accounts.er_fee_payer = ErFeePayer { config: ctx.accounts.config.key(), bump: ctx.bumps.er_fee_payer };
    if lamports > 0 {
        anchor_lang::system_program::transfer(CpiContext::new(ctx.accounts.system_program.key(),
            anchor_lang::system_program::Transfer { from: ctx.accounts.admin.to_account_info(), to: ctx.accounts.er_fee_payer.to_account_info() }), lamports)?;
    }
    Ok(())
}
pub fn publish_quote(ctx: Context<PublishErQuote>, base_reserve: u64, quote_reserve: u64, fee_bps: [u16; 3], publish_time: i64, source_slot: u64) -> Result<()> {
    // SOL collateral is priced exclusively by the verified MagicBlock account.
    require!(ctx.accounts.er_quote.asset != sol_collateral_asset(), MixError::Binding);
    // Mainnet reserves need an explicitly paired SOL/USD observation. The
    // legacy instruction is valid only for the fixed-value USDC heartbeat.
    #[cfg(feature="mainnet")]
    require!(ctx.accounts.er_quote.is_quote, MixError::Binding);
    ctx.accounts.er_quote.publish(base_reserve, quote_reserve, fee_bps, publish_time, source_slot, Clock::get()?.unix_timestamp)?;
    ctx.accounts.history.record(&ctx.accounts.er_quote)
}
pub fn sample(ctx: Context<SampleErPrices>) -> Result<()> {
    let clock = Clock::get()?;
    let accounts = [&ctx.accounts.btc_oracle, &ctx.accounts.zec_oracle, &ctx.accounts.sol_oracle, &ctx.accounts.eth_oracle];
    // Independent feeds: one stale/temporarily missing market cannot stop the others.
    for (index, account) in accounts.iter().enumerate() {
        if let Ok(price) = oracle::read_oracle(&account.to_account_info(), index as u16, clock.unix_timestamp, clock.slot) {
            let market = &mut ctx.accounts.er_risk.markets[index];
            if price.posted_slot > market.source_slot && price.publish_time >= market.publish_time { market.sample(price)?; }
            if index == 2 {
                if let (Some(quote), Some(history)) = (ctx.accounts.sol_quote.as_mut(), ctx.accounts.sol_history.as_mut()) {
                    sample_sol_collateral(quote, history, price, ctx.accounts.config.paused)?;
                }
            }
        }
    }
    if clock.unix_timestamp.saturating_sub(ctx.accounts.er_risk.cap_refreshed_at) >= risk::REFRESH_SECONDS {
        for market in ctx.accounts.er_risk.markets.iter_mut() {
            market.long_cap = risk::refreshed_cap(market.long_oi, market.long_cap)?;
            market.short_cap = risk::refreshed_cap(market.short_oi, market.short_cap)?;
        }
        ctx.accounts.er_risk.cap_refreshed_at = clock.unix_timestamp;
    }
    Ok(())
}
pub fn sync(ctx: Context<SyncErTrader>) -> Result<()> {
    sync_custody(&mut ctx.accounts.er_trader, &mut ctx.accounts.er_risk, &ctx.accounts.er_custody)
}
pub fn change_core_stake(ctx: Context<ChangeErCoreStake>, amount: u64, unstake: bool) -> Result<()> {
    require!(unstake || !ctx.accounts.config.paused, MixError::Paused);
    // Each venue stakes its own pool settlement token.
    let backing = Backing::new(ctx.accounts.pool.settlement, ctx.accounts.config.quote_mint);
    crate::stake_backing::change_stake(&mut ctx.accounts.er_trader, amount, unstake, &backing)
}
pub fn change_backing(ctx: Context<ChangeErCoreStake>, asset: Pubkey, amount: u64, unstake: bool) -> Result<()> {
    require!(unstake || !ctx.accounts.config.paused, MixError::Paused);
    let backing = Backing::new(ctx.accounts.pool.settlement, ctx.accounts.config.quote_mint);
    crate::stake_backing::change_backing(&mut ctx.accounts.er_trader, asset, amount, unstake, &backing)
}
pub fn open(ctx: Context<OpenErPosition>, request: OpenRequest) -> Result<()> {
    require!(!ctx.accounts.config.paused, MixError::Paused);
    require!(ctx.accounts.asset.collateral_enabled, MixError::Binding);
    let now = Clock::get()?.unix_timestamp;
    // No caller-supplied pool value. Old clients that omit the observation can
    // use only the conservative $1M floor, never bypass the dynamic cap.
    let pool_usd=match ctx.accounts.pool_exposure.as_ref() {Some(p)=>p.value(now)?,None=>0};
    risk::check_pool_entry(ctx.accounts.er_quote.value(request.amount,now)?,request.leverage,
        ctx.accounts.er_trader.notional,pool_usd)?;
    let backing = Backing::new(ctx.accounts.pool.settlement, ctx.accounts.config.quote_mint);
    if backing.requires(ctx.accounts.asset.key()) {
        crate::stake_backing::check_entry_mixed(&ctx.accounts.er_trader, &ctx.accounts.core_quote,
            [ctx.accounts.sol_backing_quote.as_deref().map(|q| &**q),ctx.accounts.usd_backing_quote.as_deref().map(|q| &**q)],
            ctx.accounts.er_quote.value(request.amount,now)?, now, &backing)?;
    }
    open_slot(&mut ctx.accounts.er_trader, &mut ctx.accounts.er_risk, &ctx.accounts.er_quote, request, now)
}
pub fn observe(ctx: Context<ObserveErTrader>) -> Result<()> {
    observe_slots(&mut ctx.accounts.er_trader, &ctx.accounts.er_risk)
}
pub fn close(ctx: Context<CloseErPosition>, slot: u8, nonce: u64, expires: i64) -> Result<()> {
    close_slot(&mut ctx.accounts.er_trader, &mut ctx.accounts.er_risk, &ctx.accounts.er_quote,
        slot, nonce, expires, ctx.accounts.caller.key(), Clock::get()?.unix_timestamp)
}
pub fn force_core_close(ctx: Context<CloseErPosition>, slot: u8, nonce: u64, expires: i64) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let backing = Backing::new(ctx.accounts.pool.settlement, ctx.accounts.config.quote_mint);
    crate::stake_backing::check_forced_close_mixed(&ctx.accounts.er_trader, &ctx.accounts.core_quote,
        [ctx.accounts.sol_backing_quote.as_deref().map(|q| &**q),ctx.accounts.usd_backing_quote.as_deref().map(|q| &**q)], slot, now, &backing)?;
    close_slot_authorized(&mut ctx.accounts.er_trader, &mut ctx.accounts.er_risk, &ctx.accounts.er_quote,
        slot, nonce, expires, ctx.accounts.caller.key(), now, true)
}
/// Owner-authorized recovery only: if monitoring history is unknowable, the
/// owner may waive synthetic profit and recover collateral through normal
/// settlement. Keepers cannot impose this haircut, and losses are not forgiven.
pub fn waive_unverified_profit(ctx: Context<CloseErPosition>, slot:u8, nonce:u64, expires:i64) -> Result<()> {
    let now=Clock::get()?.unix_timestamp;
    valid_expiry(expires,now)?;
    require_keys_eq!(ctx.accounts.caller.key(),ctx.accounts.er_trader.owner,MixError::Unauthorized);
    ctx.accounts.history.matches(&ctx.accounts.er_quote)?;
    ctx.accounts.guard.observe(&ctx.accounts.er_trader,&ctx.accounts.history)?;
    let position=*ctx.accounts.er_trader.slots.get(slot as usize).ok_or(MixError::Binding)?;
    require!(position.nonce==nonce && position.kind==ER_TRADE && [ER_OPEN,ER_PENDING].contains(&position.state),MixError::Replay);
    let floor=ctx.accounts.guard.floors.get(slot as usize).ok_or(MixError::Binding)?;
    require!(floor.nonce==nonce && floor.incomplete,MixError::History);
    if position.state==ER_OPEN {
        close_slot(&mut ctx.accounts.er_trader,&mut ctx.accounts.er_risk,&ctx.accounts.er_quote,
            slot,nonce,expires,ctx.accounts.caller.key(),now)?;
    }
    let floor=&mut ctx.accounts.guard.floors[slot as usize];
    floor.value=0;floor.incomplete=false;
    floor.covered_at=ctx.accounts.er_trader.slots[slot as usize].closed_at.checked_add(1).ok_or(MixError::Arithmetic)?;
    ctx.accounts.guard.freeze(&ctx.accounts.er_trader,slot)?;
    emit!(CollateralProfitWaived {owner:ctx.accounts.er_trader.owner,slot,nonce});
    Ok(())
}
#[event]
pub struct CollateralProfitWaived {pub owner:Pubkey,pub slot:u8,pub nonce:u64}
pub fn forfeit_history_gap(ctx: Context<ForfeitErHistoryGap>, slot: u8, nonce: u64, expires: i64) -> Result<()> {
    require!(cfg!(feature = "devnet"), MixError::ProductionDisabled);
    let clock = Clock::get()?;
    let market = ctx.accounts.er_trader.slots.get(slot as usize).ok_or(MixError::Binding)?.market;
    let exit = oracle::read_oracle(&ctx.accounts.oracle.to_account_info(), market, clock.unix_timestamp, clock.slot)?;
    forfeit_history_gap_slot(&mut ctx.accounts.er_trader, &ctx.accounts.er_risk,
        slot, nonce, expires, ctx.accounts.owner.key(), exit, clock.unix_timestamp)?;
    ctx.accounts.guard.freeze(&ctx.accounts.er_trader,slot)?;
    let position = ctx.accounts.er_trader.slots[slot as usize];
    emit!(ErHistoryGapForfeited { owner: ctx.accounts.owner.key(), slot, nonce, margin: position.margin,
        exit: position.exit, cursor: position.cursor, sequence: ctx.accounts.er_risk.markets[market as usize].sequence });
    Ok(())
}
pub fn withdraw(ctx: Context<WithdrawErTrader>, slot: u8, nonce: u64, amount: u64, expires: i64) -> Result<()> {
    withdraw_slot(&mut ctx.accounts.er_trader, &mut ctx.accounts.er_risk, ctx.accounts.asset.key(),
        slot, nonce, amount, expires, Clock::get()?.unix_timestamp)
}
pub fn delegate_trader(ctx: Context<DelegateErTrader>) -> Result<()> {
    // An owner must not redelegate a returned legacy wallet before its atomic
    // capacity migration. Typed ER instructions require the current layout.
    require!(ctx.accounts.er_trader.data_len()==8+ErTrader::INIT_SPACE,MixError::Binding);
    #[cfg(feature="mainnet")]
    require!(!ctx.accounts.config.paused || ctx.accounts.payer.key()==ctx.accounts.config.admin,MixError::Paused);
    let owner = ctx.accounts.owner.key();
    let n = ns(ctx.accounts.config.key());
    let seeds = crate::venue::ns_seeds(b"er-trader", &n, &[owner.as_ref()]);
    ctx.accounts.delegate_er_trader(&ctx.accounts.payer, &seeds, delegation_config())?;
    Ok(())
}
pub fn delegate_risk(ctx: Context<DelegateErRisk>) -> Result<()> {
    let n = ns(ctx.accounts.config.key());
    let seeds = crate::venue::ns_seeds(b"er-risk", &n, &[]);
    ctx.accounts.delegate_er_risk(&ctx.accounts.admin, &seeds, delegation_config())?;
    Ok(())
}
pub fn delegate_quote(ctx: Context<DelegateErQuote>) -> Result<()> {
    let asset = ctx.accounts.asset.key();
    ctx.accounts.delegate_er_quote(&ctx.accounts.admin, &[b"er-quote", asset.as_ref()], delegation_config())?;
    Ok(())
}
fn delegation_config() -> DelegateConfig {
    DelegateConfig { validator: Some(ER_VALIDATOR), commit_frequency_ms: u32::MAX }
}
pub fn delegate_fee_payer(ctx: Context<DelegateErFeePayer>) -> Result<()> {
    ctx.accounts.delegate_er_fee_payer(&ctx.accounts.admin, &[b"er-fee-payer"], delegation_config())?;
    Ok(())
}
pub fn commit_trader(ctx: Context<CommitErTrader>) -> Result<()> {
    // The trader includes stake debits and frozen exits. Collateral-history
    // guards are obsolete for opening/closing quotes and cannot block commit.
    ctx.accounts.er_trader.exit(&crate::ID)?;
    MagicIntentBundleBuilder::new(ctx.accounts.er_fee_payer.to_account_info(), ctx.accounts.magic_context.to_account_info(), ctx.accounts.magic_program.to_account_info())
        .magic_fee_vault(ctx.accounts.magic_fee_vault.to_account_info())
        .commit(&[ctx.accounts.er_trader.to_account_info()])
        .build_and_invoke_signed(&[&[b"er-fee-payer", &[ctx.accounts.er_fee_payer.bump]]])?;
    Ok(())
}

/// Browser-wallet commits pay from excess SOL on this user's delegated trader,
/// never from the platform-wide commit reserve. The prepaid service funds that
/// account through the normal delegated-lamports transfer before authorizing opens.
pub fn commit_prepaid_trader(ctx:Context<CommitPrepaidErTrader>)->Result<()> {
    let a=&ctx.accounts;
    a.er_trader.exit(&crate::ID)?;
    let (n,bump)=(ns(a.er_trader.config),[a.er_trader.bump]);
    let seeds=crate::venue::ns_seeds(b"er-trader",&n,&[a.er_trader.owner.as_ref(),&bump]);
    MagicIntentBundleBuilder::new(a.er_trader.to_account_info(),a.magic_context.to_account_info(),a.magic_program.to_account_info())
        .magic_fee_vault(a.magic_fee_vault.to_account_info())
        .commit(&[a.er_trader.to_account_info()])
        .build_and_invoke_signed(&[&seeds])?;
    Ok(())
}

/// An idle browser session can return to Solana so its owner can withdraw
/// unspent commit credit. Only that trader pays the undelegation/commit charge.
pub fn release_prepaid_trader(ctx:Context<ReleasePrepaidErTrader>)->Result<()> {
    let a=&ctx.accounts;
    crate::recovery::idle(&a.er_trader)?;
    require!(a.er_trader.assets.iter().all(|asset|asset.available==0),MixError::Inventory);
    a.er_trader.exit(&crate::ID)?;
    let (n,bump,owner)=(ns(a.er_trader.config),[a.er_trader.bump],a.owner.key());
    let seeds=crate::venue::ns_seeds(b"er-trader",&n,&[owner.as_ref(),&bump]);
    MagicIntentBundleBuilder::new(a.er_trader.to_account_info(),a.magic_context.to_account_info(),a.magic_program.to_account_info())
        .magic_fee_vault(a.magic_fee_vault.to_account_info())
        .commit_and_undelegate(&[a.er_trader.to_account_info()])
        .build_and_invoke_signed(&[&seeds])?;
    Ok(())
}
pub fn withdraw_prepaid_gas(ctx:Context<WithdrawPrepaidGas>)->Result<()> {
    let a=&ctx.accounts;
    crate::recovery::idle(&a.er_trader)?;
    require!(a.er_trader.assets.iter().all(|asset|asset.available==0),MixError::Inventory);
    let trader=a.er_trader.to_account_info();
    let rent=Rent::get()?.minimum_balance(trader.data_len());
    let amount=trader.lamports().checked_sub(rent).ok_or(MixError::Amount)?;
    require!(amount>0,MixError::Amount);
    let after=a.owner.lamports().checked_add(amount).ok_or(MixError::Arithmetic)?;
    **trader.try_borrow_mut_lamports()?=rent;
    **a.owner.try_borrow_mut_lamports()?=after;
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeErRisk<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    // Any program config: the root, or a venue created by create_venue.
    #[account(has_one=admin)] pub config: Account<'info, Config>,
    #[account(init,payer=admin,space=8+ErRisk::INIT_SPACE,seeds=[b"er-risk",vns!(config.key())],bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct InitializeErQuote<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)] pub config: Account<'info, Config>,
    #[account(seeds=[b"asset",asset.mint.as_ref()],bump=asset.bump)]
    pub asset: Account<'info, Asset>,
    #[account(init,payer=admin,space=8+ErQuote::INIT_SPACE,seeds=[b"er-quote",asset.key().as_ref()],bump)] pub er_quote: Account<'info, ErQuote>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct InitializeErTrader<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)] pub pool: Account<'info, Pool>,
    #[account(init,payer=payer,space=8+ErTrader::INIT_SPACE,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(seeds=[b"er-custody",vns!(config.key()),owner.key().as_ref()],bump=er_custody.bump,has_one=owner,has_one=config,has_one=pool)] pub er_custody: Box<Account<'info, ErCustody>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct PublishErQuote<'info> {
    pub publisher: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=publisher)] pub config: Account<'info, Config>,
    #[account(mut,seeds=[b"er-quote",er_quote.asset.as_ref()],bump=er_quote.bump,has_one=config)] pub er_quote: Account<'info, ErQuote>,
    #[account(mut,seeds=[b"er-collateral-history",er_quote.asset.as_ref()],bump=history.bump,has_one=config)] pub history: Account<'info,CollateralHistory>,
    pub execution: ErExecution<'info>,
}
#[derive(Accounts)]
pub struct SampleErPrices<'info> {
    pub publisher: Signer<'info>,
    #[account(has_one=publisher)] pub config: Account<'info, Config>,
    #[account(mut,seeds=[b"er-risk",vns!(config.key())],bump=er_risk.bump,has_one=config)] pub er_risk: Box<Account<'info, ErRisk>>,
    /// CHECK: Strict owner/PDA/discriminator/verification/source freshness in oracle::read_oracle.
    pub btc_oracle: UncheckedAccount<'info>,
    /// CHECK: Strict oracle validation in handler.
    pub zec_oracle: UncheckedAccount<'info>,
    /// CHECK: Strict oracle validation in handler.
    pub sol_oracle: UncheckedAccount<'info>,
    /// CHECK: Strict oracle validation in handler.
    pub eth_oracle: UncheckedAccount<'info>,
    pub execution: ErExecution<'info>,
    #[account(mut,seeds=[b"er-quote",sol_collateral_asset().as_ref()],bump=sol_quote.bump,has_one=config)]
    pub sol_quote: Option<Account<'info, ErQuote>>,
    #[account(mut,seeds=[b"er-collateral-history",sol_collateral_asset().as_ref()],bump=sol_history.bump,has_one=config)]
    pub sol_history: Option<Box<Account<'info, CollateralHistory>>>,
}
#[derive(Accounts)]
pub struct SyncErTrader<'info> {
    #[account(mut,seeds=[b"er-trader",vns!(er_trader.config),er_trader.owner.as_ref()],bump=er_trader.bump)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-risk",vns!(er_trader.config)],bump=er_risk.bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    #[account(seeds=[b"er-custody",vns!(er_trader.config),er_trader.owner.as_ref()],bump=er_custody.bump)] pub er_custody: Box<Account<'info, ErCustody>>,
    pub execution: ErExecution<'info>,
}
#[derive(Accounts)]
pub struct ChangeErCoreStake<'info> {
    pub owner: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)] pub pool: Account<'info, Pool>,
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump=er_trader.bump,has_one=owner,has_one=config,has_one=pool)]
    pub er_trader: Box<Account<'info, ErTrader>>,
    pub execution: ErExecution<'info>,
}
#[derive(Accounts)]
pub struct OpenErPosition<'info> {
    pub owner: Signer<'info>,
    pub config: Account<'info, Config>,
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump=er_trader.bump,has_one=owner,has_one=config)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-risk",vns!(config.key())],bump=er_risk.bump,has_one=config)] pub er_risk: Box<Account<'info, ErRisk>>,
    pub asset: Account<'info, Asset>,
    #[account(seeds=[b"er-quote",asset.key().as_ref()],bump=er_quote.bump,has_one=asset,constraint=shared(config.key(),er_quote.config) @ MixError::Binding)] pub er_quote: Account<'info, ErQuote>,
    #[account(seeds=[b"er-collateral-history",asset.key().as_ref()],bump=history.bump,has_one=asset,constraint=shared(config.key(),history.config) @ MixError::Binding)] pub history: Account<'info,CollateralHistory>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(config.key()),owner.key().as_ref()],bump=guard.bump,has_one=owner,has_one=config)] pub guard: Box<Account<'info,CollateralGuard>>,
    pub execution: ErExecution<'info>,
    #[account(seeds=[b"er-quote",pool.settlement.as_ref()],bump=core_quote.bump,constraint=shared(config.key(),core_quote.config) @ MixError::Binding)]
    pub core_quote: Box<Account<'info, ErQuote>>,
    // Admission co-signature protects prepaid settlement funding even when an
    // owner submits directly to ER instead of using the public HTTP endpoint.
    #[cfg(feature="mainnet")]
    #[account(address=config.publisher @ MixError::Unauthorized)]
    pub gas_authority: Signer<'info>,
    /// The venue pool; its settlement token is this venue's core stake.
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump,constraint=er_trader.pool==pool.key() @ MixError::Binding)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(seeds=[b"pool-exposure",pool.key().as_ref()],bump=pool_exposure.bump,
        constraint=pool_exposure.config==config.key() && pool_exposure.pool==pool.key() @ MixError::Binding)]
    pub pool_exposure: Option<Box<Account<'info,crate::pool_exposure::PoolExposure>>>,
    #[account(seeds=[b"er-quote",sol_collateral_asset().as_ref()],bump=sol_backing_quote.bump)]
    pub sol_backing_quote: Option<Box<Account<'info,ErQuote>>>,
    #[account(constraint=usd_backing_quote.key()==Pubkey::find_program_address(&[b"er-quote",usd_collateral_asset(config.quote_mint).as_ref()],&crate::ID).0 @ MixError::Binding)]
    pub usd_backing_quote: Option<Box<Account<'info,ErQuote>>>,
}
#[derive(Accounts)]
pub struct ObserveErTrader<'info> {
    #[account(mut,seeds=[b"er-trader",vns!(er_trader.config),er_trader.owner.as_ref()],bump=er_trader.bump)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(seeds=[b"er-risk",vns!(er_trader.config)],bump=er_risk.bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    pub execution: ErExecution<'info>,
}
#[derive(Accounts)]
pub struct CloseErPosition<'info> {
    pub caller: Signer<'info>,
    #[account(mut,seeds=[b"er-trader",vns!(er_trader.config),er_trader.owner.as_ref()],bump=er_trader.bump)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-risk",vns!(er_trader.config)],bump=er_risk.bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    #[account(seeds=[b"er-quote",er_quote.asset.as_ref()],bump=er_quote.bump)] pub er_quote: Account<'info, ErQuote>,
    #[account(seeds=[b"er-collateral-history",er_quote.asset.as_ref()],bump=history.bump)] pub history: Account<'info,CollateralHistory>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(er_trader.config),er_trader.owner.as_ref()],bump=guard.bump)] pub guard: Box<Account<'info,CollateralGuard>>,
    pub execution: ErExecution<'info>,
    #[account(seeds=[b"er-quote",pool.settlement.as_ref()],bump=core_quote.bump)]
    pub core_quote: Box<Account<'info, ErQuote>>,
    #[account(address=er_trader.config)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds=[b"pool",vns!(er_trader.config),&0u16.to_le_bytes()],bump=pool.bump,constraint=er_trader.pool==pool.key() @ MixError::Binding)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(seeds=[b"er-quote",sol_collateral_asset().as_ref()],bump=sol_backing_quote.bump)]
    pub sol_backing_quote: Option<Box<Account<'info,ErQuote>>>,
    #[account(constraint=usd_backing_quote.key()==Pubkey::find_program_address(&[b"er-quote",usd_collateral_asset(config.quote_mint).as_ref()],&crate::ID).0 @ MixError::Binding)]
    pub usd_backing_quote: Option<Box<Account<'info,ErQuote>>>,
}
#[derive(Accounts)]
pub struct ForfeitErHistoryGap<'info> {
    pub owner: Signer<'info>,
    #[account(mut,seeds=[b"er-trader",vns!(er_trader.config),owner.key().as_ref()],bump=er_trader.bump,has_one=owner)]
    pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(er_trader.config),er_trader.owner.as_ref()],bump=guard.bump)] pub guard: Box<Account<'info,CollateralGuard>>,
    #[account(seeds=[b"er-risk",vns!(er_trader.config)],bump=er_risk.bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    /// CHECK: Canonical native feed, owner, discriminator and freshness checked for the selected slot's market.
    pub oracle: UncheckedAccount<'info>,
    pub execution: ErExecution<'info>,
}
#[derive(Accounts)]
pub struct WithdrawErTrader<'info> {
    pub owner: Signer<'info>,
    #[account(mut,seeds=[b"er-trader",vns!(er_trader.config),owner.key().as_ref()],bump=er_trader.bump,has_one=owner)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-risk",vns!(er_trader.config)],bump=er_risk.bump)] pub er_risk: Box<Account<'info, ErRisk>>,
    pub asset: Account<'info, Asset>,
    pub execution: ErExecution<'info>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateErTrader<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    pub config: Account<'info, Config>,
    /// CHECK: Only this owner's canonical account can be delegated; DLP verifies current program ownership.
    #[account(mut,del,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump)] pub er_trader: UncheckedAccount<'info>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateErRisk<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(has_one=admin)] pub config: Account<'info, Config>,
    /// CHECK: Canonical engine account; DLP verifies current program ownership.
    #[account(mut,del,seeds=[b"er-risk",vns!(config.key())],bump)] pub er_risk: UncheckedAccount<'info>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateErQuote<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)] pub config: Account<'info, Config>,
    #[account(seeds=[b"asset",asset.mint.as_ref()],bump=asset.bump)]
    pub asset: Account<'info, Asset>,
    /// CHECK: Canonical quote account; DLP verifies current program ownership.
    #[account(mut,del,seeds=[b"er-quote",asset.key().as_ref()],bump)] pub er_quote: UncheckedAccount<'info>,
}
#[commit]
#[derive(Accounts)]
pub struct CommitErTrader<'info> {
    pub publisher: Signer<'info>,
    #[account(has_one=publisher)] pub config: Account<'info, Config>,
    // One shared commit fee payer, owned by the root config, serves every venue.
    #[account(mut,seeds=[b"er-fee-payer"],bump=er_fee_payer.bump,constraint=er_fee_payer.config==crate::ROOT_CONFIG @ MixError::Binding)] pub er_fee_payer: Account<'info, ErFeePayer>,
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),er_trader.owner.as_ref()],bump=er_trader.bump,has_one=config)] pub er_trader: Box<Account<'info, ErTrader>>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(config.key()),er_trader.owner.as_ref()],bump=guard.bump,has_one=config)] pub guard: Box<Account<'info,CollateralGuard>>,
    /// CHECK: MagicBlock validates the validator-specific delegated fee vault during the commit.
    #[account(mut,address=ephemeral_rollups_sdk::pda::magic_fee_vault_pda_from_validator(&ER_VALIDATOR))] pub magic_fee_vault: UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct CommitPrepaidErTrader<'info> {
    pub publisher:Signer<'info>,
    #[account(has_one=publisher)] pub config:Account<'info,Config>,
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),er_trader.owner.as_ref()],bump=er_trader.bump,has_one=config)] pub er_trader:Box<Account<'info,ErTrader>>,
    /// CHECK: Validator-specific fee vault, additionally validated by MagicBlock.
    #[account(mut,address=ephemeral_rollups_sdk::pda::magic_fee_vault_pda_from_validator(&ER_VALIDATOR))] pub magic_fee_vault:UncheckedAccount<'info>,
}

#[commit]
#[derive(Accounts)]
pub struct ReleasePrepaidErTrader<'info> {
    pub owner:Signer<'info>,
    pub publisher:Signer<'info>,
    #[account(has_one=publisher)] pub config:Account<'info,Config>,
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump=er_trader.bump,has_one=config,has_one=owner)] pub er_trader:Box<Account<'info,ErTrader>>,
    /// CHECK: Validator-specific fee vault, additionally validated by MagicBlock.
    #[account(mut,address=ephemeral_rollups_sdk::pda::magic_fee_vault_pda_from_validator(&ER_VALIDATOR))] pub magic_fee_vault:UncheckedAccount<'info>,
    pub execution:ErExecution<'info>,
}
#[derive(Accounts)]
pub struct WithdrawPrepaidGas<'info> {
    #[account(mut)] pub owner:Signer<'info>,
    pub publisher:Signer<'info>,
    #[account(has_one=publisher)] pub config:Account<'info,Config>,
    // Account<T> rejects a still-delegated base account. Only the committed,
    // returned ledger may release excess lamports, preserving its rent floor.
    #[account(mut,seeds=[b"er-trader",vns!(config.key()),owner.key().as_ref()],bump=er_trader.bump,has_one=config,has_one=owner)] pub er_trader:Box<Account<'info,ErTrader>>,
    pub execution:BaseExecution<'info>,
}

#[derive(Accounts)]
pub struct InitializeErFeePayer<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Account<'info, Config>,
    #[account(init,payer=admin,space=8+ErFeePayer::INIT_SPACE,seeds=[b"er-fee-payer"],bump)] pub er_fee_payer: Account<'info, ErFeePayer>,
    pub system_program: Program<'info, System>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateErFeePayer<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Account<'info, Config>,
    /// CHECK: Canonical fee payer; DLP verifies current program ownership.
    #[account(mut,del,seeds=[b"er-fee-payer"],bump)] pub er_fee_payer: UncheckedAccount<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const USD: u64 = 1_000_000;
    const PX: u64 = 1_000_000_000_000;

    fn setup() -> (ErTrader, ErRisk, ErCustody, ErQuote) {
        let config = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let asset = Pubkey::new_unique();
        let mut trader = ErTrader { owner, config, pool, next_nonce: 1, ..ErTrader::default() };
        let mut engine = ErRisk { config, ..ErRisk::default() };
        engine.markets[0].sample(oracle::OraclePrice { price: 100 * PX, publish_time: 100, posted_slot: 10 }).unwrap();
        let mut custody = ErCustody { owner, config, pool, ..ErCustody::default() };
        custody.assets[0] = CustodyAsset { asset, deposited_credits: 10 * USD, available: 10 * USD, ..CustodyAsset::default() };
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        let quote = ErQuote { config, asset, is_quote: true, publish_time: 100, source_slot: 100, ..ErQuote::default() };
        (trader, engine, custody, quote)
    }
    fn request(nonce: u64) -> OpenRequest {
        OpenRequest { slot: 0, nonce, market: 0, amount: 5 * USD, leverage: 10, long: true,
            limit_price: 200 * PX, min_value: 5 * USD, max_value: 5 * USD, expires: 120 }
    }
    fn price(engine: &mut ErRisk, value: u64, time: i64) {
        let source = engine.markets[0].source_slot + 1;
        engine.markets[0].sample(oracle::OraclePrice { price: value, publish_time: time, posted_slot: source }).unwrap();
    }
    fn settle(custody: &mut ErCustody, trader: &ErTrader, loss: u64) {
        let slot = &trader.slots[0];
        custody.assets[0].available -= loss;
        custody.assets[0].returned_credits += slot.margin - loss;
        custody.receipts[0] = ErReceipt { nonce: slot.nonce, position: Pubkey::new_unique(), kind: ER_TRADE, settled: true };
    }

    #[test]
    fn manual_hold_rejects_early_close_without_mutation_and_unlocks_at_five_seconds() {
        for long in [true, false] {
            let (mut trader, mut engine, _, quote) = setup();
            let mut req = request(1); req.long = long; req.limit_price = if long {u64::MAX} else {0};
            open_slot(&mut trader, &mut engine, &quote, req, 100).unwrap();
            let owner = trader.owner;
            for now in 100..105 {
                price(&mut engine, 100 * PX, now);
                let mut before = Vec::new(); trader.try_serialize(&mut before).unwrap();
                let seq = engine.next_close_seq;
                let failure = close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, now).unwrap_err();
                assert!(failure.to_string().contains("MinimumHold"));
                let mut after = Vec::new(); trader.try_serialize(&mut after).unwrap();
                assert_eq!(before, after);
                assert_eq!(seq, engine.next_close_seq);
            }
            price(&mut engine, 100 * PX, 105);
            close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 105).unwrap();
            assert_eq!(trader.slots[0].state, ER_PENDING);
            assert_eq!(trader.slots[0].closed_at, 105);
            assert!(require_manual_hold(106, 105).is_err());
            assert!(require_manual_hold(i64::MIN, i64::MAX).is_err());
        }
    }

    #[test]
    fn liquidation_and_protective_close_remain_immediate_and_existing_bust_is_preserved() {
        for long in [true, false] {
            let (mut trader, mut engine, _, quote) = setup();
            let mut req = request(1); req.leverage = 1000; req.long = long;
            req.limit_price = if long {u64::MAX} else {0};
            open_slot(&mut trader, &mut engine, &quote, req, 100).unwrap();
            let bust = trader.slots[0].bust;
            assert_eq!(bust, if long {100 * PX - PX / 10} else {100 * PX + PX / 10});
            // The old half-margin threshold no longer liquidates a new trade.
            price(&mut engine, if long {100 * PX - PX / 20} else {100 * PX + PX / 20}, 100);
            assert!(!engine.markets[0].crossed(&trader.slots[0]));
            price(&mut engine, bust, 100);
            close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 100).unwrap();
            assert!(trader.slots[0].liquidated);
        }
        let (mut trader, mut engine, _, quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        close_slot_authorized(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 100, true).unwrap();
        assert_eq!(trader.slots[0].state, ER_PENDING);
        let (mut trader, mut engine, _, quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        trader.slots[0].bust = 99_950 * PX / 1_000; // Stored pre-upgrade 1000x threshold.
        let recorded = trader.slots[0].bust;
        price(&mut engine, recorded, 101);
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 101).unwrap();
        assert!(trader.slots[0].liquidated);
        assert_eq!(trader.slots[0].bust, recorded);
    }

    #[test]
    fn market_orders_record_execution_price_and_value_without_client_tolerance() {
        for long in [true, false] {
            let (mut trader, mut engine, _, quote) = setup();
            let mut request = request(1);
            request.long = long;
            // These are the unrestricted bounds signed by the market-order UI.
            request.limit_price = if long { u64::MAX } else { 0 };
            request.min_value = 0;
            request.max_value = u64::MAX;
            price(&mut engine, if long { 110 * PX } else { 90 * PX }, 101);
            open_slot(&mut trader, &mut engine, &quote, request, 101).unwrap();
            assert_eq!(trader.slots[0].entry, engine.markets[0].price);
            assert_eq!(trader.slots[0].entry_value, quote.value(request.amount, 101).unwrap());
        }
        let (mut trader, mut engine, _, quote) = setup();
        let mut request = request(1);
        request.limit_price = u64::MAX;
        request.min_value = 0;
        request.max_value = u64::MAX;
        assert!(open_slot(&mut trader, &mut engine, &quote, request, 110).is_err(), "freshness still required");
        request.amount = USD - 1;
        assert!(open_slot(&mut trader, &mut engine, &quote, request, 100).is_err(), "USD risk limits still required");
    }

    #[test]
    fn credits_replay_and_pending_reservations_conserve_collateral() {
        let (mut trader, mut engine, mut custody, mut quote) = setup();
        let stale = ErCustody { owner: custody.owner, config: custody.config, pool: custody.pool,
            assets: custody.assets.clone(), receipts: custody.receipts.clone(), bump: custody.bump,
            core_coverage: custody.core_coverage, ..ErCustody::default() };
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        assert_eq!(trader.assets[0].available + trader.slots[0].margin, custody.assets[0].available);
        assert_eq!(trader.notional, 50 * USD);
        price(&mut engine, 99 * PX, 105);
        quote.source_slot = 102;
        let owner = trader.owner;
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 105).unwrap();
        assert_eq!(trader.slots[0].close_seq, 0);
        assert_eq!(trader.notional, 50 * USD);
        assert_eq!(engine.markets[0].long_oi, 50 * USD);
        assert!(open_slot(&mut trader, &mut engine, &quote, request(2), 105).is_err());
        let loss = math::pnl(5 * USD, 100 * PX, 99 * PX, 10, true).unwrap().1;
        settle(&mut custody, &trader, loss);
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert_eq!(trader.assets[0].available, 10 * USD - loss);
        assert_eq!(trader.assets[0].available, custody.assets[0].available);
        assert_eq!(trader.notional, 0);
        assert_eq!(engine.markets[0].long_oi, 0);
        assert_eq!(trader.slots[0].state, ER_FREE);
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        sync_custody(&mut trader, &mut engine, &stale).unwrap();
        assert_eq!(trader.assets[0].available, 10 * USD - loss);
        assert_eq!(trader.assets[0].credited_returns, 5 * USD - loss);
    }

    #[test]
    fn reused_slots_reject_old_open_close_and_receipt() {
        let (mut trader, mut engine, mut custody, mut quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        price(&mut engine, 101 * PX, 105);
        quote.source_slot = 102;
        quote.publish_time = 105;
        let owner = trader.owner;
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 105).unwrap();
        settle(&mut custody, &trader, 0);
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert!(open_slot(&mut trader, &mut engine, &quote, request(1), 105).is_err());
        open_slot(&mut trader, &mut engine, &quote, request(2), 105).unwrap();
        quote.source_slot = 104;
        price(&mut engine, 101 * PX, 110);
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 101).is_err());
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 2, 100, owner, 101).is_err());
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert_eq!(trader.slots[0].state, ER_OPEN);
        assert_eq!(trader.slots[0].nonce, 2);
        close_slot(&mut trader, &mut engine, &quote, 0, 2, 120, owner, 110).unwrap();
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert_eq!(trader.slots[0].state, ER_PENDING);
        assert_eq!(trader.notional, 50 * USD);
    }

    #[test]
    fn withdrawal_ack_releases_slot_without_creating_return_credit() {
        let (mut trader, mut engine, mut custody, quote) = setup();
        withdraw_slot(&mut trader, &mut engine, quote.asset, 0, 1, 4 * USD, 120, 100).unwrap();
        assert_eq!(trader.assets[0].available, 6 * USD);
        assert_eq!(trader.slots[0].close_seq, 0);
        assert_eq!(engine.next_close_seq, 1);
        assert!(withdraw_slot(&mut trader, &mut engine, quote.asset, 1, 2, 7 * USD, 120, 100).is_err());
        custody.assets[0].available -= 4 * USD;
        custody.receipts[0] = ErReceipt { nonce: 1, kind: ER_WITHDRAW, settled: true, ..ErReceipt::default() };
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert_eq!(trader.assets[0].available, custody.assets[0].available);
        assert_eq!(trader.slots[0].state, ER_FREE);
    }

    #[test]
    fn transient_crossing_cannot_be_erased_by_recovery_or_stale_observer() {
        let (mut trader, mut engine, _, quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        price(&mut engine, 89 * PX, 100);
        price(&mut engine, 105 * PX, 101);
        observe_slots(&mut trader, &engine).unwrap();
        assert!(trader.slots[0].bust_observed);
        let cursor = trader.slots[0].cursor;
        price(&mut engine, 110 * PX, 102);
        observe_slots(&mut trader, &engine).unwrap();
        assert_eq!(trader.slots[0].cursor, cursor);
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 106).is_err());
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 102).unwrap();
        assert!(trader.slots[0].liquidated);
        assert_eq!(trader.slots[0].exit, 110 * PX);
        assert_eq!(trader.slots[0].close_seq, 0);
    }

    #[test]
    fn history_gap_cannot_be_advanced_into_a_false_win() {
        let (mut trader, mut engine, _, mut quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        let cursor = trader.slots[0].cursor;
        for _ in 0..65 { price(&mut engine, 105 * PX, 101); }
        observe_slots(&mut trader, &engine).unwrap();
        assert_eq!(trader.slots[0].cursor, cursor);
        quote.source_slot = 102;
        let owner = trader.owner;
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 101).is_err());
        assert_eq!(trader.slots[0].state, ER_OPEN);
        price(&mut engine, 89 * PX, 101);
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 101).unwrap();
        assert!(trader.slots[0].liquidated);
    }

    #[test]
    fn history_gap_forfeiture_requires_owner_nonce_expiry_and_a_real_gap() {
        let (mut trader, mut engine, _, quote) = setup();
        engine.markets[0].publish_time = 96;
        let mut quote = quote; quote.publish_time = 96;
        open_slot(&mut trader, &mut engine, &quote, request(1), 96).unwrap();
        quote.publish_time = 101;
        let owner = trader.owner;
        for _ in 0..64 { price(&mut engine, 105 * PX, 101); }
        let exit = oracle::OraclePrice { price: 106 * PX, publish_time: 101, posted_slot: 1000 };
        let unchanged = trader.clone();
        assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 101).is_err());
        price(&mut engine, 105 * PX, 101);
        for (slot, nonce, expiry, caller) in [(0,1,120,Pubkey::new_unique()), (0,2,120,owner),
            (0,1,100,owner), (0,1,222,owner), (8,1,120,owner)] {
            assert!(forfeit_history_gap_slot(&mut trader, &engine, slot, nonce, expiry, caller, exit, 101).is_err());
        }
        assert_eq!(trader.slots[0].state, unchanged.slots[0].state);
        assert_eq!(trader.slots[0].cursor, unchanged.slots[0].cursor);
        assert_eq!(trader.assets[0].available, unchanged.assets[0].available);
        assert!(!trader.slots[0].bust_observed);
    }

    #[test]
    fn history_gap_forfeiture_preserves_backing_and_cannot_replay_after_ack() {
        let (mut trader, mut engine, mut custody, quote) = setup();
        engine.markets[0].publish_time = 96;
        let mut quote = quote; quote.publish_time = 96;
        open_slot(&mut trader, &mut engine, &quote, request(1), 96).unwrap();
        quote.publish_time = 101;
        for _ in 0..65 { price(&mut engine, 105 * PX, 101); }
        let owner = trader.owner;
        let exit = oracle::OraclePrice { price: 106 * PX, publish_time: 101, posted_slot: 1000 };
        let (notional, available, oi, seq) = (trader.notional, trader.assets[0].available,
            engine.markets[0].long_oi, engine.next_close_seq);
        forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 101).unwrap();
        let position = trader.slots[0];
        assert_eq!(position.state, ER_PENDING);
        assert_eq!(position.kind, ER_TRADE);
        assert_eq!(position.exit, 106 * PX); // Favorable real exit still forfeits the full margin.
        assert!(position.liquidated && position.bust_observed);
        assert_eq!(position.close_seq, 0);
        assert_eq!(position.margin, 5 * USD);
        assert_eq!((trader.notional, trader.assets[0].available, engine.markets[0].long_oi,
            engine.next_close_seq), (notional, available, oi, seq));
        assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 101).is_err());
        settle(&mut custody, &trader, position.margin);
        sync_custody(&mut trader, &mut engine, &custody).unwrap();
        assert_eq!(trader.assets[0].available, available);
        assert_eq!(trader.notional, 0);
        assert_eq!(engine.markets[0].long_oi, 0);
        let mut next = request(2); next.limit_price = 200 * PX;
        open_slot(&mut trader, &mut engine, &quote, next, 101).unwrap();
        for _ in 0..65 { price(&mut engine, 105 * PX, 102); }
        assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 102).is_err());
        assert_eq!(trader.slots[0].nonce, 2);
        assert_eq!(trader.slots[0].state, ER_OPEN);
    }

    #[test]
    fn history_gap_forfeiture_rejects_known_crossings_stale_or_rewound_prices() {
        let (mut trader, mut engine, _, quote) = setup();
        engine.markets[0].publish_time = 96;
        let mut quote = quote; quote.publish_time = 96;
        open_slot(&mut trader, &mut engine, &quote, request(1), 96).unwrap();
        quote.publish_time = 101;
        for _ in 0..65 { price(&mut engine, 105 * PX, 101); }
        let owner = trader.owner;
        let exit = oracle::OraclePrice { price: 106 * PX, publish_time: 101, posted_slot: 1000 };
        for invalid in [oracle::OraclePrice { publish_time: 97, ..exit },
            oracle::OraclePrice { publish_time: 104, ..exit },
            oracle::OraclePrice { posted_slot: 1, ..exit }, oracle::OraclePrice { price: 0, ..exit }] {
            assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, invalid, 101).is_err());
        }
        trader.slots[0].bust_observed = true;
        assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 101).is_err());
        trader.slots[0].bust_observed = false;
        price(&mut engine, 80 * PX, 101);
        assert!(forfeit_history_gap_slot(&mut trader, &engine, 0, 1, 120, owner, exit, 101).is_err());
        let impostor = ErRisk { config: Pubkey::new_unique(), ..ErRisk::default() };
        assert!(forfeit_history_gap_slot(&mut trader, &impostor, 0, 1, 120, owner, exit, 101).is_err());
        assert_eq!(trader.slots[0].state, ER_OPEN);
    }

    #[test]
    fn compact_ring_preserves_every_retained_crossing_at_wrap_boundaries() {
        for gap in [1u64, 63, 64, 65, 128] {
            let (mut trader, mut engine, _, _) = setup();
            trader.slots[0] = ErSlot { state: ER_OPEN, kind: ER_TRADE, cursor: 1,
                bust: 90 * PX, long: true, ..ErSlot::default() };
            for offset in 1..=gap {
                // A transient crossing at the oldest possible retained point.
                let crossing = offset == gap.saturating_sub(63).max(1);
                price(&mut engine, if crossing { 89 * PX } else { 105 * PX }, 101);
            }
            assert!(engine.markets[0].crossed(&trader.slots[0]), "gap {gap}");
            observe_slots(&mut trader, &engine).unwrap();
            assert!(trader.slots[0].bust_observed, "gap {gap}");
        }
        let (mut trader, mut engine, _, _) = setup();
        trader.slots[0] = ErSlot { state: ER_OPEN, kind: ER_TRADE, cursor: 1,
            bust: 90 * PX, long: true, ..ErSlot::default() };
        price(&mut engine, 89 * PX, 101);
        for _ in 0..64 { price(&mut engine, 105 * PX, 101); }
        // An overwritten crossing is unknowable; the cursor must stay frozen.
        assert!(!engine.markets[0].crossed(&trader.slots[0]));
        observe_slots(&mut trader, &engine).unwrap();
        assert_eq!(trader.slots[0].cursor, 1);
        assert!(!trader.slots[0].bust_observed);
    }

    #[test]
    fn manual_close_freezes_winner_rank_without_waiting_for_collateral() {
        let (mut trader, mut engine, _, mut quote) = setup();
        open_slot(&mut trader, &mut engine, &quote, request(1), 100).unwrap();
        price(&mut engine, 105 * PX, 105);
        quote.source_slot = 100;
        quote.publish_time = 90; // even a stale collateral quote cannot delay exit
        let owner = trader.owner;
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, Pubkey::new_unique(), 105).is_err());
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 105).unwrap();
        assert_eq!(trader.slots[0].close_seq, 1);
        assert_eq!(engine.next_close_seq, 2);
        price(&mut engine, 80 * PX, 106);
        observe_slots(&mut trader, &engine).unwrap();
        assert_eq!(trader.slots[0].exit, 105 * PX);
        assert!(!trader.slots[0].liquidated);
        assert!(close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 106).is_err());
    }

    #[test]
    fn exact_full_size_quote_and_freshness_replay_checks() {
        let (_, _, _, mut quote) = setup();
        quote.is_quote = false;
        #[cfg(feature="mainnet")]
        { quote.mainnet = crate::MainnetCollateralContext { real_quote_reserve:100*USD,
            pool_source_slot:101, usd_per_quote_atom_e21:1_000_000_000_000_000, conversion_publish_time:101, conversion_source_slot:101 }; }
        quote.publish(10 * USD, 100 * USD, [20, 5, 0], 101, 101, 101).unwrap();
        assert_eq!(quote.value(USD, 101).unwrap(), 9_068_181);
        assert!(quote.value(USD, 101).unwrap() * 5 > quote.value(5 * USD, 101).unwrap());
        assert!(quote.value(USD, 105).is_err());
        assert!(quote.publish(1, 1, [0; 3], 101, 101, 101).is_err());
        assert!(quote.publish(1, 1, [0; 3], 100, 102, 101).is_err());
        assert!(quote.publish(1, 1, [10_000, 0, 0], 101, 102, 101).is_err());
    }

    #[test]
    fn source_repeats_are_idempotent_but_new_equal_prices_keep_history() {
        let (_, mut engine, _, _) = setup();
        let original = engine.markets[0].sequence;
        engine.markets[0].sample(oracle::OraclePrice { price: 100 * PX, publish_time: 100, posted_slot: 10 }).unwrap();
        assert_eq!(engine.markets[0].sequence, original);
        price(&mut engine, 100 * PX, 100);
        assert_eq!(engine.markets[0].sequence, original + 1);
        assert!(engine.markets[0].sample(oracle::OraclePrice { price: 50 * PX, publish_time: 100, posted_slot: 10 }).is_err());
        assert!(engine.markets[0].sample(oracle::OraclePrice { price: 50 * PX, publish_time: 99, posted_slot: 12 }).is_err());
    }

    #[test]
    fn user_global_exposure_cannot_be_reused_while_settlement_pending() {
        let (mut trader, mut engine, _, mut quote) = setup();
        trader.assets[0].available = 10_000_000 * USD;
        let mut first = request(1);
        first.amount = risk::MAX_NOTIONAL / 2000;
        first.min_value = first.amount;
        first.max_value = first.amount;
        first.leverage = 1000;
        open_slot(&mut trader, &mut engine, &quote, first, 100).unwrap();
        let mut second = first;
        second.slot = 1;
        second.nonce = 2;
        second.market = 1;
        engine.markets[1].sample(oracle::OraclePrice { price: 100 * PX, publish_time: 100, posted_slot: 10 }).unwrap();
        open_slot(&mut trader, &mut engine, &quote, second, 100).unwrap();
        price(&mut engine, 105 * PX, 105);
        quote.source_slot = 102;
        quote.publish_time = 105;
        let owner = trader.owner;
        close_slot(&mut trader, &mut engine, &quote, 0, 1, 120, owner, 105).unwrap();
        let mut third = request(3);
        third.slot = 2;
        assert!(open_slot(&mut trader, &mut engine, &quote, third, 105).is_err());
        assert_eq!(trader.notional, risk::MAX_NOTIONAL);
    }

    #[test]
    fn account_sizes_fit_delegation_commit_limit() {
        assert!(8 + ErTrader::INIT_SPACE < 10_240);
        assert!(8 + ErRisk::INIT_SPACE < 10_240);
        // Box is an in-memory stack optimization, never a Borsh length prefix.
        let (trader, engine, custody, _) = setup();
        for (actual, expected) in [
            ({ let mut bytes=Vec::new(); trader.try_serialize(&mut bytes).unwrap(); bytes.len() }, 8 + ErTrader::INIT_SPACE),
            ({ let mut bytes=Vec::new(); engine.try_serialize(&mut bytes).unwrap(); bytes.len() }, 8 + ErRisk::INIT_SPACE),
            ({ let mut bytes=Vec::new(); custody.try_serialize(&mut bytes).unwrap(); bytes.len() }, 8 + ErCustody::INIT_SPACE),
        ] { assert_eq!(actual, expected); }
        assert!(std::mem::size_of::<ErTrader>() < 256);
        assert!(std::mem::size_of::<ErCustody>() < 256);
        assert!(std::mem::size_of::<ErRisk>() < 256);
        assert_eq!(ErRisk::INIT_SPACE, 2_357);
    }

    /// Deterministic accounting simulation, not a transaction/load benchmark.
    /// Each owner repeatedly has a pending trade and withdrawal at once; base
    /// receipts and stale cloned custody snapshots are delivered repeatedly.
    #[test]
    fn hundred_users_twenty_cycles_conserve_custody_and_release_oi_once() {
        let (template, mut engine, _, mut quote) = setup();
        let mut users: Vec<(ErTrader, ErCustody)> = (0..100).map(|_| {
            let owner = Pubkey::new_unique();
            let trader = ErTrader { owner, config: template.config, pool: template.pool,
                next_nonce: 1, ..ErTrader::default() };
            let mut custody = ErCustody { owner, config: template.config,
                pool: template.pool, ..ErCustody::default() };
            custody.assets[0].asset = quote.asset;
            (trader, custody)
        }).collect();
        let mut aggregate_deposits = 0u64;
        let mut aggregate_losses = 0u64;
        let mut aggregate_withdrawals = 0u64;
        let mut next_base_winner = 1u64;
        for cycle in 0..20 {
            let now = 100 + cycle * 6;
            quote.publish_time = now;
            quote.source_slot += 2;
            price(&mut engine, 100 * PX, now);
            for (index, (trader, custody)) in users.iter_mut().enumerate() {
                custody.assets[0].deposited_credits += 10 * USD;
                custody.assets[0].available += 10 * USD;
                aggregate_deposits += 10 * USD;
                sync_custody(trader, &mut engine, custody).unwrap();
                let nonce = trader.next_nonce;
                let mut open = request(nonce);
                open.amount = 4 * USD;
                open.min_value = open.amount;
                open.max_value = open.amount;
                open.long = index % 2 == 0;
                open.limit_price = if open.long { 200 * PX } else { 1 };
                open.expires = now + 20;
                open_slot(trader, &mut engine, &quote, open, now).unwrap();
                withdraw_slot(trader, &mut engine, quote.asset, 1, nonce + 1, USD, now + 20, now).unwrap();
                assert_eq!(trader.assets[0].available + 5 * USD, custody.assets[0].available);
            }
            assert_eq!(engine.markets[0].long_oi + engine.markets[0].short_oi, 4_000 * USD);
            price(&mut engine, if cycle % 3 == 0 { 89 * PX } else { 105 * PX }, now + 5);
            quote.source_slot += 2;
            for (trader, _) in &mut users {
                let owner = trader.owner;
                let nonce = trader.slots[0].nonce;
                close_slot(trader, &mut engine, &quote, 0, nonce, now + 20, owner, now + 5).unwrap();
            }
            for (index, (trader, custody)) in users.iter_mut().enumerate() {
                let stale = ErCustody { owner: custody.owner, config: custody.config, pool: custody.pool,
                    assets: custody.assets.clone(), receipts: custody.receipts.clone(), bump: custody.bump,
                    core_coverage: custody.core_coverage, ..ErCustody::default() };
                let intent = trader.slots[0];
                crate::basebridge::pending_intent(trader, custody, 0, intent.nonce, ER_TRADE, next_base_winner).unwrap();
                let (win, pnl) = math::pnl(intent.margin, intent.entry, intent.exit, intent.leverage, intent.long).unwrap();
                let loss = if intent.liquidated { intent.margin } else if win { 0 } else { pnl };
                // Alternate which base receipt arrives first, including while
                // the trade still holds ER exposure and reserved collateral.
                if index % 2 == 0 {
                    let withdrawal = trader.slots[1];
                    crate::basebridge::pending_intent(trader, custody, 1, withdrawal.nonce, ER_WITHDRAW, next_base_winner).unwrap();
                    custody.assets[0].available -= withdrawal.margin;
                    custody.receipts[1] = ErReceipt { nonce: withdrawal.nonce, kind: ER_WITHDRAW, settled: true, ..ErReceipt::default() };
                    sync_custody(trader, &mut engine, custody).unwrap();
                    assert_eq!(trader.slots[0].state, ER_PENDING);
                    assert_eq!(trader.notional, 40 * USD);
                }
                crate::basebridge::finish_trade(custody, 0, &intent, Pubkey::new_unique(), loss).unwrap();
                if intent.close_seq > 0 { next_base_winner += 1; }
                if index % 2 != 0 {
                    let withdrawal = trader.slots[1];
                    crate::basebridge::pending_intent(trader, custody, 1, withdrawal.nonce, ER_WITHDRAW, next_base_winner).unwrap();
                    custody.assets[0].available -= withdrawal.margin;
                    custody.receipts[1] = ErReceipt { nonce: withdrawal.nonce, kind: ER_WITHDRAW, settled: true, ..ErReceipt::default() };
                }
                aggregate_losses += loss;
                aggregate_withdrawals += USD;
                sync_custody(trader, &mut engine, custody).unwrap();
                sync_custody(trader, &mut engine, &stale).unwrap();
                sync_custody(trader, &mut engine, custody).unwrap();
                assert_eq!(trader.notional, 0);
                assert!(trader.slots.iter().all(|slot| slot.state == ER_FREE));
                assert_eq!(trader.assets[0].available, custody.assets[0].available);
                assert!(crate::basebridge::finish_trade(custody, 0, &intent, Pubkey::new_unique(), loss).is_err());
            }
            assert_eq!(engine.markets[0].long_oi + engine.markets[0].short_oi, 0);
            assert_eq!(engine.next_close_seq, next_base_winner);
            let total_custody: u64 = users.iter().map(|(_, custody)| custody.assets[0].available).sum();
            assert_eq!(total_custody, aggregate_deposits - aggregate_losses - aggregate_withdrawals);
        }
    }
}
