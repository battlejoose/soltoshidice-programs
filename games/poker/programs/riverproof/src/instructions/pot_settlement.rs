use anchor_lang::prelude::*;
use anchor_lang::solana_program::{instruction::{AccountMeta, Instruction}, program::invoke_signed};
use anchor_spl::token_2022::Token2022;
use anchor_spl::token_interface::{Mint, TokenAccount};
use crate::{errors::RiverproofError, instructions::table::assert_chips_conserved,
    pot_settlement::{native_fees, PokerPotSettlement, POT_SEED}, sdice::{base_units, valid_mint}, state::*,
    tournament::accounts::{PokerArena, ARENA_SEED}};

#[derive(Accounts)]
pub struct CollectPokerPotFee<'info> {
    #[account(mut)]
    pub payer: Signer<'info>,
    #[account(seeds = [ARENA_SEED, arena.operator.as_ref()], bump = arena.bump)]
    pub arena: Box<Account<'info, PokerArena>>,
    #[account(constraint = config.join_key == arena.key() && arena.tables.contains(&config.key())
        @ RiverproofError::InvalidTournament)]
    pub config: Box<Account<'info, TableConfig>>,
    #[account(mut, seeds = [TABLE_STATE_SEED, config.key().as_ref()], bump = table_state.bump)]
    pub table_state: Box<Account<'info, TableState>>,
    #[account(seeds = [HAND_SEED, config.key().as_ref()], bump = hand_state.bump)]
    pub hand_state: Box<Account<'info, HandState>>,
    #[account(mut, seeds = [POT_SEED, config.key().as_ref()], bump = settlement.bump,
        constraint = settlement.table == config.key() @ RiverproofError::InvalidPotFee)]
    pub settlement: Box<Account<'info, PokerPotSettlement>>,
    #[account(mut, seeds = [VAULT_SEED, config.key().as_ref()], bump = config.vault_bump,
        token::mint = mint, token::authority = vault, token::token_program = token_program)]
    pub vault: Box<InterfaceAccount<'info, TokenAccount>>,
    #[account(address = config.mint, constraint = mint.key() == arena.mint @ RiverproofError::InvalidTournament)]
    pub mint: Box<InterfaceAccount<'info, Mint>>,
    /// CHECK: Immutable registry destination, executable program required.
    #[account(address = arena.bank_program, executable)]
    pub bank_program: UncheckedAccount<'info>,
    /// CHECK: The registered bank program verifies its own account and PDAs in CPI.
    #[account(address = arena.bank, owner = arena.bank_program)]
    pub bank: UncheckedAccount<'info>,
    pub token_program: Program<'info, Token2022>,
    pub system_program: Program<'info, System>,
}

/// Remaining accounts: winner wallet, bank vault, stake pool, rewards vault,
/// virtual event, receipt, winner profile, immutable direct parent, root profile.
/// Amount and beneficiary come from the completed native hand, not ix data.
pub fn collect_poker_pot_fee<'info>(ctx: Context<'info, CollectPokerPotFee<'info>>, seat_index: u8) -> Result<()> {
    require!(seat_index < 10 && ctx.remaining_accounts.len() == 9, RiverproofError::InvalidPotFee);
    let hand = &ctx.accounts.hand_state;
    require!(hand.stage == HandStage::Complete && hand.settled, RiverproofError::InvalidPotFee);
    assert_chips_conserved(&ctx.accounts.table_state, hand)?;
    let amount = native_fees(hand)?[seat_index as usize].fee_chips;
    require!(amount > 0, RiverproofError::InvalidPotFee);
    let seat = ctx.accounts.table_state.seats[seat_index as usize];
    require!(seat.occupied && seat.owner == *ctx.remaining_accounts[0].key, RiverproofError::InvalidPotFee);
    let ledger = &mut ctx.accounts.settlement;
    if ledger.hand != hand.hand_id {
        require!(ledger.hand < hand.hand_id, RiverproofError::InvalidPotFee);
        ledger.hand = hand.hand_id;
        ledger.paid = 0;
    }
    require!(ledger.paid & (1 << seat_index) == 0, RiverproofError::PotFeeAlreadyPaid);
    valid_mint(&ctx.accounts.mint.to_account_info())?;
    let rest = ctx.remaining_accounts;
    let payer = ctx.accounts.payer.to_account_info();
    let vault = ctx.accounts.vault.to_account_info();
    let bank = ctx.accounts.bank.to_account_info();
    let mint = ctx.accounts.mint.to_account_info();
    let token = ctx.accounts.token_program.to_account_info();
    let system = ctx.accounts.system_program.to_account_info();
    let accounts = vec![payer, vault.clone(), vault.clone(), rest[0].clone(), bank, mint,
        rest[1].clone(), token, rest[2].clone(), rest[3].clone(), rest[4].clone(),
        rest[5].clone(), rest[6].clone(), rest[7].clone(), rest[8].clone(), system];
    let writable = [true, false, true, false, false, false, true, false, true, true, false, true, true, true, true, false];
    let keys = accounts.iter().enumerate().map(|(i, a)| {
        if writable[i] { AccountMeta::new(*a.key, i == 0 || i == 1) }
        else { AccountMeta::new_readonly(*a.key, i == 0 || i == 1) }
    }).collect();
    let mut data = vec![71];
    data.extend_from_slice(&hand.hand_id.to_le_bytes());
    data.extend_from_slice(&base_units(amount)?.to_le_bytes());
    let config = ctx.accounts.config.key();
    let bump = [ctx.accounts.config.vault_bump];
    let signer: &[&[&[u8]]] = &[&[VAULT_SEED, config.as_ref(), &bump]];
    invoke_signed(&Instruction { program_id: ctx.accounts.bank_program.key(), accounts: keys, data }, &accounts, signer)?;
    // The CPI and all accounting writes roll back as one operation on failure.
    let table = &mut ctx.accounts.table_state;
    table.seats[seat_index as usize].stack = seat.stack.checked_sub(amount).ok_or(RiverproofError::VaultUnderfunded)?;
    table.escrowed = table.escrowed.checked_sub(amount).ok_or(RiverproofError::VaultUnderfunded)?;
    table.version = table.version.checked_add(1).ok_or(RiverproofError::InvalidPotFee)?;
    ledger.paid |= 1 << seat_index;
    assert_chips_conserved(table, hand)
}
