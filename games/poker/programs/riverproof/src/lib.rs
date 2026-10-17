//! Hash Holdem - on-chain Texas hold'em.
//!
//! On-chain identity, play-money escrow, and the ported poker engine with
//! session keys and permissionless cranks.
//!
//! The rules live in the `riverproof-engine` crate, which has no Solana
//! dependency and is differentially tested against `server/poker-engine.mjs`.
//! This crate is the account layout, authority model, and escrow around it.
//!
//! Cards are dealt by the players themselves - mental poker over ristretto255,
//! in `riverproof-mental` and `crate::mental`. The chain holds a digest of each
//! round and the twenty-five positions a hand opens, and opens them by
//! multiplying published unlock shares together until a card falls out. It is
//! never told what a card is.
//!
//! That scheme needs three players: with two, a board threshold is either one,
//! so either player opens the board early, or two, so either can stall it
//! forever. `create_table` enforces the floor rather than letting a table be
//! created that can never deal.

use anchor_lang::prelude::*;

pub mod bridge;
pub mod errors;
pub mod instructions;
pub mod mental;
pub mod state;
pub mod sdice;
pub mod rules;
pub mod tournament;
pub mod pot_fees;
pub mod pot_settlement;

use instructions::*;

declare_id!("3EB6JJ2k1yPdg9qokViBdSWq4jaEVG4wiKbS8Pw3FmWQ");

#[program]
pub mod riverproof {
    use super::*;

    pub fn activate_poker_tournament(ctx: Context<ActivatePokerTournament>) -> Result<()> {
        instructions::tournament_play::activate_poker_tournament(ctx)
    }
    pub fn enter_poker_tournament(ctx: Context<EnterPokerTournament>, seat_index: u8) -> Result<()> {
        instructions::tournament_play::enter_poker_tournament(ctx, seat_index)
    }
    pub fn record_poker_tournament_hand(ctx: Context<RecordPokerTournamentHand>) -> Result<()> {
        instructions::tournament_play::record_poker_tournament_hand(ctx)
    }
    pub fn exit_poker_tournament(ctx: Context<ExitPokerTournament>, seat_index: u8) -> Result<()> {
        instructions::tournament_seats::exit_poker_tournament(ctx, seat_index)
    }
    pub fn move_poker_tournament_seat(ctx: Context<MovePokerTournamentSeat>, entry_index: u8) -> Result<()> {
        instructions::tournament_seats::move_poker_tournament_seat(ctx, entry_index)
    }
    pub fn finalize_poker_tournament(ctx: Context<FinalizePokerTournament>) -> Result<()> {
        instructions::tournament_finish::finalize_poker_tournament(ctx)
    }
    pub fn refund_poker_tournament_balance(ctx: Context<RefundPokerTournamentBalance>, entry_index: u8) -> Result<()> {
        instructions::tournament_finish::refund_poker_tournament_balance(ctx, entry_index)
    }
    pub fn claim_poker_tournament_prize(ctx: Context<ClaimPokerTournamentPrize>, award_index: u8) -> Result<()> {
        instructions::tournament_finish::claim_poker_tournament_prize(ctx, award_index)
    }
    pub fn restore_poker_cash_tables(ctx: Context<ActivatePokerTournament>) -> Result<()> {
        instructions::tournament_finish::restore_poker_cash_tables(ctx)
    }

