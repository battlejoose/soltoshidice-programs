//! Freeze all native results together, then pay bounded, replay-safe claims.
use anchor_lang::prelude::*;
use anchor_spl::{associated_token::AssociatedToken,
    token_interface::{Mint, TokenAccount}, token_2022::{self as token, Token2022, TransferChecked}};
use crate::{bridge, errors::RiverproofError, state::*, sdice::{base_units, valid_mint, DECIMALS},
    tournament::{accounts::*, model::{EntryStatus, Phase, FinishReason, PrizeAward, TABLES, CAPACITY, DRAINED_TABLES}},
    pot_settlement::{require_paid, PokerPotSettlement, POT_SEED}};
use super::{table::{assert_chips_conserved, hand_is_live},
    tournament_play::{ActivatePokerTournament, record_native_hand}};

#[derive(Accounts)]
pub struct FinalizePokerTournament<'info> {
    pub cranker: Signer<'info>,
    #[account(mut, seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active, constraint = tournament.arena == arena.key())]
    pub tournament: Box<Account<'info, PokerTournament>>,
    pub table_a: Box<Account<'info, TableConfig>>,
    pub table_b: Box<Account<'info, TableConfig>>,
    pub table_c: Box<Account<'info, TableConfig>>,
    #[account(mut)]
    pub state_a: Box<Account<'info, TableState>>,
    #[account(mut)]
    pub state_b: Box<Account<'info, TableState>>,
    #[account(mut)]
    pub state_c: Box<Account<'info, TableState>>,
    #[account(mut)]
    pub hand_a: Box<Account<'info, HandState>>,
    #[account(mut)]
    pub hand_b: Box<Account<'info, HandState>>,
    #[account(mut)]
    pub hand_c: Box<Account<'info, HandState>>,
    // Every table/state/hand/card/ledger identity is checked together below.
    // Keeping their seed expressions out of this parser stays below SBF 4 KiB.
    #[account(mut)]
    pub cards_a: Box<Account<'info, HandCards>>,
    #[account(mut)]
    pub cards_b: Box<Account<'info, HandCards>>,
    #[account(mut)]
    pub cards_c: Box<Account<'info, HandCards>>,
    pub pot_a: Box<Account<'info, PokerPotSettlement>>,
    pub pot_b: Box<Account<'info, PokerPotSettlement>>,
    pub pot_c: Box<Account<'info, PokerPotSettlement>>,
    pub table_d: Box<Account<'info, TableConfig>>,
    #[account(mut)]
    pub state_d: Box<Account<'info, TableState>>,
    #[account(mut)]
    pub hand_d: Box<Account<'info, HandState>>,
    #[account(mut)]
    pub cards_d: Box<Account<'info, HandCards>>,
    pub pot_d: Box<Account<'info, PokerPotSettlement>>,
}

