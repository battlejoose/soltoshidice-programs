//! Phase 2 vault escrow and Phase 3 seating, plus session key authorization.
//!
//! Chips live in a real SPL token vault. Buy-in and rebuy move tokens in,
//! cash-out moves them back, and everything between is internal accounting the
//! engine performs. Settlement never mints or burns: `TableState::escrowed`
//! must always equal the sum of seat stacks plus any live pot.

use anchor_lang::prelude::*;
use solana_sha256_hasher::hashv;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount};
use anchor_spl::token_2022::{self as token, Token2022 as Token, TransferChecked};
use crate::sdice::{base_units, valid_mint, DECIMALS};
use riverproof_engine::MAX_SEATS;

use crate::bridge::{self, NO_SEAT};
use crate::errors::RiverproofError;
use crate::state::{
    EndpointQuota, FaucetConfig, HandCards, HandStage, HandState, SeatRecord, SessionKey, TableConfig,
    TableState, DEFAULT_ENDPOINT_CAP, ENDPOINT_SEED, HAND_CARDS_SEED, HAND_SEED, SESSION_SEED,
    FAUCET_SEED, MENTAL_DEAL_SEED, TABLE_SEED, TABLE_STATE_SEED, VAULT_SEED,
};

use crate::rules::valid_action_time;

/// The engine's schedule is in milliseconds; the chain's clock is seconds.
fn now_ms() -> Result<i64> {
    Ok(Clock::get()?.unix_timestamp.saturating_mul(1_000))
}

/// How long a seat must have been sitting out before anyone may take it.
///
/// Five minutes gives players time to renew a session or reconnect before a
/// manual kick. The keeper does not remove players for idleness on its own.
pub const SEAT_TAKEOVER_GRACE_MS: i64 = 300_000;

/// The invariant that replaces `chipTotal(table)` from the off-chain engine.
///
/// Chips may move between stacks and the pot, but the total backed by the vault
/// must never change except through buy-in, rebuy, and cash-out.
pub fn assert_chips_conserved(state: &TableState, hand: &HandState) -> Result<()> {
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    let live_pot: u64 = if matches!(hand.stage, HandStage::Idle | HandStage::Complete) {
        0
    } else {
        hand.contributions.iter().sum()
    };
    require!(
        stacks.saturating_add(live_pot) == state.escrowed,
        RiverproofError::VaultUnderfunded
    );
    Ok(())
}

