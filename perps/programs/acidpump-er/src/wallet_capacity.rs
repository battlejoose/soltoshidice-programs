//! Bounded legacy 8/50 -> 10 slot migration. No token movements or credit
//! resets. The paused wallet must finish every position/withdrawal before its
//! canonical ER trader and guard are committed and returned to Solana.
use crate::*;
use ephemeral_rollups_sdk::anchor::commit;
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder,MagicIntentBundleBuilder};

pub const LEGACY_SLOTS:usize=8;
pub const LEGACY_ASSETS:usize=16;
pub const LEGACY_BALANCE_BYTES:usize=56;
pub const PREVIOUS_SLOTS:usize=50;
pub const PREVIOUS_ASSETS:usize=51;

/// Preserve every named token record and every replay watermark. A 50-slot
/// source may be compacted only when its named records fit the smaller ledger;
/// excess records are rejected rather than silently losing balances or credits.
fn expand_ledger<T:AccountDeserialize+Space>(bytes:&[u8],prefix:usize,slot_bytes:usize,tail:usize)->Result<Vec<u8>> {
    let size=8+T::INIT_SPACE;
    if bytes.len()==size {T::try_deserialize(&mut &bytes[..])?;return Ok(bytes.to_vec());}
    require!(cfg!(feature="mainnet"),MixError::Binding);
    let legacy=bytes.len()==prefix+LEGACY_ASSETS*LEGACY_BALANCE_BYTES+LEGACY_SLOTS*slot_bytes+tail;
    let previous=bytes.len()==prefix+PREVIOUS_ASSETS*64+PREVIOUS_SLOTS*slot_bytes+tail+8;
    require!(legacy || previous,MixError::Binding);
    let (assets,slots,balance_bytes)=if legacy {(LEGACY_ASSETS,LEGACY_SLOTS,LEGACY_BALANCE_BYTES)}else{(PREVIOUS_ASSETS,PREVIOUS_SLOTS,64)};
    let mut expanded=Vec::with_capacity(size);expanded.extend_from_slice(&bytes[..prefix]);
    let mut kept=0;
    for i in 0..assets {
        let at=prefix+i*balance_bytes;let record=&bytes[at..at+balance_bytes];
        if previous && record[..32].iter().all(|b|*b==0) {
            require!(record.iter().all(|b|*b==0),MixError::Binding);
            continue;
        }
        require!(kept<ER_ASSETS,MixError::Inventory);
        expanded.extend_from_slice(record);if legacy {expanded.extend_from_slice(&[0;8]);}kept+=1;
    }
    expanded.resize(prefix+ER_ASSETS*64,0);
    let at=prefix+assets*balance_bytes;
    // Check every discarded slot, including the last slot in the old account.
    // A close/withdrawal awaiting settlement must never disappear in a resize.
    for i in ER_SLOTS..slots {
        let record=&bytes[at+i*slot_bytes..at+(i+1)*slot_bytes];
        if slot_bytes==ErSlot::INIT_SPACE {require!(record[0]==ER_FREE,MixError::Closed);}
        else {require!(record[..8].iter().all(|b|*b==0)||record[41]==1,MixError::Closed);}
    }
    expanded.extend_from_slice(&bytes[at..at+slots.min(ER_SLOTS)*slot_bytes]);
    expanded.resize(prefix+ER_ASSETS*64+ER_SLOTS*slot_bytes,0);
    expanded.extend_from_slice(&bytes[at+slots*slot_bytes..]);if legacy {expanded.extend_from_slice(&[0;8]);}
    require!(expanded.len()==size,MixError::Binding);
    T::try_deserialize(&mut expanded.as_slice())?;Ok(expanded)
}

