//! Acidpump: mixed custody, shared core pool and USD FIFO; isolated deployment profiles.
use anchor_lang::prelude::*;
use ephemeral_rollups_sdk::anchor::ephemeral;

/// Venue namespace seed: empty for the root venue, the venue config otherwise.
/// Being a macro keeps it out of the IDL's static PDA descriptions, which
/// cannot express it; clients derive these addresses explicitly.
macro_rules! vns { ($config:expr) => { crate::venue::ns($config).as_ref() }; }
/// Position namespace seed, derived from the pool where only the pool is known.
macro_rules! pns { ($pool:expr) => { crate::venue::pool_ns($pool).as_ref() }; }
mod heap;
// Allocate from the bottom: small SDK callbacks also work in the default
// 32 KiB frame. Large financial calls still request 128 KiB explicitly.
#[cfg(all(feature="custom-heap",target_os="solana"))]
#[global_allocator]
static ALLOC: heap::AscendingHeap = heap::AscendingHeap;
use anchor_spl::token_interface::{self, Mint, TokenAccount, TokenInterface, TransferChecked};
pub mod math;
pub mod emissions;
pub mod risk;
pub mod stake_backing;
pub mod core_migration;
pub use core_migration::*;
pub mod recovery;
pub use recovery::*;
pub mod wallet_capacity;
pub use wallet_capacity::*;
pub use risk::UserExposure;
#[cfg(not(feature="mainnet"))]
pub mod pump;
#[cfg(feature="mainnet")]
#[path="pump_mainnet.rs"]
pub mod pump;
pub mod mainnet_pump;
pub mod mainnet_curve;
pub mod mainnet_collateral;
pub mod mainnet_quote_path;
pub mod mainnet_config;
pub mod deployment_config {
    use super::*;
    include!(concat!(env!("OUT_DIR"), "/launch_constants.rs"));
}
pub use mainnet_collateral::{MainnetCollateralContext, MainnetCollateralObservation, SolUsdQuote};
pub mod ownership;
pub mod engine;
pub mod oracle;
pub mod basebridge;
pub mod settlement;
pub mod economics;
pub use economics::*;
pub mod fee_surplus;
pub use fee_surplus::*;
pub mod collateral_floor;
pub use collateral_floor::*;
pub use engine::*;
pub use basebridge::*;
pub use ownership::{OwnershipPool, OwnershipTrader, OwnershipReceipt, OwnershipAwarded};
pub mod venue;
pub use venue::*;
#[cfg(all(feature="mainnet",feature="devnet"))]
compile_error!("mainnet and devnet builds must be isolated");
// Program ID, root config and root pool are generated together by build.rs.
include!(concat!(env!("OUT_DIR"), "/program_identity.rs"));

// Keep the retired PUMP feed and accounts valid for closing old positions.
pub const CORE_MARKET_COUNT: u16 = 5;
pub const ACTIVE_MARKET_COUNT: u16 = 4;

#[ephemeral]
#[program]
pub mod acidpump_er {
    use super::*;
pub fn withdraw_gas_surplus(ctx: Context<WithdrawGasSurplus>, amount:u64) -> Result<()> { fee_surplus::withdraw(ctx,amount) }
pub fn deposit_gas_revenue(ctx: Context<DepositGasRevenue>, amount:u64) -> Result<()> { fee_surplus::deposit_revenue(ctx,amount) }
pub fn initialize_economics_v1(ctx: Context<InitializeEconomicsV1>, deadband:bool) -> Result<()> { economics::initialize(ctx,deadband) }
pub fn er_initialize_collateral_history(ctx: Context<InitializeCollateralHistory>) -> Result<()> { collateral_floor::initialize_history(ctx) }
pub fn er_initialize_collateral_guard(ctx: Context<InitializeCollateralGuard>) -> Result<()> { collateral_floor::initialize_guard(ctx) }
pub fn er_delegate_collateral_history(ctx: Context<DelegateCollateralHistory>) -> Result<()> { collateral_floor::delegate_history(ctx) }
pub fn er_delegate_collateral_guard(ctx: Context<DelegateCollateralGuard>) -> Result<()> { collateral_floor::delegate_guard(ctx) }
pub fn er_observe_collateral(ctx: Context<ObserveCollateral>) -> Result<()> { collateral_floor::observe(ctx) }
pub fn er_recover_collateral(ctx: Context<RecoverCollateral>, recovery: CollateralRecovery) -> Result<()> { collateral_floor::recover(ctx,recovery) }
pub fn er_waive_unverified_profit(ctx: Context<CloseErPosition>, slot:u8, nonce:u64, expires:i64) -> Result<()> { engine::waive_unverified_profit(ctx,slot,nonce,expires) }
pub fn er_initialize_risk(ctx: Context<InitializeErRisk>) -> Result<()> { engine::initialize_risk(ctx) }
pub fn er_initialize_quote(ctx: Context<InitializeErQuote>) -> Result<()> { engine::initialize_quote(ctx) }
pub fn er_initialize_trader(ctx: Context<InitializeErTrader>) -> Result<()> { engine::initialize_trader(ctx) }
pub fn er_commit_prepaid(ctx:Context<CommitPrepaidErTrader>)->Result<()> {engine::commit_prepaid_trader(ctx)}
pub fn er_release_prepaid(ctx:Context<ReleasePrepaidErTrader>)->Result<()> {engine::release_prepaid_trader(ctx)}
pub fn withdraw_prepaid_gas(ctx:Context<WithdrawPrepaidGas>)->Result<()> {engine::withdraw_prepaid_gas(ctx)}
pub fn er_initialize_fee_payer(ctx: Context<InitializeErFeePayer>, lamports: u64) -> Result<()> { engine::initialize_fee_payer(ctx, lamports) }
pub fn er_delegate_risk(ctx: Context<DelegateErRisk>) -> Result<()> { engine::delegate_risk(ctx) }
pub fn er_delegate_quote(ctx: Context<DelegateErQuote>) -> Result<()> { engine::delegate_quote(ctx) }
pub fn er_delegate_trader(ctx: Context<DelegateErTrader>) -> Result<()> { engine::delegate_trader(ctx) }
pub fn er_delegate_fee_payer(ctx: Context<DelegateErFeePayer>) -> Result<()> { engine::delegate_fee_payer(ctx) }
pub fn er_publish_quote(ctx: Context<PublishErQuote>, base_reserve: u64, quote_reserve: u64, fee_bps: [u16;3], publish_time: i64, source_slot: u64) -> Result<()> {
    engine::publish_quote(ctx, base_reserve, quote_reserve, fee_bps, publish_time, source_slot)
}
pub fn er_publish_mainnet_quote(ctx: Context<PublishErQuote>, observation: MainnetCollateralObservation) -> Result<()> {
    mainnet_collateral::publish(ctx, observation)
}
pub fn er_sample(ctx: Context<SampleErPrices>) -> Result<()> { engine::sample(ctx) }
pub fn er_sync(ctx: Context<SyncErTrader>) -> Result<()> { engine::sync(ctx) }
pub fn er_release_legacy_trader(ctx: Context<ReleaseLegacyTrader>) -> Result<()> { core_migration::release(ctx) }
pub fn er_release_trader(ctx: Context<ReleaseLegacyTrader>) -> Result<()> { recovery::release(ctx) }
pub fn er_release_wallet_capacity(ctx: Context<ReleaseWalletCapacity>) -> Result<()> { wallet_capacity::release(ctx) }
pub fn migrate_wallet_capacity(ctx: Context<MigrateWalletCapacity>) -> Result<()> { wallet_capacity::migrate(ctx) }
pub fn request_trader_release(ctx: Context<RequestCoreUndelegation>) -> Result<()> { recovery::request_release(ctx) }
pub fn withdraw_released_collateral(ctx: Context<WithdrawReleasedCollateral>, amount:u64) -> Result<()> { recovery::withdraw(ctx,amount) }
pub fn request_core_undelegation(ctx: Context<RequestCoreUndelegation>) -> Result<()> { core_migration::request_release(ctx) }
pub fn recover_core_trader(ctx: Context<RecoverCoreTrader>, expected_hash:[u8;32]) -> Result<()> { core_migration::recover(ctx,expected_hash) }
pub fn migrate_core_account(ctx: Context<MigrateCoreAccount>, kind:u8) -> Result<()> { core_migration::migrate(ctx,kind) }
pub fn redelegate_core_trader(ctx: Context<RedelegateCoreTrader>) -> Result<()> { core_migration::redelegate(ctx) }
pub fn er_change_core_stake(ctx: Context<ChangeErCoreStake>, amount:u64, unstake:bool) -> Result<()> { engine::change_core_stake(ctx,amount,unstake) }
pub fn er_open(ctx: Context<OpenErPosition>, slot: u8, nonce: u64, market: u16, amount: u64, leverage: u16, long: bool, limit_price: u64, min_value: u64, max_value: u64, expires: i64) -> Result<()> {
    engine::open(ctx, engine::OpenRequest { slot, nonce, market, amount, leverage, long, limit_price, min_value, max_value, expires })
}
pub fn er_observe(ctx: Context<ObserveErTrader>) -> Result<()> { engine::observe(ctx) }
pub fn er_force_core_close(ctx: Context<CloseErPosition>, slot: u8, nonce: u64, expires: i64) -> Result<()> { engine::force_core_close(ctx, slot, nonce, expires) }
pub fn er_close(ctx: Context<CloseErPosition>, slot: u8, nonce: u64, expires: i64) -> Result<()> { engine::close(ctx, slot, nonce, expires) }
pub fn er_forfeit_history_gap(ctx: Context<ForfeitErHistoryGap>, slot: u8, nonce: u64, expires: i64) -> Result<()> { engine::forfeit_history_gap(ctx, slot, nonce, expires) }
pub fn er_withdraw(ctx: Context<WithdrawErTrader>, slot: u8, nonce: u64, amount: u64, expires: i64) -> Result<()> { engine::withdraw(ctx, slot, nonce, amount, expires) }
pub fn er_commit(ctx: Context<CommitErTrader>) -> Result<()> { engine::commit_trader(ctx) }

