//! Candidate SBF, real token transfers, no public-network transactions.
use super::*;
fn contribution(h:&Bank,nonce:u64,min_issued:u128)->Instruction {
    let t=h.tokens[0];let owner=h.signer.pubkey();
    ix(p::accounts::ContributePool {
        payer:owner,owner,funding_authority:owner,config:h.config,pool:h.pool,asset:t.asset,
        authority:h.authority,mint:t.mint,token_program:t.program,source:t.wallet,vault:t.vault,
        inventory:t.inventory,ownership_pool:h.ownership,
        ownership_trader:pda(&[b"ownership-trader",h.pool.as_ref(),owner.as_ref()]),
        ledger:pda(&[b"pool-contribution",h.pool.as_ref(),owner.as_ref()]),valuation:h.valuation(t),
        execution:p::accounts::BaseExecution{magic_program:ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID},
        system_program:Pubkey::default(),
    },p::instruction::ContributePool{amount:100*UNIT,nonce,min_issued,expires:h.f["now"].as_i64().unwrap()+60})
}
#[test]
fn contribution_moves_full_principal_and_awards_double_loss_reward_without_replay() {
    for high in [false,true] {for queued in [false,true] {
        let mut h=Bank::new();let t=h.tokens[0];let owner=h.signer.pubkey();
        if high {super::pool_tokenomics::seed_high_pool(&mut h,t);}
        if queued {let mut pool:Pool=get(&h.svm,h.pool);pool.queue_usd=1;pool.queue_tail=1;put(&mut h.svm,h.pool,&pool,p::ID);}
        let amount=100*UNIT;let expected=amount as u128*if high {20}else{200}*if queued {2}else{1};
        let before_wallet=token(&h.svm,t.wallet);let before_vault=token(&h.svm,t.vault);
        let before_treasury=get::<Inventory>(&h.svm,t.inventory).treasury;
        let before_rewards:OwnershipPool=get(&h.svm,h.ownership);
        let before_mint=supply(&h.svm,t.mint);
        h.send("reject insufficient contribution reward atomically",vec![contribution(&h,0,expected+1)],true);
        h.send("contribution sends entire principal to pool",vec![contribution(&h,0,expected)],false);
        assert_eq!(token(&h.svm,t.wallet),before_wallet-amount);
        assert_eq!(token(&h.svm,t.vault),before_vault+amount);
        assert_eq!(get::<Inventory>(&h.svm,t.inventory).treasury,before_treasury+amount);
        assert_eq!(supply(&h.svm,t.mint),before_mint);
        let rewards:OwnershipPool=get(&h.svm,h.ownership);
        assert_eq!(rewards.supply,before_rewards.supply+expected);
        assert_eq!(rewards.fee_pending,before_rewards.fee_pending);
        assert_eq!(rewards.reward_pool,before_rewards.reward_pool);
        let ledger:p::ContributionLedger=get(&h.svm,pda(&[b"pool-contribution",h.pool.as_ref(),owner.as_ref()]));
        assert_eq!((ledger.next_nonce,ledger.contributed,ledger.issued),(1,amount as u128,expected));
        let recipient:OwnershipTrader=get(&h.svm,pda(&[b"ownership-trader",h.pool.as_ref(),owner.as_ref()]));
        assert_eq!(recipient.available,expected);
        h.send("reject duplicate contribution nonce",vec![contribution(&h,0,expected)],true);
        h.send("next contribution uses next nonce",vec![contribution(&h,1,expected)],false);
        if queued {
            let mut pool:Pool=get(&h.svm,h.pool);pool.queue_usd=0;pool.queue_head=pool.queue_tail;
            put(&mut h.svm,h.pool,&pool,p::ID);
            let balance=token(&h.svm,t.wallet);
            h.send("cleared FIFO rejects stale higher reward minimum without transferring",vec![contribution(&h,2,expected)],true);
            assert_eq!(token(&h.svm,t.wallet),balance);
            h.send("fresh unqueued contribution earns twice base rewards",vec![contribution(&h,2,expected/2)],false);
        }
    }}
}
#[test]
fn contribution_rejects_wrong_pool_asset_source_authority_and_missing_consent() {
    for field in ["pool","asset","source","funding","owner"] {
        let mut h=Bank::new();let mut instruction=contribution(&h,0,1);
        match field {
            "pool"=>instruction.accounts[4].pubkey=Pubkey::new_unique(),
            "asset"=>instruction.accounts[5].pubkey=h.tokens[1].asset,
            "source"=>instruction.accounts[9].pubkey=h.tokens[0].vault,
            "funding"=>{instruction.accounts[2].pubkey=p::mainnet_config::ADMIN;instruction.accounts[2].is_signer=false;},
            _=>{instruction.accounts[1].pubkey=p::mainnet_config::ADMIN;instruction.accounts[1].is_signer=false;},
        }
        h.send(field,vec![instruction],true);
    }
}
