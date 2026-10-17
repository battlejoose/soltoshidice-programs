use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{program_error::ProgramError, pubkey::Pubkey};

pub const DURATION_SECONDS: i64 = 3 * 60 * 60;
pub const MAX_WINNERS: usize = 10;
pub const OPEN: u8 = 1;
pub const PAYING: u8 = 2;
pub const COMPLETE: u8 = 3;
pub const CARRIED: u8 = 4;
pub const ENTRY_REFUND_FEE: u64 = 5_000;
pub const ENTRY_RECEIPTS: u32 = 2;
// SDICE has six decimals. Applied to new v3 series; existing events stay intact.
pub const MINIMUM_FUNDING: u64 = 10_000_000 * 1_000_000;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Series {
    pub version: u8,
    pub bank: Pubkey,
    pub mint: Pubkey,
    pub intake: Pubkey,
    pub current: u64,
    pub next: u64,
    pub carry: u64,
    pub bump: u8,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Standing {
    pub wallet: Pubkey,
    pub score: u128,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Event {
    pub version: u8,
    pub series: Pubkey,
    pub nonce: u64,
    pub start: i64,
    pub end: i64,
    pub status: u8,
    pub participants: u32,
    pub pending: u32,
    pub bets: u64,
    pub prize: u64,
    pub paid: u64,
    pub winners: u8,
    pub paid_mask: u16,
    pub top: [Standing; MAX_WINNERS],
    pub awards: [u64; MAX_WINNERS],
    pub bump: u8,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Entry {
    pub version: u8,
    pub event: Pubkey,
    pub wallet: Pubkey,
    pub score: u128,
    pub bets: u32,
    pub wins: u32,
    pub pending: u32,
    pub bump: u8,
    pub open_receipts: u32,
    pub refunded: bool,
}

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone)]
pub struct Receipt {
    pub version: u8,
    pub series: Pubkey,
    pub event: Pubkey,
    pub bet: Pubkey,
    pub wallet: Pubkey,
    pub wager: u64,
    pub accepted_at: i64,
    pub scored: bool,
    pub bump: u8,
}

pub const SERIES_LEN: usize = 1 + 32 * 3 + 8 * 3 + 1;
pub const EVENT_LEN: usize = 1 + 32 + 8 * 3 + 1 + 4 * 2 + 8 * 3 + 1 + 2 + (32 + 16) * MAX_WINNERS + 8 * MAX_WINNERS + 1;
pub const ENTRY_LEN: usize = 1 + 32 * 2 + 16 + 4 * 3 + 1;
pub const PAID_ENTRY_LEN: usize = ENTRY_LEN + 4 + 1;
pub const RECEIPT_LEN: usize = 1 + 32 * 4 + 8 * 2 + 1 + 1;

impl Event {
    pub fn new(series: Pubkey, nonce: u64, now: i64, bump: u8) -> Result<Self, ProgramError> {
        Ok(Self {
            version: 1, series, nonce, start: now,
            end: now.checked_add(DURATION_SECONDS).ok_or(ProgramError::ArithmeticOverflow)?,
            status: OPEN, participants: 0, pending: 0, bets: 0, prize: 0, paid: 0,
            winners: 0, paid_mask: 0, top: [Standing::default(); MAX_WINNERS],
            awards: [0; MAX_WINNERS], bump,
        })
    }

    pub fn accepts(&self, now: i64) -> bool {
        self.status == OPEN && now >= self.start && now < self.end
    }

    /// Scores only increase; remove this wallet's old entry before bounded insertion.
    pub fn rank(&mut self, wallet: Pubkey, score: u128) {
        let mut ranked: Vec<Standing> = self.top.iter().copied()
            .filter(|s| s.score > 0 && s.wallet != wallet).collect();
        if score > 0 { ranked.push(Standing { wallet, score }); }
        ranked.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.wallet.to_bytes().cmp(&b.wallet.to_bytes())));
        self.top = [Standing::default(); MAX_WINNERS];
        for (i, standing) in ranked.into_iter().take(MAX_WINNERS).enumerate() { self.top[i] = standing; }
        self.winners = self.top.iter().filter(|s| s.score > 0).count() as u8;
    }

    /// Freeze integer allocations only after all qualifying results are accounted for.
    pub fn freeze(&mut self, now: i64, prize: u64) -> Result<(), ProgramError> {
        if self.status != OPEN || now < self.end || self.pending != 0 {
            return Err(ProgramError::Custom(102));
        }
        self.prize = prize;
        self.awards = awards(prize, &self.top);
        self.status = if self.winners == 0 { CARRIED } else { PAYING };
        Ok(())
    }
}