/// Closes a table its creator is finished with, and gives its slot back.
///
/// Every account the table owns is returned to the creator, who paid their rent
/// — and the endpoint's count comes down by one, which is the point. Without
/// this the quota is a ratchet: a table opened and abandoned spends one of an
/// endpoint's places for ever, and an operator raising a cap would be paying
/// for tables nobody is at.
///
/// Three things have to be true, and all of them are about not destroying
/// something somebody is standing in:
///
/// - Only the creator. The seat-freeing crank is permissionless because it
///   *returns* chips to the person it evicts; this deletes the table those
///   chips sat in, which is not a thing to let a stranger do.
/// - No seats occupied, and nothing escrowed. Chips live in the vault and the
///   vault is closed here — a seated player would have theirs deleted, so the
///   two checks are the same check said twice, and both are worth saying.
/// - Not mid-hand. `Idle` or `Complete` only, so nothing is closed underneath a
///   pot that has not been pushed.
pub fn close_table<'info>(ctx: Context<'info, CloseTable<'info>>) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Close)?;
    // Its creator, or the operator when there is no creator left to ask.
    //
    // A table's creator key can be gone for good — `pnpm agent:table` generates
    // one per run and discards it, and a person can lose a wallet the same way.
    // Nobody can close those, and each one holds a place on its endpoint for
    // ever, so without a second key they accumulate until the endpoint is full
    // of tables nobody is at and nobody can remove.
    //
    // It is real central power and worth naming as such: the operator can end
    // any table on the deployment. What it is not is a way to take anything —
    // the payout below is the same for both, and it pays the seats rather than
    // whoever signed.
    let signer = ctx.accounts.creator.key();
    let by_creator = ctx.accounts.config.creator == signer;
    let by_operator = ctx
        .accounts
        .faucet
        .as_ref()
        .is_some_and(|faucet| faucet.authority == signer);
    require!(by_creator || by_operator, RiverproofError::NotTableCreator);

    let now = now_ms()?;

    // A hand in progress is cancelled rather than refused.
    //
    // `void_hand` is the engine's own way of unwinding a hand nobody can
    // finish: every chip in the pot goes back to the seat that put it there,
    // which is the only defensible thing to do with a pot when the table it
    // was staked on is about to stop existing. Nobody wins it, because nobody
    // finished the hand.
    if !matches!(ctx.accounts.hand_state.stage, HandStage::Idle | HandStage::Complete) {
        let mut table = bridge::load(
            &ctx.accounts.config,
            &ctx.accounts.table_state,
            Some((&ctx.accounts.hand_state, &ctx.accounts.hand_cards)),
        );
        table.void_hand(now).map_err(RiverproofError::from)?;
        bridge::store_table(&table, &mut ctx.accounts.table_state, now);
        bridge::store_hand(
            &table,
            &mut ctx.accounts.hand_state,
            &mut ctx.accounts.hand_cards,
        );
        ctx.accounts.hand_state.settled = true;
    }

    // Everybody's chips, back to their own wallet.
    //
    // The token accounts arrive as `remaining_accounts`, one per occupied seat
    // in seat order, and each is checked against the address derived from that
    // seat's *recorded owner* before a chip moves. That check is the whole of
    // the safety here: without it a creator could name their own account ten
    // times and close the table into their pocket. The seat says where its
    // chips go, not whoever sent this.
    let seats: Vec<(usize, Pubkey, u64)> = ctx
        .accounts
        .table_state
        .seats
        .iter()
        .enumerate()
        .filter(|(_, seat)| seat.occupied)
        .map(|(index, seat)| (index, seat.owner, seat.stack))
        .collect();
    require!(
        ctx.remaining_accounts.len() == seats.len(),
        RiverproofError::TableNotEmpty
    );

    let config_key = ctx.accounts.config.key();
    let bump = [ctx.accounts.config.vault_bump];
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config_key.as_ref(), &bump]];

    for ((_, owner, stack), account) in seats.iter().zip(ctx.remaining_accounts.iter()) {
        let expected = anchor_spl::associated_token::get_associated_token_address_with_program_id(
            owner,
            &ctx.accounts.config.mint,
            &token::ID,
        );
        require!(account.key() == expected, RiverproofError::NotTableCreator);
        if *stack == 0 {
            continue;
        }
        token::transfer_checked(
            CpiContext::new_with_signer(
                ctx.accounts.token_program.key(),
                TransferChecked {
                    mint: ctx.accounts.mint.to_account_info(),
                    from: ctx.accounts.vault.to_account_info(),
                    to: account.clone(),
                    authority: ctx.accounts.vault.to_account_info(),
                },
                signer_seeds,
            ),
            base_units(*stack)?,
            DECIMALS,
        )?;
    }

    // The table now owes nobody anything, which is what lets the vault close.
    let state = &mut ctx.accounts.table_state;
    let paid: u64 = seats.iter().map(|(_, _, stack)| *stack).sum();
    state.escrowed = state.escrowed.saturating_sub(paid);
    for (index, _, _) in seats.iter() {
        state.seats[*index] = SeatRecord::default();
    }
    require!(state.escrowed == 0, RiverproofError::TableNotEmpty);

    // The slot, back where it came from — when there is one to give back.
    //
    // Optional because tables exist that predate the quota entirely: their
    // endpoint has no account, and demanding one would make exactly the oldest
    // tables the ones that can never be removed. A table that was never counted
    // has nothing to uncount.
    if let Some(quota) = ctx.accounts.endpoint_quota.as_ref() {
        // It has to be *this table's* endpoint. The account carries the hash it
        // was seeded from, so a self-referential seeds constraint would let any
        // valid quota account through — and closing a table while pointing at
        // somebody else's endpoint would take a slot off them and leave this
        // one's count high for ever. Repeat that and an endpoint's count can be
        // driven to zero without a single table of theirs being closed.
        let expected = Pubkey::find_program_address(
            &[ENDPOINT_SEED, hashv(&[ctx.accounts.config.rpc_url.as_bytes()]).as_ref()],
            &crate::ID,
        )
        .0;
        require!(quota.key() == expected, RiverproofError::InvalidTableConfig);
    }
    if let Some(quota) = ctx.accounts.endpoint_quota.as_mut() {
        quota.tables = quota.tables.saturating_sub(1);
    }

    // The vault is its own authority, and it has to be emptied of rent by hand:
    // `close` in the accounts below only knows about program accounts, and this
    // is a token account.
    let config_key = ctx.accounts.config.key();
    let bump = [ctx.accounts.config.vault_bump];
    let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config_key.as_ref(), &bump]];
    token::close_account(CpiContext::new_with_signer(
        ctx.accounts.token_program.key(),
        token::CloseAccount {
            account: ctx.accounts.vault.to_account_info(),
            destination: ctx.accounts.creator.to_account_info(),
            authority: ctx.accounts.vault.to_account_info(),
        },
        signer_seeds,
    ))?;

    Ok(())
}

#[derive(Accounts)]
pub struct CloseTable<'info> {
    /// The player who opened it, and nobody else. Rent comes back here.
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(mut, close = creator)]
    pub config: Box<Account<'info, TableConfig>>,

    /// Present only when the operator is closing a table whose creator is gone.
    /// Checked in the handler, because whether it is needed depends on who
    /// signed.
    #[account(seeds = [FAUCET_SEED], bump = faucet.bump)]
    pub faucet: Option<Box<Account<'info, FaucetConfig>>>,

    #[account(
        mut,
        close = creator,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        mut,
        close = creator,
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        close = creator,
        seeds = [HAND_CARDS_SEED, config.key().as_ref()],
        bump = hand_cards.bump,
    )]
    pub hand_cards: Box<Account<'info, HandCards>>,

    /// Only exists once a table has dealt a hand, so a table closed before its
    /// first is closed without one.
    #[account(
        mut,
        close = creator,
        seeds = [MENTAL_DEAL_SEED, config.key().as_ref()],
        bump,
    )]
    pub deal: Option<Box<Account<'info, crate::state::MentalDeal>>>,

    #[account(
        mut,
        seeds = [VAULT_SEED, config.key().as_ref()],
        bump = config.vault_bump,
    )]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,

    /// The endpoint this table was counted against, so closing gives the slot
    /// back rather than spending it for ever.
    /// Absent for a table opened before endpoints were counted at all.
    #[account(mut)]
    pub endpoint_quota: Option<Box<Account<'info, EndpointQuota>>>,

    #[account(constraint = mint.key() == config.mint @ RiverproofError::InvalidTableConfig)]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
}

