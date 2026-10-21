//! Protocol v1. Cosmetic animation must never modify these rules.
use borsh::{BorshDeserialize, BorshSerialize};
use solana_program::{hash::hashv, program_error::ProgramError, pubkey::Pubkey};

pub const MAX_STRIKES: usize = 64;
pub const START_HP: u8 = 100;

#[derive(BorshSerialize, BorshDeserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Strike {
    pub actor: u8,
    pub dice: [u8; 3],
    // 0 hit, 1 block, 2 dodge, 3 parry, 4 critical. Pressure sets bit 7.
    pub outcome: u8,
    pub damage: u8,
    pub counter: u8,
    pub hp: [u8; 2],
}

pub fn initiative(random: &[u8; 64], fight: &Pubkey) -> u8 {
    hashv(&[b"hooddice:fight:initiative:v1", random, fight.as_ref()]).to_bytes()[0] & 1
}

pub fn dice(random: &[u8; 64], fight: &Pubkey, index: u8) -> Result<[u8; 3], ProgramError> {
    let mut result = [0; 3];
    let mut count = 0;
    for block in 0..8u8 {
        let hash = hashv(&[b"hooddice:fight:strike:v1", random, fight.as_ref(), &[index, block]]).to_bytes();
        for byte in hash {
            if byte < 252 {
                result[count] = byte % 6 + 1;
                count += 1;
                if count == 3 { return Ok(result); }
            }
        }
    }
    // Never replace extremely unlikely rejection exhaustion with biased dice.
    Err(ProgramError::Custom(110))
}

pub fn strike(hp: [u8; 2], actor: u8, index: u8, dice: [u8; 3]) -> Result<Strike, ProgramError> {
    if actor > 1 || index as usize >= MAX_STRIKES || hp.contains(&0)
        || !dice.iter().all(|d| (1..=6).contains(d)) {
        return Err(ProgramError::InvalidArgument);
    }
    let [attack, force, defense] = dice;
    let base = 8 + 2 * attack + 2 * force;
    let (mut outcome, mut damage, mut counter) = if defense == 6 && force <= 3 {
        (3, 0, 4 + attack)
    } else if defense == 5 && force <= 4 {
        (2, 0, 0)
    } else if defense >= 4 {
        (1, base / 2, 0)
    } else if force == 6 {
        (4, base + 8, 0)
    } else { (0, base, 0) };
    if index >= 32 {
        outcome |= 128;
        damage = damage.max(8);
        // No double knockout: parry counter is disabled in the pressure phase.
        counter = 0;
    }
    let a = actor as usize;
    let d = 1 - a;
    let mut next = hp;
    next[d] = next[d].saturating_sub(damage);
    next[a] = next[a].saturating_sub(counter);
    Ok(Strike { actor, dice, outcome, damage, counter, hp: next })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_roll_has_symmetric_damage_and_cannot_knock_both_out() {
        for a in 1..=6 { for b in 1..=6 { for c in 1..=6 {
            for hp in [[100, 100], [1, 1], [7, 31]] {
                for index in [0, 31, 32, 63] {
                    let left = strike(hp, 0, index, [a,b,c]).unwrap();
                    let right = strike([hp[1], hp[0]], 1, index, [a,b,c]).unwrap();
                    assert_eq!(left.damage, right.damage);
                    assert_eq!(left.counter, right.counter);
                    assert_eq!(left.hp, [right.hp[1], right.hp[0]]);
                    assert_ne!(left.hp, [0,0]);
                    if index >= 32 { assert!(left.damage >= 8); }
                }
            }
        }}}
    }

    #[test]
    fn worst_case_dodges_terminate_before_capacity() {
        for first in 0..2 {
            let mut hp = [100,100];
            let mut ended = false;
            for index in 0..64 {
                hp = strike(hp, (first + index) % 2, index, [1,1,5]).unwrap().hp;
                if hp.contains(&0) { assert!(index <= 56); ended = true; break; }
            }
            assert!(ended);
        }
    }

    #[test]
    fn seeded_fights_always_finish_and_have_legal_dice() {
        let fight = Pubkey::new_from_array([9;32]);
        for seed in 0..2048u32 {
            let hash = hashv(&[&seed.to_le_bytes()]).to_bytes();
            let mut random = [0;64]; random[..32].copy_from_slice(&hash); random[32..].copy_from_slice(&hash);
            let first = initiative(&random, &fight);
            let mut hp = [100,100];
            for i in 0..64 {
                hp = strike(hp, (first+i)%2, i, dice(&random, &fight, i).unwrap()).unwrap().hp;
                if hp.contains(&0) { break; }
            }
            assert!(hp.contains(&0));
            assert_ne!(hp, [0,0]);
        }
    }
}
