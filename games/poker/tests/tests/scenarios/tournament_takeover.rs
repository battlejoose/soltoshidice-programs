use super::*;
use riverproof::tournament::accounts::{arena_address, PokerArena, PokerTournament};
use riverproof::tournament::model::Phase;

struct ArenaFixture { h: Harness, operator: Keypair, sponsor: Keypair, tables: [Pubkey; 4], arena: Pubkey }

impl ArenaFixture {
    fn finalize_ix(&self, tournament: Pubkey) -> Instruction {
        let [a, b, c, d] = self.tables;
        Instruction { program_id: program_id(), accounts: riverproof::accounts::FinalizePokerTournament {
            cranker: self.operator.pubkey(), arena: self.arena, tournament, table_a: a, table_b: b, table_c: c, table_d: d,
            state_a: table_state_pda(&a), state_b: table_state_pda(&b), state_c: table_state_pda(&c), state_d: table_state_pda(&d),
            hand_a: hand_pda(&a), hand_b: hand_pda(&b), hand_c: hand_pda(&c), hand_d: hand_pda(&d),
            cards_a: hand_cards_pda(&a), cards_b: hand_cards_pda(&b), cards_c: hand_cards_pda(&c), cards_d: hand_cards_pda(&d),
            pot_a: pda(&[b"poker-pot", a.as_ref()]), pot_b: pda(&[b"poker-pot", b.as_ref()]), pot_c: pda(&[b"poker-pot", c.as_ref()]), pot_d: pda(&[b"poker-pot", d.as_ref()]),
        }.to_account_metas(None), data: riverproof::instruction::FinalizePokerTournament {}.data() }
    }
    fn terminal_refund_ix(&self, tournament: Pubkey, entry_index: u8) -> Instruction {
        let t: PokerTournament = self.h.account(&tournament);
        let e = t.state.entries[entry_index as usize]; let config = self.tables[e.table as usize];
        Instruction { program_id: program_id(), accounts: riverproof::accounts::RefundPokerTournamentBalance {
            cranker: self.operator.pubkey(), arena: self.arena, tournament, config,
            table_state: table_state_pda(&config), hand_state: hand_pda(&config), vault: vault_pda(&config), owner: e.wallet,
            owner_tokens: get_associated_token_address(&e.wallet, &self.h.mint), mint: self.h.mint,
            token_program: anchor_spl::token_2022::ID, associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }.to_account_metas(None), data: riverproof::instruction::RefundPokerTournamentBalance { entry_index }.data() }
    }
    fn prize_ix(&self, tournament: Pubkey, award_index: u8) -> Instruction {
        let t: PokerTournament = self.h.account(&tournament); let wallet = t.awards[award_index as usize].wallet;
        Instruction { program_id: program_id(), accounts: riverproof::accounts::ClaimPokerTournamentPrize {
            cranker: self.operator.pubkey(), arena: self.arena, tournament,
            prize_vault: pda(&[b"poker-prize", tournament.as_ref()]), recipient: wallet,
            recipient_tokens: get_associated_token_address(&wallet, &self.h.mint), mint: self.h.mint,
            token_program: anchor_spl::token_2022::ID, associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }.to_account_metas(None), data: riverproof::instruction::ClaimPokerTournamentPrize { award_index }.data() }
    }
    fn restore_ix(&self, tournament: Pubkey) -> Instruction {
        let [a, b, c, d] = self.tables;
        Instruction { program_id: program_id(), accounts: riverproof::accounts::ActivatePokerTournament {
            cranker: self.operator.pubkey(), arena: self.arena, tournament, table_a: a, table_b: b, table_c: c, table_d: d,
            state_a: table_state_pda(&a), state_b: table_state_pda(&b), state_c: table_state_pda(&c), state_d: table_state_pda(&d),
        }.to_account_metas(None), data: riverproof::instruction::RestorePokerCashTables {}.data() }
    }
    fn exit_ix(&self, tournament: Pubkey, actor: Pubkey, table: usize, seat_index: u8, owner: Pubkey) -> Instruction {
        let config = self.tables[table];
        Instruction { program_id: program_id(), accounts: riverproof::accounts::ExitPokerTournament {
            actor, arena: self.arena, tournament, config, table_state: table_state_pda(&config), hand_state: hand_pda(&config),
            settlement: pda(&[b"poker-pot", config.as_ref()]), vault: vault_pda(&config), owner,
            owner_tokens: get_associated_token_address(&owner, &self.h.mint), mint: self.h.mint,
            token_program: anchor_spl::token_2022::ID, associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }.to_account_metas(None), data: riverproof::instruction::ExitPokerTournament { seat_index }.data() }
    }
    fn move_ix(&self, event: Pubkey) -> Instruction {
        let t: PokerTournament = self.h.account(&event);
        let m = t.state.rebalance_plan().unwrap()[0];
        let source = self.tables[m.from_table as usize]; let destination = self.tables[m.to_table as usize];
        let owner = t.state.entries[m.entry as usize].wallet;
        Instruction { program_id: program_id(), accounts: riverproof::accounts::MovePokerTournamentSeat {
            cranker: self.operator.pubkey(), arena: self.arena, tournament: event, source, destination,
            source_state: table_state_pda(&source), destination_state: table_state_pda(&destination),
            source_hand: hand_pda(&source), destination_hand: hand_pda(&destination), source_vault: vault_pda(&source),
            destination_vault: vault_pda(&destination), destination_session: session_pda(&owner, &destination),
            source_settlement: pda(&[b"poker-pot", source.as_ref()]), destination_settlement: pda(&[b"poker-pot", destination.as_ref()]),
            mint: self.h.mint, token_program: anchor_spl::token_2022::ID,
        }.to_account_metas(None), data: riverproof::instruction::MovePokerTournamentSeat { entry_index: m.entry }.data() }
    }
    fn activate(&mut self, tournament: Pubkey) -> Result<(), String> {
        self.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::ActivatePokerTournament {
            cranker: self.operator.pubkey(), arena: self.arena, tournament,
            table_a: self.tables[0], table_b: self.tables[1], table_c: self.tables[2], table_d: self.tables[3],
            state_a: table_state_pda(&self.tables[0]), state_b: table_state_pda(&self.tables[1]), state_c: table_state_pda(&self.tables[2]), state_d: table_state_pda(&self.tables[3]),
        }.to_account_metas(None), data: riverproof::instruction::ActivatePokerTournament {}.data() }, &[&self.operator])
    }
    fn enter(&mut self, tournament: Pubkey, player: &Keypair, table_index: usize, seat_index: u8) -> Result<(), String> {
        let config = self.tables[table_index];
        self.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::EnterPokerTournament {
            player: player.pubkey(), arena: self.arena, tournament, config,
            table_state: table_state_pda(&config), hand_state: hand_pda(&config), vault: vault_pda(&config),
            player_tokens: get_associated_token_address(&player.pubkey(), &self.h.mint),
            mint: self.h.mint, token_program: anchor_spl::token_2022::ID,
        }.to_account_metas(None), data: riverproof::instruction::EnterPokerTournament { seat_index }.data() }, &[player])
    }
    fn register(&mut self, tournament: Pubkey, player: &Keypair) -> Result<(), String> {
        self.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::RegisterPokerTournament {
            player: player.pubkey(), arena: self.arena, tournament,
        }.to_account_metas(None), data: riverproof::instruction::RegisterPokerTournament {}.data() }, &[player])
    }
    fn new() -> Self { Self::build(false) }
    fn build(legacy: bool) -> Self {
        let mut h = Harness::new();
        if legacy { h.svm.add_program_from_file(program_id(), std::env::var("LEGACY_POKER_PROGRAM_SO").unwrap()).unwrap(); }
        let operator = h.player("operator");
        let sponsor = h.player("sponsor");
        let mut tables = [Pubkey::default(); 4];
        for i in 0..4 {
            tables[i] = if legacy && i == 3 {
                create_table_on_with_rules(&mut h, &operator, 10, Pubkey::default(), 100_000,
                    "http://127.0.0.1:9003", 1_000, 100_000).unwrap()
            } else { create_table_on(&mut h, &operator, 10, Pubkey::default(), 100 + i as u64,
                &format!("http://127.0.0.1:{}", 9000 + i)).unwrap() };
        }
        let arena = arena_address(&operator.pubkey());
        // Bind to real bank executable with a canonical bank account. Full
        // cross-program transfers are exercised in the poker fee VM suite.
        let bank_program = Pubkey::new_unique();
        h.svm.add_program_from_file(bank_program, std::env::var("BANK_PROGRAM_SO").expect("bank build required")).unwrap();
        let (bank, bank_bump) = Pubkey::find_program_address(&[b"bank", operator.pubkey().as_ref(), h.mint.as_ref()], &bank_program);
        let mut bank_data = vec![0; 114];
        bank_data[0] = 4;
        bank_data[1..33].copy_from_slice(operator.pubkey().as_ref());
        bank_data[33..65].copy_from_slice(h.mint.as_ref());
        bank_data[113] = bank_bump;
        h.svm.set_account(bank, solana_sdk::account::Account {
            lamports: 10_000_000, data: bank_data, owner: bank_program, executable: false, rent_epoch: 0,
        }).unwrap();
        let accounts = riverproof::accounts::BindPokerArena {
            operator: operator.pubkey(), arena, table_a: tables[0], table_b: tables[1], table_c: tables[2], table_d: tables[3],
            state_a: table_state_pda(&tables[0]), state_b: table_state_pda(&tables[1]), state_c: table_state_pda(&tables[2]), state_d: table_state_pda(&tables[3]),
            pot_a: pda(&[b"poker-pot", tables[0].as_ref()]), pot_b: pda(&[b"poker-pot", tables[1].as_ref()]),
            pot_c: pda(&[b"poker-pot", tables[2].as_ref()]), pot_d: pda(&[b"poker-pot", tables[3].as_ref()]), bank_program, bank,
            system_program: anchor_lang::system_program::ID,
        };
        let mut bind_metas = accounts.to_account_metas(None);
        if legacy { bind_metas.truncate(bind_metas.len() - 3); }
        let ix = Instruction { program_id: program_id(), accounts: bind_metas,
            data: riverproof::instruction::BindPokerArena {}.data() };
        let mut wrong = ix.clone();
        wrong.accounts[0].pubkey = sponsor.pubkey();
        assert!(h.send(wrong, &[&sponsor]).is_err());
        let mut duplicate = ix.clone();
        duplicate.accounts[3].pubkey = tables[0];
        duplicate.accounts[6].pubkey = table_state_pda(&tables[0]);
        assert!(h.send(duplicate, &[&operator]).is_err());
        h.send(ix, &[&operator]).unwrap();
        h.arena = Some(arena);
        // Genesis token funding only, for a bounded offline fixture. The tested
        // schedule still performs the real Token-2022 debit and escrow credit.
        let ata = get_associated_token_address(&sponsor.pubkey(), &h.mint);
        let mut tokens = h.svm.get_account(&ata).unwrap();
        tokens.data[64..72].copy_from_slice(&(3_000_000u64 * riverproof::sdice::UNITS).to_le_bytes());
        h.svm.set_account(ata, tokens).unwrap();
        let mut mint = h.svm.get_account(&h.mint).unwrap();
        let supply = u64::from_le_bytes(mint.data[36..44].try_into().unwrap());
        mint.data[36..44].copy_from_slice(&(supply + (3_000_000 - FAUCET_AMOUNT) * riverproof::sdice::UNITS).to_le_bytes());
        h.svm.set_account(h.mint, mint).unwrap();
        Self { h, operator, sponsor, tables, arena }
    }
    fn schedule(&mut self, start: i64, prize: u64) -> Result<Pubkey, String> {
        self.schedule_buy_in(start, prize, 10_000)
    }
    fn schedule_buy_in(&mut self, start: i64, prize: u64, buy_in: u64) -> Result<Pubkey, String> {
        let registry: PokerArena = self.h.account(&self.arena);
        let tournament = pda(&[b"poker-tournament", self.arena.as_ref(), &registry.next_id.to_le_bytes()]);
        let accounts = riverproof::accounts::SchedulePokerTournament {
            sponsor: self.sponsor.pubkey(), arena: self.arena, tournament,
            prize_vault: pda(&[b"poker-prize", tournament.as_ref()]),
            sponsor_tokens: get_associated_token_address(&self.sponsor.pubkey(), &self.h.mint),
            mint: self.h.mint, token_program: anchor_spl::token_2022::ID,
            system_program: anchor_lang::system_program::ID,
        };
        self.h.send(Instruction { program_id: program_id(), accounts: accounts.to_account_metas(None),
            data: riverproof::instruction::SchedulePokerTournament { starts_at: start, prize_sdice: prize, buy_in }.data(),
        }, &[&self.sponsor]).map(|_| tournament)
    }
    fn prepare(&mut self, tournament: Pubkey, table: usize) -> Result<(), String> {
        let config = self.tables[table];
        let accounts = riverproof::accounts::PreparePokerTable {
            cranker: self.operator.pubkey(), arena: self.arena, tournament, config,
            table_state: table_state_pda(&config), hand_state: hand_pda(&config), hand_cards: hand_cards_pda(&config),
        };
        self.h.send(Instruction { program_id: program_id(), accounts: accounts.to_account_metas(None),
            data: riverproof::instruction::PreparePokerTable {}.data() }, &[&self.operator])
    }
    fn refund_ix(&self, tournament: Pubkey, table: usize, seat_index: u8, owner: Pubkey) -> Instruction {
        let config = self.tables[table];
        let accounts = riverproof::accounts::RefundPokerSeat {
            cranker: self.operator.pubkey(), arena: self.arena, tournament, config,
            table_state: table_state_pda(&config), hand_state: hand_pda(&config), vault: vault_pda(&config),
            evicted: owner, evicted_tokens: get_associated_token_address(&owner, &self.h.mint),
            mint: self.h.mint, token_program: anchor_spl::token_2022::ID,
            associated_token_program: anchor_spl::associated_token::ID, system_program: anchor_lang::system_program::ID,
        };
        Instruction { program_id: program_id(), accounts: accounts.to_account_metas(None),
            data: riverproof::instruction::RefundPokerSeat { seat_index }.data() }
    }
}

