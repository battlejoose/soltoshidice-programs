//! Shared bracket arithmetic. Outcomes are supplied only by the native game handlers.
use solana_program::{hash::hashv, pubkey::Pubkey, program_error::ProgramError};

pub const MAX_PLAYERS: usize = 100;
pub const EMPTY: u8 = 255;
pub const WAITING: u8 = 254;
pub const PLAYING: u8 = 253;
pub const MIN_PRIZE: u64 = 100_000_000_000;
pub const MAX_ADVANCE: i64 = 86_400;
pub const ENTRY_WINDOW: i64 = 900;
pub const PHASE_SECONDS: i64 = 60;

pub fn best_of(n: u8) -> bool { [1, 3, 5, 7, 9].contains(&n) }
pub fn width(n: usize) -> usize { n.max(2).next_power_of_two() }
pub fn resolved(v: u8) -> bool { v < MAX_PLAYERS as u8 || v == EMPTY }

/// Rejection sampling avoids modulo bias. Randomness is requested only after entries lock.
pub fn seeds(random: &[u8;64], event: &Pubkey, n: usize) -> Result<Vec<u8>, ProgramError> {
    if !(2..=MAX_PLAYERS).contains(&n) { return Err(ProgramError::InvalidArgument); }
    let mut players: Vec<u8> = (0..n as u8).collect();
    let mut counter = 0u32;
    for i in (1..n).rev() {
        let bound = (i + 1) as u32;
        let limit = u32::MAX - u32::MAX % bound;
        let j = loop {
            let h = hashv(&[b"sdice:bracket:seeding:v1",random,event.as_ref(),&counter.to_le_bytes()]).to_bytes();
            counter = counter.checked_add(1).ok_or(ProgramError::ArithmeticOverflow)?;
            let v = u32::from_le_bytes(h[..4].try_into().unwrap());
            if v < limit { break (v % bound) as usize; }
        };
        players.swap(i,j);
    }
    Ok(players)
}

/// Spread byes across first-round pairs rather than giving repeated free rounds.
pub fn tree(players: &[u8]) -> Vec<u8> {
    let w = width(players.len());
    let mut nodes = vec![WAITING; w*2];
    nodes[0] = WAITING; // Third-place playoff lives outside the elimination tree.
    nodes[w..].fill(EMPTY);
    let half = w/2;
    for (i,p) in players.iter().enumerate() {
        let leaf = if i<half {2*i} else {2*(i-half)+1};
        nodes[w+leaf]=*p;
    }
    nodes
}

/// Missing places and integer rounding accrue to the champion. The full pool is paid.
pub fn prizes(total: u64, places: [u8;3]) -> Result<[u64;3], ProgramError> {
    if places[0] == EMPTY { return Err(ProgramError::InvalidArgument); }
    let second = if places[1] == EMPTY {0} else {(total as u128 * 30 / 100) as u64};
    let third = if places[2] == EMPTY {0} else {(total as u128 * 20 / 100) as u64};
    Ok([total-second-third,second,third])
}

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn all_field_sizes_have_exactly_one_leaf_per_player_and_no_repeated_byes() {
        for n in 2..=100 {
            let p:Vec<u8>=(0..n as u8).collect(); let t=tree(&p); let w=width(n);
            for i in p { assert_eq!(t[w..].iter().filter(|&&v|v==i).count(),1); }
            assert_eq!(t[w..].iter().filter(|&&v|v==EMPTY).count(),w-n);
            assert!(t[w..].chunks(2).all(|pair|pair[0]!=EMPTY));
        }
    }
    #[test] fn seeding_is_reproducible_bound_and_complete() {
        let event=Pubkey::new_unique();let a=seeds(&[3;64],&event,100).unwrap();
        assert_eq!(a,seeds(&[3;64],&event,100).unwrap());
        assert_ne!(a,seeds(&[4;64],&event,100).unwrap());
        assert_ne!(a,seeds(&[3;64],&Pubkey::new_unique(),100).unwrap());
        let mut s=a;s.sort();assert_eq!(s,(0..100).collect::<Vec<_>>());
    }
    #[test] fn prizes_conserve_every_unit() {
        for n in [1,99,100,101,MIN_PRIZE,u64::MAX] {
            for p in [[0,1,2],[0,1,EMPTY],[0,EMPTY,2],[0,EMPTY,EMPTY]] {
                let amounts=prizes(n,p).unwrap();assert_eq!(amounts.iter().map(|v|*v as u128).sum::<u128>(),n as u128);
            }
        }
        assert_eq!(prizes(100,[0,1,2]).unwrap(),[50,30,20]);
        assert_eq!(prizes(100,[0,1,EMPTY]).unwrap(),[70,30,0]);
    }
}
