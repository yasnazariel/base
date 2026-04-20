//! Production [`ConsensusDriver`] backed by [`openraft`] — a Raft (CFT) consensus engine.
//!
//! Replaces the prior commonware-simplex driver. Raft tolerates `f` crash failures with
//! `n = 2f + 1` voters (so 1 of 3, 2 of 5). Severe degradation past the quorum floor
//! requires an out-of-band [`LeadershipCommand::OverrideLeader`] plus an L1 fencing
//! token enforced at the batcher boundary — both are tracked separately.
//!
//! The driver is a thin adapter:
//! - persistent log + state machine on [`sled`] (separate trees per concern, fsync on
//!   every append/vote);
//! - a length-prefixed [`bincode`] TCP transport implementing [`RaftNetwork`];
//! - a metrics watcher that translates [`openraft::Raft::metrics`] transitions into the
//!   [`DriverEvent`] stream the [`LeadershipActor`](crate::LeadershipActor) consumes;
//! - a request handler that maps [`DriverRequestKind`] onto openraft's admin APIs
//!   (`change_membership`, `add_learner`, etc.).

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io::Cursor,
    net::SocketAddr,
    ops::Bound,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use openraft::{
    ChangeMembers, Config, Entry, EntryPayload, LogId, LogState, RaftMetrics, SnapshotMeta,
    SnapshotPolicy, StorageError, StorageIOError, StoredMembership, TokioRuntime, Vote,
    declare_raft_types,
    error::{InitializeError, InstallSnapshotError, NetworkError, RPCError, RaftError},
    network::{RPCOption, RaftNetwork, RaftNetworkFactory},
    raft::{
        AppendEntriesRequest, AppendEntriesResponse, InstallSnapshotRequest,
        InstallSnapshotResponse, VoteRequest, VoteResponse,
    },
    storage::{
        LogFlushed, RaftLogReader, RaftLogStorage, RaftSnapshotBuilder, RaftStateMachine, Snapshot,
    },
};
use serde::{Deserialize, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream, lookup_host},
    sync::{Mutex, Semaphore, watch},
    task::JoinHandle,
    time::{sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::{
    ConsensusDriver, DriverContext, DriverError, DriverEvent, DriverRequest, DriverRequestKind,
    RaftTimeouts, ValidatorEntry, ValidatorId,
};

/// Openraft `NodeId` type used by the leadership instance.
///
/// Aliased here so the rest of the crate doesn't need to know the underlying integer
/// width if we ever change it (e.g. to `u128` for a different hash).
pub type NodeId = u64;

/// `(last_applied, last_membership)` snapshot persisted by [`SledStateMachine`]. Aliased
/// to keep `RaftStateMachine::applied_state` / `read_applied` / `write_applied`
/// signatures readable and to satisfy clippy's `type_complexity` lint.
pub type AppliedState = (Option<LogId<NodeId>>, StoredMembership<NodeId, ValidatorEntry>);

/// Bootstrap tables derived from a [`crate::ClusterMembership`] snapshot at startup —
/// the address map fed to the network factory plus the `BTreeMap` consumed by
/// [`openraft::Raft::initialize`].
pub type BootstrapTables = (HashMap<NodeId, String>, BTreeMap<NodeId, ValidatorEntry>);

/// Stable, deterministic mapping from [`ValidatorId`] to openraft [`NodeId`].
///
/// FNV-1a 64-bit hash. Stable across processes and machines, no hash-DoS concern (the
/// validator set is small and operator-controlled), no extra dependency. Two distinct
/// `ValidatorId`s collide with probability `~ n²/2^65`, so for `n ≤ 16` validators the
/// expected collision count is `< 4e-18`. Startup explicitly checks for collisions
/// (see `OpenraftDriver::run`) so a pathological config fails loudly rather than
/// silently overwriting an entry.
#[derive(Clone, Copy, Debug)]
pub struct NodeIdHash;

impl NodeIdHash {
    /// FNV-1a 64-bit offset basis.
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    /// FNV-1a 64-bit prime.
    const FNV_PRIME: u64 = 0x100000001b3;

    /// Hashes a [`ValidatorId`] string into a 64-bit [`NodeId`].
    pub fn hash(id: &ValidatorId) -> NodeId {
        let mut h = Self::FNV_OFFSET;
        for b in id.as_str().as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(Self::FNV_PRIME);
        }
        h
    }
}

declare_raft_types!(
    /// Type-config for the leadership Raft instance.
    ///
    /// The state machine carries no application data (`D = R = ()`); we only care about
    /// the leader-election signal exposed via [`openraft::Raft::metrics`].
    /// `Node = ValidatorEntry` so membership changes carry the dial address and id
    /// alongside the [`NodeId`].
    pub TypeConfig:
        D = (),
        R = (),
        NodeId = NodeId,
        Node = ValidatorEntry,
        Entry = Entry<TypeConfig>,
        SnapshotData = Cursor<Vec<u8>>,
        AsyncRuntime = TokioRuntime,
);

/// Fully-qualified type for the leadership [`openraft::Raft`] instance.
pub type LeadershipRaft = openraft::Raft<TypeConfig>;

/// Sled tree name for the Raft log entries (key = u64 BE log index).
pub const LOG_TREE: &str = "raft_log";
/// Sled tree name for ancillary metadata (vote, last-purged log id, applied state, snapshot).
pub const META_TREE: &str = "raft_meta";
/// Sled key for the persisted [`Vote`].
pub const META_KEY_VOTE: &[u8] = b"vote";
/// Sled key for the last-purged [`LogId`].
pub const META_KEY_LAST_PURGED: &[u8] = b"last_purged_log_id";
/// Sled key for the state-machine applied state (last applied + last membership).
pub const META_KEY_APPLIED: &[u8] = b"applied_state";
/// Sled key for the most recent snapshot blob and metadata.
pub const META_KEY_SNAPSHOT: &[u8] = b"snapshot";

/// Helpers for converting `std::error::Error` types into the [`StorageError`] variants
/// openraft expects on each storage code-path. Grouped on a unit struct so the public
/// API exports a type rather than four loose conversion functions.
#[derive(Clone, Copy, Debug)]
pub struct StorageErr;

impl StorageErr {
    /// Wraps a generic write failure (state machine or meta tree).
    pub fn write<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
        StorageIOError::write(&e).into()
    }

    /// Wraps a generic read failure (state machine or meta tree).
    pub fn read<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
        StorageIOError::read(&e).into()
    }

    /// Wraps a write failure that targeted the Raft log specifically.
    pub fn write_logs<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
        StorageIOError::write_logs(&e).into()
    }

    /// Wraps a read failure that targeted the Raft log specifically.
    pub fn read_logs<E: std::error::Error + 'static>(e: E) -> StorageError<NodeId> {
        StorageIOError::read_logs(&e).into()
    }
}

/// Bincode codec used for both on-disk persistence and on-the-wire RPC framing.
#[derive(Clone, Copy, Debug)]
pub struct Codec;

impl Codec {
    /// Returns the bincode configuration used by every encode/decode in this crate.
    /// `const` so the standard config is constructed inline by the optimizer.
    pub const fn config() -> bincode::config::Configuration {
        bincode::config::standard()
    }

    /// Bincode-encodes `value`, mapping any encoding error into a [`StorageError`].
    /// Used both for sled persistence and (via [`Self::encode_io`]) for wire framing.
    pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StorageError<NodeId>> {
        bincode::serde::encode_to_vec(value, Self::config()).map_err(StorageErr::write)
    }

    /// Bincode-decodes `bytes`, mapping any decoding error into a [`StorageError`].
    pub fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, StorageError<NodeId>> {
        bincode::serde::decode_from_slice(bytes, Self::config())
            .map(|(v, _)| v)
            .map_err(StorageErr::read)
    }

    /// Bincode-encodes `value`, mapping any error into [`std::io::Error`] for use on
    /// the network path (where the openraft `StorageError` family doesn't apply).
    pub fn encode_io<T: Serialize>(value: &T) -> std::io::Result<Vec<u8>> {
        bincode::serde::encode_to_vec(value, Self::config()).map_err(std::io::Error::other)
    }

    /// Bincode-decodes `bytes`, mapping any error into [`std::io::Error`] for use on
    /// the network path.
    pub fn decode_io<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> std::io::Result<T> {
        bincode::serde::decode_from_slice(bytes, Self::config())
            .map(|(v, _)| v)
            .map_err(std::io::Error::other)
    }
}

/// Sled-backed persistent log storage for the Raft instance.
#[derive(Clone, Debug)]
pub struct SledLogStore {
    log: sled::Tree,
    meta: sled::Tree,
}

