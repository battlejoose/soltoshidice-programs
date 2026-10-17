//! Feldman verifiable secret sharing, for the board threshold.
//!
//! Hole cards need no sharing: only their owner should ever open them, and a
//! player who will not open their own cards has folded. The board is the
//! opposite — it belongs to nobody, so somebody other than its author has to be
//! able to open it when its author walks away from the table.
//!
//! So every player, while locking the deck, splits each *board* position's key
//! across the other players with a threshold. All but one of them can rebuild a
//! missing player's key and turn the board over; a minority cannot. There is no
//! operator share and there must never be one: a threshold that always includes
//! the operator means the operator plus one player opens any card, which is the
//! trusted party this whole scheme exists to remove.
//!
//! Feldman rather than plain Shamir because a share arrives encrypted and a
//! recipient must be able to tell a real share from a plausible one *before* it
//! matters. The commitments make that a local check.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use sha2::{Digest, Sha512};

use crate::group::{
    basepoint, decode_point, decode_scalar, encode_point, encode_scalar, Seed, CARD_COUNT,
};
use crate::MentalError;

const SHARE_DOMAIN: &[u8] = b"riverproof/mental/share/v1";

/// A player another player can hand a share to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Recipient {
    pub player: u8,
    /// The public half of that player's per-hand key exchange key.
    pub public_key: [u8; 32],
}

/// One player's split of one position's key.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SharePackage {
    pub dealer: u8,
    pub position: u8,
    /// `a_m * G` for each polynomial coefficient. `commitments[0]` is the key
    /// commitment itself, so this doubles as the public statement of what is
    /// being shared.
    pub commitments: Vec<[u8; 32]>,
    pub shares: Vec<EncryptedShare>,
}

/// A share, readable only by the player it was addressed to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct EncryptedShare {
    pub holder: u8,
    pub ciphertext: [u8; 32],
}

/// The threshold Hash Holdem uses: every player but one.
///
/// Chosen so that **any two honest players can stop the board being opened
/// early**. A coalition can rebuild an absent player's key only if it can
/// reach the threshold, so blocking them means `players - 2 < t`, which is
/// `t >= players - 1`. That makes `players - 1` the smallest threshold that
/// buys the property — `players` itself costs more and buys nothing further.
///
/// It was a strict majority (`players / 2 + 1`) before, under which half the
/// table plus one could open the board ahead of everybody else. The two agree
/// at three and four players and diverge from five up.
///
/// The cost is on the other side of the same coin: a key can be rebuilt only
/// while at most one player is missing. That is not a regression today — the
/// program opens a slot only when *every* player has peeled it
/// (`Slot::is_open`), so the chain has never routed around an absent player —
/// but it does cap what such a crank could ever tolerate.
///
/// Below three players there is no workable threshold at all: two players give
/// 1, where either can open the board early. The engine refuses that table
/// rather than pretending otherwise.
pub fn board_threshold(players: u8) -> u8 {
    players.saturating_sub(1)
}

/// Shamir's evaluation point for a player. Never zero, which is the secret.
fn evaluation_point(player: u8) -> Scalar {
    Scalar::from(player as u64 + 1)
}

/// The pad a share travels under: a one-time key from the two players' shared
/// secret, bound to who sent it, who it is for, and which position it opens.
fn share_pad(shared: &RistrettoPoint, dealer: u8, holder: u8, position: u8) -> [u8; 32] {
    let mut hasher = Sha512::new();
    hasher.update(SHARE_DOMAIN);
    hasher.update(encode_point(shared));
    hasher.update([dealer, holder, position]);
    let digest = hasher.finalize();
    let mut pad = [0u8; 32];
    pad.copy_from_slice(&digest[..32]);
    pad
}

fn xor(value: &[u8; 32], pad: &[u8; 32]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (index, slot) in out.iter_mut().enumerate() {
        *slot = value[index] ^ pad[index];
    }
    out
}

