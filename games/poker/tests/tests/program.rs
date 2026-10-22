//! End-to-end tests against the compiled program in an in-process SVM.
//!
//! These run the real `riverproof.so`, so they cover the account layout, PDA
//! derivations, SPL escrow, and the authority model - the parts the engine's own
//! differential tests cannot reach.
//!
//! The hand played here is also replayed through `riverproof-engine` directly,
//! so the on-chain result is checked against the engine, which is in turn
//! checked against the JavaScript oracle. Run `anchor build` first.

use anchor_lang::{AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas, Owner};
fn get_associated_token_address(owner: &Pubkey, mint: &Pubkey) -> Pubkey {
    anchor_spl::associated_token::get_associated_token_address_with_program_id(owner, mint, &anchor_spl::token_2022::ID)
}
use litesvm::LiteSVM;
use solana_compute_budget_interface::ComputeBudgetInstruction;
use riverproof::instructions::DealRound;
use riverproof::state::{HandStage, HandState, MentalDeal, PlayerProfile, TableConfig, TableState};
use riverproof_engine::table as engine;
use riverproof_mental::group::{encode_point, encode_scalar, Seed};
use riverproof_mental::protocol::{
    deal_hand, lock_round_commitment, lock_round_digest, shuffle_round_digest, Session,
};
use solana_sdk::{
    clock::Clock,
    instruction::Instruction,
    pubkey::Pubkey,
    signature::{Keypair, Signer},
    transaction::Transaction,
};

const PROGRAM_SO: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../target/deploy/riverproof.so");

const SMALL_BLIND: u64 = 10;
const ANTE: u64 = 0;
const BUY_IN: u64 = 1_000;

#[test]
fn courtyard_mainnet_proxy_is_allowed_without_accepting_lookalikes() {
    let mut h = Harness::new();
    let owner = h.player("gateway_test");
    let gateway = "https://hooddice-holdem-mainnet.hooddice.workers.dev/rpc";
    create_table_on(&mut h, &owner, 10, Pubkey::default(), 40, gateway).unwrap();
    for (i, url) in [
        "http://hooddice-holdem-mainnet.hooddice.workers.dev/rpc",
        "https://hooddice-holdem-mainnet.hooddice.workers.dev.evil.example/rpc",
        "https://hooddice-holdem-mainnet.hooddice.workers.dev/other",
        "https://hooddice-holdem-mainnet.hooddice.workers.dev/rpc?api-key=private",
    ].iter().enumerate() {
        let error = create_table_on(&mut h, &owner, 10, Pubkey::default(), 41 + i as u64, url).expect_err("unsafe gateway accepted");
        assert!(error.contains("EndpointNotAllowed"), "wrong refusal: {error}");
    }
}

#[test]
fn only_the_deployment_authority_can_initialize_the_operator() {
    let mut h = Harness::uninitialized();
    let attacker = Keypair::new();
    h.svm.airdrop(&attacker.pubkey(), 1_000_000_000).unwrap();
    let instruction = Instruction {
        program_id: program_id(),
        accounts: riverproof::accounts::InitializeFaucet {
            authority: attacker.pubkey(), program_data: program_data_pda(),
            faucet: faucet_pda(), mint: mint_pda(),
            token_program: anchor_spl::token_2022::ID,
            system_program: anchor_lang::system_program::ID,
            rent: solana_sdk::sysvar::rent::ID,
        }.to_account_metas(None),
        data: riverproof::instruction::InitializeFaucet { claim_amount: 0, cooldown_seconds: 60 }.data(),
    };
    assert!(h.send(instruction.clone(), &[&attacker]).is_err());
    assert!(h.svm.get_account(&faucet_pda()).is_none());
    assert!(h.svm.get_account(&mint_pda()).is_none());
    // A lookalike ProgramData account must not authorize another signer.
    let fake = Pubkey::new_unique();
    let mut copied = h.svm.get_account(&program_data_pda()).unwrap();
    copied.data[13..45].copy_from_slice(attacker.pubkey().as_ref());
    h.svm.set_account(fake, copied).unwrap();
    let mut spoofed = instruction;
    spoofed.accounts[1].pubkey = fake;
    assert!(h.send(spoofed, &[&attacker]).is_err());
    assert!(h.svm.get_account(&faucet_pda()).is_none());
    h.initialize_faucet();
    let operator: riverproof::state::FaucetConfig = h.account(&faucet_pda());
    assert_eq!(operator.authority, h.payer.pubkey());
}

#[test]
fn sdice_rejects_legacy_wrong_decimals_freezing_and_transfer_extensions() {
    for variant in 0..4 {
        let mut h = Harness::new();
        let owner = h.player("sdice_test");
        let mut mint = h.svm.get_account(&h.mint).unwrap();
        match variant {
            0 => mint.owner = anchor_spl::token::ID,
            1 => mint.data[44] = 0,
            2 => { mint.data[46..50].copy_from_slice(&1u32.to_le_bytes()); mint.data[50..82].copy_from_slice(owner.pubkey().as_ref()); },
            _ => {
                // Initialized mint with a valid-sized TransferFeeConfig extension.
                mint.data.resize(166 + 4 + 108, 0);
                mint.data[165] = 1;
                mint.data[166..168].copy_from_slice(&1u16.to_le_bytes());
                mint.data[168..170].copy_from_slice(&108u16.to_le_bytes());
                mint.lamports = 10_000_000;
            }
        }
        h.svm.set_account(h.mint, mint).unwrap();
        assert!(create_table_on(&mut h, &owner, 6, Pubkey::default(), 31, RPC_URL).is_err(), "variant {variant} accepted");
        assert!(h.svm.get_account(&table_pda(&owner.pubkey(), 31)).is_none());
    }
}

#[test]
fn sdice_accepts_the_exact_public_metadata_mint_format() {
    use base64::Engine;
    let mut h = Harness::new();
    let owner = h.player("metadata_test");
    let fixture: serde_json::Value = serde_json::from_str(include_str!("../fixtures/sdice-mainnet-mint.json")).unwrap();
    let mut mint = h.svm.get_account(&h.mint).unwrap();
    mint.data = base64::engine::general_purpose::STANDARD.decode(fixture["data"].as_str().unwrap()).unwrap();
    mint.lamports = 10_000_000;
    h.svm.set_account(h.mint, mint).unwrap();
    create_table_on(&mut h, &owner, 6, Pubkey::default(), 32, RPC_URL).unwrap();
}

#[test]
fn sdice_join_cannot_substitute_a_different_mint() {
    let mut h = Harness::new();
    let owner = h.player("mint_test");
    let table = create_table(&mut h, &owner, 6);
    let wrong_mint = Pubkey::new_unique();
    h.svm.set_account(wrong_mint, h.svm.get_account(&h.mint).unwrap()).unwrap();
    let mut accounts = seat_accounts(&h, &owner.pubkey(), &table);
    accounts.mint = wrong_mint;
    accounts.player_tokens = get_associated_token_address(&owner.pubkey(), &wrong_mint);
    assert!(h.send(Instruction { program_id: program_id(), accounts: accounts.to_account_metas(None), data: riverproof::instruction::JoinTable { seat_index: 0 }.data() }, &[&owner]).is_err());
    assert_eq!(h.vault_balance(&table), 0);
    assert_eq!(h.token_balance(&owner.pubkey()), FAUCET_AMOUNT);
}
/// SHA-256 of an endpoint URL, which is what its quota account is seeded from.
fn endpoint_hash(url: &str) -> [u8; 32] {
    solana_sdk::hash::hashv(&[url.as_bytes()]).to_bytes()
}

fn endpoint_quota_pda(url: &str) -> Pubkey {
    Pubkey::find_program_address(&[b"endpoint", &endpoint_hash(url)], &program_id()).0
}

/// Shape only — the program checks length and scheme and nothing else, because
/// a program has no network to ask whether an endpoint answers.
const RPC_URL: &str = "https://devnet.helius-rpc.com/?api-key=00000000-0000-0000-0000-000000000000";

const ACTION_TIME_MS: i64 = 40_000;
const FAUCET_AMOUNT: u64 = 10_000;
const TABLE_ID: u64 = 7;
/// The most a transaction may ask for.
const MAX_COMPUTE_UNITS: u32 = 1_400_000;
/// Locked-deck points per transaction. Sixteen leaves comfortable headroom
/// under the 1232-byte limit at every table size.
const SEAL_CHUNK: usize = 16;

struct Harness {
    svm: LiteSVM,
    payer: Keypair,
    mint: Pubkey,
    now: i64,
    /// Opt-in registry account for managed-table integration scenarios.
    arena: Option<Pubkey>,
}

fn program_id() -> Pubkey {
    riverproof::ID
}

fn program_data_pda() -> Pubkey {
    Pubkey::find_program_address(&[program_id().as_ref()], &anchor_lang::prelude::ProgramData::owner()).0
}

impl Harness {
    fn new() -> Self {
        let mut harness = Self::uninitialized();
        harness.mint = harness.initialize_faucet();
        harness
    }

    fn uninitialized() -> Self {
        let mut svm = LiteSVM::new();
        svm.add_program_from_file(program_id(), PROGRAM_SO)
            .expect("anchor build must run first: target/deploy/riverproof.so is missing");

        let payer = Keypair::new();
        svm.airdrop(&payer.pubkey(), 100_000_000_000).unwrap();
        // LiteSVM installs programs immutable by default. Model the real loader's
        // upgrade-authority metadata without changing the executable bytes.
        let mut program_data = svm.get_account(&program_data_pda()).unwrap();
        assert_eq!(&program_data.data[..4], &3u32.to_le_bytes());
        program_data.data[12] = 1;
        program_data.data[13..45].copy_from_slice(payer.pubkey().as_ref());
        svm.set_account(program_data_pda(), program_data).unwrap();

        let mut harness = Self {
            svm,
            payer,
            mint: Pubkey::default(),
            now: 1_700_000_000,
            arena: None,
        };
        harness.set_clock(harness.now);
        harness
    }

    fn set_clock(&mut self, unix_timestamp: i64) {
        self.now = unix_timestamp;
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = unix_timestamp;
        self.svm.set_sysvar(&clock);
    }

    fn advance(&mut self, seconds: i64) {
        self.set_clock(self.now + seconds);
    }

    fn send(&mut self, instruction: Instruction, signers: &[&Keypair]) -> Result<(), String> {
        self.send_metered(instruction, signers).map(|_| ())
    }

    /// Sends, and reports what it cost: compute units burned, and the size of
    /// the transaction on the wire.
    ///
    /// Size is computed rather than taken from a serializer: a compact-u16
    /// count of signatures, sixty-four bytes each, then the serialized message.
    /// Under 128 signatures the count is one byte, which every transaction here
    /// is comfortably inside.
    fn send_metered(
        &mut self,
        mut instruction: Instruction,
        signers: &[&Keypair],
    ) -> Result<(u64, usize), String> {
        if let Some(arena) = self.arena {
            if !instruction.accounts.iter().any(|a| a.pubkey == arena) {
                instruction.accounts.push(solana_sdk::instruction::AccountMeta::new(arena, false));
            }
        }
        self.svm.expire_blockhash();
        let mut all: Vec<&Keypair> = vec![&self.payer];
        all.extend_from_slice(signers);
        // Every transaction asks for the maximum, the way the client does.
        // Settlement at a full table does not fit the 200,000-unit default, and
        // a harness that did not ask would be measuring a different program to
        // the one that ships.
        let tx = Transaction::new_signed_with_payer(
            &[
                ComputeBudgetInstruction::set_compute_unit_limit(MAX_COMPUTE_UNITS),
                instruction,
            ],
            Some(&self.payer.pubkey()),
            &all,
            self.svm.latest_blockhash(),
        );
        let size = 1 + 64 * tx.signatures.len() + tx.message.serialize().len();
        self.svm
            .send_transaction(tx)
            .map(|meta| (meta.compute_units_consumed, size))
            // The program's own log line is the useful part; the error code
            // alone sends you looking in the wrong crate.
            .map_err(|failure| {
                let named = failure
                    .meta
                    .logs
                    .iter()
                    .find(|line| line.contains("Error Message:") || line.contains("Error Code:"))
                    .cloned()
                    .unwrap_or_default();
                format!("{:?} {}", failure.err, named)
            })
    }

    fn account<T: AccountDeserialize>(&self, address: &Pubkey) -> T {
        let raw = self.svm.get_account(address).expect("account exists");
        T::try_deserialize(&mut raw.data.as_slice()).expect("account deserializes")
    }

    fn player(&mut self, username: &str) -> Keypair {
        let wallet = Keypair::new();
        self.svm.airdrop(&wallet.pubkey(), 10_000_000_000).unwrap();

        self.send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::CreateProfile {
                    wallet: wallet.pubkey(),
                    profile: profile_pda(&wallet.pubkey()),
                    claim: claim_pda(&username.to_ascii_lowercase()),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::CreateProfile {
                    username: username.to_string(),
                    normalized: username.to_ascii_lowercase(),
                }
                .data(),
            },
            &[&wallet],
        )
        .expect("create_profile");

        self.claim_faucet(&wallet);
        wallet
    }

    fn claim_faucet(&mut self, wallet: &Keypair) {
        let mint = self.mint;
        self.send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::ClaimFaucet {
                    wallet: wallet.pubkey(),
                    profile: profile_pda(&wallet.pubkey()),
                    faucet: faucet_pda(),
                    mint,
                    destination: get_associated_token_address(&wallet.pubkey(), &mint),
                    token_program: anchor_spl::token_2022::ID,
                    associated_token_program: anchor_spl::associated_token::ID,
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::ClaimFaucet {}.data(),
            },
            &[wallet],
        )
        .expect("claim_faucet");
    }

    fn initialize_faucet(&mut self) -> Pubkey {
        let mint = mint_pda();
        let authority = self.payer.insecure_clone();
        self.send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::InitializeFaucet {
                    authority: authority.pubkey(),
                    program_data: program_data_pda(),
                    faucet: faucet_pda(),
                    mint,
                    token_program: anchor_spl::token_2022::ID,
                    system_program: anchor_lang::system_program::ID,
                    rent: solana_sdk::sysvar::rent::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::InitializeFaucet {
                    claim_amount: FAUCET_AMOUNT,
                    cooldown_seconds: 60,
                }
                .data(),
            },
            &[],
        )
        .expect("initialize_faucet");
        mint
    }

    fn token_balance(&self, owner: &Pubkey) -> u64 {
        let address = get_associated_token_address(owner, &self.mint);
        self.svm
            .get_account(&address)
            .map(|account| {
                // SPL token account: amount is a little-endian u64 at offset 64.
                u64::from_le_bytes(account.data[64..72].try_into().unwrap()) / riverproof::sdice::UNITS
            })
            .unwrap_or(0)
    }

    fn vault_balance(&self, config: &Pubkey) -> u64 {
        let account = self.svm.get_account(&vault_pda(config)).expect("vault");
        u64::from_le_bytes(account.data[64..72].try_into().unwrap()) / riverproof::sdice::UNITS
    }
}

fn pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &program_id()).0
}

fn profile_pda(wallet: &Pubkey) -> Pubkey {
    pda(&[b"profile", wallet.as_ref()])
}

fn claim_pda(normalized: &str) -> Pubkey {
    pda(&[b"username", normalized.as_bytes()])
}

fn faucet_pda() -> Pubkey {
    pda(&[b"faucet"])
}

fn mint_pda() -> Pubkey {
    pda(&[b"mint"])
}

fn table_pda(creator: &Pubkey, table_id: u64) -> Pubkey {
    pda(&[b"table", creator.as_ref(), &table_id.to_le_bytes()])
}

fn table_state_pda(config: &Pubkey) -> Pubkey {
    pda(&[b"table_state", config.as_ref()])
}

fn hand_pda(config: &Pubkey) -> Pubkey {
    pda(&[b"hand", config.as_ref()])
}

fn hand_cards_pda(config: &Pubkey) -> Pubkey {
    pda(&[b"hand_cards", config.as_ref()])
}

fn mental_deal_pda(config: &Pubkey) -> Pubkey {
    pda(&[riverproof::state::MENTAL_DEAL_SEED, config.as_ref()])
}

fn vault_pda(config: &Pubkey) -> Pubkey {
    pda(&[b"vault", config.as_ref()])
}

fn session_pda(wallet: &Pubkey, config: &Pubkey) -> Pubkey {
    pda(&[b"session", wallet.as_ref(), config.as_ref()])
}

