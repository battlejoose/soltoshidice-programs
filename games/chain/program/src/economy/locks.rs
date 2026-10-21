//! Bank equity and fee power are separate. A lock expires by chain time;
//! its multiplier remains until shares are withdrawn. Deposits never relock others.
use super::*;

pub const LEDGER_LEN: usize = 158;
pub const FOREVER_LEDGER_LEN: usize = LEDGER_LEN + 16;
pub const FOREVER_TIER: usize = 5;
pub const POSITION_LEN: usize = 115;
pub const MULTIPLIERS: [u128; 5] = [1, 2, 3, 5, 10];
pub const DURATIONS: [i64; 5] = [0, 3600, 86400, 604800, 2592000];
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Ledger {
    pub version: u8, pub bank: Pubkey, pub wallet: Pubkey,
    pub shares: [u128; 5], pub next_nonce: u64, pub positions: u32, pub bump: u8,
    // Appended only on a wallet's first Forever stake. Legacy offsets stay intact.
    pub forever_shares: u128,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Position {
    pub version: u8, pub bank: Pubkey, pub wallet: Pubkey, pub nonce: u64,
    pub shares: u128, pub deposited: u64, pub tier: u8,
    pub created_at: i64, pub unlock_at: i64, pub bump: u8,
}
impl Ledger {
    pub fn power(&self) -> Result<u128, ProgramError> {
        self.shares.iter().zip(MULTIPLIERS).try_fold(mul128(self.forever_shares, 20)?, |sum, (shares, multiplier)| add128(sum, mul128(*shares, multiplier)?))
    }
    pub(super) fn bucket(&self, tier: usize) -> u128 { if tier == FOREVER_TIER { self.forever_shares } else { self.shares[tier] } }
    pub(super) fn bucket_mut(&mut self, tier: usize) -> &mut u128 { if tier == FOREVER_TIER { &mut self.forever_shares } else { &mut self.shares[tier] } }
}
// Forever has no expiry. Withdrawal checks its tier explicitly, not a distant date.
fn unlock_time(now: i64, tier: usize) -> Result<i64, ProgramError> {
    if tier == FOREVER_TIER { Ok(0) } else { now.checked_add(DURATIONS[tier]).ok_or(ProgramError::ArithmeticOverflow) }
}
pub(super) fn write_ledger(account: &AccountInfo, ledger: &Ledger) -> ProgramResult {
    let size = if ledger.version == 1 { LEDGER_LEN } else { FOREVER_LEDGER_LEN };
    check(account.data_len() == size && (ledger.version == 2 || ledger.forever_shares == 0), 60)?;
    let bytes = borsh::to_vec(ledger).map_err(|_| ProgramError::InvalidAccountData)?;
    account.try_borrow_mut_data()?.copy_from_slice(&bytes[..size]); Ok(())
}
pub(super) fn ensure_ledger<'a>(program: &Pubkey, wallet: &AccountInfo<'a>, account: &AccountInfo<'a>, system: &AccountInfo<'a>, bank: &Pubkey, ledger: &mut Ledger, tier: usize) -> ProgramResult {
    if tier == FOREVER_TIER { ledger.version = 2; }
    let size = if ledger.version == 1 { LEDGER_LEN } else { FOREVER_LEDGER_LEN };
    if account.data_is_empty() {
        create_pda(wallet, account, system, program, size,
            &[b"stakes", bank.as_ref(), wallet.key.as_ref(), &[ledger.bump]])?;
    } else if account.data_len() != size {
        check(account.data_len() == LEDGER_LEN && size == FOREVER_LEDGER_LEN && *system.key == system_program::id(), 60)?;
        let needed = Rent::get()?.minimum_balance(size).saturating_sub(account.lamports());
        if needed > 0 { invoke(&system_instruction::transfer(wallet.key, account.key, needed),
            &[wallet.clone(), account.clone(), system.clone()])?; }
        account.resize(size)?;
    }
    Ok(())
}
fn ledger_address(program: &Pubkey, bank: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"stakes", bank.as_ref(), wallet.as_ref()], program)
}
fn position_address(program: &Pubkey, bank: &Pubkey, wallet: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"stake", bank.as_ref(), wallet.as_ref(), &nonce.to_le_bytes()], program)
}
pub(super) fn read_ledger(program: &Pubkey, account: &AccountInfo, bank: &Pubkey, profile: &Profile) -> Result<Ledger, ProgramError> {
    let (expected, bump) = ledger_address(program, bank, &profile.wallet);
    check(*account.key == expected, 60)?;
    if account.owner == &system_program::id() && account.data_is_empty() {
        // Existing stakes are automatically flexible at 1x; no bulk wallet migration.
        return Ok(Ledger {version: 1, bank: *bank, wallet: profile.wallet,
            shares: [profile.shares, 0, 0, 0, 0], next_nonce: 0, positions: 0, bump, forever_shares: 0});
    }
    check(account.owner == program, 2)?;
    let mut bytes = account.try_borrow_data()?.to_vec();
    if bytes.len() == LEDGER_LEN && bytes[0] == 1 { bytes.extend_from_slice(&[0; 16]); }
    let ledger = Ledger::try_from_slice(&bytes).map_err(|_| ProgramError::InvalidAccountData)?;
    let total = ledger.shares.iter().try_fold(ledger.forever_shares, |sum, shares| add128(sum, *shares))?;
    check(((ledger.version == 1 && account.data_len() == LEDGER_LEN) || (ledger.version == 2 && account.data_len() == FOREVER_LEDGER_LEN)) && ledger.bank == *bank && ledger.wallet == profile.wallet
        && ledger.bump == bump && total == profile.shares, 60)?;
    Ok(ledger)
}
pub(super) fn read_position(program: &Pubkey, account: &AccountInfo, bank: &Pubkey, wallet: &Pubkey, nonce: u64) -> Result<Position, ProgramError> {
    check(account.owner == program, 2)?;
    let position = Position::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (key, bump) = position_address(program, bank, wallet, nonce);
    check(*account.key == key && position.version == 1 && position.bank == *bank
        && position.wallet == *wallet && position.nonce == nonce && position.bump == bump
        && position.tier > 0 && position.tier as usize <= FOREVER_TIER, 60)?;
    check(unlock_time(position.created_at, position.tier as usize)? == position.unlock_at, 60)?;
    Ok(position)
}
fn shares_at(data: &[u8], offset: usize) -> Result<u128, ProgramError> {
    Ok(u128::from_le_bytes(data.get(offset..offset+16).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))
}

