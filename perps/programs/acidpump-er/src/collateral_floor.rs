//! Per-position collateral low-water marks. Mainnet tracks relative USD token
//! prices; executable bag valuation remains an entry and settlement concern.
//! The deployed devnet layout and valuation remain unchanged.
use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::delegate;
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use crate::{engine::*, pump, Asset, Config, MixError};
use crate::venue::{ns, shared};

pub const HISTORY: usize = 64;
pub const MAX_SAMPLE_GAP: i64 = 10;

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct CollateralSample {
    pub base: u64,
    pub quote: u64,
    pub fees: [u16; 3],
    pub publish_time: i64,
    #[cfg(feature="mainnet")]
    pub mainnet: crate::mainnet_collateral::MainnetCollateralContext,
}

#[account]
pub struct CollateralHistory {
    pub config: Pubkey,
    pub asset: Pubkey,
    pub sequence: u64,
    pub source_slot: u64,
    pub publish_time: i64,
    /// A position spanning a monitoring outage cannot claim an unverified win.
    pub last_gap: u64,
    pub last_gap_start: i64,
    pub is_quote: bool,
    #[cfg(feature="mainnet")]
    pub decimals: u8,
    pub samples: Vec<CollateralSample>,
    pub bump: u8,
}
impl Space for CollateralHistory {
    const INIT_SPACE: usize = 64 + 5 * 8 + 1 + cfg!(feature="mainnet") as usize + 4 + HISTORY * CollateralSample::INIT_SPACE + 1;
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct CollateralFloor {
    pub nonce: u64,
    pub value: u64,
    pub cursor: u64,
    pub incomplete: bool,
    pub closed_at: i64,
    pub exit: u64,
    pub covered_at: i64,
    /// USD/token in 1e12 units, captured from the entry observation. Supply,
    /// historical swap fees and bag size cannot change this price baseline.
    #[cfg(feature="mainnet")]
    pub entry_price: u64,
}

#[account]
pub struct CollateralGuard {
    pub owner: Pubkey,
    pub config: Pubkey,
    #[borsh(deserialize_with="crate::heap::deserialize_fixed")]
    pub floors: Box<[CollateralFloor; ER_SLOTS]>,
    pub bump: u8,
}
impl Space for CollateralGuard {const INIT_SPACE:usize=2*32+ER_SLOTS*CollateralFloor::INIT_SPACE+1;}

/// The existing trusted publisher attests an archived replay. This is not a
/// trustless historical-state proof. Every immutable close field is bound to
/// prevent cross-position reuse, and recovery can only reduce the stored floor.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy)]
pub struct CollateralRecovery {
    pub slot: u8,
    pub nonce: u64,
    pub opened_source_slot: u64,
    pub closed_at: i64,
    pub exit: u64,
    pub value: u64,
    pub evidence_hash: [u8; 32],
}

impl CollateralHistory {
    #[cfg(feature="mainnet")]
    pub fn price(&self, sample: CollateralSample) -> Result<u64> {
        if self.is_quote { return Ok(1_000_000_000_000); }
        if self.asset == sol_collateral_asset() {
            require!(sample.base == 0 && sample.fees == [0;3] && sample.quote > 0, MixError::Quote);
            return Ok(sample.quote);
        }
        sample.mainnet.spot_price(sample.base, sample.quote, sample.publish_time, self.decimals)
    }
    pub fn protected_value(&self, sample: CollateralSample, position: &ErSlot, floor: &CollateralFloor) -> Result<u64> {
        #[cfg(feature="mainnet")]
        {
            require!(floor.entry_price > 0, MixError::History);
            let price = self.price(sample)?.min(floor.entry_price);
            crate::math::div(position.entry_value as u128, price as u128, floor.entry_price as u128)
        }
        #[cfg(not(feature="mainnet"))]
        { let _ = floor; self.value(sample, position.margin) }
    }
    pub fn record(&mut self, quote: &ErQuote) -> Result<()> {
        require!(self.config == quote.config && self.asset == quote.asset && self.is_quote == quote.is_quote
            && self.samples.len() == HISTORY, MixError::Binding);
        require!(quote.source_slot > self.source_slot && quote.publish_time >= self.publish_time, MixError::Replay);
        let sequence = self.sequence.checked_add(1).ok_or(MixError::Arithmetic)?;
        if self.sequence > 0 && quote.publish_time.saturating_sub(self.publish_time) > MAX_SAMPLE_GAP {
            self.last_gap = sequence;
            self.last_gap_start = self.publish_time;
        }
        self.samples[(sequence % HISTORY as u64) as usize] = CollateralSample {
            base: quote.base_reserve, quote: quote.quote_reserve, fees: quote.fee_bps, publish_time:quote.publish_time,
            #[cfg(feature="mainnet")]
            mainnet: quote.mainnet,
        };
        self.sequence = sequence;
        self.source_slot = quote.source_slot;
        self.publish_time = quote.publish_time;
        Ok(())
    }
    pub fn matches(&self, quote: &ErQuote) -> Result<()> {
        require!(self.config == quote.config && self.asset == quote.asset && self.is_quote == quote.is_quote
            && self.samples.len() == HISTORY && self.sequence > 0
            && self.source_slot == quote.source_slot && self.publish_time == quote.publish_time, MixError::History);
        let sample = self.samples[(self.sequence % HISTORY as u64) as usize];
        require!(sample.base == quote.base_reserve && sample.quote == quote.quote_reserve && sample.fees == quote.fee_bps, MixError::Binding);
        #[cfg(feature="mainnet")]
        require!(sample.mainnet == quote.mainnet, MixError::Binding);
        Ok(())
    }
    pub fn value(&self, sample: CollateralSample, amount: u64) -> Result<u64> {
        if self.is_quote { return Ok(amount); }
        if self.asset == sol_collateral_asset() {
            require!(sample.base == 0 && sample.fees == [0; 3], MixError::Quote);
            return sol_collateral_value(amount, sample.quote);
        }
        #[cfg(feature="mainnet")]
        { sample.mainnet.value(sample.base, sample.quote, sample.fees, sample.publish_time, amount) }
        #[cfg(not(feature="mainnet"))]
        {
            // Empty real quote liquidity is a genuine zero, never a skipped sample.
            if sample.quote == 0 || sample.base == 0 { return Ok(0); }
            pump::sell_from_reserves(amount, sample.base, sample.quote, sample.fees)
        }
    }
}

impl CollateralGuard {
    pub fn empty_floors()->Box<[CollateralFloor;ER_SLOTS]> {
        vec![CollateralFloor::default();ER_SLOTS].into_boxed_slice().try_into().unwrap_or_else(|_|unreachable!())
    }
    pub fn bind(&self, trader: &ErTrader) -> Result<()> {
        require!(self.owner == trader.owner && self.config == trader.config, MixError::Binding);
        Ok(())
    }
    pub fn start(&mut self, trader: &ErTrader, history: &CollateralHistory, slot: u8) -> Result<()> {
        self.bind(trader)?;
        let position = trader.slots.get(slot as usize).ok_or(MixError::Binding)?;
        require!(position.state == ER_OPEN && position.asset == history.asset && shared(self.config, history.config)
            && history.sequence > 0 && history.source_slot == position.opened_source_slot, MixError::Binding);
        self.floors[slot as usize] = CollateralFloor { nonce: position.nonce, value: position.entry_value,
            #[cfg(feature="mainnet")]
            entry_price: {
                let price=history.price(history.samples[(history.sequence % HISTORY as u64) as usize])?;
                require!(price > 0, MixError::Quote); price
            },
            cursor: history.sequence, covered_at:history.publish_time, ..CollateralFloor::default() };
        Ok(())
    }
    pub fn observe(&mut self, trader: &ErTrader, history: &CollateralHistory) -> Result<()> {
        self.bind(trader)?;
        require!(shared(self.config, history.config) && history.samples.len() == HISTORY, MixError::Binding);
        for (position, floor) in trader.slots.iter().zip(self.floors.iter_mut()) {
            if ![ER_OPEN,ER_PENDING].contains(&position.state) || position.kind != ER_TRADE || position.asset != history.asset { continue; }
            // Once a post-close sample is included, later token moves and
            // later monitoring outages cannot change this position's outcome.
            if position.state == ER_PENDING && floor.covered_at > position.closed_at { continue; }
            require!(floor.nonce == position.nonce && floor.value <= position.entry_value,
                MixError::Binding);
            require!(history.sequence >= floor.cursor, MixError::Replay);
            if history.sequence - floor.cursor > HISTORY as u64 {
                floor.incomplete = true;
            }
            let first = (floor.cursor + 1).max(history.sequence.saturating_sub(HISTORY as u64 - 1));
            for sequence in first..=history.sequence {
                let sample = history.samples[(sequence % HISTORY as u64) as usize];
                if sample.publish_time.saturating_sub(floor.covered_at)>MAX_SAMPLE_GAP {
                    floor.incomplete=true;
                }
                if position.state == ER_OPEN || sample.publish_time <= position.closed_at {
                    let value = history.protected_value(sample, position, floor)?;
                    floor.value = floor.value.min(value);
                }
                floor.covered_at = sample.publish_time;
                if position.state == ER_PENDING && sample.publish_time > position.closed_at { break; }
            }
            floor.cursor = history.sequence;
        }
        Ok(())
    }
    pub fn freeze(&mut self, trader: &ErTrader, slot: u8) -> Result<()> {
        self.bind(trader)?;
        let position = trader.slots.get(slot as usize).ok_or(MixError::Binding)?;
        let floor = self.floors.get_mut(slot as usize).ok_or(MixError::Binding)?;
        require!(position.state == ER_PENDING && position.kind == ER_TRADE && floor.nonce == position.nonce,
            MixError::Binding);
        // Closing fixes market exposure even if historical collateral still
        // needs recovery. settled_value continues to block unverified payouts.
        floor.closed_at = position.closed_at;
        floor.exit = position.exit;
        Ok(())
    }
    pub fn recover(&mut self, trader: &ErTrader, history: &CollateralHistory, recovery: CollateralRecovery) -> Result<()> {
        self.bind(trader)?;
        let position = trader.slots.get(recovery.slot as usize).ok_or(MixError::Binding)?;
        let floor = self.floors.get_mut(recovery.slot as usize).ok_or(MixError::Binding)?;
        require!(position.state == ER_PENDING && position.kind == ER_TRADE && position.close_seq > 0,
            MixError::Closed);
        require!(position.nonce == recovery.nonce && floor.nonce == recovery.nonce
            && position.opened_source_slot == recovery.opened_source_slot
            && position.closed_at == recovery.closed_at && floor.closed_at == recovery.closed_at
            && position.exit == recovery.exit && floor.exit == recovery.exit
            && position.asset == history.asset && shared(trader.config, history.config), MixError::Binding);
        require!(floor.incomplete || floor.covered_at <= position.closed_at, MixError::Replay);
        require!(recovery.evidence_hash != [0; 32] && recovery.value <= floor.value
            && recovery.value <= position.entry_value, MixError::History);
        floor.value = recovery.value;
        floor.covered_at = position.closed_at.checked_add(1).ok_or(MixError::Arithmetic)?;
        floor.cursor = history.sequence;
        floor.incomplete = false;
        Ok(())
    }
    pub fn settled_value(&self, trader: &ErTrader, slot: u8) -> Result<u64> {
        self.bind(trader)?;
        let position = trader.slots.get(slot as usize).ok_or(MixError::Binding)?;
        let floor = self.floors.get(slot as usize).ok_or(MixError::Binding)?;
        require!(position.state == ER_PENDING && position.kind == ER_TRADE && floor.nonce == position.nonce
            && floor.closed_at == position.closed_at && floor.exit == position.exit
            && floor.value <= position.entry_value, MixError::Binding);
        require!(position.close_seq == 0 || (!floor.incomplete && floor.covered_at > position.closed_at), MixError::History);
        Ok(floor.value)
    }
}

pub fn initialize_history(ctx: Context<InitializeCollateralHistory>) -> Result<()> {
    *ctx.accounts.history = CollateralHistory { config:ctx.accounts.config.key(), asset:ctx.accounts.asset.key(),
        #[cfg(feature="mainnet")]
        decimals:ctx.accounts.asset.decimals,
        is_quote:pump::is_quote(&ctx.accounts.asset), sequence:0, source_slot:0, publish_time:0, last_gap:0,last_gap_start:0,
        samples:vec![CollateralSample::default();HISTORY], bump:ctx.bumps.history };
    Ok(())
}
pub fn initialize_guard(ctx: Context<InitializeCollateralGuard>) -> Result<()> {
    **ctx.accounts.guard = CollateralGuard { owner:ctx.accounts.owner.key(), config:ctx.accounts.config.key(),
        floors:CollateralGuard::empty_floors(), bump:ctx.bumps.guard };
    Ok(())
}
pub fn delegate_history(ctx: Context<DelegateCollateralHistory>) -> Result<()> {
    let asset = ctx.accounts.asset.key();
    ctx.accounts.delegate_history(&ctx.accounts.admin, &[b"er-collateral-history",asset.as_ref()],
        DelegateConfig {validator:Some(ER_VALIDATOR),commit_frequency_ms:u32::MAX})?;
    Ok(())
}
pub fn delegate_guard(ctx: Context<DelegateCollateralGuard>) -> Result<()> {
    // Delegation is publicly sponsored; reject legacy layouts so a third
    // party cannot race the paused return/resize migration.
    require!(ctx.accounts.guard.data_len()==8+CollateralGuard::INIT_SPACE,MixError::Binding);
    // The guard's size is unchanged by the backing extension. A public sponsor
    // must not redelegate it between the paired return and trader resize.
    #[cfg(feature="mainnet")]
    require!(!ctx.accounts.config.paused || ctx.accounts.payer.key()==ctx.accounts.config.admin,MixError::Paused);
    let owner = ctx.accounts.owner.key();
    let n = ns(ctx.accounts.config.key());
    let seeds = crate::venue::ns_seeds(b"er-collateral-guard", &n, &[owner.as_ref()]);
    ctx.accounts.delegate_guard(&ctx.accounts.payer, &seeds,
        DelegateConfig {validator:Some(ER_VALIDATOR),commit_frequency_ms:u32::MAX})?;
    Ok(())
}
pub fn observe(ctx: Context<ObserveCollateral>) -> Result<()> {
    ctx.accounts.guard.observe(&ctx.accounts.er_trader,&ctx.accounts.history)
}

pub fn recover(ctx: Context<RecoverCollateral>, recovery: CollateralRecovery) -> Result<()> {
    require!(recovery.closed_at > 0 && recovery.closed_at <= Clock::get()?.unix_timestamp, MixError::Stale);
    ctx.accounts.guard.recover(&ctx.accounts.er_trader, &ctx.accounts.history, recovery)?;
    emit!(CollateralHistoryRecovered { owner:ctx.accounts.er_trader.owner, asset:ctx.accounts.history.asset,
        slot:recovery.slot,nonce:recovery.nonce,opened_source_slot:recovery.opened_source_slot,
        closed_at:recovery.closed_at,exit:recovery.exit,value:recovery.value,evidence_hash:recovery.evidence_hash });
    Ok(())
}

#[event]
pub struct CollateralHistoryRecovered {
    pub owner: Pubkey, pub asset: Pubkey, pub slot: u8, pub nonce: u64,
    pub opened_source_slot: u64, pub closed_at: i64, pub exit: u64, pub value: u64,
    pub evidence_hash: [u8;32],
}

#[derive(Accounts)]
pub struct RecoverCollateral<'info> {
    pub publisher: Signer<'info>,
    #[account(has_one=publisher)] pub config: Account<'info,Config>,
    #[account(seeds=[b"er-trader",vns!(config.key()),er_trader.owner.as_ref()],bump=er_trader.bump,has_one=config)]
    pub er_trader: Box<Account<'info,ErTrader>>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(config.key()),er_trader.owner.as_ref()],bump=guard.bump,has_one=config)]
    pub guard: Box<Account<'info,CollateralGuard>>,
    #[account(seeds=[b"er-collateral-history",history.asset.as_ref()],bump=history.bump,constraint=shared(config.key(),history.config) @ MixError::Binding)]
    pub history: Account<'info,CollateralHistory>,
    pub execution: ErExecution<'info>,
}

