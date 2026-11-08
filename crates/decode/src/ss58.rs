//! SS58 address encoding, used to show `AccountId32` map keys.

use sp_crypto_hashing::blake2_512;

/// Encodes a 32-byte account id as an SS58 address with the given network prefix.
pub fn encode(prefix: u16, account: &[u8; 32]) -> String {
    let mut data = match prefix {
        0..=63 => vec![prefix as u8],
        // Two-byte form for prefixes 64..=16383 (see the SS58 spec).
        _ => vec![
            ((prefix & 0b1111_1100) >> 2) as u8 | 0b0100_0000,
            (prefix >> 8) as u8 | ((prefix & 0b11) << 6) as u8,
        ],
    };
    data.extend_from_slice(account);
    let hash = blake2_512(&[b"SS58PRE".as_slice(), &data].concat());
    data.extend_from_slice(&hash[..2]);
    bs58::encode(data).into_string()
}
