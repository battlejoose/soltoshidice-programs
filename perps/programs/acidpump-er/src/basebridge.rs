//! Base-layer custody and settlement of canonical committed ER intents.
//! Custody.available includes reserved collateral: only losses and withdrawals
//! remove backing, while cumulative returned credits release ER reservations.
use crate::*;

pub const DELEGATION_OWNER: Pubkey = pubkey!("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh");

#[inline(never)]
pub fn committed_trader(info: &AccountInfo, owner: Pubkey, config: Pubkey, pool: Pubkey) -> Result<Box<ErTrader>> {
    require_keys_eq!(*info.owner, DELEGATION_OWNER, MixError::Unauthorized);
    require_keys_eq!(info.key(), Pubkey::find_program_address(&[b"er-trader", owner.as_ref()], &crate::ID).0, MixError::Binding);
    let data = info.try_borrow_data()?;
    let trader = decode_trader(&data)?;
    require!(trader.owner == owner && trader.config == config && trader.pool == pool, MixError::Binding);
    Ok(trader)
}

/// Nested arrays in ErTrader are boxed, so deserialization stays below SBF's stack limit.
#[inline(never)]
fn decode_trader(data: &[u8]) -> Result<Box<ErTrader>> {
    Ok(Box::new(ErTrader::try_deserialize(&mut &data[..])?))
}

pub fn committed_collateral_floor(info: &AccountInfo, trader: &ErTrader, slot: u8) -> Result<u64> {
    require_keys_eq!(*info.owner, DELEGATION_OWNER, MixError::Unauthorized);
    require_keys_eq!(info.key(), Pubkey::find_program_address(&[b"er-collateral-guard", trader.owner.as_ref()], &crate::ID).0, MixError::Binding);
    let data = info.try_borrow_data()?;
    let guard = CollateralGuard::try_deserialize(&mut &data[..])?;
    guard.settled_value(trader, slot)
}

/// The native Magic program exists only on ER. Custody credits must be minted
/// by actual base-layer token transfers, even if a validator permits writable clones.
#[derive(Accounts)]
pub struct BaseExecution<'info> {
    /// CHECK: Fixed address; an executable native Magic program identifies ER.
    #[account(address=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID,
        constraint=!magic_program.executable @ MixError::Unauthorized)]
    pub magic_program: UncheckedAccount<'info>,
}

#[derive(Accounts)]
pub struct ErCloseBridge<'info> {
    #[account(mut,seeds=[b"config"],bump=config.bump)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut,seeds=[b"er-custody",custody.owner.as_ref()],bump=custody.bump,has_one=config)]
    pub custody: Box<Account<'info, ErCustody>>,
    /// CHECK: Canonical PDA, DLP owner, position owner, pool and intent checked in handler.
    pub trader: UncheckedAccount<'info>,
    /// CHECK: Retained legacy ABI slot. close_backed uses committed trader intent
    /// and opening/closing collateral quotes, not the retired historical floor.
    /// Its client may alias config here to avoid an unused account lock.
    pub guard: UncheckedAccount<'info>,
}

pub fn pending_intent(trader: &ErTrader, custody: &ErCustody, slot: u8, nonce: u64, kind: u8, next_seq: u64) -> Result<ErSlot> {
    require!(trader.owner == custody.owner && trader.config == custody.config && trader.pool == custody.pool, MixError::Binding);
    let i = usize::from(slot);
    let intent = *trader.slots.get(i).ok_or(MixError::Binding)?;
    let receipt = custody.receipts.get(i).ok_or(MixError::Binding)?;
    require!(intent.state == 2 && intent.kind == kind && intent.nonce == nonce && nonce > 0, MixError::Binding);
    // Only winning closes reserve a FIFO claim rank. Losses and withdrawals
    // remain executable if another user's conversion cannot currently settle.
    if kind == 1 && intent.close_seq > 0 {
        require!(intent.close_seq == next_seq && next_seq > 0, MixError::QueueOrder);
    } else {
        require!(intent.close_seq == 0, MixError::QueueOrder);
    }
    // Monotone nonces prohibit replay even after an old Position's rent is recycled.
    require!(nonce > receipt.nonce, MixError::Replay);
    require!(intent.margin > 0, MixError::Amount);
    Ok(intent)
}