    pub fn initialize(ctx: Context<Initialize>, publisher: Pubkey) -> Result<()> {
        require!(cfg!(feature = "devnet") || cfg!(feature = "mainnet"), MixError::ProductionDisabled);
        #[cfg(feature="mainnet")]
        mainnet_config::initialize(ctx.accounts.admin.key(), publisher, ctx.accounts.quote_mint.key(),
            *ctx.accounts.quote_mint.to_account_info().owner)?;
        let c = &mut ctx.accounts.config;
        c.admin = ctx.accounts.admin.key();
        c.publisher = publisher;
        require!(ctx.accounts.quote_mint.decimals == 6, MixError::Binding);
        #[cfg(not(feature="mainnet"))]
        validate_mint(&ctx.accounts.quote_mint.to_account_info())?;
        c.quote_mint = ctx.accounts.quote_mint.key();
        c.paused = cfg!(feature="mainnet");
        c.bump = ctx.bumps.config;
        c.next_settlement_seq = 1;
        Ok(())
    }
    pub fn create_feed(ctx: Context<CreateFeed>, id: u16, symbol: [u8; 12]) -> Result<()> {
        require!(id < CORE_MARKET_COUNT || id >= 100, MixError::Binding);
        let f = &mut ctx.accounts.feed;
        f.id = id;
        f.symbol = symbol;
        f.bump = ctx.bumps.feed;
        Ok(())
    }
    pub fn register_asset(
        ctx: Context<RegisterAsset>,
        source_mint: Pubkey,
        source_pool: Pubkey,
        collateral_enabled: bool,
    ) -> Result<()> {
        #[cfg(not(feature="mainnet"))]
        require!(
            ctx.accounts.feed.id >= 100
                && (!collateral_enabled || (source_mint != Pubkey::default()
                && source_pool != Pubkey::default())),
            MixError::Binding
        );
        validate_mint(&ctx.accounts.mint.to_account_info())?;
        let a = &mut ctx.accounts.asset;
        a.feed = ctx.accounts.feed.key();
        a.id = ctx.accounts.feed.id;
        a.mint = ctx.accounts.mint.key();
        a.token_program = ctx.accounts.token_program.key();
        a.decimals = ctx.accounts.mint.decimals;
        a.source_mint = source_mint;
        a.source_pool = source_pool;
        a.bump = ctx.bumps.asset;
        a.quote_mint = pump::swap_quote_mint(ctx.accounts.config.quote_mint);
        a.dex_pool = ctx.accounts.valuation.dex_pool.key();
        a.collateral_enabled = collateral_enabled;
        #[cfg(feature="mainnet")]
        {
            a.quote_mint = pump::registration_quote_mint(a.mint,&ctx.accounts.valuation)?;
            if a.mint == mainnet_pump::SOL { a.dex_pool = Pubkey::default(); }
            mainnet_config::validate_asset(a)?;
        }
        pump::validate_pool(a, &ctx.accounts.valuation)?;
        Ok(())
    }
    pub fn publish(
        ctx: Context<Publish>,
        price: u64,
        timestamp: i64,
        source_slot: u64,
        base_reserve: u64,
        quote_reserve: u64,
        real_quote_reserve: u64,
        quote_usd: u64,
        quote_decimals: u8,
        fee_bps: [u16; 3],
    ) -> Result<()> {
        require!(cfg!(feature = "devnet"), MixError::ProductionDisabled);
        let f = &mut ctx.accounts.feed;
        let now = Clock::get()?.unix_timestamp;
        require!(
            price >= 2_000 && price <= 1_000_000_000_000_000_000,
            MixError::Quote
        );
        require!(
            timestamp <= now + 2 && timestamp >= now - 10,
            MixError::Stale
        );
        require!(
            timestamp >= f.publish_time && source_slot > f.source_slot,
            MixError::Replay
        );
        if f.id >= 100 {
            require!(
                base_reserve > 0
                    && quote_reserve > 0
                    && real_quote_reserve > 0
                    && quote_usd > 0
                    && quote_usd <= 1_000_000_000_000_000_000
                    && quote_decimals <= 9
                    && fee_bps.iter().map(|x| *x as u32).sum::<u32>() < 10_000,
                MixError::Quote
            );
        }
        f.price = price;
        f.publish_time = timestamp;
        f.source_slot = source_slot;
        f.base_reserve = base_reserve;
        f.quote_reserve = quote_reserve;
        f.real_quote_reserve = real_quote_reserve;
        f.quote_usd = quote_usd;
        f.quote_decimals = quote_decimals;
        f.fee_bps = fee_bps;
        f.sequence = f.sequence.checked_add(1).ok_or(MixError::Arithmetic)?;
        let sequence = f.sequence;
        f.observations[(sequence % 64) as usize] = Observation { sequence, price };
        Ok(())
    }
    pub fn create_pool(ctx: Context<CreatePool>, id: u16) -> Result<()> {
        require!(id == 0, MixError::Binding);
        #[cfg(feature="mainnet")]
        mainnet_config::validate_primary(&ctx.accounts.settlement)?;
        let p = &mut ctx.accounts.pool;
        p.id = id;
        p.bump = ctx.bumps.pool;
        require!((id == 0 && ctx.accounts.settlement.id == 102)
            || (id >= 100 && id == ctx.accounts.settlement.id), MixError::Binding);
        p.settlement = ctx.accounts.settlement.key();
        p.authority_bump = ctx.bumps.authority;
        Ok(())
    }
    pub fn create_market(ctx: Context<CreateMarket>) -> Result<()> {
        let id = ctx.accounts.feed.id;
        let p = &ctx.accounts.pool;
        require!(
            p.id == 0 && id < CORE_MARKET_COUNT,
            MixError::Binding
        );
        let m = &mut ctx.accounts.market;
        m.pool = p.key();
        m.feed = ctx.accounts.feed.key();
        m.id = id;
        m.long_cap = risk::OI_HEADROOM;
        m.short_cap = risk::OI_HEADROOM;
        m.cap_refreshed_at = Clock::get()?.unix_timestamp;
        m.bump = ctx.bumps.market;
        Ok(())
    }
    /// Enable only the configured core settlement asset as a bet token.
    /// Devnet's platform mint has no mainnet source token before launch.
    pub fn enable_platform_collateral(ctx: Context<EnablePlatformCollateral>) -> Result<()> {
        ctx.accounts.asset.collateral_enabled = true;
        Ok(())
    }
    /// Register only the configured six-decimal settlement quote as dollar collateral.
    pub fn register_quote_collateral(ctx: Context<RegisterQuoteCollateral>) -> Result<()> {
        #[cfg(not(feature="mainnet"))]
        validate_mint(&ctx.accounts.mint.to_account_info())?;
        #[cfg(feature="mainnet")]
        require_keys_eq!(ctx.accounts.mint.key(),mainnet_pump::USDC,MixError::Binding);
        let a=&mut ctx.accounts.asset;
        a.id=104; a.feed=ctx.accounts.feed.key(); a.mint=ctx.accounts.mint.key();
        a.quote_mint=ctx.accounts.config.quote_mint; a.token_program=anchor_spl::token::ID;
        a.decimals=6; a.collateral_enabled=true; a.bump=ctx.bumps.asset;
        #[cfg(feature="mainnet")]
        { a.quote_mint=mainnet_pump::SOL; a.dex_pool=pump::USDC_POOL; }
        Ok(())
    }
    pub fn enable_sol_collateral(ctx: Context<EnableSolCollateral>) -> Result<()> {
        ctx.accounts.asset.collateral_enabled=true;
        Ok(())
    }
    pub fn initialize_sol_intermediate(_ctx:Context<InitializeSolIntermediate>)->Result<()> {
        require!(cfg!(feature="mainnet"),MixError::ProductionDisabled);
        Ok(())
    }
    pub fn refresh_limits(ctx: Context<RefreshLimits>) -> Result<()> {
        let now = Clock::get()?.unix_timestamp;
        let m = &mut ctx.accounts.market;
        require!(now >= m.cap_refreshed_at + risk::REFRESH_SECONDS, MixError::SameSlot);
        m.long_cap = risk::refreshed_cap(m.long_oi, m.long_cap)?;
        m.short_cap = risk::refreshed_cap(m.short_oi, m.short_cap)?;
        m.cap_refreshed_at = now;
        Ok(())
    }
    pub fn create_inventory(ctx: Context<CreateInventory>) -> Result<()> {
        let i = &mut ctx.accounts.inventory;
        i.pool = ctx.accounts.pool.key();
        i.asset = ctx.accounts.asset.key();
        i.bump = ctx.bumps.inventory;
        Ok(())
    }
    /// Trading is only available through delegated ER state.
    pub fn open(_ctx: Context<Open>, _nonce: u64, _amount: u64, _leverage: u16,
        _long: bool, _limit_price: u64, _min_value: u64, _max_value: u64) -> Result<()> {
        err!(MixError::Unauthorized)
    }
    pub fn initialize_custody(ctx: Context<InitializeCustody>) -> Result<()> { basebridge::initialize_custody(ctx) }
    pub fn deposit_collateral(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> { basebridge::deposit_collateral(ctx, amount) }
    pub fn prepare_settlement(ctx: Context<PrepareSettlement>, nonce: u64, slot: u8) -> Result<()> { basebridge::prepare_settlement(ctx, nonce, slot) }
    pub fn settle_withdrawal(ctx: Context<SettleWithdrawal>, nonce: u64, slot: u8) -> Result<()> { basebridge::settle_withdrawal(ctx, nonce, slot) }
    pub fn initialize_exposure(ctx: Context<InitializeExposure>) -> Result<()> {
        let e = &mut ctx.accounts.exposure;
        e.owner = ctx.accounts.owner.key();
        e.bump = ctx.bumps.exposure;
        Ok(())
    }
    /// Reinvest completed position/receipt rent in the next trade, atomically.
    /// The following PrepareSettlement and InitializeOwnership are mandatory: a sponsored
    /// account's rent cannot be withdrawn as a free SOL grant.
    pub fn recycle_position(ctx: Context<RecyclePosition>, new_nonce: u64) -> Result<()> {
        use anchor_lang::Discriminator;
        use solana_instructions_sysvar::{load_current_index_checked, load_instruction_at_checked};
        let x = &ctx.accounts.position;
        require!(matches!(x.state, 1 | 2) && x.remaining == 0, MixError::NoClaim);
        require!(x.profit == 0 || x.queue_seq < ctx.accounts.pool.queue_head, MixError::QueueOrder);
        require!(ctx.accounts.ownership_receipt.awarded, MixError::Replay);
        // Strict fresh destinations prevent init_if_needed from refunding a
        // sponsored account without buying replacement storage.
        let rent = Rent::get()?;
        require!(x.to_account_info().data_len() == 8 + Position::INIT_SPACE
            && ctx.accounts.ownership_receipt.to_account_info().data_len() == 8 + OwnershipReceipt::INIT_SPACE
            && x.to_account_info().lamports() == rent.minimum_balance(8 + Position::INIT_SPACE)
            && ctx.accounts.ownership_receipt.to_account_info().lamports() == rent.minimum_balance(8 + OwnershipReceipt::INIT_SPACE), MixError::Binding);
        require!(ctx.accounts.next_position.data_is_empty() && ctx.accounts.next_position.lamports() == 0
            && ctx.accounts.next_receipt.data_is_empty() && ctx.accounts.next_receipt.lamports() == 0, MixError::Binding);
        let owner = ctx.accounts.owner.key();
        let executor = ctx.accounts.executor.key();
        let next_position = Pubkey::find_program_address(&[b"position", venue::pool_ns(ctx.accounts.pool.key()).as_ref(), owner.as_ref(), &new_nonce.to_le_bytes()], &crate::ID).0;
        require!(next_position != x.key(), MixError::Binding);
        let sysvar = ctx.accounts.instructions.to_account_info();
        let index = load_current_index_checked(&sysvar)? as usize;
        // A wrapper must not invoke recycling several times before one PrepareSettlement,
        // which would release multiple sponsored rents but reinvest only one.
        let current = load_instruction_at_checked(index, &sysvar)?;
        require!(current.program_id == crate::ID && current.data.starts_with(crate::instruction::RecyclePosition::DISCRIMINATOR), MixError::Unauthorized);
        let open = load_instruction_at_checked(index + 1, &sysvar)?;
        require!(open.program_id == crate::ID && open.data.starts_with(crate::instruction::PrepareSettlement::DISCRIMINATOR), MixError::Binding);
        require!(open.data.get(8..16) == Some(new_nonce.to_le_bytes().as_slice()), MixError::Binding);
        require!(open.accounts.first().is_some_and(|a| a.pubkey == executor && a.is_signer)
            && open.accounts.get(1).is_some_and(|a| a.pubkey == owner)
            && open.accounts.get(8).is_some_and(|a| a.pubkey == next_position), MixError::Binding);
        let initialize = load_instruction_at_checked(index + 2, &sysvar)?;
        require!(initialize.program_id == crate::ID && initialize.data.starts_with(crate::instruction::InitializeOwnership::DISCRIMINATOR), MixError::Binding);
        require!(initialize.accounts.first().is_some_and(|a| a.pubkey == executor && a.is_signer)
            && initialize.accounts.get(1).is_some_and(|a| a.pubkey == next_position)
            && initialize.accounts.get(3).is_some_and(|a| a.pubkey == ctx.accounts.next_receipt.key()), MixError::Binding);
        let mut data = Vec::new();
        x.try_serialize(&mut data)?;
        emit!(PositionRecycled { position: x.key(), next_position, data, issued: ctx.accounts.ownership_receipt.issued });
        Ok(())
    }
    /// Frozen ER intents cannot be re-observed or repriced on the base layer.
    pub fn observe(_ctx: Context<Observe>) -> Result<()> { err!(MixError::Unauthorized) }
    pub fn close<'info>(ctx: Context<'info, Close<'info>>, sell_count: u8, min_swap_out: u64, market_accounts:u8, fee_sell_count:u8, perp_buy_count:u8, min_perp:u64, min_gas:u64) -> Result<()> {
        let _=(ctx,sell_count,min_swap_out,market_accounts,fee_sell_count,perp_buy_count,min_perp,min_gas);
        err!(MixError::ProductionDisabled)
    }
    pub fn close_backed<'info>(ctx: Context<'info, Close<'info>>, sell_count: u8, min_swap_out: u64, market_accounts:u8, fee_sell_count:u8, perp_buy_count:u8, min_perp:u64, min_gas:u64, sol_quote:SolUsdQuote) -> Result<()> {
        require_keys_eq!(ctx.accounts.quote_publisher.key(), ctx.accounts.bridge.config.publisher, MixError::Unauthorized);
        settlement::settle(ctx, sell_count, min_swap_out, market_accounts, fee_sell_count, perp_buy_count, min_perp, min_gas, sol_quote)
    }
    pub fn redeem<'info>(ctx: Context<'info, Redeem<'info>>, amount:u64, max_debit:u64, expires:i64, convert:bool, sell_count:u8, min_out:u64) -> Result<()> {
        // Mainnet claims are paid exclusively by redeem_usdc's verified route.
        require!(!cfg!(feature="mainnet"), MixError::ProductionDisabled);
        let now=Clock::get()?.unix_timestamp;
        require!(expires>=now && expires<=now+120,MixError::QuoteExpired);
        let x=&mut ctx.accounts.position;
        let pool=&mut ctx.accounts.pool;
        require!(x.state!=0 && x.remaining>0,MixError::NoClaim);
        require!(x.queue_seq==pool.queue_head,MixError::QueueOrder);
        require_keys_eq!(ctx.accounts.asset.key(),pool.settlement,MixError::Binding);
        require!(amount>0 && amount<=ctx.accounts.inventory.treasury,MixError::Inventory);
        let mut debit=pump::sell_value(amount,&ctx.accounts.asset,&ctx.accounts.valuation)?;
        require!(debit>0 && debit<=x.remaining && debit<=max_debit,MixError::ClaimLimit);
        let left=x.remaining-debit;
        if math::dust(left,x.profit){debit=debit.checked_add(left).ok_or(MixError::Arithmetic)?;}
        require!(debit<=max_debit,MixError::ClaimLimit);
        let converted_out=if convert {
            require_keys_neq!(ctx.accounts.asset.key(),ctx.accounts.collateral.key(),MixError::Binding);
            require!(min_out>0,MixError::Quote);
            let dest=ctx.accounts.conversion_vault.as_ref().ok_or(MixError::Binding)?;
            require_keys_eq!(dest.key(),Pubkey::find_program_address(&[b"vault",pool.key().as_ref(),ctx.accounts.collateral.key().as_ref()],&crate::ID).0,MixError::Binding);
            let out=pump::swap_loss(pool,&ctx.accounts.authority,&ctx.accounts.asset,&ctx.accounts.collateral,
                &ctx.accounts.vault,dest,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
                ctx.remaining_accounts,sell_count,amount,min_out)?;
            let mint=ctx.accounts.collateral_mint.as_ref().ok_or(MixError::Binding)?;
            let program=ctx.accounts.collateral_program.as_ref().ok_or(MixError::Binding)?;
            let target=ctx.accounts.converted_destination.as_ref().ok_or(MixError::Binding)?;
            require!(mint.key()==ctx.accounts.collateral.mint && program.key()==ctx.accounts.collateral.token_program
                && target.owner==x.owner && target.mint==mint.key(),MixError::Binding);
            pay(pool,dest,target,mint,program,&ctx.accounts.authority,out)?;
            out
        }else{
            require_keys_eq!(ctx.accounts.destination.owner,x.owner,MixError::Unauthorized);
            pay(pool,&ctx.accounts.vault,&ctx.accounts.destination,&ctx.accounts.mint,&ctx.accounts.token_program,&ctx.accounts.authority,amount)?;
            0
        };
        ctx.accounts.inventory.treasury-=amount;
        ctx.accounts.inventory.paid=ctx.accounts.inventory.paid.checked_add(amount).ok_or(MixError::Arithmetic)?;
        x.remaining-=debit;
        pool.queue_usd=pool.queue_usd.checked_sub(debit).ok_or(MixError::Arithmetic)?;
        if x.remaining==0 {pool.queue_head=pool.queue_head.checked_add(1).ok_or(MixError::Arithmetic)?;}
        emit!(Redeemed{position:x.key(),asset:ctx.accounts.asset.key(),amount,debit_usd:debit,converted_out});
        Ok(())
    }
    /// Permissionless FIFO settlement. All sizing and destination checks are on chain;
    /// a keeper pays network fees but cannot redirect funds or choose the claim debit.
    pub fn redeem_usdc<'info>(ctx: Context<'info, RedeemUsdc<'info>>) -> Result<()> {
        #[cfg(feature="mainnet")]
        { return pump::redeem_usdc(ctx); }
        #[cfg(not(feature="mainnet"))]
        {
        let x=&mut ctx.accounts.position;
        let pool=&mut ctx.accounts.pool;
        require!(x.state!=0 && x.remaining>0,MixError::NoClaim);
        require!(x.queue_seq==pool.queue_head,MixError::QueueOrder);
        let available=ctx.accounts.inventory.treasury;
        require!(available>0,MixError::Inventory);
        let (base,quote,fees)=pump::validate_pool(&ctx.accounts.asset,&ctx.accounts.valuation)?;
        // Find the maximum executable sale that cannot overpay this USD claim.
        let mut lo=0u64;let mut hi=available;
        while lo<hi {
            let mid=lo+(hi-lo)/2+(hi-lo)%2;
            if pump::sell_from_reserves(mid,base,quote,fees)?<=x.remaining {lo=mid;} else {hi=mid-1;}
        }
        let expected=pump::sell_from_reserves(lo,base,quote,fees)?;
        require!(lo>0 && expected>0,MixError::Inventory);
        let out=pump::sell_to_quote(pool,&ctx.accounts.authority,&ctx.accounts.asset,
            &ctx.accounts.vault,&ctx.accounts.quote_vault,&ctx.accounts.pump_program,
            ctx.remaining_accounts,lo,expected)?;
        require!(out<=x.remaining,MixError::ClaimLimit);
        let left=x.remaining-out;
        // Only settle unspendable rounding; every other unpaid dollar stays in FIFO.
        let debit=if math::dust(left,x.profit) {x.remaining} else {out};
        pay(pool,&ctx.accounts.quote_vault,&ctx.accounts.destination,&ctx.accounts.quote_mint,
            &ctx.accounts.quote_program,&ctx.accounts.authority,out)?;
        ctx.accounts.inventory.treasury-=lo;
        ctx.accounts.inventory.paid=ctx.accounts.inventory.paid.checked_add(lo).ok_or(MixError::Arithmetic)?;
        x.remaining-=debit;
        pool.queue_usd=pool.queue_usd.checked_sub(debit).ok_or(MixError::Arithmetic)?;
        if x.remaining==0 {pool.queue_head=pool.queue_head.checked_add(1).ok_or(MixError::Arithmetic)?;}
        emit!(Redeemed{position:x.key(),asset:ctx.accounts.asset.key(),amount:lo,debit_usd:debit,converted_out:out});
        Ok(())
        }
    }
    pub fn sweep_dust(ctx: Context<SweepDust>) -> Result<()> {
        let x = &mut ctx.accounts.position;
        let p = &mut ctx.accounts.pool;
        require!(
            x.state != 0 && x.queue_seq == p.queue_head && math::dust(x.remaining, x.profit),
            MixError::ClaimLimit
        );
        p.queue_usd = p
            .queue_usd
            .checked_sub(x.remaining)
            .ok_or(MixError::Arithmetic)?;
        x.remaining = 0;
        p.queue_head = p.queue_head.checked_add(1).ok_or(MixError::Arithmetic)?;
        Ok(())
    }
    pub fn initialize_ownership_pool(ctx: Context<InitializeOwnershipPool>) -> Result<()> {
        let p = &mut ctx.accounts.ownership_pool;
        p.pool = ctx.accounts.pool.key();
        p.activated_at = Clock::get()?.unix_timestamp;
        p.bump = ctx.bumps.ownership_pool;
        Ok(())
    }
    pub fn initialize_ownership(ctx: Context<InitializeOwnership>) -> Result<()> {
        let x = &ctx.accounts.position;
        let t = &mut ctx.accounts.ownership_trader;
        if t.pool == Pubkey::default() {
            t.pool = x.pool; t.owner = x.owner; t.bump = ctx.bumps.ownership_trader;
        }
        let r = &mut ctx.accounts.ownership_receipt;
        if r.position == Pubkey::default() {
            r.position = x.key(); r.pool = x.pool; r.owner = x.owner; r.bump = ctx.bumps.ownership_receipt;
        }
        Ok(())
    }
    pub fn stake_ownership(ctx: Context<StakeOwnership>, amount: u128, unstake: bool) -> Result<()> {
        ownership::change_stake(&mut ctx.accounts.ownership_pool, &mut ctx.accounts.ownership_trader, &mut ctx.accounts.inventory, amount, unstake)
    }
    pub fn sweep_surplus(ctx: Context<SweepSurplus>) -> Result<()> {
        if ctx.accounts.pool.queue_head < ctx.accounts.pool.queue_tail { return Ok(()); }
        ownership::sweep_surplus(&mut ctx.accounts.ownership_pool,&mut ctx.accounts.inventory,ctx.accounts.pool.queue_usd,&ctx.accounts.settlement,&ctx.accounts.valuation)?;
        Ok(())
    }
    pub fn claim_ownership_rewards(ctx: Context<ClaimOwnershipRewards>) -> Result<()> {
        let p = &mut ctx.accounts.ownership_pool;
        let t = &mut ctx.accounts.ownership_trader;
        ownership::distribute(p)?;
        ownership::accrue(p, t)?;
        let amount = t.pending;
        require!(amount > 0, MixError::NoClaim);
        p.reward_pool = p.reward_pool.checked_sub(amount).ok_or(MixError::Inventory)?;
        p.pending_liabilities = p.pending_liabilities.checked_sub(amount).ok_or(MixError::Inventory)?;
        t.pending = 0;
        t.claimed = t.claimed.checked_add(amount).ok_or(MixError::Arithmetic)?;
        pay(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.destination, &ctx.accounts.mint, &ctx.accounts.token_program, &ctx.accounts.authority, amount)
    }
    pub fn claim_ownership_developer(ctx: Context<ClaimOwnershipDeveloper>) -> Result<()> {
        let amount = ctx.accounts.ownership_pool.dev_fees;
        require!(amount > 0, MixError::NoClaim);
        ctx.accounts.ownership_pool.dev_fees = 0;
        pay(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.destination, &ctx.accounts.mint, &ctx.accounts.token_program, &ctx.accounts.authority, amount)
    }
    /// "Create a market": a new venue backed by a registered pump.fun token.
    pub fn create_venue(ctx: Context<CreateVenue>) -> Result<()> { venue::create_venue(ctx) }
    /// Buy and burn SDICE with a venue's 1% allocation, without spending root inventory.
    pub fn route_buyback<'info>(ctx: Context<'info, RouteBuyback<'info>>, amount: u64, min_out: u64) -> Result<()> { venue::route_buyback(ctx, amount, min_out) }
    pub fn pause(ctx: Context<Admin>, paused: bool) -> Result<()> {
        ctx.accounts.config.paused = paused;
        Ok(())
    }
    pub fn initialize_gas(ctx:Context<InitializeGas>)->Result<()> {
        ctx.accounts.gas_reserve.bump=ctx.bumps.gas_reserve;
        Ok(())
    }
    pub fn create_gas_vault(_ctx:Context<CreateGasVault>)->Result<()> {Ok(())}
    pub fn refill_sponsor(ctx:Context<RefillSponsor>,amount:u64)->Result<()> {
        let g=&mut ctx.accounts.gas_reserve;
        let day=Clock::get()?.unix_timestamp/86400;
        if g.day!=day {g.day=day;g.daily_spent=0;}
        require!(amount>0 && amount<=20_000_000 && ctx.accounts.publisher.lamports().checked_add(amount).ok_or(MixError::Arithmetic)?<=50_000_000, MixError::GasBudget);
        // Retain the seed-funded daily guard, but do not trap earned gas receipts.
        require!(g.daily_spent.checked_add(amount).ok_or(MixError::Arithmetic)?<=500_000_000
            || g.sol_spent.checked_add(amount).ok_or(MixError::Arithmetic)?<=g.sol_received,MixError::GasBudget);
        let rent=Rent::get()?.minimum_balance(8+GasReserve::INIT_SPACE);
        require!(g.to_account_info().lamports().saturating_sub(rent)>=amount,MixError::GasBudget);
        **g.to_account_info().try_borrow_mut_lamports()?-=amount;
        **ctx.accounts.publisher.to_account_info().try_borrow_mut_lamports()?+=amount;
        g.daily_spent+=amount;
        g.sol_spent=g.sol_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
        Ok(())
    }
}
pub fn crossed(x: &Position, f: &Feed) -> bool {
    x.bust_observed
        || math::crossed(f.price, x.bust, x.long)
        || f.observations
            .iter()
            .any(|o| o.sequence > x.cursor && math::crossed(o.price, x.bust, x.long))
}
fn fresh(f: &Feed, now: i64) -> Result<()> {
    let max_age = if f.id == 4 { 5 } else { 10 };
    require!(
        f.price > 0 && f.publish_time <= now + 2 && now - f.publish_time <= max_age,
        MixError::Stale
    );
    Ok(())
}
#[cfg(test)]
mod market_freshness_tests {
    use super::*;
    #[test]
    fn pump_expires_after_five_seconds_without_changing_other_markets() {
        let mut feed = Feed { id: 4, price: 2_000_000_000, publish_time: 100, ..Feed::default() };
        assert!(fresh(&feed, 105).is_ok());
        assert!(fresh(&feed, 106).is_err());
        for id in [0, 1, 2, 3, 100, 102] {
            feed.id = id;
            assert!(fresh(&feed, 110).is_ok());
            assert!(fresh(&feed, 111).is_err());
        }
        feed.id = 4;
        assert!(fresh(&feed, 98).is_ok());
        assert!(fresh(&feed, 97).is_err());
    }
}
fn validate_mint(info: &AccountInfo) -> Result<()> {
    use anchor_spl::token_2022::spl_token_2022::{
        extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions},
        state::Mint as SplMint,
    };
    let data = info.try_borrow_data()?;
    let mint = StateWithExtensions::<SplMint>::unpack(&data)?;
    require!(
        mint.base.decimals <= 9 && mint.base.freeze_authority.is_none(),
        MixError::Binding
    );
    for ext in mint.get_extension_types()? {
        require!(
            matches!(
                ext,
                ExtensionType::MetadataPointer | ExtensionType::TokenMetadata
            ),
            MixError::Binding
        );
    }
    Ok(())
}
fn pay<'info>(
    p: &Account<'info, Pool>,
    vault: &InterfaceAccount<'info, TokenAccount>,
    to: &InterfaceAccount<'info, TokenAccount>,
    mint: &InterfaceAccount<'info, Mint>,
    program: &Interface<'info, TokenInterface>,
    authority: &UncheckedAccount<'info>,
    amount: u64,
) -> Result<()> {
    if amount == 0 {
        return Ok(());
    }
    let key = p.key();
    let seeds: &[&[u8]] = &[b"authority", key.as_ref(), &[p.authority_bump]];
    token_interface::transfer_checked(
        CpiContext::new_with_signer(
            program.key(),
            TransferChecked {
                from: vault.to_account_info(),
                to: to.to_account_info(),
                mint: mint.to_account_info(),
                authority: authority.to_account_info(),
            },
            &[seeds],
        ),
        amount,
        mint.decimals,
    )
}

