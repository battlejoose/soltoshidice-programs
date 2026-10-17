use super::*;

/// Fund a receipt at bet placement; settlement does not need the winner online.
pub fn prepare_bet<'a>(program: &Pubkey, bank: &AccountInfo<'a>, bet: &AccountInfo<'a>,
    player: &AccountInfo<'a>, system: &AccountInfo<'a>, pool_account: &AccountInfo<'a>,
    profile_account: &AccountInfo<'a>, receipt_account: &AccountInfo<'a>, wager: u64) -> ProgramResult {
    let pool = load_pool(program, pool_account, bank.key)?;
    check(pool.active && pool.total_shares > 0, 63)?;
    let mut profile = load_profile(program, profile_account, bank.key, player.key)?;
    let (expected, bump) = receipt_address(program, bet.key); check(expected == *receipt_account.key, 60)?;
    create_pda(player, receipt_account, system, program, RECEIPT_LEN, &[b"fees", bet.key.as_ref(), &[bump]])?;
    profile.bet_count = add(profile.bet_count, 1)?;
    profile.wagered = add128(profile.wagered, wager.into())?;
    write(profile_account, &profile)?;
    write(receipt_account, &FeeReceipt { version: 1, bank: *bank.key, bet: *bet.key, player: *player.key,
        next: Pubkey::default(), remaining: 0, direct: 0, staking: 0, tree: 0, gross: 0,
        collected: false, complete: false, bump })
}

/// v2 escrows profit fees. v3 has already funded rewards at acceptance and only
/// records the gross prize; the shared claim flag makes that update one-shot.
pub fn collect_fees<'a>(program: &Pubkey, bank: &Bank, bank_account: &AccountInfo<'a>,
    bet_account: &AccountInfo<'a>, bet: &shared::Bet, mint: &AccountInfo<'a>,
    vault: &AccountInfo<'a>, token: &AccountInfo<'a>, pool_account: &AccountInfo<'a>,
    fee_vault: &AccountInfo<'a>, receipt_account: &AccountInfo<'a>,
    player_account: &AccountInfo<'a>, direct_account: &AccountInfo<'a>) -> ProgramResult {
    if bet.version >= 3 {
        let mut pool = load_pool(program, pool_account, bank_account.key)?;
        let mut player = load_profile(program, player_account, bank_account.key, &bet.player)?;
        let receipt = load_receipt(program, receipt_account, bank_account.key, bet_account.key, &bet.player)?;
        let gross = if bet.version == 4 { banking::gross(bet.wager, bet.outcome, bet.dice)? } else { payout(bet.wager, bet.outcome)? };
        check(pool.active && receipt.collected && bet.payout == gross, 67)?;
        if bet.outcome == 1 {
            pool.gross_prizes = add128(pool.gross_prizes, bet.payout.into())?;
            player.gross_prizes = add128(player.gross_prizes, bet.payout.into())?;
        }
        write(player_account, &player)?;
        return write_pool(pool_account, &pool);
    }
    let f = fees(bet.wager, bet.outcome)?;
    check(bet.version == 2 && f.net == bet.payout, 67)?;
    credit_rewards(program, bank, bank_account, bet_account, &bet.player, mint, vault, token,
        pool_account, fee_vault, receipt_account, player_account, direct_account, f,
        if bet.outcome == 1 { f.gross } else { 0 })
}

/// Every accepted v3 wager funds rewards, regardless of its eventual outcome.
pub fn fund_bet_rewards<'a>(program: &Pubkey, bank: &Bank, bank_account: &AccountInfo<'a>,
    bet_account: &AccountInfo<'a>, player: &Pubkey, wager: u64, mint: &AccountInfo<'a>,
    vault: &AccountInfo<'a>, token: &AccountInfo<'a>, pool_account: &AccountInfo<'a>,
    fee_vault: &AccountInfo<'a>, receipt_account: &AccountInfo<'a>,
    player_account: &AccountInfo<'a>, direct_account: &AccountInfo<'a>) -> ProgramResult {
    check(bank.version >= 3, 67)?;
    credit_rewards(program, bank, bank_account, bet_account, player, mint, vault, token,
        pool_account, fee_vault, receipt_account, player_account, direct_account, bet_fees(wager)?, 0)
}

