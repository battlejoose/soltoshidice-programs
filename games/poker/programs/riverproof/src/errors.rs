use anchor_lang::prelude::*;
use riverproof_engine::EngineError;

#[error_code]
pub enum RiverproofError {
    #[msg("Usernames are 3 to 20 characters of letters, digits, or underscores.")]
    InvalidUsername,
    #[msg("That username is already claimed.")]
    UsernameTaken,
    #[msg("The faucet is not ready to refill this account yet.")]
    FaucetCooldown,
    #[msg("That seat is already taken.")]
    SeatOccupied,
    #[msg("You do not have a seat at this table.")]
    NoSeat,
    #[msg("That seat is empty.")]
    SeatEmpty,
    #[msg("You are already seated at this table.")]
    AlreadySeated,
    #[msg("Rebuy is available after you run out of chips.")]
    RebuyNotAvailable,
    #[msg("You cannot do that during a hand.")]
    HandInProgress,
    #[msg("There is no active hand.")]
    NoActiveHand,
    #[msg("That action belongs to an earlier hand.")]
    WrongHand,
    #[msg("The table changed before that action arrived.")]
    StaleVersion,
    #[msg("It is not your turn.")]
    NotYourTurn,
    #[msg("The action timer has expired.")]
    ActionExpired,
    #[msg("You cannot check while facing a bet.")]
    CannotCheckFacingBet,
    #[msg("There is no bet to call.")]
    NoBetToCall,
    #[msg("Raising has not been reopened.")]
    RaiseNotReopened,
    #[msg("Raising is not available.")]
    RaiseUnavailable,
    #[msg("That raise is below the minimum.")]
    RaiseBelowMinimum,
    #[msg("That raise exceeds your stack.")]
    RaiseExceedsStack,
    #[msg("A raise must exceed the current bet.")]
    RaiseNotAboveCurrentBet,
    #[msg("A hand needs at least two ready players.")]
    NotEnoughPlayers,
    #[msg("The action timer has not expired yet.")]
    TimerNotExpired,
    #[msg("The next hand is not due yet.")]
    NextHandNotDue,
    #[msg("This signer may not act for that seat.")]
    UnauthorizedActor,
    #[msg("That session key has expired.")]
    SessionExpired,
    #[msg("That session key is scoped to a different table.")]
    SessionTableMismatch,
    #[msg("A session key may not authorize another session key.")]
    SessionMayNotDelegate,
    #[msg("Buy-in must match the table configuration.")]
    InvalidBuyIn,
    #[msg("Table settings are outside the supported range.")]
    InvalidTableConfig,
    #[msg("The vault balance does not cover the table's chips.")]
    VaultUnderfunded,
    #[msg("Cash out the whole stack; partial withdrawals are not supported.")]
    PartialCashOutUnsupported,
    #[msg("The confidential computation was aborted.")]
    AbortedComputation,
    #[msg("This hand has not been dealt confidentially.")]
    NotConfidential,
    #[msg("The hand is not waiting for a card reveal.")]
    NotAwaitingCards,
    #[msg("The reveal delivered the wrong number of cards.")]
    WrongCardCount,
    #[msg("A player-shuffled hand needs at least three ready players.")]
    NotEnoughPlayersForMentalDeal,
    #[msg("That unlock share is not a usable scalar for this card.")]
    InvalidUnlockShare,
    #[msg("That unlock key is not the one its author committed to.")]
    UnlockKeyNotCommitted,
    #[msg("You have already published an unlock share for that card.")]
    UnlockShareAlreadyGiven,
    #[msg("The unlock shares did not open a card; somebody published a wrong one.")]
    UnlockDidNotOpenACard,
    #[msg("That player is not part of this deal.")]
    UnknownDealPlayer,
    #[msg("It is not your turn to publish a round.")]
    NotYourDealTurn,
    #[msg("The deal is not at that stage.")]
    WrongDealPhase,
    #[msg("A round commitment may not be empty.")]
    EmptyRoundCommitment,
    #[msg("Every player must lock the deck before it can be sealed.")]
    DealNotLocked,
    #[msg("That is not a position this hand deals.")]
    UnknownDealSlot,
    #[msg("The wrong number of card positions was supplied.")]
    WrongDealSlotCount,
    #[msg("That board card is not due to be opened yet.")]
    BoardCardNotDue,
    #[msg("That card is not open yet; some player has not published a share.")]
    CardNotOpenYet,
    #[msg("Publish your own unlock share with publish_unlocks, not this.")]
    CannotRecoverYourOwnKey,
    #[msg("That seat has not been given long enough to publish its own share.")]
    RecoveryNotDue,
    #[msg("That seat has already shown its hand.")]
    HoleAlreadyRevealed,
    #[msg("You have already committed that round.")]
    RoundAlreadyCommitted,
    #[msg("This table is private. Joining it needs its password.")]
    TableLocked,
    #[msg("Tables are played through a Helius endpoint.")]
    EndpointNotAllowed,
    #[msg("That endpoint is already carrying as many tables as it may.")]
    EndpointAtCapacity,
    #[msg("Only the faucet authority may change an endpoint's table limit.")]
    NotFaucetAuthority,
    #[msg("Only the player who created this table may close it.")]
    NotTableCreator,
    #[msg("A table can only be closed once every seat is empty.")]
    TableNotEmpty,
    // Appended, and always appended: the discriminant is the error number the
    // client matches on, so inserting one anywhere else renumbers the rest.
    #[msg("This session key has been replaced. Authorize a new one and retry.")]
    SessionKeyReplaced,
    #[msg("Include this table's on-chain tournament registry.")]
    TournamentAccountRequired,
    #[msg("The tournament accounts or schedule are invalid.")]
    InvalidTournament,
    #[msg("This table is reserved for the scheduled tournament.")]
    TournamentTableReserved,
    #[msg("The scheduled takeover time has not arrived.")]
    TournamentNotDue,
    #[msg("That poker arena already has a funded tournament.")]
    TournamentAlreadyScheduled,
    #[msg("The completed hand's pot fee is invalid.")]
    InvalidPotFee,
    #[msg("This completed hand still has pot fees to settle.")]
    PotFeesPending,
    #[msg("That winner's pot fee was already paid.")]
    PotFeeAlreadyPaid,
    #[msg("All thirty tournament registration places are taken")]
    TournamentFull,
    #[msg("This wallet is already registered in this tournament")]
    TournamentAlreadyRegistered,
    #[msg("The tournament's fifteen-minute entry window has closed")]
    TournamentEntryClosed,
    #[msg("Record the completed tournament hand before starting another hand")]
    TournamentResultPending,
}