fn asset_index(custody: &ErCustody, asset: Pubkey) -> Result<usize> {
    custody.assets.iter().position(|a| a.asset == asset).ok_or_else(|| error!(MixError::Binding))
}

pub fn validate_staged(position: &Position, intent: &ErSlot, market: &Market, asset: Pubkey) -> Result<()> {
    require!(position.state == 0 && intent.kind == 1 && intent.market == market.id && intent.asset == asset
        && position.nonce == intent.nonce && position.margin == intent.margin
        && position.entry_value == intent.entry_value && position.entry == intent.entry
        && position.bust == intent.bust && position.leverage == intent.leverage && position.long == intent.long
        && position.opened_at == intent.opened_at && position.opened_slot == intent.opened_source_slot
        && position.cursor == intent.cursor && position.bust_observed == intent.bust_observed
        && position.exit == intent.exit && position.closed_at == intent.closed_at
        && position.er_close_seq == intent.close_seq, MixError::Binding);
    Ok(())
}

pub fn initialize_custody(ctx: Context<InitializeCustody>) -> Result<()> {
    require!(cfg!(feature = "devnet") || cfg!(feature = "mainnet"), MixError::ProductionDisabled);
    #[cfg(feature="mainnet")]
    crate::mainnet_config::admit(&ctx.accounts.config, ctx.accounts.payer.key())?;
    let c = &mut ctx.accounts.custody;
    c.owner = ctx.accounts.owner.key();
    c.config = ctx.accounts.config.key();
    c.pool = ctx.accounts.pool.key();
    c.bump = ctx.bumps.custody;
    Ok(())
}

pub fn deposit_collateral(ctx: Context<DepositCollateral>, amount: u64) -> Result<()> {
    require!(!ctx.accounts.config.paused, MixError::Paused);
    require!(amount > 0 && amount <= 1_000_000_000_000_000, MixError::Amount);
    require!(ctx.accounts.asset.collateral_enabled, MixError::Binding);
    let asset = ctx.accounts.asset.key();
    let i = ctx.accounts.custody.asset_index_or_insert(asset)?;
    let before = pump::token_amount(&ctx.accounts.vault.to_account_info())?;
    token_interface::transfer_checked(CpiContext::new(ctx.accounts.token_program.key(), TransferChecked {
        from: ctx.accounts.source.to_account_info(), to: ctx.accounts.vault.to_account_info(),
        mint: ctx.accounts.mint.to_account_info(), authority: ctx.accounts.owner.to_account_info(),
    }), amount, ctx.accounts.asset.decimals)?;
    require!(pump::token_amount(&ctx.accounts.vault.to_account_info())?.checked_sub(before) == Some(amount), MixError::Binding);
    let balance = &mut ctx.accounts.custody.assets[i];
    balance.asset = asset;
    balance.available = balance.available.checked_add(amount).ok_or(MixError::Arithmetic)?;
    balance.deposited_credits = balance.deposited_credits.checked_add(amount).ok_or(MixError::Arithmetic)?;
    ctx.accounts.inventory.locked = ctx.accounts.inventory.locked.checked_add(amount).ok_or(MixError::Arithmetic)?;
    Ok(())
}

