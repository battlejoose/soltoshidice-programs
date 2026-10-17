//! Immutable referral identities, bet-funded rewards, and bank equity shares.
use super::*;
mod staking;
mod settlement;
mod locks;
pub mod game_rewards;
pub use settlement::{prepare_bet, collect_fees, fund_bet_rewards};
pub(crate) use settlement::credit_rewards;

pub const ROOT: Pubkey = solana_program::pubkey!("6gkkRU5t8WfXHWNuc6zQ7FoQxybcigLvzJKLz7Z1tGg");
pub const PROFILE_LEN: usize = 391;
pub const POOL_LEN: usize = 259;
pub const POWER_POOL_LEN: usize = POOL_LEN + 16;
pub const RECEIPT_LEN: usize = 172;
/// Game receipts retain recovery identities until credits finish and rent is returned.
pub const GAME_RECEIPT_LEN: usize = RECEIPT_LEN + 32 + 32 + 8 + 32;
pub const REWARD_SCALE: u128 = 1_000_000_000_000_000_000;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Profile {
    pub version: u8, pub bank: Pubkey, pub wallet: Pubkey, pub referrer: Pubkey,
    pub joined_at: i64, pub depth: u32, pub direct_count: u64,
    pub name_len: u8, pub name: [u8; 32],
    pub bet_count: u64, pub wagered: u128, pub gross_prizes: u128, pub fees_paid: u128,
    pub direct_credit: u64, pub tree_credit: u64,
    pub direct_earned: u128, pub tree_earned: u128, pub referral_paid: u128,
    pub shares: u128, pub reward_debt: u128, pub reward_fraction: u128,
    pub stake_credit: u64, pub stake_earned: u128, pub stake_paid: u128,
    pub deposited: u128, pub withdrawn: u128, pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Pool {
    pub version: u8, pub bank: Pubkey, pub root: Pubkey, pub fee_vault: Pubkey,
    pub active: bool, pub total_shares: u128, pub reward_per_share: u128, pub reward_remainder: u128,
    pub stake_reserve: u64, pub referral_reserve: u64, pub tree_reserve: u64,
    pub gross_prizes: u128, pub fees_collected: u128,
    pub direct_fees: u128, pub staking_fees: u128, pub tree_fees: u128,
    pub stakers: u64, pub bump: u8, pub total_power: u128,
}
#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct FeeReceipt {
    pub version: u8, pub bank: Pubkey, pub bet: Pubkey, pub player: Pubkey,
    pub next: Pubkey, pub remaining: u64, pub direct: u64, pub staking: u64, pub tree: u64,
    pub gross: u64, pub collected: bool, pub complete: bool, pub bump: u8,
}
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fees { pub gross: u64, pub net: u64, pub direct: u64, pub staking: u64, pub tree: u64 }

