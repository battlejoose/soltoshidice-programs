#![cfg(not(feature="mainnet"))]
//! Canonical asset and devnet-publisher onboarding authorization in actual SBF.
use acidpump_er::{self as program, engine::*, Asset, Config};
use anchor_lang::{prelude::Pubkey, AccountDeserialize, AccountSerialize, InstructionData, ToAccountMetas,
    solana_program::instruction::{AccountMeta, Instruction}};
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
fn pda(seeds: &[&[u8]], owner: Pubkey) -> Pubkey { Pubkey::find_program_address(seeds, &owner).0 }
fn send(svm: &mut LiteSVM, payer: &Keypair, signer: &Keypair, ix: Instruction, expected: Option<&str>) {
    svm.expire_blockhash();
    let mut heap = vec![1u8]; heap.extend_from_slice(&(128u32 * 1024).to_le_bytes());
    let instructions = [Instruction { program_id: Pubkey::from_str("ComputeBudget111111111111111111111111111111").unwrap(), accounts: vec![], data: heap }, ix];
    let message = v0::Message::try_compile(&payer.pubkey(), &instructions, &[], svm.latest_blockhash()).unwrap();
    let mut signers = vec![payer];
    if instructions[1].accounts.iter().any(|a| a.pubkey == signer.pubkey() && a.is_signer) { signers.push(signer); }
    let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &signers).unwrap();
    let before: Vec<_> = instructions[1].accounts.iter().map(|a| (a.pubkey, svm.get_account(&a.pubkey))).collect();
    match (svm.send_transaction(tx), expected) {
        (Ok(meta), None) => println!("quote onboarding: {} CU", meta.compute_units_consumed),
        (Err(error), Some(reason)) => {
            assert!(error.meta.logs.iter().any(|log| log.contains(reason)), "expected {reason}: {error:?}");
            for (key, old) in before { assert_eq!(old, svm.get_account(&key), "failed instruction changed {key}"); }
        }
        (result, expected) => panic!("expected {expected:?}, got {result:?}"),
    }
}

#[test]
fn publisher_can_prepare_only_canonical_registered_quotes_without_admin_authority() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut svm = LiteSVM::new();
    svm.add_program(program::ID, &std::fs::read(root.join("target/deploy/acidpump_er.so")).unwrap()).unwrap();
    let delegation = Pubkey::from_str("DELeGGvXpWV2fqJUhqcF5ZSYMS4JTLjteaAMARRSaeSh").unwrap();
    svm.add_program(delegation, &std::fs::read(root.join(".cache/magicblock-validator-audit-v1/test-integration/schedulecommit/elfs/dlp.so")).unwrap()).unwrap();
    let payer = Keypair::new_from_array([61; 32]); let publisher = Keypair::new_from_array([62; 32]);
    let admin = Keypair::new_from_array([63; 32]); let stranger = Keypair::new_from_array([64; 32]);
    for key in [&payer, &publisher, &admin, &stranger] { svm.airdrop(&key.pubkey(), 1_000_000_000).unwrap(); }
    let (config, config_bump) = Pubkey::find_program_address(&[b"config"], &program::ID);
    put(&mut svm, config, &Config { admin: admin.pubkey(), publisher: publisher.pubkey(), quote_mint: Pubkey::new_unique(),
        paused: false, bump: config_bump, next_settlement_seq: 1 });
    let mint = Pubkey::new_unique(); let (asset, bump) = Pubkey::find_program_address(&[b"asset", mint.as_ref()], &program::ID);
    let asset_data = Asset { id: 200, feed: Pubkey::new_unique(), mint, token_program: Pubkey::new_unique(),
        source_mint: Pubkey::new_unique(), source_pool: Pubkey::new_unique(), quote_mint: Pubkey::new_unique(),
        dex_pool: Pubkey::new_unique(), collateral_enabled: true, decimals: 6, bump };
    put(&mut svm, asset, &asset_data);
    let quote = pda(&[b"er-quote", asset.as_ref()], program::ID);
    let initialize = Instruction { program_id: program::ID,
        accounts: program::accounts::InitializeErQuote { admin: publisher.pubkey(), config, asset, er_quote: quote,
            system_program: Pubkey::default() }.to_account_metas(None), data: program::instruction::ErInitializeQuote {}.data() };
    let mut unsigned = initialize.clone(); unsigned.accounts[0].is_signer = false;
    send(&mut svm, &payer, &publisher, unsigned, Some("AccountNotSigner"));
    let mut foreign = initialize.clone(); foreign.accounts[0].pubkey = stranger.pubkey();
    send(&mut svm, &payer, &stranger, foreign, Some("Unauthorized"));
    let fake_asset = Pubkey::new_unique(); put(&mut svm, fake_asset, &asset_data);
    let mut forged = initialize.clone(); forged.accounts[2].pubkey = fake_asset;
    forged.accounts[3].pubkey = pda(&[b"er-quote", fake_asset.as_ref()], program::ID);
    send(&mut svm, &payer, &publisher, forged, Some("ConstraintSeeds"));
    send(&mut svm, &payer, &publisher, initialize.clone(), None);
    let stored = svm.get_account(&quote).unwrap();
    let row = ErQuote::try_deserialize(&mut &stored.data[..]).unwrap();
    assert_eq!(row.config, config); assert_eq!(row.asset, asset); assert!(!row.is_quote); assert_eq!(row.source_slot, 0);
    let delegate = Instruction { program_id: program::ID, accounts: vec![
        AccountMeta::new(publisher.pubkey(), true), AccountMeta::new_readonly(config, false), AccountMeta::new_readonly(asset, false),
        AccountMeta::new(pda(&[b"buffer", quote.as_ref()], program::ID), false),
        AccountMeta::new(pda(&[b"delegation", quote.as_ref()], delegation), false),
        AccountMeta::new(pda(&[b"delegation-metadata", quote.as_ref()], delegation), false),
        AccountMeta::new(quote, false), AccountMeta::new_readonly(program::ID, false), AccountMeta::new_readonly(delegation, false),
        AccountMeta::new_readonly(Pubkey::default(), false),
    ], data: program::instruction::ErDelegateQuote {}.data() };
    let mut unauthorized = delegate.clone(); unauthorized.accounts[0].pubkey = stranger.pubkey();
    send(&mut svm, &payer, &stranger, unauthorized, Some("Unauthorized"));
    let mut forged = delegate.clone(); forged.accounts[2].pubkey = fake_asset;
    send(&mut svm, &payer, &publisher, forged, Some("ConstraintSeeds"));
    send(&mut svm, &payer, &publisher, delegate, None);
    assert_eq!(svm.get_account(&quote).unwrap().owner, delegation);
    let record = svm.get_account(&pda(&[b"delegation", quote.as_ref()], delegation)).unwrap();
    assert_eq!(record.owner, delegation); assert_eq!(&record.data[8..40], ER_VALIDATOR.as_ref());
    // The publisher did not gain general administrator powers.
    let risk = pda(&[b"er-risk"], program::ID);
    let risk_ix = Instruction { program_id: program::ID,
        accounts: program::accounts::InitializeErRisk { admin: publisher.pubkey(), config, er_risk: risk, system_program: Pubkey::default() }.to_account_metas(None),
        data: program::instruction::ErInitializeRisk {}.data() };
    send(&mut svm, &payer, &publisher, risk_ix, Some("ConstraintHasOne"));
    // The original administrator path remains valid as well.
    svm.set_account(quote, Account::default()).unwrap();
    let mut admin_init = initialize; admin_init.accounts[0].pubkey = admin.pubkey();
    send(&mut svm, &payer, &admin, admin_init, None);
}
