//! LOCAL LITESVM ONLY. This deliberately omits platform custody authorization
//! so the production swap adapter can be tested independently. Never deploy.
use anchor_lang::solana_program::{account_info::AccountInfo,entrypoint,entrypoint::ProgramResult,program_error::ProgramError,pubkey::Pubkey};
entrypoint!(process);
pub fn process(id:&Pubkey, accounts:&[AccountInfo], data:&[u8])->ProgramResult {
    if *id!=acidpump_er::ID || data.len()<114 || data[97]>7 ||
        data.len()!=match data[97] {0..=2=>114,3..=6=>146,7=>211,_=>0} {return Err(ProgramError::InvalidInstructionData);}
    let pool=Pubkey::new_from_array(data[..32].try_into().unwrap());
    let mint=Pubkey::new_from_array(data[33..65].try_into().unwrap());
    let dex=Pubkey::new_from_array(data[65..97].try_into().unwrap());
    if data[97]==7 {
        let split=usize::from(data[210]);
        if split>accounts.len() {return Err(ProgramError::NotEnoughAccountKeys);}
        let quote=Pubkey::new_from_array(data[114..146].try_into().unwrap());
        let (_,received)=acidpump_er::mainnet_curve::execute_token_sell(pool,data[32],dex,mint,quote,&accounts[..split],
            u64::from_le_bytes(data[98..106].try_into().unwrap()),1)?;
        acidpump_er::mainnet_pump::execute_conversion_leg(pool,data[32],Pubkey::new_from_array(data[146..178].try_into().unwrap()),quote,
            Pubkey::new_from_array(data[178..210].try_into().unwrap()),&accounts[split..],received,
            u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    } else if data[97]==6 {
        acidpump_er::mainnet_pump::execute_conversion_leg(pool,data[32],dex,mint,Pubkey::new_from_array(data[114..146].try_into().unwrap()),accounts,
            u64::from_le_bytes(data[98..106].try_into().unwrap()),u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    } else if data[97]>=4 {
        acidpump_er::mainnet_pump::execute_token_leg(pool,data[32],dex,mint,Pubkey::new_from_array(data[114..146].try_into().unwrap()),accounts,data[97]==5,
            u64::from_le_bytes(data[98..106].try_into().unwrap()),u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    } else if data[97]==3 {
        acidpump_er::mainnet_curve::execute_token_sell(pool,data[32],dex,mint,Pubkey::new_from_array(data[114..146].try_into().unwrap()),accounts,
            u64::from_le_bytes(data[98..106].try_into().unwrap()),u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    } else if data[97]==2 {
        acidpump_er::mainnet_curve::execute_sol_sell(pool,data[32],dex,mint,accounts,
            u64::from_le_bytes(data[98..106].try_into().unwrap()),u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    } else { acidpump_er::mainnet_pump::execute_sol_leg(pool,data[32],dex,mint,accounts,data[97]==1,
        u64::from_le_bytes(data[98..106].try_into().unwrap()),u64::from_le_bytes(data[106..114].try_into().unwrap()))?;
    }
    Ok(())
}
