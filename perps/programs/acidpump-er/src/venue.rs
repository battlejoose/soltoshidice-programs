//! Venues: additional token-backed perps markets inside one deployment.
//!
//! The root venue is the original SDICE market. It keeps every original
//! singleton address. A created venue copies the root's operating identities
//! into its own `Config` at `[b"config", primary_mint]` and namespaces each
//! per-venue PDA with that config key. Feeds, registered assets, collateral
//! quotes, the gas reserve and the ER commit fee payer stay shared.
//!
//! A venue settles losses into its own primary token exactly like the root,
//! with own-token burns and SDICE purchases credited to the root payout pool.
//! Pending source FIFO claims suspend new revenue distributions.
use crate::*;

/// Seed component that is empty for the root venue. `[prefix, ns, rest..]`
/// then hashes exactly like the original `[prefix, rest..]`.
#[derive(Clone, Copy)]
pub struct Ns {
    key: [u8; 32],
    root: bool,
}
impl AsRef<[u8]> for Ns {
    fn as_ref(&self) -> &[u8] {
        if self.root { &[] } else { &self.key }
    }
}
/// Namespace of every config-scoped account: traders, custody, risk, markets, pools.
pub fn ns(config: Pubkey) -> Ns {
    Ns { key: config.to_bytes(), root: config == ROOT_CONFIG }
}
/// Positions are created where only the pool is known.
pub fn pool_ns(pool: Pubkey) -> Ns {
    Ns { key: pool.to_bytes(), root: pool == ROOT_POOL }
}
pub fn is_root(config: &Pubkey) -> bool {
    *config == ROOT_CONFIG
}
/// Collateral quotes and histories are maintained once, under the root config.
/// A venue-scoped account may use them; nothing else may substitute.
pub fn shared(owner_config: Pubkey, quote_config: Pubkey) -> bool {
    quote_config == ROOT_CONFIG || quote_config == owner_config
}
pub fn pool_address(config: Pubkey) -> Pubkey {
    if is_root(&config) { return ROOT_POOL; }
    Pubkey::find_program_address(&[b"pool", config.as_ref(), &0u16.to_le_bytes()], &crate::ID).0
}
/// Explicit seed lists (delegation, PDA signing) never contain an empty seed.
pub fn ns_seeds<'a>(prefix: &'a [u8], ns: &'a Ns, rest: &[&'a [u8]]) -> Vec<&'a [u8]> {
    let mut out = Vec::with_capacity(rest.len() + 2);
    out.push(prefix);
    if !ns.root { out.push(ns.as_ref()); }
    out.extend_from_slice(rest);
    out
}

/// Venue loss allocation in basis points of each realised loss.
pub const VENUE_POOL_BPS: u16 = 9_000;
pub const VENUE_STAKER_BPS: u16 = 700;
pub const VENUE_BURN_BPS: u16 = 100;
pub const VENUE_GAS_BPS: u16 = 100;
pub const VENUE_BUYBACK_BPS: u16 = 100;

/// Accounts of one venue loss conversion (see `pump::route_venue`).
pub struct VenueRoute<'a,'info> {
    pub pool:&'a Account<'info,Pool>,
    pub authority:&'a UncheckedAccount<'info>,
    pub from:&'a Account<'info,Asset>,
    pub perp:&'a Account<'info,Asset>,
    pub gas:&'a Account<'info,Asset>,
    pub source:&'a InterfaceAccount<'info,TokenAccount>,
    pub perp_vault:&'a InterfaceAccount<'info,TokenAccount>,
    pub gas_vault:&'a InterfaceAccount<'info,TokenAccount>,
    pub quote:&'a InterfaceAccount<'info,TokenAccount>,
    pub buyback:&'a InterfaceAccount<'info,TokenAccount>,
    pub program:&'a UncheckedAccount<'info>,
}

#[account]
#[derive(InitSpace)]
pub struct Venue {
    pub config: Pubkey,
    pub pool: Pubkey,
    pub mint: Pubkey,
    pub asset: Pubkey,
    pub creator: Pubkey,
    pub created_at: i64,
    pub pool_bps: u16,
    pub staker_bps: u16,
    pub burn_bps: u16,
    pub gas_bps: u16,
    pub buyback_bps: u16,
    /// Swap-quote atoms moved into the buyback vault by settlements.
    pub buyback_received: u64,
    /// Swap-quote atoms spent on SDICE by `route_buyback`.
    pub buyback_spent: u64,
    /// Historical SDICE burned before pool-size tokenomics; never incremented now.
    pub sdice_burned: u64,
    pub bump: u8,
}

