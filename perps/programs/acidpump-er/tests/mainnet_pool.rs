//! Quotation against captured mainnet accounts and independent Pump SDK outputs.
//! No RPC, private keys or chain writes. Execution coverage lives separately.
use acidpump_er::{mainnet_pump::{self, SolPoolAccounts, SolPoolQuote}, pump};
use anchor_lang::prelude::{AccountInfo, Pubkey};
use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::Value;
use std::{collections::BTreeMap, str::FromStr};

fn pk(value: &str) -> Pubkey { Pubkey::from_str(value).unwrap() }
struct Row { key: Pubkey, owner: Pubkey, data: Vec<u8>, lamports: u64, executable: bool }
struct Fixture { pool: Pubkey, mint: Pubkey, base: Pubkey, quote: Pubkey, rows: BTreeMap<Pubkey, Row>, source: Value }
impl Fixture {
    fn load(payout: bool) -> Self {
        let source: Value = serde_json::from_str(if payout {
            include_str!("../../../projects/acidpump-er/src/fixtures/mainnet-payout-pool.json")
        } else { include_str!("../../../projects/acidpump-er/src/fixtures/mainnet-sdice-pool.json") }).unwrap();
        let snapshot = &source["snapshot"];
        let identity = if payout { snapshot } else { &source["audit"] };
        let pool = pk(identity["pool"].as_str().unwrap());
        let mint = pk(identity["mint"].as_str().unwrap());
        let rows: BTreeMap<_,_> = snapshot["accounts"].as_object().unwrap().iter().map(|(address, row)| {
            let key = pk(address);
            (key, Row { key, owner: pk(row["owner"].as_str().unwrap()), data: STANDARD.decode(row["data"].as_str().unwrap()).unwrap(),
                lamports: row["lamports"].as_u64().unwrap(), executable: row["executable"].as_bool().unwrap() })
        }).collect();
        let data = &rows[&pool].data;
        Self { pool, mint, base: Pubkey::new_from_array(data[139..171].try_into().unwrap()),
            quote: Pubkey::new_from_array(data[171..203].try_into().unwrap()), rows, source }
    }
    fn quote(&mut self) -> anchor_lang::Result<SolPoolQuote> {
        self.read_quote(false)
    }
    fn read_quote(&mut self, collateral: bool) -> anchor_lang::Result<SolPoolQuote> {
        let global = Pubkey::find_program_address(&[b"global_config"], &pump::ID).0;
        let fees = Pubkey::find_program_address(&[b"fee_config",pump::ID.as_ref()], &pump::FEES_ID).0;
        let infos: BTreeMap<_,_> = self.rows.values_mut().map(|r| (r.key, AccountInfo::new(&r.key, false, false,
            &mut r.lamports, &mut r.data, &r.owner, r.executable))).collect();
        let accounts = SolPoolAccounts { pool: &infos[&self.pool], mint: &infos[&self.mint], base_vault: &infos[&self.base],
            quote_vault: &infos[&self.quote], global: &infos[&global], fees: &infos[&fees] };
        if collateral { accounts.validate_collateral(self.pool,self.mint) } else { accounts.validate(self.pool,self.mint) }
    }
}

#[test]
fn empty_collateral_can_be_valued_but_never_swapped_or_forged() {
    for drain_base in [false,true] {
        let mut f=Fixture::load(false);
        let vault=if drain_base {f.base} else {f.quote};
        f.rows.get_mut(&vault).unwrap().data[64..72].fill(0);
        assert!(f.quote().is_err(),"empty pool cannot admit a swap");
        let q=f.read_quote(true).unwrap();
        assert_eq!(q.collateral_lamports(1_000_000_000).unwrap(),0);
        assert_eq!(q.maximum_sell(u64::MAX).unwrap(),0);
        assert!(q.sell(1).is_err());
        f.rows.get_mut(&vault).unwrap().data[32..64].fill(0);
        assert!(f.read_quote(true).is_err(),"zero value must not hide forged vault authority");
    }
    let mut f=Fixture::load(false);let reserve=f.quote().unwrap().real_quote;
    f.rows.get_mut(&f.pool).unwrap().data[271..279].copy_from_slice(&reserve.to_le_bytes());
    assert_eq!(f.read_quote(true).unwrap().collateral_lamports(1_000_000_000).unwrap(),0);
    assert!(f.quote().is_err());
    f.rows.get_mut(&f.pool).unwrap().data[271..279].copy_from_slice(&(reserve+1).to_le_bytes());
    assert!(f.read_quote(true).is_err(),"fee underflow remains invalid even for closing collateral");
}

