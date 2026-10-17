//! Native paid admission and reconciliation. Prize custody stays separate.
use anchor_lang::prelude::*;
use anchor_spl::token_interface::{Mint, TokenAccount};
use anchor_spl::token_2022::{self as token, Token2022, TransferChecked};
use crate::{errors::RiverproofError, state::*, sdice::{base_units, valid_mint, DECIMALS},
    tournament::{accounts::*, model::{Phase, StackResult, EntryStatus, TABLES, DRAINED_TABLES}},
    pot_settlement::{native_fees, require_paid, PokerPotSettlement, POT_SEED}};
use super::table::assert_chips_conserved;

#[derive(Accounts)]
pub struct ActivatePokerTournament<'info> {
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active,
        constraint = tournament.arena == arena.key() @ RiverproofError::InvalidTournament)]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(mut, address = arena.tables[0], constraint = table_a.join_key == arena.key())]
    pub table_a: Box<Account<'info, TableConfig>>,
    #[account(mut, address = arena.tables[1], constraint = table_b.join_key == arena.key())]
    pub table_b: Box<Account<'info, TableConfig>>,
    #[account(mut, address = arena.tables[2], constraint = table_c.join_key == arena.key())]
    pub table_c: Box<Account<'info, TableConfig>>,
    #[account(seeds = [TABLE_STATE_SEED, table_a.key().as_ref()], bump = state_a.bump)]
    pub state_a: Box<Account<'info, TableState>>,
    #[account(seeds = [TABLE_STATE_SEED, table_b.key().as_ref()], bump = state_b.bump)]
    pub state_b: Box<Account<'info, TableState>>,
    #[account(seeds = [TABLE_STATE_SEED, table_c.key().as_ref()], bump = state_c.bump)]
    pub state_c: Box<Account<'info, TableState>>,
    #[account(mut, address = arena.tables[3], constraint = table_d.join_key == arena.key())]
    pub table_d: Box<Account<'info, TableConfig>>,
    #[account(seeds = [TABLE_STATE_SEED, table_d.key().as_ref()], bump = state_d.bump)]
    pub state_d: Box<Account<'info, TableState>>,
}

pub fn activate_poker_tournament(ctx: Context<ActivatePokerTournament>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(now >= ctx.accounts.arena.starts_at && ctx.accounts.arena.drained == DRAINED_TABLES,
        RiverproofError::TournamentNotDue);
    for state in [&ctx.accounts.state_a, &ctx.accounts.state_b, &ctx.accounts.state_c, &ctx.accounts.state_d] {
        require!(state.escrowed == 0 && state.seats.iter().all(|seat| !seat.occupied), RiverproofError::TableNotEmpty);
    }
    ctx.accounts.tournament.state.activate(now).map_err(|_| error!(RiverproofError::InvalidTournament))?;
    let buy_in = ctx.accounts.tournament.state.buy_in;
    let (small_blind, big_blind) = crate::tournament::blinds::blinds_at(buy_in, ctx.accounts.arena.starts_at, now)
        .map_err(|_| error!(RiverproofError::InvalidTournament))?;
    for config in [&mut ctx.accounts.table_a, &mut ctx.accounts.table_b, &mut ctx.accounts.table_c, &mut ctx.accounts.table_d] {
        require!(config.mint == ctx.accounts.arena.mint && config.creator == ctx.accounts.arena.operator,
            RiverproofError::InvalidTournament);
        config.buy_in = buy_in; config.small_blind = small_blind;
        config.big_blind = big_blind; config.ante = 0; config.action_time_ms = crate::rules::ACTION_TIME_MS;
    }
    let counters = [ctx.accounts.state_a.hand_counter, ctx.accounts.state_b.hand_counter, ctx.accounts.state_c.hand_counter, ctx.accounts.state_d.hand_counter];
    ctx.accounts.tournament.state.last_hand = counters;
    ctx.accounts.arena.reconciled = counters;
    ctx.accounts.arena.decision_counter = 0;
    ctx.accounts.arena.completed_hand = counters;
    ctx.accounts.arena.completion_order = [0; TABLES];
    ctx.accounts.arena.running = true;
    Ok(())
}

#[derive(Accounts)]
pub struct EnterPokerTournament<'info> {
    pub player: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active,
        constraint = tournament.arena == arena.key() @ RiverproofError::InvalidTournament)]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key())
        @ RiverproofError::InvalidTournament)]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(mut, seeds = [VAULT_SEED, config.key().as_ref()], bump = config.vault_bump,
        token::mint = mint, token::authority = vault, token::token_program = token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = player,
        associated_token::token_program = token_program)]
    pub player_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = config.mint, constraint = mint.key() == arena.mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
}

