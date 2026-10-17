//! Manifest-bound mainnet identities and asset validation.
//! Each production deployment has its own program, pool and ownership balances.
use crate::*;

pub use crate::deployment_config::{ADMIN,PUBLISHER,PRIMARY_MINT,PRIMARY_POOL,
    MAX_ENTRY_USD,MAX_WALLET_NOTIONAL_USD};

pub fn initialize(admin: Pubkey, publisher: Pubkey, quote: Pubkey, quote_program: Pubkey) -> Result<()> {
    require_keys_eq!(admin, ADMIN, MixError::Unauthorized);
    require_keys_eq!(publisher, PUBLISHER, MixError::Binding);
    require!(quote == mainnet_pump::USDC && quote_program == anchor_spl::token::ID, MixError::Binding);
    Ok(())
}

/// Historical private deployments required administrator admission.
/// The production policy allows public wallet creation; exits remain unrestricted.
pub fn admit(config: &Config, payer: Pubkey) -> Result<()> {
    if crate::deployment_config::REQUIRE_WALLET_ADMISSION {require_keys_eq!(payer, config.admin, MixError::Unauthorized);}
    Ok(())
}

pub fn validate_asset(a: &Asset) -> Result<()> {
    require_keys_eq!(a.source_mint, a.mint, MixError::Binding);
    require!(a.source_pool == a.dex_pool && (a.quote_mint == mainnet_pump::SOL || a.quote_mint == mainnet_pump::USDC), MixError::Binding);
    if a.id == 102 || a.mint == PRIMARY_MINT {
        require!(a.id == 102 && a.mint == PRIMARY_MINT && a.dex_pool == PRIMARY_POOL
            && a.token_program == crate::deployment_config::PRIMARY_TOKEN_PROGRAM && a.quote_mint == mainnet_pump::SOL, MixError::Binding);
    } else if a.id == 103 || a.mint == mainnet_pump::SOL {
        require!(a.id == 103 && a.mint == mainnet_pump::SOL && a.decimals == 9
            && a.token_program == anchor_spl::token::ID && a.dex_pool == Pubkey::default() && a.quote_mint == mainnet_pump::SOL, MixError::Binding);
    } else {
        // USDC has a dedicated initializer; generic assets must have a real route.
        require!(a.id >= 100 && a.id != 104 && a.mint != mainnet_pump::USDC
            && a.dex_pool != Pubkey::default(), MixError::Binding);
    }
    Ok(())
}

pub fn validate_primary(a: &Asset) -> Result<()> {
    require!(a.id == 102 && a.mint == PRIMARY_MINT && a.dex_pool == PRIMARY_POOL, MixError::Binding);
    validate_asset(a)
}

#[cfg(all(test,feature="mainnet"))]
mod tests {
    use super::*;
    #[test]
    fn compiled_identities_match_the_explicit_deployment_manifest() {
        let manifest: serde_json::Value = serde_json::from_str(crate::deployment_config::MANIFEST_JSON).unwrap();
        for (field,key) in [("programId",crate::ID),("adminAuthority",ADMIN),("publisher",PUBLISHER),
            ("acidpumpMint",PRIMARY_MINT),("quoteMint",mainnet_pump::USDC)] {
            assert_eq!(manifest[field].as_str().unwrap(),key.to_string());
        }
        assert_eq!(manifest["routePlan"]["primary"]["pool"].as_str().unwrap(),PRIMARY_POOL.to_string());
        assert_eq!(manifest["er"]["validator"].as_str().unwrap(),engine::ER_VALIDATOR.to_string());
        assert_eq!(manifest["launchTransition"]["reuseProgramId"],false);
    }
    #[test]
    fn admission_obeys_the_compiled_launch_policy() {
        let config=Config {admin:ADMIN,publisher:PUBLISHER,quote_mint:mainnet_pump::USDC,paused:true,bump:0,next_settlement_seq:0};
        assert!(admit(&config,ADMIN).is_ok());
        assert_eq!(admit(&config,Pubkey::new_unique()).is_err(),crate::deployment_config::REQUIRE_WALLET_ADMISSION);
    }
    #[test]
    fn compiled_risk_policy_does_not_confuse_production_and_private_deployments() {
        assert_eq!(risk::check_entry(MAX_WALLET_NOTIONAL_USD/1000,1000,0).unwrap(),MAX_WALLET_NOTIONAL_USD);
        assert!(risk::check_entry(risk::USD,1,MAX_WALLET_NOTIONAL_USD).is_err());
        if crate::deployment_config::ROLLING_CAPACITY {
            assert!(risk::check_entry(101*risk::USD,1000,0).is_ok());
            assert_eq!(risk::refreshed_cap(risk::OI_HEADROOM,risk::OI_HEADROOM).unwrap(),2*risk::OI_HEADROOM);
        } else {
            assert!(risk::check_entry(MAX_ENTRY_USD+1,1,0).is_err());
            for oi in [0,risk::OI_HEADROOM/2,risk::OI_HEADROOM,u64::MAX] {
                assert_eq!(risk::refreshed_cap(oi,u64::MAX).unwrap(),risk::OI_HEADROOM);
            }
        }
    }
}
