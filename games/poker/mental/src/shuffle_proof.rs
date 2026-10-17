//! The zero-knowledge argument that a shuffle round really shuffled.
//!
//! [`crate::shuffle::verify_shuffle`] can see that a round's fifty-two points
//! are distinct and decode. On its own that says nothing about where they came
//! from: a player who discards the deck they were handed and publishes fifty-two
//! cards of their own choosing produces something indistinguishable from an
//! honest round, and [`crate::shuffle::audit`] only catches it afterwards, at
//! the cost of the hand's privacy. This module is what catches it at publish
//! time instead.
//!
//! # The statement
//!
//! `shuffle()` computes `D_j = s * C_π(j)` — **one scalar for the whole deck**,
//! not one per card. So the statement is a *common-exponent* shuffle:
//!
//! > given the deck `C` this player was handed, the deck `D` they published,
//! > and `S = s·G` already published as `ShuffleRound::mask`, there exist a
//! > scalar `s` and a permutation `π` with `D_j = s·C_π(j)` for every `j`.
//!
//! That is strictly weaker than the re-encryption shuffle Bayer-Groth,
//! Furukawa-Sako and Groth-Lu prove, which needs a fresh randomiser per
//! ciphertext: 52 group elements here instead of 104, and one secret scalar
//! instead of 53.
//!
//! Binding `S` into the statement is what closes the chain of custody.
//! [`crate::lock::verify_lock`] already proves "I stripped exactly the `s` whose
//! `s·G` is `S`". Without `S` in here a cheat could prove a permutation under
//! one scalar and lock under another, and both proofs would pass.
//!
//! # The argument
//!
//! A Terelius-Wikström permutation-matrix argument, specialised to the common
//! exponent. Neff's Simple k-Shuffle proves this exact statement and does not
//! transplant — its prover needs the ratio of the two vectors at each presented
//! position, and for a deck the only known ratio is `s` under the secret
//! pairing, which is precisely what must stay hidden. `docs/SHUFFLE_PROOF.md`
//! records why.
//!
//! Three generators, all hashed into the group under a fresh domain separator
//! exactly as [`crate::group::card_point`] is, so nobody knows their relative
//! discrete logarithms and there is no trusted setup:
//!
//! * `slots[i]`, one per deck position, which a permutation commitment selects
//!   from;
//! * `blind`, the randomiser of that commitment, and the base of the bridging
//!   chain;
//! * `bridge`, the randomiser of the bridging chain.
//!
//! The prover publishes `c_j = r_j·blind + slots[π(j)]`, one hiding commitment
//! per output position — statistically hiding for a uniform `r_j`, and in
//! practice hiding under the seed PRF that draws it. The verifier draws weights `e_1..e_52` over
//! everything published so far and forms `Com = Σ e_j·c_j` himself, which
//! commits to the vector `b` with `b_π(j) = e_j`. Four things are then proved
//! under a single shared challenge, which is what stops the parts being mixed
//! and matched:
//!
//! 1. `Σ_j c_j - Σ_i slots[i]` is a multiple of `blind` — every row of the
//!    committed matrix sums to one.
//! 2. `∏_i b_i = ∏_j e_j`, by the bridging chain below.
//! 3. `Σ_i b_i·C_i = w·T` where `T = Σ_j e_j·D_j`, linking the committed
//!    permutation to the actual decks.
//! 4. `w·S = G`, which pins `w` to `s⁻¹` for the `s` the round already
//!    committed to.
//!
//! (1) and (2) are Terelius-Wikström's characterisation: a matrix whose rows
//! sum to one and which satisfies the product identity for a random `e` is a
//! permutation matrix. (3) and (4) then give
//! `Σ_j e_j·(s·C_π(j) − D_j) = 0`, and because `e` was drawn after `C`, `D`,
//! `S` and `c` were fixed, every term is zero.
//!
//! # The bridging chain
//!
//! Proving `∏_i b_i = P` without revealing `b` is the whole cost of the
//! argument. The chain carries the running product in the exponent of `blind`:
//!
//! ```text
//! B_-1 = blind
//! B_i  = γ_i·bridge + b_i·B_i-1        so  B_i = δ_i·bridge + (∏_{k≤i} b_k)·blind
//! ```
//!
//! The last blinding is not drawn but *forced* — `γ_51 = −b_51·δ_50` — which
//! makes `δ_51` zero and the final bridge exactly `P·blind`. The verifier
//! computes that one for himself, so only fifty-one travel, and the chain
//! cannot terminate anywhere else without a known relation between `blind` and
//! `bridge`.
//!
//! # What is never published
//!
//! `F = Σ_i b_i·C_i` is not on the wire, and that is deliberate rather than
//! incidental. `F` is a checkable function of the permutation: anyone holding it
//! can test a candidate `π` by recomputing `Σ_j e_j·C_π(j)` and comparing, which
//! turns recovering the ordering from a decision problem nobody can solve into a
//! search nobody can afford. Writing the link against `w = s⁻¹` instead of
//! against `F` keeps the argument honest-verifier zero-knowledge: every value on
//! the wire is either perfectly hiding or reconstructible by a simulator that
//! picks the responses first.
//!
//! # Blinding
//!
//! Every blinding and every nonce is drawn with [`Transcript::nonce`], which
//! folds the statement into the seed. Drawing them from the seed alone would be
//! a leak rather than an inefficiency: the permutation commitment and the
//! bridging chain are both linear in the value they hide, so one player
//! publishing two rounds over different decks under one hand seed would have
//! the blindings cancel between them and the ordering fall out in the clear.
//!
//! # Form
//!
//! Challenge-and-response, like [`crate::proof`]: the prover's nonce
//! commitments are not transmitted, the verifier rebuilds them from the
//! responses, and the proof is valid exactly when the challenge it carries is
//! the one its own commitments produce. 103 points and 108 scalars, 6,752 bytes.