pub(crate) fn credit_rewards<'a>(program: &Pubkey, bank: &Bank, bank_account: &AccountInfo<'a>,
    bet_account: &AccountInfo<'a>, player_wallet: &Pubkey, mint: &AccountInfo<'a>,
    vault: &AccountInfo<'a>, token: &AccountInfo<'a>, pool_account: &AccountInfo<'a>,
    fee_vault: &AccountInfo<'a>, receipt_account: &AccountInfo<'a>,
    player_account: &AccountInfo<'a>, direct_account: &AccountInfo<'a>, f: Fees, gross_prize: u64) -> ProgramResult {
    let mut pool = load_pool(program, pool_account, bank_account.key)?;
    check(pool.active && *fee_vault.key == pool.fee_vault && fee_vault.key != vault.key, 63)?;
    let mut receipt = load_receipt(program, receipt_account, bank_account.key, bet_account.key, player_wallet)?;
    check(!receipt.collected, 67)?;
    let mut player = load_profile(program, player_account, bank_account.key, player_wallet)?;
    let referrer = if player.wallet == ROOT { ROOT } else { player.referrer };
    let mut direct = load_profile(program, direct_account, bank_account.key, &referrer)?;
    token_account(fee_vault, mint.key, pool_account.key)?;
    let total = f.gross - f.net;
    if total > 0 {
        transfer(vault, fee_vault, mint, bank_account, token, total, valid_mint(mint)?,
            Some(&[b"bank", bank.authority.as_ref(), bank.mint.as_ref(), &[bank.bump]]))?;
        pool.accrue(f.staking)?;
    }
    pool.fees_collected = add128(pool.fees_collected, total.into())?;
    pool.direct_fees = add128(pool.direct_fees, f.direct.into())?;
    pool.staking_fees = add128(pool.staking_fees, f.staking.into())?;
    pool.tree_fees = add128(pool.tree_fees, f.tree.into())?;
    pool.referral_reserve = add(pool.referral_reserve, f.direct)?;
    pool.gross_prizes = add128(pool.gross_prizes, gross_prize.into())?;
    player.gross_prizes = add128(player.gross_prizes, gross_prize.into())?;
    player.fees_paid = add128(player.fees_paid, total.into())?;
    let same = player_account.key == direct_account.key;
    let recipient = if same { &mut player } else { &mut direct };
    recipient.credit_direct(f.direct)?;
    receipt.next = if recipient.referrer == Pubkey::default() { ROOT } else { recipient.referrer };
    receipt.remaining = f.tree;
    if recipient.wallet == ROOT {
        recipient.credit_tree(f.tree)?;
        pool.referral_reserve = add(pool.referral_reserve, f.tree)?;
        receipt.remaining = 0;
    } else {
        pool.tree_reserve = add(pool.tree_reserve, f.tree)?;
        if receipt.remaining < 2 { receipt.next = ROOT; }
    }
    receipt.direct = f.direct; receipt.staking = f.staking; receipt.tree = f.tree;
    receipt.gross = gross_prize; receipt.collected = true; receipt.complete = receipt.remaining == 0;
    write(player_account, &player)?;
    if !same { write(direct_account, &direct)?; }
    write(receipt_account, &receipt)?; write_pool(pool_account, &pool)
}

pub(super) fn distribute(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(data.len() == 1 && accounts.len() >= 4 && accounts.len() <= 11, 9)?;
    let mut iter = accounts.iter();
    let bank_account = next_account_info(&mut iter)?; let pool_account = next_account_info(&mut iter)?;
    let receipt_account = next_account_info(&mut iter)?;
    load_bank(program, bank_account)?;
    let mut pool = load_pool(program, pool_account, bank_account.key)?;
    check(pool.active && receipt_account.owner == program, 63)?;
    let raw = decode_receipt(&receipt_account.try_borrow_data()?)?;
    let mut receipt = load_receipt(program, receipt_account, bank_account.key, &raw.bet, &raw.player)?;
    check(receipt.collected, 67)?;
    if receipt.complete { return Ok(()); }
    for account in iter {
        if receipt.complete { break; }
        let mut profile = load_profile(program, account, bank_account.key, &receipt.next)?;
        let amount = if profile.wallet == ROOT { receipt.remaining } else { receipt.remaining / 2 };
        check(amount > 0, 67)?;
        profile.credit_tree(amount)?;
        receipt.remaining = sub(receipt.remaining, amount)?;
        pool.tree_reserve = sub(pool.tree_reserve, amount)?;
        pool.referral_reserve = add(pool.referral_reserve, amount)?;
        receipt.complete = receipt.remaining == 0;
        receipt.next = if receipt.remaining < 2 || profile.referrer == Pubkey::default() { ROOT } else { profile.referrer };
        write(account, &profile)?;
    }
    write(receipt_account, &receipt)?; write_pool(pool_account, &pool)
}