pub fn prepare_settlement(ctx: Context<PrepareSettlement>, nonce: u64, slot: u8) -> Result<()> {
    let trader = committed_trader(&ctx.accounts.trader, ctx.accounts.owner.key(), ctx.accounts.config.key(), ctx.accounts.pool.key())?;
    let intent = pending_intent(&trader, &ctx.accounts.custody, slot, nonce, 1, ctx.accounts.config.next_settlement_seq)?;
    require!(intent.market == ctx.accounts.market.id && intent.asset == ctx.accounts.asset.key(), MixError::Binding);
    let i = asset_index(&ctx.accounts.custody, intent.asset)?;
    require!(ctx.accounts.custody.assets[i].available >= intent.margin, MixError::Inventory);
    let x = &mut ctx.accounts.position;
    if x.owner != Pubkey::default() {
        require!(x.er_slot == slot, MixError::Binding);
        return validate_staged(x, &intent, &ctx.accounts.market, ctx.accounts.asset.key());
    }
    x.owner = trader.owner;
    x.pool = trader.pool;
    x.market = ctx.accounts.market.key();
    x.collateral = intent.asset;
    x.nonce = nonce;
    x.margin = intent.margin;
    x.entry_value = intent.entry_value;
    x.entry = intent.entry;
    x.bust = intent.bust;
    x.leverage = intent.leverage;
    x.long = intent.long;
    x.opened_at = intent.opened_at;
    x.opened_slot = intent.opened_source_slot;
    x.cursor = intent.cursor;
    x.bust_observed = intent.bust_observed;
    x.exit = intent.exit;
    x.closed_at = intent.closed_at;
    x.queue_seq = u64::MAX;
    x.er_slot = slot;
    x.er_close_seq = intent.close_seq;
    x.bump = ctx.bumps.position;
    // No backing, exposure, sequence or receipt changes until the real close succeeds.
    Ok(())
}

pub fn finish_trade(custody: &mut ErCustody, slot: u8, intent: &ErSlot, position: Pubkey, loss: u64) -> Result<()> {
    let receipt = custody.receipts.get(usize::from(slot)).ok_or(MixError::Binding)?;
    require!(intent.nonce > receipt.nonce && intent.kind == 1 && intent.state == 2, MixError::Replay);
    let returned = intent.margin.checked_sub(loss).ok_or(MixError::Arithmetic)?;
    let i = asset_index(custody, intent.asset)?;
    let balance = &mut custody.assets[i];
    require!(balance.available >= intent.margin, MixError::Inventory);
    let available = balance.available.checked_sub(loss).ok_or(MixError::Arithmetic)?;
    let returned_credits = balance.returned_credits.checked_add(returned).ok_or(MixError::Arithmetic)?;
    balance.available = available;
    balance.returned_credits = returned_credits;
    custody.receipts[usize::from(slot)] = ErReceipt { nonce: intent.nonce, position, kind: 1, settled: true };
    Ok(())
}

pub fn settle_withdrawal(ctx: Context<SettleWithdrawal>, nonce: u64, slot: u8) -> Result<()> {
    let trader = committed_trader(&ctx.accounts.trader, ctx.accounts.custody.owner, ctx.accounts.config.key(), ctx.accounts.pool.key())?;
    let intent = pending_intent(&trader, &ctx.accounts.custody, slot, nonce, 2, ctx.accounts.config.next_settlement_seq)?;
    require_keys_eq!(intent.asset, ctx.accounts.asset.key(), MixError::Binding);
    let i = asset_index(&ctx.accounts.custody, intent.asset)?;
    let balance = &mut ctx.accounts.custody.assets[i];
    balance.available = balance.available.checked_sub(intent.margin).ok_or(MixError::Inventory)?;
    ctx.accounts.inventory.locked = ctx.accounts.inventory.locked.checked_sub(intent.margin).ok_or(MixError::Arithmetic)?;
    pay(&ctx.accounts.pool, &ctx.accounts.vault, &ctx.accounts.destination, &ctx.accounts.mint,
        &ctx.accounts.token_program, &ctx.accounts.authority, intent.margin)?;
    ctx.accounts.custody.receipts[usize::from(slot)] = ErReceipt { nonce, position: Pubkey::default(), kind: 2, settled: true };
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeCustody<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    pub owner: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump)] pub config: Box<Account<'info, Config>>,
    #[account(seeds=[b"pool",&0u16.to_le_bytes()],bump=pool.bump,constraint=pool.id==0 @ MixError::Binding)] pub pool: Box<Account<'info, Pool>>,
    #[account(init,payer=payer,space=8+ErCustody::INIT_SPACE,seeds=[b"er-custody",owner.key().as_ref()],bump)] pub custody: Box<Account<'info, ErCustody>>,
    pub system_program: Program<'info, System>,
    pub execution: BaseExecution<'info>,
}