/// Raises what one endpoint may carry, signed by the faucet authority.
///
/// The cap cannot be derived on chain: it depends on the plan behind an API
/// key, a program has no network, and Helius publishes nothing about a plan
/// over RPC — measured, not assumed. So it is set by somebody who *can* look,
/// and the signature is what makes that somebody the operator rather than
/// whoever is asking. Self-declaring would be worth nothing: the declaration
/// would come from the same client the cap is meant to bind.
///
/// The endpoint is named by hash rather than by URL. The account is seeded from
/// it either way, so the address is the proof it is the right one, and the
/// operator never has to paste somebody's key into a transaction to raise it.
pub fn set_endpoint_cap(ctx: Context<SetEndpointCap>, endpoint: [u8; 32], cap: u16) -> Result<()> {
    require!(
        ctx.accounts.faucet.authority == ctx.accounts.authority.key(),
        RiverproofError::NotFaucetAuthority
    );
    let quota = &mut ctx.accounts.endpoint_quota;
    // First sight of an endpoint is allowed: an operator may raise a cap before
    // anybody has opened a table through it.
    if quota.cap == 0 {
        quota.endpoint = endpoint;
        quota.bump = ctx.bumps.endpoint_quota;
    }
    // Never below what is already open, or existing tables would be over a cap
    // nothing can bring them under.
    require!(cap >= quota.tables, RiverproofError::EndpointAtCapacity);
    quota.cap = cap;
    Ok(())
}

#[derive(Accounts)]
#[instruction(endpoint: [u8; 32])]
pub struct SetEndpointCap<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    #[account(seeds = [FAUCET_SEED], bump = faucet.bump)]
    pub faucet: Box<Account<'info, FaucetConfig>>,

    #[account(
        init_if_needed,
        payer = authority,
        space = 8 + EndpointQuota::INIT_SPACE,
        seeds = [ENDPOINT_SEED, endpoint.as_ref()],
        bump,
    )]
    pub endpoint_quota: Box<Account<'info, EndpointQuota>>,

    pub system_program: Program<'info, System>,
}

#[derive(Accounts)]
#[instruction(table_id: u64, name: String, rpc_url: String, endpoint: [u8; 32])]
pub struct CreateTable<'info> {
    #[account(mut)]
    pub creator: Signer<'info>,

    #[account(
        init,
        payer = creator,
        space = 8 + TableConfig::INIT_SPACE,
        seeds = [TABLE_SEED, creator.key().as_ref(), &table_id.to_le_bytes()],
        bump
    )]
    pub config: Box<Account<'info, TableConfig>>,

    /// How many tables this endpoint is carrying, keyed by its own hash.
    ///
    /// Seeded from the hash the caller names, which the handler checks against
    /// the URL it was given — so naming a different endpoint's account fails
    /// there rather than spending its allowance. Hashing inside the seeds
    /// expression would be tidier and Anchor's IDL build cannot see `rpc_url`
    /// from there. `init_if_needed` because the first table through an endpoint
    /// is what creates its account.
    #[account(
        init_if_needed,
        payer = creator,
        space = 8 + EndpointQuota::INIT_SPACE,
        seeds = [ENDPOINT_SEED, endpoint.as_ref()],
        bump,
    )]
    pub endpoint_quota: Box<Account<'info, EndpointQuota>>,

    #[account(
        init,
        payer = creator,
        space = 8 + TableState::INIT_SPACE,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        init,
        payer = creator,
        space = 8 + HandState::INIT_SPACE,
        seeds = [HAND_SEED, config.key().as_ref()],
        bump
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        init,
        payer = creator,
        space = 8 + HandCards::INIT_SPACE,
        seeds = [HAND_CARDS_SEED, config.key().as_ref()],
        bump
    )]
    pub hand_cards: Box<Account<'info, HandCards>>,

    /// Escrow for every chip at this table. The vault is its own authority, so
    /// only this program can move chips out.
    #[account(
        init,
        payer = creator,
        seeds = [VAULT_SEED, config.key().as_ref()],
        bump,
        token::mint = mint,
        token::authority = vault,
        token::token_program = token_program,
    )]
    pub vault: InterfaceAccount<'info, TokenAccount>,

    pub mint: InterfaceAccount<'info, Mint>,
    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

