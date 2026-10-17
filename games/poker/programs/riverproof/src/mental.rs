//! Opening a player-shuffled card, on chain.
//!
//! `program/mental` is the whole protocol; almost none of it belongs here. The
//! chain does not shuffle, does not lock, and does not check a proof — the
//! players do all of that between themselves and the chain holds a digest of
//! each round so nobody can later claim they published something else.
//!
//! What the chain does do is open cards, and it does it with one operation.
//!
//! A dealt card sits in the account as `(∏ k_i) * P`, for the card point `P`
//! and one key per player. Each player publishes the *inverse* of their key —
//! their unlock share — and the program multiplies it into the running point.
//! When every player has contributed, what is left is `P` itself, and the card
//! is whichever of the fifty-two that is. So the program is never told what a
//! card is; it works it out, and a wrong share produces a point that is no card
//! at all rather than a card of somebody's choosing.
//!
//! One `multiply_ristretto` per share, which is a syscall on chain and
//! curve25519-dalek in a native test — so the tests below exercise the same
//! arithmetic the validator will run, against deals `riverproof-mental`
//! actually dealt.

// `solana-curve25519` marks its whole surface deprecated as advance notice that
// 4.0 will gate it behind `agave-unstable-api`. The 3.x line this program is on
// has no such feature to opt into, and the syscall it wraps is not going
// anywhere, so the notice is acknowledged here rather than repeated forty times.
#![allow(deprecated)]

use anchor_lang::prelude::*;
use solana_sha256_hasher::hashv;
use solana_curve25519::ristretto::{multiply_ristretto, PodRistrettoPoint};
use solana_curve25519::scalar::PodScalar;

use crate::errors::RiverproofError;

/// Cards in a deck.
pub const CARD_COUNT: usize = 52;

/// Positions a hand ever opens: two per seat, plus five board cards.
///
/// Burns are dealt from and never opened, so they are not tracked. Ten seats is
/// the table maximum, which makes twenty-five the most slots a hand can have.
pub const MAX_SLOTS: usize = riverproof_engine::MAX_SEATS * 2 + 5;

/// A slot that has not resolved to a card yet.
pub const NO_CARD: u8 = u8::MAX;