fn unwrap_gas<'info>(p:&Account<'info,Pool>,authority:&UncheckedAccount<'info>,vault:&InterfaceAccount<'info,TokenAccount>,mint:&InterfaceAccount<'info,Mint>,reserve:&Account<'info,GasReserve>,program:&Program<'info,anchor_spl::token::Token>,system:&Program<'info,System>)->Result<()> {
    use anchor_lang::solana_program::{program::invoke_signed,system_instruction};
    use anchor_spl::token::{self,CloseAccount,InitializeAccount3};
    require!(mint.key()==token::spl_token::native_mint::ID && vault.mint==mint.key() && vault.owner==authority.key(),MixError::Binding);
    let pool=p.key();
    let auth_seeds:&[&[u8]]=&[b"authority",pool.as_ref(),&[p.authority_bump]];
    let (_,bump)=Pubkey::find_program_address(&[b"gas-vault",pool.as_ref()],&crate::ID);
    let vault_seeds:&[&[u8]]=&[b"gas-vault",pool.as_ref(),&[bump]];
    let rent=Rent::get()?.minimum_balance(165);
    let native=vault.to_account_info().lamports().checked_sub(rent).ok_or(MixError::Arithmetic)?;
    token::close_account(CpiContext::new_with_signer(program.key(),CloseAccount{account:vault.to_account_info(),destination:authority.to_account_info(),authority:authority.to_account_info()},&[auth_seeds]))?;
    // Reuse the same rent deposit. The gas vault starts and ends empty, so no
    // user wallet funds its creation during settlement.
    invoke_signed(&system_instruction::create_account(&authority.key(),&vault.key(),rent,165,&token::ID),&[authority.to_account_info(),vault.to_account_info(),system.to_account_info()],&[auth_seeds,vault_seeds])?;
    token::initialize_account3(CpiContext::new(program.key(),InitializeAccount3{account:vault.to_account_info(),mint:mint.to_account_info(),authority:authority.to_account_info()}))?;
    invoke_signed(&system_instruction::transfer(&authority.key(),&reserve.key(),native),&[authority.to_account_info(),reserve.to_account_info(),system.to_account_info()],&[auth_seeds])?;
    Ok(())
}