use curve25519_dalek::ristretto::RistrettoPoint;
use curve25519_dalek::scalar::Scalar;
use std::sync::OnceLock;

use crate::group::{
    basepoint, decode_point, decode_scalar, encode_point, encode_scalar, multiscalar, Seed,
    Transcript, CARD_COUNT,
};
use crate::MentalError;

/// Bridging commitments on the wire. The fifty-second is `(∏ e_j)·blind`, which
/// the verifier computes rather than reads.
pub const BRIDGE_COUNT: usize = CARD_COUNT - 1;

const GENERATOR_DOMAIN: &[u8] = b"riverproof/mental/shuffle-gen/v1";

/// The generators the argument commits against.
///
/// Derived by hashing into the group, never as `d·G`, for the same reason
/// [`crate::group::card_point`] is: a prover who knew a relation between any two
/// of these could open a permutation commitment two ways and prove a shuffle
/// they did not perform. Hashing leaves nobody holding one, and leaves nothing
/// to trust in a setup.
pub struct Generators {
    /// The randomiser of the permutation commitment, and the base of the
    /// bridging chain.
    pub blind: RistrettoPoint,
    /// The randomiser of the bridging chain.
    pub bridge: RistrettoPoint,
    /// One per deck position. A permutation commitment picks one of these.
    pub slots: Vec<RistrettoPoint>,
    /// `Σ_i slots[i]`, which every verification needs and none should re-add.
    pub slot_sum: RistrettoPoint,
}

fn derive_generator(kind: u8, index: u8) -> RistrettoPoint {
    use sha2::{Digest, Sha512};
    let mut hasher = Sha512::new();
    hasher.update(GENERATOR_DOMAIN);
    hasher.update([kind, index]);
    RistrettoPoint::from_uniform_bytes(&hasher.finalize().into())
}

/// The generators, derived once.
pub fn generators() -> &'static Generators {
    static GENERATORS: OnceLock<Generators> = OnceLock::new();
    GENERATORS.get_or_init(|| {
        let slots: Vec<RistrettoPoint> = (0..CARD_COUNT)
            .map(|index| derive_generator(2, index as u8))
            .collect();
        let slot_sum = slots.iter().sum();
        Generators {
            blind: derive_generator(0, 0),
            bridge: derive_generator(1, 0),
            slots,
            slot_sum,
        }
    })
}

