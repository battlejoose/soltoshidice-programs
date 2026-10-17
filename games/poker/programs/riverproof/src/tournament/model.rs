use anchor_lang::prelude::*;

pub const TABLES: usize = 4;
pub const DRAINED_TABLES: u8 = (1 << TABLES) - 1;
pub const SEATS: usize = 10;
pub const CAPACITY: usize = TABLES * SEATS;
pub const MIN_PRIZE_SDICE: u64 = 1_000_000;
pub const ENTRY_SECONDS: i64 = 15 * 60;
pub const CASH_CLOSE_SECONDS: i64 = 5 * 60;
pub const DURATION_SECONDS: i64 = 3 * 60 * 60;
const BASE_UNITS: u64 = 1_000_000;
pub const UNSEATED: u8 = u8::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyError {
    InvalidSchedule, InvalidFunding, InvalidBuyIn, Arithmetic, WrongPhase,
    EntryClosed, Full, InvalidWallet, AlreadyRegistered, NotRegistered,
    EntryAlreadyUsed, InvalidSeat, Occupied, NotSeated, TablesNotDrained,
    WrongSnapshot, ReplayedHand, ChipsNotConserved, FinishInstead,
}
type PolicyResult<T> = std::result::Result<T, PolicyError>;

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase { Scheduled, Draining, Running, Finishing, Complete, Cancelled }

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum EntryStatus { #[default] Empty, Registered, Seated, Eliminated, Withdrawn }

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, Default)]
pub struct Entrant {
    pub wallet: Pubkey,
    pub status: EntryStatus,
    pub table: u8,
    pub seat: u8,
    /// Whole SDICE backed by custody, excluding the separate sponsored prize.
    pub stack: u64,
    /// Same-hand eliminations share an order; pre-hand stack breaks that tie.
    pub elimination_order: u64,
    pub pre_elimination_stack: u64,
    /// Historical amount returned atomically on withdrawal; not a second debt.
    pub cash_out: u64,
}

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Debug)]
pub struct TournamentState {
    pub creator: Pubkey,
    pub prize_sdice: u64,
    pub buy_in: u64,
    pub starts_at: i64,
    pub cash_closes_at: i64,
    pub entry_closes_at: i64,
    pub ends_at: i64,
    pub phase: Phase,
    pub drained_tables: u8,
    pub registered: u8,
    pub entered: u8,
    pub elimination_order: u64,
    pub rake_paid: u64,
    pub last_hand: [u64; TABLES],
    #[max_len(40)]
    pub entries: Vec<Entrant>,
}

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinishReason { FinalTwo, TimeLimit, InsufficientEntrants }

#[derive(Clone, Copy, Debug)]
pub struct StackResult { pub wallet: Pubkey, pub stack: u64 }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Standing {
    pub wallet: Pubkey,
    pub stack: u64,
    /// Descending order; exact ties use the same prize sharing as World Series.
    pub rank_key: [u64; 3],
}

#[derive(AnchorSerialize, AnchorDeserialize, InitSpace, Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrizeAward {
    pub wallet: Pubkey,
    /// Token-2022 base units, separate from this wallet's real table balance.
    pub amount: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SeatMove {
    pub entry: u8, pub from_table: u8, pub from_seat: u8,
    pub to_table: u8, pub to_seat: u8,
}

impl TournamentState {
    pub fn new(creator: Pubkey, now: i64, starts_at: i64, prize: u64, buy_in: u64) -> PolicyResult<Self> {
        if creator == Pubkey::default() { return Err(PolicyError::InvalidWallet); }
        let earliest = now.checked_add(CASH_CLOSE_SECONDS).ok_or(PolicyError::Arithmetic)?;
        if now < 0 || starts_at < earliest { return Err(PolicyError::InvalidSchedule); }
        if prize < MIN_PRIZE_SDICE || prize.checked_mul(BASE_UNITS).is_none() {
            return Err(PolicyError::InvalidFunding);
        }
        // The existing engine requires at least forty small blinds. Choosing
        // max(1, buy_in / 100) preserves that rule for creator-set buy-ins.
        if buy_in < 40 || buy_in.checked_mul(CAPACITY as u64)
            .and_then(|n| n.checked_mul(BASE_UNITS)).is_none() {
            return Err(PolicyError::InvalidBuyIn);
        }
        let entry_closes_at = starts_at.checked_add(ENTRY_SECONDS).ok_or(PolicyError::Arithmetic)?;
        let ends_at = starts_at.checked_add(DURATION_SECONDS).ok_or(PolicyError::Arithmetic)?;
        Ok(Self {
            creator, prize_sdice: prize, buy_in, starts_at,
            cash_closes_at: starts_at - CASH_CLOSE_SECONDS, entry_closes_at, ends_at,
            phase: Phase::Scheduled, drained_tables: 0, registered: 0, entered: 0,
            elimination_order: 0, rake_paid: 0, last_hand: [0; TABLES],
            entries: vec![Entrant::default(); CAPACITY],
        })
    }

