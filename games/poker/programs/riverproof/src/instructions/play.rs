//! Phase 3 gameplay: betting actions and the permissionless cranks.
//!
//! There is no tick loop on-chain. `tickTable()` ran server-side every 500ms in
//! the off-chain build; here `force_timeout` and `start_next_hand` are cranks
//! anyone may send, validated against the `Clock` sysvar. The relay runs them
//! for convenience, so a dead relay slows a table down but cannot freeze it.

use anchor_lang::prelude::*;
use riverproof_engine::table as engine;

use crate::bridge::{self, NO_SEAT};
use crate::errors::RiverproofError;
use crate::instructions::table::assert_chips_conserved;
use crate::state::{
    HandCards, HandStage, HandState, SessionKey, TableConfig, TableState, WinnerRecord,
    HAND_CARDS_SEED, HAND_SEED, SESSION_SEED, TABLE_STATE_SEED,
};

/// The engine's timers are milliseconds; the chain's clock is seconds.
fn now_ms() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp.saturating_mul(1_000))
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug)]
pub enum PokerAction {
    Fold,
    Check,
    Call,
    AllIn,
    Raise { to: u64 },
}

impl From<PokerAction> for engine::Action {
    fn from(action: PokerAction) -> Self {
        match action {
            PokerAction::Fold => engine::Action::Fold,
            PokerAction::Check => engine::Action::Check,
            PokerAction::Call => engine::Action::Call,
            PokerAction::AllIn => engine::Action::AllIn,
            PokerAction::Raise { to } => engine::Action::Raise { to },
        }
    }
}

#[derive(Accounts)]
pub struct Play<'info> {
    /// Either the seat's wallet or an authorized session key.
    pub signer: Signer<'info>,

    /// Present only when acting through a session key.
    #[account(
        seeds = [SESSION_SEED, session.authority.as_ref(), config.key().as_ref()],
        bump = session.bump,
    )]
    pub session: Option<Account<'info, SessionKey>>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
        constraint = table_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        mut,
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
        constraint = hand_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        seeds = [HAND_CARDS_SEED, config.key().as_ref()],
        bump = hand_cards.bump,
        constraint = hand_cards.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_cards: Box<Account<'info, HandCards>>,
}

impl<'info> Play<'info> {
    /// Resolves which seat this signer may act for.
    ///
    /// This is the additive authority check from the plan:
    /// `seat.owner == signer` becomes
    /// `seat.owner == signer || valid_session_key(signer, seat.owner)`.
    fn authorized_seat(&self, now_seconds: i64) -> Result<u8> {
        let signer = self.signer.key();
        let owner = match self.session.as_ref() {
            Some(session) if session.session_key == signer => {
                require!(
                    session.table == self.config.key(),
                    RiverproofError::SessionTableMismatch
                );
                require!(session.expiry > now_seconds, RiverproofError::SessionExpired);
                session.authority
            }
            _ => signer,
        };

        self.table_state
            .seats
            .iter()
            .position(|seat| seat.occupied && seat.owner == owner)
            .map(|index| index as u8)
            .ok_or_else(|| {
                // A signer that owns no seat, while a session account exists
                // naming a *different* key, is a client still signing with a
                // session key the program has since replaced. The arm above
                // treats such an account as no session at all and falls back to
                // looking the signer up as a wallet — which owns no seat — so
                // this used to surface as `NoSeat`: "you do not have a seat at
                // this table", to a player sitting in a live hand holding
                // cards. Saying which of the two it is costs nothing and is the
                // difference between a player reauthorizing and a player
                // watching their turn time out.
                match self.session.as_ref() {
                    Some(session) if session.session_key != signer => {
                        RiverproofError::SessionKeyReplaced.into()
                    }
                    _ => RiverproofError::NoSeat.into(),
                }
            })
    }
}

/// Applies one betting action.
///
/// `expected_hand_id` and `expected_version` reject an action that raced a state
/// change, the same guard the off-chain protocol enforced with `handId` and
/// `hand.version`.
pub fn act(
    ctx: Context<Play>,
    action: PokerAction,
    expected_hand_id: u64,
    expected_version: u32,
) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let seat_index = ctx.accounts.authorized_seat(clock.unix_timestamp)?;
    let now = clock.unix_timestamp.saturating_mul(1_000);

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );

    table
        .apply_action(
            seat_index,
            action.into(),
            now,
            Some((expected_hand_id, expected_version)),
        )
        .map_err(RiverproofError::from)?;

    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(
        &table,
        &mut ctx.accounts.hand_state,
        &mut ctx.accounts.hand_cards,
    );

    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Owner/session-authorized persistent away mode, with no additional wagering.
/// The existing connected byte preserves account size and old hand decoding.
pub fn set_auto_check_fold(ctx: Context<Play>, enabled: bool) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts,
        crate::tournament::accounts::CashOperation::Continue)?;
    let clock = Clock::get()?;
    let seat = ctx.accounts.authorized_seat(clock.unix_timestamp)?;
    let now = clock.unix_timestamp.saturating_mul(1_000);
    ctx.accounts.table_state.seats[seat as usize].connected = !enabled;
    let mut table = bridge::load(&ctx.accounts.config, &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)));
    table.version += 1;
    if enabled && table.hand.as_ref().is_some_and(|hand|
        hand.stage != riverproof_engine::table::Stage::Complete && hand.actor_seat == Some(seat)) {
        table.force_timeout(now).map_err(RiverproofError::from)?;
    }
    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(&table, &mut ctx.accounts.hand_state, &mut ctx.accounts.hand_cards);
    crate::tournament::accounts::note_native_completion(&ctx.accounts.config,
        &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Permissionless crank: forces a check or fold once the action timer expires.
pub fn force_timeout(ctx: Context<Play>) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let now = now_ms()?;

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );

    let changed = table.force_timeout(now).map_err(RiverproofError::from)?;
    require!(changed, RiverproofError::TimerNotExpired);

    bridge::store_table(&table, &mut ctx.accounts.table_state, now);
    bridge::store_hand(
        &table,
        &mut ctx.accounts.hand_state,
        &mut ctx.accounts.hand_cards,
    );

    crate::tournament::accounts::note_native_completion(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    assert_chips_conserved(&ctx.accounts.table_state, &ctx.accounts.hand_state)
}

/// Permissionless crank: clears a completed hand once its display delay elapses.
///
/// Deliberately separate from `start_hand`, because Phase 4 makes starting a
/// hand an asynchronous Arcium request rather than something that completes
/// inside one instruction.
pub fn start_next_hand(ctx: Context<Play>) -> Result<()> {
    crate::tournament::accounts::require_recorded(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let now = now_ms()?;

    let mut table = bridge::load(
        &ctx.accounts.config,
        &ctx.accounts.table_state,
        Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
    );

    require!(table.start_next_hand(now), RiverproofError::NextHandNotDue);

    bridge::store_table(&table, &mut ctx.accounts.table_state, now);

    let hand_state = &mut ctx.accounts.hand_state;
    hand_state.stage = HandStage::Idle;
    hand_state.actor_seat = NO_SEAT;
    hand_state.settled = false;
    hand_state.winner_count = 0;
    hand_state.winners = [WinnerRecord::default(); riverproof_engine::MAX_SEATS];

    let cards = &mut ctx.accounts.hand_cards;
    cards.revealed = 0;
    cards.confidential = false;

    assert_chips_conserved(&ctx.accounts.table_state, hand_state)
}


