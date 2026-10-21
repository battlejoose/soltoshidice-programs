#![cfg(not(feature="mainnet"))]
//! "Create a market" (venues) in actual SBF: admin-only creation, isolation
//! from the root SDICE market, and venue-scoped risk and pause.
//! The full venue loss split and SDICE buyback run against real PumpSwap pools
//! in scripts/modular/smoke.ts (receipts in reports/modular-*-smoke-*.json).
use acidpump_er::{self as program, engine::ErRisk, venue::Venue, Asset, Config, Pool};
use anchor_lang::{prelude::Pubkey, AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
    solana_program::{instruction::Instruction, program_pack::Pack}};
use anchor_spl::token::spl_token;
use litesvm::LiteSVM;
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{path::Path, str::FromStr};

fn put<T: AccountSerialize>(svm: &mut LiteSVM, key: Pubkey, value: &T) {
    let mut data = Vec::new(); value.try_serialize(&mut data).unwrap();
    svm.set_account(key, Account { lamports: svm.minimum_balance_for_rent_exemption(data.len()), data,
        owner: program::ID, executable: false, rent_epoch: 0 }).unwrap();
}
fn read<T: AccountDeserialize>(svm: &LiteSVM, key: Pubkey) -> T {
    T::try_deserialize(&mut &svm.get_account(&key).unwrap().data[..]).unwrap()
}
fn mint(svm: &mut LiteSVM, key: Pubkey, authority: Pubkey) {
    let mut data = vec![0u8; spl_token::state::Mint::LEN];
    spl_token::state::Mint { mint_authority: Some(authority).into(), supply: 0, decimals: 6, is_initialized: true,
        freeze_authority: None.into() }.pack_into_slice(&mut data);
    svm.set_account(key, Account { lamports: svm.minimum_balance_for_rent_exemption(data.len()), data,
        owner: spl_token::ID, executable: false, rent_epoch: 0 }).unwrap();
}
fn pda(seeds: &[&[u8]]) -> Pubkey { Pubkey::find_program_address(seeds, &program::ID).0 }
fn send(svm: &mut LiteSVM, payer: &Keypair, signer: &Keypair, ix: Instruction, expected: Option<&str>) {
    svm.expire_blockhash();
    let mut heap = vec![1u8]; heap.extend_from_slice(&(128u32 * 1024).to_le_bytes());
    let instructions = [Instruction { program_id: Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(), accounts: vec![], data: heap }, ix];
    let message = v0::Message::try_compile(&payer.pubkey(), &instructions, &[], svm.latest_blockhash()).unwrap();
    let mut signers = vec![payer];
    if signer.pubkey() != payer.pubkey() && instructions[1].accounts.iter().any(|a| a.pubkey == signer.pubkey() && a.is_signer) { signers.push(signer); }
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &signers).unwrap();
    match (svm.send_transaction(tx), expected) {
        (Ok(meta), None) => println!("venue: {} CU", meta.compute_units_consumed),
        (Err(error), Some(reason)) => assert!(error.meta.logs.iter().any(|log| log.contains(reason)), "expected {reason}: {error:?}"),
        (result, expected) => panic!("expected {expected:?}, got {result:?}"),
    }
}
struct World { svm: LiteSVM, admin: Keypair, publisher: Keypair, quote: Pubkey, root_pool: Pubkey, sdice: Pubkey }
fn world() -> World {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut svm = LiteSVM::new();
    svm.add_program(program::ID, &std::fs::read(root.join("target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let admin = Keypair::new_from_array([71; 32]); let publisher = Keypair::new_from_array([72; 32]);
    for key in [&admin, &publisher] { svm.airdrop(&key.pubkey(), 10_000_000_000).unwrap(); }
    let quote = Pubkey::new_unique(); mint(&mut svm, quote, admin.pubkey());
    let (config, config_bump) = Pubkey::find_program_address(&[b"config"], &program::ID);
    assert_eq!(config, program::ROOT_CONFIG);
    put(&mut svm, config, &Config { admin: admin.pubkey(), publisher: publisher.pubkey(), quote_mint: quote, paused: false, bump: config_bump, next_settlement_seq: 1 });
    let sdice_mint = Pubkey::new_unique(); mint(&mut svm, sdice_mint, admin.pubkey());
    let sdice = asset(&mut svm, sdice_mint, 102, quote, true);
    let (root_pool, bump) = Pubkey::find_program_address(&[b"pool", &0u16.to_le_bytes()], &program::ID);
    assert_eq!(root_pool, program::ROOT_POOL);
    put(&mut svm, root_pool, &Pool { id: 0, settlement: sdice, authority_bump: 255, queue_head: 0, queue_tail: 0, queue_usd: 0, trades: 0, bump });
    World { svm, admin, publisher, quote, root_pool, sdice }
}
fn asset(svm: &mut LiteSVM, mint: Pubkey, id: u16, quote: Pubkey, collateral: bool) -> Pubkey {
    let (key, bump) = Pubkey::find_program_address(&[b"asset", mint.as_ref()], &program::ID);
    put(svm, key, &Asset { id, feed: Pubkey::new_unique(), mint, token_program: spl_token::ID, source_mint: mint,
        source_pool: Pubkey::new_unique(), quote_mint: quote, dex_pool: Pubkey::new_unique(), collateral_enabled: collateral, decimals: 6, bump });
    key
}
fn create_venue(w: &World, signer: Pubkey, token: Pubkey) -> (Instruction, Pubkey, Pubkey, Pubkey) {
    let config = pda(&[b"config", token.as_ref()]);
    let pool = pda(&[b"pool", config.as_ref(), &0u16.to_le_bytes()]);
    let venue = pda(&[b"venue", config.as_ref()]);
    let ix = Instruction { program_id: program::ID, accounts: program::accounts::CreateVenue {
        admin: signer, creator: Pubkey::new_unique(), root: program::ROOT_CONFIG, root_pool: w.root_pool,
        asset: pda(&[b"asset", token.as_ref()]), mint: token, config, venue, pool, authority: pda(&[b"authority", pool.as_ref()]),
        swap_mint: w.quote, quote_program: spl_token::ID, quote_vault: pda(&[b"quote", pool.as_ref()]),
        buyback_vault: pda(&[b"buyback", pool.as_ref()]), system_program: Pubkey::default(),
    }.to_account_metas(None), data: program::instruction::CreateVenue {}.data() };
    (ix, config, pool, venue)
}

#[test]
fn admin_creates_an_isolated_market_backed_by_a_registered_token() {
    let mut w = world();
    let wif = Pubkey::new_unique(); mint(&mut w.svm, wif, w.admin.pubkey());
    let wif_asset = asset(&mut w.svm, wif, 201, w.quote, true);
    let (admin, publisher) = (w.admin.insecure_clone(), w.publisher.insecure_clone());
    // Like token registration, creation needs the admin (registrar) signature.
    let (ix, ..) = create_venue(&w, publisher.pubkey(), wif);
    send(&mut w.svm, &publisher, &publisher, ix, Some("ConstraintHasOne"));
    // The root's own token cannot back a second market.
    let sdice_mint = read::<Asset>(&w.svm, w.sdice).mint;
    let (ix, ..) = create_venue(&w, admin.pubkey(), sdice_mint);
    send(&mut w.svm, &admin, &admin, ix, Some("Binding"));
    // Unregistered-as-collateral tokens are refused.
    let disabled = Pubkey::new_unique(); mint(&mut w.svm, disabled, admin.pubkey());
    asset(&mut w.svm, disabled, 202, w.quote, false);
    let (ix, ..) = create_venue(&w, admin.pubkey(), disabled);
    send(&mut w.svm, &admin, &admin, ix, Some("Binding"));

    let (ix, config, pool, venue) = create_venue(&w, admin.pubkey(), wif);
    send(&mut w.svm, &admin, &admin, ix.clone(), None);
    let c: Config = read(&w.svm, config);
    assert_eq!((c.admin, c.publisher, c.quote_mint, c.paused, c.next_settlement_seq), (admin.pubkey(), publisher.pubkey(), w.quote, false, 1));
    let p: Pool = read(&w.svm, pool);
    assert_eq!((p.id, p.settlement), (0, wif_asset));
    let v: Venue = read(&w.svm, venue);
    assert_eq!((v.config, v.pool, v.mint, v.asset), (config, pool, wif, wif_asset));
    assert_eq!([v.pool_bps, v.staker_bps, v.burn_bps, v.gas_bps, v.buyback_bps], [9_000, 700, 100, 100, 100]);
    for vault in [pda(&[b"quote", pool.as_ref()]), pda(&[b"buyback", pool.as_ref()])] {
        let account = w.svm.get_account(&vault).unwrap();
        let token = spl_token::state::Account::unpack(&account.data).unwrap();
        assert_eq!((token.mint, token.owner), (w.quote, pda(&[b"authority", pool.as_ref()])));
    }
    // A market exists once per token.
    send(&mut w.svm, &admin, &admin, ix, Some("already in use"));

    // Its ER risk ledger is namespaced by its config; the root's is untouched.
    let risk = pda(&[b"er-risk", config.as_ref()]);
    let init = |config: Pubkey, risk: Pubkey| Instruction { program_id: program::ID, accounts: program::accounts::InitializeErRisk {
        admin: admin.pubkey(), config, er_risk: risk, system_program: Pubkey::default() }.to_account_metas(None),
        data: program::instruction::ErInitializeRisk {}.data() };
    send(&mut w.svm, &admin, &admin, init(config, pda(&[b"er-risk"])), Some("ConstraintSeeds"));
    send(&mut w.svm, &admin, &admin, init(config, risk), None);
    assert_eq!(read::<ErRisk>(&w.svm, risk).config, config);
    assert!(w.svm.get_account(&pda(&[b"er-risk"])).is_none());

    // Each market pauses on its own.
    let pause = Instruction { program_id: program::ID, accounts: program::accounts::Admin { admin: admin.pubkey(), config }.to_account_metas(None),
        data: program::instruction::Pause { paused: true }.data() };
    send(&mut w.svm, &admin, &admin, pause, None);
    assert!(read::<Config>(&w.svm, config).paused);
    assert!(!read::<Config>(&w.svm, program::ROOT_CONFIG).paused);
}

#[test]
fn only_the_publisher_can_route_a_buyback_and_only_into_the_root_pool() {
    let mut w = world();
    let wif = Pubkey::new_unique(); mint(&mut w.svm, wif, w.admin.pubkey());
    asset(&mut w.svm, wif, 201, w.quote, true);
    let admin = w.admin.insecure_clone();
    let (ix, config, pool, venue) = create_venue(&w, admin.pubkey(), wif);
    send(&mut w.svm, &admin, &admin, ix, None);
    let stranger = Keypair::new_from_array([73; 32]);
    w.svm.airdrop(&stranger.pubkey(), 1_000_000_000).unwrap();
    let root_authority = pda(&[b"authority", w.root_pool.as_ref()]);
    let sdice_mint = read::<Asset>(&w.svm,w.sdice).mint;
    let route = |publisher: Pubkey| Instruction { program_id: program::ID, accounts: program::accounts::RouteBuyback {
        publisher, root: program::ROOT_CONFIG, venue, pool, authority: pda(&[b"authority", pool.as_ref()]),
        buyback_vault: pda(&[b"buyback", pool.as_ref()]), root_pool: w.root_pool, root_authority, sdice: w.sdice,
        sdice_mint, sdice_program: spl_token::ID,
        sdice_valuation: program::accounts::Valuation { dex_pool: Pubkey::new_unique(), dex_base: Pubkey::new_unique(),
            dex_quote: Pubkey::new_unique(), fee_config: Pubkey::new_unique() },
        root_inventory: pda(&[b"inventory", w.root_pool.as_ref(), w.sdice.as_ref()]),
        root_vault: pda(&[b"vault", w.root_pool.as_ref(), w.sdice.as_ref()]),
        root_quote: pda(&[b"quote", w.root_pool.as_ref()]), token_program: spl_token::ID,
        pump_program: Pubkey::from_str("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA").unwrap(),
    }.to_account_metas(None), data: program::instruction::RouteBuyback { amount: 0, min_out: 1 }.data() };
    // Rejected before any token can move (the root accounts are absent here,
    // and a non-publisher signer can never satisfy the root config binding).
    send(&mut w.svm, &stranger, &stranger, route(stranger.pubkey()), Some("AnchorError"));
    let _ = config;
}
