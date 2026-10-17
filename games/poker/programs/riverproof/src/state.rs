//! On-chain account layout.
//!
//! The split is by what a thing is, not by who writes it. `HandCards` is the
//! only account that holds card material and it holds only what a showdown has
//! already made public; `MentalDeal` holds the commitments and the locked
//! positions a hand opens. Everything else is plain program state, and no
//! account anywhere carries a deck.

use anchor_lang::prelude::*;
use riverproof_engine::MAX_SEATS;

pub const MAX_USERNAME_LEN: usize = 20;
pub const MIN_USERNAME_LEN: usize = 3;

pub const PROFILE_SEED: &[u8] = b"profile";
pub const USERNAME_SEED: &[u8] = b"username";
pub const SESSION_SEED: &[u8] = b"session";
pub const TABLE_SEED: &[u8] = b"table";
pub const TABLE_STATE_SEED: &[u8] = b"table_state";
pub const HAND_SEED: &[u8] = b"hand";
pub const HAND_CARDS_SEED: &[u8] = b"hand_cards";
pub const VAULT_SEED: &[u8] = b"vault";
pub const FAUCET_SEED: &[u8] = b"faucet";
pub const MENTAL_DEAL_SEED: &[u8] = b"mental_deal";
pub const ENDPOINT_SEED: &[u8] = b"endpoint";

/// Wallet to username, plus lifetime statistics.
///
/// Replaces the `localStorage` reconnect-token scheme the off-chain build used.
#[account]
#[derive(InitSpace)]
pub struct PlayerProfile {
    pub wallet: Pubkey,
    #[max_len(MAX_USERNAME_LEN)]
    pub username: String,
    /// Lowercased form the `UsernameClaim` is seeded on.
    #[max_len(MAX_USERNAME_LEN)]
    pub normalized: String,
    pub created_at: i64,
    pub hands_played: u64,
    pub hands_won: u64,
    pub last_faucet_claim: i64,
    pub bump: u8,
}

/// Seeded on the normalized username so uniqueness is enforced by the runtime
/// refusing to initialize the same PDA twice. No server registry needed.
#[account]
#[derive(InitSpace)]
pub struct UsernameClaim {
    pub owner: Pubkey,
    #[max_len(MAX_USERNAME_LEN)]
    pub normalized: String,
    pub bump: u8,
}

/// Faucet configuration for the play-money mint.
#[account]
#[derive(InitSpace)]
pub struct FaucetConfig {
    pub mint: Pubkey,
    pub authority: Pubkey,
    /// Amount minted per claim, in base units.
    pub claim_amount: u64,
    /// Seconds a profile must wait between claims.
    pub cooldown_seconds: i64,
    pub bump: u8,
}

/// How many tables one RPC endpoint is carrying, and how many it may.
///
/// A table is played through the endpoint whoever opened it brought, and one
/// endpoint carrying too many is slow for everybody at all of them. The count
/// is here rather than in the client because a client-side limit is not a limit
/// at all: `create_table` is public and anybody can call it directly.
///
/// The *cap* cannot be derived the same way. It depends on the plan behind the
/// key, and a program has no network to ask — Helius publishes nothing about a
/// plan over RPC either, which was measured rather than assumed: no header, no
/// method, no difference in rate behaviour. So every endpoint starts at
/// `DEFAULT_ENDPOINT_CAP` and the faucet authority raises the ones it has
/// checked by hand. Self-declaring a plan would be worth nothing, since the
/// declaration would come from the same client the limit is meant to bind.
#[account]
#[derive(InitSpace)]
pub struct EndpointQuota {
    /// SHA-256 of the endpoint URL, which is also this account's seed — so the
    /// quota for one endpoint cannot be spent by naming another's account.
    pub endpoint: [u8; 32],
    /// How many tables this endpoint may carry at once.
    pub cap: u16,
    /// How many it is carrying. Raised by `create_table`, lowered by
    /// `close_table` — otherwise abandoning a table would spend a slot for ever.
    pub tables: u16,
    pub bump: u8,
}

/// What an endpoint may carry before anybody has vouched for it.
///
/// One, because that is what a free key comfortably runs and there is no way to
/// tell a free key from a paid one by asking it. Raising it is a deliberate act
/// by the faucet authority, who can look at the plan the way the program
/// cannot.
pub const DEFAULT_ENDPOINT_CAP: u16 = 1;

