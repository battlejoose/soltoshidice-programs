//! Cash-outs and consolidation use the same real table custody as cash poker.
use anchor_lang::prelude::*;
use anchor_spl::{associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount}, token_2022::{self as token, Token2022, TransferChecked}};
use crate::{errors::RiverproofError, state::*, sdice::{base_units, valid_mint, DECIMALS},
    tournament::{accounts::*, model::{EntryStatus, Phase}},
    pot_settlement::{require_paid, PokerPotSettlement, POT_SEED}};
use super::table::{assert_chips_conserved, hand_is_live, seat_is_in_live_hand, SEAT_TAKEOVER_GRACE_MS};

fn clear_unplayable_idle_timer(table: &mut TableState, hand: &HandState) {
    if hand.stage == HandStage::Idle && table.seats.iter()
        .filter(|s| s.occupied && s.ready && s.connected && s.stack > 0).count()
        < riverproof_engine::table::MENTAL_MIN_SEATS as usize {
        table.next_hand_at = 0;
    }
}

#[derive(Accounts)]
#[instruction(seat_index: u8)]
pub struct ExitPokerTournament<'info> {
    /// The owner may leave; anyone may execute the ordinary idle-removal rule.
    #[account(mut)]
    pub actor: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active, constraint = tournament.arena == arena.key())]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key()))]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(seeds = [POT_SEED, config.key().as_ref()], bump = settlement.bump)]
    pub settlement: Box<Account<'info, PokerPotSettlement>>,
    #[account(mut, seeds = [VAULT_SEED, config.key().as_ref()], bump = config.vault_bump,
        token::mint = mint, token::authority = vault, token::token_program = token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: fixed to the occupied native seat; the caller cannot redirect funds.
    #[account(constraint = table_state.seats.get(seat_index as usize)
        .filter(|seat| seat.occupied).map(|seat| seat.owner) == Some(owner.key()) @ RiverproofError::NoSeat)]
    pub owner: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = actor, associated_token::mint = mint,
        associated_token::authority = owner, associated_token::token_program = token_program)]
    pub owner_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = config.mint, constraint = mint.key() == arena.mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn exit_poker_tournament(ctx: Context<ExitPokerTournament>, seat_index: u8) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.arena.running && ctx.accounts.tournament.state.phase == Phase::Running
        && ctx.accounts.tournament.state.finish_reason(now).is_none(), RiverproofError::TournamentTableReserved);
    let table_index = ctx.accounts.arena.tables.iter().position(|t| *t == ctx.accounts.config.key()).unwrap();
    let seat = ctx.accounts.table_state.seats[seat_index as usize];
    require!(ctx.accounts.tournament.state.entries.iter().any(|e| e.status == EntryStatus::Seated
        && e.wallet == seat.owner && e.table == table_index as u8 && e.seat == seat_index), RiverproofError::InvalidTournament);
    let now_ms = now.saturating_mul(1000);
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)?;

    // Match cash poker: signing LEAVE in a hand requests a later cash-out. It
    // cannot remove committed chips, change the score, or end that hand early.
    if ctx.accounts.actor.key() == seat.owner && seat_is_in_live_hand(&ctx.accounts.hand_state, seat_index) {
        let table = &mut ctx.accounts.table_state;
        table.seats[seat_index as usize].leave_after_hand = true;
        table.seats[seat_index as usize].set_ready(false, now_ms);
        table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
        return Ok(());
    }
    ctx.accounts.arena.require_current_results()?;
    if ctx.accounts.actor.key() != seat.owner {
        require!(!seat.ready, RiverproofError::SeatOccupied);
        require!(!hand_is_live(&ctx.accounts.hand_state), RiverproofError::HandInProgress);
        if !seat.leave_after_hand {
            require!(seat.sitting_out_since != 0
                && now_ms >= seat.sitting_out_since.saturating_add(SEAT_TAKEOVER_GRACE_MS), RiverproofError::TimerNotExpired);
        }
    }
    require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, &[ctx.accounts.settlement.to_account_info()])?;
    if ctx.accounts.hand_state.stage == HandStage::Complete {
        require!(ctx.accounts.arena.reconciled[table_index] >= ctx.accounts.hand_state.hand_id,
            RiverproofError::TournamentResultPending);
    }
    let decision = ctx.accounts.arena.next_decision()?;
    let amount = ctx.accounts.tournament.state.withdraw_at(seat.owner, decision)
        .map_err(|_| error!(RiverproofError::InvalidTournament))?;
    require!(amount == seat.stack, RiverproofError::InvalidTournament);
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let config_key = ctx.accounts.config.key();
    let bump = [ctx.accounts.config.vault_bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config_key.as_ref(), &bump]];
    if amount > 0 {
        token::transfer_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), TransferChecked {
            mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.vault.to_account_info(),
            to: ctx.accounts.owner_tokens.to_account_info(), authority: ctx.accounts.vault.to_account_info(),
        }, seeds), base_units(amount)?, DECIMALS)?;
    }
    let table = &mut ctx.accounts.table_state;
    table.seats[seat_index as usize] = SeatRecord::default();
    table.escrowed = table.escrowed.checked_sub(amount).ok_or(RiverproofError::InvalidTournament)?;
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    clear_unplayable_idle_timer(table, &ctx.accounts.hand_state);
    if ctx.accounts.tournament.state.finish_reason(now).is_some() { ctx.accounts.arena.running = false; }
    assert_chips_conserved(table, &ctx.accounts.hand_state)
}