    pub fn cash_hand_may_start(&self, now: i64) -> bool {
        self.phase == Phase::Scheduled && now < self.cash_closes_at
    }

    pub fn cash_hand_may_continue(&self, now: i64) -> bool {
        self.phase == Phase::Scheduled && now < self.starts_at
    }

    pub fn register(&mut self, wallet: Pubkey, now: i64) -> PolicyResult<u8> {
        if !matches!(self.phase, Phase::Scheduled | Phase::Draining | Phase::Running) {
            return Err(PolicyError::WrongPhase);
        }
        if now >= self.entry_closes_at { return Err(PolicyError::EntryClosed); }
        if self.phase == Phase::Running && self.finish_reason(now).is_some() { return Err(PolicyError::FinishInstead); }
        if wallet == Pubkey::default() { return Err(PolicyError::InvalidWallet); }
        if self.index(wallet).is_some() { return Err(PolicyError::AlreadyRegistered); }
        if usize::from(self.registered) >= CAPACITY { return Err(PolicyError::Full); }
        let index = self.registered;
        self.entries[usize::from(index)] = Entrant {
            wallet, status: EntryStatus::Registered, table: UNSEATED, seat: UNSEATED,
            ..Entrant::default()
        };
        self.registered += 1;
        Ok(index)
    }

    pub fn begin(&mut self, now: i64) -> PolicyResult<()> {
        if self.phase != Phase::Scheduled { return Err(PolicyError::WrongPhase); }
        if now < self.starts_at || now >= self.ends_at { return Err(PolicyError::InvalidSchedule); }
        self.phase = Phase::Draining;
        Ok(())
    }

    /// Call only after native accounts prove this cash table has no pot,
    /// occupied seats or outstanding cash-out/refund liabilities.
    pub fn mark_drained(&mut self, table: u8) -> PolicyResult<()> {
        if self.phase != Phase::Draining { return Err(PolicyError::WrongPhase); }
        if usize::from(table) >= TABLES { return Err(PolicyError::InvalidSeat); }
        self.drained_tables |= 1 << table;
        Ok(())
    }

    pub fn activate(&mut self, now: i64) -> PolicyResult<()> {
        if self.phase != Phase::Draining { return Err(PolicyError::WrongPhase); }
        if self.drained_tables != DRAINED_TABLES { return Err(PolicyError::TablesNotDrained); }
        if now >= self.entry_closes_at { return Err(PolicyError::EntryClosed); }
        self.phase = Phase::Running;
        Ok(())
    }

    /// Returns the exact deposit the instruction must transfer from the signing
    /// entrant. A reconnect uses their existing seat; it never calls enter again.
    pub fn enter(&mut self, wallet: Pubkey, table: u8, seat: u8, now: i64) -> PolicyResult<u64> {
        if self.phase != Phase::Running { return Err(PolicyError::WrongPhase); }
        if now < self.starts_at || now >= self.entry_closes_at { return Err(PolicyError::EntryClosed); }
        let index = self.index(wallet).ok_or(PolicyError::NotRegistered)?;
        if self.entries[index].status != EntryStatus::Registered { return Err(PolicyError::EntryAlreadyUsed); }
        if self.finish_reason(now).is_some() { return Err(PolicyError::FinishInstead); }
        self.require_empty_seat(table, seat)?;
        self.entries[index].status = EntryStatus::Seated;
        self.entries[index].table = table;
        self.entries[index].seat = seat;
        self.entries[index].stack = self.buy_in;
        self.entered += 1;
        Ok(self.buy_in)
    }