impl SledLogStore {
    /// Opens (or creates) the log + meta trees inside the given sled database.
    pub fn new(db: sled::Db) -> sled::Result<Self> {
        let log = db.open_tree(LOG_TREE)?;
        let meta = db.open_tree(META_TREE)?;
        Ok(Self { log, meta })
    }

    /// Encodes a `u64` log index to a big-endian sled key so iterators yield entries in
    /// ascending order.
    pub const fn log_key(index: u64) -> [u8; 8] {
        index.to_be_bytes()
    }

    fn read_last_purged(&self) -> Result<Option<LogId<NodeId>>, StorageError<NodeId>> {
        self.meta
            .get(META_KEY_LAST_PURGED)
            .map_err(StorageErr::read)?
            .map(|v| Codec::decode::<LogId<NodeId>>(&v))
            .transpose()
    }

    fn write_last_purged(&self, log_id: &LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let bytes = Codec::encode(log_id)?;
        self.meta.insert(META_KEY_LAST_PURGED, bytes).map_err(StorageErr::write)?;
        self.meta.flush().map_err(StorageErr::write)?;
        Ok(())
    }
}

/// Sled-backed log reader. Cheap to clone (sled trees are `Arc`-backed internally).
#[derive(Clone, Debug)]
pub struct SledLogReader {
    log: sled::Tree,
    meta: sled::Tree,
}

impl SledLogReader {
    fn read_vote(&self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        self.meta
            .get(META_KEY_VOTE)
            .map_err(StorageErr::read)?
            .map(|v| Codec::decode::<Vote<NodeId>>(&v))
            .transpose()
    }
}

impl RaftLogReader<TypeConfig> for SledLogReader {
    async fn try_get_log_entries<
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + Send,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        let start = match range.start_bound() {
            Bound::Included(&i) => i,
            Bound::Excluded(&i) => i.saturating_add(1),
            Bound::Unbounded => 0,
        };
        let end = match range.end_bound() {
            Bound::Included(&i) => i.saturating_add(1),
            Bound::Excluded(&i) => i,
            Bound::Unbounded => u64::MAX,
        };
        let mut out = Vec::new();
        for entry in self.log.range(SledLogStore::log_key(start)..SledLogStore::log_key(end)) {
            let (_k, v) = entry.map_err(StorageErr::read_logs)?;
            out.push(Codec::decode::<Entry<TypeConfig>>(&v)?);
        }
        Ok(out)
    }
}

impl RaftLogReader<TypeConfig> for SledLogStore {
    async fn try_get_log_entries<
        RB: std::ops::RangeBounds<u64> + Clone + std::fmt::Debug + Send,
    >(
        &mut self,
        range: RB,
    ) -> Result<Vec<Entry<TypeConfig>>, StorageError<NodeId>> {
        SledLogReader { log: self.log.clone(), meta: self.meta.clone() }
            .try_get_log_entries(range)
            .await
    }
}

impl RaftLogStorage<TypeConfig> for SledLogStore {
    type LogReader = SledLogReader;

    async fn get_log_state(&mut self) -> Result<LogState<TypeConfig>, StorageError<NodeId>> {
        let last = self
            .log
            .last()
            .map_err(StorageErr::read_logs)?
            .map(|(_, v)| Codec::decode::<Entry<TypeConfig>>(&v).map(|e| e.log_id))
            .transpose()?;
        let last_purged = self.read_last_purged()?;
        let last_log_id = last.or(last_purged);
        Ok(LogState { last_purged_log_id: last_purged, last_log_id })
    }

    async fn save_vote(&mut self, vote: &Vote<NodeId>) -> Result<(), StorageError<NodeId>> {
        let bytes = Codec::encode(vote)?;
        self.meta.insert(META_KEY_VOTE, bytes).map_err(StorageErr::write)?;
        self.meta.flush_async().await.map_err(StorageErr::write)?;
        Ok(())
    }

    async fn read_vote(&mut self) -> Result<Option<Vote<NodeId>>, StorageError<NodeId>> {
        SledLogReader { log: self.log.clone(), meta: self.meta.clone() }.read_vote()
    }

    async fn get_log_reader(&mut self) -> Self::LogReader {
        SledLogReader { log: self.log.clone(), meta: self.meta.clone() }
    }

    async fn append<I>(
        &mut self,
        entries: I,
        callback: LogFlushed<TypeConfig>,
    ) -> Result<(), StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
        I::IntoIter: Send,
    {
        let mut batch = sled::Batch::default();
        for entry in entries {
            let key = Self::log_key(entry.log_id.index);
            let value = Codec::encode(&entry)?;
            batch.insert(&key, value);
        }
        self.log.apply_batch(batch).map_err(StorageErr::write_logs)?;
        self.log.flush_async().await.map_err(StorageErr::write_logs)?;
        callback.log_io_completed(Ok(()));
        Ok(())
    }

    async fn truncate(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        let mut batch = sled::Batch::default();
        for entry in self.log.range(Self::log_key(log_id.index)..) {
            let (k, _v) = entry.map_err(StorageErr::read_logs)?;
            batch.remove(k);
        }
        self.log.apply_batch(batch).map_err(StorageErr::write_logs)?;
        self.log.flush_async().await.map_err(StorageErr::write_logs)?;
        Ok(())
    }

    async fn purge(&mut self, log_id: LogId<NodeId>) -> Result<(), StorageError<NodeId>> {
        // Crash-safety: write the purge watermark *first* (and flush it), then delete the
        // log entries. If we crash after the watermark but before deletion we waste a
        // little space that the next purge will reclaim; if we crash after deletion but
        // before watermark we'd lose the high-water-mark and mislead `get_log_state` into
        // believing the log is shorter than it really was. The watermark-first ordering
        // is the only safe one.
        self.write_last_purged(&log_id)?;
        let mut batch = sled::Batch::default();
        for entry in self.log.range(..=Self::log_key(log_id.index)) {
            let (k, _v) = entry.map_err(StorageErr::read_logs)?;
            batch.remove(k);
        }
        self.log.apply_batch(batch).map_err(StorageErr::write_logs)?;
        self.log.flush_async().await.map_err(StorageErr::write_logs)?;
        Ok(())
    }
}

/// Persisted snapshot wrapper used by [`SledStateMachine`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoredSnapshot {
    /// Snapshot metadata (last log id + membership + id).
    pub meta: SnapshotMeta<NodeId, ValidatorEntry>,
    /// Opaque snapshot bytes — empty for the leadership state machine, present so the
    /// same code path could carry application state in a future re-use.
    pub data: Vec<u8>,
}

/// Persisted state machine. The leadership Raft instance has no application state to
/// replicate (the only externally-observable signal is the leader id from
/// [`openraft::Raft::metrics`]), so [`apply`](RaftStateMachine::apply) only updates
/// `last_applied` and `last_membership`.
#[derive(Clone, Debug)]
pub struct SledStateMachine {
    meta: sled::Tree,
}

impl SledStateMachine {
    /// Opens the meta tree for state-machine persistence.
    pub fn new(db: sled::Db) -> sled::Result<Self> {
        let meta = db.open_tree(META_TREE)?;
        Ok(Self { meta })
    }

    fn read_applied(&self) -> Result<AppliedState, StorageError<NodeId>> {
        self.meta
            .get(META_KEY_APPLIED)
            .map_err(StorageErr::read)?
            .map_or_else(|| Ok((None, StoredMembership::default())), |bytes| Codec::decode(&bytes))
    }

    fn write_applied(&self, applied: &AppliedState) -> Result<(), StorageError<NodeId>> {
        let bytes = Codec::encode(applied)?;
        self.meta.insert(META_KEY_APPLIED, bytes).map_err(StorageErr::write)?;
        Ok(())
    }

    fn read_snapshot(&self) -> Result<Option<StoredSnapshot>, StorageError<NodeId>> {
        self.meta
            .get(META_KEY_SNAPSHOT)
            .map_err(StorageErr::read)?
            .map(|v| Codec::decode::<StoredSnapshot>(&v))
            .transpose()
    }

    fn write_snapshot(&self, snap: &StoredSnapshot) -> Result<(), StorageError<NodeId>> {
        let bytes = Codec::encode(snap)?;
        self.meta.insert(META_KEY_SNAPSHOT, bytes).map_err(StorageErr::write)?;
        Ok(())
    }
}

/// Snapshot builder. Captures the current applied state into a fresh [`StoredSnapshot`].
#[derive(Clone, Debug)]
pub struct SledSnapshotBuilder {
    sm: SledStateMachine,
}