#[allow(clippy::too_many_arguments)]
#[allow(clippy::too_many_arguments)]
/// Whether an endpoint is one tables may be played through.
///
/// Helius, by host — matched on the host itself rather than anywhere in the
/// string, because `https://evil.example/?x=helius-rpc.com` contains the name
/// and is not Helius.
///
/// Loopback is allowed too, and deliberately: every headless run in this repo
/// creates tables against `http://127.0.0.1:8899`, and a rule that only the
/// deployment needs would otherwise refuse the integration tests, `pnpm
/// play:hand` and `pnpm agent:table`. The browser is stricter still — it also
/// requires `https`, because a page served over TLS cannot fetch an insecure
/// endpoint — and that rule lives with the browser, in `commands.ts`.
fn endpoint_allowed(url: &str) -> bool {
    let host = endpoint_host(url);
    // Public, key-free gateway for the operator-hosted mainnet courtyard.
    // Match the whole URL so credentials, lookalike hosts and other paths fail.
    url == "https://hooddice-holdem-mainnet.hooddice.workers.dev/rpc"
        || host == "helius-rpc.com" || host.ends_with(".helius-rpc.com") || endpoint_is_local(url)
}

/// The host out of a URL, without a port, userinfo, path or query.
fn endpoint_host(url: &str) -> &str {
    let rest = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => return "",
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split('@').next_back().unwrap_or("");
    host.split(':').next().unwrap_or("")
}

/// Somebody's own machine, which is nobody else's to run out of.
///
/// The cap exists because one hosted endpoint carrying too many tables is slow
/// for everybody at all of them. A loopback address is a developer's own
/// validator, and counting it means the first `pnpm play:hand` takes the single
/// table a default cap allows and every run after it cannot create one at all —
/// the whole local suite refusing to run twice.
fn endpoint_is_local(url: &str) -> bool {
    let host = endpoint_host(url);
    host == "localhost" || host == "127.0.0.1" || host == "::1"
}

pub fn create_table(
    ctx: Context<CreateTable>,
    table_id: u64,
    name: String,
    rpc_url: String,
    // SHA-256 of `rpc_url`, which seeds the quota account. Checked below.
    endpoint: [u8; 32],
    join_key: Pubkey,
    small_blind: u64,
    ante: u64,
    max_players: u8,
    buy_in: u64,
    action_time_ms: i64,
) -> Result<()> {
    require!(name.len() <= 32 && !name.is_empty(), RiverproofError::InvalidTableConfig);
    // The endpoint the table is played through, brought by whoever opens it.
    //
    // Length only. Whether it answers, which cluster it answers for, and
    // whether its key will serve the origin asking are all questions a program
    // cannot ask — it has no network — so the client tests the endpoint before
    // it sends this and the chain records what it was told.
    //
    // Not a scheme check either, though the browser needs `https`: a page
    // served over TLS cannot fetch an insecure endpoint, so `commands.ts`
    // refuses anything else and `rpc-check.ts` proves it works before the
    // transaction is built. Requiring it *here* would make the program refuse
    // `http://127.0.0.1:8899`, which is the endpoint every local validator run
    // uses — the integration tests, `pnpm play:hand`, and `pnpm agent:table`
    // all create tables against it. A rule that only the browser needs belongs
    // where the browser is.
    require!(
        !rpc_url.is_empty() && rpc_url.len() <= 200,
        RiverproofError::InvalidTableConfig
    );
    require!(endpoint_allowed(&rpc_url), RiverproofError::EndpointNotAllowed);

    // What this endpoint is already carrying, and what it is allowed to.
    //
    // The account is a PDA of the endpoint's own hash, so a creator cannot
    // spend one endpoint's allowance by naming another's account — the seeds
    // constraint below does not let them. Fresh accounts start at
    // `DEFAULT_ENDPOINT_CAP`; raising one is `set_endpoint_cap`, which the
    // faucet authority signs after looking at a plan the program cannot see.
    // The named account has to be this endpoint's. Its address is derived from
    // `endpoint`, so checking the hash against the URL is what ties the two
    // together — without it a creator could spend a different endpoint's
    // allowance by passing its hash.
    require!(
        hashv(&[rpc_url.as_bytes()]).to_bytes() == endpoint,
        RiverproofError::InvalidTableConfig
    );

    // Counted, unless it is somebody's own machine.
    if !endpoint_is_local(&rpc_url) {
        let quota = &mut ctx.accounts.endpoint_quota;
        if quota.cap == 0 {
            quota.endpoint = endpoint;
            quota.cap = DEFAULT_ENDPOINT_CAP;
            quota.bump = ctx.bumps.endpoint_quota;
        }
        quota.tables = quota.tables.saturating_add(1);
        require!(quota.tables <= quota.cap, RiverproofError::EndpointAtCapacity);
    }
    require!(small_blind >= 1, RiverproofError::InvalidTableConfig);
    // Three, not two, and the floor is the dealing scheme's rather than a
    // preference. A player-shuffled hand opens board cards on a threshold of
    // the players; with two, that threshold is either 1 — so either player
    // opens the board early — or 2, so either can stall it forever. The engine
    // refuses such a hand at `start_hand`, so a two-seat table would be one
    // that could be created, joined, and never dealt. Refusing it here is the
    // difference between a rule and a trap.
    require!(
        (riverproof_engine::table::MENTAL_MIN_SEATS as u8..=MAX_SEATS as u8).contains(&max_players),
        RiverproofError::InvalidTableConfig
    );
    require!(ante <= small_blind, RiverproofError::InvalidTableConfig);
    require!(valid_action_time(action_time_ms), RiverproofError::InvalidTableConfig);
    // Matches `createTable`'s floor of forty small blinds.
    require!(buy_in >= small_blind * 40, RiverproofError::InvalidTableConfig);

    valid_mint(&ctx.accounts.mint.to_account_info())?;
    base_units(buy_in)?;
    let config = &mut ctx.accounts.config;
    config.creator = ctx.accounts.creator.key();
    config.mint = ctx.accounts.mint.key();
    config.name = name;
    config.rpc_url = rpc_url;
    // All zeroes for a table anyone may sit at, which is most of them.
    config.join_key = join_key;
    config.table_id = table_id;
    config.small_blind = small_blind;
    config.big_blind = small_blind * 2;
    config.ante = ante;
    config.buy_in = buy_in;
    config.max_players = max_players;
    config.action_time_ms = crate::rules::ACTION_TIME_MS;
    config.bump = ctx.bumps.config;
    config.vault_bump = ctx.bumps.vault;

    let state = &mut ctx.accounts.table_state;
    state.table = config.key();
    state.seats = [SeatRecord::default(); MAX_SEATS];
    state.dealer_seat = NO_SEAT;
    state.hand_counter = 0;
    state.next_hand_at = 0;
    state.version = 0;
    state.escrowed = 0;
    state.bump = ctx.bumps.table_state;

    let hand = &mut ctx.accounts.hand_state;
    hand.table = config.key();
    hand.stage = HandStage::Idle;
    hand.actor_seat = NO_SEAT;
    hand.bump = ctx.bumps.hand_state;

    let cards = &mut ctx.accounts.hand_cards;
    cards.table = config.key();
    cards.bump = ctx.bumps.hand_cards;

    Ok(())
}