/// Immutable table settings.
#[account]
#[derive(InitSpace)]
pub struct TableConfig {
    pub creator: Pubkey,
    pub mint: Pubkey,
    #[max_len(32)]
    pub name: String,
    /// The RPC endpoint this table is played through.
    ///
    /// Every client at a table talks to the chain constantly — a hand is
    /// forty-four transactions three-handed and two hundred and six at a full
    /// table, on top of the account subscriptions every seat holds open — so
    /// one shared endpoint is the ceiling on how many tables can exist at once
    /// rather than how many players want to play. Whoever opens a table brings
    /// the endpoint it runs on, and the cost scales with tables instead of
    /// landing on whoever deployed the app.
    ///
    /// Stored here rather than agreed off chain because every seat needs the
    /// same answer and there is no other place all of them already read. It is
    /// public, and that is understood: a browser cannot reach an endpoint
    /// without the URL, so any endpoint a client uses is visible to anyone who
    /// opens the page. Restricting the key to an origin is what makes it safe,
    /// which is the same protection the deployment's own endpoint has always
    /// relied on — see `scrubSecrets` in `app/chain/wallet-log.ts`.
    ///
    /// 200 is a Helius URL with room: the host, the path, and an `api-key`
    /// query parameter of a UUID's length, with slack for a longer key or a
    /// different provider.
    #[max_len(200)]
    pub rpc_url: String,
    /// The public half of a key derived from this table's password, or all
    /// zeroes for a table anyone may sit at.
    ///
    /// A password cannot be checked on chain by being sent to the chain: an
    /// instruction's data is public for ever, so the first player to join a
    /// private table would publish its password to everyone watching. Nor can
    /// it be stored here hashed and compared, for the same reason — the
    /// comparison needs the password.
    ///
    /// So the password never leaves the client. It is stretched into a keypair
    /// (see `tableGate` in `app/chain/table-gate.ts`), and this is that
    /// keypair's public half. Joining means *signing the join transaction* with
    /// the private half, which proves the password without naming it: the
    /// signature covers a message nobody else will send again, so it cannot be
    /// replayed, and a signature reveals nothing about the key behind it.
    ///
    /// What this does not defend against is grinding. The public key is here
    /// for anyone to read, so passwords can be tried offline as fast as
    /// keypairs can be derived — which is why the client stretches with PBKDF2
    /// rather than hashing once, and why the table says so when it asks for one.
    pub join_key: Pubkey,
    pub table_id: u64,
    pub small_blind: u64,
    pub big_blind: u64,
    pub ante: u64,
    pub buy_in: u64,
    pub max_players: u8,
    /// Stored creation timer; the native bridge enforces the current allowance
    /// for new turns in legacy accounts without changing their layout.
    pub action_time_ms: i64,
    pub bump: u8,
    pub vault_bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, PartialEq, Eq, Debug)]
pub struct SeatRecord {
    pub owner: Pubkey,
    pub stack: u64,
    pub occupied: bool,
    pub ready: bool,
    /// Legacy byte, preserved in place: true = manual actions, false = explicit
    /// auto check/fold. Previously always true for occupied onchain seats.
    /// It is not a network-presence signal. ready controls encrypted dealing.
    pub connected: bool,
    pub leave_after_hand: bool,
    /// Unix milliseconds this seat stopped being ready, or 0 while it is.
    ///
    /// The clock the takeover grace period is measured from. It has to be a
    /// timestamp rather than a count of hands, because the case that most needs
    /// it is a table that cannot deal a hand *at all* — three seats with one
    /// player away is a stuck table, and a hand-based grace period would never
    /// come due there.
    pub sitting_out_since: i64,
}

impl SeatRecord {
    /// Sets `ready`, stamping when it went false so a takeover can be timed.
    ///
    /// Every path that changes readiness goes through here rather than
    /// assigning the field, so there is one answer to "since when" instead of
    /// one per instruction that happened to remember.
    pub fn set_ready(&mut self, ready: bool, now: i64) {
        if ready {
            self.sitting_out_since = 0;
        } else if self.ready || self.sitting_out_since == 0 {
            self.sitting_out_since = now;
        }
        self.ready = ready;
    }
}

