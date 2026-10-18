//! Prospective profit policy. The activation record is immutable; old trades
//! keep the original curve, and existing FIFO claims are never recalculated.
use crate::*;

#[account]
#[derive(InitSpace)]
pub struct EconomicsV1 {
    pub activated_at: i64,
    pub deadband: bool,
    pub bump: u8,
}

pub fn initialize(ctx: Context<InitializeEconomicsV1>, deadband: bool) -> Result<()> {
    require!(ctx.accounts.config.paused, MixError::Paused);
    let policy = &mut ctx.accounts.economics;
    // Configure before reopening entries. Future versions require new immutable
    // activation records and must retain this policy for trades opened under it.
    policy.activated_at = Clock::get()?.unix_timestamp.checked_add(60).ok_or(MixError::Arithmetic)?;
    policy.deadband = deadband;
    policy.bump = ctx.bumps.economics;
    emit!(EconomicsActivated { activated_at: policy.activated_at, deadband });
    Ok(())
}

#[derive(Accounts)]
pub struct InitializeEconomicsV1<'info> {
    #[account(mut)] pub admin: Signer<'info>,
    #[account(seeds=[b"config"], bump=config.bump, has_one=admin)]
    pub config: Box<Account<'info, Config>>,
    #[account(init, payer=admin, space=8+EconomicsV1::INIT_SPACE, seeds=[b"economics-v1"], bump)]
    pub economics: Box<Account<'info, EconomicsV1>>,
    pub execution: BaseExecution<'info>,
    pub system_program: Program<'info, System>,
}

#[event]
pub struct EconomicsActivated { pub activated_at: i64, pub deadband: bool }

#[derive(Debug,PartialEq,Eq)]
pub struct ProfitBreakdown {
    pub raw: u64,
    pub after_deadband: u64,
    pub after_impact: u64,
    pub win_fee: u64,
    pub net: u64,
}

/// Papertrade's published ETH constants, deliberately shared by all markets:
/// base=10%, rateMultiplier=15000, referenceNotional=$100000,
/// positionMultiplier=483.979. Trade size does not change the haircut rate.
/// Terms round UP at 1e-12 precision; payouts round DOWN in token/USD atoms.
pub fn winning_profit(margin:u64,entry:u64,exit:u64,lev:u16,deadband:bool)->Result<ProfitBreakdown> {
    require!(entry>0 && (1..=1000).contains(&lev),MixError::Arithmetic);
    let diff=entry.abs_diff(exit);
    let notional=(margin as u128).checked_mul(lev as u128).ok_or(MixError::Arithmetic)?;
    let raw=math::div(notional,diff as u128,entry as u128)?;
    let adjusted=diff.saturating_sub(if deadband {entry/50_000}else{0});
    let gross=math::div(notional,adjusted as u128,entry as u128)?;
    let movement=math::div(adjusted as u128,math::PX,entry as u128)? as u128;
    if movement==0 || gross==0 {return Ok(ProfitBreakdown{raw,after_deadband:gross,after_impact:0,win_fee:0,net:0});}
    // term2 = 100 / (483979 * movement), using the exact three-decimal parameter.
    let ceil128=|n:u128,d:u128|n/d+u128::from(n%d!=0);
    let term1=ceil128(math::PX*math::PX,movement*15_000);
    let term2=ceil128(100*math::PX*math::PX,movement*483_979);
    let impacted=math::div(gross as u128,math::PX*9/10,math::PX+term1+term2)?;
    let net=math::div(impacted as u128,98,100)?;
    Ok(ProfitBreakdown{raw,after_deadband:gross,after_impact:impacted,win_fee:impacted-net,net})
}

impl EconomicsV1 {
    pub fn profit(&self,opened_at:i64,margin:u64,entry:u64,exit:u64,lev:u16,long:bool)->Result<u64> {
        if opened_at < self.activated_at {return Ok(math::pnl(margin,entry,exit,lev,long)?.1);}
        require!(if long {exit>entry}else{exit<entry},MixError::Arithmetic);
        Ok(winning_profit(margin,entry,exit,lev,self.deadband)?.net)
    }
}

#[event]
pub struct ProfitPolicyApplied { pub position:Pubkey, pub version:u8, pub deadband:bool, pub net_profit:u64 }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activation_preserves_old_positions_and_changes_only_future_wins() {
        let p=EconomicsV1{activated_at:1000,deadband:true,bump:0};
        assert_eq!(p.profit(999,100_000_000,100_000_000,101_000_000,10,true).unwrap(),9_881_188);
        let new=p.profit(1000,100_000_000,100_000_000,101_000_000,10,true).unwrap();
        assert!(new<8_820_000);
        assert_eq!(new,p.profit(1001,100_000_000,100_000_000,99_000_000,10,false).unwrap());
    }
    #[test]
    fn deadband_and_win_fee_are_separate_and_conservative() {
        let zero=winning_profit(100_000_000,100_000_000,100_002_000,1000,true).unwrap();
        assert_eq!(zero.raw,2_000_000);assert_eq!(zero.net,0);
        assert!(winning_profit(100_000_000,100_000_000,100_002_000,1000,false).unwrap().net>0);
        let mut prior=0;
        for diff in [1,1999,2000,2001,10_000,50_000,100_000,1_000_000,10_000_000] {
            let q=winning_profit(100_000_000,100_000_000,100_000_000+diff,1000,true).unwrap();
            assert!(q.net>=prior);prior=q.net;
            assert!(q.after_impact as u128*10<=q.after_deadband as u128*9);
            assert_eq!(q.win_fee+q.net,q.after_impact);
            assert_eq!(q.net as u128,q.after_impact as u128*98/100);
        }
        assert!(winning_profit(1,0,1,1000,true).is_err());
        assert!(winning_profit(1,100,101,0,true).is_err());
        assert!(winning_profit(u64::MAX,1,u64::MAX,1000,true).is_err());
        assert!(winning_profit(100_000_000_000,1_000_000_000_000,1_000_000_000_001,1000,false).is_ok());
    }
}