#[test]
fn captured_sdice_quotes_match_official_sdk() {
    let mut f = Fixture::load(false);
    let q = f.quote().unwrap();
    assert_eq!(q.fees, [20,5,95]);
    assert_eq!(q.virtual_quote, 17_584_505_464);
    for row in f.source["audit"]["sells"].as_array().unwrap() {
        let amount = row["sdiceTokens"].as_u64().unwrap() * 1_000_000;
        assert_eq!(q.sell(amount).unwrap().to_string(), row["quoteLamportsAfterFees"].as_str().unwrap());
        assert_eq!(q.sell_usd_micro(amount,110_000_000_000_000).unwrap(), q.sell(amount).unwrap() * 110 / 1000);
    }
    for row in f.source["audit"]["buys"].as_array().unwrap() {
        assert_eq!(q.buy(row["inputLamports"].as_u64().unwrap()).unwrap().to_string(),row["sdiceBaseUnits"].as_str().unwrap());
    }
}

#[test]
fn historical_pool_layouts_preserve_old_tokens_and_reject_partial_fields() {
    for size in [211,243,244,245,261,269,270,271,279,287,300] {
        let mut f = Fixture::load(false);
        f.rows.get_mut(&f.pool).unwrap().data.truncate(size);
        let q = f.quote().unwrap();
        assert_eq!(q.virtual_quote,if size < 261 {0} else {17_584_505_464});
        assert_eq!(q.fees[2],if size < 243 {0} else {95});
        assert!(q.sell(1_000_000_000).unwrap() > 0);
    }
    for size in [210,212,242,246,260,262,268,272,278,280,286] {
        let mut f = Fixture::load(false);
        f.rows.get_mut(&f.pool).unwrap().data.truncate(size);
        assert!(f.quote().is_err(),"partial pool layout {size}");
    }
}

#[test]
fn mayhem_uses_fixed_supply_and_cashback_preserves_net_quotation() {
    let mut regular = Fixture::load(false);
    regular.rows.get_mut(&regular.mint).unwrap().data[36..44].copy_from_slice(&1_000_000_000_000_000u64.to_le_bytes());
    let reference = regular.quote().unwrap();
    for cashback in [false,true] {
        let mut f = Fixture::load(false);
        let data = &mut f.rows.get_mut(&f.pool).unwrap().data;
        data[243] = 1; data[244] = u8::from(cashback);
        f.rows.get_mut(&f.mint).unwrap().data[36..44].copy_from_slice(&1_000_000_000_000_000_000u64.to_le_bytes());
        let q = f.quote().unwrap();
        assert_eq!(q.fees,reference.fees);
        assert_eq!(q.sell(1_000_000_000).unwrap(),reference.sell(1_000_000_000).unwrap());
        assert_eq!(q.buy(100_000_000).unwrap(),reference.buy(100_000_000).unwrap());
    }
    for flag in [243,244,269,270] {
        let mut f = Fixture::load(false); f.rows.get_mut(&f.pool).unwrap().data[flag] = 2;
        assert!(f.quote().is_err(),"non-boolean flag {flag}");
    }
}