fn seat_accounts(
    harness: &Harness,
    wallet: &Pubkey,
    config: &Pubkey,
) -> riverproof::accounts::SeatContext {
    riverproof::accounts::SeatContext {
        // Open tables need no gate; the private-table test supplies one.
        gate: None,
        wallet: *wallet,
        config: *config,
        table_state: table_state_pda(config),
        hand_state: hand_pda(config),
        vault: vault_pda(config),
        player_tokens: get_associated_token_address(wallet, &harness.mint),
        mint: harness.mint,
        token_program: anchor_spl::token_2022::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
}

fn play_accounts(
    signer: &Pubkey,
    session: Option<Pubkey>,
    config: &Pubkey,
) -> riverproof::accounts::Play {
    riverproof::accounts::Play {
        signer: *signer,
        session,
        config: *config,
        table_state: table_state_pda(config),
        hand_state: hand_pda(config),
        hand_cards: hand_cards_pda(config),
    }
}

fn create_table(harness: &mut Harness, creator: &Keypair, max_players: u8) -> Pubkey {
    create_table_locked(harness, creator, max_players, Pubkey::default())
}

/// The same, with the public half of a password-derived key. All zeroes is open.
fn create_table_locked(
    harness: &mut Harness,
    creator: &Keypair,
    max_players: u8,
    join_key: Pubkey,
) -> Pubkey {
    create_table_on(harness, creator, max_players, join_key, TABLE_ID, RPC_URL)
        .expect("create_table")
}

/// The same, naming the id and the endpoint, and saying what happened.
fn create_table_on(
    harness: &mut Harness,
    creator: &Keypair,
    max_players: u8,
    join_key: Pubkey,
    table_id: u64,
    rpc_url: &str,
) -> Result<Pubkey, String> {
    create_table_on_with_rules(harness, creator, max_players, join_key, table_id, rpc_url, SMALL_BLIND, BUY_IN)
}

fn create_table_on_with_rules(harness: &mut Harness, creator: &Keypair, max_players: u8,
    join_key: Pubkey, table_id: u64, rpc_url: &str, small_blind: u64, buy_in: u64) -> Result<Pubkey, String> {
    let config = table_pda(&creator.pubkey(), table_id);
    let mint = harness.mint;
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::CreateTable {
                    endpoint_quota: endpoint_quota_pda(rpc_url),
                    creator: creator.pubkey(),
                    config,
                    table_state: table_state_pda(&config),
                    hand_state: hand_pda(&config),
                    hand_cards: hand_cards_pda(&config),
                    vault: vault_pda(&config),
                    mint,
                    token_program: anchor_spl::token_2022::ID,
                    system_program: anchor_lang::system_program::ID,
                    rent: solana_sdk::sysvar::rent::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::CreateTable {
                    table_id,
                    name: "Integration".to_string(),
                    rpc_url: rpc_url.to_string(),
                    endpoint: endpoint_hash(rpc_url),
                    // Open, which is what almost every table is.
                    join_key,
                    small_blind,
                    ante: ANTE,
                    max_players,
                    buy_in,
                    action_time_ms: ACTION_TIME_MS,
                }
                .data(),
            },
            &[creator],
        )
        .map(|_| config)
}

fn join(harness: &mut Harness, wallet: &Keypair, config: &Pubkey, seat: u8) {
    join_with(harness, wallet, config, seat, None).expect("join_table");
}

/// Joining while presenting a table's gate key, and saying what happened.
fn join_with(
    harness: &mut Harness,
    wallet: &Keypair,
    config: &Pubkey,
    seat: u8,
    gate: Option<&Keypair>,
) -> Result<(), String> {
    let mut accounts = seat_accounts(harness, &wallet.pubkey(), config);
    accounts.gate = gate.map(|key| key.pubkey());
    let mut signers: Vec<&Keypair> = vec![wallet];
    if let Some(key) = gate {
        signers.push(key);
    }
    harness.send(
        Instruction {
            program_id: program_id(),
            accounts: accounts.to_account_metas(None),
            data: riverproof::instruction::JoinTable { seat_index: seat }.data(),
        },
        &signers,
    )
}

fn set_ready(harness: &mut Harness, wallet: &Keypair, config: &Pubkey, ready: bool) {
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::UpdateSeat {
                    wallet: wallet.pubkey(),
                    config: *config,
                    table_state: table_state_pda(config),
                    hand_state: hand_pda(config),
                }
                .to_account_metas(None),
                data: riverproof::instruction::SetReady { ready }.data(),
            },
            &[wallet],
        )
        .expect("set_ready");
}

/// A three-handed hand with the betting open.
///
/// Replaces `start_hand_confidential`, which existed only to get a hand in
/// progress without dealing one: Arcium had its deck by the time the hand
/// started, so betting opened immediately and the timeout, session-key and
/// staleness tests never needed a card.
///
/// There is no such shortcut now. A player-shuffled hand parks on
/// `Awaiting::Deal` with its blinds posted and refuses every action until every
/// hole card can be read, so a test that wants somebody on the clock has to
/// deal one. Slower, and more honest: these tests now run against the only way
/// a hand can actually begin.
fn hand_in_progress(harness: &mut Harness, wallets: &[Keypair], config: &Pubkey) {
    let signing = Signing { keys: wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(harness, &signing, config, &mut meter);
}

fn start_hand_mental(harness: &mut Harness, cranker: &Keypair, config: &Pubkey) -> Result<(), String> {
    start_hand_mental_metered(harness, cranker, config).map(|_| ())
}

fn start_hand_mental_metered(
    harness: &mut Harness,
    cranker: &Keypair,
    config: &Pubkey,
) -> Result<(u64, usize), String> {
    harness.send_metered(
        Instruction {
            program_id: program_id(),
            accounts: riverproof::accounts::StartMentalHand {
                cranker: cranker.pubkey(),
                config: *config,
                table_state: table_state_pda(config),
                hand_state: hand_pda(config),
                hand_cards: hand_cards_pda(config),
                deal: mental_deal_pda(config),
                system_program: anchor_lang::system_program::ID,
            }
            .to_account_metas(None),
            data: riverproof::instruction::StartHandMental {}.data(),
        },
        &[cranker],
    )
}

fn act(
    harness: &mut Harness,
    signer: &Keypair,
    session: Option<Pubkey>,
    config: &Pubkey,
    action: riverproof::instructions::PokerAction,
) -> Result<(), String> {
    let hand: HandState = harness.account(&hand_pda(config));
    harness.send(
        Instruction {
            program_id: program_id(),
            accounts: play_accounts(&signer.pubkey(), session, config).to_account_metas(None),
            data: riverproof::instruction::Act {
                action,
                expected_hand_id: hand.hand_id,
                expected_version: hand.version,
            }
            .data(),
        },
        &[signer],
    )
}

/// Two seated, funded, ready players and the table they sit at.
/// Two players at a table that seats three.
///
/// There is no two-seat table any more — `create_table` refuses one, because a
/// player-shuffled hand has no board threshold that is both private and live
/// with two participants. This is the nearest legal thing: a table with one
/// chair still empty, which is exactly the state a hand must refuse to deal.
fn two_seated() -> (Harness, Keypair, Keypair, Pubkey) {
    let mut harness = Harness::new();
    let alice = harness.player("alice");
    let bob = harness.player("bob");
    let config = create_table(&mut harness, &alice, 3);

    join(&mut harness, &alice, &config, 0);
    join(&mut harness, &bob, &config, 1);
    set_ready(&mut harness, &alice, &config, true);
    set_ready(&mut harness, &bob, &config, true);
    harness.advance(5);

    (harness, alice, bob, config)
}

#[test]
fn identity_enforces_global_username_uniqueness() {
    let mut harness = Harness::new();
    let _alice = harness.player("Alice");

    let profile: PlayerProfile = harness.account(&profile_pda(&_alice.pubkey()));
    assert_eq!(profile.username, "Alice");
    assert_eq!(profile.normalized, "alice");

    // A different wallet cannot take the same name, and case does not help:
    // the claim PDA is seeded on the normalized form.
    let impostor = Keypair::new();
    harness.svm.airdrop(&impostor.pubkey(), 1_000_000_000).unwrap();
    let failure = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::CreateProfile {
                    wallet: impostor.pubkey(),
                    profile: profile_pda(&impostor.pubkey()),
                    claim: claim_pda("alice"),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::CreateProfile {
                    username: "ALICE".to_string(),
                    normalized: "alice".to_string(),
                }
                .data(),
            },
            &[&impostor],
        )
        .unwrap_err();
    assert!(failure.contains("already in use") || failure.contains("Custom"), "{failure}");
}

#[test]
fn a_mismatched_normalization_is_rejected() {
    let mut harness = Harness::new();
    let wallet = Keypair::new();
    harness.svm.airdrop(&wallet.pubkey(), 1_000_000_000).unwrap();

    // The seed says "bob" but the display name normalizes to "robert".
    let failure = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::CreateProfile {
                    wallet: wallet.pubkey(),
                    profile: profile_pda(&wallet.pubkey()),
                    claim: claim_pda("bob"),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::CreateProfile {
                    username: "robert".to_string(),
                    normalized: "bob".to_string(),
                }
                .data(),
            },
            &[&wallet],
        )
        .unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");
}

#[test]
fn buy_in_moves_real_tokens_into_escrow() {
    let mut harness = Harness::new();
    let alice = harness.player("alice");
    let config = create_table(&mut harness, &alice, 3);

    assert_eq!(harness.token_balance(&alice.pubkey()), FAUCET_AMOUNT);
    join(&mut harness, &alice, &config, 0);

    assert_eq!(harness.token_balance(&alice.pubkey()), FAUCET_AMOUNT - BUY_IN);
    assert_eq!(harness.vault_balance(&config), BUY_IN);
    let vault = harness.svm.get_account(&vault_pda(&config)).unwrap();
    assert_eq!(vault.owner, anchor_spl::token_2022::ID);
    assert_eq!(u64::from_le_bytes(vault.data[64..72].try_into().unwrap()), BUY_IN * 1_000_000);

    let state: TableState = harness.account(&table_state_pda(&config));
    assert_eq!(state.escrowed, BUY_IN);
    assert_eq!(state.seats[0].stack, BUY_IN);
    assert!(state.seats[0].occupied);

    // Cashing out returns the chips and frees the seat.
    let accounts = seat_accounts(&harness, &alice.pubkey(), &config);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: accounts.to_account_metas(None),
                data: riverproof::instruction::LeaveTable {}.data(),
            },
            &[&alice],
        )
        .expect("leave_table");

    assert_eq!(harness.token_balance(&alice.pubkey()), FAUCET_AMOUNT);
    assert_eq!(harness.vault_balance(&config), 0);
    let state: TableState = harness.account(&table_state_pda(&config));
    assert_eq!(state.escrowed, 0);
    assert!(!state.seats[0].occupied);
}

/// `count` players, seated and ready.
///
/// Never fewer than three: that is the smallest table a player-shuffled hand can
/// be dealt to at all.
fn seated(count: usize) -> (Harness, Vec<Keypair>, Pubkey) {
    const NAMES: [&str; 10] = [
        "alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi", "ivan", "judy",
    ];
    let mut harness = Harness::new();
    let wallets: Vec<Keypair> = NAMES[..count].iter().map(|name| harness.player(name)).collect();
    let config = create_table(&mut harness, &wallets[0], count as u8);

    for (seat, wallet) in wallets.iter().enumerate() {
        join(&mut harness, wallet, &config, seat as u8);
        set_ready(&mut harness, wallet, &config, true);
    }
    harness.advance(5);
    (harness, wallets, config)
}

fn three_handed() -> (Harness, Vec<Keypair>, Pubkey) {
    seated(3)
}

/// Three players who did not sit in the first three seats.
///
/// `seated` fills from seat zero, which makes a seat index and a player index
/// the same number — and hides every place the two are confused. A real table
/// seats people wherever there is room.
fn seated_at(seats: &[u8]) -> (Harness, Vec<Keypair>, Pubkey) {
    const NAMES: [&str; 10] = [
        "alice", "bob", "carol", "dave", "erin", "frank", "grace", "heidi", "ivan", "judy",
    ];
    let mut harness = Harness::new();
    let wallets: Vec<Keypair> =
        NAMES[..seats.len()].iter().map(|name| harness.player(name)).collect();
    let config = create_table(&mut harness, &wallets[0], 10);

    for (wallet, seat) in wallets.iter().zip(seats) {
        join(&mut harness, wallet, &config, *seat);
        set_ready(&mut harness, wallet, &config, true);
    }
    harness.advance(5);
    (harness, wallets, config)
}

/// What an instruction cost, worst case across a run.
#[derive(Default)]
struct Meter {
    rows: Vec<(&'static str, u64, usize)>,
}

impl Meter {
    fn record(&mut self, label: &'static str, measured: (u64, usize)) {
        self.rows.push((label, measured.0, measured.1));
    }

    /// The most expensive send under this label: compute units, then bytes.
    fn worst(&self, label: &str) -> (u64, usize) {
        self.rows
            .iter()
            .filter(|row| row.0 == label)
            .fold((0, 0), |worst, row| (worst.0.max(row.1), worst.1.max(row.2)))
    }

    fn labels(&self) -> Vec<&'static str> {
        let mut labels: Vec<&'static str> = self.rows.iter().map(|row| row.0).collect();
        labels.sort_unstable();
        labels.dedup();
        labels
    }
}

/// Drives a hand from `start_hand_mental` to the point where every hole card is
/// one share short of opening — the state a hand is in when betting begins.
///
/// Shared by the correctness test and the cost test so that what is measured is
/// the same code path that is checked.
/// How many proved shares fit one transaction, measured by the size test.
const PROVED_SHARES_PER_TX: usize = 2;

fn deal_to_hole_cards(
    harness: &mut Harness,
    signing: &Signing,
    config: &Pubkey,
    meter: &mut Meter,
) -> (Session, MentalDeal, Vec<riverproof::state::DealSlot>) {
    let players = signing.len();
    meter.record(
        "start_hand_mental",
        start_hand_mental_metered(harness, signing.key(0), config).expect("start_hand_mental"),
    );

    // The chain decides which deck positions a hand opens and who owns them.
    // The players are told; they do not choose.
    let deal: MentalDeal = harness.account(&mental_deal_pda(config));
    assert_eq!(deal.players, players as u8);
    assert_eq!(deal.slot_count as usize, players * 2 + 5);
    let slots: Vec<riverproof::state::DealSlot> =
        deal.slots[..deal.slot_count as usize].to_vec();
    let board_positions: Vec<u8> = slots[players * 2..].iter().map(|slot| slot.position).collect();

    // Off chain, between the players. None of this touches the program.
    let seeds: Vec<Seed> = (0..players as u8)
        .map(|player| {
            let mut bytes = [0u8; 32];
            bytes[0] = player;
            bytes[1] = 0xd4;
            Seed::new(bytes)
        })
        .collect();
    let session = deal_hand(&seeds, &board_positions, None).expect("the players deal");

    // A digest per round, in turn. The rounds themselves are thousands of bytes
    // each and stay off chain; what the chain holds is enough that nobody can
    // later claim they published something else.
    for (player, round) in session.deal.shuffles.iter().enumerate() {
        meter.record(
            "commit_deal_round",
            mental_send_metered(
                harness,
                signing.key(player),
                signing.session(player),
                config,
                riverproof::instruction::CommitDealRound {
                    round: DealRound::Shuffle,
                    digest: shuffle_round_digest(round),
                        }
                .data(),
            )
            .expect("shuffle commitment"),
        );
    }
    for (player, round) in session.deal.locks.iter().enumerate() {
        meter.record(
            "commit_deal_round",
            mental_send_metered(
                harness,
                signing.key(player),
                signing.session(player),
                config,
                riverproof::instruction::CommitDealRound {
                    round: DealRound::Lock,
                    digest: lock_round_commitment(round),
                        }
                .data(),
            )
            .expect("lock commitment"),
        );
    }

    // The locked deck, at the positions this hand opens and no others. The
    // burns are dealt from and never looked at, so they never go on chain.
    let locked: Vec<[u8; 32]> = slots
        .iter()
        .map(|slot| encode_point(&session.locked[slot.position as usize]))
        .collect();
    // In chunks: a ten-seat deck is twenty-five points, which does not fit in
    // one transaction alongside its accounts.
    for (index, chunk) in locked.chunks(SEAL_CHUNK).enumerate() {
        meter.record(
            "seal_mental_deck",
            mental_send_metered(
                harness,
                signing.key(players - 1),
                signing.session(players - 1),
                config,
                riverproof::instruction::SealMentalDeck {
                    offset: (index * SEAL_CHUNK) as u8,
                    locked: chunk.to_vec(),
                }
                .data(),
            )
            .expect("seal_mental_deck"),
        );
    }

    // Every player takes their layer off every hole card except their own.
    for player in 0..players {
        let seat = deal.seats[player];
        let mut indices = Vec::new();
        let mut shares = Vec::new();
        let mut keys: Vec<[u8; 32]> = Vec::new();
        let mut paths: Vec<[u8; 32]> = Vec::new();
        for (index, slot) in slots.iter().enumerate().take(players * 2) {
            if slot.owner == seat {
                continue;
            }
            indices.push(index as u8);
            shares.push(encode_scalar(
                &session.participants[player].unlock(slot.position).expect("an unlock share"),
            ));
            // Somebody else's hole card, so the share is proved: the key this
            // player published for that position, and its path to the root
            // inside the commitment they made before any bytes moved.
            let round = &session.deal.locks[player];
            keys.push(round.keys[slot.position as usize]);
            paths.extend(riverproof_mental::merkle::path(&round.keys, slot.position as usize));
        }
        let round_digest = lock_round_digest(&session.deal.locks[player]);
        // A proved share is 224 bytes of key and path against a 1,232-byte
        // transaction, so these go out in twos. Board and own-hole shares owe
        // no proof and still batch whole.
        for chunk in 0..indices.len().div_ceil(PROVED_SHARES_PER_TX) {
            let from = chunk * PROVED_SHARES_PER_TX;
            let to = (from + PROVED_SHARES_PER_TX).min(indices.len());
            meter.record(
                "publish_unlocks (deal)",
                mental_send_metered(
                    harness,
                    signing.key(player),
                    signing.session(player),
                    config,
                    riverproof::instruction::PublishUnlocks {
                        slots: indices[from..to].to_vec(),
                        shares: shares[from..to].to_vec(),
                        keys: keys[from..to].to_vec(),
                        paths: paths[from * riverproof::mental::MERKLE_DEPTH
                            ..to * riverproof::mental::MERKLE_DEPTH]
                            .to_vec(),
                        round_digest,
                    }
                    .data(),
                )
                .expect("hole unlocks"),
            );
        }
    }

    (session, deal, slots)
}

