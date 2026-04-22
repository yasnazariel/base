//! End-to-end failover tests: an [`EndpointPool`] of real alloy providers
//! backed by [`httpmock::MockServer`]s, driven through
//! [`RpcL1HeadPollingSource`] and [`HealthMonitor`].
//!
//! Exercises the same code paths the production batcher uses — `pool.active()`
//! is resolved on every poll, and a failed health probe rotates the active
//! selection so the next poll lands on a healthy endpoint without restarting
//! the source.

use std::{sync::Arc, time::Duration};

use alloy_provider::{Provider, ProviderBuilder, RootProvider, network::Ethereum};
use base_batcher_service::{
    EndpointPool, HealthMonitor, L1EndpointPool, Probe, RpcL1HeadPollingSource,
};
use base_batcher_source::L1HeadPolling;
use httpmock::{Mock, MockServer, prelude::*};

fn block_number_response(block: u64) -> String {
    format!(r#"{{"jsonrpc":"2.0","id":0,"result":"0x{block:x}"}}"#)
}

async fn block_number_mock(server: &MockServer, block: u64) -> Mock<'_> {
    server
        .mock_async(|when, then| {
            when.method(POST).path("/").json_body_includes(r#"{"method":"eth_blockNumber"}"#);
            then.status(200)
                .header("content-type", "application/json")
                .body(block_number_response(block));
        })
        .await
}

async fn error_mock(server: &MockServer) -> Mock<'_> {
    server
        .mock_async(|when, then| {
            when.method(POST).path("/").json_body_includes(r#"{"method":"eth_blockNumber"}"#);
            then.status(500).body("upstream is down");
        })
        .await
}

async fn build_provider(server: &MockServer) -> Arc<dyn Provider + Send + Sync> {
    let p: RootProvider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .connect(server.url("/").as_str())
        .await
        .expect("connect to mock server");
    Arc::new(p)
}

async fn build_pool(servers: &[&MockServer]) -> Arc<L1EndpointPool> {
    let mut entries = Vec::with_capacity(servers.len());
    for server in servers {
        entries.push((server.url("/").parse().unwrap(), build_provider(server).await));
    }
    Arc::new(EndpointPool::new(entries).unwrap())
}

fn monitor(pool: Arc<L1EndpointPool>) -> HealthMonitor<dyn Provider + Send + Sync> {
    // 60s interval is irrelevant — we drive the monitor manually via check_once.
    HealthMonitor::new(pool, Duration::from_secs(60), "test", Probe::block_number::<_, Ethereum>())
}

/// With a healthy active endpoint the polling source returns the block number
/// reported by the active server, and the passive endpoint receives no traffic.
#[tokio::test]
async fn test_polling_source_reads_from_active_endpoint() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    let mock_a = block_number_mock(&server_a, 100).await;
    let mock_b = block_number_mock(&server_b, 200).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    let source = RpcL1HeadPollingSource::new(Arc::clone(&pool));

    let head = source.latest_head().await.expect("active endpoint responds");
    assert_eq!(head, 100, "reads from the initial active endpoint (index 0)");

    mock_a.assert_async().await;
    assert_eq!(mock_b.calls_async().await, 0, "passive endpoint must not be polled");
}

/// The polling source resolves `pool.active()` on every call, so a `set_active`
/// from outside (e.g. by the health monitor) takes effect on the very next poll
/// without rebuilding the source.
#[tokio::test]
async fn test_polling_source_rotates_after_set_active() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    block_number_mock(&server_a, 100).await;
    let mock_b = block_number_mock(&server_b, 200).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    let source = RpcL1HeadPollingSource::new(Arc::clone(&pool));
    assert_eq!(source.latest_head().await.unwrap(), 100);

    pool.set_active(1);

    let head = source.latest_head().await.expect("rotated endpoint responds");
    assert_eq!(head, 200, "next poll uses the newly-active endpoint");
    mock_b.assert_async().await;
}

/// The polling source's circuit-breaker tolerates one transient error and
/// rotates the pool forward only after a second consecutive failure on the
/// same endpoint, closing the latency gap that would otherwise wait for the
/// health monitor's tick. (No monitor is spawned here, so all `active_index`
/// movement comes solely from the source's `record_call_failure`.)
#[tokio::test]
async fn test_polling_source_rotates_forward_after_consecutive_errors() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    error_mock(&server_a).await;
    let mock_b = block_number_mock(&server_b, 200).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    let source = RpcL1HeadPollingSource::new(Arc::clone(&pool));

    // First failure — within tolerance, no rotation yet.
    assert!(source.latest_head().await.is_err(), "first poll surfaces transport error");
    assert_eq!(pool.active_index(), 0, "first failure tolerated by circuit breaker");

    // Second consecutive failure trips the breaker and rotates to B.
    assert!(source.latest_head().await.is_err(), "second poll still on dead A");
    assert_eq!(pool.active_index(), 1, "second consecutive failure rotated to B");

    // Third poll: lands on B, which is healthy.
    let head = source.latest_head().await.expect("rotated endpoint responds");
    assert_eq!(head, 200);
    mock_b.assert_async().await;
}

/// Full failover loop: active endpoint starts healthy, then begins erroring;
/// the health monitor's probe detects the failure and rotates to the second
/// endpoint; subsequent polls land on the second endpoint and succeed.
#[tokio::test]
async fn test_health_monitor_fails_over_to_healthy_endpoint() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    let healthy_a = block_number_mock(&server_a, 100).await;
    block_number_mock(&server_b, 200).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    let source = RpcL1HeadPollingSource::new(Arc::clone(&pool));
    assert_eq!(source.latest_head().await.unwrap(), 100, "starts on A");

    // A starts erroring. Delete the healthy mock first so the error mock wins.
    healthy_a.delete_async().await;
    error_mock(&server_a).await;

    monitor(Arc::clone(&pool)).check_once().await;

    assert_eq!(pool.active_index(), 1, "monitor failed over from A to B");

    let head = source.latest_head().await.expect("B is now active and healthy");
    assert_eq!(head, 200);
}

/// When every endpoint is unhealthy the monitor must NOT silently switch to a
/// still-broken endpoint — it leaves the active selection alone so the next
/// real RPC call surfaces the failure with the original endpoint URL.
#[tokio::test]
async fn test_health_monitor_keeps_active_when_no_alternative_healthy() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    error_mock(&server_a).await;
    error_mock(&server_b).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    monitor(Arc::clone(&pool)).check_once().await;

    assert_eq!(pool.active_index(), 0, "no healthy alternative; keep current selection");
}

/// After failover from A → B, if A recovers and B then dies, the monitor must
/// fail back to A. Protects against an endpoint becoming permanently stuck on
/// a degraded backup after a brief primary outage.
#[tokio::test]
async fn test_health_monitor_recovers_to_healed_endpoint() {
    let server_a = MockServer::start_async().await;
    let server_b = MockServer::start_async().await;
    error_mock(&server_a).await;
    let healthy_b = block_number_mock(&server_b, 200).await;

    let pool = build_pool(&[&server_a, &server_b]).await;
    let m = monitor(Arc::clone(&pool));

    m.check_once().await;
    assert_eq!(pool.active_index(), 1, "first failover lands on B");

    // A heals, B dies.
    server_a.reset_async().await;
    block_number_mock(&server_a, 100).await;
    healthy_b.delete_async().await;
    error_mock(&server_b).await;

    m.check_once().await;
    assert_eq!(pool.active_index(), 0, "monitor must fail back to recovered endpoint");
}