/// Splits one position's key across the players.
pub fn deal_shares(
    seed: &Seed,
    dealer: u8,
    dealer_key: &Scalar,
    position: u8,
    secret: &Scalar,
    recipients: &[Recipient],
    threshold: u8,
) -> Result<SharePackage, MentalError> {
    if position as usize >= CARD_COUNT {
        return Err(MentalError::PositionOutOfRange);
    }
    if threshold < 2 || threshold as usize > recipients.len() {
        return Err(MentalError::BadThreshold);
    }

    let generator = basepoint();
    // f(x) = secret + a_1 x + ... + a_{t-1} x^(t-1)
    let mut coefficients = vec![*secret];
    for degree in 1..threshold {
        let index = position as u32 * 64 + degree as u32;
        coefficients.push(seed.scalar("vss-coefficient", index));
    }
    let commitments = coefficients
        .iter()
        .map(|coefficient| encode_point(&(generator * coefficient)))
        .collect();

    let mut shares = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        let point = evaluation_point(recipient.player);
        // Horner, so the share is one pass over the coefficients.
        let mut value = Scalar::ZERO;
        for coefficient in coefficients.iter().rev() {
            value = value * point + coefficient;
        }

        let public = decode_point(&recipient.public_key)?;
        let shared = public * dealer_key;
        let pad = share_pad(&shared, dealer, recipient.player, position);
        shares.push(EncryptedShare {
            holder: recipient.player,
            ciphertext: xor(&encode_scalar(&value), &pad),
        });
    }

    Ok(SharePackage { dealer, position, commitments, shares })
}

/// What the package's commitments say the shared key is: `k * G`.
pub fn shared_key_commitment(package: &SharePackage) -> Result<RistrettoPoint, MentalError> {
    let first = package.commitments.first().ok_or(MentalError::BadShare)?;
    decode_point(first)
}

/// What a holder's share must equal, from the public commitments alone.
fn expected_commitment(
    package: &SharePackage,
    holder: u8,
) -> Result<RistrettoPoint, MentalError> {
    let point = evaluation_point(holder);
    let mut total = RistrettoPoint::default();
    let mut power = Scalar::ONE;
    for commitment in &package.commitments {
        total += decode_point(commitment)? * power;
        power *= point;
    }
    Ok(total)
}

/// Checks a share somebody else claims to hold, against the public commitments.
///
/// This is what makes a recovered board key safe to rebuild from shares passed
/// between players rather than decrypted from the package. A holder can only
/// open its *own* share ([`open_share`] needs their identity key), so rebuilding
/// an absent player's key means the present players telling each other what they
/// hold — and a forged contribution would push [`recover`] onto a different
/// polynomial, producing a key that opens nothing and killing the hand it was
/// meant to rescue.
///
/// Feldman is what closes that: the dealer's coefficients are committed in the
/// clear inside their own lock round, whose digest is on chain, so `s * G` must
/// equal the commitments evaluated at the holder's point. A contribution that
/// fails this is a lie, and naming it is the difference between a stalled hand
/// and a named cheat.
pub fn verify_share(
    package: &SharePackage,
    holder: u8,
    value: &Scalar,
) -> Result<(), MentalError> {
    if value == &Scalar::ZERO {
        return Err(MentalError::ZeroScalar);
    }
    if basepoint() * value != expected_commitment(package, holder)? {
        return Err(MentalError::BadShare);
    }
    Ok(())
}

/// Opens the share addressed to `holder` and checks it against the commitments.
///
/// A tampered ciphertext, a share dealt to somebody else, or a dealer who split
/// a different key than they committed to all fail here rather than later.
pub fn open_share(
    package: &SharePackage,
    holder: u8,
    holder_key: &Scalar,
    dealer_public: &[u8; 32],
) -> Result<Scalar, MentalError> {
    let encrypted = package
        .shares
        .iter()
        .find(|share| share.holder == holder)
        .ok_or(MentalError::BadShare)?;

    let shared = decode_point(dealer_public)? * holder_key;
    let pad = share_pad(&shared, package.dealer, holder, package.position);
    let value = decode_scalar(&xor(&encrypted.ciphertext, &pad))
        .map_err(|_| MentalError::BadShare)?;

    verify_share(package, holder, &value)?;
    Ok(value)
}

/// Rebuilds a secret from a threshold of its shares, by Lagrange at zero.
pub fn recover(shares: &[(u8, Scalar)], threshold: u8) -> Result<Scalar, MentalError> {
    if shares.len() < threshold as usize {
        return Err(MentalError::NotEnoughShares);
    }
    let points: Vec<Scalar> = shares
        .iter()
        .map(|(holder, _)| evaluation_point(*holder))
        .collect();
    for (index, point) in points.iter().enumerate() {
        if points[index + 1..].contains(point) {
            return Err(MentalError::DuplicateShare);
        }
    }

    let mut secret = Scalar::ZERO;
    for (index, (_, value)) in shares.iter().enumerate() {
        let mut coefficient = Scalar::ONE;
        for (other, point) in points.iter().enumerate() {
            if other == index {
                continue;
            }
            let difference = point - points[index];
            // Distinct evaluation points, so this never inverts zero.
            coefficient *= point * difference.invert();
        }
        secret += coefficient * value;
    }
    Ok(secret)
}