/// Who signs for each seat: its own wallet, or a session key authorized for it.
///
/// A hand is 206 transactions at a full table. Whether those are 206 wallet
/// prompts or one is the whole difference between this being playable and not,
/// so both are driven through the same code and only this differs.
struct Signing<'a> {
    /// Indexed by **player**, not by seat.
    ///
    /// These used to be looked up through `deal.seats`, turning a player index
    /// into a seat and then using the seat to index these — which is the same
    /// number only when the table is filled from seat zero. Every test here
    /// does exactly that, so the confusion was invisible until a table seated
    /// people at 0, 4 and 5 and the lookup ran off the end.
    keys: &'a [Keypair],
    sessions: Option<&'a [Pubkey]>,
}

impl Signing<'_> {
    fn key(&self, player: usize) -> &Keypair {
        &self.keys[player]
    }

    fn session(&self, player: usize) -> Option<Pubkey> {
        self.sessions.map(|sessions| sessions[player])
    }

    fn len(&self) -> usize {
        self.keys.len()
    }
}

fn mental_send(
    harness: &mut Harness,
    wallet: &Keypair,
    session: Option<Pubkey>,
    config: &Pubkey,
    data: Vec<u8>,
) -> Result<(), String> {
    mental_send_metered(harness, wallet, session, config, data).map(|_| ())
}

fn mental_send_metered(
    harness: &mut Harness,
    wallet: &Keypair,
    session: Option<Pubkey>,
    config: &Pubkey,
    data: Vec<u8>,
) -> Result<(u64, usize), String> {
    harness.send_metered(
        Instruction {
            program_id: program_id(),
            accounts: riverproof::accounts::MentalPlay {
                signer: wallet.pubkey(),
                session,
                config: *config,
                table_state: table_state_pda(config),
                hand_state: hand_pda(config),
                hand_cards: hand_cards_pda(config),
                deal: mental_deal_pda(config),
            }
            .to_account_metas(None),
            data,
        },
        &[wallet],
    )
}

/// A whole hand: shuffled by the players, opened and settled by the program.
///
/// This is the test the SVM harness exists for, and the one that could not be
/// written while Arcium was the only confidential deal — its cards arrive
/// through cluster callbacks that an in-process runtime has nobody to run. A
/// player-shuffled hand needs no third party, so every step is an instruction
/// the test sends itself.
///
/// The whole deck is known to the test and to nobody else. It is the oracle the
/// result is checked against, never an input: the program is told a digest per
/// round, a locked deck, and a scalar per card per player, and works the rest
/// out.
/// Plays the hand out with everybody checking and calling, opening each street
/// and each contender's cards as the program asks for them.
///
/// Returns the same hand played locally against the same deck, so the caller can
/// compare. Shared by the correctness test and the cost test.
fn play_to_showdown(
    harness: &mut Harness,
    signing: &Signing,
    config: &Pubkey,
    session: &Session,
    deal: &MentalDeal,
    slots: &[riverproof::state::DealSlot],
    meter: &mut Meter,
) -> engine::Table {
    let players = signing.len();
    let deck = session.deck().expect("the deck opens");
    let player_of = |seat: u8| deal.seats.iter().position(|s| *s == seat).unwrap();

    // The same hand, played locally against the deck the players produced.
    let mut expected = engine::Table::new(engine::TableConfig {
        small_blind: SMALL_BLIND,
        big_blind: SMALL_BLIND * 2,
        ante: ANTE,
        max_players: players as u8,
        buy_in: BUY_IN,
        action_time_ms: ACTION_TIME_MS,
    });
    for seat in 0..players {
        expected.seats[seat] = engine::Seat {
            occupied: true,
            stack: BUY_IN,
            ready: true,
            connected: true,
            leave_after_hand: false,
        };
    }
    let now_ms = harness.now * 1_000;
    let mut ordering = [0u8; 52];
    ordering.copy_from_slice(&deck);
    expected.start_hand(ordering, now_ms).expect("engine start_hand");

    // Everyone checks and calls it down, so the hand reaches a showdown and
    // every reveal path gets exercised.
    let mut guard = 0;
    loop {
        guard += 1;
        assert!(guard < 128, "the hand did not finish");

        let hand: HandState = harness.account(&hand_pda(config));
        if hand.stage == HandStage::Complete {
            break;
        }

        // Parked for a street: every player takes a layer off the cards that
        // street needs, and only those. A board card is not due before its own
        // street, which is what stops the whole board being public preflop.
        if hand.awaiting != 0 && hand.awaiting != 5 {
            let dealt = hand.board_len as usize;
            let pending = match hand.awaiting {
                1 => 3,
                4 => 5 - dealt,
                _ => 1,
            };
            let indices: Vec<u8> = (dealt..dealt + pending)
                .map(|at| (players * 2 + at) as u8)
                .collect();
            for player in 0..players {
                let shares: Vec<[u8; 32]> = indices
                    .iter()
                    .map(|index| {
                        let position = slots[*index as usize].position;
                        encode_scalar(
                            &session.participants[player].unlock(position).expect("a share"),
                        )
                    })
                    .collect();
                meter.record(
                    "publish_unlocks (board)",
                    mental_send_metered(
                        harness,
                        signing.key(player),
                        signing.session(player),
                        config,
                        riverproof::instruction::PublishUnlocks {
                            keys: Vec::new(),
                            paths: Vec::new(),
                            round_digest: [0u8; 32],
                            slots: indices.clone(),
                            shares,
                        }
                        .data(),
                    )
                    .expect("board unlocks"),
                );
            }
            meter.record(
                "reveal_mental_street",
                mental_send_metered(
                    harness,
                    signing.key(0),
                    signing.session(0),
                    config,
                    riverproof::instruction::RevealMentalStreet {}.data(),
                )
                .expect("reveal_mental_street"),
            );
            continue;
        }

        // Showdown: each contender publishes the one share only they hold.
        if hand.awaiting == 5 {
            let contenders = hand.active_seats & !hand.folded;
            for seat in 0..players as u8 {
                if contenders & (1 << seat) == 0 {
                    continue;
                }
                let player = player_of(seat);
                let mut indices = Vec::new();
                let mut shares = Vec::new();
                for (index, slot) in slots.iter().enumerate().take(players * 2) {
                    if slot.owner != seat {
                        continue;
                    }
                    indices.push(index as u8);
                    shares.push(encode_scalar(
                        &session.participants[player].unlock(slot.position).expect("a share"),
                    ));
                }
                meter.record(
                    "publish_unlocks (showdown)",
                    mental_send_metered(
                        harness,
                        signing.key(player),
                        signing.session(player),
                        config,
                        riverproof::instruction::PublishUnlocks { slots: indices, shares, keys: Vec::new(), paths: Vec::new(), round_digest: [0u8; 32] }.data(),
                    )
                    .expect("own hole unlocks"),
                );
                meter.record(
                    "reveal_mental_hole",
                    mental_send_metered(
                        harness,
                        signing.key(player),
                        signing.session(player),
                        config,
                        riverproof::instruction::RevealMentalHole { seat }.data(),
                    )
                    .expect("reveal_mental_hole"),
                );
            }
            continue;
        }

        let actor = hand.actor_seat;
        assert_ne!(actor, u8::MAX, "nobody is on the clock and nothing is pending");
        assert_eq!(
            Some(actor),
            expected.hand.as_ref().unwrap().actor_seat,
            "the program and the engine disagree about whose turn it is",
        );

        let legal = expected.legal_actions(actor).expect("the engine offers actions");
        let action = if legal.can_check {
            riverproof::instructions::PokerAction::Check
        } else {
            riverproof::instructions::PokerAction::Call
        };
        meter.record(
            "act",
            {
                let hand_state: HandState = harness.account(&hand_pda(config));
                harness
                    .send_metered(
                        Instruction {
                            program_id: program_id(),
                            accounts: play_accounts(
                                &signing.key(player_of(actor)).pubkey(),
                                signing.session(player_of(actor)),
                                config,
                            )
                            .to_account_metas(None),
                            data: riverproof::instruction::Act {
                                action,
                                expected_hand_id: hand_state.hand_id,
                                expected_version: hand_state.version,
                            }
                            .data(),
                        },
                        &[signing.key(player_of(actor))],
                    )
                    .expect("act")
            },
        );
        expected
            .apply_action(actor, engine_action(action), now_ms, None)
            .expect("engine action");
    }

    expected
}

#[test]
fn a_player_shuffled_hand_matches_the_engine_and_conserves_chips() {
    let (mut harness, wallets, config) = three_handed();
    let mut meter = Meter::default();
    let signing = Signing { keys: &wallets, sessions: None };
    let (session, deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    assert_eq!(deal.threshold, 2, "three players, all but one");
    let deck = session.deck().expect("the deck opens");

    // Out of turn is refused, which is the whole ordering rule: each round masks
    // the deck the one before it produced.
    let out_of_turn = mental_send(
        &mut harness,
        &wallets[deal.seats[1] as usize],
        None,
        &config,
        riverproof::instruction::CommitDealRound {
            round: DealRound::Lock,
            digest: [7u8; 32],
        }
        .data(),
    );
    assert!(out_of_turn.is_err(), "a fourth round was accepted from three players");

    // The claim, checked on chain rather than argued: with every other player's
    // layer off, a hole card is still shut. Two of three shares in, no card.
    let after_deal: MentalDeal = harness.account(&mental_deal_pda(&config));
    for (index, slot) in after_deal.slots[..6].iter().enumerate() {
        assert_eq!(slot.unlocked.count_ones(), 2, "hole slot {index}: wrong layer count");
        assert_eq!(slot.card, 0xff, "hole slot {index} opened without its owner");
    }
    // And its owner, holding one scalar nobody else has, can read it.
    for player in 0..3usize {
        let seat = deal.seats[player];
        for slot in slots.iter().take(6).filter(|slot| slot.owner == seat) {
            assert_eq!(
                session.open_hole(player as u8, slot.position).expect("the owner opens it"),
                deck[slot.position as usize],
                "seat {seat} could not read its own card",
            );
        }
    }

    let expected = play_to_showdown(
        &mut harness, &signing, &config, &session, &deal, &slots, &mut meter,
    );

    // The board the program opened is the board the deck says, card for card.
    let on_chain: HandState = harness.account(&hand_pda(&config));
    let engine_hand = expected.hand.as_ref().unwrap();
    assert_eq!(&on_chain.board[..5], engine_hand.board(), "the board diverged");
    assert!(on_chain.settled);

    // Every dealt card on chain is the card the players shuffled to that spot.
    let opened: MentalDeal = harness.account(&mental_deal_pda(&config));
    for (index, slot) in opened.slots[..opened.slot_count as usize].iter().enumerate() {
        assert_eq!(
            slot.card, deck[slot.position as usize],
            "slot {index} opened as a card nobody dealt",
        );
    }

    // And the settlement matches the engine, with the chips still conserved.
    let state: TableState = harness.account(&table_state_pda(&config));
    for seat in 0..3 {
        assert_eq!(
            state.seats[seat].stack, expected.seats[seat].stack,
            "seat {seat} stack disagrees with the engine",
        );
    }
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(stacks, 3 * BUY_IN);
    assert_eq!(state.escrowed, 3 * BUY_IN);
    assert_eq!(harness.vault_balance(&config), 3 * BUY_IN);

    capture_settled_accounts(&harness, &config);
}

/// Writes the account bytes a settled hand left behind, for the client tests.
///
/// `tests/fixtures/settled-hand-accounts.json` is what `tests/chain-decode`,
/// `chain-view` and `chain-session` decode, and the point of it is that the
/// bytes are real rather than synthetic — a decoder checked against bytes it
/// invented only proves it agrees with itself.
///
/// It used to be captured from a live Arcium cluster, which made regenerating
/// it a deployment. It comes from here now: the same compiled program, the same
/// instructions, a whole hand played and settled in-process. No cluster, no
/// network, no SOL, and deterministic.
///
/// Off by default, because rewriting a fixture is a decision. Regenerate with:
///
/// ```text
/// UPDATE_SETTLED_ACCOUNTS=1 cargo test -p riverproof-integration a_player_shuffled_hand
/// ```
fn capture_settled_accounts(harness: &Harness, config: &Pubkey) {
    if std::env::var("UPDATE_SETTLED_ACCOUNTS").is_err() {
        return;
    }
    use base64::Engine as _;

    let encode = |address: Pubkey| {
        let raw = harness.svm.get_account(&address).expect("account exists");
        base64::engine::general_purpose::STANDARD.encode(raw.data)
    };
    let fixture = serde_json::json!({
        "TableConfig": encode(*config),
        "TableState": encode(table_state_pda(config)),
        "HandState": encode(hand_pda(config)),
        "HandCards": encode(hand_cards_pda(config)),
        "MentalDeal": encode(mental_deal_pda(config)),
    });

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/settled-hand-accounts.json");
    std::fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&fixture).expect("fixture serializes")),
    )
    .expect("fixture is writable");
}

fn engine_action(action: riverproof::instructions::PokerAction) -> engine::Action {
    action.into()
}

#[test]
fn a_session_key_may_act_but_never_move_chips() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);
    let (alice, bob) = (wallets[0].insecure_clone(), wallets[1].insecure_clone());

    let session = Keypair::new();
    harness.svm.airdrop(&session.pubkey(), 1_000_000_000).unwrap();
    let expiry = harness.now + 3_600;

    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::AuthorizeSession {
                    wallet: alice.pubkey(),
                    config,
                    session: session_pda(&alice.pubkey(), &config),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::AuthorizeSession {
                    session_key: session.pubkey(),
                    expiry,
                }
                .data(),
            },
            &[&alice],
        )
        .expect("authorize_session");

    // Alice acts through the session key without touching her wallet.
    act(
        &mut harness,
        &session,
        Some(session_pda(&alice.pubkey(), &config)),
        &config,
        riverproof::instructions::PokerAction::Call,
    )
    .expect("session key acts");

    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.actor_seat, 1, "action should pass to Bob");

    // The session key cannot cash Alice out: leave_table requires the wallet.
    let accounts = seat_accounts(&harness, &session.pubkey(), &config);
    let failure = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: accounts.to_account_metas(None),
                data: riverproof::instruction::LeaveTable {}.data(),
            },
            &[&session],
        )
        .unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");

    let _ = bob;
}