/// The fifty-two card points, compressed.
///
/// Hashed into the group by `riverproof_mental::card_point`, and baked here
/// because the ristretto one-way map is not a syscall and running it on chain
/// for every card would cost far more than storing the answer.
///
/// `the_card_table_is_the_hashed_one` re-derives all fifty-two and compares, so
/// this is a cache of a definition rather than a magic number.
pub const CARD_POINTS: [[u8; 32]; CARD_COUNT] = [
    // 2C
    [
        0x02, 0xf1, 0xb1, 0xcc, 0xaa, 0x9a, 0xb2, 0x7d,
        0x8f, 0x1e, 0x2e, 0xff, 0x42, 0x91, 0x8c, 0x26,
        0x1e, 0x81, 0x21, 0xae, 0x6b, 0x30, 0x2f, 0xbb,
        0x09, 0xa1, 0xb3, 0xf5, 0x94, 0x8e, 0x1f, 0x1c,
    ],
    // 3C
    [
        0x74, 0xa7, 0x0c, 0x63, 0x27, 0xda, 0xc3, 0xb9,
        0x7f, 0x4e, 0x5e, 0x1b, 0x17, 0xd6, 0xa3, 0x5e,
        0xfa, 0x43, 0x37, 0xb2, 0x68, 0x2a, 0x9e, 0x69,
        0x46, 0xa8, 0xcb, 0x6f, 0x5f, 0x8c, 0x0a, 0x01,
    ],
    // 4C
    [
        0x38, 0x24, 0x31, 0x8e, 0x3b, 0xea, 0x9d, 0x6c,
        0xef, 0x03, 0xe7, 0xf7, 0x28, 0x68, 0x1c, 0x8f,
        0x26, 0xc5, 0xcc, 0x4c, 0x4c, 0x23, 0x58, 0x50,
        0x13, 0x3b, 0x39, 0x0a, 0xd2, 0xda, 0x4c, 0x25,
    ],
    // 5C
    [
        0x0e, 0x91, 0x21, 0xac, 0xea, 0x48, 0x91, 0xef,
        0xa3, 0x2c, 0xef, 0x61, 0x5b, 0xca, 0x09, 0x92,
        0xe2, 0x8e, 0x62, 0xcb, 0x0e, 0x8a, 0x01, 0xbf,
        0xb5, 0x6e, 0xc9, 0xde, 0xfa, 0x2c, 0x95, 0x0b,
    ],
    // 6C
    [
        0x40, 0x5e, 0xc9, 0xfb, 0x9b, 0xa7, 0xcb, 0x5e,
        0x8d, 0x9a, 0x35, 0xd0, 0xae, 0xa9, 0xa8, 0x67,
        0xa6, 0x52, 0x5e, 0xf2, 0xda, 0x7c, 0x92, 0xe7,
        0xbe, 0x0a, 0x8f, 0x6e, 0x6b, 0x14, 0x74, 0x64,
    ],
    // 7C
    [
        0x32, 0x2f, 0x93, 0xc7, 0xfd, 0x3e, 0x07, 0x5b,
        0xb7, 0xee, 0x70, 0x89, 0xb2, 0x7a, 0xb4, 0xd6,
        0x5b, 0xd2, 0xcf, 0xf5, 0x6c, 0x3a, 0x09, 0x41,
        0x8b, 0x77, 0x1e, 0x63, 0x65, 0xa4, 0xcc, 0x78,
    ],
    // 8C
    [
        0x52, 0x44, 0xc5, 0x82, 0x5e, 0x41, 0xf1, 0x43,
        0x11, 0xf3, 0x58, 0xa2, 0x3b, 0x9e, 0x23, 0xc7,
        0x73, 0x71, 0xd3, 0xf5, 0x4b, 0x38, 0xdc, 0x7e,
        0x37, 0x68, 0x59, 0xfd, 0xd8, 0xcc, 0x2f, 0x2e,
    ],
    // 9C
    [
        0x6c, 0x4e, 0x1c, 0x50, 0x49, 0x3b, 0x4c, 0x5d,
        0xee, 0x10, 0x28, 0xfa, 0x81, 0x5e, 0xf6, 0x86,
        0x7a, 0x80, 0x58, 0xa9, 0xd4, 0x4a, 0x5e, 0xdb,
        0x6f, 0x6c, 0xe0, 0x2a, 0xdf, 0xc6, 0x96, 0x51,
    ],
    // TC
    [
        0x42, 0x6b, 0xb7, 0xc8, 0x09, 0x0e, 0xc7, 0x05,
        0x99, 0xca, 0xb0, 0xab, 0x61, 0xc0, 0xf0, 0x47,
        0xb7, 0xf8, 0x8a, 0xfa, 0xa3, 0x8a, 0xfe, 0xfe,
        0xf0, 0x8b, 0x06, 0xc6, 0xab, 0xf3, 0x75, 0x29,
    ],
    // JC
    [
        0x5e, 0x12, 0x08, 0xd0, 0x01, 0xd2, 0xcb, 0xbe,
        0x6a, 0x1e, 0xa4, 0xd2, 0x9f, 0x70, 0x8d, 0x0c,
        0x32, 0x96, 0x91, 0x8c, 0x40, 0x6e, 0x54, 0xce,
        0xd0, 0xad, 0x02, 0x46, 0x09, 0x03, 0x5e, 0x50,
    ],
    // QC
    [
        0xd2, 0xbc, 0x2d, 0x6a, 0x77, 0x86, 0x01, 0x80,
        0xd0, 0x24, 0xaa, 0x6f, 0x86, 0x2a, 0x59, 0x91,
        0x87, 0x45, 0x3b, 0xa1, 0xa4, 0xe7, 0x00, 0x27,
        0x6e, 0xc2, 0x5b, 0x2c, 0x40, 0x3d, 0x71, 0x0d,
    ],
    // KC
    [
        0x6e, 0xce, 0x85, 0x79, 0x39, 0x19, 0x68, 0x3c,
        0x4b, 0x86, 0xf2, 0x4e, 0x0a, 0x50, 0xc6, 0xa6,
        0x32, 0xcd, 0x75, 0x70, 0xb1, 0x3f, 0xc9, 0x2d,
        0x11, 0xfb, 0x10, 0x2a, 0x71, 0x25, 0x84, 0x5b,
    ],
    // AC
    [
        0xe4, 0x8f, 0xcf, 0x24, 0xc7, 0xa4, 0x69, 0x32,
        0xa9, 0x78, 0x9e, 0x88, 0x0e, 0x33, 0x76, 0x06,
        0x18, 0x4a, 0xde, 0x79, 0x6c, 0x19, 0xd8, 0xb1,
        0xf7, 0x63, 0xc9, 0x39, 0xd3, 0x64, 0xa8, 0x60,
    ],
    // 2D
    [
        0x7c, 0x5c, 0x4d, 0xa7, 0x57, 0x44, 0xe2, 0x0e,
        0xb2, 0x72, 0x3c, 0x3f, 0x65, 0x7d, 0xaa, 0x3a,
        0x07, 0xce, 0x5c, 0xd6, 0x27, 0xaf, 0xa6, 0x02,
        0x2d, 0xe3, 0xd2, 0x06, 0xb2, 0xc1, 0xbd, 0x70,
    ],
    // 3D
    [
        0xc8, 0xb2, 0x51, 0x3c, 0x76, 0x17, 0xf3, 0x7f,
        0xe4, 0x6c, 0x36, 0x2b, 0xbe, 0xd1, 0x20, 0xe6,
        0xbb, 0x04, 0xa0, 0x1d, 0x36, 0x56, 0x07, 0x22,
        0x2b, 0xc6, 0xb9, 0x93, 0xfb, 0x1c, 0xef, 0x72,
    ],
    // 4D
    [
        0x0a, 0x67, 0x87, 0x08, 0xc2, 0xce, 0x11, 0xe8,
        0x9b, 0x53, 0x24, 0x4f, 0x1b, 0x5f, 0xc7, 0x54,
        0x82, 0x92, 0x58, 0x76, 0x6c, 0x1e, 0x4f, 0x8c,
        0x6f, 0x50, 0x3a, 0xab, 0x34, 0xa3, 0x0b, 0x5c,
    ],
    // 5D
    [
        0x42, 0xee, 0xa1, 0xe6, 0x2a, 0xa3, 0xed, 0xcc,
        0x19, 0xb0, 0xae, 0x1c, 0xe4, 0x3b, 0xda, 0xe1,
        0x63, 0x68, 0xdc, 0x46, 0xb3, 0xec, 0xee, 0x3e,
        0xe6, 0xe0, 0x78, 0xe3, 0xf6, 0xdd, 0xde, 0x03,
    ],
    // 6D
    [
        0x04, 0x51, 0x43, 0xdc, 0x2f, 0xd7, 0xd8, 0x56,
        0xf1, 0x00, 0xf9, 0xbe, 0x38, 0xf3, 0x68, 0x5a,
        0xcb, 0x71, 0xf4, 0x5b, 0x62, 0x83, 0x54, 0x88,
        0x8e, 0x40, 0xac, 0x00, 0x86, 0xa1, 0x50, 0x5e,
    ],
    // 7D
    [
        0xf0, 0xdc, 0x98, 0xe8, 0xf9, 0xa9, 0xc9, 0x21,
        0xa1, 0xe0, 0x94, 0xce, 0xbb, 0xd8, 0x4f, 0x53,
        0xaf, 0x18, 0xe4, 0x41, 0x7b, 0x44, 0xb3, 0x74,
        0x90, 0xcd, 0x2c, 0x17, 0xf5, 0xe2, 0xa4, 0x57,
    ],
    // 8D
    [
        0xea, 0xc6, 0x40, 0xf9, 0x36, 0xd2, 0x46, 0xea,
        0xb5, 0x61, 0x89, 0xd2, 0xa9, 0xf6, 0xdc, 0xef,
        0xb6, 0xcb, 0xc5, 0x7a, 0xe1, 0x03, 0x31, 0x52,
        0xcc, 0xa5, 0x27, 0x17, 0x86, 0x68, 0xb7, 0x1c,
    ],
    // 9D
    [
        0xb6, 0xdd, 0xe4, 0xc9, 0x0c, 0xfa, 0x86, 0x11,
        0x64, 0x0c, 0x6d, 0x5c, 0xe1, 0xa5, 0xb2, 0xbb,
        0xaf, 0xce, 0x7d, 0x53, 0x02, 0x4f, 0x02, 0xfc,
        0xb4, 0x0c, 0x80, 0xa2, 0x39, 0x0a, 0x94, 0x41,
    ],
    // TD
    [
        0x94, 0x44, 0xf5, 0xfe, 0x7b, 0x0e, 0xa9, 0xcd,
        0xa1, 0xe6, 0xdd, 0x6a, 0x20, 0x1d, 0x02, 0x26,
        0x4e, 0x0f, 0x08, 0x0d, 0x5d, 0xe1, 0x71, 0x75,
        0x66, 0x6e, 0x3a, 0xf7, 0x6c, 0x28, 0xd2, 0x75,
    ],
    // JD
    [
        0x66, 0x30, 0x84, 0xc6, 0x5e, 0xdd, 0xd9, 0x64,
        0xeb, 0x42, 0xca, 0x0c, 0xe9, 0x8f, 0xb1, 0x00,
        0x8e, 0x51, 0x5d, 0xbc, 0xc1, 0x03, 0x8e, 0x31,
        0x18, 0x04, 0xee, 0xdb, 0x3a, 0x0e, 0x09, 0x1c,
    ],
    // QD
    [
        0xe0, 0x22, 0x39, 0xe3, 0xfa, 0x5d, 0x2e, 0xfb,
        0x35, 0xcb, 0xae, 0x2b, 0x24, 0xf3, 0x35, 0xf4,
        0x33, 0x48, 0x7f, 0x5b, 0x94, 0x69, 0x4d, 0x1a,
        0x7d, 0x67, 0x92, 0x94, 0x56, 0x31, 0x81, 0x0e,
    ],
    // KD
    [
        0x1e, 0x76, 0x19, 0x65, 0xfe, 0xdd, 0xa0, 0x76,
        0x01, 0xbb, 0x8b, 0x11, 0x11, 0x39, 0xe9, 0xac,
        0xf3, 0xa4, 0x3b, 0x26, 0x36, 0x18, 0xd1, 0xd7,
        0x1f, 0x37, 0xc1, 0x4c, 0xab, 0x43, 0x51, 0x1c,
    ],
    // AD
    [
        0xe4, 0x53, 0x64, 0x1c, 0x6e, 0x83, 0x96, 0x03,
        0xec, 0xb6, 0xaa, 0x5b, 0x3d, 0x96, 0xfa, 0xf2,
        0x64, 0x34, 0x22, 0x69, 0x90, 0x9a, 0x30, 0x1d,
        0xb4, 0x34, 0x68, 0xdb, 0x95, 0x10, 0xa6, 0x3f,
    ],
    // 2H
    [
        0x1a, 0xef, 0xe4, 0x3b, 0x2e, 0xf0, 0xc0, 0x80,
        0xda, 0x8a, 0x77, 0x9c, 0x12, 0xe5, 0x52, 0xa3,
        0x03, 0xf1, 0xf5, 0xea, 0xa7, 0x16, 0xb1, 0x7f,
        0x2c, 0x01, 0xf9, 0xb5, 0xc1, 0x70, 0x5b, 0x2e,
    ],
    // 3H
    [
        0xda, 0x1a, 0xa7, 0x7c, 0xf2, 0x2c, 0x07, 0x11,
        0x0a, 0x16, 0x5a, 0xc8, 0x2f, 0x61, 0x93, 0xd2,
        0x5b, 0xee, 0x03, 0x42, 0xed, 0x65, 0xf1, 0xd6,
        0xd2, 0x2d, 0x2b, 0x34, 0x07, 0x21, 0x4f, 0x7f,
    ],
    // 4H
    [
        0x24, 0xe8, 0x0a, 0x6a, 0x06, 0x73, 0x1b, 0x72,
        0xf7, 0xc6, 0x6b, 0xb0, 0x5d, 0x20, 0xf7, 0xca,
        0xb0, 0xff, 0x33, 0xe4, 0x29, 0x60, 0xd6, 0x72,
        0xca, 0x49, 0xa6, 0x92, 0xbb, 0x20, 0xdb, 0x5e,
    ],
    // 5H
    [
        0xe0, 0xe0, 0x7e, 0x2e, 0x0b, 0x46, 0x18, 0xd0,
        0x92, 0xe5, 0xac, 0x29, 0xc1, 0x01, 0x82, 0x77,
        0xaa, 0x6b, 0xfc, 0x42, 0x68, 0xfb, 0xa8, 0xdc,
        0xe6, 0x55, 0x56, 0x81, 0x06, 0x8e, 0x7f, 0x1f,
    ],
    // 6H
    [
        0xd8, 0x7c, 0x23, 0xe3, 0x46, 0x7f, 0x6e, 0x56,
        0xc8, 0x23, 0x63, 0x78, 0x5c, 0x83, 0x17, 0x2c,
        0x9f, 0x10, 0x10, 0x9d, 0x59, 0x66, 0xc4, 0xb8,
        0x8b, 0x00, 0x61, 0xef, 0x85, 0x36, 0x59, 0x08,
    ],
    // 7H
    [
        0x92, 0x68, 0x0d, 0xa1, 0x89, 0x54, 0xc9, 0xa9,
        0x1f, 0xaf, 0x24, 0x6b, 0x47, 0x6c, 0x62, 0x31,
        0xf0, 0xaf, 0x3b, 0x4e, 0x53, 0x0e, 0xf1, 0xce,
        0x5e, 0xdc, 0xb4, 0x8d, 0x08, 0x75, 0x11, 0x47,
    ],
    // 8H
    [
        0xe4, 0x80, 0x8d, 0xd7, 0x6e, 0xf0, 0x4b, 0x45,
        0xb5, 0x29, 0x8e, 0x57, 0xf4, 0x92, 0xba, 0x52,
        0x99, 0xc5, 0x6e, 0x57, 0x7b, 0xe0, 0xa1, 0xbf,
        0x64, 0x8e, 0x87, 0x49, 0x05, 0x2f, 0xc2, 0x48,
    ],
    // 9H
    [
        0xee, 0x77, 0x62, 0xb0, 0x07, 0x62, 0x38, 0xc9,
        0x0d, 0xce, 0x0e, 0xd8, 0x56, 0x6e, 0x76, 0xbf,
        0x5e, 0x15, 0xd4, 0x4c, 0x38, 0x9e, 0xc5, 0x90,
        0xf5, 0x2e, 0x22, 0x51, 0x5e, 0x94, 0xec, 0x61,
    ],
    // TH
    [
        0x90, 0x08, 0x6b, 0xaf, 0xc6, 0x99, 0x25, 0x19,
        0xf8, 0xb9, 0x84, 0x89, 0x0e, 0x5d, 0x40, 0x3e,
        0xc6, 0xa0, 0x25, 0x18, 0xe1, 0x3d, 0xc5, 0xfa,
        0x7d, 0x63, 0x01, 0xf2, 0x4b, 0x04, 0x4b, 0x7e,
    ],
    // JH
    [
        0x7e, 0x16, 0x9e, 0x1c, 0x9c, 0xf8, 0x16, 0x8c,
        0xd0, 0x8e, 0xdd, 0x48, 0xb6, 0x25, 0x55, 0xe1,
        0xf3, 0x17, 0xc0, 0x3c, 0x33, 0x35, 0xfa, 0xa4,
        0x3b, 0x26, 0xff, 0x15, 0x29, 0x99, 0x34, 0x37,
    ],
    // QH
    [
        0xe6, 0xc5, 0xea, 0x3f, 0xab, 0x7b, 0x6b, 0xf2,
        0xe4, 0x41, 0xa2, 0x03, 0xff, 0x6b, 0xa2, 0x0a,
        0xc0, 0x83, 0x31, 0x19, 0x9e, 0x93, 0xb8, 0x75,
        0xa0, 0xcd, 0xda, 0x6d, 0xb0, 0xcc, 0x54, 0x05,
    ],
    // KH
    [
        0x2c, 0xe2, 0x37, 0x97, 0x3a, 0x14, 0x51, 0xe1,
        0x49, 0xde, 0xc3, 0xec, 0x10, 0x0a, 0xbc, 0x80,
        0x19, 0xbd, 0x84, 0xac, 0x4b, 0x8d, 0x88, 0xa7,
        0x09, 0x76, 0x7f, 0x04, 0x90, 0x49, 0x23, 0x0a,
    ],
    // AH
    [
        0xec, 0x2d, 0x40, 0xbf, 0xbb, 0x67, 0xd5, 0xdb,
        0x14, 0x53, 0x84, 0xb1, 0x1a, 0xde, 0x77, 0x08,
        0xae, 0xd5, 0xd5, 0x1c, 0xb9, 0x2c, 0xe7, 0x68,
        0x22, 0xfc, 0x02, 0x83, 0x30, 0x12, 0x4b, 0x45,
    ],
    // 2S
    [
        0x96, 0xba, 0x65, 0x10, 0x13, 0xc7, 0x88, 0xf4,
        0x00, 0x5f, 0xb5, 0xed, 0xa0, 0xd2, 0x89, 0x4c,
        0xdb, 0xb9, 0x96, 0xea, 0x89, 0x02, 0xdc, 0x7a,
        0xc5, 0x36, 0x3b, 0x21, 0x3d, 0x26, 0x04, 0x27,
    ],
    // 3S
    [
        0x4a, 0x4d, 0x05, 0x62, 0xbf, 0xaf, 0x62, 0x61,
        0x73, 0x10, 0x7c, 0x21, 0xfd, 0x9c, 0x78, 0xb8,
        0xe0, 0x65, 0x10, 0xac, 0x29, 0x9f, 0x2e, 0xd7,
        0x6e, 0xbf, 0xc6, 0x35, 0xa9, 0x2a, 0x31, 0x3d,
    ],
    // 4S
    [
        0x7e, 0xf7, 0xb5, 0x49, 0xd5, 0x01, 0xae, 0xd2,
        0x5b, 0xf3, 0x71, 0x70, 0x72, 0xaf, 0x1c, 0xba,
        0x4d, 0x34, 0x4f, 0xae, 0xdc, 0xd8, 0x7f, 0x82,
        0x27, 0x60, 0xd6, 0x33, 0xbb, 0x24, 0x5f, 0x79,
    ],
    // 5S
    [
        0x2e, 0xd6, 0x1e, 0xdd, 0x4d, 0xa1, 0xc7, 0xd7,
        0xb6, 0x92, 0xda, 0xbb, 0x3f, 0xeb, 0x6c, 0x4b,
        0x6c, 0xe0, 0xe8, 0xd8, 0x60, 0x14, 0x43, 0x04,
        0x9d, 0xa6, 0x3d, 0x20, 0x10, 0xcd, 0x9f, 0x2f,
    ],
    // 6S
    [
        0x54, 0x11, 0x4c, 0xc9, 0x96, 0x1b, 0x71, 0xb8,
        0x79, 0x5e, 0xf6, 0xfb, 0xa4, 0xa1, 0xb7, 0xbb,
        0x4d, 0x5d, 0xff, 0x3b, 0x04, 0xcf, 0x29, 0x41,
        0x1c, 0x3c, 0xec, 0x84, 0x10, 0x7f, 0xe2, 0x13,
    ],
    // 7S
    [
        0xd6, 0x1a, 0xd1, 0x8b, 0x8f, 0x7f, 0x57, 0x9b,
        0xa9, 0x37, 0x8e, 0xed, 0x11, 0xe4, 0xaf, 0xd7,
        0x39, 0x95, 0x0f, 0x0e, 0xf8, 0xf1, 0x87, 0x10,
        0x04, 0xf3, 0x4d, 0xa1, 0x6e, 0xce, 0x6f, 0x1a,
    ],
    // 8S
    [
        0xac, 0xd2, 0x3c, 0x73, 0xf2, 0xf5, 0xe0, 0x15,
        0xa7, 0xa6, 0xef, 0xc0, 0x5a, 0x3f, 0xf5, 0xfc,
        0xff, 0xcc, 0x0b, 0x66, 0xda, 0x42, 0xa8, 0xe1,
        0x6c, 0x3c, 0xf7, 0x9f, 0xc3, 0x1e, 0x89, 0x16,
    ],
    // 9S
    [
        0xd6, 0x35, 0x80, 0xfe, 0xe5, 0x21, 0x45, 0xcb,
        0x67, 0x47, 0xe8, 0xaa, 0x5a, 0xda, 0xb7, 0x0e,
        0x45, 0xb9, 0xc5, 0xf7, 0x25, 0xf3, 0xe6, 0xa1,
        0xd3, 0x09, 0xf3, 0x0c, 0x42, 0x5d, 0x40, 0x44,
    ],
    // TS
    [
        0x32, 0x46, 0xf6, 0xf5, 0x15, 0x01, 0xb1, 0x41,
        0xb5, 0xee, 0xbd, 0xd4, 0x06, 0x42, 0xab, 0x53,
        0x2d, 0x02, 0xd3, 0x0e, 0xab, 0x60, 0x3d, 0x2e,
        0x12, 0xd2, 0xae, 0x78, 0x71, 0x74, 0xf1, 0x5f,
    ],
    // JS
    [
        0xc8, 0xff, 0x12, 0x69, 0x6f, 0xf4, 0x75, 0x9a,
        0x99, 0xa9, 0xa1, 0x84, 0x95, 0x8e, 0x64, 0x16,
        0x08, 0x9c, 0xec, 0x19, 0x7e, 0x66, 0xf6, 0x61,
        0xcc, 0x0c, 0x7c, 0x31, 0xec, 0x75, 0xb0, 0x79,
    ],
    // QS
    [
        0x02, 0xf8, 0x99, 0xb4, 0xc5, 0x4b, 0x82, 0xb4,
        0x07, 0x8c, 0x16, 0x38, 0x00, 0xfa, 0x59, 0x48,
        0x44, 0x58, 0x42, 0x9f, 0x1e, 0x37, 0x81, 0x86,
        0x6d, 0xc4, 0x00, 0x4d, 0x32, 0x68, 0x54, 0x02,
    ],
    // KS
    [
        0xa8, 0xa3, 0x78, 0x59, 0x72, 0x8a, 0x45, 0x40,
        0x43, 0x20, 0x74, 0x60, 0x7b, 0x38, 0x60, 0xe5,
        0xef, 0x12, 0x05, 0xfb, 0xca, 0x90, 0x03, 0x61,
        0x2c, 0x2c, 0xed, 0xa0, 0xba, 0x0f, 0x30, 0x61,
    ],
    // AS
    [
        0x4e, 0x10, 0x6d, 0x82, 0x00, 0xae, 0xf0, 0x11,
        0x70, 0x00, 0xc1, 0x95, 0x2c, 0xb6, 0x85, 0x9b,
        0x29, 0x6d, 0x57, 0x68, 0x19, 0x01, 0xc2, 0x13,
        0x30, 0x51, 0xe8, 0x93, 0x19, 0x50, 0xfc, 0x22,
    ],
];