/// What a shuffler publishes to show their round was a shuffle.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ShuffleProof {
    /// `c_j = r_j·blind + slots[π(j)]`, one per output position.
    pub permutation: Vec<[u8; 32]>,
    /// `B_0..B_50`. The chain's last link is public and is not sent.
    pub bridges: Vec<[u8; 32]>,
    pub challenge: [u8; 32],
    /// One per position, for the weight committed at that position.
    pub responses: Vec<[u8; 32]>,
    /// One per bridging step.
    pub bridge_responses: Vec<[u8; 32]>,
    /// For the permutation commitment's randomiser.
    pub blind_response: [u8; 32],
    /// For the randomiser of `Σ_j c_j`, which is the row-sum check.
    pub ones_response: [u8; 32],
    /// For `s⁻¹`, which links the argument to the mask the round committed to.
    pub mask_response: [u8; 32],
}

/// The transcript both sides build, up to but not including the permutation
/// commitment.
///
/// Everything the statement depends on is absorbed here: whose round it is,
/// their key-exchange key, the deck handed in, the mask committed to, and the
/// deck published.
///
/// `identity` is in there to bind the proof to one hand. The first shuffler's
/// input deck is [`crate::group::ordered_deck`] — a global constant — so
/// without it, round zero of any finished deal at any table would verify
/// unchanged inside a different one, and a proof that travels is a proof that
/// attributes nothing. `Participant`'s identity key comes from the per-hand
/// seed ([`Seed::for_hand`]), so absorbing it ties the statement to that seed:
/// a round cannot be replayed by another player, and cannot be replayed by its
/// own author outside the hand it was made for. It also stops the key itself
/// being swapped out, which matters because share packages are encrypted to it.
///
/// It stops short of the commitment because the prover needs a transcript state
/// to draw the commitment's own blinding from, and that state has to be one the
/// verifier never reaches for — see [`prove_shuffle`].
fn statement(
    player: u8,
    identity: &[u8; 32],
    input: &[RistrettoPoint],
    output: &[RistrettoPoint],
    mask_commitment: &RistrettoPoint,
) -> Transcript {
    let mut transcript = Transcript::new("shuffle-proof");
    transcript.absorb_u8("player", player);
    transcript.absorb("identity", identity);
    transcript.absorb_points("input", input);
    transcript.absorb_point("mask", mask_commitment);
    transcript.absorb_points("output", output);
    transcript
}

