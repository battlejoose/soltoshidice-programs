//! Owner-authorized, irreversible bank equity -> root Perps payout inventory.
//! Locked/Forever equity may convert, but normal withdrawal locks never change.
use super::*;
use solana_program::instruction::{AccountMeta, Instruction};

const PERPS: Pubkey = solana_program::pubkey!("7YWUnPaP5tQjJTC5kZu235P5Qix2VpoJk5rTMgkXeCw7");
const BANK: Pubkey = solana_program::pubkey!("9vBaKrxgtQEc6zavSVTvptAM2oRmX38SkyHXpfvUFoKR");
const SDICE: Pubkey = solana_program::pubkey!("4nCmpwne7hCoWTSpAd54uENmCgHJrHTyn4DMPCEMpump");
const MAGIC: Pubkey = solana_program::pubkey!("Magic11111111111111111111111111111111111111");
const CONTRIBUTE: [u8;8] = [238,61,229,10,130,18,35,70];
const WRITABLE: [usize;7] = [0,9,10,11,12,13,14];
fn pda(seeds: &[&[u8]]) -> Pubkey { Pubkey::find_program_address(seeds, &PERPS).0 }
fn uint128(data: &[u8], offset: usize) -> Result<u128, ProgramError> {
    Ok(u128::from_le_bytes(data.get(offset..offset+16).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))
}
/// Round UP the shares removed for an exact token amount. Remaining stakers
/// never subsidize rounding; max_shares protects approval against bank losses.
fn debit_shares(amount: u64, balance: u64, total: u128) -> Result<u128, ProgramError> {
    check(amount > 0 && balance >= amount && total > 0, 65)?;
    let numerator = mul128(amount.into(), total)?;
    add128(numerator / u128::from(balance), u128::from(numerator % u128::from(balance) != 0))
}

