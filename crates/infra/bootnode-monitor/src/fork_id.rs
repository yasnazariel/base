//! Fork ID constants, parsing, and network tag classification for ENR records.

use discv5::Enr;
use unsigned_varint::decode as varint_decode;

/// Base Sepolia fork hash (Azul era).
pub const BASE_SEPOLIA_FORK_HASH: [u8; 4] = [0xa4, 0x19, 0xb1, 0xda];
/// Base Sepolia fork hash (Jovian era).
pub const BASE_SEPOLIA_FORK_HASH_JOVIAN: [u8; 4] = [0xce, 0x48, 0x4a, 0x55];
/// Base Mainnet fork hash (Jovian era).
pub const BASE_MAINNET_FORK_HASH_JOVIAN: [u8; 4] = [0x1c, 0xfe, 0xaf, 0xc9];
/// Base Mainnet fork hash (Azul era).
pub const BASE_MAINNET_FORK_HASH_AZUL: [u8; 4] = [0x1b, 0x2c, 0x5c, 0xdf];
/// Base Zeronet fork hash (Jovian era, current as of 2026-04).
pub const BASE_ZERONET_FORK_HASH_JOVIAN: [u8; 4] = [0x44, 0x12, 0x5f, 0xac];
/// Base Zeronet fork hash (Azul era, activates 2027-03).
pub const BASE_ZERONET_FORK_HASH_AZUL: [u8; 4] = [0x30, 0xd7, 0x39, 0xc2];

const ETH_MAINNET_CANCUN: [u8; 4] = [0x9f, 0x3d, 0x22, 0x54];
const ETH_MAINNET_PRAGUE: [u8; 4] = [0xc3, 0x76, 0xcf, 0x8b];
const ETH_SEPOLIA_CANCUN: [u8; 4] = [0x88, 0xcf, 0x81, 0xd9];
const ETH_SEPOLIA_PRAGUE: [u8; 4] = [0xed, 0x88, 0xb5, 0xfd];
const ETH_HOLESKY_CANCUN: [u8; 4] = [0x9b, 0x19, 0x2a, 0xd0];
const ETH_HOLESKY_PRAGUE: [u8; 4] = [0xdf, 0xbd, 0x9b, 0xed];
const ETH_HOODI_PRAGUE: [u8; 4] = [0x09, 0x29, 0xe2, 0x4e];

/// All 256 XOR-distance buckets — querying with all of them returns the full routing table.
pub const ALL_DISTANCES: std::ops::RangeInclusive<u64> = 1..=256;

/// Returns the current fork hash for the given L2 chain ID, or `None` if unknown.
pub fn fork_hash_for_chain(chain_id: u64) -> Option<[u8; 4]> {
    match chain_id {
        8453 => Some(BASE_MAINNET_FORK_HASH_JOVIAN),
        84532 => Some(BASE_SEPOLIA_FORK_HASH),
        763360 => Some(BASE_ZERONET_FORK_HASH_JOVIAN),
        _ => None,
    }
}

fn parse_fork_hash_from_key(enr: &Enr, key: &[u8]) -> Option<[u8; 4]> {
    let raw = enr.get_raw_rlp(key)?;
    if raw.len() >= 6 && raw[0] >= 0xc0 && raw[1] == 0x84 {
        return raw[2..6].try_into().ok();
    }
    if raw.len() >= 7 && raw[0] >= 0xc0 && raw[1] >= 0xc0 && raw[2] == 0x84 {
        return raw[3..7].try_into().ok();
    }
    None
}

/// Decodes `opstack` as RLP bytes wrapping two unsigned varints: `chain_id`, `version`.
fn parse_chain_id_from_opstack(enr: &Enr) -> Option<u64> {
    let raw = enr.get_raw_rlp(b"opstack")?;
    if raw.is_empty() || raw[0] < 0x81 || raw[0] > 0xb7 {
        return None;
    }
    let len = (raw[0] - 0x80) as usize;
    if raw.len() < 1 + len {
        return None;
    }
    varint_decode::u64(&raw[1..1 + len]).ok().map(|(id, _)| id)
}

/// Returns a stable tag string identifying the network from an ENR's fork ID or chain ID.
pub fn network_tag(enr: &Enr) -> &'static str {
    let opel = parse_fork_hash_from_key(enr, b"opel");
    let eth = parse_fork_hash_from_key(enr, b"eth");

    match (opel, eth) {
        (Some(h), _) if h == BASE_SEPOLIA_FORK_HASH => "base-sepolia/azul",
        (Some(h), _) if h == BASE_SEPOLIA_FORK_HASH_JOVIAN => "base-sepolia/jovian",
        (Some(h), _) if h == BASE_MAINNET_FORK_HASH_JOVIAN => "base-mainnet/jovian",
        (Some(h), _) if h == BASE_MAINNET_FORK_HASH_AZUL => "base-mainnet/azul",
        (Some(h), _) if h == BASE_ZERONET_FORK_HASH_JOVIAN => "base-zeronet/jovian",
        (Some(h), _) if h == BASE_ZERONET_FORK_HASH_AZUL => "base-zeronet/azul",
        (Some(_), _) => "opstack-unknown",
        (None, Some(h)) if h == ETH_MAINNET_CANCUN || h == ETH_MAINNET_PRAGUE => "eth-mainnet",
        (None, Some(h)) if h == ETH_SEPOLIA_CANCUN || h == ETH_SEPOLIA_PRAGUE => "eth-sepolia",
        (None, Some(h)) if h == ETH_HOLESKY_CANCUN || h == ETH_HOLESKY_PRAGUE => "eth-holesky",
        (None, Some(h)) if h == ETH_HOODI_PRAGUE => "eth-hoodi",
        (None, Some(_)) => "eth-unknown",
        (None, None) => {
            match parse_chain_id_from_opstack(enr) {
                Some(8453) => "base-mainnet/jovian",
                Some(84532) => "base-sepolia/azul",
                Some(763360) => "base-zeronet/jovian",
                Some(_) => "opstack-unknown",
                None => "no-fork-id",
            }
        }
    }
}