/// Which card a point is, or `None` if it is not a card at all.
///
/// The failure is the useful case. A point that is none of the fifty-two means
/// somebody published a share that was not the inverse of the key they locked
/// with — the hand is broken and cannot be settled, which is the right outcome.
/// Reaching here should now take a client bug rather than a cheat: every round
/// is proved where it is published, and `publish_unlocks` checks a share
/// against its author's committed key before applying it. `program/mental`'s
/// audit remains the break-glass for the case no proof can rule out, which is
/// the two implementations of a verifier disagreeing with each other.
pub fn card_of_point(point: &[u8; 32]) -> Option<u8> {
    CARD_POINTS
        .iter()
        .position(|candidate| candidate == point)
        .map(|card| card as u8)
}

/// Multiplies one player's unlock share into a slot's running point.
///
/// The order shares arrive in does not matter: scalar multiplication commutes,
/// which is the property the whole scheme is built on, so players unlock in
/// whatever order they happen to act in.
pub fn apply_unlock(point: &[u8; 32], share: &[u8; 32]) -> Result<[u8; 32]> {
    let product = multiply_ristretto(&PodScalar(*share), &PodRistrettoPoint(*point))
        .ok_or(RiverproofError::InvalidUnlockShare)?;
    Ok(product.0)
}

