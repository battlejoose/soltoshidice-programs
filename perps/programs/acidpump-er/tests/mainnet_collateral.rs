//! Mainnet collateral denomination, source ordering and low-water accounting.
//! This is contract logic coverage; native ER execution is tested separately.
use acidpump_er::{mainnet_pump::SolPoolQuote, MainnetCollateralContext, MainnetCollateralObservation};

const SOL: u64 = 1_000_000_000;
const PRICE: u64 = 110_000_000_000_000;
#[test]
fn renamed_quote_conversion_preserves_the_deployed_forty_byte_wire_layout() {
    use anchor_lang::{AnchorSerialize,AnchorDeserialize,Space};
    let c=observation().context;
    let mut expected=vec![];
    for v in [100*SOL,500,PRICE,100,900] {expected.extend_from_slice(&v.to_le_bytes());}
    assert_eq!(MainnetCollateralContext::INIT_SPACE,40);
    let mut actual=vec![];c.serialize(&mut actual).unwrap();assert_eq!(actual,expected);
    assert_eq!(MainnetCollateralContext::try_from_slice(&expected).unwrap(),c);
}
#[test]
fn usdc_quotes_return_exact_micro_usd_without_sol_repricing() {
    let o=observation();
    let c=MainnetCollateralContext{usd_per_quote_atom_e21:1_000_000_000_000_000,..o.context};
    let q=SolPoolQuote{base:o.base_reserve,real_quote:c.real_quote_reserve,virtual_quote:o.quote_reserve as i128-c.real_quote_reserve as i128,fees:o.fee_bps};
    for amount in [1_000_000,1_000_000_000,10_000_000_000] {
        assert_eq!(c.value(o.base_reserve,o.quote_reserve,o.fee_bps,o.publish_time,amount).unwrap(),q.sell(amount).unwrap());
    }
    assert_eq!(c.spot_price(o.base_reserve,o.quote_reserve,100,6).unwrap(),110_000_000_000);
}
fn observation() -> MainnetCollateralObservation {
    MainnetCollateralObservation {base_reserve:1_000_000_000_000, quote_reserve:110*SOL,
        fee_bps:[20,5,95], publish_time:100, observation:1,
        context:MainnetCollateralContext {real_quote_reserve:100*SOL,pool_source_slot:500,
            usd_per_quote_atom_e21:PRICE,conversion_publish_time:100,conversion_source_slot:900} }
}

#[test]
fn collateral_is_usd_after_real_sol_sale_fees_and_virtual_reserves() {
    for virtual_quote in [-10i128,0,10] {
        let mut o=observation();o.quote_reserve=(100+virtual_quote) as u64*SOL;
        let q=SolPoolQuote {base:o.base_reserve,real_quote:o.context.real_quote_reserve,
            virtual_quote:virtual_quote*SOL as i128,fees:o.fee_bps};
        for amount in [1_000_000,1_000_000_000,10_000_000_000,100_000_000_000] {
            let expected=q.sell(amount).unwrap() as u128*PRICE as u128/1_000_000_000_000_000;
            assert_eq!(o.context.value(o.base_reserve,o.quote_reserve,o.fee_bps,o.publish_time,amount).unwrap(),expected as u64);
        }
    }
}

#[test]
fn zero_or_unexecutable_bags_latch_zero_but_invalid_data_is_rejected() {
    let o=observation();let amount=o.base_reserve*1000;
    assert_eq!(o.context.value(o.base_reserve,o.quote_reserve,o.fee_bps,100,amount).unwrap(),0);
    assert_eq!(o.context.value(0,o.quote_reserve,o.fee_bps,100,amount).unwrap(),0);
    let empty=MainnetCollateralContext {real_quote_reserve:0,..o.context};
    assert_eq!(empty.value(o.base_reserve,10*SOL,o.fee_bps,100,amount).unwrap(),0);
    assert_eq!(o.context.value(o.base_reserve,o.quote_reserve,o.fee_bps,100,1).unwrap(),0);
    for context in [MainnetCollateralContext{usd_per_quote_atom_e21:0,..o.context},
        MainnetCollateralContext{usd_per_quote_atom_e21:u64::MAX,..o.context},
        MainnetCollateralContext{conversion_publish_time:96,..o.context},
        MainnetCollateralContext{conversion_publish_time:101,..o.context},
        MainnetCollateralContext{conversion_source_slot:0,..o.context},
        MainnetCollateralContext{pool_source_slot:0,..o.context}] {
        assert!(context.value(o.base_reserve,o.quote_reserve,o.fee_bps,100,1_000_000).is_err());
    }
    assert!(o.context.value(o.base_reserve,0,o.fee_bps,100,1_000_000).is_err());
    assert!(o.context.value(o.base_reserve,o.quote_reserve,[10_000,0,0],100,1_000_000).is_err());
}

