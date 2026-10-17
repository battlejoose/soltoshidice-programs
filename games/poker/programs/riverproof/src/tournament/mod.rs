//! Tournament policy. Instruction adapters must validate native table accounts
//! and perform Token-2022 transfers atomically with these state transitions.
//! This module alone does not authorize a transfer or trust a server score.
pub mod model;
pub mod accounts;
pub mod blinds;
pub mod legacy;
