//! Auxiliary SOL/USDC coverage is converted inside the same base transaction
//! as the frozen close. A failed route rolls back the charge and the receipt.
use crate::*;

#[derive(AnchorSerialize,AnchorDeserialize,Clone,Copy,Default,Debug)]
pub struct BackingRoute {
    /// Three bound source accounts followed by the swap route, or zero.
    pub accounts:u8,
    pub sell_count:u8,
    pub min_out:u64,
}
#[derive(AnchorSerialize,AnchorDeserialize,Clone,Copy,Default,Debug)]
pub struct BackingRoutes { pub sol:BackingRoute, pub usd:BackingRoute }

pub fn available(custody:&ErCustody,trader:&ErTrader,backing:&stake_backing::Backing)->Result<[u64;3]> {
    require!(custody.owner==trader.owner && custody.config==trader.config && custody.pool==trader.pool,MixError::Binding);
    let basket=stake_backing::basket(trader);
    basket.validate(&stake_backing::basket_assets(backing)?)?;
    let sources=[custody.core_coverage,custody.auxiliary_coverage[0],custody.auxiliary_coverage[1]];
    let mut out=[0;3];
    for i in 0..3 {
        let pending=sources[i].debited_tokens.checked_sub(basket.stakes[i].acknowledged_debits).ok_or(MixError::Replay)?;
        out[i]=basket.stakes[i].tokens.checked_sub(pending).ok_or(MixError::Inventory)?;
    }
    Ok(out)
}

#[event]
pub struct BackingCoverageConverted {pub position:Pubkey,pub owner:Pubkey,pub asset:Pubkey,pub tokens:u64,pub primary_received:u64}

/// Accounts are sliced by explicit lengths and authenticated independently of
/// route instructions. Both custody and inventory debit only actual reserved
/// tokens, and the returned amount is the measured primary vault increase.
pub fn convert<'info>(a:&mut Close<'info>,trader:&ErTrader,plan:&backing_basket::Plan,
    routes:BackingRoutes,accounts:&'info [AccountInfo<'info>])->Result<u64> {
    let backing=stake_backing::Backing::new(a.pool_accounts.pool.settlement,a.bridge.config.quote_mint);
    let ids=stake_backing::basket_assets(&backing)?;
    let available=available(&a.bridge.custody,trader,&backing)?;
    let mut offset=0usize;let mut received=0u64;
    for (i,route) in [routes.sol,routes.usd].into_iter().enumerate() {
        let count=route.accounts as usize;
        let end=offset.checked_add(count).ok_or(MixError::Arithmetic)?;
        let source=accounts.get(offset..end).ok_or(MixError::Binding)?;offset=end;
        let amount=plan.debits[i+1].tokens;
        if amount==0 {
            // A funded reserve's route can be prepared before the exact close
            // price is known. Unused routes never deserialize or write accounts.
            require!(count==0 || (count>=3 && ids.active(i+1)),MixError::Binding);continue;
        }
        require!(count>=3 && route.min_out>0 && ids.active(i+1) && amount<=available[i+1],MixError::Binding);
        let asset=Account::<Asset>::try_from(&source[0])?;
        require!(asset.key()==ids.0[i+1] && native_market::kind(&asset)?.is_some(),MixError::Binding);
        let mut inventory=Account::<Inventory>::try_from(&source[1])?;
        let vault=InterfaceAccount::<TokenAccount>::try_from(&source[2])?;
        require!(source[1].is_writable && source[2].is_writable && *source[2].owner==anchor_spl::token::ID,MixError::Binding);
        let pool=a.pool_accounts.pool.key();let asset_key=asset.key();
        require!(inventory.pool==pool && inventory.asset==asset_key
            && inventory.key()==Pubkey::find_program_address(&[b"inventory",pool.as_ref(),asset_key.as_ref()],&crate::ID).0
            && vault.key()==Pubkey::find_program_address(&[b"vault",pool.as_ref(),asset_key.as_ref()],&crate::ID).0
            && vault.owner==a.pool_accounts.authority.key() && vault.mint==asset.mint,MixError::Binding);
        let index=a.bridge.custody.assets.iter().position(|b|b.asset==asset_key).ok_or(MixError::Inventory)?;
        let balance=a.bridge.custody.assets[index].available.checked_sub(amount).ok_or(MixError::Inventory)?;
        let locked=inventory.locked.checked_sub(amount).ok_or(MixError::Inventory)?;
        let debit=a.bridge.custody.auxiliary_coverage[i].debited_tokens.checked_add(amount).ok_or(MixError::Arithmetic)?;
        let before=pump::token_amount(&a.fees.perp_vault.to_account_info())?;
        let out=pump::swap_loss(&a.pool_accounts.pool,&a.pool_accounts.authority,&asset,&a.fees.perp,
            &vault,&a.fees.perp_vault,&a.quote_vault,&a.pump_program,&source[3..],route.sell_count,amount,route.min_out)?;
        require!(out>=route.min_out && pump::token_amount(&a.fees.perp_vault.to_account_info())?.checked_sub(before)==Some(out),MixError::Binding);
        inventory.locked=locked;
        a.bridge.custody.assets[index].available=balance;
        a.bridge.custody.auxiliary_coverage[i].debited_tokens=debit;
        inventory.exit(&crate::ID)?;
        received=received.checked_add(out).ok_or(MixError::Arithmetic)?;
        emit!(BackingCoverageConverted{position:a.position.key(),owner:trader.owner,asset:asset_key,tokens:amount,primary_received:out});
    }
    require!(offset==accounts.len(),MixError::Binding);
    Ok(received)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn stale_commits_cannot_charge_auxiliary_backing_twice() {
        let b=stake_backing::root_backing();let owner=Pubkey::new_unique();
        let mut trader=ErTrader{owner,..ErTrader::default()};trader.auxiliary_stakes[0].tokens=100;
        let mut custody=ErCustody{owner,..ErCustody::default()};custody.auxiliary_coverage[0].debited_tokens=40;
        assert_eq!(available(&custody,&trader,&b).unwrap(),[0,60,0]);
        trader.auxiliary_stakes[0].sync(custody.auxiliary_coverage[0]).unwrap();
        assert_eq!(available(&custody,&trader,&b).unwrap(),[0,60,0]);
        let old=ErCustody{owner,..ErCustody::default()};assert!(available(&old,&trader,&b).is_err());
        custody.owner=Pubkey::new_unique();assert!(available(&custody,&trader,&b).is_err());
    }
}
