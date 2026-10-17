//! Phase 1 - on-chain identity, and Phase 2 - the play-money faucet.
//!
//! Username uniqueness is enforced without a server: `UsernameClaim` is a PDA
//! seeded on the normalized username, so a second claim fails because the
//! runtime will not initialize the same address twice.

use anchor_lang::prelude::*;
use anchor_spl::associated_token::AssociatedToken;
use anchor_spl::token_interface::{Mint, TokenAccount};
use anchor_spl::token_2022::{self as token, MintTo, Token2022 as Token};
use crate::sdice::{base_units, DECIMALS};

use crate::errors::RiverproofError;
use crate::state::{
    FaucetConfig, PlayerProfile, UsernameClaim, FAUCET_SEED, MAX_USERNAME_LEN, MIN_USERNAME_LEN,
    PROFILE_SEED, USERNAME_SEED,
};

/// Lowercases and validates a username.
///
/// Letters, digits, and underscores only, so the normalized form is safe to use
/// directly as a PDA seed and cannot collide through case or whitespace tricks.
pub fn normalize_username(username: &str) -> Result<String> {
    require!(
        (MIN_USERNAME_LEN..=MAX_USERNAME_LEN).contains(&username.len()),
        RiverproofError::InvalidUsername
    );
    require!(
        username
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_'),
        RiverproofError::InvalidUsername
    );
    Ok(username.to_ascii_lowercase())
}

#[derive(Accounts)]
#[instruction(username: String, normalized: String)]
pub struct CreateProfile<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,

    #[account(
        init,
        payer = wallet,
        space = 8 + PlayerProfile::INIT_SPACE,
        seeds = [PROFILE_SEED, wallet.key().as_ref()],
        bump
    )]
    pub profile: Account<'info, PlayerProfile>,

    #[account(
        init,
        payer = wallet,
        space = 8 + UsernameClaim::INIT_SPACE,
        seeds = [USERNAME_SEED, normalized.as_bytes()],
        bump
    )]
    pub claim: Account<'info, UsernameClaim>,

    pub system_program: Program<'info, System>,
}

pub fn create_profile(
    ctx: Context<CreateProfile>,
    username: String,
    normalized: String,
) -> Result<()> {
    // The seed came from instruction data, so the normalization has to be
    // re-derived on-chain rather than trusted.
    require!(
        normalize_username(&username)? == normalized,
        RiverproofError::InvalidUsername
    );

    let now = Clock::get()?.unix_timestamp;
    let profile = &mut ctx.accounts.profile;
    profile.wallet = ctx.accounts.wallet.key();
    profile.username = username;
    profile.normalized = normalized.clone();
    profile.created_at = now;
    profile.hands_played = 0;
    profile.hands_won = 0;
    // Backdated so a fresh profile can claim immediately.
    profile.last_faucet_claim = 0;
    profile.bump = ctx.bumps.profile;

    let claim = &mut ctx.accounts.claim;
    claim.owner = ctx.accounts.wallet.key();
    claim.normalized = normalized;
    claim.bump = ctx.bumps.claim;

    Ok(())
}

#[derive(Accounts)]
#[instruction(username: String, normalized: String)]
pub struct ChangeUsername<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,

    #[account(
        mut,
        seeds = [PROFILE_SEED, wallet.key().as_ref()],
        bump = profile.bump,
        has_one = wallet @ RiverproofError::UnauthorizedActor
    )]
    pub profile: Account<'info, PlayerProfile>,

    /// The claim being released. Rent returns to the owner.
    #[account(
        mut,
        close = wallet,
        seeds = [USERNAME_SEED, profile.normalized.as_bytes()],
        bump = old_claim.bump,
        constraint = old_claim.owner == wallet.key() @ RiverproofError::UnauthorizedActor
    )]
    pub old_claim: Account<'info, UsernameClaim>,

    #[account(
        init,
        payer = wallet,
        space = 8 + UsernameClaim::INIT_SPACE,
        seeds = [USERNAME_SEED, normalized.as_bytes()],
        bump
    )]
    pub new_claim: Account<'info, UsernameClaim>,

    pub system_program: Program<'info, System>,
}

pub fn change_username(
    ctx: Context<ChangeUsername>,
    username: String,
    normalized: String,
) -> Result<()> {
    require!(
        normalize_username(&username)? == normalized,
        RiverproofError::InvalidUsername
    );

    let profile = &mut ctx.accounts.profile;
    profile.username = username;
    profile.normalized = normalized.clone();

    let claim = &mut ctx.accounts.new_claim;
    claim.owner = ctx.accounts.wallet.key();
    claim.normalized = normalized;
    claim.bump = ctx.bumps.new_claim;

    Ok(())
}

