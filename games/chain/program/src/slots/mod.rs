use crate::*;
use sdice_slots_rules as rules;
use solana_program::instruction::{AccountMeta, Instruction};

mod rewards;
#[cfg(test)]
mod tests;
pub const CALLBACK: [u8; 8] = *b"SLOTCB01";
pub const CONFIG_SEED: &[u8] = b"slots-config-v1";
pub const CURSOR_SEED: &[u8] = b"slots-player-v1";
pub const SPIN_SEED: &[u8] = b"slots-spin-v1";
pub const CONFIG_LEN: usize = 50;
pub const CURSOR_LEN: usize = 82;
pub const SPIN_LEN: usize = 261;
pub const CALLBACK_SECONDS: i64 = 180;
const PENDING: u8 = 1;
const RANDOM: u8 = 2;
const SETTLED: u8 = 3;
const REFUNDED: u8 = 4;

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct Config {
    pub magic: [u8; 8],
    pub bank: Pubkey,
    pub enabled: bool,
    pub max_wager: u64,
    pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct Cursor {
    pub magic: [u8; 8],
    pub bank: Pubkey,
    pub player: Pubkey,
    pub next_nonce: u64,
    pub pending: bool,
    pub bump: u8,
}
#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct Spin {
    pub magic: [u8; 8],
    pub bank: Pubkey,
    pub player: Pubkey,
    pub destination: Pubkey,
    pub config: Pubkey,
    pub nonce: u64,
    pub wager: u64,
    pub liability: u64,
    pub seed: [u8; 32],
    pub random: [u8; 32],
    pub requested_slot: u64,
    pub fulfilled_slot: u64,
    pub deadline: i64,
    pub status: u8,
    pub stops: [u8; 3],
    pub gross: u64,
    pub bump: u8,
}
pub fn config_address(program: &Pubkey, bank: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CONFIG_SEED, bank.as_ref()], program)
}
pub fn cursor_address(program: &Pubkey, bank: &Pubkey, player: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[CURSOR_SEED, bank.as_ref(), player.as_ref()], program)
}
pub fn spin_address(program: &Pubkey, cursor: &Pubkey, nonce: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[SPIN_SEED, cursor.as_ref(), &nonce.to_le_bytes()], program)
}
fn config(program: &Pubkey, a: &AccountInfo, bank: &Pubkey) -> Result<Config, ProgramError> {
    check(a.owner == program && a.data_len() == CONFIG_LEN, 900)?;
    let s = Config::try_from_slice(&a.try_borrow_data()?)?;
    let (key, bump) = config_address(program, bank);
    check(
        *a.key == key
            && s.magic == *b"SLOTSC01"
            && s.bank == *bank
            && s.bump == bump
            && rules::valid_wager(s.max_wager),
        900,
    )?;
    Ok(s)
}
fn cursor(
    program: &Pubkey,
    a: &AccountInfo,
    bank: &Pubkey,
    player: &Pubkey,
) -> Result<Cursor, ProgramError> {
    check(a.owner == program && a.data_len() == CURSOR_LEN, 901)?;
    let s = Cursor::try_from_slice(&a.try_borrow_data()?)?;
    let (key, bump) = cursor_address(program, bank, player);
    check(
        *a.key == key
            && s.magic == *b"SLOTSP01"
            && s.bank == *bank
            && s.player == *player
            && s.bump == bump,
        901,
    )?;
    Ok(s)
}
fn spin(program: &Pubkey, a: &AccountInfo) -> Result<Spin, ProgramError> {
    check(a.owner == program && a.data_len() == SPIN_LEN, 902)?;
    let s = Spin::try_from_slice(&a.try_borrow_data()?)?;
    let p = cursor_address(program, &s.bank, &s.player).0;
    let (key, bump) = spin_address(program, &p, s.nonce);
    check(
        *a.key == key
            && s.magic == *b"SLOTSS01"
            && s.bump == bump
            && s.config == config_address(program, &s.bank).0
            && (PENDING..=REFUNDED).contains(&s.status)
            && rules::liability(s.wager) == Some(s.liability),
        902,
    )?;
    Ok(s)
}
fn callback_result(
    s: &mut Spin,
    nonce: u64,
    seed: &[u8; 32],
    random: &[u8; 32],
    now: i64,
    slot: u64,
) -> ProgramResult {
    check(
        s.status == PENDING
            && nonce == s.nonce
            && *seed == s.seed
            && slot > s.requested_slot
            && now <= s.deadline,
        903,
    )?;
    s.random = *random;
    s.fulfilled_slot = slot;
    s.status = RANDOM;
    Ok(())
}
pub fn callback(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(
        a.len() == 2 && data.len() == 80 && data.starts_with(&CALLBACK),
        9,
    )?;
    let identity = Pubkey::find_program_address(
        &[b"identity", program.as_ref()],
        &blackjack::magicblock::PROGRAM,
    )
    .0;
    check(
        *a[0].key == identity && a[0].is_signer && a[1].is_writable,
        904,
    )?;
    let mut s = spin(program, &a[1])?;
    let clock = Clock::get()?;
    callback_result(
        &mut s,
        u64_at(data, 40)?,
        &bytes32(data, 48)?,
        &bytes32(data, 8)?,
        clock.unix_timestamp,
        clock.slot,
    )?;
    write(&a[1], &s)?;
    solana_program::log::sol_log_data(&[
        b"SDICE_SLOTS_RANDOM_V1",
        a[1].key.as_ref(),
        &a[1].try_borrow_data()?,
    ]);
    Ok(())
}
fn finish(s: &mut Spin, bank_reserved: u64, refund: bool, now: i64) -> Result<u64, ProgramError> {
    let remaining = bank_reserved
        .checked_sub(s.liability)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    if refund {
        check(s.status == PENDING && now > s.deadline, 905)?;
        s.status = REFUNDED;
        s.gross = s.wager;
    } else {
        check(
            s.status == RANDOM && s.fulfilled_slot > s.requested_slot,
            905,
        )?;
        s.stops = rules::stops_from_randomness(&s.random, &s.seed)
            .ok_or(ProgramError::InvalidAccountData)?;
        s.gross = rules::gross_return(s.wager, s.stops).ok_or(ProgramError::InvalidArgument)?;
        s.status = SETTLED;
    }
    Ok(remaining)
}

