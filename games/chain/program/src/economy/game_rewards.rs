//! Incoming game fees use existing staking and immutable referral identities.
//! The source vault MUST fund the full amount in this transaction. This public
//! funding adapter cannot authorize a withdrawal from existing bank capital.
use super::*;

/// No new event account is needed: this key names a unique source/hand/winner.
pub fn event_address(program: &Pubkey, bank: &Pubkey, source: &Pubkey, nonce: u64, winner: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[b"game-fee", bank.as_ref(), source.as_ref(), &nonce.to_le_bytes(), winner.as_ref()], program)
}

pub fn split(amount: u64) -> Result<Fees, ProgramError> {
    check(amount > 0, 67)?;
    let quarter = amount / 4;
    // Any indivisible base-unit remainder stays in the bank; liabilities never
    // exceed the incoming transfer. Poker whole-chip fees divide exactly by 4.
    Ok(Fees { gross: amount, net: amount - quarter * 3,
        direct: quarter, staking: quarter, tree: quarter })
}

/// Tag 71, nonce:u64, total_fee_base_units:u64.
/// Accounts: payer(S,W), source authority(S), source token(W), winner wallet,
/// bank, mint, bank token(W), Token-2022, pool(W), reward token(W), event key,
/// fee receipt(W), winner profile(W, may not exist), direct profile(W),
/// root profile(W), System Program. Source must sign using its own PDA seeds
/// in CPI; the keeper's signature alone cannot debit a poker table vault.
pub(super) fn process<'a>(program: &Pubkey, a: &[AccountInfo<'a>], data: &[u8]) -> ProgramResult {
    check(a.len() == 16 && data.len() == 17 && data[0] == 71, 9)?;
    signer(&a[0])?; signer(&a[1])?;
    check(*a[7].key == spl_token_2022::id() && *a[15].key == system_program::id(), 4)?;
    let bank = load_bank(program, &a[4])?;
    check(bank.version >= 3 && bank.mint == *a[5].key && bank.vault == *a[6].key, 67)?;
    let decimals = valid_mint(&a[5])?;
    token_account(&a[2], a[5].key, a[1].key)?;
    token_account(&a[6], a[5].key, a[4].key)?;
    check(a[2].key != a[6].key && a[2].key != a[9].key, 67)?;
    let nonce = u64_at(data, 1)?;
    let amount = u64_at(data, 9)?;
    let fees = split(amount)?;
    let (event, _) = event_address(program, a[4].key, a[1].key, nonce, a[3].key);
    check(event == *a[10].key, 60)?;
    let (receipt, bump) = receipt_address(program, &event);
    check(receipt == *a[11].key && a[11].owner != program, 67)?;
    let (profile, _) = profile_address(program, a[4].key, a[3].key);
    check(profile == *a[12].key, 60)?;
    // A player who has not registered a bank profile uses the immutable root
    // referral. An existing profile can never be omitted to bypass its parent.
    let beneficiary = if a[12].owner == program {
        load_profile(program, &a[12], a[4].key, a[3].key)?;
        &a[12]
    } else {
        check(a[12].owner == &system_program::id() && a[12].data_is_empty(), 60)?;
        load_profile(program, &a[14], a[4].key, &ROOT)?;
        &a[14]
    };
    let player = if beneficiary.key == a[12].key { *a[3].key } else { ROOT };
    // Funding, receipt creation, and all reward credits roll back together.
    transfer(&a[2], &a[6], &a[5], &a[1], &a[7], amount, decimals, None)?;
    create_pda(&a[0], &a[11], &a[15], program, GAME_RECEIPT_LEN, &[b"fees", event.as_ref(), &[bump]])?;
    write(&a[11], &FeeReceipt { version: 2, bank: *a[4].key, bet: event, player,
        next: Pubkey::default(), remaining: 0, direct: 0, staking: 0, tree: 0,
        gross: 0, collected: false, complete: false, bump })?;
    // The ordinary receipt prefix stays compatible with tree crediting. These
    // immutable fields make recovery discoverable without a server-side queue.
    {
        let mut bytes = a[11].try_borrow_mut_data()?;
        bytes[172..204].copy_from_slice(a[0].key.as_ref());
        bytes[204..236].copy_from_slice(a[1].key.as_ref());
        bytes[236..244].copy_from_slice(&nonce.to_le_bytes());
        bytes[244..276].copy_from_slice(a[3].key.as_ref());
    }
    settlement::credit_rewards(program, &bank, &a[4], &a[10], &player, &a[5], &a[6], &a[7],
        &a[8], &a[9], &a[11], beneficiary, &a[13], fees, 0)
}

/// Tag 72. Bank, completed game receipt(W), original rent payer(W).
/// Permissionless recovery can pay ONLY the original payer, after the entire
/// tree has been credited. No source token authority or bank withdrawal exists.
pub(super) fn reclaim(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(data == [72] && a.len() == 3, 9)?;
    load_bank(program, &a[0])?;
    check(a[1].owner == program && a[1].data_len() == GAME_RECEIPT_LEN && a[1].is_writable
        && a[2].is_writable && a[1].key != a[2].key, 60)?;
    {
        let bytes = a[1].try_borrow_data()?;
        let raw = decode_receipt(&bytes)?;
        let source = Pubkey::new_from_array(bytes[204..236].try_into().unwrap());
        let nonce = u64_at(&bytes, 236)?;
        let winner = Pubkey::new_from_array(bytes[244..276].try_into().unwrap());
        let event = event_address(program, a[0].key, &source, nonce, &winner).0;
        let receipt = load_receipt(program, &a[1], a[0].key, &event, &raw.player)?;
        check(receipt.version == 2 && receipt.collected && receipt.complete && receipt.remaining == 0
            && bytes[172..204] == a[2].key.to_bytes(), 67)?;
    }
    let rent = a[1].lamports();
    **a[2].try_borrow_mut_lamports()? = add(a[2].lamports(), rent)?;
    **a[1].try_borrow_mut_lamports()? = 0;
    a[1].realloc(0, false)?;
    a[1].assign(&system_program::id());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn four_buckets_conserve_every_incoming_base_unit() {
        for amount in [1, 2, 3, 4, 5, 99, 1_000_000, u64::MAX] {
            let f = split(amount).unwrap();
            assert_eq!(f.net + f.direct + f.staking + f.tree, amount);
            assert_eq!(f.direct, amount / 4);
            assert_eq!(f.direct, f.staking);
            assert_eq!(f.direct, f.tree);
        }
        assert_eq!(split(1_000_000).unwrap(), Fees {
            gross: 1_000_000, net: 250_000, direct: 250_000, staking: 250_000, tree: 250_000,
        });
        assert!(split(0).is_err());
    }
    #[test]
    fn receipts_bind_the_bank_source_hand_and_actual_winner() {
        let keys: Vec<_> = (0..5).map(|n| Pubkey::new_from_array([n; 32])).collect();
        let base = event_address(&keys[0], &keys[1], &keys[2], 1, &keys[3]).0;
        assert_ne!(base, event_address(&keys[0], &keys[4], &keys[2], 1, &keys[3]).0);
        assert_ne!(base, event_address(&keys[0], &keys[1], &keys[4], 1, &keys[3]).0);
        assert_ne!(base, event_address(&keys[0], &keys[1], &keys[2], 2, &keys[3]).0);
        assert_ne!(base, event_address(&keys[0], &keys[1], &keys[2], 1, &keys[4]).0);
    }
}