#[test]
fn an_expired_session_key_is_refused() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);
    let alice = wallets[0].insecure_clone();

    let session = Keypair::new();
    harness.svm.airdrop(&session.pubkey(), 1_000_000_000).unwrap();
    let expiry = harness.now + 30;

    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::AuthorizeSession {
                    wallet: alice.pubkey(),
                    config,
                    session: session_pda(&alice.pubkey(), &config),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::AuthorizeSession {
                    session_key: session.pubkey(),
                    expiry,
                }
                .data(),
            },
            &[&alice],
        )
        .expect("authorize_session");

    harness.advance(60);

    let failure = act(
        &mut harness,
        &session,
        Some(session_pda(&alice.pubkey(), &config)),
        &config,
        riverproof::instructions::PokerAction::Call,
    )
    .unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");
}

#[test]
fn an_unrelated_wallet_cannot_act_for_a_seat() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);
    let alice = wallets[0].insecure_clone();

    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();

    let failure = act(
        &mut harness,
        &stranger,
        None,
        &config,
        riverproof::instructions::PokerAction::Call,
    )
    .unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");
}

#[test]
fn a_stale_version_is_rejected() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);
    let (alice, bob) = (wallets[0].insecure_clone(), wallets[1].insecure_clone());

    let hand: HandState = harness.account(&hand_pda(&config));
    let stale_version = hand.version;

    act(&mut harness, &alice, None, &config, riverproof::instructions::PokerAction::Call)
        .expect("alice calls");

    // Bob replays a transaction built against the pre-call version.
    let failure = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&bob.pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::Act {
                    action: riverproof::instructions::PokerAction::Check,
                    expected_hand_id: hand.hand_id,
                    expected_version: stale_version,
                }
                .data(),
            },
            &[&bob],
        )
        .unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");
}

#[test]
fn the_timeout_crank_is_permissionless_and_respects_the_clock() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);
    let alice = wallets[0].insecure_clone();

    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();

    let crank = |harness: &mut Harness, signer: &Keypair| {
        harness.send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&signer.pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::ForceTimeout {}.data(),
            },
            &[signer],
        )
    };

    // Too early: the deadline has not passed.
    assert!(crank(&mut harness, &stranger).is_err());

    harness.advance(ACTION_TIME_MS / 1_000);

    // Anyone may crank once it has, so a dead relay cannot freeze the table.
    crank(&mut harness, &stranger).expect("stranger cranks the timeout");

    let hand: HandState = harness.account(&hand_pda(&config));
    // Whoever was on the clock is folded or checked through, and recorded as
    // having been skipped. Three-handed the hand carries on without them.
    assert_ne!(hand.timed_out, 0, "the skipped seat was not recorded");

    let state: TableState = harness.account(&table_state_pda(&config));
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    let pot: u64 = if matches!(hand.stage, HandStage::Idle | HandStage::Complete) {
        0
    } else {
        hand.contributions.iter().sum()
    };
    assert_eq!(
        stacks + pot,
        3 * BUY_IN,
        "a timeout must not change the chip supply",
    );

    // And a seat that was skipped stops being dealt in. Crank the rest of the
    // hand out, then check nobody who missed their turn is still ready.
    for _ in 0..12 {
        let hand: HandState = harness.account(&hand_pda(&config));
        if matches!(hand.stage, HandStage::Complete) {
            break;
        }
        harness.advance(ACTION_TIME_MS / 1_000);
        if crank(&mut harness, &stranger).is_err() {
            break;
        }
    }

    let hand: HandState = harness.account(&hand_pda(&config));
    let state: TableState = harness.account(&table_state_pda(&config));
    for seat in 0..3usize {
        if hand.timed_out & (1u16 << seat) != 0 {
            assert!(
                !state.seats[seat].ready,
                "seat {seat} was skipped and is still ready, so it will be dealt in again",
            );
            assert_ne!(
                state.seats[seat].sitting_out_since, 0,
                "seat {seat} was sat out without a clock, so it can never be taken",
            );
        }
    }
}

#[test]
fn table_settings_outside_the_supported_range_are_refused() {
    let mut harness = Harness::new();
    let alice = harness.player("alice");
    let mint = harness.mint;
    let config = table_pda(&alice.pubkey(), TABLE_ID);

    let attempt = |harness: &mut Harness, max_players: u8, buy_in: u64, speed: i64| {
        harness.send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::CreateTable {
                    endpoint_quota: endpoint_quota_pda(RPC_URL),
                    creator: alice.pubkey(),
                    config,
                    table_state: table_state_pda(&config),
                    hand_state: hand_pda(&config),
                    hand_cards: hand_cards_pda(&config),
                    vault: vault_pda(&config),
                    mint,
                    token_program: anchor_spl::token_2022::ID,
                    system_program: anchor_lang::system_program::ID,
                    rent: solana_sdk::sysvar::rent::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::CreateTable {
                    table_id: TABLE_ID,
                    name: "Bad".to_string(),
                    rpc_url: RPC_URL.to_string(),
                    endpoint: endpoint_hash(RPC_URL),
                    // Open, which is what almost every table is.
                    join_key: Pubkey::default(),
                    small_blind: SMALL_BLIND,
                    ante: ANTE,
                    max_players,
                    buy_in,
                    action_time_ms: speed,
                }
                .data(),
            },
            &[&alice],
        )
    };

    // Eleven seats, a buy-in under forty small blinds, and an unlisted speed.
    assert!(attempt(&mut harness, 11, BUY_IN, ACTION_TIME_MS).is_err());
    assert!(attempt(&mut harness, 3, 100, ACTION_TIME_MS).is_err());
    assert!(attempt(&mut harness, 3, BUY_IN, 1_234).is_err());
    for old_timer in [15_000, 25_000, 39_999] {
        assert!(attempt(&mut harness, 3, BUY_IN, old_timer).is_err());
    }

    // Two seats is refused with them, and that is the dealing scheme talking
    // rather than a preference: a player-shuffled hand has no board threshold
    // that is both private and live with two participants, so a two-seat table
    // could be created, joined, and never dealt.
    assert!(attempt(&mut harness, 2, BUY_IN, ACTION_TIME_MS).is_err());
    assert!(attempt(&mut harness, 3, BUY_IN, ACTION_TIME_MS).is_ok());
}

/// The three-seat minimum, enforced by the runtime rather than by a unit test.
///
/// A two-player mental hand has no board threshold that is both private and
/// live: one lets either player open the board early, two lets either stall it
/// forever. The engine refuses it, and this is the check that the refusal
/// survives all the way out through the instruction.
#[test]
fn a_mental_hand_refuses_a_table_of_two() {
    let (mut harness, alice, _bob, config) = two_seated();

    let failure = start_hand_mental(&mut harness, &alice, &config).unwrap_err();
    assert!(failure.contains("Custom"), "{failure}");

    // The table is untouched: no hand started, and no deal account left behind
    // half-initialised for the next attempt to trip over.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Idle);

    // And there is nothing to fall back on. Arcium used to deal exactly this
    // case — it was why both schemes existed — so removing it made the refusal
    // final rather than a routing decision. A table of two waits for a third.
}

fn kick_idle_accounts(
    harness: &Harness,
    cranker: &Pubkey,
    evicted: &Pubkey,
    config: &Pubkey,
) -> riverproof::accounts::KickIdle {
    riverproof::accounts::KickIdle {
        cranker: *cranker,
        config: *config,
        table_state: table_state_pda(config),
        hand_state: hand_pda(config),
        vault: vault_pda(config),
        evicted: *evicted,
        evicted_tokens: get_associated_token_address(evicted, &harness.mint),
        mint: harness.mint,
        token_program: anchor_spl::token_2022::ID,
        associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }
}

fn kick_idle(
    harness: &mut Harness,
    cranker: &Keypair,
    evicted: &Pubkey,
    config: &Pubkey,
    seat_index: u8,
) -> Result<(), String> {
    harness.send(
        Instruction {
            program_id: program_id(),
            accounts: kick_idle_accounts(harness, &cranker.pubkey(), evicted, config)
                .to_account_metas(None),
            data: riverproof::instruction::KickIdle { seat_index }.data(),
        },
        &[cranker],
    )
}

/// A seat somebody is sitting in but not playing from can be freed, and the
/// player who loses it is paid out in full.
///
/// The three-player floor is what makes this necessary: one absent player at a
/// three-seat table is a table that deals nothing at all, and `leave_table`
/// needs the absent player's own signature. Without this the chair could only
/// be freed by its occupant coming back.
#[test]
fn a_seat_left_sitting_out_can_be_freed_and_the_stack_goes_home() {
    let (mut harness, wallets, config) = three_handed();
    let cranker = harness.player("dave");

    // Carol sits out. Everyone starts ready in `three_handed`, so this is the
    // transition that starts her clock.
    set_ready(&mut harness, &wallets[2], &config, false);
    let carol = wallets[2].pubkey();
    let before = harness.token_balance(&carol);
    // The cranker starts with whatever the faucet gave them; what matters is
    // that freeing a seat does not add to it.
    let cranker_before = harness.token_balance(&cranker.pubkey());

    // Too early: the grace period is what separates stepping away from leaving.
    assert!(
        kick_idle(&mut harness, &cranker, &carol, &config, 2).is_err(),
        "a seat was freed before the grace period elapsed",
    );

    harness.advance(120);
    assert!(kick_idle(&mut harness, &cranker, &carol, &config, 2)
        .unwrap_err().contains("TimerNotExpired"), "the old two-minute boundary must stay protected");
    harness.advance(179);
    assert!(kick_idle(&mut harness, &cranker, &carol, &config, 2)
        .unwrap_err().contains("TimerNotExpired"), "299 seconds is still inside the grace period");
    harness.advance(1);
    kick_idle(&mut harness, &cranker, &carol, &config, 2).expect("the seat is freed");

    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(!state.seats[2].occupied, "the seat was not emptied");
    assert_eq!(state.seats[2].stack, 0);
    assert_eq!(state.seats[2].owner, Pubkey::default(), "the seat kept its old owner");

    // Carol's chips went to Carol, not to whoever kicked her. That is the whole
    // safety argument for an instruction that moves another wallet's money
    // without their signature.
    assert_eq!(
        harness.token_balance(&carol),
        before + BUY_IN,
        "the evicted stack did not go back to its owner",
    );
    // And the cranker was not paid for their trouble.
    assert_eq!(
        harness.token_balance(&cranker.pubkey()),
        cranker_before,
        "the cranker was paid for the eviction",
    );

    // The vault still backs exactly what the seats hold.
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(stacks, state.escrowed, "the vault and the stacks disagree");
    assert_eq!(state.escrowed, 2 * BUY_IN, "the freed stack stayed escrowed");

    // The chair is now anybody's, through the ordinary front door.
    // Ordinary front door, no special case: the chair is simply empty now.
    join(&mut harness, &cranker, &config, 2);
    let state: TableState = harness.account(&table_state_pda(&config));
    assert_eq!(state.seats[2].owner, cranker.pubkey());
    assert_eq!(state.escrowed, 3 * BUY_IN);
}

/// The players still at the table are the ones who can free the seat.
///
/// This is the reason the instruction is permissionless rather than a takeover.
/// An earlier version seated whoever sent it, which required them to be
/// *unseated* — so a three-handed table with one player away could only be
/// unstuck by a stranger wandering past, and the two people actually blocked by
/// it had no lever at all.
#[test]
fn a_seated_player_can_free_an_idle_seat() {
    let (mut harness, wallets, config) = three_handed();
    set_ready(&mut harness, &wallets[2], &config, false);
    harness.advance(301);

    let carol = wallets[2].pubkey();
    // Alice is seated, and that is exactly who needs this to work.
    kick_idle(&mut harness, &wallets[0], &carol, &config, 2).expect("a seated player may kick");

    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(!state.seats[2].occupied, "a seated player could not free the seat");
    assert!(state.seats[0].occupied, "the kicker lost their own seat");
    assert_eq!(state.seats[0].owner, wallets[0].pubkey());
}

/// A player who is ready keeps their seat, however long they have been there.
#[test]
fn a_ready_player_cannot_be_evicted() {
    let (mut harness, wallets, config) = three_handed();
    let newcomer = harness.player("dave");
    let carol = wallets[2].pubkey();

    harness.advance(600);
    assert!(
        kick_idle(&mut harness, &newcomer, &carol, &config, 2).is_err(),
        "a ready player was evicted from their seat",
    );

    // And sitting out only starts the clock; it does not open the seat at once.
    set_ready(&mut harness, &wallets[2], &config, false);
    assert!(
        kick_idle(&mut harness, &newcomer, &carol, &config, 2).is_err(),
        "the grace period did not restart when the player sat out",
    );
}

/// A seat in a live hand is never takeable, or the pot would be owed to a seat
/// that changed hands underneath it.
#[test]
fn a_seat_in_a_live_hand_cannot_be_taken() {
    let (mut harness, wallets, config) = three_handed();
    let newcomer = harness.player("dave");
    hand_in_progress(&mut harness, &wallets, &config);

    let carol = wallets[2].pubkey();
    harness.advance(600);
    assert!(
        kick_idle(&mut harness, &newcomer, &carol, &config, 2).is_err(),
        "a seat was taken out from under a live hand",
    );
}

/// The refund is derived from the seat's owner, so an evictor cannot be paid.
#[test]
fn an_evictor_cannot_name_their_own_refund_account() {
    let (mut harness, wallets, config) = three_handed();
    let newcomer = harness.player("dave");
    set_ready(&mut harness, &wallets[2], &config, false);
    harness.advance(301);

    // Naming somebody other than the seat's owner is refused outright, which is
    // what stops the refund being redirected: `evicted_tokens` is derived from
    // this account, so controlling it would mean controlling where chips land.
    let impostor = newcomer.pubkey();
    assert!(
        kick_idle(&mut harness, &newcomer, &impostor, &config, 2).is_err(),
        "the evicted wallet was not checked against the seat",
    );
}

/// What a hand costs, measured rather than estimated.
///
/// Two ceilings decide whether this design is shippable, and neither is
/// negotiable. A transaction is 1232 bytes on the wire. An instruction gets
/// 200,000 compute units by default, and 1.4M is the most a transaction can ask
/// for. Both of the design's largest messages scale with the number of players
/// — the sealed deck is one point per dealt position, and a player's deal-time
/// unlock is one scalar per other player's hole card — so the question is
/// whether a full table still fits.
///
/// The numbers are printed as well as asserted. `cargo test -p
/// riverproof-integration -- --nocapture a_mental_deal` shows the table.
#[test]
fn a_mental_deal_fits_in_a_transaction_and_a_compute_budget() {
    /// Solana's packet limit. Not a target to approach — a wall.
    const TRANSACTION_LIMIT: usize = 1232;
    /// What an instruction gets without asking.
    const DEFAULT_COMPUTE_LIMIT: u64 = 200_000;
    /// What `MENTAL_COMPUTE_UNITS` in `app/chain/mental-deal.ts` asks for.
    const CLIENT_REQUESTED_COMPUTE_UNITS: u64 = 300_000;

    let mut violations: Vec<String> = Vec::new();
    let mut needs_budget: Vec<String> = Vec::new();
    for players in [3usize, 9, 10] {
        let (mut harness, wallets, config) = seated(players);
        let mut meter = Meter::default();

        let signing = Signing { keys: &wallets, sessions: None };
        let (session, deal, slots) =
            deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);
        let expected =
            play_to_showdown(&mut harness, &signing, &config, &session, &deal, &slots, &mut meter);

        // The hand really finished, or the measurements are of half a hand.
        let hand: HandState = harness.account(&hand_pda(&config));
        assert_eq!(hand.stage, HandStage::Complete);
        assert_eq!(&hand.board[..5], expected.hand.as_ref().unwrap().board());

        println!("\n{players} players - {} transactions for one hand", meter.rows.len());
        for label in meter.labels() {
            let (units, bytes) = meter.worst(label);
            println!("  {label:<26} {units:>7} CU  {bytes:>5} bytes");
        }

        for label in meter.labels() {
            let (units, bytes) = meter.worst(label);
            if bytes > TRANSACTION_LIMIT {
                violations.push(format!(
                    "{players} players: {label} is {bytes} bytes, over the \
                     {TRANSACTION_LIMIT}-byte transaction limit",
                ));
            }
            if units > u64::from(MAX_COMPUTE_UNITS) {
                violations.push(format!(
                    "{players} players: {label} burns {units} CU, over the {MAX_COMPUTE_UNITS} \
                     a transaction may ask for",
                ));
            }
            // Under the default budget an instruction needs no request at all.
            // Over it, the client must send one - so this is recorded rather
            // than failed, and `docs/DEALING.md` carries the number.
            if units >= DEFAULT_COMPUTE_LIMIT {
                needs_budget.push(format!("{players} players: {label} at {units} CU"));
                // And what the client asks for has to cover it. This is the
                // other half of `MENTAL_COMPUTE_UNITS` in
                // `app/chain/mental-deal.ts`; raising one means raising both.
                if units > CLIENT_REQUESTED_COMPUTE_UNITS {
                    violations.push(format!(
                        "{players} players: {label} burns {units} CU, more than the \
                         {CLIENT_REQUESTED_COMPUTE_UNITS} the client requests in \
                         MENTAL_COMPUTE_UNITS - raise both",
                    ));
                }
            }
        }
    }

    assert!(violations.is_empty(), "measured limits exceeded:\n  {}", violations.join("\n  "));
    println!("\nover the default budget, so the client must request more:");
    for row in &needs_budget {
        println!("  {row}");
    }
}

