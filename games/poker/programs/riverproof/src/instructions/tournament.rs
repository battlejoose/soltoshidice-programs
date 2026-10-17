//! Funded, immutable schedules and permissionless cash-table takeover.
//! A stalled private deal cannot hold the tournament's clock open.
use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount};
use anchor_spl::token_2022::{self as token, Token2022, TransferChecked};
use crate::{bridge, errors::RiverproofError, sdice::{base_units, valid_mint, DECIMALS}, state::*};
use crate::tournament::{accounts::*, model::{Phase, TournamentState, TABLES}};
use super::table::assert_chips_conserved;
use crate::pot_settlement::{PokerPotSettlement, POT_SEED};

#[derive(Accounts)]
pub struct BindPokerArena<'info> {
    #[account(mut)]
    pub operator: Signer<'info>,
    #[account(init, payer = operator, space = 8 + PokerArena::INIT_SPACE,
        seeds = [ARENA_SEED, operator.key().as_ref()], bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut)]
    pub table_a: Box<Account<'info, TableConfig>>,
    #[account(mut)]
    pub table_b: Box<Account<'info, TableConfig>>,
    #[account(mut)]
    pub table_c: Box<Account<'info, TableConfig>>,
    #[account(seeds = [TABLE_STATE_SEED, table_a.key().as_ref()], bump = state_a.bump,
        constraint = state_a.table == table_a.key() @ RiverproofError::InvalidTournament)]
    pub state_a: Box<Account<'info, TableState>>,
    #[account(seeds = [TABLE_STATE_SEED, table_b.key().as_ref()], bump = state_b.bump,
        constraint = state_b.table == table_b.key() @ RiverproofError::InvalidTournament)]
    pub state_b: Box<Account<'info, TableState>>,
    #[account(seeds = [TABLE_STATE_SEED, table_c.key().as_ref()], bump = state_c.bump,
        constraint = state_c.table == table_c.key() @ RiverproofError::InvalidTournament)]
    pub state_c: Box<Account<'info, TableState>>,
    #[account(init, payer = operator, space = 8 + PokerPotSettlement::INIT_SPACE,
        seeds = [POT_SEED, table_a.key().as_ref()], bump)]
    pub pot_a: Box<Account<'info, PokerPotSettlement>>,
    #[account(init, payer = operator, space = 8 + PokerPotSettlement::INIT_SPACE,
        seeds = [POT_SEED, table_b.key().as_ref()], bump)]
    pub pot_b: Box<Account<'info, PokerPotSettlement>>,
    #[account(init, payer = operator, space = 8 + PokerPotSettlement::INIT_SPACE,
        seeds = [POT_SEED, table_c.key().as_ref()], bump)]
    pub pot_c: Box<Account<'info, PokerPotSettlement>>,
    /// CHECK: Immutable executable destination, explicitly chosen by table operator.
    #[account(executable)]
    pub bank_program: UncheckedAccount<'info>,
    /// CHECK: Canonical bank PDA, authority, mint and version validated below.
    #[account(owner = bank_program.key())]
    pub bank: UncheckedAccount<'info>,
    pub system_program: Program<'info, System>,
    #[account(mut)]
    pub table_d: Box<Account<'info, TableConfig>>,
    #[account(seeds = [TABLE_STATE_SEED, table_d.key().as_ref()], bump = state_d.bump,
        constraint = state_d.table == table_d.key() @ RiverproofError::InvalidTournament)]
    pub state_d: Box<Account<'info, TableState>>,
    #[account(init, payer = operator, space = 8 + PokerPotSettlement::INIT_SPACE,
        seeds = [POT_SEED, table_d.key().as_ref()], bump)]
    pub pot_d: Box<Account<'info, PokerPotSettlement>>,
}

