//! Historical devnet backing migration and paused current-layout redelegation.
//! Mainnet only enables the latter; it cannot rewrite or recover account bytes.
use crate::*;
use ephemeral_rollups_sdk::anchor::{commit,delegate};
use ephemeral_rollups_sdk::cpi::DelegateConfig;
use ephemeral_rollups_sdk::ephem::{FoldableIntentBuilder,MagicIntentBundleBuilder};

fn padded<T:AccountDeserialize+Space>(data:&[u8],extra:usize)->Result<T> {
    require!(data.len()==8+T::INIT_SPACE-extra || data.len()==8+T::INIT_SPACE,MixError::Binding);
    let mut bytes=data.to_vec();bytes.resize(8+T::INIT_SPACE,0);
    T::try_deserialize(&mut bytes.as_slice())
}
fn idle(t:&ErTrader,owner:Pubkey,config:Pubkey)->Result<()> {
    require!(t.owner==owner && t.config==config && t.pool==Pubkey::find_program_address(&[b"pool",&0u16.to_le_bytes()],&crate::ID).0,MixError::Binding);
    require!(t.notional==0 && t.slots.iter().all(|s|s.state==ER_FREE) && t.core_stake.tokens==0,MixError::Closed);
    Ok(())
}

pub fn release(ctx:Context<ReleaseLegacyTrader>)->Result<()> {
    require!(cfg!(feature="devnet") && ctx.accounts.config.paused,MixError::Paused);
    let t:ErTrader=padded(&ctx.accounts.er_trader.try_borrow_data()?,32)?;
    idle(&t,ctx.accounts.owner.key(),ctx.accounts.config.key())?;
    MagicIntentBundleBuilder::new(ctx.accounts.er_fee_payer.to_account_info(),ctx.accounts.magic_context.to_account_info(),ctx.accounts.magic_program.to_account_info())
        .magic_fee_vault(ctx.accounts.magic_fee_vault.to_account_info())
        .commit_and_undelegate(&[ctx.accounts.er_trader.to_account_info()])
        .build_and_invoke_signed(&[&[b"er-fee-payer",&[ctx.accounts.er_fee_payer.bump]]])?;
    Ok(())
}
pub fn migrate(ctx:Context<MigrateCoreAccount>,kind:u8)->Result<()> {
    require!(cfg!(feature="devnet") && ctx.accounts.config.paused,MixError::Paused);
    let a=&ctx.accounts.target;
    let size={let data=a.try_borrow_data()?; match kind {
        0=>{let t:ErTrader=padded(&data,32)?;idle(&t,t.owner,ctx.accounts.config.key())?;
            require_keys_eq!(a.key(),Pubkey::find_program_address(&[b"er-trader",t.owner.as_ref()],&crate::ID).0,MixError::Binding);8+ErTrader::INIT_SPACE},
        1=>{let c:ErCustody=padded(&data,24)?;
            require!(c.config==ctx.accounts.config.key(),MixError::Binding);
            require_keys_eq!(a.key(),Pubkey::find_program_address(&[b"er-custody",c.owner.as_ref()],&crate::ID).0,MixError::Binding);8+ErCustody::INIT_SPACE},
        2=>{let p:Position=padded(&data,32)?;require!(p.state!=0,MixError::Closed);
            require_keys_eq!(a.key(),Pubkey::find_program_address(&[b"position",p.owner.as_ref(),&p.nonce.to_le_bytes()],&crate::ID).0,MixError::Binding);8+Position::INIT_SPACE},
        _=>return err!(MixError::Binding),
    }};
    if a.data_len()==size {return Ok(());}
    let rent=Rent::get()?.minimum_balance(size).saturating_sub(a.lamports());
    if rent>0 {anchor_lang::system_program::transfer(CpiContext::new(ctx.accounts.system_program.key(),anchor_lang::system_program::Transfer{
        from:ctx.accounts.payer.to_account_info(),to:a.to_account_info()}),rent)?;}
    a.to_account_info().resize(size)?;
    Ok(())
}
/// Re-request an interrupted return through DLP's owner-authorized base path.
/// No ledger bytes are changed or supplied by the caller.
pub fn request_release(ctx:Context<RequestCoreUndelegation>)->Result<()> {
    require!(cfg!(feature="devnet") && ctx.accounts.config.paused,MixError::Paused);
    let t:ErTrader=padded(&ctx.accounts.er_trader.try_borrow_data()?,32)?;
    idle(&t,ctx.accounts.owner.key(),ctx.accounts.config.key())?;
    let a=&ctx.accounts;
    let ix=ephemeral_rollups_sdk::instruction_builder::request_undelegation(a.payer.key(),a.er_trader.key(),crate::ID);
    anchor_lang::solana_program::program::invoke_signed(&ix,&[
        a.payer.to_account_info(),a.er_trader.to_account_info(),a.owner_program.to_account_info(),
        a.undelegation_request.to_account_info(),a.delegation_record.to_account_info(),
        a.delegation_metadata.to_account_info(),a.system_program.to_account_info(),a.delegation_program.to_account_info(),
    ],&[&[b"er-trader",a.owner.key().as_ref(),&[ctx.bumps.er_trader]]])?;
    Ok(())
}
/// Timed return of a fully committed idle ledger after a failed callback.
/// Refuses pending commits and requires the preserved byte digest. The client
/// additionally checks the frozen ER copy against that same durable snapshot.
pub fn recover(ctx:Context<RecoverCoreTrader>,expected_hash:[u8;32])->Result<()> {
    let a=&ctx.accounts.request;
    require!(cfg!(feature="devnet") && a.config.paused,MixError::Paused);
    require!(ctx.accounts.commit_state.data_is_empty() && ctx.accounts.commit_record.data_is_empty(),MixError::Binding);
    let bytes=a.er_trader.try_borrow_data()?.to_vec();
    require!(ephemeral_rollups_sdk::dlp_api::solana_program::hash::hash(&bytes).to_bytes()==expected_hash,MixError::Binding);
    let t:ErTrader=padded(&bytes,32)?;idle(&t,a.owner.key(),a.config.key())?;
    let ix=ephemeral_rollups_sdk::instruction_builder::undelegate_with_rollback_after_timeout(a.er_trader.key(),crate::ID,a.payer.key(),a.payer.key());
    let owner=a.owner.key();let bump=Pubkey::find_program_address(&[b"er-trader",owner.as_ref()],&crate::ID).1;
    anchor_lang::solana_program::program::invoke_signed(&ix,&[
        a.er_trader.to_account_info(),a.owner_program.to_account_info(),a.undelegation_request.to_account_info(),
        a.delegation_record.to_account_info(),a.delegation_metadata.to_account_info(),a.payer.to_account_info(),
        ctx.accounts.commit_state.to_account_info(),ctx.accounts.commit_record.to_account_info(),
        a.delegation_program.to_account_info(),
    ],&[&[b"er-trader",owner.as_ref(),&[bump]]])?;
    require_keys_eq!(*a.er_trader.owner,crate::ID,MixError::Binding);
    a.er_trader.to_account_info().resize(bytes.len())?;
    a.er_trader.try_borrow_mut_data()?.copy_from_slice(&bytes);
    Ok(())
}
pub fn redelegate(ctx:Context<RedelegateCoreTrader>)->Result<()> {
    require!((cfg!(feature="devnet") || cfg!(feature="mainnet")) && ctx.accounts.config.paused,MixError::Paused);
    require_keys_eq!(*ctx.accounts.er_trader.owner,crate::ID,MixError::Binding);
    require!(ctx.accounts.er_trader.data_len()==8+ErTrader::INIT_SPACE,MixError::Binding);
    let t=ErTrader::try_deserialize(&mut ctx.accounts.er_trader.try_borrow_data()?.as_ref())?;
    #[cfg(feature="mainnet")]
    {
        // A completed capacity migration may retain locked backing. Preserve
        // every byte and return the idle canonical ledger to the pinned ER;
        // the administrator cannot trade or withdraw using this instruction.
        require!(t.owner==ctx.accounts.owner.key() && t.config==ctx.accounts.config.key()
            && t.pool==Pubkey::find_program_address(&[b"pool",&0u16.to_le_bytes()],&crate::ID).0,MixError::Binding);
        require!(t.notional==0 && t.slots.iter().all(|s|s.state==ER_FREE),MixError::Closed);
    }
    #[cfg(not(feature="mainnet"))]
    idle(&t,ctx.accounts.owner.key(),ctx.accounts.config.key())?;
    ctx.accounts.delegate_er_trader(&ctx.accounts.payer,&[b"er-trader",ctx.accounts.owner.key().as_ref()],DelegateConfig{validator:Some(ER_VALIDATOR),commit_frequency_ms:u32::MAX})?;
    Ok(())
}

