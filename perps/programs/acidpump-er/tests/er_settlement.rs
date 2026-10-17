#![cfg(not(feature="mainnet"))]
//! Actual devnet Acidpump ER, SPL Token, PumpSwap and fee-program SBF execution.
//! Only the DLP-owned committed ErTrader snapshot is injected. This deliberately
//! does not claim to exercise delegation, ER execution or MagicBlock commits.
use acidpump_er::{self as program, basebridge::DELEGATION_OWNER, engine::*, math, pump,
    Config, GasReserve, Inventory, OwnershipPool, OwnershipReceipt, Pool, Position};
use anchor_lang::{prelude::{Clock, Pubkey}, AccountDeserialize, AccountSerialize, InstructionData,
    ToAccountMetas, solana_program::instruction::{AccountMeta, Instruction}};
use litesvm::LiteSVM;
use serde_json::{json, Value};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_message::{v0, AddressLookupTableAccount, VersionedMessage};
use solana_signer::Signer;
use solana_transaction::versioned::VersionedTransaction;
use std::{collections::BTreeSet, path::Path, str::FromStr};

fn pk(s: &str) -> Pubkey { Pubkey::from_str(s).unwrap() }
fn key(row: &Value, name: &str) -> Pubkey { pk(row[name].as_str().unwrap()) }
fn pda(seeds: &[&[u8]]) -> Pubkey { Pubkey::find_program_address(seeds, &program::ID).0 }
fn read<T: AccountDeserialize>(svm: &LiteSVM, address: Pubkey) -> T {
    T::try_deserialize(&mut &svm.get_account(&address).unwrap().data[..]).unwrap()
}
fn token(svm: &LiteSVM, address: Pubkey) -> u64 {
    u64::from_le_bytes(svm.get_account(&address).unwrap().data[64..72].try_into().unwrap())
}
fn wire(row: &Value) -> Instruction {
    Instruction { program_id: key(row, "program"), data: row["data"].as_array().unwrap().iter().map(|b| b.as_u64().unwrap() as u8).collect(),
        accounts: row["keys"].as_array().unwrap().iter().map(|a| AccountMeta { pubkey: key(a,"key"),
            is_writable: a["writable"].as_bool().unwrap(), is_signer: a["signer"].as_bool().unwrap() }).collect() }
}
fn ix(accounts: impl ToAccountMetas, instruction: impl InstructionData) -> Instruction {
    Instruction { program_id: program::ID, accounts: accounts.to_account_metas(None), data: instruction.data() }
}
fn instructions(rows: &Value) -> Vec<Instruction> { rows.as_array().unwrap().iter().map(wire).collect() }