impl From<EngineError> for RiverproofError {
    fn from(error: EngineError) -> Self {
        match error {
            EngineError::NoActiveHand => Self::NoActiveHand,
            EngineError::WrongHand => Self::WrongHand,
            EngineError::StaleVersion => Self::StaleVersion,
            EngineError::NotYourTurn => Self::NotYourTurn,
            EngineError::ActionExpired => Self::ActionExpired,
            EngineError::CannotCheckFacingBet => Self::CannotCheckFacingBet,
            EngineError::NoBetToCall => Self::NoBetToCall,
            EngineError::RaiseNotReopened => Self::RaiseNotReopened,
            EngineError::RaiseUnavailable => Self::RaiseUnavailable,
            EngineError::RaiseBelowMinimum => Self::RaiseBelowMinimum,
            EngineError::RaiseExceedsStack => Self::RaiseExceedsStack,
            EngineError::RaiseNotAboveCurrentBet => Self::RaiseNotAboveCurrentBet,
            EngineError::NotEnoughPlayers => Self::NotEnoughPlayers,
            EngineError::NotEnoughPlayersForMentalDeal => Self::NotEnoughPlayersForMentalDeal,
            EngineError::HandInProgress => Self::HandInProgress,
            EngineError::SeatEmpty => Self::SeatEmpty,
            EngineError::NotAwaitingCards => Self::NotAwaitingCards,
            EngineError::WrongCardCount => Self::WrongCardCount,

        }
    }
}