/// Mutable table state: who is seated, their stacks, and the button.
#[account]
#[derive(InitSpace)]
pub struct TableState {
    pub table: Pubkey,
    pub seats: [SeatRecord; MAX_SEATS],
    /// `u8::MAX` when no hand has been dealt yet.
    pub dealer_seat: u8,
    pub hand_counter: u64,
    /// Unix seconds; 0 when no hand is scheduled.
    pub next_hand_at: i64,
    pub version: u64,
    /// Chips escrowed in the vault for this table. Must equal the sum of seat
    /// stacks plus any live pot.
    pub escrowed: u64,
    pub bump: u8,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum HandStage {
    Idle,
    Preflop,
    Flop,
    Turn,
    River,
    Complete,
}

#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, PartialEq, Eq, Debug)]
pub struct WinnerRecord {
    pub seat: u8,
    pub amount: u64,
    /// Hand category index, or `u8::MAX` for a pot won uncontested.
    pub category: u8,
}

/// Per-hand betting state. Holds no card material other than the public board.
#[account]
#[derive(InitSpace)]
pub struct HandState {
    pub table: Pubkey,
    pub hand_id: u64,
    pub stage: HandStage,
    pub version: u32,
    pub dealer_seat: u8,
    pub small_blind_seat: u8,
    pub big_blind_seat: u8,
    pub active_seats: u16,
    pub folded: u16,
    pub all_in: u16,
    pub acted: u16,
    pub street_bets: [u64; MAX_SEATS],
    pub contributions: [u64; MAX_SEATS],
    pub current_bet: u64,
    pub min_raise: u64,
    /// `u8::MAX` when nobody is on the clock.
    pub actor_seat: u8,
    /// Unix milliseconds.
    pub action_deadline: i64,
    pub board: [u8; 5],
    pub board_len: u8,
    pub deck_index: u8,
    pub settled: bool,
    /// Which reveal the hand is parked on, if any. 0 none, 1 flop, 2 turn,
    /// 3 river, 4 all-in run-out, 5 showdown, 6 the deal itself. The pause
    /// spans transactions, so it has to be persisted rather than held in the
    /// engine.
    pub awaiting: u8,
    /// Contenders whose hole cards have been revealed at showdown. Settlement
    /// cannot run until this covers every live seat.
    pub revealed_holes: u16,
    /// Seats skipped by `force_timeout` this hand.
    ///
    /// Persisted because the decision it feeds spans transactions: the skip
    /// happens in one instruction and the un-readying in whichever later one
    /// completes the hand. Cleared with the rest of the hand when the next
    /// starts.
    pub timed_out: u16,
    pub winners: [WinnerRecord; MAX_SEATS],
    pub winner_count: u8,
    pub bump: u8,
}

/// Card material for one hand.
///
/// There is no deck here, and there must never be one again. The players hold
/// the deck between themselves; what the chain keeps is in `MentalDeal` — a
/// digest per round and the twenty-five locked positions a hand opens — and
/// what lands here is only what a showdown has already made public.
#[account]
#[derive(InitSpace)]
pub struct HandCards {
    pub table: Pubkey,
    pub hand_id: u64,

    /// Bitmask of seats revealed at showdown.
    ///
    /// Written at settlement as the live contenders, and read by the client to
    /// decide whose cards it may render face up.
    pub revealed: u16,
    /// This hand's cards are dealt confidentially, so the engine holds no deck.
    ///
    /// Set when the hand starts, before a single round is committed, because
    /// the engine has to know from the first action that it must park for its
    /// cards rather than deal them itself.
    pub confidential: bool,
    pub bump: u8,

