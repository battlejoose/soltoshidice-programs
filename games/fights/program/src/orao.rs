//! Minimal ORAO Classic RequestV2 CPI/decoder, checked against the upstream IDL.
//! Only this fixed program may supply randomness; no caller-supplied oracle ID.
use super::*;
use solana_program::instruction::{AccountMeta, Instruction};

pub const ID: Pubkey = solana_program::pubkey!("VRFzZoJdhFWL8rkvu87LpKM3RbcVezpMEc6X5GVDr7y");
const REQUEST_SEED: &[u8] = b"orao-vrf-randomness-request";
const CONFIG_SEED: &[u8] = b"orao-vrf-network-configuration";
const REQUEST_V2: [u8; 8] = [38, 151, 209, 6, 195, 102, 28, 217];
const RANDOMNESS_V2: [u8; 8] = [139, 239, 184, 215, 227, 86, 191, 226];
const NETWORK_STATE: [u8; 8] = [212, 237, 148, 56, 97, 245, 51, 169];

pub fn request_address(seed: &[u8; 32]) -> Pubkey {
    Pubkey::find_program_address(&[REQUEST_SEED, seed], &ID).0
}

pub fn read_request(account: &AccountInfo, seed: &[u8; 32], client: &Pubkey) -> Result<Option<[u8; 64]>, ProgramError> {
    check(account.owner == &ID && *account.key == request_address(seed), 40)?;
    decode_request(&account.try_borrow_data()?, seed, client)
}

fn decode_request(data: &[u8], seed: &[u8; 32], client: &Pubkey) -> Result<Option<[u8; 64]>, ProgramError> {
    check(data.len() >= 73 && data[..8] == RANDOMNESS_V2, 40)?;
    check(&data[9..41] == client.as_ref() && &data[41..73] == seed, 40)?;
    match data[8] {
        0 => {
            check(data.len() >= 77, 40)?;
            let count = u32::from_le_bytes(data[73..77].try_into().unwrap()) as usize;
            check(count <= 7 && data.len() >= 77 + count * 96, 40)?;
            Ok(None)
        },
        1 => {
            check(data.len() >= 137, 40)?;
            Ok(Some(data[73..137].try_into().unwrap()))
        },
        _ => Err(ProgramError::Custom(40)),
    }
}

pub fn request_fresh<'a>(
    payer: &AccountInfo<'a>, network: &AccountInfo<'a>, treasury: &AccountInfo<'a>,
    request: &AccountInfo<'a>, system: &AccountInfo<'a>, program: &AccountInfo<'a>, seed: &[u8; 32],
) -> ProgramResult {
    request_fresh_signed(payer, network, treasury, request, system, program, seed, None, None)
}
pub(super) fn request_fresh_signed<'a>(
    payer: &AccountInfo<'a>, network: &AccountInfo<'a>, treasury: &AccountInfo<'a>,
    request: &AccountInfo<'a>, system: &AccountInfo<'a>, program: &AccountInfo<'a>, seed: &[u8; 32],
    seeds: Option<&[&[u8]]>,
    reserve: Option<&AccountInfo<'a>>,
) -> ProgramResult {
    if seeds.is_none() { signer(payer)?; }
    check(*program.key == ID && program.executable && *system.key == system_program::id(), 40)?;
    check(network.owner == &ID && *network.key == Pubkey::find_program_address(&[CONFIG_SEED], &ID).0, 40)?;
    {
        let data = network.try_borrow_data()?;
        check(data.len() >= 80 && data[..8] == NETWORK_STATE && &data[40..72] == treasury.key.as_ref(), 40)?;
        // Refuse an unexpected oracle fee increase in this devnet deployment.
        check(u64_at(&data, 72)? <= 20_000_000, 41)?;
    }
    check(*request.key == request_address(seed), 40)?;
    // Never reuse a pending OR fulfilled request. This prevents selecting known dice.
    check(request.owner == &system_program::id() && request.data_is_empty(), 42)?;
    let mut ix = Instruction {
        program_id: ID,
        accounts: vec![
            AccountMeta::new(*payer.key, true), AccountMeta::new(*network.key, false),
            AccountMeta::new(*treasury.key, false), AccountMeta::new(*request.key, false),
            AccountMeta::new_readonly(*system.key, false),
        ],
        data: [REQUEST_V2.as_slice(), seed].concat(),
    };
    let mut infos = vec![payer.clone(), network.clone(), treasury.clone(), request.clone(), system.clone(), program.clone()];
    // Synchronize both sides of the program-owned reserve transfer at the CPI
    // boundary. ORAO ignores remaining accounts; it cannot sign for our reserve.
    if let Some(reserve) = reserve { ix.accounts.push(AccountMeta::new(*reserve.key, false)); infos.push(reserve.clone()); }
    if let Some(seeds) = seeds { invoke_signed(&ix, &infos, &[seeds])?; } else { invoke(&ix, &infos)?; }
    check(read_request(request, seed, payer.key)?.is_none(), 42)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_wrong_type_identity_seed_and_truncation() {
        let client = Pubkey::new_from_array([5; 32]);
        let seed = [7; 32];
        let mut data = vec![0; 137];
        data[..8].copy_from_slice(&RANDOMNESS_V2);
        data[8] = 1;
        data[9..41].copy_from_slice(client.as_ref());
        data[41..73].copy_from_slice(&seed);
        data[73..].fill(9);
        assert_eq!(decode_request(&data, &seed, &client).unwrap(), Some([9; 64]));
        assert!(decode_request(&data, &[8; 32], &client).is_err());
        assert!(decode_request(&data, &seed, &Pubkey::default()).is_err());
        for len in 0..137 { assert!(decode_request(&data[..len], &seed, &client).is_err()); }
        data[8] = 2;
        assert!(decode_request(&data, &seed, &client).is_err());
        data[8] = 0;
        data[73..77].fill(0);
        assert_eq!(decode_request(&data, &seed, &client).unwrap(), None);
        data[0] ^= 1;
        assert!(decode_request(&data, &seed, &client).is_err());
    }
}