/// Authorizes an ephemeral key to act for `wallet` at this table.
fn authorize_session(
    harness: &mut Harness,
    wallet: &Keypair,
    config: &Pubkey,
    session_key: &Keypair,
    expiry: i64,
) -> Pubkey {
    harness.svm.airdrop(&session_key.pubkey(), 1_000_000_000).unwrap();
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::AuthorizeSession {
                    wallet: wallet.pubkey(),
                    config: *config,
                    session: session_pda(&wallet.pubkey(), config),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::AuthorizeSession {
                    session_key: session_key.pubkey(),
                    expiry,
                }
                .data(),
            },
            &[wallet],
        )
        .expect("authorize_session");
    session_pda(&wallet.pubkey(), config)
}

/// One prompt, not two hundred and six.
///
/// This is the test the whole session-key mechanism exists for. A hand at a full
/// table is 206 transactions, and a player who had to approve each one in their
/// wallet would not finish a single hand. So the claim is that after one
/// signature authorizing an ephemeral key, that key can drive *everything* — the
/// shuffle and lock commitments, sealing the deck, every unlock share, the
/// street reveals, the showdown — and the wallet never signs again.
///
/// And the other half, which is what makes it safe to hand out: the same key
/// cannot move a single chip.
#[test]
fn a_session_key_can_deal_a_whole_hand_but_still_cannot_move_chips() {
    let (mut harness, wallets, config) = seated(3);

    // One wallet signature each. After this the wallets are done.
    let session_keys: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let expiry = harness.now + 3_600;
    let sessions: Vec<Pubkey> = wallets
        .iter()
        .zip(&session_keys)
        .map(|(wallet, key)| authorize_session(&mut harness, wallet, &config, key, expiry))
        .collect();

    // Everything from here is signed by the ephemeral keys alone.
    let signing = Signing { keys: &session_keys, sessions: Some(&sessions) };
    let mut meter = Meter::default();
    let (session, deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);
    let expected =
        play_to_showdown(&mut harness, &signing, &config, &session, &deal, &slots, &mut meter);

    // The hand really played: settled, board opened, stacks agreeing with the
    // engine, chips conserved.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete);
    assert!(hand.settled);
    assert_eq!(&hand.board[..5], expected.hand.as_ref().unwrap().board());

    let deck = session.deck().expect("the deck opens");
    let opened: MentalDeal = harness.account(&mental_deal_pda(&config));
    for slot in opened.slots[..opened.slot_count as usize].iter() {
        assert_eq!(slot.card, deck[slot.position as usize], "a card opened wrongly");
    }

    let state: TableState = harness.account(&table_state_pda(&config));
    for seat in 0..3 {
        assert_eq!(state.seats[seat].stack, expected.seats[seat].stack, "seat {seat} stack");
    }
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(stacks, 3 * BUY_IN);

    // Every one of those transactions was signed by an ephemeral key, and the
    // wallets signed once each. That ratio is the point.
    assert!(meter.rows.len() >= 40, "the hand was shorter than a hand");

    // The other half of the claim. A session key deals hands; it does not touch
    // money. `leave_table` cashes a stack out and requires the real wallet.
    let accounts = seat_accounts(&harness, &session_keys[0].pubkey(), &config);
    let failure = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: accounts.to_account_metas(None),
                data: riverproof::instruction::LeaveTable {}.data(),
            },
            &[&session_keys[0]],
        )
        .unwrap_err();
    assert!(failure.contains("Custom"), "a session key cashed out a stack: {failure}");
}

/// An expired key stops dealing, not just betting.
#[test]
fn an_expired_session_key_cannot_deal() {
    let (mut harness, wallets, config) = seated(3);
    let session_key = Keypair::new();
    let expiry = harness.now + 60;
    let session = authorize_session(&mut harness, &wallets[0], &config, &session_key, expiry);

    start_hand_mental(&mut harness, &wallets[0], &config).expect("start_hand_mental");

    // Fine while it lives.
    mental_send(
        &mut harness,
        &session_key,
        Some(session),
        &config,
        riverproof::instruction::CommitDealRound {
            round: DealRound::Shuffle,
            digest: [1u8; 32],
        }
        .data(),
    )
    .expect("a live session key commits a round");

    harness.advance(120);

    // Dead afterwards, on a deal instruction rather than only on `act`.
    let failure = mental_send(
        &mut harness,
        &session_key,
        Some(session),
        &config,
        riverproof::instruction::CommitDealRound {
            round: DealRound::Shuffle,
            digest: [2u8; 32],
        }
        .data(),
    )
    .unwrap_err();
    assert!(failure.contains("Custom"), "an expired key kept dealing: {failure}");
}

/// The positive test above would pass for the wrong reason if the program
/// simply accepted any signer, so this pins the other side: an ephemeral key
/// nobody authorized deals nothing.
#[test]
fn an_unauthorized_key_cannot_deal() {
    let (mut harness, wallets, config) = seated(3);
    start_hand_mental(&mut harness, &wallets[0], &config).expect("start_hand_mental");

    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();

    let round = || riverproof::instruction::CommitDealRound {
        round: DealRound::Shuffle,
        digest: [3u8; 32],
    }
    .data();

    // No session at all: the signer owns no seat.
    let failure = mental_send(&mut harness, &stranger, None, &config, round()).unwrap_err();
    assert!(failure.contains("Custom"), "a stranger committed a round: {failure}");

    // Nor by borrowing a seated player's session account: the program checks
    // that the signer is the key that session names, not merely that some
    // session exists.
    let session_key = Keypair::new();
    let expiry = harness.now + 3_600;
    let session = authorize_session(&mut harness, &wallets[0], &config, &session_key, expiry);
    let failure =
        mental_send(&mut harness, &stranger, Some(session), &config, round()).unwrap_err();
    assert!(failure.contains("Custom"), "a stranger borrowed a session: {failure}");

    // The real holder of that session commits fine, so the refusals above are
    // about authority rather than about the round being malformed.
    mental_send(&mut harness, &session_key, Some(session), &config, round())
        .expect("the authorized key commits");
}

fn abandon(harness: &mut Harness, wallet: &Keypair, config: &Pubkey) -> Result<(), String> {
    mental_send(
        harness,
        wallet,
        None,
        config,
        riverproof::instruction::AbandonMentalDeal {}.data(),
    )
}

/// A street cannot be voided the instant it parks.
///
/// `deal.deadline` runs unattended through a betting round, and a betting round
/// has no bound — a full raise resets `acted`, so every seat acts again. So the
/// clock could already be expired when the next street parked, and the player
/// who closed the round could void the hand in the same transaction, before
/// anybody had a slot in which to publish a share. A losing player got their
/// whole contribution back, for free, at a moment of their own choosing.
#[test]
fn a_street_cannot_be_voided_the_moment_it_parks() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    let before: TableState = harness.account(&table_state_pda(&config));
    let posted: u64 = before.seats.iter().map(|seat| seat.stack).sum();
    assert!(posted < 3 * BUY_IN, "no chips at stake, so nothing is proven");

    // Push the deal clock into the past, the way a long betting round does.
    harness.advance(600);

    // Close the betting round so the flop parks, then try to void it at once.
    // Everyone checks; the last check is what parks the street.
    for _ in 0..3 {
        let hand: HandState = harness.account(&hand_pda(&config));
        if hand.awaiting != 0 {
            break;
        }
        let actor = hand.actor_seat as usize;
        // Preflop the blind is a live bet, so the first seats must call.
        if act(
            &mut harness,
            &wallets[actor],
            None,
            &config,
            riverproof::instructions::PokerAction::Check,
        )
        .is_err()
        {
            act(
                &mut harness,
                &wallets[actor],
                None,
                &config,
                riverproof::instructions::PokerAction::Call,
            )
            .expect("call");
        }
    }

    let parked: HandState = harness.account(&hand_pda(&config));
    assert_ne!(parked.awaiting, 0, "the flop should be parked and awaiting cards");

    // Snapshot after the betting, so this measures the abandon and nothing else.
    let at_park: TableState = harness.account(&table_state_pda(&config));
    let staked: u64 = at_park.seats.iter().map(|seat| seat.stack).sum();

    // Nobody has published a single share for this street yet.
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let _ = abandon(&mut harness, &stranger, &config);

    let after: HandState = harness.account(&hand_pda(&config));
    assert_ne!(
        after.stage,
        HandStage::Complete,
        "a street was voided the instant it parked, before anyone could publish",
    );
    let state: TableState = harness.account(&table_state_pda(&config));
    let now: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(now, staked, "chips were refunded out of a live hand");
}

/// Progress can only ever buy a deal more time.
///
/// `made_progress` used to assign the deadline rather than extend it, and
/// `slot_is_due` waves through any hole slot — so a player who had folded, and
/// still held their own two hole shares unspent, could publish one in the
/// middle of a betting round and rewrite that round's allowance down to sixty
/// seconds. The next street then parked with the clock already expired.
#[test]
fn publishing_a_hole_share_cannot_shorten_a_betting_round() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    let (_session, deal_view, _slots) =
        deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.awaiting, 0, "betting should be open");

    // The allowance betting reopened with.
    let before: MentalDeal = harness.account(&mental_deal_pda(&config));
    let long = before.deadline;

    // Player 0 spends one of their own hole shares. It is theirs to spend, and
    // for a folded player it costs nothing at all.
    let mut indices: Vec<u8> = Vec::new();
    let mut shares: Vec<[u8; 32]> = Vec::new();
    for (index, slot) in deal_view.slots.iter().take(deal_view.slot_count as usize).enumerate() {
        if slot.owner != deal_view.seats[0] {
            continue;
        }
        indices.push(index as u8);
        shares.push(encode_scalar(
            &_session.participants[0].unlock(slot.position).expect("a share"),
        ));
    }
    assert!(!indices.is_empty(), "player 0 holds hole slots");

    mental_send(
        &mut harness,
        &wallets[0],
        None,
        &config,
        riverproof::instruction::PublishUnlocks { slots: indices, shares, keys: Vec::new(), paths: Vec::new(), round_digest: [0u8; 32] }.data(),
    )
    .expect("a player may always spend their own hole share");

    let after: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert!(
        after.deadline >= long,
        "publishing pulled the deal deadline back from {long} to {}",
        after.deadline,
    );
}

/// A betting round is not a stalled deal.
///
/// `deal.deadline` is refreshed only by the deal's own steps — betting cannot
/// touch it, because the `Play` context carries no deal account. So a round
/// that ran longer than `DEAL_STALL_MS` left the clock expired while the table
/// was merely thinking, and any seated player, including one who was losing,
/// could call `abandon_mental_deal` and take their contributions back out of a
/// hand they were about to lose.
///
/// Two things stop it now: the deal clock is pushed past a full betting round
/// when betting reopens, and abandoning is refused outright unless the hand is
/// actually waiting on cards.
#[test]
fn a_long_betting_round_cannot_be_voided_as_a_stalled_deal() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    // The deal is done and the table is betting: nothing is owed by the deal.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.awaiting, 0, "the hand should be betting, not waiting on cards");
    let staked: u64 = {
        let state: TableState = harness.account(&table_state_pda(&config));
        state.seats.iter().map(|seat| seat.stack).sum()
    };
    assert!(staked < 3 * BUY_IN, "no chips at stake, so nothing is proven");

    // Longer than DEAL_STALL_MS of ordinary poker thinking. Three players at a
    // 30s timer can legitimately take this long over one round.
    harness.advance(120);

    assert!(
        abandon(&mut harness, &wallets[0], &config).is_err(),
        "a betting round was voided as a stalled deal",
    );

    // The hand is untouched: still live, still holding the same chips.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_ne!(hand.stage, HandStage::Complete, "a live hand was completed");
    let state: TableState = harness.account(&table_state_pda(&config));
    let after: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(after, staked, "chips moved");

    // And the remedy that does apply still does: whoever is on the clock can be
    // skipped, which is what a slow betting round actually needs.
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&wallets[0].pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::ForceTimeout {}.data(),
            },
            &[&wallets[0]],
        )
        .expect("force_timeout is the remedy for a slow bettor");
}

#[path = "scenarios/tournament_takeover.rs"]
mod tournament_takeover;

#[path = "scenarios/away_mode.rs"]
mod away_mode;

#[path = "scenarios/afk_recovery.rs"]
mod afk_recovery;

#[test]
fn upgraded_legacy_table_gives_each_new_actor_forty_seconds() {
    let (mut harness, wallets, config) = seated(3);
    // Reproduce the account bytes already in production, without re-creating
    // its vault or touching its player balances during the upgrade.
    let mut stored: TableConfig = harness.account(&config);
    stored.action_time_ms = 25_000;
    let mut account = harness.svm.get_account(&config).unwrap();
    stored.try_serialize(&mut &mut account.data[..]).unwrap();
    harness.svm.set_account(config, account).unwrap();
    let signing = Signing { keys: &wallets, sessions: None };
    deal_to_hole_cards(&mut harness, &signing, &config, &mut Meter::default());
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.action_deadline, harness.now * 1_000 + ACTION_TIME_MS);
    let timeout = || Instruction {
        program_id: program_id(),
        accounts: play_accounts(&wallets[0].pubkey(), None, &config).to_account_metas(None),
        data: riverproof::instruction::ForceTimeout {}.data(),
    };
    harness.advance(39);
    assert!(harness.send(timeout(), &[&wallets[0]]).is_err());
    harness.advance(1);
    harness.send(timeout(), &[&wallets[0]]).expect("40 seconds permits timeout");
    let after: HandState = harness.account(&hand_pda(&config));
    assert_ne!(after.actor_seat, hand.actor_seat);
    assert_eq!(after.action_deadline, harness.now * 1_000 + ACTION_TIME_MS);
}

/// A table must never be left dead by a deal that stopped.
///
/// This is the one stall with no other remedy. A player who will not bet is
/// skipped by `force_timeout`. But a deal that never finishes has nobody on the
/// clock and never completes, so nothing else on chain can move it — the blinds
/// sit in the pot and the table is finished forever.
#[test]
fn a_deal_that_stops_can_be_abandoned_and_every_chip_comes_back() {
    let (mut harness, wallets, config) = seated(3);
    start_hand_mental(&mut harness, &wallets[0], &config).expect("start_hand_mental");

    // Blinds are posted, so chips really are at stake.
    let during: TableState = harness.account(&table_state_pda(&config));
    let staked: u64 = during.seats.iter().map(|seat| seat.stack).sum();
    assert!(staked < 3 * BUY_IN, "no chips were at stake, so nothing is proven");

    // One player commits a round and then everybody goes quiet.
    mental_send(
        &mut harness,
        &wallets[0],
        None,
        &config,
        riverproof::instruction::CommitDealRound {
            round: DealRound::Shuffle,
            digest: [1u8; 32],
        }
        .data(),
    )
    .expect("one round lands");

    // Not yet: a slow deal is not a stopped one, and every step pushes the
    // deadline out.
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    assert!(abandon(&mut harness, &stranger, &config).is_err(), "abandoned a live deal");

    harness.advance(120);

    // Permissionless, so a table cannot be held hostage by whoever went quiet.
    abandon(&mut harness, &stranger, &config).expect("a stranger unsticks the table");

    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete);
    assert_eq!(hand.winner_count, 0, "a void has no winner");

    let state: TableState = harness.account(&table_state_pda(&config));
    for seat in 0..3 {
        assert_eq!(state.seats[seat].stack, BUY_IN, "seat {seat} was not made whole");
    }
    let stacks: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(stacks, 3 * BUY_IN);
    assert_eq!(state.escrowed, 3 * BUY_IN, "the vault and the stacks disagree");
    assert_eq!(harness.vault_balance(&config), 3 * BUY_IN);

    // And the table plays again, which is the whole point.
    harness.advance(30);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&stranger.pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::StartNextHand {}.data(),
            },
            &[&stranger],
        )
        .expect("the table moves on");
}

