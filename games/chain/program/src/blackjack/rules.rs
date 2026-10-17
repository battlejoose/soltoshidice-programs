//! Casino rules, integer liabilities and deterministic card draws.
//! Entropy is accepted only by the account layer after verifying a fresh ORAO
//! request. Never seed a complete publicly readable future deck at hand start.
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{hash::hashv, program_error::ProgramError};

pub const SEATS: usize = 10;
pub const HANDS: usize = 4;
pub const CARDS: usize = 21;
pub const DECKS: u8 = 6;
pub const NO_ACTOR: u8 = 255;
pub const TURN_SECONDS: i64 = 60;
pub const BET_SECONDS: i64 = 20;
pub const REQUEST_SECONDS: i64 = 300;
pub const ORACLE_SECONDS: i64 = 180;
pub const MAX_BET: u64 = 10_000_000_000_000; // 10M SDICE, six decimals.

type Result<T> = core::result::Result<T, ProgramError>;
fn require(ok: bool, code: u32) -> Result<()> {
    if ok { Ok(()) } else { Err(ProgramError::Custom(code)) }
}
fn add(a: u64, b: u64) -> Result<u64> { a.checked_add(b).ok_or(ProgramError::ArithmeticOverflow) }
fn sub(a: u64, b: u64) -> Result<u64> { a.checked_sub(b).ok_or(ProgramError::InsufficientFunds) }
fn mul(a: u64, b: u64) -> Result<u64> { a.checked_mul(b).ok_or(ProgramError::ArithmeticOverflow) }
fn deadline(now: i64, seconds: i64) -> Result<i64> { now.checked_add(seconds).ok_or(ProgramError::ArithmeticOverflow) }

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Stage { #[default] Betting, NeedRandomness, WaitingRandomness, PlayerTurn, Complete, Refunded }
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Draw { #[default] Initial, Hit, Double, Split, SplitReplacement, Dealer }
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Status { #[default] Empty, Playing, Stood, Bust, Natural }
#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Outcome { #[default] Pending, Loss, Push, Win, Blackjack, Refund }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Hit, Stand, Double, Split }

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Hand {
    pub cards: [u8; CARDS], pub count: u8, pub wager: u64,
    pub status: Status, pub split: bool, pub split_aces: bool,
    pub outcome: Outcome, pub returned: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Value { pub total: u8, pub soft: bool, pub natural: bool }

/// Card IDs 0..51 are suit-major, A,2,...,10,J,Q,K. Duplicate IDs represent
/// distinct physical cards when a ruleset uses multiple decks.
pub fn rank(card: u8) -> Result<u8> { require(card < 52, 800)?; Ok(card % 13 + 1) }
pub fn value(cards: &[u8], split: bool) -> Result<Value> {
    require(cards.len() <= CARDS, 800)?;
    let mut total = 0u16;
    let mut aces = 0;
    for &card in cards {
        let rank = rank(card)?;
        if rank == 1 { aces += 1; total += 11; } else { total += u16::from(rank.min(10)); }
    }
    while total > 21 && aces > 0 { total -= 10; aces -= 1; }
    Ok(Value { total: u8::try_from(total).map_err(|_| ProgramError::ArithmeticOverflow)?,
        soft: aces > 0, natural: !split && cards.len() == 2 && total == 21 })
}
impl Hand {
    pub fn value(&self) -> Result<Value> { value(self.cards.get(..self.count as usize).ok_or(ProgramError::InvalidAccountData)?, self.split) }
    fn push(&mut self, card: u8) -> Result<()> {
        rank(card)?; require((self.count as usize) < CARDS, 801)?;
        self.cards[self.count as usize] = card; self.count += 1;
        let v = self.value()?;
        self.status = if v.total > 21 { Status::Bust } else if v.natural { Status::Natural }
            else if v.total == 21 || (self.split_aces && self.count == 2) { Status::Stood } else { Status::Playing };
        Ok(())
    }
    pub fn can_split(&self) -> Result<bool> {
        // Equal card values can be split, including differently named ten cards.
        Ok(self.count == 2 && !self.split_aces && self.status == Status::Playing
            && rank(self.cards[0])?.min(10) == rank(self.cards[1])?.min(10))
    }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Player {
    pub credit: u64, pub original: u64, pub count: u8, pub hands: [Hand; HANDS],
    pub returned: u64,
}
impl Player {
    pub fn staked(&self) -> Result<u64> { self.hands.iter().try_fold(0, |n, h| add(n, h.wager)) }
}

#[derive(BorshSerialize, BorshDeserialize, Clone, Debug, PartialEq, Eq)]
pub struct Round {
    pub id: u64, pub revision: u64, pub stage: Stage, pub draw: Draw,
    pub actor: u8, pub hand: u8, pub deadline: i64, pub draw_nonce: u64,
    pub shoe: [u8; 52], pub remaining: u16, pub dealer: Hand,
    #[borsh(deserialize_with = "super::read_fixed")]
    pub players: Box<[Player; SEATS]>, pub transcript: [u8; 32],
}
impl Default for Round {
    fn default() -> Self {
        Self { id: 1, revision: 0, stage: Stage::Betting, draw: Draw::Initial,
            actor: NO_ACTOR, hand: 0, deadline: 0, draw_nonce: 0,
            shoe: [DECKS; 52], remaining: 52 * u16::from(DECKS), dealer: Hand::default(),
            players: Box::new([Player::default(); SEATS]), transcript: [0; 32] }
    }
}

impl Round {
    pub fn active(&self) -> bool { !matches!(self.stage, Stage::Complete | Stage::Refunded) }
    /// Maximum liability in the EXISTING bank vault. Unbet seat credits stay in
    /// the table's player-token escrow, outside bank equity and Bank.reserved.
    pub fn reserved(&self) -> Result<u64> {
        self.players.iter().try_fold(0, |n, p| {
            let liability = if self.active() { add(p.staked()?, mul(p.original, 8)?)? } else { 0 };
            add(n, liability)
        })
    }
    pub fn credits(&self) -> Result<u64> { self.players.iter().try_fold(0, |n, p| add(n, p.credit)) }
    pub fn stakes(&self) -> Result<u64> { self.players.iter().try_fold(0, |n, p| add(n, p.staked()?)) }
    pub fn betting_players(&self) -> usize { self.players.iter().filter(|p| p.original > 0).count() }
    fn changed(&mut self) -> Result<()> { self.revision = add(self.revision, 1)?; Ok(()) }
    pub fn next_round(&mut self) -> Result<()> {
        require(!self.active(), 802)?;
        let id = add(self.id, 1)?; let revision = add(self.revision, 1)?;
        self.id = id; self.revision = revision; self.stage = Stage::Betting;
        self.draw = Draw::Initial; self.actor = NO_ACTOR; self.hand = 0;
        self.deadline = 0; self.draw_nonce = 0; self.shoe = [DECKS; 52];
        self.remaining = 52 * u16::from(DECKS); self.dealer = Hand::default(); self.transcript = [0; 32];
        for p in self.players.iter_mut() { *p = Player { credit: p.credit, ..Player::default() }; }
        Ok(())
    }
    pub fn bet(&mut self, seat: usize, amount: u64, now: i64) -> Result<()> {
        require(self.stage == Stage::Betting && seat < SEATS, 802)?;
        require(self.deadline == 0 || now < self.deadline, 803)?;
        require(amount > 0 && amount <= MAX_BET && amount % 2 == 0, 804)?;
        let p = &mut self.players[seat];
        require(p.original == 0, 805)?; // Accepted bets cannot be selectively removed.
        let credit = sub(p.credit, amount)?;
        let closes = if self.deadline == 0 { deadline(now, BET_SECONDS)? } else { self.deadline };
        *p = Player { credit, original: amount, count: 1,
            hands: [Hand { wager: amount, status: Status::Playing, ..Hand::default() }, Hand::default(), Hand::default(), Hand::default()], returned: 0 };
        self.deadline = closes; self.changed()
    }
    fn need(&mut self, draw: Draw, now: i64) -> Result<()> {
        self.draw_nonce = add(self.draw_nonce, 1)?;
        self.draw = draw; self.stage = Stage::NeedRandomness;
        self.deadline = deadline(now, REQUEST_SECONDS)?; Ok(())
    }
    pub fn start(&mut self, now: i64) -> Result<()> {
        require(self.stage == Stage::Betting && self.betting_players() > 0 && now >= self.deadline, 806)?;
        self.need(Draw::Initial, now)?; self.changed()
    }
    pub fn requested(&mut self, now: i64) -> Result<()> {
        require(self.stage == Stage::NeedRandomness && now <= self.deadline, 807)?;
        self.stage = Stage::WaitingRandomness; self.deadline = deadline(now, ORACLE_SECONDS)?;
        self.changed()
    }
    /// Call only with verified fulfilled randomness for this exact request.
    /// Solana's transaction rollback protects against partial writes on errors.
    pub fn fulfill(&mut self, random: &[u8; 64], domain: &[u8; 32], now: i64) -> Result<()> {
        require(self.stage == Stage::WaitingRandomness, 807)?;
        let mut rng = CardSource { random, domain, counter: 0 };
        match self.draw {
            Draw::Initial => {
                for seat in 0..SEATS { if self.players[seat].original > 0 {
                    let card = rng.next(&mut self.shoe, &mut self.remaining)?;
                    self.players[seat].hands[0].push(card)?;
                }}
                self.dealer.push(rng.next(&mut self.shoe, &mut self.remaining)?)?;
                for seat in 0..SEATS { if self.players[seat].original > 0 {
                    let card = rng.next(&mut self.shoe, &mut self.remaining)?;
                    self.players[seat].hands[0].push(card)?;
                }}
                self.actor = 0; self.hand = 0; self.advance(now)?;
            }
            Draw::Hit | Draw::Double | Draw::Split | Draw::SplitReplacement => {
                require((self.actor as usize) < SEATS && (self.hand as usize) < HANDS, 808)?;
                let card = rng.next(&mut self.shoe, &mut self.remaining)?;
                let hand = &mut self.players[self.actor as usize].hands[self.hand as usize];
                hand.push(card)?;
                if self.draw == Draw::Double && hand.status == Status::Playing { hand.status = Status::Stood; }
                self.advance(now)?;
            }
            Draw::Dealer => {
                // The second dealer card is drawn only after ALL decisions have
                // locked. The remaining dealer draws have no player choices.
                while self.dealer.count < 2 || self.dealer.value()?.total < 17 {
                    self.dealer.push(rng.next(&mut self.shoe, &mut self.remaining)?)?;
                }
                self.settle()?;
            }
        }
        self.transcript = hashv(&[b"sdice:blackjack:transcript:v1", &self.transcript, domain, random]).to_bytes();
        self.changed()
    }
    fn advance(&mut self, now: i64) -> Result<()> {
        while (self.actor as usize) < SEATS {
            let p = &self.players[self.actor as usize];
            while self.hand < p.count {
                let h = &p.hands[self.hand as usize];
                if h.status == Status::Playing {
                    if h.count == 1 { self.need(Draw::SplitReplacement, now)?; }
                    else { self.stage = Stage::PlayerTurn; self.deadline = deadline(now, TURN_SECONDS)?; }
                    return Ok(());
                }
                self.hand += 1;
            }
            self.actor += 1; self.hand = 0;
        }
        self.actor = NO_ACTOR; self.hand = 0; self.need(Draw::Dealer, now)
    }
    pub fn act(&mut self, seat: usize, action: Action, now: i64) -> Result<()> {
        require(self.stage == Stage::PlayerTurn && seat < SEATS && seat == self.actor as usize, 809)?;
        require(now <= self.deadline, 810)?;
        self.perform(action, now)?; self.changed()
    }
    fn perform(&mut self, action: Action, now: i64) -> Result<()> {
        let p = &mut self.players[self.actor as usize];
        let index = self.hand as usize;
        let hand = p.hands[index];
        require(hand.status == Status::Playing && hand.count >= 2, 811)?;
        match action {
            Action::Stand => { p.hands[index].status = Status::Stood; self.advance(now)?; }
            Action::Hit => { require(!hand.split_aces, 811)?; self.need(Draw::Hit, now)?; }
            Action::Double => {
                require(hand.count == 2 && !hand.split_aces, 812)?;
                let credit = sub(p.credit, hand.wager)?;
                p.hands[index].wager = mul(hand.wager, 2)?; p.credit = credit;
                self.need(Draw::Double, now)?;
            }
            Action::Split => {
                require((p.count as usize) < HANDS && hand.can_split()?, 813)?;
                let credit = sub(p.credit, hand.wager)?;
                let ace = rank(hand.cards[0])? == 1;
                for i in ((index + 1)..p.count as usize).rev() { p.hands[i + 1] = p.hands[i]; }
                let mut left = Hand { wager: hand.wager, split: true, split_aces: ace, ..Hand::default() };
                let mut right = left;
                left.push(hand.cards[0])?; right.push(hand.cards[1])?;
                p.hands[index] = left; p.hands[index + 1] = right;
                p.credit = credit; p.count += 1;
                self.need(Draw::Split, now)?;
            }
        }
        Ok(())
    }
    pub fn timeout_turn(&mut self, now: i64) -> Result<()> {
        require(self.stage == Stage::PlayerTurn && now > self.deadline, 810)?;
        // Do not kick, cash out or erase the player when their session expires.
        self.perform(Action::Stand, now)?; self.changed()
    }
    fn settle(&mut self) -> Result<()> {
        let dealer = self.dealer.value()?;
        for p in self.players.iter_mut() {
            if p.original == 0 { continue; }
            let natural = p.count == 1 && p.hands[0].value()?.natural;
            if dealer.natural && !natural {
                // Original-bets-only protection: extra split/double stakes are
                // returned, including on split hands that already busted.
                let mut loss = p.original;
                for h in p.hands.iter_mut().take(p.count as usize) {
                    let taken = loss.min(h.wager); loss -= taken;
                    h.returned = sub(h.wager, taken)?;
                    h.outcome = if taken > 0 { Outcome::Loss } else { Outcome::Refund };
                }
                require(loss == 0, 814)?;
            } else {
                for h in p.hands.iter_mut().take(p.count as usize) {
                    let v = h.value()?;
                    let (returned, outcome) = if v.total > 21 { (0, Outcome::Loss) }
                        else if dealer.natural { (h.wager, Outcome::Push) }
                        else if v.natural { (add(h.wager, mul(h.wager, 3)? / 2)?, Outcome::Blackjack) }
                        else if dealer.total > 21 || v.total > dealer.total { (mul(h.wager, 2)?, Outcome::Win) }
                        else if v.total == dealer.total { (h.wager, Outcome::Push) }
                        else { (0, Outcome::Loss) };
                    h.returned = returned; h.outcome = outcome;
                }
            }
            p.returned = p.hands.iter().try_fold(0, |n, h| add(n, h.returned))?;
            p.credit = add(p.credit, p.returned)?;
        }
        self.stage = Stage::Complete; self.deadline = 0; Ok(())
    }
    /// The account layer must first prove the pending oracle is UNFULFILLED.
    /// A fulfilled result is always consumed, even if its clock is overdue.
    pub fn refund_unavailable(&mut self, now: i64) -> Result<()> {
        require(matches!(self.stage, Stage::NeedRandomness | Stage::WaitingRandomness) && now > self.deadline, 815)?;
        for p in self.players.iter_mut() {
            p.returned = p.staked()?;
            p.credit = add(p.credit, p.returned)?;
            for h in p.hands.iter_mut().take(p.count as usize) { h.returned = h.wager; h.outcome = Outcome::Refund; }
        }
        self.stage = Stage::Refunded; self.deadline = 0; self.changed()
    }
}

struct CardSource<'a> { random: &'a [u8; 64], domain: &'a [u8; 32], counter: u32 }
impl CardSource<'_> {
    fn next(&mut self, shoe: &mut [u8; 52], remaining: &mut u16) -> Result<u8> {
        require(*remaining > 0 && shoe.iter().map(|n| u16::from(*n)).sum::<u16>() == *remaining, 816)?;
        let n = u64::from(*remaining);
        let cutoff = (1u64 << 32) / n * n;
        for _ in 0..32 {
            let bytes = hashv(&[b"sdice:blackjack:draw:v1", self.random, self.domain, &self.counter.to_le_bytes()]).to_bytes();
            self.counter = self.counter.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            let number = u64::from(u32::from_le_bytes(bytes[..4].try_into().unwrap()));
            if number >= cutoff { continue; }
            let mut index = number % n;
            for (card, count) in shoe.iter_mut().enumerate() {
                if index < u64::from(*count) { *count -= 1; *remaining -= 1; return Ok(card as u8); }
                index -= u64::from(*count);
            }
        }
        Err(ProgramError::Custom(817)) // No modulo-biased fallback.
    }
}

#[cfg(test)]
mod tests;