/// The ristretto255 basepoint, compressed.
///
/// A correct unlock share is the inverse of the key its author locked with, and
/// `k * G` is what they published for that position — so `share * (k * G)` is
/// `G` exactly when the share is right. This is the only constant that check
/// needs. Recorded from `riverproof_mental::group::basepoint`, and the test
/// below re-derives it.
const BASEPOINT: [u8; 32] = [
    0xe2, 0xf2, 0xae, 0x0a, 0x6a, 0xbc, 0x4e, 0x71, 0xa8, 0x84, 0xa9, 0x61, 0xc5, 0x00, 0x51, 0x5f,
    0x58, 0xe3, 0x0b, 0x6a, 0xa5, 0x82, 0xdd, 0x8d, 0xb6, 0xa6, 0x59, 0x45, 0xe0, 0x8d, 0x2d, 0x76,
];

/// The deepest key path a full deck needs: 52 positions padded to 64.
pub const MERKLE_DEPTH: usize = 6;

/// Board cards in a hand: the last `BOARD_SLOTS` slots of every deal.
///
/// Named because `recover_unlocks` addresses board cards by their offset within
/// this range rather than by slot index, which is what makes a hole card
/// inexpressible there.
pub const BOARD_SLOTS: usize = 5;

const LEAF_DOMAIN: &[u8] = b"riverproof/mental/merkle-leaf/v1";
const NODE_DOMAIN: &[u8] = b"riverproof/mental/merkle-node/v1";
const COMMIT_DOMAIN: &[u8] = b"riverproof/mental/lock-commit/v1";