#[derive(Accounts)]
pub struct MovePokerTournamentSeat<'info> {
    pub cranker: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active, constraint = tournament.arena == arena.key())]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = source.join_key == arena.key() && arena.tables.contains(&source.key()))]
    pub source: Box<Account<'info, TableConfig>>,
    #[account(constraint = destination.join_key == arena.key() && arena.tables.contains(&destination.key())
        && destination.key() != source.key())]
    pub destination: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, source.key().as_ref()], bump = source_state.bump)]
    pub source_state: Box<Account<'info, TableState>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, destination.key().as_ref()], bump = destination_state.bump)]
    pub destination_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, source.key().as_ref()], bump = source_hand.bump)]
    pub source_hand: Box<Account<'info, HandState>>,
    #[account(seeds = [HAND_SEED, destination.key().as_ref()], bump = destination_hand.bump)]
    pub destination_hand: Box<Account<'info, HandState>>,
    #[account(mut, seeds = [VAULT_SEED, source.key().as_ref()], bump = source.vault_bump,
        token::mint = mint, token::authority = source_vault, token::token_program = token_program)]
    pub source_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(mut, seeds = [VAULT_SEED, destination.key().as_ref()], bump = destination.vault_bump,
        token::mint = mint, token::authority = destination_vault, token::token_program = token_program)]
    pub destination_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: handler derives the compulsory PDA from the native movement owner.
    /// A missing session is legal; an existing one is expired, never delegated.
    #[account(mut)]
    pub destination_session: UncheckedAccount<'info>,
    #[account(seeds = [POT_SEED, source.key().as_ref()], bump = source_settlement.bump)]
    pub source_settlement: Box<Account<'info, PokerPotSettlement>>,
    #[account(seeds = [POT_SEED, destination.key().as_ref()], bump = destination_settlement.bump)]
    pub destination_settlement: Box<Account<'info, PokerPotSettlement>>,
    #[account(address = arena.mint, constraint = source.mint == mint.key() && destination.mint == mint.key())]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
}

