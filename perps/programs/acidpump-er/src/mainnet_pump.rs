//! Mainnet PumpSwap validation and integer quotation with a bound quote mint.
//! Amounts remain in token atoms / lamports until explicitly converted to USD.
//! Supports versioned PumpSwap pools, including Mayhem and Cashback routing.
use anchor_lang::prelude::*;
use anchor_spl::{token, token_2022};
use crate::{math, pump, MixError};

pub const USDC: Pubkey = pubkey!("EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
pub const SOL: Pubkey = token::spl_token::native_mint::ID;
const PUMP_ORIGIN: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
const POOL_DISC: [u8; 8] = [241,154,109,4,17,177,109,188];
const FEES_DISC: [u8; 8] = [143,52,146,187,219,123,76,155];
const GLOBAL_DISC: [u8; 8] = [149,8,156,202,160,252,176,217];

fn bytes<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]> {
    data.get(at..at.checked_add(N).ok_or(MixError::Arithmetic)?)
        .and_then(|s| s.try_into().ok()).ok_or_else(|| error!(MixError::Binding))
}
fn key(data: &[u8], at: usize) -> Result<Pubkey> { Ok(Pubkey::new_from_array(bytes(data, at)?)) }
fn num(data: &[u8], at: usize) -> Result<u64> { Ok(u64::from_le_bytes(bytes(data, at)?)) }
fn appended<const N: usize>(data: &[u8], at: usize) -> Result<[u8; N]> {
    if data.len() <= at { Ok([0; N]) } else { bytes(data, at) }
}
/// Pump uses zero defaults for fields absent from historical pool layouts.
/// Partial fields and non-boolean flags are still malformed, not an old version.
#[derive(Clone, Copy)]
struct PoolOptions { creator: Pubkey, mayhem: bool, cashback: bool, virtual_quote: i128, creator_fee: u64, retained_fees: u64 }
impl PoolOptions {
    fn read(data: &[u8]) -> Result<Self> {
        require!(data.len() >= 211, MixError::Binding);
        let mayhem = appended::<1>(data,243)?[0];
        let cashback = appended::<1>(data,244)?[0];
        require!(mayhem <= 1 && cashback <= 1 && appended::<1>(data,269)?[0] <= 1 && appended::<1>(data,270)?[0] <= 1, MixError::Binding);
        let retained_fees = u64::from_le_bytes(appended(data,271)?).checked_add(u64::from_le_bytes(appended(data,279)?)).ok_or(MixError::Arithmetic)?;
        Ok(Self { creator: Pubkey::new_from_array(appended(data,211)?), mayhem: mayhem == 1, cashback: cashback == 1,
            virtual_quote: i128::from_le_bytes(appended(data,245)?), creator_fee: u64::from_le_bytes(appended(data,261)?), retained_fees })
    }
}
pub fn sol_swap_accounts_len(data: &[u8], buy: bool) -> Result<usize> {
    let options = PoolOptions::read(data)?;
    Ok((if buy {25} else {23}) + usize::from(options.creator != Pubkey::default())
        + if options.cashback { if buy {1} else {2} } else {0})
}
/// Split a concatenated route without dropping support for already-built v1
/// transactions. This selects an ABI only; execute_sol_leg authenticates it.
pub fn sol_leg_accounts_len(accounts:&[AccountInfo],buy:bool)->Result<usize> {
    require!(accounts.len()>=17,MixError::Binding);
    if accounts[9].key()==token::ID || accounts[9].key()==token_2022::ID {Ok(17)}
    else {sol_swap_accounts_len(&accounts[0].try_borrow_data()?,buy)}
}
fn bound(info: &AccountInfo, address: Pubkey, owner: Pubkey) -> Result<()> {
    require!(info.key() == address && *info.owner == owner && !info.executable, MixError::Binding);
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolQuote {
    pub base: u64,
    pub real_quote: u64,
    pub virtual_quote: i128,
    pub fees: [u16; 3],
}
// The invariant and fee arithmetic use quote-token atoms. SOL routes retain
// their historical name; token routes must bind the quote mint before use.
pub type SolPoolQuote = PoolQuote;
impl PoolQuote {
    pub fn effective_quote(&self) -> Result<u64> {
        require!(self.base > 0 && self.real_quote > 0, MixError::Quote);
        require!(self.fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
        let quote = (self.real_quote as i128).checked_add(self.virtual_quote).ok_or(MixError::Arithmetic)?;
        require!(quote > 0, MixError::Quote);
        u64::try_from(quote).map_err(|_| error!(MixError::Arithmetic))
    }
    fn costs(&self, gross: u64) -> Result<[u64; 3]> {
        let mut costs = [0; 3];
        for (i, fee) in self.fees.iter().enumerate() {
            costs[i] = math::ceil(gross as u128 * *fee as u128, 10_000)?;
        }
        Ok(costs)
    }
    /// SDK sellBaseInput parity, including real liquidity after retained LP fee.
    pub fn sell(&self, amount: u64) -> Result<u64> {
        let quote = self.effective_quote()?;
        if amount == 0 { return Ok(0); }
        let gross = math::div(amount as u128, quote as u128, self.base as u128 + amount as u128)?;
        let costs = self.costs(gross)?;
        require!(gross.saturating_sub(costs[0]) <= self.real_quote, MixError::Quote);
        gross.checked_sub(costs.iter().sum()).ok_or_else(|| error!(MixError::Quote))
    }
    /// SDK buyQuoteInput parity. Tiny unusable inputs return zero, never wrap.
    pub fn buy(&self, lamports: u64) -> Result<u64> {
        let quote = self.effective_quote()?;
        let total_bps = self.fees.iter().map(|f| *f as u128).sum::<u128>();
        let mut effective = math::div(lamports as u128, 10_000, 10_000 + total_bps)?;
        let total = effective as u128 + self.costs(effective)?.iter().map(|f| *f as u128).sum::<u128>();
        if total > lamports as u128 { effective = effective.saturating_sub((total - lamports as u128) as u64); }
        let input = effective.saturating_sub(1);
        math::div(self.base as u128, input as u128, quote as u128 + input as u128)
    }
    pub fn sell_usd_micro(&self, amount: u64, sol_usd_e12: u64) -> Result<u64> {
        sol_to_usd_micro(self.sell(amount)?, sol_usd_e12)
    }
    /// A collateral bag must be executable in full. Empty/insufficient real
    /// liquidity is a zero low-water mark, not imaginary virtual SOL backing.
    /// Invalid metadata and arithmetic still fail instead of silently becoming zero.
    pub fn collateral_usd_micro(&self, amount: u64, sol_usd_e12: u64) -> Result<u64> {
        sol_to_usd_micro(0, sol_usd_e12)?;
        sol_to_usd_micro(self.collateral_lamports(amount)?, sol_usd_e12)
    }
    pub fn collateral_lamports(&self, amount: u64) -> Result<u64> {
        self.collateral_quote_atoms(amount)
    }
    /// Exact net quote-token atoms from selling the entire collateral bag.
    pub fn collateral_quote_atoms(&self, amount: u64) -> Result<u64> {
        require!(self.fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
        if amount == 0 || self.base == 0 || self.real_quote == 0 { return Ok(0); }
        let gross = math::div(amount as u128, self.effective_quote()? as u128, self.base as u128 + amount as u128)?;
        let costs = self.costs(gross)?;
        if gross.saturating_sub(costs[0]) > self.real_quote { return Ok(0); }
        Ok(gross.saturating_sub(costs.iter().sum()))
    }
    /// Exact inverse of the liquidity bound, including the LP fee ceiling.
    /// gross - ceil(gross*lp/10000) = floor(gross*(10000-lp)/10000).
    /// Invert that integer inequality and then the constant-product gross.
    /// This avoids nesting a liquidity binary search inside FIFO sizing.
    pub fn maximum_sell(&self, available:u64)->Result<u64> {
        require!(self.fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
        if available == 0 || self.base == 0 || self.real_quote == 0 { return Ok(0); }
        let quote=self.effective_quote()? as u128;
        let gross_limit=((self.real_quote as u128+1)*10_000-1)/(10_000-self.fees[0] as u128);
        if quote<=gross_limit+1 {return Ok(available);}
        let limit=((gross_limit+1)*self.base as u128-1)/(quote-gross_limit-1);
        Ok(limit.min(available as u128) as u64)
    }
}
pub fn sol_to_usd_micro(lamports: u64, sol_usd_e12: u64) -> Result<u64> {
    require!(sol_usd_e12 > 0 && sol_usd_e12 <= 1_000_000_000_000_000_000, MixError::Quote);
    math::div(lamports as u128, sol_usd_e12 as u128, 1_000_000_000_000_000)
}

fn fee_row(data: &[u8], at: usize) -> Result<[u16; 3]> {
    let mut fees = [0; 3];
    for (i, fee) in fees.iter_mut().enumerate() {
        let value = num(data, at + 8 * i)?;
        require!(value < 10_000, MixError::Quote);
        *fee = value as u16;
    }
    require!(fees.iter().map(|f| *f as u32).sum::<u32>() < 10_000, MixError::Quote);
    Ok(fees)
}
fn fee_vector_end(data: &[u8], at: usize) -> Result<usize> {
    let count = u32::from_le_bytes(bytes(data, at)?) as usize;
    // Pump's upsert instructions bound both vectors at 50 entries.
    require!(count <= 50, MixError::Binding);
    let end = at.checked_add(4 + count * 40).ok_or(MixError::Arithmetic)?;
    require!(end <= data.len(), MixError::Binding);
    Ok(end)
}
fn tier_fees(data: &[u8], vector: usize, end: usize, market_cap: u128) -> Result<[u16; 3]> {
    let count = (end - vector - 4) / 40;
    require!(count > 0, MixError::Binding);
    let mut selected = fee_row(data, vector + 20)?;
    let mut previous = 0;
    for i in 0..count {
        let at = vector + 4 + i * 40;
        let threshold = u128::from_le_bytes(bytes(data, at)?);
        require!(i == 0 || threshold > previous, MixError::Binding);
        let fees = fee_row(data, at + 16)?;
        if market_cap >= threshold { selected = fees; }
        previous = threshold;
    }
    Ok(selected)
}
/// Match Pump's versioned FeeConfig and quote-currency schedule selection.
/// Account length, not nonzero padding, determines which appended fields exist.
/// Market cap is denominated in quote-token atoms, never implicitly in SOL.
pub fn quote_fees(data: &[u8], canonical: bool, quote_mint: Pubkey, market_cap: u128) -> Result<[u16; 3]> {
    require!(data.len() >= 2512 && bytes::<8>(data, 0)? == FEES_DISC, MixError::Binding);
    let sol_end = fee_vector_end(data, 65)?;
    let stable_end = if data.len() >= 4073 { Some(fee_vector_end(data, sol_end)?) } else { None };
    if let Some(end) = stable_end {
        if data.len() >= 4097 { require!(end + 24 <= data.len(), MixError::Binding); }
    }
    if !canonical { return fee_row(data, 41); }
    if quote_mint == SOL || quote_mint == Pubkey::default() || quote_mint == token_2022::spl_token_2022::native_mint::ID {
        return tier_fees(data, 65, sol_end, market_cap);
    }
    if quote_mint == USDC {
        if let Some(end) = stable_end {
            if end > sol_end + 4 { return tier_fees(data, sol_end, end, market_cap); }
        }
        return tier_fees(data, 65, sol_end, market_cap);
    }
    if data.len() >= 4097 {
        let exotic = fee_row(data, stable_end.ok_or(MixError::Binding)?)?;
        if exotic != [0; 3] { return Ok(exotic); }
    }
    fee_row(data, 41)
}
pub fn sol_fees(data: &[u8], canonical: bool, market_cap: u128) -> Result<[u16; 3]> {
    quote_fees(data, canonical, SOL, market_cap)
}

pub struct SolPoolAccounts<'a, 'info> {
    pub pool: &'a AccountInfo<'info>,
    pub mint: &'a AccountInfo<'info>,
    pub base_vault: &'a AccountInfo<'info>,
    pub quote_vault: &'a AccountInfo<'info>,
    pub global: &'a AccountInfo<'info>,
    pub fees: &'a AccountInfo<'info>,
}
/// Non-native quote tokens require their mint account as well as the vault.
/// Do not expose a SOL/USD conversion until the caller binds a currency route.
pub struct TokenPoolAccounts<'a, 'info> {
    pub pool: SolPoolAccounts<'a, 'info>,
    pub quote_mint: &'a AccountInfo<'info>,
}
impl TokenPoolAccounts<'_, '_> {
    pub fn validate(&self, expected_pool: Pubkey, expected_mint: Pubkey, expected_quote: Pubkey, allow_empty: bool) -> Result<PoolQuote> {
        require!(expected_quote != Pubkey::default() && expected_quote != SOL &&
            expected_quote != token_2022::spl_token_2022::native_mint::ID, MixError::Binding);
        require_keys_eq!(self.quote_mint.key(), expected_quote, MixError::Binding);
        self.pool.validate_inner(expected_pool, expected_mint, allow_empty, Some(self.quote_mint), false)
    }
}
pub fn token_balance(info: &AccountInfo, mint: Pubkey, authority: Pubkey, program: Pubkey) -> Result<u64> {
    require!(*info.owner == program && !info.executable, MixError::Binding);
    let data = info.try_borrow_data()?;
    require!(key(&data, 0)? == mint && key(&data, 32)? == authority && bytes::<1>(&data,108)? == [1], MixError::Binding);
    num(&data, 64)
}
impl SolPoolAccounts<'_, '_> {
    /// Registered pool and mint addresses must come from platform state.
    pub fn validate(&self, expected_pool: Pubkey, expected_mint: Pubkey) -> Result<SolPoolQuote> {
        self.validate_inner(expected_pool, expected_mint, false, None, false)
    }
    /// Closing collateral can genuinely be worth zero. Authenticate every
    /// account as usual, but do not require drained reserves to execute a swap.
    pub fn validate_collateral(&self, expected_pool: Pubkey, expected_mint: Pubkey) -> Result<SolPoolQuote> {
        self.validate_inner(expected_pool, expected_mint, true, None, false)
    }
    /// The input mint must be bound to the preceding authenticated venue's
    /// quote. Issuer authorities on an intermediate do not admit it as user
    /// collateral; ordinary validate/validate_collateral remain unchanged.
    pub fn validate_conversion(&self, expected_pool: Pubkey, input_mint: Pubkey, expected_quote: Pubkey,
        quote_mint: Option<&AccountInfo>, allow_empty: bool) -> Result<PoolQuote> {
        require!(input_mint != Pubkey::default() && input_mint != SOL && input_mint != token_2022::spl_token_2022::native_mint::ID &&
            expected_quote != Pubkey::default() && expected_quote != input_mint && expected_quote != token_2022::spl_token_2022::native_mint::ID, MixError::Binding);
        if expected_quote == SOL { require!(quote_mint.is_none(), MixError::Binding); }
        else { require!(quote_mint.is_some_and(|m|m.key()==expected_quote), MixError::Binding); }
        self.validate_inner(expected_pool,input_mint,allow_empty,quote_mint,true)
    }
    fn validate_inner(&self, expected_pool: Pubkey, expected_mint: Pubkey, allow_empty: bool, quote_mint: Option<&AccountInfo>, intermediate: bool) -> Result<PoolQuote> {
        use token_2022::spl_token_2022::{extension::{BaseStateWithExtensions, ExtensionType, StateWithExtensions}, state::Mint};
        bound(self.pool, expected_pool, pump::ID)?;
        require!(self.mint.key() == expected_mint && !self.mint.executable &&
            (*self.mint.owner == token::ID || *self.mint.owner == token_2022::ID), MixError::Binding);
        let mint_data = self.mint.try_borrow_data()?;
        let mint = StateWithExtensions::<Mint>::unpack(&mint_data)?;
        let usdc = expected_mint == USDC && *self.mint.owner == token::ID && mint.base.decimals == 6;
        require!(mint.base.is_initialized && mint.base.decimals <= 9 &&
            (usdc || intermediate || (mint.base.mint_authority.is_none() && mint.base.freeze_authority.is_none())), MixError::Binding);
        for extension in mint.get_extension_types()? {
            require!(matches!(extension, ExtensionType::MetadataPointer | ExtensionType::TokenMetadata), MixError::Binding);
        }
        let (quote_key, quote_program) = if let Some(info) = quote_mint {
            require!(!info.executable && (*info.owner == token::ID || *info.owner == token_2022::ID), MixError::Binding);
            let data = info.try_borrow_data()?;
            let state = StateWithExtensions::<Mint>::unpack(&data)?;
            require!(state.base.is_initialized && state.base.decimals <= 9, MixError::Binding);
            if info.key() == USDC { require!(*info.owner == token::ID && state.base.decimals == 6, MixError::Binding); }
            for extension in state.get_extension_types()? {
                require!(matches!(extension, ExtensionType::MetadataPointer | ExtensionType::TokenMetadata), MixError::Binding);
            }
            (info.key(), *info.owner)
        } else { (SOL, token::ID) };
        let d = self.pool.try_borrow_data()?;
        require!(bytes::<8>(&d, 0)? == POOL_DISC && key(&d,43)? == expected_mint && key(&d,75)? == quote_key, MixError::Binding);
        let creator = key(&d, 11)?;
        let derived = Pubkey::find_program_address(&[b"pool", &bytes::<2>(&d,9)?, creator.as_ref(), expected_mint.as_ref(), quote_key.as_ref()], &pump::ID);
        require!(derived.0 == expected_pool && derived.1 == bytes::<1>(&d,8)?[0], MixError::Binding);
        let options = PoolOptions::read(&d)?;
        require!(self.base_vault.key() == key(&d,139)? && self.quote_vault.key() == key(&d,171)?, MixError::Binding);
        let base = token_balance(self.base_vault, expected_mint, expected_pool, *self.mint.owner)?;
        let real_quote = token_balance(self.quote_vault, quote_key, expected_pool, quote_program)?.checked_sub(options.retained_fees).ok_or(MixError::Quote)?;
        // Fees left in the vault cannot fund a sale. Preserve effective reserves
        // while expressing spendable reserves in the existing ER quote layout.
        let virtual_quote = options.virtual_quote.checked_add(options.retained_fees as i128).ok_or(MixError::Arithmetic)?;
        let mut result = SolPoolQuote { base, real_quote, virtual_quote, fees: [0;3] };
        let empty = base == 0 || real_quote == 0;
        let effective = if allow_empty && empty { 0 } else { result.effective_quote()? };
        bound(self.global, Pubkey::find_program_address(&[b"global_config"], &pump::ID).0, pump::ID)?;
        let global = self.global.try_borrow_data()?;
        require!(bytes::<8>(&global,0)? == GLOBAL_DISC && bytes::<1>(&global,56)?[0] & 24 == 0, MixError::Binding);
        bound(self.fees, Pubkey::find_program_address(&[b"fee_config", pump::ID.as_ref()], &pump::FEES_ID).0, pump::FEES_ID)?;
        let canonical = creator == Pubkey::find_program_address(&[b"pool-authority", expected_mint.as_ref()], &PUMP_ORIGIN).0;
        // Pump's Mayhem schedule uses TOTAL_TOKEN_SUPPLY, not live mint supply.
        let supply = if options.mayhem { 1_000_000_000_000_000 } else { mint.base.supply };
        let market_cap = if base == 0 { 0 } else {
            (supply as u128).checked_mul(effective as u128).ok_or(MixError::Arithmetic)? / base as u128
        };
        result.fees = quote_fees(&self.fees.try_borrow_data()?, canonical, quote_key, market_cap)?;
        // Appended fields are absent on older layouts; Pump treats them as unset.
        let configurable = global.get(940).copied().unwrap_or(0);
        require!(configurable <= 1, MixError::Binding);
        if configurable == 1 {
            let override_bps = options.creator_fee;
            require!(override_bps < 10_000, MixError::Quote);
            if override_bps > 0 { result.fees[2] = override_bps as u16; }
        }
        if options.creator == Pubkey::default() { result.fees[2] = 0; }
        if allow_empty && empty { result.fees = [0;3]; } else { result.effective_quote()?; }
        Ok(result)
    }
}

/// Fixed exact-input CPI under the platform pool authority. All route addresses
/// are checked before invoking Pump; both custody deltas are checked afterward.
/// Returns (input actually spent, output actually received), never a UI quote.
pub fn execute_sol_leg<'info>(
    pool: Pubkey, authority_bump: u8, expected_dex: Pubkey, expected_mint: Pubkey,
    accounts: &[AccountInfo<'info>], buy: bool, amount: u64, min_out: u64,
) -> Result<(u64,u64)> {
    execute_leg(pool,authority_bump,expected_dex,expected_mint,SOL,accounts,buy,amount,min_out,false)
}
pub fn execute_token_leg<'info>(
    pool: Pubkey, authority_bump: u8, expected_dex: Pubkey, expected_mint: Pubkey, expected_quote: Pubkey,
    accounts: &[AccountInfo<'info>], buy: bool, amount: u64, min_out: u64,
) -> Result<(u64,u64)> {
    require!(expected_quote != Pubkey::default() && expected_quote != SOL &&
        expected_quote != token_2022::spl_token_2022::native_mint::ID, MixError::Binding);
    execute_leg(pool,authority_bump,expected_dex,expected_mint,expected_quote,accounts,buy,amount,min_out,false)
}
/// Convert only the net quote proceeds of the preceding authenticated sale.
/// The caller binds input_mint to that venue and preserves intermediate dust.
pub fn execute_conversion_leg<'info>(pool:Pubkey,authority_bump:u8,expected_dex:Pubkey,input_mint:Pubkey,expected_quote:Pubkey,
    accounts:&[AccountInfo<'info>],amount:u64,min_out:u64)->Result<(u64,u64)> {
    execute_leg(pool,authority_bump,expected_dex,input_mint,expected_quote,accounts,false,amount,min_out,true)
}
fn execute_leg<'info>(
    pool: Pubkey, authority_bump: u8, expected_dex: Pubkey, expected_mint: Pubkey, expected_quote: Pubkey,
    accounts: &[AccountInfo<'info>], buy: bool, amount: u64, min_out: u64, intermediate: bool,
) -> Result<(u64,u64)> {
    use anchor_lang::solana_program::{instruction::{AccountMeta,Instruction},program::invoke_signed};
    require!(amount > 0 && min_out > 0, MixError::Amount);
    let compact = accounts.len() == 17;
    let fee_index = if compact {13} else if buy {21} else {19};
    require!(compact || accounts.len() >= fee_index + 4, MixError::Binding);
    let authority = &accounts[1];
    let seeds: &[&[u8]] = &[b"authority",pool.as_ref(),&[authority_bump]];
    let derived = Pubkey::create_program_address(seeds,&crate::ID).map_err(|_| error!(MixError::Binding))?;
    require_keys_eq!(authority.key(),derived,MixError::Binding);
    let native = expected_quote == SOL;
    let native_before = authority.lamports();
    if !native || intermediate { bound(authority,derived,anchor_lang::system_program::ID)?; require!(authority.data_is_empty(),MixError::Binding); }
    let route = SolPoolAccounts {pool:&accounts[0],mint:&accounts[3],base_vault:&accounts[7],quote_vault:&accounts[8],global:&accounts[2],fees:&accounts[fee_index]};
    if intermediate { route.validate_conversion(expected_dex,expected_mint,expected_quote,if native{None}else{Some(&accounts[4])},false)?; }
    else if native { route.validate(expected_dex,expected_mint)?; }
    else { TokenPoolAccounts{pool:route,quote_mint:&accounts[4]}.validate(expected_dex,expected_mint,expected_quote,false)?; }
    let quote_token_program = if native { token::ID } else { *accounts[4].owner };
    let base_program = if compact {9} else {11};
    let quote_program = if compact {10} else {12};
    let system_program = if compact {11} else {13};
    for (index,address) in [(4,expected_quote),(base_program,*accounts[3].owner),(quote_program,quote_token_program),
        (system_program,anchor_lang::system_program::ID),(16,pump::ID)] {
        require_keys_eq!(accounts[index].key(),address,MixError::Binding);
    }
    require!(accounts[16].executable && accounts[base_program].executable && accounts[quote_program].executable, MixError::Binding);
    let options = PoolOptions::read(&accounts[0].try_borrow_data()?)?;
    if compact {
        // Pump v2 retains protocol/creator fees in the pool. Cashback still
        // requires v1; neither route may redirect fee or volume accounts.
        require!(!options.cashback, MixError::Binding);
        let pda = |seeds:&[&[u8]]| Pubkey::find_program_address(seeds,&pump::ID).0;
        require_keys_eq!(accounts[12].key(),pda(&[b"user_volume_accumulator",derived.as_ref()]),MixError::Binding);
        require_keys_eq!(accounts[15].key(),pda(&[b"__event_authority"]),MixError::Binding);
        let global=accounts[2].try_borrow_data()?;
        let mut valid=false;
        for i in 0..8 {
            let wallet=key(&global,643+i*32)?;
            let ata=Pubkey::find_program_address(&[wallet.as_ref(),quote_token_program.as_ref(),expected_quote.as_ref()],&anchor_spl::associated_token::ID).0;
            valid |= wallet!=Pubkey::default() && accounts[14].key()==ata;
        }
        require!(valid, MixError::Binding);
    } else {
        require_keys_eq!(accounts[14].key(),anchor_spl::associated_token::ID,MixError::Binding);
        require_keys_eq!(accounts[fee_index+1].key(),pump::FEES_ID,MixError::Binding);
        require!(accounts.len() == sol_swap_accounts_len(&accounts[0].try_borrow_data()?,buy)?, MixError::Binding);
        validate_swap_tail(accounts, fee_index, derived, expected_mint, expected_quote, quote_token_program, options, buy)?;
    }
    // Shared intermediate SOL may already contain dust. A swap must never
    // consume more than this call's authorized input or debit another vault.
    require!(accounts[5].key() != accounts[6].key() && accounts[5].key() != accounts[7].key() && accounts[6].key() != accounts[8].key(), MixError::Binding);
    let base0 = token_balance(&accounts[5],expected_mint,derived,*accounts[3].owner)?;
    let quote0 = token_balance(&accounts[6],expected_quote,derived,quote_token_program)?;
    require!(if buy {quote0 >= amount} else {base0 >= amount},MixError::Inventory);
    let mut data = match (compact,buy) {
        (true,true)=>vec![194,171,28,70,104,77,91,47],
        (true,false)=>vec![93,246,130,60,231,233,64,178],
        (false,true)=>vec![198,46,21,82,180,217,232,112],
        (false,false)=>vec![51,230,133,164,1,127,131,173],
    };
    data.extend_from_slice(&amount.to_le_bytes());data.extend_from_slice(&min_out.to_le_bytes());
    if buy && !compact {data.push(0);}
    let instruction = Instruction {program_id:pump::ID,data,accounts:accounts.iter().map(|a| AccountMeta {
        pubkey:a.key(),is_writable:a.is_writable,is_signer:a.key() == derived,
    }).collect()};
    invoke_signed(&instruction,accounts,&[seeds])?;
    let base1 = token_balance(&accounts[5],expected_mint,derived,*accounts[3].owner)?;
    let quote1 = token_balance(&accounts[6],expected_quote,derived,quote_token_program)?;
    // Token swaps must not silently initialize or extend accounts with the
    // platform's operating SOL. Prepare all rent separately, with a payer.
    if !native || intermediate { require!(authority.lamports() == native_before, MixError::Binding); }
    let (spent,received) = if buy {
        (quote0.checked_sub(quote1).ok_or(MixError::Binding)?,base1.checked_sub(base0).ok_or(MixError::Binding)?)
    } else {
        (base0.checked_sub(base1).ok_or(MixError::Binding)?,quote1.checked_sub(quote0).ok_or(MixError::Binding)?)
    };
    require!(spent > 0 && spent <= amount && (buy || spent == amount) && received >= min_out, MixError::Quote);
    Ok((spent,received))
}

/// Pin every fee, volume and optional account to Pump's canonical derivation.
/// Caller-supplied accounts cannot redirect creator fees or cashback.
fn validate_swap_tail(accounts: &[AccountInfo], fee_index: usize, authority: Pubkey, mint: Pubkey, quote: Pubkey, quote_program: Pubkey, options: PoolOptions, buy: bool) -> Result<()> {
    let pda = |seeds: &[&[u8]]| Pubkey::find_program_address(seeds,&pump::ID).0;
    let ata = |owner: Pubkey| Pubkey::find_program_address(&[owner.as_ref(),quote_program.as_ref(),quote.as_ref()],&anchor_spl::associated_token::ID).0;
    let matches = |index: usize, address: Pubkey| -> Result<()> {
        require!(accounts.get(index).is_some_and(|a| a.key() == address), MixError::Binding); Ok(())
    };
    let global = accounts[2].try_borrow_data()?;
    let recipient = accounts[9].key();
    let mut protocol_valid = false;
    for i in 0..8 {
        let offset = if options.mayhem { if i == 0 {385} else {418+(i-1)*32} } else {57+i*32};
        protocol_valid |= recipient != Pubkey::default() && key(&global,offset)? == recipient;
    }
    require!(protocol_valid, MixError::Binding);
    matches(10,ata(recipient))?;
    matches(15,pda(&[b"__event_authority"]))?;
    let creator = pda(&[b"creator_vault",options.creator.as_ref()]);
    matches(17,ata(creator))?; matches(18,creator)?;
    let volume = pda(&[b"user_volume_accumulator",authority.as_ref()]);
    if buy { matches(19,pda(&[b"global_volume_accumulator"]))?; matches(20,volume)?; }
    let mut next = fee_index + 2;
    if options.cashback {
        matches(next,ata(volume))?; next += 1;
        if !buy { matches(next,volume)?; next += 1; }
    }
    if options.creator != Pubkey::default() { matches(next,pda(&[b"pool-v2",mint.as_ref()]))?; next += 1; }
    let buyback = accounts[next].key();
    let mut buyback_valid = false;
    for i in 0..8 { buyback_valid |= buyback != Pubkey::default() && key(&global,643+i*32)? == buyback; }
    require!(buyback_valid, MixError::Binding);
    matches(next+1,ata(buyback))?;
    Ok(())
}
