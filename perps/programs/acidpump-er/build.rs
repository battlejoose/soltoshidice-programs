use std::{env,fs,path::PathBuf};
use serde_json::Value;

fn main() {
    println!("cargo:rerun-if-env-changed=ACIDPUMP_BUILD_MANIFEST");
    let root=PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let mainnet=env::var_os("CARGO_FEATURE_MAINNET").is_some();
    let legacy_test=env::var_os("CARGO_FEATURE_MAINNET_TEST").is_some();
    let file=env::var("ACIDPUMP_BUILD_MANIFEST").unwrap_or_else(|_| {
        assert!(!mainnet || legacy_test,"Production builds require an explicit ACIDPUMP_BUILD_MANIFEST");
        "deployments/acidpump-mainnet-test.json".into()
    });
    let file=root.join(file).canonicalize().expect("mainnet build manifest is missing");
    println!("cargo:rerun-if-changed={}",file.display());
    let raw=fs::read_to_string(&file).expect("cannot read build manifest");
    let m:Value=serde_json::from_str(&raw).expect("invalid build manifest");
    let text=|key:&str|m[key].as_str().unwrap_or_else(||panic!("missing manifest {key}"));
    assert_eq!(text("cluster"),"mainnet-beta");
    assert!(["mainnet-test","mainnet-launch"].contains(&text("stage")));
    assert_eq!(text("secondaryTokenModel"),"contract-issued-pool-local-ownership-balances");
    assert!(m.get("apumpMint").is_none_or(Value::is_null),"secondary ownership is not an SPL mint");
    let mut keys=std::collections::BTreeSet::new();
    for field in ["programId","adminAuthority","publisher","sponsor","upgradeAuthority","deployer"] {
        assert!(keys.insert(text(field)),"deployment identities must be separate");
        assert!(!["8TwyqkVNZTHzBF6JC3k91KvWac6862K81TAzfiuiSfxc","11111111111111111111111111111111"].contains(&text(field)),"invalid mainnet identity");
    }
    assert_eq!(text("quoteMint"),"EPjFWdd5AufqSSqeM2qN1xzybapC8G4wEGGkZwyTDt1v");
    assert_eq!(m["routePlan"]["primary"]["mint"].as_str(),Some(text("acidpumpMint")));
    assert_eq!(m["collateralBacking"]["stakeMint"].as_str(),Some(text("acidpumpMint")));
    let limits=&m["contractLimits"];
    let production=text("stage")=="mainnet-launch";
    if production {
        let policy_file=root.join("projects/acidpump-er/src/production-policy.json");
        println!("cargo:rerun-if-changed={}",policy_file.display());
        let policy:Value=serde_json::from_str(&fs::read_to_string(policy_file).expect("Missing production policy")).unwrap();
        assert_eq!(limits,&policy,"Production manifest must explicitly select the shared production policy");
    }
    let micro=|key:&str| {
        let dollars=limits[key].as_u64().unwrap_or_else(||panic!("missing integer {key}"));
        assert!(dollars>0); dollars.checked_mul(1_000_000).expect("USD cap overflow")
    };
    let admission=limits["requireWalletAdmission"].as_bool().expect("explicit wallet admission policy required");
    if text("stage")=="mainnet-test" {assert!(admission,"test wallet admission must remain enabled");}
    let mut code=String::new();
    for (name,value) in [("ADMIN",text("adminAuthority")),("PUBLISHER",text("publisher")),
        ("PRIMARY_MINT",text("acidpumpMint")),("PRIMARY_POOL",m["routePlan"]["primary"]["pool"].as_str().expect("primary pool")),
        ("PRIMARY_TOKEN_PROGRAM",m["routePlan"]["primary"]["tokenProgram"].as_str().expect("primary token program")),
        ("VALIDATOR",m["er"]["validator"].as_str().expect("validator"))] {
        // Rust string escaping prevents configuration text becoming source code.
        code.push_str(&format!("pub const {name}: Pubkey = pubkey!({value:?});\n"));
    }
    let values=[
        ("MIN_ENTRY_USD",if production {micro("minimumEntryUsd")} else {1_000_000}),
        ("MAX_ENTRY_USD",if production {u64::MAX} else {micro("maximumEntryUsd")}),
        ("MAX_POSITION_NOTIONAL_USD",if production {micro("maximumPositionNotionalUsd")} else {micro("maximumWalletNotionalUsd")}),
        ("MAX_WALLET_NOTIONAL_USD",micro("maximumWalletNotionalUsd")),
        ("MARKET_HEADROOM_USD",micro(if production {"marketHeadroomUsd"} else {"maximumMarketSideNotionalUsd"})),
        ("CAP_REFRESH_SECONDS",if production {limits["marketCapacityRefreshSeconds"].as_u64().unwrap()} else {30}),
        ("CAP_HYSTERESIS_BPS",if production {limits["marketCapacityHysteresisBps"].as_u64().unwrap()} else {1000}),
        ("MAX_LEVERAGE",if production {limits["maximumLeverage"].as_u64().unwrap()} else {1000}),
    ];
    for (name,value) in values {
        code.push_str(&format!("pub const {name}: u64 = {value};\n"));
    }
    code.push_str(&format!("pub const ROLLING_CAPACITY: bool = {production};\n"));
    code.push_str(&format!("pub const REQUIRE_WALLET_ADMISSION: bool = {admission};\npub const MANIFEST_JSON: &str = {raw:?};\n"));
    let out=PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out.join("launch_constants.rs"),code).unwrap();
    if mainnet {fs::write(out.join("program_identity.rs"),format!("declare_id!({:?});\n",text("programId"))).unwrap();}
}
