//! Tests for [`SequencerActor::run_block_pipeline`] and related helpers.

use std::sync::Arc;

use alloy_primitives::B256;
use alloy_rpc_types_engine::{ExecutionPayloadV1, PayloadId};
use alloy_transport::RpcError;
use base_alloy_rpc_types_engine::{
    OpExecutionPayload, OpExecutionPayloadEnvelope, OpPayloadAttributes,
};
use base_consensus_engine::SealTaskError;
use base_protocol::{BlockInfo, L2BlockInfo, OpAttributesWithParent};

#[cfg(test)]
use crate::{
    ConductorError, SequencerActorError, UnsafePayloadGossipClientError,
    actors::{
        MockConductor, MockSequencerEngineClient, MockUnsafePayloadGossipClient,
        engine::EngineClientError,
        sequencer::{UnsealedPayloadHandle, tests::test_util::test_actor},
    },
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Creates a minimal `OpExecutionPayloadEnvelope` with a specific `block_hash`.
fn envelope_with_hash(hash: B256) -> OpExecutionPayloadEnvelope {
    OpExecutionPayloadEnvelope {
        parent_beacon_block_root: None,
        execution_payload: OpExecutionPayload::V1(ExecutionPayloadV1 {
            parent_hash: B256::ZERO,
            fee_recipient: alloy_primitives::Address::ZERO,
            state_root: B256::ZERO,
            receipts_root: B256::ZERO,
            logs_bloom: alloy_primitives::Bloom::ZERO,
            prev_randao: B256::ZERO,
            block_number: 1,
            gas_limit: 0,
            gas_used: 0,
            timestamp: 0,
            extra_data: alloy_primitives::Bytes::new(),
            base_fee_per_gas: alloy_primitives::U256::ZERO,
            block_hash: hash,
            transactions: vec![],
        }),
    }
}

/// Creates an `L2BlockInfo` at the given block number (all other fields zeroed).
fn head_at(number: u64) -> L2BlockInfo {
    L2BlockInfo { block_info: BlockInfo { number, ..Default::default() }, ..Default::default() }
}

/// Creates a pre-built `UnsealedPayloadHandle` whose parent is at `parent_number`.
fn handle_with_parent(parent_number: u64) -> UnsealedPayloadHandle {
    let parent = head_at(parent_number);
    UnsealedPayloadHandle {
        payload_id: PayloadId::default(),
        attributes_with_parent: OpAttributesWithParent::new(
            OpPayloadAttributes::default(),
            parent,
            None,
            false,
        ),
    }
}

/// A non-fatal `SealTaskError` (returns false from `is_fatal()`).
fn non_fatal_seal_error() -> SealTaskError {
    SealTaskError::UnsafeHeadChangedSinceBuild
}

/// A fatal `SealTaskError` (returns true from `is_fatal()`).
fn fatal_seal_error() -> SealTaskError {
    SealTaskError::DepositOnlyPayloadFailed
}

fn conductor_rpc_error() -> ConductorError {
    ConductorError::Rpc(RpcError::local_usage_str("test conductor error"))
}

// ---------------------------------------------------------------------------
// run_block_pipeline — no pre-built payload
// ---------------------------------------------------------------------------

/// When `next_payload` is `None`, the pipeline skips sealing and goes straight
/// to the safe-lag check and then `build()`.
#[tokio::test]
async fn test_pipeline_no_payload_calls_build() {
    let mut engine = MockSequencerEngineClient::new();
    // Called twice: once for the safe-lag guard, once inside builder.build().
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;

    let mut next_payload = None;
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some(), "build() should have produced a pre-built payload");
}

// ---------------------------------------------------------------------------
// run_block_pipeline — stale detection
// ---------------------------------------------------------------------------