    /// Atomic registration + paid admission. Unpaid pre-registration is not a
    /// guaranteed seat: it may be replaced only by an actual funded admission.
    /// Previously entered/busted/withdrawn wallets are never erased or reused.
    pub fn admit(&mut self, wallet: Pubkey, table: u8, seat: u8, now: i64) -> PolicyResult<u64> {
        let mut candidate = self.clone();
        if candidate.index(wallet).is_none() {
            if usize::from(candidate.registered) == CAPACITY {
                let index = candidate.entries.iter().position(|e| e.status == EntryStatus::Registered)
                    .ok_or(PolicyError::Full)?;
                if wallet == Pubkey::default() { return Err(PolicyError::InvalidWallet); }
                candidate.entries[index].wallet = wallet;
            } else { candidate.register(wallet, now)?; }
        }
        let amount = candidate.enter(wallet, table, seat, now)?;
        *self = candidate;
        Ok(amount)
    }

    /// Native adapters supply EVERY occupied seat from a completed hand, never
    /// a browser/relay-authored score. Table stacks are checked before all writes.
    pub fn record_hand(&mut self, table: u8, hand: u64, results: &[StackResult], rake: u64) -> PolicyResult<()> {
        let order = self.elimination_order.checked_add(1).ok_or(PolicyError::Arithmetic)?;
        self.record_hand_at(table, hand, results, rake, order)
    }

    /// Order comes from the native completion transaction. Older completed
    /// tables may be reconciled later without promoting their eliminated seats.
    pub fn record_hand_at(&mut self, table: u8, hand: u64, results: &[StackResult], rake: u64, order: u64) -> PolicyResult<()> {
        if order == 0 { return Err(PolicyError::WrongSnapshot); }
        if self.phase != Phase::Running { return Err(PolicyError::WrongPhase); }
        if usize::from(table) >= TABLES { return Err(PolicyError::InvalidSeat); }
        if hand <= self.last_hand[usize::from(table)] { return Err(PolicyError::ReplayedHand); }
        let indexes: Vec<_> = self.entries.iter().enumerate()
            .filter(|(_, e)| e.status == EntryStatus::Seated && e.table == table)
            .map(|(i, _)| i).collect();
        if indexes.is_empty() || results.len() != indexes.len() { return Err(PolicyError::WrongSnapshot); }
        let mut before = 0u64;
        let mut after = 0u64;
        for &i in &indexes {
            let matches: Vec<_> = results.iter().filter(|r| r.wallet == self.entries[i].wallet).collect();
            if matches.len() != 1 { return Err(PolicyError::WrongSnapshot); }
            before = before.checked_add(self.entries[i].stack).ok_or(PolicyError::Arithmetic)?;
            after = after.checked_add(matches[0].stack).ok_or(PolicyError::Arithmetic)?;
        }
        if after.checked_add(rake) != Some(before) || rake > before / 100 {
            return Err(PolicyError::ChipsNotConserved);
        }
        let paid = self.rake_paid.checked_add(rake).ok_or(PolicyError::Arithmetic)?;
        for i in indexes {
            let result = results.iter().find(|r| r.wallet == self.entries[i].wallet).unwrap();
            let entry = &mut self.entries[i];
            if result.stack == 0 {
                entry.status = EntryStatus::Eliminated;
                entry.elimination_order = order;
                entry.pre_elimination_stack = entry.stack;
                entry.table = UNSEATED;
                entry.seat = UNSEATED;
            }
            entry.stack = result.stack;
        }
        self.last_hand[usize::from(table)] = hand;
        self.rake_paid = paid;
        self.elimination_order = self.elimination_order.max(order);
        Ok(())
    }

    /// The existing native hand/idle/kick checks run before this method. The
    /// native transfer returns precisely this balance, never the initial buy-in.
    pub fn withdraw(&mut self, wallet: Pubkey) -> PolicyResult<u64> {
        let order = self.elimination_order.checked_add(1).ok_or(PolicyError::Arithmetic)?;
        self.withdraw_at(wallet, order)
    }