#[derive(Accounts)]
pub struct SeatContext<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
        constraint = table_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
        constraint = hand_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        seeds = [VAULT_SEED, config.key().as_ref()],
        bump = config.vault_bump,
    )]
    pub vault: InterfaceAccount<'info, TokenAccount>,

    #[account(
        init_if_needed,
        payer = wallet,
        associated_token::mint = mint,
        associated_token::authority = wallet,
        associated_token::token_program = token_program,
    )]
    pub player_tokens: InterfaceAccount<'info, TokenAccount>,

    #[account(constraint = mint.key() == config.mint @ RiverproofError::InvalidTableConfig)]
    pub mint: InterfaceAccount<'info, Mint>,

    /// The key a private table's password derives, signing to prove it.
    ///
    /// Absent for an open table, which is most of them. It signs and nothing
    /// else: never an authority over chips, never stored, and the seat still
    /// belongs to `wallet` — so knowing a table's password lets somebody sit
    /// down and gets them nothing else.
    pub gate: Option<Signer<'info>>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

impl<'info> SeatContext<'info> {
    fn deposit(&self, amount: u64) -> Result<()> {
        valid_mint(&self.mint.to_account_info())?;
        token::transfer_checked(
            CpiContext::new(
                self.token_program.key(),
                TransferChecked {
                    mint: self.mint.to_account_info(),
                    from: self.player_tokens.to_account_info(),
                    to: self.vault.to_account_info(),
                    authority: self.wallet.to_account_info(),
                },
            ),
            base_units(amount)?,
            DECIMALS,
        )
    }

    fn withdraw(&self, amount: u64) -> Result<()> {
        let config_key = self.config.key();
        let bump = [self.config.vault_bump];
        let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config_key.as_ref(), &bump]];

        token::transfer_checked(
            CpiContext::new_with_signer(
                self.token_program.key(),
                TransferChecked {
                    mint: self.mint.to_account_info(),
                    from: self.vault.to_account_info(),
                    to: self.player_tokens.to_account_info(),
                    authority: self.vault.to_account_info(),
                },
                signer_seeds,
            ),
            base_units(amount)?,
            DECIMALS,
        )
    }

    fn seat_of(&self, wallet: &Pubkey) -> Option<usize> {
        self.table_state
            .seats
            .iter()
            .position(|seat| seat.occupied && seat.owner == *wallet)
    }
}

pub fn join_table(ctx: Context<SeatContext>, seat_index: u8) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Enter)?;
    let wallet = ctx.accounts.wallet.key();
    let max_players = ctx.accounts.config.max_players;
    let buy_in = ctx.accounts.config.buy_in;

    require!(seat_index < max_players, RiverproofError::SeatEmpty);
    // A private table is opened by signing with the key its password derives.
    //
    // Checked here rather than in the account context so an open table — where
    // `join_key` is all zeroes — needs no gate account at all, and so the
    // refusal names the reason. The password itself is never sent: this is a
    // signature over the join transaction, which nobody else will send again,
    // so it cannot be lifted from the chain and replayed.
    //
    // Only `join_table` asks. `rebuy` and `leave_table` share this context and
    // deliberately do not — a player already holding the seat has answered it,
    // and making them answer again to top up or cash out would mean a lost
    // password could strand their chips at the table.
    let join_key = ctx.accounts.config.join_key;
    if join_key != Pubkey::default() && !crate::tournament::accounts::is_managed(&ctx.accounts.config) {
        let gate = ctx.accounts.gate.as_ref().ok_or(RiverproofError::TableLocked)?;
        require!(gate.key() == join_key, RiverproofError::TableLocked);
    }
    require!(ctx.accounts.seat_of(&wallet).is_none(), RiverproofError::AlreadySeated);
    require!(
        !ctx.accounts.table_state.seats[seat_index as usize].occupied,
        RiverproofError::SeatOccupied
    );

    ctx.accounts.deposit(buy_in)?;

    let now = now_ms()?;
    let state = &mut ctx.accounts.table_state;
    state.seats[seat_index as usize] = SeatRecord {
        owner: wallet,
        stack: buy_in,
        occupied: true,
        ready: false,
        connected: true,
        leave_after_hand: false,
        // A fresh seat is sitting out until its player says otherwise, and the
        // clock starts now — so somebody who buys in and walks away is
        // replaceable on the same terms as anybody else.
        sitting_out_since: now,
    };
    state.escrowed = state.escrowed.saturating_add(buy_in);
    state.version += 1;

    assert_chips_conserved(state, &ctx.accounts.hand_state)
}