#[test]
fn retained_fee_buckets_are_not_executable_collateral_reserves() {
    let mut f = Fixture::load(false); let original = f.quote().unwrap();
    let bucket = original.real_quote - 1;
    let data = &mut f.rows.get_mut(&f.pool).unwrap().data;
    data[271..279].copy_from_slice(&(bucket/2).to_le_bytes());
    data[279..287].copy_from_slice(&(bucket-bucket/2).to_le_bytes());
    let q = f.quote().unwrap();
    assert_eq!(q.real_quote,1);assert_eq!(q.effective_quote().unwrap(),original.effective_quote().unwrap());
    assert_eq!(q.fees,original.fees);assert!(q.sell(1_000_000_000).is_err());
    assert_eq!(q.collateral_lamports(1_000_000_000).unwrap(),0);
    let data = &mut f.rows.get_mut(&f.pool).unwrap().data;
    data[271..279].copy_from_slice(&(original.real_quote+1).to_le_bytes());data[279..287].fill(0);
    assert!(f.quote().is_err());
    let data = &mut f.rows.get_mut(&f.pool).unwrap().data;
    data[271..279].copy_from_slice(&u64::MAX.to_le_bytes());data[279..287].copy_from_slice(&1u64.to_le_bytes());
    assert!(f.quote().is_err());
}

#[cfg(feature="mainnet")]
#[test]
fn stake_coverage_uses_the_captured_sdice_full_size_swap_curve() {
    use acidpump_er::stake_backing::{coverage_tokens, shortfall};
    let mut fixture = Fixture::load(false);
    let pool = fixture.quote().unwrap();
    let price = 110_000_000_000_000;
    let stake = 100_000 * 1_000_000;
    let mut evaluations = 0;
    let full = pool.collateral_usd_micro(stake, price).unwrap();
    assert!(full > 1_000_000);
    for closing in [full, full / 2, 0] {
        let target = shortfall(full, closing);
        let result = coverage_tokens(stake, target, &mut |amount| {
            evaluations += 1;
            pool.collateral_usd_micro(amount, price)
        }).unwrap();
        assert_eq!(result.uncovered_usd, 0);
        assert!(result.tokens <= stake);
        assert!(pool.collateral_usd_micro(result.tokens, price).unwrap() >= target);
        assert_eq!(result.quoted_usd, pool.collateral_usd_micro(result.tokens, price).unwrap());
    }
    assert!(evaluations < 140, "bounded local calculations; no account reads per search step");
}

#[test]
fn real_usdc_route_handles_negative_virtual_reserve_and_issuer_authority() {
    let mut f = Fixture::load(true);
    let q = f.quote().unwrap();
    let raw_virtual=i128::from_le_bytes(f.rows[&f.pool].data[245..261].try_into().unwrap());
    let vault=u64::from_le_bytes(f.rows[&f.quote].data[64..72].try_into().unwrap());
    assert!(raw_virtual < 0);
    assert_eq!(q.effective_quote().unwrap() as i128,vault as i128+raw_virtual);
    assert_eq!(q.fees,[25,5,0]);
    for row in f.source["quotes"]["examples"].as_array().unwrap() {
        assert_eq!(q.buy(row["solLamports"].as_str().unwrap().parse().unwrap()).unwrap().to_string(),row["usdcMicroUnits"].as_str().unwrap());
    }
}

#[test]
fn malformed_and_substituted_accounts_are_rejected_without_panics() {
    let f = Fixture::load(false);
    let keys = [f.pool,f.mint,f.base,f.quote,
        Pubkey::find_program_address(&[b"global_config"], &pump::ID).0,
        Pubkey::find_program_address(&[b"fee_config",pump::ID.as_ref()], &pump::FEES_ID).0];
    for key in keys {
        let mut f = Fixture::load(false);
        f.rows.get_mut(&key).unwrap().owner = Pubkey::default();
        assert!(f.quote().is_err(),"wrong owner {key}");
        let mut f = Fixture::load(false);
        f.rows.get_mut(&key).unwrap().data.truncate(7);
        assert!(f.quote().is_err(),"truncated {key}");
    }
    for (is_mint,offset) in [(false,0),(false,32),(false,108),(true,0),(true,46)] {
        let mut f = Fixture::load(false);
        let address = if is_mint { f.mint } else { f.base };
        f.rows.get_mut(&address).unwrap().data[offset] ^= 1;
        assert!(f.quote().is_err(),"mint={is_mint} offset={offset}");
    }
}