#[event]
pub struct VenueCreated {
    pub config: Pubkey,
    pub venue: Pubkey,
    pub pool: Pubkey,
    pub mint: Pubkey,
    pub creator: Pubkey,
}
#[event]
pub struct VenueLossSplit {
    pub position: Pubkey,
    pub venue: Pubkey,
    pub loss: u64,
    pub token_tokens: u64,
    pub gas_tokens: u64,
    pub buyback_tokens: u64,
    pub token_bought: u64,
    pub sol_received: u64,
    pub buyback_quote: u64,
}
#[event]
pub struct SdiceBuyback {
    pub venue: Pubkey,
    pub quote_spent: u64,
    pub sdice_burned: u64,
    pub root_treasury: u64,
}

#[event]
pub struct SdicePoolContribution {
    pub venue: Pubkey,
    pub quote_spent: u64,
    pub sdice_contributed: u64,
    pub root_treasury: u64,
}

pub fn create_venue(ctx: Context<CreateVenue>) -> Result<()> {
    #[cfg(feature="mainnet")]
    {
        crate::mainnet_config::validate_asset(&ctx.accounts.asset)?;
        // A venue needs an executable buy route, not merely a collateral sale.
        // Token-quoted primary buys are not supported by the reviewed adapter.
        require!(ctx.accounts.asset.quote_mint==crate::mainnet_pump::SOL,MixError::Binding);
    }
    let root = &ctx.accounts.root;
    let c = &mut ctx.accounts.config;
    c.admin = root.admin;
    c.publisher = root.publisher;
    c.quote_mint = root.quote_mint;
    // Mainnet provisioning creates inventories, risk and routes before opening.
    c.paused = cfg!(feature="mainnet");
    c.bump = ctx.bumps.config;
    c.next_settlement_seq = 1;
    let p = &mut ctx.accounts.pool;
    p.id = 0;
    p.settlement = ctx.accounts.asset.key();
    p.authority_bump = ctx.bumps.authority;
    p.bump = ctx.bumps.pool;
    let v = &mut ctx.accounts.venue;
    v.config = c.key();
    v.pool = p.key();
    v.mint = ctx.accounts.mint.key();
    v.asset = ctx.accounts.asset.key();
    v.creator = ctx.accounts.creator.key();
    v.created_at = Clock::get()?.unix_timestamp;
    v.pool_bps = VENUE_POOL_BPS;
    v.staker_bps = VENUE_STAKER_BPS;
    v.burn_bps = VENUE_BURN_BPS;
    v.gas_bps = VENUE_GAS_BPS;
    v.buyback_bps = VENUE_BUYBACK_BPS;
    v.bump = ctx.bumps.venue;
    emit!(VenueCreated { config: c.key(), venue: v.key(), pool: p.key(), mint: v.mint, creator: v.creator });
    Ok(())
}

/// Spend a venue's accumulated contribution quote on SDICE and credit exactly
/// the purchase output to root payout inventory. Only the publisher sets minimum output, so a
/// third party cannot force a sandwiched purchase.
pub fn route_buyback<'info>(ctx: Context<'info, RouteBuyback<'info>>, amount: u64, min_out: u64) -> Result<()> {
    let a = &ctx.accounts;
    // A source queue still defers its allocated contribution. Root claims must
    // not block incoming payout liquidity. No pre-existing root funds are spent.
    require!(!math::queue_pending(a.pool.queue_usd,a.pool.queue_head,a.pool.queue_tail)?,MixError::QueueOrder);
    let available = pump::token_amount(&a.buyback_vault.to_account_info())?;
    let amount = if amount == 0 { available } else { amount };
    require!(amount > 0 && amount <= available && min_out > 0, MixError::Amount);
    // Root valuation binds the SDICE pool that the buy leg must use.
    pump::validate_pool(&a.sdice, &a.sdice_valuation)?;
    let quote0 = pump::token_amount(&a.root_quote.to_account_info())?;
    let sdice0 = pump::token_amount(&a.root_vault.to_account_info())?;
    let venue_pool = a.pool.key();
    let venue_seeds: &[&[u8]] = &[b"authority", venue_pool.as_ref(), &[a.pool.authority_bump]];
    let ix = anchor_spl::token::spl_token::instruction::transfer(&anchor_spl::token::ID, &a.buyback_vault.key(),
        &a.root_quote.key(), &a.authority.key(), &[], amount)?;
    anchor_lang::solana_program::program::invoke_signed(&ix, &[a.buyback_vault.to_account_info(),
        a.root_quote.to_account_info(), a.authority.to_account_info(), a.token_program.to_account_info()], &[venue_seeds])?;
    require!(pump::token_amount(&a.root_quote.to_account_info())? == quote0.checked_add(amount).ok_or(MixError::Arithmetic)?, MixError::Binding);
    pump::buy_into(&a.root_pool, &a.root_authority, &a.sdice, &a.root_vault, &a.root_quote, &a.pump_program,
        ctx.remaining_accounts, amount, min_out)?;
    // Only the transferred buyback quote may be spent; root quote dust is untouched.
    require!(pump::token_amount(&a.root_quote.to_account_info())? >= quote0, MixError::Binding);
    let out = pump::token_amount(&a.root_vault.to_account_info())?.checked_sub(sdice0).ok_or(MixError::Arithmetic)?;
    require!(out >= min_out, MixError::Quote);
    let root_treasury=a.root_inventory.treasury.checked_add(out).ok_or(MixError::Arithmetic)?;
    ctx.accounts.root_inventory.treasury=root_treasury;
    let v = &mut ctx.accounts.venue;
    v.buyback_spent = v.buyback_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
    // Preserve the historical event decoder; this transaction burns zero SDICE.
    emit!(SdiceBuyback { venue: v.key(), quote_spent: amount, sdice_burned: 0, root_treasury });
    emit!(SdicePoolContribution { venue: v.key(), quote_spent: amount, sdice_contributed: out, root_treasury });
    Ok(())
}

