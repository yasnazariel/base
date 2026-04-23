//! Parsing enode:// URLs into discv5-compatible Multiaddr values.

use std::net::Ipv4Addr;

use discv5::enr::k256::elliptic_curve::sec1::ToEncodedPoint;
use discv5::multiaddr::{Multiaddr, Protocol};

/// Parse an `enode://PUBKEY@IP:PORT` string into a `Multiaddr` suitable for discv5 `request_enr`.
pub fn enode_to_multiaddr(enode: &str) -> anyhow::Result<(Multiaddr, Ipv4Addr, u16)> {
    let inner = enode
        .strip_prefix("enode://")
        .ok_or_else(|| anyhow::anyhow!("expected enode:// prefix"))?;
    let (hex_id, addr_str) = inner
        .split_once('@')
        .ok_or_else(|| anyhow::anyhow!("missing @ in enode"))?;

    let id_bytes = hex::decode(hex_id)?;
    anyhow::ensure!(id_bytes.len() == 64, "pubkey must be 64 bytes");

    let mut sec1 = [0u8; 65];
    sec1[0] = 0x04;
    sec1[1..].copy_from_slice(&id_bytes);

    let pubkey = discv5::enr::k256::PublicKey::from_sec1_bytes(&sec1)?;
    let compressed = pubkey.to_encoded_point(true);
    let lp2p_pk =
        discv5::libp2p_identity::secp256k1::PublicKey::try_from_bytes(compressed.as_bytes())?;
    let peer_id = discv5::libp2p_identity::PublicKey::from(lp2p_pk).to_peer_id();

    let (ip_str, port_str) = addr_str
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("missing port in enode addr"))?;
    let ip: Ipv4Addr = ip_str.parse()?;
    let port: u16 = port_str.parse()?;

    let multiaddr = Multiaddr::empty()
        .with(Protocol::Ip4(ip))
        .with(Protocol::Udp(port))
        .with(Protocol::P2p(peer_id));

    Ok((multiaddr, ip, port))
}
