//! New policy and reserve withdrawal execute against the actual candidate SBF.
use super::*;

#[test]
fn economics_v1_preserves_old_entries_and_prices_new_entries_without_deadband() {
    for legacy in [true,false] {
        let mut h=Bank::new();let usd=h.tokens[1];let owner=h.signer.pubkey();
        let economics=pda(&[b"economics-v1"]);
        let policy:p::EconomicsV1=get(&h.svm,economics);assert!(!policy.deadband);
        h.send("fund profit-policy fixture",vec![h.deposit(usd,20*UNIT)],false);
        let mut clock:Clock=h.svm.get_sysvar();clock.unix_timestamp=policy.activated_at+11;h.svm.set_sysvar(&clock);
        h.f["now"]=json!(clock.unix_timestamp);
        let mut intent=h.committed(usd,4,10*UNIT,false,true);
        if legacy {intent.opened_at=policy.activated_at-1;}
        let mut trader:ErTrader=get(&h.svm,h.trader);trader.slots[0]=intent;
        put(&mut h.svm,h.trader,&trader,p::basebridge::DELEGATION_OWNER);
        h.send("settle according to immutable entry-time policy",h.settlement(usd,intent,1),false);
        let position:Position=get(&h.svm,pda(&[b"position",owner.as_ref(),&4u64.to_le_bytes()]));
        let expected=if legacy {p::math::pnl(position.close_value,intent.entry,intent.exit,intent.leverage,true).unwrap().1}
            else {p::economics::winning_profit(position.close_value,intent.entry,intent.exit,intent.leverage,false).unwrap().net};
        assert_eq!(position.profit,expected);assert_eq!(position.returned,intent.margin);assert_eq!(position.loss,0);
        assert_eq!(get::<Pool>(&h.svm,h.pool).queue_usd,expected);
        h.admin("policy cannot be reinitialized to change outstanding terms",vec![ix(p::accounts::InitializeEconomicsV1{
            admin:p::mainnet_config::ADMIN,config:h.config,economics,system_program:Pubkey::default(),
            execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},
            p::instruction::InitializeEconomicsV1{deadband:true})],true);
        let after:p::EconomicsV1=get(&h.svm,economics);assert_eq!(after.activated_at,policy.activated_at);assert!(!after.deadband);
    }
}

#[test]
fn queued_claim_gets_all_post_gas_loss_receipts_and_no_new_rewards_or_burns() {
    let mut h=Bank::new();let primary=h.tokens[0];let usd=h.tokens[1];let owner=h.signer.pubkey();
    for (a,n) in [(primary,100_000*UNIT),(usd,20*UNIT)] {h.send("fund queued settlement fixture",vec![h.deposit(a,n)],false);}
    let win=h.committed(usd,4,10*UNIT,false,true);
    h.send("create a real unpaid winning claim",h.settlement(usd,win,1),false);
    let claim_key=pda(&[b"position",owner.as_ref(),&4u64.to_le_bytes()]);
    let claim:Position=get(&h.svm,claim_key);let pool_before:Pool=get(&h.svm,h.pool);assert!(pool_before.queue_usd>0);
    let loss=h.committed(primary,6,10_000*UNIT,true,false);
    let rewards:OwnershipPool=get(&h.svm,h.ownership);let inventory:Inventory=get(&h.svm,primary.inventory);
    let supply_before=supply(&h.svm,primary.mint);let gas:GasReserve=get(&h.svm,h.gas);
    h.send("queued loss retains 99 percent and still converts 1 percent to SOL",h.settlement(primary,loss,1),false);
    let after:OwnershipPool=get(&h.svm,h.ownership);
    assert_eq!(after.fee_pending,rewards.fee_pending);assert_eq!(after.reward_pool,rewards.reward_pool);
    assert_eq!(supply(&h.svm,primary.mint),supply_before);
    assert_eq!(get::<Inventory>(&h.svm,primary.inventory).treasury-inventory.treasury,loss.margin-loss.margin/100);
    assert!(get::<GasReserve>(&h.svm,h.gas).sol_received>gas.sol_received);
    let queue:Pool=get(&h.svm,h.pool);assert_eq!(queue.queue_usd,pool_before.queue_usd);assert_eq!(queue.queue_head,pool_before.queue_head);
    let untouched:Position=get(&h.svm,claim_key);assert_eq!(untouched.profit,claim.profit);assert_eq!(untouched.remaining,claim.remaining);
}