    pub fn withdraw_at(&mut self, wallet: Pubkey, order: u64) -> PolicyResult<u64> {
        if order == 0 { return Err(PolicyError::WrongSnapshot); }
        if self.phase != Phase::Running { return Err(PolicyError::WrongPhase); }
        let i = self.index(wallet).ok_or(PolicyError::NotRegistered)?;
        if self.entries[i].status != EntryStatus::Seated { return Err(PolicyError::NotSeated); }
        let entry = &mut self.entries[i];
        entry.cash_out = entry.stack;
        entry.pre_elimination_stack = entry.stack;
        entry.stack = 0;
        entry.status = EntryStatus::Withdrawn;
        entry.table = UNSEATED;
        entry.seat = UNSEATED;
        entry.elimination_order = order;
        self.elimination_order = self.elimination_order.max(order);
        Ok(entry.cash_out)
    }

    pub fn finish_reason(&self, now: i64) -> Option<FinishReason> {
        if self.phase != Phase::Running { return None; }
        if now >= self.entry_closes_at && self.entered < 3 {
            return Some(FinishReason::InsufficientEntrants);
        }
        // Native play and voluntary exits stop at ends_at. A late keeper must
        // not replace already-achieved elimination standings with a deadline
        // tie among busted players merely because reconciliation was delayed.
        if self.entered >= 3 && self.alive() <= 2 { return Some(FinishReason::FinalTwo); }
        if now >= self.ends_at { return Some(FinishReason::TimeLimit); }
        None
    }

    pub fn alive(&self) -> usize {
        self.entries.iter().filter(|e| e.status == EntryStatus::Seated).count()
    }

    /// Full scoreboard, including every wallet that actually bought in. Merely
    /// registering does not create a prize position. Survivors lead elimination
    /// standings; a deadline ranks all participants by their remaining chips.
    pub fn standings(&self, reason: FinishReason) -> Vec<Standing> {
        let mut rows: Vec<_> = self.entries.iter().filter(|e| matches!(e.status,
            EntryStatus::Seated | EntryStatus::Eliminated | EntryStatus::Withdrawn))
            .map(|e| Standing {
                wallet: e.wallet, stack: e.stack,
                rank_key: if reason == FinishReason::TimeLimit { [e.stack, 0, 0] }
                    else if e.status == EntryStatus::Seated { [1, e.stack, 0] }
                    else { [0, e.elimination_order, e.pre_elimination_stack] },
            }).collect();
        rows.sort_by(|a, b| b.rank_key.cmp(&a.rank_key)
            .then_with(|| a.wallet.to_bytes().cmp(&b.wallet.to_bytes())));
        rows
    }

    /// Native callers first freeze every table at the cutoff and reconcile all
    /// owed chip balances. This computes prizes; it does not transfer tokens.
    pub fn prize_allocations(&self, now: i64) -> PolicyResult<Vec<PrizeAward>> {
        let reason = self.finish_reason(now).ok_or(PolicyError::WrongPhase)?;
        if reason == FinishReason::InsufficientEntrants { return Err(PolicyError::FinishInstead); }
        let rows = self.standings(reason);
        let count = rows.len().min(10);
        let mut remaining = self.prize_sdice.checked_mul(BASE_UNITS).ok_or(PolicyError::Arithmetic)?;
        let mut prizes = Vec::with_capacity(count);
        for (i, row) in rows.iter().take(count).enumerate() {
            let amount = if i + 1 == count { remaining } else { remaining / 2 };
            remaining -= amount;
            prizes.push(PrizeAward { wallet: row.wallet, amount });
        }
        // Identical to World Series: tied occupied positions share their total;
        // ascending wallet bytes assign indivisible dust and the top-ten cutoff.
        let mut first = 0;
        while first < count {
            let mut end = first + 1;
            while end < count && rows[end].rank_key == rows[first].rank_key { end += 1; }
            let total: u64 = prizes[first..end].iter().map(|p| p.amount).sum();
            let tied = (end - first) as u64;
            for (i, prize) in prizes[first..end].iter_mut().enumerate() {
                prize.amount = total / tied + u64::from((i as u64) < total % tied);
            }
            first = end;
        }
        Ok(prizes)
    }

