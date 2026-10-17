//! One authorized, empty-table cutover. Historical events and custody accounts
//! are never rewritten or replaced by this migration.
use anchor_lang::prelude::*;
use anchor_spl::token_interface::TokenAccount;
use crate::{errors::RiverproofError, state::*, pot_settlement::{PokerPotSettlement, POT_SEED, native_fees},
    tournament::{accounts::*, legacy::read_legacy_arena}};
use super::table::assert_chips_conserved;

#[derive(Accounts)]
pub struct MigratePokerArena<'info> {
    #[account(mut)]
    pub operator: Signer<'info>,
    /// CHECK: exact legacy owner/layout, canonical operator seed and bump below.
    #[account(mut, owner = crate::ID)]
    pub arena: UncheckedAccount<'info>,
    #[account(mut)]
    pub table_d: Box<Account<'info, TableConfig>>,
    #[account(init, payer = operator, space = 8 + PokerPotSettlement::INIT_SPACE,
        seeds = [POT_SEED, table_d.key().as_ref()], bump)]
    pub pot_d: Box<Account<'info, PokerPotSettlement>>,
    pub system_program: Program<'info, System>,
}

/// Remaining accounts, all readonly: for each original table in its unchanged
/// index order, config/state/hand/vault/pot; then fourth state/hand/vault.
pub fn migrate_poker_arena(ctx: Context<MigratePokerArena>) -> Result<()> {
    require!(ctx.remaining_accounts.len() == 18, RiverproofError::InvalidTournament);
    let old = read_legacy_arena(&ctx.accounts.arena.try_borrow_data()?)?;
    let operator = ctx.accounts.operator.key();
    let (arena_key, arena_bump) = Pubkey::find_program_address(&[ARENA_SEED, operator.as_ref()], &crate::ID);
    require!(old.operator == operator && old.bump == arena_bump && arena_key == ctx.accounts.arena.key(),
        RiverproofError::InvalidTournament);
    require!(old.active == Pubkey::default() && !old.running && old.starts_at == 0
        && old.cash_closes_at == 0 && old.ends_at == 0 && old.drained == 0,
        RiverproofError::TournamentAlreadyScheduled);
    require!(old.tables.iter().enumerate().all(|(i, table)| !old.tables[..i].contains(table))
        && !old.tables.contains(&ctx.accounts.table_d.key())
        && old.completed_hand.iter().zip(old.reconciled).all(|(done, recorded)| *done <= recorded),
        RiverproofError::InvalidTournament);
    for i in 0..3 {
        let group = &ctx.remaining_accounts[i * 5..i * 5 + 5];
        require_keys_eq!(*group[0].key, old.tables[i], RiverproofError::InvalidTournament);
        let config = read_owned::<TableConfig>(&group[0])?;
        let pot = read_owned::<PokerPotSettlement>(&group[4])?;
        require!(config.join_key == arena_key && same_rules(&config, &old.cash_rules[i]),
            RiverproofError::InvalidTournament);
        check_quiet_table(*group[0].key, &config, &group[1], &group[2], &group[3],
            *group[4].key, &pot, operator, old.mint)?;
    }
    let config = &mut ctx.accounts.table_d;
    require!(config.join_key == Pubkey::default() && config.table_id == 100_000
        && config.buy_in == 100_000 && config.small_blind == 1_000 && config.big_blind == 2_000
        && config.ante == 0 && config.action_time_ms == crate::rules::ACTION_TIME_MS,
        RiverproofError::InvalidTournament);
    let fourth = config.key();
    let fourth_pot = PokerPotSettlement { table: fourth, hand: 0, paid: 0, bump: ctx.bumps.pot_d };
    check_quiet_table(fourth, config, &ctx.remaining_accounts[15], &ctx.remaining_accounts[16],
        &ctx.remaining_accounts[17], ctx.accounts.pot_d.key(), &fourth_pot, operator, old.mint)?;
    let fourth_rules = CashRules { buy_in: config.buy_in, small_blind: config.small_blind,
        big_blind: config.big_blind, ante: config.ante };
    let new = PokerArena {
        operator: old.operator, mint: old.mint,
        tables: [old.tables[0], old.tables[1], old.tables[2], fourth],
        bank_program: old.bank_program, bank: old.bank,
        cash_rules: [old.cash_rules[0], old.cash_rules[1], old.cash_rules[2], fourth_rules],
        running: old.running, ends_at: old.ends_at,
        reconciled: [old.reconciled[0], old.reconciled[1], old.reconciled[2], 0],
        decision_counter: old.decision_counter,
        completed_hand: [old.completed_hand[0], old.completed_hand[1], old.completed_hand[2], 0],
        completion_order: [old.completion_order[0], old.completion_order[1], old.completion_order[2], 0],
        active: old.active, next_id: old.next_id, starts_at: old.starts_at,
        cash_closes_at: old.cash_closes_at, drained: old.drained, bump: old.bump,
    };
    let size = 8 + PokerArena::INIT_SPACE;
    let top_up = Rent::get()?.minimum_balance(size).saturating_sub(ctx.accounts.arena.lamports());
    if top_up > 0 {
        anchor_lang::system_program::transfer(CpiContext::new(ctx.accounts.system_program.key(),
            anchor_lang::system_program::Transfer { from: ctx.accounts.operator.to_account_info(),
                to: ctx.accounts.arena.to_account_info() }), top_up)?;
    }
    ctx.accounts.arena.resize(size)?;
    new.try_serialize(&mut &mut ctx.accounts.arena.try_borrow_mut_data()?[..])?;
    ctx.accounts.table_d.join_key = arena_key;
    ctx.accounts.pot_d.set_inner(fourth_pot);
    Ok(())
}

