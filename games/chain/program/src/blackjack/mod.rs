//! Bank-backed ten-seat blackjack. Native account instructions are added alongside
//! the rules engine; this module does not introduce another bank or token mint.
pub mod rules;
mod rewards;
mod state;
mod instructions;
pub mod sol_funding;
pub mod magicblock;
pub mod network_funding;
pub use instructions::process;
pub use state::{Table, Seat, TABLE_LEN};

// Borsh's fixed-array decoder creates several full array temporaries. Decode
// large fixed arrays item-by-item on the heap to stay inside SBF's 4 KiB stack.
fn read_fixed<T: borsh::BorshDeserialize, const N: usize, R: std::io::Read>(reader: &mut R) -> std::io::Result<Box<[T; N]>> {
    let mut values = Vec::with_capacity(N);
    for _ in 0..N { values.push(T::deserialize_reader(reader)?); }
    values.into_boxed_slice().try_into().map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "fixed array length"))
}
