use super::rules;
use crate::*;

pub const TABLE_SEED: &[u8] = b"blackjack-table-v1";
pub const ORACLE_SEED: &[u8] = b"blackjack-oracle-v1";
pub const SESSION_SECONDS: i64 = 86_400;
pub const IDLE_SECONDS: i64 = 300;
pub const RESULT_SECONDS: i64 = 12;
pub const HAND_LEN: usize = 42;
pub const PLAYER_LEN: usize = 25 + 4 * HAND_LEN;
pub const ROUND_LEN: usize = 16 + 4 + 8 + 8 + 52 + 2 + HAND_LEN + 10 * PLAYER_LEN + 32;
pub const SEAT_LEN: usize = 32 + 32 + 8 + 8 + 8 + 8;
pub const TABLE_LEN: usize = 8 + 1 + 32 + 32 + 8 + 1 + 8 + 8 + 1 + 10 * SEAT_LEN + 320 + ROUND_LEN + 8;
const MAGIC: [u8; 8] = *b"SDICEBJ1";

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default)]
pub struct Seat {
    pub wallet: Pubkey,
    pub session: Pubkey,
    pub expires: i64,
    pub allowance: u64,
    pub max_wager: u64,
    pub last_action: i64,
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug)]
pub struct Table {
    pub magic: [u8; 8], pub version: u8, pub bank: Pubkey, pub escrow: Pubkey,
    pub id: u64, pub bump: u8, pub min_wager: u64, pub max_wager: u64, pub enabled: bool,
    #[borsh(deserialize_with = "super::read_fixed")]
    pub seats: Box<[Seat; rules::SEATS]>,
    // Snapshot identities survive cash-out/reseating while results remain visible.
    pub round_wallets: [Pubkey; rules::SEATS],
    pub round: rules::Round, pub finished_at: i64,
}
impl Table {
    pub fn new(bank: Pubkey, escrow: Pubkey, id: u64, bump: u8, min: u64, max: u64) -> Self {
        Self { magic: MAGIC, version: 1, bank, escrow, id, bump, min_wager: min,
            max_wager: max, enabled: true, seats: Box::new([Seat::default(); rules::SEATS]),
            round_wallets: [Pubkey::default(); rules::SEATS], round: rules::Round::default(), finished_at: 0 }
    }
    pub fn guard(&self, data: &[u8]) -> ProgramResult {
        check(u64_at(data, 1)? == self.round.id && u64_at(data, 9)? == self.round.revision, 821)
    }
    pub fn changed(&mut self) -> ProgramResult {
        self.round.revision = self.round.revision.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
        Ok(())
    }
    pub fn idle(&self, seat: usize) -> bool {
        !self.round.active() || self.round.players[seat].original == 0
    }
    pub fn authorize(&mut self, actor: &AccountInfo, seat: usize, now: i64, spent: u64) -> ProgramResult {
        signer(actor)?;
        let identity = self.seats.get_mut(seat).ok_or(ProgramError::InvalidInstructionData)?;
        check(identity.wallet != Pubkey::default(), 822)?;
        if *actor.key != identity.wallet {
            check(*actor.key == identity.session && now < identity.expires, 823)?;
            if spent > 0 {
                check(spent <= identity.max_wager && spent <= identity.allowance, 824)?;
                identity.allowance -= spent;
            }
        }
        identity.last_action = now;
        Ok(())
    }
    pub fn seed(&self, program: &Pubkey, table: &Pubkey) -> [u8; 32] {
        hashv(&[b"sdice:blackjack:request:v1", program.as_ref(), table.as_ref(),
            &self.round.id.to_le_bytes(), &self.round.draw_nonce.to_le_bytes(),
            &[self.round.draw as u8, self.round.actor, self.round.hand], &self.round.transcript]).to_bytes()
    }
}
pub fn address(program: &Pubkey, bank: &Pubkey, id: u64) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[TABLE_SEED, bank.as_ref(), &id.to_le_bytes()], program)
}
pub fn oracle_payer(program: &Pubkey, table: &Pubkey) -> (Pubkey, u8) {
    Pubkey::find_program_address(&[ORACLE_SEED, table.as_ref()], program)
}
pub fn token_address(mint: &Pubkey, owner: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[owner.as_ref(), spl_token_2022::id().as_ref(), mint.as_ref()],
        &solana_program::pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL")).0
}
#[inline(never)]
pub fn load(program: &Pubkey, account: &AccountInfo, bank: &Pubkey) -> Result<Box<Table>, ProgramError> {
    check(account.owner == program && account.data_len() == TABLE_LEN, 820)?;
    let t = Box::new(Table::try_from_slice(&account.try_borrow_data()?).map_err(|_| ProgramError::InvalidAccountData)?);
    let (expected, bump) = address(program, bank, t.id);
    check(t.magic == MAGIC && (1..=4).contains(&t.version) && t.bank == *bank && *account.key == expected && t.bump == bump, 820)?;
    Ok(t)
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn account_lengths_and_request_identity() {
        let mut t = Table::new(Pubkey::new_unique(), Pubkey::new_unique(), 1, 1, 2, 100);
        assert_eq!(borsh::to_vec(&rules::Hand::default()).unwrap().len(), HAND_LEN);
        assert_eq!(borsh::to_vec(&rules::Player::default()).unwrap().len(), PLAYER_LEN);
        assert_eq!(borsh::to_vec(&t.round).unwrap().len(), ROUND_LEN);
        assert_eq!(borsh::to_vec(&t).unwrap().len(), TABLE_LEN);
        let program = Pubkey::new_unique(); let table = Pubkey::new_unique();
        let seed = t.seed(&program, &table);
        t.round.revision += 1; // Renewing a session cannot invalidate an in-flight request.
        assert_eq!(seed, t.seed(&program, &table));
        t.round.draw_nonce += 1;
        assert_ne!(seed, t.seed(&program, &table));
    }
}