pub(super) fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data[0] == 30 { return upgrade(program, accounts, data); }
    let mut iter = accounts.iter();
    if data[0] == 27 {
        check(data.len() == 1, 9)?;
        let authority = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
        let pool_account = next_account_info(&mut iter)?; let table_account = next_account_info(&mut iter)?;
        let system = next_account_info(&mut iter)?;
        signer(authority)?; let bank = load_bank(program, bank_account)?;
        check(bank.authority == *authority.key && bank.version >= 3, 11)?;
        let table = shared::load_table(program, table_account, bank_account.key)?;
        check(bank.reserved == 0 && table.current == 0 && table.queued == 0, 64)?;
        let mut pool = load_pool(program, pool_account, bank_account.key)?;
        check(pool.active && pool.version == 1, 68)?;
        check(*system.key == system_program::id(), 7)?;
        let needed = Rent::get()?.minimum_balance(POWER_POOL_LEN).saturating_sub(pool_account.lamports());
        if needed > 0 { invoke(&system_instruction::transfer(authority.key, pool_account.key, needed),
            &[authority.clone(), pool_account.clone(), system.clone()])?; }
        pool_account.resize(POWER_POOL_LEN)?;
        pool.version = 2; pool.total_power = pool.total_shares;
        return write_pool(pool_account, &pool);
    }
    let tag = data[0];
    check((tag == 28 && data.len() == 18) || (tag == 29 && data.len() == 26), 9)?;
    let tier = data[1] as usize; check(tier <= FOREVER_TIER, 69)?;
    check(tag != 29 || tier != FOREVER_TIER, 70)?;
    let nonce = u64_at(data, 2)?;
    let wallet = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
    let pool_account = next_account_info(&mut iter)?; let profile_account = next_account_info(&mut iter)?;
    let mint = next_account_info(&mut iter)?; let vault = next_account_info(&mut iter)?;
    let tokens = next_account_info(&mut iter)?; let token = next_account_info(&mut iter)?;
    let system = next_account_info(&mut iter)?; let ledger_account = next_account_info(&mut iter)?;
    let position_account = next_account_info(&mut iter)?;
    signer(wallet)?; let bank = load_bank(program, bank_account)?;
    check(bank.version >= 3, 68)?;
    // No one can redeem unsettled equity or enter after seeing unresolved results.
    check(bank.reserved == 0, 64)?;
    check(*mint.key == bank.mint && *vault.key == bank.vault && tokens.key != vault.key, 14)?;
    let decimals = valid_mint(mint)?;
    let balance = token_account(vault, mint.key, bank_account.key)?;
    token_account(tokens, mint.key, wallet.key)?;
    let mut pool = load_pool(program, pool_account, bank_account.key)?;
    check(pool.active && pool.version == 2, 68)?;
    let mut profile = load_profile(program, profile_account, bank_account.key, wallet.key)?;
    let mut ledger = read_ledger(program, ledger_account, bank_account.key, &profile)?;
    ensure_ledger(program, wallet, ledger_account, system, bank_account.key, &mut ledger, tier)?;
    let old_power = ledger.power()?;
    profile.checkpoint_power(&pool, old_power)?;
    let now = Clock::get()?.unix_timestamp;
    if tag == 29 {
        let shares = shares_at(data, 10)?;
        check(shares > 0 && shares <= ledger.bucket(tier) && shares <= profile.shares && shares <= pool.total_shares, 65)?;
        if tier > 0 {
            let mut position = read_position(program, position_account, bank_account.key, wallet.key, nonce)?;
            check(position.tier as usize == tier && shares <= position.shares, 65)?;
            check(now >= position.unlock_at, 70)?;
            position.shares -= shares;
            if position.shares == 0 { ledger.positions = ledger.positions.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?; }
            write(position_account, &position)?;
        }
        let amount = amount64(mul128(shares, balance.into())? / pool.total_shares)?;
        *ledger.bucket_mut(tier) -= shares; profile.shares -= shares; pool.total_shares -= shares;
        if profile.shares == 0 { pool.stakers = sub(pool.stakers, 1)?; }
        profile.withdrawn = add128(profile.withdrawn, amount.into())?;
        if amount > 0 { transfer(vault, tokens, mint, bank_account, token, amount, decimals,
            Some(&[b"bank", bank.authority.as_ref(), bank.mint.as_ref(), &[bank.bump]]))?; }
    } else {
        check(nonce == ledger.next_nonce, 71)?;
        let (shares, amount) = {
            let amount = u64_at(data, 10)?; check(amount > 0, 65)?;
            let shares = if pool.total_shares == 0 { amount.into() } else {
                check(balance > 0, 66)?;
                mul128(amount.into(), pool.total_shares)? / u128::from(balance)
            };
            check(shares > 0, 65)?;
            if profile.shares == 0 { pool.stakers = add(pool.stakers, 1)?; }
            profile.shares = add128(profile.shares, shares)?; pool.total_shares = add128(pool.total_shares, shares)?;
            profile.deposited = add128(profile.deposited, amount.into())?;
            transfer(tokens, vault, mint, wallet, token, amount, decimals, None)?;
            (shares, amount)
        };
        *ledger.bucket_mut(tier) = add128(ledger.bucket(tier), shares)?;
        if tier > 0 {
            let (expected, bump) = position_address(program, bank_account.key, wallet.key, nonce);
            check(*position_account.key == expected, 60)?;
            create_pda(wallet, position_account, system, program, POSITION_LEN,
                &[b"stake", bank_account.key.as_ref(), wallet.key.as_ref(), &nonce.to_le_bytes(), &[bump]])?;
            let unlock_at = unlock_time(now, tier)?;
            write(position_account, &Position {version: 1, bank: *bank_account.key, wallet: *wallet.key, nonce,
                shares, deposited: amount, tier: tier as u8, created_at: now, unlock_at, bump})?;
            ledger.positions = ledger.positions.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        }
        ledger.next_nonce = add(ledger.next_nonce, 1)?;
    }
    let power = ledger.power()?;
    pool.total_power = add128(pool.total_power.checked_sub(old_power).ok_or(ProgramError::ArithmeticOverflow)?, power)?;
    profile.reward_debt = mul128(power, pool.reward_per_share)?;
    write(profile_account, &profile)?; write_ledger(ledger_account, &ledger)?; write_pool(pool_account, &pool)
}