pub fn bind_poker_arena(ctx: Context<BindPokerArena>) -> Result<()> {
    let operator = ctx.accounts.operator.key();
    let tables = [ctx.accounts.table_a.key(), ctx.accounts.table_b.key(), ctx.accounts.table_c.key(), ctx.accounts.table_d.key()];
    require!(tables.iter().enumerate().all(|(i, key)| !tables[..i].contains(key)), RiverproofError::InvalidTournament);
    let mint = ctx.accounts.table_a.mint;
    let (bank, bank_bump) = Pubkey::find_program_address(
        &[b"bank", operator.as_ref(), mint.as_ref()], &ctx.accounts.bank_program.key());
    require_keys_eq!(bank, ctx.accounts.bank.key(), RiverproofError::InvalidTournament);
    {
        let data = ctx.accounts.bank.try_borrow_data()?;
        require!(data.len() == 114 && (3..=4).contains(&data[0])
            && data[1..33] == operator.to_bytes() && data[33..65] == mint.to_bytes()
            && data[113] == bank_bump, RiverproofError::InvalidTournament);
    }
    for config in [&ctx.accounts.table_a, &ctx.accounts.table_b, &ctx.accounts.table_c, &ctx.accounts.table_d] {
        require!(config.creator == operator && config.mint == mint && config.max_players == 10
            && config.join_key == Pubkey::default(), RiverproofError::InvalidTournament);
    }
    for state in [&ctx.accounts.state_a, &ctx.accounts.state_b, &ctx.accounts.state_c, &ctx.accounts.state_d] {
        require!(state.escrowed == 0 && state.seats.iter().all(|s| !s.occupied), RiverproofError::TableNotEmpty);
    }
    let arena_key = ctx.accounts.arena.key();
    let cash_rules = [&ctx.accounts.table_a, &ctx.accounts.table_b, &ctx.accounts.table_c, &ctx.accounts.table_d].map(|table|
        CashRules { buy_in: table.buy_in, small_blind: table.small_blind, big_blind: table.big_blind, ante: table.ante });
    ctx.accounts.table_a.join_key = arena_key;
    ctx.accounts.table_b.join_key = arena_key;
    ctx.accounts.table_c.join_key = arena_key;
    ctx.accounts.table_d.join_key = arena_key;
    ctx.accounts.arena.set_inner(PokerArena {
        operator, mint, tables, bank_program: ctx.accounts.bank_program.key(), bank,
        cash_rules, running: false, ends_at: 0, reconciled: [0; TABLES],
        decision_counter: 0, completed_hand: [0; TABLES], completion_order: [0; TABLES],
        active: Pubkey::default(), next_id: 0,
        starts_at: 0, cash_closes_at: 0, drained: 0, bump: ctx.bumps.arena,
    });
    for (ledger, table, bump) in [
        (&mut ctx.accounts.pot_a, tables[0], ctx.bumps.pot_a),
        (&mut ctx.accounts.pot_b, tables[1], ctx.bumps.pot_b),
        (&mut ctx.accounts.pot_c, tables[2], ctx.bumps.pot_c),
        (&mut ctx.accounts.pot_d, tables[3], ctx.bumps.pot_d),
    ] {
        ledger.set_inner(PokerPotSettlement { table, hand: 0, paid: 0, bump });
    }
    Ok(())
}

#[derive(Accounts)]
pub struct SchedulePokerTournament<'info> {
    #[account(mut)]
    pub sponsor: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(init, payer = sponsor, space = 8 + PokerTournament::INIT_SPACE,
        seeds = [TOURNAMENT_SEED, arena.key().as_ref(), &arena.next_id.to_le_bytes()], bump)]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(init, payer = sponsor, seeds = [PRIZE_SEED, tournament.key().as_ref()], bump,
        token::mint = mint, token::authority = prize_vault, token::token_program = token_program)]
    pub prize_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, associated_token::mint = mint, associated_token::authority = sponsor,
        associated_token::token_program = token_program)]
    pub sponsor_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = arena.mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

pub fn schedule_poker_tournament(ctx: Context<SchedulePokerTournament>, starts_at: i64, prize_sdice: u64, buy_in: u64) -> Result<()> {
    require!(ctx.accounts.arena.active == Pubkey::default(), RiverproofError::TournamentAlreadyScheduled);
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let state = TournamentState::new(ctx.accounts.sponsor.key(), Clock::get()?.unix_timestamp,
        starts_at, prize_sdice, buy_in).map_err(|_| error!(RiverproofError::InvalidTournament))?;
    token::transfer_checked(CpiContext::new(ctx.accounts.token_program.key(), TransferChecked {
        mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.sponsor_tokens.to_account_info(),
        to: ctx.accounts.prize_vault.to_account_info(), authority: ctx.accounts.sponsor.to_account_info(),
    }), base_units(prize_sdice)?, DECIMALS)?;
    let arena = &mut ctx.accounts.arena;
    arena.active = ctx.accounts.tournament.key();
    arena.starts_at = starts_at;
    arena.cash_closes_at = state.cash_closes_at;
    arena.drained = 0;
    arena.running = false;
    arena.ends_at = state.ends_at;
    ctx.accounts.tournament.set_inner(PokerTournament {
        arena: arena.key(), id: arena.next_id, state,
        finish_reason: None, finalized_at: 0, refunded: 0, prizes_paid: 0, awards: Vec::new(),
        bump: ctx.bumps.tournament, vault_bump: ctx.bumps.prize_vault,
    });
    arena.next_id = arena.next_id.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    Ok(())
}

#[derive(Accounts)]
pub struct RegisterPokerTournament<'info> {
    pub player: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active,
        constraint = tournament.arena == arena.key() @ RiverproofError::InvalidTournament)]
    pub tournament: Box<Account<'info, PokerTournament>>,
}

/// Register only the signing wallet in the funded event. Tournament seating
/// separately transfers the real buy-in; no token authorization is requested
/// or implied by registration. The fixed-capacity event already holds the row.
pub fn register_poker_tournament(ctx: Context<RegisterPokerTournament>) -> Result<()> {
    ctx.accounts.tournament.state.register(ctx.accounts.player.key(), Clock::get()?.unix_timestamp)
        .map_err(|error| match error {
            crate::tournament::model::PolicyError::Full => error!(RiverproofError::TournamentFull),
            crate::tournament::model::PolicyError::AlreadyRegistered => error!(RiverproofError::TournamentAlreadyRegistered),
            crate::tournament::model::PolicyError::EntryClosed => error!(RiverproofError::TournamentEntryClosed),
            _ => error!(RiverproofError::InvalidTournament),
        })?;
    Ok(())
}