/// Preserve the retained guard entries and trailing bump. The corresponding
/// trader is checked across all source slots before a migration is authorized.
fn expand<T:AccountDeserialize+Space>(bytes:&[u8],prefix:usize,element:usize)->Result<Vec<u8>> {
    let size=8+T::INIT_SPACE;
    if bytes.len()==size {T::try_deserialize(&mut &bytes[..])?;return Ok(bytes.to_vec());}
    let slots=if bytes.len()==prefix+LEGACY_SLOTS*element+1 {LEGACY_SLOTS}
        else if bytes.len()==prefix+PREVIOUS_SLOTS*element+1 {PREVIOUS_SLOTS}
        else {return err!(MixError::Binding)};
    let mut expanded=Vec::with_capacity(size);
    expanded.extend_from_slice(&bytes[..prefix+slots.min(ER_SLOTS)*element]);
    expanded.resize(prefix+ER_SLOTS*element,0);expanded.extend_from_slice(&bytes[prefix+slots*element..]);
    T::try_deserialize(&mut expanded.as_slice())?;
    Ok(expanded)
}
pub fn trader_bytes(bytes:&[u8])->Result<Vec<u8>> {
    expand_ledger::<ErTrader>(bytes,8+3*32+2*8,ErSlot::INIT_SPACE,1+crate::stake_backing::CoreStakeState::INIT_SPACE)
}
pub fn custody_bytes(bytes:&[u8])->Result<Vec<u8>> {
    expand_ledger::<ErCustody>(bytes,8+3*32,ErReceipt::INIT_SPACE,1+crate::stake_backing::CoreCoverageState::INIT_SPACE)
}
pub fn guard_bytes(bytes:&[u8])->Result<Vec<u8>> {
    expand::<CollateralGuard>(bytes,8+2*32,CollateralFloor::INIT_SPACE)
}
fn checked_trader(bytes:&[u8],owner:Pubkey,config:Pubkey)->Result<ErTrader> {
    let expanded=trader_bytes(bytes)?;let t=ErTrader::try_deserialize(&mut expanded.as_slice())?;
    require!(t.owner==owner && t.config==config && t.pool==Pubkey::find_program_address(&[b"pool",&0u16.to_le_bytes()],&crate::ID).0,MixError::Binding);
    // Backing may remain locked: it is copied, never withdrawn or consumed.
    require!(t.notional==0 && t.slots.iter().all(|s|s.state==ER_FREE),MixError::Closed);
    Ok(t)
}
fn checked_guard(bytes:&[u8],owner:Pubkey,config:Pubkey)->Result<()> {
    let expanded=guard_bytes(bytes)?;let g=CollateralGuard::try_deserialize(&mut expanded.as_slice())?;
    require!(g.owner==owner && g.config==config,MixError::Binding);Ok(())
}
pub fn release(ctx:Context<ReleaseWalletCapacity>)->Result<()> {
    let a=&ctx.accounts;
    require!(cfg!(feature="mainnet") && a.config.paused,MixError::Paused);
    checked_trader(&a.er_trader.try_borrow_data()?,a.owner.key(),a.config.key())?;
    checked_guard(&a.guard.try_borrow_data()?,a.owner.key(),a.config.key())?;
    // Both canonical records return through normal DLP finalization. No timeout
    // rollback or caller-supplied state can authorize the resize.
    MagicIntentBundleBuilder::new(a.er_fee_payer.to_account_info(),a.magic_context.to_account_info(),a.magic_program.to_account_info())
        .magic_fee_vault(a.magic_fee_vault.to_account_info())
        .commit_and_undelegate(&[a.er_trader.to_account_info(),a.guard.to_account_info()])
        .build_and_invoke_signed(&[&[b"er-fee-payer",&[a.er_fee_payer.bump]]])?;
    Ok(())
}
fn resize<'info>(target:&UncheckedAccount<'info>,bytes:&[u8],payer:&Signer<'info>,system:&Program<'info,System>)->Result<()> {
    if target.data_len()==bytes.len() {return Ok(());}
    let rent=Rent::get()?.minimum_balance(bytes.len()).saturating_sub(target.lamports());
    if rent>0 {anchor_lang::system_program::transfer(CpiContext::new(system.key(),anchor_lang::system_program::Transfer{
        from:payer.to_account_info(),to:target.to_account_info()}),rent)?;}
    target.to_account_info().resize(bytes.len())?;
    target.try_borrow_mut_data()?.copy_from_slice(bytes);Ok(())
}
pub fn migrate(ctx:Context<MigrateWalletCapacity>)->Result<()> {
    let a=&ctx.accounts;
    require!(cfg!(feature="mainnet") && a.config.paused,MixError::Paused);
    let layout=|length,legacy,previous,current|if length==legacy {0}else if length==previous {1}else if length==current {2}else {3};
    let version=layout(a.er_trader.data_len(),2081,9875,8+ErTrader::INIT_SPACE);
    require!(version<3 && version==layout(a.custody.data_len(),1361,5501,8+ErCustody::INIT_SPACE)
        && version==layout(a.guard.data_len(),529,2923,8+CollateralGuard::INIT_SPACE),MixError::Binding);
    // Account constraints require all three records to be program-owned on
    // base. A stale delegated checkpoint cannot pass while ER can still write.
    let t=checked_trader(&a.er_trader.try_borrow_data()?,a.owner.key(),a.config.key())?;
    checked_guard(&a.guard.try_borrow_data()?,a.owner.key(),a.config.key())?;
    let trader=trader_bytes(&a.er_trader.try_borrow_data()?)?;
    let custody=custody_bytes(&a.custody.try_borrow_data()?)?;
    let guard=guard_bytes(&a.guard.try_borrow_data()?)?;
    let c=ErCustody::try_deserialize(&mut custody.as_slice())?;
    require!(c.owner==t.owner && c.config==t.config && c.pool==t.pool,MixError::Binding);
    require!(c.receipts.iter().all(|r|r.nonce==0 || r.settled),MixError::Closed);
    resize(&a.er_trader,&trader,&a.payer,&a.system_program)?;
    resize(&a.custody,&custody,&a.payer,&a.system_program)?;
    resize(&a.guard,&guard,&a.payer,&a.system_program)?;
    emit!(WalletCapacityExpanded{owner:t.owner,slots:ER_SLOTS as u16});Ok(())
}