#[derive(Accounts)]
pub struct CreateVenue<'info> {
    #[account(mut)]
    pub admin: Signer<'info>,
    /// CHECK: Recorded identity of the wallet that requested the market.
    pub creator: UncheckedAccount<'info>,
    // Like token registration, market creation is co-signed by the admin
    // (the registrar service); the publisher cannot create venues.
    #[account(seeds=[b"config"],bump=root.bump,has_one=admin)]
    pub root: Box<Account<'info, Config>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=root_pool.bump)]
    pub root_pool: Box<Account<'info, Pool>>,
    // Only a registered, collateral-enabled token with a real swap route can
    // back a venue. The root's own primary token cannot be backed twice.
    #[account(seeds=[b"asset",asset.mint.as_ref()],bump=asset.bump,
        constraint=asset.collateral_enabled && asset.key()!=root_pool.settlement && asset.dex_pool!=Pubkey::default()
            && asset.mint!=anchor_spl::token::spl_token::native_mint::ID && asset.mint!=root.quote_mint @ MixError::Binding)]
    pub asset: Box<Account<'info, Asset>>,
    #[account(address=asset.mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(init,payer=admin,space=8+Config::INIT_SPACE,seeds=[b"config",mint.key().as_ref()],bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(init,payer=admin,space=8+Venue::INIT_SPACE,seeds=[b"venue",config.key().as_ref()],bump)]
    pub venue: Box<Account<'info, Venue>>,
    #[account(init,payer=admin,space=8+Pool::INIT_SPACE,seeds=[b"pool",config.key().as_ref(),&0u16.to_le_bytes()],bump)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: The venue pool's signing authority PDA.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(address=pump::swap_quote_mint(root.quote_mint),mint::token_program=quote_program)]
    pub swap_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=anchor_spl::token::ID)]
    pub quote_program: Interface<'info, TokenInterface>,
    #[account(init,payer=admin,seeds=[pump::intermediate_seed(),pool.key().as_ref()],bump,
        token::mint=swap_mint,token::authority=authority,token::token_program=quote_program)]
    pub quote_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(init,payer=admin,seeds=[b"buyback",pool.key().as_ref()],bump,
        token::mint=swap_mint,token::authority=authority,token::token_program=quote_program)]
    pub buyback_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    // Mainnet trades route through wrapped SOL but claims pay USDC. These
    // vaults must be separate so a payout cannot consume buyback/fee quote.
    #[cfg(feature="mainnet")]
    #[account(address=root.quote_mint,mint::token_program=quote_program)]
    pub payout_mint: Box<InterfaceAccount<'info, Mint>>,
    #[cfg(feature="mainnet")]
    #[account(init,payer=admin,seeds=[b"quote",pool.key().as_ref()],bump,
        token::mint=payout_mint,token::authority=authority,token::token_program=quote_program)]
    pub payout_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub system_program: Program<'info, System>,
    #[cfg(feature="mainnet")]
    pub execution: BaseExecution<'info>,
}