#[derive(Accounts)]
pub struct DepositCollateral<'info> {
    pub owner: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump)] pub config: Box<Account<'info, Config>>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,seeds=[b"er-custody",owner.key().as_ref()],bump=custody.bump,has_one=owner,has_one=config,has_one=pool)] pub custody: Box<Account<'info, ErCustody>>,
    pub asset: Box<Account<'info, Asset>>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)] pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump,has_one=pool,has_one=asset)] pub inventory: Box<Account<'info, Inventory>>,
    /// CHECK: Canonical pool signing authority.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority: UncheckedAccount<'info>,
    #[account(mut,token::mint=mint,token::authority=owner,token::token_program=token_program)] pub source: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)] pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    pub execution: BaseExecution<'info>,
}

#[derive(Accounts)]
#[instruction(nonce:u64,slot:u8)]
pub struct PrepareSettlement<'info> {
    #[account(mut)] pub payer: Signer<'info>,
    /// CHECK: Bound to canonical committed trader and custody; no owner signature is needed.
    pub owner: UncheckedAccount<'info>,
    #[account(seeds=[b"config"],bump=config.bump)] pub config: Box<Account<'info, Config>>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(has_one=pool)] pub market: Box<Account<'info, Market>>,
    pub asset: Box<Account<'info, Asset>>,
    /// CHECK: Canonical PDA, DLP owner, discriminator and all identity bindings checked in handler.
    pub trader: UncheckedAccount<'info>,
    #[account(seeds=[b"er-custody",owner.key().as_ref()],bump=custody.bump,has_one=owner,has_one=config,has_one=pool)] pub custody: Box<Account<'info, ErCustody>>,
    #[account(init_if_needed,payer=payer,space=8+Position::INIT_SPACE,seeds=[b"position",owner.key().as_ref(),&nonce.to_le_bytes()],bump)] pub position: Box<Account<'info, Position>>,
    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