fn add(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn add128(a: u128, b: u128) -> Result<u128, ProgramError> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn mul128(a: u128, b: u128) -> Result<u128, ProgramError> { a.checked_mul(b).ok_or(ProgramError::ArithmeticOverflow) }
fn sub(a: u64, b: u64) -> Result<u64, ProgramError> { a.checked_sub(b).ok_or(ProgramError::InsufficientFunds) }
fn amount64(a: u128) -> Result<u64, ProgramError> { u64::try_from(a).map_err(|_| ProgramError::ArithmeticOverflow) }

/// Legacy v2 payouts remain immutable across the v3 upgrade.
pub fn fees(wager: u64, outcome: u8) -> Result<Fees, ProgramError> {
    let gross = payout(wager, outcome)?;
    if outcome != 1 { return Ok(Fees { gross, net: gross, direct: 0, staking: 0, tree: 0 }); }
    let total = amount64(u128::from(wager) * 3 / 100)?;
    let direct = wager / 100;
    Ok(Fees { gross, net: sub(gross, total)?, direct, staking: direct, tree: total - direct * 2 })
}
/// Split an incoming wager; the bank still guarantees the full gross payout.
pub fn bet_fees(wager: u64) -> Result<Fees, ProgramError> {
    let total = amount64(u128::from(wager) * 3 / 100)?;
    let direct = wager / 100;
    Ok(Fees { gross: wager, net: sub(wager, total)?, direct, staking: direct, tree: total - direct * 2 })
}
pub fn profile_address(program: &Pubkey, bank: &Pubkey, wallet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"profile", bank.as_ref(), wallet.as_ref()], program)
}
pub fn pool_address(program: &Pubkey, bank: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"economy", bank.as_ref()], program)
}
pub fn receipt_address(program: &Pubkey, bet: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"fees", bet.as_ref()], program)
}
pub fn load_profile(program: &Pubkey, account: &AccountInfo, bank: &Pubkey, wallet: &Pubkey) -> Result<Profile, ProgramError> {
    check(account.owner == program, 2)?;
    let p = Profile::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = profile_address(program, bank, wallet);
    check(*account.key == expected && p.bank == *bank && p.wallet == *wallet && p.bump == bump && p.version == 1, 60)?;
    check((p.wallet == ROOT && p.referrer == Pubkey::default() && p.depth == 0)
        || (p.wallet != ROOT && p.referrer != Pubkey::default() && p.referrer != p.wallet && p.depth > 0), 61)?;
    Ok(p)
}
pub fn load_pool(program: &Pubkey, account: &AccountInfo, bank: &Pubkey) -> Result<Pool, ProgramError> {
    check(account.owner == program, 2)?;
    let mut bytes = account.try_borrow_data()?.to_vec();
    if bytes.len() == POOL_LEN && bytes[0] == 1 {
        bytes.extend_from_within(98..114); // Legacy total_shares becomes 1x power.
    }
    let p = Pool::try_from_slice(&bytes).map_err(|_| ProgramError::InvalidAccountData)?;
    let (expected, bump) = pool_address(program, bank);
    check(*account.key == expected && p.bank == *bank && p.root == ROOT && p.bump == bump
        && ((p.version == 1 && account.data_len() == POOL_LEN) || (p.version == 2 && account.data_len() == POWER_POOL_LEN)), 60)?;
    Ok(p)
}
fn write_pool(account: &AccountInfo, pool: &Pool) -> ProgramResult {
    let bytes = borsh::to_vec(pool).map_err(|_| ProgramError::InvalidAccountData)?;
    let size = if pool.version == 1 { POOL_LEN } else { POWER_POOL_LEN };
    check(account.data_len() == size, 60)?;
    account.try_borrow_mut_data()?.copy_from_slice(&bytes[..size]); Ok(())
}
pub fn load_receipt(program: &Pubkey, account: &AccountInfo, bank: &Pubkey, bet: &Pubkey, player: &Pubkey) -> Result<FeeReceipt, ProgramError> {
    check(account.owner == program, 2)?;
    let r = decode_receipt(&account.try_borrow_data()?)?;
    let (expected, bump) = receipt_address(program, bet);
    check(*account.key == expected && r.bank == *bank && r.bet == *bet && r.player == *player && r.bump == bump, 60)?;
    Ok(r)
}
fn decode_receipt(data: &[u8]) -> Result<FeeReceipt, ProgramError> {
    check((data.len() == RECEIPT_LEN && data[0] == 1)
        || (data.len() == GAME_RECEIPT_LEN && data[0] == 2), 60)?;
    FeeReceipt::try_from_slice(&data[..RECEIPT_LEN]).map_err(|_| ProgramError::InvalidAccountData)
}
impl Profile {
    pub fn new(bank: Pubkey, wallet: Pubkey, referrer: Pubkey, depth: u32, now: i64, bump: u8, name: &[u8]) -> Result<Self, ProgramError> {
        let mut p = Self { version: 1, bank, wallet, referrer, joined_at: now, depth, direct_count: 0,
            name_len: 0, name: [0;32], bet_count: 0, wagered: 0, gross_prizes: 0, fees_paid: 0,
            direct_credit: 0, tree_credit: 0, direct_earned: 0, tree_earned: 0, referral_paid: 0,
            shares: 0, reward_debt: 0, reward_fraction: 0, stake_credit: 0, stake_earned: 0,
            stake_paid: 0, deposited: 0, withdrawn: 0, bump };
        p.rename(name)?; Ok(p)
    }
    pub fn rename(&mut self, name: &[u8]) -> ProgramResult {
        let text = std::str::from_utf8(name).map_err(|_| ProgramError::Custom(62))?;
        check(!name.is_empty() && name.len() <= 32 && text.trim() == text && !text.chars().any(|c| c.is_control()), 62)?;
        self.name = [0; 32]; self.name[..name.len()].copy_from_slice(name); self.name_len = name.len() as u8; Ok(())
    }
    pub fn checkpoint(&mut self, pool: &Pool) -> ProgramResult {
        self.checkpoint_power(pool, self.shares)
    }
    pub fn checkpoint_power(&mut self, pool: &Pool, power: u128) -> ProgramResult {
        let accrued = mul128(power, pool.reward_per_share)?;
        let unpaid = accrued.checked_sub(self.reward_debt).ok_or(ProgramError::ArithmeticOverflow)?;
        let scaled = add128(unpaid, self.reward_fraction)?;
        let credit = amount64(scaled / REWARD_SCALE)?;
        self.reward_fraction = scaled % REWARD_SCALE;
        self.reward_debt = accrued;
        self.stake_credit = add(self.stake_credit, credit)?;
        self.stake_earned = add128(self.stake_earned, credit.into())?;
        Ok(())
    }
    fn credit_direct(&mut self, amount: u64) -> ProgramResult {
        self.direct_credit = add(self.direct_credit, amount)?;
        self.direct_earned = add128(self.direct_earned, amount.into())?; Ok(())
    }
    fn credit_tree(&mut self, amount: u64) -> ProgramResult {
        self.tree_credit = add(self.tree_credit, amount)?;
        self.tree_earned = add128(self.tree_earned, amount.into())?; Ok(())
    }
}
impl Pool {
    fn accrue(&mut self, amount: u64) -> ProgramResult {
        let denominator = if self.version == 1 { self.total_shares } else { self.total_power };
        check(denominator > 0, 63)?;
        let scaled = add128(mul128(amount.into(), REWARD_SCALE)?, self.reward_remainder)?;
        self.reward_per_share = add128(self.reward_per_share, scaled / denominator)?;
        self.reward_remainder = scaled % denominator;
        self.stake_reserve = add(self.stake_reserve, amount)?; Ok(())
    }
}

