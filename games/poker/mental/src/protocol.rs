//! A whole deal: the published transcript, and the players who produced it.
//!
//! [`Deal`] is everything a hand makes public — every shuffle round, every lock
//! round, every commitment. It carries no secrets, so anyone can hold it and
//! [`Deal::verify`] it: a spectator, a rollup sequencer, or eventually the
//! program. [`Participant`] is one player's side, and the only place a secret
//! lives.
//!
//! [`Session`] puts every player's secrets in one process to drive a hand end
//! to end. That is not how a hand is played — it is how a hand is tested, and
//! how the fixture the TypeScript player is checked against gets generated.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;

use crate::deal::{verify_peel, Peel};
use crate::group::{basepoint, encode_point, ordered_deck, Seed, Transcript};
use crate::lock::{lock, verify_lock, LockRound, LockSecret};
use crate::share::{board_threshold, open_share, recover, Recipient};
use crate::shuffle::{shuffle, verify_shuffle, ShuffleRound, ShuffleSecret};
use crate::{MentalError, MIN_PLAYERS};

/// One player's side of a deal. Holds the only copies of their secrets.
pub struct Participant {
    pub player: u8,
    seed: Seed,
    identity: Scalar,
    public_key: [u8; 32],
    shuffled: Option<ShuffleSecret>,
    locked: Option<LockSecret>,
}

impl Participant {
    pub fn new(player: u8, seed: Seed) -> Self {
        let identity = seed.scalar("identity", 0);
        Self {
            player,
            seed,
            identity,
            public_key: encode_point(&(basepoint() * identity)),
            shuffled: None,
            locked: None,
        }
    }

    /// The public half of this player's key exchange key, which they register
    /// on chain with their shuffle round.
    pub fn public_key(&self) -> [u8; 32] {
        self.public_key
    }

    /// How this player is addressed when somebody splits a key to them.
    pub fn recipient(&self) -> Recipient {
        Recipient { player: self.player, public_key: self.public_key }
    }

    /// Masks and reorders the deck. `order` is for tests that need the joint
    /// shuffle to land somewhere known; in play it is `None`.
    pub fn shuffle(
        &mut self,
        input: &[RistrettoPoint],
        order: Option<&[u8]>,
    ) -> Result<ShuffleRound, MentalError> {
        let (round, secret) = shuffle(&self.seed, self.player, input, order)?;
        self.shuffled = Some(secret);
        Ok(round)
    }

    /// Strips this player's mask and locks every position under its own key.
    pub fn lock(
        &mut self,
        input: &[RistrettoPoint],
        recipients: &[Recipient],
        shared_positions: &[u8],
        threshold: u8,
    ) -> Result<LockRound, MentalError> {
        let mask = self.shuffled.as_ref().ok_or(MentalError::RoundOutOfOrder)?.mask;
        let (round, secret) = lock(
            &self.seed,
            self.player,
            &mask,
            &self.identity,
            input,
            recipients,
            shared_positions,
            threshold,
        )?;
        self.locked = Some(secret);
        Ok(round)
    }

    /// This player's key to one position.
    fn key(&self, position: u8) -> Result<Scalar, MentalError> {
        self.locked
            .as_ref()
            .ok_or(MentalError::RoundOutOfOrder)?
            .keys
            .get(position as usize)
            .copied()
            .ok_or(MentalError::PositionOutOfRange)
    }

    /// Publishes this player's key to one position, taking their layer off it.
    ///
    /// For somebody else's hole card this is the whole of what a player owes:
    /// one scalar, opening one card, for one other player.
    pub fn peel(&self, position: u8) -> Result<Peel, MentalError> {
        Ok(Peel::new(self.player, position, &self.key(position)?))
    }

    /// The same layer, in the form the chain can use: the key's inverse.
    ///
    /// Off chain a player publishes the key itself, because it can be checked
    /// against the commitment they made for that position. On chain there is no
    /// commitment stored to check against and no scalar inversion available, so
    /// what travels is the value that takes the layer off in one multiplication.
    ///
    /// Both carry exactly the same secret — either one opens that position and
    /// neither says anything about any other.
    pub fn unlock(&self, position: u8) -> Result<Scalar, MentalError> {
        Ok(self.key(position)?.invert())
    }

