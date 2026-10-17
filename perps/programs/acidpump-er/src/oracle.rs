//! Strict decoding of the deployed MagicBlock-attested Pyth Lazer accounts.
//! These accounts use a positive decimal count, unlike Pyth's signed exponent.
use anchor_lang::prelude::*;
use crate::MixError;

pub const ORACLE_PROGRAM: Pubkey = pubkey!("PriCems5tHihc6UDXDjzjeawomAwBduWMGAi8ZUjppd");
pub const ORACLE_DISCRIMINATOR: [u8; 8] = [234, 161, 14, 36, 172, 239, 15, 232];
pub const MAX_ORACLE_AGE: i64 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OraclePrice {
    pub price: u64,
    pub publish_time: i64,
    pub posted_slot: u64,
}

pub fn feed_address(market: u16) -> Result<Pubkey> {
    let feed: &[u8] = match market {
        0 => b"1", 1 => b"66", 2 => b"6", 3 => b"2",
        _ => return err!(MixError::Binding),
    };
    Ok(Pubkey::find_program_address(&[b"price_feed", b"pyth-lazer", feed], &ORACLE_PROGRAM).0)
}

pub fn read_oracle(account: &AccountInfo, market: u16, now: i64, slot: u64) -> Result<OraclePrice> {
    require!(!account.executable, MixError::Binding);
    decode(&account.key(), account.owner, &account.try_borrow_data()?, market, now, slot)
}

pub fn decode(key: &Pubkey, owner: &Pubkey, data: &[u8], market: u16, now: i64, slot: u64) -> Result<OraclePrice> {
    require_keys_eq!(*owner, ORACLE_PROGRAM, MixError::Binding);
    require_keys_eq!(*key, feed_address(market)?, MixError::Binding);
    require!(data.len() == 134 && data[..8] == ORACLE_DISCRIMINATOR && data[40] == 1, MixError::Binding);
    require!(data[41..73] == key.to_bytes() && i32::from_le_bytes(data[89..93].try_into().unwrap()) == 8, MixError::Binding);
    let raw = i64::from_le_bytes(data[73..81].try_into().unwrap());
    let publish_time = i64::from_le_bytes(data[93..101].try_into().unwrap());
    let posted_slot = u64::from_le_bytes(data[125..133].try_into().unwrap());
    require!(raw > 0 && posted_slot > 0 && posted_slot <= slot, MixError::Quote);
    require!(publish_time > 0 && publish_time <= now.saturating_add(2)
        && now.saturating_sub(publish_time) <= MAX_ORACLE_AGE, MixError::Stale);
    let price = (raw as u64).checked_mul(10_000).ok_or(MixError::Arithmetic)?;
    require!((2_000..=1_000_000_000_000_000_000).contains(&price), MixError::Quote);
    Ok(OraclePrice { price, publish_time, posted_slot })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(market: u16) -> (Pubkey, [u8; 134]) {
        let key = feed_address(market).unwrap();
        let mut data = [0; 134];
        data[..8].copy_from_slice(&ORACLE_DISCRIMINATOR);
        data[40] = 1;
        data[41..73].copy_from_slice(key.as_ref());
        data[73..81].copy_from_slice(&12_345_678_901i64.to_le_bytes());
        data[89..93].copy_from_slice(&8i32.to_le_bytes());
        data[93..101].copy_from_slice(&100i64.to_le_bytes());
        data[125..133].copy_from_slice(&50u64.to_le_bytes());
        (key, data)
    }
    #[test]
    fn exact_price_and_source_time() {
        let (key, data) = fixture(0);
        assert_eq!(decode(&key, &ORACLE_PROGRAM, &data, 0, 103, 50).unwrap(),
            OraclePrice { price: 123_456_789_010_000, publish_time: 100, posted_slot: 50 });
        assert!(decode(&key, &ORACLE_PROGRAM, &data, 0, 104, 50).is_err());
        assert!(decode(&key, &ORACLE_PROGRAM, &data, 0, 97, 50).is_err());
        assert!(decode(&key, &ORACLE_PROGRAM, &data, 0, 100, 49).is_err());
    }
    #[test]
    fn rejects_wrong_feed_owner_layout_precision_and_unverified_data() {
        let (key, data) = fixture(0);
        assert!(decode(&key, &Pubkey::new_unique(), &data, 0, 100, 50).is_err());
        assert!(decode(&key, &ORACLE_PROGRAM, &data, 1, 100, 50).is_err());
        assert!(decode(&key, &ORACLE_PROGRAM, &data[..133], 0, 100, 50).is_err());
        for offset in [0, 40, 41, 89] {
            let mut corrupt = data;
            corrupt[offset] ^= 1;
            assert!(decode(&key, &ORACLE_PROGRAM, &corrupt, 0, 100, 50).is_err());
        }
        let mut negative = data;
        negative[73..81].copy_from_slice(&(-1i64).to_le_bytes());
        assert!(decode(&key, &ORACLE_PROGRAM, &negative, 0, 100, 50).is_err());
    }
}