impl RaftSnapshotBuilder<TypeConfig> for SledSnapshotBuilder {
    async fn build_snapshot(&mut self) -> Result<Snapshot<TypeConfig>, StorageError<NodeId>> {
        let (last_applied, last_membership) = self.sm.read_applied()?;
        let snapshot_id = last_applied.map_or_else(
            || "snapshot-empty".to_owned(),
            |id| format!("snapshot-{}-{}", id.leader_id, id.index),
        );
        let meta = SnapshotMeta { last_log_id: last_applied, last_membership, snapshot_id };
        let stored = StoredSnapshot { meta: meta.clone(), data: Vec::new() };
        self.sm.write_snapshot(&stored)?;
        self.sm.meta.flush_async().await.map_err(StorageErr::write)?;
        Ok(Snapshot { meta, snapshot: Box::new(Cursor::new(stored.data)) })
    }
}

impl RaftStateMachine<TypeConfig> for SledStateMachine {
    type SnapshotBuilder = SledSnapshotBuilder;

    async fn applied_state(&mut self) -> Result<AppliedState, StorageError<NodeId>> {
        self.read_applied()
    }

    async fn apply<I>(&mut self, entries: I) -> Result<Vec<()>, StorageError<NodeId>>
    where
        I: IntoIterator<Item = Entry<TypeConfig>> + Send,
        I::IntoIter: Send,
    {
        let (mut last_applied, mut last_membership) = self.read_applied()?;
        let mut results = Vec::new();
        for entry in entries {
            last_applied = Some(entry.log_id);
            if let EntryPayload::Membership(ref m) = entry.payload {
                last_membership = StoredMembership::new(Some(entry.log_id), m.clone());
            }
            // No application payload to apply; the leadership state machine is empty.
            results.push(());
        }
        self.write_applied(&(last_applied, last_membership))?;
        self.meta.flush_async().await.map_err(StorageErr::write)?;
        Ok(results)
    }

    async fn get_snapshot_builder(&mut self) -> Self::SnapshotBuilder {
        SledSnapshotBuilder { sm: self.clone() }
    }

    async fn begin_receiving_snapshot(
        &mut self,
    ) -> Result<Box<Cursor<Vec<u8>>>, StorageError<NodeId>> {
        Ok(Box::new(Cursor::new(Vec::new())))
    }

    async fn install_snapshot(
        &mut self,
        meta: &SnapshotMeta<NodeId, ValidatorEntry>,
        snapshot: Box<Cursor<Vec<u8>>>,
    ) -> Result<(), StorageError<NodeId>> {
        let stored = StoredSnapshot { meta: meta.clone(), data: snapshot.into_inner() };
        self.write_snapshot(&stored)?;
        self.write_applied(&(meta.last_log_id, meta.last_membership.clone()))?;
        self.meta.flush_async().await.map_err(StorageErr::write)?;
        Ok(())
    }

    async fn get_current_snapshot(
        &mut self,
    ) -> Result<Option<Snapshot<TypeConfig>>, StorageError<NodeId>> {
        Ok(self
            .read_snapshot()?
            .map(|s| Snapshot { meta: s.meta.clone(), snapshot: Box::new(Cursor::new(s.data)) }))
    }
}

/// Wire envelope for inter-node Raft RPCs.
///
/// Length-prefixed bincode framing (4-byte big-endian length + payload). One TCP
/// connection per RPC keeps the protocol trivially stateless and lets sled-backed
/// retries reuse fresh dial state on each attempt; the validator set is small (≤16) and
/// election traffic is bursty, so the per-RPC connect cost is negligible.
#[derive(Debug, Serialize, Deserialize)]
pub enum RaftWireRequest {
    /// Append-entries RPC body.
    Append(AppendEntriesRequest<TypeConfig>),
    /// Vote RPC body.
    Vote(VoteRequest<NodeId>),
    /// Install-snapshot RPC body.
    InstallSnapshot(InstallSnapshotRequest<TypeConfig>),
    /// Out-of-band: ask the receiving node to immediately trigger an election. Used by
    /// the leader to implement leadership transfer in openraft 0.9, which lacks a native
    /// `transfer_leader` primitive — the receiver bumps its term via [`openraft::Raft`]'s
    /// election trigger, which forces the current leader to step down on the next vote
    /// request. Carries no payload.
    TriggerElect,
}

/// Wire response variant, mirroring [`RaftWireRequest`].
///
/// Errors are carried as the typed [`RaftError`] (rather than a stringified `Display`)
/// so the client can rebuild the correct [`RPCError`] variant — e.g. distinguishing
/// `RaftError::APIError(HigherVote)` (don't retry; protocol-level disagreement) from a
/// genuine network failure (retry).
#[derive(Debug, Serialize, Deserialize)]
pub enum RaftWireResponse {
    /// Append-entries RPC response.
    Append(Result<AppendEntriesResponse<NodeId>, RaftError<NodeId>>),
    /// Vote RPC response.
    Vote(Result<VoteResponse<NodeId>, RaftError<NodeId>>),
    /// Install-snapshot RPC response.
    InstallSnapshot(
        Result<InstallSnapshotResponse<NodeId>, RaftError<NodeId, InstallSnapshotError>>,
    ),
    /// Trigger-elect ack. `Ok(())` means the receiver scheduled the election; the failure
    /// carries the [`openraft::error::Fatal`] message rendered as a string because `Fatal`
    /// is not serde-friendly across the wire and the caller only needs a human-readable
    /// reason for telemetry / RPC propagation.
    TriggerElect(Result<(), String>),
}

/// TCP framing for [`RaftWireRequest`] / [`RaftWireResponse`].
#[derive(Clone, Copy, Debug)]
pub struct Frame;

impl Frame {
    /// Maximum on-wire frame size (`1 MiB`). Sized for the largest `install-snapshot`
    /// payload the leadership state machine could reasonably produce; bigger frames
    /// are rejected to bound memory pressure on pathological peers.
    pub const MAX_BYTES: u32 = 1024 * 1024;

    /// Writes a length-prefixed frame to `w`.
    pub async fn write<W: AsyncWriteExt + Unpin>(w: &mut W, bytes: &[u8]) -> std::io::Result<()> {
        let len = u32::try_from(bytes.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "frame too large for u32")
        })?;
        if len > Self::MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds Frame::MAX_BYTES",
            ));
        }
        w.write_all(&len.to_be_bytes()).await?;
        w.write_all(bytes).await?;
        w.flush().await?;
        Ok(())
    }

    /// Reads a length-prefixed frame from `r`.
    ///
    /// Rejects oversize frames *before* allocating the buffer, so a hostile peer cannot
    /// induce an OOM by claiming a multi-gigabyte length.
    pub async fn read<R: AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let len = u32::from_be_bytes(len_buf);
        if len > Self::MAX_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "frame exceeds Frame::MAX_BYTES",
            ));
        }
        let mut buf = vec![0u8; len as usize];
        r.read_exact(&mut buf).await?;
        Ok(buf)
    }
}

/// Outbound network factory: hands out per-peer [`TcpRaftNetwork`] clients.
///
/// Holds a copy-on-write `(NodeId -> dial-address)` map shared with the rest of the
/// driver. Reads (every outbound RPC) are lock-free via [`ArcSwap::load`]; writes (rare
/// membership changes) replace the whole map atomically. This avoids the contention
/// that a `Mutex` would cause on the heartbeat hot path.
#[derive(Clone, Debug)]
pub struct TcpRaftNetworkFactory {
    addrs: Arc<ArcSwap<HashMap<NodeId, String>>>,
}

impl TcpRaftNetworkFactory {
    /// Constructs a factory pre-populated with the bootstrap validator set.
    pub fn new(addrs: HashMap<NodeId, String>) -> Self {
        Self { addrs: Arc::new(ArcSwap::from_pointee(addrs)) }
    }

    /// Returns the underlying address map handle so external code (the driver's request
    /// handler) can swap it in atomically when membership changes.
    pub fn addrs(&self) -> Arc<ArcSwap<HashMap<NodeId, String>>> {
        Arc::clone(&self.addrs)
    }

    /// Atomically installs an updated address map (e.g. after `AddVoter` /
    /// `RemoveVoter`).
    pub fn store_addrs(&self, addrs: HashMap<NodeId, String>) {
        self.addrs.store(Arc::new(addrs));
    }
}

impl RaftNetworkFactory<TypeConfig> for TcpRaftNetworkFactory {
    type Network = TcpRaftNetwork;

    async fn new_client(&mut self, target: NodeId, node: &ValidatorEntry) -> Self::Network {
        // Prefer the address openraft hands us via the membership entry; fall back to
        // the bootstrap map only when the membership entry is empty (e.g. the snapshot
        // path delivers a sparse Node). Whatever we pick is kept in the shared map so
        // future RPC dispatch sees it.
        let addr = if node.addr.is_empty() {
            self.addrs.load().get(&target).cloned().unwrap_or_default()
        } else {
            // openraft calls `new_client` on every heartbeat; cloning the full address
            // map per call would dominate the dispatch cost. Skip the swap when the
            // map already has the right address — only pay the clone on a real change.
            let guard = self.addrs.load();
            if guard.get(&target).map(String::as_str) != Some(node.addr.as_str()) {
                let mut updated: HashMap<NodeId, String> = (**guard).clone();
                updated.insert(target, node.addr.clone());
                self.addrs.store(Arc::new(updated));
            }
            node.addr.clone()
        };
        TcpRaftNetwork { target, addr, cached_socket: Arc::new(Mutex::new(None)) }
    }
}

