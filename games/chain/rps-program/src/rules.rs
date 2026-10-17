use solana_program::{hash::hashv, pubkey::Pubkey};

pub const PHASE_SECONDS: i64 = 60;
pub const MOVE_FEE: u64 = 10_000;
pub const SETTLE_FEE: u64 = 5_000;
pub const MOVES_PER_SIDE_BUDGET: u64 = 1_000_000;
pub const NO_WINNER: u8 = 255;

pub fn target(best_of: u8) -> Option<u8> {
    match best_of { 1|3|5|7|9 => Some(best_of / 2 + 1), _ => None }
}
/// 0 rock, 1 paper, 2 scissors. 255 is a tied round, never an automatic point.
pub fn winner(a: u8, b: u8) -> Option<u8> {
    if a > 2 || b > 2 { None }
    else if a == b { Some(NO_WINNER) }
    else if (a + 3 - b) % 3 == 1 { Some(0) } else { Some(1) }
}
pub fn commitment(program: &Pubkey, game: &Pubkey, player: &Pubkey, round: u32, choice: u8, salt: &[u8;32]) -> [u8;32] {
    hashv(&[b"hooddice:rps:commit:v1",program.as_ref(),game.as_ref(),player.as_ref(),&round.to_le_bytes(),&[choice],salt]).to_bytes()
}
pub fn authorization(program: &Pubkey, game: &Pubkey, player: &Pubkey, round: u32, action: u8, payload: &[u8]) -> Vec<u8> {
    [b"hooddice:rps:move:v1".as_slice(),program.as_ref(),game.as_ref(),player.as_ref(),&round.to_le_bytes(),&[action],payload].concat()
}
pub fn forfeit(mask: u8) -> u8 { match mask { 1 => 0, 2 => 1, _ => NO_WINNER } }

#[cfg(test)] mod tests {
    use super::*;
    #[test] fn supported_series() {
        for n in 0..=10 { assert_eq!(target(n), if [1,3,5,7,9].contains(&n) {Some(n/2+1)} else {None}); }
    }
    #[test] fn all_hands() {
        assert_eq!((0..3).flat_map(|a|(0..3).map(move|b|winner(a,b).unwrap())).collect::<Vec<_>>(), vec![255,1,0,0,255,1,1,0,255]);
        assert_eq!(winner(3,0),None);
    }
    #[test] fn commitments_bind_every_context_field() {
        let p=Pubkey::new_unique();let g=Pubkey::new_unique();let u=Pubkey::new_unique();let s=[4;32];
        let h=commitment(&p,&g,&u,3,1,&s);
        assert_ne!(h,commitment(&g,&g,&u,3,1,&s));assert_ne!(h,commitment(&p,&p,&u,3,1,&s));
        assert_ne!(h,commitment(&p,&g,&p,3,1,&s));assert_ne!(h,commitment(&p,&g,&u,4,1,&s));
        assert_ne!(h,commitment(&p,&g,&u,3,2,&s));assert_ne!(h,commitment(&p,&g,&u,3,1,&[5;32]));
    }
    #[test] fn timeout_rewards_only_the_responsive_player() { assert_eq!([forfeit(0),forfeit(1),forfeit(2),forfeit(3)],[255,0,1,255]); }
}