#[account]
#[derive(InitSpace)]
pub struct GasReserve {
    pub sol_received:u64,
    pub sol_spent:u64,
    pub perp_bought:u64,
    pub day:i64,
    pub daily_spent:u64,
    pub bump:u8,
}
#[derive(Accounts)]
pub struct InitializeGas<'info> {
    #[account(mut)] pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Box<Account<'info,Config>>,
    #[account(init,payer=admin,space=8+GasReserve::INIT_SPACE,seeds=[b"gas-reserve"],bump)] pub gas_reserve:Box<Account<'info,GasReserve>>,
    pub system_program:Program<'info,System>,
}
#[derive(Accounts)]
pub struct CreateGasVault<'info> {
    #[account(mut)] pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Box<Account<'info,Config>>,
    pub pool:Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=anchor_spl::token::spl_token::native_mint::ID)] pub gas_mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(init,payer=admin,seeds=[b"gas-vault",pool.key().as_ref()],bump,token::mint=gas_mint,token::authority=authority)] pub gas_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    pub token_program:Program<'info,anchor_spl::token::Token>,
    pub system_program:Program<'info,System>,
}
#[derive(Accounts)]
pub struct RefillSponsor<'info> {
    #[account(mut)] pub publisher:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=publisher)] pub config:Box<Account<'info,Config>>,
    #[account(mut,seeds=[b"gas-reserve"],bump=gas_reserve.bump)] pub gas_reserve:Box<Account<'info,GasReserve>>,
}
#[derive(Accounts)]
pub struct LossRouting<'info> {
    #[account(mut,seeds=[b"gas-reserve"],bump=gas_reserve.bump)] pub gas_reserve:Box<Account<'info,GasReserve>>,
    /// Bound to the pool's own settlement token in the handler.
    pub perp:Box<Account<'info,Asset>>,
    #[account(mut,address=perp.mint)] pub perp_mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(address=perp.token_program)] pub perp_program:Interface<'info,TokenInterface>,
    // The core pool uses this same SPL vault as its settlement vault. SPL
    // accounts are changed by token CPIs, never serialized by this program.
    #[account(mut,dup)] pub perp_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    pub main_pool:Option<Box<Account<'info,Pool>>>,
    #[account(mut)] pub main_ownership_pool:Option<Box<Account<'info,OwnershipPool>>>,
    #[account(mut)] pub main_inventory:Option<Box<Account<'info,Inventory>>>,
    #[account(mut)] pub main_vault:Option<Box<InterfaceAccount<'info,TokenAccount>>>,
    #[account(constraint=gas_asset.mint==gas_mint.key() && gas_asset.id==103 @ MixError::Binding)] pub gas_asset:Box<Account<'info,Asset>>,
    #[account(address=anchor_spl::token::spl_token::native_mint::ID)] pub gas_mint:Box<InterfaceAccount<'info,Mint>>,
    #[account(mut)] pub gas_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    pub gas_program:Program<'info,anchor_spl::token::Token>,
    pub system_program:Program<'info,System>,
    /// Created venues only: the venue record and its SDICE buyback vault.
    #[account(mut)] pub venue:Option<Box<Account<'info,Venue>>>,
    #[account(mut)] pub buyback_vault:Option<Box<InterfaceAccount<'info,TokenAccount>>>,
}
#[event]
pub struct LossSplit {
    pub position:Pubkey,
    pub loss:u64,
    pub market_tokens:u64,
    pub gas_tokens:u64,
    pub perp_tokens:u64,
    pub perp_bought:u64,
    pub sol_received:u64,
}
#[event]
pub struct RevenueSplit {
    pub position:Pubkey,
    pub acid_received:u64,
    pub pool_tokens:u64,
    pub staker_tokens:u64,
    pub burned_tokens:u64,
}
#[account]
#[derive(InitSpace)]
pub struct Config {
    pub admin: Pubkey,
    pub publisher: Pubkey,
    pub quote_mint: Pubkey,
    pub paused: bool,
    pub bump: u8,
    pub next_settlement_seq: u64,
}
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace)]
pub struct Observation {
    pub sequence: u64,
    pub price: u64,
}
#[account]
#[derive(InitSpace)]
pub struct Feed {
    pub id: u16,
    pub symbol: [u8; 12],
    pub price: u64,
    pub publish_time: i64,
    pub source_slot: u64,
    pub base_reserve: u64,
    pub quote_reserve: u64,
    pub real_quote_reserve: u64,
    pub quote_usd: u64,
    pub quote_decimals: u8,
    pub fee_bps: [u16; 3],
    pub sequence: u64,
    pub observations: [Observation; 64],
    pub bump: u8,
}
impl Default for Feed {
    fn default() -> Self {
        Self {
            id: 0,
            symbol: [0; 12],
            price: 0,
            publish_time: 0,
            source_slot: 0,
            base_reserve: 0,
            quote_reserve: 0,
            real_quote_reserve: 0,
            quote_usd: 0,
            quote_decimals: 0,
            fee_bps: [0; 3],
            sequence: 0,
            observations: [Observation::default(); 64],
            bump: 0,
        }
    }
}
#[account]
#[derive(InitSpace)]
pub struct Asset {
    pub id: u16,
    pub feed: Pubkey,
    pub mint: Pubkey,
    pub token_program: Pubkey,
    pub source_mint: Pubkey,
    pub source_pool: Pubkey,
    pub quote_mint: Pubkey,
    pub dex_pool: Pubkey,
    pub collateral_enabled: bool,
    pub decimals: u8,
    pub bump: u8,
}
#[account]
#[derive(InitSpace)]
pub struct Pool {
    pub id: u16,
    pub settlement: Pubkey,
    pub authority_bump: u8,
    pub queue_head: u64,
    pub queue_tail: u64,
    pub queue_usd: u64,
    pub trades: u64,
    pub bump: u8,
}
#[account]
#[derive(InitSpace)]
pub struct Market {
    pub id: u16,
    pub pool: Pubkey,
    pub feed: Pubkey,
    pub long_oi: u64,
    pub short_oi: u64,
    pub long_cap: u64,
    pub short_cap: u64,
    pub cap_refreshed_at: i64,
    pub bump: u8,
}
#[account]
#[derive(InitSpace)]
pub struct Inventory {
    pub pool: Pubkey,
    pub asset: Pubkey,
    pub locked: u64,
    pub treasury: u64,
    pub losses: u64,
    pub paid: u64,
    pub dust: u64,
    pub bump: u8,
}
#[account]
#[derive(InitSpace)]
pub struct Position {
    pub owner: Pubkey,
    pub pool: Pubkey,
    pub market: Pubkey,
    pub collateral: Pubkey,
    pub nonce: u64,
    pub margin: u64,
    pub entry_value: u64,
    pub close_value: u64,
    pub entry: u64,
    pub bust: u64,
    pub leverage: u16,
    pub long: bool,
    pub state: u8,
    pub opened_at: i64,
    pub opened_slot: u64,
    pub cursor: u64,
    pub bust_observed: bool,
    pub closed_at: i64,
    pub exit: u64,
    pub returned: u64,
    pub profit: u64,
    pub loss: u64,
    pub swapped_out: u64,
    pub remaining: u64,
    pub queue_seq: u64,
    pub bump: u8,
    pub er_slot: u8,
    pub er_close_seq: u64,
    pub collateral_close_value: u64,
    pub coverage_tokens: u64,
    pub coverage_usd: u64,
    pub uncovered_usd: u64,
}