/// Move existing equity into a longer lock without withdrawing any tokens.
/// Locked positions upgrade in full; source-tier/share preconditions reject stale approvals.
fn upgrade(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(data.len() == 27, 9)?;
    let tier = data[1] as usize; let source_tier = data[2] as usize;
    check(tier <= FOREVER_TIER && source_tier < tier, 69)?;
    let nonce = u64_at(data, 3)?; let shares = shares_at(data, 11)?;
    let mut iter = accounts.iter();
    let wallet = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
    let pool_account = next_account_info(&mut iter)?; let profile_account = next_account_info(&mut iter)?;
    let mint = next_account_info(&mut iter)?; let vault = next_account_info(&mut iter)?;
    let tokens = next_account_info(&mut iter)?; let _token = next_account_info(&mut iter)?;
    let system = next_account_info(&mut iter)?; let ledger_account = next_account_info(&mut iter)?;
    let position_account = next_account_info(&mut iter)?;
    signer(wallet)?; let bank = load_bank(program, bank_account)?;
    check(bank.version >= 3 && *mint.key == bank.mint && *vault.key == bank.vault, 68)?;
    valid_mint(mint)?; token_account(vault, mint.key, bank_account.key)?; token_account(tokens, mint.key, wallet.key)?;
    let mut pool = load_pool(program, pool_account, bank_account.key)?; check(pool.active && pool.version == 2, 68)?;
    let mut profile = load_profile(program, profile_account, bank_account.key, wallet.key)?;
    let mut ledger = read_ledger(program, ledger_account, bank_account.key, &profile)?;
    check(shares > 0 && shares <= ledger.bucket(source_tier), 65)?;
    ensure_ledger(program, wallet, ledger_account, system, bank_account.key, &mut ledger, tier)?;
    let old_power = ledger.power()?; profile.checkpoint_power(&pool, old_power)?;
    let now = Clock::get()?.unix_timestamp;
    let unlock_at = unlock_time(now, tier)?;
    let mut position = if source_tier == 0 {
        check(nonce == ledger.next_nonce, 71)?;
        let (key, bump) = position_address(program, bank_account.key, wallet.key, nonce); check(*position_account.key == key, 60)?;
        create_pda(wallet, position_account, system, program, POSITION_LEN,
            &[b"stake", bank_account.key.as_ref(), wallet.key.as_ref(), &nonce.to_le_bytes(), &[bump]])?;
        ledger.next_nonce = add(ledger.next_nonce, 1)?;
        ledger.positions = ledger.positions.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        Position { version:1, bank:*bank_account.key, wallet:*wallet.key, nonce, shares, deposited:0,
            tier:tier as u8, created_at:now, unlock_at, bump }
    } else {
        let p = read_position(program, position_account, bank_account.key, wallet.key, nonce)?;
        check(p.tier as usize == source_tier && p.shares == shares && (tier == FOREVER_TIER || unlock_at > p.unlock_at), 71)?; p
    };
    position.tier = tier as u8; position.created_at = now; position.unlock_at = unlock_at;
    *ledger.bucket_mut(source_tier) -= shares; *ledger.bucket_mut(tier) = add128(ledger.bucket(tier), shares)?;
    let power = ledger.power()?;
    pool.total_power = add128(pool.total_power.checked_sub(old_power).ok_or(ProgramError::ArithmeticOverflow)?, power)?;
    profile.reward_debt = mul128(power, pool.reward_per_share)?;
    write(position_account, &position)?; write_ledger(ledger_account, &ledger)?;
    write(profile_account, &profile)?; write_pool(pool_account, &pool)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn tiers_and_layouts() {
        let mut l = Ledger {version:1, bank:Pubkey::default(), wallet:Pubkey::default(), shares:[100;5], next_nonce:0, positions:4, bump:0, forever_shares:0};
        assert_eq!(l.power().unwrap(), 2100);
        assert_eq!(borsh::to_vec(&l).unwrap().len(), FOREVER_LEDGER_LEN);
        assert_eq!(FOREVER_LEDGER_LEN, LEDGER_LEN + 16);
        l.forever_shares = 100;
        assert_eq!(l.power().unwrap(), 4100);
        assert_eq!(unlock_time(i64::MAX, FOREVER_TIER).unwrap(), 0);
        let p = Position {version:1, bank:l.bank, wallet:l.wallet, nonce:0, shares:100, deposited:100, tier:4, created_at:0, unlock_at:2592000, bump:0};
        assert_eq!(borsh::to_vec(&p).unwrap().len(), POSITION_LEN);
        assert_eq!(DURATIONS, [0, 3600, 24*3600, 7*86400, 30*86400]);
    }
}