pub fn finalize_poker_tournament(ctx: Context<FinalizePokerTournament>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let running = ctx.accounts.tournament.state.phase == Phase::Running;
    require!(running || matches!(ctx.accounts.tournament.state.phase, Phase::Scheduled | Phase::Draining),
        RiverproofError::InvalidTournament);
    if !running {
        // Recovery after a missed entry window is not permission to confiscate
        // old cash seats. The original takeover/refunds must finish first.
        require!(now >= ctx.accounts.tournament.state.entry_closes_at && ctx.accounts.arena.drained == DRAINED_TABLES,
            RiverproofError::TournamentNotDue);
    }
    for (index, (config, table, hand, cards, pot)) in [
        (&ctx.accounts.table_a, &mut ctx.accounts.state_a, &mut ctx.accounts.hand_a, &mut ctx.accounts.cards_a, &ctx.accounts.pot_a),
        (&ctx.accounts.table_b, &mut ctx.accounts.state_b, &mut ctx.accounts.hand_b, &mut ctx.accounts.cards_b, &ctx.accounts.pot_b),
        (&ctx.accounts.table_c, &mut ctx.accounts.state_c, &mut ctx.accounts.hand_c, &mut ctx.accounts.cards_c, &ctx.accounts.pot_c),
        (&ctx.accounts.table_d, &mut ctx.accounts.state_d, &mut ctx.accounts.hand_d, &mut ctx.accounts.cards_d, &ctx.accounts.pot_d),
    ].into_iter().enumerate() {
        let key = config.key();
        require!(key == ctx.accounts.arena.tables[index] && config.join_key == ctx.accounts.arena.key(),
            RiverproofError::InvalidTournament);
        require_keys_eq!(table.key(), Pubkey::find_program_address(&[TABLE_STATE_SEED, key.as_ref()], &crate::ID).0,
            RiverproofError::InvalidTournament);
        require_keys_eq!(hand.key(), Pubkey::find_program_address(&[HAND_SEED, key.as_ref()], &crate::ID).0,
            RiverproofError::InvalidTournament);
        require_keys_eq!(cards.key(), Pubkey::find_program_address(&[HAND_CARDS_SEED, key.as_ref()], &crate::ID).0,
            RiverproofError::InvalidTournament);
        require_keys_eq!(pot.key(), Pubkey::find_program_address(&[POT_SEED, key.as_ref()], &crate::ID).0,
            RiverproofError::InvalidTournament);
        freeze_table(&mut ctx.accounts.arena, &mut ctx.accounts.tournament, config, table, hand, cards, pot.to_account_info(), now, running)?;
    }
    let tournament = &mut ctx.accounts.tournament;
    let reason = if running {
        tournament.state.finish_reason(now).ok_or(RiverproofError::TournamentNotDue)?
    } else { FinishReason::InsufficientEntrants };
    tournament.awards = if reason == FinishReason::InsufficientEntrants {
        vec![PrizeAward { wallet: tournament.state.creator, amount: base_units(tournament.state.prize_sdice)? }]
    } else {
        tournament.state.prize_allocations(now).map_err(|_| error!(RiverproofError::InvalidTournament))?
    };
    tournament.finish_reason = Some(reason);
    tournament.finalized_at = now;
    tournament.refunded = 0;
    tournament.prizes_paid = 0;
    tournament.state.phase = Phase::Finishing;
    ctx.accounts.arena.running = false;
    Ok(())
}

#[inline(never)]
fn freeze_table(arena: &mut PokerArena, tournament: &mut PokerTournament,
    config: &Account<TableConfig>, table: &mut TableState, hand: &mut HandState,
    cards: &mut HandCards, pot: AccountInfo, now: i64, running: bool) -> Result<()> {
    assert_chips_conserved(table, hand)?;
    require_paid(config, hand, &[pot.clone()])?;
    let index = arena.tables.iter().position(|t| *t == config.key()).ok_or(RiverproofError::InvalidTournament)?;
    if running {
        if hand_is_live(hand) {
            require!(now >= tournament.state.ends_at, RiverproofError::HandInProgress);
            // A real completed payout is preserved; only unfinished hands at
            // the fixed cutoff return each contribution to its original seat.
            let mut engine = bridge::load(config, table, Some((hand, cards)));
            engine.void_hand(now.saturating_mul(1000)).map_err(RiverproofError::from)?;
            bridge::store_table(&engine, table, now.saturating_mul(1000));
            bridge::store_hand(&engine, hand, cards);
            hand.settled = true;
            arena.note_hand_completion(index, hand.hand_id)?;
        }
        if hand.stage == HandStage::Complete && hand.hand_id > tournament.state.last_hand[index] {
            record_native_hand(arena, tournament, config, table, hand, pot)?;
        }
        // Check both directions, including seats which did not play this hand.
        // A completed or previously-recorded hand must agree with real custody.
        for (seat_index, seat) in table.seats.iter().enumerate().filter(|(_, seat)| seat.occupied) {
            require!(tournament.state.entries.iter().any(|e| e.status == EntryStatus::Seated
                && e.table == index as u8 && e.seat == seat_index as u8
                && e.wallet == seat.owner && e.stack == seat.stack), RiverproofError::InvalidTournament);
        }
        for entry in tournament.state.entries.iter().filter(|e| e.status == EntryStatus::Seated && e.table == index as u8) {
            require!(table.seats.get(entry.seat as usize).map(|seat| seat.occupied
                && seat.owner == entry.wallet && seat.stack == entry.stack) == Some(true), RiverproofError::InvalidTournament);
        }
    } else {
        require!(table.escrowed == 0 && table.seats.iter().all(|seat| !seat.occupied), RiverproofError::TableNotEmpty);
    }
    for seat in table.seats.iter_mut().filter(|seat| seat.occupied) { seat.set_ready(false, now.saturating_mul(1000)); }
    table.next_hand_at = 0;
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    assert_chips_conserved(table, hand)
}

