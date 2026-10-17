//! Once-per-winner bookkeeping, separate from the legacy hand account layout.
use anchor_lang::prelude::*;
use crate::{errors::RiverproofError, pot_fees::PotFee, state::{HandStage, HandState, TableConfig}};

pub const POT_SEED: &[u8] = b"poker-pot";

#[account]
#[derive(InitSpace)]
pub struct PokerPotSettlement {
    pub table: Pubkey,
    pub hand: u64,
    pub paid: u16,
    pub bump: u8,
}

pub fn native_fees(hand: &HandState) -> Result<[PotFee; 10]> {
    if hand.stage != HandStage::Complete || hand.winner_count == 0 {
        return Ok([PotFee::default(); 10]);
    }
    require!(hand.settled && hand.winner_count <= 10, RiverproofError::InvalidPotFee);
    let mut awards = [0u64; 10];
    for winner in hand.winners.iter().take(hand.winner_count as usize) {
        let index = winner.seat as usize;
        require!(index < 10, RiverproofError::InvalidPotFee);
        awards[index] = awards[index].checked_add(winner.amount).ok_or(RiverproofError::InvalidPotFee)?;
    }
    crate::pot_fees::pot_fees_with_absent(&hand.contributions, hand.folded, &awards,
        hand.folded & !hand.active_seats, hand.active_seats & !hand.folded)
        .map_err(|_| error!(RiverproofError::InvalidPotFee))
}

pub fn require_paid(config: &Account<TableConfig>, hand: &HandState, remaining: &[AccountInfo]) -> Result<()> {
    if !crate::tournament::accounts::is_managed(config) { return Ok(()); }
    let fees = native_fees(hand)?;
    let mask = fees.iter().enumerate().fold(0u16, |mask, (i, f)| mask | if f.fee_chips > 0 { 1 << i } else { 0 });
    if mask == 0 { return Ok(()); }
    let key = Pubkey::find_program_address(&[POT_SEED, config.key().as_ref()], &crate::ID).0;
    let info = remaining.iter().find(|a| *a.key == key).ok_or(RiverproofError::PotFeesPending)?;
    require_keys_eq!(*info.owner, crate::ID, RiverproofError::InvalidPotFee);
    let settlement = PokerPotSettlement::try_deserialize(&mut &info.try_borrow_data()?[..])?;
    require!(settlement.table == config.key() && settlement.hand == hand.hand_id
        && settlement.paid & mask == mask, RiverproofError::PotFeesPending);
    Ok(())
}