/// If the unsafe head has advanced past the build parent, the pre-built payload
/// is discarded and a fresh build is initiated.
#[tokio::test]
async fn test_pipeline_stale_head_advanced_discards_and_rebuilds() {
    let mut engine = MockSequencerEngineClient::new();
    // Stale check: head is at 5, but handle parent is at 4 → stale (advanced).
    // Then safe-lag check (call 2) + builder.build() (call 3).
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(5)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(5)));
    // get_sealed_payload must NOT be called (stale build is discarded).
    engine.expect_get_sealed_payload().times(0);
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;

    // Pre-built payload has parent at block 4, but head is at 5.
    let mut next_payload = Some(handle_with_parent(4));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some(), "fresh build should have populated next_payload");
}

/// If the unsafe head has rewound below the build parent (e.g. after a reset),
/// the pre-built payload is discarded and a fresh build is initiated.
#[tokio::test]
async fn test_pipeline_stale_head_rewound_discards_and_rebuilds() {
    let mut engine = MockSequencerEngineClient::new();
    // Head rewound to 2, but handle parent is at 5.
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(2)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(2)));
    engine.expect_get_sealed_payload().times(0);
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;

    let mut next_payload = Some(handle_with_parent(5));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some());
}

// ---------------------------------------------------------------------------
// run_block_pipeline — seal errors
// ---------------------------------------------------------------------------

/// A fatal `SealTaskError` cancels the actor and is returned as an error.
#[tokio::test]
async fn test_pipeline_fatal_seal_error_cancels_actor() {
    let mut engine = MockSequencerEngineClient::new();
    // Stale check: head matches parent → proceed to seal.
    engine.expect_get_unsafe_head().times(1).return_once(|| Ok(head_at(0)));
    engine
        .expect_get_sealed_payload()
        .times(1)
        .return_once(|_, _| Err(EngineClientError::SealError(fatal_seal_error())));

    let mut actor = test_actor();
    let token = actor.cancellation_token.clone();
    actor.engine_client = Arc::new(engine);

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_err());
    assert!(result.unwrap_err().is_fatal());
    assert!(token.is_cancelled(), "cancellation token must be set on fatal error");
}

/// A non-fatal `SealTaskError` drops the block and triggers an immediate rebuild.
#[tokio::test]
async fn test_pipeline_nonfatal_seal_error_drops_block_and_rebuilds() {
    let mut engine = MockSequencerEngineClient::new();
    // Stale check (call 1), then inside builder.build() (call 2).
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(0)));
    engine
        .expect_get_sealed_payload()
        .times(1)
        .return_once(|_, _| Err(EngineClientError::SealError(non_fatal_seal_error())));
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));
    // insert_and_await_head must NOT be called — block was dropped.
    engine.expect_insert_and_await_head().times(0);

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok(), "non-fatal seal error must not propagate as Err");
    assert!(next_payload.is_some(), "a fresh payload should have been built");
}

// ---------------------------------------------------------------------------
// run_block_pipeline — conductor commit
// ---------------------------------------------------------------------------

/// A conductor commit failure returns `ConductorCommitFailed` (non-fatal).
#[tokio::test]
async fn test_pipeline_conductor_failure_returns_nonfatal_error() {
    let envelope = envelope_with_hash(B256::with_last_byte(0xAB));

    let mut engine = MockSequencerEngineClient::new();
    engine.expect_get_unsafe_head().times(1).return_once(|| Ok(head_at(0)));
    engine.expect_get_sealed_payload().times(1).return_once(move |_, _| Ok(envelope));
    // insert must NOT be called — conductor failure triggers early return.
    engine.expect_insert_and_await_head().times(0);

    let mut conductor = MockConductor::new();
    conductor.expect_commit_unsafe_payload().times(1).return_once(|_| Err(conductor_rpc_error()));

    let mut actor = test_actor();
    actor.conductor = Some(conductor);
    actor.engine_client = Arc::new(engine);

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(
        matches!(result, Err(SequencerActorError::ConductorCommitFailed(_))),
        "conductor failure must return ConductorCommitFailed"
    );
    assert!(!result.unwrap_err().is_fatal(), "conductor failure is not fatal");
}

// ---------------------------------------------------------------------------
// run_block_pipeline — gossip
// ---------------------------------------------------------------------------