pub struct SettleWithdrawal<'info> {
    pub caller: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump)] pub config: Box<Account<'info, Config>>,
    pub pool: Box<Account<'info, Pool>>,
    #[account(mut,seeds=[b"er-custody",custody.owner.as_ref()],bump=custody.bump,has_one=config,has_one=pool)] pub custody: Box<Account<'info, ErCustody>>,
    /// CHECK: Canonical committed intent validation in handler.
    pub trader: UncheckedAccount<'info>,
    pub asset: Box<Account<'info, Asset>>,
    #[account(address=asset.mint,mint::token_program=token_program)] pub mint: Box<InterfaceAccount<'info, Mint>>,
    #[account(address=asset.token_program)] pub token_program: Interface<'info, TokenInterface>,
    #[account(mut,seeds=[b"inventory",pool.key().as_ref(),asset.key().as_ref()],bump=inventory.bump,has_one=pool,has_one=asset)] pub inventory: Box<Account<'info, Inventory>>,
    /// CHECK: Canonical pool authority signs withdrawal only after committed intent verification.
    #[account(seeds=[b"authority",pool.key().as_ref()],bump=pool.authority_bump)] pub authority: UncheckedAccount<'info>,
    #[account(mut,seeds=[b"vault",pool.key().as_ref(),asset.key().as_ref()],bump,token::mint=mint,token::authority=authority,token::token_program=token_program)] pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut,token::mint=mint,token::token_program=token_program,constraint=destination.owner==custody.owner @ MixError::Unauthorized)] pub destination: Box<InterfaceAccount<'info, TokenAccount>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (ErTrader, ErCustody) {
        let owner = Pubkey::new_unique();
        let config = Pubkey::new_unique();
        let pool = Pubkey::new_unique();
        let asset = Pubkey::new_unique();
        let mut trader = ErTrader { owner, config, pool, ..ErTrader::default() };
        trader.slots[0] = ErSlot { state: 2, kind: 1, nonce: 1, asset, margin: 400, close_seq: 1, ..ErSlot::default() };
        let mut custody = ErCustody { owner, config, pool, ..ErCustody::default() };
        custody.assets[0] = CustodyAsset { asset, available: 1_000, deposited_credits: 1_000, returned_credits: 0, ..CustodyAsset::default() };
        (trader, custody)
    }

    #[test]
    fn authenticates_canonical_committed_state_not_supplied_bytes() {
        let (trader, _) = fixture();
        let canonical = Pubkey::find_program_address(&[b"er-trader", trader.owner.as_ref()], &crate::ID).0;
        let fake = Pubkey::new_unique();
        let mut data = Vec::new();
        trader.try_serialize(&mut data).unwrap();
        for (key, owner, expected) in [(canonical, DELEGATION_OWNER, true), (fake, DELEGATION_OWNER, false), (canonical, crate::ID, false)] {
            let mut lamports = 1;
            let info = AccountInfo::new(&key, false, false, &mut lamports, &mut data, &owner, false);
            assert_eq!(committed_trader(&info, trader.owner, trader.config, trader.pool).is_ok(), expected);
            assert!(committed_trader(&info, trader.owner, Pubkey::new_unique(), trader.pool).is_err());
        }
        data[0] ^= 1;
        let mut lamports = 1;
        let info = AccountInfo::new(&canonical, false, false, &mut lamports, &mut data, &DELEGATION_OWNER, false);
        assert!(committed_trader(&info, trader.owner, trader.config, trader.pool).is_err());
    }

    #[test]
    fn winning_settlements_follow_fifo_without_blocking_losses_or_withdrawals() {
        let (mut trader, mut custody) = fixture();
        assert!(pending_intent(&trader, &custody, 0, 1, 1, 1).is_ok());
        assert!(pending_intent(&trader, &custody, 0, 1, 1, 2).is_err());
        assert!(pending_intent(&trader, &custody, 0, 2, 1, 1).is_err());
        assert!(pending_intent(&trader, &custody, 8, 1, 1, 1).is_err());
        trader.slots[0].state = 1;
        assert!(pending_intent(&trader, &custody, 0, 1, 1, 1).is_err());
        trader.slots[0].state = 2;
        trader.slots[0].kind = 2;
        assert!(pending_intent(&trader, &custody, 0, 1, 1, 1).is_err());
        assert!(pending_intent(&trader, &custody, 0, 1, 2, 1).is_err());
        trader.slots[0].close_seq = 0;
        assert!(pending_intent(&trader, &custody, 0, 1, 2, 1).is_ok());
        assert!(pending_intent(&trader, &custody, 0, 1, 2, 7).is_ok());
        trader.slots[0].kind = 1;
        assert!(pending_intent(&trader, &custody, 0, 1, 1, 7).is_ok());
        trader.slots[0].kind = 2;
        custody.receipts[0] = ErReceipt { nonce: 1, kind: 2, settled: true, position: Pubkey::default() };
        assert!(pending_intent(&trader, &custody, 0, 1, 2, 1).is_err());
    }

    #[test]
    fn settlement_preserves_returned_collateral_and_credits_exactly_once() {
        let (trader, mut custody) = fixture();
        let intent = trader.slots[0];
        let position = Pubkey::new_unique();
        finish_trade(&mut custody, 0, &intent, position, 250).unwrap();
        assert_eq!(custody.assets[0].available, 750);
        assert_eq!(custody.assets[0].returned_credits, 150);
        assert_eq!(custody.assets[0].deposited_credits, 1_000);
        assert_eq!(custody.receipts[0].position, position);
        assert!(finish_trade(&mut custody, 0, &intent, position, 250).is_err());
        assert_eq!(custody.assets[0].available, 750);
        assert_eq!(custody.assets[0].returned_credits, 150);
        let (trader, mut custody) = fixture();
        finish_trade(&mut custody, 0, &trader.slots[0], position, 0).unwrap();
        assert_eq!((custody.assets[0].available, custody.assets[0].returned_credits), (1_000, 400));
        let (trader, mut custody) = fixture();
        finish_trade(&mut custody, 0, &trader.slots[0], position, 400).unwrap();
        assert_eq!((custody.assets[0].available, custody.assets[0].returned_credits), (600, 0));
    }
}
