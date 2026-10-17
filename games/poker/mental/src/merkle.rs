//! A binary Merkle tree over one lock round's per-position key commitments.
//!
//! It exists so the chain can check a single unlock share without holding the
//! round it came from. A share is correct exactly when `share * (k * G) == G`,
//! and `k * G` is already published, per position, in the author's lock round
//! (`LockRound::keys`). What the chain keeps of that round is one 32-byte
//! commitment, and one hash cannot be opened for one position — so the
//! commitment becomes a root instead of a digest, and a publisher supplies the
//! key and its path alongside the share.
//!
//! Without it a forged share against somebody else's hole card is accepted in
//! silence: that slot is not checked until its owner adds the last layer at
//! showdown, and it is then the owner whose transaction fails and the owner who
//! is folded for declining to show.
//!
//! **SHA-256, not the SHA-512 the rest of the protocol uses.** SHA-256 is a
//! Solana syscall and SHA-512 is not, so this is the one hash the program can
//! afford to run per level. The two are never mixed: nothing here feeds a
//! transcript, and nothing in a transcript feeds this.

use sha2::{Digest, Sha256};

/// Domain separators, so a leaf can never be read as an interior node.
const LEAF_DOMAIN: &[u8] = b"riverproof/mental/merkle-leaf/v1";
const NODE_DOMAIN: &[u8] = b"riverproof/mental/merkle-node/v1";
/// The commitment that goes on chain, binding the round to its key tree.
const COMMIT_DOMAIN: &[u8] = b"riverproof/mental/lock-commit/v1";

/// The deepest tree this ever builds: 52 positions padded to 64.
pub const MAX_DEPTH: usize = 6;

/// A leaf, bound to its own index.
///
/// The index is absorbed so a path cannot be replayed at a different position —
/// without it, two positions holding the same key would have interchangeable
/// proofs, and a publisher could open position 7 with position 9's key.
pub fn leaf(index: u32, value: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(LEAF_DOMAIN);
    hasher.update(index.to_le_bytes());
    hasher.update(value);
    hasher.finalize().into()
}

/// An interior node.
pub fn node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(NODE_DOMAIN);
    hasher.update(left);
    hasher.update(right);
    hasher.finalize().into()
}

/// Rounds `count` up to a power of two, which is the width the tree is built at.
fn width(count: usize) -> usize {
    let mut width = 1usize;
    while width < count {
        width *= 2;
    }
    width
}

/// Every level of the tree, leaves first.
///
/// Missing leaves are the zero value rather than a copy of their neighbour. A
/// duplicated last leaf is the classic Merkle malleability, and it costs
/// nothing to avoid: the index is already in the leaf hash, so a zero leaf at
/// index 61 hashes to something no real key can collide with.
fn levels(values: &[[u8; 32]]) -> Vec<Vec<[u8; 32]>> {
    let width = width(values.len().max(1));
    let mut level: Vec<[u8; 32]> = (0..width)
        .map(|index| leaf(index as u32, values.get(index).unwrap_or(&[0u8; 32])))
        .collect();

    let mut out = vec![level.clone()];
    while level.len() > 1 {
        level = level.chunks(2).map(|pair| node(&pair[0], &pair[1])).collect();
        out.push(level.clone());
    }
    out
}

/// The root over `values`, indexed by position.
pub fn root(values: &[[u8; 32]]) -> [u8; 32] {
    *levels(values).last().and_then(|top| top.first()).unwrap_or(&[0u8; 32])
}

/// The sibling at each level, bottom up, for one index.
pub fn path(values: &[[u8; 32]], index: usize) -> Vec<[u8; 32]> {
    let all = levels(values);
    let mut out = Vec::with_capacity(all.len().saturating_sub(1));
    let mut cursor = index;
    for level in all.iter().take(all.len().saturating_sub(1)) {
        out.push(level[cursor ^ 1]);
        cursor /= 2;
    }
    out
}

/// Folds a leaf and its path back to a root.
///
/// The same walk the program runs, kept here so both sides are one function
/// apart rather than two implementations.
pub fn fold(value: &[u8; 32], index: usize, siblings: &[[u8; 32]]) -> [u8; 32] {
    let mut hash = leaf(index as u32, value);
    let mut cursor = index;
    for sibling in siblings {
        hash = if cursor % 2 == 0 { node(&hash, sibling) } else { node(sibling, &hash) };
        cursor /= 2;
    }
    hash
}

/// What the author commits on chain for a lock round.
///
/// Binds the round's own digest to the tree over its keys, so the commitment
/// still pins the whole transcript — a client re-derives both halves — while
/// remaining openable at one position by the program.
pub fn lock_commitment(round_digest: &[u8; 32], keys_root: &[u8; 32]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(COMMIT_DOMAIN);
    hasher.update(round_digest);
    hasher.update(keys_root);
    hasher.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(count: usize) -> Vec<[u8; 32]> {
        (0..count)
            .map(|index| {
                let mut value = [0u8; 32];
                value[0] = index as u8;
                value[1] = 0xa5;
                value
            })
            .collect()
    }

    #[test]
    fn every_position_opens_against_the_root() {
        for count in [1usize, 2, 3, 25, 52, 64] {
            let values = values(count);
            let root = root(&values);
            for index in 0..count {
                let path = path(&values, index);
                assert_eq!(
                    fold(&values[index], index, &path),
                    root,
                    "{count} values: position {index} did not open",
                );
            }
        }
    }

    #[test]
    fn a_path_cannot_be_replayed_at_another_position() {
        // Two positions holding the same key. Without the index in the leaf
        // their proofs would be interchangeable.
        let mut values = values(52);
        values[9] = values[7];
        let root = root(&values);
        let path = path(&values, 7);
        assert_eq!(fold(&values[7], 7, &path), root);
        assert_ne!(
            fold(&values[9], 9, &path),
            root,
            "position 7's path opened position 9",
        );
    }

    #[test]
    fn a_tampered_key_or_sibling_does_not_open() {
        let values = values(52);
        let root = root(&values);
        let path = path(&values, 13);

        let mut wrong = values[13];
        wrong[31] ^= 1;
        assert_ne!(fold(&wrong, 13, &path), root, "a changed key still opened");

        let mut siblings = path.clone();
        siblings[2][0] ^= 1;
        assert_ne!(fold(&values[13], 13, &siblings), root, "a changed path still opened");
    }

    #[test]
    fn the_depth_a_full_deck_needs_is_the_one_the_program_budgets_for() {
        assert_eq!(path(&values(52), 0).len(), MAX_DEPTH);
    }

    #[test]
    fn padding_is_a_zero_leaf_rather_than_a_duplicate() {
        // 3 real values pad to 4. If the pad copied its neighbour, swapping
        // the last real value would leave the root unchanged for one of them.
        let three = values(3);
        let mut four = three.clone();
        four.push([0u8; 32]);
        assert_eq!(root(&three), root(&four), "the pad is not the zero value");
    }
}