#[derive(Accounts)]
pub struct RouteBuyback<'info> {
    pub publisher: Signer<'info>,
    #[account(seeds=[b"config"],bump=root.bump,has_one=publisher)]
    pub root: Box<Account<'info, Config>>,
    #[account(mut,seeds=[b"venue",venue.config.as_ref()],bump=venue.bump)]
    pub venue: Box<Account<'info, Venue>>,
    #[account(address=venue.pool)]
    pub pool: Box<Account<'info, Pool>>,
    /// CHECK: Venue pool authority; signs only the buyback-vault transfer.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)]
    pub authority: UncheckedAccount<'info>,
    #[account(mut,seeds=[b"buyback",pool.key().as_ref()],bump,token::authority=authority,token::token_program=token_program)]
    pub buyback_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=root_pool.bump)]
    pub root_pool: Box<Account<'info, Pool>>,
    /// CHECK: Root pool authority; signs only the fixed SDICE buy.
    #[account(mut,seeds=[b"authority",root_pool.key().as_ref()],bump=root_pool.authority_bump)]
    pub root_authority: UncheckedAccount<'info>,
    #[account(address=root_pool.settlement)]
    pub sdice: Box<Account<'info, Asset>>,
    #[account(mut,address=sdice.mint,mint::token_program=sdice_program)]
    pub sdice_mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=sdice.token_program)]
    pub sdice_program: Interface<'info, TokenInterface>,
    pub sdice_valuation: Valuation<'info>,
    #[account(mut,seeds=[b"inventory",root_pool.key().as_ref(),sdice.key().as_ref()],bump=root_inventory.bump,
        constraint=root_inventory.pool==root_pool.key() && root_inventory.asset==sdice.key() @ MixError::Binding)]
    pub root_inventory: Box<Account<'info, Inventory>>,
    #[account(mut,seeds=[b"vault",root_pool.key().as_ref(),sdice.key().as_ref()],bump,token::authority=root_authority,
        token::mint=sdice_mint,token::token_program=sdice_program)]
    pub root_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,seeds=[pump::intermediate_seed(),root_pool.key().as_ref()],bump,
        token::mint=buyback_vault.mint,token::authority=root_authority,token::token_program=token_program)]
    pub root_quote: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address=anchor_spl::token::ID)]
    pub token_program: Interface<'info, TokenInterface>,
    /// CHECK: Fixed executable PumpSwap program, also enforced inside the CPI.
    #[account(address=pump::ID,executable)]
    pub pump_program: UncheckedAccount<'info>,
    #[cfg(feature="mainnet")]
    pub execution: BaseExecution<'info>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_namespace_reproduces_original_addresses() {
        let owner = Pubkey::new_unique();
        let root = ns(ROOT_CONFIG);
        assert!(root.as_ref().is_empty());
        for (prefix, rest) in [(&b"er-trader"[..], owner.to_bytes().to_vec()), (b"er-risk", vec![]), (b"pool", 0u16.to_le_bytes().to_vec())] {
            let original = Pubkey::find_program_address(&[prefix, &rest], &crate::ID).0;
            assert_eq!(Pubkey::find_program_address(&[prefix, root.as_ref(), &rest], &crate::ID).0, original);
            assert_eq!(Pubkey::find_program_address(&ns_seeds(prefix, &root, &[&rest]), &crate::ID).0, original);
        }
        assert_eq!(Pubkey::find_program_address(&[b"config"], &crate::ID).0, ROOT_CONFIG);
        assert_eq!(pool_address(ROOT_CONFIG), ROOT_POOL);
        assert!(pool_ns(ROOT_POOL).as_ref().is_empty());
    }
    #[test]
    fn venue_namespaces_are_disjoint_from_the_root_and_each_other() {
        let owner = Pubkey::new_unique();
        let a = Pubkey::new_unique();
        let b = Pubkey::new_unique();
        let trader = |config: Pubkey| Pubkey::find_program_address(&[b"er-trader", ns(config).as_ref(), owner.as_ref()], &crate::ID).0;
        assert_ne!(trader(ROOT_CONFIG), trader(a));
        assert_ne!(trader(a), trader(b));
        assert_eq!(ns_seeds(b"er-trader", &ns(a), &[owner.as_ref()]).len(), 3);
        assert_eq!(ns_seeds(b"er-trader", &ns(ROOT_CONFIG), &[owner.as_ref()]).len(), 2);
        assert!(shared(a, ROOT_CONFIG) && shared(a, a) && !shared(a, b));
        assert_ne!(pool_address(a), ROOT_POOL);
    }
    #[test]
    fn venue_allocation_sums_to_the_whole_loss() {
        assert_eq!(VENUE_POOL_BPS + VENUE_STAKER_BPS + VENUE_BURN_BPS + VENUE_GAS_BPS + VENUE_BUYBACK_BPS, 10_000);
    }
}