#[test]
fn tournament_blinds_snapshot_each_hand_at_shared_ten_minute_boundaries() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..12).map(|n| f.h.player(&format!("blinds_{n}"))).collect();
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    for (i, player) in players.iter().enumerate() {
        let config = f.tables[i / 3];
        f.enter(event, player, i / 3, (i % 3) as u8).unwrap();
        set_ready(&mut f.h, player, &config, true);
    }
    let base = BUY_IN / 100;
    assert_eq!(base, SMALL_BLIND, "full-hand oracle uses the original blinds");
    f.h.set_clock(start + 599);
    let mut meter = Meter::default();
    let signing = Signing { keys: &players[..3], sessions: None };
    let (session, deal, slots) = deal_to_hole_cards(&mut f.h, &signing, &f.tables[0], &mut meter);
    let first: HandState = f.h.account(&hand_pda(&f.tables[0]));
    assert_eq!(first.min_raise, base * 2);
    assert_eq!(first.contributions.iter().sum::<u64>(), base * 3);

    f.h.set_clock(start + 600);
    start_hand_mental(&mut f.h, &f.operator, &f.tables[1]).unwrap();
    f.h.set_clock(start + 601);
    start_hand_mental(&mut f.h, &f.operator, &f.tables[2]).unwrap();
    start_hand_mental(&mut f.h, &f.operator, &f.tables[3]).unwrap();
    for table in &f.tables[1..] {
        let config: TableConfig = f.h.account(table);
        let hand: HandState = f.h.account(&hand_pda(table));
        assert_eq!((config.small_blind, config.big_blind), (base * 2, base * 4));
        assert_eq!(hand.min_raise, base * 4);
        assert_eq!(hand.contributions.iter().sum::<u64>(), base * 6);
        assert_eq!(config.buy_in, BUY_IN);
    }
    // Retrying Deal cannot replace a live hand or change its frozen blinds.
    assert!(start_hand_mental(&mut f.h, &f.operator, &f.tables[0]).is_err());
    assert_eq!(f.h.account::<TableConfig>(&f.tables[0]).small_blind, base);
    // Real shuffles, locks, hole shares, all streets and showdown straddle the
    // clock boundary. The old hand still matches the original-blind oracle.
    play_to_showdown(&mut f.h, &signing, &f.tables[0], &session, &deal, &slots, &mut meter);
    assert_eq!(f.h.account::<TableConfig>(&f.tables[0]).big_blind, base * 2);
    assert_eq!(f.h.account::<HandState>(&hand_pda(&f.tables[0])).stage, HandStage::Complete);
    for table in f.tables {
        let state: TableState = f.h.account(&table_state_pda(&table));
        let hand: HandState = f.h.account(&hand_pda(&table));
        riverproof::instructions::table::assert_chips_conserved(&state, &hand).unwrap();
        assert_eq!(f.h.vault_balance(&table), BUY_IN * 3);
    }
}

#[test]
fn tournament_delayed_activation_and_first_deal_do_not_restart_the_blind_clock() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    f.h.set_clock(start + 700);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    for table in f.tables {
        let config: TableConfig = f.h.account(&table);
        assert_eq!((config.small_blind, config.big_blind), (SMALL_BLIND * 2, SMALL_BLIND * 4));
    }
    let players: Vec<_> = (0..3).map(|n| f.h.player(&format!("late_blinds_{n}"))).collect();
    for (i, player) in players.iter().enumerate() {
        f.enter(event, player, 0, i as u8).unwrap();
        set_ready(&mut f.h, player, &f.tables[0], true);
    }
    // Skip an entire level. Derive from buy-in, never add to an outdated table
    // config or compound its already-increased small blind.
    f.h.set_clock(start + 1800);
    start_hand_mental(&mut f.h, &f.operator, &f.tables[0]).unwrap();
    let config: TableConfig = f.h.account(&f.tables[0]);
    assert_eq!((config.small_blind, config.big_blind), (SMALL_BLIND * 4, SMALL_BLIND * 8));
    assert_eq!(config.buy_in, BUY_IN);
    // Finishing an event restores all cash rules even after elevated blinds.
    let saved: PokerArena = f.h.account(&f.arena);
    f.h.set_clock(start + 10800);
    let finalize = f.finalize_ix(event);
    f.h.send(finalize, &[&f.operator]).unwrap();
    for i in 0..3 {
        let refund = f.terminal_refund_ix(event, i);
        f.h.send(refund, &[&f.operator]).unwrap();
    }
    let restore = f.restore_ix(event);
    f.h.send(restore, &[&f.operator]).unwrap();
    for (index, address) in f.tables.iter().enumerate() {
        let cash: TableConfig = f.h.account(address);
        let rules = saved.cash_rules[index];
        assert_eq!((cash.buy_in, cash.small_blind, cash.big_blind, cash.ante),
            (rules.buy_in, rules.small_blind, rules.big_blind, rules.ante));
    }
}