pub fn rebuy(ctx: Context<SeatContext>) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Enter)?;
    let wallet = ctx.accounts.wallet.key();
    let buy_in = ctx.accounts.config.buy_in;
    let seat_index = ctx.accounts.seat_of(&wallet).ok_or(RiverproofError::NoSeat)?;

    require!(
        ctx.accounts.table_state.seats[seat_index].stack == 0,
        RiverproofError::RebuyNotAvailable
    );
    // Busted players are never in the live hand, so this cannot change chips
    // mid-hand.
    require!(
        !seat_is_in_live_hand(&ctx.accounts.hand_state, seat_index as u8),
        RiverproofError::HandInProgress
    );

    ctx.accounts.deposit(buy_in)?;

    let now = now_ms()?;
    let state = &mut ctx.accounts.table_state;
    state.seats[seat_index].stack = buy_in;
    state.seats[seat_index].set_ready(false, now);
    // And the takeover clock starts again from the rebuy.
    //
    // `set_ready` only stamps a seat that was ready or had no stamp, so a
    // busted seat — already un-ready, already stamped when it busted — kept the
    // old one. A player who busts, steps away, comes back and buys in again had
    // therefore just deposited a fresh stack into a chair that any cranker
    // could take from them that same instant, until they clicked DEAL ME IN.
    state.seats[seat_index].sitting_out_since = now;
    state.escrowed = state.escrowed.saturating_add(buy_in);
    state.version += 1;

    assert_chips_conserved(state, &ctx.accounts.hand_state)
}

/// Cashes the whole stack out of escrow and frees the seat.
///
/// A player in a live hand is marked to leave once it finishes rather than
/// pulling chips out of a contested pot.
pub fn leave_table(ctx: Context<SeatContext>) -> Result<()> {
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Exit)?;
    let wallet = ctx.accounts.wallet.key();
    let seat_index = ctx.accounts.seat_of(&wallet).ok_or(RiverproofError::NoSeat)?;

    if seat_is_in_live_hand(&ctx.accounts.hand_state, seat_index as u8) {
        let now = now_ms()?;
        let state = &mut ctx.accounts.table_state;
        state.seats[seat_index].leave_after_hand = true;
        state.seats[seat_index].set_ready(false, now);
        state.version += 1;
        return Ok(());
    }

    let stack = ctx.accounts.table_state.seats[seat_index].stack;
    if stack > 0 {
        ctx.accounts.withdraw(stack)?;
    }

    let state = &mut ctx.accounts.table_state;
    state.seats[seat_index] = SeatRecord::default();
    state.escrowed = state.escrowed.saturating_sub(stack);
    state.version += 1;

    assert_chips_conserved(state, &ctx.accounts.hand_state)
}

/// Frees a seat whose player has been sitting out too long.
///
/// Permissionless and clock-gated, like `force_timeout` and `start_next_hand`,
/// and for the same reason: the protection is the deadline, not the identity of
/// whoever notices it has passed. Anybody may send this — including, and mostly,
/// the players still sitting at the table.
///
/// That "mostly" is the whole design. The three-player floor makes an idle seat
/// expensive: a table of three with one player away deals nothing at all. The
/// people who care are the two still there, so the lever has to be theirs. An
/// earlier version of this took the seat as well as freeing it, which meant only
/// an *unseated* stranger could use it — the table froze and the only people who
/// minded were locked out of the cure.
///
/// The evicted player is made whole in the same transaction: their whole stack
/// goes back to their own associated token account, derived from the seat's
/// recorded owner, so a caller cannot name an account of their own and be paid
/// for the eviction. What a griefer gets for their trouble is somebody else
/// cashed out at full value and an empty chair anyone can buy into.
#[derive(Accounts)]
#[instruction(seat_index: u8)]
pub struct KickIdle<'info> {
    /// Permissionless. Pays rent if the evicted player has no token account yet.
    #[account(mut)]
    pub cranker: Signer<'info>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
        constraint = table_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
        constraint = hand_state.table == config.key() @ RiverproofError::InvalidTableConfig
    )]
    pub hand_state: Box<Account<'info, HandState>>,

    #[account(
        mut,
        seeds = [VAULT_SEED, config.key().as_ref()],
        bump = config.vault_bump,
    )]
    pub vault: InterfaceAccount<'info, TokenAccount>,

    /// The wallet being removed. Checked against the seat record, and never a
    /// signer — that is the whole point of the instruction.
    ///
    /// CHECK: only ever used as the authority the refund account is derived
    /// for, and constrained to equal the seat's recorded owner.
    // `get` rather than an index: Anchor evaluates account constraints before the
    // handler runs, so the handler's own bounds check never got the chance and a
    // `seat_index` past the end of the array panicked the program instead of
    // refusing the instruction.
    #[account(
        constraint = table_state
            .seats
            .get(seat_index as usize)
            .map(|seat| seat.owner) == Some(evicted.key())
            @ RiverproofError::NoSeat
    )]
    pub evicted: UncheckedAccount<'info>,

    /// Where the evicted stack goes. Derived from `evicted`, so the caller
    /// cannot be paid for the eviction.
    #[account(
        init_if_needed,
        payer = cranker,
        associated_token::mint = mint,
        associated_token::authority = evicted,
        associated_token::token_program = token_program,
    )]
    pub evicted_tokens: InterfaceAccount<'info, TokenAccount>,

    #[account(constraint = mint.key() == config.mint @ RiverproofError::InvalidTableConfig)]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