    /// Finishes a card the other players have already peeled.
    pub fn open(
        &self,
        residual: &RistrettoPoint,
        position: u8,
    ) -> Result<u8, MentalError> {
        crate::deal::open(residual, &[self.key(position)?])
    }

    /// Opens a share addressed to this player, checking it as it goes.
    pub fn read_share(
        &self,
        package: &crate::share::SharePackage,
        dealer_public: &[u8; 32],
    ) -> Result<Scalar, MentalError> {
        open_share(package, self.player, &self.identity, dealer_public)
    }
}

fn flat(points: &[[u8; 32]]) -> Vec<u8> {
    points.iter().flat_map(|point| point.iter().copied()).collect()
}

fn absorb_shuffle_round(transcript: &mut Transcript, round: &ShuffleRound) {
    transcript.absorb_u8("player", round.player);
    transcript.absorb("identity", &round.identity);
    transcript.absorb("mask", &round.mask);
    transcript.absorb("shuffled", &flat(&round.deck));
    transcript.absorb("permutation", &flat(&round.proof.permutation));
    transcript.absorb("bridges", &flat(&round.proof.bridges));
    transcript.absorb("shuffle-challenge", &round.proof.challenge);
    transcript.absorb("shuffle-responses", &flat(&round.proof.responses));
    transcript.absorb("bridge-responses", &flat(&round.proof.bridge_responses));
    transcript.absorb("blind-response", &round.proof.blind_response);
    transcript.absorb("ones-response", &round.proof.ones_response);
    transcript.absorb("mask-response", &round.proof.mask_response);
}

fn absorb_lock_round(transcript: &mut Transcript, round: &LockRound) {
    transcript.absorb_u8("player", round.player);
    transcript.absorb("unmasked", &flat(&round.unmasked));
    transcript.absorb("locked", &flat(&round.deck));
    transcript.absorb("keys", &flat(&round.keys));
    transcript.absorb("unmask-challenge", &round.unmask_proof.challenge);
    transcript.absorb("unmask-response", &round.unmask_proof.response);
    transcript.absorb("lock-challenge", &round.lock_proof.challenge);
    transcript.absorb("lock-responses", &flat(&round.lock_proof.responses));
    for package in &round.shares {
        transcript.absorb_u8("position", package.position);
        transcript.absorb("commitments", &flat(&package.commitments));
        for share in &package.shares {
            transcript.absorb_u8("holder", share.holder);
            transcript.absorb("ciphertext", &share.ciphertext);
        }
    }
}

/// What a player commits on chain when they publish a shuffle round.
///
/// The round itself is 8,481 bytes — four fifths of it the shuffle argument —
/// and every player verifies it in full, so only this reaches the chain: enough
/// that a player cannot afterwards claim they published something else, and
/// enough to say whose round was wrong when a card fails to open.
///
/// Truncated to the thirty-two bytes the account holds. Finding a second round
/// with the same leading 256 bits is the work of breaking a 256-bit hash, which
/// is what a commitment needs.
pub fn shuffle_round_digest(round: &ShuffleRound) -> [u8; 32] {
    let mut transcript = Transcript::new("shuffle-round");
    absorb_shuffle_round(&mut transcript, round);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&transcript.digest()[..32]);
    digest
}

/// The same, for a lock round. 7,589 bytes three-handed, and the same thirty-two.
/// What the author commits on chain for a lock round.
///
/// The digest alone cannot be opened at one position, and the program needs
/// exactly that: to check one unlock share it must learn `k * G` for one
/// position and be sure it is the one this author published. So the commitment
/// binds the digest to a Merkle root over the round's keys — a client still
/// re-derives both halves, and the program can open one leaf.
pub fn lock_round_commitment(round: &LockRound) -> [u8; 32] {
    crate::merkle::lock_commitment(&lock_round_digest(round), &crate::merkle::root(&round.keys))
}

