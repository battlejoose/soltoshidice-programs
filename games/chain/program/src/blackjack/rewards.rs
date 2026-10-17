//! Only an accepted native initial bet can debit bank-funded blackjack rewards.
//! The caller cannot choose the wager, beneficiary, parent, or rent recipient.
use super::state::*;
use crate::*;

pub const RENT_SEED: &[u8] = b"blackjack-rewards-v1";
pub fn allocation(wager: u64) -> economy::Fees {
    let part = wager / 1000;
    economy::Fees { gross: wager, net: wager - part * 3,
        direct: part, staking: part, tree: part }
}

/// Appended bet accounts: pool, fee vault, event, receipt, player profile,
/// immutable direct profile, System Program, dedicated recyclable SOL reserve.
#[inline(never)]
pub fn fund<'a>(program: &Pubkey, a: &[AccountInfo<'a>], bank: &Bank,
    t: &Table, seat: usize) -> ProgramResult {
    check(a.len() == 15 && *a[13].key == system_program::id(), 9)?;
    let wallet = t.round_wallets[seat];
    let wager = t.round.players[seat].original;
    let fees = allocation(wager);
    let total = fees.gross - fees.net;
    // reconcile() has already locked the full native payout liability.
    check(token_account(&a[4], a[3].key, a[2].key)?
        .checked_sub(total).is_some_and(|balance| balance >= bank.reserved), 826)?;
    let event = economy::game_rewards::event_address(program, a[2].key, a[1].key, t.round.id, &wallet).0;
    let (receipt, bump) = economy::receipt_address(program, &event);
    check(*a[9].key == event && *a[10].key == receipt
        && a[10].owner == &system_program::id() && a[10].data_is_empty(), 67)?;
    let (reserve, reserve_bump) = Pubkey::find_program_address(&[RENT_SEED, a[1].key.as_ref()], program);
    check(*a[14].key == reserve && a[14].owner == &system_program::id() && a[14].data_is_empty(), 836)?;
    let rent = Rent::get()?.minimum_balance(economy::GAME_RECEIPT_LEN);
    let needed = rent.saturating_sub(a[10].lamports());
    check(a[14].lamports() >= needed, 836)?;
    // Prefunded/dusted receipt PDAs remain usable. Both signer domains are fixed.
    if needed > 0 {
        invoke_signed(&system_instruction::transfer(&reserve, &receipt, needed),
            &[a[14].clone(), a[10].clone(), a[13].clone()],
            &[&[RENT_SEED, a[1].key.as_ref(), &[reserve_bump]]])?;
    }
    let seeds: &[&[u8]] = &[b"fees", event.as_ref(), &[bump]];
    invoke_signed(&system_instruction::allocate(&receipt, economy::GAME_RECEIPT_LEN as u64),
        &[a[10].clone(), a[13].clone()], &[seeds])?;
    invoke_signed(&system_instruction::assign(&receipt, program),
        &[a[10].clone(), a[13].clone()], &[seeds])?;
    write(&a[10], &economy::FeeReceipt { version: 2, bank: *a[2].key, bet: event, player: wallet,
        next: Pubkey::default(), remaining: 0, direct: 0, staking: 0, tree: 0,
        gross: 0, collected: false, complete: false, bump })?;
    {
        let mut bytes = a[10].try_borrow_mut_data()?;
        bytes[172..204].copy_from_slice(reserve.as_ref());
        bytes[204..236].copy_from_slice(a[1].key.as_ref());
        bytes[236..244].copy_from_slice(&t.round.id.to_le_bytes());
        bytes[244..276].copy_from_slice(wallet.as_ref());
    }
    let mut profile = economy::load_profile(program, &a[11], a[2].key, &wallet)?;
    profile.bet_count = profile.bet_count.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
    profile.wagered = profile.wagered.checked_add(wager.into()).ok_or(ProgramError::ArithmeticOverflow)?;
    write(&a[11], &profile)?;
    economy::credit_rewards(program, bank, &a[2], &a[9], &wallet, &a[3], &a[4], &a[6],
        &a[7], &a[8], &a[10], &a[11], &a[12], fees, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn initial_wager_rewards_conserve_and_never_exceed_three_tenths_percent() {
        for wager in [0, 2, 998, 1000, 1002, 100_000_000, 1_000_000_000_000, u64::MAX] {
            let f = allocation(wager);
            assert_eq!(f.direct, wager / 1000);
            assert_eq!(f.direct, f.staking);
            assert_eq!(f.direct, f.tree);
            assert_eq!(f.net + f.direct + f.staking + f.tree, wager);
            assert!(u128::from(f.gross - f.net) <= u128::from(wager) * 3 / 1000);
        }
    }
}