/// SHA-256, because it is a syscall and SHA-512 is not. Mirrors
/// `riverproof_mental::merkle`, which is pinned to the same recorded vector.
fn merkle_leaf(index: u32, value: &[u8; 32]) -> [u8; 32] {
    hashv(&[LEAF_DOMAIN, &index.to_le_bytes(), value]).to_bytes()
}

fn merkle_node(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    hashv(&[NODE_DOMAIN, left, right]).to_bytes()
}

/// Folds a key and its siblings back to the root its author committed.
fn merkle_fold(value: &[u8; 32], index: usize, siblings: &[[u8; 32]]) -> [u8; 32] {
    let mut hash = merkle_leaf(index as u32, value);
    let mut cursor = index;
    for sibling in siblings {
        hash = if cursor % 2 == 0 {
            merkle_node(&hash, sibling)
        } else {
            merkle_node(sibling, &hash)
        };
        cursor /= 2;
    }
    hash
}

fn lock_commitment(round_digest: &[u8; 32], keys_root: &[u8; 32]) -> [u8; 32] {
    hashv(&[COMMIT_DOMAIN, round_digest, keys_root]).to_bytes()
}

/// Whether `share` is the inverse of the key its author committed for `position`.
///
/// Two questions, in the order that makes the second one meaningful. First,
/// does `key` really belong to that author at that position — the path folds to
/// the root inside the commitment they made before any bytes moved. Second, is
/// `share` its inverse — one multiplication, against the basepoint.
///
/// Without this a forged share against somebody else's hole card is accepted in
/// silence: nothing checks a slot until its owner adds the last layer at
/// showdown, and it is then the owner whose transaction fails and the owner
/// whom `muck_unrevealed` folds for declining to show.
pub fn verify_unlock_share(
    share: &[u8; 32],
    key: &[u8; 32],
    position: u8,
    siblings: &[[u8; 32]],
    round_digest: &[u8; 32],
    commitment: &[u8; 32],
) -> Result<()> {
    require!(siblings.len() <= MERKLE_DEPTH, RiverproofError::InvalidUnlockShare);
    let root = merkle_fold(key, position as usize, siblings);
    require!(
        lock_commitment(round_digest, &root) == *commitment,
        RiverproofError::UnlockKeyNotCommitted
    );

    let product = multiply_ristretto(&PodScalar(*share), &PodRistrettoPoint(*key))
        .ok_or(RiverproofError::InvalidUnlockShare)?;
    require!(product.0 == BASEPOINT, RiverproofError::InvalidUnlockShare);
    Ok(())
}