#[cfg(feature="mainnet")]
mod mainnet {
    use super::*;
    use ::acidpump_er::*;
    use anchor_lang::prelude::*;

    fn quote() -> ErQuote {ErQuote{config:Pubkey::new_unique(),asset:Pubkey::new_unique(),..ErQuote::default()}}
    fn history(q:&ErQuote) -> CollateralHistory {CollateralHistory{config:q.config,asset:q.asset,
        decimals:6,
        sequence:0,source_slot:0,publish_time:0,last_gap:0,last_gap_start:0,is_quote:false,
        samples:vec![CollateralSample::default();HISTORY],bump:0}}

    #[test]
    fn observations_have_distinct_chain_slots_and_idempotent_source_content() {
        let o=observation();let mut q=quote();q.publish_mainnet(o,100).unwrap();
        let before=q.try_to_vec_for_test();
        let next=MainnetCollateralObservation{observation:2,publish_time:101,..o};
        let bad=[MainnetCollateralObservation{observation:1,..next},
            MainnetCollateralObservation{publish_time:99,..next},
            MainnetCollateralObservation{context:MainnetCollateralContext{pool_source_slot:499,..o.context},..next},
            MainnetCollateralObservation{context:MainnetCollateralContext{conversion_source_slot:899,..o.context},..next},
            MainnetCollateralObservation{base_reserve:o.base_reserve-1,..next},
            MainnetCollateralObservation{quote_reserve:o.quote_reserve-1,..next},
            MainnetCollateralObservation{fee_bps:[20,5,0],..next},
            MainnetCollateralObservation{context:MainnetCollateralContext{real_quote_reserve:90*SOL,..o.context},..next},
            MainnetCollateralObservation{context:MainnetCollateralContext{usd_per_quote_atom_e21:PRICE/2,..o.context},..next}];
        for invalid in bad {assert!(q.publish_mainnet(invalid,101).is_err());assert_eq!(q.try_to_vec_for_test(),before);}
        // SOL can move without a pool write, and a pool can move without a new oracle slot.
        q.publish_mainnet(MainnetCollateralObservation{context:MainnetCollateralContext{
            usd_per_quote_atom_e21:PRICE/2,conversion_source_slot:901,conversion_publish_time:101,..o.context},..next},101).unwrap();
        q.publish_mainnet(MainnetCollateralObservation{observation:3,publish_time:102,quote_reserve:90*SOL,
            context:MainnetCollateralContext{pool_source_slot:501,..q.mainnet},..o},102).unwrap();
        assert_eq!(q.source_slot,3);assert_eq!(q.mainnet.pool_source_slot,501);assert_eq!(q.mainnet.conversion_source_slot,901);
    }

    trait QuoteBytes {fn try_to_vec_for_test(&self)->Vec<u8>;}
    impl QuoteBytes for ErQuote {fn try_to_vec_for_test(&self)->Vec<u8>{let mut out=vec![];self.try_serialize(&mut out).unwrap();out}}

    #[test]
    fn delayed_dip_uses_historical_sol_and_cannot_rebound_or_reprice_frozen_exit() {
        let o=observation();let mut q=quote();q.publish_mainnet(o,100).unwrap();
        let mut h=history(&q);h.record(&q).unwrap();let owner=Pubkey::new_unique();
        let amount=1_000_000_000;
        let mut trader=ErTrader{owner,config:q.config,..ErTrader::default()};
        trader.slots[0]=ErSlot{state:ER_OPEN,kind:ER_TRADE,nonce:1,asset:q.asset,margin:amount,
            entry_value:q.value(amount,100).unwrap(),opened_source_slot:1,..ErSlot::default()};
        let mut guard=CollateralGuard{owner,config:q.config,floors:CollateralGuard::empty_floors(),bump:0};
        guard.start(&trader,&h,0).unwrap();
        trader.slots[0].state=ER_PENDING;trader.slots[0].closed_at=102;
        trader.slots[0].exit=101_000_000_000_000;trader.slots[0].close_seq=1;guard.freeze(&trader,0).unwrap();
        // Replayed at time 110, but exchange rate and observation are from 101.
        let dip=MainnetCollateralObservation{publish_time:101,observation:2,
            context:MainnetCollateralContext{usd_per_quote_atom_e21:PRICE/2,conversion_source_slot:901,conversion_publish_time:101,..o.context},..o};
        q.publish_mainnet(dip,110).unwrap();h.record(&q).unwrap();
        assert!(q.value(amount,110).is_err(),"historical replay must not open fresh trades");
        let low=trader.slots[0].entry_value/2;guard.observe(&trader,&h).unwrap();
        assert_eq!(guard.floors[0].value,low);assert!(guard.settled_value(&trader,0).is_err());
        let rebound=MainnetCollateralObservation{publish_time:102,observation:3,
            context:MainnetCollateralContext{conversion_source_slot:902,conversion_publish_time:102,..o.context},..o};
        q.publish_mainnet(rebound,110).unwrap();h.record(&q).unwrap();
        let after=MainnetCollateralObservation{publish_time:103,observation:4,
            context:MainnetCollateralContext{usd_per_quote_atom_e21:PRICE/4,conversion_source_slot:903,conversion_publish_time:103,..o.context},..o};
        q.publish_mainnet(after,110).unwrap();h.record(&q).unwrap();guard.observe(&trader,&h).unwrap();
        assert_eq!(guard.settled_value(&trader,0).unwrap(),low);
        assert_eq!(trader.slots[0].exit,101_000_000_000_000);
        h.matches(&q).unwrap();h.samples[4].mainnet.usd_per_quote_atom_e21+=1;assert!(h.matches(&q).is_err());
    }