/// At a showdown the remedy is poker's, not a refund: whoever will not show has
/// mucked, and the pot goes to whoever did.
///
/// This matters because a showdown is the one moment a player knows they are
/// beaten. If stalling there bought a refund, every losing hand would stall.
#[test]
fn a_contender_who_will_not_show_mucks_and_the_pot_goes_to_whoever_did() {
    let (mut harness, wallets, config) = seated(3);
    let mut meter = Meter::default();
    let signing = Signing { keys: &wallets, sessions: None };
    let (session, deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    // Play to the showdown park, but reveal only the first contender.
    let mut guard = 0;
    let shower = loop {
        guard += 1;
        assert!(guard < 64, "the hand never reached a showdown");
        let hand: HandState = harness.account(&hand_pda(&config));

        if hand.awaiting == 5 {
            let contenders = hand.active_seats & !hand.folded;
            let seat = (0..3u8).find(|seat| contenders & (1 << seat) != 0).expect("a contender");
            let player = deal.seats.iter().position(|s| *s == seat).unwrap();
            let mut indices = Vec::new();
            let mut shares = Vec::new();
            for (index, slot) in slots.iter().enumerate().take(6) {
                if slot.owner != seat {
                    continue;
                }
                indices.push(index as u8);
                shares.push(encode_scalar(
                    &session.participants[player].unlock(slot.position).expect("a share"),
                ));
            }
            mental_send(
                &mut harness,
                &wallets[seat as usize],
                None,
                &config,
                riverproof::instruction::PublishUnlocks { slots: indices, shares, keys: Vec::new(), paths: Vec::new(), round_digest: [0u8; 32] }.data(),
            )
            .expect("the one honest reveal");
            mental_send(
                &mut harness,
                &wallets[seat as usize],
                None,
                &config,
                riverproof::instruction::RevealMentalHole { seat }.data(),
            )
            .expect("reveal_mental_hole");
            break seat;
        }

        if hand.awaiting != 0 {
            let dealt = hand.board_len as usize;
            let pending = match hand.awaiting {
                1 => 3,
                4 => 5 - dealt,
                _ => 1,
            };
            let indices: Vec<u8> = (dealt..dealt + pending).map(|at| (6 + at) as u8).collect();
            for player in 0..3usize {
                let shares: Vec<[u8; 32]> = indices
                    .iter()
                    .map(|index| {
                        encode_scalar(
                            &session.participants[player]
                                .unlock(slots[*index as usize].position)
                                .expect("a share"),
                        )
                    })
                    .collect();
                mental_send(
                    &mut harness,
                    &wallets[deal.seats[player] as usize],
                    None,
                    &config,
                    riverproof::instruction::PublishUnlocks {
                        keys: Vec::new(),
                        paths: Vec::new(),
                        round_digest: [0u8; 32],
                        slots: indices.clone(),
                        shares,
                    }
                    .data(),
                )
                .expect("board unlocks");
            }
            mental_send(
                &mut harness,
                &wallets[0],
                None,
                &config,
                riverproof::instruction::RevealMentalStreet {}.data(),
            )
            .expect("reveal_mental_street");
            continue;
        }

        let actor = hand.actor_seat as usize;
        act(
            &mut harness,
            &wallets[actor],
            None,
            &config,
            riverproof::instructions::PokerAction::Check,
        )
        .or_else(|_| {
            act(
                &mut harness,
                &wallets[actor],
                None,
                &config,
                riverproof::instructions::PokerAction::Call,
            )
        })
        .expect("act");
    };

    // The other two never show. The pot is real and it has to go somewhere.
    let parked: HandState = harness.account(&hand_pda(&config));
    let pot: u64 = parked.contributions.iter().sum();
    assert!(pot > 0);
    let before: TableState = harness.account(&table_state_pda(&config));

    // Past the whole betting allowance, not just DEAL_STALL_MS. The deal clock
    // carries whatever `betting_reopened` budgeted for the last round — one
    // action timer per seat plus the stall grace — because `made_progress` only
    // ever extends it. A showdown nobody settles therefore waits out that
    // budget before anyone may muck it.
    harness.advance(240);
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    abandon(&mut harness, &stranger, &config).expect("the muck settles the hand");

    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete);
    assert_eq!(hand.winner_count, 1, "a mucked showdown still has a winner");
    assert_eq!(hand.winners[0].seat, shower, "the pot went to somebody who did not show");

    let after: TableState = harness.account(&table_state_pda(&config));
    assert_eq!(
        after.seats[shower as usize].stack,
        before.seats[shower as usize].stack + pot,
        "the only player who showed did not take the pot",
    );
    let stacks: u64 = after.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(stacks, 3 * BUY_IN, "mucking changed the chip supply");
    assert_eq!(harness.vault_balance(&config), 3 * BUY_IN);
}

/// Betting opens when the cards are readable, wherever people are sitting.
///
/// `publish_unlocks` decides the deal is finished by checking that every hole
/// slot holds a share from each player who is not its owner. `owner` is a
/// **seat**; `unlocked` is a mask indexed by **player**. Those are the same
/// number only when the table is filled from seat zero upwards, which is what
/// every other test here does — so a table seating people at 0, 4 and 5
/// computed a mask nobody could satisfy, finished the deal, and left the hand
/// parked on `Awaiting::Deal` with every card dealt and no betting ever
/// starting.
#[test]
fn betting_opens_for_players_who_did_not_sit_in_the_first_seats() {
    let seats = [0u8, 4, 5];
    let (mut harness, wallets, config) = seated_at(&seats);

    let session_keys: Vec<Keypair> = (0..3).map(|_| Keypair::new()).collect();
    let expiry = harness.now + 3_600;
    let sessions: Vec<Pubkey> = wallets
        .iter()
        .zip(&session_keys)
        .map(|(wallet, key)| authorize_session(&mut harness, wallet, &config, key, expiry))
        .collect();

    let signing = Signing { keys: &session_keys, sessions: Some(&sessions) };
    let mut meter = Meter::default();

    // `deal_to_hole_cards` starts the hand and then runs the whole deal, so the
    // parked state is checked from inside it — see the assertion on `awaiting`
    // once the hand exists, below.
    let (_session, _deal, _slots) =
        deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    // Every hole card is readable now, so the betting is open — and the first
    // actor is a real seat, not seat zero by coincidence.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.awaiting, 0, "the deal finished, so the hand must not still be waiting");
    assert_ne!(hand.actor_seat, 255, "somebody has to be on the clock once cards are dealt");
    assert!(
        seats.contains(&hand.actor_seat),
        "the actor {} is not one of the seated players {seats:?}",
        hand.actor_seat,
    );
}

/// A street that nobody publishes to can eventually be abandoned.
///
/// The grace above restarts the deal clock when a street has only just parked,
/// so nobody can void it before the first share has had a chance to land. It
/// used to restart that clock on *every* call, with nothing to tell the second
/// from the first — so the one case it could never reach was the case it exists
/// for. A street nobody ever publishes to is every player having closed their
/// tab, and the per-hand seed lives in `sessionStorage`, so they cannot come
/// back and finish it. The pot and every stack behind it stayed in the vault
/// with nothing in the program able to release them: `force_timeout` no-ops
/// with nobody on the clock, `start_next_hand` wants a completed hand, and both
/// `leave_table` and `kick_idle` refuse a live one.
#[test]
fn a_street_nobody_publishes_to_is_abandoned_on_the_second_ask() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    harness.advance(600);
    for _ in 0..3 {
        let hand: HandState = harness.account(&hand_pda(&config));
        if hand.awaiting != 0 {
            break;
        }
        let actor = hand.actor_seat as usize;
        if act(
            &mut harness,
            &wallets[actor],
            None,
            &config,
            riverproof::instructions::PokerAction::Check,
        )
        .is_err()
        {
            act(
                &mut harness,
                &wallets[actor],
                None,
                &config,
                riverproof::instructions::PokerAction::Call,
            )
            .expect("call");
        }
    }

    let parked: HandState = harness.account(&hand_pda(&config));
    assert_ne!(parked.awaiting, 0, "the flop should be parked and awaiting cards");
    let staked: u64 = {
        let state: TableState = harness.account(&table_state_pda(&config));
        state.seats.iter().map(|seat| seat.stack).sum()
    };

    // The first ask is refused and buys the street its one grace.
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let _ = abandon(&mut harness, &stranger, &config);
    let after_first: HandState = harness.account(&hand_pda(&config));
    assert_ne!(
        after_first.stage,
        HandStage::Complete,
        "a street was voided the instant it parked",
    );

    // Sixty seconds later, with still nothing published, it really has stopped.
    harness.advance(90);
    abandon(&mut harness, &stranger, &config).expect("a street nobody publishes to must be abandonable");

    let done: HandState = harness.account(&hand_pda(&config));
    assert_eq!(done.stage, HandStage::Complete, "the stalled hand was not voided");

    // Every chip is back where it started and the vault still balances.
    let state: TableState = harness.account(&table_state_pda(&config));
    let refunded: u64 = state.seats.iter().map(|seat| seat.stack).sum();
    assert_eq!(refunded, 3 * BUY_IN, "the voided hand did not refund every contribution");
    assert_eq!(state.escrowed, refunded, "the vault no longer matches the stacks");
    assert!(refunded > staked, "nothing was at stake, so nothing is proven");
}

/// The seats that stopped answering stop being ready.
///
/// A hand is abandoned because cards stopped arriving, and somebody stopped
/// sending them. Nothing in the abandon path used to notice: every seat stayed
/// ready, so the table dealt again, stalled again, and voided again for ever.
/// `kick_idle` could not break the loop either — it refuses a ready seat, and
/// `set_ready` zeroes the very clock it checks — so an absent player held their
/// chair with no instruction in the program able to take it. The engine has a
/// `!seat.connected` branch meant for this that has never run once, because
/// nothing anywhere sets `connected` false.
#[test]
fn an_abandoned_deal_un_readies_the_seat_that_went_quiet() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();

    // Stall inside the rounds, where exactly one player owes the next one.
    start_hand_mental(&mut harness, &wallets[0], &config).expect("a hand");
    let deal: MentalDeal = harness.account(&mental_deal_pda(&config));
    let owing_player = deal.next_round as usize;
    let owing_seat = deal.seats[owing_player] as usize;
    let _ = (&signing, &mut meter);

    harness.advance(300);
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    abandon(&mut harness, &stranger, &config).expect("a deal that never started must be abandonable");

    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(
        !state.seats[owing_seat].ready,
        "the seat that owed the round is still ready, so the table will stall again",
    );
    assert_ne!(
        state.seats[owing_seat].sitting_out_since, 0,
        "the takeover clock never started, so the seat can never be freed",
    );
    for (index, seat) in state.seats.iter().enumerate().take(3) {
        if index != owing_seat {
            assert!(seat.ready, "seat {index} was un-readied and it had answered");
        }
    }

    // And the chair can now actually be freed, which was the whole problem.
    harness.advance(301);
    let owner = state.seats[owing_seat].owner;
    kick_idle(&mut harness, &wallets[(owing_seat + 1) % 3], &owner, &config, owing_seat as u8)
        .expect("a seat that went quiet must be freeable");
    let freed: TableState = harness.account(&table_state_pda(&config));
    assert!(!freed.seats[owing_seat].occupied, "the seat was not freed");
    assert_eq!(freed.escrowed, freed.seats.iter().map(|s| s.stack).sum::<u64>());
}

/// A seat shows once.
///
/// `reveal_mental_hole` is permissionless by design — a showdown must not rest
/// on the loser's cooperation — and it calls `made_progress`. It also used to
/// accept the same seat twice, because `supply_hole_cards` simply rewrote the
/// cards and re-set a bit that was already set. Together that was a lever
/// anybody could pull, seated or not, to push the abandon deadline out another
/// sixty seconds for one transaction a minute, holding a showdown open for ever
/// and freezing every stack at the table — protecting precisely the player the
/// muck rule exists to fold.
///
/// The setup mirrors `a_contender_who_will_not_show_mucks_and_the_pot_goes_to
/// _whoever_did`: a showdown parked with one contender shown and the rest not,
/// which is the only state in which the lever had anything to hold open.
#[test]
fn a_seat_cannot_be_shown_twice() {
    let (mut harness, wallets, config) = seated(3);
    let mut meter = Meter::default();
    let signing = Signing { keys: &wallets, sessions: None };
    let (session, deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    let mut guard = 0;
    let shower = loop {
        guard += 1;
        assert!(guard < 64, "the hand never reached a showdown");
        let hand: HandState = harness.account(&hand_pda(&config));

        if hand.awaiting == 5 {
            let contenders = hand.active_seats & !hand.folded;
            let seat = (0..3u8).find(|seat| contenders & (1 << seat) != 0).expect("a contender");
            let player = deal.seats.iter().position(|s| *s == seat).unwrap();
            let mut indices = Vec::new();
            let mut shares = Vec::new();
            for (index, slot) in slots.iter().enumerate().take(6) {
                if slot.owner != seat {
                    continue;
                }
                indices.push(index as u8);
                shares.push(encode_scalar(
                    &session.participants[player].unlock(slot.position).expect("a share"),
                ));
            }
            mental_send(
                &mut harness,
                &wallets[seat as usize],
                None,
                &config,
                riverproof::instruction::PublishUnlocks { slots: indices, shares, keys: Vec::new(), paths: Vec::new(), round_digest: [0u8; 32] }.data(),
            )
            .expect("the one honest reveal");
            mental_send(
                &mut harness,
                &wallets[seat as usize],
                None,
                &config,
                riverproof::instruction::RevealMentalHole { seat }.data(),
            )
            .expect("reveal_mental_hole");
            break seat;
        }

        if hand.awaiting != 0 {
            let dealt = hand.board_len as usize;
            let pending = match hand.awaiting {
                1 => 3,
                4 => 5 - dealt,
                _ => 1,
            };
            let indices: Vec<u8> = (dealt..dealt + pending).map(|at| (6 + at) as u8).collect();
            for player in 0..3usize {
                let shares: Vec<[u8; 32]> = indices
                    .iter()
                    .map(|index| {
                        encode_scalar(
                            &session.participants[player]
                                .unlock(slots[*index as usize].position)
                                .expect("a share"),
                        )
                    })
                    .collect();
                mental_send(
                    &mut harness,
                    &wallets[deal.seats[player] as usize],
                    None,
                    &config,
                    riverproof::instruction::PublishUnlocks {
                        keys: Vec::new(),
                        paths: Vec::new(),
                        round_digest: [0u8; 32],
                        slots: indices.clone(),
                        shares,
                    }
                    .data(),
                )
                .expect("a board share");
            }
            let _ = mental_send(
                &mut harness,
                &wallets[0],
                None,
                &config,
                riverproof::instruction::RevealMentalStreet {}.data(),
            );
            continue;
        }

        let actor = hand.actor_seat as usize;
        if act(&mut harness, &wallets[actor], None, &config, riverproof::instructions::PokerAction::Check).is_err() {
            act(&mut harness, &wallets[actor], None, &config, riverproof::instructions::PokerAction::Call).expect("call");
        }
    };

    let parked: HandState = harness.account(&hand_pda(&config));
    assert_eq!(parked.awaiting, 5, "the showdown should still be open");
    assert_ne!(
        parked.active_seats & !parked.folded & !parked.revealed_holes,
        0,
        "every contender has shown, so there is nothing left to hold open",
    );

    let deadline_before = {
        let deal_now: MentalDeal = harness.account(&mental_deal_pda(&config));
        deal_now.deadline
    };

    // Anyone at all, which is the point: the lever needed no seat.
    harness.advance(30);
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let again = mental_send(
        &mut harness,
        &stranger,
        None,
        &config,
        riverproof::instruction::RevealMentalHole { seat: shower }.data(),
    );
    assert!(again.is_err(), "a seat was shown twice");

    let deal_after: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(
        deal_after.deadline, deadline_before,
        "a refused reveal still pushed the abandon deadline out, which is the whole grief",
    );
}

/// Leaving during a hand does not wedge the table, and the leaver gets paid.
///
/// `leave_table` will not pull chips out of a contested pot, so mid-hand it
/// only marks the seat. The engine then vacated that seat outright at the start
/// of the next hand — zeroing the stack along with it. Off chain that is free;
/// on chain the chips are really in escrow, so `bridge::store_table` wrote a
/// stack that no longer matched `escrowed` and `assert_chips_conserved` refused
/// the whole instruction with `VaultUnderfunded`. Nothing was ever lost, but it
/// was refused every time: the table could never start another hand, and the
/// player who asked to leave never left. A real table hit this.
///
/// Four seats, because three ready players are what a mental hand needs and the
/// leaver is not one of them any more.
#[test]
fn leaving_during_a_hand_neither_wedges_the_table_nor_strands_the_leaver() {
    let (mut harness, wallets, config) = seated(4);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    let (session, deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    // Leaving mid-hand marks the seat rather than cashing it out.
    let leaver = 1usize;
    let accounts = seat_accounts(&harness, &wallets[leaver].pubkey(), &config);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: accounts.to_account_metas(None),
                data: riverproof::instruction::LeaveTable {}.data(),
            },
            &[&wallets[leaver]],
        )
        .expect("leave_table during a hand");

    let marked: TableState = harness.account(&table_state_pda(&config));
    assert!(marked.seats[leaver].occupied, "the seat was cashed out of a live hand");
    assert!(marked.seats[leaver].leave_after_hand, "the departure was not recorded");
    let owed = marked.seats[leaver].stack;
    assert!(owed > 0, "the leaver has no chips, so the wedge cannot be reproduced");

    play_to_showdown(&mut harness, &signing, &config, &session, &deal, &slots, &mut meter);

    // The crank that used to fail for ever.
    harness.advance(30);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&wallets[0].pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::StartNextHand {}.data(),
            },
            &[&wallets[0]],
        )
        .expect("the table must be able to start another hand");

    let after: TableState = harness.account(&table_state_pda(&config));
    assert!(after.seats[leaver].occupied, "the seat was vacated with its chips still in escrow");
    assert!(!after.seats[leaver].ready, "a departing seat is still being dealt in");
    assert_eq!(
        after.escrowed,
        after.seats.iter().map(|seat| seat.stack).sum::<u64>(),
        "the vault no longer matches the stacks",
    );

    // And the departure completes with no waiting period, because the player
    // asked to go — `kick_idle` is the only instruction with the token accounts
    // to pay them.
    let before_tokens = harness.token_balance(&wallets[leaver].pubkey());
    let owner = after.seats[leaver].owner;
    let stack = after.seats[leaver].stack;
    kick_idle(&mut harness, &wallets[0], &owner, &config, leaver as u8)
        .expect("a seat that asked to leave must not wait out the idle grace");

    let freed: TableState = harness.account(&table_state_pda(&config));
    assert!(!freed.seats[leaver].occupied, "the seat was never freed");
    assert_eq!(
        harness.token_balance(&wallets[leaver].pubkey()),
        before_tokens + stack,
        "the leaver was not paid their stack",
    );
    assert_eq!(
        freed.escrowed,
        freed.seats.iter().map(|seat| seat.stack).sum::<u64>(),
        "the vault does not balance after the eviction",
    );
}