    pub fn collect_poker_pot_fee<'info>(ctx: Context<'info, CollectPokerPotFee<'info>>, seat_index: u8) -> Result<()> {
        instructions::pot_settlement::collect_poker_pot_fee(ctx, seat_index)
    }

    pub fn migrate_poker_arena(ctx: Context<MigratePokerArena>) -> Result<()> {
        instructions::tournament_migrate::migrate_poker_arena(ctx)
    }

    pub fn bind_poker_arena(ctx: Context<BindPokerArena>) -> Result<()> {
        instructions::tournament::bind_poker_arena(ctx)
    }
    pub fn schedule_poker_tournament(ctx: Context<SchedulePokerTournament>, starts_at: i64, prize_sdice: u64, buy_in: u64) -> Result<()> {
        instructions::tournament::schedule_poker_tournament(ctx, starts_at, prize_sdice, buy_in)
    }
    pub fn prepare_poker_table(ctx: Context<PreparePokerTable>) -> Result<()> {
        instructions::tournament::prepare_poker_table(ctx)
    }
    pub fn register_poker_tournament(ctx: Context<RegisterPokerTournament>) -> Result<()> {
        instructions::tournament::register_poker_tournament(ctx)
    }
    pub fn refund_poker_seat(ctx: Context<RefundPokerSeat>, seat_index: u8) -> Result<()> {
        instructions::tournament::refund_poker_seat(ctx, seat_index)
    }

    // ---- Phase 1: identity ----

    /// Claims a username and creates the caller's profile.
    pub fn create_profile(
        ctx: Context<CreateProfile>,
        username: String,
        normalized: String,
    ) -> Result<()> {
        instructions::identity::create_profile(ctx, username, normalized)
    }

    /// Releases the current username claim and takes a new one.
    pub fn change_username(
        ctx: Context<ChangeUsername>,
        username: String,
        normalized: String,
    ) -> Result<()> {
        instructions::identity::change_username(ctx, username, normalized)
    }

    // ---- Phase 2: play-money token ----

    /// One-time setup of the play-money mint and its faucet.
    pub fn initialize_faucet(
        ctx: Context<InitializeFaucet>,
        claim_amount: u64,
        cooldown_seconds: i64,
    ) -> Result<()> {
        instructions::identity::initialize_faucet(ctx, claim_amount, cooldown_seconds)
    }

    /// Tops up the caller's chip balance.
    pub fn claim_faucet(ctx: Context<ClaimFaucet>) -> Result<()> {
        instructions::identity::claim_faucet(ctx)
    }

    // ---- Phase 3: tables, escrow, and play ----

    #[allow(clippy::too_many_arguments)]
    pub fn create_table(
        ctx: Context<CreateTable>,
        table_id: u64,
        name: String,
        rpc_url: String,
        endpoint: [u8; 32],
        join_key: Pubkey,
        small_blind: u64,
        ante: u64,
        max_players: u8,
        buy_in: u64,
        action_time_ms: i64,
    ) -> Result<()> {
        instructions::table::create_table(
            ctx,
            table_id,
            name,
            rpc_url,
            endpoint,
            join_key,
            small_blind,
            ante,
            max_players,
            buy_in,
            action_time_ms,
        )
    }

    /// Raises how many tables one RPC endpoint may carry.
    ///
    /// Signed by the faucet authority, because the limit depends on a plan the
    /// program cannot see and a self-declared one would bind nobody.
    pub fn set_endpoint_cap(
        ctx: Context<SetEndpointCap>,
        endpoint: [u8; 32],
        cap: u16,
    ) -> Result<()> {
        instructions::table::set_endpoint_cap(ctx, endpoint, cap)
    }

    /// Buys in, moving chips from the caller's wallet into table escrow.
    pub fn join_table(ctx: Context<SeatContext>, seat_index: u8) -> Result<()> {
        instructions::table::join_table(ctx, seat_index)
    }

    /// Closes a table its creator is finished with.
    ///
    /// Only the creator, only once every seat is empty and nothing is escrowed,
    /// and never mid-hand. Returns the rent for every account the table owned
    /// and gives its place on the endpoint back, so an abandoned table does not
    /// spend one for ever.
    pub fn close_table<'info>(ctx: Context<'info, CloseTable<'info>>) -> Result<()> {
        instructions::table::close_table(ctx)
    }

    /// Refills a busted stack from the caller's wallet.
    pub fn rebuy(ctx: Context<SeatContext>) -> Result<()> {
        instructions::table::rebuy(ctx)
    }

    /// Cashes out and frees the seat, or marks it to leave after the hand.
    pub fn leave_table(ctx: Context<SeatContext>) -> Result<()> {
        instructions::table::leave_table(ctx)
    }

    /// Permissionless crank: frees a seat idle past the grace period.
    ///
    /// Refunds the evicted stack to that player's own token account and leaves
    /// the chair empty for anyone to `join_table` into. Never available against
    /// a seat that is ready or in a live hand.
    pub fn kick_idle(ctx: Context<KickIdle>, seat_index: u8) -> Result<()> {
        instructions::table::kick_idle(ctx, seat_index)
    }

    /// Opts into or out of the next hand.
    pub fn set_ready(ctx: Context<UpdateSeat>, ready: bool) -> Result<()> {
        instructions::table::set_ready(ctx, ready)
    }

    /// Authorizes an ephemeral key to act for this wallet at one table.
    pub fn authorize_session(
        ctx: Context<AuthorizeSession>,
        session_key: Pubkey,
        expiry: i64,
    ) -> Result<()> {
        instructions::table::authorize_session(ctx, session_key, expiry)
    }

    pub fn revoke_session(ctx: Context<RevokeSession>) -> Result<()> {
        instructions::table::revoke_session(ctx)
    }

    // ---- Phase 6: dealt by the players, with the chain as referee ----
    //
    // Six instructions, and between them they never learn a card. Players
    // shuffle and lock off chain and commit a digest of each round; the chain
    // holds the twenty-five positions a hand opens and opens them, one scalar
    // multiplication per published share. See `instructions/mental.rs`.

    /// Starts a hand the players shuffle themselves. Needs three seats.
    pub fn start_hand_mental(ctx: Context<StartMentalHand>) -> Result<()> {
        instructions::mental::start_hand_mental(ctx)
    }

    /// Records that a player published a shuffle or lock round.
    pub fn commit_deal_round(
        ctx: Context<MentalPlay>,
        round: DealRound,
        digest: [u8; 32],
    ) -> Result<()> {
        instructions::mental::commit_deal_round(ctx, round, digest)
    }

    /// Publishes the locked deck at the positions this hand opens.
    pub fn seal_mental_deck(
        ctx: Context<MentalPlay>,
        offset: u8,
        locked: Vec<[u8; 32]>,
    ) -> Result<()> {
        instructions::mental::seal_mental_deck(ctx, offset, locked)
    }

    /// Multiplies this player's unlock shares into the cards they name.
    pub fn publish_unlocks(
        ctx: Context<MentalPlay>,
        slots: Vec<u8>,
        shares: Vec<[u8; 32]>,
        keys: Vec<[u8; 32]>,
        paths: Vec<[u8; 32]>,
        round_digest: [u8; 32],
    ) -> Result<()> {
        instructions::mental::publish_unlocks(ctx, slots, shares, keys, paths, round_digest)
    }

    /// Puts an absent seat's layer on the board, rebuilt from the shares it
    /// dealt when it locked.
    ///
    /// The remedy for the one stall nothing else here can fix: a board card is
    /// locked under every seat, so one that stops answering freezes the hand
    /// even after the betting side has folded it. `board` addresses cards by
    /// their offset within the board, never by slot index, so a hole card
    /// cannot be named here at all.
    pub fn recover_unlocks(
        ctx: Context<MentalPlay>,
        absent: u8,
        board: Vec<u8>,
        shares: Vec<[u8; 32]>,
        keys: Vec<[u8; 32]>,
        paths: Vec<[u8; 32]>,
        round_digest: [u8; 32],
    ) -> Result<()> {
        instructions::mental::recover_unlocks(ctx, absent, board, shares, keys, paths, round_digest)
    }

    /// Permissionless: hands the street the engine is parked on to the engine.
    pub fn reveal_mental_street(ctx: Context<MentalPlay>) -> Result<()> {
        instructions::mental::reveal_mental_street(ctx)
    }

    /// Hands one contender's hole cards to the engine at showdown.
    pub fn reveal_mental_hole(ctx: Context<MentalPlay>, seat: u8) -> Result<()> {
        instructions::mental::reveal_mental_hole(ctx, seat)
    }

    /// Permissionless crank: unsticks a hand whose cards stopped arriving.
    pub fn abandon_mental_deal(ctx: Context<MentalPlay>) -> Result<()> {
        instructions::mental::abandon_mental_deal(ctx)
    }


    /// Applies one betting action, signed by the wallet or a session key.
    pub fn act(
        ctx: Context<Play>,
        action: PokerAction,
        expected_hand_id: u64,
        expected_version: u32,
    ) -> Result<()> {
        instructions::play::act(ctx, action, expected_hand_id, expected_version)
    }

    /// Owner/session-authorized away mode. Never calls a bet or raises.
    pub fn set_auto_check_fold(ctx: Context<Play>, enabled: bool) -> Result<()> {
        instructions::play::set_auto_check_fold(ctx, enabled)
    }

    /// Permissionless crank replacing the server's action-timeout tick.
    pub fn force_timeout(ctx: Context<Play>) -> Result<()> {
        instructions::play::force_timeout(ctx)
    }

    /// Permissionless crank replacing the server's next-hand tick.
    pub fn start_next_hand(ctx: Context<Play>) -> Result<()> {
        instructions::play::start_next_hand(ctx)
    }
}