pub fn enter_poker_tournament(ctx: Context<EnterPokerTournament>, seat_index: u8) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.arena.running && ctx.accounts.tournament.state.phase == Phase::Running,
        RiverproofError::TournamentTableReserved);
    ctx.accounts.arena.require_current_results()?;
    require!(seat_index < 10, RiverproofError::SeatEmpty);
    let table = &mut ctx.accounts.table_state;
    let hand = &ctx.accounts.hand_state;
    require!(!table.seats[seat_index as usize].occupied, RiverproofError::SeatOccupied);
    require!(matches!(hand.stage, HandStage::Idle | HandStage::Complete)
        || hand.active_seats & (1 << seat_index) == 0, RiverproofError::HandInProgress);
    require!(!table.seats.iter().any(|seat| seat.occupied && seat.owner == ctx.accounts.player.key()),
        RiverproofError::TournamentAlreadyRegistered);
    assert_chips_conserved(table, hand)?;
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let index = ctx.accounts.arena.tables.iter().position(|t| *t == ctx.accounts.config.key()).unwrap();
    let amount = ctx.accounts.tournament.state.admit(ctx.accounts.player.key(), index as u8, seat_index, now)
        .map_err(|_| error!(RiverproofError::InvalidTournament))?;
    require!(amount == ctx.accounts.config.buy_in, RiverproofError::InvalidTournament);
    token::transfer_checked(CpiContext::new(ctx.accounts.token_program.key(), TransferChecked {
        from: ctx.accounts.player_tokens.to_account_info(), to: ctx.accounts.vault.to_account_info(),
        mint: ctx.accounts.mint.to_account_info(), authority: ctx.accounts.player.to_account_info(),
    }), base_units(amount)?, DECIMALS)?;
    table.seats[seat_index as usize] = SeatRecord { owner: ctx.accounts.player.key(), stack: amount,
        occupied: true, ready: false, connected: true, leave_after_hand: false,
        sitting_out_since: now.saturating_mul(1000) };
    table.escrowed = table.escrowed.checked_add(amount).ok_or(RiverproofError::InvalidTournament)?;
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    assert_chips_conserved(table, hand)
}

#[derive(Accounts)]
pub struct RecordPokerTournamentHand<'info> {
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active,
        constraint = tournament.arena == arena.key() @ RiverproofError::InvalidTournament)]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key()))]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(seeds = [POT_SEED, config.key().as_ref()], bump = settlement.bump)]
    pub settlement: Box<Account<'info, PokerPotSettlement>>,
}

pub fn record_poker_tournament_hand(ctx: Context<RecordPokerTournamentHand>) -> Result<()> {
    record_native_hand(&mut ctx.accounts.arena, &mut ctx.accounts.tournament, &ctx.accounts.config,
        &mut ctx.accounts.table_state, &ctx.accounts.hand_state, ctx.accounts.settlement.to_account_info())?;
    if ctx.accounts.tournament.state.finish_reason(Clock::get()?.unix_timestamp).is_some() {
        ctx.accounts.arena.running = false;
    }
    Ok(())
}

pub(crate) fn record_native_hand(arena: &mut PokerArena, tournament: &mut PokerTournament,
    config: &Account<TableConfig>, table: &mut TableState, hand: &HandState, settlement: AccountInfo) -> Result<()> {
    require!(tournament.state.phase == Phase::Running && hand.stage == HandStage::Complete, RiverproofError::InvalidTournament);
    assert_chips_conserved(table, hand)?;
    require_paid(config, hand, &[settlement])?;
    let index = arena.tables.iter().position(|t| *t == config.key()).ok_or(RiverproofError::InvalidTournament)?;
    let results: Vec<_> = table.seats.iter().filter(|seat| seat.occupied)
        .map(|seat| StackResult { wallet: seat.owner, stack: seat.stack }).collect();
    // Compare actual native seat positions as well as wallets and token totals.
    for entry in tournament.state.entries.iter().filter(|e| e.status == EntryStatus::Seated && e.table == index as u8) {
        require!(entry.seat < 10 && table.seats[entry.seat as usize].occupied
            && table.seats[entry.seat as usize].owner == entry.wallet, RiverproofError::InvalidTournament);
    }
    let rake = native_fees(hand)?.iter().try_fold(0u64, |sum, fee| sum.checked_add(fee.fee_chips))
        .ok_or(RiverproofError::InvalidPotFee)?;
    require!(arena.completed_hand[index] == hand.hand_id && arena.completion_order[index] > 0,
        RiverproofError::TournamentResultPending);
    tournament.state.record_hand_at(index as u8, hand.hand_id, &results, rake, arena.completion_order[index])
        .map_err(|_| error!(RiverproofError::InvalidTournament))?;
    for seat in &mut table.seats {
        if seat.occupied && seat.stack == 0 { *seat = SeatRecord::default(); }
    }
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    arena.reconciled[index] = hand.hand_id;
    assert_chips_conserved(table, hand)
}
