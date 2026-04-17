//! Background state root computation via reth's sparse trie task.
//!
//! Adapts reth's `PayloadProcessor` to the flashblocks build loop:
//! EVM execution stays in the builder, per-tx state diffs are forwarded
//! through a shared `OnStateHook`, and the final root is awaited once
//! the last flashblock is sealed.

use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

use alloy_evm::{
    Database,
    block::{OnStateHook, StateChangeSource},
};
use alloy_primitives::B256;
use base_common_consensus::{BasePrimitives, BaseTransactionSigned};
use base_execution_evm::BaseEvmConfig;
use reth_engine_primitives::TreeConfig;
use reth_engine_tree::tree::{
    ExecutionEnv, PayloadProcessor, StateProviderBuilder, sparse_trie::StateRootComputeOutcome,
};
use reth_node_api::PayloadBuilderError;
use reth_primitives_traits::Recovered;
use reth_provider::{
    HashedPostStateProvider, ProviderError, StateRootProvider, StorageRootProvider,
    providers::OverlayStateProviderFactory,
};
use reth_revm::{State, db::states::bundle_state::BundleRetention};
use reth_trie::{HashedPostState, updates::TrieUpdates};
use reth_trie_db::ChangesetCache;
use reth_trie_parallel::root::ParallelStateRootError;
use revm::state::EvmState;
use tracing::{info, warn};

use crate::{BuilderMetrics, traits::ClientBounds};

/// Dependencies for the background state root task.
///
/// Held by the payload builder and reused across blocks so the sparse
/// trie stays warm. Only one `build_payload` runs at a time — the
/// mutex guards the single active builder invocation.
#[derive(derive_more::Debug, Clone)]
pub(crate) struct StateRootTaskDeps {
    #[debug(skip)]
    processor: Arc<Mutex<PayloadProcessor<BaseEvmConfig>>>,
    changeset_cache: ChangesetCache,
    tree_config: TreeConfig,
}

impl StateRootTaskDeps {
    pub(crate) fn new(
        processor: PayloadProcessor<BaseEvmConfig>,
        changeset_cache: ChangesetCache,
        tree_config: TreeConfig,
    ) -> Self {
        Self { processor: Arc::new(Mutex::new(processor)), changeset_cache, tree_config }
    }
}

/// Shared `OnStateHook` forwarder.
///
/// Wraps reth's `&mut self` hook in an `Arc<Mutex>` so the flashblocks
/// builder can forward state diffs from multiple call sites — pre-exec
/// system calls installed on a `BlockExecutor`, and per-tx commits from
/// the sequencer / user loops. `finish()` drops the inner hook, which
/// signals `FinishedStateUpdates` to the sparse trie task.
#[derive(Clone)]
pub(crate) struct BuilderStateHook {
    hook: Arc<Mutex<Option<Box<dyn OnStateHook>>>>,
    tx_idx: Arc<AtomicUsize>,
}

impl BuilderStateHook {
    fn new(hook: impl OnStateHook) -> Self {
        Self {
            hook: Arc::new(Mutex::new(Some(Box::new(hook)))),
            tx_idx: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Forwards a per-transaction state update to the sparse trie task.
    pub(crate) fn send_state_update(&self, state: &EvmState) {
        let idx = self.tx_idx.fetch_add(1, Ordering::Relaxed);
        self.forward(StateChangeSource::Transaction(idx), state);
    }

    /// Boxed hook for `BlockExecutor::set_state_hook`.
    pub(crate) fn as_on_state_hook(&self) -> Box<dyn OnStateHook> {
        Box::new(self.clone())
    }

    /// Drops the inner hook, triggering `FinishedStateUpdates`.
    /// Must run before awaiting the state root result.
    pub(crate) fn finish(&self) {
        self.hook.lock().expect("state hook poisoned").take();
    }

    /// Returns an RAII guard that calls [`finish`](Self::finish) on drop,
    /// ensuring the sparse trie task is signalled on all exit paths.
    pub(crate) const fn finish_guard(&self) -> FinishGuard<'_> {
        FinishGuard(self)
    }

    fn forward(&self, source: StateChangeSource, state: &EvmState) {
        let mut guard = self.hook.lock().expect("state hook poisoned");
        if let Some(h) = guard.as_mut() {
            h.on_state(source, state);
        } else {
            debug_assert!(false, "state update after finish() — diff lost for sparse trie");
        }
    }
}

impl OnStateHook for BuilderStateHook {
    fn on_state(&mut self, source: StateChangeSource, state: &EvmState) {
        self.forward(source, state);
    }
}

/// RAII guard that calls [`BuilderStateHook::finish`] on drop.
pub(crate) struct FinishGuard<'a>(&'a BuilderStateHook);

