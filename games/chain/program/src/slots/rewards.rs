//! Reward funding is part of successful native settlement, including losing
//! spins. Voids never earn rewards. The ordinary game-fee receipt provides
//! durable, permissionless tree distribution and original-payer rent recovery.
use super::*;
pub(super) fn prepare<'a>(
    program: &Pubkey,
    player: &AccountInfo<'a>,
    system: &AccountInfo<'a>,
    account: &AccountInfo<'a>,
    event: &AccountInfo<'a>,
    s: &Spin,
) -> ProgramResult {
    let expected =
        economy::game_rewards::event_address(program, &s.bank, &s.config, s.nonce, &s.player).0;
    let (receipt, bump) = economy::receipt_address(program, &expected);
    check(
        *event.key == expected && *account.key == receipt && s.player == *player.key,
        911,
    )?;
    create_pda(
        player,
        account,
        system,
        program,
        economy::GAME_RECEIPT_LEN,
        &[b"fees", expected.as_ref(), &[bump]],
    )?;
    write(
        account,
        &economy::FeeReceipt {
            version: 2,
            bank: s.bank,
            bet: expected,
            player: s.player,
            next: Pubkey::default(),
            remaining: 0,
            direct: 0,
            staking: 0,
            tree: 0,
            gross: 0,
            collected: false,
            complete: false,
            bump,
        },
    )?;
    let mut b = account.try_borrow_mut_data()?;
    b[172..204].copy_from_slice(s.player.as_ref());
    b[204..236].copy_from_slice(s.config.as_ref());
    b[236..244].copy_from_slice(&s.nonce.to_le_bytes());
    b[244..276].copy_from_slice(s.player.as_ref());
    Ok(())
}
fn validate(
    program: &Pubkey,
    account: &AccountInfo,
    event: &AccountInfo,
    s: &Spin,
) -> ProgramResult {
    check(
        *event.key
            == economy::game_rewards::event_address(
                program, &s.bank, &s.config, s.nonce, &s.player,
            )
            .0,
        911,
    )?;
    let r = economy::load_receipt(program, account, &s.bank, event.key, &s.player)?;
    check(
        r.version == 2 && !r.collected && !r.complete && r.remaining == 0,
        911,
    )?;
    Ok(())
}
pub(super) fn credit<'a>(
    program: &Pubkey,
    a: &[AccountInfo<'a>],
    bank: &Bank,
    s: &Spin,
) -> ProgramResult {
    check(s.status == SETTLED, 905)?;
    validate(program, &a[9], &a[10], s)?;
    let mut profile = economy::load_profile(program, &a[11], &s.bank, &s.player)?;
    profile.bet_count = profile
        .bet_count
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    profile.wagered = profile
        .wagered
        .checked_add(s.wager.into())
        .ok_or(ProgramError::ArithmeticOverflow)?;
    write(&a[11], &profile)?;
    // Reuse the actual existing dice accounting, including base-unit rounding,
    // immutable parent checks, stake-power index, reserves and tree continuation.
    economy::credit_rewards(
        program,
        bank,
        &a[0],
        &a[10],
        &s.player,
        &a[3],
        &a[4],
        &a[6],
        &a[7],
        &a[8],
        &a[9],
        &a[11],
        &a[12],
        economy::bet_fees(s.wager)?,
        s.gross,
    )
}
pub(super) fn cancel(
    program: &Pubkey,
    account: &AccountInfo,
    event: &AccountInfo,
    player: &AccountInfo,
    s: &Spin,
) -> ProgramResult {
    check(s.status == REFUNDED && s.player == *player.key, 905)?;
    validate(program, account, event, s)?;
    return_rent(account, player)
}