#[commit]
#[derive(Accounts)]
pub struct ReleaseLegacyTrader<'info> {
    pub admin:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Identity only; admin may only migrate a paused, idle ledger.
    pub owner:UncheckedAccount<'info>,
    /// CHECK: Canonical program-owned ledger decoded and checked without changing bytes.
    #[account(mut,seeds=[b"er-trader",owner.key().as_ref()],bump,owner=crate::ID)] pub er_trader:UncheckedAccount<'info>,
    #[account(mut,seeds=[b"er-fee-payer"],bump=er_fee_payer.bump,has_one=config)] pub er_fee_payer:Account<'info,ErFeePayer>,
    /// CHECK: SDK validates the fixed validator fee vault.
    #[account(mut,address=ephemeral_rollups_sdk::pda::magic_fee_vault_pda_from_validator(&ER_VALIDATOR))] pub magic_fee_vault:UncheckedAccount<'info>,
    pub execution:ErExecution<'info>,
}
#[derive(Accounts)]
pub struct MigrateCoreAccount<'info> {
    pub admin:Signer<'info>,
    #[account(mut)] pub payer:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Typed discriminator, canonical PDA, length and idle state checked in handler.
    #[account(mut,owner=crate::ID)] pub target:UncheckedAccount<'info>,
    pub system_program:Program<'info,System>,
    pub execution:BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct RequestCoreUndelegation<'info> {
    pub admin:Signer<'info>,
    /// DLP verifies this is the original delegation rent payer.
    #[account(mut)] pub payer:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Identity checked against the preserved idle ledger.
    pub owner:UncheckedAccount<'info>,
    /// CHECK: Canonical DLP-owned ledger; checked without rewriting it.
    #[account(mut,seeds=[b"er-trader",owner.key().as_ref()],bump,owner=ephemeral_rollups_sdk::id())] pub er_trader:UncheckedAccount<'info>,
    /// CHECK: Fixed owner program.
    #[account(address=crate::ID)] pub owner_program:UncheckedAccount<'info>,
    /// CHECK: Canonical PDA checked by the SDK instruction and DLP.
    #[account(mut)] pub undelegation_request:UncheckedAccount<'info>,
    /// CHECK: Canonical PDA checked by the SDK instruction and DLP.
    #[account(mut)] pub delegation_record:UncheckedAccount<'info>,
    /// CHECK: Canonical PDA checked by the SDK instruction and DLP.
    #[account(mut)] pub delegation_metadata:UncheckedAccount<'info>,
    /// CHECK: Fixed delegation program.
    #[account(address=ephemeral_rollups_sdk::id())] pub delegation_program:UncheckedAccount<'info>,
    pub system_program:Program<'info,System>,
    pub execution:BaseExecution<'info>,
}
#[derive(Accounts)]
pub struct RecoverCoreTrader<'info> {
    pub request:RequestCoreUndelegation<'info>,
    /// CHECK: DLP checks canonical PDA; handler refuses any pending commit.
    #[account(mut)] pub commit_state:UncheckedAccount<'info>,
    /// CHECK: DLP checks canonical PDA; handler refuses any pending commit.
    #[account(mut)] pub commit_record:UncheckedAccount<'info>,
}
#[delegate]
#[derive(Accounts)]
pub struct RedelegateCoreTrader<'info> {
    pub admin:Signer<'info>,
    #[account(mut)] pub payer:Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)] pub config:Account<'info,Config>,
    /// CHECK: Canonical owner checked against the preserved idle ledger.
    pub owner:UncheckedAccount<'info>,
    /// CHECK: Exact current layout and idle canonical ledger; paused admin only.
    #[account(mut,del,seeds=[b"er-trader",owner.key().as_ref()],bump)] pub er_trader:UncheckedAccount<'info>,
}