fn same_rules(config: &TableConfig, rules: &CashRules) -> bool {
    config.buy_in == rules.buy_in && config.small_blind == rules.small_blind
        && config.big_blind == rules.big_blind && config.ante == rules.ante
}

fn read_owned<T: AccountDeserialize>(info: &AccountInfo) -> Result<Box<T>> {
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidTournament);
    Ok(Box::new(T::try_deserialize(&mut &info.try_borrow_data()?[..])?))
}

#[inline(never)]
fn check_quiet_table(key: Pubkey, config: &TableConfig, state_info: &AccountInfo,
    hand_info: &AccountInfo, vault_info: &AccountInfo, pot_key: Pubkey, pot: &PokerPotSettlement,
    operator: Pubkey, mint: Pubkey) -> Result<()> {
    require!(config.creator == operator && config.mint == mint && config.max_players == 10,
        RiverproofError::InvalidTournament);
    require_keys_eq!(key, Pubkey::find_program_address(&[TABLE_SEED, operator.as_ref(),
        &config.table_id.to_le_bytes()], &crate::ID).0, RiverproofError::InvalidTournament);
    for (address, seed) in [(state_info.key, TABLE_STATE_SEED), (hand_info.key, HAND_SEED), (vault_info.key, VAULT_SEED)] {
        require_keys_eq!(*address, Pubkey::find_program_address(&[seed, key.as_ref()], &crate::ID).0,
            RiverproofError::InvalidTournament);
    }
    require_keys_eq!(pot_key, Pubkey::find_program_address(&[POT_SEED, key.as_ref()], &crate::ID).0,
        RiverproofError::InvalidTournament);
    let state = read_owned::<TableState>(state_info)?;
    let hand = read_owned::<HandState>(hand_info)?;
    require!(state.table == key && hand.table == key && pot.table == key,
        RiverproofError::InvalidTournament);
    require!(state.escrowed == 0 && state.seats.iter().all(|seat| !seat.occupied), RiverproofError::TableNotEmpty);
    require!(matches!(hand.stage, HandStage::Idle | HandStage::Complete)
        && (hand.stage != HandStage::Complete || hand.settled), RiverproofError::HandInProgress);
    assert_chips_conserved(&state, &hand)?;
    let fee_mask = native_fees(&hand)?.iter().enumerate().fold(0u16,
        |mask, (i, fee)| mask | if fee.fee_chips > 0 { 1 << i } else { 0 });
    require!(fee_mask == 0 || (pot.hand == hand.hand_id && pot.paid & fee_mask == fee_mask),
        RiverproofError::PotFeesPending);
    require_keys_eq!(*vault_info.owner, anchor_spl::token_2022::ID, RiverproofError::InvalidTournament);
    let vault = TokenAccount::try_deserialize(&mut &vault_info.try_borrow_data()?[..])?;
    // Anyone can donate tokens to a vault. Zero native liability is required
    // above, but a donated surplus must neither block migration nor be swept.
    require!(vault.mint == mint && vault.owner == *vault_info.key,
        RiverproofError::VaultUnderfunded);
    Ok(())
}