#[commit]
#[derive(Accounts)]
pub struct ReleaseWalletCapacity<'info> {
    pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Namespace and stored identity verified before requesting release.
    pub owner:UncheckedAccount<'info>,
    /// CHECK: Decode exact old/new layouts without modifying the delegated data.
    #[account(mut,seeds=[b"er-trader",owner.key().as_ref()],bump,owner=crate::ID)] pub er_trader:UncheckedAccount<'info>,
    /// CHECK: Exact old/new layout, owner and configuration verified in handler.
    #[account(mut,seeds=[b"er-collateral-guard",owner.key().as_ref()],bump,owner=crate::ID)] pub guard:UncheckedAccount<'info>,
    #[account(mut,seeds=[b"er-fee-payer"],bump=er_fee_payer.bump,has_one=config)] pub er_fee_payer:Account<'info,ErFeePayer>,
    /// CHECK: Fixed canonical validator fee vault, validated by the SDK.
    #[account(mut,address=ephemeral_rollups_sdk::pda::magic_fee_vault_pda_from_validator(&ER_VALIDATOR))] pub magic_fee_vault:UncheckedAccount<'info>,
    pub execution:ErExecution<'info>,
}
#[derive(Accounts)]
pub struct MigrateWalletCapacity<'info> {
    pub admin:Signer<'info>,
    #[account(mut)] pub payer:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Canonical namespace and stored owner checked for all three records.
    pub owner:UncheckedAccount<'info>,
    /// CHECK: Requires completed DLP return; legacy bytes are validated in handler.
    #[account(mut,seeds=[b"er-trader",owner.key().as_ref()],bump,owner=crate::ID)] pub er_trader:UncheckedAccount<'info>,
    /// CHECK: Canonical base custody, exact legacy/new layout and identity checked.
    #[account(mut,seeds=[b"er-custody",owner.key().as_ref()],bump,owner=crate::ID)] pub custody:UncheckedAccount<'info>,
    /// CHECK: Requires completed DLP return, exact layout and identity checked.
    #[account(mut,seeds=[b"er-collateral-guard",owner.key().as_ref()],bump,owner=crate::ID)] pub guard:UncheckedAccount<'info>,
    pub system_program:Program<'info,System>,
    pub execution:BaseExecution<'info>,
}
#[event]
pub struct WalletCapacityExpanded {pub owner:Pubkey,pub slots:u16}