#[test]
fn tournament_native_completion_order_precedes_reverse_reconciliation() {
    let mut f = ArenaFixture::new(); let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..7).map(|n| f.h.player(&format!("sequence_{n}"))).collect();
    f.h.set_clock(start); for t in 0..4 { f.prepare(event, t).unwrap(); } f.activate(event).unwrap();
    for (i, player) in players.iter().enumerate() {
        f.enter(event, player, i / 3, (i % 3) as u8).unwrap();
        let config = f.tables[i / 3]; set_ready(&mut f.h, player, &config, true);
    }
    f.h.advance(5);
    let mut meter = Meter::default();
    let mut rounds = Vec::new();
    // Both tables begin before either has a pending result. Existing hands
    // continue normally while another table awaits its fee/score crank.
    for i in 0..2 {
        let signing = Signing { keys: &players[i * 3..i * 3 + 3], sessions: None };
        let config = f.tables[i];
        rounds.push(deal_to_hole_cards(&mut f.h, &signing, &config, &mut meter));
    }
    for (i, (session, deal, slots)) in rounds.into_iter().enumerate() {
        let signing = Signing { keys: &players[i * 3..i * 3 + 3], sessions: None };
        let config = f.tables[i];
        play_to_showdown(&mut f.h, &signing, &config, &session, &deal, &slots, &mut meter);
        let arena: PokerArena = f.h.account(&f.arena);
        assert_eq!(arena.completion_order[i], i as u64 + 1);
        assert_eq!(arena.completed_hand[i], 1);
    }
    let waiting_table = f.tables[2];
    assert!(start_hand_mental(&mut f.h, &f.operator, &waiting_table).unwrap_err().contains("TournamentResultPending"));
    let exit = f.exit_ix(event, players[6].pubkey(), 2, 0, players[6].pubkey());
    assert!(f.h.send(exit, &[&players[6]]).unwrap_err().contains("TournamentResultPending"));
    // Deliberately process the later hand first. Neither score crank may
    // assign a fresh elimination sequence or change the native result markers.
    for i in [1, 0] {
        let config = f.tables[i];
        f.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::RecordPokerTournamentHand {
            cranker: f.operator.pubkey(), arena: f.arena, tournament: event, config,
            table_state: table_state_pda(&config), hand_state: hand_pda(&config), settlement: pda(&[b"poker-pot", config.as_ref()]),
        }.to_account_metas(None), data: riverproof::instruction::RecordPokerTournamentHand {}.data() }, &[&f.operator]).unwrap();
    }
    let arena: PokerArena = f.h.account(&f.arena);
    assert_eq!(arena.completion_order, [1, 2, 0, 0]); assert_eq!(arena.decision_counter, 2);
    assert_eq!(f.h.account::<PokerTournament>(&event).state.elimination_order, 2);
}

#[test]
fn tournament_deadline_refunds_forty_stalled_players_pays_top_ten_and_restores_cash() {
    use riverproof::tournament::model::FinishReason;
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, 2000).unwrap();
    let players: Vec<_> = (0..40).map(|n| f.h.player(&format!("deadline_{n}"))).collect();
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    for (i, player) in players.iter().enumerate() {
        let table = f.tables[i / 10];
        f.enter(event, player, i / 10, (i % 10) as u8).unwrap(); set_ready(&mut f.h, player, &table, true);
    }
    f.h.advance(5);
    for table in f.tables { start_hand_mental(&mut f.h, &f.operator, &table).unwrap(); }
    let frozen = f.finalize_ix(event);
    let before = f.h.svm.get_account(&event).unwrap().data;
    f.h.set_clock(start + 10800 - 1);
    assert!(f.h.send(frozen.clone(), &[&f.operator]).unwrap_err().contains("HandInProgress"));
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before);
    f.h.set_clock(start + 10800);
    let mut wrong_cards = frozen.clone(); wrong_cards.accounts[13].pubkey = hand_cards_pda(&f.tables[0]);
    assert!(f.h.send(wrong_cards, &[&f.operator]).is_err());
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before, "a bad second table rolls back the first table's void/refund bookkeeping");
    let mut wrong_pot = frozen.clone(); wrong_pot.accounts[17].pubkey = pda(&[b"poker-pot", f.tables[0].as_ref()]);
    assert!(f.h.send(wrong_pot, &[&f.operator]).is_err());
    let mut wrong_fourth_hand = frozen.clone(); wrong_fourth_hand.accounts[20].pubkey = hand_pda(&f.tables[0]);
    assert!(f.h.send(wrong_fourth_hand, &[&f.operator]).is_err());
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before, "bad fourth-table identity rolls back all earlier freezes");
    assert_eq!(f.h.account::<HandState>(&hand_pda(&f.tables[3])).stage, HandStage::Preflop);
    assert_eq!(f.h.account::<HandState>(&hand_pda(&f.tables[0])).stage, HandStage::Preflop);
    let (cu, bytes) = f.h.send_metered(frozen.clone(), &[&f.operator]).unwrap();
    assert!(cu < 300_000 && bytes <= 1232, "terminal budget: {cu} CU, {bytes} bytes");
    println!("FOUR_TABLE_BUDGET deadline_finalize players=40 tables=4 cu={cu} bytes={bytes}");
    assert!(f.h.send(frozen, &[&f.operator]).is_err());
    let t: PokerTournament = f.h.account(&event);
    assert_eq!(t.finish_reason, Some(FinishReason::TimeLimit));
    assert_eq!(t.state.phase, Phase::Finishing);
    assert_eq!((t.state.entered, t.state.rake_paid), (40, 0));
    assert_eq!(t.state.standings(FinishReason::TimeLimit).len(), 40);
    assert_eq!(t.awards.len(), 10);
    assert_eq!(t.awards.iter().map(|a| a.amount).sum::<u64>(), 1_000_000 * riverproof::sdice::UNITS);
    // All stacks tie: the ten wallet-byte winners split the occupied ladder.
    let mut sorted: Vec<_> = players.iter().map(|p| p.pubkey()).collect(); sorted.sort_by_key(|p| p.to_bytes());
    for (i, award) in t.awards.iter().enumerate() {
        assert_eq!(award.wallet, sorted[i]); assert_eq!(award.amount, 100_000 * riverproof::sdice::UNITS);
    }
    let restore = f.restore_ix(event);
    assert!(f.h.send(restore.clone(), &[&f.operator]).is_err());
    for entry in 0..40 {
        let ix = f.terminal_refund_ix(event, entry); f.h.send(ix.clone(), &[&f.operator]).unwrap();
        assert!(f.h.send(ix, &[&f.operator]).is_err());
        assert_eq!(f.h.token_balance(&players[entry as usize].pubkey()), FAUCET_AMOUNT);
    }
    assert_eq!(f.h.account::<PokerTournament>(&event).refunded, (1u64 << 40) - 1);
    for table in f.tables { assert_eq!(f.h.vault_balance(&table), 0); }
    let (cu, bytes) = f.h.send_metered(restore, &[&f.operator]).unwrap();
    assert!(cu < 300_000 && bytes <= 1232, "four-table restoration budget: {cu} / {bytes}");
    println!("FOUR_TABLE_BUDGET restore players=40 tables=4 cu={cu} bytes={bytes}");
    assert_eq!(f.h.account::<PokerArena>(&f.arena).active, Pubkey::default());
    for table in f.tables {
        let config: TableConfig = f.h.account(&table);
        assert_eq!((config.buy_in, config.small_blind, config.action_time_ms), (BUY_IN, SMALL_BLIND, 40000));
        assert_eq!(f.h.account::<TableState>(&table_state_pda(&table)).hand_counter, 1);
    }
    // Restore cash admission before all prize claims have arrived.
    let cash_table = f.tables[0];
    join_with(&mut f.h, &players[29], &cash_table, 0, None).unwrap();
    f.schedule_buy_in(f.h.now + 600, 1_000_000, 3000).unwrap();
    for i in 0..10 {
        let award = t.awards[i as usize]; let before = f.h.token_balance(&award.wallet);
        let ix = f.prize_ix(event, i); f.h.send(ix.clone(), &[&f.operator]).unwrap();
        assert_eq!(f.h.token_balance(&award.wallet) - before, 100_000);
        assert!(f.h.send(ix, &[&f.operator]).is_err());
    }
    let vault: anchor_spl::token_interface::TokenAccount = f.h.account(&pda(&[b"poker-prize", event.as_ref()]));
    assert_eq!(vault.amount, 0);
}

#[test]
fn tournament_with_too_few_entrants_returns_the_sponsor_and_both_buy_ins() {
    use riverproof::tournament::model::FinishReason;
    let mut f = ArenaFixture::new(); let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, 2000).unwrap();
    let players = [f.h.player("cancel_one"), f.h.player("cancel_two")];
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    for (i, p) in players.iter().enumerate() { f.enter(event, p, 0, i as u8).unwrap(); }
    f.h.set_clock(start + 899);
    let ix = f.finalize_ix(event); assert!(f.h.send(ix.clone(), &[&f.operator]).is_err());
    f.h.set_clock(start + 900); f.h.send(ix, &[&f.operator]).unwrap();
    let t: PokerTournament = f.h.account(&event);
    assert_eq!(t.finish_reason, Some(FinishReason::InsufficientEntrants));
    assert_eq!(t.awards.len(), 1); assert_eq!(t.awards[0].wallet, f.sponsor.pubkey());
    assert_eq!(t.awards[0].amount, 1_000_000 * riverproof::sdice::UNITS);
    for i in 0..2 {
        let ix = f.terminal_refund_ix(event, i); f.h.send(ix, &[&f.operator]).unwrap();
        assert_eq!(f.h.token_balance(&players[i as usize].pubkey()), FAUCET_AMOUNT);
    }
    let before = f.h.token_balance(&f.sponsor.pubkey()); let claim = f.prize_ix(event, 0);
    f.h.send(claim, &[&f.operator]).unwrap();
    assert_eq!(f.h.token_balance(&f.sponsor.pubkey()) - before, 1_000_000);
    let restore = f.restore_ix(event); f.h.send(restore, &[&f.operator]).unwrap();
    assert_eq!(f.h.account::<PokerTournament>(&event).state.phase, Phase::Cancelled);
}

#[test]
fn missed_tournament_activation_cannot_erase_old_cash_liabilities() {
    let mut f = ArenaFixture::new(); let player = f.h.player("missed_cash"); let table = f.tables[0];
    join_with(&mut f.h, &player, &table, 0, None).unwrap();
    let start = f.h.now + 600; let event = f.schedule_buy_in(start, 1_000_000, 2000).unwrap();
    f.h.set_clock(start + 1000);
    let finalize = f.finalize_ix(event); assert!(f.h.send(finalize.clone(), &[&f.operator]).is_err());
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    assert!(f.h.send(finalize.clone(), &[&f.operator]).is_err());
    let refund = f.refund_ix(event, 0, 0, player.pubkey()); f.h.send(refund, &[&f.operator]).unwrap();
    assert_eq!(f.h.token_balance(&player.pubkey()), FAUCET_AMOUNT);
    assert!(f.activate(event).is_err(), "recovery cannot reopen the expired entry window");
    f.h.send(finalize, &[&f.operator]).unwrap();
    let claim = f.prize_ix(event, 0); f.h.send(claim, &[&f.operator]).unwrap();
    let restore = f.restore_ix(event); f.h.send(restore, &[&f.operator]).unwrap();
    assert_eq!(f.h.account::<PokerTournament>(&event).state.phase, Phase::Cancelled);
    assert_eq!(f.h.vault_balance(&table), 0);
}

