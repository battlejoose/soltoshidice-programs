//! Local administrative signatures are simulated; no real role key is read.
//! Every platform account below is created by the compiled contract itself.
use super::*;

impl Bank {
    pub(super) fn bootstrap(&mut self) {
        let admin=p::mainnet_config::ADMIN;
        let owner=self.signer.pubkey();
        let system_program=Pubkey::default();
        let token_program=anchor_spl::token::ID;
        let magic=ephemeral_rollups_sdk::consts::MAGIC_PROGRAM_ID;
        let primary=self.tokens[0];let usd=self.tokens[1];let sol=self.tokens[2];
        assert!(self.svm.get_account(&self.config).is_none());
        assert!(self.svm.get_account(&self.pool).is_none());
        let initialize=ix(p::accounts::Initialize{admin,config:self.config,quote_mint:USDC,system_program,
            execution:p::accounts::BaseExecution{magic_program:magic}},
            p::instruction::Initialize{publisher:p::mainnet_config::PUBLISHER});
        let mut stranger=initialize.clone();stranger.accounts[0].pubkey=owner;
        self.send("stranger cannot initialize",vec![stranger],true);
        let mut unsigned=initialize.clone();unsigned.accounts[0].is_signer=false;
        self.admin("missing administrator signature rejected",vec![unsigned],true);
        let mut wrong=initialize.clone();wrong.data=p::instruction::Initialize{publisher:owner}.data();
        self.admin("wrong publisher rejected",vec![wrong],true);
        let mut wrong=initialize.clone();wrong.accounts[2].pubkey=primary.mint;
        self.admin("wrong quote rejected",vec![wrong],true);
        let original_magic=self.svm.get_account(&magic).unwrap_or_default();
        self.svm.set_account(magic,Account{lamports:1_000_000,data:vec![],owner:pk("NativeLoader1111111111111111111111111111111"),executable:true,rent_epoch:0}).unwrap();
        self.admin("ER bank cannot initialize base config",vec![initialize.clone()],true);
        self.svm.set_account(magic,original_magic).unwrap();
        self.admin("initialize pinned mainnet configuration",vec![initialize],false);
        let config:Config=get(&self.svm,self.config);
        assert!(config.paused);assert_eq!(config.admin,admin);assert_eq!(config.publisher,p::mainnet_config::PUBLISHER);
        assert_eq!(config.quote_mint,USDC);assert_eq!(config.next_settlement_seq,1);

        for id in [102u16,104,103,0,1,2,3] {
            let feed=pda(&[b"feed",&id.to_le_bytes()]);
            self.admin("initialize canonical feed",vec![ix(p::accounts::CreateFeed{admin,config:self.config,feed,system_program},
                p::instruction::CreateFeed{id,symbol:[0;12]})],false);
        }
        for a in [primary,sol] {
            let register=ix(p::accounts::RegisterAsset{admin,config:self.config,feed:a.feed,asset:a.asset,mint:a.mint,
                token_program:a.program,valuation:self.valuation(a),system_program},
                p::instruction::RegisterAsset{source_mint:a.mint,source_pool:a.pool,collateral_enabled:true});
            let mut wrong=register.clone();wrong.data=p::instruction::RegisterAsset{
                source_mint:USDC,source_pool:a.pool,collateral_enabled:true}.data();
            self.admin("wrong source rejected",vec![wrong],true);
            self.admin("register real primary or native SOL",vec![register],false);
            let stored:Asset=get(&self.svm,a.asset);
            assert_eq!(stored.source_mint,a.mint);assert_eq!(stored.source_pool,a.pool);
            assert_eq!(stored.dex_pool,a.pool);assert_eq!(stored.token_program,a.program);
        }
        self.admin("register canonical USDC",vec![ix(p::accounts::RegisterQuoteCollateral{
            admin,config:self.config,feed:usd.feed,mint:USDC,token_program,asset:usd.asset,system_program},
            p::instruction::RegisterQuoteCollateral{})],false);
        let pool=ix(p::accounts::CreatePool{admin,config:self.config,pool:self.pool,settlement:primary.asset,
            authority:self.authority,quote_mint:USDC,quote_program:token_program,quote_vault:self.usd_vault,system_program},
            p::instruction::CreatePool{id:0});
        let mut wrong=pool.clone();wrong.accounts[3].pubkey=usd.asset;
        self.admin("wrong primary rejected",vec![wrong],true);
        self.admin("create SDICE payout pool and USDC vault",vec![pool],false);
        self.admin("create SOL intermediate vault",vec![ix(p::accounts::InitializeSolIntermediate{admin,config:self.config,
            pool:self.pool,authority:self.authority,mint:SOL,token_program,sol_vault:self.sol_vault,system_program},
            p::instruction::InitializeSolIntermediate{})],false);
        for a in self.tokens {
            self.send("create inventory and custody vault",vec![ix(p::accounts::CreateInventory{payer:owner,pool:self.pool,
                authority:self.authority,asset:a.asset,mint:a.mint,token_program:a.program,inventory:a.inventory,vault:a.vault,system_program},
                p::instruction::CreateInventory{})],false);
            let er_quote=pda(&[b"er-quote",a.asset.as_ref()]);
            self.admin("initialize collateral quote and history",vec![
                ix(p::accounts::InitializeErQuote{admin,config:self.config,asset:a.asset,er_quote,system_program},p::instruction::ErInitializeQuote{}),
                ix(p::accounts::InitializeCollateralHistory{admin,config:self.config,asset:a.asset,
                    history:pda(&[b"er-collateral-history",a.asset.as_ref()]),system_program},p::instruction::ErInitializeCollateralHistory{})],false);
        }
        for id in 0..p::ACTIVE_MARKET_COUNT {
            let feed=pda(&[b"feed",&id.to_le_bytes()]);let market=pda(&[b"market",feed.as_ref()]);
            self.admin("initialize core market",vec![ix(p::accounts::CreateMarket{admin,config:self.config,pool:self.pool,
                feed,market,system_program},p::instruction::CreateMarket{})],false);
        }
        self.admin("initialize risk ownership and ER fee payer",vec![
            ix(p::accounts::InitializeErRisk{admin,config:self.config,er_risk:pda(&[b"er-risk"]),system_program},p::instruction::ErInitializeRisk{}),
            ix(p::accounts::InitializeOwnershipPool{admin,config:self.config,pool:self.pool,ownership_pool:self.ownership,system_program},p::instruction::InitializeOwnershipPool{}),
            ix(p::accounts::InitializeErFeePayer{admin,config:self.config,er_fee_payer:pda(&[b"er-fee-payer"]),system_program},p::instruction::ErInitializeFeePayer{lamports:1_000_000})],false);
        self.admin("initialize gas reserve and vault",vec![
            ix(p::accounts::InitializeGas{admin,config:self.config,gas_reserve:self.gas,system_program},p::instruction::InitializeGas{}),
            ix(p::accounts::CreateGasVault{admin,config:self.config,pool:self.pool,authority:self.authority,gas_mint:SOL,gas_vault:self.gas_vault,token_program,system_program},p::instruction::CreateGasVault{})],false);
        let custody=ix(p::accounts::InitializeCustody{payer:admin,owner,config:self.config,pool:self.pool,custody:self.custody,
            system_program,execution:p::accounts::BaseExecution{magic_program:magic}},p::instruction::InitializeCustody{});
        let mut self_admission=custody.clone();self_admission.accounts[0].pubkey=owner;
        if p::deployment_config::REQUIRE_WALLET_ADMISSION {
            self.send("wallet cannot admit itself",vec![self_admission],true);
            self.admin("admit test wallet with administrator approval",vec![custody],false);
        }else{
            self.send("public wallet initializes its own custody",vec![self_admission],false);
        }
        self.send("initialize admitted trader and collateral guard",vec![
            ix(p::accounts::InitializeErTrader{payer:owner,owner,config:self.config,pool:self.pool,er_trader:self.trader,er_custody:self.custody,system_program},p::instruction::ErInitializeTrader{}),
            ix(p::accounts::InitializeCollateralGuard{payer:owner,owner,config:self.config,guard:self.guard,system_program},p::instruction::ErInitializeCollateralGuard{}),
            ix(p::accounts::InitializeExposure{payer:owner,owner,exposure:pda(&[b"exposure",owner.as_ref()]),system_program},p::instruction::InitializeExposure{})],false);
        let risk:ErRisk=get(&self.svm,pda(&[b"er-risk"]));
        assert!(risk.markets.iter().all(|m|m.long_cap==p::risk::OI_HEADROOM&&m.short_cap==p::risk::OI_HEADROOM));
        self.send("deposit rejected while paused",vec![self.deposit(usd,UNIT)],true);
        self.admin("enable restricted mainnet test",vec![ix(p::accounts::Admin{admin,config:self.config},p::instruction::Pause{paused:false})],false);
        // Seed small unrelated vault balances by real token transfers. Settlement
        // must preserve these; it can spend only each route's own proceeds.
        self.send("seed unrelated intermediate balances",vec![
            anchor_spl::token::spl_token::instruction::transfer(&token_program,&sol.wallet,&self.sol_vault,&owner,&[],10_000).unwrap(),
            anchor_spl::token::spl_token::instruction::transfer(&token_program,&usd.wallet,&self.usd_vault,&owner,&[],100).unwrap()],false);
    }
}