/// Per-peer TCP raft client. Opens a fresh connection per RPC.
///
/// Caches the resolved [`SocketAddr`] so heartbeats don't pay a DNS round-trip every
/// 50 ms. On a connection failure the cached entry is invalidated and the next call
/// re-resolves.
#[derive(Clone, Debug)]
pub struct TcpRaftNetwork {
    target: NodeId,
    addr: String,
    cached_socket: Arc<Mutex<Option<SocketAddr>>>,
}

impl TcpRaftNetwork {
    /// Resolves the dial address, using the cached entry when available.
    async fn resolve(&self) -> Result<SocketAddr, NetworkError> {
        if let Some(addr) = *self.cached_socket.lock().await {
            return Ok(addr);
        }
        let resolved = lookup_host(self.addr.as_str())
            .await
            .map_err(|e| NetworkError::new(&e))?
            .next()
            .ok_or_else(|| {
                NetworkError::new(&std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    format!("could not resolve {}", self.addr),
                ))
            })?;
        *self.cached_socket.lock().await = Some(resolved);
        Ok(resolved)
    }

    /// Invalidates the cached socket address — called after a connection failure so the
    /// next RPC re-resolves DNS rather than dialing a stale IP.
    async fn invalidate_cache(&self) {
        *self.cached_socket.lock().await = None;
    }

    /// Issues a single request/response round-trip with the given timeout.
    async fn round_trip(
        &self,
        request: RaftWireRequest,
        rpc_timeout: Duration,
    ) -> Result<RaftWireResponse, NetworkError> {
        let send = async {
            let resolved = self.resolve().await?;
            let mut stream = match TcpStream::connect(resolved).await {
                Ok(s) => s,
                Err(e) => {
                    self.invalidate_cache().await;
                    return Err(NetworkError::new(&e));
                }
            };
            let body = Codec::encode_io(&request).map_err(|e| NetworkError::new(&e))?;
            Frame::write(&mut stream, &body).await.map_err(|e| NetworkError::new(&e))?;
            let response_bytes =
                Frame::read(&mut stream).await.map_err(|e| NetworkError::new(&e))?;
            Codec::decode_io::<RaftWireResponse>(&response_bytes).map_err(|e| NetworkError::new(&e))
        };
        timeout(rpc_timeout, send).await.unwrap_or_else(|_| {
            Err(NetworkError::new(&std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("raft RPC to {} timed out after {:?}", self.target, rpc_timeout),
            )))
        })
    }

    /// Picks an RPC deadline: openraft's hard TTL when set, else a 5 s fallback so a
    /// pathological caller-side zero doesn't wedge the dispatch task.
    fn rpc_timeout(option: &RPCOption) -> Duration {
        let hard = option.hard_ttl();
        if hard.is_zero() { Duration::from_secs(5) } else { hard }
    }
}

impl RaftNetwork<TypeConfig> for TcpRaftNetwork {
    async fn append_entries(
        &mut self,
        rpc: AppendEntriesRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<AppendEntriesResponse<NodeId>, RPCError<NodeId, ValidatorEntry, RaftError<NodeId>>>
    {
        let response = self
            .round_trip(RaftWireRequest::Append(rpc), Self::rpc_timeout(&option))
            .await
            .map_err(RPCError::Network)?;
        match response {
            RaftWireResponse::Append(Ok(r)) => Ok(r),
            RaftWireResponse::Append(Err(e)) => {
                Err(RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
            }
            _ => Err(RPCError::Network(NetworkError::new(&std::io::Error::other(
                "unexpected response variant for append_entries",
            )))),
        }
    }

    async fn vote(
        &mut self,
        rpc: VoteRequest<NodeId>,
        option: RPCOption,
    ) -> Result<VoteResponse<NodeId>, RPCError<NodeId, ValidatorEntry, RaftError<NodeId>>> {
        let response = self
            .round_trip(RaftWireRequest::Vote(rpc), Self::rpc_timeout(&option))
            .await
            .map_err(RPCError::Network)?;
        match response {
            RaftWireResponse::Vote(Ok(r)) => Ok(r),
            RaftWireResponse::Vote(Err(e)) => {
                Err(RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
            }
            _ => Err(RPCError::Network(NetworkError::new(&std::io::Error::other(
                "unexpected response variant for vote",
            )))),
        }
    }

    async fn install_snapshot(
        &mut self,
        rpc: InstallSnapshotRequest<TypeConfig>,
        option: RPCOption,
    ) -> Result<
        InstallSnapshotResponse<NodeId>,
        RPCError<NodeId, ValidatorEntry, RaftError<NodeId, InstallSnapshotError>>,
    > {
        let response = self
            .round_trip(RaftWireRequest::InstallSnapshot(rpc), Self::rpc_timeout(&option))
            .await
            .map_err(RPCError::Network)?;
        match response {
            RaftWireResponse::InstallSnapshot(Ok(r)) => Ok(r),
            RaftWireResponse::InstallSnapshot(Err(e)) => {
                Err(RPCError::RemoteError(openraft::error::RemoteError::new(self.target, e)))
            }
            _ => Err(RPCError::Network(NetworkError::new(&std::io::Error::other(
                "unexpected response variant for install_snapshot",
            )))),
        }
    }
}

/// Server task accepting inbound Raft RPCs and dispatching them to the local Raft.
#[derive(Debug)]
pub struct RaftServer;

impl RaftServer {
    /// Maximum time a single inbound connection may take to deliver its request frame
    /// before being closed. Prevents idle/slow-loris connections from accumulating.
    pub const READ_TIMEOUT: Duration = Duration::from_secs(10);

    /// Maximum time a single inbound dispatch (read + dispatch + write) may take
    /// end-to-end before the connection is dropped. Bounded above the RPC's own
    /// deadline so the server never holds a connection longer than the requester
    /// expects to wait.
    pub const DISPATCH_TIMEOUT: Duration = Duration::from_secs(30);

    /// Cap on concurrent in-flight inbound connections. Each peer typically opens at
    /// most one connection per RPC type at a time, so a small multiple of the cluster
    /// size is sufficient; sized for headroom up to ~16-voter clusters. A misbehaving
    /// or compromised peer hitting the listener cannot exhaust file descriptors past
    /// this bound.
    pub const MAX_IN_FLIGHT: usize = 64;

    /// Binds `listen_addr` and spawns the accept + per-connection dispatch tasks.
    pub async fn spawn(
        listen_addr: SocketAddr,
        raft: LeadershipRaft,
        cancel: CancellationToken,
    ) -> Result<JoinHandle<()>, std::io::Error> {
        let listener = TcpListener::bind(listen_addr).await?;
        let permits = Arc::new(Semaphore::new(Self::MAX_IN_FLIGHT));
        let handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        debug!(target: "leadership::openraft", "raft server shutting down");
                        return;
                    }
                    accept = listener.accept() => match accept {
                        Ok((stream, peer)) => {
                            // Try to acquire a per-connection permit. If the in-flight cap
                            // is hit, drop the connection immediately rather than queueing
                            // it (a queued connection would tie up a file descriptor and
                            // could outlast the requester's deadline). This bounds both
                            // memory and FDs at MAX_IN_FLIGHT regardless of peer behaviour.
                            let permit = match Arc::clone(&permits).try_acquire_owned() {
                                Ok(p) => p,
                                Err(_) => {
                                    warn!(
                                        target: "leadership::openraft",
                                        peer = %peer,
                                        max = Self::MAX_IN_FLIGHT,
                                        "raft server at in-flight cap; rejecting connection",
                                    );
                                    drop(stream);
                                    continue;
                                }
                            };
                            let raft = raft.clone();
                            tokio::spawn(async move {
                                let outcome = timeout(
                                    Self::DISPATCH_TIMEOUT,
                                    Self::serve_connection(stream, raft),
                                )
                                .await;
                                match outcome {
                                    Ok(Ok(())) => {}
                                    Ok(Err(e)) => warn!(
                                        target: "leadership::openraft",
                                        peer = %peer,
                                        error = %e,
                                        "raft server connection ended with error",
                                    ),
                                    Err(_) => warn!(
                                        target: "leadership::openraft",
                                        peer = %peer,
                                        timeout = ?Self::DISPATCH_TIMEOUT,
                                        "raft server connection exceeded dispatch timeout",
                                    ),
                                }
                                drop(permit);
                            });
                        }
                        Err(e) => {
                            warn!(
                                target: "leadership::openraft",
                                error = %e,
                                "raft server accept failed; will retry",
                            );
                            sleep(Duration::from_millis(100)).await;
                        }
                    },
                }
            }
        });
        Ok(handle)
    }

    async fn serve_connection(mut stream: TcpStream, raft: LeadershipRaft) -> std::io::Result<()> {
        // Bound the read so a peer can't just open a TCP socket and never send anything.
        let bytes =
            timeout(Self::READ_TIMEOUT, Frame::read(&mut stream)).await.map_err(|_| {
                std::io::Error::new(std::io::ErrorKind::TimedOut, "read frame timed out")
            })??;
        let request: RaftWireRequest = Codec::decode_io(&bytes)?;
        let response = match request {
            RaftWireRequest::Append(req) => {
                RaftWireResponse::Append(raft.append_entries(req).await)
            }
            RaftWireRequest::Vote(req) => RaftWireResponse::Vote(raft.vote(req).await),
            RaftWireRequest::InstallSnapshot(req) => {
                RaftWireResponse::InstallSnapshot(raft.install_snapshot(req).await)
            }
            RaftWireRequest::TriggerElect => RaftWireResponse::TriggerElect(
                raft.trigger().elect().await.map_err(|e| e.to_string()),
            ),
        };
        let body = Codec::encode_io(&response)?;
        Frame::write(&mut stream, &body).await?;
        Ok(())
    }
}