    /// Minimal between-hand moves to balanced tables. Closing tables 2 then 1
    /// keeps the surviving table nearest the normal courtyard camera. Movement
    /// does not create another buy-in and is allowed after entry has closed.
    pub fn rebalance_plan(&self) -> PolicyResult<Vec<SeatMove>> {
        if self.phase != Phase::Running { return Err(PolicyError::WrongPhase); }
        let players = self.alive();
        if players <= 2 { return Err(PolicyError::FinishInstead); }
        let active_tables = players.div_ceil(SEATS);
        let mut counts = [0usize; TABLES];
        let mut occupied = [[false; SEATS]; TABLES];
        for entry in &self.entries {
            if entry.status == EntryStatus::Seated {
                counts[usize::from(entry.table)] += 1;
                occupied[usize::from(entry.table)][usize::from(entry.seat)] = true;
            }
        }
        let mut target = [0usize; TABLES];
        for (i, n) in target.iter_mut().enumerate().take(active_tables) {
            *n = players / active_tables + usize::from(i < players % active_tables);
        }
        let mut plan = Vec::new();
        for (i, entry) in self.entries.iter().enumerate().rev() {
            if entry.status != EntryStatus::Seated { continue; }
            let from = usize::from(entry.table);
            if counts[from] <= target[from] { continue; }
            let to = (0..active_tables).find(|&t| counts[t] < target[t]).ok_or(PolicyError::WrongSnapshot)?;
            let seat = occupied[to].iter().position(|&used| !used).ok_or(PolicyError::Occupied)?;
            plan.push(SeatMove { entry: i as u8, from_table: entry.table, from_seat: entry.seat,
                to_table: to as u8, to_seat: seat as u8 });
            occupied[from][usize::from(entry.seat)] = false;
            occupied[to][seat] = true;
            counts[from] -= 1;
            counts[to] += 1;
        }
        Ok(plan)
    }

    /// Adapters require both native hands to be idle or completed/reconciled,
    /// transfer the exact stack between vaults, and replace session authority.
    pub fn move_seat(&mut self, movement: SeatMove) -> PolicyResult<()> {
        if self.phase != Phase::Running { return Err(PolicyError::WrongPhase); }
        let entry = self.entries.get(usize::from(movement.entry)).ok_or(PolicyError::NotRegistered)?;
        if entry.status != EntryStatus::Seated || entry.table != movement.from_table || entry.seat != movement.from_seat {
            return Err(PolicyError::WrongSnapshot);
        }
        self.require_empty_seat(movement.to_table, movement.to_seat)?;
        let entry = &mut self.entries[usize::from(movement.entry)];
        entry.table = movement.to_table;
        entry.seat = movement.to_seat;
        Ok(())
    }