pub fn lock_round_digest(round: &LockRound) -> [u8; 32] {
    let mut transcript = Transcript::new("lock-round");
    absorb_lock_round(&mut transcript, round);
    let mut digest = [0u8; 32];
    digest.copy_from_slice(&transcript.digest()[..32]);
    digest
}

/// Everything a deal makes public.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Deal {
    pub threshold: u8,
    /// The positions whose keys were split, so all but one can open them. In
    /// practice the five board positions; hole cards are never in here.
    pub shared_positions: Vec<u8>,
    pub recipients: Vec<Recipient>,
    pub shuffles: Vec<ShuffleRound>,
    pub locks: Vec<LockRound>,
}

impl Deal {
    pub fn players(&self) -> u8 {
        self.recipients.len() as u8
    }

    /// Checks the whole transcript from the ordered deck forward, and returns
    /// the locked deck it ends at.
    ///
    /// Everything a party without secrets can check is checked here: every
    /// round's arithmetic, both proofs per lock round, and that each lock round
    /// strips the mask its own shuffle round committed to.
    pub fn verify(&self) -> Result<Vec<RistrettoPoint>, MentalError> {
        let players = self.recipients.len();
        if players < MIN_PLAYERS as usize {
            return Err(MentalError::TooFewPlayers);
        }
        if self.shuffles.len() != players || self.locks.len() != players {
            return Err(MentalError::WrongPlayerCount);
        }
        if self.threshold < 2 || self.threshold as usize > players {
            return Err(MentalError::BadThreshold);
        }

        let mut deck = ordered_deck();
        for round in &self.shuffles {
            deck = verify_shuffle(round, &deck)?;
        }
        for (index, round) in self.locks.iter().enumerate() {
            // A lock round strips one mask, and it has to be its own.
            if round.player != self.shuffles[index].player {
                return Err(MentalError::RoundOutOfOrder);
            }
            deck = verify_lock(
                round,
                &self.shuffles[index].mask,
                &deck,
                &self.recipients,
                &self.shared_positions,
                self.threshold,
            )?;
        }
        Ok(deck)
    }

    /// A commitment to the whole published transcript.
    ///
    /// One value that changes if any byte of the deal changes: the shuffle
    /// rounds, the lock rounds, both proofs, every share. It is what a hand
    /// would put on chain to say "this is the deck we are playing", and it is
    /// what holds the TypeScript player to this one — the two derive every
    /// secret from the same seeds, so for the same seeds they must produce the
    /// same digest, and `tests/mental.test.mts` checks that they do.
    pub fn digest(&self) -> [u8; 64] {
        let mut transcript = Transcript::new("deal");
        transcript.absorb_u8("threshold", self.threshold);
        transcript.absorb("shared-positions", &self.shared_positions);
        for recipient in &self.recipients {
            transcript.absorb_u8("player", recipient.player);
            transcript.absorb("public-key", &recipient.public_key);
        }
        for round in &self.shuffles {
            absorb_shuffle_round(&mut transcript, round);
        }
        for round in &self.locks {
            absorb_lock_round(&mut transcript, round);
        }
        transcript.digest()
    }

    /// What player `player` committed their key for `position` to be.
    pub fn key_commitment(&self, player: u8, position: u8) -> Result<[u8; 32], MentalError> {
        let round = self
            .locks
            .iter()
            .find(|round| round.player == player)
            .ok_or(MentalError::UnknownPlayer)?;
        round
            .keys
            .get(position as usize)
            .copied()
            .ok_or(MentalError::PositionOutOfRange)
    }

    /// That player's public key, for reading a share they dealt.
    pub fn public_key(&self, player: u8) -> Result<[u8; 32], MentalError> {
        self.recipients
            .iter()
            .find(|recipient| recipient.player == player)
            .map(|recipient| recipient.public_key)
            .ok_or(MentalError::UnknownPlayer)
    }