impl Drop for FinishGuard<'_> {
    fn drop(&mut self) {
        self.0.finish();
    }
}

/// Receiver for the background state root result.
pub(crate) type StateRootHandle =
    std::sync::mpsc::Receiver<Result<StateRootComputeOutcome, ParallelStateRootError>>;

/// Spawns the sparse trie task in trie-only mode.
///
/// The tx list is empty — the flashblocks builder drives EVM execution
/// and forwards state diffs via the returned [`BuilderStateHook`].
/// `PayloadProcessor` only runs the sparse trie task. Consequently
/// `evm_env` and `hash` on [`ExecutionEnv`] are unused.
pub(crate) fn spawn_state_root_task<Client: ClientBounds>(
    deps: &StateRootTaskDeps,
    client: &Client,
    parent_hash: B256,
    parent_state_root: B256,
    block_number: u64,
) -> Result<(BuilderStateHook, StateRootHandle), PayloadBuilderError> {
    let overlay = OverlayStateProviderFactory::new(client.clone(), deps.changeset_cache.clone());
    let provider =
        StateProviderBuilder::<BasePrimitives, _>::new(client.clone(), parent_hash, None);

    let no_txs = (
        Vec::<Result<Recovered<BaseTransactionSigned>, core::convert::Infallible>>::new(),
        std::convert::identity,
    );
    let env = ExecutionEnv {
        evm_env: Default::default(),
        hash: B256::ZERO,
        parent_hash,
        parent_state_root,
        transaction_count: 0,
        withdrawals: None,
    };

    let mut processor = deps.processor.lock().expect("payload processor mutex poisoned");
    let mut handle = processor.spawn(env, no_txs, provider, overlay, &deps.tree_config, None);
    let rx = handle.take_state_root_rx();
    let hook = BuilderStateHook::new(handle.state_hook());

    BuilderMetrics::state_root_task_started_count().increment(1);
    info!(target: "state_root_task", block_number, %parent_state_root, "spawned");

    Ok((hook, rx))
}

/// Flushes pending updates, awaits the background root, falls back to
/// synchronous computation on any failure.
pub(crate) fn finalize_state_root<DB, P>(
    state_hook: &BuilderStateHook,
    state_root_handle: Option<StateRootHandle>,
    state: &mut State<DB>,
    block_number: u64,
) -> Result<(B256, TrieUpdates, HashedPostState), PayloadBuilderError>
where
    DB: Database<Error = ProviderError> + AsRef<P> + revm::Database,
    P: StateRootProvider + HashedPostStateProvider + StorageRootProvider,
{
    let start = std::time::Instant::now();

    state_hook.finish();

    // revm's commit() pushes to transition_state; we read bundle_state
    // below, so merge first. Safe on the final-block path: build_block's
    // later merge is a no-op once transition_state is drained.
    state.merge_transitions(BundleRetention::Reverts);

    let provider = state.database.as_ref();
    let hashed_state = provider.hashed_post_state(&state.bundle_state);

    if let Some(rx) = state_root_handle {
        // Blocking recv is safe: build_payload runs on a spawn_blocking_task
        // thread (see generator.rs), so we won't stall the tokio runtime.
        match rx.recv() {
            Ok(Ok(outcome)) => {
                let elapsed = start.elapsed();
                BuilderMetrics::state_root_task_completed_count().increment(1);
                BuilderMetrics::state_root_task_duration().record(elapsed);
                info!(
                    target: "state_root_task",
                    block_number,
                    duration_ms = elapsed.as_millis(),
                    state_root = %outcome.state_root,
                    "completed"
                );
                #[cfg(debug_assertions)]
                {
                    if let Ok((sync_root, _)) =
                        provider.state_root_with_updates(hashed_state.clone())
                    {
                        debug_assert_eq!(
                            outcome.state_root, sync_root,
                            "background state root diverges from synchronous computation \
                             (block {block_number}): hook may have missed a state update",
                        );
                    }
                }

                return Ok((outcome.state_root, outcome.trie_updates, hashed_state));
            }
            Ok(Err(err)) => warn!(target: "state_root_task", block_number, %err, "task failed"),
            Err(_) => warn!(target: "state_root_task", block_number, "task channel dropped"),
        }
        BuilderMetrics::state_root_task_error_count().increment(1);
    }

    warn!(target: "state_root_task", block_number, "falling back to synchronous state root");
    let (state_root, trie_output) = provider
        .state_root_with_updates(hashed_state.clone())
        .inspect_err(|err| warn!(target: "payload_builder", %err, "state root failure"))?;

    let elapsed = start.elapsed();
    BuilderMetrics::state_root_calculation_duration().record(elapsed);
    BuilderMetrics::state_root_calculation_gauge().set(elapsed);

    Ok((state_root, trie_output, hashed_state))
}
