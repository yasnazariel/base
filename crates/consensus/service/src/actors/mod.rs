//! [`NodeActor`] services for the node.
//!
//! [NodeActor]: super::NodeActor

mod traits;
pub use traits::{CancellableContext, NodeActor};

mod engine;
pub use engine::{
    BootstrapRole, BuildTaskError, ConsolidateInput, DerivationEngineClient, EngineClient,
    EngineClientError, EngineClientResult, EngineConfig, EngineError, EngineEvent, EngineHandle,
    EngineQueries, EngineRpcProcessor, EngineRpcRequestReceiver, EngineState, NetworkEngineClient,
    SealTaskError, SequencerEngineClient,
};
#[cfg(test)]
pub use engine::{MockDerivationEngineClient, MockNetworkEngineClient, MockSequencerEngineClient};

mod rpc;
pub use rpc::{
    QueuedEngineRpcClient, QueuedSequencerAdminAPIClient, RpcActor, RpcActorError, RpcContext,
};

// Re-export the RpcActor error for backwards compat

mod derivation;
pub use derivation::{
    DelegateDerivationActor, DelegateL2Client, DelegateL2ClientError, DelegateL2DerivationActor,
    DerivationActor, DerivationActorRequest, DerivationClientError, DerivationClientResult,
    DerivationDelegateClient, DerivationDelegateClientError, DerivationError, DerivationState,
    DerivationStateMachine, DerivationStateTransitionError, DerivationStateUpdate, L2Finalizer,
    L2SourceClient,
};

mod l1_watcher;
pub use l1_watcher::{
    AlloyL1BlockFetcher, BlockStream, L1BlockFetcher, L1WatcherActor, L1WatcherActorError,
    L1WatcherDerivationClient, LogRetrier, QueuedL1WatcherDerivationClient,
};

mod network;
#[cfg(test)]
pub use network::MockUnsafePayloadGossipClient;
pub use network::{
    GossipTransport, NetworkActor, NetworkActorError, NetworkBuilder, NetworkBuilderError,
    NetworkConfig, NetworkDriver, NetworkDriverError, NetworkHandler, NetworkInboundData,
    QueuedUnsafePayloadGossipClient, UnsafePayloadGossipClient, UnsafePayloadGossipClientError,
};

mod sequencer;
pub use sequencer::{
    Conductor, ConductorClient, ConductorError, DelayedL1OriginSelectorProvider, L1OriginSelector,
    L1OriginSelectorError, L1OriginSelectorProvider, OriginSelector, PayloadBuilder, PayloadSealer,
    PendingStopSender, PoolActivation, RecoveryModeGuard, SealState, SealStepError, SequencerActor,
    SequencerActorError, SequencerAdminQuery, SequencerConfig, UnsealedPayloadHandle,
};
#[cfg(test)]
pub use sequencer::{MockConductor, MockOriginSelector};