/// One dealt position, as the chain tracks it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Slot {
    /// The deck position this slot stands for.
    pub position: u8,
    /// The seat that owns it, or `NO_SEAT_OWNER` for a board card.
    pub owner: u8,
    /// Starts as the locked card, loses a layer per share, ends as a card point.
    pub point: [u8; 32],
    /// Players who have published a share for it.
    pub unlocked: u16,
    /// The card, once every layer is off. `NO_CARD` until then.
    pub card: u8,
}

/// A slot nobody owns: a board card.
pub const NO_SEAT_OWNER: u8 = u8::MAX;

impl Slot {
    pub fn is_board(&self) -> bool {
        self.owner == NO_SEAT_OWNER
    }

    /// Whether every player has taken their layer off.
    pub fn is_open(&self, players: u8) -> bool {
        self.unlocked.count_ones() == u32::from(players)
    }
}

/// Applies a share and, if that was the last layer, reads the card underneath.
///
/// Refuses a second share from the same player: without that, one player could
/// multiply a slot by anything they liked as many times as they liked, and the
/// count of contributors would still say the slot was properly opened.
pub fn unlock_slot(slot: &mut Slot, player: u8, share: &[u8; 32], players: u8) -> Result<()> {
    // Against `players`, not the width of the bitmask. A share attributed to a
    // seat this deal does not have would set a bit outside the mask, so
    // `is_open` could never count it and the point would take a multiplication
    // that nothing accounts for.
    require!(player < players, RiverproofError::UnknownDealPlayer);
    let bit = 1u16 << player;
    require!(slot.unlocked & bit == 0, RiverproofError::UnlockShareAlreadyGiven);

    slot.point = apply_unlock(&slot.point, share)?;
    slot.unlocked |= bit;

    if slot.is_open(players) {
        // The chain is never told what the card is. It falls out.
        slot.card = card_of_point(&slot.point).ok_or(RiverproofError::UnlockDidNotOpenACard)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use riverproof_mental::group::{encode_scalar, Seed};
    use riverproof_mental::protocol::deal_hand;

    fn seeds(count: usize) -> Vec<Seed> {
        (0..count)
            .map(|player| {
                let mut bytes = [0u8; 32];
                bytes[0] = player as u8;
                bytes[1] = 0x7c;
                Seed::new(bytes)
            })
            .collect()
    }

    /// The baked table is the hashed one, not a number somebody typed.
    #[test]
    fn the_card_table_is_the_hashed_one() {
        for card in 0..CARD_COUNT as u8 {
            let derived = riverproof_mental::group::encode_point(
                &riverproof_mental::card_point(card),
            );
            assert_eq!(CARD_POINTS[card as usize], derived, "card {card}");
        }
        // Fifty-two distinct points, or two cards would open to the same one.
        let mut sorted = CARD_POINTS;
        sorted.sort_unstable();
        assert!(sorted.windows(2).all(|pair| pair[0] != pair[1]));
    }

    /// A real deal, opened the way the chain opens it.
    ///
    /// The deck comes from `riverproof-mental`, the shares are the inverses of
    /// the keys it locked with, and the arithmetic here is the same
    /// `multiply_ristretto` the validator runs. Nothing about the card is
    /// asserted from the protocol's side: the slot resolves to a card and only
    /// then is it compared.
    /// The basepoint the share check compares against is the group's own.
    #[test]
    fn the_recorded_basepoint_is_the_one_the_protocol_uses() {
        assert_eq!(
            BASEPOINT,
            riverproof_mental::group::encode_point(&riverproof_mental::group::basepoint()),
        );
    }

    /// The program's Merkle walk and the protocol crate's are one function.
    ///
    /// The browser builds the path and the validator folds it, so a one-bit
    /// disagreement is a hand that cannot be dealt. Checked against the crate
    /// the browser is itself pinned to, rather than against a copy.
    #[test]
    fn the_merkle_walk_matches_the_protocol_crate() {
        let values: Vec<[u8; 32]> = (0..52)
            .map(|index| {
                let mut value = [0u8; 32];
                value[0] = index as u8;
                value[1] = 0xa5;
                value
            })
            .collect();
        let root = riverproof_mental::merkle::root(&values);
        for index in 0..values.len() {
            let siblings = riverproof_mental::merkle::path(&values, index);
            assert_eq!(merkle_fold(&values[index], index, &siblings), root, "index {index}");
        }
        assert_eq!(
            lock_commitment(&[7u8; 32], &root),
            riverproof_mental::merkle::lock_commitment(&[7u8; 32], &root),
        );
    }

    /// The check the forged share could not survive.
    #[test]
    fn a_share_verifies_only_against_the_key_its_author_committed() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| (players as u8 * 2) + 1 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        // `k * G` for every position, which is what a lock round publishes.
        // Derived here from the author's own unlock shares, since a share is
        // the key's inverse: `share^-1 * G == k * G`.
        let player = 1usize;
        let keys: Vec<[u8; 32]> = (0..52u8)
            .map(|position| {
                let share = session.participants[player].unlock(position).unwrap();
                riverproof_mental::group::encode_point(
                    &(riverproof_mental::group::basepoint() * share.invert()),
                )
            })
            .collect();
        let root = riverproof_mental::merkle::root(&keys);
        let digest = [0x5au8; 32];
        let commitment = riverproof_mental::merkle::lock_commitment(&digest, &root);

        let position = 0u8;
        let siblings = riverproof_mental::merkle::path(&keys, position as usize);
        let honest = encode_scalar(&session.participants[player].unlock(position).unwrap());

        verify_unlock_share(
            &honest,
            &keys[position as usize],
            position,
            &siblings,
            &digest,
            &commitment,
        )
        .expect("the author's own share verifies");

        // The forgery from the test above: a real share, for another position.
        let forged = encode_scalar(&session.participants[player].unlock(position + 1).unwrap());
        assert!(
            verify_unlock_share(
                &forged,
                &keys[position as usize],
                position,
                &siblings,
                &digest,
                &commitment,
            )
            .is_err(),
            "a forged share passed the check",
        );

        // And they cannot swap in the key that would make it verify: that key
        // is committed at a different position, so its path is wrong here.
        assert!(
            verify_unlock_share(
                &forged,
                &keys[(position + 1) as usize],
                position,
                &siblings,
                &digest,
                &commitment,
            )
            .is_err(),
            "another position's key opened this one",
        );
    }

    /// A rebuilt board key opens the card, and is accepted for the seat whose
    /// key it is rather than the seat that sent it.
    ///
    /// This is the whole point of `recover_unlocks`. `Slot::is_open` counts
    /// every player, so a board card locked under a seat that stops answering
    /// can never open — the betting side can fold that seat and the card stays
    /// shut, because folding does not surrender a key. Before this the only way
    /// out was `abandon_mental_deal` and a refund, and a devnet table died at
    /// the flop proving it.
    #[test]
    fn a_rebuilt_board_key_opens_the_card_without_its_owner() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| (players as u8 * 2) + 1 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        let absent = 2u8;
        let position = board[0];
        let present: Vec<u8> = (0..players as u8).filter(|p| *p != absent).collect();

        // The present seats rebuild the absent seat's key from the shares it
        // dealt them when it locked. Neither could do this alone.
        let rebuilt = session
            .recover_key(absent, position, &present)
            .expect("a threshold rebuilds the key");
        assert_eq!(
            riverproof_mental::group::encode_point(
                &(riverproof_mental::group::basepoint() * rebuilt)
            ),
            session.deal.key_commitment(absent, position).expect("a commitment"),
            "the rebuilt key is not the one that position was locked under",
        );

        // Applied as the absent seat's layer, the card opens exactly as it
        // would have if that seat had published it itself.
        let locked =
            riverproof_mental::group::encode_point(&session.locked[position as usize]);
        let mut slot = Slot {
            position,
            owner: NO_SEAT_OWNER,
            point: locked,
            unlocked: 0,
            card: NO_CARD,
        };
        for player in 0..players as u8 {
            let share = if player == absent {
                encode_scalar(&rebuilt.invert())
            } else {
                encode_scalar(&session.participants[player as usize].unlock(position).unwrap())
            };
            unlock_slot(&mut slot, player, &share, players as u8).expect("a layer comes off");
        }
        assert_ne!(slot.card, NO_CARD, "a rebuilt key did not open the board card");
        assert_eq!(
            slot.card,
            session.deck().expect("the deck opens")[position as usize],
            "the board card opened as the wrong card",
        );
    }

    /// And the same machinery cannot reach a hole card, by anybody.
    ///
    /// Two independent barriers, and this pins the cryptographic one.
    /// `deal_shares` runs only over the board positions, so no seat holds any
    /// share of another's *hole* key and there is nothing to interpolate — a
    /// coalition of every other seat together still cannot rebuild one. The
    /// second barrier is that `recover_unlocks` addresses cards by their offset
    /// within the board, so a hole card cannot even be named in its arguments.
    #[test]
    fn a_hole_key_cannot_be_rebuilt_by_everybody_else() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| (players as u8 * 2) + 1 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        // Position 0 is seat 0's hole card, and it is not a shared position.
        let owner = 0u8;
        let hole = 0u8;
        assert!(
            !session.deal.shared_positions.contains(&hole),
            "the fixture stopped treating a hole card as unshared",
        );

        let everybody_else: Vec<u8> = (0..players as u8).filter(|p| *p != owner).collect();
        assert!(
            session.recover_key(owner, hole, &everybody_else).is_err(),
            "every other seat together rebuilt a hole key",
        );
    }

    /// Why `publish_unlocks` proves a share before applying it: `unlock_slot`
    /// on its own cannot tell a forged one from an honest one.
    ///
    /// This is the damage that would follow if it did not. A hole slot needs
    /// every player's layer and the owner's is deliberately last — publishing
    /// it is what showing down means — so a wrong share from anyone else lands
    /// during the deal against a slot nothing would check until showdown, and
    /// `unlock_slot` applies it without complaint, as this test shows.
    ///
    /// The bill would arrive much later and at the wrong address. The owner's
    /// own share is the last layer, so it is the owner's transaction that fails
    /// `UnlockDidNotOpenACard`, and no share they could ever send would
    /// succeed. `muck_unrevealed` would then fold them for declining to show a
    /// card they were prevented from showing, and pay the pot to the contenders
    /// who did — including whoever forged the share.
    ///
    /// The check costs one `multiply_ristretto`: given the key commitment
    /// `k * G` from the publisher's lock round, a correct share is exactly the
    /// scalar with `share * (k * G) == G`. What made it possible was an
    /// authenticated commitment to check against, which is what
    /// `lock_round_commitment` is for — it binds the round digest to a Merkle
    /// root over the keys, so the program can open one leaf. `verify_unlock_share`
    /// is the check and `publish_unlocks` is where it runs; this test keeps the
    /// unguarded arithmetic honest about needing it.
    #[test]
    fn a_forged_share_poisons_a_hole_slot_and_blames_its_owner() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| (players as u8 * 2) + 1 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        // Position 0 belongs to player 0. Players 1 and 2 publish during the
        // deal; player 0's own layer waits for showdown.
        let position = 0u8;
        let owner = 0u8;
        let forger = 1u8;
        let locked =
            riverproof_mental::group::encode_point(&session.locked[position as usize]);
        let mut slot = Slot { position, owner, point: locked, unlocked: 0, card: NO_CARD };

        for player in 0..players as u8 {
            if player == owner {
                continue;
            }
            let share = if player == forger {
                // Anything at all. Nothing checks it at this layer — which is
                // exactly why `publish_unlocks` checks it at the one above.
                encode_scalar(&session.participants[forger as usize].unlock(position + 1).unwrap())
            } else {
                encode_scalar(&session.participants[player as usize].unlock(position).unwrap())
            };
            unlock_slot(&mut slot, player, &share, players as u8)
                .expect("a forged share is accepted in silence");
        }

        assert_eq!(slot.card, NO_CARD, "the slot is not open until its owner shows");

        // Showdown. The owner publishes their real share — the correct one —
        // and it is their transaction that fails.
        let honest = encode_scalar(&session.participants[owner as usize].unlock(position).unwrap());
        let outcome = unlock_slot(&mut slot, owner, &honest, players as u8);
        assert!(
            outcome.is_err(),
            "the owner could still show, so the poisoning did not take",
        );

        // And there is no share they could have sent instead: the point is
        // wrong, not their key.
        for candidate in 0..(players as u8 * 2) {
            let mut retry = Slot { position, owner, point: slot.point, unlocked: 0, card: NO_CARD };
            let share =
                encode_scalar(&session.participants[owner as usize].unlock(candidate).unwrap());
            assert!(
                unlock_slot(&mut retry, owner, &share, 1).is_err(),
                "some share opened a poisoned slot, so this is recoverable after all",
            );
        }
    }

    #[test]
    fn unlocking_every_layer_opens_the_card_the_players_dealt() {
        for players in 3..=6usize {
            let board: Vec<u8> = (0..5).map(|index| (players as u8 * 2) + 1 + index).collect();
            let session = deal_hand(&seeds(players), &board, None).expect("a deal");
            let deck = session.deck().expect("the deck opens");

            for position in 0..(players as u8 * 2) {
                let locked = riverproof_mental::group::encode_point(
                    &session.locked[position as usize],
                );
                let mut slot = Slot {
                    position,
                    owner: position % players as u8,
                    point: locked,
                    unlocked: 0,
                    card: NO_CARD,
                };

                for player in 0..players as u8 {
                    assert_eq!(slot.card, NO_CARD, "{players} players: opened early");
                    let share = encode_scalar(
                        &session.participants[player as usize].unlock(position).unwrap(),
                    );
                    unlock_slot(&mut slot, player, &share, players as u8).expect("a share");
                }

                assert!(slot.is_open(players as u8));
                assert_eq!(
                    slot.card, deck[position as usize],
                    "{players} players: position {position} opened as the wrong card",
                );
            }
        }
    }

    /// A share that is not the inverse of a real key opens nothing. It cannot
    /// open a card of the sender's choosing, which is the property that lets
    /// the chain accept whatever the arithmetic produces.
    #[test]
    fn a_wrong_share_opens_no_card_at_all() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| 7 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        let mut slot = Slot {
            position: 0,
            owner: 0,
            point: riverproof_mental::group::encode_point(&session.locked[0]),
            unlocked: 0,
            card: NO_CARD,
        };
        for player in 0..2u8 {
            let share =
                encode_scalar(&session.participants[player as usize].unlock(0).unwrap());
            unlock_slot(&mut slot, player, &share, players as u8).expect("a share");
        }

        // The last player publishes something else entirely.
        let forged = encode_scalar(&Seed::new([0x5a; 32]).scalar("forgery", 0));
        let error = unlock_slot(&mut slot, 2, &forged, players as u8).unwrap_err();
        assert_eq!(error, RiverproofError::UnlockDidNotOpenACard.into());
    }

    /// One player cannot stand in for the rest by publishing twice.
    #[test]
    fn a_player_may_only_unlock_a_slot_once() {
        let players = 3usize;
        let board: Vec<u8> = (0..5).map(|index| 7 + index).collect();
        let session = deal_hand(&seeds(players), &board, None).expect("a deal");

        let mut slot = Slot {
            position: 0,
            owner: 0,
            point: riverproof_mental::group::encode_point(&session.locked[0]),
            unlocked: 0,
            card: NO_CARD,
        };
        let share = encode_scalar(&session.participants[0].unlock(0).unwrap());
        unlock_slot(&mut slot, 0, &share, players as u8).expect("the first share");

        let error = unlock_slot(&mut slot, 0, &share, players as u8).unwrap_err();
        assert_eq!(error, RiverproofError::UnlockShareAlreadyGiven.into());
        assert_eq!(slot.unlocked.count_ones(), 1);
    }

    /// Thirty-two bytes that are not a point are refused before anything is
    /// written, so a slot cannot be left half-unlocked.
    #[test]
    fn a_share_against_a_point_that_is_not_a_point_is_refused() {
        let mut slot = Slot {
            position: 0,
            owner: 0,
            // Not a canonical ristretto encoding.
            point: [0xff; 32],
            unlocked: 0,
            card: NO_CARD,
        };
        let share = encode_scalar(&Seed::new([1u8; 32]).scalar("x", 0));
        assert_eq!(
            unlock_slot(&mut slot, 0, &share, 3).unwrap_err(),
            RiverproofError::InvalidUnlockShare.into(),
        );
        assert_eq!(slot.unlocked, 0, "a failed share still marked the player");
    }
}