pub fn winning_profit(wager: u64, payout: u64, outcome: u8, refunded: bool) -> Result<u64, ProgramError> {
    if refunded || outcome != 1 { return Ok(0); }
    payout.checked_sub(wager).ok_or(ProgramError::ArithmeticOverflow)
}

pub fn awards(prize: u64, top: &[Standing; MAX_WINNERS]) -> [u64; MAX_WINNERS] {
    let count = top.iter().take_while(|s| s.score > 0).count();
    let mut result = [0u64; MAX_WINNERS];
    let mut remaining = prize;
    for (i, value) in result.iter_mut().take(count).enumerate() {
        *value = if i + 1 == count { remaining } else { remaining / 2 };
        remaining -= *value;
    }
    // Equal scores share their combined occupied positions. Byte order assigns dust.
    let mut first = 0;
    while first < count {
        let mut end = first + 1;
        while end < count && top[end].score == top[first].score { end += 1; }
        let total: u64 = result[first..end].iter().sum();
        let tied = (end - first) as u64;
        for (i, value) in result[first..end].iter_mut().enumerate() {
            *value = total / tied + u64::from((i as u64) < total % tied);
        }
        first = end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> Event { Event::new(Pubkey::new_unique(), 1, 1_000, 1).unwrap() }
    #[test]
    fn clock_and_profit() {
        let mut e = event();
        let end = 1_000 + DURATION_SECONDS;
        assert_eq!(e.end, end);
        assert!(!e.accepts(999)); assert!(e.accepts(1_000)); assert!(e.accepts(end - 1)); assert!(!e.accepts(end));
        assert_eq!(winning_profit(10_000, 20_000, 1, false).unwrap(), 10_000);
        assert_eq!(winning_profit(10_000, 30_000, 1, false).unwrap(), 20_000);
        assert_eq!(winning_profit(10_000, 0, 0, false).unwrap(), 0);
        assert_eq!(winning_profit(10_000, 10_000, 2, true).unwrap(), 0);
        assert!(e.freeze(end - 1, 1_000).is_err());
        e.pending = 1; assert!(e.freeze(end, 1_000).is_err());
        e.pending = 0; e.freeze(end, 1_000).unwrap(); assert_eq!(e.status, CARRIED);
    }
    #[test]
    fn top_ten_and_conservation() {
        for count in 1..=20 {
            let mut e = event();
            for i in 1..=count { e.rank(Pubkey::new_from_array([i; 32]), u128::from(i)); }
            assert_eq!(e.winners, count.min(10));
            assert_eq!(e.top[0].score, u128::from(count));
            for prize in [0, 1, 9, 100, 1_000_000, u64::MAX] {
                assert_eq!(awards(prize, &e.top).iter().sum::<u64>(), prize);
            }
        }
        let mut e = event();
        for i in 1..=10 { e.rank(Pubkey::new_from_array([i;32]), u128::from(11-i)); }
        assert_eq!(awards(102_400, &e.top), [51_200,25_600,12_800,6_400,3_200,1_600,800,400,200,200]);
        let same = e.top[0].wallet; e.rank(same, 99); assert_eq!(e.top.iter().filter(|s| s.wallet==same).count(), 1);
    }
    #[test]
    fn ties_do_not_depend_on_result_delivery_order() {
        let mut a = event(); let mut b = event();
        for i in 1..=15 { a.rank(Pubkey::new_from_array([i;32]), 50); }
        for i in (1..=15).rev() { b.rank(Pubkey::new_from_array([i;32]), 50); }
        assert_eq!(a.top, b.top);
        assert_eq!(awards(1_003, &a.top), [101,101,101,100,100,100,100,100,100,100]);
    }
    #[test]
    fn layouts() {
        let e = event(); assert_eq!(borsh::to_vec(&e).unwrap().len(), EVENT_LEN);
        let s = Series {version:1,bank:e.series,mint:e.series,intake:e.series,current:0,next:1,carry:0,bump:1};
        let entry = Entry {version:2,event:e.series,wallet:e.series,score:0,bets:0,wins:0,pending:0,bump:1,open_receipts:0,refunded:false};
        let receipt = Receipt {version:1,series:e.series,event:e.series,bet:e.series,wallet:e.series,wager:1,accepted_at:1,scored:false,bump:1};
        assert_eq!(borsh::to_vec(&s).unwrap().len(), SERIES_LEN);
        assert_eq!(borsh::to_vec(&entry).unwrap().len(), PAID_ENTRY_LEN);
        assert_eq!(borsh::to_vec(&receipt).unwrap().len(), RECEIPT_LEN);
    }
}
