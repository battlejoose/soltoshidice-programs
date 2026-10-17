//! SDICE custody boundary. The poker engine still counts whole tokens.
use anchor_lang::prelude::*;
use anchor_spl::token_2022::spl_token_2022::extension::{
    BaseStateWithExtensions, ExtensionType, StateWithExtensions,
};
use anchor_spl::token_2022::spl_token_2022::state::Mint;
use crate::errors::RiverproofError;

pub const DECIMALS: u8 = 6;
pub const UNITS: u64 = 1_000_000;

pub fn base_units(chips: u64) -> Result<u64> {
    chips.checked_mul(UNITS).ok_or_else(|| error!(RiverproofError::InvalidTableConfig))
}

pub fn valid_mint(account: &AccountInfo) -> Result<()> {
    require_keys_eq!(*account.owner, anchor_spl::token_2022::ID, RiverproofError::InvalidTableConfig);
    let data = account.try_borrow_data()?;
    let mint = StateWithExtensions::<Mint>::unpack(&data)?;
    require!(mint.base.is_initialized && mint.base.decimals == DECIMALS
        && mint.base.freeze_authority.is_none(), RiverproofError::InvalidTableConfig);
    require!(mint.get_extension_types()?.iter().all(|extension| matches!(extension,
        ExtensionType::MetadataPointer | ExtensionType::TokenMetadata)),
        RiverproofError::InvalidTableConfig);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn whole_sdice_is_exact_and_overflow_is_rejected() {
        assert_eq!(base_units(10_000).unwrap(), 10_000_000_000);
        assert_eq!(base_units(1_000_000).unwrap(), 1_000_000_000_000);
        assert_eq!(base_units(0).unwrap(), 0);
        assert!(base_units(u64::MAX / UNITS + 1).is_err());
    }
}