#[derive(Accounts)]
pub struct InitializeFaucet<'info> {
    #[account(mut)]
    pub authority: Signer<'info>,

    // This account also establishes the operator that can close/refund tables.
    // Only this deployment's verified upgrade authority may reserve that role.
    #[account(
        address = Pubkey::find_program_address(&[crate::ID.as_ref()], &ProgramData::owner()).0 @ RiverproofError::UnauthorizedActor,
        constraint = program_data.upgrade_authority_address == Some(authority.key()) @ RiverproofError::UnauthorizedActor
    )]
    pub program_data: Account<'info, ProgramData>,

    #[account(
        init,
        payer = authority,
        space = 8 + FaucetConfig::INIT_SPACE,
        seeds = [FAUCET_SEED],
        bump
    )]
    pub faucet: Account<'info, FaucetConfig>,

    /// Isolated devnet test mint. Six decimals, matching SDICE custody;
    /// the poker engine itself continues to count whole tokens.
    #[account(
        init,
        payer = authority,
        seeds = [b"mint"],
        bump,
        mint::decimals = DECIMALS,
        mint::token_program = token_program,
        mint::authority = faucet,
    )]
    pub mint: InterfaceAccount<'info, Mint>,

    pub token_program: Program<'info, Token>,
    pub system_program: Program<'info, System>,
    pub rent: Sysvar<'info, Rent>,
}

pub fn initialize_faucet(
    ctx: Context<InitializeFaucet>,
    claim_amount: u64,
    cooldown_seconds: i64,
) -> Result<()> {
    let faucet = &mut ctx.accounts.faucet;
    faucet.mint = ctx.accounts.mint.key();
    faucet.authority = ctx.accounts.authority.key();
    faucet.claim_amount = claim_amount;
    faucet.cooldown_seconds = cooldown_seconds;
    faucet.bump = ctx.bumps.faucet;
    Ok(())
}

#[derive(Accounts)]
pub struct ClaimFaucet<'info> {
    #[account(mut)]
    pub wallet: Signer<'info>,

    #[account(
        mut,
        seeds = [PROFILE_SEED, wallet.key().as_ref()],
        bump = profile.bump,
        has_one = wallet @ RiverproofError::UnauthorizedActor
    )]
    pub profile: Account<'info, PlayerProfile>,

    #[account(seeds = [FAUCET_SEED], bump = faucet.bump, has_one = mint)]
    pub faucet: Account<'info, FaucetConfig>,

    #[account(mut)]
    pub mint: InterfaceAccount<'info, Mint>,

    #[account(
        init_if_needed,
        payer = wallet,
        associated_token::mint = mint,
        associated_token::authority = wallet,
        associated_token::token_program = token_program,
    )]
    pub destination: InterfaceAccount<'info, TokenAccount>,

    pub token_program: Program<'info, Token>,
    pub associated_token_program: Program<'info, AssociatedToken>,
    pub system_program: Program<'info, System>,
}

/// Open faucet: anyone with a profile can top up on a cooldown.
///
/// These are play-money chips with no redemption path, so the only thing the
/// cooldown protects is state churn.
pub fn claim_faucet(ctx: Context<ClaimFaucet>) -> Result<()> {
    let now = Clock::get()?.unix_timestamp;
    let faucet = &ctx.accounts.faucet;
    let profile = &mut ctx.accounts.profile;

    require!(
        now.saturating_sub(profile.last_faucet_claim) >= faucet.cooldown_seconds,
        RiverproofError::FaucetCooldown
    );
    profile.last_faucet_claim = now;

    let bump = [faucet.bump];
    let signer_seeds: &[&[&[u8]]] = &[&[FAUCET_SEED, &bump]];

    token::mint_to(
        CpiContext::new_with_signer(
            ctx.accounts.token_program.key(),
            MintTo {
                mint: ctx.accounts.mint.to_account_info(),
                to: ctx.accounts.destination.to_account_info(),
                authority: ctx.accounts.faucet.to_account_info(),
            },
            signer_seeds,
        ),
        base_units(faucet.claim_amount)?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_case() {
        assert_eq!(normalize_username("CrowBrow").unwrap(), "crowbrow");
        assert_eq!(normalize_username("river_proof99").unwrap(), "river_proof99");
    }

    #[test]
    fn rejects_bad_usernames() {
        assert!(normalize_username("ab").is_err());
        assert!(normalize_username(&"a".repeat(21)).is_err());
        assert!(normalize_username("has space").is_err());
        assert!(normalize_username("dash-not-allowed").is_err());
        assert!(normalize_username("emoji🙂").is_err());
    }
}
