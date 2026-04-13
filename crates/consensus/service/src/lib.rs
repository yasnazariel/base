#![doc = include_str!("../README.md")]
#![doc(
    html_logo_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    html_favicon_url = "https://avatars.githubusercontent.com/u/16627100?s=200&v=4",
    issue_tracker_base_url = "https://github.com/base/base/issues/"
)]
#![cfg_attr(docsrs, feature(doc_cfg, doc_auto_cfg))]

#[macro_use]
extern crate tracing;

mod service;
pub use service::{
    DerivationDelegateConfig, FollowNode, HEAD_STREAM_POLL_INTERVAL, L1Config, L1ConfigBuilder,
    NodeMode, RollupNode, RollupNodeBuilder, ShutdownSignal,
};

mod actors;
pub use actors::{
    AlloyL1BlockFetcher, BlockStream, BootstrapRole, BuildTaskError, CancellableContext, Conductor,
    ConductorClient, ConductorError, ConsolidateInput, DelayedL1OriginSelectorProvider,
    DelegateDerivationActor, DelegateL2Client, DelegateL2ClientError, DelegateL2DerivationActor,
    DerivationActor, DerivationActorRequest, DerivationClientError, DerivationClientResult,
    DerivationDelegateClient, DerivationDelegateClientError, DerivationEngineClient,
    DerivationError, DerivationState, DerivationStateMachine, DerivationStateTransitionError,
    DerivationStateUpdate, EngineClient, EngineClientError, EngineClientResult, EngineConfig,
    EngineError, EngineEvent, EngineHandle, EngineQueries, EngineRpcProcessor,
    EngineRpcRequestReceiver, EngineState, GossipTransport, L1BlockFetcher, L1OriginSelector,
    L1OriginSelectorError, L1OriginSelectorProvider, L1WatcherActor, L1WatcherActorError,
    L1WatcherDerivationClient, L2Finalizer, L2SourceClient, LogRetrier, NetworkActor,
    NetworkActorError, NetworkBuilder, NetworkBuilderError, NetworkConfig, NetworkDriver,
    NetworkDriverError, NetworkEngineClient, NetworkHandler, NetworkInboundData, NodeActor,
    OriginSelector, PayloadBuilder, PayloadSealer, PendingStopSender, PoolActivation,
    QueuedEngineRpcClient, QueuedL1WatcherDerivationClient, QueuedSequencerAdminAPIClient,
    QueuedUnsafePayloadGossipClient, RecoveryModeGuard, RpcActor, RpcActorError, RpcContext,
    SealState, SealStepError, SealTaskError, SequencerActor, SequencerActorError,
    SequencerAdminQuery, SequencerConfig, SequencerEngineClient, UnsafePayloadGossipClient,
    UnsafePayloadGossipClientError, UnsealedPayloadHandle,
};

mod metrics;
#[cfg(test)]
pub use actors::{
    MockConductor, MockDerivationEngineClient, MockNetworkEngineClient, MockOriginSelector,
    MockSequencerEngineClient, MockUnsafePayloadGossipClient,
};
pub use metrics::Metrics;