#[derive(Accounts)]
pub struct InitializeCollateralHistory<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,
        constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)]
    pub config: Account<'info,Config>,
    #[account(seeds=[b"asset",asset.mint.as_ref()],bump=asset.bump)] pub asset: Account<'info,Asset>,
    #[account(init,payer=admin,space=8+CollateralHistory::INIT_SPACE,seeds=[b"er-collateral-history",asset.key().as_ref()],bump)]
    pub history: Account<'info,CollateralHistory>,
    pub system_program: Program<'info,System>,
}
#[derive(Accounts)]
pub struct InitializeCollateralGuard<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    /// CHECK: Canonical owner's namespace; no custody or user permissions change.
    pub owner: UncheckedAccount<'info>,
    pub config: Account<'info,Config>,
    #[account(init,payer=payer,space=8+CollateralGuard::INIT_SPACE,seeds=[b"er-collateral-guard",vns!(config.key()),owner.key().as_ref()],bump)]
    pub guard: Box<Account<'info,CollateralGuard>>,
    pub system_program: Program<'info,System>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateCollateralHistory<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,
        constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)]
    pub config: Account<'info,Config>,
    #[account(seeds=[b"asset",asset.mint.as_ref()],bump=asset.bump)] pub asset: Account<'info,Asset>,
    /// CHECK: Canonical initialized history; DLP verifies program ownership.
    #[account(mut,del,seeds=[b"er-collateral-history",asset.key().as_ref()],bump)] pub history: UncheckedAccount<'info>,
}
#[delegate]
#[derive(Accounts)]
pub struct DelegateCollateralGuard<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    /// CHECK: Canonical owner's namespace; the PDA can only be initialized with this owner.
    pub owner: UncheckedAccount<'info>,
    pub config: Account<'info,Config>,
    /// CHECK: DLP requires the canonical program-owned guard. Anyone can sponsor its delegation.
    #[account(mut,del,seeds=[b"er-collateral-guard",vns!(config.key()),owner.key().as_ref()],bump)] pub guard: UncheckedAccount<'info>,
}
#[derive(Accounts)]
pub struct ObserveCollateral<'info> {
    #[account(seeds=[b"er-trader",vns!(er_trader.config),er_trader.owner.as_ref()],bump=er_trader.bump)] pub er_trader: Box<Account<'info,ErTrader>>,
    #[account(mut,seeds=[b"er-collateral-guard",vns!(er_trader.config),er_trader.owner.as_ref()],bump=guard.bump)] pub guard: Box<Account<'info,CollateralGuard>>,
    #[account(seeds=[b"er-collateral-history",history.asset.as_ref()],bump=history.bump)] pub history: Account<'info,CollateralHistory>,
    pub execution: ErExecution<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const USD:u64=1_000_000;
    #[test]
    fn sol_uses_oracle_value_and_retains_a_dip_through_close() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        quote.asset=sol_collateral_asset(); history.asset=quote.asset;
        let tick=|price:u64,time:i64| crate::oracle::OraclePrice {price,publish_time:time,posted_slot:time as u64};
        assert!(sample_sol_collateral(&mut quote,&mut history,tick(109_500_000_000_000,101),false).is_err());
        sample_sol_collateral(&mut quote,&mut history,tick(109_500_000_000_000,101),true).unwrap();
        assert_eq!(quote.value(2_000_000_000,101).unwrap(),219*USD);
        assert_eq!(quote.value(20_000_000_000,101).unwrap(),2190*USD);
        trader.slots[0].asset=quote.asset;trader.slots[0].margin=2_000_000_000;
        trader.slots[0].entry_value=219*USD;trader.slots[0].opened_source_slot=101;
        guard.start(&trader,&history,0).unwrap();
        sample_sol_collateral(&mut quote,&mut history,tick(100_000_000_000_000,102),false).unwrap();
        sample_sol_collateral(&mut quote,&mut history,tick(120_000_000_000_000,103),false).unwrap();
        guard.observe(&trader,&history).unwrap();
        assert_eq!(guard.floors[0].value,200*USD);
        pending(&mut trader,&mut guard,103);
        sample_sol_collateral(&mut quote,&mut history,tick(90_000_000_000_000,104),false).unwrap();
        guard.observe(&trader,&history).unwrap();
        assert_eq!(guard.settled_value(&trader,0).unwrap(),200*USD);
        let sequence=history.sequence;
        sample_sol_collateral(&mut quote,&mut history,tick(90_000_000_000_000,104),false).unwrap();
        assert_eq!(history.sequence,sequence);
        assert!(quote.value(2_000_000_000,108).is_err());
        assert!(sol_collateral_value(1,0).is_err());
        assert!(sol_collateral_value(u64::MAX,u64::MAX).is_err());
    }
    fn setup(is_quote:bool)->(ErTrader,CollateralGuard,CollateralHistory,ErQuote) {
        let owner=Pubkey::new_unique();let config=Pubkey::new_unique();let asset=Pubkey::new_unique();
        let quote=ErQuote {config,asset,is_quote,base_reserve:1_000*USD,quote_reserve:1_000*USD,
            #[cfg(feature="mainnet")]
            mainnet:crate::MainnetCollateralContext {real_quote_reserve:1_000*USD,pool_source_slot:100,
                usd_per_quote_atom_e21:1_000_000_000_000_000,conversion_publish_time:100,conversion_source_slot:100},
            fee_bps:[20,5,0],publish_time:100,source_slot:100,..ErQuote::default()};
        let mut history=CollateralHistory {config,asset,is_quote,sequence:0,source_slot:0,publish_time:0,last_gap:0,last_gap_start:0,
            #[cfg(feature="mainnet")] decimals:6,
            samples:vec![CollateralSample::default();HISTORY],bump:0};
        history.record(&quote).unwrap();
        let mut trader=ErTrader {owner,config,..ErTrader::default()};
        trader.slots[0]=ErSlot {state:ER_OPEN,kind:ER_TRADE,nonce:1,asset,margin:10*USD,
            entry_value:quote.value(10*USD,100).unwrap(),opened_source_slot:100,..ErSlot::default()};
        let mut guard=CollateralGuard {owner,config,floors:CollateralGuard::empty_floors(),bump:0};
        guard.start(&trader,&history,0).unwrap();
        (trader,guard,history,quote)
    }
    fn record(history:&mut CollateralHistory,quote:&mut ErQuote,reserve:u64,time:i64) {
        quote.quote_reserve=reserve;quote.publish_time=time;quote.source_slot+=1;
        #[cfg(feature="mainnet")]
        { quote.mainnet.real_quote_reserve=reserve;quote.mainnet.conversion_publish_time=time;
            quote.mainnet.conversion_source_slot=quote.source_slot;quote.mainnet.pool_source_slot=quote.source_slot; }
        history.record(quote).unwrap();
    }
    fn pending(trader:&mut ErTrader,guard:&mut CollateralGuard,time:i64) {
        trader.slots[0].state=ER_PENDING;trader.slots[0].closed_at=time;
        trader.slots[0].exit=101_000_000_000_000;trader.slots[0].close_seq=1;
        guard.freeze(trader,0).unwrap();
    }
    #[test]
    fn dump_recovery_never_restores_the_winning_basis() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        record(&mut history,&mut quote,200*USD,101);
        let low=history.protected_value(history.samples[2],&trader.slots[0],&guard.floors[0]).unwrap();
        record(&mut history,&mut quote,1_000*USD,102);
        guard.observe(&trader,&history).unwrap();assert_eq!(guard.floors[0].value,low);
        pending(&mut trader,&mut guard,102);
        assert!(guard.settled_value(&trader,0).is_err(),"must cover the closing second before committing");
        record(&mut history,&mut quote,2_000*USD,103);
        guard.observe(&trader,&history).unwrap();assert_eq!(guard.settled_value(&trader,0).unwrap(),low);
        let profit=crate::math::pnl(low,100_000_000_000_000,101_000_000_000_000,10,true).unwrap().1;
        assert_eq!(profit,195_177); // includes the existing deadband and profit haircut
    }
    #[test]
    fn delayed_preclose_dip_is_included_but_postclose_dip_is_not() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        pending(&mut trader,&mut guard,102);
        record(&mut history,&mut quote,300*USD,101);
        let low=history.protected_value(history.samples[2],&trader.slots[0],&guard.floors[0]).unwrap();
        record(&mut history,&mut quote,1_000*USD,102);
        record(&mut history,&mut quote,100*USD,103);
        guard.observe(&trader,&history).unwrap();assert_eq!(guard.settled_value(&trader,0).unwrap(),low);
        record(&mut history,&mut quote,0,200);
        guard.observe(&trader,&history).unwrap();assert_eq!(guard.settled_value(&trader,0).unwrap(),low);
    }
    #[test]
    fn zero_liquidity_latches_zero_through_recovery() {
        let (trader,mut guard,mut history,mut quote)=setup(false);
        record(&mut history,&mut quote,0,101);guard.observe(&trader,&history).unwrap();
        record(&mut history,&mut quote,1_000*USD,102);guard.observe(&trader,&history).unwrap();
        assert_eq!(guard.floors[0].value,0);
    }
    #[test]
    fn each_bag_preserves_its_entry_backing_when_applying_the_floor() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        trader.slots[1]=ErSlot {nonce:2,margin:500*USD,entry_value:quote.value(500*USD,100).unwrap(),..trader.slots[0]};
        guard.start(&trader,&history,1).unwrap();
        quote.base_reserve=2_000*USD;record(&mut history,&mut quote,800*USD,101);
        guard.observe(&trader,&history).unwrap();
        #[cfg(not(feature="mainnet"))]
        {
            assert_eq!(guard.floors[0].value,quote.value(10*USD,101).unwrap());
            assert_eq!(guard.floors[1].value,quote.value(500*USD,101).unwrap());
        }
        #[cfg(feature="mainnet")]
        for i in [0,1] { assert_eq!(guard.floors[i].value,trader.slots[i].entry_value*2/5); }
        assert_ne!(guard.floors[0].value*50,guard.floors[1].value);
    }
    #[test]
    fn future_entries_do_not_inherit_old_lows_and_assets_do_not_cross() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        record(&mut history,&mut quote,50*USD,101);guard.observe(&trader,&history).unwrap();
        record(&mut history,&mut quote,1_000*USD,102);
        trader.slots[0].nonce=2;trader.slots[0].opened_source_slot=quote.source_slot;
        guard.start(&trader,&history,0).unwrap();assert_eq!(guard.floors[0].value,trader.slots[0].entry_value);
        let old=guard.floors[0].value;
        history.asset=Pubkey::new_unique();history.samples[3].quote=0;
        guard.observe(&trader,&history).unwrap();assert_eq!(guard.floors[0].value,old);
        guard.owner=Pubkey::new_unique();assert!(guard.observe(&trader,&history).is_err());
    }
    #[test]
    fn quote_tokens_and_increasing_tokens_never_exceed_entry() {
        for is_quote in [false,true] {
            let (trader,mut guard,mut history,mut quote)=setup(is_quote);
            record(&mut history,&mut quote,5_000*USD,101);guard.observe(&trader,&history).unwrap();
            assert_eq!(guard.floors[0].value,trader.slots[0].entry_value);
        }
    }
    #[test]
    fn new_live_samples_cannot_clear_a_history_gap_but_close_still_freezes() {
        for ring_overflow in [false,true] {
            let (trader,mut guard,mut history,mut quote)=setup(false);
            if ring_overflow {for _ in 0..65 {record(&mut history,&mut quote,1_000*USD,101);}}
            else {record(&mut history,&mut quote,1_000*USD,120);}
            guard.observe(&trader,&history).unwrap();assert!(guard.floors[0].incomplete);
            record(&mut history,&mut quote,2_000*USD,121);guard.observe(&trader,&history).unwrap();
            assert!(guard.floors[0].incomplete);
            let mut closed=trader;closed.slots[0].state=ER_PENDING;closed.slots[0].closed_at=121;closed.slots[0].close_seq=1;
            guard.freeze(&closed,0).unwrap();
            assert!(guard.settled_value(&closed,0).is_err(),"exit freezes but unverified payouts stay blocked");
        }
    }
    #[test]
    fn publisher_recovery_binds_exact_intent_and_never_raises_floor() {
        let (mut trader,mut guard,history,_)=setup(false);
        guard.floors[0].incomplete=true;
        pending(&mut trader,&mut guard,105);
        let value=guard.floors[0].value/2;
        let valid=CollateralRecovery{slot:0,nonce:1,opened_source_slot:100,closed_at:105,
            exit:trader.slots[0].exit,value,evidence_hash:[1;32]};
        for bad in [CollateralRecovery{nonce:2,..valid},CollateralRecovery{slot:1,..valid},
            CollateralRecovery{opened_source_slot:101,..valid},CollateralRecovery{closed_at:106,..valid},
            CollateralRecovery{exit:valid.exit+1,..valid},CollateralRecovery{evidence_hash:[0;32],..valid},
            CollateralRecovery{value:guard.floors[0].value+1,..valid}] {
            assert!(guard.recover(&trader,&history,bad).is_err());
            assert!(guard.floors[0].incomplete);
        }
        guard.recover(&trader,&history,valid).unwrap();
        assert_eq!(guard.settled_value(&trader,0).unwrap(),value);
        assert!(guard.recover(&trader,&history,valid).is_err(),"completed recovery cannot be replayed");
        assert_eq!(trader.slots[0].exit,valid.exit);
        assert_eq!(trader.slots[0].closed_at,105);
    }
    #[test]
    fn persisted_floor_rejects_nonce_and_close_snapshot_replay() {
        let (mut trader,mut guard,mut history,mut quote)=setup(false);
        record(&mut history,&mut quote,400*USD,101);guard.observe(&trader,&history).unwrap();
        pending(&mut trader,&mut guard,101);record(&mut history,&mut quote,1_000*USD,102);
        guard.observe(&trader,&history).unwrap();
        let mut bytes=vec![];guard.try_serialize(&mut bytes).unwrap();
        assert_eq!(bytes.len(),8+CollateralGuard::INIT_SPACE);
        let saved=CollateralGuard::try_deserialize(&mut &bytes[..]).unwrap();
        assert_eq!(saved.settled_value(&trader,0).unwrap(),guard.floors[0].value);
        trader.slots[0].nonce+=1;assert!(saved.settled_value(&trader,0).is_err());trader.slots[0].nonce-=1;
        trader.slots[0].exit+=1;assert!(saved.settled_value(&trader,0).is_err());
        let mut bytes=vec![];history.try_serialize(&mut bytes).unwrap();assert_eq!(bytes.len(),8+CollateralHistory::INIT_SPACE);
    }
}