impl<'info> KickIdle<'info> {
    fn refund_evicted(&self, amount: u64) -> Result<()> {
        let config_key = self.config.key();
        let bump = [self.config.vault_bump];
        let signer_seeds: &[&[&[u8]]] = &[&[VAULT_SEED, config_key.as_ref(), &bump]];

        token::transfer_checked(
            CpiContext::new_with_signer(
                self.token_program.key(),
                TransferChecked {
                    mint: self.mint.to_account_info(),
                    from: self.vault.to_account_info(),
                    to: self.evicted_tokens.to_account_info(),
                    authority: self.vault.to_account_info(),
                },
                signer_seeds,
            ),
            base_units(amount)?,
            DECIMALS,
        )
    }

}

pub fn kick_idle(ctx: Context<KickIdle>, seat_index: u8) -> Result<()> {
    crate::pot_settlement::require_paid(&ctx.accounts.config, &ctx.accounts.hand_state, ctx.remaining_accounts)?;
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Exit)?;
    let now = now_ms()?;

    require!(seat_index < ctx.accounts.config.max_players, RiverproofError::SeatEmpty);

    let seat = ctx.accounts.table_state.seats[seat_index as usize];
    require!(seat.occupied, RiverproofError::SeatEmpty);
    // A player who is ready keeps their seat, whatever else is true of them.
    require!(!seat.ready, RiverproofError::SeatOccupied);
    // And nobody is evicted while the table is playing a hand.
    //
    // This used to ask only whether *this seat* was in the live hand, which
    // left every other occupied chair unguarded for as long as the hand ran: a
    // player who joined after the deal started, or who sat out a hand, is not
    // in `active_seats` and so had nothing standing between them and a
    // permissionless crank. Their seat could be freed and their stack refunded
    // out from under them while the table played on, and the first they knew of
    // it was `NoSeat` on their next action — the same error a player who never
    // sat down gets, because the seat is checked before anything else.
    //
    // Table-wide rather than per-seat, because freeing a chair is never urgent.
    // The seat this is for has already been idle for five minutes; it can wait
    // out the hand in progress. A hand that never ends is `abandon_hand`'s
    // problem, and that remedy still exists.
    require!(
        !hand_is_live(&ctx.accounts.hand_state),
        RiverproofError::HandInProgress
    );
    // The idle grace is for a seat that might come back. One whose owner has
    // already signed `leave_table` is not that: they asked to go, the hand they
    // were in has since ended, and this instruction is now the only thing that
    // can pay them — `start_next_hand` is permissionless and carries no vault.
    // Making them sit out five minutes first would be a waiting period imposed
    // on somebody who has left, on a chair nobody else can use.
    //
    // Still permissionless, and still not a way to take a seat from anyone: the
    // waiver rides on the seat's own recorded request, and `refund_evicted`
    // derives the destination from the seat's owner rather than from whoever
    // sent this, so freeing it pays the leaver and nobody else.
    if !seat.leave_after_hand {
        require!(
            seat.sitting_out_since != 0
                && now >= seat.sitting_out_since.saturating_add(SEAT_TAKEOVER_GRACE_MS),
            RiverproofError::TimerNotExpired
        );
    }

    // Their chips leave before the seat does, so the vault is never asked to
    // back a stack that no seat claims and `assert_chips_conserved` below is
    // checking a table that really balances rather than one that will.
    if seat.stack > 0 {
        ctx.accounts.refund_evicted(seat.stack)?;
    }

    let state = &mut ctx.accounts.table_state;
    state.escrowed = state.escrowed.saturating_sub(seat.stack);
    state.seats[seat_index as usize] = SeatRecord::default();
    state.version += 1;

    assert_chips_conserved(state, &ctx.accounts.hand_state)
}

pub(crate) fn seat_is_in_live_hand(hand: &HandState, seat_index: u8) -> bool {
    hand_is_live(hand) && (hand.active_seats & (1u16 << seat_index) != 0
        || hand.contributions[seat_index as usize] > 0)
}

/// Whether the table is playing a hand at all, whoever is in it.
pub(crate) fn hand_is_live(hand: &HandState) -> bool {
    !matches!(hand.stage, HandStage::Idle | HandStage::Complete)
}