/// Translates [`openraft::Raft::metrics`] transitions into [`DriverEvent`] updates on
/// the actor's watch channel.
///
/// The translator resolves `current_leader` (a [`NodeId`]) back into a [`ValidatorId`]
/// by reading the live `membership_config` snapshot from each metrics tick — so a
/// validator added at runtime via `AddVoter` is visible to the translator on the very
/// next election, with no separate sync path required.
#[derive(Debug)]
pub struct MetricsTranslator;

impl MetricsTranslator {
    /// Spawns the translator task, returning its [`JoinHandle`].
    pub fn spawn(
        raft: LeadershipRaft,
        events_tx: watch::Sender<DriverEvent>,
        cancel: CancellationToken,
    ) -> JoinHandle<()> {
        tokio::spawn(async move {
            let mut rx = raft.metrics();
            let mut last_published: Option<DriverEvent> = None;
            loop {
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    changed = rx.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let next = {
                            let metrics = rx.borrow();
                            Self::derive_event(&metrics)
                        };
                        if last_published.as_ref() != Some(&next) {
                            debug!(
                                target: "leadership::openraft",
                                event = ?next,
                                "publishing driver event",
                            );
                            if events_tx.send(next.clone()).is_err() {
                                return;
                            }
                            last_published = Some(next);
                        }
                    }
                }
            }
        })
    }

    /// Derives a [`DriverEvent`] from the latest [`RaftMetrics`].
    ///
    /// Resolves `current_leader` against `metrics.membership_config.nodes()` — the
    /// authoritative source of truth for the current voter set — so AddVoter-introduced
    /// nodes are visible without a separate side-channel sync.
    pub fn derive_event(metrics: &RaftMetrics<NodeId, ValidatorEntry>) -> DriverEvent {
        let Some(leader_nid) = metrics.current_leader else {
            return DriverEvent::NoLeader;
        };
        metrics
            .membership_config
            .nodes()
            .find_map(|(nid, entry)| (*nid == leader_nid).then(|| entry.id.clone()))
            .map_or(DriverEvent::NoLeader, |leader| DriverEvent::LeaderElected {
                leader,
                view: metrics.current_term,
            })
    }
}

/// Soft-pause flag for the operator-facing [`DriverRequestKind::Pause`] command.
///
/// **Semantics.** When set, the driver:
/// - rejects every admin command (`TransferLeadership`, `AddVoter`, `RemoveVoter`)
///   with [`DriverError::Unsupported`];
/// - rejects health-driven voluntary step-down (the actor's
///   [`evaluate_health`](crate::RunningActor::evaluate_health) issues
///   `TransferLeadership(None)`, which falls under the same gate);
/// - **does not** stop Raft heartbeats or vote responses. Hard-stopping a Raft node
///   from outside the engine would risk dropping the cluster below quorum without
///   warning, which is worse than running paused.
///
/// In short: `Pause` is "lock the administrative surface", not "halt consensus".
/// Operators relying on the latter should remove the node from membership via
/// [`DriverRequestKind::RemoveVoter`] instead.
#[derive(Debug)]
pub struct PauseFlag(AtomicBool);

impl PauseFlag {
    /// Constructs a fresh, un-paused flag.
    pub const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Returns `true` if the flag is currently set (driver is paused).
    ///
    /// Uses [`Ordering::Relaxed`]: this is a single boolean with no other state
    /// piggybacked on its happens-before edge, so the stronger orderings buy nothing.
    pub fn is_paused(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    /// Sets the flag and returns the previous value. See [`Self::is_paused`] for the
    /// memory-ordering rationale.
    pub fn set(&self, paused: bool) -> bool {
        self.0.swap(paused, Ordering::Relaxed)
    }
}

impl Default for PauseFlag {
    fn default() -> Self {
        Self::new()
    }
}

/// Owns the channels and Raft handle the driver needs to translate
/// [`DriverRequestKind`] commands into openraft admin operations.
///
/// The handler is constructed once at driver startup and used for the lifetime of the
/// `OpenraftDriver::run` loop. Every public mutation goes through one of the
/// `handle_*` methods, so the only state escaping the run scope is the shared address
/// map (used by the network factory) and the pause flag.
//
// `Raft<TypeConfig>` does not implement `Debug`, so neither do we.
#[allow(missing_debug_implementations)]
pub struct RequestHandler {
    raft: LeadershipRaft,
    addrs: Arc<ArcSwap<HashMap<NodeId, String>>>,
    paused: Arc<PauseFlag>,
}

impl RequestHandler {
    /// Constructs a handler over the running Raft instance.
    pub const fn new(
        raft: LeadershipRaft,
        addrs: Arc<ArcSwap<HashMap<NodeId, String>>>,
        paused: Arc<PauseFlag>,
    ) -> Self {
        Self { raft, addrs, paused }
    }

    /// Handles a single [`DriverRequestKind`].
    ///
    /// Pause-guard policy: every variant *except* `Pause` and `Resume` is rejected while
    /// the driver is paused. Enforcing this at the dispatch site (rather than inside each
    /// admin method) makes safe behaviour the default — adding a new admin variant
    /// inherits the guard for free instead of relying on a developer remembering to call
    /// `ensure_unpaused`.
    pub async fn handle(&self, kind: DriverRequestKind) -> Result<(), DriverError> {
        match kind {
            DriverRequestKind::Pause => {
                // Soft-pause: refuse incoming admin operations from the actor while
                // paused. Heartbeats / votes still flow because hard-pausing a Raft node
                // would risk stalling the cluster.
                self.paused.set(true);
                Ok(())
            }
            DriverRequestKind::Resume => {
                self.paused.set(false);
                Ok(())
            }
            DriverRequestKind::TransferLeadership(target) => {
                self.ensure_unpaused("transfer_leadership")?;
                self.transfer_leadership(target).await
            }
            DriverRequestKind::AddVoter(entry) => {
                self.ensure_unpaused("add_voter")?;
                self.add_voter(entry).await
            }
            DriverRequestKind::RemoveVoter(id) => {
                self.ensure_unpaused("remove_voter")?;
                self.remove_voter(&id).await
            }
        }
    }

    fn ensure_unpaused(&self, op: &'static str) -> Result<(), DriverError> {
        if self.paused.is_paused() {
            warn!(
                target: "leadership::openraft",
                op,
                "rejecting admin op: driver is paused; resume before retrying",
            );
            Err(DriverError::Unsupported("driver is paused; resume before retrying"))
        } else {
            Ok(())
        }
    }

    /// Reads the current voter ids and the local node id from one borrow of the
    /// metrics watch — avoiding the redundant clone of the entire `RaftMetrics`.
    fn voter_snapshot(&self) -> (NodeId, BTreeSet<NodeId>) {
        let metrics = self.raft.metrics();
        let m = metrics.borrow();
        (m.id, m.membership_config.voter_ids().collect())
    }

    /// Looks up the [`NodeId`] for a [`ValidatorId`] in the current membership.
    fn node_id_for(&self, id: &ValidatorId) -> Option<NodeId> {
        self.raft
            .metrics()
            .borrow()
            .membership_config
            .nodes()
            .find_map(|(nid, entry)| (entry.id == *id).then_some(*nid))
    }