#[derive(BorshSerialize)]
struct Meta {
    pubkey: [u8; 32],
    is_signer: bool,
    is_writable: bool,
}
#[derive(BorshSerialize)]
struct Request {
    caller_seed: [u8; 32],
    callback_program_id: Pubkey,
    callback_discriminator: Vec<u8>,
    callback_accounts_metas: Vec<Meta>,
    callback_args: Vec<u8>,
}

pub fn process<'a>(program: &Pubkey, a: &[AccountInfo<'a>], data: &[u8]) -> ProgramResult {
    match data.first().copied() {
        Some(106) => configure(program, a, data),
        Some(107) => open(program, a, data),
        Some(108) => accept(program, a, data),
        Some(109) => settle(program, a, data, false),
        Some(110) => settle(program, a, data, true),
        Some(111) => close(program, a, data),
        _ => Err(ProgramError::InvalidInstructionData),
    }
}
/// 106 max_wager:u64 enabled:u8. Authority(S,W), bank, config(W), System.
fn configure(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 4 && data.len() == 10 && data[9] <= 1, 9)?;
    signer(&a[0])?;
    let bank = load_bank(program, &a[1])?;
    let cap = u64_at(data, 1)?;
    check(
        bank.version == 4
            && bank.authority == *a[0].key
            && rules::valid_wager(cap)
            && cap <= bank.max_wager,
        906,
    )?;
    let (key, bump) = config_address(program, a[1].key);
    check(key == *a[2].key, 900)?;
    if a[2].owner == program {
        config(program, &a[2], a[1].key)?;
    } else {
        create_pda(
            &a[0],
            &a[2],
            &a[3],
            program,
            CONFIG_LEN,
            &[CONFIG_SEED, a[1].key.as_ref(), &[bump]],
        )?;
    }
    write(
        &a[2],
        &Config {
            magic: *b"SLOTSC01",
            bank: *a[1].key,
            enabled: data[9] == 1,
            max_wager: cap,
            bump,
        },
    )
}
/// 107. Player(S,W), bank, permanent cursor(W), System, registered profile.
/// Keeping the cursor prevents replay after refundable spin/fee receipts close.
fn open(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 5 && data.len() == 1, 9)?;
    signer(&a[0])?;
    load_bank(program, &a[1])?;
    economy::load_profile(program, &a[4], a[1].key, a[0].key)?;
    if a[2].owner == program {
        cursor(program, &a[2], a[1].key, a[0].key)?;
        return Ok(());
    }
    let (key, bump) = cursor_address(program, a[1].key, a[0].key);
    check(key == *a[2].key, 901)?;
    create_pda(
        &a[0],
        &a[2],
        &a[3],
        program,
        CURSOR_LEN,
        &[CURSOR_SEED, a[1].key.as_ref(), a[0].key.as_ref(), &[bump]],
    )?;
    write(
        &a[2],
        &Cursor {
            magic: *b"SLOTSP01",
            bank: *a[1].key,
            player: *a[0].key,
            next_nonce: 0,
            pending: false,
            bump,
        },
    )
}
/// 108 nonce:u64 wager:u64. Player(S,W), bank(W), config, cursor(W), spin(W),
/// mint, bank token(W), player token(W), Token-2022, System, caller identity,
/// standard queue(W), slot hashes, VRF executable, pool, profile,
/// prepaid fee receipt(W), derived event. Player pays request fee/rents directly.
#[inline(never)]
fn accept<'a>(program: &Pubkey, a: &[AccountInfo<'a>], data: &[u8]) -> ProgramResult {
    check(a.len() == 18 && data.len() == 17, 9)?;
    signer(&a[0])?;
    let mut bank = load_bank(program, &a[1])?;
    let cfg = config(program, &a[2], a[1].key)?;
    let mut p = cursor(program, &a[3], a[1].key, a[0].key)?;
    let nonce = u64_at(data, 1)?;
    let wager = u64_at(data, 9)?;
    check(
        bank.version == 4
            && cfg.enabled
            && rules::valid_wager(wager)
            && wager <= cfg.max_wager
            && wager <= bank.max_wager,
        906,
    )?;
    check(!p.pending && nonce == p.next_nonce, 907)?;
    p.next_nonce = p
        .next_nonce
        .checked_add(1)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    p.pending = true;
    check(
        bank.mint == *a[5].key
            && bank.vault == *a[6].key
            && *a[8].key == spl_token_2022::id()
            && *a[9].key == system_program::id(),
        14,
    )?;
    check(valid_mint(&a[5])? == 6, 5)?;
    let ata = Pubkey::find_program_address(
        &[
            a[0].key.as_ref(),
            spl_token_2022::id().as_ref(),
            a[5].key.as_ref(),
        ],
        &solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL"),
    )
    .0;
    check(*a[7].key == ata, 14)?;
    let balance = token_account(&a[6], a[5].key, a[1].key)?;
    check(token_account(&a[7], a[5].key, a[0].key)? >= wager, 15)?;
    let pool = economy::load_pool(program, &a[14], a[1].key)?;
    check(pool.active && pool.total_shares > 0, 63)?;
    economy::load_profile(program, &a[15], a[1].key, a[0].key)?;
    bank.reserved =
        rules::reserve_spin(balance, bank.reserved, wager).ok_or(ProgramError::Custom(908))?;
    let (key, bump) = spin_address(program, a[3].key, nonce);
    check(key == *a[4].key, 902)?;
    let clock = Clock::get()?;
    let s = Spin {
        magic: *b"SLOTSS01",
        bank: *a[1].key,
        player: *a[0].key,
        destination: *a[7].key,
        config: *a[2].key,
        nonce,
        wager,
        liability: rules::liability(wager).ok_or(ProgramError::ArithmeticOverflow)?,
        seed: hashv(&[
            b"sdice:slots:request:v1",
            program.as_ref(),
            a[4].key.as_ref(),
            &wager.to_le_bytes(),
        ])
        .to_bytes(),
        random: [0; 32],
        requested_slot: clock.slot,
        fulfilled_slot: 0,
        deadline: clock
            .unix_timestamp
            .checked_add(CALLBACK_SECONDS)
            .ok_or(ProgramError::ArithmeticOverflow)?,
        status: PENDING,
        stops: [0; 3],
        gross: 0,
        bump,
    };
    create_pda(
        &a[0],
        &a[4],
        &a[9],
        program,
        SPIN_LEN,
        &[SPIN_SEED, a[3].key.as_ref(), &nonce.to_le_bytes(), &[bump]],
    )?;
    rewards::prepare(program, &a[0], &a[9], &a[16], &a[17], &s)?;
    transfer(&a[7], &a[6], &a[5], &a[0], &a[8], wager, 6, None)?;
    write(&a[1], &bank)?;
    write(&a[3], &p)?;
    write(&a[4], &s)?;
    let fee = blackjack::magicblock::fee(&a[11])?;
    let (identity, ibump) = Pubkey::find_program_address(&[b"identity"], program);
    check(
        *a[10].key == identity
            && *a[12].key == solana_program::sysvar::slot_hashes::id()
            && *a[13].key == blackjack::magicblock::PROGRAM
            && a[13].executable,
        904,
    )?;
    let before = a[0].lamports();
    check(before >= fee, 909)?;
    let mut args = nonce.to_le_bytes().to_vec();
    args.extend_from_slice(&s.seed);
    let request = Request {
        caller_seed: s.seed,
        callback_program_id: *program,
        callback_discriminator: CALLBACK.to_vec(),
        callback_accounts_metas: vec![Meta {
            pubkey: key.to_bytes(),
            is_signer: false,
            is_writable: true,
        }],
        callback_args: args,
    };
    let mut payload = vec![0u8; 8];
    payload[0] = 10;
    payload.extend(borsh::to_vec(&request)?);
    invoke_signed(
        &Instruction {
            program_id: blackjack::magicblock::PROGRAM,
            accounts: vec![
                AccountMeta::new(*a[0].key, true),
                AccountMeta::new_readonly(identity, true),
                AccountMeta::new(*a[11].key, false),
                AccountMeta::new_readonly(system_program::id(), false),
                AccountMeta::new_readonly(*a[12].key, false),
            ],
            data: payload,
        },
        &[
            a[0].clone(),
            a[10].clone(),
            a[11].clone(),
            a[9].clone(),
            a[12].clone(),
            a[13].clone(),
        ],
        &[&[b"identity", &[ibump]]],
    )?;
    check(before.checked_sub(a[0].lamports()) == Some(fee), 909)?;
    msg!(
        "SDICE_SLOTS_ACCEPT spin={} player={} nonce={} wager={} liability={}",
        key,
        s.player,
        nonce,
        wager,
        s.liability
    );
    Ok(())
}