#[derive(Accounts)]
#[instruction(entry_index: u8)]
pub struct RefundPokerTournamentBalance<'info> {
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(mut, address = arena.active, constraint = tournament.arena == arena.key())]
    pub tournament: Box<Account<'info, PokerTournament>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key()))]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(mut, seeds = [VAULT_SEED, config.key().as_ref()], bump = config.vault_bump,
        token::mint = mint, token::authority = vault, token::token_program = token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: frozen entry owner and its native seat are checked, never a signer.
    #[account(constraint = tournament.state.entries.get(entry_index as usize).map(|e| e.wallet) == Some(owner.key())
        @ RiverproofError::NoSeat)]
    pub owner: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = cranker, associated_token::mint = mint,
        associated_token::authority = owner, associated_token::token_program = token_program)]
    pub owner_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = arena.mint, constraint = config.mint == mint.key())]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn refund_poker_tournament_balance(ctx: Context<RefundPokerTournamentBalance>, entry_index: u8) -> Result<()> {
    require!(ctx.accounts.tournament.state.phase == Phase::Finishing && !ctx.accounts.arena.running
        && (entry_index as usize) < CAPACITY && ctx.accounts.tournament.refunded & (1 << entry_index) == 0, RiverproofError::InvalidTournament);
    let entry = ctx.accounts.tournament.state.entries[entry_index as usize];
    require!(entry.status == EntryStatus::Seated && (entry.table as usize) < TABLES && entry.seat < 10
        && ctx.accounts.arena.tables[entry.table as usize] == ctx.accounts.config.key(), RiverproofError::NoSeat);
    let seat = ctx.accounts.table_state.seats[entry.seat as usize];
    require!(seat.occupied && seat.owner == entry.wallet && seat.stack == entry.stack, RiverproofError::InvalidTournament);
    require!(!hand_is_live(&ctx.accounts.hand_state), RiverproofError::HandInProgress);
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)?;
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let config = ctx.accounts.config.key(); let bump = [ctx.accounts.config.vault_bump];
    let seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config.as_ref(), &bump]];
    token::transfer_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), TransferChecked {
        mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.vault.to_account_info(),
        to: ctx.accounts.owner_tokens.to_account_info(), authority: ctx.accounts.vault.to_account_info(),
    }, seeds), base_units(entry.stack)?, DECIMALS)?;
    ctx.accounts.table_state.seats[entry.seat as usize] = SeatRecord::default();
    ctx.accounts.table_state.escrowed = ctx.accounts.table_state.escrowed.checked_sub(entry.stack).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.table_state.version = ctx.accounts.table_state.version.checked_add(1).ok_or(RiverproofError::InvalidTournament)?;
    ctx.accounts.tournament.refunded |= 1 << entry_index;
    // Keep the rank's stack frozen. The mask prevents a second cash-out.
    ctx.accounts.tournament.state.entries[entry_index as usize].cash_out = entry.stack;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

#[derive(Accounts)]
#[instruction(award_index: u8)]
pub struct ClaimPokerTournamentPrize<'info> {
    #[account(mut)]
    pub cranker: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    /// CHECK: legacy/current exact layout, discriminator, seeds and frozen award
    /// are checked in the handler. Deliberately independent of arena.active.
    #[account(mut, owner = crate::ID)]
    pub tournament: UncheckedAccount<'info>,
    #[account(mut, token::mint = mint, token::authority = prize_vault, token::token_program = token_program)]
    pub prize_vault: Box<InterfaceAccount<'info, TokenAccount>>,
    /// CHECK: only the exact frozen award recipient is allowed.
    pub recipient: UncheckedAccount<'info>,
    #[account(init_if_needed, payer = cranker, associated_token::mint = mint,
        associated_token::authority = recipient, associated_token::token_program = token_program)]
    pub recipient_tokens: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = arena.mint)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    pub token_program: Program<'info, Token2022>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