#[derive(Accounts)]
pub struct Initialize<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(init,payer=admin,space=8+Config::INIT_SPACE,seeds=[b"config"],bump)]
    pub config: Box<Account<'info, Config>>,
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub system_program: Program<'info, System>,
    #[cfg(feature="mainnet")]
    pub execution: BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct Admin<'info> {
    pub admin: Signer<'info>,
    /// The root or any venue config; each venue pauses independently.
    #[account(mut,has_one=admin)]
    pub config: Box<Account<'info, Config>>,
}
#[derive(Accounts)]
pub struct EnablePlatformCollateral<'info> {
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=pool.bump,constraint=pool.id==0 @ MixError::Binding)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,address=pool.settlement,constraint=asset.id==102 @ MixError::Binding)]
    pub asset: Box<Account<'info, Asset>>,
}
#[derive(Accounts)]
pub struct RegisterQuoteCollateral<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Box<Account<'info,Config>>,
    #[account(seeds=[b"feed",&104u16.to_le_bytes()],bump=feed.bump,constraint=feed.id==104 @ MixError::Binding)] pub feed: Box<Account<'info,Feed>>,
    #[account(address=config.quote_mint,mint::decimals=6,mint::token_program=token_program)] pub mint: Box<InterfaceAccount<'info,Mint>>,
    pub token_program: Program<'info,anchor_spl::token::Token>,
    #[account(init,payer=admin,space=8+Asset::INIT_SPACE,seeds=[b"asset",mint.key().as_ref()],bump)] pub asset: Box<Account<'info,Asset>>,
    pub system_program: Program<'info,System>,
}
#[derive(Accounts)]
pub struct EnableSolCollateral<'info> {
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Box<Account<'info,Config>>,
    #[account(mut,seeds=[b"asset",anchor_spl::token::spl_token::native_mint::ID.as_ref()],bump=asset.bump,
        constraint=asset.id==103 && asset.decimals==9 && asset.mint==anchor_spl::token::spl_token::native_mint::ID
        && asset.token_program==anchor_spl::token::ID && asset.quote_mint==pump::swap_quote_mint(config.quote_mint) @ MixError::Binding)] pub asset: Box<Account<'info,Asset>>,
}
#[derive(Accounts)]
#[instruction(id:u16)]
pub struct CreateFeed<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)]
    pub config: Box<Account<'info, Config>>,
    #[account(init,payer=admin,space=8+Feed::INIT_SPACE,seeds=[b"feed".as_ref(),&id.to_le_bytes()],bump)]
    pub feed: Box<Account<'info, Feed>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct RegisterAsset<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,constraint=admin.key()==config.admin || (cfg!(feature="devnet") && admin.key()==config.publisher) @ MixError::Unauthorized)]
    pub config: Box<Account<'info, Config>>,
    pub feed: Box<Account<'info, Feed>>,
    #[account(init,payer=admin,space=8+Asset::INIT_SPACE,seeds=[b"asset",mint.key().as_ref()],bump)]
    pub asset: Box<Account<'info, Asset>>,
    #[account(mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Interface<'info, TokenInterface>,
    pub valuation: Valuation<'info>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct Publish<'info> {
    pub publisher: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=publisher)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut)]
    pub feed: Box<Account<'info, Feed>>,
}
#[derive(Accounts)]
#[instruction(id:u16)]
pub struct CreatePool<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(init,payer=admin,space=8+Pool::INIT_SPACE,seeds=[b"pool".as_ref(),&id.to_le_bytes()],bump)]
    pub pool: Box<Account<'info, Pool>>,
    pub settlement: Box<Account<'info, Asset>>,
    /// CHECK: A separate system-owned PDA can fund PumpSwap account creation.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(address=config.quote_mint,mint::token_program=quote_program)]
    pub quote_mint: Box<InterfaceAccount<'info, Mint>>,
    pub quote_program: Interface<'info, TokenInterface>,
    #[account(init,payer=admin,seeds=[b"quote",pool.key().as_ref()],bump,token::mint=quote_mint,token::authority=authority,token::token_program=quote_program)]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct CreateMarket<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    #[account(has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(seeds=[b"pool",vns!(config.key()),&0u16.to_le_bytes()],bump=pool.bump)]
    pub pool: Box<Account<'info, Pool>>,
    pub feed: Box<Account<'info, Feed>>,
    #[account(init,payer=admin,space=8+Market::INIT_SPACE,seeds=[b"market",vns!(config.key()),feed.key().as_ref()],bump)]
    pub market: Box<Account<'info, Market>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct CreateInventory<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    pub asset: Box<Account<'info, Asset>>,
    #[account(address=asset.mint,mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)]
    pub token_program: Interface<'info, TokenInterface>,
    #[account(init,payer=payer,space=8+Inventory::INIT_SPACE,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump)]
    pub inventory: Box<Account<'info, Inventory>>,
    #[account(init,payer=payer,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