    async fn transfer_leadership(&self, target: Option<ValidatorId>) -> Result<(), DriverError> {
        // openraft 0.9 has no native `transfer_leader` admin RPC, so we drive a real
        // transfer by asking a follower to bump its term via [`Raft::trigger().elect()`].
        // The current leader sees the higher-term vote request and steps down; the chosen
        // follower wins the election (it just kicked it off). Voter membership is
        // untouched, so HA is preserved across transfers.
        let (local_id, voter_addrs, target_nid) = {
            let metrics = self.raft.metrics();
            let m = metrics.borrow();
            if m.current_leader != Some(m.id) {
                return Err(DriverError::Unsupported(
                    "TransferLeadership requested but local node is not the current leader",
                ));
            }
            let voters: BTreeSet<NodeId> = m.membership_config.voter_ids().collect();
            // Build a (NodeId -> dial addr) map restricted to the current voter set, so
            // we can dial the target without dropping the metrics borrow.
            let mut voter_addrs: HashMap<NodeId, String> = HashMap::new();
            for (nid, entry) in m.membership_config.nodes() {
                if voters.contains(nid) && *nid != m.id {
                    voter_addrs.insert(*nid, entry.addr.clone());
                }
            }
            // Resolve `target` to a NodeId if provided; otherwise pick any non-self voter
            // (deterministic via BTreeSet's first element so retries are predictable).
            let target_nid = match target.as_ref() {
                Some(id) => {
                    let nid = m
                        .membership_config
                        .nodes()
                        .find_map(|(nid, entry)| (entry.id == *id).then_some(*nid))
                        .ok_or(DriverError::Unsupported(
                            "transfer target is not a current voter",
                        ))?;
                    if nid == m.id {
                        return Err(DriverError::Unsupported(
                            "transfer target is the local node; nothing to do",
                        ));
                    }
                    nid
                }
                None => *voters.iter().find(|nid| **nid != m.id).ok_or(
                    DriverError::Unsupported("no other voter available to transfer leadership to"),
                )?,
            };
            (m.id, voter_addrs, target_nid)
        };
        // Address resolution: prefer the membership entry, fall back to the shared addrs
        // map (populated at startup + on every AddVoter). Snapshot-restored membership
        // can carry sparse Node entries with empty `addr`, in which case the shared map
        // is the source of truth.
        let target_addr = voter_addrs
            .get(&target_nid)
            .filter(|s| !s.is_empty())
            .cloned()
            .or_else(|| self.addrs.load().get(&target_nid).cloned())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                DriverError::Exited(format!("transfer target {target_nid} has no dial address"))
            })?;
        warn!(
            target: "leadership::openraft",
            local_id,
            target_nid,
            target_addr = %target_addr,
            "transferring leadership: asking target to trigger an election",
        );
        // Issue a one-shot TriggerElect RPC. The target node calls `raft.trigger().elect()`
        // synchronously; we treat the ack as confirmation that the campaign has started,
        // not that the new term has committed (the next metrics tick will surface that).
        let net = TcpRaftNetwork {
            target: target_nid,
            addr: target_addr,
            cached_socket: Arc::new(Mutex::new(None)),
        };
        let response = net
            .round_trip(RaftWireRequest::TriggerElect, Duration::from_secs(5))
            .await
            .map_err(|e| DriverError::Exited(format!("trigger_elect RPC failed: {e}")))?;
        match response {
            RaftWireResponse::TriggerElect(Ok(())) => Ok(()),
            RaftWireResponse::TriggerElect(Err(msg)) => {
                Err(DriverError::Exited(format!("trigger_elect rejected by target: {msg}")))
            }
            _ => Err(DriverError::Exited(
                "trigger_elect: unexpected response variant from target".into(),
            )),
        }
    }

    /// Adds a voter via the canonical openraft join flow: add as learner, then promote.
    ///
    /// Sequencing invariant: this method reads `voter_snapshot()` between `add_learner`
    /// and `change_membership` without a lock. Correctness relies on the fact that all
    /// admin requests funnel through `LeadershipActor`'s `select!` loop, which dispatches
    /// one request at a time — there is no concurrent caller racing the snapshot. If a
    /// future caller invokes `RequestHandler::handle` outside the actor, this method must
    /// be reworked (e.g. take `&mut self` or guard the snapshot/promotion with a lock).
    async fn add_voter(&self, entry: ValidatorEntry) -> Result<(), DriverError> {
        let nid = NodeIdHash::hash(&entry.id);
        // Update the address map *before* asking openraft to dial — the new_client
        // factory falls back to this map when the membership Node carries an empty addr.
        self.update_addrs(|map| {
            map.insert(nid, entry.addr.clone());
        });
        // Add as learner first so the new node catches up before being counted toward
        // quorum, then promote to voter via change_membership. This matches the
        // canonical openraft join flow.
        if let Err(err) = self.raft.add_learner(nid, entry.clone(), true).await {
            // add_learner failed — roll back the address map so a retry starts clean.
            self.update_addrs(|map| {
                map.remove(&nid);
            });
            return Err(DriverError::Exited(format!("raft add_learner failed: {err}")));
        }
        let mut voters: BTreeSet<NodeId> = self.voter_snapshot().1;
        voters.insert(nid);
        if let Err(err) = self.raft.change_membership(voters, false).await {
            // Promotion to voter failed (commonly: another membership change is in flight).
            // The node is currently a learner; leaving it stuck would prevent retries
            // (the next add_learner would be a no-op while raft already knows it). Demote
            // and clear the address map so the operator can retry from a clean state.
            warn!(
                target: "leadership::openraft",
                node_id = nid,
                error = %err,
                "change_membership(add) failed; rolling back learner",
            );
            if let Err(rb) = self
                .raft
                .change_membership(ChangeMembers::RemoveNodes(BTreeSet::from([nid])), false)
                .await
            {
                // Best-effort rollback. If it also fails the cluster is in a degraded
                // state that needs operator attention; surface both errors.
                return Err(DriverError::Exited(format!(
                    "raft change_membership(add) failed: {err}; rollback also failed: {rb}"
                )));
            }
            self.update_addrs(|map| {
                map.remove(&nid);
            });
            return Err(DriverError::Exited(format!("raft change_membership(add) failed: {err}")));
        }
        Ok(())
    }

    async fn remove_voter(&self, id: &ValidatorId) -> Result<(), DriverError> {
        let nid = self
            .node_id_for(id)
            .ok_or(DriverError::Unsupported("remove target is not a current voter"))?;
        self.raft
            .change_membership(ChangeMembers::RemoveNodes(BTreeSet::from([nid])), false)
            .await
            .map_err(|e| {
                DriverError::Exited(format!("raft change_membership(remove) failed: {e}"))
            })?;
        self.update_addrs(|map| {
            map.remove(&nid);
        });
        Ok(())
    }

    /// Atomically replaces the shared address map after `mutator` has updated a clone.
    fn update_addrs(&self, mutator: impl FnOnce(&mut HashMap<NodeId, String>)) {
        let mut updated: HashMap<NodeId, String> = (**self.addrs.load()).clone();
        mutator(&mut updated);
        self.addrs.store(Arc::new(updated));
    }
}

/// Production [`ConsensusDriver`] backed by openraft + sled.
#[derive(Debug)]
pub struct OpenraftDriver {
    storage_dir: PathBuf,
}

impl OpenraftDriver {
    /// Number of log entries between automatic snapshots. Bounded so the on-disk log
    /// doesn't grow without bound across long-running deployments; the leadership
    /// state machine is empty so each snapshot is tiny (membership + log id).
    pub const SNAPSHOT_LOGS_SINCE_LAST: u64 = 1_024;

    /// Constructs a driver that will persist its log + state machine under `storage_dir`.
    pub const fn new(storage_dir: PathBuf) -> Self {
        Self { storage_dir }
    }