#[test]
fn tournament_exits_preserve_idle_grace_exact_refunds_and_one_entry_history() {
    use riverproof::tournament::model::EntryStatus;
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..5).map(|n| f.h.player(&format!("exit_{n}"))).collect();
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    for (seat, p) in players.iter().enumerate() { f.enter(event, p, 0, seat as u8).unwrap(); }
    let initial = f.h.token_balance(&players[0].pubkey());
    let kick = f.exit_ix(event, f.operator.pubkey(), 0, 0, players[0].pubkey());
    assert!(f.h.send(kick.clone(), &[&f.operator]).unwrap_err().contains("TimerNotExpired"));
    f.h.set_clock(start + 120);
    assert!(f.h.send(kick.clone(), &[&f.operator]).unwrap_err().contains("TimerNotExpired"));
    f.h.set_clock(start + 299);
    assert!(f.h.send(kick.clone(), &[&f.operator]).unwrap_err().contains("TimerNotExpired"));
    let ready_table = f.tables[0];
    set_ready(&mut f.h, &players[1], &ready_table, true);
    f.h.set_clock(start + 300);
    let ready_kick = f.exit_ix(event, f.operator.pubkey(), 0, 1, players[1].pubkey());
    assert!(f.h.send(ready_kick, &[&f.operator]).unwrap_err().contains("SeatOccupied"));
    let redirect = f.exit_ix(event, f.operator.pubkey(), 0, 0, f.operator.pubkey());
    assert!(f.h.send(redirect, &[&f.operator]).is_err());
    let out_of_range = f.exit_ix(event, f.operator.pubkey(), 0, 10, players[0].pubkey());
    assert!(f.h.send(out_of_range, &[&f.operator]).is_err());
    let (cu, bytes) = f.h.send_metered(kick.clone(), &[&f.operator]).unwrap();
    assert!(cu < 300_000 && bytes <= 1232, "exit budget: {cu} CU, {bytes} bytes");
    assert_eq!(f.h.token_balance(&players[0].pubkey()), initial + BUY_IN);
    assert_eq!(f.h.vault_balance(&f.tables[0]), 4 * BUY_IN);
    assert!(f.h.send(kick, &[&f.operator]).is_err());
    assert!(f.enter(event, &players[0], 1, 0).is_err(), "withdrawal cannot be a rebuy");
    // A ready owner can cash out voluntarily, without a keeper's idle grace.
    let leave = f.exit_ix(event, players[1].pubkey(), 0, 1, players[1].pubkey());
    f.h.send(leave, &[&players[1]]).unwrap();
    let t: PokerTournament = f.h.account(&event);
    assert_eq!(t.state.entered, 5); assert_eq!(t.state.alive(), 3);
    assert_eq!(t.state.entries[0].status, EntryStatus::Withdrawn);
    assert_eq!((t.state.entries[0].cash_out, t.state.entries[0].stack), (BUY_IN, 0));
    assert_eq!(t.state.entries[1].elimination_order, t.state.entries[0].elimination_order + 1);
    assert_eq!(t.state.standings(riverproof::tournament::model::FinishReason::FinalTwo).len(), 5);
    // Last-two ranks freeze; a survivor cannot withdraw after the finish/deadline.
    let leave = f.exit_ix(event, players[2].pubkey(), 0, 2, players[2].pubkey());
    f.h.send(leave, &[&players[2]]).unwrap();
    assert!(!f.h.account::<PokerArena>(&f.arena).running);
    let late = f.exit_ix(event, players[3].pubkey(), 0, 3, players[3].pubkey());
    assert!(f.h.send(late, &[&players[3]]).unwrap_err().contains("TournamentTableReserved"));
    assert_eq!(f.h.vault_balance(&f.tables[0]), 2 * BUY_IN);
}

#[test]
fn tournament_mid_hand_leave_waits_for_real_settlement_and_reconciliation() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..5).map(|n| f.h.player(&format!("depart_{n}"))).collect();
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    let config = f.tables[0];
    for (seat, p) in players.iter().take(3).enumerate() {
        f.enter(event, p, 0, seat as u8).unwrap(); set_ready(&mut f.h, p, &config, true);
    }
    f.enter(event, &players[3], 1, 0).unwrap(); f.enter(event, &players[4], 2, 0).unwrap();
    let signing = Signing { keys: &players[..3], sessions: None };
    f.h.advance(5);
    let mut meter = Meter::default();
    let (session, deal, slots) = deal_to_hole_cards(&mut f.h, &signing, &config, &mut meter);
    let before_tokens = f.h.token_balance(&players[0].pubkey());
    let leave = f.exit_ix(event, players[0].pubkey(), 0, 0, players[0].pubkey());
    f.h.send(leave, &[&players[0]]).unwrap();
    let state: TableState = f.h.account(&table_state_pda(&config));
    assert!(state.seats[0].leave_after_hand && !state.seats[0].ready);
    assert_eq!(f.h.token_balance(&players[0].pubkey()), before_tokens);
    assert_eq!(f.h.account::<PokerTournament>(&event).state.alive(), 5);
    let kick = f.exit_ix(event, f.operator.pubkey(), 0, 0, players[0].pubkey());
    assert!(f.h.send(kick.clone(), &[&f.operator]).unwrap_err().contains("HandInProgress"));
    play_to_showdown(&mut f.h, &signing, &config, &session, &deal, &slots, &mut meter);
    let stack = f.h.account::<TableState>(&table_state_pda(&config)).seats[0].stack;
    assert!(f.h.send(kick.clone(), &[&f.operator]).unwrap_err().contains("TournamentResultPending"));
    f.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::RecordPokerTournamentHand {
        cranker: f.operator.pubkey(), arena: f.arena, tournament: event, config,
        table_state: table_state_pda(&config), hand_state: hand_pda(&config), settlement: pda(&[b"poker-pot", config.as_ref()]),
    }.to_account_metas(None), data: riverproof::instruction::RecordPokerTournamentHand {}.data() }, &[&f.operator]).unwrap();
    // No extra five-minute wait for a departure already signed during the hand.
    f.h.send(kick, &[&f.operator]).unwrap();
    assert_eq!(f.h.token_balance(&players[0].pubkey()), before_tokens + stack);
    assert_eq!(f.h.vault_balance(&config), 3 * BUY_IN - stack);
    assert!(!f.h.account::<TableState>(&table_state_pda(&config)).seats[0].occupied);
    assert_eq!(f.h.account::<PokerTournament>(&event).state.entries[0].cash_out, stack);
}

#[test]
fn tournament_consolidation_moves_exact_stacks_without_reentry_or_reusing_old_sessions() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..7).map(|n| f.h.player(&format!("move_{n}"))).collect();
    f.h.set_clock(start);
    for t in 0..4 { f.prepare(event, t).unwrap(); }
    f.activate(event).unwrap();
    let seats = [(0, 0), (0, 1), (1, 0), (1, 1), (1, 2), (2, 0), (2, 1)];
    for (p, &(table, seat)) in players.iter().zip(&seats) { f.enter(event, p, table, seat).unwrap(); }
    let from = f.tables[2]; let to = f.tables[0];
    let middle = f.tables[1];
    for player in &players[2..5] { set_ready(&mut f.h, player, &middle, true); }
    assert!(f.h.account::<TableState>(&table_state_pda(&middle)).next_hand_at > 0);
    set_ready(&mut f.h, &players[6], &from, true);
    let old_key = Keypair::new();
    let old_session = authorize_session(&mut f.h, &players[6], &to, &old_key, start + 5000);
    let balances: Vec<_> = players.iter().map(|p| f.h.token_balance(&p.pubkey())).collect();
    f.h.set_clock(start + 901); // Entry closed; consolidation remains available.
    let ix = f.move_ix(event);
    let before = f.h.svm.get_account(&event).unwrap().data;
    let mut wrong_session = ix.clone(); wrong_session.accounts[11].pubkey = f.operator.pubkey();
    assert!(f.h.send(wrong_session, &[&f.operator]).is_err());
    let mut wrong_vault = ix.clone(); wrong_vault.accounts[10].pubkey = vault_pda(&f.tables[1]);
    assert!(f.h.send(wrong_vault, &[&f.operator]).is_err());
    let mut wrong_entry = ix.clone(); wrong_entry.data = riverproof::instruction::MovePokerTournamentSeat { entry_index: 5 }.data();
    assert!(f.h.send(wrong_entry, &[&f.operator]).is_err());
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before);
    let (cu, bytes) = f.h.send_metered(ix.clone(), &[&f.operator]).unwrap();
    assert!(cu < 300_000 && bytes <= 1232, "move budget: {cu} CU, {bytes} bytes");
    assert!(f.h.send(ix, &[&f.operator]).is_err(), "stale moves cannot revoke a freshly authorized key");
    let expired: riverproof::state::SessionKey = f.h.account(&old_session);
    assert_eq!(expired.expiry, 0);
    assert!(!expired.is_valid_for(&old_key.pubkey(), &players[6].pubkey(), &to, f.h.now));
    let state: TableState = f.h.account(&table_state_pda(&to));
    assert_eq!(state.seats[2].owner, players[6].pubkey());
    assert!(!state.seats[2].ready);
    assert_eq!(state.seats[2].sitting_out_since, (start + 901) * 1000);
    let mut count = 1;
    while !f.h.account::<PokerTournament>(&event).state.rebalance_plan().unwrap().is_empty() {
        let next = f.move_ix(event); f.h.send(next, &[&f.operator]).unwrap(); count += 1;
        assert!(count <= 5);
    }
    assert_eq!(count, 5);
    assert_eq!(f.h.vault_balance(&f.tables[0]), 7 * BUY_IN);
    assert_eq!(f.h.vault_balance(&f.tables[1]) + f.h.vault_balance(&f.tables[2]), 0);
    assert_eq!(f.h.account::<TableState>(&table_state_pda(&middle)).next_hand_at, 0,
        "moving away ready players must not leave an impossible deal countdown");
    let t: PokerTournament = f.h.account(&event);
    assert_eq!((t.state.entered, t.state.elimination_order, t.state.rake_paid), (7, 0, 0));
    assert_eq!(t.state.standings(riverproof::tournament::model::FinishReason::TimeLimit).len(), 7);
    for (i, p) in players.iter().enumerate() {
        assert_eq!(f.h.token_balance(&p.pubkey()), balances[i], "move cannot charge or pay the wallet");
        assert_eq!(t.state.entries[i].stack, BUY_IN);
    }
    let state: TableState = f.h.account(&table_state_pda(&to));
    let idle = state.seats.iter().find(|s| s.owner == players[5].pubkey()).unwrap();
    assert_eq!(idle.sitting_out_since, start * 1000, "consolidation cannot reset an already-idle grace period");
    assert!(f.enter(event, &players[6], 1, 0).is_err());
    let new_key = Keypair::new();
    let session = authorize_session(&mut f.h, &players[6], &to, &new_key, start + 6000);
    assert!(f.h.account::<riverproof::state::SessionKey>(&session).is_valid_for(&new_key.pubkey(), &players[6].pubkey(), &to, f.h.now));
}

