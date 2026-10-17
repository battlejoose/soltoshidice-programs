use super::*;

fn u128_at(data: &[u8], offset: usize) -> Result<u128, ProgramError> {
    Ok(u128::from_le_bytes(data.get(offset..offset+16).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))
}

pub(super) fn process(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    let mut iter = accounts.iter();
    match data[0] {
        26 => {
            check(data.len() == 1, 9)?;
            let authority = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let pool_account = next_account_info(&mut iter)?; let table_account = next_account_info(&mut iter)?;
            signer(authority)?; let mut bank = load_bank(program, bank_account)?;
            check(*authority.key == bank.authority && bank.version == 2, 11)?;
            let pool = load_pool(program, pool_account, bank_account.key)?;
            let table = shared::load_table(program, table_account, bank_account.key)?;
            check(pool.active, 63)?;
            check(bank.reserved == 0 && table.current == 0 && table.queued == 0, 64)?;
            // No balances, shares, parents, or outstanding reward credits are reset.
            bank.version = 3;
            write(bank_account, &bank)
        },
        21 => {
            check(data.len() == 1, 9)?;
            let authority = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let pool_account = next_account_info(&mut iter)?; let owner_account = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?; let vault = next_account_info(&mut iter)?;
            let table_account = next_account_info(&mut iter)?;
            signer(authority)?; let mut bank = load_bank(program, bank_account)?;
            check(*authority.key == bank.authority && bank.version == 1, 11)?;
            let table = shared::load_table(program, table_account, bank_account.key)?;
            check(bank.reserved == 0 && table.current == 0 && table.queued == 0, 64)?;
            check(*mint.key == bank.mint && *vault.key == bank.vault, 14)?;
            valid_mint(mint)?; let balance = token_account(vault, mint.key, bank_account.key)?;
            let mut pool = load_pool(program, pool_account, bank_account.key)?;
            let mut owner = load_profile(program, owner_account, bank_account.key, authority.key)?;
            check(!pool.active && pool.total_shares == 0 && owner.shares == 0, 63)?;
            // An empty bank opens for player staking without granting house equity.
            pool.active = true; pool.total_shares = balance.into(); pool.stakers = u64::from(balance > 0);
            owner.shares = balance.into(); owner.deposited = balance.into();
            bank.version = 2;
            write(owner_account, &owner)?; write_pool(pool_account, &pool)?; write(bank_account, &bank)
        },
        tag @ (22 | 23) => {
            check(data.len() == if tag == 22 { 9 } else { 17 }, 9)?;
            let wallet = next_account_info(&mut iter)?; let bank_account = next_account_info(&mut iter)?;
            let pool_account = next_account_info(&mut iter)?; let profile_account = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?; let vault = next_account_info(&mut iter)?;
            let destination = next_account_info(&mut iter)?; let token = next_account_info(&mut iter)?;
            signer(wallet)?; let bank = load_bank(program, bank_account)?;
            check(bank.version >= 2 && bank.reserved == 0, 64)?;
            check(*mint.key == bank.mint && *vault.key == bank.vault && vault.key != destination.key, 14)?;
            let decimals = valid_mint(mint)?;
            let balance = token_account(vault, mint.key, bank_account.key)?;
            token_account(destination, mint.key, wallet.key)?;
            let mut pool = load_pool(program, pool_account, bank_account.key)?;
            check(pool.active && pool.version == 1, 68)?;
            let mut profile = load_profile(program, profile_account, bank_account.key, wallet.key)?;
            profile.checkpoint(&pool)?;
            if tag == 22 {
                let amount = u64_at(data, 1)?; check(amount > 0, 65)?;
                let shares = if pool.total_shares == 0 { u128::from(amount) } else {
                    check(balance > 0, 66)?; mul128(amount.into(), pool.total_shares)? / u128::from(balance)
                };
                check(shares > 0, 65)?;
                if profile.shares == 0 { pool.stakers = add(pool.stakers, 1)?; }
                profile.shares = add128(profile.shares, shares)?;
                pool.total_shares = add128(pool.total_shares, shares)?;
                profile.deposited = add128(profile.deposited, amount.into())?;
                transfer(destination, vault, mint, wallet, token, amount, decimals, None)?;
            } else {
                let shares = u128_at(data, 1)?;
                check(shares > 0 && shares <= profile.shares && shares <= pool.total_shares, 65)?;
                let amount = amount64(mul128(shares, balance.into())? / pool.total_shares)?;
                // Worthless shares may be burned after insolvency so new capital can enter.
                profile.shares -= shares; pool.total_shares -= shares;
                if profile.shares == 0 { pool.stakers = sub(pool.stakers, 1)?; }
                profile.withdrawn = add128(profile.withdrawn, amount.into())?;
                if amount > 0 { transfer(vault, destination, mint, bank_account, token, amount, decimals,
                    Some(&[b"bank", bank.authority.as_ref(), bank.mint.as_ref(), &[bank.bump]]))?; }
            }
            profile.reward_debt = mul128(profile.shares, pool.reward_per_share)?;
            write(profile_account, &profile)?; write_pool(pool_account, &pool)
        },
        24 => {
            check(data.len() == 2 && data[1] > 0 && data[1] <= 3, 9)?;
            let bank_account = next_account_info(&mut iter)?; let pool_account = next_account_info(&mut iter)?;
            let profile_account = next_account_info(&mut iter)?; let wallet = next_account_info(&mut iter)?;
            let mint = next_account_info(&mut iter)?; let fee_vault = next_account_info(&mut iter)?;
            let destination = next_account_info(&mut iter)?; let token = next_account_info(&mut iter)?;
            let bank = load_bank(program, bank_account)?;
            let mut pool = load_pool(program, pool_account, bank_account.key)?;
            check(pool.active && *mint.key == bank.mint && *fee_vault.key == pool.fee_vault && destination.key != fee_vault.key, 63)?;
            let mut profile = load_profile(program, profile_account, bank_account.key, wallet.key)?;
            // Rewards are claimed only by a transaction signed by their beneficiary.
            signer(wallet)?;
            token_account(destination, mint.key, wallet.key)?;
            token_account(fee_vault, mint.key, pool_account.key)?;
            let power = if pool.version == 2 {
                let ledger = next_account_info(&mut iter)?;
                locks::read_ledger(program, ledger, bank_account.key, &profile)?.power()?
            } else { profile.shares };
            profile.checkpoint_power(&pool, power)?;
            let mut amount = 0;
            if data[1] & 1 != 0 {
                let referral = add(profile.direct_credit, profile.tree_credit)?;
                pool.referral_reserve = sub(pool.referral_reserve, referral)?;
                profile.referral_paid = add128(profile.referral_paid, referral.into())?;
                profile.direct_credit = 0; profile.tree_credit = 0; amount = referral;
            }
            if data[1] & 2 != 0 {
                pool.stake_reserve = sub(pool.stake_reserve, profile.stake_credit)?;
                profile.stake_paid = add128(profile.stake_paid, profile.stake_credit.into())?;
                amount = add(amount, profile.stake_credit)?; profile.stake_credit = 0;
            }
            if amount > 0 { transfer(fee_vault, destination, mint, pool_account, token, amount, valid_mint(mint)?,
                Some(&[b"economy", bank_account.key.as_ref(), &[pool.bump]]))?; }
            write(profile_account, &profile)?; write_pool(pool_account, &pool)
        },
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