#[instruction(nonce:u64)]
pub struct Open<'info> {
    #[account(mut,seeds=[b"exposure",owner.key().as_ref()],bump=exposure.bump,has_one=owner)]
    pub exposure: Box<Account<'info, UserExposure>>,
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    pub valuation: Valuation<'info>,
    #[account(mut,has_one=pool,has_one=feed)]
    pub market: Box<Account<'info, Market>>,
    pub feed: Box<Account<'info, Feed>>,
    pub asset: Box<Account<'info, Asset>>,
    #[account(address=asset.feed)]
    pub collateral_feed: Box<Account<'info, Feed>>,
    #[account(address=asset.mint,mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)]
    pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump)]
    pub inventory: Box<Account<'info, Inventory>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,token::mint=mint,token::authority=owner,token::token_program=token_program)]
    pub source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(init,payer=owner,space=8+Position::INIT_SPACE,seeds=[b"position",owner.key().as_ref(),&nonce.to_le_bytes()],bump)]
    pub position: Box<Account<'info, Position>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct InitializeExposure<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    /// CHECK: Identity only. Trading still requires this owner's signature.
    pub owner: UncheckedAccount<'info>,
    #[account(init_if_needed,payer=payer,space=8+UserExposure::INIT_SPACE,seeds=[b"exposure",owner.key().as_ref()],bump)]
    pub exposure: Box<Account<'info, UserExposure>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