#[test]
fn tournament_moves_cannot_cross_live_hands_or_the_deadline() {
    for live_source in [false, true] {
        let mut f = ArenaFixture::new();
        let start = f.h.now + 600;
        let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
        let players: Vec<_> = (0..5).map(|n| f.h.player(&format!("active_move_{n}"))).collect();
        f.h.set_clock(start);
        for t in 0..4 { f.prepare(event, t).unwrap(); }
        f.activate(event).unwrap();
        let split = if live_source { 2 } else { 3 };
        for (i, player) in players.iter().enumerate() {
            let (table, seat) = if i < split { (0, i) } else { (1, i - split) };
            f.enter(event, player, table, seat as u8).unwrap();
            let config = f.tables[table]; set_ready(&mut f.h, player, &config, true);
        }
        let ix = f.move_ix(event);
        let live = f.tables[usize::from(live_source)];
        f.h.advance(5);
        start_hand_mental(&mut f.h, &f.operator, &live).unwrap();
        assert!(f.h.send(ix, &[&f.operator]).unwrap_err().contains("HandInProgress"));
        assert_eq!(f.h.vault_balance(&f.tables[0]) + f.h.vault_balance(&f.tables[1]), 5 * BUY_IN);
        // An owner at the other idle table cannot change final ranks by
        // withdrawing at the deadline, even while reconciliation is pending.
        let outside = if live_source { 0 } else { 3 };
        let outside_table = if live_source { 0 } else { 1 };
        let owner_exit = f.exit_ix(event, players[outside].pubkey(), outside_table, 0, players[outside].pubkey());
        f.h.set_clock(start + 10800);
        assert!(f.h.send(owner_exit, &[&players[outside]]).unwrap_err().contains("TournamentTableReserved"));
        let late_move = f.move_ix(event);
        assert!(f.h.send(late_move, &[&f.operator]).unwrap_err().contains("TournamentTableReserved"));
        assert_eq!(f.h.account::<PokerTournament>(&event).state.alive(), 5);
    }
}

#[test]
fn paid_admission_requires_draining_and_cannot_be_blocked_by_unpaid_registrations() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule(start, 1_000_000).unwrap();
    let players: Vec<_> = (0..41).map(|n| f.h.player(&format!("admit_{n}"))).collect();
    for player in players.iter().take(40) { f.register(event, player).unwrap(); }
    assert!(f.enter(event, &players[40], 0, 0).is_err());
    f.h.set_clock(start);
    f.prepare(event, 0).unwrap(); f.prepare(event, 1).unwrap();
    assert!(f.activate(event).is_err());
    f.prepare(event, 2).unwrap(); assert!(f.activate(event).is_err());
    f.prepare(event, 3).unwrap(); f.activate(event).unwrap();
    assert!(f.activate(event).is_err());
    let registry: PokerArena = f.h.account(&f.arena);
    assert!(registry.running);
    assert_eq!(registry.cash_rules[0].buy_in, BUY_IN);
    for table in &f.tables {
        let config: TableConfig = f.h.account(table);
        assert_eq!((config.buy_in, config.small_blind, config.big_blind, config.action_time_ms), (10_000, 100, 200, 40_000));
    }
    // Failed token funding must not replace an unpaid registration or claim a seat.
    let ata = get_associated_token_address(&players[40].pubkey(), &f.h.mint);
    let original = f.h.svm.get_account(&ata).unwrap();
    let mut poor = original.clone(); poor.data[64..72].copy_from_slice(&0u64.to_le_bytes());
    f.h.svm.set_account(ata, poor).unwrap();
    let before = f.h.svm.get_account(&event).unwrap().data;
    assert!(f.enter(event, &players[40], 0, 0).is_err());
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before);
    assert_eq!(f.h.vault_balance(&f.tables[0]), 0);
    f.h.svm.set_account(ata, original).unwrap();
    f.enter(event, &players[40], 0, 0).unwrap();
    assert_eq!(f.h.token_balance(&players[40].pubkey()), 0);
    assert!(f.enter(event, &players[40], 1, 0).is_err());
    // An unpaid registration has no guaranteed seat, but a funded entrant can
    // never be evicted/replaced by these admissions, even after forty signups.
    for (i, player) in players.iter().take(39).enumerate() {
        let slot = i + 1;
        f.enter(event, player, slot / 10, (slot % 10) as u8).unwrap();
    }
    let t: PokerTournament = f.h.account(&event);
    assert_eq!(t.state.entered, 40);
    assert_eq!(t.state.entries.iter().filter(|e| e.status == riverproof::tournament::model::EntryStatus::Seated).count(), 40);
    for table in &f.tables { assert_eq!(f.h.vault_balance(table), 100_000); }
    assert!(f.enter(event, &players[39], 0, 0).is_err());
    assert!(join_with(&mut f.h, &players[39], &f.tables[0], 0, None).unwrap_err().contains("TournamentTableReserved"));
    // Full forty-row history stays intact while nine players cash out and
    // seven necessary transfers balance the remaining 31 players to 8/8/8/7.
    for slot in 1..10 {
        let owner = &players[slot - 1];
        let leave = f.exit_ix(event, owner.pubkey(), 0, slot as u8, owner.pubkey());
        f.h.send(leave, &[owner]).unwrap();
        assert_eq!(f.h.token_balance(&owner.pubkey()), 10_000);
    }
    let mut moves = 0;
    while !f.h.account::<PokerTournament>(&event).state.rebalance_plan().unwrap().is_empty() {
        let ix = f.move_ix(event);
        let (cu, bytes) = f.h.send_metered(ix, &[&f.operator]).unwrap();
        assert!(cu < 300_000 && bytes <= 1232, "full history move: {cu} CU, {bytes} bytes");
        moves += 1; assert!(moves <= 7);
    }
    assert_eq!(moves, 7);
    for (table, expected) in f.tables.iter().zip([8, 8, 8, 7]) {
        assert_eq!(f.h.vault_balance(table), expected as u64 * 10_000);
        assert_eq!(f.h.account::<TableState>(&table_state_pda(table)).seats.iter().filter(|s| s.occupied).count(), expected);
    }
    let t: PokerTournament = f.h.account(&event);
    assert_eq!((t.state.entered, t.state.alive()), (40, 31));
    assert_eq!(t.state.entries.iter().map(|e| e.cash_out).sum::<u64>(), 90_000);
}

#[test]
fn tournament_admission_deadline_is_fixed_and_private_play_records_before_reset() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule_buy_in(start, 1_000_000, BUY_IN).unwrap();
    let players: Vec<_> = (0..5).map(|n| f.h.player(&format!("play_{n}"))).collect();
    f.h.set_clock(start);
    for table in 0..4 { f.prepare(event, table).unwrap(); }
    f.activate(event).unwrap();
    let config = f.tables[0];
    for (seat, player) in players.iter().take(3).enumerate() {
        f.enter(event, player, 0, seat as u8).unwrap();
        set_ready(&mut f.h, player, &config, true);
    }
    let signing = Signing { keys: &players[..3], sessions: None };
    f.h.advance(5);
    let mut meter = Meter::default();
    let (session, deal, slots) = deal_to_hole_cards(&mut f.h, &signing, &config, &mut meter);
    play_to_showdown(&mut f.h, &signing, &config, &session, &deal, &slots, &mut meter);
    f.h.advance(10);
    let reset = Instruction { program_id: program_id(), accounts: play_accounts(&f.operator.pubkey(), None, &config).to_account_metas(None),
        data: riverproof::instruction::StartNextHand {}.data() };
    assert!(f.h.send(reset.clone(), &[&f.operator]).unwrap_err().contains("TournamentResultPending"));
    let record = Instruction { program_id: program_id(), accounts: riverproof::accounts::RecordPokerTournamentHand {
        cranker: f.operator.pubkey(), arena: f.arena, tournament: event, config,
        table_state: table_state_pda(&config), hand_state: hand_pda(&config), settlement: pda(&[b"poker-pot", config.as_ref()]),
    }.to_account_metas(None), data: riverproof::instruction::RecordPokerTournamentHand {}.data() };
    f.h.send(record.clone(), &[&f.operator]).unwrap();
    assert!(f.h.send(record, &[&f.operator]).is_err());
    let t: PokerTournament = f.h.account(&event);
    let native: TableState = f.h.account(&table_state_pda(&config));
    for entry in t.state.entries.iter().filter(|e| e.status == riverproof::tournament::model::EntryStatus::Seated) {
        assert_eq!(entry.stack, native.seats[entry.seat as usize].stack);
    }
    f.h.send(reset, &[&f.operator]).unwrap();
    f.h.set_clock(start + 899);
    f.enter(event, &players[3], 1, 0).unwrap();
    f.h.set_clock(start + 900);
    let before = f.h.token_balance(&players[4].pubkey());
    assert!(f.enter(event, &players[4], 1, 1).is_err());
    assert_eq!(f.h.token_balance(&players[4].pubkey()), before);
    f.h.set_clock(start + 10800);
    assert!(f.h.send(Instruction { program_id: program_id(), accounts: play_accounts(&f.operator.pubkey(), None, &config).to_account_metas(None),
        data: riverproof::instruction::StartNextHand {}.data() }, &[&f.operator]).unwrap_err().contains("TournamentTableReserved"));
}