pub fn claim_poker_tournament_prize(ctx: Context<ClaimPokerTournamentPrize>, award_index: u8) -> Result<()> {
    use crate::tournament::legacy::PrizeClaimAccount;
    let mut account = PrizeClaimAccount::read(&ctx.accounts.tournament.try_borrow_data()?)?;
    let view = account.view();
    let arena = ctx.accounts.arena.key();
    let tournament = ctx.accounts.tournament.key();
    let (expected, bump) = Pubkey::find_program_address(&[TOURNAMENT_SEED, arena.as_ref(), &view.id.to_le_bytes()], &crate::ID);
    let (vault, vault_bump) = Pubkey::find_program_address(&[PRIZE_SEED, tournament.as_ref()], &crate::ID);
    require!(view.arena == arena && expected == tournament && bump == view.bump
        && vault == ctx.accounts.prize_vault.key() && vault_bump == view.vault_bump
        && award_index < 10 && view.prizes_paid & (1 << award_index) == 0,
        RiverproofError::InvalidTournament);
    let award = view.awards.get(award_index as usize).ok_or(RiverproofError::InvalidTournament)?;
    require_keys_eq!(award.wallet, ctx.accounts.recipient.key(), RiverproofError::InvalidTournament);
    let amount = award.amount;
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let bump = [vault_bump];
    let seeds: &[&[&[u8]]] = &[&[PRIZE_SEED, tournament.as_ref(), &bump]];
    token::transfer_checked(CpiContext::new_with_signer(ctx.accounts.token_program.key(), TransferChecked {
        mint: ctx.accounts.mint.to_account_info(), from: ctx.accounts.prize_vault.to_account_info(),
        to: ctx.accounts.recipient_tokens.to_account_info(), authority: ctx.accounts.prize_vault.to_account_info(),
    }, seeds), amount, DECIMALS)?;
    account.mark_paid(award_index, &mut ctx.accounts.tournament.try_borrow_mut_data()?)?;
    Ok(())
}

/// Same pinned four-table account set as activation, but no new deposits.
pub fn restore_poker_cash_tables(ctx: Context<ActivatePokerTournament>) -> Result<()> {
    require!(ctx.accounts.tournament.state.phase == Phase::Finishing && !ctx.accounts.arena.running,
        RiverproofError::InvalidTournament);
    let mask = ctx.accounts.tournament.state.entries.iter().enumerate().filter(|(_, e)| e.status == EntryStatus::Seated)
        .fold(0u64, |mask, (i, _)| mask | (1 << i));
    require!(ctx.accounts.tournament.refunded & mask == mask, RiverproofError::TableNotEmpty);
    for table in [&ctx.accounts.state_a, &ctx.accounts.state_b, &ctx.accounts.state_c, &ctx.accounts.state_d] {
        require!(table.escrowed == 0 && table.seats.iter().all(|s| !s.occupied), RiverproofError::TableNotEmpty);
    }
    for (index, config) in [&mut ctx.accounts.table_a, &mut ctx.accounts.table_b, &mut ctx.accounts.table_c, &mut ctx.accounts.table_d].into_iter().enumerate() {
        let rules = ctx.accounts.arena.cash_rules[index];
        config.buy_in = rules.buy_in; config.small_blind = rules.small_blind;
        config.big_blind = rules.big_blind; config.ante = rules.ante; config.action_time_ms = crate::rules::ACTION_TIME_MS;
    }
    ctx.accounts.tournament.state.phase = if ctx.accounts.tournament.finish_reason == Some(FinishReason::InsufficientEntrants) {
        Phase::Cancelled
    } else { Phase::Complete };
    ctx.accounts.arena.active = Pubkey::default();
    ctx.accounts.arena.starts_at = 0; ctx.accounts.arena.cash_closes_at = 0;
    ctx.accounts.arena.ends_at = 0; ctx.accounts.arena.drained = 0;
    Ok(())
}