#[derive(Accounts)]
pub struct PreparePokerTable<'info> {
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active,
        constraint = tournament.arena == arena.key() @ RiverproofError::InvalidTournament)]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key())
        @ RiverproofError::InvalidTournament)]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(mut, seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(mut, seeds = [HAND_CARDS_SEED, config.key().as_ref()], bump = hand_cards.bump)]
    pub hand_cards: Box<Account<'info, HandCards>>,
}

pub fn prepare_poker_table(ctx: Context<PreparePokerTable>) -> Result<()> {
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    let now = Clock::get()?.unix_timestamp;
    require!(now >= ctx.accounts.arena.starts_at, RiverproofError::TournamentNotDue);
    require!(matches!(ctx.accounts.tournament.state.phase, Phase::Scheduled | Phase::Draining), RiverproofError::InvalidTournament);
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)?;
    if !matches!(ctx.accounts.hand_state.stage, HandStage::Idle | HandStage::Complete) {
        let mut table = bridge::load(&ctx.accounts.config, &ctx.accounts.table_state,
            Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)));
        table.void_hand(now.saturating_mul(1_000)).map_err(RiverproofError::from)?;
        bridge::store_table(&table, &mut ctx.accounts.table_state, now.saturating_mul(1_000));
        bridge::store_hand(&table, &mut ctx.accounts.hand_state, &mut ctx.accounts.hand_cards);
        ctx.accounts.hand_state.settled = true;
    }
    for seat in &mut ctx.accounts.table_state.seats {
        if seat.occupied { seat.set_ready(false, now.saturating_mul(1_000)); }
    }
    ctx.accounts.table_state.next_hand_at = 0;
    ctx.accounts.table_state.version = ctx.accounts.table_state.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.tournament.state.phase = Phase::Draining;
    if ctx.accounts.table_state.escrowed == 0 && ctx.accounts.table_state.seats.iter().all(|s| !s.occupied) {
        mark_drained(&mut ctx.accounts.arena, &mut ctx.accounts.tournament, ctx.accounts.config.key())?;
    }
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

#[derive(Accounts)]
#[instruction(seat_index: u8)]
pub struct RefundPokerSeat<'info> {
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
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
    /// CHECK: Only the owner already recorded by the native occupied seat.
    #[account(constraint = (seat_index as usize) < 10
        && table_state.seats[seat_index as usize].occupied
        && table_state.seats[seat_index as usize].owner == evicted.key()
        @ RiverproofError::NoSeat)]
    pub evicted: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = cranker, associated_token::mint = mint,
        associated_token::authority = evicted, associated_token::token_program = token_program)]
    pub evicted_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = config.mint, constraint = mint.key() == arena.mint @ RiverproofError::InvalidTournament)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn refund_poker_seat(ctx: Context<RefundPokerSeat>, seat_index: u8) -> Result<()> {
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    require!(Clock::get()?.unix_timestamp >= ctx.accounts.arena.starts_at, RiverproofError::TournamentNotDue);
    require!(ctx.accounts.tournament.state.phase == Phase::Draining, RiverproofError::InvalidTournament);
    require!(matches!(ctx.accounts.hand_state.stage, HandStage::Idle | HandStage::Complete), RiverproofError::HandInProgress);
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)?;
    let amount = ctx.accounts.table_state.seats[seat_index as usize].stack;
    if amount > 0 {
        valid_mint(&ctx.accounts.mint.to_account_info())?;
        let config = ctx.accounts.config.key();
        let bump = [ctx.accounts.config.vault_bump];
        let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config.as_ref(), &bump]];
        token::transfer_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), TransferChecked {
            mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.vault.to_account_info(),
            to: ctx.accounts.evicted_tokens.to_account_info(), authority: ctx.accounts.vault.to_account_info(),
        }, seeds), base_units(amount)?, DECIMALS)?;
    }
    let table = &mut ctx.accounts.table_state;
    table.seats[seat_index as usize] = SeatRecord::default();
    table.escrowed = table.escrowed.checked_sub(amount).ok_or(RiverproofError::VaultUnderfunded)?;
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    if table.escrowed == 0 && table.seats.iter().all(|s| !s.occupied) {
        mark_drained(&mut ctx.accounts.arena, &mut ctx.accounts.tournament, ctx.accounts.config.key())?;
    }
    assert_chips_conserved(table, &ctx.accounts.hand_state)
}

fn mark_drained(arena: &mut PokerArena, tournament: &mut PokerTournament, table: Pubkey) -> Result<()> {
    let index = arena.tables.iter().position(|key| *key == table).ok_or(RiverproofError::InvalidTournament)?;
    tournament.state.mark_drained(index as u8).map_err(|_| error!(RiverproofError::InvalidTournament))?;
    arena.drained |= 1 << index;
    Ok(())
}