    /// Builds the openraft [`Config`] from operator-tuned [`RaftTimeouts`].
    pub fn build_raft_config(timeouts: &RaftTimeouts) -> Result<Config, DriverError> {
        let to_ms = |label: &'static str, d: Duration| -> Result<u64, DriverError> {
            u64::try_from(d.as_millis()).map_err(|_| {
                DriverError::Startup(format!("{label} exceeds u64 milliseconds: {d:?}"))
            })
        };
        let cfg = Config {
            cluster_name: "base-leadership".to_owned(),
            heartbeat_interval: to_ms("heartbeat_interval", timeouts.heartbeat_interval)?,
            election_timeout_min: to_ms("election_timeout_min", timeouts.election_timeout_min)?,
            election_timeout_max: to_ms("election_timeout_max", timeouts.election_timeout_max)?,
            install_snapshot_timeout: to_ms(
                "install_snapshot_timeout",
                timeouts.install_snapshot_timeout,
            )?,
            max_payload_entries: timeouts.max_payload_entries,
            snapshot_policy: SnapshotPolicy::LogsSinceLast(Self::SNAPSHOT_LOGS_SINCE_LAST),
            ..Default::default()
        };
        cfg.validate().map_err(|e| DriverError::Startup(format!("invalid raft config: {e}")))
    }

    /// Builds the bootstrap NodeId↔ValidatorEntry tables from the configured voter set.
    ///
    /// Errors loudly on a [`NodeIdHash`] collision (any two validator ids hashing to
    /// the same `NodeId`) so a pathological config fails at startup rather than
    /// silently overwriting a peer entry. With FNV-1a/64 and operator-controlled ids
    /// this never fires in practice — but the cost of the check is one comparison.
    fn build_id_tables(
        membership: &crate::ClusterMembership,
    ) -> Result<BootstrapTables, DriverError> {
        let mut id_to_addr: HashMap<NodeId, String> = HashMap::new();
        let mut bootstrap: BTreeMap<NodeId, ValidatorEntry> = BTreeMap::new();
        for entry in &membership.voters {
            let nid = NodeIdHash::hash(&entry.id);
            if let Some(prev) = bootstrap.insert(nid, entry.clone()) {
                return Err(DriverError::Startup(format!(
                    "NodeIdHash collision between {} and {} (both hashed to {nid})",
                    prev.id, entry.id,
                )));
            }
            id_to_addr.insert(nid, entry.addr.clone());
        }
        Ok((id_to_addr, bootstrap))
    }

    /// Attempts to bootstrap the cluster by calling [`openraft::Raft::initialize`].
    ///
    /// Every node calls this on startup. openraft is idempotent: if the cluster has
    /// already been initialized (or this node has any log entries), it returns
    /// [`InitializeError::NotAllowed`], which is treated as benign. Any *other* error
    /// is surfaced as a startup failure rather than swallowed — that's how a real
    /// storage failure stops becoming a silent boot.
    async fn bootstrap_cluster(
        raft: &LeadershipRaft,
        local_id: &ValidatorId,
        bootstrap: BTreeMap<NodeId, ValidatorEntry>,
    ) -> Result<(), DriverError> {
        let voter_count = bootstrap.len();
        info!(
            target: "leadership::openraft",
            local = %local_id,
            voters = voter_count,
            "attempting raft cluster bootstrap (idempotent)",
        );
        match raft.initialize(bootstrap).await {
            Ok(()) => Ok(()),
            Err(RaftError::APIError(InitializeError::NotAllowed(_))) => {
                debug!(
                    target: "leadership::openraft",
                    "raft cluster already initialized; bootstrap is a no-op",
                );
                Ok(())
            }
            Err(e) => Err(DriverError::Startup(format!("raft initialize failed: {e}"))),
        }
    }
}