#[cfg(all(test,feature="mainnet"))]
mod tests {
    use super::*;
    fn bytes<T:AccountSerialize>(x:&T)->Vec<u8>{let mut b=vec![];x.try_serialize(&mut b).unwrap();b}
    fn legacy(mut b:Vec<u8>,prefix:usize,element:usize)->Vec<u8>{b.drain(prefix+LEGACY_SLOTS*element..prefix+ER_SLOTS*element);b}
    fn legacy_ledger(b:&[u8],prefix:usize,slot_bytes:usize,tail:usize)->Vec<u8> {
        let mut old=b[..prefix].to_vec();
        for i in 0..LEGACY_ASSETS {let at=prefix+i*64;old.extend_from_slice(&b[at..at+56]);}
        let at=prefix+ER_ASSETS*64;old.extend_from_slice(&b[at..at+LEGACY_SLOTS*slot_bytes]);
        let at=at+ER_SLOTS*slot_bytes;old.extend_from_slice(&b[at..at+tail]);old
    }
    #[test]
    fn expands_without_changing_existing_slots_balances_or_backing_cursors() {
        assert_eq!(ER_SLOTS,10);assert_eq!(ER_ASSETS,16);assert!(8+ErTrader::INIT_SPACE<10_240);assert!(8+CollateralGuard::INIT_SPACE<10_240);
        let mut t=ErTrader{owner:Pubkey::new_unique(),config:Pubkey::new_unique(),pool:Pubkey::new_unique(),next_nonce:99,notional:123,bump:254,
            core_stake:stake_backing::CoreStakeState{tokens:91,acknowledged_debits:7,incurred_usd:6,repaid_usd:2},..ErTrader::default()};
        for i in 0..LEGACY_SLOTS {t.slots[i]=ErSlot{state:ER_OPEN,kind:ER_TRADE,nonce:i as u64+1,asset:Pubkey::new_unique(),margin:9753,..ErSlot::default()};}
        for a in t.assets.iter_mut().take(LEGACY_ASSETS) {*a=ErBalance{asset:Pubkey::new_unique(),credited_deposits:54,credited_returns:13,available:17,..ErBalance::default()};}
        let full=bytes(&t);let old=legacy_ledger(&full,8+3*32+2*8,ErSlot::INIT_SPACE,33);
        assert_eq!(old.len(),2081);assert_eq!(full.len(),2475);assert_eq!(trader_bytes(&old).unwrap(),full);
        assert_eq!(trader_bytes(&full).unwrap(),full);assert!(trader_bytes(&old[..old.len()-1]).is_err());
        let mut wrong=old.clone();wrong[0]^=1;assert!(trader_bytes(&wrong).is_err());
        let mut c=ErCustody{owner:t.owner,config:t.config,pool:t.pool,bump:252,
            core_coverage:stake_backing::CoreCoverageState{debited_tokens:7,incurred_usd:6,repaid_usd:2},..ErCustody::default()};
        for (i,r) in c.receipts.iter_mut().take(LEGACY_SLOTS).enumerate(){*r=ErReceipt{nonce:i as u64+2,kind:ER_TRADE,settled:true,position:Pubkey::new_unique()};}
        for a in c.assets.iter_mut().take(LEGACY_ASSETS) {*a=CustodyAsset{asset:Pubkey::new_unique(),deposited_credits:84,returned_credits:25,available:51,..CustodyAsset::default()};}
        let full=bytes(&c);let old=legacy_ledger(&full,8+3*32,ErReceipt::INIT_SPACE,25);
        assert_eq!(old.len(),1361);assert_eq!(full.len(),1581);assert_eq!(custody_bytes(&old).unwrap(),full);
        let mut g=CollateralGuard{owner:t.owner,config:t.config,floors:CollateralGuard::empty_floors(),bump:251};
        for (i,f) in g.floors.iter_mut().take(LEGACY_SLOTS).enumerate(){*f=CollateralFloor{nonce:i as u64+2,value:57,closed_at:87,entry_price:63,..CollateralFloor::default()};}
        let full=bytes(&g);let old=legacy(full.clone(),8+2*32,CollateralFloor::INIT_SPACE);
        assert_eq!(old.len(),529);assert_eq!(full.len(),643);assert_eq!(guard_bytes(&old).unwrap(),full);
    }
    fn fifty_ledger(data:&[u8],prefix:usize,element:usize)->Vec<u8> {
        let mut wide=data[..prefix+ER_ASSETS*64].to_vec();
        wide.resize(prefix+PREVIOUS_ASSETS*64,0);
        let at=prefix+ER_ASSETS*64;
        wide.extend_from_slice(&data[at..at+ER_SLOTS*element]);
        wide.resize(prefix+PREVIOUS_ASSETS*64+PREVIOUS_SLOTS*element,0);
        wide.extend_from_slice(&data[at+ER_SLOTS*element..]);wide
    }
    #[test]
    fn fifty_slot_shrink_preserves_high_index_balances_and_generation() {
        let mut t=ErTrader{owner:Pubkey::new_unique(),config:Pubkey::new_unique(),next_nonce:987,custody_generation:31,..ErTrader::default()};
        t.assets[0]=ErBalance{asset:Pubkey::new_unique(),available:31,credited_deposits:79,credited_returns:27,generation:29};
        t.core_stake.tokens=555;
        let expected=bytes(&t);let mut old=fifty_ledger(&expected,120,ErSlot::INIT_SPACE);
        let balance=old[120..184].to_vec();old[120..184].fill(0);old[120+50*64..120+51*64].copy_from_slice(&balance);
        assert_eq!(old.len(),9875);assert_eq!(trader_bytes(&old).unwrap(),expected);
        let last=120+PREVIOUS_ASSETS*64+49*ErSlot::INIT_SPACE;
        for state in [ER_OPEN,ER_PENDING] {old[last]=state;assert!(trader_bytes(&old).is_err());}
        old[last]=ER_FREE;
        // Keeping a smaller ledger never silently drops a seventeenth token.
        for i in 0..17 {let at=120+i*64;old[at..at+32].copy_from_slice(Pubkey::new_unique().as_ref());}
        assert!(trader_bytes(&old).is_err());
    }
    #[test]
    fn fifty_slot_shrink_rejects_unsettled_tail_receipt_and_keeps_guard_bump() {
        let mut c=ErCustody{owner:Pubkey::new_unique(),config:Pubkey::new_unique(),generation:77,bump:249,..ErCustody::default()};
        c.assets[0]=CustodyAsset{asset:Pubkey::new_unique(),available:5,deposited_credits:12,returned_credits:8,generation:76};
        let expected=bytes(&c);let mut old=fifty_ledger(&expected,104,ErReceipt::INIT_SPACE);
        let last=104+PREVIOUS_ASSETS*64+49*ErReceipt::INIT_SPACE;
        old[last..last+8].copy_from_slice(&88u64.to_le_bytes());old[last+41]=1;
        assert_eq!(old.len(),5501);assert_eq!(custody_bytes(&old).unwrap(),expected);
        old[last+41]=0;assert!(custody_bytes(&old).is_err());
        let g=CollateralGuard{owner:c.owner,config:c.config,floors:CollateralGuard::empty_floors(),bump:247};
        let expected=bytes(&g);let mut wide=expected[..expected.len()-1].to_vec();wide.resize(72+50*CollateralFloor::INIT_SPACE,0);wide.push(247);
        assert_eq!(wide.len(),2923);assert_eq!(guard_bytes(&wide).unwrap(),expected);
    }
    #[test]
    fn release_requires_finished_positions_but_preserves_locked_backing() {
        let owner=Pubkey::new_unique();let config=Pubkey::new_unique();
        let mut t=ErTrader{owner,config,pool:Pubkey::find_program_address(&[b"pool",&0u16.to_le_bytes()],&crate::ID).0,..ErTrader::default()};
        t.core_stake.tokens=9;checked_trader(&bytes(&t),owner,config).unwrap();
        t.slots[0].state=ER_OPEN;assert!(checked_trader(&bytes(&t),owner,config).is_err());
        t.slots[0].state=ER_PENDING;assert!(checked_trader(&bytes(&t),owner,config).is_err());
        t.slots[0].state=ER_FREE;t.notional=1;assert!(checked_trader(&bytes(&t),owner,config).is_err());
        t.notional=0;assert!(checked_trader(&bytes(&t),Pubkey::new_unique(),config).is_err());
    }
}