#[instruction(new_nonce:u64)]
pub struct RecyclePosition<'info> {
    #[account(mut)] pub executor: Signer<'info>,
    /// CHECK: Bound to the completed position and ownership receipt.
    pub owner: UncheckedAccount<'info>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut, close=executor, has_one=owner, has_one=pool)]
    pub position: Box<Account<'info, Position>>,
    #[account(mut, close=executor, seeds=[b"ownership-receipt",position.key().as_ref()], bump=ownership_receipt.bump, has_one=owner, has_one=pool, has_one=position)]
    pub ownership_receipt: Box<Account<'info, OwnershipReceipt>>,
    /// CHECK: Must be a fresh canonical PDA; following prepare buys the same rent.
    #[account(seeds=[b"position",pns!(pool.key()),owner.key().as_ref(),&new_nonce.to_le_bytes()],bump)]
    pub next_position: UncheckedAccount<'info>,
    /// CHECK: Must be a fresh canonical PDA; following initializer buys the same rent.
    #[account(seeds=[b"ownership-receipt",next_position.key().as_ref()],bump)]
    pub next_receipt: UncheckedAccount<'info>,
    /// CHECK: Canonical instructions sysvar, used to enforce atomic rent reinvestment.
    #[account(address=solana_instructions_sysvar::ID)]
    pub instructions: UncheckedAccount<'info>,
}
#[derive(Accounts)]
pub struct RefreshLimits<'info> {
    pub caller: Signer<'info>,
    #[account(mut)]
    pub market: Box<Account<'info, Market>>,
}
#[derive(Accounts)]
pub struct Observe<'info> {
    pub caller: Signer<'info>,
    #[account(has_one=feed)]
    pub market: Box<Account<'info, Market>>,
    pub feed: Box<Account<'info, Feed>>,
    #[account(mut,has_one=market)]
    pub position: Box<Account<'info, Position>>,
}
#[derive(Accounts)]
pub struct ClosePoolAccounts<'info> {
    pub caller: Signer<'info>,
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: Canonical pool signing authority, funded separately for Pump rent.
    #[account(mut,seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    pub valuation: Valuation<'info>,
}
#[derive(Accounts)]
pub struct Close<'info> {
    pub quote_publisher: Signer<'info>,
    pub pool_accounts: ClosePoolAccounts<'info>,
    #[account(address=pool_accounts.pool.settlement)]
    pub settlement: Box<Account<'info, Asset>>,
    #[account(mut)]
    pub settlement_inventory: Option<Box<Account<'info, Inventory>>>,
    #[account(mut)]
    pub settlement_vault: Option<Box<InterfaceAccount<'info,TokenAccount>>>,
    #[account(mut,seeds=[pump::intermediate_seed(),pool_accounts.pool.key().as_ref()],bump)]
    pub quote_vault: Box<InterfaceAccount<'info,TokenAccount>>,
    /// CHECK: Fixed executable program address, also enforced inside CPI.
    #[account(address=pump::ID,executable)]
    pub pump_program: UncheckedAccount<'info>,
    #[account(mut,constraint=market.pool==pool_accounts.pool.key() @ MixError::Binding)]
    pub market: Box<Account<'info, Market>>,
    // Replaces the unused live-price feed: exits are authenticated frozen ER
    // intents. One read-only account replaces another, preserving lock limits.
    #[cfg(feature="mainnet")]
    #[account(seeds=[b"economics-v1"],bump=economics.bump)]
    pub economics: Box<Account<'info, EconomicsV1>>,
    // Preserve the independent devnet settlement ABI until its own rollout.
    #[cfg(not(feature="mainnet"))]
    #[account(address=market.feed)]
    pub feed: Box<Account<'info, Feed>>,
    pub asset: Box<Account<'info, Asset>>,
    #[cfg(not(feature="mainnet"))]
    #[account(address=asset.feed)]
    pub collateral_feed: Box<Account<'info, Feed>>,
    #[account(mut,constraint=position.pool==pool_accounts.pool.key() @ MixError::Binding,has_one=market,constraint=position.collateral==asset.key() @ MixError::Binding)]
    pub position: Box<Account<'info, Position>>,
    pub ownership: CloseOwnership<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)]
    pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool_accounts.pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump)]
    pub inventory: Box<Account<'info, Inventory>>,
    #[account(mut,seeds=[b"vault",pool_accounts.pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=pool_accounts.authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    // ER settlement returns collateral to authenticated custody; withdrawal is
    // a separate instruction. Mainnet must not lock/create an unused wallet ATA.
    #[cfg(not(feature="mainnet"))]
    #[account(mut,token::mint=mint,token::token_program=token_program,constraint=destination.owner==position.owner @ MixError::Unauthorized)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    pub fees: LossRouting<'info>,
    pub bridge: ErCloseBridge<'info>,
}
#[derive(Accounts)]
pub struct SweepSurplus<'info> {
    pub caller: Signer<'info>,
    #[account(constraint=pool.id==0 @ MixError::Binding)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(address=pool.settlement)]
    pub settlement: Box<Account<'info, Asset>>,
    pub valuation: Valuation<'info>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),settlement.key().as_ref()],bump=inventory.bump)]
    pub inventory: Box<Account<'info, Inventory>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
}
#[derive(Accounts)]
pub struct Redeem<'info> {
    #[account(mut)]
    pub owner: Signer<'info>,
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: Canonical pool signing authority.
    #[account(mut,seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    pub valuation: Valuation<'info>,
    #[account(mut,has_one=owner,has_one=pool,has_one=collateral)]
    pub position: Box<Account<'info, Position>>,
    pub collateral: Box<Account<'info, Asset>>,
    #[account(has_one=feed)]
    pub asset: Box<Account<'info, Asset>>,
    pub feed: Box<Account<'info, Feed>>,
    #[account(address=asset.mint,mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)]
    pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump)]
    pub inventory: Box<Account<'info, Inventory>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,token::mint=mint,token::token_program=token_program)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,seeds=[b"quote",pool.key().as_ref()],bump)]
    pub quote_vault: Box<InterfaceAccount<'info,TokenAccount>>,
    /// CHECK: Fixed PumpSwap executable.
    #[account(address=pump::ID,executable)]
    pub pump_program: UncheckedAccount<'info>,
    #[account(mut)]
    pub conversion_vault: Option<Box<InterfaceAccount<'info,TokenAccount>>>,
    #[account(mut)]
    pub converted_destination: Option<Box<InterfaceAccount<'info,TokenAccount>>>,
    pub collateral_mint: Option<Box<InterfaceAccount<'info,Mint>>>,
    pub collateral_program: Option<Interface<'info,TokenInterface>>,

}
#[derive(Accounts)]
pub struct RedeemUsdc<'info> {
    pub caller: Signer<'info>,
    /// Any config: every venue shares the root's USD quote mint.
    pub config: Box<Account<'info,Config>>,
    #[account(mut)]
    pub pool: Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool authority signs only the fixed PumpSwap sell and owner payout.
    #[account(mut,seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(mut,has_one=pool)]
    pub position: Box<Account<'info,Position>>,
    #[account(address=pool.settlement,constraint=asset.quote_mint==pump::swap_quote_mint(config.quote_mint) @ MixError::Binding)]
    pub asset: Box<Account<'info,Asset>>,
    pub valuation: Valuation<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)]
    pub mint: Box<InterfaceAccount<'info,Mint>>,
    #[account(address=asset.token_program)]
    pub token_program: Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump)]
    pub inventory: Box<Account<'info,Inventory>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(address=config.quote_mint,mint::token_program=quote_program)]
    pub quote_mint: Box<InterfaceAccount<'info,Mint>>,
    #[account(address=anchor_spl::token::ID)]
    pub quote_program: Interface<'info,TokenInterface>,
    #[account(mut,seeds=[b"quote",pool.key().as_ref()],bump,token::mint=quote_mint,token::authority=authority,token::token_program=quote_program)]
    pub quote_vault: Box<InterfaceAccount<'info,TokenAccount>>,
    #[account(mut,token::mint=quote_mint,token::token_program=quote_program,constraint=destination.owner==position.owner @ MixError::Unauthorized)]
    pub destination: Box<InterfaceAccount<'info,TokenAccount>>,
    /// CHECK: Fixed executable PumpSwap program.
    #[account(address=pump::ID,executable)]
    pub pump_program: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    #[account(mut,seeds=[b"sol-quote",pool.key().as_ref()],bump)]
    pub sol_vault: Box<InterfaceAccount<'info,TokenAccount>>,
}
#[derive(Accounts)]
pub struct InitializeSolIntermediate<'info> {
    #[account(mut)] pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Box<Account<'info,Config>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=pool.bump)] pub pool:Box<Account<'info,Pool>>,
    /// CHECK: Canonical pool authority owns the isolated SOL intermediate.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority:UncheckedAccount<'info>,
    #[account(address=anchor_spl::token::spl_token::native_mint::ID)] pub mint:Box<InterfaceAccount<'info,Mint>>,
    pub token_program:Program<'info,anchor_spl::token::Token>,
    #[account(init,payer=admin,seeds=[b"sol-quote",pool.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub sol_vault:Box<InterfaceAccount<'info,TokenAccount>>,
    pub system_program:Program<'info,System>,
}
#[derive(Accounts)]
pub struct SweepDust<'info> {
    pub caller: Signer<'info>,
    #[account(mut)]
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,has_one=pool)]
    pub position: Box<Account<'info, Position>>,
}
#[derive(Accounts)]
pub struct InitializeOwnershipPool<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Box<Account<'info, Config>>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(init,payer=admin,space=8+OwnershipPool::INIT_SPACE,seeds=[b"ownership-pool",pool.key().as_ref()],bump)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct InitializeOwnership<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    pub position: Box<Account<'info, Position>>,
    #[account(init_if_needed,payer=payer,space=8+OwnershipTrader::INIT_SPACE,seeds=[b"ownership-trader",position.pool.as_ref(),position.owner.as_ref()],bump)]
    pub ownership_trader: Box<Account<'info, OwnershipTrader>>,
    #[account(init_if_needed,payer=payer,space=8+OwnershipReceipt::INIT_SPACE,seeds=[b"ownership-receipt",position.key().as_ref()],bump)]
    pub ownership_receipt: Box<Account<'info, OwnershipReceipt>>,
    pub system_program: Program<'info, System>,
}
#[derive(Accounts)]
pub struct StakeOwnership<'info> {
    pub owner: Signer<'info>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
    #[account(mut,seeds=[b"ownership-trader",pool.key().as_ref(),owner.key().as_ref()],bump=ownership_trader.bump,has_one=pool,has_one=owner)]
    pub ownership_trader: Box<Account<'info, OwnershipTrader>>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),pool.settlement.as_ref()],bump=inventory.bump,has_one=pool,constraint=inventory.asset==pool.settlement @ MixError::Binding)]
    pub inventory: Box<Account<'info, Inventory>>,
}
#[derive(Accounts)]
pub struct ClaimOwnershipRewards<'info> {
    pub owner: Signer<'info>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
    #[account(mut,seeds=[b"ownership-trader",pool.key().as_ref(),owner.key().as_ref()],bump=ownership_trader.bump,has_one=pool,has_one=owner)]
    pub ownership_trader: Box<Account<'info, OwnershipTrader>>,
    #[account(address=pool.settlement)] pub asset: Box<Account<'info, Asset>>,
    /// CHECK: Canonical authority signs the transfer.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority: UncheckedAccount<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)] pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,token::mint=mint,token::authority=owner,token::token_program=token_program)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
}
#[derive(Accounts)]
pub struct ClaimOwnershipDeveloper<'info> {
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config: Box<Account<'info, Config>>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,seeds=[b"ownership-pool",pool.key().as_ref()],bump=ownership_pool.bump,has_one=pool)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
    #[account(address=pool.settlement)] pub asset: Box<Account<'info, Asset>>,
    /// CHECK: Canonical authority signs the transfer.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority: UncheckedAccount<'info>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)] pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,token::mint=mint,token::authority=admin,token::token_program=token_program)]
    pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
}
#[event]
pub struct PositionRecycled {
    pub position: Pubkey,
    pub next_position: Pubkey,
    pub data: Vec<u8>,
    pub issued: u128,
}
#[event]
pub struct Settled {
    pub position: Pubkey,
    pub profit_usd: u64,
    pub loss: u64,
    pub returned: u64,
    pub liquidated: bool,
}
#[event]
pub struct Redeemed {
    pub position: Pubkey,
    pub asset: Pubkey,
    pub amount: u64,
    pub debit_usd: u64,
    pub converted_out: u64,
}
#[error_code]
pub enum MixError {
    #[msg("Devnet experiment only")]
    ProductionDisabled,
    #[msg("Arithmetic overflow")]
    Arithmetic,
    #[msg("Invalid account or token binding")]
    Binding,
    #[msg("Leverage must be 1 to 1000")]
    Leverage,
    #[msg("Price or collateral quote unavailable")]
    Quote,
    #[msg("Waiting for a fresh price")]
    Stale,
    #[msg("Replayed source observation")]
    Replay,
    #[msg("New positions paused")]
    Paused,
    #[msg("Invalid bet amount")]
    Amount,
    #[msg("Price moved beyond your limit")]
    PriceLimit,
    #[msg("Position or open-interest limit exceeded")]
    Exposure,
    #[msg("Position is already closed")]
    Closed,
    #[msg("Observation history gap; settlement cannot be proven")]
    History,
    #[msg("Unauthorized wallet")]
    Unauthorized,
    #[msg("Wait two slots before closing")]
    SameSlot,
    #[msg("Quote expired")]
    QuoteExpired,
    #[msg("No unpaid claim")]
    NoClaim,
    #[msg("Earlier FIFO claims must be paid first")]
    QueueOrder,
    #[msg("Insufficient free inventory")]
    Inventory,
    #[msg("Payout exceeds approved USD claim debit")]
    ClaimLimit,
    #[msg("A valid funded maker signature is required")]
    Maker,
    #[msg("Gas reserve spending limit or balance reached")]
    GasBudget,
}