/// A gossip failure is logged as a warning but does not prevent engine insertion.
#[tokio::test]
async fn test_pipeline_gossip_failure_continues_to_insert() {
    let hash = B256::with_last_byte(0xCD);
    let envelope = envelope_with_hash(hash);

    let mut engine = MockSequencerEngineClient::new();
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_sealed_payload().times(1).return_once(move |_, _| Ok(envelope));
    // insert_and_await_head must still be called after gossip fails.
    engine.expect_insert_and_await_head().times(1).return_once(move |_, _| Ok(head_at(1)));
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    let mut gossip = MockUnsafePayloadGossipClient::new();
    gossip.expect_schedule_execution_payload_gossip().times(1).return_once(|_| {
        Err(UnsafePayloadGossipClientError::RequestError("channel closed".to_string()))
    });

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;
    actor.unsafe_payload_gossip_client = gossip;

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok(), "gossip failure must not abort the pipeline");
}

// ---------------------------------------------------------------------------
// run_block_pipeline — happy path
// ---------------------------------------------------------------------------

/// Full pipeline without conductor: seal → gossip → insert → build next.
#[tokio::test]
async fn test_pipeline_happy_path_no_conductor() {
    let hash = B256::with_last_byte(0x42);
    let envelope = envelope_with_hash(hash);

    let mut engine = MockSequencerEngineClient::new();
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_sealed_payload().times(1).return_once(move |_, _| Ok(envelope));
    engine.expect_insert_and_await_head().times(1).return_once(move |_, expected| {
        assert_eq!(expected, hash);
        Ok(head_at(1))
    });
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    let mut gossip = MockUnsafePayloadGossipClient::new();
    gossip.expect_schedule_execution_payload_gossip().times(1).return_once(|_| Ok(()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;
    actor.unsafe_payload_gossip_client = gossip;

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some(), "next block should have been pre-built");
}

/// Full pipeline with conductor: seal → conductor commit → gossip → insert → build.
#[tokio::test]
async fn test_pipeline_happy_path_with_conductor() {
    let hash = B256::with_last_byte(0x99);
    let envelope = envelope_with_hash(hash);

    let mut engine = MockSequencerEngineClient::new();
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    engine.expect_get_sealed_payload().times(1).return_once(move |_, _| Ok(envelope));
    engine.expect_insert_and_await_head().times(1).return_once(|_, _| Ok(head_at(1)));
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    let mut conductor = MockConductor::new();
    conductor.expect_commit_unsafe_payload().times(1).return_once(|_| Ok(()));

    let mut gossip = MockUnsafePayloadGossipClient::new();
    gossip.expect_schedule_execution_payload_gossip().times(1).return_once(|_| Ok(()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.conductor = Some(conductor);
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;
    actor.unsafe_payload_gossip_client = gossip;

    let mut next_payload = Some(handle_with_parent(0));
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some());
}

// ---------------------------------------------------------------------------
// run_block_pipeline — safe-lag guard
// ---------------------------------------------------------------------------

/// When `unsafe_head - safe_head > MAX_SAFE_LAG`, sequencing is paused for 1s.
#[tokio::test]
async fn test_pipeline_safe_lag_exceeded_pauses_sequencing() {
    const MAX_SAFE_LAG: u64 = 1800;

    let mut engine = MockSequencerEngineClient::new();
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(MAX_SAFE_LAG + 1)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    // No build or insert should be called when paused.
    engine.expect_start_build_block().times(0);
    engine.expect_insert_and_await_head().times(0);

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);

    let mut next_payload: Option<UnsealedPayloadHandle> = None;
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert_eq!(
        result.unwrap(),
        std::time::Duration::from_secs(1),
        "safe-lag pause must return 1s backoff"
    );
    assert!(next_payload.is_none(), "next_payload must remain None while paused");
}

/// Just below the safe-lag threshold: sequencing continues normally.
#[tokio::test]
async fn test_pipeline_safe_lag_at_limit_continues() {
    const MAX_SAFE_LAG: u64 = 1800;

    let mut engine = MockSequencerEngineClient::new();
    // Exactly at the limit (not over): unsafe - safe == MAX_SAFE_LAG, should proceed.
    engine.expect_get_unsafe_head().returning(|| Ok(head_at(MAX_SAFE_LAG)));
    engine.expect_get_safe_head().returning(|| Ok(head_at(0)));
    engine.expect_start_build_block().times(1).return_once(|_| Ok(PayloadId::default()));

    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_protocol::BlockInfo as L1BlockInfo;

    use crate::actors::MockOriginSelector;

    let mut origin_selector = MockOriginSelector::new();
    origin_selector.expect_next_l1_origin().times(1).return_once(|_, _| Ok(L1BlockInfo::default()));

    let attributes_builder =
        TestAttributesBuilder { attributes: vec![Ok(OpPayloadAttributes::default())] };

    let mut actor = test_actor();
    actor.engine_client = Arc::new(engine);
    actor.builder.engine_client = Arc::clone(&actor.engine_client);
    actor.builder.origin_selector = origin_selector;
    actor.builder.attributes_builder = attributes_builder;

    let mut next_payload: Option<UnsealedPayloadHandle> = None;
    let result = actor.run_block_pipeline(&mut next_payload).await;

    assert!(result.is_ok());
    assert!(next_payload.is_some(), "build should proceed when lag == MAX_SAFE_LAG");
}

// ---------------------------------------------------------------------------
// insert_and_await_head — unit tests on QueuedSequencerEngineClient
// ---------------------------------------------------------------------------

/// `insert_and_await_head` returns immediately if the watch channel already
/// reflects the expected block hash.
#[tokio::test]
async fn test_insert_and_await_head_success_immediate() {
    use tokio::sync::{mpsc, watch};

    use crate::actors::{
        SequencerEngineClient, engine::EngineActorRequest, sequencer::QueuedSequencerEngineClient,
    };

    let expected_hash = B256::with_last_byte(0x11);
    let head = head_at(10);
    let mut head_with_hash = head;
    head_with_hash.block_info.hash = expected_hash;

    // Watch already at the expected hash before insert fires.
    let (unsafe_head_tx, unsafe_head_rx) = watch::channel(head_with_hash);
    let (safe_head_tx, safe_head_rx) = watch::channel(head_at(0));
    let (engine_tx, mut engine_rx) = mpsc::channel::<EngineActorRequest>(4);

    // Consume the insert request so the channel doesn't stall.
    tokio::spawn(async move {
        let _ = engine_rx.recv().await;
        drop(unsafe_head_tx);
        drop(safe_head_tx);
    });

    let client = QueuedSequencerEngineClient::new(engine_tx, unsafe_head_rx, safe_head_rx);
    let envelope = envelope_with_hash(expected_hash);

    let result = client.insert_and_await_head(envelope, expected_hash).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().block_info.hash, expected_hash);
}