#[test]
fn funded_tournament_registration_requires_each_wallet_and_enforces_capacity_and_the_exact_cutoff() {
    let mut f = ArenaFixture::new();
    let start = f.h.now + 600;
    let event = f.schedule(start, 1_000_000).unwrap();
    let players: Vec<_> = (0..41).map(|n| f.h.player(&format!("entrant_{n}"))).collect();
    // A caller cannot fill someone else's registration row by omitting their signature.
    let mut forged = riverproof::accounts::RegisterPokerTournament {
        player: players[0].pubkey(), arena: f.arena, tournament: event,
    }.to_account_metas(None);
    forged[0].is_signer = false;
    assert!(f.h.send(Instruction { program_id: program_id(), accounts: forged,
        data: riverproof::instruction::RegisterPokerTournament {}.data() }, &[]).is_err());
    assert_eq!(f.h.account::<PokerTournament>(&event).state.registered, 0);
    for player in players.iter().take(40) {
        let balance = f.h.token_balance(&player.pubkey());
        f.register(event, player).unwrap();
        assert_eq!(f.h.token_balance(&player.pubkey()), balance, "registration must not debit the buy-in");
    }
    assert!(f.register(event, &players[0]).unwrap_err().contains("TournamentAlreadyRegistered"));
    assert!(f.register(event, &players[40]).unwrap_err().contains("TournamentFull"));
    let tournament: PokerTournament = f.h.account(&event);
    assert_eq!(tournament.state.registered, 40);
    assert_eq!(tournament.state.entered, 0, "registration is not a paid entry or scoreboard rank");
    assert_eq!(tournament.state.entries.iter().filter(|e| e.status == riverproof::tournament::model::EntryStatus::Registered).count(), 40);

    let mut late = ArenaFixture::new();
    let start = late.h.now + 600;
    let event = late.schedule(start, 1_000_000).unwrap();
    let first = late.h.player("on_time");
    let second = late.h.player("too_late");
    late.h.set_clock(start + 899);
    late.register(event, &first).unwrap();
    late.h.set_clock(start + 900);
    assert!(late.register(event, &second).unwrap_err().contains("TournamentEntryClosed"));
    assert_eq!(late.h.account::<PokerTournament>(&event).state.registered, 1);
}

#[test]
fn funded_schedule_cannot_be_replaced_or_bypassed_by_omitting_the_registry() {
    let mut f = ArenaFixture::new();
    let now = f.h.now;
    let before = f.h.token_balance(&f.sponsor.pubkey());
    assert!(f.schedule(now + 299, 1_000_000).is_err());
    assert!(f.schedule(now + 600, 999_999).is_err());
    assert_eq!(f.h.token_balance(&f.sponsor.pubkey()), before);
    let event = f.schedule(now + 600, 1_000_000).unwrap();
    assert!(f.schedule(now + 900, 1_000_000).unwrap_err().contains("TournamentAlreadyScheduled"));
    assert_eq!(f.h.token_balance(&f.sponsor.pubkey()), before - 1_000_000);
    let t: PokerTournament = f.h.account(&event);
    assert_eq!((t.state.cash_closes_at, t.state.entry_closes_at, t.state.ends_at), (now + 300, now + 1500, now + 11400));
    f.h.arena = None;
    let refused = join_with(&mut f.h, &f.operator, &f.tables[0], 0, None).unwrap_err();
    assert!(refused.contains("TournamentAccountRequired"), "{refused}");
    f.h.arena = Some(f.arena);
    join(&mut f.h, &f.operator, &f.tables[0], 0);
    f.h.set_clock(now + 300);
    assert!(join_with(&mut f.h, &f.sponsor, &f.tables[1], 0, None).unwrap_err().contains("TournamentTableReserved"));
}

#[test]
fn scheduled_takeover_voids_a_stalled_deal_and_refunds_all_forty_seats_without_their_signatures() {
    let mut f = ArenaFixture::new();
    let mut players = Vec::new();
    for n in 0..40 {
        let wallet = f.h.player(&format!("entrant_{n}"));
        join(&mut f.h, &wallet, &f.tables[n / 10], (n % 10) as u8);
        set_ready(&mut f.h, &wallet, &f.tables[n / 10], true);
        players.push(wallet);
    }
    let start = f.h.now + 600;
    let event = f.schedule(start, 1_000_000).unwrap();
    f.h.set_clock(start - 301);
    for table in 0..4 {
        start_hand_mental(&mut f.h, &players[table * 10], &f.tables[table]).unwrap();
    }
    f.h.set_clock(start - 300);
    let refused = start_hand_mental(&mut f.h, &players[0], &f.tables[0]).unwrap_err();
    assert!(refused.contains("TournamentTableReserved"), "{refused}");
    // Already-started dealing may continue until the exact scheduled boundary.
    commit_round(&mut f.h, &players[0], &f.tables[0], DealRound::Shuffle, [1; 32]).unwrap();
    f.h.set_clock(start - 1);
    assert!(f.prepare(event, 0).unwrap_err().contains("TournamentNotDue"));
    f.h.set_clock(start);
    assert!(commit_round(&mut f.h, &players[1], &f.tables[0], DealRound::Shuffle, [2; 32])
        .unwrap_err().contains("TournamentTableReserved"));
    let before_prize = f.h.svm.get_account(&pda(&[b"poker-prize", event.as_ref()])).unwrap().data;
    for table in 0..4 {
        f.prepare(event, table).unwrap();
        let hand: HandState = f.h.account(&hand_pda(&f.tables[table]));
        assert!(hand.settled);
        assert_eq!(hand.winner_count, 0, "voided contributions are not winnings");
        let redirected = f.refund_ix(event, table, 0, players[table * 10 + 1].pubkey());
        assert!(f.h.send(redirected, &[&f.operator]).is_err());
        let invalid = f.refund_ix(event, table, 255, players[table * 10].pubkey());
        assert!(f.h.send(invalid, &[&f.operator]).is_err());
        for seat in 0..10 {
            let owner = players[table * 10 + seat].pubkey();
            let ix = f.refund_ix(event, table, seat as u8, owner);
            let (cu, bytes) = f.h.send_metered(ix.clone(), &[&f.operator]).unwrap();
            assert!(bytes <= 1232 && cu <= u64::from(MAX_COMPUTE_UNITS), "refund does not fit: {bytes} / {cu}");
            assert_eq!(f.h.token_balance(&owner), FAUCET_AMOUNT);
            assert!(f.h.send(ix, &[&f.operator]).is_err(), "replayed refund paid twice");
        }
        assert_eq!(f.h.vault_balance(&f.tables[table]), 0);
        let state: TableState = f.h.account(&table_state_pda(&f.tables[table]));
        assert_eq!(state.escrowed, 0);
        assert!(state.seats.iter().all(|s| !s.occupied));
    }
    let registry: PokerArena = f.h.account(&f.arena);
    let tournament: PokerTournament = f.h.account(&event);
    assert_eq!(registry.drained, 0b1111);
    assert_eq!(tournament.state.drained_tables, 0b1111);
    assert_eq!(tournament.state.phase, Phase::Draining);
    assert_eq!(tournament.state.starts_at, start);
    assert_eq!(tournament.state.entry_closes_at, start + 900);
    assert_eq!(before_prize, f.h.svm.get_account(&pda(&[b"poker-prize", event.as_ref()])).unwrap().data);
}

#[test]
fn takeover_refunds_a_live_raise_but_preserves_an_already_completed_payout() {
    for completed in [false, true] {
        let mut f = ArenaFixture::new();
        let players: Vec<_> = (0..3).map(|n| f.h.player(&format!("player_{n}"))).collect();
        let config = f.tables[0];
        for (seat, player) in players.iter().enumerate() {
            join(&mut f.h, player, &config, seat as u8);
            set_ready(&mut f.h, player, &config, true);
        }
        let start = f.h.now + 600;
        let event = f.schedule(start, 1_000_000).unwrap();
        f.h.set_clock(start - 301);
        let signing = Signing { keys: &players, sessions: None };
        let mut meter = Meter::default();
        let (session, deal, slots) = deal_to_hole_cards(&mut f.h, &signing, &config, &mut meter);
        if completed {
            play_to_showdown(&mut f.h, &signing, &config, &session, &deal, &slots, &mut meter);
        } else {
            let hand: HandState = f.h.account(&hand_pda(&config));
            act(&mut f.h, &players[hand.actor_seat as usize], None, &config,
                riverproof::instructions::PokerAction::Raise { to: 200 }).unwrap();
        }
        let before: TableState = f.h.account(&table_state_pda(&config));
        let hand_before: HandState = f.h.account(&hand_pda(&config));
        if completed {
            assert_eq!(hand_before.stage, HandStage::Complete);
            assert!(before.seats.iter().take(3).any(|s| s.stack != BUY_IN));
        } else {
            assert!(hand_before.contributions.iter().sum::<u64>() >= 200);
        }
        f.h.set_clock(start);
        f.prepare(event, 0).unwrap();
        for (seat, player) in players.iter().enumerate() {
            let expected = if completed { before.seats[seat].stack } else { BUY_IN };
            let account: TableState = f.h.account(&table_state_pda(&config));
            assert_eq!(account.seats[seat].stack, expected);
            let refund = f.refund_ix(event, 0, seat as u8, player.pubkey());
            f.h.send(refund, &[&f.operator]).unwrap();
            assert_eq!(f.h.token_balance(&player.pubkey()), FAUCET_AMOUNT - BUY_IN + expected);
        }
        assert_eq!(f.h.vault_balance(&config), 0);
    }
}

