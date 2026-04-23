//! Core bootnode probing: discv5 client construction and routing-table queries.

use std::collections::{HashMap, HashSet};
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use bytes::Bytes;
use discv5::enr::CombinedKey;
use discv5::{ConfigBuilder, Discv5, Enr, ListenConfig};
use tokio::time::timeout;
use tracing::warn;

use crate::enode::enode_to_multiaddr;
use crate::fork_id::{ALL_DISTANCES, network_tag};

/// A single peer discovered in a bootnode's routing table.
#[derive(Debug)]
pub struct PeerEntry {
    /// First 8 hex characters of the node ID (e.g. `"3f8a2c1b"`).
    pub node_id_prefix: String,
    /// `"ip:port"` address string.
    pub address: String,
    /// Network identification tag (e.g. `"base-sepolia/azul"`).
    pub network_tag: &'static str,
    /// Full ENR string representation.
    pub enr: String,
}

/// Result of querying a single bootnode's routing table.
#[derive(Debug)]
pub struct BootnodeResult {
    /// Display label for this bootnode (e.g. `"18.210.176.114:9200"`).
    pub label: String,
    /// Whether the bootnode responded successfully.
    pub reachable: bool,
    /// Number of peers returned by the routing table query.
    pub peer_count: usize,
    /// Query round-trip time in milliseconds.
    pub query_ms: u64,
    /// Error description if the query failed.
    pub error: Option<String>,
}

/// A point-in-time snapshot of bootnode and DHT peer state.
#[derive(Debug)]
pub struct BootnodeSnapshot {
    /// Network name (e.g. `"sepolia"`).
    pub network_name: String,
    /// Per-bootnode query results, in the order queried.
    pub bootnodes: Vec<BootnodeResult>,
    /// All unique peers discovered across all bootnode routing tables.
    pub peers: Vec<PeerEntry>,
    /// Count of peers by network tag.
    pub network_counts: HashMap<&'static str, usize>,
    /// Number of active discv5 sessions at snapshot time.
    pub connected_peers: usize,
    /// When this snapshot was collected.
    pub queried_at: Instant,
}

/// Manages a discv5 instance for probing bootnode routing tables.
pub struct BootnodeProber {
    disc: Discv5,
    enr_cache: HashMap<String, Enr>,
}

impl std::fmt::Debug for BootnodeProber {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BootnodeProber")
            .field("enr_cache_len", &self.enr_cache.len())
            .finish_non_exhaustive()
    }
}

impl BootnodeProber {
    /// Creates a discv5 instance with the given fork hash embedded in its ENR.
    pub async fn new(fork_hash: [u8; 4]) -> anyhow::Result<Self> {
        let port = free_udp_port()?;
        let key = CombinedKey::generate_secp256k1();
        let [h0, h1, h2, h3] = fork_hash;
        let fork_id_rlp = Bytes::from(vec![0xc6, 0x84, h0, h1, h2, h3, 0x80]);
        let mut builder = Enr::builder();
        builder.udp4(port);
        builder.add_value_rlp(b"opel", fork_id_rlp);
        let local_enr = builder.build(&key).map_err(|e| anyhow::anyhow!("ENR build: {e}"))?;
        let config =
            ConfigBuilder::new(ListenConfig::Ipv4 { ip: Ipv4Addr::UNSPECIFIED, port }).build();
        let mut disc =
            Discv5::new(local_enr, key, config).map_err(|e| anyhow::anyhow!("discv5::new: {e}"))?;
        disc.start().await.map_err(|e| anyhow::anyhow!("discv5::start: {e:?}"))?;
        Ok(Self { disc, enr_cache: HashMap::new() })
    }