struct Harness { svm: LiteSVM, signers: Vec<Keypair>, fixture: Value, receipts: Vec<Value>, rejected: usize }
impl Harness {
    fn send(&mut self, name: &str, mut ixs: Vec<Instruction>, fails: bool) {
        let compute = pk("ComputeBudget111111111111111111111111111111");
        if !ixs.iter().any(|i| i.program_id == compute) {
            for (tag, amount) in [(2u8, 1_400_000u32), (1, 128 * 1024)] {
                let mut data = vec![tag]; data.extend_from_slice(&amount.to_le_bytes());
                ixs.insert(0, Instruction { program_id: compute, accounts: vec![], data });
            }
        }
        let mut clock: Clock = self.svm.get_sysvar();
        clock.unix_timestamp = self.fixture["now"].as_i64().unwrap(); clock.slot += 3;
        self.svm.set_sysvar(&clock); self.svm.expire_blockhash();
        let payer = self.signers[0].pubkey();
        let addresses: Vec<Pubkey> = ixs.iter().flat_map(|i| i.accounts.iter().map(|a| a.pubkey)).collect::<BTreeSet<_>>().into_iter().collect();
        let table = AddressLookupTableAccount { key: Pubkey::new_from_array([91; 32]), addresses };
        let mut table_data = vec![0u8; 56]; table_data[..4].copy_from_slice(&1u32.to_le_bytes()); table_data[4..12].copy_from_slice(&u64::MAX.to_le_bytes());
        for address in &table.addresses { table_data.extend_from_slice(address.as_ref()); }
        self.svm.set_account(table.key, Account { lamports: 100_000_000, data: table_data,
            owner: pk("AddressLookupTab1e1111111111111111111111111"), executable: false, rent_epoch: 0 }).unwrap();
        let message = v0::Message::try_compile(&payer, &ixs, &[table], self.svm.latest_blockhash()).unwrap();
        let required: Vec<&Keypair> = self.signers.iter().filter(|s| s.pubkey() == payer || ixs.iter().any(|i| i.accounts.iter().any(|a| a.is_signer && a.pubkey == s.pubkey()))).collect();
        let tx = VersionedTransaction::try_new(VersionedMessage::V0(message), &required).unwrap();
        let before: Vec<_> = ixs.iter().flat_map(|i| i.accounts.iter()).map(|a| (a.pubkey, self.svm.get_account(&a.pubkey))).collect();
        let result = self.svm.send_transaction(tx);
        if fails {
            assert!(result.is_err(), "{name}: attack unexpectedly succeeded");
            let failure = result.as_ref().unwrap_err();
            let expected = match name {
                "uncommitted program-owned trader is rejected" | "native ER cannot mint base custody credits" | "native ER cannot initialize base custody" => Some("Unauthorized"),
                "wrong committed nonce is rejected" | "committed bytes cannot impersonate another owner" | "forged DLP snapshot at another address is rejected" => Some("Binding"),
                "second winner cannot jump first winner" | "payout cannot skip FIFO head" => Some("QueueOrder"),
                "unpaid winning claim cannot be recycled" => Some("NoClaim"),
                "initialized destination cannot extract sponsored rent" | "surplus rent cannot be extracted through recycling" => Some("Binding"),
                "withdrawal receipt prevents replay" | "settlement receipt rejects repeat nonce" => Some("Replay"),
                _ => None,
            };
            if let Some(reason) = expected { assert!(failure.meta.logs.iter().any(|l| l.contains(reason)), "{name}: unexpected rejection: {failure:?}"); }
            if name == "late swap failure rolls back stage, token CPIs and receipt" {
                assert!(failure.meta.logs.iter().any(|l| l.contains(&pump::ID.to_string())), "must reach the real PumpSwap CPI: {failure:?}");
            }
            for (address, old) in before {
                let new = self.svm.get_account(&address);
                assert_eq!(old.as_ref().map(|a| &a.data), new.as_ref().map(|a| &a.data), "{name}: mutated {address} on failure");
                if address != payer { assert_eq!(old.map(|a| a.lamports), new.map(|a| a.lamports), "{name}: moved rent/SOL on failure"); }
            }
            self.rejected += 1;
            self.receipts.push(json!({"name":name,"rejected":true,"error":format!("{:?}",failure.err)}));
        } else {
            assert!(result.is_ok(), "{name}: {result:?}");
            let meta = result.unwrap();
            println!("{name}: {} CU", meta.compute_units_consumed);
            self.receipts.push(json!({"name":name,"computeUnits":meta.compute_units_consumed,"feeLamports":meta.fee}));
        }
    }
    fn prepare(&self, owner: Pubkey, nonce: u64, slot: u8, asset: Pubkey) -> Instruction {
        ix(program::accounts::PrepareSettlement { payer: self.signers[0].pubkey(), owner,
            config: key(&self.fixture,"config"), pool: key(&self.fixture,"pool"), market: key(&self.fixture,"market"), asset,
            trader: pda(&[b"er-trader", owner.as_ref()]), custody: pda(&[b"er-custody", owner.as_ref()]),
            position: pda(&[b"position",owner.as_ref(),&nonce.to_le_bytes()]), system_program: Pubkey::default() },
            program::instruction::PrepareSettlement { nonce, slot })
    }
    fn settlement(&self, name: &str, slot: u8) -> Vec<Instruction> {
        let row = &self.fixture["settlements"][name];
        let mut result = vec![self.prepare(key(row,"owner"), row["nonce"].as_u64().unwrap(), slot, key(row,"asset"))];
        result.extend(instructions(&row["ixs"])); result
    }
    fn recycle(&self, owner:Pubkey, old_nonce:u64, new_nonce:u64) -> Instruction {
        let position=pda(&[b"position",owner.as_ref(),&old_nonce.to_le_bytes()]);
        let next_position=pda(&[b"position",owner.as_ref(),&new_nonce.to_le_bytes()]);
        ix(program::accounts::RecyclePosition { executor:self.signers[0].pubkey(),owner,pool:key(&self.fixture,"pool"),position,
            ownership_receipt:pda(&[b"ownership-receipt",position.as_ref()]),next_position,
            next_receipt:pda(&[b"ownership-receipt",next_position.as_ref()]),instructions:solana_instructions_sysvar::ID },
            program::instruction::RecyclePosition { new_nonce })
    }
    fn later_settlement(&self, owner:Pubkey, nonce:u64, slot:u8) -> Vec<Instruction> {
        let mut ixs=self.settlement("alice_loss",slot);
        ixs[0]=self.prepare(owner,nonce,slot,key(&self.fixture["assets"][0],"key"));
        let old_position=key(&self.fixture["settlements"]["alice_loss"],"position");
        let old_receipt=pda(&[b"ownership-receipt",old_position.as_ref()]);
        let next_position=pda(&[b"position",owner.as_ref(),&nonce.to_le_bytes()]);
        let next_receipt=pda(&[b"ownership-receipt",next_position.as_ref()]);
        for i in &mut ixs { for a in &mut i.accounts {
            if a.pubkey==old_position { a.pubkey=next_position; }
            if a.pubkey==old_receipt { a.pubkey=next_receipt; }
        }}
        ixs
    }
    fn initialize_custody(&self, owner:Pubkey) -> Instruction {
        ix(program::accounts::InitializeCustody { payer:self.signers[0].pubkey(),owner,
            config:key(&self.fixture,"config"),pool:key(&self.fixture,"pool"),
            custody:pda(&[b"er-custody",owner.as_ref()]),system_program:Pubkey::default(),
            execution:program::accounts::BaseExecution { magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID } },
            program::instruction::InitializeCustody {})
    }
    fn deposit(&self, owner: Pubkey, asset: &Value, amount: u64) -> Instruction {
        ix(program::accounts::DepositCollateral { owner, config: key(&self.fixture,"config"), pool: key(&self.fixture,"pool"),
            custody: pda(&[b"er-custody",owner.as_ref()]), asset: key(asset,"key"), mint: key(asset,"mint"), token_program: key(asset,"tokenProgram"),
            inventory: key(asset,"inventory"), authority: key(&self.fixture,"authority"),
            source: key(asset,if owner == self.signers[1].pubkey() {"aliceToken"} else {"bobToken"}), vault: key(asset,"vault"),
            execution: program::accounts::BaseExecution { magic_program: ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID } },
            program::instruction::DepositCollateral { amount })
    }
    fn withdrawal(&self, owner: Pubkey, asset: &Value, nonce: u64, slot: u8) -> Instruction {
        ix(program::accounts::SettleWithdrawal { caller: self.signers[0].pubkey(), config: key(&self.fixture,"config"), pool: key(&self.fixture,"pool"),
            custody: pda(&[b"er-custody",owner.as_ref()]), trader: pda(&[b"er-trader",owner.as_ref()]), asset: key(asset,"key"),
            mint: key(asset,"mint"), token_program: key(asset,"tokenProgram"), inventory: key(asset,"inventory"), authority: key(&self.fixture,"authority"),
            vault: key(asset,"vault"), destination: key(asset,if owner == self.signers[1].pubkey() {"aliceToken"} else {"bobToken"}) },
            program::instruction::SettleWithdrawal { nonce, slot })
    }
    fn commit_boundary(&mut self, trader: &ErTrader, owner_program: Pubkey) {
        let mut data = Vec::new(); trader.try_serialize(&mut data).unwrap();
        self.svm.set_account(pda(&[b"er-trader",trader.owner.as_ref()]), Account { lamports: self.svm.minimum_balance_for_rent_exemption(data.len()),
            data, owner: owner_program, executable: false, rent_epoch: 0 }).unwrap();
        let (guard_key,bump)=Pubkey::find_program_address(&[b"er-collateral-guard",trader.owner.as_ref()],&program::ID);
        let guard=program::CollateralGuard {owner:trader.owner,config:trader.config,bump,
            floors:std::array::from_fn(|index|{let slot=trader.slots[index];program::CollateralFloor {nonce:slot.nonce,
                value:slot.entry_value,cursor:1,closed_at:slot.closed_at,exit:slot.exit,covered_at:slot.closed_at+1,incomplete:false,
                #[cfg(feature="mainnet")] entry_price:1_000_000_000_000}})};
        let mut data=Vec::new();guard.try_serialize(&mut data).unwrap();
        self.svm.set_account(guard_key,Account {lamports:self.svm.minimum_balance_for_rent_exemption(data.len()),
            data,owner:owner_program,executable:false,rent_epoch:0}).unwrap();
    }
}

#[test]
fn real_base_custody_swaps_receipts_rewards_fifo_and_withdrawals() {
    run_settlement_fixture(".cache/acidpump-er-base-fixture.json", "reports/acidpump-er-base-verification.json", None);
}
#[test]
fn token_2022_custody_swaps_receipts_rewards_fifo_and_withdrawals() {
    run_settlement_fixture(".cache/acidpump-er-token-2022-fixture.json", "reports/acidpump-er-token-2022-verification.json", None);
}
#[test]
fn partial_loss_awards_ownership_for_swap_and_consumed_backing_once() {
    run_settlement_fixture(".cache/acidpump-backing-base-fixture.json", ".cache/acidpump-backing-reward-full.json", Some(50_000_000_000));
}
#[test]
fn exhausted_token_2022_backing_awards_only_tokens_actually_contributed() {
    run_settlement_fixture(".cache/acidpump-backing-token-2022-fixture.json", ".cache/acidpump-backing-reward-exhausted.json", Some(1_000_000));
}
fn run_settlement_fixture(fixture_file: &str, report_file: &str, backing: Option<u64>) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let fixture: Value = serde_json::from_slice(&std::fs::read(root.join(fixture_file)).expect("Generate the base and token-2022 fixtures first")).unwrap();
    let mut svm = LiteSVM::new();
    for row in fixture["accounts"].as_array().unwrap() {
        let data: Vec<u8> = row["data"].as_array().unwrap().iter().map(|v| v.as_u64().unwrap() as u8).collect();
        svm.set_account(key(row,"key"), Account { lamports: row["lamports"].as_u64().unwrap().max(svm.minimum_balance_for_rent_exemption(data.len())),
            owner: key(row,"owner"), data, executable: false, rent_epoch: 0 }).unwrap();
    }
    for (id,file) in [(program::ID,"target/deploy/acidpump_er.so"),(pump::ID,".cache/acidpump-pump.so"),(pump::FEES_ID,".cache/acidpump-fees.so")] {
        svm.add_program(id, &std::fs::read(root.join(file)).expect("Build SBF binaries before integration test")).unwrap();
    }
    let signers: Vec<Keypair> = (1..=3).map(|n| Keypair::new_from_array([n;32])).collect();
    for signer in &signers { svm.airdrop(&signer.pubkey(), 100_000_000_000).unwrap(); }
    svm.airdrop(&key(&fixture,"authority"),100_000_000_000).unwrap();
    let mut h = Harness { svm, signers, fixture, receipts: vec![], rejected: 0 };
    for step in h.fixture["boot"].as_array().unwrap().clone() { h.send(step["name"].as_str().unwrap(), instructions(&step["ixs"]), false); }
    let alice = h.signers[1].pubkey(); let bob = h.signers[2].pubkey();
    let config = key(&h.fixture,"config"); let pool = key(&h.fixture,"pool");
    let usd = h.fixture["assets"][0].clone(); let sdice = h.fixture["assets"][1].clone(); let acid = h.fixture["assets"][2].clone();
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    h.svm.set_account(magic,Account { lamports:1_000_000,data:vec![],owner:pk("NativeLoader1111111111111111111111111111111"),executable:true,rent_epoch:0 }).unwrap();
    h.send("native ER cannot initialize base custody",vec![h.initialize_custody(alice)],true);
    h.svm.set_account(magic,Account { lamports:0,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0 }).unwrap();
    for owner in [alice,bob] {
        h.send("initialize isolated custody",vec![h.initialize_custody(owner)],false);
        let before = token(&h.svm,key(&usd,if owner == alice {"aliceToken"} else {"bobToken"}));
        h.send("deposit actual USD collateral",vec![h.deposit(owner,&usd,100_000_000)],false);
        assert_eq!(before-token(&h.svm,key(&usd,if owner == alice {"aliceToken"} else {"bobToken"})),100_000_000);
    }
    let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
    h.svm.set_account(magic,Account { lamports:1_000_000,data:vec![],owner:pk("NativeLoader1111111111111111111111111111111"),executable:true,rent_epoch:0 }).unwrap();
    h.send("native ER cannot mint base custody credits",vec![h.deposit(alice,&usd,1_000_000)],true);
    h.svm.set_account(magic,Account { lamports:0,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0 }).unwrap();
    h.send("deposit actual SDICE collateral",vec![h.deposit(alice,&sdice,20_000_000_000)],false);
    if let Some(tokens)=backing { h.send("deposit actual backing tokens",vec![h.deposit(alice,&acid,tokens)],false); }
    assert_eq!(token(&h.svm,key(&usd,"vault")),200_000_000);
    assert_eq!(read::<Inventory>(&h.svm,key(&usd,"inventory")).locked,200_000_000);
    let mut wrong_deposit = h.deposit(alice,&usd,1_000_000); wrong_deposit.accounts[0].pubkey = bob;
    h.send("another wallet cannot debit custody",vec![wrong_deposit],true);
    let price = 100_000_000_000_000;
    let now = h.fixture["now"].as_i64().unwrap();
    let pending = |nonce,asset,margin,exit,leverage,rank,liquidated| ErSlot { state:ER_PENDING,kind:ER_TRADE,nonce,asset,market:0,margin,
        entry_value:10_000_000,entry:price,bust:math::bust(price,leverage,true).unwrap(),leverage,long:true,opened_at:now-5,opened_source_slot:100,
        cursor:1,bust_observed:liquidated,exit,closed_at:now,close_seq:rank,liquidated };
    let mut a = ErTrader { owner:alice,config,pool,next_nonce:5,..ErTrader::default() };
    a.slots[0]=pending(1,key(&usd,"key"),10_000_000,100_100_000_000_000,100,1,false);
    a.slots[1]=pending(2,key(&usd,"key"),10_000_000,99_900_000_000_000,100,0,false);
    a.slots[2]=pending(3,key(&sdice,"key"),10_000_000_000,99_000_000_000_000,1_000,0,true);
    // Produce the full-loss intent through owner-authorized gap recovery at a
    // favorable current price, then exercise its real base PumpSwap settlement.
    a.slots[2].state = ER_OPEN; a.slots[2].bust_observed = false; a.slots[2].liquidated = false;
    let mut gap_risk = ErRisk { config, ..ErRisk::default() };
    gap_risk.markets[0] = ErMarket { sequence: 66, price: 101_000_000_000_000,
        prices: [101_000_000_000_000; 64], publish_time: now, source_slot: 100, ..ErMarket::default() };
    forfeit_history_gap_slot(&mut a, &gap_risk, 2, 3, now + 60, alice,
        program::oracle::OraclePrice { price: 101_000_000_000_000, publish_time: now, posted_slot: 101 }, now).unwrap();
    a.slots[3]=ErSlot { state:ER_PENDING,kind:ER_WITHDRAW,nonce:4,asset:key(&usd,"key"),margin:5_000_000,closed_at:now,..ErSlot::default() };
    if let Some(tokens)=backing {
        a.core_stake.tokens=tokens;
        a.slots[2]=pending(3,key(&sdice,"key"),10_000_000_000,99_980_000_000_000,1_000,0,false);
        a.slots[2].entry_value=20_000_000;
    }
    let mut b = ErTrader { owner:bob,config,pool,next_nonce:2,..ErTrader::default() };
    b.slots[0]=pending(1,key(&usd,"key"),10_000_000,100_100_000_000_000,100,2,false);
    h.commit_boundary(&a,program::ID);
    h.send("uncommitted program-owned trader is rejected",vec![h.prepare(alice,1,0,key(&usd,"key"))],true);
    h.commit_boundary(&a,DELEGATION_OWNER); h.commit_boundary(&b,DELEGATION_OWNER);
    h.send("wrong committed nonce is rejected",vec![h.prepare(alice,99,0,key(&usd,"key"))],true);
    let mut wrong_owner = a.clone(); wrong_owner.owner = bob;
    let mut bytes=Vec::new(); wrong_owner.try_serialize(&mut bytes).unwrap();
    let akey=pda(&[b"er-trader",alice.as_ref()]); let original=h.svm.get_account(&akey).unwrap();
    let mut corrupt=original.clone(); corrupt.data=bytes; h.svm.set_account(akey,corrupt).unwrap();
    h.send("committed bytes cannot impersonate another owner",vec![h.prepare(alice,1,0,key(&usd,"key"))],true);
    h.svm.set_account(akey,original).unwrap();
    let forged=Pubkey::new_unique(); let copy=h.svm.get_account(&akey).unwrap(); h.svm.set_account(forged,copy).unwrap();
    let mut forged_prepare=h.prepare(alice,1,0,key(&usd,"key")); forged_prepare.accounts[6].pubkey=forged;
    h.send("forged DLP snapshot at another address is rejected",vec![forged_prepare],true);
    h.send("second winner cannot jump first winner",h.settlement("bob_win",0),true);
    let before=token(&h.svm,key(&usd,"aliceToken"));
    h.send("withdrawal proceeds before unrelated winning settlement",vec![h.withdrawal(alice,&usd,4,3)],false);
    assert_eq!(token(&h.svm,key(&usd,"aliceToken"))-before,5_000_000);
    assert_eq!(read::<Config>(&h.svm,config).next_settlement_seq,1);
    h.send("withdrawal receipt prevents replay",vec![h.withdrawal(alice,&usd,4,3)],true);
    let mut failing=h.settlement("alice_loss",1);
    let close=failing.last_mut().unwrap(); let end=close.data.len(); close.data[end-24..end-16].copy_from_slice(&u64::MAX.to_le_bytes());
    h.send("late swap failure rolls back stage, token CPIs and receipt",failing,true);
    assert!(h.svm.get_account(&key(&h.fixture["settlements"]["alice_loss"],"position")).is_none());
    h.send("permissionless USD loss runs actual swaps",h.settlement("alice_loss",1),false);
    let loss:Position=read(&h.svm,key(&h.fixture["settlements"]["alice_loss"],"position"));
    assert_eq!((loss.loss,loss.returned),(1_000_000,9_000_000));
    let c:ErCustody=read(&h.svm,pda(&[b"er-custody",alice.as_ref()]));
    assert_eq!((c.assets[0].available,c.assets[0].returned_credits),(94_000_000,9_000_000));
    assert!(c.receipts[1].settled); assert_eq!(c.receipts[1].nonce,2);
    assert_eq!(read::<Config>(&h.svm,config).next_settlement_seq,1);
    h.send("settlement receipt rejects repeat nonce",h.settlement("alice_loss",1),true);
    h.send("stake actual awarded ownership",vec![wire(&h.fixture["rewards"]["stake"])],false);
    let supply_before=u64::from_le_bytes(h.svm.get_account(&key(&acid,"mint")).unwrap().data[36..44].try_into().unwrap());
    let acid_vault_before=token(&h.svm,key(&acid,"vault"));
    let ownership_key=pda(&[b"ownership-pool",pool.as_ref()]);
    let ownership_before:OwnershipPool=read(&h.svm,ownership_key);
    h.send("owner gap forfeiture converts actual SDICE with rewards and burn",h.settlement("alice_liquidation",2),false);
    let x:Position=read(&h.svm,key(&h.fixture["settlements"]["alice_liquidation"],"position"));
    assert_eq!((x.state,x.loss,x.returned),if backing.is_some(){(1,2_000_000_000,8_000_000_000)}else{(2,10_000_000_000,0)});
    assert_eq!(x.profit, 0);
    assert_eq!(x.exit, if backing.is_some(){99_980_000_000_000}else{101_000_000_000_000});
    let supply_after=u64::from_le_bytes(h.svm.get_account(&key(&acid,"mint")).unwrap().data[36..44].try_into().unwrap());
    assert!(supply_after<supply_before,"real SPL burn must reduce mint supply");
    let burned=supply_before-supply_after;
    let received=token(&h.svm,key(&acid,"vault"))-acid_vault_before+burned;
    let ownership_after:OwnershipPool=read(&h.svm,ownership_key);
    let stakers=ownership_after.reward_pool+ownership_after.fee_pending
        -ownership_before.reward_pool-ownership_before.fee_pending;
    assert_eq!(stakers as u128,received as u128*7/98,"real swap output funds the new 7% staker share");
    assert_eq!(burned,received/98,"real SPL burn follows the new 1% share");
    assert_eq!(x.swapped_out+stakers+burned,received,"pool retains 90% and all split rounding");
    let receipt:OwnershipReceipt=read(&h.svm,pda(&[b"ownership-receipt",key(&h.fixture["settlements"]["alice_liquidation"],"position").as_ref()]));
    assert!(receipt.awarded && receipt.pool_tokens>0 && receipt.issued>0);
    assert_eq!(receipt.pool_tokens,x.swapped_out+x.coverage_tokens);
    assert_eq!(receipt.issued,receipt.pool_tokens as u128*100,"both actual contributions earn the flat emission rate");
    if let Some(tokens)=backing {
        assert!(x.coverage_tokens>0 && x.coverage_tokens<=tokens);
        let custody:ErCustody=read(&h.svm,pda(&[b"er-custody",alice.as_ref()]));
        assert_eq!(custody.core_coverage.debited_tokens,x.coverage_tokens);
        if tokens==1_000_000 {assert_eq!(x.coverage_tokens,tokens);assert!(x.uncovered_usd>0);}
        else {assert_eq!(x.uncovered_usd,0);assert!(x.coverage_tokens<tokens);}
        let rewards=ownership_after.supply;
        h.send("backing settlement cannot mint twice",h.settlement("alice_liquidation",2),true);
        assert_eq!(read::<OwnershipPool>(&h.svm,ownership_key).supply,rewards);
        assert_eq!(read::<ErCustody>(&h.svm,pda(&[b"er-custody",alice.as_ref()])).core_coverage.debited_tokens,x.coverage_tokens);
    }
    assert!(read::<GasReserve>(&h.svm,pda(&[b"gas-reserve"])).sol_received>0);
    let rewards_before=token(&h.svm,key(&acid,"aliceToken"));
    h.send("claim real staking reward tokens",vec![wire(&h.fixture["rewards"]["claim"])],false);
    assert!(token(&h.svm,key(&acid,"aliceToken"))>rewards_before);
    let custody_before_stage=h.svm.get_account(&pda(&[b"er-custody",alice.as_ref()])).unwrap().data;
    let inventory_before_stage=h.svm.get_account(&key(&usd,"inventory")).unwrap().data;
    h.send("prepare alone is retryable without financial mutation",vec![h.prepare(alice,1,0,key(&usd,"key"))],false);
    assert_eq!(h.svm.get_account(&pda(&[b"er-custody",alice.as_ref()])).unwrap().data,custody_before_stage);
    assert_eq!(h.svm.get_account(&key(&usd,"inventory")).unwrap().data,inventory_before_stage);
    assert_eq!(read::<Config>(&h.svm,config).next_settlement_seq,1);
    // Historical collateral floors are ignored by the replacement model.
    let guard_key=pda(&[b"er-collateral-guard",alice.as_ref()]);
    let mut guard:program::CollateralGuard=read(&h.svm,guard_key);
    guard.floors[0].value=2_000_000;
    let mut held=h.svm.get_account(&guard_key).unwrap();
    let mut data=vec![];guard.try_serialize(&mut data).unwrap();held.data=data;
    h.svm.set_account(guard_key,held.clone()).unwrap();
    let mut forged=held.clone();forged.owner=program::ID;h.svm.set_account(guard_key,forged).unwrap();
    // Historical floor ownership and value no longer affect a backed close.
    h.svm.set_account(guard_key,held).unwrap();
    h.send("first winning committed intent settles",h.settlement("alice_win",0),false);
    h.send("second winning committed intent settles",h.settlement("bob_win",0),false);
    assert_eq!(read::<Config>(&h.svm,config).next_settlement_seq,3);
    let awin=key(&h.fixture["settlements"]["alice_win"],"position"); let bwin=key(&h.fixture["settlements"]["bob_win"],"position");
    let x:Position=read(&h.svm,awin); let y:Position=read(&h.svm,bwin);
    assert!(x.profit>0 && y.profit>0); assert_eq!((x.queue_seq,y.queue_seq),(0,1));
    assert_eq!(x.close_value,x.entry_value);
    assert_eq!(x.profit,math::pnl(x.entry_value,x.entry,x.exit,x.leverage,x.long).unwrap().1);
    assert_eq!(x.exit,a.slots[0].exit,"base Feed is zero; the frozen ER price must be used");
    a.slots[1]=pending(5,key(&usd,"key"),5_000_000,price,100,0,false);
    a.next_nonce=6; h.commit_boundary(&a,DELEGATION_OWNER);
    let mut unpaid=vec![h.recycle(alice,1,5)]; unpaid.extend(h.later_settlement(alice,5,1));
    h.send("unpaid winning claim cannot be recycled",unpaid,true);
    h.send("payout cannot skip FIFO head",instructions(&h.fixture["settlements"]["bob_win"]["payout"]),true);
    let usd_before=token(&h.svm,key(&usd,"aliceToken"));
    h.send("first actual USDC payout",instructions(&h.fixture["settlements"]["alice_win"]["payout"]),false);
    assert!(token(&h.svm,key(&usd,"aliceToken"))>usd_before);
    assert_eq!(read::<Position>(&h.svm,awin).remaining,0);
    h.send("second actual USDC payout",instructions(&h.fixture["settlements"]["bob_win"]["payout"]),false);
    assert_eq!(read::<Position>(&h.svm,bwin).remaining,0);
    let mut initialized=vec![h.recycle(alice,2,1)]; initialized.extend(h.settlement("alice_win",0));
    h.send("initialized destination cannot extract sponsored rent",initialized,true);
    let old_position=key(&h.fixture["settlements"]["alice_loss"],"position");
    let old_receipt=pda(&[b"ownership-receipt",old_position.as_ref()]);
    let mut surplus=h.svm.get_account(&old_position).unwrap(); surplus.lamports+=1;
    h.svm.set_account(old_position,surplus.clone()).unwrap();
    let mut recycle=vec![h.recycle(alice,2,5)]; recycle.extend(h.later_settlement(alice,5,1));
    h.send("surplus rent cannot be extracted through recycling",recycle.clone(),true);
    surplus.lamports-=1; h.svm.set_account(old_position,surplus).unwrap();
    let executor=h.signers[0].pubkey(); let executor_before=h.svm.get_account(&executor).unwrap().lamports;
    let owner_before=h.svm.get_account(&alice).unwrap().lamports;
    h.send("permissionless rent recycling atomically funds next settlement",recycle,false);
    let fee=h.receipts.last().unwrap()["feeLamports"].as_u64().unwrap();
    assert_eq!(executor_before-h.svm.get_account(&executor).unwrap().lamports,fee,"executor must only pay network fee; rent fully reinvested");
    assert_eq!(owner_before,h.svm.get_account(&alice).unwrap().lamports,"owner does not sign or receive sponsored rent");
    assert!(h.svm.get_account(&old_position).is_none()); assert!(h.svm.get_account(&old_receipt).is_none());
    let next_position=pda(&[b"position",alice.as_ref(),&5u64.to_le_bytes()]);
    assert_eq!(read::<Position>(&h.svm,next_position).state,1);
    assert!(read::<OwnershipReceipt>(&h.svm,pda(&[b"ownership-receipt",next_position.as_ref()])).awarded);
    let final_pool:Pool=read(&h.svm,pool); assert_eq!((final_pool.queue_head,final_pool.queue_tail,final_pool.queue_usd),(2,2,0));
    let ac:ErCustody=read(&h.svm,pda(&[b"er-custody",alice.as_ref()])); let bc:ErCustody=read(&h.svm,pda(&[b"er-custody",bob.as_ref()]));
    assert_eq!((ac.assets[0].available,ac.assets[0].returned_credits),(94_000_000,24_000_000));
    assert_eq!((bc.assets[0].available,bc.assets[0].returned_credits),(100_000_000,10_000_000));
    assert_eq!(token(&h.svm,key(&usd,"vault")),ac.assets[0].available+bc.assets[0].available);
    assert_eq!(read::<Inventory>(&h.svm,key(&usd,"inventory")).locked,194_000_000);
    assert_eq!(token(&h.svm,key(&sdice,"vault")),read::<Inventory>(&h.svm,key(&sdice,"inventory")).locked);
    let reserve:Inventory=read(&h.svm,key(&acid,"inventory")); let rewards:OwnershipPool=read(&h.svm,pda(&[b"ownership-pool",pool.as_ref()]));
    assert_eq!(token(&h.svm,key(&acid,"vault")),reserve.locked+reserve.treasury+reserve.dust+rewards.fee_pending+rewards.reward_pool+rewards.dev_fees);
    // Exercise the collateral's own token program on withdrawal too, not only
    // the classic USDC payout route used by both fixtures above.
    h.send("deposit collateral for withdrawal",vec![h.deposit(alice,&sdice,1_000_000)],false);
    a.slots[2]=ErSlot {state:ER_PENDING,kind:ER_WITHDRAW,nonce:6,asset:key(&sdice,"key"),margin:1_000_000,closed_at:now,..ErSlot::default()};
    a.next_nonce=7;h.commit_boundary(&a,DELEGATION_OWNER);
    let wallet_before=token(&h.svm,key(&sdice,"aliceToken"));
    h.send("withdraw actual collateral with its token program",vec![h.withdrawal(alice,&sdice,6,2)],false);
    assert_eq!(token(&h.svm,key(&sdice,"aliceToken"))-wallet_before,1_000_000);
    assert_eq!(h.rejected,15+usize::from(backing.is_some()));
    let report=json!({"environment":h.fixture["environment"],"program":program::ID.to_string(),"rejections":h.rejected,
        "custodyUsd":194_000_000,"winningClaimsPaid":2,"realPumpSwap":true,"realRewardTransfer":true,"realMintBurn":true,
        "ownerGapForfeitureFullLoss":backing.is_none(),"backingRewardVerified":backing.is_some(),
        "backingTokens":backing,"rentRecycledAtomically":true,"transactions":h.receipts});
    std::fs::write(root.join(report_file),serde_json::to_string_pretty(&report).unwrap()).unwrap();
}