/// `insert_and_await_head` polls until the watch channel updates to the expected hash.
#[tokio::test]
async fn test_insert_and_await_head_success_after_update() {
    use tokio::sync::{mpsc, watch};

    use crate::actors::{
        SequencerEngineClient, engine::EngineActorRequest, sequencer::QueuedSequencerEngineClient,
    };

    let expected_hash = B256::with_last_byte(0x22);
    let initial_head = head_at(9);
    let mut updated_head = head_at(10);
    updated_head.block_info.hash = expected_hash;

    let (unsafe_head_tx, unsafe_head_rx) = watch::channel(initial_head);
    let (safe_head_tx, safe_head_rx) = watch::channel(head_at(0));
    let (engine_tx, mut engine_rx) = mpsc::channel::<EngineActorRequest>(4);

    // Simulate engine: receive insert request, then update the watch channel.
    let tx_clone = unsafe_head_tx.clone();
    tokio::spawn(async move {
        let _ = engine_rx.recv().await;
        // Brief delay to ensure the poll loop is waiting.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        let _ = tx_clone.send(updated_head);
        drop(safe_head_tx);
    });

    let client = QueuedSequencerEngineClient::new(engine_tx, unsafe_head_rx, safe_head_rx);
    let envelope = envelope_with_hash(expected_hash);

    let result = client.insert_and_await_head(envelope, expected_hash).await;
    assert!(result.is_ok());
    assert_eq!(result.unwrap().block_info.hash, expected_hash);
}