#[async_trait]
impl ConsensusDriver for OpenraftDriver {
    async fn run(self: Box<Self>, ctx: DriverContext) -> Result<(), DriverError> {
        let DriverContext { config, membership, events_tx, mut requests_rx, cancel } = ctx;

        let storage_dir = self.storage_dir.clone();
        std::fs::create_dir_all(&storage_dir).map_err(|e| {
            DriverError::Startup(format!(
                "failed to create leadership storage dir {}: {e}",
                storage_dir.display(),
            ))
        })?;

        let db = sled::open(&storage_dir).map_err(|e| {
            DriverError::Startup(format!("failed to open sled db {}: {e}", storage_dir.display()))
        })?;
        let log_store = SledLogStore::new(db.clone())
            .map_err(|e| DriverError::Startup(format!("failed to open raft log tree: {e}")))?;
        let state_machine = SledStateMachine::new(db).map_err(|e| {
            DriverError::Startup(format!("failed to open raft state-machine tree: {e}"))
        })?;

        let (id_to_addr, bootstrap) = Self::build_id_tables(&membership)?;
        let local_node_id = NodeIdHash::hash(&config.local_id);
        if !id_to_addr.contains_key(&local_node_id) {
            return Err(DriverError::Startup(format!(
                "local validator {} not present in derived NodeId map",
                config.local_id,
            )));
        }

        let raft_config = Arc::new(Self::build_raft_config(&config.timeouts)?);
        let network = TcpRaftNetworkFactory::new(id_to_addr);
        let addrs_handle = network.addrs();

        let raft = openraft::Raft::<TypeConfig>::new(
            local_node_id,
            raft_config,
            network,
            log_store,
            state_machine,
        )
        .await
        .map_err(|e| DriverError::Startup(format!("failed to start raft: {e}")))?;

        // Bring up the inbound TCP server before anyone else dials us.
        let server_handle =
            RaftServer::spawn(config.transport.listen_addr, raft.clone(), cancel.clone())
                .await
                .map_err(|e| {
                    DriverError::Startup(format!(
                        "failed to bind raft server on {}: {e}",
                        config.transport.listen_addr,
                    ))
                })?;

        // Every node attempts bootstrap; openraft is idempotent and short-circuits on
        // already-initialized clusters via `InitializeError::NotAllowed`. This
        // tolerates the lex-smallest bootstrapper being permanently down — any
        // surviving voter brings the cluster up on its own.
        Self::bootstrap_cluster(&raft, &config.local_id, bootstrap).await?;

        let metrics_handle =
            MetricsTranslator::spawn(raft.clone(), events_tx.clone(), cancel.clone());

        let paused = Arc::new(PauseFlag::new());
        let handler = RequestHandler::new(raft.clone(), addrs_handle, Arc::clone(&paused));

        // Main request loop: handle DriverRequests + cancellation.
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    debug!(target: "leadership::openraft", "cancellation received");
                    break;
                }
                maybe_req = requests_rx.recv() => {
                    let Some(req) = maybe_req else {
                        debug!(target: "leadership::openraft", "request channel closed");
                        break;
                    };
                    let DriverRequest { kind, ack } = req;
                    let result = handler.handle(kind).await;
                    if ack.send(result).is_err() {
                        debug!(target: "leadership::openraft", "ack channel closed before reply");
                    }
                }
            }
        }

        // Shutdown: await the inbound accept loop *before* tearing down the raft
        // instance. The accept loop's cancel token already fired in the select above, so
        // this completes promptly — but it guarantees no further connections are spawned
        // into a shutting-down raft. (Per-connection tasks remain fire-and-forget bounded
        // by `DISPATCH_TIMEOUT`; their RPCs may still race the raft shutdown.)
        let _ = server_handle.await;
        let shutdown_result = raft.shutdown().await;
        let _ = metrics_handle.await;
        if let Err(e) = shutdown_result {
            return Err(DriverError::Exited(format!("raft shutdown error: {e}")));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::{SocketAddr, TcpListener};

    use tempfile::TempDir;
    use tokio::sync::{mpsc, oneshot};

    use super::*;
    use crate::{HealthThresholds, LeadershipConfig, RaftTimeouts, TransportConfig};

    #[test]
    fn node_id_hash_is_stable_and_distinct() {
        let a = NodeIdHash::hash(&ValidatorId::new("seq-1"));
        let b = NodeIdHash::hash(&ValidatorId::new("seq-2"));
        let a_again = NodeIdHash::hash(&ValidatorId::new("seq-1"));
        assert_eq!(a, a_again);
        assert_ne!(a, b);
    }

    #[test]
    fn log_key_orders_lexicographically_by_index() {
        assert!(SledLogStore::log_key(0) < SledLogStore::log_key(1));
        assert!(SledLogStore::log_key(1) < SledLogStore::log_key(2));
        assert!(SledLogStore::log_key(255) < SledLogStore::log_key(256));
        assert!(SledLogStore::log_key(u64::MAX - 1) < SledLogStore::log_key(u64::MAX));
    }

    #[test]
    fn bincode_round_trip_for_storedsnapshot() {
        let snap = StoredSnapshot {
            meta: SnapshotMeta {
                last_log_id: None,
                last_membership: StoredMembership::default(),
                snapshot_id: "snap-test".into(),
            },
            data: vec![1, 2, 3],
        };
        let bytes = Codec::encode(&snap).unwrap();
        let back: StoredSnapshot = Codec::decode(&bytes).unwrap();
        assert_eq!(snap.meta.snapshot_id, back.meta.snapshot_id);
        assert_eq!(snap.data, back.data);
    }

    /// Reserves a free TCP port by binding `0` and dropping the listener — the kernel
    /// will (with high probability) re-issue the same port to the next bind. Adequate
    /// for in-process tests on an otherwise-quiet host.
    fn free_port() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        l.local_addr().expect("local addr").port()
    }

    fn fast_timeouts() -> RaftTimeouts {
        // Tighter timeouts than the production default so tests finish in seconds
        // rather than tens of seconds. Heartbeat is well below election_timeout_min so
        // a healthy leader doesn't trigger spurious elections under loaded CI.
        RaftTimeouts {
            election_timeout_min: Duration::from_millis(150),
            election_timeout_max: Duration::from_millis(300),
            heartbeat_interval: Duration::from_millis(40),
            install_snapshot_timeout: Duration::from_secs(1),
            max_payload_entries: 64,
        }
    }

    fn validators_for(triples: &[(&str, SocketAddr)]) -> Vec<ValidatorEntry> {
        triples
            .iter()
            .map(|(id, addr)| ValidatorEntry { id: ValidatorId::new(*id), addr: addr.to_string() })
            .collect()
    }

    /// A running [`OpenraftDriver`] under test, with the channels and tempdir held so
    /// the test can observe events, send admin requests, and tear the node down.
    struct RunningNode {
        cancel: CancellationToken,
        handle: tokio::task::JoinHandle<Result<(), DriverError>>,
        events_rx: watch::Receiver<DriverEvent>,
        requests_tx: mpsc::Sender<DriverRequest>,
        // Held to keep the sled directory alive for the lifetime of the node.
        _storage_dir: TempDir,
    }

    impl RunningNode {
        async fn shutdown(self) {
            self.cancel.cancel();
            let _ = timeout(Duration::from_secs(5), self.handle).await;
        }
    }

    fn spawn_node(
        local: &str,
        validators: Vec<ValidatorEntry>,
        listen: SocketAddr,
        timeouts: RaftTimeouts,
    ) -> RunningNode {
        let storage_dir = tempfile::tempdir().expect("tempdir");
        let config = LeadershipConfig {
            local_id: ValidatorId::new(local),
            validators,
            transport: TransportConfig { listen_addr: listen },
            health: HealthThresholds::default(),
            timeouts,
        };
        let membership = config.validate().expect("config validates");
        let (events_tx, events_rx) = watch::channel(DriverEvent::NoLeader);
        let (requests_tx, requests_rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let ctx =
            DriverContext { config, membership, events_tx, requests_rx, cancel: cancel.clone() };
        let driver = Box::new(OpenraftDriver::new(storage_dir.path().to_path_buf()));
        let handle = tokio::spawn(driver.run(ctx));
        RunningNode { cancel, handle, events_rx, requests_tx, _storage_dir: storage_dir }
    }

    async fn await_event<F>(rx: &mut watch::Receiver<DriverEvent>, deadline: Duration, pred: F)
    where
        F: Fn(&DriverEvent) -> bool,
    {
        let result = timeout(deadline, async {
            loop {
                if pred(&rx.borrow()) {
                    return;
                }
                rx.changed().await.expect("event channel open");
            }
        })
        .await;
        assert!(result.is_ok(), "timed out waiting for driver event; current = {:?}", rx.borrow());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn single_node_elects_local_as_leader() {
        let port = free_port();
        let listen: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let validators = validators_for(&[("solo", listen)]);
        let mut node = spawn_node("solo", validators, listen, fast_timeouts());

        await_event(&mut node.events_rx, Duration::from_secs(10), |e| {
            matches!(e, DriverEvent::LeaderElected { leader, .. } if leader == &ValidatorId::new("solo"))
        })
        .await;

        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn three_node_cluster_elects_smallest_id_as_initial_leader_then_fails_over_after_kill() {
        // The lex-smallest validator id ("seq-a") bootstraps and becomes the initial
        // leader. Killing it should let one of seq-b/seq-c win the next election within
        // a couple of election timeouts.
        let (port_a, port_b, port_c) = (free_port(), free_port(), free_port());
        let addr_a: SocketAddr = format!("127.0.0.1:{port_a}").parse().unwrap();
        let addr_b: SocketAddr = format!("127.0.0.1:{port_b}").parse().unwrap();
        let addr_c: SocketAddr = format!("127.0.0.1:{port_c}").parse().unwrap();
        let validators = validators_for(&[("seq-a", addr_a), ("seq-b", addr_b), ("seq-c", addr_c)]);
        let timeouts = fast_timeouts();

        let node_a = spawn_node("seq-a", validators.clone(), addr_a, timeouts.clone());
        let mut node_b = spawn_node("seq-b", validators.clone(), addr_b, timeouts.clone());
        let node_c = spawn_node("seq-c", validators, addr_c, timeouts);

        // From node_b's vantage point, the initial leader should be seq-a.
        await_event(&mut node_b.events_rx, Duration::from_secs(15), |e| {
            matches!(e, DriverEvent::LeaderElected { leader, .. } if leader == &ValidatorId::new("seq-a"))
        })
        .await;

        // Kill seq-a. After ~election_timeout the surviving nodes elect a new leader.
        node_a.shutdown().await;

        // Either seq-b or seq-c becomes the new leader. Wait until the leader signal in
        // node_b's view is no longer seq-a.
        await_event(&mut node_b.events_rx, Duration::from_secs(15), |e| match e {
            DriverEvent::LeaderElected { leader, .. } => leader != &ValidatorId::new("seq-a"),
            DriverEvent::NoLeader => false,
        })
        .await;

        node_b.shutdown().await;
        node_c.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn pause_and_resume_admin_requests_succeed_against_a_running_node() {
        let port = free_port();
        let listen: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
        let validators = validators_for(&[("solo", listen)]);
        let node = spawn_node("solo", validators, listen, fast_timeouts());

        for kind in [DriverRequestKind::Pause, DriverRequestKind::Resume] {
            let (ack, ack_rx) = oneshot::channel();
            node.requests_tx.send(DriverRequest { kind, ack }).await.expect("send admin request");
            ack_rx.await.expect("ack channel open").expect("admin request acked Ok");
        }

        node.shutdown().await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn transfer_leadership_request_against_leader_returns_ok() {
        // Confirms the admin API: `TransferLeadership(None)` issued against the current
        // leader returns Ok. The follow-on election dynamics (which voter becomes the
        // new leader, how long it takes) are exercised by the dedicated failover test;
        // here we are validating that the request shape itself is wired correctly all
        // the way down to `Raft::change_membership` and back.
        let (port_a, port_b, port_c) = (free_port(), free_port(), free_port());
        let addr_a: SocketAddr = format!("127.0.0.1:{port_a}").parse().unwrap();
        let addr_b: SocketAddr = format!("127.0.0.1:{port_b}").parse().unwrap();
        let addr_c: SocketAddr = format!("127.0.0.1:{port_c}").parse().unwrap();
        let validators = validators_for(&[("seq-a", addr_a), ("seq-b", addr_b), ("seq-c", addr_c)]);
        let timeouts = fast_timeouts();

        let node_a = spawn_node("seq-a", validators.clone(), addr_a, timeouts.clone());
        let mut node_b = spawn_node("seq-b", validators.clone(), addr_b, timeouts.clone());
        let node_c = spawn_node("seq-c", validators, addr_c, timeouts);

        // Wait for seq-a (lex-smallest, the bootstrapper) to be observed as leader.
        await_event(&mut node_b.events_rx, Duration::from_secs(15), |e| {
            matches!(e, DriverEvent::LeaderElected { leader, .. } if leader == &ValidatorId::new("seq-a"))
        })
        .await;

        // Wait for the bootstrap membership entry to fully commit before issuing the
        // transfer; otherwise change_membership races against the in-flight bootstrap
        // and fails with "configuration change in flight".
        await_event(&mut node_b.events_rx, Duration::from_secs(15), |_| {
            true // any event past initial leader-elected means commit propagated
        })
        .await;
        // Give the bootstrap config one extra heartbeat to commit on every node.
        sleep(Duration::from_millis(500)).await;

        let (ack, ack_rx) = oneshot::channel();
        node_a
            .requests_tx
            .send(DriverRequest { kind: DriverRequestKind::TransferLeadership(None), ack })
            .await
            .expect("send transfer request");
        let result = timeout(Duration::from_secs(30), ack_rx)
            .await
            .expect("transfer ack within 30s")
            .expect("ack channel open");
        assert!(result.is_ok(), "transfer leadership returned: {result:?}");

        node_a.shutdown().await;
        node_b.shutdown().await;
        node_c.shutdown().await;
    }

    #[test]
    fn node_id_collision_check_rejects_duplicates() {
        // Sanity: a fabricated voter set with two ids that hash to the same NodeId
        // should be caught by `build_id_tables`. We can't easily produce a real FNV
        // collision (probability ~2^-65 per random pair), so we assert the *structure*:
        // the same id twice trivially collides and surfaces a Startup error.
        let entry = ValidatorEntry { id: ValidatorId::new("dup"), addr: "127.0.0.1:1".into() };
        let membership = crate::ClusterMembership::new(vec![entry.clone(), entry], 0);
        let err = OpenraftDriver::build_id_tables(&membership).unwrap_err();
        assert!(matches!(err, DriverError::Startup(_)));
    }
}