/// Proves that `output` is `input` permuted by `order` and scaled by `mask`.
///
/// `order[j]` is the input position that ends up at output position `j`, which
/// is [`crate::shuffle::ShuffleSecret`]'s convention and the one the commitment
/// below selects a slot generator with.
pub fn prove_shuffle(
    seed: &Seed,
    player: u8,
    identity: &[u8; 32],
    input: &[RistrettoPoint],
    output: &[RistrettoPoint],
    mask_commitment: &RistrettoPoint,
    mask: &Scalar,
    order: &[u8],
) -> Result<ShuffleProof, MentalError> {
    if input.len() != CARD_COUNT || output.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }
    crate::shuffle::valid_order(order, CARD_COUNT)?;

    let generators = generators();
    let mut transcript = statement(player, identity, input, output, mask_commitment);

    // Every blinding in this proof is drawn from the transcript rather than
    // from the seed alone, and that is load-bearing rather than stylistic.
    // `Seed::scalar` is a pure function of the seed and a label, so two rounds
    // by one player under one hand seed would carry the *same* blindings — and
    // both the commitment and the bridging chain are linear in the secret they
    // hide, so subtracting one round from the other cancels the blinding and
    // publishes the ordering outright. `Transcript::nonce` folds the statement
    // in, so two rounds over different decks cannot share one. It is the same
    // discipline `prove_same_scalar` and `prove_per_index` already follow.
    let blinds: Vec<Scalar> = (0..CARD_COUNT)
        .map(|position| transcript.nonce(seed, "permutation-blind", position as u32))
        .collect();
    let permutation: Vec<RistrettoPoint> = order
        .iter()
        .zip(&blinds)
        .map(|(position, blind)| {
            generators.blind * blind + generators.slots[*position as usize]
        })
        .collect();

    transcript.absorb_points("permutation", &permutation);
    let weights = transcript.challenge_vector("weights", CARD_COUNT);

    // `committed[i]` is the weight the permutation carries to position `i`:
    // what `Σ e_j·c_j` commits to, without the verifier learning the ordering.
    let mut committed = vec![Scalar::ZERO; CARD_COUNT];
    for (index, position) in order.iter().enumerate() {
        committed[*position as usize] = weights[index];
    }

    let weighted_blind: Scalar = weights
        .iter()
        .zip(&blinds)
        .map(|(weight, blind)| weight * blind)
        .sum();
    let ones_blind: Scalar = blinds.iter().sum();
    let product = weights.iter().fold(Scalar::ONE, |running, weight| running * weight);
    let inverse_mask = mask.invert();

    // The bridging chain: the running product of the committed weights, carried
    // in the exponent of `blind`.
    let mut bridges: Vec<RistrettoPoint> = Vec::with_capacity(BRIDGE_COUNT);
    let mut bridge_blinds: Vec<Scalar> = Vec::with_capacity(CARD_COUNT);
    let mut carried = Scalar::ZERO;
    let mut previous = generators.blind;
    for position in 0..BRIDGE_COUNT {
        let blind = transcript.nonce(seed, "bridge-blind", position as u32);
        let bridge = generators.bridge * blind + previous * committed[position];
        carried = blind + committed[position] * carried;
        bridge_blinds.push(blind);
        bridges.push(bridge);
        previous = bridge;
    }
    // Forced rather than drawn, so the chain lands exactly on `product·blind`
    // and the verifier can compute its last link instead of being told it.
    bridge_blinds.push(-(committed[CARD_COUNT - 1] * carried));

    let mut chain = bridges.clone();
    chain.push(generators.blind * product);

    transcript.absorb_points("bridges", &bridges);

    // Deterministic in the manner of RFC 6979, and drawn at one transcript
    // state so no two statements can share a nonce.
    //
    // Every family below has its own label, and that is not tidiness. All 107
    // are drawn at the same state, so two families sharing one would hand out
    // the same nonce twice — and subtracting the two responses cancels it and
    // leaves the challenge times a difference of secrets, which for `b` is a
    // difference of two *public* weights and therefore names two positions of
    // the ordering outright.
    let nonces: Vec<Scalar> = (0..CARD_COUNT)
        .map(|index| transcript.nonce(seed, "nonce-weight", index as u32))
        .collect();
    let bridge_nonces: Vec<Scalar> = (0..CARD_COUNT)
        .map(|index| transcript.nonce(seed, "nonce-bridge", index as u32))
        .collect();
    let blind_nonce = transcript.nonce(seed, "nonce-permutation", 0);
    let ones_nonce = transcript.nonce(seed, "nonce-ones", 0);
    let mask_nonce = transcript.nonce(seed, "nonce-mask", 0);

    // Nonce commitments. Written one point at a time rather than through
    // `multiscalar`, which is variable time and has no business touching a
    // nonce.
    let commitment_nonce = generators.blind * blind_nonce
        + generators
            .slots
            .iter()
            .zip(&nonces)
            .map(|(slot, nonce)| slot * nonce)
            .sum::<RistrettoPoint>();
    let bridge_commitments: Vec<RistrettoPoint> = (0..CARD_COUNT)
        .map(|position| {
            let base = if position == 0 { generators.blind } else { chain[position - 1] };
            generators.bridge * bridge_nonces[position] + base * nonces[position]
        })
        .collect();
    let ones_commitment = generators.blind * ones_nonce;
    let weighted_output: RistrettoPoint = output
        .iter()
        .zip(&weights)
        .map(|(card, weight)| card * weight)
        .sum();
    let link_commitment = input
        .iter()
        .zip(&nonces)
        .map(|(card, nonce)| card * nonce)
        .sum::<RistrettoPoint>()
        - weighted_output * mask_nonce;
    let mask_check = mask_commitment * mask_nonce;

    transcript.absorb_point("nonce-commitment", &commitment_nonce);
    transcript.absorb_points("nonce-bridges", &bridge_commitments);
    transcript.absorb_point("nonce-ones", &ones_commitment);
    transcript.absorb_point("nonce-link", &link_commitment);
    transcript.absorb_point("nonce-mask", &mask_check);

    let challenge = transcript.challenge("shuffle-proof");
    Ok(ShuffleProof {
        permutation: permutation.iter().map(encode_point).collect(),
        bridges: bridges.iter().map(encode_point).collect(),
        challenge: encode_scalar(&challenge),
        responses: nonces
            .iter()
            .zip(&committed)
            .map(|(nonce, weight)| encode_scalar(&(nonce + challenge * weight)))
            .collect(),
        bridge_responses: bridge_nonces
            .iter()
            .zip(&bridge_blinds)
            .map(|(nonce, blind)| encode_scalar(&(nonce + challenge * blind)))
            .collect(),
        blind_response: encode_scalar(&(blind_nonce + challenge * weighted_blind)),
        ones_response: encode_scalar(&(ones_nonce + challenge * ones_blind)),
        mask_response: encode_scalar(&(mask_nonce + challenge * inverse_mask)),
    })
}

