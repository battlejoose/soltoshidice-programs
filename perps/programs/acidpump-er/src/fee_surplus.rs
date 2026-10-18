//! Admin-only recovery of earned operating-fee surplus. User vaults are never inputs.
use crate::*;

pub const OPERATING_FLOOR: u64 = 1_000_000_000;

/// Credit actual setup revenue, not a claim about a prior transfer. The
/// private registrar calls this only after reconciling a creator's costs.
/// A plain SOL transfer remains seed funding and is not counted as earnings.
pub fn deposit_revenue(ctx: Context<DepositGasRevenue>, amount: u64) -> Result<()> {
    require!(amount > 0, MixError::Amount);
    anchor_lang::system_program::transfer(CpiContext::new(ctx.accounts.system_program.key(),
        anchor_lang::system_program::Transfer {from:ctx.accounts.admin.to_account_info(),to:ctx.accounts.gas_reserve.to_account_info()}), amount)?;
    let reserve=&mut ctx.accounts.gas_reserve;
    reserve.sol_received=reserve.sol_received.checked_add(amount).ok_or(MixError::Arithmetic)?;
    emit!(GasRevenueDeposited {admin:ctx.accounts.admin.key(),amount,total_received:reserve.sol_received});
    Ok(())
}

#[derive(Accounts)]
pub struct DepositGasRevenue<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"],bump=config.bump,has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut,seeds=[b"gas-reserve"],bump=gas_reserve.bump)]
    pub gas_reserve: Box<Account<'info, GasReserve>>,
    pub system_program: Program<'info,System>,
    pub execution: BaseExecution<'info>,
}

#[event]
pub struct GasRevenueDeposited {pub admin:Pubkey,pub amount:u64,pub total_received:u64}

pub fn available(balance: u64, rent: u64, received: u64, outflows: u64) -> u64 {
    balance.saturating_sub(rent).saturating_sub(OPERATING_FLOOR)
        .min(received.saturating_sub(outflows))
}

pub fn withdraw(ctx: Context<WithdrawGasSurplus>, amount: u64) -> Result<()> {
    let reserve = &mut ctx.accounts.gas_reserve;
    let rent = Rent::get()?.minimum_balance(8 + GasReserve::INIT_SPACE);
    require!(amount > 0 && amount <= available(reserve.to_account_info().lamports(), rent,
        reserve.sol_received, reserve.sol_spent), MixError::GasBudget);
    let remaining = reserve.to_account_info().lamports().checked_sub(amount).ok_or(MixError::Arithmetic)?;
    let destination = ctx.accounts.recipient.lamports().checked_add(amount).ok_or(MixError::Arithmetic)?;
    reserve.sol_spent = reserve.sol_spent.checked_add(amount).ok_or(MixError::Arithmetic)?;
    **reserve.to_account_info().try_borrow_mut_lamports()? = remaining;
    **ctx.accounts.recipient.to_account_info().try_borrow_mut_lamports()? = destination;
    emit!(GasSurplusWithdrawn { admin: ctx.accounts.admin.key(), recipient: ctx.accounts.recipient.key(),
        amount, remaining, operating_floor: OPERATING_FLOOR, total_outflows: reserve.sol_spent });
    Ok(())
}

#[derive(Accounts)]
pub struct WithdrawGasSurplus<'info> {
    pub admin: Signer<'info>,
    #[account(seeds=[b"config"], bump=config.bump, has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(mut, seeds=[b"gas-reserve"], bump=gas_reserve.bump)]
    pub gas_reserve: Box<Account<'info, GasReserve>>,
    #[account(mut, constraint=recipient.key()!=gas_reserve.key() @ MixError::Binding)]
    pub recipient: SystemAccount<'info>,
    pub execution: BaseExecution<'info>,
}

#[event]
pub struct GasSurplusWithdrawn {
    pub admin: Pubkey,
    pub recipient: Pubkey,
    pub amount: u64,
    pub remaining: u64,
    pub operating_floor: u64,
    /// Includes operational refills and explicit surplus withdrawals, not just network fees.
    pub total_outflows: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn surplus_cannot_spend_rent_floor_or_unearned_seed() {
        let rent = 899_160;
        assert_eq!(available(6_160_811_036,rent,5_000_143_451,640_347_544),4_359_795_907);
        assert_eq!(available(rent+OPERATING_FLOOR,rent,u64::MAX,0),0);
        assert_eq!(available(0,rent,100,0),0);
        assert_eq!(available(u64::MAX,rent,100,99),1);
        assert_eq!(available(u64::MAX,rent,100,101),0);
        let before=available(6_160_811_036,rent,5_000_143_451,640_347_544);
        let after=available(2_160_811_036,rent,5_000_143_451,4_640_347_544);
        assert_eq!(before-after,4_000_000_000);
    }
}