pub(super) fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    // bank pool/profile/ledger/position, Perps executable, then its exact 29-account ABI.
    check(data.len() == 66 && accounts.len() == 34, 9)?;
    let tier = data[1] as usize; check(tier <= locks::FOREVER_TIER, 69)?;
    let stake_nonce = u64_at(data,2)?; let amount = u64_at(data,10)?;
    let max_shares = uint128(data,18)?;
    let nonce = u64_at(data,34)?; let minimum = uint128(data,42)?;
    let expires = i64::from_le_bytes(data[58..66].try_into().unwrap());
    let now = Clock::get()?.unix_timestamp;
    check(minimum > 0 && expires >= now && expires <= now.saturating_add(120), 73)?;
    let pool_account = &accounts[0]; let profile_account = &accounts[1];
    let ledger_account = &accounts[2]; let position_account = &accounts[3];
    let executable = &accounts[4]; let a = &accounts[5..];
    let wallet = &a[1]; let bank_account = &a[2]; let mint = &a[7]; let vault = &a[9];
    signer(wallet)?; signer(&a[0])?;
    check(a[0].key == wallet.key && *bank_account.key == BANK && *mint.key == SDICE, 74)?;
    check(*executable.key == PERPS && executable.executable, 74)?;
    let bank = load_bank(program, bank_account)?;
    check(bank.version >= 3 && bank.mint == SDICE && bank.vault == *vault.key, 74)?;
    // Same equity protection as withdrawals: no redemption against unresolved games.
    check(bank.reserved == 0, 64)?;
    valid_mint(mint)?;
    let balance = token_account(vault, mint.key, bank_account.key)?;
    let root = pda(&[b"pool", &0u16.to_le_bytes()]); let asset = pda(&[b"asset", SDICE.as_ref()]);
    for (i, expected) in [
        (3,pda(&[b"config"])), (4,root), (5,asset), (6,pda(&[b"authority",root.as_ref()])),
        (8,spl_token_2022::id()), (10,pda(&[b"vault",root.as_ref(),asset.as_ref()])),
        (11,pda(&[b"inventory",root.as_ref(),asset.as_ref()])), (12,pda(&[b"ownership-pool",root.as_ref()])),
        (13,pda(&[b"ownership-trader",root.as_ref(),wallet.key.as_ref()])),
        (14,pda(&[b"pool-contribution",root.as_ref(),wallet.key.as_ref()])),
        (27,MAGIC), (28,system_program::id()),
    ] { check(*a[i].key == expected, 74)?; }
    // Destination token balance and all valuation/state constraints are checked
    // again by the pinned Perps program. It cannot receive arbitrary privileges.
    let mut pool = load_pool(program,pool_account,bank_account.key)?;
    check(pool.active && pool.version == 2, 68)?;
    let mut profile = load_profile(program,profile_account,bank_account.key,wallet.key)?;
    let mut ledger = locks::read_ledger(program,ledger_account,bank_account.key,&profile)?;
    let shares = debit_shares(amount,balance,pool.total_shares)?;
    check(shares > 0 && shares <= max_shares && shares <= ledger.bucket(tier)
        && shares <= profile.shares && shares <= pool.total_shares, 65)?;
    // Do not burn the final bank share for less than the whole vault: otherwise
    // rounding could leave tokens with no remaining equity owner.
    check(shares < pool.total_shares || amount == balance, 65)?;
    if tier == 0 { check(stake_nonce == 0 && position_account.key == ledger_account.key, 74)?; }
    let mut position = if tier > 0 {
        let p = locks::read_position(program,position_account,bank_account.key,wallet.key,stake_nonce)?;
        check(p.tier as usize == tier && shares <= p.shares, 65)?; Some(p)
    } else { None };
    locks::ensure_ledger(program,wallet,ledger_account,&a[28],bank_account.key,&mut ledger,tier)?;
    let old_power = ledger.power()?; profile.checkpoint_power(&pool,old_power)?;
    if let Some(p) = position.as_mut() {
        p.shares -= shares;
        if p.shares == 0 { ledger.positions = ledger.positions.checked_sub(1).ok_or(ProgramError::ArithmeticOverflow)?; }
    }
    *ledger.bucket_mut(tier) -= shares; profile.shares -= shares; pool.total_shares -= shares;
    if profile.shares == 0 { pool.stakers = sub(pool.stakers,1)?; }
    profile.withdrawn = add128(profile.withdrawn,amount.into())?;
    let power = ledger.power()?;
    pool.total_power = add128(pool.total_power.checked_sub(old_power).ok_or(ProgramError::ArithmeticOverflow)?,power)?;
    profile.reward_debt = mul128(power,pool.reward_per_share)?;
    // Persist before CPI; Solana rolls back these writes on ANY inner failure.
    // Fee credits/reserves remain claimable from the separate bank fee vault.
    write(profile_account,&profile)?; locks::write_ledger(ledger_account,&ledger)?; write_pool(pool_account,&pool)?;
    if let Some(p) = position { write(position_account,&p)?; }
    let mut payload = CONTRIBUTE.to_vec(); payload.extend_from_slice(&amount.to_le_bytes());
    payload.extend_from_slice(&nonce.to_le_bytes()); payload.extend_from_slice(&minimum.to_le_bytes());
    payload.extend_from_slice(&expires.to_le_bytes());
    let metas = a.iter().enumerate().map(|(i,account)| if WRITABLE.contains(&i) {
        AccountMeta::new(*account.key,i<=2)
    } else { AccountMeta::new_readonly(*account.key,i<=2) }).collect();
    let mut infos = a.to_vec(); infos.push(executable.clone());
    invoke_signed(&Instruction {program_id:PERPS,accounts:metas,data:payload}, &infos,
        &[&[b"bank",bank.authority.as_ref(),bank.mint.as_ref(),&[bank.bump]]])?;
    check(token_account(vault,mint.key,bank_account.key)? == balance - amount, 74)?;
    msg!("BANK_PERPS_CONTRIBUTION owner={} amount={} shares={} tier={} nonce={}",wallet.key,amount,shares,tier,nonce);
    Ok(())
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn conversion_rounds_against_the_contributor_not_remaining_stakers() {
        assert_eq!(debit_shares(1,3,10).unwrap(),4);
        assert_eq!(debit_shares(3,3,10).unwrap(),10);
        assert_eq!(debit_shares(10,100,1000).unwrap(),100);
        assert!(debit_shares(0,100,1000).is_err());
        assert!(debit_shares(101,100,1000).is_err());
        assert!(debit_shares(1,0,1000).is_err());
        assert!(debit_shares(u64::MAX,u64::MAX,u128::MAX).is_err());
    }
}