#[derive(Accounts)]
pub struct UpdateSeat<'info> {
    pub wallet: Signer<'info>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        seeds = [TABLE_STATE_SEED, config.key().as_ref()],
        bump = table_state.bump,
    )]
    pub table_state: Box<Account<'info, TableState>>,

    #[account(
        seeds = [HAND_SEED, config.key().as_ref()],
        bump = hand_state.bump,
    )]
    pub hand_state: Box<Account<'info, HandState>>,
}

/// Opts a seat into or out of the next hand. Mirrors `setReady`.
pub fn set_ready(ctx: Context<UpdateSeat>, ready: bool) -> Result<()> {
    crate::tournament::accounts::guard_cash(&ctx.accounts.config, ctx.remaining_accounts, crate::tournament::accounts::CashOperation::Continue)?;
    let wallet = ctx.accounts.wallet.key();
    let seat_index = ctx
        .accounts
        .table_state
        .seats
        .iter()
        .position(|seat| seat.occupied && seat.owner == wallet)
        .ok_or(RiverproofError::NoSeat)?;

    require!(
        ctx.accounts.table_state.seats[seat_index].stack > 0,
        RiverproofError::RebuyNotAvailable
    );
    require!(
        !seat_is_in_live_hand(&ctx.accounts.hand_state, seat_index as u8),
        RiverproofError::HandInProgress
    );

    let now_ms = now_ms()?;
    let state = &mut ctx.accounts.table_state;
    let changed = state.seats[seat_index].ready != ready;
    state.seats[seat_index].set_ready(ready, now_ms);
    state.seats[seat_index].connected = true;
    // Sitting back in cancels a pending departure. Without this a player who
    // clicked LEAVE mid-hand and then changed their mind stayed flagged for
    // ever: nothing else clears the flag, so they would be dealt in and then
    // silently un-readied again at the end of every hand, and `kick_idle`
    // could take their chair out from under them with no grace at all.
    if ready {
        state.seats[seat_index].leave_after_hand = false;
    }

    let ready_count = state
        .seats
        .iter()
        .filter(|seat| seat.occupied && seat.ready && seat.stack > 0 && seat.connected)
        .count();
    // Three, because three is what this program can actually deal.
    //
    // `start_hand_mental` is the only way a hand begins here and it refuses
    // fewer than `MENTAL_MIN_SEATS`, so arming the clock at two told every
    // client a hand was due and then failed each one of them with
    // `NotEnoughPlayersForMentalDeal`. Worse, the refusal aborts the
    // transaction, so the engine's own clearing of `next_hand_at` is rolled
    // back with it and the table sits pointing at a moment in the past with a
    // countdown stuck at zero.
    //
    // Only when it changes. `start_hand_mental` refuses while `now` is short of
    // the scheduled time, so re-stamping it on every call — including a call
    // that changed nothing — let any seated player push the start of the next
    // hand permanently into the future for the cost of one transaction every
    // couple of seconds.
    let due = ready_count as u32 >= riverproof_engine::table::MENTAL_MIN_SEATS;
    if changed || (due != (state.next_hand_at != 0)) {
        state.next_hand_at = if due {
            now_ms + riverproof_engine::table::NEXT_HAND_DELAY_MS
        } else {
            0
        };
    }
    state.version += 1;

    Ok(())
}

#[derive(Accounts)]
#[instruction(session_key: Pubkey)]
pub struct AuthorizeSession<'info> {
    /// The real wallet. A session key may never authorize another session key.
    #[account(mut)]
    pub wallet: Signer<'info>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        init_if_needed,
        payer = wallet,
        space = 8 + SessionKey::INIT_SPACE,
        seeds = [SESSION_SEED, wallet.key().as_ref(), config.key().as_ref()],
        bump
    )]
    pub session: Account<'info, SessionKey>,

    pub system_program: Program<'info, System>,
}

/// Authorizes an ephemeral keypair to act for this wallet at one table.
///
/// Scoped deliberately: a leaked session key can fold one player's hands at one
/// table until expiry, and can never move chips in or out of escrow.
pub fn authorize_session(
    ctx: Context<AuthorizeSession>,
    session_key: Pubkey,
    expiry: i64,
) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    require!(expiry > now, RiverproofError::SessionExpired);
    require!(
        session_key != ctx.accounts.wallet.key(),
        RiverproofError::SessionMayNotDelegate
    );

    let session = &mut ctx.accounts.session;
    session.authority = ctx.accounts.wallet.key();
    session.session_key = session_key;
    session.table = ctx.accounts.config.key();
    session.expiry = expiry;
    session.bump = ctx.bumps.session;
    Ok(())
}

#[derive(Accounts)]
pub struct RevokeSession<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,

    pub config: Box<Account<'info, TableConfig>>,

    #[account(
        mut,
        close = wallet,
        seeds = [SESSION_SEED, wallet.key().as_ref(), config.key().as_ref()],
        bump = session.bump,
        constraint = session.authority == wallet.key() @ RiverproofError::UnauthorizedActor
    )]
    pub session: Account<'info, SessionKey>,
}

pub fn revoke_session(_ctx: Context<RevokeSession>) -> Result<()> {
    Ok(())
}