/// Checks a [`ShuffleProof`] against the round it claims to be about.
///
/// Everything here is public, so verification uses [`multiscalar`] freely; the
/// prover does not.
pub fn verify_shuffle_proof(
    player: u8,
    identity: &[u8; 32],
    input: &[RistrettoPoint],
    output: &[RistrettoPoint],
    mask_commitment: &RistrettoPoint,
    proof: &ShuffleProof,
) -> Result<(), MentalError> {
    if input.len() != CARD_COUNT || output.len() != CARD_COUNT {
        return Err(MentalError::WrongDeckSize);
    }
    if proof.permutation.len() != CARD_COUNT
        || proof.bridges.len() != BRIDGE_COUNT
        || proof.responses.len() != CARD_COUNT
        || proof.bridge_responses.len() != CARD_COUNT
    {
        return Err(MentalError::BadProof);
    }

    let generators = generators();
    let permutation: Vec<RistrettoPoint> =
        proof.permutation.iter().map(decode_point).collect::<Result<_, _>>()?;
    let bridges: Vec<RistrettoPoint> =
        proof.bridges.iter().map(decode_point).collect::<Result<_, _>>()?;

    let scalar = |bytes: &[u8; 32]| decode_scalar(bytes).map_err(|_| MentalError::BadProof);
    let challenge = scalar(&proof.challenge)?;
    let responses: Vec<Scalar> =
        proof.responses.iter().map(scalar).collect::<Result<_, _>>()?;
    let bridge_responses: Vec<Scalar> =
        proof.bridge_responses.iter().map(scalar).collect::<Result<_, _>>()?;
    let blind_response = scalar(&proof.blind_response)?;
    let ones_response = scalar(&proof.ones_response)?;
    let mask_response = scalar(&proof.mask_response)?;

    let mut transcript = statement(player, identity, input, output, mask_commitment);
    transcript.absorb_points("permutation", &permutation);
    let weights = transcript.challenge_vector("weights", CARD_COUNT);

    let commitment = multiscalar(&permutation, &weights);
    // Every row of the committed matrix sums to one exactly when this is a
    // multiple of `blind`, which `ones_response` is the knowledge of.
    let ones = permutation.iter().sum::<RistrettoPoint>() - generators.slot_sum;
    let product = weights.iter().fold(Scalar::ONE, |running, weight| running * weight);
    let weighted_output = multiscalar(output, &weights);

    let mut chain = bridges.clone();
    chain.push(generators.blind * product);

    transcript.absorb_points("bridges", &bridges);

    let commitment_nonce = generators.blind * blind_response
        + multiscalar(&generators.slots, &responses)
        - commitment * challenge;
    let bridge_commitments: Vec<RistrettoPoint> = (0..CARD_COUNT)
        .map(|position| {
            let base = if position == 0 { generators.blind } else { chain[position - 1] };
            generators.bridge * bridge_responses[position] + base * responses[position]
                - chain[position] * challenge
        })
        .collect();
    let ones_commitment = generators.blind * ones_response - ones * challenge;
    // The linking claim is that this difference is zero, so unlike the others
    // it carries no multiple of the challenge.
    let link_commitment = multiscalar(input, &responses) - weighted_output * mask_response;
    let mask_check = mask_commitment * mask_response - basepoint() * challenge;

    transcript.absorb_point("nonce-commitment", &commitment_nonce);
    transcript.absorb_points("nonce-bridges", &bridge_commitments);
    transcript.absorb_point("nonce-ones", &ones_commitment);
    transcript.absorb_point("nonce-link", &link_commitment);
    transcript.absorb_point("nonce-mask", &mask_check);

    if transcript.challenge("shuffle-proof") == challenge {
        Ok(())
    } else {
        Err(MentalError::BadProof)
    }
}
