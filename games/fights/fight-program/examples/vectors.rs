use hooddice_fights::combat::{dice,initiative,strike};
use solana_program::pubkey::Pubkey;
fn main() {
    let fight = Pubkey::new_from_array([9;32]);
    println!("[");
    for seed in 0..4u8 {
        let random: [u8;64] = std::array::from_fn(|i| (i as u8).wrapping_mul(seed+1).wrapping_add(seed*19));
        let first = initiative(&random,&fight);
        let mut hp = [100,100]; let mut bytes = Vec::new();
        for i in 0..64 {
            let s = strike(hp,(first+i)%2,i,dice(&random,&fight,i).unwrap()).unwrap();
            bytes.extend(borsh::to_vec(&s).unwrap()); hp = s.hp;
            if hp.contains(&0) { break; }
        }
        let hex = |b: &[u8]| b.iter().map(|x| format!("{:02x}",x)).collect::<String>();
        println!("{{\"address\":\"{}\",\"random\":\"{}\",\"first\":{},\"strikes\":\"{}\"}}{}",fight,hex(&random),first,hex(&bytes),if seed<3 {","} else {""});
    }
    println!("]");
}
