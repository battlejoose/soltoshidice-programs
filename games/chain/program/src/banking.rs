//! Version 4 banker-first rules. Legacy wagers continue using result()/payout().
use super::*;

pub fn instant(bank: [u8; 3]) -> Option<u8> {
    if bank == [0; 3] { return Some(2); }
    match score(bank) {
        1 | 11 => Some(1),
        16 | 100 | 201..=206 => Some(0),
        _ => None,
    }
}
pub fn outcome(bank: [u8; 3], player: [u8; 3]) -> u8 {
    if let Some(value) = instant(bank) { return value; }
    if player == [0; 3] { return 2; }
    if score(player) > score(bank) { 1 } else { 0 }
}
pub fn gross(wager: u64, outcome: u8, player: [u8; 3]) -> Result<u64, ProgramError> {
    let multiplier = match outcome {
        0 => 0, 2 => 1,
        1 => if player != [0; 3] && score(player) >= 100 { 3 } else { 2 },
        _ => return Err(ProgramError::InvalidArgument),
    };
    wager.checked_mul(multiplier).ok_or(ProgramError::ArithmeticOverflow)
}
pub fn activate(program: &Pubkey, accounts: &[AccountInfo], data: &[u8]) -> ProgramResult {
    check(data == [31], 9)?;
    let mut iter = accounts.iter();
    let authority = next_account_info(&mut iter)?;
    let bank_account = next_account_info(&mut iter)?;
    let table_account = next_account_info(&mut iter)?;
    let pool_account = next_account_info(&mut iter)?;
    signer(authority)?;
    let mut bank = load_bank(program, bank_account)?;
    check(bank.authority == *authority.key && bank.version == 3, 11)?;
    let table = shared::load_table(program, table_account, bank_account.key)?;
    let pool = economy::load_pool(program, pool_account, bank_account.key)?;
    check(pool.active && pool.version == 2, 68)?;
    check(bank.reserved == 0 && table.current == 0 && table.queued == 0, 64)?;
    bank.version = 4;
    write(bank_account, &bank)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn exact_banking_odds_and_total_returns() {
        let hands: Vec<[u8;3]> = (1..=6).flat_map(|a| (1..=6).flat_map(move |b| (1..=6).map(move |c| [a,b,c])))
            .filter(|d| score(*d)>0).collect();
        assert_eq!(hands.len(),108);
        let (mut wins,mut bonus,mut returned)=(0,0,0);
        for b in &hands { for p in &hands {
            let o=outcome(*b,*p);
            let rolled=if instant(*b).is_some() {[0;3]} else {*p};
            let value=gross(1,o,rolled).unwrap();
            wins+=usize::from(o==1);bonus+=usize::from(value==3);returned+=value;
        }}
        assert_eq!((wins,bonus,returned),(5238,720,11196));
        assert_eq!(outcome([2,2,4],[5,5,4]),0);
        assert_eq!(gross(1000,1,[4,5,6]).unwrap(),3000);
        assert_eq!(gross(1000,1,[1,1,1]).unwrap(),3000);
        assert_eq!(gross(1000,1,[0;3]).unwrap(),2000);
        assert_eq!(gross(1000,2,[0;3]).unwrap(),1000);
    }
}