#[test]
fn real_liquidity_and_integer_limits_are_enforced() {
    let q = Fixture::load(false).quote().unwrap();
    assert!(q.sell(q.base * 1000).is_err());
    for lamports in [0,1,2,3] { assert_eq!(q.buy(lamports).unwrap(),0); }
    assert!(SolPoolQuote {virtual_quote: i128::MAX,..q}.effective_quote().is_err());
    assert!(SolPoolQuote {virtual_quote: -(q.real_quote as i128),..q}.effective_quote().is_err());
    assert!(SolPoolQuote {fees: [10_000,0,0],..q}.effective_quote().is_err());
    assert!(mainnet_pump::sol_to_usd_micro(100,0).is_err());
    assert_eq!(mainnet_pump::sol_to_usd_micro(2_000_000_000,110_000_000_000_000).unwrap(),220_000_000);
    assert!(mainnet_pump::sol_to_usd_micro(u64::MAX,1_000_000_000_000_000_000).is_err());
}

#[test]
fn exact_liquidity_inverse_matches_independent_search_and_integer_boundaries() {
    for base in [1u64,97,1_000_000_000,u64::MAX] {
        for real_quote in [1u64,101,100_000_000_000,u64::MAX] {
            for effective in [1u64,103,120_000_000_000,u64::MAX] {
                for lp in [0u16,20,500,9999] {
                    let q=SolPoolQuote{base,real_quote,virtual_quote:effective as i128-real_quote as i128,fees:[lp,0,0]};
                    let allowed=|amount:u64| {
                        let gross=amount as u128*effective as u128/(base as u128+amount as u128);
                        let fee=(gross*lp as u128+9999)/10_000;
                        gross-fee<=real_quote as u128
                    };
                    for available in [0u64,1,999,1_000_000_000,u64::MAX] {
                        let mut lo=0u64;let mut hi=available;
                        while lo<hi {let mid=lo+(hi-lo)/2+(hi-lo)%2;if allowed(mid){lo=mid;}else{hi=mid-1;}}
                        let actual=q.maximum_sell(available).unwrap();assert_eq!(actual,lo,"{q:?}, available={available}");
                        assert!(allowed(actual));if actual<available {assert!(!allowed(actual+1));}
                    }
                }
            }
        }
    }
}

#[test]
fn fee_tier_boundaries_and_creator_overrides_match_pool_configuration() {
    let mut f = Fixture::load(false);
    let fees = Pubkey::find_program_address(&[b"fee_config",pump::ID.as_ref()], &pump::FEES_ID).0;
    let data = &f.rows[&fees].data;
    let count = u32::from_le_bytes(data[65..69].try_into().unwrap()) as usize;
    for i in 0..count {
        let at = 69 + i * 40;
        let threshold = u128::from_le_bytes(data[at..at+16].try_into().unwrap());
        let expected = std::array::from_fn(|n| u64::from_le_bytes(data[at+16+8*n..at+24+8*n].try_into().unwrap()) as u16);
        assert_eq!(mainnet_pump::sol_fees(data,true,threshold).unwrap(),expected);
        if i > 0 { assert_eq!(mainnet_pump::sol_fees(data,true,threshold-1).unwrap(),mainnet_pump::sol_fees(data,true,u128::from_le_bytes(data[at-40..at-24].try_into().unwrap())).unwrap()); }
    }
    let global = Pubkey::find_program_address(&[b"global_config"], &pump::ID).0;
    f.rows.get_mut(&global).unwrap().data[940] = 1;
    f.rows.get_mut(&f.pool).unwrap().data[261..269].copy_from_slice(&42u64.to_le_bytes());
    assert_eq!(f.quote().unwrap().fees,[20,5,42]);
    f.rows.get_mut(&f.pool).unwrap().data[211..243].fill(0);
    assert_eq!(f.quote().unwrap().fees,[20,5,0]);
    f.rows.get_mut(&global).unwrap().data[56] |= 8;
    assert!(f.quote().is_err());
}