    /// Hole cards revealed at showdown, in the clear.
    ///
    /// Only ever written for seats that reached showdown without folding, by
    /// `reveal_mental_hole` once every player has published a share for that
    /// seat's two positions. Entries accumulate across several transactions —
    /// one reveal per contender — which is why they persist here rather than
    /// living inside a single instruction.
    ///
    /// Revealed board cards land on `HandState`, not here.
    pub hole_cards: [[u8; 2]; MAX_SEATS],
}

/// One dealt position, as the chain tracks it while the players open it.
///
/// `point` starts as the card the players locked and loses one layer per
/// published unlock share. When every player has contributed it *is* a card
/// point, and `card` is which one. The chain is never told the card.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, Default, InitSpace, PartialEq, Eq, Debug)]
pub struct DealSlot {
    /// The deck position, as the engine indexes it.
    pub position: u8,
    /// The seat this card belongs to, or `NO_SEAT` for a board card.
    pub owner: u8,
    pub point: [u8; 32],
    /// Players who have published a share. Indexed by player, not by seat.
    pub unlocked: u16,
    /// `u8::MAX` until the last layer comes off.
    pub card: u8,
}

/// How far along a mental deal is.
#[derive(AnchorSerialize, AnchorDeserialize, Clone, Copy, PartialEq, Eq, Debug, InitSpace)]
pub enum DealPhase {
    /// Players are masking and permuting the deck, one after another.
    Shuffling,
    /// Players are replacing their masks with one key per position.
    Locking,
    /// The locked deck is on chain and cards can be opened.
    Sealed,
}

/// A hand the players deal themselves.
///
/// Almost nothing of the protocol lives here. The shuffle rounds and lock
/// rounds are large — thousands of bytes each, per player — and every player
/// verifies them off chain, so what the chain keeps is a digest of each: enough
/// that nobody can show one transcript to one player and a different one to
/// another, and enough to say whose round was wrong when a card fails to open.
///
/// What the chain does hold in full is the twenty-five positions a hand
/// actually opens, and it opens them itself. See `crate::mental`.
#[account]
#[derive(InitSpace)]
pub struct MentalDeal {
    pub table: Pubkey,
    /// The hand these slots belong to. Everything here is stale for any other.
    pub hand_id: u64,
    /// Player index to seat. Only the first `players` entries mean anything.
    pub seats: [u8; MAX_SEATS],
    pub players: u8,
    /// How many players it takes to rebuild an absent one's board key: every
    /// player but one, so any two honest players can stop the board being
    /// opened early. Never fewer than two, which is why a hand needs three
    /// seats.
    ///
    /// Recorded for the clients, not used here. This program opens a slot only
    /// when every player has peeled it (`Slot::is_open`), so it never rebuilds
    /// anybody's key — but the browsers size their share packages off this
    /// field, so it is load-bearing for them.
    pub threshold: u8,
    /// Whose turn it is to commit a round. Rounds are sequential: each player
    /// masks the deck the one before them handed on.
    ///
    /// **It means something else once the deck is sealed**, and an operator
    /// reading a stuck hand needs to know which. Rounds are finished by then —
    /// `commit_deal_round` refuses to run outside `Shuffling` and `Locking`,
    /// and `seal_mental_deck` will not seal until this has wrapped back to zero
    /// — so `abandon_mental_deal` borrows the byte to remember which board
    /// street has already spent its one grace, holding `board_len + 1`. So at
    /// `phase = Sealed`, a `4` here means the turn has had its grace and the
    /// next ask will void the hand; it does **not** mean the deal is waiting on
    /// player 4, who at a three-handed table does not exist.
    ///
    /// Borrowed rather than given its own field for three measured reasons, in
    /// increasing order of how final they are: `MentalDeal` is created
    /// `init_if_needed`, and Anchor neither resizes an existing account (it
    /// fails `AccountDidNotDeserialize`) nor tolerates one padded with slack
    /// (it fails `ConstraintSpace`, because the space assertion is exact); and
    /// `StartMentalHand::try_accounts` has one byte of BPF stack headroom left,
    /// measured — a `u8` still builds and two bytes does not. Growing this
    /// struct is not a thing that can be done without a migration crank and a
    /// frame to spare.
    pub next_round: u8,
    pub phase: DealPhase,
    // There is deliberately no per-player identity key here, and its absence is
    // a measured constraint rather than an oversight. The share packages in a
    // lock round are encrypted to each recipient's key-exchange public key, so
    // those keys have to be authenticated: a carrier that could substitute one
    // would be handed the share dealt to that player, and enough of those
    // rebuild a board key early.
    //
    // On chain is the obvious home and it does not fit. Adding `[[u8; 32];
    // MAX_SEATS]` here pushes Anchor's generated `try_accounts` for
    // `StartMentalHand` to a 4,736-byte frame against BPF's 4,096, and the
    // program stops loading. Measured, not guessed - `anchor build` names the
    // function and the overage.
    //
    // The right home is inside the shuffle round, where the digest each player
    // already commits here authenticates it for free and costs no account
    // space. See `docs/DEALING.md`.
    pub shuffle_commitments: [[u8; 32]; MAX_SEATS],
    pub lock_commitments: [[u8; 32]; MAX_SEATS],
    pub slots: [DealSlot; crate::mental::MAX_SLOTS],
    pub slot_count: u8,
    /// Unix milliseconds after which anyone may abandon this deal.
    ///
    /// Refreshed by every step that makes progress, so a slow deal is never
    /// abandoned, only a stopped one. Without it a hand whose cards never
    /// arrive sits with the blinds in the pot and nothing on chain able to move
    /// it: `force_timeout` needs somebody on the clock, and `start_next_hand`
    /// needs a completed hand.
    pub deadline: i64,
    /// How much of the locked deck has landed.
    ///
    /// A ten-seat deck is twenty-five points, and twenty-five points do not fit
    /// in a transaction alongside their accounts — measured at 1,285 bytes
    /// against a 1,232-byte limit, which is how this field came to exist. So it
    /// arrives in contiguous chunks and nothing opens until `slot_count` of
    /// them are down.
    pub sealed_slots: u8,
    pub bump: u8,
}


/// Scoped action authority so a player signs once per table rather than once
/// per action.
///
/// A leaked session key can fold one player's hands at one table until expiry.
/// It can never move funds: buy-in, rebuy, and cash-out all require the wallet.
#[account]
#[derive(InitSpace)]
pub struct SessionKey {
    /// The real wallet that authorized this session.
    pub authority: Pubkey,
    /// The ephemeral keypair allowed to sign actions.
    pub session_key: Pubkey,
    /// Scoped to a single table.
    pub table: Pubkey,
    /// Unix seconds after which the key is dead.
    pub expiry: i64,
    pub bump: u8,
}

impl SessionKey {
    pub fn is_valid_for(&self, signer: &Pubkey, seat_owner: &Pubkey, table: &Pubkey, now: i64) -> bool {
        self.session_key == *signer
            && self.authority == *seat_owner
            && self.table == *table
            && self.expiry > now
    }
}