    fn index(&self, wallet: Pubkey) -> Option<usize> {
        self.entries.iter().position(|e| e.status != EntryStatus::Empty && e.wallet == wallet)
    }
    fn require_empty_seat(&self, table: u8, seat: u8) -> PolicyResult<()> {
        if usize::from(table) >= TABLES || usize::from(seat) >= SEATS { return Err(PolicyError::InvalidSeat); }
        if self.entries.iter().any(|e| e.status == EntryStatus::Seated && e.table == table && e.seat == seat) {
            return Err(PolicyError::Occupied);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn encoded(t: &TournamentState) -> Vec<u8> {
        let mut bytes = Vec::new();
        t.serialize(&mut bytes).unwrap();
        bytes
    }
    fn wallet(i: u8) -> Pubkey { Pubkey::new_from_array([i + 1; 32]) }
    fn running(players: usize) -> TournamentState {
        let mut t = TournamentState::new(wallet(50), 0, 1000, MIN_PRIZE_SDICE, 10_000).unwrap();
        for i in 0..players { t.register(wallet(i as u8), 0).unwrap(); }
        t.begin(1000).unwrap();
        for table in 0..TABLES { t.mark_drained(table as u8).unwrap(); }
        t.activate(1000).unwrap();
        for i in 0..players { t.enter(wallet(i as u8), (i / 10) as u8, (i % 10) as u8, 1000).unwrap(); }
        t
    }
    #[test]
    fn paid_admission_reclaims_only_unused_registrations_and_rolls_back_invalid_seats() {
        let mut t = running(3);
        for i in 3..40 { t.register(wallet(i), 1000).unwrap(); }
        let before = encoded(&t);
        assert_eq!(t.admit(wallet(40), 0, 0, 1000), Err(PolicyError::Occupied));
        assert_eq!(encoded(&t), before);
        t.admit(wallet(40), 0, 3, 1000).unwrap();
        assert_eq!(t.entered, 4);
        assert_eq!(t.registered, 40);
        assert!(t.entries.iter().any(|e| e.wallet == wallet(40) && e.status == EntryStatus::Seated));
        assert_eq!(t.admit(wallet(0), 1, 0, 1000), Err(PolicyError::EntryAlreadyUsed));
        t.withdraw(wallet(0)).unwrap();
        assert_eq!(t.admit(wallet(0), 1, 0, 1000), Err(PolicyError::EntryAlreadyUsed));
        let before = encoded(&t);
        assert_eq!(t.admit(wallet(41), 1, 0, t.entry_closes_at), Err(PolicyError::EntryClosed));
        assert_eq!(encoded(&t), before);
    }
    #[test]
    fn late_score_updates_cannot_promote_an_earlier_elimination() {
        let mut t = running(6);
        for i in 3..6 { t.entries[i].table = 1; t.entries[i].seat = (i - 3) as u8; }
        t.record_hand_at(1, 1, &[StackResult { wallet: wallet(3), stack: 20_000 },
            StackResult { wallet: wallet(4), stack: 10_000 }, StackResult { wallet: wallet(5), stack: 0 }], 0, 20).unwrap();
        t.record_hand_at(0, 1, &[StackResult { wallet: wallet(0), stack: 20_000 },
            StackResult { wallet: wallet(1), stack: 10_000 }, StackResult { wallet: wallet(2), stack: 0 }], 0, 10).unwrap();
        assert_eq!(t.elimination_order, 20);
        t.withdraw_at(wallet(0), 30).unwrap(); t.withdraw_at(wallet(3), 40).unwrap();
        let rows = t.standings(FinishReason::FinalTwo);
        assert_eq!(rows[4].wallet, wallet(5)); assert_eq!(rows[5].wallet, wallet(2));
        assert_eq!(t.entries[2].elimination_order, 10);
        assert_eq!(t.entries[5].elimination_order, 20);
    }
    #[test]
    fn late_reconciliation_preserves_final_two_elimination_standings() {
        let mut t = running(4);
        t.record_hand(0, 1, &[StackResult { wallet: wallet(0), stack: 25_000 },
            StackResult { wallet: wallet(1), stack: 15_000 }, StackResult { wallet: wallet(2), stack: 0 },
            StackResult { wallet: wallet(3), stack: 0 }], 0).unwrap();
        assert_eq!(t.finish_reason(t.ends_at + 100), Some(FinishReason::FinalTwo));
        assert_eq!(t.prize_allocations(t.ends_at + 100).unwrap().iter().map(|p| p.amount).sum::<u64>(),
            MIN_PRIZE_SDICE * BASE_UNITS);
    }
    #[test]
    fn exact_schedule_funding_and_entry_boundaries() {
        assert!(TournamentState::new(wallet(50), 1, 0, MIN_PRIZE_SDICE, 10_000).is_err());
        assert!(TournamentState::new(wallet(50), 0, 1, MIN_PRIZE_SDICE - 1, 10_000).is_err());
        assert!(TournamentState::new(wallet(50), 0, 1, MIN_PRIZE_SDICE, u64::MAX).is_err());
        assert!(TournamentState::new(wallet(50), 0, i64::MAX, MIN_PRIZE_SDICE, 10_000).is_err());
        let mut t = running(3);
        assert_eq!(t.buy_in, 10_000);
        assert_eq!(t.ends_at - t.starts_at, 10_800);
        t.register(wallet(3), 1899).unwrap();
        t.enter(wallet(3), 0, 3, 1899).unwrap();
        assert_eq!(t.register(wallet(4), 1900), Err(PolicyError::EntryClosed));
    }
    #[test]
    fn nobody_enters_until_all_four_cash_tables_are_refunded() {
        let mut t = TournamentState::new(wallet(50), 0, 1000, MIN_PRIZE_SDICE, 25_000).unwrap();
        t.register(wallet(0), 0).unwrap();
        assert!(t.cash_hand_may_start(699));
        assert!(!t.cash_hand_may_start(700));
        assert!(t.cash_hand_may_continue(999));
        assert!(!t.cash_hand_may_continue(1000));
        assert_eq!(t.begin(999), Err(PolicyError::InvalidSchedule));
        t.begin(1000).unwrap();
        t.mark_drained(0).unwrap(); t.mark_drained(1).unwrap();
        assert_eq!(t.activate(1000), Err(PolicyError::TablesNotDrained));
        assert_eq!(t.enter(wallet(0), 0, 0, 1000), Err(PolicyError::WrongPhase));
        t.mark_drained(2).unwrap();
        assert_eq!(t.activate(1000), Err(PolicyError::TablesNotDrained));
        t.mark_drained(3).unwrap(); t.activate(1000).unwrap();
        assert_eq!(t.enter(wallet(0), 0, 0, 1000), Ok(25_000));
    }
    #[test]
    fn forty_unique_wallets_once_each_no_rebuy_after_bust_or_cash_out() {
        let mut t = running(40);
        assert_eq!(t.register(wallet(40), 1001), Err(PolicyError::Full));
        assert_eq!(t.register(wallet(0), 1001), Err(PolicyError::AlreadyRegistered));
        assert_eq!(t.enter(wallet(40), 0, 0, 1001), Err(PolicyError::NotRegistered));
        assert_eq!(t.enter(wallet(0), 1, 0, 1001), Err(PolicyError::EntryAlreadyUsed));
        assert_eq!(t.withdraw(wallet(0)), Ok(10_000));
        assert_eq!(t.withdraw(wallet(0)), Err(PolicyError::NotSeated));
        assert_eq!(t.enter(wallet(0), 0, 0, 1001), Err(PolicyError::EntryAlreadyUsed));
    }
    #[test]
    fn native_result_reconciliation_preserves_tokens_and_records_simultaneous_eliminations() {
        let mut t = running(4);
        let before = encoded(&t);
        let invalid = [StackResult { wallet: wallet(0), stack: 40_001 }];
        assert_eq!(t.record_hand(0, 1, &invalid, 0), Err(PolicyError::WrongSnapshot));
        assert_eq!(encoded(&t), before);
        let results = [0, 0, 19_800, 19_800].map(|stack| StackResult { wallet: Pubkey::default(), stack });
        let results: Vec<_> = results.iter().enumerate().map(|(i, r)| StackResult { wallet: wallet(i as u8), stack: r.stack }).collect();
        assert_eq!(t.record_hand(0, 1, &results, 399), Err(PolicyError::ChipsNotConserved));
        t.record_hand(0, 1, &results, 400).unwrap();
        assert_eq!(t.alive(), 2);
        assert_eq!(t.entries[0].elimination_order, t.entries[1].elimination_order);
        assert_eq!(t.entries.iter().map(|e| e.stack).sum::<u64>() + t.rake_paid, 40_000);
        assert_eq!(t.record_hand(0, 1, &results, 400), Err(PolicyError::ReplayedHand));
        assert_eq!(t.finish_reason(1001), Some(FinishReason::FinalTwo));
        assert_eq!(t.enter(wallet(0), 0, 0, 1001), Err(PolicyError::EntryAlreadyUsed));
    }
    #[test]
    fn every_population_three_to_forty_can_consolidate_without_changing_chips() {
        for players in 3..=40 {
            let mut t = running(40);
            // Leave alternating seats/tables, creating uneven real-world gaps.
            for i in 0..(40 - players) { t.withdraw(wallet(((i * 7) % 40) as u8)).unwrap(); }
            let balances: Vec<_> = t.entries.iter().map(|e| (e.wallet, e.stack, e.cash_out)).collect();
            for movement in t.rebalance_plan().unwrap() { t.move_seat(movement).unwrap(); }
            assert!(t.rebalance_plan().unwrap().is_empty(), "idempotent for {players}");
            let counts: Vec<_> = (0..TABLES).map(|table| t.entries.iter().filter(|e| e.status == EntryStatus::Seated && e.table as usize == table).count()).collect();
            let occupied: Vec<_> = counts.iter().filter(|&&n| n > 0).collect();
            assert!(occupied.iter().all(|&&n| (3..=10).contains(&n)), "{counts:?}");
            assert!(occupied.iter().max().unwrap().abs_diff(**occupied.iter().min().unwrap()) <= 1);
            assert_eq!(t.entries.iter().map(|e| (e.wallet, e.stack, e.cash_out)).collect::<Vec<_>>(), balances);
        }
    }
    #[test]
    fn time_limit_and_too_few_entrants_do_not_require_a_heads_up_hand() {
        let t = running(3);
        assert_eq!(t.finish_reason(t.ends_at - 1), None);
        assert_eq!(t.finish_reason(t.ends_at), Some(FinishReason::TimeLimit));
        let t = running(2);
        assert_eq!(t.finish_reason(t.entry_closes_at - 1), None);
        assert_eq!(t.finish_reason(t.entry_closes_at), Some(FinishReason::InsufficientEntrants));
        assert_eq!(t.rebalance_plan(), Err(PolicyError::FinishInstead));
    }

    #[test]
    fn final_two_rank_by_stack_then_reverse_eliminations_and_pay_only_ten() {
        let mut t = running(30);
        for i in 0..28 { t.withdraw(wallet(i)).unwrap(); }
        t.entries[28].stack = 15_000;
        t.entries[29].stack = 5_000;
        let rows = t.standings(FinishReason::FinalTwo);
        assert_eq!(rows.len(), 30);
        assert_eq!(rows.iter().take(4).map(|r| r.wallet).collect::<Vec<_>>(),
            vec![wallet(28), wallet(29), wallet(27), wallet(26)]);
        let payouts = t.prize_allocations(1001).unwrap();
        assert_eq!(payouts.len(), 10);
        assert_eq!(payouts[0].amount, 500_000 * BASE_UNITS);
        assert_eq!(payouts[1].amount, 250_000 * BASE_UNITS);
        assert_eq!(payouts[2].amount, 125_000 * BASE_UNITS);
        assert_eq!(payouts[8].amount, payouts[9].amount);
        assert_eq!(payouts.iter().map(|p| p.amount).sum::<u64>(), MIN_PRIZE_SDICE * BASE_UNITS);
    }

    #[test]
    fn deadline_standings_ignore_elimination_order_and_ties_share_world_series_prizes() {
        let mut t = running(30);
        t.withdraw(wallet(29)).unwrap();
        t.entries[0].stack = 50_000;
        t.entries[1].stack = 50_000;
        t.entries[2].stack = 20_000;
        t.entries[3].stack = 0;
        let payouts = t.prize_allocations(t.ends_at).unwrap();
        assert_eq!(payouts.len(), 10);
        assert_eq!(payouts[0], PrizeAward { wallet: wallet(0), amount: 375_000 * BASE_UNITS });
        assert_eq!(payouts[1], PrizeAward { wallet: wallet(1), amount: 375_000 * BASE_UNITS });
        assert_eq!(payouts[2].amount, 125_000 * BASE_UNITS);
        assert!(payouts.iter().all(|p| p.wallet != wallet(29) && p.wallet != wallet(3)));
        for prize in [MIN_PRIZE_SDICE, MIN_PRIZE_SDICE + 1, u64::MAX / BASE_UNITS] {
            t.prize_sdice = prize;
            assert_eq!(t.prize_allocations(t.ends_at).unwrap().iter().map(|p| p.amount).sum::<u64>(), prize * BASE_UNITS);
        }
    }

    #[test]
    fn simultaneous_busts_use_pre_hand_stacks_and_share_exact_ties() {
        let mut t = running(5);
        t.entries[0].stack = 5_000;
        t.entries[1].stack = 5_000;
        t.entries[2].stack = 10_000;
        t.entries[3].stack = 20_000;
        let results: Vec<_> = [0, 0, 0, 25_000, 25_000].iter().enumerate()
            .map(|(i, &stack)| StackResult { wallet: wallet(i as u8), stack }).collect();
        t.record_hand(0, 1, &results, 0).unwrap();
        let prizes = t.prize_allocations(1001).unwrap();
        assert_eq!(prizes.iter().map(|p| p.wallet).collect::<Vec<_>>(), vec![wallet(3), wallet(4), wallet(2), wallet(0), wallet(1)]);
        assert_eq!(prizes[0].amount, prizes[1].amount);
        assert_eq!(prizes[3].amount, prizes[4].amount);
        assert_eq!(prizes.iter().map(|p| p.amount).sum::<u64>(), MIN_PRIZE_SDICE * BASE_UNITS);
    }
}
