//! Chaum-Pedersen proofs of equal discrete logarithm.
//!
//! Two shapes, both the same sigma protocol.
//!
//! [`SameScalarProof`] proves that one secret exponent relates many base/image
//! pairs at once: a player stripping their shuffle mask off fifty-two cards
//! uses the same `s` on every one of them, and one proof says so.
//!
//! [`PerIndexProof`] proves fifty-two statements with fifty-two *different*
//! exponents under a single shared challenge: a player locking each position
//! with its own key needs a separate response per position, but the challenge
//! binds them together, so the proof cannot be assembled a statement at a time.
//!
//! Both are transmitted in challenge-and-response form. The verifier rebuilds
//! the prover's commitments from the response and re-derives the challenge; a
//! proof is valid exactly when the challenge it carries is the one its own
//! commitments produce. Both `prove` and `verify` advance the transcript
//! identically, so proofs chain without either side having to remember to.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use curve25519_dalek::traits::Identity;

use crate::group::{basepoint, decode_scalar, encode_scalar, Seed, Transcript};
use crate::MentalError;

/// One exponent, many base/image pairs.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SameScalarProof {
    pub challenge: [u8; 32],
    pub response: [u8; 32],
}

/// Proves `images[m] == secret * bases[m]` for every `m`.
pub fn prove_same_scalar(
    transcript: &mut Transcript,
    seed: &Seed,
    secret: &Scalar,
    bases: &[RistrettoPoint],
    images: &[RistrettoPoint],
) -> SameScalarProof {
    transcript.absorb_points("bases", bases);
    transcript.absorb_points("images", images);

    let nonce = transcript.nonce(seed, "same-scalar", 0);
    let commitments: Vec<RistrettoPoint> = bases.iter().map(|base| base * nonce).collect();
    transcript.absorb_points("commitments", &commitments);

    let challenge = transcript.challenge("same-scalar");
    SameScalarProof {
        challenge: encode_scalar(&challenge),
        response: encode_scalar(&(nonce + challenge * secret)),
    }
}

/// Checks a [`SameScalarProof`] against the statement it claims.
pub fn verify_same_scalar(
    transcript: &mut Transcript,
    bases: &[RistrettoPoint],
    images: &[RistrettoPoint],
    proof: &SameScalarProof,
) -> Result<(), MentalError> {
    if bases.len() != images.len() || bases.is_empty() {
        return Err(MentalError::BadProof);
    }
    // An identity base makes the relation hold for every exponent, so a proof
    // over one proves nothing.
    if bases.iter().any(|base| *base == RistrettoPoint::identity()) {
        return Err(MentalError::IdentityElement);
    }

    let challenge = decode_scalar(&proof.challenge).map_err(|_| MentalError::BadProof)?;
    let response = decode_scalar(&proof.response).map_err(|_| MentalError::BadProof)?;

    transcript.absorb_points("bases", bases);
    transcript.absorb_points("images", images);

    let commitments: Vec<RistrettoPoint> = bases
        .iter()
        .zip(images)
        .map(|(base, image)| base * response - image * challenge)
        .collect();
    transcript.absorb_points("commitments", &commitments);

    if transcript.challenge("same-scalar") == challenge {
        Ok(())
    } else {
        Err(MentalError::BadProof)
    }
}

/// One exponent per position, all under a single challenge.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PerIndexProof {
    pub challenge: [u8; 32],
    pub responses: Vec<[u8; 32]>,
}

/// Proves, for every `j`, that one secret `k_j` satisfies both
/// `commitments[j] == k_j * G` and `images[j] == k_j * bases[j]`.
pub fn prove_per_index(
    transcript: &mut Transcript,
    seed: &Seed,
    secrets: &[Scalar],
    bases: &[RistrettoPoint],
    commitments: &[RistrettoPoint],
    images: &[RistrettoPoint],
) -> PerIndexProof {
    let generator = basepoint();
    transcript.absorb_points("bases", bases);
    transcript.absorb_points("commitments", commitments);
    transcript.absorb_points("images", images);

    let nonces: Vec<Scalar> = (0..secrets.len())
        .map(|index| transcript.nonce(seed, "per-index", index as u32))
        .collect();
    let generator_commitments: Vec<RistrettoPoint> =
        nonces.iter().map(|nonce| generator * nonce).collect();
    let base_commitments: Vec<RistrettoPoint> = nonces
        .iter()
        .zip(bases)
        .map(|(nonce, base)| base * nonce)
        .collect();
    transcript.absorb_points("nonce-generator", &generator_commitments);
    transcript.absorb_points("nonce-bases", &base_commitments);

    let challenge = transcript.challenge("per-index");
    PerIndexProof {
        challenge: encode_scalar(&challenge),
        responses: nonces
            .iter()
            .zip(secrets)
            .map(|(nonce, secret)| encode_scalar(&(nonce + challenge * secret)))
            .collect(),
    }
}

/// Checks a [`PerIndexProof`] against the statements it claims.
pub fn verify_per_index(
    transcript: &mut Transcript,
    bases: &[RistrettoPoint],
    commitments: &[RistrettoPoint],
    images: &[RistrettoPoint],
    proof: &PerIndexProof,
) -> Result<(), MentalError> {
    let count = bases.len();
    if count == 0
        || commitments.len() != count
        || images.len() != count
        || proof.responses.len() != count
    {
        return Err(MentalError::BadProof);
    }
    if bases.iter().any(|base| *base == RistrettoPoint::identity()) {
        return Err(MentalError::IdentityElement);
    }

    let generator = basepoint();
    let challenge = decode_scalar(&proof.challenge).map_err(|_| MentalError::BadProof)?;
    let responses: Result<Vec<Scalar>, MentalError> = proof
        .responses
        .iter()
        .map(|bytes| decode_scalar(bytes).map_err(|_| MentalError::BadProof))
        .collect();
    let responses = responses?;

    transcript.absorb_points("bases", bases);
    transcript.absorb_points("commitments", commitments);
    transcript.absorb_points("images", images);

    let generator_commitments: Vec<RistrettoPoint> = responses
        .iter()
        .zip(commitments)
        .map(|(response, commitment)| generator * response - commitment * challenge)
        .collect();
    let base_commitments: Vec<RistrettoPoint> = responses
        .iter()
        .zip(bases.iter().zip(images))
        .map(|(response, (base, image))| base * response - image * challenge)
        .collect();
    transcript.absorb_points("nonce-generator", &generator_commitments);
    transcript.absorb_points("nonce-bases", &base_commitments);

    if transcript.challenge("per-index") == challenge {
        Ok(())
    } else {
        Err(MentalError::BadProof)
    }
}