/// 109 settle / 110 expired refund. Bank(W), spin(W), cursor(W), mint,
/// bank token(W), original player token(W), Token-2022, pool(W), fee token(W),
/// fee receipt(W), event, player profile(W), immutable direct profile(W).
/// Refund additionally returns the uncredited receipt rent to player(W), index13.
/// Permissionless caller pays their own network fee; no house SOL is debited.
#[inline(never)]
fn settle<'a>(program: &Pubkey, a: &[AccountInfo<'a>], data: &[u8], refund: bool) -> ProgramResult {
    check(
        data.len() == 1 && a.len() == if refund { 14 } else { 13 },
        9,
    )?;
    let mut bank = load_bank(program, &a[0])?;
    let mut s = spin(program, &a[1])?;
    check(
        s.bank == *a[0].key
            && bank.version == 4
            && bank.mint == *a[3].key
            && bank.vault == *a[4].key
            && s.destination == *a[5].key
            && *a[6].key == spl_token_2022::id(),
        14,
    )?;
    check(valid_mint(&a[3])? == 6, 5)?;
    token_account(&a[5], a[3].key, &s.player)?;
    let mut p = cursor(program, &a[2], a[0].key, &s.player)?;
    check(
        p.pending
            && p.next_nonce
                == s.nonce
                    .checked_add(1)
                    .ok_or(ProgramError::ArithmeticOverflow)?,
        907,
    )?;
    let remainder = finish(&mut s, bank.reserved, refund, Clock::get()?.unix_timestamp)?;
    let reward = if refund {
        0
    } else {
        rules::rewards(s.wager)
            .ok_or(ProgramError::InvalidArgument)?
            .total
    };
    let spend = s
        .gross
        .checked_add(reward)
        .ok_or(ProgramError::ArithmeticOverflow)?;
    check(
        token_account(&a[4], a[3].key, a[0].key)?
            .checked_sub(spend)
            .is_some_and(|v| v >= remainder),
        908,
    )?;
    bank.reserved = remainder;
    if refund {
        rewards::cancel(program, &a[9], &a[10], &a[13], &s)?;
    } else {
        rewards::credit(program, a, &bank, &s)?;
    }
    if s.gross > 0 {
        transfer(
            &a[4],
            &a[5],
            &a[3],
            &a[0],
            &a[6],
            s.gross,
            6,
            Some(&[
                b"bank",
                bank.authority.as_ref(),
                bank.mint.as_ref(),
                &[bank.bump],
            ]),
        )?;
    }
    p.pending = false;
    write(&a[0], &bank)?;
    write(&a[2], &p)?;
    write(&a[1], &s)?;
    solana_program::log::sol_log_data(&[
        b"SDICE_SLOTS_RESULT_V1",
        a[1].key.as_ref(),
        &a[1].try_borrow_data()?,
    ]);
    msg!(
        "SDICE_SLOTS_SETTLED spin={} status={} wager={} gross={} rewards={}",
        a[1].key,
        s.status,
        s.wager,
        s.gross,
        reward
    );
    Ok(())
}
/// 111. Bank, terminal spin(W), original player(W). Cursor remains for replay protection.
fn close(program: &Pubkey, a: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(a.len() == 3 && data.len() == 1, 9)?;
    load_bank(program, &a[0])?;
    let s = spin(program, &a[1])?;
    check(
        s.bank == *a[0].key
            && s.player == *a[2].key
            && (s.status == SETTLED || s.status == REFUNDED),
        905,
    )?;
    return_rent(&a[1], &a[2])
}
fn return_rent(from: &AccountInfo, to: &AccountInfo) -> ProgramResult {
    check(
        from.is_writable && to.is_writable && from.key != to.key,
        910,
    )?;
    **to.try_borrow_mut_lamports()? = to
        .lamports()
        .checked_add(from.lamports())
        .ok_or(ProgramError::ArithmeticOverflow)?;
    **from.try_borrow_mut_lamports()? = 0;
    from.realloc(0, false)?;
    from.assign(&system_program::id());
    Ok(())
}