    /// Queries all listed bootnodes and returns a unified snapshot.
    pub async fn probe_all(
        &mut self,
        network_name: &str,
        bootnodes: &[String],
    ) -> BootnodeSnapshot {
        let mut bootnode_results = Vec::new();
        let mut seen: HashSet<discv5::enr::NodeId> = HashSet::new();
        let mut all_peers: Vec<PeerEntry> = Vec::new();
        let mut network_counts: HashMap<&'static str, usize> = HashMap::new();

        for bootnode_str in bootnodes {
            let (label, enr_opt) = self.resolve_bootnode(bootnode_str).await;

            let Some(enr) = enr_opt else {
                bootnode_results.push(BootnodeResult {
                    label,
                    reachable: false,
                    peer_count: 0,
                    query_ms: 0,
                    error: Some("failed to resolve ENR".to_string()),
                });
                continue;
            };

            let start = Instant::now();
            let query_result = timeout(
                Duration::from_secs(5),
                self.disc.find_node_designated_peer(enr.clone(), ALL_DISTANCES.collect()),
            )
            .await;
            let query_ms = start.elapsed().as_millis() as u64;

            match query_result {
                Ok(Ok(nodes)) => {
                    let peer_count = nodes.len();
                    for n in nodes {
                        let _ = self.disc.add_enr(n.clone());
                        let node_id = n.node_id();
                        if seen.insert(node_id) {
                            let tag = network_tag(&n);
                            *network_counts.entry(tag).or_insert(0) += 1;
                            let raw = node_id.raw();
                            let prefix = format!(
                                "{:02x}{:02x}{:02x}{:02x}",
                                raw[0], raw[1], raw[2], raw[3]
                            );
                            let addr = n
                                .ip4()
                                .map(|ip| format!("{}:{}", ip, n.udp4().unwrap_or(0)))
                                .unwrap_or_else(|| "unknown".to_string());
                            all_peers.push(PeerEntry {
                                node_id_prefix: prefix,
                                address: addr,
                                network_tag: tag,
                                enr: n.to_string(),
                            });
                        }
                    }
                    bootnode_results.push(BootnodeResult {
                        label,
                        reachable: true,
                        peer_count,
                        query_ms,
                        error: None,
                    });
                }
                Ok(Err(e)) => {
                    bootnode_results.push(BootnodeResult {
                        label,
                        reachable: false,
                        peer_count: 0,
                        query_ms,
                        error: Some(format!("{e}")),
                    });
                }
                Err(_) => {
                    bootnode_results.push(BootnodeResult {
                        label,
                        reachable: false,
                        peer_count: 0,
                        query_ms,
                        error: Some("timed out".to_string()),
                    });
                }
            }
        }

        BootnodeSnapshot {
            network_name: network_name.to_string(),
            bootnodes: bootnode_results,
            peers: all_peers,
            network_counts,
            connected_peers: self.disc.connected_peers(),
            queried_at: Instant::now(),
        }
    }

    /// Resolves a bootnode string to an ENR, using a cache for subsequent calls.
    ///
    /// Returns `(display_label, Option<Enr>)`.
    async fn resolve_bootnode(&mut self, bootnode_str: &str) -> (String, Option<Enr>) {
        if let Some(cached) = self.enr_cache.get(bootnode_str) {
            let label = cached
                .ip4()
                .map(|ip| format!("{}:{}", ip, cached.udp4().unwrap_or(0)))
                .unwrap_or_else(|| short_label(bootnode_str));
            return (label, Some(cached.clone()));
        }

        if bootnode_str.starts_with("enr:") {
            match bootnode_str.parse::<Enr>() {
                Ok(enr) => {
                    let label = enr
                        .ip4()
                        .map(|ip| format!("{}:{}", ip, enr.udp4().unwrap_or(0)))
                        .unwrap_or_else(|| short_label(bootnode_str));
                    let _ = self.disc.add_enr(enr.clone());
                    self.enr_cache.insert(bootnode_str.to_string(), enr.clone());
                    (label, Some(enr))
                }
                Err(e) => {
                    warn!(error = %e, bootnode = %bootnode_str, "failed to parse ENR");
                    (short_label(bootnode_str), None)
                }
            }
        } else if bootnode_str.starts_with("enode://") {
            let label = bootnode_str.split('@').nth(1).unwrap_or(bootnode_str).to_string();
            match enode_to_multiaddr(bootnode_str) {
                Ok((multiaddr, _, _)) => {
                    match timeout(Duration::from_secs(5), self.disc.request_enr(multiaddr)).await {
                        Ok(Ok(enr)) => {
                            let display = enr
                                .ip4()
                                .map(|ip| format!("{}:{}", ip, enr.udp4().unwrap_or(0)))
                                .unwrap_or(label.clone());
                            let _ = self.disc.add_enr(enr.clone());
                            self.enr_cache.insert(bootnode_str.to_string(), enr.clone());
                            (display, Some(enr))
                        }
                        Ok(Err(e)) => {
                            warn!(error = %e, bootnode = %bootnode_str, "discv5 request_enr failed");
                            (label, None)
                        }
                        Err(_) => {
                            warn!(bootnode = %bootnode_str, "discv5 request_enr timed out");
                            (label, None)
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, bootnode = %bootnode_str, "enode parse failed");
                    (label, None)
                }
            }
        } else {
            (short_label(bootnode_str), None)
        }
    }
}

fn free_udp_port() -> anyhow::Result<u16> {
    let sock = std::net::UdpSocket::bind("0.0.0.0:0")?;
    Ok(sock.local_addr()?.port())
}

fn short_label(s: &str) -> String {
    s.chars().take(24).collect()
}