pub fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    if data.first() == Some(&71) { return game_rewards::process(program, accounts, data); }
    if data.first() == Some(&72) { return game_rewards::reclaim(program, accounts, data); }
    if data.first() == Some(&31) { return banking::activate(program, accounts, data); }
    if matches!(data.first(), Some(27..=30)) { return locks::process(program, accounts, data); }
    if matches!(data.first(), Some(21..=24) | Some(26)) { return staking::process(program, accounts, data); }
    if data.first() == Some(&25) { return settlement::distribute(program, accounts, data); }
    let mut iter = accounts.iter();
    match data.first().copied().ok_or(ProgramError::InvalidInstructionData)? {
        18 => {
            check(data.len() == 1, 9)?;
            let authority = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let pool_account = next_account_info(&mut iter)?; let root_account = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?; let fee_vault = next_account_info(&mut iter)?;
            let system = next_account_info(&mut iter)?;
            signer(authority)?; let bank = load_bank(program, bank_account)?;
            check(bank.authority == *authority.key && bank.version == 1 && *mint.key == bank.mint, 11)?;
            valid_mint(mint)?;
            let (key, bump) = pool_address(program, bank_account.key); check(*pool_account.key == key, 60)?;
            token_account(fee_vault, mint.key, &key)?;
            let (root, root_bump) = profile_address(program, bank_account.key, &ROOT); check(root == *root_account.key, 60)?;
            create_pda(authority, pool_account, system, program, POOL_LEN, &[b"economy", bank_account.key.as_ref(), &[bump]])?;
            create_pda(authority, root_account, system, program, PROFILE_LEN, &[b"profile", bank_account.key.as_ref(), ROOT.as_ref(), &[root_bump]])?;
            write(root_account, &Profile::new(*bank_account.key, ROOT, Pubkey::default(), 0, Clock::get()?.unix_timestamp, root_bump, b"SoltoshiDICE")?)?;
            write_pool(pool_account, &Pool { version: 1, bank: *bank_account.key, root: ROOT, fee_vault: *fee_vault.key,
                active: false, total_shares: 0, reward_per_share: 0, reward_remainder: 0, stake_reserve: 0,
                referral_reserve: 0, tree_reserve: 0, gross_prizes: 0, fees_collected: 0,
                direct_fees: 0, staking_fees: 0, tree_fees: 0, stakers: 0, bump, total_power: 0 })
        },
        19 => {
            check(data.len() > 1 && data.len() <= 33, 9)?;
            let wallet = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let pool_account = next_account_info(&mut iter)?; let profile_account = next_account_info(&mut iter)?;
            let parent_account = next_account_info(&mut iter)?; let parent_wallet = next_account_info(&mut iter)?;
            let system = next_account_info(&mut iter)?;
            signer(wallet)?; load_bank(program, bank_account)?; load_pool(program, pool_account, bank_account.key)?;
            check(*wallet.key != ROOT && wallet.key != parent_wallet.key, 61)?;
            let mut parent = load_profile(program, parent_account, bank_account.key, parent_wallet.key)?;
            let (expected, bump) = profile_address(program, bank_account.key, wallet.key); check(*profile_account.key == expected, 60)?;
            let depth = parent.depth.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            let profile = Profile::new(*bank_account.key, *wallet.key, parent.wallet, depth, Clock::get()?.unix_timestamp, bump, &data[1..])?;
            create_pda(wallet, profile_account, system, program, PROFILE_LEN, &[b"profile", bank_account.key.as_ref(), wallet.key.as_ref(), &[bump]])?;
            parent.direct_count = add(parent.direct_count, 1)?;
            write(profile_account, &profile)?; write(parent_account, &parent)
        },
        20 => {
            check(data.len() > 1 && data.len() <= 33, 9)?;
            let wallet = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let profile_account = next_account_info(&mut iter)?;
            signer(wallet)?; load_bank(program, bank_account)?;
            let mut profile = load_profile(program, profile_account, bank_account.key, wallet.key)?;
            profile.rename(&data[1..])?; write(profile_account, &profile)
        },
        _ => Err(ProgramError::InvalidInstructionData),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn incoming_rewards_conserve_wagers_and_do_not_depend_on_outcomes() {
        for wager in [0, 1, 33, 34, 99, 100, 101, 1_000_000_000_000, u64::MAX] {
            let f = bet_fees(wager).unwrap();
            assert_eq!(f.net + f.direct + f.staking + f.tree, wager);
            assert_eq!(u128::from(f.gross - f.net), u128::from(wager) * 3 / 100);
        }
        assert_eq!(bet_fees(1_000_000_000).unwrap().net, 970_000_000);
    }
    #[test]
    fn profit_fee_conserves_every_atomic_unit_and_leaves_refunds_alone() {
        assert_eq!(fees(1_000_000_000, 1).unwrap(), Fees { gross: 2_000_000_000, net: 1_970_000_000,
            direct: 10_000_000, staking: 10_000_000, tree: 10_000_000 });
        for wager in [1, 33, 34, 99, 100, 101, 1_000_000_000_000, u64::MAX / 4] {
            for outcome in 0..3 {
                let f = fees(wager, outcome).unwrap();
                assert_eq!(f.net + f.direct + f.staking + f.tree, f.gross);
                assert_eq!(f.gross - f.net, if outcome == 1 { (u128::from(wager) * 3 / 100) as u64 } else { 0 });
            }
        }
        assert!(fees(u64::MAX, 1).is_err());
    }
    #[test]
    fn profile_layout_and_names_are_bounded_and_utf8() {
        let mut p = Profile::new(Pubkey::new_unique(), Pubkey::new_unique(), ROOT, 1, 1, 1, b"Alice").unwrap();
        assert_eq!(borsh::to_vec(&p).unwrap().len(), PROFILE_LEN);
        assert!(p.rename(b"\nAlice").is_err()); assert!(p.rename(b"Alice ").is_err());
        assert!(p.rename(&[b'A'; 33]).is_err()); assert!(p.rename(&[255]).is_err());
        let referrer = p.referrer; p.rename("Zoë".as_bytes()).unwrap();
        assert_eq!(p.referrer, referrer); assert_eq!(p.name_len, 4);
    }
    fn pool() -> Pool {
        Pool { version: 1, bank: Pubkey::new_unique(), root: ROOT, fee_vault: Pubkey::new_unique(), active: true,
            total_shares: 4, reward_per_share: 0, reward_remainder: 0, stake_reserve: 0, referral_reserve: 0,
            tree_reserve: 0, gross_prizes: 0, fees_collected: 0, direct_fees: 0, staking_fees: 0, tree_fees: 0,
            stakers: 2, bump: 1, total_power: 4 }
    }
    #[test]
    fn rewards_are_proportional_do_not_replay_and_new_shares_do_not_take_old_rewards() {
        let mut pool = pool();
        let mut alice = Profile::new(pool.bank, Pubkey::new_unique(), ROOT, 1, 1, 1, b"Alice").unwrap();
        let mut bob = Profile::new(pool.bank, Pubkey::new_unique(), ROOT, 1, 1, 1, b"Bob").unwrap();
        alice.shares = 1; bob.shares = 3;
        pool.accrue(100).unwrap(); alice.checkpoint(&pool).unwrap(); bob.checkpoint(&pool).unwrap();
        assert_eq!((alice.stake_credit, bob.stake_credit), (25,75));
        assert_eq!((alice.stake_paid, bob.stake_paid), (0,0));
        alice.checkpoint(&pool).unwrap(); assert_eq!(alice.stake_credit,25);
        alice.shares = 2; pool.total_shares = 5;
        alice.reward_debt = alice.shares * pool.reward_per_share;
        pool.accrue(80).unwrap(); alice.checkpoint(&pool).unwrap(); bob.checkpoint(&pool).unwrap();
        assert_eq!((alice.stake_credit, bob.stake_credit), (57,123));
        assert_eq!(pool.stake_reserve, 180);
    }
    #[test]
    fn pool_and_receipt_layouts_match_sdk_sizes() {
        assert_eq!(borsh::to_vec(&pool()).unwrap().len(), POWER_POOL_LEN);
        let receipt = FeeReceipt { version:1, bank:Pubkey::new_unique(), bet:Pubkey::new_unique(), player:Pubkey::new_unique(),
            next:ROOT, remaining:1, direct:1, staking:1, tree:1, gross:200, collected:true, complete:false, bump:1 };
        assert_eq!(borsh::to_vec(&receipt).unwrap().len(), RECEIPT_LEN);
    }
}