// ---------------------------------------------------------------------------
// The once-per-street grace marker.
//
// `abandon_mental_deal` grants a parked street sixty extra seconds the first
// time it is asked, and records that it did so in `deal.next_round` as
// `board_len + 1`. The reuse rests on two claims, and both are checked here by
// running the program rather than by reading it: that `next_round` is dead once
// the deck is sealed, and that its value at that moment is zero — so no leftover
// round index can pass for a street that has already spent its grace.
// ---------------------------------------------------------------------------

fn commit_round(
    harness: &mut Harness,
    wallet: &Keypair,
    config: &Pubkey,
    round: DealRound,
    digest: [u8; 32],
) -> Result<(), String> {
    mental_send(
        harness,
        wallet,
        None,
        config,
        riverproof::instruction::CommitDealRound { round, digest }.data(),
    )
}

/// Drives a fresh hand to a sealed deck without doing any cryptography.
///
/// `commit_deal_round` stores a digest it never checks, and `seal_mental_deck`
/// only refuses an all-zero point — so a deck of placeholders reaches
/// `DealPhase::Sealed` through exactly the instructions a real one does. Nothing
/// can be *unlocked* afterwards, which is fine: what is under test here is the
/// value `next_round` holds when the phase flips, and that is decided entirely
/// by the round bookkeeping.
fn seal_a_placeholder_deck(harness: &mut Harness, wallets: &[Keypair], config: &Pubkey) {
    let players = wallets.len();
    start_hand_mental(harness, &wallets[0], config).expect("a hand");
    for player in 0..players {
        commit_round(harness, &wallets[player], config, DealRound::Shuffle, [player as u8 + 1; 32])
            .expect("shuffle commitment");
    }
    for player in 0..players {
        commit_round(harness, &wallets[player], config, DealRound::Lock, [player as u8 + 0x40; 32])
            .expect("lock commitment");
    }

    let deal: MentalDeal = harness.account(&mental_deal_pda(config));
    let count = deal.slot_count as usize;
    let locked: Vec<[u8; 32]> = (0..count).map(|index| [index as u8 + 0x80; 32]).collect();
    for (chunk_index, chunk) in locked.chunks(SEAL_CHUNK).enumerate() {
        mental_send(
            harness,
            &wallets[players - 1],
            None,
            config,
            riverproof::instruction::SealMentalDeck {
                offset: (chunk_index * SEAL_CHUNK) as u8,
                locked: chunk.to_vec(),
            }
            .data(),
        )
        .expect("seal_mental_deck");
    }
}

/// The marker can never be mistaken for a round index, at any table size.
///
/// This is the claim the whole reuse rests on. A street records its grace as
/// `board_len + 1`, which is 1, 4, 5 or 6 — so if `next_round` could still hold
/// one of those when the deck seals, that street would find its grace already
/// spent and be voided on the first ask instead of the second. It cannot:
/// `commit_deal_round` wraps `next_round` to zero on the last round of each
/// phase, and `seal_mental_deck` refuses to run unless it is zero. Checked at
/// three, four and ten seats, because the wrap is modulo the player count and a
/// table size is exactly the sort of thing that would break it.
#[test]
fn the_deck_only_ever_seals_with_next_round_at_zero() {
    for players in [3usize, 4, 10] {
        let (mut harness, wallets, config) = seated(players);
        seal_a_placeholder_deck(&mut harness, &wallets, &config);

        let deal: MentalDeal = harness.account(&mental_deal_pda(&config));
        assert_eq!(
            deal.phase,
            riverproof::state::DealPhase::Sealed,
            "{players} players: the placeholder deck did not seal",
        );
        assert_eq!(
            deal.next_round, 0,
            "{players} players: the deck sealed with next_round = {}, which is inside the \
             1..=6 the street marker uses — a street would silently lose its grace",
            deal.next_round,
        );
    }
}

/// A leftover round index is what would break the marker, so try to leave one.
///
/// `commit_deal_round` used to take a second round from player zero the moment
/// `next_round` wrapped back to them — the one lever anybody had over this
/// field. It is refused now, and the refusal is worth more than the marker:
/// that second round *overwrote* `lock_commitments[0]`, the Merkle root
/// `verify_unlock_share` checks proved hole shares against, so a player could
/// replace a root every other player had already verified off chain. Two locks
/// were between that and a sealed deck — `seal_mental_deck` refusing a nonzero
/// `next_round` — and neither was the one that should have stopped it.
#[test]
fn a_player_cannot_carry_a_nonzero_next_round_into_a_sealed_deck() {
    let (mut harness, wallets, config) = seated(3);
    start_hand_mental(&mut harness, &wallets[0], &config).expect("a hand");
    for player in 0..3 {
        commit_round(
            &mut harness,
            &wallets[player],
            &config,
            DealRound::Shuffle,
            [player as u8 + 1; 32],
        )
        .expect("shuffle commitment");
    }
    for player in 0..3 {
        commit_round(
            &mut harness,
            &wallets[player],
            &config,
            DealRound::Lock,
            [player as u8 + 0x40; 32],
        )
        .expect("lock commitment");
    }

    let locked: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(locked.next_round, 0, "the lock rounds did not wrap");

    // Player zero takes a round they no longer owe. It is refused outright.
    let refused =
        commit_round(&mut harness, &wallets[0], &config, DealRound::Lock, [0xee; 32])
            .expect_err("a player committed the same round twice");
    assert!(refused.contains("Custom"), "unexpected failure: {refused}");

    let armed: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(armed.next_round, 0, "a refused round still moved next_round");
    assert_eq!(
        armed.lock_commitments[0],
        [0x40u8; 32],
        "player zero replaced the Merkle root the other players verified against",
    );

    // That the deck then seals at zero is
    // `the_deck_only_ever_seals_with_next_round_at_zero`, which measures it at
    // three, four and ten seats rather than only here.
}

/// The field really is dead once the deck is sealed.
///
/// That is the premise the reuse rests on, and it is worth a runtime proof
/// rather than a grep: `commit_deal_round` is the only instruction that writes
/// `next_round` outside the marker, and it refuses to run outside `Shuffling`
/// and `Locking`. So once the phase flips, the byte belongs to the grace and
/// nothing else — no player, in any phase-legal way, can move it.
#[test]
fn nothing_can_move_next_round_once_the_deck_is_sealed() {
    let (mut harness, wallets, config) = seated(3);
    seal_a_placeholder_deck(&mut harness, &wallets, &config);
    let sealed: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(sealed.next_round, 0, "the deck did not seal at zero");

    for player in 0..3 {
        for round in [DealRound::Shuffle, DealRound::Lock] {
            let refused =
                commit_round(&mut harness, &wallets[player], &config, round, [0x99; 32])
                    .expect_err("a round was accepted against a sealed deck");
            assert!(refused.contains("Custom"), "unexpected failure: {refused}");
        }
    }

    let after: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(after.next_round, 0, "something moved next_round on a sealed deck");
}

/// Nothing writes the marker while the hand is still waiting on hole cards.
///
/// The grace exists for a *board street*. A deal that seals and then stops
/// before anybody can read their cards has no board slot due — `Awaiting::Deal`
/// asks for no board card — so it must be voided on the first ask, with the
/// field left alone.
#[test]
fn a_stalled_hole_deal_is_voided_at_once_and_writes_no_marker() {
    let (mut harness, wallets, config) = seated(3);
    seal_a_placeholder_deck(&mut harness, &wallets, &config);

    harness.advance(300);
    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    abandon(&mut harness, &stranger, &config).expect("a deal that never dealt must be abandonable");

    let hand: HandState = harness.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete, "the stalled deal was not voided");
    let deal: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(deal.next_round, 0, "a hole-card stall wrote a street marker");
}

/// Everybody checks or calls until the hand parks on cards again.
fn bet_until_parked(harness: &mut Harness, wallets: &[Keypair], config: &Pubkey) {
    for _ in 0..64 {
        let hand: HandState = harness.account(&hand_pda(config));
        if hand.awaiting != 0 || hand.stage == HandStage::Complete {
            return;
        }
        let actor = hand.actor_seat as usize;
        if act(harness, &wallets[actor], None, config, riverproof::instructions::PokerAction::Check)
            .is_err()
        {
            act(harness, &wallets[actor], None, config, riverproof::instructions::PokerAction::Call)
                .expect("call");
        }
    }
    panic!("the hand never parked on a street");
}

/// Opens the street the hand is parked on and hands it to the engine.
fn open_parked_street(
    harness: &mut Harness,
    wallets: &[Keypair],
    config: &Pubkey,
    session: &Session,
    slots: &[riverproof::state::DealSlot],
) {
    let players = wallets.len();
    let hand: HandState = harness.account(&hand_pda(config));
    let dealt = hand.board_len as usize;
    let pending = match hand.awaiting {
        1 => 3,
        4 => 5 - dealt,
        _ => 1,
    };
    let indices: Vec<u8> = (dealt..dealt + pending).map(|at| (players * 2 + at) as u8).collect();
    for player in 0..players {
        let shares: Vec<[u8; 32]> = indices
            .iter()
            .map(|index| {
                let position = slots[*index as usize].position;
                encode_scalar(&session.participants[player].unlock(position).expect("a share"))
            })
            .collect();
        mental_send(
            harness,
            &wallets[player],
            None,
            config,
            riverproof::instruction::PublishUnlocks {
                slots: indices.clone(),
                shares,
                keys: Vec::new(),
                paths: Vec::new(),
                round_digest: [0u8; 32],
            }
            .data(),
        )
        .expect("board unlocks");
    }
    mental_send(
        harness,
        &wallets[0],
        None,
        config,
        riverproof::instruction::RevealMentalStreet {}.data(),
    )
    .expect("reveal_mental_street");
}

/// Each street gets its own grace, and the flop having spent one does not spend
/// the turn's.
///
/// The marker is a single byte shared by every street, so the obvious way for
/// this to be wrong is for the second street to find the first street's value
/// still sitting there and conclude its own grace is gone. It does not, because
/// the value is `board_len + 1` and `board_len` has moved: the flop records 1
/// and the turn records 4. This drives both halves through the program — the
/// turn is refused on its first ask, exactly as the flop was, and voided only on
/// the second.
#[test]
fn the_turn_gets_its_own_grace_after_the_flop_has_spent_one() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    let (session, _deal, slots) = deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();

    // The flop parks and takes its one grace.
    harness.advance(600);
    bet_until_parked(&mut harness, &wallets, &config);
    let flop: HandState = harness.account(&hand_pda(&config));
    assert_eq!(flop.board_len, 0, "the flop should be parked with nothing on the board");
    let before: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(before.next_round, 0, "the marker was set before any street parked");

    harness.advance(600);
    let _ = abandon(&mut harness, &stranger, &config);
    let after_flop_grace: HandState = harness.account(&hand_pda(&config));
    assert_ne!(
        after_flop_grace.stage,
        HandStage::Complete,
        "the flop was voided on its first ask",
    );
    let marked: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(marked.next_round, 1, "the flop's grace was not recorded");

    // The flop is opened and played through, so the turn parks with the flop's
    // marker still in the field.
    open_parked_street(&mut harness, &wallets, &config, &session, &slots);
    bet_until_parked(&mut harness, &wallets, &config);
    let turn: HandState = harness.account(&hand_pda(&config));
    assert_eq!(turn.board_len, 3, "the hand did not reach the turn");
    assert_ne!(turn.awaiting, 0, "the turn is not parked on cards");
    let carried: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(carried.next_round, 1, "the flop's marker did not survive into the turn");

    // The turn's first ask must be refused, or the flop's grace has been spent
    // on the turn's behalf.
    harness.advance(600);
    let _ = abandon(&mut harness, &stranger, &config);
    let after_turn_ask: HandState = harness.account(&hand_pda(&config));
    assert_ne!(
        after_turn_ask.stage,
        HandStage::Complete,
        "THE TURN LOST ITS GRACE: a street the flop's marker made look already-graced was \
         voided on its first ask",
    );
    let turn_marked: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(turn_marked.next_round, 4, "the turn's grace was not recorded as its own");

    // And the second ask really does void it, so the grace is once and not
    // twice.
    harness.advance(90);
    abandon(&mut harness, &stranger, &config).expect("a turn nobody publishes to must be voided");
    let done: HandState = harness.account(&hand_pda(&config));
    assert_eq!(done.stage, HandStage::Complete, "the stalled turn was never voided");
}