#[test]
fn full_managed_table_keeps_private_dealing_within_the_existing_packet_and_compute_limits() {
    let mut f = ArenaFixture::new();
    let players: Vec<_> = (0..10).map(|n| f.h.player(&format!("private_{n}"))).collect();
    let config = f.tables[0];
    for (seat, player) in players.iter().enumerate() {
        join(&mut f.h, player, &config, seat as u8);
        set_ready(&mut f.h, player, &config, true);
    }
    let start = f.h.now + 600;
    f.schedule(start, 1_000_000).unwrap();
    f.h.advance(5);
    let signing = Signing { keys: &players, sessions: None };
    let mut meter = Meter::default();
    let (session, deal, slots) = deal_to_hole_cards(&mut f.h, &signing, &config, &mut meter);
    let expected = play_to_showdown(&mut f.h, &signing, &config, &session, &deal, &slots, &mut meter);
    let hand: HandState = f.h.account(&hand_pda(&config));
    assert_eq!(hand.stage, HandStage::Complete);
    assert_eq!(&hand.board[..5], expected.hand.as_ref().unwrap().board());
    for label in meter.labels() {
        let (cu, bytes) = meter.worst(label);
        assert!(bytes <= 1232 && cu <= 300_000, "managed {label}: {bytes} bytes / {cu} CU");
    }
}


fn migration_ix(f: &ArenaFixture) -> Instruction {
    use solana_sdk::instruction::AccountMeta;
    let fourth = f.tables[3];
    let mut accounts = riverproof::accounts::MigratePokerArena {
        operator: f.operator.pubkey(), arena: f.arena, table_d: fourth,
        pot_d: pda(&[b"poker-pot", fourth.as_ref()]), system_program: anchor_lang::system_program::ID,
    }.to_account_metas(None);
    for table in &f.tables[..3] {
        for key in [*table, table_state_pda(table), hand_pda(table), vault_pda(table), pda(&[b"poker-pot", table.as_ref()])] {
            accounts.push(AccountMeta::new_readonly(key, false));
        }
    }
    for key in [table_state_pda(&fourth), hand_pda(&fourth), vault_pda(&fourth)] {
        accounts.push(AccountMeta::new_readonly(key, false));
    }
    Instruction { program_id: program_id(), accounts, data: riverproof::instruction::MigratePokerArena {}.data() }
}

fn historical_prize_ix(f: &ArenaFixture, event: Pubkey, wallet: Pubkey, award_index: u8) -> Instruction {
    Instruction { program_id: program_id(), accounts: riverproof::accounts::ClaimPokerTournamentPrize {
        cranker: f.operator.pubkey(), arena: f.arena, tournament: event,
        prize_vault: pda(&[b"poker-prize", event.as_ref()]), recipient: wallet,
        recipient_tokens: get_associated_token_address(&wallet, &f.h.mint), mint: f.h.mint,
        token_program: anchor_spl::token_2022::ID, associated_token_program: anchor_spl::associated_token::ID,
        system_program: anchor_lang::system_program::ID,
    }.to_account_metas(None), data: riverproof::instruction::ClaimPokerTournamentPrize { award_index }.data() }
}

fn legacy_event(f: &ArenaFixture, event: Pubkey) -> riverproof::tournament::legacy::LegacyPokerTournament {
    use anchor_lang::AnchorDeserialize;
    let account = f.h.svm.get_account(&event).unwrap();
    assert_eq!(account.data.len(), riverproof::tournament::legacy::LEGACY_TOURNAMENT_LEN);
    riverproof::tournament::legacy::LegacyPokerTournament::deserialize(&mut &account.data[8..]).unwrap()
}

fn legacy_schedule(f: &mut ArenaFixture, start: i64) -> Pubkey {
    let registry = riverproof::tournament::legacy::read_legacy_arena(&f.h.svm.get_account(&f.arena).unwrap().data).unwrap();
    let event = pda(&[b"poker-tournament", f.arena.as_ref(), &registry.next_id.to_le_bytes()]);
    f.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::SchedulePokerTournament {
        sponsor: f.sponsor.pubkey(), arena: f.arena, tournament: event,
        prize_vault: pda(&[b"poker-prize", event.as_ref()]),
        sponsor_tokens: get_associated_token_address(&f.sponsor.pubkey(), &f.h.mint), mint: f.h.mint,
        token_program: anchor_spl::token_2022::ID, system_program: anchor_lang::system_program::ID,
    }.to_account_metas(None), data: riverproof::instruction::SchedulePokerTournament {
        starts_at: start, prize_sdice: 1_000_000, buy_in: BUY_IN,
    }.data() }, &[&f.sponsor]).unwrap();
    event
}

#[test]
fn legacy_arena_migration_rejects_authority_identity_rules_live_seats_and_unpaid_fees() {
    if std::env::var("LEGACY_POKER_PROGRAM_SO").is_err() { return; }
    let mut f = ArenaFixture::build(true);
    let original_arena = f.h.svm.get_account(&f.arena).unwrap();
    assert_eq!(original_arena.data.len(), 475);
    f.h.svm.add_program_from_file(program_id(), PROGRAM_SO).unwrap();
    let ix = migration_ix(&f);
    let mut unauthorized = ix.clone(); unauthorized.accounts[0].pubkey = f.sponsor.pubkey();
    assert!(f.h.send(unauthorized, &[&f.sponsor]).is_err());
    let wrong_arena = Pubkey::new_unique();
    f.h.svm.set_account(wrong_arena, original_arena.clone()).unwrap();
    let mut wrong = ix.clone(); wrong.accounts[1].pubkey = wrong_arena;
    assert!(f.h.send(wrong, &[&f.operator]).is_err());
    let mut wrong_state = ix.clone(); wrong_state.accounts[6].pubkey = table_state_pda(&f.tables[1]);
    assert!(f.h.send(wrong_state, &[&f.operator]).is_err());
    let fourth = f.tables[3];
    for address in [f.tables[0], fourth] {
        let original = f.h.svm.get_account(&address).unwrap();
        let mut changed = original.clone(); let mut config: TableConfig = f.h.account(&address);
        config.buy_in += 1; config.try_serialize(&mut &mut changed.data[..]).unwrap();
        f.h.svm.set_account(address, changed).unwrap();
        assert!(f.h.send(ix.clone(), &[&f.operator]).is_err());
        f.h.svm.set_account(address, original).unwrap();
    }
    let state_key = table_state_pda(&f.tables[0]);
    let state_original = f.h.svm.get_account(&state_key).unwrap();
    let mut occupied = state_original.clone(); let mut state: TableState = f.h.account(&state_key);
    state.seats[0].occupied = true; state.seats[0].owner = f.sponsor.pubkey();
    state.try_serialize(&mut &mut occupied.data[..]).unwrap(); f.h.svm.set_account(state_key, occupied).unwrap();
    assert!(f.h.send(ix.clone(), &[&f.operator]).unwrap_err().contains("TableNotEmpty"));
    f.h.svm.set_account(state_key, state_original).unwrap();
    let hand_key = hand_pda(&f.tables[0]); let hand_original = f.h.svm.get_account(&hand_key).unwrap();
    for unfinished in [true, false] {
        let mut changed = hand_original.clone(); let mut hand: HandState = f.h.account(&hand_key);
        hand.stage = if unfinished { HandStage::Preflop } else { HandStage::Complete };
        hand.settled = !unfinished;
        if !unfinished {
            hand.hand_id = 1; hand.contributions[0] = 50; hand.contributions[1] = 50;
            hand.winners[0].seat = 0; hand.winners[0].amount = 100; hand.winner_count = 1;
        }
        hand.try_serialize(&mut &mut changed.data[..]).unwrap(); f.h.svm.set_account(hand_key, changed).unwrap();
        let failure = f.h.send(ix.clone(), &[&f.operator]).unwrap_err();
        assert!(failure.contains(if unfinished { "HandInProgress" } else { "PotFeesPending" }), "{failure}");
        f.h.svm.set_account(hand_key, hand_original.clone()).unwrap();
    }
    assert_eq!(f.h.svm.get_account(&f.arena).unwrap().data, original_arena.data);
    assert!(f.h.svm.get_account(&pda(&[b"poker-pot", fourth.as_ref()])).is_none(), "failed migration cannot leave a ledger");
    let (cu, bytes) = f.h.send_metered(ix.clone(), &[&f.operator]).unwrap();
    assert!(cu < 300_000 && bytes <= 1232, "migration budget: {cu} CU, {bytes} bytes");
    println!("FOUR_TABLE_BUDGET migration cu={cu} bytes={bytes}");
    assert_eq!(f.h.svm.get_account(&f.arena).unwrap().data.len(), 563);
    assert!(f.h.send(ix, &[&f.operator]).is_err(), "migration cannot be replayed");
}