pub fn move_poker_tournament_seat(ctx: Context<MovePokerTournamentSeat>, entry_index: u8) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(ctx.accounts.arena.running && ctx.accounts.tournament.state.phase == Phase::Running
        && ctx.accounts.tournament.state.finish_reason(now).is_none(), RiverproofError::TournamentTableReserved);
    ctx.accounts.arena.require_current_results()?;
    // Only the first necessary move in the deterministic balance plan is valid.
    // Arbitrary player teleports, stale plans and repeated moves cannot be used
    // to interrupt play or keep extending an idle player's removal deadline.
    let plan = ctx.accounts.tournament.state.rebalance_plan().map_err(|_| error!(RiverproofError::InvalidTournament))?;
    let movement = *plan.first().ok_or(RiverproofError::InvalidTournament)?;
    require!(movement.entry == entry_index
        && ctx.accounts.arena.tables[movement.from_table as usize] == ctx.accounts.source.key()
        && ctx.accounts.arena.tables[movement.to_table as usize] == ctx.accounts.destination.key(), RiverproofError::InvalidTournament);
    // A table with too few ready players may keep its completed hand on screen
    // indefinitely (no next-hand timer). Reconciliation, not that timer, makes
    // its balances safe to move and prevents consolidation from getting stuck.
    for (config, hand, ledger, index) in [
        (&ctx.accounts.source, &ctx.accounts.source_hand, &ctx.accounts.source_settlement, movement.from_table),
        (&ctx.accounts.destination, &ctx.accounts.destination_hand, &ctx.accounts.destination_settlement, movement.to_table),
    ] {
        require!(!hand_is_live(hand), RiverproofError::HandInProgress);
        require_paid(config, hand, &[ledger.to_account_info()])?;
        if hand.stage == HandStage::Complete {
            require!(ctx.accounts.arena.reconciled[index as usize] >= hand.hand_id, RiverproofError::TournamentResultPending);
        }
    }
    assert_chips_conserved(&ctx.accounts.source_state, &ctx.accounts.source_hand)?;
    assert_chips_conserved(&ctx.accounts.destination_state, &ctx.accounts.destination_hand)?;
    let entry = ctx.accounts.tournament.state.entries[entry_index as usize];
    let seat = ctx.accounts.source_state.seats[movement.from_seat as usize];
    require!(seat.occupied && seat.owner == entry.wallet && seat.stack == entry.stack
        && !ctx.accounts.destination_state.seats[movement.to_seat as usize].occupied
        && !ctx.accounts.destination_state.seats.iter().any(|s| s.occupied && s.owner == entry.wallet), RiverproofError::InvalidTournament);
    valid_mint(&ctx.accounts.mint.to_account_info())?;

    // A wallet may have authorized this destination table in an earlier cash
    // session. Require its canonical address even when absent, so a caller
    // cannot omit a live old key and secretly give it control of the moved stack.
    let destination = ctx.accounts.destination.key();
    let session = &ctx.accounts.destination_session;
    let (session_key, session_bump) = Pubkey::find_program_address(&[SESSION_SEED, entry.wallet.as_ref(), destination.as_ref()], &crate::ID);
    require_keys_eq!(session.key(), session_key, RiverproofError::UnauthorizedActor);
    if session.owner == &crate::ID {
        let mut data = session.try_borrow_mut_data()?;
        let mut record = SessionKey::try_deserialize(&mut &data[..])?;
        require!(record.authority == entry.wallet && record.table == destination && record.bump == session_bump,
            RiverproofError::UnauthorizedActor);
        record.expiry = 0; record.session_key = Pubkey::default();
        record.try_serialize(&mut &mut data[..])?;
    } else {
        require!(session.owner == &anchor_lang::system_program::ID && session.data_is_empty(), RiverproofError::UnauthorizedActor);
    }
    let source = ctx.accounts.source.key();
    let bump = [ctx.accounts.source.vault_bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, source.as_ref(), &bump]];
    token::transfer_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), TransferChecked {
        mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.source_vault.to_account_info(),
        to: ctx.accounts.destination_vault.to_account_info(), authority: ctx.accounts.source_vault.to_account_info(),
    }, seeds), base_units(entry.stack)?, DECIMALS)?;
    ctx.accounts.tournament.state.move_seat(movement).map_err(|_| error!(RiverproofError::InvalidTournament))?;
    ctx.accounts.source_state.seats[movement.from_seat as usize] = SeatRecord::default();
    let mut moved = seat;
    // A fresh table needs fresh wallet/session approval. Previously idle seats
    // retain their original grace clock and pending cash-out request.
    moved.set_ready(false, now.saturating_mul(1000));
    ctx.accounts.destination_state.seats[movement.to_seat as usize] = moved;
    ctx.accounts.source_state.escrowed = ctx.accounts.source_state.escrowed.checked_sub(entry.stack).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.destination_state.escrowed = ctx.accounts.destination_state.escrowed.checked_add(entry.stack).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.source_state.version = ctx.accounts.source_state.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.destination_state.version = ctx.accounts.destination_state.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    clear_unplayable_idle_timer(&mut ctx.accounts.source_state, &ctx.accounts.source_hand);
    clear_unplayable_idle_timer(&mut ctx.accounts.destination_state, &ctx.accounts.destination_hand);
    assert_chips_conserved(&ctx.accounts.source_state, &ctx.accounts.source_hand)?;
    assert_chips_conserved(&ctx.accounts.destination_state, &ctx.accounts.destination_hand)
}