/// The marker does not survive the hand it belongs to.
///
/// `abandon_mental_deal` leaves it set — it resets `phase`, `slot_count` and
/// `sealed_slots`, but not this field — so the next hand starts from whatever
/// the last street wrote. If `initialize_deal` did not zero it, `next_round`
/// would name a player who does not exist at a three-handed table and
/// `commit_deal_round` would refuse every one of them: a table that could never
/// deal again. It does zero it, and the proof is that the next hand deals.
#[test]
fn the_marker_is_gone_by_the_next_hand() {
    let (mut harness, wallets, config) = seated(3);
    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    let stranger = Keypair::new();
    harness.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();

    harness.advance(600);
    bet_until_parked(&mut harness, &wallets, &config);
    harness.advance(600);
    let _ = abandon(&mut harness, &stranger, &config);
    harness.advance(90);
    abandon(&mut harness, &stranger, &config).expect("the stalled flop is voided");

    // The field is left dirty on purpose — this is what the next hand has to
    // clean up, and the test is worthless if it is already zero here.
    let dirty: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_ne!(
        dirty.next_round, 0,
        "the voided hand left no marker behind, so this proves nothing",
    );

    // Start the next hand. Every seat was un-readied by the abandon, because
    // nobody had published anything against the street.
    harness.advance(60);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: play_accounts(&wallets[0].pubkey(), None, &config).to_account_metas(None),
                data: riverproof::instruction::StartNextHand {}.data(),
            },
            &[&wallets[0]],
        )
        .expect("start_next_hand");
    for wallet in wallets.iter() {
        set_ready(&mut harness, wallet, &config, true);
    }
    harness.advance(5);

    start_hand_mental(&mut harness, &wallets[0], &config).expect("the next hand");
    let fresh: MentalDeal = harness.account(&mental_deal_pda(&config));
    assert_eq!(
        fresh.next_round, 0,
        "the street marker leaked into the next hand; player {} is owed a round that \
         nobody at this table can send",
        fresh.next_round,
    );

    // And the round ordering really works, which is what the field is for.
    commit_round(&mut harness, &wallets[0], &config, DealRound::Shuffle, [0x11; 32])
        .expect("the first shuffle of the next hand");
}

/// A private table is one nobody can sit at without its password.
///
/// The password never reaches the chain. It is stretched into a keypair in the
/// client and only the public half is recorded, so joining means signing the
/// join transaction with the private half — a signature over a message nobody
/// will send again, which proves the password without naming it and cannot be
/// lifted off the chain and reused.
#[test]
fn a_private_table_refuses_a_player_without_its_key() {
    let mut harness = Harness::new();
    let creator = harness.player("alice");
    let stranger = harness.player("bob");
    let gate = Keypair::new();
    let config = create_table_locked(&mut harness, &creator, 6, gate.pubkey());

    // The creator holds the password like anybody else: locking a table does
    // not seat you at it.
    assert!(
        join_with(&mut harness, &creator, &config, 0, Some(&gate)).is_ok(),
        "the password did not open the table for the player who set it",
    );

    // No key at all.
    let refused = join_with(&mut harness, &stranger, &config, 1, None)
        .expect_err("a private table admitted a player who presented no key");
    assert!(refused.contains("TableLocked"), "wrong refusal: {refused}");

    // And the wrong key, which is what a wrong password derives.
    let wrong = Keypair::new();
    let refused = join_with(&mut harness, &stranger, &config, 1, Some(&wrong))
        .expect_err("a private table admitted a player with the wrong key");
    assert!(refused.contains("TableLocked"), "wrong refusal: {refused}");

    // The right one seats them.
    join_with(&mut harness, &stranger, &config, 1, Some(&gate))
        .expect("the password did not open the table");

    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(state.seats[0].occupied && state.seats[1].occupied, "both seats should be taken");
}

/// Topping up and cashing out are not asked for the password again.
///
/// The seat is already theirs, and making them prove it a second time would let
/// a forgotten password strand a player's chips at the table.
#[test]
fn a_seated_player_needs_no_password_to_rebuy_or_leave() {
    let mut harness = Harness::new();
    let creator = harness.player("alice");
    let gate = Keypair::new();
    let config = create_table_locked(&mut harness, &creator, 6, gate.pubkey());
    join_with(&mut harness, &creator, &config, 0, Some(&gate)).expect("join");

    let accounts = seat_accounts(&harness, &creator.pubkey(), &config);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: accounts.to_account_metas(None),
                data: riverproof::instruction::LeaveTable {}.data(),
            },
            &[&creator],
        )
        .expect("leaving a private table must not ask for the password again");

    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(!state.seats[0].occupied, "the seat should be free");
}

/// One endpoint, one table, until somebody who can see the plan says otherwise.
///
/// The limit is on chain because a client-side one is not a limit: `create_table`
/// is public and anybody can call it directly. The *cap* cannot be derived the
/// same way — it depends on the plan behind an API key, the program has no
/// network, and Helius publishes nothing about a plan over RPC. So it starts at
/// one and the faucet authority raises the endpoints it has checked.
#[test]
fn an_endpoint_carries_one_table_until_its_cap_is_raised() {
    let mut harness = Harness::new();
    let creator = harness.player("alice");

    create_table_on(&mut harness, &creator, 6, Pubkey::default(), 1, RPC_URL)
        .expect("the first table through an endpoint");

    // The second is refused, and by the program rather than by a form.
    let refused = create_table_on(&mut harness, &creator, 6, Pubkey::default(), 2, RPC_URL)
        .expect_err("an endpoint carried a second table at the default cap");
    assert!(refused.contains("EndpointAtCapacity"), "wrong refusal: {refused}");

    // A different endpoint has its own allowance and is unaffected.
    create_table_on(
        &mut harness,
        &creator,
        6,
        Pubkey::default(),
        3,
        "https://mainnet.helius-rpc.com/?api-key=another",
    )
    .expect("a second endpoint has its own allowance");

    // The authority raises the first one, and the table it refused now lands.
    let quota = endpoint_quota_pda(RPC_URL);
    harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::SetEndpointCap {
                    authority: harness.payer.pubkey(),
                    faucet: faucet_pda(),
                    endpoint_quota: quota,
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::SetEndpointCap {
                    endpoint: endpoint_hash(RPC_URL),
                    cap: 4,
                }
                .data(),
            },
            &[],
        )
        .expect("the faucet authority may raise a cap");

    create_table_on(&mut harness, &creator, 6, Pubkey::default(), 2, RPC_URL)
        .expect("the raised cap did not take effect");

    let state: riverproof::state::EndpointQuota = harness.account(&quota);
    assert_eq!(state.cap, 4, "the cap should be what the authority set");
    assert_eq!(state.tables, 2, "two tables should be counted against it");
}

/// Nobody but the operator may raise a cap, or the limit binds nobody.
#[test]
fn only_the_faucet_authority_may_raise_an_endpoints_cap() {
    let mut harness = Harness::new();
    let stranger = harness.player("mallory");
    let refused = harness
        .send(
            Instruction {
                program_id: program_id(),
                accounts: riverproof::accounts::SetEndpointCap {
                    authority: stranger.pubkey(),
                    faucet: faucet_pda(),
                    endpoint_quota: endpoint_quota_pda(RPC_URL),
                    system_program: anchor_lang::system_program::ID,
                }
                .to_account_metas(None),
                data: riverproof::instruction::SetEndpointCap {
                    endpoint: endpoint_hash(RPC_URL),
                    cap: 99,
                }
                .data(),
            },
            &[&stranger],
        )
        .expect_err("a stranger raised an endpoint's cap");
    assert!(refused.contains("NotFaucetAuthority"), "wrong refusal: {refused}");
}

/// Tables are played through Helius, and a lookalike host is not Helius.
#[test]
fn a_table_cannot_be_opened_on_an_endpoint_that_is_not_helius() {
    let mut harness = Harness::new();
    let creator = harness.player("alice");

    for url in [
        "https://api.mainnet-beta.solana.com",
        // Contains the name and is not the host, which a substring check would
        // have waved through.
        "https://evil.example.com/?rpc=helius-rpc.com",
        "https://helius-rpc.com.evil.example.com/?api-key=x",
    ] {
        let refused = create_table_on(&mut harness, &creator, 6, Pubkey::default(), 9, url)
            .expect_err(&format!("a table was opened on {url}"));
        assert!(refused.contains("EndpointNotAllowed"), "wrong refusal for {url}: {refused}");
    }

    // A subdomain is Helius, and loopback is allowed so the headless runs work.
    create_table_on(&mut harness, &creator, 6, Pubkey::default(), 10, "http://127.0.0.1:8899")
        .expect("a local validator must still be able to host a table");
}

/// Closing a table pays everyone out first, and only its creator may.
///
/// A table is deleted while people are sitting at it, so the chips in it have
/// to go somewhere: the pot goes back to whoever staked it, every stack goes
/// back to its own wallet, and only then does the vault close. Where they go is
/// decided by the seat's recorded owner, not by whoever sent the transaction.
#[test]
fn closing_a_table_pays_every_seat_and_returns_the_slot() {
    let mut harness = Harness::new();
    let creator = harness.player("alice");
    let bob = harness.player("bob");
    let stranger = harness.player("mallory");
    let config = create_table_on(&mut harness, &creator, 6, Pubkey::default(), 1, RPC_URL)
        .expect("create_table");
    let quota = endpoint_quota_pda(RPC_URL);

    join(&mut harness, &creator, &config, 0);
    join(&mut harness, &bob, &config, 1);
    let bob_before = harness.token_balance(&bob.pubkey());
    assert_eq!(bob_before, FAUCET_AMOUNT - BUY_IN, "bob's buy-in should be in the vault");

    // A stranger cannot delete somebody else's table, whoever is sitting at it.
    let refused = close_table(&mut harness, &stranger, &config)
        .expect_err("a stranger closed a table they did not create");
    assert!(refused.contains("NotTableCreator"), "wrong refusal: {refused}");

    // Nor can the creator send somebody else's chips to their own account. This
    // is the check the whole payout rests on: without it, deleting a table is a
    // way to take everybody's stack.
    let mine = get_associated_token_address(&creator.pubkey(), &harness.mint);
    let refused = close_table_paying(&mut harness, &creator, &config, Some(vec![mine, mine]))
        .expect_err("a creator redirected another seat's chips to themselves");
    assert!(refused.contains("NotTableCreator"), "wrong refusal: {refused}");

    // Done properly, everybody is made whole and the endpoint gets its place back.
    close_table(&mut harness, &creator, &config).expect("the creator could not close their table");

    assert_eq!(
        harness.token_balance(&bob.pubkey()),
        FAUCET_AMOUNT,
        "bob should have his whole stack back",
    );
    assert_eq!(
        harness.token_balance(&creator.pubkey()),
        FAUCET_AMOUNT,
        "the creator should have their own stack back and no more",
    );
    let after: riverproof::state::EndpointQuota = harness.account(&quota);
    assert_eq!(after.tables, 0, "closing must give the endpoint's slot back");
    assert!(
        harness.svm.get_account(&config).is_none_or(|a| a.owner != program_id()),
        "the table account should be gone",
    );
}

/// A hand in progress is cancelled, and its pot goes back to who staked it.
#[test]
fn closing_mid_hand_returns_the_pot_to_the_players() {
    let (mut harness, wallets, config) = three_handed();
    hand_in_progress(&mut harness, &wallets, &config);

    // Blinds are in the pot, so nobody is holding their whole stack right now.
    let hand: HandState = harness.account(&hand_pda(&config));
    assert!(hand.contributions.iter().any(|c| *c > 0), "the pot should have chips in it");

    close_table(&mut harness, &wallets[0], &config).expect("close_table mid-hand");

    // Every player is whole again: the pot was returned before the stacks were.
    for wallet in wallets.iter() {
        assert_eq!(
            harness.token_balance(&wallet.pubkey()),
            FAUCET_AMOUNT,
            "a player did not get their whole buy-in back",
        );
    }
}

fn close_table(
    harness: &mut Harness,
    creator: &Keypair,
    config: &Pubkey,
) -> Result<(), String> {
    close_table_paying(harness, creator, config, None)
}

/// Closing while naming where each seated player's chips go.
///
/// `payees` overrides the accounts, which is how a test can try to redirect
/// somebody else's chips and be refused.
fn close_table_paying(
    harness: &mut Harness,
    creator: &Keypair,
    config: &Pubkey,
    payees: Option<Vec<Pubkey>>,
) -> Result<(), String> {
    let state: TableState = harness.account(&table_state_pda(config));
    let mint = harness.mint;
    let seated: Vec<Pubkey> = payees.unwrap_or_else(|| {
        state
            .seats
            .iter()
            .filter(|seat| seat.occupied)
            .map(|seat| get_associated_token_address(&seat.owner, &mint))
            .collect()
    });
    let mut metas = riverproof::accounts::CloseTable {
        creator: creator.pubkey(),
        config: *config,
        // Only needed when the operator closes an orphan; harmless otherwise.
        faucet: Some(faucet_pda()),
        table_state: table_state_pda(config),
        hand_state: hand_pda(config),
        hand_cards: hand_cards_pda(config),
        // Only passed when the table has actually dealt. A table that never did
        // has no such account, and naming one that does not exist is
        // `AccountNotInitialized` rather than "no deal".
        deal: harness
            .svm
            .get_account(&mental_deal_pda(config))
            .filter(|account| account.owner == program_id())
            .map(|_| mental_deal_pda(config)),
        vault: vault_pda(config),
        endpoint_quota: Some(endpoint_quota_pda(RPC_URL)),
        mint: harness.mint,
        token_program: anchor_spl::token_2022::ID,
        system_program: anchor_lang::system_program::ID,
    }
    .to_account_metas(None);
    // One per occupied seat, in seat order, after the named accounts.
    for payee in seated {
        metas.push(solana_sdk::instruction::AccountMeta::new(payee, false));
    }
    harness.send(
        Instruction {
            program_id: program_id(),
            accounts: metas,
            data: riverproof::instruction::CloseTable {}.data(),
        },
        &[creator],
    )
}

/// The operator can close a table whose creator is gone, and nobody else can.
///
/// A creator key really can vanish — `pnpm agent:table` generated one per run
/// and discarded it, so devnet accumulated tables nobody could ever remove,
/// each holding a place on its endpoint. Without a second key they pile up
/// until the endpoint is full of tables nobody is at.
#[test]
fn the_operator_can_close_a_table_whose_creator_is_gone() {
    let mut harness = Harness::new();
    let orphan = harness.player("gone");
    let stranger = harness.player("mallory");
    let config = create_table_on(&mut harness, &orphan, 6, Pubkey::default(), 1, RPC_URL)
        .expect("create_table");
    join(&mut harness, &orphan, &config, 0);
    let before = harness.token_balance(&orphan.pubkey());

    // Still nobody else's to close. The operator is one key, not anybody.
    let refused = close_table(&mut harness, &stranger, &config)
        .expect_err("a stranger closed somebody else's table");
    assert!(refused.contains("NotTableCreator"), "wrong refusal: {refused}");

    // The faucet authority can, and the seat is paid out exactly as it would be
    // if its own creator had done it.
    let operator = harness.payer.insecure_clone();
    close_table(&mut harness, &operator, &config).expect("the operator could not close an orphan");

    assert_eq!(
        harness.token_balance(&orphan.pubkey()),
        before + BUY_IN,
        "the seated player must be paid out by an operator close too",
    );
    let quota: riverproof::state::EndpointQuota = harness.account(&endpoint_quota_pda(RPC_URL));
    assert_eq!(quota.tables, 0, "the endpoint should get its place back");
}

/// A seat that is not in the current hand still cannot be taken during it.
///
/// The guard used to ask only whether *this seat* was in the live hand. A
/// player who sits down while a hand is running is not in `active_seats`, so
/// nothing protected them: `join_table` starts their takeover clock the instant
/// they buy in, and two minutes later any cranker could free the chair and
/// refund the stack out from under them. They would find out on their next
/// action, as `NoSeat` — the error a player who never sat down gets.
#[test]
fn a_seat_outside_the_live_hand_survives_it() {
    let mut harness = Harness::new();
    let wallets: Vec<Keypair> =
        ["alice", "bob", "carol"].iter().map(|name| harness.player(name)).collect();
    let config = create_table(&mut harness, &wallets[0], 4);
    for (seat, wallet) in wallets.iter().enumerate() {
        join(&mut harness, wallet, &config, seat as u8);
        set_ready(&mut harness, wallet, &config, true);
    }
    harness.advance(5);

    let signing = Signing { keys: &wallets, sessions: None };
    let mut meter = Meter::default();
    deal_to_hole_cards(&mut harness, &signing, &config, &mut meter);

    // Dave takes the free chair while the other three are playing. Seated, not
    // ready, and not in this hand.
    let dave = harness.player("dave");
    join(&mut harness, &dave, &config, 3);

    // Long enough that the takeover grace has run out underneath him.
    harness.advance(310);

    let live: HandState = harness.account(&hand_pda(&config));
    assert_ne!(live.stage, HandStage::Complete, "the hand should still be running");

    assert!(
        kick_idle(&mut harness, &wallets[0], &dave.pubkey(), &config, 3).is_err(),
        "a seat was taken while the table was mid-hand",
    );
    let state: TableState = harness.account(&table_state_pda(&config));
    assert!(state.seats[3].occupied, "the seat was freed under a live hand");
}