#[test]
fn surplus_withdrawal_requires_admin_and_preserves_rent_floor_and_user_vaults() {
    let mut h=Bank::new();let recipient=Pubkey::new_unique();
    h.svm.set_account(recipient,Account{lamports:1,data:vec![],owner:Pubkey::default(),executable:false,rent_epoch:0}).unwrap();
    let mut reserve:GasReserve=get(&h.svm,h.gas);reserve.sol_received=5_000_000_000;reserve.sol_spent=0;
    put(&mut h.svm,h.gas,&reserve,p::ID);
    let mut data=h.svm.get_account(&h.gas).unwrap();data.lamports+=6_000_000_000;h.svm.set_account(h.gas,data).unwrap();
    let make=|admin,amount|ix(p::accounts::WithdrawGasSurplus{admin,config:h.config,gas_reserve:h.gas,recipient,
        execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},
        p::instruction::WithdrawGasSurplus{amount});
    let good=make(p::mainnet_config::ADMIN,4_000_000_000);
    let stranger=make(h.signer.pubkey(),1);let excessive=make(p::mainnet_config::ADMIN,5_000_000_001);
    let zero=make(p::mainnet_config::ADMIN,0);let mut unsigned=good.clone();unsigned.accounts[0].is_signer=false;
    let balances:Vec<_>=h.tokens.iter().map(|a|token(&h.svm,a.vault)).collect();
    h.send("non-admin cannot withdraw fee surplus",vec![stranger],true);
    h.admin("missing surplus authority signature rejected",vec![unsigned],true);
    h.admin("surplus cannot take seed funding or operating floor",vec![excessive],true);
    h.admin("zero surplus withdrawal rejected",vec![zero],true);
    let before=h.svm.get_account(&h.gas).unwrap().lamports;
    h.admin("withdraw exactly four SOL from earned fee surplus",vec![good.clone()],false);
    assert_eq!(h.svm.get_account(&h.gas).unwrap().lamports,before-4_000_000_000);
    assert_eq!(h.svm.get_account(&recipient).unwrap().lamports,4_000_000_001);
    assert_eq!(get::<GasReserve>(&h.svm,h.gas).sol_spent,4_000_000_000);
    assert_eq!(h.tokens.iter().map(|a|token(&h.svm,a.vault)).collect::<Vec<_>>(),balances);
    h.admin("cannot repeat a withdrawal beyond remaining earned surplus",vec![good],true);
}

#[test]
fn setup_revenue_requires_real_admin_funding_and_obeys_the_withdrawal_floor() {
    let mut h=Bank::new();let amount=50_000_000;
    let deposit=ix(p::accounts::DepositGasRevenue{admin:p::mainnet_config::ADMIN,config:h.config,gas_reserve:h.gas,
        system_program:Pubkey::default(),execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID}},
        p::instruction::DepositGasRevenue{amount});
    let before=h.svm.get_account(&h.gas).unwrap().lamports;let received=get::<GasReserve>(&h.svm,h.gas).sol_received;
    let mut wrong=deposit.clone();wrong.accounts[0].pubkey=h.signer.pubkey();
    h.send("stranger cannot relabel reserve funding as earnings",vec![wrong],true);
    h.admin("credit actual reconciled market setup surplus",vec![deposit],false);
    assert_eq!(h.svm.get_account(&h.gas).unwrap().lamports,before+amount);
    assert_eq!(get::<GasReserve>(&h.svm,h.gas).sol_received,received+amount);
    let rent=h.svm.minimum_balance_for_rent_exemption(h.svm.get_account(&h.gas).unwrap().data.len());
    assert_eq!(p::fee_surplus::available(rent+1_000_000_000,rent,received+amount,0),0);
}