/// `insert_and_await_head` returns a `RequestError` after 500ms if the watch
/// channel never reflects the expected hash.
///
/// Uses `start_paused = true` so tokio auto-advances to the 500ms timer without
/// burning real wall-clock time. The watch senders are kept alive in the outer
/// scope so the channel stays open; this ensures the *timeout*, not a
/// channel-close, governs the outcome.
#[tokio::test(start_paused = true)]
async fn test_insert_and_await_head_timeout() {
    use tokio::sync::{mpsc, watch};

    use crate::actors::{
        SequencerEngineClient, engine::EngineActorRequest, sequencer::QueuedSequencerEngineClient,
    };

    let expected_hash = B256::with_last_byte(0x33);
    // Watch stays at a different hash — never matches expected.
    let wrong_head = head_at(0);

    // Keep senders alive so the watch channel remains open during the timeout.
    let (_unsafe_head_tx, unsafe_head_rx) = watch::channel(wrong_head);
    let (_safe_head_tx, safe_head_rx) = watch::channel(head_at(0));
    let (engine_tx, mut engine_rx) = mpsc::channel::<EngineActorRequest>(4);

    // Drain the insert request; idle without sending any update.
    tokio::spawn(async move {
        let _ = engine_rx.recv().await;
    });

    let client = QueuedSequencerEngineClient::new(engine_tx, unsafe_head_rx, safe_head_rx);
    let envelope = envelope_with_hash(expected_hash);

    let result = client.insert_and_await_head(envelope, expected_hash).await;
    assert!(
        matches!(result, Err(EngineClientError::RequestError(_))),
        "timeout must return RequestError, got {result:?}"
    );
}

// ---------------------------------------------------------------------------
// schedule_next_tick
// ---------------------------------------------------------------------------

/// With no pre-built payload, `schedule_next_tick` returns the 100ms retry interval.
#[test]
fn test_schedule_next_tick_no_payload_returns_retry_interval() {
    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_consensus_genesis::RollupConfig;

    use crate::actors::{
        MockConductor, MockOriginSelector, MockSequencerEngineClient,
        MockUnsafePayloadGossipClient, sequencer::SequencerActor,
    };

    type TestActor = SequencerActor<
        TestAttributesBuilder,
        MockConductor,
        MockOriginSelector,
        MockSequencerEngineClient,
        MockUnsafePayloadGossipClient,
    >;

    let cfg = RollupConfig::default();
    let d = TestActor::schedule_next_tick(&None, &cfg);
    assert_eq!(d, std::time::Duration::from_millis(100));
}

/// With a payload whose block timestamp is in the past, `schedule_next_tick`
/// returns `Duration::ZERO` (catch-up mode).
#[test]
fn test_schedule_next_tick_past_deadline_returns_zero() {
    use base_consensus_derive::test_utils::TestAttributesBuilder;
    use base_consensus_genesis::RollupConfig;

    use crate::actors::{
        MockConductor, MockOriginSelector, MockSequencerEngineClient,
        MockUnsafePayloadGossipClient, sequencer::SequencerActor,
    };

    type TestActor = SequencerActor<
        TestAttributesBuilder,
        MockConductor,
        MockOriginSelector,
        MockSequencerEngineClient,
        MockUnsafePayloadGossipClient,
    >;

    let cfg = RollupConfig { block_time: 2, ..Default::default() };
    // timestamp = 1 means next_block_ts = 3 seconds since UNIX_EPOCH — always in the past.
    let mut attrs = OpPayloadAttributes::default();
    attrs.payload_attributes.timestamp = 1;
    let handle = UnsealedPayloadHandle {
        payload_id: PayloadId::default(),
        attributes_with_parent: OpAttributesWithParent::new(
            attrs,
            L2BlockInfo::default(),
            None,
            false,
        ),
    };

    let d = TestActor::schedule_next_tick(&Some(handle), &cfg);
    assert_eq!(d, std::time::Duration::ZERO);
}