#[derive(Accounts)]
pub struct Valuation<'info> {
    /// CHECK: Pump ownership, discriminator, PDA and registered mint binding checked below.
    pub dex_pool: UncheckedAccount<'info>,
    /// CHECK: Owner, authority and stored vault binding checked below.
    pub dex_base: UncheckedAccount<'info>,
    /// CHECK: Owner, authority and stored vault binding checked below.
    pub dex_quote: UncheckedAccount<'info>,
    /// CHECK: Pump fee-program owner, canonical PDA and discriminator checked below.
    pub fee_config: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Mint owner, authorities, precision and extensions are checked by the mainnet adapter.
    pub dex_mint: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Canonical Pump global configuration is checked by the mainnet adapter.
    pub global_config: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Pinned USDC/SOL route, validated against its mint and vaults.
    pub usd_pool: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Canonical Circle USDC mint and token program.
    pub usd_mint: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Bound USDC reserve account.
    pub usd_base: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Bound SOL reserve account.
    pub usd_quote: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Canonical source-venue global, checked by the mainnet curve/AMM adapter.
    pub source_global: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    /// CHECK: Canonical source-venue fee configuration, checked by that adapter.
    pub source_fees: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct CloseOwnership<'info> {
    #[account(mut,seeds=[b"exposure",exposure.owner.as_ref()],bump=exposure.bump)]
    pub exposure: Box<Account<'info, UserExposure>>,
    pub settlement_valuation: Valuation<'info>,
    #[account(mut,seeds=[b"ownership-pool",ownership_pool.pool.as_ref()],bump=ownership_pool.bump)]
    pub ownership_pool: Box<Account<'info, OwnershipPool>>,
    #[account(mut,seeds=[b"ownership-trader",ownership_trader.pool.as_ref(),ownership_trader.owner.as_ref()],bump=ownership_trader.bump)]
    pub ownership_trader: Box<Account<'info, OwnershipTrader>>,
    #[account(mut,seeds=[b"ownership-receipt",ownership_receipt.position.as_ref()],bump=ownership_receipt.bump)]
    pub ownership_receipt: Box<Account<'info, OwnershipReceipt>>,
}
impl CloseOwnership<'_> {
    #[inline(never)]
    fn validate(&self, x: &Account<Position>, pool: Pubkey) -> Result<()> {
        require!(self.exposure.owner == x.owner && self.ownership_pool.pool == pool && self.ownership_trader.pool == pool
            && self.ownership_trader.owner == x.owner && self.ownership_receipt.position == x.key()
            && self.ownership_receipt.owner == x.owner && self.ownership_receipt.pool == pool, MixError::Binding);
        Ok(())
    }
}

#[event]
pub struct CoreCoverageApplied {
    pub position: Pubkey, pub owner: Pubkey, pub closing_usd: u64,
    pub core_tokens: u64, pub covered_usd: u64, pub uncovered_usd: u64,
}