    /// Their split of one position's key, if that position was shared.
    pub fn share_package(
        &self,
        player: u8,
        position: u8,
    ) -> Result<&crate::share::SharePackage, MentalError> {
        self.locks
            .iter()
            .find(|round| round.player == player)
            .ok_or(MentalError::UnknownPlayer)?
            .shares
            .iter()
            .find(|package| package.position == position)
            .ok_or(MentalError::PositionOutOfRange)
    }

    /// Checks a peel against the commitment its player made.
    pub fn check_peel(&self, peel: &Peel) -> Result<Scalar, MentalError> {
        verify_peel(peel, &self.key_commitment(peel.player, peel.position)?)
    }
}

/// A hand with every player's secrets in one process.
///
/// Only tests and fixture generation build one of these. A real table has one
/// [`Participant`] per browser and a [`Deal`] everybody can see.
pub struct Session {
    pub deal: Deal,
    pub participants: Vec<Participant>,
    pub locked: Vec<RistrettoPoint>,
}

/// Runs a whole deal: shuffle rounds, then lock rounds, then verification.
///
/// `orders` overrides each player's permutation, which only tests do — it is
/// how the differential suite steers the joint shuffle onto a recorded deck so
/// the hand that comes out can be compared with the one the oracle played.
pub fn deal_hand(
    seeds: &[Seed],
    shared_positions: &[u8],
    orders: Option<&[Vec<u8>]>,
) -> Result<Session, MentalError> {
    if seeds.len() < MIN_PLAYERS as usize {
        return Err(MentalError::TooFewPlayers);
    }
    if let Some(orders) = orders {
        if orders.len() != seeds.len() {
            return Err(MentalError::WrongPlayerCount);
        }
    }

    let mut participants: Vec<Participant> = seeds
        .iter()
        .enumerate()
        .map(|(player, seed)| Participant::new(player as u8, *seed))
        .collect();
    let recipients: Vec<Recipient> =
        participants.iter().map(Participant::recipient).collect();
    debug_assert_eq!(recipients.len(), seeds.len());
    let threshold = board_threshold(participants.len() as u8);

    let mut deck = ordered_deck();
    let mut shuffles = Vec::with_capacity(participants.len());
    for (index, participant) in participants.iter_mut().enumerate() {
        let order = orders.map(|orders| orders[index].as_slice());
        let round = participant.shuffle(&deck, order)?;
        deck = verify_shuffle(&round, &deck)?;
        shuffles.push(round);
    }

    let mut locks = Vec::with_capacity(participants.len());
    for participant in participants.iter_mut() {
        let round = participant.lock(&deck, &recipients, shared_positions, threshold)?;
        deck = crate::group::decode_deck(&round.deck)?;
        locks.push(round);
    }

    let deal = Deal {
        threshold,
        shared_positions: shared_positions.to_vec(),
        recipients,
        shuffles,
        locks,
    };
    // Everything published is checked before anybody plays a card, exactly as
    // a party holding only the transcript would check it.
    let locked = deal.verify()?;
    Ok(Session { deal, participants, locked })
}

impl Session {
    fn participant(&self, player: u8) -> Result<&Participant, MentalError> {
        self.participants
            .iter()
            .find(|participant| participant.player == player)
            .ok_or(MentalError::UnknownPlayer)
    }

    fn card(&self, position: u8) -> Result<RistrettoPoint, MentalError> {
        self.locked
            .get(position as usize)
            .copied()
            .ok_or(MentalError::PositionOutOfRange)
    }

    /// The peels every player except `owner` owes for one position, each one
    /// checked against its commitment the way a recipient would check it.
    pub fn peels_for(&self, owner: u8, position: u8) -> Result<Vec<Scalar>, MentalError> {
        self.participants
            .iter()
            .filter(|participant| participant.player != owner)
            .map(|participant| self.deal.check_peel(&participant.peel(position)?))
            .collect()
    }

    /// What is left of a hole card once everybody else has peeled: a point only
    /// its owner's key opens.
    pub fn residual(&self, owner: u8, position: u8) -> Result<RistrettoPoint, MentalError> {
        Ok(crate::deal::strip(
            &self.card(position)?,
            &self.peels_for(owner, position)?,
        ))
    }