    #[test]
    fn quote_and_sol_paths_cannot_be_overridden_and_history_stays_below_er_limit() {
        let mut q=quote();q.is_quote=true;assert!(q.publish_mainnet(observation(),100).is_err());
        q.is_quote=false;q.asset=sol_collateral_asset();assert!(q.publish_mainnet(observation(),100).is_err());
        assert!(8+CollateralHistory::INIT_SPACE<10_240);
        let mut bytes=vec![];history(&q).try_serialize(&mut bytes).unwrap();assert_eq!(bytes.len(),8+CollateralHistory::INIT_SPACE);
    }

    #[test]
    fn price_floor_ignores_fees_and_pool_depth_then_latches_the_usd_percentage() {
        let o=observation();let mut q=quote();q.publish_mainnet(o,100).unwrap();
        let mut h=history(&q);h.record(&q).unwrap();let owner=Pubkey::new_unique();
        let mut trader=ErTrader{owner,config:q.config,..ErTrader::default()};
        trader.slots[0]=ErSlot{state:ER_OPEN,kind:ER_TRADE,nonce:1,asset:q.asset,margin:1_000_000_000,
            entry_value:10_000_000,opened_source_slot:1,..ErSlot::default()};
        let mut guard=CollateralGuard{owner,config:q.config,floors:CollateralGuard::empty_floors(),bump:0};
        guard.start(&trader,&h,0).unwrap();
        assert_eq!(guard.floors[0].entry_price,12_100_000_000);
        // Same price, one thousandth of the pool depth and very different fees.
        let shallow=MainnetCollateralObservation{base_reserve:o.base_reserve/1000,quote_reserve:o.quote_reserve/1000,
            fee_bps:[100,100,100],publish_time:101,observation:2,
            context:MainnetCollateralContext{real_quote_reserve:o.context.real_quote_reserve/1000,
                pool_source_slot:501,conversion_source_slot:901,conversion_publish_time:101,..o.context}};
        q.publish_mainnet(shallow,101).unwrap();h.record(&q).unwrap();guard.observe(&trader,&h).unwrap();
        assert_eq!(guard.floors[0].value,10_000_000,"historical pool depth is not the price floor");
        assert!(q.value(trader.slots[0].margin,101).unwrap()<10_000_000,"entry/settlement execution checks remain separate");
        q.publish_mainnet(MainnetCollateralObservation{observation:3,publish_time:102,
            context:MainnetCollateralContext{usd_per_quote_atom_e21:PRICE*3/5,conversion_source_slot:902,conversion_publish_time:102,..shallow.context},..shallow},102).unwrap();
        h.record(&q).unwrap();guard.observe(&trader,&h).unwrap();assert_eq!(guard.floors[0].value,6_000_000);
        q.publish_mainnet(MainnetCollateralObservation{observation:4,publish_time:103,
            context:MainnetCollateralContext{usd_per_quote_atom_e21:PRICE*2,conversion_source_slot:903,conversion_publish_time:103,..shallow.context},..shallow},103).unwrap();
        h.record(&q).unwrap();guard.observe(&trader,&h).unwrap();assert_eq!(guard.floors[0].value,6_000_000);
    }

    #[test]
    fn spot_units_are_exact_for_different_decimals_and_cannot_overflow() {
        let o=observation();
        for decimals in [0u8,6,9] {
            let base=1_000_000*10u64.pow(decimals as u32);
            assert_eq!(o.context.spot_price(base,o.quote_reserve,100,decimals).unwrap(),12_100_000_000);
        }
        assert!(o.context.spot_price(1,o.quote_reserve,100,10).is_err());
        assert!(o.context.spot_price(1,u64::MAX,100,9).is_err());
    }
}