#[test]
fn compiled_legacy_upgrade_preserves_history_refunds_and_unclaimed_prizes() {
    use anchor_lang::AnchorSerialize;
    use riverproof::tournament::legacy::{read_legacy_arena, PrizeClaimAccount};
    if std::env::var("LEGACY_POKER_PROGRAM_SO").is_err() { return; }
    let mut f = ArenaFixture::build(true);
    let start = f.h.now + 600;
    let event = legacy_schedule(&mut f, start);
    let players: Vec<_> = (0..30).map(|n| f.h.player(&format!("legacy_{n}"))).collect();
    for player in &players { f.register(event, player).unwrap(); }
    let funded = f.h.svm.get_account(&event).unwrap();
    f.h.svm.add_program_from_file(program_id(), PROGRAM_SO).unwrap();
    let migration = migration_ix(&f);
    assert!(f.h.send(migration.clone(), &[&f.operator]).unwrap_err().contains("TournamentAlreadyScheduled"));
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, funded.data);
    // Complete the agreed old event using its real old executable and account
    // groups. The new program never retroactively changes its entry capacity.
    f.h.svm.add_program_from_file(program_id(), std::env::var("LEGACY_POKER_PROGRAM_SO").unwrap()).unwrap();
    f.h.set_clock(start);
    for table in 0..3 { f.prepare(event, table).unwrap(); }
    let mut activate = f.restore_ix(event); activate.accounts.truncate(9);
    activate.data = riverproof::instruction::ActivatePokerTournament {}.data();
    f.h.send(activate, &[&f.operator]).unwrap();
    for (i, player) in players.iter().take(3).enumerate() { f.enter(event, player, i, 0).unwrap(); }
    f.h.set_clock(start + 10_800);
    let mut finalize = f.finalize_ix(event); finalize.accounts.truncate(18);
    f.h.send(finalize, &[&f.operator]).unwrap();
    for (i, player) in players.iter().take(3).enumerate() {
        let table = f.tables[i];
        f.h.send(Instruction { program_id: program_id(), accounts: riverproof::accounts::RefundPokerTournamentBalance {
            cranker: f.operator.pubkey(), arena: f.arena, tournament: event, config: table,
            table_state: table_state_pda(&table), hand_state: hand_pda(&table), vault: vault_pda(&table), owner: player.pubkey(),
            owner_tokens: get_associated_token_address(&player.pubkey(), &f.h.mint), mint: f.h.mint,
            token_program: anchor_spl::token_2022::ID, associated_token_program: anchor_spl::associated_token::ID,
            system_program: anchor_lang::system_program::ID,
        }.to_account_metas(None), data: riverproof::instruction::RefundPokerTournamentBalance { entry_index: i as u8 }.data() }, &[&f.operator]).unwrap();
        assert_eq!(f.h.token_balance(&player.pubkey()), FAUCET_AMOUNT);
    }
    let mut restore = f.restore_ix(event); restore.accounts.truncate(9);
    f.h.send(restore, &[&f.operator]).unwrap();
    let old_event = legacy_event(&f, event);
    assert_eq!(old_event.state.entries.len(), 30); assert_eq!(old_event.refunded, 7);
    assert_eq!(old_event.state.phase, Phase::Complete); assert_eq!(old_event.awards.len(), 3);
    // One old payment is already complete; the others must survive the upgrade.
    let paid = historical_prize_ix(&f, event, old_event.awards[0].wallet, 0);
    f.h.send(paid.clone(), &[&f.operator]).unwrap();
    // Public one-base-unit donations cannot create a migration veto. Execute
    // real Token-2022 transfers, then preserve both old and fourth vault bytes.
    for table in [f.tables[0], f.tables[3]] {
        let donation = anchor_spl::token_2022::spl_token_2022::instruction::transfer_checked(
            &anchor_spl::token_2022::ID, &get_associated_token_address(&f.sponsor.pubkey(), &f.h.mint),
            &f.h.mint, &vault_pda(&table), &f.sponsor.pubkey(), &[], 1, 6).unwrap();
        f.h.send(donation, &[&f.sponsor]).unwrap();
    }
    let before_event = f.h.svm.get_account(&event).unwrap();
    let before_arena = f.h.svm.get_account(&f.arena).unwrap();
    let legacy_arena = read_legacy_arena(&before_arena.data).unwrap();
    let mut custody = Vec::new();
    for table in &f.tables[..3] {
        for address in [*table, table_state_pda(table), hand_pda(table), hand_cards_pda(table), vault_pda(table), pda(&[b"poker-pot", table.as_ref()])] {
            custody.push((address, f.h.svm.get_account(&address).unwrap()));
        }
    }
    let fourth_vault = vault_pda(&f.tables[3]);
    custody.push((fourth_vault, f.h.svm.get_account(&fourth_vault).unwrap()));
    let prize_key = pda(&[b"poker-prize", event.as_ref()]);
    let before_prize = f.h.svm.get_account(&prize_key).unwrap();
    f.h.svm.add_program_from_file(program_id(), PROGRAM_SO).unwrap();
    f.h.send(migration, &[&f.operator]).unwrap();
    assert_eq!(f.h.svm.get_account(&event).unwrap().data, before_event.data);
    assert_eq!(f.h.svm.get_account(&prize_key).unwrap().data, before_prize.data);
    for (address, before) in custody {
        let after = f.h.svm.get_account(&address).unwrap();
        assert_eq!(after.data, before.data); assert_eq!(after.lamports, before.lamports);
    }
    let arena: PokerArena = f.h.account(&f.arena);
    assert_eq!(&arena.tables[..3], &legacy_arena.tables); assert_eq!(arena.tables[3], f.tables[3]);
    assert_eq!(arena.next_id, legacy_arena.next_id); assert_eq!(arena.bank, legacy_arena.bank);
    assert_eq!(arena.bank_program, legacy_arena.bank_program); assert_eq!(arena.decision_counter, legacy_arena.decision_counter);
    assert_eq!(&arena.reconciled[..3], &legacy_arena.reconciled);
    for i in 0..3 {
        let mut a = Vec::new(); let mut b = Vec::new();
        arena.cash_rules[i].serialize(&mut a).unwrap(); legacy_arena.cash_rules[i].serialize(&mut b).unwrap(); assert_eq!(a, b);
    }
    assert_eq!((arena.cash_rules[3].buy_in, arena.cash_rules[3].small_blind, arena.cash_rules[3].big_blind), (100_000, 1_000, 2_000));
    assert!(f.h.send(paid, &[&f.operator]).is_err());
    // Exact 100K cash admission and withdrawal preserve both the public token
    // donation and every original table row. No fee is due without a played hand.
    let untouched: Vec<_> = f.tables[..3].iter().flat_map(|table| [*table, table_state_pda(table),
        hand_pda(table), hand_cards_pda(table), vault_pda(table), pda(&[b"poker-pot", table.as_ref()])])
        .map(|key| (key, f.h.svm.get_account(&key).unwrap().data)).collect();
    let sponsor_ata = get_associated_token_address(&f.sponsor.pubkey(), &f.h.mint);
    let before_cash = f.h.svm.get_account(&sponsor_ata).unwrap().data;
    join_with(&mut f.h, &f.sponsor, &f.tables[3], 0, None).unwrap();
    let after_join = f.h.svm.get_account(&sponsor_ata).unwrap().data;
    assert_eq!(u64::from_le_bytes(before_cash[64..72].try_into().unwrap())
        - u64::from_le_bytes(after_join[64..72].try_into().unwrap()), 100_000 * riverproof::sdice::UNITS);
    let joined: TableState = f.h.account(&table_state_pda(&f.tables[3]));
    assert_eq!((joined.escrowed, joined.seats[0].stack), (100_000, 100_000));
    let seat_accounts = seat_accounts(&f.h, &f.sponsor.pubkey(), &f.tables[3]);
    f.h.send(Instruction { program_id: program_id(), accounts: seat_accounts.to_account_metas(None),
        data: riverproof::instruction::LeaveTable {}.data() }, &[&f.sponsor]).unwrap();
    assert_eq!(f.h.svm.get_account(&sponsor_ata).unwrap().data, before_cash);
    let cash: TableState = f.h.account(&table_state_pda(&f.tables[3]));
    assert_eq!(cash.escrowed, 0); assert!(cash.seats.iter().all(|seat| !seat.occupied));
    let vault: anchor_spl::token_interface::TokenAccount = f.h.account(&fourth_vault);
    assert_eq!(vault.amount, 1, "cash-out leaves the donated base unit intact");
    for (key, before) in untouched { assert_eq!(f.h.svm.get_account(&key).unwrap().data, before); }
    println!("FOUR_TABLE_CASH buy_in=100000 small_blind=1000 big_blind=2000 full_refund=true donation_retained=true");
    // A new forty-entry event does not invalidate any previous frozen claim.
    let next_start = f.h.now + 600;
    let next = f.schedule(next_start, 1_000_000).unwrap();
    assert_eq!(f.h.svm.get_account(&next).unwrap().data.len(), 3290);
    assert_eq!(f.h.account::<PokerTournament>(&next).state.entries.len(), 40);
    let old = legacy_event(&f, event);
    for i in 1..old.awards.len() {
        let award = old.awards[i]; let before = f.h.svm.get_account(&get_associated_token_address(&award.wallet, &f.h.mint)).unwrap();
        let claim = historical_prize_ix(&f, event, award.wallet, i as u8);
        let mut redirected = claim.clone(); redirected.accounts[4].pubkey = f.sponsor.pubkey();
        redirected.accounts[5].pubkey = get_associated_token_address(&f.sponsor.pubkey(), &f.h.mint);
        assert!(f.h.send(redirected, &[&f.operator]).is_err());
        let original = f.h.svm.get_account(&event).unwrap();
        for corrupt in [0, 1, 2] {
            let mut bad = original.clone();
            if corrupt == 0 { bad.data[0] ^= 1; } else if corrupt == 1 { bad.data.push(0); } else { bad.owner = anchor_lang::system_program::ID; }
            f.h.svm.set_account(event, bad).unwrap();
            assert!(f.h.send(claim.clone(), &[&f.operator]).is_err());
            f.h.svm.set_account(event, original.clone()).unwrap();
        }
        let mut expected = PrizeClaimAccount::read(&original.data).unwrap();
        let mut expected_bytes = original.data.clone(); expected.mark_paid(i as u8, &mut expected_bytes).unwrap();
        let (cu, bytes) = f.h.send_metered(claim.clone(), &[&f.operator]).unwrap();
        assert!(cu < 300_000 && bytes <= 1232, "legacy prize budget: {cu} CU, {bytes} bytes");
        let after = f.h.svm.get_account(&get_associated_token_address(&award.wallet, &f.h.mint)).unwrap();
        assert_eq!(u64::from_le_bytes(after.data[64..72].try_into().unwrap()) - u64::from_le_bytes(before.data[64..72].try_into().unwrap()), award.amount);
        let after_event = f.h.svm.get_account(&event).unwrap().data;
        assert_eq!(after_event, expected_bytes);
        let changed: Vec<_> = original.data.iter().zip(&after_event).enumerate()
            .filter_map(|(offset, (before, after))| (before != after).then_some(offset)).collect();
        assert_eq!(changed, vec![2200], "legacy completed-event claim changes only its existing paid byte");
        assert!(f.h.send(claim, &[&f.operator]).is_err());
    }
    let finished = legacy_event(&f, event);
    assert_eq!(finished.refunded, 7); assert_eq!(finished.prizes_paid, 7);
    assert_eq!(finished.state.entries.len(), 30); assert_eq!(finished.state.registered, 30);
    let prize: anchor_spl::token_interface::TokenAccount = f.h.account(&prize_key);
    assert_eq!(prize.amount, 0);
}