    /// Opens a hole card as its owner: everybody else peels, the owner
    /// finishes.
    pub fn open_hole(&self, owner: u8, position: u8) -> Result<u8, MentalError> {
        let residual = self.residual(owner, position)?;
        self.participant(owner)?.open(&residual, position)
    }

    /// Rebuilds one player's key for one position from a threshold of the
    /// shares they split during the lock.
    ///
    /// This is what makes a board card nobody's property: a player who stops
    /// answering does not take the board with them.
    pub fn recover_key(
        &self,
        absent: u8,
        position: u8,
        holders: &[u8],
    ) -> Result<Scalar, MentalError> {
        let package = self.deal.share_package(absent, position)?;
        let dealer_public = self.deal.public_key(absent)?;
        let mut shares = Vec::with_capacity(holders.len());
        for holder in holders {
            let value = self.participant(*holder)?.read_share(package, &dealer_public)?;
            shares.push((*holder, value));
        }
        let key = recover(&shares, self.deal.threshold)?;

        // A rebuilt key is still just a key, so it is checked like any other.
        if encode_point(&(basepoint() * key))
            != self.deal.key_commitment(absent, position)?
        {
            return Err(MentalError::BadShare);
        }
        Ok(key)
    }

    /// Opens a board card from the players who are present.
    ///
    /// Everyone present publishes their own key; anybody missing has theirs
    /// rebuilt from the shares they left behind. Fewer than the threshold and
    /// this fails, which is the half of the claim worth testing.
    pub fn open_board(&self, position: u8, present: &[u8]) -> Result<u8, MentalError> {
        if !self.deal.shared_positions.contains(&position) {
            return Err(MentalError::PositionOutOfRange);
        }
        if present.len() < self.deal.threshold as usize {
            return Err(MentalError::NotEnoughShares);
        }

        let mut keys = Vec::with_capacity(self.participants.len());
        for participant in &self.participants {
            let key = if present.contains(&participant.player) {
                self.deal.check_peel(&participant.peel(position)?)?
            } else {
                self.recover_key(participant.player, position, present)?
            };
            keys.push(key);
        }
        crate::deal::open(&self.card(position)?, &keys)
    }

    /// The masks every player used, for [`crate::shuffle::audit`].
    ///
    /// Only a dispute should ever call this: with the masks out, the whole
    /// deck is readable, folded hands included.
    pub fn masks(&self) -> Result<Vec<Scalar>, MentalError> {
        self.participants
            .iter()
            .map(|participant| {
                participant
                    .shuffled
                    .as_ref()
                    .map(|secret| secret.mask)
                    .ok_or(MentalError::RoundOutOfOrder)
            })
            .collect()
    }

    /// The whole deck in the clear, as the players between them know it.
    ///
    /// Tests use it to say what the shuffle *should* have produced; a hand
    /// never computes it, because computing it is the thing being prevented.
    pub fn deck(&self) -> Result<Vec<u8>, MentalError> {
        (0..self.locked.len() as u8)
            .map(|position| {
                let keys: Result<Vec<Scalar>, MentalError> = self
                    .participants
                    .iter()
                    .map(|participant| participant.key(position))
                    .collect();
                crate::deal::open(&self.card(position)?, &keys?)
            })
            .collect()
    }
}

/// Point the last player's permutation at whatever makes the deal come out as
/// `target`.
///
/// Every other player still shuffles freely and still hides their own ordering;
/// this only fixes where the composition lands. It exists so a hand dealt by
/// the protocol can be compared against a hand the JavaScript oracle recorded,
/// card for card, rather than merely looking plausible.
pub fn steer_last_order(earlier: &[Vec<u8>], target: &[u8]) -> Vec<u8> {
    let mut composed: Vec<u8> = (0..target.len() as u8).collect();
    for order in earlier {
        composed = crate::shuffle::compose(&composed, order);
    }
    let inverse = crate::shuffle::invert_order(&composed);
    target.iter().map(|card| inverse[*card as usize]).collect()
}
