// Isolated copy of the existing Token-2022/PDA checks; no live program refactor.
use super::*;
pub(super) fn check(ok: bool, code: u32) -> ProgramResult { if ok { Ok(()) } else { Err(ProgramError::Custom(code)) } }
pub(super) fn signer(a: &AccountInfo) -> ProgramResult { check(a.is_signer, 1) }
pub(super) fn u64_at(data: &[u8], offset: usize) -> Result<u64, ProgramError> {
    Ok(u64::from_le_bytes(data.get(offset..offset+8).ok_or(ProgramError::InvalidInstructionData)?.try_into().unwrap()))
}
pub(super) fn write<T: BorshSerialize>(account: &AccountInfo, value: &T) -> ProgramResult {
    value.serialize(&mut &mut account.try_borrow_mut_data()?[..]).map_err(|_|ProgramError::InvalidAccountData)
}
pub(super) fn read<T: BorshDeserialize>(program: &Pubkey, account: &AccountInfo) -> Result<T, ProgramError> {
    check(account.owner == program, 2)?;
    T::try_from_slice(&account.try_borrow_data()?).map_err(|_|ProgramError::InvalidAccountData)
}
pub(super) fn config(program: &Pubkey, a: &AccountInfo) -> Result<Config, ProgramError> {
    let c: Config = read(program,a)?;
    let (key,bump) = Pubkey::find_program_address(&[b"rps-config-v1"],program);
    check(*a.key == key && c.bump == bump && c.version == 1,3)?; Ok(c)
}
pub(super) fn game(program: &Pubkey, a: &AccountInfo) -> Result<Match, ProgramError> {
    let f: Match = read(program,a)?;
    let (key,bump) = Pubkey::find_program_address(&[b"rps-v1",f.challenger.as_ref(),&f.nonce.to_le_bytes()],program);
    check(*a.key == key && f.bump == bump && f.version == 1,3)?; Ok(f)
}
pub(super) fn valid_mint(a: &AccountInfo) -> Result<u8, ProgramError> {
    check(*a.owner == spl_token_2022::id(),4)?;
    let data = a.try_borrow_data()?;
    let mint = StateWithExtensions::<Mint>::unpack(&data)?;
    check(mint.base.decimals == 6 && mint.base.freeze_authority.is_none()
        && mint.get_extension_types()?.iter().all(|e| matches!(e,ExtensionType::MetadataPointer | ExtensionType::TokenMetadata)),5)?;
    Ok(mint.base.decimals)
}
pub(super) fn token_account(a: &AccountInfo, mint: &Pubkey, owner: &Pubkey) -> Result<u64,ProgramError> {
    check(*a.owner == spl_token_2022::id(),4)?;
    let data = a.try_borrow_data()?;
    let t = StateWithExtensions::<TokenAccount>::unpack(&data)?;
    check(t.base.mint == *mint && t.base.owner == *owner && !t.base.is_frozen()
        && t.base.delegate.is_none() && t.base.close_authority.is_none(),6)?;
    Ok(t.base.amount)
}
pub(super) fn create_pda<'a>(payer: &AccountInfo<'a>, account: &AccountInfo<'a>, system: &AccountInfo<'a>, owner: &Pubkey, len: usize, seeds: &[&[u8]]) -> ProgramResult {
    check(*system.key == system_program::id(),7)?;
    check(account.data_is_empty() && *account.owner == system_program::id(),8)?;
    let rent = Rent::get()?.minimum_balance(len);
    if account.lamports() == 0 {
        invoke_signed(&system_instruction::create_account(payer.key,account.key,rent,len as u64,owner),&[payer.clone(),account.clone(),system.clone()],&[seeds])
    } else {
        let needed = rent.saturating_sub(account.lamports());
        if needed > 0 { invoke(&system_instruction::transfer(payer.key,account.key,needed),&[payer.clone(),account.clone(),system.clone()])?; }
        invoke_signed(&system_instruction::allocate(account.key,len as u64),&[account.clone(),system.clone()],&[seeds])?;
        invoke_signed(&system_instruction::assign(account.key,owner),&[account.clone(),system.clone()],&[seeds])
    }
}
pub(super) fn lamports(from: &AccountInfo, to: &AccountInfo, amount: u64) -> ProgramResult {
    check(from.key != to.key,6)?;
    let a = from.lamports().checked_sub(amount).ok_or(ProgramError::InsufficientFunds)?;
    let b = to.lamports().checked_add(amount).ok_or(ProgramError::ArithmeticOverflow)?;
    **from.try_borrow_mut_lamports()? = a; **to.try_borrow_mut_lamports()? = b; Ok(())
}
pub(super) fn transfer<'a>(from: &AccountInfo<'a>, to: &AccountInfo<'a>, mint: &AccountInfo<'a>, authority: &AccountInfo<'a>, token: &AccountInfo<'a>, amount: u64, seeds: Option<&[&[u8]]>) -> ProgramResult {
    check(*token.key == spl_token_2022::id() && from.key != to.key,4)?;
    let ix = spl_token_2022::instruction::transfer_checked(token.key,from.key,mint.key,to.key,authority.key,&[],amount,6)?;
    let infos = [from.clone(),mint.clone(),to.clone(),authority.clone(),token.clone()];
    if let Some(s) = seeds { invoke_signed(&ix,&infos,&[s]) } else { invoke(&ix,&infos) }
}
pub(super) fn escrow(program: &Pubkey, a: &AccountInfo, f: &AccountInfo, mint: &Pubkey) -> ProgramResult {
    check(*a.key == Pubkey::find_program_address(&[b"rps-vault-v1",f.key.as_ref()],program).0,3)?;
    token_account(a,mint,f.key)?; Ok(())
}
